#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all --check
cargo clippy -p rubicson -p eu-dst -p karu-menu --target x86_64-unknown-linux-gnu -- -D warnings
cargo test -p rubicson -p eu-dst -p karu-menu --target x86_64-unknown-linux-gnu
