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
- Use `esp_bootloader_esp_idf::ota_updater::OtaUpdater` for partition and OTA metadata handling.
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

Suggested path:

```text
tools/ota/keys/dev_ed25519.key
tools/ota/keys/dev_ed25519.pub
```

Document clearly:

```text
Test key only. Never trusted by release firmware.
```

## Phase 4: MQTT OTA Command Handling

### 10. Define `DEVICE_ID`

Add to `firmware/src/secrets.rs.example`:

```rust
pub const DEVICE_ID: &str = "garage-sensor-01";
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
2. Publish `ota accepted` status if validation passes far enough.
3. Publish `ota starting`.
4. Disconnect/stand down for OTA maintenance mode.

Acceptance criteria:

- Device receives signed OTA manifest over MQTT.
- Normal telemetry still works when no OTA command exists.

### 12. Add dedicated OTA task/channel

MQTT should hand off OTA work to a dedicated OTA task.

OTA task owns:

- Manifest validation.
- Maintenance mode.
- HTTP(S) download.
- Flash write.
- SHA verification.
- Slot activation.
- Reboot/failure reporting.

Acceptance criteria:

- MQTT and OTA responsibilities are separate.

## Phase 5: Local Dev OTA

### 13. Add OTA maintenance mode

During OTA, keep only required systems active:

- Wi-Fi/network stack.
- OTA HTTP client.
- Flash writer.
- Hash/signature verifier.
- Minimal display/progress if useful.

Pause or suppress:

- 433MHz radio ingest.
- Telemetry publishing.
- MQTT reconnect loop.
- Time sync.
- UI menu interactions.
- Deep sleep/power-saving decisions.
- Verbose payload logging.

Acceptance criteria:

- Tasks observe `ota_update_in_progress()` and go quiet where needed.
- Device does not enter sleep during OTA.

### 14. Implement local HTTP download for dev OTA

Dev OTA profile:

- Allows `http://` local URLs.
- Requires signed manifest using dev key.
- Anti-rollback disabled.
- Streams body; never buffers full image.

Use 4 KiB chunks initially.

Checks:

- Byte count equals manifest `size`.
- SHA-256 matches manifest.
- First byte is ESP image magic `0xE9`.
- Image fits inactive slot.

Acceptance criteria:

- Failed download does not switch boot slot.
- SHA mismatch is rejected.
- Oversized image is rejected.

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

### 17. Add local OTA scripts

Suggested scripts:

```text
scripts/ota-build-image.sh
scripts/ota-serve.sh
scripts/ota-send-manifest.sh
```

Example flow:

```bash
scripts/ota-build-image.sh
scripts/ota-serve.sh target/.../firmware.bin
scripts/ota-send-manifest.sh \
  --device garage-sensor-01 \
  --url http://HOST:PORT/firmware.bin
```

Acceptance criteria:

- Local script can OTA-update from dev build A to dev build B.

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

## Minimum Local OTA Success Test

1. USB flash dev-OTA build A.
2. Build dev-OTA build B.
3. Serve build B from local HTTP server.
4. Send signed manifest over MQTT to:

```text
motonet/<DEVICE_ID>/cmd/ota
```

5. Device verifies manifest signature.
6. Device enters OTA maintenance mode.
7. Device downloads image.
8. Device writes inactive slot.
9. Device verifies size, SHA-256, magic byte, and slot fit.
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
8. Local HTTP dev OTA.
9. Hardware rollback verification.
10. Release anti-rollback.
11. HTTPS/GitHub support.
12. GitHub Actions release pipeline.
