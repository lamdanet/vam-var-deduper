//! Disable / enable packages in place, the way VaM itself does it: an empty
//! `<package>.var.disabled` beside the `.var` tells VaM not to load it.
//! Nothing moves, so it is instant and VaM's own package manager agrees with
//! us. After VaM Backstage's `storage-state.js` (marker layout) and
//! `scanner/graph.js` (cascade).
//!
//! Cascade: disabling a package also disables the dependencies nothing still
//! active needs — a dependency qualifies once every enabled package using it
//! is being disabled too (a fixed point, so chains go with it). Enabling a
//! package enables the disabled dependencies it needs again. Offloaded
//! packages are left alone either way; Restore is their way back.

use std::{
    collections::{HashSet, VecDeque},
    fs,
    path::Path,
};

use serde::Serialize;
use tauri::State;

use crate::{
    library::{DepResolution, LibIndex},
    models::{AppState, VarPackageListItem},
    packages::disabled_sidecar,
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DisableEntry {
    pub(crate) file_path: String,
    pub(crate) package_id: String,
    pub(crate) size_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DisablePlan {
    pub(crate) targets: Vec<DisableEntry>,
    /// Disable: dependencies only the targets still need. Enable: disabled
    /// dependencies the targets need.
    pub(crate) cascade: Vec<DisableEntry>,
    /// Disable only: enabled packages that use a target and stay enabled —
    /// they lose a dependency.
    pub(crate) breaks: Vec<DisableEntry>,
}

fn entry(item: &VarPackageListItem) -> DisableEntry {
    DisableEntry {
        file_path: item.file_path.clone(),
        package_id: item.package_id.clone(),
        size_bytes: item.size_bytes,
    }
}

fn active(item: &VarPackageListItem) -> bool {
    !item.disabled && !item.offloaded
}

/// Resolved forward edges (any version that's present) and their reverse.
fn graph(items: &[VarPackageListItem]) -> (Vec<Vec<usize>>, Vec<Vec<usize>>) {
    let index = LibIndex::build(items);
    let mut forward = vec![Vec::new(); items.len()];
    let mut users = vec![Vec::new(); items.len()];
    for (i, item) in items.iter().enumerate() {
        let mut seen = HashSet::new();
        for dep in &item.deps {
            if let DepResolution::Found(t) | DepResolution::OtherVersion(t) = index.resolve(dep) {
                if t != i && seen.insert(t) {
                    forward[i].push(t);
                    users[t].push(i);
                }
            }
        }
    }
    (forward, users)
}

fn transitive(forward: &[Vec<usize>], roots: &HashSet<usize>) -> Vec<usize> {
    let mut seen: HashSet<usize> = roots.clone();
    let mut order = Vec::new();
    let mut queue: VecDeque<usize> = roots.iter().copied().collect();
    while let Some(i) = queue.pop_front() {
        for &d in &forward[i] {
            if seen.insert(d) {
                order.push(d);
                queue.push_back(d);
            }
        }
    }
    order
}

pub(crate) fn plan(items: &[VarPackageListItem], targets: &HashSet<usize>, disable: bool) -> DisablePlan {
    let (forward, users) = graph(items);
    let deps = transitive(&forward, targets);
    let mut out = DisablePlan {
        targets: targets.iter().map(|&i| entry(&items[i])).collect(),
        ..DisablePlan::default()
    };
    if !disable {
        out.cascade = deps
            .iter()
            .filter(|&&d| items[d].disabled && !items[d].offloaded)
            .map(|&d| entry(&items[d]))
            .collect();
        return out;
    }
    let mut going: HashSet<usize> = targets.clone();
    loop {
        let mut changed = false;
        for &d in &deps {
            if going.contains(&d) || !active(&items[d]) {
                continue;
            }
            let needed = users[d].iter().any(|&u| !going.contains(&u) && active(&items[u]));
            if !needed {
                going.insert(d);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    out.cascade = deps
        .iter()
        .filter(|d| going.contains(d) && !targets.contains(d))
        .map(|&d| entry(&items[d]))
        .collect();
    let mut breaks: Vec<usize> = targets
        .iter()
        .flat_map(|&t| users[t].iter().copied())
        .filter(|u| !going.contains(u) && active(&items[*u]))
        .collect();
    breaks.sort_unstable();
    breaks.dedup();
    out.breaks = breaks.into_iter().map(|u| entry(&items[u])).collect();
    out
}

fn with_items<T>(state: &AppState, f: impl FnOnce(&mut [VarPackageListItem]) -> T) -> Result<T, String> {
    let mut cache = state
        .var_packages_folder_cache
        .lock()
        .map_err(|_| "library cache poisoned".to_string())?;
    let items = cache.as_mut().ok_or("Scan the library first.")?;
    Ok(f(&mut items.items))
}

/// What disabling (or enabling) these packages would also do.
#[tauri::command]
pub(crate) fn plan_disable(
    file_paths: Vec<String>,
    disable: bool,
    state: State<'_, AppState>,
) -> Result<DisablePlan, String> {
    with_items(&state, |items| {
        let wanted: HashSet<&str> = file_paths.iter().map(String::as_str).collect();
        let targets: HashSet<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| wanted.contains(it.file_path.as_str()))
            .map(|(i, _)| i)
            .collect();
        plan(items, &targets, disable)
    })
}

/// Create (disable) or remove (enable) the empty marker beside one package.
/// A non-empty `.var.disabled` is somebody's renamed package, not a marker,
/// and is left alone.
pub(crate) fn set_marker(var_path: &Path, disable: bool) -> Result<bool, String> {
    if !var_path.is_file() {
        return Err("the package isn't there any more".into());
    }
    let marker = disabled_sidecar(var_path);
    let existing = fs::metadata(&marker).ok().map(|m| m.len());
    if existing.is_some_and(|len| len > 0) {
        return Err(format!(
            "{} isn't an empty marker — it looks like a renamed package, so it was left alone",
            marker.display()
        ));
    }
    match (disable, existing.is_some()) {
        (true, false) => fs::write(&marker, b"").map(|()| true).map_err(|e| format!("couldn't write the marker: {e}")),
        (false, true) => fs::remove_file(&marker).map(|()| true).map_err(|e| format!("couldn't remove the marker: {e}")),
        _ => Ok(false),
    }
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetDisabledResult {
    pub(crate) changed: Vec<String>,
    pub(crate) failed: Vec<(String, String)>,
}

/// Disable or enable packages (no cascade here: the plan's cascade is passed in
/// as more paths). The cached listing is updated in place.
#[tauri::command]
pub(crate) fn set_packages_disabled(
    file_paths: Vec<String>,
    disabled: bool,
    state: State<'_, AppState>,
) -> SetDisabledResult {
    let mut result = SetDisabledResult::default();
    let mut done: HashSet<String> = HashSet::new();
    for p in &file_paths {
        match set_marker(Path::new(p), disabled) {
            Ok(changed) => {
                done.insert(p.clone());
                if changed {
                    result.changed.push(p.clone());
                }
            }
            Err(e) => result.failed.push((p.clone(), e)),
        }
    }
    let _ = with_items(&state, |items| {
        for item in items.iter_mut().filter(|it| done.contains(&it.file_path)) {
            item.disabled = disabled;
        }
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, deps: &[&str]) -> VarPackageListItem {
        VarPackageListItem {
            file_path: format!("/lib/{id}.var"),
            package_id: id.to_string(),
            deps: deps.iter().map(|d| d.to_string()).collect(),
            ..VarPackageListItem::default()
        }
    }

    fn ids(entries: &[DisableEntry]) -> Vec<&str> {
        let mut v: Vec<&str> = entries.iter().map(|e| e.package_id.as_str()).collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn disabling_takes_unshared_dependencies_and_reports_breakage() {
        // Scene -> Look -> Hair ; Scene -> Shared ; Other -> Shared ; User -> Scene
        let mut items = vec![
            item("A.Scene.1", &["A.Look.latest", "A.Shared.1"]),
            item("A.Look.1", &["A.Hair.1"]),
            item("A.Hair.1", &[]),
            item("A.Shared.1", &[]),
            item("A.Other.1", &["A.Shared.1"]),
            item("A.User.1", &["A.Scene.1"]),
        ];
        let plan = plan(&items, &HashSet::from([0]), true);
        assert_eq!(ids(&plan.cascade), ["A.Hair.1", "A.Look.1"], "the chain goes; Shared stays for Other");
        assert_eq!(ids(&plan.breaks), ["A.User.1"]);

        // A disabled user doesn't keep a dependency enabled.
        items[4].disabled = true;
        let plan2 = super::plan(&items, &HashSet::from([0]), true);
        assert_eq!(ids(&plan2.cascade), ["A.Hair.1", "A.Look.1", "A.Shared.1"]);

        // Enabling brings disabled dependencies back, not offloaded ones.
        items[1].disabled = true;
        items[2].offloaded = true;
        let plan3 = super::plan(&items, &HashSet::from([0]), false);
        assert_eq!(ids(&plan3.cascade), ["A.Look.1"]);
    }

    #[test]
    fn marker_is_written_and_removed_but_never_a_real_file() {
        let dir = std::env::temp_dir().join(format!("vam_disable_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let var = dir.join("A.Pkg.1.var");
        fs::write(&var, b"zip").unwrap();
        assert_eq!(set_marker(&var, true), Ok(true));
        assert_eq!(fs::metadata(disabled_sidecar(&var)).unwrap().len(), 0);
        assert_eq!(set_marker(&var, true), Ok(false), "already disabled");
        assert_eq!(set_marker(&var, false), Ok(true));
        assert!(!disabled_sidecar(&var).exists());
        fs::write(disabled_sidecar(&var), b"real content").unwrap();
        assert!(set_marker(&var, false).is_err(), "a non-empty .disabled is left alone");
        assert!(disabled_sidecar(&var).exists());
        fs::remove_dir_all(&dir).ok();
    }
}
