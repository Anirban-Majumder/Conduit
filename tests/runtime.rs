#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test ergonomics")]

use std::sync::mpsc;
use std::time::Duration;

use mcp_router::runtime::Worker;
use tokio_util::sync::CancellationToken;

#[test]
fn handle_spawns_async_task() {
    let cancel = CancellationToken::new();
    let worker = Worker::spawn(cancel).expect("spawn worker");

    let (tx, rx) = mpsc::channel();
    worker.handle().spawn(async move {
        tx.send("ran").ok();
    });

    let result = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("task ran on worker runtime");
    assert_eq!(result, "ran");
}

#[test]
fn worker_joins_within_timeout_after_cancel() {
    let cancel = CancellationToken::new();
    let worker = Worker::spawn(cancel.clone()).expect("spawn worker");

    cancel.cancel();

    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        drop(worker);
        done_tx.send(()).ok();
    });

    done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("worker dropped within 10s of cancel");
}
