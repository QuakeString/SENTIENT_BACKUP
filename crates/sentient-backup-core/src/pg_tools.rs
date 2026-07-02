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

/// Build a `Command` for a pg tool that never flashes a console window on
/// Windows. `pg_dump`/`pg_restore` are console programs; spawned from the GUI
/// they'd otherwise pop up an empty terminal. No-op on other platforms.
pub fn command(bin: &Path) -> Command {
    let cmd = Command::new(bin);
    #[cfg(windows)]
    let cmd = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut c = cmd;
        c.creation_flags(CREATE_NO_WINDOW);
        c
    };
    cmd
}

fn tool_version(bin: &Path) -> Result<String> {
    let out = command(bin)
        .arg("--version")
        .output()
        .map_err(|e| Error::msg(format!("running {}: {e}", bin.display())))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn parse_major(version_line: &str) -> Option<u32> {
    // first whitespace-separated token that begins with a digit (the version),
    // then its leading digits (major). Ignores trailing "(Ubuntu)" etc.
    for tok in version_line.split_whitespace() {
        if tok.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            let major: String = tok.chars().take_while(|c| c.is_ascii_digit()).collect();
            return major.parse().ok();
        }
    }
    None
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
