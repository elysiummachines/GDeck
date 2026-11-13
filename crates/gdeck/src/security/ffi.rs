//! Win32 bindings for tokens, SIDs and access control.
//!
//! Deliberately separate from the HID bindings in `crate::ffi`.
//!
//! These calls *can* write: applying a DACL modifies an object's security
//! descriptor. Keeping them in their own module means the device facing FFI
//! stays provably read only, and the write capability that does exist in this
//! program is confined to files and their permissions, where it is visible.
//!
//! Nothing here can reach a HID device.

#![allow(non_snake_case)]

use core::ffi::c_void;

pub const TOKEN_QUERY: u32 = 0x0008;
/// `TOKEN_INFORMATION_CLASS::TokenUser`
pub const TOKEN_USER_CLASS: u32 = 1;

/// `SE_OBJECT_TYPE::SE_FILE_OBJECT`
pub const SE_FILE_OBJECT: u32 = 1;

pub const OWNER_SECURITY_INFORMATION: u32 = 0x0000_0001;
pub const GROUP_SECURITY_INFORMATION: u32 = 0x0000_0002;
pub const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
/// Severs inheritance: parent ACEs cannot flow in.
pub const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;

pub const SDDL_REVISION_1: u32 = 1;
pub const ERROR_SUCCESS: u32 = 0;

/// `TOKEN_USER` is a single `SID_AND_ATTRIBUTES`.
#[repr(C)]
pub struct TokenUser {
    pub sid: *mut c_void,
    pub attributes: u32,
}

extern "system" {
    pub fn GetCurrentProcess() -> isize;
    pub fn OpenProcessToken(process: isize, access: u32, token: *mut isize) -> i32;
    pub fn GetTokenInformation(
        token: isize,
        class: u32,
        info: *mut c_void,
        len: u32,
        returned: *mut u32,
    ) -> i32;
    pub fn CloseHandle(handle: isize) -> i32;
    pub fn LocalFree(mem: *mut c_void) -> *mut c_void;
    pub fn GetLastError() -> u32;
}

#[link(name = "advapi32")]
extern "system" {
    pub fn ConvertSidToStringSidW(sid: *mut c_void, out: *mut *mut u16) -> i32;
    pub fn ConvertStringSidToSidW(sddl_sid: *const u16, sid: *mut *mut c_void) -> i32;
    pub fn GetLengthSid(sid: *mut c_void) -> u32;
    pub fn CopySid(dest_len: u32, dest: *mut c_void, src: *mut c_void) -> i32;
    pub fn IsValidSid(sid: *mut c_void) -> i32;

    pub fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        sddl: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
    pub fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
        descriptor: *mut c_void,
        revision: u32,
        information: u32,
        out: *mut *mut u16,
        out_len: *mut u32,
    ) -> i32;

    pub fn GetSecurityDescriptorDacl(
        descriptor: *mut c_void,
        present: *mut i32,
        dacl: *mut *mut c_void,
        defaulted: *mut i32,
    ) -> i32;
    pub fn GetSecurityDescriptorOwner(
        descriptor: *mut c_void,
        owner: *mut *mut c_void,
        defaulted: *mut i32,
    ) -> i32;

    pub fn SetNamedSecurityInfoW(
        name: *mut u16,
        object_type: u32,
        information: u32,
        owner: *mut c_void,
        group: *mut c_void,
        dacl: *mut c_void,
        sacl: *mut c_void,
    ) -> u32;
    pub fn GetNamedSecurityInfoW(
        name: *const u16,
        object_type: u32,
        information: u32,
        owner: *mut *mut c_void,
        group: *mut *mut c_void,
        dacl: *mut *mut c_void,
        sacl: *mut *mut c_void,
        descriptor: *mut *mut c_void,
    ) -> u32;

    pub fn LookupAccountSidW(
        system: *const u16,
        sid: *mut c_void,
        name: *mut u16,
        name_len: *mut u32,
        domain: *mut u16,
        domain_len: *mut u32,
        kind: *mut u32,
    ) -> i32;
}

/// Read a null terminated UTF-16 string returned by a Win32 call.
///
/// # Safety
/// `ptr` must point at a null terminated UTF-16 sequence.
pub unsafe fn string_from_wide(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(core::slice::from_raw_parts(ptr, len))
}
