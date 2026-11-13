//! Native file and folder pickers.
//!
//! Hand written against the Win32 common dialogs rather than pulling a crate,
//! which keeps the dependency count where it is and gives the real Windows
//! dialog rather than a drawn imitation.
//!
//! These only read a path chosen by the person using the app. Nothing here
//! opens, writes or executes anything.

#![allow(non_snake_case)]

use core::ffi::c_void;
use core::ptr;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// A filter string is a run of null terminated pairs ending in a second null.
fn filter(pairs: &[(&str, &str)]) -> Vec<u16> {
    let mut out = Vec::new();
    for (label, pattern) in pairs {
        out.extend(label.encode_utf16());
        out.push(0);
        out.extend(pattern.encode_utf16());
        out.push(0);
    }
    out.push(0);
    out
}

#[repr(C)]
struct OpenFileNameW {
    struct_size: u32,
    owner: isize,
    instance: isize,
    filter: *const u16,
    custom_filter: *mut u16,
    max_custom_filter: u32,
    filter_index: u32,
    file: *mut u16,
    max_file: u32,
    file_title: *mut u16,
    max_file_title: u32,
    initial_dir: *const u16,
    title: *const u16,
    flags: u32,
    file_offset: u16,
    file_extension: u16,
    default_ext: *const u16,
    custom_data: isize,
    hook: isize,
    template_name: *const u16,
    reserved_ptr: *mut c_void,
    reserved: u32,
    flags_ex: u32,
}

#[repr(C)]
struct BrowseInfoW {
    owner: isize,
    root: *const c_void,
    display_name: *mut u16,
    title: *const u16,
    flags: u32,
    callback: isize,
    param: isize,
    image: i32,
}

const OFN_PATHMUSTEXIST: u32 = 0x0000_0800;
const OFN_FILEMUSTEXIST: u32 = 0x0000_1000;
const OFN_NOCHANGEDIR: u32 = 0x0000_0008;
const OFN_EXPLORER: u32 = 0x0008_0000;

const BIF_RETURNONLYFSDIRS: u32 = 0x0000_0001;
const BIF_NEWDIALOGSTYLE: u32 = 0x0000_0040;

#[link(name = "comdlg32")]
extern "system" {
    fn GetOpenFileNameW(arg: *mut OpenFileNameW) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    fn SHBrowseForFolderW(info: *mut BrowseInfoW) -> *mut c_void;
    fn SHGetPathFromIDListW(list: *mut c_void, path: *mut u16) -> i32;
}

#[link(name = "ole32")]
extern "system" {
    fn CoTaskMemFree(block: *mut c_void);
}

fn string_from(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// Ask for a program. Returns `None` if the dialog was cancelled.
pub fn pick_program() -> Option<String> {
    pick_file(&[
        ("Programs", "*.exe;*.com;*.bat;*.cmd;*.lnk"),
        ("All files", "*.*"),
    ])
}

/// Ask for any file.
pub fn pick_any_file() -> Option<String> {
    pick_file(&[("All files", "*.*")])
}

fn pick_file(filters: &[(&str, &str)]) -> Option<String> {
    let mut buffer = vec![0u16; 32768];
    let filters = filter(filters);

    let mut args = OpenFileNameW {
        struct_size: core::mem::size_of::<OpenFileNameW>() as u32,
        owner: 0,
        instance: 0,
        filter: filters.as_ptr(),
        custom_filter: ptr::null_mut(),
        max_custom_filter: 0,
        filter_index: 1,
        file: buffer.as_mut_ptr(),
        max_file: buffer.len() as u32,
        file_title: ptr::null_mut(),
        max_file_title: 0,
        initial_dir: ptr::null(),
        title: ptr::null(),
        flags: OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR | OFN_EXPLORER,
        file_offset: 0,
        file_extension: 0,
        default_ext: ptr::null(),
        custom_data: 0,
        hook: 0,
        template_name: ptr::null(),
        reserved_ptr: ptr::null_mut(),
        reserved: 0,
        flags_ex: 0,
    };

    // Zero means cancelled or failed. Either way there is nothing to return.
    if unsafe { GetOpenFileNameW(&mut args) } == 0 {
        return None;
    }
    let chosen = string_from(&buffer);
    if chosen.is_empty() {
        None
    } else {
        Some(chosen)
    }
}

/// Ask for a folder. Returns `None` if the dialog was cancelled.
pub fn pick_folder() -> Option<String> {
    let title = wide("Choose a folder");
    let mut display = vec![0u16; 260];

    let mut info = BrowseInfoW {
        owner: 0,
        root: ptr::null(),
        display_name: display.as_mut_ptr(),
        title: title.as_ptr(),
        flags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
        callback: 0,
        param: 0,
        image: 0,
    };

    let list = unsafe { SHBrowseForFolderW(&mut info) };
    if list.is_null() {
        return None;
    }

    let mut path = vec![0u16; 32768];
    let ok = unsafe { SHGetPathFromIDListW(list, path.as_mut_ptr()) };
    unsafe { CoTaskMemFree(list) };

    if ok == 0 {
        return None;
    }
    let chosen = string_from(&path);
    if chosen.is_empty() {
        None
    } else {
        Some(chosen)
    }
}
