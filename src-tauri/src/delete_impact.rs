//! What deleting one package does to the rest of the library, for the delete
//! dialog — read from the dependency graph the VAR Packages listing already
//! holds, so the dialog can show it the moment it opens instead of behind a
//! scan of every archive.
//!
//! Two sides:
//! - **Used by**: packages that depend on it, and what each one falls back to
//!   once it's gone (another version of it, or nothing).
//! - **Dependencies**: everything it pulls in (the whole chain), each with who
//!   else uses it. Users inside the chain are listed by path, so the dialog
//!   can re-work "only used by what you're deleting" as boxes are ticked.

use std::{
    collections::{HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;
use tauri::State;

use crate::{
    db::Db,
    library::{DepResolution, LibIndex},
    models::{AppState, VarPackageListItem},
};

/// Outside users listed per dependency; past this only the count is sent.
const EXTERNAL_CAP: usize = 40;

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ImpactTarget {
    pub(crate) package_id: String,
    pub(crate) file_path: String,
    pub(crate) size_bytes: u64,
    pub(crate) pkg_type: String,
    pub(crate) item_count: u32,
    pub(crate) readable: bool,
    pub(crate) on_hub: Option<bool>,
    pub(crate) hub_resource_id: Option<String>,
    /// It sits in the offload folder, so VaM doesn't load it now.
    pub(crate) offloaded: bool,
    /// Other local versions of the same package, newest first.
    pub(crate) other_versions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ImpactUser {
    pub(crate) package_id: String,
    pub(crate) file_path: String,
    /// The dependency key it lists that resolves to the package.
    pub(crate) dep_key: String,
    /// What that key resolves to once the package is gone, if anything.
    pub(crate) fallback: Option<String>,
    /// The fallback satisfies the key (rather than being another version).
    pub(crate) fallback_exact: bool,
    /// Disabled or offloaded: VaM doesn't load it anyway.
    pub(crate) inactive: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ImpactDep {
    /// The key as the package that pulls it in declares it.
    pub(crate) key: String,
    /// `"found"` | `"other_version"` | `"missing"`.
    pub(crate) status: String,
    pub(crate) package_id: Option<String>,
    pub(crate) file_path: Option<String>,
    pub(crate) size_bytes: u64,
    /// 1 = the package lists it itself; 2+ = pulled in by another dependency.
    pub(crate) depth: u32,
    /// For depth 2+, the dependency that pulls it in.
    pub(crate) via: Option<String>,
    /// The user chose it (`roles`), so it's never suggested for removal.
    pub(crate) installed: bool,
    pub(crate) inactive: bool,
    pub(crate) on_hub: Option<bool>,
    /// Users that are the package being deleted or another dependency in this
    /// list, by file path.
    pub(crate) internal_users: Vec<String>,
    /// Every other user's package id, up to `EXTERNAL_CAP`.
    pub(crate) external_users: Vec<String>,
    pub(crate) external_total: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct DeleteImpact {
    /// Read from the VAR Packages listing (rather than a fresh folder walk).
    pub(crate) from_listing: bool,
    pub(crate) packages_considered: usize,
    pub(crate) target: ImpactTarget,
    pub(crate) used_by: Vec<ImpactUser>,
    pub(crate) dependencies: Vec<ImpactDep>,
}

fn inactive(item: &VarPackageListItem) -> bool {
    item.disabled || item.offloaded
}

fn same_path(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// The key in `item.deps` that resolves to `target`, if any.
fn key_to<'a>(index: &LibIndex, item: &'a VarPackageListItem, target: usize) -> Option<&'a str> {
    item.deps.iter().map(String::as_str).find(|d| {
        matches!(index.resolve(d), DepResolution::Found(t) | DepResolution::OtherVersion(t) if t == target)
    })
}

pub(crate) fn compute(items: &[VarPackageListItem], target: usize) -> DeleteImpact {
    let index = LibIndex::build(items);
    let (_, users) = crate::disable::graph(items);
    let t = &items[target];

    let base_lc = crate::naming::package_base(&t.package_id).to_ascii_lowercase();
    let mut siblings: Vec<&VarPackageListItem> = items
        .iter()
        .enumerate()
        .filter(|(i, it)| {
            *i != target
                && crate::naming::package_base(&it.package_id).eq_ignore_ascii_case(&base_lc)
                && !it.package_id.eq_ignore_ascii_case(&t.package_id)
        })
        .map(|(_, it)| it)
        .collect();
    siblings.sort_by_key(|it| std::cmp::Reverse(crate::naming::package_version(&it.package_id)));
    let mut other_versions: Vec<String> = siblings.iter().map(|it| it.package_id.clone()).collect();
    other_versions.dedup_by(|a, b| a.eq_ignore_ascii_case(b));

    // Used by: who resolves to it now, and to what once it's gone.
    let without = LibIndex::build_where(items, |i| i != target);
    let mut used_by: Vec<ImpactUser> = users[target]
        .iter()
        .map(|&u| {
            let user = &items[u];
            let key = key_to(&index, user, target).unwrap_or_default().to_string();
            let (fallback, exact) = match without.resolve(&key) {
                DepResolution::Found(f) => (Some(items[f].package_id.clone()), true),
                DepResolution::OtherVersion(f) => (Some(items[f].package_id.clone()), false),
                DepResolution::Missing => (None, false),
            };
            ImpactUser {
                package_id: user.package_id.clone(),
                file_path: user.file_path.clone(),
                dep_key: key,
                fallback,
                fallback_exact: exact,
                inactive: inactive(user),
            }
        })
        .collect();
    // Broken first, then those left on another version, then covered ones.
    used_by.sort_by(|a, b| {
        let rank = |u: &ImpactUser| match (&u.fallback, u.fallback_exact) {
            (None, _) => 0,
            (Some(_), false) => 1,
            (Some(_), true) => 2,
        };
        rank(a)
            .cmp(&rank(b))
            .then(a.inactive.cmp(&b.inactive))
            .then_with(|| a.package_id.to_lowercase().cmp(&b.package_id.to_lowercase()))
    });

    // Dependencies: breadth-first, so each one's depth is its shortest chain.
    struct Reached {
        idx: usize,
        key: String,
        exact: bool,
        depth: u32,
        via: Option<usize>,
    }
    let mut reached: Vec<Reached> = Vec::new();
    let mut seen: HashSet<usize> = HashSet::from([target]);
    let mut missing: Vec<ImpactDep> = Vec::new();
    let mut missing_seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(usize, u32)> = VecDeque::from([(target, 0)]);
    while let Some((i, depth)) = queue.pop_front() {
        for key in &items[i].deps {
            match index.resolve(key) {
                DepResolution::Found(d) | DepResolution::OtherVersion(d) if d == i => {}
                res @ (DepResolution::Found(d) | DepResolution::OtherVersion(d)) => {
                    if seen.insert(d) {
                        reached.push(Reached {
                            idx: d,
                            key: key.clone(),
                            exact: matches!(res, DepResolution::Found(_)),
                            depth: depth + 1,
                            via: (i != target).then_some(i),
                        });
                        queue.push_back((d, depth + 1));
                    }
                }
                DepResolution::Missing => {
                    let base = crate::naming::package_base(key).to_ascii_lowercase();
                    if missing_seen.insert(base) {
                        missing.push(ImpactDep {
                            key: key.clone(),
                            status: "missing".to_string(),
                            package_id: None,
                            file_path: None,
                            size_bytes: 0,
                            depth: depth + 1,
                            via: (i != target).then(|| items[i].package_id.clone()),
                            installed: false,
                            inactive: false,
                            on_hub: None,
                            internal_users: Vec::new(),
                            external_users: Vec::new(),
                            external_total: 0,
                        });
                    }
                }
            }
        }
    }
    let chain: HashSet<usize> = seen; // target + every reached dependency
    let mut dependencies: Vec<ImpactDep> = reached
        .iter()
        .map(|r| {
            let item = &items[r.idx];
            let mut internal = Vec::new();
            let mut external = Vec::new();
            let mut external_total = 0u32;
            for &u in &users[r.idx] {
                if chain.contains(&u) {
                    internal.push(items[u].file_path.clone());
                } else {
                    external_total += 1;
                    if external.len() < EXTERNAL_CAP {
                        external.push(items[u].package_id.clone());
                    }
                }
            }
            external.sort_by_key(|id| id.to_lowercase());
            ImpactDep {
                key: r.key.clone(),
                status: if r.exact { "found" } else { "other_version" }.to_string(),
                package_id: Some(item.package_id.clone()),
                file_path: Some(item.file_path.clone()),
                size_bytes: item.size_bytes,
                depth: r.depth,
                via: r.via.map(|v| items[v].package_id.clone()),
                installed: item.installed,
                inactive: inactive(item),
                on_hub: item.on_hub,
                internal_users: internal,
                external_users: external,
                external_total,
            }
        })
        .collect();
    dependencies.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.key.to_lowercase().cmp(&b.key.to_lowercase()))
    });
    missing.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.key.to_lowercase().cmp(&b.key.to_lowercase())));
    dependencies.extend(missing);

    DeleteImpact {
        from_listing: false,
        packages_considered: items.len(),
        target: ImpactTarget {
            package_id: t.package_id.clone(),
            file_path: t.file_path.clone(),
            size_bytes: t.size_bytes,
            pkg_type: t.pkg_type.clone(),
            item_count: t.item_count,
            readable: t.readable,
            on_hub: t.on_hub,
            hub_resource_id: t.hub_resource_id.clone(),
            offloaded: t.offloaded,
            other_versions,
        },
        used_by,
        dependencies,
    }
}

fn find(items: &[VarPackageListItem], file_path: &str) -> Option<usize> {
    items.iter().position(|it| same_path(&it.file_path, file_path))
}

fn item_for(path: &Path, size: u64, info: crate::library::VarPkgInfo) -> VarPackageListItem {
    VarPackageListItem {
        file_path: path.display().to_string(),
        file_name: path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string(),
        package_id: path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string(),
        size_bytes: size,
        pkg_type: info.pkg_type,
        item_count: info.item_count,
        readable: info.readable,
        disabled: crate::packages::disabled_sidecar(path).exists(),
        deps: info.deps,
        ..VarPackageListItem::default()
    }
}

/// The delete dialog's picture of what deleting `file_path` affects. Uses the
/// VAR Packages listing when it holds the package; otherwise lists `roots`
/// (archives already read come from the info cache, so this is quick after the
/// first time).
#[tauri::command(async)]
pub(crate) fn get_delete_impact(
    file_path: String,
    roots: Vec<String>,
    db: State<'_, Db>,
    state: State<'_, AppState>,
) -> Result<DeleteImpact, String> {
    {
        let cache = state
            .var_packages_folder_cache
            .lock()
            .map_err(|_| "library cache poisoned".to_string())?;
        if let Some(c) = cache.as_ref() {
            if let Some(t) = find(&c.items, &file_path) {
                return Ok(DeleteImpact { from_listing: true, ..compute(&c.items, t) });
            }
        }
    }

    let target_path = Path::new(&file_path);
    let target_meta = fs::metadata(target_path).map_err(|_| "The package isn't there any more.".to_string())?;
    let roots: Vec<PathBuf> = roots
        .iter()
        .map(|r| r.trim())
        .filter(|r| !r.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .collect();
    let mut items: Vec<VarPackageListItem> = Vec::new();
    if let Some((first, rest)) = roots.split_first() {
        let roots = crate::scan::scan_roots(first, rest);
        let entries = crate::utils::collect_var_files_multi(&roots).map_err(|e| e.to_string())?;
        let infos = crate::library::infos_for_entries(&entries, &state, &db);
        items = entries
            .iter()
            .zip(infos)
            .map(|(e, info)| item_for(&e.path, e.fingerprint.size, info))
            .collect();
    }
    let target = match find(&items, &file_path) {
        Some(t) => t,
        None => {
            items.push(item_for(target_path, target_meta.len(), crate::library::read_var_pkg_info(target_path)));
            items.len() - 1
        }
    };
    crate::hub_index::annotate(&mut items);
    crate::roles::apply(&mut items, &db);
    Ok(compute(&items, target))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, deps: &[&str]) -> VarPackageListItem {
        VarPackageListItem {
            file_path: format!("/lib/{id}.var"),
            package_id: id.to_string(),
            deps: deps.iter().map(|d| d.to_string()).collect(),
            size_bytes: 10,
            ..VarPackageListItem::default()
        }
    }

    fn dep<'a>(res: &'a DeleteImpact, id: &str) -> &'a ImpactDep {
        res.dependencies
            .iter()
            .find(|d| d.package_id.as_deref() == Some(id) || d.key == id)
            .unwrap_or_else(|| panic!("no row for {id}"))
    }

    #[test]
    fn chain_lists_every_dependency_with_its_depth_and_users() {
        let items = vec![
            item("A.Scene.1", &["A.Look.1", "A.Shared.1", "Gone.Pkg.latest"]),
            item("A.Look.1", &["A.Hair.1"]),
            item("A.Hair.1", &[]),
            item("A.Shared.1", &[]),
            item("B.Other.1", &["A.Shared.1"]),
        ];
        let res = compute(&items, 0);
        assert!(res.used_by.is_empty());

        let look = dep(&res, "A.Look.1");
        assert_eq!((look.depth, look.via.as_deref()), (1, None));
        assert_eq!(look.internal_users, ["/lib/A.Scene.1.var"]);
        assert_eq!(look.external_total, 0);

        let hair = dep(&res, "A.Hair.1");
        assert_eq!((hair.depth, hair.via.as_deref()), (2, Some("A.Look.1")));
        assert_eq!(hair.internal_users, ["/lib/A.Look.1.var"]);

        let shared = dep(&res, "A.Shared.1");
        assert_eq!(shared.external_users, ["B.Other.1"]);
        assert_eq!(shared.external_total, 1);

        let gone = dep(&res, "Gone.Pkg.latest");
        assert_eq!(gone.status, "missing");
        assert_eq!(res.dependencies.last().map(|d| d.status.as_str()), Some("missing"), "missing sort last");
    }

    #[test]
    fn users_report_what_they_fall_back_to() {
        let items = vec![
            item("A.Pkg.2", &[]),
            item("A.Pkg.1", &[]),
            item("U.Latest.1", &["A.Pkg.latest"]),
            item("U.Exact.1", &["A.Pkg.2"]),
            item("U.Only.1", &["B.Solo.1"]),
            item("B.Solo.1", &[]),
        ];
        let res = compute(&items, 0);
        assert_eq!(res.target.other_versions, ["A.Pkg.1"]);
        let by = |id: &str| res.used_by.iter().find(|u| u.package_id == id).unwrap();
        let latest = by("U.Latest.1");
        assert_eq!((latest.fallback.as_deref(), latest.fallback_exact), (Some("A.Pkg.1"), true));
        let exact = by("U.Exact.1");
        assert_eq!((exact.fallback.as_deref(), exact.fallback_exact), (Some("A.Pkg.1"), false));
        assert!(res.used_by.iter().all(|u| u.package_id != "U.Only.1"));

        let solo = compute(&items, 5);
        assert_eq!(solo.used_by.len(), 1);
        assert_eq!(solo.used_by[0].fallback, None, "nothing left: it breaks");
    }

    #[test]
    fn cycles_and_self_references_terminate() {
        let items = vec![
            item("A.One.1", &["A.Two.1", "A.One.1"]),
            item("A.Two.1", &["A.One.1"]),
        ];
        let res = compute(&items, 0);
        assert_eq!(res.dependencies.len(), 1);
        assert_eq!(res.used_by.len(), 1, "A.Two uses A.One back");
        assert_eq!(dep(&res, "A.Two.1").internal_users, ["/lib/A.One.1.var"]);
    }
}
