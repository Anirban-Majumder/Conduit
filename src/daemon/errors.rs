use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("db actor channel closed")]
    ActorGone,

    #[error("spawning db actor thread: {0}")]
    Spawn(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("spawning tokio worker thread: {0}")]
    SpawnThread(#[from] std::io::Error),

    #[error("tokio worker thread died before publishing its runtime handle")]
    WorkerDied,
}
