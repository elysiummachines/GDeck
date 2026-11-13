//! The config file: what each G key does.
//!
//! Hand written parser, no dependencies. The format is line based sections of
//! `key = value`, chosen over JSON or TOML for one specific reason: **nothing
//! needs escaping**. A value runs to the end of its line, so a Windows path can
//! contain spaces, backslashes, quotes or `#` and round trips verbatim. That
//! removes the class of bug a hand written parser is most likely to get wrong,
//! and it removes the `C:\\Program Files\\...` double backslash problem that
//! JSON forces on Windows paths.
//!
//! Consequences of that choice, all deliberate:
//!
//! * There are no inline comments. A `#` or `;` starts a comment only at the
//!   beginning of a line, so those characters are legal inside a path.
//! * Values are trimmed of surrounding whitespace, so a value cannot begin or
//!   end with a space. No Windows path needs to.
//! * A value cannot contain a newline. Nothing here needs one.
//!
//! The parser is strict on purpose. This file is a list of things to execute,
//! so anything unexpected is an error that names its line, never something
//! quietly ignored. Unknown keys, unknown sections, duplicates, unknown action
//! types and relative paths are all rejected.

use std::collections::BTreeMap;
use std::path::Path;

use crate::keys::{GKey, G_KEY_COUNT};
use crate::security::Policy;
use crate::{store, Error, Result};

/// Refuse to even read something this large.
pub const MAX_FILE_BYTES: usize = 256 * 1024;
pub const MAX_LINE_BYTES: usize = 8 * 1024;
pub const MAX_LABEL_CHARS: usize = 64;
pub const MAX_TARGET_CHARS: usize = 2048;
pub const MAX_ARGS_CHARS: usize = 4096;

/// The only schema version this build understands.
pub const SCHEMA_VERSION: u32 = 1;

/// URI schemes a key is allowed to launch or open.
///
/// An allowlist rather than a blocklist: unknown schemes are refused, because
/// Windows resolves a scheme to whatever handler is registered for it, and that
/// is not something a config file should get to pick freely.
pub const ALLOWED_URI_SCHEMES: [&str; 6] = [
    "http",
    "https",
    "file",
    "steam",
    "ms-settings",
    "com.epicgames.launcher",
];

/// What a target string turned out to be.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TargetKind {
    /// `C:\path\to\thing`
    AbsolutePath,
    /// `\\server\share\thing`
    Unc,
    /// An allowed URI scheme.
    Uri(String),
}

/// What a key does when pressed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    /// Unassigned.
    None,
    /// Start a program, or hand a URI to its registered handler.
    Launch {
        target: String,
        args: String,
        workdir: String,
        focus_if_running: bool,
    },
    /// Open a file, folder or URL with its default handler.
    Open { target: String },
    /// Show the quick note popup for a named note.
    Note { name: String },
    /// Run a shell command. Refused unless `shell_enabled` is set.
    Shell { command: String, hidden: bool },
}

impl Action {
    pub fn type_name(&self) -> &'static str {
        match self {
            Action::None => "none",
            Action::Launch { .. } => "launch",
            Action::Open { .. } => "open",
            Action::Note { .. } => "note",
            Action::Shell { .. } => "shell",
        }
    }
}

/// One assigned key.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Slot {
    pub label: String,
    pub action: Action,
}

/// The whole configuration.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Config {
    pub version: u32,
    /// Shell actions are refused entirely unless this is on.
    pub shell_enabled: bool,
    pub slots: [Option<Slot>; G_KEY_COUNT],
}

impl Default for Config {
    fn default() -> Config {
        Config {
            version: SCHEMA_VERSION,
            shell_enabled: false,
            slots: std::array::from_fn(|_| None),
        }
    }
}

impl Config {
    pub fn slot(&self, key: GKey) -> Option<&Slot> {
        self.slots[key.index()].as_ref()
    }

    pub fn set_slot(&mut self, key: GKey, slot: Option<Slot>) {
        self.slots[key.index()] = slot;
    }

    pub fn assigned_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }
}

fn err(line: usize, detail: impl Into<String>) -> Error {
    Error::Config {
        line,
        detail: detail.into(),
    }
}

/// Classify a target, rejecting anything that is not an absolute path or an
/// allowed URI.
///
/// Relative paths and bare command names are refused on purpose. `notepad`
/// would be resolved against `PATH` at launch time, which means what a key does
/// could change because something else was installed.
pub fn classify_target(target: &str) -> std::result::Result<TargetKind, String> {
    if target.is_empty() {
        return Err("target is empty".to_string());
    }
    if target.chars().count() > MAX_TARGET_CHARS {
        return Err(format!("target is longer than {} characters", MAX_TARGET_CHARS));
    }
    if target.chars().any(|c| c.is_control()) {
        return Err("target contains a control character".to_string());
    }

    if target.starts_with("\\\\") {
        return Ok(TargetKind::Unc);
    }

    let bytes = target.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return Ok(TargetKind::AbsolutePath);
    }

    if let Some(colon) = target.find(':') {
        let scheme = target[..colon].to_ascii_lowercase();
        let valid = !scheme.is_empty()
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '.' || c == '-');
        if valid {
            if ALLOWED_URI_SCHEMES.contains(&scheme.as_str()) {
                return Ok(TargetKind::Uri(scheme));
            }
            return Err(format!(
                "URI scheme '{}' is not in the allowlist ({})",
                scheme,
                ALLOWED_URI_SCHEMES.join(", ")
            ));
        }
    }

    Err("target must be an absolute path such as C:\\... or an allowed URI".to_string())
}

/// A note name must be a plain file name, never a path.
fn validate_note_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("note name is empty".to_string());
    }
    if name.chars().count() > MAX_LABEL_CHARS {
        return Err("note name is too long".to_string());
    }
    if name.chars().any(|c| {
        c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    }) {
        return Err("note name may not contain path separators or reserved characters".to_string());
    }
    if name == "." || name == ".." {
        return Err("note name is not a valid file name".to_string());
    }
    Ok(())
}

#[derive(Default)]
struct SlotFields {
    section_line: usize,
    fields: BTreeMap<String, (usize, String)>,
}

/// Parse the config text.
pub fn parse(text: &str) -> Result<Config> {
    if text.len() > MAX_FILE_BYTES {
        return Err(err(
            0,
            format!("file is larger than the {} byte limit", MAX_FILE_BYTES),
        ));
    }

    let mut version: Option<u32> = None;
    let mut shell_enabled = false;
    let mut globals_seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut sections: Vec<Option<SlotFields>> = (0..G_KEY_COUNT).map(|_| None).collect();
    let mut current: Option<usize> = None;

    for (index, raw_line) in text.lines().enumerate() {
        let line_no = index + 1;

        if raw_line.len() > MAX_LINE_BYTES {
            return Err(err(line_no, "line is too long"));
        }

        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        if line.starts_with('[') {
            let name = line
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .ok_or_else(|| err(line_no, "section header is missing its closing bracket"))?
                .trim();

            let number: usize = name
                .strip_prefix('G')
                .or_else(|| name.strip_prefix('g'))
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| {
                    err(
                        line_no,
                        format!("unknown section [{}], expected [G1] through [G18]", name),
                    )
                })?;

            if !(1..=G_KEY_COUNT).contains(&number) {
                return Err(err(
                    line_no,
                    format!("[G{}] is out of range, this keyboard has G1 to G18", number),
                ));
            }

            let slot_index = number - 1;
            if let Some(existing) = &sections[slot_index] {
                return Err(err(
                    line_no,
                    format!(
                        "[G{}] appears twice, first at line {}",
                        number, existing.section_line
                    ),
                ));
            }
            sections[slot_index] = Some(SlotFields {
                section_line: line_no,
                fields: BTreeMap::new(),
            });
            current = Some(slot_index);
            continue;
        }

        let equals = line
            .find('=')
            .ok_or_else(|| err(line_no, "expected 'key = value'"))?;
        let key = line[..equals].trim().to_ascii_lowercase();
        let value = line[equals + 1..].trim().to_string();

        if key.is_empty() {
            return Err(err(line_no, "key is empty"));
        }
        if value.chars().any(|c| c.is_control()) {
            return Err(err(line_no, "value contains a control character"));
        }

        match current {
            None => {
                if let Some(first) = globals_seen.get(&key) {
                    return Err(err(
                        line_no,
                        format!("'{}' is set twice, first at line {}", key, first),
                    ));
                }
                globals_seen.insert(key.clone(), line_no);

                match key.as_str() {
                    "version" => {
                        version = Some(value.parse().map_err(|_| {
                            err(line_no, format!("version '{}' is not a number", value))
                        })?)
                    }
                    "shell_enabled" => shell_enabled = parse_bool(&value, line_no)?,
                    other => {
                        return Err(err(line_no, format!("unknown setting '{}'", other)));
                    }
                }
            }
            Some(slot_index) => {
                let slot = sections[slot_index].as_mut().expect("section exists");
                if let Some((first, _)) = slot.fields.get(&key) {
                    return Err(err(
                        line_no,
                        format!("'{}' is set twice in this section, first at line {}", key, first),
                    ));
                }
                slot.fields.insert(key, (line_no, value));
            }
        }
    }

    let version = version.ok_or_else(|| err(0, "missing 'version'"))?;
    if version != SCHEMA_VERSION {
        return Err(err(
            0,
            format!(
                "config is version {}, this build understands version {}",
                version, SCHEMA_VERSION
            ),
        ));
    }

    let mut config = Config {
        version,
        shell_enabled,
        slots: std::array::from_fn(|_| None),
    };

    for (slot_index, section) in sections.into_iter().enumerate() {
        let section = match section {
            Some(section) => section,
            None => continue,
        };
        let key_name = format!("G{}", slot_index + 1);
        config.slots[slot_index] = build_slot(&key_name, section, shell_enabled)?;
    }

    Ok(config)
}

fn parse_bool(value: &str, line: usize) -> Result<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(err(
            line,
            format!("'{}' is not a boolean, use true or false", other),
        )),
    }
}

fn build_slot(key_name: &str, section: SlotFields, shell_enabled: bool) -> Result<Option<Slot>> {
    let fields = section.fields;
    let at = |name: &str| -> usize {
        fields
            .get(name)
            .map(|(line, _)| *line)
            .unwrap_or(section.section_line)
    };
    let get = |name: &str| -> Option<String> { fields.get(name).map(|(_, v)| v.clone()) };

    const KNOWN: [&str; 7] = [
        "action",
        "label",
        "target",
        "args",
        "workdir",
        "focus_if_running",
        "hidden",
    ];
    for (name, (line, _)) in fields.iter() {
        if !KNOWN.contains(&name.as_str()) {
            return Err(err(
                *line,
                format!("unknown key '{}' in [{}]", name, key_name),
            ));
        }
    }

    let action_name = get("action")
        .ok_or_else(|| err(section.section_line, format!("[{}] has no 'action'", key_name)))?;

    if action_name == "none" {
        return Ok(None);
    }

    let label = get("label")
        .ok_or_else(|| err(section.section_line, format!("[{}] has no 'label'", key_name)))?;
    if label.is_empty() {
        return Err(err(at("label"), "label is empty"));
    }
    if label.chars().count() > MAX_LABEL_CHARS {
        return Err(err(
            at("label"),
            format!("label is longer than {} characters", MAX_LABEL_CHARS),
        ));
    }

    let require_target = |field: &str| -> Result<String> {
        get(field).ok_or_else(|| {
            err(
                section.section_line,
                format!("[{}] has no '{}'", key_name, field),
            )
        })
    };

    let action = match action_name.as_str() {
        "launch" => {
            let target = require_target("target")?;
            let kind = classify_target(&target).map_err(|e| err(at("target"), e))?;
            if kind == TargetKind::Unc {
                return Err(err(
                    at("target"),
                    "UNC paths are refused: a network path can change under you between \
                     configuring a key and pressing it",
                ));
            }
            let args = get("args").unwrap_or_default();
            if args.chars().count() > MAX_ARGS_CHARS {
                return Err(err(at("args"), "args is too long"));
            }
            let workdir = get("workdir").unwrap_or_default();
            if !workdir.is_empty() {
                match classify_target(&workdir).map_err(|e| err(at("workdir"), e))? {
                    TargetKind::AbsolutePath => {}
                    _ => return Err(err(at("workdir"), "workdir must be an absolute path")),
                }
            }
            let focus = match get("focus_if_running") {
                Some(v) => parse_bool(&v, at("focus_if_running"))?,
                None => false,
            };
            Action::Launch {
                target,
                args,
                workdir,
                focus_if_running: focus,
            }
        }

        "open" => {
            let target = require_target("target")?;
            let kind = classify_target(&target).map_err(|e| err(at("target"), e))?;
            if kind == TargetKind::Unc {
                return Err(err(at("target"), "UNC paths are refused"));
            }
            Action::Open { target }
        }

        "note" => {
            let name = require_target("target")?;
            validate_note_name(&name).map_err(|e| err(at("target"), e))?;
            Action::Note { name }
        }

        "shell" => {
            if !shell_enabled {
                return Err(err(
                    section.section_line,
                    format!(
                        "[{}] is a shell action but 'shell_enabled' is not set. \
                         Shell actions are off unless you turn them on deliberately.",
                        key_name
                    ),
                ));
            }
            let command = require_target("target")?;
            if command.is_empty() {
                return Err(err(at("target"), "shell command is empty"));
            }
            if command.chars().any(|c| c.is_control()) {
                return Err(err(at("target"), "shell command contains a control character"));
            }
            let hidden = match get("hidden") {
                Some(v) => parse_bool(&v, at("hidden"))?,
                None => true,
            };
            Action::Shell { command, hidden }
        }

        other => {
            return Err(err(
                at("action"),
                format!(
                    "unknown action '{}', expected none, launch, open, note or shell",
                    other
                ),
            ))
        }
    };

    Ok(Some(Slot { label, action }))
}

/// Render the config back to text.
pub fn to_text(config: &Config) -> String {
    let mut out = String::new();
    out.push_str("# G Deck configuration\n");
    out.push_str("#\n");
    out.push_str("# Written by the app. Hand editing is fine: the parser is strict and will\n");
    out.push_str("# name the exact line it rejects rather than ignoring anything.\n");
    out.push_str("#\n");
    out.push_str("# Values run to the end of the line and need no quoting or escaping, so a\n");
    out.push_str("# path may contain spaces, backslashes, quotes or #. Comments must be on\n");
    out.push_str("# their own line.\n");
    out.push('\n');
    out.push_str(&format!("version = {}\n", config.version));
    out.push_str(&format!("shell_enabled = {}\n", config.shell_enabled));

    for index in 0..G_KEY_COUNT {
        let slot = match &config.slots[index] {
            Some(slot) => slot,
            None => continue,
        };
        out.push('\n');
        out.push_str(&format!("[G{}]\n", index + 1));
        out.push_str(&format!("action = {}\n", slot.action.type_name()));
        out.push_str(&format!("label = {}\n", slot.label));

        match &slot.action {
            Action::None => {}
            Action::Launch {
                target,
                args,
                workdir,
                focus_if_running,
            } => {
                out.push_str(&format!("target = {}\n", target));
                if !args.is_empty() {
                    out.push_str(&format!("args = {}\n", args));
                }
                if !workdir.is_empty() {
                    out.push_str(&format!("workdir = {}\n", workdir));
                }
                out.push_str(&format!("focus_if_running = {}\n", focus_if_running));
            }
            Action::Open { target } => out.push_str(&format!("target = {}\n", target)),
            Action::Note { name } => out.push_str(&format!("target = {}\n", name)),
            Action::Shell { command, hidden } => {
                out.push_str(&format!("target = {}\n", command));
                out.push_str(&format!("hidden = {}\n", hidden));
            }
        }
    }

    out
}

/// Read the config from the store. A missing file is not an error; it means
/// nothing has been configured yet.
pub fn load() -> Result<Config> {
    let path = store::config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| Error::Io {
        what: "read the config",
        detail: e.to_string(),
    })?;
    parse(&text)
}

/// Write the config, lock the file down, and seal it.
///
/// The permissions are applied after every write, not just at creation, so a
/// file that was replaced or had its descriptor loosened is re-secured rather
/// than silently inheriting whatever it ended up with.
///
/// Sealing happens here, in the same call, so the seal can never be left
/// describing an older config than the one on disk.
pub fn save(config: &Config, policy: &Policy) -> Result<()> {
    let dir = store::ensure_store_dir()?;
    crate::security::apply(&dir, &policy.directory_sddl())?;

    let text = to_text(config);
    let path = dir.join(store::CONFIG_FILE);
    std::fs::write(&path, &text).map_err(|e| Error::Io {
        what: "write the config",
        detail: e.to_string(),
    })?;

    crate::security::apply(Path::new(&path), &policy.file_sddl())?;
    crate::integrity::write_seal(&text, policy)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "version = 1\nshell_enabled = false\n";

    fn parse_ok(text: &str) -> Config {
        parse(text).expect("should parse")
    }

    fn parse_err(text: &str) -> String {
        format!("{}", parse(text).expect_err("should have been rejected"))
    }

    #[test]
    fn minimal_config_parses() {
        let config = parse_ok(MINIMAL);
        assert_eq!(config.version, 1);
        assert!(!config.shell_enabled);
        assert_eq!(config.assigned_count(), 0);
    }

    #[test]
    fn a_windows_path_survives_verbatim() {
        // The whole reason for this format. No escaping, no doubled
        // backslashes, and # inside a path is just a character.
        let path = r"C:\Program Files (x86)\Some #1 App\run it.exe";
        let text = format!(
            "{}\n[G1]\naction = launch\nlabel = Thing\ntarget = {}\n",
            MINIMAL, path
        );
        let config = parse_ok(&text);
        match &config.slot(GKey::from_index(0).unwrap()).unwrap().action {
            Action::Launch { target, .. } => assert_eq!(target, path),
            other => panic!("expected launch, got {:?}", other),
        }
    }

    #[test]
    fn round_trips_through_text() {
        let text = format!(
            "{}\n[G1]\naction = launch\nlabel = Steam\ntarget = C:\\Steam\\steam.exe\n\
             args = -silent\nfocus_if_running = true\n\n[G18]\naction = open\n\
             label = Docs\ntarget = https://example.com/docs\n",
            MINIMAL
        );
        let first = parse_ok(&text);
        let second = parse_ok(&to_text(&first));
        assert_eq!(first, second);
        assert_eq!(second.assigned_count(), 2);
    }

    #[test]
    fn relative_paths_and_bare_commands_are_refused() {
        // `notepad` would resolve against PATH at press time, so what a key
        // does could change because something else got installed.
        for bad in ["notepad", "notepad.exe", r"..\thing.exe", "./run.sh"] {
            let text = format!(
                "{}\n[G1]\naction = launch\nlabel = X\ntarget = {}\n",
                MINIMAL, bad
            );
            let msg = parse_err(&text);
            assert!(msg.contains("absolute path"), "{} gave: {}", bad, msg);
        }
    }

    #[test]
    fn unknown_uri_schemes_are_refused() {
        let text = format!(
            "{}\n[G1]\naction = open\nlabel = X\ntarget = javascript:alert(1)\n",
            MINIMAL
        );
        assert!(parse_err(&text).contains("allowlist"));
    }

    #[test]
    fn unc_paths_are_refused() {
        let text = format!(
            "{}\n[G1]\naction = launch\nlabel = X\ntarget = \\\\server\\share\\app.exe\n",
            MINIMAL
        );
        assert!(parse_err(&text).contains("UNC"));
    }

    #[test]
    fn shell_actions_need_shell_enabled() {
        let off = format!(
            "{}\n[G1]\naction = shell\nlabel = X\ntarget = ipconfig /flushdns\n",
            MINIMAL
        );
        assert!(parse_err(&off).contains("shell_enabled"));

        let on = "version = 1\nshell_enabled = true\n\n[G1]\naction = shell\nlabel = X\ntarget = ipconfig /flushdns\n";
        let config = parse_ok(on);
        match &config.slot(GKey::from_index(0).unwrap()).unwrap().action {
            Action::Shell { hidden, .. } => assert!(hidden, "shell should default to hidden"),
            other => panic!("expected shell, got {:?}", other),
        }
    }

    #[test]
    fn unknown_keys_and_sections_are_errors_not_ignored() {
        let unknown_key = format!("{}\n[G1]\naction = none\nsneaky = yes\n", MINIMAL);
        assert!(parse_err(&unknown_key).contains("unknown key"));

        let unknown_section = format!("{}\n[G99]\naction = none\n", MINIMAL);
        assert!(parse_err(&unknown_section).contains("out of range"));

        let unknown_global = format!("{}extra = 1\n", MINIMAL);
        assert!(parse_err(&unknown_global).contains("unknown setting"));
    }

    #[test]
    fn duplicates_are_errors() {
        let dup_section = format!("{}\n[G1]\naction = none\n\n[G1]\naction = none\n", MINIMAL);
        assert!(parse_err(&dup_section).contains("twice"));

        let dup_key = format!("{}\n[G1]\naction = none\naction = none\n", MINIMAL);
        assert!(parse_err(&dup_key).contains("twice"));
    }

    #[test]
    fn errors_name_the_line() {
        // Written out literally so the expected line number is readable rather
        // than something to work out from string concatenation.
        let text = concat!(
            "version = 1\n",           // 1
            "shell_enabled = false\n", // 2
            "\n",                      // 3
            "[G1]\n",                  // 4
            "action = launch\n",       // 5
            "label = X\n",             // 6
            "target = nope\n",         // 7
        );
        let msg = parse_err(text);
        assert!(msg.contains("line 7"), "message was: {}", msg);
    }

    #[test]
    fn a_missing_version_is_rejected() {
        assert!(parse_err("shell_enabled = false\n").contains("missing 'version'"));
    }

    #[test]
    fn a_future_version_is_rejected_rather_than_guessed_at() {
        assert!(parse_err("version = 99\n").contains("version 99"));
    }

    #[test]
    fn note_names_cannot_escape_the_notes_folder() {
        for bad in [r"..\..\config.ini", r"sub\note", "C:/note", "."] {
            let text = format!("{}\n[G1]\naction = note\nlabel = N\ntarget = {}\n", MINIMAL, bad);
            assert!(
                parse(&text).is_err(),
                "note name {:?} should have been refused",
                bad
            );
        }
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let text = format!(
            "# a comment\n; another\n\n{}\n# before a section\n[G2]\naction = none\n",
            MINIMAL
        );
        assert_eq!(parse_ok(&text).assigned_count(), 0);
    }

    #[test]
    fn an_oversized_file_is_refused_before_parsing() {
        let huge = "#".repeat(MAX_FILE_BYTES + 1);
        assert!(parse_err(&huge).contains("larger than"));
    }
}
