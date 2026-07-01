//! `sbr` — SENTIENT Backup & Restore CLI. A headless front-end over
//! `sentient-backup-core`.
//!
//!   sbr categories                       # the static component model
//!   sbr inspect  [conn]                  # per-component sizes on a live DB
//!   sbr backup   [conn] -o file [--no-telemetry]
//!   sbr restore  [conn] -i file [--allow-nonempty]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use sentient_backup_core::backup::{self, BackupOptions};
use sentient_backup_core::categories::catalog;
use sentient_backup_core::db::{build_report, human_bytes, ConnConfig, DbInspector};
use sentient_backup_core::progress::{Progress, ProgressFn};
use sentient_backup_core::restore::{self, RestoreOptions};

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
    /// Connect and report the size of each component.
    Inspect(ConnArgs),
    /// Back up a SENTIENT database to a .sentient-backup archive.
    Backup(BackupArgs),
    /// Restore a .sentient-backup archive into an (empty) database.
    Restore(RestoreArgs),
}

#[derive(Args, Clone)]
struct ConnArgs {
    #[arg(long, default_value = "localhost")]
    host: String,
    #[arg(long, default_value_t = 5432)]
    port: u16,
    #[arg(long, default_value = "sentient")]
    dbname: String,
    #[arg(long, default_value = "sentient")]
    user: String,
    #[arg(long, env = "PGPASSWORD", default_value = "")]
    password: String,
}

#[derive(Args)]
struct BackupArgs {
    #[command(flatten)]
    conn: ConnArgs,
    /// Output archive path.
    #[arg(short, long, default_value = "sentient.sentient-backup")]
    output: PathBuf,
    /// Exclude the telemetry (ts_kv) data — much smaller/faster.
    #[arg(long)]
    no_telemetry: bool,
    /// zstd compression level (1..=22).
    #[arg(long, default_value_t = 10)]
    level: i32,
}

#[derive(Args)]
struct RestoreArgs {
    #[command(flatten)]
    conn: ConnArgs,
    /// Backup archive to restore.
    #[arg(short, long)]
    input: PathBuf,
    /// Allow restoring into a non-empty database (unsafe; v1 default is empty-only).
    #[arg(long)]
    allow_nonempty: bool,
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

/// CLI progress sink: steps → stdout, tool logs → stderr.
fn cli_sink() -> ProgressFn {
    Arc::new(|p: Progress| match p {
        Progress::Step { name, index, total } => println!("[{index}/{total}] {name}…"),
        Progress::Done { message } => println!("✓ {message}"),
        Progress::Log { line } => eprintln!("    {line}"),
        Progress::Percent { .. } => {}
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_target(false)
        .init();

    match Cli::parse().cmd {
        Cmd::Categories => print_categories(),
        Cmd::Inspect(a) => inspect(&a).await?,
        Cmd::Backup(a) => {
            let opts = BackupOptions {
                output: a.output.clone(),
                include_telemetry: !a.no_telemetry,
                zstd_level: a.level,
            };
            let s = backup::run(&ConnConfig::from(&a.conn), &opts, cli_sink()).await?;
            println!(
                "  archive: {} ({})",
                s.output.display(),
                human_bytes(s.archive_bytes as i64)
            );
            println!("  dump sha256: {}", s.dump_sha256);
        }
        Cmd::Restore(a) => {
            let opts = RestoreOptions {
                input: a.input.clone(),
                allow_nonempty: a.allow_nonempty,
            };
            let s = restore::run(&ConnConfig::from(&a.conn), &opts, cli_sink()).await?;
            println!("  restored '{}' from {}", s.database, s.restored_from.display());
        }
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
    let (mut tb, mut tr) = (0i64, 0i64);
    for r in &report {
        tb += r.bytes;
        tr += r.rows;
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
        "TOTAL", "", tables.len(), tr, human_bytes(tb)
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
