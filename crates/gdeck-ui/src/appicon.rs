//! Pulling the real icon out of a program, folder or document.
//!
//! Uses `SHGetFileInfoW`, which is the shell's own answer to "what icon does
//! this thing have", so it handles executables, folders, shortcuts and files
//! whose icon comes from their association, rather than only reading icon
//! resources out of a PE.
//!
//! Read only: it asks the shell for an icon and converts it to pixels.

#![allow(non_snake_case)]

use core::ffi::c_void;

const MAX_PATH: usize = 260;

const SHGFI_ICON: u32 = 0x0000_0100;
const SHGFI_LARGEICON: u32 = 0x0000_0000;

const DIB_RGB_COLORS: u32 = 0;
const BI_RGB: u32 = 0;

#[repr(C)]
struct ShFileInfoW {
    icon: isize,
    icon_index: i32,
    attributes: u32,
    display_name: [u16; MAX_PATH],
    type_name: [u16; 80],
}

#[repr(C)]
struct IconInfo {
    is_icon: i32,
    x_hotspot: u32,
    y_hotspot: u32,
    mask_bitmap: isize,
    color_bitmap: isize,
}

#[repr(C)]
struct Bitmap {
    kind: i32,
    width: i32,
    height: i32,
    width_bytes: i32,
    planes: u16,
    bits_pixel: u16,
    bits: *mut c_void,
}

#[repr(C)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_pixels_per_meter: i32,
    y_pixels_per_meter: i32,
    colours_used: u32,
    colours_important: u32,
}

#[link(name = "shell32")]
extern "system" {
    fn SHGetFileInfoW(
        path: *const u16,
        file_attributes: u32,
        info: *mut ShFileInfoW,
        info_size: u32,
        flags: u32,
    ) -> usize;
}

#[link(name = "user32")]
extern "system" {
    fn DestroyIcon(icon: isize) -> i32;
    fn GetIconInfo(icon: isize, info: *mut IconInfo) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn GetObjectW(handle: isize, size: i32, object: *mut c_void) -> i32;
    fn CreateCompatibleDC(dc: isize) -> isize;
    fn DeleteDC(dc: isize) -> i32;
    fn DeleteObject(object: isize) -> i32;
    fn GetDIBits(
        dc: isize,
        bitmap: isize,
        start: u32,
        lines: u32,
        bits: *mut c_void,
        info: *mut c_void,
        usage: u32,
    ) -> i32;
}

/// A decoded icon, ready to hand to the renderer.
pub struct IconPixels {
    pub width: usize,
    pub height: usize,
    /// Straight (not premultiplied) RGBA.
    pub rgba: Vec<u8>,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// The icon the shell would show for this path, if there is one.
pub fn load(path: &str) -> Option<IconPixels> {
    unsafe {
        let mut info: ShFileInfoW = core::mem::zeroed();
        let got = SHGetFileInfoW(
            wide(path).as_ptr(),
            0,
            &mut info,
            core::mem::size_of::<ShFileInfoW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if got == 0 || info.icon == 0 {
            return None;
        }

        let pixels = to_rgba(info.icon);
        DestroyIcon(info.icon);
        pixels
    }
}

unsafe fn to_rgba(icon: isize) -> Option<IconPixels> {
    let mut icon_info: IconInfo = core::mem::zeroed();
    if GetIconInfo(icon, &mut icon_info) == 0 {
        return None;
    }

    // Both bitmaps belong to us now and must be released on every path out.
    let colour = icon_info.color_bitmap;
    let mask = icon_info.mask_bitmap;

    let result = (|| {
        if colour == 0 {
            return None;
        }

        let mut bitmap: Bitmap = core::mem::zeroed();
        if GetObjectW(
            colour,
            core::mem::size_of::<Bitmap>() as i32,
            &mut bitmap as *mut Bitmap as *mut c_void,
        ) == 0
        {
            return None;
        }

        let width = bitmap.width.max(0) as usize;
        let height = bitmap.height.max(0) as usize;
        if width == 0 || height == 0 || width > 512 || height > 512 {
            return None;
        }

        let mut header = BitmapInfoHeader {
            size: core::mem::size_of::<BitmapInfoHeader>() as u32,
            width: bitmap.width,
            // Negative height asks for top down rows, which is the order the
            // renderer wants. Otherwise the icon arrives upside down.
            height: -bitmap.height,
            planes: 1,
            bit_count: 32,
            compression: BI_RGB,
            size_image: 0,
            x_pixels_per_meter: 0,
            y_pixels_per_meter: 0,
            colours_used: 0,
            colours_important: 0,
        };

        let mut buffer = vec![0u8; width * height * 4];
        let dc = CreateCompatibleDC(0);
        if dc == 0 {
            return None;
        }
        let lines = GetDIBits(
            dc,
            colour,
            0,
            height as u32,
            buffer.as_mut_ptr() as *mut c_void,
            &mut header as *mut BitmapInfoHeader as *mut c_void,
            DIB_RGB_COLORS,
        );
        DeleteDC(dc);

        if lines == 0 {
            return None;
        }

        // Windows hands back BGRA.
        for pixel in buffer.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }

        // Older icons carry no alpha at all, which would render as nothing.
        // If every pixel is fully transparent, treat the image as opaque.
        if buffer.chunks_exact(4).all(|pixel| pixel[3] == 0) {
            for pixel in buffer.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
        }

        Some(IconPixels {
            width,
            height,
            rgba: buffer,
        })
    })();

    if colour != 0 {
        DeleteObject(colour);
    }
    if mask != 0 {
        DeleteObject(mask);
    }

    result
}
