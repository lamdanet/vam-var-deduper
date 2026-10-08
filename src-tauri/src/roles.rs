//! Installed (what the user chose) versus dependency (pulled in for another
//! package), and the orphans that follow from it. After VaM Backstage's
//! `is_direct` flag and `computeOrphanCascade` / `computeRemovableDeps`
//! (`scanner/graph.js`).
//!
//! The role is stored per package family (`Creator.Name`), not per file, so
//! it carries over to new versions — Backstage's per-file flag doesn't. A
//! family is classified the first time it is seen (nothing uses it →
//! installed, else dependency) and then sticks: when the last package using a
//! dependency goes, it becomes an orphan instead of quietly turning into
//! "installed". Downloads record their intent, and the user can change it.
//!
//! Orphans: dependencies nothing uses, plus dependencies used only by orphans
//! (a fixed point).

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use tauri::State;

use crate::{
    db::Db,
    models::{AppState, VarPackageListItem},
    naming::package_base,
};

fn family(package_id: &str) -> String {
    package_base(package_id).to_ascii_lowercase()
}

/// Classify families seen for the first time, then fill `installed` and
/// `orphan` on every item.
pub(crate) fn apply(items: &mut [VarPackageListItem], db: &Db) {
    let mut roles = crate::db::roles_all(db).unwrap_or_default();
    let mut new_rows: Vec<(String, bool)> = Vec::new();
    let mut family_used: HashMap<String, bool> = HashMap::new();
    for item in items.iter() {
        let used = family_used.entry(family(&item.package_id)).or_insert(false);
        *used |= item.used_by_count > 0;
    }
    for (fam, used) in &family_used {
        if !roles.contains_key(fam) {
            roles.insert(fam.clone(), !used);
            new_rows.push((fam.clone(), !used));
        }
    }
    if !new_rows.is_empty() {
        let _ = crate::db::roles_insert_new(db, &new_rows);
    }
    for item in items.iter_mut() {
        item.installed = roles.get(&family(&item.package_id)).copied().unwrap_or(true);
    }
    mark_orphans(items);
}

/// `orphan` = not installed, and every package using it is itself an orphan
/// (nothing at all, to start with).
pub(crate) fn mark_orphans(items: &mut [VarPackageListItem]) {
    let (_, users) = crate::disable::graph(items);
    let mut orphan: HashSet<usize> = HashSet::new();
    loop {
        let mut changed = false;
        for i in 0..items.len() {
            if orphan.contains(&i) || items[i].installed {
                continue;
            }
            if users[i].iter().all(|u| orphan.contains(u)) {
                orphan.insert(i);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for (i, item) in items.iter_mut().enumerate() {
        item.orphan = orphan.contains(&i);
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemovableDep {
    pub(crate) file_path: String,
    pub(crate) package_id: String,
    pub(crate) size_bytes: u64,
    pub(crate) on_hub: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemovePlan {
    /// Dependencies (not installed) that only the given packages use, so
    /// deleting them leaves these unused too.
    pub(crate) deps: Vec<RemovableDep>,
    pub(crate) deps_bytes: u64,
}

/// What deleting `targets` would leave unused: dependencies (never installed
/// packages) whose every user is being removed — a fixed point.
pub(crate) fn plan_remove(items: &[VarPackageListItem], targets: &HashSet<usize>) -> RemovePlan {
    let (forward, users) = crate::disable::graph(items);
    let mut going = targets.clone();
    let mut reach: Vec<usize> = Vec::new();
    let mut stack: Vec<usize> = targets.iter().copied().collect();
    let mut seen = targets.clone();
    while let Some(i) = stack.pop() {
        for &d in &forward[i] {
            if seen.insert(d) {
                reach.push(d);
                stack.push(d);
            }
        }
    }
    loop {
        let mut changed = false;
        for &d in &reach {
            if going.contains(&d) || items[d].installed {
                continue;
            }
            if users[d].iter().all(|u| going.contains(u)) {
                going.insert(d);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let deps: Vec<RemovableDep> = reach
        .iter()
        .filter(|d| going.contains(d) && !targets.contains(d))
        .map(|&d| RemovableDep {
            file_path: items[d].file_path.clone(),
            package_id: items[d].package_id.clone(),
            size_bytes: items[d].size_bytes,
            on_hub: items[d].on_hub,
        })
        .collect();
    RemovePlan {
        deps_bytes: deps.iter().map(|d| d.size_bytes).sum(),
        deps,
    }
}

/// Dependencies that deleting these packages would leave unused.
#[tauri::command]
pub(crate) fn plan_remove_packages(file_paths: Vec<String>, state: State<'_, AppState>) -> Result<RemovePlan, String> {
    let cache = state
        .var_packages_folder_cache
        .lock()
        .map_err(|_| "library cache poisoned".to_string())?;
    let items = &cache.as_ref().ok_or("Scan the library first.")?.items;
    let wanted: HashSet<&str> = file_paths.iter().map(String::as_str).collect();
    let targets: HashSet<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, it)| wanted.contains(it.file_path.as_str()))
        .map(|(i, _)| i)
        .collect();
    Ok(plan_remove(items, &targets))
}

/// Mark packages' families installed (`true`) or dependencies (`false`), from
/// the user or a download's intent. The cached listing follows at once.
#[tauri::command]
pub(crate) fn set_package_roles(
    package_ids: Vec<String>,
    installed: bool,
    state: State<'_, AppState>,
    db: State<'_, Db>,
) -> Result<(), String> {
    let families: Vec<String> = package_ids.iter().map(|p| family(p)).collect();
    crate::db::roles_set(&db, &families, installed).map_err(|e| e.to_string())?;
    let wanted: HashSet<&str> = families.iter().map(String::as_str).collect();
    if let Ok(mut cache) = state.var_packages_folder_cache.lock() {
        if let Some(c) = cache.as_mut() {
            for item in c.items.iter_mut() {
                if wanted.contains(family(&item.package_id).as_str()) {
                    item.installed = installed;
                }
            }
            mark_orphans(&mut c.items);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, deps: &[&str], installed: bool) -> VarPackageListItem {
        VarPackageListItem {
            file_path: format!("/lib/{id}.var"),
            package_id: id.to_string(),
            deps: deps.iter().map(|d| d.to_string()).collect(),
            size_bytes: 10,
            installed,
            ..VarPackageListItem::default()
        }
    }

    #[test]
    fn orphans_cascade_and_removal_frees_only_unshared_dependencies() {
        let mut items = vec![
            item("A.Scene.1", &["A.Look.1", "A.Shared.1"], true),
            item("A.Look.1", &["A.Hair.1"], false),
            item("A.Hair.1", &[], false),
            item("A.Shared.1", &[], false),
            item("A.Other.1", &["A.Shared.1"], true),
            item("B.Old.1", &["B.Leaf.1"], false), // an orphan: nothing uses it
            item("B.Leaf.1", &[], false),           // used only by an orphan
            item("C.Kept.1", &[], true),            // installed, unused: not an orphan
        ];
        mark_orphans(&mut items);
        let orphans: Vec<&str> = items.iter().filter(|i| i.orphan).map(|i| i.package_id.as_str()).collect();
        assert_eq!(orphans, ["B.Old.1", "B.Leaf.1"]);

        let plan = plan_remove(&items, &HashSet::from([0]));
        let mut freed: Vec<&str> = plan.deps.iter().map(|d| d.package_id.as_str()).collect();
        freed.sort_unstable();
        assert_eq!(freed, ["A.Hair.1", "A.Look.1"], "Shared stays for Other");
        assert_eq!(plan.deps_bytes, 20);
    }

    #[test]
    fn roles_are_set_on_first_sight_and_stick() {
        let db = crate::db::open_in_memory().unwrap();
        let mut items = vec![item("A.Scene.1", &["A.Dep.1"], false), item("A.Dep.1", &[], false)];
        crate::library::apply_graph(&mut items);
        apply(&mut items, &db);
        assert!(items[0].installed && !items[1].installed);
        assert!(!items[1].orphan);
        // The scene goes: its dependency stays a dependency — now an orphan.
        let mut later = vec![item("A.Dep.1", &[], false)];
        crate::library::apply_graph(&mut later);
        apply(&mut later, &db);
        assert!(!later[0].installed);
        assert!(later[0].orphan);
        // A new version of the dependency's family keeps the role.
        let mut newer = vec![item("A.Dep.2", &[], false)];
        crate::library::apply_graph(&mut newer);
        apply(&mut newer, &db);
        assert!(!newer[0].installed);
    }
}
