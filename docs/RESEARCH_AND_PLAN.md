# SENTIENT Backup & Restore — Research & Plan

Status: **planning** (no code yet). This document is the foundation for the
`SENTIENT_BACKUP` repository: a cross-platform (Windows + Linux) desktop
application, built with **Rust + Tauri**, that backs up and restores a SENTIENT
deployment with **user-selectable data categories** (e.g. skip the multi-GB
telemetry history to keep a backup small).

---

## 1. Goals

- **Backup** a running/stopped SENTIENT instance to a single portable archive.
- **Selective** backup — the user ticks/unticks categories (Configuration,
  Telemetry, Attributes, Alarms, Audit logs, RPC history, Reports, …). Skipping
  telemetry alone takes a backup from ~7 GB to a few tens of MB.
- **Restore** — read a backup, show what's inside, selectively restore, safely.
- **Cross-platform** desktop GUI (Windows + Linux) — Rust + Tauri.
- Integrity (checksums), optional compression + encryption, clear progress.

Non-goals (v1): continuous/PITR replication, multi-node clusters, Cassandra
timeseries backend (SQL/Timescale only for now), scheduled/automated backups
(later phase).

---

## 2. What a SENTIENT deployment actually consists of (research)

Surveyed against the live dev instance:

| Layer | Detail |
|---|---|
| Database | **PostgreSQL 18.3** + **TimescaleDB 2.26.1**, single database `sentient` |
| Time-series | **`ts_kv` hypertable ≈ 7 GB** (chunks in `_timescaledb_internal._hyper_1_*`) |
| Everything else | config/entities/alarms/attributes — **tens of MB total** |
| File store 1 | **`vc-repos`** — git repositories for entity version control (`VC_REPOS_PATH`, default `/var/lib/sentient/vc-repos`) |
| File store 2 | **`reports`** — generated report files (`REPORT_OUTPUT_DIR`, default `/var/lib/sentient/reports`) |
| Resources | dashboards/widgets/images/SCADA symbols live **in the DB** (`resource`, `widget_type`, bytea) — no separate file store |

Implications:
- The DB dump is the core artifact. The two file stores are optional add-ons.
- **Timescale hypertables need special dump/restore handling** (see §4/§5).
- Because ~99 % of the size is one hypertable, "selective backup" is mostly
  "include/exclude telemetry" — but users also legitimately want to drop noisy
  audit/debug/event logs.

---

## 3. Backup component categories (the selectable tree)

Derived from the full `public` table catalog. Each category maps to a set of
tables (+ optional file store). "Default" = suggested initial checkbox state.

| Category | Default | Contains (tables / files) | Notes |
|---|---|---|---|
| **Configuration** *(core)* | ON, locked | tenant/tenant_profile, customer, tb_user (+ credentials/settings), role, group_permission, entity_group(+membership), device(+credentials/profile), asset(+profile), edge, entity_view, dashboard, widget_type, widgets_bundle(+widget), resource, rule_chain/rule_node(+state), calculated_field, analytics_pipeline, relation, admin_settings, queue, ai_model, ota_package, component_descriptor, oauth2_*, domain*, mobile_app*, notification_rule/target/template, report_template, recipe(+data), scheduler_event, repository_settings, qr_code_settings, key_dictionary, tb_schema_settings | The system's identity + structure. Effectively mandatory; restoring anything else without this is meaningless. |
| **Telemetry (historical)** | ON (biggest) | `ts_kv` hypertable | **~7 GB.** The prime deselect target. Offer *none / all / last N days* (see §4). |
| **Telemetry (latest)** | ON | `ts_kv_latest` | Small; current value per key. |
| **Attributes** | ON | `attribute_kv` | Client/shared/server attributes. |
| **Alarms** | ON | alarm, entity_alarm, alarm_comment(+monthly partitions), alarm_types | |
| **RPC history** | OFF | `rpc` (~200 MB) | Persistent/one-way RPC log; rarely needed in a restore. |
| **Audit & event logs** | OFF | audit_log*, edge_event*, error_event*, lc_event*, stats_event*, rule_chain/node_debug_event*, cf_debug_event*, analytics_pipeline_debug_event | Noisy, large, monthly-partitioned. Usually skippable. |
| **Notifications** | OFF | notification*, notification_request, user_notification_destination | Delivered-notification history. |
| **Reports** | OFF | report(+report_YYYY), report_delivery, report_job **+ `reports/` file store** | DB rows + generated files. |
| **Version control** | OFF | entity_version, vc_request **+ `vc-repos/` file store** | Entity-version git history. |
| **API usage / stats** | OFF | api_usage_state, queue_stats, task_job, telegram_link_code, whatsapp_verification_code | Operational state. |
| **Licensing** | OFF, warn | license, license_state, license_time_anchor | **Machine-bound** (anti-piracy HW tuple). Excluding from portable backups avoids carrying a license that won't validate on the target host. |

The category→table mapping is the heart of the app. It should be **defined in
code (a static manifest) but validated at runtime** against the live catalog,
so new tables in future SENTIENT versions are detected (and surfaced as
"uncategorized — included with Configuration by default") rather than silently
dropped.

---

## 4. Backup strategy

### Engine
- **`pg_dump` (directory format `-Fd`, parallel `-j`, compressed)** for the
  relational/config data. Directory format gives parallel dump + parallel
  restore + selective restore (via a table-of-contents list).
- **`pg_dump` major version MUST match the server (18).** See §8 for how we
  ship it.

### Selectivity
- **Deselect a small category** → `--exclude-table` / omit its tables from the
  `-t` include set.
- **Deselect telemetry** → the hypertable is the hard case:
  - *All or nothing:* `--exclude-table-data` on `ts_kv` **and** its chunks
    (`_timescaledb_internal._hyper_<id>_*`). Keeps the (empty) hypertable
    schema so the target is usable.
  - *Last N days (recommended option):* pg_dump can't time-filter, so treat
    telemetry as a **separate COPY-based export** — `COPY (SELECT … WHERE ts >
    now()-interval 'N days') TO STDOUT (FORMAT binary)` → compressed file.
    This decouples the 7 GB hypertable from the config dump and gives
    none/all/last-N-days in one mechanism. Restore = `COPY … FROM`.
- Decision: **config via pg_dump; telemetry (`ts_kv`) via its own COPY stream**
  with a none/all/range selector. Cleaner selectivity + smaller config dump.

### TimescaleDB handling
- Loaded extension must be dumped/ordered correctly. Standard flow: normal
  `pg_dump`; on **restore** call `timescaledb_pre_restore()` before and
  `timescaledb_post_restore()` after (see §5). Validate chunk exclusion in
  Phase 2 against a real Timescale DB — this is the riskiest technical area.

### File stores
- If selected, `tar` + `zstd` the `vc-repos/` and `reports/` directories.
- Requires filesystem access to those paths (see §6 — matters for dockerized
  deployments).

### Archive format (`.sentient-backup` = tar, optionally zstd + encrypted)
```
manifest.json              # see below
db/config/…                # pg_dump directory-format output (config categories)
db/telemetry.copy.zst      # optional ts_kv COPY stream
db/telemetry.meta.json     # range/row-count/chunk info
files/vc-repos.tar.zst     # optional
files/reports.tar.zst      # optional
checksums.sha256
```
`manifest.json`:
```jsonc
{
  "format_version": 1,
  "created_at": "2026-07-02T…Z",
  "tool_version": "0.1.0",
  "source": { "sentient_version": "4.2.2", "postgres": "18.3", "timescaledb": "2.26.1", "host": "…" },
  "selection": { "categories": ["configuration","attributes","alarms"], "telemetry": {"mode":"range","days":30} },
  "contents": [ { "category":"configuration", "tables":[…], "rows": 12345, "bytes": … } ],
  "encryption": { "scheme": "age|aes-256-gcm|none" },
  "checksums": { "algorithm": "sha256", "files": { … } }
}
```

### Compression & encryption & integrity
- **zstd** (fast, good ratio; level configurable).
- **Optional encryption** — `age` (X25519/passphrase) or AES-256-GCM. Backups
  contain **credentials, tokens, secrets** → prompt to encrypt; warn loudly if
  the user opts out.
- **SHA-256** of every member, recorded in the manifest + `checksums.sha256`.

---

## 5. Restore strategy

1. **Read `manifest.json`** → show contents, source versions, size, encryption.
2. **Compatibility check** — target PG/Timescale major version ≥ source;
   SENTIENT schema version (`tb_schema_settings`) compatible. Block or warn.
3. **Select** which of the backup's categories to restore.
4. **Safety**:
   - Recommend restoring into an **empty database** (cleanest, no merge
     conflicts). Detect a non-empty target and warn.
   - Options: *drop & recreate schema* vs *data-only into matching schema*.
   - **Pre-restore snapshot** — offer to back up the current target first.
   - **Dry run** — validate + report without writing.
   - **Require the SENTIENT server to be stopped** during restore (open
     connections + live writes will corrupt a partial restore). App should
     detect active connections and warn.
5. **Execute**:
   - `SELECT timescaledb_pre_restore();`
   - `pg_restore -Fd -j N` (config), with `-L` list for selective categories.
   - `COPY … FROM` for telemetry (if present/selected).
   - `SELECT timescaledb_post_restore();`
   - Extract `vc-repos/` and `reports/` to their target paths (if present).
6. **Post-restore** — verify row counts vs manifest; optional integrity report.

---

## 6. Connection & environment model (important design constraint)

- The app talks to PostgreSQL over **TCP** (`host:port/db/user/pass`) — works
  whether SENTIENT is dockerized (published `5432`) or native.
- **File-store backup needs filesystem access** to `vc-repos/` and `reports/`.
  For a **dockerized** SENTIENT (the Pi deployment), these live in a named
  volume. The app must either:
  - run on the host and read the volume's host path, **or**
  - shell out to `docker cp`/`docker exec` against the sentient container, **or**
  - (v1) support **DB-only backups** and mark file-store categories
    "unavailable (no filesystem access)" when it can't reach the paths.
- Decision: v1 = **DB-only fully supported everywhere**; file stores supported
  when a path is reachable, with an optional `docker` helper mode later.
- Restore should ideally run with the **server stopped** — document a "stop
  sentient → restore → start" workflow; detect live connections and warn.

---

## 7. Architecture (Rust + Tauri)

```
┌────────────────────────── Tauri app ──────────────────────────┐
│  Frontend (webview: TS + Svelte)                              │
│   Connection · Backup(tree+options) · Progress · Restore      │
│        │  invoke() commands            ▲  events (progress)    │
│  src-tauri  (thin command/event layer, no heavy logic)        │
│        │                                                       │
│  crates/sentient-backup-core  (all real work; UI-agnostic)    │
│   ├─ DbInspector    connect, catalog, sizes, row counts       │
│   ├─ Categories     static map + runtime reconciliation       │
│   ├─ BackupEngine   pg_dump + COPY + tar/zstd + manifest + enc │
│   ├─ RestoreEngine  manifest + pg_restore + COPY + ts hooks    │
│   ├─ PgTools        locate/verify pg_dump/pg_restore (v18)     │
│   └─ Progress       channel of {step, pct, log, bytes}        │
└───────────────────────────────────────────────────────────────┘
```

- **`sentient-backup-core`** is a plain Rust library with **zero Tauri
  dependency** → unit-testable, and reusable as a headless CLI (`sbr backup …`)
  for scripting/CI. The Tauri app is a thin shell over it.
- **Progress** streams via an `mpsc`/broadcast channel; `src-tauri` forwards to
  the webview as Tauri events → live progress bar + log tail.
- **Cancellation** via a `CancellationToken`.

---

## 8. Cross-platform (Windows + Linux)

- **Tauri** produces native bundles: `.msi`/NSIS `.exe` (Windows),
  `.deb`/`.AppImage` (Linux).
- **`pg_dump`/`pg_restore` v18** is required and version-sensitive. Options:
  1. Detect a PATH install (fragile; version drift).
  2. **Bundle the client binaries + libpq per platform** in the installer
     (self-contained, best UX). **Recommended.** Adds ~10–20 MB per platform.
  3. Pure-Rust logical dump (tokio-postgres `COPY`) — loses pg_dump fidelity
     (extensions, sequences, timescale, privileges). Only used for the
     telemetry COPY stream, not the schema. Not a full replacement in v1.
- Path handling: use `PathBuf`, platform default backup dirs (Documents/…),
  handle drive letters vs POSIX.

---

## 9. Security

- **DB password**: keep in the OS keychain via the `keyring` crate (opt-in
  "remember"); never write to disk in plaintext; never log secrets.
- **Backups are sensitive** (credentials, OAuth secrets, device keys) → default
  to prompting for **encryption**; a bold warning if declined.
- **Licensing tables** excluded by default (machine-bound; see §3).
- Redact connection strings/secrets from logs and the progress stream.

---

## 10. Tech stack

- **Rust core:** `tokio`, `tokio-postgres` (rustls) or `sqlx` (match SENTIENT's
  rustls stack), `serde`/`serde_json`, `tar`, `zstd`, `sha2`, `age` (or
  `aes-gcm`), `keyring`, `which`, `thiserror`, `tracing`.
- **App shell:** `tauri` v2.
- **Frontend:** TypeScript + **Svelte** (small bundle, simple reactivity) + a
  light component set; or plain TS if we want zero framework.
- **Bundled tooling:** PostgreSQL 18 `pg_dump`/`pg_restore` + `libpq` per OS.

---

## 11. Phased implementation plan

| Phase | Deliverable |
|---|---|
| **0 — Scaffold** | Repo layout, `sentient-backup-core` crate + Tauri app skeleton, GitHub Actions building Linux + Windows bundles, CLI stub. |
| **1 — MVP (DB, all-in)** | Connect + inspect; **full** pg_dump backup (all config categories, all telemetry) → tar+zstd + manifest; **full** restore with timescale pre/post hooks; live progress. Linux first. |
| **2 — Selective backup** | Category tree UI, size/row estimates, per-category include/exclude, telemetry none/all/last-N-days via COPY, timescale chunk-exclusion validated. |
| **3 — Selective + safe restore** | Manifest-driven category picker, compatibility checks, empty-vs-merge, dry-run, pre-restore snapshot, live-connection detection. |
| **4 — File stores + crypto** | vc-repos/reports tar backup+restore, encryption (age), keychain, checksum verification UI. |
| **5 — Windows + packaging** | Bundle pg client tools, Windows installer, signed builds; backup history; (optional) scheduling. |
| **6 — Polish** | Docs, error UX, i18n, telemetry-of-the-app (opt-in), release. |

MVP target = end of Phase 1 (a working full backup+restore on Linux); the
"select what to back up" headline feature lands in Phase 2.

---

## 12. Resolved decisions (v1)

1. **Bundle the PostgreSQL 18 client tools** (`pg_dump`/`pg_restore` + `libpq`)
   in the installer — self-contained, no user install required (§8 option 2).
2. **DB + file-store backup**, but file stores are **conditional on
   reachability**: the app auto-detects whether it runs on the *same machine*
   as SENTIENT **and** can read the `vc-repos`/`reports` paths. If yes → the
   Version-control and Reports categories are **enabled**; if not (e.g. remote,
   or paths locked inside a Docker volume) → those categories are **greyed out
   / disabled** with a reason tooltip. DB backup always works over TCP.
3. **Telemetry = none / all / last-N-days** (COPY stream, §4). Per-tenant /
   per-device telemetry selection is **deferred to a later release**.
4. **Restore = empty-DB-only** for v1 (safe, no merge conflicts). Detect a
   non-empty target and block with guidance. Merge-into-populated is deferred.
5. **Encryption is a user setting (on/off toggle)**, not forced. When off, show
   a clear warning that the backup contains secrets. `age` when on (§4/§9).
6. **Scope = whole-instance** (single `sentient` DB) for v1. **Per-tenant
   backup & restore is a planned later feature** — keep the category/table
   model tenant-aware in the core so it can be filtered by `tenant_id` later.

These refine the body above: §5 is empty-DB-only for v1 (merge deferred); §6's
file-store handling is the auto-enable/disable behavior in (2); §4 telemetry is
none/all/last-N-days with per-tenant deferred.

---

## 13. Repository layout

```
SENTIENT_BACKUP/
├─ README.md
├─ docs/
│   └─ RESEARCH_AND_PLAN.md          ← this file
├─ crates/
│   └─ sentient-backup-core/         ← UI-agnostic Rust engine (+ CLI bin)
├─ src-tauri/                        ← Tauri v2 shell (commands/events)
├─ src/                              ← Svelte/TS frontend
└─ .github/workflows/               ← Linux + Windows build/release
```
