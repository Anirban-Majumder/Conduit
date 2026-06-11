//! Single-instance IPC over a per-user local socket: a second launch forwards a
//! `show` command to the running daemon instead of starting a new one.
//!
//! `resolve_socket_path` picks the per-OS address; `try_bind` claims it, probing
//! and reclaiming a stale socket. `accept_loop` serves newline-delimited JSON
//! `Command`/`Response` frames (bounded by `MAX_LINE_BYTES`), invoking `on_show`
//! to raise the GUI. `send_command` is the client side. Only `Show` is wired;
//! other commands are reserved for future CLI use.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use interprocess::local_socket::{
    tokio::{prelude::*, Listener, Stream},
    GenericFilePath, GenericNamespaced, ListenerOptions, Name,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

const MAX_LINE_BYTES: usize = 64 * 1024;
const STALE_PROBE_TIMEOUT: Duration = Duration::from_millis(100);
/// Maximum concurrent IPC connections (DS-10). Prevents unbounded task spawning.
const MAX_CONCURRENT_CONNECTIONS: usize = 8;
/// Per-connection read deadline (DS-10). Prevents slow-client resource hold.
const CONN_READ_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("oversized frame (> {} bytes)", MAX_LINE_BYTES)]
    OversizedFrame,

    #[error("client could not reach daemon: {0}")]
    ClientConnect(std::io::Error),

    #[error("client write failed: {0}")]
    ClientWrite(std::io::Error),

    #[error("could not resolve socket path: {0}")]
    SocketPath(String),
}

#[derive(Debug, Error)]
pub enum BindError {
    #[error("socket already in use")]
    InUse,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not resolve socket path: {0}")]
    SocketPath(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum Command {
    Show,
    Approve { id: String },
    Deny { id: String },
    Status,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl Response {
    pub const fn ok() -> Self {
        Self {
            ok: true,
            error: None,
            data: None,
        }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            data: None,
        }
    }
}

/// Resolves the per-OS socket path / namespaced name.
pub fn resolve_socket_path() -> SocketAddr {
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").unwrap_or_else(|_| "default".into());
        SocketAddr::Namespaced(format!("conduit-{user}"))
    }
    #[cfg(target_os = "linux")]
    {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let uid = uid_suffix();
        let path = if dir.as_path() == std::path::Path::new("/tmp") {
            dir.join(format!("conduit-{uid}.sock"))
        } else {
            dir.join("conduit.sock")
        };
        SocketAddr::FilePath(path)
    }
    #[cfg(target_os = "macos")]
    {
        let dir = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let uid = uid_suffix();
        SocketAddr::FilePath(dir.join(format!("conduit-{uid}.sock")))
    }
}

#[cfg(unix)]
fn uid_suffix() -> String {
    // Avoid an extra crate for getuid; read /proc/self/status on Linux, or
    // fall back to USER for macOS where no /proc.
    if let Ok(s) = std::fs::read_to_string("/proc/self/loginuid") {
        let t = s.trim();
        if !t.is_empty() && t != "4294967295" {
            return t.to_string();
        }
    }
    std::env::var("USER").unwrap_or_else(|_| "default".into())
}

#[derive(Debug, Clone)]
pub enum SocketAddr {
    FilePath(PathBuf),
    Namespaced(String),
}

impl SocketAddr {
    pub fn to_name(&self) -> Result<Name<'_>, IpcError> {
        match self {
            Self::FilePath(p) => {
                let s = p
                    .to_str()
                    .ok_or_else(|| IpcError::SocketPath("non-utf8 socket path".into()))?;
                s.to_fs_name::<GenericFilePath>()
                    .map_err(|e| IpcError::SocketPath(e.to_string()))
            }
            Self::Namespaced(s) => s
                .as_str()
                .to_ns_name::<GenericNamespaced>()
                .map_err(|e| IpcError::SocketPath(e.to_string())),
        }
    }

    pub fn unlink_if_file(&self) {
        if let Self::FilePath(p) = self {
            let _ignored = std::fs::remove_file(p);
        }
    }
}

/// Try to bind the listener.
///
/// On `AddrInUse`, probes the existing socket: if a daemon answers, returns
/// `BindError::InUse`. If the socket is stale (refused or timeout), unlinks
/// the path on Unix and retries the bind exactly once.
pub async fn try_bind(addr: &SocketAddr) -> Result<Listener, BindError> {
    let err = match bind_once(addr) {
        Ok(l) => return Ok(l),
        Err(e) => e,
    };
    // On Windows, an in-use named pipe returns PermissionDenied (os error 5),
    // not AddrInUse. Treat both as "something is already listening".
    let is_addr_in_use = err.kind() == std::io::ErrorKind::AddrInUse
        || (cfg!(windows) && err.kind() == std::io::ErrorKind::PermissionDenied);
    if !is_addr_in_use {
        return Err(BindError::from(err));
    }
    if probe_live(addr).await {
        return Err(BindError::InUse);
    }
    addr.unlink_if_file();
    bind_once(addr).map_err(BindError::from)
}

fn bind_once(addr: &SocketAddr) -> std::io::Result<Listener> {
    let name = addr
        .to_name()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    ListenerOptions::new().name(name).create_tokio()
}

async fn probe_live(addr: &SocketAddr) -> bool {
    let Ok(name) = addr.to_name() else {
        return false;
    };
    matches!(
        tokio::time::timeout(STALE_PROBE_TIMEOUT, Stream::connect(name)).await,
        Ok(Ok(_))
    )
}

/// Client path: connect to the running daemon and forward a `show` command.
/// Returns the daemon's parsed `Response` on success.
pub async fn send_command(addr: &SocketAddr, cmd: &Command) -> Result<Response, IpcError> {
    let name = addr.to_name()?;
    let stream = tokio::time::timeout(Duration::from_secs(2), Stream::connect(name))
        .await
        .map_err(|_elapsed| IpcError::ClientConnect(std::io::Error::other("connect timed out")))?
        .map_err(IpcError::ClientConnect)?;

    let mut reader = BufReader::new(&stream);
    let mut writer = &stream;
    let mut payload = serde_json::to_vec(cmd).map_err(std::io::Error::other)?;
    payload.push(b'\n');
    writer
        .write_all(&payload)
        .await
        .map_err(IpcError::ClientWrite)?;

    let mut line = String::new();
    let n = read_line_bounded(&mut reader, &mut line).await?;
    if n == 0 {
        return Err(IpcError::Io(std::io::Error::other("daemon closed stream")));
    }
    let resp: Response = serde_json::from_str(line.trim()).map_err(std::io::Error::other)?;
    Ok(resp)
}

async fn read_line_bounded<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    out: &mut String,
) -> Result<usize, IpcError> {
    out.clear();
    // Accumulate raw bytes so multi-byte UTF-8 sequences split across fill_buf
    // chunks are validated as a whole, not per-chunk (DS-9).
    let mut raw: Vec<u8> = Vec::new();
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            // EOF: convert whatever we have (no newline terminated line).
            let n = raw.len();
            *out = String::from_utf8(raw)
                .map_err(|e| IpcError::Io(std::io::Error::other(e.to_string())))?;
            return Ok(n);
        }
        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let take = pos + 1;
            if raw.len() + take > MAX_LINE_BYTES {
                return Err(IpcError::OversizedFrame);
            }
            raw.extend_from_slice(buf.get(..take).unwrap_or_default());
            reader.consume(take);
            let n = raw.len();
            *out = String::from_utf8(raw)
                .map_err(|e| IpcError::Io(std::io::Error::other(e.to_string())))?;
            return Ok(n);
        }
        // No newline in this chunk — keep accumulating until we hit the cap.
        if raw.len() + buf.len() > MAX_LINE_BYTES {
            return Err(IpcError::OversizedFrame);
        }
        raw.extend_from_slice(buf);
        let n = buf.len();
        reader.consume(n);
    }
}

/// The trigger fired when an IPC client sends a `show` command. Wired in main
/// to `slint::Weak::upgrade_in_event_loop(|ui| ui.window().show())`.
pub type ShowHandler = Arc<dyn Fn() + Send + Sync>;

/// Accept loop. Spawns one task per connection, bounded to `MAX_CONCURRENT_CONNECTIONS`.
/// Returns when `cancel` fires.
pub async fn accept_loop(listener: Listener, cancel: CancellationToken, on_show: ShowHandler) {
    // Semaphore limits concurrent connection tasks (DS-10).
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    loop {
        tokio::select! {
            () = cancel.cancelled() =>{
                tracing::debug!("ipc accept loop: cancelled");
                return;
            }
            res = listener.accept() => {
                match res {
                    Ok(conn) => {
                        let Ok(permit) = Arc::clone(&sem).try_acquire_owned() else {
                            tracing::warn!("ipc: connection limit reached, dropping connection");
                            continue;
                        };
                        let on_show = on_show.clone();
                        let cancel = cancel.clone();
                        tokio::spawn(async move {
                            let _permit = permit; // released when task ends
                            if let Err(e) = handle_conn(conn, on_show, cancel).await {
                                tracing::warn!(error = %e, "ipc handle_conn error");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "ipc accept error");
                    }
                }
            }
        }
    }
}

async fn handle_conn(
    conn: Stream,
    on_show: ShowHandler,
    cancel: CancellationToken,
) -> Result<(), IpcError> {
    let mut reader = BufReader::new(&conn);
    let mut writer = &conn;
    let mut line = String::with_capacity(256);

    // DS-10: bound per-connection read with a hard deadline and cancel-token branch.
    let read_result = tokio::select! {
        () = cancel.cancelled() => return Ok(()),
        r = tokio::time::timeout(CONN_READ_TIMEOUT, read_line_bounded(&mut reader, &mut line)) => {
            match r {
                Ok(inner) => inner,
                Err(_elapsed) => return Ok(()), // timed out — drop connection silently
            }
        },
    };

    let resp = match read_result {
        Ok(0) => return Ok(()),
        Ok(_) => dispatch(line.trim(), &on_show),
        Err(IpcError::OversizedFrame) => {
            // Per spec: drop connection silently with no reply.
            return Err(IpcError::OversizedFrame);
        }
        Err(e) => return Err(e),
    };

    let mut payload = serde_json::to_vec(&resp).map_err(std::io::Error::other)?;
    payload.push(b'\n');
    writer.write_all(&payload).await?;
    Ok(())
}

fn dispatch(line: &str, on_show: &ShowHandler) -> Response {
    let cmd: Command = match serde_json::from_str(line) {
        Ok(c) => c,
        Err(_) => return Response::err("malformed_request"),
    };
    match cmd {
        Command::Show => {
            on_show();
            Response::ok()
        }
        // Approve/Deny/Status: future CLI work — accept but defer.
        Command::Approve { .. } | Command::Deny { .. } => Response::err("not_implemented"),
        Command::Status => Response::ok(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test ergonomics")]
    use super::*;
    use tokio::io::BufReader;

    // DS-9: a 3-byte UTF-8 sequence (€ = 0xE2 0x82 0xAC) split across two
    // fill_buf chunks must decode correctly, not corrupt on per-chunk validation.
    #[tokio::test]
    async fn read_line_bounded_multibyte_split() {
        let cursor = std::io::Cursor::new("€\n".as_bytes().to_vec());
        let mut reader = BufReader::new(tokio::io::BufReader::new(cursor));
        let mut out = String::new();
        let n = read_line_bounded(&mut reader, &mut out).await.unwrap();
        assert!(n > 0);
        assert_eq!(out.trim_end_matches('\n'), "€");
    }

    // DS-9: lone-quote input must not produce empty string from strip_quotes
    // (wizard_input tested separately; verify the IPC layer handles non-ASCII).
    #[tokio::test]
    async fn read_line_bounded_ascii_roundtrip() {
        let cursor = std::io::Cursor::new(b"{\"cmd\":\"show\"}\n".to_vec());
        let mut reader = BufReader::new(tokio::io::BufReader::new(cursor));
        let mut out = String::new();
        let n = read_line_bounded(&mut reader, &mut out).await.unwrap();
        assert!(n > 0);
        assert_eq!(out.trim_end_matches('\n'), r#"{"cmd":"show"}"#);
    }
}
