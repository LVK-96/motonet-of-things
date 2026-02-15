set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

test-host:
    cargo fmt --all --check
    cargo clippy -p rubicson -p eu-dst -p karu-menu --target x86_64-unknown-linux-gnu -- -D warnings
    cargo test -p rubicson -p eu-dst -p karu-menu --target x86_64-unknown-linux-gnu

check-firmware:
    cp -n firmware/src/secrets.rs.example firmware/src/secrets.rs
    cd firmware && cargo check

verify: test-host check-firmware
