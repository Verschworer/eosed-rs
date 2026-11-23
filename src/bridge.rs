// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: Copyright (C) 2026  eosed contributors
//
// Rust port of eos/bridge.py — MIDI transport and high-level operations for
// the EOS editor protocol. `eos.messages` is the whole protocol layer; this
// module adds MIDI I/O sequencing around it.
//
// The Python binds directly to rtmidi; here the device is behind the
// [`MidiIo`] trait so the engine is testable against a scripted fake (and a
// real backend — midir/coremidi — can implement the same three methods).
//
// **Throttle default is a conservative guess, not an RE'd value.** No
// reverse-engineered safe gap exists yet for EOS/E4 over this protocol;
// do not treat SEND_GAP as a confirmed safe value.
//
// **Dump engine ACK/EOF ordering is inferred, not verified**: the requester
// ACKs each data packet as it arrives, and the device sends EOF once done.
// See docs/RESOLUTION_NOTES.md before relying on this against real hardware.

use crate::messages as m;
use crate::params;
use std::time::{Duration, Instant};

// --- defaults -----------------------------------------------------------

pub const SEND_GAP: Duration = Duration::from_millis(50);
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error(transparent)]
    Msg(#[from] m::MsgError),
    #[error("no SysEx reply within timeout")]
    Timeout,
    #[error("{0}")]
    DeviceCancelled(String),
    #[error("{0}")]
    DumpChecksum(String),
    #[error("{0}")]
    Protocol(String),
    #[error("{0}")]
    Ambiguous(String),
    #[error("{0}")]
    NotFound(String),
}

impl BridgeError {
    /// Python-side exception class name, so CLI messages match the twin.
    pub fn error_class(&self) -> &'static str {
        match self {
            BridgeError::Msg(_) => "ValueError",
            BridgeError::Timeout => "TimeoutError",
            BridgeError::DeviceCancelled(_) => "DeviceCancelled",
            BridgeError::DumpChecksum(_) => "DumpChecksumError",
            BridgeError::Protocol(_) => "RuntimeError",
            BridgeError::Ambiguous(_) => "AmbiguousDevice",
            BridgeError::NotFound(_) => "LookupError",
        }
    }
}

pub type Result<T> = std::result::Result<T, BridgeError>;

/// Sign-extend a parameter value read off the wire, if it is a signed one.
///
/// Values travel as 14-bit two's complement, but the device's *value* replies
/// carry no signedness flag, so decode can only hand back the raw unsigned
/// word — E4_PRESET_TRANSPOSE = -12 arrives as 16372. Which parameters are
/// signed comes from `params`' own table (a negative `minimum`), the same
/// table that supplies every other per-parameter fact. An id absent from the
/// table, or any value without bit 13 set, is passed through untouched —
/// which keeps the undocumented 3FFEh/3FFFh E4_GEN_SAMPLE sentinels intact.
///
/// Confirmed live 2026-07-31 against an E4XT Ultra (rev 4.70): writing -12
/// and -24 to E4_PRESET_TRANSPOSE read back as 16372 and 16360.
pub fn signed_value(param_id: u32, raw: u32) -> i64 {
    if raw & 0x2000 == 0 {
        return raw as i64;
    }
    match params::by_id(param_id) {
        Some(p) if p.min < 0 => raw as i64 - 0x4000,
        _ => raw as i64,
    }
}

/// The three primitives a real MIDI backend must provide.
pub trait MidiIo {
    /// Send one message (whole SysEx frame or channel message).
    fn send(&mut self, message: &[u8]);
    /// Non-blocking poll; None when nothing is waiting.
    fn poll(&mut self) -> Option<Vec<u8>>;
    /// Drop everything already buffered.
    fn drain(&mut self) {
        while self.poll().is_some() {}
    }
}

/// A ParameterRange with the per-parameter signedness applied.
#[derive(Debug, Clone, PartialEq)]
pub struct SignedParameterRange {
    pub param_id: u32,
    pub minimum: i64,
    pub maximum: i64,
    pub default: i64,
    pub device_id: u8,
}

/// A throttling wrapper: SysEx is never flooded, plain MIDI passes through.
/// Two gaps, because the two kinds of send need different protection
/// (measured live, RESOLUTION_NOTES §19b): a request is followed by a
/// blocking wait so the round trip already paces it; a fire-and-forget WRITE
/// has nothing but this gap, and its failure mode is silent.
///
/// The gap is applied as time owed *after* a send — how long the device gets
/// to digest what it was just handed.
pub struct ThrottledOut<T: MidiIo> {
    inner: T,
    gap: Duration,
    write_gap: Duration,
    last: Option<Instant>,
    owed: Duration,
}

impl<T: MidiIo> ThrottledOut<T> {
    pub fn new(inner: T, gap: Duration, write_gap: Option<Duration>) -> Self {
        Self {
            inner,
            gap,
            write_gap: write_gap.unwrap_or(gap),
            last: None,
            owed: Duration::ZERO,
        }
    }
    pub fn send_message(&mut self, message: &[u8], write: bool) {
        let is_sysex = !message.is_empty() && message[0] == 0xF0;
        if is_sysex {
            if let Some(last) = self.last {
                let elapsed = last.elapsed();
                if elapsed < self.owed {
                    std::thread::sleep(self.owed - elapsed);
                }
            }
            self.inner.send(message);
            self.last = Some(Instant::now());
            self.owed = if write { self.write_gap } else { self.gap };
        } else {
            self.inner.send(message);
        }
    }
    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

/// An EOS connection plus the editor-protocol operations. Every high-level
/// method sends one request and waits for exactly the reply it expects;
/// there is no background poller.
pub struct EosBridge<T: MidiIo> {
    pub midi_out: ThrottledOut<T>,
    pub device_id: u8,
    pub timeout: Duration,
    pub inquiry: Option<m::DeviceInquiryReply>,
    pub description: Option<String>,
    basic_channel: Option<i64>,
    /// One-frame pushback: a frame read while looking for the trailing EOF
    /// that turned out not to be EOF is handed back to the next receive.
    pushback: Option<Vec<u8>>,
}

impl<T: MidiIo> EosBridge<T> {
    pub fn new(midi_out: ThrottledOut<T>, device_id: u8, timeout: Duration) -> Self {
        Self {
            midi_out,
            device_id,
            timeout,
            inquiry: None,
            description: None,
            basic_channel: None,
            pushback: None,
        }
    }

    fn send(&mut self, frame: &[u8], write: bool) {
        self.midi_out.send_message(frame, write);
    }

    fn drain(&mut self) {
        self.pushback = None;
        self.midi_out.inner_mut().drain();
    }

    /// Read the EOF the device sends after the last data packet. A dump loop
    /// that exits on its byte count rather than on the EOF leaves that frame
    /// in the input queue; the NEXT exchange would read it as the answer to
    /// its own first question ("unexpected 0x7b while waiting on the dump
    /// header" — found the hard way).
    pub fn consume_trailing_eof(&mut self, timeout: Duration) {
        if let Ok(frame) = self.receive(Some(timeout)) {
            if let Ok((_, command, _)) = m::parse_frame(&frame) {
                if command != 0x7B {
                    self.pushback = Some(frame); // not ours: return it next
                }
            }
        }
        // timeout ("some firmware may not send it") or parse failure: fine
    }

    /// Block until one SysEx message arrives; Timeout error otherwise.
    /// A frame pushed back by consume_trailing_eof is returned first.
    pub fn receive(&mut self, timeout: Option<Duration>) -> Result<Vec<u8>> {
        if let Some(pushed) = self.pushback.take() {
            return Ok(pushed);
        }
        let deadline = Instant::now() + timeout.unwrap_or(self.timeout);
        loop {
            if let Some(data) = self.midi_out.inner_mut().poll() {
                if !data.is_empty() && data[0] == 0xF0 {
                    return Ok(data);
                }
            }
            if Instant::now() >= deadline {
                return Err(BridgeError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Drain stale input, send `frame`, return the next SysEx reply.
    pub fn send_and_receive(&mut self, frame: &[u8], timeout: Option<Duration>) -> Result<Vec<u8>> {
        self.drain();
        self.send(frame, false);
        self.receive(timeout)
    }

    // -- device identity ---------------------------------------------------

    pub fn inquire(&mut self, timeout: Option<Duration>) -> Result<m::DeviceInquiryReply> {
        let reply =
            self.send_and_receive(&m::build_device_inquiry_request(self.device_id), timeout)?;
        let parsed = m::parse_device_inquiry_reply(&reply)?;
        self.inquiry = Some(parsed.clone());
        Ok(parsed)
    }

    // -- parameters ----------------------------------------------------------

    /// Current value of one parameter, sign-extended for signed parameters.
    pub fn get_parameter(&mut self, param_id: u32, timeout: Option<Duration>) -> Result<i64> {
        let req = m::ParameterRequest {
            param_ids: vec![param_id],
            device_id: self.device_id,
            checksum_byte: None,
        };
        let reply = self.send_and_receive(&req.encode()?, timeout)?;
        let edit = m::ParameterEdit::decode(&reply)?;
        for (pid, value) in edit.values {
            if pid == param_id {
                return Ok(signed_value(pid, value));
            }
        }
        Err(BridgeError::Protocol(format!(
            "reply did not include parameter {param_id}"
        )))
    }

    /// The device's own min/max/default — authoritative over `params`'
    /// transcribed range. Sign-extended through the same `signed_value` the
    /// value read uses, so a parameter's range and its current value can
    /// never disagree about signedness. (Python reused the raw `ParameterRange`
    /// for the signed result; here the signedness shows in the type.)
    pub fn get_parameter_range(
        &mut self,
        param_id: u32,
        timeout: Option<Duration>,
    ) -> Result<SignedParameterRange> {
        let req = m::ParameterRangeRequest {
            param_id,
            device_id: self.device_id,
        };
        let reply = self.send_and_receive(&req.encode(), timeout)?;
        let raw = m::ParameterRange::decode(&reply)?;
        Ok(SignedParameterRange {
            param_id: raw.param_id,
            minimum: signed_value(param_id, raw.minimum),
            maximum: signed_value(param_id, raw.maximum),
            default: signed_value(param_id, raw.default),
            device_id: raw.device_id,
        })
    }

    /// Several parameters in as few round trips as the spec allows. The
    /// response is "a complete Parameter Value Edit message for each
    /// parameter" — one reply frame per requested id, not one combined
    /// frame — so this loops until every id in the chunk has been seen.
    pub fn get_parameters(
        &mut self,
        param_ids: &[u32],
        timeout: Option<Duration>,
    ) -> Result<std::collections::HashMap<u32, i64>> {
        let mut values = std::collections::HashMap::new();
        for chunk in param_ids.chunks(m::MAX_PARAMETER_REQUESTS) {
            let req = m::ParameterRequest {
                param_ids: chunk.to_vec(),
                device_id: self.device_id,
                checksum_byte: None,
            };
            self.drain();
            self.send(&req.encode()?, false);
            let mut remaining: std::collections::HashSet<u32> = chunk.iter().copied().collect();
            while !remaining.is_empty() {
                let frame = self.receive(timeout)?;
                let edit = m::ParameterEdit::decode(&frame)?;
                for (pid, value) in edit.values {
                    if remaining.remove(&pid) {
                        values.insert(pid, signed_value(pid, value));
                    }
                }
            }
        }
        Ok(values)
    }

    /// Write one parameter. Fire-and-forget — no reply defined.
    pub fn set_parameter(&mut self, param_id: u32, value: i64) -> Result<()> {
        let edit = m::ParameterEdit {
            values: vec![(param_id, (value as u32) & 0x3FFF)],
            device_id: self.device_id,
            checksum_byte: None,
        };
        self.send(&edit.encode()?, true);
        Ok(())
    }

    /// Write several pairs in as few messages as the spec allows.
    pub fn set_parameters(&mut self, values: &[(u32, i64)]) -> Result<()> {
        for chunk in values.chunks(m::MAX_PARAMETER_EDITS) {
            let edit = m::ParameterEdit {
                values: chunk
                    .iter()
                    .map(|(pid, val)| (*pid, (*val as u32) & 0x3FFF))
                    .collect(),
                device_id: self.device_id,
                checksum_byte: None,
            };
            self.send(&edit.encode()?, true);
        }
        Ok(())
    }

    // -- naming ----------------------------------------------------------

    pub fn get_preset_name(&mut self, preset: u32, timeout: Option<Duration>) -> Result<String> {
        let req = m::PresetNameRequest {
            preset,
            device_id: self.device_id,
        };
        let reply = self.send_and_receive(&req.encode(), timeout)?;
        Ok(m::PresetName::decode(&reply)?.name)
    }

    pub fn set_preset_name(&mut self, preset: u32, name: &str) -> Result<()> {
        let msg = m::PresetName {
            preset,
            name: name.to_string(),
            device_id: self.device_id,
        };
        self.send(&msg.encode()?, true);
        Ok(())
    }

    pub fn get_sample_name(&mut self, sample: u32, timeout: Option<Duration>) -> Result<String> {
        let req = m::SampleNameRequest {
            sample,
            device_id: self.device_id,
        };
        let reply = self.send_and_receive(&req.encode(), timeout)?;
        Ok(m::SampleName::decode(&reply)?.name)
    }

    pub fn set_sample_name(&mut self, sample: u32, name: &str) -> Result<()> {
        let msg = m::SampleName {
            sample,
            name: name.to_string(),
            device_id: self.device_id,
        };
        self.send(&msg.encode()?, true);
        Ok(())
    }

    // -- memory / configuration ---------------------------------------------

    pub fn preset_memory(&mut self, timeout: Option<Duration>) -> Result<m::PresetMemoryResponse> {
        let req = m::PresetMemoryRequest {
            device_id: self.device_id,
        };
        Ok(m::PresetMemoryResponse::decode(
            &self.send_and_receive(&req.encode(), timeout)?,
        )?)
    }

    pub fn sample_memory(&mut self, timeout: Option<Duration>) -> Result<m::SampleMemoryResponse> {
        let req = m::SampleMemoryRequest {
            device_id: self.device_id,
        };
        Ok(m::SampleMemoryResponse::decode(
            &self.send_and_receive(&req.encode(), timeout)?,
        )?)
    }

    pub fn configuration(&mut self, timeout: Option<Duration>) -> Result<m::ConfigurationResponse> {
        let req = m::ConfigurationRequest {
            device_id: self.device_id,
        };
        Ok(m::ConfigurationResponse::decode(
            &self.send_and_receive(&req.encode(), timeout)?,
        )?)
    }

    pub fn extended_configuration(
        &mut self,
        timeout: Option<Duration>,
    ) -> Result<m::ExtendedConfigurationResponse> {
        let req = m::ExtendedConfigurationRequest {
            device_id: self.device_id,
        };
        Ok(m::ExtendedConfigurationResponse::decode(
            &self.send_and_receive(&req.encode(), timeout)?,
        )?)
    }

    // The "Preset Num Of X" siblings (0x16-0x1B) do NOT all behave alike
    // despite sharing a command family — live checks showed preset_num_voices
    // and voice_num_szones are unreliable in preset/voice-dependent ways with
    // no consistent formula (docs/RESOLUTION_NOTES.md §11, §12). Raw
    // passthrough kept for API completeness, uncorrected, exactly like the
    // Python original; callers bound loops by walking indices, not counts.
    pub fn preset_num_voices(&mut self, preset: u32, timeout: Option<Duration>) -> Result<u32> {
        let req = m::PresetNumVoicesRequest {
            preset,
            device_id: self.device_id,
        };
        Ok(
            m::PresetNumVoicesResponse::decode(&self.send_and_receive(&req.encode(), timeout)?)?
                .num_voices,
        )
    }

    pub fn preset_num_links(&mut self, preset: u32, timeout: Option<Duration>) -> Result<u32> {
        let req = m::PresetNumLinksRequest {
            preset,
            device_id: self.device_id,
        };
        Ok(
            m::PresetNumLinksResponse::decode(&self.send_and_receive(&req.encode(), timeout)?)?
                .num_links,
        )
    }

    pub fn preset_num_szones(&mut self, preset: u32, timeout: Option<Duration>) -> Result<u32> {
        let req = m::PresetNumSZonesRequest {
            preset,
            device_id: self.device_id,
        };
        Ok(
            m::PresetNumSZonesResponse::decode(&self.send_and_receive(&req.encode(), timeout)?)?
                .num_szones,
        )
    }

    pub fn voice_num_szones(
        &mut self,
        preset: u32,
        voice: u32,
        timeout: Option<Duration>,
    ) -> Result<u32> {
        let req = m::VoiceNumSZonesRequest {
            preset,
            voice,
            device_id: self.device_id,
        };
        Ok(
            m::VoiceNumSZonesResponse::decode(&self.send_and_receive(&req.encode(), timeout)?)?
                .num_szones,
        )
    }

    // -- catalog (best-effort scan; the spec has no "list all presets") ----
    // A TimeoutError or a CANCEL frame both mean "nothing there": skipped.
    pub fn catalog_presets(
        &mut self,
        range: (u32, u32),
        timeout: Option<Duration>,
        on_progress: Option<&dyn Fn(u32)>,
    ) -> std::collections::HashMap<u32, String> {
        let mut names = std::collections::HashMap::new();
        for preset in range.0..range.1 {
            if let Some(f) = on_progress {
                f(preset);
            }
            let req = m::PresetNameRequest {
                preset,
                device_id: self.device_id,
            };
            let reply = match self.send_and_receive(&req.encode(), timeout) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if let Ok(n) = m::PresetName::decode(&reply) {
                names.insert(preset, n.name);
            }
        }
        names
    }

    pub fn catalog_samples(
        &mut self,
        range: (u32, u32),
        timeout: Option<Duration>,
        on_progress: Option<&dyn Fn(u32)>,
    ) -> std::collections::HashMap<u32, String> {
        let mut names = std::collections::HashMap::new();
        for sample in range.0..range.1 {
            if let Some(f) = on_progress {
                f(sample);
            }
            let req = m::SampleNameRequest {
                sample,
                device_id: self.device_id,
            };
            let reply = match self.send_and_receive(&req.encode(), timeout) {
                Ok(r) => r,
                Err(_) => continue,
            };
            if let Ok(n) = m::SampleName::decode(&reply) {
                names.insert(sample, n.name);
            }
        }
        names
    }

    // -- preset dump (OLD format) ------------------------------------------

    /// OLD-format single-preset dump: the raw concatenated data bytes.
    /// Raises DeviceCancelled if the preset does not exist, DumpChecksum if
    /// a packet's checksum keeps failing after `max_retries` NAKs.
    pub fn dump_preset_old(
        &mut self,
        preset: u32,
        timeout: Option<Duration>,
        max_retries: usize,
    ) -> Result<Vec<u8>> {
        let request = m::PresetDumpRequest {
            preset,
            device_id: self.device_id,
        };
        let reply = self.send_and_receive(&request.encode(), timeout)?;
        let (_, command, _) = m::parse_frame(&reply)?;
        if command == 0x7D {
            return Err(BridgeError::DeviceCancelled(format!(
                "preset {preset} does not exist on the device"
            )));
        }
        let header = m::OldDumpHeader::decode(&reply)?;
        // The header is "the first packet" (number 0); confirmed live
        // 2026-07-27 (RESOLUTION_NOTES §7): the device waits for this ACK.
        self.send(
            &m::Ack {
                packet_number: header.packet_number,
                device_id: self.device_id,
            }
            .encode(),
            false,
        );

        let mut data: Vec<u8> = Vec::new();
        while (data.len() as u64) < header.byte_count {
            let frame = self.receive(timeout)?;
            let (_, command, _) = m::parse_frame(&frame)?;
            if command == 0x7B {
                break;
            }
            let mut message = m::OldDumpMessage::decode(&frame)?;
            if !message.verify_checksum() {
                let mut recovered = false;
                for _ in 0..max_retries {
                    self.send(
                        &m::Nak {
                            packet_number: message.packet_number,
                            device_id: self.device_id,
                        }
                        .encode(),
                        false,
                    );
                    let frame = self.receive(timeout)?;
                    // A device may answer a NAK by giving up rather than
                    // resending — decoding that as a data message would read
                    // like a codec bug rather than "the device ended it".
                    let (_, retry_command, _) = m::parse_frame(&frame)?;
                    if retry_command == 0x7B {
                        data.truncate(header.byte_count as usize);
                        return Ok(data);
                    }
                    if retry_command == 0x7D {
                        return Err(BridgeError::DeviceCancelled(format!(
                            "device cancelled the dump of preset {preset} after a NAK"
                        )));
                    }
                    message = m::OldDumpMessage::decode(&frame)?;
                    if message.verify_checksum() {
                        recovered = true;
                        break;
                    }
                }
                if !recovered {
                    return Err(BridgeError::DumpChecksum(format!(
                        "packet {} failed checksum after {max_retries} retries",
                        message.packet_number
                    )));
                }
            }
            data.extend_from_slice(&message.data);
            self.send(
                &m::Ack {
                    packet_number: message.packet_number,
                    device_id: self.device_id,
                }
                .encode(),
                false,
            );
        }

        // Trim to the count the header promised: nothing stops a device from
        // padding the final packet to a full 256, and silently returning the
        // padding would shift every offset a caller computes into the tail.
        self.consume_trailing_eof(Duration::from_millis(500));
        data.truncate(header.byte_count as usize);
        Ok(data)
    }

    // -- preset dump (NEW format) ------------------------------------------

    pub fn dump_preset_new(
        &mut self,
        preset: u32,
        timeout: Option<Duration>,
        max_retries: usize,
    ) -> Result<(m::NewDumpHeader, Vec<u8>)> {
        let request = m::NewDumpRequest {
            preset,
            device_id: self.device_id,
        };
        let reply = self.send_and_receive(&request.encode()?, timeout)?;
        let (_, command, _) = m::parse_frame(&reply)?;
        if command == 0x7D {
            return Err(BridgeError::DeviceCancelled(format!(
                "preset {preset} does not exist on the device"
            )));
        }
        let header = m::NewDumpHeader::decode(&reply)?;
        // Extrapolated from the OLD-format finding; NOT confirmed live for
        // the NEW format (its header carries no packet-number field).
        self.send(
            &m::NewAck {
                packet_number: 0,
                device_id: self.device_id,
            }
            .encode()?,
            false,
        );

        let mut data: Vec<u8> = Vec::new();
        while (data.len() as u64) < header.total_bytes {
            let frame = self.receive(timeout)?;
            let (_, command, _) = m::parse_frame(&frame)?;
            if command == 0x7B {
                break;
            }
            let mut message = m::NewDumpMessage::decode(&frame)?;
            if !message.verify() {
                let mut recovered = false;
                for _ in 0..max_retries {
                    self.send(
                        &m::NewNak {
                            packet_number: message.packet_number,
                            device_id: self.device_id,
                        }
                        .encode()?,
                        false,
                    );
                    let frame = self.receive(timeout)?;
                    let (_, retry_command, _) = m::parse_frame(&frame)?;
                    if retry_command == 0x7B {
                        data.truncate(header.total_bytes as usize);
                        return Ok((header, data));
                    }
                    if retry_command == 0x7D {
                        return Err(BridgeError::DeviceCancelled(format!(
                            "device cancelled the dump of preset {preset} after a NAK"
                        )));
                    }
                    message = m::NewDumpMessage::decode(&frame)?;
                    if message.verify() {
                        recovered = true;
                        break;
                    }
                }
                if !recovered {
                    return Err(BridgeError::DumpChecksum(format!(
                        "packet {} failed checksum after {max_retries} retries",
                        message.packet_number
                    )));
                }
            }
            data.extend_from_slice(&message.data);
            self.send(
                &m::NewAck {
                    packet_number: message.packet_number,
                    device_id: self.device_id,
                }
                .encode()?,
                false,
            );
        }
        self.consume_trailing_eof(Duration::from_millis(500));
        data.truncate(header.total_bytes as usize);
        Ok((header, data))
    }

    // -- preset SEND (OLD format) -------------------------------------------

    /// The preset number an OLD-format dump will land in: the dump's own
    /// first field. A dump sent unmodified overwrites the preset it came from.
    pub fn dump_target(data: &[u8]) -> u32 {
        m::decode_u14(data[0], data[1])
    }

    /// `data` with its destination preset number replaced.
    pub fn retarget_dump(data: &[u8], preset: u32) -> Vec<u8> {
        let (a, b) = m::encode_u14(preset).expect("preset in u14 range");
        let mut out = vec![a, b];
        out.extend_from_slice(&data[2..]);
        out
    }

    /// OLD-format single-preset send. **Overwrites a preset slot outright** —
    /// the one write that destroys a whole preset, so it refuses unless
    /// `allow_write` and callers must put it behind an arm-then-fire flow.
    pub fn send_preset_old(
        &mut self,
        data: &[u8],
        allow_write: bool,
        timeout: Option<Duration>,
        max_retries: usize,
    ) -> Result<u32> {
        if !allow_write {
            return Err(BridgeError::Protocol(
                "send_preset_old overwrites a whole preset slot; pass allow_write=true from an explicit arm-then-fire confirmation".into()));
        }
        if data.len() < 2 {
            return Err(m::MsgError::Protocol(
                "dump data is too short to carry a preset number".into(),
            )
            .into());
        }
        let preset = Self::dump_target(data);

        self.drain();
        self.send(
            &m::OldDumpHeader {
                byte_count: data.len() as u64,
                packet_number: 0,
                device_id: self.device_id,
            }
            .encode()?,
            false,
        );
        self.await_ack(0, timeout, "the dump header")?;

        for (index, chunk) in data.chunks(m::OldDumpMessage::MAX_DATA).enumerate() {
            let packet = ((index + 1) & 0x7F) as u8;
            let frame = m::OldDumpMessage {
                packet_number: packet,
                data: chunk.to_vec(),
                device_id: self.device_id,
                checksum_byte: None,
            }
            .encode()?;
            self.send(&frame, false);
            for attempt in 0..=max_retries {
                match self.await_ack(packet as u32, timeout, &format!("packet {packet}")) {
                    Ok(()) => break,
                    Err(BridgeError::DumpChecksum(_)) => {
                        if attempt == max_retries {
                            return Err(BridgeError::DumpChecksum(format!(
                                "device NAKed packet {packet} {max_retries} times"
                            )));
                        }
                        self.send(&frame, false);
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        // "no response required" — nothing is read back for this one.
        self.send(
            &m::EndOfFile {
                device_id: self.device_id,
            }
            .encode(),
            true,
        );
        Ok(preset)
    }

    /// Block for the device's verdict on one packet. WAIT means "stop sending
    /// until an ACK arrives", so it is not an error and not an ACK — it
    /// restarts the wait. CANCEL aborts. NAK surfaces as a checksum error.
    fn await_ack(
        &mut self,
        packet_number: u32,
        timeout: Option<Duration>,
        what: &str,
    ) -> Result<()> {
        loop {
            let frame = self.receive(timeout)?;
            let (_, command, payload) = m::parse_frame(&frame)?;
            if command == 0x7F {
                if !payload.is_empty() && payload[0] != (packet_number as u8 & 0x7F) {
                    return Err(BridgeError::DumpChecksum(format!(
                        "ACK for packet {}, expected {}",
                        payload[0],
                        packet_number & 0x7F
                    )));
                }
                return Ok(());
            }
            if command == 0x7E {
                return Err(BridgeError::DumpChecksum(format!("device NAKed {what}")));
            }
            if command == 0x7D {
                return Err(BridgeError::DeviceCancelled(format!(
                    "device cancelled the transfer at {what}"
                )));
            }
            if command == 0x7C {
                continue; // keep waiting; the device will ACK when ready
            }
            return Err(BridgeError::Protocol(format!(
                "unexpected {command:#x} while waiting on {what}"
            )));
        }
    }

    // -- destructive utilities (fire-and-forget) -----------------------------
    // Never key-bind to a single keypress; always explicit arm-then-fire.

    pub fn delete_preset(&mut self, preset: u32) {
        self.send(
            &m::PresetDelete {
                preset,
                device_id: self.device_id,
            }
            .encode(),
            true,
        );
    }
    pub fn erase_ram_bank(&mut self) {
        self.send(
            &m::EraseRamBank {
                device_id: self.device_id,
            }
            .encode(),
            true,
        );
    }
    pub fn erase_all_ram_presets(&mut self) {
        self.send(
            &m::EraseAllRamPresets {
                device_id: self.device_id,
            }
            .encode(),
            true,
        );
    }
    pub fn erase_all_ram_samples(&mut self) {
        self.send(
            &m::EraseAllRamSamples {
                device_id: self.device_id,
            }
            .encode(),
            true,
        );
    }

    // -- plain MIDI performance messages (not the editor SysEx protocol) -----

    pub fn basic_channel(&mut self, timeout: Option<Duration>) -> Result<i64> {
        if self.basic_channel.is_none() {
            self.basic_channel = Some(self.get_parameter(198, timeout)?);
        }
        Ok(self.basic_channel.unwrap())
    }

    pub fn forget_basic_channel(&mut self) {
        self.basic_channel = None;
    }

    /// Select `preset` via Bank Select + Program Change — what actually
    /// makes the E4/E4XT select the preset and redraw its own front panel,
    /// the same as a musician switching patches live. PRESET_SELECT (id 223)
    /// is spec-stated to be independent of the front panel's own selection
    /// and never redraws the LCD; a plain Program Change genuinely does.
    ///
    /// Bank MSB always 0; LSB picks the block of 128; Program Change picks
    /// within it. SEND_GAP between each: live-caught — plain channel
    /// messages bypass the SysEx throttle, and sending all three back-to-back
    /// got Bank Select and/or Program Change dropped. The trailing gap is
    /// not redundant: the throttle only tracks SysEx timestamps, so a SysEx
    /// issued right after this would otherwise see a stale `_last`.
    pub fn send_program_change(
        &mut self,
        preset: u32,
        channel: Option<u8>,
        timeout: Option<Duration>,
    ) -> Result<()> {
        if preset > 16383 {
            return Err(BridgeError::Protocol(format!(
                "preset {preset} out of MIDI bank/program range"
            )));
        }
        let channel = match channel {
            Some(c) => c,
            None => self.basic_channel(timeout)? as u8,
        };
        if channel > 15 {
            return Err(BridgeError::Protocol(format!(
                "channel {channel} out of range 0-15"
            )));
        }
        let bank = preset / 128;
        let program = (preset % 128) as u8;
        self.send(&[0xB0 | channel, 0, ((bank >> 7) & 0x7F) as u8], false); // Bank Select MSB
        std::thread::sleep(SEND_GAP);
        self.send(&[0xB0 | channel, 32, (bank & 0x7F) as u8], false); // Bank Select LSB
        std::thread::sleep(SEND_GAP);
        self.send(&[0xC0 | channel, program], false); // Program Change
        std::thread::sleep(SEND_GAP);
        Ok(())
    }
}

// -- transport backends and autodetect ----------------------------------------

pub trait InPort {
    fn poll(&mut self) -> Option<Vec<u8>>;
}

pub trait OutPort {
    fn send(&mut self, message: &[u8]);
}

pub trait Backend {
    fn list_in(&mut self) -> Vec<String>;
    fn list_out(&mut self) -> Vec<String>;
    fn open_in(&mut self, index: usize) -> Result<Box<dyn InPort>>;
    fn open_out(&mut self, index: usize) -> Result<Box<dyn OutPort>>;
    fn open_in_by_name(&mut self, name: &str) -> Result<Box<dyn InPort>> {
        let names = self.list_in();
        match names.iter().position(|n| n == name) {
            Some(i) => self.open_in(i),
            None => Err(BridgeError::Protocol(format!(
                "no input port named {name:?}; have {names:?}"
            ))),
        }
    }
    fn open_out_by_name(&mut self, name: &str) -> Result<Box<dyn OutPort>> {
        let names = self.list_out();
        match names.iter().position(|n| n == name) {
            Some(i) => self.open_out(i),
            None => Err(BridgeError::Protocol(format!(
                "no output port named {name:?}; have {names:?}"
            ))),
        }
    }
}

pub struct Pair {
    out: Box<dyn OutPort>,
    in_port: Box<dyn InPort>,
}
impl MidiIo for Pair {
    fn send(&mut self, message: &[u8]) {
        self.out.send(message);
    }
    fn poll(&mut self) -> Option<Vec<u8>> {
        self.in_port.poll()
    }
}

pub struct MultiIn {
    ports: Vec<Box<dyn InPort>>,
}

impl MultiIn {
    pub fn new(backend: &mut dyn Backend, name: &str, exact: bool) -> Result<MultiIn> {
        let mut ports = Vec::new();
        for (index, port_name) in backend.list_in().iter().enumerate() {
            let matches = if exact {
                port_name == name
            } else {
                port_name.to_lowercase().contains(&name.to_lowercase())
            };
            if matches {
                ports.push(backend.open_in(index)?);
            }
        }
        if ports.is_empty() {
            return Err(BridgeError::Protocol(format!(
                "no input port matching {name:?}"
            )));
        }
        Ok(MultiIn { ports })
    }
}

impl InPort for MultiIn {
    fn poll(&mut self) -> Option<Vec<u8>> {
        for port in &mut self.ports {
            if let Some(m) = port.poll() {
                return Some(m);
            }
        }
        None
    }
}

pub const AUTODETECT_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_INQUIRY_DRAIN: usize = 64; // messages read from one port in one pass
const MAX_INQUIRY_REPLIES: usize = 32; // Device Inquiry replies kept per probe

fn is_inquiry_reply(data: &[u8]) -> bool {
    data.len() >= 15
        && data[0] == 0xF0
        && data[1] == 0x7E
        && data[3] == 0x06
        && data[4] == 0x02
        && data[5] == m::MANUFACTURER_ID
}

fn member_str(code: (u8, u8)) -> String {
    format!("{:02x} {:02x}", code.0, code.1)
}

/// Probe exactly one send/receive pair for a Device Inquiry reply.
pub fn try_port_pair(
    backend: &mut dyn Backend,
    send_name: &str,
    recv_name: &str,
    timeout: Duration,
) -> Option<Vec<u8>> {
    let request = m::build_device_inquiry_request(m::BROADCAST_DEVICE_ID);
    let mut out = backend.open_out_by_name(send_name).ok()?;
    let mut listener = backend.open_in_by_name(recv_name).ok()?;
    while listener.poll().is_some() {} // flush stale input
    out.send(&request);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(data) = listener.poll() {
            if is_inquiry_reply(&data) {
                return Some(data);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

fn connect(
    backend: &mut dyn Backend,
    send_name: &str,
    recv_name: &str,
    reply: &m::DeviceInquiryReply,
    gap: Duration,
    write_gap: Option<Duration>,
    timeout: Duration,
) -> Result<EosBridge<Pair>> {
    let out = backend.open_out_by_name(send_name)?;
    let in_port = MultiIn::new(backend, recv_name, true)?;
    let model = reply
        .model()
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("unknown model {}", member_str(reply.member_code)));
    let mut bridge = EosBridge::new(
        ThrottledOut::new(
            Pair {
                out,
                in_port: Box::new(in_port),
            },
            gap,
            write_gap,
        ),
        reply.device_id,
        timeout,
    );
    bridge.inquiry = Some(reply.clone());
    bridge.description = Some(format!(
        "auto:{send_name} -> {recv_name} ({model} rev {})",
        reply.revision
    ));
    Ok(bridge)
}

/// EosBridge.standard twin: one bidirectional port name, concrete device id.
pub fn standard_bridge(
    backend: &mut dyn Backend,
    port_name: &str,
    gap: Duration,
    write_gap: Option<Duration>,
    device_id: u8,
    timeout: Duration,
) -> Result<EosBridge<Pair>> {
    let out = backend.open_out_by_name(port_name)?;
    let in_port = MultiIn::new(backend, port_name, true)?;
    Ok(EosBridge::new(
        ThrottledOut::new(
            Pair {
                out,
                in_port: Box::new(in_port),
            },
            gap,
            write_gap,
        ),
        device_id,
        timeout,
    ))
}

/// Find an EOS device on any accessible MIDI port via Device Inquiry. Same
/// state machine as the Python original: warm cached port pair tried first;
/// broadcast sweep collecting ALL replies (not the first) so a second
/// machine on the wire is heard; same machine on two inputs collapses by
/// device id; more than one distinct id with no selection refuses.
#[allow(clippy::too_many_arguments)] // mirrors the upstream signature exactly
pub fn autodetect(
    backend: &mut dyn Backend,
    gap: Duration,
    write_gap: Option<Duration>,
    device_id: Option<u8>,
    timeout: Duration,
    config_path: Option<&str>,
    settings: &mut crate::config::Settings,
    on_try: &dyn Fn(&str),
) -> Result<EosBridge<Pair>> {
    if let Some(path) = config_path {
        if let Some((cached_send, cached_recv)) =
            crate::config::load_last_ports(Some(path), settings)
        {
            on_try(&format!("{cached_send} (cached)"));
            if let Some(data) = try_port_pair(backend, &cached_send, &cached_recv, timeout) {
                let reply = m::parse_device_inquiry_reply(&data)?;
                // A cached pair answering with the wrong device falls through
                // to the sweep: the cache remembers ports, machines change.
                if device_id.is_none() || reply.device_id == device_id.unwrap() {
                    crate::config::save_last_ports(
                        &cached_send,
                        &cached_recv,
                        Some(path),
                        settings,
                    );
                    return connect(
                        backend,
                        &cached_send,
                        &cached_recv,
                        &reply,
                        gap,
                        write_gap,
                        timeout,
                    );
                }
            }
        }
    }

    let request = m::build_device_inquiry_request(m::BROADCAST_DEVICE_ID);
    let out_names = backend.list_out();
    let in_names = backend.list_in();
    let mut listeners: Vec<(String, Box<dyn InPort>)> = Vec::new();
    for (index, name) in in_names.iter().enumerate() {
        if let Ok(port) = backend.open_in(index) {
            listeners.push((name.clone(), port));
        }
    }

    let collect_replies = |listeners: &mut [(String, Box<dyn InPort>)],
                           into: &mut Vec<(String, Vec<u8>)>| {
        for (name, port) in listeners.iter_mut() {
            for _ in 0..MAX_INQUIRY_DRAIN {
                match port.poll() {
                    None => break,
                    Some(data) => {
                        if is_inquiry_reply(&data) {
                            if into.len() >= MAX_INQUIRY_REPLIES {
                                return;
                            }
                            into.push((name.clone(), data));
                        }
                    }
                }
            }
        }
    };

    for out_name in &out_names {
        on_try(out_name);
        let mut out = match backend.open_out_by_name(out_name) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let mut seen: Vec<(String, Vec<u8>)> = Vec::new();
        for (_, port) in listeners.iter_mut() {
            while port.poll().is_some() {}
        } // flush stale input
        out.send(&request);
        // Listen for the whole window even after a reply arrives — a second
        // machine on the same wire answers too, and we need to know about it.
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            collect_replies(&mut listeners, &mut seen);
            std::thread::sleep(Duration::from_millis(5));
        }
        collect_replies(&mut listeners, &mut seen);
        if seen.is_empty() {
            continue;
        }

        let mut by_id: std::collections::HashMap<u8, (String, m::DeviceInquiryReply)> =
            std::collections::HashMap::new();
        for (recv_name, data) in seen {
            if let Ok(reply) = m::parse_device_inquiry_reply(&data) {
                by_id.entry(reply.device_id).or_insert((recv_name, reply));
            }
        }
        if by_id.is_empty() {
            continue;
        }

        let (recv_name, reply) = if let Some(wanted) = device_id {
            match by_id.get(&wanted) {
                Some(v) => v.clone(),
                None => continue,
            }
        } else if by_id.len() > 1 {
            let mut devices: Vec<(u8, String, String)> = by_id
                .iter()
                .map(|(did, (port, rep))| {
                    (
                        *did,
                        rep.model()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("member {}", member_str(rep.member_code))),
                        port.clone(),
                    )
                })
                .collect();
            devices.sort();
            let listing: Vec<String> = devices
                .iter()
                .map(|(did, model, port)| format!("  device id {did}: {model} on {port}"))
                .collect();
            return Err(BridgeError::Ambiguous(format!("{} EOS devices answered:\n{}\nPass --device-id N to choose one, or pin the ports explicitly by setting send_port/recv_port in the config file.", devices.len(), listing.join("\n"))));
        } else {
            by_id.values().next().unwrap().clone()
        };

        if let Some(path) = config_path {
            crate::config::save_last_ports(out_name, &recv_name, Some(path), settings);
        }
        return connect(
            backend, out_name, &recv_name, &reply, gap, write_gap, timeout,
        );
    }

    Err(BridgeError::Protocol(format!(
        "auto-probe: no EOS device answered a Device Inquiry on any of {} output ports (listened on {}).",
        out_names.len(), listeners.len()
    )))
}

/// The engine surface shared by the live bridge and the in-memory demo device
/// (the Rust mirror of Python's duck-typed DemoBridge). Every CLI command and
/// the TUI speak through this, so `--demo` exercises the exact same code paths.
pub trait BridgeLike {
    fn inquire(&mut self, timeout: Option<Duration>) -> Result<m::DeviceInquiryReply>;
    fn configuration(&mut self, timeout: Option<Duration>) -> Result<m::ConfigurationResponse>;
    fn extended_configuration(
        &mut self,
        timeout: Option<Duration>,
    ) -> Result<m::ExtendedConfigurationResponse>;
    fn preset_memory(&mut self, timeout: Option<Duration>) -> Result<m::PresetMemoryResponse>;
    fn sample_memory(&mut self, timeout: Option<Duration>) -> Result<m::SampleMemoryResponse>;
    fn preset_num_voices(&mut self, preset: u32, timeout: Option<Duration>) -> Result<u32>;
    fn preset_num_links(&mut self, preset: u32, timeout: Option<Duration>) -> Result<u32>;
    fn preset_num_szones(&mut self, preset: u32, timeout: Option<Duration>) -> Result<u32>;
    fn voice_num_szones(
        &mut self,
        preset: u32,
        voice: u32,
        timeout: Option<Duration>,
    ) -> Result<u32>;
    fn get_parameter(&mut self, param_id: u32, timeout: Option<Duration>) -> Result<i64>;
    fn get_parameter_range(
        &mut self,
        param_id: u32,
        timeout: Option<Duration>,
    ) -> Result<SignedParameterRange>;
    fn get_parameters(
        &mut self,
        param_ids: &[u32],
        timeout: Option<Duration>,
    ) -> Result<std::collections::HashMap<u32, i64>>;
    fn set_parameter(&mut self, param_id: u32, value: i64) -> Result<()>;
    fn get_preset_name(&mut self, preset: u32, timeout: Option<Duration>) -> Result<String>;
    fn set_preset_name(&mut self, preset: u32, name: &str) -> Result<()>;
    fn get_sample_name(&mut self, sample: u32, timeout: Option<Duration>) -> Result<String>;
    fn set_sample_name(&mut self, sample: u32, name: &str) -> Result<()>;
    fn catalog_presets(
        &mut self,
        range: (u32, u32),
        timeout: Option<Duration>,
        on_progress: Option<&dyn Fn(u32)>,
    ) -> std::collections::HashMap<u32, String>;
    fn catalog_samples(
        &mut self,
        range: (u32, u32),
        timeout: Option<Duration>,
        on_progress: Option<&dyn Fn(u32)>,
    ) -> std::collections::HashMap<u32, String>;
    fn dump_preset_old(
        &mut self,
        preset: u32,
        timeout: Option<Duration>,
        max_retries: usize,
    ) -> Result<Vec<u8>>;
    fn dump_preset_new(
        &mut self,
        preset: u32,
        timeout: Option<Duration>,
        max_retries: usize,
    ) -> Result<(m::NewDumpHeader, Vec<u8>)>;
    fn send_preset_old(
        &mut self,
        data: &[u8],
        allow_write: bool,
        timeout: Option<Duration>,
        max_retries: usize,
    ) -> Result<u32>;
    fn delete_preset(&mut self, preset: u32);
    fn erase_ram_bank(&mut self);
    fn erase_all_ram_presets(&mut self);
    fn erase_all_ram_samples(&mut self);
    fn send_program_change(
        &mut self,
        preset: u32,
        channel: Option<u8>,
        timeout: Option<Duration>,
    ) -> Result<()>;
}

impl<T: MidiIo> BridgeLike for EosBridge<T> {
    fn inquire(&mut self, timeout: Option<Duration>) -> Result<m::DeviceInquiryReply> {
        self.inquire(timeout)
    }
    fn configuration(&mut self, timeout: Option<Duration>) -> Result<m::ConfigurationResponse> {
        self.configuration(timeout)
    }
    fn extended_configuration(
        &mut self,
        timeout: Option<Duration>,
    ) -> Result<m::ExtendedConfigurationResponse> {
        self.extended_configuration(timeout)
    }
    fn preset_memory(&mut self, timeout: Option<Duration>) -> Result<m::PresetMemoryResponse> {
        self.preset_memory(timeout)
    }
    fn sample_memory(&mut self, timeout: Option<Duration>) -> Result<m::SampleMemoryResponse> {
        self.sample_memory(timeout)
    }
    fn preset_num_voices(&mut self, preset: u32, timeout: Option<Duration>) -> Result<u32> {
        self.preset_num_voices(preset, timeout)
    }
    fn preset_num_links(&mut self, preset: u32, timeout: Option<Duration>) -> Result<u32> {
        self.preset_num_links(preset, timeout)
    }
    fn preset_num_szones(&mut self, preset: u32, timeout: Option<Duration>) -> Result<u32> {
        self.preset_num_szones(preset, timeout)
    }
    fn voice_num_szones(
        &mut self,
        preset: u32,
        voice: u32,
        timeout: Option<Duration>,
    ) -> Result<u32> {
        self.voice_num_szones(preset, voice, timeout)
    }
    fn get_parameter(&mut self, param_id: u32, timeout: Option<Duration>) -> Result<i64> {
        self.get_parameter(param_id, timeout)
    }
    fn get_parameter_range(
        &mut self,
        param_id: u32,
        timeout: Option<Duration>,
    ) -> Result<SignedParameterRange> {
        self.get_parameter_range(param_id, timeout)
    }
    fn get_parameters(
        &mut self,
        param_ids: &[u32],
        timeout: Option<Duration>,
    ) -> Result<std::collections::HashMap<u32, i64>> {
        self.get_parameters(param_ids, timeout)
    }
    fn set_parameter(&mut self, param_id: u32, value: i64) -> Result<()> {
        self.set_parameter(param_id, value)
    }
    fn get_preset_name(&mut self, preset: u32, timeout: Option<Duration>) -> Result<String> {
        self.get_preset_name(preset, timeout)
    }
    fn set_preset_name(&mut self, preset: u32, name: &str) -> Result<()> {
        self.set_preset_name(preset, name)
    }
    fn get_sample_name(&mut self, sample: u32, timeout: Option<Duration>) -> Result<String> {
        self.get_sample_name(sample, timeout)
    }
    fn set_sample_name(&mut self, sample: u32, name: &str) -> Result<()> {
        self.set_sample_name(sample, name)
    }
    fn catalog_presets(
        &mut self,
        range: (u32, u32),
        timeout: Option<Duration>,
        on_progress: Option<&dyn Fn(u32)>,
    ) -> std::collections::HashMap<u32, String> {
        self.catalog_presets(range, timeout, on_progress)
    }
    fn catalog_samples(
        &mut self,
        range: (u32, u32),
        timeout: Option<Duration>,
        on_progress: Option<&dyn Fn(u32)>,
    ) -> std::collections::HashMap<u32, String> {
        self.catalog_samples(range, timeout, on_progress)
    }
    fn dump_preset_old(
        &mut self,
        preset: u32,
        timeout: Option<Duration>,
        max_retries: usize,
    ) -> Result<Vec<u8>> {
        self.dump_preset_old(preset, timeout, max_retries)
    }
    fn dump_preset_new(
        &mut self,
        preset: u32,
        timeout: Option<Duration>,
        max_retries: usize,
    ) -> Result<(m::NewDumpHeader, Vec<u8>)> {
        self.dump_preset_new(preset, timeout, max_retries)
    }
    fn send_preset_old(
        &mut self,
        data: &[u8],
        allow_write: bool,
        timeout: Option<Duration>,
        max_retries: usize,
    ) -> Result<u32> {
        self.send_preset_old(data, allow_write, timeout, max_retries)
    }
    fn delete_preset(&mut self, preset: u32) {
        self.delete_preset(preset)
    }
    fn erase_ram_bank(&mut self) {
        self.erase_ram_bank()
    }
    fn erase_all_ram_presets(&mut self) {
        self.erase_all_ram_presets()
    }
    fn erase_all_ram_samples(&mut self) {
        self.erase_all_ram_samples()
    }
    fn send_program_change(
        &mut self,
        preset: u32,
        channel: Option<u8>,
        timeout: Option<Duration>,
    ) -> Result<()> {
        self.send_program_change(preset, channel, timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scripted fake device: replies preloaded; every send is recorded.
    struct Fake {
        /// Scripted device: the next reply is put on the wire after each send.
        scripted: std::collections::VecDeque<Vec<u8>>,
        wire: std::collections::VecDeque<Vec<u8>>,
        sent: Vec<Vec<u8>>,
    }
    impl Fake {
        fn new(replies: Vec<Vec<u8>>) -> Self {
            Self {
                scripted: replies.into(),
                wire: std::collections::VecDeque::new(),
                sent: Vec::new(),
            }
        }
        fn inject(&mut self, frames: Vec<Vec<u8>>) {
            self.wire.extend(frames);
        }
    }
    impl MidiIo for Fake {
        fn send(&mut self, message: &[u8]) {
            self.sent.push(message.to_vec());
            if let Some(reply) = self.scripted.pop_front() {
                self.wire.push_back(reply);
            }
        }
        fn poll(&mut self) -> Option<Vec<u8>> {
            self.wire.pop_front()
        }
    }

    fn bridge(replies: Vec<Vec<u8>>) -> EosBridge<Fake> {
        let fake = Fake::new(replies);
        let out = ThrottledOut::new(fake, Duration::from_millis(1), None);
        EosBridge::new(out, 0, Duration::from_millis(50))
    }

    #[test]
    fn signed_value_table_driven() {
        // E4_PRESET_TRANSPOSE (id 0, min -24): 16372 -> -12
        assert_eq!(signed_value(0, 16372), -12);
        // E4_VOICE_DELAY (id 61, min 0): 10000 stays 10000
        assert_eq!(signed_value(61, 10000), 10000);
        // id 38 E4_GEN_SAMPLE now has min -8 (structure sentinels), so 3FFFh
        // sign-extends to -1 = the "this voice is multisample" sentinel;
        // that is how the app consumes it.
        assert_eq!(signed_value(38, 0x3FFF), -1);
        // 3FFEh -> -2 = "this voice index does not exist"
        assert_eq!(signed_value(38, 0x3FFE), -2);
        // unknown id passes through
        assert_eq!(signed_value(9999, 0x2001), 0x2001);
    }

    #[test]
    fn get_parameter_round_trip() {
        let reply = m::ParameterEdit {
            values: vec![(0, 16372)],
            device_id: 0,
            checksum_byte: None,
        }
        .encode()
        .unwrap();
        let mut b = bridge(vec![reply]);
        assert_eq!(b.get_parameter(0, None).unwrap(), -12);
        // the request went out as a 0x02 frame
        assert_eq!(b.midi_out.inner_mut().sent[0][5], 0x02);
    }

    #[test]
    fn dump_old_checksum_retry_and_trim() {
        // header promises 300 bytes; first packet bad checksum, retry good;
        // device pads the last packet, so the trim matters.
        let dev = 0u8;
        let header = m::OldDumpHeader {
            byte_count: 300,
            packet_number: 0,
            device_id: dev,
        }
        .encode()
        .unwrap();
        let bad = m::OldDumpMessage {
            packet_number: 1,
            data: vec![0u8; 256],
            device_id: dev,
            checksum_byte: Some(0x00),
        }
        .encode()
        .unwrap();
        // force a wrong checksum into the frame above: 0x00 almost surely mismatches
        let good1 = m::OldDumpMessage {
            packet_number: 1,
            data: vec![1u8; 256],
            device_id: dev,
            checksum_byte: None,
        }
        .encode()
        .unwrap();
        let good2 = m::OldDumpMessage {
            packet_number: 2,
            data: vec![2u8; 256],
            device_id: dev,
            checksum_byte: None,
        }
        .encode()
        .unwrap(); // 212 over, trimmed
        let eof = m::EndOfFile { device_id: dev }.encode();
        let mut b = bridge(vec![header, bad, good1, good2, eof]);
        let data = b.dump_preset_old(7, None, 3).unwrap();
        assert_eq!(data.len(), 300); // trimmed from 512
        assert!(data[299] == 2);
        // sends: request, ACK(header), NAK(1), ACK(1), ACK(2), [EOF consumption]
        let cmds: Vec<u8> = b.midi_out.inner_mut().sent.iter().map(|f| f[5]).collect();
        assert_eq!(cmds, vec![0x0E, 0x7F, 0x7E, 0x7F, 0x7F]);
    }

    #[test]
    fn dump_old_cancel_means_absent() {
        let cancel = m::Cancel { device_id: 0 }.encode();
        let mut b = bridge(vec![cancel]);
        match b.dump_preset_old(999, None, 3) {
            Err(BridgeError::DeviceCancelled(_)) => {}
            other => panic!("expected DeviceCancelled, got {other:?}"),
        }
    }

    #[test]
    fn send_old_needs_allow_write_and_drives_handshake() {
        let payload: Vec<u8> = {
            let mut v = vec![0x07, 0x00]; // preset 7 target (u14 LSB first)
            v.extend(std::iter::repeat_n(0x55, 300));
            v
        };
        // device ACKs header (0), packets 1 and 2
        let ack0 = m::Ack {
            packet_number: 0,
            device_id: 0,
        }
        .encode();
        let ack1 = m::Ack {
            packet_number: 1,
            device_id: 0,
        }
        .encode();
        let ack2 = m::Ack {
            packet_number: 2,
            device_id: 0,
        }
        .encode();
        let mut b = bridge(vec![ack0, ack1, ack2]);
        let preset = b.send_preset_old(&payload, true, None, 2).unwrap();
        assert_eq!(preset, 7);
        let cmds: Vec<u8> = b.midi_out.inner_mut().sent.iter().map(|f| f[5]).collect();
        assert_eq!(cmds, vec![0x0D, 0x0D, 0x0D, 0x7B]); // header, pkt1, pkt2, EOF
                                                        // refused without the flag
        let mut b2 = bridge(vec![]);
        assert!(b2.send_preset_old(&payload, false, None, 2).is_err());
    }

    #[test]
    fn await_ack_wait_then_ack() {
        let wait = m::Wait { device_id: 0 }.encode();
        let nak = m::Nak {
            packet_number: 1,
            device_id: 0,
        }
        .encode();
        let ack = m::Ack {
            packet_number: 1,
            device_id: 0,
        }
        .encode();
        let mut b = bridge(vec![]);
        b.midi_out
            .inner_mut()
            .inject(vec![wait.clone(), nak, ack.clone()]);
        // WAIT restarts; NAK surfaces as checksum error
        match b.await_ack(1, None, "p") {
            Err(BridgeError::DumpChecksum(_)) => {}
            other => panic!("expected checksum error, got {other:?}"),
        }
        let mut b = bridge(vec![]);
        b.midi_out.inner_mut().inject(vec![wait, ack]);
        assert!(b.await_ack(1, None, "p").is_ok());
    }

    #[test]
    fn program_change_banks_and_gaps() {
        let mut b = bridge(vec![]);
        b.send_program_change(200, Some(0), None).unwrap(); // bank 1, program 72
        let sent = &b.midi_out.inner_mut().sent;
        assert_eq!(sent[0], vec![0xB0, 0, 0]);
        assert_eq!(sent[1], vec![0xB0, 32, 1]);
        assert_eq!(sent[2], vec![0xC0, 72]);
        assert_eq!(sent.len(), 3);
    }

    #[test]
    fn retarget_dump_rewrites_target_only() {
        let d = vec![0x00, 0x03, 9, 9];
        let r = EosBridge::<Fake>::retarget_dump(&d, 5);
        assert_eq!(r, vec![0x05, 0x00, 9, 9]);
        assert_eq!(EosBridge::<Fake>::dump_target(&r), 5);
    }
}
