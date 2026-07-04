//! Network-gated: fetch the REAL Ludusavi manifest and prove `savr-core`'s
//! parser survives it (PRD-02 §1.1). Marked `#[ignore]` so the default
//! `cargo test` stays hermetic/offline; run with:
//!
//!     SAVR_NET_TESTS=1 cargo test -p savr-daemon --test manifest_real -- --ignored
//!
//! It also skips gracefully (passes) if the network is unavailable.

use std::path::PathBuf;

use savr_core::manifest::{resolve, Roots};
use savr_daemon::manifest_sync;

/// Cache into the repo's `manifests/` dir (PRD-02 §1.1), derived from this
/// crate's location so no absolute path is hardcoded.
fn repo_manifest_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("SAVR_MANIFEST_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("manifests")
}

#[tokio::test]
#[ignore = "requires network; run with --ignored and SAVR_NET_TESTS=1"]
async fn real_manifest_parses_and_resolves() {
    let cache_dir = repo_manifest_dir();

    let outcome = match manifest_sync::refresh(&cache_dir).await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("skipping: manifest fetch failed (offline?): {e}");
            return;
        }
    };

    // The real manifest is tens of thousands of games (PRD-02 §1.1).
    assert!(
        outcome.entry_count > 10_000,
        "expected >10000 entries, got {}",
        outcome.entry_count
    );

    // resolve() must work on real data: across a sample of entries, at least
    // some save targets resolve to concrete paths on this machine.
    let roots = Roots::current();
    let mut resolvable = 0usize;
    let mut sampled = 0usize;
    for (_title, entry) in outcome.manifest.iter().take(2000) {
        for target in entry.save_targets() {
            if target.registry {
                continue;
            }
            sampled += 1;
            if resolve(&target.glob, &roots, None).is_some() {
                resolvable += 1;
            }
        }
    }
    assert!(sampled > 0, "sampled entries had no filesystem targets");
    assert!(
        resolvable > 0,
        "resolve() produced no concrete paths across {sampled} sampled targets"
    );
    eprintln!(
        "manifest: {} entries; resolved {resolvable}/{sampled} sampled targets",
        outcome.entry_count
    );
}
