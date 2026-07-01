//! Restore engine (Phase 1, empty-DB-only). Reads a `.sentient-backup`, verifies
//! the dump checksum, refuses a non-empty target, then runs the TimescaleDB
//! restore flow: ensure the extension + `timescaledb_pre_restore()`,
//! `pg_restore` (decompressed dump piped to stdin), `timescaledb_post_restore()`.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use crate::db::{ConnConfig, DbInspector};
use crate::error::{Error, Result};
use crate::manifest::Manifest;
use crate::pg_tools::PgTools;
use crate::progress::{Progress, ProgressFn, Steps};

const DUMP_MEMBER: &str = "db/dump.pgc.zst";

#[derive(Debug, Clone)]
pub struct RestoreOptions {
    pub input: PathBuf,
    /// v1 is empty-DB-only; this override is for advanced users / testing.
    pub allow_nonempty: bool,
}

#[derive(Debug, Clone)]
pub struct RestoreSummary {
    pub database: String,
    pub restored_from: PathBuf,
}

pub async fn run(
    cfg: &ConnConfig,
    opts: &RestoreOptions,
    sink: ProgressFn,
) -> Result<RestoreSummary> {
    let mut steps = Steps::new(sink.clone(), 6);

    steps.step("Reading backup");
    let (manifest, tmp_dump) = read_archive(&opts.input)?;
    steps.log(format!(
        "Backup of '{}' ({}), created {}",
        manifest.source.database,
        manifest
            .source
            .timescaledb_version
            .as_deref()
            .map(|v| format!("TimescaleDB {v}"))
            .unwrap_or_else(|| "no TimescaleDB".into()),
        manifest.created_at.to_rfc3339()
    ));

    steps.step("Verifying integrity");
    verify_checksum(&manifest, &tmp_dump)?;

    steps.step("Checking target database");
    let db = DbInspector::connect(cfg).await?;
    let server = db.server_info().await?;
    let existing = db.list_public_tables().await?;
    if !existing.is_empty() && !opts.allow_nonempty {
        let _ = std::fs::remove_file(&tmp_dump);
        return Err(Error::msg(format!(
            "target database '{}' is not empty ({} tables). v1 restores into an empty database only.",
            server.database,
            existing.len()
        )));
    }
    let has_timescale = server.timescaledb_version.is_some();

    steps.step("Preparing (timescaledb_pre_restore)");
    if has_timescale {
        db.batch("CREATE EXTENSION IF NOT EXISTS timescaledb").await?;
        db.batch("SELECT timescaledb_pre_restore()").await?;
    }

    steps.step("Restoring (pg_restore)");
    let tools = PgTools::resolve()?;
    let cfg2 = cfg.clone();
    let dump2 = tmp_dump.clone();
    let sink2 = sink.clone();
    let restore_res =
        tokio::task::spawn_blocking(move || pg_restore_stream(&tools, &cfg2, &dump2, sink2))
            .await
            .map_err(|e| Error::msg(format!("restore task panicked: {e}")))?;

    // Always attempt post_restore, even if pg_restore erred, to leave the DB sane.
    if has_timescale {
        steps.step("Finalizing (timescaledb_post_restore)");
        db.batch("SELECT timescaledb_post_restore()").await?;
    }

    let _ = std::fs::remove_file(&tmp_dump);
    restore_res?;

    steps.done(format!("Restored into '{}'", server.database));
    Ok(RestoreSummary {
        database: server.database,
        restored_from: opts.input.clone(),
    })
}

/// Extract `manifest.json` and the compressed dump (to a temp file) from the tar.
fn read_archive(input: &Path) -> Result<(Manifest, PathBuf)> {
    let f = File::open(input).map_err(|e| Error::msg(format!("opening backup: {e}")))?;
    let mut ar = tar::Archive::new(f);
    let mut manifest: Option<Manifest> = None;
    let mut dump_tmp: Option<PathBuf> = None;

    for entry in ar.entries().map_err(|e| Error::msg(e.to_string()))? {
        let mut e = entry.map_err(|e| Error::msg(e.to_string()))?;
        let path = e.path().map_err(|e| Error::msg(e.to_string()))?.to_string_lossy().into_owned();
        if path == "manifest.json" {
            let mut buf = String::new();
            e.read_to_string(&mut buf).map_err(|e| Error::msg(e.to_string()))?;
            manifest = Some(serde_json::from_str(&buf).map_err(|e| Error::msg(format!("bad manifest: {e}")))?);
        } else if path == DUMP_MEMBER {
            let tmp = input.with_extension("restore.tmp");
            let mut out = File::create(&tmp).map_err(|e| Error::msg(e.to_string()))?;
            io::copy(&mut e, &mut out).map_err(|e| Error::msg(e.to_string()))?;
            dump_tmp = Some(tmp);
        }
    }
    let manifest = manifest.ok_or_else(|| Error::msg("backup has no manifest.json"))?;
    let dump = dump_tmp.ok_or_else(|| Error::msg("backup has no database dump"))?;
    Ok((manifest, dump))
}

fn verify_checksum(manifest: &Manifest, dump_zst: &Path) -> Result<()> {
    let expected = manifest
        .files
        .iter()
        .find(|f| f.path == DUMP_MEMBER)
        .map(|f| f.sha256.clone())
        .ok_or_else(|| Error::msg("manifest is missing the dump checksum"))?;
    let mut f = File::open(dump_zst).map_err(|e| Error::msg(e.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| Error::msg(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let got = hex(&hasher.finalize());
    if got != expected {
        return Err(Error::msg(
            "integrity check failed: the backup's database dump is corrupted or altered",
        ));
    }
    Ok(())
}

/// Pipe the decompressed dump into `pg_restore` on stdin (serial; parallel
/// restore needs a seekable file and lands with bundled tools in a later phase).
fn pg_restore_stream(
    tools: &PgTools,
    cfg: &ConnConfig,
    dump_zst: &Path,
    sink: ProgressFn,
) -> Result<()> {
    let port = cfg.port.to_string();
    let mut child = Command::new(&tools.pg_restore)
        .args([
            "--no-password",
            "--exit-on-error",
            "-h",
            &cfg.host,
            "-p",
            &port,
            "-U",
            &cfg.user,
            "-d",
            &cfg.dbname,
        ])
        .env("PGPASSWORD", &cfg.password)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::msg(format!("spawning pg_restore: {e}")))?;

    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    // Feed decompressed dump into stdin on its own thread (avoid pipe deadlock).
    let dump_path = dump_zst.to_path_buf();
    let feeder = std::thread::spawn(move || -> io::Result<()> {
        let zf = File::open(&dump_path)?;
        let mut dec = zstd::stream::Decoder::new(zf)?;
        io::copy(&mut dec, &mut stdin)?;
        stdin.flush()
    });

    // Drain stdout + stderr as log lines.
    let sink_o = sink.clone();
    let out_t = std::thread::spawn(move || drain(stdout, sink_o));
    let sink_e = sink.clone();
    let err_t = std::thread::spawn(move || drain(stderr, sink_e));

    let status = child.wait().map_err(|e| Error::msg(e.to_string()))?;
    let feed = feeder.join();
    let _ = out_t.join();
    let _ = err_t.join();

    if let Ok(Err(e)) = feed {
        return Err(Error::msg(format!("feeding dump to pg_restore: {e}")));
    }
    if !status.success() {
        return Err(Error::msg(format!("pg_restore failed ({status})")));
    }
    Ok(())
}

fn drain<R: Read>(r: R, sink: ProgressFn) {
    use std::io::BufRead;
    for line in io::BufReader::new(r).lines().map_while(std::result::Result::ok) {
        sink(Progress::Log { line });
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
