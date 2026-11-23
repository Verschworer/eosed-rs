// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: Copyright (C) 2026  eosed contributors
//
// This file is part of eosed.  Original work.  GPL-2.0-or-later.
// Rust twin of fulltest_py.py: exercises the entire live engine surface
// against preset 2 and prints canonical lines for a byte diff vs Python.
use eosed::backend_midir::MidirBackend;
use eosed::bridge::{autodetect, EosBridge, Pair, AUTODETECT_TIMEOUT, SEND_GAP};
use eosed::config::Settings;

const PRESET: u32 = 2;

fn sig(data: &[u8]) -> String {
    if data.is_empty() {
        return "0 empty".into();
    }
    let head: String = data[..data.len().min(16)]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let tail_start = data.len().saturating_sub(16);
    let tail: String = data[tail_start..]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{} {} {}", data.len(), head, tail)
}

fn main() {
    let mut backend = MidirBackend::new();
    let mut settings = Settings::new("eosed", "config.toml");
    let mut bridge = match autodetect(
        &mut backend,
        SEND_GAP,
        None,
        None,
        AUTODETECT_TIMEOUT,
        Some("config.toml"),
        &mut settings,
        &|n| eprintln!("  trying {n} ..."),
    ) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("autodetect failed: {e}");
            std::process::exit(1);
        }
    };

    let seed = std::fs::read("/Users/aleksandr/.hermes/cache/scratch/livepy/py_dump.bin").unwrap();
    let seeded = EosBridge::<Pair>::retarget_dump(&seed, PRESET);
    bridge.send_preset_old(&seeded, true, None, 3).unwrap();
    bridge.set_preset_name(PRESET, "test").unwrap();
    println!("setup        : seed sent, named test");

    let rep = bridge.inquire(None).unwrap_or_else(|e| {
        eprintln!("inquire error: {e}");
        std::process::exit(1);
    });
    println!(
        "inquire      : id {} family ({}, {}) member ({}, {}) model {} fw {}",
        rep.device_id,
        rep.family_code.0,
        rep.family_code.1,
        rep.member_code.0,
        rep.member_code.1,
        rep.model().unwrap_or("unknown"),
        rep.revision
    );

    let pm = bridge.preset_memory(None).unwrap();
    let sm = bridge.sample_memory(None).unwrap();
    println!(
        "memory       : preset {}/{} kB, sample {} MB total ~{} kB free",
        pm.free_kb,
        pm.total_kb,
        sm.total_mb,
        sm.free_10kb * 10
    );

    for n in 0..6 {
        let name = bridge
            .get_preset_name(n, None)
            .unwrap_or_else(|e| format!("error {e}"));
        println!("catalog {n:>3} : {name}");
    }

    println!(
        "name before  : {}",
        bridge.get_preset_name(PRESET, None).unwrap()
    );

    let old = bridge.dump_preset_old(PRESET, None, 3).unwrap();
    std::fs::write("ft_old.bin", &old).unwrap();
    println!("dump OLD     : {}", sig(&old));

    let (hdr, data) = bridge.dump_preset_new(PRESET, None, 3).unwrap();
    std::fs::write("ft_new.bin", &data).unwrap();
    println!(
        "dump NEW     : preset {} {}/{} {}",
        hdr.preset,
        data.len(),
        hdr.total_bytes,
        sig(&data)
    );

    match bridge.preset_num_voices(PRESET, None) {
        Ok(v) => println!("voices       : {v}"),
        Err(e) => println!("voices       : error {e}"),
    }
    match bridge.preset_num_links(PRESET, None) {
        Ok(v) => println!("links        : {v}"),
        Err(e) => println!("links        : error {e}"),
    }
    match bridge.preset_num_szones(PRESET, None) {
        Ok(v) => println!("szones       : {v}"),
        Err(e) => println!("szones       : error {e}"),
    }

    bridge.set_preset_name(PRESET, "FT-RUST-2").unwrap();
    println!(
        "name rename  : {}",
        bridge.get_preset_name(PRESET, None).unwrap()
    );

    bridge.send_program_change(PRESET, None, None).unwrap();
    println!("pc select    : sent program change for preset {PRESET}");

    let vol = bridge.get_parameter(1, None).unwrap();
    println!("volume       : {vol}");
    let r = bridge.get_parameter_range(1, None).unwrap();
    println!("volume range : {} .. {}", r.minimum, r.maximum);

    bridge.set_parameter(1, 6).unwrap();
    println!("volume set   : {}", bridge.get_parameter(1, None).unwrap());
    bridge.set_parameter(1, vol).unwrap();
    println!("volume back  : {}", bridge.get_parameter(1, None).unwrap());

    let retargeted = EosBridge::<Pair>::retarget_dump(&old, PRESET);
    let written = bridge.send_preset_old(&retargeted, true, None, 3).unwrap();
    println!("send back    : preset {}, {}", written, sig(&retargeted));
    println!(
        "name after   : {}",
        bridge.get_preset_name(PRESET, None).unwrap()
    );

    let old2 = bridge.dump_preset_old(PRESET, None, 3).unwrap();
    std::fs::write("ft_old2.bin", &old2).unwrap();
    println!("dump after   : {}", sig(&old2));
    println!(
        "roundtrip    : {}",
        if old == old2 {
            "identical"
        } else {
            "DIFFERENT"
        }
    );

    bridge.delete_preset(PRESET);
    match bridge.dump_preset_old(PRESET, None, 3) {
        Ok(old3) => {
            std::fs::write("ft_old3.bin", &old3).unwrap();
            println!("dump deleted : {}", sig(&old3));
        }
        Err(e) => println!("dump deleted : error {e}"),
    }

    match bridge.basic_channel(None) {
        Ok(ch) => println!("basic channel: {ch}"),
        Err(e) => println!("basic channel: error {e}"),
    }

    println!("done");
}
