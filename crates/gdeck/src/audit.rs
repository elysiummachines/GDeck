//! Append only record of what each key did.
//!
//! One entry per line, tab separated, UTC. UTC rather than local time because
//! a log that goes backwards for an hour every autumn is a log you cannot
//! reason about.
//!
//! The file gets a descriptor of its own granting the owner read and
//! `FILE_APPEND_DATA` but **not** `FILE_WRITE_DATA`, so existing entries cannot
//! be overwritten in place, by this program or by a careless tool.
//!
//! Being precise about how far that goes: it is not tamper proof. The owner of
//! an object can always rewrite its descriptor, and the store directory grants
//! delete rights over its children, so a determined owner can remove the file
//! outright. What the permissions buy is that quietly editing an entry takes a
//! deliberate act rather than an ordinary write, and that bugs in this program
//! cannot truncate the history.

use std::io::Write;
use std::path::Path;

use crate::keys::GKey;
use crate::security::{Policy, Sid};
use crate::{store, Error, Result};

#[repr(C)]
#[derive(Default)]
struct SystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

extern "system" {
    fn GetSystemTime(time: *mut SystemTime);
}

/// Rotate once the log passes this size, so it cannot grow without bound.
pub const MAX_AUDIT_BYTES: u64 = 1024 * 1024;

/// The previous log, kept beside the current one. Only one generation is kept.
pub const ROTATED_AUDIT_FILE: &str = "audit.1.log";

/// How a key press ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The action was carried out.
    Started,
    /// The action was refused before anything ran.
    Refused,
    /// The action was attempted and failed.
    Failed,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Started => "started",
            Outcome::Refused => "refused",
            Outcome::Failed => "failed",
        }
    }
}

/// `2026-11-11T18:45:01Z`
pub fn utc_now() -> String {
    let mut now = SystemTime::default();
    unsafe { GetSystemTime(&mut now) };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        now.year, now.month, now.day, now.hour, now.minute, now.second
    )
}

/// Collapse anything that would break the one entry per line rule.
///
/// A config value can legally contain almost anything, so a target is not
/// trusted to be free of tabs. Without this, a crafted label could inject a
/// second line into the log and forge an entry.
fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() || c == '\t' { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Build one line. Pure, so the format is testable without touching disk.
pub fn format_entry(
    timestamp: &str,
    key: Option<GKey>,
    action: &str,
    outcome: Outcome,
    detail: &str,
) -> String {
    format!(
        "{}\t{}\t{}\t{}\t{}",
        sanitize(timestamp),
        key.map(|k| k.to_string())
            .unwrap_or_else(|| "-".to_string()),
        sanitize(action),
        outcome.as_str(),
        sanitize(detail)
    )
}

/// Move the current log aside once it gets large.
///
/// Renaming needs `DELETE` on the source, which is why the audit descriptor
/// grants it. One generation is kept; the older one is discarded.
fn rotate_if_needed(path: &Path) -> Result<()> {
    let size = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(_) => return Ok(()),
    };
    if size < MAX_AUDIT_BYTES {
        return Ok(());
    }

    let rotated = path.with_file_name(ROTATED_AUDIT_FILE);
    let _ = std::fs::remove_file(&rotated);
    std::fs::rename(path, &rotated).map_err(|e| Error::Io {
        what: "rotate the audit log",
        detail: e.to_string(),
    })?;

    let owner = Sid::current_user()?;
    crate::security::apply(&rotated, &Policy::lockdown(owner).audit_sddl())?;
    Ok(())
}

/// Append one entry, creating and securing the file if it is not there yet.
pub fn record(key: Option<GKey>, action: &str, outcome: Outcome, detail: &str) -> Result<()> {
    let dir = store::ensure_store_dir()?;
    let path = dir.join(store::AUDIT_FILE);
    rotate_if_needed(&path)?;
    let is_new = !path.exists();

    let line = format_entry(&utc_now(), key, action, outcome, detail);

    {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| Error::Io {
                what: "open the audit log",
                detail: e.to_string(),
            })?;
        writeln!(file, "{}", line).map_err(|e| Error::Io {
            what: "write to the audit log",
            detail: e.to_string(),
        })?;
    }

    if is_new {
        // Secure it only on creation. Re-applying on every write would need
        // WRITE_DAC each time, which rather defeats the point of withholding
        // write access to the contents.
        let owner = Sid::current_user()?;
        let policy = Policy::lockdown(owner);
        crate::security::apply(Path::new(&path), &policy.audit_sddl())?;
    }

    Ok(())
}

/// The last `count` entries, oldest first.
pub fn tail(count: usize) -> Result<Vec<String>> {
    let path = store::audit_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| Error::Io {
        what: "read the audit log",
        detail: e.to_string(),
    })?;
    let lines: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.to_string())
        .collect();
    let start = lines.len().saturating_sub(count);
    Ok(lines[start..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_exactly_five_tab_separated_fields() {
        let line = format_entry(
            "2026-11-11T18:45:01Z",
            GKey::from_index(0),
            "launch",
            Outcome::Started,
            r"C:\Windows\System32\notepad.exe",
        );
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 5);
        assert_eq!(fields[0], "2026-11-11T18:45:01Z");
        assert_eq!(fields[1], "G1");
        assert_eq!(fields[2], "launch");
        assert_eq!(fields[3], "started");
        assert_eq!(fields[4], r"C:\Windows\System32\notepad.exe");
    }

    #[test]
    fn a_crafted_label_cannot_forge_a_second_entry() {
        // Without sanitising, this detail would end the line and start a new
        // one that looks like a genuine record.
        let nasty = "ok\n2026-01-01T00:00:00Z\tG9\tlaunch\tstarted\tsomething else";
        let line = format_entry(
            "2026-11-11T18:45:01Z",
            None,
            "launch",
            Outcome::Failed,
            nasty,
        );
        assert!(!line.contains('\n'), "a newline survived into the log");
        assert_eq!(line.split('\t').count(), 5, "tabs leaked and added fields");
    }

    #[test]
    fn a_tab_in_a_path_does_not_add_a_field() {
        let line = format_entry("t", None, "open", Outcome::Started, "C:\\a\tb\\c");
        assert_eq!(line.split('\t').count(), 5);
    }

    #[test]
    fn events_without_a_key_are_marked() {
        let line = format_entry("t", None, "config", Outcome::Refused, "bad line 7");
        assert_eq!(line.split('\t').nth(1), Some("-"));
    }

    #[test]
    fn the_timestamp_is_shaped_like_utc() {
        let now = utc_now();
        assert_eq!(now.len(), 20, "unexpected timestamp: {}", now);
        assert!(now.ends_with('Z'));
        assert_eq!(now.chars().nth(4), Some('-'));
        assert_eq!(now.chars().nth(10), Some('T'));
    }
}
