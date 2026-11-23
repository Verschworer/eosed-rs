// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: Copyright (C) 2026  eosed contributors
//
// Rust port of eos/messages.py — SysEx frame codec for the EOS remote
// editor protocol. Frame layout, command bytes, parameter semantics and the
// checksum algorithm are transcribed as data from E-mu's protocol spec:
//   "Remote Preset Editing via MIDI SysEx", Draft #30, EOS 4.00
//   Brian Clark, E-mu Systems, 17 February 1999

//! Frame: `F0 18 21 <devID> 55 <cmd> [...payload...] F7`
//!
//! * `18h` — E-mu Systems manufacturer id.
//! * `21h` — the E4/EOS family's editor product id.
//! * `<devID>` — SysEx device id, 0-126 unique, 127 = broadcast.
//! * `55h` — the "special editor designator" byte.
//! * `<cmd>` — one byte, see [`Command`].
//!
//! Device inquiry uses the separate standard MIDI Universal SysEx
//! (`F0 7E <devID> 06 01 F7`) — see [`build_device_inquiry_request`] /
//! [`parse_device_inquiry_reply`].

use thiserror::Error;

pub type Result<T> = std::result::Result<T, MsgError>;

#[derive(Error, Debug)]
pub enum MsgError {
    #[error("{0}")]
    Protocol(String),
    #[error("payload too short: need {need} bytes, got {have}")]
    Truncated { need: usize, have: usize },
}

// --- framing constants --------------------------------------------------

pub const SOX: u8 = 0xF0;
pub const EOX: u8 = 0xF7;
pub const MANUFACTURER_ID: u8 = 0x18; // E-mu Systems
pub const PRODUCT_ID_E4: u8 = 0x21; // EOS / E4 family editor protocol
pub const EDITOR_DESIGNATOR: u8 = 0x55;
pub const IGNORE_CHECKSUM: u8 = 0x7F; // checksum byte meaning "don't verify"

pub const DEFAULT_DEVICE_ID: u8 = 0x00;
pub const BROADCAST_DEVICE_ID: u8 = 0x7F;

pub const NAME_LENGTH: usize = 16; // preset/sample names are exactly 16 chars

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    ParameterEdit = 0x01,
    ParameterRequest = 0x02,
    ParameterMinMaxDefaultRequest = 0x03,
    ParameterMinMaxDefaultResponse = 0x04,

    PresetName = 0x05,
    PresetNameRequest = 0x06,
    PresetNameCharUpdate = 0x07,
    PresetNameCharRequest = 0x08,

    SampleName = 0x09,
    SampleNameRequest = 0x0A,
    SampleNameCharUpdate = 0x0B,
    SampleNameCharRequest = 0x0C,

    PresetDump = 0x0D,        // sub-commanded; see DumpSubCommand
    PresetDumpRequest = 0x0E, // OLD-format single-preset dump request

    PresetMemoryRequest = 0x10,
    PresetMemoryResponse = 0x11,
    SampleMemoryRequest = 0x12,
    SampleMemoryResponse = 0x13,
    ConfigurationRequest = 0x14,
    ConfigurationResponse = 0x15,
    PresetNumVoicesRequest = 0x16,
    PresetNumVoicesResponse = 0x17,
    PresetNumLinksRequest = 0x18,
    PresetNumLinksResponse = 0x19,
    PresetNumSZonesRequest = 0x1A,
    PresetNumSZonesResponse = 0x1B,
    VoiceNumSZonesRequest = 0x1C,
    VoiceNumSZonesResponse = 0x1D,
    ExtendedConfigurationRequest = 0x1E,
    ExtendedConfigurationResponse = 0x1F,

    NewVoice = 0x20,
    DeleteVoice = 0x21,
    CopyVoice = 0x22,

    NewSampleZone = 0x30,
    GetMultisample = 0x31,
    DeleteSampleZone = 0x32,
    Combine = 0x33,
    Expand = 0x34,

    NewLink = 0x40,
    DeleteLink = 0x41,
    CopyLink = 0x42,

    SampleErase = 0x50,
    SampleMemoryDefrag = 0x52,

    PresetCopy = 0x70,
    PresetDelete = 0x71,
    MultimodeMapDump = 0x72,
    MultimodeMapDumpRequest = 0x73,
    EraseRamBank = 0x74,
    EraseAllRamPresets = 0x75,
    EraseAllRamSamples = 0x76,

    NewDumpNak = 0x79,
    NewDumpAck = 0x7A,
    Eof = 0x7B,
    Wait = 0x7C,
    Cancel = 0x7D,
    Nak = 0x7E, // "old" handshake, 1-byte packet numbers
    Ack = 0x7F, // "old" handshake, 1-byte packet numbers
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpSubCommand {
    OldDumpHeader = 0x01,
    OldDumpMessage = 0x02,
    NewDumpHeader = 0x03,
    NewDumpMessage = 0x04,
    NewDumpRequest = 0x05,
}

/// Destructive, one-shot, no-device-confirmation commands. Never key-bind
/// these in a UI; only reachable through an explicit arm-then-fire flow.
pub fn is_destructive(command: u8) -> bool {
    matches!(
        command,
        0x71 | 0x21 | 0x32 | 0x41 | 0x74 | 0x75 | 0x76 // PRESET_DELETE, DELETE_VOICE,
                                                       // DELETE_SAMPLE_ZONE, DELETE_LINK, ERASE_RAM_BANK,
                                                       // ERASE_ALL_RAM_PRESETS, ERASE_ALL_RAM_SAMPLES
    )
}

// --- 14-bit / multi-byte codecs -----------------------------------------
// Any "N,N" or "N,N,N,N" field is N MIDI data bytes (each 0-127) holding a
// little-endian base-128 integer. Two-byte fields are the common case;
// the OLD dump header's byte count is the one 4-byte field (up to 28 bits).

pub fn encode_u14(value: u32) -> Result<(u8, u8)> {
    if value > 0x3FFF {
        return Err(MsgError::Protocol(format!(
            "value {value} out of unsigned 14-bit range"
        )));
    }
    Ok((value as u8 & 0x7F, (value >> 7) as u8 & 0x7F))
}

pub fn decode_u14(lsb: u8, msb: u8) -> u32 {
    (lsb as u32 & 0x7F) | ((msb as u32 & 0x7F) << 7)
}

/// Encode a signed value (-8192..8191) as (lsb, msb), two's complement in 14 bits.
pub fn encode_s14(value: i32) -> Result<(u8, u8)> {
    if !(-8192..=8191).contains(&value) {
        return Err(MsgError::Protocol(format!(
            "value {value} out of signed 14-bit range"
        )));
    }
    encode_u14((value as u32) & 0x3FFF)
}

/// Encode a 14-bit word that may be signed *or* unsigned. Parameter
/// min/max/default fields carry no signedness flag and a real device mixes
/// both in one reply; accepts either domain and emits the same bit pattern.
pub fn encode_14(value: i64) -> Result<(u8, u8)> {
    if !(-8192..=0x3FFF).contains(&value) {
        return Err(MsgError::Protocol(format!(
            "value {value} out of 14-bit range"
        )));
    }
    encode_u14((value as u32) & 0x3FFF)
}

pub fn decode_s14(lsb: u8, msb: u8) -> i32 {
    let mut value = decode_u14(lsb, msb) as i32;
    if value & 0x2000 != 0 {
        // bit 13 set -> negative in 14-bit two's complement
        value -= 0x4000;
    }
    value
}

/// Encode `value` as `n` 7-bit MIDI bytes, LSB first (base-128).
pub fn encode_lsb_bytes(value: u64, n: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(n);
    let mut remaining = value;
    for _ in 0..n {
        out.push((remaining & 0x7F) as u8);
        remaining >>= 7;
    }
    if remaining != 0 {
        return Err(MsgError::Protocol(format!(
            "value {value} does not fit in {n} 7-bit bytes"
        )));
    }
    Ok(out)
}

pub fn decode_lsb_bytes(data: &[u8]) -> u64 {
    let mut value = 0u64;
    for (i, byte) in data.iter().enumerate() {
        value |= (*byte as u64 & 0x7F) << (7 * i);
    }
    value
}

/// 1's-complement of the sum of `data_bytes`, masked to a 7-bit byte.
pub fn checksum(data: &[u8]) -> u8 {
    let sum: u32 = data.iter().map(|b| *b as u32).sum();
    (!sum) as u8 & 0x7F
}

fn pad_name(name: &str) -> Result<Vec<u8>> {
    if !name.is_ascii() {
        let bad = name.chars().find(|c| !c.is_ascii()).unwrap();
        return Err(MsgError::Protocol(format!(
            "name contains non-ASCII character {bad:?}"
        )));
    }
    let raw = &name.as_bytes()[..name.len().min(NAME_LENGTH)];
    let mut out = raw.to_vec();
    out.resize(NAME_LENGTH, 0x20);
    Ok(out)
}

fn unpad_name(raw: &[u8]) -> String {
    let s: String = raw.iter().map(|b| (b & 0x7F) as char).collect::<String>();
    s.trim_end().to_string()
}

// --- generic frame build/parse -------------------------------------------

pub fn build_frame(command: u8, payload: &[u8], device_id: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 7);
    out.extend_from_slice(&[
        SOX,
        MANUFACTURER_ID,
        PRODUCT_ID_E4,
        device_id,
        EDITOR_DESIGNATOR,
        command,
    ]);
    out.extend_from_slice(payload);
    out.push(EOX);
    out
}

/// Validate and split an editor-protocol frame into (device_id, command, payload).
pub fn parse_frame(data: &[u8]) -> Result<(u8, u8, &[u8])> {
    if data.len() < 7 || data[0] != SOX || data[data.len() - 1] != EOX {
        return Err(MsgError::Protocol("not a SysEx frame".into()));
    }
    if data[1] != MANUFACTURER_ID || data[2] != PRODUCT_ID_E4 || data[4] != EDITOR_DESIGNATOR {
        return Err(MsgError::Protocol(
            "not an EOS editor-protocol frame".into(),
        ));
    }
    Ok((data[3], data[5], &data[6..data.len() - 1]))
}

pub fn has_valid_header(data: &[u8]) -> bool {
    parse_frame(data).is_ok()
}

fn need(payload: &[u8], n: usize) -> Result<()> {
    if payload.len() < n {
        Err(MsgError::Truncated {
            need: n,
            have: payload.len(),
        })
    } else {
        Ok(())
    }
}

// --- device inquiry (separate Universal Non-Realtime SysEx) -------------

pub const FAMILY_CODE: (u8, u8) = (0x01, 0x04); // fixed for the whole EOS/E4 line

pub fn family_member(member: (u8, u8)) -> Option<&'static str> {
    match member {
        (0x00, 0x05) => Some("E4"),
        (0x01, 0x05) => Some("E64"),
        (0x02, 0x05) => Some("E4k"),
        (0x03, 0x05) => Some("E64FX"),
        (0x04, 0x05) => Some("E4XT"),
        (0x05, 0x05) => Some("E4X"),
        (0x06, 0x05) => Some("E6400"),
        (0x07, 0x05) => Some("E4XT Ultra"),
        (0x08, 0x05) => Some("E6400 Ultra"),
        _ => None,
    }
}

pub fn build_device_inquiry_request(device_id: u8) -> Vec<u8> {
    vec![SOX, 0x7E, device_id, 0x06, 0x01, EOX]
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInquiryReply {
    pub device_id: u8,
    pub family_code: (u8, u8),
    pub member_code: (u8, u8),
    pub revision: String,
}

impl DeviceInquiryReply {
    pub fn model(&self) -> Option<&'static str> {
        family_member(self.member_code)
    }
}

pub fn parse_device_inquiry_reply(data: &[u8]) -> Result<DeviceInquiryReply> {
    if data.len() < 15
        || data[0] != SOX
        || data[data.len() - 1] != EOX
        || data[1] != 0x7E
        || data[3] != 0x06
        || data[4] != 0x02
    {
        return Err(MsgError::Protocol("not a Device Inquiry reply".into()));
    }
    if data[5] != MANUFACTURER_ID {
        return Err(MsgError::Protocol(format!(
            "not an E-mu device inquiry reply (mfr id {:#x})",
            data[5]
        )));
    }
    let revision = data[10..14].iter().map(|b| *b as char).collect();
    Ok(DeviceInquiryReply {
        device_id: data[2],
        family_code: (data[6], data[7]),
        member_code: (data[8], data[9]),
        revision,
    })
}

// --- field-message scaffolding -------------------------------------------
// Most commands are a fixed sequence of unsigned fields (1 raw byte or a
// u14 pair) with no checksum. The macro declares the layout once and does
// the packing/unpacking, so no copy-paste slip is possible in one of them.

/// Declares a fixed-field, no-checksum message struct with encode/decode.
/// Width 1 = raw byte, width 2 = u14 pair.
macro_rules! field_message {
    ($name:ident, $cmd:expr, [$(($field:ident, $width:literal)),* $(,)?]) => {
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            $(pub $field: u32,)*
            pub device_id: u8,
        }
        impl $name {
            pub const COMMAND: u8 = $cmd;
            pub fn encode(&self) -> Vec<u8> {
                let mut payload: Vec<u8> = Vec::new();
                $(
                    if $width == 1 {
                        payload.push(self.$field as u8 & 0x7F);
                    } else {
                        let (a, b) = encode_u14(self.$field).expect("field out of u14 range");
                        payload.push(a);
                        payload.push(b);
                    }
                )*
                build_frame($cmd, &payload, self.device_id)
            }
            pub fn decode(data: &[u8]) -> Result<Self> {
                let (device_id, command, payload) = parse_frame(data)?;
                if command != $cmd {
                    return Err(MsgError::Protocol(format!(
                        concat!(stringify!($name), ": expected command {:#x}, got {:#x}"),
                        $cmd, command
                    )));
                }
                need(payload, 0 $(+ $width)*)?;
                let mut i = 0usize;
                $(
                    let $field = if $width == 1 {
                        let v = payload[i] as u32;
                        i += 1;
                        v
                    } else {
                        let v = decode_u14(payload[i], payload[i + 1]);
                        i += 2;
                        v
                    };
                )*
                let _ = i;
                Ok(Self { $($field,)* device_id })
            }
        }
    };
}

macro_rules! no_field_message {
    ($name:ident, $cmd:expr) => {
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            pub device_id: u8,
        }
        impl $name {
            pub const COMMAND: u8 = $cmd;
            pub fn new() -> Self {
                Self {
                    device_id: DEFAULT_DEVICE_ID,
                }
            }
            pub fn encode(&self) -> Vec<u8> {
                build_frame($cmd, &[], self.device_id)
            }
            pub fn decode(data: &[u8]) -> Result<Self> {
                let (device_id, command, _) = parse_frame(data)?;
                if command != $cmd {
                    return Err(MsgError::Protocol(format!(
                        concat!(stringify!($name), ": expected command {:#x}, got {:#x}"),
                        $cmd, command
                    )));
                }
                Ok(Self { device_id })
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

// -- Parameter Edit / Request (0x01 / 0x02) — checksummed, variable length --

pub const MAX_PARAMETER_EDITS: usize = 42; // spec: "no more than 256 Data Bytes, or 42 edits"
pub const MAX_PARAMETER_REQUESTS: usize = 64; // spec: "no more than 256 Data Bytes, or 64 IDs"

/// Command 0x01 — write one or more (param_id, value) pairs. `values` holds
/// already-packed 14-bit words (`value & 0x3FFF` for signed data; `params`
/// provides the per-parameter signed/unsigned convention). The device's
/// response to a request (0x02) reuses this exact format, one message per
/// parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct ParameterEdit {
    pub values: Vec<(u32, u32)>,
    pub device_id: u8,
    /// None when constructing to send ("compute a fresh checksum"). A decoded
    /// instance holds the byte that was actually on the wire, so callers can
    /// verify() before trusting the data (e.g. before ACKing a dump packet).
    pub checksum_byte: Option<u8>,
}

impl ParameterEdit {
    pub const COMMAND: u8 = 0x01;
    fn data_bytes(&self) -> Result<Vec<u8>> {
        let mut data = Vec::with_capacity(self.values.len() * 4);
        for (param_id, value) in &self.values {
            let (a, b) = encode_u14(*param_id)?;
            let (c, d) = encode_u14(*value & 0x3FFF)?;
            data.extend_from_slice(&[a, b, c, d]);
        }
        Ok(data)
    }
    pub fn verify(&self) -> Result<bool> {
        match self.checksum_byte {
            None | Some(IGNORE_CHECKSUM) => Ok(true),
            Some(ck) => Ok(ck == checksum(&self.data_bytes()?)),
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.values.is_empty() {
            return Err(MsgError::Protocol(
                "ParameterEdit needs at least one (id, value) pair".into(),
            ));
        }
        if self.values.len() > MAX_PARAMETER_EDITS {
            return Err(MsgError::Protocol(format!(
                "at most {MAX_PARAMETER_EDITS} parameter edits per message"
            )));
        }
        let data = self.data_bytes()?;
        let byte_count = (2 * self.values.len()) as u8;
        let cksum = self.checksum_byte.unwrap_or_else(|| checksum(&data));
        let mut payload = Vec::with_capacity(data.len() + 2);
        payload.push(byte_count);
        payload.extend_from_slice(&data);
        payload.push(cksum);
        Ok(build_frame(Self::COMMAND, &payload, self.device_id))
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let (device_id, command, payload) = parse_frame(data)?;
        if command != Self::COMMAND {
            return Err(MsgError::Protocol(format!(
                "not a PARAMETER_EDIT frame: {command:#x}"
            )));
        }
        need(payload, 2)?;
        let byte_count = payload[0] as usize * 2;
        if payload.len() < byte_count + 2 {
            return Err(MsgError::Truncated {
                need: byte_count + 2,
                have: payload.len(),
            });
        }
        let body = &payload[1..1 + byte_count];
        let cksum = payload[1 + byte_count];
        let mut pairs = Vec::with_capacity(byte_count / 4);
        for chunk in body.as_chunks::<4>().0 {
            pairs.push((
                decode_u14(chunk[0], chunk[1]),
                decode_u14(chunk[2], chunk[3]),
            ));
        }
        Ok(Self {
            values: pairs,
            device_id,
            checksum_byte: Some(cksum),
        })
    }
}

/// Command 0x02 — request the current value of one or more parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct ParameterRequest {
    pub param_ids: Vec<u32>,
    pub device_id: u8,
    pub checksum_byte: Option<u8>,
}

impl ParameterRequest {
    pub const COMMAND: u8 = 0x02;
    fn data_bytes(&self) -> Result<Vec<u8>> {
        let mut data = Vec::with_capacity(self.param_ids.len() * 2);
        for param_id in &self.param_ids {
            let (a, b) = encode_u14(*param_id)?;
            data.push(a);
            data.push(b);
        }
        Ok(data)
    }
    pub fn verify(&self) -> Result<bool> {
        match self.checksum_byte {
            None | Some(IGNORE_CHECKSUM) => Ok(true),
            Some(ck) => Ok(ck == checksum(&self.data_bytes()?)),
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.param_ids.is_empty() {
            return Err(MsgError::Protocol(
                "ParameterRequest needs at least one parameter id".into(),
            ));
        }
        if self.param_ids.len() > MAX_PARAMETER_REQUESTS {
            return Err(MsgError::Protocol(format!(
                "at most {MAX_PARAMETER_REQUESTS} parameter ids per message"
            )));
        }
        let data = self.data_bytes()?;
        let byte_count = self.param_ids.len() as u8;
        let cksum = self.checksum_byte.unwrap_or_else(|| checksum(&data));
        let mut payload = Vec::with_capacity(data.len() + 2);
        payload.push(byte_count);
        payload.extend_from_slice(&data);
        payload.push(cksum);
        Ok(build_frame(Self::COMMAND, &payload, self.device_id))
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let (device_id, command, payload) = parse_frame(data)?;
        if command != Self::COMMAND {
            return Err(MsgError::Protocol(format!(
                "not a PARAMETER_REQUEST frame: {command:#x}"
            )));
        }
        need(payload, 2)?;
        let byte_count = payload[0] as usize * 2;
        if payload.len() < byte_count + 2 {
            return Err(MsgError::Truncated {
                need: byte_count + 2,
                have: payload.len(),
            });
        }
        let body = &payload[1..1 + byte_count];
        let cksum = payload[1 + byte_count];
        let ids = body
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| decode_u14(p[0], p[1]))
            .collect();
        Ok(Self {
            param_ids: ids,
            device_id,
            checksum_byte: Some(cksum),
        })
    }
}

// -- Parameter min/max/default (0x03 / 0x04) — no checksum ------------------

field_message!(ParameterRangeRequest, 0x03, [(param_id, 2)]);

/// min/max/default travel as RAW 14-bit words. This layer genuinely cannot
/// know signedness — the wire carries no flag — so decode() returns raw
/// u14s and `bridge` applies signedness from the `params` table, the single
/// source of truth that also drives value decoding, so the two halves of
/// one parameter cannot disagree. (An earlier version sign-extended
/// unconditionally and corrupted every unsigned range past 8191:
/// E4_VOICE_DELAY's max 10000 came back as -6384, confirmed live 2026-07-31
/// on an E4XT Ultra rev 4.70.)
#[derive(Debug, Clone, PartialEq)]
pub struct ParameterRange {
    pub param_id: u32,
    pub minimum: u32,
    pub maximum: u32,
    pub default: u32,
    pub device_id: u8,
}

impl ParameterRange {
    pub const COMMAND: u8 = 0x04;
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut payload = Vec::with_capacity(8);
        for w in [
            encode_u14(self.param_id)?,
            encode_14(self.minimum as i64)?,
            encode_14(self.maximum as i64)?,
            encode_14(self.default as i64)?,
        ] {
            payload.push(w.0);
            payload.push(w.1);
        }
        Ok(build_frame(Self::COMMAND, &payload, self.device_id))
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let (device_id, command, payload) = parse_frame(data)?;
        if command != Self::COMMAND {
            return Err(MsgError::Protocol(format!(
                "not a PARAMETER_MINMAXDEFAULT_RESPONSE frame: {command:#x}"
            )));
        }
        need(payload, 8)?;
        Ok(Self {
            param_id: decode_u14(payload[0], payload[1]),
            minimum: decode_u14(payload[2], payload[3]),
            maximum: decode_u14(payload[4], payload[5]),
            default: decode_u14(payload[6], payload[7]),
            device_id,
        })
    }
}

// -- Preset / Sample naming (0x05-0x0C) — no checksum -----------------------

macro_rules! name_message {
    ($name:ident, $cmd:expr, $index:ident) => {
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            pub $index: u32,
            pub name: String,
            pub device_id: u8,
        }
        impl $name {
            pub const COMMAND: u8 = $cmd;
            pub fn encode(&self) -> Result<Vec<u8>> {
                let mut payload = Vec::with_capacity(2 + NAME_LENGTH);
                let (a, b) = encode_u14(self.$index)?;
                payload.push(a);
                payload.push(b);
                payload.extend(pad_name(&self.name)?);
                Ok(build_frame($cmd, &payload, self.device_id))
            }
            pub fn decode(data: &[u8]) -> Result<Self> {
                let (device_id, command, payload) = parse_frame(data)?;
                if command != $cmd {
                    return Err(MsgError::Protocol(format!(
                        concat!(stringify!($name), ": expected command {:#x}, got {:#x}"),
                        $cmd, command
                    )));
                }
                need(payload, 2 + NAME_LENGTH)?;
                Ok(Self {
                    $index: decode_u14(payload[0], payload[1]),
                    name: unpad_name(&payload[2..2 + NAME_LENGTH]),
                    device_id,
                })
            }
        }
    };
}

name_message!(PresetName, 0x05, preset);
name_message!(SampleName, 0x09, sample);

field_message!(PresetNameRequest, 0x06, [(preset, 2)]);
field_message!(SampleNameRequest, 0x0A, [(sample, 2)]);

// (an optional-field generic form does not expand in macro_rules; the two
// shapes are written explicitly)
macro_rules! name_char_update {
    ($name:ident, $cmd:expr, $index:ident) => {
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            pub $index: u32,
            pub char_index: u8, // 0-15
            pub char_val: char,
            pub device_id: u8,
        }
        impl $name {
            pub const COMMAND: u8 = $cmd;
            pub fn encode(&self) -> Result<Vec<u8>> {
                if self.char_index > NAME_LENGTH as u8 - 1 {
                    return Err(MsgError::Protocol(format!(
                        "char_index {} out of 0-15 range",
                        self.char_index
                    )));
                }
                let (a, b) = encode_u14(self.$index)?;
                let payload = vec![a, b, self.char_index, self.char_val as u32 as u8 & 0x7F];
                Ok(build_frame($cmd, &payload, self.device_id))
            }
            pub fn decode(data: &[u8]) -> Result<Self> {
                let (device_id, command, payload) = parse_frame(data)?;
                if command != $cmd {
                    return Err(MsgError::Protocol(format!(
                        concat!(stringify!($name), ": expected command {:#x}, got {:#x}"),
                        $cmd, command
                    )));
                }
                need(payload, 4)?;
                Ok(Self {
                    $index: decode_u14(payload[0], payload[1]),
                    char_index: payload[2],
                    char_val: payload[3] as char,
                    device_id,
                })
            }
        }
    };
}

macro_rules! name_char_request {
    ($name:ident, $cmd:expr, $index:ident) => {
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            pub $index: u32,
            pub char_index: u8, // 0-15
            pub device_id: u8,
        }
        impl $name {
            pub const COMMAND: u8 = $cmd;
            pub fn encode(&self) -> Result<Vec<u8>> {
                if self.char_index > NAME_LENGTH as u8 - 1 {
                    return Err(MsgError::Protocol(format!(
                        "char_index {} out of 0-15 range",
                        self.char_index
                    )));
                }
                let (a, b) = encode_u14(self.$index)?;
                let payload = vec![a, b, self.char_index];
                Ok(build_frame($cmd, &payload, self.device_id))
            }
            pub fn decode(data: &[u8]) -> Result<Self> {
                let (device_id, command, payload) = parse_frame(data)?;
                if command != $cmd {
                    return Err(MsgError::Protocol(format!(
                        concat!(stringify!($name), ": expected command {:#x}, got {:#x}"),
                        $cmd, command
                    )));
                }
                need(payload, 3)?;
                Ok(Self {
                    $index: decode_u14(payload[0], payload[1]),
                    char_index: payload[2],
                    device_id,
                })
            }
        }
    };
}

name_char_update!(PresetNameCharUpdate, 0x07, preset);
name_char_request!(PresetNameCharRequest, 0x08, preset);
name_char_update!(SampleNameCharUpdate, 0x0B, sample);
name_char_request!(SampleNameCharRequest, 0x0C, sample);

// -- Preset dump (0x0D sub-commanded, 0x0E) ---------------------------------

// Command 0x0E — request the OLD-format dump of a preset by number. If a
// non-existent preset is requested, the response is a Cancel message (per
// spec). Only one preset may be dumped to/from the device at a time.
field_message!(PresetDumpRequest, 0x0E, [(preset, 2)]);

macro_rules! dump_sub_check {
    ($payload:expr, $sub:expr, $what:expr) => {
        if $payload.is_empty() || $payload[0] != $sub as u8 {
            return Err(MsgError::Protocol(
                concat!("not a ", $what, " frame").into(),
            ));
        }
    };
}

/// 0x0D/0x01 — OLD dump format header: total byte count only.
#[derive(Debug, Clone, PartialEq)]
pub struct OldDumpHeader {
    pub byte_count: u64,
    pub packet_number: u8, // spec: always 0 (the header is the first packet)
    pub device_id: u8,
}

impl OldDumpHeader {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut payload = vec![DumpSubCommand::OldDumpHeader as u8, self.packet_number];
        payload.extend(encode_lsb_bytes(self.byte_count, 4)?);
        Ok(build_frame(0x0D, &payload, self.device_id))
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let (device_id, command, payload) = parse_frame(data)?;
        if command != 0x0D {
            return Err(MsgError::Protocol("not an OLD_DUMP_HEADER frame".into()));
        }
        dump_sub_check!(payload, DumpSubCommand::OldDumpHeader, "OLD_DUMP_HEADER");
        need(payload, 6)?;
        Ok(Self {
            byte_count: decode_lsb_bytes(&payload[2..6]),
            packet_number: payload[1],
            device_id,
        })
    }
}

/// 0x0D/0x02 — OLD dump format data message: up to 256 bytes + checksum.
#[derive(Debug, Clone, PartialEq)]
pub struct OldDumpMessage {
    pub packet_number: u8, // low 7 bits of a running count
    pub data: Vec<u8>,
    pub device_id: u8,
    pub checksum_byte: Option<u8>,
}

impl OldDumpMessage {
    pub const MAX_DATA: usize = 256;
    fn verify(&self) -> bool {
        match self.checksum_byte {
            None | Some(IGNORE_CHECKSUM) => true,
            Some(ck) => ck == checksum(&self.data),
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.data.len() > Self::MAX_DATA {
            return Err(MsgError::Protocol(format!(
                "OLD dump message data exceeds {} bytes",
                Self::MAX_DATA
            )));
        }
        let cksum = self.checksum_byte.unwrap_or_else(|| checksum(&self.data));
        let mut payload = Vec::with_capacity(self.data.len() + 3);
        payload.push(DumpSubCommand::OldDumpMessage as u8);
        payload.push(self.packet_number & 0x7F);
        payload.extend_from_slice(&self.data);
        payload.push(cksum);
        Ok(build_frame(0x0D, &payload, self.device_id))
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let (device_id, command, payload) = parse_frame(data)?;
        if command != 0x0D {
            return Err(MsgError::Protocol("not an OLD_DUMP_MESSAGE frame".into()));
        }
        dump_sub_check!(payload, DumpSubCommand::OldDumpMessage, "OLD_DUMP_MESSAGE");
        need(payload, 3)?;
        Ok(Self {
            packet_number: payload[1],
            data: payload[2..payload.len() - 1].to_vec(),
            device_id,
            checksum_byte: Some(payload[payload.len() - 1]),
        })
    }
    pub fn verify_checksum(&self) -> bool {
        self.verify()
    }
}

/// 0x0D/0x05 — request the NEW-format dump of a preset by number.
#[derive(Debug, Clone, PartialEq)]
pub struct NewDumpRequest {
    pub preset: u32,
    pub device_id: u8,
}

impl NewDumpRequest {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let (a, b) = encode_u14(self.preset)?;
        Ok(build_frame(
            0x0D,
            &[DumpSubCommand::NewDumpRequest as u8, a, b],
            self.device_id,
        ))
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let (device_id, command, payload) = parse_frame(data)?;
        if command != 0x0D {
            return Err(MsgError::Protocol("not a NEW_DUMP_REQUEST frame".into()));
        }
        dump_sub_check!(payload, DumpSubCommand::NewDumpRequest, "NEW_DUMP_REQUEST");
        need(payload, 3)?;
        Ok(Self {
            preset: decode_u14(payload[1], payload[2]),
            device_id,
        })
    }
}

/// 0x0D/0x03 — NEW dump format header: per-section parameter counts let the
/// parser stay forward-compatible with EOS versions that add parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct NewDumpHeader {
    pub preset: u32,
    pub total_bytes: u64,
    pub num_global_params: u32,
    pub num_link_params: u32,
    pub num_voice_params: u32,
    pub num_zone_params: u32,
    pub device_id: u8,
}

impl NewDumpHeader {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut payload = vec![DumpSubCommand::NewDumpHeader as u8];
        let (a, b) = encode_u14(self.preset)?;
        payload.extend([a, b]);
        payload.extend(encode_lsb_bytes(self.total_bytes, 4)?);
        for n in [
            self.num_global_params,
            self.num_link_params,
            self.num_voice_params,
            self.num_zone_params,
        ] {
            let (a, b) = encode_u14(n)?;
            payload.extend([a, b]);
        }
        Ok(build_frame(0x0D, &payload, self.device_id))
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let (device_id, command, payload) = parse_frame(data)?;
        if command != 0x0D {
            return Err(MsgError::Protocol("not a NEW_DUMP_HEADER frame".into()));
        }
        dump_sub_check!(payload, DumpSubCommand::NewDumpHeader, "NEW_DUMP_HEADER");
        need(payload, 15)?;
        Ok(Self {
            preset: decode_u14(payload[1], payload[2]),
            total_bytes: decode_lsb_bytes(&payload[3..7]),
            num_global_params: decode_u14(payload[7], payload[8]),
            num_link_params: decode_u14(payload[9], payload[10]),
            num_voice_params: decode_u14(payload[11], payload[12]),
            num_zone_params: decode_u14(payload[13], payload[14]),
            device_id,
        })
    }
}

/// 0x0D/0x04 — NEW dump format data message: up to 244 bytes + checksum,
/// with a 2-byte (u14) running packet count starting at 1.
#[derive(Debug, Clone, PartialEq)]
pub struct NewDumpMessage {
    pub packet_number: u32,
    pub data: Vec<u8>,
    pub device_id: u8,
    pub checksum_byte: Option<u8>,
}

impl NewDumpMessage {
    pub const MAX_DATA: usize = 244;
    pub fn verify(&self) -> bool {
        match self.checksum_byte {
            None | Some(IGNORE_CHECKSUM) => true,
            Some(ck) => ck == checksum(&self.data),
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.data.len() > Self::MAX_DATA {
            return Err(MsgError::Protocol(format!(
                "NEW dump message data exceeds {} bytes",
                Self::MAX_DATA
            )));
        }
        let (a, b) = encode_u14(self.packet_number)?;
        let cksum = self.checksum_byte.unwrap_or_else(|| checksum(&self.data));
        let mut payload = Vec::with_capacity(self.data.len() + 4);
        payload.push(DumpSubCommand::NewDumpMessage as u8);
        payload.push(a);
        payload.push(b);
        payload.extend_from_slice(&self.data);
        payload.push(cksum);
        Ok(build_frame(0x0D, &payload, self.device_id))
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let (device_id, command, payload) = parse_frame(data)?;
        if command != 0x0D {
            return Err(MsgError::Protocol("not a NEW_DUMP_MESSAGE frame".into()));
        }
        dump_sub_check!(payload, DumpSubCommand::NewDumpMessage, "NEW_DUMP_MESSAGE");
        need(payload, 4)?;
        Ok(Self {
            packet_number: decode_u14(payload[1], payload[2]),
            data: payload[3..payload.len() - 1].to_vec(),
            device_id,
            checksum_byte: Some(payload[payload.len() - 1]),
        })
    }
}

// -- Generic dump handshake (ACK/NAK/WAIT/CANCEL/EOF) ------------------------

/// Old-style ACK (0x7F) — 1-byte packet number.
#[derive(Debug, Clone, PartialEq)]
pub struct Ack {
    pub packet_number: u8,
    pub device_id: u8,
}
impl Ack {
    pub fn encode(&self) -> Vec<u8> {
        build_frame(0x7F, &[self.packet_number & 0x7F], self.device_id)
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let (device_id, command, payload) = parse_frame(data)?;
        if command != 0x7F {
            return Err(MsgError::Protocol(format!(
                "not an ACK frame: {command:#x}"
            )));
        }
        need(payload, 1)?;
        Ok(Self {
            packet_number: payload[0],
            device_id,
        })
    }
}

/// Old-style NAK (0x7E) — 1-byte packet number; resend that packet.
#[derive(Debug, Clone, PartialEq)]
pub struct Nak {
    pub packet_number: u8,
    pub device_id: u8,
}
impl Nak {
    pub fn encode(&self) -> Vec<u8> {
        build_frame(0x7E, &[self.packet_number & 0x7F], self.device_id)
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let (device_id, command, payload) = parse_frame(data)?;
        if command != 0x7E {
            return Err(MsgError::Protocol(format!("not a NAK frame: {command:#x}")));
        }
        need(payload, 1)?;
        Ok(Self {
            packet_number: payload[0],
            device_id,
        })
    }
}

/// NEW-style ACK (0x7A) — 2-byte (u14) packet number.
#[derive(Debug, Clone, PartialEq)]
pub struct NewAck {
    pub packet_number: u32,
    pub device_id: u8,
}
impl NewAck {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let (a, b) = encode_u14(self.packet_number)?;
        Ok(build_frame(0x7A, &[a, b], self.device_id))
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let (device_id, command, payload) = parse_frame(data)?;
        if command != 0x7A {
            return Err(MsgError::Protocol(format!(
                "not a NEW_DUMP_ACK frame: {command:#x}"
            )));
        }
        need(payload, 2)?;
        Ok(Self {
            packet_number: decode_u14(payload[0], payload[1]),
            device_id,
        })
    }
}

/// NEW-style NAK (0x79) — 2-byte (u14) packet number; resend that packet.
#[derive(Debug, Clone, PartialEq)]
pub struct NewNak {
    pub packet_number: u32,
    pub device_id: u8,
}
impl NewNak {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let (a, b) = encode_u14(self.packet_number)?;
        Ok(build_frame(0x79, &[a, b], self.device_id))
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let (device_id, command, payload) = parse_frame(data)?;
        if command != 0x79 {
            return Err(MsgError::Protocol(format!(
                "not a NEW_DUMP_NAK frame: {command:#x}"
            )));
        }
        need(payload, 2)?;
        Ok(Self {
            packet_number: decode_u14(payload[0], payload[1]),
            device_id,
        })
    }
}

no_field_message!(Wait, 0x7C); // stop sending packets until an ACK
no_field_message!(Cancel, 0x7D); // abort the dump
no_field_message!(EndOfFile, 0x7B); // no more packets; must end a transfer

// -- Configuration / memory / count queries (0x10-0x1F) ---------------------

no_field_message!(PresetMemoryRequest, 0x10);
field_message!(PresetMemoryResponse, 0x11, [(total_kb, 2), (free_kb, 2)]);
no_field_message!(SampleMemoryRequest, 0x12);
// spec: "Total Sample Memory Free (in 10's of kBytes)"
field_message!(SampleMemoryResponse, 0x13, [(total_mb, 2), (free_10kb, 2)]);
no_field_message!(ConfigurationRequest, 0x14);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigOptions {
    pub voices_128: bool,
    pub fx_card: bool,
    pub midi_card: bool,
    pub octopus_card: bool,
    pub digital_io: bool,
}

pub fn decode_config_options(byte: u8) -> ConfigOptions {
    ConfigOptions {
        voices_128: byte & 0x01 != 0,
        fx_card: byte & 0x02 != 0,
        midi_card: byte & 0x04 != 0,
        octopus_card: byte & 0x08 != 0,
        digital_io: byte & 0x10 != 0,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigurationResponse {
    pub options: u8,
    pub ram_mb: u32,
    pub device_id: u8,
}
impl ConfigurationResponse {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let (a, b) = encode_u14(self.ram_mb)?;
        Ok(build_frame(
            0x15,
            &[self.options & 0x7F, a, b],
            self.device_id,
        ))
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let (device_id, command, payload) = parse_frame(data)?;
        if command != 0x15 {
            return Err(MsgError::Protocol(
                "not a CONFIGURATION_RESPONSE frame".into(),
            ));
        }
        need(payload, 3)?;
        Ok(Self {
            options: payload[0],
            ram_mb: decode_u14(payload[1], payload[2]),
            device_id,
        })
    }
    pub fn option_flags(&self) -> ConfigOptions {
        decode_config_options(self.options)
    }
}

field_message!(PresetNumVoicesRequest, 0x16, [(preset, 2)]);
field_message!(PresetNumVoicesResponse, 0x17, [(num_voices, 2)]);
field_message!(PresetNumLinksRequest, 0x18, [(preset, 2)]);
field_message!(PresetNumLinksResponse, 0x19, [(num_links, 2)]);
field_message!(PresetNumSZonesRequest, 0x1A, [(preset, 2)]);
field_message!(PresetNumSZonesResponse, 0x1B, [(num_szones, 2)]);
field_message!(VoiceNumSZonesRequest, 0x1C, [(preset, 2), (voice, 2)]);
field_message!(VoiceNumSZonesResponse, 0x1D, [(num_szones, 2)]);
no_field_message!(ExtendedConfigurationRequest, 0x1E);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtendedConfigOptions {
    pub voices_128: bool,
    pub fx_card: bool,
    pub midi_card: bool,
    pub octopus_card: bool,
    pub digital_io: bool,
    pub preset_flash: bool,
    pub adat_io: bool,
}

pub fn decode_extended_config_options(byte: u8) -> ExtendedConfigOptions {
    ExtendedConfigOptions {
        voices_128: byte & 0x01 != 0,
        fx_card: byte & 0x02 != 0,
        midi_card: byte & 0x04 != 0,
        octopus_card: byte & 0x08 != 0,
        digital_io: byte & 0x10 != 0,
        preset_flash: byte & 0x20 != 0,
        adat_io: byte & 0x40 != 0,
    }
}

/// 0-126 MB directly; 0x7F is defined by the spec to mean 128 MB.
fn decode_mb_byte(raw: u8) -> u32 {
    if raw == 0x7F {
        128
    } else {
        raw as u32
    }
}

fn encode_mb_byte(mb: u32) -> Result<u8> {
    if mb == 128 {
        return Ok(0x7F);
    }
    if mb > 126 {
        return Err(MsgError::Protocol(format!(
            "MB value {mb} out of encodable range (0-126 or 128)"
        )));
    }
    Ok(mb as u8)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtendedConfigurationResponse {
    pub options1: u8,
    pub options2: u8,
    pub ram_mb: u32,
    pub rom_mb: u32,
    pub flash_mb: u32,
    pub device_id: u8,
}

impl ExtendedConfigurationResponse {
    pub fn option_flags(&self) -> ExtendedConfigOptions {
        decode_extended_config_options(self.options1)
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        let (a, b) = encode_u14(self.ram_mb)?;
        let payload = vec![
            self.options1 & 0x7F,
            self.options2 & 0x7F,
            a,
            b,
            encode_mb_byte(self.rom_mb)?,
            encode_mb_byte(self.flash_mb)?,
            0,
            0,
            0,
            0, // 4 reserved bytes
        ];
        Ok(build_frame(0x1F, &payload, self.device_id))
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let (device_id, command, payload) = parse_frame(data)?;
        if command != 0x1F {
            return Err(MsgError::Protocol(
                "not an EXTENDED_CONFIGURATION_RESPONSE frame".into(),
            ));
        }
        need(payload, 6)?;
        Ok(Self {
            options1: payload[0],
            options2: payload[1],
            ram_mb: decode_u14(payload[2], payload[3]),
            rom_mb: decode_mb_byte(payload[4]),
            flash_mb: decode_mb_byte(payload[5]),
            device_id,
        })
    }
}

// -- Voice / SZone / Link utilities (0x20-0x42) ------------------------------

field_message!(NewVoice, 0x20, [(preset, 2)]);
field_message!(DeleteVoice, 0x21, [(preset, 2), (voice, 2)]);
// group 0-31, single raw byte on the wire (unlike Combine's group)
field_message!(
    CopyVoice,
    0x22,
    [(src_preset, 2), (src_voice, 2), (dst_preset, 2), (group, 1)]
);
field_message!(NewSampleZone, 0x30, [(preset, 2), (voice, 2)]);
field_message!(
    GetMultisample,
    0x31,
    [
        (src_preset, 2),
        (src_voice, 2),
        (dst_preset, 2),
        (dst_voice, 2)
    ]
);
field_message!(
    DeleteSampleZone,
    0x32,
    [(preset, 2), (voice, 2), (sample_zone, 2)]
);
// group 0-31, but sent as a u14 pair on the wire (unlike CopyVoice's group)
field_message!(Combine, 0x33, [(preset, 2), (group, 2)]);
field_message!(Expand, 0x34, [(preset, 2), (voice, 2)]);
field_message!(NewLink, 0x40, [(preset, 2)]);
field_message!(DeleteLink, 0x41, [(preset, 2), (link, 2)]);
field_message!(
    CopyLink,
    0x42,
    [(src_preset, 2), (src_link, 2), (dst_preset, 2)]
);

// -- Sample utilities (0x50, 0x52) ------------------------------------------

field_message!(SampleErase, 0x50, [(sample, 2)]);
no_field_message!(SampleMemoryDefrag, 0x52);

// -- Misc utilities (0x70-0x76) — several are one-shot destroyers -----------

// Destroys whatever preset already exists at `dst` (spec-stated).
field_message!(PresetCopy, 0x70, [(src, 2), (dst, 2)]);
// DESTRUCTIVE, one-shot, no device-side confirmation. See is_destructive.
field_message!(PresetDelete, 0x71, [(preset, 2)]);

/// 0x72 — 8 bytes/channel (preset, volume, pan, submix), 16 or 32 channels.
#[derive(Debug, Clone, PartialEq)]
pub struct MultimodeMapDump {
    pub raw: Vec<u8>,
    pub device_id: u8,
}

impl MultimodeMapDump {
    pub fn encode(&self) -> Vec<u8> {
        // validated on decode; encode accepts any raw for symmetry
        build_frame(0x72, &self.raw, self.device_id)
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let (device_id, command, payload) = parse_frame(data)?;
        if command != 0x72 {
            return Err(MsgError::Protocol("not a MULTIMODE_MAP_DUMP frame".into()));
        }
        if payload.len() != 128 && payload.len() != 256 {
            return Err(MsgError::Protocol(
                "multimode map must be 128 bytes (16ch) or 256 bytes (32ch)".into(),
            ));
        }
        Ok(Self {
            raw: payload.to_vec(),
            device_id,
        })
    }
    /// Decode into per-channel (preset, volume, pan, submix) tuples.
    pub fn channels(&self) -> Vec<(u32, u32, i32, i32)> {
        self.raw
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| {
                (
                    decode_u14(c[0], c[1]),
                    decode_u14(c[2], c[3]),
                    decode_s14(c[4], c[5]),
                    decode_s14(c[6], c[7]),
                )
            })
            .collect()
    }
}

no_field_message!(MultimodeMapDumpRequest, 0x73);
no_field_message!(EraseRamBank, 0x74); // DESTRUCTIVE, one-shot
no_field_message!(EraseAllRamPresets, 0x75); // DESTRUCTIVE, one-shot
no_field_message!(EraseAllRamSamples, 0x76); // DESTRUCTIVE, one-shot

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u14_roundtrip() {
        for v in [0u32, 1, 127, 128, 0x2000, 0x3FFF] {
            let (a, b) = encode_u14(v).unwrap();
            assert_eq!(decode_u14(a, b), v);
        }
        assert!(encode_u14(0x4000).is_err());
    }

    #[test]
    fn s14_signed_domain() {
        // transpose -24 round-trips through the wire and back as negative
        let (a, b) = encode_s14(-24).unwrap();
        assert_eq!(decode_s14(a, b), -24);
        // E4_VOICE_DELAY max 10000: raw unsigned, must NOT sign-extend
        let (a, b) = encode_14(10000).unwrap();
        assert_eq!(decode_u14(a, b), 10000);
        assert!(encode_s14(8192).is_err());
    }

    #[test]
    fn lsb_bytes_4_byte_count() {
        let v: u64 = (1 << 28) - 1; // 28-bit max
        let enc = encode_lsb_bytes(v, 4).unwrap();
        assert_eq!(enc.len(), 4);
        assert_eq!(decode_lsb_bytes(&enc), v);
        assert!(encode_lsb_bytes(v + 1, 4).is_err());
    }

    #[test]
    fn checksum_is_ones_complement_of_sum() {
        assert_eq!(checksum(&[]), 0x7F);
        assert_eq!(checksum(&[0x01]), (!1u32) as u8 & 0x7F);
        let data: Vec<u8> = (0u16..256).map(|b| b as u8).collect();
        assert_eq!(
            checksum(&data),
            (!data.iter().map(|b| *b as u32).sum::<u32>()) as u8 & 0x7F
        );
    }

    #[test]
    fn parameter_edit_roundtrip_with_checksum() {
        let m = ParameterEdit {
            values: vec![(223, 100), (224, 0x3FFF), (1, 0x2000)],
            device_id: 17,
            checksum_byte: None,
        };
        let wire = m.encode().unwrap();
        let back = ParameterEdit::decode(&wire).unwrap();
        assert_eq!(back.values, m.values);
        assert_eq!(back.device_id, 17);
        assert!(back.verify().unwrap());
        // corrupt one data byte: checksum must fail
        let mut bad = wire.clone();
        bad[9] ^= 0x40;
        let back_bad = ParameterEdit::decode(&bad).unwrap();
        assert!(!back_bad.verify().unwrap());
    }

    #[test]
    fn parameter_request_roundtrip() {
        let m = ParameterRequest {
            param_ids: vec![1, 223, 4095],
            device_id: 0,
            checksum_byte: None,
        };
        let wire = m.encode().unwrap();
        let back = ParameterRequest::decode(&wire).unwrap();
        assert_eq!(back.param_ids, m.param_ids);
        assert!(back.verify().unwrap());
        let too_many = ParameterRequest {
            param_ids: vec![1; 65],
            device_id: 0,
            checksum_byte: None,
        };
        assert!(too_many.encode().is_err());
    }

    #[test]
    fn name_pad_unpad() {
        let m = PresetName {
            preset: 42,
            name: "BRASS".into(),
            device_id: 0,
        };
        let wire = m.encode().unwrap();
        assert_eq!(wire.len(), 7 + 2 + 16);
        let back = PresetName::decode(&wire).unwrap();
        assert_eq!(back.name, "BRASS"); // trailing spaces trimmed
        assert_eq!(back.preset, 42);
        // over-long names truncate to 16
        let long = PresetName {
            preset: 0,
            name: "0123456789abcdefXYZ".into(),
            device_id: 0,
        };
        let back = PresetName::decode(&long.encode().unwrap()).unwrap();
        assert_eq!(back.name, "0123456789abcdef");
        assert!(PresetName {
            preset: 0,
            name: "bröt".into(),
            device_id: 0
        }
        .encode()
        .is_err());
    }

    #[test]
    fn old_and_new_dump_roundtrip() {
        let h = OldDumpHeader {
            byte_count: 69632,
            packet_number: 0,
            device_id: 0,
        };
        let back = OldDumpHeader::decode(&h.encode().unwrap()).unwrap();
        assert_eq!(back.byte_count, 69632);

        let payload = vec![0x7Fu8; 256];
        let d = OldDumpMessage {
            packet_number: 127,
            data: payload.clone(),
            device_id: 0,
            checksum_byte: None,
        };
        let back = OldDumpMessage::decode(&d.encode().unwrap()).unwrap();
        assert_eq!(back.data, payload);
        assert!(back.verify_checksum());
        assert!(OldDumpMessage {
            packet_number: 0,
            data: vec![0; 257],
            device_id: 0,
            checksum_byte: None
        }
        .encode()
        .is_err());

        let nh = NewDumpHeader {
            preset: 9,
            total_bytes: 70000,
            num_global_params: 40,
            num_link_params: 12,
            num_voice_params: 60,
            num_zone_params: 30,
            device_id: 0,
        };
        let back = NewDumpHeader::decode(&nh.encode().unwrap()).unwrap();
        assert_eq!(back, nh);

        let nd = NewDumpMessage {
            packet_number: 16383,
            data: vec![1, 2, 3],
            device_id: 0,
            checksum_byte: None,
        };
        let back = NewDumpMessage::decode(&nd.encode().unwrap()).unwrap();
        assert_eq!(back.packet_number, nd.packet_number);
        assert_eq!(back.data, nd.data);
        assert!(back.verify());
        assert!(NewDumpMessage {
            packet_number: 0,
            data: vec![0; 245],
            device_id: 0,
            checksum_byte: None
        }
        .encode()
        .is_err());
    }

    #[test]
    fn handshake_roundtrips() {
        let a = Ack {
            packet_number: 9,
            device_id: 3,
        };
        assert_eq!(Ack::decode(&a.encode()).unwrap(), a);
        let na = NewAck {
            packet_number: 1000,
            device_id: 3,
        };
        assert_eq!(NewAck::decode(&na.encode().unwrap()).unwrap(), na);
        let nn = NewNak {
            packet_number: 1000,
            device_id: 3,
        };
        assert_eq!(NewNak::decode(&nn.encode().unwrap()).unwrap(), nn);
        let w = Wait { device_id: 0 };
        assert_eq!(Wait::decode(&w.encode()).unwrap(), w);
        // wrong command rejects
        assert!(Ack::decode(
            &Nak {
                packet_number: 1,
                device_id: 0
            }
            .encode()
        )
        .is_err());
    }

    #[test]
    fn config_and_queries() {
        let c = ConfigurationResponse {
            options: 0x1F,
            ram_mb: 64,
            device_id: 0,
        };
        let back = ConfigurationResponse::decode(&c.encode().unwrap()).unwrap();
        assert_eq!(
            back.option_flags(),
            ConfigOptions {
                voices_128: true,
                fx_card: true,
                midi_card: true,
                octopus_card: true,
                digital_io: true
            }
        );

        let e = ExtendedConfigurationResponse {
            options1: 0x60,
            options2: 0,
            ram_mb: 128,
            rom_mb: 128,
            flash_mb: 64,
            device_id: 0,
        };
        let back = ExtendedConfigurationResponse::decode(&e.encode().unwrap()).unwrap();
        assert_eq!(back.ram_mb, 128);
        assert_eq!(back.rom_mb, 128); // 0x7F byte means 128
        assert!(back.option_flags().adat_io);

        let r = VoiceNumSZonesResponse {
            num_szones: 8,
            device_id: 0,
        };
        assert_eq!(VoiceNumSZonesResponse::decode(&r.encode()).unwrap(), r);
    }

    #[test]
    fn field_widths_asymmetry_preserved() {
        // CopyVoice group: 1 raw byte; Combine group: u14 pair.
        let cv = CopyVoice {
            src_preset: 1,
            src_voice: 2,
            dst_preset: 3,
            group: 31,
            device_id: 0,
        };
        let wire = cv.encode();
        assert_eq!(wire.len(), 7 + 7); // 3*u14 + 1 byte
        assert_eq!(CopyVoice::decode(&wire).unwrap(), cv);
        let cb = Combine {
            preset: 1,
            group: 31,
            device_id: 0,
        };
        let wire = cb.encode();
        assert_eq!(wire.len(), 7 + 4);
        assert_eq!(Combine::decode(&wire).unwrap(), cb);
    }

    #[test]
    fn multimode_map() {
        let raw = vec![0x40u8; 128];
        let m = MultimodeMapDump {
            raw: raw.clone(),
            device_id: 0,
        };
        let back = MultimodeMapDump::decode(&m.encode()).unwrap();
        assert_eq!(back.channels().len(), 16);
        assert_eq!(back.channels()[0], (0x2040, 0x2040, -8128, -8128)); // u14(0x40,0x40)=0x2040; s14 wraps negative
        assert!(MultimodeMapDump::decode(
            &MultimodeMapDump {
                raw: vec![0; 100],
                device_id: 0
            }
            .encode()
        )
        .is_err());
    }

    #[test]
    fn destructive_predicate() {
        assert!(is_destructive(0x71)); // PRESET_DELETE
        assert!(is_destructive(0x76)); // ERASE_ALL_RAM_SAMPLES
        assert!(!is_destructive(0x70)); // PRESET_COPY is spec-destructive at dst but not in the one-shot set
        assert!(!is_destructive(0x01));
    }

    #[test]
    fn device_inquiry() {
        let req = build_device_inquiry_request(BROADCAST_DEVICE_ID);
        assert_eq!(req, vec![0xF0, 0x7E, 0x7F, 0x06, 0x01, 0xF7]);
        let mut reply = vec![
            0xF0, 0x7E, 0x10, 0x06, 0x02, 0x18, 0x01, 0x04, 0x04, 0x05, b'4', b'.', b'7', b'0',
            0x00, 0xF7,
        ];
        let r = parse_device_inquiry_reply(&reply).unwrap();
        assert_eq!(r.model(), Some("E4XT"));
        assert_eq!(r.revision, "4.70");
        reply[8] = 0x07;
        assert_eq!(
            parse_device_inquiry_reply(&reply).unwrap().model(),
            Some("E4XT Ultra")
        );
    }
}
