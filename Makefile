# SPDX-License-Identifier: GPL-2.0-or-later
# Convenience targets mirroring the CI gates.

all: build

build:
	cargo build --release

test:
	cargo test

lint:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

audit:
	cargo audit

demo:
	cargo run --bin eosed_cli -- --demo

clean:
	cargo clean
