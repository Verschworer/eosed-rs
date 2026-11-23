# eosed-rs

Rust port of **eosed** — a tool for the E-mu EOS sampler (E4XT Ultra, firmware
4.70 verified live).

**Upstream project this port is derived from:**
<https://github.com/lentferj/eosed> (Python, GPL-2.0-or-later). All protocol
knowledge — MIDI message formats, panel protocol, LCD encoding, parameter
tables, measured device quirks — comes from that project and its
`docs/RESOLUTION_NOTES.md`. This Rust port is a reimplementation, not a fork;
it is proven by differential runs: every layer's output is byte-compared
against upstream Python on the same input, live hardware where possible.

License: GPL-2.0-or-later, with SPDX headers in every source file, matching
upstream.

## Binaries

- `eosed_cli`   — CLI parity with `eoscli` (ports, config, hardware, memory,
                  catalog, get, dump, send, rename, program change).
- `eosed_tui`   — ratatui twin of the Textual `app.py`. `--demo` runs the canned
                  device; `--capture-screens` writes text renders of every screen
                  to docs/screenshots/.
- `load_bank`   — panel-protocol bank loader. Refuses to destroy RAM without
                  `--destroy`; classifies the panel screen (MAIN/LOAD/CONFIRM)
                  before ever touching F4.
- `preset_snapshot` — capture/verify/restore voice parameters; returns to voice 0.
- `carrier_check` — envelope-capture QA: a-priori pitch check + carrier-cycle
                  swing verdict (REFUSE/WARN/OK). FFT is self-contained.
- `lcd_read`    — OCR of the highlighted numeric LCD field via exact glyph match
                  against lcd_glyphs.json; unknown glyphs are reported, never
                  guessed.
- `fulltest`    — live end-to-end harness used for the Python-vs-Rust diff.

## Quick start

```sh
cargo build
cargo test                          # 58 tests
cargo run --bin eosed_cli -- --demo
cargo run --bin eosed_cli -- ports
cargo run --bin eosed_cli -- hardware
cargo run --bin eosed_tui -- --demo # TUI without hardware
cargo run --bin eosed_tui           # TUI against live E4XT
```

## Safety model

Writes are off by default. `--allow-write` arms the mode; the `w` key toggles
arm/disarm at runtime. Destructive panel actions additionally need explicit
confirmation (`--destroy` for RAM-destroying bank loads). The front-panel divert
frame is refused on the wire, not at call sites.

## Verification ledger (all diffs zero)

params 32140 lines · lcd+panel 848 · bridge 1346 · config 36 · autodetect 26 ·
ports 16 · live memory 2 · catalog 128 · get 2 · dump OLD 360 B · dump NEW 358 B ·
demo hw/cfg/mem/cat/get · demo dump 62 B · send round-trip · preset_snapshot
capture 0 lines + cross-verify IDENTICAL ×2 · fulltest 28 lines live ·
carrier_check 4 argument combinations · panel encoders half/braille ·
lcd_read glyph OCR on synthetic field.

See docs/PORT_NOTES.md for deliberate divergences (entry.py, rig.py, probes).
