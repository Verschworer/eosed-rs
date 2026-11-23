// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: Copyright (C) 2026  eosed contributors
//
// This file is part of eosed.  Original work.  GPL-2.0-or-later.
// Two-process routing check: B listens on a source port, A sends to the
// matching destination port. Run listener first.
use eosed::backend_midir::MidirBackend;
use eosed::bridge::Backend;
use std::time::{Duration, Instant};

fn main() {
    let role = std::env::args().nth(1).unwrap_or("listen".into());
    let mut b = MidirBackend::new();
    if role == "listen" {
        let ins = b.list_in();
        let i = ins
            .iter()
            .position(|n| n == "SSL V-MIDI Port 1 Source")
            .unwrap();
        let mut p = b.open_in(i).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            if let Some(m) = p.poll() {
                println!("got: {:?}", m);
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        println!("nothing received");
    } else {
        std::thread::sleep(Duration::from_secs(2));
        let outs = b.list_out();
        let o = outs
            .iter()
            .position(|n| n == "SSL V-MIDI Port 1 Destination")
            .unwrap();
        let mut out = b.open_out(o).unwrap();
        out.send(&[
            0xF0, 0x7E, 0x7F, 0x06, 0x01, 0x7C, 0x01, 0x04, 0x00, 0x05, 0xF7,
        ]);
        println!("sent inquiry-shaped SysEx");
    }
}
