//! G Deck: read only G key input for the Corsair K95 RGB.
//!
//! The application never writes to the keyboard: it links no HID write
//! function at all (see [`ffi`] for the audit). It opens one vendor HID
//! collection for reading, and that collection carries nothing but G key state.
//! Normal typing travels on a separate collection that Windows claims
//! exclusively and refuses to hand out, so this process cannot observe
//! keystrokes even in principle. No keyboard hook is installed anywhere.
//!
//! See docs/PHASE0-FINDINGS.md for the measurements behind all of that.

pub mod actions;
pub mod audit;
pub mod config;
pub mod device;
pub mod ffi;
pub mod integrity;
pub mod keys;
pub mod reader;
pub mod security;
pub mod store;

pub use config::{Action, Config, Slot};
pub use device::{find_g_key_pipe, Collection};
pub use keys::{decode_state, is_held, GKey, KeyState, G_KEY_BITS, G_KEY_COUNT};
pub use reader::{Event, Reader};
pub use security::{Policy, Sid};

use core::fmt;

#[derive(Debug)]
pub enum Error {
    /// The keyboard is not attached, or its G key collection was not found.
    DeviceNotFound,
    /// A Win32 call failed.
    Win32 { what: &'static str, code: u32 },
    /// `%LOCALAPPDATA%` is not set, which should not happen on Windows.
    NoLocalAppData,
    /// A filesystem operation failed.
    Io {
        what: &'static str,
        detail: String,
    },
    /// The config file was rejected. Carries the line so it can be pointed at.
    Config { line: usize, detail: String },
    /// An action was refused by policy before anything ran.
    Refused { reason: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::DeviceNotFound => {
                write!(f, "Corsair K95 RGB not found (expected 1b1c:1b11)")
            }
            Error::Win32 { what, code } => {
                write!(f, "{} failed, win32 error {}", what, code)?;
                if *code == 5 {
                    write!(f, " (access denied)")?;
                }
                Ok(())
            }
            Error::NoLocalAppData => write!(f, "LOCALAPPDATA is not set"),
            Error::Io { what, detail } => write!(f, "could not {}: {}", what, detail),
            Error::Config { line, detail } => {
                if *line == 0 {
                    write!(f, "config rejected: {}", detail)
                } else {
                    write!(f, "config rejected at line {}: {}", line, detail)
                }
            }
            Error::Refused { reason } => write!(f, "refused: {}", reason),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = core::result::Result<T, Error>;
