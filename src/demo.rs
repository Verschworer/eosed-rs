// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: Copyright (C) 2026  eosed contributors
//
// Rust port of eosed/demo.py: a canned, in-memory EOS device for --demo mode.
// No MIDI port is ever opened. Per project convention (CLAUDE.md hardware
// rule): development and demonstration must be possible with zero hardware.

use std::collections::HashMap;
use std::time::Duration;

use crate::bridge::{BridgeError, BridgeLike, Result, SignedParameterRange};
use crate::messages as m;
use crate::params;

const GEN_SAMPLE_ID: u32 = 38; // E4_GEN_SAMPLE (signed; sign-extended domain)
const VOICE_SELECT_ID: u32 = 225;
const NO_SUCH_VOICE_MARKER: i64 = -2; // -2, not 3FFEh: the bridge sign-extends

fn name_bytes(name: &str) -> Vec<u8> {
    let mut out = name.as_bytes()[..name.len().min(m::NAME_LENGTH)].to_vec();
    out.resize(m::NAME_LENGTH, b' ');
    out
}

/// The starting state is copied per instance — never mutated in place — so
/// two DemoBridges in one process are genuinely independent devices (the
/// Python original had exactly this bug before it was fixed).
pub struct Demo {
    #[allow(dead_code)]
    description: String,
    device_id: u8,
    inquiry: m::DeviceInquiryReply,
    preset_names: HashMap<u32, String>,
    sample_names: HashMap<u32, String>,
    param_values: HashMap<u32, i64>,
}

impl Default for Demo {
    fn default() -> Self {
        Self::new()
    }
}

impl Demo {
    pub fn new() -> Self {
        let mut preset_names = HashMap::new();
        preset_names.insert(0, "Demo Grand Piano".into());
        preset_names.insert(1, "Demo Warm Pad".into());
        preset_names.insert(5, "Demo Bass".into());
        let mut sample_names = HashMap::new();
        sample_names.insert(0, "Demo Kick".into());
        sample_names.insert(1, "Demo Snare".into());
        let mut param_values = HashMap::new();
        param_values.insert(1, 0); // E4_PRESET_VOLUME
        param_values.insert(39, -6); // E4_GEN_VOLUME
        param_values.insert(183, 0); // MASTER_TUNING_OFFSET
        Self {
            description: "demo (no hardware)".into(),
            device_id: m::DEFAULT_DEVICE_ID,
            inquiry: m::DeviceInquiryReply {
                device_id: 0,
                family_code: m::FAMILY_CODE,
                member_code: (0x04, 0x05),
                revision: "4.00".into(),
            },
            preset_names,
            sample_names,
            param_values,
        }
    }

    fn dump_body(&mut self, preset: u32) -> Result<Vec<u8>> {
        let name = match self.preset_names.get(&preset) {
            Some(n) => n.clone(),
            None => {
                return Err(BridgeError::Protocol(format!(
                    "demo has no preset {preset}"
                )))
            }
        };
        let mut body = name_bytes(&name);
        body.extend(std::iter::repeat_n(0u8, 44)); // globals, all zero
        Ok(body)
    }
}

impl BridgeLike for Demo {
    fn inquire(&mut self, _timeout: Option<Duration>) -> Result<m::DeviceInquiryReply> {
        Ok(self.inquiry.clone())
    }

    fn configuration(&mut self, _timeout: Option<Duration>) -> Result<m::ConfigurationResponse> {
        Ok(m::ConfigurationResponse {
            options: 0b00011,
            ram_mb: 32,
            device_id: self.device_id,
        })
    }

    fn extended_configuration(
        &mut self,
        _timeout: Option<Duration>,
    ) -> Result<m::ExtendedConfigurationResponse> {
        Ok(m::ExtendedConfigurationResponse {
            options1: 0b0011111,
            options2: 0,
            ram_mb: 32,
            rom_mb: 16,
            flash_mb: 0,
            device_id: self.device_id,
        })
    }

    fn preset_memory(&mut self, _timeout: Option<Duration>) -> Result<m::PresetMemoryResponse> {
        // Used RAM tracks ACTUAL demo content: real banks measure ~0.5 KB of
        // preset RAM per voice, so a canned device claiming a whole bank's
        // worth would make sweep-time estimates wildly wrong.
        let used_kb = self.preset_names.len().max(1) as u32;
        Ok(m::PresetMemoryResponse {
            total_kb: 8192,
            free_kb: 8192 - used_kb,
            device_id: self.device_id,
        })
    }

    fn sample_memory(&mut self, _timeout: Option<Duration>) -> Result<m::SampleMemoryResponse> {
        Ok(m::SampleMemoryResponse {
            total_mb: 32,
            free_10kb: 2000,
            device_id: self.device_id,
        })
    }

    fn preset_num_voices(&mut self, preset: u32, _timeout: Option<Duration>) -> Result<u32> {
        Ok(if self.preset_names.contains_key(&preset) {
            1
        } else {
            0
        })
    }

    fn preset_num_links(&mut self, _preset: u32, _timeout: Option<Duration>) -> Result<u32> {
        Ok(0)
    }

    fn preset_num_szones(&mut self, preset: u32, _timeout: Option<Duration>) -> Result<u32> {
        Ok(if self.preset_names.contains_key(&preset) {
            1
        } else {
            0
        })
    }

    fn voice_num_szones(
        &mut self,
        _preset: u32,
        _voice: u32,
        _timeout: Option<Duration>,
    ) -> Result<u32> {
        Ok(1)
    }

    fn get_parameter(&mut self, param_id: u32, _timeout: Option<Duration>) -> Result<i64> {
        // Voice-scoped params only exist while voice 0 is selected; every
        // demo preset has exactly one voice, so other voices are "no such
        // voice", answered in the signed domain the real bridge uses.
        if param_id == GEN_SAMPLE_ID
            && self
                .param_values
                .get(&VOICE_SELECT_ID)
                .copied()
                .unwrap_or(0)
                != 0
        {
            return Ok(NO_SUCH_VOICE_MARKER);
        }
        Ok(self.param_values.get(&param_id).copied().unwrap_or(0))
    }

    fn get_parameter_range(
        &mut self,
        param_id: u32,
        _timeout: Option<Duration>,
    ) -> Result<SignedParameterRange> {
        let param = match params::by_id(param_id) {
            Some(p) => p,
            None => {
                return Err(BridgeError::Protocol(format!(
                    "no demo data for parameter {param_id}"
                )))
            }
        };
        Ok(SignedParameterRange {
            param_id,
            minimum: param.min as i64,
            maximum: param.max as i64,
            default: param.default.unwrap_or(0) as i64,
            device_id: self.device_id,
        })
    }

    fn get_parameters(
        &mut self,
        param_ids: &[u32],
        _timeout: Option<Duration>,
    ) -> Result<std::collections::HashMap<u32, i64>> {
        let mut out = std::collections::HashMap::new();
        for &id in param_ids {
            out.insert(id, self.get_parameter(id, None)?);
        }
        Ok(out)
    }

    fn set_parameter(&mut self, param_id: u32, value: i64) -> Result<()> {
        self.param_values.insert(param_id, value);
        Ok(())
    }

    fn get_preset_name(&mut self, preset: u32, _timeout: Option<Duration>) -> Result<String> {
        match self.preset_names.get(&preset) {
            Some(n) => Ok(n.clone()),
            None => Err(BridgeError::NotFound(format!(
                "demo has no preset {preset}"
            ))),
        }
    }

    fn set_preset_name(&mut self, preset: u32, name: &str) -> Result<()> {
        self.preset_names.insert(preset, name.to_string());
        Ok(())
    }

    fn get_sample_name(&mut self, sample: u32, _timeout: Option<Duration>) -> Result<String> {
        match self.sample_names.get(&sample) {
            Some(n) => Ok(n.clone()),
            None => Err(BridgeError::NotFound(format!(
                "demo has no sample {sample}"
            ))),
        }
    }

    fn set_sample_name(&mut self, sample: u32, name: &str) -> Result<()> {
        self.sample_names.insert(sample, name.to_string());
        Ok(())
    }

    fn catalog_presets(
        &mut self,
        range: (u32, u32),
        _timeout: Option<Duration>,
        on_progress: Option<&dyn Fn(u32)>,
    ) -> HashMap<u32, String> {
        let mut result = HashMap::new();
        for number in range.0..range.1 {
            if let Some(f) = on_progress {
                f(number);
            }
            if let Some(n) = self.preset_names.get(&number) {
                result.insert(number, n.clone());
            }
        }
        result
    }

    fn catalog_samples(
        &mut self,
        range: (u32, u32),
        _timeout: Option<Duration>,
        on_progress: Option<&dyn Fn(u32)>,
    ) -> HashMap<u32, String> {
        let mut result = HashMap::new();
        for number in range.0..range.1 {
            if let Some(f) = on_progress {
                f(number);
            }
            if let Some(n) = self.sample_names.get(&number) {
                result.insert(number, n.clone());
            }
        }
        result
    }

    fn dump_preset_old(
        &mut self,
        preset: u32,
        _timeout: Option<Duration>,
        _max_retries: usize,
    ) -> Result<Vec<u8>> {
        // OLD payload carries the preset number: <preset u14><name><globals>.
        // Confirmed live (RESOLUTION_NOTES §7).
        let (lsb, msb) = m::encode_u14(preset)?;
        let mut out = vec![lsb, msb];
        out.extend(self.dump_body(preset)?);
        Ok(out)
    }

    fn dump_preset_new(
        &mut self,
        preset: u32,
        _timeout: Option<Duration>,
        _max_retries: usize,
    ) -> Result<(m::NewDumpHeader, Vec<u8>)> {
        // NEW payload carries NO preset number: it lives in the header.
        let data = self.dump_body(preset)?;
        let header = m::NewDumpHeader {
            preset,
            total_bytes: data.len() as u64,
            num_global_params: 22,
            num_link_params: 0,
            num_voice_params: 0,
            num_zone_params: 0,
            device_id: self.device_id,
        };
        Ok((header, data))
    }

    fn send_preset_old(
        &mut self,
        data: &[u8],
        allow_write: bool,
        _timeout: Option<Duration>,
        _max_retries: usize,
    ) -> Result<u32> {
        // The guard is part of the rehearsal: skipping it rehearses nothing.
        if !allow_write {
            return Err(BridgeError::Protocol(
                "send_preset_old overwrites a whole preset slot; pass allow_write=true from an explicit arm-then-fire confirmation".into(),
            ));
        }
        if data.len() < 18 {
            return Err(BridgeError::Protocol(
                "sent data too short to be a preset dump".into(),
            ));
        }
        let preset = m::decode_u14(data[0], data[1]);
        let name = String::from_utf8_lossy(&data[2..18]).to_string();
        self.preset_names.insert(preset, name);
        Ok(preset)
    }

    fn delete_preset(&mut self, preset: u32) {
        self.preset_names.remove(&preset);
    }

    fn erase_ram_bank(&mut self) {
        self.preset_names.clear();
    }

    fn erase_all_ram_presets(&mut self) {
        self.preset_names.clear();
    }

    fn erase_all_ram_samples(&mut self) {
        self.sample_names.clear();
    }

    fn send_program_change(
        &mut self,
        _preset: u32,
        _channel: Option<u8>,
        _timeout: Option<Duration>,
    ) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_independent_state() {
        let mut a = Demo::new();
        a.erase_all_ram_presets();
        let b = Demo::new();
        assert_eq!(b.preset_names.len(), 3); // erasing in one demo must not leak into another
    }

    #[test]
    fn demo_old_dump_carries_preset_number() {
        let mut d = Demo::new();
        let old = d.dump_preset_old(5, None, 3).unwrap();
        assert_eq!(m::decode_u14(old[0], old[1]), 5);
        assert_eq!(&old[2..18], b"Demo Bass       ");
        assert_eq!(old.len(), 2 + 16 + 44);
        let (_header, new_data) = d.dump_preset_new(5, None, 3).unwrap();
        assert_eq!(&new_data[..16], b"Demo Bass       "); // no number in NEW payload
        assert_eq!(new_data.len(), 60);
    }

    #[test]
    fn demo_send_guard_and_name_memory() {
        let mut d = Demo::new();
        let payload: [u8; 18] = [
            0x02, 0x00, b'X', b'T', b'A', b'S', b'T', b' ', b' ', b' ', b' ', b' ', b' ', b' ',
            b' ', b' ', b' ', b' ',
        ];
        let bad = d.send_preset_old(&payload, false, None, 3);
        assert!(bad.is_err()); // guard enforced even in demo
        let ok = d.send_preset_old(&payload, true, None, 3);
        assert_eq!(ok.unwrap(), 2);
        assert_eq!(d.get_preset_name(2, None).unwrap(), "XTAST           ");
    }

    #[test]
    fn demo_no_such_voice_marker() {
        let mut d = Demo::new();
        assert_eq!(d.get_parameter(GEN_SAMPLE_ID, None).unwrap(), 0);
        d.set_parameter(VOICE_SELECT_ID, 1).unwrap();
        assert_eq!(d.get_parameter(GEN_SAMPLE_ID, None).unwrap(), -2);
    }
}
