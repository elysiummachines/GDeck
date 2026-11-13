//! Tamper detection for the config, using DPAPI.
//!
//! The DACL on the store stops other standard accounts, but as established it
//! does not stop an administrator: anyone with those rights can take ownership
//! and rewrite any file. This is the layer that notices when they have.
//!
//! On every save we seal a copy of the exact config text with
//! `CryptProtectData`, scoped to the current **user**, not the machine. The
//! sealed blob can only be opened by code running as that user. Someone who
//! edits `config.ini` directly cannot produce a matching seal without first
//! being you.
//!
//! Sealing the whole text rather than a digest of it is deliberate and buys
//! two things: no hash function has to be written and trusted, and because the
//! last sealed copy is recoverable we can show **exactly which lines changed**
//! rather than only reporting that something did.
//!
//! What this does not do:
//!
//! * It does not stop the edit. It detects it.
//! * It does not defend against malware already running as you. Such code can
//!   call DPAPI as you and reseal whatever it likes. Nothing a program can do
//!   on its own survives an attacker with your identity.
//! * The entropy constant below is scoping, not a secret. It is in the binary.
//!   It stops an unrelated DPAPI blob being mistaken for ours, nothing more.

use core::ffi::c_void;
use core::ptr;

use crate::security::Policy;
use crate::{store, Error, Result};

/// Sits beside `config.ini`.
pub const SEAL_FILE: &str = "config.seal";

/// Above this many lines, a line by line diff is not worth the quadratic cost.
/// The config is capped far below this in practice.
const MAX_DIFF_LINES: usize = 2000;

#[repr(C)]
struct DataBlob {
    cb_data: u32,
    pb_data: *mut u8,
}

/// Never show a UI prompt. This runs in a tray app.
const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

/// Scoping value, not a secret. See the module note.
const ENTROPY: &[u8] = b"gdeck.config.seal.v1";

#[link(name = "crypt32")]
extern "system" {
    fn CryptProtectData(
        data_in: *const DataBlob,
        description: *const u16,
        entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;
    fn CryptUnprotectData(
        data_in: *const DataBlob,
        description: *mut *mut u16,
        entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;
}

extern "system" {
    fn LocalFree(mem: *mut c_void) -> *mut c_void;
    fn GetLastError() -> u32;
}

fn blob(bytes: &[u8]) -> DataBlob {
    DataBlob {
        cb_data: bytes.len() as u32,
        pb_data: bytes.as_ptr() as *mut u8,
    }
}

/// Encrypt bytes so that only this user can read them back.
pub fn seal(plaintext: &[u8]) -> Result<Vec<u8>> {
    unsafe {
        let input = blob(plaintext);
        let entropy = blob(ENTROPY);
        let mut output = DataBlob {
            cb_data: 0,
            pb_data: ptr::null_mut(),
        };

        if CryptProtectData(
            &input,
            ptr::null(),
            &entropy,
            ptr::null_mut(),
            ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        ) == 0
        {
            return Err(Error::Win32 {
                what: "CryptProtectData",
                code: GetLastError(),
            });
        }

        let sealed =
            core::slice::from_raw_parts(output.pb_data, output.cb_data as usize).to_vec();
        LocalFree(output.pb_data as *mut c_void);
        Ok(sealed)
    }
}

/// Decrypt bytes sealed by [`seal`] as this user.
pub fn unseal(sealed: &[u8]) -> Result<Vec<u8>> {
    unsafe {
        let input = blob(sealed);
        let entropy = blob(ENTROPY);
        let mut output = DataBlob {
            cb_data: 0,
            pb_data: ptr::null_mut(),
        };

        if CryptUnprotectData(
            &input,
            ptr::null_mut(),
            &entropy,
            ptr::null_mut(),
            ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        ) == 0
        {
            return Err(Error::Win32 {
                what: "CryptUnprotectData",
                code: GetLastError(),
            });
        }

        let plain = core::slice::from_raw_parts(output.pb_data, output.cb_data as usize).to_vec();
        LocalFree(output.pb_data as *mut c_void);
        Ok(plain)
    }
}

/// One line of a comparison.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DiffLine {
    Same(String),
    /// Present in the sealed copy, gone from what is on disk now.
    Removed(String),
    /// On disk now, not in the sealed copy.
    Added(String),
}

/// What the seal says about the config on disk.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Integrity {
    /// No seal yet. First run, or it was deleted.
    NotSealed,
    /// The config is byte for byte what we last wrote.
    Intact,
    /// The config changed outside the app.
    Modified { diff: Vec<DiffLine> },
    /// A seal exists but this account cannot open it. Either it belongs to a
    /// different user, or it is corrupt.
    SealUnreadable { code: u32 },
}

/// Line by line comparison, longest common subsequence.
///
/// Falls back to reporting wholesale replacement on very large inputs, since
/// the table is O(n * m).
pub fn diff_lines(sealed: &str, current: &str) -> Vec<DiffLine> {
    let old: Vec<&str> = sealed.lines().collect();
    let new: Vec<&str> = current.lines().collect();

    if old.len() > MAX_DIFF_LINES || new.len() > MAX_DIFF_LINES {
        let mut out: Vec<DiffLine> = old.iter().map(|l| DiffLine::Removed(l.to_string())).collect();
        out.extend(new.iter().map(|l| DiffLine::Added(l.to_string())));
        return out;
    }

    let (n, m) = (old.len(), new.len());
    let mut table = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if old[i] == new[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }

    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old[i] == new[j] {
            out.push(DiffLine::Same(old[i].to_string()));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            out.push(DiffLine::Removed(old[i].to_string()));
            i += 1;
        } else {
            out.push(DiffLine::Added(new[j].to_string()));
            j += 1;
        }
    }
    while i < n {
        out.push(DiffLine::Removed(old[i].to_string()));
        i += 1;
    }
    while j < m {
        out.push(DiffLine::Added(new[j].to_string()));
        j += 1;
    }
    out
}

/// Only the lines that actually differ.
pub fn changed_only(diff: &[DiffLine]) -> Vec<&DiffLine> {
    diff.iter()
        .filter(|line| !matches!(line, DiffLine::Same(_)))
        .collect()
}

fn seal_path() -> Result<std::path::PathBuf> {
    Ok(store::store_dir()?.join(SEAL_FILE))
}

/// Seal the given config text and write it beside the config.
pub fn write_seal(config_text: &str, policy: &Policy) -> Result<()> {
    let path = seal_path()?;
    let sealed = seal(config_text.as_bytes())?;
    std::fs::write(&path, &sealed).map_err(|e| Error::Io {
        what: "write the seal",
        detail: e.to_string(),
    })?;
    crate::security::apply(&path, &policy.file_sddl())?;
    Ok(())
}

/// Compare the config on disk against the seal.
pub fn check(config_text: &str) -> Result<Integrity> {
    let path = seal_path()?;
    if !path.exists() {
        return Ok(Integrity::NotSealed);
    }

    let sealed = std::fs::read(&path).map_err(|e| Error::Io {
        what: "read the seal",
        detail: e.to_string(),
    })?;

    let plain = match unseal(&sealed) {
        Ok(plain) => plain,
        Err(Error::Win32 { code, .. }) => return Ok(Integrity::SealUnreadable { code }),
        Err(other) => return Err(other),
    };

    let previous = String::from_utf8_lossy(&plain).to_string();
    if previous == config_text {
        Ok(Integrity::Intact)
    } else {
        Ok(Integrity::Modified {
            diff: diff_lines(&previous, config_text),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_bytes_round_trip() {
        let secret = b"version = 1\nshell_enabled = false\n";
        let sealed = seal(secret).expect("seal");
        assert_ne!(sealed.as_slice(), secret, "the blob must not be plaintext");
        assert_eq!(unseal(&sealed).expect("unseal"), secret);
    }

    #[test]
    fn a_tampered_blob_does_not_open() {
        let mut sealed = seal(b"hello").expect("seal");
        let last = sealed.len() - 1;
        sealed[last] ^= 0xFF;
        assert!(unseal(&sealed).is_err());
    }

    #[test]
    fn identical_text_has_no_changed_lines() {
        let text = "a\nb\nc\n";
        assert!(changed_only(&diff_lines(text, text)).is_empty());
    }

    #[test]
    fn a_changed_line_shows_as_one_removal_and_one_addition() {
        let diff = diff_lines("a\nb\nc\n", "a\nB\nc\n");
        let changed = changed_only(&diff);
        assert_eq!(changed.len(), 2);
        assert!(changed.contains(&&DiffLine::Removed("b".to_string())));
        assert!(changed.contains(&&DiffLine::Added("B".to_string())));
    }

    #[test]
    fn an_inserted_line_shows_as_one_addition() {
        let diff = diff_lines("a\nc\n", "a\nb\nc\n");
        let changed = changed_only(&diff);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0], &DiffLine::Added("b".to_string()));
    }

    #[test]
    fn a_realistic_target_swap_is_pinpointed() {
        // The attack this exists to catch: the path a key launches is swapped.
        let before = "version = 1\n\n[G1]\naction = launch\nlabel = Steam\ntarget = C:\\Steam\\steam.exe\n";
        let after = "version = 1\n\n[G1]\naction = launch\nlabel = Steam\ntarget = C:\\Temp\\evil.exe\n";
        let diff = diff_lines(before, after);
        let changed = changed_only(&diff);
        assert_eq!(changed.len(), 2);
        assert_eq!(
            changed[0],
            &DiffLine::Removed("target = C:\\Steam\\steam.exe".to_string())
        );
        assert_eq!(
            changed[1],
            &DiffLine::Added("target = C:\\Temp\\evil.exe".to_string())
        );
    }
}
