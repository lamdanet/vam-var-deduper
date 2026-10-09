use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde_json::json;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

use crate::{
    db,
    execute::execute_with_progress,
    fix_var::scan_target_var_for_broken_refs,
    models::{BrokenKind, ExecuteRequest},
    scan::{
        cache_scan_result, extract_vaj_self_paths, load_cached_scan_with_target,
        scan_directory_with_target_with_progress,
    },
    tasks::{export_vam_bundle, list_target_var_text_refs},
    utils::read_json_bytes,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn write_test_var(var_path: &Path, files: &[(&str, &[u8])]) {
    let content_list = files
        .iter()
        .map(|(path, _)| path.to_string())
        .collect::<Vec<_>>();
    let meta = json!({
        "licenseType": "FC",
        "dependencies": {},
        "contentList": content_list,
    });

    let writer = fs::File::create(var_path).expect("create var");
    let mut zip = ZipWriter::new(writer);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    zip.start_file("meta.json", options)
        .expect("write meta header");
    zip.write_all(
        serde_json::to_string_pretty(&meta)
            .expect("serialize meta")
            .as_bytes(),
    )
    .expect("write meta");

    for (path, contents) in files {
        zip.start_file(path, options).expect("write file header");
        zip.write_all(contents).expect("write file");
    }

    zip.finish().expect("finish var");
}

#[test]
#[ignore = "needs the local fixture folder test/ (not in the repo)"]
fn scan_detects_duplicates() {
    let input_dir = repo_root().join("test");
    let scanned = scan_directory_with_target_with_progress(&input_dir, &[], None, |_, _| {})
        .expect("scan should succeed");
    assert_eq!(scanned.packages.len(), 2);
    assert!(!scanned.duplicate_groups.is_empty());
    assert!(scanned
        .duplicate_groups
        .iter()
        .all(|group| group.refs.len() >= 2));
}

#[test]
fn extract_vaj_self_paths_strips_prefix_and_skips_other_refs() {
    // Regression: a source `.vaj` whose customTexture_* fields reference
    // textures via `SELF:/` used to drop those entries silently, causing
    // internalize to leave broken refs in the copied `.vaj`. The bundle
    // expansion now harvests them, so the textures get copied too.
    let raw = br#"{
        "storables": [{
            "id": "x:MaterialCombined",
            "customTexture_MainTex": "SELF:/Custom/Clothing/foo/bar/main.png",
            "customTexture_SpecTex": "SELF:/Custom/Clothing/foo/bar/spec.png",
            "customTexture_GlossTex": "./tex/gloss.png",
            "customTexture_AlphaTex": "OtherPkg.x.1:/Custom/Clothing/foo/alpha.png",
            "customTexture_BumpMap": "",
            "customTexture_DecalTex": "SELF:/"
        }]
    }"#;

    let paths = extract_vaj_self_paths("Custom/Clothing/foo/bar/baz.vaj", raw);
    assert_eq!(
        paths,
        std::collections::BTreeSet::from([
            "Custom/Clothing/foo/bar/main.png".to_string(),
            "Custom/Clothing/foo/bar/spec.png".to_string(),
        ])
    );
}

#[test]
fn text_refs_match_versioned_and_implicit_references() {
    // Regression: a scene pins the texture via `<creator.asset>.latest:/...`
    // (and CJK creator segments) while the .var on disk is `<...>.1.var`. The
    // old exact-stem substring match missed `.latest`, so DB-mode's text-ref
    // safety filter hid the texture even though it shows in local mode. The
    // base-tolerant match (plus implicit .vam same-stem / relative .vaj refs)
    // must surface every referenced resource regardless of how it's named.
    let dir = repo_root().join("tmp_text_refs_test");
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("cleanup");
    }
    fs::create_dir_all(&dir).expect("create dir");
    let var_path = dir.join("vamxw.异界上仙冰若蝶.1.var");

    let scene = r#"{
        "atoms": [{
            "storables": [{
                "id": "clothing",
                "customTexture_MainTex": "vamxw.异界上仙冰若蝶.latest:/Custom/Clothing/Female/Archer/fengyu/jiandai/FengYujiandaimina.png",
                "customTexture_BumpMap": "vamxw.异界上仙冰若蝶.latest:/Custom/Clothing/Female/Archer/fengyu/jiandai/FengYujiandaibimp.png",
                "self": "SELF:/Custom/Scripts/foo.cs"
            }]
        }]
    }"#;
    // A .vam that implicitly loads its same-stem .png with no reference string.
    let vam = br#"{ "id": "Skin" }"#;

    write_test_var(
        &var_path,
        &[
            ("Saves/scene/scene.json", scene.as_bytes()),
            (
                "Custom/Clothing/Female/Archer/fengyu/jiandai/FengYujiandaimina.png",
                b"png-a",
            ),
            (
                "Custom/Clothing/Female/Archer/fengyu/jiandai/FengYujiandaibimp.png",
                b"png-b",
            ),
            ("Custom/Clothing/Female/Skin/Skin.vam", vam),
            ("Custom/Clothing/Female/Skin/Skin.png", b"png-c"),
            ("Custom/Scripts/foo.cs", b"// script"),
        ],
    );

    let refs = list_target_var_text_refs(var_path.to_string_lossy().to_string())
        .expect("text ref scan should succeed");
    let set: BTreeSet<String> = refs.into_iter().collect();

    // `.latest`-pinned references to this package resolve to its own resources.
    assert!(
        set.contains("Custom/Clothing/Female/Archer/fengyu/jiandai/FengYujiandaimina.png"),
        "version-pinned (.latest) texture ref must be harvested: {set:?}"
    );
    assert!(
        set.contains("Custom/Clothing/Female/Archer/fengyu/jiandai/FengYujiandaibimp.png"),
        "version-pinned bump-map ref must be harvested: {set:?}"
    );
    // SELF:/ references still work.
    assert!(set.contains("Custom/Scripts/foo.cs"));
    // Implicit same-stem .vam → .png is treated as referenced.
    assert!(
        set.contains("Custom/Clothing/Female/Skin/Skin.png"),
        "same-stem texture of a .vam must be harvested: {set:?}"
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn read_json_bytes_accepts_trailing_commas() {
    let raw = br#"{
      "dependencies": {
        "foo.bar.1": {},
      },
      "referenceIssues": [
      ],
    }"#;

    let parsed = read_json_bytes(raw, "test-meta.json").expect("json should parse");
    assert!(parsed.get("dependencies").is_some());
    assert!(parsed.get("referenceIssues").is_some());
}

#[test]
#[ignore = "needs the local fixture folder tmp_verify_vars/ (not in the repo)"]
fn scan_detects_duplicates_recursively() {
    let fixture_dir = repo_root().join("tmp_verify_vars");
    let nested_root = repo_root().join("tmp_recursive_scan");
    let nested_dir = nested_root.join("nested");

    if nested_root.exists() {
        fs::remove_dir_all(&nested_root).expect("cleanup");
    }

    fs::create_dir_all(&nested_dir).expect("create nested dir");

    for file_name in ["KeepA.var", "DropB.var"] {
        fs::copy(fixture_dir.join(file_name), nested_dir.join(file_name)).expect("copy var");
    }

    let scanned = scan_directory_with_target_with_progress(&nested_root, &[], None, |_, _| {})
        .expect("scan should succeed");

    assert_eq!(scanned.packages.len(), 2);
    assert!(!scanned.duplicate_groups.is_empty());

    fs::remove_dir_all(&nested_root).expect("cleanup");
}

#[test]
fn execute_generates_report() {
    let temp_root = repo_root().join("tmp_rust_verify_out_report");
    let input_dir = temp_root.join("input");
    let output_dir = temp_root.join("out");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&input_dir).expect("create input dir");

    write_test_var(
        &input_dir.join("KeepA.var"),
        &[("Custom/Test/shared.asset", b"same-asset")],
    );
    write_test_var(
        &input_dir.join("DropB.var"),
        &[
            ("Custom/Test/shared.asset", b"same-asset"),
            ("Custom/Test/unique.asset", b"drop-only"),
        ],
    );

    if output_dir.exists() {
        fs::remove_dir_all(&output_dir).expect("cleanup");
    }

    let scanned = scan_directory_with_target_with_progress(&input_dir, &[], None, |_, _| {})
        .expect("scan should succeed");
    let group = scanned
        .duplicate_groups
        .first()
        .expect("fixture should have duplicate groups");
    let kept_ref = group
        .refs
        .iter()
        .find(|item| item.package_id == "KeepA")
        .expect("expected KeepA ref");
    let mut keep_map = BTreeMap::new();
    keep_map.insert(group.key.clone(), kept_ref.display_name());
    let result = execute_with_progress(
        ExecuteRequest {
            input_dir: input_dir.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: output_dir.display().to_string(),
            vap_dir: None,
            target_var_path: None,
            keep_map,
            target_package_id: None,
            replace: false,
            backup: false,
        },
        None,
        None,
        |_, _| {},
    )
    .expect("execute should succeed");

    let report_path = output_dir.join("dedupe-report.json");
    assert!(report_path.exists());
    assert_eq!(result.stats.scanned_packages, 2);
    assert!(result.stats.duplicate_groups >= 1);

    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&report_path).expect("read report"))
            .expect("parse report");
    let packages = report["packages"]
        .as_object()
        .expect("packages should be an object");
    assert_eq!(
        packages.len(),
        result.stats.changed_packages,
        "report should only include changed packages"
    );
    assert_eq!(
        report["summary"]["unchanged_packages"].as_u64(),
        Some((result.stats.scanned_packages - result.stats.changed_packages) as u64)
    );
    assert!(
        packages
            .values()
            .all(|package| package.get("output").is_some()),
        "changed packages should always include output path"
    );

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn execute_report_includes_rewritten_vap_files() {
    let temp_root = repo_root().join("tmp_rust_verify_report_vap");
    let input_dir = temp_root.join("input");
    let output_dir = temp_root.join("out");
    let vap_dir = temp_root.join("vap");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&input_dir).expect("create input dir");
    fs::create_dir_all(vap_dir.join("nested")).expect("create vap dir");

    write_test_var(
        &input_dir.join("KeepA.var"),
        &[("Custom/Test/shared.asset", b"same-asset")],
    );
    write_test_var(
        &input_dir.join("DropB.var"),
        &[("Custom/Test/shared.asset", b"same-asset")],
    );

    let vap_path = vap_dir.join("nested").join("preset.vap");
    fs::write(
        &vap_path,
        br#"{
            "uid": "DropB:/Custom/Test/shared.asset"
        }"#,
    )
    .expect("write vap");

    let scanned = scan_directory_with_target_with_progress(&input_dir, &[], None, |_, _| {})
        .expect("scan should succeed");
    let group = scanned
        .duplicate_groups
        .first()
        .expect("fixture should have duplicate groups");
    let kept_ref = group
        .refs
        .iter()
        .find(|item| item.package_id == "KeepA")
        .expect("expected KeepA ref");
    let mut keep_map = BTreeMap::new();
    keep_map.insert(group.key.clone(), kept_ref.display_name());
    let result = execute_with_progress(
        ExecuteRequest {
            input_dir: input_dir.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: output_dir.display().to_string(),
            vap_dir: Some(vap_dir.display().to_string()),
            target_var_path: None,
            keep_map,
            target_package_id: None,
            replace: false,
            backup: false,
        },
        None,
        None,
        |_, _| {},
    )
    .expect("execute should succeed");

    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(&result.report_path).expect("read report"))
            .expect("parse report");
    let vap_files = report["vap_files"]
        .as_array()
        .expect("vap_files should be an array");
    let expected_source = vap_path.display().to_string();
    let expected_output = output_dir
        .join("changed")
        .join("vap")
        .join("nested")
        .join("preset.vap")
        .display()
        .to_string();
    assert_eq!(vap_files.len(), 1);
    assert_eq!(report["summary"]["vap_files_rewritten"].as_u64(), Some(1));
    assert_eq!(
        vap_files[0]["source"].as_str(),
        Some(expected_source.as_str())
    );
    assert_eq!(
        vap_files[0]["output"].as_str(),
        Some(expected_output.as_str())
    );
    assert_eq!(vap_files[0]["mode"].as_str(), Some("copy"));

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
#[ignore = "needs the local fixture folder tmp_verify_vars/ (not in the repo)"]
fn execute_rewrites_vap_references() {
    let input_dir = repo_root().join("tmp_verify_vars");
    let output_dir = repo_root().join("tmp_rust_verify_vap_out");
    let vap_dir = repo_root().join("tmp_rust_verify_vap_dir");
    if output_dir.exists() {
        fs::remove_dir_all(&output_dir).expect("cleanup");
    }
    if vap_dir.exists() {
        fs::remove_dir_all(&vap_dir).expect("cleanup");
    }
    fs::create_dir_all(&vap_dir).expect("create vap dir");

    let scanned = scan_directory_with_target_with_progress(&input_dir, &[], None, |_, _| {})
        .expect("scan should succeed");
    let group = scanned
        .duplicate_groups
        .first()
        .expect("fixture should have duplicate groups");
    let kept_ref = group.refs.first().expect("group should have refs");
    let old_ref = group
        .refs
        .get(1)
        .expect("group should have at least two refs");
    let mut keep_map = BTreeMap::new();
    keep_map.insert(group.key.clone(), kept_ref.display_name());

    let vap_path = vap_dir.join("nested").join("preset.vap");
    fs::create_dir_all(vap_path.parent().expect("vap parent")).expect("create nested vap dir");
    fs::write(
        &vap_path,
        format!(
            "{{\n  \"uid\": \"{}:/{}\",\n  \"keep\": \"{}\"\n}}",
            old_ref.package_id, old_ref.internal_path, kept_ref.package_id
        ),
    )
    .expect("write vap");

    let result = execute_with_progress(
        ExecuteRequest {
            input_dir: input_dir.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: output_dir.display().to_string(),
            vap_dir: Some(vap_dir.display().to_string()),
            target_var_path: None,
            keep_map,
            target_package_id: None,
            replace: false,
            backup: false,
        },
        None,
        None,
        |_, _| {},
    )
    .expect("execute should succeed");

    let vap_content = fs::read_to_string(&vap_path).expect("read original vap");
    assert!(vap_content.contains(&format!(
        "{}:/{}",
        old_ref.package_id, old_ref.internal_path
    )));

    let changed_vap_path = output_dir
        .join("changed")
        .join("vap")
        .join("nested")
        .join("preset.vap");
    let changed_vap_content = fs::read_to_string(&changed_vap_path).expect("read changed vap");
    assert!(changed_vap_content.contains(&format!(
        "{}:/{}",
        kept_ref.package_id, kept_ref.internal_path
    )));
    assert!(!changed_vap_content.contains(&format!(
        "{}:/{}",
        old_ref.package_id, old_ref.internal_path
    )));
    assert_eq!(result.stats.vap_files_scanned, 1);
    assert_eq!(result.stats.vap_files_rewritten, 1);

    fs::remove_dir_all(&output_dir).expect("cleanup");
    fs::remove_dir_all(&vap_dir).expect("cleanup");
}

#[test]
#[ignore = "needs the local fixture folder tmp_verify_vars/ (not in the repo)"]
fn execute_replace_mode_backs_up_original_vap_files() {
    let temp_root = repo_root().join("tmp_rust_verify_vap_replace");
    let input_dir = temp_root.join("input");
    let output_dir = temp_root.join("out");
    let vap_dir = temp_root.join("vap_source");
    let backup_dir = output_dir.join("backup");

    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }

    fs::create_dir_all(&input_dir).expect("create input dir");
    fs::create_dir_all(&vap_dir).expect("create vap dir");

    for file_name in ["KeepA.var", "DropB.var"] {
        fs::copy(
            repo_root().join("tmp_verify_vars").join(file_name),
            input_dir.join(file_name),
        )
        .expect("copy var");
    }

    let scanned = scan_directory_with_target_with_progress(&input_dir, &[], None, |_, _| {})
        .expect("scan should succeed");
    let group = scanned
        .duplicate_groups
        .first()
        .expect("fixture should have duplicate groups");
    let kept_ref = group.refs.first().expect("group should have refs");
    let old_ref = group
        .refs
        .get(1)
        .expect("group should have at least two refs");
    let mut keep_map = BTreeMap::new();
    keep_map.insert(group.key.clone(), kept_ref.display_name());

    let vap_path = vap_dir.join("nested").join("preset.vap");
    fs::create_dir_all(vap_path.parent().expect("vap parent")).expect("create nested vap dir");
    let original_content = format!(
        "{{\n  \"uid\": \"{}:/{}\",\n  \"keep\": \"{}\"\n}}",
        old_ref.package_id, old_ref.internal_path, kept_ref.package_id
    );
    fs::write(&vap_path, &original_content).expect("write vap");

    let result = execute_with_progress(
        ExecuteRequest {
            input_dir: input_dir.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: output_dir.display().to_string(),
            vap_dir: Some(vap_dir.display().to_string()),
            target_var_path: None,
            keep_map,
            target_package_id: None,
            replace: true,
            backup: true,
        },
        None,
        None,
        |_, _| {},
    )
    .expect("execute should succeed");

    let vap_content = fs::read_to_string(&vap_path).expect("read rewritten vap");
    assert!(vap_content.contains(&format!(
        "{}:/{}",
        kept_ref.package_id, kept_ref.internal_path
    )));
    assert!(!vap_content.contains(&format!(
        "{}:/{}",
        old_ref.package_id, old_ref.internal_path
    )));

    let backup_vap_path = backup_dir.join("vap").join("nested").join("preset.vap");
    let backup_content = fs::read_to_string(&backup_vap_path).expect("read backup vap");
    assert_eq!(backup_content, original_content);
    assert_eq!(result.stats.vap_files_scanned, 1);
    assert_eq!(result.stats.vap_files_rewritten, 1);

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
#[ignore = "needs the local fixture folder tmp_verify_vars/ (not in the repo)"]
fn execute_scoped_to_target_package_only_changes_requested_var() {
    let input_dir = repo_root().join("tmp_verify_vars");
    let output_dir = repo_root().join("tmp_rust_verify_scoped_out");
    if output_dir.exists() {
        fs::remove_dir_all(&output_dir).expect("cleanup");
    }

    let scanned = scan_directory_with_target_with_progress(&input_dir, &[], None, |_, _| {})
        .expect("scan should succeed");
    let group = scanned
        .duplicate_groups
        .first()
        .expect("fixture should have duplicate groups");
    let target_ref = group
        .refs
        .iter()
        .find(|item| item.package_id == "DropB")
        .expect("fixture should include DropB");
    let keep_ref = group
        .refs
        .iter()
        .find(|item| item.package_id == "KeepA")
        .expect("fixture should include KeepA");

    let mut keep_map = BTreeMap::new();
    keep_map.insert(group.key.clone(), keep_ref.display_name());

    let result = execute_with_progress(
        ExecuteRequest {
            input_dir: input_dir.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: output_dir.display().to_string(),
            vap_dir: None,
            target_var_path: None,
            keep_map,
            target_package_id: Some(target_ref.package_id.clone()),
            replace: false,
            backup: false,
        },
        None,
        None,
        |_, _| {},
    )
    .expect("execute should succeed");

    assert_eq!(result.stats.changed_packages, 1);
    assert_eq!(result.stats.duplicate_groups, 1);
    assert!(output_dir.join("changed").join("DropB.var").exists());
    assert!(!output_dir.join("changed").join("KeepA.var").exists());

    fs::remove_dir_all(&output_dir).expect("cleanup");
}

#[test]
#[ignore = "needs the local fixture folder tmp_verify_vars/ (not in the repo)"]
fn single_var_scan_hashes_only_target_related_size_candidates() {
    let input_dir = repo_root().join("tmp_verify_vars");
    let target_var_path = input_dir.join("DropB.var");

    let scanned =
        scan_directory_with_target_with_progress(&input_dir, &[], Some(&target_var_path), |_, _| {})
            .expect("scan should succeed");

    assert!(!scanned.duplicate_groups.is_empty());
    assert!(scanned
        .duplicate_groups
        .iter()
        .all(|group| group.refs.iter().any(|item| item.package_id == "DropB")));

    let keep_a = scanned
        .packages
        .get("KeepA")
        .expect("fixture should include KeepA");
    assert!(keep_a
        .resource_refs
        .iter()
        .any(|resource| resource.crc32.is_some()));
}

#[test]
#[ignore = "needs the local fixture folder tmp_verify_vars/ (not in the repo)"]
fn execute_external_target_var_only_changes_that_var() {
    let temp_root = repo_root().join("tmp_rust_verify_external_target");
    let input_dir = temp_root.join("input");
    let external_dir = temp_root.join("external");
    let output_dir = temp_root.join("out");

    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }

    fs::create_dir_all(&input_dir).expect("create input dir");
    fs::create_dir_all(&external_dir).expect("create external dir");

    fs::copy(
        repo_root().join("tmp_verify_vars").join("KeepA.var"),
        input_dir.join("KeepA.var"),
    )
    .expect("copy KeepA");
    fs::copy(
        repo_root().join("tmp_verify_vars").join("DropB.var"),
        input_dir.join("DropB.var"),
    )
    .expect("copy DropB");

    let external_var_path = external_dir.join("ExternalDrop.var");
    fs::copy(
        repo_root().join("tmp_verify_vars").join("DropB.var"),
        &external_var_path,
    )
    .expect("copy external DropB");

    let scanned = crate::scan::scan_directory_with_target_with_progress(
        &input_dir, &[],
        Some(&external_var_path),
        |_, _| {},
    )
    .expect("scan should succeed");
    let group = scanned
        .duplicate_groups
        .iter()
        .find(|group| {
            group.refs.iter().any(|item| item.package_id == "KeepA")
                && group
                    .refs
                    .iter()
                    .any(|item| item.package_id == "ExternalDrop")
        })
        .expect("fixture should have a matching duplicate group");
    let keep_ref = group
        .refs
        .iter()
        .find(|item| item.package_id == "KeepA")
        .expect("fixture should include KeepA");

    let mut keep_map = BTreeMap::new();
    keep_map.insert(group.key.clone(), keep_ref.display_name());

    let result = execute_with_progress(
        ExecuteRequest {
            input_dir: input_dir.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: output_dir.display().to_string(),
            vap_dir: None,
            target_var_path: Some(external_var_path.display().to_string()),
            keep_map,
            target_package_id: Some("ExternalDrop".to_string()),
            replace: false,
            backup: false,
        },
        None,
        None,
        |_, _| {},
    )
    .expect("execute should succeed");

    assert_eq!(result.stats.changed_packages, 1);
    assert!(output_dir.join("changed").join("ExternalDrop.var").exists());
    assert!(!output_dir.join("changed").join("KeepA.var").exists());
    assert_eq!(
        fs::read(input_dir.join("DropB.var")).expect("read input DropB"),
        fs::read(&external_var_path).expect("read external DropB")
    );

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn scan_ignores_vaj_and_vab_duplicate_groups() {
    let temp_root = repo_root().join("tmp_rust_verify_vam_family_scan");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&temp_root).expect("create temp dir");

    write_test_var(
        &temp_root.join("Single.var"),
        &[
            ("Custom/Hair/Female/Test/StyleA.vam", br#"{"id":"StyleA"}"#),
            ("Custom/Hair/Female/Test/StyleA.vaj", b"vaj-a"),
            ("Custom/Hair/Female/Test/StyleA.vab", b"same-vab"),
            ("Custom/Hair/Female/Test/StyleB.vam", br#"{"id":"StyleB"}"#),
            ("Custom/Hair/Female/Test/StyleB.vaj", b"vaj-b"),
            ("Custom/Hair/Female/Test/StyleB.vab", b"same-vab"),
        ],
    );

    let scanned = scan_directory_with_target_with_progress(&temp_root, &[], None, |_, _| {})
        .expect("scan should succeed");

    assert!(scanned.duplicate_groups.is_empty());

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn scan_ignores_vmb_duplicate_groups() {
    let temp_root = repo_root().join("tmp_rust_verify_vmi_family_scan");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&temp_root).expect("create temp dir");

    write_test_var(
        &temp_root.join("Single.var"),
        &[
            (
                "Custom/Atom/Person/Morphs/Test/MorphA.vmi",
                br#"{"id":"MorphA"}"#,
            ),
            ("Custom/Atom/Person/Morphs/Test/MorphA.vmb", b"same-vmb"),
            (
                "Custom/Atom/Person/Morphs/Test/MorphB.vmi",
                br#"{"id":"MorphB"}"#,
            ),
            ("Custom/Atom/Person/Morphs/Test/MorphB.vmb", b"same-vmb"),
        ],
    );

    let scanned = scan_directory_with_target_with_progress(&temp_root, &[], None, |_, _| {})
        .expect("scan should succeed");

    assert!(scanned.duplicate_groups.is_empty());

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn target_scan_cache_can_be_reused() {
    let temp_root = std::env::temp_dir().join("vam_var_deduper_target_scan_cache");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&temp_root).expect("create temp dir");

    write_test_var(
        &temp_root.join("KeepA.var"),
        &[(
            "Custom/Atom/Person/Clothing/Test/item.vam",
            br#"{"id":"same"}"#,
        )],
    );
    write_test_var(
        &temp_root.join("DropB.var"),
        &[(
            "Custom/Atom/Person/Clothing/Test/item.vam",
            br#"{"id":"same"}"#,
        )],
    );

    let target_var_path = temp_root.join("DropB.var");
    let scanned =
        scan_directory_with_target_with_progress(&temp_root, &[], Some(&target_var_path), |_, _| {})
            .expect("scan should succeed");
    let cache = Arc::new(Mutex::new(HashMap::new()));
    cache_scan_result(&cache, &temp_root, &[], Some(&target_var_path), &scanned)
        .expect("cache scan result");

    let cached = load_cached_scan_with_target(&cache, &temp_root, &[], Some(&target_var_path))
        .expect("load cached scan")
        .expect("expected cached scan");

    assert_eq!(cached.files, scanned.files);
    assert_eq!(
        cached.duplicate_groups.len(),
        scanned.duplicate_groups.len()
    );
    assert_eq!(cached.packages.len(), scanned.packages.len());

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn scan_spans_multiple_input_roots_and_reuses_cache() {
    let root = std::env::temp_dir().join("vam_var_deduper_multi_root_scan");
    if root.exists() {
        fs::remove_dir_all(&root).expect("cleanup");
    }
    let dir_a = root.join("a");
    let dir_b = root.join("b");
    fs::create_dir_all(&dir_a).expect("create dir a");
    fs::create_dir_all(&dir_b).expect("create dir b");

    write_test_var(
        &dir_a.join("PackA.var"),
        &[("Custom/Atom/Person/Clothing/Test/a.vam", br#"{"id":"a"}"#)],
    );
    write_test_var(
        &dir_b.join("PackB.var"),
        &[("Custom/Atom/Person/Clothing/Test/b.vam", br#"{"id":"b"}"#)],
    );

    let additional = vec![dir_b.clone()];
    let scanned = scan_directory_with_target_with_progress(&dir_a, &additional, None, |_, _| {})
        .expect("multi-root scan should succeed");

    // Both roots' packages are present.
    assert_eq!(scanned.packages.len(), 2);
    let from_a = scanned
        .packages
        .values()
        .any(|pkg| pkg.file_path.starts_with(&dir_a));
    let from_b = scanned
        .packages
        .values()
        .any(|pkg| pkg.file_path.starts_with(&dir_b));
    assert!(from_a, "expected a package from dir_a");
    assert!(from_b, "expected a package from dir_b");

    // Scan/execute must agree on the cache key: caching and loading with the
    // same additional dirs hits the cache regardless of root order.
    let cache = Arc::new(Mutex::new(HashMap::new()));
    cache_scan_result(&cache, &dir_a, &additional, None, &scanned).expect("cache scan result");

    let cached = load_cached_scan_with_target(&cache, &dir_a, &additional, None)
        .expect("load cached scan")
        .expect("expected cached scan");
    assert_eq!(cached.packages.len(), 2);

    // Without the additional dir, the key differs -> cache miss (would rescan a
    // single root). This guards the correctness pillar.
    let miss = load_cached_scan_with_target(&cache, &dir_a, &[], None).expect("load cached scan");
    assert!(miss.is_none());

    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn export_bundle_includes_vmi_support_files() {
    let temp_root = repo_root().join("tmp_rust_export_vmi_bundle");
    let output_dir = temp_root.join("out");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&temp_root).expect("create temp dir");

    let package_path = temp_root.join("MorphPack.var");
    write_test_var(
        &package_path,
        &[
            (
                "Custom/Atom/Person/Morphs/Test/Morph.vmi",
                br#"{"id":"Morph"}"#,
            ),
            ("Custom/Atom/Person/Morphs/Test/Morph.vmb", b"morph-binary"),
        ],
    );

    let count = export_vam_bundle(
        package_path.display().to_string(),
        "Custom/Atom/Person/Morphs/Test/Morph.vmi".to_string(),
        output_dir.display().to_string(),
    )
    .expect("export bundle should succeed");

    let bundle_dir = output_dir.join("Morph");
    assert_eq!(count, 2);
    assert!(bundle_dir.join("Morph.vmi").exists());
    assert!(bundle_dir.join("Morph.vmb").exists());

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn db_open_in_memory_applies_v4_schema() {
    use crate::db;

    let db = db::open_in_memory().expect("open in-memory db");
    let conn = db.conn.lock().expect("lock db");

    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read user_version");
    assert_eq!(version, db::SCHEMA_VERSION);

    let table_names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .expect("prepare table list")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query tables")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("collect tables");

    assert!(table_names.iter().any(|name| name == "packages"));
    assert!(table_names.iter().any(|name| name == "resources"));
    assert!(table_names.iter().any(|name| name == "creators"));
    assert!(table_names.iter().any(|name| name == "categories"));

    let crc_column_present: bool = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('resources') WHERE name = 'crc32'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    assert!(crc_column_present, "resources.crc32 column should exist");

    let creator_column_present: bool = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('packages') WHERE name = 'creator_id'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    assert!(creator_column_present, "packages.creator_id column should exist");

    let category_column_present: bool = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('resources') WHERE name = 'category_id'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    assert!(category_column_present, "resources.category_id column should exist");

    let unique_index_present: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master
             WHERE type = 'index' AND name = 'uq_resources_package_internal_path'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    assert!(
        unique_index_present,
        "uq_resources_package_internal_path index should exist"
    );

    // All 12 seeded categories should be present.
    let category_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM categories", [], |row| row.get(0))
        .expect("count categories");
    assert_eq!(category_count, 12);

    // The Db cache should be populated and match the row ids.
    assert_eq!(db.categories.len(), 12);
    let scene_id_from_db: i64 = conn
        .query_row(
            "SELECT category_id FROM categories WHERE name = 'Scene'",
            [],
            |row| row.get(0),
        )
        .expect("scene id");
    assert_eq!(db.categories.get("Scene").copied(), Some(scene_id_from_db));
}

#[test]
fn db_v8_migration_removes_fts_artifacts() {
    // FTS5 path-search was retired; v5 still creates the table + meta keys
    // (history not rewritten), and v8 tears them down in the same upgrade
    // pass. After a fresh open_in_memory the DB must be free of every FTS
    // artifact regardless of whether v5 ran.
    use crate::db;

    let db = db::open_in_memory().expect("open in-memory db");
    let conn = db.conn.lock().expect("lock db");

    let fts_table_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type IN ('table', 'view') AND name = 'resources_fts'",
            [],
            |row| row.get(0),
        )
        .expect("count fts table");
    assert_eq!(
        fts_table_count, 0,
        "resources_fts must not exist after v8 migration"
    );

    let trigger_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type = 'trigger' AND name LIKE 'resources_fts_%'",
            [],
            |row| row.get(0),
        )
        .expect("count fts triggers");
    assert_eq!(trigger_count, 0, "no resources_fts_* triggers must remain");

    let stale_meta: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM app_meta \
             WHERE key IN ('path_search_ready', 'path_search_last_indexed_id')",
            [],
            |row| row.get(0),
        )
        .expect("count stale meta keys");
    assert_eq!(stale_meta, 0, "no path_search_* keys must remain in app_meta");
}

#[test]
fn upsert_links_creator_and_resource_categories() {
    use crate::{db, models::ResourceRef};

    let db = db::open_in_memory().expect("open db");
    let mut conn = db.conn.lock().expect("lock db");

    // upsert_package internally derives & inserts the creator row.
    db::upsert_package(&conn, "Acme.Pack.1", "C:/Acme.Pack.1.var", 100, 200, None)
        .expect("upsert pack");

    let (creator_id_on_pack, creator_name): (Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT p.creator_id, c.name
             FROM packages p
             LEFT JOIN creators c ON c.creator_id = p.creator_id
             WHERE p.package_id = 'Acme.Pack.1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read pack");
    assert!(creator_id_on_pack.is_some(), "creator_id should be set");
    assert_eq!(creator_name.as_deref(), Some("Acme"));

    // A package id with no `.` prefix Ã¢â€ â€™ creator_id stays NULL.
    db::upsert_package(&conn, "NoDotPack", "C:/NoDotPack.var", 0, 0, None).expect("upsert");
    let no_dot_creator: Option<i64> = conn
        .query_row(
            "SELECT creator_id FROM packages WHERE package_id = 'NoDotPack'",
            [],
            |row| row.get(0),
        )
        .expect("read no-dot pack");
    assert_eq!(no_dot_creator, None);

    // Resources should pick up category ids derived from internal_path.
    let resources = vec![
        ResourceRef {
            package_id: "Acme.Pack.1".to_string(),
            package_file: "C:/Acme.Pack.1.var".to_string(),
            internal_path: "Custom/Atom/Person/Morphs/female/foo.vmi".to_string(),
            crc32: Some(1),
            size: 10,
            effective_size: 10,
        },
        ResourceRef {
            package_id: "Acme.Pack.1".to_string(),
            package_file: "C:/Acme.Pack.1.var".to_string(),
            internal_path: "Saves/scene/My.json".to_string(),
            crc32: Some(2),
            size: 20,
            effective_size: 20,
        },
        ResourceRef {
            package_id: "Acme.Pack.1".to_string(),
            package_file: "C:/Acme.Pack.1.var".to_string(),
            internal_path: "README.txt".to_string(),
            crc32: Some(3),
            size: 30,
            effective_size: 30,
        },
    ];
    db::replace_resources(&mut conn, "Acme.Pack.1", &resources).expect("save resources");

    let mut stmt = conn
        .prepare(
            "SELECT r.internal_path, c.name
             FROM resources r
             LEFT JOIN categories c ON c.category_id = r.category_id
             WHERE r.package_id = 'Acme.Pack.1'
             ORDER BY r.internal_path",
        )
        .expect("prepare select");
    let rows: Vec<(String, Option<String>)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .collect::<std::result::Result<_, _>>()
        .expect("collect");

    let by_path: std::collections::HashMap<String, Option<String>> = rows.into_iter().collect();
    assert_eq!(
        by_path.get("Custom/Atom/Person/Morphs/female/foo.vmi"),
        Some(&Some("Morph".to_string())),
    );
    assert_eq!(
        by_path.get("Saves/scene/My.json"),
        Some(&Some("Scene".to_string())),
    );
    assert_eq!(by_path.get("README.txt"), Some(&Some("Other".to_string())));
}

#[test]
fn db_replace_resources_updates_on_conflict_and_keeps_stale_rows() {
    use crate::{db, models::ResourceRef};

    let db = db::open_in_memory().expect("open db");
    let mut conn = db.conn.lock().expect("lock db");

    db::upsert_package(&conn, "Pack", "C:/pack.var", 100, 200, None).expect("upsert");

    // Initial save: foo + bar
    let initial = vec![
        ResourceRef {
            package_id: "Pack".to_string(),
            package_file: "C:/pack.var".to_string(),
            internal_path: "foo.vam".to_string(),
            crc32: Some(0x11111111),
            size: 10,
            effective_size: 10,
        },
        ResourceRef {
            package_id: "Pack".to_string(),
            package_file: "C:/pack.var".to_string(),
            internal_path: "bar.vam".to_string(),
            crc32: Some(0x22222222),
            size: 20,
            effective_size: 20,
        },
    ];
    db::replace_resources(&mut conn, "Pack", &initial).expect("initial save");

    // Capture row id of `foo.vam` so we can prove the row was UPDATED, not replaced.
    let foo_id_before: i64 = conn
        .query_row(
            "SELECT id FROM resources WHERE package_id = 'Pack' AND internal_path = 'foo.vam'",
            [],
            |row| row.get(0),
        )
        .expect("read foo id");

    // Second save: foo with new size + crc; bar removed; baz added.
    let updated = vec![
        ResourceRef {
            package_id: "Pack".to_string(),
            package_file: "C:/pack.var".to_string(),
            internal_path: "foo.vam".to_string(),
            crc32: Some(0x33333333),
            size: 999,
            effective_size: 1000,
        },
        ResourceRef {
            package_id: "Pack".to_string(),
            package_file: "C:/pack.var".to_string(),
            internal_path: "baz.vam".to_string(),
            crc32: Some(0x44444444),
            size: 30,
            effective_size: 30,
        },
    ];
    db::replace_resources(&mut conn, "Pack", &updated).expect("second save");

    // foo's row id is preserved Ã¢â€ â€™ confirms ON CONFLICT DO UPDATE (not delete+insert)
    let foo_id_after: i64 = conn
        .query_row(
            "SELECT id FROM resources WHERE package_id = 'Pack' AND internal_path = 'foo.vam'",
            [],
            |row| row.get(0),
        )
        .expect("read foo id after");
    assert_eq!(
        foo_id_before, foo_id_after,
        "foo's row id should be stable across conflict-update"
    );

    // foo has new field values
    let (foo_crc, foo_size): (Option<i64>, i64) = conn
        .query_row(
            "SELECT crc32, size FROM resources
             WHERE package_id = 'Pack' AND internal_path = 'foo.vam'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read foo fields");
    assert_eq!(foo_crc, Some(0x33333333));
    assert_eq!(foo_size, 999);

    // bar (no longer in the new set) should be retained, untouched
    let (bar_count, bar_crc): (i64, Option<i64>) = conn
        .query_row(
            "SELECT COUNT(*), MAX(crc32) FROM resources
             WHERE package_id = 'Pack' AND internal_path = 'bar.vam'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("count bar");
    assert_eq!(bar_count, 1, "stale rows should be kept");
    assert_eq!(
        bar_crc,
        Some(0x22222222),
        "bar's original fields should be preserved"
    );

    // baz (newly added) should be present
    let baz_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM resources WHERE package_id = 'Pack' AND internal_path = 'baz.vam'",
            [],
            |row| row.get(0),
        )
        .expect("count baz");
    assert_eq!(baz_count, 1, "new rows should be inserted");
}

#[test]
fn db_resources_unique_on_package_and_internal_path() {
    use crate::db;

    let db = db::open_in_memory().expect("open db");
    let conn = db.conn.lock().expect("lock db");

    db::upsert_package(&conn, "P", "C:/p.var", 1, 1, None).expect("upsert package");

    conn.execute(
        "INSERT INTO resources (package_id, internal_path, crc32, size, effective_size)
         VALUES ('P', 'foo.vam', 1, 10, 10)",
        [],
    )
    .expect("first insert should succeed");

    let result = conn.execute(
        "INSERT INTO resources (package_id, internal_path, crc32, size, effective_size)
         VALUES ('P', 'foo.vam', 2, 20, 20)",
        [],
    );
    assert!(
        result.is_err(),
        "duplicate (package_id, internal_path) must be rejected, got {:?}",
        result
    );

    // Same internal_path under a different package_id is fine.
    db::upsert_package(&conn, "Q", "C:/q.var", 2, 2, None).expect("upsert other package");
    conn.execute(
        "INSERT INTO resources (package_id, internal_path, crc32, size, effective_size)
         VALUES ('Q', 'foo.vam', 3, 30, 30)",
        [],
    )
    .expect("same path under different package_id should succeed");
}

#[test]
fn db_round_trip_packages_and_resources() {
    use crate::{db, models::ResourceRef};

    let db = db::open_in_memory().expect("open db");
    let mut conn = db.conn.lock().expect("lock db");

    let big_modified_ns: u128 = (i64::MAX as u128) + 12345;

    db::upsert_package(
        &conn,
        "Author.Pack.1",
        "C:/vars/Author.Pack.1.var",
        9876,
        big_modified_ns,
        Some("Saves/scene/preview.jpg"),
    )
    .expect("upsert package");

    let resources = vec![
        ResourceRef {
            package_id: "Author.Pack.1".to_string(),
            package_file: "C:/vars/Author.Pack.1.var".to_string(),
            internal_path: "Custom/Clothing/Female/A.vam".to_string(),
            crc32: Some(0xDEAD_BEEFu32),
            size: 1024,
            effective_size: 1024,
        },
        ResourceRef {
            package_id: "Author.Pack.1".to_string(),
            package_file: "C:/vars/Author.Pack.1.var".to_string(),
            internal_path: "Custom/Clothing/Female/B.vam".to_string(),
            crc32: None,
            size: 2048,
            effective_size: 2000,
        },
    ];

    db::replace_resources(&mut conn, "Author.Pack.1", &resources).expect("replace resources");

    let fingerprint =
        db::load_package_fingerprint(&conn, "C:/vars/Author.Pack.1.var").expect("load fp");
    assert_eq!(fingerprint, Some((9876u64, big_modified_ns)));

    let loaded = db::load_resources_for_package(
        &conn,
        "Author.Pack.1",
        "C:/vars/Author.Pack.1.var",
    )
    .expect("load resources");
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].crc32, Some(0xDEAD_BEEFu32));
    assert_eq!(loaded[1].crc32, None);
    assert_eq!(loaded[1].effective_size, 2000);

}

#[test]
fn db_replace_resources_accumulates_across_calls() {
    // Resources from earlier scans of the same package are kept even if a
    // later scan no longer contains them Ã¢â‚¬â€ historical record is preserved.
    use crate::{db, models::ResourceRef};

    let db = db::open_in_memory().expect("open db");
    let mut conn = db.conn.lock().expect("lock db");

    db::upsert_package(&conn, "Pack.A", "C:/x.var", 100, 200, None).expect("upsert");

    let v1 = vec![ResourceRef {
        package_id: "Pack.A".to_string(),
        package_file: "C:/x.var".to_string(),
        internal_path: "first.vam".to_string(),
        crc32: Some(1),
        size: 10,
        effective_size: 10,
    }];
    db::replace_resources(&mut conn, "Pack.A", &v1).expect("v1");

    let v2 = vec![ResourceRef {
        package_id: "Pack.A".to_string(),
        package_file: "C:/x.var".to_string(),
        internal_path: "second.vam".to_string(),
        crc32: Some(2),
        size: 20,
        effective_size: 20,
    }];
    db::replace_resources(&mut conn, "Pack.A", &v2).expect("v2");

    let loaded =
        db::load_resources_for_package(&conn, "Pack.A", "C:/x.var").expect("load resources");
    assert_eq!(loaded.len(), 2, "both resources should be retained");

    let paths: std::collections::HashSet<_> =
        loaded.iter().map(|r| r.internal_path.clone()).collect();
    assert!(paths.contains("first.vam"));
    assert!(paths.contains("second.vam"));
}

#[test]
fn db_load_package_fingerprint_returns_none_for_missing() {
    use crate::db;

    let db = db::open_in_memory().expect("open db");
    let conn = db.conn.lock().expect("lock db");

    let fingerprint = db::load_package_fingerprint(&conn, "C:/does-not-exist.var").expect("load");
    assert!(fingerprint.is_none());
}

#[test]
fn db_prune_missing_packages_drops_stale_rows_and_cascades_resources() {
    use crate::{db, models::ResourceRef};
    use std::collections::HashSet;

    let db = db::open_in_memory().expect("open db");
    let mut conn = db.conn.lock().expect("lock db");

    db::upsert_package(&conn, "A", "C:/a.var", 1, 1, None).expect("a");
    db::upsert_package(&conn, "B", "C:/b.var", 2, 2, None).expect("b");
    db::upsert_package(&conn, "C", "C:/c.var", 3, 3, None).expect("c");

    let res = vec![ResourceRef {
        package_id: "B".to_string(),
        package_file: "C:/b.var".to_string(),
        internal_path: "x.vam".to_string(),
        crc32: Some(0xFFFF_FFFFu32),
        size: 1,
        effective_size: 1,
    }];
    db::replace_resources(&mut conn, "B", &res).expect("insert resources for B");

    let mut present = HashSet::new();
    present.insert("C:/a.var".to_string());

    let removed = db::prune_missing_packages(&mut conn, &present).expect("prune");
    assert_eq!(removed, 2);

    let pkg_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM packages", [], |row| row.get(0))
        .expect("count packages");
    assert_eq!(pkg_count, 1);

    let res_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM resources", [], |row| row.get(0))
        .expect("count resources");
    assert_eq!(res_count, 0);
}

#[test]
fn import_parses_plain_manifest_line() {
    use crate::import::parse_manifest_line;
    let line = "[1] 845842B5   Blazedust.EmissiveClothingPlus.4:/meta.json";
    let parsed = parse_manifest_line(line).expect("plain line should parse");
    assert_eq!(parsed.crc32, 0x845842B5);
    assert_eq!(parsed.package_id, "Blazedust.EmissiveClothingPlus.4");
    assert_eq!(parsed.internal_path, "meta.json");
}

#[test]
fn import_parses_manifest_line_with_display_name() {
    use crate::import::parse_manifest_line;
    let line = "[1] E00CDE36   ||| left salute |||   klphgz.x.1:/Custom/Atom/Person/Morphs/female/Pose Hands/left salute-377b06a7.vmi";
    let parsed = parse_manifest_line(line).expect("display-name line should parse");
    assert_eq!(parsed.crc32, 0xE00CDE36);
    assert_eq!(parsed.package_id, "klphgz.x.1");
    assert_eq!(
        parsed.internal_path,
        "Custom/Atom/Person/Morphs/female/Pose Hands/left salute-377b06a7.vmi"
    );
}

#[test]
fn import_skips_blank_and_malformed_lines() {
    use crate::import::parse_manifest_line;
    assert!(parse_manifest_line("").is_none());
    assert!(parse_manifest_line("   ").is_none());
    assert!(parse_manifest_line("garbage line with no crc").is_none());
    assert!(parse_manifest_line("[1] ZZZZZZZZ pkg:/path").is_none());
    assert!(parse_manifest_line("[1] 12345 pkg:/path").is_none()); // crc < 8 hex
    assert!(parse_manifest_line("[1] 12345678 nopackagecolon").is_none());
}

#[test]
fn import_manifest_inserts_packages_and_resources() {
    use crate::import::import_manifest;
    let dir = tempdir_unique("manifest-import");
    let manifest_path = dir.join("manifest.txt");
    fs::write(
        &manifest_path,
        "[1] 845842B5   Blazedust.EmissiveClothingPlus.4:/meta.json\n\
         [1] 5D6F21AC   Blazedust.EmissiveClothingPlus.4:/Custom/Assets/emissiveshader.assetbundle\n\
         [1] E00CDE36   ||| left salute |||   klphgz.x.1:/Custom/Atom/Person/Morphs/female/Pose Hands/left salute-377b06a7.vmi\n\
         \n\
         garbage that should be skipped\n",
    )
    .expect("write manifest");

    let db = crate::db::open_in_memory().expect("open db");
    let response = import_manifest(&manifest_path, &db, |_, _| {}).expect("import should succeed");

    assert_eq!(response.lines_total, 5);
    assert_eq!(response.lines_parsed, 3);
    assert_eq!(response.lines_skipped, 2);
    assert_eq!(response.packages_inserted, 2);
    assert_eq!(response.packages_existing, 0);
    assert_eq!(response.resources_inserted, 3);
    assert_eq!(response.resources_skipped_existing, 0);

    let conn = db.conn.lock().unwrap();
    let pkg_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM packages", [], |row| row.get(0))
        .expect("count");
    assert_eq!(pkg_count, 2);
    let res_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM resources", [], |row| row.get(0))
        .expect("count");
    assert_eq!(res_count, 3);

    let crc: Option<i64> = conn
        .query_row(
            "SELECT crc32 FROM resources WHERE package_id = ?1 AND internal_path = ?2",
            rusqlite::params![
                "Blazedust.EmissiveClothingPlus.4",
                "meta.json"
            ],
            |row| row.get(0),
        )
        .expect("query resource");
    assert_eq!(crc, Some(0x845842B5_i64));
}

#[test]
fn import_manifest_skips_existing_rows() {
    use crate::import::import_manifest;
    use crate::models::ResourceRef;

    let dir = tempdir_unique("manifest-skip");
    let db = crate::db::open_in_memory().expect("open db");

    {
        let mut conn = db.conn.lock().unwrap();
        crate::db::upsert_package(&conn, "Pack", "C:/Pack.var", 100, 200, None).expect("pkg");
        let resources = vec![ResourceRef {
            package_id: "Pack".to_string(),
            package_file: "C:/Pack.var".to_string(),
            internal_path: "meta.json".to_string(),
            crc32: Some(0x11111111),
            size: 1024,
            effective_size: 1024,
        }];
        crate::db::replace_resources(&mut conn, "Pack", &resources).expect("res");
    }

    let manifest_path = dir.join("manifest.txt");
    fs::write(
        &manifest_path,
        "[1] FFFFFFFF   Pack:/meta.json\n\
         [1] 22222222   Pack:/new_resource.json\n",
    )
    .expect("write manifest");

    let response = import_manifest(&manifest_path, &db, |_, _| {}).expect("import");
    assert_eq!(response.resources_inserted, 1); // only new_resource.json
    assert_eq!(response.resources_skipped_existing, 1); // meta.json already existed
    assert_eq!(response.packages_inserted, 0); // Pack already existed
    assert_eq!(response.packages_existing, 1);

    let conn = db.conn.lock().unwrap();
    let crc: Option<i64> = conn
        .query_row(
            "SELECT crc32 FROM resources WHERE package_id = ?1 AND internal_path = ?2",
            rusqlite::params!["Pack", "meta.json"],
            |row| row.get(0),
        )
        .expect("query");
    assert_eq!(crc, Some(0x11111111_i64), "existing row preserved (crc)");
}

#[test]
fn import_manifest_streams_large_synthetic_file() {
    use crate::import::import_manifest;
    use std::io::Write as _;

    let dir = tempdir_unique("manifest-stream");
    let manifest_path = dir.join("big.txt");

    // 50,000 resources spread across 50 packages Ã¢â‚¬â€ exercises the streaming
    // path and the per-run package dedup HashSet without taking too long.
    {
        let f = fs::File::create(&manifest_path).expect("create manifest");
        let mut w = std::io::BufWriter::new(f);
        for pkg in 0..50 {
            for res in 0..1000 {
                writeln!(
                    w,
                    "[1] {:08X}   Author.Pack{}.1:/Custom/Resource/{}.bin",
                    (pkg * 10000 + res) as u32,
                    pkg,
                    res
                )
                .expect("write line");
            }
        }
        w.flush().expect("flush");
    }

    let db = crate::db::open_in_memory().expect("open db");
    let response =
        import_manifest(&manifest_path, &db, |_, _| {}).expect("import should succeed");

    assert_eq!(response.lines_total, 50_000);
    assert_eq!(response.lines_parsed, 50_000);
    assert_eq!(response.lines_skipped, 0);
    assert_eq!(response.packages_inserted, 50);
    assert_eq!(response.resources_inserted, 50_000);

    let conn = db.conn.lock().unwrap();
    let pkg_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM packages", [], |row| row.get(0))
        .expect("count");
    assert_eq!(pkg_count, 50);
    let res_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM resources", [], |row| row.get(0))
        .expect("count");
    assert_eq!(res_count, 50_000);
}

#[test]
fn db_find_crc_matches_bulk_returns_matches_excluding_self() {
    use crate::{db, models::ResourceRef};

    let db = db::open_in_memory().expect("open db");
    let mut conn = db.conn.lock().expect("lock db");

    // 3 packages: source has CRCs that overlap with target1 and target2.
    db::upsert_package(&conn, "Source", "D:/Source.var", 0, 0, None).expect("upsert source");
    db::upsert_package(&conn, "Target1", "D:/Target1.var", 0, 0, None).expect("upsert t1");
    db::upsert_package(&conn, "Target2", "D:/Target2.var", 0, 0, None).expect("upsert t2");

    fn make_ref(pkg: &str, path: &str, name: &str, crc: u32) -> ResourceRef {
        ResourceRef {
            package_id: pkg.to_string(),
            package_file: path.to_string(),
            internal_path: name.to_string(),
            crc32: Some(crc),
            size: 100,
            effective_size: 100,
        }
    }

    db::replace_resources(
        &mut conn,
        "Source",
        &[
            make_ref("Source", "D:/Source.var", "a.vam", 0xAAAA0001),
            make_ref("Source", "D:/Source.var", "b.vam", 0xAAAA0002),
            make_ref("Source", "D:/Source.var", "lonely.vam", 0xFFFF9999),
        ],
    )
    .expect("seed source");
    db::replace_resources(
        &mut conn,
        "Target1",
        &[
            make_ref("Target1", "D:/Target1.var", "x.vam", 0xAAAA0001),
            make_ref("Target1", "D:/Target1.var", "y.vam", 0xAAAA0002),
        ],
    )
    .expect("seed t1");
    db::replace_resources(
        &mut conn,
        "Target2",
        &[make_ref("Target2", "D:/Target2.var", "z.vam", 0xAAAA0001)],
    )
    .expect("seed t2");

    let mut exclude = std::collections::HashSet::new();
    exclude.insert("Source".to_string());

    let crcs: Vec<u32> = vec![0xAAAA0001, 0xAAAA0002, 0xFFFF9999];
    let mut chunk_calls = 0usize;
    let matches = db::find_crc_matches_bulk(&conn, &crcs, &exclude, |_, _| {
        chunk_calls += 1;
    })
    .expect("bulk match");

    assert_eq!(chunk_calls, 1, "single chunk for small input");
    let aaaa1 = matches.get(&0xAAAA0001).expect("CRC AAAA0001 in result");
    let aaaa1_pkgs: std::collections::HashSet<&str> =
        aaaa1.iter().map(|r| r.package_id.as_str()).collect();
    assert_eq!(aaaa1_pkgs, ["Target1", "Target2"].iter().copied().collect());

    let aaaa2 = matches.get(&0xAAAA0002).expect("CRC AAAA0002 in result");
    assert_eq!(aaaa2.len(), 1);
    assert_eq!(aaaa2[0].package_id, "Target1");

    assert!(
        !matches.contains_key(&0xFFFF9999),
        "CRC with no other-package matches should be absent"
    );
    assert!(
        aaaa1.iter().all(|r| r.package_id != "Source"),
        "self matches must be excluded"
    );
}

#[test]
fn db_find_crc_matches_bulk_empty_input_does_no_query() {
    use crate::db;

    let db = db::open_in_memory().expect("open db");
    let conn = db.conn.lock().expect("lock db");

    let exclude = std::collections::HashSet::new();
    let mut calls = 0usize;
    let matches = db::find_crc_matches_bulk(&conn, &[], &exclude, |_, _| calls += 1)
        .expect("bulk match");
    assert!(matches.is_empty());
    assert_eq!(calls, 0);
}

#[test]
fn db_find_crc_matches_bulk_uses_crc_index() {
    use crate::db;

    let db = db::open_in_memory().expect("open db");
    let conn = db.conn.lock().expect("lock db");

    // EXPLAIN QUERY PLAN check: confirm SQLite uses idx_resources_crc32 for the IN lookup.
    let plan: Vec<String> = conn
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT r.crc32, r.package_id, p.file_path, r.internal_path, r.size
             FROM resources r
             JOIN packages p ON p.package_id = r.package_id
             WHERE r.crc32 IN (?1, ?2, ?3)",
        )
        .expect("prepare plan")
        .query_map(rusqlite::params![1i64, 2i64, 3i64], |row| {
            row.get::<_, String>(3)
        })
        .expect("plan rows")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("collect plan");
    let plan_text = plan.join("\n");
    assert!(
        plan_text.contains("idx_resources_crc32"),
        "expected idx_resources_crc32 in plan, got: {plan_text}"
    );
}

#[test]
fn execute_resolves_db_keep_target_via_fallback() {
    use crate::{db, models::ResourceRef};

    let temp_root = tempdir_unique("execute-db-fallback");
    let input_dir = temp_root.join("input");
    let output_dir = temp_root.join("out");
    fs::create_dir_all(&input_dir).expect("create input dir");

    // Two local packages with a shared resource Ã¢â‚¬â€ produces a duplicate group.
    write_test_var(
        &input_dir.join("Source.var"),
        &[("Custom/Test/shared.asset", b"same-asset")],
    );
    write_test_var(
        &input_dir.join("Other.var"),
        &[("Custom/Test/shared.asset", b"same-asset")],
    );

    let scanned =
        scan_directory_with_target_with_progress(&input_dir, &[], None, |_, _| {}).expect("scan");
    let group = scanned
        .duplicate_groups
        .first()
        .expect("expected one duplicate group")
        .clone();
    let group_crc = group
        .refs
        .iter()
        .find_map(|r| r.crc32)
        .expect("scanned ref must carry crc32");

    // Seed an in-memory DB with a Catalog package the local scan never saw.
    // Same internal_path + crc + size Ã¢â‚¬â€ exercises the keep-key validation
    // path (group key parses to (crc, size), kept ref must match).
    let db = db::open_in_memory().expect("open db");
    {
        let mut conn = db.conn.lock().expect("lock db");
        db::upsert_package(&conn, "Catalog", "C:/Catalog.var", 0, 0, None).expect("upsert");
        db::replace_resources(
            &mut conn,
            "Catalog",
            &[ResourceRef {
                package_id: "Catalog".to_string(),
                package_file: "C:/Catalog.var".to_string(),
                internal_path: "Custom/Test/shared.asset".to_string(),
                crc32: Some(group_crc),
                size: 10,
                effective_size: 10,
            }],
        )
        .expect("seed catalog");
    }

    // Keep-map points to the Catalog (DB-only) package.
    let mut keep_map = BTreeMap::new();
    keep_map.insert(
        group.key.clone(),
        "Catalog:Custom/Test/shared.asset".to_string(),
    );

    let result = execute_with_progress(
        ExecuteRequest {
            input_dir: input_dir.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: output_dir.display().to_string(),
            vap_dir: None,
            target_var_path: None,
            keep_map,
            target_package_id: None,
            replace: false,
            backup: false,
        },
        None,
        Some(&db),
        |_, _| {},
    )
    .expect("execute should succeed via DB fallback");

    assert!(result.stats.duplicate_groups >= 1);
    assert!(
        result.stats.changed_packages >= 1,
        "at least one local package should be rewritten to point at Catalog"
    );

    // Both Source.var and Other.var should be rewritten so the resource is
    // gone (their copies relocated to Catalog).
    let changed_dir = output_dir.join("changed");
    assert!(changed_dir.exists(), "changed dir should be created");

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

fn tempdir_unique(suffix: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.push(format!("vam-var-deduper-test-{suffix}-{nanos}"));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn vmi_stays_duplicate_against_peer_after_dedup_in_separate_output_dir() {
    // Simulates the user's actual workflow:
    //   AddonPackages/vamxw.var          (dedup target, has the .vmi)
    //   AddonPackages/peer.var           (peer with matching .vmi CRC)
    //   dedup-output/ ...                (rewritten vamxw lands here)
    // After dedup, re-scanning the AddonPackages folder must still surface
    // the .vmi as a duplicate against `peer.var`. The output dir is OUTSIDE
    // AddonPackages so there's no package_id collision from a sibling copy.
    use crate::scan::scan_directory_with_target_with_progress_db;

    let temp_root = repo_root().join("tmp_rust_verify_vmi_after_dedup_external_out");
    let addons = temp_root.join("AddonPackages");
    let out = temp_root.join("dedup-output");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&addons).expect("create addons");

    let shared_vam = br#"{"id":"Hair"}"#;
    let shared_vaj = b"vaj-bytes";
    let shared_vmi: &[u8] = br#"{"id":"TestMorph","numDeltas":"100","formulas":[]}"#;
    let shared_vmb = b"vmb-binary-bytes";

    write_test_var(
        &addons.join("vamxw.var"),
        &[
            ("Custom/Hair/Test/Hair.vam", shared_vam),
            ("Custom/Hair/Test/Hair.vaj", shared_vaj),
            ("Custom/Atom/Person/Morphs/female/TestMorph.vmi", shared_vmi),
            ("Custom/Atom/Person/Morphs/female/TestMorph.vmb", shared_vmb),
        ],
    );
    write_test_var(
        &addons.join("peer.var"),
        &[
            ("Custom/Hair/Test/Hair.vam", shared_vam),
            ("Custom/Hair/Test/Hair.vaj", shared_vaj),
            ("Custom/Atom/Person/Morphs/female/TestMorph.vmi", shared_vmi),
            ("Custom/Atom/Person/Morphs/female/TestMorph.vmb", shared_vmb),
        ],
    );

    let scanned = scan_directory_with_target_with_progress_db(
        &addons, &[],
        None,
        None,
        false,
        |_, _| {},
    )
    .expect("first scan");
    let vmi_path = "Custom/Atom/Person/Morphs/female/TestMorph.vmi";
    let first_scan_has_vmi = scanned
        .duplicate_groups
        .iter()
        .any(|g| g.refs.iter().any(|r| r.internal_path == vmi_path));
    assert!(
        first_scan_has_vmi,
        "precondition: .vmi must be a duplicate in the first scan"
    );

    let vam_group = scanned
        .duplicate_groups
        .iter()
        .find(|g| g.refs.iter().any(|r| r.internal_path == "Custom/Hair/Test/Hair.vam"))
        .expect("vam group");
    let keep_ref = vam_group
        .refs
        .iter()
        .find(|r| r.package_id == "peer")
        .expect("peer Hair.vam ref");
    let mut keep_map = BTreeMap::new();
    keep_map.insert(vam_group.key.clone(), keep_ref.display_name());

    execute_with_progress(
        ExecuteRequest {
            input_dir: addons.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: out.display().to_string(),
            vap_dir: None,
            target_var_path: None,
            keep_map,
            target_package_id: Some("vamxw".to_string()),
            replace: false,
            backup: false,
        },
        Some(scanned),
        None,
        |_, _| {},
    )
    .expect("dedup");

    // Output lives OUTSIDE AddonPackages, so re-scanning AddonPackages should
    // still see both vamxw (untouched here Ã¢â‚¬â€ dedup wrote to dedup-output/) and
    // peer, and the .vmi must reappear as a duplicate.
    let rescan = scan_directory_with_target_with_progress_db(
        &addons, &[],
        None,
        None,
        false,
        |_, _| {},
    )
    .expect("rescan");
    let rescan_has_vmi = rescan
        .duplicate_groups
        .iter()
        .any(|g| g.refs.iter().any(|r| r.internal_path == vmi_path));
    assert!(
        rescan_has_vmi,
        ".vmi should still be a duplicate when output dir is outside scan scope"
    );

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn single_var_scan_keeps_designated_target_and_drops_same_pkgid_copies_from_refs() {
    // The dedup-flow invariant the user described: scanning for dedup must
    // treat the target VAR as authoritative even when the input folder
    // contains another copy with the same package_id (the rewritten output
    // from a prior dedup run, a `backup/` snapshot, a stray re-download).
    // Those same-package_id copies aren't valid reference sources Ã¢â‚¬â€ they
    // share content with the target, so matching against them would mark
    // every file for removal. The scan must:
    //   - Keep the exact target_var_path as the package_id entry.
    //   - Drop every other same-package_id copy.
    //   - Still surface unrelated-package_id peers as refs.
    use crate::scan::scan_directory_with_target_with_progress_db;

    let temp_root = repo_root().join("tmp_rust_verify_target_aware_merge");
    let addons = temp_root.join("AddonPackages");
    let output_subdir = addons.join("changed");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&output_subdir).expect("create changed");

    let shared_vmi: &[u8] = br#"{"id":"M","numDeltas":"1","formulas":[]}"#;
    let original = &[
        ("Custom/Atom/Person/Morphs/female/M.vmi", shared_vmi),
        ("Custom/Atom/Person/Morphs/female/M.vmb", b"vmb-bytes" as &[u8]),
        ("Custom/Original/keep-only-here.txt", b"original-marker-bytes"),
    ];
    let rewritten = &[
        ("Custom/Atom/Person/Morphs/female/M.vmi", shared_vmi),
        ("Custom/Atom/Person/Morphs/female/M.vmb", b"vmb-bytes" as &[u8]),
        ("Custom/Rewritten/keep-only-here.txt", b"rewritten-marker-bytes"),
    ];
    let peer = &[
        ("Custom/Atom/Person/Morphs/female/M.vmi", shared_vmi),
        ("Custom/Atom/Person/Morphs/female/M.vmb", b"vmb-bytes" as &[u8]),
    ];
    write_test_var(&addons.join("vamxw.var"), original);
    write_test_var(&output_subdir.join("vamxw.var"), rewritten);
    write_test_var(&addons.join("peer.var"), peer);

    let target_path = output_subdir.join("vamxw.var");
    let scanned = scan_directory_with_target_with_progress_db(
        &addons, &[],
        Some(&target_path),
        None,
        false,
        |_, _| {},
    )
    .expect("scan");

    // The package_id collision must resolve in favor of the explicit target.
    let vamxw = scanned
        .packages
        .get("vamxw")
        .expect("vamxw package present");
    let canon_kept =
        fs::canonicalize(&vamxw.file_path).expect("canonicalize kept vamxw file_path");
    let canon_target = fs::canonicalize(&target_path).expect("canonicalize target");
    assert_eq!(
        canon_kept, canon_target,
        "target_var_path must win the package_id collision over the in-folder original"
    );

    // The kept entry's contents reflect the rewritten copy Ã¢â‚¬â€ confirms the
    // dedup engine will operate on the target's actual bytes, not stale
    // data from the original.
    assert!(
        vamxw
            .resource_refs
            .iter()
            .any(|r| r.internal_path == "Custom/Rewritten/keep-only-here.txt"),
        "kept package should be the rewritten target, not the original"
    );

    // The peer's `.vmi` matches the target's `.vmi` (same CRC) and forms a
    // duplicate group. The dropped original copy must NOT contribute as a
    // ref (otherwise the .vmi would pair against itself and the dedup
    // engine would mark it for removal).
    let vmi_group = scanned
        .duplicate_groups
        .iter()
        .find(|g| {
            g.refs
                .iter()
                .any(|r| r.internal_path == "Custom/Atom/Person/Morphs/female/M.vmi")
        })
        .expect(".vmi must be a duplicate against peer.var");
    let pkg_ids: BTreeSet<&str> = vmi_group
        .refs
        .iter()
        .map(|r| r.package_id.as_str())
        .collect();
    assert_eq!(
        pkg_ids,
        BTreeSet::from(["vamxw", "peer"]),
        ".vmi group should pair the target with the genuine peer only (no self-pairing)"
    );

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn vmi_disappears_when_output_dir_is_inside_addonpackages_due_to_package_id_collision() {
    // The user-reported scenario: dedup output written INTO AddonPackages
    // ('changed/' subdir). Recursive scan finds two VARs with the same
    // package_id (original + rewritten). The 'feedback_duplicate_package_id'
    // rule keeps one and silently ignores the other. Result: if no other
    // non-vamxw peer carries the matching .vmi, the .vmi loses its peer and
    // drops out of the duplicate group list Ã¢â‚¬â€ even though the file is still
    // physically present in both copies.
    use crate::scan::scan_directory_with_target_with_progress_db;

    let temp_root = repo_root().join("tmp_rust_verify_vmi_lost_to_pkgid_collision");
    let addons = temp_root.join("AddonPackages");
    let out = addons.join("changed");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&addons).expect("create addons");

    let shared_vam = br#"{"id":"Hair"}"#;
    let shared_vaj = b"vaj-bytes";
    let shared_vmi: &[u8] = br#"{"id":"TestMorph","numDeltas":"100","formulas":[]}"#;
    let shared_vmb = b"vmb-binary-bytes";

    write_test_var(
        &addons.join("vamxw.var"),
        &[
            ("Custom/Hair/Test/Hair.vam", shared_vam),
            ("Custom/Hair/Test/Hair.vaj", shared_vaj),
            ("Custom/Atom/Person/Morphs/female/TestMorph.vmi", shared_vmi),
            ("Custom/Atom/Person/Morphs/female/TestMorph.vmb", shared_vmb),
        ],
    );

    // Pre-create a "rewritten" copy in `changed/` carrying the same .vmi. No
    // peer.var here Ã¢â‚¬â€ the only place this .vmi exists is across two copies of
    // vamxw with identical package_id. The collision rule collapses them to
    // one entry, so no cross-package duplicate is possible.
    fs::create_dir_all(&out).expect("create changed");
    fs::copy(addons.join("vamxw.var"), out.join("vamxw.var")).expect("copy");

    let scanned = scan_directory_with_target_with_progress_db(
        &addons, &[],
        None,
        None,
        false,
        |_, _| {},
    )
    .expect("scan");

    let vmi_path = "Custom/Atom/Person/Morphs/female/TestMorph.vmi";
    let has_vmi = scanned
        .duplicate_groups
        .iter()
        .any(|g| g.refs.iter().any(|r| r.internal_path == vmi_path));
    assert!(
        !has_vmi,
        "package_id collision should silently merge the two copies and the .vmi loses its only peer"
    );
    assert_eq!(
        scanned.packages.len(),
        1,
        "both copies of vamxw should collapse into a single package entry"
    );

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn vmi_unrelated_to_dedup_keeps_its_crc_after_rewrite() {
    // User-reported regression: a .vmi morph that participated in a
    // cross-VAR duplicate group disappears from the duplicate list after
    // dedup, even though the user didn't select it for removal and the
    // file is still in the rewritten VAR. The cause would be the .vmi
    // going through `rewrite_text_payload` and getting mutated (different
    // bytes Ã¢â€ â€™ different CRC Ã¢â€ â€™ no longer matches the peer). Verify the
    // file bytes survive a rewrite that touched a sibling .vam bundle.
    use crate::scan::scan_directory_with_target_with_progress_db;

    let temp_root = repo_root().join("tmp_rust_verify_vmi_crc_stable");
    let output_dir = temp_root.join("out");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&temp_root).expect("create temp dir");

    let shared_vam = br#"{"id":"Hair"}"#;
    let shared_vaj = b"vaj-bytes";
    let shared_vmi: &[u8] = br#"{
   "id" : "TestMorph",
   "displayName" : "TestMorph",
   "numDeltas" : "100",
   "formulas" : []
}"#;
    let shared_vmb = b"vmb-binary-bytes";

    write_test_var(
        &temp_root.join("KeepA.var"),
        &[
            ("Custom/Hair/Test/Hair.vam", shared_vam),
            ("Custom/Hair/Test/Hair.vaj", shared_vaj),
            ("Custom/Atom/Person/Morphs/female/TestMorph.vmi", shared_vmi),
            ("Custom/Atom/Person/Morphs/female/TestMorph.vmb", shared_vmb),
        ],
    );
    write_test_var(
        &temp_root.join("DropB.var"),
        &[
            ("Custom/Hair/Test/Hair.vam", shared_vam),
            ("Custom/Hair/Test/Hair.vaj", shared_vaj),
            ("Custom/Atom/Person/Morphs/female/TestMorph.vmi", shared_vmi),
            ("Custom/Atom/Person/Morphs/female/TestMorph.vmb", shared_vmb),
        ],
    );

    let scanned = scan_directory_with_target_with_progress_db(
        &temp_root, &[],
        None,
        None,
        false,
        |_, _| {},
    )
    .expect("scan");

    let vam_group = scanned
        .duplicate_groups
        .iter()
        .find(|g| {
            g.refs
                .iter()
                .any(|r| r.internal_path == "Custom/Hair/Test/Hair.vam")
        })
        .expect("vam group");
    let keep_ref = vam_group
        .refs
        .iter()
        .find(|r| r.package_id == "KeepA")
        .expect("KeepA ref");

    let mut keep_map = BTreeMap::new();
    keep_map.insert(vam_group.key.clone(), keep_ref.display_name());

    execute_with_progress(
        ExecuteRequest {
            input_dir: temp_root.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: output_dir.display().to_string(),
            vap_dir: None,
            target_var_path: None,
            keep_map,
            target_package_id: Some("DropB".to_string()),
            replace: false,
            backup: false,
        },
        Some(scanned),
        None,
        |_, _| {},
    )
    .expect("execute");

    let original_vmi_crc = {
        let reader = fs::File::open(temp_root.join("KeepA.var")).expect("open KeepA");
        let mut archive = zip::ZipArchive::new(reader).expect("read KeepA");
        let entry = archive
            .by_name("Custom/Atom/Person/Morphs/female/TestMorph.vmi")
            .expect(".vmi in source");
        entry.crc32()
    };

    let changed = output_dir.join("changed").join("DropB.var");
    let rewritten_vmi_crc = {
        let reader = fs::File::open(&changed).expect("open changed");
        let mut archive = zip::ZipArchive::new(reader).expect("read changed");
        let entry = archive
            .by_name("Custom/Atom/Person/Morphs/female/TestMorph.vmi")
            .expect(".vmi must still be present (not picked for removal)");
        entry.crc32()
    };

    assert_eq!(
        rewritten_vmi_crc, original_vmi_crc,
        ".vmi bytes should be unchanged after rewrite; if this fails the duplicate \
         pairing across VARs breaks even though the user didn't pick the .vmi"
    );

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn vam_effective_size_includes_full_bundle() {
    // The UI's "reclaimable" column reads `effective_size` for each .vam row.
    // It must report the bundle total (.vam + .vaj + .vab + same-stem image
    // + .vaj-referenced textures), not just the .vam file's own bytes.
    use crate::scan::scan_directory_with_target_with_progress_db;

    let temp_root = repo_root().join("tmp_rust_verify_vam_bundle_size");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&temp_root).expect("create temp dir");

    let vam_bytes: &[u8] = br#"{"id":"Style"}"#;
    let vaj_bytes: &[u8] = br#"{"customTexture_AlphaTex":"alp.png"}"#;
    let vab_bytes: &[u8] = &[0xAB; 5000];
    let jpg_bytes: &[u8] = &[0xCD; 800];
    let png_bytes: &[u8] = &[0xEF; 1234];

    write_test_var(
        &temp_root.join("Solo.var"),
        &[
            ("Custom/Clothing/Test/Style.vam", vam_bytes),
            ("Custom/Clothing/Test/Style.vaj", vaj_bytes),
            ("Custom/Clothing/Test/Style.vab", vab_bytes),
            ("Custom/Clothing/Test/Style.jpg", jpg_bytes),
            ("Custom/Clothing/Test/alp.png", png_bytes),
        ],
    );

    let scanned = scan_directory_with_target_with_progress_db(
        &temp_root, &[],
        None,
        None,
        false,
        |_, _| {},
    )
    .expect("scan");

    let package = scanned.packages.get("Solo").expect("Solo package");
    let vam = package
        .resource_refs
        .iter()
        .find(|r| r.internal_path == "Custom/Clothing/Test/Style.vam")
        .expect("vam ref");
    let expected = (vam_bytes.len()
        + vaj_bytes.len()
        + vab_bytes.len()
        + jpg_bytes.len()
        + png_bytes.len()) as u64;
    assert_eq!(
        vam.effective_size, expected,
        "vam effective_size should include the full bundle ({} bytes)",
        expected
    );

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn build_scan_response_indexes_vab_under_parent_vam() {
    // Regression guard for the UI hide rule: the `bundle_index` in
    // ScanResponse must list every `.vab` under its parent `.vam` so
    // `filterEverythingBundleMembers` in app.js drops the lone-.vab dedup
    // row. Without this, the user sees the .vab as an independent dedup
    // candidate even though the engine atomically removes it with the .vam.
    use crate::scan::{
        build_scan_response, scan_directory_with_target_with_progress_db,
    };

    let temp_root = repo_root().join("tmp_rust_verify_bundle_index_vab");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&temp_root).expect("create temp dir");

    write_test_var(
        &temp_root.join("Solo.var"),
        &[
            ("Custom/Clothing/Test/Style.vam", br#"{"id":"Style"}"#),
            (
                "Custom/Clothing/Test/Style.vaj",
                br#"{"customTexture_AlphaTex":"alp.png"}"#,
            ),
            ("Custom/Clothing/Test/Style.vab", b"vab-bytes"),
            ("Custom/Clothing/Test/Style.jpg", b"jpg-bytes"),
            ("Custom/Clothing/Test/alp.png", b"png-bytes"),
        ],
    );

    let scanned = scan_directory_with_target_with_progress_db(
        &temp_root, &[],
        None,
        None,
        false,
        |_, _| {},
    )
    .expect("scan");
    let response = build_scan_response(&scanned);

    let siblings = response
        .bundle_index
        .get("Solo")
        .expect("Solo package in bundle_index")
        .get("Custom/Clothing/Test/Style.vam")
        .expect("vam entry in bundle_index");
    assert!(
        siblings.contains("Custom/Clothing/Test/Style.vab"),
        ".vab must be indexed under its parent .vam so UI can hide it; got {siblings:?}"
    );
    assert!(siblings.contains("Custom/Clothing/Test/Style.vaj"));
    assert!(siblings.contains("Custom/Clothing/Test/Style.jpg"));
    assert!(siblings.contains("Custom/Clothing/Test/alp.png"));

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

fn read_var_entry_names(var_path: &Path) -> Vec<String> {
    let reader = fs::File::open(var_path).expect("open var");
    let mut archive = zip::ZipArchive::new(reader).expect("read archive");
    (0..archive.len())
        .map(|i| {
            archive
                .by_index(i)
                .expect("read entry")
                .name()
                .replace('\\', "/")
        })
        .collect()
}

fn read_var_entry_bytes(var_path: &Path, internal_path: &str) -> Vec<u8> {
    let reader = fs::File::open(var_path).expect("open var");
    let mut archive = zip::ZipArchive::new(reader).expect("read archive");
    let mut entry = archive.by_name(internal_path).expect("entry by name");
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).expect("read entry");
    buf
}

#[test]
fn execute_cascades_vam_bundle_in_everything_mode() {
    use crate::scan::scan_directory_with_target_with_progress_db;

    let temp_root = repo_root().join("tmp_rust_verify_everything_cascade");
    let output_dir = temp_root.join("out");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&temp_root).expect("create temp dir");

    let shared_vam = br#"{"id":"Style","internalId":"Style"}"#;
    let shared_vaj = br#"{"id":"Style","customTexture_AlphaTex":"alp.png"}"#;
    let shared_vab = b"vab-cache-bytes";
    let shared_jpg = b"jpg-thumbnail-bytes";
    let shared_png = b"alpha-texture-bytes";

    write_test_var(
        &temp_root.join("KeepA.var"),
        &[
            ("Custom/Clothing/Test/Style.vam", shared_vam),
            ("Custom/Clothing/Test/Style.vaj", shared_vaj),
            ("Custom/Clothing/Test/Style.vab", shared_vab),
            ("Custom/Clothing/Test/Style.jpg", shared_jpg),
            ("Custom/Clothing/Test/alp.png", shared_png),
        ],
    );
    write_test_var(
        &temp_root.join("DropB.var"),
        &[
            ("Custom/Clothing/Test/Style.vam", shared_vam),
            ("Custom/Clothing/Test/Style.vaj", shared_vaj),
            ("Custom/Clothing/Test/Style.vab", shared_vab),
            ("Custom/Clothing/Test/Style.jpg", shared_jpg),
            ("Custom/Clothing/Test/alp.png", shared_png),
            (
                // Scene references both the .vam (top-level look slot) and the
                // alpha texture directly (customTexture_* block, mirroring the
                // VRDollz Makeup case the user surfaced). Both SELF: refs must
                // be rewritten to the source VAR so the scene loads after dedup.
                "Saves/scene/test.json",
                br#"{
                    "look": "SELF:/Custom/Clothing/Test/Style.vam",
                    "materials": [
                        {
                            "id": "lipgloss",
                            "customTexture_AlphaTex": "SELF:/Custom/Clothing/Test/alp.png"
                        }
                    ]
                }"#,
            ),
        ],
    );

    let scanned = scan_directory_with_target_with_progress_db(
        &temp_root, &[],
        None,
        None,
        false,
        |_, _| {},
    )
    .expect("scan should succeed");

    let vam_group = scanned
        .duplicate_groups
        .iter()
        .find(|group| {
            group
                .refs
                .iter()
                .any(|item| item.internal_path == "Custom/Clothing/Test/Style.vam")
        })
        .expect("vam duplicate group");
    let keep_ref = vam_group
        .refs
        .iter()
        .find(|item| item.package_id == "KeepA")
        .expect("KeepA vam ref");

    let mut keep_map = BTreeMap::new();
    keep_map.insert(vam_group.key.clone(), keep_ref.display_name());

    let result = execute_with_progress(
        ExecuteRequest {
            input_dir: temp_root.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: output_dir.display().to_string(),
            vap_dir: None,
            target_var_path: None,
            keep_map,
            target_package_id: Some("DropB".to_string()),
            replace: false,
            backup: false,
        },
        Some(scanned),
        None,
        |_, _| {},
    )
    .expect("execute should succeed");

    assert_eq!(result.stats.changed_packages, 1);

    let changed_var = output_dir.join("changed").join("DropB.var");
    let names = read_var_entry_names(&changed_var);
    assert!(!names.iter().any(|n| n == "Custom/Clothing/Test/Style.vam"));
    assert!(
        !names.iter().any(|n| n == "Custom/Clothing/Test/Style.vaj"),
        ".vaj sibling must cascade out with the .vam"
    );
    assert!(
        !names.iter().any(|n| n == "Custom/Clothing/Test/Style.vab"),
        ".vab sibling must cascade out with the .vam"
    );
    assert!(
        !names.iter().any(|n| n == "Custom/Clothing/Test/Style.jpg"),
        ".jpg sibling must cascade out with the .vam"
    );
    assert!(
        !names.iter().any(|n| n == "Custom/Clothing/Test/alp.png"),
        ".vaj-referenced texture must cascade out with the .vam"
    );
    assert!(names.iter().any(|n| n == "Saves/scene/test.json"));

    let scene_text = String::from_utf8(read_var_entry_bytes(&changed_var, "Saves/scene/test.json"))
        .expect("scene utf8");
    assert!(
        scene_text.contains("KeepA:/Custom/Clothing/Test/Style.vam"),
        "scene .vam SELF ref should be rewritten to KeepA package; got: {scene_text}"
    );
    assert!(!scene_text.contains("SELF:/Custom/Clothing/Test/Style.vam"));
    assert!(
        scene_text.contains("KeepA:/Custom/Clothing/Test/alp.png"),
        "scene texture SELF ref must follow its .vam to the source VAR; got: {scene_text}"
    );
    assert!(
        !scene_text.contains("SELF:/Custom/Clothing/Test/alp.png"),
        "stale SELF: texture ref would point at the removed local copy"
    );

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn execute_preserves_shared_texture_when_other_vam_still_references_it() {
    use crate::scan::scan_directory_with_target_with_progress_db;

    let temp_root = repo_root().join("tmp_rust_verify_shared_texture");
    let output_dir = temp_root.join("out");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&temp_root).expect("create temp dir");

    let vam_x = br#"{"id":"StyleX"}"#;
    let vam_y = br#"{"id":"StyleY"}"#;
    let vaj_x = br#"{"id":"StyleX","customTexture_AlphaTex":"shared.png"}"#;
    let vaj_y = br#"{"id":"StyleY","customTexture_AlphaTex":"shared.png"}"#;
    let shared_png = b"shared-alpha-bytes";

    write_test_var(
        &temp_root.join("KeepA.var"),
        &[
            ("Custom/Test/StyleX.vam", vam_x),
            ("Custom/Test/StyleX.vaj", vaj_x),
            ("Custom/Test/StyleY.vam", vam_y),
            ("Custom/Test/StyleY.vaj", vaj_y),
            ("Custom/Test/shared.png", shared_png),
        ],
    );
    write_test_var(
        &temp_root.join("DropB.var"),
        &[
            ("Custom/Test/StyleX.vam", vam_x),
            ("Custom/Test/StyleX.vaj", vaj_x),
            ("Custom/Test/StyleY.vam", vam_y),
            ("Custom/Test/StyleY.vaj", vaj_y),
            ("Custom/Test/shared.png", shared_png),
        ],
    );

    let scanned = scan_directory_with_target_with_progress_db(
        &temp_root, &[],
        None,
        None,
        false,
        |_, _| {},
    )
    .expect("scan should succeed");

    let stylex_group = scanned
        .duplicate_groups
        .iter()
        .find(|group| {
            group
                .refs
                .iter()
                .any(|item| item.internal_path == "Custom/Test/StyleX.vam")
        })
        .expect("StyleX vam group");
    let keep_ref = stylex_group
        .refs
        .iter()
        .find(|item| item.package_id == "KeepA")
        .expect("KeepA StyleX ref");

    let mut keep_map = BTreeMap::new();
    keep_map.insert(stylex_group.key.clone(), keep_ref.display_name());

    execute_with_progress(
        ExecuteRequest {
            input_dir: temp_root.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: output_dir.display().to_string(),
            vap_dir: None,
            target_var_path: None,
            keep_map,
            target_package_id: Some("DropB".to_string()),
            replace: false,
            backup: false,
        },
        Some(scanned),
        None,
        |_, _| {},
    )
    .expect("execute should succeed");

    let changed_var = output_dir.join("changed").join("DropB.var");
    let names = read_var_entry_names(&changed_var);
    assert!(!names.iter().any(|n| n == "Custom/Test/StyleX.vam"));
    assert!(!names.iter().any(|n| n == "Custom/Test/StyleX.vaj"));
    assert!(
        names.iter().any(|n| n == "Custom/Test/StyleY.vam"),
        "StyleY.vam must stay (not deduped)"
    );
    assert!(
        names.iter().any(|n| n == "Custom/Test/StyleY.vaj"),
        "StyleY.vaj must stay (paired with StyleY.vam which stays)"
    );
    assert!(
        names.iter().any(|n| n == "Custom/Test/shared.png"),
        "shared.png must survive because StyleY.vaj still references it"
    );

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn execute_blocks_sibling_only_removal_in_everything_mode() {
    use crate::scan::scan_directory_with_target_with_progress_db;

    let temp_root = repo_root().join("tmp_rust_verify_atomicity");
    let output_dir = temp_root.join("out");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&temp_root).expect("create temp dir");

    let shared_vam = br#"{"id":"Style"}"#;
    let shared_vaj = br#"{"id":"Style","customTexture_AlphaTex":"alp.png"}"#;
    let shared_png = b"alpha-bytes";

    // Two VARs share the .vaj and .png CRCs but DIFFER on the .vam. The user
    // picks the .vaj for removal alone (e.g. via a CLI / scripted call). The
    // atomicity guard must drop the .vaj from the removal plan because its
    // parent .vam stays in DropB.
    write_test_var(
        &temp_root.join("KeepA.var"),
        &[
            ("Custom/Test/Style.vam", br#"{"id":"Style","variant":"A"}"#),
            ("Custom/Test/Style.vaj", shared_vaj),
            ("Custom/Test/alp.png", shared_png),
        ],
    );
    write_test_var(
        &temp_root.join("DropB.var"),
        &[
            ("Custom/Test/Style.vam", shared_vam),
            ("Custom/Test/Style.vaj", shared_vaj),
            ("Custom/Test/alp.png", shared_png),
        ],
    );

    let scanned = scan_directory_with_target_with_progress_db(
        &temp_root, &[],
        None,
        None,
        false,
        |_, _| {},
    )
    .expect("scan should succeed");

    let vaj_group = scanned
        .duplicate_groups
        .iter()
        .find(|group| {
            group
                .refs
                .iter()
                .any(|item| item.internal_path == "Custom/Test/Style.vaj")
        })
        .expect("vaj duplicate group");
    let keep_ref = vaj_group
        .refs
        .iter()
        .find(|item| item.package_id == "KeepA")
        .expect("KeepA vaj ref");

    let mut keep_map = BTreeMap::new();
    keep_map.insert(vaj_group.key.clone(), keep_ref.display_name());

    let result = execute_with_progress(
        ExecuteRequest {
            input_dir: temp_root.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: output_dir.display().to_string(),
            vap_dir: None,
            target_var_path: None,
            keep_map,
            target_package_id: Some("DropB".to_string()),
            replace: false,
            backup: false,
        },
        Some(scanned),
        None,
        |_, _| {},
    )
    .expect("execute should succeed");

    // The atomicity guard turned the only requested removal into a no-op,
    // so DropB has no changes and stays in the input dir untouched.
    assert_eq!(
        result.stats.changed_packages, 0,
        "lone .vaj removal must be blocked when its parent .vam stays"
    );
    let changed_var = output_dir.join("changed").join("DropB.var");
    assert!(
        !changed_var.exists(),
        "no rewritten VAR should land in changed/ when atomicity blocks the plan"
    );

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn missing_resources_keeps_explicit_sibling_as_transitive() {
    // Scene names both the parent res.vam AND the sibling res.vab directly.
    // The parent .vam is present in Other.Pkg.1 (healthy); the .vab is not.
    // Explicit text refs to absent paths surface as Transitive Ã¢â‚¬â€ that's the
    // single source of truth the page operates on.
    let temp_root = repo_root().join("tmp_missing_explicit_sibling_stays_transitive");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    let input_dir = temp_root.join("input");
    fs::create_dir_all(&input_dir).expect("create input dir");

    let scene = serde_json::json!({
        "atoms": [{
            "id": "ClothingItem",
            "clothing": "Other.Pkg.1:/Custom/Hair/res.vam",
            "audioBundle": "Other.Pkg.1:/Custom/Hair/res.vab"
        }]
    });
    let scene_bytes = serde_json::to_vec(&scene).expect("serialize scene");
    let target_path = input_dir.join("Target.Pkg.1.var");
    write_test_var(
        &target_path,
        &[("Saves/scene/scene1.json", scene_bytes.as_slice())],
    );
    write_test_var(
        &input_dir.join("Other.Pkg.1.var"),
        &[("Custom/Hair/res.vam", b"vam-bytes")],
    );

    let scanned = scan_directory_with_target_with_progress(&input_dir, &[], None, |_, _| {})
        .expect("scan should succeed");
    let db = db::open_in_memory().expect("open in-memory db");
    let broken = scan_target_var_for_broken_refs(&target_path, &scanned, &db)
        .expect("scan target should succeed");

    let vab_entry = broken
        .iter()
        .find(|b| b.ref_path.as_deref() == Some("Custom/Hair/res.vab"))
        .expect("explicit res.vab ref must surface as Transitive");
    assert_eq!(vab_entry.kind, BrokenKind::Transitive);

    // The parent .vam is healthy, so it must not appear in the broken set Ã¢â‚¬â€
    // the page only surfaces refs to resources not present in any local VAR.
    assert!(
        !broken
            .iter()
            .any(|b| b.ref_path.as_deref() == Some("Custom/Hair/res.vam")),
        "healthy parent .vam must not appear as broken"
    );

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn missing_resources_flags_chained_dedup_redirect() {
    // User-reported scenario: 1.var was deduped to point at 2.var, then 2.var
    // was deduped to point at 3.var. The chain leaves 1.var with a stale
    // `2.var:/Custom/Hair/a.vam` text ref while 2.var no longer ships that
    // resource (3.var does). Scanning 1.var must surface the broken ref as
    // Transitive so the user can repoint it to 3.var.
    let temp_root = repo_root().join("tmp_missing_chained_dedup_redirect");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    let input_dir = temp_root.join("input");
    fs::create_dir_all(&input_dir).expect("create input dir");

    // 1.var's scene text-references 2.Pkg.1:/Custom/Hair/a.vam (the state
    // produced by the first dedup, which redirected 1.var's bundled a.vam
    // onto 2.var). 1.var's meta.json *legitimately* declares Other.Pkg.2 as
    // a dependency Ã¢â‚¬â€ the dedup step that produced the redirect would add it.
    // We simulate that here so the test mirrors the real scenario where the
    // declared dep's resource was deduped away (it must still surface).
    let scene = serde_json::json!({
        "atoms": [{
            "id": "ClothingItem",
            "clothing": "Other.Pkg.2:/Custom/Hair/a.vam"
        }]
    });
    let scene_bytes = serde_json::to_vec(&scene).expect("serialize scene");
    let target_path = input_dir.join("Target.Pkg.1.var");
    let target_meta = serde_json::json!({
        "licenseType": "FC",
        "dependencies": {
            "Other.Pkg.2": { "licenseType": "FC", "dependencies": {} }
        },
        "contentList": ["Saves/scene/scene1.json"],
    });
    {
        let writer = fs::File::create(&target_path).expect("create target var");
        let mut zip = ZipWriter::new(writer);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        zip.start_file("meta.json", options).expect("meta hdr");
        zip.write_all(
            serde_json::to_string_pretty(&target_meta)
                .expect("serialize meta")
                .as_bytes(),
        )
        .expect("meta body");
        zip.start_file("Saves/scene/scene1.json", options)
            .expect("scene hdr");
        zip.write_all(&scene_bytes).expect("scene body");
        zip.finish().expect("finish target var");
    }

    // 2.var, post second dedup: no longer carries Custom/Hair/a.vam.
    write_test_var(&input_dir.join("Other.Pkg.2.var"), &[]);

    // 3.var has the actual resource (and its bundle siblings).
    write_test_var(
        &input_dir.join("Other.Pkg.3.var"),
        &[
            ("Custom/Hair/a.vam", b"vam-bytes"),
            ("Custom/Hair/a.vab", b"vab-bytes"),
        ],
    );

    let scanned = scan_directory_with_target_with_progress(&input_dir, &[], None, |_, _| {})
        .expect("scan should succeed");
    let db = db::open_in_memory().expect("open in-memory db");
    // This is the user's reported scenario; the broken ref must surface Ã¢â‚¬â€ the page
    // even though its package is a declared dependency, because the resource
    // itself was deduped away and 3.var now carries it.
    let broken = scan_target_var_for_broken_refs(&target_path, &scanned, &db)
        .expect("scan target should succeed");

    let stale = broken
        .iter()
        .find(|b| {
            b.ref_pkg == "Other.Pkg.2" && b.ref_path.as_deref() == Some("Custom/Hair/a.vam")
        })
        .expect(
            "stale Other.Pkg.2:/Custom/Hair/a.vam ref must surface Ã¢â‚¬â€ 2.var no longer carries it",
        );
    assert_eq!(stale.kind, BrokenKind::Transitive);
    assert!(
        stale
            .local_candidates
            .iter()
            .any(|c| c.resource.package_id == "Other.Pkg.3"
                && c.resource.internal_path == "Custom/Hair/a.vam"),
        "3.var's copy should appear as a local candidate so the user can repoint there"
    );

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn missing_resources_present_file_in_present_pkg_is_not_flagged() {
    // Regression for a user report: a scene ref like
    // `Archer.LIUSHEN0C02.1:/Custom/Atom/Person/Textures/LIUSHEN/FaceG.jpg`
    // must NOT surface as broken when Archer.LIUSHEN0C02.1.var is present and
    // actually carries FaceG.jpg — even though the SAME package is missing
    // other referenced textures. A present file in a present package resolves
    // Healthy; only the genuinely-absent siblings surface (as Transitive).
    //
    // Uses the checked-in fixtures under docs/tests/input. If they aren't
    // present (they're large binary VARs and may be absent in some checkouts),
    // skip rather than fail so the suite stays green everywhere.
    let input_dir = repo_root().join("docs").join("tests").join("input");
    let target_path = input_dir.join("未知.樱桃.1.var");
    let archer_path = input_dir.join("Archer.LIUSHEN0C02.1.var");
    if !target_path.is_file() || !archer_path.is_file() {
        eprintln!(
            "skipping: fixtures not present at {}",
            input_dir.display()
        );
        return;
    }

    let scanned = scan_directory_with_target_with_progress(&input_dir, &[], None, |_, _| {})
        .expect("scan should succeed");
    assert!(
        scanned.packages.contains_key("Archer.LIUSHEN0C02.1"),
        "Archer package must be picked up by the scan"
    );

    let db = db::open_in_memory().expect("open in-memory db");
    let broken = scan_target_var_for_broken_refs(&target_path, &scanned, &db)
        .expect("scan target should succeed");

    let is_broken = |path: &str| {
        broken.iter().any(|b| {
            b.ref_pkg == "Archer.LIUSHEN0C02.1" && b.ref_path.as_deref() == Some(path)
        })
    };

    // The file the user named — present in Archer — must resolve cleanly.
    assert!(
        !is_broken("Custom/Atom/Person/Textures/LIUSHEN/FaceG.jpg"),
        "FaceG.jpg is present in Archer.LIUSHEN0C02.1 and must not be flagged missing"
    );
    // Other present siblings must likewise not be flagged.
    for present in [
        "Custom/Atom/Person/Textures/LIUSHEN/FaceN.jpg",
        "Custom/Atom/Person/Textures/LIUSHEN/GenitalsG.jpg",
        "Custom/Atom/Person/Textures/LIUSHEN/TorsoN.jpg",
        "Custom/Atom/Person/Textures/LIUSHEN/torsoC.png",
    ] {
        assert!(!is_broken(present), "present file wrongly flagged: {present}");
    }

    // The genuinely-absent siblings the scene references must still surface as
    // Transitive (package present, this specific path absent) so the user
    // learns the scene points at textures the package never shipped.
    for absent in [
        "Custom/Atom/Person/Textures/LIUSHEN/GenC.png",
        "Custom/Atom/Person/Textures/LIUSHEN/limbsC.png",
        "Custom/Atom/Person/Textures/LIUSHEN/LimbsG.jpg",
        "Custom/Atom/Person/Textures/LIUSHEN/TorsoG.jpg",
    ] {
        let hit = broken
            .iter()
            .find(|b| {
                b.ref_pkg == "Archer.LIUSHEN0C02.1" && b.ref_path.as_deref() == Some(absent)
            })
            .unwrap_or_else(|| panic!("absent file must surface as broken: {absent}"));
        assert_eq!(hit.kind, BrokenKind::Transitive, "wrong kind for {absent}");
    }
}

#[test]
fn missing_resources_detects_non_ascii_package_id_ref() {
    // Regression for the byte-level `is_pkg_id_byte` bug: package IDs whose
    // creator segment contains CJK characters (e.g.
    // `Anonymous.VAMÃ§ÂÂµÃ¦Â¢Â¦-Ã¦ËœÂ¥Ã¥ÂºÂ­Ã©â€ºÂª.1`) were silently skipped during pkg-ref
    // collection. The walk-back from `:/` stopped at the first non-ASCII
    // byte, truncating the captured pkg to `.1` (one dot Ã¢â‚¬â€ fails the
    // `>= 2 dots` validity check), so the whole reference never reached the
    // classifier. The Missing Resources page then reported "no broken refs"
    // for the CJK-named package even when its scene-referenced resource was
    // demonstrably gone from disk.
    let temp_root = repo_root().join("tmp_missing_non_ascii_pkg_id");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    let input_dir = temp_root.join("input");
    fs::create_dir_all(&input_dir).expect("create input dir");

    let scene = serde_json::json!({
        "atoms": [{
            "id": "ClothingItem",
            "clothing": "Anonymous.VAMÃ§ÂÂµÃ¦Â¢Â¦-Ã¦ËœÂ¥Ã¥ÂºÂ\u{AD}Ã©â€ºÂª.1:/Custom/Hair/a.vam"
        }]
    });
    let scene_bytes = serde_json::to_vec(&scene).expect("serialize scene");
    let target_path = input_dir.join("Target.Pkg.1.var");
    write_test_var(
        &target_path,
        &[("Saves/scene/scene1.json", scene_bytes.as_slice())],
    );

    // The CJK-named package is present on disk but does NOT carry the
    // referenced resource (mirrors a post-dedup-redirect state).
    write_test_var(&input_dir.join("Anonymous.VAMÃ§ÂÂµÃ¦Â¢Â¦-Ã¦ËœÂ¥Ã¥ÂºÂ\u{AD}Ã©â€ºÂª.1.var"), &[]);

    let scanned = scan_directory_with_target_with_progress(&input_dir, &[], None, |_, _| {})
        .expect("scan should succeed");
    let db = db::open_in_memory().expect("open in-memory db");
    let broken = scan_target_var_for_broken_refs(&target_path, &scanned, &db)
        .expect("scan target should succeed");

    let cjk_ref = broken
        .iter()
        .find(|b| {
            b.ref_pkg == "Anonymous.VAMÃ§ÂÂµÃ¦Â¢Â¦-Ã¦ËœÂ¥Ã¥ÂºÂ\u{AD}Ã©â€ºÂª.1"
                && b.ref_path.as_deref() == Some("Custom/Hair/a.vam")
        })
        .expect("CJK-named pkg ref must be collected and surfaced as broken");
    assert_eq!(cjk_ref.kind, BrokenKind::Transitive);

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

fn db_insert_resources(
    conn: &mut rusqlite::Connection,
    package_id: &str,
    file_path: &str,
    rows: &[(&str, u32, u64)],
) {
    db::upsert_package(conn, package_id, file_path, 1, 1, None).expect("upsert");
    let resources: Vec<crate::models::ResourceRef> = rows
        .iter()
        .map(|(path, crc, size)| crate::models::ResourceRef {
            package_id: package_id.to_string(),
            package_file: file_path.to_string(),
            internal_path: path.to_string(),
            crc32: Some(*crc),
            size: *size,
            effective_size: *size,
        })
        .collect();
    db::replace_resources(conn, package_id, &resources).expect("replace_resources");
}

#[test]
fn execute_relocates_synthetic_dbfind_keep_to_db_source() {
    // Regression for: Find Duplicates (DB mode) injects synthetic groups for
    // every target-VAR resource the scan didn't cluster, keyed
    // `dbfind|<target_pid>|<path>`. Picking a DB row as the keep for one of
    // those groups must remove the resource from the target VAR and rewrite
    // self-refs to point at the DB package Ã¢â‚¬â€ even though the synthetic group
    // never appears in `scanned.duplicate_groups`.
    let temp_root = repo_root().join("tmp_dbfind_synthetic_relocate");
    let input_dir = temp_root.join("input");
    let output_dir = temp_root.join("out");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&input_dir).expect("create input dir");

    let vam_bytes = br#"{"id":"UniqueStyle"}"#;
    let vab_bytes = b"vab-binary-cache";
    let scene_bytes = br#"{"look":"SELF:/Custom/Hair/Unique.vam"}"#;
    let target_var = input_dir.join("Target.Pack.1.var");
    write_test_var(
        &target_var,
        &[
            ("Custom/Hair/Unique.vam", vam_bytes),
            ("Custom/Hair/Unique.vab", vab_bytes),
            ("Saves/scene/test.json", scene_bytes),
        ],
    );

    // No other local VAR carries this CRC32 Ã¢â‚¬â€ the backend scan produces zero
    // duplicate groups for these files. The relocation is driven purely by
    // the keep_map entry that the UI ships for the synthetic group. The
    // dedup step trusts the user's pick (it doesn't re-validate existence),
    // so this DB row only exists to enrich the kept_ref's `package_file`
    // metadata Ã¢â‚¬â€ the relocation would work even without it.
    let db = db::open_in_memory().expect("open in-memory db");
    {
        let mut conn = db.conn.lock().expect("lock db");
        db_insert_resources(
            &mut conn,
            "Other.Pack.1",
            "C:/Other.Pack.1.var",
            &[("Custom/Hair/Unique.vam", 0, vam_bytes.len() as u64)],
        );
    }

    let scanned = scan_directory_with_target_with_progress(&input_dir, &[], None, |_, _| {})
        .expect("scan should succeed");
    assert!(
        scanned.duplicate_groups.is_empty(),
        "fixture intentionally has no local duplicates"
    );

    let mut keep_map = BTreeMap::new();
    keep_map.insert(
        "dbfind|Target.Pack.1|Custom/Hair/Unique.vam".to_string(),
        "Other.Pack.1:Custom/Hair/Unique.vam".to_string(),
    );

    let result = execute_with_progress(
        ExecuteRequest {
            input_dir: input_dir.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: output_dir.display().to_string(),
            vap_dir: None,
            target_var_path: Some(target_var.display().to_string()),
            keep_map,
            target_package_id: Some("Target.Pack.1".to_string()),
            replace: false,
            backup: false,
        },
        Some(scanned),
        Some(&db),
        |_, _| {},
    )
    .expect("execute should succeed");

    assert_eq!(result.stats.changed_packages, 1);
    // .vam + cascaded .vab.
    assert_eq!(
        result.stats.removed_files, 2,
        "both .vam and its stem-paired .vab must come out"
    );

    let changed_var = output_dir.join("changed").join("Target.Pack.1.var");
    let names = read_var_entry_names(&changed_var);
    assert!(
        !names.iter().any(|n| n == "Custom/Hair/Unique.vam"),
        ".vam must be removed from the target VAR"
    );
    assert!(
        !names.iter().any(|n| n == "Custom/Hair/Unique.vab"),
        ".vab must cascade out with the .vam in Everything mode"
    );
    assert!(names.iter().any(|n| n == "Saves/scene/test.json"));

    let scene_text =
        String::from_utf8(read_var_entry_bytes(&changed_var, "Saves/scene/test.json"))
            .expect("scene utf8");
    assert!(
        scene_text.contains("Other.Pack.1:/Custom/Hair/Unique.vam"),
        "SELF: ref must be rewritten to the DB-sourced package; got: {scene_text}"
    );
    assert!(!scene_text.contains("SELF:/Custom/Hair/Unique.vam"));

    let meta_bytes = read_var_entry_bytes(&changed_var, "meta.json");
    let meta: serde_json::Value = serde_json::from_slice(&meta_bytes).expect("parse meta");
    let deps = meta
        .get("dependencies")
        .and_then(|v| v.as_object())
        .expect("dependencies object");
    assert!(
        deps.contains_key("Other.Pack.1"),
        "Other.Pack.1 must be added as a dependency; got {deps:?}"
    );

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn execute_ignores_synthetic_dbfind_keep_at_default() {
    // When the user hasn't picked a DB row, the keep_map entry for a
    // synthetic dbfind group still ships Ã¢â‚¬â€ its value is the target VAR's
    // own ref. The backend must treat that as a no-op (do not remove
    // anything, do not consult the DB).
    let temp_root = repo_root().join("tmp_dbfind_synthetic_default");
    let input_dir = temp_root.join("input");
    let output_dir = temp_root.join("out");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&input_dir).expect("create input dir");

    let target_var = input_dir.join("Target.Pack.1.var");
    write_test_var(
        &target_var,
        &[("Custom/Hair/Unique.vam", br#"{"id":"UniqueStyle"}"#)],
    );

    let scanned = scan_directory_with_target_with_progress(&input_dir, &[], None, |_, _| {})
        .expect("scan should succeed");

    let mut keep_map = BTreeMap::new();
    keep_map.insert(
        "dbfind|Target.Pack.1|Custom/Hair/Unique.vam".to_string(),
        "Target.Pack.1:Custom/Hair/Unique.vam".to_string(),
    );

    let result = execute_with_progress(
        ExecuteRequest {
            input_dir: input_dir.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: output_dir.display().to_string(),
            vap_dir: None,
            target_var_path: Some(target_var.display().to_string()),
            keep_map,
            target_package_id: Some("Target.Pack.1".to_string()),
            replace: false,
            backup: false,
        },
        Some(scanned),
        None,
        |_, _| {},
    )
    .expect("execute should succeed");

    assert_eq!(
        result.stats.changed_packages, 0,
        "default-value synthetic keep must be a no-op"
    );
    assert_eq!(result.stats.removed_files, 0);

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn execute_trusts_user_keep_pick_even_when_neither_local_nor_db_has_it() {
    // The dedup step must be a pure transformation: when the user picks a
    // keep that doesn't exist in any scanned local VAR and isn't in the DB,
    // we still remove the resource and rewrite refs to the picked target.
    // Existence is the scanner's / DB-search's responsibility Ã¢â‚¬â€ execute
    // doesn't second-guess the pick.
    let temp_root = repo_root().join("tmp_keep_trust_no_lookup");
    let input_dir = temp_root.join("input");
    let output_dir = temp_root.join("out");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&input_dir).expect("create input dir");

    let scene_bytes = br#"{"look":"SELF:/Custom/Hair/Unique.vam"}"#;
    let target_var = input_dir.join("Target.Pack.1.var");
    write_test_var(
        &target_var,
        &[
            ("Custom/Hair/Unique.vam", br#"{"id":"UniqueStyle"}"#),
            ("Saves/scene/test.json", scene_bytes),
        ],
    );

    let scanned = scan_directory_with_target_with_progress(&input_dir, &[], None, |_, _| {})
        .expect("scan should succeed");

    let mut keep_map = BTreeMap::new();
    keep_map.insert(
        "dbfind|Target.Pack.1|Custom/Hair/Unique.vam".to_string(),
        // Phantom package Ã¢â‚¬â€ not in the scan, no DB seeded. The user said it,
        // so dedup honors it.
        "Phantom.Pack.1:Custom/Hair/Unique.vam".to_string(),
    );

    let result = execute_with_progress(
        ExecuteRequest {
            input_dir: input_dir.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: output_dir.display().to_string(),
            vap_dir: None,
            target_var_path: Some(target_var.display().to_string()),
            keep_map,
            target_package_id: Some("Target.Pack.1".to_string()),
            replace: false,
            backup: false,
        },
        Some(scanned),
        None,
        |_, _| {},
    )
    .expect("execute must succeed without DB validation");

    assert_eq!(result.stats.changed_packages, 1);
    assert_eq!(result.stats.removed_files, 1);

    let changed_var = output_dir.join("changed").join("Target.Pack.1.var");
    let names = read_var_entry_names(&changed_var);
    assert!(!names.iter().any(|n| n == "Custom/Hair/Unique.vam"));
    assert!(names.iter().any(|n| n == "Saves/scene/test.json"));

    let scene_text =
        String::from_utf8(read_var_entry_bytes(&changed_var, "Saves/scene/test.json"))
            .expect("scene utf8");
    assert!(
        scene_text.contains("Phantom.Pack.1:/Custom/Hair/Unique.vam"),
        "SELF: ref must be rewritten to the phantom package; got: {scene_text}"
    );

    let meta: serde_json::Value =
        serde_json::from_slice(&read_var_entry_bytes(&changed_var, "meta.json"))
            .expect("parse meta");
    let deps = meta
        .get("dependencies")
        .and_then(|v| v.as_object())
        .expect("dependencies object");
    assert!(
        deps.contains_key("Phantom.Pack.1"),
        "phantom package must be added as a dependency; got {deps:?}"
    );

    fs::remove_dir_all(&temp_root).expect("cleanup");
}

#[test]
fn execute_does_not_mutate_db() {
    // Invariant: the dedup transformation is read-only against the DB.
    // The DB is the historical reference the missing-resources flow uses
    // to track down accidentally-removed files; if dedup pruned its own
    // package rows or resource rows post-rewrite, that recovery story
    // would be silently broken. Locks the invariant in: a future commit
    // that adds a sync/prune step at execute-time will fail this test.
    let temp_root = repo_root().join("tmp_dedup_db_readonly");
    let input_dir = temp_root.join("input");
    let output_dir = temp_root.join("out");
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).expect("cleanup");
    }
    fs::create_dir_all(&input_dir).expect("create input dir");

    write_test_var(
        &input_dir.join("KeepA.var"),
        &[("Custom/Test/shared.asset", b"same-asset")],
    );
    write_test_var(
        &input_dir.join("DropB.var"),
        &[("Custom/Test/shared.asset", b"same-asset")],
    );

    // Seed the DB with rows for *both* local packages plus a catalog-only
    // package that the user picks as the keep target. After dedup we want
    // every single row preserved Ã¢â‚¬â€ DropB's local file gets rewritten on
    // disk, but its DB row must stay so a later missing-resources scan
    // can still trace where the asset used to live.
    let db = db::open_in_memory().expect("open db");
    {
        let mut conn = db.conn.lock().expect("lock db");
        db_insert_resources(
            &mut conn,
            "KeepA",
            "C:/KeepA.var",
            &[("Custom/Test/shared.asset", 0xAAAA_BBBB, 10)],
        );
        db_insert_resources(
            &mut conn,
            "DropB",
            "C:/DropB.var",
            &[("Custom/Test/shared.asset", 0xAAAA_BBBB, 10)],
        );
        db_insert_resources(
            &mut conn,
            "Catalog",
            "C:/Catalog.var",
            &[("Custom/Test/shared.asset", 0xAAAA_BBBB, 10)],
        );
    }

    let row_count = |sql: &str| -> i64 {
        let conn = db.conn.lock().expect("lock db");
        conn.query_row(sql, [], |row| row.get(0)).expect("count")
    };
    let packages_before = row_count("SELECT COUNT(*) FROM packages");
    let resources_before = row_count("SELECT COUNT(*) FROM resources");
    let drop_b_rows_before = row_count(
        "SELECT COUNT(*) FROM resources WHERE package_id = 'DropB'",
    );

    let scanned = scan_directory_with_target_with_progress(&input_dir, &[], None, |_, _| {})
        .expect("scan");
    let group = scanned
        .duplicate_groups
        .first()
        .expect("expected duplicate group")
        .clone();
    let mut keep_map = BTreeMap::new();
    keep_map.insert(
        group.key.clone(),
        "Catalog:Custom/Test/shared.asset".to_string(),
    );

    execute_with_progress(
        ExecuteRequest {
            input_dir: input_dir.display().to_string(),
            additional_input_dirs: Vec::new(),
            output_dir: output_dir.display().to_string(),
            vap_dir: None,
            target_var_path: None,
            keep_map,
            target_package_id: None,
            replace: false,
            backup: false,
        },
        Some(scanned),
        Some(&db),
        |_, _| {},
    )
    .expect("execute should succeed");

    let packages_after = row_count("SELECT COUNT(*) FROM packages");
    let resources_after = row_count("SELECT COUNT(*) FROM resources");
    let drop_b_rows_after = row_count(
        "SELECT COUNT(*) FROM resources WHERE package_id = 'DropB'",
    );

    assert_eq!(
        packages_before, packages_after,
        "dedup must not insert/delete package rows"
    );
    assert_eq!(
        resources_before, resources_after,
        "dedup must not insert/delete resource rows"
    );
    assert_eq!(
        drop_b_rows_before, drop_b_rows_after,
        "DropB's resource row must survive Ã¢â‚¬â€ missing-resources scan relies on \
         the DB remembering what DropB used to contain"
    );

    fs::remove_dir_all(&temp_root).expect("cleanup");
}


// ----------------------------------------------------------------------------
// VAR Packages maintenance — Clean Duplicates + Organize by Creator.
//
// Every test asserts a *decision* made by a planner or the executor. Mutations
// go through FakeFileOps, so no test ever touches the real Recycle Bin.
// ----------------------------------------------------------------------------

use crate::{
    models::{PackageAction, PackageCandidate, PackageOpResponse, RecycleSupport, VarRefs},
    packages::{
        build_candidate, disabled_sidecar, image_sidecars, is_app_managed_path, organize_base,
        plan_clean_duplicates, plan_organize_by_creator, read_var_refs, run_apply_package_actions,
        FileOps,
    },
};

/// `write_test_var` hardcodes `"dependencies": {}` and writes meta.json itself
/// before the caller's files, so a caller-supplied meta would land as a *second*
/// zip entry that lookup never reaches — any protection test built on it would
/// be green and assert nothing. This one can actually express dependencies.
fn write_test_var_with_deps(var_path: &Path, deps: &[&str], files: &[(&str, &[u8])]) {
    if let Some(parent) = var_path.parent() {
        fs::create_dir_all(parent).expect("create var parent");
    }
    let mut dep_map = serde_json::Map::new();
    for dep in deps {
        dep_map.insert(
            (*dep).to_string(),
            json!({ "licenseType": "FC", "dependencies": {} }),
        );
    }
    let meta = json!({
        "licenseType": "FC",
        "dependencies": serde_json::Value::Object(dep_map),
        "contentList": files.iter().map(|(p, _)| p.to_string()).collect::<Vec<_>>(),
    });

    let writer = fs::File::create(var_path).expect("create var");
    let mut zip = ZipWriter::new(writer);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file("meta.json", options).expect("meta header");
    zip.write_all(serde_json::to_string_pretty(&meta).unwrap().as_bytes())
        .expect("meta body");
    for (path, contents) in files {
        zip.start_file(*path, options).expect("file header");
        zip.write_all(contents).expect("file body");
    }
    zip.finish().expect("finish var");
}

/// Records what the executor asked for and never touches the real Recycle Bin.
/// `recycle` still removes the file, so post-conditions are genuine.
struct FakeFileOps {
    recycled: Mutex<Vec<PathBuf>>,
    renamed: Mutex<Vec<(PathBuf, PathBuf)>>,
}

impl FakeFileOps {
    fn new() -> Self {
        Self {
            recycled: Mutex::new(Vec::new()),
            renamed: Mutex::new(Vec::new()),
        }
    }
    fn recycled_names(&self) -> Vec<String> {
        self.recycled
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect()
    }
}

impl FileOps for FakeFileOps {
    fn recycle(&self, path: &Path) -> Result<(), String> {
        self.recycled.lock().unwrap().push(path.to_path_buf());
        fs::remove_file(path).map_err(|e| e.to_string())
    }
    fn rename(&self, src: &Path, dest: &Path) -> Result<(), String> {
        self.renamed
            .lock()
            .unwrap()
            .push((src.to_path_buf(), dest.to_path_buf()));
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::rename(src, dest).map_err(|e| e.to_string())
    }
}

fn no_cancel() -> std::sync::atomic::AtomicBool {
    std::sync::atomic::AtomicBool::new(false)
}

/// A scratch dir under the repo root, matching the existing scan/execute tests.
fn pkg_test_dir(name: &str) -> PathBuf {
    let dir = repo_root().join(format!("tmp_pkg_{name}"));
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("cleanup before");
    }
    fs::create_dir_all(&dir).expect("create scratch");
    dir
}

/// Writes a real `.var` zip of a given payload size, so size tie-breaks are
/// exercised against real bytes on disk.
fn write_sized_var(path: &Path, filler: usize) {
    let blob = vec![b'x'; filler];
    write_test_var_with_deps(path, &[], &[("Custom/x.bin", &blob)]);
}

/// Builds candidates exactly the way the real task does: walk, read refs, and
/// let an unreadable file fall to `version: None`.
fn candidates_and_refs(
    roots: &[&Path],
) -> (Vec<PackageCandidate>, HashMap<PathBuf, Result<VarRefs, String>>) {
    candidates_and_refs_at(roots, crate::utils::ScanDepth::Recursive)
}

/// Mirrors `gather_candidates`: the walk is always recursive so the dependency
/// check sees everything, and `depth` only decides which packages are in scope.
fn candidates_and_refs_at(
    roots: &[&Path],
    depth: crate::utils::ScanDepth,
) -> (Vec<PackageCandidate>, HashMap<PathBuf, Result<VarRefs, String>>) {
    let mut cands = Vec::new();
    let mut refs = HashMap::new();
    for root in roots {
        let mut files: Vec<PathBuf> = walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .filter(|p| {
                p.extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x.eq_ignore_ascii_case("var"))
                    .unwrap_or(false)
            })
            .filter(|p| !is_app_managed_path(p))
            .collect();
        files.sort();
        for path in files {
            let meta = fs::metadata(&path).expect("metadata");
            let result = read_var_refs(&path, true);
            let readable = result.is_ok();
            refs.insert(path.clone(), result);
            cands.push(build_candidate(
                root,
                &path,
                meta.len(),
                meta.modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
                false,
                readable,
                crate::packages::scope_allows(depth, root, &path),
            ));
        }
    }
    (cands, refs)
}

fn plan_dupes(roots: &[&Path]) -> PackageOpResponse {
    let (cands, refs) = candidates_and_refs(roots);
    plan_clean_duplicates(&cands, &refs, &|_| RecycleSupport::Supported)
}

fn action_for<'a>(res: &'a PackageOpResponse, id: &str) -> Option<&'a PackageAction> {
    res.actions.iter().find(|a| a.package_id == id)
}

// --- Decision 1a: keep the highest version ----------------------------------

#[test]
fn dedup_keeps_highest_version() {
    let dir = pkg_test_dir("keeps_highest");
    for v in ["1", "2", "3"] {
        write_sized_var(&dir.join(format!("Creator.Pkg.{v}.var")), 10);
    }

    let res = plan_dupes(&[&dir]);

    assert_eq!(res.actions.len(), 2, "only the two older versions act");
    for id in ["Creator.Pkg.1", "Creator.Pkg.2"] {
        let a = action_for(&res, id).unwrap_or_else(|| panic!("{id} planned"));
        assert_eq!(a.reason, "older_version");
        assert_eq!(a.status, "pending");
        assert_eq!(a.op, "trash");
        assert!(a.kept_path.ends_with("Creator.Pkg.3.var"), "kept the newest");
    }
    assert!(action_for(&res, "Creator.Pkg.3").is_none(), "newest survives");

    fs::remove_dir_all(&dir).expect("cleanup");
}

// --- Decision 1b: same id + version -> keep the larger -----------------------

#[test]
fn dedup_same_id_keeps_larger() {
    let dir = pkg_test_dir("same_id_larger");
    write_sized_var(&dir.join("a").join("Creator.Pkg.1.var"), 10);
    write_sized_var(&dir.join("b").join("Creator.Pkg.1.var"), 5000);

    let res = plan_dupes(&[&dir]);

    assert_eq!(res.actions.len(), 1);
    let a = &res.actions[0];
    assert_eq!(a.reason, "duplicate_copy");
    assert!(
        a.file_path.contains("\\a\\"),
        "the small copy is removed: {}",
        a.file_path
    );
    assert!(
        a.kept_path.contains("\\b\\"),
        "the large copy is kept: {}",
        a.kept_path
    );
    assert!(a.kept_size_bytes > a.size_bytes, "UI can show both sizes");
    // No dependency check needed: the id still exists on disk afterwards.
    assert_eq!(a.status, "pending");

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn dedup_same_id_prefers_addon_packages() {
    let dir = pkg_test_dir("prefers_addon");
    // The AddonPackages copy is deliberately SMALLER, to prove location wins.
    write_sized_var(&dir.join("Vars").join("Creator.Pkg.1.var"), 5000);
    write_sized_var(&dir.join("AddonPackages").join("Creator.Pkg.1.var"), 10);

    let res = plan_dupes(&[&dir]);

    assert_eq!(res.actions.len(), 1);
    let a = &res.actions[0];
    assert!(
        a.kept_path.contains("AddonPackages"),
        "keeps the copy VAM actually loads, not merely the larger one: {}",
        a.kept_path,
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

// --- Decision 3: protection --------------------------------------------------

#[test]
fn dedup_protects_referenced_older_version() {
    let dir = pkg_test_dir("protects_ref");
    write_sized_var(&dir.join("Creator.Morphs.1.var"), 10);
    write_sized_var(&dir.join("Creator.Morphs.2.var"), 10);
    write_test_var_with_deps(
        &dir.join("Other.Scene.1.var"),
        &["Creator.Morphs.1"],
        &[("Saves/scene/s.json", b"{}")],
    );

    let res = plan_dupes(&[&dir]);

    let a = action_for(&res, "Creator.Morphs.1").expect("planned");
    assert_eq!(a.status, "protected");
    assert!(
        a.detail.contains("Other.Scene.1"),
        "names the referrer: {}",
        a.detail
    );
    assert_eq!(
        res.reclaimable_bytes, 0,
        "protected rows are not counted as freeable"
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn dedup_latest_ref_does_not_protect() {
    let dir = pkg_test_dir("latest_no_protect");
    write_sized_var(&dir.join("Creator.Morphs.1.var"), 10);
    write_sized_var(&dir.join("Creator.Morphs.2.var"), 10);
    // `.latest` resolves to whatever is newest, and we only ever remove
    // non-newest versions — so it can never be broken by this feature.
    write_test_var_with_deps(
        &dir.join("Other.Scene.1.var"),
        &["Creator.Morphs.latest"],
        &[("Saves/scene/s.json", b"{}")],
    );

    let res = plan_dupes(&[&dir]);

    let a = action_for(&res, "Creator.Morphs.1").expect("planned");
    assert_eq!(a.status, "pending", "a .latest ref must not protect");

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// The single most important edge, and the reason `run_dependency_check_task`
/// could not be reused: its self-skip is family-wide, so it would hide
/// `Creator.Pkg.3` depending on `Creator.Pkg.2` and recycle a version in use.
#[test]
fn dedup_cross_version_dep_protects() {
    let dir = pkg_test_dir("cross_version");
    write_sized_var(&dir.join("Creator.Pkg.2.var"), 10);
    write_test_var_with_deps(
        &dir.join("Creator.Pkg.3.var"),
        &["Creator.Pkg.2"],
        &[("Custom/x.bin", b"x")],
    );

    let res = plan_dupes(&[&dir]);

    let a = action_for(&res, "Creator.Pkg.2").expect("planned");
    assert_eq!(
        a.status, "protected",
        "a newer version depending on an older one must protect it",
    );
    assert!(a.detail.contains("Creator.Pkg.3"), "detail: {}", a.detail);

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// Without the fixpoint, Scene.2 protects Morphs.1 but Morphs.1's own dep on
/// Tex.1 is still trashed — breaking the very file we just protected.
#[test]
fn dedup_fixpoint_protects_protected_files_dep() {
    let dir = pkg_test_dir("fixpoint");
    write_sized_var(&dir.join("C.Tex.1.var"), 10);
    write_sized_var(&dir.join("C.Tex.2.var"), 10);
    write_test_var_with_deps(
        &dir.join("C.Morphs.1.var"),
        &["C.Tex.1"],
        &[("Custom/m.bin", b"m")],
    );
    write_sized_var(&dir.join("C.Morphs.2.var"), 10);
    write_test_var_with_deps(
        &dir.join("C.Scene.2.var"),
        &["C.Morphs.1"],
        &[("Saves/scene/s.json", b"{}")],
    );

    let res = plan_dupes(&[&dir]);

    assert_eq!(
        action_for(&res, "C.Morphs.1").expect("planned").status,
        "protected",
        "directly referenced",
    );
    assert_eq!(
        action_for(&res, "C.Tex.1").expect("planned").status,
        "protected",
        "referenced only BY the protected file — this is what the fixpoint buys",
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// Decision 3 names text payload refs explicitly, so meta-only is not a
/// conforming implementation.
#[test]
fn dedup_payload_ref_protects() {
    let dir = pkg_test_dir("payload_ref");
    write_sized_var(&dir.join("C.Morphs.1.var"), 10);
    write_sized_var(&dir.join("C.Morphs.2.var"), 10);
    // Empty meta, but the payload pins the exact version.
    let vap: &[u8] = br#"{"storables":[{"id":"geometry","morph":"C.Morphs.1:/Custom/x.vmi"}]}"#;
    write_test_var_with_deps(
        &dir.join("Other.Looks.4.var"),
        &[],
        &[("Custom/Atom/Person/Appearance/Girl.vap", vap)],
    );

    let res = plan_dupes(&[&dir]);

    let a = action_for(&res, "C.Morphs.1").expect("planned");
    assert_eq!(a.status, "protected", "a payload ref must protect");
    assert!(a.detail.contains("Other.Looks.4"), "detail: {}", a.detail);

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// A survivor we cannot read might declare anything, so protection cannot be
/// proven — but one locked file must not be a total feature outage.
#[test]
fn dedup_unreadable_survivor_marks_unverified() {
    let dir = pkg_test_dir("unverified");
    write_sized_var(&dir.join("C.Pkg.1.var"), 10);
    write_sized_var(&dir.join("C.Pkg.2.var"), 10);
    // Two identical copies of a package, so a duplicate_copy row also exists.
    write_sized_var(&dir.join("sub").join("C.Pkg.2.var"), 10);
    // A survivor that cannot be opened as a VAR at all.
    fs::write(dir.join("Broken.Thing.1.var"), b"this is not a zip").expect("write broken");

    let res = plan_dupes(&[&dir]);

    assert!(!res.protection_complete, "protection cannot be complete");
    assert!(
        res.notes.iter().any(|n| n.contains("Broken.Thing.1")),
        "the unreadable file is named in notes: {:?}",
        res.notes,
    );
    assert_eq!(
        action_for(&res, "C.Pkg.1").expect("planned").status,
        "unverified",
        "older_version rows cannot be trusted",
    );
    // The copy rule never needed the check: the id survives on disk either way.
    let copy = res
        .actions
        .iter()
        .find(|a| a.reason == "duplicate_copy")
        .expect("a duplicate_copy row exists");
    assert_eq!(
        copy.status, "pending",
        "copies are unaffected by unreadable survivors",
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// A half-copied download must never evict the working version it "supersedes".
#[test]
fn dedup_unreadable_candidate_never_wins() {
    let dir = pkg_test_dir("truncated");
    write_sized_var(&dir.join("Ash.Expr.5.var"), 5000);
    fs::write(dir.join("Ash.Expr.6.var"), b"truncated!").expect("write truncated");

    let res = plan_dupes(&[&dir]);

    assert!(
        res.actions.is_empty(),
        "a corrupt .6 must not evict a working .5: {:?}",
        res.actions
            .iter()
            .map(|a| (&a.package_id, &a.status))
            .collect::<Vec<_>>(),
    );
    assert!(
        res.notes.iter().any(|n| n.contains("not a readable VAR")),
        "notes explain why: {:?}",
        res.notes,
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// `.7` and `.007` parse to the same number but are DIFFERENT ids to VAM.
#[test]
fn dedup_leading_zero_twins_skip_family() {
    let dir = pkg_test_dir("leading_zero");
    write_sized_var(&dir.join("C.Pkg.7.var"), 10);
    write_sized_var(&dir.join("C.Pkg.007.var"), 10);

    let res = plan_dupes(&[&dir]);

    assert!(res.actions.is_empty(), "ambiguous family is left alone");
    assert!(
        res.notes.iter().any(|n| n.contains("same version number")),
        "the skip is explained: {:?}",
        res.notes,
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// The app's own backup trees are same-stem byte-copies sitting INSIDE the
/// library. Acting on them would silently revert this app's own core feature.
#[test]
fn dedup_ignores_app_managed_dirs() {
    for (index, managed) in ["backup", "changed", "fix-var-backup", "internalize-backup"]
        .iter()
        .enumerate()
    {
        // The scratch dir must not itself contain the managed name, or the
        // component assertions below would match the scratch dir rather than
        // the managed subfolder.
        let dir = pkg_test_dir(&format!("managed_{index}"));
        write_sized_var(&dir.join("C.Pkg.1.var"), 10);
        // The backup is LARGER — exactly the trap keep-the-larger would fall into.
        write_sized_var(&dir.join(managed).join("C.Pkg.1.var"), 5000);

        let res = plan_dupes(&[&dir]);
        assert!(
            res.actions.is_empty(),
            "{managed}/ must be invisible to Clean Duplicates, got {:?}",
            res.actions.iter().map(|a| &a.file_path).collect::<Vec<_>>(),
        );

        let (cands, _) = candidates_and_refs(&[&dir]);
        let org = plan_organize_by_creator(&cands);
        // Matching is component-wise, so check components rather than substrings.
        let touched_managed = org.actions.iter().any(|a| {
            Path::new(&a.file_path)
                .components()
                .any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case(managed))
        });
        assert!(!touched_managed, "{managed}/ must be invisible to Organize too");
        // The live file is still organized normally — exclusion is scoped to the
        // managed folder, not to the whole family.
        assert_eq!(org.actions.len(), 1, "the live C.Pkg.1.var still moves");

        fs::remove_dir_all(&dir).expect("cleanup");
    }
}

/// VAM disables a package with a `.disabled` sidecar while the `.var` stays on
/// disk. Recycling the enabled older version would leave nothing loadable.
#[test]
fn dedup_disabled_newer_protects_older() {
    let dir = pkg_test_dir("disabled_newer");
    write_sized_var(&dir.join("C.Pkg.1.var"), 10);
    write_sized_var(&dir.join("C.Pkg.2.var"), 10);
    fs::write(dir.join("C.Pkg.2.var.disabled"), b"").expect("sidecar");

    let res = plan_dupes(&[&dir]);

    let a = action_for(&res, "C.Pkg.1").expect("planned");
    assert_eq!(a.status, "protected");
    assert!(a.detail.contains("disabled"), "detail: {}", a.detail);

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// An offloaded newer version isn't loaded either, so the older one still in
/// AddonPackages must stay.
#[test]
fn dedup_offloaded_newer_protects_older() {
    let dir = pkg_test_dir("offloaded_newer");
    let addon = dir.join("AddonPackages");
    let offload = dir.join("AddonPackages_offload");
    fs::create_dir_all(&addon).expect("addon");
    fs::create_dir_all(&offload).expect("offload");
    write_sized_var(&addon.join("C.Pkg.1.var"), 10);
    write_sized_var(&offload.join("C.Pkg.2.var"), 10);

    let res = plan_dupes(&[&addon, &offload]);

    let a = action_for(&res, "C.Pkg.1").expect("planned");
    assert_eq!(a.status, "protected");
    assert!(a.detail.contains("outside AddonPackages"), "detail: {}", a.detail);

    fs::remove_dir_all(&dir).expect("cleanup");
}

// --- Decision 2: Recycle Bin -------------------------------------------------

#[test]
fn apply_blocked_volume_never_recycles() {
    let dir = pkg_test_dir("blocked_volume");
    write_sized_var(&dir.join("C.Pkg.1.var"), 10);
    write_sized_var(&dir.join("C.Pkg.2.var"), 10);

    let (cands, refs) = candidates_and_refs(&[&dir]);
    let res = plan_clean_duplicates(&cands, &refs, &|_| RecycleSupport::Unsupported);

    let a = action_for(&res, "C.Pkg.1").expect("planned");
    assert_eq!(a.status, "blocked");
    assert!(a.detail.contains("Recycle Bin"), "detail: {}", a.detail);

    let ops = FakeFileOps::new();
    let mut actions = res.actions.clone();
    run_apply_package_actions(&mut actions, &ops, &no_cancel(), &mut |_, _| {});
    assert!(
        ops.recycled.lock().unwrap().is_empty(),
        "a blocked row must never be recycled — that would be a permanent delete",
    );
    assert!(dir.join("C.Pkg.1.var").exists(), "file untouched");

    fs::remove_dir_all(&dir).expect("cleanup");
}

// --- Decision 4: organize ----------------------------------------------------

#[test]
fn organize_moves_to_creator_folder() {
    let dir = pkg_test_dir("organize_basic");
    write_sized_var(&dir.join("x").join("Qing.Hair.1.var"), 10);

    let (cands, _) = candidates_and_refs(&[&dir]);
    let res = plan_organize_by_creator(&cands);

    assert_eq!(res.actions.len(), 1);
    let a = &res.actions[0];
    assert_eq!(a.op, "move");
    assert_eq!(
        a.dest_path,
        dir.join("Qing").join("Qing.Hair.1.var").display().to_string(),
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn organize_leaves_correct_files() {
    let dir = pkg_test_dir("organize_correct");
    write_sized_var(&dir.join("Qing").join("Qing.Hair.1.var"), 10);

    let (cands, _) = candidates_and_refs(&[&dir]);
    let res = plan_organize_by_creator(&cands);

    assert!(res.actions.is_empty(), "already in the right place");
    assert_eq!(res.unchanged, 1);

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// The anchor is what stops a blanked VAM install: regrouping "under the root"
/// when the root is the VAM install dir would put every package where VAM never
/// looks, with nothing deleted and nothing to restore.
#[test]
fn organize_anchors_to_addon_packages() {
    let dir = pkg_test_dir("organize_anchor");
    let addon = dir.join("AddonPackages");
    write_sized_var(&addon.join("Qing.Hair.1.var"), 10);

    let (cands, _) = candidates_and_refs(&[&dir]);
    let res = plan_organize_by_creator(&cands);

    assert_eq!(res.actions.len(), 1);
    assert_eq!(
        res.actions[0].dest_path,
        addon.join("Qing").join("Qing.Hair.1.var").display().to_string(),
        "must stay inside AddonPackages, never <root>/Qing/",
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn organize_anchor_clamped_to_root() {
    let dir = pkg_test_dir("organize_clamp");
    let downloads = dir.join("AddonPackages").join("Downloads");
    write_sized_var(&downloads.join("Qing.Hair.1.var"), 10);

    // The user deliberately scanned only Downloads.
    let (cands, _) = candidates_and_refs(&[&downloads]);
    let res = plan_organize_by_creator(&cands);

    assert_eq!(res.actions.len(), 1);
    assert_eq!(
        res.actions[0].dest_path,
        downloads
            .join("Qing")
            .join("Qing.Hair.1.var")
            .display()
            .to_string(),
        "must not escape up into AddonPackages/<Creator>/",
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn organize_base_resolution() {
    assert_eq!(
        organize_base(Path::new("D:\\VAM"), Path::new("D:\\VAM\\AddonPackages\\x\\a.var")),
        PathBuf::from("D:\\VAM\\AddonPackages"),
    );
    // No AddonPackages anywhere -> the root itself.
    assert_eq!(
        organize_base(Path::new("D:\\Vars"), Path::new("D:\\Vars\\a.var")),
        PathBuf::from("D:\\Vars"),
    );
    // Root IS AddonPackages.
    assert_eq!(
        organize_base(
            Path::new("D:\\VAM\\AddonPackages"),
            Path::new("D:\\VAM\\AddonPackages\\sub\\a.var"),
        ),
        PathBuf::from("D:\\VAM\\AddonPackages"),
    );
    // Clamped: never resolves to an ancestor above the chosen root.
    assert_eq!(
        organize_base(
            Path::new("D:\\VAM\\AddonPackages\\Downloads"),
            Path::new("D:\\VAM\\AddonPackages\\Downloads\\a.var"),
        ),
        PathBuf::from("D:\\VAM\\AddonPackages\\Downloads"),
    );
}

#[test]
fn organize_reuses_existing_creator_casing() {
    let dir = pkg_test_dir("organize_casing");
    fs::create_dir_all(dir.join("Qing")).expect("existing folder");
    write_sized_var(&dir.join("qing.hgf1.1.var"), 10);

    let (cands, _) = candidates_and_refs(&[&dir]);
    let res = plan_organize_by_creator(&cands);

    assert_eq!(res.actions.len(), 1);
    assert!(
        res.actions[0].dest_path.contains("\\Qing\\"),
        "reuses the existing folder casing instead of making a colliding sibling: {}",
        res.actions[0].dest_path,
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// `dest.exists()` only sees the pre-apply disk, so it is blind to collisions
/// the plan itself creates.
#[test]
fn organize_detects_intra_plan_collision() {
    let dir = pkg_test_dir("organize_collision");
    write_sized_var(&dir.join("x").join("C.Pkg.1.var"), 10);
    write_sized_var(&dir.join("y").join("C.Pkg.1.var"), 20);

    let (cands, _) = candidates_and_refs(&[&dir]);
    let res = plan_organize_by_creator(&cands);

    assert_eq!(res.actions.len(), 1, "exactly one may claim the destination");
    assert!(
        res.notes.iter().any(|n| n.contains("Clean Duplicates")),
        "the other is explained and points at the fix: {:?}",
        res.notes,
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn organize_skips_existing_dest() {
    let dir = pkg_test_dir("organize_existing_dest");
    write_sized_var(&dir.join("x").join("C.Pkg.1.var"), 10);
    write_sized_var(&dir.join("C").join("C.Pkg.1.var"), 20);

    let (cands, _) = candidates_and_refs(&[&dir]);
    let res = plan_organize_by_creator(&cands);

    assert!(
        res.actions.is_empty(),
        "an occupied destination is never overwritten",
    );
    assert!(
        res.notes.iter().any(|n| n.contains("already exists")),
        "{:?}",
        res.notes
    );
    assert!(dir.join("x").join("C.Pkg.1.var").exists(), "source untouched");

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn organize_no_creator_untouched() {
    let dir = pkg_test_dir("organize_no_creator");
    write_sized_var(&dir.join("NoDot.var"), 10);

    let (cands, _) = candidates_and_refs(&[&dir]);
    let res = plan_organize_by_creator(&cands);

    assert!(res.actions.is_empty());
    assert!(
        res.notes.iter().any(|n| n.contains("no creator")),
        "{:?}",
        res.notes
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

// --- Executor ----------------------------------------------------------------

/// fs::rename is MoveFileExW with MOVEFILE_REPLACE_EXISTING and silently
/// replaces the destination — this guard is the only thing preventing data loss.
#[test]
fn apply_move_never_overwrites() {
    let dir = pkg_test_dir("apply_no_overwrite");
    let src = dir.join("src").join("C.Pkg.1.var");
    write_sized_var(&src, 10);
    let victim = dir.join("C").join("C.Pkg.1.var");
    write_sized_var(&victim, 5000);
    let victim_size = fs::metadata(&victim).unwrap().len();

    let meta = fs::metadata(&src).unwrap();
    let mut actions = vec![PackageAction {
        package_id: "C.Pkg.1".to_string(),
        file_path: src.display().to_string(),
        dest_path: victim.display().to_string(),
        size_bytes: meta.len(),
        op: "move".to_string(),
        reason: "organize".to_string(),
        status: "pending".to_string(),
        modified_ns: meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        ..Default::default()
    }];

    let ops = FakeFileOps::new();
    run_apply_package_actions(&mut actions, &ops, &no_cancel(), &mut |_, _| {});

    assert_eq!(actions[0].status, "skipped");
    assert!(ops.renamed.lock().unwrap().is_empty(), "no rename attempted");
    assert_eq!(
        fs::metadata(&victim).unwrap().len(),
        victim_size,
        "the destination file was NOT replaced",
    );
    assert!(src.exists(), "the source is still there");

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn apply_skips_changed_file() {
    let dir = pkg_test_dir("apply_toctou");
    let src = dir.join("C.Pkg.1.var");
    write_sized_var(&src, 10);
    let meta = fs::metadata(&src).unwrap();

    let mut actions = vec![PackageAction {
        package_id: "C.Pkg.1".to_string(),
        file_path: src.display().to_string(),
        // A size that no longer matches what is on disk.
        size_bytes: meta.len() + 999,
        op: "trash".to_string(),
        reason: "older_version".to_string(),
        status: "pending".to_string(),
        modified_ns: meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        ..Default::default()
    }];

    let ops = FakeFileOps::new();
    run_apply_package_actions(&mut actions, &ops, &no_cancel(), &mut |_, _| {});

    assert_eq!(actions[0].status, "skipped");
    assert!(actions[0].detail.contains("changed since preview"));
    assert!(ops.recycled.lock().unwrap().is_empty(), "never recycled");
    assert!(src.exists());

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn apply_moves_disabled_sidecar() {
    let dir = pkg_test_dir("apply_sidecar");
    let src = dir.join("Ash.Expr.2.var");
    write_sized_var(&src, 10);
    fs::write(disabled_sidecar(&src), b"").expect("sidecar");

    let (cands, _) = candidates_and_refs(&[&dir]);
    assert!(cands[0].disabled, "the sidecar is detected");
    let res = plan_organize_by_creator(&cands);
    let mut actions = res.actions.clone();

    let ops = FakeFileOps::new();
    run_apply_package_actions(&mut actions, &ops, &no_cancel(), &mut |_, _| {});

    assert_eq!(actions[0].status, "done");
    let moved = dir.join("Ash").join("Ash.Expr.2.var");
    assert!(moved.exists(), "the .var moved");
    assert!(
        disabled_sidecar(&moved).exists(),
        "its .disabled marker moved too — otherwise VAM silently re-enables it",
    );
    assert!(!disabled_sidecar(&src).exists(), "no orphan left behind");

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn image_sidecars_finds_jpg_jpeg_png() {
    let dir = pkg_test_dir("img_sidecar_helper");
    let src = dir.join("C.Pkg.1.var");
    write_sized_var(&src, 10);
    fs::write(dir.join("C.Pkg.1.jpg"), b"j").expect("jpg");
    fs::write(dir.join("C.Pkg.1.png"), b"p").expect("png");
    // A different stem must NOT match.
    fs::write(dir.join("C.Pkg.2.jpg"), b"x").expect("other");

    let mut names: Vec<String> = image_sidecars(&src)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(names, vec!["C.Pkg.1.jpg", "C.Pkg.1.png"]);

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// Organize must move the loose preview image with its VAR, or the image is
/// orphaned describing a package that is no longer there.
#[test]
fn organize_moves_image_sidecar() {
    let dir = pkg_test_dir("organize_img");
    let src = dir.join("x").join("Qing.Hair.1.var");
    write_sized_var(&src, 10);
    let src_img = dir.join("x").join("Qing.Hair.1.jpg");
    fs::write(&src_img, b"IMG").expect("img");

    let (cands, _) = candidates_and_refs(&[&dir]);
    let res = plan_organize_by_creator(&cands);
    let mut actions = res.actions.clone();

    let ops = FakeFileOps::new();
    run_apply_package_actions(&mut actions, &ops, &no_cancel(), &mut |_, _| {});

    assert_eq!(actions[0].status, "done");
    let moved_img = dir.join("Qing").join("Qing.Hair.1.jpg");
    assert!(moved_img.exists(), "the image moved beside its VAR");
    assert!(!src_img.exists(), "no orphan image left behind");
    assert_eq!(fs::read(&moved_img).unwrap(), b"IMG", "the moved image is intact");
    // The rename was recorded through the FileOps seam.
    assert!(
        ops.renamed
            .lock()
            .unwrap()
            .iter()
            .any(|(_, d)| d.ends_with("Qing.Hair.1.jpg")),
        "image move went through FileOps",
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// If an image already sits at the destination, never silently overwrite it.
#[test]
fn organize_skips_image_when_dest_exists() {
    let dir = pkg_test_dir("organize_img_collide");
    let src = dir.join("x").join("Qing.Hair.1.var");
    write_sized_var(&src, 10);
    let src_img = dir.join("x").join("Qing.Hair.1.jpg");
    fs::write(&src_img, b"SRC").expect("src img");
    // A pre-existing image at the destination folder.
    let dest_img = dir.join("Qing").join("Qing.Hair.1.jpg");
    fs::create_dir_all(dir.join("Qing")).expect("dest dir");
    fs::write(&dest_img, b"DEST").expect("dest img");

    let (cands, _) = candidates_and_refs(&[&dir]);
    let res = plan_organize_by_creator(&cands);
    let mut actions = res.actions.clone();

    let ops = FakeFileOps::new();
    run_apply_package_actions(&mut actions, &ops, &no_cancel(), &mut |_, _| {});

    assert_eq!(actions[0].status, "done", "the VAR still moves");
    assert_eq!(fs::read(&dest_img).unwrap(), b"DEST", "existing image untouched");
    assert!(src_img.exists(), "source image left in place, not clobbered");
    assert!(
        actions[0].detail.contains("preview image"),
        "the skip is noted: {}",
        actions[0].detail,
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// Recycling a duplicate VAR must also recycle its loose preview image.
#[test]
fn clean_duplicates_recycles_image_sidecar() {
    let dir = pkg_test_dir("dedup_img");
    write_sized_var(&dir.join("C.Pkg.1.var"), 10);
    write_sized_var(&dir.join("C.Pkg.2.var"), 10);
    // The loser (older .1) has a preview image beside it.
    fs::write(dir.join("C.Pkg.1.jpg"), b"IMG").expect("img");

    let (cands, refs) = candidates_and_refs(&[&dir]);
    let res = plan_clean_duplicates(&cands, &refs, &|_| RecycleSupport::Supported);
    let mut actions = res.actions.clone();

    let ops = FakeFileOps::new();
    run_apply_package_actions(&mut actions, &ops, &no_cancel(), &mut |_, _| {});

    let names = ops.recycled_names();
    assert!(names.contains(&"C.Pkg.1.var".to_string()), "the VAR recycled");
    assert!(
        names.contains(&"C.Pkg.1.jpg".to_string()),
        "its image recycled too, not orphaned: {names:?}",
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn apply_recycles_and_reports() {
    let dir = pkg_test_dir("apply_recycle");
    write_sized_var(&dir.join("C.Pkg.1.var"), 10);
    write_sized_var(&dir.join("C.Pkg.2.var"), 10);

    let res = plan_dupes(&[&dir]);
    let mut actions = res.actions.clone();
    let ops = FakeFileOps::new();
    let (reclaimed, cancelled) =
        run_apply_package_actions(&mut actions, &ops, &no_cancel(), &mut |_, _| {});

    assert!(!cancelled);
    assert_eq!(actions[0].status, "done");
    assert_eq!(ops.recycled_names(), vec!["C.Pkg.1.var".to_string()]);
    assert!(reclaimed > 0);
    assert!(!dir.join("C.Pkg.1.var").exists(), "the old version is gone");
    assert!(dir.join("C.Pkg.2.var").exists(), "the newest survives");

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// Selecting a row is the opt-in, whatever the planner labelled it. The
/// executor gates on `status != "pending"`, so a row left as "unverified" (or a
/// stale "protected") would be a SILENT no-op: ticked, applied, nothing happens,
/// no error. Regression guard for exactly that.
#[test]
fn apply_executes_a_selected_unverified_row() {
    let dir = pkg_test_dir("apply_unverified");
    let src = dir.join("C.Pkg.1.var");
    write_sized_var(&src, 10);
    let meta = fs::metadata(&src).unwrap();

    let mut actions = vec![PackageAction {
        package_id: "C.Pkg.1".to_string(),
        file_path: src.display().to_string(),
        size_bytes: meta.len(),
        op: "trash".to_string(),
        reason: "older_version".to_string(),
        // What run_apply_plan produces for a ticked unverified row.
        status: "pending".to_string(),
        modified_ns: meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        ..Default::default()
    }];

    let ops = FakeFileOps::new();
    run_apply_package_actions(&mut actions, &ops, &no_cancel(), &mut |_, _| {});

    assert_eq!(actions[0].status, "done", "a ticked row must actually run");
    assert_eq!(ops.recycled_names(), vec!["C.Pkg.1.var".to_string()]);

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// The one status selection may NOT override: recycling on a volume with no
/// Recycle Bin is a permanent delete, which contradicts the confirmed decision.
#[test]
fn apply_never_normalizes_blocked_to_pending() {
    let dir = pkg_test_dir("apply_blocked_stays");
    let src = dir.join("C.Pkg.1.var");
    write_sized_var(&src, 10);

    let mut actions = vec![PackageAction {
        package_id: "C.Pkg.1".to_string(),
        file_path: src.display().to_string(),
        size_bytes: fs::metadata(&src).unwrap().len(),
        op: "trash".to_string(),
        reason: "older_version".to_string(),
        status: "blocked".to_string(),
        ..Default::default()
    }];

    let ops = FakeFileOps::new();
    run_apply_package_actions(&mut actions, &ops, &no_cancel(), &mut |_, _| {});

    assert_eq!(actions[0].status, "blocked", "still blocked");
    assert!(ops.recycled.lock().unwrap().is_empty(), "never recycled");
    assert!(src.exists());

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// Normal scopes what Clean Duplicates may act on to the top-level files.
#[test]
fn normal_depth_scopes_dedup_to_top_level() {
    use crate::utils::ScanDepth;

    let dir = pkg_test_dir("depth_dedup_scope");
    // A duplicate pair at the top level, and an untouched pair in a subfolder.
    write_sized_var(&dir.join("Top.Pkg.1.var"), 10);
    write_sized_var(&dir.join("Top.Pkg.2.var"), 10);
    write_sized_var(&dir.join("sub").join("Nested.Pkg.1.var"), 10);
    write_sized_var(&dir.join("sub").join("Nested.Pkg.2.var"), 10);

    let (cands, refs) = candidates_and_refs_at(&[&dir], ScanDepth::Recursive);
    let deep = plan_clean_duplicates(&cands, &refs, &|_| RecycleSupport::Supported);
    let mut deep_ids: Vec<&str> = deep.actions.iter().map(|a| a.package_id.as_str()).collect();
    deep_ids.sort();
    assert_eq!(
        deep_ids,
        vec!["Nested.Pkg.1", "Top.Pkg.1"],
        "deep plans both families",
    );
    assert_eq!(deep.scanned, 4);

    let (cands, refs) = candidates_and_refs_at(&[&dir], ScanDepth::TopLevelOnly);
    let normal = plan_clean_duplicates(&cands, &refs, &|_| RecycleSupport::Supported);
    let normal_ids: Vec<&str> = normal.actions.iter().map(|a| a.package_id.as_str()).collect();
    assert_eq!(
        normal_ids,
        vec!["Top.Pkg.1"],
        "normal only plans the top-level family",
    );
    assert_eq!(normal.scanned, 2, "counts describe the in-scope set only");
    // (The "subfolders were left alone" note is added by gather_candidates in
    // the command layer, which the plan-level helper here bypasses — see
    // gather_candidates_notes_disclose_out_of_scope for that assertion.)

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// THE safety property of scoping by depth: Normal narrows what may be REMOVED,
/// never what the dependency check can SEE. VAM loads a subfolder scene whether
/// or not the user chose to browse subfolders, so its reference to a top-level
/// older version must still protect it from being recycled.
#[test]
fn normal_depth_still_honors_subfolder_referrers() {
    use crate::utils::ScanDepth;

    let dir = pkg_test_dir("depth_protection");
    write_sized_var(&dir.join("C.Morphs.1.var"), 10);
    write_sized_var(&dir.join("C.Morphs.2.var"), 10);
    // The only thing referencing Morphs.1 lives in a subfolder that Normal mode
    // does not list — but VAM still loads it.
    write_test_var_with_deps(
        &dir.join("sub").join("Other.Scene.1.var"),
        &["C.Morphs.1"],
        &[("Saves/scene/s.json", b"{}")],
    );

    let (cands, refs) = candidates_and_refs_at(&[&dir], ScanDepth::TopLevelOnly);

    // The subfolder scene is present as a candidate, just not actionable.
    let scene = cands
        .iter()
        .find(|c| c.package_id == "Other.Scene.1")
        .expect("the subfolder package is still gathered");
    assert!(!scene.in_scope, "it is out of scope for actions");

    let res = plan_clean_duplicates(&cands, &refs, &|_| RecycleSupport::Supported);
    let a = action_for(&res, "C.Morphs.1").expect("planned");
    assert_eq!(
        a.status, "protected",
        "a subfolder referrer must still protect a top-level version in Normal mode",
    );
    assert!(a.detail.contains("Other.Scene.1"), "detail: {}", a.detail);

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// Normal organize files the loose top-level packages away and leaves whatever
/// is already in subfolders alone.
#[test]
fn normal_depth_scopes_organize_to_top_level() {
    use crate::utils::ScanDepth;

    let dir = pkg_test_dir("depth_organize_scope");
    write_sized_var(&dir.join("Qing.Hair.1.var"), 10);
    write_sized_var(&dir.join("misc").join("Ash.Expr.1.var"), 10);

    let (cands, _) = candidates_and_refs_at(&[&dir], ScanDepth::Recursive);
    let deep = plan_organize_by_creator(&cands);
    assert_eq!(deep.actions.len(), 2, "deep files both");

    let (cands, _) = candidates_and_refs_at(&[&dir], ScanDepth::TopLevelOnly);
    let out = cands.iter().filter(|c| !c.in_scope).count();
    assert!(
        crate::packages::out_of_scope_note(out).is_some(),
        "the skip is disclosed when subfolder packages exist",
    );
    assert!(crate::packages::out_of_scope_note(0).is_none());
    let normal = plan_organize_by_creator(&cands);
    assert_eq!(normal.actions.len(), 1, "normal files only the loose one");
    assert_eq!(normal.actions[0].package_id, "Qing.Hair.1");
    assert_eq!(
        normal.actions[0].dest_path,
        dir.join("Qing").join("Qing.Hair.1.var").display().to_string(),
    );
    assert!(
        dir.join("misc").join("Ash.Expr.1.var").exists(),
        "the subfolder package is untouched",
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// The VAR Packages scan-depth toggle: deep walks subfolders, normal lists only
/// what sits directly in the folder.
#[test]
fn scan_depth_controls_subfolder_descent() {
    use crate::utils::{collect_var_files_multi_with_depth, ScanDepth};

    let dir = pkg_test_dir("scan_depth");
    write_sized_var(&dir.join("Top.Pkg.1.var"), 10);
    write_sized_var(&dir.join("sub").join("Nested.Pkg.1.var"), 10);
    write_sized_var(&dir.join("sub").join("deeper").join("Deep.Pkg.1.var"), 10);

    let roots = vec![dir.clone()];

    let deep = collect_var_files_multi_with_depth(&roots, ScanDepth::Recursive).expect("deep walk");
    let mut deep_names: Vec<String> = deep
        .iter()
        .map(|e| e.path.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    deep_names.sort();
    assert_eq!(
        deep_names,
        vec!["Deep.Pkg.1.var", "Nested.Pkg.1.var", "Top.Pkg.1.var"],
        "deep reaches every nesting level",
    );

    let top =
        collect_var_files_multi_with_depth(&roots, ScanDepth::TopLevelOnly).expect("top-level walk");
    let top_names: Vec<String> = top
        .iter()
        .map(|e| e.path.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        top_names,
        vec!["Top.Pkg.1.var"],
        "normal never descends into subfolders",
    );

    // The two modes must not share a cache entry, or switching would re-serve
    // the previous walk.
    assert_ne!(
        ScanDepth::Recursive.cache_token(),
        ScanDepth::TopLevelOnly.cache_token(),
    );
    // Absent flag = deep, preserving the behavior this page always had.
    assert_eq!(ScanDepth::from_deep(true), ScanDepth::Recursive);
    assert_eq!(ScanDepth::from_deep(false), ScanDepth::TopLevelOnly);

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// Every other walk in the app must stay recursive — the toggle is scoped to the
/// VAR Packages listing.
#[test]
fn default_walk_stays_recursive() {
    let dir = pkg_test_dir("depth_default");
    write_sized_var(&dir.join("Top.Pkg.1.var"), 10);
    write_sized_var(&dir.join("sub").join("Nested.Pkg.1.var"), 10);

    // The two entry points every non-VAR-Packages caller reaches.
    let found = crate::utils::collect_var_files_multi(std::slice::from_ref(&dir)).expect("walk");
    assert_eq!(found.len(), 2, "collect_var_files_multi still recurses");

    let found = crate::utils::collect_var_files_by_root(std::slice::from_ref(&dir)).expect("walk");
    assert_eq!(found.len(), 2, "collect_var_files_by_root still recurses");

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// `Path::starts_with` matches whole components, so a `\\`-prefix guard would
/// never fire. Pinned so nobody "simplifies" the volume probe into one.
#[test]
fn path_starts_with_is_component_matching() {
    assert!(
        !Path::new("\\\\NAS\\share\\a.var").starts_with("\\\\"),
        "a UNC string guard is a no-op — Recycle Bin support must be probed per volume",
    );
}

#[test]
fn migration_v11_creates_package_flags() {
    use crate::db;

    let db = db::open_in_memory().expect("open db");
    let conn = db.conn.lock().expect("lock db");
    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("read user_version");
    // The current version, not a literal: the point is that migrations ran to
    // completion and package_flags exists, not to pin the version number.
    assert_eq!(version, db::SCHEMA_VERSION);
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM package_flags", [], |r| r.get(0))
        .expect("count package_flags");
    assert_eq!(n, 0);
}

#[test]
fn package_flag_roundtrip_without_indexed_package() {
    use crate::db;

    let db = db::open_in_memory().expect("open db");
    let conn = db.conn.lock().expect("lock db");
    // Unknown id reads as none.
    assert_eq!(
        db::get_package_flag(&conn, "Acme.Scene.3").expect("get"),
        db::PACKAGE_FLAG_NONE
    );
    // Favoriting an id absent from `packages` must succeed (unindexed case).
    db::set_package_flag(&conn, "Acme.Scene.3", db::PACKAGE_FLAG_FAVORITE).expect("set");
    assert_eq!(
        db::get_package_flag(&conn, "Acme.Scene.3").expect("get"),
        db::PACKAGE_FLAG_FAVORITE
    );
    assert!(db::get_favorite_package_ids(&conn)
        .expect("list")
        .contains("Acme.Scene.3"));
    // Clearing removes the row (sparse table — "none" is row absence).
    db::set_package_flag(&conn, "Acme.Scene.3", db::PACKAGE_FLAG_NONE).expect("clear");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM package_flags", [], |r| r.get(0))
        .expect("count");
    assert_eq!(n, 0);
    // Out-of-range flags rejected.
    assert!(db::set_package_flag(&conn, "Acme.Scene.3", 7).is_err());
    assert!(db::set_package_flag(&conn, "Acme.Scene.3", -1).is_err());
}

#[test]
fn replacement_prefs_roundtrip() {
    use crate::db;

    let db = db::open_in_memory().expect("open db");
    let conn = db.conn.lock().expect("lock db");
    assert!(db::get_replacement_prefs(&conn).expect("list").is_empty());
    db::set_replacement_pref(&conn, "Good.Source.2", db::REPLACEMENT_PREFERRED).expect("prefer");
    db::set_replacement_pref(&conn, "Bad.Source.1", db::REPLACEMENT_AVOID).expect("avoid");
    // Separate from favorites: favoriting doesn't touch it.
    db::set_package_flag(&conn, "Good.Source.2", db::PACKAGE_FLAG_FAVORITE).expect("fav");
    assert_eq!(
        db::get_replacement_prefs(&conn).expect("list"),
        vec![("Bad.Source.1".to_string(), -1), ("Good.Source.2".to_string(), 1)]
    );
    // Switching and clearing.
    db::set_replacement_pref(&conn, "Bad.Source.1", db::REPLACEMENT_PREFERRED).expect("switch");
    db::set_replacement_pref(&conn, "Good.Source.2", db::REPLACEMENT_NONE).expect("clear");
    assert_eq!(db::get_replacement_prefs(&conn).expect("list"), vec![("Bad.Source.1".to_string(), 1)]);
    assert!(db::set_replacement_pref(&conn, "X.Y.1", 2).is_err());
    assert!(db::set_replacement_pref(&conn, "X.Y.1", -2).is_err());
}

#[test]
fn db_package_where_favorite_filter_matches_flagged_only() {
    use crate::db;
    use crate::models::VarPackageFilters;

    let db = db::open_in_memory().expect("open db");
    let conn = db.conn.lock().expect("lock db");
    db::upsert_package(&conn, "Acme.SceneA.1", "C:\\vars\\Acme.SceneA.1.var", 10, 0, None)
        .expect("seed a");
    db::upsert_package(&conn, "Beta.SceneB.1", "C:\\vars\\Beta.SceneB.1.var", 10, 0, None)
        .expect("seed b");
    db::set_package_flag(&conn, "Acme.SceneA.1", db::PACKAGE_FLAG_FAVORITE).expect("fav");

    let filters = VarPackageFilters {
        favorite: Some(true),
        ..Default::default()
    };
    let (where_sql, params) = crate::tasks::build_db_package_where(&None, &filters);
    let sql = format!("SELECT p.package_id FROM packages p{where_sql} ORDER BY p.package_id");
    let ids: Vec<String> = conn
        .prepare(&sql)
        .expect("prepare")
        .query_map(rusqlite::params_from_iter(params), |r| r.get::<_, String>(0))
        .expect("query")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("collect");
    assert_eq!(ids, vec!["Acme.SceneA.1".to_string()]);
}

// ----------------------------------------------------------------------------
// Delete Dependency Scan — the delete modal's "Scan dependencies" option.
// Each test asserts a classification decision (exclusive vs shared), the
// stated ordering, or a resilience behavior of run_delete_dependency_scan_task.
// ----------------------------------------------------------------------------

use crate::models::{DeleteDependencyItem, DeleteDependencyScanRequest, DeleteDependencyScanResponse};

/// Like `write_test_var_with_deps` but takes the raw `dependencies` object, so
/// tests can express VAM's nested (transitive) dependency trees — that helper
/// hardcodes empty nested maps.
fn write_test_var_with_dep_tree(
    var_path: &Path,
    deps: serde_json::Value,
    files: &[(&str, &[u8])],
) {
    if let Some(parent) = var_path.parent() {
        fs::create_dir_all(parent).expect("create var parent");
    }
    let meta = json!({
        "licenseType": "FC",
        "dependencies": deps,
        "contentList": files.iter().map(|(p, _)| p.to_string()).collect::<Vec<_>>(),
    });
    let writer = fs::File::create(var_path).expect("create var");
    let mut zip = ZipWriter::new(writer);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file("meta.json", options).expect("meta header");
    zip.write_all(serde_json::to_string_pretty(&meta).unwrap().as_bytes())
        .expect("meta body");
    for (path, contents) in files {
        zip.start_file(*path, options).expect("file header");
        zip.write_all(contents).expect("file body");
    }
    zip.finish().expect("finish var");
}

/// Drives the worker directly — progress writes into a task map with no
/// matching id are no-ops, so a fresh empty map is all the harness needs.
fn run_dep_scan_with_cancel(
    folder: &Path,
    extra: &[&Path],
    target: &Path,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> DeleteDependencyScanResponse {
    let tasks = Arc::new(Mutex::new(HashMap::new()));
    crate::tasks::run_delete_dependency_scan_task(
        &tasks,
        1,
        DeleteDependencyScanRequest {
            folder: folder.display().to_string(),
            additional_folders: extra.iter().map(|p| p.display().to_string()).collect(),
            target_var_path: target.display().to_string(),
        },
        cancel,
    )
    .expect("scan succeeds")
}

fn run_dep_scan(folder: &Path, extra: &[&Path], target: &Path) -> DeleteDependencyScanResponse {
    run_dep_scan_with_cancel(
        folder,
        extra,
        target,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
}

fn dep_row<'a>(res: &'a DeleteDependencyScanResponse, base: &str) -> &'a DeleteDependencyItem {
    res.dependencies
        .iter()
        .find(|d| d.package_base.eq_ignore_ascii_case(base))
        .unwrap_or_else(|| {
            panic!(
                "row {base} missing: {:?}",
                res.dependencies
                    .iter()
                    .map(|d| &d.package_base)
                    .collect::<Vec<_>>()
            )
        })
}

#[test]
fn dep_scan_classifies_exclusive_and_shared_and_orders() {
    let dir = pkg_test_dir("ddep_classify");
    let target = dir.join("T.Pkg.1.var");
    write_test_var_with_deps(&target, &["C.A.1", "C.B.1"], &[("Custom/x.bin", b"x")]);
    write_sized_var(&dir.join("C.A.1.var"), 10);
    write_sized_var(&dir.join("C.B.1.var"), 10);
    write_test_var_with_deps(
        &dir.join("Other.Scene.1.var"),
        &["C.A.1"],
        &[("Saves/scene/s.json", b"{}")],
    );

    let res = run_dep_scan(&dir, &[], &target);

    assert_eq!(res.dependencies.len(), 2, "{:?}", res.dependencies);
    let b = dep_row(&res, "C.B");
    assert!(b.exclusive);
    assert_eq!(b.used_by_total, 0);
    let a = dep_row(&res, "C.A");
    assert!(!a.exclusive);
    assert_eq!(a.used_by_total, 1);
    assert_eq!(a.used_by[0].package_id, "Other.Scene.1");
    assert!(!a.used_by[0].is_target_dependency);
    assert!(!a.used_by[0].is_target_family);
    // The stated ordering: exclusive B before shared A.
    assert_eq!(res.dependencies[0].package_base, "C.B");
    assert_eq!(res.dependencies[1].package_base, "C.A");
    assert!(res.usage_complete);

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn dep_scan_transitive_attribution_and_effective_exclusivity() {
    let dir = pkg_test_dir("ddep_transitive");
    let target = dir.join("T.Pkg.1.var");
    // The target's meta nests C.B.1 under C.A.1 — VAM's transitive tree.
    write_test_var_with_dep_tree(
        &target,
        json!({
            "C.A.1": {
                "licenseType": "FC",
                "dependencies": { "C.B.1": { "dependencies": {} } }
            }
        }),
        &[("Custom/x.bin", b"x")],
    );
    // On disk, C.A.1 itself declares C.B.1 top-level (as real packages do).
    write_test_var_with_deps(&dir.join("C.A.1.var"), &["C.B.1"], &[("Custom/a.bin", b"a")]);
    write_sized_var(&dir.join("C.B.1.var"), 10);

    let res = run_dep_scan(&dir, &[], &target);

    let a = dep_row(&res, "C.A");
    assert!(a.direct);
    let b = dep_row(&res, "C.B");
    assert!(!b.direct, "B is only reachable through the nested tree");
    assert_eq!(b.used_by_total, 1);
    assert_eq!(b.used_by[0].package_id, "C.A.1");
    assert!(
        b.used_by[0].is_target_dependency,
        "the UI's effective-exclusivity hook",
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn dep_scan_version_tolerance_and_all_local_files() {
    let dir = pkg_test_dir("ddep_versions");
    let target = dir.join("T.Pkg.1.var");
    write_test_var_with_deps(&target, &["C.D.latest"], &[("Custom/x.bin", b"x")]);
    write_sized_var(&dir.join("C.D.2.var"), 10);
    write_sized_var(&dir.join("C.D.3.var"), 10);
    write_test_var_with_deps(
        &dir.join("Other.Scene.1.var"),
        &["C.D.2"],
        &[("Saves/scene/s.json", b"{}")],
    );

    let res = run_dep_scan(&dir, &[], &target);

    let d = dep_row(&res, "C.D");
    assert_eq!(d.declared_ids, vec!["C.D.latest".to_string()]);
    let ids: Vec<&str> = d.local_files.iter().map(|f| f.package_id.as_str()).collect();
    assert_eq!(ids, vec!["C.D.3", "C.D.2"], "newest version first");
    assert_eq!(
        d.used_by_total, 1,
        "a versioned declaration counts for the base",
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn dep_scan_missing_dep_reported_not_installed() {
    let dir = pkg_test_dir("ddep_missing");
    let target = dir.join("T.Pkg.1.var");
    write_test_var_with_deps(&target, &["Gone.Pkg.1"], &[("Custom/x.bin", b"x")]);

    let res = run_dep_scan(&dir, &[], &target);

    let gone = dep_row(&res, "Gone.Pkg");
    assert!(gone.local_files.is_empty(), "not installed is still reported");
    assert!(gone.exclusive);

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// Cross-version family deps are the most important case (see
/// dedup_cross_version_dep_protects): the row must list the older sibling as a
/// deletable file while the target's own file stays excluded by path.
#[test]
fn dep_scan_family_dep_lists_older_version_not_target() {
    let dir = pkg_test_dir("ddep_family");
    let target = dir.join("C.P.3.var");
    write_test_var_with_deps(&target, &["C.P.2"], &[("Custom/x.bin", b"x")]);
    write_sized_var(&dir.join("C.P.2.var"), 10);

    let res = run_dep_scan(&dir, &[], &target);

    let p = dep_row(&res, "C.P");
    let ids: Vec<&str> = p.local_files.iter().map(|f| f.package_id.as_str()).collect();
    assert_eq!(ids, vec!["C.P.2"], "the target's own file is excluded by path");
    for d in &res.dependencies {
        assert!(
            d.used_by
                .iter()
                .all(|u| !u.package_id.eq_ignore_ascii_case("C.P.3")),
            "the target never appears as a user",
        );
    }

    fs::remove_dir_all(&dir).expect("cleanup");
}

/// Exact-file exclusion, not family-wide (the compute_protection stance): a
/// surviving sibling version still needs the shared dep and must count.
#[test]
fn dep_scan_surviving_sibling_counts_as_user() {
    let dir = pkg_test_dir("ddep_sibling");
    let target = dir.join("C.P.3.var");
    write_test_var_with_deps(&target, &["C.A.1"], &[("Custom/x.bin", b"x")]);
    write_test_var_with_deps(&dir.join("C.P.2.var"), &["C.A.1"], &[("Custom/y.bin", b"y")]);
    write_sized_var(&dir.join("C.A.1.var"), 10);

    let res = run_dep_scan(&dir, &[], &target);

    let a = dep_row(&res, "C.A");
    assert_eq!(a.used_by_total, 1, "exact-file exclusion, not family-wide");
    assert_eq!(a.used_by[0].package_id, "C.P.2");
    assert!(a.used_by[0].is_target_family);

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn dep_scan_unreadable_file_counts_scan_error_not_failure() {
    let dir = pkg_test_dir("ddep_unreadable");
    let target = dir.join("T.Pkg.1.var");
    write_test_var_with_deps(&target, &["C.A.1"], &[("Custom/x.bin", b"x")]);
    write_sized_var(&dir.join("C.A.1.var"), 10);
    // Not a zip; its stem also belongs to the dep family, so it must still be
    // listed as a deletable local file (stem needs no archive read).
    fs::write(dir.join("C.A.2.var"), b"not a zip").expect("broken var");

    let res = run_dep_scan(&dir, &[], &target);

    assert_eq!(res.scan_errors, 1);
    assert!(!res.usage_complete, "shared counts may under-count");
    let a = dep_row(&res, "C.A");
    let ids: Vec<&str> = a.local_files.iter().map(|f| f.package_id.as_str()).collect();
    assert_eq!(ids, vec!["C.A.2", "C.A.1"], "corrupt file still listed");

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn dep_scan_duplicate_dep_across_folders_reports_all_paths() {
    let dir = pkg_test_dir("ddep_dupes");
    let main = dir.join("main");
    let extra = dir.join("extra");
    let target = main.join("T.Pkg.1.var");
    write_test_var_with_deps(&target, &["C.A.1"], &[("Custom/x.bin", b"x")]);
    write_sized_var(&main.join("C.A.1.var"), 10);
    write_sized_var(&extra.join("C.A.1.var"), 10);

    let res = run_dep_scan(&main, &[&extra], &target);

    let a = dep_row(&res, "C.A");
    assert_eq!(
        a.local_files.len(),
        2,
        "same id across folders is legitimate — both listed",
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn dep_scan_disabled_sidecar_flagged() {
    let dir = pkg_test_dir("ddep_disabled");
    let target = dir.join("T.Pkg.1.var");
    write_test_var_with_deps(&target, &["C.A.1"], &[("Custom/x.bin", b"x")]);
    let dep = dir.join("C.A.1.var");
    write_sized_var(&dep, 10);
    fs::write(disabled_sidecar(&dep), b"").expect("sidecar");

    let res = run_dep_scan(&dir, &[], &target);

    let a = dep_row(&res, "C.A");
    assert!(a.local_files[0].disabled);

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn dep_scan_empty_dependencies_returns_empty() {
    let dir = pkg_test_dir("ddep_empty");
    let target = dir.join("T.Pkg.1.var");
    write_sized_var(&target, 10);
    write_sized_var(&dir.join("C.A.1.var"), 10);

    let res = run_dep_scan(&dir, &[], &target);

    assert!(res.dependencies.is_empty());
    assert!(res.usage_complete);
    assert!(!res.was_cancelled);

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn dep_scan_precancelled_returns_partial_with_flag() {
    let dir = pkg_test_dir("ddep_cancelled");
    let target = dir.join("T.Pkg.1.var");
    write_test_var_with_deps(&target, &["C.A.1"], &[("Custom/x.bin", b"x")]);
    write_sized_var(&dir.join("C.A.1.var"), 10);

    let res = run_dep_scan_with_cancel(
        &dir,
        &[],
        &target,
        Arc::new(std::sync::atomic::AtomicBool::new(true)),
    );

    assert!(res.was_cancelled);
    assert!(!res.usage_complete);
    // The dep rows are still reported — just without usage info, since the
    // walk was skipped.
    assert_eq!(res.dependencies.len(), 1);

    fs::remove_dir_all(&dir).expect("cleanup");
}

// --- Export Scene Images: scene image only, never a fallback ---------------
//
// The exporter used to fall back to "the largest embedded image" when a VAR had
// no scene image, which meant a clothing or hair pack silently got a side-car
// the user never asked for. These pin the scene-only contract.

#[test]
fn export_scene_image_writes_the_saves_scene_image() {
    let dir = tempdir_unique("export_scene_ok");
    let var_path = dir.join("C.Scene.1.var");
    write_test_var(
        &var_path,
        &[
            ("Saves/scene/Look.jpg", b"scene-bytes"),
            ("Custom/Clothing/Female/Thing.vam", b"vam"),
        ],
    );

    let res = crate::tasks::export_var_scene_image(var_path.display().to_string(), None)
        .expect("export");

    assert_eq!(res.status, "exported");
    let out = dir.join("C.Scene.1.jpg");
    assert!(out.is_file(), "side-car should exist");
    assert_eq!(fs::read(&out).expect("read side-car"), b"scene-bytes");

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn export_scene_image_never_falls_back_to_a_non_scene_image() {
    let dir = tempdir_unique("export_scene_no_fallback");
    let var_path = dir.join("C.Clothes.1.var");
    // A big non-scene image is exactly what the old fallback would have picked.
    write_test_var(
        &var_path,
        &[
            ("Custom/Clothing/Female/Look.vam", b"vam"),
            ("Custom/Clothing/Female/Look.jpg", &[7u8; 4096]),
        ],
    );

    let res = crate::tasks::export_var_scene_image(var_path.display().to_string(), None)
        .expect("export");

    assert_eq!(res.status, "no_image");
    assert!(res.output_path.is_none());
    assert!(
        !dir.join("C.Clothes.1.jpg").exists() && !dir.join("C.Clothes.1.png").exists(),
        "no side-car may be written for a VAR without a scene image",
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn export_scene_image_matches_saves_scene_case_insensitively() {
    let dir = tempdir_unique("export_scene_case");
    let var_path = dir.join("C.Scene.2.var");
    // Some VARs ship "Saves/Scene/". With the fallback gone, a case-sensitive
    // prefix would export nothing at all for them.
    write_test_var(&var_path, &[("Saves/Scene/s.png", b"png-bytes")]);

    let res = crate::tasks::export_var_scene_image(var_path.display().to_string(), None)
        .expect("export");

    assert_eq!(res.status, "exported");
    assert!(dir.join("C.Scene.2.png").is_file());

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn export_scene_image_keeps_an_existing_side_car_without_overwrite() {
    let dir = tempdir_unique("export_scene_exists");
    let var_path = dir.join("C.Scene.3.var");
    write_test_var(&var_path, &[("Saves/scene/s.jpg", b"new-bytes")]);
    let out = dir.join("C.Scene.3.jpg");
    fs::write(&out, b"old-bytes").expect("seed side-car");

    let res = crate::tasks::export_var_scene_image(var_path.display().to_string(), None)
        .expect("export");

    assert_eq!(res.status, "exists");
    assert_eq!(
        fs::read(&out).expect("read side-car"),
        b"old-bytes",
        "must not overwrite without the flag",
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn db_package_where_scene_image_filter_and_id_set_agree() {
    use crate::db;
    use crate::models::{ResourceRef, VarPackageFilters};

    // open_in_memory runs the migrations, so this also proves the v12 partial
    // index accepts SCENE_IMAGE_PREDICATE_SQL as its WHERE.
    let db = db::open_in_memory().expect("open db");
    let mut conn = db.conn.lock().expect("lock db");

    let seed = |conn: &rusqlite::Connection, id: &str| {
        db::upsert_package(conn, id, &format!("C:/vars/{id}.var"), 10, 0, None).expect("seed");
    };
    seed(&conn, "Acme.Scene.1");
    seed(&conn, "Beta.Clothes.1");
    seed(&conn, "Gama.CapsScene.1");

    let res = |id: &str, internal: &str| ResourceRef {
        package_id: id.to_string(),
        package_file: format!("C:/vars/{id}.var"),
        internal_path: internal.to_string(),
        crc32: None,
        size: 8,
        effective_size: 8,
    };
    db::replace_resources(&mut conn, "Acme.Scene.1", &[res("Acme.Scene.1", "Saves/scene/pic.jpg")])
        .expect("resources a");
    // A non-scene image must NOT count — that was the exporter's old fallback.
    db::replace_resources(
        &mut conn,
        "Beta.Clothes.1",
        &[res("Beta.Clothes.1", "Custom/Clothing/Female/big.png")],
    )
    .expect("resources b");
    // Case-insensitive prefix + extension, matching choose_scene_image.
    db::replace_resources(
        &mut conn,
        "Gama.CapsScene.1",
        &[res("Gama.CapsScene.1", "Saves/Scene/S.PNG")],
    )
    .expect("resources c");

    // Folder-mode id set.
    let ids = db::get_scene_image_package_ids(&conn).expect("id set");
    assert!(ids.contains("Acme.Scene.1"));
    assert!(ids.contains("Gama.CapsScene.1"));
    assert!(!ids.contains("Beta.Clothes.1"));

    // Database-mode WHERE, both directions — must agree with the id set.
    let run = |scene: &str| -> Vec<String> {
        let filters = VarPackageFilters {
            scene_image: Some(scene.to_string()),
            ..Default::default()
        };
        let (where_sql, params) = crate::tasks::build_db_package_where(&None, &filters);
        let sql = format!("SELECT p.package_id FROM packages p{where_sql} ORDER BY p.package_id");
        conn.prepare(&sql)
            .expect("prepare")
            .query_map(rusqlite::params_from_iter(params), |r| r.get::<_, String>(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect")
    };
    assert_eq!(run("with"), vec!["Acme.Scene.1", "Gama.CapsScene.1"]);
    assert_eq!(run("without"), vec!["Beta.Clothes.1"]);
}

// ----------------------------------------------------------------------------
// VAR Packages sorting — the Sort chip on the filter bar.
//
// The invariant these protect: folder mode (an in-memory comparator) and
// database mode (a SQL ORDER BY) must produce the SAME sequence, ties included,
// because the page is served by whichever mode is active and the order must not
// change when the user switches between them.
// ----------------------------------------------------------------------------

use crate::models::VarPackageListItem;
use crate::tasks::{
    compare_var_packages, parse_sort_desc, var_package_order_by_sql, VarPackageSort,
};

fn sort_item(package_id: &str, size_bytes: u64, modified_ms: Option<u64>) -> VarPackageListItem {
    VarPackageListItem {
        file_path: format!("C:\\vars\\{package_id}.var"),
        file_name: format!("{package_id}.var"),
        package_id: package_id.to_string(),
        creator: crate::naming::creator_from_package_id(package_id).map(str::to_string),
        size_bytes,
        modified_ms,
        indexed: true,
        ..VarPackageListItem::default()
    }
}

/// Seed set shared by both modes. Beta and Delta tie on BOTH size (100) and
/// mtime (500), so every tiebreak path is exercised.
fn sort_fixture() -> Vec<(&'static str, u64, u64)> {
    vec![
        ("Acme.Alpha.1", 300, 900),
        ("Beta.Bravo.1", 100, 500),
        ("Delta.Delta.1", 100, 500),
        ("Echo.Echo.1", 200, 100),
    ]
}

fn folder_order(sort: VarPackageSort, desc: bool) -> Vec<String> {
    let mut items: Vec<VarPackageListItem> = sort_fixture()
        .into_iter()
        .map(|(id, size, m)| sort_item(id, size, Some(m)))
        .collect();
    items.sort_by(|a, b| compare_var_packages(a, b, sort, desc));
    items.into_iter().map(|i| i.package_id).collect()
}

fn db_order(sort: VarPackageSort, desc: bool) -> Vec<String> {
    let db = db::open_in_memory().expect("open db");
    let conn = db.conn.lock().expect("lock db");
    for (id, size, m) in sort_fixture() {
        // modified_ns is nanoseconds; scaling the fixture's ms keeps the
        // relative order identical to what folder mode compares.
        db::upsert_package(
            &conn,
            id,
            &format!("C:\\vars\\{id}.var"),
            size,
            (m as u128) * 1_000_000,
            None,
        )
        .expect("seed");
    }
    let sql = format!(
        "SELECT p.package_id FROM packages p ORDER BY {}",
        var_package_order_by_sql(sort, desc)
    );
    // Bound to a local: the Statement borrows `conn`, so returning the chain
    // directly would drop `conn` while still borrowed.
    let ids: Vec<String> = conn
        .prepare(&sql)
        .expect("prepare")
        .query_map([], |r| r.get::<_, String>(0))
        .expect("query")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("collect");
    ids
}

#[test]
fn var_package_sort_parse_defaults_to_name() {
    assert_eq!(VarPackageSort::parse(None), VarPackageSort::Name);
    assert_eq!(VarPackageSort::parse(Some("")), VarPackageSort::Name);
    assert_eq!(VarPackageSort::parse(Some("nonsense")), VarPackageSort::Name);
    assert_eq!(VarPackageSort::parse(Some("size")), VarPackageSort::Size);
    assert_eq!(
        VarPackageSort::parse(Some("modified")),
        VarPackageSort::Modified
    );
    // Direction is opt-in: only an explicit "desc" descends.
    assert!(parse_sort_desc(Some("desc")));
    assert!(!parse_sort_desc(Some("asc")));
    assert!(!parse_sort_desc(None));
    assert!(!parse_sort_desc(Some("DROP TABLE packages")));
}

/// The reason the sort key is an enum: a hostile string can never reach the
/// SQL. It degrades to the default ordering, and the query still executes.
#[test]
fn var_package_sort_never_interpolates_request_text() {
    let hostile = "size; DROP TABLE packages --";
    let sql = var_package_order_by_sql(
        VarPackageSort::parse(Some(hostile)),
        parse_sort_desc(Some(hostile)),
    );
    assert!(!sql.contains("DROP"));
    assert!(!sql.contains(';'));
    assert_eq!(sql, "p.package_id COLLATE NOCASE ASC");

    let db = db::open_in_memory().expect("open db");
    let conn = db.conn.lock().expect("lock db");
    db::upsert_package(&conn, "Acme.A.1", "C:\\vars\\Acme.A.1.var", 1, 0, None).expect("seed");
    let ids: Vec<String> = conn
        .prepare(&format!("SELECT p.package_id FROM packages p ORDER BY {sql}"))
        .expect("prepare")
        .query_map([], |r| r.get::<_, String>(0))
        .expect("query")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("collect");
    assert_eq!(ids, vec!["Acme.A.1".to_string()]);
}

/// Size and Modified must carry a deterministic tiebreaker or rows duplicate
/// and vanish across LIMIT/OFFSET page boundaries. Modified must CAST, since
/// modified_ns is TEXT and manifest imports store the literal '0'.
#[test]
fn var_package_sort_sql_has_tiebreaker_and_casts_modified() {
    for desc in [false, true] {
        let size = var_package_order_by_sql(VarPackageSort::Size, desc);
        let modified = var_package_order_by_sql(VarPackageSort::Modified, desc);
        // Tiebreaker stays ASC in BOTH directions so it matches the comparator.
        assert!(size.ends_with(", p.package_id COLLATE NOCASE ASC"), "{size}");
        assert!(
            modified.ends_with(", p.package_id COLLATE NOCASE ASC"),
            "{modified}"
        );
        assert!(
            modified.contains("CAST(p.modified_ns AS INTEGER)"),
            "{modified}"
        );
    }
    // package_id is unique, so Name needs no tiebreaker.
    assert_eq!(
        var_package_order_by_sql(VarPackageSort::Name, true),
        "p.package_id COLLATE NOCASE DESC"
    );
}

#[test]
fn var_package_sort_orders_by_size_and_modified_both_directions() {
    assert_eq!(
        folder_order(VarPackageSort::Size, false),
        vec!["Beta.Bravo.1", "Delta.Delta.1", "Echo.Echo.1", "Acme.Alpha.1"]
    );
    assert_eq!(
        folder_order(VarPackageSort::Size, true),
        vec!["Acme.Alpha.1", "Echo.Echo.1", "Beta.Bravo.1", "Delta.Delta.1"]
    );
    // "Newest first" is just Modified/desc — there is no separate key.
    assert_eq!(
        folder_order(VarPackageSort::Modified, true),
        vec!["Acme.Alpha.1", "Beta.Bravo.1", "Delta.Delta.1", "Echo.Echo.1"]
    );
    assert_eq!(
        folder_order(VarPackageSort::Name, true),
        vec!["Echo.Echo.1", "Delta.Delta.1", "Beta.Bravo.1", "Acme.Alpha.1"]
    );
}

/// Beta and Delta tie on size and mtime. The tiebreak must stay ASCENDING even
/// when the primary key descends, or the two modes disagree and paging is
/// unstable.
#[test]
fn var_package_sort_ties_break_ascending_in_both_directions() {
    for desc in [false, true] {
        for sort in [VarPackageSort::Size, VarPackageSort::Modified] {
            let order = folder_order(sort, desc);
            let beta = order.iter().position(|id| id == "Beta.Bravo.1").unwrap();
            let delta = order.iter().position(|id| id == "Delta.Delta.1").unwrap();
            assert!(beta < delta, "sort={sort:?} desc={desc} order={order:?}");
        }
    }
}

/// The cross-mode invariant. If this fails, switching between Folder and
/// Database mode silently reorders the library.
#[test]
fn var_package_sort_folder_and_database_modes_agree() {
    for sort in [
        VarPackageSort::Name,
        VarPackageSort::Size,
        VarPackageSort::Modified,
    ] {
        for desc in [false, true] {
            assert_eq!(
                folder_order(sort, desc),
                db_order(sort, desc),
                "sort={sort:?} desc={desc}"
            );
        }
    }
}

/// A file with no mtime sorts oldest rather than floating to an arbitrary
/// position. SQL agrees, because such rows carry modified_ns '0'.
#[test]
fn var_package_sort_missing_modified_sorts_oldest() {
    let mut items = [
        sort_item("Acme.Has.1", 10, Some(500)),
        sort_item("Beta.None.1", 10, None),
    ];
    items.sort_by(|a, b| compare_var_packages(a, b, VarPackageSort::Modified, false));
    assert_eq!(items[0].package_id, "Beta.None.1");
    items.sort_by(|a, b| compare_var_packages(a, b, VarPackageSort::Modified, true));
    assert_eq!(items[0].package_id, "Acme.Has.1");
}

// ----------------------------------------------------------------------------
// VAR Packages library: classifier, dependency graph, search
// ----------------------------------------------------------------------------

fn lib_names(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|p| p.to_string()).collect()
}

#[test]
fn library_classifier_types_and_dedup() {
    use crate::library::classify_entries;
    let items = classify_entries(&lib_names(&[
        "Saves/scene/My Scene.json",
        "Saves/scene/My Scene.jpg",
        "Custom/Clothing/Female/Me/Dress/Dress.vam",
        "Custom/Clothing/Female/Me/Dress/Dress.vaj",
        "Custom/Clothing/Female/Me/Dress/Dress.vab",
        "Custom/Atom/Person/Clothing/Preset_Dress.vap",
        "Custom/Atom/Person/Clothing/Preset_Dress.jpg",
        "Custom/Hair/Female/Me/Bob/Bob.vam",
        "Custom/Atom/Person/Morphs/female/Me/Smile.vmi",
        "Custom/Atom/Person/Morphs/female/Me/Smile.vmb",
        "Custom/Atom/Person/Textures/FullBody/skin.png",
        "Custom/Scripts/Me/Plugin.cs",
        "Custom/Atom/Person/Appearance/Preset_Look.vap.disabled",
    ]));
    let count = |fine: &str| items.iter().filter(|i| i.fine == fine).count();
    assert_eq!(count("scene"), 1);
    // .vam/.vaj/.vab collapse to one item, and the item/preset pair named
    // "Dress" collapses to the preset, which has the thumbnail.
    assert_eq!(count("clothingItem"), 0);
    assert_eq!(count("clothingPreset"), 1);
    assert_eq!(count("hairItem"), 1);
    assert_eq!(count("morphBinary"), 1, "vmi/vmb pair is one morph");
    assert_eq!(count("texture"), 1);
    assert_eq!(count("pluginScript"), 1);
    assert_eq!(count("look"), 1, ".disabled suffix is still content");
    let scene = items.iter().find(|i| i.fine == "scene").unwrap();
    assert_eq!(scene.thumb.as_deref(), Some("Saves/scene/My Scene.jpg"));
    let preset = items.iter().find(|i| i.fine == "clothingPreset").unwrap();
    assert_eq!(preset.name, "Dress");
    assert_eq!(preset.category, Some("clothing"));
}

#[test]
fn library_primary_type_precedence() {
    use crate::library::primary_type;
    let mut counts = std::collections::BTreeMap::new();
    counts.insert("clothing".to_string(), 30);
    counts.insert("scene".to_string(), 1);
    assert_eq!(primary_type(&counts), "scene");
    counts.clear();
    counts.insert("subscene".to_string(), 2);
    assert_eq!(primary_type(&counts), "other", "subscenes alone are Other");
}

#[test]
fn library_read_info_from_archive() {
    let dir = std::env::temp_dir().join(format!("vam_lib_info_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let var = dir.join("Me.Look.3.var");
    write_test_var_with_deps(
        &var,
        &["Other.Base.2", "Other.Tex.latest"],
        &[
            ("Custom/Atom/Person/Appearance/Preset_Me.vap", b"{}"),
            ("Custom/Atom/Person/Appearance/Preset_Me.jpg", b"jpg"),
            ("Saves/scene/Demo.json", b"{}"),
        ],
    );
    let info = crate::library::read_var_pkg_info(&var);
    assert!(info.readable);
    assert_eq!(info.pkg_type, "scene");
    assert_eq!(info.item_count, 2);
    assert_eq!(info.license.as_deref(), Some("FC"));
    assert_eq!(info.deps.len(), 2);
    assert_eq!(
        info.thumb_entry.as_deref(),
        Some("Custom/Atom/Person/Appearance/Preset_Me.jpg"),
        "scene has no image, so the look's thumbnail is used"
    );
    fs::remove_dir_all(&dir).expect("cleanup");
}

fn lib_item(package_id: &str, deps: &[&str]) -> VarPackageListItem {
    VarPackageListItem {
        file_path: format!("C:\\vars\\{package_id}.var"),
        file_name: format!("{package_id}.var"),
        package_id: package_id.to_string(),
        creator: crate::naming::creator_from_package_id(package_id).map(str::to_string),
        deps: deps.iter().map(|d| d.to_string()).collect(),
        ..VarPackageListItem::default()
    }
}

#[test]
fn replacement_source_statuses_and_counts() {
    use crate::library::{compute_facets, status_matches, PackageMarks};
    use crate::models::VarPackageFilters;

    let items = vec![lib_item("Good.Source.1", &[]), lib_item("Bad.Source.1", &[]), lib_item("Plain.Pkg.1", &[])];
    let mut marks = PackageMarks::default();
    marks.replacement.insert("Good.Source.1".into(), 1);
    marks.replacement.insert("Bad.Source.1".into(), -1);
    marks.favorites.insert("Plain.Pkg.1".into());

    assert!(status_matches(&items[0], "preferred_source", &marks));
    assert!(!status_matches(&items[0], "avoided_source", &marks));
    assert!(status_matches(&items[1], "avoided_source", &marks));
    assert!(!status_matches(&items[2], "preferred_source", &marks));
    assert!(status_matches(&items[2], "favorites", &marks));

    let base: Vec<&VarPackageListItem> = items.iter().collect();
    let facets = compute_facets(&items, &base, &VarPackageFilters::default(), &marks, 0);
    assert_eq!(facets.statuses.get("preferred_source"), Some(&1));
    assert_eq!(facets.statuses.get("avoided_source"), Some(&1));
    assert_eq!(facets.statuses.get("favorites"), Some(&1));
}

#[test]
fn library_dependency_graph() {
    let mut items = vec![
        lib_item("A.Scene.1", &["B.Look.2", "C.Hair.latest", "D.Gone.1", "E.Old.min5"]),
        lib_item("B.Look.1", &[]),
        lib_item("B.Look.2", &[]),
        lib_item("C.Hair.4", &[]),
        lib_item("E.Old.3", &[]),
    ];
    crate::library::apply_graph(&mut items);
    let get = |id: &str| items.iter().find(|i| i.package_id == id).unwrap();
    // D.Gone is absent entirely; E.Old.min5 only has v3 (a fallback, not missing).
    assert_eq!(get("A.Scene.1").missing_dep_count, 1);
    assert_eq!(get("B.Look.2").used_by_count, 1);
    assert_eq!(get("B.Look.1").used_by_count, 0);
    assert_eq!(get("C.Hair.4").used_by_count, 1);
    assert_eq!(get("E.Old.3").used_by_count, 1);
    assert!(get("B.Look.1").newer_version);
    assert!(!get("B.Look.2").newer_version);
}

#[test]
fn library_dependency_bytes() {
    let sized = |id: &str, deps: &[&str], size: u64| VarPackageListItem { size_bytes: size, ..lib_item(id, deps) };
    let mut items = vec![
        // C.Hair named twice (two versions) counts once; D.Gone isn't installed.
        sized("A.Scene.1", &["B.Look.2", "C.Hair.latest", "C.Hair.4", "D.Gone.1"], 1),
        sized("B.Look.2", &[], 300),
        sized("C.Hair.4", &[], 40),
    ];
    crate::library::apply_graph(&mut items);
    assert_eq!(items[0].dep_bytes, 340);
    assert_eq!(items[1].dep_bytes, 0);
}

#[test]
fn library_dependency_resolution_specs() {
    use crate::library::{DepResolution, LibIndex};
    let items = vec![lib_item("X.Pkg.2", &[]), lib_item("X.Pkg.7", &[])];
    let index = LibIndex::build(&items);
    assert_eq!(index.resolve("X.Pkg.2"), DepResolution::Found(0));
    assert_eq!(index.resolve("x.pkg.LATEST"), DepResolution::Found(1));
    assert_eq!(index.resolve("X.Pkg.min3"), DepResolution::Found(1));
    assert_eq!(index.resolve("X.Pkg.min9"), DepResolution::OtherVersion(1));
    assert_eq!(index.resolve("X.Pkg.5"), DepResolution::OtherVersion(1));
    assert_eq!(index.resolve("Y.Pkg.1"), DepResolution::Missing);
}

#[test]
fn library_search_query_terms() {
    use crate::library::SearchQuery;
    let mut item = lib_item("MacGruber.LongHair.3", &[]);
    item.pkg_type = "hair".to_string();
    let q = |s: &str| SearchQuery::parse(Some(s)).unwrap();
    assert!(q("@macgruber hair").matches(&item));
    assert!(!q("@macgruber -hair").matches(&item));
    assert!(q("long type:hair").matches(&item));
    assert!(!q("@someone").matches(&item));
    assert!(SearchQuery::parse(Some("   ")).is_none());
}

#[test]
fn inspect_vam_dir_accepts_vam_root_or_addonpackages() {
    let root = std::env::temp_dir().join(format!("vam_dir_check_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let addon = root.join("AddonPackages");
    fs::create_dir_all(addon.join("Creator")).unwrap();
    fs::write(addon.join("A.B.1.var"), b"x").unwrap();
    fs::write(addon.join("Creator").join("C.D.2.var"), b"x").unwrap();
    fs::write(addon.join("notes.txt"), b"x").unwrap();

    let info = crate::library::inspect_vam_dir(root.display().to_string());
    assert!(info.valid);
    assert_eq!(info.var_count, 2);
    assert_eq!(info.vam_dir, root.display().to_string());

    // Picking AddonPackages itself resolves to the VaM directory.
    let picked = crate::library::inspect_vam_dir(addon.display().to_string());
    assert!(picked.valid);
    assert_eq!(picked.vam_dir, root.display().to_string());

    let bad = crate::library::inspect_vam_dir(root.join("Creator").display().to_string());
    assert!(!bad.valid);
    assert_eq!(bad.var_count, 0);
    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn hub_detail_must_be_the_requested_family() {
    use crate::hub::detail_matches_family;
    let own = json!({
        "resource_id": "123",
        "hubFiles": [{ "filename": "MacGruber.Life.13.var", "file_size": "100" }],
    });
    assert!(detail_matches_family(&own, "macgruber.life"));
    // A resource that merely depends on the package is not the package.
    let user = json!({
        "resource_id": "999",
        "hubFiles": [{ "filename": "Someone.Scene.2.var" }],
    });
    assert!(!detail_matches_family(&user, "macgruber.life"));
    // Paid multi-var listings put the family under `dependencies`.
    let paid = json!({
        "resource_id": "555",
        "hubFiles": [],
        "dependencies": { "Creator.Pack.4": {} },
    });
    assert!(detail_matches_family(&paid, "creator.pack"));
}

#[test]
fn hub_body_matches_vam_format() {
    let mut params = serde_json::Map::new();
    params.insert("perpage".to_string(), json!("60"));
    params.insert("search".to_string(), json!("say \"hi\""));
    params.insert("action".to_string(), json!("ignored"));
    let body = crate::hub::vam_body("getResources", &params);
    assert_eq!(
        body,
        r#"{"source":"VaM", "action":"getResources", "perpage":"60", "search":"say \"hi\""}"#
    );
    let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    assert_eq!(v["action"], "getResources");
}

#[test]
fn migration_creates_hub_wishlist_table() {
    let db = crate::db::open_in_memory().expect("db");
    let conn = db.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO hub_wishlist (resource_id, snapshot, created_at) VALUES ('94', '{}', 1)",
        [],
    )
    .expect("insert");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM hub_wishlist", [], |r| r.get(0))
        .expect("count");
    assert_eq!(n, 1);
}

#[test]
fn local_package_ids_walk_roots_recursively() {
    let root = std::env::temp_dir().join(format!("vam_local_ids_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("Creator")).unwrap();
    fs::write(root.join("A.B.1.var"), b"x").unwrap();
    fs::write(root.join("Creator").join("C.D.2.VAR"), b"x").unwrap();
    fs::write(root.join("Creator").join("readme.txt"), b"x").unwrap();
    let ids = crate::library::list_local_package_ids(vec![root.display().to_string(), String::new()]);
    assert_eq!(ids, vec!["a.b.1".to_string(), "c.d.2".to_string()]);
    fs::remove_dir_all(&root).expect("cleanup");
}

fn offload_item(package_id: &str, deps: &[&str], offloaded: bool) -> VarPackageListItem {
    let dir = if offloaded { "AddonPackages_offload" } else { "AddonPackages" };
    VarPackageListItem {
        file_path: format!(r"C:\VaM\{dir}\{package_id}.var"),
        offloaded,
        ..lib_item(package_id, deps)
    }
}

#[test]
fn offload_plan_leaves_shared_dependencies_unselected() {
    let items = vec![
        offload_item("A.Scene.1", &["B.Look.1", "C.Hair.1", "E.Mid.1", "G.Off.1", "M.Gone.1"], false),
        offload_item("B.Look.1", &["D.Tex.1"], false),
        offload_item("C.Hair.1", &[], false),
        offload_item("D.Tex.1", &[], false),
        offload_item("E.Mid.1", &["F.Base.1"], false),
        offload_item("F.Base.1", &[], false),
        offload_item("G.Off.1", &[], true),
        // Stay in AddonPackages and use C directly / F through E.
        offload_item("X.Scene.1", &["C.Hair.1"], false),
        offload_item("W.Scene.1", &["E.Mid.1"], false),
    ];
    let plan = crate::offload::build_plan(&items, &[0], crate::offload::PlanMode::Offload);
    let dep = |id: &str| plan.deps.iter().find(|d| d.package_id == id).unwrap();
    assert!(plan.targets[0].default_selected);
    assert!(dep("B.Look.1").default_selected && dep("B.Look.1").direct);
    assert!(dep("D.Tex.1").default_selected && !dep("D.Tex.1").direct, "only A needs it, via B");
    assert!(!dep("C.Hair.1").default_selected);
    assert_eq!(dep("C.Hair.1").used_by, vec!["X.Scene.1".to_string()]);
    assert!(!dep("E.Mid.1").default_selected);
    assert!(!dep("F.Base.1").default_selected, "E stays in AddonPackages and needs it");
    assert!(dep("F.Base.1").used_by.is_empty());
    assert!(!dep("G.Off.1").movable && !dep("G.Off.1").default_selected);
    assert_eq!(plan.missing, vec!["M.Gone.1".to_string()]);
}

#[test]
fn restore_plan_brings_back_offloaded_dependencies() {
    let items = vec![
        offload_item("A.Scene.1", &["B.Look.1", "C.Hair.1"], true),
        offload_item("B.Look.1", &[], true),
        offload_item("C.Hair.1", &[], false),
    ];
    let plan = crate::offload::build_plan(&items, &[0], crate::offload::PlanMode::Restore);
    let dep = |id: &str| plan.deps.iter().find(|d| d.package_id == id).unwrap();
    assert!(plan.targets[0].movable && plan.targets[0].default_selected);
    assert!(dep("B.Look.1").default_selected);
    assert!(!dep("C.Hair.1").movable, "already in AddonPackages");
}

#[test]
fn offload_relative_paths_ignore_case_and_separators() {
    use crate::offload::{path_is_under, relative_to};
    assert_eq!(
        relative_to(Path::new(r"D:\VaM\AddonPackages\Sub\A.B.1.var"), Path::new("d:/vam/addonpackages/")),
        Some(PathBuf::from(r"Sub\A.B.1.var"))
    );
    assert!(!path_is_under(Path::new(r"D:\VaM\AddonPackages_offload\A.B.1.var"), Path::new(r"D:\VaM\AddonPackages")));
    assert!(!path_is_under(Path::new(r"D:\VaM\AddonPackages"), Path::new(r"D:\VaM\AddonPackages")));
}

#[test]
fn offload_moves_into_creator_folders_and_back() {
    use crate::offload::move_package;
    let root = std::env::temp_dir().join(format!("vam_offload_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let addon = root.join("AddonPackages");
    let offload = root.join("AddonPackages_offload");
    fs::create_dir_all(addon.join("Downloads")).unwrap();
    let src = addon.join("Downloads").join("Qing.Hair.1.var");
    fs::write(&src, b"var").unwrap();
    fs::write(addon.join("Downloads").join("Qing.Hair.1.var.disabled"), b"").unwrap();
    fs::write(addon.join("Downloads").join("Qing.Hair.1.jpg"), b"img").unwrap();

    let (dest, notes) = move_package(&src, &addon, &offload, true, false).expect("offload");
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(dest, offload.join("Qing").join("Qing.Hair.1.var"));
    assert!(dest.is_file() && !src.exists());
    assert!(offload.join("Qing").join("Qing.Hair.1.var.disabled").exists());
    assert!(offload.join("Qing").join("Qing.Hair.1.jpg").exists());

    // Not by creator: keeps its path relative to the folder it leaves.
    let (back, _) = move_package(&dest, &offload, &addon, false, true).expect("restore");
    assert_eq!(back, addon.join("Qing").join("Qing.Hair.1.var"));
    assert!(back.is_file());

    // Never overwrites.
    fs::create_dir_all(offload.join("Qing")).unwrap();
    fs::write(offload.join("Qing").join("Qing.Hair.1.var"), b"other").unwrap();
    assert!(move_package(&back, &addon, &offload, true, false).is_err());
    assert!(back.is_file());
    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn known_package_lookups_use_only_the_ids_asked_for() {
    use crate::tasks::{known_package_ids_among, known_package_in_family};
    let db = crate::db::open_in_memory().expect("db");
    let conn = db.conn.lock().unwrap();
    for (i, id) in ["Acid.Look.2", "Acid.LookPack.1", "Bee.Hair.latest", "Cat.Scene.3"].iter().enumerate() {
        conn.execute(
            "INSERT INTO packages (package_id, file_path, size_bytes, modified_ns, last_scanned_at) \
             VALUES (?1, ?2, 1, '0', 0)",
            rusqlite::params![id, format!("f{i}.var")],
        )
        .expect("insert");
    }
    let known = known_package_ids_among(&conn, ["Acid.Look.2", "Acid.Look.3", "Cat.Scene.3"]).unwrap();
    assert_eq!(known.len(), 2);
    assert!(known.contains("Acid.Look.2") && known.contains("Cat.Scene.3"));

    assert_eq!(known_package_in_family(&conn, "Acid.Look").unwrap().as_deref(), Some("Acid.Look.2"));
    // A different family that merely shares the prefix doesn't count.
    assert_eq!(known_package_in_family(&conn, "Acid.LookP").unwrap(), None);
    assert_eq!(known_package_in_family(&conn, "acid.look").unwrap().as_deref(), Some("Acid.Look.2"));
    assert_eq!(known_package_in_family(&conn, "Bee.Hair").unwrap().as_deref(), Some("Bee.Hair.latest"));
    assert_eq!(known_package_in_family(&conn, "Dog.Pose").unwrap(), None);
    let plan: String = conn
        .query_row(
            "EXPLAIN QUERY PLAN SELECT package_id FROM packages \
             WHERE lower(package_id) >= 'a.' AND lower(package_id) < 'a/'",
            [],
            |row| row.get(3),
        )
        .expect("plan");
    assert!(plan.contains("idx_packages_id_lower"), "{plan}");
}

#[test]
fn offload_prunes_folders_left_empty_but_never_the_root() {
    use crate::packages::prune_empty_dirs;
    let root = std::env::temp_dir().join(format!("vam_prune_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let addon = root.join("AddonPackages");
    fs::create_dir_all(addon.join("Qing").join("Sub")).unwrap();
    fs::create_dir_all(addon.join("Kept")).unwrap();
    fs::write(addon.join("Kept").join("Other.Pkg.1.var"), b"x").unwrap();
    fs::create_dir_all(addon.join("Empty")).unwrap();

    prune_empty_dirs(
        vec![addon.join("Qing").join("Sub"), addon.join("Kept"), addon.clone()],
        &addon,
    );
    assert!(!addon.join("Qing").exists(), "Sub and then Qing became empty");
    assert!(addon.join("Kept").exists(), "still holds a package");
    assert!(addon.join("Empty").exists(), "never asked about it");
    assert!(addon.exists(), "the root itself always stays");

    // Even when the root ends up empty.
    let lone = root.join("Offload");
    fs::create_dir_all(lone.join("A")).unwrap();
    prune_empty_dirs(vec![lone.join("A")], &lone);
    assert!(!lone.join("A").exists() && lone.exists());
    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn prune_root_is_the_scan_folder_holding_the_file() {
    use crate::packages::prune_root_for;
    let roots = vec![PathBuf::from("D:/VaM/AddonPackages"), PathBuf::from("D:/VaM/AddonPackages/Downloads")];
    // The deepest root that holds it wins.
    assert_eq!(
        prune_root_for(Path::new("D:/VaM/AddonPackages/Downloads/Qing/A.B.1.var"), &roots),
        Some(PathBuf::from("D:/VaM/AddonPackages/Downloads"))
    );
    // No root given: the nearest AddonPackages folder.
    assert_eq!(
        prune_root_for(Path::new("E:/Games/VaM/AddonPackages/Qing/A.B.1.var"), &[]),
        Some(PathBuf::from("E:/Games/VaM/AddonPackages"))
    );
    // Under nothing anyone chose: prune nothing.
    assert_eq!(prune_root_for(Path::new("C:/Users/me/Desktop/A.B.1.var"), &[]), None);
}

#[test]
fn deleting_the_last_package_removes_its_creator_folder() {
    use crate::packages::prune_after_removal;
    let root = std::env::temp_dir().join(format!("vam_prune_del_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let addon = root.join("AddonPackages");
    fs::create_dir_all(addon.join("Qing")).unwrap();
    let var = addon.join("Qing").join("Qing.Hair.1.var");
    fs::write(&var, b"x").unwrap();
    fs::remove_file(&var).unwrap(); // what the Recycle Bin does
    prune_after_removal(&var, &[]);
    assert!(!addon.join("Qing").exists());
    assert!(addon.exists());
    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn download_links_normalize_pixeldrain_share_pages() {
    use crate::sources::normalize_link;
    assert_eq!(
        normalize_link(" https://pixeldrain.com/u/AbC123xY "),
        Some(("https://pixeldrain.com/api/file/AbC123xY?download".to_string(), "pixeldrain"))
    );
    assert_eq!(
        normalize_link("https://pixeldrain.com/api/file/AbC123xY?download").map(|(u, _)| u),
        Some("https://pixeldrain.com/api/file/AbC123xY?download".to_string())
    );
    assert_eq!(normalize_link("https://www.mediafire.com/file/x/A.B.1.var/file").map(|(_, h)| h), Some("mediafire"));
    assert_eq!(normalize_link("https://example.com/A.B.1.var").map(|(_, h)| h), Some("other"));
    assert_eq!(normalize_link("pixeldrain.com/u/abc"), None, "not http(s)");

    // Imported lists get the same treatment.
    let row = crate::tasks::parse_link_line("Acid.Look.3.var https://pixeldrain.com/u/Zz9").unwrap();
    assert_eq!(row.url, "https://pixeldrain.com/api/file/Zz9?download");
    assert_eq!((row.package_base.as_str(), row.version), ("acid.look", Some(3)));
}

#[test]
fn download_link_inspect_reads_the_file_name_from_a_direct_url() {
    let info = crate::sources::inspect_link("https://files.example.com/vars/Acid.My%20Look.3.var?x=1");
    assert_eq!(info.filename.as_deref(), Some("Acid.My Look.3.var"));
    assert_eq!(info.host, "other");
    assert!(info.error.is_none());
    let bad = crate::sources::inspect_link("ftp://x");
    assert!(bad.error.is_some());
}

#[test]
fn stored_link_for_the_exact_version_wins() {
    let db = crate::db::open_in_memory().expect("db");
    let rows: Vec<_> = [
        "Acid.Look.5.var https://pixeldrain.com/u/five",
        "Acid.Look.3.var https://example.com/Acid.Look.3.var",
    ]
    .iter()
    .map(|l| crate::tasks::parse_link_line(l).unwrap())
    .collect();
    crate::db::insert_download_links(&db, &rows).unwrap();
    let pick = |want| crate::tasks::pick_db_link(&db, "acid.look", want).unwrap().filename;
    assert_eq!(pick(Some(3)), "Acid.Look.3.var", "exact version, even though not Pixeldrain");
    assert_eq!(pick(None), "Acid.Look.5.var", ".latest: Pixeldrain, newest");
    assert_eq!(pick(Some(9)), "Acid.Look.5.var", "no exact match: same as before");
    assert!(crate::db::remove_download_link(&db, "Acid.Look.3.var", "https://example.com/Acid.Look.3.var").unwrap());
    assert_eq!(pick(Some(3)), "Acid.Look.5.var");
}


#[test]
fn mega_links_parse_every_form() {
    use crate::mega::{parse, MegaRef};
    let file_key = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"; // 32 bytes
    let folder_key = "AAECAwQFBgcICQoLDA0ODw"; // 16 bytes
    let a = parse(&format!("https://mega.nz/file/AbCdEf12#{file_key}")).unwrap();
    let b = parse(&format!("https://mega.nz/#!AbCdEf12!{file_key}")).unwrap();
    assert_eq!(a, b);
    assert!(matches!(a, MegaRef::File { ref handle, .. } if handle == "AbCdEf12"));

    let f = parse(&format!("https://mega.nz/folder/FoLd3r#{folder_key}")).unwrap();
    assert!(matches!(f, MegaRef::Folder { ref handle, sub: None, .. } if handle == "FoLd3r"));
    let legacy = parse(&format!("https://mega.nz/#F!FoLd3r!{folder_key}")).unwrap();
    assert_eq!(f, legacy);
    let sub = parse(&format!("https://mega.nz/folder/FoLd3r#{folder_key}/folder/SubDir1")).unwrap();
    assert!(matches!(sub, MegaRef::Folder { sub: Some(ref s), .. } if s == "SubDir1"));
    let inside = parse(&format!("https://mega.nz/folder/FoLd3r#{folder_key}/file/NoDe99")).unwrap();
    assert!(matches!(inside, MegaRef::FolderFile { ref node, .. } if node == "NoDe99"));
    let legacy_inside = parse(&format!("https://mega.nz/#F!FoLd3r!{folder_key}!NoDe99")).unwrap();
    assert_eq!(inside, legacy_inside);

    assert!(parse("https://mega.nz/file/AbCdEf12").unwrap_err().contains("no key"));
    assert!(parse(&format!("https://mega.nz/folder/FoLd3r#{file_key}")).unwrap_err().contains("length"));
}

#[test]
fn mega_ctr_decrypts_across_chunk_boundaries() {
    let key = crate::mega::testing::file_key([7u8; 32]);
    let plain: Vec<u8> = (0..100u8).collect();
    let mut cipher = plain.clone();
    key.decryptor().apply(&mut cipher); // encrypting is the same XOR
    assert_ne!(cipher, plain);
    let mut d = key.decryptor();
    let mut out = Vec::new();
    for piece in cipher.chunks(7) {
        let mut p = piece.to_vec();
        d.apply(&mut p);
        out.extend(p);
    }
    assert_eq!(out, plain);
}

// The MEGA round-trip tests encrypt and decrypt with the same code, so they
// can't catch an AES that's wrong both ways. This pins it to the standard.
#[test]
fn mega_aes_matches_the_fips_197_test_vector() {
    // FIPS-197 appendix C.1: AES-128, key 00 01 .. 0f, plaintext 00 11 .. ff.
    let key: [u8; 16] = core::array::from_fn(|i| i as u8);
    let plain: [u8; 16] = core::array::from_fn(|i| i as u8 * 0x11);
    let (enc, dec) = crate::mega::testing::aes_block(&key, plain);
    assert_eq!(
        enc,
        [0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4, 0xc5, 0x5a]
    );
    assert_eq!(dec, plain);
}

#[test]
fn mega_folder_node_keys_unwrap_and_name_the_file() {
    use crate::mega::testing::*;
    let folder_key = [9u8; 16];
    let raw: [u8; 32] = core::array::from_fn(|i| i as u8 * 3);
    let at = encrypt_name(&aes_key_of(&file_key(raw)), "Acid.Look.3.var");
    let wrapped = wrap_key(&folder_key, &raw);
    // Several `id:key` pairs; only one is wrapped with this folder's key.
    let k = format!("OtherId:{}/RootId:{wrapped}", wrap_key(&[1u8; 16], &raw));
    assert_eq!(unwrap(&folder_key, &k, &at, true).as_deref(), Some("Acid.Look.3.var"));
    assert_eq!(unwrap(&[2u8; 16], &k, &at, true), None, "wrong folder key");
    // A subfolder: a 16-byte key used directly.
    let dir_key = [5u8; 16];
    let dir_at = encrypt_name(&dir_key, "Looks");
    assert_eq!(
        unwrap(&folder_key, &format!("RootId:{}", wrap_key(&folder_key, &dir_key)), &dir_at, false).as_deref(),
        Some("Looks")
    );
}

#[test]
#[ignore]
fn bench_mega_decrypt_throughput() {
    let key = crate::mega::testing::file_key([7u8; 32]);
    let mut data = vec![0u8; 32 * 1024 * 1024];
    let mut d = key.decryptor();
    let t = std::time::Instant::now();
    for chunk in data.chunks_mut(16 * 1024) {
        d.apply(chunk);
    }
    let secs = t.elapsed().as_secs_f64();
    println!("MEGA decrypt: {:.1} MB/s", 32.0 / secs);
}

#[test]
fn mega_ctr_can_start_mid_stream() {
    let key = crate::mega::testing::file_key([5u8; 32]);
    let plain: Vec<u8> = (0..200u32).map(|i| (i * 7) as u8).collect();
    let mut cipher = plain.clone();
    key.decryptor().apply(&mut cipher);
    // Any start offset, aligned or not, decrypts the rest correctly.
    for start in [0usize, 16, 37, 96, 199] {
        let mut tail = cipher[start..].to_vec();
        key.decryptor_at(start as u64).apply(&mut tail);
        assert_eq!(tail, plain[start..], "from {start}");
    }
}

/// A stand-in for MEGA's storage server: serves `<anything>/<a>-<b>` byte
/// ranges (inclusive) of `body`, one connection per request.
fn serve_ranges(body: Vec<u8>) -> String {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let body = std::sync::Arc::new(body);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let body = std::sync::Arc::clone(&body);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap() == 0 || h == "\r\n" {
                        break;
                    }
                }
                let path = line.split_whitespace().nth(1).unwrap_or("");
                let range = path.rsplit('/').next().unwrap();
                let (a, b) = range.split_once('-').unwrap();
                let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
                let slice = &body[a..=b];
                let mut out = stream;
                write!(out, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", slice.len()).unwrap();
                // In pieces, like a network would.
                for piece in slice.chunks(50_000) {
                    out.write_all(piece).unwrap();
                }
            });
        }
    });
    format!("http://{addr}/dl/abc")
}

#[test]
fn mega_parallel_download_reassembles_and_decrypts() {
    let key = crate::mega::testing::file_key(core::array::from_fn(|i| (i * 11) as u8));
    let plain: Vec<u8> = (0..10_000_003u32).map(|i| (i % 251) as u8).collect();
    let mut encrypted = plain.clone();
    key.decryptor().apply(&mut encrypted);
    let address = serve_ranges(encrypted);
    let dir = std::env::temp_dir().join(format!("vam_mega_par_{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("out.bin");
    let m = crate::mega::testing::download(address, key, plain.len() as u64);
    assert!(crate::hub::download_mega_for_test(&m, &dest).expect("download"));
    let got = fs::read(&dest).unwrap();
    assert_eq!(got.len(), plain.len());
    assert!(got == plain, "decrypted bytes differ");
    fs::remove_dir_all(&dir).ok();
}

/// A file server that honours `Range: bytes=a-b` (206 + Content-Range), as
/// Pixeldrain does, and serves the whole body otherwise.
fn serve_file(body: Vec<u8>) -> String {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let body = std::sync::Arc::new(body);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let body = std::sync::Arc::clone(&body);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut range: Option<(usize, usize)> = None;
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).unwrap() == 0 || h == "\r\n" {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        let (a, b) = v.trim().split_once('-').unwrap();
                        // `bytes=N-` (a resume) runs to the end.
                        range = Some((a.parse().unwrap(), b.parse().unwrap_or(usize::MAX)));
                    }
                }
                let mut out = stream;
                match range {
                    Some((a, b)) => {
                        let b = b.min(body.len() - 1);
                        write!(out, "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {a}-{b}/{}\r\nConnection: close\r\n\r\n", b + 1 - a, body.len()).unwrap();
                        out.write_all(&body[a..=b]).unwrap();
                    }
                    None => {
                        write!(out, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                        out.write_all(&body).unwrap();
                    }
                }
            });
        }
    });
    format!("http://{addr}/api/file/abc?download")
}

/// A zip like the ones shared on forums: an AES-protected .var, a
/// ZipCrypto-protected one in a subfolder, and a preview image.
fn forum_zip() -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        let aes = SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "s3cret");
        w.start_file("Acid.Look.3.var", aes).unwrap();
        w.write_all(&vec![b'A'; 300_000]).unwrap();
        use zip::unstable::write::FileOptionsExt;
        let old = SimpleFileOptions::default().with_deprecated_encryption(b"s3cret").expect("non-empty password");
        w.start_file("deps/Bee.Hair.2.var", old).unwrap();
        w.write_all(b"hair bytes").unwrap();
        w.start_file("preview.png", SimpleFileOptions::default()).unwrap();
        w.write_all(b"png").unwrap();
        w.finish().unwrap();
    }
    buf.into_inner()
}

#[test]
fn archives_list_over_http_ranges_and_extract_with_password() {
    let zip = forum_zip();
    let dir = std::env::temp_dir().join(format!("vam_archive_{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let local = dir.join("pack.zip");
    fs::write(&local, &zip).unwrap();

    let entries = crate::archives::list_local_for_test(&local).unwrap();
    let names: Vec<(&str, bool)> = entries.iter().map(|e| (e.name.as_str(), e.encrypted)).collect();
    assert_eq!(names, vec![("Acid.Look.3.var", true), ("deps/Bee.Hair.2.var", true), ("preview.png", false)]);

    // The same listing through range requests only.
    let url = serve_file(zip.clone());
    let remote = crate::archives::list_http_for_test(&url).unwrap();
    assert_eq!(remote.len(), 3);
    assert_eq!(remote[0].size, 300_000);

    let out = dir.join("out.var");
    use crate::archives::extract_entry;
    extract_entry(&local, "Acid.Look.3.var", Some("s3cret"), &out).unwrap();
    assert_eq!(fs::read(&out).unwrap(), vec![b'A'; 300_000]);
    // By file name alone, ZipCrypto.
    extract_entry(&local, "Bee.Hair.2.var", Some("s3cret"), &out).unwrap();
    assert_eq!(fs::read(&out).unwrap(), b"hair bytes");
    assert!(extract_entry(&local, "Acid.Look.3.var", Some("nope"), &out).unwrap_err().contains("wrong password"));
    assert!(extract_entry(&local, "Bee.Hair.2.var", Some("nope"), &out).unwrap_err().contains("wrong password"));
    assert!(extract_entry(&local, "Acid.Look.3.var", None, &out).unwrap_err().contains("password-protected"));

    // Downloading through the cache: one fetch, then reused.
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let cached = crate::archives::ensure_cached(&url, &cancel, &mut |_, _| {}).unwrap();
    assert_eq!(fs::read(&cached).unwrap(), zip);
    let again = crate::archives::ensure_cached(&url, &cancel, &mut |_, _| {}).unwrap();
    assert_eq!(cached, again);
    let _ = fs::remove_file(&cached);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn source_links_are_found_in_pasted_text() {
    let text = "Download (pw: vam123): https://pixeldrain.com/u/AbC123, mirror https://mega.nz/folder/FoLd3r#AAECAwQFBgcICQoLDA0ODw!\n\
                <a href=\"https://www.mediafire.com/file/x1/Acid.Look.3.var/file\">MF</a> \
                https://f95zone.to/threads/123/ https://example.com/packs/Pack.zip?dl=1 https://pixeldrain.com/u/AbC123";
    let links = crate::sources::extract_links(text);
    assert_eq!(
        links,
        vec![
            "https://pixeldrain.com/u/AbC123",
            "https://mega.nz/folder/FoLd3r#AAECAwQFBgcICQoLDA0ODw!",
            "https://www.mediafire.com/file/x1/Acid.Look.3.var/file",
            "https://example.com/packs/Pack.zip?dl=1",
        ]
    );
}

#[test]
fn download_task_extracts_a_var_from_a_password_protected_zip_source() {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    // A real (tiny) .var: VaM packages are zips with Saves/ or Custom/ entries.
    let mut var = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut var);
        w.start_file("meta.json", SimpleFileOptions::default()).unwrap();
        w.write_all(b"{}").unwrap();
        w.start_file("Saves/scene/Look.json", SimpleFileOptions::default()).unwrap();
        w.write_all(b"{}").unwrap();
        w.finish().unwrap();
    }
    let mut outer = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut outer);
        let aes = SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "pw1");
        w.start_file("Pack/Acid.Look.3.var", aes).unwrap();
        w.write_all(var.get_ref()).unwrap();
        w.finish().unwrap();
    }
    let url = format!("{}&case=e2e{}", serve_file(outer.into_inner()), std::process::id());

    let db = crate::db::open_in_memory().expect("db");
    let mut row = crate::tasks::parse_link_line(&format!("Acid.Look.3.var {url}")).unwrap();
    row.archive_entry = Some("Pack/Acid.Look.3.var".to_string());
    row.archive_password = Some("pw1".to_string());
    crate::db::insert_download_links(&db, std::slice::from_ref(&row)).unwrap();

    let dest = std::env::temp_dir().join(format!("vam_zip_e2e_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dest);
    let tasks = std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
    tasks.lock().unwrap().insert(1, crate::tasks::new_progress_payload("t", "t"));
    let res = crate::tasks::run_download_one_task(
        &tasks,
        1,
        "Acid.Look.3".to_string(),
        url.clone(),
        "Acid.Look.3.var".to_string(),
        dest.display().to_string(),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        false,
        db,
    )
    .expect("task");
    assert_eq!(res.items[0].status, "downloaded", "{:?}", res.items[0].error);
    assert_eq!(fs::read(dest.join("Acid.Look.3.var")).unwrap(), var.into_inner());
    fs::remove_dir_all(&dest).ok();
}

#[test]
fn mega_zip_is_listed_through_decrypted_ranges() {
    let zip = forum_zip();
    let key = crate::mega::testing::file_key(core::array::from_fn(|i| (i * 5 + 1) as u8));
    let mut encrypted = zip.clone();
    key.decryptor().apply(&mut encrypted);
    let address = serve_ranges(encrypted);
    let m = crate::mega::testing::download(address, key, zip.len() as u64);
    let entries = crate::archives::list_mega_for_test(m).expect("listing");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["Acid.Look.3.var", "deps/Bee.Hair.2.var", "preview.png"]);
    assert!(entries[0].encrypted);
}

#[test]
fn passwords_are_found_in_posts() {
    let text = "Part 1: https://pixeldrain.com/u/aaa Password: Fire!Fox9\n\
                <b>Pass:</b> <code>second-one</code> https://pixeldrain.com/u/bbb\n\
                pw - third. Archive is password protected.";
    let found: Vec<String> = crate::sources::extract_passwords(text).into_iter().map(|(_, p)| p).collect();
    assert_eq!(found, vec!["Fire!Fox9", "second-one", "third"]);
}

#[test]
fn archive_password_is_verified_over_ranges() {
    let url = serve_file(forum_zip());
    let (entries, pw) = crate::archives::inspect_remote(&url, &["wrong".to_string(), "s3cret".to_string()]).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(pw.as_deref(), Some("s3cret"));
    let (_, none) = crate::archives::inspect_remote(&url, &["nope".to_string()]).unwrap();
    assert_eq!(none, None);
}

/// Writes two password-protected test zips into $VAM_FIXTURE_DIR (manual UI testing).
#[test]
#[ignore]
fn write_password_zip_fixtures() {
    use std::io::Write;
    use zip::unstable::write::FileOptionsExt;
    use zip::write::SimpleFileOptions;
    let dir = PathBuf::from(std::env::var("VAM_FIXTURE_DIR").expect("VAM_FIXTURE_DIR"));
    fs::create_dir_all(&dir).unwrap();
    let var = |scene: &str| {
        let mut v = std::io::Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut v);
        w.start_file("meta.json", SimpleFileOptions::default()).unwrap();
        w.write_all(b"{}").unwrap();
        w.start_file(format!("Saves/scene/{scene}.json"), SimpleFileOptions::default()).unwrap();
        w.write_all(b"{}").unwrap();
        w.finish().unwrap();
        v.into_inner()
    };
    let write = |name: &str, opts: SimpleFileOptions, files: Vec<(&str, Vec<u8>)>| {
        let mut out = std::io::Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut out);
        for (n, bytes) in files {
            w.start_file(n, opts).unwrap();
            w.write_all(&bytes).unwrap();
        }
        w.finish().unwrap();
        fs::write(dir.join(name), out.into_inner()).unwrap();
    };
    write("Pack1.zip", SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "alpha1"),
        vec![("ZzTest.LookA.1.var", var("a"))]);
    write("Pack2.zip", SimpleFileOptions::default().with_deprecated_encryption(b"beta2").expect("non-empty password"),
        vec![("Looks/ZzTest.HairB.2.var", var("b")), ("preview.png", b"png".to_vec())]);
}

#[test]
fn f95_changelog_html_yields_only_real_links() {
    // As copied from an F95 post: escaped HTML, favicon proxies, masked links.
    let html = r#"<a href="https://www.mediafire.com/folder/gvtp9c62fsetz/Pack+1" class="link">Pack 1</a>
        <img src="https://external-content.duckduckgo.com/ip3/www.mediafire.com.ico&quot;" />
        <a href="https://f95zone.to/masked/pixeldrain.com/239921/10996954/OG1PCXo1ab">PD</a>
        <img src="https://external-content.duckduckgo.com/ip3/pixeldrain.com.ico&quot;" />
        <a href="https://pixeldrain.com/u/AbCd12?foo=1&amp;bar=2">direct</a>"#;
    let text = crate::sources::decode_html_entities(html);
    let links = crate::sources::extract_links(&text);
    assert_eq!(
        links,
        vec![
            "https://www.mediafire.com/folder/gvtp9c62fsetz/Pack+1",
            "https://f95zone.to/masked/pixeldrain.com/239921/10996954/OG1PCXo1ab",
            "https://pixeldrain.com/u/AbCd12?foo=1&bar=2",
        ]
    );
    use crate::sources::normalize_link;
    assert_eq!(normalize_link(&links[0]).map(|(_, h)| h), Some("mediafire"));
    assert_eq!(normalize_link(&links[1]).map(|(_, h)| h), Some("f95"), "masked, not Pixeldrain");
    assert!(crate::sources::is_mediafire_page("https://www.mediafire.com/file/kgtrv44zyya5de7/x.var/file"));
    assert!(!crate::sources::is_mediafire_page("https://www.mediafire.com/folder/gvtp9c62fsetz/Pack+1"));
    assert!(!crate::sources::is_mediafire_page("https://download1528.mediafire.com/abc/kgtrv44zyya5de7/x.var"));
}

#[test]
fn f95_post_labels_and_unsupported_hosts() {
    let post = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../f95htmllinks.md"));
    let text = match post {
        Ok(p) => crate::sources::decode_html_entities(&p),
        // The sample post isn't committed; fall back to two of its lines.
        Err(_) => crate::sources::decode_html_entities(r#"Collection Update 2025-05-13 (Pack 1):  <a href="https://www.mediafire.com/folder/gvtp9c62fsetz/Pack+1" style="background-image: url(&quot;https://external-content.duckduckgo.com/ip3/www.mediafire.com.ico&quot;);">MEDIAFIRE</a> - <a href="https://f95zone.to/masked/gofile.io/239921/10996954/x">GOFILE</a> - <a href="https://vikingfile.com/f/5hKKoMEy2U">VIKINGFILE</a><br>
Collection Update 2025-05-13 (Pack 2):  <a href="https://www.mediafire.com/folder/a907i6iiy8cik/Pack+2">MEDIAFIRE </a>- <a href="https://vikingfile.com/f/JlEAsCP0RD">VIKINGFILE</a><br>"#),
    };
    let links = crate::sources::extract_links(&text);
    let first = &links[0];
    assert!(first.contains("mediafire.com/folder/gvtp9c62fsetz"), "{first}");
    let pos = text.find(first.as_str()).unwrap();
    assert_eq!(crate::sources::link_label(&text, pos).as_deref(), Some("Collection Update 2025-05-13 (Pack 1)"));
    let ignored = crate::sources::unsupported_hosts(&text);
    let hosts: Vec<&str> = ignored.iter().map(|(h, _)| h.as_str()).collect();
    assert!(hosts.contains(&"vikingfile.com") && hosts.contains(&"gofile.io"), "{ignored:?}");
    assert!(!hosts.iter().any(|h| h.contains("mediafire") || h.contains("pixeldrain") || h.contains("mega")));
}

#[test]
fn file_host_pages_are_not_taken_for_direct_zips() {
    let text = r#"<a href="https://datanodes.to/9572z1t321av/Pack_1.zip">DATANODES</a> <a href="https://example.com/files/Pack_1.zip">direct</a>"#;
    assert_eq!(crate::sources::extract_links(text), vec!["https://example.com/files/Pack_1.zip"]);
    assert_eq!(crate::sources::unsupported_hosts(text), vec![("datanodes.to".to_string(), 1)]);
}

#[test]
fn masked_links_to_unsupported_hosts_are_skipped() {
    let text = r#"<a href="https://f95zone.to/masked/gofile.io/1/2/abc">G</a> <a href="https://f95zone.to/masked/mega.nz/1/2/def">M</a>"#;
    assert_eq!(crate::sources::extract_links(text), vec!["https://f95zone.to/masked/mega.nz/1/2/def"]);
    assert_eq!(crate::sources::unsupported_hosts(text), vec![("gofile.io".to_string(), 1)]);
}

/// Serves `body` at any path except ones containing "dead" (404).
fn serve_or_404(body: Vec<u8>) -> String {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap() == 0 || h == "\r\n" {
                    break;
                }
            }
            let mut out = stream;
            if line.contains("dead") {
                write!(out, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            } else {
                write!(out, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                out.write_all(&body).unwrap();
            }
        }
    });
    format!("http://{addr}")
}

#[test]
fn download_falls_back_to_a_working_link_and_remembers() {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let mut var = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut var);
        w.start_file("Saves/scene/a.json", SimpleFileOptions::default()).unwrap();
        w.write_all(b"{}").unwrap();
        w.finish().unwrap();
    }
    let base = serve_or_404(var.into_inner());
    let good = format!("{base}/old/Acid.Look.3.var");
    let dead = format!("{base}/dead/Acid.Look.3.var");

    let db = crate::db::open_in_memory().expect("db");
    let rows: Vec<_> = [&good, &dead]
        .iter()
        .map(|u| crate::tasks::parse_link_line(&format!("Acid.Look.3.var {u}")).unwrap())
        .collect();
    crate::db::insert_download_links(&db, &rows).unwrap();

    let dest = std::env::temp_dir().join(format!("vam_fallback_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dest);
    let tasks = std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
    tasks.lock().unwrap().insert(1, crate::tasks::new_progress_payload("t", "t"));
    // Asked for through the dead (newly added) link: the old one saves it.
    let res = crate::tasks::run_download_one_task(
        &tasks,
        1,
        "Acid.Look.3".to_string(),
        dead.clone(),
        "Acid.Look.3.var".to_string(),
        dest.display().to_string(),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        false,
        db.clone(),
    )
    .expect("task");
    assert_eq!(res.items[0].status, "downloaded", "{:?}", res.items[0].error);
    assert!(dest.join("Acid.Look.3.var").is_file());

    let links = crate::db::links_for_file(&db, "acid.look.3.var");
    assert_eq!(links[0].url, good, "the working link ranks first");
    assert!(links[0].last_ok.is_some() && links[0].fail_count == 0);
    assert_eq!(links[1].fail_count, 1);
    assert!(links[1].last_error.as_deref().unwrap_or("").contains("404"));
    // And it is the one picked for this file from now on.
    assert_eq!(crate::tasks::pick_db_link(&db, "acid.look", Some(3)).unwrap().url, good);
    fs::remove_dir_all(&dest).ok();
}

#[test]
fn the_same_link_written_differently_is_saved_once() {
    use crate::sources::{link_key, normalize_link};
    // Pixeldrain: share page vs file link; but a file inside a zip is distinct.
    assert_eq!(link_key("https://pixeldrain.com/u/AbC12"), link_key("https://pixeldrain.com/api/file/AbC12?download"));
    let inner = "https://pixeldrain.com/api/file/n7ERz6Uu/info/zip/Daiana Prestes/EuLinRabei.Daiana_Prestes.1.var";
    let inner_light = "https://pixeldrain.com/api/file/n7ERz6Uu/info/zip/Daiana Prestes Light/EuLinRabei.Daiana_Prestes.1.var";
    assert_ne!(link_key(inner), link_key(inner_light));
    assert_ne!(link_key(inner), link_key("https://pixeldrain.com/u/n7ERz6Uu"));
    // ...and is never rewritten into a link to the whole zip.
    assert_eq!(normalize_link(inner).unwrap().0, inner);
    // MediaFire with or without the name; MEGA old and new forms.
    assert_eq!(
        link_key("https://www.mediafire.com/file/kgtrv44zyya5de7/Acid.Look.3.var/file"),
        link_key("https://mediafire.com/file/kgtrv44zyya5de7/file")
    );
    let k = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
    assert_eq!(link_key(&format!("https://mega.nz/#!AbCd!{k}")), link_key(&format!("https://mega.nz/file/AbCd#{k}")));

    let db = crate::db::open_in_memory().expect("db");
    let line = |l: &str| crate::tasks::parse_link_line(l).unwrap();
    assert_eq!(crate::db::insert_download_links(&db, &[line("Acid.Look.3.var https://pixeldrain.com/u/AbC12")]).unwrap(), 1);
    // Same file (different case), same link (different spelling): skipped.
    assert_eq!(crate::db::insert_download_links(&db, &[line("acid.look.3.var https://pixeldrain.com/api/file/AbC12")]).unwrap(), 0);
    // A genuinely different link for the file: kept beside the first.
    assert_eq!(crate::db::insert_download_links(&db, &[line("Acid.Look.3.var https://pixeldrain.com/u/ZzZ99")]).unwrap(), 1);
    assert_eq!(crate::db::links_for_file(&db, "Acid.Look.3.var").len(), 2);
}

#[test]
fn dependency_scan_prefers_the_copy_in_the_first_scan_folder() {
    use crate::tasks::build_dependency_items;
    use std::collections::{BTreeMap, BTreeSet};
    let root = std::env::temp_dir().join(format!("vam_depscan_rank_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let addon = root.join("AddonPackages");
    let offload = root.join("AddonPackages_offload");
    let extra = root.join("Downloads");
    for dir in [&addon, &offload, &extra] {
        fs::create_dir_all(dir).unwrap();
    }
    // A newer copy elsewhere doesn't beat the one VaM already loads.
    fs::write(addon.join("A.Look.2.var"), b"x").unwrap();
    fs::write(extra.join("A.Look.5.var"), b"x").unwrap();
    // Offloaded only: found there, the exact version over the newest.
    fs::write(offload.join("B.Hair.1.var"), b"x").unwrap();
    fs::write(offload.join("B.Hair.3.var"), b"x").unwrap();
    // Only in an added folder.
    fs::write(extra.join("C.Tex.1.var"), b"x").unwrap();

    let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for id in ["A.Look.5", "B.Hair.1", "B.Hair.latest", "C.Tex.1", "D.Gone.1"] {
        deps.insert(id.to_string(), BTreeSet::new());
    }
    let dirs: Vec<String> = [&addon, &offload, &extra].iter().map(|d| d.display().to_string()).collect();
    let (used, count, items) = build_dependency_items(&deps, &dirs);
    assert!(used);
    assert_eq!(count, 5);
    let path = |id: &str| {
        let item = items.iter().find(|i| i.package_id == id).unwrap();
        item.local_path.as_deref().map(|p| PathBuf::from(p).strip_prefix(&root).unwrap().to_path_buf())
    };
    assert_eq!(path("A.Look.5"), Some(PathBuf::from("AddonPackages").join("A.Look.2.var")));
    assert_eq!(path("B.Hair.1"), Some(PathBuf::from("AddonPackages_offload").join("B.Hair.1.var")));
    assert_eq!(path("B.Hair.latest"), Some(PathBuf::from("AddonPackages_offload").join("B.Hair.3.var")));
    assert_eq!(path("C.Tex.1"), Some(PathBuf::from("Downloads").join("C.Tex.1.var")));
    assert_eq!(path("D.Gone.1"), None);
    fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn restore_takes_packages_from_any_scan_folder_but_never_addon_packages() {
    use crate::offload::restore_source_root;
    let addon = PathBuf::from(r"D:\VaM\AddonPackages");
    let roots = vec![
        PathBuf::from(r"D:\VaM\AddonPackages_offload"),
        PathBuf::from(r"D:\VaM\AddonPackages\Sub"),
        PathBuf::from(r"E:\Downloads"),
    ];
    let root = |p: &str| restore_source_root(Path::new(p), &roots, &addon).cloned();
    assert_eq!(root(r"D:\VaM\AddonPackages_offload\A\A.B.1.var"), Some(roots[0].clone()));
    assert_eq!(root(r"e:\downloads\x\A.B.1.var"), Some(roots[2].clone()));
    assert_eq!(root(r"D:\VaM\AddonPackages\Sub\A.B.1.var"), None, "already loaded");
    assert_eq!(root(r"F:\Elsewhere\A.B.1.var"), None);
}


/// A partial download is continued with a Range request (206 + Content-Range)
/// instead of starting over, and the result is byte-identical.
#[test]
fn download_resumes_a_partial_file() {
    let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let url = serve_file(body.clone());
    let dest = std::env::temp_dir().join(format!("vam_resume_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dest);
    fs::create_dir_all(&dest).unwrap();
    let part = dest.join("partial.bin");
    fs::write(&part, &body[..70_000]).unwrap();

    let mut first_report: Option<u64> = None;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let done = crate::hub::download_to_file(&url, &part, &cancel, &mut |bytes, _total| {
        first_report.get_or_insert(bytes);
    })
    .unwrap();
    assert!(done);
    assert_eq!(first_report, Some(70_000), "progress starts at the resumed offset");
    assert_eq!(fs::read(&part).unwrap(), body);
    fs::remove_dir_all(&dest).ok();
}

/// Errors worth an automatic retry versus ones that aren't.
#[test]
fn transient_download_errors() {
    use crate::tasks::is_transient_download_error as t;
    assert!(t("download request failed: error sending request"));
    assert!(t("download stalled — no data received for 60s"));
    assert!(t("download returned HTTP 503 Service Unavailable"));
    assert!(t("download returned HTTP 429 Too Many Requests"));
    assert!(!t("download returned HTTP 404 Not Found"));
    assert!(!t("downloaded file is not a valid .var"));
}

#[test]
fn internalize_flags_clashes_and_keeps_every_backup() {
    use crate::internalize::{apply_internalize, scan_target_var_for_external_refs};
    use crate::models::InternalizeSelection;

    let dir = repo_root().join("tmp_internalize_test");
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("cleanup");
    }
    let vars = dir.join("vars");
    let backups = dir.join("bk");
    let scene = br#"{ "a": "S.Pack.1:/Custom/Clothing/x/top.vam", "b": "S.Pack.1:/Custom/Assets/clash.png" }"#;
    write_test_var_with_deps(
        &vars.join("T.Scene.1.var"),
        &["S.Pack.1"],
        &[("Saves/scene/s.json", scene), ("Custom/Assets/clash.png", b"the scene's own image")],
    );
    write_test_var_with_deps(
        &vars.join("S.Pack.1.var"),
        &[],
        &[
            ("Custom/Clothing/x/top.vam", b"{}"),
            ("Custom/Clothing/x/top.vaj", b"{}"),
            ("Custom/Assets/clash.png", b"the pack's image"),
        ],
    );
    let target = vars.join("T.Scene.1.var");
    let scanned = scan_directory_with_target_with_progress(&vars, &[], None, |_, _| {}).expect("scan");

    let groups = scan_target_var_for_external_refs(&target, &scanned).expect("refs");
    assert_eq!(groups.len(), 1);
    let refs = &groups[0].refs;
    let clash = refs.iter().find(|r| r.ref_path == "Custom/Assets/clash.png").expect("clash ref");
    assert_eq!(clash.conflicts, vec!["Custom/Assets/clash.png".to_string()]);
    let top = refs.iter().find(|r| r.ref_path == "Custom/Clothing/x/top.vam").expect("vam ref");
    assert!(top.conflicts.is_empty() && top.already_inside.is_empty());
    assert_eq!(top.bundle_paths.len(), 2, "the .vaj comes along");

    let pick = |path: &str| InternalizeSelection { source_pkg_id: "S.Pack.1".into(), ref_path: path.into(), from_pkg: None, from_path: None };
    let first = apply_internalize(&target, None, &scanned, &[pick("Custom/Clothing/x/top.vam")], Some(&backups))
        .expect("first apply");
    assert_eq!(first.entries_copied, 2);
    assert!(first.dependencies_removed.is_empty(), "clash.png still points at the pack");
    assert_eq!(first.backup_path.as_deref(), Some(backups.join("T.Scene.1.var").to_string_lossy().as_ref()));

    let second = apply_internalize(&target, None, &scanned, &[pick("Custom/Assets/clash.png")], Some(&backups))
        .expect("second apply");
    assert_eq!(second.entries_skipped_collision, vec!["Custom/Assets/clash.png".to_string()]);
    assert_eq!(second.dependencies_removed, vec!["S.Pack.1".to_string()]);
    assert_eq!(
        second.backup_path.as_deref(),
        Some(backups.join("T.Scene.1 (2).var").to_string_lossy().as_ref()),
        "an earlier backup is never overwritten"
    );
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn users_of_leaves_out_the_asking_package() {
    let items = vec![
        lib_item("S.Pack.1", &[]),
        lib_item("T.Scene.1", &["S.Pack.1"]),
        lib_item("U.Look.1", &["S.Pack.latest"]),
        lib_item("V.Alone.1", &[]),
    ];
    let paths = vec![
        items[0].file_path.clone(),
        items[3].file_path.clone(),
        "C:/elsewhere/X.Y.1.var".to_string(),
    ];
    let users = crate::library::users_of(&items, &paths, &items[1].file_path);
    assert_eq!(users[0], Some(vec!["U.Look.1".to_string()]));
    assert_eq!(users[1], Some(Vec::new()));
    assert_eq!(users[2], None, "not in the listing");
}

#[test]
fn package_usage_adds_up_every_user() {
    use crate::dep_usage::{analyze_package_usage, RefCache};

    let dir = repo_root().join("tmp_dep_usage_test");
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("cleanup");
    }
    let big = vec![b'x'; 5000];
    write_test_var_with_deps(
        &dir.join("D.Big.1.var"),
        &[],
        &[
            ("Custom/a.png", &b"aaaa"[..]),
            ("Custom/big.png", &big[..]),
            ("Custom/Clothing/x.vam", &b"{}"[..]),
            ("Custom/Clothing/x.vaj", &b"{}"[..]),
        ],
    );
    write_test_var_with_deps(
        &dir.join("T.Scene.1.var"),
        &["D.Big.1"],
        &[("Saves/scene/s.json", &br#"{ "t": "D.Big.1:/Custom/a.png" }"#[..])],
    );
    write_test_var_with_deps(
        &dir.join("U.Look.1.var"),
        &["D.Big.latest"],
        &[("Saves/Person/l.json", &br#"{ "c": "D.Big.latest:/Custom/Clothing/x.vam", "a": "D.Big.1:/Custom/a.png" }"#[..])],
    );
    write_test_var_with_deps(&dir.join("V.Other.1.var"), &[], &[("Saves/scene/v.json", &b"{}"[..])]);
    write_test_var_with_deps(&dir.join("W.Listed.1.var"), &["D.Big.1"], &[("Saves/scene/w.json", &b"{}"[..])]);

    let item = |id: &str, deps: &[&str]| {
        let path = dir.join(format!("{id}.var"));
        let mut it = lib_item(id, deps);
        it.file_path = path.to_string_lossy().to_string();
        it.size_bytes = fs::metadata(&path).expect("size").len();
        it
    };
    let items = vec![
        item("D.Big.1", &[]),
        item("T.Scene.1", &["D.Big.1"]),
        item("U.Look.1", &["D.Big.latest"]),
        item("V.Other.1", &[]),
        item("W.Listed.1", &["D.Big.1"]),
    ];
    let cache = Mutex::new(RefCache::default());
    let r = analyze_package_usage(&dir.join("D.Big.1.var"), &items, &cache, |_, _| {}).expect("analyze");

    assert_eq!(r.package_id, "D.Big.1");
    assert_eq!(r.users.len(), 3, "T, U and W depend on it; V doesn't");
    assert_eq!(r.users[0].package_id, "U.Look.1", "the one using most first");
    assert_eq!((r.users[0].files, r.users[0].bytes), (3, 4 + 2 + 2), "a.png and the .vam with its .vaj");
    let t = r.users.iter().find(|u| u.package_id == "T.Scene.1").expect("T");
    assert_eq!((t.bytes, t.paths.clone()), (4, vec!["Custom/a.png".to_string()]));
    let w = r.users.iter().find(|u| u.package_id == "W.Listed.1").expect("W");
    assert_eq!(w.bytes, 0, "listed, references nothing");
    assert_eq!(r.sum_bytes, 4 + 8);
    assert_eq!(r.union_bytes, 8, "a.png counted once");
    assert_eq!(r.content_bytes, 5000 + 8);
    assert_eq!(r.unused_bytes, 5000, "big.png: nobody uses it");
    assert_eq!(r.used_files[0].path, "Custom/a.png");
    assert_eq!(r.used_files[0].users, 2);
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn internalize_drops_a_dependency_named_by_another_version() {
    use crate::internalize::{apply_internalize, scan_target_var_for_external_refs};
    use crate::models::InternalizeSelection;

    let dir = repo_root().join("tmp_internalize_family_test");
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("cleanup");
    }
    let vars = dir.join("vars");
    // meta.json lists S.Pack.1; the scene names it .latest, and one more
    // reference points at a file the pack doesn't have.
    write_test_var_with_deps(
        &vars.join("T.Scene.1.var"),
        &["S.Pack.1"],
        &[("Saves/scene/s.json", &br#"{ "a": "S.Pack.latest:/Custom/a.png", "gone": "S.Pack.1:/Custom/missing.png" }"#[..])],
    );
    write_test_var_with_deps(&vars.join("S.Pack.1.var"), &[], &[("Custom/a.png", &b"aaaa"[..])]);
    let target = vars.join("T.Scene.1.var");
    let scanned = scan_directory_with_target_with_progress(&vars, &[], None, |_, _| {}).expect("scan");

    let groups = scan_target_var_for_external_refs(&target, &scanned).expect("refs");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].other_refs, vec!["S.Pack.1:/Custom/missing.png".to_string()], "the missing file keeps it needed");

    let pick = InternalizeSelection { source_pkg_id: "S.Pack.latest".into(), ref_path: "Custom/a.png".into(), from_pkg: None, from_path: None };
    let report = apply_internalize(&target, None, &scanned, std::slice::from_ref(&pick), None).expect("apply");
    assert!(report.dependencies_removed.is_empty(), "the missing file still points at it");

    // Without that reference, the .latest reference copied in drops S.Pack.1.
    fs::remove_dir_all(&dir).expect("cleanup");
    write_test_var_with_deps(
        &vars.join("T.Scene.1.var"),
        &["S.Pack.1"],
        &[("Saves/scene/s.json", &br#"{ "a": "S.Pack.latest:/Custom/a.png" }"#[..])],
    );
    write_test_var_with_deps(&vars.join("S.Pack.1.var"), &[], &[("Custom/a.png", &b"aaaa"[..])]);
    let scanned = scan_directory_with_target_with_progress(&vars, &[], None, |_, _| {}).expect("scan");
    assert!(scan_target_var_for_external_refs(&target, &scanned).expect("refs")[0].other_refs.is_empty());
    let report = apply_internalize(&target, None, &scanned, &[pick], None).expect("apply");
    assert_eq!(report.dependencies_removed, vec!["S.Pack.1".to_string()]);
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn internalize_takes_a_missing_file_from_another_package() {
    use crate::internalize::{apply_internalize, fill_from_copies, scan_target_var_for_external_refs};
    use crate::models::{BrokenKind, BrokenRef, BrokenRefCandidate, InternalizeSelection, ResourceRef};

    let dir = repo_root().join("tmp_internalize_fill_test");
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("cleanup");
    }
    let vars = dir.join("vars");
    write_test_var_with_deps(
        &vars.join("T.Scene.1.var"),
        &["S.Pack.1"],
        &[("Saves/scene/s.json", &br#"{ "a": "S.Pack.1:/Custom/a.png", "gone": "S.Pack.1:/Custom/gone.png" }"#[..])],
    );
    write_test_var_with_deps(&vars.join("S.Pack.1.var"), &[], &[("Custom/a.png", &b"aaaa"[..])]);
    write_test_var_with_deps(&vars.join("O.Other.1.var"), &[], &[("Textures/elsewhere.png", &b"gone-bytes"[..])]);
    let target = vars.join("T.Scene.1.var");
    let scanned = scan_directory_with_target_with_progress(&vars, &[], None, |_, _| {}).expect("scan");

    let mut groups = scan_target_var_for_external_refs(&target, &scanned).expect("refs");
    assert_eq!(groups[0].other_refs, vec!["S.Pack.1:/Custom/gone.png".to_string()]);
    // What the missing-file check reports: the file's contents, and a copy.
    let crc = zip::ZipArchive::new(fs::File::open(vars.join("O.Other.1.var")).expect("open"))
        .expect("zip")
        .by_name("Textures/elsewhere.png")
        .expect("entry")
        .crc32();
    let broken = vec![BrokenRef {
        kind: BrokenKind::Transitive,
        ref_pkg: "S.Pack.1".into(),
        ref_path: Some("Custom/gone.png".into()),
        expected_crc32: Some(crc),
        expected_size: Some(10),
        source_files_in_target: vec!["Saves/scene/s.json".into()],
        local_candidates: vec![BrokenRefCandidate {
            resource: ResourceRef {
                package_id: "O.Other.1".into(),
                package_file: vars.join("O.Other.1.var").to_string_lossy().to_string(),
                internal_path: "Textures/elsewhere.png".into(),
                crc32: Some(crc),
                size: 10,
                effective_size: 10,
            },
        }],
    }];
    fill_from_copies(&mut groups, &broken, &target).expect("fill");
    let fill = &groups[0].fills[0];
    assert_eq!((fill.from_pkg.as_str(), fill.from_path.as_str()), ("O.Other.1", "Textures/elsewhere.png"));

    let selections = vec![
        InternalizeSelection { source_pkg_id: "S.Pack.1".into(), ref_path: "Custom/a.png".into(), from_pkg: None, from_path: None },
        InternalizeSelection {
            source_pkg_id: "S.Pack.1".into(),
            ref_path: "Custom/gone.png".into(),
            from_pkg: Some(fill.from_pkg.clone()),
            from_path: Some(fill.from_path.clone()),
        },
    ];
    let report = apply_internalize(&target, None, &scanned, &selections, None).expect("apply");
    assert_eq!(report.dependencies_removed, vec!["S.Pack.1".to_string()], "nothing points at it any more");
    assert_eq!(report.entries_copied, 2);

    let mut zip = zip::ZipArchive::new(fs::File::open(&target).expect("open")).expect("zip");
    let mut text = String::new();
    zip.by_name("Saves/scene/s.json").expect("scene").read_to_string(&mut text).expect("read");
    assert!(text.contains("SELF:/Custom/gone.png") && !text.contains("S.Pack.1:/"), "{text}");
    let mut raw = Vec::new();
    zip.by_name("Custom/gone.png").expect("copied").read_to_end(&mut raw).expect("read");
    assert_eq!(raw, b"gone-bytes", "landed where the reference points");
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn internalize_leaves_plugins_as_dependencies() {
    use crate::internalize::scan_target_var_for_external_refs;

    let dir = repo_root().join("tmp_internalize_plugin_test");
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("cleanup");
    }
    let vars = dir.join("vars");
    write_test_var_with_deps(
        &vars.join("T.Scene.1.var"),
        &["P.Plugin.1", "Q.Tool.1"],
        &[(
            "Saves/scene/s.json",
            &br#"{ "plugin#0": "P.Plugin.1:/Custom/Scripts/P/p.cslist", "lut": "P.Plugin.1:/Custom/Assets/P/lut.png", "plugin#1": "Q.Tool.1:/Custom/Scripts/Q/q.cs" }"#[..],
        )],
    );
    write_test_var_with_deps(
        &vars.join("P.Plugin.1.var"),
        &[],
        &[("Custom/Scripts/P/p.cslist", &b"Internal/a.cs"[..]), ("Custom/Scripts/P/Internal/a.cs", &b"class A {}"[..]), ("Custom/Assets/P/lut.png", &b"lut"[..])],
    );
    write_test_var_with_deps(&vars.join("Q.Tool.1.var"), &[], &[("Custom/Scripts/Q/q.cs", &b"class Q {}"[..])]);
    let target = vars.join("T.Scene.1.var");
    let scanned = scan_directory_with_target_with_progress(&vars, &[], None, |_, _| {}).expect("scan");

    let groups = scan_target_var_for_external_refs(&target, &scanned).expect("refs");
    let p = groups.iter().find(|g| g.source_pkg_id == "P.Plugin.1").expect("P");
    assert_eq!(p.refs.iter().map(|r| r.ref_path.as_str()).collect::<Vec<_>>(), vec!["Custom/Assets/P/lut.png"], "the .cslist isn't offered");
    assert_eq!(p.plugin_refs, vec!["P.Plugin.1:/Custom/Scripts/P/p.cslist".to_string()]);
    let q = groups.iter().find(|g| g.source_pkg_id == "Q.Tool.1").expect("a plugin used for nothing else still shows");
    assert!(q.refs.is_empty());
    assert_eq!(q.plugin_refs, vec!["Q.Tool.1:/Custom/Scripts/Q/q.cs".to_string()]);
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn internalize_makes_a_package_whole_from_copies_of_a_missing_dependency() {
    use crate::internalize::{apply_internalize, fill_from_copies, scan_target_var_for_external_refs};
    use crate::models::{BrokenKind, BrokenRef, BrokenRefCandidate, InternalizeSelection, ResourceRef};

    let dir = repo_root().join("tmp_internalize_missing_dep_test");
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("cleanup");
    }
    let vars = dir.join("vars");
    // Gone.Pack.1 isn't installed; another package has the same image.
    write_test_var_with_deps(
        &vars.join("T.Scene.1.var"),
        &["Gone.Pack.1"],
        &[("Saves/scene/s.json", &br#"{ "t": "Gone.Pack.1:/Custom/skin.png" }"#[..])],
    );
    write_test_var_with_deps(&vars.join("O.Other.1.var"), &[], &[("Custom/Other/skin.png", &b"skin-bytes"[..])]);
    let target = vars.join("T.Scene.1.var");
    let scanned = scan_directory_with_target_with_progress(&vars, &[], None, |_, _| {}).expect("scan");

    let mut groups = scan_target_var_for_external_refs(&target, &scanned).expect("refs");
    let gone = groups.iter().find(|g| g.source_pkg_id == "Gone.Pack.1").expect("listed though not installed");
    assert!(!gone.installed && gone.refs.is_empty());
    assert_eq!(gone.other_refs, vec!["Gone.Pack.1:/Custom/skin.png".to_string()]);

    let crc = zip::ZipArchive::new(fs::File::open(vars.join("O.Other.1.var")).expect("open"))
        .expect("zip")
        .by_name("Custom/Other/skin.png")
        .expect("entry")
        .crc32();
    let broken = vec![BrokenRef {
        kind: BrokenKind::TextRef,
        ref_pkg: "Gone.Pack.1".into(),
        ref_path: Some("Custom/skin.png".into()),
        expected_crc32: Some(crc),
        expected_size: Some(10),
        source_files_in_target: vec!["Saves/scene/s.json".into()],
        local_candidates: vec![BrokenRefCandidate {
            resource: ResourceRef {
                package_id: "O.Other.1".into(),
                package_file: vars.join("O.Other.1.var").to_string_lossy().to_string(),
                internal_path: "Custom/Other/skin.png".into(),
                crc32: Some(crc),
                size: 10,
                effective_size: 10,
            },
        }],
    }];
    fill_from_copies(&mut groups, &broken, &target).expect("fill");
    let fill = groups.iter().find(|g| g.source_pkg_id == "Gone.Pack.1").expect("gone").fills[0].clone();

    let pick = InternalizeSelection {
        source_pkg_id: fill.ref_pkg.clone(),
        ref_path: fill.ref_path.clone(),
        from_pkg: Some(fill.from_pkg.clone()),
        from_path: Some(fill.from_path.clone()),
    };
    let report = apply_internalize(&target, None, &scanned, &[pick], None).expect("apply");
    assert_eq!(report.dependencies_removed, vec!["Gone.Pack.1".to_string()], "the missing dependency is gone");
    assert!(report.dependencies_removed.iter().all(|d| d != "O.Other.1"));
    let mut zip = zip::ZipArchive::new(fs::File::open(&target).expect("open")).expect("zip");
    let mut raw = Vec::new();
    zip.by_name("Custom/skin.png").expect("copied in").read_to_end(&mut raw).expect("read");
    assert_eq!(raw, b"skin-bytes");
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn missing_file_check_is_reused_until_something_changes() {
    use crate::fix_var::{scan_broken_refs_cached, BrokenRefsCache};

    let dir = repo_root().join("tmp_broken_cache_test");
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("cleanup");
    }
    let vars = dir.join("vars");
    let target = vars.join("T.Scene.1.var");
    write_test_var_with_deps(&target, &["S.Pack.1"], &[("Saves/scene/s.json", &br#"{ "a": "S.Pack.1:/gone.png" }"#[..])]);
    write_test_var_with_deps(&vars.join("S.Pack.1.var"), &[], &[("other.png", &b"x"[..])]);
    let scanned = scan_directory_with_target_with_progress(&vars, &[], None, |_, _| {}).expect("scan");
    let db = crate::db::open_in_memory().expect("db");
    let cache = Mutex::new(BrokenRefsCache::default());

    let first = scan_broken_refs_cached(&cache, &target, &scanned, &db).expect("check");
    assert_eq!(first.len(), 1);
    let again = scan_broken_refs_cached(&cache, &target, &scanned, &db).expect("check");
    assert_eq!(again.len(), 1);
    assert_eq!(cache.lock().unwrap().len(), 1, "nothing changed: reused");

    // The package changes: checked again.
    std::thread::sleep(std::time::Duration::from_millis(20));
    write_test_var_with_deps(&target, &["S.Pack.1"], &[("Saves/scene/s.json", &br#"{ "a": "S.Pack.1:/gone.png", "b": "S.Pack.1:/also-gone.png" }"#[..])]);
    let changed = scan_broken_refs_cached(&cache, &target, &scanned, &db).expect("check");
    assert_eq!(changed.len(), 2);
    assert_eq!(cache.lock().unwrap().len(), 2);

    // The database changes: checked again.
    db.conn.lock().unwrap().execute_batch("CREATE TABLE t_cache_probe (x INTEGER); INSERT INTO t_cache_probe VALUES (1);").expect("write");
    scan_broken_refs_cached(&cache, &target, &scanned, &db).expect("check");
    assert_eq!(cache.lock().unwrap().len(), 3);
    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn database_log_is_emptied_at_open_and_kept_small() {
    let dir = std::env::temp_dir().join(format!("vvd_wal_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("t.db");
    {
        let db = crate::db::open_file(&path).expect("open");
        let conn = db.conn.lock().unwrap();
        let limit: i64 = conn.query_row("PRAGMA journal_size_limit", [], |r| r.get(0)).unwrap();
        assert_eq!(limit, 64 * 1024 * 1024);
        conn.execute_batch("CREATE TABLE big (x BLOB); INSERT INTO big VALUES (zeroblob(4000000));").unwrap();
        // Left as if the app had been stopped without closing.
        std::mem::forget(conn);
        std::mem::forget(db);
    }
    let wal = dir.join("t.db-wal");
    assert!(fs::metadata(&wal).map(|m| m.len()).unwrap_or(0) > 1_000_000, "the change sits in the log");
    let db = crate::db::open_file(&path).expect("reopen");
    assert!(fs::metadata(&wal).map(|m| m.len()).unwrap_or(0) < 100_000, "checkpointed and emptied at open");
    let n: i64 = db.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM big", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1, "nothing lost");
    drop(db);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn clean_var_points_used_files_at_copies_elsewhere() {
    use crate::clean_var::{apply_clean, clean_candidates};

    let dir = repo_root().join("tmp_clean_var_test");
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("cleanup");
    }
    let vars = dir.join("vars");
    let scene = br#"{ "img": "SELF:/Custom/a.png", "cloth": "SELF:/Custom/Clothing/x.vam" }"#;
    let target = vars.join("T.Scene.1.var");
    write_test_var_with_deps(
        &target,
        &["D.Dep.1"],
        &[
            ("Saves/scene/s.json", &scene[..]),
            ("Custom/a.png", &b"image-a"[..]),
            ("Custom/Clothing/x.vam", &b"{ \"id\": \"x\" }"[..]),
            ("Custom/Clothing/x.vaj", &b"{}"[..]),
            ("Custom/Clothing/x.vab", &b"unity-x"[..]),
            ("Custom/unused.png", &b"never-used"[..]),
        ],
    );
    write_test_var_with_deps(
        &vars.join("S.Pack.1.var"),
        &[],
        &[
            ("Custom/a.png", &b"image-a"[..]),
            ("Custom/Clothing/x.vam", &b"{ \"id\": \"x\" }"[..]),
            ("Custom/Clothing/x.vaj", &b"{}"[..]),
            ("Custom/Clothing/x.vab", &b"unity-x"[..]),
            ("Custom/unused.png", &b"never-used"[..]),
        ],
    );
    // The same .vam without its .vab: unsafe to point at (the old page's rule).
    write_test_var_with_deps(
        &vars.join("S.Broken.1.var"),
        &[],
        &[
            ("Custom/Clothing/x.vam", &b"{ \"id\": \"x\" }"[..]),
            ("Custom/Clothing/x.vaj", &b"{}"[..]),
        ],
    );
    write_test_var_with_deps(&vars.join("D.Dep.1.var"), &[], &[("Other/a-copy.png", &b"image-a"[..])]);
    let scanned = scan_directory_with_target_with_progress(&vars, &[], Some(&target), |_, _| {}).expect("scan");

    let report = clean_candidates(&target, &scanned, None).expect("candidates");
    let paths: Vec<&str> = report.items.iter().map(|i| i.path.as_str()).collect();
    assert!(paths.contains(&"Custom/a.png") && paths.contains(&"Custom/Clothing/x.vam"), "{paths:?}");
    assert!(!paths.contains(&"Custom/Clothing/x.vaj"), "a bundle member comes with its .vam");
    assert!(!paths.iter().any(|p| p.starts_with("Saves/scene/")));
    assert_eq!(report.unreferenced_files, 1, "unused.png has a copy but isn't used");
    let a = report.items.iter().find(|i| i.path == "Custom/a.png").expect("a");
    let d = a.copies.iter().find(|c| c.package_id == "D.Dep.1").expect("D has it");
    assert!(d.already_dependency && d.installed);
    assert!(!a.copies.iter().find(|c| c.package_id == "S.Pack.1").unwrap().already_dependency);
    let x = report.items.iter().find(|i| i.path == "Custom/Clothing/x.vam").expect("x");
    assert_eq!(x.bundle, vec!["Custom/Clothing/x.vab".to_string(), "Custom/Clothing/x.vaj".to_string()]);
    let whole = x.copies.iter().find(|c| c.package_id == "S.Pack.1").expect("S has it");
    assert!(whole.incomplete.is_empty(), "{:?}", whole.incomplete);
    let broken = x.copies.iter().find(|c| c.package_id == "S.Broken.1").expect("listed");
    assert_eq!(broken.incomplete, vec!["Custom/Clothing/x.vab".to_string()]);
    let mut bad = BTreeMap::new();
    bad.insert(x.key.clone(), "S.Broken.1:Custom/Clothing/x.vam".to_string());
    let err = apply_clean(&target, scanned.clone(), &bad, None, None).expect_err("refused");
    assert!(err.to_string().contains("x.vab"), "{err}");

    let mut keep = BTreeMap::new();
    keep.insert(a.key.clone(), "D.Dep.1:Other/a-copy.png".to_string());
    keep.insert(x.key.clone(), "S.Pack.1:Custom/Clothing/x.vam".to_string());
    let backups = dir.join("bk");
    let result = apply_clean(&target, scanned, &keep, Some(&backups), None).expect("clean");
    let mut zip = zip::ZipArchive::new(fs::File::open(&target).expect("open")).expect("zip");
    let names: Vec<String> = zip.file_names().map(str::to_string).collect();
    assert_eq!(result.removed_files, 4, "a.png, x.vam with its .vaj and .vab; left: {names:?}");
    assert!(result.dependencies.iter().any(|d| d == "S.Pack.1"));
    assert!(backups.join("T.Scene.1.var").is_file());
    assert!(!names.iter().any(|n| n == "Custom/a.png" || n.starts_with("Custom/Clothing/")), "{names:?}");
    assert!(names.iter().any(|n| n == "Custom/unused.png"), "left alone");
    let mut text = String::new();
    zip.by_name("Saves/scene/s.json").expect("scene").read_to_string(&mut text).expect("read");
    assert!(text.contains("D.Dep.1:/Other/a-copy.png"), "{text}");
    assert!(text.contains("S.Pack.1:/Custom/Clothing/x.vam"), "{text}");
    fs::remove_dir_all(&dir).expect("cleanup");
}

