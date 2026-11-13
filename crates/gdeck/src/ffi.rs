//! Hand written Win32 bindings.
//!
//! This module is the complete list of operating system calls the application
//! makes against the keyboard. It declares **no HID write function**: no
//! `HidD_SetFeature`, no `HidD_SetOutputReport`, no `HidD_FlushQueue`, no
//! `WriteFile`, no `DeviceIoControl`. Audit it on the built binary:
//!
//! ```text
//! dumpbin /imports gdeck.exe
//! ```
//!
//! Against HID.DLL that shows only `HidD_Get*` and `HidP_Get*`, so no code path
//! exists that can send a byte to the device.
//!
//! One caveat, stated because the audit does show it: `NtWriteFile` appears in
//! the import table. It is the Rust standard library's, used for console and
//! file output, not this module's. So the honest claim is "no HID write
//! surface", which is checkable, rather than "no write syscall exists in the
//! process", which is untrue of any program that can print.
//!
//! Keep it that way. Adding a write capable HID import here silently removes
//! the strongest security claim the project makes.

#![allow(non_snake_case)]

use core::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

#[repr(C)]
pub struct SpDeviceInterfaceData {
    pub cb_size: u32,
    pub interface_class_guid: Guid,
    pub flags: u32,
    pub reserved: usize,
}

#[repr(C)]
#[derive(Default)]
pub struct HiddAttributes {
    pub size: u32,
    pub vendor_id: u16,
    pub product_id: u16,
    pub version_number: u16,
}

/// `HIDP_CAPS` is 64 bytes. Field order matters; do not reorder.
#[repr(C)]
#[derive(Default)]
pub struct HidpCaps {
    pub usage: u16,
    pub usage_page: u16,
    pub input_report_byte_length: u16,
    pub output_report_byte_length: u16,
    pub feature_report_byte_length: u16,
    pub reserved: [u16; 17],
    pub number_link_collection_nodes: u16,
    pub number_input_button_caps: u16,
    pub number_input_value_caps: u16,
    pub number_input_data_indices: u16,
    pub number_output_button_caps: u16,
    pub number_output_value_caps: u16,
    pub number_output_data_indices: u16,
    pub number_feature_button_caps: u16,
    pub number_feature_value_caps: u16,
    pub number_feature_data_indices: u16,
}

#[repr(C)]
pub struct Overlapped {
    pub internal: usize,
    pub internal_high: usize,
    pub offset: u32,
    pub offset_high: u32,
    pub h_event: isize,
}

impl Overlapped {
    pub fn new(event: isize) -> Self {
        Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            h_event: event,
        }
    }
}

pub const INVALID_HANDLE_VALUE: isize = -1;
pub const DIGCF_PRESENT: u32 = 0x0000_0002;
pub const DIGCF_DEVICEINTERFACE: u32 = 0x0000_0010;
pub const GENERIC_READ: u32 = 0x8000_0000;
pub const FILE_SHARE_READ: u32 = 0x0000_0001;
pub const FILE_SHARE_WRITE: u32 = 0x0000_0002;
pub const OPEN_EXISTING: u32 = 3;
pub const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;

pub const ERROR_IO_PENDING: u32 = 997;
pub const WAIT_OBJECT_0: u32 = 0;
pub const WAIT_TIMEOUT: u32 = 258;
pub const WAIT_FAILED: u32 = 0xFFFF_FFFF;
pub const INFINITE: u32 = 0xFFFF_FFFF;
pub const HIDP_STATUS_SUCCESS: i32 = 0x0011_0000;

extern "system" {
    pub fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const c_void,
        disposition: u32,
        flags: u32,
        template: isize,
    ) -> isize;
    pub fn CloseHandle(handle: isize) -> i32;
    pub fn ReadFile(
        handle: isize,
        buffer: *mut u8,
        len: u32,
        read: *mut u32,
        overlapped: *mut Overlapped,
    ) -> i32;
    pub fn CancelIoEx(handle: isize, overlapped: *mut Overlapped) -> i32;
    pub fn GetOverlappedResult(
        handle: isize,
        overlapped: *mut Overlapped,
        transferred: *mut u32,
        wait: i32,
    ) -> i32;
    pub fn CreateEventW(
        security: *const c_void,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> isize;
    pub fn ResetEvent(handle: isize) -> i32;
    pub fn SetEvent(handle: isize) -> i32;
    pub fn WaitForSingleObject(handle: isize, millis: u32) -> u32;
    pub fn WaitForMultipleObjects(
        count: u32,
        handles: *const isize,
        wait_all: i32,
        millis: u32,
    ) -> u32;
    pub fn GetLastError() -> u32;
}

#[link(name = "setupapi")]
extern "system" {
    pub fn SetupDiGetClassDevsW(
        guid: *const Guid,
        enumerator: *const u16,
        hwnd: isize,
        flags: u32,
    ) -> isize;
    pub fn SetupDiEnumDeviceInterfaces(
        set: isize,
        device_info: *mut c_void,
        guid: *const Guid,
        index: u32,
        data: *mut SpDeviceInterfaceData,
    ) -> i32;
    pub fn SetupDiGetDeviceInterfaceDetailW(
        set: isize,
        data: *mut SpDeviceInterfaceData,
        detail: *mut u8,
        detail_size: u32,
        required: *mut u32,
        device_info: *mut c_void,
    ) -> i32;
    pub fn SetupDiDestroyDeviceInfoList(set: isize) -> i32;
}

#[link(name = "hid")]
extern "system" {
    pub fn HidD_GetHidGuid(guid: *mut Guid);
    pub fn HidD_GetAttributes(handle: isize, attrs: *mut HiddAttributes) -> u8;
    pub fn HidD_GetPreparsedData(handle: isize, preparsed: *mut isize) -> u8;
    pub fn HidD_FreePreparsedData(preparsed: isize) -> u8;
    pub fn HidP_GetCaps(preparsed: isize, caps: *mut HidpCaps) -> i32;
    pub fn HidD_GetProductString(handle: isize, buffer: *mut u16, len: u32) -> u8;
}

/// Null terminated UTF-16, for the `W` entry points.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}
