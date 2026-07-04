//! The `.savr` on-disk archive format (PRD-05 §5): a zstd-compressed tar
//! holding `meta.json`, `files/<rel_path>`, and optionally `registry.json`.
//!
//! Deviation from the PRD sketch: `deletions.json` is folded into
//! `meta.json` (`ArchiveMeta.deletions`) — one source of truth instead of two
//! files that could disagree. `meta.json` is also *not* the full `Version`
//! struct: `Version.blob_hash` is the hash of this archive, so it cannot be
//! known while writing it. `ArchiveMeta` carries everything else.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::types::{DeviceId, FileEntry, GameId, VersionId, VersionKind};

/// Zstd level 3: fast with a good ratio; saves are small (PRD-01 tech stack).
const ZSTD_LEVEL: i32 = 3;
const META_PATH: &str = "meta.json";
const REGISTRY_PATH: &str = "registry.json";
const FILES_PREFIX: &str = "files/";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ArchiveMeta {
    pub game_id: GameId,
    pub device_id: DeviceId,
    pub parent: Option<VersionId>,
    pub kind: VersionKind,
    /// Full logical file set of the snapshot (also what `Version.files` holds).
    pub files: Vec<FileEntry>,
    /// rel_paths removed since the last full (differential only).
    pub deletions: Vec<String>,
    pub created_at: DateTime<Utc>,
}

/// Result of unpacking an archive.
pub struct Unpacked {
    pub meta: ArchiveMeta,
    /// Raw `registry.json` bytes, if the archive carried one (Windows).
    pub registry: Option<Vec<u8>>,
}

/// Write a `.savr` stream: `meta` + the payload files (rel_path → source path
/// on disk) + optional registry export. Streaming by construction — nothing is
/// buffered whole.
pub fn pack<W: Write>(
    out: W,
    meta: &ArchiveMeta,
    payload: &[(String, PathBuf)],
    registry_json: Option<&[u8]>,
) -> Result<()> {
    let enc = zstd::Encoder::new(out, ZSTD_LEVEL)
        .map_err(Error::Io)?
        .auto_finish();
    let mut tar = tar::Builder::new(enc);

    let meta_bytes = serde_json::to_vec_pretty(meta).map_err(|e| Error::Manifest(e.to_string()))?;
    append_bytes(&mut tar, META_PATH, &meta_bytes)?;

    if let Some(reg) = registry_json {
        append_bytes(&mut tar, REGISTRY_PATH, reg)?;
    }

    for (rel, src) in payload {
        let mut f = std::fs::File::open(src)?;
        let len = f.metadata()?.len();
        let mut header = tar::Header::new_gnu();
        header.set_size(len);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, format!("{FILES_PREFIX}{rel}"), &mut f)?;
    }

    tar.into_inner()?; // flushes tar + zstd
    Ok(())
}

fn append_bytes<W: Write>(tar: &mut tar::Builder<W>, path: &str, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, path, bytes)?;
    Ok(())
}

/// Read a `.savr` stream, extracting `files/<rel_path>` entries under `dest`.
/// Entry paths are sanitized: absolute paths and `..` components are rejected
/// (a tampered archive must not write outside `dest`).
pub fn unpack<R: Read>(input: R, dest: &Path) -> Result<Unpacked> {
    let dec = zstd::Decoder::new(input)?;
    let mut tar = tar::Archive::new(dec);

    let mut meta: Option<ArchiveMeta> = None;
    let mut registry: Option<Vec<u8>> = None;

    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let name = path.to_string_lossy().into_owned();

        if name == META_PATH {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            meta = Some(serde_json::from_slice(&buf).map_err(|e| Error::Manifest(e.to_string()))?);
        } else if name == REGISTRY_PATH {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            registry = Some(buf);
        } else if let Some(rel) = name.strip_prefix(FILES_PREFIX) {
            let rel_path = Path::new(rel);
            if !is_safe_rel_path(rel_path) {
                return Err(Error::Manifest(format!("unsafe path in archive: {name}")));
            }
            let target = dest.join(rel_path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&target)?;
            std::io::copy(&mut entry, &mut out)?;
        }
        // Unknown entries are skipped — forward compatibility.
    }

    let meta = meta.ok_or_else(|| Error::Manifest("archive missing meta.json".into()))?;
    Ok(Unpacked { meta, registry })
}

fn is_safe_rel_path(p: &Path) -> bool {
    !p.components().any(|c| !matches!(c, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::Blake3Hash;
    use uuid::Uuid;

    fn meta_with(files: Vec<FileEntry>, deletions: Vec<String>) -> ArchiveMeta {
        ArchiveMeta {
            game_id: Uuid::nil(),
            device_id: Uuid::nil(),
            parent: None,
            kind: VersionKind::Full,
            files,
            deletions,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn pack_unpack_roundtrip() {
        let work = std::env::temp_dir().join(format!("savr-arc-{}", Uuid::now_v7()));
        let src = work.join("src");
        let dst = work.join("dst");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.sav"), b"level 3").unwrap();
        std::fs::write(src.join("sub/b.sav"), b"gold 999").unwrap();

        let files = vec![FileEntry {
            rel_path: "a.sav".into(),
            size: 7,
            mtime: 0,
            hash: Blake3Hash::of(b"level 3"),
        }];
        let meta = meta_with(files, vec!["old.sav".into()]);
        let payload = vec![
            ("a.sav".to_string(), src.join("a.sav")),
            ("sub/b.sav".to_string(), src.join("sub/b.sav")),
        ];

        let mut buf = Vec::new();
        pack(&mut buf, &meta, &payload, Some(b"{\"hkcu\":{}}")).unwrap();

        let unpacked = unpack(buf.as_slice(), &dst).unwrap();
        assert_eq!(unpacked.meta.deletions, vec!["old.sav".to_string()]);
        assert_eq!(
            unpacked.registry.as_deref(),
            Some(b"{\"hkcu\":{}}".as_ref())
        );
        assert_eq!(std::fs::read(dst.join("a.sav")).unwrap(), b"level 3");
        assert_eq!(std::fs::read(dst.join("sub/b.sav")).unwrap(), b"gold 999");

        std::fs::remove_dir_all(&work).ok();
    }

    #[test]
    fn rejects_path_traversal() {
        // The tar *writer* refuses `..` paths, so a hostile archive can't be
        // built with `append_data`. Inject the malicious name straight into the
        // header bytes to simulate an archive from an untrusted source, then
        // prove our *reader* (`unpack`) still refuses to escape `dest`.
        let mut raw = Vec::new();
        {
            let enc = zstd::Encoder::new(&mut raw, 3).unwrap().auto_finish();
            let mut tar = tar::Builder::new(enc);
            let meta = serde_json::to_vec(&meta_with(vec![], vec![])).unwrap();
            append_bytes(&mut tar, META_PATH, &meta).unwrap();

            let payload = b"pwned";
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o644);
            {
                let gnu = header.as_gnu_mut().unwrap();
                let name = b"files/../evil.sav";
                gnu.name[..name.len()].copy_from_slice(name);
            }
            header.set_cksum();
            tar.append(&header, &payload[..]).unwrap();
            tar.into_inner().unwrap();
        }
        let dst = std::env::temp_dir().join(format!("savr-evil-{}", Uuid::now_v7()));
        let err = unpack(raw.as_slice(), &dst);
        assert!(err.is_err(), "traversal must be rejected");
        assert!(!dst.parent().unwrap().join("evil.sav").exists());
        std::fs::remove_dir_all(&dst).ok();
    }
}
