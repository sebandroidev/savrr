//! Ludusavi-manifest fetch + cache (PRD-02 §1.1). Downloads the YAML save-path
//! database from GitHub, honoring `ETag`/`If-None-Match` so a daily refresh is
//! a cheap `304` when nothing changed, and caches it on disk so the daemon
//! works offline. Parsing is delegated to `savr_core::manifest::parse`.

use std::path::{Path, PathBuf};

use savr_core::manifest::{self, Manifest};

/// The upstream manifest URL (PRD-02 §1.1). The only third-party outbound call
/// the product makes (PRD-06 §8).
pub const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/mtkennerly/ludusavi-manifest/master/data/manifest.yaml";

const MANIFEST_FILE: &str = "manifest.yaml";
const ETAG_FILE: &str = "manifest.etag";

/// What a refresh produced.
pub struct RefreshOutcome {
    pub manifest: Manifest,
    /// True if the server returned `304` and we used the cached copy.
    pub not_modified: bool,
    pub entry_count: usize,
}

fn manifest_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(MANIFEST_FILE)
}

fn etag_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(ETAG_FILE)
}

/// Parse whatever is currently cached, if anything. Used at startup so an
/// offline daemon still has its games DB.
pub fn load_cached(cache_dir: &Path) -> anyhow::Result<Option<Manifest>> {
    match std::fs::read_to_string(manifest_path(cache_dir)) {
        Ok(text) => Ok(Some(manifest::parse(&text)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Fetch (conditionally), cache, and parse the manifest. On a network failure
/// with a cached copy present, falls back to the cache rather than erroring —
/// a flaky connection must not blind the daemon.
pub async fn refresh(cache_dir: &Path) -> anyhow::Result<RefreshOutcome> {
    std::fs::create_dir_all(cache_dir)?;
    let client = reqwest::Client::builder()
        .user_agent("savr-daemon")
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let stored_etag = std::fs::read_to_string(etag_path(cache_dir)).ok();
    let mut req = client.get(MANIFEST_URL);
    if let Some(etag) = &stored_etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, etag.trim());
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            // Offline: use the cache if we have one.
            if let Some(manifest) = load_cached(cache_dir)? {
                tracing::warn!("manifest fetch failed ({e}); using cached copy");
                let entry_count = manifest.len();
                return Ok(RefreshOutcome {
                    manifest,
                    not_modified: true,
                    entry_count,
                });
            }
            return Err(e.into());
        }
    };

    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        let manifest = load_cached(cache_dir)?
            .ok_or_else(|| anyhow::anyhow!("server said 304 but no cached manifest exists"))?;
        let entry_count = manifest.len();
        tracing::info!("ludusavi manifest unchanged: {entry_count} entries (cached)");
        return Ok(RefreshOutcome {
            manifest,
            not_modified: true,
            entry_count,
        });
    }

    let resp = resp.error_for_status()?;
    let new_etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let body = resp.text().await?;

    // Parse before committing to cache so we never persist garbage.
    let manifest = manifest::parse(&body)?;
    let entry_count = manifest.len();

    std::fs::write(manifest_path(cache_dir), &body)?;
    if let Some(etag) = new_etag {
        std::fs::write(etag_path(cache_dir), etag)?;
    }
    tracing::info!("ludusavi manifest refreshed: {entry_count} entries");

    Ok(RefreshOutcome {
        manifest,
        not_modified: false,
        entry_count,
    })
}
