# Port notes: what upstream has and what the Rust port does

Upstream repo: `eosed` (Python, Textual TUI, vinsynlib). Port: `eosed-rs` (Rust, ratatui).
Every ported layer is proven by byte-diff against upstream (see docs/RESOLUTION_NOTES.md upstream for the measurements).

Verified identical (diff = 0):
- params table (32 140 lines), lcd + panel protocol (848), bridge messages (1346),
  config (36), autodetect (26), ports rendering (16)
- live hardware: memory, catalog (128), get, dump OLD (360 B) and NEW (358 B),
  send round-trip, rename, fulltest harness (28 lines)
- demo mode: hw/config/memory/catalog/get, dump 0, send round-trip
- preset_snapshot: capture diff 0 lines, cross-verify IDENTICAL both directions
- carrier_check: 4 argument combinations identical output and exit codes
- front panel encoders (halfblock/braille): identical on a synthetic bitmap

Deliberate divergences (documented, not silent):
1. `entry.py` — N/A. It is a pip-dependency guard for vinsynlib. The Rust binary
   statically links its own library; there is no missing-package failure mode to
   reproduce.
2. `tools/rig.py` — N/A. A jack/PipeWire capture rig for an audio bench; its
   dependency (`hw_measure`) is not part of the repository. Its three teardown
   lessons (guard partial construction, catch BaseException not Exception,
   bound every teardown with a deadline) are recorded here so they are not lost:
   the Rust MIDI tools follow the same rule — ports are closed on every exit
   path, and nothing blocks without a timeout.
3. `tools/capture_screenshots.py` — ported as `eosed_tui --capture-screens`:
   text renders of the same screens (compact_view, extended_view_voice,
   master_menu, history, front panel halfblock/braille) via ratatui's
   TestBackend. Textual's SVG→PNG pipeline has no ratatui equivalent; the
   screenshots are plain text, the panel encoders are diff-identical.
4. The upstream app tests (~100 async pilot tests) are not mirrored one-to-one.
   The behaviour they pin (write gate arming, nudge collapse, undo scope,
   sweep confirmation thresholds, sample-usage early stop, integrity check,
   wrap_blocks, slot labels) is covered by Rust unit tests in tui_logic and
   the TUI tests; the Textual-widget-specific assertions (pane queries,
   highlight styling) do not exist in ratatui.
