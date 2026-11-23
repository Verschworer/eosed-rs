## What changed

- one-line summary
- layer(s) touched (messages / panel / lcd / bridge / config / params / TUI / tool)

## Differential verification (required for protocol or formatting changes)

This port is proven by byte-diff against upstream Python. If your change
touches protocol, rendering, or CLI output:

- [ ] Ran the corresponding `difftest_*` or `fulltest` against upstream Python
      (venv with python-rtmidi + vinsynlib), diff is zero.
- [ ] If a quirk had to be reproduced, cite the upstream RESOLUTION_NOTES
      section number in the code comment.

## Gates

- [ ] `make lint` passes (rustfmt + clippy `-D warnings`)
- [ ] `cargo test` passes
- [ ] Write-safety preserved: nothing sends writes without the
      `--allow-write`/armed gate; destructive panel actions still require
      explicit `--destroy`
- [ ] No front-panel divert frame can leave any code path (it is refused
      on the wire)

## Notes for the reviewer

- behaviour justified by upstream: link or section number
- any deliberate divergence from upstream is documented in docs/PORT_NOTES.md
