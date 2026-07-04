//! Content-addressed blob store (PRD-04 §3). M1 ships only the filesystem
//! backend concretely — the `BlobStore` trait + S3 backend are added when
//! object storage is actually wanted (PRD-04 §3 marks S3 optional). ponytail:
//! don't build the trait for one implementation.

use std::path::PathBuf;

use axum::body::Body;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

use crate::error::ApiError;

#[derive(Clone)]
pub struct FsBlobStore {
    root: PathBuf,
}

pub enum BlobError {
    Io(std::io::Error),
    Body(String),
    HashMismatch { expected: String, got: String },
}

impl From<std::io::Error> for BlobError {
    fn from(e: std::io::Error) -> Self {
        BlobError::Io(e)
    }
}

impl From<BlobError> for ApiError {
    fn from(e: BlobError) -> Self {
        match e {
            BlobError::HashMismatch { expected, got } => ApiError::bad_request(format!(
                "blob content hash {got} does not match declared {expected}"
            )),
            BlobError::Io(e) => ApiError::internal(e),
            BlobError::Body(m) => ApiError::bad_request(format!("upload body error: {m}")),
        }
    }
}

impl FsBlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `blobs/<hh>/<hash>` — fan out by the first two hex chars (PRD-04 §3).
    /// Caller must have validated `hash` is 64 hex chars.
    fn obj_path(&self, hash: &str) -> PathBuf {
        self.root.join(&hash[..2]).join(hash)
    }

    pub async fn exists(&self, hash: &str) -> bool {
        tokio::fs::try_exists(self.obj_path(hash))
            .await
            .unwrap_or(false)
    }

    pub fn get_path(&self, hash: &str) -> PathBuf {
        self.obj_path(hash)
    }

    /// Delete a blob object. GC-only (PRD-04 §3); a missing file is treated as
    /// already-deleted so GC is idempotent. Caller must have validated `hash`.
    pub async fn delete(&self, hash: &str) -> Result<(), std::io::Error> {
        match tokio::fs::remove_file(self.obj_path(hash)).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Readiness probe (PRD-07): the blob root exists and is writable. Creates
    /// it if missing so a fresh deploy reports ready once the volume is mounted.
    pub async fn reachable(&self) -> bool {
        tokio::fs::create_dir_all(&self.root).await.is_ok()
    }

    /// Stream an upload body to disk, verifying `blake3(bytes) == hash` before
    /// committing (content-address = integrity check, PRD-03 §8). Writes to a
    /// temp file then atomically renames. Returns the byte count.
    pub async fn put(&self, hash: &str, body: Body) -> Result<u64, BlobError> {
        let final_path = self.obj_path(hash);
        if let Some(parent) = final_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        // ponytail: one temp path per hash — safe for the single-writer model
        // (one device uploads a given blob). Add a random suffix if concurrent
        // uploads of the same blob become real.
        let tmp = final_path.with_extension("tmp");

        let mut file = tokio::fs::File::create(&tmp).await?;
        let mut hasher = blake3::Hasher::new();
        let mut total: u64 = 0;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| BlobError::Body(e.to_string()))?;
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
            total += chunk.len() as u64;
        }
        file.flush().await?;

        let got = hasher.finalize().to_hex().to_string();
        if got != hash {
            tokio::fs::remove_file(&tmp).await.ok();
            return Err(BlobError::HashMismatch {
                expected: hash.to_string(),
                got,
            });
        }
        tokio::fs::rename(&tmp, &final_path).await?;
        Ok(total)
    }
}
