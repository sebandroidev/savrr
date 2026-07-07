//! "Start the daemon when I sign in" — a per-user OS login hook.
//!
//! On Windows this is an `HKCU\...\Run` value pointing at the daemon's own exe
//! (no admin rights needed, runs headless at login — the daemon is built with
//! `windows_subsystem = "windows"` in release so no console window appears).
//! The daemon self-registers via `current_exe()`, which is the same installed
//! path whether it was launched as the app's sidecar or by this Run entry.
//!
//! Everywhere else these are no-ops: the GUI is Windows-only today, and login
//! autostart is inherently platform-specific.

#[cfg(windows)]
mod imp {
    use anyhow::{Context, Result};
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE_NAME: &str = "Savr Daemon";

    /// Add or remove the login Run entry for this daemon binary.
    pub fn set(enabled: bool) -> Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (run, _) = hkcu.create_subkey(RUN_KEY).context("open HKCU Run key")?;
        if enabled {
            let exe = std::env::current_exe().context("resolve daemon exe path")?;
            // Quote the path: "Program Files" and friends contain spaces, and the
            // Run value is parsed as a command line.
            run.set_value(VALUE_NAME, &format!("\"{}\"", exe.display()))
                .context("write Run value")?;
        } else {
            // Absent value -> nothing to remove; not an error.
            match run.delete_value(VALUE_NAME) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).context("delete Run value"),
            }
        }
        Ok(())
    }

    /// Whether the login Run entry currently exists.
    pub fn is_enabled() -> bool {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(RUN_KEY)
            .and_then(|run| run.get_value::<String, _>(VALUE_NAME))
            .is_ok()
    }
}

#[cfg(not(windows))]
mod imp {
    use anyhow::{bail, Result};

    pub fn set(_enabled: bool) -> Result<()> {
        bail!("login autostart is only supported on Windows")
    }

    pub fn is_enabled() -> bool {
        false
    }
}

pub use imp::{is_enabled, set};
