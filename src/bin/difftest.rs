// SPDX-License-Identifier: GPL-2.0-or-later
// Differential harness: prints describe_value(id, value) for a sampled set
// so the output can be diffed against the Python original.

use eosed::params;

fn samples(min: i32, max: i32, default: Option<i32>) -> Vec<i32> {
    let mut v = Vec::new();
    if max - min <= 400 {
        v.extend(min..=max);
    } else {
        v.push(min);
        v.push(max);
        for extra in [-1, 0, 1] {
            if extra >= min && extra <= max {
                v.push(extra);
            }
        }
        if let Some(d) = default {
            if d >= min && d <= max {
                v.push(d);
            }
        }
        for k in 0..21u32 {
            let x = min + (max - min) * k as i32 / 20;
            v.push(x);
        }
    }
    v.sort_unstable();
    v.dedup();
    v
}

fn main() {
    for p in params::PARAMS_ITER {
        for value in samples(p.min, p.max, p.default) {
            println!("{}|{}|{}", p.id, value, params::describe_value(p, value));
        }
    }
}
