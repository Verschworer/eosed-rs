// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: Copyright (C) 2026  eosed contributors
//
// Rust port of eos/params.py — EOS editor-protocol parameter table.
// The data lives in params_data.rs (generated from the Python source);
// this file is the logic: lookup, value naming, display conversions.

//! Every parameter is addressed by a 14-bit id (see [`crate::messages`]'s
//! ParameterEdit/ParameterRequest). This module gives each id a name, group
//! and a *static* min/max transcribed from the specification — but the
//! device's own 03h/04h reply is authoritative at runtime and should be
//! preferred where the two might drift across EOS versions (see
//! [`crate::messages::ParameterRange`]).
//!
//! Ids intentionally absent (22, 36, 196, 197, 200) are the specification's
//! own "not used" gaps, not omissions.

use crate::params_data as data;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Parameter {
    pub id: u32,
    pub name: &'static str,
    pub group: &'static str,
    pub min: i32,
    pub max: i32,
    pub unit: Option<&'static str>,
    pub default: Option<i32>,
    pub notes: Option<&'static str>,
}

/// Resolve a parameter by numeric id. The table is id-sorted by construction.
pub fn by_id(id: u32) -> Option<&'static Parameter> {
    data::PARAMETERS
        .binary_search_by_key(&id, |p| p.id)
        .ok()
        .map(|i| &data::PARAMETERS[i])
}

/// Resolve a parameter by name (e.g. "E4_PRESET_VOLUME").
pub fn by_name(name: &str) -> Option<&'static Parameter> {
    data::PARAMETERS.iter().find(|p| p.name == name)
}

pub const SAMPLE_ZONE_PARAM_IDS: [u32; 13] = data::SAMPLE_ZONE_PARAM_IDS;

/// All parameters, id-sorted (for the differential harness and UIs).
pub static PARAMS_ITER: &[Parameter] = data::PARAMETERS;

fn label(table: &[(i32, &'static str)], value: i32) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == value).map(|(_, v)| *v)
}

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// MIDI key number -> E-mu's own note spelling (0 = C-2, 60 = C3, 127 = G8).
/// The octave offset is not scientific pitch: it is pinned by the spec's own
/// "60 = C3" for E4_GEN_ORIG_KEY and the "C-2 -> G8" key-limit range.
pub fn note_name(value: i32) -> String {
    format!(
        "{}{}",
        NOTE_NAMES[(value.rem_euclid(12)) as usize],
        value.div_euclid(12) - 2
    )
}

// Absolute key numbers, so a note name is meaningful. The matching *FADE
// fields are deliberately absent: those are widths in semitones, not keys.
const NOTE_VALUED: [&str; 6] = [
    "E4_GEN_ORIG_KEY",
    "E4_GEN_KEY_LOW",
    "E4_GEN_KEY_HIGH",
    "E4_LINK_KEY_LOW",
    "E4_LINK_KEY_HIGH",
    "MASTER_AUDITION_KEY",
];

const ON_OFF: [&str; 9] = [
    "E4_VOICE_NON_TRANSPOSE",
    "E4_VOICE_LATCHMODE",
    "MASTER_AES_BOOST",
    "MASTER_AKAI_LOOP_ADJ",
    "MIDIGLO_PEDAL_OVERRIDE",
    "MIDIGLO_RCV_PROGRAM_CHANGE",
    "MIDIGLO_SEND_PROGRAM_CHANGE",
    "MASTER_FX_BYPASS",
    "MASTER_OUTPUT_DITHER",
];

// Same shape, senses reversed (0 = on). Kept apart rather than normalised —
// the reversal is the device's.
const ON_OFF_INVERTED: [&str; 2] = ["MASTER_SCSI_TERM", "MASTER_COMBINE_LR"];

const FILTER_FLAG_PREFIX: &str = "E4_LINK_FILTER_";

pub fn volenv_depth_display(value: i32) -> String {
    // Spec: "Amp Env Depth: -96dB to -48dB by 3's" — 17 steps from -96.
    format!("{}dB", -96 + 3 * value)
}

pub fn velocity_curve_display(value: i32) -> String {
    if value == 0 {
        "linear".into()
    } else {
        format!("curve {value}")
    }
}

pub fn magic_preset_display(value: i32) -> String {
    if value == 0 {
        "off".into()
    } else {
        format!("preset {:03}", value - 1)
    }
}

/// A 0-based MIDI channel shown 1-based, the way every panel displays it.
pub fn midi_channel_display(value: i32, off_value: Option<i32>) -> String {
    if off_value == Some(value) {
        "off".into()
    } else {
        format!("ch {}", value + 1)
    }
}

/// E4_LINK_INTERNAL_EXTERNAL (id 251). INFERRED from the EOS manual's
/// "1 internal + 16 external MIDI channels = range 0..16"; not confirmed
/// live (see docs/RESOLUTION_NOTES.md).
pub fn link_type_display(value: i32) -> String {
    if value == 0 {
        "internal".into()
    } else {
        format!("MIDI ch {value}")
    }
}

pub fn midi_control_display(value: i32) -> String {
    match value {
        -1 => "off".into(),
        32 => "ptwheel".into(),
        33 => "chnpres".into(),
        _ => value.to_string(),
    }
}

/// Look up a human-readable name for one parameter's value across every
/// enum/name table this module defines. None => caller shows the raw number
/// (or, for the computed families below, value_display_dynamic answers).
pub fn known_value_name(p: &Parameter, value: i32) -> Option<&'static str> {
    let name = p.name;
    // Whole families first: listing each id individually would be a
    // maintenance trap.
    if NOTE_VALUED.contains(&name) {
        return None; // computed spelling: value_display_dynamic
    }
    if ON_OFF.contains(&name) || name.starts_with(FILTER_FLAG_PREFIX) {
        return match value {
            0 => Some("off"),
            1 => Some("on"),
            _ => None,
        };
    }
    if ON_OFF_INVERTED.contains(&name) {
        return match value {
            0 => Some("on"),
            1 => Some("off"),
            _ => None,
        };
    }
    if name == "E4_LINK_INTERNAL_EXTERNAL" {
        return None; // computed: value_display_dynamic
    }
    if name == "E4_VOICE_GLIDE_CURVE" {
        return label(data::GLIDE_CURVE_LABELS, value);
    }
    if matches!(name, "E4_VOICE_LFO_RATE" | "E4_VOICE_LFO2_RATE") {
        return None; // computed Hz: value_display_dynamic
    }
    if matches!(name, "E4_VOICE_LFO_SYNC" | "E4_VOICE_LFO2_SYNC") {
        return label(data::LFO_SYNC_LABELS, value);
    }
    if matches!(
        name,
        "MIDIGLO_VEL_CURVE"
            | "MIDIGLO_MAGIC_PRESET"
            | "MIDIGLO_BASIC_CHANNEL"
            | "MASTER_FX_MM_CTRL_CHANNEL"
            | "MULTIMODE_CHANNEL"
            | "MASTER_USING_MAC"
            | "MASTER_AKAI_SAMPLER_ID"
    ) {
        return None; // computed: value_display_dynamic
    }
    if matches!(
        name,
        "E4_PRESET_CTRL_A" | "E4_PRESET_CTRL_B" | "E4_PRESET_CTRL_C" | "E4_PRESET_CTRL_D"
    ) {
        return if value == -1 { Some("off") } else { None };
    }
    if name == "MULTIMODE_PRESET" {
        return if value == -1 { Some("off") } else { None };
    }
    if name == "MULTIMODE_SUBMIX" || name == "E4_VOICE_SUBMIX" {
        return label(data::SUBMIX_LABELS, value);
    }
    if name.ends_with("FX_A_ALGORITHM") {
        return label(data::FX_A_ALGORITHM_NAMES, value);
    }
    if name.ends_with("FX_B_ALGORITHM") {
        return label(data::FX_B_ALGORITHM_NAMES, value);
    }
    if name.ends_with("FX_A_PARM_0")
        || name.ends_with("FX_A_PARM_1")
        || name.ends_with("FX_A_PARM_2")
    {
        return label(
            data::FX_A_PARM_NAMES,
            name.as_bytes()[name.len() - 1] as i32 - b'0' as i32,
        );
    }
    if name.ends_with("FX_B_PARM_0")
        || name.ends_with("FX_B_PARM_1")
        || name.ends_with("FX_B_PARM_2")
    {
        return label(
            data::FX_B_PARM_NAMES,
            name.as_bytes()[name.len() - 1] as i32 - b'0' as i32,
        );
    }
    if (name.ends_with("_AMT_0")
        || name.ends_with("_AMT_1")
        || name.ends_with("_AMT_2")
        || name.ends_with("_AMT_3"))
        && (name.contains("FX_A") || name.contains("FX_B"))
    {
        return label(
            data::FX_AMT_BUS_NAMES,
            name.as_bytes()[name.len() - 1] as i32 - b'0' as i32,
        );
    }
    if p.group == "voice.cords" && name.ends_with("_SRC") {
        return label(data::CORD_SOURCES, value);
    }
    if p.group == "voice.cords" && name.ends_with("_DST") {
        return label(data::CORD_DESTINATIONS, value);
    }
    match name {
        "E4_VOICE_SOLO" => return label(data::VOICE_SOLO_MODES, value),
        "E4_VOICE_ASSIGN_GROUP" => return label(data::VOICE_ASSIGN_GROUPS, value),
        "E4_VOICE_LFO_SHAPE" | "E4_VOICE_LFO2_SHAPE" => return label(data::LFO_SHAPES, value),
        "MASTER_OUTPUT_FORMAT" => return label(data::OUTPUT_FORMAT_LABELS, value),
        "MASTER_OUTPUT_CLOCK" => return label(data::OUTPUT_CLOCK_LABELS, value),
        "MASTER_WORD_CLOCK_IN" => return label(data::WORD_CLOCK_IN_LABELS, value),
        "MIDIGLO_MIDI_MODE" => return label(data::MIDI_MODE_LABELS, value),
        "MIDIGLO_CTRL7_CURVE" => return label(data::CTRL7_CURVE_LABELS, value),
        "E4_VOICE_FTYPE" => return label(data::FILTER_TYPE_NAMES, value),
        "MIDIGLO_PITCH_CONTROL"
        | "MIDIGLO_MOD_CONTROL"
        | "MIDIGLO_PRESSURE_CONTROL"
        | "MIDIGLO_PEDAL_CONTROL"
        | "MIDIGLO_SWITCH_1_CONTROL"
        | "MIDIGLO_SWITCH_2_CONTROL"
        | "MIDIGLO_THUMB_CONTROL"
        | "MIDIGLO_MIDI_A_CONTROL"
        | "MIDIGLO_MIDI_B_CONTROL"
        | "MIDIGLO_MIDI_C_CONTROL"
        | "MIDIGLO_MIDI_D_CONTROL"
        | "MIDIGLO_MIDI_E_CONTROL"
        | "MIDIGLO_MIDI_F_CONTROL"
        | "MIDIGLO_MIDI_G_CONTROL"
        | "MIDIGLO_MIDI_H_CONTROL" => return None, // computed: value_display_dynamic
        _ => {}
    }
    // Envelope segment ids carry a clean role label in their notes.
    if matches!(
        p.group,
        "voice.amp.env" | "voice.filter.env" | "voice.aux.env"
    ) {
        return p.notes;
    }
    None
}

/// Dynamic half of value naming: values whose display is computed rather
/// than table-lookup (notes, Hz, SCSI ids, MIDI controls...).
pub fn value_display_dynamic(p: &Parameter, value: i32) -> Option<String> {
    let name = p.name;
    if NOTE_VALUED.contains(&name) {
        return if (0..=127).contains(&value) {
            Some(note_name(value))
        } else {
            None
        };
    }
    match name {
        "E4_VOICE_VOLENV_DEPTH" => return Some(volenv_depth_display(value)),
        "MIDIGLO_VEL_CURVE" => return Some(velocity_curve_display(value)),
        "MIDIGLO_MAGIC_PRESET" => return Some(magic_preset_display(value)),
        "MIDIGLO_BASIC_CHANNEL" => return Some(midi_channel_display(value, None)),
        "MASTER_FX_MM_CTRL_CHANNEL" => return Some(midi_channel_display(value, Some(-1))),
        "MULTIMODE_CHANNEL" => return Some(format!("ch {value}")), // already 1-based
        "E4_LINK_INTERNAL_EXTERNAL" => return Some(link_type_display(value)),
        "MASTER_USING_MAC" | "MASTER_AKAI_SAMPLER_ID" => {
            return Some(if value == -1 {
                "none".into()
            } else {
                format!("SCSI id {value}")
            });
        }
        "E4_VOICE_LFO_RATE" | "E4_VOICE_LFO2_RATE" => return Some(cnv_lfo_rate(value)),
        "MIDIGLO_PITCH_CONTROL"
        | "MIDIGLO_MOD_CONTROL"
        | "MIDIGLO_PRESSURE_CONTROL"
        | "MIDIGLO_PEDAL_CONTROL"
        | "MIDIGLO_SWITCH_1_CONTROL"
        | "MIDIGLO_SWITCH_2_CONTROL"
        | "MIDIGLO_THUMB_CONTROL"
        | "MIDIGLO_MIDI_A_CONTROL"
        | "MIDIGLO_MIDI_B_CONTROL"
        | "MIDIGLO_MIDI_C_CONTROL"
        | "MIDIGLO_MIDI_D_CONTROL"
        | "MIDIGLO_MIDI_E_CONTROL"
        | "MIDIGLO_MIDI_F_CONTROL"
        | "MIDIGLO_MIDI_G_CONTROL"
        | "MIDIGLO_MIDI_H_CONTROL" => return Some(midi_control_display(value)),
        _ => {}
    }
    None
}

/// "123" normally, or "123 (Name)" when a known mapping exists.
pub fn describe_value(p: &Parameter, value: i32) -> String {
    if let Some(dyn_) = value_display_dynamic(p, value) {
        return format!("{value} ({dyn_})");
    }
    match known_value_name(p, value) {
        Some(n) => format!("{value} ({n})"),
        None => value.to_string(),
    }
}

/// describe_value with the sign in a column of its own: a "-" for negatives
/// and a space for positives, so the FIRST digit of every row lines up.
pub fn describe_value_aligned(p: &Parameter, value: i32) -> String {
    let text = format!("{}{}", if value < 0 { "-" } else { " " }, value.abs());
    let name =
        value_display_dynamic(p, value).or_else(|| known_value_name(p, value).map(str::to_string));
    match name {
        Some(n) => format!("{text} ({n})"),
        None => text,
    }
}

/// Every named value this parameter can take, or None if it has no list.
/// The caller must still filter by the DEVICE's reported range.
pub fn value_choices(name: &str, group: &str) -> Option<&'static [(i32, &'static str)]> {
    if name.ends_with("FX_A_ALGORITHM") {
        return Some(data::FX_A_ALGORITHM_NAMES);
    }
    if name.ends_with("FX_B_ALGORITHM") {
        return Some(data::FX_B_ALGORITHM_NAMES);
    }
    if group == "voice.cords" && name.ends_with("_SRC") {
        return Some(data::CORD_SOURCES);
    }
    if group == "voice.cords" && name.ends_with("_DST") {
        return Some(data::CORD_DESTINATIONS);
    }
    Some(match name {
        "E4_VOICE_SOLO" => data::VOICE_SOLO_MODES,
        "E4_VOICE_ASSIGN_GROUP" => data::VOICE_ASSIGN_GROUPS,
        "E4_VOICE_LFO_SHAPE" | "E4_VOICE_LFO2_SHAPE" => data::LFO_SHAPES,
        "E4_VOICE_SUBMIX" => data::SUBMIX_LABELS,
        "E4_VOICE_FTYPE" => data::FILTER_TYPE_NAMES,
        "MASTER_OUTPUT_FORMAT" => data::OUTPUT_FORMAT_LABELS,
        "MASTER_OUTPUT_CLOCK" => data::OUTPUT_CLOCK_LABELS,
        "MASTER_WORD_CLOCK_IN" => data::WORD_CLOCK_IN_LABELS,
        "MIDIGLO_MIDI_MODE" => data::MIDI_MODE_LABELS,
        "MIDIGLO_CTRL7_CURVE" => data::CTRL7_CURVE_LABELS,
        _ => return None,
    })
}

// --- filter Hz/dB display conversions (closed-form; verified against the
//     spec's own C source) --------------------------------------------------

/// Port of the spec's fil_freq(input, maxfreq, mul): input 0..255 -> Hz,
/// exponentially spaced down from maxfreq.
pub fn fil_freq(value: i32, maxfreq: u64, mul: u64) -> u64 {
    let mut f = maxfreq;
    let mut remaining = 255 - value;
    while remaining > 0 {
        f = f * mul / 1024;
        remaining -= 1;
    }
    f
}

pub fn cnv_morph_freq(value: i32) -> String {
    format!("{}Hz", fil_freq(value, 10000, 1006))
}

/// Filter Table 4: input 0..127 -> "+d.ddB"/"-d.ddB", -24.0..+23.6dB.
/// Python's floor-division is replicated with div_euclid/rem_euclid.
pub fn cnv_morph_gain(value: i32) -> String {
    let gain10x = -240 + (value * 120) / 32;
    let gain_i = gain10x.div_euclid(10);
    let gain_f = gain10x.rem_euclid(10);
    let sign = if gain10x >= 0 { "+" } else { "-" };
    format!("{sign}{}.{}dB", gain_i.abs(), gain_f)
}

pub fn filter_table_1(value: i32) -> String {
    format!("{}Hz", fil_freq(value, 20000, 1002))
}
pub fn filter_table_2(value: i32) -> String {
    format!("{}Hz", fil_freq(value, 18000, 1003))
}
pub fn filter_table_3(value: i32) -> String {
    format!("{}Hz", fil_freq(value, 10000, 1006))
}

// --- glide rate display (firmware-verified tables, byte-identical to
//     glide_units1[]/glide_units2[] at 0x6e770 / 0x6e7f0) ------------------

pub fn cnv_glide_rate(value: usize) -> String {
    assert!(value < 128);
    let msec =
        (data::GLIDE_UNITS1[value] as i64 * 1000 + data::GLIDE_UNITS2[value] as i64 * 10) / 5;
    format!("{}.{:03}sec/oct", msec / 1000, msec % 1000)
}

// --- LFO rate: EOS's OWN lfounits1[]/lfounits2[] (0x6e970 / 0x6e9f0).
//     Anchors 0 -> 0.08, 64 -> 4.12, 127 -> 18.01 Hz confirmed off the
//     E4XT's own rate menu. Exact to the front panel, digit for digit.

pub fn cnv_lfo_rate(value: i32) -> String {
    let v = value.clamp(0, 127) as usize;
    format!("{}.{:02}Hz", data::LFO_UNITS1[v], data::LFO_UNITS2[v])
}

// --- master tuning offset / chorus ITD display conversions ---------------

pub fn cnv_master_tuning(value: i32) -> String {
    let magnitude = data::MASTER_TUNING_MAGNITUDE[value.unsigned_abs() as usize];
    let sign = if value < 0 { "-" } else { "+" };
    format!("{sign}{magnitude}")
}

pub fn cnv_chorus_itd(value: i32) -> String {
    let magnitude = data::CHORUS_ITD_MAGNITUDE_MS[value.unsigned_abs() as usize];
    let sign = if value < 0 { "-" } else { "+" };
    format!("{sign}{magnitude}ms")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_sorted_and_complete() {
        assert!(data::PARAMETERS.windows(2).all(|w| w[0].id < w[1].id));
        assert_eq!(data::PARAMETERS.len(), 267);
        // the spec's own "not used" gaps
        for gap in [22u32, 36, 196, 197, 200] {
            assert!(by_id(gap).is_none());
        }
        assert_eq!(by_id(223).unwrap().name, "PRESET_SELECT");
        assert_eq!(by_name("E4_VOICE_DELAY").unwrap().max, 10000);
    }

    #[test]
    fn corrections_preserved() {
        // id 14: max corrected 27 -> 32 for EOS 4.70
        assert_eq!(by_id(14).unwrap().max, 32);
        // FENV/AENV TGTLVL bipolar -100..100 (spec transcription error fixed)
        assert_eq!(by_id(94).unwrap().min, -100);
        // MASTER_FX_A_ALGORITHM minimum 1 (cannot inherit from itself)
        assert_eq!(by_id(228).unwrap().min, 1);
        // E4_GEN_SAMPLE min -8 (structure sentinels -1/-2)
        assert_eq!(by_id(38).unwrap().min, -8);
    }

    #[test]
    fn note_names() {
        assert_eq!(note_name(0), "C-2");
        assert_eq!(note_name(60), "C3");
        assert_eq!(note_name(127), "G8");
    }

    #[test]
    fn display_conversions() {
        assert_eq!(cnv_lfo_rate(0), "0.08Hz");
        assert_eq!(cnv_lfo_rate(64), "4.12Hz");
        assert_eq!(cnv_lfo_rate(127), "18.01Hz");
        assert_eq!(cnv_lfo_rate(999), "18.01Hz"); // clamped
        assert_eq!(cnv_glide_rate(0), "0.000sec/oct");
        assert_eq!(cnv_glide_rate(127), "32.738sec/oct"); // (163*1000+69*10)/5
        assert_eq!(cnv_morph_gain(64), "+0.0dB");
        assert_eq!(cnv_morph_gain(0), "-24.0dB");
        assert_eq!(fil_freq(255, 20000, 1002), 20000);
        assert!(fil_freq(0, 20000, 1002) < 20000);
    }

    #[test]
    fn value_descriptions() {
        let p = by_id(223).unwrap();
        assert_eq!(describe_value(p, 42), "42");
        let p = by_name("E4_VOICE_LFO_RATE").unwrap();
        assert_eq!(describe_value(p, 64), "64 (4.12Hz)");
        let p = by_name("MASTER_USING_MAC").unwrap();
        assert_eq!(describe_value(p, -1), "-1 (none)");
        let p = by_name("E4_VOICE_LFO_SHAPE").unwrap();
        assert_eq!(describe_value(p, 1), "1 (sine)");
        let p = by_name("E4_GEN_ORIG_KEY").unwrap();
        assert_eq!(describe_value(p, 60), "60 (C3)");
        // aligned: sign column
        let p = by_name("E4_PRESET_VOLUME").unwrap();
        assert_eq!(describe_value_aligned(p, -6), "-6");
        assert_eq!(describe_value_aligned(p, 60), " 60");
    }

    #[test]
    fn chord_amt_family_generated() {
        // C{n}Amt = 168 + n for all 18 cords
        for n in 0..18 {
            let expected = format!("C{n:02}Amt");
            assert_eq!(
                label(data::CORD_DESTINATIONS, 168 + n).map(str::to_string),
                Some(expected),
                "C{n:02}Amt"
            );
        }
        assert_eq!(label(data::CORD_DESTINATIONS, 176), Some("C08Amt"));
    }
}
