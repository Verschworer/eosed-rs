# Security posture

## Threat model

`eosed-rs` is a local CLI/TUI tool that talks to one E-mu EOS sampler over
MIDI. There is no network listener, no daemon, no elevated privilege. The
realistic attack surfaces are:

1. **Untrusted device replies on the MIDI bus.** A sampler (or anything else
   on the bus) can send arbitrary SysEx. Every inbound frame goes through
   parsers that are tested against hostile/foreign frames
   (`messages::has_valid_header`, `panel::parse_rejects_foreign_frames`,
   `lcd::short_frames_rejected`): length bounds, designator checks, and
   ones-complement checksum verification before any value is used. Values
   from the device are clamped to device-reported ranges, never trusted blind.
2. **Config files** (`config.toml`): untrusted input from disk. Parsing is
   strict, falls back to defaults, and tests cover unparseable, empty,
   cp1252-encoded and foreign config shapes.
3. **Command-line arguments**: numeric ranges validated (device id 0–127,
   positive timeouts); unknown arguments are a usage error, never ignored.

## Workflow-level exploitability — the part generic scanners cannot see

The dangerous operation is not code execution but *acting on the device*:
writing presets, pressing panel keys, wiping RAM banks. The controls:

- Writes are off by default. `--allow-write` arms the mode; the TUI `w` key
  toggles arm/disarm; nothing sends without both.
- The front-panel divert frame (designator 0x50, 7F 00) is **refused on the
  wire**, not merely avoided at call sites — no code path can emit it.
- RAM-destroying bank loads require an explicit `--destroy` flag *and* the
  CONFIRM screen classification; an unclassified screen stops the tool.
- F4 is never pressed without first classifying the screen, because F4 on an
  open LOAD dialog is MERGE (a silent write). This guard is encoded in code
  with measured screen signatures, not prose.
- Every device round-trip has a timeout; nothing waits forever on a hung bus.

Exploitability against these controls requires physical/logical access to the
MIDI transport itself; if an attacker can inject MIDI messages on the bus
they can act on the sampler directly, so the tool's gates are defense-in-depth
for user mistakes, not against a malicious bus.

## Automated checks

- **SCA**: `cargo audit` (RustSec advisories) runs in CI on every push/PR.
- **SAST-lite**: `cargo clippy --all-targets -D warnings` is a hard CI gate.
  Optional CodeQL can be enabled on GitHub for deeper static analysis.
- Differential tests against upstream Python double-check protocol parsers on
  real byte streams, including malformed-frame rejection tests.

## Dependencies

`toml`, `thiserror`, `dirs`, `encoding_rs`, `midir`, `ratatui`, `crossterm`,
`serde_json` — all pure-userland crates; none fork processes, none open
network sockets, none write outside explicitly passed paths.

## Disclosure

Report vulnerabilities via GitHub issues or the upstream project
<https://github.com/lentferj/eosed>; include the MIDI byte stream if the
finding is protocol-related.
