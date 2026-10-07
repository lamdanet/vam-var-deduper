use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64},
        Arc, Mutex,
    },
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const TEXT_EXTENSIONS: &[&str] = &[".json", ".vam", ".vaj", ".vmi", ".vap"];
pub(crate) const META_PATH: &str = "meta.json";
pub(crate) const KEEP_ALL_VALUE: &str = "__KEEP_ALL__";
pub(crate) const CONFIG_FILE_NAME: &str = "vam_var_deduper.config.json";
pub(crate) const DEFAULT_LANGUAGE: &str = "en_US";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AppConfig {
    pub(crate) language: String,
    pub(crate) theme: String,
    /// The VaM install directory, as in VaM Backstage. `<vam_dir>/AddonPackages`
    /// is the VAR folder every page scans; the per-page folder fields mirror it.
    #[serde(default)]
    pub(crate) vam_dir: Option<String>,
    #[serde(default)]
    pub(crate) input_dir: Option<String>,
    /// Overview scan: extra VAR folders unioned with `input_dir` when scanning.
    /// Overview-only; not mirrored to any other page.
    #[serde(default)]
    pub(crate) scan_additional_dirs: Option<Vec<String>>,
    /// VAR Details "Scan Local": extra VAR folders unioned with the local scan.
    /// Independent from the Overview list.
    #[serde(default)]
    pub(crate) var_details_additional_dirs: Option<Vec<String>>,
    /// VAR Packages page — primary folder scanned in "VAR Folders" mode.
    /// Persisted so the selection survives app restarts.
    #[serde(default)]
    pub(crate) var_packages_input_dir: Option<String>,
    /// VAR Packages page: extra VAR folders unioned with `var_packages_input_dir`.
    /// Independent from the Overview / VAR Details lists.
    #[serde(default)]
    pub(crate) var_packages_additional_dirs: Option<Vec<String>>,
    /// VAR Packages page: when true (the default, and the behavior before this
    /// existed) the scan walks every subfolder of the chosen folders. When false
    /// it lists only the `.var` files sitting directly in them.
    #[serde(default = "default_var_packages_deep_scan")]
    pub(crate) var_packages_deep_scan: bool,
    #[serde(default)]
    pub(crate) output_dir: Option<String>,
    #[serde(default)]
    pub(crate) vap_dir: Option<String>,
    #[serde(default)]
    pub(crate) replace_in_place: bool,
    #[serde(default = "default_backup_changed")]
    pub(crate) backup_changed: bool,
    #[serde(default)]
    pub(crate) process_vap: bool,
    /// Find Duplicates page paths. Persisted independently from the Overview
    /// fields so editing one page's values doesn't affect the other.
    #[serde(default)]
    pub(crate) dbf_input_dir: Option<String>,
    /// Find Duplicates: extra VAR folders unioned with `dbf_input_dir`.
    #[serde(default)]
    pub(crate) dbf_additional_dirs: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) dbf_output_dir: Option<String>,
    #[serde(default)]
    pub(crate) dbf_target_var_path: Option<String>,
    #[serde(default)]
    pub(crate) dbf_vap_dir: Option<String>,
    /// Find Duplicates working mode: "db" (default; augments target resources
    /// and looks up DB candidates on row click) or "local" (Overview-style
    /// view scoped to the target VAR, no DB work). Stored as a string so
    /// older configs without the field default to "db" via the JS-side null
    /// fallback.
    #[serde(default)]
    pub(crate) dbf_mode: Option<String>,
    /// Download VARs page — VAR library folder a dropped/pasted VAR's
    /// dependencies are checked against. Persists across runs; the dropped VARs
    /// themselves are re-picked each session.
    #[serde(default)]
    pub(crate) download_vars_folder: Option<String>,
    /// Download VARs: extra library folders unioned with `download_vars_folder`.
    #[serde(default)]
    pub(crate) download_vars_additional_dirs: Option<Vec<String>>,
    /// Download VARs: where downloaded .var files are written. Blank → falls
    /// back to the primary library folder at download time.
    #[serde(default)]
    pub(crate) download_vars_downloads_folder: Option<String>,
    /// Download VARs: when true, downloads are filed under a per-creator
    /// subfolder of the downloads folder (e.g. `<downloads>/qing/qing.hgf1.1.var`).
    #[serde(default)]
    pub(crate) download_vars_organize_by_creator: bool,
    /// Internalize Resources page paths. The target VAR is intentionally NOT
    /// persisted — same convention as Missing Resources / Find Duplicates.
    #[serde(default)]
    pub(crate) internalize_input_dir: Option<String>,
    /// Missing Resources: extra VAR folders unioned with AddonPackages when
    /// scanning for replacements.
    #[serde(default)]
    pub(crate) missing_additional_dirs: Option<Vec<String>>,
    /// Internalize Resources: extra VAR folders unioned with `internalize_input_dir`.
    #[serde(default)]
    pub(crate) internalize_additional_dirs: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) internalize_output_dir: Option<String>,
    #[serde(default)]
    pub(crate) internalize_replace_in_place: bool,
    #[serde(default = "default_backup_changed")]
    pub(crate) internalize_backup: bool,
    /// VAR Packages "Collect Dependencies": the folders outside the library that
    /// are searched for a package's dependencies. `Some` (even empty) means the
    /// modal's "Remember these folders" box is ticked; `None` means it is not,
    /// and the folders picked in a session are forgotten on exit.
    #[serde(default)]
    pub(crate) dep_source_dirs: Option<Vec<String>>,
    /// Where Offload moves packages so VaM stops loading them. `None` means the
    /// default, `<vam_dir>/AddonPackages_offload` beside AddonPackages.
    #[serde(default)]
    pub(crate) offload_dir: Option<String>,
    /// Offload and Restore file each package under a creator folder at the
    /// destination. Off keeps the path it had relative to the folder it left.
    #[serde(default = "default_offload_by_creator")]
    pub(crate) offload_by_creator: bool,
}

fn default_offload_by_creator() -> bool {
    true
}

fn default_backup_changed() -> bool {
    true
}

/// A bare `#[serde(default)]` on a bool yields `false`, which would silently
/// switch every existing config to a top-level-only scan. Deep is the behavior
/// this page has always had, so it stays the default.
fn default_var_packages_deep_scan() -> bool {
    true
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            language: DEFAULT_LANGUAGE.to_string(),
            theme: "light".to_string(),
            vam_dir: None,
            input_dir: None,
            scan_additional_dirs: None,
            var_details_additional_dirs: None,
            var_packages_input_dir: None,
            var_packages_additional_dirs: None,
            var_packages_deep_scan: true,
            output_dir: None,
            vap_dir: None,
            replace_in_place: false,
            backup_changed: true,
            process_vap: false,
            dbf_input_dir: None,
            dbf_additional_dirs: None,
            dbf_output_dir: None,
            dbf_target_var_path: None,
            dbf_vap_dir: None,
            dbf_mode: None,
            download_vars_folder: None,
            download_vars_additional_dirs: None,
            download_vars_downloads_folder: None,
            download_vars_organize_by_creator: false,
            internalize_input_dir: None,
            missing_additional_dirs: None,
            internalize_additional_dirs: None,
            internalize_output_dir: None,
            internalize_replace_in_place: false,
            internalize_backup: true,
            dep_source_dirs: None,
            offload_dir: None,
            offload_by_creator: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ResourceRef {
    pub(crate) package_id: String,
    pub(crate) package_file: String,
    pub(crate) internal_path: String,
    #[serde(default)]
    pub(crate) crc32: Option<u32>,
    pub(crate) size: u64,
    pub(crate) effective_size: u64,
}

impl ResourceRef {
    pub(crate) fn display_name(&self) -> String {
        format!("{}:{}", self.package_id, self.internal_path)
    }

    pub(crate) fn package_ref(&self) -> String {
        format!("{}:/{}", self.package_id, self.internal_path)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DuplicateGroup {
    pub(crate) key: String,
    pub(crate) refs: Vec<ResourceRef>,
    pub(crate) removable_bytes: u64,
    pub(crate) package_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ScanResourceRef {
    pub(crate) package_id: String,
    pub(crate) internal_path: String,
    #[serde(default)]
    pub(crate) crc32: Option<u32>,
    pub(crate) size: u64,
    pub(crate) effective_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ScanDuplicateGroup {
    pub(crate) key: String,
    pub(crate) refs: Vec<ScanResourceRef>,
    pub(crate) removable_bytes: u64,
    pub(crate) package_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ExecutionStats {
    pub(crate) scanned_packages: usize,
    pub(crate) duplicate_groups: usize,
    pub(crate) changed_packages: usize,
    pub(crate) removed_files: usize,
    pub(crate) reclaimed_bytes: u64,
    pub(crate) vap_files_scanned: usize,
    pub(crate) vap_files_rewritten: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ScanSummary {
    pub(crate) packages: usize,
    pub(crate) duplicate_groups: usize,
    pub(crate) reclaimable_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ScanInfo {
    pub(crate) packages_persisted: usize,
    pub(crate) resources_persisted: usize,
    pub(crate) packages_pruned: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ScanResponse {
    pub(crate) summary: ScanSummary,
    pub(crate) groups: Vec<ScanDuplicateGroup>,
    pub(crate) package_files: BTreeMap<String, String>,
    pub(crate) package_sizes: BTreeMap<String, u64>,
    pub(crate) default_keep_map: BTreeMap<String, String>,
    pub(crate) warnings: Vec<String>,
    #[serde(default)]
    pub(crate) info: ScanInfo,
    /// Per-package bundle index: `package_id → vam_path → {sibling paths}`.
    /// Mirrors each `PreparedPackage.support_paths`; lets the UI hide bundle
    /// members in Everything mode so the dedup decision only happens at the
    /// `.vam` row. Sibling set typically includes the `.vaj`, the same-stem
    /// `.vab/.jpg/.png/.jpeg`, and any `customTexture_*` paths discovered by
    /// parsing the `.vaj`.
    #[serde(default)]
    pub(crate) bundle_index: BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
    /// Subset of `bundle_index` listing expected-but-MISSING siblings per
    /// bundle parent. A non-empty entry means the package's `.vam`/`.vmi`
    /// claims to bundle siblings that the ZIP doesn't actually contain — so
    /// using that ref as a dedup keep would leave dangling references at
    /// runtime. The UI shows a warning chip and disables keep-selection for
    /// incomplete refs; refs with the ENTIRE bundle missing are dropped from
    /// duplicate groups at scan time (see `scan.rs` filter pass).
    #[serde(default)]
    pub(crate) bundle_missing_index: BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct VamPreviewImage {
    pub(crate) internal_path: String,
    pub(crate) mime_type: String,
    pub(crate) size: u64,
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) data_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct VamPreviewResponse {
    pub(crate) package_id: String,
    pub(crate) vam_path: String,
    pub(crate) images: Vec<VamPreviewImage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExecuteRequest {
    pub(crate) input_dir: String,
    #[serde(default)]
    pub(crate) additional_input_dirs: Vec<String>,
    pub(crate) output_dir: String,
    pub(crate) vap_dir: Option<String>,
    pub(crate) target_var_path: Option<String>,
    pub(crate) keep_map: BTreeMap<String, String>,
    pub(crate) target_package_id: Option<String>,
    pub(crate) replace: bool,
    pub(crate) backup: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ExecuteResponse {
    pub(crate) stats: ExecutionStats,
    pub(crate) report_path: String,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedPackage {
    pub(crate) file_path: PathBuf,
    pub(crate) package_id: String,
    pub(crate) meta: Value,
    pub(crate) content_list: Vec<String>,
    pub(crate) dependencies: Value,
    pub(crate) license_type: String,
    pub(crate) resource_refs: Vec<ResourceRef>,
    /// Forward bundle map: `.vam`/`.vmi` parent → {`.vaj`, `.vab`/`.vmb`,
    /// same-stem image, `.vaj`-referenced texture paths}. Drives the
    /// `effective_size` accumulation so each parent's row reports the full
    /// bundle's bytes, and also feeds the Everything-mode UI bundle hide
    /// rule and the dedup-time cascade.
    pub(crate) support_paths: BTreeMap<String, BTreeSet<String>>,
    /// Inverse of `support_paths`: each sibling → all `.vam`/`.vmi` parents
    /// that pull it in. A sibling with more than one owner is "shared" (e.g.
    /// a texture used by two different clothing items in the same VAR); the
    /// cascade pass treats it as safe to remove only when every owner is
    /// also being removed. Pre-built at scan time so the dedup decision is a
    /// constant-time lookup rather than a re-classification.
    pub(crate) sibling_owners: BTreeMap<String, BTreeSet<String>>,
    pub(crate) removed_paths: BTreeSet<String>,
    pub(crate) required_dependencies: BTreeSet<String>,
    pub(crate) replacement_map: BTreeMap<String, ResourceRef>,
}

#[derive(Debug, Clone)]
pub(crate) struct ScannedData {
    pub(crate) files: Vec<VarFileFingerprint>,
    pub(crate) packages: BTreeMap<String, PreparedPackage>,
    pub(crate) duplicate_groups: Vec<DuplicateGroup>,
    pub(crate) warnings: Vec<String>,
    pub(crate) info: ScanInfo,
}

#[derive(Debug, Clone)]
pub(crate) enum TextEncoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

/// `Default` lets every construction site list only the fields it cares about
/// and finish with `..Default::default()`, so adding a new `*_result` variant
/// is a one-line change here instead of an edit at every site.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ProgressPayload {
    pub(crate) phase: String,
    pub(crate) progress: f64,
    pub(crate) message: String,
    pub(crate) done: bool,
    pub(crate) error: Option<String>,
    pub(crate) scan_result: Option<ScanResponse>,
    pub(crate) execute_result: Option<ExecuteResponse>,
    pub(crate) bulk_import_result: Option<BulkImportResponse>,
    #[serde(default)]
    pub(crate) db_find_result: Option<DbFindResponse>,
    #[serde(default)]
    pub(crate) backfill_result: Option<BackfillSizesResponse>,
    #[serde(default)]
    pub(crate) reclaim_scan_result: Option<ReclaimScanResponse>,
    #[serde(default)]
    pub(crate) unique_resources_result: Option<UniqueResourcesResponse>,
    #[serde(default)]
    pub(crate) dependency_check_result: Option<DependencyCheckResponse>,
    /// Result payload for `start_delete_dependency_scan_task` (VAR Packages
    /// delete modal, "Scan dependencies" option).
    #[serde(default)]
    pub(crate) delete_dependency_scan_result: Option<DeleteDependencyScanResponse>,
    #[serde(default)]
    pub(crate) fix_report: Option<FixReport>,
    #[serde(default)]
    pub(crate) internalize_report: Option<InternalizeReport>,
    /// Result payload for `start_scan_missing_resources_task`. Background
    /// scan that walks every text payload in the target VAR, harvests
    /// `Pkg:/path` refs, classifies them against the local scan + DB, and
    /// returns the broken set. Lifted out of the synchronous
    /// `scan_missing_resources` command so a large VAR doesn't freeze the UI.
    #[serde(default)]
    pub(crate) missing_resources_result: Option<Vec<BrokenRef>>,
    /// Result payload for `start_analyze_var_dependencies_task` (Download VARs
    /// page). Reading dropped VARs' dependencies + indexing the library is run
    /// as a task so a large library walk reports progress instead of blocking.
    #[serde(default)]
    pub(crate) analyze_var_deps_result: Option<AnalyzeVarDepsResponse>,
    /// Result payload for `start_download_vars_task` (Download VARs page).
    #[serde(default)]
    pub(crate) download_vars_result: Option<DownloadVarsResponse>,
    /// Result payload for `start_import_download_links_task` (Build Database).
    #[serde(default)]
    pub(crate) import_links_result: Option<ImportLinksResult>,
    /// Result payload for `start_resolve_var_source_task` (VAR Details download).
    #[serde(default)]
    pub(crate) var_source_result: Option<VarSourceInfo>,
    /// Result payload for the VAR Packages maintenance tasks — both planners
    /// and the apply share it, since `PackageAction` carries the op.
    #[serde(default)]
    pub(crate) package_op_result: Option<PackageOpResponse>,
    /// Result payload for `start_export_scene_images_task` (VAR Packages).
    #[serde(default)]
    pub(crate) export_scenes_result: Option<ExportScenesResponse>,
    /// Result payload for `start_collect_deps_scan_task` (VAR Packages).
    #[serde(default)]
    pub(crate) collect_deps_scan_result: Option<CollectDepsScanResponse>,
    /// Result payload for `start_collect_deps_copy_task` (VAR Packages).
    #[serde(default)]
    pub(crate) collect_deps_copy_result: Option<CollectDepsCopyResponse>,
    #[serde(default)]
    pub(crate) offload_result: Option<OffloadResponse>,
    #[serde(default)]
    pub(crate) source_scan_result: Option<crate::sources::SourceScanResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskHandle {
    pub(crate) id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ScanTaskRequest {
    pub(crate) input_dir: String,
    #[serde(default)]
    pub(crate) additional_input_dirs: Vec<String>,
    pub(crate) target_var_path: Option<String>,
    #[serde(default)]
    pub(crate) index_only: bool,
    #[serde(default)]
    pub(crate) skip_db: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BulkImportRequest {
    pub(crate) manifest_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BackfillSizesRequest {
    pub(crate) input_dir: String,
    #[serde(default)]
    pub(crate) include_vap: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct BackfillSizesResponse {
    pub(crate) files_scanned: u64,
    pub(crate) crcs_collected: u64,
    pub(crate) rows_updated: u64,
    pub(crate) was_cancelled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct VarResourceEntry {
    pub(crate) internal_path: String,
    pub(crate) crc32: u32,
    pub(crate) size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DbFindRef {
    pub(crate) package_id: String,
    pub(crate) file_path: String,
    pub(crate) internal_path: String,
    pub(crate) size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DbFindGroup {
    pub(crate) crc32: u32,
    pub(crate) crc32_hex: String,
    pub(crate) size: i64,
    pub(crate) source_refs: Vec<DbFindRef>,
    pub(crate) db_matches: Vec<DbFindRef>,
    pub(crate) db_matches_truncated: i64,
    pub(crate) reclaimable_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DbFindResponse {
    pub(crate) mode: String,
    pub(crate) sources_scanned: usize,
    pub(crate) source_resources: usize,
    pub(crate) groups: Vec<DbFindGroup>,
}

/// Request for the Reclaim Space scan. Walks every `.var` under `folder`,
/// aggregates the unique CRC32 set, then queries the DB for packages whose
/// indexed resources cover the largest byte share of that set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReclaimScanRequest {
    pub(crate) folder: String,
    /// Extra VAR folders unioned with `folder` when walking for .var files.
    #[serde(default)]
    pub(crate) additional_folders: Vec<String>,
    #[serde(default)]
    pub(crate) include_vap: bool,
}

/// Single candidate VAR that the user could download to reclaim space —
/// because it already contains some subset of the resources currently
/// duplicated across the user's local VARs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReclaimCandidate {
    pub(crate) package_id: String,
    pub(crate) file_path: String,
    pub(crate) creator_name: Option<String>,
    pub(crate) matched_resource_count: u64,
    pub(crate) matched_bytes: i64,
    pub(crate) coverage_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReclaimScanResponse {
    pub(crate) local_vars_scanned: usize,
    pub(crate) local_unique_resources: usize,
    pub(crate) local_unique_bytes: i64,
    pub(crate) candidates: Vec<ReclaimCandidate>,
}

/// Request for the Unique Resources scan. Local-only: walks every `.var` in
/// `folder`, deduplicates resources by (crc32, size), and returns the unique
/// set with every source VAR that contains each one. Never touches the DB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UniqueResourcesRequest {
    pub(crate) folder: String,
    /// Extra VAR folders unioned with `folder` when walking for .var files.
    #[serde(default)]
    pub(crate) additional_folders: Vec<String>,
    #[serde(default)]
    pub(crate) include_vap: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UniqueResourceSource {
    pub(crate) package_id: String,
    pub(crate) file_path: String,
    pub(crate) internal_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UniqueResource {
    pub(crate) crc32: u32,
    pub(crate) crc32_hex: String,
    pub(crate) representative_path: String,
    pub(crate) unique_size: u64,
    pub(crate) source_count: u64,
    pub(crate) combined_size: u64,
    pub(crate) sources: Vec<UniqueResourceSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct UniqueResourcesResponse {
    pub(crate) vars_scanned: usize,
    pub(crate) unique_resources: usize,
    pub(crate) total_unique_bytes: u64,
    pub(crate) total_combined_bytes: u64,
    pub(crate) reclaimable_bytes: u64,
    pub(crate) resources: Vec<UniqueResource>,
}

/// Request for the Dependency Check scan. Walks every `.var` under `folder`
/// and reports each one whose `meta.json` declares the target as a dependency
/// (or, when `deep_scan` is on, contains `<target_base>.<version>:/path`
/// references in scene/text payload files).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DependencyCheckRequest {
    pub(crate) folder: String,
    /// Extra VAR folders unioned with `folder` when walking for .var files.
    #[serde(default)]
    pub(crate) additional_folders: Vec<String>,
    pub(crate) target_package_id: String,
    #[serde(default)]
    pub(crate) deep_scan: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DependencyDeepRef {
    pub(crate) internal_path: String,
    pub(crate) ref_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DependencyCheckMatch {
    pub(crate) package_id: String,
    pub(crate) file_path: String,
    pub(crate) creator_name: Option<String>,
    pub(crate) size_bytes: u64,
    pub(crate) modified_ms: Option<i64>,
    /// True when at least one key in this package's `meta.json` `dependencies`
    /// resolved to the target's base id.
    pub(crate) meta_match: bool,
    /// The set of `meta.json` dependency keys that matched the target base —
    /// includes any explicit versions and `.latest`.
    pub(crate) meta_match_keys: Vec<String>,
    /// Text refs found inside this package's payload files (scene/vam/vap…)
    /// pointing at the target package. Empty unless `deep_scan` was true.
    pub(crate) deep_refs: Vec<DependencyDeepRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DependencyCheckResponse {
    pub(crate) target_package_id: String,
    pub(crate) target_package_base: String,
    pub(crate) deep_scan_used: bool,
    pub(crate) vars_scanned: usize,
    pub(crate) dependents: Vec<DependencyCheckMatch>,
}

/// Request for the delete-modal "Scan dependencies" task: read the target's
/// own meta.json dependency tree (recursively — VAM nests the transitive
/// closure) and, for each dependency family, list its local .var files and
/// count which OTHER local packages also declare it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DeleteDependencyScanRequest {
    pub(crate) folder: String,
    /// Extra VAR folders unioned with `folder` when walking for .var files.
    #[serde(default)]
    pub(crate) additional_folders: Vec<String>,
    /// Full path of the .var being deleted. A path (not a package_id, unlike
    /// `DependencyCheckRequest`) because this scan must open the target's
    /// archive to read its dependency tree.
    pub(crate) target_var_path: String,
}

/// One local .var belonging to a dependency family. Every version and every
/// duplicate across folders gets its own entry — each is separately deletable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DependencyLocalFile {
    pub(crate) package_id: String,
    pub(crate) file_path: String,
    /// Trailing version segment ("3", "latest") or None.
    pub(crate) version: Option<String>,
    pub(crate) size_bytes: u64,
    pub(crate) modified_ms: Option<i64>,
    /// Has a `<name>.var.disabled` sidecar (VAM's "package off" marker).
    pub(crate) disabled: bool,
}

/// One package (other than the file being deleted) whose top-level meta.json
/// declares this dependency family.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DependencyUser {
    pub(crate) package_id: String,
    /// True when this user is itself in the target's dependency set (by base),
    /// so the UI can compute "effective exclusivity" — a dep used only by
    /// other deps that are also checked for deletion.
    pub(crate) is_target_dependency: bool,
    /// True when this user is another version (or duplicate copy) of the
    /// target's own family — it survives the delete and still needs its deps.
    pub(crate) is_target_family: bool,
}

/// One dependency family of the package being deleted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DeleteDependencyItem {
    /// Version-stripped family, display-cased from the first declaration seen.
    pub(crate) package_base: String,
    pub(crate) creator: Option<String>,
    /// Every id the target's meta declared for this base, e.g.
    /// ["C.D.2", "C.D.latest"]. Preserved verbatim for display.
    pub(crate) declared_ids: Vec<String>,
    /// True when at least one TOP-LEVEL meta key resolves to this base;
    /// false for deps only reachable through the nested (transitive) tree.
    pub(crate) direct: bool,
    /// Local files of this family, newest version first. Empty = not
    /// installed (still reported so the UI can say so).
    pub(crate) local_files: Vec<DependencyLocalFile>,
    /// Other packages declaring this base, deduped by id, capped at
    /// USED_BY_CAP. `used_by_total` is the uncapped count.
    pub(crate) used_by: Vec<DependencyUser>,
    pub(crate) used_by_total: usize,
    /// used_by_total == 0 — the "only this package needs it" badge.
    pub(crate) exclusive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DeleteDependencyScanResponse {
    pub(crate) target_package_id: String,
    pub(crate) target_package_base: String,
    pub(crate) vars_scanned: usize,
    /// Library files whose meta could not be read while counting users. The
    /// counts still stand but may under-count — mirrors `protection_complete`.
    pub(crate) scan_errors: usize,
    /// scan_errors == 0 && !was_cancelled.
    pub(crate) usage_complete: bool,
    pub(crate) was_cancelled: bool,
    /// Ordered: exclusive first, then ascending used_by_total, then base.
    pub(crate) dependencies: Vec<DeleteDependencyItem>,
}

/// Per dropped/selected .var processed by `analyze_var_dependencies`: its own
/// package id, how many dependencies it declared, and a per-file error (e.g.
/// the archive couldn't be opened or had no meta.json) so one bad file doesn't
/// sink the whole batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DownloadDepSource {
    pub(crate) file_path: String,
    pub(crate) package_id: String,
    pub(crate) dependency_count: usize,
    pub(crate) error: Option<String>,
}

/// One dependency package surfaced on the Download VARs page. `status` is
/// "found"/"missing" when a VAR library was supplied to check against, or
/// "unknown" when none was (presence can't be judged without a reference set).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DownloadDepItem {
    /// The dependency id exactly as declared, e.g. "Creator.Pkg.3" or
    /// "Creator.Pkg.latest".
    pub(crate) package_id: String,
    /// Version-stripped family, e.g. "Creator.Pkg".
    pub(crate) package_base: String,
    pub(crate) creator: Option<String>,
    /// Trailing version segment ("3", "latest") or None.
    pub(crate) version: Option<String>,
    /// "found" | "missing" | "unknown".
    pub(crate) status: String,
    /// Package ids of the dropped VAR(s) that declared this dependency.
    pub(crate) source_vars: Vec<String>,
    /// Hub download URL for a missing package, when resolved during analysis.
    #[serde(default)]
    pub(crate) download_url: Option<String>,
    /// Download size in bytes, when resolved.
    #[serde(default)]
    pub(crate) file_size: Option<u64>,
    /// Concrete filename the Hub will serve (e.g. "Author.Package.61.var").
    #[serde(default)]
    pub(crate) filename: Option<String>,
    /// Where the resolved source came from: "hub" | "pixeldrain" | "mediafire".
    /// Drives per-row behaviour (auto-download vs. open-in-browser).
    #[serde(default)]
    pub(crate) source_host: Option<String>,
    /// Full path of the matched local .var for a "found" item, so the UI can pull
    /// its scene preview thumbnail. None for missing/unknown items.
    #[serde(default)]
    pub(crate) local_path: Option<String>,
}

/// One imported download-link row stored in the `download_links` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DownloadLinkRow {
    /// Version-stripped family, lowercased (e.g. "acidbubbles.timeline").
    pub(crate) package_base: String,
    /// Numeric version parsed from the filename, when present.
    pub(crate) version: Option<i64>,
    /// The filename the link points to (e.g. "AcidBubbles.Timeline.291.var").
    pub(crate) filename: String,
    /// "pixeldrain" | "mediafire" | "other".
    pub(crate) host: String,
    pub(crate) url: String,
    /// Download size in bytes once known. Imported links start without a size
    /// (the mirror lists don't include it); it's filled in after a download so
    /// future analyses can show it. Stored as i64 (SQLite INTEGER).
    pub(crate) size: Option<i64>,
    /// When the link is a `.zip`: the `.var`'s path inside it.
    #[serde(default)]
    pub(crate) archive_entry: Option<String>,
    /// The archive's password, when it has one.
    #[serde(default)]
    pub(crate) archive_password: Option<String>,
    /// When a download through this link last worked (unix seconds).
    #[serde(default)]
    pub(crate) last_ok: Option<i64>,
    /// Failed downloads through it since it last worked.
    #[serde(default)]
    pub(crate) fail_count: i64,
    #[serde(default)]
    pub(crate) last_error: Option<String>,
}

impl DownloadLinkRow {
    /// 0 worked (and hasn't failed since), 1 untried, 2 worked once but
    /// failing now, 3 never worked and failing — lower is tried first.
    pub(crate) fn health_rank(&self) -> u8 {
        match (self.last_ok.is_some(), self.fail_count > 0) {
            (true, false) => 0,
            (false, false) => 1,
            (true, true) => 2,
            (false, true) => 3,
        }
    }
}

/// Result of importing download links from a folder or text file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ImportLinksResult {
    pub(crate) files_scanned: usize,
    pub(crate) links_added: usize,
    pub(crate) total_in_db: i64,
}

/// A single VAR's resolved download source, for the "download this VAR" banner
/// on the VAR Details page. `download_url` is None when the package isn't on the
/// Hub or in the imported link DB; `error` is set only when the Hub couldn't be
/// reached (so the UI can tell "not available" apart from "couldn't check").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct VarSourceInfo {
    pub(crate) download_url: Option<String>,
    pub(crate) filename: Option<String>,
    pub(crate) file_size: Option<u64>,
    /// "hub" | "pixeldrain" | "mediafire" | "other".
    pub(crate) host: Option<String>,
    pub(crate) error: Option<String>,
}

/// Result of analyzing dropped/selected VARs for their dependencies on the
/// Download VARs page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AnalyzeVarDepsResponse {
    pub(crate) sources: Vec<DownloadDepSource>,
    /// True when at least one usable library folder was supplied.
    pub(crate) library_used: bool,
    /// Number of .var files seen across the library folders.
    pub(crate) library_var_count: usize,
    pub(crate) dependencies: Vec<DownloadDepItem>,
}

/// One dependency row in the VAR Packages "Collect Dependencies" modal.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CollectDepItem {
    /// The dependency id exactly as declared, e.g. "Creator.Pkg.3".
    pub(crate) package_id: String,
    /// "found" (in the search folders, copyable) | "in_library" | "missing".
    pub(crate) status: String,
    pub(crate) creator: Option<String>,
    /// Trailing version segment ("3", "latest") or None.
    pub(crate) version: Option<String>,
    /// Set when the dependency was discovered in a *found* dependency's own
    /// meta.json rather than the package's: that dependency's id.
    pub(crate) via: Option<String>,
    /// found: the file in the search folders that a copy would take.
    /// in_library: the library file that satisfies it.
    pub(crate) path: Option<String>,
    pub(crate) file_name: Option<String>,
    pub(crate) size: Option<u64>,
    /// found only: false when that exact version was not there and `path` is
    /// the newest version of the same package instead.
    pub(crate) exact: bool,
    /// Anything worth a second line: a version stand-in, a damaged copy skipped.
    pub(crate) note: Option<String>,
}

/// Result of `start_collect_deps_scan_task`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CollectDepsScanResponse {
    pub(crate) package_id: String,
    pub(crate) var_path: String,
    /// The search folders that were actually walked.
    pub(crate) search_dirs: Vec<String>,
    /// `.var` files seen across the search folders.
    pub(crate) search_var_count: usize,
    /// True when at least one usable library folder was supplied.
    pub(crate) library_used: bool,
    pub(crate) library_var_count: usize,
    /// `<library>/<Creator>`, where the package goes. None with `destination_error`.
    pub(crate) creator_dir: Option<String>,
    /// `<library>/<Creator>/deps`, where dependencies are copied.
    pub(crate) deps_dir: Option<String>,
    /// The package already sits in its creator folder, so a copy won't move it.
    pub(crate) package_in_place: bool,
    /// Why nothing can be copied yet (e.g. no library folder in Settings).
    pub(crate) destination_error: Option<String>,
    /// Copying is possible but probably not what the user wants.
    pub(crate) destination_warning: Option<String>,
    pub(crate) items: Vec<CollectDepItem>,
    pub(crate) notes: Vec<String>,
    pub(crate) was_cancelled: bool,
}

/// What happened to one dependency file in a Collect Dependencies copy.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CollectDepCopyResult {
    pub(crate) source_path: String,
    pub(crate) dest_path: String,
    /// "copied" | "exists" | "skipped" | "failed" | "cancelled".
    pub(crate) status: String,
    pub(crate) detail: String,
    pub(crate) bytes: u64,
}

/// Result of `start_collect_deps_copy_task`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CollectDepsCopyResponse {
    /// Where the package is now: its new path when it was moved.
    pub(crate) var_path: String,
    pub(crate) var_moved: bool,
    /// Why the package could not be moved, or which of its sidecars stayed behind.
    pub(crate) var_note: Option<String>,
    pub(crate) creator_dir: String,
    pub(crate) deps_dir: String,
    pub(crate) results: Vec<CollectDepCopyResult>,
    pub(crate) copied: usize,
    pub(crate) failed: usize,
    pub(crate) bytes_copied: u64,
    pub(crate) was_cancelled: bool,
}

/// Outcome of downloading one requested package on the Download VARs page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DownloadVarItem {
    /// The requested dependency id (as listed in the results table).
    pub(crate) package_id: String,
    /// "downloaded" | "exists" | "no_source" | "failed".
    pub(crate) status: String,
    /// Resolved destination filename when known (e.g. "Author.Package.61.var").
    pub(crate) filename: Option<String>,
    pub(crate) error: Option<String>,
}

/// Result payload for `start_download_vars_task`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DownloadVarsResponse {
    pub(crate) items: Vec<DownloadVarItem>,
    pub(crate) downloaded: usize,
    pub(crate) failed: usize,
    pub(crate) no_source: usize,
    /// The folder the files were written to (resolved from the request).
    pub(crate) dest_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DbFindRequest {
    pub(crate) mode: String,
    #[serde(default)]
    pub(crate) target_var_path: Option<String>,
    /// When set, the source CRCs are harvested from the indexed resources for
    /// this package_id instead of opening the .var on disk. Lets the analyzer
    /// run against packages whose stored file_path is stale or removed.
    #[serde(default)]
    pub(crate) source_package_id: Option<String>,
    #[serde(default)]
    pub(crate) input_dir: Option<String>,
    /// Extra VAR folders unioned with `input_dir` in local mode.
    #[serde(default)]
    pub(crate) additional_input_dirs: Vec<String>,
    #[serde(default)]
    pub(crate) include_vap: bool,
    /// When true, the response includes a group for every harvested source
    /// resource — including ones with no matches anywhere. Lets the VAR
    /// Details page show "All" vs "Shared" filters without a second harvest.
    #[serde(default)]
    pub(crate) include_unshared: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct BulkImportResponse {
    pub(crate) lines_total: u64,
    pub(crate) lines_parsed: u64,
    pub(crate) lines_skipped: u64,
    pub(crate) packages_inserted: u64,
    pub(crate) packages_existing: u64,
    pub(crate) resources_inserted: u64,
    pub(crate) resources_skipped_existing: u64,
}

pub(crate) struct AppState {
    pub(crate) next_task_id: AtomicU64,
    pub(crate) tasks: Arc<Mutex<HashMap<u64, ProgressPayload>>>,
    pub(crate) scan_cache: Arc<Mutex<HashMap<String, CachedScan>>>,
    /// Per-task cancel flags. Tasks that opt in register an `AtomicBool`
    /// here at start; the matching `cancel_task` command flips it. Spawned
    /// threads poll the flag at safe points and bail out gracefully.
    pub(crate) cancellations: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
    /// Cached result of the most recent VAR Packages folder scan. Populated
    /// by `list_var_packages` on the initial scan / explicit refresh, then
    /// reused to serve subsequent paginated/filter queries without rescanning
    /// the directory.
    pub(crate) var_packages_folder_cache: Arc<Mutex<Option<VarPackagesFolderCache>>>,
    /// The plan awaiting the user's Apply, kept server-side so the frontend
    /// sends indices instead of paths. Cleared on every apply exit path.
    pub(crate) package_plan: Arc<Mutex<Option<PackagePlan>>>,
    /// Per-archive library info (content type, deps, ...) keyed by file path
    /// and validated by size + mtime. `None` until first loaded from the
    /// `var_info_cache` table, so a restart doesn't re-open every archive.
    pub(crate) var_info_cache: Arc<Mutex<Option<HashMap<String, crate::library::CachedVarInfo>>>>,
    /// Per-volume Recycle Bin probe results, cached for the session. The probe
    /// writes a file and enumerates the bin, so re-running it on every
    /// single-package delete would be needless work — and Recycle Bin
    /// availability is a property of the volume that effectively never changes
    /// while the app is open.
    pub(crate) recycle_support: Arc<Mutex<HashMap<PathBuf, RecycleSupport>>>,
    /// Bumped whenever an apply mutates the library.
    ///
    /// `list_var_packages` reads its cache-hit decision under lock, drops the
    /// lock for a multi-second walk, then blind-writes the result — and the
    /// cache is validated by `cache_key` alone, with no fingerprint. So a walk
    /// that started before an apply could otherwise commit a torn pre-apply
    /// snapshot under the live key and be served forever. The walker snapshots
    /// this counter first and refuses to commit if it moved.
    pub(crate) var_packages_cache_generation: Arc<AtomicU64>,
}

impl AppState {
    pub(crate) fn new() -> Self {
        Self {
            next_task_id: AtomicU64::new(0),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            scan_cache: Arc::new(Mutex::new(HashMap::new())),
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            var_packages_folder_cache: Arc::new(Mutex::new(None)),
            package_plan: Arc::new(Mutex::new(None)),
            var_info_cache: Arc::new(Mutex::new(None)),
            recycle_support: Arc::new(Mutex::new(HashMap::new())),
            var_packages_cache_generation: Arc::new(AtomicU64::new(0)),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct VarPackagesFolderCache {
    /// Composite key of the scanned roots (primary + sorted additional dirs) so
    /// the cache invalidates when the set of folders changes.
    pub(crate) cache_key: String,
    pub(crate) items: Vec<VarPackageListItem>,
    /// Distinct dependency keys that don't resolve exactly in `items`.
    pub(crate) missing_unique: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VarFileFingerprint {
    pub(crate) relative_path: String,
    pub(crate) size: u64,
    pub(crate) modified_ns: u128,
}

#[derive(Debug, Clone)]
pub(crate) struct VarFileEntry {
    pub(crate) path: PathBuf,
    pub(crate) fingerprint: VarFileFingerprint,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct VarFileStats {
    pub(crate) modified_ms: Option<u64>,
    pub(crate) size_bytes: u64,
    pub(crate) scene_image_path: Option<String>,
    pub(crate) scene_image_data: Option<String>,
}

/// Outcome of writing a VAR's preview image next to it as `<var stem>.<ext>`.
#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct SceneImageExportResult {
    /// `"exported"` | `"exists"` | `"no_image"` | `"failed"`.
    pub(crate) status: String,
    /// The written side-car path (present for `"exported"`, and for `"exists"`
    /// the path that already existed).
    pub(crate) output_path: Option<String>,
    /// Error text for `"failed"`.
    pub(crate) detail: Option<String>,
}

/// Batch summary for exporting preview images across a folder of VARs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ExportScenesResponse {
    pub(crate) scanned: u64,
    pub(crate) exported: u64,
    pub(crate) skipped_exists: u64,
    pub(crate) no_image: u64,
    pub(crate) failed: u64,
    pub(crate) was_cancelled: bool,
    /// A sample of failure messages, for the summary.
    pub(crate) notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct VarPackageListItem {
    pub(crate) file_path: String,
    pub(crate) file_name: String,
    pub(crate) package_id: String,
    pub(crate) creator: Option<String>,
    pub(crate) size_bytes: u64,
    pub(crate) modified_ms: Option<u64>,
    pub(crate) indexed: bool,
    // ---- Library fields. Folder mode only: they come from opening the archive
    // (see `library::read_var_pkg_info`) and from the dependency graph across
    // the scanned set. Database-mode rows leave them at their defaults.
    /// Primary content type key (`library::TYPE_KEYS`); empty when unknown.
    pub(crate) pkg_type: String,
    /// User-facing content items (scenes, presets, clothing, hair, ...).
    pub(crate) item_count: u32,
    /// Top-level `meta.json` dependency count.
    pub(crate) dep_count: u32,
    /// Dependencies that no version of resolves inside the scanned folders.
    pub(crate) missing_dep_count: u32,
    /// How many scanned packages declare this one as a dependency.
    pub(crate) used_by_count: u32,
    /// A higher numbered version of the same package sits in the scanned set.
    pub(crate) newer_version: bool,
    /// `<file>.var.disabled` exists beside it — VaM will not load it.
    pub(crate) disabled: bool,
    /// It sits in the offload folder (see `offload`), not AddonPackages.
    pub(crate) offloaded: bool,
    pub(crate) has_scene_image: bool,
    pub(crate) license: Option<String>,
    /// The archive and its meta.json could be read.
    pub(crate) readable: bool,
    /// Top-level dependency keys, kept server-side for the dependency graph
    /// and the details panel; never sent with a page.
    #[serde(skip)]
    pub(crate) deps: Vec<String>,
}

/// Facet counts and totals for the VAR Packages filter panel and status bar.
/// Each facet's counts ignore that facet's own selection (the usual faceted
/// search rule), so picking a type never zeroes the other type rows.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct VarPackageFacets {
    pub(crate) types: BTreeMap<String, u64>,
    pub(crate) statuses: BTreeMap<String, u64>,
    /// Packages outside the offload folder / inside it.
    pub(crate) active: u64,
    pub(crate) offloaded: u64,
    /// Totals over the fully filtered set.
    pub(crate) total_bytes: u64,
    pub(crate) total_items: u64,
    pub(crate) total_deps: u64,
    /// Totals over the whole scanned library, ignoring every filter.
    pub(crate) library_count: u64,
    pub(crate) library_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct VarPackagePage {
    pub(crate) items: Vec<VarPackageListItem>,
    pub(crate) total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) facets: Option<VarPackageFacets>,
}

// ----------------------------------------------------------------------------
// VAR Packages maintenance — Clean Duplicates + Organize by Creator.
//
// Both features are the same shape: walk the roots, decide per file "do X or
// leave it alone". So they are two read-only planners feeding one shared
// executor, and they share these types.
// ----------------------------------------------------------------------------

/// One planned filesystem operation on a whole `.var`. Produced by a planner,
/// rendered by the UI, then consumed by the executor.
///
/// `ProgressPayload` derives `Deserialize`, so every field here must round-trip
/// — hence `modified_ns` is `#[serde(skip)]`: a `u128` does not survive a JS
/// `Number`, and the frontend has no use for it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct PackageAction {
    pub(crate) package_id: String,
    pub(crate) file_path: String,
    /// Destination for `move`; empty for `trash`.
    pub(crate) dest_path: String,
    pub(crate) size_bytes: u64,
    /// `"trash"` | `"move"`.
    pub(crate) op: String,
    /// `"older_version"` | `"duplicate_copy"` | `"organize"`.
    pub(crate) reason: String,
    /// Dedup only: the survivor this file lost to. Empty for organize.
    pub(crate) kept_path: String,
    pub(crate) kept_size_bytes: u64,
    /// Protection explanation at plan time; error text after apply.
    pub(crate) detail: String,
    /// Plan:  `"pending"` | `"protected"` | `"unverified"` | `"blocked"`.
    /// Apply: `"done"` | `"failed"` | `"skipped"`.
    pub(crate) status: String,
    /// Present in the `packages` table at plan time — drives the "database is
    /// stale, rebuild it" banner without re-querying.
    pub(crate) indexed: bool,
    /// Has a `<file_name>.disabled` sidecar next to it.
    pub(crate) disabled: bool,
    /// TOCTOU guard: re-checked at apply so a file that changed since the
    /// preview is skipped rather than acted on.
    #[serde(skip)]
    pub(crate) modified_ns: u128,
}

/// Result of a plan or an apply. One payload serves both features and both
/// phases, because `PackageAction` already carries `op` + `dest_path`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct PackageOpResponse {
    pub(crate) plan_id: u64,
    /// `"clean_duplicates"` | `"organize_by_creator"` | `"apply"`.
    pub(crate) kind: String,
    pub(crate) actions: Vec<PackageAction>,
    pub(crate) scanned: u64,
    pub(crate) unchanged: u64,
    /// Human-readable exclusions/ambiguities, mirroring the `pre_skipped_blocked`
    /// idiom in `scan.rs` — surfaced so a skip is never silent.
    pub(crate) notes: Vec<String>,
    pub(crate) reclaimable_bytes: u64,
    pub(crate) deep_scan_used: bool,
    /// False when any surviving package's refs could not be read, so protection
    /// could not be proven complete.
    pub(crate) protection_complete: bool,
    pub(crate) was_cancelled: bool,
}

/// Package references harvested from one `.var` in a single archive open.
#[derive(Debug, Clone, Default)]
pub(crate) struct VarRefs {
    /// Top-level `meta.json` `dependencies` keys only — deliberately not the
    /// recursive tree, which describes VAM's build-time snapshot rather than
    /// what is actually installed.
    pub(crate) meta_keys: BTreeSet<String>,
    /// `Creator.Pkg.N:/path` ids harvested from text payloads (deep scan only).
    pub(crate) payload_refs: BTreeSet<String>,
}

/// One `.var` on disk, as the planners see it.
#[derive(Debug, Clone)]
pub(crate) struct PackageCandidate {
    pub(crate) path: PathBuf,
    /// The root this file was found under — organize anchors relative to it, so
    /// every rename stays on one volume.
    pub(crate) base_dir: PathBuf,
    /// The file stem. This *is* VAM's package identity.
    pub(crate) package_id: String,
    pub(crate) id_lc: String,
    /// `naming::package_base(package_id)`, lowercased — the dedup family key.
    pub(crate) base_lc: String,
    /// `None` for `.latest`, non-numeric versions, and unreadable files — none
    /// of which may ever win a family.
    pub(crate) version: Option<u64>,
    pub(crate) size: u64,
    pub(crate) modified_ns: u128,
    pub(crate) in_addon_packages: bool,
    pub(crate) disabled: bool,
    pub(crate) indexed: bool,
    /// Whether this package is inside the scan-depth the user selected — i.e.
    /// whether the planners may act on it.
    ///
    /// The walk itself is ALWAYS recursive, even in Normal mode, and
    /// out-of-scope packages are kept here rather than dropped. They are never
    /// removed, moved, or counted, but they still contribute to the dependency
    /// ref index: a scene sitting in a subfolder is loaded by VAM whether or not
    /// the user chose to browse subfolders, so its reference to a top-level
    /// older version must still protect that version from being recycled.
    pub(crate) in_scope: bool,
}

/// A plan held server-side between the preview and the apply. Never serialized:
/// the frontend addresses actions by index, so there are no paths to re-validate
/// and a huge plan never crosses IPC.
///
/// `candidates` + `refs` are retained so apply can recompute the protection
/// fixpoint against what the user actually kept, with zero further IO.
pub(crate) struct PackagePlan {
    pub(crate) plan_id: u64,
    pub(crate) actions: Vec<PackageAction>,
    pub(crate) candidates: Vec<PackageCandidate>,
    /// `Err` is preserved rather than dropped: "could not read" must never be
    /// mistaken for "declares no dependencies".
    pub(crate) refs: HashMap<PathBuf, Result<VarRefs, String>>,
}

/// Whether a volume genuinely sends deletions to the Recycle Bin. Probed, not
/// assumed — `trash::delete` reports success on volumes that permanently delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecycleSupport {
    Supported,
    Unsupported,
}

/// Available filter values for the VAR Packages view, used to populate the
/// Creator dropdown.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct VarPackageFilterOptions {
    pub(crate) creators: Vec<String>,
}

/// Optional filter selections passed alongside `list_var_packages_from_db`.
/// Empty/absent fields mean "no filter" — the SQL builder skips them.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VarPackageFilters {
    /// `"indexed"` (file exists on disk) or `"unindexed"` (missing on disk).
    /// Folder mode also accepts the library statuses `"dependency"`,
    /// `"standalone"`, `"broken"` and `"outdated"` (see `library::status_matches`).
    #[serde(default)]
    pub(crate) status: Option<String>,
    /// `"sm"` (< 100 MB), `"md"` (100 MB–1 GB), or `"lg"` (> 1 GB).
    #[serde(default)]
    pub(crate) size_bucket: Option<String>,
    /// Exact creator name (matched against `creators.name`).
    #[serde(default)]
    pub(crate) creator: Option<String>,
    /// `true` limits results to packages whose id carries the favorite flag
    /// in `package_flags`. Absent/false = no filter.
    #[serde(default)]
    pub(crate) favorite: Option<bool>,
    /// `"with"` (has a `Saves/scene/` preview image) or `"without"`. Resolved
    /// from the `resources` table — a package with no DB rows counts as
    /// "without", since nothing else can tell us cheaply.
    #[serde(default)]
    pub(crate) scene_image: Option<String>,
    /// Folder mode only: a `library::TYPE_KEYS` content type.
    #[serde(default)]
    pub(crate) pkg_type: Option<String>,
    /// Folder mode only: `"active"` (outside the offload folder) or `"offloaded"`.
    #[serde(default)]
    pub(crate) location: Option<String>,
}

/// Single row in the global Resource List page. Joins `resources` with
/// `categories` and `packages` so the UI has everything it needs for one
/// render pass.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ResourceListItem {
    pub(crate) resource_id: i64,
    pub(crate) package_id: String,
    pub(crate) package_file: String,
    pub(crate) internal_path: String,
    pub(crate) crc32: Option<u32>,
    pub(crate) size: u64,
    pub(crate) effective_size: u64,
    pub(crate) category: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ResourceListPage {
    pub(crate) items: Vec<ResourceListItem>,
    pub(crate) total: u64,
}

/// Optional filter selections for `list_resources_from_db`. Empty/absent
/// fields mean "no filter".
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResourceListFilters {
    /// Exact category name (matched against `categories.name`).
    #[serde(default)]
    pub(crate) category: Option<String>,
    /// `"sm"` (< 1 MB), `"md"` (1 MB–100 MB), or `"lg"` (> 100 MB).
    #[serde(default)]
    pub(crate) size_bucket: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ResourceListFilterOptions {
    pub(crate) categories: Vec<String>,
}

/// Single duplicate occurrence returned by `list_resource_duplicates` — every
/// indexed row sharing a given `crc32` so the side panel can list every
/// package containing the resource.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ResourceDuplicateRef {
    pub(crate) package_id: String,
    pub(crate) package_file: String,
    pub(crate) internal_path: String,
    pub(crate) size: u64,
}

/// Paged response for the Missing Resources DB-candidate panel. `has_more`
/// signals whether there's at least one more row beyond what we returned,
/// driving the infinite-scroll fetch in the UI.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PagedDbCandidates {
    pub(crate) items: Vec<ResourceDuplicateRef>,
    pub(crate) has_more: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CachedScan {
    pub(crate) files: Vec<VarFileFingerprint>,
    pub(crate) scanned: ScannedData,
}

/// Categorization of a broken dependency reference on the Missing Resources
/// page. Different kinds reflect different repair paths in the UI.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BrokenKind {
    /// Found inside a payload file (.json/.vam/.vap/...): `Pkg:/path` where
    /// `Pkg` is not on disk at all.
    TextRef,
    /// Listed in the target's `meta.json` `dependencies` but the package is
    /// not on disk and not in the index.
    MetaDependency,
    /// The referenced package exists on disk, but its current `contentList`
    /// no longer holds `path` — typically because the package itself was
    /// deduped against another keeper.
    Transitive,
}

/// One broken reference inside a target VAR. The list is built by
/// `fix_var::scan_target_var_for_broken_refs` and consumed by the Missing
/// Resources page. `Deserialize` is required because `BrokenRef` is now also
/// surfaced through `ProgressPayload.missing_resources_result` and the
/// payload's derive has to round-trip through `serde::Deserialize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BrokenRef {
    pub(crate) kind: BrokenKind,
    pub(crate) ref_pkg: String,
    pub(crate) ref_path: Option<String>,
    pub(crate) expected_crc32: Option<u32>,
    pub(crate) expected_size: Option<u64>,
    pub(crate) source_files_in_target: Vec<String>,
    pub(crate) local_candidates: Vec<BrokenRefCandidate>,
}

/// A local-scan candidate the user can pick to repair a broken ref. Carries
/// the underlying [`ResourceRef`] flattened so the UI sees the same
/// `package_id` / `internal_path` / `size` / `crc32` fields at the top level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BrokenRefCandidate {
    #[serde(flatten)]
    pub(crate) resource: ResourceRef,
}

/// One user-approved fix to apply to a target VAR. `broken_path` is `None`
/// for `MetaDependency` directives that only swap a top-level dep key.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct FixDirective {
    pub(crate) broken_pkg: String,
    #[serde(default)]
    pub(crate) broken_path: Option<String>,
    pub(crate) replacement_pkg: String,
    #[serde(default)]
    pub(crate) replacement_license_type: Option<String>,
    /// Internal path inside the replacement package. When `None` we fall
    /// back to `broken_path`, but candidates whose path differs from the
    /// broken ref MUST set this — otherwise the rewrite produces a fresh
    /// broken reference (correct pkg, wrong path).
    #[serde(default)]
    pub(crate) replacement_path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct FixReport {
    pub(crate) fixes_applied: u32,
    pub(crate) files_rewritten: u32,
    pub(crate) dependencies_added: Vec<String>,
    pub(crate) dependencies_removed: Vec<String>,
    pub(crate) errors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) output_path: Option<String>,
}

/// One external `Pkg:/path` ref the target VAR makes into a *valid* source
/// VAR — i.e. a ref the user could choose to internalize by copying the
/// referenced bundle into the target as SELF. Built by
/// `internalize::scan_target_var_for_external_refs` and rendered on the
/// Internalize Resources page.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExternalRef {
    pub(crate) ref_path: String,
    pub(crate) crc32: u32,
    pub(crate) size: u64,
    /// Includes `ref_path` itself plus every sibling that must come along —
    /// the `.vaj/.vab/.png/.jpg/.jpeg` siblings of a `.vam` and every
    /// `customTexture_*` path inside the `.vaj`s. For a `.vmi` ref, the
    /// matching `.vmb` cache. For other extensions, just `{ref_path}`.
    pub(crate) bundle_paths: Vec<String>,
    pub(crate) bundle_total_size: u64,
    /// Internal paths inside the target VAR whose text payload mentions
    /// `SourcePkg:/ref_path`. Drives the "Referenced in" panel on the right,
    /// same shape as the Missing Resources page.
    pub(crate) source_files_in_target: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExternalRefGroup {
    pub(crate) source_pkg_id: String,
    pub(crate) source_var_path: String,
    pub(crate) source_var_size: u64,
    pub(crate) refs: Vec<ExternalRef>,
    /// Sum of distinct bundle bytes if every ref in the group is internalized.
    /// Bundle members shared across multiple refs (e.g. a texture used by two
    /// different `.vam`s) are counted once.
    pub(crate) total_bundle_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct InternalizeSelection {
    pub(crate) source_pkg_id: String,
    pub(crate) ref_path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct InternalizeReport {
    pub(crate) refs_rewritten: u32,
    pub(crate) entries_copied: u32,
    pub(crate) entries_skipped_collision: Vec<String>,
    pub(crate) dependencies_removed: Vec<String>,
    pub(crate) files_rewritten: u32,
    pub(crate) bytes_added: u64,
    pub(crate) errors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) output_path: Option<String>,
}

/// Outcome of an Offload or Restore run (`offload::start_offload_task`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct OffloadResponse {
    /// `true` for Restore (offload folder -> AddonPackages).
    pub(crate) restore: bool,
    pub(crate) moved: Vec<OffloadMove>,
    pub(crate) failed: Vec<OffloadFailure>,
    pub(crate) moved_bytes: u64,
    pub(crate) was_cancelled: bool,
    /// Sidecars that could not follow their package, and similar asides.
    pub(crate) notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct OffloadMove {
    pub(crate) package_id: String,
    pub(crate) from: String,
    pub(crate) to: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct OffloadFailure {
    pub(crate) package_id: String,
    pub(crate) file_path: String,
    pub(crate) error: String,
}
