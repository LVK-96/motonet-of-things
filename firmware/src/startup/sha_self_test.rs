//! SHA-256 self-test for esp-hal's `Sha256Context` API.
//!
//! These checks compare the SHA work-queue context against `sha2::Sha256`
//! across single-block, multi-block, repeated-context, HMAC-style, and
//! AES-adjacent scenarios.

use defmt::{error, info};
use sha2::Digest;

/// Compute a hex string from a 32-byte digest.
#[allow(clippy::unwrap_used)]
fn digest_hex(d: &[u8; 32]) -> heapless::String<64> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut buf = [0u8; 64];
    for (i, byte) in d.iter().enumerate() {
        buf[i * 2] = HEX[(byte >> 4) as usize];
        buf[i * 2 + 1] = HEX[(byte & 0xF) as usize];
    }
    let s = unsafe { core::str::from_utf8_unchecked(&buf) };
    let vec = heapless::Vec::<u8, 64>::from_slice(s.as_bytes()).unwrap();
    unsafe { heapless::String::from_utf8_unchecked(vec) }
}

fn hw_sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut ctx = esp_hal::sha::Sha256Context::new();
    for part in parts {
        ctx.update(part).wait_blocking();
    }
    let mut digest = [0u8; 32];
    esp_hal::sha::Sha256Context::finalize(&mut ctx, &mut digest).wait_blocking();
    digest
}

fn sw_sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    let result = hasher.finalize();
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&result);
    digest
}

fn hw_hmac_sha256(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
    let mut k_inner = [0x36u8; 64];
    let mut k_outer = [0x5Cu8; 64];
    for i in 0..32 {
        k_inner[i] ^= key[i];
        k_outer[i] ^= key[i];
    }

    let inner = {
        let mut ctx = esp_hal::sha::Sha256Context::new();
        ctx.update(&k_inner).wait_blocking();
        for part in parts {
            ctx.update(part).wait_blocking();
        }
        let mut digest = [0u8; 32];
        esp_hal::sha::Sha256Context::finalize(&mut ctx, &mut digest).wait_blocking();
        digest
    };

    hw_sha256(&[&k_outer, &inner])
}

fn log_sha_result(label: &str, hw: &[u8; 32], sw: &[u8; 32]) {
    if hw == sw {
        info!("{}: PASSED", label);
    } else {
        error!("{}: FAILED", label);
        error!("  sw: {}", digest_hex(sw));
        error!("  hw: {}", digest_hex(hw));
    }
}

// ── Test 1: known vector ────────────────────────────────────────────────

fn sha_isolation_test() {
    let test_input = b"HELLO, ESPRESSIF!";
    let expected = "4e7ed9c02ddc57b87bee406850a374739d10414e3c3d551a4cf7f134e811c9d2";

    let hw = hw_sha256(&[test_input]);
    let sw = sw_sha256(&[test_input]);
    let hw_hex = digest_hex(&hw);

    info!("SHA self-test input: \"HELLO, ESPRESSIF!\"");
    info!("  expected: {}", expected);
    info!("  hw:       {}", hw_hex);
    if hw == sw && hw_hex.as_str() == expected {
        info!("SHA isolation test: PASSED");
    } else {
        error!("SHA isolation test: FAILED");
        error!("  sw:       {}", digest_hex(&sw));
    }
}

// ── Test 2: helper used by OTA HMAC/manifest code ───────────────────────

fn ota_sha_context_test() {
    let test_data = [0xA5u8; 128];
    let hw = hw_sha256(&[&test_data]);
    let sw = sw_sha256(&[&test_data]);
    log_sha_result("SHA context helper test", &hw, &sw);
}

// ── Test 3: multi-block SHA (input > 64 bytes) ──────────────────────────

fn sha_multiblock_test() {
    let test_data = [0xA5u8; 128];
    let hw = hw_sha256(&[&test_data[..64], &test_data[64..]]);
    let sw = sw_sha256(&[&test_data]);
    log_sha_result("SHA multiblock test", &hw, &sw);
}

// ── Test 4: sequential context reuse ────────────────────────────────────

fn sha_sequential_reuse_test() {
    let test_data: &[u8; 32] = b"0123456789abcdef0123456789abcdef";
    let sw = sw_sha256(&[test_data]);

    let hw1 = hw_sha256(&[test_data]);
    let hw2 = hw_sha256(&[test_data]);

    if hw1 == sw && hw2 == sw && hw1 == hw2 {
        info!("SHA sequential reuse test: PASSED");
    } else {
        error!("SHA sequential reuse test: FAILED");
        error!("  sw:        {}", digest_hex(&sw));
        error!("  hw1:       {}", digest_hex(&hw1));
        error!("  hw2:       {}", digest_hex(&hw2));
    }
}

// ── Test 5: HMAC-style manual inner/outer SHA ───────────────────────────

fn hmac_simplified_test() {
    let key: [u8; 32] = {
        let mut k = [0x0bu8; 32];
        k[20..].fill(0);
        k
    };
    let data = b"Hi There";
    let rfc_expected: [u8; 32] = [
        0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53, 0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b, 0xf1,
        0x2b, 0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7, 0x26, 0xe9, 0x37, 0x6c, 0x2e, 0x32,
        0xcf, 0xf7,
    ];

    let mut k_inner = [0x36u8; 64];
    let mut k_outer = [0x5Cu8; 64];
    for i in 0..32 {
        k_inner[i] ^= key[i];
        k_outer[i] ^= key[i];
    }

    let inner = hw_sha256(&[&k_inner, data]);
    let outer = hw_sha256(&[&k_outer, &inner]);

    if outer == rfc_expected {
        info!("HMAC simplified test: PASSED");
    } else {
        error!("HMAC simplified test: FAILED");
        error!("  RFC expected: {}", digest_hex(&rfc_expected));
        error!("  hw result:    {}", digest_hex(&outer));
        error!("  inner_digest: {}", digest_hex(&inner));
    }
}

// ── Test 6: OTA HMAC helper ─────────────────────────────────────────────

fn hmac_self_test() {
    let key: [u8; 32] = {
        let mut k = [0x0bu8; 32];
        k[20..].fill(0);
        k
    };
    let data = b"Hi There";
    let rfc_expected: [u8; 32] = [
        0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53, 0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b, 0xf1,
        0x2b, 0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7, 0x26, 0xe9, 0x37, 0x6c, 0x2e, 0x32,
        0xcf, 0xf7,
    ];

    let hw = hw_hmac_sha256(&key, &[data]);

    if hw == rfc_expected {
        info!("HMAC self-test: PASSED");
        info!("  hw: {}", digest_hex(&hw));
    } else {
        error!("HMAC self-test: FAILED");
        error!("  RFC expected: {}", digest_hex(&rfc_expected));
        error!("  hw computed:  {}", digest_hex(&hw));
    }
}

// ── Test 7: SHA after AES use ───────────────────────────────────────────

#[allow(clippy::unwrap_used)]
fn sha_after_aes_test() {
    let aes_key: [u8; 32] = [0xBBu8; 32];
    let nonce = [0u8; 16];
    let mut aes_buf = [0xCCu8; 128];
    let test_data: [u8; 128] = aes_buf;

    let mut aes_ctx = esp_hal::aes::AesContext::new(
        esp_hal::aes::cipher_modes::Ctr::new(nonce),
        esp_hal::aes::Operation::Encrypt,
        aes_key,
    );
    aes_ctx
        .process_in_place(&mut aes_buf)
        .unwrap()
        .wait_blocking();

    let hw = hw_sha256(&[&test_data]);
    let sw = sw_sha256(&[&test_data]);
    log_sha_result("SHA after AES test", &hw, &sw);
}

// ── Test 8: SHA, AES, SHA sequence ──────────────────────────────────────

#[allow(clippy::unwrap_used)]
fn sha_aes_sha_sequence_test() {
    let aes_key: [u8; 32] = [0xDDu8; 32];
    let nonce = [0u8; 16];
    let test_data: &[u8; 64] = &[0xEEu8; 64];

    let hw0 = hw_sha256(&[test_data]);

    let mut aes_buf = [0xFFu8; 64];
    let mut aes_ctx = esp_hal::aes::AesContext::new(
        esp_hal::aes::cipher_modes::Ctr::new(nonce),
        esp_hal::aes::Operation::Encrypt,
        aes_key,
    );
    aes_ctx
        .process_in_place(&mut aes_buf)
        .unwrap()
        .wait_blocking();

    let hw1 = hw_sha256(&[test_data]);
    let sw = sw_sha256(&[test_data]);

    if hw0 == sw && hw1 == sw && hw0 == hw1 {
        info!("SHA→AES→SHA sequence test: PASSED");
    } else {
        error!("SHA→AES→SHA sequence test: FAILED");
        error!("  sw:  {}", digest_hex(&sw));
        error!("  hw0: {}", digest_hex(&hw0));
        error!("  hw1: {}", digest_hex(&hw1));
    }
}

// ── Entry point ───────────────────────────────────────────────────────

#[allow(clippy::similar_names)]
pub(crate) fn run_sha_self_test(
    sha_peripheral: esp_hal::peripherals::SHA<'static>,
    aes_peripheral: esp_hal::peripherals::AES<'static>,
) {
    let mut sha_backend = esp_hal::sha::ShaBackend::new(sha_peripheral);
    let _sha_driver = sha_backend.start();
    let mut aes_backend = esp_hal::aes::AesBackend::new(aes_peripheral);
    let _aes_driver = aes_backend.start();

    sha_isolation_test();
    ota_sha_context_test();
    hmac_self_test();
    sha_multiblock_test();
    sha_sequential_reuse_test();
    hmac_simplified_test();
    sha_after_aes_test();
    sha_aes_sha_sequence_test();
}
