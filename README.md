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
**Planning.** See [`docs/RESEARCH_AND_PLAN.md`](docs/RESEARCH_AND_PLAN.md) for
the full research, data-model categorization, backup/restore strategy,
architecture, cross-platform approach, and the phased implementation plan.

## Planned layout
```
crates/sentient-backup-core/   Rust engine (+ CLI)
src-tauri/                     Tauri v2 shell
src/                           Svelte/TS frontend
docs/                          design docs
```
