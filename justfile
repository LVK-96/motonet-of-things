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

# Local OTA test: build, package, and run OTA update test
# Usage: just local-ota-test [URL]
#   URL: HTTP URL for firmware download (default: http://<local-ip>:8000/firmware.bin.enc)
local-ota-test url="":
    #!/bin/bash
    set -euo pipefail

    # Generate dev master key if it doesn't exist
    if [ ! -f tools/ota/keys/dev_master.hex ]; then
        echo "=== Generating dev master key ==="
        openssl rand -hex 32 > tools/ota/keys/dev_master.hex
        echo "Created tools/ota/keys/dev_master.hex"
    fi

    # Build firmware for local testing (dev keys, no release-ota feature)
    echo "=== Building firmware ==="
    cp -n firmware/src/secrets.rs.example firmware/src/secrets.rs
    (cd firmware && cargo build --release)

    # Create directories
    mkdir -p target/ota-local
    mkdir -p target/ota-stackfix-test

    # Copy firmware for HTTP server
    cp target/xtensa-esp32-none-elf/release/esp32-rust-project target/ota-local/firmware.bin

    # Get local IP for URL
    LOCAL_IP=$(ip route get 1 2>/dev/null | awk '{print $7; exit}')
    OTA_URL="${url:-http://${LOCAL_IP}:8000/firmware.bin.enc}"

    # Package firmware with dev keys
    echo "=== Packaging firmware ==="
    ./scripts/ota-pack.sh \
        --input target/ota-local/firmware.bin \
        --output target/ota-local/firmware.bin.enc \
        --manifest target/ota-stackfix-test/firmware.manifest.json \
        --url "$OTA_URL" \
        --version "v0.0.0-local" \
        --build 1 \
        --signing-seed-hex-file tools/ota/keys/dev_ed25519.seed.hex \
        --ota-key-id 1 \
        --ota-master-key-hex-file tools/ota/keys/dev_master.hex

    # Run the test
    echo "=== Running local OTA test ==="
    ./scripts/local-ota-test.sh

# Package firmware for OTA (without running test)
# Usage: just ota-package [URL]
ota-package url="http://$(ip route get 1 2>/dev/null | awk '{print $7; exit}'):8000/firmware.bin.enc":
    #!/bin/bash
    set -euo pipefail

    # Generate dev master key if it doesn't exist
    if [ ! -f tools/ota/keys/dev_master.hex ]; then
        echo "=== Generating dev master key ==="
        openssl rand -hex 32 > tools/ota/keys/dev_master.hex
        echo "Created tools/ota/keys/dev_master.hex"
    fi

    mkdir -p target/ota-local
    mkdir -p target/ota-stackfix-test

    cp target/xtensa-esp32-none-elf/release/esp32-rust-project target/ota-local/firmware.bin

    ./scripts/ota-pack.sh \
        --input target/ota-local/firmware.bin \
        --output target/ota-local/firmware.bin.enc \
        --manifest target/ota-stackfix-test/firmware.manifest.json \
        --url "{{url}}" \
        --version "v0.0.0-local" \
        --build 1 \
        --signing-seed-hex-file tools/ota/keys/dev_ed25519.seed.hex \
        --ota-key-id 1 \
        --ota-master-key-hex-file tools/ota/keys/dev_master.hex

    echo "Manifest: target/ota-stackfix-test/firmware.manifest.json"
    echo "Encrypted: target/ota-local/firmware.bin.enc"

verify: fmt-check esp-clippy host-clippy host-test esp-check
