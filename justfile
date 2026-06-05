set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

install-hooks:
    git config --local core.hooksPath .githooks
    chmod +x .githooks/pre-commit

test-host:
    cp -n firmware/src/secrets.rs.example firmware/src/secrets.rs
    cargo +stable fmt --all --check
    cargo +stable clippy -p rubicson -p eu-dst -p karu-menu -p app-core -p telemetry-core -p ota-core --target x86_64-unknown-linux-gnu -- -D warnings
    cargo +stable test -p rubicson -p eu-dst -p karu-menu -p app-core -p telemetry-core -p ota-core --target x86_64-unknown-linux-gnu

esp-check:
    cp -n firmware/src/secrets.rs.example firmware/src/secrets.rs
    cargo check -Zbuild-std=core,alloc

esp-clippy:
    cp -n firmware/src/secrets.rs.example firmware/src/secrets.rs
    cargo clippy -Zbuild-std=core,alloc -- -D warnings

esp-build:
    cp -n firmware/src/secrets.rs.example firmware/src/secrets.rs
    cargo build -Zbuild-std=core,alloc --release

flash:
    cd firmware && cargo run --release

verify: test-host esp-check
