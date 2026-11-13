//! Finding and opening the keyboard's G key pipe.

use core::ptr;

use crate::ffi::*;
use crate::keys::MIN_REPORT_LEN;
use crate::{Error, Result};

/// Corsair.
pub const VENDOR_ID: u16 = 0x1B1C;
/// K95 RGB, the original 2014 model with 18 G keys.
pub const PRODUCT_ID: u16 = 0x1B11;

/// The vendor collection that carries G key state.
///
/// Measured in Phase 0. The device exposes three vendor collections; this is
/// the one that reports, and crucially it is input only, so it is not the
/// channel any lighting software writes to.
pub const G_KEY_USAGE_PAGE: u16 = 0xFFC0;
pub const G_KEY_USAGE: u16 = 0x0002;

/// One HID top level collection.
#[derive(Clone, Debug)]
pub struct Collection {
    pub path: String,
    pub product: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub input_len: usize,
    pub output_len: usize,
    pub feature_len: usize,
}

impl Collection {
    /// Is this the collection that carries G key state?
    pub fn is_g_key_pipe(&self) -> bool {
        self.usage_page == G_KEY_USAGE_PAGE
            && self.usage == G_KEY_USAGE
            && self.input_len >= MIN_REPORT_LEN
    }
}

fn wide_from_bytes(buf: &[u8], offset: usize) -> String {
    let mut units = Vec::new();
    let mut i = offset;
    while i + 1 < buf.len() {
        let u = u16::from_le_bytes([buf[i], buf[i + 1]]);
        if u == 0 {
            break;
        }
        units.push(u);
        i += 2;
    }
    String::from_utf16_lossy(&units)
}

/// Enumerate every HID collection belonging to the keyboard.
///
/// Each candidate is opened with `dwDesiredAccess = 0`, which queries metadata
/// without requesting read or write access, so enumeration cannot disturb any
/// other process that has the device open.
pub fn enumerate() -> Vec<Collection> {
    let mut found = Vec::new();

    unsafe {
        let mut guid = Guid::default();
        HidD_GetHidGuid(&mut guid);

        let set =
            SetupDiGetClassDevsW(&guid, ptr::null(), 0, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
        if set == INVALID_HANDLE_VALUE {
            return found;
        }

        let mut index = 0u32;
        loop {
            let mut data = SpDeviceInterfaceData {
                cb_size: core::mem::size_of::<SpDeviceInterfaceData>() as u32,
                interface_class_guid: Guid::default(),
                flags: 0,
                reserved: 0,
            };
            if SetupDiEnumDeviceInterfaces(set, ptr::null_mut(), &guid, index, &mut data) == 0 {
                break;
            }
            index += 1;

            let mut buf = vec![0u8; 2048];
            // SP_DEVICE_INTERFACE_DETAIL_DATA_W.cbSize is 8 on 64 bit and 6 on
            // 32 bit. DevicePath always starts at byte offset 4 either way.
            let cb: u32 = if cfg!(target_pointer_width = "64") {
                8
            } else {
                6
            };
            buf[0..4].copy_from_slice(&cb.to_le_bytes());

            let mut required = 0u32;
            if SetupDiGetDeviceInterfaceDetailW(
                set,
                &mut data,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut required,
                ptr::null_mut(),
            ) == 0
            {
                continue;
            }

            let path = wide_from_bytes(&buf, 4);
            if path.is_empty() {
                continue;
            }

            let handle = CreateFileW(
                wide(&path).as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                0,
                0,
            );
            if handle == INVALID_HANDLE_VALUE {
                continue;
            }

            let mut attrs = HiddAttributes {
                size: core::mem::size_of::<HiddAttributes>() as u32,
                ..Default::default()
            };
            let matched = HidD_GetAttributes(handle, &mut attrs) != 0
                && attrs.vendor_id == VENDOR_ID
                && attrs.product_id == PRODUCT_ID;

            if matched {
                let mut name = vec![0u16; 256];
                let product = if HidD_GetProductString(handle, name.as_mut_ptr(), 512) != 0 {
                    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
                    String::from_utf16_lossy(&name[..end]).trim().to_string()
                } else {
                    String::new()
                };

                let mut preparsed: isize = 0;
                if HidD_GetPreparsedData(handle, &mut preparsed) != 0 {
                    let mut caps = HidpCaps::default();
                    if HidP_GetCaps(preparsed, &mut caps) == HIDP_STATUS_SUCCESS {
                        found.push(Collection {
                            path: path.clone(),
                            product,
                            vendor_id: attrs.vendor_id,
                            product_id: attrs.product_id,
                            usage_page: caps.usage_page,
                            usage: caps.usage,
                            input_len: caps.input_report_byte_length as usize,
                            output_len: caps.output_report_byte_length as usize,
                            feature_len: caps.feature_report_byte_length as usize,
                        });
                    }
                    HidD_FreePreparsedData(preparsed);
                }
            }

            CloseHandle(handle);
        }

        SetupDiDestroyDeviceInfoList(set);
    }

    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

/// The G key collection, if the keyboard is attached.
pub fn find_g_key_pipe() -> Option<Collection> {
    enumerate().into_iter().find(|c| c.is_g_key_pipe())
}

/// Open a collection for reading.
///
/// `GENERIC_READ` only, never `GENERIC_WRITE`, and shared for both read and
/// write so that other software using this keyboard is never locked out.
pub fn open_for_read(collection: &Collection) -> Result<isize> {
    let handle = unsafe {
        CreateFileW(
            wide(&collection.path).as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            0,
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        return Err(Error::Win32 {
            what: "CreateFileW",
            code: unsafe { GetLastError() },
        });
    }
    Ok(handle)
}
