#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test ergonomics"
)]

use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};
use std::time::Duration;

use mcp_router::daemon::ipc::{
    self, accept_loop, send_command, try_bind, BindError, Command, ShowHandler, SocketAddr,
};
use tokio_util::sync::CancellationToken;

fn unique_addr(tag: &str) -> SocketAddr {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let mut p = std::env::temp_dir();
    p.push(format!("mcp-router-ipc-{}-{tag}-{n}.sock", std::process::id()));
    let _ignored = std::fs::remove_file(&p);
    SocketAddr::FilePath(p)
}

fn noop_show() -> ShowHandler {
    Arc::new(|| {})
}

#[tokio::test]
async fn bind_succeeds_when_socket_absent() {
    let addr = unique_addr("bind-absent");
    let listener = try_bind(&addr).await.expect("bind should succeed");
    drop(listener);
    addr.unlink_if_file();
}

#[tokio::test]
async fn second_bind_returns_in_use_while_first_alive() {
    let addr = unique_addr("inuse");
    let listener1 = try_bind(&addr).await.expect("first bind");

    let cancel = CancellationToken::new();
    let task = tokio::spawn(accept_loop(listener1, cancel.clone(), noop_show()));

    // Give the accept loop a tick to be ready to accept the probe connect.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let err = try_bind(&addr).await.expect_err("second bind should fail");
    assert!(matches!(err, BindError::InUse), "got {err:?}");

    cancel.cancel();
    let _ignored = tokio::time::timeout(Duration::from_secs(2), task).await;
    addr.unlink_if_file();
}

#[tokio::test]
async fn stale_socket_path_is_rebound() {
    let addr = unique_addr("stale");
    // Create a stale file at the socket path — nobody listening.
    if let SocketAddr::FilePath(p) = &addr {
        std::fs::write(p, b"stale").expect("create stale file");
    }
    let listener = try_bind(&addr)
        .await
        .expect("bind should rebind over stale path");
    drop(listener);
    addr.unlink_if_file();
}

#[tokio::test]
async fn show_command_round_trip_fires_handler() {
    let addr = unique_addr("show");
    let listener = try_bind(&addr).await.expect("bind");

    let flag = Arc::new(AtomicBool::new(false));
    let flag_for_handler = flag.clone();
    let on_show: ShowHandler = Arc::new(move || {
        flag_for_handler.store(true, Ordering::SeqCst);
    });

    let cancel = CancellationToken::new();
    let task = tokio::spawn(accept_loop(listener, cancel.clone(), on_show));

    let resp = send_command(&addr, &Command::Show).await.expect("send_command");
    assert!(resp.ok, "response: {resp:?}");
    assert!(flag.load(Ordering::SeqCst), "on_show should fire");

    cancel.cancel();
    let _ignored = tokio::time::timeout(Duration::from_secs(2), task).await;
    addr.unlink_if_file();
}

#[tokio::test]
async fn malformed_request_yields_error_response() {
    use interprocess::local_socket::tokio::{prelude::*, Stream};
    use tokio::io::AsyncWriteExt;

    let addr = unique_addr("malformed");
    let listener = try_bind(&addr).await.expect("bind");
    let cancel = CancellationToken::new();
    let task = tokio::spawn(accept_loop(listener, cancel.clone(), noop_show()));

    let name = addr.to_name().expect("name");
    let stream = Stream::connect(name).await.expect("connect");
    let mut writer = &stream;
    writer.write_all(b"this is not json\n").await.unwrap();
    let mut reader = tokio::io::BufReader::new(&stream);
    let mut line = String::new();
    tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line)
        .await
        .unwrap();
    let resp: ipc::Response = serde_json::from_str(line.trim()).expect("parse");
    assert!(!resp.ok);
    assert_eq!(resp.error.as_deref(), Some("malformed_request"));

    cancel.cancel();
    let _ignored = tokio::time::timeout(Duration::from_secs(2), task).await;
    addr.unlink_if_file();
}

#[tokio::test]
async fn oversized_line_dropped_without_reply() {
    use interprocess::local_socket::tokio::{prelude::*, Stream};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let addr = unique_addr("oversize");
    let listener = try_bind(&addr).await.expect("bind");
    let cancel = CancellationToken::new();
    let task = tokio::spawn(accept_loop(listener, cancel.clone(), noop_show()));

    let name = addr.to_name().expect("name");
    let stream = Stream::connect(name).await.expect("connect");
    let mut writer = &stream;
    let oversize = vec![b'A'; 65 * 1024];
    // No newline — daemon must reject and drop the connection.
    let _ignored = writer.write_all(&oversize).await;
    let _ignored = writer.flush().await;

    let mut reader = &stream;
    let mut buf = [0u8; 16];
    // Read should return EOF (or error) without yielding any reply payload.
    let n = tokio::time::timeout(Duration::from_secs(2), reader.read(&mut buf))
        .await
        .expect("read should not hang")
        .unwrap_or(0);
    assert_eq!(n, 0, "expected no reply, got {n} bytes");

    cancel.cancel();
    let _ignored = tokio::time::timeout(Duration::from_secs(2), task).await;
    addr.unlink_if_file();
}

#[tokio::test]
async fn cancel_returns_from_accept_loop_promptly() {
    let addr = unique_addr("cancel");
    let listener = try_bind(&addr).await.expect("bind");
    let cancel = CancellationToken::new();
    let task = tokio::spawn(accept_loop(listener, cancel.clone(), noop_show()));

    tokio::time::sleep(Duration::from_millis(20)).await;
    cancel.cancel();

    let res = tokio::time::timeout(Duration::from_millis(500), task)
        .await
        .expect("accept_loop should exit within 500ms of cancel");
    res.unwrap();
    addr.unlink_if_file();
}
