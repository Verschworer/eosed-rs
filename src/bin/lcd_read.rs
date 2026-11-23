// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: Copyright (C) 2026  eosed contributors
//
// Rust port of tools/lcd_read.py: read the highlighted numeric field off the
// E4XT LCD. The field is white-on-black, so glyph pixels are the UNSET pixels
// inside the highlight block; glyphs segment on empty columns and are
// classified by exact bitmap match. An unrecognised glyph is reported as '?',
// never guessed.

use eosed::bridge::Backend;
use eosed::config::{Settings, DEFAULT_CONFIG_PATH};
use eosed::lcd;
use eosed::panel::{open_session, request_screen, PANEL_DESIGNATOR};
use std::collections::HashMap;
use std::time::{Duration, Instant};

struct Panel {
    out: Box<dyn eosed::bridge::OutPort>,
    inp: Box<dyn eosed::bridge::InPort>,
}

impl Panel {
    fn new(
        send_name: &str,
        recv_name: &str,
        backend: &mut eosed::backend_midir::MidirBackend,
    ) -> Panel {
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
        Panel { out, inp }
    }
    fn send(&mut self, frame: &[u8]) {
        if frame.len() >= 7 && frame[4] == PANEL_DESIGNATOR && frame[5] == 0x7F && frame[6] == 0x00
        {
            eprintln!("REFUSED: front-panel divert");
            std::process::exit(1);
        }
        self.out.send(frame);
        std::thread::sleep(Duration::from_millis(50));
    }
    fn drain(&mut self) {
        while self.inp.poll().is_some() {}
    }
    fn screen(&mut self, timeout: Duration) -> Option<Vec<Vec<u8>>> {
        self.drain();
        self.send(&request_screen(0).expect("screen request"));
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if let Some(msg) = self.inp.poll() {
                if msg.len() > 500 && msg[5] == 0x50 {
                    return lcd::decode_display(&msg);
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    }
}

/// (glyph-bitmap, (y0,y1,x0,x1)) for the highlighted value field.
type Field = (Option<Vec<Vec<u8>>>, Option<(usize, usize, usize, usize)>);

fn rate_field(a: &[Vec<u8>]) -> Field {
    let mut cand: Vec<usize> = Vec::new();
    for y in 5..26 {
        if y >= a.len() {
            break;
        }
        let mut max_run = 0usize;
        let mut run = 0usize;
        for &v in &a[y] {
            if v != 0 {
                run += 1;
                max_run = max_run.max(run);
            } else {
                run = 0;
            }
        }
        if max_run > 25 {
            cand.push(y);
        }
    }
    if cand.is_empty() {
        return (None, None);
    }
    let (y0, y1) = (cand[0], cand[cand.len() - 1]);
    let xs: Vec<usize> = (0..a[y0].len()).filter(|&x| a[y0][x] != 0).collect();
    if xs.is_empty() {
        return (None, None);
    }
    let (x0, x1) = (xs[0], xs[xs.len() - 1]);
    let g: Vec<Vec<u8>> = (y0..=y1)
        .map(|y| (x0..=x1).map(|x| 1 - a[y][x]).collect())
        .collect();
    (Some(g), Some((y0, y1, x0, x1)))
}

fn any_row(rows: &[Vec<u8>], y: usize) -> bool {
    rows[y].iter().any(|&v| v != 0)
}
fn any_col(rows: &[Vec<u8>], x: usize) -> bool {
    rows.iter().any(|r| r.get(x).copied().unwrap_or(0) != 0)
}

fn trim(s: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let rows: Vec<usize> = (0..s.len()).filter(|&y| any_row(&s, y)).collect();
    if rows.is_empty() {
        return s;
    }
    (rows[0]..=rows[rows.len() - 1])
        .map(|y| s[y].clone())
        .collect()
}

fn segment(g: &[Vec<u8>]) -> Vec<Vec<Vec<u8>>> {
    let width = g.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut out = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    for x in 0..width {
        if any_col(g, x) {
            run.push(x);
        } else if !run.is_empty() {
            out.push(
                (0..g.len())
                    .map(|y| {
                        run.iter()
                            .map(|&x| g[y].get(x).copied().unwrap_or(0))
                            .collect()
                    })
                    .collect(),
            );
            run.clear();
        }
    }
    if !run.is_empty() {
        out.push(
            (0..g.len())
                .map(|y| {
                    run.iter()
                        .map(|&x| g[y].get(x).copied().unwrap_or(0))
                        .collect()
                })
                .collect(),
        );
    }
    out.into_iter().map(trim).collect()
}

fn key(s: &[Vec<u8>]) -> String {
    let h = s.len();
    let w = s.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut flat = String::new();
    for row in s {
        for &v in row {
            flat.push((b'0' + v) as char);
        }
    }
    format!("{h}x{w}:{flat}")
}

fn load(path: &str) -> HashMap<String, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return HashMap::new(),
    };
    let mut lib = HashMap::new();
    // the templates file is one `"key": "char",` line per entry
    for line in text.lines() {
        let l = line.trim().trim_end_matches(',');
        if !l.starts_with('"') {
            continue;
        }
        // split on the first `": "` after the closing quote of the key
        if let Some(pos) = l.find("\": \"") {
            let k = l[1..pos].to_string();
            let rest = &l[pos + 4..];
            let v = rest.trim_end_matches('"').to_string();
            lib.insert(k, v);
        }
    }
    lib
}

fn save(path: &str, lib: &HashMap<String, String>) {
    let mut out = String::from("{\n");
    for (k, v) in lib {
        out.push_str(&format!("\"{k}\": \"{v}\",\n"));
    }
    out.push_str("}\n");
    std::fs::write(path, out).unwrap();
}

fn read_rate(a: &[Vec<u8>], lib: &HashMap<String, String>) -> (Option<String>, Vec<String>) {
    let (g, _) = rate_field(a);
    let g = match g {
        Some(g) => g,
        None => return (None, vec!["NO FIELD".into()]),
    };
    let mut chars = String::new();
    let mut unknown = Vec::new();
    for s in segment(&g) {
        let k = key(&s);
        match lib.get(&k) {
            Some(c) => chars.push_str(c),
            None => {
                chars.push('?');
                unknown.push(k);
            }
        }
    }
    (Some(chars), unknown)
}

fn learn(
    a: &[Vec<u8>],
    text: &str,
    lib: &mut HashMap<String, String>,
    path: &str,
) -> (bool, String) {
    let (g, _) = rate_field(a);
    let g = match g {
        Some(g) => g,
        None => return (false, "NO FIELD".into()),
    };
    let segs = segment(&g);
    if segs.len() != text.chars().count() {
        return (
            false,
            format!("{} glyphs vs {} chars", segs.len(), text.chars().count()),
        );
    }
    for (s, c) in segs.iter().zip(text.chars()) {
        lib.insert(key(s), c.to_string());
    }
    save(path, lib);
    (true, format!("learned {} glyphs", segs.len()))
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let glyphs = std::env::var("EOSED_LCD_GLYPHS").unwrap_or_else(|_| "lcd_glyphs.json".into());
    let mut bitmap_file: Option<String> = None;
    let mut learn_text: Option<String> = None;
    let mut live = false;
    let mut render = false;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--bitmap" => {
                bitmap_file = Some(argv[i + 1].clone());
                i += 2;
            }
            "--render" => {
                render = true;
                i += 1;
            }
            "--learn" => {
                learn_text = Some(argv[i + 1].clone());
                i += 2;
            }
            "--panel" => {
                live = true;
                i += 1;
            }
            _ => {
                eprintln!("usage: lcd_read [--bitmap FILE | --panel] [--learn TEXT] [--render]");
                std::process::exit(2);
            }
        }
    }
    let bitmap: Vec<Vec<u8>> = if let Some(f) = &bitmap_file {
        let raw: Vec<Vec<i64>> =
            serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap();
        raw.into_iter()
            .map(|r| r.into_iter().map(|x| x as u8).collect())
            .collect()
    } else if live {
        let mut settings = Settings::new("eosed", DEFAULT_CONFIG_PATH);
        let _ = eosed::config::load_cache_depth(Some(DEFAULT_CONFIG_PATH), &mut settings);
        let (send, recv) =
            match eosed::config::load_last_ports(Some(DEFAULT_CONFIG_PATH), &mut settings) {
                Some(p) => p,
                None => {
                    eprintln!("no cached panel ports; run eosed_cli ports first");
                    std::process::exit(1);
                }
            };
        let mut backend = eosed::backend_midir::MidirBackend::new();
        let mut panel = Panel::new(&send, &recv, &mut backend);
        panel.send(&open_session(0).expect("panel session frame"));
        std::thread::sleep(Duration::from_millis(350));
        panel.drain();
        match panel.screen(Duration::from_secs(3)) {
            Some(bm) => bm,
            None => {
                eprintln!("no panel screen reply");
                std::process::exit(1);
            }
        }
    } else {
        eprintln!("usage: lcd_read [--bitmap FILE | --panel] [--learn TEXT]");
        std::process::exit(2);
    };

    if render {
        for line in lcd::to_halfblocks(&bitmap) {
            println!("{}", line.trim_end());
        }
        return;
    }

    if let Some(text) = learn_text {
        let mut lib = load(&glyphs);
        let (ok, msg) = learn(&bitmap, &text, &mut lib, &glyphs);
        println!("{msg}");
        if !ok {
            std::process::exit(1);
        }
    } else {
        let lib = load(&glyphs);
        let (text, unknown) = read_rate(&bitmap, &lib);
        match text {
            Some(t) => println!("{t}"),
            None => println!("(no field)"),
        }
        if !unknown.is_empty() {
            println!("unrecognised glyphs:");
            for k in unknown {
                println!("  {k}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::needless_range_loop)] // rectangle fill by coordinates
    fn reads_glyphs_from_a_highlight_block() {
        // mimic the real field: a wide highlight block, glyphs are the UNSET
        // pixels inside it. Block rows 10..16, cols 20..80 -> every row has a
        // highlight run > 25.
        let mut bm = vec![vec![0u8; 240]; 64];
        for r in 10..17 {
            for c in 20..=80 {
                bm[r][c] = 1;
            }
        }
        let pat: Vec<u8> = b"01110101010101".to_vec(); // the '1' glyph, 7x2
        for r in 0..7 {
            for c in 0..2 {
                if pat[r * 2 + c] == b'1' {
                    bm[10 + r][26 + c] = 0;
                }
            }
        }
        bm[10][31] = 0; // the '.' glyph, 1x1
        let mut lib = HashMap::new();
        lib.insert("7x2:01110101010101".to_string(), "1".to_string());
        lib.insert("1x1:1".to_string(), ".".to_string());
        let (text, unknown) = read_rate(&bm, &lib);
        assert_eq!(text, Some("1.".to_string()));
        assert!(
            unknown.is_empty(),
            "glyphs from the shipped templates must match exactly"
        );
    }
}
