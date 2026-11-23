// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: Copyright (C) 2026  eosed contributors
//
// Rust port of tools/carrier_check.py.  The verdict logic (refuse on swing,
// never on the cycle count) is preserved verbatim from upstream: the count is
// recorded, the synthetic swing decides.  The FFT is a direct radix-2 Cooley
// -Tukey over the segment truncated to a power of two; numpy's rfft on a
// power-of-two segment is the same transform.

use std::f64::consts::PI;
use std::io::Read;

fn equal_tempered(note: i64) -> f64 {
    440.0 * 2.0f64.powf((note - 69) as f64 / 12.0)
}

struct Wav {
    sr: usize,
    samples: Vec<f64>,
}

fn read_wav(path: &str) -> Result<Wav, String> {
    let mut data = Vec::new();
    let mut f = std::fs::File::open(path).map_err(|e| format!("cannot open {path}: {e}"))?;
    f.read_to_end(&mut data)
        .map_err(|e| format!("cannot read {path}: {e}"))?;
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let mut sr = 0usize;
    let mut channels = 1usize;
    let mut audio_format = 1u16;
    let mut pcm = Vec::new();
    let mut pos = 12;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
            as usize;
        let body_start = pos + 8;
        let body_end = (body_start + size).min(data.len());
        let body = &data[body_start..body_end];
        match id {
            b"fmt " if body.len() >= 16 => {
                audio_format = u16::from_le_bytes([body[0], body[1]]);
                channels = u16::from_le_bytes([body[2], body[3]]) as usize;
                sr = u32::from_le_bytes([body[4], body[5], body[6], body[7]]) as usize;
            }
            b"data" => pcm.extend_from_slice(body),
            _ => {}
        }
        pos = body_start + size;
        if size % 2 == 1 {
            pos += 1;
        }
    }
    if audio_format != 1 {
        return Err(format!(
            "unsupported audio format {audio_format} (need PCM16)"
        ));
    }
    if sr == 0 {
        return Err("no fmt chunk found".into());
    }
    let raw: Vec<i16> = pcm
        .chunks(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    let samples: Vec<f64> = raw
        .chunks(channels)
        .map(|ch| ch.iter().map(|s| *s as f64).sum::<f64>() / (channels as f64) / 32768.0)
        .collect();
    Ok(Wav { sr, samples })
}

/// complex FFT, in-place, radix-2, length N (power of two)
fn fft(z: &mut [(f64, f64)]) {
    let n = z.len();
    // bit reversal
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j += bit;
        if i < j {
            z.swap(i, j);
        }
    }
    let mut len = 2usize;
    while len <= n {
        let half = len / 2;
        let step = 2.0 * PI / (len as f64);
        for i in (0..n).step_by(len) {
            for k in 0..half {
                let ang = -step * (k as f64);
                let w = (ang.cos(), ang.sin());
                let a = z[i + k];
                let b = z[i + k + half];
                let wb = (b.0 * w.0 - b.1 * w.1, b.0 * w.1 + b.1 * w.0);
                z[i + k] = (a.0 + wb.0, a.1 + wb.1);
                z[i + k + half] = (a.0 - wb.0, a.1 - wb.1);
            }
        }
        len <<= 1;
    }
}

#[allow(clippy::needless_range_loop)] // k is the frequency-bin index, not an artifact
fn carrier_hz(w: &Wav, t0: f64, t1: f64, floor_hz: f64) -> Option<(f64, usize)> {
    let a = (t0 * w.sr as f64) as usize;
    let b = (t1 * w.sr as f64) as usize;
    if b <= a || b - a < 4096 {
        return None;
    }
    let len = b - a;
    let mut n = 1usize;
    while n * 2 <= len {
        n *= 2;
    }
    let seg = &w.samples[a..a + n];
    // hanning (symmetric, as numpy)
    let mut z: Vec<(f64, f64)> = Vec::with_capacity(n);
    for (i, &s) in seg.iter().enumerate() {
        let w_i = 0.5 * (1.0 - (2.0 * PI * (i as f64) / (n as f64 - 1.0)).cos());
        z.push((s * w_i, 0.0));
    }
    fft(&mut z);
    let sp: Vec<f64> = z[..=n / 2]
        .iter()
        .map(|c| (c.0 * c.0 + c.1 * c.1).sqrt())
        .collect();
    // fr[k] = k * sr / n
    let lo = ((floor_hz * n as f64 / w.sr as f64).ceil() as usize).min(sp.len() - 1);
    let mut k = lo;
    let mut best = sp[lo];
    for i in lo + 1..sp.len() {
        if sp[i] > best {
            best = sp[i];
            k = i;
        }
    }
    let mut d = 0.0;
    if k > 0 && k < sp.len() - 1 {
        let (a1, b1, c1) = (sp[k - 1], sp[k], sp[k + 1]);
        let den = a1 - 2.0 * b1 + c1;
        if den != 0.0 {
            d = 0.5 * (a1 - c1) / den;
        }
    }
    let fr_k = (k as f64) * (w.sr as f64) / (n as f64);
    let df = (w.sr as f64) / (n as f64);
    Some((fr_k + d * df, w.sr))
}

fn envelope_swing_db(f0: f64, smoothing_ms: f64, sr: usize, seconds: f64) -> f64 {
    let total = (seconds * sr as f64) as usize;
    let win = ((smoothing_ms * 1e-3 * sr as f64) as usize).max(1);
    let n = total / win;
    if n < 8 {
        return f64::NAN;
    }
    let mut max_e = f64::MIN;
    let mut min_e = f64::MAX;
    for j in 0..n {
        let mut acc = 0.0;
        for i in 0..win {
            let t = ((j * win + i) as f64) / (sr as f64);
            let s = (2.0 * PI * f0 * t).sin();
            acc += s * s;
        }
        let e = (acc / win as f64).sqrt();
        if e > max_e {
            max_e = e;
        }
        if e < min_e {
            min_e = e;
        }
    }
    let mn = min_e.max(1e-12);
    20.0 * (max_e / mn).log10()
}

fn verdict(swing: f64, cycles: f64) -> (String, String) {
    let tail = format!(
        "(swing measured on a synthetic tone at this f0 and window; {cycles:.3} cycles/window)"
    );
    if swing.is_nan() {
        return (
            "UNKNOWN".into(),
            format!("could not synthesise a long enough window {tail}"),
        );
    }
    if swing > 3.0 {
        return ("REFUSE".into(), format!("{swing:.2} dB of envelope swing on a CONSTANT tone. A -3 dB threshold can be crossed early by that, so any time measured here is biased. Use many whole cycles per window {tail}"));
    }
    if swing > 1.0 {
        return (
            "WARN".into(),
            format!("{swing:.2} dB of swing on a constant tone. Usable, not safe {tail}"),
        );
    }
    ("OK".into(), format!("{swing:.2} dB of swing {tail}"))
}

fn pyg(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{}", v)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut smoothing = 5.0;
    let mut t0 = 1.0;
    let mut t1 = 3.0;
    let mut octave = 0i64;
    let mut cents_tol = 50.0;
    let mut strict = false;
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--smoothing-ms" => {
                smoothing = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--window" => {
                t0 = args[i + 1].parse().unwrap();
                t1 = args[i + 2].parse().unwrap();
                i += 3;
            }
            "--octave" => {
                octave = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--cents-tolerance" => {
                cents_tol = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--strict" => {
                strict = true;
                i += 1;
            }
            other => {
                positional.push(other.to_string());
                i += 1;
            }
        }
    }
    if positional.len() < 2 {
        eprintln!("usage: carrier_check WAV NOTE [--smoothing-ms N] [--window T0 T1] [--octave N] [--cents-tolerance N] [--strict]");
        std::process::exit(2);
    }
    let wav = positional[0].clone();
    let note: i64 = positional[1].parse().unwrap();

    let w = match read_wav(&wav) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    let (f, sr) = match carrier_hz(&w, t0, t1, 8.0) {
        Some(x) => x,
        None => {
            println!("  too little audio in the analysis window");
            std::process::exit(2);
        }
    };
    let expected = equal_tempered(note) * 2.0f64.powf(octave as f64);
    let harmonic = (f / expected).round().max(1.0) as i64;
    let f0 = f / harmonic as f64;
    let cents = if expected > 0.0 {
        1200.0 * (f0 / expected).log2()
    } else {
        f64::NAN
    };
    let cycles = f0 * smoothing * 1e-3;
    println!("  sample rate (header)      {sr} Hz");
    println!("  carrier measured          {f:.2} Hz  (harmonic x{harmonic})");
    println!("  fundamental               {f0:.2} Hz");
    println!("  expected from note {:<3}    {:.2} Hz", note, expected);
    println!("  error                     {cents:+.1} cents");
    let swing = envelope_swing_db(f0, smoothing, sr, 4.0);
    let (v, why) = verdict(swing, cycles);
    println!("  cycles per {} ms window  {cycles:.3}", pyg(smoothing));
    println!("  envelope swing (synthetic) {swing:.2} dB");
    println!("  VERDICT                   {v} -- {why}");
    let off = cents.abs() > cents_tol;
    if off {
        println!("  PITCH MISS: {cents:+.1} cents is outside +/-{}. Check sample rate, wav header, analysis scaling and the preset's tuning BEFORE trusting any time measured from this file.", pyg(cents_tol));
    }
    if strict && (v == "REFUSE" || off) {
        std::process::exit(1);
    }
}
