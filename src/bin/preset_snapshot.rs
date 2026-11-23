// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: Copyright (C) 2026  eosed contributors
//
// Rust twin of tools/preset_snapshot.py: capture/verify/replay a preset's
// VOICE parameters as a stand-in for restore. RAM does not survive a power
// cycle (RESOLUTION_NOTES §48) and neither dump format restores, so a
// parameter-table snapshot is what makes a hand-built reference preset
// reproducible. It is NOT a full restore: zones, links, preset-level fields
// and sample data are not covered.

use eosed::bridge::{autodetect, BridgeLike, AUTODETECT_TIMEOUT, SEND_GAP};
use eosed::config::{Settings, DEFAULT_CONFIG_PATH};
use eosed::params;
use std::thread::sleep;
use std::time::Duration;

const GROUPS: [&str; 10] = [
    "voice.general",
    "voice.tuning",
    "voice.mode",
    "voice.amp",
    "voice.amp.env",
    "voice.filter",
    "voice.filter.env",
    "voice.lfo",
    "voice.aux.env",
    "voice.cords",
];
const MAX_CORD: u32 = 12;
const PRESET_SELECT: u32 = 223;
const VOICE_SELECT: u32 = 225;

fn voice_param_ids() -> Vec<u32> {
    params::PARAMS_ITER
        .iter()
        .filter(|p| GROUPS.contains(&p.group))
        .filter(|p| !(p.group == "voice.cords" && (p.id - 129) / 3 >= MAX_CORD))
        .map(|p| p.id)
        .collect()
}

fn confirm(bridge: &mut dyn BridgeLike, pid: u32, want: i64) -> bool {
    for _ in 0..5 {
        if bridge.set_parameter(pid, want).is_ok() {
            sleep(Duration::from_millis(120));
            if let Ok(v) = bridge.get_parameter(pid, Some(Duration::from_secs(1))) {
                if v == want {
                    return true;
                }
            }
        }
        sleep(Duration::from_millis(180));
    }
    false
}

fn select(bridge: &mut dyn BridgeLike, preset: u32, voice: u32) {
    if !confirm(bridge, PRESET_SELECT, preset as i64) {
        eprintln!("could not select preset {preset}");
        std::process::exit(1);
    }
    if !confirm(bridge, VOICE_SELECT, voice as i64) {
        eprintln!("could not select voice {voice}");
        std::process::exit(1);
    }
}

fn read_voice(
    bridge: &mut dyn BridgeLike,
    preset: u32,
    voice: u32,
    ids: &[u32],
) -> Vec<(u32, i64)> {
    select(bridge, preset, voice);
    let mut out = Vec::new();
    for &pid in ids {
        // Params the device declines to answer are omitted, not guessed.
        if let Ok(v) = bridge.get_parameter(pid, Some(Duration::from_secs(1))) {
            out.push((pid, v));
        }
    }
    out
}

fn write_json(preset: u32, voices: &[(u32, Vec<(u32, i64)>)]) -> String {
    // Python json.dump(indent=1) layout, emitted byte-for-byte so the
    // differential comparison is exact.
    let mut s = String::new();
    s.push_str("{\n");
    s.push_str(&format!(" \"preset\": {preset},\n"));
    s.push_str(" \"voices\": {\n");
    for (vi, params) in voices.iter() {
        s.push_str(&format!("  \"{vi}\": {{\n"));
        for (i, &(pid, val)) in params.iter().enumerate() {
            let comma = if i + 1 < params.len() { "," } else { "" };
            s.push_str(&format!("   \"{pid}\": {val}{comma}\n"));
        }
        let comma = if ((vi + 1) as usize) < voices.len() {
            ","
        } else {
            ""
        };
        s.push_str(&format!("  }}{comma}\n"));
    }
    s.push_str(" }\n}");
    s
}

type SnapshotJson = (u32, Vec<(u32, Vec<(u32, i64)>)>);

fn read_json(path: &str) -> SnapshotJson {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("could not read {path}: {e}");
            std::process::exit(1);
        }
    };
    let data: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("invalid json in {path}: {e}");
            std::process::exit(1);
        }
    };
    let preset = data["preset"].as_u64().unwrap_or(0) as u32;
    let mut voices = Vec::new();
    if let Some(obj) = data["voices"].as_object() {
        for (vs, params) in obj {
            let voice: u32 = vs.parse().unwrap_or(0);
            let mut row = Vec::new();
            if let Some(pobj) = params.as_object() {
                for (k, v) in pobj {
                    row.push((k.parse::<u32>().unwrap_or(0), v.as_i64().unwrap_or(0)));
                }
            }
            row.sort();
            voices.push((voice, row));
        }
    }
    voices.sort();
    (preset, voices)
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut config = DEFAULT_CONFIG_PATH.to_string();
    let mut voices_count: usize = 6;
    let mut cmd = String::new();
    let mut preset: u32 = 0;
    let mut file = String::new();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--config" => {
                config = argv[i + 1].clone();
                i += 2;
            }
            "--voices" => {
                voices_count = argv[i + 1].parse().unwrap_or(6);
                i += 2;
            }
            "capture" | "verify" | "replay" => {
                cmd = argv[i].clone();
                i += 1;
            }
            _ => {
                if let Ok(n) = argv[i].parse::<u32>() {
                    preset = n;
                } else {
                    file = argv[i].clone();
                }
                i += 1;
            }
        }
    }
    if cmd.is_empty() || file.is_empty() {
        eprintln!("usage: preset_snapshot [--voices N] (capture|verify|replay) PRESET FILE");
        std::process::exit(2);
    }

    let mut backend = eosed::backend_midir::MidirBackend::new();
    let mut settings = Settings::new("eosed", &config);
    let mut live = match autodetect(
        &mut backend,
        SEND_GAP,
        None,
        None,
        AUTODETECT_TIMEOUT,
        Some(&config),
        &mut settings,
        &|n| eprintln!("  trying {n} ..."),
    ) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let bridge: &mut dyn BridgeLike = &mut live;

    let ids = voice_param_ids();
    let mut rc = 0;
    if cmd == "capture" {
        let mut voices = Vec::new();
        for v in 0..voices_count as u32 {
            let row = read_voice(bridge, preset, v, &ids);
            println!("  voice {v}: {} of {} parameters", row.len(), ids.len());
            voices.push((v, row));
        }
        std::fs::write(&file, write_json(preset, &voices)).unwrap_or_else(|e| {
            eprintln!("could not write {file}: {e}");
            std::process::exit(1);
        });
        println!("  -> {file}");
    } else if cmd == "verify" {
        let (preset, saved) = read_json(&file);
        let mut diffs = 0;
        for (vs, want) in saved {
            let got = read_voice(
                bridge,
                preset,
                vs,
                &want.iter().map(|(k, _)| *k).collect::<Vec<u32>>(),
            );
            let bad: Vec<(u32, i64, Option<i64>)> = want
                .iter()
                .filter_map(|&(k, w)| match got.iter().find(|(gk, _)| *gk == k) {
                    Some((_, g)) if *g != w => Some((k, w, Some(*g))),
                    None => Some((k, w, None)),
                    _ => None,
                })
                .collect();
            diffs += bad.len();
            let mut line = format!(
                "  voice {vs}: {}/{} match",
                want.len() - bad.len(),
                want.len()
            );
            if !bad.is_empty() {
                let shown: Vec<String> = bad
                    .iter()
                    .take(8)
                    .map(|(i, w, g)| match g {
                        Some(g) => format!("id {i}={g} (file {w})"),
                        None => format!("id {i}=None (file {w})"),
                    })
                    .collect();
                line.push_str("   differs: ");
                line.push_str(&shown.join(", "));
                if bad.len() > 8 {
                    line.push_str(" ...");
                }
            }
            println!("{line}");
        }
        println!();
        if diffs == 0 {
            println!("  IDENTICAL");
        } else {
            println!("  {diffs} DIFFERENCES");
            rc = 1;
        }
    } else {
        let (preset, saved) = read_json(&file);
        let mut failed: Vec<(u32, u32, i64)> = Vec::new();
        for (vs, want) in saved {
            select(bridge, preset, vs);
            for &(pid, target) in &want {
                if !confirm(bridge, pid, target) {
                    failed.push((vs, pid, target));
                }
            }
            let written = want.len() - failed.iter().filter(|f| f.0 == vs).count();
            println!("  voice {vs}: {written}/{} written", want.len());
        }
        if !failed.is_empty() {
            println!("\n  COULD NOT SET:");
            for &(vs, pid, target) in &failed {
                println!("    voice {vs} id {pid} -> {target}");
            }
            println!("\n  {} FAILED", failed.len());
            rc = 1;
        } else {
            println!("\n  all values written and read back");
        }
    }

    // Always hand the device back to voice 0.
    confirm(bridge, VOICE_SELECT, 0);
    std::process::exit(rc);
}
