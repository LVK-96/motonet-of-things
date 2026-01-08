# ESP32 Rust Embassy

Async embedded Rust project for ESP32-WROOM using the [Embassy](https://embassy.dev/) framework.

## Features

- 🦀 **Rust** with `no_std` bare-metal development
- ⚡ **Embassy** async executor for concurrent tasks
- 🔧 **esp-hal** hardware abstraction layer
- 📡 **defmt + RTT** efficient logging via JTAG
- 🐛 **probe-rs** flashing and debugging

## Prerequisites

### 1. Install Toolchain

```bash
# Install espup (Xtensa Rust toolchain manager)
cargo install espup --locked
espup install

# Add to ~/.bashrc or ~/.zshrc
echo '. $HOME/export-esp.sh' >> ~/.bashrc
source ~/.bashrc
```

### 2. Install Debugger Tools

```bash
cargo install probe-rs-tools --locked
```

### 3. Setup udev Rules (Linux)

```bash
curl -fsSL https://probe.rs/files/69-probe-rs.rules | sudo tee /etc/udev/rules.d/69-probe-rs.rules
sudo udevadm control --reload-rules
sudo udevadm trigger
```

## Hardware Setup

### ESP-PROG JTAG Wiring

| ESP-PROG | ESP32 GPIO | Function |
|----------|------------|----------|
| TDI      | GPIO12     | Data In  |
| TDO      | GPIO15     | Data Out |
| TCK      | GPIO13     | Clock    |
| TMS      | GPIO14     | Mode Select |
| GND      | GND        | Ground   |

## Build & Flash

```bash
# Build only
cargo build --release

# Build, flash, and monitor RTT output
cargo run --release
```

## Debugging

### VS Code

1. Install the [probe-rs](https://marketplace.visualstudio.com/items?itemName=probe-rs.probe-rs-debugger) extension
2. Open this folder in VS Code
3. Set breakpoints in `src/bin/main.rs`
4. Press **F5** to start debugging

### Command Line

```bash
# Attach to running target
probe-rs attach --chip esp32

# Flash and start GDB server
probe-rs gdb --chip esp32 target/xtensa-esp32-none-elf/release/esp32-rust-project
```

## Project Structure

```
├── .cargo/config.toml     # Build target & probe-rs runner
├── .vscode/               # Debug launch configuration
├── src/
│   └── bin/main.rs        # Embassy async main entry point
├── Cargo.toml             # Dependencies
├── build.rs               # Linker script setup
└── rust-toolchain.toml    # Nightly Xtensa toolchain
```

## Useful Commands

```bash
# Check probe connection
probe-rs list

# View chip info
probe-rs info --chip esp32

# Erase flash
probe-rs erase --chip esp32
```

## Resources

- [The Rust on ESP Book](https://docs.espressif.com/projects/rust/book/)
- [esp-hal Examples](https://github.com/esp-rs/esp-hal/tree/main/examples)
- [Embassy Documentation](https://embassy.dev/book/)
- [probe-rs Documentation](https://probe.rs/)
