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
| `just test-host` | host `fmt`, host `clippy`, and host tests for `rubicson`, `eu-dst`, and `karu-menu` |
| `just check-firmware` | firmware `cargo check` for `xtensa-esp32-none-elf` |
| `just verify` | full host + firmware verification bundle |
| `cargo test -p karu-menu --target x86_64-unknown-linux-gnu` | root-level host sanity test command |
