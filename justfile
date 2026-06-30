set shell := ["bash", "-euo", "pipefail", "-c"]

# Workspace crates that compile on the host (firmware and its esp-* deps
# are excluded by listing these explicitly — `cargo --workspace --exclude
# firmware` would still pull in the firmware's dependency graph).
host_crates := "-p rubicson -p eu-dst -p karu-menu -p app-core -p telemetry-core -p ota-core"

default:
    @just --list

install-hooks:
    git config --local core.hooksPath .githooks
    chmod +x .githooks/pre-commit

fmt-check:
    cargo +stable fmt --all --check

fmt-fix:
    cargo +stable fmt --all

# Run clippy on host-compilable crates.
host-clippy:
    cargo +stable clippy {{host_crates}} --target x86_64-unknown-linux-gnu -- -D warnings

host-test:
    cp -n firmware/src/secrets.rs.example firmware/src/secrets.rs
    just fmt-check
    just host-clippy
    cargo +stable test {{host_crates}} --target x86_64-unknown-linux-gnu

esp-clippy:
    cp -n firmware/src/secrets.rs.example firmware/src/secrets.rs
    cargo xtensa-clippy --workspace -- -D warnings

esp-check:
    cp -n firmware/src/secrets.rs.example firmware/src/secrets.rs
    cargo check -Zbuild-std=core,alloc

esp-build:
    cp -n firmware/src/secrets.rs.example firmware/src/secrets.rs
    cargo build -Zbuild-std=core,alloc --release

self_test_features := "sha-self-test,rsa-self-test"

esp-self-test-build:
    cp -n firmware/src/secrets.rs.example firmware/src/secrets.rs
    cargo build -Zbuild-std=core,alloc --release --features {{self_test_features}}

flash features="":
    cd firmware && cargo run --release {{features}}

flash-self-test:
    just flash "--features {{self_test_features}}"

verify: fmt-check esp-clippy host-clippy host-test esp-check
