//! Locating and describing the PostgreSQL client tools (`pg_dump` /
//! `pg_restore`). Resolution order:
//!   1. `SBR_PG_DUMP` / `SBR_PG_RESTORE` env vars (explicit path — also used to
//!      point at a wrapper, e.g. one that shells into a docker container).
//!   2. bundled tools next to the executable (added in a later phase).
//!   3. the system `PATH`.
//!
//! The tool major version must be >= the server's (a Postgres rule); we surface
//! the version so the app can warn on a mismatch.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct PgTools {
    pub pg_dump: PathBuf,
    pub pg_restore: PathBuf,
}

impl PgTools {
    pub fn resolve() -> Result<Self> {
        Ok(Self {
            pg_dump: resolve_one("pg_dump", "SBR_PG_DUMP")?,
            pg_restore: resolve_one("pg_restore", "SBR_PG_RESTORE")?,
        })
    }

    /// e.g. "pg_dump (PostgreSQL) 18.3"
    pub fn dump_version(&self) -> Result<String> {
        tool_version(&self.pg_dump)
    }

    /// Parsed major version of `pg_dump`, if determinable.
    pub fn dump_major(&self) -> Option<u32> {
        parse_major(&self.dump_version().ok()?)
    }
}

fn resolve_one(name: &str, env: &str) -> Result<PathBuf> {
    if let Ok(p) = std::env::var(env) {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    which::which(name).map_err(|_| {
        Error::msg(format!(
            "'{name}' not found. Install PostgreSQL 18 client tools, or set {env} to its path."
        ))
    })
}

fn tool_version(bin: &Path) -> Result<String> {
    let out = Command::new(bin)
        .arg("--version")
        .output()
        .map_err(|e| Error::msg(format!("running {}: {e}", bin.display())))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn parse_major(version_line: &str) -> Option<u32> {
    // last whitespace-separated token, first dot-separated number
    version_line
        .split_whitespace()
        .last()?
        .split('.')
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::parse_major;
    #[test]
    fn parses() {
        assert_eq!(parse_major("pg_dump (PostgreSQL) 18.3"), Some(18));
        assert_eq!(parse_major("pg_restore (PostgreSQL) 16.2 (Ubuntu)"), Some(16));
    }
}
