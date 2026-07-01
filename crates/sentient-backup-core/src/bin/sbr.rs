//! `sbr` — SENTIENT Backup & Restore CLI. A headless front-end over
//! `sentient-backup-core`, useful for scripting/CI and for validating the
//! engine independently of the GUI.
//!
//! Phase 0 commands:
//!   sbr categories                 # print the static backup-category model
//!   sbr inspect [conn flags]       # connect and report per-category sizes

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use sentient_backup_core::categories::catalog;
use sentient_backup_core::db::{build_report, human_bytes, ConnConfig, DbInspector};

#[derive(Parser)]
#[command(name = "sbr", version, about = "SENTIENT Backup & Restore (CLI)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the static backup-component catalog.
    Categories,
    /// Connect to a SENTIENT database and report the size of each component.
    Inspect(ConnArgs),
}

#[derive(Args)]
struct ConnArgs {
    #[arg(long, default_value = "localhost")]
    host: String,
    #[arg(long, default_value_t = 5432)]
    port: u16,
    #[arg(long, default_value = "sentient")]
    dbname: String,
    #[arg(long, default_value = "sentient")]
    user: String,
    /// Password (or set PGPASSWORD).
    #[arg(long, env = "PGPASSWORD", default_value = "")]
    password: String,
}

impl From<&ConnArgs> for ConnConfig {
    fn from(a: &ConnArgs) -> Self {
        ConnConfig {
            host: a.host.clone(),
            port: a.port,
            dbname: a.dbname.clone(),
            user: a.user.clone(),
            password: a.password.clone(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn".into()),
        )
        .with_target(false)
        .init();

    match Cli::parse().cmd {
        Cmd::Categories => print_categories(),
        Cmd::Inspect(a) => inspect(&a).await?,
    }
    Ok(())
}

fn print_categories() {
    println!("Backup components ({} categories):\n", catalog().len());
    for c in catalog() {
        let flags = format!(
            "{}{}",
            if c.default_selected { "[x]" } else { "[ ]" },
            if c.locked { " (locked)" } else { "" }
        );
        println!("  {flags}  {:<34} {}", c.name, c.id);
        println!("        {}", c.notes);
        if let Some(fs) = c.file_store {
            println!("        + file store: {} ({})", fs.id, fs.default_path);
        }
    }
}

async fn inspect(a: &ConnArgs) -> Result<()> {
    let cfg = ConnConfig::from(a);
    let db = DbInspector::connect(&cfg).await?;
    let info = db.server_info().await?;

    println!("Connected to '{}'", info.database);
    println!("  {}", info.postgres_version.replace('\n', " "));
    println!(
        "  TimescaleDB: {}\n",
        info.timescaledb_version.as_deref().unwrap_or("(not installed)")
    );

    let tables = db.tables_with_true_sizes().await?;
    let report = build_report(&tables);

    println!(
        "  {:<38} {:>4} {:>7} {:>14} {:>10}",
        "COMPONENT", "SEL", "TABLES", "ROWS", "SIZE"
    );
    println!("  {}", "-".repeat(76));
    let (mut total_bytes, mut total_rows) = (0i64, 0i64);
    for r in &report {
        total_bytes += r.bytes;
        total_rows += r.rows;
        let sel = if r.locked {
            "req"
        } else if r.default_selected {
            "on"
        } else {
            "off"
        };
        println!(
            "  {:<38} {:>4} {:>7} {:>14} {:>10}",
            truncate(&r.name, 38),
            sel,
            r.tables.len(),
            r.rows,
            human_bytes(r.bytes)
        );
    }
    println!("  {}", "-".repeat(76));
    println!(
        "  {:<38} {:>4} {:>7} {:>14} {:>10}",
        "TOTAL",
        "",
        tables.len(),
        total_rows,
        human_bytes(total_bytes)
    );
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}
