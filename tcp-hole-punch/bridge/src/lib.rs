pub mod stream;
pub mod swarm;
pub mod tonic_integration;

/// Top-level error type for the bridge.
#[derive(thiserror::Error, Debug)]
pub enum BridgeError {
    #[error("swarm command channel closed")]
    CommandChannelClosed,

    #[error("dial attempt failed: {0}")]
    DialFailed(String),

    #[error("incoming stream channel closed")]
    IncomingClosed,

    #[error("underlying IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result alias for bridge operations.
pub type Result<T> = std::result::Result<T, BridgeError>;
