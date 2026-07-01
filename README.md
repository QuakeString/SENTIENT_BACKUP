# SENTIENT Backup & Restore

A cross-platform (Windows + Linux) desktop application to **back up** and
**restore** a [SENTIENT](../SENTIENT) deployment, with **user-selectable data
categories** — e.g. skip the multi-GB telemetry history to keep a backup small.

Built with **Rust + Tauri**. A UI-agnostic core engine (`sentient-backup-core`)
does the real work and doubles as a headless CLI; the Tauri app is a thin GUI
over it.

## Why
A SENTIENT instance is PostgreSQL 18 + TimescaleDB plus two on-disk stores
(`vc-repos`, `reports`). ~99 % of the size is one telemetry hypertable
(`ts_kv`), so a full `pg_dump` is huge and slow. This tool lets you choose what
to include (Configuration, Telemetry, Attributes, Alarms, Audit logs, RPC,
Reports, …) and restore selectively and safely.

## Status
**Phase 0 (scaffold) — done.** The core engine + CLI work end-to-end against a
live SENTIENT database (connect, enumerate tables, roll up per-category sizes
incl. the Timescale telemetry hypertable). The Tauri shell + frontend +
multi-OS CI are scaffolded. See
[`docs/RESEARCH_AND_PLAN.md`](docs/RESEARCH_AND_PLAN.md) for the full plan and
the phase breakdown (Phase 1 = MVP full backup/restore; Phase 2 = the selective
backup feature).

## Getting started (Phase 0)

```bash
# Build + test the core engine and CLI
cargo build
cargo test

# Inspect a live SENTIENT database — prints each backup component's size
cargo run --bin sbr -- inspect --host localhost --user sentient --password <pw>
cargo run --bin sbr -- categories          # the static component model
```

`sbr inspect` shows exactly how much each component contributes — e.g. skipping
"Telemetry (historical)" is what turns a multi-GB backup into a small one.

The Tauri desktop app lives in `src-tauri/` (its own workspace, depends on the
core by path). Building it needs the platform WebView deps + the Tauri CLI; CI
compiles it on Linux + Windows.

## Layout
```
crates/sentient-backup-core/   Rust engine + `sbr` CLI  (DbInspector, category model)
src-tauri/                     Tauri v2 shell (commands over the core)
src/                           frontend (static HTML/JS for now; Svelte later)
docs/                          design docs
.github/workflows/ci.yml       Linux/Windows/macOS build + test
```
