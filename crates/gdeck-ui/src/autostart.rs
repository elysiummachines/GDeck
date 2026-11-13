//! Starting with Windows, if you ask for it.
//!
//! This is the one feature that writes anything outside the project's own
//! folder, so the choices are deliberate and narrow:
//!
//! * `HKEY_CURRENT_USER` only. The machine wide key needs administrator
//!   rights and would start G Deck for every account on the box, including
//!   ones that never asked for it.
//! * Not a service and not a scheduled task. Both can be made to run as
//!   SYSTEM, which is a far larger thing to hand a key mapper than it needs.
//! * The stored command is the executable path in quotes. Unquoted, a path
//!   containing a space lets Windows try shorter prefixes first, so a file
//!   planted beside a parent directory can be launched instead. Quoting
//!   closes that, and it is the same defect as the unquoted service path
//!   problem.
//! * Removing the value is the whole uninstall. Nothing else is left behind.

#![allow(non_snake_case)]

use core::ptr;

const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "G Deck";

/// The exact location the toggle writes, spelled out for the interface so a
/// reader can go and look at it rather than take our word for it.
pub const RUN_KEY_DISPLAY: &str =
    r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run";

const ERROR_SUCCESS: i32 = 0;
const ERROR_FILE_NOT_FOUND: i32 = 2;

const KEY_QUERY_VALUE: u32 = 0x0001;
const KEY_SET_VALUE: u32 = 0x0002;

const REG_SZ: u32 = 1;
const REG_EXPAND_SZ: u32 = 2;

/// Reported when the running executable's own path cannot be determined, so
/// there is nothing meaningful to write.
const NO_EXE_PATH: i32 = -1;

#[link(name = "advapi32")]
extern "system" {
    fn RegOpenKeyExW(
        key: isize,
        sub_key: *const u16,
        options: u32,
        desired: u32,
        out: *mut isize,
    ) -> i32;
    fn RegSetValueExW(
        key: isize,
        name: *const u16,
        reserved: u32,
        kind: u32,
        data: *const u8,
        len: u32,
    ) -> i32;
    fn RegQueryValueExW(
        key: isize,
        name: *const u16,
        reserved: *mut u32,
        kind: *mut u32,
        data: *mut u8,
        len: *mut u32,
    ) -> i32;
    fn RegDeleteValueW(key: isize, name: *const u16) -> i32;
    fn RegCloseKey(key: isize) -> i32;
}

/// What the registry currently says.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum State {
    /// No value under our name.
    Off,
    /// A value that points at the executable now running.
    On,
    /// A value is there, but not for this file. Left alone rather than
    /// silently corrected, because it may belong to a copy elsewhere that
    /// the person actually wants.
    Stale { command: String },
    /// The key could not be read.
    Unreadable { code: i32 },
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

fn quote(path: &str) -> String {
    format!("\"{}\"", path)
}

/// The command to store: this executable, in quotes.
fn command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(quote(&exe.to_string_lossy()))
}

/// Read what is there now.
pub fn state() -> State {
    let wanted = match command() {
        Some(command) => command,
        None => return State::Unreadable { code: NO_EXE_PATH },
    };

    unsafe {
        let mut key = 0isize;
        let status = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        );
        if status != ERROR_SUCCESS {
            return State::Unreadable { code: status };
        }
        let found = read(key);
        RegCloseKey(key);

        match found {
            Ok(None) => State::Off,
            // Paths are not case sensitive on Windows, so neither is this.
            Ok(Some(found)) if found.eq_ignore_ascii_case(&wanted) => State::On,
            Ok(Some(found)) => State::Stale { command: found },
            Err(code) => State::Unreadable { code },
        }
    }
}

unsafe fn read(key: isize) -> Result<Option<String>, i32> {
    let name = wide(VALUE_NAME);
    let mut kind = 0u32;
    let mut len = 0u32;

    // The first call sizes the buffer, the second fills it.
    let status = RegQueryValueExW(
        key,
        name.as_ptr(),
        ptr::null_mut(),
        &mut kind,
        ptr::null_mut(),
        &mut len,
    );
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(status);
    }
    if kind != REG_SZ && kind != REG_EXPAND_SZ {
        // Something that is not text is sitting under our name. Report it as
        // occupied rather than overwriting whatever it turns out to be.
        return Ok(Some(format!("a registry value of type {}, not text", kind)));
    }

    let mut buffer = vec![0u8; len as usize];
    let status = RegQueryValueExW(
        key,
        name.as_ptr(),
        ptr::null_mut(),
        &mut kind,
        buffer.as_mut_ptr(),
        &mut len,
    );
    if status != ERROR_SUCCESS {
        return Err(status);
    }

    let units: Vec<u16> = buffer
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let text = String::from_utf16_lossy(&units);
    Ok(Some(text.trim_end_matches('\0').to_string()))
}

/// Start with Windows, for this account only.
pub fn enable() -> Result<(), i32> {
    let command = match command() {
        Some(command) => command,
        None => return Err(NO_EXE_PATH),
    };

    unsafe {
        let mut key = 0isize;
        let status = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut key,
        );
        if status != ERROR_SUCCESS {
            return Err(status);
        }

        let data = wide(&command);
        // The length counts the terminating null, which a REG_SZ value is
        // expected to carry.
        let bytes = data.len() * core::mem::size_of::<u16>();
        let status = RegSetValueExW(
            key,
            wide(VALUE_NAME).as_ptr(),
            0,
            REG_SZ,
            data.as_ptr() as *const u8,
            bytes as u32,
        );
        RegCloseKey(key);
        if status != ERROR_SUCCESS {
            return Err(status);
        }
    }
    Ok(())
}

/// Stop starting with Windows. Removing the value is the whole of it.
pub fn disable() -> Result<(), i32> {
    unsafe {
        let mut key = 0isize;
        let status = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut key,
        );
        if status != ERROR_SUCCESS {
            return Err(status);
        }
        let status = RegDeleteValueW(key, wide(VALUE_NAME).as_ptr());
        RegCloseKey(key);
        // Already gone is the outcome that was asked for.
        if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
            return Err(status);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_with_spaces_is_quoted() {
        // Unquoted, Windows would try C:\Program.exe first, so a file planted
        // there would run instead of the real one.
        assert_eq!(
            quote(r"C:\Program Files\G Deck\gdeck-ui.exe"),
            "\"C:\\Program Files\\G Deck\\gdeck-ui.exe\""
        );
    }

    #[test]
    fn the_command_is_this_executable_quoted() {
        let command = command().expect("the running executable has a path");
        assert!(command.starts_with('"'));
        assert!(command.ends_with('"'));
        assert!(command.to_lowercase().contains(".exe"));
    }

    /// Touches the real registry, so it is kept out of the default run and
    /// asked for deliberately with `cargo test -p gdeck-ui -- --ignored`.
    /// It refuses to run at all if a value is already there, so it can never
    /// overwrite a real one, and it leaves the key as it found it.
    #[test]
    #[ignore = "writes to HKCU, run it deliberately"]
    fn enable_then_disable_leaves_the_key_as_it_was() {
        assert_eq!(
            state(),
            State::Off,
            "something is already under this name; refusing to overwrite it"
        );

        enable().expect("write the Run value");
        assert_eq!(state(), State::On, "the value should point at this test");

        disable().expect("remove the Run value");
        assert_eq!(state(), State::Off, "the key must be left as it was found");
    }

    #[test]
    fn the_run_key_is_the_per_user_one() {
        // Not the machine wide hive, which needs administrator rights and
        // would start G Deck for every account on this computer.
        assert_eq!(HKEY_CURRENT_USER, 0x8000_0001u32 as i32 as isize);
        assert!(!RUN_KEY.starts_with('\\'));
        assert!(RUN_KEY.ends_with(r"CurrentVersion\Run"));
    }
}
