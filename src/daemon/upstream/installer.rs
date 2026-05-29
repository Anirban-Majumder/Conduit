//! pnpm/npm/uv/pipx install of Node/Python MCP upstreams.
//!
//! Per spec §"Server installation", installs into `<installs_dir>`.
//! `install_node` / `install_python` install once, then callers spawn the
//! resulting binary directly via `stdio::connect`.
//!
//! Never `npx -y` at runtime — the 5+ second cold-fetch tax is unacceptable on
//! the request path (spec gotcha + CLAUDE.md).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Error)]
pub enum InstallerError {
    #[error("no supported package manager found: tried {tried}")]
    NotFound { tried: &'static str },

    #[error("{manager} failed (exit {exit_code}): {stderr}")]
    CommandFailed {
        manager: &'static str,
        exit_code: i32,
        stderr: String,
    },

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Install a Node MCP server package into `<installs_dir>` via pnpm
/// (preferred) or npm (fallback). Returns the absolute path to the binary
/// the upstream exposes inside `<installs_dir>/node_modules/.bin/`.
pub async fn install_node(installs_dir: &Path, package: &str) -> Result<PathBuf, InstallerError> {
    tokio::fs::create_dir_all(installs_dir).await?;

    if probe("pnpm").await {
        run_pm("pnpm", &["add", "--prefix"], installs_dir, package).await?;
    } else if probe("npm").await {
        run_pm("npm", &["install", "--prefix"], installs_dir, package).await?;
    } else {
        return Err(InstallerError::NotFound { tried: "pnpm, npm" });
    }

    Ok(node_bin_path(installs_dir, package))
}

/// Install a Python MCP server into `<installs_dir>/python-tools` via uv
/// (preferred) or pipx (fallback).
pub async fn install_python(installs_dir: &Path, package: &str) -> Result<PathBuf, InstallerError> {
    let tools_dir = installs_dir.join("python-tools");
    tokio::fs::create_dir_all(&tools_dir).await?;

    if probe("uv").await {
        run_uv(&tools_dir, package).await?;
    } else if probe("pipx").await {
        run_pipx(&tools_dir, package).await?;
    } else {
        return Err(InstallerError::NotFound { tried: "uv, pipx" });
    }

    Ok(python_bin_path(&tools_dir, package))
}

async fn probe(binary: &str) -> bool {
    Command::new(binary)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

async fn run_pm(
    manager: &'static str,
    subcmd: &[&str],
    installs_dir: &Path,
    package: &str,
) -> Result<(), InstallerError> {
    let output = Command::new(manager)
        .args(subcmd)
        .arg(installs_dir)
        .arg(package)
        .stdin(Stdio::null())
        .output()
        .await?;
    if output.status.success() {
        tracing::info!(manager, package, "installed node package");
        Ok(())
    } else {
        Err(InstallerError::CommandFailed {
            manager,
            exit_code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

async fn run_uv(tools_dir: &Path, package: &str) -> Result<(), InstallerError> {
    let output = Command::new("uv")
        .arg("tool")
        .arg("install")
        .arg(package)
        .arg("--tools-dir")
        .arg(tools_dir)
        .stdin(Stdio::null())
        .output()
        .await?;
    if output.status.success() {
        tracing::info!(package, "installed python package via uv");
        Ok(())
    } else {
        Err(InstallerError::CommandFailed {
            manager: "uv",
            exit_code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

async fn run_pipx(tools_dir: &Path, package: &str) -> Result<(), InstallerError> {
    let output = Command::new("pipx")
        .arg("install")
        .arg(package)
        .env("PIPX_HOME", tools_dir)
        .env("PIPX_BIN_DIR", tools_dir.join("bin"))
        .stdin(Stdio::null())
        .output()
        .await?;
    if output.status.success() {
        tracing::info!(package, "installed python package via pipx");
        Ok(())
    } else {
        Err(InstallerError::CommandFailed {
            manager: "pipx",
            exit_code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn unscoped_tail(package: &str) -> &str {
    package.rsplit('/').next().unwrap_or(package)
}

fn node_bin_path(installs_dir: &Path, package: &str) -> PathBuf {
    let bin = unscoped_tail(package);
    let exe = if cfg!(windows) {
        format!("{bin}.cmd")
    } else {
        bin.to_string()
    };
    installs_dir.join("node_modules").join(".bin").join(exe)
}

fn python_bin_path(tools_dir: &Path, package: &str) -> PathBuf {
    let bin = unscoped_tail(package);
    let exe = if cfg!(windows) {
        format!("{bin}.exe")
    } else {
        bin.to_string()
    };
    tools_dir.join("bin").join(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unscoped_tail_strips_scope() {
        assert_eq!(
            unscoped_tail("@modelcontextprotocol/server-filesystem"),
            "server-filesystem"
        );
        assert_eq!(unscoped_tail("mcp-server-time"), "mcp-server-time");
        assert_eq!(unscoped_tail("@upstash/context7-mcp"), "context7-mcp");
    }

    #[test]
    fn node_bin_path_under_node_modules_dot_bin() {
        let p = node_bin_path(
            Path::new("/installs"),
            "@modelcontextprotocol/server-filesystem",
        );
        let tail = if cfg!(windows) {
            "node_modules/.bin/server-filesystem.cmd"
        } else {
            "node_modules/.bin/server-filesystem"
        };
        assert!(p.ends_with(tail), "got {p:?}");
    }

    #[test]
    fn python_bin_path_under_tools_bin() {
        let p = python_bin_path(Path::new("/tools"), "mcp-atlassian");
        let tail = if cfg!(windows) {
            "bin/mcp-atlassian.exe"
        } else {
            "bin/mcp-atlassian"
        };
        assert!(p.ends_with(tail), "got {p:?}");
    }
}
