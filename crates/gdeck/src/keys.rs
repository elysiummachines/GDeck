//! The 18 G keys and how to read them out of a vendor HID report.
//!
//! Every constant here was measured on the hardware during Phase 0 and then
//! independently replicated in a second run. See docs/PHASE0-FINDINGS.md.

use core::fmt;

/// Number of G keys on the K95 RGB.
pub const G_KEY_COUNT: usize = 18;

/// HID report ID carried by every report on the G key pipe.
pub const REPORT_ID: u8 = 0x03;

/// Highest byte index we read, plus one. Reports shorter than this are ignored.
pub const MIN_REPORT_LEN: usize = 19;

/// `(byte offset, bit index)` for G1 through G18, in order.
///
/// Captured 2026-11-11 on `0xFFC0/0x0002`. All 18 pairs are distinct.
///
/// Byte 17 bits 2 to 7 are deliberately absent. Six bits are unaccounted for
/// there, and the K95 has exactly six other software visible special keys
/// (MR, M1, M2, M3, brightness, Windows lock), so those bits are almost
/// certainly those keys. Reading them as G keys would make the brightness key
/// fire a user action, which is why decoding masks to this table rather than
/// treating any set bit in bytes 16 to 18 as a G key.
pub const G_KEY_BITS: [(usize, u8); G_KEY_COUNT] = [
    (16, 0), // G1
    (16, 1), // G2
    (16, 2), // G3
    (16, 3), // G4
    (16, 4), // G5
    (16, 5), // G6
    (16, 6), // G7
    (16, 7), // G8
    (17, 0), // G9
    (17, 1), // G10
    (18, 0), // G11
    (18, 1), // G12
    (18, 2), // G13
    (18, 3), // G14
    (18, 4), // G15
    (18, 5), // G16
    (18, 6), // G17
    (18, 7), // G18
];

/// One of the 18 G keys. Always in range, by construction.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct GKey(u8);

impl GKey {
    /// Build from a zero based index. Returns `None` outside 0..18.
    pub fn from_index(index: usize) -> Option<GKey> {
        if index < G_KEY_COUNT {
            Some(GKey(index as u8))
        } else {
            None
        }
    }

    /// Zero based index, 0 for G1.
    pub fn index(self) -> usize {
        self.0 as usize
    }

    /// Human number, 1 for G1.
    pub fn number(self) -> u8 {
        self.0 + 1
    }

    /// All 18 keys in order.
    pub fn all() -> impl Iterator<Item = GKey> {
        (0..G_KEY_COUNT).map(|i| GKey(i as u8))
    }
}

impl fmt::Display for GKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "G{}", self.number())
    }
}

/// Packed state of all 18 keys. Bit `n` is set when `GKey::from_index(n)` is held.
pub type KeyState = u32;

/// Decode a raw HID report into the packed state of the 18 G keys.
///
/// Returns `None` for anything that is not a well formed report on this pipe,
/// so a short read or an unexpected report ID can never be mistaken for all
/// keys being released.
pub fn decode_state(report: &[u8]) -> Option<KeyState> {
    if report.len() < MIN_REPORT_LEN || report[0] != REPORT_ID {
        return None;
    }

    let mut state: KeyState = 0;
    for (index, &(byte, bit)) in G_KEY_BITS.iter().enumerate() {
        if report[byte] & (1u8 << bit) != 0 {
            state |= 1 << index;
        }
    }
    Some(state)
}

/// Whether a given key is held in a packed state.
pub fn is_held(state: KeyState, key: GKey) -> bool {
    state & (1 << key.index()) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_report() -> [u8; 64] {
        let mut r = [0u8; 64];
        r[0] = REPORT_ID;
        r
    }

    #[test]
    fn bit_table_has_no_duplicates() {
        for (i, first) in G_KEY_BITS.iter().enumerate() {
            for (j, second) in G_KEY_BITS.iter().enumerate().skip(i + 1) {
                assert_ne!(first, second, "G{} and G{} share a bit", i + 1, j + 1);
            }
        }
    }

    #[test]
    fn every_bit_is_within_a_valid_report() {
        for &(byte, bit) in G_KEY_BITS.iter() {
            assert!(byte < MIN_REPORT_LEN, "byte {} is outside the validated length", byte);
            assert!(bit < 8, "bit {} is not a bit index", bit);
        }
    }

    #[test]
    fn idle_report_decodes_to_nothing_held() {
        assert_eq!(decode_state(&empty_report()), Some(0));
    }

    #[test]
    fn each_key_decodes_to_exactly_itself() {
        for key in GKey::all() {
            let (byte, bit) = G_KEY_BITS[key.index()];
            let mut r = empty_report();
            r[byte] = 1u8 << bit;

            let state = decode_state(&r).expect("valid report");
            assert_eq!(state.count_ones(), 1, "{} set more than one key", key);
            assert!(is_held(state, key), "{} did not decode to itself", key);
        }
    }

    #[test]
    fn combinations_decode_together() {
        // Byte 16 = 0x07 was observed on the wire with G1, G2 and G3 held.
        let mut r = empty_report();
        r[16] = 0x07;
        let state = decode_state(&r).expect("valid report");
        assert_eq!(state.count_ones(), 3);
        for n in 0..3 {
            assert!(is_held(state, GKey::from_index(n).unwrap()));
        }
    }

    #[test]
    fn unmapped_bits_are_ignored() {
        // Byte 17 bits 2 to 7 are probably the M and brightness keys. They must
        // never register as a G key.
        for bit in 2..8u8 {
            let mut r = empty_report();
            r[17] = 1u8 << bit;
            assert_eq!(
                decode_state(&r),
                Some(0),
                "byte 17 bit {} leaked through as a G key",
                bit
            );
        }
    }

    #[test]
    fn malformed_reports_are_rejected_not_read_as_empty() {
        let mut wrong_id = empty_report();
        wrong_id[0] = 0x02;
        assert_eq!(decode_state(&wrong_id), None);

        assert_eq!(decode_state(&[]), None);
        assert_eq!(decode_state(&[REPORT_ID; 4]), None);
    }
}
