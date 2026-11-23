// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: Copyright (C) 2026  eosed contributors
//
// Rust port of eos/panel.py — the panel/remote protocol:
// `F0 18 7F <devID> 7A <cmd> ... F7`.
//
// Deliberately a separate module from `messages` (the *editor* protocol,
// `F0 18 21 <devID> 55 ...`): one is fully specified by E-mu, this one is
// undocumented and every byte in it was either captured here or is prior art.
// Keeping them apart prevents a false sense of authority by proximity.
//
// **Nothing in this module is guessed.** Every frame corresponds to traffic
// observed on the wire (§28 open, §29/§30 buttons and dial). There is no
// close message because none was ever captured.

use thiserror::Error;

pub const SOX: u8 = 0xF0;
pub const EOX: u8 = 0xF7;
pub const MANUFACTURER_ID: u8 = 0x18;
pub const PANEL_ID: u8 = 0x7F; // where the editor protocol has 21h
pub const PANEL_DESIGNATOR: u8 = 0x7A; // where the editor protocol has 55h

#[derive(Error, Debug)]
pub enum PanelError {
    #[error("device id out of range: {0}")]
    DeviceId(u8),
    #[error("payload byte not 7-bit: {0}")]
    PayloadByte(u8),
    #[error("dial delta out of 14-bit range: {0}")]
    Delta(i32),
}

/// Opcode byte, position 5. Names describe observed behaviour only.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelCommand {
    Open = 0x10,           // host->device, session open (§28)
    Button = 0x40,         // 40 <key> 00 <01=down|00=up>  (§29, §30)
    Dial = 0x43,           // 43 01 <lo> <hi>, signed 14-bit delta (§30)
    Display = 0x50,        // device->host, 240x64 bitmap (§26, §32)
    DisplayRequest = 0x51, // host->device, "send me the screen" (§33)
    DisplayUpdate = 0x52,  // host->device, delta request; 86 bytes = nothing new (§33a)
    ScreenQuery = 0x60,    // host->device, provokes a 61h reply (§33)
    ScreenState = 0x61,    // device->host, two payload bytes, meaning unknown
}

/// Front-panel key codes, captured 2026-08-15 (§30). 32 observed directly;
/// the number keys marked inferred were placed after both ends of the run and
/// a midpoint were tested — which is how the +/- key between 9 and 0 was
/// caught, and why ZERO is 7Eh rather than the 7Dh a naive sequence expects.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    PresetManage = 0x58,
    SampleManage = 0x59,
    PresetEdit = 0x5A,
    SampleEdit = 0x5B,
    Master = 0x5C,
    DiskBrowse = 0x5D,
    PageExit = 0x5E,
    Assign1 = 0x5F,
    Assign2 = 0x60,
    // 0x61 -- exists, never pressed, unidentified
    F1 = 0x62,
    Assign3 = 0x63,
    F2 = 0x64,
    Audition = 0x65,
    F3 = 0x66,
    // 0x67 -- exists, never pressed, unidentified
    F4 = 0x68,
    PagePrev = 0x69,
    F5 = 0x6A,
    PageNext = 0x6B,
    F6 = 0x6C,
    Enter = 0x6D,
    CursorUp = 0x6E,
    CursorLeft = 0x6F,
    CursorRight = 0x70,
    CursorDown = 0x71,
    Dec = 0x72,
    Inc = 0x73,
    Num1 = 0x74,
    Num2 = 0x75, // inferred
    Num3 = 0x76, // inferred
    Num4 = 0x77, // inferred
    Num5 = 0x78, // inferred
    Num6 = 0x79, // inferred
    Num7 = 0x7A,
    Num8 = 0x7B, // inferred
    Num9 = 0x7C,
    Sign = 0x7D, // +/-
    Num0 = 0x7E,
    SetShift = 0x7F, // "."
}

/// Codes never pressed but known to exist (gaps in a contiguous run); kept
/// so a later session can go looking rather than rediscovering the gap.
pub const UNIDENTIFIED_CODES: [u8; 2] = [0x61, 0x67];

/// Key codes inferred from the sequence rather than observed; machine-readable
/// so a UI can mark them and a future capture can assert against the set.
pub fn is_inferred(key: Key) -> bool {
    matches!(
        key,
        Key::Num2 | Key::Num3 | Key::Num4 | Key::Num5 | Key::Num6 | Key::Num8
    )
}

fn frame(device_id: u8, command: u8, payload: &[u8]) -> Result<Vec<u8>, PanelError> {
    if device_id > 0x7F {
        return Err(PanelError::DeviceId(device_id));
    }
    for b in payload {
        if *b > 0x7F {
            return Err(PanelError::PayloadByte(*b));
        }
    }
    let mut out = vec![
        SOX,
        MANUFACTURER_ID,
        PANEL_ID,
        device_id,
        PANEL_DESIGNATOR,
        command,
    ];
    out.extend_from_slice(payload);
    out.push(EOX);
    Ok(out)
}

/// `F0 18 7F <devID> 7A 10 F7` — captured verbatim in §28. The device is
/// silent until this is sent: it does NOT echo front-panel activity cold
/// (§27), the finding that makes this the precondition for everything else.
pub fn open_session(device_id: u8) -> Result<Vec<u8>, PanelError> {
    frame(device_id, PanelCommand::Open as u8, &[])
}

/// `40 <key> 00 <01|00>`. A real press is a pair — down then up; see press().
pub fn button(device_id: u8, key: u8, down: bool) -> Result<Vec<u8>, PanelError> {
    frame(
        device_id,
        PanelCommand::Button as u8,
        &[key, 0x00, if down { 0x01 } else { 0x00 }],
    )
}

/// The two frames a single keypress consists of, in order.
pub fn press(device_id: u8, key: u8) -> Result<[Vec<u8>; 2], PanelError> {
    Ok([
        button(device_id, key, true)?,
        button(device_id, key, false)?,
    ])
}

/// 51h — ask for the current screen. Confirmed live (§33): with a session
/// open this returns a full 2212-byte 50h frame immediately, every time.
/// The device never pushes a screen; a client polls or it sees nothing.
pub fn request_screen(device_id: u8) -> Result<Vec<u8>, PanelError> {
    frame(device_id, PanelCommand::DisplayRequest as u8, &[])
}

/// 52h — the cheap update request: 86 bytes and ~70ms when nothing has
/// changed, against 716ms for a full screen (§33b). Classify the reply with
/// `lcd::classify_update` before decoding it.
pub fn update_screen(device_id: u8) -> Result<Vec<u8>, PanelError> {
    frame(device_id, PanelCommand::DisplayUpdate as u8, &[])
}

/// 60h — provokes a short `61h <a> <b>` reply of unknown meaning (§28 saw
/// `7F 7E`, §33 saw `77 7E`). Exposed because it is cheap, harmless and the
/// next person to look at this needs a way to poke it.
pub fn query_state(device_id: u8) -> Result<Vec<u8>, PanelError> {
    frame(device_id, PanelCommand::ScreenQuery as u8, &[])
}

/// `43 01 <lo> <hi>` — signed 14-bit two's complement, LSB septet first.
/// The device coalesces when a human spins fast: it raises this magnitude
/// rather than the frame rate (§30), so a delta >1 is the natural way to
/// move a list quickly and is the device's own shape.
pub fn dial(device_id: u8, delta: i32) -> Result<Vec<u8>, PanelError> {
    if !(-0x2000..=0x1FFF).contains(&delta) {
        return Err(PanelError::Delta(delta));
    }
    let value = (delta as u32) & 0x3FFF;
    frame(
        device_id,
        PanelCommand::Dial as u8,
        &[0x01, value as u8 & 0x7F, (value >> 7) as u8 & 0x7F],
    )
}

/// Signed delta from a 43h frame, or None if it is not one.
pub fn parse_dial(f: &[u8]) -> Option<i32> {
    if f.len() < 9 || f[0..3] != [SOX, MANUFACTURER_ID, PANEL_ID] {
        return None;
    }
    if f[5] != PanelCommand::Dial as u8 {
        return None;
    }
    let value = f[7] as u32 | ((f[8] as u32) << 7);
    Some(if value >= 0x2000 {
        value as i32 - 0x4000
    } else {
        value as i32
    })
}

/// `(keycode, is_down)` from a 40h frame, or None. Accepts either byte order
/// at positions 3/4: the device sends `7A <devID>` and the host sends
/// `<devID> 7A` (§30). A parser insisting on one would silently drop half
/// the conversation.
pub fn parse_button(f: &[u8]) -> Option<(u8, bool)> {
    if f.len() < 10 || f[0..3] != [SOX, MANUFACTURER_ID, PANEL_ID] {
        return None;
    }
    if f[5] != PanelCommand::Button as u8 {
        return None;
    }
    Some((f[6], f[8] != 0))
}

pub fn key_label(code: u8) -> Option<&'static str> {
    Some(match code {
        0x58 => "MANAGE", // PRESET_MANAGE
        0x59 => "MANAGE", // SAMPLE_MANAGE
        0x5A => "EDIT",   // PRESET_EDIT
        0x5B => "EDIT",   // SAMPLE_EDIT
        0x5C => "MASTER",
        0x5D => "DISK/BROWSE",
        0x5E => "EXIT",
        0x5F => "1",
        0x60 => "2",
        0x62 => "F1",
        0x63 => "3",
        0x64 => "F2",
        0x65 => "AUDITION",
        0x66 => "F3",
        0x68 => "F4",
        0x69 => "PREV",
        0x6A => "F5",
        0x6B => "NEXT",
        0x6C => "F6",
        0x6D => "ENTER",
        0x6E => "▲",
        0x6F => "◀",
        0x70 => "▶",
        0x71 => "▼",
        0x72 => "DEC",
        0x73 => "INC",
        0x74 => "1",
        0x75 => "2",
        0x76 => "3",
        0x77 => "4",
        0x78 => "5",
        0x79 => "6",
        0x7A => "7",
        0x7B => "8",
        0x7C => "9",
        0x7D => "+/-",
        0x7E => "0",
        0x7F => ".",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_frame_verbatim() {
        assert_eq!(
            open_session(17).unwrap(),
            vec![0xF0, 0x18, 0x7F, 17, 0x7A, 0x10, 0xF7]
        );
        assert!(open_session(0x80).is_err());
    }

    #[test]
    fn button_pair() {
        let [down, up] = press(0, Key::Enter as u8).unwrap();
        assert_eq!(
            down,
            vec![0xF0, 0x18, 0x7F, 0, 0x7A, 0x40, 0x6D, 0x00, 0x01, 0xF7]
        );
        assert_eq!(up[8], 0x00);
        let (key, is_down) = parse_button(&down).unwrap();
        assert_eq!((key, is_down), (0x6D, true));
        let (key2, is_down2) = parse_button(&up).unwrap();
        assert_eq!((key2, is_down2), (0x6D, false));
    }

    #[test]
    fn dial_signed_roundtrip() {
        for d in [-0x2000i32, -1, 0, 1, 0x1FFF] {
            let f = dial(0, d).unwrap();
            assert_eq!(parse_dial(&f), Some(d));
        }
        assert!(dial(0, 0x2000).is_err());
        assert_eq!(
            dial(0, -1).unwrap(),
            vec![0xF0, 0x18, 0x7F, 0, 0x7A, 0x43, 0x01, 0x7F, 0x7F, 0xF7]
        );
    }

    #[test]
    fn parse_rejects_foreign_frames() {
        // an editor-protocol frame is not a panel frame
        let ed = crate::messages::build_frame(0x02, &[0, 1], 0);
        assert!(parse_button(&ed).is_none());
        assert!(parse_dial(&ed).is_none());
    }

    #[test]
    fn key_map_consistent() {
        // every declared Key has a label; unidentified codes have none
        assert_eq!(key_label(Key::Sign as u8), Some("+/-"));
        assert_eq!(key_label(Key::Num0 as u8), Some("0"));
        assert_eq!(key_label(0x61), None);
        assert_eq!(key_label(0x67), None);
        assert!(is_inferred(Key::Num5));
        assert!(!is_inferred(Key::Num7));
        assert!(!is_inferred(Key::Num9)); // observed directly
    }
}
