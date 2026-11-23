// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: Copyright (C) 2026  eosed contributors
//
// Rust port of eos/lcd.py — decoding the E4XT's LCD out of a 50h panel frame.
// The display encoding is this project's own reverse engineering
// (docs/RESOLUTION_NOTES.md §26, §32).

//! **The encoding is a plain bitstream, seven bits per byte, MSB first.**
//! Not MIDI's usual 7->8 packing — that first reading was wrong in a way
//! that looked right: 2195 septets x 7/8 truncates to exactly 1920 bytes,
//! which is 240x64/8, so the arithmetic appeared to confirm itself while the
//! image sheared across the screen. The bitstream needs no coincidence:
//! 2195 x 7 = 15365 bits for 15360 pixels, and the picture comes out square.
//! A decoding that produces the right size is not thereby the right decoding.

pub const WIDTH: usize = 240;
pub const HEIGHT: usize = 64;

/// Bytes 0-5 are the frame header (`F0 18 7F <a> <b> 50`); bytes 6-15 are a
/// sub-header constant across every frame seen; the bitstream follows.
const SUBHEADER: usize = 10;
const HEADER: usize = 6;

pub type Bitmap = Vec<Vec<u8>>;

/// A 64x240 bitmap of 0/1 from a 50h frame, or None if it is not one.
/// Rows top to bottom, each WIDTH bits, 1 = lit.
pub fn decode_display(frame: &[u8]) -> Option<Bitmap> {
    if frame.len() < HEADER + SUBHEADER + 2 {
        return None;
    }
    if frame[0..3] != [0xF0u8, 0x18, 0x7F] || frame[5] != 0x50 {
        return None;
    }
    let payload = &frame[HEADER + SUBHEADER..frame.len() - 1];

    let needed = WIDTH * HEIGHT;
    let mut bits: Vec<u8> = Vec::with_capacity(needed);
    'outer: for value in payload {
        for shift in (0..7).rev() {
            bits.push((value >> shift) & 1);
            if bits.len() >= needed {
                break 'outer;
            }
        }
    }
    if bits.len() < needed {
        return None;
    }
    Some(
        (0..HEIGHT)
            .map(|y| bits[y * WIDTH..(y + 1) * WIDTH].to_vec())
            .collect(),
    )
}

/// True for a 50h frame too short to be a whole screen. Partial/region
/// updates exist (§26 saw 86- and 112-byte frames) and their region encoding
/// is unknown, so callers should keep showing the last full screen.
pub fn is_partial(frame: &[u8]) -> bool {
    if frame.len() < 7 || frame[5] != 0x50 {
        return false;
    }
    let payload = frame.len() - HEADER - SUBHEADER - 1;
    payload * 7 < WIDTH * HEIGHT
}

/// Braille dot bit order within a 2x4 cell: (x, y) -> bit in U+2800.
const BRAILLE_BITS: [(u16, u16); 4] = [(0x01, 0x08), (0x02, 0x10), (0x04, 0x20), (0x40, 0x80)];

/// The screen as Unicode braille — 240x64 becomes 120x16 characters; the
/// cheap portable way to put real pixels in a text grid.
#[allow(clippy::needless_range_loop)] // the index arithmetic IS the pixel geometry
pub fn to_braille(bitmap: &Bitmap, invert: bool) -> Vec<String> {
    let mut lines = Vec::new();
    for cy in (0..HEIGHT).step_by(4) {
        let mut row = String::new();
        for cx in (0..WIDTH).step_by(2) {
            let mut dots: u16 = 0;
            for dy in 0..4 {
                for dx in 0..2 {
                    let (y, x) = (cy + dy, cx + dx);
                    if y >= HEIGHT || x >= WIDTH {
                        continue;
                    }
                    let mut lit = bitmap[y][x];
                    if invert {
                        lit = 1 - lit;
                    }
                    if lit == 1 {
                        dots |= if dx == 0 {
                            BRAILLE_BITS[dy].0
                        } else {
                            BRAILLE_BITS[dy].1
                        };
                    }
                }
            }
            row.push(char::from_u32(0x2800 + dots as u32).unwrap());
        }
        lines.push(row);
    }
    lines
}

/// Half-block rendering: two vertical pixels per character cell.
#[allow(clippy::needless_range_loop)] // the index arithmetic IS the pixel geometry
pub fn to_halfblocks(bitmap: &Bitmap) -> Vec<String> {
    const GLYPHS: [char; 4] = [' ', '▄', '▀', '█'];
    let mut lines = Vec::new();
    for cy in (0..HEIGHT).step_by(2) {
        let mut row = String::new();
        for x in 0..WIDTH {
            let top = bitmap[cy][x];
            let bottom = if cy + 1 < HEIGHT {
                bitmap[cy + 1][x]
            } else {
                0
            };
            row.push(GLYPHS[((top as usize) << 1) | bottom as usize]);
        }
        lines.push(row);
    }
    lines
}

/// One character per pixel. For tests and diffs, not for a UI.
pub fn to_ascii(bitmap: &Bitmap) -> Vec<String> {
    bitmap
        .iter()
        .map(|row| {
            row.iter()
                .map(|b| if *b == 1 { '#' } else { '.' })
                .collect()
        })
        .collect()
}

// --- refresh policy (§33b) ------------------------------------------------

/// A 52h reply this small means "nothing changed" — measured on a quiet
/// screen (§33a). Anything larger carries something.
pub const EMPTY_UPDATE_MAX: usize = 100;

/// Below this a 50h frame cannot be a whole screen. Derived, not guessed:
/// a full screen needs WIDTH*HEIGHT bits at 7 bits per byte, plus header.
pub const FULL_MIN_BYTES: usize = (WIDTH * HEIGHT).div_ceil(7) + HEADER + SUBHEADER;

pub enum RefreshDecision {
    Idle,     // nothing changed; do not repaint
    Use,      // decodable full screen arrived; show it
    Escalate, // changed but not decodable; ask 51h
}

/// Decide from a 52h reply alone, without decoding it. Polling with 52h
/// costs 70ms against 716ms (§33b); that saving only survives if the
/// repaint decision comes from the reply's *shape*, before paying decode.
pub fn classify_update(frame: Option<&[u8]>) -> RefreshDecision {
    let frame = match frame {
        Some(f) if !f.is_empty() => f,
        _ => return RefreshDecision::Idle,
    };
    if frame.len() < 7 || frame[5] != 0x50 {
        return RefreshDecision::Idle;
    }
    if frame.len() <= EMPTY_UPDATE_MAX {
        return RefreshDecision::Idle;
    }
    if frame.len() >= FULL_MIN_BYTES {
        return RefreshDecision::Use;
    }
    RefreshDecision::Escalate
}

/// Quadrant blocks: 2x2 pixels per cell, indexed by (tl,tr,bl,br) nibble.
const QUADRANTS: [char; 16] = [
    ' ', '▗', '▖', '▄', '▝', '▐', '▞', '▟', '▘', '▚', '▌', '▙', '▀', '▜', '▛', '█',
];

/// The screen as quadrant blocks — 120x32 characters. The default renderer:
/// braille is denser than the device font survives, half-blocks need 240
/// columns.
pub fn to_quadrants(bitmap: &Bitmap) -> Vec<String> {
    let mut lines = Vec::new();
    for cy in (0..HEIGHT).step_by(2) {
        let mut row = String::new();
        for cx in (0..WIDTH).step_by(2) {
            let tl = bitmap[cy][cx] as usize;
            let tr = if cx + 1 < WIDTH {
                bitmap[cy][cx + 1] as usize
            } else {
                0
            };
            let bl = if cy + 1 < HEIGHT {
                bitmap[cy + 1][cx] as usize
            } else {
                0
            };
            let br = if cy + 1 < HEIGHT && cx + 1 < WIDTH {
                bitmap[cy + 1][cx + 1] as usize
            } else {
                0
            };
            row.push(QUADRANTS[(tl << 3) | (tr << 2) | (bl << 1) | br]);
        }
        lines.push(row);
    }
    lines
}

/// Where the six soft keys sit, as a fraction of display width: physically
/// at sixths, which is the alignment right on every page.
pub const SOFT_KEY_CENTRES: [f64; 6] = [
    1.0 / 12.0,
    3.0 / 12.0,
    5.0 / 12.0,
    7.0 / 12.0,
    9.0 / 12.0,
    11.0 / 12.0,
];

/// Column of each soft key's centre, for a render of `render_width`.
pub fn soft_key_columns(render_width: usize) -> Vec<usize> {
    SOFT_KEY_CENTRES
        .iter()
        .map(|f| (f * render_width as f64).round() as usize)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_frame(payload_byte: u8) -> Vec<u8> {
        let mut f = vec![0xF0, 0x18, 0x7F, 0x00, 0x00, 0x50];
        f.extend(std::iter::repeat_n(0x01, 10)); // sub-header
                                                 // 2195 septets of payload, each `payload_byte`
        f.extend(std::iter::repeat_n(payload_byte, 2195));
        f.push(0xF7);
        f
    }

    #[test]
    fn full_screen_decodes_square() {
        let b = decode_display(&full_frame(0x55)).expect("full frame must decode");
        assert_eq!(b.len(), HEIGHT);
        assert!(b.iter().all(|r| r.len() == WIDTH));
        // 0x55 = 0b1010101 septet: MSB-first bits 6..0
        assert_eq!(&b[0][0..7], &[1u8, 0, 1, 0, 1, 0, 1]);
    }

    #[test]
    fn short_frames_rejected() {
        assert!(decode_display(&[0xF0, 0x18, 0x7F, 0, 0, 0x50, 0, 0]).is_none());
        let mut f = full_frame(0);
        f.truncate(200); // partial, §26's 86/112-byte shape
        assert!(decode_display(&f).is_none());
        assert!(is_partial(&f));
    }

    #[test]
    fn refresh_policy() {
        assert!(matches!(classify_update(None), RefreshDecision::Idle));
        let mut small = full_frame(0);
        small.truncate(86);
        assert!(matches!(
            classify_update(Some(&small)),
            RefreshDecision::Idle
        ));
        assert!(matches!(
            classify_update(Some(&full_frame(0))),
            RefreshDecision::Use
        ));
        let mut mid = full_frame(0);
        mid.truncate((EMPTY_UPDATE_MAX + FULL_MIN_BYTES) / 2);
        assert!(matches!(
            classify_update(Some(&mid)),
            RefreshDecision::Escalate
        ));
    }

    #[test]
    fn renderers_agree_on_shape() {
        let b = decode_display(&full_frame(0xFF)).unwrap();
        assert_eq!(to_braille(&b, false).len(), 16);
        assert_eq!(to_braille(&b, false)[0].chars().count(), 120);
        assert_eq!(to_halfblocks(&b).len(), 32);
        assert_eq!(to_halfblocks(&b)[0].chars().count(), 240);
        assert_eq!(to_quadrants(&b).len(), 32);
        assert_eq!(to_quadrants(&b)[0].chars().count(), 120);
        assert_eq!(to_ascii(&b)[0].chars().count(), 240);
        // all-lit screen: every quadrant cell fully solid
        assert!(to_quadrants(&b).iter().all(|l| l.chars().all(|c| c == '█')));
    }

    #[test]
    fn soft_key_alignment() {
        // sixths: centres at odd twelfths -> 10,30,...,110 for 120 columns
        assert_eq!(soft_key_columns(120), vec![10, 30, 50, 70, 90, 110]);
    }
}
