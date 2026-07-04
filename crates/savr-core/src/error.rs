use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest: {0}")]
    Manifest(String),
    #[error("hash: {0}")]
    Hash(String),
    #[error("glob: {0}")]
    Glob(String),
}

pub type Result<T> = std::result::Result<T, Error>;
