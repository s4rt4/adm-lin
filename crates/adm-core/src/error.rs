//! Tipe error engine.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("server tidak mengembalikan ukuran (Content-Length/Content-Range)")]
    UnknownSize,

    #[error("status http tak terduga: {0}")]
    BadStatus(u16),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
