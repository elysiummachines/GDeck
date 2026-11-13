//! Where the config lives, and making sure it exists.

use std::path::PathBuf;

use crate::{Error, Result};

/// Folder name under `%LOCALAPPDATA%`.
pub const STORE_DIR_NAME: &str = "GDeck";

/// Line based sections with `key = value`, so `.ini` is the honest extension.
/// Windows opens it in Notepad without a file association prompt.
pub const CONFIG_FILE: &str = "config.ini";
pub const AUDIT_FILE: &str = "audit.log";
pub const NOTES_DIR: &str = "notes";

/// `%LOCALAPPDATA%\GDeck`.
///
/// Per user by construction, so every account that runs this gets its own
/// store owned by its own SID. Nothing is shared, and locking the store down
/// cannot lock another user out of theirs.
pub fn store_dir() -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").ok_or(Error::NoLocalAppData)?;
    Ok(PathBuf::from(base).join(STORE_DIR_NAME))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(store_dir()?.join(CONFIG_FILE))
}

pub fn audit_path() -> Result<PathBuf> {
    Ok(store_dir()?.join(AUDIT_FILE))
}

/// Create the store directory if it is missing. Returns its path either way.
///
/// Creating it does not secure it; call [`crate::security::apply`] afterwards.
/// They are separate steps so that the permissions step is explicit and can be
/// re-run on its own.
pub fn ensure_store_dir() -> Result<PathBuf> {
    let dir = store_dir()?;
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| Error::Io {
            what: "create the store directory",
            detail: e.to_string(),
        })?;
    }
    Ok(dir)
}

/// Re-apply the policy to the store directory and every file in it.
///
/// This is the repair path. A file whose descriptor was loosened, or which was
/// created by an older build with a different mask, is put back under the
/// current policy without needing it to be deleted first.
pub fn secure_all(
    policy: &crate::security::Policy,
) -> Result<Vec<(PathBuf, &'static str)>> {
    let dir = ensure_store_dir()?;
    crate::security::apply(&dir, &policy.directory_sddl())?;
    let mut secured = vec![(dir.clone(), "directory")];

    for (name, kind) in [
        (CONFIG_FILE, "config"),
        (crate::integrity::SEAL_FILE, "seal"),
    ] {
        let path = dir.join(name);
        if path.exists() {
            crate::security::apply(&path, &policy.file_sddl())?;
            secured.push((path, kind));
        }
    }

    for (name, kind) in [
        (AUDIT_FILE, "audit log"),
        (crate::audit::ROTATED_AUDIT_FILE, "rotated audit log"),
    ] {
        let path = dir.join(name);
        if path.exists() {
            crate::security::apply(&path, &policy.audit_sddl())?;
            secured.push((path, kind));
        }
    }

    Ok(secured)
}
