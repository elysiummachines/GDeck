//! The G Deck mark, drawn in code at whatever size is asked for.
//!
//! One definition used in three places: the icon `build.rs` compiles into the
//! executable, the window icon set at startup, and the notification area icon.
//! Drawing it instead of shipping a `.ico` keeps the artwork reviewable as
//! source and keeps the three from drifting apart.
//!
//! No dependencies, and nothing here touches the operating system.

/// Supersampling factor. Every output pixel averages this many samples per
/// axis, which is what keeps the 16 pixel icon from turning to mush.
const SAMPLES: u32 = 4;

/// Outer tile.
const TILE_INSET: f32 = 0.03;
const TILE_RADIUS: f32 = 0.22;

/// The two by two block of keys inside it.
const KEY_ORIGIN: f32 = 0.22;
const KEY_SIZE: f32 = 0.24;
const KEY_GAP: f32 = 0.08;
const KEY_RADIUS: f32 = 0.06;

/// Top and bottom of the tile's vertical gradient, and the key colour.
const TOP: [f32; 3] = [184.0, 107.0, 255.0];
const BOTTOM: [f32; 3] = [124.0, 58.0, 237.0];
const KEY: [f32; 3] = [243.0, 232.0, 255.0];

/// Signed distance to a rounded rectangle, negative inside.
fn rounded(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, radius: f32) -> f32 {
    let half_x = (x1 - x0) * 0.5;
    let half_y = (y1 - y0) * 0.5;
    let dx = (px - (x0 + x1) * 0.5).abs() - (half_x - radius);
    let dy = (py - (y0 + y1) * 0.5).abs() - (half_y - radius);
    let outside = (dx.max(0.0) * dx.max(0.0) + dy.max(0.0) * dy.max(0.0)).sqrt();
    let inside = dx.max(dy).min(0.0);
    outside + inside - radius
}

fn in_key(px: f32, py: f32) -> bool {
    for row in 0..2 {
        for column in 0..2 {
            let x0 = KEY_ORIGIN + column as f32 * (KEY_SIZE + KEY_GAP);
            let y0 = KEY_ORIGIN + row as f32 * (KEY_SIZE + KEY_GAP);
            if rounded(px, py, x0, y0, x0 + KEY_SIZE, y0 + KEY_SIZE, KEY_RADIUS) <= 0.0 {
                return true;
            }
        }
    }
    false
}

/// The mark as straight (not premultiplied) RGBA, `size` by `size`, top down.
pub fn rgba(size: u32) -> Vec<u8> {
    let mut out = vec![0u8; (size * size * 4) as usize];
    let total = (SAMPLES * SAMPLES) as f32;
    let step = 1.0 / SAMPLES as f32;

    for y in 0..size {
        for x in 0..size {
            let mut tile_hits = 0.0f32;
            let mut key_hits = 0.0f32;

            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let px = (x as f32 + (sx as f32 + 0.5) * step) / size as f32;
                    let py = (y as f32 + (sy as f32 + 0.5) * step) / size as f32;
                    let inside_tile = rounded(
                        px,
                        py,
                        TILE_INSET,
                        TILE_INSET,
                        1.0 - TILE_INSET,
                        1.0 - TILE_INSET,
                        TILE_RADIUS,
                    ) <= 0.0;
                    if inside_tile {
                        tile_hits += 1.0;
                        if in_key(px, py) {
                            key_hits += 1.0;
                        }
                    }
                }
            }

            let alpha = tile_hits / total;
            let offset = ((y * size + x) * 4) as usize;
            if alpha <= 0.0 {
                continue;
            }

            // Vertical gradient, sampled at the pixel centre.
            let t = (y as f32 + 0.5) / size as f32;
            let key_share = key_hits / tile_hits;
            for channel in 0..3 {
                let base = TOP[channel] + (BOTTOM[channel] - TOP[channel]) * t;
                let mixed = base * (1.0 - key_share) + KEY[channel] * key_share;
                out[offset + channel] = mixed.round().clamp(0.0, 255.0) as u8;
            }
            out[offset + 3] = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_are_transparent_and_centre_is_not() {
        let art = rgba(32);
        let at = |x: u32, y: u32| art[((y * 32 + x) * 4 + 3) as usize];
        assert_eq!(at(0, 0), 0, "the tile is rounded, so the corner is empty");
        assert_eq!(at(31, 31), 0);
        assert_eq!(at(16, 16), 255, "the middle of the tile is solid");
    }

    #[test]
    fn every_size_is_fully_populated() {
        for size in [16u32, 24, 32, 48, 64, 128, 256] {
            assert_eq!(rgba(size).len(), (size * size * 4) as usize);
        }
    }
}
