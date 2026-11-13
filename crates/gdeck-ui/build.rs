//! Compiles the application icon into the executable.
//!
//! Windows reads an executable's icon out of its resource section. Without
//! one, Explorer, the taskbar and Alt Tab all fall back to the generic
//! document icon, no matter what the running window does.
//!
//! The usual way to get a resource section is `rc.exe` from the Windows SDK,
//! or a crate that shells out to it. Neither is needed here: the `.res`
//! container is a short, documented format, so this writes one directly and
//! hands it to the linker, which accepts `.res` files as input. The build
//! stays a plain `cargo build`, nothing is added to the dependency tree, and
//! the artwork stays readable Rust rather than a checked in binary.
//!
//! Format reference: the RESOURCEHEADER layout, plus ICONDIR, GRPICONDIR and
//! the BITMAPINFOHEADER based icon image, all as documented by Microsoft.

#[path = "src/iconart.rs"]
mod iconart;

use std::env;
use std::fs;
use std::path::PathBuf;

/// Windows picks the closest of these and scales. 256 is what the largest
/// Explorer view wants; 16 is the taskbar and the title bar.
const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;

/// Legacy loader hints. Win32 ignores them, but real `.res` files carry them
/// and some tooling still reads them back.
const MOVEABLE_DISCARDABLE: u16 = 0x1010;
const MOVEABLE_PURE_DISCARDABLE: u16 = 0x1030;

const LANG_EN_US: u16 = 0x0409;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/iconart.rs");

    // Nothing to do anywhere else, and the `.res` input is MSVC specific.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let path = out_dir.join("gdeck-icon.res");
    fs::write(&path, resource_file()).expect("write the icon resource");
    println!("cargo:rustc-link-arg-bins={}", path.display());
}

fn resource_file() -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = SIZES.iter().map(|&s| (s, icon_image(s))).collect();

    let mut file = Vec::new();
    // Every .res begins with an empty entry whose type and name are both
    // ordinal zero. It is a signature, not a resource.
    entry(&mut file, 0, 0, 0, 0, &[]);
    for (index, (_, data)) in images.iter().enumerate() {
        let id = (index + 1) as u16;
        entry(&mut file, RT_ICON, id, MOVEABLE_DISCARDABLE, LANG_EN_US, data);
    }
    entry(
        &mut file,
        RT_GROUP_ICON,
        1,
        MOVEABLE_PURE_DISCARDABLE,
        LANG_EN_US,
        &group_directory(&images),
    );
    file
}

/// One RESOURCEHEADER followed by its data, padded to a four byte boundary.
///
/// Both the type and the name are written as ordinals, which is the
/// `0xFFFF` marker followed by the number. That keeps the header a fixed 32
/// bytes and avoids the alignment padding a string name would need.
fn entry(out: &mut Vec<u8>, type_id: u16, name_id: u16, flags: u16, language: u16, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // DataSize
    out.extend_from_slice(&32u32.to_le_bytes()); // HeaderSize
    out.extend_from_slice(&0xFFFFu16.to_le_bytes()); // type is an ordinal
    out.extend_from_slice(&type_id.to_le_bytes());
    out.extend_from_slice(&0xFFFFu16.to_le_bytes()); // name is an ordinal
    out.extend_from_slice(&name_id.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    out.extend_from_slice(&flags.to_le_bytes()); // MemoryFlags
    out.extend_from_slice(&language.to_le_bytes()); // LanguageId
    out.extend_from_slice(&0u32.to_le_bytes()); // Version
    out.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
    out.extend_from_slice(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

/// One RT_ICON: a BITMAPINFOHEADER, the pixels bottom up as BGRA, then the
/// AND mask.
fn icon_image(size: u32) -> Vec<u8> {
    let art = iconart::rgba(size);
    let mut out = Vec::new();

    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(size as i32).to_le_bytes()); // biWidth
    // Doubled, because the header describes the colour image and the mask
    // together even when the mask carries nothing.
    out.extend_from_slice(&((size * 2) as i32).to_le_bytes()); // biHeight
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            out.push(art[i + 2]); // B
            out.push(art[i + 1]); // G
            out.push(art[i]); // R
            out.push(art[i + 3]); // A
        }
    }

    // The alpha channel above does the masking, so every mask bit stays zero,
    // meaning "take this pixel from the colour image". The rows are still
    // padded to four bytes, as a one bit per pixel bitmap must be.
    let row = (size.div_ceil(32) * 4) as usize;
    out.extend(std::iter::repeat(0u8).take(row * size as usize));
    out
}

/// The RT_GROUP_ICON directory, which is what Windows actually looks up. It
/// lists the sizes and points at each RT_ICON by id.
fn group_directory(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());

    for (index, (size, data)) in images.iter().enumerate() {
        // A 256 pixel icon is recorded as zero, since the field is one byte.
        let measure = if *size >= 256 { 0u8 } else { *size as u8 };
        out.push(measure); // width
        out.push(measure); // height
        out.push(0); // colours in the palette, zero above 8 bits
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bit count
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&((index + 1) as u16).to_le_bytes()); // RT_ICON id
    }
    out
}
