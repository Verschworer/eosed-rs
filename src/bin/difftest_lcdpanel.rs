// SPDX-License-Identifier: GPL-2.0-or-later
// Differential harness for lcd + panel: deterministic pseudo-random frames,
// rendered/decoded and printed for diffing against the Python original.

use eosed::{lcd, panel};

fn lcg(seed: u64) -> impl Iterator<Item = u8> {
    let mut s = seed;
    std::iter::from_fn(move || {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        Some(((s >> 33) as u8) & 0x7F)
    })
}

fn main() {
    // 1. LCD: random 50h full-screen frames, render all three ways
    for seed in 1..=5u64 {
        let mut frame = vec![0xF0u8, 0x18, 0x7F, 0x01, 0x02, 0x50];
        frame.extend((0..10).map(|i| (i as u8) & 0x7F));
        frame.extend(lcg(seed).take(2195));
        frame.push(0xF7);
        let bm = lcd::decode_display(&frame).expect("full frame");
        for line in lcd::to_ascii(&bm) {
            println!("ascii|{seed}|{line}");
        }
        for line in lcd::to_braille(&bm, false) {
            println!("braille|{seed}|{line}");
        }
        for line in lcd::to_quadrants(&bm) {
            println!("quadrants|{seed}|{line}");
        }
        println!("partial|{seed}|{}", lcd::is_partial(&frame));
    }

    // 2. LCD: partial frames and refresh classification
    for len in [40usize, 86, 100, 101, 500, 1500, 2212] {
        let mut frame = vec![0xF0u8, 0x18, 0x7F, 0x00, 0x00, 0x50];
        frame.extend((0..10).map(|i| i as u8));
        frame.extend(std::iter::repeat_n(0x55u8, len));
        frame.push(0xF7);
        let dec = match lcd::classify_update(Some(&frame)) {
            lcd::RefreshDecision::Idle => "idle",
            lcd::RefreshDecision::Use => "use",
            lcd::RefreshDecision::Escalate => "escalate",
        };
        println!("classify|{len}|{dec}|{}", lcd::is_partial(&frame));
    }

    // 3. Panel: build every frame kind over keys and deltas
    for dev in [0u8, 17, 127] {
        println!(
            "open|{}",
            panel::open_session(dev)
                .unwrap()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        for key in 0x58u8..=0x7F {
            println!(
                "press|{key}|{}",
                panel::press(dev, key)
                    .unwrap()
                    .iter()
                    .map(|f| f
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" "))
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
            println!("label|{key}|{}", panel::key_label(key).unwrap_or(""));
        }
        for delta in [-8192i32, -7000, -1234, -1, 0, 1, 42, 8191] {
            let f = panel::dial(dev, delta).unwrap();
            println!(
                "dial|{delta}|{}|{}",
                f.iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(","),
                panel::parse_dial(&f).unwrap()
            );
        }
        println!(
            "screen|{}",
            panel::request_screen(dev)
                .unwrap()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        println!(
            "update|{}",
            panel::update_screen(dev)
                .unwrap()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(",")
        );
        println!(
            "query|{}",
            panel::query_state(dev)
                .unwrap()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(",")
        );
    }
}
