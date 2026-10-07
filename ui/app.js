const invoke = window.__TAURI__?.core?.invoke;

const KEEP_ALL_VALUE = "__KEEP_ALL__";
const TASK_POLL_MS = 300;
const GROUP_PAGE_SIZE = 20;
const THEMES = [
  "light",
  "dark",
  "nord",
  "solarized",
  "monolith",
  "amber",
  "emerald",
  "midnight",
  "cyber",
  "porcelain",
  "frost",
  "circuit",
  "backstage",
];

const state = {
  language: "en_US",
  theme: "dark",
  scan: null,
  defaultKeepMap: {},
  selectedKey: null,
  selectedKeys: [],
  keepMap: {},
  logs: [],
  // Central, browser-style download manager. One queue for the whole app; any
  // page calls queueDownload(...). Each job:
  //   { id, packageId, label, filename, url, host, destDir, status, percent,
  //     detail, error, taskId, onDone }
  // status ∈ queued | downloading | cancelling | done | exists | failed | cancelled
  downloads: [],
  activeTask: null,
  pollTimer: null,
  outputDirAutoSynced: true,
  processVap: false,
  // Per-section "additional VAR folders" lists, keyed by section id
  // (overview, dbf, internalize, reclaim, unique, varPackages).
  // Each is an array of extra scan roots unioned into that section's scan.
  additionalDirs: {},
  groupPage: 0,
  contextMenuItems: [],
  targetVarPath: "",
  targetPackageId: null,
  pendingDialog: null,
  previewCache: {},
  bulkImportPath: "",
  // Cache of database CRC lookups keyed by crc32 (number).
  // Each entry: { status: "loading"|"ready"|"error", refs?: DbFindRef[], error?: string }
  dbResourceMatches: {},
  // Cache of full DB package resource listings keyed by package_id. Populated
  // when the user picks a DB row as the keep target on Find Duplicates so
  // Apply-to-filtered can relocate via CRC matching.
  // Entry: { status, resourcesByCrc?: Map<u32, ResourceRef>, resources?: [], error? }
  dbPackageResources: {},
  // Tracks the most recently rendered group so renderDetail can drop the
  // DB-cached entries when the user navigates to a different resource.
  lastDetailKey: null,
  // Active sidebar page. "db-find" (Clean VARs) is the sole workspace page and
  // the default landing page; the per-page snapshot/restore machinery is
  // retained (the string is load-bearing for the render dispatchers) even
  // though there is now only one workspace page to restore.
  currentPage: "db-find",
  // Find Duplicates working mode: "db" (default) augments target resources
  // + harvests text refs + shows DB candidates; "local" skips all DB work
  // and renders only the actual cross-package local duplicates the scan
  // produced (Overview-style view scoped to the target VAR).
  dbfMode: "db",
  // True once dbfAugmentScanWithAllTargetResources + dbfLoadTargetTextRefs
  // have run for the current scan. Lets a local→DB mode switch lazy-load
  // those once without re-running them every render.
  dbfAugmentLoaded: false,
  // Snapshot of every page's full state. The currently-active page's state
  // lives on `state` directly; switching pages flushes it here and restores
  // the destination page's snapshot.
  pageStates: {},
  // Resource List page state. DB-only browser of every indexed file across
  // the local database. Mirrors the VAR Packages slot, plus side-panel for
  // duplicates and clickable column sort.
  resourceList: {
    items: [],
    filter: "",
    loading: false,
    page: 0,
    total: 0,
    // True once a COUNT(*) for the current filter+search has returned. False
    // means we've rendered rows but the grand total is still pending — the
    // pagination summary shows "Showing 1-50" instead of "Showing 1-50 of N"
    // and the controls show only Prev/Next (no page-jump buttons).
    totalKnown: false,
    pageSize: 50,
    filters: { category: null, sizeBucket: null },
    filterOptions: { categories: [] },
    categoryMenuQuery: "",
    // Toggle key for the side panel — a `crc:<hex>` string derived from the
    // row's crc32. Always present for indexed rows (every persisted resource
    // has crc32); null only for rows the catalog couldn't fingerprint.
    sidePanelKey: null,
    sidePanelItems: [],
    sidePanelLoading: false,
    sidePanelMeta: null,
    initialized: false,
  },
  // Settings → VaM directory (see applyVamDir) and its last inspect_vam_dir result.
  vamDir: "",
  vamDirInfo: null,
  // VAR Packages (library view). Rows loaded so far — the grid grows in chunks
  // as it scrolls, so this is a prefix of the full matching set.
  varPackagesItems: [],
  varPackagesFilter: "",
  varPackagesLoading: false,
  varPackagesLoadingMore: false,
  // Total matching rows and the facet counts/totals from the last listing.
  varPackagesTotal: 0,
  varPackagesFacets: null,
  // True once a folder listing exists in the backend cache, so re-entering the
  // page re-queries instead of waiting for another Scan.
  vpHasListing: false,
  // Folder-mode scan depth switch. true = walk every subfolder (the
  // long-standing behavior); false = only .var files sitting directly in the
  // chosen folders. This is the switch POSITION — moving it does not rescan.
  varPackagesDeepScan: true,
  // The depth the listing currently on screen was actually built with. Only the
  // Scan button copies varPackagesDeepScan into this. Scrolling, filters and
  // search send this one, so they keep hitting the same backend cache entry
  // instead of silently re-walking the library after the switch is moved.
  varPackagesScannedDeep: true,
  // Filter selections; null means "no filter". `status` is the Status list
  // (favorites | dependency | standalone | broken | missing | outdated |
  // indexed | unindexed), `pkgType` a LIB_TYPES key, `enabled` "enabled" |
  // "disabled", `sizeBucket` "sm" | "md" | "lg", `scene` "with" | "without".
  varPackagesFilters: {
    status: null,
    pkgType: null,
    enabled: null,
    sizeBucket: null,
    creator: null,
    scene: null,
  },
  // Sort is NOT part of varPackagesFilters: Reset clears that object and must
  // not throw away the sort. key: "type" | "name" | "size" | "items" | "deps" |
  // "modified"; dir: "asc" | "desc".
  varPackagesSort: { key: "type", dir: "asc" },
  // Selection, keyed by file_path (NOT package_id — the same id can live in
  // two folders and each copy must be selectable on its own). Values are item
  // snapshots so the selection panel can describe rows that scrolled out.
  // One entry = the package in the details panel; two or more = bulk mode.
  vpSelected: new Map(),
  // file_path of the last toggled item — the shift-click range anchor.
  vpSelAnchor: null,
  // file_path of the keyboard lead (the card arrow keys move from).
  vpLead: null,
  // A package to select once the listing that contains it arrives.
  vpRevealPath: null,
  // One bulk operation at a time; disables the selection-bar action buttons.
  vpBulkRunning: false,
  // View preferences (persisted in localStorage by setupLibraryView).
  vpView: "cards",
  vpCardWidth: 220,
  vpDetailWidth: 340,
  // The Missing status view: { loading, items, error } from list_missing_dependencies.
  vpMissing: null,
  // Author suggestions: the creators of the scanned folders.
  varPackagesFilterOptions: { creators: [] },
  // Roots+depth signature the creator options above were loaded for, so a
  // folder listing only re-fetches them when it actually changed folders.
  varPackagesFilterOptionsKey: null,
  // A filter/search change that arrived while a listing was in flight; it is
  // replayed when that listing settles.
  varPackagesRequery: false,
  // Set when a maintenance apply changed the library while a folder refresh was
  // already in flight — refreshVarPackagesFromFolder re-dispatches once from its
  // `finally`, since its early-return guard would otherwise swallow the refresh
  // and leave recycled files on screen.
  varPackagesDirty: false,
  // True once an apply moved/removed packages that were indexed, so the DB's
  // file_path values are stale. Drives the rebuild banner.
  varPackagesDbStale: false,
  // Build Database — Backfill Resource Sizes task state. Independent of the
  // global activeTask slot so it doesn't clobber Overview/Find Duplicates if
  // the user starts one while the other is running.
  buildDbBackfill: { taskId: null, running: false, cancelRequested: false },
  // Selected package for the VAR Details drill-down. `item` mirrors the
  // VarPackageListItem row that was clicked in the VAR Packages table;
  // `resources` caches the load_db_package_resources response so re-opening
  // the same package is instant.
  varDetails: {
    packageId: null,
    item: null,
    // How the current item was loaded. "local" — picked/dropped/typed by the
    // user, file is on disk. "folder" — VAR Packages folder-mode row, file is
    // on disk. "db" — VAR Packages database-mode row OR a top-package click
    // for an indexed package whose file may not be on disk anymore. The
    // analyzer reads this flag to decide whether to harvest from the .var
    // file (local/folder) or fall back to load_db_package_resources (db).
    itemSource: null,
    // True when the current item is known to live on local disk: either it
    // was loaded from a real file path (loadVarDetailsFromPath, source="local"),
    // came from VAR Packages in folder mode (source="folder"), or a Scan Local
    // has just completed against it. False for DB-only synthetic rows whose
    // file_path is just an index hint and may be stale.
    itemIsLocal: false,
    // VAR folder used by Scan Local. Mirrors #var-details-folder-input and is
    // initialized from the global config's input_dir.
    inputDir: "",
    // Raw DbFindResponse from start_db_find_task. Cleared on package change.
    dbFindResult: null,
    // Derived overlap metrics from computeVarOverlap(). Cleared with dbFindResult.
    overlap: null,
    // Per-resource rows derived from dbFindResult.groups (one row per source_ref).
    resources: [],
    // Resource-table filter: "all" includes both shared and unique rows;
    // "shared" only shows resources with at least one DB match.
    resourceFilter: "all",
    // package_ids of Top Sharing Packages rows currently expanded to show
    // their per-resource breakdown.
    expandedPackages: new Set(),
    // Sort mode for the Top Sharing Packages list — "count" ranks by number
    // of shared resources, "size" by total shared bytes. Toggle only surfaces
    // for local-source scans where users care about reclaim potential.
    topPackagesSortBy: "count",
    // "none" before any scan; "database" or "local" after a successful run.
    assetSource: "none",
    assetPage: 0,
    resourcesLoading: false,
    resourcesError: null,
    search: "",
    scanning: false,
    scanKind: null,
    scanTaskId: null,
    progress: 0,
    progressMessage: "",
    // Internal path of the row currently selected in the Resources table. Drives
    // the inline preview panel — null hides it. Resets whenever the active
    // package changes so previews from one VAR don't bleed into another.
    selectedResourceKey: null,
    // "Download this VAR" banner state, shown only when the opened VAR isn't
    // present locally. state: "idle"|"resolving"|"available"|"unavailable"
    // |"downloading"|"cancelling"|"done"|"failed". resolvedFor guards one resolve
    // per package; presenceChecked gates the banner until path_exists confirms
    // the file really is absent (a stale-but-present DB path = still in library).
    availability: {
      state: "idle",
      resolvedFor: null,
      presenceChecked: false,
      // Whether the VAR's file is actually present on disk. Drives the banner —
      // kept separate from itemIsLocal (which Scan Local flips for its own UI).
      filePresent: false,
      url: null,
      filename: null,
      size: null,
      host: null,
      percent: 0,
      detail: "",
      error: null,
      taskId: null,
    },
  },
  // Reclaim Space page — scans a folder of local VARs, aggregates their unique
  // CRC set, and ranks DB packages by total bytes covered. Independent of any
  // VAR Details state so the two pages can coexist with separate scan tasks.
  reclaimSpace: {
    folder: "",
    scanning: false,
    scanTaskId: null,
    progress: 0,
    progressMessage: "",
    error: null,
    summary: null, // { localVarsScanned, localUniqueResources, localUniqueBytes }
    candidates: [],
    sortBy: "bytes", // "bytes" | "count" | "coverage" | "package" | "creator"
    sortDir: "desc",
    page: 0,
  },
  // Unique Resources page — walks a folder locally and lists every unique
  // resource (by crc32+size) with all the VARs that contain it. No DB use.
  // Independent state slot so it survives switchPage like Reclaim Space does.
  uniqueResources: {
    folder: "",
    scanning: false,
    scanTaskId: null,
    progress: 0,
    progressMessage: "",
    error: null,
    summary: null, // { varsScanned, uniqueResources, totalUniqueBytes, totalCombinedBytes, reclaimableBytes }
    resources: [],
    filter: "",
    sortBy: "combined", // "combined" | "unique" | "sources" | "resource" | "reclaimable"
    sortDir: "desc",
    page: 0,
    selectedKey: null, // crc32_hex of the row whose side panel is open
    topCollapsed: false, // collapses Source Folder + Local Summary after first scan
  },
  // Missing Resources page — scans a target VAR for broken Pkg:/path refs
  // and lets the user batch-pick replacement packages. Independent state slot
  // so it can coexist with Overview / Find Duplicates scans.
  missingResources: {
    inputDir: "",
    outputDir: "",
    targetVar: "",
    brokenRefs: [],         // BrokenRef[]
    // Set to true after a scan finishes successfully so the UI can tell
    // apart "never scanned" from "scanned and clean".
    lastScanCompleted: false,
    filter: "",
    listPage: 0,
    selectedKey: null,      // `${ref_pkg}|${ref_path ?? ""}`
    // selectedKey → { broken_pkg, broken_path?, replacement_pkg, replacement_license_type?, source }
    replacementMap: {},
    // selectedKey → { status: "idle"|"loading"|"ok"|"err", refs?, error? }
    dbCandidatesByKey: {},
    // Cache for the nested-ref recovery flow — when ref.ref_path embeds a
    // second `Pkg:/<inner>` (a malformed scene-text-ref like
    // `Pkg1:/Pkg2:/Custom/.../X.png`), we run a parallel DB lookup on just
    // `<inner>` and surface those candidates alongside the regular DB list.
    // Shape mirrors dbCandidatesByKey: selectedKey → { status, items?, error? }.
    nestedDbCandidatesByKey: {},
    status: "",             // free-form status message for the toolbar
    scanning: false,
    applying: false,
    backup: true,
    replaceInPlace: false,
    // Candidate-source mode for the detail panel. "local" shows only local
    // candidates; "db" additionally shows database candidates. Display-only —
    // detection (which refs are broken) is independent of mode, so a missing
    // resource with zero candidates still appears in either mode.
    mode: "local",
  },
  // Internalize Resources page — inverse of Missing Resources. Scans a target
  // VAR for *valid* external `Pkg:/path` refs and lets the user pick which
  // ones to inline by copying the source bundle into the target as SELF.
  internalize: {
    inputDir: "",
    outputDir: "",
    targetVar: "",
    groups: [],             // ExternalRefGroup[]
    lastScanCompleted: false,
    filter: "",
    listPage: 0,
    selectedPkg: null,      // selected source_pkg_id (the left-panel row)
    // Map of `${source_pkg_id}|${ref_path}` → true for refs the user has
    // picked to internalize. Set semantics — flip on/off via checkbox.
    selectedRefs: {},
    status: "",
    scanning: false,
    applying: false,
    backup: true,
    replaceInPlace: false,
  },
  // Settings → Advanced → Clear database. Separate task slot.
  settingsClearDb: {
    taskId: null,
    taskProgress: 0,
    taskMessage: "",
    pollTimer: null,
  },
};

// Memoization for getFilteredGroups(). The function is called 3x per render
// (renderGroups -> normalizeSelections -> getSelectedGroup) and rebuilds a
// sorted+filtered array each time. The cache key embeds every input the
// function reads — including a `_keepMapVersion` counter bumped by
// `bumpKeepMutation()` at every state.keepMap mutation site, so the sort
// (which depends on getGroupScopedMaxReclaimableBytes -> keepMap) stays
// correct after keep changes. Within a synchronous render call nothing
// mutates, so the cache hits cleanly on calls 2 and 3.
let _filteredGroupsCache = null;
let _filteredGroupsCacheKey = null;
let _keepMapVersion = 0;
function bumpKeepMutation() {
  _keepMapVersion++;
}
function invalidateFilteredGroups() {
  _filteredGroupsCache = null;
  _filteredGroupsCacheKey = null;
}

// Cheap debounce helper for input handlers. Trailing-edge only; cancels the
// pending call on every re-invocation.
function debounce(fn, ms) {
  let timer = null;
  return function (...args) {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      fn.apply(this, args);
    }, ms);
  };
}

// Precomputed lowercased search haystack for a group. Built once on scan
// ingestion so per-keystroke filtering in `getFilteredGroups` skips the
// per-group string concat + lowercase. Stored as a non-enumerable string so
// it survives `JSON.stringify` / state snapshots untouched.
function buildGroupHaystack(group) {
  if (!group) return;
  const parts = [String(group.key ?? "")];
  if (Array.isArray(group.package_ids)) parts.push(group.package_ids.join(" "));
  if (Array.isArray(group.refs)) {
    for (const ref of group.refs) {
      if (ref && ref.internal_path) parts.push(ref.internal_path);
    }
  }
  Object.defineProperty(group, "__haystack", {
    value: parts.join(" ").toLowerCase(),
    enumerable: false,
    writable: true,
    configurable: true,
  });
}
function buildGroupHaystacks(groups) {
  if (!Array.isArray(groups)) return;
  for (const g of groups) buildGroupHaystack(g);
}

const VAR_DETAILS_PAGE_SIZE = 20;
const VAR_DETAILS_TOP_PACKAGES = 10;

const I18N = {
  en_US: {
    appTitle: "VAM VAR Deduper",
    eyebrow: "VAM VAR Desktop Tool",
    heroCopy: "Scan `.var` packages in one folder, detect cross-package duplicates, choose the source to keep, and write a deduped output set.",
    themeButtonLight: "Light",
    themeButtonDark: "Dark",
    themeButtonNord: "Nord",
    themeButtonSolarized: "Solarized",
    themeButtonMonolith: "Monolith",
    themeButtonBackstage: "Backstage",
    themeButtonAmber: "Amber Noir",
    themeButtonEmerald: "Emerald Dark",
    themeButtonMidnight: "Midnight Blue",
    themeButtonCyber: "Cyber Violet",
    themeButtonPorcelain: "Porcelain",
    themeButtonFrost: "Nordic Frost",
    themeButtonCircuit: "Amber Circuit",
    inputLabel: "VAR Folder",
    scanAdditionalDirsLabel: "Additional VAR Folders",
    addFolder: "Add Folder",
    removeFolder: "Remove folder",
    outputLabel: "Output Folder",
    processVap: "Process VAP",
    vapLabel: "VAP Folder (Optional)",
    browse: "Browse",
    replaceInPlace: "Replace original files in place",
    backupChanged: "Back up changed originals to sibling backup folder",
    replaceWarning: "Risk: this will overwrite original .var files in the input folder. Enable backup if you want a safety copy.",
    scan: "Scan",
    run: "Run Dedup",
    progressScan: "Scan Progress",
    progressRun: "Dedup Progress",
    statPackages: "Packages",
    statGroups: "Duplicate Groups",
    statSpace: "Reclaimable",
    statFiltered: "Filtered",
    statModified: "Modified",
    statReclaimedSize: "Modified Size",
    statCurrentSize: "Current Size",
    statEstimatedSize: "Estimated Remaining",
    statMode: "State",
    statIdle: "Idle",
    statReady: "Scanned",
    statBusy: "Running",
    groupsTitle: "Duplicate Group Overview",
    groupsSubtitle: "Groups are sorted by reclaimable space so the highest-impact items are handled first.",
    groupsEmpty: "Scanned duplicate groups will appear here.",
    selectionHint: "Left click to select, Ctrl/Shift for multi-select, right click for bulk keep actions.",
    detailTitle: "Sources and Keep Strategy",
    detailSubtitle: "Choose the source to keep for each duplicate group. Removed items will be rewritten in the output package.",
    detailSubtitleSingle: "Keep the target VAR itself, or relocate its conflicting resources to another VAR.",
    detailEmpty: "Select a duplicate group on the left first.",
    detailHash: "Group Key (CRC32 : size)",
    keepSourceTitle: "Keep Source",
    keepSourceTitleSingle: "Relocate Target To",
    sourceFromLocalLabel: "Source (Local target — being relocated)",
    keepAll: "Do not dedupe this group",
    keepSelf: "Keep",
    relocateTo: (pkg) => `Relocate to ${pkg}`,
    relocateToPath: (pkg, path) => `Relocate to ${pkg} / ${path}`,
    quickActionsTitle: "Quick Apply",
    quickActionsHint: "Quick apply targets the same package set, the current filtered results, or all groups.",
    applyScope: "Apply to same package set",
    applyFiltered: "Apply to filtered groups",
    applyGlobal: "Apply to all groups",
    autoTarget: "Auto Target",
    autoTargetHint: "Auto-pick a relocation target for every group that doesn't have one yet. Manual selections are preserved.",
    autoTargetDone: (count) => `Auto Target: assigned ${count} groups`,
    autoTargetNone: "Auto Target: nothing to assign",
    favSource: "Favorite Source",
    favSourceHint:
      "Auto-pick a favorited VAR (or a favorite creator's VAR) as the relocation target for every group that doesn't have one yet. Groups without a favorite candidate are left unchanged — run Auto Target afterwards for the rest.",
    favSourceDone: (count) => `Favorite Source: assigned ${count} groups`,
    favSourceNone: "Favorite Source: no favorite candidates to assign",
    favSourceNoFavorites: "Favorite Source: no favorite VARs or creators yet — mark some first.",
    resetKeep: "Reset All Choices",
    exportResource: "Export Current Resource",
    logTitle: "Activity Log",
    logSubtitle: "Tracks selection, progress, scans, and execution.",
    logEmpty: "No log entries yet.",
    filterPlaceholder: "Filter by path / package / hash",
    selectedFiles: (count) => `${count} sources`,
    reclaimable: (text) => `Reclaimable ${text}`,
    keepFrom: (pkg) => `Keep ${pkg}`,
    keepFromPath: (pkg, path) => `Keep ${pkg} / ${path}`,
    keepAllShort: "Keep all",
    scanSuccess: (packages, groups) => `Scan complete: ${packages} packages, ${groups} duplicate groups.`,
    dbSaved: (packages, resources) =>
      `Saved ${packages} packages and ${resources} resources to database.`,
    dbSavedWithPrune: (packages, resources, pruned) =>
      `Saved ${packages} packages and ${resources} resources to database (pruned ${pruned} stale entries).`,
    buildDbScanSummary: (packages, resources, dbSize) =>
      `Indexed ${packages} packages, ${resources} resources · DB size: ${dbSize}`,
    buildDbStatusTitle: "Database Status",
    buildDbStatPackages: "Packages",
    buildDbStatResources: "Resources",
    buildDbStatSize: "DB Size",
    buildDbStatLast: "Last Indexed",
    buildDbStatusEmpty: "Empty",
    buildDbStatusReady: "Ready",
    buildDbLastNever: "Never",
    buildDbLastJustNow: "Just now",
    buildDbLastSecondsAgo: (n) => `${n}s ago`,
    buildDbLastMinutesAgo: (n) => `${n}m ago`,
    buildDbLastHoursAgo: (n) => `${n}h ago`,
    buildDbLastDaysAgo: (n) => `${n}d ago`,
    bulkImportPickPrompt: "Click to browse for a manifest .txt file",
    bulkImportPickHint: "Supports CRC32 registry via plain text manifest",
    bulkImportCancel: "Cancel",
    bulkImportStart: "Start Bulk Import",
    bulkImportLabel: "Manifest Import",
    bulkImportStarted: "Started parsing manifest file.",
    bulkImportNoFile: "Pick a manifest file first.",
    bulkImportSuccess: (parsed, packagesNew, resourcesInserted, resourcesSkipped) =>
      `Manifest import complete: parsed ${parsed} lines, ${packagesNew} new packages, ${resourcesInserted} resources inserted (${resourcesSkipped} skipped — already in DB).`,
    bulkImportLinesSkipped: (n) => `Skipped ${n} lines (blank or malformed).`,
    bulkImportProgressLines: (parsed, total) => `${parsed} / ${total} lines`,
    bulkImportStatusActive: "Active",
    bulkImportStatusIdle: "Idle",
    bulkImportIdleNote:
      "Pick a manifest file and click Start to import. Existing rows are preserved; only new rows are written.",
    settingsTitle: "Settings",
    settingsSubtitle:
      "Manage your data environment, application appearance, and processing logic.",
    settingsPathsTitle: "Environment Paths",
    settingsPathsDesc:
      "Configure where the engine looks for source files and where results are stored.",
    settingsPathInputDesc:
      "Root directory containing your Virt-A-Mate .var archives.",
    settingsPathOutputDesc:
      "Processed deduplicated files will be moved here.",
    settingsPathVapDesc:
      "Optional preset folder used when 'Process VAP' is enabled.",
    settingsAppearanceEyebrow: "Appearance",
    settingsThemeLabel: "Theme",
    settingsThemeDesc: "Switch between light and dark interface.",
    settingsScanEyebrow: "Scan Options",
    settingsScanTitle: "Scan Defaults",
    settingsScanDesc:
      "Same toggles as the dashboard — change them here for convenience.",
    settingsDbEyebrow: "Database",
    settingsDbTitle: "Local Cache Database",
    settingsDbDesc:
      "Stores scanned packages and resources for fast rescans and persistent CRC history.",
    settingsDbPackages: "Packages",
    settingsDbResources: "Resources",
    settingsDbSize: "Database Size",
    settingsRefresh: "Refresh",
    settingsClearCache: "Clear Cache",
    settingsAboutEyebrow: "About",
    settingsAboutDesc:
      "Cross-package duplicate resource detection and removal for VAR packages.",
    settingsConfirmClear:
      "Clear all package and resource records from the local database? This cannot be undone.",
    settingsCleared: "Database cleared.",
    settingsClearing: "Clearing database…",
    settingsLoadFailed: "Failed to load database stats: ",
    settingsClearFailed: "Failed to clear database: ",
    settingsSave: "Save Preferences",
    settingsReset: "Discard Changes",
    settingsConfirmReset:
      "Discard the unsaved changes and revert to the last saved settings?",
    settingsSaved: "Settings saved.",
    settingsResetDone: "Reverted to last saved settings.",
    settingsSaveFailed: "Failed to save settings: ",
    settingsResetFailed: "Failed to load saved settings: ",
    scanNone: "No cross-package duplicates were found.",
    scanStarted: "Started scanning the folder.",
    scanWarning: (message) => `Skipped: ${message}`,
    quickActionsIntraHint: "When duplicates are inside the same package, keep choices must be made by exact path, so package-level bulk apply is disabled.",
    runSuccess: (changed, removed, vapChanged, report) => `Dedup completed: changed ${changed} packages, removed ${removed} files, rewritten ${vapChanged} VAP files, report ${report}`,
    runStarted: "Started dedup.",
    missingPath: "Input and output folders are required.",
    scanFirst: "Scan the folder first.",
    sourceBadgeSize: (text) => `Size ${text}`,
    sourceBadgePkg: (pkg) => `Pkg ${pkg}`,
    menuCurrentKeepFrom: (pkg) => `Current group keep ${pkg}`,
    menuCurrentKeepAll: "Current group keep all",
    menuSelectedKeepFrom: (pkg) => `Selected groups keep ${pkg}`,
    menuSelectedKeepAll: (count) => `Selected ${count} groups keep all`,
    menuSingleVarApply: (label) => `Apply all from ${label}`,
    menuScope: (label) => `Apply ${label} to same package set`,
    menuGlobal: (label) => `Apply ${label} to all groups`,
    menuShowInExplorer: "Show in Explorer",
    menuSearchByCrc: (hex) => `Search Resource List by CRC (${hex})`,
    menuCopyCrc: (hex) => `Copy CRC (${hex})`,
    menuCopyFilePath: "Copy .var file path",
    menuCopyInternalPath: "Copy internal path",
    detailHashCopyHint: "Right-click to copy CRC or path",
    crcCopied: (hex) => `Copied CRC ${hex}`,
    crcCopyFailed: (error) => `Copy failed: ${error}`,
  },
};

Object.assign(I18N.en_US, {
  scopeModeRoot: "All Conflicts",
  scopeModeClear: "Exit Scoped Mode",
  scopeModeBanner: (pkg) => `Showing and executing only conflicts related to ${pkg}`,
  scopeModeSet: (pkg) => `Only modify ${pkg}`,
  scopeModeEnabledLog: (pkg) => `Scoped mode enabled for ${pkg}.`,
  scopeModeDisabledLog: "Scoped mode disabled.",
  scopeEmpty: (pkg) => `No conflicts related to ${pkg} are visible in the current mode.`,
});

Object.assign(I18N.en_US, {
  openOutput: "Open Output",
  quickActionsHintSingle:
    "Apply your keep choices to the conflicts currently visible for the target VAR.",
  dialogConfirmTitle: "Confirm",
  dialogConfirm: "Continue",
  dialogCancel: "Cancel",
  dedupCompleteTitle: "Dedup Complete",
  dedupCompleteOk: "OK",
  dedupCompleteOpen: "Open Output Folder",
  dedupCompleteOpenBackup: "Open Backup Folder",
  dedupCompleteReport: "Open Report JSON",
  dedupBefore: "Before",
  dedupAfter: "After",
  dedupStripped: "Stripped",
  targetVarChangedReset:
    "Target VAR changed. Cleared the previous scan result. Please scan again.",
  targetVarLabel: "Target VAR",
  targetVarRequired: "Select a .var file before scanning.",
  openVarDetails: "VAR Details",
  varInfoPanel: "VAR Info",
  varInfoSize: "Size",
  varInfoTime: "Modified",
  varInfoScene: "Scene Image",
    varInfoEmpty: "Select a .var file to begin scanning.",
  scopeModeRoot: "Batch Mode",
  scopeModeBanner: (pkg) =>
    `Showing only conflicts inside ${pkg}; dedupe will modify only that VAR package`,
  scopeEmpty: (pkg) => `No conflicts related to resources inside ${pkg} were found.`,
  backupChanged: "Back up changed VAR/VAP originals to the sibling backup folder",
  replaceWarning:
    "Risk: this will overwrite changed VAR/VAP originals in place. Enable backup if you want a safety copy.",
  previewTitle: "Related Image Preview",
  previewLoading: "Loading preview...",
  previewEmpty: "No same-stem jpg/png image was found for this .vam entry.",
  previewError: "Preview failed to load",
  previewResolution: (width, height) => `${width} x ${height}`,
  previewCount: (count) => `${count} image${count === 1 ? "" : "s"}`,
  previewLoadLarge: "Image too large, click to load",
  varPackagesNav: "VAR Packages",
  varPackagesTitle: "VAR Packages",
  varPackagesSubtitle:
    "Browse every .var package in a folder. Status reflects whether the package is currently indexed in your local database.",
  varPackagesScan: "Rescan",
  varPackagesSearchPlaceholder: "Search…",
  varPackagesThStatus: "Status",
  varPackagesThName: "Package Name",
  varPackagesThCreator: "Creator",
  varPackagesThSize: "Size",
  varPackagesThModified: "Last Modified",
  varPackagesEmpty: "Click Rescan to list the packages in AddonPackages.",
  varPackagesNoResults: "No packages match the current filter.",
  varPackagesPickFirst: "Please pick a VAR folder first.",
  varPackagesScanFailed: (error) => `Failed to list VAR packages: ${error}`,
  varPackagesCount: (n) => `${n.toLocaleString()} package${n === 1 ? "" : "s"}`,
  varPackagesPageSummary: (start, end, total) =>
    `Showing ${start.toLocaleString()}-${end.toLocaleString()} of ${total.toLocaleString()}`,
  varPackagesPagePrev: "Previous",
  varPackagesPageNext: "Next",
  varPackagesDbHint: "Listing every package recorded in the local database.",
  varPackagesRefresh: "Refresh",
  varPackagesStatusMissing: "Missing",
  varPackagesStatusOnDisk: "On Disk",
  varPackagesDbFailed: (error) => `Failed to load packages from the database: ${error}`,
  varPackagesLoadingDb: "Loading from database…",
  varPackagesScanning: "Scanning directory…",
  varPackagesFiltersLabel: "Filters:",
  varPackagesFilterClearAll: "Clear All",
  varPackagesFilterAll: "All",
  varPackagesFilterSize: "Size",
  varPackagesFilterCreator: "Creator",
  varPackagesFilterCategory: "Category",
  varPackagesFilterChip: (name, value) => `${name}: ${value}`,
  varPackagesFilterEmpty: "None",
  varPackagesFilterMenuSearchPlaceholder: "Search…",
  varPackagesSizeSmall: "< 100 MB",
  varPackagesSizeMedium: "100 MB – 1 GB",
  varPackagesSizeLarge: "> 1 GB",
  varPackagesFilterOptionsFailed: (error) =>
    `Failed to load filter options: ${error}`,
  varPackagesFilterFavorites: "Favorites",
  varPackagesFavoritesOnly: "Only favorites",
  varPackagesFilterScene: "Scene image",
  // Row right-click actions in the delete modal's result lists. The two
  // navigating actions are listed first and separated from the two that leave
  // the modal (and the pending delete) intact.
  varPackagesRowOpenDetails: "Open Details",
  varPackagesRowScanDeps: "Download Dependencies",
  varPackagesRowShowInExplorer: "Show in Explorer",
  varPackagesRowCopyPath: "Copy file path",
  // Hover info card on a VAR Packages card/row. The two extra status labels
  // exist because `indexed` means the opposite thing per mode: folder-mode
  // rows are read off disk and the flag says "also in the database", while
  // database-mode rows are read from the database and the flag says "still on
  // disk" (the On Disk / Missing pair the table column already uses).
  varPackagesHoverVersion: "Version",
  varPackagesHoverLocation: "Location",
  varPackagesHoverLocationStale:
    "Recorded in the database. The file is no longer at this path.",
  varPackagesStatusInDb: "Indexed",
  varPackagesStatusNotInDb: "Not indexed",
  varPackagesFilterSort: "Sort",
  varPackagesSortName: "Name",
  varPackagesSortSize: "Size",
  // Descending on this key is the "newest first" the sort menu is usually
  // reached for; there is no separate "Newest" key.
  varPackagesSortModified: "Date modified",
  varFavAdd: "Add to favorites",
  varFavRemove: "Remove from favorites",
  varFavLabelOff: "Favorite",
  varFavLabelOn: "Favorited",
  varPackagesSelCount: (n, size) => `${n} selected · ${size}`,
  varPackagesSelNone: "None selected",
  varPackagesDeleteTitle: "Delete package?",
  varPackagesDeleteMessage: (name) =>
    `Send ${name} to the Recycle Bin? You can restore it from the Recycle Bin if you need it back.`,
  varPackagesDeleteScan: "Scan for usage",
  varPackagesDeleteConfirm: "Delete",
  varPackagesDeleteCancel: "Cancel",
  varPackagesDeleteCancelScan: "Cancel scan",
  varPackagesDeleteNoUsage: "No other packages reference this — safe to delete.",
  varPackagesDeleteUsedBy: (n) =>
    `${n} package${n === 1 ? "" : "s"} reference this (any version) — deleting may break them:`,
  varPackagesDeleteRootLabel: "Scan folders",
  varPackagesDeleteRootUnset: "Not set — configure your VAR library folder in Settings.",
  varPackagesDeleteChangeInSettings: "Change in Settings",
  varPackagesDeleteNoRoots: "Set your VAR library (AddonPackages) folder in Settings to scan usage.",
  varPackagesDeleteDepScan: "Scan dependencies",
  varPackagesDeleteUsageHeader: "Used by",
  varPackagesDeleteDepsHeader: "Dependencies",
  varPackagesDeleteDepsHint:
    "Tick dependencies to send them to the Recycle Bin together with this package.",
  varPackagesDeleteDepsNone: "This package declares no dependencies.",
  varPackagesDeleteDepsSummary: (ex, sh, miss) =>
    `${ex} exclusive · ${sh} shared with other packages · ${miss} not installed`,
  varPackagesDeleteDepsScanErrors: (n) =>
    ` ${n} package${n === 1 ? "" : "s"} could not be read — shared counts may be incomplete.`,
  varPackagesDeleteDepsExclusive: "Not used by others",
  varPackagesDeleteDepsShared: (n) => `Used by ${n} other${n === 1 ? "" : "s"}`,
  varPackagesDeleteDepsEffExclusive: "Only used by packages being deleted",
  varPackagesDeleteDepsMissing: "Not installed",
  varPackagesDeleteDepsTransitive: "transitive",
  varPackagesDeleteDepsDisabled: "disabled",
  varPackagesDeleteDepsMoreFiles: (n) => `+${n} more file${n === 1 ? "" : "s"}`,
  varPackagesDeleteDepsMoreUsers: (n) => `and ${n} more`,
  varPackagesDeleteConfirmWithDeps: (n) => `Delete + ${n} dep${n === 1 ? "" : "s"}`,
  varPackagesImagesTitle: "Images in package",
  varPackagesImagesOpen: "View images",
  varPackagesImagesMaxLabel: "Max size",
  varPackagesImagesLoading: "Reading package…",
  varPackagesImagesError: (msg) => `Could not read package: ${msg}`,
  // "no images at all" and "images exist but all are over the limit" are
  // different situations — the second must never look like the first.
  varPackagesImagesNone: "This package contains no JPG or PNG images.",
  varPackagesImagesAllSkipped: (n, mb) =>
    `All ${n} image${n === 1 ? " is" : "s are"} over ${mb} MB — raise the limit to see ${n === 1 ? "it" : "them"}.`,
  varPackagesImagesSummary: (shown, total) =>
    `${shown} image${shown === 1 ? "" : "s"} of ${total}`,
  varPackagesImagesSkipped: (n, mb) => ` · ${n} skipped (over ${mb} MB)`,
  varPackagesImagesCapped: (cap) => ` · showing first ${cap}`,
  varPackagesMoveNoRoot: "Set your VAR library (AddonPackages root) folder in Settings first.",
  varPackagesMoveDone: (dest) => `Moved to ${dest}`,
  varPackagesBulkDeleteConfirm: (n, size) =>
    `Send ${n} package${n === 1 ? "" : "s"} (${size}) to the Recycle Bin?\n\n` +
    `Other packages or scenes may still reference them — this is not checked. ` +
    `You can restore them from the Recycle Bin.`,
  varPackagesBulkDeleteProgress: (i, n, freed) => `Deleting ${i}/${n} · ${freed} freed`,
  varPackagesBulkDeleteDone: (ok, freed) =>
    `Deleted ${ok} package${ok === 1 ? "" : "s"} · ${freed} freed`,
  varPackagesBulkDeleteSomeFailed: (ok, failed) =>
    `Deleted ${ok}, ${failed} failed — see Activity Log for details`,
  varPackagesBulkExportProgress: (i, n) => `Exporting images ${i}/${n}`,
  varPackagesBulkExportDone: (parts, n) => `Scene images done — ${parts} (of ${n}).`,
  varPackagesBulkOrganizeHint: (matched, selected) =>
    `${matched} of your ${selected} selected package${selected === 1 ? "" : "s"} ` +
    `have a planned change and are pre-ticked. The rest need nothing ` +
    `or were excluded (see notes).`,
  varPackagesBulkOrganizeScopeNote: (n) =>
    `The plan still scans every configured folder — only the rows matching your ` +
    `${n} selected package${n === 1 ? "" : "s"} are pre-ticked below.`,
});

Object.assign(I18N.en_US, {
  resourceListNav: "Resource List",
  resourceListTitle: "Resource List",
  resourceListSubtitle:
    "Browse every indexed file across your local database. Sort, filter, and inspect duplicates without picking a single package first.",
  resourceListCount: (n) => `${n.toLocaleString()} resource${n === 1 ? "" : "s"}`,
  resourceListEmpty: "No resources indexed. Build the database first.",
  resourceListNoResults: "No resources match the current filter.",
  resourceListLoadingDb: "Loading from database…",
  resourceListLoadFailed: (error) => `Failed to load resources: ${error}`,
  resourceListSearchPlaceholder: "Search by package prefix or CRC32 (8 hex)…",
  resourceListSearchButton: "Search",
  resourceListSearchClear: "Clear",
  settingsAdvancedTitle: "Advanced",
  settingsAdvancedDesc:
    "Optional power-user controls for the local database. Each item below has its own warning — read it before clicking.",
  settingsAdvancedTag: "Optional",
  // Settings → Advanced → Clear local database
  settingsClearDbTitle: "Clear local database",
  settingsClearDbDesc:
    "Permanently deletes every indexed package, resource, and creator from the local SQLite file. Your .var files on disk are not touched.",
  settingsClearDbWarningTitle: "This cannot be undone:",
  settingsClearDbWarningBody:
    "You'll need to re-scan or re-import to rebuild the database. On multi-million-row libraries this takes several minutes. Do not touch if you don't know what you are doing.",
  settingsClearDbButton: "Clear database",
  resourceListRefresh: "Refresh",
  resourceListThIcon: "Type",
  resourceListThName: "Resource",
  resourceListThCategory: "Category",
  resourceListThCrc: "CRC32",
  resourceListThPackage: "Package",
  resourceListThSize: "Size",
  resourceListFilterCategory: "Category",
  resourceListFilterSize: "Size",
  resourceListFiltersLabel: "Filters:",
  resourceListFilterAll: "All",
  resourceListFilterClearAll: "Clear All",
  resourceListFilterChip: (name, value) => `${name}: ${value}`,
  resourceListSizeSmall: "< 1 MB",
  resourceListSizeMedium: "1 MB – 100 MB",
  resourceListSizeLarge: "> 100 MB",
  resourceListSidePanelTitle: "Appears In",
  resourceListSidePanelSubtitle: "Every package containing this hash.",
  resourceListSidePanelEmpty: "Select a row to inspect duplicates.",
  resourceListSidePanelOnlyOne: "This resource appears in only one package.",
  resourceListSidePanelLoadFailed: (error) => `Failed to load duplicates: ${error}`,
  resourceListSideJump: "Open in VAR Details",
  resourceListPagePrev: "Previous",
  resourceListPageNext: "Next",
  // `total` can be a number (count known) or a string like "…" while the
  // count is still computing.
  resourceListPageSummary: (start, end, total) =>
    `Showing ${start.toLocaleString()}-${end.toLocaleString()} of ${
      typeof total === "number" ? total.toLocaleString() : total
    }`,
  resourceListContextCopyCrc: "Copy CRC32",
  resourceListContextOpenPackage: "Open package in VAR Details",
  resourceListContextFindDupes: "Find duplicates",
  resourceListContextShowExplorer: "Show in Explorer",
  resourceListCopyOk: "Copied to clipboard.",
  resourceListCopyFailed: "Copy failed.",
  resourceListFilterOptionsFailed: (error) =>
    `Failed to load filter options: ${error}`,
});

function t(key, ...args) {
  const table = I18N.en_US;
  const value = table[key];
  return typeof value === "function" ? value(...args) : value;
}

function $(id) {
  return document.getElementById(id);
}

function iconSvg(name) {
  const icons = {
    sun: `
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M12 4V2m0 20v-2m8-8h2M2 12h2m13.66 5.66 1.41 1.41M4.93 4.93l1.41 1.41m11.32-1.41-1.41 1.41M6.34 17.66l-1.41 1.41M12 7a5 5 0 1 0 0 10 5 5 0 0 0 0-10Z"/>
      </svg>`,
    moon: `
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M20 15.5A8.5 8.5 0 0 1 8.5 4 8.5 8.5 0 1 0 20 15.5Z"/>
      </svg>`,
    globe: `
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20Zm-7.5 10h15M12 2c2.5 2.7 3.8 6.1 3.8 10S14.5 19.3 12 22M12 2C9.5 4.7 8.2 8.1 8.2 12S9.5 19.3 12 22"/>
      </svg>`,
    folderOpen: `
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M3 7.5A2.5 2.5 0 0 1 5.5 5H10l2 2h6.5A2.5 2.5 0 0 1 21 9.5v.5H8.5A2.5 2.5 0 0 0 6.1 11.8L4.6 18.5A2 2 0 0 1 3 16.6V7.5Z"/>
        <path d="M8.5 10.5H21l-2 8.2A2.5 2.5 0 0 1 16.6 20H5.8a1.8 1.8 0 0 1-1.8-2.2l1.6-5.9a1.8 1.8 0 0 1 1.9-1.4Z"/>
      </svg>`,
    folder: `
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M3 7.5A2.5 2.5 0 0 1 5.5 5H10l2 2h6.5A2.5 2.5 0 0 1 21 9.5v7A2.5 2.5 0 0 1 18.5 19h-13A2.5 2.5 0 0 1 3 16.5v-9Z"/>
      </svg>`,
    search: `
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14Z"/>
        <path d="m20 20-3.5-3.5"/>
      </svg>`,
    wand: `
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="m14 5 1-2 1 2 2 1-2 1-1 2-1-2-2-1 2-1Zm4 7 1.5-.8L20.3 9l.8 1.5L23 11.3l-1.9.8-.8 1.9-.8-1.9L18 12Zm-8.2-1.8 4 4L7 21l-4-4 6.8-6.8Z"/>
      </svg>`,
  };
  return icons[name] ?? "";
}

function setButtonIcon(id, icon, label, { showLabel = false } = {}) {
  const button = $(id);
  if (!button) {
    return;
  }
  button.classList.toggle("icon-only-button", !showLabel);
  button.classList.toggle("icon-label-button", showLabel);
  button.setAttribute("title", label);
  button.setAttribute("aria-label", label);
  button.innerHTML = showLabel
    ? `<span class="button-icon">${iconSvg(icon)}</span><span>${escapeHtml(label)}</span>`
    : `<span class="button-icon">${iconSvg(icon)}</span>`;
}

function addLog(message) {
  state.logs.unshift({
    message,
    time: new Date().toLocaleTimeString(),
  });
  renderLogs();
}

function formatBytesLocal(size) {
  const units = ["B", "KB", "MB", "GB"];
  let value = Number(size ?? 0);
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  return `${value.toFixed(1)} ${units[unitIndex]}`;
}

// ============================================================
// Central downloads manager (browser-style)
//
// A single app-wide queue + top-right popover. Any page calls queueDownload(...)
// instead of running its own download UI. Backs onto the existing per-task
// backend (start_download_one_task / get_task_progress / cancel_task / clear_task),
// which is already concurrent, and caps how many run at once.
// ============================================================
let downloadJobSeq = 0;
const DOWNLOAD_CONCURRENCY = 3;

function findDownloadJob(packageId) {
  return state.downloads.find(
    (j) =>
      j.packageId === packageId &&
      (j.status === "queued" || j.status === "downloading" || j.status === "cancelling")
  );
}

function downloadJobActive(job) {
  return job.status === "queued" || job.status === "downloading" || job.status === "cancelling";
}

// Resolves the downloads destination synchronously: the folder set in Settings,
// else the VAR library folder. "" when neither is configured.
function configuredDownloadsDir() {
  const settingsDir = ($("settings-downloads-folder")?.value || "").trim();
  if (settingsDir) return settingsDir;
  return ($("settings-library-folder")?.value || "").trim();
}

// Like configuredDownloadsDir, but if nothing is configured it prompts the user
// to pick a folder once and saves it to Settings for future downloads.
async function ensureDownloadsDir() {
  const dir = configuredDownloadsDir();
  if (dir) return dir;
  if (!invoke) return "";
  try {
    const picked = await invoke("pick_folder");
    if (picked) {
      const trimmed = String(picked).trim();
      const field = $("settings-downloads-folder");
      if (field) field.value = trimmed;
      try { await persistAllConfig(); } catch (_e) {}
      return trimmed;
    }
  } catch (_e) {}
  return "";
}

// When enabled in Settings, nest a download under a per-creator subfolder of the
// destination (e.g. <downloads>/qing for "qing.hgf1.1"). The backend creates the
// folder on demand.
function nestDestByCreator(destDir, packageId) {
  if (!$("settings-organize-by-creator")?.checked) return destDir;
  const creator = deriveCreatorFromPackageId(packageId);
  if (!creator) return destDir;
  const sep = destDir.includes("\\") ? "\\" : "/";
  return destDir.replace(/[\\/]+$/, "") + sep + creator;
}

// Enqueue a download. opts: { packageId, url, filename, host, destDir, label?, onDone?, nest? }.
// onDone(status, { destDir, filename, localPath }) fires when the job settles.
// nest: false saves into destDir as given (no per-creator subfolder).
function queueDownload(opts) {
  const o = opts || {};
  if (!o.url || !o.destDir) {
    addLog("Downloads: missing URL or destination folder.");
    return null;
  }
  const existing = o.packageId ? findDownloadJob(o.packageId) : null;
  if (existing) {
    existing.onDone = typeof o.onDone === "function" ? o.onDone : existing.onDone;
    existing.onProgress = typeof o.onProgress === "function" ? o.onProgress : existing.onProgress;
    return existing.id;
  }
  const job = {
    id: ++downloadJobSeq,
    packageId: o.packageId || "",
    label: o.label || o.filename || o.packageId || "download",
    filename: o.filename || "",
    url: o.url,
    host: o.host || "",
    destDir: o.nest === false ? o.destDir : nestDestByCreator(o.destDir, o.packageId || o.filename || ""),
    status: "queued",
    percent: 0,
    detail: "",
    error: null,
    taskId: null,
    localPath: null,
    onDone: typeof o.onDone === "function" ? o.onDone : null,
    onProgress: typeof o.onProgress === "function" ? o.onProgress : null,
  };
  state.downloads.push(job);
  renderDownloadsPanel();
  updateDownloadsBadge();
  pumpDownloadQueue();
  return job.id;
}

// Start queued jobs up to the concurrency cap.
function pumpDownloadQueue() {
  const active = state.downloads.filter(
    (j) => j.status === "downloading" || j.status === "cancelling"
  ).length;
  let slots = DOWNLOAD_CONCURRENCY - active;
  if (slots <= 0) return;
  for (const job of state.downloads) {
    if (slots <= 0) break;
    if (job.status === "queued") {
      slots -= 1;
      runDownloadJob(job);
    }
  }
}

async function runDownloadJob(job) {
  if (!invoke) {
    job.status = "failed";
    job.error = "backend unavailable";
    renderDownloadsPanel();
    updateDownloadsBadge();
    return;
  }
  job.status = "downloading";
  job.percent = 0;
  job.detail = "";
  job.error = null;
  renderDownloadsPanel();
  updateDownloadsBadge();
  if (job.onProgress) { try { job.onProgress(job); } catch (_e) {} }

  let result = null;
  try {
    const handle = await invoke("start_download_one_task", {
      packageId: job.packageId,
      downloadUrl: job.url,
      filename: job.filename || "",
      destDir: job.destDir,
    });
    job.taskId = handle && handle.id != null ? handle.id : null;
    if (job.taskId == null) throw new Error("failed to start download");
    // Cancel was clicked while the task was still starting (no task id to
    // signal yet): stop it now.
    if (job.status === "cancelling") {
      try { await invoke("cancel_task", { taskId: job.taskId }); } catch (_e) {}
    }
    while (true) {
      await new Promise((r) => setTimeout(r, 300));
      let payload;
      try {
        payload = await invoke("get_task_progress", { taskId: job.taskId });
      } catch (_e) {
        throw new Error("lost contact with download task");
      }
      if (!payload) break;
      if (job.status === "downloading") {
        job.percent = Math.round(Math.max(0, Math.min(1, Number(payload.progress ?? 0))) * 100);
        job.detail = String(payload.message ?? "");
        updateDownloadJobUI(job);
        if (job.onProgress) { try { job.onProgress(job); } catch (_e) {} }
      }
      if (payload.error) throw new Error(String(payload.error));
      if (payload.done) { result = payload.download_vars_result ?? null; break; }
    }
    try { await invoke("clear_task", { taskId: job.taskId }); } catch (_e) {}
  } catch (e) {
    job.status = "failed";
    job.error = String(e);
    addLog(`Downloads: ${job.label} — ${String(e)}`);
    renderDownloadsPanel();
    updateDownloadsBadge();
    pumpDownloadQueue();
    if (job.onDone) { try { job.onDone("failed", { destDir: job.destDir, filename: job.filename, localPath: "" }); } catch (_e) {} }
    return;
  }

  const it = result && result.items && result.items[0];
  const status = it ? it.status : "failed";
  const fname = (it && it.filename) || job.filename || `${job.packageId}.var`;
  const dir = (result && result.dest_dir) || job.destDir;
  let localPath = "";
  if (dir && fname) {
    const sep = dir.includes("\\") ? "\\" : "/";
    localPath = dir.replace(/[\\/]+$/, "") + sep + fname;
  }
  if (status === "downloaded" || status === "exists") {
    job.status = "done";
    job.percent = 100;
    job.localPath = localPath;
    addLog(`Downloads: ${job.label} → ${dir}`);
  } else if (status === "cancelled") {
    job.status = "cancelled";
  } else {
    job.status = "failed";
    job.error = (it && it.error) || "download failed";
  }
  renderDownloadsPanel();
  updateDownloadsBadge();
  pumpDownloadQueue();
  if (job.onDone) {
    try { job.onDone(job.status, { destDir: dir, filename: fname, localPath }); } catch (_e) {}
  }
}

async function cancelDownload(id) {
  const job = state.downloads.find((j) => j.id === id);
  if (!job) return;
  if (job.status === "queued") {
    job.status = "cancelled";
    renderDownloadsPanel();
    updateDownloadsBadge();
    pumpDownloadQueue();
    if (job.onDone) { try { job.onDone("cancelled", { destDir: job.destDir, filename: job.filename, localPath: "" }); } catch (_e) {} }
    return;
  }
  if (job.status !== "downloading") return;
  job.status = "cancelling";
  renderDownloadsPanel();
  if (invoke && job.taskId != null) {
    try { await invoke("cancel_task", { taskId: job.taskId }); } catch (_e) {}
  }
}

// Queued jobs go first, so finishing cancels can't start them.
function cancelAllDownloads() {
  const queued = state.downloads.filter((j) => j.status === "queued");
  const running = state.downloads.filter((j) => j.status === "downloading");
  for (const job of queued) cancelDownload(job.id);
  for (const job of running) cancelDownload(job.id);
}

function retryDownload(id) {
  const job = state.downloads.find((j) => j.id === id);
  if (!job) return;
  job.status = "queued";
  job.percent = 0;
  job.detail = "";
  job.error = null;
  job.taskId = null;
  job.localPath = null;
  renderDownloadsPanel();
  updateDownloadsBadge();
  pumpDownloadQueue();
}

function clearFinishedDownloads() {
  state.downloads = state.downloads.filter((j) => downloadJobActive(j));
  renderDownloadsPanel();
  updateDownloadsBadge();
}

function syncDownloadsHeadActions() {
  const active = state.downloads.filter((j) => j.status === "queued" || j.status === "downloading").length;
  const cancelAll = $("downloads-cancel-all");
  if (cancelAll) {
    cancelAll.classList.toggle("hidden", active === 0);
    cancelAll.textContent = active > 1 ? `Cancel all (${active})` : "Cancel all";
  }
  const clear = $("downloads-clear");
  if (clear) clear.disabled = !state.downloads.some((j) => !downloadJobActive(j));
}

function dlItemInner(job) {
  const label = escapeHtml(job.label || job.packageId || job.filename || "download");
  const hostLabel =
    job.host === "hub" ? "Hub"
    : job.host === "pixeldrain" ? "Pixeldrain"
    : job.host === "mediafire" ? "MediaFire"
    : "";
  let body = "";
  let actions = "";
  let statusChip = "";
  switch (job.status) {
    case "queued":
      statusChip = `<span class="dl-status">Queued</span>`;
      actions = `<button class="ghost-button dl-mini" type="button" data-dl-cancel="${job.id}">Cancel</button>`;
      break;
    case "downloading":
    case "cancelling": {
      const detail = job.status === "cancelling" ? "" : (job.detail || "");
      const indeterminate = job.status === "downloading" && !detail.includes(" / ");
      const pct = Math.max(0, Math.min(100, Math.round(job.percent || 0)));
      const fill = indeterminate
        ? `<div class="dv-row-fill dv-row-fill-indeterminate"></div>`
        : `<div class="dv-row-fill" style="width:${pct}%"></div>`;
      const pctText = job.status === "cancelling" ? "Cancelling…" : (indeterminate ? "" : `${pct}%`);
      body =
        `<div class="dv-row-progress"><div class="dv-row-track">${fill}</div>` +
        `<span class="dv-row-pct">${pctText}</span></div>` +
        (detail ? `<div class="dv-row-detail">${escapeHtml(detail)}</div>` : "");
      if (job.status === "downloading") {
        actions = `<button class="ghost-button dl-mini" type="button" data-dl-cancel="${job.id}">Cancel</button>`;
      }
      break;
    }
    case "done":
    case "exists":
      statusChip = `<span class="dl-status dl-status-done"><span class="material-symbols-outlined">check_circle</span>Done</span>`;
      break;
    case "cancelled":
      statusChip = `<span class="dl-status">Cancelled</span>`;
      actions = `<button class="ghost-button dl-mini" type="button" data-dl-retry="${job.id}">Retry</button>`;
      break;
    case "failed":
      statusChip = `<span class="dl-status dl-status-failed">Failed</span>`;
      actions = `<button class="ghost-button dl-mini" type="button" data-dl-retry="${job.id}">Retry</button>`;
      break;
    default:
      break;
  }
  const errLine = job.status === "failed" && job.error ? `<div class="dl-item-err">${escapeHtml(job.error)}</div>` : "";
  const openable = Boolean(job.packageId || job.localPath);
  const openAttrs = openable ? ` data-dl-open="${job.id}" title="Open in VAR Details"` : "";
  return (
    `<div class="dl-item${openable ? " dl-item-openable" : ""}" data-dl-id="${job.id}"${openAttrs}>` +
    `<div class="dl-item-head"><span class="dl-item-name" title="${label}">${label}</span>` +
    (hostLabel ? `<span class="chip dl-host">${escapeHtml(hostLabel)}</span>` : "") +
    statusChip +
    `</div>` +
    body +
    errLine +
    (actions ? `<div class="dl-item-actions">${actions}</div>` : "") +
    `</div>`
  );
}

function renderDownloadsPanel() {
  syncDownloadsHeadActions();
  const list = $("downloads-list");
  if (!list) return;
  if (!state.downloads.length) {
    list.innerHTML = `<div class="downloads-empty">No downloads yet.</div>`;
    return;
  }
  list.innerHTML = state.downloads.slice().reverse().map(dlItemInner).join("");
}

// In-place progress update for one job's row (avoids restarting CSS animations).
function updateDownloadJobUI(job) {
  const list = $("downloads-list");
  if (!list) return;
  const el = list.querySelector(`.dl-item[data-dl-id="${job.id}"]`);
  if (!el) { renderDownloadsPanel(); return; }
  const detailText = job.status === "cancelling" ? "" : (job.detail || "");
  const indeterminate = job.status === "downloading" && !detailText.includes(" / ");
  const fill = el.querySelector(".dv-row-fill");
  const pctEl = el.querySelector(".dv-row-pct");
  const detailEl = el.querySelector(".dv-row-detail");
  if (!fill || !pctEl) { renderDownloadsPanel(); return; }
  const fillIsIndet = fill.classList.contains("dv-row-fill-indeterminate");
  if (fillIsIndet !== indeterminate || (detailText && !detailEl)) { renderDownloadsPanel(); return; }
  if (!indeterminate) {
    const pct = Math.max(0, Math.min(100, Math.round(job.percent || 0)));
    fill.style.width = `${pct}%`;
    pctEl.textContent = job.status === "cancelling" ? "Cancelling…" : `${pct}%`;
  }
  if (detailEl) detailEl.textContent = detailText;
}

function updateDownloadsBadge() {
  const badge = $("downloads-badge");
  if (!badge) return;
  const active = state.downloads.filter(downloadJobActive).length;
  if (active > 0) {
    badge.textContent = String(active);
    badge.classList.remove("hidden");
  } else {
    badge.classList.add("hidden");
  }
}

function openDownloadsPanel() {
  $("downloads-popover")?.classList.add("open");
  $("downloads-overlay")?.classList.add("open");
}
function closeDownloadsPanel() {
  $("downloads-popover")?.classList.remove("open");
  $("downloads-overlay")?.classList.remove("open");
}
window.__toggleDownloads = function () {
  const p = $("downloads-popover");
  if (!p) return;
  if (p.classList.contains("open")) {
    closeDownloadsPanel();
  } else {
    renderDownloadsPanel();
    openDownloadsPanel();
  }
};

function setupDownloadsManager() {
  const overlay = $("downloads-overlay");
  if (overlay) overlay.addEventListener("click", closeDownloadsPanel);
  const closeBtn = $("downloads-close");
  if (closeBtn) closeBtn.addEventListener("click", closeDownloadsPanel);
  const clearBtn = $("downloads-clear");
  if (clearBtn) clearBtn.addEventListener("click", clearFinishedDownloads);
  $("downloads-cancel-all")?.addEventListener("click", cancelAllDownloads);
  // Top-bar button: opens the shared dependency modal in paste-text mode
  // (defined in the VAR Details dep-scan block); its downloads land in this panel.
  $("find-deps-toggle")?.addEventListener("click", () => {
    closeDownloadsPanel();
    depStartTextScan();
  });

  const list = $("downloads-list");
  if (list) {
    list.addEventListener("click", (event) => {
      const c = event.target.closest("[data-dl-cancel]");
      if (c) { cancelDownload(Number(c.getAttribute("data-dl-cancel"))); return; }
      const r = event.target.closest("[data-dl-retry]");
      if (r) { retryDownload(Number(r.getAttribute("data-dl-retry"))); return; }
      const o = event.target.closest("[data-dl-open]");
      if (o) {
        const job = state.downloads.find((j) => j.id === Number(o.getAttribute("data-dl-open")));
        if (!job) return;
        closeDownloadsPanel();
        openCandidatePackageInVarDetails(job.packageId, job.localPath || "");
      }
    });
  }
  renderDownloadsPanel();
  updateDownloadsBadge();
}

function selectedKeySet() {
  return new Set(state.selectedKeys);
}

function normalizeSelections() {
  const all = new Set(getFilteredGroups().map((group) => group.key));
  state.selectedKeys = state.selectedKeys.filter((key) => all.has(key));
  if (state.selectedKey && !all.has(state.selectedKey)) {
    state.selectedKey = state.selectedKeys[0] ?? null;
  }
  if (!state.selectedKey && state.selectedKeys.length) {
    state.selectedKey = state.selectedKeys[0];
  }
  if (!state.selectedKey && all.size) {
    state.selectedKey = [...all][0];
    state.selectedKeys = [state.selectedKey];
  }
}

// The backend returns a per-package `bundle_index` map:
// `pkg_id → vam_path → {sibling paths}`. The engine atomically cascades
// `.vam` removal to those siblings (.vaj/.vab/.jpg + .vaj-referenced textures
// like alp2.png), so showing them as independent dedup rows is misleading —
// the user picks one bundle member and the rest still vanish. This helper
// returns the flat set of `pkgid|path` entries the UI should hide.
function getBundleManagedMembersSet() {
  const index = state.scan?.bundle_index;
  if (!index) return null;
  const out = new Set();
  for (const pkgId of Object.keys(index)) {
    const vamMap = index[pkgId] ?? {};
    for (const vamPath of Object.keys(vamMap)) {
      const siblings = vamMap[vamPath] ?? [];
      for (const sibling of siblings) {
        out.add(`${pkgId}|${sibling}`);
      }
    }
  }
  return out;
}

// Returns siblings managed by a given `.vam` in a given package — used by
// the keep-source panel to surface how many extra files will follow a `.vam`
// out the door when the user picks a non-self keep target.
function getBundleSiblingsFor(packageId, vamPath) {
  const index = state.scan?.bundle_index;
  const siblings = index?.[packageId]?.[vamPath];
  return siblings ?? [];
}

// Returns expected-but-missing bundle siblings for a (package, parent) pair,
// as emitted by the backend's `bundle_missing_index`. A non-empty list means
// the package claims to bundle siblings that aren't actually in its ZIP —
// picking that ref as a dedup keep would leave dangling SELF:/sibling refs
// (e.g. game logs "Path Eros:/...png is not valid"). Refs where the entire
// bundle is missing are already dropped from duplicate groups at scan time;
// partial misses survive so the UI can show the user what's broken.
function getMissingSiblings(packageId, internalPath) {
  const index = state.scan?.bundle_missing_index;
  const missing = index?.[packageId]?.[internalPath];
  return Array.isArray(missing) ? missing : [];
}

function isRefIncomplete(ref) {
  if (!ref) return false;
  return getMissingSiblings(ref.package_id, ref.internal_path).length > 0;
}

// Builds the inline warning chip + tooltip rendered on incomplete ref rows.
// Returns "" when the ref's bundle is complete so callers can unconditionally
// concatenate without guard checks.
function renderIncompleteRefWarning(ref) {
  const missing = getMissingSiblings(ref?.package_id, ref?.internal_path);
  if (!missing.length) return "";
  const tooltip = `Missing in this VAR:\n${missing.join("\n")}`;
  return `
    <div class="source-incomplete-warning" title="${escapeAttribute(tooltip)}">
      <span class="material-symbols-outlined source-incomplete-icon" aria-hidden="true">warning</span>
      <span class="source-incomplete-text">${missing.length} bundled file${missing.length === 1 ? "" : "s"} missing in this VAR — unsafe as keep source</span>
    </div>
  `;
}

// Drops duplicate groups whose only remaining refs (after hiding managed
// bundle members) can't drive a dedup decision. A group needs at least two
// refs spanning at least two packages to be actionable. The engine's bundle
// cascade handles the hidden members during `start_execute_task`, so dropping
// these groups from the UI loses no functionality.
function filterEverythingBundleMembers(groups) {
  const managed = getBundleManagedMembersSet();
  if (!managed || managed.size === 0) return groups;
  const filtered = [];
  for (const group of groups) {
    const visibleRefs = group.refs.filter(
      (ref) => !managed.has(`${ref.package_id}|${ref.internal_path}`)
    );
    if (visibleRefs.length < 2) continue;
    const visiblePkgIds = new Set(visibleRefs.map((r) => r.package_id));
    if (visiblePkgIds.size < 2) continue;
    filtered.push({
      ...group,
      refs: visibleRefs,
      package_ids: [...visiblePkgIds],
    });
  }
  return filtered;
}

function getScopedGroups() {
  // Overview only — Find Duplicates dispatches through dbfGetScopedGroups
  // which intentionally skips this bundle filter so it can show every
  // target-VAR resource individually. Keep the filter for Overview where
  // bundle-cascade dedup makes the collapsed view correct.
  const groups = filterEverythingBundleMembers(state.scan?.groups ?? []);
  const targetPackageId = getActiveTargetPackageId();
  if (!targetPackageId) {
    return groups;
  }
  return groups.filter((group) => group.refs.some((ref) => ref.package_id === targetPackageId));
}

function deriveTargetPackageIdFromPath(filePath) {
  const normalizedPath = String(filePath ?? "").trim();
  if (!normalizedPath) {
    return null;
  }
  const fileName = normalizedPath.split(/[/\\]/).pop() ?? "";
  return fileName.toLowerCase().endsWith(".var") ? fileName.slice(0, -4) : fileName || null;
}

function deriveTargetPackageId() {
  const filePath = String(state.targetVarPath ?? "").trim();
  if (!filePath) {
    return null;
  }
  return deriveTargetPackageIdFromPath(filePath);
}

function getActiveTargetPackageId() {
  return state.targetPackageId || deriveTargetPackageId();
}

// =====================================================================
// Per-page state isolation
// Overview and Find Duplicates share the same DOM but maintain separate
// state — scan results, keep choices, target VAR, form inputs, even
// in-flight scan tasks. Switching pages snapshots the current page's
// state into a slot and restores the destination page's slot.
// =====================================================================
const PAGE_STATE_KEYS = [
  "scan", "targetVarPath", "targetPackageId",
  "keepMap", "defaultKeepMap", "selectedKey", "selectedKeys", "groupPage",
  "dbResourceMatches", "dbPackageResources", "previewCache",
  "outputDirAutoSynced", "processVap", "lastDetailKey", "activeTask",
  "dbfMode", "dbfAugmentLoaded",
];

const PAGE_INPUT_IDS = [
  "input-dir", "output-dir", "vap-dir", "target-var-path", "group-filter",
  "dbfind-source-filter",
];

const PAGE_CHECKBOX_IDS = [
  "replace-in-place", "backup-changed", "process-vap",
];

function getRadioValue(name) {
  const checked = document.querySelector(`input[type="radio"][name="${name}"]:checked`);
  return checked ? checked.value : null;
}

function setRadioValue(name, value) {
  const all = document.querySelectorAll(`input[type="radio"][name="${name}"]`);
  let matched = false;
  for (const el of all) {
    const ok = el.value === value;
    el.checked = ok;
    if (ok) matched = true;
  }
  if (!matched && all.length > 0) all[0].checked = true;
}

function makeFreshPageState() {
  return {
    scan: null,
    targetVarPath: "",
    targetPackageId: null,
    keepMap: {},
    defaultKeepMap: {},
    selectedKey: null,
    selectedKeys: [],
    groupPage: 0,
    dbResourceMatches: {},
    dbPackageResources: {},
    previewCache: {},
    outputDirAutoSynced: true,
    processVap: false,
    lastDetailKey: null,
    activeTask: null,
    dbfMode: "db",
    dbfAugmentLoaded: false,
    inputs: {},
    checkboxes: {},
  };
}

function snapshotCurrentPage() {
  const slot = state.pageStates[state.currentPage] ?? makeFreshPageState();
  state.pageStates[state.currentPage] = slot;
  for (const key of PAGE_STATE_KEYS) {
    slot[key] = state[key];
  }
  slot.inputs = {};
  slot.checkboxes = {};
  for (const id of PAGE_INPUT_IDS) {
    const el = $(id);
    if (el) slot.inputs[id] = el.value;
  }
  for (const id of PAGE_CHECKBOX_IDS) {
    const el = $(id);
    if (el) slot.checkboxes[id] = !!el.checked;
  }
  // Stop the per-page polling timer; we restart it in restorePage if the
  // destination page has its own active task.
  if (state.pollTimer) {
    stopPollingTask();
  }
}

function restorePage(pageName) {
  if (!state.pageStates[pageName]) {
    state.pageStates[pageName] = makeFreshPageState();
  }
  const slot = state.pageStates[pageName];
  state.currentPage = pageName;
  for (const key of PAGE_STATE_KEYS) {
    state[key] = slot[key];
  }
  for (const id of PAGE_INPUT_IDS) {
    const el = $(id);
    if (el) el.value = slot.inputs?.[id] ?? "";
  }
  for (const id of PAGE_CHECKBOX_IDS) {
    const el = $(id);
    if (el) el.checked = !!slot.checkboxes?.[id];
  }
  // Clean VARs (dbf-page) is the sole workspace page; reveal it when active.
  // The renderer dispatch in renderGroups/renderDetail/renderSummary keys off
  // state.currentPage. (The former Overview ov-page section was removed.)
  const dbfPage = $("dbf-page");
  if (dbfPage) dbfPage.classList.toggle("hidden", pageName !== "db-find");
  hideProgress();
  renderModeControls();
  renderSummary();
  renderGroups();
  renderDetail();
  updateTargetVarInfo();
  if (pageName === "db-find") dbfUpdateVarInfo();
  syncReplaceOptions();
  if (state.activeTask) {
    showProgress(state.activeTask.kind, 0, "");
    startPollingTask();
  }
}

function switchPage(newPage) {
  if (state.currentPage === newPage) return;
  snapshotCurrentPage();
  restorePage(newPage);
}

window.__switchPage = switchPage;

function resetScanState({ clearTargetVar = false } = {}) {
  state.scan = null;
  state.defaultKeepMap = {};
  state.keepMap = {};
  bumpKeepMutation();
  state.previewCache = {};
  state.dbResourceMatches = {};
  state.dbPackageResources = {};
  state.selectedKey = null;
  state.selectedKeys = [];
  state.groupPage = 0;
  if (clearTargetVar) {
    state.targetVarPath = "";
    state.targetPackageId = null;
    if ($("target-var-path")) {
      $("target-var-path").value = "";
    }
  } else {
    state.targetPackageId = deriveTargetPackageId();
  }
}

function clearGroupFilter() {
  if ($("group-filter")) {
    $("group-filter").value = "";
  }
}

function handleTargetVarChange(nextPath) {
  const previousPath = String(state.targetVarPath ?? "").trim();
  const normalizedNextPath = String(nextPath ?? "").trim();
  const changed = previousPath !== normalizedNextPath;

  state.targetVarPath = normalizedNextPath;
  state.targetPackageId = deriveTargetPackageIdFromPath(normalizedNextPath);

  if (changed && state.scan) {
    resetScanState();
    clearGroupFilter();
    hideContextMenu();
    addLog(t("targetVarChangedReset"));
  }

  updateTargetVarInfo();
  renderSummary();
  renderGroups();
  renderDetail();
}

function updateTargetVarInfo() {
  const panel = $("var-info-panel");
  if (!panel) return;

  const body = $("var-info-body");
  const emptyHint = $("var-info-empty");
  const thumb = $("var-info-scene-img").parentElement;
  const sceneRow = $("var-info-scene-row");
  const pkgId = getActiveTargetPackageId();
  if (!pkgId) {
    body.classList.add("hidden");
    emptyHint.textContent = t("varInfoEmpty");
    emptyHint.classList.remove("hidden");
    return;
  }

  emptyHint.classList.add("hidden");
  body.classList.remove("hidden");
  $("var-info-label").textContent = pkgId;

  $("var-info-scene-val").textContent = "";
  $("var-info-time-val").textContent = "";
  $("var-info-size-val").textContent = "";
  const img = $("var-info-scene-img");
  img.src = "";
  thumb.classList.add("empty");
  sceneRow.classList.add("hidden");
  thumb.classList.add("hidden");

  invoke("get_var_file_stats", { packagePath: state.targetVarPath })
    .then((stats) => {
      if (getActiveTargetPackageId() !== pkgId) return;

      $("var-info-size-val").textContent = formatBytesLocal(stats.size_bytes);

      const date = stats.modified_ms != null ? new Date(stats.modified_ms) : null;
      $("var-info-time-val").textContent = date ? date.toLocaleString() : "—";

      if (stats.scene_image_path) {
        $("var-info-scene-val").textContent = stats.scene_image_path;
        img.src = stats.scene_image_data || "";
        sceneRow.classList.remove("hidden");
        thumb.classList.remove("hidden");
        if (stats.scene_image_data) thumb.classList.remove("empty");
      } else {
        $("var-info-scene-val").textContent = "";
        img.src = "";
        sceneRow.classList.add("hidden");
        thumb.classList.add("hidden");
      }
    })
    .catch(() => {
      if (getActiveTargetPackageId() !== pkgId) return;
      $("var-info-size-val").textContent = "—";
      $("var-info-time-val").textContent = "—";
      $("var-info-scene-val").textContent = "";
      img.src = "";
      sceneRow.classList.add("hidden");
      thumb.classList.add("hidden");
    });
}

function dbfUpdateVarInfo() {
  const panel = $("dbf-var-info-panel");
  if (!panel) return;
  const body = $("dbf-var-info-body");
  const emptyHint = $("dbf-var-info-empty");
  const img = $("dbf-var-info-scene-img");
  const thumb = img?.parentElement ?? null;
  const sceneRow = $("dbf-var-info-scene-row");
  const pkgId = dbfGetActiveTargetPackageId();
  const filePath = (state.targetVarPath || $("dbf-target-var-path")?.value || "").trim();
  if (!pkgId || !filePath) {
    if (body) body.classList.add("hidden");
    if (emptyHint) {
      emptyHint.textContent = t("varInfoEmpty");
      emptyHint.classList.remove("hidden");
    }
    return;
  }
  if (emptyHint) emptyHint.classList.add("hidden");
  if (body) body.classList.remove("hidden");
  const label = $("dbf-var-info-label");
  if (label) label.textContent = pkgId;
  const sizeVal = $("dbf-var-info-size-val");
  const timeVal = $("dbf-var-info-time-val");
  const sceneVal = $("dbf-var-info-scene-val");
  if (sizeVal) sizeVal.textContent = "";
  if (timeVal) timeVal.textContent = "";
  if (sceneVal) sceneVal.textContent = "";
  if (img) img.src = "";
  if (thumb) thumb.classList.add("empty", "hidden");
  if (sceneRow) sceneRow.classList.add("hidden");
  if (!invoke) return;
  invoke("get_var_file_stats", { packagePath: filePath })
    .then((stats) => {
      if (dbfGetActiveTargetPackageId() !== pkgId) return;
      if (sizeVal) sizeVal.textContent = formatBytesLocal(stats.size_bytes);
      const date = stats.modified_ms != null ? new Date(stats.modified_ms) : null;
      if (timeVal) timeVal.textContent = date ? date.toLocaleString() : "—";
      if (stats.scene_image_path) {
        if (sceneVal) sceneVal.textContent = stats.scene_image_path;
        if (img) img.src = stats.scene_image_data || "";
        if (sceneRow) sceneRow.classList.remove("hidden");
        if (thumb) {
          thumb.classList.remove("hidden");
          if (stats.scene_image_data) thumb.classList.remove("empty");
        }
      }
    })
    .catch(() => {
      if (dbfGetActiveTargetPackageId() !== pkgId) return;
      if (sizeVal) sizeVal.textContent = "—";
      if (timeVal) timeVal.textContent = "—";
    });
}

/// Lightweight, auto-dismissing notification. Visible without the console
/// drawer (which is closed by default). `kind` ∈ "info" | "success" | "error".
/// Reusable by any feature.
const TOAST_ICONS = { success: "check_circle", error: "error", info: "info" };

/// Shows a toast and returns a handle: `update(message, kind?)` to change it in
/// place (e.g. live progress → final result), and `dismiss(afterMs)` to close
/// it (0 = now). Pass `ms = 0` for a sticky toast that stays until dismissed —
/// used for long-running progress.
function showToast(message, kind = "info", ms = 3200) {
  const host = $("toast-host");
  if (!host) return { update() {}, dismiss() {} };
  const el = document.createElement("div");
  el.className = `toast toast-${kind}`;
  const icon = document.createElement("span");
  icon.className = "material-symbols-outlined toast-icon";
  icon.textContent = TOAST_ICONS[kind] || TOAST_ICONS.info;
  const text = document.createElement("span");
  text.className = "toast-text";
  // textContent, not innerHTML — messages carry filenames/errors verbatim.
  text.textContent = String(message ?? "");
  el.append(icon, text);
  host.appendChild(el);
  requestAnimationFrame(() => el.classList.add("show"));

  let timer = null;
  const remove = () => {
    el.classList.remove("show");
    setTimeout(() => el.remove(), 220);
  };
  const arm = (delay) => {
    if (timer) clearTimeout(timer);
    timer = delay > 0 ? setTimeout(remove, delay) : null;
  };
  if (ms > 0) arm(ms);
  el.addEventListener("click", () => {
    if (timer) clearTimeout(timer);
    remove();
  });

  return {
    update(msg, newKind) {
      text.textContent = String(msg ?? "");
      if (newKind) {
        el.className = `toast toast-${newKind} show`;
        icon.textContent = TOAST_ICONS[newKind] || TOAST_ICONS.info;
      }
    },
    dismiss(afterMs = 0) {
      if (afterMs > 0) arm(afterMs);
      else {
        if (timer) clearTimeout(timer);
        remove();
      }
    },
  };
}

function showAppConfirm(message) {
  return new Promise((resolve) => {
    state.pendingDialog = resolve;
    $("dialog-title").textContent = t("dialogConfirmTitle");
    $("dialog-message").textContent = message;
    $("dialog-cancel").textContent = t("dialogCancel");
    $("dialog-confirm").textContent = t("dialogConfirm");
    $("dialog-backdrop").classList.remove("hidden");
  });
}

function closeAppConfirm(result) {
  $("dialog-backdrop").classList.add("hidden");
  const resolver = state.pendingDialog;
  state.pendingDialog = null;
  if (resolver) {
    resolver(Boolean(result));
  }
}

function showDedupComplete(stats, reportPath) {
  const reclaimed = Number(stats.reclaimed_bytes ?? 0);
  const totalBefore = Number(state.scan?.summary?.reclaimable_bytes ?? 0) + reclaimed;
  const afterBytes = Math.max(0, totalBefore - reclaimed);

  const targetPackageId = getActiveTargetPackageId();
  let beforeSize, afterSize;
  if (targetPackageId && state.scan?.package_sizes?.[targetPackageId]) {
    beforeSize = Number(state.scan.package_sizes[targetPackageId]);
  } else {
    beforeSize = Object.values(state.scan?.package_sizes ?? {}).reduce((s, v) => s + Number(v), 0);
  }
  afterSize = Math.max(0, beforeSize - reclaimed);

  $("complete-title").textContent = t("dedupCompleteTitle");
  $("complete-message").textContent = t(
    "runSuccess",
    stats.changed_packages,
    stats.removed_files,
    stats.vap_files_rewritten ?? 0,
    reportPath || ""
  );
  $("complete-sizes").textContent =
    `${t("dedupBefore")}: ${formatBytesLocal(beforeSize)}    ${t("dedupAfter")}: ${formatBytesLocal(afterSize)}    ${t("dedupStripped")}: ${formatBytesLocal(reclaimed)}`;
  $("complete-open").textContent = t("dedupCompleteOpen");
  $("complete-ok").textContent = t("dedupCompleteOk");
  const reportBtn = $("complete-report");
  if (reportPath) {
    reportBtn.textContent = `${t("dedupCompleteReport")}: ${reportPath}`;
    reportBtn.dataset.path = reportPath;
    reportBtn.classList.remove("hidden");
  } else {
    reportBtn.textContent = "";
    reportBtn.dataset.path = "";
    reportBtn.classList.add("hidden");
  }

  // Read from the active Clean VARs (dbf-*) run controls; the Overview controls
  // were removed. Null-safe so a missing control never throws.
  const replace = !!($("dbf-replace-in-place") || $("replace-in-place"))?.checked;
  const backup = !!($("dbf-backup-changed") || $("backup-changed"))?.checked;
  const backupBtn = $("complete-open-backup");
  if (replace && backup) {
    backupBtn.textContent = t("dedupCompleteOpenBackup");
    backupBtn.classList.remove("hidden");
  } else {
    backupBtn.classList.add("hidden");
  }

  $("complete-backdrop").classList.remove("hidden");
}

function closeDedupComplete() {
  const reportBtn = $("complete-report");
  reportBtn.textContent = "";
  reportBtn.dataset.path = "";
  reportBtn.classList.add("hidden");
  // Clear per-page open overrides so the next Overview run isn't redirected.
  $("complete-open").dataset.path = "";
  $("complete-open-backup").dataset.path = "";
  $("complete-backdrop").classList.add("hidden");
}


function getGroupTargetRefs(group) {
  const targetPackageId = getActiveTargetPackageId();
  if (!targetPackageId) {
    return [];
  }
  return group.refs.filter((ref) => ref.package_id === targetPackageId);
}

// Returns the per-resource file size for a group (the target VAR's ref size,
// or refs[0]'s size as a fallback). Used by Find Duplicates renderers for
// the row's size chip and for computing reclaimable bytes including DB matches.
// Prefers effective_size: for .vam/.vmi bundle parents the backend rolls the
// bundled siblings (.vab/.vaj/textures) into effective_size, so the row's size
// chip and DB-mode reclaim reflect the full bundle, not just the tiny manifest.
function getGroupResourceSize(group) {
  if (!group?.refs?.length) return 0;
  const targetPackageId = getActiveTargetPackageId();
  const ref = (targetPackageId && group.refs.find((r) => r.package_id === targetPackageId))
    ?? group.refs[0];
  return Number(ref?.effective_size ?? ref?.size ?? 0);
}

// Returns the CRC32 (unsigned int) used to look up DB matches for a group.
// Prefers the target VAR's ref, then falls back to any ref with a populated
// crc32 — refs in a duplicate group are CRC-equivalent so any one is fine.
// Returns null if no ref has crc32 (the group can't be DB-looked-up).
function getGroupLookupCrc(group) {
  if (!group?.refs?.length) return null;
  const targetPid = getActiveTargetPackageId();
  if (targetPid) {
    const target = group.refs.find(
      (r) => r.package_id === targetPid && r.crc32 != null
    );
    if (target) return target.crc32 >>> 0;
  }
  const any = group.refs.find((r) => r.crc32 != null);
  return any ? any.crc32 >>> 0 : null;
}

// Returns the cached database matches for a group's CRC32, with any (pkg, path)
// pair already present in `group.refs` filtered out so the count reflects only
// *additional* sources discovered in the catalog. Returns an empty array if the
// fetch is still in flight, errored, or hasn't been triggered.
//
// Also filters out DB rows whose `size` differs from the group's size. CRC32 is
// only 32 bits and collisions across unrelated files of different sizes are
// common in real libraries (especially for near-empty Unity binary caches).
// The backend's `prepare_package_changes` validates the kept ref against the
// group's (crc, size) tuple and bails on mismatch — surfacing those mismatches
// in the UI lets the user pick something that will fail to dedup. The user
// reported this with: a `.vmb` group whose Nokisaki DB match had a matching
// CRC but different size; auto-target picked it; dedup failed with "Invalid
// keep choice for duplicate group ...".
function getGroupDbRefs(group) {
  const crc = getGroupLookupCrc(group);
  if (crc == null) return [];
  const entry = state.dbResourceMatches[crc];
  if (!entry || entry.status !== "ready" || !Array.isArray(entry.refs)) return [];
  const localKeys = new Set(group.refs.map((r) => `${r.package_id}|${r.internal_path}`));
  const expectedSize = group.refs.find((r) => r.size != null)?.size ?? null;
  return entry.refs.filter((m) => {
    if (localKeys.has(`${m.package_id}|${m.internal_path}`)) return false;
    if (expectedSize != null && m.size != null && m.size !== expectedSize) return false;
    return true;
  });
}

// Total number of sources for the group — local refs + non-overlapping DB matches.
function getGroupTotalRefCount(group) {
  return group.refs.length + getGroupDbRefs(group).length;
}

// Reclaimable bytes for Find Duplicates. The DB matches aren't files this app
// can delete — they're just evidence the resource exists elsewhere. So as soon
// as any duplicate (local or DB) is found, the one local copy in the target
// VAR becomes recoverable: reclaim = the resource's own file size. With no
// other source, reclaim is zero.
function getGroupDbModeReclaimable(group) {
  const total = getGroupTotalRefCount(group);
  if (total <= 1) return 0;
  return getGroupResourceSize(group);
}

function getGroupScopedMaxReclaimableBytes(group) {
  const targetRefs = getGroupTargetRefs(group);
  if (!targetRefs.length) {
    return Number(group.removable_bytes ?? 0);
  }
  const hasExternalChoice = group.refs.some((ref) => ref.package_id !== targetRefs[0].package_id);
  const targetSizes = targetRefs.map((ref) => Number(ref.effective_size ?? ref.size ?? 0));
  if (hasExternalChoice) {
    return targetSizes.reduce((total, size) => total + size, 0);
  }
  return targetSizes.reduce((total, size) => total + size, 0) - Math.min(...targetSizes, Number.POSITIVE_INFINITY);
}

function getGroupCurrentReclaimableBytes(group) {
  const targetRefs = getGroupTargetRefs(group);
  if (!targetRefs.length) {
    return 0;
  }

  const keepValue = getKeepValue(group);
  if (keepValue === KEEP_ALL_VALUE) {
    return 0;
  }
  return targetRefs
    .filter((ref) => getRefValue(ref) !== keepValue)
    .reduce((total, ref) => total + Number(ref.effective_size ?? ref.size ?? 0), 0);
}

function setSelectionFromClick(key, event) {
  const visibleKeys = getFilteredGroups().map((group) => group.key);
  if (event.shiftKey && state.selectedKey && visibleKeys.includes(state.selectedKey)) {
    const from = visibleKeys.indexOf(state.selectedKey);
    const to = visibleKeys.indexOf(key);
    const [start, end] = from < to ? [from, to] : [to, from];
    state.selectedKeys = visibleKeys.slice(start, end + 1);
    state.selectedKey = key;
    return;
  }

  if (event.ctrlKey || event.metaKey) {
    const selected = new Set(state.selectedKeys);
    if (selected.has(key)) {
      selected.delete(key);
    } else {
      selected.add(key);
    }
    state.selectedKeys = [...selected];
    state.selectedKey = key;
    if (!state.selectedKeys.length) {
      state.selectedKeys = [key];
    }
    return;
  }

  state.selectedKey = key;
  state.selectedKeys = [key];
}

function ensureRightClickSelection(key) {
  if (!state.selectedKeys.includes(key)) {
    state.selectedKey = key;
    state.selectedKeys = [key];
  } else {
    state.selectedKey = key;
  }
}

function setButtonsBusy(isBusy) {
  // Every control gated here lived in the removed Overview workspace. Clean VARs
  // gates its own controls via dbfRenderModeControls/dbfSyncReplaceOptions, so
  // bail out when the Overview DOM is absent.
  if (!$("scan-button")) return;
  const replace = $("replace-in-place").checked;
  const backup = $("backup-changed").checked;
  const enableOutput = !replace || backup;
  $("scan-button").disabled = isBusy;
  $("run-button").disabled = isBusy || !state.scan;
  $("input-dir").disabled = isBusy;
  $("pick-input-button").disabled = isBusy;
  const overviewAddBtn = $("scan-add-folder-button");
  if (overviewAddBtn) overviewAddBtn.disabled = isBusy;
  document
    .querySelectorAll("#scan-additional-dirs .additional-dir-remove")
    .forEach((btn) => {
      btn.disabled = isBusy;
    });
  $("target-var-path").disabled = isBusy;
  $("pick-target-var-button").disabled = isBusy;
  $("output-dir").disabled = isBusy || !enableOutput;
  $("pick-output-button").disabled = isBusy || !enableOutput;
  $("open-output-button").disabled = isBusy || !(replace ? state.targetVarPath : $("output-dir").value.trim());
  $("vap-dir").disabled = isBusy || !state.processVap;
  $("pick-vap-button").disabled = isBusy || !state.processVap;
  $("replace-in-place").disabled = isBusy;
  $("backup-changed").disabled = isBusy || !$("replace-in-place").checked;
  $("process-vap").disabled = isBusy;
}

function showProgress(kind, progress, message) {
  const clamped = Math.max(0, Math.min(1, progress ?? 0));
  const pct = Math.round(clamped * 100);

  if (kind === "bulk_import") {
    const block = $("build-db-bulk-progress");
    if (block) {
      block.classList.remove("hidden");
      const fill = $("build-db-bulk-progress-fill");
      const pctEl = $("build-db-bulk-progress-pct");
      const counts = $("build-db-bulk-progress-counts");
      const status = $("build-db-bulk-progress-status");
      if (fill) fill.style.width = `${pct}%`;
      if (pctEl) pctEl.textContent = `${pct}%`;
      if (counts) counts.textContent = message ?? "";
      if (status) status.textContent = t("bulkImportStatusActive");
    }
    return;
  }

  const title = kind === "scan" ? t("progressScan") : t("progressRun");
  // The Overview progress card was removed; guard in case it is absent.
  const ovCard = $("progress-card");
  if (ovCard) {
    ovCard.classList.remove("hidden");
    $("progress-title").textContent = title;
    $("progress-bar").style.width = `${pct}%`;
    $("progress-percent").textContent = `${pct}%`;
    $("progress-message").textContent = message ?? "";
  }
  // Mirror progress onto the Clean VARs progress card (always present in
  // dbf-page). The user's active page sees the bar regardless of which page
  // kicked off the task.
  const dbfCard = $("dbf-progress-card");
  if (dbfCard) {
    dbfCard.classList.remove("hidden");
    const t1 = $("dbf-progress-title"); if (t1) t1.textContent = title;
    const b1 = $("dbf-progress-bar"); if (b1) b1.style.width = `${pct}%`;
    const p1 = $("dbf-progress-percent"); if (p1) p1.textContent = `${pct}%`;
    const m1 = $("dbf-progress-message"); if (m1) m1.textContent = message ?? "";
  }

  const buildDbBlock = $("build-db-progress");
  if (buildDbBlock && kind === "scan") {
    buildDbBlock.classList.remove("hidden");
    const fill = $("build-db-progress-fill");
    const pctEl = $("build-db-progress-pct");
    const targetEl = $("build-db-progress-target");
    if (fill) fill.style.width = `${pct}%`;
    if (pctEl) pctEl.textContent = `${pct}%`;
    if (targetEl) targetEl.textContent = message ?? "";
  }
}

function hideProgress() {
  const ovCard = $("progress-card");
  if (ovCard) {
    ovCard.classList.add("hidden");
    $("progress-bar").style.width = "0%";
    $("progress-percent").textContent = "0%";
    $("progress-message").textContent = "";
  }
  const dbfCard = $("dbf-progress-card");
  if (dbfCard) {
    dbfCard.classList.add("hidden");
    const b1 = $("dbf-progress-bar"); if (b1) b1.style.width = "0%";
    const p1 = $("dbf-progress-percent"); if (p1) p1.textContent = "0%";
    const m1 = $("dbf-progress-message"); if (m1) m1.textContent = "";
  }

  const buildDbBlock = $("build-db-progress");
  if (buildDbBlock) {
    buildDbBlock.classList.add("hidden");
    const fill = $("build-db-progress-fill");
    const pctEl = $("build-db-progress-pct");
    const targetEl = $("build-db-progress-target");
    if (fill) fill.style.width = "0%";
    if (pctEl) pctEl.textContent = "0%";
    if (targetEl) targetEl.textContent = "";
  }

  const bulkBlock = $("build-db-bulk-progress");
  if (bulkBlock) {
    bulkBlock.classList.add("hidden");
    const fill = $("build-db-bulk-progress-fill");
    const pctEl = $("build-db-bulk-progress-pct");
    const counts = $("build-db-bulk-progress-counts");
    const status = $("build-db-bulk-progress-status");
    if (fill) fill.style.width = "0%";
    if (pctEl) pctEl.textContent = "0%";
    if (counts) counts.textContent = "";
    if (status) status.textContent = t("bulkImportStatusIdle");
  }
}

function themeToggleIcon(theme) {
  if (theme === "light") return "moon";
  if (theme === "dark") return "sun";
  return "globe";
}

function themeToggleLabel(theme) {
  const idx = THEMES.indexOf(theme);
  const next = THEMES[(idx + 1) % THEMES.length] ?? "dark";
  const labelKey = {
    light: "themeButtonLight",
    dark: "themeButtonDark",
    nord: "themeButtonNord",
    solarized: "themeButtonSolarized",
    monolith: "themeButtonMonolith",
    amber: "themeButtonAmber",
    emerald: "themeButtonEmerald",
    midnight: "themeButtonMidnight",
    cyber: "themeButtonCyber",
    porcelain: "themeButtonPorcelain",
    frost: "themeButtonFrost",
    circuit: "themeButtonCircuit",
    backstage: "themeButtonBackstage",
  }[next];
  return labelKey ? t(labelKey) : "";
}

function updateStaticCopy() {
  document.documentElement.lang = "en";
  document.documentElement.dataset.theme = state.theme;
  document.title = t("appTitle");
  $("eyebrow").textContent = t("eyebrow");
  $("app-title").textContent = t("appTitle");
  for (const cfg of Object.values(ADDITIONAL_DIR_SECTIONS)) {
    const lbl = $(cfg.labelId);
    if (lbl) lbl.textContent = t("scanAdditionalDirsLabel");
    if ($(cfg.addBtnId)) {
      setButtonIcon(cfg.addBtnId, "folderOpen", t("addFolder"), { showLabel: true });
    }
  }
  updateTargetVarInfo();
  setButtonIcon("theme-toggle", themeToggleIcon(state.theme), themeToggleLabel(state.theme));
  // The Overview workspace (its inputs, stat cards, keep-strategy panel, and
  // scan/run buttons) was removed; Clean VARs sets its own labels via
  // applyDbFindCopy() below. Only the global header, theme toggle, and confirm
  // dialog copy are set here.
  $("dialog-title").textContent = t("dialogConfirmTitle");
  $("dialog-cancel").textContent = t("dialogCancel");
  $("dialog-confirm").textContent = t("dialogConfirm");
  applySettingsCopy();
  applyDbFindCopy();
  renderModeControls();
  renderSummary();
  renderGroups();
  renderDetail();
  renderLogs();
}

// Mirror of the Overview labels onto the dbf-* duplicates. Lets the Find
// Duplicates page render correctly in both languages without sharing any
// id with Overview.
function applyDbFindCopy() {
  const setText = (id, value) => { const el = $(id); if (el) el.textContent = value; };
  setText("dbf-hero-copy", t("heroCopy"));
  setText("dbf-input-label", t("inputLabel"));
  setText("dbf-output-label", t("outputLabel"));
  setText("dbf-vap-label", t("vapLabel"));
  setText("dbf-target-var-label", t("targetVarLabel"));
  setText("dbf-mode-label", "Mode");
  setText("dbf-replace-in-place-label", t("replaceInPlace"));
  setText("dbf-backup-changed-label", t("backupChanged"));
  setText("dbf-process-vap-label", t("processVap"));
  setText("dbf-replace-warning", t("replaceWarning"));
  setText("dbf-var-info-size-key", t("varInfoSize"));
  setText("dbf-var-info-time-key", t("varInfoTime"));
  setText("dbf-var-info-scene-key", t("varInfoScene"));
  const dbfVarInfoBody = $("dbf-var-info-body");
  if (dbfVarInfoBody?.classList.contains("hidden")) {
    setText("dbf-var-info-empty", t("varInfoEmpty"));
  }
  setButtonIcon("dbf-pick-input-button", "folderOpen", t("browse"), { showLabel: true });
  setButtonIcon("dbf-pick-target-var-button", "folderOpen", t("browse"), { showLabel: true });
  setText("dbf-open-var-details-label", t("openVarDetails"));
  setButtonIcon("dbf-pick-output-button", "folderOpen", t("browse"), { showLabel: true });
  setButtonIcon("dbf-open-output-button", "folder", t("openOutput"), { showLabel: true });
  setButtonIcon("dbf-pick-vap-button", "folderOpen", t("browse"), { showLabel: true });
  setButtonIcon("dbf-scan-button", "search", t("scan"), { showLabel: true });
  setButtonIcon("dbf-run-button", "wand", t("run"), { showLabel: true });
  setText("dbf-stat-packages-label", t("statPackages"));
  setText("dbf-stat-groups-label", t("statGroups"));
  setText("dbf-stat-space-label", t("statSpace"));
  setText("dbf-stat-filtered-label", t("statFiltered"));
  setText("dbf-stat-modified-label", t("statModified"));
  setText("dbf-stat-current-size-label", t("statCurrentSize"));
  setText("dbf-stat-estimated-size-label", t("statEstimatedSize"));
  setText("dbf-stat-reclaimed-size-label", t("statReclaimedSize"));
  setText("dbf-groups-title", t("groupsTitle"));
  setText("dbf-groups-subtitle", t("groupsSubtitle"));
  setText("dbf-selection-hint", t("selectionHint"));
  setText("dbf-detail-title", t("detailTitle"));
  setText("dbf-detail-subtitle", getDetailSubtitleText());
  setText("dbf-detail-empty", t("detailEmpty"));
  setText("dbf-detail-hash-label", t("detailHash"));
  setText("dbf-keep-source-title", getKeepSourceTitleText());
  setText("dbf-local-section-title", "Local candidates");
  setText("dbf-db-section-title", "Database candidates");
  setText("dbf-quick-actions-title", t("quickActionsTitle"));
  setText("dbf-quick-actions-hint", t("quickActionsHint"));
  setText("dbf-reset-keep-button", t("resetKeep"));
  setText("dbf-export-resource-button", t("exportResource"));
  setText("dbf-apply-filtered-button", t("applyFiltered"));
  setText("dbf-auto-target-button", t("autoTarget"));
  const dbfAutoBtn = $("dbf-auto-target-button");
  if (dbfAutoBtn) dbfAutoBtn.title = t("autoTargetHint");
  setText("dbf-fav-source-button", t("favSource"));
  const dbfFavSrcBtn = $("dbf-fav-source-button");
  if (dbfFavSrcBtn) dbfFavSrcBtn.title = t("favSourceHint");
  const dbfFilterInput = $("dbf-group-filter");
  if (dbfFilterInput) dbfFilterInput.placeholder = t("filterPlaceholder");
  const dbfSrcFilterInput = $("dbf-source-filter");
  if (dbfSrcFilterInput) dbfSrcFilterInput.placeholder = t("dbfindSourceFilterPlaceholder") || dbfSrcFilterInput.placeholder;
}

function applySettingsCopy() {
  const setText = (id, value) => {
    const node = $(id);
    if (node) node.textContent = value;
  };
  setText("settings-title", t("settingsTitle"));
  setText("settings-subtitle", t("settingsSubtitle"));

  setText("settings-paths-title", t("settingsPathsTitle"));
  setText("settings-paths-desc", t("settingsPathsDesc"));
  setText("settings-path-input-label", t("inputLabel"));
  setText("settings-path-output-label", t("outputLabel"));
  setText("settings-path-vap-label", t("vapLabel"));
  setText("settings-path-input-desc", t("settingsPathInputDesc"));
  setText("settings-path-output-desc", t("settingsPathOutputDesc"));
  setText("settings-path-vap-desc", t("settingsPathVapDesc"));
  setText("settings-pick-input-label", t("browse"));
  setText("settings-pick-output-label", t("browse"));
  setText("settings-pick-vap-label", t("browse"));
  setText("build-db-input-label", t("inputLabel"));
  setText("build-db-pick-input-label", t("browse"));
  setText("build-db-status-title", t("buildDbStatusTitle"));
  setText("build-db-stat-packages-label", t("buildDbStatPackages"));
  setText("build-db-stat-resources-label", t("buildDbStatResources"));
  setText("build-db-stat-size-label", t("buildDbStatSize"));
  setText("build-db-stat-last-label", t("buildDbStatLast"));
  setText("build-db-bulk-dropzone-text", t("bulkImportPickPrompt"));
  setText("build-db-bulk-dropzone-hint", t("bulkImportPickHint"));
  setText("build-db-bulk-cancel-label", t("bulkImportCancel"));
  setText("build-db-bulk-start-label", t("bulkImportStart"));
  setText("build-db-bulk-progress-label", t("bulkImportLabel"));
  setText("build-db-bulk-idle", t("bulkImportIdleNote"));

  setText("sidebar-var-packages-label", t("varPackagesNav"));
  setText("var-packages-input-label", t("inputLabel"));
  setText("var-packages-pick-input-label", t("browse"));
  setText("var-packages-scan-label", t("varPackagesScan"));
  const vpFilter = $("var-packages-filter");
  if (vpFilter) vpFilter.placeholder = t("varPackagesSearchPlaceholder");
  renderVarPackagesFilterBar();
  renderVarPackages();

  setText("settings-appearance-eyebrow", t("settingsAppearanceEyebrow"));
  setText("settings-theme-label", t("settingsThemeLabel"));
  setText("settings-theme-desc", t("settingsThemeDesc"));
  setText("settings-theme-light-label", t("themeButtonLight"));
  setText("settings-theme-dark-label", t("themeButtonDark"));
  setText("settings-theme-nord-label", t("themeButtonNord"));
  setText("settings-theme-solarized-label", t("themeButtonSolarized"));
  setText("settings-theme-monolith-label", t("themeButtonMonolith"));
  setText("settings-theme-amber-label", t("themeButtonAmber"));
  setText("settings-theme-emerald-label", t("themeButtonEmerald"));
  setText("settings-theme-midnight-label", t("themeButtonMidnight"));
  setText("settings-theme-cyber-label", t("themeButtonCyber"));
  setText("settings-theme-porcelain-label", t("themeButtonPorcelain"));
  setText("settings-theme-frost-label", t("themeButtonFrost"));
  setText("settings-theme-circuit-label", t("themeButtonCircuit"));
  setText("settings-theme-backstage-label", t("themeButtonBackstage"));

  setText("settings-scan-eyebrow", t("settingsScanEyebrow"));
  setText("settings-scan-title", t("settingsScanTitle"));
  setText("settings-scan-desc", t("settingsScanDesc"));
  setText("settings-default-replace-label", t("replaceInPlace"));
  setText("settings-default-backup-label", t("backupChanged"));
  setText("settings-default-vap-label", t("processVap"));

  setText("settings-db-eyebrow", t("settingsDbEyebrow"));
  setText("settings-db-title", t("settingsDbTitle"));
  setText("settings-db-desc", t("settingsDbDesc"));
  setText("settings-db-packages-label", t("settingsDbPackages"));
  setText("settings-db-resources-label", t("settingsDbResources"));
  setText("settings-db-size-label", t("settingsDbSize"));
  setText("settings-refresh-label", t("settingsRefresh"));

  setText("settings-about-eyebrow", t("settingsAboutEyebrow"));
  setText("settings-about-desc", t("settingsAboutDesc"));

  // Advanced settings
  setText("settings-advanced-title", t("settingsAdvancedTitle"));
  setText("settings-advanced-desc", t("settingsAdvancedDesc"));
  setText("settings-advanced-eyebrow", t("settingsAdvancedTag"));

  setText("settings-clear-db-title", t("settingsClearDbTitle"));
  setText("settings-clear-db-desc", t("settingsClearDbDesc"));
  setText("settings-clear-db-warning-title", t("settingsClearDbWarningTitle"));
  setText("settings-clear-db-warning-body", t("settingsClearDbWarningBody"));
  setText("settings-clear-db-button-label", t("settingsClearDbButton"));
  renderSettingsClearDb();

  setText("settings-save-label", t("settingsSave"));
  setText("settings-reset-label", t("settingsReset"));

  syncSettingsControlState();
}

function syncSettingsControlState() {
  for (const theme of THEMES) {
    const btn = $(`settings-theme-${theme}`);
    if (btn) btn.classList.toggle("active", state.theme === theme);
  }
  // Settings mirrors the active workspace's run controls. Overview was removed,
  // so source these from the Clean VARs (dbf-*) controls. All reads are
  // null-safe so a missing control never throws.
  const wsReplace = $("dbf-replace-in-place");
  const wsBackup = $("dbf-backup-changed");
  const wsVap = $("dbf-process-vap");
  const replaceCb = $("settings-default-replace");
  const backupCb = $("settings-default-backup");
  const vapCb = $("settings-default-vap");
  if (replaceCb) replaceCb.checked = !!wsReplace?.checked;
  if (backupCb) {
    backupCb.checked = !!wsBackup?.checked;
    backupCb.disabled = !wsReplace?.checked;
  }
  if (vapCb) vapCb.checked = !!wsVap?.checked;

  const settingsInputDir = $("settings-input-dir");
  const settingsOutputDir = $("settings-output-dir");
  const settingsVapDir = $("settings-vap-dir");
  const inputDirEl = $("dbf-input-dir");
  const outputDirEl = $("dbf-output-dir");
  const vapDirEl = $("dbf-vap-dir");
  if (inputDirEl && settingsInputDir && document.activeElement !== settingsInputDir) {
    settingsInputDir.value = inputDirEl.value;
  }
  if (outputDirEl && settingsOutputDir && document.activeElement !== settingsOutputDir) {
    settingsOutputDir.value = outputDirEl.value;
    settingsOutputDir.disabled = outputDirEl.disabled;
  }
  if (vapDirEl && settingsVapDir && document.activeElement !== settingsVapDir) {
    settingsVapDir.value = vapDirEl.value;
    settingsVapDir.disabled = vapDirEl.disabled;
  }
  const vapPickBtn = $("settings-pick-vap");
  if (vapPickBtn) vapPickBtn.disabled = !!$("dbf-pick-vap-button")?.disabled;
  const outputPickBtn = $("settings-pick-output");
  if (outputPickBtn) outputPickBtn.disabled = !!$("dbf-pick-output-button")?.disabled;
  const inputPickBtn = $("settings-pick-input");
  if (inputPickBtn) inputPickBtn.disabled = !!$("dbf-pick-input-button")?.disabled;
  const vapRow = $("settings-path-vap-row");
  if (vapRow) vapRow.classList.toggle("is-disabled", !!vapDirEl?.disabled);
}

async function loadSettingsView() {
  applySettingsCopy();
  syncSettingsControlState();
  const feedback = $("settings-feedback");
  if (feedback) {
    feedback.textContent = "";
    feedback.classList.remove("success", "error");
  }
  if (!invoke) {
    if (feedback) {
      feedback.textContent = `${t("settingsLoadFailed")}Tauri runtime unavailable`;
      feedback.classList.add("error");
    }
    return;
  }
  try {
    const stats = await invoke("get_database_stats");
    $("settings-db-packages-value").textContent = String(stats?.package_count ?? 0);
    $("settings-db-resources-value").textContent = String(stats?.resource_count ?? 0);
    $("settings-db-size-value").textContent = formatBytesLocal(
      stats?.db_size_bytes ?? 0
    );
  } catch (error) {
    if (feedback) {
      feedback.textContent = `${t("settingsLoadFailed")}${String(error)}`;
      feedback.classList.add("error");
    }
  }
}

window.__loadSettingsView = loadSettingsView;
window.__refreshBuildDbStats = () => {
  refreshBuildDbStats();
  refreshDownloadLinksCount();
};

// ============================================================
// Build Database → Download Links importer
// ============================================================

function refreshDownloadLinksCount() {
  if (!invoke) return;
  invoke("get_download_links_count")
    .then((n) => {
      const el = $("dl-links-count");
      if (el) el.textContent = String(n ?? 0);
    })
    .catch(() => {});
}

function setupDownloadLinksImport() {
  const folderInput = $("dl-links-folder");
  const fileInput = $("dl-links-file");
  const status = $("dl-links-status");
  const setStatus = (msg) => { if (status) status.textContent = msg || ""; };

  const folderPick = $("dl-links-folder-pick");
  if (folderPick) {
    folderPick.addEventListener("click", async () => {
      if (!invoke) return;
      try { const f = await invoke("pick_folder"); if (f && folderInput) folderInput.value = f; }
      catch (e) { addLog(String(e)); }
    });
  }
  const filePick = $("dl-links-file-pick");
  if (filePick) {
    filePick.addEventListener("click", async () => {
      if (!invoke) return;
      try { const f = await invoke("pick_manifest_file"); if (f && fileInput) fileInput.value = f; }
      catch (e) { addLog(String(e)); }
    });
  }

  let importing = false;
  const importButtons = ["dl-links-folder-import", "dl-links-file-import", "dl-links-clear"];
  function setImporting(active) {
    importing = active;
    importButtons.forEach((id) => { const b = $(id); if (b) b.disabled = active; });
    const prog = $("dl-links-progress");
    if (prog) prog.classList.toggle("hidden", !active);
  }

  // Polls the import task, painting the card's progress bar. Returns the result
  // payload (or null), throws on a task-reported error.
  async function pollImport(taskId) {
    const fill = $("dl-links-progress-fill");
    const pct = $("dl-links-progress-pct");
    const msg = $("dl-links-progress-msg");
    while (true) {
      await new Promise((r) => setTimeout(r, 200));
      let payload;
      try {
        payload = await invoke("get_task_progress", { taskId });
      } catch (e) {
        throw new Error("lost contact with import task");
      }
      if (!payload) return null;
      const p = Math.round(Math.max(0, Math.min(1, Number(payload.progress ?? 0))) * 100);
      if (fill) fill.style.width = `${p}%`;
      if (pct) pct.textContent = `${p}%`;
      if (msg) msg.textContent = String(payload.message ?? "");
      if (payload.error) throw new Error(String(payload.error));
      if (payload.done) return payload.import_links_result ?? null;
    }
  }

  // Runs the import as a background task so the UI stays responsive.
  async function runImport(folder, file) {
    if (!invoke || importing) return;
    setImporting(true);
    setStatus("Starting…");
    let taskId = null;
    try {
      const handle = await invoke("start_import_download_links_task", {
        sourceFolder: folder || null,
        sourceFile: file || null,
      });
      taskId = handle && handle.id != null ? handle.id : null;
      if (taskId == null) throw new Error("failed to start import");
      const res = await pollImport(taskId);
      if (res) {
        setStatus(
          `Imported ${res.links_added} new link(s) from ${res.files_scanned} file(s). ` +
          `Total in DB: ${res.total_in_db}.`
        );
        refreshDownloadLinksCount();
      }
    } catch (e) {
      setStatus(`Import failed: ${String(e)}`);
      addLog(`Download links import: ${String(e)}`);
    } finally {
      if (taskId != null) { try { await invoke("clear_task", { taskId }); } catch (_e) {} }
      setImporting(false);
    }
  }

  const folderImport = $("dl-links-folder-import");
  if (folderImport) {
    folderImport.addEventListener("click", () => {
      const folder = (folderInput?.value || "").trim();
      if (!folder) { setStatus("Pick a links folder first."); return; }
      runImport(folder, null);
    });
  }
  const fileImport = $("dl-links-file-import");
  if (fileImport) {
    fileImport.addEventListener("click", () => {
      const file = (fileInput?.value || "").trim();
      if (!file) { setStatus("Pick a links file first."); return; }
      runImport(null, file);
    });
  }

  const clearBtn = $("dl-links-clear");
  if (clearBtn) {
    clearBtn.addEventListener("click", async () => {
      if (!invoke) return;
      try {
        await invoke("clear_download_links");
        setStatus("Cleared all imported links.");
        refreshDownloadLinksCount();
      } catch (e) {
        addLog(`Download links clear: ${String(e)}`);
      }
    });
  }

  refreshDownloadLinksCount();
}

// ============================================================
// Settings → Advanced → Clear local database (task-based)
// ============================================================

function scd() {
  return state.settingsClearDb;
}

function stopSettingsClearDbPolling() {
  if (scd().pollTimer) {
    clearInterval(scd().pollTimer);
    scd().pollTimer = null;
  }
}

function renderSettingsClearDb() {
  const button = $("settings-clear-db-button");
  const progressWrap = $("settings-clear-db-progress");
  const progressPct = $("settings-clear-db-progress-pct");
  const progressFill = $("settings-clear-db-progress-fill");
  const progressStatus = $("settings-clear-db-progress-status");
  const progressLabel = $("settings-clear-db-progress-label");

  const taskActive = scd().taskId != null;
  if (button) button.disabled = taskActive;
  if (progressLabel) progressLabel.textContent = "In progress";
  if (progressWrap) progressWrap.classList.toggle("hidden", !taskActive);
  if (taskActive) {
    const pct = Math.round((scd().taskProgress ?? 0) * 100);
    if (progressPct) progressPct.textContent = `${pct}%`;
    if (progressFill) progressFill.style.width = `${pct}%`;
    if (progressStatus) progressStatus.textContent = scd().taskMessage ?? "";
  }
}

async function pollSettingsClearDbTask() {
  if (!invoke) return;
  const id = scd().taskId;
  if (id == null) {
    stopSettingsClearDbPolling();
    return;
  }
  try {
    // Tauri maps Rust `task_id` to `taskId` on the JS side.
    const payload = await invoke("get_task_progress", { taskId: id });
    if (!payload) return;
    scd().taskProgress = Number(payload.progress ?? 0);
    scd().taskMessage = payload.message ?? "";
    renderSettingsClearDb();

    if (payload.done) {
      const feedback = $("settings-clear-db-status");
      stopSettingsClearDbPolling();
      scd().taskId = null;

      if (feedback) {
        feedback.classList.remove("success", "error");
        if (payload.error) {
          feedback.textContent = `${t("settingsClearFailed")}${String(payload.error)}`;
          feedback.classList.add("error");
        } else {
          feedback.textContent = t("settingsCleared");
          feedback.classList.add("success");
        }
      }

      try {
        await invoke("clear_task", { taskId: id });
      } catch (_) {}

      // Refresh dependent state: DB stats card on this page and the Resource
      // List data (now empty).
      await loadSettingsView();
      if (window.__rlRefreshResourceList) window.__rlRefreshResourceList();
      renderSettingsClearDb();
    }
  } catch (error) {
    addLog(String(error));
    stopSettingsClearDbPolling();
    scd().taskId = null;
    renderSettingsClearDb();
  }
}

async function startSettingsClearDb() {
  if (!invoke || scd().taskId != null) return;
  const feedback = $("settings-clear-db-status");
  if (feedback) {
    feedback.textContent = "";
    feedback.classList.remove("success", "error");
  }
  try {
    const handle = await invoke("clear_database");
    const id = Number(handle?.id);
    if (!Number.isFinite(id)) throw new Error("missing task id");
    scd().taskId = id;
    scd().taskProgress = 0;
    scd().taskMessage = t("settingsClearing");
    renderSettingsClearDb();
    stopSettingsClearDbPolling();
    scd().pollTimer = setInterval(pollSettingsClearDbTask, 300);
    pollSettingsClearDbTask();
  } catch (error) {
    if (feedback) {
      feedback.textContent = `${t("settingsClearFailed")}${String(error)}`;
      feedback.classList.add("error");
    }
  }
}

function formatVarModifiedMs(ms) {
  if (ms == null) return "—";
  const date = new Date(Number(ms));
  if (Number.isNaN(date.getTime())) return "—";
  const pad = (n) => String(n).padStart(2, "0");
  const yyyy = date.getFullYear();
  const mm = pad(date.getMonth() + 1);
  const dd = pad(date.getDate());
  const hh = pad(date.getHours());
  const mi = pad(date.getMinutes());
  return `${yyyy}-${mm}-${dd} ${hh}:${mi}`;
}

// ============================================================
// VAR Packages — library view
//
// Laid out after VaM Backstage's Library: a filter panel (left), a card grid
// or table (centre), a details panel (right) and a status bar. Everything
// lists the scanned VAR folders through `list_var_packages`, which also
// returns each package's content type, item count, dependency graph flags and
// the facet counts the filter panel shows. Rows load in chunks as the grid
// scrolls rather than in numbered pages. The database listing that used to be
// this page's second mode lives on the Database page (see dbPkgs*).
// ============================================================

// Rows fetched per request while scrolling. A refresh that must keep what is
// on screen re-fetches up to VP_MAX_RELOAD (list_var_packages caps `limit`).
const VP_CHUNK = 120;
const VP_MAX_RELOAD = 1000;
const LIB_GAP = 12;
const LIB_PAD = 16;

const LIB_TYPES = [
  { key: "scene", label: "Scenes", color: "#3b82f6" },
  { key: "look", label: "Looks", color: "#ec4899" },
  { key: "pose", label: "Poses", color: "#f97316" },
  { key: "clothing", label: "Clothing", color: "#8b5cf6" },
  { key: "hair", label: "Hairstyles", color: "#f59e0b" },
  { key: "other", label: "Other", color: "#64748b" },
];
const LIB_TYPE_BY_KEY = Object.fromEntries(LIB_TYPES.map((type) => [type.key, type]));

// Content categories in the details panel, in display order.
const LIB_CATEGORIES = [
  { key: "scene", label: "Scenes" },
  { key: "subscene", label: "SubScenes" },
  { key: "look", label: "Looks" },
  { key: "pose", label: "Poses" },
  { key: "clothing", label: "Clothing" },
  { key: "hair", label: "Hairstyles" },
];
const LIB_TYPE_HUE = { scene: 220, subscene: 210, look: 330, pose: 25, clothing: 270, hair: 40 };
const LIB_CONTENT_TAGS = {
  legacyScene: { label: "Legacy", color: "#fbbf24" },
  legacyLook: { label: "Legacy", color: "#fbbf24" },
  legacyPose: { label: "Legacy", color: "#fbbf24" },
  clothingPreset: { label: "Preset", color: "#7dd3fc" },
  hairPreset: { label: "Preset", color: "#7dd3fc" },
  skinPreset: { label: "Skin Preset", color: "#7dd3fc" },
};

// `key` is what the backend's `status` filter takes; null is "All".
// `missing` is not a package filter: it swaps the grid for the table of
// dependencies the scanned packages reference but the folders don't have.
const LIB_STATUSES = [
  { key: null, label: "All", title: "Every package in the scanned folders", count: "all" },
  { key: "favorites", label: "Favorites" },
  {
    key: "dependency",
    label: "Dependencies",
    title: "Used by at least one other scanned package",
  },
  {
    key: "standalone",
    label: "Top-level",
    title: "Not used by any other scanned package",
    indent: true,
  },
  {
    key: "broken",
    label: "Broken",
    title: "Have dependencies that are not in the scanned folders",
  },
  {
    key: "missing",
    label: "Missing",
    title: "Dependencies referenced by your packages but not found in the scanned folders",
  },
  {
    key: "outdated",
    label: "Old versions",
    title: "A newer version of the same package is in the scanned folders",
  },
  { key: "indexed", label: "In database", title: "Recorded in the local database index" },
  { key: "unindexed", label: "Not in database", title: "Not yet indexed — run Database → Build" },
];

const LIB_STORE = {
  view: "vp.lib.view",
  cardWidth: "vp.lib.cardWidth",
  detailWidth: "vp.lib.detailWidth",
  hint: "vp.lib.selectHintDismissed",
  category: "vp.lib.cat.",
};

function libStoreGet(key, fallback = null) {
  try {
    const value = window.localStorage.getItem(key);
    return value == null ? fallback : value;
  } catch {
    return fallback;
  }
}

function libStoreSet(key, value) {
  try {
    window.localStorage.setItem(key, String(value));
  } catch {
    /* storage unavailable — the preference just won't persist */
  }
}

// Java-style string hash, as Backstage uses for placeholder colors.
function libHash(text) {
  let h = 0;
  const s = String(text ?? "");
  for (let i = 0; i < s.length; i++) h = (s.charCodeAt(i) + ((h << 5) - h)) | 0;
  return h;
}

function libGradient(id) {
  const h = libHash(id);
  const h1 = Math.abs(h % 360);
  const h2 = Math.abs((h * 7) % 360);
  const h3 = Math.abs((h * 13) % 360);
  return (
    `radial-gradient(ellipse at 25% 75%, hsl(${h1} 45% 22%), transparent 55%), ` +
    `radial-gradient(ellipse at 75% 25%, hsl(${h2} 50% 18%), transparent 50%), ` +
    `linear-gradient(135deg, hsl(${h3} 25% 10%), hsl(${(h3 + 60) % 360} 20% 7%))`
  );
}

function libContentGradient(name, category) {
  const h = libHash(`${name}${category}`);
  const b = LIB_TYPE_HUE[category] ?? Math.abs(h % 360);
  return (
    `radial-gradient(ellipse at 30% 70%, hsl(${b} 40% 24%), transparent 60%), ` +
    `radial-gradient(ellipse at 70% 30%, hsl(${(b + 40) % 360} 35% 16%), transparent 50%), ` +
    `linear-gradient(160deg, hsl(${b} 20% 10%), hsl(${(b + 30) % 360} 15% 6%))`
  );
}

function libAuthorColor(author) {
  return `hsl(${Math.abs(libHash(author) % 360)} 45% 35%)`;
}

function libAuthorInitials(author) {
  const text = String(author ?? "?");
  const parts = text.split(/[-_\s]/).filter(Boolean);
  return parts.length >= 2
    ? (parts[0][0] + parts[1][0]).toUpperCase()
    : text.slice(0, 2).toUpperCase();
}

// "Creator.Package_Name.12" -> "Package Name". The creator is shown on its
// own line and the version as a "v12" suffix, so the title drops both.
function libTitle(item) {
  const id = String(item?.package_id ?? "");
  const parts = id.split(".");
  let core;
  if (parts.length >= 3 && /^\d+$/.test(parts[parts.length - 1])) {
    core = parts.slice(1, -1).join(".");
  } else if (parts.length >= 2) {
    core = parts.slice(1).join(".");
  } else {
    core = id;
  }
  return (core || item?.file_name || id).replaceAll("_", " ");
}

function libVersion(item) {
  const parts = String(item?.package_id ?? "").split(".");
  const last = parts.length >= 3 ? parts[parts.length - 1] : "";
  return /^\d+$/.test(last) ? last : "";
}

function libCreator(item) {
  return item?.creator || deriveCreatorFromPackageId(item?.package_id) || "—";
}

function libType(item) {
  return LIB_TYPE_BY_KEY[item?.pkg_type] ?? LIB_TYPE_BY_KEY.other;
}

// Inactive packages are drawn dimmed, like Backstage's disabled/offloaded ones.
function libIsDim(item) {
  return Boolean(item?.disabled);
}

function libFindItem(filePath) {
  if (!filePath) return null;
  return (
    (state.varPackagesItems ?? []).find((it) => it.file_path === filePath) ??
    state.vpSelected.get(filePath) ??
    null
  );
}

function libIsMissingView() {
  return state.varPackagesFilters?.status === "missing";
}

// ---- Thumbnails ------------------------------------------------------------

// file_path (or `${file_path}::${entry}` for content rows) -> data URL | null.
// Also read by ensureVarDetailsPreview, so VAR Details opens with the image the
// card already showed.
const varPackageThumbCache = new Map();
const VP_THUMB_CACHE_MAX = 900;
const LIB_THUMB = { observer: null, queue: [], active: 0, token: 0 };

function libThumbCacheSet(key, value) {
  if (varPackageThumbCache.size >= VP_THUMB_CACHE_MAX) {
    // Maps iterate in insertion order: drop the oldest entry.
    const oldest = varPackageThumbCache.keys().next().value;
    varPackageThumbCache.delete(oldest);
  }
  varPackageThumbCache.set(key, value);
}

function libThumbPaint(el, url) {
  if (!el || !url || el.querySelector("img")) return;
  const img = document.createElement("img");
  img.alt = "";
  img.decoding = "async";
  img.src = url;
  el.prepend(img);
}

function libThumbObserver() {
  if (LIB_THUMB.observer || typeof IntersectionObserver !== "function") {
    return LIB_THUMB.observer;
  }
  LIB_THUMB.observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        LIB_THUMB.observer.unobserve(entry.target);
        LIB_THUMB.queue.push(entry.target);
      }
      libThumbPump();
    },
    { rootMargin: "400px 0px" },
  );
  return LIB_THUMB.observer;
}

// Lazily fills every [data-thumb] placeholder inside `root` as it nears the
// viewport. Cached images paint synchronously.
function libThumbWatch(root) {
  if (!root) return;
  const observer = libThumbObserver();
  root.querySelectorAll("[data-thumb]").forEach((el) => {
    const key = libThumbKey(el);
    if (varPackageThumbCache.has(key)) {
      libThumbPaint(el, varPackageThumbCache.get(key));
      return;
    }
    if (observer) observer.observe(el);
    else LIB_THUMB.queue.push(el);
  });
  if (!observer) libThumbPump();
}

function libThumbKey(el) {
  const file = el.getAttribute("data-thumb") || "";
  const entry = el.getAttribute("data-thumb-entry") || "";
  return entry ? `${file}::${entry}` : file;
}

// Four at a time: each request opens an archive on a backend worker thread.
async function libThumbPump() {
  if (!invoke) return;
  while (LIB_THUMB.active < 4 && LIB_THUMB.queue.length) {
    const el = LIB_THUMB.queue.shift();
    if (!el.isConnected) continue;
    const key = libThumbKey(el);
    if (varPackageThumbCache.has(key)) {
      libThumbPaint(el, varPackageThumbCache.get(key));
      continue;
    }
    LIB_THUMB.active += 1;
    const filePath = el.getAttribute("data-thumb");
    const entry = el.getAttribute("data-thumb-entry") || null;
    invoke("get_var_image", { filePath, entry })
      .then((url) => {
        libThumbCacheSet(key, url || null);
        // The element may have been re-rendered; paint every live copy.
        document.querySelectorAll("[data-thumb]").forEach((node) => {
          if (libThumbKey(node) === key) libThumbPaint(node, url);
        });
      })
      .catch(() => libThumbCacheSet(key, null))
      .finally(() => {
        LIB_THUMB.active -= 1;
        libThumbPump();
      });
  }
}

function libThumbHtml(filePath, gradientSeed, cls, entry = "") {
  const key = entry ? `${filePath}::${entry}` : filePath;
  const cached = varPackageThumbCache.get(key);
  const img = cached ? `<img alt="" decoding="async" src="${escapeAttribute(cached)}" />` : "";
  return `<div class="${cls}" style="--lib-thumb-bg:${escapeAttribute(gradientSeed)}"
      data-thumb="${escapeAttribute(filePath)}"${
        entry ? ` data-thumb-entry="${escapeAttribute(entry)}"` : ""
      }>${img}`;
}

// ---- Layout: columns, slider, panes ---------------------------------------

function libAvailWidth() {
  const scroll = $("lib-scroll");
  return Math.max(0, (scroll?.clientWidth ?? 0) - LIB_PAD * 2);
}

function libColumnsFor(width) {
  const avail = libAvailWidth();
  return Math.max(1, Math.floor((avail + LIB_GAP) / (width + LIB_GAP)));
}

// Applies the card width as a column count (cards stretch to fill a row, as in
// Backstage) and keeps the size slider's range in step with the pane width.
function libApplyLayout() {
  const grid = $("var-packages-grid");
  const slider = $("lib-size-slider");
  const sliderWrap = $("lib-size-slider-wrap");
  const avail = libAvailWidth();
  if (!grid || avail <= 0) return;
  const width = Math.min(500, Math.max(100, Number(state.vpCardWidth) || 220));
  const cols = libColumnsFor(width);
  grid.style.setProperty("--lib-cols", String(cols));
  if (slider && sliderWrap) {
    const minCols = Math.max(1, Math.ceil((avail + LIB_GAP) / (500 + LIB_GAP)));
    const maxCols = Math.max(minCols, Math.floor((avail + LIB_GAP) / (100 + LIB_GAP)));
    slider.min = String(minCols);
    slider.max = String(maxCols);
    slider.step = "1";
    slider.value = String(Math.min(maxCols, Math.max(minCols, cols)));
    const hide = state.vpView === "table" || libIsMissingView() || maxCols <= minCols;
    sliderWrap.classList.toggle("hidden", hide);
  }
}

function libSetColumns(cols) {
  const avail = libAvailWidth();
  const n = Math.max(1, Number(cols) || 1);
  state.vpCardWidth = Math.floor((avail - (n - 1) * LIB_GAP) / n);
  libStoreSet(LIB_STORE.cardWidth, state.vpCardWidth);
  libApplyLayout();
}

function libSetView(view) {
  if (!["compact", "cards", "table"].includes(view) || state.vpView === view) return;
  state.vpView = view;
  libStoreSet(LIB_STORE.view, view);
  renderVarPackages();
}

function libApplyPaneWidths() {
  const view = $("var-packages-view");
  if (!view) return;
  view.style.setProperty("--lib-detail-w", `${state.vpDetailWidth}px`);
}

// The details panel's left edge drags to resize it (260–500px, remembered).
function setupLibResizeHandles() {
  document.querySelectorAll("[data-lib-resize]").forEach((handle) => {
    handle.addEventListener("mousedown", (event) => {
      event.preventDefault();
      const startX = event.clientX;
      const start = state.vpDetailWidth;
      document.body.classList.add("lib-resizing");
      const onMove = (e) => {
        state.vpDetailWidth = Math.min(500, Math.max(260, start - (e.clientX - startX)));
        libApplyPaneWidths();
        libApplyLayout();
      };
      const onUp = () => {
        document.body.classList.remove("lib-resizing");
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
        libStoreSet(LIB_STORE.detailWidth, state.vpDetailWidth);
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    });
  });
}

// ---- Rendering ---------------------------------------------------------------

function libCardHtml(it, idx) {
  const compact = state.vpView === "compact";
  const fp = it.file_path ?? "";
  const pid = it.package_id ?? "";
  const type = libType(it);
  const title = libTitle(it);
  const version = libVersion(it);
  const creator = libCreator(it);
  const picked = state.vpSelected.has(fp);
  const fav = _favoritePackages.has(pid);

  const chips = [];
  if (!state.varPackagesFilters?.pkgType) {
    chips.push(
      `<span class="lib-chip lib-chip-type" style="background:${type.color}cc">${escapeHtml(type.label)}</span>`,
    );
  }
  if (it.used_by_count > 0) {
    chips.push(
      `<span class="lib-chip lib-chip-dep" title="Used by ${it.used_by_count} scanned package${it.used_by_count === 1 ? "" : "s"}">Dep</span>`,
    );
  }
  if (it.newer_version) {
    chips.push(
      `<span class="lib-chip lib-chip-old" title="A newer version of this package is in the scanned folders">Old</span>`,
    );
  }
  if (compact && it.missing_dep_count > 0) {
    chips.push(
      `<span class="lib-chip lib-chip-warn" title="${it.missing_dep_count} missing dependencies"><span class="material-symbols-outlined">warning</span>${it.missing_dep_count}</span>`,
    );
  }

  const icons = [];
  if (it.disabled) {
    icons.push(
      `<span class="lib-thumb-glyph" title="Disabled — VaM will not load it"><span class="material-symbols-outlined">power_settings_new</span></span>`,
    );
  }
  if (!it.readable) {
    icons.push(
      `<span class="lib-chip lib-chip-error" title="The archive or its meta.json could not be read">Corrupted</span>`,
    );
  }
  icons.push(
    `<button type="button" class="lib-thumb-glyph lib-fav${fav ? " is-active" : ""}" data-vp-fav="${escapeAttribute(pid)}" aria-pressed="${fav}" title="${fav ? "Remove from favorites" : "Add to favorites"}"><span class="material-symbols-outlined">star</span></button>`,
  );

  const author = `<button type="button" class="lib-author-link" data-lib-author="${escapeAttribute(creator)}" title="Filter by ${escapeAttribute(creator)}">${escapeHtml(creator)}</button>`;
  const footer = compact
    ? `<div class="lib-card-scrim">
         <div class="lib-card-title" title="${escapeAttribute(title)}">${escapeHtml(title)}</div>
         <span class="lib-by">by ${author}</span>
       </div>`
    : "";
  const stats = compact
    ? ""
    : `<div class="lib-card-footer">
         <div class="lib-card-row">
           <span class="lib-avatar" style="background:${libAuthorColor(creator)}">${escapeHtml(libAuthorInitials(creator))}</span>
           <div class="lib-card-text">
             <div class="lib-title-line">
               <span class="lib-card-title" title="${escapeAttribute(title)}">${escapeHtml(title)}</span>
               ${version ? `<span class="lib-ver">v${escapeHtml(version)}</span>` : ""}
             </div>
             <span class="lib-by">by ${author}</span>
           </div>
         </div>
         <div class="lib-card-stats">
           <span class="lib-stat"><span class="material-symbols-outlined">hard_drive</span>${escapeHtml(formatBytesLocal(it.size_bytes))}</span>
           <span class="lib-stat lib-stat-items"><span class="material-symbols-outlined">layers</span>${Number(it.item_count) || 0}<span class="lib-stat-word"> items</span></span>
           ${
             it.missing_dep_count > 0
               ? `<span class="lib-stat lib-stat-warn" title="${it.missing_dep_count} dependencies are not in the scanned folders"><span class="material-symbols-outlined">warning</span>${it.missing_dep_count} missing</span>`
               : ""
           }
         </div>
       </div>`;

  return `
    <div class="lib-card${picked ? " is-picked" : ""}${picked && state.vpSelected.size > 1 ? " is-checked" : ""}${libIsDim(it) ? " is-dim" : ""}${state.vpSelected.size > 1 && state.vpLead === fp ? " is-lead" : ""}"
         role="option" tabindex="-1" aria-selected="${picked}"
         data-file-path="${escapeAttribute(fp)}" data-package-id="${escapeAttribute(pid)}" data-idx="${idx}">
      ${libThumbHtml(fp, libGradient(it.file_name || pid), "lib-thumb")}
        <div class="lib-thumb-shade"></div>
        <div class="lib-thumb-chips">${chips.join("")}</div>
        <div class="lib-thumb-icons">${icons.join("")}</div>
        <button type="button" class="lib-check" data-vp-select="${escapeAttribute(fp)}" aria-label="Select ${escapeAttribute(title)}"><span class="material-symbols-outlined">check</span></button>
        ${footer}
      </div>
      ${stats}
    </div>`;
}

function libStatusCellHtml(it) {
  const parts = [];
  if (!it.readable) parts.push(`<span class="lib-status-err">Corrupted</span>`);
  else if (it.disabled) parts.push(`<span class="lib-status-warn">Disabled</span>`);
  else if (it.used_by_count > 0) parts.push(`<span class="lib-status-dep">Dep</span>`);
  else parts.push(`<span class="lib-status-ok">Top-level</span>`);
  if (it.newer_version) parts.push(`<span class="lib-status-warn">Old</span>`);
  if (!it.indexed) parts.push(`<span class="lib-status-muted">Not in DB</span>`);
  return `<span class="lib-status-text">${parts.join(" · ")}</span>`;
}

function libRowHtml(it, idx) {
  const fp = it.file_path ?? "";
  const pid = it.package_id ?? "";
  const type = libType(it);
  const title = libTitle(it);
  const version = libVersion(it);
  const creator = libCreator(it);
  const picked = state.vpSelected.has(fp);
  const fav = _favoritePackages.has(pid);
  return `
    <tr class="${picked ? "is-picked" : ""}${libIsDim(it) ? " is-dim" : ""}"
        data-file-path="${escapeAttribute(fp)}" data-package-id="${escapeAttribute(pid)}" data-idx="${idx}">
      <td class="lib-check-cell">
        <input type="checkbox" data-vp-select="${escapeAttribute(fp)}" ${picked ? "checked" : ""}
               aria-label="Select ${escapeAttribute(title)}" />
      </td>
      <td>
        <div class="lib-pkg-cell">
          ${libThumbHtml(fp, libGradient(it.file_name || pid), "lib-pkg-thumb")}</div>
          <div class="lib-pkg-text">
            <span class="lib-pkg-name" title="${escapeAttribute(title)}">${escapeHtml(title)}
              ${version ? `<span class="lib-ver">v${escapeHtml(version)}</span>` : ""}</span>
            <span class="lib-pkg-file" title="${escapeAttribute(fp)}">${escapeHtml(it.file_name ?? "")}</span>
          </div>
        </div>
      </td>
      <td><button type="button" class="lib-author-link" data-lib-author="${escapeAttribute(creator)}">${escapeHtml(creator)}</button></td>
      <td><span class="lib-type-chip" style="--dot:${type.color}">${escapeHtml(type.label)}</span></td>
      <td>${libStatusCellHtml(it)}</td>
      <td class="lib-mono">${escapeHtml(formatBytesLocal(it.size_bytes))}</td>
      <td>${fav ? `<span class="material-symbols-outlined" style="color:#fbbf24;font-variation-settings:'FILL' 1">star</span> ` : ""}${Number(it.item_count) || 0}</td>
      <td>${
        it.missing_dep_count > 0
          ? `<span class="lib-status-warn" title="${it.missing_dep_count} missing">⚠ ${it.missing_dep_count}</span>`
          : `<span class="lib-status-muted">${Number(it.dep_count) || 0}</span>`
      }</td>
    </tr>`;
}

function libEmptyHtml() {
  if (state.varPackagesLoading) {
    return `<div class="lib-empty">${escapeHtml(t("varPackagesScanning"))}</div>`;
  }
  const total = Math.max(0, Number(state.varPackagesTotal ?? 0));
  const hasQuery =
    String(state.varPackagesFilter ?? "").trim() ||
    activeVarPackageFilterCount(state.varPackagesFilters) > 0 ||
    state.varPackagesFilters?.status;
  if (total === 0 && !hasQuery && !state.vpHasListing) {
    const dir = vpResolveInputDir();
    if (!dir) {
      return `<div class="lib-empty">Set your VaM directory to list the packages in AddonPackages.
        <span class="lib-empty-sub"><button type="button" class="lib-btn lib-btn-gradient lib-btn-sm" data-vam-settings>
          <span class="material-symbols-outlined">settings</span>Open Settings</button></span></div>`;
    }
    return `<div class="lib-empty">${escapeHtml(t("varPackagesEmpty"))}
      <span class="lib-empty-sub"><button type="button" class="lib-btn lib-btn-gradient lib-btn-sm" data-lib-action="scan">
        <span class="material-symbols-outlined">sync</span>Scan ${escapeHtml(dir)}</button></span></div>`;
  }
  return `<div class="lib-empty">No items found<span class="lib-empty-sub">${escapeHtml(
    t("varPackagesNoResults"),
  )}</span></div>`;
}

function renderVarPackages() {
  const grid = $("var-packages-grid");
  const tableWrap = $("lib-table-wrap");
  const tbody = $("var-packages-tbody");
  const missingWrap = $("lib-missing-wrap");
  if (!grid || !tbody) return;

  $("var-packages-progress")?.classList.toggle(
    "hidden",
    !(state.varPackagesLoading || state.varPackagesLoadingMore || state.vpMissing?.loading),
  );
  document.querySelectorAll("[data-lib-view]").forEach((btn) => {
    btn.classList.toggle("active", btn.getAttribute("data-lib-view") === state.vpView);
  });

  const missingView = libIsMissingView();
  const table = state.vpView === "table";
  const items = state.varPackagesItems ?? [];

  grid.classList.toggle("hidden", missingView || table);
  tableWrap?.classList.toggle("hidden", missingView || !table);
  missingWrap?.classList.toggle("hidden", !missingView);
  $("lib-detail")?.classList.toggle("hidden", missingView);
  document
    .querySelector("[data-lib-resize='detail']")
    ?.classList.toggle("hidden", missingView);

  libRenderToolbar();
  libRenderStatusBar();

  if (missingView) {
    libRenderMissing();
    libRenderLoadMore();
    libApplyLayout();
    return;
  }

  if (items.length === 0) {
    if (table) {
      tbody.innerHTML = `<tr><td colspan="8">${libEmptyHtml()}</td></tr>`;
    } else {
      grid.innerHTML = libEmptyHtml();
    }
  } else if (table) {
    tbody.innerHTML = items.map((it, i) => libRowHtml(it, i)).join("");
    libThumbWatch(tbody);
  } else {
    grid.classList.toggle("is-bulk", state.vpSelected.size > 1);
    grid.innerHTML = items.map((it, i) => libCardHtml(it, i)).join("");
    libThumbWatch(grid);
  }
  libRenderLoadMore();
  libApplyLayout();
  renderVpSelectionBar();
  libRenderDetail();
}

function libRenderLoadMore() {
  const el = $("lib-load-more");
  if (!el) return;
  const loaded = (state.varPackagesItems ?? []).length;
  const total = Math.max(0, Number(state.varPackagesTotal ?? 0));
  const show = !libIsMissingView() && loaded > 0 && (loaded < total || state.varPackagesLoadingMore);
  el.classList.toggle("hidden", !show);
  el.textContent = state.varPackagesLoadingMore
    ? "Loading more…"
    : `Showing ${loaded.toLocaleString()} of ${total.toLocaleString()} — scroll for more`;
}

function libRenderToolbar() {
  const count = $("var-packages-count");
  const total = Math.max(0, Number(state.varPackagesTotal ?? 0));
  if (count) {
    if (libIsMissingView()) {
      const n = state.vpMissing?.items?.length ?? 0;
      count.textContent = state.vpMissing?.loading
        ? "… missing dependencies"
        : `${n.toLocaleString()} missing ${n === 1 ? "dependency" : "dependencies"}`;
    } else {
      count.textContent = state.varPackagesLoading && total === 0 ? "… packages" : t("varPackagesCount", total);
    }
  }

  const nFilters = activeVarPackageFilterCount(state.varPackagesFilters);
  $("lib-filter-summary")?.classList.toggle("hidden", nFilters === 0);
  vpSetText("lib-filter-summary-text", `${nFilters} filter${nFilters === 1 ? "" : "s"}`);

  const hintDismissed = libStoreGet(LIB_STORE.hint) === "1";
  $("lib-select-hint")?.classList.toggle(
    "hidden",
    hintDismissed || libIsMissingView() || (state.varPackagesItems ?? []).length < 2,
  );
  // The Missing table has no cards, so no size or view mode either.
  document.querySelector(".lib-view-toggle")?.classList.toggle("hidden", libIsMissingView());

  // Contextual actions for the selected status, as in Backstage's toolbar.
  const actions = $("lib-toolbar-actions");
  if (!actions) return;
  const status = state.varPackagesFilters?.status ?? null;
  if (status === "broken") {
    actions.innerHTML = `<button type="button" class="lib-btn lib-btn-xs lib-btn-outline" data-lib-action="view-missing">View Missing Packages</button>`;
  } else if (status === "missing") {
    const want = (state.vpMissing?.items ?? []).filter((m) => m.status === "missing").length;
    actions.innerHTML = `
      <button type="button" class="lib-btn lib-btn-xs lib-btn-gradient" data-lib-action="find-missing" ${want ? "" : "disabled"}>
        <span class="material-symbols-outlined">download</span>Find &amp; Download All (${want})
      </button>
      <button type="button" class="lib-icon-btn lib-icon-btn-sm" data-lib-action="refresh-missing" title="Refresh">
        <span class="material-symbols-outlined">refresh</span>
      </button>`;
  } else if (status === "outdated" && total > 0) {
    actions.innerHTML = `<button type="button" class="lib-btn lib-btn-xs lib-btn-destructive" data-lib-action="clean-old">
        <span class="material-symbols-outlined">delete_sweep</span>Clean Old Versions…</button>`;
  } else {
    actions.innerHTML = "";
  }
}

function libRenderStatusBar() {
  const bar = $("lib-statusbar");
  if (!bar) return;
  const f = state.varPackagesFacets;
  if (!f) {
    bar.innerHTML = `<span class="lib-sb-item">${escapeHtml(
      vpResolveInputDir() ? "Not scanned yet" : "VaM directory not set",
    )}</span>`;
    return;
  }
  const total = Math.max(0, Number(state.varPackagesTotal ?? 0));
  const sep = `<span class="lib-sb-sep">·</span>`;
  const dir = vpResolveInputDir();
  const extra = getAdditionalDirs("varPackages").length;
  bar.innerHTML = `
    <span class="lib-sb-item" title="Packages matching the current filters"><span class="material-symbols-outlined">inventory_2</span>${total.toLocaleString()} packages</span>${sep}
    <span class="lib-sb-item" title="Of those, packages another scanned package depends on"><span class="material-symbols-outlined">account_tree</span>${Number(f.total_deps || 0).toLocaleString()} deps</span>${sep}
    <span class="lib-sb-item" title="Content items (scenes, looks, poses, clothing, hair)"><span class="material-symbols-outlined">layers</span>${Number(f.total_items || 0).toLocaleString()} items</span>${sep}
    <span class="lib-sb-item" title="Total size"><span class="material-symbols-outlined">hard_drive</span>${escapeHtml(formatBytesLocal(f.total_bytes || 0))}</span>
    <span class="lib-sb-right" title="${escapeAttribute(dir)}">${escapeHtml(dir)}${
      extra ? ` +${extra} folder${extra === 1 ? "" : "s"}` : ""
    } · ${Number(f.library_count || 0).toLocaleString()} packages · ${escapeHtml(
      formatBytesLocal(f.library_bytes || 0),
    )}</span>`;
}

// ---- Missing dependencies view -------------------------------------------------

async function libLoadMissing() {
  if (!invoke) return;
  state.vpMissing = { ...(state.vpMissing ?? {}), loading: true };
  renderVarPackages();
  try {
    const rows = await invoke("list_missing_dependencies");
    state.vpMissing = { loading: false, items: Array.isArray(rows) ? rows : [], error: null };
  } catch (err) {
    state.vpMissing = { loading: false, items: [], error: String(err) };
    addLog(`Missing dependencies: ${String(err)}`);
  }
  renderVarPackages();
}

function libDepVersionLabel(id) {
  const last = String(id).split(".").pop() ?? "";
  if (/^latest$/i.test(last)) return "any";
  const min = /^min(\d+)$/i.exec(last);
  if (min) return `v${min[1]}+`;
  return /^\d+$/.test(last) ? `v${last}` : "any";
}

function libRenderMissing() {
  const wrap = $("lib-missing-wrap");
  if (!wrap) return;
  const m = state.vpMissing ?? {};
  if (m.loading && !(m.items ?? []).length) {
    wrap.innerHTML = `<div class="lib-empty">Resolving dependencies…</div>`;
    return;
  }
  const rows = m.items ?? [];
  if (!rows.length) {
    wrap.innerHTML = `<div class="lib-empty">Nothing missing<span class="lib-empty-sub">Every dependency the scanned packages declare is in the scanned folders.</span></div>`;
    return;
  }
  wrap.innerHTML = `
    <table class="lib-table lib-missing-table">
      <thead><tr>
        <th style="width:34%">Package</th><th style="width:16%">Version</th>
        <th style="width:16%">Author</th><th>Needed by</th><th style="width:96px">Status</th>
      </tr></thead>
      <tbody>${rows
        .map((row) => {
          const id = String(row.id ?? "");
          const base = id.split(".").slice(0, -1).join(".") || id;
          const have = row.have_id ? ` — have v${escapeHtml(String(row.have_id).split(".").pop())}` : "";
          const users = (row.needed_by ?? [])
            .slice(0, 3)
            .map(
              (u) =>
                `<button type="button" class="lib-link" data-lib-reveal="${escapeAttribute(u.file_path)}">${escapeHtml(libTitle(u))}</button>`,
            )
            .join("");
          const more = (row.needed_by?.length ?? 0) > 3 ? ` <span class="lib-status-muted">+${row.needed_by.length - 3}</span>` : "";
          const pill =
            row.status === "other_version"
              ? `<span class="lib-pill lib-pill-warn" title="Another version is present; VaM may fall back to it">Fallback</span>`
              : row.indexed
                ? `<span class="lib-pill lib-pill-info" title="Not in the scanned folders, but the database index knows this package">In database</span>`
                : `<span class="lib-pill lib-pill-err">Missing</span>`;
          return `<tr>
            <td title="${escapeAttribute(id)}"><span class="lib-pkg-name">${escapeHtml(base)}</span></td>
            <td class="lib-mono">${escapeHtml(libDepVersionLabel(id))}${have}</td>
            <td>${escapeHtml(deriveCreatorFromPackageId(id) ?? "—")}</td>
            <td>${users}${more}</td>
            <td>${pill}</td>
          </tr>`;
        })
        .join("")}</tbody>
    </table>`;
}

// Hands the missing package ids to the Find Dependencies dialog, which looks
// them up on the Hub and in the imported download links.
function libFindMissing(ids) {
  const list = (ids ?? []).filter(Boolean);
  if (!list.length) return;
  depStartTextScan();
  const input = $("dep-scan-text-input");
  if (!input) return;
  input.value = list.join("\n");
  depAnalyzeText().catch((e) => addLog(`Find Dependencies: ${String(e)}`));
}

// Shows one package in the grid: leaves the Missing view and searches for it.
function libRevealPackage(filePath, packageId) {
  const loaded = (state.varPackagesItems ?? []).find((it) => it.file_path === filePath);
  if (loaded && !libIsMissingView()) {
    vpSelectOnly(loaded);
    libScrollIntoView(filePath);
    return;
  }
  const id = packageId || String(filePath).split(/[\\/]/).pop()?.replace(/\.var$/i, "") || "";
  state.vpRevealPath = filePath;
  state.varPackagesFilters = { ...state.varPackagesFilters, status: null };
  state.varPackagesFilter = id;
  const input = $("var-packages-filter");
  if (input) input.value = id;
  renderVarPackagesFilterBar();
  refreshVarPackagesFromFolder({ forceRescan: false });
}

function libScrollIntoView(filePath) {
  const node = document.querySelector(
    `#lib-scroll [data-file-path="${CSS.escape(String(filePath))}"]`,
  );
  node?.scrollIntoView({ block: "nearest" });
}

// ---- Details panel -----------------------------------------------------------

const LIB_DETAILS = { cache: new Map(), pending: new Map(), expanded: new Set() };

function libDetailsKey(item) {
  return `${item.file_path}|${item.size_bytes}|${item.modified_ms}`;
}

function libLoadDetails(item) {
  const key = libDetailsKey(item);
  if (LIB_DETAILS.cache.has(key)) return Promise.resolve(LIB_DETAILS.cache.get(key));
  if (LIB_DETAILS.pending.has(key)) return LIB_DETAILS.pending.get(key);
  const p = invoke("get_var_package_details", { filePath: item.file_path })
    .then((details) => {
      if (LIB_DETAILS.cache.size > 60) LIB_DETAILS.cache.delete(LIB_DETAILS.cache.keys().next().value);
      LIB_DETAILS.cache.set(key, details);
      return details;
    })
    .catch((err) => {
      const failed = { error: String(err) };
      LIB_DETAILS.cache.set(key, failed);
      return failed;
    })
    .finally(() => LIB_DETAILS.pending.delete(key));
  LIB_DETAILS.pending.set(key, p);
  return p;
}

function libLicenseHtml(license) {
  if (!license) return "";
  const text = String(license);
  const upper = text.toUpperCase();
  let cls = "";
  let title = "License";
  if (/\bNC\b/.test(upper) || ["PC", "PC EA", "QUESTIONABLE"].includes(upper)) {
    cls = " is-restricted";
    title = "Commercial use not allowed";
  } else if (["CC BY", "CC BY-SA", "CC BY-ND", "PD", "PUBLIC DOMAIN"].includes(upper)) {
    cls = " is-commercial";
    title = "Commercial use allowed";
  }
  return `<span class="lib-chip lib-license${cls}" title="${escapeAttribute(title)}">${escapeHtml(text)}</span>`;
}

function libDepRank(status) {
  return { missing: 95, indexed: 80, other_version: 72, found: 0 }[status] ?? 50;
}

function libDepPill(dep) {
  switch (dep.status) {
    case "found":
      return `<span class="lib-pill lib-pill-ok">Present</span>`;
    case "other_version":
      return `<span class="lib-pill lib-pill-warn" title="Only ${escapeAttribute(dep.resolved_id ?? "another version")} is present">Fallback</span>`;
    case "indexed":
      return `<span class="lib-pill lib-pill-info" title="Not in the scanned folders; the database index knows ${escapeAttribute(dep.resolved_id ?? "")}">In database</span>`;
    default:
      return `<span class="lib-pill lib-pill-err">Missing</span>`;
  }
}

function libCollapsible(rows, key, total) {
  const expanded = LIB_DETAILS.expanded.has(key);
  if (rows.length <= 4 || expanded) {
    const less =
      rows.length > 4
        ? `<button type="button" class="lib-more" data-lib-expand="${escapeAttribute(key)}">Show less</button>`
        : "";
    return rows.join("") + less;
  }
  return (
    rows.slice(0, 3).join("") +
    `<button type="button" class="lib-more" data-lib-expand="${escapeAttribute(key)}">+ ${total - 3} more</button>`
  );
}

function libDepsSectionHtml(item, details) {
  const deps = [...(details.dependencies ?? [])].sort(
    (a, b) => libDepRank(b.status) - libDepRank(a.status) || a.id.localeCompare(b.id),
  );
  const missing = deps.filter((d) => d.status === "missing");
  const issue = missing.length
    ? `<button type="button" class="lib-issue lib-small-link" data-lib-action="find-deps" title="${missing.length} dependencies are not in the scanned folders — look them up on the Hub">
         <span class="material-symbols-outlined">warning</span>${missing.length} missing</button>`
    : "";
  const fallback = deps.filter((d) => d.status === "other_version").length;
  const fallbackChip = fallback
    ? `<span class="lib-issue" title="Only another version of these is present"><span class="material-symbols-outlined">swap_horiz</span>${fallback} fallback</span>`
    : "";
  const rows = deps.map((dep) => {
    const resolved = dep.status === "found" || dep.status === "other_version";
    const ref = resolved
      ? `<button type="button" class="lib-dep-ref is-resolved" data-lib-reveal="${escapeAttribute(dep.file_path ?? "")}" data-lib-reveal-id="${escapeAttribute(dep.resolved_id ?? "")}" title="${escapeAttribute(dep.id)}">${escapeHtml(dep.id)}</button>`
      : `<span class="lib-dep-ref" title="${escapeAttribute(dep.id)}">${escapeHtml(dep.id)}</span>`;
    const size = dep.size_bytes != null ? `<span class="lib-dep-size">${escapeHtml(formatBytesLocal(dep.size_bytes))}</span>` : "";
    return `<div class="lib-dep-row">${ref}${size}${libDepPill(dep)}</div>`;
  });
  const body = rows.length
    ? `<div class="lib-box">${libCollapsible(rows, `deps:${item.file_path}`, rows.length)}</div>`
    : `<p class="lib-aside">Declares no dependencies.</p>`;
  return `
    <section class="lib-ds">
      <div class="lib-group-head">
        <span class="lib-group-title">Dependencies <small>(${deps.length})</small></span>
        <span class="lib-group-tools">${fallbackChip}${issue}</span>
      </div>
      ${body}
    </section>`;
}

function libUsedBySectionHtml(item, details) {
  const users = details.used_by ?? [];
  if (!users.length) return "";
  const rows = users.map(
    (u) => `<button type="button" class="lib-user-row" data-lib-reveal="${escapeAttribute(u.file_path)}" data-lib-reveal-id="${escapeAttribute(u.package_id)}">
        <span class="lib-user-name">${escapeHtml(libTitle(u))}</span>
        <span class="lib-user-by">by ${escapeHtml(deriveCreatorFromPackageId(u.package_id) ?? "—")}</span>
      </button>`,
  );
  return `
    <section class="lib-ds">
      <div class="lib-group-head"><span class="lib-group-title">Used by <small>(${users.length})</small></span></div>
      <div class="lib-box">${libCollapsible(rows, `users:${item.file_path}`, rows.length)}</div>
    </section>`;
}

function libContentSectionHtml(item, details) {
  const content = details.content ?? [];
  const groups = LIB_CATEGORIES.map((cat) => ({
    ...cat,
    rows: content.filter((c) => c.category === cat.key),
  })).filter((g) => g.rows.length);
  const count = Number(details.item_count) || content.length;
  const head = `
    <div class="lib-group-head">
      <span class="lib-group-title">Content ${count ? `<small>(${count})</small>` : "<small>(none detected)</small>"}</span>
      <span class="lib-group-tools">
        <button type="button" class="lib-small-link is-quiet" data-lib-action="browse-files" title="Open in VAR Details"><span class="material-symbols-outlined">account_tree</span>Browse files</button>
        <button type="button" class="lib-small-link" data-lib-action="images" title="Every image in the package"><span class="material-symbols-outlined">grid_view</span>View images</button>
      </span>
    </div>`;
  const body = groups
    .map((g) => {
      const collapsed = libStoreGet(LIB_STORE.category + g.key) === "0";
      const limitKey = `cat:${item.file_path}:${g.key}`;
      const showAll = LIB_DETAILS.expanded.has(limitKey);
      const visible = showAll ? g.rows : g.rows.slice(0, 60);
      const rows = visible
        .map((c) => {
          const tag = LIB_CONTENT_TAGS[c.fine];
          const gradient = libContentGradient(c.name, c.category);
          // Only rows with their own image get a loader — without an entry the
          // key would be the package's, and every row would show the package art.
          const thumb = c.thumb
            ? `${libThumbHtml(item.file_path, gradient, "lib-content-thumb", c.thumb)}</div>`
            : `<div class="lib-content-thumb" style="--lib-thumb-bg:${escapeAttribute(gradient)}"></div>`;
          return `<div class="lib-content-row" title="${escapeAttribute(c.path)}">
            ${thumb}
            <span class="lib-content-name">${escapeHtml(c.name)}${
              tag ? `<span class="lib-content-tag" style="color:${tag.color}bb">${escapeHtml(tag.label)}</span>` : ""
            }</span>
          </div>`;
        })
        .join("");
      const more =
        g.rows.length > visible.length
          ? `<button type="button" class="lib-more" data-lib-expand="${escapeAttribute(limitKey)}">+ ${g.rows.length - visible.length} more</button>`
          : "";
      return `<div class="lib-cat">
          <button type="button" class="lib-cat-head" data-lib-cat="${g.key}">
            <span class="material-symbols-outlined">${collapsed ? "chevron_right" : "expand_more"}</span>
            ${escapeHtml(g.label)} <small>(${g.rows.length})</small>
          </button>
          ${collapsed ? "" : `<div class="lib-box">${rows}${more}</div>`}
        </div>`;
    })
    .join("");
  const truncated = details.content_truncated
    ? `<p class="lib-aside" style="margin-top:6px">Showing the first ${content.length.toLocaleString()} items.</p>`
    : "";
  return `<section class="lib-ds">${head}${body}${truncated}</section>`;
}

function libDetailHeaderHtml(item, details) {
  const type = LIB_TYPE_BY_KEY[details?.pkg_type || item.pkg_type] ?? libType(item);
  const title = details?.title ? String(details.title).replaceAll("_", " ") : libTitle(item);
  const version = libVersion(item);
  const creator = libCreator(item);
  const fav = _favoritePackages.has(item.package_id);
  const chips = [
    `<span class="lib-chip lib-chip-type" style="background:${type.color}cc">${escapeHtml(type.label)}</span>`,
  ];
  if (item.used_by_count > 0) chips.push(`<span class="lib-chip lib-chip-depchip">Dep</span>`);
  if (item.newer_version) chips.push(`<span class="lib-chip lib-chip-storage">Old</span>`);
  if (item.disabled) {
    chips.push(
      `<span class="lib-chip lib-chip-storage"><span class="material-symbols-outlined">power_settings_new</span>Disabled</span>`,
    );
  }
  if (!item.readable && !(details && details.readable)) chips.push(`<span class="lib-chip lib-chip-error">Corrupted</span>`);
  if (!item.indexed) chips.push(`<span class="lib-chip lib-chip-muted" title="Not recorded in the database index">Not in DB</span>`);
  chips.push(libLicenseHtml(details?.license ?? item.license));
  if (details?.morph_count > 0) {
    chips.push(
      `<span class="lib-chip lib-chip-plain"><span class="material-symbols-outlined">blur_on</span>${details.morph_count} morph${details.morph_count === 1 ? "" : "s"}</span>`,
    );
  }
  const support = details?.promotional_link
    ? `<button type="button" class="lib-link lib-support" data-lib-url="${escapeAttribute(details.promotional_link)}"><span class="material-symbols-outlined">favorite</span>Support</button>`
    : "";

  const users = details?.used_by ?? [];
  const usedLine = item.used_by_count > 0
    ? `Used by ${users
        .slice(0, 2)
        .map((u) => escapeHtml(libTitle(u)))
        .join(", ")}${item.used_by_count > 2 ? ` +${item.used_by_count - 2}` : ""}. Deleting it breaks ${item.used_by_count === 1 ? "that package" : "those packages"}.`
    : `Frees ${escapeHtml(formatBytesLocal(item.size_bytes))}.`;

  return `
    <section class="lib-ds">
      <div class="lib-dh">
        <button type="button" class="lib-dh-thumb" data-lib-action="images" title="View images"
                style="--lib-thumb-bg:${escapeAttribute(libGradient(item.file_name || item.package_id))}"
                data-thumb="${escapeAttribute(item.file_path)}">${
                  varPackageThumbCache.get(item.file_path)
                    ? `<img alt="" src="${escapeAttribute(varPackageThumbCache.get(item.file_path))}" />`
                    : ""
                }</button>
        <div class="lib-dh-text">
          <div class="lib-title-line">
            <span class="lib-dh-title" title="${escapeAttribute(title)}">${escapeHtml(title)}</span>
            ${version ? `<span class="lib-ver">v${escapeHtml(version)}</span>` : ""}
          </div>
          <div class="lib-dh-by">
            <span class="lib-avatar is-sm" style="background:${libAuthorColor(creator)}">${escapeHtml(libAuthorInitials(creator))}</span>
            <span class="lib-by">by <button type="button" class="lib-author-link" data-lib-author="${escapeAttribute(creator)}">${escapeHtml(creator)}</button></span>
            ${support}
          </div>
          <div class="lib-dh-chips">${chips.join("")}</div>
        </div>
      </div>
      <div class="lib-actions">
        <button type="button" class="lib-btn lib-btn-accent lib-btn-full" data-lib-action="open-details">
          <span class="material-symbols-outlined">open_in_new</span>Open in VAR Details
        </button>
        <div class="lib-actions-row">
          <button type="button" class="lib-btn lib-btn-destructive" data-lib-action="delete">
            <span class="material-symbols-outlined">delete</span>Delete · ${escapeHtml(formatBytesLocal(item.size_bytes))}
          </button>
          <button type="button" class="lib-btn lib-btn-quiet${item.disabled ? " is-warn" : ""}" data-lib-action="toggle-disabled"
                  title="${item.disabled ? "Remove the .disabled marker so VaM loads it again" : "Add a .disabled marker so VaM skips it"}">
            <span class="material-symbols-outlined">${item.disabled ? "power" : "power_settings_new"}</span>${item.disabled ? "Enable" : "Disable"}
          </button>
          <button type="button" class="lib-icon-btn${fav ? " lib-btn-quiet is-on" : ""}" data-vp-fav="${escapeAttribute(item.package_id)}" title="${fav ? "Remove from favorites" : "Add to favorites"}">
            <span class="material-symbols-outlined" ${fav ? `style="font-variation-settings:'FILL' 1"` : ""}>star</span>
          </button>
          <button type="button" class="lib-icon-btn" data-lib-action="explorer" title="Show in Explorer">
            <span class="material-symbols-outlined">folder_open</span>
          </button>
        </div>
        <p class="lib-aside">${usedLine}</p>
        <p class="lib-path" title="${escapeAttribute(item.file_path)}">${escapeHtml(item.file_path)}</p>
      </div>
    </section>`;
}

function libRenderDetail() {
  const host = $("lib-detail");
  if (!host || libIsMissingView()) return;
  const n = state.vpSelected.size;
  if (n > 1) {
    host.innerHTML = libSelectionPanelHtml();
    return;
  }
  const fp = n === 1 ? state.vpSelected.keys().next().value : null;
  const item = libFindItem(fp);
  if (!item) {
    host.innerHTML = `<div class="lib-detail-empty"><span class="material-symbols-outlined">touch_app</span>Select a package to see its details</div>`;
    return;
  }
  const key = libDetailsKey(item);
  const details = LIB_DETAILS.cache.get(key);
  if (!details) {
    host.innerHTML =
      libDetailHeaderHtml(item, null) +
      `<section class="lib-ds"><div class="lib-skeleton" style="width:60%"></div>
         <div class="lib-skeleton" style="width:90%;margin-top:8px"></div>
         <div class="lib-skeleton" style="width:75%;margin-top:8px"></div></section>`;
    libThumbWatch(host);
    if (invoke && item.file_path) {
      libLoadDetails(item).then(() => {
        const current = state.vpSelected.size === 1 ? state.vpSelected.keys().next().value : null;
        if (current === item.file_path) libRenderDetail();
      });
    }
    return;
  }
  if (details.error) {
    host.innerHTML =
      libDetailHeaderHtml(item, null) +
      `<section class="lib-ds"><p class="lib-desc">Could not read this package: ${escapeHtml(details.error)}</p></section>`;
    libThumbWatch(host);
    return;
  }
  const desc = details.description
    ? `<section class="lib-ds lib-ds-tight"><p class="lib-desc">${escapeHtml(
        details.description.length > 300 ? `${details.description.slice(0, 300)}…` : details.description,
      )}</p></section>`
    : "";
  host.innerHTML =
    libDetailHeaderHtml(item, details) +
    desc +
    libDepsSectionHtml(item, details) +
    libUsedBySectionHtml(item, details) +
    libContentSectionHtml(item, details);
  libThumbWatch(host);
}

function libSelectionPanelHtml() {
  const snaps = [...state.vpSelected.values()];
  const bytes = snaps.reduce((sum, s) => sum + (Number(s.size_bytes) || 0), 0);
  const disabled = snaps.filter((s) => s.disabled).length;
  const broken = snaps.filter((s) => s.missing_dep_count > 0).length;
  const rows = snaps
    .slice(0, 200)
    .map(
      (s) => `<div class="lib-sel-row">
        <span class="lib-type-chip" style="--dot:${libType(s).color}">${escapeHtml(libType(s).label)}</span>
        <span class="lib-content-name">${escapeHtml(libTitle(s))}</span>
        <span class="lib-dep-size">${escapeHtml(formatBytesLocal(s.size_bytes))}</span>
      </div>`,
    )
    .join("");
  return `
    <section class="lib-ds">
      <div class="lib-dh-title">${snaps.length.toLocaleString()} packages selected</div>
      <p class="lib-aside" style="margin-top:4px">${escapeHtml(formatBytesLocal(bytes))}${
        disabled ? ` · ${disabled} disabled` : ""
      }${broken ? ` · ${broken} with missing dependencies` : ""}</p>
      <div class="lib-actions">
        <div class="lib-actions-row">
          <button type="button" class="lib-btn lib-btn-destructive" data-lib-action="bulk-delete">
            <span class="material-symbols-outlined">delete</span>Delete · ${escapeHtml(formatBytesLocal(bytes))}
          </button>
        </div>
        <div class="lib-actions-row">
          <button type="button" class="lib-btn lib-btn-sm lib-btn-outline" data-lib-action="bulk-disable"><span class="material-symbols-outlined">power_settings_new</span>Disable</button>
          <button type="button" class="lib-btn lib-btn-sm lib-btn-outline" data-lib-action="bulk-enable"><span class="material-symbols-outlined">power</span>Enable</button>
          <button type="button" class="lib-btn lib-btn-sm lib-btn-outline" data-lib-action="bulk-favorite"><span class="material-symbols-outlined">star</span>Favorite</button>
        </div>
        <div class="lib-actions-row">
          <button type="button" class="lib-btn lib-btn-sm lib-btn-outline" data-lib-action="bulk-clean"><span class="material-symbols-outlined">content_copy</span>Clean Duplicates</button>
          <button type="button" class="lib-btn lib-btn-sm lib-btn-outline" data-lib-action="bulk-organize"><span class="material-symbols-outlined">create_new_folder</span>Organize</button>
          <button type="button" class="lib-btn lib-btn-sm lib-btn-outline" data-lib-action="bulk-export"><span class="material-symbols-outlined">image</span>Export images</button>
          <button type="button" class="lib-btn lib-btn-sm lib-btn-outline" data-lib-action="bulk-clear">Deselect</button>
        </div>
      </div>
    </section>
    <section class="lib-ds"><div class="lib-box lib-sel-list">${rows}</div></section>`;
}

// ---- Data loading ------------------------------------------------------------

// Called on sidebar entry to the page. AddonPackages comes from the VaM
// directory, so the first visit loads it straight away (no Scan click); later
// visits re-query the backend's cached scan, which is cheap. Rescan re-walks.
async function refreshVarPackagesView() {
  libApplyPaneWidths();
  if (!invoke) return;
  if (state.vpHasListing) {
    await refreshVarPackagesFromFolder({ forceRescan: false, keepLoaded: true });
  } else if (vpResolveInputDir()) {
    state.varPackagesScannedDeep = state.varPackagesDeepScan;
    await refreshVarPackagesFromFolder({ forceRescan: true, reset: true });
  } else {
    renderVarPackages();
    renderVarPackagesFilterBar();
  }
}

// A new VaM directory is a different library: drop the listing so the page
// reloads from the new AddonPackages (now if it is on screen, else on entry).
function libOnVamDirChanged() {
  state.vpHasListing = false;
  state.varPackagesItems = [];
  state.varPackagesTotal = 0;
  state.varPackagesFacets = null;
  state.vpMissing = null;
  state.vpSelected.clear();
  state.vpSelAnchor = null;
  LIB_DETAILS.cache.clear();
  const view = $("var-packages-view");
  if (view && !view.classList.contains("hidden")) refreshVarPackagesView();
  else renderVarPackages();
}

// Fetches a slice of the folder listing.
//   forceRescan — re-walk the folders (otherwise served from the backend cache)
//   append      — load the next chunk below what is shown (infinite scroll)
//   keepLoaded  — refetch as many rows as are shown, so a refresh after a
//                 delete/move doesn't throw the user back to the top
//   reset       — clear the grid first (the Scan button)
async function refreshVarPackagesFromFolder({
  forceRescan = true,
  append = false,
  keepLoaded = false,
  reset = false,
} = {}) {
  if (!invoke) return;
  if (state.varPackagesLoading || state.varPackagesLoadingMore) {
    // Never drop a request: a scan or filter change that arrives mid-flight is
    // replayed once the current one settles (see the finally below).
    if (append) return;
    if (forceRescan) state.varPackagesDirty = true;
    else state.varPackagesRequery = true;
    return;
  }
  const inputDir = vpResolveInputDir();
  if (!inputDir) {
    if (forceRescan) addLog(t("varPackagesPickFirst"));
    renderVarPackages();
    return;
  }

  const loaded = (state.varPackagesItems ?? []).length;
  const offset = append ? loaded : 0;
  const limit = append
    ? VP_CHUNK
    : keepLoaded
      ? Math.min(VP_MAX_RELOAD, Math.max(VP_CHUNK, loaded))
      : VP_CHUNK;

  if (append) state.varPackagesLoadingMore = true;
  else state.varPackagesLoading = true;
  if (reset) {
    state.varPackagesItems = [];
    state.varPackagesTotal = 0;
    state.varPackagesFacets = null;
  }
  if (forceRescan) LIB_DETAILS.cache.clear();
  renderVarPackages();
  const scanBtn = $("var-packages-scan-button");
  if (scanBtn) scanBtn.disabled = true;

  let listed = false;
  try {
    const page = await invoke("list_var_packages", {
      inputDir,
      additionalInputDirs: getAdditionalDirs("varPackages"),
      offset,
      limit,
      search: String(state.varPackagesFilter ?? "").trim() || null,
      filters: serializeVarPackageFilters(state.varPackagesFilters),
      ...varPackagesSortArgs(),
      forceRescan,
      // The committed depth, never the switch position — see varPackagesScannedDeep.
      deepScan: state.varPackagesScannedDeep !== false,
    });
    const items = Array.isArray(page?.items) ? page.items : [];
    state.varPackagesItems = append ? [...(state.varPackagesItems ?? []), ...items] : items;
    state.varPackagesTotal = Math.max(0, Number(page?.total ?? 0));
    if (page?.facets) state.varPackagesFacets = page.facets;
    state.vpHasListing = true;
    listed = true;
  } catch (error) {
    if (!append) {
      state.varPackagesItems = [];
      state.varPackagesTotal = 0;
      state.varPackagesFacets = null;
    }
    addLog(t("varPackagesScanFailed", String(error)));
  } finally {
    state.varPackagesLoading = false;
    state.varPackagesLoadingMore = false;
    if (scanBtn) scanBtn.disabled = false;

    if (listed && !append) {
      libAfterFreshListing({ keepScroll: keepLoaded });
    }
    renderVarPackagesFilterBar();
    renderVarPackages();

    if (listed && !append) {
      // The Author suggestions come from the folder cache this call just left
      // in place, so reload them whenever the scanned roots or depth changed.
      const optionsKey = [
        inputDir,
        getAdditionalDirs("varPackages").join("|"),
        state.varPackagesScannedDeep !== false ? "deep" : "top",
      ].join(" ");
      const menuEmpty = (state.varPackagesFilterOptions?.creators?.length ?? 0) === 0;
      if (forceRescan || menuEmpty || state.varPackagesFilterOptionsKey !== optionsKey) {
        state.varPackagesFilterOptionsKey = optionsKey;
        loadVarPackagesFilterOptions().catch((err) =>
          addLog(t("varPackagesFilterOptionsFailed", String(err))),
        );
      }
      if (libIsMissingView() || forceRescan) state.vpMissing = null;
      if (libIsMissingView()) libLoadMissing();
    }
    applyVarPackagesDepthUi();

    if (state.varPackagesDirty) {
      state.varPackagesDirty = false;
      state.varPackagesRequery = false;
      refreshVarPackagesFromFolder({ forceRescan: true, keepLoaded: true }).catch((err) =>
        addLog(`VAR Packages: ${String(err)}`),
      );
    } else if (state.varPackagesRequery) {
      state.varPackagesRequery = false;
      refreshVarPackagesFromFolder({ forceRescan: false }).catch((err) =>
        addLog(`VAR Packages: ${String(err)}`),
      );
    }
  }
}

// After a fresh listing: refresh selection snapshots, follow a pending reveal,
// and — like Backstage — keep exactly one package in the details panel.
function libAfterFreshListing({ keepScroll }) {
  const items = state.varPackagesItems ?? [];
  const byPath = new Map(items.map((it) => [it.file_path, it]));
  for (const [fp, snap] of state.vpSelected) {
    const fresh = byPath.get(fp);
    if (fresh) state.vpSelected.set(fp, vpSnapshotItem(fresh));
    else if (snap && state.vpSelected.size === 1) state.vpSelected.delete(fp);
  }
  if (state.vpRevealPath && byPath.has(state.vpRevealPath)) {
    vpSelectOnly(byPath.get(state.vpRevealPath), { render: false });
    const target = state.vpRevealPath;
    state.vpRevealPath = null;
    requestAnimationFrame(() => libScrollIntoView(target));
  } else if (state.vpSelected.size === 0 && items.length) {
    vpSelectOnly(items[0], { render: false });
  }
  if (!keepScroll) {
    const scroll = $("lib-scroll");
    if (scroll) scroll.scrollTop = 0;
  }
}

// Every library mutation (delete, move, organize, collect) refreshes through
// here: the folder listing if one is on screen, and the Database page's
// package table if that is what the user is looking at.
async function vpRefreshAfterMutation() {
  LIB_DETAILS.cache.clear();
  if (state.vpHasListing) {
    if (state.varPackagesLoading) {
      state.varPackagesDirty = true;
    } else {
      await refreshVarPackagesFromFolder({ forceRescan: true, keepLoaded: true });
    }
  }
  if (typeof dbPkgsRefreshIfVisible === "function") dbPkgsRefreshIfVisible();
}

// ---- Enable / disable --------------------------------------------------------

async function libSetDisabled(snaps, disabled) {
  if (!invoke) return;
  const targets = snaps.filter((s) => s?.file_path && Boolean(s.disabled) !== disabled);
  if (!targets.length) return;
  let failed = 0;
  for (const s of targets) {
    try {
      await invoke("set_var_package_disabled", { filePath: s.file_path, disabled });
      s.disabled = disabled;
      const live = (state.varPackagesItems ?? []).find((it) => it.file_path === s.file_path);
      if (live) live.disabled = disabled;
      const sel = state.vpSelected.get(s.file_path);
      if (sel) sel.disabled = disabled;
    } catch (err) {
      failed += 1;
      addLog(`${disabled ? "Disable" : "Enable"} ${s.file_name ?? s.file_path}: ${String(err)}`);
    }
  }
  const done = targets.length - failed;
  if (done) {
    showToast(
      `${disabled ? "Disabled" : "Enabled"} ${done} package${done === 1 ? "" : "s"}`,
      failed ? "error" : "success",
    );
  } else if (failed) {
    showToast(`Could not ${disabled ? "disable" : "enable"} the package — see Console`, "error");
  }
  // The backend patched its cache, so a requery just refreshes the facets and
  // drops rows that no longer match an Enabled filter.
  await refreshVarPackagesFromFolder({ forceRescan: false, keepLoaded: true });
}

// ---- Selection ---------------------------------------------------------------

function vpSelectOnly(item, { render = true } = {}) {
  if (!vpSelectableItem(item)) return;
  state.vpSelected.clear();
  state.vpSelected.set(item.file_path, vpSnapshotItem(item));
  state.vpSelAnchor = item.file_path;
  state.vpLead = item.file_path;
  if (render) vpSyncSelectionUi();
}

function libItemAt(index) {
  const items = state.varPackagesItems ?? [];
  return items[Math.max(0, Math.min(items.length - 1, index))] ?? null;
}

function libLeadIndex() {
  const items = state.varPackagesItems ?? [];
  const lead = state.vpLead ?? state.vpSelAnchor;
  const idx = items.findIndex((it) => it.file_path === lead);
  return idx;
}

function libColumns() {
  if (state.vpView === "table") return 1;
  const grid = $("var-packages-grid");
  return Math.max(1, Number(grid?.style.getPropertyValue("--lib-cols")) || 1);
}

// Grid-aware keyboard navigation, after Backstage's: arrows/Home/End move the
// selection, Shift extends it, Ctrl/Cmd+A selects every match, Esc collapses a
// multi-selection, Enter opens VAR Details, Delete recycles.
function libOnKeyDown(event) {
  const view = $("var-packages-view");
  if (!view || view.classList.contains("hidden") || libIsMissingView()) return;
  const tag = event.target?.tagName;
  if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || event.target?.isContentEditable) return;
  if (document.querySelector(".modal-backdrop:not(.hidden), dialog[open]")) return;
  const items = state.varPackagesItems ?? [];
  if (!items.length) return;
  const mod = event.ctrlKey || event.metaKey;

  if (mod && (event.key === "a" || event.key === "A")) {
    event.preventDefault();
    vpSelectAll().catch((e) => addLog(`Select all: ${String(e)}`));
    return;
  }
  if (event.key === "Escape") {
    if (state.vpSelected.size > 1) {
      const lead = libFindItem(state.vpLead);
      if (lead) vpSelectOnly(lead);
      else vpClearSelection();
    }
    return;
  }
  if (event.key === "Enter") {
    const item = libFindItem(state.vpLead ?? state.vpSelAnchor);
    if (item) openVarDetailsView(item, "folder");
    return;
  }
  if (event.key === "Delete") {
    event.preventDefault();
    if (state.vpSelected.size > 1) vpBulkDelete().catch((e) => addLog(`Bulk delete: ${String(e)}`));
    else if (state.vpSelected.size === 1) {
      vpDeleteOne(state.vpSelected.keys().next().value, null).catch((e) => addLog(`VAR Packages: ${String(e)}`));
    }
    return;
  }

  const cols = libColumns();
  const deltas = {
    ArrowLeft: -1,
    ArrowRight: 1,
    ArrowUp: -cols,
    ArrowDown: cols,
  };
  let target = null;
  const current = libLeadIndex();
  if (event.key in deltas) {
    if (state.vpView === "table" && (event.key === "ArrowLeft" || event.key === "ArrowRight")) return;
    target = current < 0 ? 0 : current + deltas[event.key];
    if (target < 0 || target >= items.length) {
      // At the bottom edge of what is loaded: pull the next chunk.
      if (target >= items.length) libMaybeLoadMore(true);
      return;
    }
  } else if (event.key === "Home") {
    target = 0;
  } else if (event.key === "End") {
    target = items.length - 1;
  } else if (event.key === " ") {
    const lead = libFindItem(state.vpLead);
    if (lead) {
      event.preventDefault();
      vpToggleSelect(lead);
    }
    return;
  } else {
    return;
  }
  event.preventDefault();
  const item = libItemAt(target);
  if (!item) return;
  if (event.shiftKey) {
    vpSelectRangeTo(item);
    state.vpLead = item.file_path;
    vpSyncSelectionUi();
  } else if (mod) {
    state.vpLead = item.file_path;
    vpSyncSelectionUi();
  } else {
    vpSelectOnly(item);
  }
  libScrollIntoView(item.file_path);
}

function libMaybeLoadMore(force = false) {
  if (libIsMissingView()) return;
  const scroll = $("lib-scroll");
  const loaded = (state.varPackagesItems ?? []).length;
  const total = Math.max(0, Number(state.varPackagesTotal ?? 0));
  if (!scroll || loaded >= total || state.varPackagesLoading || state.varPackagesLoadingMore) return;
  const nearBottom = scroll.scrollTop + scroll.clientHeight >= scroll.scrollHeight - 600;
  if (force || nearBottom) {
    refreshVarPackagesFromFolder({ forceRescan: false, append: true }).catch((e) =>
      addLog(`VAR Packages: ${String(e)}`),
    );
  }
}

// ---- Wiring ------------------------------------------------------------------

function libSelectedSnaps() {
  return [...state.vpSelected.values()];
}

function libCurrentItem() {
  if (state.vpSelected.size !== 1) return null;
  return libFindItem(state.vpSelected.keys().next().value);
}

function libRunAction(action, trigger) {
  const item = libCurrentItem();
  switch (action) {
    case "scan":
      $("var-packages-scan-button")?.click();
      break;
    case "view-missing":
      setVarPackagesFilter("status", "missing");
      break;
    case "find-missing":
      libFindMissing(
        (state.vpMissing?.items ?? []).filter((m) => m.status === "missing").map((m) => m.id),
      );
      break;
    case "refresh-missing":
      libLoadMissing();
      break;
    case "clean-old":
      vpStartPlan("clean_duplicates").catch((e) => addLog(`VAR Packages: ${String(e)}`));
      break;
    case "open-details":
    case "browse-files":
      if (item) openVarDetailsView(item, "folder");
      break;
    case "delete":
      if (item) vpDeleteOne(item.file_path, trigger).catch((e) => addLog(`VAR Packages: ${String(e)}`));
      break;
    case "toggle-disabled":
      if (item) libSetDisabled([libFindItem(item.file_path) ?? item], !item.disabled);
      break;
    case "explorer":
      if (item && invoke) {
        invoke("show_in_explorer", { path: item.file_path }).catch((e) => addLog(`VAR Packages: ${String(e)}`));
      }
      break;
    case "images":
      if (item) vpImagesOpen(item.file_path);
      break;
    case "find-deps": {
      const details = item ? LIB_DETAILS.cache.get(libDetailsKey(item)) : null;
      libFindMissing((details?.dependencies ?? []).filter((d) => d.status === "missing").map((d) => d.id));
      break;
    }
    case "bulk-delete":
      vpBulkDelete().catch((e) => addLog(`Bulk delete: ${String(e)}`));
      break;
    case "bulk-disable":
      libSetDisabled(libSelectedSnaps(), true);
      break;
    case "bulk-enable":
      libSetDisabled(libSelectedSnaps(), false);
      break;
    case "bulk-favorite":
      (async () => {
        for (const s of libSelectedSnaps()) {
          if (s.package_id && !_favoritePackages.has(s.package_id)) await togglePackageFavorite(s.package_id);
        }
      })().catch((e) => addLog(`Favorites: ${String(e)}`));
      break;
    case "bulk-clean":
      vpBulkClean().catch((e) => addLog(`Clean Duplicates: ${String(e)}`));
      break;
    case "bulk-organize":
      vpBulkOrganize().catch((e) => addLog(`Bulk organize: ${String(e)}`));
      break;
    case "bulk-export":
      vpBulkExportImages().catch((e) => addLog(`Bulk export: ${String(e)}`));
      break;
    case "bulk-clear": {
      const lead = libFindItem(state.vpLead);
      if (lead) vpSelectOnly(lead);
      else vpClearSelection();
      break;
    }
    default:
      break;
  }
}

function libSelectAllItem() {
  const total = Math.max(0, Number(state.varPackagesTotal ?? 0));
  return {
    label: `Select all (${total.toLocaleString()})`,
    action: () => vpSelectAll(),
  };
}

// Right-click on empty space in the grid. The folder tools (Clean Duplicates,
// Organize, Export) live on the bulk toolbar and the selection panel; Find
// Dependencies (paste text) is in the app's top bar.
function libBackgroundMenu(event) {
  const hasItems = (state.varPackagesItems ?? []).length > 0 && !libIsMissingView();
  const items = [];
  if (hasItems) {
    items.push(libSelectAllItem());
    if (state.vpSelected.size > 1) items.push({ label: "Deselect", action: () => libRunAction("bulk-clear") });
    items.push({ separator: true });
  }
  items.push({ label: "Rescan AddonPackages", action: () => $("var-packages-scan-button")?.click() });
  showContextMenu(event.clientX, event.clientY, items);
}

function libContextMenu(event, item) {
  const filePath = item.file_path;
  const packageId = item.package_id;
  const fav = _favoritePackages.has(packageId);
  const multi = state.vpSelected.size > 1 && state.vpSelected.has(filePath);
  const items = multi
    ? [
        { label: `Disable ${state.vpSelected.size} packages`, action: () => libRunAction("bulk-disable") },
        { label: `Enable ${state.vpSelected.size} packages`, action: () => libRunAction("bulk-enable") },
        { label: "Add to favorites", action: () => libRunAction("bulk-favorite") },
        { separator: true },
        { label: "Clean Duplicates of selected…", action: () => libRunAction("bulk-clean") },
        { label: "Organize selected by Creator…", action: () => libRunAction("bulk-organize") },
        { label: "Export selected Scene Images", action: () => libRunAction("bulk-export") },
        { separator: true },
        libSelectAllItem(),
        { label: "Deselect", action: () => libRunAction("bulk-clear") },
        { separator: true },
        {
          label: `Delete ${state.vpSelected.size} packages (Recycle Bin)`,
          danger: true,
          action: () => libRunAction("bulk-delete"),
        },
      ]
    : [
        { label: "Open Details", action: () => openVarDetailsView(item, "folder") },
        {
          label: "Show in Explorer",
          action: () =>
            invoke("show_in_explorer", { path: filePath }).catch((e) => addLog(`VAR Packages: ${String(e)}`)),
        },
        {
          label: fav ? "Remove from favorites" : "Add to favorites",
          action: () => togglePackageFavorite(packageId),
        },
        {
          label: item.disabled ? "Enable" : "Disable",
          action: () => libSetDisabled([item], !item.disabled),
        },
        { separator: true },
        {
          label: "Download Dependencies…",
          action: () =>
            depStartScan({ filePath, packageId }).catch((e) => addLog(`Download Dependencies: ${String(e)}`)),
        },
        { label: "Find Dependencies Locally…", action: () => dcOpen({ filePath, packageId }) },
        { label: "Export Scene Image", action: () => exportOneSceneImage(filePath, packageId) },
        {
          label: "Move to creator folder",
          action: () =>
            vpMoveToCreatorFolder(filePath).catch((e) => addLog(`Move to creator folder: ${String(e)}`)),
        },
        {
          label: "Send to",
          submenu: [
            { label: "Clean VARs", action: () => sendVarToTargetPage("db-find", filePath) },
            { label: "Missing Resources", action: () => sendVarToTargetPage("missing-resources", filePath) },
            { label: "Internalize Resources", action: () => sendVarToTargetPage("internalize-resources", filePath) },
          ],
        },
        { separator: true },
        { label: t("varPackagesImagesOpen"), action: () => vpImagesOpen(filePath) },
        { separator: true },
        libSelectAllItem(),
        { separator: true },
        {
          label: "Delete (Recycle Bin)",
          danger: true,
          action: () => vpDeleteOne(filePath, null).catch((e) => addLog(`VAR Packages: ${String(e)}`)),
        },
      ];
  showContextMenu(event.clientX, event.clientY, items);
}

// Clicks shared by the grid, the table, the Missing table and the details
// panel: author links, package reveals, actions, expanders.
function libHandleSharedClick(event) {
  const target = event.target;
  const author = target.closest?.("[data-lib-author]");
  if (author) {
    event.stopPropagation();
    setVarPackagesFilter("creator", author.getAttribute("data-lib-author"));
    return true;
  }
  const reveal = target.closest?.("[data-lib-reveal]");
  if (reveal) {
    const fp = reveal.getAttribute("data-lib-reveal");
    if (fp) libRevealPackage(fp, reveal.getAttribute("data-lib-reveal-id"));
    return true;
  }
  const expand = target.closest?.("[data-lib-expand]");
  if (expand) {
    const key = expand.getAttribute("data-lib-expand");
    if (LIB_DETAILS.expanded.has(key)) LIB_DETAILS.expanded.delete(key);
    else LIB_DETAILS.expanded.add(key);
    libRenderDetail();
    return true;
  }
  const cat = target.closest?.("[data-lib-cat]");
  if (cat) {
    const key = LIB_STORE.category + cat.getAttribute("data-lib-cat");
    libStoreSet(key, libStoreGet(key) === "0" ? "1" : "0");
    libRenderDetail();
    return true;
  }
  const url = target.closest?.("[data-lib-url]");
  if (url) {
    const href = url.getAttribute("data-lib-url");
    if (href && invoke) invoke("open_url", { url: href }).catch((e) => addLog(`Open link: ${String(e)}`));
    return true;
  }
  const action = target.closest?.("[data-lib-action]");
  if (action) {
    libRunAction(action.getAttribute("data-lib-action"), action);
    return true;
  }
  return false;
}

function setupLibraryView() {
  const clamp = (v, lo, hi, d) => {
    const n = Number(v);
    return Number.isFinite(n) && n > 0 ? Math.min(hi, Math.max(lo, n)) : d;
  };
  const storedView = libStoreGet(LIB_STORE.view, "cards");
  state.vpView = ["compact", "cards", "table"].includes(storedView) ? storedView : "cards";
  state.vpCardWidth = clamp(libStoreGet(LIB_STORE.cardWidth), 100, 500, 220);
  state.vpDetailWidth = clamp(libStoreGet(LIB_STORE.detailWidth), 260, 500, 340);
  libApplyPaneWidths();
  setupLibResizeHandles();

  // --- Folder source --------------------------------------------------------
  // The folder is AddonPackages under Settings → VaM directory (applyVamDir);
  // only the extra folders and the scan depth are chosen here.
  $("var-packages-scan-button")?.addEventListener("click", () => {
    if (!vpResolveInputDir()) {
      openVamDirSettings();
      return;
    }
    // The only place the depth switch takes effect. Everything else (scrolling,
    // filters, post-delete refreshes) keeps whatever depth this listing used.
    state.varPackagesScannedDeep = state.varPackagesDeepScan;
    // A rescan is the commit point for root/depth changes; old selected paths
    // may fall out of scope.
    state.vpSelected.clear();
    state.vpSelAnchor = null;
    refreshVarPackagesFromFolder({ forceRescan: true, reset: true });
  });
  $("var-packages-depth-deep")?.addEventListener("click", () => setVarPackagesDeepScan(true));
  $("var-packages-depth-normal")?.addEventListener("click", () => setVarPackagesDeepScan(false));
  applyVarPackagesDepthUi();

  // --- Search ---------------------------------------------------------------
  const search = $("var-packages-filter");
  let searchTimer = null;
  search?.addEventListener("input", () => {
    if (searchTimer) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      const next = search.value;
      if (state.varPackagesFilter === next) return;
      state.varPackagesFilter = next;
      renderVarPackagesFilterBar();
      refreshVarPackagesFromFolder({ forceRescan: false });
    }, 180);
  });
  search?.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && search.value) {
      search.value = "";
      search.dispatchEvent(new Event("input"));
    }
  });
  $("lib-search-clear")?.addEventListener("click", () => {
    if (!search) return;
    search.value = "";
    search.dispatchEvent(new Event("input"));
    search.focus();
  });

  // --- Filter bar dropdowns ----------------------------------------------------
  document.querySelectorAll("[data-lib-dd-trigger]").forEach((trigger) => {
    trigger.addEventListener("click", (event) => {
      event.stopPropagation();
      libToggleDropdown(trigger.getAttribute("data-lib-dd-trigger"));
    });
  });
  // Clicks inside a menu stay in it; anywhere else closes whatever is open.
  document.querySelectorAll("[data-lib-dd-menu]").forEach((menu) => {
    menu.addEventListener("click", (event) => event.stopPropagation());
  });
  document.addEventListener("click", () => libCloseDropdowns());
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && document.querySelector("[data-lib-dd-menu]:not(.hidden)")) {
      libCloseDropdowns();
    }
  });

  const listField = {
    "lib-status-list": "status",
    "lib-type-list": "pkgType",
    "lib-enabled-list": "enabled",
    "lib-size-list": "sizeBucket",
    "lib-scene-list": "scene",
  };
  for (const [id, field] of Object.entries(listField)) {
    $(id)?.addEventListener("click", (event) => {
      const row = event.target.closest("[data-lib-value]");
      if (!row) return;
      let value = row.getAttribute("data-lib-value") || null;
      // Clicking the sole selected type again clears it, as in Backstage.
      if (field === "pkgType" && value && state.varPackagesFilters?.pkgType === value) value = null;
      libCloseDropdowns();
      setVarPackagesFilter(field, value);
    });
  }
  $("lib-sort-list")?.addEventListener("click", (event) => {
    const row = event.target.closest("[data-lib-value]");
    if (!row) return;
    libCloseDropdowns();
    setVarPackagesSort(row.getAttribute("data-lib-value"));
  });
  $("lib-sort-dir")?.addEventListener("click", () =>
    setVarPackagesSort(state.varPackagesSort?.key ?? "type", { flip: true }),
  );

  // --- Author ---------------------------------------------------------------
  const authorInput = $("lib-author-input");
  authorInput?.addEventListener("input", () => {
    LIB_AC.active = -1;
    libRenderAuthorPopup();
  });
  authorInput?.addEventListener("keydown", (event) => {
    const n = LIB_AC.matches.length;
    if (event.key === "ArrowDown" && n) {
      event.preventDefault();
      LIB_AC.active = (LIB_AC.active + 1) % n;
      libRenderAuthorPopup();
    } else if (event.key === "ArrowUp" && n) {
      event.preventDefault();
      LIB_AC.active = (LIB_AC.active - 1 + n) % n;
      libRenderAuthorPopup();
    } else if (event.key === "Enter") {
      event.preventDefault();
      const exact = (state.varPackagesFilterOptions?.creators ?? []).find(
        (c) => c.toLowerCase() === authorInput.value.trim().toLowerCase(),
      );
      const pick = LIB_AC.matches[LIB_AC.active] ?? exact ?? LIB_AC.matches[0];
      if (pick) libPickAuthor(pick);
    }
  });
  $("lib-author-popup")?.addEventListener("click", (event) => {
    const opt = event.target.closest("[data-lib-author-pick]");
    if (opt) libPickAuthor(opt.getAttribute("data-lib-author-pick"));
  });
  $("lib-author-chips")?.addEventListener("click", (event) => {
    if (!event.target.closest("[data-lib-clear-author]")) return;
    libCloseDropdowns();
    setVarPackagesFilter("creator", null);
  });

  // --- Toolbar ----------------------------------------------------------------
  $("var-packages-filter-clear")?.addEventListener("click", clearVarPackagesFilters);
  document.querySelectorAll("[data-lib-view]").forEach((btn) => {
    btn.addEventListener("click", () => libSetView(btn.getAttribute("data-lib-view")));
  });
  $("lib-size-slider")?.addEventListener("input", (e) => libSetColumns(e.target.value));
  $("lib-select-hint-close")?.addEventListener("click", () => {
    libStoreSet(LIB_STORE.hint, "1");
    libRenderToolbar();
  });
  $("lib-toolbar-actions")?.addEventListener("click", (event) => {
    const btn = event.target.closest("[data-lib-action]");
    if (btn && !btn.disabled) libRunAction(btn.getAttribute("data-lib-action"), btn);
  });
  $("lib-bulk-disable")?.addEventListener("click", () => libRunAction("bulk-disable"));
  $("lib-bulk-clean")?.addEventListener("click", () => libRunAction("bulk-clean"));
  $("lib-bulk-enable")?.addEventListener("click", () => libRunAction("bulk-enable"));
  $("lib-bulk-close")?.addEventListener("click", () => libRunAction("bulk-clear"));

  // --- Grid / table -----------------------------------------------------------
  const scroll = $("lib-scroll");
  const hostItem = (target) => {
    const host = target.closest?.("[data-file-path][data-idx]");
    if (!host) return null;
    const fp = host.getAttribute("data-file-path");
    return (state.varPackagesItems ?? []).find((it) => it.file_path === fp) ?? null;
  };
  scroll?.addEventListener("click", (event) => {
    // Favorite / checkbox / delete / images are handled by their own delegated
    // listeners (setupVarPackagesMaintenance, setupVarPackagesSelection).
    if (event.target.closest("[data-vp-fav],[data-vp-select],[data-vp-delete],[data-vp-images]")) return;
    if (libHandleSharedClick(event)) return;
    const item = hostItem(event.target);
    if (!item) return;
    const mod = event.ctrlKey || event.metaKey;
    state.vpLead = item.file_path;
    if (event.shiftKey) {
      // Shift replaces the selection with the range; Ctrl+Shift adds to it.
      // The anchor survives the clear, so the range starts where it should.
      if (!mod) state.vpSelected.clear();
      vpSelectRangeTo(item);
    } else if (mod) {
      vpToggleSelect(item);
    } else {
      vpSelectOnly(item);
    }
  });
  scroll?.addEventListener("dblclick", (event) => {
    if (event.target.closest("button, input, [data-lib-author]")) return;
    const item = hostItem(event.target);
    if (item) openVarDetailsView(item, "folder");
  });
  scroll?.addEventListener("contextmenu", (event) => {
    const item = hostItem(event.target);
    if (!item) {
      if (event.target.closest("a, input, textarea")) return;
      event.preventDefault();
      libBackgroundMenu(event);
      return;
    }
    event.preventDefault();
    if (!state.vpSelected.has(item.file_path)) vpSelectOnly(item);
    libContextMenu(event, item);
  });
  scroll?.addEventListener("mousedown", (event) => {
    // Shift-click would otherwise smear a text selection across cards.
    if (event.shiftKey) event.preventDefault();
  });
  scroll?.addEventListener(
    "scroll",
    () => {
      libMaybeLoadMore();
      $("lib-scroll-top")?.classList.toggle("hidden", scroll.scrollTop <= scroll.clientHeight);
    },
    { passive: true },
  );
  $("lib-scroll-top")?.addEventListener("click", () => scroll?.scrollTo({ top: 0, behavior: "smooth" }));
  if (scroll && typeof ResizeObserver === "function") {
    new ResizeObserver(() => libApplyLayout()).observe(scroll);
  }

  // --- Details panel ------------------------------------------------------------
  $("lib-detail")?.addEventListener("click", (event) => {
    const fav = event.target.closest("[data-vp-fav]");
    if (fav) {
      togglePackageFavorite(fav.getAttribute("data-vp-fav"));
      return;
    }
    libHandleSharedClick(event);
  });

  document.addEventListener("keydown", libOnKeyDown);
  renderVarPackagesFilterBar();
  renderVarPackages();
}

// ===========================================================================
// VAR Packages maintenance — Clean Duplicates + Organize by Creator.
//
// Both features share one flow: run a read-only planner, show the plan, let the
// user tick/untick rows, then apply. The plan itself lives server-side; we only
// ever send back indices.
//
// Self-contained on purpose (own poll loop, own dialog) rather than going
// through state.activeTask/showProgress — that switch is for the global,
// single-task pages.
// ===========================================================================

const VP_PLAN = {
  kind: "",
  planId: null,
  actions: [],
  response: null,
  taskId: null,
  running: false,
  // Set of lowercased file paths from a bulk-organize selection. When set,
  // vpRenderPlan pre-ticks ONLY matching pending rows — the plan itself still
  // covers every configured folder.
  limitPaths: null,
};

const VP_REASON_LABEL = {
  older_version: "Older version",
  duplicate_copy: "Duplicate copy",
  organize: "Move to creator folder",
};

const VP_STATUS_LABEL = {
  protected: "Protected",
  unverified: "Unverified",
  blocked: "No Recycle Bin",
  skipped: "Skipped",
  done: "Done",
  failed: "Failed",
};

// `setText` exists only as function-local consts elsewhere in this file, never
// at global scope, so define our own rather than shadowing anything.
function vpSetText(id, value) {
  const el = $(id);
  if (el) el.textContent = String(value ?? "");
}

/// Hoisted out of refreshVarPackagesFromFolder so Scan and both maintenance
/// buttons resolve the root identically — including the fall back to the
/// Overview folder when this page's field is empty.
function vpResolveInputDir() {
  const inputField = $("var-packages-input-dir");
  const mainInput = $("input-dir");
  if (inputField && !inputField.value && mainInput && mainInput.value) {
    inputField.value = mainInput.value;
  }
  return inputField ? inputField.value.trim() : "";
}

function vpRoots() {
  return {
    inputDir: vpResolveInputDir(),
    additionalInputDirs: getAdditionalDirs("varPackages"),
  };
}

function vpBusy(busy) {
  VP_PLAN.running = busy;
  for (const id of [
    "var-packages-scan-button",
    "var-packages-pick-input",
  ]) {
    const el = $(id);
    if (el) el.disabled = busy;
  }
  const apply = $("vp-plan-apply");
  if (apply) apply.disabled = busy;
  const deep = $("vp-plan-deep");
  if (deep) deep.disabled = busy;
  // While a task runs, Close becomes Cancel: hiding the dialog would leave the
  // worker recycling files behind the user's back.
  vpSetText("vp-plan-cancel", busy ? "Cancel" : "Close");
  $("vp-plan-progress")?.classList.toggle("hidden", !busy);
}

/// Starts a task and polls it to completion, returning its package_op_result.
///
/// get_task_progress returns a Result and ERRORS with "task {id} not found" —
/// it never resolves to null — so the try/catch is the real exit, not a falsy
/// check on the payload.
async function vpRunTask(command, args) {
  const handle = await invoke(command, args);
  VP_PLAN.taskId = handle?.id ?? null;
  if (VP_PLAN.taskId == null) throw new Error("task did not start");

  try {
    for (;;) {
      await new Promise((resolve) => setTimeout(resolve, 300));
      const payload = await invoke("get_task_progress", { taskId: VP_PLAN.taskId });
      if (!payload) break;

      const pct = Math.round(Math.max(0, Math.min(1, Number(payload.progress ?? 0))) * 100);
      const bar = $("vp-plan-bar");
      if (bar) bar.style.width = `${pct}%`;
      vpSetText("vp-plan-progress-message", payload.message ?? "");

      if (payload.error) throw new Error(String(payload.error));
      if (payload.done) return payload.package_op_result ?? null;
    }
    return null;
  } finally {
    const id = VP_PLAN.taskId;
    VP_PLAN.taskId = null;
    try {
      await invoke("clear_task", { taskId: id });
    } catch (_e) {
      /* the task may already be gone; nothing to clean up */
    }
  }
}

function vpOpenPlanDialog() {
  $("vp-plan-backdrop")?.classList.remove("hidden");
}

function vpClosePlan() {
  // Refuse while a task is live — see vpBusy.
  if (VP_PLAN.running) return;
  $("vp-plan-backdrop")?.classList.add("hidden");
  VP_PLAN.kind = "";
  VP_PLAN.planId = null;
  VP_PLAN.actions = [];
  VP_PLAN.response = null;
  VP_PLAN.limitPaths = null;
}

async function vpStartPlan(kind, { limitPaths = null, deepOverride = null } = {}) {
  if (!invoke || VP_PLAN.running) return;

  const isDupes = kind === "clean_duplicates";
  VP_PLAN.kind = kind;
  VP_PLAN.actions = [];
  VP_PLAN.response = null;
  VP_PLAN.planId = null;
  VP_PLAN.limitPaths = limitPaths;

  vpSetText("vp-plan-title", isDupes ? "Clean Duplicates" : "Organize by Creator");
  vpSetText("vp-plan-summary", "");
  $("vp-plan-list").innerHTML = "";
  $("vp-plan-warning")?.classList.add("hidden");
  $("vp-plan-notes-wrap")?.classList.add("hidden");
  // Deep scan only means anything for the dependency check.
  $("vp-plan-deep-row")?.classList.toggle("hidden", !isDupes);

  const { inputDir, additionalInputDirs } = vpRoots();
  if (!inputDir) {
    // addLog alone is invisible here: the console drawer is closed by default.
    const field = $("var-packages-input-dir");
    field?.focus();
    field?.classList.add("vp-input-invalid");
    setTimeout(() => field?.classList.remove("vp-input-invalid"), 1600);
    vpSetText("vp-plan-scope", "");
    vpSetText("vp-plan-summary", "Pick a VAR folder first.");
    $("vp-plan-apply").disabled = true;
    vpOpenPlanDialog();
    return;
  }

  const roots = [inputDir, ...additionalInputDirs];
  // These run their own scan when clicked, so they follow the switch as it is
  // now — not whatever depth the grid behind them happens to be showing. Say
  // which was used, since the two produce very different plans.
  // deepOverride: bulk-organize passes the COMMITTED depth instead — the
  // selection was made against the committed listing, so the plan must cover
  // the same tree or selected subfolder files would silently drop out.
  const deep = deepOverride != null ? deepOverride : state.varPackagesDeepScan !== false;
  // Organize files creator folders under the Settings VAR library root
  // (AddonPackages root); additional folders are ignored for the destination.
  const orgRoot = isDupes ? "" : ($("settings-library-folder")?.value || "").trim();
  vpSetText(
    "vp-plan-scope",
    `${isDupes ? "Scanning" : "Organizing"} ${roots.join("  ·  ")}\n` +
      (deep
        ? "Deep — including every subfolder."
        : "Normal — only packages directly in these folders. Subfolders are left alone.") +
      (orgRoot ? `\nInto creator folders under ${orgRoot}` : ""),
  );

  // The header count pill is a FILTERED total, so someone filtered to one
  // creator can see "12 packages" and be handed a plan spanning hundreds.
  const filtered =
    activeVarPackageFilterCount(state.varPackagesFilters) > 0 ||
    String(state.varPackagesFilter ?? "").trim() !== "";
  const warning = $("vp-plan-warning");
  if (limitPaths && warning) {
    warning.textContent = t("varPackagesBulkOrganizeScopeNote", limitPaths.size);
    warning.classList.remove("hidden");
  } else if (filtered && warning) {
    warning.textContent =
      "Your search and filters do not limit this operation — it covers every configured folder.";
    warning.classList.remove("hidden");
  }

  vpOpenPlanDialog();
  vpBusy(true);
  try {
    const command = isDupes
      ? "start_plan_clean_duplicates_task"
      : "start_plan_organize_by_creator_task";
    // deepScan = the page's folder-depth switch (walk subfolders or not).
    // deepPayloadScan = read text payloads inside each .var for version refs.
    // Two different knobs that both read as "deep"; keep them straight.
    const args = { inputDir, additionalInputDirs, deepScan: deep };
    if (isDupes) args.deepPayloadScan = $("vp-plan-deep")?.checked !== false;
    else args.destRoot = orgRoot || null;

    const res = await vpRunTask(command, args);
    VP_PLAN.response = res;
    VP_PLAN.actions = res?.actions ?? [];
    VP_PLAN.planId = res?.plan_id ?? null;
    vpRenderPlan();
  } catch (err) {
    vpSetText("vp-plan-summary", `Failed: ${String(err)}`);
    addLog(`VAR Packages: ${String(err)}`);
  } finally {
    // Always clears `running`, so a thrown helper can never brick the toolbar
    // for the rest of the session.
    vpBusy(false);
    const apply = $("vp-plan-apply");
    if (apply) apply.disabled = VP_PLAN.actions.length === 0;
  }
}

function vpRenderPlan() {
  const res = VP_PLAN.response;
  if (!res) return;
  const isDupes = VP_PLAN.kind === "clean_duplicates";

  const pending = VP_PLAN.actions.filter((a) => a.status === "pending").length;
  if (res.was_cancelled) {
    vpSetText("vp-plan-summary", "Cancelled — nothing was changed.");
  } else if (!VP_PLAN.actions.length) {
    vpSetText(
      "vp-plan-summary",
      isDupes
        ? `No duplicates found. Checked ${res.scanned} package(s).`
        : `Everything is already filed by creator. Checked ${res.scanned} package(s).`,
    );
  } else if (isDupes) {
    vpSetText(
      "vp-plan-summary",
      `${pending} package(s) to the Recycle Bin, freeing ${formatBytesLocal(res.reclaimable_bytes)} · ` +
        `${res.unchanged} left alone`,
    );
  } else {
    let summary = `${pending} package(s) will move into creator folders · ${res.unchanged} already correct`;
    if (VP_PLAN.limitPaths) {
      // Honest signal for selected packages that produced no row: they are
      // either already filed (counted in `unchanged`) or excluded (in notes).
      const matched = VP_PLAN.actions.filter(
        (a) =>
          a.status === "pending" &&
          VP_PLAN.limitPaths.has(String(a.file_path).toLowerCase()),
      ).length;
      summary += ` · ${t("varPackagesBulkOrganizeHint", matched, VP_PLAN.limitPaths.size)}`;
    }
    vpSetText("vp-plan-summary", summary);
  }

  const warning = $("vp-plan-warning");
  if (warning && res.protection_complete === false) {
    warning.textContent =
      "Some packages could not be read, so we could not confirm nothing still depends on these. " +
      "Those rows are unticked — review them before applying.";
    warning.classList.remove("hidden");
  }

  const rows = VP_PLAN.actions
    .map((a, i) => {
      const chips = [`<span class="chip">${escapeHtml(VP_REASON_LABEL[a.reason] ?? a.reason)}</span>`];
      if (a.status !== "pending") {
        chips.push(
          `<span class="chip vp-plan-${escapeHtml(a.status)}">${escapeHtml(
            VP_STATUS_LABEL[a.status] ?? a.status,
          )}</span>`,
        );
      }
      const sizes = a.kept_size_bytes
        ? `${formatBytesLocal(a.size_bytes)} → keeping ${formatBytesLocal(a.kept_size_bytes)}`
        : formatBytesLocal(a.size_bytes);
      const detail = a.detail || a.dest_path || (a.kept_path ? `keeping ${a.kept_path}` : "");
      const preChecked =
        a.status === "pending" &&
        (!VP_PLAN.limitPaths ||
          VP_PLAN.limitPaths.has(String(a.file_path).toLowerCase()));
      return `<label class="check-row vp-plan-row">
  <input type="checkbox" class="vp-plan-check" data-vp-idx="${i}"
         ${preChecked ? "checked" : ""} ${a.status === "blocked" ? "disabled" : ""} />
  <span class="vp-plan-main">
    <span class="vp-plan-name">${escapeHtml(a.package_id)}</span>
    <span class="vp-plan-path">${escapeHtml(a.file_path)}</span>
    <span class="vp-plan-detail">${escapeHtml(detail)}</span>
  </span>
  <span class="vp-plan-size">${escapeHtml(sizes)}</span>
  <span class="vp-plan-chips">${chips.join("")}</span>
  <button class="icon-button" type="button" data-vp-reveal="${i}" title="Show in Explorer">
    <span class="material-symbols-outlined">folder_open</span>
  </button>
</label>`;
    })
    .join("");
  $("vp-plan-list").innerHTML = rows;

  const notes = res.notes ?? [];
  const wrap = $("vp-plan-notes-wrap");
  if (wrap) {
    wrap.classList.toggle("hidden", notes.length === 0);
    vpSetText("vp-plan-notes-summary", `${notes.length} note(s) about skipped files`);
    $("vp-plan-notes").innerHTML = notes
      .map((n) => `<p class="vp-plan-note">${escapeHtml(n)}</p>`)
      .join("");
  }
}

async function vpApplyPlan() {
  if (!invoke || VP_PLAN.running || VP_PLAN.planId == null) return;

  const picked = [...document.querySelectorAll(".vp-plan-check:checked")].map((c) =>
    Number(c.dataset.vpIdx),
  );
  if (!picked.length) {
    vpSetText("vp-plan-summary", "Nothing selected.");
    return;
  }
  // Ticking a `protected` row IS the override — no separate flag needed.
  const forced = picked.filter((i) => VP_PLAN.actions[i]?.status === "protected");
  const isDupes = VP_PLAN.kind === "clean_duplicates";

  let msg = `This will ${isDupes ? "send to the Recycle Bin" : "move"} ${picked.length} package${
    picked.length === 1 ? "" : "s"
  }.`;
  if (forced.length) {
    msg += ` ${forced.length} of them ${
      forced.length === 1 ? "is" : "are"
    } still referenced by other packages and may break scenes.`;
  }
  if (VP_PLAN.response?.protection_complete === false) {
    msg += " Some packages could not be read, so protection is incomplete.";
  }
  if (isDupes) {
    // We only ever check other *packages*; loose files are out of scope, and
    // saying so is the honest alternative to a scan that can never be complete.
    msg += " Loose scenes and presets under Saves/ and Custom/ are not checked.";
  }
  if (!(await showAppConfirm(msg))) return;

  vpBusy(true);
  try {
    const planNotes = VP_PLAN.response?.notes ?? [];
    const res = await vpRunTask("start_apply_package_plan_task", {
      planId: VP_PLAN.planId,
      selected: picked,
      forced,
    });
    // The apply response carries no notes of its own, so keep the plan's — they
    // explain what was deliberately excluded, which is exactly what the user is
    // reconciling against while reviewing the outcome.
    if (res && !res.notes?.length) res.notes = planNotes;
    VP_PLAN.response = res;
    VP_PLAN.actions = res?.actions ?? VP_PLAN.actions;
    // The plan is consumed server-side once applied.
    VP_PLAN.planId = null;

    const done = VP_PLAN.actions.filter((a) => a.status === "done");
    const failed = VP_PLAN.actions.filter((a) => a.status === "failed");
    // Moved/recycled paths can't stay multi-selected. Unconditional — a
    // whole-folder Clean Duplicates that touches selected files prunes too.
    vpPruneSelection(done.map((a) => a.file_path));
    vpRenderPlan();
    vpSetText(
      "vp-plan-summary",
      `${done.length} done${failed.length ? `, ${failed.length} failed` : ""}` +
        (isDupes && res?.reclaimable_bytes
          ? ` · freed ${formatBytesLocal(res.reclaimable_bytes)}`
          : "") +
        (res?.was_cancelled ? " · cancelled" : ""),
    );

    // Only mark dirty when a refresh is already in flight — that call would be
    // dropped by the early-return guard. Setting it unconditionally would make
    // the refresh below re-dispatch itself from its own `finally`, walking the
    // whole library twice on every apply.
    if (state.varPackagesLoading) {
      state.varPackagesDirty = true;
    } else {
      await refreshVarPackagesFromFolder({ forceRescan: true });
    }
    const staleCount = done.filter((a) => a.indexed).length;
    if (staleCount) showVarPackagesDbStale(staleCount);
  } catch (err) {
    vpSetText("vp-plan-summary", `Failed: ${String(err)}`);
    addLog(`VAR Packages: ${String(err)}`);
  } finally {
    vpBusy(false);
    // The plan is gone, so Apply cannot run twice.
    const apply = $("vp-plan-apply");
    if (apply) apply.disabled = true;
  }
}

/// The apply path never writes to the database (by design — the rows stay as a
/// recovery reference), so moved/removed packages now show as "Missing" in
/// Database mode. Rebuilding genuinely repairs it: package_id is the primary key
/// and is the file stem, so the upsert just corrects file_path.
function showVarPackagesDbStale(count) {
  state.varPackagesDbStale = true;
  vpSetText(
    "var-packages-db-stale-text",
    `${count} indexed package(s) changed on disk. The database still points at their old paths — ` +
      `rebuild it so Database mode and downloads stay accurate.`,
  );
  $("var-packages-db-stale")?.classList.remove("hidden");
}

// =====================================================================
// Delete-confirmation modal with an OPT-IN reverse-dependency scan. Opening
// is cheap; the "Scan for usage" button runs start_dependency_check_task
// (a whole-library archive read) only when clicked, so the user learns which
// other packages reference this one before recycling it. Reuses the existing
// reverse-dependency backend — no new Rust. Mirrors the DEP_SCAN modal shape.
// =====================================================================
const VP_DELETE = {
  filePath: null,
  packageId: null,
  fileName: null,
  taskId: null,
  running: false,
  cancelled: false,
  resolve: null,
  // Normalized dependency-scan rows (vpDdepNormalize). Checkbox state lives in
  // the DOM and is read once at confirm — no mirror state to drift.
  deps: null,
  // Raw usage-scan matches, kept so the row right-click menu can resolve an
  // index back to a package_id + file_path.
  dependents: null,
};

const VP_DDEP_TASK_CMD = "start_delete_dependency_scan_task";
const VP_DDEP_RESULT_FIELD = "delete_dependency_scan_result";

function vpDeleteSetText(id, v) {
  const el = $(id);
  if (el) el.textContent = String(v ?? "");
}

// The AddonPackages root(s) the usage scan walks: the Settings "VAR library
// folder" + its additional dirs (the downloadVars context), deduped
// case-insensitively. NO var-details fallback (unlike depLibraryDirs) — the
// roots must come from the Settings config, or the modal hints to configure it.
function vpDeleteRootDirs() {
  const raw = [
    ($("settings-library-folder")?.value || "").trim(),
    ...getAdditionalDirs("downloadVars"),
  ].filter(Boolean);
  const seen = new Set();
  const out = [];
  for (const dir of raw) {
    const key = dir.toLowerCase();
    if (!seen.has(key)) {
      seen.add(key);
      out.push(dir);
    }
  }
  return out;
}

// Populates the modal's root selector from the configured library roots.
// Value "" means "all roots"; a path value means scan just that root. When no
// root is configured, disables the selector + Scan and hints toward Settings.
// Lists every configured VAR library root (read-only) that the usage scan will
// cover — primary + additional folders from Settings — so the user sees the
// full scope. The folders are changed in Settings (the link navigates there).
function vpDeleteRenderRoot() {
  const listEl = $("vp-delete-root-list");
  const scanBtn = $("vp-delete-scan-button");
  const roots = vpDeleteRootDirs();
  const depScanBtn = $("vp-delete-dep-scan-button");
  if (roots.length === 0) {
    if (listEl) {
      listEl.innerHTML = `<div class="vp-delete-root-empty">${escapeHtml(t("varPackagesDeleteRootUnset"))}</div>`;
    }
    if (scanBtn) scanBtn.disabled = true;
    if (depScanBtn) depScanBtn.disabled = true;
    const summary = $("vp-delete-scan-summary");
    if (summary) {
      summary.textContent = t("varPackagesDeleteNoRoots");
      summary.classList.remove("hidden", "is-safe");
      summary.classList.add("is-warn");
    }
    return;
  }
  if (scanBtn) scanBtn.disabled = false;
  if (depScanBtn) depScanBtn.disabled = false;
  if (listEl) {
    listEl.innerHTML = roots
      .map(
        (dir) =>
          `<div class="vp-delete-root-item" title="${escapeAttribute(dir)}">${escapeHtml(dir)}</div>`,
      )
      .join("");
  }
}

// Opens the modal and resolves true (proceed to delete) / false (cancel).
function vpDeleteModalOpen(filePath) {
  return new Promise((resolve) => {
    VP_DELETE.filePath = filePath;
    VP_DELETE.fileName = filePath.split(/[\\/]/).pop() || filePath;
    VP_DELETE.packageId = deriveTargetPackageIdFromPath(filePath);
    VP_DELETE.taskId = null;
    VP_DELETE.running = false;
    VP_DELETE.cancelled = false;
    VP_DELETE.resolve = resolve;
    VP_DELETE.deps = null;
    VP_DELETE.dependents = null;

    vpDeleteSetText("vp-delete-title", t("varPackagesDeleteTitle"));
    vpDeleteSetText("vp-delete-message", t("varPackagesDeleteMessage", VP_DELETE.fileName));
    vpDeleteSetText("vp-delete-scan-button", t("varPackagesDeleteScan"));
    vpDeleteSetText("vp-delete-dep-scan-button", t("varPackagesDeleteDepScan"));
    vpDeleteSetText("vp-delete-usage-header", t("varPackagesDeleteUsageHeader"));
    vpDeleteSetText("vp-delete-dep-header", t("varPackagesDeleteDepsHeader"));
    vpDeleteSetText("vp-delete-dep-hint", t("varPackagesDeleteDepsHint"));
    vpDeleteSetText("vp-delete-confirm", t("varPackagesDeleteConfirm"));
    // Reset the scan UI — a prior open must not leak its results.
    const deep = $("vp-delete-deep");
    if (deep) deep.checked = false;
    const summary = $("vp-delete-scan-summary");
    if (summary) {
      summary.classList.add("hidden");
      summary.classList.remove("is-safe", "is-warn");
      summary.textContent = "";
    }
    const list = $("vp-delete-scan-list");
    if (list) list.innerHTML = "";
    $("vp-delete-usage-header")?.classList.add("hidden");
    $("vp-delete-dep-header")?.classList.add("hidden");
    $("vp-delete-dep-hint")?.classList.add("hidden");
    const depSummary = $("vp-delete-dep-summary");
    if (depSummary) {
      depSummary.classList.add("hidden");
      depSummary.classList.remove("is-safe", "is-warn");
      depSummary.textContent = "";
    }
    const depList = $("vp-delete-dep-list");
    if (depList) depList.innerHTML = "";
    vpDeleteSetText("vp-delete-root-label", t("varPackagesDeleteRootLabel"));
    vpDeleteSetText("vp-delete-root-settings", t("varPackagesDeleteChangeInSettings"));
    vpDeleteModalBusy(false);
    // After Busy(false), which force-enables the scan button — so a "no roots"
    // state can still disable it.
    vpDeleteRenderRoot();
    $("vp-delete-backdrop")?.classList.remove("hidden");
  });
}

// Resolves `null` (cancelled) or `{ extraPaths }` — the checked dependencies'
// file paths, read from the DOM before the backdrop hides.
function vpDeleteModalClose(result) {
  if (VP_DELETE.running) return; // Cancel the scan first — see vpDeleteModalBusy.
  const payload = result ? { extraPaths: vpDdepCheckedPaths() } : null;
  $("vp-delete-backdrop")?.classList.add("hidden");
  const resolve = VP_DELETE.resolve;
  VP_DELETE.resolve = null;
  if (resolve) resolve(payload);
}

function vpDeleteModalBusy(busy) {
  VP_DELETE.running = busy;
  $("vp-delete-scan-progress")?.classList.toggle("hidden", !busy);
  for (const id of ["vp-delete-scan-button", "vp-delete-deep", "vp-delete-dep-scan-button", "vp-delete-confirm", "vp-delete-root-settings"]) {
    const el = $(id);
    if (el) el.disabled = busy;
  }
  // While a scan runs, Cancel cancels the scan (not the modal).
  vpDeleteSetText("vp-delete-cancel", busy ? t("varPackagesDeleteCancelScan") : t("varPackagesDeleteCancel"));
}

// The opt-in reverse-dependency scan. Invoked only from the Scan button.
async function vpDeleteRunScan() {
  if (!invoke || VP_DELETE.running) return;
  const summary = $("vp-delete-scan-summary");
  const list = $("vp-delete-scan-list");
  const showSummary = (text, cls) => {
    if (!summary) return;
    summary.textContent = text;
    summary.classList.remove("hidden", "is-safe", "is-warn");
    if (cls) summary.classList.add(cls);
  };

  const roots = vpDeleteRootDirs();
  if (roots.length === 0 || !VP_DELETE.packageId) {
    showSummary(t("varPackagesDeleteNoRoots"), "is-warn");
    if (list) list.innerHTML = "";
    return;
  }
  // Always scan every configured root (folder = first, additional = rest) —
  // a referencing package could live in any root.
  const folder = roots[0];
  const additional = roots.slice(1);
  const deep = !!$("vp-delete-deep")?.checked;

  VP_DELETE.cancelled = false;
  vpDeleteModalBusy(true);
  const bar = $("vp-delete-scan-bar");
  if (bar) bar.style.width = "0%";
  vpDeleteSetText("vp-delete-scan-message", "");
  if (summary) summary.classList.add("hidden");
  if (list) list.innerHTML = "";

  try {
    const handle = await invoke("start_dependency_check_task", {
      request: {
        folder,
        additional_folders: additional,
        target_package_id: VP_DELETE.packageId,
        deep_scan: deep,
      },
    });
    VP_DELETE.taskId = handle?.id ?? null;
    if (VP_DELETE.taskId == null) throw new Error("task did not start");

    let result = null;
    for (;;) {
      await new Promise((r) => setTimeout(r, 300));
      if (VP_DELETE.cancelled) break;
      const payload = await invoke("get_task_progress", { taskId: VP_DELETE.taskId });
      if (!payload) break;
      const pct = Math.round(Math.max(0, Math.min(1, Number(payload.progress ?? 0))) * 100);
      if (bar) bar.style.width = `${pct}%`;
      vpDeleteSetText("vp-delete-scan-message", payload.message ?? "");
      if (payload.error) throw new Error(String(payload.error));
      if (payload.done) {
        result = payload.dependency_check_result ?? null;
        break;
      }
    }
    // Only render on a real result — never show the green "safe to delete"
    // summary off a null/unexpected payload before a destructive action.
    if (!VP_DELETE.cancelled && result) vpDeleteRenderScan(result);
  } catch (err) {
    showSummary(String(err), "is-warn");
    if (list) list.innerHTML = "";
  } finally {
    const id = VP_DELETE.taskId;
    VP_DELETE.taskId = null;
    if (id != null) {
      try {
        await invoke("clear_task", { taskId: id });
      } catch (_e) {
        /* task may already be gone */
      }
    }
    vpDeleteModalBusy(false);
  }
}

// Renders the dependents list. Warn-but-allow: Delete stays enabled either way.
function vpDeleteRenderScan(res) {
  // Label the section — the dependency list below may coexist with it.
  $("vp-delete-usage-header")?.classList.remove("hidden");
  const summary = $("vp-delete-scan-summary");
  const list = $("vp-delete-scan-list");
  const dependents = Array.isArray(res?.dependents) ? res.dependents : [];
  if (summary) {
    summary.classList.remove("hidden", "is-safe", "is-warn");
    summary.textContent = dependents.length === 0
      ? t("varPackagesDeleteNoUsage")
      : t("varPackagesDeleteUsedBy", dependents.length);
    summary.classList.add(dependents.length === 0 ? "is-safe" : "is-warn");
  }
  if (!list) return;
  // Kept for the row right-click menu, which resolves data-vpu-idx back to a
  // match so it can act on that package's file_path.
  VP_DELETE.dependents = dependents;
  if (dependents.length === 0) {
    list.innerHTML = "";
    return;
  }
  list.innerHTML = dependents
    .map((d, i) => {
      const hasMeta = d.meta_match;
      const refCount = Array.isArray(d.deep_refs) ? d.deep_refs.length : 0;
      const badges = [];
      if (hasMeta) badges.push(`<span class="dc-badge is-meta">meta.json</span>`);
      if (refCount > 0) badges.push(`<span class="dc-badge is-deep">${refCount} ref${refCount === 1 ? "" : "s"}</span>`);
      return `
        <li class="vp-delete-dep-row" data-vpu-idx="${i}">
          <span class="vp-delete-dep-name">${escapeHtml(d.package_id || "")}</span>
          ${badges.join("")}
          ${d.creator_name ? `<span class="vp-delete-dep-creator">By ${escapeHtml(d.creator_name)}</span>` : ""}
        </li>`;
    })
    .join("");
}

// --- "Scan dependencies": the forward scan. One row per dependency BASE (the
// exclusivity verdict is per family); checking a row deletes every local file
// of that family. Warn-but-allow throughout — shared deps stay deletable.

// The opt-in dependency scan. Structural clone of vpDeleteRunScan: same task
// polling, same shared progress card — only one scan runs at a time.
async function vpDeleteRunDepScan() {
  if (!invoke || VP_DELETE.running) return;
  const summary = $("vp-delete-dep-summary");
  const list = $("vp-delete-dep-list");
  const showSummary = (text, cls) => {
    if (!summary) return;
    summary.textContent = text;
    summary.classList.remove("hidden", "is-safe", "is-warn");
    if (cls) summary.classList.add(cls);
  };

  const roots = vpDeleteRootDirs();
  if (roots.length === 0 || !VP_DELETE.filePath) {
    showSummary(t("varPackagesDeleteNoRoots"), "is-warn");
    if (list) list.innerHTML = "";
    return;
  }
  const folder = roots[0];
  const additional = roots.slice(1);

  VP_DELETE.cancelled = false;
  vpDeleteModalBusy(true);
  const bar = $("vp-delete-scan-bar");
  if (bar) bar.style.width = "0%";
  vpDeleteSetText("vp-delete-scan-message", "");
  // Clear only this section — the usage-scan results stay put.
  if (summary) summary.classList.add("hidden");
  if (list) list.innerHTML = "";
  $("vp-delete-dep-header")?.classList.add("hidden");
  $("vp-delete-dep-hint")?.classList.add("hidden");
  VP_DELETE.deps = null;
  // The cleared list has no checked rows — the confirm label must not keep
  // advertising deps from a previous scan.
  vpDdepUpdateConfirmLabel();

  try {
    const handle = await invoke(VP_DDEP_TASK_CMD, {
      request: {
        folder,
        additional_folders: additional,
        target_var_path: VP_DELETE.filePath,
      },
    });
    VP_DELETE.taskId = handle?.id ?? null;
    if (VP_DELETE.taskId == null) throw new Error("task did not start");

    let result = null;
    for (;;) {
      await new Promise((r) => setTimeout(r, 300));
      if (VP_DELETE.cancelled) break;
      const payload = await invoke("get_task_progress", { taskId: VP_DELETE.taskId });
      if (!payload) break;
      const pct = Math.round(Math.max(0, Math.min(1, Number(payload.progress ?? 0))) * 100);
      if (bar) bar.style.width = `${pct}%`;
      vpDeleteSetText("vp-delete-scan-message", payload.message ?? "");
      if (payload.error) throw new Error(String(payload.error));
      if (payload.done) {
        result = payload[VP_DDEP_RESULT_FIELD] ?? null;
        break;
      }
    }
    // Only render on a real result — a cancelled/partial scan must not
    // pre-check deps off incomplete usage counts before a destructive action.
    if (!VP_DELETE.cancelled && result && !result.was_cancelled) {
      VP_DELETE.deps = vpDdepNormalize(result);
      vpDeleteRenderDeps(result);
    }
  } catch (err) {
    showSummary(String(err), "is-warn");
    if (list) list.innerHTML = "";
  } finally {
    const id = VP_DELETE.taskId;
    VP_DELETE.taskId = null;
    if (id != null) {
      try {
        await invoke("clear_task", { taskId: id });
      } catch (_e) {
        /* task may already be gone */
      }
    }
    vpDeleteModalBusy(false);
  }
}

// Maps the backend response to display rows and applies the display order:
// installed before not-installed, then ascending shared-user count (exclusive
// = 0 naturally first), then base A–Z.
function vpDdepNormalize(res) {
  const deps = Array.isArray(res?.dependencies) ? res.dependencies : [];
  const out = deps.map((d) => {
    const files = Array.isArray(d.local_files) ? d.local_files : [];
    return {
      base: String(d.package_base || ""),
      direct: !!d.direct,
      files,
      installed: files.length > 0,
      sizeBytes: files.reduce((s, f) => s + (Number(f.size_bytes) || 0), 0),
      usedBy: Array.isArray(d.used_by) ? d.used_by : [],
      usedByTotal: Number(d.used_by_total) || 0,
    };
  });
  const key = (e) => e.base.toLowerCase();
  out.sort((a, b) => {
    if (a.installed !== b.installed) return a.installed ? -1 : 1;
    if (a.usedByTotal !== b.usedByTotal) return a.usedByTotal - b.usedByTotal;
    return key(a) < key(b) ? -1 : key(a) > key(b) ? 1 : 0;
  });
  return out;
}

// Renders the dependency section from VP_DELETE.deps. Exclusive installed deps
// are pre-checked; shared and not-installed start unchecked.
function vpDeleteRenderDeps(res) {
  const summary = $("vp-delete-dep-summary");
  const list = $("vp-delete-dep-list");
  const deps = Array.isArray(VP_DELETE.deps) ? VP_DELETE.deps : [];
  $("vp-delete-dep-header")?.classList.remove("hidden");
  const anyInstalled = deps.some((d) => d.installed);
  $("vp-delete-dep-hint")?.classList.toggle("hidden", !anyInstalled);

  if (summary) {
    summary.classList.remove("hidden", "is-safe", "is-warn");
    if (deps.length === 0) {
      summary.textContent = t("varPackagesDeleteDepsNone");
      summary.classList.add("is-safe");
    } else {
      const ex = deps.filter((d) => d.installed && d.usedByTotal === 0).length;
      const sh = deps.filter((d) => d.installed && d.usedByTotal > 0).length;
      const miss = deps.filter((d) => !d.installed).length;
      let text = t("varPackagesDeleteDepsSummary", ex, sh, miss);
      const errors = Number(res?.scan_errors) || 0;
      if (errors > 0) text += t("varPackagesDeleteDepsScanErrors", errors);
      summary.textContent = text;
      // Unreadable archives can under-count sharing — never show green then.
      summary.classList.add(sh === 0 && errors === 0 ? "is-safe" : "is-warn");
    }
  }
  if (!list) return;
  list.innerHTML = deps
    .map((d, i) => {
      const metaParts = [];
      if (d.installed) {
        metaParts.push(escapeHtml(d.files[0]?.file_path || ""));
        if (d.files.length > 1) {
          metaParts.push(escapeHtml(t("varPackagesDeleteDepsMoreFiles", d.files.length - 1)));
        }
        if (d.files.some((f) => f.disabled)) {
          metaParts.push(escapeHtml(t("varPackagesDeleteDepsDisabled")));
        }
      } else {
        metaParts.push(escapeHtml(t("varPackagesDeleteDepsMissing")));
      }
      const chips = [];
      if (!d.installed) {
        chips.push(`<span class="chip vp-ddep-missing">${escapeHtml(t("varPackagesDeleteDepsMissing"))}</span>`);
      } else if (d.usedByTotal === 0) {
        chips.push(`<span class="chip vp-ddep-exclusive">${escapeHtml(t("varPackagesDeleteDepsExclusive"))}</span>`);
      } else {
        chips.push(
          `<button type="button" class="chip vp-ddep-shared" data-ddep-toggle="${i}" aria-expanded="false">${escapeHtml(t("varPackagesDeleteDepsShared", d.usedByTotal))} ▸</button>`,
        );
      }
      if (!d.direct) {
        chips.push(`<span class="chip">${escapeHtml(t("varPackagesDeleteDepsTransitive"))}</span>`);
      }
      const preChecked = d.installed && d.usedByTotal === 0;
      const row = `
        <label class="check-row vp-ddep-row${d.installed ? "" : " is-missing"}" data-ddep-row="${i}">
          <input type="checkbox" class="vp-ddep-check" data-ddep-idx="${i}"
            ${preChecked ? "checked" : ""} ${d.installed ? "" : "disabled"} />
          <span class="vp-ddep-main">
            <span class="vp-ddep-name">${escapeHtml(d.base)}</span>
            <span class="vp-ddep-meta">${metaParts.join(" · ")}</span>
          </span>
          <span class="vp-ddep-size">${d.installed ? escapeHtml(formatBytesLocal(d.sizeBytes)) : ""}</span>
          <span class="vp-ddep-chips">${chips.join("")}</span>
        </label>`;
      // The users panel is a SIBLING of the row label, so clicks inside it
      // can't toggle the checkbox.
      let users = "";
      if (d.usedBy.length > 0) {
        const userChips = d.usedBy
          .map((u) => `<span class="chip">${escapeHtml(u.package_id || "")}</span>`)
          .join("");
        const more =
          d.usedByTotal > d.usedBy.length
            ? `<span class="chip">${escapeHtml(t("varPackagesDeleteDepsMoreUsers", d.usedByTotal - d.usedBy.length))}</span>`
            : "";
        users = `<div class="vp-ddep-users hidden" data-ddep-users="${i}">${userChips}${more}</div>`;
      }
      return row + users;
    })
    .join("");
  vpDdepUpdateConfirmLabel();
  // Pre-checked exclusive rows may already satisfy other rows' effective
  // exclusivity — compute the chips now, not only on the first change event.
  vpDdepRefreshEffective();
}

function vpDdepCheckedEntries() {
  if (!Array.isArray(VP_DELETE.deps)) return [];
  return [...document.querySelectorAll("#vp-delete-dep-list .vp-ddep-check:checked")]
    .map((c) => VP_DELETE.deps[Number(c.dataset.ddepIdx)])
    .filter(Boolean);
}

// The checked dependencies' file paths — deduped case-insensitively, never
// including the main package's own path (it is deleted regardless).
function vpDdepCheckedPaths() {
  const seen = new Set([String(VP_DELETE.filePath || "").toLowerCase()]);
  const out = [];
  for (const entry of vpDdepCheckedEntries()) {
    for (const f of entry.files) {
      const k = String(f.file_path).toLowerCase();
      if (!seen.has(k)) {
        seen.add(k);
        out.push(f.file_path);
      }
    }
  }
  return out;
}

function vpDdepUpdateConfirmLabel() {
  const n = vpDdepCheckedEntries().length;
  vpDeleteSetText(
    "vp-delete-confirm",
    n ? t("varPackagesDeleteConfirmWithDeps", n) : t("varPackagesDeleteConfirm"),
  );
}

// Chip-swap only, never a reorder (rows jumping under the cursor is worse than
// a stale order): a shared dep whose every user is itself checked for deletion
// (or is the base of a checked row) reads as effectively exclusive. Requires
// the full user list (cap not hit) — a truncated list can't prove anything.
function vpDdepRefreshEffective() {
  if (!Array.isArray(VP_DELETE.deps)) return;
  const checkedBases = new Set(vpDdepCheckedEntries().map((e) => e.base.toLowerCase()));
  const baseOf = (id) => String(id).replace(/\.(\d+|latest)$/i, "").toLowerCase();
  for (const btn of document.querySelectorAll("#vp-delete-dep-list [data-ddep-toggle]")) {
    const entry = VP_DELETE.deps[Number(btn.dataset.ddepToggle)];
    if (!entry) continue;
    const provable = entry.usedBy.length > 0 && entry.usedBy.length === entry.usedByTotal;
    const eff =
      provable && entry.usedBy.every((u) => checkedBases.has(baseOf(u.package_id)));
    btn.classList.toggle("vp-ddep-eff", eff);
    btn.textContent = eff
      ? `${t("varPackagesDeleteDepsEffExclusive")} ▸`
      : `${t("varPackagesDeleteDepsShared", entry.usedByTotal)} ▸`;
  }
}

/// Sends one package to the Recycle Bin, after a confirm modal that can
/// optionally scan for reverse dependencies (which other packages reference
/// this one) on demand. The Recycle Bin is what makes a wrong click recoverable.
///
/// Resolves the paths actually recycled (empty when cancelled or failed). The
/// grid callers ignore it; the Download Dependencies modal needs it to know which
/// rows to flip, and can't re-derive the list because the modal may also recycle
/// the dependencies the user ticked.
async function vpDeleteOne(filePath, trigger) {
  if (!invoke || !filePath) return [];
  const fileName = filePath.split(/[\\/]/).pop() || filePath;

  const ok = await vpDeleteModalOpen(filePath);
  if (!ok) return [];
  const extraPaths = Array.isArray(ok.extraPaths) ? ok.extraPaths : [];

  if (trigger) trigger.disabled = true;
  if (extraPaths.length === 0) {
    try {
      const freed = await invoke("delete_var_package", { filePath });
      addLog(`Deleted ${fileName} (${formatBytesLocal(freed)} freed) — restorable from the Recycle Bin.`);
      // A deleted file can't stay multi-selected.
      vpPruneSelection([filePath]);
      // The backend already dropped its folder cache; this repopulates the grid.
      await vpRefreshAfterMutation();
      return [filePath];
    } catch (err) {
      // Re-enable so a failure (locked file, no Recycle Bin on the volume) can be
      // retried once the user fixes the cause.
      if (trigger) trigger.disabled = false;
      addLog(`Delete failed: ${String(err)}`);
      await showAppConfirm(`Could not delete ${fileName}.\n\n${String(err)}`);
      return [];
    }
  }

  // Multi-file: the main package first, then the checked dependencies. Same
  // continue-on-failure loop as bulk delete — every file is independently
  // Recycle-Bin-recoverable, so one locked dep must not strand the rest.
  const targets = [filePath, ...extraPaths].map((p) => ({
    file_path: p,
    file_name: p.split(/[\\/]/).pop() || p,
  }));
  const toast = showToast(
    t("varPackagesBulkDeleteProgress", 0, targets.length, "0 B"),
    "info",
    0,
  );
  const { removed, failures, freed } = await vpDeleteFilesSequential(targets, (i, n, f) =>
    toast.update(t("varPackagesBulkDeleteProgress", i, n, formatBytesLocal(f))),
  );
  for (const f of failures) addLog(`Delete: ${f.name} — ${f.err}`);
  if (failures.length) {
    toast.update(t("varPackagesBulkDeleteSomeFailed", removed.length, failures.length), "error");
    // Re-enable so the row stays actionable after the refresh below.
    if (trigger) trigger.disabled = false;
  } else {
    toast.update(t("varPackagesBulkDeleteDone", removed.length, formatBytesLocal(freed)), "success");
  }
  toast.dismiss(6000);
  vpPruneSelection(removed.map((r) => r.file_path));
  await vpRefreshAfterMutation();
  return removed.map((r) => r.file_path);
}

/// Moves one VAR into its creator folder under the Settings VAR library root
/// (AddonPackages root; additional folders are ignored). Creates the folder if
/// it doesn't exist. Refuses to overwrite a same-named file at the destination.
async function vpMoveToCreatorFolder(filePath) {
  if (!invoke || !filePath) return;
  const root = ($("settings-library-folder")?.value || "").trim();
  if (!root) {
    showToast(t("varPackagesMoveNoRoot"), "error");
    return;
  }
  const fileName = filePath.split(/[\\/]/).pop() || filePath;
  try {
    const dest = await invoke("move_var_to_creator_folder", { filePath, rootDir: root });
    const destName = String(dest || "").split(/[\\/]/).slice(-2).join("\\");
    showToast(t("varPackagesMoveDone", destName), "success");
    addLog(`Moved ${fileName} → ${dest}`);
    vpPruneSelection([filePath]);
    await vpRefreshAfterMutation();
  } catch (err) {
    showToast(`Move failed: ${String(err)}`, "error");
    addLog(`Move to creator folder (${fileName}): ${String(err)}`);
  }
}

// --- Export scene images ----------------------------------------------------
// Pulls a VAR's preview image out of the archive and writes it next to the .var
// as <var name>.<ext>, so a loose thumbnail sits beside each package — handy for
// identifying VARs whose names are in another language.

/// Single VAR (grid right-click). Reports the outcome with a quiet toast — no
/// Explorer window.
async function exportOneSceneImage(filePath, packageId) {
  if (!invoke || !filePath) return;
  const name = packageId || filePath.split(/[\\/]/).pop() || filePath;
  const imgName = (p) => (p ? p.split(/[\\/]/).pop() : "");
  try {
    const res = await invoke("export_var_scene_image", { packagePath: filePath, overwrite: false });
    const status = res?.status;
    if (status === "exported") {
      showToast(`Saved ${imgName(res.output_path)}`, "success");
    } else if (status === "exists") {
      // The image is there — that's the goal, so green.
      showToast(`Image already exists: ${imgName(res.output_path)}`, "success");
    } else if (status === "no_image") {
      // Not a failure: most VARs are clothing/hair/morphs and simply aren't
      // scenes. Nothing was written, and that is the correct outcome.
      showToast(`No scene image in ${name} — nothing exported.`, "info");
    } else {
      showToast(`Couldn't export image: ${res?.detail || "unknown error"}`, "error");
    }
  } catch (err) {
    showToast(`Export failed: ${String(err)}`, "error");
  }
}

// --- Bulk actions on the multi-selection ------------------------------------
// All three loop the existing single-item commands / plan flow from JS rather
// than adding batch Rust commands: delete inherits the recycle-bin probe and
// sidecar cascade, export keeps per-file status granularity, and organize
// reuses the reviewed-plan apply path (selected indices) unchanged.

/// Sequential Recycle-Bin deletes with a per-drive circuit breaker: the first
/// "no Recycle Bin" failure marks the whole drive dead so the remaining paths
/// on it skip with one clear reason instead of N identical errors. `targets`
/// are `{ file_path, file_name, ... }` objects returned in `removed`/`failures`
/// verbatim so callers keep their own bookkeeping (e.g. `.indexed`).
async function vpDeleteFilesSequential(targets, onProgress) {
  const removed = [];
  const failures = [];
  let freed = 0;
  const deadDrives = new Set();
  for (let i = 0; i < targets.length; i++) {
    const tgt = targets[i];
    const drive = /^[a-z]:/i.test(tgt.file_path)
      ? tgt.file_path.slice(0, 2).toLowerCase()
      : "";
    if (drive && deadDrives.has(drive)) {
      failures.push({ name: tgt.file_name, err: "skipped — volume has no Recycle Bin" });
      continue;
    }
    try {
      freed += Number(await invoke("delete_var_package", { filePath: tgt.file_path })) || 0;
      removed.push(tgt);
    } catch (err) {
      const msg = String(err);
      if (msg.includes("already gone")) {
        // Deleted externally mid-batch — the goal is achieved.
        removed.push(tgt);
      } else {
        failures.push({ name: tgt.file_name, err: msg });
        if (drive && msg.includes("no Recycle Bin")) deadDrives.add(drive);
      }
    }
    onProgress?.(i + 1, targets.length, freed);
  }
  return { removed, failures, freed };
}

/// Sends every selected package to the Recycle Bin, one confirm up front.
/// Sequential on purpose — each delete is an SHFileOperation; parallelism
/// buys nothing and interleaves failure toasts.
async function vpBulkDelete() {
  if (!invoke || state.vpBulkRunning || state.vpSelected.size === 0) return;
  const targets = [...state.vpSelected.values()];
  let totalBytes = 0;
  for (const tgt of targets) totalBytes += tgt.size_bytes;

  const ok = await showAppConfirm(
    t("varPackagesBulkDeleteConfirm", targets.length, formatBytesLocal(totalBytes)),
  );
  if (!ok) return;

  state.vpBulkRunning = true;
  renderVpSelectionBar();
  const toast = showToast(
    t("varPackagesBulkDeleteProgress", 0, targets.length, "0 B"),
    "info",
    0,
  );
  let removed = [];
  let failures = [];
  let freed = 0;
  try {
    ({ removed, failures, freed } = await vpDeleteFilesSequential(targets, (i, n, f) =>
      toast.update(t("varPackagesBulkDeleteProgress", i, n, formatBytesLocal(f))),
    ));
  } finally {
    state.vpBulkRunning = false;
  }

  vpPruneSelection(removed.map((r) => r.file_path));
  for (const f of failures) addLog(`Bulk delete: ${f.name} — ${f.err}`);
  if (failures.length) {
    toast.update(t("varPackagesBulkDeleteSomeFailed", removed.length, failures.length), "error");
  } else {
    toast.update(t("varPackagesBulkDeleteDone", removed.length, formatBytesLocal(freed)), "success");
  }
  toast.dismiss(6000);

  await vpRefreshAfterMutation();
  // Deleted rows that were indexed leave stale paths in the database.
  const stale = removed.filter((r) => r.indexed).length;
  if (stale) showVarPackagesDbStale(stale);
}

/// Exports the preview image of every selected package (skips existing
/// sidecars). No confirm — non-destructive.
async function vpBulkExportImages() {
  if (!invoke || state.vpBulkRunning || state.vpSelected.size === 0) return;
  const targets = [...state.vpSelected.values()];
  state.vpBulkRunning = true;
  renderVpSelectionBar();
  const toast = showToast(t("varPackagesBulkExportProgress", 0, targets.length), "info", 0);
  const counts = { exported: 0, exists: 0, noImage: 0, failed: 0 };
  const notes = [];
  try {
    for (let i = 0; i < targets.length; i++) {
      const tgt = targets[i];
      try {
        const res = await invoke("export_var_scene_image", {
          packagePath: tgt.file_path,
          overwrite: false,
        });
        const status = res?.status;
        if (status === "exported") counts.exported += 1;
        else if (status === "exists") counts.exists += 1;
        else if (status === "no_image") counts.noImage += 1;
        else {
          counts.failed += 1;
          notes.push(`${tgt.file_name}: ${res?.detail || "failed"}`);
        }
      } catch (err) {
        // "that package is not on disk" covers stale DB paths and files
        // deleted externally mid-batch.
        counts.failed += 1;
        notes.push(`${tgt.file_name}: ${String(err)}`);
      }
      toast.update(t("varPackagesBulkExportProgress", i + 1, targets.length));
    }
  } finally {
    state.vpBulkRunning = false;
    renderVpSelectionBar();
  }

  const parts = [`${counts.exported} exported`];
  if (counts.exists) parts.push(`${counts.exists} already existed`);
  if (counts.noImage) parts.push(`${counts.noImage} had no scene image`);
  if (counts.failed) parts.push(`${counts.failed} failed`);
  const kind = counts.failed ? "error" : counts.noImage ? "info" : "success";
  toast.update(t("varPackagesBulkExportDone", parts.join(", "), targets.length), kind);
  toast.dismiss(6000);
  for (const n of notes) addLog(`Export Scene Images: ${n}`);
  // Nothing on the listing changed — the selection is kept.
}

/// Organize only the selected packages: runs the normal whole-folder plan,
/// with only the rows matching the selection pre-ticked for review.
// Clean Duplicates over the selection: the plan still covers every folder,
// but only the selected packages' rows start ticked (same as bulk Organize).
async function vpBulkClean() {
  if (state.vpBulkRunning || state.vpSelected.size === 0) return;
  const limitPaths = new Set([...state.vpSelected.keys()].map((p) => p.toLowerCase()));
  await vpStartPlan("clean_duplicates", {
    limitPaths,
    deepOverride: state.varPackagesScannedDeep !== false,
  });
}

async function vpBulkOrganize() {
  if (state.vpBulkRunning || state.vpSelected.size === 0) return;
  const limitPaths = new Set([...state.vpSelected.keys()].map((p) => p.toLowerCase()));
  await vpStartPlan("organize_by_creator", {
    limitPaths,
    // The committed depth — the selection was made against the committed
    // listing (see the deepOverride note in vpStartPlan).
    deepOverride: state.varPackagesScannedDeep !== false,
  });
}

// =====================================================================
// VAR Packages multi-select. Selection is keyed by file_path and stores an
// item snapshot (name/size/indexed) so the bar and bulk summaries can talk
// about items that have paged out of state.varPackagesItems. It survives
// pagination / search / filter changes and is cleared on mode switch and
// Scan Directory (different universes of paths).
// =====================================================================

function vpSelectableItem(item) {
  return Boolean(item?.file_path);
}

// A full copy: the details and selection panels describe selected packages
// even after they scroll out of the loaded rows.
function vpSnapshotItem(item) {
  return { ...item, size_bytes: Number(item.size_bytes) || 0, indexed: Boolean(item.indexed) };
}

function vpToggleSelect(item) {
  if (!vpSelectableItem(item)) return;
  if (state.vpSelected.has(item.file_path)) state.vpSelected.delete(item.file_path);
  else state.vpSelected.set(item.file_path, vpSnapshotItem(item));
  state.vpSelAnchor = item.file_path;
  vpSyncSelectionUi();
}

// Shift-click: ADDITIVE contiguous range from the anchor in current-page
// order. Deliberate deviation from setSelectionFromClick (dedup groups),
// which replaces on shift — here plain-click doesn't select (it opens
// details), so there is no "replace" gesture, and replacing would silently
// drop off-page selections. Anchor off-page → degrade to a plain toggle.
function vpSelectRangeTo(item) {
  const items = state.varPackagesItems ?? [];
  const anchorIdx = items.findIndex((it) => it.file_path === state.vpSelAnchor);
  const targetIdx = items.findIndex((it) => it.file_path === item.file_path);
  if (anchorIdx < 0 || targetIdx < 0) {
    vpToggleSelect(item);
    return;
  }
  const [lo, hi] = anchorIdx <= targetIdx ? [anchorIdx, targetIdx] : [targetIdx, anchorIdx];
  for (const it of items.slice(lo, hi + 1)) {
    if (vpSelectableItem(it)) state.vpSelected.set(it.file_path, vpSnapshotItem(it));
  }
  state.vpSelAnchor = item.file_path;
  vpSyncSelectionUi();
}

function vpSelectPage() {
  for (const it of state.varPackagesItems ?? []) {
    if (vpSelectableItem(it)) state.vpSelected.set(it.file_path, vpSnapshotItem(it));
  }
  vpSyncSelectionUi();
}

// Pages through the backend with the CURRENT search + filters to gather every
// matching item (not just the visible page). Both list commands cap `limit`
// at 1000, so we page in 1000s off the known total. Folder-mode pages are
// served from the in-memory scan cache, so this is cheap even for large sets.
async function vpFetchAllMatchingItems() {
  if (!invoke) return [];
  const total = Math.max(0, Number(state.varPackagesTotal ?? 0));
  if (total === 0) return [];
  const search = String(state.varPackagesFilter ?? "").trim() || null;
  const filters = serializeVarPackageFilters(state.varPackagesFilters);
  const PAGE = 1000;
  const out = [];
  const { inputDir, additionalInputDirs } = vpRoots();
  if (!inputDir) return out;
  for (let offset = 0; offset < total; offset += PAGE) {
    // Same sort as the visible grid: this walks the matching set in pages, so
    // a different ordering here would page over a differently-ordered list.
    const page = await invoke("list_var_packages", {
      inputDir,
      additionalInputDirs,
      offset,
      limit: PAGE,
      search,
      filters,
      ...varPackagesSortArgs(),
      forceRescan: false,
      deepScan: state.varPackagesScannedDeep !== false,
    });
    const items = Array.isArray(page?.items) ? page.items : [];
    out.push(...items);
    if (items.length < PAGE) break; // last (or only) page
  }
  return out;
}

// Select every package matching the current search + filters, across all
// pages. Fetches the full set first (see vpFetchAllMatchingItems), guarded
// against re-entrancy via vpBulkRunning so the bar buttons disable while it
// works.
async function vpSelectAll() {
  if (state.vpBulkRunning) return;
  state.vpBulkRunning = true;
  renderVpSelectionBar();
  try {
    const items = await vpFetchAllMatchingItems();
    for (const it of items) {
      if (vpSelectableItem(it)) state.vpSelected.set(it.file_path, vpSnapshotItem(it));
    }
  } catch (err) {
    addLog(`Select all: ${String(err)}`);
  } finally {
    state.vpBulkRunning = false;
    vpSyncSelectionUi();
  }
}

function vpClearSelection() {
  state.vpSelected.clear();
  state.vpSelAnchor = null;
  vpSyncSelectionUi();
}

// Drop the given paths from the selection (post-delete/move). Lowercased
// compare — NTFS paths are case-insensitive and plan rows may differ in case.
function vpPruneSelection(paths) {
  const gone = new Set((paths ?? []).map((p) => String(p).toLowerCase()));
  if (!gone.size) return;
  for (const key of [...state.vpSelected.keys()]) {
    if (gone.has(key.toLowerCase())) state.vpSelected.delete(key);
  }
  vpSyncSelectionUi();
}

// The bulk toolbar: only while two or more packages are selected (one is
// simply the package in the details panel, as in Backstage).
function renderVpSelectionBar() {
  const bar = $("var-packages-selection-bar");
  if (!bar) return;
  const n = state.vpSelected.size;
  bar.classList.toggle("hidden", n < 2);
  if (n < 2) return;

  let totalBytes = 0;
  for (const snap of state.vpSelected.values()) totalBytes += Number(snap.size_bytes) || 0;
  vpSetText("var-packages-selection-count", t("varPackagesSelCount", n, formatBytesLocal(totalBytes)));
  for (const id of [
    "var-packages-bulk-delete",
    "var-packages-bulk-organize",
    "var-packages-bulk-export",
    "var-packages-selection-clear",
    "lib-bulk-disable",
    "lib-bulk-enable",
    "lib-bulk-clean",
    "var-packages-select-all",
    "var-packages-select-page",
  ]) {
    const btn = $(id);
    if (btn) btn.disabled = state.vpBulkRunning;
  }
}

// Re-assert selection state onto whatever is currently in the DOM (called
// after every list render and after every selection mutation).
function vpSyncSelectionUi() {
  renderVpSelectionBar();
  const n = state.vpSelected.size;
  const grid = $("var-packages-grid");
  if (grid) {
    grid.classList.toggle("is-bulk", n > 1);
    grid.querySelectorAll(".lib-card[data-file-path]").forEach((card) => {
      const fp = card.getAttribute("data-file-path");
      const picked = state.vpSelected.has(fp);
      card.classList.toggle("is-picked", picked);
      card.classList.toggle("is-checked", picked && n > 1);
      card.classList.toggle("is-lead", n > 1 && state.vpLead === fp);
      card.setAttribute("aria-selected", String(picked));
    });
  }
  const tbody = $("var-packages-tbody");
  if (tbody) {
    tbody.querySelectorAll("tr[data-file-path]").forEach((row) => {
      const picked = state.vpSelected.has(row.getAttribute("data-file-path"));
      row.classList.toggle("is-picked", picked);
      const box = row.querySelector("[data-vp-select]");
      if (box) box.checked = picked;
    });
  }
  libRenderDetail();
}

function setupVarPackagesSelection() {
  for (const containerId of ["var-packages-grid", "var-packages-tbody"]) {
    const container = $(containerId);
    if (!container) continue;
    container.addEventListener("click", (event) => {
      const box = event.target.closest?.("[data-vp-select]");
      if (!box || box.disabled) return;
      // No preventDefault: vpSyncSelectionUi re-asserts .checked from state,
      // so the native toggle is irrelevant either way.
      const fp = box.getAttribute("data-vp-select");
      const item = (state.varPackagesItems ?? []).find((it) => it.file_path === fp);
      if (!item) return;
      if (event.shiftKey) vpSelectRangeTo(item);
      else vpToggleSelect(item);
    });
    // Shift-click would otherwise smear a text selection across table rows.
    container.addEventListener("mousedown", (event) => {
      if (event.shiftKey) event.preventDefault();
    });
  }

  $("var-packages-select-all")?.addEventListener("click", () => {
    vpSelectAll().catch((e) => addLog(`Select all: ${String(e)}`));
  });
  $("var-packages-select-page")?.addEventListener("click", () => vpSelectPage());
  $("var-packages-selection-clear")?.addEventListener("click", () => vpClearSelection());
  $("var-packages-bulk-delete")?.addEventListener("click", () => {
    vpBulkDelete().catch((e) => addLog(`Bulk delete: ${String(e)}`));
  });
  $("var-packages-bulk-organize")?.addEventListener("click", () => {
    vpBulkOrganize().catch((e) => addLog(`Bulk organize: ${String(e)}`));
  });
  $("var-packages-bulk-export")?.addEventListener("click", () => {
    vpBulkExportImages().catch((e) => addLog(`Bulk export: ${String(e)}`));
  });
}

// ============================================================
// VAR Packages — image gallery modal ("View images")
//
// Shows every JPG/PNG inside ONE .var as a thumbnail grid, excluding entries
// over an adjustable size limit. Two existing commands do all the work:
//
//   list_var_resources      -> every entry with its UNCOMPRESSED size, read
//                              from the zip central directory. No decompression,
//                              so the size filter costs nothing and changing
//                              the limit re-filters client-side with no rescan.
//   load_preview_image_data -> a ready `data:image/...;base64,...` string for
//                              one entry, fetched lazily per visible tile.
//
// No task/cancel scaffolding here (unlike the delete modal): that exists
// because the delete scan walks the whole library, whereas this reads a single
// archive's directory in one call.
// ============================================================

const VP_IMAGES = {
  filePath: null,
  // { internalPath, size } for every image entry, ascending by path. Populated
  // once per open; the MB limit filters this list, it never refetches.
  entries: [],
  // internalPath -> data URL. Cleared on close — this is the memory release.
  cache: new Map(),
  observer: null,
  queue: [],
  draining: false,
  token: 0,
};

// Hard cap on rendered tiles. Each loaded tile holds a base64 string ~1.33x the
// raw image, so an uncapped grid on a texture pack can pin hundreds of MB.
const VP_IMAGES_MAX_TILES = 300;
const VP_IMAGES_DEFAULT_MB = 2;
// MUST stay in sync with image_mime_type() in tasks.rs — it is the only
// vocabulary load_preview_image_data accepts. naming.rs also categorises
// tif/tiff/tga as textures, but those cannot be loaded, so they are not listed.
const VP_IMAGES_EXT_RE = /\.(jpe?g|png)$/i;

function vpImagesMaxBytes() {
  const raw = Number($("vp-images-max")?.value);
  const mb = Number.isFinite(raw) && raw > 0 ? raw : VP_IMAGES_DEFAULT_MB;
  return { mb, bytes: mb * 1024 * 1024 };
}

function vpImagesOpen(filePath) {
  const path = String(filePath ?? "");
  if (!path) return;
  // Bump the token so a slow list_var_resources from a previous open cannot
  // render into this one.
  VP_IMAGES.token += 1;
  const token = VP_IMAGES.token;
  vpImagesResetRuntime();
  VP_IMAGES.filePath = path;
  VP_IMAGES.entries = [];

  vpImagesSetText("vp-images-message", path.split(/[\\/]/).pop() || path);
  const summary = $("vp-images-summary");
  if (summary) {
    summary.textContent = t("varPackagesImagesLoading");
    summary.classList.remove("hidden");
  }
  const grid = $("vp-images-grid");
  if (grid) grid.innerHTML = "";
  $("vp-images-backdrop")?.classList.remove("hidden");

  invoke("list_var_resources", { varPath: path })
    .then((rows) => {
      if (token !== VP_IMAGES.token) return;
      VP_IMAGES.entries = (Array.isArray(rows) ? rows : [])
        .filter((r) => VP_IMAGES_EXT_RE.test(String(r?.internal_path ?? "")))
        .map((r) => ({ internalPath: String(r.internal_path), size: Number(r.size ?? 0) }))
        .sort((a, b) => a.internalPath.localeCompare(b.internalPath));
      vpImagesRender();
    })
    .catch((err) => {
      if (token !== VP_IMAGES.token) return;
      vpImagesSetText("vp-images-summary", t("varPackagesImagesError", String(err)));
      addLog(`View images: ${String(err)}`);
    });
}

function vpImagesSetText(id, value) {
  const el = $(id);
  if (!el) return;
  el.textContent = String(value ?? "");
  el.classList.remove("hidden");
}

function vpImagesRender() {
  const grid = $("vp-images-grid");
  const summary = $("vp-images-summary");
  if (!grid || !summary) return;

  // A re-render (limit changed) invalidates in-flight tile loads: their tiles
  // are about to be discarded. The cache survives, so nothing is refetched.
  vpImagesStopObserver();
  VP_IMAGES.queue = [];

  const { mb, bytes } = vpImagesMaxBytes();
  const total = VP_IMAGES.entries.length;
  const shown = VP_IMAGES.entries.filter((e) => e.size <= bytes);
  const skipped = total - shown.length;
  const tiles = shown.slice(0, VP_IMAGES_MAX_TILES);

  if (total === 0) {
    grid.innerHTML = "";
    summary.textContent = t("varPackagesImagesNone");
    summary.classList.remove("hidden");
    return;
  }
  if (shown.length === 0) {
    grid.innerHTML = "";
    summary.textContent = t("varPackagesImagesAllSkipped", total, mb);
    summary.classList.remove("hidden");
    return;
  }

  // Always state what was withheld — a filtered grid must never read as
  // "this is everything".
  let text = t("varPackagesImagesSummary", tiles.length, total);
  if (skipped > 0) text += t("varPackagesImagesSkipped", skipped, mb);
  if (shown.length > tiles.length) text += t("varPackagesImagesCapped", VP_IMAGES_MAX_TILES);
  summary.textContent = text;
  summary.classList.remove("hidden");

  grid.innerHTML = tiles
    .map((entry) => {
      const p = escapeAttribute(entry.internalPath);
      return (
        `<figure class="vp-images-tile is-loading" data-vp-img-path="${p}" ` +
        `title="${p} · ${escapeAttribute(formatBytesLocal(entry.size))}"></figure>`
      );
    })
    .join("");

  vpImagesStartObserver();
}

// Tiles load only once scrolled into view. This, plus the tile cap and the
// cache clear on close, is what bounds the modal's memory.
function vpImagesStartObserver() {
  const grid = $("vp-images-grid");
  if (!grid) return;
  if (typeof IntersectionObserver !== "function") {
    // No observer available — fall back to loading every rendered tile.
    grid.querySelectorAll("[data-vp-img-path]").forEach((el) => vpImagesEnqueue(el));
    return;
  }
  VP_IMAGES.observer = new IntersectionObserver(
    (items, obs) => {
      for (const item of items) {
        if (!item.isIntersecting) continue;
        obs.unobserve(item.target);
        vpImagesEnqueue(item.target);
      }
    },
    { root: grid, rootMargin: "200px" },
  );
  grid.querySelectorAll("[data-vp-img-path]").forEach((el) => VP_IMAGES.observer.observe(el));
}

function vpImagesStopObserver() {
  if (VP_IMAGES.observer) {
    VP_IMAGES.observer.disconnect();
    VP_IMAGES.observer = null;
  }
}

function vpImagesEnqueue(tile) {
  const path = tile?.dataset?.vpImgPath;
  if (!path) return;
  const cached = VP_IMAGES.cache.get(path);
  if (cached) {
    vpImagesPaint(tile, cached, path);
    return;
  }
  VP_IMAGES.queue.push(tile);
  vpImagesDrain().catch((e) => addLog(`View images: ${String(e)}`));
}

// Sequential (concurrency 1), matching the library thumbnails' "avoid opening
// 20 zip archives at once" rule. One failed entry must not stall the rest.
async function vpImagesDrain() {
  if (VP_IMAGES.draining) return;
  VP_IMAGES.draining = true;
  try {
    while (VP_IMAGES.queue.length) {
      const tile = VP_IMAGES.queue.shift();
      const path = tile?.dataset?.vpImgPath;
      // Dropped by a re-render or a close.
      if (!path || !tile.isConnected) continue;
      const token = VP_IMAGES.token;
      const packagePath = VP_IMAGES.filePath;
      let dataUrl = VP_IMAGES.cache.get(path);
      if (!dataUrl) {
        try {
          dataUrl = await invoke("load_preview_image_data", {
            packagePath,
            internalPath: path,
          });
        } catch (err) {
          addLog(`View images (${path}): ${String(err)}`);
          if (token === VP_IMAGES.token && tile.isConnected) {
            tile.classList.remove("is-loading");
            tile.classList.add("is-failed");
            tile.innerHTML = '<span class="material-symbols-outlined">broken_image</span>';
          }
          continue;
        }
        if (dataUrl) VP_IMAGES.cache.set(path, dataUrl);
      }
      if (token !== VP_IMAGES.token || !tile.isConnected) continue;
      vpImagesPaint(tile, dataUrl, path);
    }
  } finally {
    VP_IMAGES.draining = false;
  }
}

function vpImagesPaint(tile, dataUrl, path) {
  if (!tile || !dataUrl) return;
  tile.classList.remove("is-loading", "is-failed");
  const img = document.createElement("img");
  img.loading = "lazy";
  img.alt = path;
  img.src = dataUrl;
  tile.innerHTML = "";
  tile.appendChild(img);
}

function vpImagesResetRuntime() {
  vpImagesStopObserver();
  VP_IMAGES.queue = [];
  VP_IMAGES.cache.clear();
}

function vpImagesClose() {
  const backdrop = $("vp-images-backdrop");
  if (!backdrop || backdrop.classList.contains("hidden")) return;
  backdrop.classList.add("hidden");
  // Invalidate any in-flight load, then drop the grid and every decoded data
  // URL. Without the clear the base64 blobs live for the process lifetime.
  VP_IMAGES.token += 1;
  vpImagesResetRuntime();
  VP_IMAGES.entries = [];
  VP_IMAGES.filePath = null;
  const grid = $("vp-images-grid");
  if (grid) grid.innerHTML = "";
}

function setupVarPackagesImagesModal() {
  $("vp-images-close")?.addEventListener("click", () => vpImagesClose());
  $("vp-images-backdrop")?.addEventListener("click", (event) => {
    if (event.target === $("vp-images-backdrop")) vpImagesClose();
  });
  // Re-filter is pure client-side (sizes are already known), so debounce only
  // to avoid re-laying out the grid on every keystroke.
  let timer = null;
  $("vp-images-max")?.addEventListener("input", () => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      if (!$("vp-images-backdrop")?.classList.contains("hidden")) vpImagesRender();
    }, 150);
  });
  // Delegated: click a loaded tile to open it in the shared zoom lightbox.
  $("vp-images-grid")?.addEventListener("click", (event) => {
    const img = event.target.closest?.(".vp-images-tile img");
    if (img?.src) openImageZoom(img.src, img.alt);
  });
}

// --- Row actions in the delete modal's two result lists -------------------
//
// Right-click a row in "Used by" or "Dependencies" to act on THAT package
// rather than the one being deleted. Right-click (not inline buttons) because
// the dependency rows are checkbox <label>s — a button inside one is a
// mis-click away from toggling that dependency's delete tickbox.
//
// Two of the four actions leave the modal, and both must close it first
// (which cancels the pending delete, exactly like the "Change in Settings"
// button already does):
//   - Open Details navigates the whole page away.
//   - Scan dependencies opens #dep-scan-backdrop, which sits BEFORE
//     #vp-delete-backdrop in the DOM; every .dialog-backdrop shares z-index
//     1400 and stacks by DOM order, so it would otherwise open behind this
//     modal and be unreachable.
// Show in Explorer and Copy file path touch nothing, so they leave the modal
// open and the delete still pending.
function vpDeleteRowMenuItems(packageId, filePath) {
  const path = String(filePath || "");
  const pkg = String(packageId || "");
  if (!path) return [];
  return [
    {
      label: t("varPackagesRowOpenDetails"),
      action: () => {
        // vpDeleteModalClose refuses while a scan is running. Navigating anyway
        // would strand the modal open behind the new page, so bail instead.
        if (VP_DELETE.running) return;
        // Close first: this navigates away, so the delete is abandoned.
        vpDeleteModalClose(false);
        openCandidatePackageInVarDetails(pkg, path);
      },
    },
    {
      label: t("varPackagesRowScanDeps"),
      action: () => {
        if (VP_DELETE.running) return;
        vpDeleteModalClose(false);
        depStartScan({ filePath: path, packageId: pkg }).catch((e) =>
          addLog(`Download Dependencies: ${String(e)}`),
        );
      },
    },
    { separator: true },
    {
      label: t("varPackagesRowShowInExplorer"),
      action: () =>
        invoke("show_in_explorer", { path }).catch((e) =>
          addLog(`VAR Packages: ${String(e)}`),
        ),
    },
    {
      label: t("varPackagesRowCopyPath"),
      action: () =>
        copyTextToClipboard(path).catch((e) => addLog(`VAR Packages: ${String(e)}`)),
    },
  ];
}

// A dependency row is a whole FAMILY and may have several local files (two
// versions, or the same version in two folders). One file -> a flat menu; more
// than one -> a submenu per file so it is never ambiguous which is acted on.
function vpDdepRowMenuItems(entry) {
  const files = Array.isArray(entry?.files) ? entry.files : [];
  if (files.length === 0) return [];
  if (files.length === 1) {
    return vpDeleteRowMenuItems(files[0].package_id, files[0].file_path);
  }
  return files.map((f) => ({
    label: f.package_id || f.file_path,
    submenu: vpDeleteRowMenuItems(f.package_id, f.file_path),
  }));
}

function setupVarPackagesDeleteModal() {
  // "Used by" rows.
  $("vp-delete-scan-list")?.addEventListener("contextmenu", (event) => {
    const row = event.target.closest?.("[data-vpu-idx]");
    if (!row) return;
    event.preventDefault();
    const match = (VP_DELETE.dependents ?? [])[Number(row.dataset.vpuIdx)];
    if (!match) return;
    const items = vpDeleteRowMenuItems(match.package_id, match.file_path);
    if (items.length) showContextMenu(event.clientX, event.clientY, items);
  });
  // "Dependencies" rows. preventDefault also stops the browser's own menu from
  // appearing over ours; the label's checkbox is untouched by a right-click.
  $("vp-delete-dep-list")?.addEventListener("contextmenu", (event) => {
    const row = event.target.closest?.("[data-ddep-row]");
    if (!row) return;
    event.preventDefault();
    const entry = (VP_DELETE.deps ?? [])[Number(row.dataset.ddepRow)];
    if (!entry) return;
    const items = vpDdepRowMenuItems(entry);
    // A dependency that is not installed locally has no file to act on.
    if (items.length) showContextMenu(event.clientX, event.clientY, items);
  });

  $("vp-delete-scan-button")?.addEventListener("click", () => {
    vpDeleteRunScan().catch((e) => addLog(`Usage scan: ${String(e)}`));
  });
  $("vp-delete-dep-scan-button")?.addEventListener("click", () => {
    vpDeleteRunDepScan().catch((e) => addLog(`Dependency scan: ${String(e)}`));
  });
  // Delegated: expand/collapse a shared dep's user list. The chip is a real
  // <button> inside the row label, so it never toggles the checkbox.
  $("vp-delete-dep-list")?.addEventListener("click", (event) => {
    const btn = event.target.closest?.("[data-ddep-toggle]");
    if (!btn) return;
    event.preventDefault();
    const users = document.querySelector(`[data-ddep-users="${btn.dataset.ddepToggle}"]`);
    if (!users) return;
    const nowHidden = users.classList.toggle("hidden");
    btn.setAttribute("aria-expanded", String(!nowHidden));
    btn.closest(".vp-ddep-row")?.classList.toggle("has-users-expanded", !nowHidden);
  });
  $("vp-delete-dep-list")?.addEventListener("change", (event) => {
    if (!event.target.classList?.contains("vp-ddep-check")) return;
    vpDdepUpdateConfirmLabel();
    vpDdepRefreshEffective();
  });
  // "Change in Settings" — close the modal (cancels this delete) and jump to
  // the Settings page to edit the VAR library root. Blocked while a scan runs.
  $("vp-delete-root-settings")?.addEventListener("click", () => {
    if (VP_DELETE.running) return;
    vpDeleteModalClose(false);
    document.querySelector('[data-sidebar-link="settings"]')?.click();
  });
  $("vp-delete-confirm")?.addEventListener("click", () => {
    if (!VP_DELETE.running) vpDeleteModalClose(true);
  });
  $("vp-delete-cancel")?.addEventListener("click", () => {
    if (VP_DELETE.running) {
      // Cancel the in-flight scan; the poll breaks on the flag and re-enables
      // the UI. cancel_task is best-effort (no-op if the task has no flag).
      VP_DELETE.cancelled = true;
      if (VP_DELETE.taskId != null) {
        invoke("cancel_task", { taskId: VP_DELETE.taskId }).catch(() => {});
      }
      return;
    }
    vpDeleteModalClose(false);
  });
  // Click outside the card closes (only when idle).
  $("vp-delete-backdrop")?.addEventListener("click", (event) => {
    if (event.target === $("vp-delete-backdrop") && !VP_DELETE.running) {
      vpDeleteModalClose(false);
    }
  });
}

function setupVarPackagesMaintenance() {
  // One delegated handler for both views: the folder grid's cards and the
  // database table's rows both carry data-vp-delete.
  for (const containerId of ["var-packages-grid", "var-packages-tbody"]) {
    $(containerId)?.addEventListener("click", (event) => {
      const btn = event.target.closest?.("[data-vp-delete]");
      if (!btn) return;
      // Stop the row/card's own "open VAR Details" handler from also firing.
      event.preventDefault();
      event.stopPropagation();
      vpDeleteOne(btn.getAttribute("data-vp-delete"), btn).catch((e) =>
        addLog(`VAR Packages: ${String(e)}`),
      );
    });
    // "View images". The grid has this on its right-click menu alongside the
    // other per-card actions; the database table has no context menu, so the
    // row button is the only launcher there.
    $(containerId)?.addEventListener("click", (event) => {
      const btn = event.target.closest?.("[data-vp-images]");
      if (!btn) return;
      // Same reason as the delete button: don't also open VAR Details.
      event.preventDefault();
      event.stopPropagation();
      vpImagesOpen(btn.getAttribute("data-vp-images"));
    });
    // Favorite star toggle — the table-row star and the grid-card star both
    // carry data-vp-fav.
    $(containerId)?.addEventListener("click", (event) => {
      const btn = event.target.closest?.("[data-vp-fav]");
      if (!btn) return;
      event.preventDefault();
      event.stopPropagation();
      togglePackageFavorite(btn.getAttribute("data-vp-fav"));
    });
  }

  $("vp-plan-apply")?.addEventListener("click", () => {
    vpApplyPlan().catch((e) => addLog(`VAR Packages: ${String(e)}`));
  });
  $("vp-plan-cancel")?.addEventListener("click", () => {
    if (VP_PLAN.running) {
      // cancel_task is a no-op for tasks without a cancel flag; all three of
      // ours register one. Keep the dialog open until the poll reports done.
      if (VP_PLAN.taskId != null) {
        invoke("cancel_task", { taskId: VP_PLAN.taskId }).catch((e) =>
          addLog(`VAR Packages: ${String(e)}`),
        );
      }
      return;
    }
    vpClosePlan();
  });

  // Delegated, because rows are re-rendered on every plan/apply.
  $("vp-plan-list")?.addEventListener("click", (event) => {
    const btn = event.target.closest?.("[data-vp-reveal]");
    if (!btn) return;
    // The button lives inside a <label>, which would otherwise toggle the box.
    event.preventDefault();
    event.stopPropagation();
    const action = VP_PLAN.actions[Number(btn.dataset.vpReveal)];
    if (action?.file_path) {
      invoke("show_in_explorer", { path: action.file_path }).catch((e) =>
        addLog(`VAR Packages: ${String(e)}`),
      );
    }
  });

  $("var-packages-db-stale-rebuild")?.addEventListener("click", () => {
    $("var-packages-db-stale")?.classList.add("hidden");
    state.varPackagesDbStale = false;
    switchToBuildDatabase();
  });
}

/// Sends the user to the Database page's Build tab, where the rebuild actually
/// lives, rather than duplicating the scan wiring here.
function switchToBuildDatabase() {
  setDatabaseTab("build");
  const link = document.querySelector('[data-sidebar-link="build-db"]');
  if (link) {
    link.click();
  } else {
    addLog("VAR Packages: open Database → Build and re-scan to refresh package paths.");
  }
}

// Toggle every sibling .app-main view off and reveal the VAR Details page.
// Used both by the sidebar route switch and by row-clicks inside the VAR
// Packages table. Sidebar nav also flips the .active class on its own.
function showVarDetailsView() {
  const varDetails = $("var-details-view");
  if (!varDetails) return;
  // Every page is an .app-main section; show only VAR Details.
  document.querySelectorAll(".app-main").forEach((view) => {
    view.classList.toggle("hidden", view !== varDetails);
  });

  const sidebarLinks = document.querySelectorAll("[data-sidebar-link]");
  sidebarLinks.forEach((link) => {
    link.classList.toggle(
      "active",
      link.getAttribute("data-sidebar-link") === "var-details"
    );
  });
}

// Set the package shown by the VAR Details view and reset its table state.
// `source` is "local" | "folder" | "db" — determines which scan path the
// analyzer takes (file harvest vs DB-only lookup).
function selectVarDetailsItem(item, source = null) {
  if (!item || !item.package_id) return;
  state.varDetails.packageId = item.package_id;
  state.varDetails.item = item;
  state.varDetails.itemSource = source;
  // "local" → loaded from a picked .var path (verified on disk).
  // "folder" → from VAR Packages in folder mode (also verified on disk).
  // "db" or null → DB-only or unknown; file_path may be stale.
  state.varDetails.itemIsLocal = source === "local" || source === "folder";
  state.varDetails.search = "";
  state.varDetails.resources = [];
  state.varDetails.dbFindResult = null;
  state.varDetails.overlap = null;
  state.varDetails.assetSource = "none";
  state.varDetails.assetPage = 0;
  state.varDetails.resourcesError = null;
  state.varDetails.resourcesLoading = false;
  state.varDetails.expandedPackages = new Set();
  state.varDetails.selectedResourceKey = null;
  // Reset the download-availability banner for the newly selected package so it
  // re-resolves (or stays hidden when the file is present).
  state.varDetails.availability = {
    state: "idle",
    resolvedFor: null,
    presenceChecked: state.varDetails.itemIsLocal, // known-local needs no check
    filePresent: state.varDetails.itemIsLocal, // local/folder sources are on disk
    url: null,
    filename: null,
    size: null,
    host: null,
    percent: 0,
    detail: "",
    error: null,
    taskId: null,
  };
  // For DB/unknown-source items the indexed file_path may be stale. Verify it
  // exists; if so, treat the VAR as in-library (no banner). If not (or no path),
  // the banner appears and offers to download.
  if (!state.varDetails.itemIsLocal) {
    if (item.file_path && invoke) {
      const checking = item;
      invoke("path_exists", { path: item.file_path })
        .then((exists) => {
          if (state.varDetails.item !== checking) return;
          state.varDetails.availability.filePresent = !!exists;
          if (exists) state.varDetails.itemIsLocal = true;
          state.varDetails.availability.presenceChecked = true;
          renderVarDetails();
        })
        .catch(() => {
          if (state.varDetails.item !== checking) return;
          state.varDetails.availability.presenceChecked = true;
          renderVarDetails();
        });
    } else {
      state.varDetails.availability.presenceChecked = true;
    }
  }
  if (typeof item.creatorFlag !== "number") item.creatorFlag = 0;
  const searchInput = $("var-details-search");
  if (searchInput) searchInput.value = "";
  renderVarDetails();
  ensureVarDetailsPreview(item);
  refreshCreatorFlagForItem(item);
}

// VAR Packages rows/cards (folder mode) and DB rows arrive without an embedded
// scene image — only `loadVarDetailsFromPath` pre-fills `scene_image_data`.
// When the details view opens for such an item, pull its scene preview on
// demand via `get_var_file_stats`, reusing the grid's thumbnail cache so a
// click from the Local grid is instant. Fire-and-forget; a missing/unreadable
// file just leaves the placeholder in place.
async function ensureVarDetailsPreview(item) {
  if (!item || item.scene_image_data || !item.file_path || !invoke) return;
  const filePath = item.file_path;
  const apply = (dataUrl) => {
    if (!dataUrl) return;
    item.scene_image_data = dataUrl;
    if (state.varDetails.item === item) renderVarDetailsPreview(dataUrl);
  };
  if (varPackageThumbCache.has(filePath)) {
    apply(varPackageThumbCache.get(filePath));
    return;
  }
  try {
    const stats = await invoke("get_var_file_stats", { packagePath: filePath });
    const dataUrl = stats?.scene_image_data ?? null;
    varPackageThumbCache.set(filePath, dataUrl);
    apply(dataUrl);
  } catch {
    /* file missing or unreadable — keep the placeholder */
  }
}

// Pulls the persisted creator flag for the freshly selected VAR. The buttons
// render in their default (inactive) state immediately; this fills in the
// active class once the DB call returns. Fire-and-forget by design — failures
// just leave the buttons in their default state.
async function refreshCreatorFlagForItem(item) {
  if (!item || !item.creator) return;
  try {
    const flag = await invoke("get_creator_flag", { creatorName: item.creator });
    if (state.varDetails.item !== item) return;
    item.creatorFlag = Number(flag) || 0;
    renderVarDetails();
  } catch (err) {
    console.warn("get_creator_flag failed", err);
  }
}

// Click-through entry point used by VAR Packages row clicks: switch to the
// details view AND populate it with the clicked item. The asset table stays
// empty until the user runs a scan.
function openVarDetailsView(item, source = null) {
  showVarDetailsView();
  if (item && item.package_id) {
    selectVarDetailsItem(item, source);
  } else {
    renderVarDetails();
  }
}

window.__openVarDetailsView = openVarDetailsView;

// Action-button entry point: open a candidate package in VAR Details.
// Prefers a freshly-loaded view from disk when we have a real file path
// (local candidates carry the absolute .var path in `package_file`);
// falls back to a cached VAR Packages row, then to a synthetic DB stub.
// Hoisted to file scope so both the Missing Resources and Internalize
// Resources IIFEs can call it — they're sibling closures and a function
// defined inside one is invisible from the other.
function openCandidatePackageInVarDetails(packageId, packageFile) {
  if (!packageId || typeof openVarDetailsView !== "function") return;
  const hasPath = packageFile && /[\\/]/.test(packageFile) && /\.var$/i.test(packageFile);
  if (hasPath && typeof loadVarDetailsFromPath === "function") {
    showVarDetailsView();
    loadVarDetailsFromPath(packageFile);
    return;
  }
  const cached = (state.varPackagesItems ?? []).find((it) => it.package_id === packageId);
  if (cached) {
    openVarDetailsView(cached, "folder");
    return;
  }
  const fileName = packageFile ? String(packageFile).split(/[\\/]/).pop() : `${packageId}.var`;
  openVarDetailsView({
    package_id: packageId,
    file_name: fileName,
    file_path: hasPath ? packageFile : "",
    creator: deriveCreatorFromPackageId(packageId),
    size_bytes: 0,
    modified_ms: null,
    indexed: true,
    scene_image_data: null,
  }, "db");
}

// Sidebar entry point: re-render with whatever package (if any) is already
// cached on state. Never auto-fetches resources — the user must trigger a
// scan explicitly. View toggling is handled by index.html's switch.
// ===========================================================================
// VAR Details — Download Dependencies.
//
// Reads the current VAR's recursive meta.json dependency tree, checks each dep
// against the VAR library folder (Settings), and lists them all in a modal
// (found / missing / unknown). Missing deps get a one-click download.
//
// Reuses the backend dependency-analysis commands and the central queueDownload
// manager. Own `dep-`/`DEP_SCAN` prefix so nothing collides with the VAR
// Packages maintenance modal (vp-/VP_PLAN) in this single global scope.
// ===========================================================================

// `target` is the .var a file-mode scan read ({ filePath, packageId }); null in
// text mode. "Find Dependencies Locally…" hands it to the collect-deps modal.
const DEP_SCAN = { items: [], response: null, taskId: null, running: false, filter: "all", target: null };

const DEP_STATUS_LABEL = { found: "Found", missing: "Missing", unknown: "Unknown" };

function depScanSetText(id, v) {
  const el = $(id);
  if (el) el.textContent = String(v ?? "");
}

// No global fileName() exists (it's only ever a function-local const elsewhere).
function depFileName(p) {
  return String(p || "").split(/[\\/]/).pop() || String(p || "");
}

/// The VAR library folder (Settings), read from the DOM. Falls back to the VAR
/// Details folder so the scan is not "everything unknown" when it is unset.
function depLibraryDirs() {
  const dl = [
    ($("settings-library-folder")?.value || "").trim(),
    ...getAdditionalDirs("downloadVars"),
  ].filter(Boolean);
  if (dl.length) return dl;
  return [
    ($("var-details-folder-input")?.value || "").trim(),
    ...getAdditionalDirs("varDetails"),
  ].filter(Boolean);
}

// Re-runs the open Download Dependencies scan after its folder list changed.
function depRescanAfterFolderChange() {
  const backdrop = $("dep-scan-backdrop");
  if (!backdrop || backdrop.classList.contains("hidden") || DEP_SCAN.running) return;
  if (DEP_SCAN.target) {
    depStartScan(DEP_SCAN.target).catch((e) => addLog(`Download Dependencies: ${String(e)}`));
  } else if (($("dep-scan-text-input")?.value || "").trim()) {
    depAnalyzeText().catch((e) => addLog(`Find Dependencies: ${String(e)}`));
  }
}

function depScanBusy(busy) {
  DEP_SCAN.running = busy;
  // Every entry point into this modal is disabled while a task runs, so a second
  // one can't start mid-flight.
  for (const id of ["var-details-scan-deps-button", "find-deps-toggle", "dep-scan-analyze-text", "dep-scan-organize", "dep-scan-text-input", "dep-scan-root-settings", "dep-scan-add-folder"]) {
    const el = $(id);
    if (el) el.disabled = busy;
  }
  const all = $("dep-scan-download-all");
  if (all) all.disabled = busy || depMissingDownloadable().length === 0;
  // While a scan runs, Close becomes Cancel: hiding the modal would leave the
  // worker running behind it.
  depScanSetText("dep-scan-close", busy ? "Cancel" : "Close");
  $("dep-scan-progress")?.classList.toggle("hidden", !busy);
}

// Clone of vpRunTask: that one hardcodes #vp-plan-bar and returns
// package_op_result, so it can't be shared. get_task_progress ERRORS (never
// returns null) with "task not found", so the try/catch is the real exit.
async function depRunTask(command, args) {
  const handle = await invoke(command, args);
  DEP_SCAN.taskId = handle?.id ?? null;
  if (DEP_SCAN.taskId == null) throw new Error("task did not start");
  try {
    for (;;) {
      await new Promise((resolve) => setTimeout(resolve, 300));
      const payload = await invoke("get_task_progress", { taskId: DEP_SCAN.taskId });
      if (!payload) break;
      const pct = Math.round(Math.max(0, Math.min(1, Number(payload.progress ?? 0))) * 100);
      const bar = $("dep-scan-bar");
      if (bar) bar.style.width = `${pct}%`;
      depScanSetText("dep-scan-progress-message", payload.message ?? "");
      if (payload.error) throw new Error(String(payload.error));
      if (payload.done) return payload.analyze_var_deps_result ?? null;
    }
    return null;
  } finally {
    const id = DEP_SCAN.taskId;
    DEP_SCAN.taskId = null;
    try {
      await invoke("clear_task", { taskId: id });
    } catch (_e) {
      /* task may already be gone */
    }
  }
}

function depScanOpen() {
  $("dep-scan-backdrop")?.classList.remove("hidden");
}

function depScanClose() {
  if (DEP_SCAN.running) return; // Cancel first — see depScanBusy.
  $("dep-scan-backdrop")?.classList.add("hidden");
  DEP_SCAN.items = [];
  DEP_SCAN.response = null;
  DEP_SCAN.target = null;
}

// `target` names the .var to scan: { filePath, packageId }. Omitted (the VAR
// Details button) → the package currently open in VAR Details. Passing it lets
// the VAR Packages grid right-click menu scan any card without opening it first.
// Both entry points (a .var scan and a pasted-text analyze) return the same
// AnalyzeVarDepsResponse, so they share this mapping and this scope line.
function depMapItems(res) {
  return (res?.dependencies || []).map((d) => ({
    pkg: d.package_id,
    creator: d.creator || (String(d.package_id).split(".")[0] || ""),
    version: d.version || "",
    status: d.status,
    size: Number(d.file_size) || 0,
    url: d.download_url || "",
    filename: d.filename || "",
    host: d.source_host || "",
    localPath: d.local_path || "",
  }));
}

// The dialog's folder row: where its downloads are saved (Settings → Downloads
// folder, else AddonPackages), with the organize-by-creator switch beside it.
// The folders a dependency is CHECKED against (AddonPackages + extra folders)
// are an implementation detail and no longer listed here.
function depScanRenderScope() {
  const listEl = $("dep-scan-root-list");
  if (listEl) {
    const dir = configuredDownloadsDir();
    const custom = Boolean(($("settings-downloads-folder")?.value || "").trim());
    listEl.innerHTML = dir
      ? `<div class="dep-scan-root-item" title="${escapeAttribute(dir)}">${escapeHtml(dir)}${
          custom ? "" : ' <span class="dep-scan-root-note">(default)</span>'
        }</div>`
      : '<div class="dep-scan-root-empty">No downloads folder — set your VaM directory in Settings, or you will be asked to pick a folder on the first download.</div>';
  }
  const organize = $("dep-scan-organize");
  if (organize) organize.checked = Boolean($("settings-organize-by-creator")?.checked);
}

// `target` names the .var to scan: { filePath, packageId }. Omitted (the VAR
// Details button) → the package currently open in VAR Details. Passing it lets
// the VAR Packages grid right-click menu scan any card without opening it first.
async function depStartScan(target) {
  if (!invoke || DEP_SCAN.running) return;
  depHubResetFailures();
  const filePath = target?.filePath || state.varDetails?.item?.file_path || "";
  const packageId = target?.packageId || state.varDetails?.item?.package_id || "";
  if (!filePath) {
    addLog("Download Dependencies: no .var file on disk to scan.");
    return;
  }

  // File mode: the paste box belongs to the text entry point only.
  $("dep-scan-text-region")?.classList.add("hidden");
  DEP_SCAN.target = { filePath, packageId };

  DEP_SCAN.items = [];
  DEP_SCAN.response = null;
  DEP_SCAN.filter = "all";
  depApplyFilterUi();
  depScanSetText("dep-scan-title", `Download Dependencies · ${packageId || depFileName(filePath)}`);
  depScanSetText("dep-scan-summary", "");
  $("dep-scan-list").innerHTML = "";

  const libraryDirs = depLibraryDirs();
  depScanRenderScope();

  depScanOpen();
  depScanBusy(true);
  try {
    const res = await depRunTask("start_analyze_var_dependencies_task", {
      varPaths: [filePath],
      libraryDirs,
    });
    DEP_SCAN.response = res;
    DEP_SCAN.items = depMapItems(res);
    // Surface per-file problems (bad archive, no meta.json).
    (res?.sources || []).forEach((s) => {
      if (s && s.error) addLog(`Download Dependencies: ${depFileName(s.file_path)} — ${s.error}`);
    });
    depRenderList();
  } catch (err) {
    depScanSetText("dep-scan-summary", `Failed: ${String(err)}`);
    addLog(`Download Dependencies: ${String(err)}`);
  } finally {
    depScanBusy(false);
  }
}

// Text entry point (VAR Packages "Find Dependencies"): opens the same modal with
// the paste box shown. Does not run anything until the user pastes and clicks
// Analyze — that's depAnalyzeText.
function depStartTextScan() {
  if (!invoke || DEP_SCAN.running) return;
  $("dep-scan-text-region")?.classList.remove("hidden");
  DEP_SCAN.target = null;

  DEP_SCAN.items = [];
  DEP_SCAN.response = null;
  DEP_SCAN.filter = "all";
  depApplyFilterUi();
  depScanSetText("dep-scan-title", "Find Dependencies");
  $("dep-scan-list").innerHTML = "";
  depScanRenderScope();
  depRenderList(); // reset the Download-All button label/state
  // After depRenderList: with no items and no response it blanks the summary,
  // which used to swallow this hint before it was ever painted.
  depScanSetText("dep-scan-summary", "Paste text and click Analyze.");

  depScanOpen();
  const input = $("dep-scan-text-input");
  if (input) input.focus();
}

async function depAnalyzeText() {
  if (!invoke || DEP_SCAN.running) return;
  const input = $("dep-scan-text-input");
  const text = (input?.value || "").trim();
  if (!text) {
    depScanSetText("dep-scan-summary", "Paste some text first.");
    input?.focus();
    return;
  }

  const libraryDirs = depLibraryDirs();
  depScanRenderScope();
  DEP_SCAN.filter = "all";
  depApplyFilterUi();

  depScanBusy(true);
  try {
    const res = await depRunTask("start_analyze_text_dependencies_task", {
      text,
      libraryDirs,
    });
    DEP_SCAN.response = res;
    DEP_SCAN.items = depMapItems(res);
    depRenderList();
  } catch (err) {
    depScanSetText("dep-scan-summary", `Failed: ${String(err)}`);
    addLog(`Find Dependencies: ${String(err)}`);
  } finally {
    depScanBusy(false);
  }
}

/// Missing rows we can auto-download (have a source, and not MediaFire — which
/// is open-in-browser only).
function depMissingDownloadable() {
  return DEP_SCAN.items.filter(
    (it) => it.status === "missing" && it.url && it.host !== "mediafire",
  );
}

function depApplyFilterUi() {
  document.querySelectorAll("#dep-scan-filter [data-dep-filter]").forEach((btn) => {
    const on = btn.getAttribute("data-dep-filter") === DEP_SCAN.filter;
    btn.classList.toggle("active", on);
    btn.setAttribute("aria-selected", String(on));
  });
}

// ============================================================
// Package cards in dependency lists
//
// Rows in Download Dependencies and Find Dependencies Locally show a
// thumbnail, title, author, size, license and type: read from the .var when
// it is on disk (get_var_image, the library's thumbnail loader), else looked
// up on the VaM Hub (get_hub_package_meta, cached per package family).
// Hub answers patch the card in place so a row's buttons never re-render
// under the cursor.
// ============================================================

const DEP_HUB = { meta: new Map(), pending: new Set(), failed: new Set(), queue: [], active: 0 };

function depFamilyKey(pkg) {
  return String(pkg ?? "")
    .trim()
    .replace(/\.(\d+|latest|min\d+)$/i, "")
    .toLowerCase();
}

// What a card needs, from either list's item shape.
function depCardItem(list, it) {
  if (list === "collect") {
    return {
      pkg: it.package_id,
      creator: it.creator,
      version: it.version,
      size: it.size,
      localPath: it.path || "",
    };
  }
  return { pkg: it.pkg, creator: it.creator, version: it.version, size: it.size, localPath: it.localPath || "" };
}

function depCardHtml(list, it, extraLines = "") {
  const card = depCardItem(list, it);
  const key = depFamilyKey(card.pkg);
  const hub = card.localPath ? null : DEP_HUB.meta.get(key);
  const gradient = libGradient(card.pkg);
  const thumb = card.localPath
    ? `${libThumbHtml(card.localPath, gradient, "dep-thumb")}</div>`
    : `<span class="dep-thumb" style="--lib-thumb-bg:${escapeAttribute(gradient)}">${
        hub?.image_data ? `<img alt="" src="${escapeAttribute(hub.image_data)}" />` : ""
      }</span>`;
  const title = hub?.title || libTitle({ package_id: card.pkg });
  const version = /^\d+$/.test(String(card.version ?? "")) ? `v${card.version}` : card.version || "";
  const author = hub?.username || card.creator || "";
  const size = Number(card.size) || Number(hub?.file_size) || 0;
  const meta = [author && `by ${author}`, size ? formatBytesLocal(size) : ""].filter(Boolean).join(" · ");
  const chips = [
    hub?.resource_type ? `<span class="dep-card-chip">${escapeHtml(hub.resource_type)}</span>` : "",
    hub?.category === "Paid" ? `<span class="dep-card-chip is-paid">Paid</span>` : "",
    hub?.license ? `<span class="dep-card-chip">${escapeHtml(hub.license)}</span>` : "",
    hub?.hub_url
      ? `<button type="button" class="dep-card-hub" data-dep-hub-url="${escapeAttribute(hub.hub_url)}" title="Open on the VaM Hub"><span class="material-symbols-outlined">open_in_new</span>Hub</button>`
      : "",
  ].join("");
  return `<span class="dep-card" data-dep-card="${escapeAttribute(key)}">
    ${thumb}
    <span class="dep-scan-main">
      <span class="dep-card-title-line">
        <span class="dep-scan-name" title="${escapeAttribute(title)}">${escapeHtml(title)}</span>
        ${version ? `<span class="dep-card-ver">${escapeHtml(version)}</span>` : ""}
      </span>
      ${meta || chips ? `<span class="dep-scan-meta">${escapeHtml(meta)}${chips}</span>` : ""}
      <span class="dep-card-id" title="${escapeAttribute(card.pkg)}">${escapeHtml(card.pkg)}</span>
      ${extraLines}
    </span>
  </span>`;
}

// After a list renders: start local thumbnails, and queue Hub lookups for the
// rows whose package isn't on disk.
function depCardsActivate(listEl, list, items) {
  if (!listEl) return;
  libThumbWatch(listEl);
  if (!invoke) return;
  for (const it of items) {
    const card = depCardItem(list, it);
    if (card.localPath || !card.pkg) continue;
    const key = depFamilyKey(card.pkg);
    if (DEP_HUB.meta.has(key) || DEP_HUB.pending.has(key) || DEP_HUB.failed.has(key)) continue;
    DEP_HUB.pending.add(key);
    DEP_HUB.queue.push(card.pkg);
  }
  depHubPump();
}

function depHubPump() {
  while (DEP_HUB.active < 3 && DEP_HUB.queue.length) {
    const pkg = DEP_HUB.queue.shift();
    const key = depFamilyKey(pkg);
    DEP_HUB.active += 1;
    invoke("get_hub_package_meta", { packageId: pkg })
      .then((meta) => {
        if (meta?.error) DEP_HUB.failed.add(key);
        else DEP_HUB.meta.set(key, meta ?? { found: false });
      })
      .catch(() => DEP_HUB.failed.add(key))
      .finally(() => {
        DEP_HUB.pending.delete(key);
        DEP_HUB.active -= 1;
        depCardsRefresh(key);
        depHubPump();
      });
  }
}

// Re-renders just the cards for one package family, in whichever list shows it.
function depCardsRefresh(key) {
  document.querySelectorAll(`.dep-scan-row[data-dep-list] [data-dep-card="${CSS.escape(key)}"]`).forEach((cardEl) => {
    const row = cardEl.closest(".dep-scan-row");
    const list = row.getAttribute("data-dep-list");
    const idx = Number(row.getAttribute("data-dep-idx"));
    const it = (list === "collect" ? DEP_COLLECT.items : DEP_SCAN.items)[idx];
    if (!it) return;
    const extra = list === "collect" ? dcRowExtraLines(it) : "";
    cardEl.outerHTML = depCardHtml(list, it, extra);
  });
}

// A new scan may come back online: retry families whose lookup failed.
function depHubResetFailures() {
  DEP_HUB.failed.clear();
}

function depRenderList() {
  const res = DEP_SCAN.response;
  const total = DEP_SCAN.items.length;
  const missing = DEP_SCAN.items.filter((it) => it.status === "missing").length;

  if (!total) {
    depScanSetText("dep-scan-summary", res ? "This package declares no dependencies." : "");
  } else if (res?.library_used) {
    depScanSetText(
      "dep-scan-summary",
      `${total} dependencies · ${missing} missing · checked against ${res.library_var_count} VAR(s) in your library`,
    );
  } else {
    depScanSetText(
      "dep-scan-summary",
      `${total} dependencies · no library set, so availability is unknown`,
    );
  }

  const rows = DEP_SCAN.items
    .filter((it) => DEP_SCAN.filter !== "missing" || it.status === "missing")
    .map((it) => {
      const i = DEP_SCAN.items.indexOf(it);
      let action = "";
      const activeJob = it.status === "missing" ? findDownloadJob(it.pkg) : null;
      if (it.deleted) {
        // Recycled from this modal — see depDeleteOne for why the status is
        // "unknown" rather than "missing".
        action = `<span class="dep-scan-act-note">Deleted — re-scan</span>`;
      } else if (activeJob) {
        action = `<button class="ghost-button dep-scan-action dep-scan-action-busy" type="button" disabled data-dep-job="${escapeAttribute(it.pkg)}">${escapeHtml(depJobButtonLabel(activeJob))}</button>`;
      } else if (it.status === "missing" && it.url && it.host !== "mediafire") {
        action = `<button class="ghost-button dep-scan-action" type="button" data-dep-download="${escapeAttribute(it.pkg)}">Download</button>`;
      } else if (it.status === "missing" && it.host === "mediafire" && it.url) {
        action = `<button class="ghost-button dep-scan-action" type="button" data-dep-open="${escapeAttribute(it.pkg)}">Open (MediaFire)</button>`;
      } else if (it.status === "found" && it.localPath) {
        // localPath is the highest-version file of the dep's FAMILY (the backend
        // matches on the version-stripped base), so a row reading Creator.Pkg.3
        // can point at Creator.Pkg.7.var — name the delete after the real file.
        const localName = it.filename || depFileName(it.localPath);
        // Deliberately never `disabled` here: the row-producing depRenderList()
        // runs INSIDE the scan's try block, while DEP_SCAN.running is still true
        // (it clears in the finally), so a busy-stamp would render every button
        // permanently dead. The click handlers hold the DEP_SCAN.running guard.
        action =
          `<button class="icon-button" type="button" data-dep-details="${escapeAttribute(it.pkg)}" title="Open this package in VAR Details"><span class="material-symbols-outlined">description</span></button>` +
          `<button class="icon-button" type="button" data-dep-reveal="${escapeAttribute(it.pkg)}" title="Show in Explorer"><span class="material-symbols-outlined">folder_open</span></button>` +
          `<button class="icon-button dep-scan-act-danger" type="button" data-dep-delete="${escapeAttribute(it.pkg)}" title="Send ${escapeAttribute(localName)} to the Recycle Bin" aria-label="Send ${escapeAttribute(localName)} to the Recycle Bin"><span class="material-symbols-outlined">delete</span></button>`;
      } else if (it.status === "missing" && !it.url) {
        // Neither the Hub nor an imported mirror link resolved a source for it.
        // Say so rather than leave a blank slot.
        action = `<span class="dep-scan-act-note">No source</span>`;
      }
      return `<div class="dep-scan-row" data-dep-idx="${i}" data-dep-list="scan">
  ${depCardHtml("scan", it)}
  <span class="chip dep-scan-${escapeHtml(it.status)}">${escapeHtml(DEP_STATUS_LABEL[it.status] ?? it.status)}</span>
  <span class="dep-scan-act">${action}</span>
</div>`;
    })
    .join("");
  $("dep-scan-list").innerHTML =
    rows || `<div class="dep-scan-empty">${escapeHtml(DEP_SCAN.filter === "missing" ? "No missing dependencies." : "Nothing to show.")}</div>`;
  depCardsActivate($("dep-scan-list"), "scan", DEP_SCAN.items);

  const all = $("dep-scan-download-all");
  const downloadable = depMissingDownloadable();
  const pending = downloadable.filter((it) => !findDownloadJob(it.pkg)).length;
  const active = depActiveJobs().length;
  if (all) {
    // While its downloads run, the same button cancels them.
    all.dataset.mode = active ? "cancel" : "download";
    all.disabled = active ? false : DEP_SCAN.running || pending === 0;
    all.textContent = active
      ? `Cancel Downloads (${active})`
      : pending
        ? `Download All Missing (${pending})`
        : "Download All Missing";
  }
}

// Label for a dep row's action button while its download job is active.
function depJobButtonLabel(job) {
  if (job.status === "queued") return "Queued…";
  if (job.status === "cancelling") return "Cancelling…";
  const pct = Math.max(0, Math.min(100, Math.round(job.percent || 0)));
  return pct > 0 ? `Downloading ${pct}%` : "Downloading…";
}

// In-place progress update for one dep row's busy button; falls back to a full
// re-render when the row isn't showing the busy button yet (e.g. first tick).
function depScanSyncJob(job) {
  const backdrop = $("dep-scan-backdrop");
  if (!backdrop || backdrop.classList.contains("hidden")) return;
  const btn = $("dep-scan-list")?.querySelector(
    `[data-dep-job="${CSS.escape(job.packageId)}"]`,
  );
  if (btn) btn.textContent = depJobButtonLabel(job);
  else depRenderList();
}

async function depDownloadOne(pkg, dest) {
  const it = DEP_SCAN.items.find((x) => x.pkg === pkg);
  if (!it || !it.url) return;
  if (findDownloadJob(pkg)) return; // already queued/downloading
  const destDir = dest || (await ensureDownloadsDir());
  if (!destDir) {
    addLog("Download Dependencies: no downloads folder selected.");
    return;
  }
  queueDownload({
    packageId: pkg,
    url: it.url,
    filename: it.filename,
    host: it.host,
    destDir,
    label: it.filename || pkg,
    onProgress: depScanSyncJob,
    onDone: (status, info) => {
      const row = DEP_SCAN.items.find((x) => x.pkg === pkg);
      if (!row) return;
      if (status === "done" || status === "exists") {
        row.status = "found";
        if (info && info.localPath) {
          row.localPath = info.localPath;
          varPackageThumbCache.delete(info.localPath);
        }
      }
      depRenderList();
    },
  });
  depRenderList(); // reflect the queued state
}

/// Recycles a found dependency from inside the modal. Delegates to vpDeleteOne
/// so the opt-in usage/dependency scans, Recycle Bin semantics, selection
/// pruning and grid refresh all stay in one place — this only mirrors the
/// outcome back into the dep rows (the exact inverse of depDownloadOne's onDone).
async function depDeleteOne(pkg) {
  if (!invoke || DEP_SCAN.running) return;
  // vpDeleteModalOpen overwrites VP_DELETE.resolve unconditionally, which would
  // orphan a pending promise — refuse a second open instead.
  if (VP_DELETE.resolve) return;
  const it = DEP_SCAN.items.find((x) => x.pkg === pkg);
  if (!it?.localPath) return;
  // A queued download is about to write this exact file back.
  if (findDownloadJob(pkg)) return;

  // Never pass the clicked button as `trigger`: vpDeleteOne re-enables it after
  // awaits, and depRenderList replaces the whole list meanwhile, so the
  // re-enable would land on a detached node. The grid context menu does the same.
  const removed = (await vpDeleteOne(it.localPath, null)) || [];
  if (removed.length === 0) return;

  // Re-look-up by path, never through the captured row: DEP_SCAN.items is
  // replaced wholesale by a re-scan. Several rows can share one localPath
  // (family-base matching), so every match has to flip or they'd keep claiming
  // "Found" — with a Show in Explorer that no longer resolves.
  //
  // "unknown", not "missing": the backend resolves a dependency against the
  // highest-version file of its FAMILY, so recycling that file can still leave
  // an older version on disk that satisfies the dep. Only a re-scan knows —
  // until then the row says so rather than claiming a package is gone.
  const gone = new Set(removed.map((p) => String(p).toLowerCase()));
  let flipped = 0;
  for (const row of DEP_SCAN.items) {
    if (row.localPath && gone.has(String(row.localPath).toLowerCase())) {
      row.status = "unknown";
      row.localPath = "";
      row.deleted = true;
      flipped += 1;
    }
  }
  for (const p of removed) varPackageThumbCache.delete(p);
  // The backend counts the library once per scan and never recomputes it; keep
  // the summary honest per deleted FILE, not per flipped row.
  const res = DEP_SCAN.response;
  if (res && Number.isFinite(Number(res.library_var_count))) {
    res.library_var_count = Math.max(0, Number(res.library_var_count) - removed.length);
  }
  if (flipped) depRenderList();
}

// Active download jobs for this modal's dependencies.
function depActiveJobs() {
  return DEP_SCAN.items.map((it) => findDownloadJob(it.pkg)).filter(Boolean);
}

function depCancelDownloads() {
  const jobs = depActiveJobs();
  for (const job of jobs.filter((j) => j.status === "queued")) cancelDownload(job.id);
  for (const job of jobs.filter((j) => j.status === "downloading")) cancelDownload(job.id);
  depRenderList();
}

async function depDownloadAllMissing() {
  const queue = depMissingDownloadable().filter((it) => !findDownloadJob(it.pkg));
  if (queue.length === 0) {
    addLog("Download Dependencies: nothing to auto-download (Hub/Pixeldrain only; MediaFire is manual).");
    return;
  }
  const dest = await ensureDownloadsDir();
  if (!dest) {
    addLog("Download Dependencies: no downloads folder selected.");
    return;
  }
  for (const it of queue) await depDownloadOne(it.pkg, dest);
}

function setupVarDetailsDeps() {
  $("var-details-scan-deps-button")?.addEventListener("click", () => {
    depStartScan().catch((e) => addLog(`Download Dependencies: ${String(e)}`));
  });

  $("dep-scan-analyze-text")?.addEventListener("click", () => {
    depAnalyzeText().catch((e) => addLog(`Find Dependencies: ${String(e)}`));
  });

  $("dep-scan-download-all")?.addEventListener("click", (event) => {
    if (event.currentTarget.dataset.mode === "cancel") return depCancelDownloads();
    depDownloadAllMissing().catch((e) => addLog(`Download Dependencies: ${String(e)}`));
  });

  // "Change in Settings" — close the modal and jump to the Settings page to edit
  // the VAR library folders. Blocked while a scan runs (Close is Cancel then).
  // Same setting as Settings → "Organize downloads into creator subfolders".
  $("dep-scan-organize")?.addEventListener("change", (event) => {
    const settingsBox = $("settings-organize-by-creator");
    if (settingsBox) settingsBox.checked = event.target.checked;
    persistAllConfig().catch((e) => addLog(`Settings: ${String(e)}`));
  });
  for (const id of ["dep-scan-list", "dep-collect-list"]) {
    $(id)?.addEventListener("click", (event) => {
      const link = event.target.closest?.("[data-dep-hub-url]");
      if (!link || !invoke) return;
      event.stopPropagation();
      invoke("open_url", { url: link.getAttribute("data-dep-hub-url") }).catch((e) => addLog(`Open link: ${String(e)}`));
    });
  }
  $("dep-scan-root-settings")?.addEventListener("click", () => {
    if (DEP_SCAN.running) return;
    depScanClose();
    document.querySelector('[data-sidebar-link="settings"]')?.click();
  });

  $("dep-scan-close")?.addEventListener("click", () => {
    if (DEP_SCAN.running) {
      // cancel_task is a no-op without a registered cancel flag; the analyze
      // task registers one. Keep the modal open until the poll reports done.
      if (DEP_SCAN.taskId != null) {
        invoke("cancel_task", { taskId: DEP_SCAN.taskId }).catch((e) =>
          addLog(`Download Dependencies: ${String(e)}`),
        );
      }
      return;
    }
    depScanClose();
  });

  document.querySelectorAll("#dep-scan-filter [data-dep-filter]").forEach((btn) => {
    btn.addEventListener("click", () => {
      DEP_SCAN.filter = btn.getAttribute("data-dep-filter") || "all";
      depApplyFilterUi();
      depRenderList();
    });
  });

  // Delegated, because rows are re-rendered on every scan/download.
  $("dep-scan-list")?.addEventListener("click", (event) => {
    const dl = event.target.closest?.("[data-dep-download]");
    if (dl) {
      depDownloadOne(dl.getAttribute("data-dep-download")).catch((e) =>
        addLog(`Download Dependencies: ${String(e)}`),
      );
      return;
    }
    const open = event.target.closest?.("[data-dep-open]");
    if (open) {
      const it = DEP_SCAN.items.find((x) => x.pkg === open.getAttribute("data-dep-open"));
      if (it?.url) invoke("open_url", { url: it.url }).catch((e) => addLog(`Download Dependencies: ${String(e)}`));
      return;
    }
    const reveal = event.target.closest?.("[data-dep-reveal]");
    if (reveal) {
      const it = DEP_SCAN.items.find((x) => x.pkg === reveal.getAttribute("data-dep-reveal"));
      if (it?.localPath) invoke("show_in_explorer", { path: it.localPath }).catch((e) => addLog(`Download Dependencies: ${String(e)}`));
      return;
    }
    const del = event.target.closest?.("[data-dep-delete]");
    if (del) {
      depDeleteOne(del.getAttribute("data-dep-delete")).catch((e) =>
        addLog(`Download Dependencies: ${String(e)}`),
      );
      return;
    }
    const details = event.target.closest?.("[data-dep-details]");
    if (details) {
      if (DEP_SCAN.running) return;
      const it = DEP_SCAN.items.find((x) => x.pkg === details.getAttribute("data-dep-details"));
      if (!it?.localPath) return;
      // Read both before closing — depScanClose clears DEP_SCAN.items.
      const pkg = it.pkg;
      const path = it.localPath;
      // The backdrop is position:fixed inset:0, so navigating without closing
      // would leave VAR Details sitting behind it. Not the sidebar-link route the
      // "Change in Settings" handler takes: showVarDetailsView already unhides the
      // view and moves the sidebar highlight, and the link would additionally
      // re-render the previously selected package for a frame.
      depScanClose();
      openCandidatePackageInVarDetails(pkg, path);
    }
  });
}

// ===========================================================================
// VAR Packages — Collect Dependencies.
//
// Download Dependencies only looks in the VAM library, so a dependency sitting in a
// download folder elsewhere shows up there as "missing" with a Hub link. This
// modal searches folders the user picks (subfolders included) and copies what
// it finds into <library>\<Creator>\deps\, moving the package itself and its
// preview image into <library>\<Creator>\ — the folder "Move to creator folder"
// uses, under the Settings VAR library folder.
//
// The search folders last for the session; with "Remember these folders"
// ticked they are also saved in config (dep_source_dirs, null = not
// remembered). Own `dc-`/DEP_COLLECT prefix, like dep-/DEP_SCAN and vp-/VP_PLAN.
// ===========================================================================

const DEP_COLLECT = {
  target: null, // { filePath, packageId } — filePath follows the package when a copy moves it
  dirs: [],
  remember: false,
  items: [],
  response: null,
  taskId: null,
  running: false, // false | "scan" | "copy"
  copying: null, // lowercased source paths in the running copy
  filter: "all",
  fromDepScan: false, // opened over the Download Dependencies modal
  changed: false, // something was moved or copied since the modal opened
};

const DC_CHIP = {
  found: { label: "Found", cls: "dep-collect-found" },
  in_library: { label: "In library", cls: "dep-scan-found" },
  missing: { label: "Not found", cls: "dep-scan-missing" },
  copied: { label: "Copied", cls: "dep-scan-found" },
  exists: { label: "Already in deps", cls: "dep-scan-found" },
  failed: { label: "Failed", cls: "dep-scan-missing" },
  skipped: { label: "Skipped", cls: "dep-scan-unknown" },
};

const DC_PICK_HINT = "Click Browse… and pick the folders where you keep downloaded VARs.";

// Collect Dependencies always copies into AddonPackages of the VaM directory.
function dcLibraryRoot() {
  return vamAddonPackagesDir() || ($("settings-library-folder")?.value || "").trim();
}

/// What a row shows: what the last copy did to it, else what the scan found. A
/// cancelled copy leaves nothing behind, so its row is simply copyable again.
function dcRowState(it) {
  const copied = it.copy?.status;
  return copied && copied !== "cancelled" ? copied : it.status;
}

function dcCanCopyRow(it) {
  const st = dcRowState(it);
  return Boolean(it.path) && (st === "found" || st === "failed");
}

/// Where the row's file is now: its copy in deps\ once copied, else where the
/// scan found it (the search folder, or the library).
function dcRowWhere(it) {
  const st = dcRowState(it);
  return it.copy && (st === "copied" || st === "exists") ? it.copy.dest : it.path;
}

/// Unique source paths still waiting to be copied. Two rows can share one file
/// (Pkg.3 and Pkg.latest when only one version is on disk).
function dcCopyablePaths() {
  const seen = new Map();
  for (const it of DEP_COLLECT.items) {
    if (dcCanCopyRow(it)) seen.set(String(it.path).toLowerCase(), String(it.path));
  }
  return [...seen.values()];
}

// Same poll loop as depRunTask, against this modal's bar and with the result
// slot passed in. get_task_progress ERRORS (never returns null) once the task
// is gone, so the try/catch is the real exit.
async function dcRunTask(command, args, resultKey) {
  const handle = await invoke(command, args);
  DEP_COLLECT.taskId = handle?.id ?? null;
  if (DEP_COLLECT.taskId == null) throw new Error("task did not start");
  try {
    for (;;) {
      await new Promise((resolve) => setTimeout(resolve, 250));
      const payload = await invoke("get_task_progress", { taskId: DEP_COLLECT.taskId });
      if (!payload) break;
      const pct = Math.round(Math.max(0, Math.min(1, Number(payload.progress ?? 0))) * 100);
      const bar = $("dep-collect-bar");
      if (bar) bar.style.width = `${pct}%`;
      depScanSetText("dep-collect-progress-message", payload.message ?? "");
      if (payload.error) throw new Error(String(payload.error));
      if (payload.done) return payload[resultKey] ?? null;
    }
    return null;
  } finally {
    const id = DEP_COLLECT.taskId;
    DEP_COLLECT.taskId = null;
    try {
      await invoke("clear_task", { taskId: id });
    } catch (_e) {
      /* task may already be gone */
    }
  }
}

function dcBusy(kind) {
  DEP_COLLECT.running = kind || false;
  const busy = Boolean(kind);
  const browse = $("dep-collect-browse");
  if (browse) browse.disabled = busy;
  // While a task runs, Close becomes Cancel: hiding the modal would leave the
  // worker running behind it.
  depScanSetText("dep-collect-close", busy ? "Cancel" : "Close");
  $("dep-collect-progress")?.classList.toggle("hidden", !busy);
  if (busy) {
    const bar = $("dep-collect-bar");
    if (bar) bar.style.width = "0%";
    depScanSetText("dep-collect-progress-message", "");
  }
  dcRenderDirs();
  dcRenderList();
}

function dcApplyFilterUi() {
  document.querySelectorAll("#dep-collect-filter [data-dc-filter]").forEach((btn) => {
    const on = btn.getAttribute("data-dc-filter") === DEP_COLLECT.filter;
    btn.classList.toggle("active", on);
    btn.setAttribute("aria-selected", String(on));
  });
}

function dcRenderDirs() {
  const busy = Boolean(DEP_COLLECT.running);
  const rescan = $("dep-collect-rescan");
  if (rescan) rescan.disabled = busy || DEP_COLLECT.dirs.length === 0;
  const el = $("dep-collect-dirs");
  if (!el) return;
  if (!DEP_COLLECT.dirs.length) {
    el.innerHTML =
      '<div class="dep-collect-dirs-empty">No folders yet. Browse… lets you pick several at once; their subfolders are searched too.</div>';
    return;
  }
  el.innerHTML = DEP_COLLECT.dirs
    .map(
      (dir, i) => `<div class="additional-dir-row">
  <span class="additional-dir-path" title="${escapeAttribute(dir)}">${escapeHtml(dir)}</span>
  <button type="button" class="ghost-button additional-dir-remove" data-dc-remove="${i}" aria-label="${escapeAttribute(t("removeFolder"))}"${busy ? " disabled" : ""}>✕</button>
</div>`,
    )
    .join("");
}

/// Where a copy will put things — or why it can't yet. The missing-library case
/// is known before any scan, so it shows from the moment the modal opens.
function dcRenderDest() {
  const el = $("dep-collect-dest");
  if (!el) return;
  const res = DEP_COLLECT.response;
  const error =
    res?.destination_error ||
    (dcLibraryRoot() ? "" : "Set your VAR library (AddonPackages root) folder in Settings first.");
  if (error) {
    el.innerHTML = `<div class="dep-collect-dest-error"><span>${escapeHtml(error)}</span><button class="dep-scan-root-settings" type="button" data-dc-settings="1">Change in Settings</button></div>`;
    return;
  }
  if (!res) {
    el.innerHTML = "";
    return;
  }
  const pkg = depFileName(DEP_COLLECT.target?.filePath || res.var_path);
  const packageLine = res.package_in_place
    ? `${pkg} is already there: ${res.creator_dir}`
    : `Copying moves ${pkg} and its preview image to ${res.creator_dir}`;
  el.innerHTML =
    `<span class="dep-collect-dest-label">Dependencies</span><span class="dep-collect-dest-path">${escapeHtml(res.deps_dir || "")}</span>` +
    `<span class="dep-collect-dest-label">Package</span><span class="dep-collect-dest-path">${escapeHtml(packageLine)}</span>` +
    (res.destination_warning
      ? `<div class="dep-collect-dest-warn">${escapeHtml(res.destination_warning)}</div>`
      : "");
}

function dcRenderNotes(notes) {
  const el = $("dep-collect-notes");
  if (el) el.innerHTML = (notes || []).map((n) => `<span>${escapeHtml(n)}</span>`).join("");
}

function dcRenderSummary() {
  const res = DEP_COLLECT.response;
  if (!res) return;
  const total = DEP_COLLECT.items.length;
  if (!total) {
    depScanSetText("dep-collect-summary", "This package declares no dependencies.");
    return;
  }
  const counts = {};
  for (const it of DEP_COLLECT.items) {
    const st = dcRowState(it);
    counts[st] = (counts[st] || 0) + 1;
  }
  const toCopy = (counts.found || 0) + (counts.failed || 0);
  const parts = [`${total} ${total === 1 ? "dependency" : "dependencies"}`];
  if (toCopy) parts.push(`${toCopy} to copy`);
  if (counts.copied) parts.push(`${counts.copied} copied`);
  if (counts.exists) parts.push(`${counts.exists} already in deps`);
  if (counts.in_library) parts.push(`${counts.in_library} already in your library`);
  if (counts.missing) parts.push(`${counts.missing} not found`);
  if (counts.skipped) parts.push(`${counts.skipped} skipped`);
  const vars = Number(res.search_var_count) || 0;
  const folders = (res.search_dirs || []).length;
  parts.push(
    `searched ${vars.toLocaleString()} VAR${vars === 1 ? "" : "s"} in ${folders} folder${folders === 1 ? "" : "s"}`,
  );
  depScanSetText("dep-collect-summary", parts.join(" · "));
}

// The lines under a Find Dependencies Locally card: who needs it, where the
// file is, and what the last copy (or the scan) had to say about it.
function dcRowExtraLines(it) {
  const st = dcRowState(it);
  const where = dcRowWhere(it);
  const detail = it.copy && (st === "failed" || st === "skipped") ? it.copy.detail : it.note;
  return [
    it.via ? `<span class="dep-scan-meta">needed by ${escapeHtml(it.via)}</span>` : "",
    where
      ? `<span class="dep-scan-meta dep-collect-path" title="${escapeAttribute(where)}">${escapeHtml(where)}</span>`
      : "",
    detail ? `<span class="dep-collect-note">${escapeHtml(detail)}</span>` : "",
  ].join("");
}

function dcRenderList() {
  const listEl = $("dep-collect-list");
  if (!listEl) return;
  const res = DEP_COLLECT.response;
  const canCopy = Boolean(res) && !res.destination_error && !DEP_COLLECT.running;
  const copying = DEP_COLLECT.copying;
  const rows = DEP_COLLECT.items
    .map((it, i) => ({ it, i, st: dcRowState(it) }))
    .filter(({ it }) => DEP_COLLECT.filter !== "copy" || dcCanCopyRow(it))
    .map(({ it, i, st }) => {
      const chip = DC_CHIP[st] || { label: st, cls: "dep-scan-unknown" };
      const where = dcRowWhere(it);
      let action = "";
      if (copying && it.path && copying.has(String(it.path).toLowerCase())) {
        action = `<button class="ghost-button dep-scan-action" type="button" disabled>Copying…</button>`;
      } else if (dcCanCopyRow(it)) {
        action = `<button class="ghost-button dep-scan-action" type="button" data-dc-copy="${i}"${canCopy ? "" : " disabled"}>${st === "failed" ? "Retry" : "Copy"}</button>`;
      }
      if (where) {
        action += `<button class="icon-button" type="button" data-dc-reveal="${i}" title="Show in Explorer"><span class="material-symbols-outlined">folder_open</span></button>`;
      }
      return `<div class="dep-scan-row" data-dep-idx="${i}" data-dep-list="collect">
  ${depCardHtml("collect", it, dcRowExtraLines(it))}
  <span class="chip ${chip.cls}">${escapeHtml(chip.label)}</span>
  <span class="dep-scan-act">${action}</span>
</div>`;
    })
    .join("");
  listEl.innerHTML =
    rows ||
    (res && DEP_COLLECT.items.length
      ? `<div class="dep-scan-empty">Nothing left to copy.</div>`
      : "");
  depCardsActivate(listEl, "collect", DEP_COLLECT.items);

  const all = $("dep-collect-copy-all");
  const pending = dcCopyablePaths().length;
  if (all) {
    all.disabled = !canCopy || pending === 0;
    all.textContent = pending ? `Copy All (${pending})` : "Copy All";
  }
  dcRenderSummary();
}

/// Opens the modal for one package. `fromDepScan` means it was opened over the
/// Download Dependencies modal, which gets re-run on close if anything changed.
function dcOpen(target, { fromDepScan = false } = {}) {
  if (!invoke || !target?.filePath) return;
  depHubResetFailures();
  // One modal, one package: re-targeting it mid-task would orphan the task.
  if (DEP_COLLECT.running) return;
  const filePath = String(target.filePath);
  DEP_COLLECT.target = {
    filePath,
    packageId: target.packageId || depFileName(filePath).replace(/\.var$/i, ""),
  };
  DEP_COLLECT.fromDepScan = fromDepScan;
  DEP_COLLECT.changed = false;
  DEP_COLLECT.items = [];
  DEP_COLLECT.response = null;
  DEP_COLLECT.filter = "all";
  dcApplyFilterUi();
  depScanSetText("dep-collect-title", `Find Dependencies Locally · ${DEP_COLLECT.target.packageId}`);
  const remember = $("dep-collect-remember");
  if (remember) remember.checked = DEP_COLLECT.remember;
  depScanSetText("dep-collect-summary", "");
  dcRenderNotes([]);
  dcRenderDirs();
  dcRenderDest();
  dcRenderList();
  $("dep-collect-backdrop")?.classList.remove("hidden");
  // Remembered (or earlier-this-session) folders: search straight away.
  if (DEP_COLLECT.dirs.length) {
    dcScan().catch((e) => addLog(`Find Dependencies Locally: ${String(e)}`));
  } else {
    depScanSetText("dep-collect-summary", DC_PICK_HINT);
  }
}

function dcClose() {
  if (DEP_COLLECT.running) return; // Cancel first — see dcBusy.
  $("dep-collect-backdrop")?.classList.add("hidden");
  const { changed, fromDepScan, target } = DEP_COLLECT;
  DEP_COLLECT.items = [];
  DEP_COLLECT.response = null;
  // The Download Dependencies modal underneath still shows the statuses (and maybe
  // the package path) from before the copy, so re-run it to tell the truth.
  if (
    changed &&
    fromDepScan &&
    target &&
    !$("dep-scan-backdrop")?.classList.contains("hidden") &&
    !DEP_SCAN.running
  ) {
    depStartScan({ filePath: target.filePath, packageId: target.packageId }).catch((e) =>
      addLog(`Download Dependencies: ${String(e)}`),
    );
  }
}

async function dcScan() {
  if (!invoke || DEP_COLLECT.running || !DEP_COLLECT.target || !DEP_COLLECT.dirs.length) return;
  DEP_COLLECT.items = [];
  DEP_COLLECT.response = null;
  depScanSetText("dep-collect-summary", "");
  dcRenderNotes([]);
  dcBusy("scan");
  try {
    const res = await dcRunTask(
      "start_collect_deps_scan_task",
      {
        varPath: DEP_COLLECT.target.filePath,
        searchDirs: [...DEP_COLLECT.dirs],
        libraryDirs: depLibraryDirs(),
        rootDir: dcLibraryRoot(),
      },
      "collect_deps_scan_result",
    );
    if (!res || res.was_cancelled) {
      depScanSetText("dep-collect-summary", "Scan cancelled.");
      return;
    }
    DEP_COLLECT.response = res;
    DEP_COLLECT.items = (res.items || []).map((it) => ({ ...it, copy: null }));
    dcRenderNotes(res.notes);
  } catch (err) {
    depScanSetText("dep-collect-summary", `Failed: ${String(err)}`);
    addLog(`Find Dependencies Locally: ${String(err)}`);
  } finally {
    dcBusy(false);
    dcRenderDest();
  }
}

async function dcCopy(paths) {
  if (!invoke || DEP_COLLECT.running || !DEP_COLLECT.target) return;
  const res = DEP_COLLECT.response;
  if (!res || res.destination_error) return;
  const unique = [
    ...new Map(
      (paths || []).filter(Boolean).map((p) => [String(p).toLowerCase(), String(p)]),
    ).values(),
  ];
  if (!unique.length) return;
  const fromPath = DEP_COLLECT.target.filePath;
  DEP_COLLECT.copying = new Set(unique.map((p) => p.toLowerCase()));
  dcBusy("copy");
  let out = null;
  try {
    out = await dcRunTask(
      "start_collect_deps_copy_task",
      { varPath: fromPath, rootDir: dcLibraryRoot(), depPaths: unique },
      "collect_deps_copy_result",
    );
  } catch (err) {
    showToast(`Copy failed: ${String(err)}`, "error", 6000);
    addLog(`Find Dependencies Locally: ${String(err)}`);
  } finally {
    DEP_COLLECT.copying = null;
    dcBusy(false);
  }
  if (out) dcApplyCopyResult(out, fromPath);
}

function dcApplyCopyResult(out, fromPath) {
  const results = out.results || [];
  const bySource = new Map(results.map((r) => [String(r.source_path).toLowerCase(), r]));
  for (const it of DEP_COLLECT.items) {
    const r = it.path ? bySource.get(String(it.path).toLowerCase()) : null;
    if (r) it.copy = { status: r.status, detail: r.detail, dest: r.dest_path };
  }

  const moved = Boolean(out.var_moved);
  if (moved) {
    DEP_COLLECT.target.filePath = out.var_path;
    if (DEP_COLLECT.response) DEP_COLLECT.response.package_in_place = true;
    // VAR Details may be showing this very package; keep its path live.
    const vdItem = state.varDetails?.item;
    if (vdItem && vdItem.file_path === fromPath) vdItem.file_path = out.var_path;
    vpPruneSelection([fromPath]);
    addLog(`Moved ${depFileName(fromPath)} → ${out.var_path}`);
  }
  for (const r of results) {
    if (r.status === "copied") addLog(`Copied ${depFileName(r.source_path)} → ${r.dest_path}`);
    else if (r.status === "failed" || r.status === "skipped") {
      addLog(`Find Dependencies Locally: ${depFileName(r.source_path)} — ${r.detail}`);
    }
  }
  // Set without a move means the move itself failed; with one, a sidecar stayed.
  const moveFailed = !moved && Boolean(out.var_note);
  if (out.var_note) addLog(`Find Dependencies Locally: ${depFileName(fromPath)} — ${out.var_note}`);

  const failed = results.filter((r) => r.status === "failed").length;
  const already = results.filter((r) => r.status === "exists").length;
  const bits = [];
  if (out.was_cancelled) bits.push("Cancelled");
  if (out.copied) {
    bits.push(
      `Copied ${out.copied} ${out.copied === 1 ? "dependency" : "dependencies"} (${formatBytesLocal(out.bytes_copied)})`,
    );
  } else if (already) {
    bits.push(`${already} already in deps`);
  }
  if (moved) bits.push(`moved ${depFileName(out.var_path)} into ${depFileName(out.creator_dir)}`);
  if (moveFailed) bits.push(`couldn't move ${depFileName(fromPath)}: ${out.var_note}`);
  if (failed) bits.push(`${failed} failed — see the console`);
  const kind = failed || moveFailed ? "error" : out.was_cancelled ? "info" : "success";
  showToast(bits.join(" · ") || "Nothing was copied.", kind, 7000);

  dcRenderDest();
  dcRenderList();
  if (out.copied > 0 || moved) {
    DEP_COLLECT.changed = true;
    dcRefreshVarPackages();
  }
}

/// The library changed under the VAR Packages listing: a moved package, new
/// files in deps\. Same refresh vpMoveToCreatorFolder does, but only for a
/// folder listing that exists — this modal is also reachable from VAR Details.
function dcRefreshVarPackages() {
  vpRefreshAfterMutation().catch((e) => addLog(`VAR Packages: ${String(e)}`));
}

async function dcPersistDirs() {
  if (!DEP_COLLECT.remember) return;
  try {
    await persistAllConfig();
  } catch (error) {
    addLog(String(error));
  }
}

async function dcBrowse() {
  if (!invoke || DEP_COLLECT.running) return;
  const picked = await invoke("pick_folders");
  let added = 0;
  for (const raw of Array.isArray(picked) ? picked : []) {
    const dir = String(raw ?? "").trim();
    if (!dir || DEP_COLLECT.dirs.some((d) => d.toLowerCase() === dir.toLowerCase())) continue;
    DEP_COLLECT.dirs.push(dir);
    added += 1;
  }
  if (!added) return;
  dcRenderDirs();
  await dcPersistDirs();
  await dcScan();
}

async function dcRemoveDir(index) {
  if (DEP_COLLECT.running || !(index >= 0 && index < DEP_COLLECT.dirs.length)) return;
  DEP_COLLECT.dirs.splice(index, 1);
  dcRenderDirs();
  await dcPersistDirs();
  if (DEP_COLLECT.dirs.length) {
    await dcScan();
    return;
  }
  DEP_COLLECT.items = [];
  DEP_COLLECT.response = null;
  dcRenderNotes([]);
  dcRenderDest();
  dcRenderList();
  depScanSetText("dep-collect-summary", DC_PICK_HINT);
}

function setupDepCollect() {
  const log = (e) => addLog(`Find Dependencies Locally: ${String(e)}`);

  $("dep-collect-browse")?.addEventListener("click", () => {
    dcBrowse().catch(log);
  });

  $("dep-collect-rescan")?.addEventListener("click", () => {
    dcScan().catch(log);
  });

  // Unticking saves dep_source_dirs as null, which is what forgets them.
  $("dep-collect-remember")?.addEventListener("change", (event) => {
    DEP_COLLECT.remember = Boolean(event.target.checked);
    persistAllConfig().catch((e) => addLog(String(e)));
  });

  $("dep-collect-dirs")?.addEventListener("click", (event) => {
    const btn = event.target.closest?.("[data-dc-remove]");
    if (btn) dcRemoveDir(Number(btn.getAttribute("data-dc-remove"))).catch(log);
  });

  $("dep-collect-copy-all")?.addEventListener("click", () => {
    dcCopy(dcCopyablePaths()).catch(log);
  });

  $("dep-collect-close")?.addEventListener("click", () => {
    if (DEP_COLLECT.running) {
      // Both tasks register a cancel flag. Keep the modal open until the poll
      // reports done — a copy in flight finishes its current chunk first.
      if (DEP_COLLECT.taskId != null) {
        invoke("cancel_task", { taskId: DEP_COLLECT.taskId }).catch(log);
      }
      return;
    }
    dcClose();
  });

  document.querySelectorAll("#dep-collect-filter [data-dc-filter]").forEach((btn) => {
    btn.addEventListener("click", () => {
      DEP_COLLECT.filter = btn.getAttribute("data-dc-filter") || "all";
      dcApplyFilterUi();
      dcRenderList();
    });
  });

  // "Change in Settings". Close Download Dependencies too when it's underneath —
  // first, so dcClose doesn't re-run its scan — or Settings opens behind it.
  $("dep-collect-dest")?.addEventListener("click", (event) => {
    if (!event.target.closest?.("[data-dc-settings]") || DEP_COLLECT.running) return;
    if (!$("dep-scan-backdrop")?.classList.contains("hidden")) depScanClose();
    dcClose();
    document.querySelector('[data-sidebar-link="settings"]')?.click();
  });

  // Delegated, because rows are re-rendered on every scan and copy.
  $("dep-collect-list")?.addEventListener("click", (event) => {
    const copyBtn = event.target.closest?.("[data-dc-copy]");
    if (copyBtn) {
      const it = DEP_COLLECT.items[Number(copyBtn.getAttribute("data-dc-copy"))];
      if (it?.path) dcCopy([it.path]).catch(log);
      return;
    }
    const reveal = event.target.closest?.("[data-dc-reveal]");
    if (reveal) {
      const it = DEP_COLLECT.items[Number(reveal.getAttribute("data-dc-reveal"))];
      const path = it ? dcRowWhere(it) : "";
      if (path) invoke("show_in_explorer", { path }).catch(log);
    }
  });
}

window.__refreshVarDetailsView = function () {
  renderVarDetails();
};

function closeVarDetailsView() {
  const varDetails = $("var-details-view");
  const varPackages = $("var-packages-view");
  if (!varDetails || !varPackages) return;
  varDetails.classList.add("hidden");
  varPackages.classList.remove("hidden");
  const sidebarLinks = document.querySelectorAll("[data-sidebar-link]");
  sidebarLinks.forEach((link) => {
    link.classList.toggle(
      "active",
      link.getAttribute("data-sidebar-link") === "var-packages"
    );
  });
}

// Mirrors the Rust `naming::creator_from_package_id` rule: the creator is
// the leading dot-segment of the package id, or null if the id has no dot.
function deriveCreatorFromPackageId(packageId) {
  if (!packageId) return null;
  const idx = String(packageId).indexOf(".");
  if (idx <= 0) return null;
  return String(packageId).slice(0, idx);
}

// Blocked creators are global across pages (Overview + Find Duplicates share
// the same DB-backed flag), so we keep them in a module-level Set rather than
// on `state` — `state` is snapshotted per-page via JSON.parse/stringify, which
// would clobber a Set anyway. Populated once at startup from the DB, then
// updated in-place whenever the user right-clicks a candidate and picks
// "Disable creator". Render-time filters consult this Set so already-displayed
// rows from the newly-blocked creator vanish immediately, without a rescan.
const _blockedCreators = new Set();

function isCreatorBlockedForPackage(packageId) {
  const creator = deriveCreatorFromPackageId(packageId);
  return creator != null && _blockedCreators.has(creator);
}

async function loadBlockedCreators() {
  if (!invoke) return;
  try {
    const names = await invoke("list_blocked_creators");
    _blockedCreators.clear();
    if (Array.isArray(names)) {
      for (const n of names) _blockedCreators.add(n);
    }
  } catch (err) {
    addLog(`Failed to load blocked creators: ${String(err)}`);
  }
}

// Favorite packages / creators: global DB-backed sets, same rationale as
// _blockedCreators above (module Sets survive the per-page state
// snapshotting). Bulk-loaded once at startup, mutated in place on toggle so
// every star surface (list rows, cards, details header, Favorite Source
// picks) reads the same truth without per-item DB fetches.
const _favoritePackages = new Set();
const _favoriteCreators = new Set();

function isPackageFavorite(packageId) {
  return packageId != null && _favoritePackages.has(String(packageId));
}

function isCreatorFavoriteForPackage(packageId) {
  const creator = deriveCreatorFromPackageId(packageId);
  return creator != null && _favoriteCreators.has(creator);
}

async function loadFavorites() {
  if (!invoke) return;
  try {
    const ids = await invoke("list_favorite_packages");
    _favoritePackages.clear();
    if (Array.isArray(ids)) {
      for (const id of ids) _favoritePackages.add(id);
    }
  } catch (err) {
    addLog(`Failed to load favorite packages: ${String(err)}`);
  }
  try {
    const names = await invoke("list_favorite_creators");
    _favoriteCreators.clear();
    if (Array.isArray(names)) {
      for (const n of names) _favoriteCreators.add(n);
    }
  } catch (err) {
    addLog(`Failed to load favorite creators: ${String(err)}`);
  }
}

// Re-paint every surface that shows a package star. Both calls are safe
// no-ops when their views are hidden or empty.
function refreshFavoriteIndicators() {
  renderVarPackages();
  renderVarDetailsFavoriteButton();
  dbPkgsRender();
}

async function togglePackageFavorite(packageId) {
  if (!packageId || !invoke) return;
  const next = _favoritePackages.has(packageId) ? 0 : 1;
  // Optimistic flip; rolled back if the DB write fails.
  if (next === 1) _favoritePackages.add(packageId);
  else _favoritePackages.delete(packageId);
  refreshFavoriteIndicators();
  try {
    await invoke("set_package_flag", { packageId, flag: next });
  } catch (err) {
    if (next === 1) _favoritePackages.delete(packageId);
    else _favoritePackages.add(packageId);
    refreshFavoriteIndicators();
    addLog(`Favorite update failed — ${String(err)}`);
  }
}

// Block the creator that owns this package_id. Updates the DB via the
// `set_creator_flag` command (flag 2 = BLOCKED, matches CREATOR_FLAG_BLOCKED
// in db.rs), then refreshes the in-memory set and re-renders both pages so
// already-displayed rows from this creator disappear.
async function blockCreatorByPackageId(packageId) {
  const creator = deriveCreatorFromPackageId(packageId);
  if (!creator) {
    addLog(`Could not extract a creator name from "${packageId}".`);
    return;
  }
  if (_blockedCreators.has(creator)) {
    addLog(`${creator} is already disabled.`);
    return;
  }
  if (!invoke) return;
  try {
    await invoke("set_creator_flag", { creatorName: creator, flag: 2 });
    _blockedCreators.add(creator);
    // The flag is one DB column, so blocking overwrites a favorite — keep the
    // in-memory mirror consistent or Favorite Source would chase a ghost.
    _favoriteCreators.delete(creator);
    addLog(`Disabled creator: ${creator}. Their packages will no longer appear as candidates.`);
    // Clear any keepMap entries that now point at a blocked creator —
    // otherwise an Apply could still relocate to it via a stale pick.
    if (state.scan?.groups) {
      let cleared = 0;
      for (const group of state.scan.groups) {
        const v = state.keepMap[group.key];
        if (!v || v === KEEP_ALL_VALUE) continue;
        const pid = String(v).split(":")[0];
        if (isCreatorBlockedForPackage(pid)) {
          state.keepMap[group.key] = state.defaultKeepMap[group.key] ?? KEEP_ALL_VALUE;
          cleared += 1;
        }
      }
      if (cleared > 0) bumpKeepMutation();
    }
    // Mirror the cleanup on Missing Resources picks — replacement_pkg pointing
    // at a now-blocked creator would silently re-introduce the same broken
    // ref when Run executes, so drop those entries and let the UI re-render.
    const mrState = state.missingResources;
    if (mrState && mrState.replacementMap) {
      for (const k of Object.keys(mrState.replacementMap)) {
        const entry = mrState.replacementMap[k];
        if (entry && isCreatorBlockedForPackage(entry.replacement_pkg || "")) {
          delete mrState.replacementMap[k];
        }
      }
    }
    if (typeof renderGroups === "function") renderGroups();
    if (typeof renderDetail === "function") renderDetail();
    if (typeof dbfRenderGroups === "function") dbfRenderGroups();
    if (typeof dbfRenderDetail === "function") dbfRenderDetail();
    if (typeof window.__refreshMissingResourcesView === "function") {
      window.__refreshMissingResourcesView();
    }
  } catch (err) {
    addLog(`Failed to disable creator ${creator}: ${String(err)}`);
  }
}

// Build a synthetic VarPackageListItem from an arbitrary .var path the user
// picked or dropped onto the page. Calls get_var_file_stats for size/modified
// + scene preview, and reuses any cached row from state.varPackagesItems so
// the indexed flag survives.
async function loadVarDetailsFromPath(filePath) {
  if (!filePath || !invoke) return;
  const trimmed = String(filePath).trim();
  if (!trimmed) return;
  if (!/\.var$/i.test(trimmed)) {
    const message = `Only .var files are supported (got "${trimmed}").`;
    addLog(`VAR Details: ${message}`);
    state.varDetails.resourcesError = message;
    renderVarDetails();
    return;
  }

  let stats = null;
  try {
    stats = await invoke("get_var_file_stats", { packagePath: trimmed });
  } catch (error) {
    const message = `Could not read "${trimmed}" — ${String(error)}`;
    addLog(`VAR Details: ${message}`);
    state.varDetails.resourcesError = message;
    renderVarDetails();
    return;
  }

  const fileName = trimmed.split(/[\\/]/).pop() || trimmed;
  const packageId = fileName.replace(/\.var$/i, "");
  const cached = (state.varPackagesItems ?? []).find(
    (it) => it.package_id === packageId || it.file_path === trimmed
  );

  const item = {
    file_path: trimmed,
    file_name: fileName,
    package_id: packageId,
    creator: cached?.creator ?? deriveCreatorFromPackageId(packageId),
    size_bytes: stats?.size_bytes ?? cached?.size_bytes ?? 0,
    modified_ms: stats?.modified_ms ?? cached?.modified_ms ?? null,
    indexed: cached ? Boolean(cached.indexed) : false,
    scene_image_data: stats?.scene_image_data ?? null,
  };

  showVarDetailsView();
  selectVarDetailsItem(item, "local");
}

// Send a VAR (by path) to another page as its Target VAR and navigate there.
// Shared by the VAR Details "Send as Target VAR" tile and the VAR Packages
// grid right-click menu. `page`: "db-find" | "missing-resources" | "internalize-resources".
function sendVarToTargetPage(page, path) {
  const target = String(path || "").trim();
  if (!target) return;
  const parentDir = () => {
    const idx = Math.max(target.lastIndexOf("\\"), target.lastIndexOf("/"));
    return idx > 0 ? target.slice(0, idx) : "";
  };
  const goto = (route) => {
    const link = document.querySelector(`[data-sidebar-link="${route}"]`);
    if (link) link.click();
    else if (window.__switchPage) window.__switchPage(route);
  };

  if (page === "missing-resources") {
    goto("missing-resources");
    if (state.missingResources) {
      state.missingResources.targetVar = target;
      // Seed the VAR Folder from the file's parent directory when empty, so the
      // user doesn't have to fill it in separately to scan.
      if (!state.missingResources.inputDir) {
        const p = parentDir();
        if (p) state.missingResources.inputDir = p;
      }
    }
    const targetEl = $("missing-target-var");
    if (targetEl) targetEl.value = target;
    const dirEl = $("missing-input-dir");
    if (dirEl && !dirEl.value && state.missingResources?.inputDir) {
      dirEl.value = state.missingResources.inputDir;
    }
    renderVamSources();
    addLog(`Sent target VAR to Missing Resources: ${target}`);
    return;
  }

  if (page === "internalize-resources") {
    goto("internalize-resources");
    if (state.internalize) {
      state.internalize.targetVar = target;
      if (!state.internalize.inputDir) {
        const p = parentDir();
        if (p) state.internalize.inputDir = p;
      }
    }
    const targetEl = $("internalize-target-var");
    if (targetEl) {
      targetEl.value = target;
      // The page's own input listener syncs state + re-renders its VAR info.
      targetEl.dispatchEvent(new Event("input", { bubbles: true }));
    }
    const dirEl = $("internalize-input-dir");
    if (dirEl && !dirEl.value && state.internalize?.inputDir) {
      dirEl.value = state.internalize.inputDir;
    }
    renderVamSources();
    addLog(`Sent target VAR to Internalize Resources: ${target}`);
    return;
  }

  // Default hand-off target is Clean VARs (db-find), the sole workspace page.
  goto("db-find");
  const dbfInput = $("dbf-target-var-path");
  if (dbfInput) {
    dbfInput.value = target;
    dbfInput.dispatchEvent(new Event("input", { bubbles: true }));
  }
  addLog(`Sent target VAR to Clean VARs: ${target}`);
}

// Sums DbFindResponse.groups into the per-package overlap summary the UI
// renders. shared* metrics are denominated in "this VAR's resources" — every
// group with at least one db_match counts as one shared resource. perPackage
// aggregates how many of THIS VAR's resources also live inside each other
// package, so the user can see who could most benefit from deduping against
// this base candidate.
function computeVarOverlap(response) {
  const totalResources = Number(response?.source_resources ?? 0);
  let sharedResources = 0;
  let reclaimable = 0;
  const perPackage = new Map();
  for (const g of response?.groups ?? []) {
    const matches = Array.isArray(g.db_matches) ? g.db_matches : [];
    if (matches.length === 0) continue;
    sharedResources += 1;
    reclaimable += Number(g.reclaimable_bytes ?? 0);
    const groupSize = Number(g.size ?? 0);
    // Source path inside THIS var that's being shared. Collapse multiple
    // source_refs (rare; same CRC at different paths) by taking the first.
    const srcPath = g.source_refs?.[0]?.internal_path ?? "";
    for (const m of matches) {
      const key = String(m.package_id ?? "");
      if (!key) continue;
      const slot = perPackage.get(key) ?? {
        sharedCount: 0,
        sharedBytes: 0,
        filePath: m.file_path ?? "",
        resources: [],
      };
      slot.sharedCount += 1;
      slot.sharedBytes += groupSize;
      slot.resources.push({
        internal_path: srcPath,
        other_path: m.internal_path ?? "",
        size: groupSize,
      });
      perPackage.set(key, slot);
    }
  }
  const unique = Math.max(0, totalResources - sharedResources);
  return {
    totalResources,
    sharedResources,
    unique,
    overlapPercent: totalResources ? (sharedResources / totalResources) * 100 : 0,
    uniquePercent: totalResources ? (unique / totalResources) * 100 : 0,
    reclaimable,
    // Full per-package list, unsliced. The renderer sorts and slices based
    // on the current topPackagesSortBy mode so toggling between count/size
    // is instant and doesn't require recomputing the overlap.
    topPackages: [...perPackage.entries()]
      .map(([packageId, v]) => ({ packageId, ...v })),
    distinctPackages: perPackage.size,
  };
}

// Reshape DbFindResponse.groups into one row per source_ref so the resource
// table can render a flat list with per-row "shared with N" badges. A single
// CRC may appear multiple times inside the same VAR (rare), so we expand
// every source_ref individually.
function buildVarDetailsResources(response) {
  const out = [];
  for (const g of response?.groups ?? []) {
    const sharedWith = Array.isArray(g.db_matches) ? g.db_matches.length : 0;
    const truncatedExtra = Number(g.db_matches_truncated ?? 0);
    for (const ref of g.source_refs ?? []) {
      out.push({
        package_id: ref.package_id,
        package_file: ref.file_path,
        internal_path: ref.internal_path,
        size: Number(ref.size ?? g.size ?? 0),
        crc32: g.crc32,
        crc32_hex: g.crc32_hex,
        shared_with: sharedWith,
        shared_with_truncated: truncatedExtra,
      });
    }
  }
  // Order: most-shared first, then by path so similar resources cluster.
  out.sort((a, b) => (b.shared_with - a.shared_with) || a.internal_path.localeCompare(b.internal_path));
  return out;
}

// Single entry point for both Scan to Database and Scan Local. Calls
// start_db_find_task with the right request, polls progress locally, then
// stores the DbFindResponse + computed overlap on state.varDetails. For
// DB-sourced items the request is sent with `source_package_id` so the
// backend harvests CRCs from the index instead of opening a (possibly
// missing) .var on disk — Scan Local then matches those CRCs against the
// picked folder, giving accurate "shared with" + size data even for
// packages that aren't local.
async function runVarDetailsAnalysis(mode) {
  const item = state.varDetails.item;
  if (!item || !invoke) return;
  if (state.varDetails.scanning) return;
  if (mode !== "database" && mode !== "local") return;

  const useDbSource = state.varDetails.itemSource === "db";
  if (!useDbSource && !item.file_path) {
    state.varDetails.resourcesError = "No file path available for this package.";
    renderVarDetails();
    return;
  }

  let inputDir = null;
  if (mode === "local") {
    const dirRaw = state.varDetails.inputDir || $("var-details-folder-input")?.value || "";
    inputDir = String(dirRaw).trim();
    if (!inputDir) {
      const message = "Set your VaM directory in Settings before running Scan Local.";
      addLog(`VAR Details: ${message}`);
      state.varDetails.resourcesError = message;
      state.varDetails.dbFindResult = null;
      state.varDetails.overlap = null;
      state.varDetails.assetSource = "none";
      renderVarDetails();
      return;
    }
    state.varDetails.inputDir = inputDir;
    // Mirror to the global #input-dir so other pages and save_config pick it up.
    const globalInput = $("input-dir");
    if (globalInput && globalInput.value.trim() !== inputDir) {
      globalInput.value = inputDir;
    }
    try {
      await invoke("save_config", { config: buildCurrentConfig() });
    } catch (_error) {}
  }

  const packageId = state.varDetails.packageId;
  state.varDetails.scanning = true;
  state.varDetails.scanKind = mode;
  state.varDetails.progress = 0;
  state.varDetails.progressMessage = mode === "database" ? "Querying database…" : "Scanning folder…";
  state.varDetails.resourcesError = null;
  state.varDetails.dbFindResult = null;
  state.varDetails.overlap = null;
  state.varDetails.resources = [];
  state.varDetails.assetSource = "none";
  state.varDetails.assetPage = 0;
  state.varDetails.search = "";
  state.varDetails.expandedPackages = new Set();
  const searchInput = $("var-details-search");
  if (searchInput) searchInput.value = "";
  renderVarDetails();

  let taskId = null;
  try {
    // DB-sourced items: harvest source CRCs from the indexed resource list
    // (no disk read of the .var). Local-sourced items: harvest from the file.
    // include_unshared lets the response carry every harvested resource, so
    // the Resources table can switch between All / Shared without a reharvest.
    const requestPayload = useDbSource
      ? {
          mode,
          source_package_id: state.varDetails.packageId,
          input_dir: inputDir,
          additional_input_dirs: getAdditionalDirs("varDetails"),
          include_vap: false,
          include_unshared: true,
        }
      : {
          mode,
          target_var_path: item.file_path,
          input_dir: inputDir,
          additional_input_dirs: getAdditionalDirs("varDetails"),
          include_vap: false,
          include_unshared: true,
        };
    const handle = await invoke("start_db_find_task", { request: requestPayload });
    taskId = handle?.id ?? null;
    state.varDetails.scanTaskId = taskId;
  } catch (error) {
    const message = String(error);
    addLog(`VAR Details: scan failed to start — ${message}`);
    state.varDetails.scanning = false;
    state.varDetails.scanKind = null;
    state.varDetails.progressMessage = "";
    state.varDetails.resourcesError = message;
    renderVarDetails();
    return;
  }

  if (!taskId) {
    state.varDetails.scanning = false;
    state.varDetails.scanKind = null;
    renderVarDetails();
    return;
  }

  let result = null;
  let pollError = null;
  try {
    while (true) {
      await new Promise((resolve) => setTimeout(resolve, 400));
      let payload;
      try {
        payload = await invoke("get_task_progress", { taskId });
      } catch (error) {
        pollError = String(error);
        break;
      }
      if (!payload) break;
      const fraction = Number(payload.progress ?? 0);
      const pct = Math.max(0, Math.min(100, Math.round(fraction * 100)));
      state.varDetails.progress = pct;
      state.varDetails.progressMessage = String(payload.message ?? "Working…");
      renderVarDetailsPipeline();
      renderVarDetailsOverlap();
      if (payload.error) {
        pollError = String(payload.error);
        break;
      }
      if (payload.done) {
        result = payload.db_find_result ?? null;
        break;
      }
    }
  } finally {
    if (taskId) {
      try {
        await invoke("clear_task", { taskId });
      } catch (_error) {}
    }
  }

  // Bail if the user has switched to a different package mid-flight.
  if (state.varDetails.packageId !== packageId) {
    state.varDetails.scanning = false;
    state.varDetails.scanKind = null;
    state.varDetails.scanTaskId = null;
    return;
  }

  if (pollError) {
    addLog(`VAR Details: scan failed — ${pollError}`);
    state.varDetails.resourcesError = pollError;
  } else if (result) {
    state.varDetails.dbFindResult = result;
    state.varDetails.overlap = computeVarOverlap(result);
    state.varDetails.resources = buildVarDetailsResources(result);
    state.varDetails.assetSource = mode;
    // A successful Scan Local proves the file is on disk regardless of how
    // the item was originally loaded (e.g. a DB row that turned out to also
    // exist locally), so flip the flag so Path / Send-as-Target appear.
    if (mode === "local") {
      state.varDetails.itemIsLocal = true;
    }
  }
  state.varDetails.scanning = false;
  state.varDetails.scanKind = null;
  state.varDetails.scanTaskId = null;
  renderVarDetails();
}

// ---- VAR Details "download this VAR" availability banner -------------------
// Shown only when the opened VAR isn't present locally. Resolves a Hub/mirror
// source, then downloads it (reusing start_download_one_task) with progress +
// cancel, and reloads the now-present file on success.

// VAR Details prefers to re-download a missing var into the folder the DB
// expected it in (re-download in place). Returns "" when there's no such path,
// and the caller falls back to the central downloads folder (ensureDownloadsDir).
function varDetailsDownloadsDir() {
  const item = state.varDetails.item;
  if (item && item.file_path) {
    const p = String(item.file_path);
    const idx = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
    if (idx > 0) return p.slice(0, idx);
  }
  return "";
}

async function resolveVarSource(packageId) {
  const av = state.varDetails.availability;
  if (!invoke || !packageId) return;
  av.state = "resolving";
  av.resolvedFor = packageId;
  av.error = null;
  renderVarDetailsAvailability();
  let taskId = null;
  let result = null;
  try {
    const handle = await invoke("start_resolve_var_source_task", { packageId });
    taskId = handle && handle.id != null ? handle.id : null;
    if (taskId == null) throw new Error("failed to start source check");
    while (true) {
      await new Promise((r) => setTimeout(r, 250));
      if (state.varDetails.packageId !== packageId) return; // user navigated away
      const payload = await invoke("get_task_progress", { taskId });
      if (!payload) break;
      if (payload.error) throw new Error(String(payload.error));
      if (payload.done) { result = payload.var_source_result ?? null; break; }
    }
    try { await invoke("clear_task", { taskId }); } catch (_e) {}
  } catch (e) {
    if (state.varDetails.packageId === packageId) {
      av.state = "unavailable";
      av.error = String(e);
      renderVarDetailsAvailability();
    }
    return;
  }
  if (state.varDetails.packageId !== packageId) return;
  if (result && result.download_url) {
    av.state = "available";
    av.url = result.download_url;
    av.filename = result.filename || `${packageId}.var`;
    av.size = result.file_size != null ? Number(result.file_size) : null;
    av.host = result.host || "";
    av.error = null;
  } else {
    av.state = "unavailable";
    av.error = result && result.error ? String(result.error) : null;
  }
  renderVarDetailsAvailability();
}

// Hands the download to the central Downloads manager; the banner shows a
// "downloading (see Downloads)" note and reloads the VAR on completion. Live
// progress + cancel live in the top-bar Downloads popover.
async function downloadVarItself() {
  const av = state.varDetails.availability;
  const packageId = state.varDetails.packageId;
  if (!av.url || !packageId) return;
  // Prefer re-downloading in place; otherwise use the central downloads folder
  // (Settings → library → prompt-and-save).
  const destDir = varDetailsDownloadsDir() || (await ensureDownloadsDir());
  if (!destDir) {
    av.error = "No downloads folder selected.";
    renderVarDetailsAvailability();
    addLog("VAR Details: no downloads folder selected.");
    return;
  }
  const filename = av.filename || `${packageId}.var`;
  av.state = "downloading";
  av.error = null;
  renderVarDetailsAvailability();
  queueDownload({
    packageId,
    url: av.url,
    filename,
    host: av.host,
    destDir,
    label: filename,
    onDone: (status, info) => {
      if (state.varDetails.packageId !== packageId) return; // navigated away
      if (status === "done" || status === "exists") {
        av.state = "done";
        renderVarDetailsAvailability();
        const finalPath = (info && info.localPath) || "";
        if (finalPath) {
          varPackageThumbCache.delete(finalPath);
          loadVarDetailsFromPath(finalPath).catch(() => {});
        }
      } else if (status === "cancelled") {
        av.state = "available";
        renderVarDetailsAvailability();
      } else {
        av.state = "failed";
        av.error = "Download failed — see the Downloads panel.";
        renderVarDetailsAvailability();
      }
    },
  });
}

function renderVarDetailsAvailability() {
  const container = $("var-details-availability");
  if (!container) return;
  const item = state.varDetails.item;
  const av = state.varDetails.availability;
  // Show only when the file isn't actually on disk (filePresent), and presence
  // was checked. Driven by filePresent — NOT itemIsLocal — so a Scan Local
  // (which flips itemIsLocal for its own UI) doesn't hide a not-downloaded VAR.
  if (!item || av.filePresent || !av.presenceChecked) {
    container.classList.add("hidden");
    container.innerHTML = "";
    return;
  }
  container.classList.remove("hidden");
  // Resolve the source once per package; resolveVarSource flips state + re-renders.
  if (av.resolvedFor !== state.varDetails.packageId && av.state === "idle") {
    resolveVarSource(state.varDetails.packageId);
    return;
  }

  const sizeStr = av.size && av.size > 0 ? formatBytesLocal(av.size) : "";
  const hostLabel =
    av.host === "hub" ? "Hub"
    : av.host === "pixeldrain" ? "Pixeldrain"
    : av.host === "mediafire" ? "MediaFire"
    : (av.host || "");

  let icon = "cloud_download";
  let title = "Not in your library";
  let sub = "";
  let actions = "";
  let progress = "";
  let cls = "";

  switch (av.state) {
    case "resolving":
      sub = "Checking the Hub…";
      cls = "is-resolving";
      break;
    case "available": {
      sub = [hostLabel, sizeStr].filter(Boolean).join(" · ") || "Available to download";
      if (av.error) sub = `${sub} — ${escapeHtml(av.error)}`;
      actions = av.host === "mediafire"
        ? `<button class="ghost-button" type="button" data-vda="open"><span class="material-symbols-outlined">open_in_new</span><span>Open (MediaFire)</span></button>`
        : `<button class="primary-button" type="button" data-vda="download"><span class="material-symbols-outlined">download</span><span>Download</span></button>`;
      break;
    }
    case "downloading":
    case "cancelling":
      icon = "downloading";
      sub = hostLabel ? `Downloading from ${hostLabel} — see Downloads` : "Downloading — see Downloads";
      cls = "is-resolving";
      actions = `<button class="ghost-button" type="button" data-vda="open-downloads"><span class="material-symbols-outlined">download</span><span>View Downloads</span></button>`;
      break;
    case "done":
      icon = "check_circle";
      title = "Downloaded";
      sub = "Now in your library";
      cls = "is-done";
      break;
    case "failed":
      icon = "error";
      title = "Download failed";
      sub = av.error ? escapeHtml(av.error) : "Try again.";
      cls = "is-failed";
      actions = `<button class="primary-button" type="button" data-vda="download"><span class="material-symbols-outlined">refresh</span><span>Retry</span></button>`;
      break;
    case "unavailable":
      icon = "cloud_off";
      title = "Not available to download";
      sub = av.error
        ? `Couldn't reach the Hub — ${escapeHtml(av.error)}`
        : "Not on the Hub or in your imported links.";
      cls = "is-unavailable";
      break;
    default:
      sub = "Not in your library";
  }

  container.className = `var-details-availability ${cls}`.trim();
  container.innerHTML =
    `<div class="var-details-availability-main">` +
    `<span class="material-symbols-outlined var-details-availability-icon">${icon}</span>` +
    `<div class="var-details-availability-text">` +
    `<span class="var-details-availability-title">${escapeHtml(title)}</span>` +
    `<span class="var-details-availability-sub">${sub}</span>` +
    `</div>` +
    `<div class="var-details-availability-actions">${actions}</div>` +
    `</div>` +
    progress;
}

function renderVarDetails() {
  const item = state.varDetails.item;

  const set = (id, value) => {
    const el = $(id);
    if (el) el.textContent = value;
  };

  const titleEl = $("var-details-title");
  if (titleEl) {
    titleEl.textContent = item
      ? `VAR Details: ${item.file_name ?? item.package_id ?? ""}`
      : "VAR Details";
  }

  // "Download this VAR" banner (only when the file isn't present locally).
  renderVarDetailsAvailability();

  if (!item) {
    set("var-details-meta-filename", "—");
    set("var-details-meta-creator", "—");
    set("var-details-meta-size", "—");
    set("var-details-meta-modified", "—");
    set("var-details-meta-status", "No package selected");
    const statusTile = $("var-details-meta-status-tile");
    if (statusTile) {
      statusTile.classList.remove("is-indexed");
      statusTile.classList.remove("is-unindexed");
    }
    const statusDot = $("var-details-status-dot");
    if (statusDot) {
      statusDot.classList.remove("is-indexed");
      statusDot.classList.remove("is-unindexed");
    }
    const pathTile = $("var-details-meta-path-tile");
    if (pathTile) pathTile.hidden = true;
    const sendToTile = $("var-details-meta-sendto-tile");
    if (sendToTile) sendToTile.hidden = true;
    renderCreatorFlagButtons(null);
    renderVarDetailsFavoriteButton();
    renderVarDetailsPreview(null);
    const scanDb = $("var-details-scan-db-button");
    const scanLocal = $("var-details-scan-local-button");
    if (scanDb) scanDb.disabled = true;
    if (scanLocal) scanLocal.disabled = true;
    renderVarDetailsPipeline();
    renderVarDetailsAssetTable();
    return;
  }

  set("var-details-meta-filename", item.file_name ?? item.package_id ?? "—");
  set("var-details-meta-creator", item.creator ?? "—");
  renderCreatorFlagButtons(item);
  renderVarDetailsFavoriteButton();
  set("var-details-meta-size", formatBytesLocal(item.size_bytes));
  set("var-details-meta-modified", formatVarModifiedMs(item.modified_ms));

  const indexed = Boolean(item.indexed);
  const statusEl = $("var-details-meta-status");
  const statusTile = $("var-details-meta-status-tile");
  const statusDot = $("var-details-status-dot");
  if (statusEl) {
    statusEl.textContent = indexed ? "Indexed / Active" : "Not Indexed";
  }
  if (statusTile) {
    statusTile.classList.toggle("is-indexed", indexed);
    statusTile.classList.toggle("is-unindexed", !indexed);
  }
  if (statusDot) {
    statusDot.classList.toggle("is-indexed", indexed);
    statusDot.classList.toggle("is-unindexed", !indexed);
  }

  renderVarDetailsPreview(item.scene_image_data ?? null);

  const pathTile = $("var-details-meta-path-tile");
  const pathValue = $("var-details-meta-path");
  const pathAction = $("var-details-meta-path-action");
  const filePath = item.file_path ?? "";
  // Path + Send tiles share the same gate: only meaningful when the file is
  // actually on local disk (loaded from a path/folder, or a local scan ran).
  // A DB-only row's file_path is just an index hint and may be stale.
  const showLocalActions = filePath && state.varDetails.itemIsLocal;
  if (pathTile) pathTile.hidden = !showLocalActions;
  if (pathValue) {
    pathValue.textContent = filePath || "—";
    pathValue.title = filePath;
  }
  if (pathAction) {
    pathAction.hidden = !showLocalActions;
    pathAction.dataset.filePath = filePath;
  }

  const sendToTile = $("var-details-meta-sendto-tile");
  if (sendToTile) sendToTile.hidden = !showLocalActions;

  const packageInput = $("var-details-package-input");
  if (packageInput && packageInput.value !== (item.file_path ?? "")) {
    packageInput.value = item.file_path ?? "";
  }

  const scanDbLabel = $("var-details-scan-db-label");
  if (scanDbLabel) {
    scanDbLabel.textContent = indexed ? "Re-Scan to Database" : "Scan to Database";
  }
  const scanDb = $("var-details-scan-db-button");
  const scanLocal = $("var-details-scan-local-button");
  const noPath = !item.file_path;
  const isDbSource = state.varDetails.itemSource === "db";
  // For DB-sourced items, both scans harvest source CRCs from the index
  // (`source_package_id`) so neither needs the .var to exist on disk. For
  // local items, both modes need a real path.
  const ready = isDbSource || !noPath;
  if (scanDb) scanDb.disabled = state.varDetails.scanning || !ready;
  if (scanLocal) scanLocal.disabled = state.varDetails.scanning || !ready;
  // Send-to-Missing-Resources only makes sense when there's a real .var on
  // disk — DB-only synthetic items have no file path the other page can
  // open. Mirror Scan Local's gating logic.
  const sendMissing = $("var-details-send-missing-button");
  if (sendMissing) sendMissing.disabled = state.varDetails.scanning || noPath;

  // Reading dependencies means opening the .var to parse meta.json, so it needs
  // a real file on disk — same gate as Send-to-Missing-Resources.
  const scanDeps = $("var-details-scan-deps-button");
  if (scanDeps) scanDeps.disabled = state.varDetails.scanning || noPath;

  renderVarDetailsPipeline();
  renderVarDetailsAssetTable();
}

// Show/hide the favorite/block button row and reflect the active flag.
//   flag 1 = favorite, 2 = blocked, anything else = none.
// The row is hidden entirely when there's no creator to act on.
function renderCreatorFlagButtons(item) {
  const wrap = $("var-details-creator-flags");
  if (!wrap) return;
  const creator = item?.creator;
  if (!creator) {
    wrap.hidden = true;
    return;
  }
  wrap.hidden = false;
  const flag = Number(item.creatorFlag) || 0;
  const favBtn = $("var-details-creator-flag-favorite");
  const blockBtn = $("var-details-creator-flag-blocked");
  if (favBtn) favBtn.classList.toggle("is-active", flag === 1);
  if (blockBtn) blockBtn.classList.toggle("is-active", flag === 2);
}

// Header star for the currently open package. State comes from the bulk
// _favoritePackages set, so no per-item DB fetch is needed (unlike
// refreshCreatorFlagForItem for the creator flag).
function renderVarDetailsFavoriteButton() {
  const btn = $("var-details-fav-button");
  if (!btn) return;
  const pid = state.varDetails?.item?.package_id ?? null;
  btn.hidden = !pid;
  if (!pid) return;
  const fav = _favoritePackages.has(pid);
  btn.classList.toggle("is-active", fav);
  btn.setAttribute("aria-pressed", fav ? "true" : "false");
  btn.title = fav ? t("varFavRemove") : t("varFavAdd");
  const label = $("var-details-fav-label");
  if (label) label.textContent = fav ? t("varFavLabelOn") : t("varFavLabelOff");
}

function renderVarDetailsPreview(sceneImageData) {
  const preview = $("var-details-preview");
  if (!preview) return;
  const caption = preview.querySelector(".var-details-preview-caption");
  preview.querySelectorAll("img, .material-symbols-outlined").forEach((el) => el.remove());

  if (sceneImageData) {
    const img = document.createElement("img");
    const src = String(sceneImageData);
    img.src = src.startsWith("data:") ? src : `data:image/jpeg;base64,${src}`;
    img.alt = "Scene preview";
    preview.insertBefore(img, caption ?? null);
    // The image is clickable to open the full-size zoom lightbox.
    preview.classList.add("is-zoomable");
  } else {
    const icon = document.createElement("span");
    icon.className = "material-symbols-outlined";
    icon.textContent = "image_not_supported";
    preview.insertBefore(icon, caption ?? null);
    preview.classList.remove("is-zoomable");
  }
}

// Image zoom lightbox: opens the given image source full-size with a zoom
// slider. Reusable for any preview image; currently driven by the VAR Details
// scene preview. Closes on backdrop click, the X button, or ESC.
function openImageZoom(src, alt) {
  const backdrop = $("image-zoom-backdrop");
  const img = $("image-zoom-img");
  const slider = $("image-zoom-slider");
  if (!backdrop || !img || !slider) return;
  img.src = src;
  img.alt = alt || "Image preview";
  slider.value = "100";
  applyImageZoom(100);
  backdrop.classList.remove("hidden");
}

function closeImageZoom() {
  const backdrop = $("image-zoom-backdrop");
  const img = $("image-zoom-img");
  if (!backdrop || backdrop.classList.contains("hidden")) return;
  backdrop.classList.add("hidden");
  // Drop the data URL so the next open starts clean and frees memory.
  if (img) img.src = "";
}

function applyImageZoom(percent) {
  const img = $("image-zoom-img");
  const stage = $("image-zoom-stage");
  const level = $("image-zoom-level");
  if (!img) return;
  const scale = Math.max(100, Number(percent) || 100) / 100;
  img.style.transform = `scale(${scale})`;
  if (stage) stage.classList.toggle("is-zoomed", scale > 1);
  if (level) level.textContent = `${Math.round(scale * 100)}%`;
}

function setupImageZoom() {
  const backdrop = $("image-zoom-backdrop");
  const slider = $("image-zoom-slider");
  if (!backdrop || !slider) return;
  slider.addEventListener("input", () => applyImageZoom(slider.value));
  // Backdrop / close button (anything carrying data-zoom-close).
  backdrop.addEventListener("click", (event) => {
    if (event.target.closest("[data-zoom-close]")) closeImageZoom();
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") closeImageZoom();
  });
  // Open from the VAR Details scene preview image.
  const preview = $("var-details-preview");
  if (preview) {
    preview.addEventListener("click", () => {
      const img = preview.querySelector("img");
      if (img && img.src) openImageZoom(img.src, img.alt);
    });
  }
}

function renderVarDetailsPipeline() {
  const pct = Math.max(0, Math.min(100, Math.round(state.varDetails.progress || 0)));
  const card = $("var-details-progress-card");
  const fill = $("var-details-pipeline-fill");
  const percent = $("var-details-pipeline-percent");
  const text = $("var-details-pipeline-text");
  const dot = $("var-details-pipeline-dot");
  if (card) card.hidden = !state.varDetails.scanning;
  if (!state.varDetails.scanning) return;
  if (fill) fill.style.width = `${pct}%`;
  if (percent) percent.textContent = `${pct}%`;
  if (text) {
    text.textContent = state.varDetails.progressMessage || "Indexing package…";
  }
  if (dot) {
    dot.classList.toggle("is-active", true);
  }
}

const VAR_DETAILS_CATEGORY_ICONS = {
  geometry: "view_in_ar",
  textures: "image",
  scenes: "movie",
  scripts: "code",
  clothing: "checkroom",
  morphs: "face",
  hair: "cut",
  audio: "graphic_eq",
  documentation: "description",
  custom: "folder",
};

const VAR_DETAILS_EXT_ICONS = {
  png: "image",
  jpg: "image",
  jpeg: "image",
  tga: "image",
  webp: "image",
  bmp: "image",
  gif: "image",
  cs: "code",
  cslist: "code",
  js: "javascript",
  json: "data_object",
  vmi: "category",
  vam: "deployed_code",
  vmb: "deployed_code",
  vap: "deployed_code",
  vaj: "deployed_code",
  obj: "view_in_ar",
  fbx: "view_in_ar",
  glb: "view_in_ar",
  txt: "description",
  md: "description",
  pdf: "description",
  wav: "graphic_eq",
  mp3: "graphic_eq",
  ogg: "graphic_eq",
};

function pickVarDetailsAssetIcon(internalPath) {
  if (!internalPath) return "draft";
  const lower = String(internalPath).toLowerCase();
  const firstSegment = lower.split(/[\\/]/, 1)[0] ?? "";
  if (firstSegment in VAR_DETAILS_CATEGORY_ICONS) {
    return VAR_DETAILS_CATEGORY_ICONS[firstSegment];
  }
  const dot = lower.lastIndexOf(".");
  if (dot >= 0) {
    const ext = lower.slice(dot + 1);
    if (ext in VAR_DETAILS_EXT_ICONS) return VAR_DETAILS_EXT_ICONS[ext];
  }
  return "draft";
}

// Mirror of Rust naming::category_name_for_path — keep in sync. Path-based
// prefixes win over extension fallbacks so e.g. a .json under Saves/scene/ is
// classified Scene rather than Other.
function deriveVarDetailsCategory(internalPath) {
  const lower = String(internalPath ?? "").toLowerCase();
  if (!lower) return "—";
  if (lower.startsWith("saves/scene/") || lower.includes("/saves/scene/")) return "Scene";
  if (lower.startsWith("custom/subscene/") || lower.includes("/subscene/")) return "SubScene";
  if (lower.includes("/morphs/") || lower.startsWith("custom/atom/person/morphs/")) return "Morph";
  if (lower.includes("/clothing/") || lower.startsWith("custom/clothing/")) return "Clothing";
  if (lower.includes("/hair/") || lower.startsWith("custom/hair/")) return "Hair";
  if (lower.includes("/textures/") || lower.startsWith("custom/atom/person/textures/")) return "Texture";
  if (lower.includes("/scripts/") || lower.startsWith("custom/scripts/")) return "Scripts";
  if (lower.includes("/sounds/") || lower.startsWith("custom/sounds/")) return "Audio";
  if (lower.includes("/assets/") || lower.startsWith("custom/assets/")) return "Asset";
  if (lower.includes("/presets/")) return "Preset";
  const dot = lower.lastIndexOf(".");
  const ext = dot >= 0 ? lower.slice(dot + 1) : "";
  switch (ext) {
    case "vmi":
    case "vmb":
      return "Morph";
    case "vam":
    case "vaj":
    case "vab":
      return "Asset";
    case "vap":
      return "Preset";
    case "jpg":
    case "jpeg":
    case "png":
    case "tif":
    case "tiff":
    case "tga":
      return "Texture";
    case "wav":
    case "mp3":
    case "ogg":
      return "Audio";
    case "cs":
    case "cslist":
    case "dll":
      return "Scripts";
    default:
      return "Other";
  }
}

function formatCrc32Hex(crc) {
  if (crc == null) return "—";
  if (typeof crc === "string" && crc.length > 0) return crc.toUpperCase();
  const num = Number(crc);
  if (!Number.isFinite(num)) return "—";
  return (num >>> 0).toString(16).padStart(8, "0").toUpperCase();
}

function renderVarDetailsOverlap() {
  const overlap = state.varDetails.overlap;
  const card = $("var-details-overlap-card");
  const sourceTag = $("var-details-overlap-source");
  const hint = $("var-details-overlap-hint");
  const verdict = $("var-details-verdict");
  const set = (id, value) => {
    const el = $(id);
    if (el) el.textContent = value;
  };

  const sourceLabel = state.varDetails.assetSource === "database"
    ? "Database"
    : state.varDetails.assetSource === "local"
    ? "Local Folder"
    : "Not Scanned";
  if (sourceTag) sourceTag.textContent = sourceLabel;

  // Surface scan errors prominently — the user is most likely to be looking
  // at this card, so an error in start_db_find_task (e.g., the file isn't a
  // valid zip, or the folder is missing) needs to be visible here, not just
  // in the hidden console log.
  if (state.varDetails.resourcesError && !state.varDetails.scanning) {
    set("var-details-stat-total", "—");
    set("var-details-stat-shared", "—");
    set("var-details-stat-shared-sub", "");
    set("var-details-stat-unique", "—");
    set("var-details-stat-unique-sub", "");
    set("var-details-stat-reclaim", "—");
    if (hint) hint.textContent = "The last scan failed.";
    if (verdict) {
      verdict.hidden = false;
      verdict.dataset.kind = "warn";
      verdict.textContent = `Scan failed: ${state.varDetails.resourcesError}`;
    }
    if (card) card.classList.remove("has-data");
    return;
  }

  if (!overlap) {
    set("var-details-stat-total", "—");
    set("var-details-stat-shared", "—");
    set("var-details-stat-shared-sub", "");
    set("var-details-stat-unique", "—");
    set("var-details-stat-unique-sub", "");
    set("var-details-stat-reclaim", "—");
    if (hint) {
      hint.textContent = state.varDetails.scanning
        ? state.varDetails.progressMessage || "Working…"
        : "Run a scan to compare this package against your collection.";
    }
    if (verdict) verdict.hidden = true;
    if (card) card.classList.remove("has-data");
    return;
  }

  set("var-details-stat-total", String(overlap.totalResources));
  set("var-details-stat-shared", String(overlap.sharedResources));
  set("var-details-stat-shared-sub", `${overlap.overlapPercent.toFixed(1)}% of resources`);
  set("var-details-stat-unique", String(overlap.unique));
  set("var-details-stat-unique-sub", `${overlap.uniquePercent.toFixed(1)}% unique`);
  set("var-details-stat-reclaim", formatBytesLocal(overlap.reclaimable));
  if (card) card.classList.add("has-data");

  if (hint) {
    hint.textContent = state.varDetails.assetSource === "database"
      ? "Compared against every package indexed in the local database."
      : "Compared against every .var in the picked folder.";
  }

  // Verdict — show whichever is more striking. Threshold logic: a strong
  // base candidate has ≥60% overlap; a low-download-value VAR has ≤20% unique.
  if (verdict) {
    let kind = null;
    let message = "";
    if (overlap.totalResources === 0) {
      kind = "neutral";
      message = "No resources to compare.";
    } else if (overlap.overlapPercent >= 60) {
      kind = "good";
      message = `Strong base candidate — ${overlap.sharedResources} of ${overlap.totalResources} resources (${overlap.overlapPercent.toFixed(0)}%) live in ${overlap.distinctPackages} other package${overlap.distinctPackages === 1 ? "" : "s"}, ${formatBytesLocal(overlap.reclaimable)} potentially reclaimable.`;
    } else if (overlap.uniquePercent <= 20) {
      kind = "warn";
      message = `Low download value — only ${overlap.unique} of ${overlap.totalResources} resources (${overlap.uniquePercent.toFixed(0)}%) are not already in your collection.`;
    } else if (overlap.sharedResources > 0) {
      kind = "neutral";
      message = `${overlap.sharedResources} of ${overlap.totalResources} resources (${overlap.overlapPercent.toFixed(0)}%) overlap with ${overlap.distinctPackages} other package${overlap.distinctPackages === 1 ? "" : "s"}.`;
    } else {
      kind = "good";
      message = `Fully unique — none of this package's resources appear in your collection.`;
    }
    verdict.hidden = false;
    verdict.dataset.kind = kind;
    verdict.textContent = message;
  }
}

function renderVarDetailsTopPackages() {
  const list = $("var-details-top-packages-list");
  if (!list) return;
  const overlap = state.varDetails.overlap;
  const all = overlap?.topPackages ?? [];

  // Toggle is only meaningful when the target VAR lives on local disk — its
  // own resource sizes are what feed `sharedBytes`, so DB-only items can't
  // produce a meaningful "by size" ranking. Either scan kind (local or
  // database) is fine as long as itemIsLocal is true.
  const sortBy = state.varDetails.topPackagesSortBy === "size" ? "size" : "count";
  const sortToggle = $("var-details-top-packages-sort");
  const showToggle = state.varDetails.itemIsLocal
    && state.varDetails.assetSource !== "none"
    && all.length > 0;
  if (sortToggle) sortToggle.hidden = !showToggle;
  document.querySelectorAll("[data-top-sort]").forEach((btn) => {
    const isActive = btn.getAttribute("data-top-sort") === sortBy;
    btn.classList.toggle("is-active", isActive);
    btn.setAttribute("aria-selected", isActive ? "true" : "false");
  });
  const hint = $("var-details-top-packages-hint");
  if (hint) {
    hint.textContent = showToggle && sortBy === "size"
      ? "Other VARs that share the most data with this package."
      : "Other VARs that contain the most resources from this package.";
  }

  if (!overlap || all.length === 0) {
    const message = !state.varDetails.item
      ? "Pick a package to begin."
      : state.varDetails.assetSource === "none"
      ? "Run a scan to see overlapping packages."
      : "No other packages share resources with this one.";
    list.innerHTML = `<li class="var-details-top-packages-empty">${escapeHtml(message)}</li>`;
    return;
  }
  const top = [...all]
    .sort((a, b) => sortBy === "size"
      ? b.sharedBytes - a.sharedBytes
      : b.sharedCount - a.sharedCount)
    .slice(0, VAR_DETAILS_TOP_PACKAGES);
  // Bar-fill denominator follows the active sort metric so the longest bar
  // always represents the leader of whichever ranking is active.
  const denom = sortBy === "size"
    ? Math.max(1, top[0]?.sharedBytes ?? 1)
    : Math.max(1, overlap.sharedResources);
  const expanded = state.varDetails.expandedPackages ?? new Set();
  list.innerHTML = top.map((p) => {
    const metric = sortBy === "size" ? p.sharedBytes : p.sharedCount;
    const pct = Math.max(0, Math.min(100, Math.round((metric / denom) * 100)));
    const isExpanded = expanded.has(p.packageId);
    const sharedRows = (p.resources ?? []).map((r) => {
      const path = r.internal_path || r.other_path || "(unknown path)";
      const icon = pickVarDetailsAssetIcon(path);
      return `
        <li class="var-details-shared-resource">
          <span class="var-details-shared-resource-icon">
            <span class="material-symbols-outlined">${escapeHtml(icon)}</span>
          </span>
          <span class="var-details-shared-resource-path" title="${escapeAttribute(path)}">${escapeHtml(path)}</span>
          <span class="var-details-shared-resource-size">${escapeHtml(formatBytesLocal(Number(r.size ?? 0)))}</span>
        </li>
      `;
    }).join("");
    return `
      <li class="var-details-top-package${isExpanded ? " is-expanded" : ""}" data-package-id="${escapeAttribute(p.packageId)}" data-file-path="${escapeAttribute(p.filePath ?? "")}">
        <button class="var-details-top-package-toggle" type="button" data-vd-toggle="1" aria-expanded="${isExpanded ? "true" : "false"}">
          <span class="var-details-top-package-main">
            <span class="var-details-top-package-name">${escapeHtml(p.packageId)}</span>
            <span class="var-details-top-package-meta">${p.sharedCount} resource${p.sharedCount === 1 ? "" : "s"} · ${escapeHtml(formatBytesLocal(p.sharedBytes))}</span>
          </span>
          <span class="var-details-share-bar"><span class="var-details-share-bar-fill" style="width: ${pct}%"></span></span>
          <span class="var-details-top-package-chevron material-symbols-outlined">${isExpanded ? "expand_less" : "expand_more"}</span>
        </button>
        <button class="var-details-top-package-open" type="button" data-vd-open="1" aria-label="Open package details" title="Open package details">
          <span class="material-symbols-outlined">arrow_forward</span>
        </button>
        <ul class="var-details-shared-resources" ${isExpanded ? "" : "hidden"}>
          ${sharedRows || '<li class="var-details-shared-resource is-empty">No resource details available.</li>'}
        </ul>
      </li>
    `;
  }).join("");
}

function renderVarDetailsResourceTable() {
  const tbody = $("var-details-tbody");
  const countEl = $("var-details-asset-count");
  const summaryEl = $("var-details-browser-summary");
  const pagination = $("var-details-pagination");
  if (!tbody) return;

  const resources = state.varDetails.resources ?? [];
  const search = String(state.varDetails.search ?? "").trim().toLowerCase();
  const filterMode = state.varDetails.resourceFilter === "shared" ? "shared" : "all";
  const matchesFilter = (r) =>
    filterMode === "shared" ? Number(r.shared_with ?? 0) > 0 : true;
  const matchesSearch = (r) =>
    !search || String(r.internal_path ?? "").toLowerCase().includes(search);
  const filtered = resources.filter((r) => matchesFilter(r) && matchesSearch(r));

  // Reflect the filter selection on the toggle buttons each render.
  document.querySelectorAll("[data-resource-filter]").forEach((btn) => {
    const isActive = btn.getAttribute("data-resource-filter") === filterMode;
    btn.classList.toggle("is-active", isActive);
    btn.setAttribute("aria-selected", isActive ? "true" : "false");
  });

  if (countEl) {
    if (state.varDetails.assetSource === "none") {
      countEl.textContent = "Not Scanned";
    } else {
      const sourceLabel = state.varDetails.assetSource === "local" ? "Local" : "Database";
      countEl.textContent = `${resources.length} Resource${resources.length === 1 ? "" : "s"} · ${sourceLabel}`;
    }
  }

  const hidePagination = () => {
    if (pagination) pagination.hidden = true;
  };

  if (!state.varDetails.item) {
    tbody.innerHTML = `<tr class="var-details-empty-row"><td colspan="6">Pick a .var package to view its details.</td></tr>`;
    if (summaryEl) summaryEl.textContent = "";
    hidePagination();
    return;
  }
  if (state.varDetails.assetSource === "none" && !state.varDetails.scanning) {
    tbody.innerHTML = `<tr class="var-details-empty-row"><td colspan="6">Choose <b>Scan to Database</b> or <b>Scan Local</b> in Source Configuration to populate the resource list.</td></tr>`;
    if (summaryEl) summaryEl.textContent = "";
    hidePagination();
    return;
  }
  if (state.varDetails.scanning) {
    tbody.innerHTML = `<tr class="var-details-empty-row"><td colspan="6">${escapeHtml(state.varDetails.progressMessage || "Working…")}</td></tr>`;
    if (summaryEl) summaryEl.textContent = "";
    hidePagination();
    return;
  }
  if (state.varDetails.resourcesError) {
    tbody.innerHTML = `<tr class="var-details-empty-row"><td colspan="6">${escapeHtml(state.varDetails.resourcesError)}</td></tr>`;
    if (summaryEl) summaryEl.textContent = "";
    hidePagination();
    return;
  }
  if (resources.length === 0) {
    tbody.innerHTML = `<tr class="var-details-empty-row"><td colspan="6">No resources returned by the scan.</td></tr>`;
    if (summaryEl) summaryEl.textContent = "0 of 0 items";
    hidePagination();
    return;
  }
  if (filtered.length === 0) {
    let message;
    if (search && filterMode === "shared") {
      message = `No shared resources match "${escapeHtml(search)}"`;
    } else if (search) {
      message = `No resources match "${escapeHtml(search)}"`;
    } else if (filterMode === "shared") {
      message = `This package has no shared resources — switch to <b>All</b> to see the full list.`;
    } else {
      message = `No resources to display.`;
    }
    tbody.innerHTML = `<tr class="var-details-empty-row"><td colspan="6">${message}</td></tr>`;
    if (summaryEl) summaryEl.textContent = `0 of ${resources.length} items`;
    hidePagination();
    return;
  }

  const totalPages = Math.max(1, Math.ceil(filtered.length / VAR_DETAILS_PAGE_SIZE));
  if (state.varDetails.assetPage >= totalPages) state.varDetails.assetPage = totalPages - 1;
  if (state.varDetails.assetPage < 0) state.varDetails.assetPage = 0;
  const page = state.varDetails.assetPage;
  const start = page * VAR_DETAILS_PAGE_SIZE;
  const slice = filtered.slice(start, start + VAR_DETAILS_PAGE_SIZE);

  const activePath = state.varDetails.selectedResourceKey;
  const previewable = state.varDetails.itemIsLocal && Boolean(state.varDetails.item?.file_path);
  const rows = slice.map((r) => {
    const internalPath = r.internal_path ?? "";
    const icon = pickVarDetailsAssetIcon(internalPath);
    const sizeText = formatBytesLocal(Number(r.size ?? 0));
    const sharedWith = Number(r.shared_with ?? 0);
    const truncated = Number(r.shared_with_truncated ?? 0);
    const category = deriveVarDetailsCategory(internalPath);
    const crcHex = r.crc32_hex ? String(r.crc32_hex).toUpperCase() : formatCrc32Hex(r.crc32);
    let chip;
    if (sharedWith === 0) {
      chip = `<span class="var-details-share-chip is-unique">Unique</span>`;
    } else {
      const label = truncated > 0
        ? `Shared · ${sharedWith}+`
        : `Shared · ${sharedWith}`;
      chip = `<span class="var-details-share-chip is-shared">${escapeHtml(label)}</span>`;
    }
    const isPreviewable = previewable && isPreviewablePath(internalPath);
    const isActive = activePath && activePath === internalPath;
    const rowClass = [
      isPreviewable ? "is-previewable" : "",
      isActive ? "is-active" : "",
    ]
      .filter(Boolean)
      .join(" ");
    return `
      <tr class="${rowClass}" data-resource-path="${escapeAttribute(internalPath)}">
        <td class="vd-col-preview">
          <div class="var-details-asset-icon">
            <span class="material-symbols-outlined">${escapeHtml(icon)}</span>
          </div>
        </td>
        <td class="vd-col-name"><span class="var-details-asset-name">${escapeHtml(internalPath)}</span></td>
        <td class="vd-col-cat"><span class="var-details-cat-chip">${escapeHtml(category)}</span></td>
        <td class="vd-col-crc"><code class="var-details-crc">${escapeHtml(crcHex)}</code></td>
        <td class="vd-col-category">${chip}</td>
        <td class="vd-col-size">${escapeHtml(sizeText)}</td>
      </tr>
    `;
  });
  tbody.innerHTML = rows.join("");

  if (summaryEl) {
    summaryEl.textContent = `Showing ${start + 1}–${start + slice.length} of ${filtered.length}${filtered.length !== resources.length ? ` (filtered from ${resources.length})` : ""}`;
  }

  if (pagination) {
    pagination.hidden = totalPages <= 1;
    const indicator = $("var-details-page-indicator");
    if (indicator) indicator.textContent = `${page + 1} / ${totalPages}`;
    const prev = $("var-details-page-prev");
    const next = $("var-details-page-next");
    if (prev) prev.disabled = page <= 0;
    if (next) next.disabled = page >= totalPages - 1;
  }
}

// Backward-compat alias used by older render call-sites — both paths render
// every overlap section so keeping a single function name avoids drift.
function renderVarDetailsAssetTable() {
  renderVarDetailsOverlap();
  renderVarDetailsTopPackages();
  renderVarDetailsResourceTable();
  renderVarDetailsResourcePreview();
}

// Mirrors the Overview/Find Duplicates source-panel preview: when a row in the
// VAR Details Resources table is selected and the package is on local disk,
// fetch (and cache) the preview images via get_vam_preview and render them in
// the inline panel under the table. Hidden when no row is selected, the row
// isn't previewable (.vam/.png/.jpg only), or the VAR is DB-only.
function renderVarDetailsResourcePreview() {
  const slot = $("var-details-resource-preview");
  const overlay = $("var-details-resource-preview-overlay");
  if (!slot) return;

  const item = state.varDetails.item;
  const internalPath = state.varDetails.selectedResourceKey;
  const packageFile = item?.file_path ?? "";

  const shouldShow =
    item
    && internalPath
    && state.varDetails.itemIsLocal
    && packageFile
    && isPreviewablePath(internalPath);

  if (!shouldShow) {
    slot.classList.add("hidden");
    slot.classList.remove("is-open");
    slot.innerHTML = "";
    if (overlay) {
      overlay.classList.add("hidden");
      overlay.classList.remove("is-open");
    }
    return;
  }

  const ref = { package_id: item.package_id, internal_path: internalPath };
  const fileName = internalPath.split(/[\\/]/).pop() || internalPath;
  slot.innerHTML = `
    <header class="var-details-sheet-head">
      <div class="var-details-sheet-title-row">
        <h3 id="var-details-resource-preview-title">Preview</h3>
        <button
          type="button"
          class="icon-button var-details-sheet-close"
          data-vd-preview-close="1"
          aria-label="Close"
          title="Close"
        >
          <span class="material-symbols-outlined">close</span>
        </button>
      </div>
      <p class="var-details-sheet-path" title="${escapeAttribute(internalPath)}">${escapeHtml(fileName)}</p>
    </header>
    <div class="var-details-sheet-body vam-preview">
      ${buildPreviewMarkup(ref, packageFile)}
    </div>
  `;
  const body = slot.querySelector(".var-details-sheet-body");
  if (body) {
    bindPreviewResolution(body);
    bindPreviewActions(body);
  }

  // Two-step reveal so the CSS transition runs: drop .hidden first, then add
  // .is-open on the next frame to slide the panel in from the right.
  if (overlay) overlay.classList.remove("hidden");
  slot.classList.remove("hidden");
  requestAnimationFrame(() => {
    if (overlay) overlay.classList.add("is-open");
    slot.classList.add("is-open");
  });

  const cacheKey = getPreviewCacheKey(item.package_id, internalPath);
  if (!state.previewCache[cacheKey]) {
    void ensureVarDetailsResourcePreviewLoaded(ref, packageFile);
  }
}

async function ensureVarDetailsResourcePreviewLoaded(ref, packageFile) {
  if (!invoke || !packageFile || !isPreviewablePath(ref.internal_path)) return;

  const key = getPreviewCacheKey(ref.package_id, ref.internal_path);
  const current = state.previewCache[key];
  if (current?.status === "loading" || current?.status === "ready") return;

  state.previewCache[key] = { status: "loading", startedAt: Date.now() };
  renderVarDetailsResourcePreview();

  // Tick once a second so the "Loading (Ns)" counter advances while the
  // preview is in-flight. Stops as soon as the cache transitions out of
  // loading or the user navigates to another row.
  const loadingTicker = setInterval(() => {
    const latest = state.previewCache[key];
    if (!latest || latest.status !== "loading") {
      clearInterval(loadingTicker);
      return;
    }
    if (state.varDetails.selectedResourceKey !== ref.internal_path) {
      clearInterval(loadingTicker);
      return;
    }
    renderVarDetailsResourcePreview();
  }, 1000);

  try {
    const data = await Promise.race([
      invoke("get_vam_preview", {
        packageId: ref.package_id,
        packagePath: packageFile,
        vamPath: ref.internal_path,
      }),
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error("Preview request timed out")), 10000)
      ),
    ]);
    state.previewCache[key] = { status: "ready", data, finishedAt: Date.now() };
  } catch (error) {
    state.previewCache[key] = { status: "error", error: String(error), finishedAt: Date.now() };
    addLog(`Preview failed: ${String(error)}`);
  }
  clearInterval(loadingTicker);
  if (state.varDetails.selectedResourceKey === ref.internal_path) {
    renderVarDetailsResourcePreview();
  }
}

// Maps the frontend filter shape to the backend's VarPackageFilters (camelCase
// via serde). Returns `null` when nothing is selected so Tauri sends None.
// The Missing status is a view, not a package filter, so it is never sent.
function serializeVarPackageFilters(filters) {
  if (!filters) return null;
  const payload = {};
  if (filters.status && filters.status !== "missing") payload.status = filters.status;
  if (filters.pkgType) payload.pkgType = filters.pkgType;
  if (filters.enabled) payload.enabled = filters.enabled;
  if (filters.sizeBucket) payload.sizeBucket = filters.sizeBucket;
  if (filters.creator) payload.creator = filters.creator;
  if (filters.scene) payload.sceneImage = filters.scene;
  return Object.keys(payload).length > 0 ? payload : null;
}

// Status is a view selection (like Backstage's), so it neither counts as a
// filter nor gets cleared by Reset.
function activeVarPackageFilterCount(filters) {
  if (!filters) return 0;
  let n = 0;
  if (filters.pkgType) n += 1;
  if (filters.enabled) n += 1;
  if (filters.sizeBucket) n += 1;
  if (filters.creator) n += 1;
  if (filters.scene) n += 1;
  return n;
}

function emptyVarPackageFilters(status = null) {
  return { status, pkgType: null, enabled: null, sizeBucket: null, creator: null, scene: null };
}

/// Loads the Author suggestions: the creators of the scanned folders, read off
/// the backend's folder cache.
async function loadVarPackagesFilterOptions() {
  if (!invoke) return;
  try {
    const opts = await invoke("list_var_package_filter_options", { scope: "folder" });
    state.varPackagesFilterOptions = {
      creators: Array.isArray(opts?.creators) ? opts.creators : [],
    };
  } catch (error) {
    state.varPackagesFilterOptions = { creators: [] };
    addLog(t("varPackagesFilterOptionsFailed", String(error)));
  }
  libRenderAuthorPopup();
}

function setVarPackagesFilter(field, value) {
  if (!state.varPackagesFilters) state.varPackagesFilters = emptyVarPackageFilters();
  const next = value || null;
  if (state.varPackagesFilters[field] === next) return;
  const wasMissing = libIsMissingView();
  state.varPackagesFilters[field] = next;
  renderVarPackagesFilterBar();
  if (field === "status" && next === "missing") {
    renderVarPackages();
    libLoadMissing();
    return;
  }
  if (wasMissing && field === "status") renderVarPackages();
  refreshVarPackagesFromFolder({ forceRescan: false });
}

function clearVarPackagesFilters() {
  if (activeVarPackageFilterCount(state.varPackagesFilters) === 0) return;
  state.varPackagesFilters = emptyVarPackageFilters(state.varPackagesFilters?.status ?? null);
  renderVarPackagesFilterBar();
  refreshVarPackagesFromFolder({ forceRescan: false });
}

function libCountText(n) {
  if (state.varPackagesLoading && !state.varPackagesFacets) return "…";
  if (n == null) return "";
  return Number(n).toLocaleString();
}

function libListRowHtml({ value, label, title = "", count, selected, indent = false, dot = "" }) {
  return `<button type="button" class="lib-row${selected ? " is-selected" : ""}${indent ? " is-indented" : ""}"
      data-lib-value="${escapeAttribute(value ?? "")}" ${title ? `title="${escapeAttribute(title)}"` : ""}>
      ${dot ? `<span class="lib-type-dot" style="--dot:${dot}"></span>` : ""}
      <span class="lib-row-label">${escapeHtml(label)}</span>
      <span class="lib-row-count">${escapeHtml(libCountText(count))}</span>
    </button>`;
}

const LIB_SIZE_OPTIONS = [
  { value: null, label: "Any size" },
  { value: "sm", label: "< 100 MB" },
  { value: "md", label: "100 MB – 1 GB" },
  { value: "lg", label: "> 1 GB" },
];
const LIB_SCENE_OPTIONS = [
  { value: null, label: "Any" },
  { value: "with", label: "Has a scene image" },
  { value: "without", label: "No scene image" },
];
const LIB_SORT_OPTIONS = [
  { value: "type", label: "Type" },
  { value: "name", label: "Name" },
  { value: "size", label: "Size" },
  { value: "items", label: "Content" },
  { value: "deps", label: "Deps" },
  { value: "modified", label: "Date modified" },
];

// One dropdown open at a time; the trigger shows the current value and turns
// accent-coloured while its filter differs from the default.
function libSetDropdown(name, label, active) {
  vpSetText(`lib-dd-${name}-label`, label);
  document.querySelector(`[data-lib-dd-trigger='${name}']`)?.classList.toggle("is-active", Boolean(active));
}

function libCloseDropdowns(except = null) {
  document.querySelectorAll("[data-lib-dd-menu]").forEach((menu) => {
    if (menu.getAttribute("data-lib-dd-menu") === except) return;
    menu.classList.add("hidden");
  });
  document.querySelectorAll("[data-lib-dd-trigger].is-open").forEach((trigger) => {
    if (trigger.getAttribute("data-lib-dd-trigger") !== except) trigger.classList.remove("is-open");
  });
}

function libToggleDropdown(name) {
  const menu = document.querySelector(`[data-lib-dd-menu='${name}']`);
  const trigger = document.querySelector(`[data-lib-dd-trigger='${name}']`);
  if (!menu || !trigger) return;
  const open = menu.classList.contains("hidden");
  libCloseDropdowns(name);
  menu.classList.toggle("hidden", !open);
  trigger.classList.toggle("is-open", open);
  if (open && name === "author") {
    const input = $("lib-author-input");
    if (input) {
      input.value = "";
      LIB_AC.active = -1;
      libRenderAuthorPopup();
      input.focus();
    }
  } else if (open) {
    // Menus with a search box (the Hub's Tags / Author) focus it on open.
    menu.querySelector("input[type='text']")?.focus();
  }
}

// The filter bar: one dropdown per filter (Status / Type / Enabled lists carry
// facet counts), the Author autocomplete, and Sort. Rebuilt after every listing.
function renderVarPackagesFilterBar() {
  const f = state.varPackagesFilters ?? emptyVarPackageFilters();
  const facets = state.varPackagesFacets;

  const statusList = $("lib-status-list");
  if (statusList) {
    statusList.innerHTML = LIB_STATUSES.map((s) =>
      libListRowHtml({
        value: s.key,
        label: s.label,
        title: s.title,
        indent: s.indent,
        selected: (f.status ?? null) === s.key,
        count: facets ? facets.statuses?.[s.count ?? s.key] : null,
      }),
    ).join("");
  }
  const status = LIB_STATUSES.find((s) => s.key === (f.status ?? null));
  libSetDropdown("status", f.status ? status?.label ?? f.status : "Status: All", Boolean(f.status));

  const typeList = $("lib-type-list");
  if (typeList) {
    const all = facets
      ? Object.values(facets.types ?? {}).reduce((sum, n) => sum + Number(n || 0), 0)
      : null;
    typeList.innerHTML =
      libListRowHtml({ value: null, label: "All types", selected: !f.pkgType, count: all }) +
      LIB_TYPES.map((type) =>
        libListRowHtml({
          value: type.key,
          label: type.label,
          dot: type.color,
          selected: f.pkgType === type.key,
          count: facets ? facets.types?.[type.key] ?? 0 : null,
        }),
      ).join("");
  }
  const type = f.pkgType ? LIB_TYPE_BY_KEY[f.pkgType] : null;
  libSetDropdown("type", type ? type.label : "Type", Boolean(type));
  const typeDot = $("lib-dd-type-dot");
  if (typeDot) {
    typeDot.classList.toggle("hidden", !type);
    if (type) typeDot.style.setProperty("--dot", type.color);
  }

  const enabledList = $("lib-enabled-list");
  if (enabledList) {
    const en = facets ? Number(facets.enabled || 0) : null;
    const dis = facets ? Number(facets.disabled || 0) : null;
    enabledList.innerHTML = [
      { value: null, label: "All", count: facets ? en + dis : null },
      { value: "enabled", label: "Enabled", count: en },
      { value: "disabled", label: "Disabled", count: dis, title: "Have a .disabled marker — VaM skips them" },
    ]
      .map((row) => libListRowHtml({ ...row, selected: (f.enabled ?? null) === row.value }))
      .join("");
  }
  libSetDropdown(
    "enabled",
    f.enabled === "enabled" ? "Enabled only" : f.enabled === "disabled" ? "Disabled only" : "Enabled",
    Boolean(f.enabled),
  );

  const chips = $("lib-author-chips");
  if (chips) {
    chips.classList.toggle("hidden", !f.creator);
    chips.innerHTML = f.creator
      ? `<button type="button" class="lib-filter-chip" data-lib-clear-author title="Clear author filter">
           ${escapeHtml(f.creator)}<span class="material-symbols-outlined">close</span></button>`
      : "";
  }
  libSetDropdown("author", f.creator ? `by ${f.creator}` : "Author", Boolean(f.creator));

  const pickList = (id, options, current) => {
    const el = $(id);
    if (el) {
      el.innerHTML = options
        .map((o) => libListRowHtml({ value: o.value, label: o.label, selected: (current ?? null) === o.value }))
        .join("");
    }
  };
  pickList("lib-size-list", LIB_SIZE_OPTIONS, f.sizeBucket);
  libSetDropdown(
    "size",
    f.sizeBucket ? LIB_SIZE_OPTIONS.find((o) => o.value === f.sizeBucket)?.label ?? "Size" : "Size",
    Boolean(f.sizeBucket),
  );
  pickList("lib-scene-list", LIB_SCENE_OPTIONS, f.scene);
  libSetDropdown(
    "scene",
    f.scene ? LIB_SCENE_OPTIONS.find((o) => o.value === f.scene)?.label ?? "Scene image" : "Scene image",
    Boolean(f.scene),
  );

  const sort = state.varPackagesSort ?? { key: "type", dir: "asc" };
  pickList("lib-sort-list", LIB_SORT_OPTIONS, sort.key);
  libSetDropdown("sort", `Sort: ${LIB_SORT_OPTIONS.find((o) => o.value === sort.key)?.label ?? sort.key}`, false);
  const dirIcon = document.querySelector("#lib-sort-dir .material-symbols-outlined");
  if (dirIcon) dirIcon.textContent = sort.dir === "asc" ? "arrow_upward" : "arrow_downward";

  const hasSearch = Boolean(String(state.varPackagesFilter ?? "").trim());
  $("lib-search-clear")?.classList.toggle("hidden", !hasSearch);
  $("lib-search")?.classList.toggle("has-value", hasSearch);
  $("lib-search-legend")?.classList.toggle("is-available", !hasSearch);
  libRenderToolbar();
}

// Direction a key gets when first picked: names and types read naturally
// ascending, while sizes, counts and dates are asked about "biggest"/"newest".
function varPackagesSortDefaultDir(key) {
  return key === "name" || key === "type" ? "asc" : "desc";
}

function setVarPackagesSort(key, { flip = false } = {}) {
  const next = String(key ?? "type");
  const current = state.varPackagesSort ?? { key: "type", dir: "asc" };
  if (flip || current.key === next) {
    state.varPackagesSort = { key: next, dir: current.dir === "asc" ? "desc" : "asc" };
  } else {
    state.varPackagesSort = { key: next, dir: varPackagesSortDefaultDir(next) };
  }
  renderVarPackagesFilterBar();
  // Not a rescan: sort is not part of the Rust folder-cache key, so this is
  // served from the cached scan with no disk walk.
  refreshVarPackagesFromFolder({ forceRescan: false });
}

// The wire format for list_var_packages. Always sent — the backend defaults to
// name/asc for anything unrecognised, so this is also the safe fallback.
function varPackagesSortArgs() {
  const s = state.varPackagesSort ?? {};
  return { sort: s.key || "type", sortDir: s.dir === "desc" ? "desc" : "asc" };
}

// ---- Author autocomplete ----------------------------------------------------

const LIB_AC = { active: -1, matches: [] };

// The list inside the Author dropdown: every scanned creator, narrowed by
// what's typed (prefix matches first).
function libRenderAuthorPopup() {
  const input = $("lib-author-input");
  const popup = $("lib-author-popup");
  if (!input || !popup) return;
  const query = input.value.trim().toLowerCase();
  const creators = state.varPackagesFilterOptions?.creators ?? [];
  const starts = [];
  const contains = [];
  for (const name of creators) {
    const lower = name.toLowerCase();
    if (!query || lower.startsWith(query)) starts.push(name);
    else if (lower.includes(query)) contains.push(name);
  }
  LIB_AC.matches = [...starts, ...contains].slice(0, 200);
  if (LIB_AC.active >= LIB_AC.matches.length) LIB_AC.active = LIB_AC.matches.length - 1;
  const current = state.varPackagesFilters?.creator ?? null;
  popup.innerHTML = LIB_AC.matches.length
    ? LIB_AC.matches
        .map(
          (name, i) =>
            `<button type="button" class="lib-ac-option${i === LIB_AC.active ? " is-active" : ""}${
              name === current ? " is-current" : ""
            }" data-lib-author-pick="${escapeAttribute(name)}">${escapeHtml(name)}</button>`,
        )
        .join("")
    : `<div class="lib-ac-empty">${creators.length ? "No matching authors" : "Scan a folder to list its authors"}</div>`;
  popup.querySelector(".lib-ac-option.is-active")?.scrollIntoView({ block: "nearest" });
}

function libPickAuthor(name) {
  const input = $("lib-author-input");
  if (input) input.value = "";
  LIB_AC.active = -1;
  libCloseDropdowns();
  setVarPackagesFilter("creator", name || null);
}

// Extra VAR folders live in a dropdown on the top bar; its trigger shows how
// many there are.
function libRenderFoldersBadge() {
  const n = getAdditionalDirs("varPackages").length;
  vpSetText("lib-folders-label", n ? `+${n} folder${n === 1 ? "" : "s"}` : "Folders");
  document.querySelector("[data-lib-dd-trigger='folders']")?.classList.toggle("is-active", n > 0);
}

// --- Scan depth (VAR Folders) -------------------------------------------------
// Deep walks every subfolder of the chosen folders; Normal lists only the .var
// files sitting directly in them. Deep is the default.

function applyVarPackagesDepthUi() {
  const deep = state.varPackagesDeepScan !== false;
  const deepBtn = $("var-packages-depth-deep");
  const normalBtn = $("var-packages-depth-normal");
  if (deepBtn) {
    deepBtn.classList.toggle("active", deep);
    deepBtn.setAttribute("aria-selected", String(deep));
  }
  if (normalBtn) {
    normalBtn.classList.toggle("active", !deep);
    normalBtn.setAttribute("aria-selected", String(!deep));
  }

  // The switch only takes effect on the next Scan, so say so while the listing
  // on screen was built with the other depth.
  const pending =
    state.varPackagesDeepScan !== state.varPackagesScannedDeep &&
    (state.varPackagesItems?.length ?? 0) > 0;
  const hint = $("var-packages-depth-hint");
  if (hint) hint.classList.toggle("vp-depth-pending", pending);
  vpSetText(
    "var-packages-depth-hint",
    pending
      ? "Click Scan to apply."
      : deep
        ? "Includes .var files inside subfolders."
        : "Only .var files directly in the folder.",
  );
}

/// Moves the switch. Deliberately does NOT rescan — scanning happens only when
/// the user clicks Scan.
function setVarPackagesDeepScan(deep) {
  if (state.varPackagesDeepScan === deep) return;
  state.varPackagesDeepScan = deep;
  applyVarPackagesDepthUi();
  persistAllConfig().catch((e) => addLog(`Settings: ${String(e)}`));
}

window.__refreshVarPackagesView = () => {
  refreshVarPackagesView();
};

// ============================================================
// Database page — tabs, and the Packages tab
//
// The Packages tab lists every package row in the local database. It used to
// be the VAR Packages page's "Database" mode; it lives here now so VAR Packages
// is purely the folder library. Paginated server-side by
// `list_var_packages_from_db`; status = whether the indexed file is still on
// disk (`indexed` means "on disk" for these rows).
// ============================================================

const DB_PKGS_PAGE_SIZE = 50;
const dbPkgs = {
  tab: "build",
  items: [],
  total: 0,
  page: 0,
  loading: false,
  requery: false,
  loadedOnce: false,
  search: "",
  filters: { status: null, sizeBucket: null, creator: null, favorite: false },
  sort: { key: "name", dir: "asc" },
};

function setDatabaseTab(tab) {
  dbPkgs.tab = tab === "packages" ? "packages" : "build";
  document.querySelectorAll("[data-db-tab]").forEach((btn) => {
    const on = btn.getAttribute("data-db-tab") === dbPkgs.tab;
    btn.classList.toggle("active", on);
    btn.setAttribute("aria-selected", String(on));
  });
  $("db-tab-panel-build")?.classList.toggle("hidden", dbPkgs.tab !== "build");
  $("db-tab-panel-packages")?.classList.toggle("hidden", dbPkgs.tab !== "packages");
  if (dbPkgs.tab === "packages") {
    if (!dbPkgs.loadedOnce) dbPkgsLoadCreators();
    dbPkgsRefresh();
  }
}

function dbPkgsVisible() {
  const view = $("build-db-view");
  return Boolean(view && !view.classList.contains("hidden") && dbPkgs.tab === "packages");
}

// Called after library mutations and on entering the Database page.
function dbPkgsRefreshIfVisible() {
  if (dbPkgsVisible()) dbPkgsRefresh();
}

window.__refreshDatabasePackages = dbPkgsRefreshIfVisible;

function dbPkgsSerializeFilters() {
  const f = dbPkgs.filters;
  const payload = {};
  if (f.status) payload.status = f.status;
  if (f.sizeBucket) payload.sizeBucket = f.sizeBucket;
  if (f.creator) payload.creator = f.creator;
  if (f.favorite) payload.favorite = true;
  return Object.keys(payload).length ? payload : null;
}

async function dbPkgsRefresh() {
  if (!invoke) return;
  if (dbPkgs.loading) {
    dbPkgs.requery = true;
    return;
  }
  dbPkgs.loading = true;
  dbPkgsRender();
  const btn = $("db-pkgs-refresh");
  if (btn) btn.disabled = true;
  try {
    const page = await invoke("list_var_packages_from_db", {
      offset: dbPkgs.page * DB_PKGS_PAGE_SIZE,
      limit: DB_PKGS_PAGE_SIZE,
      search: dbPkgs.search.trim() || null,
      filters: dbPkgsSerializeFilters(),
      sort: dbPkgs.sort.key,
      sortDir: dbPkgs.sort.dir,
    });
    dbPkgs.items = Array.isArray(page?.items) ? page.items : [];
    dbPkgs.total = Math.max(0, Number(page?.total ?? 0));
    const pages = Math.max(1, Math.ceil(dbPkgs.total / DB_PKGS_PAGE_SIZE));
    if (dbPkgs.page >= pages) {
      dbPkgs.page = pages - 1;
      dbPkgs.requery = dbPkgs.total > 0;
    }
    dbPkgs.loadedOnce = true;
  } catch (error) {
    dbPkgs.items = [];
    dbPkgs.total = 0;
    addLog(t("varPackagesDbFailed", String(error)));
  } finally {
    dbPkgs.loading = false;
    if (btn) btn.disabled = false;
    dbPkgsRender();
    if (dbPkgs.requery) {
      dbPkgs.requery = false;
      dbPkgsRefresh();
    }
  }
}

async function dbPkgsLoadCreators() {
  if (!invoke) return;
  try {
    const opts = await invoke("list_var_package_filter_options", { scope: null });
    const list = $("db-pkgs-creators");
    if (list) {
      list.innerHTML = (opts?.creators ?? [])
        .map((name) => `<option value="${escapeAttribute(name)}"></option>`)
        .join("");
    }
  } catch (error) {
    addLog(t("varPackagesFilterOptionsFailed", String(error)));
  }
}

function dbPkgsRender() {
  const tbody = $("db-pkgs-tbody");
  if (!tbody) return;
  $("db-pkgs-progress")?.classList.toggle("hidden", !dbPkgs.loading);
  vpSetText("db-pkgs-count", t("varPackagesCount", dbPkgs.total));
  const dirIcon = document.querySelector("#db-pkgs-sort-dir .material-symbols-outlined");
  if (dirIcon) dirIcon.textContent = dbPkgs.sort.dir === "asc" ? "arrow_upward" : "arrow_downward";

  const message = (text) => {
    tbody.innerHTML = `<tr class="var-packages-empty-row"><td colspan="6">${escapeHtml(text)}</td></tr>`;
  };
  if (dbPkgs.loading && !dbPkgs.items.length) {
    message(t("varPackagesLoadingDb"));
    dbPkgsRenderPagination();
    return;
  }
  if (!dbPkgs.items.length) {
    const filtered = dbPkgs.search.trim() || dbPkgsSerializeFilters();
    message(filtered ? t("varPackagesNoResults") : "The database has no packages yet — build it from the Build tab.");
    dbPkgsRenderPagination();
    return;
  }
  tbody.innerHTML = dbPkgs.items
    .map((it) => {
      const onDisk = Boolean(it.indexed);
      const fav = _favoritePackages.has(it.package_id);
      const fp = it.file_path ?? "";
      return `
        <tr data-package-id="${escapeAttribute(it.package_id ?? "")}" data-file-path="${escapeAttribute(fp)}"
            title="${escapeAttribute(fp)}">
          <td class="vp-col-status">
            <span class="vp-status ${onDisk ? "is-indexed" : "is-unindexed"}">
              <span class="vp-status-dot"></span>
              <span>${escapeHtml(t(onDisk ? "varPackagesStatusOnDisk" : "varPackagesStatusMissing"))}</span>
            </span>
          </td>
          <td class="vp-col-name vp-cell-name">${escapeHtml(it.file_name ?? "")}</td>
          <td class="vp-col-creator">${escapeHtml(it.creator || "—")}</td>
          <td class="vp-col-size vp-cell-size">${escapeHtml(formatBytesLocal(it.size_bytes))}</td>
          <td class="vp-col-modified vp-cell-modified">${escapeHtml(formatVarModifiedMs(it.modified_ms))}</td>
          <td class="vp-col-actions">
            <button class="icon-button vp-row-fav${fav ? " is-active" : ""}" type="button"
                    data-db-fav="${escapeAttribute(it.package_id ?? "")}" aria-pressed="${fav}"
                    title="${fav ? "Remove from favorites" : "Add to favorites"}">
              <span class="material-symbols-outlined">star</span>
            </button>
            <button class="icon-button vp-row-images" type="button" data-db-images="${escapeAttribute(fp)}"
                    ${onDisk ? "" : "disabled"} title="${onDisk ? "View images" : "The file is already gone from disk"}">
              <span class="material-symbols-outlined">photo_library</span>
            </button>
            <button class="icon-button vp-row-delete" type="button" data-db-delete="${escapeAttribute(fp)}"
                    ${onDisk ? "" : "disabled"} title="${onDisk ? "Send to Recycle Bin" : "The file is already gone from disk"}">
              <span class="material-symbols-outlined">delete</span>
            </button>
          </td>
        </tr>`;
    })
    .join("");
  dbPkgsRenderPagination();
}

function dbPkgsRenderPagination() {
  const pagination = $("db-pkgs-pagination");
  const summary = $("db-pkgs-pagination-summary");
  const controls = $("db-pkgs-pagination-controls");
  if (!pagination || !summary || !controls) return;
  if (dbPkgs.total === 0) {
    pagination.classList.add("hidden");
    return;
  }
  pagination.classList.remove("hidden");
  const pages = Math.max(1, Math.ceil(dbPkgs.total / DB_PKGS_PAGE_SIZE));
  const current = Math.min(dbPkgs.page, pages - 1);
  const start = current * DB_PKGS_PAGE_SIZE + 1;
  const end = Math.min(dbPkgs.total, (current + 1) * DB_PKGS_PAGE_SIZE);
  summary.textContent = t("varPackagesPageSummary", start, end, dbPkgs.total);
  if (pages <= 1) {
    controls.innerHTML = "";
    return;
  }
  controls.innerHTML = `
    <button class="page-nav-button" data-db-page="${current - 1}" type="button" ${current === 0 ? "disabled" : ""}>
      <span class="material-symbols-outlined">chevron_left</span>${escapeHtml(t("varPackagesPagePrev"))}
    </button>
    <div class="page-numbers">${buildPageNumbers(pages, current)
      .map((p) =>
        p === "ellipsis"
          ? `<span class="page-ellipsis">…</span>`
          : `<button class="page-button ${p === current ? "active" : ""}" data-db-page="${p}" type="button">${p + 1}</button>`,
      )
      .join("")}</div>
    <button class="page-nav-button" data-db-page="${current + 1}" type="button" ${current >= pages - 1 ? "disabled" : ""}>
      ${escapeHtml(t("varPackagesPageNext"))}<span class="material-symbols-outlined">chevron_right</span>
    </button>`;
}

function dbPkgsItem(row) {
  const fp = row?.getAttribute("data-file-path");
  return dbPkgs.items.find((it) => it.file_path === fp) ?? null;
}

function setupDatabasePackages() {
  document.querySelectorAll("[data-db-tab]").forEach((btn) => {
    btn.addEventListener("click", () => setDatabaseTab(btn.getAttribute("data-db-tab")));
  });

  const requery = () => {
    dbPkgs.page = 0;
    dbPkgsRefresh();
  };
  const search = $("db-pkgs-search");
  let timer = null;
  search?.addEventListener("input", () => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      if (dbPkgs.search === search.value) return;
      dbPkgs.search = search.value;
      requery();
    }, 180);
  });
  $("db-pkgs-status")?.addEventListener("change", (e) => {
    dbPkgs.filters.status = e.target.value || null;
    requery();
  });
  $("db-pkgs-size")?.addEventListener("change", (e) => {
    dbPkgs.filters.sizeBucket = e.target.value || null;
    requery();
  });
  $("db-pkgs-creator")?.addEventListener("change", (e) => {
    dbPkgs.filters.creator = e.target.value.trim() || null;
    requery();
  });
  $("db-pkgs-fav")?.addEventListener("change", (e) => {
    dbPkgs.filters.favorite = e.target.checked;
    requery();
  });
  $("db-pkgs-sort")?.addEventListener("change", (e) => {
    const key = e.target.value || "name";
    dbPkgs.sort = { key, dir: key === "name" ? "asc" : "desc" };
    requery();
  });
  $("db-pkgs-sort-dir")?.addEventListener("click", () => {
    dbPkgs.sort = { ...dbPkgs.sort, dir: dbPkgs.sort.dir === "asc" ? "desc" : "asc" };
    requery();
  });
  $("db-pkgs-refresh")?.addEventListener("click", () => {
    dbPkgsLoadCreators();
    dbPkgsRefresh();
  });
  $("db-pkgs-pagination-controls")?.addEventListener("click", (event) => {
    const btn = event.target.closest("[data-db-page]");
    if (!btn || btn.disabled) return;
    dbPkgs.page = Math.max(0, Number(btn.getAttribute("data-db-page")) || 0);
    dbPkgsRefresh();
  });

  const tbody = $("db-pkgs-tbody");
  tbody?.addEventListener("click", (event) => {
    const fav = event.target.closest("[data-db-fav]");
    if (fav) {
      togglePackageFavorite(fav.getAttribute("data-db-fav"));
      return;
    }
    const images = event.target.closest("[data-db-images]");
    if (images) {
      vpImagesOpen(images.getAttribute("data-db-images"));
      return;
    }
    const del = event.target.closest("[data-db-delete]");
    if (del) {
      vpDeleteOne(del.getAttribute("data-db-delete"), del).catch((e) => addLog(`Database: ${String(e)}`));
      return;
    }
    const item = dbPkgsItem(event.target.closest("tr[data-file-path]"));
    if (item) openVarDetailsView(item, "db");
  });
  tbody?.addEventListener("contextmenu", (event) => {
    const item = dbPkgsItem(event.target.closest("tr[data-file-path]"));
    if (!item) return;
    event.preventDefault();
    const onDisk = Boolean(item.indexed);
    showContextMenu(event.clientX, event.clientY, [
      { label: "Open Details", action: () => openVarDetailsView(item, "db") },
      ...(onDisk
        ? [
            {
              label: "Show in Explorer",
              action: () =>
                invoke("show_in_explorer", { path: item.file_path }).catch((e) => addLog(`Database: ${String(e)}`)),
            },
            { label: t("varPackagesImagesOpen"), action: () => vpImagesOpen(item.file_path) },
          ]
        : []),
      {
        label: _favoritePackages.has(item.package_id) ? "Remove from favorites" : "Add to favorites",
        action: () => togglePackageFavorite(item.package_id),
      },
      {
        label: t("varPackagesRowCopyPath"),
        action: () => navigator.clipboard?.writeText(item.file_path ?? "").catch(() => {}),
      },
      ...(onDisk
        ? [
            { separator: true },
            {
              label: "Delete (Recycle Bin)",
              danger: true,
              action: () => vpDeleteOne(item.file_path, null).catch((e) => addLog(`Database: ${String(e)}`)),
            },
          ]
        : []),
    ]);
  });
}

function renderModeControls() {
  if (state.currentPage === "db-find") return dbfRenderModeControls();
  // Single-VAR is the only mode; just keep the target-var inputs gated on
  // whether a task is running. Kept as a function (vs. inlined at the few
  // call sites) so dbf's branch + the activeTask gating stay one place.
  $("target-var-path").disabled = Boolean(state.activeTask);
  $("pick-target-var-button").disabled = Boolean(state.activeTask);
}

function renderSummary() {
  if (state.currentPage === "db-find") return dbfRenderSummary();
  // Single-VAR mode is the only mode: always show per-target size cards,
  // hide the all-folders "groups" card.
  $("stat-groups-card").classList.add("hidden");
  $("stat-current-size-card").classList.remove("hidden");
  $("stat-estimated-size-card").classList.remove("hidden");

  if (!state.scan) {
    $("stat-packages").textContent = "0";
    $("stat-groups").textContent = "0";
    $("stat-space").textContent = "0 B";
    $("stat-filtered").textContent = "0";
    $("stat-modified").textContent = "0";
    $("stat-current-size").textContent = "0 B";
    $("stat-estimated-size").textContent = "0 B";
    $("stat-reclaimed-size").textContent = "0 B";
    setButtonsBusy(Boolean(state.activeTask));
    return;
  }

  const scopedGroups = getScopedGroups();
  const filteredGroups = getFilteredGroups();
  const modifiedGroups = scopedGroups.filter((group) => {
    const currentKeep = state.keepMap[group.key] ?? getRefValue(group.refs[0]);
    const defaultKeep = state.defaultKeepMap[group.key] ?? getRefValue(group.refs[0]);
    return currentKeep !== defaultKeep;
  });
  const theoreticalReclaimableBytes = scopedGroups.reduce(
    (total, group) => total + getGroupScopedMaxReclaimableBytes(group),
    0
  );
  const currentReclaimableBytes = scopedGroups.reduce(
    (total, group) => total + getGroupCurrentReclaimableBytes(group),
    0
  );
  const targetPackageId = getActiveTargetPackageId();
  const currentPackageBytes = targetPackageId
    ? Number(state.scan.package_sizes?.[targetPackageId] ?? 0)
    : 0;
  const estimatedRemainingBytes = Math.max(0, currentPackageBytes - currentReclaimableBytes);

  $("stat-packages").textContent = String(state.scan.summary.packages);
  $("stat-groups").textContent = String(state.scan.summary.duplicate_groups);
  $("stat-space").textContent = formatBytesLocal(theoreticalReclaimableBytes);
  $("stat-filtered").textContent = String(filteredGroups.length);
  $("stat-modified").textContent = String(modifiedGroups.length);
  $("stat-current-size").textContent = formatBytesLocal(currentPackageBytes);
  $("stat-estimated-size").textContent = formatBytesLocal(estimatedRemainingBytes);

  // Total bytes that would be reclaimed by the user's modifications — sum
  // of the current reclaim for every group whose keep choice differs from
  // the default. Visible on both Overview and Find Duplicates.
  const modifiedReclaimedBytes = modifiedGroups.reduce(
    (sum, g) => sum + getGroupCurrentReclaimableBytes(g),
    0
  );
  $("stat-reclaimed-size").textContent = formatBytesLocal(modifiedReclaimedBytes);

  setButtonsBusy(Boolean(state.activeTask));
}

// Sort-only cache: shares all inputs with getFilteredGroups *except* the
// keyword. Keystrokes in the filter box invalidate the filtered cache but
// reuse this sorted-scoped array, so the Array.prototype.sort doesn't fire
// per keystroke on large group sets.
let _sortedScopedCache = null;
let _sortedScopedCacheKey = null;
function _getSortedScopedGroups() {
  const cacheKey = [
    state.scan,
    state.targetPackageId,
    state.targetVarPath,
    state.currentPage,
    _keepMapVersion,
  ];
  if (_sortedScopedCacheKey && _sortedScopedCache) {
    let same = true;
    for (let i = 0; i < cacheKey.length; i++) {
      if (cacheKey[i] !== _sortedScopedCacheKey[i]) {
        same = false;
        break;
      }
    }
    if (same) return _sortedScopedCache;
  }
  const groups = [...getScopedGroups()];
  groups.sort((a, b) => {
    const delta = getGroupScopedMaxReclaimableBytes(b) - getGroupScopedMaxReclaimableBytes(a);
    if (delta !== 0) {
      return delta;
    }
    return a.refs[0].internal_path.localeCompare(b.refs[0].internal_path);
  });
  _sortedScopedCache = groups;
  _sortedScopedCacheKey = cacheKey;
  return groups;
}

function getFilteredGroups() {
  const filterEl = $("group-filter");
  const keyword = (filterEl ? filterEl.value : "").trim().toLowerCase();
  // Cache key tuple — referential identity for state.scan plus primitives for
  // the rest. _keepMapVersion guards the sort, which depends on keepMap via
  // getGroupScopedMaxReclaimableBytes.
  const cacheKey = [
    state.scan,
    state.targetPackageId,
    state.targetVarPath,
    state.currentPage,
    keyword,
    _keepMapVersion,
  ];
  if (_filteredGroupsCacheKey && _filteredGroupsCache) {
    let same = true;
    for (let i = 0; i < cacheKey.length; i++) {
      if (cacheKey[i] !== _filteredGroupsCacheKey[i]) {
        same = false;
        break;
      }
    }
    if (same) return _filteredGroupsCache;
  }
  const sorted = _getSortedScopedGroups();
  let result;
  if (!keyword) {
    // Defensive copy so callers that mutate the array (none today, but cheap
    // insurance) don't corrupt the sort cache.
    result = sorted.slice();
  } else {
    result = sorted.filter((group) => {
      // Use precomputed lowercased haystack when available (set during scan
      // ingestion); fall back to per-call concat for any code path that still
      // hands us a group without one.
      const haystack = group.__haystack ?? (
        group.key + " " + group.package_ids.join(" ") + " " +
        group.refs.map((item) => item.internal_path).join(" ")
      ).toLowerCase();
      return haystack.includes(keyword);
    });
  }
  _filteredGroupsCache = result;
  _filteredGroupsCacheKey = cacheKey;
  return result;
}

function buildPageNumbers(totalPages, currentPage) {
  if (totalPages <= 7) {
    return Array.from({ length: totalPages }, (_, i) => i);
  }
  const pages = new Set([0, totalPages - 1, currentPage]);
  if (currentPage - 1 >= 0) pages.add(currentPage - 1);
  if (currentPage + 1 < totalPages) pages.add(currentPage + 1);
  const sorted = [...pages].sort((a, b) => a - b);
  const result = [];
  let prev = -1;
  for (const page of sorted) {
    if (prev !== -1 && page > prev + 1) {
      result.push("ellipsis");
    }
    result.push(page);
    prev = page;
  }
  return result;
}

// One-time delegated click handler for the Overview pagination strip. The
// strip is rebuilt by innerHTML on every renderGroupPagination call, so
// per-button listeners would have to be rebound each render. Reading data
// attributes off the event target keeps the cost flat.
let _groupPaginationDelegated = false;
function _ensureGroupPaginationDelegated() {
  if (_groupPaginationDelegated) return;
  const pagination = $("group-pagination");
  if (!pagination) return;
  pagination.addEventListener("click", (event) => {
    const button = event.target.closest("button");
    if (!button || !pagination.contains(button)) return;
    if (button.disabled) return;
    const totalPages = Math.max(1, Math.ceil(_lastRenderedGroupCount / GROUP_PAGE_SIZE));
    const action = button.dataset.pageAction;
    if (action === "prev") {
      state.groupPage = Math.max(0, state.groupPage - 1);
    } else if (action === "next") {
      state.groupPage = Math.min(totalPages - 1, state.groupPage + 1);
    } else if (button.dataset.pageJump != null) {
      state.groupPage = Number(button.dataset.pageJump) || 0;
    } else {
      return;
    }
    renderGroups();
  });
  _groupPaginationDelegated = true;
}
// Tracks the most recent total group count so the pagination delegate can
// derive `totalPages` for the prev/next clamp without re-running the filter.
let _lastRenderedGroupCount = 0;

function renderGroupPagination(totalGroups) {
  _lastRenderedGroupCount = totalGroups;
  const pagination = $("group-pagination");
  if (!pagination) return;
  _ensureGroupPaginationDelegated();
  if (totalGroups <= GROUP_PAGE_SIZE) {
    pagination.classList.add("hidden");
    pagination.innerHTML = "";
    return;
  }

  const totalPages = Math.max(1, Math.ceil(totalGroups / GROUP_PAGE_SIZE));
  if (state.groupPage >= totalPages) {
    state.groupPage = totalPages - 1;
  }
  const currentPage = state.groupPage;
  const pages = buildPageNumbers(totalPages, currentPage);

  pagination.classList.remove("hidden");
  pagination.innerHTML = `
    <button class="page-nav-button" data-page-action="prev" type="button" ${currentPage === 0 ? "disabled" : ""}>
      <span class="material-symbols-outlined">chevron_left</span>
      Previous
    </button>
    <div class="page-numbers">
      ${pages
        .map((page) =>
          page === "ellipsis"
            ? `<span class="page-ellipsis">…</span>`
            : `<button class="page-button ${page === currentPage ? "active" : ""}" data-page-jump="${page}" type="button">${page + 1}</button>`
        )
        .join("")}
    </div>
    <button class="page-nav-button" data-page-action="next" type="button" ${currentPage >= totalPages - 1 ? "disabled" : ""}>
      Next
      <span class="material-symbols-outlined">chevron_right</span>
    </button>
  `;
}

// One-time delegated click + contextmenu on the group list. The inner HTML
// is rebuilt on every render, so per-row addEventListener calls would
// dominate paint time on large pages. Delegation reads data-key off the
// row that received the event.
let _groupListDelegated = false;
function _ensureGroupListDelegated() {
  if (_groupListDelegated) return;
  const container = $("group-list");
  if (!container) return;
  container.addEventListener("click", (event) => {
    const row = event.target.closest(".group-row");
    if (!row || !container.contains(row)) return;
    hideContextMenu();
    setSelectionFromClick(row.dataset.key, event);
    renderGroups();
    renderDetail();
  });
  container.addEventListener("contextmenu", (event) => {
    const row = event.target.closest(".group-row");
    if (!row || !container.contains(row)) return;
    event.preventDefault();
    hideContextMenu();
    ensureRightClickSelection(row.dataset.key);
    renderGroups();
    showContextMenu(event.clientX, event.clientY, groupMenuItems());
  });
  _groupListDelegated = true;
}

function renderGroups() {
  if (state.currentPage === "db-find") return dbfRenderGroups();
  renderSummary();
  const container = $("group-list");
  _ensureGroupListDelegated();
  const groups = getFilteredGroups();
  const totalPages = Math.max(1, Math.ceil(groups.length / GROUP_PAGE_SIZE));
  if (state.groupPage >= totalPages) {
    state.groupPage = totalPages - 1;
  }
  if (state.groupPage < 0) {
    state.groupPage = 0;
  }
  const start = state.groupPage * GROUP_PAGE_SIZE;
  const visibleGroups = groups.slice(start, start + GROUP_PAGE_SIZE);
  normalizeSelections();
  const selected = selectedKeySet();

  if (!groups.length) {
    const targetPackageId = getActiveTargetPackageId();
    container.classList.add("empty");
    container.innerHTML = `<div class="detail-empty">${
      state.scan
        ? (targetPackageId ? t("scopeEmpty", targetPackageId) : t("scanNone"))
        : t("groupsEmpty")
    }</div>`;
    renderGroupPagination(0);
    return;
  }

  container.classList.remove("empty");
  container.innerHTML = visibleGroups
    .map((group) => {
      const isSelected = selected.has(group.key) ? "active" : "";
      const isFocused = group.key === state.selectedKey ? "focused" : "";
      const keepValue = getKeepValue(group);
      const keepRef = group.refs.find((item) => getRefValue(item) === keepValue);
      const keepClass = keepValue === KEEP_ALL_VALUE
        ? "keep-all"
        : keepRef?.package_id === getActiveTargetPackageId()
          ? "keep-self"
          : "keep-picked";
      const rowKeepClass = keepValue === KEEP_ALL_VALUE ? "row-keep-all" : "";
      const keepLabel = formatKeepChoice(group, keepValue);

      const sourceCount = group.refs.length;
      const reclaimText = formatBytesLocal(getGroupScopedMaxReclaimableBytes(group));
      const sizeCell = "";

      // Group label: prefer the path from the target VAR's own ref so the
      // row shows the user's local file, not whichever cross-package ref
      // happens to sort first. The CRC-match relocation target (often a
      // different folder/creator) is shown in the detail panel, not here.
      const targetPackageId = getActiveTargetPackageId();
      const labelRef = (targetPackageId
        ? group.refs.find((ref) => ref.package_id === targetPackageId)
        : null) || group.refs[0];
      return `
        <button class="group-row ${rowKeepClass} ${isSelected} ${isFocused}" data-key="${group.key}" type="button">
          <div class="group-row-top">
            <strong>${escapeHtml(labelRef.internal_path)}</strong>
            <span class="chip ${keepClass}">${escapeHtml(keepLabel)}</span>
          </div>
          <div class="group-row-meta">
            ${sizeCell}
            <span>${escapeHtml(t("selectedFiles", sourceCount))}</span>
            <span>${escapeHtml(t("reclaimable", reclaimText))}</span>
          </div>
        </button>
      `;
    })
    .join("");

  // Click + contextmenu are delegated on #group-list via
  // _ensureGroupListDelegated, so no per-row listeners needed here.

  renderGroupPagination(groups.length);
}

function getSelectedGroup() {
  return getFilteredGroups().find((item) => item.key === state.selectedKey) ?? null;
}

function selectedGroups() {
  const selected = selectedKeySet();
  return getFilteredGroups().filter((group) => selected.has(group.key));
}

function getRefValue(ref) {
  return `${ref.package_id}:${ref.internal_path}`;
}

function getRefPackageRef(ref) {
  return `${ref.package_id}:/${ref.internal_path}`;
}

function getPreferredSingleKeepRef(group) {
  const targetPackageId = getActiveTargetPackageId();
  const targetRef = group.refs.find((ref) => ref.package_id === targetPackageId);
  return targetRef ?? group.refs[0] ?? null;
}

function getPackageChoiceCounts(group) {
  const counts = new Map();
  for (const ref of group.refs) {
    counts.set(ref.package_id, (counts.get(ref.package_id) ?? 0) + 1);
  }
  return counts;
}

function getUniquePackageChoices(group) {
  return [...getPackageChoiceCounts(group).entries()]
    .filter(([, count]) => count === 1)
    .map(([packageId]) => packageId)
    .sort((a, b) => a.localeCompare(b, undefined, { sensitivity: "base" }));
}

function findUniqueRefByPackage(group, packageId) {
  const matches = group.refs.filter((item) => item.package_id === packageId);
  return matches.length === 1 ? matches[0] : null;
}

function getKeepValue(group) {
  const rawValue = state.keepMap[group.key];
  const preferredRef = getPreferredSingleKeepRef(group);
  if (rawValue === KEEP_ALL_VALUE || rawValue == null) {
    return preferredRef ? getRefValue(preferredRef) : getRefValue(group.refs[0]);
  }
  return rawValue;
}

function getActiveExportRef(group) {
  if (!group?.refs?.length) {
    return null;
  }
  const keepValue = getKeepValue(group);
  if (keepValue && keepValue !== KEEP_ALL_VALUE) {
    return group.refs.find((item) => getRefValue(item) === keepValue) ?? group.refs[0];
  }
  return group.refs[0];
}

function formatKeepChoice(group, keepValue) {
  if (!group || !keepValue) {
    return "";
  }
  if (keepValue === KEEP_ALL_VALUE) {
    return t("keepAllShort");
  }
  const ref = group.refs.find((item) => getRefValue(item) === keepValue);
  if (!ref) {
    return t("relocateTo", keepValue.split(":")[0]);
  }
  if (ref.package_id === getActiveTargetPackageId()) {
    return t("keepSelf");
  }
  return formatPackageActionLabel(group, ref);
}

function commonSelectedPackageChoices() {
  const groups = selectedGroups();
  if (!groups.length) {
    return [];
  }
  let common = new Set(getUniquePackageChoices(groups[0]));
  for (const group of groups.slice(1)) {
    common = new Set(getUniquePackageChoices(group).filter((id) => common.has(id)));
  }
  return [...common].sort((a, b) => a.localeCompare(b, undefined, { sensitivity: "base" }));
}

function findPackageScopeKeys(packageIds) {
  const needle = [...packageIds].sort().join("|");
  return (state.scan?.groups ?? []).filter((group) => [...group.package_ids].sort().join("|") === needle).map((group) => group.key);
}

function applyKeepChoiceToKeys(keys, keepValue) {
  let count = 0;
  let skipped = 0;
  for (const group of state.scan.groups) {
    if (!keys.includes(group.key)) {
      continue;
    }
    if (keepValue === KEEP_ALL_VALUE) {
      state.keepMap[group.key] = KEEP_ALL_VALUE;
      count += 1;
      continue;
    }
    const match = group.refs.find((item) => {
      const refValue = getRefValue(item);
      return refValue === keepValue || getRefPackageRef(item) === keepValue;
    });
    if (!match) {
      continue;
    }
    // Same incomplete-bundle gate as applyKeepToKeys / applyDbKeepToKeys.
    if (isRefIncomplete(match)) {
      skipped += 1;
      continue;
    }
    state.keepMap[group.key] = `${match.package_id}:${match.internal_path}`;
    count += 1;
  }
  if (count > 0) bumpKeepMutation();
  if (skipped > 0) {
    const pid = keepValue.split(":")[0] || keepValue;
    addLog(`Skipped ${skipped} group${skipped === 1 ? "" : "s"} where ${pid} has missing bundle siblings — unsafe as keep source.`);
  }
  return count;
}

function applyKeepToKeys(keys, packageId) {
  let count = 0;
  let skipped = 0;
  for (const group of state.scan.groups) {
    if (!keys.includes(group.key)) {
      continue;
    }
    const match = findUniqueRefByPackage(group, packageId);
    if (!match) {
      continue;
    }
    // Mirror the per-row "incomplete ref" gate from the detail picker. The
    // per-radio disable only protects direct clicks — without this check,
    // bulk-apply would happily set an incomplete-bundle ref as keep for
    // groups where the user can't even see/click it, breaking the cascade
    // at execute time. Skip silently per-group, log the total at the end.
    if (isRefIncomplete(match)) {
      skipped += 1;
      continue;
    }
    state.keepMap[group.key] = `${match.package_id}:${match.internal_path}`;
    count += 1;
  }
  if (count > 0) bumpKeepMutation();
  if (skipped > 0) {
    addLog(`Skipped ${skipped} group${skipped === 1 ? "" : "s"} where ${packageId} has missing bundle siblings — unsafe as keep source.`);
  }
  return count;
}

// Find Duplicates: relocate filtered groups to a DB package using the CRC
// map fetched by ensureDbPackageResources. For each key in `keys`, look up
// the group's CRC32 in the DB package's resourcesByCrc; if present, set
// keep_map to that DB resource. Groups with no CRC match are skipped so the
// user's earlier choices on them are preserved.
function applyDbKeepToKeys(keys, packageId) {
  const entry = state.dbPackageResources[packageId];
  if (!entry || entry.status !== "ready") return 0;
  const map = entry.resourcesByCrc;
  if (!map || map.size === 0) return 0;
  const keySet = new Set(keys);
  let count = 0;
  let skipped = 0;
  for (const group of state.scan?.groups ?? []) {
    if (!keySet.has(group.key)) continue;
    const crc = getGroupLookupCrc(group);
    if (crc == null) continue;
    const matched = map.get(crc);
    if (!matched) continue;
    // Same gate as applyKeepToKeys: don't write a keep that points at a
    // package missing required bundle siblings for this resource path,
    // even when the DB has a CRC match. The user can't click these in the
    // detail picker; bulk-apply must honor the same rule.
    if (getMissingSiblings(packageId, matched.internal_path).length > 0) {
      skipped += 1;
      continue;
    }
    state.keepMap[group.key] = `${packageId}:${matched.internal_path}`;
    count += 1;
  }
  if (count > 0) bumpKeepMutation();
  if (skipped > 0) {
    addLog(`Skipped ${skipped} group${skipped === 1 ? "" : "s"} where ${packageId} has missing bundle siblings — unsafe as keep source.`);
  }
  return count;
}

// Auto Target: for every scoped duplicate group whose keep is still the
// default (KEEP_ALL — i.e. the user hasn't manually picked a relocation
// target yet), pick the first available non-target ref as the keep choice.
// Manual selections are preserved. In Find Duplicates mode the "target VAR"
// is the user's scanned VAR; everything else in `group.refs` plus cached DB
// matches counts as a relocation candidate. DB matches that aren't cached
// yet get fetched first so every group has a chance to be assigned.
async function applyAutoTargetToScopedGroups() {
  if (!state.scan?.groups?.length) {
    addLog(t("autoTargetNone"));
    return;
  }
  const scoped = state.currentPage === "db-find" ? dbfGetScopedGroups() : getScopedGroups();
  const targetPid = state.currentPage === "db-find" ? dbfGetActiveTargetPackageId() : getActiveTargetPackageId();

  // Step 1: kick off DB-match fetches for unloaded groups that still need a
  // pick. Skip groups the user has already manually claimed.
  const pending = [];
  for (const group of scoped) {
    const rawKeep = state.keepMap[group.key];
    if (rawKeep && rawKeep !== KEEP_ALL_VALUE) continue;
    // If the group already has a non-target local ref we can use that
    // without needing a DB lookup.
    const hasLocalNonTarget = group.refs.some(
      (r) => !targetPid || r.package_id !== targetPid
    );
    if (hasLocalNonTarget) continue;
    const crc = getGroupLookupCrc(group);
    if (crc == null) continue;
    if (state.dbResourceMatches[crc]) continue;
    pending.push(fetchDbMatchesForGroup(group, crc, targetPid));
  }
  if (pending.length) {
    await Promise.all(pending);
  }

  // Step 2: pick a target for each eligible group.
  let assigned = 0;
  for (const group of scoped) {
    const rawKeep = state.keepMap[group.key];
    if (rawKeep && rawKeep !== KEEP_ALL_VALUE) continue;

    // Group key is "<crc-hex>:<size>" for real CRC groups (synthetic db-find
    // groups use "dbfind|..." and skip the size check). Used here to filter
    // size-mismatched DB matches out of the *auto-assign* path — the backend
    // trusts whatever the user picks at execute time, so this is a UI-side
    // heuristic only, not a correctness gate.
    const expectedSize = (() => {
      const parts = String(group.key).split(":");
      if (parts.length !== 2) return null;
      const n = Number(parts[1]);
      return Number.isFinite(n) ? n : null;
    })();

    let chosen = null;
    // Prefer a local non-target ref — no DB dependency needed at apply time.
    // Skip refs whose package is missing bundle siblings for this resource;
    // picking one would create the dangling-SELF cascade the per-row gate
    // also prevents. Fall through to the next eligible candidate.
    const localCandidates = group.refs.filter(
      (r) => (!targetPid || r.package_id !== targetPid) && !isRefIncomplete(r)
    );
    if (localCandidates.length) {
      const localNonTarget = localCandidates[0];
      chosen = `${localNonTarget.package_id}:${localNonTarget.internal_path}`;
    } else {
      const dbRefs = getGroupDbRefs(group);
      // Filter DB matches whose recorded size disagrees with the group's
      // size half — the backend validates this and would reject the pick.
      // Also drop DB refs whose package is incomplete for this path.
      const usable = (expectedSize == null
        ? dbRefs
        : dbRefs.filter((m) => Number(m.size) === expectedSize)
      ).filter((m) => getMissingSiblings(m.package_id, m.internal_path).length === 0);
      if (usable.length) {
        const dbRef = usable[0];
        chosen = `${dbRef.package_id}:${dbRef.internal_path}`;
        // Eagerly preload the picked package's full resource map so the
        // Apply-to-filtered button (gated on resourcesByCrc.size) can light
        // up immediately. No-op if already cached.
        ensureDbPackageResources(dbRef.package_id);
      }
    }
    if (chosen) {
      state.keepMap[group.key] = chosen;
      assigned += 1;
    }
  }

  if (assigned > 0) {
    bumpKeepMutation();
    addLog(t("autoTargetDone", assigned));
    renderGroups();
    renderDetail();
  } else {
    addLog(t("autoTargetNone"));
  }
}

// Favorite Source: like Auto Target above (identical manual-claim skip and
// incomplete-ref / size / missing-sibling gates), but only assigns groups
// where a favorite candidate exists. Ranking: a candidate whose package_id is
// itself favorited (2) beats one whose creator is favorited (1); the first
// highest-rank candidate wins ties. Favorite-package matching is EXACT id —
// favoriting Creator.Asset.3 does not make Creator.Asset.4 a favorite. The
// DB-ref fallback is consulted only when no local candidate ranks >= 1.
// Groups with no favorite candidate are left untouched — there is
// deliberately NO fallback to the generic first-candidate heuristic (that is
// what Auto Target is for; run this first, then Auto Target for the rest).
// Unlike Auto Target we also exclude blocked-creator candidates, because
// dbfRenderDetail hides those rows and a pick would be an invisible
// selection. Mutates only state.keepMap — never writes the DB.
async function applyFavoriteSourceToScopedGroups() {
  if (!state.scan?.groups?.length) {
    addLog(t("favSourceNone"));
    return;
  }
  if (_favoritePackages.size === 0 && _favoriteCreators.size === 0) {
    addLog(t("favSourceNoFavorites"));
    return;
  }
  const scoped = state.currentPage === "db-find" ? dbfGetScopedGroups() : getScopedGroups();
  const targetPid = state.currentPage === "db-find" ? dbfGetActiveTargetPackageId() : getActiveTargetPackageId();

  const favRank = (pid) =>
    _favoritePackages.has(pid) ? 2 : isCreatorFavoriteForPackage(pid) ? 1 : 0;
  const pickBest = (list, pidOf) => {
    let best = null;
    let bestRank = 0;
    for (const entry of list) {
      const rank = favRank(pidOf(entry));
      if (rank > bestRank) {
        best = entry;
        bestRank = rank;
        if (rank === 2) break;
      }
    }
    return best;
  };

  // DB candidates are only offered in the workspace's "db" mode — in local
  // mode dbfRenderDetail never shows DB rows, so a DB-sourced pick would be
  // an invisible selection.
  const dbAllowed = state.currentPage !== "db-find" || state.dbfMode === "db";

  // Step 1: kick off DB-match fetches for unassigned groups that don't
  // already have a USABLE favorite among their local non-target refs. Auto
  // Target skips the fetch whenever ANY local non-target exists; we must go
  // wider, because a non-favorite local ref doesn't satisfy this feature —
  // and "usable" applies the same incomplete/blocked gates as Step 2, or a
  // favorite that Step 2 rejects would have suppressed the fetch it needs.
  const pending = [];
  if (dbAllowed) {
    for (const group of scoped) {
      const rawKeep = state.keepMap[group.key];
      if (rawKeep && rawKeep !== KEEP_ALL_VALUE) continue;
      const hasLocalFavorite = group.refs.some(
        (r) =>
          (!targetPid || r.package_id !== targetPid) &&
          !isRefIncomplete(r) &&
          !isCreatorBlockedForPackage(r.package_id) &&
          favRank(r.package_id) > 0
      );
      if (hasLocalFavorite) continue;
      const crc = getGroupLookupCrc(group);
      if (crc == null) continue;
      // Only a settled "ready" entry can serve Step 2. A "loading" entry
      // belongs to someone else's in-flight fetch we cannot await, and an
      // "error" entry deserves a retry — re-fetching is a benign duplicate
      // read in both cases.
      if (state.dbResourceMatches[crc]?.status === "ready") continue;
      pending.push(fetchDbMatchesForGroup(group, crc, targetPid));
    }
  }
  if (pending.length) {
    await Promise.all(pending);
  }

  // Step 2: assign only where a favorite candidate exists.
  let assigned = 0;
  for (const group of scoped) {
    const rawKeep = state.keepMap[group.key];
    if (rawKeep && rawKeep !== KEEP_ALL_VALUE) continue;

    // Same "<crc-hex>:<size>" key parse as Auto Target — a UI-side size gate
    // for DB matches only; synthetic "dbfind|..." keys skip it.
    const expectedSize = (() => {
      const parts = String(group.key).split(":");
      if (parts.length !== 2) return null;
      const n = Number(parts[1]);
      return Number.isFinite(n) ? n : null;
    })();

    let chosen = null;
    const localCandidates = group.refs.filter(
      (r) =>
        (!targetPid || r.package_id !== targetPid) &&
        !isRefIncomplete(r) &&
        !isCreatorBlockedForPackage(r.package_id)
    );
    const localPick = pickBest(localCandidates, (r) => r.package_id);
    if (localPick) {
      chosen = `${localPick.package_id}:${localPick.internal_path}`;
    } else if (dbAllowed) {
      const dbRefs = getGroupDbRefs(group);
      const usable = (expectedSize == null
        ? dbRefs
        : dbRefs.filter((m) => Number(m.size) === expectedSize)
      )
        .filter((m) => getMissingSiblings(m.package_id, m.internal_path).length === 0)
        .filter((m) => !isCreatorBlockedForPackage(m.package_id));
      const dbPick = pickBest(usable, (m) => m.package_id);
      if (dbPick) {
        chosen = `${dbPick.package_id}:${dbPick.internal_path}`;
        // Same eager preload as Auto Target so Apply can light up immediately.
        ensureDbPackageResources(dbPick.package_id);
      }
    }
    if (chosen) {
      state.keepMap[group.key] = chosen;
      assigned += 1;
    }
  }

  if (assigned > 0) {
    bumpKeepMutation();
    addLog(t("favSourceDone", assigned));
    renderGroups();
    renderDetail();
  } else {
    addLog(t("favSourceNone"));
  }
}

function getCurrentKeepValue() {
  const checked = document.querySelector('input[name="keep-source"]:checked');
  if (checked) {
    return checked.value;
  }
  const group = getSelectedGroup();
  if (!group) {
    return null;
  }
  return state.keepMap[group.key] ?? getRefValue(group.refs[0]);
}

function getCurrentKeepPackage() {
  const keepValue = getCurrentKeepValue();
  if (!keepValue || keepValue === KEEP_ALL_VALUE) {
    return null;
  }
  return keepValue.split(":")[0];
}

// Lets the user deselect a keep-source radio by clicking it again — native
// radios can't toggle off, so without this the only way to clear a pick was
// to hit the "reset all" button. A second click on the already-selected
// option reverts the group to KEEP_ALL_VALUE ("do not dedupe this group").
//
// The mousedown listener has to live on the wrapping <label>, not the input:
// when the user clicks the label (which is what the source rows actually
// expose to the cursor), mousedown fires on the label/inner content and the
// click is then forwarded to the input synthetically — listening on the
// input alone would never see the pre-click checked state.
function attachKeepRadioToggle(input, groupKey) {
  if (input.value === KEEP_ALL_VALUE) return;
  const target = input.closest("label") ?? input;
  let preClickChecked = false;
  target.addEventListener("mousedown", () => {
    preClickChecked = input.checked;
  });
  input.addEventListener("click", () => {
    if (!preClickChecked) return;
    preClickChecked = false;
    input.checked = false;
    state.keepMap[groupKey] = KEEP_ALL_VALUE;
    bumpKeepMutation();
    renderGroups();
    renderDetail();
  });
}

function getDetailSubtitleText() {
  return t("detailSubtitleSingle");
}

function getKeepSourceTitleText() {
  return t("keepSourceTitleSingle");
}

function formatPackageActionLabel(_group, ref) {
  if (ref.package_id === getActiveTargetPackageId()) {
    return t("keepSelf");
  }
  return ref.package_id;
}

function buildEffectiveKeepMap() {
  const effectiveKeepMap = {};
  for (const group of state.scan?.groups ?? []) {
    effectiveKeepMap[group.key] = getKeepValue(group);
  }
  return effectiveKeepMap;
}

function refreshAfterKeepChange() {
  renderGroups();
  renderDetail();
  hideContextMenu();
}

async function copyTextToClipboard(text) {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
    } else {
      // Older webview fallback — the textarea + execCommand stays in the
      // same user-gesture window so the copy actually succeeds.
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      document.body.removeChild(ta);
    }
    return true;
  } catch (error) {
    addLog(t("crcCopyFailed", String(error)));
    return false;
  }
}

// Switch to the Resource List page and seed its search box with the CRC32
// (8-char hex). The view's existing search machinery handles the refetch and
// pagination reset — we just push the new filter through it.
function searchResourceListByCrc(crc) {
  if (crc == null) return;
  const hex = formatCrc32Hex(crc);
  if (hex === "—") return;

  const link = document.querySelector('[data-sidebar-link="resource-list"]');
  if (link) link.click();

  const input = $("resource-list-filter");
  if (input) input.value = hex;
  if (state.resourceList) {
    state.resourceList.filter = hex;
    state.resourceList.page = 0;
  }
  // The sidebar click triggered __refreshResourceListView (render-only on
  // re-entry); kick a real refetch so the new filter actually queries.
  if (typeof window.__rlRefreshResourceList === "function") {
    window.__rlRefreshResourceList();
  }
  hideContextMenu();
}

async function openSourceRowInVarDetails(packageId, packageFile, sourceType) {
  if (!packageId) return;
  if (typeof openVarDetailsView !== "function") return;
  hideContextMenu();

  const cached = (state.varPackagesItems ?? []).find(
    (it) => it.package_id === packageId || (packageFile && it.file_path === packageFile)
  );

  // If we have a file path, try to read it from disk. A successful stats call
  // means the file really is local — load with full scene image / size /
  // modified-time metadata and flag the item as local so the path + send-to
  // tiles light up. Fall through to the synthetic flow on any failure (file
  // moved, db row pointing at stale path, etc.).
  if (packageFile && invoke) {
    try {
      const stats = await invoke("get_var_file_stats", { packagePath: packageFile });
      if (stats) {
        const fileName = packageFile.split(/[\\/]/).pop() || `${packageId}.var`;
        const item = {
          package_id: packageId,
          file_path: packageFile,
          file_name: fileName,
          creator: cached?.creator ?? deriveCreatorFromPackageId(packageId),
          size_bytes: stats.size_bytes ?? cached?.size_bytes ?? 0,
          modified_ms: stats.modified_ms ?? cached?.modified_ms ?? null,
          indexed: cached ? Boolean(cached.indexed) : true,
          scene_image_data: stats.scene_image_data ?? null,
        };
        showVarDetailsView();
        selectVarDetailsItem(item, "local");
        return;
      }
    } catch (_error) {
      // File isn't readable on disk — fall back to the synthetic flow below.
    }
  }

  const source = cached
    ? "folder"
    : (sourceType === "db" ? "db" : (packageFile ? "local" : "db"));
  const item = cached ?? {
    package_id: packageId,
    file_path: packageFile || "",
    file_name: packageFile ? (packageFile.split(/[\\/]/).pop() || `${packageId}.var`) : `${packageId}.var`,
    creator: deriveCreatorFromPackageId(packageId),
    size_bytes: 0,
    modified_ms: null,
    indexed: Boolean(packageFile),
    scene_image_data: null,
  };
  openVarDetailsView(item, source);
}

function groupMenuItems() {
  const selectedCount = state.selectedKeys.length;
  const currentGroup = getSelectedGroup();
  if (!currentGroup) {
    return [];
  }

  const items = [];

  // CRC actions — always available when we can derive a CRC for the group.
  // Ordered before the keep/relocate items so the user doesn't have to scroll
  // past the per-package list to copy or jump to the Resource List.
  const lookupCrc = getGroupLookupCrc(currentGroup);
  if (lookupCrc != null) {
    const hex = formatCrc32Hex(lookupCrc);
    items.push({
      label: t("menuSearchByCrc", hex),
      action: () => searchResourceListByCrc(lookupCrc),
    });
    items.push({
      label: t("menuCopyCrc", hex),
      action: async () => {
        const ok = await copyTextToClipboard(hex);
        if (ok) addLog(t("crcCopied", hex));
      },
    });
    items.push({ separator: true });
  }

  const currentPackages = getUniquePackageChoices(currentGroup);
  for (const packageId of currentPackages) {
    items.push({
      label: t("menuSingleVarApply", packageId),
      action: () => {
        applyKeepToKeys(getScopedGroups().map((item) => item.key), packageId);
        refreshAfterKeepChange();
      },
    });
  }
  return items;
}

// Renders one item (or separator) to HTML. `parent` marks an item that opens a
// flyout submenu (rendered with a ▸ marker).
function contextMenuItemHtml(item) {
  if (item.separator) {
    return '<div class="context-menu-separator"></div>';
  }
  const danger = item.danger ? " context-menu-item-danger" : "";
  const hasSub = Array.isArray(item.submenu) && item.submenu.length > 0;
  const parent = hasSub ? " context-menu-item-parent" : "";
  const arrow = hasSub ? '<span class="context-menu-arrow">▸</span>' : "";
  return `<button class="context-menu-item${danger}${parent}" type="button">${escapeHtml(item.label)}${arrow}</button>`;
}

// Runs a leaf item's action, closing the whole menu first.
async function runContextMenuAction(action) {
  hideContextMenu();
  if (!action) return;
  try {
    await action();
  } catch (error) {
    addLog(String(error));
  }
}

function showContextMenu(x, y, items) {
  const menu = $("context-menu");
  if (!items.length) {
    return;
  }

  state.contextMenuItems = items;
  hideContextSubmenu();

  menu.innerHTML = items.map(contextMenuItemHtml).join("");

  // Buttons map 1:1 to the non-separator items, in order.
  const flat = items.filter((item) => !item.separator);
  menu.querySelectorAll(".context-menu-item").forEach((button, index) => {
    const item = flat[index];
    if (!item) return;
    if (Array.isArray(item.submenu) && item.submenu.length > 0) {
      const open = () => showContextSubmenu(button, item.submenu);
      button.addEventListener("mouseenter", open);
      button.addEventListener("click", (event) => {
        event.stopPropagation();
        open();
      });
    } else {
      // Hovering a leaf item dismisses any open flyout.
      button.addEventListener("mouseenter", hideContextSubmenu);
      button.addEventListener("click", () => runContextMenuAction(item.action));
    }
  });

  menu.classList.remove("hidden");
  const maxX = window.innerWidth - menu.offsetWidth - 8;
  const maxY = window.innerHeight - menu.offsetHeight - 8;
  menu.style.left = `${Math.max(8, Math.min(x, maxX))}px`;
  menu.style.top = `${Math.max(8, Math.min(y, maxY))}px`;
}

// Opens the flyout submenu to the right of `parentButton` (flips left / clamps
// vertically to stay on screen). A separate top-level element so the parent
// menu's overflow scrolling doesn't clip it.
function showContextSubmenu(parentButton, subItems) {
  const submenu = $("context-submenu");
  if (!submenu) return;
  submenu.innerHTML = subItems.map(contextMenuItemHtml).join("");
  const flat = subItems.filter((item) => !item.separator);
  submenu.querySelectorAll(".context-menu-item").forEach((button, index) => {
    const item = flat[index];
    if (!item) return;
    button.addEventListener("click", () => runContextMenuAction(item.action));
  });

  submenu.classList.remove("hidden");
  submenu.style.left = "0px";
  submenu.style.top = "0px";
  const rect = parentButton.getBoundingClientRect();
  const w = submenu.offsetWidth;
  const h = submenu.offsetHeight;
  // 2px overlap so the cursor can cross from parent to submenu without a gap.
  let left = rect.right - 2;
  if (left + w > window.innerWidth - 8) left = rect.left - w + 2;
  let top = rect.top - 4;
  if (top + h > window.innerHeight - 8) top = window.innerHeight - h - 8;
  submenu.style.left = `${Math.max(8, left)}px`;
  submenu.style.top = `${Math.max(8, top)}px`;
}

function hideContextSubmenu() {
  const submenu = $("context-submenu");
  if (submenu) {
    submenu.classList.add("hidden");
    submenu.innerHTML = "";
  }
}

function hideContextMenu() {
  state.contextMenuItems = [];
  $("context-menu").classList.add("hidden");
  hideContextSubmenu();
}

function handleWindowScroll(event) {
  if (event.target instanceof Element && event.target.closest(".context-menu")) {
    return;
  }
  hideContextMenu();
}

function renderDetail() {
  if (state.currentPage === "db-find") return dbfRenderDetail();
  const group = getSelectedGroup();
  if (group && state.lastDetailKey !== null && state.lastDetailKey !== group.key) {
    // Selection changed — unload all previously-loaded catalog matches so
    // stale DB data from the prior resource doesn't linger. The new resource
    // will trigger its own fresh fetch via appendDbMatchRows.
    state.dbResourceMatches = {};
  }
  state.lastDetailKey = group?.key ?? null;

  const empty = $("detail-empty");
  const panel = $("detail-panel");
  const preview = $("detail-preview");
  const resetButton = $("reset-keep-button");
  const exportButton = $("export-resource-button");
  const scopeButton = $("apply-scope-button");
  const filteredButton = $("apply-filtered-button");
  const globalButton = $("apply-global-button");

  if (!group) {
    empty.classList.remove("hidden");
    panel.classList.add("hidden");
    preview.classList.add("hidden");
    preview.innerHTML = "";
    resetButton.disabled = true;
    exportButton.disabled = true;
    scopeButton.disabled = true;
    filteredButton.disabled = true;
    globalButton.disabled = true;
    scopeButton.classList.remove("hidden");
    globalButton.classList.remove("hidden");
    return;
  }

  empty.classList.add("hidden");
  panel.classList.remove("hidden");
  $("detail-subtitle").textContent = getDetailSubtitleText();
  $("keep-source-title").textContent = getKeepSourceTitleText();

  const keepValue = getKeepValue(group);
  // Keep options are *relocation targets* — packages we'd redirect the
  // source's references to. Refs from the same VAR as the source are not
  // valid relocation candidates:
  //   - Picking one means "rewrite refs from one local path to a different
  //     local path in the same VAR." The dedup engine can't act on that
  //     coherently and the backend rejected such picks ("Invalid keep
  //     choice ...") when a single VAR carried two CRC-identical files
  //     (e.g. `xingye - head.vmb` + `xingye - head-A月清.vmb`).
  //   - The source file itself is implicit; the left-panel row already
  //     shows what we're processing.
  // After excluding target refs, also dedupe by package_id so a single
  // *external* package contributing multiple internal CRC-duplicates
  // surfaces only one representative row. DB matches (rendered separately
  // by appendDbMatchRows) are unaffected.
  const targetPackageId = getActiveTargetPackageId();
  const sorted = [...group.refs]
    .filter((r) => r.package_id !== targetPackageId)
    .filter((r) => !isCreatorBlockedForPackage(r.package_id))
    .sort((a, b) =>
      a.package_id.localeCompare(b.package_id) ||
      a.internal_path.localeCompare(b.internal_path)
    );
  const seen = new Set();
  const sortedRefs = [];
  for (const ref of sorted) {
    if (seen.has(ref.package_id)) {
      if (getRefValue(ref) === keepValue) {
        const idx = sortedRefs.findIndex((r) => r.package_id === ref.package_id);
        if (idx >= 0) sortedRefs[idx] = ref;
      }
      continue;
    }
    seen.add(ref.package_id);
    sortedRefs.push(ref);
  }
  $("detail-hash").textContent = group.key;
  $("detail-file-count").textContent = t("selectedFiles", group.refs.length);
  $("detail-space").textContent = t(
    "reclaimable",
    formatBytesLocal(getGroupScopedMaxReclaimableBytes(group))
  );

  // Overview never tags rows as "Local"; the Local/DB distinction is
  // Find-Duplicates–only and is rendered by dbfRenderDetail.
  const localTagHtml = "";

  // The source target ref is excluded from sortedRefs because it isn't a
  // valid relocation target. Surface it as a non-selectable info row so the
  // panel always communicates which local file is being relocated FROM,
  // even before DB matches arrive.
  const sourceInfoRefs = targetPackageId
    ? group.refs.filter((r) => r.package_id === targetPackageId)
    : [];
  const sourceInfoHtml = sourceInfoRefs
    .map((ref) => {
      const packageFile = state.scan?.package_files?.[ref.package_id] ?? "";
      return `
        <div class="source-row source-row-info" data-package-file="${escapeAttribute(packageFile)}" data-package-id="${escapeAttribute(ref.package_id)}" data-source-type="local">
          <div class="source-main">
            <strong>${escapeHtml(t("sourceFromLocalLabel"))}</strong>
            <span>${escapeHtml(ref.internal_path)}</span>
            <span class="source-file-path">${escapeHtml(packageFile)}</span>
          </div>
        </div>
      `;
    })
    .join("");

  $("keep-options").innerHTML = `
    ${sourceInfoHtml}
    ${sortedRefs
      .map((ref) => {
        const value = `${ref.package_id}:${ref.internal_path}`;
        const packageFile = state.scan?.package_files?.[ref.package_id] ?? "";
        const isTargetRef = ref.package_id === getActiveTargetPackageId();
        const incomplete = isRefIncomplete(ref);
        const warningHtml = renderIncompleteRefWarning(ref);
        const classes = ["source-row"];
        if (isTargetRef) classes.push("source-row-self");
        if (incomplete) classes.push("source-row-incomplete");
        const radioChecked = !incomplete && keepValue === value ? "checked" : "";
        return `
          <label class="${classes.join(" ")}" data-package-file="${escapeAttribute(packageFile)}" data-package-id="${escapeAttribute(ref.package_id)}" data-source-type="local">
            <input type="radio" name="keep-source" value="${escapeAttribute(value)}" ${radioChecked} ${incomplete ? "disabled" : ""} />
            <div class="source-main">
              <strong>${escapeHtml(formatPackageActionLabel(group, ref))}${localTagHtml}</strong>
              <span>${escapeHtml(ref.internal_path)}</span>
              <span class="source-file-path">${escapeHtml(packageFile)}</span>
              ${warningHtml}
            </div>
          </label>
        `;
      })
      .join("")}
  `;

  $("keep-options").querySelectorAll('input[name="keep-source"]').forEach((input) => {
    attachKeepRadioToggle(input, group.key);
    input.addEventListener("change", () => {
      state.keepMap[group.key] = input.value;
      bumpKeepMutation();
      renderGroups();
      renderDetail();
    });
  });

  $("keep-options").querySelectorAll(".source-row[data-package-file]").forEach((element) => {
    element.addEventListener("contextmenu", (event) => {
      event.preventDefault();
      const items = [];
      const packageId = element.dataset.packageId || "";
      const packageFile = element.dataset.packageFile || "";
      const sourceType = element.dataset.sourceType || "local";
      if (packageId) {
        items.push({
          label: t("resourceListContextOpenPackage"),
          action: () => openSourceRowInVarDetails(packageId, packageFile, sourceType),
        });
      }
      if (packageFile) {
        items.push({
          label: t("menuShowInExplorer"),
          action: () => showPackageInExplorer(packageFile),
        });
      }
      // "Disable creator" — block this row's creator from appearing as a
      // dedup candidate anywhere. Skipped for the source target's own row
      // (the user's working VAR) and for rows we can't extract a creator
      // from (id has no leading "Creator." segment).
      const creator = deriveCreatorFromPackageId(packageId);
      const isTargetRow = packageId === getActiveTargetPackageId();
      if (creator && !isTargetRow && !_blockedCreators.has(creator)) {
        items.push({
          label: `Disable creator (${creator})`,
          action: () => blockCreatorByPackageId(packageId),
        });
      }
      showContextMenu(event.clientX, event.clientY, items);
    });
  });

  // Note: an earlier version of this function appended DB-match rows inline
  // here when `isDbFindMode` was set. That branch is now handled entirely by
  // `dbfRenderDetail` (which `renderDetail` dispatches to at the top), and
  // `isDbFindMode` was never declared in this scope — so the original block
  // threw `ReferenceError: isDbFindMode is not defined` and silently crashed
  // `renderDetail`, leaving every Apply / Reset / Export button stuck in its
  // HTML-default `disabled` state. Removed.

  // The keep value is a *relocation target* (an external pid), so it's not
  // in sortedRefs and won't match any preview ref. Use the local source
  // ref(s) instead so the preview shows the file we actually have on disk.
  renderGroupPreview(sourceInfoRefs, keepValue);

  // Source of truth for the apply-buttons' "what would we apply?" question
  // is `state.keepMap[group.key]` — NOT the DOM. The Find Duplicates page
  // already does it this way (see the `dbf-apply-filtered-button` click
  // handler at the bottom of this file) and its button works reliably; the
  // Overview path was previously routing through `getCurrentKeepPackage()`
  // → `getCurrentKeepValue()` which does a global
  // `document.querySelector('input[name="keep-source"]:checked')` and is
  // sensitive to DOM render order / stale state. Pull from state directly
  // so the button enable matches the data the click handler will act on.
  const groupKeepRaw = state.keepMap[group.key];
  const selectedPackage = groupKeepRaw && groupKeepRaw !== KEEP_ALL_VALUE
    ? groupKeepRaw.split(":")[0]
    : null;
  const exportRef = getActiveExportRef(group);
  const filteredKeys = getFilteredGroups().map((item) => item.key);
  resetButton.disabled = !state.scan;
  exportButton.disabled = !exportRef;
  // A DB-sourced selection is also a valid apply target on Find Duplicates:
  // once we've fetched the DB package's full resource listing into
  // state.dbPackageResources, we can relocate filtered groups to whichever
  // resource in that package shares each group's CRC32.
  const dbPkgEntry = selectedPackage ? state.dbPackageResources[selectedPackage] : null;
  const dbPkgReady = !!dbPkgEntry
    && dbPkgEntry.status === "ready"
    && (dbPkgEntry.resourcesByCrc?.size ?? 0) > 0;
  // Enable rule mirrors the Find Duplicates page (`dbf-apply-filtered-button`):
  // the button reflects "is this operation conceptually possible right now?"
  // — i.e. are there filtered groups to act on. The Keep-pick prerequisite
  // is enforced at click-time with an inline status message rather than by
  // silently disabling, because users were unable to figure out *why* the
  // button was greyed-out and assumed it was broken. Scope/Global stay
  // hidden — they were batch-mode affordances and the single-VAR flow only
  // uses Filtered + Auto Target.
  filteredButton.disabled = !filteredKeys.length;
  scopeButton.disabled = true;
  globalButton.disabled = true;
  scopeButton.classList.add("hidden");
  globalButton.classList.add("hidden");
  // Auto Target picks a relocation target for every scoped group that
  // doesn't have a manual pick yet. Only useful when there are groups.
  const autoTargetButton = $("auto-target-button");
  if (autoTargetButton) {
    const hasGroups = (state.scan?.groups?.length ?? 0) > 0;
    autoTargetButton.classList.remove("hidden");
    autoTargetButton.disabled = !hasGroups;
  }
  $("quick-actions-hint").textContent = "";

  // Click handlers read `selectedPackage` from the same state.keepMap-derived
  // value used for the enable check above — so what the button looks like
  // and what it does are guaranteed to agree. `applyKeepToKeys` and
  // `applyDbKeepToKeys` each skip per-group when the chosen package isn't
  // unique (or has no CRC match) for that specific group, so a "noisy"
  // pick in the current group never produces an invalid write.
  scopeButton.onclick = () => {
    if (!selectedPackage) return;
    applyKeepToKeys(findPackageScopeKeys(group.package_ids), selectedPackage);
    renderGroups();
    renderDetail();
  };

  globalButton.onclick = () => {
    if (!selectedPackage) return;
    applyKeepToKeys(getScopedGroups().map((item) => item.key), selectedPackage);
    renderGroups();
    renderDetail();
  };

  filteredButton.onclick = () => {
    // The button is always enabled when there are filtered groups (matching
    // Find Duplicates' UX). If the user hasn't picked a non-KEEP_ALL Keep
    // on the current group yet, there's nothing to apply — say so via the
    // activity log so they understand what to do next, instead of having
    // the click silently no-op.
    if (!selectedPackage) {
      addLog(
        "Pick a Keep source on the current group first, then click 'Apply to filtered groups' to copy that choice across every filtered group where it's available."
      );
      return;
    }
    const filteredKeysNow = getFilteredGroups().map((item) => item.key);
    const dbEntry = state.dbPackageResources[selectedPackage];
    const useDb = dbEntry?.status === "ready"
      && (dbEntry.resourcesByCrc?.size ?? 0) > 0;
    const applied = useDb
      ? applyDbKeepToKeys(filteredKeysNow, selectedPackage)
      : applyKeepToKeys(filteredKeysNow, selectedPackage);
    if (typeof applied === "number") {
      addLog(
        applied === 0
          ? `No filtered groups had ${selectedPackage} as a unique candidate — nothing was applied.`
          : `Applied ${selectedPackage} to ${applied} filtered group${applied === 1 ? "" : "s"}.`
      );
    }
    renderGroups();
    renderDetail();
  };

  resetButton.onclick = () => {
    state.keepMap = { ...state.defaultKeepMap };
    bumpKeepMutation();
    renderGroups();
    renderDetail();
  };

  exportButton.onclick = async () => {
    try {
      await exportCurrentResource(group);
    } catch (error) {
      addLog(String(error));
    }
  };
}

function isBundleExportPath(path) {
  return /\.(vam|vmi)$/i.test(String(path ?? ""));
}

function getPathStem(path) {
  const fileName = String(path ?? "").split("/").pop() || "";
  return fileName.replace(/\.[^.]+$/, "") || fileName || "vam-export";
}

function joinDisplayPath(base, name) {
  const root = String(base ?? "").replace(/[\\/]+$/, "");
  return root ? `${root}\\${name}` : name;
}

function getPreviewCacheKey(packageId, internalPath) {
  return `${packageId}:${internalPath}`;
}

function isPreviewablePath(path) {
  return /\.(vam|png|jpe?g)$/i.test(String(path ?? ""));
}

function shouldDeferLargePreview(image) {
  return Number(image?.width ?? 0) > 2048 || Number(image?.height ?? 0) > 2048;
}

function getPreviewRefForGroup(refs, keepValue) {
  if (keepValue && keepValue !== KEEP_ALL_VALUE) {
    const keepRef = refs.find((ref) => getRefValue(ref) === keepValue);
    if (keepRef && isPreviewablePath(keepRef.internal_path)) {
      return keepRef;
    }
  }
  return refs.find((ref) => isPreviewablePath(ref.internal_path)) ?? null;
}

function buildPreviewMarkup(ref, packageFile) {
  const key = getPreviewCacheKey(ref.package_id, ref.internal_path);
  const entry = state.previewCache[key];
  const packageName = packageFile.split(/[\\/]/).pop() || ref.package_id;
  const header = `
    <div class="vam-preview-head">
      <strong>${escapeHtml(t("previewTitle"))}</strong>
      <span class="vam-preview-count">${escapeHtml(packageName)}</span>
    </div>
  `;

  if (!invoke) {
    return `${header}<div class="vam-preview-empty">${escapeHtml(t("previewError"))}</div>`;
  }

  if (!entry) {
    return `${header}<div class="vam-preview-empty">${escapeHtml(t("previewLoading"))}</div>`;
  }

  if (entry.status === "loading") {
    const startedAt = Number(entry.startedAt ?? 0);
    const loadingFor = startedAt ? Math.max(0, Math.round((Date.now() - startedAt) / 1000)) : 0;
    return `${header}<div class="vam-preview-empty">${escapeHtml(`${t("previewLoading")} (${loadingFor}s)`)}</div>`;
  }

  if (entry.status === "error") {
    return `${header}<div class="vam-preview-empty">${escapeHtml(t("previewError"))}: ${escapeHtml(entry.error)}</div>`;
  }

  const images = entry.data?.images ?? [];
  if (!images.length) {
    return `${header}<div class="vam-preview-empty">${escapeHtml(t("previewEmpty"))}</div>`;
  }

  return `
    <div class="vam-preview-head">
      <strong>${escapeHtml(t("previewTitle"))}</strong>
      <span class="vam-preview-count">${escapeHtml(t("previewCount", images.length))}</span>
    </div>
    <div class="vam-preview-grid">
      ${images
        .map((image, index) => `
          <figure class="vam-preview-card">
            ${buildPreviewImageNode(image, key, index)}
            <figcaption class="vam-preview-meta">
              <span>${escapeHtml(image.internal_path.split("/").pop() || image.internal_path)}</span>
              <span>${escapeHtml(formatBytesLocal(image.size))}</span>
              <span data-resolution-for="${escapeAttribute(`${key}:${index}`)}">${escapeHtml(
                image.width && image.height ? t("previewResolution", image.width, image.height) : ""
              )}</span>
            </figcaption>
          </figure>
        `)
        .join("")}
    </div>
  `;
}

function buildPreviewImageNode(image, cacheKey, index) {
  if (shouldDeferLargePreview(image) || !image.data_url) {
    return `
      <button
        class="vam-preview-deferred ghost-button"
        type="button"
        data-preview-load="1"
        data-preview-cache-key="${escapeAttribute(cacheKey)}"
        data-preview-index="${escapeAttribute(index)}"
      >${escapeHtml(t("previewLoadLarge"))}</button>
    `;
  }

  return `
    <img
      class="vam-preview-image"
      src="${escapeAttribute(image.data_url)}"
      alt="${escapeAttribute(image.internal_path)}"
      loading="lazy"
      data-resolution-id="${escapeAttribute(`${cacheKey}:${index}`)}"
    />
  `;
}

function bindPreviewResolution(slot) {
  slot.querySelectorAll(".vam-preview-image").forEach((image) => {
    const sync = () => {
      const resolutionId = image.dataset.resolutionId || "";
      const target = [...slot.querySelectorAll("[data-resolution-for]")].find(
        (element) => element.dataset.resolutionFor === resolutionId
      );
      if (!target) {
        return;
      }
      const width = image.naturalWidth;
      const height = image.naturalHeight;
      target.textContent = width && height ? t("previewResolution", width, height) : "";
    };
    if (image.complete) {
      sync();
    } else {
      image.addEventListener("load", sync, { once: true });
    }
  });
}

function bindPreviewActions(slot) {
  slot.querySelectorAll("[data-preview-load]").forEach((button) => {
    button.addEventListener("click", async () => {
      const cacheKey = button.dataset.previewCacheKey;
      const index = Number(button.dataset.previewIndex ?? -1);
      if (!cacheKey || index < 0) {
        return;
      }

      const entry = state.previewCache[cacheKey];
      const image = entry?.data?.images?.[index];
      const activeGroup = getSelectedGroup();
      const previewRef = activeGroup ? getPreviewRefForGroup(activeGroup.refs, getKeepValue(activeGroup)) : null;
      // Resolution order: Find Duplicates / Overview state first (the
      // original use case). When the preview lives inside the VAR Details
      // sheet there is no active group — fall back to the VAR Details
      // item's own file_path, which is guaranteed on-disk because the
      // sheet only renders when `state.varDetails.itemIsLocal` is true.
      let packagePath = previewRef
        ? state.scan?.package_files?.[previewRef.package_id]
        : null;
      if (!packagePath) {
        const vdItem = state.varDetails?.item;
        if (vdItem && state.varDetails?.itemIsLocal && vdItem.file_path) {
          packagePath = vdItem.file_path;
        }
      }
      if (!image || !packagePath || !invoke) {
        return;
      }

      button.disabled = true;
      button.textContent = t("previewLoading");
      try {
        const dataUrl = await invoke("load_preview_image_data", {
          packagePath,
          internalPath: image.internal_path,
        });
        const img = document.createElement("img");
        img.className = "vam-preview-image";
        img.src = dataUrl;
        img.alt = image.internal_path;
        img.loading = "lazy";
        img.dataset.resolutionId = `${cacheKey}:${index}`;
        button.replaceWith(img);
        bindPreviewResolution(slot);
      } catch (error) {
        button.disabled = false;
        button.textContent = `${t("previewError")}: ${String(error)}`;
      }
    });
  });
}

// Appends database-match rows (and a loading/error placeholder) to the
// keep-options panel for the currently selected group when on the Find
// Duplicates page. Local refs are rendered first by renderDetail; this
// runs after to preserve "local source takes precedence" ordering.
function appendDbMatchRows(group) {
  const crc = getGroupLookupCrc(group);
  if (crc == null) {
    return;
  }
  const targetPid = getActiveTargetPackageId();
  const entry = state.dbResourceMatches[crc];

  if (!entry) {
    appendDbLoadingRow();
    fetchDbMatchesForGroup(group, crc, targetPid);
    return;
  }
  if (entry.status === "loading") {
    appendDbLoadingRow();
    return;
  }
  if (entry.status === "error") {
    appendDbErrorRow(entry.error);
    return;
  }
  // ready
  const localKey = (pid, path) => `${pid}|${path}`;
  const localRefKeys = new Set(group.refs.map((r) => localKey(r.package_id, r.internal_path)));
  let dbRefs = (entry.refs || []).filter(
    (m) => !localRefKeys.has(localKey(m.package_id, m.internal_path))
  );

  // Apply the user's package-name filter — only affects DB rows; locals were
  // rendered earlier in renderDetail and are never filtered.
  const filterRaw = ($("dbfind-source-filter")?.value || "").trim().toLowerCase();
  if (filterRaw) {
    dbRefs = dbRefs.filter((m) => m.package_id.toLowerCase().includes(filterRaw));
  }

  for (const dbRef of dbRefs) {
    appendDbMatchRow(group, dbRef);
  }
}

// Lazy-fetches every resource of `pid` from the indexed catalog and builds a
// CRC32→ResourceRef map so apply-filtered can relocate target-VAR resources
// to the picked DB package via CRC matching. Cached per package_id; only
// fires once. Re-renders the detail panel on resolution so the gate updates.
async function ensureDbPackageResources(pid) {
  if (!pid) return;
  if (state.dbPackageResources[pid]) return; // already loading / ready / errored
  state.dbPackageResources[pid] = { status: "loading" };
  if (!invoke) {
    state.dbPackageResources[pid] = {
      status: "ready",
      resourcesByCrc: new Map(),
      resources: [],
    };
    return;
  }
  try {
    const refs = await invoke("load_db_package_resources", { packageId: pid });
    const list = Array.isArray(refs) ? refs : [];
    const map = new Map(
      list.filter((r) => r.crc32 != null).map((r) => [r.crc32 >>> 0, r])
    );
    state.dbPackageResources[pid] = {
      status: "ready",
      resourcesByCrc: map,
      resources: list,
    };
  } catch (err) {
    state.dbPackageResources[pid] = { status: "error", error: String(err) };
  }
  if (state.currentPage === "db-find" && dbfGetSelectedGroup()) {
    renderDetail();
  }
}

async function fetchDbMatchesForGroup(group, crc, targetPid) {
  if (!invoke) {
    state.dbResourceMatches[crc] = { status: "ready", refs: [] };
    return;
  }
  state.dbResourceMatches[crc] = { status: "loading" };
  try {
    const refs = await invoke("find_resources_by_crc", {
      crc32: crc,
      excludePackageId: targetPid || null,
    });
    state.dbResourceMatches[crc] = { status: "ready", refs: Array.isArray(refs) ? refs : [] };
  } catch (err) {
    state.dbResourceMatches[crc] = { status: "error", error: String(err) };
  }
  // Re-render so the group row's source-count, reclaim chip, and size info
  // reflect the freshly arrived DB matches. Re-running the right panel only
  // when the same group is still selected keeps focus stable when the user
  // has clicked through to a different row before the fetch resolved.
  renderGroups();
  if (getSelectedGroup()?.key === group.key) {
    renderDetail();
  }
}

function appendDbLoadingRow() {
  const container = $("keep-options");
  if (!container) return;
  const div = document.createElement("div");
  div.className = "source-row source-row-loading";
  div.innerHTML = `
    <div class="source-main">
      <strong>
        <span class="source-loading-spinner" aria-hidden="true"></span>
        Searching database…
        <span class="source-tag source-tag-db">DB</span>
      </strong>
    </div>
  `;
  container.appendChild(div);
}

function appendDbErrorRow(message) {
  const container = $("keep-options");
  if (!container) return;
  const div = document.createElement("div");
  div.className = "source-row source-row-error";
  div.innerHTML = `
    <div class="source-main">
      <strong>Database lookup failed <span class="source-tag source-tag-db">DB</span></strong>
      <span>${escapeHtml(message || "")}</span>
    </div>
  `;
  container.appendChild(div);
}

function appendDbMatchRow(group, dbRef) {
  const container = $("keep-options");
  if (!container) return;
  const value = `${dbRef.package_id}:${dbRef.internal_path}`;
  const checked = getKeepValue(group) === value ? "checked" : "";
  const filePath = dbRef.file_path || "";
  const label = document.createElement("label");
  label.className = "source-row source-row-db";
  label.dataset.packageFile = filePath;
  label.dataset.packageId = dbRef.package_id;
  label.dataset.sourceType = "db";
  label.innerHTML = `
    <input type="radio" name="keep-source" value="${escapeAttribute(value)}" ${checked} />
    <div class="source-main">
      <strong>
        ${escapeHtml(dbRef.package_id)}
        <span class="source-tag source-tag-db">DB</span>
      </strong>
      <span>${escapeHtml(dbRef.internal_path)}</span>
      <span class="source-file-path">${escapeHtml(filePath)}</span>
    </div>
  `;
  const radio = label.querySelector('input[name="keep-source"]');
  if (radio) {
    attachKeepRadioToggle(radio, group.key);
    radio.addEventListener("change", () => {
      state.keepMap[group.key] = value;
      bumpKeepMutation();
      // Eagerly fetch every resource of the picked DB package so the
      // Apply-to-filtered button (gated on resourcesByCrc.size) can light
      // up and relocate matching groups. No-op if already cached.
      ensureDbPackageResources(dbRef.package_id);
      renderGroups();
      renderDetail();
    });
  }
  label.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    const items = [];
    if (dbRef.package_id) {
      items.push({
        label: t("resourceListContextOpenPackage"),
        action: () => openSourceRowInVarDetails(dbRef.package_id, filePath, "db"),
      });
    }
    if (filePath) {
      items.push({
        label: t("menuShowInExplorer"),
        action: () => showPackageInExplorer(filePath),
      });
    }
    if (items.length) {
      showContextMenu(event.clientX, event.clientY, items);
    }
  });
  container.appendChild(label);
}

function renderGroupPreview(refs, keepValue) {
  const previewRef = getPreviewRefForGroup(refs, keepValue);
  const slot = $("detail-preview");
  if (!slot) {
    return;
  }

  if (!previewRef) {
    slot.classList.add("hidden");
    slot.innerHTML = "";
    return;
  }

  const packageFile = state.scan?.package_files?.[previewRef.package_id] ?? "";
  if (!packageFile) {
    slot.classList.add("hidden");
    slot.innerHTML = "";
    return;
  }

  slot.classList.remove("hidden");
  slot.className = "vam-preview detail-preview";
  slot.innerHTML = buildPreviewMarkup(previewRef, packageFile);
  bindPreviewResolution(slot);
  bindPreviewActions(slot);
  if (!state.previewCache[getPreviewCacheKey(previewRef.package_id, previewRef.internal_path)]) {
    void ensurePreviewLoaded(previewRef, packageFile);
  }
}

async function ensurePreviewLoaded(ref, packageFile) {
  if (!invoke || !packageFile || !isPreviewablePath(ref.internal_path)) {
    return;
  }

  const key = getPreviewCacheKey(ref.package_id, ref.internal_path);
  const current = state.previewCache[key];
  if (current?.status === "loading" || current?.status === "ready") {
    return;
  }

  state.previewCache[key] = { status: "loading", startedAt: Date.now() };
  const previewSlot = $("detail-preview");
  if (previewSlot && !previewSlot.classList.contains("hidden")) {
    previewSlot.innerHTML = buildPreviewMarkup(ref, packageFile);
  }
  const loadingTicker = setInterval(() => {
    const latest = state.previewCache[key];
    if (!latest || latest.status !== "loading") {
      clearInterval(loadingTicker);
      return;
    }
    const activeGroup = getSelectedGroup();
    if (!activeGroup) {
      clearInterval(loadingTicker);
      return;
    }
    renderGroupPreview(
      [...activeGroup.refs].sort((a, b) => {
        const targetPackageId = getActiveTargetPackageId();
        const aRank = a.package_id === targetPackageId ? 0 : 1;
        const bRank = b.package_id === targetPackageId ? 0 : 1;
        return aRank - bRank || a.package_id.localeCompare(b.package_id) || a.internal_path.localeCompare(b.internal_path);
      }),
      getKeepValue(activeGroup)
    );
  }, 1000);

  try {
    const data = await Promise.race([
      invoke("get_vam_preview", {
        packageId: ref.package_id,
        packagePath: packageFile,
        vamPath: ref.internal_path,
      }),
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error("Preview request timed out")), 10000)
      ),
    ]);
    state.previewCache[key] = { status: "ready", data, finishedAt: Date.now() };
  } catch (error) {
    const message = String(error);
    state.previewCache[key] = { status: "error", error: message, finishedAt: Date.now() };
    addLog(`Preview failed: ${message}`);
  }
  clearInterval(loadingTicker);

  const currentGroup = getSelectedGroup();
  if (!currentGroup) {
    return;
  }
  renderGroupPreview(
    [...currentGroup.refs].sort((a, b) => {
      const targetPackageId = getActiveTargetPackageId();
      const aRank = a.package_id === targetPackageId ? 0 : 1;
      const bRank = b.package_id === targetPackageId ? 0 : 1;
      return aRank - bRank || a.package_id.localeCompare(b.package_id) || a.internal_path.localeCompare(b.internal_path);
    }),
    getKeepValue(currentGroup)
  );
}

function renderLogs() {
  const logList = $("log-list");
  if (!state.logs.length) {
    logList.innerHTML = `<div class="detail-empty">${escapeHtml(t("logEmpty"))}</div>`;
    return;
  }
  logList.innerHTML = state.logs.map((entry) => `<div class="log-item">[${escapeHtml(entry.time)}] ${escapeHtml(entry.message)}</div>`).join("");
}

function stopPollingTask() {
  if (state.pollTimer) {
    clearInterval(state.pollTimer);
    state.pollTimer = null;
  }
}

async function clearActiveTask() {
  if (!state.activeTask) {
    return;
  }
  const id = state.activeTask.id;
  state.activeTask = null;
  stopPollingTask();
  setButtonsBusy(false);
  renderSummary();
  if (invoke) {
    try {
      await invoke("clear_task", { taskId: id });
    } catch (_error) {
    }
  }
}

async function handleTaskCompletion(task, payload) {
  const kind = task?.kind;
  if (payload.error) {
    addLog(payload.error);
  } else if (task?.page === "db-find" && kind === "scan" && payload.scan_result) {
    await dbfHandleScanCompletion(payload);
    await clearActiveTask();
    return;
  } else if (kind === "scan" && payload.scan_result) {
    state.scan = payload.scan_result;
    buildGroupHaystacks(state.scan?.groups);
    state.groupPage = 0;
    state.defaultKeepMap = { ...payload.scan_result.default_keep_map };
    state.keepMap = { ...payload.scan_result.default_keep_map };
    bumpKeepMutation();
    state.targetPackageId = getActiveTargetPackageId();
    // Pick the first *visible* group as the initial selection. In Everything
    // mode `state.scan.groups[0]` could be a bundle member that the UI hides,
    // leaving the detail panel empty until the user clicks something.
    const firstVisible = getScopedGroups()[0] ?? null;
    state.selectedKey = firstVisible?.key ?? null;
    state.selectedKeys = state.selectedKey ? [state.selectedKey] : [];
    addLog(t("scanSuccess", state.scan.summary.packages, state.scan.summary.duplicate_groups));
    const dbInfo = state.scan.info;
    if (dbInfo && dbInfo.packages_persisted > 0) {
      addLog(
        dbInfo.packages_pruned > 0
          ? t(
              "dbSavedWithPrune",
              dbInfo.packages_persisted,
              dbInfo.resources_persisted,
              dbInfo.packages_pruned
            )
          : t("dbSaved", dbInfo.packages_persisted, dbInfo.resources_persisted)
      );
    }
    if (dbInfo && (dbInfo.packages_persisted ?? 0) > 0) {
      writeBuildDbLastIndexed(Date.now());
    }
    refreshBuildDbStats();
    for (const warning of payload.scan_result.warnings ?? []) {
      addLog(t("scanWarning", warning));
    }
    if (!state.scan.groups.length) {
      addLog(t("scanNone"));
    }
  } else if (kind === "execute" && payload.execute_result) {
    const { stats, report_path: reportPath } = payload.execute_result;
    addLog(t("runSuccess", stats.changed_packages, stats.removed_files, stats.vap_files_rewritten ?? 0, reportPath));
    hideProgress();
    renderSummary();
    renderGroups();
    renderDetail();
    await clearActiveTask();
    showDedupComplete(stats, reportPath);
    return;
  } else if (kind === "bulk_import" && payload.bulk_import_result) {
    const r = payload.bulk_import_result;
    addLog(
      t(
        "bulkImportSuccess",
        Number(r.lines_parsed ?? 0).toLocaleString(),
        Number(r.packages_inserted ?? 0).toLocaleString(),
        Number(r.resources_inserted ?? 0).toLocaleString(),
        Number(r.resources_skipped_existing ?? 0).toLocaleString()
      )
    );
    if ((r.lines_skipped ?? 0) > 0) {
      addLog(t("bulkImportLinesSkipped", Number(r.lines_skipped).toLocaleString()));
    }
    if ((r.resources_inserted ?? 0) > 0) {
      writeBuildDbLastIndexed(Date.now());
    }
    hideProgress();
    setBulkImportSelected("");
    refreshBuildDbStats();
    await clearActiveTask();
    return;
  }

  hideProgress();
  renderSummary();
  renderGroups();
  renderDetail();
  await clearActiveTask();
}

function startPollingTask() {
  stopPollingTask();
  state.pollTimer = setInterval(async () => {
    if (!state.activeTask || !invoke) {
      stopPollingTask();
      return;
    }
    const task = { ...state.activeTask };
    try {
      const payload = await invoke("get_task_progress", { taskId: task.id });
      if (!state.activeTask || state.activeTask.id !== task.id) {
        return;
      }
      showProgress(task.kind, payload.progress, payload.message);
      if (payload.done) {
        await handleTaskCompletion(task, payload);
      }
    } catch (error) {
      addLog(String(error));
      hideProgress();
      await clearActiveTask();
    }
  }, TASK_POLL_MS);
}

async function startScan() {
  const inputDir = $("input-dir").value.trim();
  const outputDir = $("output-dir").value.trim();
  if (!inputDir || !outputDir) {
    addLog(t("missingPath"));
    return;
  }
  const targetVarPath = $("target-var-path").value.trim();
  if (!targetVarPath) {
    addLog(t("targetVarRequired"));
    return;
  }
  if (!invoke) {
    throw new Error("Tauri runtime is unavailable.");
  }

  state.targetVarPath = targetVarPath;
  state.targetPackageId = getActiveTargetPackageId();
  const handle = await invoke("start_scan_task", {
    request: {
      input_dir: inputDir,
      additional_input_dirs: getScanAdditionalDirs(),
      target_var_path: targetVarPath,
    },
  });
  state.activeTask = { kind: "scan", id: handle.id };
  resetScanState();
  renderModeControls();
  renderSummary();
  renderGroups();
  renderDetail();
  showProgress("scan", 0, "");
  addLog(t("scanStarted"));
  startPollingTask();
}

// Find-Duplicates per-target augmentation now lives at
// dbfAugmentScanWithAllTargetResources.

async function startBuildDbScan() {
  const inputDir = vamAddonPackagesDir();
  if (!inputDir) {
    showToast("Set your VaM directory in Settings first.", "error");
    openVamDirSettings();
    return;
  }
  if (!invoke) {
    throw new Error("Tauri runtime is unavailable.");
  }

  state.targetVarPath = "";
  state.targetPackageId = null;
  // AddonPackages plus the extra folders from Settings (and any legacy
  // Overview list); the backend dedupes overlapping roots.
  const extra = [...new Set([...getScanAdditionalDirs(), ...getAdditionalDirs("downloadVars")])];
  const handle = await invoke("start_scan_task", {
    request: {
      input_dir: inputDir,
      additional_input_dirs: extra,
      target_var_path: null,
      index_only: true,
    },
  });
  state.activeTask = { kind: "scan", id: handle.id };
  resetScanState();
  renderModeControls();
  renderSummary();
  renderGroups();
  renderDetail();
  showProgress("scan", 0, "");
  addLog(t("scanStarted"));
  startPollingTask();
}

// Backfill resource sizes by harvesting CRC->size from a folder of .var files
// and patching DB rows where size=0 (typically manifest-imported). Runs as an
// independent background task so it doesn't interfere with Overview/scan
// flows. Polls progress locally; Cancel button toggles cancellation server-
// side via cancel_task.
async function startBuildDbBackfill() {
  if (!invoke) return;
  if (state.buildDbBackfill?.running) return;
  const input = $("build-db-backfill-input");
  const inputDir = String(input?.value ?? "").trim();
  if (!inputDir) {
    addLog("Backfill: pick a VAR folder first.");
    return;
  }

  // UI: show progress, lock Start, unlock Cancel.
  const progressEl = $("build-db-backfill-progress");
  const fillEl = $("build-db-backfill-progress-fill");
  const pctEl = $("build-db-backfill-progress-pct");
  const statusEl = $("build-db-backfill-progress-status");
  const resultEl = $("build-db-backfill-result");
  const startBtn = $("build-db-backfill-start");
  const cancelBtn = $("build-db-backfill-cancel");
  if (progressEl) progressEl.classList.remove("hidden");
  if (resultEl) {
    resultEl.classList.add("hidden");
    resultEl.textContent = "";
  }
  if (fillEl) fillEl.style.width = "0%";
  if (pctEl) pctEl.textContent = "0%";
  if (statusEl) statusEl.textContent = "Starting…";
  if (startBtn) startBtn.disabled = true;
  if (cancelBtn) cancelBtn.disabled = false;

  state.buildDbBackfill.running = true;
  state.buildDbBackfill.cancelRequested = false;
  state.buildDbBackfill.taskId = null;

  let taskId = null;
  try {
    const handle = await invoke("start_backfill_sizes_task", {
      request: { input_dir: inputDir, include_vap: false },
    });
    taskId = handle?.id ?? null;
    state.buildDbBackfill.taskId = taskId;
  } catch (error) {
    finalizeBackfillUi(null, String(error));
    return;
  }
  if (!taskId) {
    finalizeBackfillUi(null, "No task id returned.");
    return;
  }

  // Poll loop. Local — does not touch state.activeTask.
  let payload = null;
  while (true) {
    await new Promise((r) => setTimeout(r, 400));
    try {
      payload = await invoke("get_task_progress", { taskId });
    } catch (error) {
      finalizeBackfillUi(null, String(error));
      return;
    }
    if (!payload) break;
    const fraction = Number(payload.progress ?? 0);
    const pct = Math.max(0, Math.min(100, Math.round(fraction * 100)));
    if (fillEl) fillEl.style.width = `${pct}%`;
    if (pctEl) pctEl.textContent = `${pct}%`;
    if (statusEl) {
      statusEl.textContent = state.buildDbBackfill.cancelRequested
        ? "Cancelling…"
        : String(payload.message ?? "Working…");
    }
    if (payload.error) {
      finalizeBackfillUi(null, String(payload.error));
      return;
    }
    if (payload.done) break;
  }

  finalizeBackfillUi(payload?.backfill_result ?? null, null);
  if (taskId && invoke) {
    try {
      await invoke("clear_task", { taskId });
    } catch (_error) {}
  }
}

function finalizeBackfillUi(result, errorMessage) {
  state.buildDbBackfill.running = false;
  state.buildDbBackfill.taskId = null;
  state.buildDbBackfill.cancelRequested = false;
  const startBtn = $("build-db-backfill-start");
  const cancelBtn = $("build-db-backfill-cancel");
  const statusEl = $("build-db-backfill-progress-status");
  const resultEl = $("build-db-backfill-result");
  if (startBtn) startBtn.disabled = false;
  if (cancelBtn) cancelBtn.disabled = true;
  if (errorMessage) {
    if (statusEl) statusEl.textContent = "Failed";
    if (resultEl) {
      resultEl.classList.remove("hidden");
      resultEl.textContent = `Backfill failed: ${errorMessage}`;
    }
    addLog(`Backfill failed: ${errorMessage}`);
    return;
  }
  if (!result) {
    if (statusEl) statusEl.textContent = "Idle";
    return;
  }
  const filesScanned = Number(result.files_scanned ?? 0);
  const rowsUpdated = Number(result.rows_updated ?? 0);
  const crcs = Number(result.crcs_collected ?? 0);
  const cancelled = Boolean(result.was_cancelled);
  if (statusEl) statusEl.textContent = cancelled ? "Cancelled" : "Complete";
  if (resultEl) {
    resultEl.classList.remove("hidden");
    const prefix = cancelled ? "Cancelled — " : "";
    resultEl.textContent = `${prefix}Patched ${rowsUpdated.toLocaleString()} row${rowsUpdated === 1 ? "" : "s"} from ${filesScanned.toLocaleString()} .var file${filesScanned === 1 ? "" : "s"} (${crcs.toLocaleString()} unique CRC${crcs === 1 ? "" : "s"} harvested).`;
  }
  addLog(
    `Backfill ${cancelled ? "cancelled" : "complete"}: ${rowsUpdated} rows updated across ${filesScanned} files.`
  );
}

const BUILD_DB_LAST_INDEXED_KEY = "vam_var_deduper.build_db.last_indexed_at";

function readBuildDbLastIndexed() {
  try {
    const raw = window.localStorage?.getItem(BUILD_DB_LAST_INDEXED_KEY);
    if (!raw) return 0;
    const n = Number(raw);
    return Number.isFinite(n) ? n : 0;
  } catch {
    return 0;
  }
}

function writeBuildDbLastIndexed(timestampMs) {
  try {
    window.localStorage?.setItem(BUILD_DB_LAST_INDEXED_KEY, String(timestampMs));
  } catch {
    /* ignore */
  }
}

function formatBuildDbRelative(timestampMs) {
  if (!timestampMs) return t("buildDbLastNever");
  const deltaMs = Date.now() - timestampMs;
  if (deltaMs < 10_000) return t("buildDbLastJustNow");
  const sec = Math.round(deltaMs / 1000);
  if (sec < 60) return t("buildDbLastSecondsAgo", sec);
  const min = Math.round(sec / 60);
  if (min < 60) return t("buildDbLastMinutesAgo", min);
  const hr = Math.round(min / 60);
  if (hr < 24) return t("buildDbLastHoursAgo", hr);
  const days = Math.round(hr / 24);
  return t("buildDbLastDaysAgo", days);
}

function setBuildDbStatusPill(state) {
  const pill = $("build-db-status-pill");
  const label = $("build-db-status-label");
  if (!pill || !label) return;
  pill.classList.remove("is-success");
  if (state === "ready") {
    pill.classList.add("is-success");
    label.textContent = t("buildDbStatusReady");
  } else {
    label.textContent = t("buildDbStatusEmpty");
  }
}

async function refreshBuildDbStats() {
  if (!$("build-db-stat-packages-value")) return;
  if (!invoke) return;
  try {
    const stats = await invoke("get_database_stats");
    const packages = Number(stats?.package_count ?? 0);
    const resources = Number(stats?.resource_count ?? 0);
    const sizeBytes = Number(stats?.db_size_bytes ?? 0);
    $("build-db-stat-packages-value").textContent = packages.toLocaleString();
    $("build-db-stat-resources-value").textContent = resources.toLocaleString();
    $("build-db-stat-size-value").textContent = formatBytesLocal(sizeBytes);
    $("build-db-stat-last-value").textContent = formatBuildDbRelative(
      readBuildDbLastIndexed()
    );
    setBuildDbStatusPill(packages > 0 ? "ready" : "empty");
  } catch (_e) {
    /* leave placeholders if stats call fails */
  }
}

function setBulkImportSelected(path) {
  state.bulkImportPath = path || "";
  const selectedEl = $("build-db-bulk-selected");
  const pathEl = $("build-db-bulk-selected-path");
  const startBtn = $("build-db-bulk-start-button");
  const cancelBtn = $("build-db-bulk-cancel-button");
  if (path) {
    if (pathEl) pathEl.textContent = path;
    if (selectedEl) selectedEl.classList.remove("hidden");
    if (startBtn) startBtn.disabled = false;
    if (cancelBtn) cancelBtn.disabled = false;
  } else {
    if (pathEl) pathEl.textContent = "";
    if (selectedEl) selectedEl.classList.add("hidden");
    if (startBtn) startBtn.disabled = true;
    if (cancelBtn) cancelBtn.disabled = true;
  }
}

async function pickBulkImportFile() {
  if (!invoke) return;
  try {
    const path = await invoke("pick_manifest_file");
    if (path) setBulkImportSelected(path);
  } catch (error) {
    addLog(String(error));
  }
}

async function startBulkImport() {
  const path = state.bulkImportPath?.trim();
  if (!path) {
    addLog(t("bulkImportNoFile"));
    return;
  }
  if (!invoke) {
    throw new Error("Tauri runtime is unavailable.");
  }

  const handle = await invoke("start_bulk_import_task", {
    request: { manifest_path: path },
  });
  state.activeTask = { kind: "bulk_import", id: handle.id };
  showProgress("bulk_import", 0, t("bulkImportProgressLines", 0, 0));
  addLog(t("bulkImportStarted"));
  startPollingTask();
}

async function startExecute() {
  if (!state.scan) {
    addLog(t("scanFirst"));
    return;
  }
  if (!invoke) {
    throw new Error("Tauri runtime is unavailable.");
  }

  const targetVarPath = $("target-var-path").value.trim();
  const targetPackageId = getActiveTargetPackageId();
  const request = {
    input_dir: $("input-dir").value.trim(),
    additional_input_dirs: getScanAdditionalDirs(),
    output_dir: $("output-dir").value.trim(),
    vap_dir: state.processVap ? $("vap-dir").value.trim() || null : null,
    target_var_path: targetVarPath || null,
    keep_map: buildEffectiveKeepMap(),
    target_package_id: targetPackageId,
    replace: $("replace-in-place").checked,
    backup: $("backup-changed").checked,
  };

  const handle = await invoke("start_execute_task", { request });
  state.activeTask = { kind: "execute", id: handle.id };
  renderSummary();
  showProgress("execute", 0, "");
  addLog(t("runStarted"));
  startPollingTask();
}

async function showPackageInExplorer(packageFile) {
  if (!packageFile) {
    throw new Error("Package file path is unavailable.");
  }
  if (!invoke) {
    throw new Error("Tauri runtime is unavailable.");
  }
  await invoke("show_in_explorer", { path: packageFile });
}

async function showOutputInExplorer() {
  if (!invoke) {
    throw new Error("Tauri runtime is unavailable.");
  }
  // Active workspace is Clean VARs (dbf-*); the Overview controls were removed.
  const replace = !!($("dbf-replace-in-place") || $("replace-in-place"))?.checked;
  const outEl = $("dbf-output-dir") || $("output-dir");
  const path = replace ? state.targetVarPath : (outEl?.value || "").trim();
  if (!path) {
    throw new Error("Path is unavailable.");
  }
  await invoke("show_in_explorer", { path });
}

async function exportCurrentResource(group) {
  const ref = getActiveExportRef(group);
  if (!ref) {
    throw new Error("No resource available to export.");
  }
  if (!invoke) {
    throw new Error("Tauri runtime is unavailable.");
  }
  const packagePath = state.scan?.package_files?.[ref.package_id];
  if (!packagePath) {
    throw new Error("Package file path is unavailable.");
  }

  if (isBundleExportPath(ref.internal_path)) {
    const suggestedFolderName = getPathStem(ref.internal_path);
    const outputDir = await invoke("pick_folder");
    if (!outputDir) return;
    const count = await invoke("export_vam_bundle", {
      packagePath,
      internalPath: ref.internal_path,
      outputDir,
    });
    addLog(`Exported ${count} related files to: ${joinDisplayPath(outputDir, suggestedFolderName)}`);
  } else {
    const fileName = ref.internal_path.split("/").pop() || "resource.bin";
    const outputPath = await invoke("pick_save_file", { fileName });
    if (!outputPath) return;
    await invoke("export_var_resource", {
      packagePath,
      internalPath: ref.internal_path,
      outputPath,
    });
    addLog(`Exported resource: ${ref.internal_path}`);
  }
}

async function chooseVarFile() {
  if (!invoke) {
    return;
  }
  const selected = await invoke("pick_var_file");
  if (!selected) {
    return;
  }
  $("target-var-path").value = selected;
  handleTargetVarChange(selected);
}

function escapeHtml(text) {
  return String(text)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function escapeAttribute(text) {
  return escapeHtml(text).replaceAll("'", "&#39;");
}

function deriveOutputDir(inputDir) {
  const value = String(inputDir ?? "").trim();
  return value ? `${value}_deduped` : "";
}

function syncOutputDirFromInput(force = false) {
  const inputDir = $("input-dir").value.trim();
  const outputDir = $("output-dir").value.trim();
  if (force || !outputDir || state.outputDirAutoSynced) {
    $("output-dir").value = deriveOutputDir(inputDir);
    state.outputDirAutoSynced = true;
    setButtonsBusy(Boolean(state.activeTask));
  }
}

async function initConfig() {
  if (!invoke) {
    updateStaticCopy();
    addLog("Tauri runtime is unavailable in this view.");
    return;
  }
  const config = await invoke("load_config");
  if (config?.language) {
    state.language = config.language;
  }
  if (typeof config?.theme === "string" && THEMES.includes(config.theme)) {
    state.theme = config.theme;
  }
  applyConfigToInputs(config);
  // Same for the mode toggle: persist into the per-page slot so the first
  // restorePage("db-find") sees the saved mode, not the fresh-slot default.
  if (config?.dbf_mode === "local" || config?.dbf_mode === "db") {
    state.pageStates["db-find"] = state.pageStates["db-find"] ?? makeFreshPageState();
    state.pageStates["db-find"].dbfMode = config.dbf_mode;
  }
  updateStaticCopy();
}

function applyConfigToInputs(config) {
  if (!config) return;
  state.vamDir =
    typeof config.vam_dir === "string" && config.vam_dir.trim()
      ? config.vam_dir.trim()
      : vamDirFromLegacyConfig(config);
  const setVal = (id, val) => {
    const el = $(id);
    if (el && typeof val === "string") el.value = val;
  };
  setVal("input-dir", config.input_dir ?? "");
  setAdditionalDirs("overview", config.scan_additional_dirs);
  setAdditionalDirs("varDetails", config.var_details_additional_dirs);
  setVal("var-packages-input-dir", config.var_packages_input_dir ?? "");
  setAdditionalDirs("varPackages", config.var_packages_additional_dirs);
  setAdditionalDirs("dbf", config.dbf_additional_dirs);
  setAdditionalDirs("downloadVars", config.download_vars_additional_dirs);
  setAdditionalDirs("internalize", config.internalize_additional_dirs);
  setVal("output-dir", config.output_dir ?? "");
  setVal("vap-dir", config.vap_dir ?? "");
  if (typeof config.replace_in_place === "boolean") {
    const el = $("replace-in-place");
    if (el) el.checked = config.replace_in_place;
  }
  if (typeof config.backup_changed === "boolean") {
    const el = $("backup-changed");
    if (el) el.checked = config.backup_changed;
  }
  if (typeof config.process_vap === "boolean") {
    const el = $("process-vap");
    if (el) el.checked = config.process_vap;
    state.processVap = config.process_vap;
  }
  const outDirEl = $("output-dir");
  state.outputDirAutoSynced =
    !outDirEl ||
    !outDirEl.value.trim() ||
    outDirEl.value.trim() === deriveOutputDir($("input-dir")?.value ?? "");
  syncReplaceOptions();

  // Find Duplicates page paths are persisted independently. Target VAR path
  // is intentionally not restored — the user re-picks it each session.
  setVal("dbf-input-dir", config.dbf_input_dir ?? "");
  setVal("dbf-output-dir", config.dbf_output_dir ?? "");
  setVal("dbf-vap-dir", config.dbf_vap_dir ?? "");
  // Find Duplicates mode toggle.
  if (config.dbf_mode === "local" || config.dbf_mode === "db") {
    state.dbfMode = config.dbf_mode;
  }
  setRadioValue("dbf-mode", state.dbfMode);

  // VAR library folder (Settings) — the reference set for dependency checks.
  // Still stored as download_vars_folder so existing configs carry over.
  if (typeof config.download_vars_folder === "string") {
    const libInput = $("settings-library-folder");
    if (libInput && !libInput.value) libInput.value = config.download_vars_folder;
  }
  if (typeof config.download_vars_downloads_folder === "string") {
    const dvDl = $("settings-downloads-folder");
    if (dvDl && !dvDl.value) dvDl.value = config.download_vars_downloads_folder;
  }
  if (typeof config.download_vars_organize_by_creator === "boolean") {
    const el = $("settings-organize-by-creator");
    if (el) el.checked = config.download_vars_organize_by_creator;
  }
  // Collect Dependencies search folders: saved (even as an empty list) only
  // while its "Remember these folders" box is ticked. Not remembered leaves this
  // session's folders alone — a Settings reset must not wipe them.
  if (Array.isArray(config.dep_source_dirs)) {
    DEP_COLLECT.dirs = config.dep_source_dirs.filter((d) => typeof d === "string" && d.trim());
    DEP_COLLECT.remember = true;
  } else {
    DEP_COLLECT.remember = false;
  }
  if (typeof config.var_packages_deep_scan === "boolean") {
    state.varPackagesDeepScan = config.var_packages_deep_scan;
    // Seed the committed depth too: nothing has been scanned yet, so the saved
    // setting is what the first Scan should use — and with no listing on screen
    // there is nothing for a "pending" hint to contradict.
    state.varPackagesScannedDeep = config.var_packages_deep_scan;
    applyVarPackagesDepthUi();
  }

  // Internalize Resources page — own paths persist, but if absent fall back
  // to the global input_dir / output_dir so the user doesn't have to retype.
  if (state.internalize) {
    if (typeof config.internalize_input_dir === "string" && !state.internalize.inputDir) {
      state.internalize.inputDir = config.internalize_input_dir;
    } else if (typeof config.input_dir === "string" && !state.internalize.inputDir) {
      state.internalize.inputDir = config.input_dir;
    }
    if (typeof config.internalize_output_dir === "string" && !state.internalize.outputDir) {
      state.internalize.outputDir = config.internalize_output_dir;
    } else if (typeof config.output_dir === "string" && !state.internalize.outputDir) {
      state.internalize.outputDir = config.output_dir;
    }
    if (typeof config.internalize_replace_in_place === "boolean") {
      state.internalize.replaceInPlace = config.internalize_replace_in_place;
    }
    if (typeof config.internalize_backup === "boolean") {
      state.internalize.backup = config.internalize_backup;
    }
    const intDir = $("internalize-input-dir");
    const intOut = $("internalize-output-dir");
    const intBackup = $("internalize-backup");
    const intReplace = $("internalize-replace-in-place");
    if (intDir && !intDir.value) intDir.value = state.internalize.inputDir;
    if (intOut && !intOut.value) intOut.value = state.internalize.outputDir;
    if (intBackup) intBackup.checked = !!state.internalize.backup;
    if (intReplace) intReplace.checked = !!state.internalize.replaceInPlace;
    if (intBackup && intReplace) intBackup.disabled = !intReplace.checked;
  }

  // Mirror the same prefill onto the Missing Resources page so the user's
  // saved output folder applies everywhere.
  if (state.missingResources) {
    if (typeof config.input_dir === "string" && !state.missingResources.inputDir) {
      state.missingResources.inputDir = config.input_dir;
    }
    if (typeof config.output_dir === "string" && !state.missingResources.outputDir) {
      state.missingResources.outputDir = config.output_dir;
    }
    if (typeof config.replace_in_place === "boolean") {
      state.missingResources.replaceInPlace = config.replace_in_place;
    }
    if (typeof config.backup_changed === "boolean") {
      state.missingResources.backup = config.backup_changed;
    }
    const mrDir = $("missing-input-dir");
    const mrOut = $("missing-output-dir");
    const mrBackup = $("missing-backup");
    const mrReplace = $("missing-replace-in-place");
    if (mrDir && !mrDir.value) mrDir.value = state.missingResources.inputDir;
    if (mrOut && !mrOut.value) mrOut.value = state.missingResources.outputDir;
    if (mrBackup) mrBackup.checked = !!state.missingResources.backup;
    if (mrReplace) mrReplace.checked = !!state.missingResources.replaceInPlace;
    if (mrBackup && mrReplace) mrBackup.disabled = !mrReplace.checked;
  }

  // Last: every page's VAR folder is AddonPackages under the VaM directory,
  // whatever per-page folder an older config remembered.
  applyVamDir();
  refreshVamDirInfo();
}

function buildCurrentConfig() {
  return {
    language: state.language,
    theme: state.theme,
    vam_dir: vamDir() || null,
    input_dir: ($("input-dir")?.value || "").trim() || null,
    scan_additional_dirs: getScanAdditionalDirs().length ? getScanAdditionalDirs() : null,
    var_details_additional_dirs: getAdditionalDirs("varDetails").length
      ? getAdditionalDirs("varDetails")
      : null,
    var_packages_input_dir: ($("var-packages-input-dir")?.value || "").trim() || null,
    var_packages_additional_dirs: getAdditionalDirs("varPackages").length
      ? getAdditionalDirs("varPackages")
      : null,
    // State-backed rather than DOM-backed: the control is a mode-switch, so
    // there is no checkbox to read the value off.
    var_packages_deep_scan: state.varPackagesDeepScan !== false,
    output_dir: ($("output-dir")?.value || "").trim() || null,
    vap_dir: ($("vap-dir")?.value || "").trim() || null,
    replace_in_place: !!$("replace-in-place")?.checked,
    backup_changed: !!$("backup-changed")?.checked,
    process_vap: !!$("process-vap")?.checked,
    dbf_input_dir: ($("dbf-input-dir")?.value || "").trim() || null,
    dbf_additional_dirs: getAdditionalDirs("dbf").length ? getAdditionalDirs("dbf") : null,
    dbf_output_dir: ($("dbf-output-dir")?.value || "").trim() || null,
    dbf_target_var_path: null,
    dbf_vap_dir: ($("dbf-vap-dir")?.value || "").trim() || null,
    dbf_mode: state.dbfMode === "local" ? "local" : "db",
    download_vars_folder: ($("settings-library-folder")?.value || "").trim() || null,
    download_vars_additional_dirs: getAdditionalDirs("downloadVars").length
      ? getAdditionalDirs("downloadVars")
      : null,
    download_vars_downloads_folder:
      ($("settings-downloads-folder")?.value || "").trim() || null,
    download_vars_organize_by_creator: !!$("settings-organize-by-creator")?.checked,
    internalize_input_dir:
      ((($("internalize-input-dir")?.value || "").trim()) ||
        state.internalize?.inputDir ||
        "").trim() || null,
    internalize_additional_dirs: getAdditionalDirs("internalize").length
      ? getAdditionalDirs("internalize")
      : null,
    internalize_output_dir:
      ((($("internalize-output-dir")?.value || "").trim()) ||
        state.internalize?.outputDir ||
        "").trim() || null,
    internalize_replace_in_place: !!(
      ($("internalize-replace-in-place")?.checked) ?? state.internalize?.replaceInPlace
    ),
    internalize_backup:
      $("internalize-backup")?.checked !== undefined
        ? !!$("internalize-backup").checked
        : (state.internalize?.backup ?? true),
    dep_source_dirs: DEP_COLLECT.remember ? [...DEP_COLLECT.dirs] : null,
  };
}

async function saveAllSettings() {
  if (!invoke) throw new Error("Tauri runtime unavailable");
  await invoke("save_config", { config: buildCurrentConfig() });
}

async function resetSettingsFromDisk() {
  if (!invoke) throw new Error("Tauri runtime unavailable");
  const config = await invoke("load_config");
  if (config?.language) state.language = config.language;
  if (typeof config?.theme === "string" && THEMES.includes(config.theme)) {
    state.theme = config.theme;
  }
  applyConfigToInputs(config);
  updateStaticCopy();
  syncSettingsControlState();
  setButtonsBusy(Boolean(state.activeTask));
}

async function chooseFolder(targetId) {
  if (!invoke) {
    return;
  }
  const selected = await invoke("pick_folder");
  if (!selected) {
    return;
  }
  $(targetId).value = selected;
  $(targetId).dispatchEvent(new Event("input", { bubbles: true }));
  if (targetId === "input-dir") {
    $("output-dir").dispatchEvent(new Event("input", { bubbles: true }));
    await autoSelectVapFolder();
    $("vap-dir").dispatchEvent(new Event("input", { bubbles: true }));
  } else if (targetId === "output-dir") {
    state.outputDirAutoSynced = false;
    setButtonsBusy(Boolean(state.activeTask));
  }
}

// =====================================================================
// VaM directory — set once in Settings, as in VaM Backstage.
//
// `<VaM>\AddonPackages` (the folder VaM itself loads) is the VAR folder for
// every page. The pages still read their old per-page folder fields, which
// are now hidden inputs kept equal to AddonPackages by applyVamDir(); what
// the user sees is a read-only [data-vam-source] line with a link to
// Settings. Each page's own "Add folder" list still adds extra scan folders.
// =====================================================================

const VAM_FOLDER_FIELDS = [
  "dbf-input-dir",
  "build-db-input-dir",
  "build-db-backfill-input",
  "var-packages-input-dir",
  "var-details-folder-input",
  "reclaim-folder-input",
  "ur-folder-input",
  "missing-input-dir",
  "internalize-input-dir",
  "settings-input-dir",
  "settings-library-folder",
];

function vamDir() {
  return String(state.vamDir ?? "").trim();
}

function vamJoin(dir, name) {
  const base = String(dir).replace(/[\\/]+$/, "");
  const sep = base.includes("/") && !base.includes("\\") ? "/" : "\\";
  return `${base}${sep}${name}`;
}

function vamAddonPackagesDir() {
  const dir = vamDir();
  return dir ? vamJoin(dir, "AddonPackages") : "";
}

// Older configs only know per-page VAR folders. Any of them that IS an
// AddonPackages folder names the VaM directory (its parent).
function vamDirFromLegacyConfig(config) {
  const candidates = [
    config?.download_vars_folder,
    config?.var_packages_input_dir,
    config?.dbf_input_dir,
    config?.input_dir,
    config?.internalize_input_dir,
  ];
  for (const raw of candidates) {
    const path = String(raw ?? "").trim().replace(/[\\/]+$/, "");
    const parts = path.split(/[\\/]/);
    if (parts.length > 1 && parts[parts.length - 1].toLowerCase() === "addonpackages") {
      return path.slice(0, path.length - parts[parts.length - 1].length).replace(/[\\/]+$/, "");
    }
  }
  return "";
}

// Points every page at AddonPackages and repaints the read-only lines.
function applyVamDir() {
  const addon = vamAddonPackagesDir();
  for (const id of VAM_FOLDER_FIELDS) {
    const el = $(id);
    if (el) el.value = addon;
  }
  if (state.internalize) state.internalize.inputDir = addon;
  if (state.missingResources) state.missingResources.inputDir = addon;
  if (state.varDetails) state.varDetails.inputDir = addon;
  renderVamSources();
  renderSettingsVamDir();
  libRenderStatusBar();
}

// Each [data-vam-source] line describes the hidden folder field right after it,
// so it always shows the folder that page will actually scan.
function renderVamSources() {
  const addon = vamAddonPackagesDir();
  document.querySelectorAll("[data-vam-source]").forEach((row) => {
    const field = row.nextElementSibling?.matches?.('input[type="hidden"]') ? row.nextElementSibling : null;
    const value = String(field?.value ?? addon).trim();
    if (!value) {
      row.classList.add("is-missing");
      row.innerHTML = `
        <span class="material-symbols-outlined">warning</span>
        <span class="vam-source-path">VaM directory not set</span>
        <button type="button" class="vam-source-change" data-vam-settings>Set in Settings</button>`;
      return;
    }
    row.classList.remove("is-missing");
    const isAddon = addon && value.toLowerCase() === addon.toLowerCase();
    row.title = value;
    row.innerHTML = `
      <span class="material-symbols-outlined">hard_drive</span>
      <span class="vam-source-name">${isAddon ? "AddonPackages" : "Folder"}</span>
      <span class="vam-source-path">${escapeHtml(value)}</span>
      <button type="button" class="vam-source-change" data-vam-settings title="Change the VaM directory in Settings">Change</button>`;
  });
}

function renderSettingsVamDir() {
  const text = $("settings-vam-dir-text");
  if (text) {
    const dir = vamDir();
    text.textContent = dir || "Not configured";
    text.classList.toggle("is-empty", !dir);
    text.title = dir;
  }
  const status = $("settings-vam-dir-status");
  if (!status) return;
  const info = state.vamDirInfo;
  if (!vamDir()) {
    status.textContent = "Pick the folder VaM is installed in (the one that contains AddonPackages).";
  } else if (info && info.vam_dir === vamDir()) {
    status.textContent = info.valid
      ? `AddonPackages: ${info.addon_packages} · ${Number(info.var_count).toLocaleString()} .var files`
      : `No AddonPackages folder in ${info.vam_dir}. Pick the folder VaM is installed in.`;
    status.classList.toggle("is-error", !info.valid);
  } else {
    status.textContent = `AddonPackages: ${vamAddonPackagesDir()}`;
  }
}

// Fills in the .var count / validity line for the stored directory.
async function refreshVamDirInfo() {
  if (!invoke || !vamDir()) return;
  try {
    state.vamDirInfo = await invoke("inspect_vam_dir", { path: vamDir() });
  } catch (err) {
    addLog(`VaM directory: ${String(err)}`);
  }
  renderSettingsVamDir();
}

// Browse, as in Backstage: the folder must contain AddonPackages (picking
// AddonPackages itself is accepted and resolved to its parent). Saved at once.
async function pickVamDir() {
  if (!invoke) return;
  const picked = await invoke("pick_folder");
  if (!picked) return;
  const info = await invoke("inspect_vam_dir", { path: picked });
  if (!info?.valid) {
    showToast("That folder has no AddonPackages folder — pick the folder VaM is installed in.", "error", 6000);
    return;
  }
  await setVamDir(info);
  showToast(`VaM directory set. Found ${Number(info.var_count).toLocaleString()} .var files.`, "success", 6000);
}

// Stores a validated directory (an inspect_vam_dir result), points every page
// at its AddonPackages and saves. VAR Packages reloads from the new library.
async function setVamDir(info) {
  const changed = info.vam_dir !== vamDir();
  state.vamDir = info.vam_dir;
  state.vamDirInfo = info;
  applyVamDir();
  await persistAllConfig();
  if (changed) libOnVamDirChanged();
}

// ---- First run ---------------------------------------------------------------
// Like VaM Backstage's welcome step: while no VaM directory is set, the app
// opens on a dialog asking for one. "Skip for now" leaves it unset (pages then
// show "VaM directory not set" with a link to Settings).

const VAM_SETUP = { info: null };

function renderVamSetup() {
  const info = VAM_SETUP.info;
  const path = $("vam-setup-path");
  if (path) {
    path.textContent = info?.valid ? info.vam_dir : "Not selected";
    path.classList.toggle("is-empty", !info?.valid);
  }
  vpSetText("vam-setup-browse", info?.valid ? "Change" : "Select");
  const found = $("vam-setup-found");
  if (found) found.classList.toggle("hidden", !info?.valid);
  vpSetText("vam-setup-found-text", info?.valid ? `${Number(info.var_count).toLocaleString()} var files found` : "");
  const cont = $("vam-setup-continue");
  if (cont) cont.disabled = !info?.valid;
}

function showVamSetup() {
  VAM_SETUP.info = null;
  $("vam-setup-error")?.classList.add("hidden");
  renderVamSetup();
  $("vam-setup-backdrop")?.classList.remove("hidden");
}

function hideVamSetup() {
  $("vam-setup-backdrop")?.classList.add("hidden");
}

async function browseVamSetup() {
  if (!invoke) return;
  const picked = await invoke("pick_folder");
  if (!picked) return;
  const info = await invoke("inspect_vam_dir", { path: picked });
  const error = $("vam-setup-error");
  if (!info?.valid) {
    if (error) {
      error.textContent = "No AddonPackages folder found in that directory.";
      error.classList.remove("hidden");
    }
    return;
  }
  error?.classList.add("hidden");
  VAM_SETUP.info = info;
  renderVamSetup();
}

function openVamDirSettings() {
  document.querySelector('[data-sidebar-link="settings"]')?.click();
  requestAnimationFrame(() => {
    const card = $("settings-vam-card");
    if (!card) return;
    card.scrollIntoView({ block: "center", behavior: "smooth" });
    card.classList.remove("is-flash");
    void card.offsetWidth;
    card.classList.add("is-flash");
  });
}

function setupVamDir() {
  $("settings-pick-vam-dir")?.addEventListener("click", () => {
    pickVamDir().catch((e) => addLog(`VaM directory: ${String(e)}`));
  });
  $("vam-setup-browse")?.addEventListener("click", () => {
    browseVamSetup().catch((e) => addLog(`VaM directory: ${String(e)}`));
  });
  $("vam-setup-skip")?.addEventListener("click", hideVamSetup);
  $("vam-setup-continue")?.addEventListener("click", async () => {
    if (!VAM_SETUP.info?.valid) return;
    hideVamSetup();
    await setVamDir(VAM_SETUP.info).catch((e) => addLog(`VaM directory: ${String(e)}`));
    showToast("VaM directory set — your library is ready.", "success");
  });
  document.addEventListener("click", (event) => {
    if (!event.target.closest?.("[data-vam-settings]")) return;
    event.preventDefault();
    event.stopPropagation();
    openVamDirSettings();
  });
  applyVamDir();
  // Config is loaded before setup runs, so an unset directory here is a first
  // run (or a skipped one).
  if (invoke && !vamDir()) showVamSetup();
}

// =====================================================================
// Additional VAR folders — shared across sections.
// Each section keeps a primary single-folder field plus an optional list of
// extra scan roots (unioned into that section's scan, deduped by the backend).
// One generic registry + helper set drives every section's list UI.
// =====================================================================
async function persistAllConfig() {
  if (!invoke) return;
  await invoke("save_config", { config: buildCurrentConfig() });
}

// onChange: how to persist after a list edit (null = session-only, not saved).
const ADDITIONAL_DIR_SECTIONS = {
  overview: {
    listId: "scan-additional-dirs",
    addBtnId: "scan-add-folder-button",
    labelId: "scan-additional-dirs-label",
    primaryInputId: "input-dir",
    onChange: persistAllConfig,
  },
  dbf: {
    listId: "dbf-additional-dirs",
    addBtnId: "dbf-add-folder-button",
    labelId: "dbf-additional-dirs-label",
    primaryInputId: "dbf-input-dir",
    onChange: persistAllConfig,
  },
  internalize: {
    listId: "internalize-additional-dirs",
    addBtnId: "internalize-add-folder-button",
    labelId: "internalize-additional-dirs-label",
    primaryInputId: "internalize-input-dir",
    onChange: persistAllConfig,
  },
  // VAR library folders (Settings) — the reference set a package's dependencies
  // are checked against to decide what's missing. Key stays `downloadVars` so the
  // AppConfig fields download_vars_folder / download_vars_additional_dirs keep
  // working with no migration; only the DOM now lives in Settings.
  downloadVars: {
    listId: "settings-library-additional-dirs",
    addBtnId: "settings-library-add-folder-button",
    labelId: "settings-library-additional-dirs-label",
    primaryInputId: "settings-library-folder",
    onChange: persistAllConfig,
  },
  // Download Dependencies dialog: the same list as Settings (downloadVars),
  // edited in place; a change re-runs the open scan.
  depScan: {
    listId: "dep-scan-extra-dirs",
    addBtnId: "dep-scan-add-folder",
    primaryInputId: "settings-library-folder",
    stateKey: "downloadVars",
    onChange: async () => {
      await persistAllConfig();
      depRescanAfterFolderChange();
    },
  },
  reclaim: {
    listId: "reclaim-additional-dirs",
    addBtnId: "reclaim-add-folder-button",
    labelId: "reclaim-additional-dirs-label",
    primaryInputId: "reclaim-folder-input",
    onChange: null,
  },
  unique: {
    listId: "ur-additional-dirs",
    addBtnId: "ur-add-folder-button",
    labelId: "ur-additional-dirs-label",
    primaryInputId: "ur-folder-input",
    onChange: null,
  },
  varPackages: {
    listId: "var-packages-additional-dirs",
    addBtnId: "var-packages-add-folder-button",
    labelId: "var-packages-additional-dirs-label",
    primaryInputId: "var-packages-input-dir",
    onChange: persistAllConfig,
  },
  // VAR Details "Scan Local" keeps its own independent additional-folders list,
  // persisted separately as var_details_additional_dirs.
  varDetails: {
    listId: "var-details-additional-dirs",
    addBtnId: "var-details-add-folder-button",
    labelId: "var-details-additional-dirs-label",
    primaryInputId: "var-details-folder-input",
    onChange: persistAllConfig,
  },
};

// Sections may share one underlying list via `stateKey` (e.g. varDetails aliases
// overview). All state reads/writes go through the resolved key.
function additionalDirsKey(sectionId) {
  return ADDITIONAL_DIR_SECTIONS[sectionId]?.stateKey || sectionId;
}

function additionalDirsState(sectionId) {
  const key = additionalDirsKey(sectionId);
  if (!state.additionalDirs[key]) state.additionalDirs[key] = [];
  return state.additionalDirs[key];
}

// Re-render every section's list. Cheap, and keeps aliased lists (sections that
// share a stateKey) in sync after any mutation.
function renderAllAdditionalDirs() {
  for (const sectionId of Object.keys(ADDITIONAL_DIR_SECTIONS)) {
    renderAdditionalDirs(sectionId);
  }
}

// Trimmed, deduped list with any entry equal to the section's primary folder
// dropped (the backend dedups by canonical path anyway; this keeps requests clean).
function getAdditionalDirs(sectionId) {
  const cfg = ADDITIONAL_DIR_SECTIONS[sectionId];
  const primaryEl = cfg?.primaryInputId ? $(cfg.primaryInputId) : null;
  const primary = (primaryEl?.value || "").trim().toLowerCase();
  const seen = new Set();
  const result = [];
  for (const dir of additionalDirsState(sectionId)) {
    const trimmed = String(dir ?? "").trim();
    if (!trimmed) continue;
    const key = trimmed.toLowerCase();
    if ((primary && key === primary) || seen.has(key)) continue;
    seen.add(key);
    result.push(trimmed);
  }
  return result;
}

function renderAdditionalDirs(sectionId) {
  const cfg = ADDITIONAL_DIR_SECTIONS[sectionId];
  if (!cfg) return;
  const container = $(cfg.listId);
  if (!container) return;
  container.innerHTML = "";
  additionalDirsState(sectionId).forEach((dir, index) => {
    const row = document.createElement("div");
    row.className = "additional-dir-row";
    const label = document.createElement("span");
    label.className = "additional-dir-path";
    label.textContent = dir;
    label.title = dir;
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "ghost-button additional-dir-remove";
    remove.textContent = "✕";
    remove.setAttribute("aria-label", t("removeFolder"));
    remove.disabled = Boolean(state.activeTask);
    remove.addEventListener("click", () => {
      removeAdditionalDir(sectionId, index).catch((error) => addLog(String(error)));
    });
    row.append(label, remove);
    container.append(row);
  });
  if (sectionId === "varPackages") libRenderFoldersBadge();
}

async function addAdditionalDir(sectionId) {
  if (!invoke) return;
  const selected = await invoke("pick_folder");
  if (!selected) return;
  const trimmed = String(selected).trim();
  if (!trimmed) return;
  const dirs = additionalDirsState(sectionId);
  if (dirs.some((dir) => String(dir).trim().toLowerCase() === trimmed.toLowerCase())) {
    return;
  }
  dirs.push(trimmed);
  renderAllAdditionalDirs();
  await persistAdditionalDirs(sectionId);
}

async function removeAdditionalDir(sectionId, index) {
  additionalDirsState(sectionId).splice(index, 1);
  renderAllAdditionalDirs();
  await persistAdditionalDirs(sectionId);
}

async function persistAdditionalDirs(sectionId) {
  const cfg = ADDITIONAL_DIR_SECTIONS[sectionId];
  if (!cfg || !cfg.onChange || !invoke) return;
  try {
    await cfg.onChange();
  } catch (error) {
    addLog(String(error));
  }
}

// Replace a section's list (e.g. when loading config) and re-render. Writes to
// the resolved stateKey so aliased sections share the same array.
function setAdditionalDirs(sectionId, dirs) {
  state.additionalDirs[additionalDirsKey(sectionId)] = Array.isArray(dirs)
    ? dirs.filter((dir) => typeof dir === "string" && dir.trim())
    : [];
  renderAllAdditionalDirs();
}

// Back-compat thin wrapper — Overview call sites still use this name.
function getScanAdditionalDirs() {
  return getAdditionalDirs("overview");
}

// Wire every section's Add Folder button + render its initial (empty) list.
function initAdditionalDirSections() {
  for (const [sectionId, cfg] of Object.entries(ADDITIONAL_DIR_SECTIONS)) {
    const btn = $(cfg.addBtnId);
    if (btn) {
      btn.addEventListener("click", async () => {
        try {
          await addAdditionalDir(sectionId);
        } catch (error) {
          addLog(String(error));
        }
      });
    }
    renderAdditionalDirs(sectionId);
  }
}

function findNearbyVapCandidates(inputDir) {
  const value = String(inputDir ?? "").trim();
  if (!value) {
    return [];
  }

  const normalized = value.replaceAll("/", "\\").replace(/[\\]+$/, "");
  const candidates = [`${normalized}\\Custom\\Atom\\Person\\Appearance`];
  const lastSlash = normalized.lastIndexOf("\\");
  if (lastSlash > 1) {
    candidates.push(`${normalized.slice(0, lastSlash)}\\Custom\\Atom\\Person\\Appearance`);
  }
  return [...new Set(candidates)];
}

async function autoSelectVapFolder() {
  if (!invoke) {
    throw new Error("Tauri runtime is unavailable.");
  }

  for (const candidate of findNearbyVapCandidates($("input-dir").value)) {
    const exists = await invoke("path_exists", { path: candidate });
    if (exists) {
      $("vap-dir").value = candidate;
      return;
    }
  }

  addLog("Could not find a nearby Custom\\Atom\\Person\\Appearance folder.");
}

async function persistLanguage() {
  if (!invoke) {
    return;
  }
  await invoke("save_config", { config: buildCurrentConfig() });
}

function syncReplaceOptions() {
  // Every control here lived in the removed Overview workspace. Clean VARs runs
  // its own dbfSyncReplaceOptions; bail when the Overview DOM is absent.
  if (!$("replace-in-place")) return;
  const replace = $("replace-in-place").checked;
  if (!replace) {
    $("backup-changed").checked = false;
  }
  const backup = $("backup-changed").checked;
  const enableOutput = !replace || backup;
  $("output-dir").disabled = !enableOutput || Boolean(state.activeTask);
  $("pick-output-button").disabled = !enableOutput || Boolean(state.activeTask);
  $("open-output-button").disabled = Boolean(state.activeTask) || !(replace ? state.targetVarPath : $("output-dir").value.trim());
  $("backup-changed").disabled = !replace || Boolean(state.activeTask);
  $("vap-dir").disabled = Boolean(state.activeTask) || !state.processVap;
  $("pick-vap-button").disabled = Boolean(state.activeTask) || !state.processVap;
  $("process-vap").disabled = Boolean(state.activeTask);
  $("replace-warning").classList.toggle("hidden", !replace);
  $("output-dir").closest(".path-row")?.classList.toggle("field-disabled", !enableOutput);
  $("vap-dir").closest(".path-row")?.classList.toggle("field-disabled", !state.processVap);
}

// ============================================================
// Hub — browse and download VaM Hub resources (after VaM Backstage's Hub)
//
// The Hub JSON API (hub_api → getInfo / getResources / getResourceDetail /
// findPackages) drives a filter bar, an infinitely-scrolling card grid and a
// details panel. Download opens a picker (the resource's own .var ticked, its
// dependencies to tick one by one or all at once); the picked dependencies
// are resolved the way Backstage does (detail.dependencies, then
// findPackages) and every file goes to the Downloads queue, which saves into
// AddonPackages. "Installed" is decided against the .var files actually in
// AddonPackages (+ Settings' extra folders). A resource's files are saved in
// <downloads>\<Creator>\ and its dependencies in <downloads>\<Creator>\deps\.
// Resource pages (description, reviews, …) open in the Hub page, an embedded
// browser (see "Hub page" below); .var downloads clicked there come back to the
// Downloads queue too.
// ============================================================

const HUB_PER_PAGE = 60;
const HUB_TYPE_COLORS = {
  Scenes: "#3b82f6",
  SubScenes: "#64839e",
  Looks: "#ec4899",
  Poses: "#f97316",
  Clothing: "#8b5cf6",
  Hairstyles: "#f59e0b",
};
const HUB_TYPE_FIRST = ["Scenes", "SubScenes", "Looks", "Poses", "Clothing", "Hairstyles"];
const HUB_LICENSES = [
  "Any",
  "Non-commercial use allowed",
  "Commercial use allowed",
  "Public Domain",
  "CC BY",
  "CC BY-SA",
  "CC BY-ND",
  "CC BY-NC",
  "CC BY-NC-SA",
  "CC BY-NC-ND",
  "FC",
  "PC",
  "PC EA",
  "Questionable",
];
const HUB_COMMERCIAL = new Set(["Public Domain", "CC BY", "CC BY-SA", "CC BY-ND", "FC"]);
const HUB_NONCOMMERCIAL = new Set([...HUB_COMMERCIAL, "CC BY-NC", "CC BY-NC-SA", "CC BY-NC-ND"]);
const HUB_WISHLIST_SORTS = ["Recently added", "Author", "Name (A-Z)", "Downloads", "Rating", "Reaction Score"];
const HUB_STORE_KEY = "hub.view";
const HUB_GAP = 12;

const HUB = {
  mode: "hub",
  info: null,
  infoError: null,
  items: [],
  total: 0,
  page: 0,
  loading: false,
  done: false,
  error: null,
  token: 0,
  refreshNext: false,
  filters: { search: "", type: "All", pricing: "All", tags: [], author: "", license: "Any", sort: "" },
  wishSort: "Recently added",
  wishlist: new Map(),
  wishlistLoaded: false,
  selected: null,
  rows: new Map(),
  details: new Map(),
  detailPending: new Map(),
  local: { ids: new Set(), bases: new Map(), loading: null, loaded: false },
  installs: new Map(),
  view: "cards",
  cardWidth: 220,
  detailWidth: 340,
  opened: false,
};

// ---- Small helpers -----------------------------------------------------------

function hubStr(v) {
  return v == null || v === "null" ? "" : String(v);
}

function hubNum(v) {
  const n = parseFloat(hubStr(v));
  return Number.isFinite(n) ? n : 0;
}

function hubFormatNumber(v) {
  const n = hubNum(v);
  if (n >= 1e6) return `${(n / 1e6).toFixed(1).replace(/\.0$/, "")}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1).replace(/\.0$/, "")}k`;
  return String(Math.round(n));
}

function hubDate(unixSeconds) {
  const n = hubNum(unixSeconds);
  if (!n) return "—";
  return new Date(n * 1000).toLocaleDateString("en-US", { year: "numeric", month: "short", day: "numeric" });
}

function hubTypeColor(type) {
  if (HUB_TYPE_COLORS[type]) return HUB_TYPE_COLORS[type];
  if (!type) return "#6366f1";
  return `hsl(${Math.abs(libHash(type) % 360)} 45% 50%)`;
}

function hubLicense(r) {
  return hubStr(r?.hubFiles?.[0]?.licenseType) || hubStr(r?.licenseType);
}

function hubLicenseMatches(license, filter) {
  if (!filter || filter === "Any") return true;
  if (filter === "Commercial use allowed") return HUB_COMMERCIAL.has(license);
  if (filter === "Non-commercial use allowed") return HUB_NONCOMMERCIAL.has(license);
  return license === filter;
}

function hubIsRealUrl(v) {
  const s = hubStr(v);
  return Boolean(s) && !s.endsWith("?file=");
}

function hubFileUrl(f) {
  if (hubIsRealUrl(f?.downloadUrl)) return hubStr(f.downloadUrl);
  if (hubIsRealUrl(f?.urlHosted)) return hubStr(f.urlHosted);
  return null;
}

function hubEnsureVar(name) {
  const s = hubStr(name).trim();
  return !s ? "" : /\.var$/i.test(s) ? s : `${s}.var`;
}

function hubStem(name) {
  return hubStr(name).trim().replace(/\.var$/i, "");
}

function hubBaseOf(stemOrRef) {
  return hubStem(stemOrRef).replace(/\.(\d+|latest|min\d+)$/i, "");
}

function hubResourceUrl(rid) {
  return `https://hub.virtamate.com/resources/${encodeURIComponent(rid)}/`;
}

function hubExternalLabel(url) {
  const u = hubStr(url).toLowerCase();
  const known = [
    ["patreon.com", "Patreon"],
    ["gumroad.com", "Gumroad"],
    ["booth.pm", "Booth"],
    ["ko-fi.com", "Ko-fi"],
    ["subscribestar", "SubscribeStar"],
    ["github.com", "GitHub"],
  ];
  const hit = known.find(([host]) => u.includes(host));
  return hit ? `Get on ${hit[1]}` : "Get Package";
}

function hubOpenUrl(url) {
  if (!url || !invoke) return;
  invoke("open_url", { url }).catch((e) => addLog(`Open link: ${String(e)}`));
}

function hubTitleOf(rid) {
  const key = String(rid);
  return hubStr(HUB.details.get(key)?.title || HUB.rows.get(key)?.title || HUB.wishlist.get(key)?.snapshot?.title);
}

function hubToast(message, kind = "info") {
  showToast(message, kind, kind === "error" ? 7000 : 4000);
}

async function hubCall(action, params = {}, refresh = false) {
  const data = await invoke("hub_api", { action, params, refresh });
  if (data && data.status === "error") throw new Error(data.error || "Hub API error");
  return data;
}

// ---- Preferences -------------------------------------------------------------

function hubLoadPrefs() {
  try {
    const raw = JSON.parse(window.localStorage.getItem(HUB_STORE_KEY) || "{}");
    const f = raw.filters || {};
    HUB.filters = {
      search: "",
      type: typeof f.type === "string" ? f.type : "All",
      pricing: ["All", "Free", "Paid"].includes(f.pricing) ? f.pricing : "All",
      tags: Array.isArray(f.tags) ? f.tags.filter((t) => typeof t === "string") : [],
      author: typeof f.author === "string" ? f.author : "",
      license: HUB_LICENSES.includes(f.license) ? f.license : "Any",
      sort: typeof f.sort === "string" ? f.sort : "",
    };
    if (["cards", "compact"].includes(raw.view)) HUB.view = raw.view;
    if (Number(raw.cardWidth) >= 100 && Number(raw.cardWidth) <= 500) HUB.cardWidth = Number(raw.cardWidth);
    if (Number(raw.detailWidth) >= 260 && Number(raw.detailWidth) <= 500) HUB.detailWidth = Number(raw.detailWidth);
    if (HUB_WISHLIST_SORTS.includes(raw.wishSort)) HUB.wishSort = raw.wishSort;
  } catch {
    /* defaults */
  }
}

function hubSavePrefs() {
  try {
    const { search, ...filters } = HUB.filters;
    void search;
    window.localStorage.setItem(
      HUB_STORE_KEY,
      JSON.stringify({ filters, view: HUB.view, cardWidth: HUB.cardWidth, detailWidth: HUB.detailWidth, wishSort: HUB.wishSort }),
    );
  } catch {
    /* storage unavailable */
  }
}

// ---- Local packages (what "Installed" means) -------------------------------------

function hubLocalRoots() {
  const roots = [vamAddonPackagesDir(), ...getAdditionalDirs("varPackages"), ...getAdditionalDirs("downloadVars")];
  const custom = ($("settings-downloads-folder")?.value || "").trim();
  if (custom) roots.push(custom);
  const seen = new Set();
  return roots.filter((r) => {
    const k = String(r || "").trim().toLowerCase();
    if (!k || seen.has(k)) return false;
    seen.add(k);
    return true;
  });
}

function hubLocalIndex(ids) {
  HUB.local.ids = new Set(ids);
  HUB.local.bases = new Map();
  for (const id of ids) {
    const m = /^(.*)\.(\d+)$/.exec(id);
    const base = m ? m[1] : id;
    if (!HUB.local.bases.has(base)) HUB.local.bases.set(base, []);
    if (m) HUB.local.bases.get(base).push(Number(m[2]));
  }
}

function hubLoadLocal(force = false) {
  if (!invoke) return Promise.resolve();
  if (HUB.local.loading && !force) return HUB.local.loading;
  if (HUB.local.loaded && !force) return Promise.resolve();
  HUB.local.loading = invoke("list_local_package_ids", { roots: hubLocalRoots() })
    .then((ids) => {
      hubLocalIndex(Array.isArray(ids) ? ids : []);
      HUB.local.loaded = true;
    })
    .catch((e) => addLog(`Hub: could not read local packages — ${String(e)}`))
    .finally(() => {
      HUB.local.loading = null;
      hubRenderGrid();
      hubRenderDetail();
    });
  return HUB.local.loading;
}

function hubLocalAdd(stem) {
  const id = hubStem(stem).toLowerCase();
  if (!id) return;
  HUB.local.ids.add(id);
  const m = /^(.*)\.(\d+)$/.exec(id);
  const base = m ? m[1] : id;
  if (!HUB.local.bases.has(base)) HUB.local.bases.set(base, []);
  if (m) HUB.local.bases.get(base).push(Number(m[2]));
}

// A dependency ref ("A.B.5", "A.B.latest", "A.B.min5") against local files.
function hubResolveLocal(ref) {
  const lc = hubStem(ref).toLowerCase();
  const m = /^(.*)\.(?:(latest)|min(\d+)|(\d+))$/.exec(lc);
  const base = m ? m[1] : lc;
  const versions = HUB.local.bases.get(base);
  if (!versions) return "missing";
  if (!m || m[2]) return "latest";
  if (m[3]) return versions.some((v) => v >= Number(m[3])) ? "latest" : "fallback";
  return versions.includes(Number(m[4])) ? "exact" : "fallback";
}

// ---- Install state ---------------------------------------------------------------

function hubInstallJobs(rid) {
  const inst = HUB.installs.get(String(rid));
  if (!inst) return [];
  return state.downloads.filter((j) => inst.jobs.has(j.id));
}

function hubResourceState(r) {
  const rid = String(r?.resource_id ?? "");
  const inst = HUB.installs.get(rid);
  const jobs = hubInstallJobs(rid);
  const active = jobs.filter(downloadJobActive);
  if (inst?.resolving) return { kind: "queued" };
  if (active.length) {
    const done = jobs.filter((j) => j.status === "done").length;
    const pct = Math.round(jobs.reduce((sum, j) => sum + (j.status === "done" ? 100 : Number(j.percent) || 0), 0) / jobs.length);
    return { kind: "downloading", pct, done, total: jobs.length };
  }
  const files = Array.isArray(r?.hubFiles) ? r.hubFiles : [];
  const stems = files.map((f) => hubStem(f.filename).toLowerCase()).filter(Boolean);
  const exact = stems.find((s) => HUB.local.ids.has(s));
  if (exact) return { kind: "installed", stem: exact };
  if (inst && jobs.some((j) => j.status === "failed")) return { kind: "failed" };
  if (stems.some((s) => HUB.local.bases.has(hubBaseOf(s)))) return { kind: "update" };
  if (hubStr(r?.hubDownloadable) === "false") {
    return { kind: "external", url: hubStr(r.download_url) || hubStr(r.external_url) || hubResourceUrl(rid) };
  }
  if (!files.length && hubStr(r?.category) === "Paid") return { kind: "hub-only" };
  return { kind: "install" };
}

function hubActionHtml(r, { big = false, compact = false } = {}) {
  const st = hubResourceState(r);
  const rid = escapeAttribute(String(r.resource_id));
  const cls = `hub-btn${big ? " is-big" : ""}${compact ? " is-compact" : ""}`;
  switch (st.kind) {
    case "downloading":
    {
      const bar = `<div class="${cls} hub-btn-progress" title="Downloading">
          <span class="hub-btn-fill" style="width:${Math.max(2, st.pct)}%"></span>
          <span class="hub-btn-label">${compact ? `${st.pct}%` : `Downloading ${st.done}/${st.total} · ${st.pct}%`}</span>
        </div>`;
      return big
        ? `<div class="hub-btn-row">${bar}<button type="button" class="lib-btn lib-btn-outline hub-btn-cancel" data-hub-act="cancel" data-hub-rid="${rid}" title="Cancel this package's downloads (finished files are kept)">
            <span class="material-symbols-outlined">close</span>Cancel</button></div>`
        : bar;
    }
    case "queued":
      return `<div class="${cls} hub-btn-queued"><span class="material-symbols-outlined">schedule</span>${compact ? "" : "Queued…"}</div>`;
    case "installed":
      return `<button type="button" class="${cls} hub-btn-installed" data-hub-act="library" data-hub-rid="${rid}">
          <span class="material-symbols-outlined">check_circle</span><span class="hub-btn-text">${compact ? "View" : "View in Library"}</span></button>`;
    case "update":
      return `<button type="button" class="${cls} hub-btn-install" data-hub-act="download" data-hub-rid="${rid}" title="Another version is installed — download this one">
          <span class="material-symbols-outlined">upgrade</span><span class="hub-btn-text">Update</span></button>`;
    case "external":
      return `<button type="button" class="${cls} hub-btn-external" data-hub-act="external" data-hub-rid="${rid}">
          <span class="material-symbols-outlined">open_in_new</span><span class="hub-btn-text">${escapeHtml(compact ? "Get" : hubExternalLabel(st.url))}</span></button>`;
    case "failed":
      return `<button type="button" class="${cls} hub-btn-failed" data-hub-act="download" data-hub-rid="${rid}">
          <span class="material-symbols-outlined">refresh</span><span class="hub-btn-text">Retry</span></button>`;
    case "hub-only":
      return `<button type="button" class="${cls} hub-btn-external" data-hub-act="open" data-hub-rid="${rid}">
          <span class="material-symbols-outlined">open_in_new</span><span class="hub-btn-text">${compact ? "Hub" : "Open on Hub"}</span></button>`;
    default: {
      if (!big) {
        return `<button type="button" class="${cls} hub-btn-install" data-hub-act="download" data-hub-rid="${rid}">
          <span class="material-symbols-outlined">download</span><span class="hub-btn-text">Download</span></button>`;
      }
      // Details panel: Download All (the package + its missing dependencies)
      // in one click, plus the picker beside it to choose files instead.
      const detail = HUB.details.get(String(r.resource_id));
      const missing = detail ? hubMissingDeps(detail) : [];
      const size = detail ? hubFilesSize(detail) + missing.reduce((sum, d) => sum + d.size, 0) : 0;
      const label = missing.length ? `Download All` : "Download";
      const title = missing.length
        ? `The package and its ${missing.length} missing ${missing.length === 1 ? "dependency" : "dependencies"}`
        : "Download the package";
      const main = `<button type="button" class="${cls} hub-btn-install" data-hub-act="download-all" data-hub-rid="${rid}" title="${escapeAttribute(title)}">
          <span class="material-symbols-outlined">download</span><span class="hub-btn-text">${label}${
            size ? ` · ${escapeHtml(formatBytesLocal(size))}` : ""
          }</span></button>`;
      const deps = detail ? hubDependencyList(detail).length : 0;
      if (!deps) return main;
      return `<div class="hub-btn-row">${main}<button type="button" class="lib-btn lib-btn-outline hub-btn-pick" data-hub-act="download" data-hub-rid="${rid}" title="Choose what to download">
          <span class="material-symbols-outlined">checklist</span></button></div>`;
    }
  }
}

// ---- Data loading ----------------------------------------------------------------

async function hubLoadInfo(refresh = false) {
  if (!invoke || (HUB.info && !refresh)) return;
  try {
    const data = await hubCall("getInfo", {}, refresh);
    if (!Array.isArray(data?.type) || !data.type.length || !Array.isArray(data?.sort) || !data.sort.length) {
      throw new Error("Hub getInfo returned an unexpected shape");
    }
    HUB.info = data;
    HUB.infoError = null;
    if (!HUB.filters.sort || !data.sort.includes(HUB.filters.sort)) HUB.filters.sort = data.sort[0];
  } catch (e) {
    HUB.infoError = String(e);
    addLog(`Hub: ${String(e)}`);
  }
  hubRenderFilters();
}

function hubSearchParams(page) {
  const f = HUB.filters;
  const p = { latest_image: "Y", perpage: String(HUB_PER_PAGE), page: String(page) };
  if (f.sort) p.sort = f.sort;
  if (f.search.trim()) {
    p.search = f.search.trim();
    p.searchall = "true";
  }
  if (f.type && f.type !== "All") p.type = f.type;
  if (f.pricing !== "All") p.category = f.pricing;
  if (f.author) p.username = f.author;
  if (f.tags.length) p.tags = f.tags.join(",");
  return p;
}

async function hubLoadPage({ reset = false } = {}) {
  if (!invoke || HUB.mode !== "hub") return;
  if (reset) {
    HUB.token += 1;
    HUB.items = [];
    HUB.total = 0;
    HUB.page = 0;
    HUB.done = false;
    HUB.error = null;
    HUB.loading = false;
    const scroll = $("hub-scroll");
    if (scroll) scroll.scrollTop = 0;
  }
  if (HUB.loading || HUB.done) return;
  const token = HUB.token;
  const page = HUB.page + 1;
  HUB.loading = true;
  hubRenderGrid();
  try {
    const data = await hubCall("getResources", hubSearchParams(page), HUB.refreshNext && page === 1);
    if (token !== HUB.token) return;
    const rows = Array.isArray(data?.resources) ? data.resources : [];
    for (const r of rows) HUB.rows.set(String(r.resource_id), r);
    // The Hub ignores the license parameter, so the page is filtered here.
    HUB.items.push(...rows.filter((r) => hubLicenseMatches(hubLicense(r), HUB.filters.license)));
    HUB.page = page;
    HUB.total = parseInt(data?.pagination?.total_found, 10) || HUB.items.length;
    const pages = parseInt(data?.pagination?.total_pages, 10) || page;
    HUB.done = rows.length === 0 || page >= pages;
    HUB.error = null;
  } catch (e) {
    if (token !== HUB.token) return;
    HUB.error = String(e?.message || e);
    if (page > 1) hubToast(`Failed to load Hub results: ${HUB.error}`, "error");
  } finally {
    if (token === HUB.token) {
      HUB.loading = false;
      if (page === 1) HUB.refreshNext = false;
      hubRenderGrid();
      requestAnimationFrame(() => hubMaybeLoadMore());
    }
  }
}

// Loads the next page when the grid's end is near (or the page didn't fill the
// viewport — license filtering can leave a page short).
function hubMaybeLoadMore() {
  if (HUB.mode !== "hub" || HUB.loading || HUB.done || HUB.error) return;
  const scroll = $("hub-scroll");
  if (!scroll || scroll.offsetParent === null) return;
  if (scroll.scrollTop + scroll.clientHeight >= scroll.scrollHeight - 700) hubLoadPage();
}

async function hubGetDetail(rid) {
  const key = String(rid);
  if (HUB.details.has(key)) return HUB.details.get(key);
  if (HUB.detailPending.has(key)) return HUB.detailPending.get(key);
  const p = hubCall("getResourceDetail", { latest_image: "Y", resource_id: key })
    .then((d) => {
      HUB.details.set(key, d);
      if (HUB.wishlist.has(key)) hubWishlistSet(key, d, { quiet: true });
      return d;
    })
    .finally(() => HUB.detailPending.delete(key));
  HUB.detailPending.set(key, p);
  return p;
}

async function hubLoadWishlist() {
  if (!invoke) return;
  try {
    const rows = await invoke("hub_wishlist_list");
    HUB.wishlist = new Map((rows || []).map((r) => [String(r.resource_id), r]));
  } catch (e) {
    addLog(`Hub wishlist: ${String(e)}`);
  }
  HUB.wishlistLoaded = true;
  vpSetText("hub-wishlist-count", HUB.wishlist.size ? String(HUB.wishlist.size) : "");
}

function hubSnapshot(r) {
  const out = {};
  for (const [k, v] of Object.entries(r || {})) if (!k.startsWith("_")) out[k] = v;
  return out;
}

async function hubWishlistSet(rid, row, { quiet = false } = {}) {
  const key = String(rid);
  try {
    if (row) {
      await invoke("hub_wishlist_set", { resourceId: key, snapshot: hubSnapshot(row) });
      const prev = HUB.wishlist.get(key);
      HUB.wishlist.set(key, { resource_id: key, snapshot: hubSnapshot(row), created_at: prev?.created_at ?? Date.now() / 1000 });
    } else {
      await invoke("hub_wishlist_set", { resourceId: key, snapshot: null });
      HUB.wishlist.delete(key);
    }
  } catch (e) {
    if (!quiet) hubToast(`Wishlist: ${String(e)}`, "error");
    return;
  }
  vpSetText("hub-wishlist-count", HUB.wishlist.size ? String(HUB.wishlist.size) : "");
  if (!quiet) {
    if (HUB.mode === "wishlist") hubRenderGrid();
    else hubSyncCards();
    hubRenderDetail();
  }
}

function hubToggleWishlist(rid) {
  const key = String(rid);
  if (HUB.wishlist.has(key)) hubWishlistSet(key, null);
  else hubWishlistSet(key, HUB.details.get(key) || HUB.rows.get(key));
}

// The wishlist, filtered and sorted client-side like Backstage's.
function hubWishlistItems() {
  const f = HUB.filters;
  const q = f.search.trim().toLowerCase();
  const items = [...HUB.wishlist.values()]
    .map((w) => ({ ...w.snapshot, _added: w.created_at }))
    .filter((r) => {
      if (q) {
        const hay = [r.title, r.username, r.tag_line, r.tags].map(hubStr).join("\n").toLowerCase();
        if (!q.split(/\s+/).every((t) => hay.includes(t))) return false;
      }
      if (f.type !== "All" && hubStr(r.type) !== f.type) return false;
      if (f.pricing !== "All" && hubStr(r.category) !== f.pricing) return false;
      if (f.author && hubStr(r.username).toLowerCase() !== f.author.toLowerCase()) return false;
      if (f.tags.length) {
        const tags = hubStr(r.tags).toLowerCase().split(",").map((t) => t.trim());
        if (!f.tags.every((t) => tags.includes(t.toLowerCase()))) return false;
      }
      return hubLicenseMatches(hubLicense(r), f.license);
    });
  const by = {
    "Recently added": (a, b) => b._added - a._added,
    Author: (a, b) => hubStr(a.username).localeCompare(hubStr(b.username)),
    "Name (A-Z)": (a, b) => hubStr(a.title).localeCompare(hubStr(b.title)),
    Downloads: (a, b) => hubNum(b.download_count) - hubNum(a.download_count),
    Rating: (a, b) => hubNum(b.rating_avg) - hubNum(a.rating_avg),
    "Reaction Score": (a, b) => hubNum(b.reaction_score) - hubNum(a.reaction_score),
  }[HUB.wishSort];
  return by ? items.sort(by) : items;
}

// ---- Install -------------------------------------------------------------------------

// Every dependency of a resource, de-duplicated, with its local resolution and
// the concrete file a download would fetch.
function hubDependencyList(detail) {
  const groups = detail?.dependencies && typeof detail.dependencies === "object" ? detail.dependencies : {};
  const seen = new Set();
  const out = [];
  for (const [group, list] of Object.entries(groups)) {
    if (!Array.isArray(list)) continue;
    for (const d of list) {
      const ref = hubStr(d?.filename) || hubStr(d?.packageName);
      const key = (ref || group).toLowerCase();
      if (!ref || seen.has(key)) continue;
      seen.add(key);
      const version = hubStr(d.latest_version);
      const concrete = /^\d+$/.test(version) && hubStr(d.packageName) ? `${d.packageName}.${version}.var` : "";
      const resolution = hubResolveLocal(ref);
      out.push({
        ref,
        packageName: hubStr(d.packageName) || hubBaseOf(ref),
        concrete,
        url: hubFileUrl(d),
        size: hubNum(d.file_size),
        resourceId: hubStr(d.resource_id),
        username: hubStr(d.username),
        license: hubStr(d.licenseType),
        resolution,
        installed: resolution === "exact" || resolution === "latest",
      });
    }
  }
  return out;
}

// Dependencies Download All fetches: not on disk in any version (one that is
// already has VaM fall back to it), and with a source to download from.
function hubMissingDeps(detail) {
  return hubDependencyList(detail).filter((d) => !d.installed && d.resolution !== "fallback" && (d.url || d.packageName));
}

// The package's own files plus every missing dependency, no picker.
async function hubDownloadAll(rid) {
  try {
    const detail = await hubGetDetail(String(rid));
    hubDownload(rid, { files: null, depRefs: hubMissingDeps(detail).map((d) => d.ref) });
  } catch (e) {
    hubToast(`Download failed: ${String(e?.message || e)}`, "error");
  }
}

function hubFilesSize(detail) {
  return (detail?.hubFiles || []).reduce((sum, f) => sum + hubNum(f.file_size), 0);
}

function hubJoinPath(dir, name) {
  const sep = dir.includes("\\") ? "\\" : "/";
  return dir.replace(/[\\/]+$/, "") + sep + name;
}

function hubSafeFolder(name) {
  return hubStr(name).trim().replace(/[<>:"/\\|?*\x00-\x1f]/g, "_").replace(/[. ]+$/, "");
}

// <downloads>\<Creator> for a resource: the creator part of its package file
// name (what VaM sorts by), else the Hub author.
function hubResourceDir(rid, baseDir) {
  const detail = HUB.details.get(String(rid)) || HUB.rows.get(String(rid));
  const file = (detail?.hubFiles || []).map((f) => hubStem(f.filename)).find(Boolean) || "";
  const creator = hubSafeFolder(deriveCreatorFromPackageId(file) || detail?.username || "");
  return creator ? hubJoinPath(baseDir, creator) : baseDir;
}

// The resource's own files go to <downloads>\<Creator>, its dependencies to
// <downloads>\<Creator>\deps.
function hubQueueFile(rid, { url, filename, label, host = "hub", depRef = null }, baseDir) {
  const name = hubEnsureVar(filename);
  const stem = hubStem(name);
  if (!url || !name) return null;
  if (HUB.local.ids.has(stem.toLowerCase())) return null;
  const inst = HUB.installs.get(String(rid));
  if (depRef && inst) inst.depJobs.set(depRef, stem.toLowerCase());
  const dir = hubResourceDir(rid, baseDir);
  const id = queueDownload({
    packageId: stem,
    url,
    filename: name,
    host,
    destDir: depRef ? hubJoinPath(dir, "deps") : dir,
    nest: false,
    label: label || stem,
    onProgress: () => hubOnJobProgress(rid),
    onDone: (status) => hubOnJobDone(rid, stem, status),
  });
  if (id != null && inst) inst.jobs.add(id);
  return id;
}

let hubProgressTimer = 0;
function hubOnJobProgress() {
  if (hubProgressTimer) return;
  hubProgressTimer = setTimeout(() => {
    hubProgressTimer = 0;
    hubSyncCards();
    hubSyncDetailAction();
  }, 250);
}

function hubOnJobDone(rid, stem, status) {
  if (status === "done") hubLocalAdd(stem);
  hubSyncCards();
  hubRenderDetail();
  const jobs = hubInstallJobs(rid);
  if (jobs.length && jobs.every((j) => !downloadJobActive(j))) {
    const failed = jobs.filter((j) => j.status === "failed").length;
    const cancelled = jobs.filter((j) => j.status === "cancelled").length;
    const done = jobs.length - failed - cancelled;
    const title = hubTitleOf(rid) || "Package";
    if (!failed && !cancelled) hubToast(`${title} downloaded`, "success");
    else {
      const parts = [`${done} downloaded`];
      if (failed) parts.push(`${failed} failed`);
      if (cancelled) parts.push(`${cancelled} cancelled`);
      hubToast(`${title}: ${parts.join(", ")}`, failed ? "error" : "info");
    }
    hubLoadLocal(true);
    // The library listing scans AddonPackages; pick up the new files.
    vpRefreshAfterMutation().catch(() => {});
  }
}

// Download part of a resource through the Downloads queue into AddonPackages:
// `files` — its own .var file names (null = all of them) — and `depRefs`, the
// dependencies to fetch (resolved through detail.dependencies, then
// findPackages). Dependencies already on disk are skipped.
async function hubDownload(rid, { files = null, depRefs = [] } = {}) {
  if (!invoke) return;
  const key = String(rid);
  const existing = HUB.installs.get(key);
  if (existing?.resolving) return;
  const inst = existing || { jobs: new Set(), depJobs: new Map(), unavailable: new Set() };
  inst.resolving = true;
  HUB.installs.set(key, inst);
  hubSyncCards();
  hubSyncDetailAction();
  try {
    await hubLoadLocal();
    const destDir = await ensureDownloadsDir();
    if (!destDir) throw new Error("No downloads folder — set your VaM directory in Settings.");
    const detail = await hubGetDetail(key);
    const title = hubStr(detail?.title) || `Resource ${key}`;
    let queued = 0;
    const wantDeps = new Set(depRefs);
    const deps = hubDependencyList(detail).filter((d) => wantDeps.has(d.ref));
    const lookups = new Map(); // findPackages name -> dependency ref

    const wantFiles = files == null ? null : new Set(files.map((f) => hubStr(f).toLowerCase()));
    if (wantFiles === null || wantFiles.size) {
      const all = Array.isArray(detail?.hubFiles) ? detail.hubFiles : [];
      const picked = all.filter((f) => wantFiles === null || wantFiles.has(hubStr(f.filename).toLowerCase()));
      if (!picked.length) throw new Error("No downloadable files");
      let anyUrl = false;
      for (const f of picked) {
        const url = hubFileUrl(f);
        if (!url) continue;
        anyUrl = true;
        if (hubQueueFile(key, { url, filename: f.filename, label: title }, destDir) != null) queued += 1;
      }
      if (!anyUrl) throw new Error("No download URL available");
    }

    for (const d of deps) {
      if (d.installed) continue;
      if (d.concrete && d.url) {
        if (hubQueueFile(key, { url: d.url, filename: d.concrete, label: d.concrete, depRef: d.ref }, destDir) != null) {
          queued += 1;
        }
      } else {
        lookups.set(d.packageName ? `${d.packageName}.latest` : d.ref, d.ref);
      }
    }

    const unresolved = [];
    const names = [...lookups.keys()];
    for (let i = 0; i < names.length; i += 50) {
      const batch = names.slice(i, i + 50);
      let found = {};
      try {
        found = (await hubCall("findPackages", { packages: batch.join(",") }))?.packages || {};
      } catch (e) {
        addLog(`Hub: findPackages failed — ${String(e)}`);
      }
      for (const name of batch) {
        const hit = found[name];
        const file = hubStr(hit?.filename);
        const url = hubFileUrl(hit);
        if (file && /\.\d+\.var$/i.test(file) && url) {
          if (hubQueueFile(key, { url, filename: file, label: file, depRef: lookups.get(name) }, destDir) != null) {
            queued += 1;
          }
        } else {
          unresolved.push(hubBaseOf(name));
          inst.unavailable.add(lookups.get(name));
        }
      }
    }

    if (unresolved.length) {
      hubToast(
        `${unresolved.length} ${unresolved.length === 1 ? "dependency" : "dependencies"} unavailable: ${unresolved.slice(0, 3).join(", ")}${unresolved.length > 3 ? "…" : ""}`,
        "error",
      );
    }
    if (!queued && !unresolved.length) hubToast(`${title}: already downloaded`, "success");
    else if (queued) hubToast(`Downloading ${title} — ${queued} file${queued === 1 ? "" : "s"} queued`, "info");
  } catch (e) {
    hubToast(`Download failed: ${String(e?.message || e)}`, "error");
  } finally {
    inst.resolving = false;
    hubSyncCards();
    hubRenderDetail();
  }
}

function hubShowInLibrary(r) {
  const st = hubResourceState(r);
  const stem = st.stem || hubStem(r?.hubFiles?.[0]?.filename);
  document.querySelector('[data-sidebar-link="var-packages"]')?.click();
  if (stem) setTimeout(() => libRevealPackage("", stem), 50);
}

// Cancel every download still running or queued for a resource.
function hubCancelDownloads(rid) {
  const active = hubInstallJobs(rid).filter(downloadJobActive);
  for (const job of active.filter((j) => j.status === "queued")) cancelDownload(job.id);
  for (const job of active.filter((j) => j.status === "downloading")) cancelDownload(job.id);
}

// ---- Download picker -------------------------------------------------------------------
// Every Download button opens this: the resource's own .var files start ticked
// and its dependencies unticked — tick them one by one, or all the missing ones
// at once. Dependencies with another version on disk (VaM falls back to it)
// can be ticked but aren't part of "Select all".

const HUB_DL = { rid: null, files: [], deps: [], token: 0 };

function hubDlActiveJob(stem) {
  const lc = hubStr(stem).toLowerCase();
  if (!lc) return null;
  return state.downloads.find((j) => j.packageId && j.packageId.toLowerCase() === lc && downloadJobActive(j)) || null;
}

function hubDlSelectable(item) {
  return item.available && !item.installed && !item.busy;
}

function hubDlMissingDeps() {
  return HUB_DL.deps.filter((d) => hubDlSelectable(d) && d.resolution !== "fallback");
}

function hubDlRenderHead(r) {
  const title = hubStr(r?.title) || `Resource ${HUB_DL.rid}`;
  $("hub-dl-title").textContent = title;
  $("hub-dl-title").title = title;
  const sub = [hubStr(r?.username), hubStr(r?.type), hubStr(r?.version_string) && `v${hubStr(r.version_string)}`].filter(Boolean);
  $("hub-dl-sub").textContent = sub.join(" · ");
  $("hub-dl-thumb").innerHTML = r?.resource_id ? `${hubThumbHtml(r, "hub-dl-thumb-img")}</div>` : "";
}

function hubDlRowHtml(kind, i, item, name, meta, chip) {
  const enabled = hubDlSelectable(item);
  return `<label class="check-row hub-dl-row${enabled ? "" : " is-off"}">
      <input type="checkbox" data-hub-dl="${kind}" data-hub-dl-idx="${i}"${item.checked ? " checked" : ""}${enabled ? "" : " disabled"} />
      <span class="hub-dl-main">
        <span class="hub-dl-name" title="${escapeAttribute(name)}">${escapeHtml(name)}</span>
        ${meta ? `<span class="hub-dl-meta">${escapeHtml(meta)}</span>` : ""}
      </span>
      <span class="hub-dl-size">${item.size ? escapeHtml(formatBytesLocal(item.size)) : ""}</span>
      <span class="hub-dl-chip">${chip}</span>
    </label>`;
}

function hubDlChip(item) {
  if (item.installed) return `<span class="lib-pill lib-pill-ok">Installed</span>`;
  if (item.busy) return `<span class="lib-pill lib-pill-info">Downloading</span>`;
  if (!item.available) return `<span class="lib-pill lib-pill-err">No source</span>`;
  if (item.resolution === "fallback") {
    return `<span class="lib-pill lib-pill-warn" title="Another version is installed and VaM will use it">Other version</span>`;
  }
  return item.resolution === "missing" ? `<span class="lib-pill lib-pill-err">Missing</span>` : "";
}

function hubDlRenderBody() {
  const body = $("hub-dl-body");
  if (!body) return;
  const fileRows = HUB_DL.files.map((f, i) => hubDlRowHtml("file", i, f, f.name, "", hubDlChip(f))).join("");
  const depRows = HUB_DL.deps
    .map((d, i) => {
      const meta = [
        d.concrete && d.concrete.toLowerCase() !== hubEnsureVar(d.ref).toLowerCase() ? `→ ${d.concrete}` : "",
        d.username,
        d.license,
      ]
        .filter(Boolean)
        .join(" · ");
      return hubDlRowHtml("dep", i, d, d.ref, meta, hubDlChip(d));
    })
    .join("");
  const missing = hubDlMissingDeps().length;
  body.innerHTML = `
    <div class="hub-dl-group">
      <div class="hub-dl-group-head"><span class="hub-dl-group-title">Package <small>(${HUB_DL.files.length})</small></span></div>
      ${fileRows ? `<div class="hub-dl-list">${fileRows}</div>` : `<p class="hub-dl-empty">The Hub lists no downloadable files for this resource.</p>`}
    </div>
    <div class="hub-dl-group">
      <div class="hub-dl-group-head">
        <span class="hub-dl-group-title">Dependencies <small>(${HUB_DL.deps.length})</small></span>
        ${
          missing
            ? `<label class="check-row hub-dl-all"><input type="checkbox" id="hub-dl-all" /><span>Select all missing (${missing})</span></label>`
            : ""
        }
      </div>
      ${
        depRows
          ? `<div class="hub-dl-list">${depRows}</div>`
          : `<p class="hub-dl-empty">No dependencies</p>`
      }
      ${HUB_DL.deps.length && !missing ? `<p class="hub-dl-empty">Every dependency is already installed.</p>` : ""}
    </div>`;
}

// Select-all state, the selection total and the Download button.
function hubDlSync() {
  const all = $("hub-dl-all");
  if (all) {
    const missing = hubDlMissingDeps();
    const on = missing.filter((d) => d.checked).length;
    all.checked = on > 0 && on === missing.length;
    all.indeterminate = on > 0 && on < missing.length;
  }
  const picked = [...HUB_DL.files, ...HUB_DL.deps].filter((x) => x.checked);
  const size = picked.reduce((sum, x) => sum + (x.size || 0), 0);
  const total = $("hub-dl-total");
  if (total) {
    total.textContent = picked.length
      ? `${picked.length} file${picked.length === 1 ? "" : "s"}${size ? ` · ${formatBytesLocal(size)}` : ""}`
      : "Nothing selected";
  }
  const confirm = $("hub-dl-confirm");
  if (confirm) {
    confirm.disabled = !picked.length;
    confirm.textContent = picked.length > 1 ? `Download (${picked.length})` : "Download";
  }
}

async function hubOpenDownloadPicker(rid) {
  const key = String(rid);
  const backdrop = $("hub-dl-backdrop");
  if (!backdrop) return hubDownload(key);
  const token = ++HUB_DL.token;
  HUB_DL.rid = key;
  HUB_DL.files = [];
  HUB_DL.deps = [];
  hubDlRenderHead(HUB.details.get(key) || HUB.rows.get(key) || HUB.wishlist.get(key)?.snapshot || null);
  $("hub-dl-body").innerHTML = `<div class="hub-dl-loading"><div class="lib-skeleton" style="width:70%"></div><div class="lib-skeleton" style="width:50%"></div></div>`;
  hubDlSync();
  backdrop.classList.remove("hidden");
  try {
    await hubLoadLocal();
    const detail = await hubGetDetail(key);
    if (token !== HUB_DL.token) return;
    hubDlRenderHead(detail);
    HUB_DL.files = (Array.isArray(detail?.hubFiles) ? detail.hubFiles : []).map((f) => {
      const name = hubEnsureVar(f.filename);
      const item = {
        name,
        size: hubNum(f.file_size),
        installed: HUB.local.ids.has(hubStem(name).toLowerCase()),
        busy: Boolean(hubDlActiveJob(hubStem(name))),
        available: Boolean(hubFileUrl(f)),
        checked: false,
      };
      item.checked = hubDlSelectable(item);
      return item;
    });
    HUB_DL.deps = hubDependencyList(detail).map((d) => ({
      ...d,
      busy: Boolean(d.concrete && hubDlActiveJob(hubStem(d.concrete))),
      available: Boolean(d.url || d.packageName),
      checked: false,
    }));
    hubDlRenderBody();
  } catch (e) {
    if (token !== HUB_DL.token) return;
    $("hub-dl-body").innerHTML = `<p class="hub-dl-empty is-error">Couldn't load the details: ${escapeHtml(String(e?.message || e))}</p>`;
  }
  hubDlSync();
}

function hubDlClose() {
  HUB_DL.token += 1;
  $("hub-dl-backdrop")?.classList.add("hidden");
}

function hubDlConfirm() {
  const rid = HUB_DL.rid;
  const files = HUB_DL.files.filter((f) => f.checked).map((f) => f.name);
  const depRefs = HUB_DL.deps.filter((d) => d.checked).map((d) => d.ref);
  if (!rid || (!files.length && !depRefs.length)) return;
  hubDlClose();
  hubDownload(rid, { files, depRefs });
}

function setupHubDownloadPicker() {
  const backdrop = $("hub-dl-backdrop");
  if (!backdrop) return;
  backdrop.addEventListener("error", hubOnImageError, true);
  backdrop.addEventListener("click", (e) => {
    if (e.target === backdrop) hubDlClose();
  });
  $("hub-dl-cancel")?.addEventListener("click", hubDlClose);
  $("hub-dl-confirm")?.addEventListener("click", hubDlConfirm);
  $("hub-dl-body")?.addEventListener("change", (e) => {
    const input = e.target;
    if (!(input instanceof HTMLInputElement)) return;
    if (input.id === "hub-dl-all") {
      for (const d of hubDlMissingDeps()) d.checked = input.checked;
      document.querySelectorAll('#hub-dl-body input[data-hub-dl="dep"]').forEach((box) => {
        box.checked = Boolean(HUB_DL.deps[Number(box.getAttribute("data-hub-dl-idx"))]?.checked);
      });
    } else if (input.hasAttribute("data-hub-dl")) {
      const list = input.getAttribute("data-hub-dl") === "file" ? HUB_DL.files : HUB_DL.deps;
      const item = list[Number(input.getAttribute("data-hub-dl-idx"))];
      if (item) item.checked = input.checked;
    }
    hubDlSync();
  });
}

// .var downloads clicked inside the Hub page arrive here instead of the
// webview's own download manager. The open resource's own files go to its
// creator folder and its dependencies to <Creator>\deps; anything else to
// the file's own creator folder.
async function hubOnPageDownload(event) {
  const url = hubStr(event?.payload?.url);
  const filename = hubEnsureVar(event?.payload?.filename);
  if (!url || !filename) return;
  const stem = hubStem(filename);
  if (HUB.local.ids.has(stem.toLowerCase())) {
    hubToast(`${filename} is already in your library`, "success");
    return;
  }
  const baseDir = await ensureDownloadsDir();
  if (!baseDir) {
    hubToast("No downloads folder — set your VaM directory in Settings.", "error");
    return;
  }
  const rid = HUB_PAGE.rid || HUB.selected;
  const detail = rid ? HUB.details.get(rid) : null;
  const base = hubBaseOf(stem).toLowerCase();
  const own = (detail?.hubFiles || []).some((f) => hubBaseOf(f.filename).toLowerCase() === base);
  const dep = detail && !own && hubDependencyList(detail).some((d) => hubBaseOf(d.packageName || d.ref).toLowerCase() === base);
  let destDir;
  if (own) destDir = hubResourceDir(rid, baseDir);
  else if (dep) destDir = hubJoinPath(hubResourceDir(rid, baseDir), "deps");
  else {
    const creator = hubSafeFolder(deriveCreatorFromPackageId(stem) || "");
    destDir = creator ? hubJoinPath(baseDir, creator) : baseDir;
  }
  const id = queueDownload({
    packageId: stem,
    url,
    filename,
    host: "hub",
    destDir,
    nest: false,
    label: stem,
    onDone: (status) => {
      if (status !== "done") return;
      hubLocalAdd(stem);
      hubLoadLocal(true);
      vpRefreshAfterMutation().catch(() => {});
    },
  });
  if (id != null) hubToast(`Downloading ${filename}`, "info");
}

// ---- Rendering: filters ----------------------------------------------------------------

function hubTypes() {
  const types = Array.isArray(HUB.info?.type) ? [...HUB.info.type] : [];
  return types.sort((a, b) => {
    const ia = HUB_TYPE_FIRST.indexOf(a);
    const ib = HUB_TYPE_FIRST.indexOf(b);
    if (ia >= 0 || ib >= 0) return (ia < 0 ? 99 : ia) - (ib < 0 ? 99 : ib);
    return a.localeCompare(b);
  });
}

function hubActiveFilterCount() {
  const f = HUB.filters;
  return (f.type !== "All" ? 1 : 0) + (f.pricing !== "All" ? 1 : 0) + f.tags.length + (f.author ? 1 : 0) + (f.license !== "Any" ? 1 : 0);
}

function hubRenderFilters() {
  const f = HUB.filters;
  const typeList = $("hub-type-list");
  if (typeList) {
    typeList.innerHTML = ["All", ...hubTypes()]
      .map((t) =>
        libListRowHtml({
          value: t === "All" ? null : t,
          label: t === "All" ? "All types" : t,
          dot: t === "All" ? "" : hubTypeColor(t),
          selected: f.type === t,
          count: null,
        }),
      )
      .join("");
  }
  libSetDropdown("hub-type", f.type === "All" ? "Type" : f.type, f.type !== "All");
  const dot = $("lib-dd-hub-type-dot");
  if (dot) {
    dot.classList.toggle("hidden", f.type === "All");
    dot.style.setProperty("--dot", hubTypeColor(f.type));
  }

  const pricing = $("hub-pricing-list");
  if (pricing) {
    pricing.innerHTML = ["All", "Free", "Paid"]
      .map((v) => libListRowHtml({ value: v === "All" ? null : v, label: v, selected: f.pricing === v, count: null }))
      .join("");
  }
  libSetDropdown("hub-pricing", f.pricing === "All" ? "Pricing" : f.pricing, f.pricing !== "All");

  const tagChips = $("hub-tag-chips");
  if (tagChips) {
    tagChips.classList.toggle("hidden", !f.tags.length);
    tagChips.innerHTML =
      f.tags
        .map(
          (t) => `<button type="button" class="lib-filter-chip" data-hub-tag-remove="${escapeAttribute(t)}">${escapeHtml(t)}<span class="material-symbols-outlined">close</span></button>`,
        )
        .join("") + (f.tags.length ? `<button type="button" class="lib-link-btn" data-hub-tags-clear>Clear</button>` : "");
  }
  libSetDropdown("hub-tags", f.tags.length ? `Tags · ${f.tags.length}` : "Tags", f.tags.length > 0);

  const authorChips = $("hub-author-chips");
  if (authorChips) {
    authorChips.classList.toggle("hidden", !f.author);
    authorChips.innerHTML = f.author
      ? `<button type="button" class="lib-filter-chip" data-hub-author-clear>${escapeHtml(f.author)}<span class="material-symbols-outlined">close</span></button>`
      : "";
  }
  libSetDropdown("hub-author", f.author ? `by ${f.author}` : "Author", Boolean(f.author));

  const license = $("hub-license-list");
  if (license) {
    license.innerHTML = HUB_LICENSES.map((l) =>
      libListRowHtml({ value: l === "Any" ? null : l, label: l, selected: f.license === l, count: null }),
    ).join("");
  }
  libSetDropdown("hub-license", f.license === "Any" ? "License" : f.license, f.license !== "Any");

  const sortList = $("hub-sort-list");
  const sorts = HUB.mode === "wishlist" ? HUB_WISHLIST_SORTS : HUB.info?.sort || [];
  const current = HUB.mode === "wishlist" ? HUB.wishSort : f.sort;
  if (sortList) {
    sortList.innerHTML = sorts
      .map((s) => libListRowHtml({ value: s, label: s, selected: s === current, count: null }))
      .join("");
  }
  libSetDropdown("hub-sort", current ? `Sort: ${current}` : "Sort", false);

  const n = hubActiveFilterCount();
  $("hub-filter-summary")?.classList.toggle("hidden", n === 0);
  vpSetText("hub-filter-summary-text", `${n} filter${n === 1 ? "" : "s"}`);
  const hasSearch = Boolean(f.search.trim());
  $("hub-search-clear")?.classList.toggle("hidden", !hasSearch);
  $("hub-search-wrap")?.classList.toggle("has-value", hasSearch);
  document.querySelectorAll("[data-hub-mode]").forEach((b) =>
    b.classList.toggle("active", b.getAttribute("data-hub-mode") === HUB.mode),
  );
  hubRenderSuggestions("tag");
  hubRenderSuggestions("author");
}

// Tag / author suggestions from getInfo (prefix matches first, by count).
const HUB_AC = { tag: -1, author: -1, tagMatches: [], authorMatches: [] };

function hubRenderSuggestions(kind) {
  const input = $(kind === "tag" ? "hub-tag-input" : "hub-author-input");
  const list = $(kind === "tag" ? "hub-tag-list" : "hub-author-list");
  if (!input || !list) return;
  const source = kind === "tag" ? HUB.info?.tags : HUB.info?.users;
  const q = input.value.trim().toLowerCase();
  const entries = source && typeof source === "object" ? Object.entries(source) : [];
  const chosen = new Set(kind === "tag" ? HUB.filters.tags.map((t) => t.toLowerCase()) : []);
  const starts = [];
  const contains = [];
  for (const [name, meta] of entries) {
    const lower = name.toLowerCase();
    if (chosen.has(lower)) continue;
    const ct = Number(meta?.ct) || 0;
    if (!q || lower.startsWith(q)) starts.push([name, ct]);
    else if (lower.includes(q)) contains.push([name, ct]);
  }
  const byCount = (a, b) => b[1] - a[1] || a[0].localeCompare(b[0]);
  const matches = [...starts.sort(byCount), ...contains.sort(byCount)].slice(0, 20);
  HUB_AC[`${kind}Matches`] = matches.map((m) => m[0]);
  if (HUB_AC[kind] >= matches.length) HUB_AC[kind] = matches.length - 1;
  list.innerHTML = matches.length
    ? matches
        .map(
          ([name, ct], i) => `<button type="button" class="lib-ac-option${i === HUB_AC[kind] ? " is-active" : ""}" data-hub-pick-${kind}="${escapeAttribute(name)}">
            <span class="lib-row-label">${escapeHtml(name)}</span><span class="lib-row-count">${ct.toLocaleString()}</span></button>`,
        )
        .join("")
    : `<div class="lib-ac-empty">${HUB.info ? "No matches" : "Loading Hub filters…"}</div>`;
}

// ---- Rendering: grid ---------------------------------------------------------------------

function hubAvatarHtml(name, iconUrl, size = 30) {
  const initials = libAuthorInitials(name || "?");
  const img = hubStr(iconUrl)
    ? `<img alt="" loading="lazy" referrerpolicy="no-referrer" src="${escapeAttribute(hubStr(iconUrl))}" data-hub-hide-on-error />`
    : "";
  return `<span class="lib-avatar hub-avatar${size < 24 ? " is-sm" : ""}" style="background:${libAuthorColor(name || "?")};width:${size}px;height:${size}px">${escapeHtml(initials)}${img}</span>`;
}

function hubThumbHtml(r, cls = "lib-thumb") {
  const url = hubStr(r.image_url);
  return `<div class="${cls}" style="--lib-thumb-bg:${escapeAttribute(libGradient(String(r.resource_id)))}">
      ${url ? `<img alt="" loading="lazy" referrerpolicy="no-referrer" src="${escapeAttribute(url)}" data-hub-img="${escapeAttribute(url)}" />` : ""}`;
}

function hubCardHtml(r) {
  const rid = String(r.resource_id);
  const compact = HUB.view === "compact";
  const type = hubStr(r.type);
  const pinned = HUB.wishlist.has(rid);
  const chips = [];
  if (type && HUB.filters.type === "All") {
    chips.push(`<span class="lib-chip lib-chip-type" style="background:${hubTypeColor(type)}cc">${escapeHtml(type)}</span>`);
  }
  if (hubStr(r.category) === "Paid") chips.push(`<span class="lib-chip lib-chip-type" style="background:#fbbf24cc">Paid</span>`);
  const title = hubStr(r.title);
  const user = hubStr(r.username);
  const author = `<button type="button" class="lib-author-link" data-hub-author="${escapeAttribute(user)}" title="Filter by ${escapeAttribute(user)}">${escapeHtml(user)}</button>`;
  const pin =
    HUB.mode === "wishlist"
      ? `<button type="button" class="hub-pin is-remove" data-hub-pin="${escapeAttribute(rid)}" title="Remove from wishlist">
           <span class="material-symbols-outlined">delete</span></button>`
      : `<button type="button" class="hub-pin${pinned ? " is-on" : ""}" data-hub-pin="${escapeAttribute(rid)}" title="${pinned ? "Remove from wishlist" : "Add to wishlist"}">
           <span class="material-symbols-outlined">push_pin</span></button>`;
  const stats = `<div class="lib-card-stats hub-stats">
      <span class="lib-stat" title="Downloads"><span class="material-symbols-outlined">download</span>${hubFormatNumber(r.download_count)}</span>
      <span class="lib-stat hub-stat-react" title="Reaction score"><span class="material-symbols-outlined">thumb_up</span>${hubFormatNumber(r.reaction_score)}</span>
      <span class="lib-stat hub-stat-rating" title="Rating"><span class="material-symbols-outlined">star</span>${hubNum(r.rating_avg) ? hubNum(r.rating_avg).toFixed(1).replace(/\.0$/, "") : "—"}</span>
    </div>`;
  return `<div class="lib-card hub-card${HUB.selected === rid ? " is-picked" : ""}" data-hub-rid="${escapeAttribute(rid)}" role="option" tabindex="-1">
      ${hubThumbHtml(r)}
        <div class="lib-thumb-shade"></div>
        <div class="lib-thumb-chips">${chips.join("")}</div>
        ${pin}
        ${
          compact
            ? `<div class="lib-card-scrim hub-scrim">
                 <div class="hub-scrim-text">
                   <div class="lib-card-title" title="${escapeAttribute(title)}">${escapeHtml(title)}</div>
                   <span class="lib-by">by ${author}</span>
                 </div>
                 <div class="hub-card-action" data-hub-action-slot="${escapeAttribute(rid)}">${hubActionHtml(r, { compact: true })}</div>
               </div>`
            : ""
        }
      </div>
      ${
        compact
          ? ""
          : `<div class="lib-card-footer hub-card-footer">
               <div class="lib-card-row">
                 ${hubAvatarHtml(user, r.icon_url)}
                 <div class="lib-card-text">
                   <div class="lib-title-line"><span class="lib-card-title" title="${escapeAttribute(title)}">${escapeHtml(title)}</span></div>
                   <span class="lib-by">by ${author}</span>
                 </div>
               </div>
               ${stats}
             </div>
             <div class="hub-card-actions" data-hub-action-slot="${escapeAttribute(rid)}">${hubActionHtml(r)}</div>`
      }
    </div>`;
}

function hubVisibleItems() {
  return HUB.mode === "wishlist" ? hubWishlistItems() : HUB.items;
}

function hubRenderGrid() {
  const grid = $("hub-grid");
  if (!grid) return;
  const items = hubVisibleItems();
  $("hub-progress")?.classList.toggle("hidden", !HUB.loading);
  const err = $("hub-error");
  const showErr = HUB.mode === "hub" && HUB.error && !HUB.items.length;
  err?.classList.toggle("hidden", !showErr);
  if (showErr) vpSetText("hub-error-text", `Couldn't reach the Hub: ${HUB.error}`);
  document.querySelectorAll("[data-hub-view]").forEach((b) =>
    b.classList.toggle("active", b.getAttribute("data-hub-view") === HUB.view),
  );

  if (HUB.mode === "wishlist") {
    vpSetText("hub-count", `${items.length.toLocaleString()} of ${HUB.wishlist.size.toLocaleString()} wishlisted`);
  } else {
    vpSetText(
      "hub-count",
      HUB.loading && !HUB.items.length ? "Searching…" : `${HUB.total.toLocaleString()} packages`,
    );
  }

  if (!items.length) {
    if (HUB.mode === "hub" && HUB.loading) {
      grid.innerHTML = Array.from({ length: 12 }, () => `<div class="lib-card hub-card hub-skeleton"><div class="lib-thumb lib-skeleton"></div><div class="lib-card-footer hub-card-footer"><div class="lib-skeleton" style="width:70%"></div><div class="lib-skeleton" style="width:40%;margin-top:8px"></div></div></div>`).join("");
    } else if (HUB.mode === "wishlist") {
      grid.innerHTML = `<div class="lib-empty">${HUB.wishlist.size ? "No wishlisted packages match the filters" : "Your wishlist is empty."}<span class="lib-empty-sub">Pin a package with <span class="material-symbols-outlined" style="font-size:13px;vertical-align:-2px">push_pin</span> to keep it here.</span></div>`;
    } else if (!showErr) {
      grid.innerHTML = `<div class="lib-empty">No packages found</div>`;
    } else {
      grid.innerHTML = "";
    }
  } else {
    grid.innerHTML = items.map((r) => hubCardHtml(r)).join("");
  }
  const more = $("hub-load-more");
  if (more) {
    const show = HUB.mode === "hub" && HUB.items.length > 0 && (!HUB.done || HUB.loading);
    more.classList.toggle("hidden", !show);
    more.innerHTML = HUB.loading
      ? "Loading more…"
      : HUB.error
        ? `Couldn't load more results. <button type="button" class="lib-link-btn" data-hub-retry-more>Retry</button>`
        : `Showing ${HUB.items.length.toLocaleString()} — scroll for more`;
  }
  hubApplyLayout();
  hubRenderStatusBar();
  hubRenderFilters();
}

// Patches only the action buttons and pin states (download progress ticks).
function hubSyncCards() {
  document.querySelectorAll("#hub-grid [data-hub-action-slot]").forEach((slot) => {
    const rid = slot.getAttribute("data-hub-action-slot");
    const r = HUB.rows.get(rid) || HUB.wishlist.get(rid)?.snapshot;
    if (r) slot.innerHTML = hubActionHtml(r, { compact: HUB.view === "compact" });
  });
  document.querySelectorAll("#hub-grid .hub-pin:not(.is-remove)").forEach((pin) => {
    const on = HUB.wishlist.has(pin.getAttribute("data-hub-pin"));
    pin.classList.toggle("is-on", on);
    pin.title = on ? "Remove from wishlist" : "Add to wishlist";
  });
}

function hubApplyLayout() {
  const grid = $("hub-grid");
  const scroll = $("hub-scroll");
  if (!grid || !scroll) return;
  const avail = Math.max(0, scroll.clientWidth - 32);
  if (!avail) return;
  const cols = Math.max(1, Math.floor((avail + HUB_GAP) / (HUB.cardWidth + HUB_GAP)));
  grid.style.setProperty("--lib-cols", String(cols));
  const slider = $("hub-size-slider");
  if (slider) {
    const minCols = Math.max(1, Math.ceil((avail + HUB_GAP) / (500 + HUB_GAP)));
    const maxCols = Math.max(minCols, Math.floor((avail + HUB_GAP) / (100 + HUB_GAP)));
    slider.min = String(minCols);
    slider.max = String(maxCols);
    slider.value = String(Math.min(maxCols, Math.max(minCols, cols)));
    $("hub-size-slider-wrap")?.classList.toggle("hidden", maxCols <= minCols);
  }
  const view = $("hub-view");
  view?.style.setProperty("--lib-detail-w", `${HUB.detailWidth}px`);
}

function hubRenderStatusBar() {
  const bar = $("hub-statusbar");
  if (!bar) return;
  const sep = `<span class="lib-sb-sep">·</span>`;
  const activeInstalls = [...HUB.installs.keys()].filter((rid) => hubInstallJobs(rid).some(downloadJobActive)).length;
  bar.innerHTML = `
    <span class="lib-sb-item"><span class="material-symbols-outlined">explore</span>${
      HUB.mode === "wishlist" ? "Wishlist" : `${HUB.items.length.toLocaleString()} loaded of ${HUB.total.toLocaleString()}`
    }</span>${sep}
    <span class="lib-sb-item"><span class="material-symbols-outlined">push_pin</span>${HUB.wishlist.size.toLocaleString()} wishlisted</span>${sep}
    <span class="lib-sb-item"><span class="material-symbols-outlined">inventory_2</span>${HUB.local.ids.size.toLocaleString()} installed</span>
    ${activeInstalls ? `${sep}<span class="lib-sb-item hub-sb-active"><span class="material-symbols-outlined">downloading</span>${activeInstalls} downloading</span>` : ""}
    <span class="lib-sb-right">VaM Hub · hub.virtamate.com</span>`;
}

// ---- Rendering: details ---------------------------------------------------------------------

function hubLicenseTag(license) {
  if (!license) return "";
  const cls = HUB_COMMERCIAL.has(license)
    ? " is-commercial"
    : ["PC", "PC EA", "Questionable"].includes(license) || /NC/.test(license)
      ? " is-restricted"
      : "";
  return `<span class="lib-chip lib-license${cls}">${escapeHtml(license)}</span>`;
}

// An active download's pill; clicking it cancels that one download.
function hubJobPill(job) {
  const cancel = `data-hub-cancel-job="${job.id}" title="Click to cancel"`;
  if (job.status === "cancelling") return `<span class="lib-pill lib-pill-info">Cancelling…</span>`;
  return job.status === "queued"
    ? `<button type="button" class="lib-pill lib-pill-info hub-pulse hub-pill-cancel" ${cancel}><b>Queued</b><i>Cancel</i></button>`
    : `<button type="button" class="lib-pill hub-pill-progress hub-pill-cancel" ${cancel}><span style="width:${Math.max(2, job.percent || 0)}%"></span><b>${job.percent || 0}%</b><i>Cancel</i></button>`;
}

// Installed / Fallback (another version on disk; VaM uses it) / Download, plus
// the live download state and "Unavailable" once the Hub couldn't find it.
function hubDepPill(d, rid) {
  const inst = HUB.installs.get(String(rid));
  const stem = inst?.depJobs.get(d.ref) || (d.concrete ? hubStem(d.concrete).toLowerCase() : "");
  const job = stem
    ? [...state.downloads].reverse().find((j) => j.packageId && j.packageId.toLowerCase() === stem)
    : null;
  if (job && downloadJobActive(job)) return hubJobPill(job);
  if (d.installed) return `<span class="lib-pill lib-pill-ok">Installed</span>`;
  if (job && job.status === "failed") return `<span class="lib-pill lib-pill-err" title="${escapeAttribute(job.error || "")}">Failed</span>`;
  const attrs = `data-hub-dep-install="${escapeAttribute(d.ref)}" data-hub-rid="${escapeAttribute(String(rid))}"`;
  if (d.resolution === "fallback") {
    return d.url || d.packageName
      ? `<button type="button" class="lib-pill lib-pill-warn" ${attrs} title="Another version is installed and VaM will use it. Click to download this version.">Fallback</button>`
      : `<span class="lib-pill lib-pill-warn" title="Another version is installed and VaM will use it">Fallback</span>`;
  }
  if (inst?.unavailable.has(d.ref)) {
    return `<span class="lib-pill lib-pill-err" title="Not available on the Hub">Unavailable</span>`;
  }
  if (d.url || d.packageName) {
    return `<button type="button" class="lib-pill hub-pill-install" ${attrs} title="Download this dependency">Download</button>`;
  }
  return `<span class="lib-pill lib-pill-err">Missing</span>`;
}

function hubFileRowsHtml(detail, rid) {
  const files = Array.isArray(detail?.hubFiles) ? detail.hubFiles : [];
  return files
    .map((f) => {
      const stem = hubStem(f.filename);
      const installed = HUB.local.ids.has(stem.toLowerCase());
      const job = [...state.downloads].reverse().find((j) => j.packageId && j.packageId.toLowerCase() === stem.toLowerCase());
      let pill;
      if (job && downloadJobActive(job)) {
        pill = hubJobPill(job);
      } else if (installed) {
        pill = `<span class="lib-pill lib-pill-ok">Installed</span>`;
      } else if (hubFileUrl(f)) {
        pill = `<button type="button" class="lib-pill hub-pill-install" data-hub-file="${escapeAttribute(hubStr(f.filename))}" data-hub-rid="${escapeAttribute(String(rid))}" title="Download this file only">Download</button>`;
      } else {
        pill = `<span class="lib-pill lib-pill-err">No file</span>`;
      }
      return `<div class="lib-dep-row hub-file-row">
          <span class="lib-dep-ref is-resolved" title="${escapeAttribute(hubStr(f.filename))}">${escapeHtml(hubStr(f.filename))}</span>
          <span class="lib-dep-size">${hubNum(f.file_size) ? escapeHtml(formatBytesLocal(hubNum(f.file_size))) : ""}</span>
          ${pill}
        </div>`;
    })
    .join("");
}

function hubDetailHtml(r, detail) {
  const rid = String(r.resource_id);
  const d = detail || r;
  const type = hubStr(d.type);
  const category = hubStr(d.category);
  const license = hubLicense(d);
  const pinned = HUB.wishlist.has(rid);
  const support = hubStr(d.promotional_link);
  const supportUrl = support ? (/^https?:\/\//i.test(support) ? support : `https://${support}`) : "";
  const files = Array.isArray(detail?.hubFiles) ? detail.hubFiles : [];
  const deps = detail ? hubDependencyList(detail) : [];
  const depRows = deps.map(
    (dep) => `<div class="lib-dep-row">
        ${
          dep.resourceId
            ? `<button type="button" class="lib-dep-ref${dep.installed ? " is-resolved" : ""}" data-hub-open-rid="${escapeAttribute(dep.resourceId)}" title="${escapeAttribute(dep.ref)}">${escapeHtml(dep.ref)}</button>`
            : `<span class="lib-dep-ref" title="${escapeAttribute(dep.ref)}">${escapeHtml(dep.ref)}</span>`
        }
        <span class="lib-dep-size">${dep.size ? escapeHtml(formatBytesLocal(dep.size)) : ""}</span>
        ${hubDepPill(dep, rid)}
      </div>`,
  );
  const links = hubPageTabs(rid);
  return `
    <section class="lib-ds hub-detail-top">
      ${hubThumbHtml(d, "hub-hero")}</div>
      <div class="lib-title-line hub-detail-title">
        <span class="lib-dh-title" title="${escapeAttribute(hubStr(d.title))}">${escapeHtml(hubStr(d.title))}</span>
        ${hubStr(d.version_string) ? `<span class="lib-ver">${escapeHtml(hubStr(d.version_string))}</span>` : ""}
      </div>
      <button type="button" class="hub-author-card" data-hub-author="${escapeAttribute(hubStr(d.username))}" title="Show this author's packages">
        ${hubAvatarHtml(hubStr(d.username), d.icon_url, 32)}
        <span class="hub-author-text"><b>${escapeHtml(hubStr(d.username))}</b><small>Package author</small></span>
      </button>
      ${supportUrl ? `<button type="button" class="lib-link lib-support hub-support" data-hub-url="${escapeAttribute(supportUrl)}"><span class="material-symbols-outlined">favorite</span>Support this creator</button>` : ""}
      <div class="lib-dh-chips hub-badges">
        ${type ? `<span class="lib-chip lib-chip-type" style="background:${hubTypeColor(type)}cc">${escapeHtml(type)}</span>` : ""}
        ${
          category === "Free"
            ? `<span class="lib-chip lib-chip-type" style="background:#34d399cc">Free</span>`
            : category === "Paid"
              ? `<span class="lib-chip lib-chip-type" style="background:#fbbf24cc">Paid</span>`
              : category
                ? `<span class="lib-chip lib-chip-plain">${escapeHtml(category)}</span>`
                : ""
        }
        ${hubLicenseTag(license)}
      </div>
      ${hubStr(d.tag_line) ? `<p class="hub-tagline">${escapeHtml(hubStr(d.tag_line))}</p>` : ""}
      <div class="hub-stats-row">
        <span title="Downloads"><span class="material-symbols-outlined">download</span>${hubFormatNumber(d.download_count)}</span>
        <span title="${escapeAttribute(`${hubNum(d.rating_count)} ratings · ${hubNum(d.rating_avg).toFixed(1)} average · ${hubNum(d.rating_weighted).toFixed(1)} weighted`)}"><span class="material-symbols-outlined">star</span>${hubNum(d.rating_avg) ? hubNum(d.rating_avg).toFixed(1).replace(/\.0$/, "") : "—"}</span>
        <span title="Reaction score"><span class="material-symbols-outlined">thumb_up</span>${hubFormatNumber(d.reaction_score)}</span>
        <button type="button" class="hub-pin-inline${pinned ? " is-on" : ""}" data-hub-pin="${escapeAttribute(rid)}" title="${pinned ? "Remove from wishlist" : "Add to wishlist"}">
          <span class="material-symbols-outlined">push_pin</span></button>
      </div>
      <dl class="hub-dates">
        <dt><span class="material-symbols-outlined">event</span>Released</dt><dd>${escapeHtml(hubDate(d.resource_date))}</dd>
        <dt><span class="material-symbols-outlined">schedule</span>Updated</dt><dd>${escapeHtml(hubDate(d.last_update))}</dd>
      </dl>
      <div class="hub-detail-action" data-hub-detail-action="${escapeAttribute(rid)}">${hubActionHtml(d, { big: true })}</div>
      <div class="hub-open-row">
        <button type="button" class="lib-btn lib-btn-accent lib-btn-full hub-open-hub" data-hub-tab="overview" data-hub-rid="${escapeAttribute(rid)}" title="Show the resource's Hub page here in the app">
          <span class="material-symbols-outlined">web</span>Open on Hub
        </button>
        <button type="button" class="lib-icon-btn hub-open-browser" data-hub-browser-url="${escapeAttribute(hubResourceUrl(rid))}" title="Open in your web browser">
          <span class="material-symbols-outlined">open_in_new</span>
        </button>
      </div>
      <div class="hub-links">
        ${links
          .map((t) => `<button type="button" class="lib-small-link" data-hub-tab="${t.key}" data-hub-rid="${escapeAttribute(rid)}">${escapeHtml(t.label)}</button>`)
          .join("")}
      </div>
    </section>
    <section class="lib-ds">
      <div class="lib-group-head"><span class="lib-group-title">Package files <small>(${detail ? files.length : "…"})</small></span></div>
      ${
        !detail
          ? `<div class="lib-skeleton" style="width:80%"></div>`
          : files.length
            ? `<div class="lib-box">${hubFileRowsHtml(detail, rid)}</div>`
            : `<p class="lib-aside">The Hub lists no downloadable files for this resource.</p>`
      }
    </section>
    <section class="lib-ds">
      <div class="lib-group-head">
        <span class="lib-group-title">Dependencies <small>(${detail ? deps.length : hubNum(r.dependency_count) || "…"})</small></span>
        <span class="lib-group-tools">${
          detail && deps.some((x) => !x.installed)
            ? `<span class="lib-issue"><span class="material-symbols-outlined">warning</span>${deps.filter((x) => !x.installed).length} not installed</span>`
            : ""
        }</span>
      </div>
      ${
        !detail
          ? `<div class="lib-skeleton" style="width:70%"></div><div class="lib-skeleton" style="width:55%;margin-top:8px"></div>`
          : deps.length
            ? `<div class="lib-box">${libCollapsible(depRows, `hubdeps:${rid}`, depRows.length)}</div>`
            : `<p class="lib-aside">No dependencies</p>`
      }
    </section>`;
}

// The details panel, and the Hub page's info panel while that is open.
function hubDetailHosts() {
  return [$("hub-detail"), HUB_PAGE.rid ? $("hub-page-info") : null].filter(Boolean);
}

function hubRenderDetail() {
  const hosts = hubDetailHosts();
  if (!hosts.length) return;
  const rid = HUB.selected;
  const r = rid ? HUB.rows.get(rid) || HUB.wishlist.get(rid)?.snapshot || HUB.details.get(rid) : null;
  const html = !rid
    ? `<div class="lib-detail-empty"><span class="material-symbols-outlined">explore</span>Select a package to see its details</div>`
    : !r
      ? `<div class="lib-detail-empty"><span class="material-symbols-outlined">hourglass_empty</span>Loading…</div>`
      : hubDetailHtml(r, HUB.details.get(rid) || null);
  for (const host of hosts) host.innerHTML = html;
  if (HUB_PAGE.rid) hubPageRenderChrome();
}

function hubSyncDetailAction() {
  for (const host of hubDetailHosts()) {
    const slot = host.querySelector("[data-hub-detail-action]");
    if (!slot) continue;
    const rid = slot.getAttribute("data-hub-detail-action");
    const r = HUB.details.get(rid) || HUB.rows.get(rid) || HUB.wishlist.get(rid)?.snapshot;
    if (r) slot.innerHTML = hubActionHtml(r, { big: true });
  }
}

async function hubSelect(rid) {
  const key = String(rid);
  HUB.selected = key;
  document.querySelectorAll("#hub-grid .hub-card").forEach((c) =>
    c.classList.toggle("is-picked", c.getAttribute("data-hub-rid") === key),
  );
  hubRenderDetail();
  try {
    const detail = await hubGetDetail(key);
    if (!HUB.rows.has(key) && detail) HUB.rows.set(key, detail);
    if (HUB.selected === key) hubRenderDetail();
  } catch (e) {
    if (HUB.selected === key) {
      const host = $("hub-detail");
      const msg = `Couldn't load the details: ${escapeHtml(String(e?.message || e))}`;
      if (host?.querySelector(".lib-detail-empty")) {
        host.innerHTML = `<div class="lib-detail-empty"><span class="material-symbols-outlined">cloud_off</span>${msg}</div>`;
      } else {
        host?.insertAdjacentHTML("beforeend", `<section class="lib-ds"><p class="lib-desc">${msg}</p></section>`);
      }
    }
  }
}

// ---- Filters / actions ---------------------------------------------------------------------

function hubSetFilter(field, value) {
  HUB.filters[field] = value;
  hubSavePrefs();
  if (HUB.mode === "wishlist") {
    hubRenderGrid();
  } else {
    hubRenderFilters();
    hubLoadPage({ reset: true });
  }
}

function hubResetFilters() {
  HUB.filters = { ...HUB.filters, type: "All", pricing: "All", tags: [], author: "", license: "Any" };
  hubSavePrefs();
  if (HUB.mode === "wishlist") hubRenderGrid();
  else {
    hubRenderFilters();
    hubLoadPage({ reset: true });
  }
}

function hubSetMode(mode) {
  if (HUB.mode === mode) return;
  HUB.mode = mode;
  hubRenderFilters();
  if (mode === "hub" && !HUB.items.length) hubLoadPage({ reset: true });
  else hubRenderGrid();
}

async function hubRefresh() {
  HUB.refreshNext = true;
  HUB.details.clear();
  await Promise.all([hubLoadInfo(true), hubLoadLocal(true), hubLoadWishlist()]);
  if (HUB.mode === "hub") hubLoadPage({ reset: true });
  else hubRenderGrid();
}

function hubRunAction(btn) {
  const act = btn.getAttribute("data-hub-act");
  const rid = btn.getAttribute("data-hub-rid");
  const r = HUB.details.get(rid) || HUB.rows.get(rid) || HUB.wishlist.get(rid)?.snapshot;
  if (!r) return;
  if (act === "download") hubOpenDownloadPicker(rid);
  else if (act === "download-all") hubDownloadAll(rid);
  else if (act === "cancel") hubCancelDownloads(rid);
  else if (act === "library") hubShowInLibrary(r);
  else if (act === "external") hubOpenUrl(hubResourceState(r).url || hubResourceUrl(rid));
  else if (act === "open") hubPageOpen(rid);
}

// Thumbnails the webview can't load directly (hotlink rules) go through the
// Rust proxy once; anything still failing falls back to the gradient.
function hubOnImageError(event) {
  const img = event.target;
  if (!(img instanceof HTMLImageElement)) return;
  if (img.hasAttribute("data-hub-hide-on-error")) {
    img.remove();
    return;
  }
  const url = img.getAttribute("data-hub-img");
  if (!url || img.dataset.proxied || !invoke) {
    img.remove();
    return;
  }
  img.dataset.proxied = "1";
  invoke("hub_image", { url })
    .then((data) => {
      if (data && img.isConnected) img.src = data;
      else img.remove();
    })
    .catch(() => img.remove());
}

// ---- Hub page (embedded browser) ------------------------------------------------------
// After Backstage's HubDetail: over the whole Hub view, the resource's Hub
// pages, with the package info on the right (where the Hub view's details
// panel sits) — the Hub's own *-panel pages,
// with browser controls and Overview / Updates / Reviews / History /
// Discussion tabs. The browser is a native child webview (hub_embed_*) kept on
// #hub-page-frame; anything drawn over that area (modals, the Downloads
// popover, the console) hides it, since HTML can't render above it.

const HUB_PAGE = { rid: null, tab: "overview", url: "", loading: false, embedded: false, bounds: "", syncQueued: false };

function hubPageTabs(rid) {
  const d = HUB.details.get(rid) || HUB.rows.get(rid) || HUB.wishlist.get(rid)?.snapshot || {};
  const base = `https://hub.virtamate.com/resources/${encodeURIComponent(rid)}`;
  const tabs = [{ key: "overview", label: "Overview", url: `${base}/overview-panel` }];
  if (hubNum(d.update_count) > 0) tabs.push({ key: "updates", label: `Updates (${hubNum(d.update_count)})`, url: `${base}/updates-panel` });
  if (hubNum(d.review_count) > 0 || hubNum(d.rating_count) > 0) {
    tabs.push({ key: "reviews", label: `Reviews (${hubNum(d.review_count)})`, url: `${base}/review-panel` });
  }
  tabs.push({ key: "history", label: "History", url: `${base}/history-panel` });
  const thread = hubStr(d.discussion_thread_id);
  if (thread) {
    tabs.push({ key: "discussion", label: "Discussion", url: `https://hub.virtamate.com/threads/${encodeURIComponent(thread)}/discussion-panel` });
  }
  return tabs;
}

function hubPageTabUrl(rid, tab) {
  const tabs = hubPageTabs(rid);
  return (tabs.find((t) => t.key === tab) || tabs[0]).url;
}

// Which tab a page URL belongs to (panel or full-page form), or null.
function hubPageTabOf(url) {
  const m = /^https:\/\/hub\.virtamate\.com\/(resources|threads)\/[^/?#]+\/?([a-z-]*)/i.exec(hubStr(url));
  if (!m) return null;
  const part = m[2].toLowerCase().replace(/-panel$/, "");
  if (m[1].toLowerCase() === "threads") return "discussion";
  return { "": "overview", overview: "overview", updates: "updates", review: "reviews", reviews: "reviews", history: "history" }[part] ?? null;
}

// A *-panel URL as the full Hub page, for the address bar / copy / browser.
function hubFullUrl(url) {
  const m = /^(https:\/\/hub\.virtamate\.com\/(?:resources|threads)\/[^/?#]+)\/(overview|review|history|updates|discussion)-panel\/?$/i.exec(hubStr(url));
  if (!m) return hubStr(url);
  const tail = { overview: "/", discussion: "/", review: "/reviews", history: "/history", updates: "/updates" }[m[2].toLowerCase()];
  return m[1] + tail;
}

function hubPageRect() {
  const r = $("hub-page-frame")?.getBoundingClientRect();
  if (!r || r.width < 10 || r.height < 10) return null;
  return { x: Math.round(r.left), y: Math.round(r.top), width: Math.round(r.width), height: Math.round(r.height) };
}

function hubPageEmbedVisible() {
  if (!HUB_PAGE.rid) return false;
  if ($("hub-view")?.classList.contains("hidden") || $("hub-page")?.classList.contains("hidden")) return false;
  if (document.querySelector(".dialog-backdrop:not(.hidden), .image-zoom-backdrop:not(.hidden), .context-menu:not(.hidden)")) return false;
  if ($("downloads-popover")?.classList.contains("open") || $("console-overlay")?.classList.contains("open")) return false;
  // Dragging the panel edge: the webview would swallow the mouse.
  return !document.body.classList.contains("lib-resizing");
}

function hubPageSyncEmbed() {
  if (!HUB_PAGE.embedded) {
    document.body.classList.remove("hub-page-on");
    return;
  }
  const rect = hubPageEmbedVisible() ? hubPageRect() : null;
  document.body.classList.toggle("hub-page-on", Boolean(rect));
  if (rect) {
    // Toasts stay within the info panel: the browser covers everything left of it.
    const info = $("hub-page-info")?.getBoundingClientRect();
    if (info) document.body.style.setProperty("--hub-toast-w", `${Math.max(200, Math.round(info.width - 32))}px`);
  }
  const key = rect ? `${rect.x},${rect.y},${rect.width},${rect.height}` : "hidden";
  if (key === HUB_PAGE.bounds) return;
  HUB_PAGE.bounds = key;
  invoke("hub_embed_bounds", rect ? { ...rect, visible: true } : { x: 0, y: 0, width: 1, height: 1, visible: false }).catch((e) =>
    addLog(`Hub page: ${String(e)}`),
  );
}

function hubPageScheduleSync() {
  if (!HUB_PAGE.embedded || HUB_PAGE.syncQueued) return;
  HUB_PAGE.syncQueued = true;
  requestAnimationFrame(() => {
    HUB_PAGE.syncQueued = false;
    hubPageSyncEmbed();
  });
}

function hubPageRenderChrome() {
  const rid = HUB_PAGE.rid;
  if (!rid) return;
  $("hub-page-title").textContent = hubTitleOf(rid) || `Resource ${rid}`;
  $("hub-page-tabs").innerHTML = hubPageTabs(rid)
    .map(
      (t) =>
        `<button type="button" class="hub-page-tab${t.key === HUB_PAGE.tab ? " is-active" : ""}" role="tab" data-hub-page-tab="${t.key}">${escapeHtml(t.label)}</button>`,
    )
    .join("");
  const list = hubVisibleItems();
  const idx = list.findIndex((r) => String(r.resource_id) === rid);
  $("hub-page-pager")?.classList.toggle("hidden", idx < 0);
  if (idx >= 0) {
    $("hub-page-pos").textContent = `${idx + 1} / ${list.length}`;
    $("hub-page-prev").disabled = idx === 0;
    $("hub-page-next").disabled = idx >= list.length - 1;
  }
  const reload = $("hub-page-reload");
  if (reload) {
    reload.querySelector(".material-symbols-outlined").textContent = HUB_PAGE.loading ? "close" : "refresh";
    reload.title = HUB_PAGE.loading ? "Stop" : "Reload";
  }
  const address = $("hub-page-address");
  if (address && document.activeElement !== address) address.value = hubFullUrl(HUB_PAGE.url);
}

async function hubPageNavigate(url) {
  HUB_PAGE.url = url;
  HUB_PAGE.loading = true;
  hubPageRenderChrome();
  await new Promise((r) => requestAnimationFrame(r));
  const rect = hubPageRect() || { x: 0, y: 0, width: 1, height: 1 };
  try {
    await invoke("hub_embed_open", { url, ...rect });
    HUB_PAGE.embedded = true;
    HUB_PAGE.bounds = "";
    hubPageSyncEmbed();
  } catch (e) {
    addLog(`Hub page: ${String(e)} — opening in the browser instead`);
    hubOpenUrl(hubFullUrl(url));
    hubPageClose();
  }
}

// Open a resource's Hub page on `tab` (overview, updates, reviews, history,
// discussion).
function hubPageOpen(rid, tab = "overview") {
  if (!invoke || !rid) return;
  const key = String(rid);
  HUB_PAGE.rid = key;
  HUB_PAGE.tab = tab;
  $("hub-page")?.classList.remove("hidden");
  if (HUB.selected !== key) hubSelect(key);
  else hubRenderDetail();
  hubPageNavigate(hubPageTabUrl(key, tab));
}

function hubPageClose() {
  if (!HUB_PAGE.rid && !HUB_PAGE.embedded) return;
  HUB_PAGE.rid = null;
  HUB_PAGE.url = "";
  $("hub-page")?.classList.add("hidden");
  if (HUB_PAGE.embedded) {
    HUB_PAGE.embedded = false;
    HUB_PAGE.bounds = "";
    invoke("hub_embed_close").catch((e) => addLog(`Hub page: ${String(e)}`));
  }
  document.body.classList.remove("hub-page-on");
  hubRenderDetail();
}

function hubPageStep(delta) {
  const list = hubVisibleItems();
  const idx = list.findIndex((r) => String(r.resource_id) === HUB_PAGE.rid);
  const next = list[idx + delta];
  if (idx >= 0 && next) hubPageOpen(next.resource_id, "overview");
}

// Page loads inside the browser: address bar, tab, and — following a link to
// another resource — the info panel, as in Backstage.
function hubPageOnNav(event) {
  const url = hubStr(event?.payload?.url);
  if (!HUB_PAGE.rid || !/^https?:/i.test(url)) return;
  HUB_PAGE.url = url;
  HUB_PAGE.loading = Boolean(event.payload.loading);
  const m = /^https:\/\/hub\.virtamate\.com\/resources\/(?:[^/?#]*\.)?(\d+)(?:[/?#]|$)/i.exec(url);
  if (m && m[1] !== HUB_PAGE.rid) {
    HUB_PAGE.rid = m[1];
    hubSelect(m[1]);
  }
  HUB_PAGE.tab = hubPageTabOf(url) || "";
  hubPageRenderChrome();
}

function setupHubPage() {
  const page = $("hub-page");
  if (!page) return;
  page.addEventListener("error", hubOnImageError, true);
  $("hub-page-close")?.addEventListener("click", hubPageClose);
  $("hub-page-back")?.addEventListener("click", hubPageClose);
  $("hub-page-prev")?.addEventListener("click", () => hubPageStep(-1));
  $("hub-page-next")?.addEventListener("click", () => hubPageStep(1));
  $("hub-page-tabs")?.addEventListener("click", (e) => {
    const tab = e.target.closest("[data-hub-page-tab]");
    if (!tab || !HUB_PAGE.rid) return;
    HUB_PAGE.tab = tab.getAttribute("data-hub-page-tab");
    hubPageNavigate(hubPageTabUrl(HUB_PAGE.rid, HUB_PAGE.tab));
  });
  page.querySelector(".hub-page-toolbar")?.addEventListener("click", (e) => {
    const btn = e.target.closest("[data-hub-page-ctl]");
    if (!btn) return;
    const ctl = btn.getAttribute("data-hub-page-ctl");
    const full = hubFullUrl(HUB_PAGE.url);
    if (ctl === "external") hubOpenUrl(full);
    else if (ctl === "copy") navigator.clipboard?.writeText(full).then(() => hubToast("Link copied", "success")).catch(() => {});
    else {
      const action = ctl === "reload" && HUB_PAGE.loading ? "stop" : ctl;
      invoke("hub_embed_control", { action }).catch((err) => addLog(`Hub page: ${String(err)}`));
    }
  });
  const address = $("hub-page-address");
  address?.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      let url = address.value.trim();
      if (url && !/^[a-z]+:\/\//i.test(url)) url = `https://${url}`;
      if (/^https:\/\/hub\.virtamate\.com\//i.test(url)) hubPageNavigate(url);
      else if (url) hubOpenUrl(url);
      address.blur();
    } else if (e.key === "Escape") {
      e.preventDefault();
      address.value = hubFullUrl(HUB_PAGE.url);
      address.blur();
    }
  });
  address?.addEventListener("blur", () => {
    address.value = hubFullUrl(HUB_PAGE.url);
  });

  // Info panel width (left-edge drag) — the same width as the details panel.
  document.querySelector("[data-hub-page-resize]")?.addEventListener("mousedown", (event) => {
    event.preventDefault();
    const startX = event.clientX;
    const start = HUB.detailWidth;
    document.body.classList.add("lib-resizing");
    const onMove = (e) => {
      HUB.detailWidth = Math.min(500, Math.max(260, start - (e.clientX - startX)));
      hubApplyLayout();
    };
    const onUp = () => {
      document.body.classList.remove("lib-resizing");
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      hubSavePrefs();
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  });

  // Keep the webview on its placeholder, and out of the way of overlays.
  new MutationObserver(hubPageScheduleSync).observe(document.body, {
    attributes: true,
    subtree: true,
    attributeFilter: ["class"],
  });
  if (typeof ResizeObserver === "function") new ResizeObserver(hubPageScheduleSync).observe($("hub-page-frame"));
  window.addEventListener("resize", hubPageScheduleSync);
  window.__TAURI__?.event?.listen?.("hub-page-nav", hubPageOnNav);
}

function setupHubView() {
  hubLoadPrefs();
  setupHubDownloadPicker();
  setupHubPage();
  window.__TAURI__?.event?.listen?.("hub-page-download", (event) => {
    hubOnPageDownload(event).catch((e) => addLog(`Hub page download: ${String(e)}`));
  });
  const view = $("hub-view");
  if (!view) return;
  view.addEventListener("error", hubOnImageError, true);

  document.querySelectorAll("[data-hub-mode]").forEach((b) =>
    b.addEventListener("click", () => hubSetMode(b.getAttribute("data-hub-mode"))),
  );

  const search = $("hub-search");
  let timer = 0;
  search?.addEventListener("input", () => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      if (HUB.filters.search === search.value) return;
      hubSetFilter("search", search.value);
    }, 320);
  });
  search?.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      clearTimeout(timer);
      hubSetFilter("search", search.value);
    } else if (e.key === "Escape" && search.value) {
      search.value = "";
      clearTimeout(timer);
      hubSetFilter("search", "");
    }
  });
  $("hub-search-clear")?.addEventListener("click", () => {
    if (!search) return;
    search.value = "";
    hubSetFilter("search", "");
    search.focus();
  });

  const listFields = { "hub-type-list": "type", "hub-pricing-list": "pricing", "hub-license-list": "license" };
  for (const [id, field] of Object.entries(listFields)) {
    $(id)?.addEventListener("click", (e) => {
      const row = e.target.closest("[data-lib-value]");
      if (!row) return;
      const v = row.getAttribute("data-lib-value") || (field === "license" ? "Any" : "All");
      libCloseDropdowns();
      hubSetFilter(field, v);
    });
  }
  $("hub-sort-list")?.addEventListener("click", (e) => {
    const row = e.target.closest("[data-lib-value]");
    if (!row) return;
    libCloseDropdowns();
    const v = row.getAttribute("data-lib-value");
    if (HUB.mode === "wishlist") {
      HUB.wishSort = v;
      hubSavePrefs();
      hubRenderGrid();
    } else {
      hubSetFilter("sort", v);
    }
  });

  // Tags: chip input with suggestions; Enter or comma commits.
  const tagInput = $("hub-tag-input");
  const addTag = (t) => {
    const tag = String(t || "").trim().replace(/,+$/, "");
    if (!tag || HUB.filters.tags.some((x) => x.toLowerCase() === tag.toLowerCase())) return;
    if (tagInput) tagInput.value = "";
    HUB_AC.tag = -1;
    hubSetFilter("tags", [...HUB.filters.tags, tag]);
  };
  tagInput?.addEventListener("input", () => {
    if (tagInput.value.includes(",")) {
      addTag(tagInput.value.split(",")[0]);
      return;
    }
    HUB_AC.tag = -1;
    hubRenderSuggestions("tag");
  });
  tagInput?.addEventListener("keydown", (e) => {
    const n = HUB_AC.tagMatches.length;
    if (e.key === "ArrowDown" && n) {
      e.preventDefault();
      HUB_AC.tag = (HUB_AC.tag + 1) % n;
      hubRenderSuggestions("tag");
    } else if (e.key === "ArrowUp" && n) {
      e.preventDefault();
      HUB_AC.tag = (HUB_AC.tag - 1 + n) % n;
      hubRenderSuggestions("tag");
    } else if (e.key === "Enter") {
      e.preventDefault();
      addTag(HUB_AC.tagMatches[HUB_AC.tag] ?? tagInput.value);
    }
  });
  $("hub-tag-list")?.addEventListener("click", (e) => {
    const opt = e.target.closest("[data-hub-pick-tag]");
    if (opt) addTag(opt.getAttribute("data-hub-pick-tag"));
  });
  $("hub-tag-chips")?.addEventListener("click", (e) => {
    const rm = e.target.closest("[data-hub-tag-remove]");
    if (rm) hubSetFilter("tags", HUB.filters.tags.filter((t) => t !== rm.getAttribute("data-hub-tag-remove")));
    else if (e.target.closest("[data-hub-tags-clear]")) hubSetFilter("tags", []);
  });

  // Author: autocomplete from getInfo.users.
  const authorInput = $("hub-author-input");
  const pickAuthor = (name) => {
    if (authorInput) authorInput.value = "";
    HUB_AC.author = -1;
    libCloseDropdowns();
    hubSetFilter("author", String(name || "").trim());
  };
  authorInput?.addEventListener("input", () => {
    HUB_AC.author = -1;
    hubRenderSuggestions("author");
  });
  authorInput?.addEventListener("keydown", (e) => {
    const n = HUB_AC.authorMatches.length;
    if (e.key === "ArrowDown" && n) {
      e.preventDefault();
      HUB_AC.author = (HUB_AC.author + 1) % n;
      hubRenderSuggestions("author");
    } else if (e.key === "ArrowUp" && n) {
      e.preventDefault();
      HUB_AC.author = (HUB_AC.author - 1 + n) % n;
      hubRenderSuggestions("author");
    } else if (e.key === "Enter") {
      e.preventDefault();
      const pick = HUB_AC.authorMatches[HUB_AC.author] ?? authorInput.value;
      if (String(pick).trim()) pickAuthor(pick);
    }
  });
  $("hub-author-list")?.addEventListener("click", (e) => {
    const opt = e.target.closest("[data-hub-pick-author]");
    if (opt) pickAuthor(opt.getAttribute("data-hub-pick-author"));
  });
  $("hub-author-chips")?.addEventListener("click", (e) => {
    if (e.target.closest("[data-hub-author-clear]")) pickAuthor("");
  });

  $("hub-filter-reset")?.addEventListener("click", hubResetFilters);
  $("hub-refresh")?.addEventListener("click", () => hubRefresh().catch((e) => addLog(`Hub: ${String(e)}`)));
  $("hub-error-retry")?.addEventListener("click", () => {
    HUB.error = null;
    hubLoadInfo();
    hubLoadPage({ reset: true });
  });
  document.querySelectorAll("[data-hub-view]").forEach((b) =>
    b.addEventListener("click", () => {
      HUB.view = b.getAttribute("data-hub-view");
      hubSavePrefs();
      hubRenderGrid();
    }),
  );
  $("hub-size-slider")?.addEventListener("input", (e) => {
    const scroll = $("hub-scroll");
    const avail = Math.max(0, (scroll?.clientWidth || 0) - 32);
    const n = Math.max(1, Number(e.target.value) || 1);
    HUB.cardWidth = Math.max(100, Math.min(500, Math.floor((avail - (n - 1) * HUB_GAP) / n)));
    hubSavePrefs();
    hubApplyLayout();
  });

  $("hub-load-more")?.addEventListener("click", (e) => {
    if (!e.target.closest("[data-hub-retry-more]")) return;
    HUB.error = null;
    hubLoadPage();
  });

  const scroll = $("hub-scroll");
  scroll?.addEventListener("scroll", () => hubMaybeLoadMore(), { passive: true });
  if (scroll && typeof ResizeObserver === "function") new ResizeObserver(() => hubApplyLayout()).observe(scroll);

  // Grid clicks: pin, author, action buttons, then the card itself.
  $("hub-grid")?.addEventListener("click", (e) => {
    const pin = e.target.closest("[data-hub-pin]");
    if (pin) {
      e.stopPropagation();
      hubToggleWishlist(pin.getAttribute("data-hub-pin"));
      return;
    }
    const author = e.target.closest("[data-hub-author]");
    if (author) {
      e.stopPropagation();
      hubSetFilter("author", author.getAttribute("data-hub-author"));
      return;
    }
    const act = e.target.closest("[data-hub-act]");
    if (act) {
      e.stopPropagation();
      hubRunAction(act);
      return;
    }
    const card = e.target.closest("[data-hub-rid]");
    if (card) hubSelect(card.getAttribute("data-hub-rid"));
  });
  $("hub-grid")?.addEventListener("dblclick", (e) => {
    const card = e.target.closest(".hub-card[data-hub-rid]");
    if (card && !e.target.closest("button")) hubPageOpen(card.getAttribute("data-hub-rid"));
  });

  const onDetailClick = (e) => {
    const url = e.target.closest("[data-hub-url]");
    if (url) return hubOpenUrl(url.getAttribute("data-hub-url"));
    const tab = e.target.closest("[data-hub-tab]");
    if (tab) return hubPageOpen(tab.getAttribute("data-hub-rid"), tab.getAttribute("data-hub-tab"));
    const cancelJob = e.target.closest("[data-hub-cancel-job]");
    if (cancelJob) return cancelDownload(Number(cancelJob.getAttribute("data-hub-cancel-job")));
    const browser = e.target.closest("[data-hub-browser-url]");
    if (browser) return hubOpenUrl(browser.getAttribute("data-hub-browser-url"));
    const pin = e.target.closest("[data-hub-pin]");
    if (pin) return hubToggleWishlist(pin.getAttribute("data-hub-pin"));
    const author = e.target.closest("[data-hub-author]");
    if (author) return hubSetFilter("author", author.getAttribute("data-hub-author"));
    const dep = e.target.closest("[data-hub-dep-install]");
    if (dep) return hubDownload(dep.getAttribute("data-hub-rid"), { files: [], depRefs: [dep.getAttribute("data-hub-dep-install")] });
    const file = e.target.closest("[data-hub-file]");
    if (file) return hubDownload(file.getAttribute("data-hub-rid"), { files: [file.getAttribute("data-hub-file")] });
    const act = e.target.closest("[data-hub-act]");
    if (act) return hubRunAction(act);
    const open = e.target.closest("[data-hub-open-rid]");
    if (open) return hubSelect(open.getAttribute("data-hub-open-rid"));
    const expand = e.target.closest("[data-lib-expand]");
    if (expand) {
      const k = expand.getAttribute("data-lib-expand");
      if (LIB_DETAILS.expanded.has(k)) LIB_DETAILS.expanded.delete(k);
      else LIB_DETAILS.expanded.add(k);
      hubRenderDetail();
    }
    return undefined;
  };
  $("hub-detail")?.addEventListener("click", onDetailClick);
  $("hub-page-info")?.addEventListener("click", onDetailClick);

  // Details panel width (left-edge drag), remembered.
  document.querySelector("[data-hub-resize]")?.addEventListener("mousedown", (event) => {
    event.preventDefault();
    const startX = event.clientX;
    const start = HUB.detailWidth;
    document.body.classList.add("lib-resizing");
    const onMove = (e) => {
      HUB.detailWidth = Math.min(500, Math.max(260, start - (e.clientX - startX)));
      hubApplyLayout();
    };
    const onUp = () => {
      document.body.classList.remove("lib-resizing");
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      hubSavePrefs();
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  });

  hubRenderFilters();
  hubRenderGrid();
  hubRenderDetail();
}

// Sidebar entry: first visit loads filters, local packages, wishlist and page 1.
window.__refreshHubView = () => {
  if (!invoke) return;
  hubApplyLayout();
  if (!HUB.opened) {
    HUB.opened = true;
    // The default sort comes from getInfo; a remembered one needn't wait for it.
    const info = hubLoadInfo();
    hubLoadLocal();
    hubLoadWishlist().then(() => hubRenderGrid());
    if (HUB.mode === "hub") {
      if (HUB.filters.sort) hubLoadPage({ reset: true });
      else info.finally(() => hubLoadPage({ reset: true }));
    }
  } else {
    hubLoadLocal(true);
    hubRenderGrid();
  }
};

window.addEventListener("DOMContentLoaded", async () => {
  await initConfig();
  // Pull the persisted blocked-creators set so render-time filters honor
  // previous "Disable creator" choices from the moment the UI mounts.
  await loadBlockedCreators();
  await loadFavorites();
  hideProgress();
  setButtonsBusy(false);
  syncReplaceOptions();
  // Clean VARs (db-find) is the sole workspace and the landing page. Seed its
  // state slot from the freshly-loaded config so it opens with the saved
  // input/output/target paths. applyConfigToInputs already populated the
  // dbf-* inputs; snapshotCurrentPage() captures them into the db-find slot.
  state.currentPage = "db-find";
  snapshotCurrentPage();

  $("theme-toggle").addEventListener("click", async () => {
    const idx = THEMES.indexOf(state.theme);
    state.theme = THEMES[(idx + 1) % THEMES.length] ?? "dark";
    updateStaticCopy();
    await persistLanguage();
  });

  $("scan-button")?.addEventListener("click", async () => {
    try {
      await startScan();
    } catch (error) {
      addLog(String(error));
      hideProgress();
      await clearActiveTask();
    }
  });

  // Find Duplicates' DB-source filter handler is bound by bindDbFindEvents
  // on the dbf-source-filter element. The legacy dbfind-source-filter input
  // is no longer rendered (the element lived in the now-Overview-only DOM).

  const buildDbScanBtn = $("build-db-scan-button");
  if (buildDbScanBtn) {
    buildDbScanBtn.addEventListener("click", async () => {
      try {
        await startBuildDbScan();
      } catch (error) {
        addLog(String(error));
        hideProgress();
        await clearActiveTask();
      }
    });
  }

  setupDownloadLinksImport();
  setupDownloadsManager();

  setupVamDir();
  setupLibraryView();
  setupDatabasePackages();
  setupHubView();

  const vdBack = $("var-details-back");
  if (vdBack) {
    vdBack.addEventListener("click", () => closeVarDetailsView());
  }

  const vdFav = $("var-details-fav-button");
  if (vdFav) {
    vdFav.addEventListener("click", () => {
      const pid = state.varDetails?.item?.package_id;
      if (pid) togglePackageFavorite(pid);
    });
  }

  // "Download this VAR" availability banner — delegated (innerHTML is rebuilt).
  const vdAvail = $("var-details-availability");
  if (vdAvail) {
    vdAvail.addEventListener("click", (event) => {
      const btn = event.target.closest("[data-vda]");
      if (!btn) return;
      const action = btn.getAttribute("data-vda");
      if (action === "download") {
        downloadVarItself().catch((e) => addLog(`VAR Details: ${String(e)}`));
      } else if (action === "open-downloads") {
        if (window.__toggleDownloads) window.__toggleDownloads();
      } else if (action === "open") {
        const url = state.varDetails.availability.url;
        if (url && invoke) invoke("open_url", { url }).catch((e) => addLog(`VAR Details: ${String(e)}`));
      }
    });
  }

  const vdSearch = $("var-details-search");
  if (vdSearch) {
    let vdSearchTimer = null;
    vdSearch.addEventListener("input", () => {
      const next = vdSearch.value;
      if (vdSearchTimer) clearTimeout(vdSearchTimer);
      vdSearchTimer = setTimeout(() => {
        if (state.varDetails.search === next) return;
        state.varDetails.search = next;
        state.varDetails.assetPage = 0;
        renderVarDetailsAssetTable();
      }, 120);
    });
  }

  const vdScanDb = $("var-details-scan-db-button");
  if (vdScanDb) {
    vdScanDb.addEventListener("click", () => {
      runVarDetailsAnalysis("database").catch((error) => {
        addLog(`VAR Details: scan error — ${String(error)}`);
        state.varDetails.scanning = false;
        state.varDetails.scanKind = null;
        renderVarDetails();
      });
    });
  }

  // Resource table row click → toggle the inline preview panel for the row's
  // internal path. Only previewable rows (.vam/.png/.jp(e)g) on a local VAR
  // produce a payload; the renderer hides itself in every other case.
  const vdTbody = $("var-details-tbody");
  if (vdTbody) {
    vdTbody.addEventListener("click", (event) => {
      const tr = event.target.closest("tr[data-resource-path]");
      if (!tr || !tr.classList.contains("is-previewable")) return;
      const path = tr.getAttribute("data-resource-path") || "";
      if (!path) return;
      state.varDetails.selectedResourceKey =
        state.varDetails.selectedResourceKey === path ? null : path;
      renderVarDetailsResourceTable();
      renderVarDetailsResourcePreview();
    });
  }

  const closeVdResourcePreview = () => {
    if (state.varDetails.selectedResourceKey == null) return;
    state.varDetails.selectedResourceKey = null;
    renderVarDetailsResourceTable();
    renderVarDetailsResourcePreview();
  };

  // Close handlers: X button (delegated inside the sheet), backdrop click, and
  // ESC. Backdrop also has `data-vd-preview-close` so the same selector hits.
  document.addEventListener("click", (event) => {
    if (!event.target.closest("[data-vd-preview-close]")) return;
    closeVdResourcePreview();
  });
  document.addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    if (state.varDetails.selectedResourceKey == null) return;
    closeVdResourcePreview();
  });

  setupImageZoom();

  const vdScanLocal = $("var-details-scan-local-button");
  if (vdScanLocal) {
    vdScanLocal.addEventListener("click", () => {
      runVarDetailsAnalysis("local").catch((error) => {
        addLog(`VAR Details: scan error — ${String(error)}`);
        state.varDetails.scanning = false;
        state.varDetails.scanKind = null;
        renderVarDetails();
      });
    });
  }

  const vdFolderInput = $("var-details-folder-input");
  if (vdFolderInput) {
    // Pre-fill from the global config's input_dir (already loaded into
    // #input-dir at startup).
    const seed = $("input-dir")?.value?.trim() ?? "";
    if (seed && !vdFolderInput.value) {
      vdFolderInput.value = seed;
      state.varDetails.inputDir = seed;
    }
    vdFolderInput.addEventListener("input", () => {
      state.varDetails.inputDir = vdFolderInput.value;
    });
    vdFolderInput.addEventListener("change", async () => {
      const trimmed = vdFolderInput.value.trim();
      state.varDetails.inputDir = trimmed;
      const globalInput = $("input-dir");
      if (globalInput && globalInput.value.trim() !== trimmed) {
        globalInput.value = trimmed;
      }
      try {
        if (invoke) await invoke("save_config", { config: buildCurrentConfig() });
      } catch (_error) {}
    });
  }

  const vdPathAction = $("var-details-meta-path-action");
  if (vdPathAction) {
    vdPathAction.addEventListener("click", async (event) => {
      event.stopPropagation();
      const path = vdPathAction.dataset.filePath || state.varDetails.item?.file_path || "";
      if (!path) return;
      try {
        await showPackageInExplorer(path);
      } catch (error) {
        addLog(`VAR Details: ${String(error)}`);
      }
    });
  }

  const sendVarDetailsToTargetVar = (page) => {
    sendVarToTargetPage(page, state.varDetails.item?.file_path || "");
  };

  const vdSendDbFind = $("var-details-send-dbfind");
  if (vdSendDbFind) {
    vdSendDbFind.addEventListener("click", (event) => {
      event.stopPropagation();
      sendVarDetailsToTargetVar("db-find");
    });
  }
  const vdSendMissing = $("var-details-send-missing-button");
  if (vdSendMissing) {
    vdSendMissing.addEventListener("click", (event) => {
      event.stopPropagation();
      sendVarDetailsToTargetVar("missing-resources");
    });
  }
  const vdSendInternalize = $("var-details-send-internalize-button");
  if (vdSendInternalize) {
    vdSendInternalize.addEventListener("click", (event) => {
      event.stopPropagation();
      sendVarDetailsToTargetVar("internalize-resources");
    });
  }

  const vdFolderPick = $("var-details-folder-pick");
  if (vdFolderPick) {
    vdFolderPick.addEventListener("click", async () => {
      if (!invoke) return;
      try {
        const selected = await invoke("pick_folder");
        if (!selected) return;
        const input = $("var-details-folder-input");
        if (input) input.value = selected;
        state.varDetails.inputDir = selected;
        const globalInput = $("input-dir");
        if (globalInput) globalInput.value = selected;
        try {
          await invoke("save_config", { config: buildCurrentConfig() });
        } catch (_error) {}
      } catch (error) {
        addLog(`VAR Details: folder picker failed — ${String(error)}`);
      }
    });
  }

  // Top Sharing Packages: clicking the row body toggles its expansion to
  // show shared resources, while a separate arrow button navigates into
  // the package's own VAR Details view.
  const vdTopList = $("var-details-top-packages-list");
  if (vdTopList) {
    vdTopList.addEventListener("click", async (event) => {
      const li = event.target.closest("[data-package-id]");
      if (!li) return;
      const packageId = li.getAttribute("data-package-id");
      const filePath = li.getAttribute("data-file-path");
      if (!packageId) return;

      const openBtn = event.target.closest("[data-vd-open]");
      const toggleBtn = event.target.closest("[data-vd-toggle]");

      if (openBtn) {
        event.stopPropagation();
        if (state.varDetails.assetSource === "local" && filePath) {
          showVarDetailsView();
          await loadVarDetailsFromPath(filePath);
          return;
        }
        const cached = (state.varPackagesItems ?? []).find((it) => it.package_id === packageId);
        if (cached) {
          openVarDetailsView(cached, "folder");
          return;
        }
        const fileName = filePath ? filePath.split(/[\\/]/).pop() : `${packageId}.var`;
        const synthetic = {
          package_id: packageId,
          file_name: fileName,
          file_path: filePath ?? "",
          creator: deriveCreatorFromPackageId(packageId),
          size_bytes: 0,
          modified_ms: null,
          indexed: true,
          scene_image_data: null,
        };
        openVarDetailsView(synthetic, "db");
        return;
      }

      if (toggleBtn || event.target.closest(".var-details-top-package")) {
        const set = state.varDetails.expandedPackages ?? new Set();
        if (set.has(packageId)) set.delete(packageId);
        else set.add(packageId);
        state.varDetails.expandedPackages = set;
        renderVarDetailsTopPackages();
      }
    });

    vdTopList.addEventListener("contextmenu", (event) => {
      if (state.varDetails.assetSource !== "local") return;
      const li = event.target.closest("[data-package-id]");
      if (!li) return;
      const filePath = li.getAttribute("data-file-path");
      if (!filePath) return;
      event.preventDefault();
      hideContextMenu();
      showContextMenu(event.clientX, event.clientY, [
        {
          label: t("menuShowInExplorer"),
          action: () => showPackageInExplorer(filePath),
        },
      ]);
    });
  }

  const vdPagePrev = $("var-details-page-prev");
  if (vdPagePrev) {
    vdPagePrev.addEventListener("click", () => {
      if (state.varDetails.assetPage <= 0) return;
      state.varDetails.assetPage -= 1;
      renderVarDetailsAssetTable();
    });
  }
  const vdPageNext = $("var-details-page-next");
  if (vdPageNext) {
    vdPageNext.addEventListener("click", () => {
      state.varDetails.assetPage += 1;
      renderVarDetailsAssetTable();
    });
  }

  // Resource filter toggle (All / Shared). Resets pagination on switch since
  // the filtered row count usually changes substantially.
  document.querySelectorAll("[data-resource-filter]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const next = btn.getAttribute("data-resource-filter");
      if (!next || state.varDetails.resourceFilter === next) return;
      state.varDetails.resourceFilter = next;
      state.varDetails.assetPage = 0;
      renderVarDetailsResourceTable();
    });
  });

  // Top Sharing Packages sort toggle (By Count / By Size). Only re-renders
  // the top-packages list — the resource table is unaffected.
  document.querySelectorAll("[data-top-sort]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const next = btn.getAttribute("data-top-sort");
      if (!next || state.varDetails.topPackagesSortBy === next) return;
      state.varDetails.topPackagesSortBy = next;
      renderVarDetailsTopPackages();
    });
  });

  // Creator flag toggles (favorite / blocked). Clicking the active button
  // again clears the flag back to 0 (none). Persisted on the creator row so
  // it applies the next time the reclaim scan runs.
  document.querySelectorAll("[data-creator-flag]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const item = state.varDetails.item;
      if (!item?.creator || !invoke) return;
      const targetFlag = btn.getAttribute("data-creator-flag") === "favorite" ? 1 : 2;
      const current = Number(item.creatorFlag) || 0;
      const next = current === targetFlag ? 0 : targetFlag;
      try {
        await invoke("set_creator_flag", {
          creatorName: item.creator,
          flag: next,
        });
        item.creatorFlag = next;
        // Keep the module-level mirrors honest without a restart. Flag values
        // are mutually exclusive, so a flip to any value clears the other set.
        // (_blockedCreators was previously only updated by the context-menu
        // "Disable creator" path — render-time blocked filtering lagged when
        // the flag was toggled from here.)
        if (next === 1) _favoriteCreators.add(item.creator);
        else _favoriteCreators.delete(item.creator);
        if (next === 2) _blockedCreators.add(item.creator);
        else _blockedCreators.delete(item.creator);
        renderCreatorFlagButtons(item);
      } catch (error) {
        addLog(`Creator flag update failed — ${String(error)}`);
      }
    });
  });

  const pickVarFromDialog = async () => {
    if (!invoke) return;
    try {
      const selected = await invoke("pick_var_file");
      if (selected) await loadVarDetailsFromPath(selected);
    } catch (error) {
      addLog(`VAR Details: file picker failed — ${String(error)}`);
    }
  };

  const vdSelectFiles = $("var-details-select-files");
  if (vdSelectFiles) {
    vdSelectFiles.addEventListener("click", (event) => {
      event.stopPropagation();
      pickVarFromDialog();
    });
  }

  // Text input for typing/pasting a .var path directly. Loads on change/blur
  // (not on every keystroke, since path entry may be partial mid-typing) and
  // also on Enter so power users can paste-and-go.
  const vdPackageInput = $("var-details-package-input");
  const vdPackagePick = $("var-details-package-pick");
  const tryLoadFromPackageInput = async () => {
    const value = vdPackageInput?.value?.trim();
    if (!value) return;
    if (state.varDetails.item?.file_path === value) return;
    await loadVarDetailsFromPath(value);
  };
  if (vdPackageInput) {
    vdPackageInput.addEventListener("change", () => {
      tryLoadFromPackageInput();
    });
    vdPackageInput.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        tryLoadFromPackageInput();
      }
    });
  }
  if (vdPackagePick) {
    vdPackagePick.addEventListener("click", () => {
      pickVarFromDialog();
    });
  }

  const vdBrowseDir = $("var-details-browse-dir");
  if (vdBrowseDir) {
    vdBrowseDir.addEventListener("click", (event) => {
      event.stopPropagation();
      // "Browse Directory" jumps to VAR Packages so the user can pick from a
      // full folder/database listing — that's the canonical browser, no need
      // to duplicate it here.
      const target = document.querySelector('[data-sidebar-link="var-packages"]');
      if (target) target.click();
    });
  }

  const vdDropzone = $("var-details-dropzone");
  if (vdDropzone) {
    vdDropzone.addEventListener("click", () => pickVarFromDialog());
    // OS-level drag-over feedback. The actual file path arrives via the
    // Tauri window event below — HTML5 drop on the webview can't read paths
    // reliably, so we just paint the visual hover state here.
    ["dragenter", "dragover"].forEach((evt) => {
      vdDropzone.addEventListener(evt, (e) => {
        e.preventDefault();
        vdDropzone.classList.add("is-dragover");
      });
    });
    ["dragleave", "drop"].forEach((evt) => {
      vdDropzone.addEventListener(evt, (e) => {
        e.preventDefault();
        vdDropzone.classList.remove("is-dragover");
      });
    });
  }

  // Tauri 2 emits `tauri://drag-drop` on the window with `{ paths, position }`.
  // We accept the first .var path when the VAR Details view is visible.
  const tauriEvent = window.__TAURI__?.event;
  if (tauriEvent && typeof tauriEvent.listen === "function") {
    const handleDrop = (event) => {
      const detailsView = $("var-details-view");
      if (!detailsView || detailsView.classList.contains("hidden")) return;
      const paths = event?.payload?.paths ?? [];
      const varPath = paths.find((p) => /\.var$/i.test(String(p)));
      if (!varPath) {
        if (paths.length > 0) {
          const message = "Only .var files are supported here.";
          addLog(`VAR Details: ${message}`);
          state.varDetails.resourcesError = message;
          renderVarDetails();
        }
        return;
      }
      loadVarDetailsFromPath(varPath);
    };
    const dragoverPaint = (active) => {
      const dz = $("var-details-dropzone");
      if (!dz) return;
      dz.classList.toggle("is-dragover", active);
    };
    tauriEvent.listen("tauri://drag-drop", (event) => {
      dragoverPaint(false);
      handleDrop(event);
    }).catch(() => {});
    tauriEvent.listen("tauri://drag-enter", () => dragoverPaint(true)).catch(() => {});
    tauriEvent.listen("tauri://drag-over", () => dragoverPaint(true)).catch(() => {});
    tauriEvent.listen("tauri://drag-leave", () => dragoverPaint(false)).catch(() => {});
  }

  const bulkDropzone = $("build-db-bulk-dropzone");
  if (bulkDropzone) {
    bulkDropzone.addEventListener("click", pickBulkImportFile);
    bulkDropzone.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        pickBulkImportFile();
      }
    });
  }
  const bulkCancelBtn = $("build-db-bulk-cancel-button");
  if (bulkCancelBtn) {
    bulkCancelBtn.addEventListener("click", () => setBulkImportSelected(""));
  }
  const bulkStartBtn = $("build-db-bulk-start-button");
  if (bulkStartBtn) {
    bulkStartBtn.addEventListener("click", async () => {
      try {
        await startBulkImport();
      } catch (error) {
        addLog(String(error));
        hideProgress();
        await clearActiveTask();
      }
    });
  }

  const backfillInput = $("build-db-backfill-input");
  if (backfillInput) {
    const seed = $("input-dir")?.value?.trim() ?? "";
    if (seed && !backfillInput.value) backfillInput.value = seed;
  }
  const backfillPick = $("build-db-backfill-pick");
  if (backfillPick) {
    backfillPick.addEventListener("click", async () => {
      if (!invoke) return;
      try {
        const selected = await invoke("pick_folder");
        if (selected && backfillInput) backfillInput.value = selected;
      } catch (error) {
        addLog(`Backfill: folder picker failed — ${String(error)}`);
      }
    });
  }
  const backfillStart = $("build-db-backfill-start");
  if (backfillStart) {
    backfillStart.addEventListener("click", () => {
      startBuildDbBackfill().catch((error) => {
        addLog(`Backfill: ${String(error)}`);
        finalizeBackfillUi(null, String(error));
      });
    });
  }
  const backfillCancel = $("build-db-backfill-cancel");
  if (backfillCancel) {
    backfillCancel.addEventListener("click", async () => {
      const taskId = state.buildDbBackfill?.taskId;
      if (!taskId || !invoke) return;
      state.buildDbBackfill.cancelRequested = true;
      const status = $("build-db-backfill-progress-status");
      if (status) status.textContent = "Cancelling…";
      try {
        await invoke("cancel_task", { taskId });
      } catch (error) {
        addLog(`Backfill: cancel failed — ${String(error)}`);
      }
    });
  }

  $("run-button")?.addEventListener("click", async () => {
    try {
      await startExecute();
    } catch (error) {
      addLog(String(error));
      hideProgress();
      await clearActiveTask();
    }
  });

  $("auto-target-button")?.addEventListener("click", async () => {
    const button = $("auto-target-button");
    button.disabled = true;
    try {
      await applyAutoTargetToScopedGroups();
    } catch (error) {
      addLog(String(error));
    } finally {
      // Re-derive disabled state via renderDetail below; this is a safety
      // net in case renderDetail isn't triggered (e.g. nothing to assign).
      button.disabled = false;
      renderDetail();
    }
  });

  // Trailing-edge debounced render so rapid typing doesn't trigger a
  // full sort+filter+innerHTML rebuild per keystroke. Page reset and
  // context-menu hide stay synchronous so the UI feels responsive even
  // before the list catches up.
  const _renderGroupsAndDetailDebounced = debounce(() => {
    renderGroups();
    // `renderGroups` runs `normalizeSelections`, which may flip
    // `state.selectedKey` to a different group when the user's prior
    // selection drops out of the filtered view. The detail panel — which
    // also owns the Apply-to-{scope,filtered,global} buttons' enable state
    // — won't refresh on its own, so it would otherwise display the old
    // group's keep choice (and stale `filteredKeys.length` for the button
    // disable check). Without this call the user can land on a state where
    // "Apply to filtered groups" stays disabled forever despite the
    // filtered list having matches.
    renderDetail();
  }, 100);
  $("group-filter")?.addEventListener("input", () => {
    hideContextMenu();
    state.groupPage = 0;
    _renderGroupsAndDetailDebounced();
  });

  $("pick-input-button")?.addEventListener("click", async () => {
    try {
      await chooseFolder("input-dir");
    } catch (error) {
      addLog(String(error));
    }
  });

  initAdditionalDirSections();

  $("pick-output-button")?.addEventListener("click", async () => {
    try {
      await chooseFolder("output-dir");
    } catch (error) {
      addLog(String(error));
    }
  });

  $("open-output-button")?.addEventListener("click", async () => {
    try {
      await showOutputInExplorer();
    } catch (error) {
      addLog(String(error));
    }
  });

  $("pick-vap-button")?.addEventListener("click", async () => {
    try {
      await chooseFolder("vap-dir");
    } catch (error) {
      addLog(String(error));
    }
  });

  $("pick-target-var-button")?.addEventListener("click", async () => {
    try {
      await chooseVarFile();
    } catch (error) {
      addLog(String(error));
    }
  });

  $("input-dir")?.addEventListener(
    "input",
    debounce(() => {
      syncOutputDirFromInput();
    }, 150)
  );

  $("process-vap")?.addEventListener("change", () => {
    state.processVap = $("process-vap").checked;
    setButtonsBusy(Boolean(state.activeTask));
    syncReplaceOptions();
  });

  $("target-var-path")?.addEventListener(
    "input",
    debounce(() => {
      handleTargetVarChange($("target-var-path").value);
    }, 150)
  );

  $("output-dir")?.addEventListener(
    "input",
    debounce(() => {
      const derived = deriveOutputDir($("input-dir").value);
      state.outputDirAutoSynced = $("output-dir").value.trim() === derived;
      setButtonsBusy(Boolean(state.activeTask));
    }, 150)
  );

  $("replace-in-place")?.addEventListener("change", () => {
    if ($("replace-in-place").checked) {
      $("backup-changed").checked = true;
    }
    syncReplaceOptions();
    renderSummary();
  });

  $("backup-changed")?.addEventListener("change", () => {
    syncReplaceOptions();
    renderSummary();
  });

  document.addEventListener("click", (event) => {
    if (!event.target.closest(".context-menu")) {
      hideContextMenu();
    }
  });

  document.addEventListener("keydown", (event) => {
    // One Escape closes exactly ONE modal — the topmost. Every .dialog-backdrop
    // shares z-index 1400 and stacks by DOM order, so this chain runs in reverse
    // DOM order. It has to be an else-if chain: the dep-scan modal opens the
    // delete modal over itself (a row's Remove button), and depScanClose() wipes
    // DEP_SCAN.items — so independent ifs would let one Escape cancel the delete
    // AND destroy the scan underneath it.
    if (event.key === "Escape" && state.pendingDialog) {
      closeAppConfirm(false);
    } else if (event.key === "Escape" && !$("complete-backdrop").classList.contains("hidden")) {
      closeDedupComplete();
    } else if (event.key === "Escape" && !$("hub-dl-backdrop")?.classList.contains("hidden")) {
      hubDlClose();
    // Image gallery. Guarded on the zoom lightbox being closed: setupImageZoom
    // registers its OWN unconditional Escape listener outside this chain, so
    // without the guard one Escape would close the lightbox AND this modal
    // underneath it. This is the first modal the lightbox can open over.
    } else if (
      event.key === "Escape" &&
      $("image-zoom-backdrop")?.classList.contains("hidden") &&
      !$("vp-images-backdrop")?.classList.contains("hidden")
    ) {
      vpImagesClose();
    // Delete modal: Escape cancels (resolve false); vpDeleteModalClose refuses
    // while a scan runs, so a stray keypress can't abandon a live scan.
    } else if (event.key === "Escape" && !$("vp-delete-backdrop")?.classList.contains("hidden")) {
      vpDeleteModalClose(false);
    // Collect Dependencies stacks over Download Dependencies, so it closes first.
    // dcClose refuses while a scan or copy runs, like depScanClose.
    } else if (event.key === "Escape" && !$("dep-collect-backdrop")?.classList.contains("hidden")) {
      dcClose();
    // Same close-if-idle rule: depScanClose refuses while a scan runs.
    } else if (event.key === "Escape" && !$("dep-scan-backdrop")?.classList.contains("hidden")) {
      depScanClose();
    // Close-if-idle only. vpClosePlan refuses while a task is running, so a
    // stray keypress can never abandon a live destructive apply.
    } else if (event.key === "Escape" && !$("vp-plan-backdrop")?.classList.contains("hidden")) {
      vpClosePlan();
    } else if (
      event.key === "Escape" &&
      HUB_PAGE.rid &&
      !$("hub-view")?.classList.contains("hidden") &&
      !document.querySelector("[data-lib-dd-menu]:not(.hidden)") &&
      document.activeElement?.id !== "hub-page-address"
    ) {
      hubPageClose();
    }
  });

  setupVarPackagesMaintenance();
  setupVarPackagesSelection();
  setupVarPackagesDeleteModal();
  setupVarPackagesImagesModal();
  setupVarDetailsDeps();
  setupDepCollect();

  $("dialog-cancel").addEventListener("click", () => {
    closeAppConfirm(false);
  });

  $("dialog-confirm").addEventListener("click", () => {
    closeAppConfirm(true);
  });

  $("dialog-backdrop").addEventListener("click", (event) => {
    if (event.target === $("dialog-backdrop")) {
      closeAppConfirm(false);
    }
  });

  $("complete-ok").addEventListener("click", closeDedupComplete);
  $("complete-open").addEventListener("click", () => {
    // Honor dataset.path override when another page (e.g. Missing Resources)
    // populated the dialog with its own output location.
    const override = $("complete-open").dataset.path;
    if (override && invoke) {
      invoke("show_in_explorer", { path: override }).catch(() => {});
      return;
    }
    showOutputInExplorer().catch(() => {});
  });
  $("complete-report").addEventListener("click", () => {
    const path = $("complete-report").dataset.path;
    if (path && invoke) {
      invoke("show_in_explorer", { path }).catch(() => {});
    }
  });
  $("complete-open-backup").addEventListener("click", () => {
    const override = $("complete-open-backup").dataset.path;
    if (override && invoke) {
      invoke("show_in_explorer", { path: override }).catch(() => {});
      return;
    }
    const outEl = $("dbf-output-dir") || $("output-dir");
    const outputDir = (outEl?.value || "").trim();
    if (outputDir && invoke) {
      invoke("show_in_explorer", { path: outputDir + "\\backup" }).catch(() => {});
    }
  });
  $("complete-backdrop").addEventListener("click", (event) => {
    if (event.target === $("complete-backdrop")) {
      closeDedupComplete();
    }
  });

  window.addEventListener("resize", hideContextMenu);
  window.addEventListener("scroll", handleWindowScroll, true);

  const setThemeFromSettings = async (next) => {
    if (state.theme === next) return;
    state.theme = next;
    updateStaticCopy();
    await persistLanguage();
  };
  for (const theme of THEMES) {
    const btn = $(`settings-theme-${theme}`);
    if (btn) btn.addEventListener("click", () => setThemeFromSettings(theme));
  }

  const triggerChange = (el) => {
    el.dispatchEvent(new Event("change", { bubbles: true }));
  };
  const settingsReplace = $("settings-default-replace");
  const settingsBackup = $("settings-default-backup");
  const settingsVap = $("settings-default-vap");
  if (settingsReplace) {
    settingsReplace.addEventListener("change", () => {
      const target = $("replace-in-place");
      target.checked = settingsReplace.checked;
      triggerChange(target);
      syncSettingsControlState();
    });
  }
  if (settingsBackup) {
    settingsBackup.addEventListener("change", () => {
      const target = $("backup-changed");
      target.checked = settingsBackup.checked;
      triggerChange(target);
      syncSettingsControlState();
    });
  }
  if (settingsVap) {
    settingsVap.addEventListener("change", () => {
      const target = $("process-vap");
      target.checked = settingsVap.checked;
      triggerChange(target);
      syncSettingsControlState();
    });
  }

  const mirrorPathInput = (settingsId, mainId) => {
    const settingsEl = $(settingsId);
    const mainEl = $(mainId);
    if (!settingsEl || !mainEl) return;
    settingsEl.addEventListener("input", () => {
      mainEl.value = settingsEl.value;
      mainEl.dispatchEvent(new Event("input", { bubbles: true }));
    });
    settingsEl.addEventListener("change", () => {
      mainEl.dispatchEvent(new Event("change", { bubbles: true }));
    });
    mainEl.addEventListener("input", () => {
      if (document.activeElement !== settingsEl) settingsEl.value = mainEl.value;
    });
  };
  mirrorPathInput("settings-input-dir", "input-dir");
  mirrorPathInput("settings-output-dir", "output-dir");
  mirrorPathInput("settings-vap-dir", "vap-dir");
  mirrorPathInput("build-db-input-dir", "input-dir");

  const mirrorPickButton = (settingsId, mainId) => {
    const settingsBtn = $(settingsId);
    const mainBtn = $(mainId);
    if (!settingsBtn || !mainBtn) return;
    settingsBtn.addEventListener("click", () => mainBtn.click());
  };
  mirrorPickButton("settings-pick-input", "pick-input-button");
  mirrorPickButton("settings-pick-output", "pick-output-button");
  mirrorPickButton("settings-pick-vap", "pick-vap-button");
  mirrorPickButton("build-db-pick-input", "pick-input-button");

  // Downloads folder — standalone config (not mirrored). Browse + persist.
  const dlFolderInput = $("settings-downloads-folder");
  if (dlFolderInput) {
    dlFolderInput.addEventListener("change", () => {
      persistAllConfig().catch((e) => addLog(`Settings: ${String(e)}`));
    });
  }
  const dlFolderPick = $("settings-pick-downloads");
  if (dlFolderPick) {
    dlFolderPick.addEventListener("click", async () => {
      if (!invoke) return;
      try {
        const folder = await invoke("pick_folder");
        if (folder && dlFolderInput) {
          dlFolderInput.value = folder;
          persistAllConfig().catch((e) => addLog(`Settings: ${String(e)}`));
        }
      } catch (err) {
        addLog(`Settings: ${String(err)}`);
      }
    });
  }
  // VAR library folder — the reference set for dependency checks. Browse +
  // persist, mirroring the Downloads folder above.
  const libFolderInput = $("settings-library-folder");
  if (libFolderInput) {
    libFolderInput.addEventListener("change", () => {
      persistAllConfig().catch((e) => addLog(`Settings: ${String(e)}`));
    });
  }
  const libFolderPick = $("settings-pick-library");
  if (libFolderPick) {
    libFolderPick.addEventListener("click", async () => {
      if (!invoke) return;
      try {
        const folder = await invoke("pick_folder");
        if (folder && libFolderInput) {
          libFolderInput.value = folder;
          persistAllConfig().catch((e) => addLog(`Settings: ${String(e)}`));
        }
      } catch (err) {
        addLog(`Settings: ${String(err)}`);
      }
    });
  }
  const organizeByCreator = $("settings-organize-by-creator");
  if (organizeByCreator) {
    organizeByCreator.addEventListener("change", () => {
      persistAllConfig().catch((e) => addLog(`Settings: ${String(e)}`));
    });
  }

  const showSaveFeedback = (text, kind) => {
    const node = $("settings-save-feedback");
    if (!node) return;
    node.textContent = text;
    node.classList.remove("success", "error");
    if (kind) node.classList.add(kind);
    if (kind === "success") {
      window.clearTimeout(showSaveFeedback._timer);
      showSaveFeedback._timer = window.setTimeout(() => {
        if (node.textContent === text) {
          node.textContent = "";
          node.classList.remove("success", "error");
        }
      }, 2400);
    }
  };

  if ($("settings-save-button")) {
    $("settings-save-button").addEventListener("click", async () => {
      const btn = $("settings-save-button");
      btn.disabled = true;
      try {
        await saveAllSettings();
        showSaveFeedback(t("settingsSaved"), "success");
      } catch (error) {
        showSaveFeedback(`${t("settingsSaveFailed")}${String(error)}`, "error");
      } finally {
        btn.disabled = false;
      }
    });
  }

  if ($("settings-reset-button")) {
    $("settings-reset-button").addEventListener("click", async () => {
      const confirmed = await showAppConfirm(t("settingsConfirmReset"));
      if (!confirmed) return;
      const btn = $("settings-reset-button");
      btn.disabled = true;
      try {
        await resetSettingsFromDisk();
        showSaveFeedback(t("settingsResetDone"), "success");
      } catch (error) {
        showSaveFeedback(`${t("settingsResetFailed")}${String(error)}`, "error");
      } finally {
        btn.disabled = false;
      }
    });
  }

  if ($("settings-refresh-button")) {
    $("settings-refresh-button").addEventListener("click", () => {
      loadSettingsView();
    });
  }
  if ($("settings-clear-db-button")) {
    $("settings-clear-db-button").addEventListener("click", async () => {
      const confirmed = await showAppConfirm(t("settingsConfirmClear"));
      if (!confirmed) return;
      if (!invoke) {
        const feedback = $("settings-clear-db-status");
        if (feedback) {
          feedback.textContent = `${t("settingsClearFailed")}Tauri runtime unavailable`;
          feedback.classList.add("error");
        }
        return;
      }
      await startSettingsClearDb();
    });
  }

  // ============================================================
  // Resource List page
  // ============================================================
  // DB-only global browser of every indexed file. Mirrors VAR Packages but
  // operates at the resource grain; clicking a row opens a right-side panel
  // listing every package containing the same CRC32.

  const RL_SIZE_THRESHOLDS = {
    sm: { label: () => t("resourceListSizeSmall") },
    md: { label: () => t("resourceListSizeMedium") },
    lg: { label: () => t("resourceListSizeLarge") },
  };

  const RL_CATEGORY_ICONS = {
    Scene: "movie",
    SubScene: "subdirectory_arrow_right",
    Morph: "face",
    Clothing: "checkroom",
    Hair: "content_cut",
    Texture: "image",
    Asset: "deployed_code",
    Plugin: "extension",
    Scripts: "code",
    Audio: "music_note",
    Preset: "tune",
    Other: "draft",
  };

  function rl() {
    return state.resourceList;
  }

  function rlActiveFilterCount(filters) {
    if (!filters) return 0;
    let n = 0;
    if (filters.category) n += 1;
    if (filters.sizeBucket) n += 1;
    return n;
  }

  function rlSerializeFilters(filters) {
    const out = {};
    if (filters?.category) out.category = filters.category;
    if (filters?.sizeBucket) out.sizeBucket = filters.sizeBucket;
    return out;
  }

  function rlFilterFieldLabel(field) {
    switch (field) {
      case "category":
        return t("resourceListFilterCategory");
      case "size":
        return t("resourceListFilterSize");
      default:
        return field;
    }
  }

  function rlFilterDisplayValue(field) {
    const f = rl().filters ?? {};
    switch (field) {
      case "category":
        return f.category || t("resourceListFilterAll");
      case "size":
        if (f.sizeBucket && RL_SIZE_THRESHOLDS[f.sizeBucket]) {
          return RL_SIZE_THRESHOLDS[f.sizeBucket].label();
        }
        return t("resourceListFilterAll");
      default:
        return t("resourceListFilterAll");
    }
  }

  function rlIsFilterActive(field) {
    const f = rl().filters ?? {};
    switch (field) {
      case "category":
        return Boolean(f.category);
      case "size":
        return Boolean(f.sizeBucket);
      default:
        return false;
    }
  }

  function rlClearAllFilters() {
    if (rlActiveFilterCount(rl().filters) === 0) return;
    rl().filters = { category: null, sizeBucket: null };
    rl().page = 0;
    rlRenderFilterBar();
    refreshResourceList();
  }

  function rlSetFilter(field, value) {
    const next = value || null;
    const stateField = field === "size" ? "sizeBucket" : field;
    if (rl().filters[stateField] === next) return;
    rl().filters[stateField] = next;
    rl().page = 0;
    rlRenderFilterBar();
    refreshResourceList();
  }

  function rlCategoryOptions() {
    const opts = [{ value: null, label: t("resourceListFilterAll") }];
    const cats = rl().filterOptions?.categories ?? [];
    const q = String(rl().categoryMenuQuery ?? "").trim().toLowerCase();
    const filtered = q ? cats.filter((c) => c.toLowerCase().includes(q)) : cats.slice();
    for (const name of filtered) opts.push({ value: name, label: name });
    if (filtered.length === 0 && cats.length > 0) {
      opts.push({ value: null, label: t("varPackagesFilterEmpty"), disabled: true });
    }
    return opts;
  }

  function rlSizeOptions() {
    return [
      { value: null, label: t("resourceListFilterAll") },
      { value: "sm", label: t("resourceListSizeSmall") },
      { value: "md", label: t("resourceListSizeMedium") },
      { value: "lg", label: t("resourceListSizeLarge") },
    ];
  }

  function rlPopulateMenuOptions(field, options, selected) {
    const menu = document.querySelector(`[data-rl-menu='${field}']`);
    if (!menu) return;
    const list = menu.querySelector(".rl-filter-menu-list");
    const target = list || menu;
    target.innerHTML = options
      .map((opt) => {
        const isActive = (opt.value ?? null) === (selected ?? null);
        const dataValue = opt.value == null ? "" : escapeAttribute(String(opt.value));
        const disabledAttr = opt.disabled ? " disabled" : "";
        return `<button type="button" class="rl-filter-menu-item${
          isActive ? " is-active" : ""
        }" data-rl-menu-value="${dataValue}"${disabledAttr}>${escapeHtml(
          opt.label
        )}</button>`;
      })
      .join("");
  }

  function rlPopulateAllMenus() {
    const f = rl().filters;
    rlPopulateMenuOptions("category", rlCategoryOptions(), f.category);
    rlPopulateMenuOptions("size", rlSizeOptions(), f.sizeBucket);
  }

  function rlCloseAllMenus() {
    document.querySelectorAll(".rl-filter-menu").forEach((m) => m.classList.add("hidden"));
    document
      .querySelectorAll(".rl-filter-trigger.is-open")
      .forEach((tr) => tr.classList.remove("is-open"));
  }

  function rlToggleFilterMenu(field) {
    const menu = document.querySelector(`[data-rl-menu='${field}']`);
    const trigger = document.querySelector(`[data-rl-trigger='${field}']`);
    if (!menu || !trigger) return;
    const willOpen = menu.classList.contains("hidden");
    rlCloseAllMenus();
    if (willOpen) {
      menu.classList.remove("hidden");
      trigger.classList.add("is-open");
      if (field === "category") {
        const search = document.querySelector(`[data-rl-menu-search='${field}']`);
        if (search) {
          search.value = rl().categoryMenuQuery ?? "";
          search.focus();
        }
      }
    }
  }

  function rlRenderFilterBar() {
    const setText = (id, value) => {
      const node = $(id);
      if (node) node.textContent = value;
    };
    ["category", "size"].forEach((field) => {
      const fieldLabel = rlFilterFieldLabel(field);
      const value = rlFilterDisplayValue(field);
      const active = rlIsFilterActive(field);
      setText(
        `resource-list-filter-${field}-label`,
        active ? t("resourceListFilterChip", fieldLabel, value) : `${fieldLabel}: ${t("resourceListFilterAll")}`
      );
      const trigger = document.querySelector(`[data-rl-trigger='${field}']`);
      if (trigger) {
        trigger.classList.toggle("is-active", active);
      }
    });
    const clearBtn = $("resource-list-filter-clear");
    if (clearBtn) {
      clearBtn.classList.toggle(
        "hidden",
        rlActiveFilterCount(rl().filters) === 0
      );
    }
    rlPopulateAllMenus();
  }

  function rlIconFor(item) {
    const cat = item?.category || "Other";
    const icon = RL_CATEGORY_ICONS[cat] || RL_CATEGORY_ICONS.Other;
    return `<span class="rl-icon-bubble" title="${escapeAttribute(cat)}"><span class="material-symbols-outlined">${icon}</span></span>`;
  }

  function rlFormatCrc32(value) {
    if (value === null || value === undefined) return "—";
    const n = Number(value);
    if (!Number.isFinite(n)) return "—";
    return n.toString(16).toUpperCase().padStart(8, "0");
  }

  function rlSplitInternalPath(path) {
    const norm = String(path ?? "").replace(/\\/g, "/");
    const idx = norm.lastIndexOf("/");
    if (idx < 0) return { name: norm, dir: "" };
    return { name: norm.slice(idx + 1), dir: norm.slice(0, idx + 1) };
  }

  function rlRenderRows() {
    const tbody = $("resource-list-tbody");
    if (!tbody) return;
    const items = rl().items ?? [];
    if (rl().loading && items.length === 0) {
      tbody.innerHTML = `<tr class="resource-list-empty-row"><td colspan="6">${escapeHtml(
        t("resourceListLoadingDb")
      )}</td></tr>`;
      return;
    }
    if (items.length === 0) {
      const hasFilter =
        Boolean(rl().filter) || rlActiveFilterCount(rl().filters) > 0;
      tbody.innerHTML = `<tr class="resource-list-empty-row"><td colspan="6">${escapeHtml(
        hasFilter ? t("resourceListNoResults") : t("resourceListEmpty")
      )}</td></tr>`;
      return;
    }
    const selectedKey = rl().sidePanelKey;
    const rows = items.map((it) => {
      const { name, dir } = rlSplitInternalPath(it.internal_path);
      const isSelected = selectedKey != null && rlKeyForItem(it) === selectedKey;
      return `<tr data-rl-resource-id="${escapeAttribute(String(it.resource_id))}" data-rl-package-id="${escapeAttribute(String(it.package_id || ""))}"${
        isSelected ? ' class="is-selected"' : ""
      }>
        <td class="rl-col-icon">${rlIconFor(it)}</td>
        <td class="rl-col-name"><div class="rl-col-name-cell"><span class="rl-name-primary">${escapeHtml(
          name
        )}</span>${dir ? `<span class="rl-name-secondary">${escapeHtml(dir)}</span>` : ""}</div></td>
        <td class="rl-col-cat"><span class="rl-cat-badge">${escapeHtml(it.category || "Other")}</span></td>
        <td class="rl-col-crc">${escapeHtml(rlFormatCrc32(it.crc32))}</td>
        <td class="rl-col-package"><div class="rl-pkg-cell"><strong>${escapeHtml(
          it.package_id || ""
        )}</strong></div></td>
        <td class="rl-col-size">${escapeHtml(formatBytesLocal(it.size))}</td>
      </tr>`;
    });
    tbody.innerHTML = rows.join("");
  }

  function rlRenderCount() {
    const node = $("resource-list-count");
    if (!node) return;
    if (rl().totalKnown) {
      node.textContent = t("resourceListCount", Number(rl().total ?? 0));
    } else {
      // Don't fake a 0 count while the COUNT(*) is still computing — show
      // an ellipsis so the user knows it's loading.
      node.textContent = "…";
    }
  }

  function rlRenderPagination() {
    const wrap = $("resource-list-pagination");
    const summary = $("resource-list-pagination-summary");
    const controls = $("resource-list-pagination-controls");
    if (!wrap || !summary || !controls) return;

    const pageSize = rl().pageSize;
    const rowCount = (rl().items ?? []).length;

    // No rows AND no count known yet → no pagination yet (initial / empty
    // states). Note: keep showing pagination when totalKnown=true even if
    // rows are empty, in case search returned zero — handled below.
    if (rowCount === 0 && !rl().totalKnown) {
      wrap.classList.add("hidden");
      return;
    }

    if (rl().totalKnown) {
      const total = Number(rl().total ?? 0);
      if (total === 0) {
        wrap.classList.add("hidden");
        return;
      }
      const totalPages = Math.max(1, Math.ceil(total / pageSize));
      const page = Math.min(rl().page, totalPages - 1);
      rl().page = page;

      wrap.classList.remove("hidden");
      const start = page * pageSize + 1;
      const end = Math.min(total, (page + 1) * pageSize);
      summary.innerHTML = escapeHtml(t("resourceListPageSummary", start, end, total));

      const pages = buildPageNumbers(totalPages, page);
      const buttons = [];
      buttons.push(
        `<button type="button" data-rl-page-action="prev"${page === 0 ? " disabled" : ""}>${escapeHtml(
          t("resourceListPagePrev")
        )}</button>`
      );
      for (const p of pages) {
        if (p === "...") {
          buttons.push('<span class="resource-list-page-ellipsis">…</span>');
        } else {
          const isActive = p === page;
          buttons.push(
            `<button type="button" data-rl-page-jump="${p}"${
              isActive ? ' class="is-active" disabled' : ""
            }>${p + 1}</button>`
          );
        }
      }
      buttons.push(
        `<button type="button" data-rl-page-action="next"${
          page >= totalPages - 1 ? " disabled" : ""
        }>${escapeHtml(t("resourceListPageNext"))}</button>`
      );
      controls.innerHTML = buttons.join("");
    } else {
      // Total not yet known. Show a simplified prev/next-only pager and a
      // summary without the "/ N total" suffix. We know there's a next
      // page only if the current page filled completely (rowCount ==
      // pageSize). Prev is enabled whenever page > 0.
      wrap.classList.remove("hidden");
      const page = rl().page;
      const start = page * pageSize + 1;
      const end = page * pageSize + rowCount;
      // Reuse the existing summary i18n by passing a "?" placeholder so
      // the user understands the total is still loading.
      summary.innerHTML = escapeHtml(
        t("resourceListPageSummary", start, end, "…")
      );
      const hasNext = rowCount >= pageSize;
      const buttons = [
        `<button type="button" data-rl-page-action="prev"${
          page === 0 ? " disabled" : ""
        }>${escapeHtml(t("resourceListPagePrev"))}</button>`,
        `<button type="button" data-rl-page-action="next"${
          !hasNext ? " disabled" : ""
        }>${escapeHtml(t("resourceListPageNext"))}</button>`,
      ];
      controls.innerHTML = buttons.join("");
    }
  }

  function rlRenderHeaders() {
    const setText = (id, value) => {
      const node = $(id);
      if (node) {
        const span = node.querySelector("span");
        if (span) span.textContent = value;
        else node.textContent = value;
      }
    };
    setText("resource-list-th-icon", t("resourceListThIcon"));
    setText("resource-list-th-name", t("resourceListThName"));
    setText("resource-list-th-category", t("resourceListThCategory"));
    setText("resource-list-th-crc", t("resourceListThCrc"));
    setText("resource-list-th-package", t("resourceListThPackage"));
    setText("resource-list-th-size", t("resourceListThSize"));
  }

  function rlRenderStaticLabels() {
    const setText = (id, value) => {
      const node = $(id);
      if (node) node.textContent = value;
    };
    setText("sidebar-resource-list-label", t("resourceListNav"));
    setText("resource-list-title", t("resourceListTitle"));
    setText("resource-list-subtitle", t("resourceListSubtitle"));
    setText("resource-list-refresh-label", t("resourceListRefresh"));
    setText("resource-list-filter-bar-label", t("resourceListFiltersLabel") || t("varPackagesFiltersLabel"));
    setText("resource-list-filter-clear", t("resourceListFilterClearAll"));
    setText("resource-list-side-title", t("resourceListSidePanelTitle"));
    setText("resource-list-side-subtitle", t("resourceListSidePanelSubtitle"));
    const empty = $("resource-list-side-empty");
    if (empty) empty.textContent = t("resourceListSidePanelEmpty");
    const tableEmpty = $("resource-list-empty");
    if (tableEmpty) tableEmpty.textContent = t("resourceListEmpty");
    const searchInput = $("resource-list-filter");
    if (searchInput) {
      searchInput.placeholder = t("resourceListSearchPlaceholder");
    }
    setText("resource-list-filter-button-label", t("resourceListSearchButton"));
    setText("resource-list-filter-clear-button-label", t("resourceListSearchClear"));
    if (typeof rlSyncSearchClearVisibility === "function") {
      rlSyncSearchClearVisibility();
    }
    rlRenderHeaders();
  }

  function rlRenderAll() {
    rlRenderStaticLabels();
    rlRenderFilterBar();
    rlRenderCount();
    rlRenderRows();
    rlRenderPagination();
  }

  let rlFetchSeq = 0;

  async function rlFetchPage() {
    if (!invoke) return;
    const seq = ++rlFetchSeq;
    rl().loading = true;
    const progress = $("resource-list-progress");
    if (progress) progress.classList.remove("hidden");
    rlRenderRows();
    try {
      const offset = rl().page * rl().pageSize;
      // `skip_total: true` makes the page query return rows without running
      // the multi-second COUNT(*). The count is fetched separately via
      // `rlFetchCount` so the table renders immediately.
      const page = await invoke("list_resources_from_db", {
        offset,
        limit: rl().pageSize,
        search: rl().filter || null,
        filters: rlSerializeFilters(rl().filters),
        skipTotal: true,
      });
      // A later fetch superseded this one — discard.
      if (seq !== rlFetchSeq) return;
      rl().items = Array.isArray(page?.items) ? page.items : [];
    } catch (error) {
      if (seq !== rlFetchSeq) return;
      rl().items = [];
      addLog(t("resourceListLoadFailed", String(error)));
    } finally {
      if (seq === rlFetchSeq) {
        rl().loading = false;
        if (progress) progress.classList.add("hidden");
        rlRenderCount();
        rlRenderRows();
        rlRenderPagination();
      }
    }
  }

  let rlCountFetchSeq = 0;

  async function rlFetchCount() {
    if (!invoke) return;
    const seq = ++rlCountFetchSeq;
    rl().totalKnown = false;
    rlRenderPagination();
    try {
      const total = await invoke("count_resources_from_db", {
        search: rl().filter || null,
        filters: rlSerializeFilters(rl().filters),
      });
      // A filter/search change after this fetch started — drop the result.
      if (seq !== rlCountFetchSeq) return;
      rl().total = Number(total ?? 0);
      rl().totalKnown = true;
    } catch (error) {
      if (seq !== rlCountFetchSeq) return;
      // Leave totalKnown=false; pagination falls back to prev/next-only.
      addLog(t("resourceListLoadFailed", String(error)));
    } finally {
      if (seq === rlCountFetchSeq) {
        rlRenderCount();
        rlRenderPagination();
      }
    }
  }

  async function rlFetchFilterOptions() {
    if (!invoke) return;
    try {
      const opts = await invoke("list_resource_filter_options");
      rl().filterOptions = {
        categories: Array.isArray(opts?.categories) ? opts.categories : [],
      };
    } catch (error) {
      rl().filterOptions = { categories: [] };
      addLog(t("resourceListFilterOptionsFailed", String(error)));
    }
    rlRenderFilterBar();
  }

  /// Fetches the visible page; optionally fetches the global COUNT(*) too.
  /// The count is the slow query, so callers default to fetching it but
  /// pagination clicks can pass `{ refetchCount: false }` to reuse the
  /// previously-known total without paying for another scan.
  async function refreshResourceList(opts = {}) {
    const refetchCount = opts.refetchCount ?? true;
    if (refetchCount) {
      // Wipe stale total before rendering so the summary doesn't display
      // the previous filter's number while the new one is computing.
      rl().total = 0;
      rl().totalKnown = false;
    }
    // Rows-first: await so the table paints before the (slow) count fires.
    // Both go through the same Mutex<Connection> so serializing them keeps
    // the rows from waiting behind the count on the same thread.
    await rlFetchPage();
    if (refetchCount) {
      rlFetchCount();
    }
  }

  async function refreshResourceListView() {
    rlRenderAll();
    if (!rl().initialized) {
      rl().initialized = true;
      await rlFetchFilterOptions();
      refreshResourceList();
    } else {
      // Re-render in case theme or language changed while away.
      rlRenderAll();
    }
  }

  window.__refreshResourceListView = refreshResourceListView;

  // ----- Side panel (duplicates) -----

  // Build the toggle key for a row. Returns a `crc:<n>` string when the row
  // has a CRC32, or null when it doesn't (such rows can't be inspected for
  // duplicates).
  function rlKeyForItem(item) {
    if (!item || item.crc32 == null) return null;
    return `crc:${Number(item.crc32) >>> 0}`;
  }

  function rlOpenSidePanel(item) {
    const key = rlKeyForItem(item);
    if (!key) return;
    rl().sidePanelKey = key;
    rl().sidePanelMeta = {
      crc32: item.crc32,
      size: item.size,
      internal_path: item.internal_path,
      category: item.category,
    };
    rl().sidePanelItems = [];
    rl().sidePanelLoading = true;
    rlRenderSidePanel();
    rlFetchSidePanel({ crc32: item.crc32, key });
    // Highlight the selected row.
    rlRenderRows();
  }

  function rlCloseSidePanel() {
    rl().sidePanelKey = null;
    rl().sidePanelItems = [];
    rl().sidePanelMeta = null;
    rl().sidePanelLoading = false;
    rlRenderSidePanel();
    rlRenderRows();
  }

  async function rlFetchSidePanel({ crc32, key }) {
    if (!invoke || crc32 == null) return;
    try {
      const refs = await invoke("list_resource_duplicates", {
        crc32: Number(crc32) >>> 0,
      });
      // Stale request — user already navigated away or selected a different row.
      if (rl().sidePanelKey !== key) return;
      rl().sidePanelItems = Array.isArray(refs) ? refs : [];
    } catch (error) {
      if (rl().sidePanelKey !== key) return;
      rl().sidePanelItems = [];
      addLog(t("resourceListSidePanelLoadFailed", String(error)));
    } finally {
      if (rl().sidePanelKey === key) {
        rl().sidePanelLoading = false;
        rlRenderSidePanel();
      }
    }
  }

  function rlRenderSidePanel() {
    const panel = $("resource-list-side-panel");
    const body = $("resource-list-body");
    const list = $("resource-list-side-list");
    const empty = $("resource-list-side-empty");
    const meta = $("resource-list-side-meta");
    if (!panel || !body || !list || !empty) return;

    const key = rl().sidePanelKey;
    if (!key) {
      panel.classList.add("hidden");
      body.classList.remove("has-side-panel");
      return;
    }
    panel.classList.remove("hidden");
    body.classList.add("has-side-panel");

    const m = rl().sidePanelMeta || {};
    if (meta) {
      const parts = [];
      if (m.internal_path) {
        parts.push(`<div><code>${escapeHtml(m.internal_path)}</code></div>`);
      }
      const crc = rlFormatCrc32(m.crc32);
      if (crc !== "—") parts.push(`<div>CRC32 · <code>${escapeHtml(crc)}</code></div>`);
      if (m.size != null) parts.push(`<div>${escapeHtml(formatBytesLocal(m.size))}</div>`);
      meta.innerHTML = parts.join("");
    }

    const items = rl().sidePanelItems ?? [];
    if (rl().sidePanelLoading) {
      empty.textContent = t("resourceListLoadingDb");
      empty.classList.remove("hidden");
      list.innerHTML = "";
      return;
    }
    if (items.length === 0) {
      empty.textContent = t("resourceListSidePanelEmpty");
      empty.classList.remove("hidden");
      list.innerHTML = "";
      return;
    }
    if (items.length === 1) {
      empty.textContent = t("resourceListSidePanelOnlyOne");
      empty.classList.remove("hidden");
    } else {
      empty.classList.add("hidden");
    }

    list.innerHTML = items
      .map((ref, idx) => {
        return `<li class="resource-list-side-item" data-rl-side-idx="${idx}">
          <div class="resource-list-side-item-head">
            <span class="resource-list-side-item-pkg">${escapeHtml(ref.package_id || "")}</span>
          </div>
          <div class="resource-list-side-item-path">${escapeHtml(ref.internal_path || "")}</div>
          <div class="resource-list-side-item-foot">
            <span class="resource-list-side-item-size">${escapeHtml(formatBytesLocal(ref.size || 0))}</span>
            <button type="button" class="resource-list-side-item-jump" data-rl-side-jump="${idx}">
              <span class="material-symbols-outlined">open_in_new</span>
              <span>${escapeHtml(t("resourceListSideJump"))}</span>
            </button>
          </div>
        </li>`;
      })
      .join("");
  }

  function rlJumpToVarDetails(ref) {
    if (!ref || !ref.package_id) return;
    // Defer to the shared opener — when `package_file` resolves on disk it
    // pulls scene image / size / modified via get_var_file_stats and flags the
    // item as local so the VAR Details page renders the scene preview. Falls
    // back to a synthetic db-only item when the path is stale.
    if (typeof openSourceRowInVarDetails === "function") {
      openSourceRowInVarDetails(ref.package_id, ref.package_file || "", "db");
    }
  }

  // ----- Right-click context menu -----

  async function rlCopyToClipboard(text) {
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(text);
      } else {
        // Fallback path for older webviews. textarea + execCommand stays in
        // the synchronous user-gesture window so the copy succeeds.
        const ta = document.createElement("textarea");
        ta.value = text;
        ta.style.position = "fixed";
        ta.style.opacity = "0";
        document.body.appendChild(ta);
        ta.select();
        document.execCommand("copy");
        document.body.removeChild(ta);
      }
      addLog(t("resourceListCopyOk"));
    } catch (error) {
      addLog(`${t("resourceListCopyFailed")} ${String(error)}`);
    }
  }

  function rlShowContextMenu(event, item) {
    event.preventDefault();
    const items = [];
    if (item.crc32 != null) {
      const hex = rlFormatCrc32(item.crc32);
      items.push({
        label: `${t("resourceListContextCopyCrc")} (${hex})`,
        action: () => rlCopyToClipboard(hex),
      });
    }
    if (item.package_id) {
      items.push({
        label: t("resourceListContextOpenPackage"),
        action: () => rlJumpToVarDetails({
          package_id: item.package_id,
          package_file: item.package_file,
          internal_path: item.internal_path,
          size: item.size,
        }),
      });
    }
    if (item.package_file) {
      items.push({ separator: true });
      items.push({
        label: t("resourceListContextShowExplorer"),
        action: async () => {
          if (!invoke) return;
          try {
            await invoke("show_in_explorer", { path: item.package_file });
          } catch (error) {
            addLog(String(error));
          }
        },
      });
    }
    if (items.length === 0) return;
    showContextMenu(event.clientX, event.clientY, items);
  }

  // ----- Wiring -----

  const rlRefreshBtn = $("resource-list-refresh-button");
  if (rlRefreshBtn) {
    rlRefreshBtn.addEventListener("click", async () => {
      rl().page = 0;
      await rlFetchFilterOptions();
      refreshResourceList();
    });
  }

  // Search is button-triggered (not type-as-you-go) because the underlying
  // query scans the resources table — sub-second on small DBs but slow
  // enough on multi-million-row datasets that a per-keystroke debounce
  // still queues up several full-table scans.
  const rlSearchInput = $("resource-list-filter");
  const rlSearchButton = $("resource-list-filter-button");
  const rlSearchClearButton = $("resource-list-filter-clear-button");

  function rlSyncSearchClearVisibility() {
    if (!rlSearchClearButton) return;
    const hasFilter = Boolean((rl().filter ?? "").trim());
    rlSearchClearButton.classList.toggle("hidden", !hasFilter);
  }

  function rlApplySearch() {
    const next = rlSearchInput ? rlSearchInput.value : "";
    if (rl().filter === next) {
      rlSyncSearchClearVisibility();
      return;
    }
    rl().filter = next;
    rl().page = 0;
    rlSyncSearchClearVisibility();
    refreshResourceList();
  }

  function rlClearSearch() {
    if (rlSearchInput) rlSearchInput.value = "";
    if (rl().filter === "") {
      rlSyncSearchClearVisibility();
      return;
    }
    rl().filter = "";
    rl().page = 0;
    rlSyncSearchClearVisibility();
    refreshResourceList();
  }

  if (rlSearchInput) {
    rlSearchInput.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        rlApplySearch();
      }
    });
  }
  if (rlSearchButton) {
    rlSearchButton.addEventListener("click", rlApplySearch);
  }
  if (rlSearchClearButton) {
    rlSearchClearButton.addEventListener("click", rlClearSearch);
  }

  document.querySelectorAll(".rl-filter-trigger").forEach((trigger) => {
    trigger.addEventListener("click", (event) => {
      event.stopPropagation();
      const field = trigger.getAttribute("data-rl-trigger");
      if (field) rlToggleFilterMenu(field);
    });
  });

  const rlFilterBar = $("resource-list-filter-bar");
  if (rlFilterBar) {
    rlFilterBar.addEventListener("click", (event) => {
      const item = event.target.closest("[data-rl-menu-value]");
      if (!item || item.disabled) return;
      const menu = item.closest(".rl-filter-menu");
      if (!menu) return;
      const field = menu.getAttribute("data-rl-menu");
      if (!field) return;
      const raw = item.getAttribute("data-rl-menu-value");
      const value = raw === "" ? null : raw;
      rlSetFilter(field, value);
      rlCloseAllMenus();
    });
  }

  const rlCategorySearch = document.querySelector("[data-rl-menu-search='category']");
  if (rlCategorySearch) {
    rlCategorySearch.addEventListener("input", () => {
      rl().categoryMenuQuery = rlCategorySearch.value;
      rlPopulateMenuOptions("category", rlCategoryOptions(), rl().filters?.category ?? null);
    });
    rlCategorySearch.addEventListener("click", (event) => event.stopPropagation());
  }

  const rlClearAll = $("resource-list-filter-clear");
  if (rlClearAll) rlClearAll.addEventListener("click", rlClearAllFilters);

  // Close any open RL filter menu when clicking outside the dropdown.
  document.addEventListener("click", (event) => {
    if (!event.target.closest(".rl-filter-dropdown")) {
      rlCloseAllMenus();
    }
  });

  // Row click → open side panel; right-click → context menu.
  const rlTbody = $("resource-list-tbody");
  if (rlTbody) {
    rlTbody.addEventListener("click", (event) => {
      const row = event.target.closest("tr[data-rl-resource-id]");
      if (!row) return;
      const resourceId = row.getAttribute("data-rl-resource-id");
      const item = (rl().items ?? []).find(
        (it) => String(it.resource_id) === String(resourceId)
      );
      if (!item) return;
      // Toggle: clicking the already-selected row closes the panel.
      const key = rlKeyForItem(item);
      if (key && rl().sidePanelKey === key) {
        rlCloseSidePanel();
      } else {
        rlOpenSidePanel(item);
      }
    });
    rlTbody.addEventListener("contextmenu", (event) => {
      const row = event.target.closest("tr[data-rl-resource-id]");
      if (!row) return;
      const resourceId = row.getAttribute("data-rl-resource-id");
      const item = (rl().items ?? []).find(
        (it) => String(it.resource_id) === String(resourceId)
      );
      if (!item) return;
      rlShowContextMenu(event, item);
    });
  }

  // Side panel close button + jump buttons.
  const rlSideClose = $("resource-list-side-close");
  if (rlSideClose) rlSideClose.addEventListener("click", rlCloseSidePanel);

  const rlSideList = $("resource-list-side-list");
  if (rlSideList) {
    rlSideList.addEventListener("click", (event) => {
      const button = event.target.closest("[data-rl-side-jump]");
      if (!button) return;
      const idx = Number(button.getAttribute("data-rl-side-jump"));
      const ref = (rl().sidePanelItems ?? [])[idx];
      if (ref) rlJumpToVarDetails(ref);
    });
  }

  function rlSyncSearchPlaceholder() {
    const input = $("resource-list-filter");
    if (!input) return;
    input.placeholder = t("resourceListSearchPlaceholder");
  }

  // Expose for the existing __refreshResourceListView entry point.
  window.__rlRefreshResourceList = () => {
    if (rl().initialized) refreshResourceList();
  };

  // Pagination clicks (delegated).
  const rlPaginationControls = $("resource-list-pagination-controls");
  if (rlPaginationControls) {
    rlPaginationControls.addEventListener("click", (event) => {
      const action = event.target.closest("[data-rl-page-action]");
      if (action) {
        const op = action.getAttribute("data-rl-page-action");
        if (op === "prev" && rl().page > 0) {
          rl().page -= 1;
          // Pagination doesn't change the underlying COUNT — reuse the
          // cached total instead of paying for another multi-second scan.
          refreshResourceList({ refetchCount: false });
        } else if (op === "next") {
          // When total is known, clamp; otherwise trust the renderer that
          // already disabled Next on the last page.
          if (rl().totalKnown) {
            const total = Number(rl().total ?? 0);
            const totalPages = Math.max(1, Math.ceil(total / rl().pageSize));
            if (rl().page >= totalPages - 1) return;
          }
          rl().page += 1;
          refreshResourceList({ refetchCount: false });
        }
        return;
      }
      const jump = event.target.closest("[data-rl-page-jump]");
      if (jump) {
        const target = Number(jump.getAttribute("data-rl-page-jump"));
        if (Number.isFinite(target) && target !== rl().page) {
          rl().page = target;
          refreshResourceList({ refetchCount: false });
        }
      }
    });
  }

  // ===========================================================
  // Reclaim Space page wiring
  //
  // Walks the chosen folder of local VARs server-side via
  // start_reclaim_scan_task, polls progress, then renders a sortable
  // ranked table of DB packages by bytes covered. The whole module is
  // namespaced inside this IIFE so its helpers (rs(), renderRsTable…)
  // can't collide with VAR Details / Resource List symbols.
  // ===========================================================
  (function setupReclaimSpace() {
    if (!state.reclaimSpace) return;
    function rs() { return state.reclaimSpace; }

    const RS_PAGE_SIZE = 25;

    function formatPct(n) {
      if (!Number.isFinite(n)) return "—";
      if (n >= 99.95) return "100%";
      if (n >= 10) return `${n.toFixed(0)}%`;
      return `${n.toFixed(1)}%`;
    }

    function comparator() {
      const dir = rs().sortDir === "asc" ? 1 : -1;
      switch (rs().sortBy) {
        case "package":
          return (a, b) => dir * String(a.package_id).localeCompare(String(b.package_id));
        case "creator":
          return (a, b) => dir * String(a.creator_name ?? "").localeCompare(String(b.creator_name ?? ""));
        case "count":
          return (a, b) => dir * (Number(a.matched_resource_count) - Number(b.matched_resource_count));
        case "coverage":
          return (a, b) => dir * (Number(a.coverage_pct) - Number(b.coverage_pct));
        case "bytes":
        default:
          return (a, b) => dir * (Number(a.matched_bytes) - Number(b.matched_bytes));
      }
    }

    function renderRsSummary() {
      const summary = rs().summary;
      const set = (id, val) => { const el = $(id); if (el) el.textContent = val; };
      if (!summary) {
        set("reclaim-stat-vars", "—");
        set("reclaim-stat-resources", "—");
        set("reclaim-stat-bytes", "—");
        set("reclaim-stat-candidates", "—");
        return;
      }
      set("reclaim-stat-vars", String(summary.localVarsScanned ?? 0));
      set("reclaim-stat-resources", String(summary.localUniqueResources ?? 0));
      set("reclaim-stat-bytes", formatBytesLocal(Number(summary.localUniqueBytes ?? 0)));
      set("reclaim-stat-candidates", String(rs().candidates.length));
    }

    function renderRsProgress() {
      const pct = Math.max(0, Math.min(100, Math.round(Number(rs().progress) || 0)));
      const fill = $("reclaim-progress-fill");
      const label = $("reclaim-progress-percent");
      const text = $("reclaim-progress-text");
      const dot = $("reclaim-progress-dot");
      if (fill) fill.style.width = `${pct}%`;
      if (label) label.textContent = `${pct}%`;
      if (text) {
        text.textContent = rs().error
          ? rs().error
          : (rs().progressMessage || (rs().scanning ? "Working…" : "Idle"));
      }
      if (dot) {
        dot.classList.toggle("is-active", rs().scanning);
        dot.classList.toggle("is-error", Boolean(rs().error));
      }
    }

    function renderRsTable() {
      const tbody = $("reclaim-tbody");
      const countEl = $("reclaim-candidate-count");
      const summaryEl = $("reclaim-summary");
      const pagination = $("reclaim-pagination");
      const indicator = $("reclaim-page-indicator");
      if (!tbody) return;

      const candidates = Array.from(rs().candidates).sort(comparator());

      if (candidates.length === 0) {
        const message = rs().error
          ? rs().error
          : rs().scanning
            ? (rs().progressMessage || "Scanning…")
            : rs().summary
              ? "No replacement candidates found — every matching CRC belongs to a VAR already in your folder."
              : "Pick a folder and click \"Scan Folder\" to find replacement candidates.";
        tbody.innerHTML = `<tr class="var-details-empty-row"><td colspan="5">${escapeHtml(message)}</td></tr>`;
        if (countEl) countEl.textContent = rs().summary ? "0 Candidates" : "Not Scanned";
        if (summaryEl) summaryEl.textContent = "";
        if (pagination) pagination.hidden = true;
        return;
      }

      const total = candidates.length;
      const totalPages = Math.max(1, Math.ceil(total / RS_PAGE_SIZE));
      if (rs().page >= totalPages) rs().page = totalPages - 1;
      if (rs().page < 0) rs().page = 0;
      const start = rs().page * RS_PAGE_SIZE;
      const slice = candidates.slice(start, start + RS_PAGE_SIZE);

      tbody.innerHTML = slice
        .map((c) => {
          const creator = c.creator_name ?? deriveCreatorFromPackageId(c.package_id) ?? "—";
          return `
            <tr data-package-id="${escapeAttribute(c.package_id ?? "")}">
              <td class="vd-col-name" title="${escapeAttribute(c.file_path ?? "")}">${escapeHtml(c.package_id)}</td>
              <td class="vd-col-cat">${escapeHtml(creator)}</td>
              <td class="vd-col-size">${Number(c.matched_resource_count).toLocaleString()}</td>
              <td class="vd-col-size">${escapeHtml(formatBytesLocal(Number(c.matched_bytes)))}</td>
              <td class="vd-col-size">${escapeHtml(formatPct(Number(c.coverage_pct)))}</td>
            </tr>`;
        })
        .join("");

      if (countEl) countEl.textContent = `${total} Candidate${total === 1 ? "" : "s"}`;
      if (summaryEl) {
        summaryEl.textContent = `Showing ${start + 1}–${Math.min(start + slice.length, total)} of ${total}`;
      }
      if (pagination) pagination.hidden = totalPages <= 1;
      if (indicator) indicator.textContent = `${rs().page + 1} / ${totalPages}`;
    }

    function renderRsAll() {
      renderRsSummary();
      renderRsProgress();
      renderRsTable();
    }

    async function pollRsProgress(taskId) {
      let payload = null;
      while (true) {
        await new Promise((r) => setTimeout(r, 400));
        try {
          payload = await invoke("get_task_progress", { taskId });
        } catch (error) {
          rs().error = String(error);
          rs().scanning = false;
          renderRsAll();
          return null;
        }
        if (!payload) break;
        rs().progress = Math.round(Math.max(0, Math.min(1, Number(payload.progress ?? 0))) * 100);
        rs().progressMessage = String(payload.message ?? "Working…");
        renderRsProgress();
        if (payload.error) {
          rs().error = String(payload.error);
          rs().scanning = false;
          renderRsAll();
          return null;
        }
        if (payload.done) {
          return payload.reclaim_scan_result ?? null;
        }
      }
      return null;
    }

    async function runReclaimScan() {
      if (!invoke || rs().scanning) return;
      const folderInput = $("reclaim-folder-input");
      const folder = (folderInput?.value ?? "").trim();
      if (!folder) {
        rs().error = "Pick a VAR folder before scanning.";
        renderRsAll();
        return;
      }
      rs().folder = folder;
      rs().scanning = true;
      rs().error = null;
      rs().progress = 0;
      rs().progressMessage = "Starting…";
      rs().summary = null;
      rs().candidates = [];
      rs().page = 0;
      renderRsAll();

      let taskId = null;
      try {
        const handle = await invoke("start_reclaim_scan_task", {
          request: {
            folder,
            additional_folders: getAdditionalDirs("reclaim"),
            include_vap: false,
          },
        });
        taskId = handle?.id ?? null;
        rs().scanTaskId = taskId;
      } catch (error) {
        rs().error = String(error);
        rs().scanning = false;
        renderRsAll();
        return;
      }
      if (!taskId) {
        rs().scanning = false;
        renderRsAll();
        return;
      }

      let result = null;
      try {
        result = await pollRsProgress(taskId);
      } finally {
        try {
          await invoke("clear_task", { taskId });
        } catch (_e) {}
      }

      rs().scanning = false;
      rs().scanTaskId = null;
      if (result) {
        rs().summary = {
          localVarsScanned: Number(result.local_vars_scanned ?? 0),
          localUniqueResources: Number(result.local_unique_resources ?? 0),
          localUniqueBytes: Number(result.local_unique_bytes ?? 0),
        };
        rs().candidates = Array.isArray(result.candidates) ? result.candidates : [];
        rs().progress = 100;
        rs().progressMessage = `Scanned ${rs().summary.localVarsScanned} VAR${
          rs().summary.localVarsScanned === 1 ? "" : "s"
        }`;
      }
      renderRsAll();
    }

    // ---- Wire up DOM ----
    const folderInput = $("reclaim-folder-input");
    if (folderInput) {
      folderInput.addEventListener("input", () => {
        rs().folder = folderInput.value;
        if (rs().error) {
          rs().error = null;
          renderRsProgress();
        }
      });
      folderInput.addEventListener("keydown", (event) => {
        if (event.key === "Enter") {
          event.preventDefault();
          runReclaimScan();
        }
      });
    }

    const pick = $("reclaim-folder-pick");
    if (pick) {
      pick.addEventListener("click", async () => {
        if (!invoke) return;
        try {
          const selected = await invoke("pick_folder");
          if (selected) {
            if (folderInput) folderInput.value = String(selected);
            rs().folder = String(selected);
          }
        } catch (error) {
          addLog(`Reclaim Space: folder picker failed — ${String(error)}`);
        }
      });
    }

    const scanBtn = $("reclaim-scan-button");
    if (scanBtn) {
      scanBtn.addEventListener("click", () => { runReclaimScan(); });
    }

    document.querySelectorAll("#reclaim-space-view [data-reclaim-sort]").forEach((th) => {
      th.addEventListener("click", () => {
        const key = th.getAttribute("data-reclaim-sort");
        if (!key) return;
        if (rs().sortBy === key) {
          rs().sortDir = rs().sortDir === "desc" ? "asc" : "desc";
        } else {
          rs().sortBy = key;
          rs().sortDir = key === "package" || key === "creator" ? "asc" : "desc";
        }
        renderRsTable();
      });
    });

    const prev = $("reclaim-page-prev");
    const next = $("reclaim-page-next");
    if (prev) prev.addEventListener("click", () => {
      if (rs().page > 0) { rs().page -= 1; renderRsTable(); }
    });
    if (next) next.addEventListener("click", () => {
      const total = rs().candidates.length;
      const totalPages = Math.max(1, Math.ceil(total / RS_PAGE_SIZE));
      if (rs().page < totalPages - 1) { rs().page += 1; renderRsTable(); }
    });

    // Delegated row-click on the reclaim candidates table → open VAR Details
    // for the clicked package. Candidates are *DB matches* from the catalog
    // (packages that could replace local content), not the files in the
    // scanned folder — their file_path is just an indexed hint and may be
    // stale on disk. Pass source="db" so the details view harvests CRCs from
    // the index instead of trying to open the .var from disk.
    const rsTbody = $("reclaim-tbody");
    if (rsTbody) {
      rsTbody.addEventListener("click", (event) => {
        const row = event.target.closest("tr[data-package-id]");
        if (!row) return;
        const packageId = row.getAttribute("data-package-id");
        if (!packageId) return;
        const candidate = (rs().candidates ?? []).find((c) => c.package_id === packageId);
        if (!candidate) return;
        const filePath = candidate.file_path ?? "";
        const fileName = filePath ? filePath.split(/[\\/]/).pop() : candidate.package_id;
        const item = {
          package_id: candidate.package_id,
          file_path: filePath,
          file_name: fileName,
          creator: candidate.creator_name ?? deriveCreatorFromPackageId(candidate.package_id) ?? null,
        };
        openVarDetailsView(item, "db");
      });
    }

    // Sidebar entry point: re-render with whatever state is already cached.
    // Never auto-scans — user must press the Scan Folder button.
    window.__refreshReclaimSpaceView = function () {
      const input = $("reclaim-folder-input");
      if (input && !input.value && rs().folder) input.value = rs().folder;
      renderRsAll();
    };

    // Initial paint so the empty state shows on first load.
    renderRsAll();
  })();

  // ===========================================================
  // Unique Resources page wiring
  //
  // Local-only: walks the picked folder via start_unique_resources_task,
  // dedupes resources by (crc32, size), and lists every unique resource
  // along with each VAR that contains it. No DB use anywhere in the flow.
  // ===========================================================
  (function setupUniqueResources() {
    if (!state.uniqueResources) return;
    function ur() { return state.uniqueResources; }

    const UR_PAGE_SIZE = 50;

    function matchesFilter(r, needle) {
      if (!needle) return true;
      const n = needle.toLowerCase();
      if ((r.representative_path || "").toLowerCase().includes(n)) return true;
      if ((r.crc32_hex || "").toLowerCase().includes(n)) return true;
      const sources = r.sources || [];
      for (let i = 0; i < sources.length; i += 1) {
        const s = sources[i] || {};
        if ((s.package_id || "").toLowerCase().includes(n)) return true;
        if ((s.internal_path || "").toLowerCase().includes(n)) return true;
      }
      return false;
    }

    function comparator() {
      const dir = ur().sortDir === "asc" ? 1 : -1;
      switch (ur().sortBy) {
        case "resource":
          return (a, b) => dir * String(a.representative_path || "").localeCompare(String(b.representative_path || ""));
        case "sources":
          return (a, b) => dir * (Number(a.source_count) - Number(b.source_count));
        case "unique":
          return (a, b) => dir * (Number(a.unique_size) - Number(b.unique_size));
        case "reclaimable":
          return (a, b) => dir * ((Number(a.combined_size) - Number(a.unique_size)) - (Number(b.combined_size) - Number(b.unique_size)));
        case "combined":
        default:
          return (a, b) => dir * (Number(a.combined_size) - Number(b.combined_size));
      }
    }

    function filteredResources() {
      const needle = (ur().filter || "").trim();
      const arr = needle
        ? ur().resources.filter((r) => matchesFilter(r, needle))
        : ur().resources.slice();
      arr.sort(comparator());
      return arr;
    }

    function renderUrSummary() {
      const summary = ur().summary;
      const set = (id, val) => { const el = $(id); if (el) el.textContent = val; };
      if (!summary) {
        set("ur-stat-vars", "—");
        set("ur-stat-resources", "—");
        set("ur-stat-unique-bytes", "—");
        set("ur-stat-reclaimable", "—");
        return;
      }
      set("ur-stat-vars", String(summary.varsScanned ?? 0));
      set("ur-stat-resources", String(summary.uniqueResources ?? 0));
      set("ur-stat-unique-bytes", formatBytesLocal(Number(summary.totalUniqueBytes ?? 0)));
      set("ur-stat-reclaimable", formatBytesLocal(Number(summary.reclaimableBytes ?? 0)));
    }

    function renderUrProgress() {
      const pct = Math.max(0, Math.min(100, Math.round(Number(ur().progress) || 0)));
      const fill = $("ur-progress-fill");
      const label = $("ur-progress-percent");
      const text = $("ur-progress-text");
      const dot = $("ur-progress-dot");
      const card = $("ur-progress-card");
      if (fill) fill.style.width = `${pct}%`;
      if (label) label.textContent = `${pct}%`;
      if (text) {
        text.textContent = ur().error
          ? ur().error
          : (ur().progressMessage || (ur().scanning ? "Working…" : "Idle"));
      }
      if (dot) {
        dot.classList.toggle("is-active", ur().scanning);
        dot.classList.toggle("is-error", Boolean(ur().error));
      }
      // Hide the card entirely when there's nothing to report so the list +
      // side panel reclaim its vertical space. Visible during scans or when
      // an error needs to be surfaced.
      if (card) card.classList.toggle("hidden", !ur().scanning && !ur().error);
    }

    function renderUrTable() {
      const list = $("ur-list");
      const countEl = $("ur-resource-count");
      const summaryEl = $("ur-summary");
      const pagination = $("ur-pagination");
      const indicator = $("ur-page-indicator");
      const clearBtn = $("ur-filter-clear-button");
      if (!list) return;

      if (clearBtn) clearBtn.classList.toggle("hidden", !(ur().filter && ur().filter.trim()));

      const resources = filteredResources();

      if (resources.length === 0) {
        const message = ur().error
          ? ur().error
          : ur().scanning
            ? (ur().progressMessage || "Scanning…")
            : ur().summary
              ? (ur().filter && ur().filter.trim()
                  ? "No resources match this filter."
                  : "No unique resources found in this folder.")
              : "Pick a folder and click \"Scan Folder\" to list every unique resource.";
        list.classList.add("empty");
        list.innerHTML = `<div class="detail-empty">${escapeHtml(message)}</div>`;
        if (countEl) {
          countEl.textContent = ur().summary
            ? `0 / ${Number(ur().resources.length).toLocaleString()} Resources`
            : "Not Scanned";
        }
        if (summaryEl) summaryEl.textContent = "";
        if (pagination) pagination.hidden = true;
        renderUrSortPills();
        return;
      }

      const total = resources.length;
      const totalPages = Math.max(1, Math.ceil(total / UR_PAGE_SIZE));
      if (ur().page >= totalPages) ur().page = totalPages - 1;
      if (ur().page < 0) ur().page = 0;
      const start = ur().page * UR_PAGE_SIZE;
      const slice = resources.slice(start, start + UR_PAGE_SIZE);

      list.classList.remove("empty");
      list.innerHTML = slice
        .map((r) => {
          const sourceCount = Number(r.source_count);
          const reclaimable = Math.max(0, Number(r.combined_size) - Number(r.unique_size));
          const isSelected = ur().selectedKey === r.crc32_hex ? "active focused" : "";
          const sourcesChipClass = sourceCount > 1 ? "chip chip-accent" : "chip";
          const sourcesLabel = `× ${sourceCount.toLocaleString()} source${sourceCount === 1 ? "" : "s"}`;
          const path = r.representative_path ?? "";
          return `
            <button class="group-row ${isSelected}" data-crc-hex="${escapeAttribute(r.crc32_hex ?? "")}" type="button">
              <div class="group-row-top">
                <strong title="${escapeAttribute(path)}">${escapeHtml(path)}</strong>
                <span class="${sourcesChipClass}">${escapeHtml(sourcesLabel)}</span>
              </div>
              <div class="group-row-meta">
                <span class="group-row-size">${escapeHtml(formatBytesLocal(Number(r.unique_size)))}</span>
                <span>combined ${escapeHtml(formatBytesLocal(Number(r.combined_size)))}</span>
                <span>reclaimable ${escapeHtml(formatBytesLocal(reclaimable))}</span>
              </div>
            </button>`;
        })
        .join("");

      const totalAll = Number(ur().resources.length);
      if (countEl) {
        countEl.textContent = total === totalAll
          ? `${total.toLocaleString()} Resource${total === 1 ? "" : "s"}`
          : `${total.toLocaleString()} / ${totalAll.toLocaleString()} Resources`;
      }
      if (summaryEl) {
        summaryEl.textContent = `Showing ${start + 1}–${Math.min(start + slice.length, total)} of ${total}`;
      }
      if (pagination) pagination.hidden = totalPages <= 1;
      if (indicator) indicator.textContent = `${ur().page + 1} / ${totalPages}`;
      renderUrSortPills();
    }

    function renderUrSortPills() {
      const group = $("ur-sort-group");
      if (!group) return;
      const activeKey = ur().sortBy;
      const arrow = ur().sortDir === "asc" ? " ▲" : " ▼";
      group.querySelectorAll("[data-ur-sort]").forEach((button) => {
        const key = button.getAttribute("data-ur-sort");
        const isActive = key === activeKey;
        button.classList.toggle("is-active", isActive);
        button.setAttribute("aria-selected", isActive ? "true" : "false");
        // Label without the arrow lives in the button's first text node; we
        // store it once on data-label so re-renders don't accumulate arrows.
        if (!button.dataset.label) button.dataset.label = (button.textContent || "").trim();
        button.textContent = isActive ? `${button.dataset.label}${arrow}` : button.dataset.label;
      });
    }

    // O(2) DOM update for "user picked a different row" — toggle .active.focused
    // on the prev/next row instead of re-running renderUrTable's full innerHTML
    // rebuild. Both keys are crc32_hex; either may be null when the change is
    // "no selection → selected" or "selected → cleared".
    function updateUrSelectionClass(prevHex, nextHex) {
      const container = $("ur-list");
      if (!container) return;
      if (prevHex) {
        const prevRow = container.querySelector(`button[data-crc-hex="${cssAttrEscape(prevHex)}"]`);
        if (prevRow) prevRow.classList.remove("active", "focused");
      }
      if (nextHex) {
        const nextRow = container.querySelector(`button[data-crc-hex="${cssAttrEscape(nextHex)}"]`);
        if (nextRow) {
          nextRow.classList.add("active", "focused");
          nextRow.scrollIntoView({ block: "nearest" });
        }
      }
    }

    // CSS.escape isn't always available in the embedded webview; crc32 hex is
    // [0-9A-F]{8} so a plain pass-through is safe, but keep the helper for
    // future-proofing.
    function cssAttrEscape(value) {
      if (typeof CSS !== "undefined" && typeof CSS.escape === "function") return CSS.escape(value);
      return String(value).replace(/"/g, '\\"');
    }

    // Keyboard navigation across the visible (filtered + paginated) slice of
    // resources. Wraps within the current page. Returns true when the key was
    // handled so the caller can preventDefault.
    function handleUrKeyboardNav(key) {
      if (key !== "ArrowDown" && key !== "ArrowUp" && key !== "Home" && key !== "End") return false;
      const resources = filteredResources();
      if (!resources.length) return false;
      const start = ur().page * UR_PAGE_SIZE;
      const slice = resources.slice(start, start + UR_PAGE_SIZE);
      if (!slice.length) return false;
      const currentIdx = slice.findIndex((r) => r.crc32_hex === ur().selectedKey);
      let nextIdx;
      switch (key) {
        case "Home":
          nextIdx = 0;
          break;
        case "End":
          nextIdx = slice.length - 1;
          break;
        case "ArrowDown":
          nextIdx = currentIdx < 0 ? 0 : Math.min(slice.length - 1, currentIdx + 1);
          break;
        case "ArrowUp":
          nextIdx = currentIdx < 0 ? slice.length - 1 : Math.max(0, currentIdx - 1);
          break;
      }
      const nextHex = slice[nextIdx]?.crc32_hex ?? null;
      if (!nextHex || nextHex === ur().selectedKey) return true;
      const prev = ur().selectedKey;
      ur().selectedKey = nextHex;
      updateUrSelectionClass(prev, nextHex);
      renderUrSidePanel();
      return true;
    }

    // Apply / reflect the topCollapsed state on the top grid and the toggle
    // button. Called whenever scan state changes or the user clicks the toggle.
    function renderUrTopCollapse() {
      const grid = $("ur-top-grid");
      const toggle = $("ur-summary-toggle");
      const hasSummary = Boolean(ur().summary);
      if (toggle) {
        toggle.classList.toggle("hidden", !hasSummary);
        toggle.textContent = ur().topCollapsed ? "Show details" : "Hide details";
        toggle.setAttribute("aria-pressed", ur().topCollapsed ? "true" : "false");
      }
      if (grid) grid.classList.toggle("is-collapsed", hasSummary && ur().topCollapsed);
    }

    function renderUrSidePanel() {
      const list = $("ur-side-list");
      const empty = $("ur-side-empty");
      const meta = $("ur-side-meta");
      const subtitle = $("ur-side-subtitle");
      const title = $("ur-side-title");
      const previewSlot = $("ur-side-preview");

      const key = ur().selectedKey;
      const resource = key
        ? (ur().resources || []).find((r) => r.crc32_hex === key)
        : null;
      // Stale selection (e.g. after a new scan) — drop it silently.
      if (key && !resource) ur().selectedKey = null;

      if (!resource) {
        if (title) title.textContent = "Sources";
        if (subtitle) subtitle.textContent = "Every VAR containing the selected resource.";
        if (meta) meta.textContent = "";
        if (empty) empty.classList.remove("hidden");
        if (list) list.innerHTML = "";
        if (previewSlot) { previewSlot.classList.add("hidden"); previewSlot.innerHTML = ""; }
        return;
      }

      if (title) title.textContent = resource.representative_path || "Sources";
      if (subtitle) subtitle.textContent = `CRC32 ${resource.crc32_hex} · ${formatBytesLocal(Number(resource.unique_size))} each`;
      if (meta) {
        const reclaimable = Math.max(0, Number(resource.combined_size) - Number(resource.unique_size));
        meta.textContent = `${Number(resource.source_count).toLocaleString()} source${resource.source_count === 1 ? "" : "s"} · combined ${formatBytesLocal(Number(resource.combined_size))} · reclaimable ${formatBytesLocal(reclaimable)}`;
      }

      const sources = resource.sources || [];
      if (empty) empty.classList.toggle("hidden", sources.length > 0);
      renderUrSourceList(resource);

      renderUrPreview(resource);
      syncUrSidePanelHeight();
    }

    // Row height for virtualization (matches actual rendered footprint: head +
    // 2 paths + footer with actions). Keep in sync with .resource-list-side-item
    // padding in styles.css. Used only when sources.length > THRESHOLD.
    const UR_SOURCE_ROW_HEIGHT = 110;
    const UR_SOURCE_ROW_GAP = 8;
    const UR_SOURCE_VIRT_THRESHOLD = 60;
    const UR_SOURCE_BUFFER_ROWS = 4;

    // Per-render virtualization scratch: tracks which slice is currently in
    // the DOM so the scroll listener can skip work if the visible window
    // hasn't moved enough to reveal new rows.
    const urSourcesView = {
      sources: [],
      resourceKey: null,
      mode: "none", // "all" | "virtual"
      windowFrom: -1,
      windowTo: -1,
    };

    function buildUrSourceRowHtml(s, idx, resource) {
      const packageId = s.package_id ?? "";
      const internalPath = s.internal_path ?? "";
      const filePath = s.file_path ?? "";
      return `
        <li class="resource-list-side-item" data-ur-source-idx="${idx}">
          <div class="resource-list-side-item-head">
            <span class="resource-list-side-item-pkg">${escapeHtml(packageId)}</span>
          </div>
          <div class="resource-list-side-item-path">${escapeHtml(internalPath)}</div>
          <div class="resource-list-side-item-path" title="${escapeAttribute(filePath)}">${escapeHtml(filePath)}</div>
          <div class="resource-list-side-item-foot">
            <span class="resource-list-side-item-size">${escapeHtml(formatBytesLocal(Number(resource.unique_size)))}</span>
            <span class="ur-side-actions">
              <button type="button" class="resource-list-side-item-jump" data-ur-action="explorer" data-ur-source-idx="${idx}" title="${escapeAttribute(t("menuShowInExplorer"))}">
                <span class="material-symbols-outlined">folder_open</span>
                <span>${escapeHtml(t("menuShowInExplorer"))}</span>
              </button>
              <button type="button" class="resource-list-side-item-jump" data-ur-action="var-details" data-ur-source-idx="${idx}" title="${escapeAttribute(t("resourceListContextOpenPackage"))}">
                <span class="material-symbols-outlined">open_in_new</span>
                <span>${escapeHtml(t("resourceListContextOpenPackage"))}</span>
              </button>
            </span>
          </div>
        </li>`;
    }

    function renderUrSourceList(resource) {
      const list = $("ur-side-list");
      if (!list) return;
      const sources = resource?.sources || [];
      urSourcesView.sources = sources;
      urSourcesView.resourceKey = resource?.crc32_hex ?? null;
      urSourcesView.windowFrom = -1;
      urSourcesView.windowTo = -1;

      if (sources.length === 0) {
        urSourcesView.mode = "none";
        list.innerHTML = "";
        return;
      }

      if (sources.length <= UR_SOURCE_VIRT_THRESHOLD) {
        urSourcesView.mode = "all";
        list.scrollTop = 0;
        // Simple render — flat <li> children, no spacer / window wrappers.
        list.innerHTML = sources.map((s, idx) => buildUrSourceRowHtml(s, idx, resource)).join("");
        return;
      }

      urSourcesView.mode = "virtual";
      const stride = UR_SOURCE_ROW_HEIGHT + UR_SOURCE_ROW_GAP;
      const totalHeight = sources.length * stride;
      // The list itself stops being a flex container in virtual mode; the
      // spacer carries the full scroll height and the window is absolutely
      // positioned inside it.
      list.innerHTML = `
        <div class="ur-side-list-spacer" style="height: ${totalHeight}px;">
          <div class="ur-side-list-window" id="ur-side-list-window"></div>
        </div>`;
      list.scrollTop = 0;
      renderUrSourceWindow(resource);
    }

    function renderUrSourceWindow(resource) {
      const list = $("ur-side-list");
      const win = $("ur-side-list-window");
      if (!list || !win || urSourcesView.mode !== "virtual") return;
      const sources = urSourcesView.sources;
      if (!sources.length) return;
      const stride = UR_SOURCE_ROW_HEIGHT + UR_SOURCE_ROW_GAP;
      const viewportH = list.clientHeight || 1;
      const scrollTop = list.scrollTop;
      const firstVisible = Math.floor(scrollTop / stride);
      const visibleCount = Math.ceil(viewportH / stride);
      const from = Math.max(0, firstVisible - UR_SOURCE_BUFFER_ROWS);
      const to = Math.min(sources.length, firstVisible + visibleCount + UR_SOURCE_BUFFER_ROWS);
      if (from === urSourcesView.windowFrom && to === urSourcesView.windowTo) return;
      urSourcesView.windowFrom = from;
      urSourcesView.windowTo = to;
      const slice = sources.slice(from, to);
      // Translate the window so the first rendered row aligns with the
      // matching slot of the underlying scroll height.
      win.style.transform = `translateY(${from * stride}px)`;
      win.innerHTML = slice.map((s, i) => buildUrSourceRowHtml(s, from + i, resource)).join("");
    }

    // Render an image preview for the selected unique resource into the side
    // panel, reusing the shared preview cache and markup helpers so .vam
    // bundles and bare jpg/png look identical to Overview / Find Duplicates.
    function renderUrPreview(resource) {
      const slot = $("ur-side-preview");
      if (!slot) return;
      const sources = resource?.sources ?? [];
      // Pick the first source whose internal_path is renderable — all sources
      // share the same content (same CRC32 + size), so any previewable one
      // will produce the same image. The on-disk .var path comes from the
      // source itself; there's no scan map to consult here.
      const source = sources.find((s) => isPreviewablePath(s.internal_path));
      if (!source || !source.file_path) {
        slot.classList.add("hidden");
        slot.innerHTML = "";
        return;
      }
      const ref = { package_id: source.package_id, internal_path: source.internal_path };
      slot.classList.remove("hidden");
      slot.className = "vam-preview detail-preview";
      slot.innerHTML = buildPreviewMarkup(ref, source.file_path);
      bindPreviewResolution(slot);
      urBindPreviewActions(slot);
      const cacheKey = getPreviewCacheKey(ref.package_id, ref.internal_path);
      if (!state.previewCache[cacheKey]) {
        void urEnsurePreviewLoaded(ref, source.file_path);
      }
    }

    async function urEnsurePreviewLoaded(ref, packageFile) {
      if (!invoke || !packageFile || !isPreviewablePath(ref.internal_path)) return;
      const cacheKey = getPreviewCacheKey(ref.package_id, ref.internal_path);
      const current = state.previewCache[cacheKey];
      if (current?.status === "loading" || current?.status === "ready") return;
      state.previewCache[cacheKey] = { status: "loading", startedAt: Date.now() };
      const slot = $("ur-side-preview");
      if (slot && !slot.classList.contains("hidden")) {
        slot.innerHTML = buildPreviewMarkup(ref, packageFile);
      }
      try {
        const data = await Promise.race([
          invoke("get_vam_preview", {
            packageId: ref.package_id,
            packagePath: packageFile,
            vamPath: ref.internal_path,
          }),
          new Promise((_, reject) =>
            setTimeout(() => reject(new Error("Preview request timed out")), 10000)
          ),
        ]);
        state.previewCache[cacheKey] = { status: "ready", data, finishedAt: Date.now() };
      } catch (error) {
        state.previewCache[cacheKey] = { status: "error", error: String(error), finishedAt: Date.now() };
      }
      // Re-render only if the user hasn't moved on to a different resource.
      const stillSelected = (ur().resources || []).find((r) => r.crc32_hex === ur().selectedKey);
      if (stillSelected) renderUrPreview(stillSelected);
    }

    // Deferred-large-image button handler. The shared bindPreviewActions
    // resolves the .var path through state.scan.package_files (Overview /
    // Find Duplicates state) which UR doesn't populate, so handle the click
    // here using the UR-selected resource's own source.
    function urBindPreviewActions(slot) {
      slot.querySelectorAll("[data-preview-load]").forEach((button) => {
        button.addEventListener("click", async () => {
          const cacheKey = button.dataset.previewCacheKey;
          const index = Number(button.dataset.previewIndex ?? -1);
          if (!cacheKey || index < 0) return;
          const entry = state.previewCache[cacheKey];
          const image = entry?.data?.images?.[index];
          const resource = (ur().resources || []).find((r) => r.crc32_hex === ur().selectedKey);
          const source = resource?.sources?.find((s) => isPreviewablePath(s.internal_path));
          if (!image || !source?.file_path || !invoke) return;
          button.disabled = true;
          button.textContent = t("previewLoading");
          try {
            const dataUrl = await invoke("load_preview_image_data", {
              packagePath: source.file_path,
              internalPath: image.internal_path,
            });
            const img = document.createElement("img");
            img.className = "vam-preview-image";
            img.src = dataUrl;
            img.alt = image.internal_path;
            img.loading = "lazy";
            img.dataset.resolutionId = `${cacheKey}:${index}`;
            button.replaceWith(img);
            bindPreviewResolution(slot);
          } catch (error) {
            button.disabled = false;
            button.textContent = `${t("previewError")}: ${String(error)}`;
          }
        });
      });
    }

    function renderUrAll() {
      renderUrSummary();
      renderUrProgress();
      renderUrTopCollapse();
      renderUrTable();
      renderUrSidePanel();
      syncUrSidePanelHeight();
    }

    // Match the right (sources) panel to the left (resources list) panel
    // height so the source list scrolls inside the panel for resources with
    // hundreds of refs instead of stretching the row to thousands of pixels.
    function syncUrSidePanelHeight() {
      const leftPanel = document.querySelector("#unique-resources-view .var-details-browser");
      const rightPanel = $("ur-side-panel");
      if (!leftPanel || !rightPanel) return;
      // Defer to next frame so layout has settled after the latest render.
      requestAnimationFrame(() => {
        const h = leftPanel.offsetHeight;
        if (h > 0) rightPanel.style.maxHeight = `${h}px`;
        // After the right panel is sized, re-window the virtualized source
        // list — the initial render may have computed visible rows against a
        // zero-height container before layout settled.
        if (urSourcesView.mode === "virtual") {
          const resource = (ur().resources || []).find((r) => r.crc32_hex === urSourcesView.resourceKey);
          if (resource) {
            urSourcesView.windowFrom = -1;
            urSourcesView.windowTo = -1;
            renderUrSourceWindow(resource);
          }
        }
      });
    }

    // Re-sync when the window resizes (left panel grows / shrinks with the
    // viewport) and when the left panel itself changes size (pagination,
    // filter changes, scan completes).
    window.addEventListener("resize", syncUrSidePanelHeight);
    if (typeof ResizeObserver !== "undefined") {
      const leftEl = document.querySelector("#unique-resources-view .var-details-browser");
      if (leftEl) new ResizeObserver(syncUrSidePanelHeight).observe(leftEl);
    }

    async function pollUrProgress(taskId) {
      let payload = null;
      while (true) {
        await new Promise((r) => setTimeout(r, 400));
        try {
          payload = await invoke("get_task_progress", { taskId });
        } catch (error) {
          ur().error = String(error);
          ur().scanning = false;
          renderUrAll();
          return null;
        }
        if (!payload) break;
        ur().progress = Math.round(Math.max(0, Math.min(1, Number(payload.progress ?? 0))) * 100);
        ur().progressMessage = String(payload.message ?? "Working…");
        renderUrProgress();
        if (payload.error) {
          ur().error = String(payload.error);
          ur().scanning = false;
          renderUrAll();
          return null;
        }
        if (payload.done) {
          return payload.unique_resources_result ?? null;
        }
      }
      return null;
    }

    async function runUniqueResourcesScan() {
      if (!invoke || ur().scanning) return;
      const folderInput = $("ur-folder-input");
      const folder = (folderInput?.value ?? "").trim();
      if (!folder) {
        ur().error = "Pick a VAR folder before scanning.";
        renderUrAll();
        return;
      }
      ur().folder = folder;
      ur().scanning = true;
      ur().error = null;
      ur().progress = 0;
      ur().progressMessage = "Starting…";
      ur().summary = null;
      ur().resources = [];
      ur().page = 0;
      ur().selectedKey = null;
      renderUrAll();

      let taskId = null;
      try {
        const handle = await invoke("start_unique_resources_task", {
          request: {
            folder,
            additional_folders: getAdditionalDirs("unique"),
            include_vap: false,
          },
        });
        taskId = handle?.id ?? null;
        ur().scanTaskId = taskId;
      } catch (error) {
        ur().error = String(error);
        ur().scanning = false;
        renderUrAll();
        return;
      }
      if (!taskId) {
        ur().scanning = false;
        renderUrAll();
        return;
      }

      let result = null;
      try {
        result = await pollUrProgress(taskId);
      } finally {
        try {
          await invoke("clear_task", { taskId });
        } catch (_e) {}
      }

      ur().scanning = false;
      ur().scanTaskId = null;
      if (result) {
        ur().summary = {
          varsScanned: Number(result.vars_scanned ?? 0),
          uniqueResources: Number(result.unique_resources ?? 0),
          totalUniqueBytes: Number(result.total_unique_bytes ?? 0),
          totalCombinedBytes: Number(result.total_combined_bytes ?? 0),
          reclaimableBytes: Number(result.reclaimable_bytes ?? 0),
        };
        ur().resources = Array.isArray(result.resources) ? result.resources : [];
        ur().progress = 100;
        ur().progressMessage = `Scanned ${ur().summary.varsScanned} VAR${
          ur().summary.varsScanned === 1 ? "" : "s"
        }`;
        // Auto-collapse the top cards now that the scan has produced data —
        // the user is here to browse results, not re-pick the folder. The
        // toggle remains available for re-scans / folder changes.
        ur().topCollapsed = true;
      }
      renderUrAll();
    }

    // ---- Wire up DOM ----
    const folderInput = $("ur-folder-input");
    if (folderInput) {
      folderInput.addEventListener("input", () => {
        ur().folder = folderInput.value;
        if (ur().error) {
          ur().error = null;
          renderUrProgress();
        }
      });
      folderInput.addEventListener("keydown", (event) => {
        if (event.key === "Enter") {
          event.preventDefault();
          runUniqueResourcesScan();
        }
      });
    }

    const pick = $("ur-folder-pick");
    if (pick) {
      pick.addEventListener("click", async () => {
        if (!invoke) return;
        try {
          const selected = await invoke("pick_folder");
          if (selected) {
            if (folderInput) folderInput.value = String(selected);
            ur().folder = String(selected);
          }
        } catch (error) {
          addLog(`Unique Resources: folder picker failed — ${String(error)}`);
        }
      });
    }

    const scanBtn = $("ur-scan-button");
    if (scanBtn) {
      scanBtn.addEventListener("click", () => { runUniqueResourcesScan(); });
    }

    const filterInput = $("ur-filter");
    if (filterInput) {
      filterInput.addEventListener("input", () => {
        ur().filter = filterInput.value;
        ur().page = 0;
        renderUrTable();
      });
    }
    const filterClear = $("ur-filter-clear-button");
    if (filterClear) {
      filterClear.addEventListener("click", () => {
        ur().filter = "";
        ur().page = 0;
        if (filterInput) filterInput.value = "";
        renderUrTable();
      });
    }

    document.querySelectorAll("#unique-resources-view [data-ur-sort]").forEach((th) => {
      th.addEventListener("click", () => {
        const key = th.getAttribute("data-ur-sort");
        if (!key) return;
        if (ur().sortBy === key) {
          ur().sortDir = ur().sortDir === "desc" ? "asc" : "desc";
        } else {
          ur().sortBy = key;
          ur().sortDir = key === "resource" ? "asc" : "desc";
        }
        renderUrTable();
      });
    });

    const prev = $("ur-page-prev");
    const next = $("ur-page-next");
    if (prev) prev.addEventListener("click", () => {
      if (ur().page > 0) { ur().page -= 1; renderUrTable(); }
    });
    if (next) next.addEventListener("click", () => {
      const total = filteredResources().length;
      const totalPages = Math.max(1, Math.ceil(total / UR_PAGE_SIZE));
      if (ur().page < totalPages - 1) { ur().page += 1; renderUrTable(); }
    });

    const list = $("ur-list");
    if (list) {
      list.addEventListener("click", (event) => {
        const row = event.target.closest("button[data-crc-hex]");
        if (!row) return;
        const crcHex = row.getAttribute("data-crc-hex");
        if (!crcHex) return;
        // Toggle: clicking the already-selected row closes the panel.
        const prev = ur().selectedKey;
        const next = prev === crcHex ? null : crcHex;
        ur().selectedKey = next;
        // Selection-only change: toggle the .active.focused classes on the
        // affected rows instead of re-running renderUrTable's full innerHTML
        // rebuild. This preserves scroll position and is O(2) DOM writes.
        updateUrSelectionClass(prev, next);
        renderUrSidePanel();
      });
    }

    // Keyboard nav: arrow keys move selection through the currently filtered
    // resources, Home/End jump to the ends, and the moved-to row is scrolled
    // into view. Tied to the left list container so it only fires when the
    // user has interacted with the resources panel.
    if (list) {
      list.addEventListener("keydown", (event) => {
        if (event.altKey || event.ctrlKey || event.metaKey) return;
        const handled = handleUrKeyboardNav(event.key);
        if (handled) {
          event.preventDefault();
          event.stopPropagation();
        }
      });
    }

    function getSelectedUrSource(idx) {
      const resource = (ur().resources || []).find((r) => r.crc32_hex === ur().selectedKey);
      return resource?.sources?.[idx] ?? null;
    }

    const sideList = $("ur-side-list");
    if (sideList) {
      sideList.addEventListener("click", (event) => {
        const button = event.target.closest("button[data-ur-action]");
        if (!button) return;
        const action = button.getAttribute("data-ur-action");
        const idx = Number(button.getAttribute("data-ur-source-idx") ?? -1);
        const source = idx >= 0 ? getSelectedUrSource(idx) : null;
        if (!source) return;
        if (action === "explorer") {
          if (!source.file_path) return;
          showPackageInExplorer(source.file_path).catch((err) => {
            addLog(`Show in Explorer failed: ${String(err)} — path: ${source.file_path}`);
          });
        } else if (action === "var-details") {
          if (!source.package_id) return;
          openSourceRowInVarDetails(source.package_id, source.file_path || "", "local");
        }
      });
      sideList.addEventListener("contextmenu", (event) => {
        const item = event.target.closest("li[data-ur-source-idx]");
        if (!item) return;
        event.preventDefault();
        hideContextMenu();
        const idx = Number(item.getAttribute("data-ur-source-idx") ?? -1);
        const source = idx >= 0 ? getSelectedUrSource(idx) : null;
        if (!source) return;
        const menu = [];
        if (source.package_id) {
          menu.push({
            label: t("resourceListContextOpenPackage"),
            action: () => openSourceRowInVarDetails(source.package_id, source.file_path || "", "local"),
          });
        }
        if (source.file_path) {
          menu.push({
            label: t("menuShowInExplorer"),
            action: () => {
              showPackageInExplorer(source.file_path).catch((err) => {
                addLog(`Show in Explorer failed: ${String(err)} — path: ${source.file_path}`);
              });
            },
          });
          menu.push({
            label: "Copy file path",
            action: () => {
              copyTextToClipboard(source.file_path).then((ok) => {
                if (ok) addLog(`Copied ${source.file_path}`);
              });
            },
          });
        }
        if (menu.length) showContextMenu(event.clientX, event.clientY, menu);
      });

      // Virtualized scroll handler: only fires re-window work when the user
      // is in virtual mode (large source counts) and the visible slice has
      // moved enough to need new rows. rAF-coalesced to one per frame.
      let scrollRaf = 0;
      sideList.addEventListener("scroll", () => {
        if (urSourcesView.mode !== "virtual") return;
        if (scrollRaf) return;
        scrollRaf = requestAnimationFrame(() => {
          scrollRaf = 0;
          const resource = (ur().resources || []).find((r) => r.crc32_hex === urSourcesView.resourceKey);
          if (resource) renderUrSourceWindow(resource);
        });
      }, { passive: true });
    }

    // Local Summary collapse toggle: hides the Source Folder card and shrinks
    // the stat tiles to a horizontal strip so the list + side panel get back
    // ~200px after the first scan completes.
    const summaryToggle = $("ur-summary-toggle");
    if (summaryToggle) {
      summaryToggle.addEventListener("click", () => {
        ur().topCollapsed = !ur().topCollapsed;
        renderUrTopCollapse();
        syncUrSidePanelHeight();
      });
    }

    // Sidebar entry point: re-render with whatever state is already cached.
    // Never auto-scans — user must press the Scan Folder button.
    window.__refreshUniqueResourcesView = function () {
      const input = $("ur-folder-input");
      if (input && !input.value && ur().folder) input.value = ur().folder;
      const filterEl = $("ur-filter");
      if (filterEl && filterEl.value !== ur().filter) filterEl.value = ur().filter || "";
      renderUrAll();
    };

    // Initial paint so the empty state shows on first load.
    renderUrAll();
  })();

  // ===========================================================
  // Missing Resources page (sidebar link `missing-resources`)
  //
  // Scans a target VAR for broken `Pkg:/path` references — created when an
  // earlier dedup rewrote SELF: refs to point at another VAR that has since
  // been deleted (or itself deduped, breaking the chain transitively). The
  // right panel offers local + DB candidate replacements; Run batch-applies
  // the user's picks by rewriting payload text refs and meta.json deps.
  // Module is IIFE-scoped so its helpers don't collide with Overview /
  // Find Duplicates / Reclaim Space symbols.
  // ===========================================================
  (function setupMissingResources() {
    if (!state.missingResources) return;
    function mr() { return state.missingResources; }

    function brokenKey(ref) {
      return `${ref.ref_pkg}|${ref.ref_path ?? ""}`;
    }

    function kindLabel(kind) {
      switch (kind) {
        case "text_ref": return "Text Ref";
        case "meta_dependency": return "Dep";
        case "transitive": return "Transitive";
        default: return String(kind ?? "");
      }
    }

    function kindClass(kind) {
      return `missing-kind-${String(kind ?? "").replace(/_/g, "-")}`;
    }

    function setStatus(msg, isError) {
      mr().status = msg ?? "";
      const el = $("missing-status");
      if (!el) return;
      el.textContent = msg ?? "";
      el.classList.toggle("missing-status-error", !!isError);
    }

    function refMatchesFilter(ref, filterLc) {
      if (!filterLc) return true;
      const pkg = (ref.ref_pkg ?? "").toLowerCase();
      const path = (ref.ref_path ?? "").toLowerCase();
      const kind = kindLabel(ref.kind).toLowerCase();
      return pkg.includes(filterLc) || path.includes(filterLc) || kind.includes(filterLc);
    }

    function renderList() {
      const container = $("missing-list");
      const subtitle = $("missing-list-subtitle");
      if (!container) return;
      const refs = mr().brokenRefs ?? [];
      const filterLc = (mr().filter ?? "").toLowerCase().trim();
      const visible = refs.filter((r) => refMatchesFilter(r, filterLc));
      const picked = Object.keys(mr().replacementMap).length;
      const scanned = !!mr().lastScanCompleted;

      if (subtitle) {
        if (refs.length === 0) {
          if (mr().scanning) {
            subtitle.textContent = "Scanning…";
          } else if (scanned) {
            subtitle.textContent = "Scan complete — no broken references found.";
          } else {
            subtitle.textContent = "Scan a target VAR to populate this list.";
          }
        } else {
          subtitle.textContent = `${refs.length} broken ref${refs.length === 1 ? "" : "s"}` +
            (picked ? ` — ${picked} pending fix${picked === 1 ? "" : "es"}` : "");
        }
      }

      if (visible.length === 0) {
        container.classList.add("empty");
        let emptyHtml;
        if (refs.length > 0) {
          emptyHtml = `<p class="group-empty">No rows match the current filter.</p>`;
        } else if (mr().scanning) {
          emptyHtml = `<p class="group-empty">Scanning…</p>`;
        } else if (scanned) {
          emptyHtml = `
            <div class="missing-empty-clean">
              <span class="material-symbols-outlined missing-empty-clean-icon">check_circle</span>
              <h4 class="missing-empty-clean-title">No broken references found</h4>
              <p class="missing-empty-clean-subtitle">
                Every <code>Pkg:/path</code> reference in this VAR resolves cleanly.
                Nothing to fix.
              </p>
            </div>
          `;
        } else {
          emptyHtml = `<p class="group-empty">No broken references yet. Click <strong>Scan</strong> to analyze the target VAR.</p>`;
        }
        container.innerHTML = emptyHtml;
        renderMissingListPagination(0);
        return;
      }
      container.classList.remove("empty");

      const totalPages = Math.max(1, Math.ceil(visible.length / GROUP_PAGE_SIZE));
      if (mr().listPage >= totalPages) mr().listPage = totalPages - 1;
      if (mr().listPage < 0) mr().listPage = 0;
      const start = mr().listPage * GROUP_PAGE_SIZE;
      const pageVisible = visible.slice(start, start + GROUP_PAGE_SIZE);

      const html = pageVisible.map((ref) => {
        const key = brokenKey(ref);
        const isSelected = key === mr().selectedKey;
        const fix = mr().replacementMap[key];
        const localCount = (ref.local_candidates ?? []).length;
        const pathLabel = ref.ref_path ? escapeHtml(ref.ref_path) : "(dependency only)";
        const fixChip = fix
          ? `<span class="chip chip-accent missing-row-fixed" title="Replacement: ${escapeAttribute(fix.replacement_pkg)}">→ ${escapeHtml(fix.replacement_pkg)}</span>`
          : "";
        return `
          <button type="button" class="group-row missing-row ${isSelected ? "active focused" : ""}" data-missing-key="${escapeAttribute(key)}">
            <div class="group-row-line missing-row-head">
              <span class="chip ${kindClass(ref.kind)}">${escapeHtml(kindLabel(ref.kind))}</span>
              <span class="missing-row-pkg">${escapeHtml(ref.ref_pkg)}</span>
              ${fixChip}
            </div>
            <div class="group-row-line missing-row-path">${pathLabel}</div>
            <div class="group-row-line missing-row-meta">
              <span>${localCount} local candidate${localCount === 1 ? "" : "s"}</span>
            </div>
          </button>
        `;
      }).join("");
      container.innerHTML = html;
      renderMissingListPagination(visible.length);
    }

    function renderMissingListPagination(totalVisible) {
      const pagination = $("missing-list-pagination");
      if (!pagination) return;
      if (totalVisible <= GROUP_PAGE_SIZE) {
        pagination.classList.add("hidden");
        pagination.innerHTML = "";
        return;
      }
      const totalPages = Math.max(1, Math.ceil(totalVisible / GROUP_PAGE_SIZE));
      if (mr().listPage >= totalPages) mr().listPage = totalPages - 1;
      const currentPage = mr().listPage;
      const pages = buildPageNumbers(totalPages, currentPage);

      pagination.classList.remove("hidden");
      pagination.innerHTML = `
        <button class="page-nav-button" data-page-action="prev" type="button" ${currentPage === 0 ? "disabled" : ""}>
          <span class="material-symbols-outlined">chevron_left</span>
          Previous
        </button>
        <div class="page-numbers">
          ${pages
            .map((page) =>
              page === "ellipsis"
                ? `<span class="page-ellipsis">…</span>`
                : `<button class="page-button ${page === currentPage ? "active" : ""}" data-page-jump="${page}" type="button">${page + 1}</button>`
            )
            .join("")}
        </div>
        <button class="page-nav-button" data-page-action="next" type="button" ${currentPage >= totalPages - 1 ? "disabled" : ""}>
          Next
          <span class="material-symbols-outlined">chevron_right</span>
        </button>
      `;

      pagination.querySelectorAll("[data-page-action]").forEach((button) => {
        button.addEventListener("click", () => {
          const action = button.dataset.pageAction;
          if (action === "prev") mr().listPage = Math.max(0, mr().listPage - 1);
          if (action === "next") mr().listPage = Math.min(totalPages - 1, mr().listPage + 1);
          renderList();
        });
      });

      pagination.querySelectorAll("[data-page-jump]").forEach((button) => {
        button.addEventListener("click", () => {
          mr().listPage = Number(button.dataset.pageJump) || 0;
          renderList();
        });
      });
    }

    function findRefByKey(key) {
      return (mr().brokenRefs ?? []).find((r) => brokenKey(r) === key);
    }

    function renderDetail() {
      const empty = $("missing-detail-empty");
      const panel = $("missing-detail-panel");
      const subtitle = $("missing-detail-subtitle");
      const key = mr().selectedKey;
      const ref = key ? findRefByKey(key) : null;
      if (!ref || !panel || !empty) {
        if (panel) panel.classList.add("hidden");
        if (empty) empty.classList.remove("hidden");
        if (subtitle) subtitle.textContent = "Select a missing resource to view candidates.";
        return;
      }
      empty.classList.add("hidden");
      panel.classList.remove("hidden");

      const refLabel = ref.ref_path
        ? `${ref.ref_pkg}:/${ref.ref_path}`
        : `${ref.ref_pkg} (declared dependency)`;
      const refEl = $("missing-detail-ref");
      if (refEl) refEl.textContent = refLabel;
      const kindEl = $("missing-detail-kind");
      if (kindEl) {
        kindEl.textContent = kindLabel(ref.kind);
        kindEl.className = `chip ${kindClass(ref.kind)}`;
      }
      const crcEl = $("missing-detail-crc");
      if (crcEl) {
        if (typeof ref.expected_crc32 === "number") {
          crcEl.textContent = `CRC32 ${ref.expected_crc32.toString(16).padStart(8, "0").toUpperCase()}`;
          crcEl.classList.remove("hidden");
        } else {
          crcEl.textContent = "CRC32 unknown";
          crcEl.classList.remove("hidden");
        }
      }
      if (subtitle) subtitle.textContent = `Pick a replacement source. Local candidates resolve immediately; database candidates load on demand.`;

      const filesUl = $("missing-source-files");
      if (filesUl) {
        const files = ref.source_files_in_target ?? [];
        if (files.length === 0) {
          filesUl.innerHTML = `<li class="missing-source-empty">No payload references (meta.json only).</li>`;
        } else {
          filesUl.innerHTML = files.map((f) => `<li><code>${escapeHtml(f)}</code></li>`).join("");
        }
      }

      renderLocalCandidates(ref);
      // Database candidates only render in "db" mode. Hide the whole section
      // (and skip the on-demand DB fetch) in "local" mode. Detection of the
      // missing ref above is independent of mode, so a ref with no candidate
      // in either source still shows in the list and detail header.
      const dbSection = $("missing-db-section");
      const dbMode = mr().mode === "db";
      if (dbSection) dbSection.classList.toggle("hidden", !dbMode);
      if (dbMode) {
        renderDbCandidates(ref);
      }
    }

    function buildCandidateRow(ref, opts) {
      const key = mr().selectedKey;
      const picked = mr().replacementMap[key];
      const pickedPath = picked?.replacement_path ?? null;
      const checked =
        picked &&
        picked.replacement_pkg === opts.replacement_pkg &&
        picked.source === opts.source &&
        (pickedPath == null || pickedPath === (opts.path ?? null));
      const sizeStr = opts.size != null ? formatBytesLocal(opts.size) : "";
      const crcStr = (typeof opts.crc32 === "number")
        ? `CRC32 ${opts.crc32.toString(16).padStart(8, "0").toUpperCase()}`
        : "";
      const tagHtml = opts.tag
        ? `<span class="chip ${opts.tagClass ?? ""}">${escapeHtml(opts.tag)}</span>`
        : "";
      const fileHtml = opts.file
        ? `<div class="missing-candidate-file">${escapeHtml(opts.file)}</div>`
        : "";
      const fileAttr = opts.file ? ` data-package-file="${escapeAttribute(opts.file)}"` : "";
      const pathAttr = opts.path != null ? ` data-internal-path="${escapeAttribute(opts.path)}"` : "";
      const pkgAttr = escapeAttribute(opts.replacement_pkg);
      return `
        <label class="keep-option missing-candidate ${checked ? "is-selected" : ""}"${fileAttr}>
          <input type="radio" name="missing-candidate" value="${pkgAttr}"
                 data-source="${escapeAttribute(opts.source)}"${pathAttr} ${checked ? "checked" : ""}>
          <div class="missing-candidate-body">
            <div class="missing-candidate-head">
              <strong>${escapeHtml(opts.replacement_pkg)}</strong>
              ${tagHtml}
            </div>
            <div class="missing-candidate-path">${escapeHtml(opts.path)}</div>
            ${fileHtml}
            <div class="missing-candidate-meta">
              ${sizeStr ? `<span>${sizeStr}</span>` : ""}
              ${crcStr ? `<span>${crcStr}</span>` : ""}
            </div>
            <div class="missing-candidate-actions">
              <button type="button" class="missing-candidate-action" data-action="open-var-details"
                      data-package-id="${pkgAttr}" title="Open this package in VAR Details">
                <span class="material-symbols-outlined">description</span>
                <span>VAR Details</span>
              </button>
              <button type="button" class="missing-candidate-action" data-action="copy-pkg-id"
                      data-package-id="${pkgAttr}" title="Copy the package id to clipboard">
                <span class="material-symbols-outlined">content_copy</span>
                <span>Copy id</span>
              </button>
            </div>
          </div>
        </label>
      `;
    }

    function renderLocalCandidates(ref) {
      const container = $("missing-local-candidates");
      if (!container) return;
      const candidates = (ref.local_candidates ?? []).filter(
        (c) => !isCreatorBlockedForPackage(c.package_id)
      );

      if (!ref.ref_path) {
        // MetaDependency: no path-level candidates. Offer a free-text package
        // id input so the user can manually pick the replacement dep.
        const current = mr().replacementMap[mr().selectedKey];
        container.innerHTML = `
          <div class="missing-meta-only">
            <label>
              Replacement package
              <input type="text" id="missing-meta-replacement" placeholder="Creator.Pkg.Version"
                     value="${escapeAttribute(current?.replacement_pkg ?? "")}" />
            </label>
            <p class="missing-meta-hint">
              The broken dependency has no payload references. Provide a package id to swap in
              meta.json on Run.
            </p>
          </div>
        `;
        const input = $("missing-meta-replacement");
        if (input) {
          input.addEventListener("input", () => {
            const key = mr().selectedKey;
            if (!key) return;
            const value = input.value.trim();
            if (!value) {
              delete mr().replacementMap[key];
            } else {
              mr().replacementMap[key] = {
                broken_pkg: ref.ref_pkg,
                broken_path: null,
                replacement_pkg: value,
                source: "manual",
              };
            }
            updateRunButton();
            renderList();
          });
        }
        return;
      }

      if (candidates.length === 0) {
        container.innerHTML = `<p class="keep-option-empty">No local candidates match this resource.</p>`;
        return;
      }

      container.innerHTML = candidates.map((c) => buildCandidateRow(ref, {
        replacement_pkg: c.package_id,
        path: c.internal_path,
        file: c.package_file,
        size: c.size,
        crc32: c.crc32,
        source: "local",
        tag: typeof c.crc32 === "number" && typeof ref.expected_crc32 === "number" && c.crc32 === ref.expected_crc32
          ? "CRC match"
          : "Path match",
        tagClass: "chip-accent",
      })).join("");
    }

    // DB candidates: server-paged, searchable, infinite-scroll.
    const DB_PAGE_SIZE = 50;

    function skeletonRowsHtml(n) {
      const ROW = `
        <div class="missing-candidate-skeleton">
          <div class="skel-lines">
            <div class="skel-bar wide"></div>
            <div class="skel-bar full"></div>
            <div class="skel-bar short"></div>
          </div>
        </div>`;
      return ROW.repeat(n);
    }

    function ensureDbState(key) {
      let entry = mr().dbCandidatesByKey[key];
      if (!entry) {
        entry = {
          items: [],
          hasMore: false,
          loading: false,
          error: null,
          search: "",
          offset: 0,
          initialized: false,
        };
        mr().dbCandidatesByKey[key] = entry;
      }
      return entry;
    }

    // Detects malformed nested refs of the form `<Creator.Pkg.Ver>:/<inner>`
    // embedded inside ref.ref_path. Pkg requires ≥2 dots to match
    // collect_pkg_refs's `.matches('.').count() >= 2` guard in fix_var.rs and
    // its is_pkg_id_byte character set (no whitespace, `"`, `'`, `,`, `:`,
    // `/`). False positives are extremely unlikely — legitimate VAR internal
    // paths don't embed `Creator.Pkg.Ver:/`.
    function detectNestedRef(refPath) {
      if (typeof refPath !== "string") return null;
      const m = refPath.match(/^([^\s"',/:][^\s"',/:]*(?:\.[^\s"',/:]+){2,}):\/(.+)$/);
      if (!m) return null;
      const innerPath = m[2].trim();
      if (!innerPath) return null;
      return { innerPkg: m[1], innerPath };
    }

    function ensureNestedDbState(key) {
      let entry = mr().nestedDbCandidatesByKey[key];
      if (!entry) {
        entry = { status: "idle", items: [], error: null, innerPath: "" };
        mr().nestedDbCandidatesByKey[key] = entry;
      }
      return entry;
    }

    // Look up the inner path via the same Tauri command the regular DB
    // section uses; passing crc32: null falls through to the path-only
    // branch (SELECT ... WHERE r.internal_path = ?1). Limit 50 mirrors the
    // first DB page so the section stays bounded without pagination.
    async function fetchNestedDbCandidates(ref, innerPath) {
      if (!invoke) return;
      const key = brokenKey(ref);
      const entry = ensureNestedDbState(key);
      if (entry.status === "loading" || entry.status === "ok") return;
      entry.status = "loading";
      entry.innerPath = innerPath;
      entry.error = null;
      if (mr().selectedKey === key) renderDetail();
      try {
        const page = await invoke("find_db_candidates_for_broken_ref", {
          crc32: null,
          internalPath: innerPath,
          excludePkg: null,
          search: null,
          offset: 0,
          limit: 50,
        });
        entry.items = Array.isArray(page?.items) ? page.items : [];
        entry.status = "ok";
      } catch (err) {
        entry.error = String(err);
        entry.status = "err";
      }
      if (mr().selectedKey === key) renderDetail();
    }

    let lastDbRenderKey = null;

    function renderDbCandidates(ref) {
      const container = $("missing-db-candidates");
      const searchEl = $("missing-db-search");
      const scrollEl = $("missing-db-scroll");
      if (!container) return;
      const key = brokenKey(ref);

      // Reset scroll position when the user navigates to a different ref so
      // they don't land mid-list from the previous selection.
      if (scrollEl && key !== lastDbRenderKey) {
        scrollEl.scrollTop = 0;
      }
      lastDbRenderKey = key;

      if (!ref.ref_path) {
        container.innerHTML = `<p class="keep-option-empty">No DB lookup for meta-only dependencies.</p>`;
        if (searchEl) {
          searchEl.value = "";
          searchEl.disabled = true;
        }
        return;
      }
      if (searchEl) searchEl.disabled = false;

      const entry = ensureDbState(key);
      // Keep the visible search box in sync with the per-ref entry — when
      // the user navigates between broken refs the box should reflect that
      // ref's filter, not the previous one's.
      if (searchEl && searchEl.value !== entry.search) {
        searchEl.value = entry.search;
      }

      // Kick off the first page on first render (or when search changed and
      // page was reset to 0 with no items).
      if (!entry.initialized && !entry.loading) {
        fetchDbCandidatesPage(ref).catch(() => {});
      }

      const rows = (entry.items ?? [])
        .filter((r) => !isCreatorBlockedForPackage(r.package_id))
        .map((r) => buildCandidateRow(ref, {
          replacement_pkg: r.package_id,
          path: r.internal_path,
          file: r.package_file,
          size: r.size,
          crc32: ref.expected_crc32,
          source: "db",
          tag: "Database",
        })).join("");

      let footer = "";
      if (entry.error) {
        footer = `<p class="keep-option-empty">DB lookup failed: ${escapeHtml(entry.error ?? "")}</p>`;
      } else if (entry.loading) {
        // Skeleton count: a few for initial load, fewer for subsequent pages.
        const n = entry.items.length === 0 ? 5 : 3;
        footer = skeletonRowsHtml(n);
      } else if (entry.items.length === 0) {
        footer = entry.search
          ? `<p class="keep-option-empty">No DB candidates match the filter.</p>`
          : `<p class="keep-option-empty">No database candidates found.</p>`;
      }

      // Nested-ref recovery: when the broken_path embeds a second `Pkg:/...`
      // prefix (a malformed scene-text ref), the regular DB lookup above
      // returns nothing because the path is invalid. Detect the inner clean
      // path and surface candidates that match it. Picking one wires the
      // chosen package + inner path into the FixDirective; the apply-fix
      // rewrite literal-replaces the full malformed substring (built from
      // the unchanged broken_pkg + broken_path) with the clean new ref.
      const nested = detectNestedRef(ref.ref_path);
      let nestedHtml = "";
      if (nested) {
        const nestedEntry = ensureNestedDbState(key);
        if (nestedEntry.status === "idle") {
          fetchNestedDbCandidates(ref, nested.innerPath).catch(() => {});
        }
        const bannerHtml = `
          <div class="missing-nested-banner" role="note">
            <span class="material-symbols-outlined missing-nested-icon" aria-hidden="true">warning</span>
            <div class="missing-nested-text">
              <strong>Nested ref detected</strong>
              <div>This ref has two package prefixes — likely an editor mistake. Searching for candidates that contain the inner path:</div>
              <code class="missing-nested-inner">${escapeHtml(nested.innerPath)}</code>
            </div>
          </div>
        `;
        let nestedBody = "";
        if (nestedEntry.status === "loading") {
          nestedBody = skeletonRowsHtml(3);
        } else if (nestedEntry.status === "err") {
          nestedBody = `<p class="keep-option-empty">Nested-ref lookup failed: ${escapeHtml(nestedEntry.error ?? "")}</p>`;
        } else if (nestedEntry.status === "ok") {
          const filtered = (nestedEntry.items ?? [])
            .filter((r) => !isCreatorBlockedForPackage(r.package_id));
          if (filtered.length === 0) {
            nestedBody = `<p class="keep-option-empty">No packages found that contain this inner path.</p>`;
          } else {
            // Order: if the malformed text's inner pkg actually has a hit,
            // surface it first — it's the most likely original intent.
            const innerPkg = nested.innerPkg;
            filtered.sort((a, b) => {
              const aHit = a.package_id === innerPkg ? 0 : 1;
              const bHit = b.package_id === innerPkg ? 0 : 1;
              return aHit - bHit;
            });
            nestedBody = filtered.map((r) => buildCandidateRow(ref, {
              replacement_pkg: r.package_id,
              path: r.internal_path,
              file: r.package_file,
              size: r.size,
              crc32: ref.expected_crc32,
              source: "db",
              tag: r.package_id === innerPkg ? "Nested · likely" : "Nested ref",
              tagClass: r.package_id === innerPkg ? "chip-accent" : "",
            })).join("");
          }
        }
        nestedHtml = bannerHtml + nestedBody;
      }

      container.innerHTML = rows + footer + nestedHtml;
    }

    async function fetchDbCandidatesPage(ref) {
      if (!invoke) return;
      const key = brokenKey(ref);
      const entry = ensureDbState(key);
      if (entry.loading) return;
      if (entry.initialized && !entry.hasMore) return;

      entry.loading = true;
      entry.error = null;
      if (mr().selectedKey === key) renderDetail();

      try {
        const page = await invoke("find_db_candidates_for_broken_ref", {
          crc32: typeof ref.expected_crc32 === "number" ? ref.expected_crc32 : null,
          internalPath: ref.ref_path ?? "",
          excludePkg: ref.ref_pkg,
          search: entry.search || null,
          offset: entry.offset,
          limit: DB_PAGE_SIZE,
        });
        const items = Array.isArray(page?.items) ? page.items : [];
        entry.items = entry.items.concat(items);
        entry.hasMore = !!page?.has_more;
        entry.offset = entry.items.length;
        entry.initialized = true;
      } catch (err) {
        entry.error = String(err);
        entry.hasMore = false;
      } finally {
        entry.loading = false;
      }

      if (mr().selectedKey === key) renderDetail();
    }

    function resetAndRefetchDb(ref, newSearch) {
      if (!ref) return;
      const key = brokenKey(ref);
      const entry = ensureDbState(key);
      entry.items = [];
      entry.offset = 0;
      entry.hasMore = false;
      entry.initialized = false;
      entry.error = null;
      entry.search = newSearch ?? "";
      // Render immediately to show skeleton, then kick off the fetch.
      if (mr().selectedKey === key) renderDetail();
      fetchDbCandidatesPage(ref).catch(() => {});
    }

    function updateRunButton() {
      const run = $("missing-run-button");
      const clear = $("missing-clear-button");
      const applyScope = $("missing-apply-scope-button");
      const applyFiltered = $("missing-apply-filtered-button");
      const applyGlobal = $("missing-apply-global-button");
      const picked = Object.keys(mr().replacementMap).length;
      if (run) run.disabled = picked === 0 || mr().applying;
      if (clear) clear.disabled = picked === 0;

      // Quick-apply buttons need a currently-selected ref with a chosen
      // replacement package — that's the "what to apply" source. Disabled
      // when there's no selection to copy from. Mirrors the Find Duplicates
      // page's scope / filtered / global trio in
      // `renderDetail` → keep-actions wiring.
      const selectedKey = mr().selectedKey;
      const selectedFix = selectedKey ? mr().replacementMap[selectedKey] : null;
      const hasPick = !!selectedFix?.replacement_pkg;
      if (applyScope) applyScope.disabled = !hasPick || mr().applying;
      if (applyFiltered) applyFiltered.disabled = !hasPick || mr().applying;
      if (applyGlobal) applyGlobal.disabled = !hasPick || mr().applying;
    }

    // openCandidatePackageInVarDetails is hoisted to file scope so the
    // Internalize Resources page (sibling IIFE) can call it too. See the
    // definition near openVarDetailsView / loadVarDetailsFromPath.

    // Looks up the best candidate ResourceRef inside `ref.local_candidates`
    // (and, if loaded, the per-ref DB-candidate cache) whose package_id
    // matches `packageId`. Returns `null` when neither source has a
    // candidate from that package — the caller (`applyReplacementToRefs`)
    // skips refs without a match so we never wire up a fix that points at
    // a package which doesn't actually carry a usable resource.
    //
    // The DB-candidate side is intentionally best-effort: results are only
    // available for refs the user has previously clicked into (their
    // per-ref `dbCandidatesByKey` entry is populated lazily). For refs that
    // were never inspected, only `local_candidates` is consulted — which
    // is fine for the common case where the replacement package is local
    // to the input folder.
    function findRefCandidateForPackage(ref, packageId) {
      if (!ref || !packageId) return null;
      for (const c of ref.local_candidates ?? []) {
        if (c.package_id === packageId) {
          return { internal_path: c.internal_path, source: "local" };
        }
      }
      const dbEntry = mr().dbCandidatesByKey[brokenKey(ref)];
      for (const c of dbEntry?.items ?? []) {
        if (c.package_id === packageId) {
          return { internal_path: c.internal_path, source: "db" };
        }
      }
      return null;
    }

    // Applies the selected ref's replacement package to every ref in
    // `targetRefs` that has a matching candidate, returning the number of
    // refs actually updated. Refs without a matching candidate are skipped
    // (no mutation) rather than getting an invalid fix attached.
    function applyReplacementToRefs(targetRefs, replacementPkg, fallbackLicenseType) {
      let applied = 0;
      for (const ref of targetRefs) {
        if (!ref.ref_path) continue; // MetaDependency-style refs aren't matched by candidate scan.
        const candidate = findRefCandidateForPackage(ref, replacementPkg);
        if (!candidate) continue;
        const key = brokenKey(ref);
        mr().replacementMap[key] = {
          broken_pkg: ref.ref_pkg,
          broken_path: ref.ref_path ?? null,
          replacement_pkg: replacementPkg,
          replacement_license_type: fallbackLicenseType ?? null,
          replacement_path: candidate.internal_path ?? null,
          source: candidate.source,
        };
        applied++;
      }
      return applied;
    }

    // Right-click menu for a broken-ref row. Mirrors the Overview pattern:
    // copy actions first, then a clear-replacement entry if one's selected.
    function brokenRefMenuItems(ref) {
      if (!ref) return [];
      const items = [];
      const pkgPath = ref.ref_path
        ? `${ref.ref_pkg}:/${ref.ref_path}`
        : ref.ref_pkg;
      items.push({
        label: `Copy ${pkgPath}`,
        action: async () => {
          const ok = await copyTextToClipboard(pkgPath);
          if (ok) addLog(`Copied ${pkgPath}`);
        },
      });
      items.push({
        label: `Copy package id (${ref.ref_pkg})`,
        action: async () => {
          const ok = await copyTextToClipboard(ref.ref_pkg);
          if (ok) addLog(`Copied ${ref.ref_pkg}`);
        },
      });
      if (ref.ref_path) {
        items.push({
          label: `Copy path (${ref.ref_path})`,
          action: async () => {
            const ok = await copyTextToClipboard(ref.ref_path);
            if (ok) addLog(`Copied ${ref.ref_path}`);
          },
        });
      }
      if (Number.isFinite(ref.expected_crc32) && ref.expected_crc32 != null) {
        const hex = formatCrc32Hex(ref.expected_crc32);
        items.push({ separator: true });
        items.push({
          label: `Search Resource List by CRC ${hex}`,
          action: () => searchResourceListByCrc(ref.expected_crc32),
        });
        items.push({
          label: `Copy CRC ${hex}`,
          action: async () => {
            const ok = await copyTextToClipboard(hex);
            if (ok) addLog(`Copied ${hex}`);
          },
        });
      }
      const key = brokenKey(ref);
      if (mr().replacementMap[key]) {
        items.push({ separator: true });
        items.push({
          label: "Clear replacement",
          action: () => {
            delete mr().replacementMap[key];
            updateRunButton();
            renderList();
            renderDetail();
          },
        });
      }
      return items;
    }

    // Reuse the Overview's `complete-backdrop` modal to mirror the post-Run
    // popup. The open button is redirected via dataset.path so it shows the
    // missing-resources output instead of the dedupe output folder.
    function showFixComplete(report, ctx) {
      const outputPath = report?.output_path ?? "";
      const fixesApplied = report?.fixes_applied ?? 0;
      const filesRewritten = report?.files_rewritten ?? 0;
      const added = report?.dependencies_added?.length ?? 0;
      const removed = report?.dependencies_removed?.length ?? 0;
      const replaceInPlace = !!ctx?.replaceInPlace;
      const backupMade = !!ctx?.backup && replaceInPlace;

      const titleEl = $("complete-title");
      const messageEl = $("complete-message");
      const sizesEl = $("complete-sizes");
      const openBtn = $("complete-open");
      const okBtn = $("complete-ok");
      const reportBtn = $("complete-report");
      const backupBtn = $("complete-open-backup");

      if (titleEl) titleEl.textContent = "Fixes applied";
      if (messageEl) {
        messageEl.textContent =
          `Applied ${fixesApplied} fix${fixesApplied === 1 ? "" : "es"} — ` +
          `${filesRewritten} file${filesRewritten === 1 ? "" : "s"} rewritten, ` +
          `${added} dep${added === 1 ? "" : "s"} added, ` +
          `${removed} dep${removed === 1 ? "" : "s"} removed.`;
      }
      if (sizesEl) sizesEl.textContent = outputPath ? `Output: ${outputPath}` : "";

      // Hide the dedupe report button — Missing Resources doesn't write one.
      if (reportBtn) {
        reportBtn.textContent = "";
        reportBtn.dataset.path = "";
        reportBtn.classList.add("hidden");
      }

      if (openBtn) {
        openBtn.textContent = outputPath ? "Open output" : "Close";
        openBtn.dataset.path = outputPath;
      }
      if (okBtn) okBtn.textContent = "Close";

      if (backupBtn) {
        if (backupMade) {
          // Mirror the Rust path resolution: `<output>/backup` when an
          // output folder was set, else fall back to `<target_parent>/fix-var-backup`.
          const backupDir = (() => {
            const outDir = (mr().outputDir ?? "").trim().replace(/[\\/]+$/, "");
            if (outDir) return `${outDir}\\backup`;
            const target = mr().targetVar.trim();
            const idx = Math.max(target.lastIndexOf("\\"), target.lastIndexOf("/"));
            const parent = idx >= 0 ? target.slice(0, idx) : "";
            return parent ? `${parent}\\fix-var-backup` : "";
          })();
          backupBtn.textContent = "Open backup";
          backupBtn.dataset.path = backupDir;
          backupBtn.classList.toggle("hidden", !backupDir);
        } else {
          backupBtn.textContent = "";
          backupBtn.dataset.path = "";
          backupBtn.classList.add("hidden");
        }
      }

      $("complete-backdrop").classList.remove("hidden");
    }

    function sleep(ms) {
      return new Promise((resolve) => setTimeout(resolve, ms));
    }

    function showProgress(title, fraction, message) {
      const card = $("missing-progress-card");
      const pct = $("missing-progress-percent");
      const bar = $("missing-progress-bar");
      const msg = $("missing-progress-message");
      const titleEl = $("missing-progress-title");
      if (!card) return;
      card.classList.remove("hidden");
      const clamped = Math.max(0, Math.min(1, Number(fraction) || 0));
      if (pct) pct.textContent = `${Math.round(clamped * 100)}%`;
      if (bar) bar.style.width = `${clamped * 100}%`;
      if (msg) msg.textContent = message ?? "";
      if (titleEl && title) titleEl.textContent = title;
    }

    function hideProgress() {
      const card = $("missing-progress-card");
      if (card) card.classList.add("hidden");
      const bar = $("missing-progress-bar");
      if (bar) bar.style.width = "0%";
      const pct = $("missing-progress-percent");
      if (pct) pct.textContent = "0%";
    }

    // Drives a scan_task task to completion, polling progress with the same
    // cadence as Overview. Returns once done=true; throws on task error so
    // the caller can surface the message.
    async function runScanTask(inputDir, targetVar, opts = {}) {
      const { base = 0, span = 1, title = "Scanning" } = opts;
      const handle = await invoke("start_scan_task", {
        request: { input_dir: inputDir, target_var_path: targetVar, skip_db: true },
      });
      const taskId = handle?.id;
      if (taskId == null) throw new Error("scan task did not return a handle");
      // Best-effort cleanup so the task table doesn't leak rows even on
      // failures — silently ignore errors here.
      const cleanup = () => invoke("clear_task", { taskId }).catch(() => {});
      try {
        while (true) {
          const payload = await invoke("get_task_progress", { taskId });
          if (payload) {
            const fraction = Number(payload.progress ?? 0);
            const msg = payload.message ?? "";
            showProgress(title, base + fraction * span, msg);
            if (payload.done) {
              if (payload.error) throw new Error(String(payload.error));
              return payload;
            }
          }
          await sleep(TASK_POLL_MS);
        }
      } finally {
        cleanup();
      }
    }

    // Background broken-refs analysis with the same polling shape as
    // runScanTask. The Rust side spawns a worker thread for
    // scan_target_var_for_broken_refs so the IPC dispatcher (and therefore
    // the WebView event loop) isn't held hostage by a long-running walk —
    // the user sees the progress bar tick and the UI stays interactive.
    async function runMissingScanTask(inputDir, targetVar, opts = {}) {
      const { base = 0, span = 1, title = "Analyzing" } = opts;
      const handle = await invoke("start_scan_missing_resources_task", {
        inputDir,
        targetVarPath: targetVar,
      });
      const taskId = handle?.id;
      if (taskId == null) throw new Error("missing-scan task did not return a handle");
      const cleanup = () => invoke("clear_task", { taskId }).catch(() => {});
      try {
        while (true) {
          const payload = await invoke("get_task_progress", { taskId });
          if (payload) {
            const fraction = Number(payload.progress ?? 0);
            const msg = payload.message ?? "";
            showProgress(title, base + fraction * span, msg);
            if (payload.done) {
              if (payload.error) throw new Error(String(payload.error));
              return Array.isArray(payload.missing_resources_result)
                ? payload.missing_resources_result
                : [];
            }
          }
          await sleep(TASK_POLL_MS);
        }
      } finally {
        cleanup();
      }
    }

    async function scanForBrokenRefs() {
      if (!invoke) return;
      const inputDir = mr().inputDir.trim();
      const targetVar = mr().targetVar.trim();
      if (!inputDir || !targetVar) {
        setStatus("Pick both a VAR folder and a target .var file.", true);
        return;
      }
      mr().scanning = true;
      mr().brokenRefs = [];
      mr().replacementMap = {};
      mr().dbCandidatesByKey = {};
      mr().selectedKey = null;
      mr().listPage = 0;
      // Clear the "scanned" flag while a new scan is running so the list
      // empty-state shows "Scanning…" instead of "no broken references".
      mr().lastScanCompleted = false;
      setStatus("", false);
      showProgress("Scanning", 0, "Starting scan…");
      renderList();
      renderDetail();
      updateRunButton();
      const scanBtn = $("missing-scan-button");
      if (scanBtn) scanBtn.disabled = true;
      try {
        // The two phases share one continuous progress bar: the folder scan
        // (the bulk of the wall-clock time) fills 0→85%, the broken-ref
        // analysis fills 85→100%. Each phase reports its own 0..1 fraction;
        // the {base, span} range maps it onto its slice so the bar only ever
        // moves forward (previously phase 2 reset to 0 and jumped backward).
        //
        // 1. Run the local-folder scan (populates the shared scan cache used
        //    by scan_missing_resources). Mirrors Overview's scan invocation
        //    so the Missing Resources page is fully self-contained.
        await runScanTask(inputDir, targetVar, { base: 0, span: 0.85 });
        // 2. Analyze the target VAR via the background-task variant. The
        //    old one-shot `scan_missing_resources` invoke blocked the IPC
        //    dispatcher for the entire duration of the walk; on large VARs
        //    that froze the UI for tens of seconds. The task variant runs
        //    on a worker thread and we poll progress so the event loop
        //    stays free to handle clicks / re-renders.
        const brokenRefs = await runMissingScanTask(inputDir, targetVar, {
          base: 0.85,
          span: 0.15,
        });
        mr().brokenRefs = Array.isArray(brokenRefs) ? brokenRefs : [];
        mr().lastScanCompleted = true;
        const count = mr().brokenRefs.length;
        const summary = count === 0
          ? "Scan complete — no broken references found."
          : `Found ${count} broken reference${count === 1 ? "" : "s"}.`;
        showProgress("Done", 1, summary);
        setStatus(summary, false);
        // Tuck the progress card away after a brief beat so the user sees
        // the 100% / final message but the page isn't permanently crowded.
        setTimeout(hideProgress, 1500);
      } catch (err) {
        hideProgress();
        setStatus(String(err && err.message ? err.message : err), true);
        mr().brokenRefs = [];
        // Scan errored — don't mark scan complete; user needs to retry.
        mr().lastScanCompleted = false;
      } finally {
        mr().scanning = false;
        if (scanBtn) scanBtn.disabled = false;
        renderList();
        renderDetail();
      }
    }

    async function applyFixes() {
      if (!invoke) return;
      const fixes = Object.values(mr().replacementMap)
        .map((entry) => ({
          broken_pkg: entry.broken_pkg,
          broken_path: entry.broken_path ?? null,
          replacement_pkg: entry.replacement_pkg,
          replacement_license_type: entry.replacement_license_type ?? null,
          replacement_path: entry.replacement_path ?? null,
        }));
      if (fixes.length === 0) return;
      const replaceInPlace = !!mr().replaceInPlace;
      const outputDir = mr().outputDir.trim();
      if (!replaceInPlace && !outputDir) {
        setStatus("Set an Output Folder or enable 'Replace in place' before running fixes.", true);
        return;
      }
      mr().applying = true;
      updateRunButton();
      setStatus(`Applying ${fixes.length} fix${fixes.length === 1 ? "" : "es"}…`, false);
      showProgress("Applying", 0, `Rewriting target VAR with ${fixes.length} fix${fixes.length === 1 ? "" : "es"}…`);
      // The apply call runs off a worker thread on the Rust side and emits
      // only coarse phase updates (loading → rewriting → done), so the bar
      // sits on indeterminate while we poll — the UI never blocks.
      const progressBar = $("missing-progress-bar");
      const progressPct = $("missing-progress-percent");
      if (progressBar) progressBar.classList.add("is-indeterminate");
      if (progressPct) progressPct.textContent = "";
      let taskId = null;
      try {
        const handle = await invoke("start_apply_missing_resources_fix_task", {
          inputDir: mr().inputDir.trim(),
          targetVarPath: mr().targetVar.trim(),
          // Always forward the output folder — Rust uses it for `changed/`
          // in output-folder mode AND for `backup/` in replace-in-place mode.
          outputDir: outputDir || null,
          replaceInPlace,
          fixes,
          backup: !!mr().backup,
        });
        taskId = handle?.id;
        if (taskId == null) throw new Error("apply-fix task did not return a handle");
        let report = null;
        while (true) {
          const payload = await invoke("get_task_progress", { taskId });
          if (payload) {
            const msg = payload.message ?? "";
            if (msg) setStatus(msg, false);
            if (payload.done) {
              if (payload.error) throw new Error(String(payload.error));
              report = payload.fix_report ?? null;
              break;
            }
          }
          await sleep(TASK_POLL_MS);
        }
        if (progressBar) progressBar.classList.remove("is-indeterminate");
        showProgress("Applying", 0.85, "Re-analyzing target VAR…");
        const added = report?.dependencies_added?.length ?? 0;
        const removed = report?.dependencies_removed?.length ?? 0;
        const rewritten = report?.files_rewritten ?? 0;
        const wrotePath = report?.output_path ?? "";
        setStatus(
          `Applied ${report?.fixes_applied ?? fixes.length} fix(es). ` +
          `${rewritten} file(s) rewritten, ${added} dep(s) added, ${removed} dep(s) removed.` +
          (wrotePath ? ` → ${wrotePath}` : ""),
          false,
        );
        mr().replacementMap = {};
        mr().dbCandidatesByKey = {};
        mr().selectedKey = null;
        mr().listPage = 0;
        // Re-analyze only (no need to re-walk the whole folder — the cache
        // is still valid for every other package; the target .var is read
        // fresh from disk by the missing-scan task). Reuses the same
        // background-task path as the initial scan so the UI doesn't freeze.
        try {
          // The bar is already at 85% ("Applying"); let the re-analysis fill
          // the remaining 85→100% so it doesn't reset backward.
          const brokenRefs = await runMissingScanTask(
            mr().inputDir.trim(),
            mr().targetVar.trim(),
            { base: 0.85, span: 0.15 },
          );
          mr().brokenRefs = Array.isArray(brokenRefs) ? brokenRefs : [];
        } catch (err) {
          // If the scan cache was invalidated (rare — typically when the
          // user changed inputs between Scan and Run), just clear and let
          // the user re-scan.
          mr().brokenRefs = [];
        }
        showProgress("Done", 1, `Applied ${report?.fixes_applied ?? fixes.length} fix(es).`);
        setTimeout(hideProgress, 1500);
        renderList();
        renderDetail();
        showFixComplete(report, { replaceInPlace, backup: !!mr().backup });
      } catch (err) {
        if (progressBar) progressBar.classList.remove("is-indeterminate");
        hideProgress();
        setStatus(`Apply failed: ${String(err)}`, true);
      } finally {
        if (progressBar) progressBar.classList.remove("is-indeterminate");
        // Best-effort cleanup so the task table doesn't leak rows. The
        // backend stores the completed payload until cleared.
        if (taskId != null) {
          invoke("clear_task", { taskId }).catch(() => {});
        }
        mr().applying = false;
        updateRunButton();
      }
    }

    function bindEvents() {
      const targetInput = $("missing-target-var");
      if (targetInput) {
        targetInput.addEventListener("input", () => {
          mr().targetVar = targetInput.value;
          renderVarInfo();
        });
      }

      // Match VAR Details dropzone behavior: clicking the card opens the
      // picker; HTML5 drag events just paint the hover state since the
      // webview can't read OS file paths reliably — the path arrives via
      // the Tauri window event registered below.
      const dropzone = $("missing-target-dropzone");
      if (dropzone) {
        dropzone.addEventListener("click", (e) => {
          // Let inner buttons handle their own click.
          if (e.target.closest("button")) return;
          $("missing-pick-target")?.click();
        });
        ["dragenter", "dragover"].forEach((evt) => {
          dropzone.addEventListener(evt, (e) => {
            e.preventDefault();
            dropzone.classList.add("is-dragover");
          });
        });
        ["dragleave", "drop"].forEach((evt) => {
          dropzone.addEventListener(evt, (e) => {
            e.preventDefault();
            dropzone.classList.remove("is-dragover");
          });
        });
      }

      const dirInput = $("missing-input-dir");
      if (dirInput) {
        dirInput.addEventListener("input", () => {
          mr().inputDir = dirInput.value;
          // Default output folder once the user types an input folder, but
          // only if they haven't manually set one yet.
          const outputInput = $("missing-output-dir");
          if (outputInput && !mr().outputDir && dirInput.value.trim()) {
            mr().outputDir = `${dirInput.value.trim()}_fixed`;
            outputInput.value = mr().outputDir;
          }
        });
      }
      const outputInput = $("missing-output-dir");
      if (outputInput) {
        outputInput.addEventListener("input", () => {
          mr().outputDir = outputInput.value;
        });
      }
      const pickTarget = $("missing-pick-target");
      if (pickTarget) {
        pickTarget.addEventListener("click", async () => {
          if (!invoke) return;
          try {
            const selected = await invoke("pick_var_file");
            if (selected) {
              mr().targetVar = String(selected);
              if (targetInput) targetInput.value = mr().targetVar;
              renderVarInfo();
            }
          } catch (err) {
            setStatus(`Pick failed: ${String(err)}`, true);
          }
        });
      }
      const pickDir = $("missing-pick-input");
      if (pickDir) {
        pickDir.addEventListener("click", async () => {
          if (!invoke) return;
          try {
            const selected = await invoke("pick_folder");
            if (selected) {
              mr().inputDir = String(selected);
              if (dirInput) dirInput.value = mr().inputDir;
              if (!mr().outputDir) {
                mr().outputDir = `${mr().inputDir}_fixed`;
                const outEl = $("missing-output-dir");
                if (outEl) outEl.value = mr().outputDir;
              }
            }
          } catch (err) {
            setStatus(`Pick failed: ${String(err)}`, true);
          }
        });
      }

      // Tauri 2 emits `tauri://drag-drop` on the window with `{ paths }`.
      // Accept the first .var when the Missing Resources view is active.
      const tauriEvent = window.__TAURI__?.event;
      if (tauriEvent && typeof tauriEvent.listen === "function") {
        const isViewActive = () => {
          const view = $("missing-resources-view");
          return view && !view.classList.contains("hidden");
        };
        const paintDrag = (active) => {
          const dz = $("missing-target-dropzone");
          if (dz) dz.classList.toggle("is-dragover", active);
        };
        tauriEvent.listen("tauri://drag-drop", (event) => {
          paintDrag(false);
          if (!isViewActive()) return;
          const paths = event?.payload?.paths ?? [];
          const varPath = paths.find((p) => /\.var$/i.test(String(p)));
          if (!varPath) {
            if (paths.length > 0) setStatus("Only .var files are accepted here.", true);
            return;
          }
          mr().targetVar = String(varPath);
          const el = $("missing-target-var");
          if (el) el.value = mr().targetVar;
          renderVarInfo();
        }).catch(() => {});
        tauriEvent.listen("tauri://drag-enter", () => {
          if (isViewActive()) paintDrag(true);
        }).catch(() => {});
        tauriEvent.listen("tauri://drag-over", () => {
          if (isViewActive()) paintDrag(true);
        }).catch(() => {});
        tauriEvent.listen("tauri://drag-leave", () => paintDrag(false)).catch(() => {});
      }
      const pickOutput = $("missing-pick-output");
      if (pickOutput) {
        pickOutput.addEventListener("click", async () => {
          if (!invoke) return;
          try {
            const selected = await invoke("pick_folder");
            if (selected) {
              mr().outputDir = String(selected);
              if (outputInput) outputInput.value = mr().outputDir;
            }
          } catch (err) {
            setStatus(`Pick failed: ${String(err)}`, true);
          }
        });
      }
      // Candidate-source mode (Local only / Database). Display-only: flipping
      // it just re-renders the detail panel — no re-scan, and detection of
      // missing refs is unaffected.
      for (const radio of document.querySelectorAll('input[type="radio"][name="missing-mode"]')) {
        radio.addEventListener("change", () => {
          if (!radio.checked) return;
          mr().mode = radio.value === "db" ? "db" : "local";
          renderDetail();
        });
      }
      const backup = $("missing-backup");
      const replaceCheckbox = $("missing-replace-in-place");
      const syncBackupEnabled = () => {
        // Match Overview: backup only meaningful in replace-in-place mode.
        if (backup && replaceCheckbox) backup.disabled = !replaceCheckbox.checked;
      };
      if (backup) {
        backup.checked = !!mr().backup;
        backup.addEventListener("change", () => {
          mr().backup = !!backup.checked;
        });
      }
      if (replaceCheckbox) {
        replaceCheckbox.checked = !!mr().replaceInPlace;
        replaceCheckbox.addEventListener("change", () => {
          mr().replaceInPlace = !!replaceCheckbox.checked;
          // Replace-in-place overwrites the original — turn backup on by
          // default whenever the user enables it, so they don't lose work
          // unless they explicitly opt out.
          if (replaceCheckbox.checked && backup) {
            backup.checked = true;
            mr().backup = true;
          }
          syncBackupEnabled();
        });
      }
      syncBackupEnabled();
      const scanBtn = $("missing-scan-button");
      if (scanBtn) scanBtn.addEventListener("click", scanForBrokenRefs);
      const runBtn = $("missing-run-button");
      if (runBtn) runBtn.addEventListener("click", applyFixes);
      const clearBtn = $("missing-clear-button");
      if (clearBtn) clearBtn.addEventListener("click", () => {
        mr().replacementMap = {};
        updateRunButton();
        renderList();
        renderDetail();
      });

      // Quick-apply handlers: copy the currently-selected ref's
      // replacement_pkg onto every other ref in scope (same broken pkg /
      // currently filtered list / all). Mirrors the Find Duplicates page's
      // scope / filtered / global trio. Skips refs that don't have a
      // candidate from the chosen package — we never invent a fix.
      const runQuickApply = (scope) => {
        const key = mr().selectedKey;
        const ref = key ? findRefByKey(key) : null;
        const fix = key ? mr().replacementMap[key] : null;
        if (!ref || !fix?.replacement_pkg) return;
        const allRefs = mr().brokenRefs ?? [];
        let targets;
        if (scope === "scope") {
          // "Same broken pkg" — every ref whose ref_pkg matches the current
          // selection. Picking once for `Anonymous.VAM灵梦-春庭雪.1:/X.vam`
          // and clicking this applies the same replacement package to all
          // other refs from that package, where a candidate exists.
          targets = allRefs.filter((r) => r.ref_pkg === ref.ref_pkg);
        } else if (scope === "filtered") {
          const filterLc = (mr().filter ?? "").toLowerCase().trim();
          targets = allRefs.filter((r) => refMatchesFilter(r, filterLc));
        } else {
          targets = allRefs;
        }
        const applied = applyReplacementToRefs(
          targets,
          fix.replacement_pkg,
          fix.replacement_license_type ?? null,
        );
        const scopeLabel = scope === "scope" ? "same-pkg refs"
          : scope === "filtered" ? "filtered refs"
          : "all refs";
        setStatus(
          applied === 0
            ? `No ${scopeLabel} had a matching candidate from ${fix.replacement_pkg}.`
            : `Applied ${fix.replacement_pkg} to ${applied} ${scopeLabel}.`,
          applied === 0,
        );
        updateRunButton();
        renderList();
        renderDetail();
      };
      const scopeBtn = $("missing-apply-scope-button");
      if (scopeBtn) scopeBtn.addEventListener("click", () => runQuickApply("scope"));
      const filteredBtn = $("missing-apply-filtered-button");
      if (filteredBtn) filteredBtn.addEventListener("click", () => runQuickApply("filtered"));
      const globalBtn = $("missing-apply-global-button");
      if (globalBtn) globalBtn.addEventListener("click", () => runQuickApply("global"));
      const filterInput = $("missing-list-filter");
      if (filterInput) {
        filterInput.addEventListener("input", () => {
          mr().filter = filterInput.value ?? "";
          mr().listPage = 0;
          renderList();
        });
      }

      const listEl = $("missing-list");
      if (listEl) {
        listEl.addEventListener("click", (event) => {
          const row = event.target.closest("[data-missing-key]");
          if (!row) return;
          const key = row.getAttribute("data-missing-key");
          if (!key) return;
          mr().selectedKey = key;
          // Refresh the quick-apply enable state — it depends on whether the
          // newly-selected ref already has a replacement picked.
          updateRunButton();
          renderList();
          renderDetail();
        });
        listEl.addEventListener("contextmenu", (event) => {
          const row = event.target.closest("[data-missing-key]");
          if (!row) return;
          const key = row.getAttribute("data-missing-key");
          if (!key) return;
          event.preventDefault();
          hideContextMenu();
          // Match Overview behavior: right-click selects the row before
          // opening the menu so actions operate on the visible target.
          mr().selectedKey = key;
          updateRunButton();
          renderList();
          renderDetail();
          const ref = findRefByKey(key);
          if (!ref) return;
          const items = brokenRefMenuItems(ref);
          if (items.length) {
            showContextMenu(event.clientX, event.clientY, items);
          }
        });
      }

      const dbContainer = $("missing-db-candidates");
      const localContainer = $("missing-local-candidates");
      const onCandidateChange = (event) => {
        const target = event.target;
        if (!target || target.name !== "missing-candidate") return;
        const key = mr().selectedKey;
        const ref = key ? findRefByKey(key) : null;
        if (!key || !ref) return;
        const source = target.getAttribute("data-source") ?? "local";
        const replacementPkg = target.value;
        const replacementPath = target.getAttribute("data-internal-path");
        mr().replacementMap[key] = {
          broken_pkg: ref.ref_pkg,
          broken_path: ref.ref_path ?? null,
          replacement_pkg: replacementPkg,
          replacement_path: replacementPath || null,
          source,
        };
        updateRunButton();
        renderList();
        renderDetail();
      };
      if (dbContainer) dbContainer.addEventListener("change", onCandidateChange);
      if (localContainer) localContainer.addEventListener("change", onCandidateChange);

      // Action buttons inside each candidate row (VAR Details / Copy id).
      // Buttons live inside the `<label>`, so a click would normally activate
      // the radio — preventDefault on the label-bound click stops that, and
      // stopPropagation keeps it from reaching the row-select handler.
      const onCandidateClick = (event) => {
        const btn = event.target.closest(".missing-candidate-action");
        if (!btn) return;
        event.preventDefault();
        event.stopPropagation();
        const action = btn.getAttribute("data-action");
        const packageId = btn.getAttribute("data-package-id") || "";
        if (!packageId) return;
        if (action === "copy-pkg-id") {
          copyTextToClipboard(packageId).then((ok) => {
            if (ok) addLog(`Copied ${packageId}`);
          });
          return;
        }
        if (action === "open-var-details") {
          const row = btn.closest(".missing-candidate");
          const packageFile = row?.getAttribute("data-package-file") || "";
          openCandidatePackageInVarDetails(packageId, packageFile);
        }
      };
      if (dbContainer) dbContainer.addEventListener("click", onCandidateClick);
      if (localContainer) localContainer.addEventListener("click", onCandidateClick);

      // Right-click on a candidate row → context menu with "Disable creator"
      // alongside the same Open / Copy / Show actions surfaced inline. Matches
      // the Overview / Find Duplicates contextmenu UX so the block action is
      // discoverable in the same gesture across all three pages.
      const onCandidateContextMenu = (event) => {
        const row = event.target.closest(".missing-candidate");
        if (!row) return;
        event.preventDefault();
        const radio = row.querySelector('input[name="missing-candidate"]');
        const packageId = radio?.value || "";
        const packageFile = row.getAttribute("data-package-file") || "";
        const items = [];
        if (packageId) {
          items.push({
            label: t("resourceListContextOpenPackage"),
            action: () => openCandidatePackageInVarDetails(packageId, packageFile),
          });
          items.push({
            label: `Copy package id (${packageId})`,
            action: async () => {
              const ok = await copyTextToClipboard(packageId);
              if (ok) addLog(`Copied ${packageId}`);
            },
          });
        }
        if (packageFile) {
          items.push({
            label: t("menuShowInExplorer"),
            action: () => showPackageInExplorer(packageFile),
          });
        }
        const creator = deriveCreatorFromPackageId(packageId);
        if (creator && !_blockedCreators.has(creator)) {
          items.push({
            label: `Disable creator (${creator})`,
            action: () => blockCreatorByPackageId(packageId),
          });
        }
        if (items.length) showContextMenu(event.clientX, event.clientY, items);
      };
      if (dbContainer) dbContainer.addEventListener("contextmenu", onCandidateContextMenu);
      if (localContainer) localContainer.addEventListener("contextmenu", onCandidateContextMenu);

      // DB search: debounce so we don't refetch on every keystroke, and
      // reset the page to 0 + new search whenever the term changes.
      const dbSearch = $("missing-db-search");
      if (dbSearch) {
        let searchTimer = null;
        dbSearch.addEventListener("input", () => {
          if (searchTimer) clearTimeout(searchTimer);
          const value = dbSearch.value;
          searchTimer = setTimeout(() => {
            const key = mr().selectedKey;
            const ref = key ? findRefByKey(key) : null;
            if (!ref) return;
            resetAndRefetchDb(ref, value.trim());
          }, 200);
        });
      }

      // Infinite scroll: when the sentinel below the candidate list scrolls
      // into view, fetch the next page. IntersectionObserver root is the
      // scrolling container so we only fire when the user is actually
      // scrolling that subtree.
      const dbScroll = $("missing-db-scroll");
      const dbSentinel = $("missing-db-sentinel");
      if (dbScroll && dbSentinel && typeof IntersectionObserver !== "undefined") {
        const observer = new IntersectionObserver((entries) => {
          for (const e of entries) {
            if (!e.isIntersecting) continue;
            const key = mr().selectedKey;
            const ref = key ? findRefByKey(key) : null;
            if (!ref) continue;
            const entry = ensureDbState(brokenKey(ref));
            if (entry.loading || !entry.initialized || !entry.hasMore) continue;
            fetchDbCandidatesPage(ref).catch(() => {});
          }
        }, { root: dbScroll, rootMargin: "120px", threshold: 0 });
        observer.observe(dbSentinel);
      }

      if (localContainer) {
        localContainer.addEventListener("contextmenu", (event) => {
          const row = event.target.closest("[data-package-file]");
          if (!row) return;
          const packageFile = row.getAttribute("data-package-file");
          if (!packageFile) return;
          event.preventDefault();
          hideContextMenu();
          showContextMenu(event.clientX, event.clientY, [
            {
              label: "Show in Explorer",
              action: async () => {
                if (!invoke) return;
                try {
                  await invoke("show_in_explorer", { path: packageFile });
                } catch (err) {
                  // Surface the Rust-side failure so the user can see why
                  // (e.g. path was canonicalised differently, file moved).
                  addLog(`Show in Explorer failed: ${String(err)} — path: ${packageFile}`);
                  setStatus(`Show in Explorer failed: ${String(err)}`, true);
                }
              },
            },
            {
              label: "Copy file path",
              action: async () => {
                const ok = await copyTextToClipboard(packageFile);
                if (ok) addLog(`Copied ${packageFile}`);
              },
            },
          ]);
        });
      }
    }

    function syncInputsFromState() {
      const targetInput = $("missing-target-var");
      const dirInput = $("missing-input-dir");
      const outputInput = $("missing-output-dir");
      const backup = $("missing-backup");
      const replaceCheckbox = $("missing-replace-in-place");
      if (targetInput && !targetInput.value) targetInput.value = mr().targetVar;
      if (dirInput && !dirInput.value) dirInput.value = mr().inputDir;
      if (outputInput && !outputInput.value) outputInput.value = mr().outputDir;
      if (backup) backup.checked = !!mr().backup;
      if (replaceCheckbox) replaceCheckbox.checked = !!mr().replaceInPlace;
      if (backup && replaceCheckbox) backup.disabled = !replaceCheckbox.checked;
      setRadioValue("missing-mode", mr().mode === "db" ? "db" : "local");
    }

    function renderVarInfo() {
      const emptyHint = $("missing-var-info-empty");
      const body = $("missing-var-info-body");
      const label = $("missing-var-info-label");
      const sizeVal = $("missing-var-info-size-val");
      const timeVal = $("missing-var-info-time-val");
      const sceneRow = $("missing-var-info-scene-row");
      const sceneVal = $("missing-var-info-scene-val");
      const thumb = $("missing-var-info-thumb");
      const img = $("missing-var-info-scene-img");
      if (!emptyHint || !body || !label) return;

      const target = mr().targetVar.trim();
      if (!target) {
        body.classList.add("hidden");
        emptyHint.classList.remove("hidden");
        return;
      }
      emptyHint.classList.add("hidden");
      body.classList.remove("hidden");

      // Derive package id (file name without .var)
      const fileName = target.split(/[\\/]/).pop() ?? "";
      const pkgId = fileName.replace(/\.var$/i, "");
      label.textContent = pkgId || target;
      if (sizeVal) sizeVal.textContent = "—";
      if (timeVal) timeVal.textContent = "—";
      if (sceneVal) sceneVal.textContent = "";
      if (img) img.src = "";
      if (sceneRow) sceneRow.classList.add("hidden");
      if (thumb) {
        thumb.classList.add("hidden");
        thumb.classList.add("empty");
      }

      if (!invoke) return;
      invoke("get_var_file_stats", { packagePath: target })
        .then((stats) => {
          if (mr().targetVar.trim() !== target) return;
          if (sizeVal) sizeVal.textContent = formatBytesLocal(stats.size_bytes);
          const date = stats.modified_ms != null ? new Date(stats.modified_ms) : null;
          if (timeVal) timeVal.textContent = date ? date.toLocaleString() : "—";
          if (stats.scene_image_path) {
            if (sceneVal) sceneVal.textContent = stats.scene_image_path;
            if (img) img.src = stats.scene_image_data || "";
            if (sceneRow) sceneRow.classList.remove("hidden");
            if (thumb) {
              thumb.classList.remove("hidden");
              if (stats.scene_image_data) thumb.classList.remove("empty");
            }
          }
        })
        .catch(() => {});
    }

    bindEvents();

    window.__refreshMissingResourcesView = function () {
      // Seed from Overview's currently-active paths if Missing Resources is
      // fresh — saves the user retyping the folder/target they just used.
      if (!mr().inputDir) {
        const overviewInput = $("input-dir");
        if (overviewInput?.value) mr().inputDir = overviewInput.value;
      }
      if (!mr().targetVar) {
        const overviewTarget = $("target-var-path");
        if (overviewTarget?.value) mr().targetVar = overviewTarget.value;
      }
      syncInputsFromState();
      renderVarInfo();
      renderList();
      renderDetail();
      updateRunButton();
      setStatus(mr().status, false);
    };

    renderVarInfo();
    renderList();
    renderDetail();
    updateRunButton();
  })();

  // ===========================================================
  // Internalize Resources page (sidebar link `internalize-resources`)
  //
  // The mirror image of Missing Resources: scans the target VAR for *valid*
  // external `Pkg:/path` refs and lets the user inline whichever ones they
  // want by copying the source bundle into the target (rewriting refs to
  // `SELF:/...`). When every ref to a source pkg is internalized, the source
  // pkg is dropped from the target's meta.json dependencies — at which point
  // the user can delete the (often huge) source var entirely.
  // IIFE-scoped to keep helpers out of the global namespace.
  // ===========================================================
  (function setupInternalizeResources() {
    if (!state.internalize) return;
    function iz() { return state.internalize; }

    function refKey(pkg, path) {
      return `${pkg}|${path ?? ""}`;
    }

    function setStatus(msg, isError) {
      iz().status = msg ?? "";
      const el = $("internalize-status");
      if (!el) return;
      el.textContent = msg ?? "";
      el.classList.toggle("missing-status-error", !!isError);
    }

    function groupMatchesFilter(group, filterLc) {
      if (!filterLc) return true;
      if (group.source_pkg_id.toLowerCase().includes(filterLc)) return true;
      // Search bundles by ref path too so users can find "the one with this
      // texture" without already knowing the source pkg id.
      return (group.refs || []).some((r) =>
        (r.ref_path || "").toLowerCase().includes(filterLc),
      );
    }

    function countSelectedInGroup(group) {
      if (!group) return 0;
      let n = 0;
      for (const r of group.refs || []) {
        if (iz().selectedRefs[refKey(group.source_pkg_id, r.ref_path)]) n++;
      }
      return n;
    }

    function totalSelectedRefs() {
      // Recount from scratch every time — selectedRefs entries can become
      // stale across scans (a previously-selected ref might not appear in
      // the new group list). Only count refs we still have a group for.
      let n = 0;
      for (const g of iz().groups || []) {
        for (const r of g.refs || []) {
          if (iz().selectedRefs[refKey(g.source_pkg_id, r.ref_path)]) n++;
        }
      }
      return n;
    }

    function findGroup(pkg) {
      return (iz().groups || []).find((g) => g.source_pkg_id === pkg);
    }

    function renderList() {
      const container = $("internalize-list");
      const subtitle = $("internalize-list-subtitle");
      if (!container) return;
      const groups = iz().groups || [];
      const filterLc = (iz().filter ?? "").toLowerCase().trim();
      const visible = groups.filter((g) => groupMatchesFilter(g, filterLc));
      const scanned = !!iz().lastScanCompleted;

      if (subtitle) {
        if (groups.length === 0) {
          if (iz().scanning) {
            subtitle.textContent = "Scanning…";
          } else if (scanned) {
            subtitle.textContent = "Scan complete — no internalizable external refs found.";
          } else {
            subtitle.textContent = "Scan a target VAR to populate this list.";
          }
        } else {
          const selected = totalSelectedRefs();
          subtitle.textContent = `${groups.length} source pkg${groups.length === 1 ? "" : "s"}` +
            (selected ? ` — ${selected} ref${selected === 1 ? "" : "s"} selected` : "");
        }
      }

      if (visible.length === 0) {
        container.classList.add("empty");
        let emptyHtml;
        if (groups.length > 0) {
          emptyHtml = `<p class="group-empty">No source packages match the current filter.</p>`;
        } else if (iz().scanning) {
          emptyHtml = `<p class="group-empty">Scanning…</p>`;
        } else if (scanned) {
          emptyHtml = `
            <div class="missing-empty-clean">
              <span class="material-symbols-outlined missing-empty-clean-icon">check_circle</span>
              <h4 class="missing-empty-clean-title">No internalizable refs found</h4>
              <p class="missing-empty-clean-subtitle">
                Every external <code>Pkg:/path</code> reference is either broken
                (handled by Missing Resources) or the target has none.
              </p>
            </div>
          `;
        } else {
          emptyHtml = `<p class="group-empty">No external refs yet. Click <strong>Scan</strong> to analyze the target VAR.</p>`;
        }
        container.innerHTML = emptyHtml;
        renderListPagination(0);
        return;
      }
      container.classList.remove("empty");

      const totalPages = Math.max(1, Math.ceil(visible.length / GROUP_PAGE_SIZE));
      if (iz().listPage >= totalPages) iz().listPage = totalPages - 1;
      if (iz().listPage < 0) iz().listPage = 0;
      const start = iz().listPage * GROUP_PAGE_SIZE;
      const pageVisible = visible.slice(start, start + GROUP_PAGE_SIZE);

      const html = pageVisible.map((g) => {
        const isSelected = g.source_pkg_id === iz().selectedPkg;
        const refCount = (g.refs || []).length;
        const picked = countSelectedInGroup(g);
        const pickedChip = picked
          ? `<span class="chip chip-accent missing-row-fixed">${picked}/${refCount} picked</span>`
          : "";
        const sourceSize = formatBytesLocal(g.source_var_size ?? 0);
        const bundleSize = formatBytesLocal(g.total_bundle_bytes ?? 0);
        return `
          <button type="button" class="group-row missing-row ${isSelected ? "active focused" : ""}" data-internalize-pkg="${escapeAttribute(g.source_pkg_id)}">
            <div class="group-row-line missing-row-head">
              <span class="missing-row-pkg">${escapeHtml(g.source_pkg_id)}</span>
              ${pickedChip}
            </div>
            <div class="group-row-line missing-row-meta">
              <span>${refCount} ref${refCount === 1 ? "" : "s"}</span>
              <span>·</span>
              <span title="Total bundle bytes if every ref is internalized">copy ~${bundleSize}</span>
              <span>·</span>
              <span title="On-disk size of the source VAR">source ${sourceSize}</span>
            </div>
          </button>
        `;
      }).join("");
      container.innerHTML = html;
      renderListPagination(visible.length);
    }

    function renderListPagination(totalVisible) {
      const pagination = $("internalize-list-pagination");
      if (!pagination) return;
      if (totalVisible <= GROUP_PAGE_SIZE) {
        pagination.classList.add("hidden");
        pagination.innerHTML = "";
        return;
      }
      const totalPages = Math.max(1, Math.ceil(totalVisible / GROUP_PAGE_SIZE));
      if (iz().listPage >= totalPages) iz().listPage = totalPages - 1;
      const currentPage = iz().listPage;
      const pages = buildPageNumbers(totalPages, currentPage);

      pagination.classList.remove("hidden");
      pagination.innerHTML = `
        <button class="page-nav-button" data-page-action="prev" type="button" ${currentPage === 0 ? "disabled" : ""}>
          <span class="material-symbols-outlined">chevron_left</span>
          Previous
        </button>
        <div class="page-numbers">
          ${pages
            .map((page) =>
              page === "ellipsis"
                ? `<span class="page-ellipsis">…</span>`
                : `<button class="page-button ${page === currentPage ? "active" : ""}" data-page-jump="${page}" type="button">${page + 1}</button>`
            )
            .join("")}
        </div>
        <button class="page-nav-button" data-page-action="next" type="button" ${currentPage >= totalPages - 1 ? "disabled" : ""}>
          Next
          <span class="material-symbols-outlined">chevron_right</span>
        </button>
      `;
      pagination.querySelectorAll("[data-page-action]").forEach((button) => {
        button.addEventListener("click", () => {
          const action = button.dataset.pageAction;
          if (action === "prev") iz().listPage = Math.max(0, iz().listPage - 1);
          if (action === "next") iz().listPage = Math.min(totalPages - 1, iz().listPage + 1);
          renderList();
        });
      });
      pagination.querySelectorAll("[data-page-jump]").forEach((button) => {
        button.addEventListener("click", () => {
          iz().listPage = Number(button.dataset.pageJump) || 0;
          renderList();
        });
      });
    }

    function renderDetail() {
      const empty = $("internalize-detail-empty");
      const panel = $("internalize-detail-panel");
      const pkg = iz().selectedPkg;
      const group = pkg ? findGroup(pkg) : null;
      if (!group || !panel || !empty) {
        if (panel) panel.classList.add("hidden");
        if (empty) empty.classList.remove("hidden");
        return;
      }
      empty.classList.add("hidden");
      panel.classList.remove("hidden");

      const pkgEl = $("internalize-detail-pkg");
      if (pkgEl) pkgEl.textContent = group.source_pkg_id;
      const sizeChip = $("internalize-detail-source-size");
      if (sizeChip) sizeChip.textContent = `Source VAR ${formatBytesLocal(group.source_var_size ?? 0)}`;
      const bundleChip = $("internalize-detail-bundle-size");
      if (bundleChip) bundleChip.textContent = `Bundle ${formatBytesLocal(group.total_bundle_bytes ?? 0)}`;

      const refsEl = $("internalize-refs");
      if (refsEl) {
        const pkgAttr = escapeAttribute(group.source_pkg_id);
        const sourceVarAttr = escapeAttribute(group.source_var_path || "");
        const rows = (group.refs || []).map((r) => {
          const key = refKey(group.source_pkg_id, r.ref_path);
          const checked = !!iz().selectedRefs[key];
          const crcStr = (typeof r.crc32 === "number")
            ? `CRC32 ${r.crc32.toString(16).padStart(8, "0").toUpperCase()}`
            : "";
          const bundleMembers = (r.bundle_paths || []).length;
          const sourceFiles = (r.source_files_in_target || []);
          // Limit the previewed bundle list — clothing items can drag in a
          // dozen textures and we don't want the row to balloon. The full
          // list still gets copied at apply time; this is a display cap.
          const previewBundles = (r.bundle_paths || []).slice(0, 5);
          const moreBundles = bundleMembers > previewBundles.length
            ? `<span class="missing-source-empty">…+${bundleMembers - previewBundles.length} more</span>`
            : "";
          const bundleList = previewBundles
            .map((p) => `<li><code>${escapeHtml(p)}</code></li>`)
            .join("");
          const referencedIn = sourceFiles.length === 0
            ? `<li class="missing-source-empty">No payload references (meta.json only).</li>`
            : sourceFiles.map((f) => `<li><code>${escapeHtml(f)}</code></li>`).join("");
          const refPathAttr = escapeAttribute(r.ref_path);
          return `
            <label class="source-row missing-candidate ${checked ? "is-checked" : ""}"
                   data-ref-path="${refPathAttr}"
                   data-source-pkg="${pkgAttr}"
                   data-source-var="${sourceVarAttr}">
              <input type="checkbox" class="internalize-ref-check" ${checked ? "checked" : ""}
                     data-ref-key="${escapeAttribute(key)}" />
              <div class="source-main">
                <strong><code>${escapeHtml(r.ref_path)}</code></strong>
                <span class="source-row-meta">
                  ${formatBytesLocal(r.bundle_total_size ?? r.size ?? 0)} bundle
                  · ${bundleMembers} file${bundleMembers === 1 ? "" : "s"}
                  ${crcStr ? `· ${crcStr}` : ""}
                </span>
                <details class="internalize-bundle-details">
                  <summary>Bundle members (${bundleMembers})</summary>
                  <ul class="missing-source-files">${bundleList}${moreBundles}</ul>
                </details>
                <details class="internalize-referenced-details">
                  <summary>Referenced in (${sourceFiles.length})</summary>
                  <ul class="missing-source-files">${referencedIn}</ul>
                </details>
                <div class="missing-candidate-actions">
                  <button type="button" class="missing-candidate-action" data-iz-action="open-var-details"
                          data-source-pkg="${pkgAttr}" data-source-var="${sourceVarAttr}"
                          title="Open the source package in VAR Details">
                    <span class="material-symbols-outlined">description</span>
                    <span>VAR Details</span>
                  </button>
                  <button type="button" class="missing-candidate-action" data-iz-action="copy-ref"
                          data-source-pkg="${pkgAttr}" data-ref-path="${refPathAttr}"
                          title="Copy SourcePkg:/path to clipboard">
                    <span class="material-symbols-outlined">content_copy</span>
                    <span>Copy ref</span>
                  </button>
                  <button type="button" class="missing-candidate-action" data-iz-action="copy-pkg-id"
                          data-source-pkg="${pkgAttr}"
                          title="Copy source package id to clipboard">
                    <span class="material-symbols-outlined">tag</span>
                    <span>Copy id</span>
                  </button>
                  <button type="button" class="missing-candidate-action" data-iz-action="copy-path"
                          data-ref-path="${refPathAttr}"
                          title="Copy the internal path to clipboard">
                    <span class="material-symbols-outlined">link</span>
                    <span>Copy path</span>
                  </button>
                  <button type="button" class="missing-candidate-action" data-iz-action="show-in-explorer"
                          data-source-var="${sourceVarAttr}"
                          title="Show the source .var file in Explorer">
                    <span class="material-symbols-outlined">folder_open</span>
                    <span>Show .var</span>
                  </button>
                </div>
              </div>
            </label>
          `;
        }).join("");
        refsEl.innerHTML = rows;
      }

      const clearBtn = $("internalize-clear-button");
      const selectAllBtn = $("internalize-select-all-group");
      const picked = countSelectedInGroup(group);
      if (clearBtn) clearBtn.disabled = picked === 0;
      if (selectAllBtn) selectAllBtn.disabled = (group.refs || []).length === 0;
    }

    function updateRunButton() {
      const btn = $("internalize-run-button");
      if (!btn) return;
      const picked = totalSelectedRefs();
      btn.disabled = picked === 0 || iz().applying;
      btn.textContent = picked === 0
        ? "Internalize Selected"
        : `Internalize ${picked} Ref${picked === 1 ? "" : "s"}`;
    }

    function showProgress(title, fraction, message) {
      const card = $("internalize-progress-card");
      const pct = $("internalize-progress-percent");
      const bar = $("internalize-progress-bar");
      const msg = $("internalize-progress-message");
      const titleEl = $("internalize-progress-title");
      if (!card) return;
      card.classList.remove("hidden");
      const clamped = Math.max(0, Math.min(1, Number(fraction) || 0));
      if (pct) pct.textContent = `${Math.round(clamped * 100)}%`;
      if (bar) bar.style.width = `${clamped * 100}%`;
      if (msg) msg.textContent = message ?? "";
      if (titleEl && title) titleEl.textContent = title;
    }

    function hideProgress() {
      const card = $("internalize-progress-card");
      if (card) card.classList.add("hidden");
      const bar = $("internalize-progress-bar");
      if (bar) bar.style.width = "0%";
      const pct = $("internalize-progress-percent");
      if (pct) pct.textContent = "0%";
    }

    function sleep(ms) {
      return new Promise((resolve) => setTimeout(resolve, ms));
    }

    async function runScanTask(inputDir, targetVar) {
      // Reuse the same scan_task command as Missing Resources — it populates
      // the shared scan cache that scan_internalize_candidates reads from.
      const handle = await invoke("start_scan_task", {
        request: {
          input_dir: inputDir,
          additional_input_dirs: getAdditionalDirs("internalize"),
          target_var_path: targetVar,
          skip_db: true,
        },
      });
      const taskId = handle?.id;
      if (taskId == null) throw new Error("scan task did not return a handle");
      const cleanup = () => invoke("clear_task", { taskId }).catch(() => {});
      try {
        while (true) {
          const payload = await invoke("get_task_progress", { taskId });
          if (payload) {
            const fraction = Number(payload.progress ?? 0);
            const msg = payload.message ?? "";
            showProgress("Scanning", fraction, msg);
            if (payload.done) {
              if (payload.error) throw new Error(String(payload.error));
              return payload;
            }
          }
          await sleep(TASK_POLL_MS);
        }
      } finally {
        cleanup();
      }
    }

    async function scanExternalRefs() {
      if (!invoke) return;
      const inputDir = iz().inputDir.trim();
      const targetVar = iz().targetVar.trim();
      if (!inputDir || !targetVar) {
        setStatus("Pick both a VAR folder and a target .var file.", true);
        return;
      }
      iz().scanning = true;
      iz().groups = [];
      iz().selectedRefs = {};
      iz().selectedPkg = null;
      iz().listPage = 0;
      iz().lastScanCompleted = false;
      setStatus("", false);
      showProgress("Scanning", 0, "Starting scan…");
      renderList();
      renderDetail();
      updateRunButton();
      const scanBtn = $("internalize-scan-button");
      if (scanBtn) scanBtn.disabled = true;
      try {
        await runScanTask(inputDir, targetVar);
        showProgress("Analyzing", 0.97, "Harvesting external refs…");
        const groups = await invoke("scan_internalize_candidates", {
          inputDir,
          additionalInputDirs: getAdditionalDirs("internalize"),
          targetVarPath: targetVar,
        });
        iz().groups = Array.isArray(groups) ? groups : [];
        iz().lastScanCompleted = true;
        const count = iz().groups.length;
        const totalRefs = iz().groups.reduce((sum, g) => sum + (g.refs?.length || 0), 0);
        const summary = count === 0
          ? "Scan complete — no internalizable external refs found."
          : `Found ${count} source pkg${count === 1 ? "" : "s"}, ${totalRefs} ref${totalRefs === 1 ? "" : "s"}.`;
        showProgress("Done", 1, summary);
        setStatus(summary, false);
        setTimeout(hideProgress, 1500);
      } catch (err) {
        hideProgress();
        setStatus(String(err && err.message ? err.message : err), true);
        iz().groups = [];
        iz().lastScanCompleted = false;
      } finally {
        iz().scanning = false;
        if (scanBtn) scanBtn.disabled = false;
        renderList();
        renderDetail();
        updateRunButton();
      }
    }

    async function applyInternalize() {
      if (!invoke) return;
      // Build selections from selectedRefs map. Skip stale keys whose group
      // is no longer present (a defensive measure — the scan refresh clears
      // selectedRefs, but better to filter than send garbage to Rust).
      const selections = [];
      for (const g of iz().groups || []) {
        for (const r of g.refs || []) {
          const key = refKey(g.source_pkg_id, r.ref_path);
          if (iz().selectedRefs[key]) {
            selections.push({ source_pkg_id: g.source_pkg_id, ref_path: r.ref_path });
          }
        }
      }
      if (selections.length === 0) return;
      const replaceInPlace = !!iz().replaceInPlace;
      const outputDir = iz().outputDir.trim();
      if (!replaceInPlace && !outputDir) {
        setStatus("Set an Output Folder or enable 'Replace in place' before running.", true);
        return;
      }
      iz().applying = true;
      updateRunButton();
      setStatus(`Internalizing ${selections.length} ref${selections.length === 1 ? "" : "s"}…`, false);
      showProgress("Applying", 0, `Rewriting target VAR with ${selections.length} ref${selections.length === 1 ? "" : "s"}…`);
      const progressBar = $("internalize-progress-bar");
      const progressPct = $("internalize-progress-percent");
      if (progressBar) progressBar.classList.add("is-indeterminate");
      if (progressPct) progressPct.textContent = "";
      let taskId = null;
      try {
        const handle = await invoke("start_apply_internalize_task", {
          inputDir: iz().inputDir.trim(),
          additionalInputDirs: getAdditionalDirs("internalize"),
          targetVarPath: iz().targetVar.trim(),
          outputDir: outputDir || null,
          replaceInPlace,
          selections,
          backup: !!iz().backup,
        });
        taskId = handle?.id;
        if (taskId == null) throw new Error("internalize task did not return a handle");
        let report = null;
        while (true) {
          const payload = await invoke("get_task_progress", { taskId });
          if (payload) {
            const msg = payload.message ?? "";
            if (msg) setStatus(msg, false);
            if (payload.done) {
              if (payload.error) throw new Error(String(payload.error));
              report = payload.internalize_report ?? null;
              break;
            }
          }
          await sleep(TASK_POLL_MS);
        }
        if (progressBar) progressBar.classList.remove("is-indeterminate");
        const copied = report?.entries_copied ?? 0;
        const skipped = (report?.entries_skipped_collision || []).length;
        const depsRemoved = (report?.dependencies_removed || []).length;
        const rewritten = report?.files_rewritten ?? 0;
        const bytesAdded = report?.bytes_added ?? 0;
        const wrotePath = report?.output_path ?? "";
        setStatus(
          `Internalized ${report?.refs_rewritten ?? selections.length} ref(s). ` +
          `${copied} file(s) copied (${formatBytesLocal(bytesAdded)}), ${rewritten} file(s) rewritten, ` +
          `${depsRemoved} dep(s) dropped` +
          (skipped ? `, ${skipped} skipped (collision)` : "") +
          (wrotePath ? ` → ${wrotePath}` : ""),
          false,
        );
        // Reset selections; re-run the scan so the list reflects the new
        // state (any internalized ref now becomes a SELF: ref the next scan
        // won't pick up).
        iz().selectedRefs = {};
        iz().selectedPkg = null;
        iz().listPage = 0;
        showProgress("Applying", 0.85, "Re-analyzing target VAR…");
        try {
          const groups = await invoke("scan_internalize_candidates", {
            inputDir: iz().inputDir.trim(),
            additionalInputDirs: getAdditionalDirs("internalize"),
            targetVarPath: iz().targetVar.trim(),
          });
          iz().groups = Array.isArray(groups) ? groups : [];
        } catch (err) {
          iz().groups = [];
        }
        showProgress("Done", 1, `Internalized ${report?.refs_rewritten ?? selections.length} ref(s).`);
        setTimeout(hideProgress, 1500);
        renderList();
        renderDetail();
      } catch (err) {
        if (progressBar) progressBar.classList.remove("is-indeterminate");
        hideProgress();
        setStatus(`Internalize failed: ${String(err)}`, true);
      } finally {
        if (progressBar) progressBar.classList.remove("is-indeterminate");
        if (taskId != null) {
          invoke("clear_task", { taskId }).catch(() => {});
        }
        iz().applying = false;
        updateRunButton();
      }
    }

    function syncInputsFromState() {
      const dirInput = $("internalize-input-dir");
      const outputInput = $("internalize-output-dir");
      const targetInput = $("internalize-target-var");
      const backup = $("internalize-backup");
      const replaceCheckbox = $("internalize-replace-in-place");
      if (dirInput && !dirInput.value) dirInput.value = iz().inputDir;
      if (outputInput && !outputInput.value) outputInput.value = iz().outputDir;
      if (targetInput) targetInput.value = iz().targetVar;
      if (backup) backup.checked = !!iz().backup;
      if (replaceCheckbox) replaceCheckbox.checked = !!iz().replaceInPlace;
      if (backup && replaceCheckbox) backup.disabled = !replaceCheckbox.checked;
    }

    function renderVarInfo() {
      const emptyHint = $("internalize-var-info-empty");
      const body = $("internalize-var-info-body");
      const label = $("internalize-var-info-label");
      const sizeVal = $("internalize-var-info-size-val");
      const timeVal = $("internalize-var-info-time-val");
      const sceneRow = $("internalize-var-info-scene-row");
      const sceneVal = $("internalize-var-info-scene-val");
      const thumb = $("internalize-var-info-thumb");
      const img = $("internalize-var-info-scene-img");
      if (!emptyHint || !body || !label) return;

      const target = iz().targetVar.trim();
      if (!target) {
        body.classList.add("hidden");
        emptyHint.classList.remove("hidden");
        return;
      }
      emptyHint.classList.add("hidden");
      body.classList.remove("hidden");

      const fileName = target.split(/[\\/]/).pop() ?? "";
      const pkgId = fileName.replace(/\.var$/i, "");
      label.textContent = pkgId || target;
      if (sizeVal) sizeVal.textContent = "—";
      if (timeVal) timeVal.textContent = "—";
      if (sceneVal) sceneVal.textContent = "";
      if (img) img.src = "";
      if (sceneRow) sceneRow.classList.add("hidden");
      if (thumb) { thumb.classList.add("hidden"); thumb.classList.add("empty"); }

      if (!invoke) return;
      invoke("get_var_file_stats", { packagePath: target })
        .then((stats) => {
          if (iz().targetVar.trim() !== target) return;
          if (sizeVal) sizeVal.textContent = formatBytesLocal(stats.size_bytes);
          const date = stats.modified_ms != null ? new Date(stats.modified_ms) : null;
          if (timeVal) timeVal.textContent = date ? date.toLocaleString() : "—";
          if (stats.scene_image_path) {
            if (sceneVal) sceneVal.textContent = stats.scene_image_path;
            if (img) img.src = stats.scene_image_data || "";
            if (sceneRow) sceneRow.classList.remove("hidden");
            if (thumb) {
              thumb.classList.remove("hidden");
              if (stats.scene_image_data) thumb.classList.remove("empty");
            }
          }
        })
        .catch(() => {});
    }

    function bindEvents() {
      const targetInput = $("internalize-target-var");
      if (targetInput) {
        targetInput.addEventListener("input", () => {
          iz().targetVar = targetInput.value;
          renderVarInfo();
        });
      }
      const dropzone = $("internalize-target-dropzone");
      if (dropzone) {
        dropzone.addEventListener("click", (e) => {
          if (e.target.closest("button")) return;
          $("internalize-pick-target")?.click();
        });
        ["dragenter", "dragover"].forEach((evt) => {
          dropzone.addEventListener(evt, (e) => {
            e.preventDefault();
            dropzone.classList.add("is-dragover");
          });
        });
        ["dragleave", "drop"].forEach((evt) => {
          dropzone.addEventListener(evt, (e) => {
            e.preventDefault();
            dropzone.classList.remove("is-dragover");
          });
        });
      }
      const dirInput = $("internalize-input-dir");
      if (dirInput) {
        dirInput.addEventListener("input", () => {
          iz().inputDir = dirInput.value;
          const outputInput = $("internalize-output-dir");
          if (outputInput && !outputInput.value) {
            iz().outputDir = `${iz().inputDir}_internalized`;
            outputInput.value = iz().outputDir;
          }
        });
      }
      const outputInput = $("internalize-output-dir");
      if (outputInput) {
        outputInput.addEventListener("input", () => {
          iz().outputDir = outputInput.value;
        });
      }
      const pickTarget = $("internalize-pick-target");
      if (pickTarget) {
        pickTarget.addEventListener("click", async () => {
          if (!invoke) return;
          try {
            const selected = await invoke("pick_var_file");
            if (selected) {
              iz().targetVar = String(selected);
              if (targetInput) targetInput.value = iz().targetVar;
              renderVarInfo();
            }
          } catch (err) {
            setStatus(`Pick failed: ${String(err)}`, true);
          }
        });
      }
      const pickDir = $("internalize-pick-input");
      if (pickDir) {
        pickDir.addEventListener("click", async () => {
          if (!invoke) return;
          try {
            const selected = await invoke("pick_folder");
            if (selected) {
              iz().inputDir = String(selected);
              if (dirInput) dirInput.value = iz().inputDir;
              if (!iz().outputDir) {
                iz().outputDir = `${iz().inputDir}_internalized`;
                if (outputInput) outputInput.value = iz().outputDir;
              }
            }
          } catch (err) {
            setStatus(`Pick failed: ${String(err)}`, true);
          }
        });
      }
      // Same Tauri drag-drop wiring as Missing Resources — accept the first
      // .var when this view is active.
      const tauriEvent = window.__TAURI__?.event;
      if (tauriEvent && typeof tauriEvent.listen === "function") {
        const isViewActive = () => {
          const view = $("internalize-resources-view");
          return view && !view.classList.contains("hidden");
        };
        const paintDrag = (active) => {
          const dz = $("internalize-target-dropzone");
          if (dz) dz.classList.toggle("is-dragover", active);
        };
        tauriEvent.listen("tauri://drag-drop", (event) => {
          paintDrag(false);
          if (!isViewActive()) return;
          const paths = event?.payload?.paths ?? [];
          const varPath = paths.find((p) => /\.var$/i.test(String(p)));
          if (!varPath) {
            if (paths.length > 0) setStatus("Only .var files are accepted here.", true);
            return;
          }
          iz().targetVar = String(varPath);
          const el = $("internalize-target-var");
          if (el) el.value = iz().targetVar;
          renderVarInfo();
        }).catch(() => {});
        tauriEvent.listen("tauri://drag-enter", () => {
          if (isViewActive()) paintDrag(true);
        }).catch(() => {});
        tauriEvent.listen("tauri://drag-over", () => {
          if (isViewActive()) paintDrag(true);
        }).catch(() => {});
        tauriEvent.listen("tauri://drag-leave", () => paintDrag(false)).catch(() => {});
      }
      const pickOutput = $("internalize-pick-output");
      if (pickOutput) {
        pickOutput.addEventListener("click", async () => {
          if (!invoke) return;
          try {
            const selected = await invoke("pick_folder");
            if (selected) {
              iz().outputDir = String(selected);
              if (outputInput) outputInput.value = iz().outputDir;
            }
          } catch (err) {
            setStatus(`Pick failed: ${String(err)}`, true);
          }
        });
      }
      const backup = $("internalize-backup");
      const replaceCheckbox = $("internalize-replace-in-place");
      const syncBackupEnabled = () => {
        if (backup && replaceCheckbox) backup.disabled = !replaceCheckbox.checked;
      };
      if (backup) {
        backup.checked = !!iz().backup;
        backup.addEventListener("change", () => {
          iz().backup = !!backup.checked;
          invoke?.("save_config", { config: buildCurrentConfig() }).catch(() => {});
        });
      }
      if (replaceCheckbox) {
        replaceCheckbox.checked = !!iz().replaceInPlace;
        replaceCheckbox.addEventListener("change", () => {
          iz().replaceInPlace = !!replaceCheckbox.checked;
          // Default to backup-on whenever the user switches to in-place mode
          // so a misclick can't destroy work silently.
          if (replaceCheckbox.checked && backup) {
            backup.checked = true;
            iz().backup = true;
          }
          syncBackupEnabled();
          invoke?.("save_config", { config: buildCurrentConfig() }).catch(() => {});
        });
      }
      syncBackupEnabled();

      const scanBtn = $("internalize-scan-button");
      if (scanBtn) scanBtn.addEventListener("click", scanExternalRefs);
      const runBtn = $("internalize-run-button");
      if (runBtn) runBtn.addEventListener("click", applyInternalize);

      const clearBtn = $("internalize-clear-button");
      if (clearBtn) clearBtn.addEventListener("click", () => {
        const pkg = iz().selectedPkg;
        const group = pkg ? findGroup(pkg) : null;
        if (!group) return;
        // Clear only the current group's selections, not every selection on
        // the page. The user has a separate way to wipe everything (re-scan).
        for (const r of group.refs || []) {
          delete iz().selectedRefs[refKey(group.source_pkg_id, r.ref_path)];
        }
        updateRunButton();
        renderList();
        renderDetail();
      });
      const selectAllBtn = $("internalize-select-all-group");
      if (selectAllBtn) selectAllBtn.addEventListener("click", () => {
        const pkg = iz().selectedPkg;
        const group = pkg ? findGroup(pkg) : null;
        if (!group) return;
        for (const r of group.refs || []) {
          iz().selectedRefs[refKey(group.source_pkg_id, r.ref_path)] = true;
        }
        updateRunButton();
        renderList();
        renderDetail();
      });

      const filterInput = $("internalize-list-filter");
      if (filterInput) {
        filterInput.addEventListener("input", () => {
          iz().filter = filterInput.value ?? "";
          iz().listPage = 0;
          renderList();
        });
      }

      const listEl = $("internalize-list");
      if (listEl) {
        listEl.addEventListener("click", (event) => {
          const row = event.target.closest("[data-internalize-pkg]");
          if (!row) return;
          const pkg = row.getAttribute("data-internalize-pkg");
          if (!pkg) return;
          iz().selectedPkg = pkg;
          renderList();
          renderDetail();
        });
      }

      const refsEl = $("internalize-refs");
      if (refsEl) {
        refsEl.addEventListener("change", (event) => {
          const cb = event.target;
          if (!cb || !cb.classList.contains("internalize-ref-check")) return;
          const key = cb.getAttribute("data-ref-key");
          if (!key) return;
          if (cb.checked) {
            iz().selectedRefs[key] = true;
          } else {
            delete iz().selectedRefs[key];
          }
          updateRunButton();
          // Repaint the row's is-checked class without a full detail re-render
          // (would lose the user's open <details> elements).
          const label = cb.closest(".source-row");
          if (label) label.classList.toggle("is-checked", cb.checked);
          // Refresh the left list so the "x/y picked" chip stays in sync.
          renderList();
        });

        // Action-button clicks. Buttons sit inside the row's <label>, so a
        // raw click would normally toggle the checkbox — preventDefault on the
        // label-bound click stops that and stopPropagation keeps it from
        // bubbling to the row-select handler. Mirrors the Missing Resources
        // candidate-action pattern.
        refsEl.addEventListener("click", async (event) => {
          const btn = event.target.closest("[data-iz-action]");
          if (!btn) return;
          event.preventDefault();
          event.stopPropagation();
          const action = btn.getAttribute("data-iz-action");
          const pkg = btn.getAttribute("data-source-pkg") || "";
          const refPath = btn.getAttribute("data-ref-path") || "";
          const sourceVar = btn.getAttribute("data-source-var") || "";
          try {
            if (action === "copy-pkg-id" && pkg) {
              const ok = await copyTextToClipboard(pkg);
              if (ok) addLog(`Copied ${pkg}`);
            } else if (action === "copy-path" && refPath) {
              const ok = await copyTextToClipboard(refPath);
              if (ok) addLog(`Copied ${refPath}`);
            } else if (action === "copy-ref" && pkg && refPath) {
              const full = `${pkg}:/${refPath}`;
              const ok = await copyTextToClipboard(full);
              if (ok) addLog(`Copied ${full}`);
            } else if (action === "open-var-details" && pkg) {
              openCandidatePackageInVarDetails(pkg, sourceVar);
            } else if (action === "show-in-explorer" && sourceVar) {
              await showPackageInExplorer(sourceVar);
            }
          } catch (err) {
            setStatus(`Action failed: ${String(err)}`, true);
          }
        });

        // Right-click any ref row to open a context menu. Same set of actions
        // as the buttons, plus a "Search Resource List by CRC" entry when the
        // ref has a CRC32 (useful for finding other refs to the same bytes).
        refsEl.addEventListener("contextmenu", (event) => {
          const row = event.target.closest(".source-row");
          if (!row) return;
          event.preventDefault();
          hideContextMenu();
          const pkg = row.getAttribute("data-source-pkg") || "";
          const refPath = row.getAttribute("data-ref-path") || "";
          const sourceVar = row.getAttribute("data-source-var") || "";
          const group = pkg ? findGroup(pkg) : null;
          const refData = group ? (group.refs || []).find((r) => r.ref_path === refPath) : null;
          showContextMenu(event.clientX, event.clientY, buildRefMenuItems(pkg, refPath, sourceVar, refData));
        });
      }

      // Right-click on a group row in the left panel — bulk actions that
      // apply at the source-pkg level (Copy id, open VAR Details, show .var
      // in Explorer, select-all-in-pkg).
      const listElForMenu = $("internalize-list");
      if (listElForMenu) {
        listElForMenu.addEventListener("contextmenu", (event) => {
          const row = event.target.closest("[data-internalize-pkg]");
          if (!row) return;
          event.preventDefault();
          hideContextMenu();
          const pkg = row.getAttribute("data-internalize-pkg") || "";
          // Match Overview behavior: right-click first selects the row so any
          // subsequent action (Internalize Selected, etc.) sees the visible
          // detail panel matching the chosen group.
          iz().selectedPkg = pkg;
          renderList();
          renderDetail();
          const group = pkg ? findGroup(pkg) : null;
          showContextMenu(event.clientX, event.clientY, buildGroupMenuItems(group));
        });
      }
    }

    function buildRefMenuItems(pkg, refPath, sourceVar, refData) {
      const items = [];
      if (pkg && refPath) {
        const full = `${pkg}:/${refPath}`;
        items.push({
          label: `Copy ${full}`,
          action: async () => {
            const ok = await copyTextToClipboard(full);
            if (ok) addLog(`Copied ${full}`);
          },
        });
      }
      if (pkg) {
        items.push({
          label: `Copy package id (${pkg})`,
          action: async () => {
            const ok = await copyTextToClipboard(pkg);
            if (ok) addLog(`Copied ${pkg}`);
          },
        });
      }
      if (refPath) {
        items.push({
          label: `Copy path (${refPath})`,
          action: async () => {
            const ok = await copyTextToClipboard(refPath);
            if (ok) addLog(`Copied ${refPath}`);
          },
        });
      }
      // CRC-driven search bridges this ref into the Resource List view so the
      // user can see every other VAR that holds the same bytes — handy when
      // deciding whether to internalize or to keep the source dep around.
      if (refData && Number.isFinite(refData.crc32) && refData.crc32 != null) {
        const hex = refData.crc32.toString(16).padStart(8, "0").toUpperCase();
        items.push({ separator: true });
        if (typeof searchResourceListByCrc === "function") {
          items.push({
            label: `Search Resource List by CRC ${hex}`,
            action: () => searchResourceListByCrc(refData.crc32),
          });
        }
        items.push({
          label: `Copy CRC ${hex}`,
          action: async () => {
            const ok = await copyTextToClipboard(hex);
            if (ok) addLog(`Copied ${hex}`);
          },
        });
      }
      if (pkg) {
        items.push({ separator: true });
        items.push({
          label: "Open source pkg in VAR Details",
          action: () => openCandidatePackageInVarDetails(pkg, sourceVar),
        });
      }
      if (sourceVar) {
        items.push({
          label: "Show source .var in Explorer",
          action: async () => {
            await showPackageInExplorer(sourceVar);
          },
        });
        items.push({
          label: `Copy source .var path`,
          action: async () => {
            const ok = await copyTextToClipboard(sourceVar);
            if (ok) addLog(`Copied ${sourceVar}`);
          },
        });
      }
      // Toggle-pick lives at the bottom — it's a state mutation, not a copy
      // action, so the separator helps the eye distinguish it.
      if (pkg && refPath) {
        const key = refKey(pkg, refPath);
        const isPicked = !!iz().selectedRefs[key];
        items.push({ separator: true });
        items.push({
          label: isPicked ? "Deselect this ref" : "Select this ref",
          action: () => {
            if (isPicked) {
              delete iz().selectedRefs[key];
            } else {
              iz().selectedRefs[key] = true;
            }
            updateRunButton();
            renderList();
            renderDetail();
          },
        });
      }
      return items;
    }

    function buildGroupMenuItems(group) {
      if (!group) return [];
      const items = [];
      items.push({
        label: `Copy package id (${group.source_pkg_id})`,
        action: async () => {
          const ok = await copyTextToClipboard(group.source_pkg_id);
          if (ok) addLog(`Copied ${group.source_pkg_id}`);
        },
      });
      items.push({
        label: "Open source pkg in VAR Details",
        action: () => openCandidatePackageInVarDetails(group.source_pkg_id, group.source_var_path),
      });
      if (group.source_var_path) {
        items.push({
          label: "Show source .var in Explorer",
          action: async () => {
            await showPackageInExplorer(group.source_var_path);
          },
        });
        items.push({
          label: "Copy source .var path",
          action: async () => {
            const ok = await copyTextToClipboard(group.source_var_path);
            if (ok) addLog(`Copied ${group.source_var_path}`);
          },
        });
      }
      const refCount = (group.refs || []).length;
      const pickedCount = countSelectedInGroup(group);
      if (refCount > 0) {
        items.push({ separator: true });
        items.push({
          label: pickedCount === refCount ? "Deselect every ref in this pkg" : `Select every ref in this pkg (${refCount})`,
          action: () => {
            const allSelected = pickedCount === refCount;
            for (const r of group.refs || []) {
              const k = refKey(group.source_pkg_id, r.ref_path);
              if (allSelected) delete iz().selectedRefs[k];
              else iz().selectedRefs[k] = true;
            }
            updateRunButton();
            renderList();
            renderDetail();
          },
        });
      }
      return items;
    }

    bindEvents();

    window.__refreshInternalizeResourcesView = function () {
      // Seed from Overview's currently-active paths if Internalize is fresh.
      if (!iz().inputDir) {
        const overviewInput = $("input-dir");
        if (overviewInput?.value) iz().inputDir = overviewInput.value;
      }
      syncInputsFromState();
      renderVarInfo();
      renderList();
      renderDetail();
      updateRunButton();
      setStatus(iz().status, false);
    };

    renderVarInfo();
    renderList();
    renderDetail();
    updateRunButton();
  })();

  renderLogs();
  renderSummary();
  renderModeControls();
  bindDbFindEvents();
});

// =============================================================================
// Find Duplicates page (sidebar route "db-find") — self-contained renderers,
// helpers, event handlers, and scan/execute flow. Targets only dbf-* DOM ids;
// can be edited without risk to the Overview page (and vice versa). Reads
// from / writes to the shared per-page-isolated `state` object.
// =============================================================================

function dbfGetActiveTargetPackageId() {
  return state.targetPackageId || deriveTargetPackageId();
}

// Build the flat managed-members set from the backend's per-package
// `bundle_index`. Bundle cascade is always on, so this is just an alternative
// view of the same data structure consumed by getBundleManagedMembersSet —
// duplicated here so DBF page rendering can be edited without coupling.
function dbfGetBundleManagedMembersSet() {
  const index = state.scan?.bundle_index;
  if (!index) return null;
  const out = new Set();
  for (const pkgId of Object.keys(index)) {
    const vamMap = index[pkgId] ?? {};
    for (const vamPath of Object.keys(vamMap)) {
      for (const sibling of (vamMap[vamPath] ?? [])) {
        out.add(`${pkgId}|${sibling}`);
      }
    }
  }
  return out;
}

// Scene files (Saves/scene/*.json) are intrinsic to the VAR being analyzed —
// they're not a resource the user would swap for a DB-found replacement, so
// they're excluded from the Duplicate Group Overview on Find Duplicates.
// Mirrors the path rule in src-tauri/src/naming.rs:category_name_for_path.
function dbfIsScenePath(internalPath) {
  if (!internalPath) return false;
  const lower = String(internalPath).toLowerCase();
  return lower.startsWith("saves/scene/") || lower.includes("/saves/scene/");
}

// True if `internalPath` is a bundle parent in the target VAR (has an entry
// in bundle_index whose siblings represent its bundle). Bundle parents are
// always shown — the bundle cascade rewriter handles ref updates for them.
function dbfIsBundleParent(pkgId, internalPath) {
  if (!pkgId || !internalPath) return false;
  const vamMap = state.scan?.bundle_index?.[pkgId];
  return !!(vamMap && vamMap[internalPath]);
}

function dbfGetScopedGroups() {
  const groups = state.scan?.groups ?? [];
  const targetPid = dbfGetActiveTargetPackageId();
  // The only difference between the two modes is the *search scope* that feeds
  // the candidate set — local looks only at the scanned folders, DB also
  // augments with every target resource (its replacement lookup runs lazily on
  // row click). The bundling and ref-lookup eligibility rules below are
  // identical for both so a resource that surfaces in one mode surfaces in the
  // other.
  //
  // Local mode: only the actual cross-package duplicate groups the backend
  // produced. Strip the augmented single-ref rows a prior DB-mode scan added
  // to state.scan.groups (their key starts with "dbfind|"); they exist only to
  // surface DB-replacement candidates and have nothing to contribute locally.
  const base = state.dbfMode === "local"
    ? groups.filter((g) => !String(g.key ?? "").startsWith("dbfind|"))
    : groups;
  const scoped = targetPid
    ? base.filter((g) => g.refs.some((r) => r.package_id === targetPid))
    : base;
  // Collapse bundle members under their .vam/.vmi parent: hide any group
  // whose every ref is a managed sibling (the parent .vam row represents
  // the whole bundle). The .vam itself is never in the managed set so its
  // group still surfaces.
  const managed = dbfGetBundleManagedMembersSet();
  const bundleFiltered = (!managed || managed.size === 0)
    ? scoped
    : scoped.filter((g) =>
        g.refs.some((r) => !managed.has(`${r.package_id}|${r.internal_path}`))
      );
  // Drop scene rows. Use the target ref's path (or refs[0]) as the label —
  // same rule the overview row uses to pick what to display.
  const sceneFiltered = bundleFiltered.filter((g) => {
    const labelRef = (targetPid ? g.refs.find((r) => r.package_id === targetPid) : null) || g.refs[0];
    return !dbfIsScenePath(labelRef?.internal_path);
  });
  // Safety filter: an individual (non-bundle) resource only surfaces if the
  // target VAR textually references it (in some scene .json, .vap, .vaj,
  // etc.). Without a text reference we have no way to redirect the game to
  // the replacement, so removing the file would silently break runtime
  // lookups. Bundle parents bypass this gate — the cascade rewriter handles
  // their refs. If the text-ref set hasn't loaded yet (race during scan),
  // err on the conservative side and show everything until it arrives.
  const textRefs = state.dbfTargetTextRefs;
  if (!textRefs) return sceneFiltered;
  return sceneFiltered.filter((g) => {
    const labelRef = (targetPid ? g.refs.find((r) => r.package_id === targetPid) : null) || g.refs[0];
    if (!labelRef) return false;
    if (dbfIsBundleParent(labelRef.package_id, labelRef.internal_path)) return true;
    return textRefs.has(labelRef.internal_path);
  });
}

function dbfGetFilteredGroups() {
  const groups = [...dbfGetScopedGroups()];
  const targetPid = dbfGetActiveTargetPackageId();
  // Use effective_size for sort — for .vam parents the backend rolls bundle
  // siblings (.vab/.vaj/textures) into effective_size, so big bundles sort
  // above their tiny manifest's raw size.
  const sortSizeFor = (group) => {
    const ref = (targetPid && group.refs.find((r) => r.package_id === targetPid))
      ?? group.refs[0];
    return Number(ref?.effective_size ?? ref?.size ?? 0);
  };
  groups.sort((a, b) => {
    const delta = sortSizeFor(b) - sortSizeFor(a);
    if (delta !== 0) return delta;
    return a.refs[0].internal_path.localeCompare(b.refs[0].internal_path);
  });
  const keyword = ($("dbf-group-filter")?.value || "").trim().toLowerCase();
  if (!keyword) return groups;
  return groups.filter((g) =>
    [g.key, ...g.package_ids, ...g.refs.map((r) => r.internal_path)]
      .join(" ").toLowerCase().includes(keyword)
  );
}

function dbfGetSelectedGroup() {
  return dbfGetFilteredGroups().find((g) => g.key === state.selectedKey) ?? null;
}

function dbfGetKeepValue(group) {
  const targetPid = dbfGetActiveTargetPackageId();
  const targetRef = group.refs.find((r) => r.package_id === targetPid) ?? group.refs[0];
  const rawValue = state.keepMap[group.key];
  if (rawValue == null || rawValue === KEEP_ALL_VALUE) {
    return getRefValue(targetRef);
  }
  return rawValue;
}

function dbfRenderModeControls() {
  const busy = Boolean(state.activeTask);
  const tvi = $("dbf-target-var-path");
  if (tvi) tvi.disabled = busy;
  const tvp = $("dbf-pick-target-var-button");
  if (tvp) tvp.disabled = busy;
  const sbtn = $("dbf-scan-button");
  if (sbtn) sbtn.disabled = busy;
  const rbtn = $("dbf-run-button");
  if (rbtn) rbtn.disabled = busy || !state.scan;
  const pickIn = $("dbf-pick-input-button"); if (pickIn) pickIn.disabled = busy;
  // Output and VAP picker enables are owned by dbfSyncReplaceOptions so the
  // Replace toggle drives them. Re-run that here whenever mode controls
  // refresh so a busy-state change folds in the latest Replace setting.
  dbfSyncReplaceOptions();
}

// Mirrors syncReplaceOptions for the Find Duplicates page:
//   - Replace off: backup is forced off and locked; output folder is editable.
//   - Replace on:  backup unlocks (user choice); when backup is on we still
//     need the output dir for the backup zip; when backup is off the output
//     dir is meaningless so we disable it. Output picker tracks the same.
function dbfSyncReplaceOptions() {
  const busy = Boolean(state.activeTask);
  const replaceEl = $("dbf-replace-in-place");
  const backupEl = $("dbf-backup-changed");
  const procVapEl = $("dbf-process-vap");
  if (!replaceEl || !backupEl) return;
  const replace = replaceEl.checked;
  if (!replace) backupEl.checked = false;
  const backup = backupEl.checked;
  const enableOutput = !replace || backup;
  const outEl = $("dbf-output-dir");
  const pickOut = $("dbf-pick-output-button");
  const openOut = $("dbf-open-output-button");
  const vapEl = $("dbf-vap-dir");
  const pickVap = $("dbf-pick-vap-button");
  const warn = $("dbf-replace-warning");
  if (outEl) outEl.disabled = !enableOutput || busy;
  if (pickOut) pickOut.disabled = !enableOutput || busy;
  if (openOut) openOut.disabled = busy || !(replace ? state.targetVarPath : (outEl?.value || "").trim());
  backupEl.disabled = !replace || busy;
  const procVap = !!procVapEl?.checked;
  if (vapEl) vapEl.disabled = busy || !procVap;
  if (pickVap) pickVap.disabled = busy || !procVap;
  if (procVapEl) procVapEl.disabled = busy;
  if (warn) warn.classList.toggle("hidden", !replace);
  outEl?.closest(".path-row")?.classList.toggle("field-disabled", !enableOutput);
  vapEl?.closest(".path-row")?.classList.toggle("field-disabled", !procVap);
}

function dbfRenderSummary() {
  const setText = (id, v) => { const el = $(id); if (el) el.textContent = v; };
  // Mirror Overview: always reveal the per-target size cards on Find Duplicates
  // so the user sees Current / Estimated bytes for the picked target VAR.
  $("dbf-stat-current-size-card")?.classList.remove("hidden");
  $("dbf-stat-estimated-size-card")?.classList.remove("hidden");
  if (!state.scan) {
    setText("dbf-stat-packages", "0");
    setText("dbf-stat-groups", "0");
    setText("dbf-stat-space", "0 B");
    setText("dbf-stat-filtered", "0");
    setText("dbf-stat-modified", "0");
    setText("dbf-stat-current-size", "0 B");
    setText("dbf-stat-estimated-size", "0 B");
    setText("dbf-stat-reclaimed-size", "0 B");
    dbfRenderModeControls();
    return;
  }
  const scoped = dbfGetScopedGroups();
  const modified = scoped.filter((g) => {
    const cur = state.keepMap[g.key] ?? getRefValue(g.refs[0]);
    const def = state.defaultKeepMap[g.key] ?? getRefValue(g.refs[0]);
    return cur !== def;
  });
  const currentReclaim = scoped.reduce((sum, g) => sum + getGroupCurrentReclaimableBytes(g), 0);
  const targetPid = dbfGetActiveTargetPackageId();
  const currentPackageBytes = targetPid ? Number(state.scan.package_sizes?.[targetPid] ?? 0) : 0;
  const remaining = Math.max(0, currentPackageBytes - currentReclaim);
  // Aggregate reclaimable total (migrated from the removed Overview page). Sum
  // the same per-group helper the group rows use so the header total agrees
  // with the per-row "Reclaimable" chips in whichever mode is active: DB mode
  // counts the target's own copy once any duplicate exists; local mode uses the
  // scoped max-reclaimable across the duplicate set.
  const theoreticalReclaimable = scoped.reduce(
    (sum, g) => sum + (state.dbfMode === "db"
      ? getGroupDbModeReclaimable(g)
      : getGroupScopedMaxReclaimableBytes(g)),
    0
  );
  setText("dbf-stat-packages", String(state.scan.summary.packages));
  setText("dbf-stat-groups", String(state.scan.summary.duplicate_groups));
  setText("dbf-stat-space", formatBytesLocal(theoreticalReclaimable));
  setText("dbf-stat-filtered", String(dbfGetFilteredGroups().length));
  setText("dbf-stat-modified", String(modified.length));
  setText("dbf-stat-current-size", formatBytesLocal(currentPackageBytes));
  setText("dbf-stat-estimated-size", formatBytesLocal(remaining));
  setText(
    "dbf-stat-reclaimed-size",
    formatBytesLocal(modified.reduce((sum, g) => sum + getGroupCurrentReclaimableBytes(g), 0))
  );
  dbfRenderModeControls();
}

// Set of group keys whose bundle-sibling list is currently expanded in the
// left group list. Transient UI state — not persisted across page navigation.
const _dbfExpandedBundleKeys = new Set();

let _dbfGroupListDelegated = false;
function _ensureDbfGroupListDelegated() {
  if (_dbfGroupListDelegated) return;
  const container = $("dbf-group-list");
  if (!container) return;
  container.addEventListener("click", (event) => {
    const toggle = event.target.closest('[data-action="toggle-bundle"]');
    if (toggle && container.contains(toggle)) {
      event.preventDefault();
      event.stopPropagation();
      const key = toggle.dataset.key;
      if (key) {
        if (_dbfExpandedBundleKeys.has(key)) _dbfExpandedBundleKeys.delete(key);
        else _dbfExpandedBundleKeys.add(key);
        dbfRenderGroups();
      }
      return;
    }
    const row = event.target.closest(".group-row");
    if (!row || !container.contains(row)) return;
    hideContextMenu();
    state.selectedKey = row.dataset.key;
    state.selectedKeys = [state.selectedKey];
    dbfRenderGroups();
    dbfRenderDetail();
  });
  container.addEventListener("contextmenu", (event) => {
    const row = event.target.closest(".group-row");
    if (!row || !container.contains(row)) return;
    event.preventDefault();
    hideContextMenu();
    const key = row.dataset.key;
    state.selectedKey = key;
    state.selectedKeys = [key];
    const group = dbfGetFilteredGroups().find((g) => g.key === key);
    dbfRenderGroups();
    dbfRenderDetail();
    if (!group) return;
    const lookupCrc = getGroupLookupCrc(group);
    if (lookupCrc == null) return;
    const hex = formatCrc32Hex(lookupCrc);
    showContextMenu(event.clientX, event.clientY, [
      {
        label: t("menuSearchByCrc", hex),
        action: () => searchResourceListByCrc(lookupCrc),
      },
      {
        label: t("menuCopyCrc", hex),
        action: async () => {
          const ok = await copyTextToClipboard(hex);
          if (ok) addLog(t("crcCopied", hex));
        },
      },
    ]);
  });
  _dbfGroupListDelegated = true;
}

function dbfRenderGroups() {
  dbfRenderSummary();
  const container = $("dbf-group-list");
  if (!container) return;
  _ensureDbfGroupListDelegated();
  const groups = dbfGetFilteredGroups();
  const totalPages = Math.max(1, Math.ceil(groups.length / GROUP_PAGE_SIZE));
  if (state.groupPage >= totalPages) state.groupPage = totalPages - 1;
  const start = state.groupPage * GROUP_PAGE_SIZE;
  const visibleGroups = groups.slice(start, start + GROUP_PAGE_SIZE);
  if (!visibleGroups.length) {
    container.classList.add("empty");
    container.innerHTML = `<div class="detail-empty">${escapeHtml(t("groupsEmpty"))}</div>`;
    dbfRenderGroupPagination(0);
    return;
  }
  container.classList.remove("empty");
  const targetPid = dbfGetActiveTargetPackageId();
  container.innerHTML = visibleGroups.map((group) => {
    const isSelected = group.key === state.selectedKey ? "active focused" : "";
    const keepValue = dbfGetKeepValue(group);
    const keepRef = group.refs.find((item) => getRefValue(item) === keepValue);
    const keepClass = keepRef?.package_id === targetPid ? "keep-self" : "keep-picked";
    const keepLabel = formatKeepChoice(group, keepValue);
    // In local mode the row reflects only the actual cross-package refs the
    // backend scan found, ignoring any cached DB matches from a prior
    // DB-mode session — otherwise the counts and reclaim chips would lie
    // about what's available in the current mode.
    const sourceCount = state.dbfMode === "db"
      ? getGroupTotalRefCount(group)
      : group.refs.length;
    const reclaimText = state.dbfMode === "db"
      ? formatBytesLocal(getGroupDbModeReclaimable(group))
      : formatBytesLocal(getGroupScopedMaxReclaimableBytes(group));
    const lookupCrc = getGroupLookupCrc(group);
    const cacheEntry = (state.dbfMode === "db" && lookupCrc != null) ? state.dbResourceMatches[lookupCrc] : null;
    const dbStatus = cacheEntry?.status;
    const dbBadge = dbStatus === "loading"
      ? '<span class="group-row-db-status group-row-db-status-loading"><span class="source-loading-spinner" aria-hidden="true"></span>DB</span>'
      : dbStatus === "error"
        ? '<span class="group-row-db-status group-row-db-status-error">DB</span>'
        : "";
    const sizeCell = `<span class="group-row-size">${escapeHtml(formatBytesLocal(getGroupResourceSize(group)))}</span>${dbBadge}`;
    const labelRef = (targetPid ? group.refs.find((r) => r.package_id === targetPid) : null) || group.refs[0];
    // When this row is a .vam/.vmi parent, surface the number of bundled
    // siblings the row stands in for so the user understands they're acting
    // on a multi-file bundle, not just one resource.
    const siblings = labelRef?.package_id
      ? getBundleSiblingsFor(labelRef.package_id, labelRef.internal_path)
      : [];
    const siblingCount = siblings.length;
    const isExpanded = siblingCount > 0 && _dbfExpandedBundleKeys.has(group.key);
    // Extra bytes the bundle siblings contribute beyond the .vam/.vmi
    // manifest. effective_size on a bundle parent is the manifest + every
    // sibling rolled up by the backend (see models.rs::support_paths), so the
    // difference is the additional cost of the siblings alone.
    const bundleExtraBytes = Math.max(
      0,
      Number(labelRef?.effective_size ?? 0) - Number(labelRef?.size ?? 0)
    );
    const bundleSizeLabel = siblingCount > 0
      ? ` · ${formatBytesLocal(bundleExtraBytes)}`
      : "";
    const bundleBadge = siblingCount > 0
      ? `<span class="chip chip-muted chip-bundle-toggle ${isExpanded ? "expanded" : ""}" role="button" tabindex="0" aria-expanded="${isExpanded}" data-action="toggle-bundle" data-key="${escapeAttribute(group.key)}" title="${escapeAttribute(t("bundleToggleHint") || "Show bundled siblings")}"><span class="material-symbols-outlined chip-bundle-caret" aria-hidden="true">${isExpanded ? "expand_more" : "chevron_right"}</span>+${siblingCount} bundled${escapeHtml(bundleSizeLabel)}</span>`
      : "";
    const expandedList = isExpanded
      ? `<div class="group-row-bundle-list" data-key="${escapeAttribute(group.key)}">
          ${siblings
            .map((sib) => `<div class="group-row-bundle-item"><span class="material-symbols-outlined group-row-bundle-item-icon" aria-hidden="true">subdirectory_arrow_right</span><span class="group-row-bundle-item-path">${escapeHtml(sib)}</span></div>`)
            .join("")}
        </div>`
      : "";
    return `
      <button class="group-row ${isSelected} ${isExpanded ? "has-bundle-expanded" : ""}" data-key="${escapeAttribute(group.key)}" type="button">
        <div class="group-row-top">
          <strong>${escapeHtml(labelRef.internal_path)}</strong>
          ${bundleBadge}
          <span class="chip ${keepClass}">${escapeHtml(keepLabel)}</span>
        </div>
        <div class="group-row-meta">
          ${sizeCell}
          <span>${escapeHtml(t("selectedFiles", sourceCount))}</span>
          <span>${escapeHtml(t("reclaimable", reclaimText))}</span>
        </div>
      </button>
      ${expandedList}
    `;
  }).join("");
  // Click + contextmenu are delegated on #dbf-group-list via
  // _ensureDbfGroupListDelegated, so no per-row listeners needed here.
  dbfRenderGroupPagination(groups.length);
}

let _dbfGroupPaginationDelegated = false;
let _dbfLastRenderedGroupCount = 0;
function _ensureDbfGroupPaginationDelegated() {
  if (_dbfGroupPaginationDelegated) return;
  const pagination = $("dbf-group-pagination");
  if (!pagination) return;
  pagination.addEventListener("click", (event) => {
    const button = event.target.closest("button");
    if (!button || !pagination.contains(button)) return;
    if (button.disabled) return;
    const totalPages = Math.max(1, Math.ceil(_dbfLastRenderedGroupCount / GROUP_PAGE_SIZE));
    const action = button.dataset.pageAction;
    if (action === "prev") {
      state.groupPage = Math.max(0, state.groupPage - 1);
    } else if (action === "next") {
      state.groupPage = Math.min(totalPages - 1, state.groupPage + 1);
    } else if (button.dataset.pageJump != null) {
      state.groupPage = Number(button.dataset.pageJump) || 0;
    } else {
      return;
    }
    dbfRenderGroups();
  });
  _dbfGroupPaginationDelegated = true;
}

function dbfRenderGroupPagination(totalGroups) {
  _dbfLastRenderedGroupCount = totalGroups;
  const pagination = $("dbf-group-pagination");
  if (!pagination) return;
  _ensureDbfGroupPaginationDelegated();
  if (totalGroups <= GROUP_PAGE_SIZE) {
    pagination.classList.add("hidden");
    pagination.innerHTML = "";
    return;
  }
  const totalPages = Math.max(1, Math.ceil(totalGroups / GROUP_PAGE_SIZE));
  if (state.groupPage >= totalPages) state.groupPage = totalPages - 1;
  if (state.groupPage < 0) state.groupPage = 0;
  const currentPage = state.groupPage;
  const pages = buildPageNumbers(totalPages, currentPage);

  pagination.classList.remove("hidden");
  pagination.innerHTML = `
    <button class="page-nav-button" data-page-action="prev" type="button" ${currentPage === 0 ? "disabled" : ""}>
      <span class="material-symbols-outlined">chevron_left</span>
      Previous
    </button>
    <div class="page-numbers">
      ${pages
        .map((page) =>
          page === "ellipsis"
            ? `<span class="page-ellipsis">…</span>`
            : `<button class="page-button ${page === currentPage ? "active" : ""}" data-page-jump="${page}" type="button">${page + 1}</button>`
        )
        .join("")}
    </div>
    <button class="page-nav-button" data-page-action="next" type="button" ${currentPage >= totalPages - 1 ? "disabled" : ""}>
      Next
      <span class="material-symbols-outlined">chevron_right</span>
    </button>
  `;
}

function dbfRenderDetail() {
  const group = dbfGetSelectedGroup();
  if (group && state.lastDetailKey !== null && state.lastDetailKey !== group.key) {
    state.dbResourceMatches = {};
  }
  state.lastDetailKey = group?.key ?? null;
  const empty = $("dbf-detail-empty");
  const panel = $("dbf-detail-panel");
  const preview = $("dbf-detail-preview");
  const resetBtn = $("dbf-reset-keep-button");
  const exportBtn = $("dbf-export-resource-button");
  const filteredBtn = $("dbf-apply-filtered-button");
  const autoBtn = $("dbf-auto-target-button");
  const favSrcBtn = $("dbf-fav-source-button");
  if (!group) {
    if (empty) empty.classList.remove("hidden");
    if (panel) panel.classList.add("hidden");
    if (preview) { preview.classList.add("hidden"); preview.innerHTML = ""; }
    // Reset Choices acts on every group, so it stays usable as long as
    // there's a scan to reset — independent of selection.
    if (resetBtn) resetBtn.disabled = !state.scan;
    if (exportBtn) exportBtn.disabled = true;
    if (filteredBtn) filteredBtn.disabled = true;
    if (autoBtn) autoBtn.disabled = true;
    if (favSrcBtn) favSrcBtn.disabled = true;
    return;
  }
  if (empty) empty.classList.add("hidden");
  if (panel) panel.classList.remove("hidden");
  if (resetBtn) resetBtn.disabled = !state.scan;
  const detSub = $("dbf-detail-subtitle");
  if (detSub) detSub.textContent = getDetailSubtitleText();
  const keepTitle = $("dbf-keep-source-title");
  if (keepTitle) keepTitle.textContent = getKeepSourceTitleText();
  const keepValue = dbfGetKeepValue(group);
  const targetPid = dbfGetActiveTargetPackageId();
  const sorted = [...group.refs]
    .filter((r) => r.package_id !== targetPid)
    .filter((r) => !isCreatorBlockedForPackage(r.package_id))
    .sort((a, b) =>
      a.package_id.localeCompare(b.package_id) ||
      a.internal_path.localeCompare(b.internal_path)
    );
  const seen = new Set();
  const sortedRefs = [];
  for (const ref of sorted) {
    if (seen.has(ref.package_id)) {
      if (getRefValue(ref) === keepValue) {
        const idx = sortedRefs.findIndex((r) => r.package_id === ref.package_id);
        if (idx >= 0) sortedRefs[idx] = ref;
      }
      continue;
    }
    seen.add(ref.package_id);
    sortedRefs.push(ref);
  }
  const detHash = $("dbf-detail-hash");
  if (detHash) {
    // group.key from the backend is the raw dedup key "{crc_hex}:{size_bytes}".
    // Render it as the label promises — "CRC32 : human-readable size" — and
    // attach a context menu so the user can grab the CRC or the on-disk path
    // straight from the chip.
    const [rawCrc = "", rawSize = ""] = String(group.key ?? "").split(":");
    const lookupCrc = getGroupLookupCrc(group);
    const crcHex = lookupCrc != null ? formatCrc32Hex(lookupCrc) : rawCrc.toUpperCase();
    const sizeBytes = Number(rawSize) || Number(group.refs?.[0]?.size ?? 0);
    detHash.textContent = `${crcHex} : ${formatBytesLocal(sizeBytes)}`;
    detHash.style.cursor = "context-menu";
    detHash.title = t("detailHashCopyHint");
    detHash.oncontextmenu = (event) => {
      event.preventDefault();
      hideContextMenu();
      const labelRef = (targetPid ? group.refs.find((r) => r.package_id === targetPid) : null) || group.refs[0];
      const packageFile = labelRef ? (state.scan?.package_files?.[labelRef.package_id] ?? "") : "";
      const internalPath = labelRef?.internal_path ?? "";
      const items = [];
      if (crcHex && crcHex !== "—") {
        items.push({
          label: t("menuCopyCrc", crcHex),
          action: async () => {
            const ok = await copyTextToClipboard(crcHex);
            if (ok) addLog(t("crcCopied", crcHex));
          },
        });
        items.push({
          label: t("menuSearchByCrc", crcHex),
          action: () => searchResourceListByCrc(lookupCrc ?? parseInt(rawCrc, 16)),
        });
      }
      if (packageFile) {
        items.push({
          label: t("menuCopyFilePath"),
          action: async () => {
            const ok = await copyTextToClipboard(packageFile);
            if (ok) addLog(`Copied ${packageFile}`);
          },
        });
      }
      if (internalPath) {
        items.push({
          label: t("menuCopyInternalPath"),
          action: async () => {
            const ok = await copyTextToClipboard(internalPath);
            if (ok) addLog(`Copied ${internalPath}`);
          },
        });
      }
      if (items.length) showContextMenu(event.clientX, event.clientY, items);
    };
  }
  const detCount = $("dbf-detail-file-count");
  if (detCount) detCount.textContent = t("selectedFiles", group.refs.length);
  const detSpace = $("dbf-detail-space");
  if (detSpace) detSpace.textContent = t("reclaimable", formatBytesLocal(getGroupScopedMaxReclaimableBytes(group)));

  // Render the "source from local" info row (which target ref we're
  // relocating FROM) into its own panel above the candidate sections.
  const sourceInfoBox = $("dbf-source-info");
  if (sourceInfoBox) {
    const sourceInfoRefs = targetPid ? group.refs.filter((r) => r.package_id === targetPid) : [];
    sourceInfoBox.innerHTML = sourceInfoRefs.map((ref) => {
      const packageFile = state.scan?.package_files?.[ref.package_id] ?? "";
      const siblings = getBundleSiblingsFor(ref.package_id, ref.internal_path);
      const bundleExtraBytes = Math.max(
        0,
        Number(ref?.effective_size ?? 0) - Number(ref?.size ?? 0)
      );
      const bundleSection = siblings.length
        ? `<div class="source-bundle">
            <button type="button" class="source-bundle-summary" data-action="toggle-source-bundle" aria-expanded="false" title="Click to expand the bundled files list">
              <span class="source-bundle-caret" aria-hidden="true">▸</span>
              <span class="source-bundle-summary-text">+${siblings.length} bundled · ${escapeHtml(formatBytesLocal(bundleExtraBytes))}</span>
            </button>
            <div class="source-bundle-list">
              ${siblings
                .map((sib) => `<div class="source-bundle-item" title="${escapeAttribute(sib)}"><span class="source-bundle-item-icon" aria-hidden="true">↳</span><span class="source-bundle-item-path">${escapeHtml(sib)}</span></div>`)
                .join("")}
            </div>
          </div>`
        : "";
      return `
        <div class="source-row source-row-info" data-package-file="${escapeAttribute(packageFile)}" data-package-id="${escapeAttribute(ref.package_id)}" data-source-type="local">
          <div class="source-main">
            <strong>${escapeHtml(t("sourceFromLocalLabel"))}</strong>
            <span>${escapeHtml(ref.internal_path)}</span>
            <span class="source-file-path">${escapeHtml(packageFile)}</span>
            ${bundleSection}
          </div>
        </div>
      `;
    }).join("");
    sourceInfoBox.querySelectorAll(".source-row[data-package-file]").forEach((element) => {
      element.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        const items = [];
        const packageId = element.dataset.packageId || "";
        const packageFile = element.dataset.packageFile || "";
        if (packageId) items.push({ label: t("resourceListContextOpenPackage"), action: () => openSourceRowInVarDetails(packageId, packageFile, "local") });
        if (packageId) items.push({
          label: `Copy package id (${packageId})`,
          action: async () => {
            const ok = await copyTextToClipboard(packageId);
            if (ok) addLog(`Copied ${packageId}`);
          },
        });
        if (packageFile) items.push({ label: t("menuShowInExplorer"), action: () => showPackageInExplorer(packageFile) });
        showContextMenu(event.clientX, event.clientY, items);
      });
    });
    // Toggle the bundle list collapsed/expanded. Pure DOM manipulation so the
    // re-render on group change resets to expanded (matches the inline HTML
    // default of `.source-bundle-expanded`) without persisting transient UI
    // state across navigation. Bound after every innerHTML rewrite — old
    // listeners die with the replaced nodes, so we never accumulate handlers.
    sourceInfoBox.querySelectorAll('[data-action="toggle-source-bundle"]').forEach((toggle) => {
      toggle.addEventListener("click", (event) => {
        event.preventDefault();
        const bundle = toggle.closest(".source-bundle");
        if (!bundle) return;
        const expanded = bundle.classList.toggle("source-bundle-expanded");
        toggle.setAttribute("aria-expanded", String(expanded));
        const caret = toggle.querySelector(".source-bundle-caret");
        if (caret) caret.textContent = expanded ? "▾" : "▸";
      });
    });
  }

  // Local candidates: every non-target same-CRC ref the local scan found.
  const localBox = $("dbf-local-candidates");
  if (localBox) {
    if (!sortedRefs.length) {
      localBox.innerHTML = `<div class="source-row source-row-empty"><div class="source-main"><span>${escapeHtml(t("scanNone"))}</span></div></div>`;
    } else {
      localBox.innerHTML = sortedRefs.map((ref) => {
        const value = `${ref.package_id}:${ref.internal_path}`;
        const packageFile = state.scan?.package_files?.[ref.package_id] ?? "";
        const incomplete = isRefIncomplete(ref);
        const warningHtml = renderIncompleteRefWarning(ref);
        const classes = ["source-row"];
        if (incomplete) classes.push("source-row-incomplete");
        const radioChecked = !incomplete && keepValue === value ? "checked" : "";
        return `
          <label class="${classes.join(" ")}" data-package-file="${escapeAttribute(packageFile)}" data-package-id="${escapeAttribute(ref.package_id)}" data-source-type="local">
            <input type="radio" name="dbf-keep-source" value="${escapeAttribute(value)}" ${radioChecked} ${incomplete ? "disabled" : ""} />
            <div class="source-main">
              <strong>${escapeHtml(formatPackageActionLabel(group, ref))}<span class="source-tag source-tag-local">Local</span></strong>
              <span>${escapeHtml(ref.internal_path)}</span>
              <span class="source-file-path">${escapeHtml(packageFile)}</span>
              ${warningHtml}
            </div>
          </label>
        `;
      }).join("");
    }
    localBox.querySelectorAll('input[name="dbf-keep-source"]').forEach((input) => {
      dbfAttachKeepRadioToggle(input, group.key);
      input.addEventListener("change", () => {
        state.keepMap[group.key] = input.value;
        bumpKeepMutation();
        dbfRenderGroups();
        dbfRenderDetail();
      });
    });
    localBox.querySelectorAll(".source-row[data-package-file]").forEach((element) => {
      element.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        const items = [];
        const packageId = element.dataset.packageId || "";
        const packageFile = element.dataset.packageFile || "";
        const sourceType = element.dataset.sourceType || "local";
        if (packageId) items.push({ label: t("resourceListContextOpenPackage"), action: () => openSourceRowInVarDetails(packageId, packageFile, sourceType) });
        if (packageId) items.push({
          label: `Copy package id (${packageId})`,
          action: async () => {
            const ok = await copyTextToClipboard(packageId);
            if (ok) addLog(`Copied ${packageId}`);
          },
        });
        if (packageFile) items.push({ label: t("menuShowInExplorer"), action: () => showPackageInExplorer(packageFile) });
        const creator = deriveCreatorFromPackageId(packageId);
        if (creator && !_blockedCreators.has(creator)) {
          items.push({
            label: `Disable creator (${creator})`,
            action: () => blockCreatorByPackageId(packageId),
          });
        }
        showContextMenu(event.clientX, event.clientY, items);
      });
    });
  }

  // Database candidates: rendered into a separate container by the lazy
  // dbfAppendDbMatchRows path. Entire DB section (header + list) hides in
  // local mode since we never look up the catalog there.
  const dbBox = $("dbf-db-candidates");
  const dbHeader = document.querySelector(".dbf-db-header");
  const showDb = state.dbfMode === "db";
  if (dbHeader) dbHeader.classList.toggle("hidden", !showDb);
  if (dbBox) {
    dbBox.classList.toggle("hidden", !showDb);
    if (showDb) {
      dbBox.innerHTML = "";
      dbfAppendDbMatchRows(group);
    } else {
      dbBox.innerHTML = "";
    }
  }

  // Preview (jpg/png/vam) for the selected group's target-VAR ref.
  dbfRenderPreviewForGroup(group);

  const exportRef = getActiveExportRef(group);
  if (resetBtn) resetBtn.disabled = !state.scan;
  if (exportBtn) exportBtn.disabled = !exportRef;
  if (filteredBtn) filteredBtn.disabled = !dbfGetFilteredGroups().length;
  if (autoBtn) autoBtn.disabled = !(state.scan?.groups?.length ?? 0);
  if (favSrcBtn) favSrcBtn.disabled = !(state.scan?.groups?.length ?? 0);
}

// Loads the set of internal paths that the target VAR's own text files
// reference (via SELF:/path or <target_pkg>:/path). Cached on state so the
// dbf filter can decide which non-bundle resources are safe to surface as
// dedup candidates. A file the user can't redirect from any scene/.vap is
// not safe to remove: the game would silently fail to find it at runtime.
async function dbfLoadTargetTextRefs(targetVarPath) {
  state.dbfTargetTextRefs = null;
  if (!invoke || !targetVarPath) return;
  try {
    const refs = await invoke("list_target_var_text_refs", { varPath: targetVarPath });
    state.dbfTargetTextRefs = new Set(Array.isArray(refs) ? refs : []);
  } catch (err) {
    addLog(`Clean VARs: text-ref scan failed: ${String(err)}`);
    state.dbfTargetTextRefs = new Set();
  }
}

async function dbfAugmentScanWithAllTargetResources(targetVarPath, targetPackageId) {
  if (!invoke || !state.scan || !targetVarPath || !targetPackageId) return;
  let resources;
  try {
    resources = await invoke("list_var_resources", { varPath: targetVarPath });
  } catch (err) { addLog(String(err)); return; }
  if (!Array.isArray(resources) || resources.length === 0) return;
  const covered = new Set();
  for (const group of state.scan.groups) {
    for (const ref of group.refs) {
      if (ref.package_id === targetPackageId) covered.add(ref.internal_path);
    }
  }
  // Build a path → size lookup so .vam/.vmi parents can roll up their
  // sibling bytes (matches the backend's apply_effective_resource_sizes).
  // Without this, augmented bundle-parent rows would have effective_size ==
  // size and the row's "+N bundled · X B" chip would always read 0 B.
  const resourceSizeByPath = new Map();
  for (const r of resources) resourceSizeByPath.set(r.internal_path, Number(r.size ?? 0));
  const bundleIndexForPkg = state.scan?.bundle_index?.[targetPackageId] ?? {};
  const extraGroups = [];
  for (const r of resources) {
    if (covered.has(r.internal_path)) continue;
    const crcUnsigned = r.crc32 >>> 0;
    const siblings = bundleIndexForPkg[r.internal_path] ?? [];
    let effectiveSize = Number(r.size ?? 0);
    for (const sib of siblings) effectiveSize += Number(resourceSizeByPath.get(sib) ?? 0);
    extraGroups.push({
      key: `dbfind|${targetPackageId}|${r.internal_path}`,
      refs: [{ package_id: targetPackageId, internal_path: r.internal_path, crc32: crcUnsigned, size: r.size, effective_size: effectiveSize }],
      removable_bytes: 0,
      package_ids: [targetPackageId],
    });
  }
  if (!extraGroups.length) return;
  state.scan.groups = state.scan.groups.concat(extraGroups);
  state.scan.summary = { ...state.scan.summary, duplicate_groups: state.scan.groups.length };
  for (const g of extraGroups) {
    const ref = g.refs[0];
    const keepValue = `${ref.package_id}:${ref.internal_path}`;
    state.scan.default_keep_map[g.key] = keepValue;
    state.defaultKeepMap[g.key] = keepValue;
    state.keepMap[g.key] = keepValue;
  }
  if (extraGroups.length) {
    bumpKeepMutation();
    // state.scan.groups was mutated in place — drop stale haystacks from the
    // memo (entries are stored on group objects so old ones survive, but the
    // newly appended extraGroups need their haystacks built once).
    for (const g of extraGroups) buildGroupHaystack(g);
  }
}

function dbfAppendDbMatchRows(group) {
  const crc = getGroupLookupCrc(group);
  if (crc == null) return;
  const targetPid = dbfGetActiveTargetPackageId();
  const entry = state.dbResourceMatches[crc];
  const container = $("dbf-db-candidates");
  if (!container) return;
  if (!entry) { dbfAppendDbLoadingRow(); dbfFetchDbMatchesForGroup(group, crc, targetPid); return; }
  if (entry.status === "loading") return dbfAppendDbLoadingRow();
  if (entry.status === "error") return dbfAppendDbErrorRow(entry.error);
  const localKey = (pid, path) => `${pid}|${path}`;
  const localRefKeys = new Set(group.refs.map((r) => localKey(r.package_id, r.internal_path)));
  let dbRefs = (entry.refs || [])
    .filter((m) => !localRefKeys.has(localKey(m.package_id, m.internal_path)))
    .filter((m) => !isCreatorBlockedForPackage(m.package_id));
  const filterRaw = ($("dbf-source-filter")?.value || "").trim().toLowerCase();
  if (filterRaw) dbRefs = dbRefs.filter((m) => m.package_id.toLowerCase().includes(filterRaw));
  if (!dbRefs.length) {
    container.innerHTML = `<div class="source-row source-row-empty"><div class="source-main"><span>${escapeHtml("No database matches.")}</span></div></div>`;
    return;
  }
  for (const dbRef of dbRefs) dbfAppendDbMatchRow(group, dbRef);
}

function dbfAppendDbLoadingRow() {
  const container = $("dbf-db-candidates");
  if (!container) return;
  const div = document.createElement("div");
  div.className = "source-row source-row-loading";
  div.innerHTML = `<div class="source-main"><strong><span class="source-loading-spinner" aria-hidden="true"></span>Searching database…<span class="source-tag source-tag-db">DB</span></strong></div>`;
  container.appendChild(div);
}

function dbfAppendDbErrorRow(message) {
  const container = $("dbf-db-candidates");
  if (!container) return;
  const div = document.createElement("div");
  div.className = "source-row source-row-error";
  div.innerHTML = `<div class="source-main"><strong>Database lookup failed <span class="source-tag source-tag-db">DB</span></strong><span>${escapeHtml(message || "")}</span></div>`;
  container.appendChild(div);
}

function dbfAppendDbMatchRow(group, dbRef) {
  const container = $("dbf-db-candidates");
  if (!container) return;
  const value = `${dbRef.package_id}:${dbRef.internal_path}`;
  const filePath = dbRef.file_path || "";
  // DB matches for packages that are also in the current scan share the same
  // bundle_missing_index entry, so the warning fires consistently across the
  // local and DB rows. Packages not in the scan have no entry → undefined →
  // treated as complete (no false-positive warning).
  const incomplete = isRefIncomplete(dbRef);
  const warningHtml = renderIncompleteRefWarning(dbRef);
  const checked = !incomplete && dbfGetKeepValue(group) === value ? "checked" : "";
  const label = document.createElement("label");
  const classes = ["source-row", "source-row-db"];
  if (incomplete) classes.push("source-row-incomplete");
  label.className = classes.join(" ");
  label.dataset.packageFile = filePath;
  label.dataset.packageId = dbRef.package_id;
  label.dataset.sourceType = "db";
  label.innerHTML = `
    <input type="radio" name="dbf-keep-source" value="${escapeAttribute(value)}" ${checked} ${incomplete ? "disabled" : ""} />
    <div class="source-main">
      <strong>${escapeHtml(dbRef.package_id)}<span class="source-tag source-tag-db">DB</span></strong>
      <span>${escapeHtml(dbRef.internal_path)}</span>
      <span class="source-file-path">${escapeHtml(filePath)}</span>
      ${warningHtml}
    </div>
  `;
  const radio = label.querySelector('input[name="dbf-keep-source"]');
  if (radio && !incomplete) {
    dbfAttachKeepRadioToggle(radio, group.key);
    radio.addEventListener("change", () => {
      state.keepMap[group.key] = value;
      bumpKeepMutation();
      ensureDbPackageResources(dbRef.package_id);
      dbfRenderGroups();
      dbfRenderDetail();
    });
  }
  label.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    const items = [];
    if (dbRef.package_id) items.push({ label: t("resourceListContextOpenPackage"), action: () => openSourceRowInVarDetails(dbRef.package_id, filePath, "db") });
    if (dbRef.package_id) items.push({
      label: `Copy package id (${dbRef.package_id})`,
      action: async () => {
        const ok = await copyTextToClipboard(dbRef.package_id);
        if (ok) addLog(`Copied ${dbRef.package_id}`);
      },
    });
    if (filePath) items.push({ label: t("menuShowInExplorer"), action: () => showPackageInExplorer(filePath) });
    const creator = deriveCreatorFromPackageId(dbRef.package_id || "");
    if (creator && !_blockedCreators.has(creator)) {
      items.push({
        label: `Disable creator (${creator})`,
        action: () => blockCreatorByPackageId(dbRef.package_id),
      });
    }
    if (items.length) showContextMenu(event.clientX, event.clientY, items);
  });
  container.appendChild(label);
}

// Toggle-off helper for Find Duplicates radios: clicking the already-checked
// row reverts the group's keep choice to its default (the target VAR's own
// ref for augmented groups, or the backend scan's default for cross-local
// groups). Mirrors Overview's attachKeepRadioToggle but writes back the dbf
// default value instead of KEEP_ALL.
function dbfAttachKeepRadioToggle(input, groupKey) {
  const target = input.closest("label") ?? input;
  let preClickChecked = false;
  target.addEventListener("mousedown", () => { preClickChecked = input.checked; });
  input.addEventListener("click", () => {
    if (!preClickChecked) return;
    preClickChecked = false;
    input.checked = false;
    const def = state.defaultKeepMap[groupKey];
    if (def != null) state.keepMap[groupKey] = def;
    else delete state.keepMap[groupKey];
    bumpKeepMutation();
    dbfRenderGroups();
    dbfRenderDetail();
  });
}

// Renders the preview thumbnail/grid into dbf-detail-preview for the selected
// group. Preview always reads from the target VAR's local file (it's the only
// ref with a guaranteed-on-disk path). Reuses the shared preview cache and
// markup helpers so VAM bundle previews look identical to Overview.
function dbfRenderGroupPreview(refs, keepValue) {
  const previewRef = getPreviewRefForGroup(refs, keepValue);
  const slot = $("dbf-detail-preview");
  if (!slot) return;
  if (!previewRef) {
    slot.classList.add("hidden");
    slot.innerHTML = "";
    return;
  }
  const packageFile = state.scan?.package_files?.[previewRef.package_id] ?? "";
  if (!packageFile) {
    slot.classList.add("hidden");
    slot.innerHTML = "";
    return;
  }
  slot.classList.remove("hidden");
  slot.className = "vam-preview detail-preview";
  slot.innerHTML = buildPreviewMarkup(previewRef, packageFile);
  bindPreviewResolution(slot);
  bindPreviewActions(slot);
  if (!state.previewCache[getPreviewCacheKey(previewRef.package_id, previewRef.internal_path)]) {
    void dbfEnsurePreviewLoaded(previewRef, packageFile);
  }
}

async function dbfEnsurePreviewLoaded(ref, packageFile) {
  if (!invoke || !packageFile || !isPreviewablePath(ref.internal_path)) return;
  const key = getPreviewCacheKey(ref.package_id, ref.internal_path);
  const current = state.previewCache[key];
  if (current?.status === "loading" || current?.status === "ready") return;
  state.previewCache[key] = { status: "loading", startedAt: Date.now() };
  const previewSlot = $("dbf-detail-preview");
  if (previewSlot && !previewSlot.classList.contains("hidden")) {
    previewSlot.innerHTML = buildPreviewMarkup(ref, packageFile);
  }
  const tick = setInterval(() => {
    const latest = state.previewCache[key];
    if (!latest || latest.status !== "loading") { clearInterval(tick); return; }
    const grp = dbfGetSelectedGroup();
    if (!grp) { clearInterval(tick); return; }
    dbfRenderPreviewForGroup(grp);
  }, 1000);
  try {
    const data = await Promise.race([
      invoke("get_vam_preview", {
        packageId: ref.package_id,
        packagePath: packageFile,
        vamPath: ref.internal_path,
      }),
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error("Preview request timed out")), 10000)
      ),
    ]);
    state.previewCache[key] = { status: "ready", data, finishedAt: Date.now() };
  } catch (error) {
    state.previewCache[key] = { status: "error", error: String(error), finishedAt: Date.now() };
    addLog(`Preview failed: ${String(error)}`);
  }
  clearInterval(tick);
  const grp = dbfGetSelectedGroup();
  if (grp) dbfRenderPreviewForGroup(grp);
}

// Build the dbf preview from the target VAR's own refs — DB matches don't
// have a local file path the preview command can open. For .vam, the helper
// scans inside the .var for sibling .jpg/.png; for top-level .jpg/.png it
// returns the image itself.
function dbfRenderPreviewForGroup(group) {
  const targetPid = dbfGetActiveTargetPackageId();
  const sourceRefs = targetPid ? group.refs.filter((r) => r.package_id === targetPid) : group.refs;
  dbfRenderGroupPreview(sourceRefs, dbfGetKeepValue(group));
}

async function dbfFetchDbMatchesForGroup(group, crc, targetPid) {
  if (!invoke) { state.dbResourceMatches[crc] = { status: "ready", refs: [] }; return; }
  state.dbResourceMatches[crc] = { status: "loading" };
  try {
    const refs = await invoke("find_resources_by_crc", { crc32: crc, excludePackageId: targetPid || null });
    state.dbResourceMatches[crc] = { status: "ready", refs: Array.isArray(refs) ? refs : [] };
  } catch (err) {
    state.dbResourceMatches[crc] = { status: "error", error: String(err) };
  }
  dbfRenderGroups();
  if (dbfGetSelectedGroup()?.key === group.key) dbfRenderDetail();
}

async function dbfStartScan() {
  const inputDir = ($("dbf-input-dir")?.value || "").trim();
  const outputDir = ($("dbf-output-dir")?.value || "").trim();
  const targetVarPath = ($("dbf-target-var-path")?.value || "").trim();
  if (!inputDir || !outputDir) { addLog(t("missingPath")); return; }
  if (!targetVarPath) { addLog(t("targetVarRequired")); return; }
  if (!invoke) throw new Error("Tauri runtime is unavailable.");
  state.targetVarPath = targetVarPath;
  state.targetPackageId = deriveTargetPackageIdFromPath(targetVarPath);
  const handle = await invoke("start_scan_task", {
    request: {
      input_dir: inputDir,
      additional_input_dirs: getAdditionalDirs("dbf"),
      target_var_path: targetVarPath,
    },
  });
  state.activeTask = { kind: "scan", id: handle.id, page: "db-find" };
  state.scan = null;
  state.defaultKeepMap = {};
  state.keepMap = {};
  bumpKeepMutation();
  state.previewCache = {};
  state.dbResourceMatches = {};
  state.dbPackageResources = {};
  state.selectedKey = null;
  state.selectedKeys = [];
  state.groupPage = 0;
  dbfRenderModeControls();
  dbfRenderSummary();
  dbfRenderGroups();
  dbfRenderDetail();
  showProgress("scan", 0, "");
  addLog(t("scanStarted"));
  startPollingTask();
}

async function dbfStartExecute() {
  if (!state.scan) { addLog(t("scanFirst")); return; }
  if (!invoke) throw new Error("Tauri runtime is unavailable.");
  const targetVarPath = ($("dbf-target-var-path")?.value || "").trim();
  const targetPackageId = dbfGetActiveTargetPackageId();
  const request = {
    input_dir: ($("dbf-input-dir")?.value || "").trim(),
    additional_input_dirs: getAdditionalDirs("dbf"),
    output_dir: ($("dbf-output-dir")?.value || "").trim(),
    vap_dir: $("dbf-process-vap")?.checked ? (($("dbf-vap-dir")?.value || "").trim() || null) : null,
    target_var_path: targetVarPath || null,
    keep_map: buildEffectiveKeepMap(),
    target_package_id: targetPackageId,
    replace: $("dbf-replace-in-place")?.checked ?? false,
    backup: $("dbf-backup-changed")?.checked ?? true,
  };
  const handle = await invoke("start_execute_task", { request });
  state.activeTask = { kind: "execute", id: handle.id, page: "db-find" };
  dbfRenderSummary();
  showProgress("execute", 0, "");
  addLog(t("runStarted"));
  startPollingTask();
}

async function dbfHandleScanCompletion(payload) {
  state.scan = payload.scan_result;
  buildGroupHaystacks(state.scan?.groups);
  state.groupPage = 0;
  state.defaultKeepMap = { ...payload.scan_result.default_keep_map };
  state.keepMap = { ...payload.scan_result.default_keep_map };
  bumpKeepMutation();
  state.targetPackageId = dbfGetActiveTargetPackageId();
  state.dbfAugmentLoaded = false;
  if (state.targetVarPath) {
    // Harvest the target VAR's own text refs in BOTH modes so the overview's
    // ref-lookup eligibility (dbfGetScopedGroups' text-ref gate) is identical
    // for local and DB scans — a file that isn't textually referenced can't be
    // safely redirected during dedup, regardless of where its duplicate lives.
    await dbfLoadTargetTextRefs(state.targetVarPath);
  }
  if (state.dbfMode === "db" && state.targetPackageId && state.targetVarPath) {
    // DB mode additionally surfaces every target resource as a cleanup
    // candidate; the actual DB match lookup is deferred to row click via
    // dbfAppendDbMatchRows / dbfFetchDbMatchesForGroup.
    await dbfAugmentScanWithAllTargetResources(state.targetVarPath, state.targetPackageId);
    state.dbfAugmentLoaded = true;
  }
  const firstVisible = dbfGetScopedGroups()[0] ?? null;
  state.selectedKey = firstVisible?.key ?? null;
  state.selectedKeys = state.selectedKey ? [state.selectedKey] : [];
  addLog(t("scanSuccess", state.scan.summary.packages, state.scan.summary.duplicate_groups));
  if (!state.scan.groups.length) addLog(t("scanNone"));
  hideProgress();
  dbfRenderSummary();
  dbfRenderGroups();
  dbfRenderDetail();
}

function bindDbFindEvents() {
  const sbtn = $("dbf-scan-button");
  if (sbtn) sbtn.addEventListener("click", async () => {
    try { await dbfStartScan(); }
    catch (e) { addLog(String(e)); hideProgress(); await clearActiveTask(); }
  });
  const rbtn = $("dbf-run-button");
  if (rbtn) rbtn.addEventListener("click", async () => {
    try { await dbfStartExecute(); }
    catch (e) { addLog(String(e)); hideProgress(); await clearActiveTask(); }
  });
  // Persist dbf path edits so they survive an app restart. Saves are
  // fire-and-forget; we don't block typing on the disk write.
  const persistDbfConfig = () => {
    if (!invoke) return;
    invoke("save_config", { config: buildCurrentConfig() }).catch(() => {});
  };
  // Enable the "Open in VAR Details" link only when the target is a .var —
  // mirrors VAR Details' "Clean VARs" hand-off in the other direction.
  const syncOpenVarDetailsButton = () => {
    const btn = $("dbf-open-var-details-button");
    if (!btn) return;
    const v = (state.targetVarPath || $("dbf-target-var-path")?.value || "").trim();
    btn.disabled = !/\.var$/i.test(v);
  };
  const tvi = $("dbf-target-var-path");
  if (tvi) tvi.addEventListener("input", () => {
    const v = tvi.value.trim();
    state.targetVarPath = v;
    state.targetPackageId = deriveTargetPackageIdFromPath(v);
    dbfRenderModeControls();
    dbfRenderSummary();
    dbfRenderGroups();
    dbfRenderDetail();
    dbfUpdateVarInfo();
    syncOpenVarDetailsButton();
    persistDbfConfig();
  });
  const tvp = $("dbf-pick-target-var-button");
  if (tvp) tvp.addEventListener("click", async () => {
    if (!invoke) return;
    const p = await invoke("pick_var_file");
    if (p) { $("dbf-target-var-path").value = p; $("dbf-target-var-path").dispatchEvent(new Event("input")); }
  });
  const openVd = $("dbf-open-var-details-button");
  if (openVd) openVd.addEventListener("click", () => {
    const v = (state.targetVarPath || $("dbf-target-var-path")?.value || "").trim();
    if (!/\.var$/i.test(v)) { addLog(t("targetVarRequired")); return; }
    loadVarDetailsFromPath(v).catch((e) => addLog(`VAR Details: ${String(e)}`));
  });
  syncOpenVarDetailsButton();
  const inp = $("dbf-input-dir");
  if (inp) inp.addEventListener("input", persistDbfConfig);
  const inpPick = $("dbf-pick-input-button");
  if (inpPick) inpPick.addEventListener("click", async () => {
    if (!invoke) return;
    const p = await invoke("pick_folder");
    if (p) { $("dbf-input-dir").value = p; persistDbfConfig(); }
  });
  const out = $("dbf-output-dir");
  if (out) out.addEventListener("input", persistDbfConfig);
  const outPick = $("dbf-pick-output-button");
  if (outPick) outPick.addEventListener("click", async () => {
    if (!invoke) return;
    const p = await invoke("pick_folder");
    if (p) { $("dbf-output-dir").value = p; persistDbfConfig(); }
  });
  const vap = $("dbf-vap-dir");
  if (vap) vap.addEventListener("input", persistDbfConfig);
  const vapPick = $("dbf-pick-vap-button");
  if (vapPick) vapPick.addEventListener("click", async () => {
    if (!invoke) return;
    const p = await invoke("pick_folder");
    if (p) { $("dbf-vap-dir").value = p; persistDbfConfig(); }
  });
  const openOut = $("dbf-open-output-button");
  if (openOut) openOut.addEventListener("click", async () => {
    const replace = $("dbf-replace-in-place")?.checked;
    const path = replace ? state.targetVarPath : ($("dbf-output-dir")?.value || "").trim();
    if (path && invoke) { try { await invoke("show_in_explorer", { path }); } catch (e) { addLog(String(e)); } }
  });
  // Replace / backup / process-vap toggles: identical UX to Overview —
  // backup is locked off when Replace is off, unlocks when Replace is on,
  // and the output-dir field gates on the (replace, backup) combo.
  const replaceCb = $("dbf-replace-in-place");
  if (replaceCb) replaceCb.addEventListener("change", () => {
    if (replaceCb.checked) {
      const backupCb = $("dbf-backup-changed");
      if (backupCb) backupCb.checked = true;
    }
    dbfSyncReplaceOptions();
    dbfRenderSummary();
  });
  const backupCb = $("dbf-backup-changed");
  if (backupCb) backupCb.addEventListener("change", () => {
    dbfSyncReplaceOptions();
    dbfRenderSummary();
  });
  const procVapCb = $("dbf-process-vap");
  if (procVapCb) procVapCb.addEventListener("change", () => {
    dbfSyncReplaceOptions();
  });

  // Mode toggle: lazy augment on local→DB switch, immediate re-filter on
  // DB→local. Persist the choice so the next launch starts in the same mode.
  for (const radio of document.querySelectorAll('input[type="radio"][name="dbf-mode"]')) {
    radio.addEventListener("change", async () => {
      if (!radio.checked) return;
      state.dbfMode = radio.value;
      persistDbfConfig();
      // Text refs drive the ref-lookup gate in both modes; load them if a scan
      // exists but they haven't been harvested yet (e.g. a scan that predates
      // this session's first mode interaction).
      if (state.scan && state.targetVarPath && !state.dbfTargetTextRefs) {
        await dbfLoadTargetTextRefs(state.targetVarPath);
      }
      if (
        state.dbfMode === "db" &&
        state.scan &&
        !state.dbfAugmentLoaded &&
        state.targetVarPath &&
        state.targetPackageId
      ) {
        await dbfAugmentScanWithAllTargetResources(state.targetVarPath, state.targetPackageId);
        state.dbfAugmentLoaded = true;
      }
      dbfRenderSummary();
      dbfRenderGroups();
      dbfRenderDetail();
    });
  }

  const gf = $("dbf-group-filter");
  if (gf) {
    const _dbfRenderGroupsAndDetailDebounced = debounce(() => {
      dbfRenderGroups();
      // Mirror of the Overview group-filter handler: detail panel owns the
      // Apply-to-{scope,filtered,global} buttons' disable state and won't
      // refresh on its own when the filter narrows out the prior selection
      // and `normalizeSelections` flips `state.selectedKey`.
      dbfRenderDetail();
    }, 100);
    gf.addEventListener("input", () => {
      hideContextMenu();
      state.groupPage = 0;
      _dbfRenderGroupsAndDetailDebounced();
    });
  }
  const sf = $("dbf-source-filter");
  if (sf) sf.addEventListener("input", () => {
    const container = $("dbf-db-candidates");
    if (!container) return;
    container.innerHTML = "";
    const group = dbfGetSelectedGroup();
    if (group) dbfAppendDbMatchRows(group);
  });
  const resetBtn = $("dbf-reset-keep-button");
  if (resetBtn) resetBtn.addEventListener("click", () => {
    if (!state.scan) return;
    state.keepMap = { ...state.defaultKeepMap };
    bumpKeepMutation();
    dbfRenderGroups();
    dbfRenderDetail();
  });
  const autoBtn = $("dbf-auto-target-button");
  if (autoBtn) autoBtn.addEventListener("click", async () => {
    try { await applyAutoTargetToScopedGroups(); }
    catch (e) { addLog(String(e)); }
    dbfRenderGroups();
    dbfRenderDetail();
  });
  const favSrcBtn = $("dbf-fav-source-button");
  if (favSrcBtn) favSrcBtn.addEventListener("click", async () => {
    try { await applyFavoriteSourceToScopedGroups(); }
    catch (e) { addLog(String(e)); }
    dbfRenderGroups();
    dbfRenderDetail();
  });
  const filteredBtn = $("dbf-apply-filtered-button");
  if (filteredBtn) filteredBtn.addEventListener("click", () => {
    const group = dbfGetSelectedGroup();
    if (!group) return;
    const selectedPid = (state.keepMap[group.key] ?? "").split(":")[0];
    if (!selectedPid) return;
    const filteredKeys = dbfGetFilteredGroups().map((g) => g.key);
    const dbPkg = state.dbPackageResources[selectedPid];
    if (dbPkg?.status === "ready" && (dbPkg.resourcesByCrc?.size ?? 0) > 0) {
      applyDbKeepToKeys(filteredKeys, selectedPid);
    } else {
      applyKeepToKeys(filteredKeys, selectedPid);
    }
    dbfRenderGroups();
    dbfRenderDetail();
  });
  const exportBtn = $("dbf-export-resource-button");
  if (exportBtn) exportBtn.addEventListener("click", async () => {
    const group = dbfGetSelectedGroup();
    if (!group) return;
    try { await exportCurrentResource(group); }
    catch (e) { addLog(String(e)); }
  });
}

