// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: Copyright (C) 2026  eosed contributors

//! eosed-rs — Rust port of eosed: a terminal tool for the E-mu EOS sampler
//! family (E4, E4XT, E4XT Ultra, E6400, …) driven over MIDI System Exclusive.

pub mod backend_midir;
pub mod bridge;
pub mod config;
pub mod demo;
pub mod lcd;
pub mod messages;
pub mod panel;
pub mod params;
mod params_data;
pub mod tui_logic;

#[test]
fn every_source_file_carries_the_spdx_header() {
    let mut stack = vec!["src".to_string()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p.to_string_lossy().to_string());
            } else if p.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&p).unwrap();
                assert!(
                    text.contains("SPDX-License-Identifier: GPL-2.0-or-later"),
                    "{p:?} is missing the GPL SPDX header"
                );
            }
        }
    }
}
