#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod archives;
mod config;
mod db;
mod dep_usage;
mod disable;
mod dropin;
mod execute;
mod extract;
mod fix_var;
mod hub;
mod hub_index;
mod integrity;
mod import;
mod internalize;
mod library;
mod mega;
mod models;
mod naming;
mod offload;
mod packages;
mod roles;
mod scan;
mod sources;
mod tasks;
#[cfg(test)]
mod tests;
mod utils;
mod vamprefs;

use tauri::Manager;

use crate::{
    extract::{extract_probe, extract_run},
    hub_index::{hub_exact_download, hub_index_lookup, hub_index_refresh, hub_index_status},
    integrity::start_verify_packages_task,
    disable::{plan_disable, set_packages_disabled},
    roles::{plan_remove_packages, set_package_roles},
    dropin::{detect_vam_dir, import_var_files},
    vamprefs::{auto_hide_sync, vam_prefs_carry_over, vam_prefs_get, vam_prefs_set},
    library::{
        get_hub_package_meta, get_var_image, get_var_package_details, hub_api,
        hub_image, hub_wishlist_list, hub_wishlist_set, list_local_package_ids,
        hub_embed_open, hub_embed_bounds, hub_embed_control, hub_embed_close,
        inspect_vam_dir,
        list_missing_dependencies,
    },
    models::AppState,
    offload::{plan_offload, start_offload_task},
    sources::{
        add_download_link, inspect_download_link, list_download_links, remove_download_link,
        search_download_links, start_scan_source_links_task, check_archive_password,
    },
    packages::{
        delete_var_package, move_var_to_creator_folder,
        start_apply_package_plan_task,
        start_plan_clean_duplicates_task, start_plan_organize_by_creator_task,
    },
    tasks::{
        start_analyze_text_dependencies_task, start_analyze_var_dependencies_task,
        start_download_one_task, start_import_download_links_task, get_download_links_count,
        clear_download_links, open_url, start_resolve_var_source_task,
        start_apply_internalize_task, start_apply_missing_resources_fix_task, cancel_task, pause_task,
        clear_database, clear_task, export_vam_bundle, export_var_resource,
        count_resources_from_db, find_db_candidates_for_broken_ref, find_resources_by_crc,
        find_resources_by_crcs_bulk, app_info, format_bytes_command, get_creator_flag, get_database_stats,
        get_package_flag, list_blocked_creators, list_favorite_creators, list_favorite_packages,
        list_replacement_prefs, set_replacement_pref,
        set_package_flag,
        export_var_scene_image, start_export_scene_images_task,
        get_task_progress, get_vam_preview, get_var_file_stats, list_resource_duplicates,
        list_resource_filter_options, list_resources_from_db, list_var_package_filter_options,
        list_target_var_text_refs, list_var_packages, list_var_packages_from_db, list_var_resources, load_config,
        load_db_package_resources, load_preview_image_data, path_exists, pick_folder, pick_folders,
        pick_manifest_file, pick_save_file, pick_var_file, save_config,
        scan_internalize_candidates, scan_missing_resources, set_creator_flag, show_in_explorer,
        start_scan_missing_resources_task,
        start_backfill_sizes_task, start_bulk_import_task, start_db_find_task,
        start_delete_dependency_scan_task, start_dependency_check_task, start_execute_task,
        start_reclaim_scan_task, start_scan_task, start_unique_resources_task,
    },
};

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let db = db::open(app.handle())?;
            app.manage(AppState::new());
            app.manage(db);
            if let Ok(dir) = app.path().app_config_dir() {
                hub_index::init(dir);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            extract_probe,
            extract_run,
            hub_index_refresh,
            hub_index_status,
            hub_index_lookup,
            hub_exact_download,
            start_verify_packages_task,
            plan_disable,
            set_packages_disabled,
            plan_remove_packages,
            set_package_roles,
            detect_vam_dir,
            import_var_files,
            vam_prefs_get,
            vam_prefs_set,
            auto_hide_sync,
            vam_prefs_carry_over,
            load_config,
            path_exists,
            pick_folder,
            pick_folders,
            pick_save_file,
            pick_var_file,
            pick_manifest_file,
            show_in_explorer,
            save_config,
            start_scan_task,
            start_execute_task,
            start_bulk_import_task,
            start_db_find_task,
            start_backfill_sizes_task,
            get_task_progress,
            clear_task,
            cancel_task,
            pause_task,
            format_bytes_command,
            app_info,
            get_vam_preview,
            load_preview_image_data,
            export_var_resource,
            export_vam_bundle,
            get_var_file_stats,
            get_database_stats,
            clear_database,
            list_var_resources,
            list_target_var_text_refs,
            list_var_packages,
            list_var_packages_from_db,
            list_var_package_filter_options,
            find_resources_by_crc,
            find_resources_by_crcs_bulk,
            load_db_package_resources,
            list_resources_from_db,
            count_resources_from_db,
            list_resource_filter_options,
            list_resource_duplicates,
            start_reclaim_scan_task,
            start_unique_resources_task,
            start_dependency_check_task,
            start_delete_dependency_scan_task,
            start_analyze_var_dependencies_task,
            start_analyze_text_dependencies_task,
            start_download_one_task,
            start_import_download_links_task,
            get_download_links_count,
            clear_download_links,
            open_url,
            start_resolve_var_source_task,
            set_creator_flag,
            get_creator_flag,
            list_blocked_creators,
            set_package_flag,
            get_package_flag,
            list_favorite_packages,
            set_replacement_pref,
            list_replacement_prefs,
            list_favorite_creators,
            scan_missing_resources,
            start_scan_missing_resources_task,
            find_db_candidates_for_broken_ref,
            start_apply_missing_resources_fix_task,
            scan_internalize_candidates,
            start_apply_internalize_task,
            dep_usage::start_dependency_usage_task,
            start_plan_clean_duplicates_task,
            start_plan_organize_by_creator_task,
            move_var_to_creator_folder,
            start_apply_package_plan_task,
            delete_var_package,
            export_var_scene_image,
            start_export_scene_images_task,
            get_var_package_details,
            get_var_image,
            list_missing_dependencies,
            plan_offload,
            start_offload_task,
            inspect_download_link,
            add_download_link,
            list_download_links,
            remove_download_link,
            search_download_links,
            start_scan_source_links_task,
            check_archive_password,
            inspect_vam_dir,
            get_hub_package_meta,
            hub_api,
            hub_image,
            hub_embed_open,
            hub_embed_bounds,
            hub_embed_control,
            hub_embed_close,
            list_local_package_ids,
            hub_wishlist_list,
            hub_wishlist_set,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
