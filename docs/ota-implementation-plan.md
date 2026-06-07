# OTA Implementation Plan

## Goal

Implement pull-based OTA updates for the ESP32-WROOM firmware without using ESP-IDF as the application runtime.

The device receives a signed OTA manifest over MQTT, downloads the app image from the manifest URL, verifies it, writes it to the inactive OTA slot, switches boot slot, and confirms the new app only after it proves healthy.

## Agreed Design Summary

- Use A/B OTA with rollback.
- OTA updates the application image only, not bootloader or partition table.
- Assume 4 MiB ESP32-WROOM flash.
- Use two 1.75 MiB OTA slots.
- Leave remaining flash unused for now.
- Keep OTA policy, manifest validation, signing, state transitions, topic construction, and health confirmation in the platform-independent `crates/ota-core` crate.
- Keep firmware limited to hardware/platform adapters: MQTT/TLS transport, HTTP client, ESP flash writes, ESP boot metadata, and reboot.
- Use `esp_bootloader_esp_idf::ota_updater::OtaUpdater` only inside the firmware boot/flash adapter for partition and OTA metadata handling.
- Use signed canonical JSON manifests.
- Use Ed25519 signatures.
- MQTT notification carries the full signed manifest.
- Firmware downloads from the manifest URL.
- Dev OTA uses local HTTP and a committed test keypair.
- Release OTA uses HTTPS and a release key stored in GitHub Actions secrets.
- Release pipeline is last; local OTA must work first.

## Partition Layout

Create `firmware/partitions.csv`:

```csv
# Name,   Type, SubType, Offset,   Size
nvs,      data, nvs,     0x9000,   0x4000
otadata,  data, ota,     0xd000,   0x2000
phy_init, data, phy,     0xf000,   0x1000
ota_0,    app,  ota_0,   0x10000,  0x1C0000
ota_1,    app,  ota_1,   0x1D0000, 0x1C0000
```

Maximum OTA app image size:

```text
0x1C0000 bytes
```

## Phase 1: Flash Layout and Boot Metadata

### 1. Add OTA partition table

- Add `firmware/partitions.csv`.
- Configure `cargo run` / `espflash` to use it.
- Ensure normal USB flashing still works.

Acceptance criteria:

- Firmware flashes and boots from the new partition layout.
- Generated app image fits within `0x1C0000`.

### 2. Add OTA boot wrapper

Create a firmware module wrapping `esp_bootloader_esp_idf::ota_updater::OtaUpdater`.

Responsibilities:

- Read partition table.
- Log current boot/OTA slot and state.
- Find inactive OTA slot.
- Mark current app valid.
- Activate next partition.

Acceptance criteria:

- Device logs current OTA slot/state on boot.
- Current app can be marked valid without breaking normal boot.

## Phase 2: Health Confirmation and Rollback Test

### 3. Add OTA health confirmation gate

Only mark app valid after:

- Wi-Fi is connected.
- MQTT is connected.
- Internal status heartbeat has been published.
- Optional 30–60 second uptime delay has elapsed.

Do not depend on Rubicson/433MHz telemetry, because external sensors may be quiet.

Acceptance criteria:

- App does not confirm itself immediately on boot.
- App confirms only after network/MQTT health is proven.

### 4. Disable sleep during OTA confirmation

Power policy must reject sleep while:

- OTA update is in progress.
- OTA confirmation is pending.

Acceptance criteria:

- Pending OTA confirmation cannot deep sleep before validation.

### 5. Add rollback-test build mode

Add feature flag:

```toml
ota-rollback-test = []
```

Behavior:

- Boot.
- Log rollback-test mode.
- Do not mark app valid.
- Reboot after about 10 seconds.

Acceptance criteria:

- Hardware rollback can be tested deterministically.

## Phase 3: Manifest and Signing Core

### 6. Add OTA manifest model

Manifest fields:

```json
{
  "schema": 1,
  "key_id": 1001,
  "target": "motonet-of-things/esp32",
  "chip": "esp32-wroom",
  "version": "0.2.0",
  "build": 0,
  "force": false,
  "url": "http://192.168.1.10:8000/firmware.bin",
  "size": 1234567,
  "sha256": "...",
  "signature": "..."
}
```

Suggested limits:

```rust
MAX_MANIFEST_BYTES: usize = 1024;
MAX_URL_LEN: usize = 512;
MAX_VERSION_LEN: usize = 32;
MAX_TARGET_LEN: usize = 48;
MAX_CHIP_LEN: usize = 24;
MAX_REDIRECTS: usize = 3;
```

Acceptance criteria:

- Oversized manifests are rejected.
- Wrong `schema`, `target`, or `chip` are rejected.
- Malformed manifests are rejected.

### 7. Implement canonical JSON signing

The signature covers the manifest without the `signature` field, serialized in fixed field order.

Acceptance criteria:

- Host tests prove canonical bytes are stable.
- Changing any signed field invalidates the signature.

### 8. Add Ed25519 verification

Profiles:

```text
dev-ota:
  key_id = 1001
  trusts committed test public key

release-ota:
  key_id = 1
  trusts release public key only
```

Acceptance criteria:

- Valid dev manifest verifies in dev OTA mode.
- Modified manifest fails verification.
- Wrong `key_id` fails.
- Release firmware does not trust the committed dev/test key.

### 9. Add committed dev test keypair

Committed local-dev key material:

```text
tools/ota/keys/dev_ed25519.seed.hex
tools/ota/keys/dev_ed25519.pub.hex
```

The seed file is a raw 32-byte Ed25519 seed encoded as hex. Local shell tooling can wrap it as temporary PKCS#8 key material for `openssl` signing.

Document clearly:

```text
Test key only. Never trusted by release firmware.
```

## Phase 4: MQTT OTA Command Handling

### 10. Define `DEVICE_ID`

Add to `firmware/src/secrets.rs.example`:

```rust
pub const DEVICE_ID: &str = "test-sensor";
```

MQTT topics:

```text
motonet/<DEVICE_ID>/status
motonet/<DEVICE_ID>/cmd/ota
```

Acceptance criteria:

- Status topic uses `DEVICE_ID`.
- OTA command topic is subscribed after MQTT connect.

### 11. Extend MQTT task to receive OTA commands

Current MQTT flow is publish-oriented. Add subscription/receive support.

On OTA manifest received:

1. Pass manifest bytes to the OTA task/channel.
2. OTA task validates manifest authenticity/policy before entering maintenance mode.
3. If the manifest is accepted, OTA task sets `RuntimeMode::OtaDownloading`.
4. MQTT observes runtime mode, disconnects/stands down, and publishes `MqttSessionState::Disconnected` to a watch.

Acceptance criteria:

- Device receives signed OTA manifest over MQTT.
- Normal telemetry still works when no OTA command exists.

### 12. Add dedicated OTA task/channel

MQTT should hand off OTA work to a dedicated OTA task.

The dedicated OTA task is a firmware adapter around `ota-core`. `ota-core` owns platform-independent decisions and validation:

- Manifest validation.
- Signature/key policy.
- State transitions.
- Maintenance-mode decision policy.
- Size, SHA, image-magic, and slot-fit checks as pure policy.

Firmware owns hardware/platform effects:

- MQTT receive/publish adapter.
- HTTP(S) download adapter.
- ESP flash write adapter.
- ESP OTA metadata/slot activation adapter.
- Reboot/failure reporting adapter.

Acceptance criteria:

- MQTT and OTA responsibilities are separate.
- Platform-independent OTA behavior is testable in `crates/ota-core` without ESP hardware.

## Phase 5: Local Dev OTA

### 13. Add OTA maintenance mode

Use two separate concepts:

- `ota-core::OtaState`: OTA lifecycle/power policy (`Inactive`, `Downloading`, `Applying`, `PendingConfirmation`).
- `app-core::RuntimeMode`: app-wide coordination surface observed by firmware tasks.

Add `RuntimeMode` to `crates/app-core`:

```rust
pub enum RuntimeMode {
    Normal,
    OtaDownloading,
    OtaApplying,
}
```

Add host-tested policy helpers in `app-core`, for example:

```rust
runtime_mode_allows_mqtt(mode)
runtime_mode_allows_radio_capture(mode)
runtime_mode_allows_ui_input(mode)
runtime_mode_display_message(mode)
```

Expose runtime mode in firmware through a single watch in `firmware/src/app_bus.rs`. The OTA task is the only writer during OTA; MQTT, radio, display, and UI input are observers. This avoids direct OTA dependencies in those tasks.

During `RuntimeMode::OtaDownloading`:

- Wi-Fi/network stack remains active.
- MQTT disconnects/stands down before HTTP download.
- OTA task waits up to 5 seconds for `MqttSessionState::Disconnected`; if the timeout expires, it logs a warning and proceeds with HTTP download.
- 433MHz radio capture/decoding pauses at task level. Add local radio quiesce/resume hooks that are initially no-ops/logging, so CC1101 sleep/reinitialization can be added later without changing OTA coordination.
- Display shows a static OTA screen, e.g. `OTA download...`.
- UI navigation/settings input is ignored.
- Deep sleep remains blocked by the existing OTA state/guard.
- Normal mode is restored after download verification succeeds or fails.

Acceptance criteria:

- Tasks depend on `app-core::RuntimeMode`/policy helpers, not on OTA task internals.
- MQTT publishes `MqttSessionState` to a watch; OTA can wait for MQTT stand-down.
- Device does not enter sleep during OTA.
- Normal MQTT/radio/display/UI behavior resumes after OTA download verification ends.

### 14. Implement local HTTP download for dev OTA

Dev OTA profile:

- Requires signed manifest using the committed dev key for this slice.
- Anti-rollback is disabled/ignored for dev download verification.
- URL policy is strict local HTTP only: `http://<ipv4>:<port>/<path>`.
- Reject hostnames, HTTPS, missing port, empty path, query, fragment, and userinfo.
- Use `reqwless` high-level `HttpClient` behind a firmware OTA download adapter.
- Start with an IPv4-literal-only resolver boundary, structured so DNS support can be added later.
- Streams body; never buffers full image.
- Keeps the first 64 bytes of the image for header validation.

HTTP response policy:

- Accept `HTTP/1.0` or `HTTP/1.1` responses via the client.
- Require status `200 OK`.
- Require `Content-Length` exactly equal to manifest `size`.
- Reject missing/mismatched `Content-Length`.
- Reject `Transfer-Encoding: chunked` for this slice.
- Reject redirects/non-200 responses.

Pure policy in `ota-core`:

- OTA URL policy validation.
- ESP image prefix validation for the retained 64-byte prefix.

ESP image prefix validation is conservative plausibility only:

- Prefix is long enough for the basic image header.
- Magic byte is `0xE9`.
- Segment count is sane (`1..=16`).
- Flash mode byte is a known ESP mode.
- Flash size/frequency byte uses known nibbles.
- Entry address is nonzero.

Download verification checks:

- Manifest parses and verifies before entering `RuntimeMode::OtaDownloading`.
- Byte count equals manifest `size`.
- SHA-256 matches manifest.
- Retained prefix passes `ota-core` ESP image prefix validation.

Download-only acceptance criteria:

- Invalid/unsigned manifest is rejected without entering maintenance mode.
- Failed download does not switch boot slot.
- SHA mismatch is rejected.
- Size mismatch is rejected.
- Invalid ESP image prefix is rejected.
- Success/failure is logged only for this slice; MQTT OTA status publishing is deferred.

### 15. Write inactive OTA slot

Use `OtaUpdater::next_partition()` to get the inactive slot.

Stream flow:

1. Receive HTTP body chunk.
2. Update SHA-256.
3. Write chunk to inactive OTA partition.
4. Repeat until complete.
5. Verify final byte count and SHA.

Acceptance criteria:

- Current running slot is not modified.
- Inactive slot is written correctly.

### 16. Activate and reboot

After verification:

```rust
ota_updater.activate_next_partition()?;
reboot();
```

Acceptance criteria:

- Device boots into the new image.
- New image confirms after health gate.

### 17. Add local OTA helper script commands

Keep local OTA helper commands in `scripts/mqtt-test.sh` instead of adding separate scripts.

Subcommands:

```text
scripts/mqtt-test.sh ota-smoke [plain|tls]
scripts/mqtt-test.sh ota-build
scripts/mqtt-test.sh ota-serve [--file target/ota/firmware.bin] [--port 8000]
scripts/mqtt-test.sh ota-send [plain|tls] --url http://HOST:8000/firmware.bin [--file target/ota/firmware.bin]
```

Behavior:

- Rename the current dummy `ota` MQTT smoke command to `ota-smoke`; no backwards-compatible `ota` alias is needed.
- `ota-build` builds release firmware and uses `espflash save-image --chip esp32` to write an app image to `target/ota/firmware.bin`.
- `ota-serve` defaults to `target/ota/firmware.bin`, creates a temporary served directory, exposes it as `/firmware.bin`, binds to `0.0.0.0`, and prints LAN URL hints plus a warning that the file is openly served on the LAN.
- `ota-send` defaults to `target/ota/firmware.bin` if it exists and prints the file path, size, and SHA-256 being signed.
- `ota-send` requires `--url`; if omitted, print suggested LAN URLs and fail instead of guessing on multi-interface hosts.
- `ota-send` computes size with `wc -c`, SHA-256 with `sha256sum`, canonical JSON with `jq -cn`, signs with `openssl` using Ed25519, hex-encodes with `xxd`, and publishes with `mosquitto_pub`.
- Dev signing defaults to `tools/ota/keys/dev_ed25519.seed.hex`, with an override for a different seed file.
- `version` defaults to `git describe --always --dirty`; `build` defaults to `git rev-list --count HEAD`; both can be overridden.
- The production manifest path can remain shell-based later, using CI-provided release key material instead of the committed dev seed.

Example flow:

```bash
scripts/mqtt-test.sh broker tls
scripts/mqtt-test.sh ota-build
scripts/mqtt-test.sh ota-serve --port 8000
scripts/mqtt-test.sh ota-send tls --url http://HOST:8000/firmware.bin
```

Acceptance criteria:

- `ota-smoke` can still verify MQTT OTA routing with a dummy manifest-shaped payload.
- `ota-build` produces an ESP app image suitable as an OTA payload.
- `ota-serve` serves the built image as `/firmware.bin` from a LAN-reachable address.
- `ota-send` publishes a signed dev manifest whose size/SHA match the served image.
- Local script flow supports dev build A downloading and verifying dev build B before flash-writing is implemented.

## Phase 6: Hardware Rollback Verification

Test procedure:

1. USB flash known-good build A.
2. OTA install rollback-test build B.
3. B boots and intentionally does not confirm.
4. B reboots.
5. Bootloader rolls back to A.
6. A publishes rollback/status message if possible.

Acceptance criteria:

- Rollback works on real ESP32-WROOM hardware.
- If prebuilt bootloader lacks rollback support, build/provide correct bootloader before continuing.

## Phase 7: Release Anti-Rollback

### 18. Add anti-rollback abstraction

Policy interface:

```rust
trait AntiRollback {
    fn current_floor(&self) -> u32;
    fn can_accept(&self, build: u32, force: bool) -> bool;
    fn commit_successful_build(&mut self, build: u32) -> Result<(), Error>;
}
```

Release policy:

```text
accept if build > floor
accept if force && build == floor
reject if build < floor
```

`force` allows reinstalling the same build only. It must not allow downgrade.

Acceptance criteria:

- Downgrades are rejected in release OTA mode.
- Same-build reinstall is allowed only with signed `force: true`.

### 19. Add flash/NVS-backed anti-rollback floor

For release mode initially.

Rules:

- Commit floor only after successful OTA health confirmation.
- Failed/pending app does not advance floor.
- Design storage abstraction so eFuse-backed anti-rollback can replace it later.

Acceptance criteria:

- Floor advances after confirmed release OTA.
- Floor never decreases.

## Phase 8: HTTPS/GitHub-Compatible Download

Do this only after local dev OTA is proven.

Release OTA profile:

- Requires HTTPS.
- Allows up to 3 redirects.
- Redirect targets must also be HTTPS.
- Streams body; no full-image buffering.
- SHA/signature remain the real trust mechanism.

Acceptance criteria:

- Device can download a GitHub Release asset.
- Redirect chain is handled.
- HTTP is rejected in release mode.

## Phase 9: GitHub Actions Release Pipeline

This is intentionally last.

Pipeline:

1. Build release firmware ELF.
2. Convert ELF to ESP app image `.bin`.
3. Fail if image size exceeds `0x1C0000`.
4. Compute `size` and `sha256`.
5. Generate canonical manifest.
6. Sign manifest with Ed25519 private key from GitHub Actions secret.
7. Upload `.bin` and `.manifest.json` to GitHub Release.

Acceptance criteria:

- Release manifest verifies on host.
- Release firmware trusts only release public key.
- Dev/test private key is never used for release firmware.
- Dev/test public key is not trusted by release firmware.

## Minimum Local OTA Success Tests

### Download-only local OTA success test

1. USB flash dev-OTA build A.
2. Build dev-OTA build B as `target/ota/firmware.bin`.
3. Serve build B from local HTTP server as `/firmware.bin`.
4. Send signed manifest over MQTT to:

```text
motonet/<DEVICE_ID>/cmd/ota
```

5. Device verifies manifest signature and URL policy.
6. Device enters `RuntimeMode::OtaDownloading`.
7. MQTT disconnects/stands down, or OTA proceeds after the 5 second stand-down timeout with a warning.
8. Radio capture pauses, UI input is ignored, and display shows static OTA screen.
9. Device downloads image over local HTTP.
10. Device verifies status/content-length, final byte count, SHA-256, and 64-byte ESP image prefix.
11. Device logs success/failure and returns to `RuntimeMode::Normal`.
12. Device does not write flash, activate a slot, or reboot in this slice.

### Full local OTA success test

1. USB flash dev-OTA build A.
2. Build dev-OTA build B.
3. Serve build B from local HTTP server.
4. Send signed manifest over MQTT to `motonet/<DEVICE_ID>/cmd/ota`.
5. Device verifies manifest signature.
6. Device enters OTA maintenance mode.
7. Device downloads and verifies image.
8. Device writes inactive slot.
9. Device verifies size, SHA-256, image header, and slot fit.
10. Device activates inactive slot.
11. Device reboots.
12. Build B boots.
13. Build B connects Wi-Fi and MQTT.
14. Build B publishes confirmed status.

## Implementation Order

1. Partition table and flash config.
2. `OtaUpdater` wrapper.
3. Health confirmation and mark-valid flow.
4. Rollback-test mode.
5. Manifest/signature host-tested code.
6. MQTT command subscription.
7. Dedicated OTA task.
8. Download-only local OTA slice, split into:
   1. Runtime mode quiet-mode plumbing, local helper script subcommands, URL policy validation, and ESP image prefix validation.
   2. `reqwless` firmware HTTP download integration and streaming size/SHA/header verification.
9. Inactive slot flash write.
10. Activate/reboot local OTA.
11. Hardware rollback verification.
12. Release anti-rollback.
13. HTTPS/GitHub support.
14. GitHub Actions release pipeline.
