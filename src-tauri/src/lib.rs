//! Tauri command/event layer — a thin shell over `sentient-backup-core`. All
//! real work lives in the core crate; these commands marshal args/results and
//! stream progress to the webview via a `Channel`.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::ipc::Channel;

use sentient_backup_core::backup::{self, BackupOptions};
use sentient_backup_core::categories::catalog;
use sentient_backup_core::db::{build_report, CategoryReport, ConnConfig, DbInspector, ServerInfo};
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

#[derive(Serialize)]
pub struct BackupResult {
    output: String,
    archive_bytes: u64,
    dump_sha256: String,
}

#[tauri::command]
async fn backup(
    host: String,
    port: u16,
    dbname: String,
    user: String,
    password: String,
    output: String,
    include_telemetry: bool,
    on_progress: Channel<Progress>,
) -> Result<BackupResult, String> {
    let opts = BackupOptions {
        output: PathBuf::from(output),
        include_telemetry,
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
    on_progress: Channel<Progress>,
) -> Result<RestoreResult, String> {
    let opts = RestoreOptions {
        input: PathBuf::from(input),
        allow_nonempty,
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
            default_categories
        ])
        .run(tauri::generate_context!())
        .expect("error while running SENTIENT Backup & Restore");
}
