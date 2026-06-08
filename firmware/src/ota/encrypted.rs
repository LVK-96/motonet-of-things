//! AES-256-CTR decryption, HMAC-SHA256 verification, and key derivation
//! for v2 encrypted OTA firmware downloads.
//!
//! Uses the ESP32 hardware crypto accelerators via the `esp-hal` work-queue
//! API. The `AesBackend` and `ShaBackend` must be started (`.start()`) and
//! their drivers kept alive for the lifetime of any [`AesContext`] /
//! [`Sha256Context`] operations. This module only provides pure functions and
//! key-derivation helpers; context creation is done in the download pipeline.

use esp_hal::aes::{AesContext, Operation, cipher_modes::Ctr};
use esp_hal::sha::Sha256Context;

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

/// HMAC-SHA256-based key expansion: `HMAC-SHA256(prk, info || key_id_be)`.
fn hkdf_expand(prk: &[u8; 32], info: &[u8], key_id: u32) -> [u8; 32] {
    let key_id_be = key_id.to_be_bytes();
    hmac_sha256_raw(prk, &[info, &key_id_be])
}

// ── HMAC-SHA256 (manual, two-pass via Sha256Context) ──────────────────────

/// Compute HMAC-SHA256 with a 32-byte key, streaming over multiple input parts.
fn hmac_sha256_raw(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    // If key > block size, hash it first. Our key is always 32 ≤ 64, so skip.
    let mut k_inner = [IPAD; HMAC_BLOCK_SIZE];
    let mut k_outer = [OPAD; HMAC_BLOCK_SIZE];

    for i in 0..HMAC_KEY_SIZE {
        k_inner[i] ^= key[i];
        k_outer[i] ^= key[i];
    }

    // Inner: SHA256(k_ipad || parts[0] || parts[1] || ...)
    let mut ctx = Sha256Context::new();
    ctx.update(&k_inner).wait_blocking();
    for part in parts {
        ctx.update(part).wait_blocking();
    }
    let mut inner = [0u8; SHA256_OUTPUT_SIZE];
    ctx.finalize(&mut inner).wait_blocking();

    // Outer: SHA256(k_opad || inner)
    let mut ctx = Sha256Context::new();
    ctx.update(&k_outer).wait_blocking();
    ctx.update(&inner).wait_blocking();
    let mut digest = [0u8; SHA256_OUTPUT_SIZE];
    ctx.finalize(&mut digest).wait_blocking();

    digest
}

// ── Manifest digest ───────────────────────────────────────────────────────

/// Compute the SHA-256 digest of the canonical unsigned manifest JSON.
///
/// This binds the chunk HMAC verification to a specific manifest, preventing
/// an attacker from substituting a different manifest.
#[must_use]
pub fn compute_manifest_digest(canonical_json: &str) -> [u8; SHA256_OUTPUT_SIZE] {
    let mut ctx = Sha256Context::new();
    ctx.update(canonical_json.as_bytes()).wait_blocking();
    let mut digest = [0u8; SHA256_OUTPUT_SIZE];
    ctx.finalize(&mut digest).wait_blocking();
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

// ── AES error ─────────────────────────────────────────────────────────────

/// AES operation error returned by [`aes_ctr_crypt_in_place`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AesError {
    /// Underlying AES hardware operation failed.
    OperationFailed,
}

// ── AES-256-CTR (in-place, hardware-accelerated) ─────────────────────────

/// AES-256-CTR decrypt (or encrypt) `data` in-place.
///
/// Creates a fresh [`AesContext`] for each call. The counter is constructed as
/// `nonce_prefix[12] || chunk_index_u32_be` (16 bytes). The hardware CTR
/// implementation auto-increments the counter for each subsequent 16-byte
/// block within the same operation.
///
/// # Errors
///
/// Returns [`AesError::OperationFailed`] if the underlying AES hardware
/// operation fails.
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
    let handle = ctx
        .process_in_place(data)
        .map_err(|_| AesError::OperationFailed)?;
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
