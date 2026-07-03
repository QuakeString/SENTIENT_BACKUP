//! Tauri command/event layer — a thin shell over `sentient-backup-core`. All
//! real work lives in the core crate; these commands marshal args/results and
//! stream progress to the webview via a `Channel`.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::Manager;

use serde::Deserialize;

use sentient_backup_core::backup::{self, BackupOptions, FileStoreSpec, Selection};
use sentient_backup_core::categories::catalog;
use sentient_backup_core::db::{build_report, CategoryReport, ConnConfig, DbInspector, ServerInfo};
use sentient_backup_core::files::{self, FileStoreStatus};
use sentient_backup_core::progress::{Progress, ProgressFn};
use sentient_backup_core::restore::{self, RestoreOptions};

mod store;

#[derive(Serialize)]
pub struct InspectResult {
    server: ServerInfo,
    categories: Vec<CategoryReport>,
    total_bytes: i64,
    total_rows: i64,
    table_count: usize,
}

fn conn(host: String, port: u16, dbname: String, user: String, password: String) -> ConnConfig {
    ConnConfig {
        host,
        port,
        dbname,
        user,
        password,
    }
}

/// Bridge the engine's progress sink to a Tauri channel.
fn channel_sink(ch: Channel<Progress>) -> ProgressFn {
    Arc::new(move |p| {
        let _ = ch.send(p);
    })
}

/// Connect to a SENTIENT database and report per-category sizes/rows.
#[tauri::command]
async fn inspect(
    host: String,
    port: u16,
    dbname: String,
    user: String,
    password: String,
) -> Result<InspectResult, String> {
    let db = DbInspector::connect(&conn(host, port, dbname, user, password))
        .await
        .map_err(|e| e.to_string())?;
    let server = db.server_info().await.map_err(|e| e.to_string())?;
    let tables = db.tables_with_true_sizes().await.map_err(|e| e.to_string())?;
    let categories = build_report(&tables);
    let total_bytes = categories.iter().map(|c| c.bytes).sum();
    let total_rows = categories.iter().map(|c| c.rows).sum();
    Ok(InspectResult {
        server,
        categories,
        total_bytes,
        total_rows,
        table_count: tables.len(),
    })
}

#[derive(Deserialize)]
pub struct FileStoreArg {
    id: String,
    category_id: String,
    path: String,
}

#[derive(Serialize)]
pub struct BackupResult {
    output: String,
    archive_bytes: u64,
    dump_sha256: String,
    file_stores: usize,
}

/// File-store reachability, for enabling/disabling those categories in the UI.
#[tauri::command]
fn file_store_status() -> Vec<FileStoreStatus> {
    files::statuses()
}

/// Whether a backup archive is password-protected (age-encrypted).
#[tauri::command]
fn is_encrypted(path: String) -> Result<bool, String> {
    restore::is_encrypted(std::path::Path::new(&path)).map_err(|e| e.to_string())
}

#[tauri::command]
async fn backup(
    app: tauri::AppHandle,
    host: String,
    port: u16,
    dbname: String,
    user: String,
    password: String,
    output: String,
    skip: Vec<String>,
    telemetry_days: Option<u32>,
    file_stores: Vec<FileStoreArg>,
    passphrase: Option<String>,
    on_progress: Channel<Progress>,
) -> Result<BackupResult, String> {
    let (host_c, dbname_c, output_c) = (host.clone(), dbname.clone(), output.clone());
    let telemetry_label = if skip.iter().any(|s| s == "telemetry_historical") {
        "excluded".to_string()
    } else {
        match telemetry_days {
            Some(n) => format!("last {n}d"),
            None => "all".to_string(),
        }
    };
    let skipped_label = skip.join(",");

    let mut selection = Selection::skipping(&skip);
    selection.telemetry_days = telemetry_days;
    let specs = file_stores
        .into_iter()
        .filter(|f| selection.is_included(&f.category_id))
        .map(|f| FileStoreSpec {
            id: f.id,
            category_id: f.category_id,
            path: PathBuf::from(f.path),
        })
        .collect();
    let opts = BackupOptions {
        output: PathBuf::from(output),
        selection,
        file_stores: specs,
        zstd_level: 10,
        passphrase: passphrase.filter(|p| !p.is_empty()),
    };

    let start = std::time::Instant::now();
    let result = backup::run(
        &conn(host, port, dbname, user, password),
        &opts,
        channel_sink(on_progress),
    )
    .await;
    let dur = start.elapsed().as_millis() as i64;

    match result {
        Ok(s) => {
            let out = s.output.display().to_string();
            store::record_backup(&app, &host_c, &dbname_c, &out, s.archive_bytes as i64,
                &s.dump_sha256, &skipped_label, &telemetry_label, "success", "", dur);
            Ok(BackupResult {
                output: out,
                archive_bytes: s.archive_bytes,
                dump_sha256: s.dump_sha256,
                file_stores: s.file_stores,
            })
        }
        Err(e) => {
            let msg = e.to_string();
            store::record_backup(&app, &host_c, &dbname_c, &output_c, 0, "",
                &skipped_label, &telemetry_label, "failed", &msg, dur);
            Err(msg)
        }
    }
}

#[derive(Serialize)]
pub struct RestoreResult {
    database: String,
}

#[tauri::command]
async fn restore(
    app: tauri::AppHandle,
    host: String,
    port: u16,
    dbname: String,
    user: String,
    password: String,
    input: String,
    allow_nonempty: bool,
    file_store_paths: Vec<(String, String)>,
    passphrase: Option<String>,
    on_progress: Channel<Progress>,
) -> Result<RestoreResult, String> {
    let (host_c, dbname_c, input_c) = (host.clone(), dbname.clone(), input.clone());
    let opts = RestoreOptions {
        input: PathBuf::from(input),
        allow_nonempty,
        file_store_paths: file_store_paths
            .into_iter()
            .map(|(id, p)| (id, PathBuf::from(p)))
            .collect(),
        passphrase: passphrase.filter(|p| !p.is_empty()),
    };
    let start = std::time::Instant::now();
    let result = restore::run(
        &conn(host, port, dbname, user, password),
        &opts,
        channel_sink(on_progress),
    )
    .await;
    let dur = start.elapsed().as_millis() as i64;

    match result {
        Ok(s) => {
            store::record_restore(&app, &host_c, &dbname_c, &input_c, "success", "", dur);
            Ok(RestoreResult { database: s.database })
        }
        Err(e) => {
            let msg = e.to_string();
            store::record_restore(&app, &host_c, &dbname_c, &input_c, "failed", &msg, dur);
            Err(msg)
        }
    }
}

/// Create a new empty database (restore target) with the given credentials.
#[tauri::command]
async fn create_database(
    host: String,
    port: u16,
    dbname: String,
    user: String,
    password: String,
    name: String,
) -> Result<(), String> {
    sentient_backup_core::db::create_database(&conn(host, port, dbname, user, password), &name)
        .await
        .map_err(|e| e.to_string())
}

/// The static backup-component catalog (for rendering before connecting).
#[tauri::command]
fn default_categories() -> serde_json::Value {
    serde_json::to_value(catalog()).unwrap_or(serde_json::Value::Null)
}

/// Native "Save As" dialog for choosing where to write the backup archive.
/// Returns the chosen path, or `None` if the user cancelled. Sync command so
/// the blocking dialog runs off the async runtime / main thread.
#[tauri::command]
fn pick_save_path(app: tauri::AppHandle, default_name: String) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    app.dialog()
        .file()
        .add_filter("SENTIENT backup", &["sentient-backup"])
        .set_file_name(default_name)
        .blocking_save_file()
        .and_then(|fp| fp.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Native "Open" dialog for choosing a backup archive to restore.
#[tauri::command]
fn pick_open_path(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    app.dialog()
        .file()
        .add_filter("SENTIENT backup", &["sentient-backup"])
        .blocking_pick_file()
        .and_then(|fp| fp.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // WebKitGTK's DMABUF renderer crashes the web process ("WebKitWebProcess ...
    // fatal error") on some Linux setups (KDE / Nvidia / certain Wayland combos).
    // Disable it before the webview starts; WebKit falls back to a compatible
    // renderer. Set only if the user hasn't overridden it. No effect off Linux.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // If we bundled pg client tools as resources, point the engine at
            // them (PgTools::resolve checks SBR_PG_DUMP/SBR_PG_RESTORE first) so
            // the app is self-contained and doesn't need a system PostgreSQL.
            if let Ok(res) = app.path().resource_dir() {
                let bin = res.join("pgtools").join("bin");
                let (dump, restore) = if cfg!(windows) {
                    (bin.join("pg_dump.exe"), bin.join("pg_restore.exe"))
                } else {
                    (bin.join("pg_dump"), bin.join("pg_restore"))
                };
                if dump.exists() {
                    std::env::set_var("SBR_PG_DUMP", &dump);
                }
                if restore.exists() {
                    std::env::set_var("SBR_PG_RESTORE", &restore);
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            inspect,
            backup,
            restore,
            default_categories,
            file_store_status,
            is_encrypted,
            pick_save_path,
            pick_open_path,
            create_database,
            store::list_connections,
            store::save_connection,
            store::delete_connection,
            store::get_connection_password,
            store::list_backup_history,
            store::list_restore_history,
            store::clear_history,
            store::setting_get,
            store::setting_set
        ])
        .run(tauri::generate_context!())
        .expect("error while running SENTIENT Backup & Restore");
}
