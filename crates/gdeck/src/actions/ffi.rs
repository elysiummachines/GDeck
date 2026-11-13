//! Win32 bindings for starting processes and finding windows.
//!
//! Separate again from the HID bindings in `crate::ffi`, which stay provably
//! incapable of writing to a device. This module is where the program gains
//! the ability to start things, so it is worth it being its own short, readable
//! file rather than buried among unrelated calls.

#![allow(non_snake_case)]

use core::ffi::c_void;

#[repr(C)]
pub struct StartupInfoW {
    pub cb: u32,
    pub reserved: *mut u16,
    pub desktop: *mut u16,
    pub title: *mut u16,
    pub x: u32,
    pub y: u32,
    pub x_size: u32,
    pub y_size: u32,
    pub x_count_chars: u32,
    pub y_count_chars: u32,
    pub fill_attribute: u32,
    pub flags: u32,
    pub show_window: u16,
    pub reserved2_len: u16,
    pub reserved2: *mut u8,
    pub std_input: isize,
    pub std_output: isize,
    pub std_error: isize,
}

impl Default for StartupInfoW {
    fn default() -> Self {
        let mut info: StartupInfoW = unsafe { core::mem::zeroed() };
        info.cb = core::mem::size_of::<StartupInfoW>() as u32;
        info
    }
}

#[repr(C)]
#[derive(Default)]
pub struct ProcessInformation {
    pub process: isize,
    pub thread: isize,
    pub process_id: u32,
    pub thread_id: u32,
}

/// `SHELLEXECUTEINFOW`. Padding between the `i32`/`u32` fields and the pointer
/// sized ones is inserted by `repr(C)` exactly as a C compiler would.
#[repr(C)]
pub struct ShellExecuteInfoW {
    pub cb_size: u32,
    pub mask: u32,
    pub hwnd: isize,
    pub verb: *const u16,
    pub file: *const u16,
    pub parameters: *const u16,
    pub directory: *const u16,
    pub show: i32,
    pub inst_app: isize,
    pub id_list: *mut c_void,
    pub class: *const u16,
    pub hkey_class: isize,
    pub hot_key: u32,
    pub icon_or_monitor: isize,
    pub process: isize,
}

impl Default for ShellExecuteInfoW {
    fn default() -> Self {
        let mut info: ShellExecuteInfoW = unsafe { core::mem::zeroed() };
        info.cb_size = core::mem::size_of::<ShellExecuteInfoW>() as u32;
        info
    }
}

#[repr(C)]
pub struct SecurityAttributes {
    pub length: u32,
    pub descriptor: *mut c_void,
    pub inherit_handle: i32,
}

pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;
/// Without this, a child started from a process that already owns a console
/// attaches to that one instead of creating a visible window of its own.
pub const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

pub const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
pub const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
pub const WAIT_OBJECT_0: u32 = 0;
pub const WAIT_TIMEOUT: u32 = 258;

pub const SW_HIDE: i32 = 0;
pub const SW_SHOWNORMAL: i32 = 1;
pub const SW_RESTORE: i32 = 9;

pub const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
pub const SEE_MASK_NOASYNC: u32 = 0x0000_0100;
pub const SEE_MASK_FLAG_NO_UI: u32 = 0x0000_0400;

pub const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x0000_1000;

pub const COINIT_APARTMENTTHREADED: u32 = 0x2;
pub const COINIT_DISABLE_OLE1DDE: u32 = 0x4;

extern "system" {
    pub fn CreateProcessW(
        application_name: *const u16,
        command_line: *mut u16,
        process_attributes: *mut c_void,
        thread_attributes: *mut c_void,
        inherit_handles: i32,
        creation_flags: u32,
        environment: *mut c_void,
        current_directory: *const u16,
        startup_info: *mut StartupInfoW,
        process_information: *mut ProcessInformation,
    ) -> i32;
    pub fn CloseHandle(handle: isize) -> i32;
    pub fn GetLastError() -> u32;
    pub fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> isize;
    pub fn QueryFullProcessImageNameW(
        process: isize,
        flags: u32,
        name: *mut u16,
        size: *mut u32,
    ) -> i32;
    pub fn CreatePipe(
        read: *mut isize,
        write: *mut isize,
        attributes: *mut SecurityAttributes,
        size: u32,
    ) -> i32;
    pub fn SetHandleInformation(handle: isize, mask: u32, flags: u32) -> i32;
    // ReadFile is deliberately not redeclared here. `crate::ffi` already
    // declares it taking a `*mut Overlapped`, and two extern declarations of
    // one symbol with different signatures is a real hazard, not a style
    // question. This module uses that one.
    pub fn WaitForSingleObject(handle: isize, millis: u32) -> u32;
    pub fn GetExitCodeProcess(process: isize, code: *mut u32) -> i32;
}

#[link(name = "user32")]
extern "system" {
    pub fn EnumWindows(callback: extern "system" fn(isize, isize) -> i32, param: isize) -> i32;
    pub fn GetWindowThreadProcessId(window: isize, process_id: *mut u32) -> u32;
    pub fn IsWindowVisible(window: isize) -> i32;
    pub fn GetWindowTextLengthW(window: isize) -> i32;
    pub fn IsIconic(window: isize) -> i32;
    pub fn ShowWindow(window: isize, command: i32) -> i32;
    pub fn SetForegroundWindow(window: isize) -> i32;
    pub fn GetWindow(window: isize, command: u32) -> isize;
}

#[link(name = "shell32")]
extern "system" {
    pub fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
}

#[link(name = "ole32")]
extern "system" {
    pub fn CoInitializeEx(reserved: *mut c_void, flags: u32) -> i32;
}
