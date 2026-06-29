//! AES-256-CTR decryption, HMAC-SHA256 verification, and key derivation
//! for v2 encrypted OTA firmware downloads.
//!
//! AES-256-CTR decryption uses the ESP32 hardware accelerator; SHA-256
//! uses the hardware path where buffers fit and software hashing for
//! streaming plaintext digests.

use esp_hal::aes::{AesContext, Operation, cipher_modes::Ctr};
use sha2::Digest;

const USE_HARDWARE_SHA: bool = true;

/// Reset the SHA peripheral to a clean state before operations.
///
/// This is a no-op placeholder; hardware SHA is serialized block-by-block in
/// [`hw_sha256`] to work around the ESP32 SHA_CONTINUE pipelining bug.
pub fn gate_sha_clock() {
    // No-op: hardware SHA is serialized block-by-block below.
}

/// SHA-256 digest length in bytes.
pub const SHA256_OUTPUT_SIZE: usize = 32;

/// AES-256 key length in bytes.
pub const AES_KEY_SIZE: usize = 32;

/// HMAC-SHA256 key length in bytes (same as master key).
pub const HMAC_KEY_SIZE: usize = 32;

/// HMAC-SHA256 block size (inner/outer padding).
const HMAC_BLOCK_SIZE: usize = 64;

/// HMAC inner pad byte.
const IPAD: u8 = 0x36;

/// HMAC outer pad byte.
const OPAD: u8 = 0x5C;

/// OTA v2 chunk HMAC context string.
const CHUNK_HMAC_CONTEXT: &[u8] = b"MOTONET-OTA-CHUNK-v2";

/// Info string for deriving AES subkey.
const AES_KEY_INFO: &[u8] = b"motonet-ota aes-256-ctr key v1";

/// Info string for deriving HMAC subkey.
const HMAC_KEY_INFO: &[u8] = b"motonet-ota hmac-sha256 key v1";

/// OTA v2 header magic bytes (8 bytes).
pub const OTA_HEADER_MAGIC: &[u8; 8] = b"MOTOTA2\0";

/// OTA v2 schema version for the header.
pub const OTA_V2_HEADER_VERSION: u8 = 2;

/// OTA v2 header size in bytes (magic + version + reserved).
pub const OTA_V2_HEADER_SIZE: usize = 16;

/// HMAC-SHA256 tag size in bytes (raw digest).
pub const HMAC_TAG_SIZE: usize = SHA256_OUTPUT_SIZE;

/// Per-chunk wire overhead: 4-byte length prefix + 32-byte HMAC tag.
pub const CHUNK_WIRE_OVERHEAD: usize = 4 + HMAC_TAG_SIZE;

// ── Hardware SHA-256 (block-serialized via esp-hal Sha driver) ──────

/// Compute SHA-256 using the ESP32 hardware accelerator.
///
/// Uses the esp-hal [`Sha`] driver directly (not the work-queue-based
/// [`ShaBackend`]) and spins on `is_busy()` between blocks to avoid the
/// pipelining bug where `write_data` would overwrite `SHA_TEXT` while
/// the engine is still processing the previous block.
#[allow(dead_code)]
#[must_use]
pub fn hw_sha256(data: &[u8], sha_peripheral: &esp_hal::peripherals::SHA<'static>) -> [u8; 32] {
    // Safety: the OTA task serialises SHA access.
    let sha_handle = unsafe { sha_peripheral.clone_unchecked() };
    let mut sha = esp_hal::sha::Sha::new(sha_handle);
    let mut digest = sha.start::<esp_hal::sha::Sha256>();

    let mut remaining: &[u8] = data;
    while !remaining.is_empty() {
        remaining = nb::block!(digest.update(remaining)).unwrap();
        while digest.is_busy() {}
    }

    let mut result = [0u8; 32];
    nb::block!(digest.finish(&mut result)).unwrap();
    result
}

// ── Key derivation ────────────────────────────────────────────────────────

/// Derive AES-256 and HMAC-SHA256 subkeys from a 32-byte master key and
/// `key_id` (from the manifest).
///
/// Uses HMAC-SHA256 as a KDF with separate info strings so the two subkeys
/// are independent.
#[must_use]
pub fn derive_subkeys(master: &[u8; 32], key_id: u32) -> ([u8; AES_KEY_SIZE], [u8; HMAC_KEY_SIZE]) {
    let aes_key = hkdf_expand(master, AES_KEY_INFO, key_id);
    let hmac_key = hkdf_expand(master, HMAC_KEY_INFO, key_id);
    (aes_key, hmac_key)
}

/// Public test-only wrapper for [`hmac_sha256_raw`].
/// Used by the boot-time HMAC self-test.
#[doc(hidden)]
#[must_use]
pub fn hmac_sha256_test(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    hmac_sha256_raw(key, parts)
}

/// HMAC-SHA256-based key expansion: `HMAC-SHA256(prk, info || key_id_be)`.
fn hkdf_expand(prk: &[u8; 32], info: &[u8], key_id: u32) -> [u8; 32] {
    let key_id_be = key_id.to_be_bytes();
    hmac_sha256_raw(prk, &[info, &key_id_be])
}

// ── HMAC-SHA256 ─────────────────────────────────────────────────────

/// Compute HMAC-SHA256 with a 32-byte key, streaming over multiple input parts.
fn hmac_sha256_raw(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    let mut k_inner = [IPAD; HMAC_BLOCK_SIZE];
    let mut k_outer = [OPAD; HMAC_BLOCK_SIZE];

    for i in 0..HMAC_KEY_SIZE {
        k_inner[i] ^= key[i];
        k_outer[i] ^= key[i];
    }

    // Inner: SHA256(k_ipad || parts)
    let inner = if USE_HARDWARE_SHA {
        let mut data: heapless::Vec<
            u8,
            {
                HMAC_BLOCK_SIZE + HMAC_TAG_SIZE + 32 + 4 + 4 + 4 + ota_core::ENC_CHUNK_SIZE as usize
            },
        > = heapless::Vec::new();
        data.extend_from_slice(&k_inner).ok();
        for part in parts {
            data.extend_from_slice(part).ok();
        }
        let sha = unsafe { crate::ota::sha_ref() };
        hw_sha256(&data, sha)
    } else {
        let mut hasher = sha2::Sha256::new();
        hasher.update(k_inner);
        for part in parts {
            hasher.update(part);
        }
        let result = hasher.finalize();
        let mut d = [0u8; SHA256_OUTPUT_SIZE];
        d.copy_from_slice(&result);
        d
    };

    // Outer: SHA256(k_opad || inner)
    if USE_HARDWARE_SHA {
        let sha = unsafe { crate::ota::sha_ref() };
        let mut data = [0u8; HMAC_BLOCK_SIZE + SHA256_OUTPUT_SIZE];
        data[..HMAC_BLOCK_SIZE].copy_from_slice(&k_outer);
        data[HMAC_BLOCK_SIZE..].copy_from_slice(&inner);
        hw_sha256(&data, sha)
    } else {
        let mut hasher = sha2::Sha256::new();
        hasher.update(k_outer);
        hasher.update(inner);
        let result = hasher.finalize();
        let mut digest = [0u8; SHA256_OUTPUT_SIZE];
        digest.copy_from_slice(&result);
        digest
    }
}

// ── Manifest digest ───────────────────────────────────────────────────────

/// Compute the SHA-256 digest of the canonical unsigned manifest JSON.
#[must_use]
pub fn compute_manifest_digest(canonical_json: &str) -> [u8; SHA256_OUTPUT_SIZE] {
    if USE_HARDWARE_SHA {
        // SAFETY: called from OTA task which holds the SHA peripheral guard.
        let sha = unsafe { crate::ota::sha_ref() };
        hw_sha256(canonical_json.as_bytes(), sha)
    } else {
        let mut hasher = sha2::Sha256::new();
        hasher.update(canonical_json.as_bytes());
        let result = hasher.finalize();
        let mut digest = [0u8; SHA256_OUTPUT_SIZE];
        digest.copy_from_slice(&result);
        digest
    }
}

/// Compute the SHA-256 digest of arbitrary bytes.
///
/// Always uses software SHA-256 (streaming).  The plaintext hashing
/// accumulates across ~283 chunks; batching 1.1 MB for hardware SHA
/// would require buffering the entire image in RAM.
#[must_use]
pub fn sha256_digest(data: &[u8]) -> [u8; SHA256_OUTPUT_SIZE] {
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut digest = [0u8; SHA256_OUTPUT_SIZE];
    digest.copy_from_slice(&result);
    digest
}

// ── Chunk HMAC ────────────────────────────────────────────────────────────

/// Compute the HMAC tag for a single encrypted chunk.
///
/// Authenticated input (in order):
/// `MOTONET-OTA-CHUNK-v2 || manifest_digest[32] || chunk_index_u32_be || plaintext_offset_u32_be || plaintext_len_u32_be || ciphertext`
#[must_use]
pub fn compute_chunk_hmac(
    hmac_key: &[u8; HMAC_KEY_SIZE],
    manifest_digest: &[u8; SHA256_OUTPUT_SIZE],
    chunk_index: u32,
    plaintext_offset: u32,
    plaintext_len: u32,
    ciphertext: &[u8],
) -> [u8; HMAC_TAG_SIZE] {
    let chunk_index_be = chunk_index.to_be_bytes();
    let plaintext_offset_be = plaintext_offset.to_be_bytes();
    let plaintext_len_be = plaintext_len.to_be_bytes();
    hmac_sha256_raw(
        hmac_key,
        &[
            CHUNK_HMAC_CONTEXT,
            manifest_digest,
            &chunk_index_be,
            &plaintext_offset_be,
            &plaintext_len_be,
            ciphertext,
        ],
    )
}

/// Verify a chunk HMAC tag in constant time.
///
/// Returns `true` if `tag` matches the computed HMAC.
#[must_use]
pub fn verify_chunk_hmac(
    hmac_key: &[u8; HMAC_KEY_SIZE],
    manifest_digest: &[u8; SHA256_OUTPUT_SIZE],
    chunk_index: u32,
    plaintext_offset: u32,
    plaintext_len: u32,
    ciphertext: &[u8],
    tag: &[u8; HMAC_TAG_SIZE],
) -> bool {
    let expected = compute_chunk_hmac(
        hmac_key,
        manifest_digest,
        chunk_index,
        plaintext_offset,
        plaintext_len,
        ciphertext,
    );
    constant_time_eq(&expected, tag)
}

/// Constant-time byte comparison.
///
/// Avoids short-circuiting on the first differing byte to prevent timing
/// side-channel leakage of HMAC tag values.
#[must_use]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ── AES-256-CTR (in-place, hardware-accelerated) ─────────────────────────

/// AES-256-CTR decrypt (or encrypt) `data` in-place.
///
/// Creates a fresh [`AesContext`] for each call. The counter is constructed as
/// `nonce_prefix[12] || chunk_index_u32_be` (16 bytes). The hardware CTR
/// implementation auto-increments the counter for each subsequent 16-byte
/// block within the same operation.
///
/// # Panics
///
/// The caller must ensure an [`AesBackend`] with an active driver is running.
///
/// # Errors
///
/// Returns [`AesError::OperationFailed`] if the underlying AES
/// work-queue operation fails.
pub fn aes_ctr_crypt_in_place(
    key: &[u8; AES_KEY_SIZE],
    nonce_prefix: &[u8; 12],
    chunk_index: u32,
    data: &mut [u8],
) -> Result<(), AesError> {
    if data.is_empty() {
        return Ok(());
    }

    let mut nonce = [0u8; 16];
    nonce[..12].copy_from_slice(nonce_prefix);
    nonce[12..].copy_from_slice(&chunk_index.to_be_bytes());

    let ctr = Ctr::new(nonce);
    // CTR always uses the "encrypt" operation to generate keystream.
    let mut ctx = AesContext::new(ctr, Operation::Encrypt, *key);
    // process_in_place is infallible for CTR mode (no block alignment requirement).
    #[allow(clippy::expect_used)]
    let handle = ctx
        .process_in_place(data)
        .expect("AES CTR process_in_place should be infallible");
    handle.wait_blocking();
    Ok(())
}

// ── Nonce prefix parsing ──────────────────────────────────────────────────

/// Nonce prefix parse error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoncePrefixError {
    /// The hex string is not exactly 24 characters.
    InvalidLength,
    /// A character is not a valid hex digit.
    InvalidHex,
}

/// Parse a 24-character hex string into a 12-byte nonce prefix.
///
/// # Errors
///
/// Returns [`NoncePrefixError::InvalidLength`] if the string is not exactly
/// 24 hex characters, or [`NoncePrefixError::InvalidHex`] if any character is
/// not a valid hex digit.
pub fn parse_nonce_prefix(hex: &str) -> Result<[u8; 12], NoncePrefixError> {
    if hex.len() != 24 {
        return Err(NoncePrefixError::InvalidLength);
    }
    let mut bytes = [0u8; 12];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        if chunk.len() < 2 {
            return Err(NoncePrefixError::InvalidLength);
        }
        bytes[i] = (hex_nibble(chunk[0])? << 4) | hex_nibble(chunk[1])?;
    }
    Ok(bytes)
}

fn hex_nibble(b: u8) -> Result<u8, NoncePrefixError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(NoncePrefixError::InvalidHex),
    }
}

// ── Header validation ─────────────────────────────────────────────────────

/// OTA v2 stream header validation errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaHeaderError {
    TooShort,
    InvalidMagic,
    InvalidVersion,
    ReservedNonZero,
}

/// Validate the 16-byte OTA v2 stream header.
///
/// Expected format:
/// - Bytes 0..7: `MOTOTA2\0`
/// - Byte 8: version (must be [`OTA_V2_HEADER_VERSION`])
/// - Bytes 9..15: reserved (must be all zero)
///
/// # Errors
///
/// Returns [`OtaHeaderError`] if the header is malformed or unsupported.
pub fn validate_ota_header(header: &[u8]) -> Result<(), OtaHeaderError> {
    if header.len() < OTA_V2_HEADER_SIZE {
        return Err(OtaHeaderError::TooShort);
    }
    if header[..8] != *OTA_HEADER_MAGIC {
        return Err(OtaHeaderError::InvalidMagic);
    }
    if header[8] != OTA_V2_HEADER_VERSION {
        return Err(OtaHeaderError::InvalidVersion);
    }
    if header[9..16].iter().any(|&b| b != 0) {
        return Err(OtaHeaderError::ReservedNonZero);
    }
    Ok(())
}

/// AES operation errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AesError {
    /// The AES work-queue operation failed.
    OperationFailed,
}
