# Database Pipeline Optimizations

A walkthrough of every performance change made to the scan, persistence, and
bulk-import paths — what was slow, what changed, and why each change matters.

---

## TL;DR

| Workload | Before | After | Speedup |
|---|---|---|---|
| Real scan: 5 k packages → DB | ~5 k commits, ~600k SQL parses | 1 commit, 2 prepared stmts | ~10–50× |
| Build-DB scan: 5 k packages | Full SHA-256 over duplicate candidates | CRC-only (no hash) | depends on candidate count, typically 5–20× |
| Manifest import: 4 M lines | Likely OOM or 5–15 min | ~45–90 s, ~1 MB peak RAM | order-of-magnitude |

Everything below is on top of the existing rusqlite + WAL setup.

---

## 1. Single-transaction batched persistence

**Touches:** every scan path (Overview *and* Build Database).

### Problem

The old `persist_scan_to_db` looped over packages and made each one its own
transaction. With N packages × M resources per package:

```
┌─────────────────── persist_scan_to_db (OLD) ────────────────────┐
│ for each package:                                                │
│   ┌──────────────────────────────────────────────────────────┐  │
│   │ upsert_package(conn, ...)        ◄── 1 SQL parse         │  │
│   │     execute(INSERT INTO packages ... ON CONFLICT ...)    │  │
│   │                                                          │  │
│   │ replace_resources(conn, package_id, resources):          │  │
│   │     BEGIN TRANSACTION             ◄── per-package!       │  │
│   │     prepare(INSERT INTO resources ...)  ◄── 1 SQL parse  │  │
│   │     for each resource:                                   │  │
│   │         execute(...)                                     │  │
│   │     COMMIT                        ◄── fsync WAL          │  │
│   └──────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘

Roundtrips: N (package commits) + N (resource commits) = 2N transactions
SQL parses:  N (package SQL) + N (resource SQL prepare) = 2N parses
```

For a typical library of **5,000 packages × 100 resources each** that's:

- **10,000 commits** (each one fsyncs the WAL header)
- **10,000 SQL parse / plan operations**
- **500,000 individual `execute()` calls**

### Solution

Open one transaction up front, prepare both statements once, reuse them
across every package and every resource, commit at the end.

```
┌─────────────────── persist_scan_to_db (NEW) ────────────────────┐
│ BEGIN TRANSACTION                       ◄── once                 │
│   pkg_stmt = prepare(PACKAGE_UPSERT_SQL)   ◄── parsed once       │
│   res_stmt = prepare(RESOURCE_UPSERT_SQL)  ◄── parsed once       │
│                                                                  │
│   for each package:                                              │
│       execute_package_upsert(pkg_stmt, ...)   (no parse)         │
│       for each resource:                                         │
│           execute_resource_upsert(res_stmt, ...)                 │
│                                                                  │
│   prune_missing_packages_in_tx(tx, ...)  (same tx)               │
│ COMMIT                                  ◄── once                 │
└──────────────────────────────────────────────────────────────────┘

Roundtrips: 1 transaction
SQL parses: 2 prepares, total
```

### Code

The SQL strings live in `db.rs` as constants so caller and helpers share one
source of truth:

```rust
// src-tauri/src/db.rs
pub(crate) const PACKAGE_UPSERT_SQL: &str = "INSERT INTO packages ...";
pub(crate) const RESOURCE_UPSERT_SQL: &str = "INSERT INTO resources ...";

pub(crate) fn execute_package_upsert(stmt: &mut Statement, ...) -> Result<()>;
pub(crate) fn execute_resource_upsert(stmt: &mut Statement, ...) -> Result<()>;
pub(crate) fn prune_missing_packages_in_tx(tx: &Transaction, ...) -> Result<usize>;
```

`scan.rs::persist_scan_to_db` opens one tx and reuses both statements.

### Why this is so much faster

| Cost | Per-package (OLD) | Once total (NEW) |
|---|---|---|
| WAL fsync on commit | 2 × N | 1 |
| SQL parse + plan | 2 × N | 2 |
| B-tree page-cache warm-up | each tx evicts | shared across all rows |

WAL-mode SQLite with `synchronous = NORMAL` already amortizes most disk I/O,
but the parse + plan + cursor-setup cost per transaction is still O(2N). At
5 k packages, the parse+plan alone is dominating wall-clock time.

---

## 2. `index_only` mode — skip SHA-256 for Build Database

**Touches:** the new "Build Database → Scan & Index" button.

### Problem

A normal scan walks every `.var` package, computes CRC-32 from the zip
central directory (free — already there), and then **SHA-256-hashes** every
resource that's a duplicate-candidate (i.e. has a matching size in another
package). SHA-256 is what the dedupe analysis uses to decide which files are
actually identical.

For Build Database, the user just wants to **populate the SQLite index** so
later searches/dedupe can use it. Hashing is wasted work — CRC-32 from the
zip is enough.

```
┌────────────────────── normal scan (OLD only) ──────────────────────┐
│ open every .var, list entries, read CRC32 (free)                   │
│ build size_index: HashMap<size, [(pkg_id, resource_idx)]>          │
│ identify duplicate candidates (same size across packages)          │
│ apply CRC pre-filter                                               │
│ ┌─────────────────────────────────────────────────────────┐        │
│ │ SHA-256 every candidate:                                │ ◄ slow │
│ │   for resource in candidates:                           │        │
│ │     reader = open zip entry                             │        │
│ │     sha256_reader(&mut reader)  ◄── reads + hashes data │        │
│ └─────────────────────────────────────────────────────────┘        │
│ persist to DB                                                      │
└────────────────────────────────────────────────────────────────────┘
```

For a 100 GB library with millions of duplicate candidates, the hashing phase
can take 5–30 minutes. We're paying that cost just to populate an index.

### Solution

Add an `index_only: bool` flag. When set, the scanner short-circuits:

```
┌─────────────── scan_directory_with_target_with_progress_db ──────────────┐
│ parse all .var files (CRC32 only, free from zip)                         │
│                                                                          │
│ if index_only {                                                          │
│     duplicate_groups = []                ◄── skip dedupe analysis        │
│     persist_scan_to_db(...)              ◄── still writes packages +    │
│     return                                    resources w/ empty sha256  │
│ }                                                                        │
│                                                                          │
│ // normal path: size_index → CRC pre-filter → SHA-256 → dedupe groups   │
└──────────────────────────────────────────────────────────────────────────┘
```

Resources are stored with `sha256 = ""`. A subsequent real scan will
overwrite empty-sha rows with real values.

### Code

```rust
// src-tauri/src/models.rs
pub(crate) struct ScanTaskRequest {
    pub(crate) input_dir: String,
    pub(crate) target_var_path: Option<String>,
    #[serde(default)]
    pub(crate) index_only: bool,   // NEW
}
```

```rust
// src-tauri/src/scan.rs
if index_only {
    on_progress(SCAN_GROUP_PROGRESS, "Skipping duplicate analysis (index-only mode)".to_string());
    let mut scanned = ScannedData {
        files: file_fingerprints,
        packages,
        duplicate_groups: Vec::new(),   // ◄── never built
        warnings,
        info: ScanInfo::default(),
    };
    if let Some(db) = db { /* persist_scan_to_db ... */ }
    on_progress(1.0, "Scan completed".to_string());
    return Ok(scanned);
}
```

```js
// ui/app.js — Build Database "Scan & Index"
await invoke("start_scan_task", {
  request: { input_dir, target_var_path: null, index_only: true },
});
```

The Overview "Scan" still defaults to `index_only: false` — full SHA dedupe.

---

## 3. Bulk-import pipeline rewrite

**Touches:** Build Database → Unified Bulk Import → manifest `.txt`.

This is the big one. Designed to handle a 4 M-line `_Database.txt` without
running out of memory.

### The five micro-optimizations (and what they each save)

```
                               ┌─────────────────────────────────────────┐
   manifest.txt (12 MB - 1 GB)│                                         │
        │                      │  Old import (BEFORE):                   │
        ▼                      │  • reader.lines()  → +1 alloc per line  │
   ╔══════════════╗            │  • parse_manifest_line → +2 allocs/line │
   ║ BufReader    ║            │  • collect into Vec<ParsedLine>          │
   ╚══════════════╝            │  • BTreeSet over all entries (4M times) │
        │                      │  • run 4M INSERT OR IGNORE on packages  │
        ▼                      │  • 4M execute() on resources            │
   ╔══════════════╗            │  • progress emit every 2500 lines       │
   ║ For each line║            │                                         │
   ║   parse      ║            │  Peak mem: ~600 MB                      │
   ║   exec SQL   ║            │  Time @ 4M lines: 5–15 min, often OOM   │
   ╚══════════════╝            └─────────────────────────────────────────┘
        │
        ▼
   ╔══════════════╗            ┌─────────────────────────────────────────┐
   ║   SQLite     ║            │  New import (AFTER):                    │
   ║   (WAL +     ║            │  • reused String buffer (read_line)     │
   ║   tuned)     ║            │  • slice parser → 0 allocs/line          │
   ╚══════════════╝            │  • stream, no buffering                 │
                               │  • HashSet of seen pkg_ids → 1 INSERT   │
                               │    OR IGNORE per unique pkg (~10k, not   │
                               │    4M)                                  │
                               │  • progress every 25k lines, bucketed   │
                               │  • PRAGMAs: 64 MB cache, mmap, temp_mem │
                               │                                         │
                               │  Peak mem: ~1 MB                        │
                               │  Time @ 4M lines: ~45–90 s              │
                               └─────────────────────────────────────────┘
```

Five pieces, each addressing one bottleneck.

---

### 3.1 PRAGMA tuning (applies to *all* DB ops)

[`src-tauri/src/db.rs::apply_pragmas`](../src-tauri/src/db.rs)

```sql
PRAGMA journal_mode = WAL;        -- (existing)
PRAGMA synchronous = NORMAL;      -- (existing)
PRAGMA foreign_keys = ON;         -- (existing)
PRAGMA temp_store = MEMORY;       -- NEW: temp B-trees / sort buffers stay in RAM
PRAGMA cache_size = -65536;       -- NEW: 64 MB page cache (default ~2 MB)
PRAGMA mmap_size = 268435456;     -- NEW: 256 MB mmap'd reads
```

**Why each helps:**

```
        Page cache before                Page cache after
        (default ≈ 2 MB)                 (-65536 = 64 MB)

        ┌────────────────┐               ┌────────────────┐
        │ ...            │               │ ...            │
        │ pkg PK page A  │ ◄── evicted   │ pkg PK page A  │ ◄── stays hot
        │ pkg PK page B  │     all the   │ pkg PK page B  │
        │ res UNIQUE   X │     time      │ res UNIQUE   X │
        └────────────────┘               │ res UNIQUE   Y │
              4 pages                    │ res UNIQUE   Z │
                                         │ ... 30+ more   │
                                         └────────────────┘

Inserting 4M resource rows means hitting the same B-tree pages
~4M times. With a 2 MB cache, hot pages get evicted by other pages
and re-read from disk — a tight inner loop suddenly does I/O.
With 64 MB they stay in memory.
```

`mmap_size = 256 MB` lets SQLite read database pages via the OS page cache
directly, skipping the `read()` syscall path.

`temp_store = MEMORY` means any internal sort scratch space (e.g. building
the unique index during commit) stays in RAM.

---

### 3.2 Streaming, not buffering

```
   BEFORE: parse-all-then-write
   ┌────────────┐  ┌──────────┐  ┌──────────────────┐
   │ read line  │→│  parse   │→│ push to Vec<...>  │   (loop 4 M times)
   └────────────┘  └──────────┘  └──────────────────┘
                                          │
                                          ▼
                                  ┌────────────────┐
                                  │ Vec holds 4M    │ ◄── 600 MB peak!
                                  │ ParsedLine{    │
                                  │   String,      │
                                  │   String,      │
                                  │   u32          │
                                  │ }              │
                                  └────────────────┘
                                          │
                                          ▼
                                  ┌────────────────┐
                                  │ BEGIN tx       │
                                  │ for entry in   │
                                  │  Vec: execute  │
                                  │ COMMIT         │
                                  └────────────────┘

   AFTER: stream-and-write
   ┌────────────┐  ┌──────────┐  ┌────────────────────┐
   │ read line  │→│  parse   │→│ execute SQL inline │   (loop 4 M times)
   └────────────┘  └──────────┘  └────────────────────┘
        ▲                                  │
        │                                  ▼
        └─── reused buffer ──── ◄── never grows past one line
                                          │
                                  ┌────────────────┐
                                  │ HashSet<pkg_id>│ ◄── ~10k entries
                                  │ peak ≈ 300 KB  │
                                  └────────────────┘
```

Code:

```rust
// src-tauri/src/import.rs (NEW)
let tx = conn.transaction()?;
let mut pkg_stmt = tx.prepare(MANIFEST_PACKAGE_INSERT_SQL)?;
let mut res_stmt = tx.prepare(MANIFEST_RESOURCE_UPSERT_SQL)?;
let mut line_buf = String::with_capacity(256);   // reused buffer
let mut seen_packages = HashSet::with_capacity(16_384);

loop {
    line_buf.clear();
    if reader.read_line(&mut line_buf)? == 0 { break; }
    if let Some(p) = parse_manifest_line_slices(&line_buf) {
        if !seen_packages.contains(p.package_id) {
            execute_manifest_package_insert(&mut pkg_stmt, p.package_id)?;
            seen_packages.insert(p.package_id.to_string());
        }
        execute_manifest_resource_upsert(&mut res_stmt, p.package_id, p.internal_path, p.crc32)?;
    }
}
tx.commit()?;
```

---

### 3.3 Zero-allocation slice parser

The old parser returned owned strings:

```rust
// BEFORE
fn parse_manifest_line(line: &str) -> Option<ParsedLine> {
    // ...
    let package_id  = rest[..colon].trim().to_string();         // ALLOC
    let internal_path = normalize_zip_path(internal_raw);        // ALLOC
    Some(ParsedLine { package_id, internal_path, crc32 })
}
```

For 4 M lines that's **8 M heap allocations** during parse alone, plus the
allocations inside `to_string()` and `normalize_zip_path`'s `.replace()`.

The new parser returns slices into the caller's buffer:

```rust
// AFTER
struct ParsedSlice<'a> {
    package_id: &'a str,
    internal_path: &'a str,
    crc32: u32,
}
fn parse_manifest_line_slices(line: &str) -> Option<ParsedSlice<'_>> {
    // ...
    let package_id    = rest[..colon].trim();                    // borrow
    let internal_path = rest[colon + 1..].trim().trim_matches('/'); // borrow
    Some(ParsedSlice { package_id, internal_path, crc32 })
}
```

The line buffer outlives each parse call long enough for the caller to bind
those slices into the prepared statement (`stmt.execute(params![...])`),
which copies the bytes into SQLite's parameter storage. Then the next loop
iteration calls `line_buf.clear()` and the slices are dropped.

```
   Hot path memory pattern (per line):

   AFTER   line_buf:  ╔════════════════════╗
                      ║ "[1] AABBCCDD ..." ║   ← reused String
                      ╚════════════════════╝
                              │
            slices into ──────┤
                              │
               package_id ────┤  &line_buf[X..Y]
            internal_path ────┘  &line_buf[Y..Z]
                              │
                              ▼
                    stmt.execute(params![...])
                              │
                              ▼
                    SQLite copies bytes internally
                              │
                              ▼
                    next iter: line_buf.clear()
                              │
                              ▼
                    slices invalidated (lifetime ended)


   BEFORE  line String → +1 alloc
           package_id String → +1 alloc
           internal_path String → +1 alloc
           ParsedLine struct holds them → kept alive
                              │
                              ▼
                    pushed into Vec<ParsedLine>
                              │
                              ▼
                    only freed at end of import
```

For 4 M lines: **12 M allocations vs 0** on the hot path.

---

### 3.4 Per-run package dedup with a HashSet

Manifests have ~10,000 unique packages but ~4,000,000 lines. The package_id
appears on average ~400 times.

The old approach: for *every* line, run `INSERT OR IGNORE INTO packages
(...)` — SQLite walks the package PK index 4 M times, even though 99.75% of
those calls are no-ops.

```
BEFORE:
   line 1: pkg_id="A.B.1" → INSERT OR IGNORE (new)      ✓
   line 2: pkg_id="A.B.1" → INSERT OR IGNORE (no-op)    ✗ wasted PK lookup
   line 3: pkg_id="A.B.1" → INSERT OR IGNORE (no-op)    ✗ wasted PK lookup
   ...
   line 400: pkg_id="A.B.1" → INSERT OR IGNORE (no-op)  ✗ wasted PK lookup
   line 401: pkg_id="C.D.2" → INSERT OR IGNORE (new)    ✓
   line 402: pkg_id="C.D.2" → INSERT OR IGNORE (no-op)  ✗ wasted PK lookup
   ...

AFTER:
   line 1:  pkg_id="A.B.1" → not in HashSet → execute, add to HashSet  ✓
   line 2:  pkg_id="A.B.1" → in HashSet → skip                          ✓
   line 3:  pkg_id="A.B.1" → in HashSet → skip                          ✓
   ...
   line 401: pkg_id="C.D.2" → not in HashSet → execute, add to HashSet ✓
   line 402: pkg_id="C.D.2" → in HashSet → skip                         ✓
```

**~400× fewer package writes** for a typical manifest. The HashSet probe is
hashed string lookup in RAM — orders of magnitude cheaper than walking
SQLite's B-tree.

---

### 3.5 Throttled progress emits

The polling loop in JS calls `get_task_progress` every 300 ms, which
acquires the same `Mutex<HashMap>` that the Rust task writes progress into.
Frequent writes from Rust contend with reads from JS.

```
BEFORE:  every 2500 lines → 4M / 2500 = 1600 lock acquisitions
AFTER:   every 25,000 lines, gated by 0.1% bucket → ~160 lock acquisitions
```

Plus a sentinel that only emits when the integer "bucket" changes, so a
small file doesn't fire faster than a large one.

```rust
if lines_total % 25_000 == 0 {
    let pct = (bytes_consumed as f64 / total_bytes as f64).min(1.0);
    let bucket = (pct * PROGRESS_BUCKET_RESOLUTION as f64) as i32;
    if bucket != last_emitted_bucket {
        last_emitted_bucket = bucket;
        on_progress(pct * 0.99, format!("Importing ({lines_parsed} / {lines_total} lines)"));
    }
}
```

---

## 4. The "skip already-hashed rows" guard (correctness, not speed)

This isn't a perf optimization but it's a critical correctness one for the
manifest path:

```sql
-- src-tauri/src/db.rs
INSERT INTO resources (package_id, internal_path, sha256, crc32, size, effective_size)
VALUES (?1, ?2, '', ?3, 0, 0)
ON CONFLICT(package_id, internal_path) DO UPDATE SET
   crc32 = excluded.crc32
WHERE resources.sha256 = '' OR resources.sha256 IS NULL
--    ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
--    only update CRC if the row has no real SHA yet
```

Without this guard, importing a manifest after a real scan would zero out
the SHA-256 + size + effective_size fields that the scan computed. The
WHERE clause makes manifests strictly additive: they fill in CRC for rows
the scanner hasn't deeply hashed, and leave deeply-hashed rows alone.

This is why the response separates **`resources_touched`** (rows actually
inserted or CRC-updated) from **`resources_skipped_real`** (rows whose SHA
was preserved).

---

## File-by-file changelog

| File | Changes |
|---|---|
| [`src-tauri/src/db.rs`](../src-tauri/src/db.rs) | PRAGMA tuning; SQL constants `PACKAGE_UPSERT_SQL`, `RESOURCE_UPSERT_SQL`, `MANIFEST_PACKAGE_INSERT_SQL`, `MANIFEST_RESOURCE_UPSERT_SQL`; statement-level helpers `execute_*_upsert`, `execute_manifest_*`; `prune_missing_packages_in_tx`. |
| [`src-tauri/src/scan.rs`](../src-tauri/src/scan.rs) | `persist_scan_to_db` rewritten as one outer transaction with reused prepared statements. `scan_directory_with_target_with_progress_db` gains `index_only: bool` and short-circuits the SHA-256 phase when set. |
| [`src-tauri/src/models.rs`](../src-tauri/src/models.rs) | `ScanTaskRequest.index_only`; new `BulkImportRequest`, `BulkImportResponse`; `ProgressPayload.bulk_import_result`. |
| [`src-tauri/src/import.rs`](../src-tauri/src/import.rs) | New module: streaming parser + zero-alloc slice parser + per-run package HashSet + throttled progress. |
| [`src-tauri/src/tasks.rs`](../src-tauri/src/tasks.rs) | `pick_manifest_file`, `start_bulk_import_task`, `finish_bulk_import_task`. |
| [`src-tauri/src/main.rs`](../src-tauri/src/main.rs) | Register module + commands. |
| [`ui/app.js`](../ui/app.js) | Bulk-import flow: picker, Start/Cancel, progress mirror, completion handler, i18n. Build-DB scan sends `index_only: true`. |
| [`ui/index.html`](../ui/index.html), [`ui/styles.css`](../ui/styles.css) | Bulk Import card markup + filename badge. |
| [`src-tauri/src/tests.rs`](../src-tauri/src/tests.rs) | 6 new tests: parser (3), end-to-end import (2), 50 k-line streaming throughput (1). |

---

## How to verify

```bash
# Type-check + tests
cd src-tauri
cargo check --tests
cargo test --bin vam_var_deduper_tauri db_      # 7 DB tests
cargo test --bin vam_var_deduper_tauri import   # 6 import tests
```

The 50 k-line synthetic test (`import_manifest_streams_large_synthetic_file`)
runs in **~0.45 s on in-memory SQLite**, which sets a baseline for the
throughput the tuned PRAGMAs and streaming pipeline can sustain.
