use std::io;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, CameraError>;

#[derive(Debug, Error)]
pub enum CameraError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("channel disconnected: {0}")]
    Channel(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("timeout: {0}")]
    Timeout(String),

    #[error("end of stream")]
    Eof,

    #[error("operation failed: {0}")]
    Other(String),
}
