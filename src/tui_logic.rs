// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: Copyright (C) 2026  eosed contributors
//
// Pure (non-UI) logic ported from eosed/app.py: browser labels, sweep
// estimation, the voice/zone walk, sample-row resolution, dangling-reference
// integrity and the full-bank sweep state machine. The sweep is a step
// machine so the UI can render progress and honour 'escape' between presets,
// exactly as Textual's threaded worker does.
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use crate::bridge::BridgeLike;
use crate::params;

pub const LEGEND_SEP: &str = " · ";
pub const BROWSER_MIN_WINDOW: usize = 16;
pub const BROWSER_EXTEND_THRESHOLD: usize = 10;
pub const BROWSER_EXTEND_CHUNK: usize = 50;
// The full PRESET_SELECT wire range (spec'd 0-999), not the 0-127 default:
// live-confirmed to hold presets past 127 (RESOLUTION_NOTES §19).
pub const SCAN_LAST: u32 = 999;
pub const SWEEP_CONFIRM_SECONDS: f64 = 60.0;
pub const SAMPLE_USAGE_EARLY_STOP_DEFAULT: usize = 10;

// Live-tested multipliers from docs/RESOLUTION_NOTES.md §20.
fn sweep_seconds_per_kb(depth: &str) -> Option<f64> {
    match depth {
        "names" => Some(0.075),
        "structure" => Some(0.68),
        "full" => Some(3.10),
        _ => None,
    }
}

// Confirmed live (§13): an unused sample slot replies with this placeholder.
pub const EMPTY_SAMPLE_NAME: &str = "Empty Sample";
// E4_GEN_SAMPLE is signed (§11): -1 multisample, -2 no such voice.
pub const MULTISAMPLE_SENTINEL: i64 = -1;
pub const NO_SUCH_VOICE_MARKER: i64 = -2;
pub const MAX_ZONE_SCAN: u32 = 256;
pub const MAX_VOICE_SCAN: u32 = 256;

pub fn slot_label(kind: &str, number: u32) -> String {
    if kind == "sample" {
        format!("S{number:03}")
    } else {
        format!("P{number:03}")
    }
}

pub fn group_param_ids(group: &str) -> Vec<u32> {
    let prefix = match group {
        "voice" => "voice.",
        "global" => "global",
        "link" => "link",
        _ => return vec![],
    };
    let mut ids: Vec<u32> = params::PARAMS_ITER
        .iter()
        .filter(|p| p.group.starts_with(prefix))
        .map(|p| p.id)
        .collect();
    ids.sort();
    ids
}

pub fn estimate_sweep_seconds(depth: &str, used_kb: Option<u64>) -> Option<f64> {
    let used = used_kb?;
    if used == 0 {
        return None;
    }
    sweep_seconds_per_kb(depth).map(|per| used as f64 * per)
}

pub fn humanize_seconds(seconds: f64) -> String {
    if seconds < 90.0 {
        return format!("{} seconds", seconds as u64);
    }
    let minutes = seconds / 60.0;
    if minutes < 90.0 {
        return format!("{:.0} minutes", minutes);
    }
    format!("{:.1} hours", minutes / 60.0)
}

// Pack hint blocks into lines no wider than `width`; blocks are never split.
pub fn wrap_blocks(blocks: &[String], width: usize) -> String {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for block in blocks {
        let candidate = if current.is_empty() {
            block.clone()
        } else {
            format!("{current}{LEGEND_SEP}{block}")
        };
        if candidate.len() > width && !current.is_empty() {
            lines.push(current);
            current = block.clone();
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines.join("\n")
}

/// (zone count, raw sample numbers) for one voice, or None for a voice index
/// that does not exist on this preset. Same walk as app.py's
/// _voice_sample_info: trust the voice's own E4_GEN_SAMPLE, not
/// voice_num_szones (§11/§12).
pub fn voice_sample_info(
    bridge: &mut dyn BridgeLike,
    preset: u32,
    voice: u32,
    timeout: Option<Duration>,
) -> Option<(usize, Vec<i64>)> {
    let preset_select = params::by_name("PRESET_SELECT").unwrap().id;
    let voice_select = params::by_name("VOICE_SELECT").unwrap().id;
    let zone_select = params::by_name("SAMPLE_ZONE_SELECT").unwrap().id;
    let gen_sample = params::by_name("E4_GEN_SAMPLE").unwrap().id;
    bridge.set_parameter(preset_select, preset as i64).ok()?;
    bridge.set_parameter(voice_select, voice as i64).ok()?;
    let voice_level = bridge.get_parameter(gen_sample, timeout).ok()?;
    if voice_level == NO_SUCH_VOICE_MARKER {
        return None;
    }
    if voice_level != MULTISAMPLE_SENTINEL {
        return Some((1, vec![voice_level]));
    }
    let mut numbers = Vec::new();
    for zone in 0..MAX_ZONE_SCAN {
        bridge.set_parameter(zone_select, zone as i64).ok()?;
        let sample = bridge.get_parameter(gen_sample, timeout).ok()?;
        if sample == 0 {
            break;
        }
        numbers.push(sample);
    }
    Some((numbers.len(), numbers))
}

/// Resolve raw sample numbers to (number, name, "V1,V3") rows. A failed name
/// fetch yields "" (transport failure, not evidence of an erased sample).
pub fn resolve_sample_rows(
    bridge: &mut dyn BridgeLike,
    by_voice: &BTreeMap<usize, Vec<i64>>,
    memo: &mut HashMap<i64, String>,
    cache: &HashMap<u32, String>,
    use_catalog_cache: bool,
    timeout: Option<Duration>,
) -> Vec<(i64, String, String)> {
    let mut users: BTreeMap<i64, std::collections::BTreeSet<usize>> = BTreeMap::new();
    for (voice, numbers) in by_voice {
        for &number in numbers {
            users.entry(number).or_default().insert(*voice);
        }
    }
    let mut rows = Vec::new();
    for (number, voices) in users {
        let key = number as u32;
        let name = if use_catalog_cache && cache.contains_key(&key) {
            cache[&key].clone()
        } else if let std::collections::hash_map::Entry::Vacant(e) = memo.entry(number) {
            let fetched = bridge.get_sample_name(key, timeout).unwrap_or_default();
            e.insert(fetched.clone());
            fetched
        } else {
            memo[&number].clone()
        };
        let label = voices
            .iter()
            .map(|v| format!("V{}", v + 1))
            .collect::<Vec<_>>()
            .join(",");
        rows.push((number, name, label));
    }
    rows
}

#[derive(Clone)]
pub struct Overview {
    pub voice_count: usize,
    pub sample_rows: Vec<(i64, String, String)>,
}

/// Every voice still pointing at a sample the device no longer holds (§21c).
/// Sample 0 is never dangling (empty voices read 0); a blank name is unknown,
/// not missing -- only the literal "Empty Sample" placeholder counts.
pub fn dangling_sample_refs(overviews: &BTreeMap<u32, Overview>) -> Vec<(u32, i64, String)> {
    let mut found = Vec::new();
    for (&preset, ov) in overviews {
        let mut dangling: Vec<(i64, String)> = ov
            .sample_rows
            .iter()
            .filter(|(number, name, _)| {
                *number != 0 && name.trim().eq_ignore_ascii_case(EMPTY_SAMPLE_NAME)
            })
            .map(|(number, _, label)| (*number, label.clone()))
            .collect();
        dangling.sort();
        for (number, label) in dangling {
            found.push((preset, number, label));
        }
    }
    found
}

#[derive(Debug, PartialEq)]
pub enum SweepPhase {
    PresetWalk,
    SampleNames,
    Done,
}

/// The 0-999 walk behind 'u', 'i', 'c' and 'C', one preset per step so the
/// terminal stays responsive and 'escape' can cancel it mid-sweep.
pub struct SweepState {
    pub depth: String,
    pub gap: Option<usize>,
    pub phase: SweepPhase,
    pub preset: u32,
    pub sample: u32,
    pub consecutive_empty: usize,
    pub consecutive_empty_samples: usize,
    pub stopped_early: bool,
    pub stopped_at: u32,
    pub sample_stopped_early: bool,
    pub sample_stopped_at: u32,
    pub cancelled: bool,
    pub preset_names: HashMap<u32, String>,
    pub sample_names: HashMap<u32, String>,
    pub overviews: BTreeMap<u32, Overview>,
    pub sample_index: HashMap<i64, Vec<(u32, String)>>,
    pub memo: HashMap<i64, String>,
}

impl SweepState {
    pub fn new(depth: &str, gap: Option<usize>) -> Self {
        SweepState {
            depth: depth.to_string(),
            gap,
            phase: SweepPhase::PresetWalk,
            preset: 0,
            sample: 0,
            consecutive_empty: 0,
            consecutive_empty_samples: 0,
            stopped_early: false,
            stopped_at: 0,
            sample_stopped_early: false,
            sample_stopped_at: 0,
            cancelled: false,
            preset_names: HashMap::new(),
            sample_names: HashMap::new(),
            overviews: BTreeMap::new(),
            sample_index: HashMap::new(),
            memo: HashMap::new(),
        }
    }

    /// Advance exactly one preset (or one sample name). Returns the status
    /// line Textual would show; None when the sweep is done.
    pub fn step(
        &mut self,
        bridge: &mut dyn BridgeLike,
        timeout: Option<Duration>,
    ) -> Option<String> {
        match self.phase {
            SweepPhase::Done => None,
            SweepPhase::PresetWalk => {
                let preset = self.preset;
                self.stopped_at = preset;
                let preset_select = params::by_name("PRESET_SELECT").unwrap().id;
                let voice_select = params::by_name("VOICE_SELECT").unwrap().id;
                if bridge.set_parameter(preset_select, preset as i64).is_err() {
                    self.cancelled = true;
                    self.phase = SweepPhase::Done;
                    return Some(format!("scan aborted at preset {preset}"));
                }
                if let Ok(name) = bridge.get_preset_name(preset, timeout) {
                    self.preset_names.insert(preset, name);
                }
                let walk = self.depth == "structure" || self.depth == "full";
                if walk {
                    let mut by_voice: BTreeMap<usize, Vec<i64>> = BTreeMap::new();
                    let mut voice_count = 0usize;
                    for voice in 0..MAX_VOICE_SCAN {
                        match voice_sample_info(bridge, preset, voice, timeout) {
                            None => break,
                            Some((_, numbers)) => {
                                by_voice.insert(voice as usize, numbers.clone());
                                voice_count = voice as usize + 1;
                            }
                        }
                    }
                    // Re-select voice 0: the zone walk left SAMPLE_ZONE_SELECT
                    // at its last zone, so voice/global reads would come back
                    // as the -1 "not applicable" sentinel otherwise.
                    bridge.set_parameter(voice_select, 0).ok();
                    let rows = resolve_sample_rows(
                        bridge,
                        &by_voice,
                        &mut self.memo,
                        &HashMap::new(),
                        false,
                        timeout,
                    );
                    self.overviews.insert(
                        preset,
                        Overview {
                            voice_count,
                            sample_rows: rows,
                        },
                    );
                    if voice_count > 0 {
                        let used: std::collections::BTreeSet<i64> =
                            by_voice.values().flat_map(|v| v.iter().copied()).collect();
                        for used_number in used {
                            let name = self.preset_names.get(&preset).cloned().unwrap_or_default();
                            self.sample_index
                                .entry(used_number)
                                .or_default()
                                .push((preset, name));
                        }
                        self.consecutive_empty = 0;
                    } else {
                        self.consecutive_empty += 1;
                        if self.gap.is_some() && self.consecutive_empty >= self.gap.unwrap() {
                            self.stopped_early = true;
                            self.phase = SweepPhase::SampleNames;
                            return Some(format!("preset walk stopped at {preset} after {} consecutive empty presets", self.consecutive_empty));
                        }
                    }
                }
                self.preset += 1;
                if self.preset > SCAN_LAST {
                    self.phase = SweepPhase::SampleNames;
                }
                Some(format!(
                    "caching preset {preset}/{SCAN_LAST} ({} — 'escape' to cancel) ...",
                    self.depth
                ))
            }
            SweepPhase::SampleNames => {
                let sample = self.sample;
                self.sample_stopped_at = sample;
                let fetched = bridge.get_sample_name(sample, timeout).unwrap_or_default();
                if !fetched.trim().is_empty()
                    && !fetched.trim().eq_ignore_ascii_case(EMPTY_SAMPLE_NAME)
                {
                    self.sample_names.insert(sample, fetched);
                    self.consecutive_empty_samples = 0;
                } else {
                    self.consecutive_empty_samples += 1;
                    if self.gap.is_some() && self.consecutive_empty_samples >= self.gap.unwrap() {
                        self.sample_stopped_early = true;
                        self.phase = SweepPhase::Done;
                        return Some(format!(
                            "sample names stopped at {sample} after {} consecutive unnamed samples",
                            self.consecutive_empty_samples
                        ));
                    }
                }
                self.sample += 1;
                if self.sample > SCAN_LAST {
                    self.phase = SweepPhase::Done;
                }
                Some(format!("caching sample names: {sample}/{SCAN_LAST} ..."))
            }
        }
    }

    pub fn done(&self) -> bool {
        self.phase == SweepPhase::Done
    }

    /// How many scanned presets came back with no voices at all: the walk is
    /// best-effort, so "0 voices" can mean "no answer", and silence needs a
    /// counter (app.py _zero_voice_note).
    pub fn zero_voice_note(&self) -> String {
        if self.depth != "structure" && self.depth != "full" {
            return String::new();
        }
        let zero = self
            .overviews
            .values()
            .filter(|ov| ov.voice_count == 0)
            .count();
        format!(
            "; {zero} of {} scanned preset(s) reported 0 voices",
            self.overviews.len()
        )
    }

    pub fn sweep_note(&self) -> String {
        let mut note = if self.stopped_early {
            format!(
                " (stopped at preset {} after {} consecutive empty presets)",
                self.stopped_at,
                self.gap.unwrap_or(0)
            )
        } else {
            format!(" (full sweep to preset {})", self.stopped_at)
        };
        if self.sample_stopped_early {
            note.push_str(&format!(
                "; sample names stopped at {} after {} consecutive unnamed samples",
                self.sample_stopped_at,
                self.gap.unwrap_or(0)
            ));
        }
        note.push_str(&self.zero_voice_note());
        note
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::Demo;

    #[test]
    fn slot_labels_match_device_notation() {
        assert_eq!(slot_label("preset", 75), "P075");
        assert_eq!(slot_label("sample", 195), "S195");
        assert_eq!(slot_label("sample", 0), "S000");
    }

    #[test]
    fn sweep_estimates_match_measured_bank() {
        // 150s / 2013 KB, 1371s / 2013 KB, 6241s / 2013 KB (RESOLUTION_NOTES §20)
        let names = estimate_sweep_seconds("names", Some(2013)).unwrap();
        let structure = estimate_sweep_seconds("structure", Some(2013)).unwrap();
        let full = estimate_sweep_seconds("full", Some(2013)).unwrap();
        assert!((names - 151.0).abs() < 1.0);
        assert!((structure - 1368.8).abs() < 1.0);
        assert!((full - 6240.3).abs() < 1.0);
        assert_eq!(estimate_sweep_seconds("names", None), None);
        assert_eq!(estimate_sweep_seconds("names", Some(0)), None);
    }

    #[test]
    fn humanize_boundaries() {
        assert_eq!(humanize_seconds(89.0), "89 seconds");
        assert_eq!(humanize_seconds(1371.0), "23 minutes");
        assert_eq!(humanize_seconds(6241.0), "1.7 hours");
    }

    #[test]
    fn wrap_blocks_breaks_only_between_blocks() {
        let blocks = vec![
            "q Quit".into(),
            "w Write gate".into(),
            "z Undo".into(),
            "very long hint block here".into(),
        ];
        let out = wrap_blocks(&blocks, 20);
        for line in out.lines() {
            assert!(line.len() <= 20 || line == "very long hint block here");
        }
        assert_eq!(wrap_blocks(&[], 10), "");
        assert_eq!(wrap_blocks(&["a".into(), "b".into()], 100), "a · b");
    }

    #[test]
    fn dangling_filters_placeholder_but_not_blank_or_zero() {
        let mut ov = BTreeMap::new();
        ov.insert(
            3,
            Overview {
                voice_count: 2,
                sample_rows: vec![
                    (0, "Empty Sample".into(), "V1".into()),
                    (5, "Empty Sample".into(), "V2".into()),
                    (7, "".into(), "V3".into()),
                    (9, "Real Sample".into(), "V4".into()),
                ],
            },
        );
        let found = dangling_sample_refs(&ov);
        assert_eq!(found, vec![(3u32, 5i64, "V2".into())]);
    }

    #[test]
    fn voice_group_size_is_stable() {
        assert_eq!(group_param_ids("voice").len(), 146); // the 146-id group
    }

    #[test]
    fn demo_sweep_walks_known_names_and_builds_usage() {
        let mut d = Demo::new();
        // The demo bridge accepts PRESET_SELECT for any preset, so the walk
        // runs the full range; only the three canned presets have names.
        let mut sweep = SweepState::new("structure", Some(10));
        while !sweep.done() {
            sweep.step(&mut d, None);
        }
        assert!(!sweep.cancelled);
        assert_eq!(sweep.preset_names.len(), 3); // 0, 1, 5
        assert_eq!(sweep.overviews.len(), 1000);
        assert!(sweep.sample_index.contains_key(&0)); // every voice reads sample 0
        assert!(!sweep.stopped_early);
    }
}
