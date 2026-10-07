use std::{
    collections::{HashMap, HashSet},
    fs,
    ops::Deref,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};
use tauri::Manager;

use crate::{
    models::{DbFindRef, DownloadLinkRow, ResourceRef},
    naming::{self, SEEDED_CATEGORIES},
};

const DB_FILE_NAME: &str = "vam_var_deduper.db";
pub(crate) const SCHEMA_VERSION: i32 = 17;

/// Number of additional read-only connections opened against the same file.
/// WAL lets these run concurrently with the single writer and with each
/// other — so the UI's list / count / search queries no longer serialize on
/// the writer mutex when a scan persist or manifest import is in flight.
/// 3 matches the typical concurrent-UI-fetch budget (resource list paging,
/// VAR packages tab, CRC lookups during DB Find) without spending too many
/// SQLite file descriptors.
const READ_POOL_SIZE: usize = 3;

#[derive(Clone)]
pub(crate) struct Db {
    pub(crate) conn: Arc<Mutex<Connection>>,
    /// Pool of read-only connections sharing the same file (and therefore the
    /// same WAL). For in-memory test databases the pool is empty; `read()`
    /// then falls back to the writer mutex, preserving single-connection
    /// behavior so tests keep their existing data view.
    pub(crate) readers: Arc<ReadPool>,
    /// Maps each seeded category name (`Scene`, `Morph`, …) to its row id in
    /// the `categories` table. Loaded once at open time so resource inserts
    /// don't need a per-row SELECT.
    pub(crate) categories: Arc<HashMap<&'static str, i64>>,
}

/// Bounded pool of read-only `Connection`s. Acquires block via `Condvar` when
/// all connections are in use. Drop returns the connection to the pool.
pub(crate) struct ReadPool {
    inner: Mutex<ReadPoolInner>,
    cv: Condvar,
}

struct ReadPoolInner {
    available: Vec<Connection>,
    capacity: usize,
}

impl ReadPool {
    fn open_from_path(path: &Path, size: usize) -> Result<Self> {
        let mut available = Vec::with_capacity(size);
        for _ in 0..size {
            let conn = Connection::open(path).with_context(|| {
                format!("failed to open SQLite read connection at {}", path.display())
            })?;
            apply_pragmas(&conn)?;
            available.push(conn);
        }
        Ok(Self {
            inner: Mutex::new(ReadPoolInner {
                available,
                capacity: size,
            }),
            cv: Condvar::new(),
        })
    }

    /// Empty pool used by in-memory test databases where opening another
    /// `Connection` would create a separate DB. `Db::read()` detects the
    /// empty pool and routes reads through the writer mutex instead.
    #[cfg(test)]
    fn empty() -> Self {
        Self {
            inner: Mutex::new(ReadPoolInner {
                available: Vec::new(),
                capacity: 0,
            }),
            cv: Condvar::new(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.inner.lock().map(|g| g.capacity == 0).unwrap_or(true)
    }

    fn acquire(&self) -> ReadGuard<'_> {
        let mut guard = self
            .inner
            .lock()
            .expect("ReadPool mutex poisoned");
        loop {
            if let Some(conn) = guard.available.pop() {
                return ReadGuard {
                    pool: self,
                    conn: Some(conn),
                };
            }
            guard = self
                .cv
                .wait(guard)
                .expect("ReadPool condvar poisoned");
        }
    }
}

pub(crate) struct ReadGuard<'a> {
    pool: &'a ReadPool,
    conn: Option<Connection>,
}

impl Deref for ReadGuard<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn
            .as_ref()
            .expect("ReadGuard accessed after drop")
    }
}

impl Drop for ReadGuard<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            if let Ok(mut guard) = self.pool.inner.lock() {
                guard.available.push(conn);
                self.pool.cv.notify_one();
            }
        }
    }
}

/// Handle to a connection usable for read queries. Either a pool-backed
/// connection (production) or the writer connection (in-memory tests).
pub(crate) enum ReadHandle<'a> {
    Pool(ReadGuard<'a>),
    Writer(MutexGuard<'a, Connection>),
}

impl<'a> Deref for ReadHandle<'a> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match self {
            ReadHandle::Pool(g) => g.deref(),
            ReadHandle::Writer(g) => g.deref(),
        }
    }
}

impl Db {
    fn migrate_writer(mut conn: Connection) -> Result<Connection> {
        apply_pragmas(&conn)?;
        migrate(&mut conn)?;
        // Re-run the seed step on every open so new categories added to
        // SEEDED_CATEGORIES land in DBs that already have v4 applied. Cheap
        // (one INSERT OR IGNORE per name) and idempotent.
        seed_categories(&conn)?;
        Ok(conn)
    }

    fn finalize(conn: Connection, readers: ReadPool) -> Result<Self> {
        let categories = Arc::new(load_category_ids(&conn)?);
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            readers: Arc::new(readers),
            categories,
        })
    }

    /// Acquire a connection suitable for read-only queries. Prefer this over
    /// `db.conn.lock()` on hot read paths so they don't serialize on the
    /// writer mutex during scans / imports / dedup writes.
    pub(crate) fn read(&self) -> Result<ReadHandle<'_>> {
        if self.readers.is_empty() {
            let guard = self
                .conn
                .lock()
                .map_err(|_| anyhow!("database connection poisoned"))?;
            Ok(ReadHandle::Writer(guard))
        } else {
            Ok(ReadHandle::Pool(self.readers.acquire()))
        }
    }
}

fn db_path(app: &tauri::AppHandle) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .context("failed to resolve app data dir")?;
    fs::create_dir_all(&dir).context("failed to create app data dir")?;
    Ok(dir.join(DB_FILE_NAME))
}

pub(crate) fn open(app: &tauri::AppHandle) -> Result<Db> {
    let path = db_path(app)?;
    let conn = Connection::open(&path)
        .with_context(|| format!("failed to open SQLite database at {}", path.display()))?;
    // Migrate on the writer FIRST so the schema (incl. WAL mode) is committed
    // before any reader connects. Then open the read pool against the same
    // file — its connections inherit the migrated schema via SQLite's
    // standard cross-connection visibility under WAL.
    let conn = Db::migrate_writer(conn)?;
    let readers = ReadPool::open_from_path(&path, READ_POOL_SIZE)?;
    Db::finalize(conn, readers)
}

#[cfg(test)]
pub(crate) fn open_in_memory() -> Result<Db> {
    let conn = Connection::open_in_memory().context("failed to open in-memory SQLite database")?;
    // In-memory DBs can't share a pool (each Connection::open is a separate
    // DB); reads route through the writer via ReadHandle::Writer.
    let conn = Db::migrate_writer(conn)?;
    Db::finalize(conn, ReadPool::empty())
}

fn apply_pragmas(conn: &Connection) -> Result<()> {
    // - WAL journal + synchronous=NORMAL: durable & fast (NORMAL fsyncs WAL on
    //   commit, not every write).
    // - cache_size = -262144: 256 MB page cache (default ~2 MB). On a 4M-row
    //   dataset the idx_resources_crc32 index alone can exceed 64 MB; bumping
    //   to 256 MB keeps it hot across bulk dedup queries.
    // - temp_store = MEMORY: temp tables / sort buffers stay off disk.
    // - mmap_size = 1 GB: maps up to 1 GB of the DB so reads bypass syscalls.
    //   Advisory — SQLite caps at file size if the DB is smaller.
    // - wal_autocheckpoint = 2000: doubles the default checkpoint threshold,
    //   reducing fsync stalls during long bulk-insert sessions.
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -262144;
         PRAGMA mmap_size = 1073741824;
         PRAGMA wal_autocheckpoint = 2000;",
    )
    .context("failed to apply SQLite pragmas")?;
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let current: i32 =
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))
            .context("failed to read schema version")?;

    if current < 1 {
        let tx = conn.transaction().context("failed to start v1 tx")?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS packages (
                package_id        TEXT PRIMARY KEY,
                file_path         TEXT NOT NULL UNIQUE,
                size_bytes        INTEGER NOT NULL,
                modified_ns       TEXT NOT NULL,
                scene_image_path  TEXT,
                last_scanned_at   INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS resources (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                package_id      TEXT NOT NULL,
                internal_path   TEXT NOT NULL,
                sha256          TEXT NOT NULL,
                size            INTEGER NOT NULL,
                effective_size  INTEGER,
                FOREIGN KEY (package_id) REFERENCES packages(package_id) ON DELETE CASCADE,
                UNIQUE (package_id, internal_path)
            );

            CREATE INDEX IF NOT EXISTS idx_resources_sha256  ON resources(sha256);
            CREATE INDEX IF NOT EXISTS idx_resources_package ON resources(package_id);",
        )
        .context("failed to apply v1 schema")?;
        tx.commit().context("failed to commit v1 migration")?;
    }

    if current < 2 {
        let tx = conn.transaction().context("failed to start v2 tx")?;
        let column_present: bool = tx
            .query_row(
                "SELECT 1 FROM pragma_table_info('resources') WHERE name = 'crc32'",
                [],
                |_| Ok(true),
            )
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                other => Err(other),
            })
            .context("failed to inspect resources columns")?;
        if !column_present {
            tx.execute("ALTER TABLE resources ADD COLUMN crc32 INTEGER", [])
                .context("failed to add resources.crc32 column")?;
        }
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_resources_crc32 ON resources(crc32);",
        )
        .context("failed to add crc32 index")?;
        tx.commit().context("failed to commit v2 migration")?;
    }

    if current < 3 {
        let tx = conn.transaction().context("failed to start v3 tx")?;
        // Explicit named unique index on (package_id, internal_path).
        // The v1 schema also declared `UNIQUE (package_id, internal_path)` inline,
        // but a named index is easier to inspect in DB browsers and is idempotent
        // for any DB whose v1 ran without the inline constraint.
        tx.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS uq_resources_package_internal_path
                 ON resources(package_id, internal_path);",
        )
        .context("failed to add unique index on resources(package_id, internal_path)")?;
        tx.commit().context("failed to commit v3 migration")?;
    }

    if current < 4 {
        let tx = conn.transaction().context("failed to start v4 tx")?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS creators (
                creator_id INTEGER PRIMARY KEY AUTOINCREMENT,
                name       TEXT NOT NULL UNIQUE
            );

            CREATE TABLE IF NOT EXISTS categories (
                category_id INTEGER PRIMARY KEY AUTOINCREMENT,
                name        TEXT NOT NULL UNIQUE
            );",
        )
        .context("failed to create v4 reference tables")?;

        // Idempotent column adds — guard each `ALTER TABLE` with the
        // pragma_table_info check used for v2's crc32 column so re-running on a
        // partially-applied DB is safe.
        let pkg_creator_present: bool = tx
            .query_row(
                "SELECT 1 FROM pragma_table_info('packages') WHERE name = 'creator_id'",
                [],
                |_| Ok(true),
            )
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                other => Err(other),
            })
            .context("failed to inspect packages columns")?;
        if !pkg_creator_present {
            tx.execute(
                "ALTER TABLE packages ADD COLUMN creator_id INTEGER REFERENCES creators(creator_id)",
                [],
            )
            .context("failed to add packages.creator_id column")?;
        }

        let res_category_present: bool = tx
            .query_row(
                "SELECT 1 FROM pragma_table_info('resources') WHERE name = 'category_id'",
                [],
                |_| Ok(true),
            )
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                other => Err(other),
            })
            .context("failed to inspect resources columns")?;
        if !res_category_present {
            tx.execute(
                "ALTER TABLE resources ADD COLUMN category_id INTEGER REFERENCES categories(category_id)",
                [],
            )
            .context("failed to add resources.category_id column")?;
        }

        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_packages_creator   ON packages(creator_id);
             CREATE INDEX IF NOT EXISTS idx_resources_category ON resources(category_id);",
        )
        .context("failed to add v4 indexes")?;

        seed_categories(&tx)?;

        tx.commit().context("failed to commit v4 migration")?;
    }

    if current < 5 {
        let tx = conn.transaction().context("failed to start v5 tx")?;

        // Generic small key/value store for runtime feature state. Kept
        // around even though the only original consumer (FTS path-search
        // index, torn down in v8) is gone — future feature flags can reuse
        // it without another migration.
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS app_meta (\
                key   TEXT PRIMARY KEY,\
                value TEXT NOT NULL\
             );",
        )
        .context("failed to create app_meta table")?;

        // Legacy FTS5 virtual table + meta seed. The v8 migration immediately
        // drops these and deletes the meta keys, so on a fresh DB they live
        // for one startup pass before being removed. Migration bodies aren't
        // rewritten to preserve schema history; v8 is the canonical answer.
        tx.execute_batch(
            "INSERT OR IGNORE INTO app_meta (key, value) VALUES ('path_search_ready', '0'); \
             INSERT OR IGNORE INTO app_meta (key, value) VALUES ('path_search_last_indexed_id', '0');",
        )
        .context("failed to seed app_meta defaults")?;
        tx.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS resources_fts \
             USING fts5(internal_path, content='resources', content_rowid='id', tokenize='trigram');",
        )
        .context("failed to create resources_fts virtual table")?;

        tx.commit().context("failed to commit v5 migration")?;
    }

    if current < 6 {
        // Drop the legacy `sha256` column and index. Duplicate grouping now
        // identifies groups by `(size, crc32)` (the existing CRC pre-filter
        // promoted to the final pass); the SHA pass and column are gone.
        // SQLite ≥ 3.35.0 supports `ALTER TABLE ... DROP COLUMN`; this repo's
        // rusqlite 0.31 + `bundled` ships a recent SQLite.
        let tx = conn.transaction().context("failed to start v6 tx")?;
        tx.execute_batch(
            "DROP INDEX IF EXISTS idx_resources_sha256;
             ALTER TABLE resources DROP COLUMN sha256;",
        )
        .context("failed to drop resources.sha256")?;
        tx.commit().context("failed to commit v6 migration")?;
    }

    if current < 7 {
        // Per-creator flag for the reclaim scan: 0=none, 1=favorite,
        // 2=blocked, 3+ reserved. Favorite creators are skipped in the local
        // walk; blocked creators are excluded from DB-lookup candidates.
        // Validation lives in Rust so future values don't require a migration.
        let tx = conn.transaction().context("failed to start v7 tx")?;
        let flag_present: bool = tx
            .query_row(
                "SELECT 1 FROM pragma_table_info('creators') WHERE name = 'flag'",
                [],
                |_| Ok(true),
            )
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                other => Err(other),
            })
            .context("failed to inspect creators columns")?;
        if !flag_present {
            tx.execute(
                "ALTER TABLE creators ADD COLUMN flag INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .context("failed to add creators.flag column")?;
        }
        tx.commit().context("failed to commit v7 migration")?;
    }

    if current < 8 {
        // Tear down the FTS5 path-search index installed by v5. The Resource
        // List page no longer supports substring path search; remaining
        // search modes (CRC32 8-hex + package_id prefix) don't need any
        // index beyond the ones already on `resources`. `IF EXISTS` keeps
        // this a no-op for fresh DBs that never had the artifacts in the
        // first place.
        let tx = conn.transaction().context("failed to start v8 tx")?;
        tx.execute_batch(
            "DROP TRIGGER IF EXISTS resources_fts_ai;\
             DROP TRIGGER IF EXISTS resources_fts_ad;\
             DROP TRIGGER IF EXISTS resources_fts_au;\
             DROP TABLE   IF EXISTS resources_fts;\
             DELETE FROM app_meta WHERE key IN ('path_search_ready', 'path_search_last_indexed_id');",
        )
        .context("failed to tear down FTS path search artifacts")?;
        tx.commit().context("failed to commit v8 migration")?;
    }

    if current < 9 {
        // Community download-link DB for the Download VARs page: Pixeldrain /
        // MediaFire mirror links imported from per-creator .txt files. Keyed by
        // the version-stripped family so a missing dependency resolves
        // regardless of the exact version requested.
        let tx = conn.transaction().context("failed to start v9 tx")?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS download_links (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                package_base  TEXT NOT NULL,
                version       INTEGER,
                filename      TEXT NOT NULL,
                host          TEXT NOT NULL,
                url           TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_download_links_base ON download_links(package_base);
            CREATE UNIQUE INDEX IF NOT EXISTS uq_download_links_file_url
                ON download_links(filename, url);",
        )
        .context("failed to create v9 download_links table")?;
        tx.commit().context("failed to commit v9 migration")?;
    }

    if current < 10 {
        // download_links gains a nullable size, filled after a successful
        // download (Pixeldrain doesn't report a size up front). Guarded so it's
        // idempotent on a partially-applied DB.
        let tx = conn.transaction().context("failed to start v10 tx")?;
        let size_present: bool = tx
            .query_row(
                "SELECT 1 FROM pragma_table_info('download_links') WHERE name = 'size'",
                [],
                |_| Ok(true),
            )
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                other => Err(other),
            })
            .context("failed to inspect download_links columns")?;
        if !size_present {
            tx.execute("ALTER TABLE download_links ADD COLUMN size INTEGER", [])
                .context("failed to add download_links.size column")?;
        }
        tx.commit().context("failed to commit v10 migration")?;
    }

    if current < 11 {
        // Per-package favorite flag. TEXT key with NO foreign key into
        // `packages`: folder-mode listings surface packages that were never
        // indexed, and the same package_id can exist in multiple folders — the
        // flag must survive both. Values mirror the creator flags (0 = none,
        // 1 = favorite); "none" is represented by row absence, so the table
        // stays sparse and unfavoriting self-cleans stale rows.
        let tx = conn.transaction().context("failed to start v11 tx")?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS package_flags (
                package_id TEXT PRIMARY KEY,
                flag       INTEGER NOT NULL DEFAULT 0
            );",
        )
        .context("failed to create v11 package_flags table")?;
        tx.commit().context("failed to commit v11 migration")?;
    }

    if current < 12 {
        // Partial index for the VAR Packages "scene image" filter. The filter's
        // predicate scans all of `resources` (4M+ rows on a real library, ~4 s
        // cold) on every page/filter change; indexing just the matching rows
        // (~85k) makes both the DISTINCT id-set load and the database-mode IN
        // subquery index-only (measured 4 s -> ~40-70 ms). One-time build cost
        // ~5 s on a 4.4M-row DB, paid here in the migration.
        //
        // The WHERE text must stay byte-identical to SCENE_IMAGE_PREDICATE_SQL:
        // SQLite only uses a partial index when the query's WHERE provably
        // implies the index's WHERE, and that proof is textual. If the predicate
        // ever changes, add a migration that drops and recreates this index.
        let tx = conn.transaction().context("failed to start v12 tx")?;
        tx.execute_batch(&format!(
            "CREATE INDEX IF NOT EXISTS idx_resources_scene_image \
                 ON resources(package_id) WHERE {SCENE_IMAGE_PREDICATE_SQL};",
        ))
        .context("failed to create v12 scene image index")?;
        tx.commit().context("failed to commit v12 migration")?;
    }

    if current < 13 {
        // VAR Packages library info, one JSON blob per archive path. A cache,
        // not data: rows are validated against the file's size + mtime (and the
        // classifier version inside the blob) on every read, so a stale row
        // only costs a re-read of that one archive. Keyed by path rather than
        // package_id because the same package can sit in several folders.
        let tx = conn.transaction().context("failed to start v13 tx")?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS var_info_cache (
                file_path   TEXT PRIMARY KEY,
                size        INTEGER NOT NULL,
                modified_ns TEXT NOT NULL,
                info        TEXT NOT NULL
            );",
        )
        .context("failed to create v13 var_info_cache table")?;
        tx.commit().context("failed to commit v13 migration")?;
    }

    if current < 14 {
        // Hub page wishlist, as in VaM Backstage: a durable copy of each saved
        // resource's Hub JSON (paid/removed resources can't be re-fetched from
        // an id alone, so the row owns its snapshot).
        let tx = conn.transaction().context("failed to start v14 tx")?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS hub_wishlist (
                resource_id TEXT PRIMARY KEY,
                snapshot    TEXT NOT NULL,
                created_at  INTEGER NOT NULL
            );",
        )
        .context("failed to create v14 hub_wishlist table")?;
        tx.commit().context("failed to commit v14 migration")?;
    }

    if current < 15 {
        // Case-insensitive package-id lookups by prefix (is any version of
        // this dependency indexed?) as an index range scan, instead of loading
        // every package id — tens of thousands on a full index.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_packages_id_lower ON packages (lower(package_id));",
        )
        .context("failed to create v15 package id index")?;
    }

    if current < 16 {
        // A download link can be a .zip holding the .var: its path inside the
        // archive and the archive's password. Guarded like v10.
        let tx = conn.transaction().context("failed to start v16 tx")?;
        for column in ["archive_entry", "archive_password"] {
            let present: bool = tx
                .query_row(
                    "SELECT 1 FROM pragma_table_info('download_links') WHERE name = ?1",
                    [column],
                    |_| Ok(true),
                )
                .or_else(|err| match err {
                    rusqlite::Error::QueryReturnedNoRows => Ok(false),
                    other => Err(other),
                })
                .context("failed to inspect download_links columns")?;
            if !present {
                tx.execute(&format!("ALTER TABLE download_links ADD COLUMN {column} TEXT"), [])
                    .context("failed to add a v16 download_links column")?;
            }
        }
        tx.commit().context("failed to commit v16 migration")?;
    }

    if current < 17 {
        // How each download link has fared: when a download through it last
        // worked, how many times in a row it has failed since, and why — so
        // a dead mirror stops being picked and the next one is tried.
        let tx = conn.transaction().context("failed to start v17 tx")?;
        for (column, ty) in [("last_ok", "INTEGER"), ("fail_count", "INTEGER NOT NULL DEFAULT 0"), ("last_error", "TEXT")] {
            let present: bool = tx
                .query_row(
                    "SELECT 1 FROM pragma_table_info('download_links') WHERE name = ?1",
                    [column],
                    |_| Ok(true),
                )
                .or_else(|err| match err {
                    rusqlite::Error::QueryReturnedNoRows => Ok(false),
                    other => Err(other),
                })
                .context("failed to inspect download_links columns")?;
            if !present {
                tx.execute(&format!("ALTER TABLE download_links ADD COLUMN {column} {ty}"), [])
                    .context("failed to add a v17 download_links column")?;
            }
        }
        tx.commit().context("failed to commit v17 migration")?;
    }

    if current != SCHEMA_VERSION {
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
            .context("failed to set schema version")?;
    }

    Ok(())
}

/// Bulk-inserts download links (`INSERT OR IGNORE` on the (filename, url) unique
/// index, so re-importing is idempotent). Returns the number of new rows added.
pub(crate) fn insert_download_links(db: &Db, rows: &[DownloadLinkRow]) -> Result<usize> {
    let mut conn = db
        .conn
        .lock()
        .map_err(|_| anyhow!("database connection poisoned"))?;
    let tx = conn.transaction().context("failed to start link import tx")?;
    let mut added = 0usize;
    {
        let mut stmt = tx.prepare(
            "INSERT OR IGNORE INTO download_links
                 (package_base, version, filename, host, url, size, archive_entry, archive_password)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        // The file's links so far (through the family index), to skip one it
        // already has under another spelling — `/u/<id>` vs `/api/file/<id>`,
        // a MediaFire page with or without the name, the file name's case.
        let mut existing = tx.prepare(
            "SELECT url FROM download_links WHERE package_base = ?1 AND filename = ?2 COLLATE NOCASE",
        )?;
        for row in rows {
            let key = crate::sources::link_key(&row.url);
            let known: Vec<String> = existing
                .query_map(params![row.package_base, row.filename], |r| r.get::<_, String>(0))?
                .flatten()
                .collect();
            if known.iter().any(|u| crate::sources::link_key(u) == key) {
                continue;
            }
            added += stmt.execute(params![
                row.package_base,
                row.version,
                row.filename,
                row.host,
                row.url,
                row.size,
                row.archive_entry,
                row.archive_password
            ])?;
        }
    }
    tx.commit().context("failed to commit link import")?;
    Ok(added)
}

/// Records the actual size (bytes) for every link row pointing at `filename`,
/// after a successful download. No-op when no link row matches (e.g. a Hub-only
/// package). Only overwrites a missing/zero size so a real value isn't lost.
pub(crate) fn update_download_link_size(db: &Db, filename: &str, size: u64) -> Result<()> {
    let conn = db
        .conn
        .lock()
        .map_err(|_| anyhow!("database connection poisoned"))?;
    conn.execute(
        "UPDATE download_links SET size = ?1
         WHERE filename = ?2 AND (size IS NULL OR size = 0)",
        params![size as i64, filename],
    )?;
    Ok(())
}

/// All links for a package family, newest version first and Pixeldrain ahead of
/// MediaFire within a version so the caller can prefer the auto-downloadable
/// mirror.
pub(crate) fn find_download_links(db: &Db, package_base_lc: &str) -> Result<Vec<DownloadLinkRow>> {
    let handle = db.read()?;
    let mut stmt = handle.prepare(
        "SELECT package_base, version, filename, host, url, size, archive_entry, archive_password, last_ok, fail_count, last_error
         FROM download_links
         WHERE package_base = ?1
         ORDER BY version DESC,
                  CASE host WHEN 'pixeldrain' THEN 0 WHEN 'mediafire' THEN 1 ELSE 2 END",
    )?;
    let rows = stmt
        .query_map(params![package_base_lc], |r| {
            Ok(DownloadLinkRow {
                package_base: r.get(0)?,
                version: r.get(1)?,
                filename: r.get(2)?,
                host: r.get(3)?,
                url: r.get(4)?,
                size: r.get(5)?,
                archive_entry: r.get(6)?,
                archive_password: r.get(7)?,
                last_ok: r.get(8)?,
                fail_count: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
                last_error: r.get(10)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub(crate) fn count_download_links(db: &Db) -> Result<i64> {
    let handle = db.read()?;
    let n = handle.query_row("SELECT COUNT(*) FROM download_links", [], |r| r.get(0))?;
    Ok(n)
}

/// The archive member and password stored for a link, when the link is a zip.
pub(crate) fn find_link_archive(db: &Db, filename: &str, url: &str) -> Option<(String, Option<String>)> {
    let handle = db.read().ok()?;
    handle
        .query_row(
            "SELECT archive_entry, archive_password FROM download_links
             WHERE filename = ?1 AND url = ?2 AND archive_entry IS NOT NULL",
            params![filename, url],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .ok()
}

/// Every stored link for exactly this file (any host), best first: links that
/// last worked, then untried ones, then failing ones (fewest failures first).
pub(crate) fn links_for_file(db: &Db, filename: &str) -> Vec<DownloadLinkRow> {
    let Ok(handle) = db.read() else { return Vec::new() };
    let Ok(mut stmt) = handle.prepare(&format!(
        "SELECT {cols} FROM download_links WHERE filename = ?1 COLLATE NOCASE",
        cols = "package_base, version, filename, host, url, size, archive_entry, archive_password, last_ok, fail_count, last_error"
    )) else {
        return Vec::new();
    };
    let rows = stmt.query_map(params![filename], |r| {
        Ok(DownloadLinkRow {
            package_base: r.get(0)?,
            version: r.get(1)?,
            filename: r.get(2)?,
            host: r.get(3)?,
            url: r.get(4)?,
            size: r.get(5)?,
            archive_entry: r.get(6)?,
            archive_password: r.get(7)?,
            last_ok: r.get(8)?,
            fail_count: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
            last_error: r.get(10)?,
        })
    });
    let mut out: Vec<DownloadLinkRow> = rows.map(|it| it.flatten().collect()).unwrap_or_default();
    out.sort_by_key(|l| (l.health_rank(), l.fail_count, std::cmp::Reverse(l.last_ok)));
    out
}

/// Records how a download through `url` for `filename` went. A success clears
/// the failure count; links that aren't stored (the Hub's) are left alone.
pub(crate) fn record_link_result(db: &Db, filename: &str, url: &str, error: Option<&str>) {
    let Ok(conn) = db.conn.lock() else { return };
    let _ = match error {
        None => conn.execute(
            "UPDATE download_links SET last_ok = strftime('%s','now'), fail_count = 0, last_error = NULL
             WHERE filename = ?1 COLLATE NOCASE AND url = ?2",
            params![filename, url],
        ),
        Some(e) => conn.execute(
            "UPDATE download_links SET fail_count = COALESCE(fail_count, 0) + 1, last_error = ?3
             WHERE filename = ?1 COLLATE NOCASE AND url = ?2",
            params![filename, url, e],
        ),
    };
}

/// Sets an archive link's member and password (insert is `OR IGNORE`, so a
/// re-added archive source updates them here — e.g. a password added later).
pub(crate) fn set_link_archive(db: &Db, row: &DownloadLinkRow) -> Result<()> {
    let conn = db
        .conn
        .lock()
        .map_err(|_| anyhow!("database connection poisoned"))?;
    conn.execute(
        "UPDATE download_links SET archive_entry = ?3, archive_password = ?4 WHERE filename = ?1 AND url = ?2",
        params![row.filename, row.url, row.archive_entry, row.archive_password],
    )?;
    Ok(())
}

/// Stored links whose file name contains `query` (case-insensitive; all when
/// empty), newest first, at most `limit`; and how many match in all.
pub(crate) fn search_download_links(db: &Db, query: &str, limit: u32) -> Result<(Vec<DownloadLinkRow>, u64)> {
    let handle = db.read()?;
    let pattern = format!("%{}%", query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let total: i64 = handle.query_row(
        "SELECT COUNT(*) FROM download_links WHERE filename LIKE ?1 ESCAPE '\\'",
        params![pattern],
        |r| r.get(0),
    )?;
    let mut stmt = handle.prepare(
        "SELECT package_base, version, filename, host, url, size, archive_entry, archive_password, last_ok, fail_count, last_error
         FROM download_links WHERE filename LIKE ?1 ESCAPE '\\' ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![pattern, limit], |r| {
            Ok(DownloadLinkRow {
                package_base: r.get(0)?,
                version: r.get(1)?,
                filename: r.get(2)?,
                host: r.get(3)?,
                url: r.get(4)?,
                size: r.get(5)?,
                archive_entry: r.get(6)?,
                archive_password: r.get(7)?,
                last_ok: r.get(8)?,
                fail_count: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
                last_error: r.get(10)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, total.max(0) as u64))
}

/// Removes one stored link. Returns whether a row was deleted.
pub(crate) fn remove_download_link(db: &Db, filename: &str, url: &str) -> Result<bool> {
    let conn = db
        .conn
        .lock()
        .map_err(|_| anyhow!("database connection poisoned"))?;
    let n = conn.execute(
        "DELETE FROM download_links WHERE filename = ?1 AND url = ?2",
        params![filename, url],
    )?;
    Ok(n > 0)
}

pub(crate) fn clear_download_links(db: &Db) -> Result<()> {
    let conn = db
        .conn
        .lock()
        .map_err(|_| anyhow!("database connection poisoned"))?;
    conn.execute("DELETE FROM download_links", [])?;
    Ok(())
}

/// Inserts every name in `SEEDED_CATEGORIES` into the `categories` table.
/// Idempotent — uses `INSERT OR IGNORE`. Called both from the v4 migration and
/// on every DB open so new entries added to the seed list later still land.
fn seed_categories(conn: &Connection) -> Result<()> {
    let mut stmt = conn
        .prepare("INSERT OR IGNORE INTO categories (name) VALUES (?1)")
        .context("failed to prepare category seed insert")?;
    for name in SEEDED_CATEGORIES {
        stmt.execute(params![name])
            .with_context(|| format!("failed to seed category {name}"))?;
    }
    Ok(())
}

/// Loads `(category_name → category_id)` for every seeded category. Called
/// once at DB open time and stashed on the `Db` struct so insert paths can do
/// a HashMap lookup instead of a per-row SQL query.
fn load_category_ids(conn: &Connection) -> Result<HashMap<&'static str, i64>> {
    let mut map = HashMap::with_capacity(SEEDED_CATEGORIES.len());
    let mut stmt = conn
        .prepare("SELECT category_id FROM categories WHERE name = ?1")
        .context("failed to prepare category id lookup")?;
    for &name in SEEDED_CATEGORIES {
        let id: i64 = stmt
            .query_row(params![name], |row| row.get(0))
            .with_context(|| format!("failed to load category id for {name}"))?;
        map.insert(name, id);
    }
    Ok(map)
}

/// Look up an existing creator id by name, or insert a new row, returning the
/// id either way. Uses `RETURNING` so it's a single round trip. Callers
/// typically wrap this in a per-transaction `HashMap<String, i64>` cache to
/// avoid hitting it once per package.
pub(crate) fn ensure_creator_id(conn: &Connection, name: &str) -> Result<i64> {
    conn.query_row(
        "INSERT INTO creators (name) VALUES (?1)
         ON CONFLICT(name) DO UPDATE SET name = excluded.name
         RETURNING creator_id",
        params![name],
        |row| row.get::<_, i64>(0),
    )
    .with_context(|| format!("failed to upsert creator {name}"))
}

/// Resolves `creator_id` for a `package_id`, inserting a `creators` row if
/// needed. Returns `Ok(None)` when the package_id has no creator prefix.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn ensure_creator_id_for_package(
    conn: &Connection,
    package_id: &str,
) -> Result<Option<i64>> {
    match naming::creator_from_package_id(package_id) {
        Some(name) => ensure_creator_id(conn, name).map(Some),
        None => Ok(None),
    }
}

pub(crate) const CREATOR_FLAG_NONE: i32 = 0;
pub(crate) const CREATOR_FLAG_FAVORITE: i32 = 1;
pub(crate) const CREATOR_FLAG_BLOCKED: i32 = 2;

pub(crate) fn set_creator_flag(conn: &Connection, name: &str, flag: i32) -> Result<()> {
    if !(CREATOR_FLAG_NONE..=CREATOR_FLAG_BLOCKED).contains(&flag) {
        return Err(anyhow!("invalid creator flag {flag}"));
    }
    let creator_id = ensure_creator_id(conn, name)?;
    conn.execute(
        "UPDATE creators SET flag = ?1 WHERE creator_id = ?2",
        params![flag, creator_id],
    )
    .with_context(|| format!("failed to update flag for creator {name}"))?;
    Ok(())
}

pub(crate) fn get_creator_flag(conn: &Connection, name: &str) -> Result<i32> {
    conn.query_row(
        "SELECT flag FROM creators WHERE name = ?1",
        params![name],
        |row| row.get::<_, i32>(0),
    )
    .or_else(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(CREATOR_FLAG_NONE),
        other => Err(other.into()),
    })
}

pub(crate) fn get_favorite_creator_names(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn
        .prepare("SELECT name FROM creators WHERE flag = ?1")
        .context("failed to prepare favorite creator query")?;
    let rows = stmt
        .query_map(params![CREATOR_FLAG_FAVORITE], |row| row.get::<_, String>(0))
        .context("failed to query favorite creators")?;
    let mut out = HashSet::new();
    for r in rows {
        out.insert(r.context("failed to read favorite creator row")?);
    }
    Ok(out)
}

pub(crate) fn get_blocked_creator_names(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn
        .prepare("SELECT name FROM creators WHERE flag = ?1")
        .context("failed to prepare blocked creator query")?;
    let rows = stmt
        .query_map(params![CREATOR_FLAG_BLOCKED], |row| row.get::<_, String>(0))
        .context("failed to query blocked creators")?;
    let mut out = HashSet::new();
    for r in rows {
        out.insert(r.context("failed to read blocked creator row")?);
    }
    Ok(out)
}

pub(crate) const PACKAGE_FLAG_NONE: i32 = 0;
pub(crate) const PACKAGE_FLAG_FAVORITE: i32 = 1;

/// Per-package flag, keyed by package_id text with no FK (see the v11
/// migration comment). Unlike creator flags, `package_flags` survives
/// `clear_database` — favorites are a user preference, not index data.
pub(crate) fn set_package_flag(conn: &Connection, package_id: &str, flag: i32) -> Result<()> {
    if !(PACKAGE_FLAG_NONE..=PACKAGE_FLAG_FAVORITE).contains(&flag) {
        return Err(anyhow!("invalid package flag {flag}"));
    }
    if flag == PACKAGE_FLAG_NONE {
        conn.execute(
            "DELETE FROM package_flags WHERE package_id = ?1",
            params![package_id],
        )
        .with_context(|| format!("failed to clear flag for package {package_id}"))?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO package_flags (package_id, flag) VALUES (?1, ?2)
         ON CONFLICT(package_id) DO UPDATE SET flag = excluded.flag",
        params![package_id, flag],
    )
    .with_context(|| format!("failed to set flag for package {package_id}"))?;
    Ok(())
}

pub(crate) fn get_package_flag(conn: &Connection, package_id: &str) -> Result<i32> {
    conn.query_row(
        "SELECT flag FROM package_flags WHERE package_id = ?1",
        params![package_id],
        |row| row.get::<_, i32>(0),
    )
    .or_else(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(PACKAGE_FLAG_NONE),
        other => Err(other.into()),
    })
}

pub(crate) fn get_favorite_package_ids(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn
        .prepare("SELECT package_id FROM package_flags WHERE flag = ?1")
        .context("failed to prepare favorite package query")?;
    let rows = stmt
        .query_map(params![PACKAGE_FLAG_FAVORITE], |row| row.get::<_, String>(0))
        .context("failed to query favorite packages")?;
    let mut out = HashSet::new();
    for r in rows {
        out.insert(r.context("failed to read favorite package row")?);
    }
    Ok(out)
}

/// The one place the "has a scene image" predicate is written. `tasks.rs`
/// re-uses `SCENE_IMAGE_PREDICATE_SQL` for the database-mode WHERE clause, so
/// folder mode and database mode can never drift apart. The v12 partial index
/// embeds this exact text as its WHERE — SQLite's partial-index implication
/// check is textual, so editing this string silently de-indexes the filter;
/// any change here needs a companion migration rebuilding the index.
pub(crate) const SCENE_IMAGE_PREDICATE_SQL: &str = "lower(internal_path) LIKE 'saves/scene/%' \
     AND (lower(internal_path) LIKE '%.jpg' OR lower(internal_path) LIKE '%.jpeg' \
          OR lower(internal_path) LIKE '%.png')";

/// Package ids that carry a `Saves/scene/` preview image, for the VAR Packages
/// "scene image" filter.
///
/// Read from `resources` rather than `packages.scene_image_path`: that column is
/// never populated by either ingest path (the scanner passes None, the manifest
/// insert hardcodes NULL), so filtering on it would match nothing. `resources`
/// holds every package's contentList, so this is accurate for anything in the
/// DB — a package that was never indexed simply isn't in the set.
///
/// Matched on `lower(internal_path)` so it is byte-for-byte the same predicate
/// as the exporter's `choose_scene_image`, which lowercases the whole entry name
/// before testing the prefix. A GLOB character class would miss `SAVES/SCENE/`,
/// and then a package could export an image the filter says it doesn't have.
/// `internal_path` is stored forward-slashed and untrimmed of case by
/// `normalize_zip_path`, so no slash handling is needed here.
///
/// Folder mode now reads the scene-image flag from the archive during the scan
/// (`library::read_var_pkg_info`), so only the tests pin this id set against
/// the database-mode SQL predicate.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn get_scene_image_package_ids(conn: &Connection) -> Result<HashSet<String>> {
    let sql =
        format!("SELECT DISTINCT package_id FROM resources WHERE {SCENE_IMAGE_PREDICATE_SQL}");
    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare scene image package query")?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .context("failed to query scene image packages")?;
    let mut out = HashSet::new();
    for r in rows {
        out.insert(r.context("failed to read scene image package row")?);
    }
    Ok(out)
}

fn category_id_for_path(
    category_ids: &HashMap<&'static str, i64>,
    internal_path: &str,
) -> Option<i64> {
    let name = naming::category_name_for_path(internal_path);
    category_ids.get(name).copied()
}

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) const PACKAGE_UPSERT_SQL: &str =
    "INSERT INTO packages (package_id, file_path, size_bytes, modified_ns, scene_image_path, last_scanned_at, creator_id)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
     ON CONFLICT(package_id) DO UPDATE SET
        file_path        = excluded.file_path,
        size_bytes       = excluded.size_bytes,
        modified_ns      = excluded.modified_ns,
        scene_image_path = excluded.scene_image_path,
        last_scanned_at  = excluded.last_scanned_at,
        creator_id       = COALESCE(excluded.creator_id, packages.creator_id)";

pub(crate) const RESOURCE_UPSERT_SQL: &str =
    "INSERT INTO resources (package_id, internal_path, crc32, size, effective_size, category_id)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
     ON CONFLICT(package_id, internal_path) DO UPDATE SET
        crc32          = excluded.crc32,
        size           = excluded.size,
        effective_size = excluded.effective_size,
        category_id    = COALESCE(excluded.category_id, resources.category_id)";

pub(crate) const MANIFEST_PACKAGE_INSERT_SQL: &str =
    "INSERT OR IGNORE INTO packages
        (package_id, file_path, size_bytes, modified_ns, scene_image_path, last_scanned_at, creator_id)
     VALUES (?1, ?2, 0, '0', NULL, ?3, ?4)";

pub(crate) const MANIFEST_RESOURCE_INSERT_SQL: &str =
    "INSERT OR IGNORE INTO resources
        (package_id, internal_path, crc32, size, effective_size, category_id)
     VALUES (?1, ?2, ?3, 0, 0, ?4)";

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn upsert_package(
    conn: &Connection,
    package_id: &str,
    file_path: &str,
    size_bytes: u64,
    modified_ns: u128,
    scene_image_path: Option<&str>,
) -> Result<()> {
    let creator_id = ensure_creator_id_for_package(conn, package_id)?;
    conn.execute(
        PACKAGE_UPSERT_SQL,
        params![
            package_id,
            file_path,
            size_bytes as i64,
            modified_ns.to_string(),
            scene_image_path,
            now_unix_seconds(),
            creator_id,
        ],
    )
    .context("failed to upsert package row")?;
    Ok(())
}

pub(crate) fn execute_package_upsert(
    stmt: &mut rusqlite::Statement<'_>,
    package_id: &str,
    file_path: &str,
    size_bytes: u64,
    modified_ns: u128,
    scene_image_path: Option<&str>,
    creator_id: Option<i64>,
) -> Result<()> {
    stmt.execute(params![
        package_id,
        file_path,
        size_bytes as i64,
        modified_ns.to_string(),
        scene_image_path,
        now_unix_seconds(),
        creator_id,
    ])
    .with_context(|| format!("failed to upsert package row {package_id}"))?;
    Ok(())
}

pub(crate) fn execute_resource_upsert(
    stmt: &mut rusqlite::Statement<'_>,
    resource: &ResourceRef,
    category_ids: &HashMap<&'static str, i64>,
) -> Result<()> {
    let category_id = category_id_for_path(category_ids, &resource.internal_path);
    stmt.execute(params![
        resource.package_id,
        resource.internal_path,
        resource.crc32.map(|v| v as i64),
        resource.size as i64,
        resource.effective_size as i64,
        category_id,
    ])
    .with_context(|| {
        format!(
            "failed to upsert resource {} for package {}",
            resource.internal_path, resource.package_id
        )
    })?;
    Ok(())
}

pub(crate) fn execute_manifest_package_insert(
    stmt: &mut rusqlite::Statement<'_>,
    package_id: &str,
    creator_id: Option<i64>,
) -> Result<usize> {
    let placeholder = format!("manifest:{package_id}");
    let changed = stmt
        .execute(params![package_id, placeholder, now_unix_seconds(), creator_id])
        .with_context(|| format!("failed to insert manifest package row {package_id}"))?;
    Ok(changed)
}

pub(crate) fn execute_manifest_resource_insert(
    stmt: &mut rusqlite::Statement<'_>,
    package_id: &str,
    internal_path: &str,
    crc32: u32,
    category_ids: &HashMap<&'static str, i64>,
) -> Result<usize> {
    let category_id = category_id_for_path(category_ids, internal_path);
    let changed = stmt
        .execute(params![package_id, internal_path, crc32 as i64, category_id])
        .with_context(|| {
            format!(
                "failed to insert manifest resource {internal_path} for {package_id}"
            )
        })?;
    Ok(changed)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn replace_resources(
    conn: &mut Connection,
    package_id: &str,
    resources: &[ResourceRef],
) -> Result<()> {
    let category_ids = load_category_ids(conn)?;
    let tx = conn
        .transaction()
        .context("failed to start replace_resources tx")?;

    {
        let mut stmt = tx
            .prepare(RESOURCE_UPSERT_SQL)
            .context("failed to prepare upsert statement")?;
        for resource in resources {
            execute_resource_upsert(&mut stmt, resource, &category_ids).with_context(|| {
                format!(
                    "failed to upsert resources for package {}",
                    package_id
                )
            })?;
        }
    }

    // Note: rows for internal paths that were present in a previous scan but
    // missing in this one are intentionally left in place. The user has asked
    // to keep historical resource records even if a later scan of the package
    // no longer contains that file.

    tx.commit().context("failed to commit replace_resources")?;
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn load_package_fingerprint(
    conn: &Connection,
    file_path: &str,
) -> Result<Option<(u64, u128)>> {
    let row = conn
        .query_row(
            "SELECT size_bytes, modified_ns FROM packages WHERE file_path = ?1",
            params![file_path],
            |row| {
                let size: i64 = row.get(0)?;
                let modified_ns_text: String = row.get(1)?;
                Ok((size, modified_ns_text))
            },
        )
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .context("failed to read package fingerprint")?;

    Ok(row.map(|(size, modified_ns_text)| {
        let modified_ns = modified_ns_text.parse::<u128>().unwrap_or(0);
        (size as u64, modified_ns)
    }))
}

pub(crate) fn load_resources_for_package(
    conn: &Connection,
    package_id: &str,
    package_file: &str,
) -> Result<Vec<ResourceRef>> {
    let mut stmt = conn
        .prepare(
            "SELECT internal_path, crc32, size, effective_size
             FROM resources
             WHERE package_id = ?1
             ORDER BY internal_path",
        )
        .context("failed to prepare load_resources statement")?;
    let rows = stmt
        .query_map(params![package_id], |row| {
            let internal_path: String = row.get(0)?;
            let crc32: Option<i64> = row.get(1)?;
            let size: i64 = row.get(2)?;
            let effective_size: Option<i64> = row.get(3)?;
            Ok(ResourceRef {
                package_id: package_id.to_string(),
                package_file: package_file.to_string(),
                internal_path,
                crc32: crc32.map(|v| v as u32),
                size: size as u64,
                effective_size: effective_size.unwrap_or(size) as u64,
            })
        })
        .context("failed to query resources")?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row.context("failed to read resource row")?);
    }
    Ok(out)
}

/// Single-row resource lookup for the execute-time DB fallback. Used when a
/// keep_value points to a package that wasn't part of the local scan (e.g. a
/// catalog/manifest entry chosen as the relocation target on the Find
/// Duplicates page). Returns the package's recorded `file_path` so callers
/// can populate `ResourceRef::package_file`.
pub(crate) fn load_resource_by_pid_and_path(
    conn: &Connection,
    package_id: &str,
    internal_path: &str,
) -> Result<Option<ResourceRef>> {
    let mut stmt = conn
        .prepare(
            "SELECT p.file_path, r.crc32, r.size, r.effective_size
             FROM resources r
             JOIN packages p ON p.package_id = r.package_id
             WHERE r.package_id = ?1 AND r.internal_path = ?2",
        )
        .context("failed to prepare load_resource_by_pid_and_path")?;
    let row = stmt
        .query_row(params![package_id, internal_path], |row| {
            let file_path: String = row.get(0)?;
            let crc32: Option<i64> = row.get(1)?;
            let size: i64 = row.get(2)?;
            let effective_size: Option<i64> = row.get(3)?;
            Ok(ResourceRef {
                package_id: package_id.to_string(),
                package_file: file_path,
                internal_path: internal_path.to_string(),
                crc32: crc32.map(|v| v as u32),
                size: size as u64,
                effective_size: effective_size.unwrap_or(size) as u64,
            })
        })
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .context("failed to query resource by pid + path")?;
    Ok(row)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn prune_missing_packages(
    conn: &mut Connection,
    present_paths: &HashSet<String>,
) -> Result<usize> {
    let tx = conn.transaction().context("failed to start prune tx")?;
    let mut removed = 0usize;
    {
        let mut select = tx
            .prepare("SELECT file_path FROM packages")
            .context("failed to prepare prune select")?;
        let mut delete = tx
            .prepare("DELETE FROM packages WHERE file_path = ?1")
            .context("failed to prepare prune delete")?;

        let stored: Vec<String> = select
            .query_map([], |row| row.get::<_, String>(0))
            .context("failed to query stored paths")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to read stored paths")?;

        for path in stored {
            if !present_paths.contains(&path) {
                delete
                    .execute(params![path])
                    .context("failed to delete stale package row")?;
                removed += 1;
            }
        }
    }
    tx.commit().context("failed to commit prune")?;
    Ok(removed)
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DbStats {
    pub(crate) package_count: i64,
    pub(crate) resource_count: i64,
    pub(crate) db_size_bytes: u64,
}

pub(crate) fn database_stats(app: &tauri::AppHandle, db: &Db) -> Result<DbStats> {
    let conn = db
        .conn
        .lock()
        .map_err(|_| anyhow!("database connection poisoned"))?;
    let package_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM packages", [], |row| row.get(0))?;
    let resource_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM resources", [], |row| row.get(0))?;

    let db_size_bytes = db_path(app)
        .ok()
        .and_then(|path| fs::metadata(path).ok())
        .map(|meta| meta.len())
        .unwrap_or(0);

    Ok(DbStats {
        package_count,
        resource_count,
        db_size_bytes,
    })
}

// Default chunk size for `WHERE crc32 IN (?, ?, ...)` lookups.
// SQLite's default `SQLITE_LIMIT_VARIABLE_NUMBER` is 999 in older builds and
// 32766 in modern ones — 500 is safely below both and keeps each prepared
// statement compact enough that the join with `packages` stays cheap.
const CRC_LOOKUP_CHUNK: usize = 500;

// LEFT JOIN + COALESCE so legacy `packages` rows without a `creator_id` are
// still included; we only exclude rows whose creator's flag is set to BLOCKED.
const CRC_MATCH_QUERY_PREFIX: &str =
    "SELECT r.crc32, r.package_id, p.file_path, r.internal_path, r.size
     FROM resources r
     JOIN packages p ON p.package_id = r.package_id
     LEFT JOIN creators c ON c.creator_id = p.creator_id
     WHERE COALESCE(c.flag, 0) != 2
       AND r.crc32 IN (";

/// Bulk lookup of every `(package, internal_path, size)` row in the indexed
/// `resources` table whose `crc32` matches any of `crcs`, excluding any rows
/// belonging to a package id in `exclude_package_ids`.
///
/// The query is chunked at [`CRC_LOOKUP_CHUNK`] CRCs per statement to stay
/// below SQLite's parameter limit and to keep each indexed range scan cheap.
/// Optimization relies on the existing `idx_resources_crc32` index from the
/// v2 schema migration.
///
/// `on_chunk(processed, total)` is invoked after each chunk so callers can
/// report progress.
pub(crate) fn find_crc_matches_bulk<F: FnMut(usize, usize)>(
    conn: &Connection,
    crcs: &[u32],
    exclude_package_ids: &HashSet<String>,
    mut on_chunk: F,
) -> Result<HashMap<u32, Vec<DbFindRef>>> {
    let mut out: HashMap<u32, Vec<DbFindRef>> = HashMap::new();
    if crcs.is_empty() {
        return Ok(out);
    }

    let total = crcs.len();
    let mut processed = 0usize;

    // Prepare once at the full chunk size and reuse it for every full chunk;
    // only the (possibly short) tail needs a second prepare. Saves ceil(N/500)
    // format!+prepare calls down to at most 2 across the whole batch.
    let full_chunks = crcs.chunks_exact(CRC_LOOKUP_CHUNK);
    let tail = full_chunks.remainder();
    let mut full_stmt = if crcs.len() >= CRC_LOOKUP_CHUNK {
        let placeholders = vec!["?"; CRC_LOOKUP_CHUNK].join(",");
        let sql = format!("{}{})", CRC_MATCH_QUERY_PREFIX, placeholders);
        Some(
            conn.prepare(&sql)
                .context("failed to prepare find_crc_matches_bulk full chunk")?,
        )
    } else {
        None
    };

    let run_chunk = |stmt: &mut rusqlite::Statement<'_>,
                     chunk: &[u32],
                     out: &mut HashMap<u32, Vec<DbFindRef>>|
     -> Result<()> {
        let params_iter = chunk.iter().map(|c| *c as i64);
        let mapped = stmt
            .query_map(rusqlite::params_from_iter(params_iter), |row| {
                let crc: i64 = row.get(0)?;
                let package_id: String = row.get(1)?;
                let file_path: String = row.get(2)?;
                let internal_path: String = row.get(3)?;
                let size: i64 = row.get(4)?;
                Ok((crc as u32, package_id, file_path, internal_path, size))
            })
            .context("failed to query find_crc_matches_bulk chunk")?;
        for row in mapped {
            let (crc, package_id, file_path, internal_path, size) =
                row.context("failed to read crc match row")?;
            if exclude_package_ids.contains(&package_id) {
                continue;
            }
            out.entry(crc).or_default().push(DbFindRef {
                package_id,
                file_path,
                internal_path,
                size,
            });
        }
        Ok(())
    };

    if let Some(stmt) = full_stmt.as_mut() {
        for chunk in crcs.chunks_exact(CRC_LOOKUP_CHUNK) {
            run_chunk(stmt, chunk, &mut out)?;
            processed += chunk.len();
            on_chunk(processed, total);
        }
    }

    if !tail.is_empty() {
        let placeholders = vec!["?"; tail.len()].join(",");
        let sql = format!("{}{})", CRC_MATCH_QUERY_PREFIX, placeholders);
        let mut tail_stmt = conn
            .prepare(&sql)
            .context("failed to prepare find_crc_matches_bulk tail chunk")?;
        run_chunk(&mut tail_stmt, tail, &mut out)?;
        processed += tail.len();
        on_chunk(processed, total);
    }

    Ok(out)
}

/// Wipe every row from packages / resources / creators. Reports progress
/// through `on_progress(phase, fraction_0_to_1, message)` so the caller can
/// stream updates to a Tauri task. Pass a no-op closure if you don't need
/// progress.
pub(crate) fn clear_all<F: FnMut(&str, f64, &str)>(db: &Db, mut on_progress: F) -> Result<()> {
    on_progress("clear_db_prepare", 0.02, "Disabling foreign keys");
    let mut conn = db
        .conn
        .lock()
        .map_err(|_| anyhow!("database connection poisoned"))?;

    // Hot path for users with millions of resources. With `foreign_keys = ON`
    // the CASCADE relationship from `resources` to `packages` forces SQLite to
    // walk every row, journalling each delete — that's the source of the
    // multi-second freeze on 4M-row DBs. Disabling FKs lets the un-WHERE'd
    // DELETEs hit SQLite's O(1) "truncate optimization" instead.
    //
    // PRAGMA foreign_keys cannot be toggled inside a transaction, so we wrap
    // the work in a closure and always restore the pragma — a failure mid-way
    // would otherwise leave the shared connection with FKs disabled.
    conn.execute_batch("PRAGMA foreign_keys = OFF;")
        .context("failed to disable foreign keys")?;

    let result = (|| -> Result<()> {
        let tx = conn.transaction().context("failed to start clear tx")?;
        on_progress("clear_db_resources", 0.10, "Clearing resources");
        tx.execute("DELETE FROM resources", [])
            .context("failed to clear resources")?;
        on_progress("clear_db_packages", 0.55, "Clearing packages");
        tx.execute("DELETE FROM packages", [])
            .context("failed to clear packages")?;
        // Creators are reference rows attached to packages; without packages
        // they're orphans. Categories are seeded reference data — keep them.
        // package_flags is deliberately NOT cleared: package favorites are a
        // user preference, not index data (creator favorites die here only
        // because they live on the creators rows themselves).
        on_progress("clear_db_creators", 0.72, "Clearing creators");
        tx.execute("DELETE FROM creators", [])
            .context("failed to clear creators")?;
        on_progress("clear_db_sequence", 0.85, "Resetting sequences");
        // Reset the AUTOINCREMENT counters so the next scan starts from id 1
        // — also a no-op cost, but a nice side effect of the truncate path.
        tx.execute(
            "DELETE FROM sqlite_sequence WHERE name IN ('resources', 'creators')",
            [],
        )
        .context("failed to reset sqlite_sequence")?;
        tx.commit().context("failed to commit clear")?;
        Ok(())
    })();

    // Always restore FK enforcement, even on failure. apply_pragmas at the
    // next open would also restore it, but within this session leaving it OFF
    // would silently allow integrity violations on subsequent inserts.
    let restore = conn
        .execute_batch("PRAGMA foreign_keys = ON;")
        .context("failed to re-enable foreign keys");

    result.and(restore)
}
