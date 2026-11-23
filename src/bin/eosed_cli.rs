// SPDX-License-Identifier: GPL-2.0-or-later
// Rust twin of eosed/cli.py — command-line explorer for the EOS editor
// protocol over a real MIDI port (midir: CoreMIDI/ALSA/WinMM).
// Same subcommands and exit-code contract: 0 worked, 1 failed, 2 bad args.

use eosed::backend_midir::MidirBackend;
use eosed::bridge::{
    autodetect, standard_bridge, Backend, BridgeLike, EosBridge, Pair, AUTODETECT_TIMEOUT,
    DEFAULT_TIMEOUT, SEND_GAP,
};
use eosed::config::{Settings, DEFAULT_CONFIG_PATH};
use eosed::demo::Demo;
use eosed::messages as m;
use eosed::params;
use std::time::Duration;

struct Args {
    port: Option<String>,
    device_id: Option<u8>,
    timeout: Duration,
    config: String,
    gap: Duration,
    write_gap: Option<Duration>,
    command: String,
    positional: Vec<String>,
    flags: std::collections::HashSet<String>,
    range: (u32, u32),
    preset: Option<u32>,
}

fn die_usage(msg: &str) -> ! {
    eprintln!("error: {msg}");
    eprintln!("usage: eosed_cli [--port NAME] [--device-id N] [--timeout SECS] [--config PATH] [--gap MS] [--write-gap MS] <command> [args]");
    eprintln!("commands: ports, hardware, config, memory, catalog [--range LO-HI] [--progress], get PARAM, dump PRESET OUTPUT [--new-format], send FILE [--preset N] [--allow-write] [--yes]");
    std::process::exit(2);
}

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1);
}

fn parse_duration_ms(text: &str) -> Duration {
    match text.parse::<u64>() {
        Ok(ms) => Duration::from_millis(ms),
        Err(_) => die_usage(&format!("expected a number of milliseconds, got {text:?}")),
    }
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut a = Args {
        port: None,
        device_id: None,
        timeout: DEFAULT_TIMEOUT,
        config: DEFAULT_CONFIG_PATH.to_string(),
        gap: SEND_GAP,
        write_gap: None,
        command: String::new(),
        positional: Vec::new(),
        flags: std::collections::HashSet::new(),
        range: (0, 127),
        preset: None,
    };
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        let mut take_next = |name: &str| -> String {
            if i + 1 >= argv.len() {
                die_usage(&format!("{name} needs a value"));
            }
            let v = argv[i + 1].clone();
            i += 2;
            v
        };
        match arg.as_str() {
            "--port" => a.port = Some(take_next("--port")),
            "--device-id" => {
                let v = take_next("--device-id");
                match v.parse::<u32>() {
                    Ok(n) if n <= 127 => a.device_id = Some(n as u8),
                    _ => die_usage(&format!("--device-id must be 0-127, got {v}")),
                }
            }
            "--timeout" => {
                let v = take_next("--timeout");
                match v.parse::<f64>() {
                    Ok(s) if s > 0.0 => {
                        a.timeout = Duration::new(s as u64, ((s - s.floor()) * 1e9) as u32)
                    }
                    _ => die_usage(&format!(
                        "--timeout must be a positive number of seconds, got {v}"
                    )),
                }
            }
            "--config" => a.config = take_next("--config"),
            "--gap" => a.gap = parse_duration_ms(&take_next("--gap")),
            "--write-gap" => a.write_gap = Some(parse_duration_ms(&take_next("--write-gap"))),
            "--range" => {
                let v = take_next("--range");
                let (lo, hi) = v.split_once('-').unwrap_or(("", ""));
                match (lo.parse::<u32>(), hi.parse::<u32>()) {
                    (Ok(l), Ok(h)) if l <= h => a.range = (l, h),
                    _ => die_usage(&format!("expected --range LOW-HIGH, got {v:?}")),
                }
            }
            "--preset" => {
                let v = take_next("--preset");
                match v.parse::<u32>() {
                    Ok(n) => a.preset = Some(n),
                    Err(_) => die_usage(&format!("--preset must be an integer, got {v}")),
                }
            }
            "--allow-write" | "--yes" | "--new-format" | "--progress" | "--demo" => {
                a.flags.insert(arg.clone());
                i += 1;
            }
            _ => {
                if a.command.is_empty() {
                    a.command = arg.clone();
                } else {
                    a.positional.push(arg.clone());
                }
                i += 1;
            }
        }
    }
    if a.command.is_empty() {
        die_usage("no command given");
    }
    a
}

// EosBridge.standard twin: one bidirectional port name, concrete device id.
fn cli_standard_bridge(
    backend: &mut MidirBackend,
    name: &str,
    device_id: u8,
    timeout: Duration,
    gap: Duration,
    write_gap: Option<Duration>,
) -> EosBridge<Pair> {
    match standard_bridge(backend, name, gap, write_gap, device_id, timeout) {
        Ok(b) => b,
        Err(e) => die(&format!("could not connect to {name:?}: {e}")),
    }
}

fn cmd_ports(backend: &mut MidirBackend) {
    let ins = backend.list_in();
    let outs = backend.list_out();
    let both: std::collections::HashSet<String> =
        ins.iter().filter(|i| outs.contains(i)).cloned().collect();
    for (label, names) in [("inputs", &ins), ("outputs", &outs)] {
        println!("{label}:");
        if names.is_empty() {
            println!("  (none)");
            continue;
        }
        for name in names {
            let suffix = if both.contains(name) {
                "   [bidirectional]"
            } else {
                ""
            };
            println!("  {name}{suffix}");
        }
    }
    println!();
    println!("Bidirectional ports are the standard-rig candidates: a device that can be asked a question has both an input and an output. `eoscli hardware` asks each one who is there.");
}

fn cmd_hardware(bridge: &mut dyn BridgeLike) {
    let reply = match bridge.inquire(None) {
        Ok(r) => r,
        Err(e) => die(&e.to_string()),
    };
    println!("device id      : {}", reply.device_id);
    println!(
        "family code    : ({}, {})",
        reply.family_code.0, reply.family_code.1
    );
    println!(
        "member code    : ({}, {})",
        reply.member_code.0, reply.member_code.1
    );
    println!("model          : {}", reply.model().unwrap_or("unknown"));
    println!("firmware       : {}", reply.revision);
}

fn cmd_config(bridge: &mut dyn BridgeLike) {
    let cfg = match bridge.configuration(None) {
        Ok(c) => c,
        Err(e) => die(&e.to_string()),
    };
    let flags = cfg.option_flags();
    let pybool = |b: bool| if b { "True" } else { "False" };
    println!("RAM            : {} MB", cfg.ram_mb);
    println!("128 voices     : {}", pybool(flags.voices_128));
    println!("FX card        : {}", pybool(flags.fx_card));
    println!("MIDI card      : {}", pybool(flags.midi_card));
    println!("Octopus card   : {}", pybool(flags.octopus_card));
    println!("Digital I/O    : {}", pybool(flags.digital_io));
    match bridge.extended_configuration(None) {
        Ok(ext) => {
            let ef = ext.option_flags();
            println!("ROM            : {} MB", ext.rom_mb);
            println!("Flash          : {} MB", ext.flash_mb);
            println!("Preset Flash   : {}", pybool(ef.preset_flash));
            println!("ADAT I/O       : {}", pybool(ef.adat_io));
        }
        Err(e) => eprintln!("error: extended configuration: {e}"),
    }
}

fn cmd_memory(bridge: &mut dyn BridgeLike) {
    let preset = match bridge.preset_memory(None) {
        Ok(m) => m,
        Err(e) => die(&e.to_string()),
    };
    let sample = match bridge.sample_memory(None) {
        Ok(m) => m,
        Err(e) => die(&e.to_string()),
    };
    println!(
        "Preset memory  : {} / {} kB free",
        preset.free_kb, preset.total_kb
    );
    println!(
        "Sample memory  : {} MB total, ~{} kB free",
        sample.total_mb,
        sample.free_10kb * 10
    );
}

fn cmd_catalog(bridge: &mut dyn BridgeLike, a: &Args) {
    let progress = a.flags.contains("--progress");
    let scan = |n: u32| eprintln!("\r  scanning preset {n}/{}", a.range.1);
    let progress_fn: Option<&dyn Fn(u32)> = if progress { Some(&scan) } else { None };
    let names = bridge.catalog_presets((a.range.0, a.range.1 + 1), None, progress_fn);
    if progress {
        println!();
    }
    let mut keys: Vec<u32> = names.keys().copied().collect();
    keys.sort();
    for number in keys {
        println!("{number:4}  {}", names[&number]);
    }
}

fn cmd_get(bridge: &mut dyn BridgeLike, a: &Args) {
    let param_text = match a.positional.first() {
        Some(p) => p.clone(),
        None => die_usage("get needs a parameter id or name"),
    };
    let param = match param_text.trim_start_matches('-').parse::<u32>() {
        Ok(id) => params::by_id(id),
        Err(_) => params::by_name(&param_text),
    };
    let param = match param {
        Some(p) => p,
        None => die(&format!("unknown parameter {param_text:?}")),
    };
    let value = match bridge.get_parameter(param.id, None) {
        Ok(v) => v,
        Err(e) => die(&e.to_string()),
    };
    let mut line = format!(
        "{} (id {}) = {}",
        param.name,
        param.id,
        params::describe_value(param, value as i32)
    );
    if let Some(unit) = param.unit {
        line.push(' ');
        line.push_str(unit);
    }
    println!("{line}");
    match bridge.get_parameter_range(param.id, None) {
        Ok(rng) => println!(
            "  device range   : {} .. {} (default {})",
            rng.minimum, rng.maximum, rng.default
        ),
        Err(_) => println!(
            "  static range   : {} .. {} (spec, not device-verified)",
            param.min, param.max
        ),
    }
}

fn cmd_dump(bridge: &mut dyn BridgeLike, a: &Args) {
    let preset: u32 = match a.positional.first().and_then(|p| p.parse::<u32>().ok()) {
        Some(p) => p,
        None => die_usage("dump needs a preset number"),
    };
    let output = match a.positional.get(1) {
        Some(o) => o.clone(),
        None => die_usage("dump needs an output file path"),
    };
    let data: Vec<u8> = if a.flags.contains("--new-format") {
        match bridge.dump_preset_new(preset, None, 3) {
            Ok((header, data)) => {
                println!(
                    "NEW format dump: preset {}, {}/{} bytes",
                    header.preset,
                    data.len(),
                    header.total_bytes
                );
                data
            }
            Err(e) => die(&e.to_string()),
        }
    } else {
        match bridge.dump_preset_old(preset, None, 3) {
            Ok(data) => {
                println!("OLD format dump: preset {preset}, {} bytes", data.len());
                data
            }
            Err(e) => die(&e.to_string()),
        }
    };
    if let Err(e) = std::fs::write(&output, &data) {
        die(&format!("could not write {output}: {e}"));
    }
    println!("wrote {output}");
}

fn cmd_rename(bridge: &mut dyn BridgeLike, a: &Args) {
    let preset: u32 = match a.positional.first().and_then(|p| p.parse::<u32>().ok()) {
        Some(p) => p,
        None => die_usage("rename needs a preset number"),
    };
    let name = match a.positional.get(1) {
        Some(n) => n.clone(),
        None => die_usage("rename needs a new name"),
    };
    if !a.flags.contains("--allow-write") {
        die("rename overwrites a preset name; pass --allow-write to confirm");
    }
    match bridge.set_preset_name(preset, &name) {
        Ok(()) => {
            println!("  renamed        : preset {preset} -> '{name}'");
            match bridge.get_preset_name(preset, None) {
                Ok(readback) => println!("  slot now reads : '{readback}'"),
                Err(e) => println!("  slot now reads : (name read-back failed: {e})"),
            }
        }
        Err(e) => die(&e.to_string()),
    }
}

fn cmd_send(bridge: &mut dyn BridgeLike, a: &Args) {
    let file = match a.positional.first() {
        Some(f) => f.clone(),
        None => die_usage("send needs a preset dump file"),
    };
    let data = match std::fs::read(&file) {
        Ok(d) => d,
        Err(e) => die(&format!("could not read {file}: {e}")),
    };
    if data.len() < 2 {
        die(&format!("{file}: too short to be a preset dump"));
    }
    let came_from = EosBridge::<Pair>::dump_target(&data);
    let preset = a.preset.unwrap_or(came_from);
    let data = if preset != came_from {
        EosBridge::<Pair>::retarget_dump(&data, preset)
    } else {
        data
    };
    println!("  file           : {file} ({} bytes)", data.len());
    println!("  dump was for   : preset {came_from}");
    println!("  will overwrite : preset {preset}");
    match bridge.get_preset_name(preset, None) {
        Ok(name) => println!("  that slot now  : '{}'", name),
        Err(e) => println!(
            "  that slot now  : unreadable ({}) -- proceed only if you know what is there",
            e.error_class()
        ),
    }
    if !a.flags.contains("--allow-write") {
        die("refusing: this overwrites the whole preset slot. Re-run with --allow-write once you have checked the target above.");
    }
    if !a.flags.contains("--yes") {
        print!("  type the preset number {preset} to confirm: ");
        std::io::Write::flush(&mut std::io::stdout()).ok();
        let mut typed = String::new();
        if std::io::stdin().read_line(&mut typed).is_err() {
            die("not confirmed; nothing sent");
        }
        if typed.trim() != preset.to_string() {
            die("not confirmed; nothing sent");
        }
    }
    match bridge.send_preset_old(&data, true, None, 3) {
        Ok(written) => {
            println!("  sent           : preset {written}, {} bytes", data.len());
            match bridge.get_preset_name(written, None) {
                Ok(name) => println!("  slot now reads : '{}'", name),
                Err(_) => println!("  slot now reads : (name read-back failed)"),
            }
        }
        Err(e) => die(&e.to_string()),
    }
}

fn main() {
    let a = parse_args();
    if a.command == "ports" {
        let mut backend = MidirBackend::new();
        cmd_ports(&mut backend);
        return;
    }
    let mut backend = MidirBackend::new();
    if a.flags.contains("--demo") {
        // The canned device: no MIDI port is ever opened, but every command
        // runs through the identical dispatch and formatting.
        let mut demo = Demo::new();
        dispatch(&mut demo, &a);
        return;
    }
    let mut bridge = if let Some(port) = &a.port {
        cli_standard_bridge(
            &mut backend,
            port,
            a.device_id.unwrap_or(m::DEFAULT_DEVICE_ID),
            a.timeout,
            a.gap,
            a.write_gap,
        )
    } else {
        match autodetect(
            &mut backend,
            a.gap,
            a.write_gap,
            a.device_id,
            AUTODETECT_TIMEOUT,
            Some(&a.config),
            &mut Settings::new("eosed", &a.config.clone()),
            &|name| eprintln!("  trying {name} ..."),
        ) {
            Ok(b) => b,
            Err(e) => die(&e.to_string()),
        }
    };
    dispatch(&mut bridge, &a);
}

fn dispatch(bridge: &mut dyn BridgeLike, a: &Args) {
    match a.command.as_str() {
        "hardware" | "inquire" => cmd_hardware(bridge),
        "rename" => cmd_rename(bridge, a),
        "config" => cmd_config(bridge),
        "memory" => cmd_memory(bridge),
        "catalog" => cmd_catalog(bridge, a),
        "get" => cmd_get(bridge, a),
        "dump" => cmd_dump(bridge, a),
        "send" => cmd_send(bridge, a),
        _ => die_usage(&format!("unknown command {:?}", a.command)),
    }
}
