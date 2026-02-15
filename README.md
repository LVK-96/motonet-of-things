# (MTX) Basically Freezing

Receive 433 MHz data from cheap wireless temperature sensors (Rubicson protocol), show it on OLED, and publish readings over MQTT.

- Rust
- ESP32-WROOM (Xtensa ESP32)
- CC1101
- Sensor example (Motonet / MTX Basic): https://www.motonet.fi/tuote/mtx-basic-langaton-lampomittari-sisa-ja-ulkokayttoon?product=86-01355

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

## Tests

Run tests from repository root or crate directories:

```bash
cargo test
```

Note: firmware is `no_std` and host tests are primarily in helper crates (for example `crates/rubicson`).
