// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: Copyright (C) 2026  eosed contributors
//
// Rust twin of tools/load_bank.py: load a disk bank into RAM over the panel
// protocol, verifiably. The two recorded traps are kept as guards:
//  1. F4 on an already-open LOAD dialog is MERGE, not "close" -- so F4 is
//     never pressed without first classifying the screen.
//  2. There are THREE screens (MAIN / LOAD / destroy-CONFIRM), not two; a
//     two-way classifier misread the confirm as a finished load. Anything
//     else is UNKNOWN and stops the run.
// The destroy-confirm is never answered automatically: --destroy is required.
// Verification is by reading the RAM catalog back afterwards.

use eosed::backend_midir::MidirBackend;
use eosed::bridge::{autodetect, Backend, SEND_GAP};
use eosed::config::{Settings, DEFAULT_CONFIG_PATH};
use eosed::lcd;
use eosed::panel::{open_session, press, request_screen, Key, PANEL_DESIGNATOR};
use std::thread::sleep;
use std::time::{Duration, Instant};

const DEV: u8 = 5;
const SETTLE: Duration = Duration::from_millis(450);

fn longest_run(line: &[u8]) -> usize {
    let mut best = 0;
    let mut run = 0;
    for &v in line {
        if v != 0 {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

fn runs(bitmap: &[Vec<u8>]) -> (usize, usize) {
    let h = bitmap
        .iter()
        .map(|row| longest_run(row.as_slice()))
        .max()
        .unwrap_or(0);
    let height = bitmap.first().map(|r| r.len()).unwrap_or(0);
    let mut v = 0;
    for col in 0..height {
        let mut run = 0;
        let mut best = 0;
        for row in bitmap {
            if row.get(col).copied().unwrap_or(0) != 0 {
                run += 1;
                best = best.max(run);
            } else {
                run = 0;
            }
        }
        v = v.max(best);
    }
    (v, h)
}

fn classify(bitmap: Option<Vec<Vec<u8>>>) -> (String, (Option<usize>, Option<usize>)) {
    match bitmap {
        None => ("UNKNOWN".into(), (None, None)),
        Some(bm) => {
            let (v, h) = runs(&bm);
            if v >= 40 && h >= 180 {
                ("LOAD".into(), (Some(v), Some(h)))
            } else if (20..40).contains(&v) && h >= 100 {
                ("CONFIRM".into(), (Some(v), Some(h)))
            } else if v < 20 && h < 100 {
                ("MAIN".into(), (Some(v), Some(h)))
            } else {
                ("UNKNOWN".into(), (Some(v), Some(h)))
            }
        }
    }
}

struct Panel {
    out: Box<dyn eosed::bridge::OutPort>,
    inp: Box<dyn eosed::bridge::InPort>,
}

impl Panel {
    fn new(send_name: &str, recv_name: &str, backend: &mut MidirBackend) -> Panel {
        let outs = backend.list_out();
        let ins = backend.list_in();
        let oi = outs
            .iter()
            .position(|n| n == send_name)
            .expect("no such output port");
        let ii = ins
            .iter()
            .position(|n| n == recv_name)
            .expect("no such input port");
        let out = backend
            .open_out(oi)
            .expect("could not open panel send port");
        let inp = backend
            .open_in(ii)
            .expect("could not open panel receive port");
        let session = open_session(DEV).expect("panel session frame");
        let mut panel = Panel { out, inp };
        panel.send(&session);
        sleep(Duration::from_millis(350));
        panel.drain();
        panel
    }
    fn send(&mut self, frame: &[u8]) {
        // The front-panel divert must never leave this process, whatever
        // built the frame: asserted on the wire, not at the call sites.
        if frame.len() >= 7 && frame[4] == PANEL_DESIGNATOR && frame[5] == 0x7F && frame[6] == 0x00
        {
            eprintln!("REFUSED: front-panel divert");
            std::process::exit(1);
        }
        self.out.send(frame);
        sleep(Duration::from_millis(50));
    }
    fn drain(&mut self) {
        while self.inp.poll().is_some() {}
    }
    fn key(&mut self, k: Key, settle: Duration) {
        for frame in press(DEV, k as u8).expect("press frames") {
            self.send(&frame);
        }
        sleep(settle);
        self.drain();
    }
    fn screen(&mut self, timeout: Duration) -> Option<Vec<Vec<u8>>> {
        self.drain();
        self.send(&request_screen(DEV).expect("screen request"));
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if let Some(msg) = self.inp.poll() {
                if msg.len() > 500 && msg[5] == 0x50 {
                    return lcd::decode_display(&msg);
                }
            }
            sleep(Duration::from_millis(2));
        }
        None
    }
    fn state(&mut self) -> (String, (Option<usize>, Option<usize>)) {
        classify(self.screen(Duration::from_secs(3)))
    }
}

// unused placeholder removed

fn fmt_runs(m: (Option<usize>, Option<usize>)) -> String {
    match m {
        (None, None) => "(None, None)".into(),
        (Some(v), Some(h)) => format!("({v}, {h})"),
        _ => "(?)".into(),
    }
}

fn to_main(panel: &mut Panel) {
    for _ in 0..4 {
        let (st, m) = panel.state();
        println!("    screen: {st} {}", fmt_runs(m));
        if st == "MAIN" {
            return;
        }
        if st == "UNKNOWN" {
            eprintln!(
                "  unrecognised screen {}; refusing to press keys blind",
                fmt_runs(m)
            );
            std::process::exit(1);
        }
        panel.key(Key::F1, Duration::from_millis(800)); // Cancel on both dialogs
    }
    eprintln!("  could not reach the main preset page");
    std::process::exit(1);
}

fn load(panel: &mut Panel, drive_steps: i32, bank_steps: i32, destroy: bool) -> bool {
    to_main(panel);
    panel.key(Key::F4, Duration::from_millis(800)); // F4 on MAIN opens LOAD
    let (st, m) = panel.state();
    if st != "LOAD" {
        eprintln!(
            "  F4 did not open the LOAD dialog (got {st} {})",
            fmt_runs(m)
        );
        std::process::exit(1);
    }
    println!("    LOAD dialog open");
    for _ in 0..drive_steps.abs() {
        panel.key(if drive_steps > 0 { Key::Inc } else { Key::Dec }, SETTLE);
    }
    if drive_steps != 0 || bank_steps != 0 {
        panel.key(Key::CursorDown, SETTLE);
        panel.key(Key::CursorDown, SETTLE);
    }
    for _ in 0..bank_steps.abs() {
        panel.key(if bank_steps > 0 { Key::Inc } else { Key::Dec }, SETTLE);
    }
    println!("    firing Load (F6)");
    panel.key(Key::F6, Duration::from_millis(1000));
    let start = Instant::now();
    while Instant::now() - start < Duration::from_secs(180) {
        let (st, _m) = panel.state();
        if st == "CONFIRM" {
            if !destroy {
                println!("    'Destroys current RAM Bank' -- RAM is NOT empty.");
                println!("    CANCELLING: pass --destroy to answer this deliberately.");
                panel.key(Key::F1, Duration::from_millis(800));
                return false;
            }
            println!("    confirming (--destroy given)");
            panel.key(Key::F6, Duration::from_millis(1000));
            continue;
        }
        if st == "MAIN" {
            println!(
                "    back on the main page after {:.1}s",
                (Instant::now() - start).as_secs_f64()
            );
            return true;
        }
        sleep(Duration::from_secs(1));
    }
    eprintln!("  timed out waiting for the load to finish");
    std::process::exit(1);
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut drive_steps = 0;
    let mut bank_steps = 0;
    let mut destroy = false;
    let mut state_only = false;
    let mut config = DEFAULT_CONFIG_PATH.to_string();
    let mut device_id: u8 = DEV; // the bench device; override with --device-id
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--drive-steps" => {
                drive_steps = argv[i + 1].parse().unwrap_or(0);
                i += 2;
            }
            "--bank-steps" => {
                bank_steps = argv[i + 1].parse().unwrap_or(0);
                i += 2;
            }
            "--device-id" => {
                device_id = argv[i + 1].parse().unwrap_or(5);
                i += 2;
            }
            "--destroy" => {
                destroy = true;
                i += 1;
            }
            "--state-only" => {
                state_only = true;
                i += 1;
            }
            "--config" => {
                config = argv[i + 1].clone();
                i += 2;
            }
            _ => {
                eprintln!("usage: load_bank [--drive-steps N] [--bank-steps N] [--destroy] [--state-only] [--config PATH]");
                std::process::exit(2);
            }
        }
    }

    let mut backend = MidirBackend::new();
    let ports = eosed::config::load_last_ports(Some(&config), &mut Settings::new("eosed", &config));
    let (send, recv) = match ports {
        Some(p) => p,
        None => {
            eprintln!("no cached ports in {config}; run eosed_cli hardware first");
            std::process::exit(1);
        }
    };
    let mut panel = Panel::new(&send, &recv, &mut backend);
    if state_only {
        let (st, m) = panel.state();
        println!("  screen: {st}  (vrun,hrun)={}", fmt_runs(m));
        return;
    }
    let ok = load(&mut panel, drive_steps, bank_steps, destroy);
    drop(panel);
    sleep(Duration::from_millis(300));
    if !ok {
        std::process::exit(2);
    }
    println!("  verifying against RAM (the panel cannot answer this):");
    let mut settings = Settings::new("eosed", &config);
    let mut bridge = match autodetect(
        &mut backend,
        SEND_GAP,
        None,
        Some(device_id),
        Duration::from_secs(2),
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
    let names = bridge.catalog_presets((0, 24), None, None);
    let mut keys: Vec<u32> = names.keys().copied().collect();
    keys.sort();
    for k in keys {
        println!("    {k:4}  {}", names[&k]);
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    // Measured signatures from the tool's docstring (E4XT Ultra, EOS 4.70):
    // MAIN 17/35, LOAD 51/221, CONFIRM 27/148.
    #[allow(clippy::needless_range_loop)] // rectangle fill by coordinates, mirrors the docstring
    fn synthetic(vrun: usize, hrun: usize) -> Vec<Vec<u8>> {
        let mut bm = vec![vec![0u8; 240]; 64];
        for r in 0..vrun {
            bm[r][0] = 1;
        }
        for c in 0..hrun {
            bm[0][c] = 1;
        }
        bm
    }

    #[test]
    fn classify_measured_screens() {
        assert_eq!(runs(&synthetic(51, 221)), (51, 221), "runs sanity");
        assert_eq!(classify(Some(synthetic(17, 35))).0, "MAIN");
        assert_eq!(classify(Some(synthetic(51, 221))).0, "LOAD");
        assert_eq!(classify(Some(synthetic(27, 148))).0, "CONFIRM");
        assert_eq!(classify(None).0, "UNKNOWN");
        assert_eq!(classify(None).1, (None, None));
        assert_eq!(classify(Some(synthetic(30, 50))).0, "UNKNOWN");
    }
}
