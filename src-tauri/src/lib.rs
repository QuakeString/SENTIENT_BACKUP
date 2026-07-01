//! Tauri command/event layer — a thin shell over `sentient-backup-core`. All
//! real work lives in the core crate; these commands marshal args/results and
//! stream progress to the webview via a `Channel`.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::ipc::Channel;

use serde::Deserialize;

use sentient_backup_core::backup::{self, BackupOptions, FileStoreSpec, Selection};
use sentient_backup_core::categories::catalog;
use sentient_backup_core::db::{build_report, CategoryReport, ConnConfig, DbInspector, ServerInfo};
use sentient_backup_core::files::{self, FileStoreStatus};
use sentient_backup_core::progress::{Progress, ProgressFn};
use sentient_backup_core::restore::{self, RestoreOptions};

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

#[tauri::command]
async fn backup(
    host: String,
    port: u16,
    dbname: String,
    user: String,
    password: String,
    output: String,
    skip: Vec<String>,
    telemetry_days: Option<u32>,
    file_stores: Vec<FileStoreArg>,
    on_progress: Channel<Progress>,
) -> Result<BackupResult, String> {
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
    };
    let s = backup::run(
        &conn(host, port, dbname, user, password),
        &opts,
        channel_sink(on_progress),
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(BackupResult {
        output: s.output.display().to_string(),
        archive_bytes: s.archive_bytes,
        dump_sha256: s.dump_sha256,
        file_stores: s.file_stores,
    })
}

#[derive(Serialize)]
pub struct RestoreResult {
    database: String,
}

#[tauri::command]
async fn restore(
    host: String,
    port: u16,
    dbname: String,
    user: String,
    password: String,
    input: String,
    allow_nonempty: bool,
    file_store_paths: Vec<(String, String)>,
    on_progress: Channel<Progress>,
) -> Result<RestoreResult, String> {
    let opts = RestoreOptions {
        input: PathBuf::from(input),
        allow_nonempty,
        file_store_paths: file_store_paths
            .into_iter()
            .map(|(id, p)| (id, PathBuf::from(p)))
            .collect(),
    };
    let s = restore::run(
        &conn(host, port, dbname, user, password),
        &opts,
        channel_sink(on_progress),
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(RestoreResult {
        database: s.database,
    })
}

/// The static backup-component catalog (for rendering before connecting).
#[tauri::command]
fn default_categories() -> serde_json::Value {
    serde_json::to_value(catalog()).unwrap_or(serde_json::Value::Null)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            inspect,
            backup,
            restore,
            default_categories,
            file_store_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running SENTIENT Backup & Restore");
}
