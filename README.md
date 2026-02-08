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

## Tests

Run tests from repository root or crate directories:

```bash
cargo test
```

Note: firmware is `no_std` and host tests are primarily in helper crates (for example `crates/rubicson`).
