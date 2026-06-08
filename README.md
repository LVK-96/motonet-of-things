# (MTX) Basically Freezing

Receive 433 MHz data from cheap wireless temperature sensors (Rubicson protocol), show it on OLED, and publish readings over MQTT.

- Rust
- ESP32-WROOM (Xtensa ESP32)
- CC1101
- Sensor example (Motonet / MTX Basic): https://www.motonet.fi/tuote/mtx-basic-langaton-lampomittari-sisa-ja-ulkokayttoon?product=86-01355

This is also a little experiment to see how agentic AI workflows work in firmware development.

## Build And Flash

Run firmware commands from `firmware/` so the local target config is used.

```bash
cd firmware
cargo run --release
```

This uses `espflash` runner to:
- Build for `xtensa-esp32-none-elf`
- Flash the board
- Attach monitor output (`defmt` log format)

## Pulse Capture Backends

The firmware supports two pulse capture backends:

- `pulse_rmt` (default): uses ESP32 RMT peripheral (recommended)
- `pulse_sw`: software edge capture

Default build (RMT):

```bash
cd firmware
cargo run --release
```

Software capture build:

```bash
cd firmware
cargo run --release --no-default-features --features pulse_sw
```

## Log Levels (`defmt`)

Default log level is configured in `firmware/.cargo/config.toml`:

```toml
[env]
DEFMT_LOG = "warn"
```

This keeps production output quieter. For development, override per run:

```bash
cd firmware
DEFMT_LOG=info cargo run --release
DEFMT_LOG=debug cargo run --release
DEFMT_LOG=trace cargo run --release
```

Suggested usage:
- `warn`: production/default
- `info`: normal bring-up/debugging
- `debug`/`trace`: deep protocol and timing investigation

## Local Secrets Setup

Create local firmware credentials before running firmware checks/flash commands:

```bash
cp firmware/src/secrets.rs.example firmware/src/secrets.rs
```

Then edit `firmware/src/secrets.rs` with your local Wi-Fi and MQTT values.
The file is git-ignored and must never be committed.

## Sleep Modes And Power Policy

Power behavior is controlled by the firmware settings menu:

- `Pred Slp`: enable/disable predictive deep sleep
- `Sleep`: deep sleep duration (seconds)
- `UI Idle`: idle timeout before predictive sleep is allowed

Current behavior:

- Any UI input resets the idle countdown.
- Predictive deep sleep is only considered after a successful sensor frame decode.
- Deep sleep wake sources are timer + EC11 push button (`GPIO27`).
- Rotary A/B movement does not wake from deep sleep.

## Wi-Fi Reconnect Tuning

The firmware logs per-stage reconnect timings (Wi-Fi start, association, IP config, TCP connect, MQTT connect, first publish) with wake context tags to make power/performance tuning measurable.

Optional reconnect optimizations are configured in `firmware/src/secrets.rs`:

- `WIFI_CHANNEL_HINT`
- `WIFI_BSSID_HINT`
- `WIFI_STATIC_IP`
- `WIFI_SUBNET_PREFIX`
- `WIFI_GATEWAY_IP`
- `WIFI_DNS1_IP`
- `WIFI_DNS2_IP`

Use `None` values to keep default behavior (scan + DHCP).

Time sync failover is configured with:

- `NTP_SERVER_IPV4_LIST` (comma-separated IPv4 values, tried in order)

Invalid entries are ignored. If no valid server remains, firmware falls back to built-in defaults.

## OTA From GitHub Releases

The firmware uses encrypted OTA v2 packaging: the plaintext firmware is
encrypted into a `.bin.enc` container and paired with a signed manifest
(`firmware.manifest.json`). The manifest's `url` points at the encrypted
container hosted on GitHub Releases:

```text
https://github.com/<owner>/<repo>/releases/download/<tag>/firmware.bin.enc
```

### Firmware Secrets

Secure the OTA master key in `firmware/src/secrets.rs`:

- `OTA_ENCRYPTION_KEY_ID: u32 = 1` — key identifier.
- `OTA_ENCRYPTION_MASTER_KEY: [u8; 32]` — 32 raw bytes from which per-key-id
  AES-256 and HMAC-SHA256 subkeys are derived. Must match the key used by the
  packaging tool.
- `OTA_TLS_ALLOW_INVALID_CA: bool` — when `true`, TLS certificate verification
  for OTA downloads is skipped entirely. Only use for local dev; never enable
  for production. Use `OTA_TLS_CA_CERT_DER` for CA-pinned verification instead.

Generate a fresh master key:

```bash
openssl rand -hex 32
```

### Release Workflow

Release OTA builds must be compiled with:

```bash
cd firmware
cargo build --release --features release-ota
```

Tagged pushes (`v*`) run `.github/workflows/ota-release.yml`, which:
- bakes `OTA_ENCRYPTION_MASTER_KEY` into the firmware via env secret
- builds `firmware.bin`
- encrypts and packages it into `firmware.bin.enc` + signed
  `firmware.manifest.json` using `scripts/ota-pack.sh`
- uploads only the encrypted container and manifest to the GitHub Release
  (never the plaintext binary)

The workflow uses these GitHub secrets:
- `OTA_RELEASE_ED25519_SEED_HEX` — Ed25519 seed (64 hex chars)
- `OTA_ENCRYPTION_MASTER_KEY_HEX` — master key (64 hex chars)

The release manifest uses `--signing-key-id 1` and `--ota-key-id 1`.

### Local Packaging Flow

Use `scripts/ota-pack.sh` to produce an encrypted container + manifest:

```bash
scripts/ota-pack.sh \
  --input target/ota/firmware.bin \
  --output target/ota/firmware.bin.enc \
  --manifest target/ota/firmware.manifest.json \
  --url https://example.com/firmware.bin.enc \
  --version 0.2.0 --build 42 \
  --signing-seed-hex-file tools/ota/keys/dev_ed25519.seed.hex \
  --signing-key-id 1001 \
  --ota-key-id 1 \
  --ota-master-key-hex-file tools/ota/keys/dev_master.hex
```

Or with env var fallback for the master key:

```bash
OTA_ENCRYPTION_MASTER_KEY_HEX=... scripts/ota-pack.sh ...
```

### Testing OTA Locally

Build an OTA image, package it, serve it, and publish the manifest:

```bash
scripts/mqtt-test.sh ota-build
scripts/ota-pack.sh ...   # produce .enc + .manifest.json
scripts/mqtt-test.sh ota-serve --port 9000 --file target/ota/firmware.bin.enc
scripts/mqtt-test.sh ota-send [plain|tls] --manifest target/ota/firmware.manifest.json
```

## Verification

Run verification from repository root:

```bash
just test-host
just check-firmware
# Or run everything:
just verify
```

Verification matrix:

| Command | Validates |
| --- | --- |
| `just test-host` | host `fmt`, host `clippy`, and host tests for `rubicson`, `eu-dst`, `karu-menu`, `app-core`, `telemetry-core`, and `ota-core` |
| `just check-firmware` | firmware `cargo check` for `xtensa-esp32-none-elf` |
| `just verify` | full host + firmware verification bundle |
| `cargo clippy -p app-core -p telemetry-core -p ota-core --target x86_64-unknown-linux-gnu -- -D warnings` | strict lint gate for host-only architecture/core crates |
| `cargo test -p app-core -p telemetry-core -p ota-core --target x86_64-unknown-linux-gnu` | host regression suite for core logic, including NTP list parsing and OTA policy/signing |
