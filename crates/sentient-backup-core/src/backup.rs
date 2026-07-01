//! Backup engine (Phase 1, DB-only). Streams `pg_dump` (custom format) through
//! zstd + SHA-256 into a `.sentient-backup` tar containing `manifest.json` and
//! `db/dump.pgc.zst`. Selective telemetry: `include_telemetry=false` excludes
//! the `ts_kv` hypertable data (config stays tiny). Full category-level
//! selection + file stores + encryption arrive in later phases.

use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use crate::db::{build_report, ConnConfig, DbInspector};
use crate::error::{Error, Result};
use crate::manifest::{
    ComponentEntry, EncryptionInfo, FileEntry, Manifest, SourceInfo, TelemetrySelection,
    FORMAT_VERSION,
};
use crate::pg_tools::PgTools;
use crate::progress::{Progress, ProgressFn, Steps};

const DUMP_MEMBER: &str = "db/dump.pgc.zst";

#[derive(Debug, Clone)]
pub struct BackupOptions {
    pub output: PathBuf,
    /// Phase 1 toggle: include the `ts_kv` telemetry data. Phase 2 replaces this
    /// with per-category selection + none/all/last-N-days.
    pub include_telemetry: bool,
    pub zstd_level: i32,
}

impl Default for BackupOptions {
    fn default() -> Self {
        Self {
            output: PathBuf::from("sentient.sentient-backup"),
            include_telemetry: true,
            zstd_level: 10,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackupSummary {
    pub output: PathBuf,
    pub archive_bytes: u64,
    pub dump_sha256: String,
}

pub async fn run(cfg: &ConnConfig, opts: &BackupOptions, sink: ProgressFn) -> Result<BackupSummary> {
    let mut steps = Steps::new(sink.clone(), 4);

    steps.step("Connecting and inspecting");
    let db = DbInspector::connect(cfg).await?;
    let server = db.server_info().await?;
    let tables = db.tables_with_true_sizes().await?;
    let report = build_report(&tables);
    drop(db);

    let tools = PgTools::resolve()?;
    steps.log(tools.dump_version().unwrap_or_else(|_| "pg_dump: unknown version".into()));

    steps.step(if opts.include_telemetry {
        "Dumping database (full)"
    } else {
        "Dumping database (excluding telemetry data)"
    });
    let cfg2 = cfg.clone();
    let opts2 = opts.clone();
    let sink2 = sink.clone();
    let (tmp_dump, dump_sha, dump_bytes) =
        tokio::task::spawn_blocking(move || dump_compressed(&tools, &cfg2, &opts2, sink2))
            .await
            .map_err(|e| Error::msg(format!("dump task panicked: {e}")))??;

    steps.step("Writing manifest and archive");
    let manifest = Manifest {
        format_version: FORMAT_VERSION,
        tool_version: crate::VERSION.to_string(),
        created_at: chrono::Utc::now(),
        source: SourceInfo {
            database: server.database,
            postgres_version: server.postgres_version,
            timescaledb_version: server.timescaledb_version,
        },
        components: report
            .iter()
            .map(|c| ComponentEntry {
                id: c.id.clone(),
                name: c.name.clone(),
                selected: if c.id == "telemetry_historical" {
                    opts.include_telemetry
                } else {
                    true
                },
                tables: c.tables.clone(),
                bytes: c.bytes,
                rows: c.rows,
            })
            .collect(),
        telemetry: if opts.include_telemetry {
            TelemetrySelection::All
        } else {
            TelemetrySelection::None
        },
        files: vec![FileEntry {
            path: DUMP_MEMBER.into(),
            bytes: dump_bytes,
            sha256: dump_sha.clone(),
        }],
        encryption: EncryptionInfo::none(),
    };
    write_archive(&opts.output, &manifest, &tmp_dump)?;
    let _ = std::fs::remove_file(&tmp_dump);

    let archive_bytes = std::fs::metadata(&opts.output)?.len();
    steps.done(format!("Backup written: {}", opts.output.display()));
    Ok(BackupSummary {
        output: opts.output.clone(),
        archive_bytes,
        dump_sha256: dump_sha,
    })
}

/// Run pg_dump (custom format) to stdout → zstd → temp file, hashing the
/// compressed bytes. Returns (temp path, sha256-hex, byte length).
fn dump_compressed(
    tools: &PgTools,
    cfg: &ConnConfig,
    opts: &BackupOptions,
    sink: ProgressFn,
) -> Result<(PathBuf, String, u64)> {
    let port = cfg.port.to_string();
    let mut args: Vec<String> = vec![
        "--format=custom".into(),
        "--no-password".into(),
        "-h".into(),
        cfg.host.clone(),
        "-p".into(),
        port,
        "-U".into(),
        cfg.user.clone(),
        "-d".into(),
        cfg.dbname.clone(),
    ];
    if !opts.include_telemetry {
        // Skip the telemetry data: the hypertable parent + its chunks. Schema is
        // still emitted so the target keeps an (empty) ts_kv.
        args.push("--exclude-table-data=public.ts_kv".into());
        args.push("--exclude-table-data=_timescaledb_internal.*".into());
    }

    let mut child = Command::new(&tools.pg_dump)
        .args(&args)
        .env("PGPASSWORD", &cfg.password)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::msg(format!("spawning pg_dump: {e}")))?;

    let mut stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let sink_err = sink.clone();
    let err_thread = std::thread::spawn(move || {
        use std::io::BufRead;
        for line in io::BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
            sink_err(Progress::Log { line });
        }
    });

    let tmp = opts.output.with_extension("dump.tmp");
    let file = File::create(&tmp).map_err(|e| Error::msg(format!("creating {}: {e}", tmp.display())))?;
    let mut hw = HashingWriter::new(file);
    {
        let mut enc = zstd::stream::Encoder::new(&mut hw, opts.zstd_level)
            .map_err(|e| Error::msg(format!("zstd: {e}")))?;
        io::copy(&mut stdout, &mut enc).map_err(|e| Error::msg(format!("streaming dump: {e}")))?;
        enc.finish().map_err(|e| Error::msg(format!("zstd finish: {e}")))?;
    }
    let status = child.wait().map_err(|e| Error::msg(e.to_string()))?;
    let _ = err_thread.join();
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::msg(format!("pg_dump failed ({status})")));
    }
    let (sha, bytes) = hw.finish();
    Ok((tmp, sha, bytes))
}

fn write_archive(output: &PathBuf, manifest: &Manifest, dump_tmp: &PathBuf) -> Result<()> {
    let f = File::create(output).map_err(|e| Error::msg(format!("creating archive: {e}")))?;
    let mut tar = tar::Builder::new(f);

    let mj = serde_json::to_vec_pretty(manifest).map_err(|e| Error::msg(e.to_string()))?;
    let mut header = tar::Header::new_gnu();
    header.set_size(mj.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "manifest.json", &mj[..])
        .map_err(|e| Error::msg(e.to_string()))?;

    let mut df = File::open(dump_tmp).map_err(|e| Error::msg(e.to_string()))?;
    tar.append_file(DUMP_MEMBER, &mut df)
        .map_err(|e| Error::msg(e.to_string()))?;

    tar.finish().map_err(|e| Error::msg(e.to_string()))?;
    Ok(())
}

/// A writer that tees bytes into a SHA-256 hasher and a byte counter.
struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    count: u64,
}

impl<W: Write> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            count: 0,
        }
    }
    fn finish(self) -> (String, u64) {
        let digest = self.hasher.finalize();
        (hex(&digest), self.count)
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.count += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
