//! Tauri command/event layer — a thin shell over `sentient-backup-core`.
//! All real work lives in the core crate; these commands just marshal args and
//! results to/from the webview.

use serde::Serialize;

use sentient_backup_core::categories::catalog;
use sentient_backup_core::db::{build_report, CategoryReport, ConnConfig, DbInspector, ServerInfo};

#[derive(Serialize)]
pub struct InspectResult {
    server: ServerInfo,
    categories: Vec<CategoryReport>,
    total_bytes: i64,
    total_rows: i64,
    table_count: usize,
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
    let cfg = ConnConfig {
        host,
        port,
        dbname,
        user,
        password,
    };
    let db = DbInspector::connect(&cfg).await.map_err(|e| e.to_string())?;
    let server = db.server_info().await.map_err(|e| e.to_string())?;
    let tables = db
        .tables_with_true_sizes()
        .await
        .map_err(|e| e.to_string())?;
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

/// The static backup-component catalog (for rendering the tree before connecting).
#[tauri::command]
fn default_categories() -> serde_json::Value {
    serde_json::to_value(catalog()).unwrap_or(serde_json::Value::Null)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![inspect, default_categories])
        .run(tauri::generate_context!())
        .expect("error while running SENTIENT Backup & Restore");
}
