//! Backup engine. Streams `pg_dump` (custom format) through zstd + SHA-256 into
//! a `.sentient-backup` tar, plus optional file-store members (`vc-repos`,
//! `reports`). The full schema is always dumped; deselected categories only lose
//! their DATA. See `docs/RESEARCH_AND_PLAN.md`.

use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::categories::{catalog, CategoryKind};
use crate::db::{build_report, ConnConfig, DbInspector};
use crate::error::{Error, Result};
use crate::files;
use crate::manifest::{
    ComponentEntry, EncryptionInfo, FileEntry, FileStoreEntry, Manifest, SourceInfo,
    TelemetrySelection, FORMAT_VERSION,
};
use crate::pg_tools::PgTools;
use crate::progress::{Progress, ProgressFn, Steps};
use crate::util::HashingWriter;

const DUMP_MEMBER: &str = "db/dump.pgc.zst";

/// A file store to include: its category/store id and the directory to archive.
#[derive(Debug, Clone)]
pub struct FileStoreSpec {
    pub id: String,
    pub category_id: String,
    pub path: PathBuf,
}

/// Which components' DATA to include. Full schema is always dumped, so a restore
/// always has every table; only deselected data is omitted. `configuration` is
/// always included.
#[derive(Debug, Clone)]
pub struct Selection {
    pub include: HashSet<String>,
    // Phase 2b: telemetry last-N-days via COPY (reserved).
    pub telemetry_days: Option<u32>,
}

impl Selection {
    pub fn full() -> Self {
        Self {
            include: catalog().iter().map(|c| c.id.to_string()).collect(),
            telemetry_days: None,
        }
    }
    pub fn skipping(skip: &[String]) -> Self {
        let mut include: HashSet<String> = catalog().iter().map(|c| c.id.to_string()).collect();
        for s in skip {
            include.remove(s);
        }
        include.insert("configuration".into());
        Self {
            include,
            telemetry_days: None,
        }
    }
    pub fn is_included(&self, id: &str) -> bool {
        id == "configuration" || self.include.contains(id)
    }
}

#[derive(Debug, Clone)]
pub struct BackupOptions {
    pub output: PathBuf,
    pub selection: Selection,
    /// File stores to include (only reachable ones should be passed).
    pub file_stores: Vec<FileStoreSpec>,
    pub zstd_level: i32,
}

impl Default for BackupOptions {
    fn default() -> Self {
        Self {
            output: PathBuf::from("sentient.sentient-backup"),
            selection: Selection::full(),
            file_stores: Vec::new(),
            zstd_level: 10,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackupSummary {
    pub output: PathBuf,
    pub archive_bytes: u64,
    pub dump_sha256: String,
    pub file_stores: usize,
}

/// A member staged in a temp file, to be written into the archive.
struct StagedMember {
    archive_path: String,
    temp: PathBuf,
    sha256: String,
    bytes: u64,
}

fn exclude_data_args(sel: &Selection) -> Vec<String> {
    let mut args = Vec::new();
    for c in catalog() {
        if c.kind == CategoryKind::Configuration || sel.is_included(c.id) {
            continue;
        }
        if c.kind == CategoryKind::TelemetryHistorical {
            args.push("--exclude-table-data=public.ts_kv".into());
            args.push("--exclude-table-data=_timescaledb_internal.*".into());
        } else {
            for pat in c.pg_patterns() {
                args.push(format!("--exclude-table-data={pat}"));
            }
        }
    }
    args
}

pub async fn run(cfg: &ConnConfig, opts: &BackupOptions, sink: ProgressFn) -> Result<BackupSummary> {
    let total_steps = 4 + opts.file_stores.len() as u32;
    let mut steps = Steps::new(sink.clone(), total_steps);

    steps.step("Connecting and inspecting");
    let db = DbInspector::connect(cfg).await?;
    let server = db.server_info().await?;
    let tables = db.tables_with_true_sizes().await?;
    let report = build_report(&tables);
    drop(db);

    let tools = PgTools::resolve()?;
    steps.log(tools.dump_version().unwrap_or_else(|_| "pg_dump: unknown version".into()));

    let n_skipped = catalog()
        .iter()
        .filter(|c| c.kind != CategoryKind::Configuration && !opts.selection.is_included(c.id))
        .count();
    steps.step(if n_skipped == 0 {
        "Dumping database (full)".to_string()
    } else {
        format!("Dumping database ({n_skipped} component(s)' data excluded)")
    });
    let cfg2 = cfg.clone();
    let opts2 = opts.clone();
    let sink2 = sink.clone();
    let (tmp_dump, dump_sha, dump_bytes) =
        tokio::task::spawn_blocking(move || dump_compressed(&tools, &cfg2, &opts2, sink2))
            .await
            .map_err(|e| Error::msg(format!("dump task panicked: {e}")))??;

    let mut members = vec![StagedMember {
        archive_path: DUMP_MEMBER.into(),
        temp: tmp_dump,
        sha256: dump_sha.clone(),
        bytes: dump_bytes,
    }];
    let mut file_store_entries = Vec::new();

    // File stores (only ones passed in — caller filters by selected+reachable).
    for fs in &opts.file_stores {
        steps.step(format!("Archiving file store '{}'", fs.id));
        if !files::reachable(&fs.path) {
            steps.log(format!("  skipping '{}': {} not readable", fs.id, fs.path.display()));
            continue;
        }
        let member = format!("files/{}.tar.zst", fs.id);
        let temp = opts.output.with_extension(format!("{}.fs.tmp", fs.id));
        let (sha, bytes) = files::archive_dir(&fs.path, &temp, opts.zstd_level)?;
        members.push(StagedMember {
            archive_path: member.clone(),
            temp,
            sha256: sha,
            bytes,
        });
        file_store_entries.push(FileStoreEntry {
            id: fs.id.clone(),
            category_id: fs.category_id.clone(),
            source_path: fs.path.display().to_string(),
            member,
        });
    }

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
                selected: opts.selection.is_included(&c.id),
                tables: c.tables.clone(),
                bytes: c.bytes,
                rows: c.rows,
            })
            .collect(),
        telemetry: if opts.selection.is_included("telemetry_historical") {
            TelemetrySelection::All
        } else {
            TelemetrySelection::None
        },
        files: members
            .iter()
            .map(|m| FileEntry {
                path: m.archive_path.clone(),
                bytes: m.bytes,
                sha256: m.sha256.clone(),
            })
            .collect(),
        file_stores: file_store_entries.clone(),
        encryption: EncryptionInfo::none(),
    };
    write_archive(&opts.output, &manifest, &members)?;
    for m in &members {
        let _ = std::fs::remove_file(&m.temp);
    }

    let archive_bytes = std::fs::metadata(&opts.output)?.len();
    steps.done(format!("Backup written: {}", opts.output.display()));
    Ok(BackupSummary {
        output: opts.output.clone(),
        archive_bytes,
        dump_sha256: dump_sha,
        file_stores: file_store_entries.len(),
    })
}

/// pg_dump (custom format) → stdout → zstd → temp file, hashing the compressed
/// bytes. Returns (temp path, sha256-hex, byte length).
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
    for a in exclude_data_args(&opts.selection) {
        args.push(a);
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

fn write_archive(output: &PathBuf, manifest: &Manifest, members: &[StagedMember]) -> Result<()> {
    let f = File::create(output).map_err(|e| Error::msg(format!("creating archive: {e}")))?;
    let mut tar = tar::Builder::new(f);

    let mj = serde_json::to_vec_pretty(manifest).map_err(|e| Error::msg(e.to_string()))?;
    let mut header = tar::Header::new_gnu();
    header.set_size(mj.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "manifest.json", &mj[..])
        .map_err(|e| Error::msg(e.to_string()))?;

    for m in members {
        let mut df = File::open(&m.temp).map_err(|e| Error::msg(e.to_string()))?;
        tar.append_file(&m.archive_path, &mut df)
            .map_err(|e| Error::msg(e.to_string()))?;
    }

    tar.finish().map_err(|e| Error::msg(e.to_string()))?;
    Ok(())
}
