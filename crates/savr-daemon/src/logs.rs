//! Read the tail of the daemon's own log file for the GUI's Developer view.
//!
//! The daemon logs to a daily-rolled file under `<data_root>/logs` (see
//! `main.rs`: `tracing_appender::rolling::daily(&log_dir, "daemon.log")`), which
//! produces files named `daemon.log.YYYY-MM-DD`. We surface the newest one's
//! last `max_lines` lines so a user (or we) can diagnose a headless daemon
//! without a console.

use std::path::{Path, PathBuf};

use crate::config::data_root;

fn log_dir() -> PathBuf {
    data_root().join("logs")
}

/// The newest `daemon.log*` file in `dir`, if any. Dated suffixes sort lexically
/// in chronological order, so the max filename is the most recent day.
fn newest_log_file(dir: &Path) -> Option<PathBuf> {
    let mut newest: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let is_log = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("daemon.log"))
            .unwrap_or(false);
        if is_log && newest.as_ref().is_none_or(|cur| path > *cur) {
            newest = Some(path);
        }
    }
    newest
}

/// The last `max_lines` lines of the newest daemon log file under `dir`. Returns
/// a single explanatory line rather than erroring when no log exists yet or it
/// can't be read, so the Developer view always has something to show.
fn tail_from(dir: &Path, max_lines: usize) -> Vec<String> {
    let Some(path) = newest_log_file(dir) else {
        return vec![format!("(no daemon log file yet under {})", dir.display())];
    };
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => return vec![format!("(could not read {}: {e})", path.display())],
    };
    let lines: Vec<&str> = contents.lines().collect();
    let start = lines.len().saturating_sub(max_lines.max(1));
    lines[start..].iter().map(|s| s.to_string()).collect()
}

/// The last `max_lines` lines of the daemon's current log file.
pub fn tail(max_lines: usize) -> Vec<String> {
    tail_from(&log_dir(), max_lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tails_the_newest_log_file() {
        let dir = tempfile::tempdir().unwrap();
        // Older day and newer day; also a non-log file that must be ignored.
        std::fs::write(dir.path().join("daemon.log.2026-07-06"), "old1\nold2\n").unwrap();
        std::fs::write(dir.path().join("daemon.log.2026-07-08"), "a\nb\nc\nd\ne\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignore me\n").unwrap();

        // Picks the newest file and returns only its last N lines.
        let last3 = tail_from(dir.path(), 3);
        assert_eq!(last3, vec!["c", "d", "e"]);
    }

    #[test]
    fn explains_when_no_log_exists() {
        let dir = tempfile::tempdir().unwrap();
        let out = tail_from(dir.path(), 100);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("no daemon log file"));
    }
}
