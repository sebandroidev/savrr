//! A tiny parser for Valve's KeyValues text format (`.vdf` / `.acf`) so we can
//! read Steam library layout without a dependency (PRD-02 §2). We only need
//! `libraryfolders.vdf` (library paths) and `appmanifest_*.acf` (appid +
//! installdir), both of which are shallow key/value trees — a hand parser is
//! plenty (PRD guidance).

use std::path::{Path, PathBuf};

/// One installed Steam game as read from an `appmanifest_*.acf`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamGame {
    pub appid: u32,
    pub installdir: String,
    /// Human-readable title from the `.acf` `name` field. Lets us list and
    /// detect a game without a Ludusavi manifest match; falls back to the
    /// install dir name if the manifest omits it.
    pub name: String,
}

/// A Steam library folder and the games installed under it.
#[derive(Debug, Clone)]
pub struct SteamLibrary {
    /// The library root (the folder that *contains* `steamapps/`).
    pub path: PathBuf,
    pub games: Vec<SteamGame>,
}

impl SteamLibrary {
    /// Absolute install dir of a game: `<lib>/steamapps/common/<installdir>`.
    pub fn install_path(&self, game: &SteamGame) -> PathBuf {
        self.path
            .join("steamapps")
            .join("common")
            .join(&game.installdir)
    }

    /// Proton compat prefix drive_c for `appid` (Linux):
    /// `<lib>/steamapps/compatdata/<appid>/pfx/drive_c` (PRD-02 §2, §4).
    pub fn proton_drive_c(&self, appid: u32) -> PathBuf {
        self.path
            .join("steamapps")
            .join("compatdata")
            .join(appid.to_string())
            .join("pfx")
            .join("drive_c")
    }
}

/// A parsed KeyValues node: either a leaf string or a nested object. Order is
/// preserved (Valve files can repeat keys, though we don't rely on that).
#[derive(Debug, Clone, PartialEq)]
pub enum Kv {
    Str(String),
    Obj(Vec<(String, Kv)>),
}

impl Kv {
    /// Look up a direct child by key (first match).
    pub fn get(&self, key: &str) -> Option<&Kv> {
        match self {
            Kv::Obj(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            Kv::Str(_) => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Kv::Str(s) => Some(s),
            Kv::Obj(_) => None,
        }
    }

    fn entries(&self) -> &[(String, Kv)] {
        match self {
            Kv::Obj(e) => e,
            Kv::Str(_) => &[],
        }
    }
}

/// Parse a KeyValues document into a root object. Returns `None` on malformed
/// input (unterminated string/brace) rather than panicking — a corrupt Steam
/// file must not crash the daemon.
pub fn parse(text: &str) -> Option<Kv> {
    let tokens = tokenize(text)?;
    let mut pos = 0;
    // A document is a sequence of top-level key/value pairs; wrap them in a
    // synthetic root object.
    let mut root = Vec::new();
    while pos < tokens.len() {
        let (key, value) = parse_pair(&tokens, &mut pos)?;
        root.push((key, value));
    }
    Some(Kv::Obj(root))
}

#[derive(Debug, PartialEq)]
enum Token {
    Str(String),
    Open,
    Close,
}

fn tokenize(text: &str) -> Option<Vec<Token>> {
    let mut tokens = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                tokens.push(Token::Open);
                i += 1;
            }
            b'}' => {
                tokens.push(Token::Close);
                i += 1;
            }
            b'"' => {
                i += 1;
                let mut s = String::new();
                loop {
                    if i >= bytes.len() {
                        return None; // unterminated string
                    }
                    match bytes[i] {
                        b'"' => {
                            i += 1;
                            break;
                        }
                        b'\\' => {
                            i += 1;
                            if i >= bytes.len() {
                                return None;
                            }
                            // VDF escapes: \\ \" \n \t; unknown escapes pass
                            // through the escaped char verbatim.
                            let c = match bytes[i] {
                                b'n' => '\n',
                                b't' => '\t',
                                other => other as char,
                            };
                            s.push(c);
                            i += 1;
                        }
                        _ => {
                            // Accumulate a UTF-8 char starting at i.
                            let ch_len = utf8_len(bytes[i]);
                            let end = (i + ch_len).min(bytes.len());
                            s.push_str(std::str::from_utf8(&bytes[i..end]).ok()?);
                            i = end;
                        }
                    }
                }
                tokens.push(Token::Str(s));
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                // Line comment.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            _ => i += 1, // whitespace / unquoted junk between tokens
        }
    }
    Some(tokens)
}

fn utf8_len(first: u8) -> usize {
    match first {
        b if b < 0x80 => 1,
        b if b >> 5 == 0b110 => 2,
        b if b >> 4 == 0b1110 => 3,
        _ => 4,
    }
}

fn parse_pair(tokens: &[Token], pos: &mut usize) -> Option<(String, Kv)> {
    let key = match tokens.get(*pos)? {
        Token::Str(s) => s.clone(),
        _ => return None,
    };
    *pos += 1;
    match tokens.get(*pos)? {
        Token::Str(s) => {
            *pos += 1;
            Some((key, Kv::Str(s.clone())))
        }
        Token::Open => {
            *pos += 1;
            let obj = parse_obj(tokens, pos)?;
            Some((key, obj))
        }
        Token::Close => None,
    }
}

fn parse_obj(tokens: &[Token], pos: &mut usize) -> Option<Kv> {
    let mut entries = Vec::new();
    loop {
        match tokens.get(*pos)? {
            Token::Close => {
                *pos += 1;
                return Some(Kv::Obj(entries));
            }
            _ => {
                let (k, v) = parse_pair(tokens, pos)?;
                entries.push((k, v));
            }
        }
    }
}

/// Extract library folder paths from `libraryfolders.vdf`. Handles both the
/// modern object form (`"0" { "path" "..." }`) and the legacy string form
/// (`"1" "D:\\SteamLibrary"`).
pub fn parse_library_paths(vdf: &str) -> Vec<PathBuf> {
    let Some(root) = parse(vdf) else {
        return Vec::new();
    };
    // The real root has a single "libraryfolders" child (Valve keys are
    // case-insensitive: modern files use "libraryfolders", legacy ones
    // "LibraryFolders"). Fall back to the root if it's already the container.
    let container = root
        .entries()
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("libraryfolders"))
        .map(|(_, v)| v)
        .unwrap_or(&root);
    let mut out = Vec::new();
    for (key, value) in container.entries() {
        // Library entries are numeric keys.
        if key.parse::<u32>().is_err() {
            continue;
        }
        match value {
            Kv::Str(p) => out.push(PathBuf::from(p)),
            Kv::Obj(_) => {
                if let Some(p) = value.get("path").and_then(Kv::as_str) {
                    out.push(PathBuf::from(p));
                }
            }
        }
    }
    out
}

/// Extract `(appid, installdir)` from an `appmanifest_*.acf`.
pub fn parse_app_manifest(acf: &str) -> Option<SteamGame> {
    let root = parse(acf)?;
    let state = root.get("AppState")?;
    let appid = state.get("appid").and_then(Kv::as_str)?.parse().ok()?;
    let installdir = state.get("installdir").and_then(Kv::as_str)?.to_string();
    // Prefer the .acf's own display name; fall back to the install dir.
    let name = state
        .get("name")
        .and_then(Kv::as_str)
        .map(|s| s.to_string())
        .unwrap_or_else(|| installdir.clone());
    Some(SteamGame {
        appid,
        installdir,
        name,
    })
}

/// Discover Steam libraries under a Steam root by reading its
/// `steamapps/libraryfolders.vdf` and each library's `appmanifest_*.acf`.
/// Missing/unreadable files are skipped (best-effort scan).
pub fn discover_libraries(steam_root: &Path) -> Vec<SteamLibrary> {
    let mut libs = Vec::new();
    let vdf_path = steam_root.join("steamapps").join("libraryfolders.vdf");
    let mut lib_paths = std::fs::read_to_string(&vdf_path)
        .ok()
        .map(|t| parse_library_paths(&t))
        .unwrap_or_default();
    // The Steam install root is itself a library even if the vdf omits it.
    if !lib_paths.iter().any(|p| p == steam_root) {
        lib_paths.push(steam_root.to_path_buf());
    }

    for lib in lib_paths {
        let steamapps = lib.join("steamapps");
        let Ok(entries) = std::fs::read_dir(&steamapps) else {
            continue;
        };
        let mut games = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("appmanifest_") && name.ends_with(".acf") {
                if let Ok(text) = std::fs::read_to_string(entry.path()) {
                    if let Some(game) = parse_app_manifest(&text) {
                        games.push(game);
                    }
                }
            }
        }
        libs.push(SteamLibrary { path: lib, games });
    }
    libs
}

/// Well-known Steam install roots for the current OS (PRD-02 §4). Only existing
/// paths are returned.
pub fn default_steam_roots() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let candidates: Vec<PathBuf> = if cfg!(target_os = "macos") {
        vec![home.join("Library/Application Support/Steam")]
    } else if cfg!(target_os = "windows") {
        vec![
            PathBuf::from(r"C:\Program Files (x86)\Steam"),
            PathBuf::from(r"C:\Program Files\Steam"),
        ]
    } else {
        vec![
            home.join(".steam/steam"),
            home.join(".local/share/Steam"),
            home.join(".var/app/com.valvesoftware.Steam/data/Steam"),
        ]
    };
    candidates.into_iter().filter(|p| p.exists()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARYFOLDERS: &str = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"/home/deck/.local/share/Steam"
		"label"		""
		"apps"
		{
			"220"		"5495838"
		}
	}
	"1"
	{
		"path"		"/run/media/deck/SD/SteamLibrary"
		"apps"
		{
			"1145360"		"12345"
		}
	}
}
"#;

    const APPMANIFEST: &str = r#"
"AppState"
{
	"appid"		"220"
	"Universe"		"1"
	"name"		"Half-Life 2"
	"StateFlags"		"4"
	"installdir"		"Half-Life 2"
	"LastUpdated"		"1700000000"
}
"#;

    #[test]
    fn parses_library_paths_object_form() {
        let paths = parse_library_paths(LIBRARYFOLDERS);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/home/deck/.local/share/Steam"),
                PathBuf::from("/run/media/deck/SD/SteamLibrary"),
            ]
        );
    }

    #[test]
    fn parses_legacy_string_form() {
        let legacy = r#"
"LibraryFolders"
{
	"TimeNextStatsReport"		"123"
	"ContentStatsID"		"456"
	"1"		"D:\\SteamLibrary"
	"2"		"E:\\Games\\Steam"
}
"#;
        let paths = parse_library_paths(legacy);
        assert_eq!(
            paths,
            vec![
                PathBuf::from(r"D:\SteamLibrary"),
                PathBuf::from(r"E:\Games\Steam"),
            ],
            "numeric keys are libraries; stats keys are ignored"
        );
    }

    #[test]
    fn parses_app_manifest_fields() {
        let game = parse_app_manifest(APPMANIFEST).unwrap();
        assert_eq!(
            game,
            SteamGame {
                appid: 220,
                installdir: "Half-Life 2".into(),
                name: "Half-Life 2".into(),
            }
        );
    }

    #[test]
    fn install_and_proton_paths() {
        let lib = SteamLibrary {
            path: PathBuf::from("/lib"),
            games: vec![],
        };
        let game = SteamGame {
            appid: 220,
            installdir: "Half-Life 2".into(),
            name: "Half-Life 2".into(),
        };
        assert_eq!(
            lib.install_path(&game),
            PathBuf::from("/lib/steamapps/common/Half-Life 2")
        );
        assert_eq!(
            lib.proton_drive_c(220),
            PathBuf::from("/lib/steamapps/compatdata/220/pfx/drive_c")
        );
    }

    #[test]
    fn malformed_input_does_not_panic() {
        assert!(parse("\"unterminated").is_none());
        assert!(parse_app_manifest("garbage {{{").is_none());
        assert!(parse_library_paths("nonsense").is_empty());
    }

    #[test]
    fn handles_escapes() {
        let kv = parse(r#""k" "a\\b\"c""#).unwrap();
        assert_eq!(kv.get("k").and_then(Kv::as_str), Some(r#"a\b"c"#));
    }
}
