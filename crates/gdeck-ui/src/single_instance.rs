//! One G Deck at a time.
//!
//! Every launch of the executable is a new process. Clicking a pinned taskbar
//! button is a launch, so without this you collect a process per click, each
//! with its own keyboard reader, and one G key press fires the action once per
//! running copy.
//!
//! A named mutex is the standard answer. The name lives in the `Local`
//! namespace, which is per logon session, so a second user signed in at the
//! same time gets their own G Deck rather than being told one is running.

#![allow(non_snake_case)]

use core::ffi::c_void;
use core::ptr;

const ERROR_ALREADY_EXISTS: u32 = 183;

/// Versioned, so a future change to the handover protocol cannot be confused
/// by a copy of the old build still running.
const MUTEX_NAME: &str = r"Local\GDeck.SingleInstance.v1";

#[link(name = "kernel32")]
extern "system" {
    fn CreateMutexW(attributes: *const c_void, initial_owner: i32, name: *const u16) -> isize;
    fn GetLastError() -> u32;
    fn CloseHandle(handle: isize) -> i32;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Holds the claim for as long as this process runs.
pub struct Guard {
    handle: isize,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.handle != 0 {
            unsafe { CloseHandle(self.handle) };
        }
    }
}

/// Claim the single instance name.
///
/// `None` means another G Deck already holds it and this process should hand
/// over and leave. If the claim cannot be made at all, which should not
/// happen, this returns a guard anyway: refusing to start is a worse failure
/// than briefly running twice.
pub fn acquire() -> Option<Guard> {
    unsafe {
        let name = wide(MUTEX_NAME);
        let handle = CreateMutexW(ptr::null(), 0, name.as_ptr());
        if handle == 0 {
            return Some(Guard { handle: 0 });
        }
        // CreateMutexW hands back the existing object rather than failing, so
        // the last error is the only thing that distinguishes the two cases.
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(handle);
            return None;
        }
        Some(Guard { handle })
    }
}
