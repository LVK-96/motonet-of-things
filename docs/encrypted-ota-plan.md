# Encrypted OTA Implementation Plan

## Agreed Design Summary

- Schema v2, always encrypted, no backward compatibility.
- Crypto: AES-256-CTR (hardware) + HMAC-SHA256 (hardware SHA, two passes).
- Container: `MOTOTA2\0` 16-byte header + fixed-size 4096-byte chunks.
- Manifest signed with Ed25519; chunk HMAC binds chunks to signed manifest.
- Tooling: `scripts/ota-pack.sh` packages, encrypts, signs, self-checks.
- Local HTTPS without valid CA allowed via `OTA_TLS_ALLOW_INVALID_CA`.
- GitHub Actions upload only encrypted container + signed manifest.

## Full Schema

### Manifest (schema v2)

```json
{
  "schema": 2,
  "key_id": 1001,
  "target": "motonet-of-things/esp32",
  "chip": "esp32-wroom",
  "version": "0.2.0",
  "build": 42,
  "force": false,
  "url": "https://.../firmware.bin.enc",
  "download_size": 1200000,
  "image_size": 1180000,
  "image_sha256": "abc...",
  "enc": {
    "alg": "AES-256-CTR-HMAC-SHA256",
    "key_id": 1,
    "chunk_size": 4096,
    "nonce_prefix": "00112233445566778899aabb"
  },
  "signature": "..."
}
```

### Canonical signing order (unsigned)

Fixed field order for canonical JSON:

```text
schema
key_id
target
chip
version
build
force
url
download_size
image_size
image_sha256
enc {
  alg
  key_id
  chunk_size
  nonce_prefix
}
```

### Encrypted container format

```text
header (16 bytes):
  magic[8]        = "MOTOTA2\0"
  format_version  = 2 (u8)
  reserved[7]     = zero

chunks (repeated):
  plaintext_len_be[4]
  ciphertext[plaintext_len]
  tag[32]          = HMAC-SHA256

Constraints:
  chunk_count = ceil(image_size / chunk_size)
  chunks 0..N-2 must have plaintext_len == chunk_size
  last chunk must have 1 <= plaintext_len <= chunk_size
  download_size = 16 + image_size + chunk_count * 36
```

## Key Material

### Firmware secrets

```rust
pub const OTA_ENCRYPTION_KEY_ID: u32 = 1;
pub const OTA_ENCRYPTION_MASTER_KEY: [u8; 32] = [...];
pub const OTA_TLS_ALLOW_INVALID_CA: bool = true;
pub const OTA_TLS_CA_CERT_DER: &[u8] = &[];
```

### Key derivation

```text
key_id_be = u32 big-endian representation

aes_key =
  HMAC-SHA256(
    key = master_key,
    data = "motonet-ota aes-256-ctr key v1" || key_id_be
  )

hmac_key =
  HMAC-SHA256(
    key = master_key,
    data = "motonet-ota hmac-sha256 key v1" || key_id_be
  )
```

### GitHub Actions secret

```text
OTA_ENCRYPTION_MASTER_KEY_HEX  (64 hex chars, 32 raw bytes)
```

## Crypto Operations

### Per-chunk HMAC

```text
manifest_digest = SHA256(canonical_unsigned_manifest_json)

tag = HMAC-SHA256(
  key = hmac_key,
  data =
    "MOTONET-OTA-CHUNK-v2" ||
    manifest_digest ||
    chunk_index_u32_be ||
    plaintext_offset_u32_be ||
    plaintext_len_u32_be ||
    ciphertext
)
```

All numeric fields (`chunk_index`, `plaintext_offset`, `plaintext_len`) are `u32`, big-endian.

### AES-256-CTR counter block

```text
Per chunk:
  initial_counter_block[0..12]  = enc.nonce_prefix (12 bytes, 96 bits)
  initial_counter_block[12..16] = chunk_index_u32_be

Within chunk:
  counter increments from initial_counter_block for each AES block (16 bytes).
```

### Constant-time tag comparison

```rust
fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}
```

## ESP32 Hardware Acceleration

### Available hardware

| Peripheral | Original ESP32 | esp-hal driver |
| --- | --- | --- |
| AES (CTR) | ✓ | `AesContext` work-queue |
| SHA-256 | ✓ | `Sha256Context` work-queue |
| HMAC | ✗ chip has no HMAC engine | N/A |

### SHA-256 context sharing

Original ESP32 can only share SHA hardware across contexts by falling back to
software. esp-hal handles this transparently:

- First `Sha256Context` → hardware.
- Second (or more) → automatic `sha2::Sha256` software fallback.

For OTA, the plaintext hash context (large streaming data) gets hardware; the
per-chunk HMAC context (small fixed-size input) uses software fallback.

### AES and SHA driver lifecycle

- `AesBackend::new()` + `.start()` → `AesWorkQueueDriver` (must live for OTA).
- `ShaBackend::new()` + `.start()` → `ShaWorkQueueDriver` (must live for OTA).
- `AesContext::new(Ctr, Operation::Decrypt, key)` created per chunk.
- `Sha256Context::new()` created once for plaintext hash and once per chunk for HMAC.

## Firmware OTA Download Flow

```text
1. Parse & verify signed manifest.
2. Reject if schema != 2 or enc missing or enc.key_id != configured.
3. Compute manifest_digest = SHA256(canonical_unsigned_manifest_json).
4. Derive aes_key, hmac_key from master key.
5. Enter OtaState::Downloading, stand down MQTT, enter Applying.
6. Download encrypted container.
7. Verify 16-byte header (magic, version, reserved).
8. For each chunk (index 0..N-1):
   a. Read plaintext_len_be[4].
   b. Read ciphertext[len].
   c. Read tag[32].
   d. Compute expected HMAC tag.
   e. Constant-time compare tag.
   f. If mismatch: abort OTA, return error.
   g. Set AES-CTR counter = nonce_prefix || chunk_index_be.
   h. Decrypt ciphertext in-place.
   i. If first chunk: validate ESP image prefix on first 8 plaintext bytes.
   j. Write plaintext to inactive OTA slot.
   k. Update plaintext SHA-256.
9. Verify total plaintext bytes == image_size.
10. Verify plaintext SHA-256 == image_sha256.
11. Verify flash readback.
12. Activate new slot, mark app new, reboot.
```

## Implementation Commits

### Commit 1: `ota-core: introduce schema v2 manifest`

- `crates/ota-core/src/lib.rs`:
  - `SCHEMA_VERSION`: bump to 2.
  - New `OtaManifest` fields: `download_size`, `image_size`, `image_sha256`, `enc`.
  - `EncMetadata` struct: `alg`, `key_id`, `chunk_size`, `nonce_prefix`.
  - Remove old `size`, `sha256` fields.
  - Update `validate()` for v2 constraints.
  - Update `canonical_unsigned_json()` and `canonical_signed_json()` for v2 field order incl. `enc`.
  - Update `ManifestError` variants as needed.
  - Update all tests.
- `scripts/mqtt-test.sh`:
  - Update `default_ota_manifest()` (dummy) to v2 shape.
  - Remove manifest-building from `ota-send`; it now reads `--manifest FILE` only.

### Commit 2: `tools: add scripts/ota-pack.sh`

- New `scripts/ota-pack.sh`:
  - Arguments: `--input`, `--output`, `--manifest`, `--url`, `--version`, `--build`,
    `--signing-seed-hex-file`, `--ota-master-key-hex-file` (or env var),
    `--ota-key-id`, `--nonce-prefix-hex` (optional, auto-generated).
  - Computes `download_size`, `image_size`, `image_sha256` from input.
  - Generates random `nonce_prefix` (12 bytes = 24 hex chars).
  - Derives `aes_key`, `hmac_key` via `openssl dgst -hmac`.
  - Iterates chunks:
    - AES-256-CTR encrypt each chunk with `nonce_prefix || chunk_index_be` as IV.
    - HMAC-SHA256 tag over AAD + ciphertext.
    - Writes `[len_be][ciphertext][tag]`.
  - Builds unsigned canonical JSON, signs with Ed25519.
  - Writes `.enc` and `.manifest.json`.
  - Self-check: decrypt verify loop to confirm `image_sha256` matches.
- Update `scripts/mqtt-test.sh ota-send` to call `ota-pack.sh` before publishing.

### Commit 3: `firmware: encrypted OTA download with HW crypto`

- `firmware/Cargo.toml`: remove `aes` + `hmac` crate deps (if present); `sha2` already present.
- `firmware/src/secrets.rs.example`:
  - Add `OTA_ENCRYPTION_KEY_ID`, `OTA_ENCRYPTION_MASTER_KEY`, `OTA_TLS_ALLOW_INVALID_CA`.
- `firmware/src/startup/hardware.rs`: extract AES + SHA peripherals, pass to OTA task.
- `firmware/src/ota/encrypted.rs` (new module):
  - `derive_subkeys()`: HMAC-SHA256 key derivation.
  - `verify_chunk_hmac()`: compute expected tag, constant-time compare.
  - `decrypt_chunk_ctr()`: AesContext CTR decrypt in-place.
  - `compute_manifest_digest()`: SHA-256 of canonical unsigned manifest JSON.
- `firmware/src/ota/flash_write.rs`:
  - `download_and_write_to_flash()`: detect `enc` in manifest → encrypted path.
  - Encrypted path: header verify → chunk loop with HMAC → AES decrypt → write.
  - Standard path: existing unencrypted flow (if still needed for non-encrypted dev OTA).
- `firmware/src/tasks/ota.rs`:
  - Initialize `AesBackend` + `ShaBackend` at task start or pass them in.
  - Pass crypto context into `download_and_write_to_flash()`.

### Commit 4: `ci: release encrypted OTA artifacts`

- `.github/workflows/ota-release.yml`:
  - Add `OTA_ENCRYPTION_MASTER_KEY_HEX` secret usage.
  - Call `scripts/ota-pack.sh` after build.
  - Upload only `firmware.bin.enc` and `firmware.manifest.json` (no plaintext `firmware.bin`).
- Update `README.md`: document encrypted OTA flow, secret setup, manifest shape.

## Verification Checklist

- `cargo +stable fmt --all --check`
- `cargo +stable test -p ota-core --target x86_64-unknown-linux-gnu`
- `cargo +stable clippy -p ota-core --target x86_64-unknown-linux-gnu -- -D warnings`
- `cargo check -Zbuild-std=core,alloc`
- `cargo check -Zbuild-std=core,alloc --features release-ota`
- `cargo clippy -Zbuild-std=core,alloc -- -D warnings`
- `cargo clippy -Zbuild-std=core,alloc --features release-ota -- -D warnings`
- `scripts/ota-pack.sh` self-check passes.
- `scripts/mqtt-test.sh ota-build && scripts/ota-pack.sh ... && scripts/mqtt-test.sh ota-serve ...`
- Smoke test on hardware (local HTTP encrypted OTA).

## Open Design Items (future)

- Per-device OTA encryption keys.
- Encryption key rotation (keyring).
- Move Wi-Fi/MQTT secrets out of firmware image into NVS/config partition.
- Hardware HMAC for ESP32 variants that support it (different cfg path).
