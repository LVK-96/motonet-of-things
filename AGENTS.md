# AGENTS

## Project Overview
This is a Rust-based firmware project for ESP32, focusing on IoT sensor data collection and display.
- **Goal**: Ingest 433MHz radio data (Rubicson sensors), display it on an OLED screen, and publish via MQTT.
- **Maintainer**: Leo (Single contributor).
- **Style**: Casual but descriptive commit messages.

## Architecture
The system uses `embassy` for async task management on the ESP32.
- **Runtime**: `embassy-executor` on `esp-rtos` (via `esp-hal`).
- **Communication**: `embassy-sync` channels pass data between tasks.
  - `radio_433_rx`: Ingests raw radio data.
  - `mqtt_task`: Publishes telemetry.
  - `display_task`: Updates the UI.
- **Hardware Abstraction**: `esp-hal` for peripheral access.

## Structure
- `firmware/`: Main application binary (`no_std`).
- `crates/rubicson`: Driver for Rubicson 433MHz temperature sensors.
- `crates/eu-dst`: European Daylight Saving Time calculation.
- `scripts/`: Utility scripts (e.g., `mqtt-test.sh`).

## Development Workflow

### Build & Run
- **Flash & Monitor**: Run from `firmware/` directory:
  ```bash
  cargo run --release
  ```
  This uses `espflash` to flash and monitor the device.

### Testing
- **Unit Tests (Host)**: Run from root or crate directory:
  ```bash
  cargo test
  ```
  Note: `firmware` crate is `no_std` and difficult to test on host. Logic is moved to helper crates (like `rubicson`) for host-side testing.
- **Integration Tests**: `scripts/mqtt-test.sh` for verifying MQTT flows locally.

## Key Considerations for Agents
1.  **Environment**: `no_std` context for firmware. Standard library is available for testing helper crates.
2.  **Testing**: Always verify logic changes with `cargo test` in the relevant crate before proposing fixes.
3.  **Conventions**:
    - Use `cargo fmt` to maintain style.
    - Run `cargo clippy` to check for lints. Note: Run `cargo clippy` in `firmware/` for the target, and `cargo clippy -p <crate>` for host crates.
    - Commit messages should be "Action: Details" (e.g., "Fix decode logic for negative temps").
4.  **Known Issues**:
    - **Temperature Encoding**: The `rubicson` crate uses 12-bit two's complement. Be careful with sign extension. Negative temperatures (e.g., -10.5°C) correspond to raw values with high nibbles (e.g., 0xFxx). Always test with negative values.
5.  **Networking Troubleshooting**:
    - For Wi-Fi/MQTT connection errors, always ask the user to confirm `firmware/src/secrets.rs` setup first (SSID/password and any channel/BSSID/static-IP hints) before proposing code fixes.
