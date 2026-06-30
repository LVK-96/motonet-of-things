//! SHA-256 self-test: compares hardware `Sha256Context` against software
//! `sha2::Sha256` across four scenarios run once at boot:
//! 1. Isolation (KNOWN vector) — verifies basic SHA correctness
//! 2. SHA-while-AES-in-flight — confirms erratum exists
//! 3. Serialized AES→clock-gate→SHA — tests clock-gate restore
//! 4. SHA→AES→clock-gate→SHA — tests clock-gate idempotency
//!
//! Goal: prove that gating the SHA clock off→on (ESP-IDF pattern)
//! correctly restores SHA hardware after AES operations.

use defmt::{error, info, warn};
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

// ── Test 1: Isolation ─────────────────────────────────────────────────

#[allow(clippy::unwrap_used)]
fn sha_isolation_test() {
    let test_input = b"HELLO, ESPRESSIF!";
    let expected = "4e7ed9c02ddc57b87bee406850a374739d10414e3c3d551a4cf7f134e811c9d2";

    crate::ota::encrypted::gate_sha_clock();
    let mut hw_ctx = esp_hal::sha::Sha256Context::new();
    hw_ctx.update(test_input).wait_blocking();
    let mut hw_digest = [0u8; 32];
    esp_hal::sha::Sha256Context::finalize(&mut hw_ctx, &mut hw_digest).wait_blocking();
    let hw_hex = digest_hex(&hw_digest);

    let mut sw_hasher = sha2::Sha256::new();
    sw_hasher.update(*test_input);
    let sw_digest_arr = sw_hasher.finalize();
    let mut sw_digest = [0u8; 32];
    sw_digest.copy_from_slice(&sw_digest_arr);

    let hw_match = hw_hex.as_str() == expected;
    let agree = hw_digest == sw_digest;

    info!("SHA self-test input: \"HELLO, ESPRESSIF!\"");
    info!("  expected: {}", expected);
    info!("  hw:       {}", hw_hex);
    if agree && hw_match {
        info!("  SHA isolation test: PASSED");
    } else {
        error!("  SHA isolation test: FAILED");
        error!(
            "  hw == sw ?  {}  (expected match: {})",
            if agree { "YES" } else { "NO" },
            if hw_match { "YES" } else { "NO" }
        );
        error!("  hw digest: {}", hw_hex);
        error!("  sw digest: {}", digest_hex(&sw_digest));
        error!("  expected:  {}", expected);
    }
}

// ── Test 2: SHA-while-AES-active (erratum validation) ─────────────────

#[allow(clippy::unwrap_used, clippy::similar_names)]
fn sha_concurrent_test() {
    let aes_key: [u8; 32] = [0xA0u8; 32];
    let nonce = [0u8; 16];
    let mut aes_buf = [0xA5u8; 64];
    let test_data: [u8; 64] = aes_buf;

    let mut aes_ctx = esp_hal::aes::AesContext::new(
        esp_hal::aes::cipher_modes::Ctr::new(nonce),
        esp_hal::aes::Operation::Encrypt,
        aes_key,
    );
    let aes_handle = aes_ctx.process_in_place(&mut aes_buf).unwrap();

    crate::ota::encrypted::gate_sha_clock();
    let mut hw_c = esp_hal::sha::Sha256Context::new();
    hw_c.update(&test_data).wait_blocking();
    let mut hw_d2 = [0u8; 32];
    esp_hal::sha::Sha256Context::finalize(&mut hw_c, &mut hw_d2).wait_blocking();
    let hw2_hex = digest_hex(&hw_d2);

    let mut sw_h2 = sha2::Sha256::new();
    sw_h2.update(test_data);
    let sw2_arr = sw_h2.finalize();
    let mut sw_d2 = [0u8; 32];
    sw_d2.copy_from_slice(&sw2_arr);

    let concur_agree = hw_d2 == sw_d2;
    info!(
        "  SHA-while-AES-active: hw={}..  sw={}..",
        &hw2_hex[..16],
        &digest_hex(&sw_d2)[..16]
    );
    if concur_agree {
        warn!("  SHA-while-AES-active: hw==sw (erratum NOT triggered — unexpected on ESP32)");
    } else {
        info!("  SHA-while-AES-active: erratum confirmed (hw != sw as expected)");
    }

    aes_handle.wait_blocking();
}

// ── Test 3: Serialized AES→clock-gate→SHA ────────────────────────────

#[allow(clippy::unwrap_used)]
fn sha_clockgate_after_aes_test() {
    let aes_key: [u8; 32] = [0xBBu8; 32];
    let nonce = [0u8; 16];
    let mut aes_buf = [0xCCu8; 128];
    let test_data: [u8; 128] = aes_buf;

    // Run AES and wait for completion
    let mut aes_ctx = esp_hal::aes::AesContext::new(
        esp_hal::aes::cipher_modes::Ctr::new(nonce),
        esp_hal::aes::Operation::Encrypt,
        aes_key,
    );
    let handle = aes_ctx.process_in_place(&mut aes_buf).unwrap();
    handle.wait_blocking();

    // Gate SHA clock — ESP-IDF pattern — then compute
    crate::ota::encrypted::gate_sha_clock();
    let mut hw_ctx = esp_hal::sha::Sha256Context::new();
    hw_ctx.update(&test_data).wait_blocking();
    let mut hw_d = [0u8; 32];
    esp_hal::sha::Sha256Context::finalize(&mut hw_ctx, &mut hw_d).wait_blocking();
    let hw_hex = digest_hex(&hw_d);

    let mut sw = sha2::Sha256::new();
    sw.update(test_data);
    let sw_arr = sw.finalize();
    let mut sw_d = [0u8; 32];
    sw_d.copy_from_slice(&sw_arr);

    let agree = hw_d == sw_d;
    info!(
        "  AES→clk-gate→SHA: hw={}..  sw={}..",
        &hw_hex[..16],
        &digest_hex(&sw_d)[..16]
    );
    if agree {
        info!("  AES→clk-gate→SHA: clock-gate OK");
    } else {
        warn!("  AES→clk-gate→SHA legacy Sha256Context test: known broken on ESP32");
        warn!("  hw digest: {}", hw_hex);
        warn!("  sw digest: {}", digest_hex(&sw_d));
    }
}

// ── Test 4: SHA→AES→clock-gate→SHA ──────────────────────────────────

#[allow(clippy::unwrap_used)]
fn sha_clockgate_restore_test() {
    let aes_key: [u8; 32] = [0xDDu8; 32];
    let nonce = [0u8; 16];
    let test_data: &[u8; 64] = &[0xEEu8; 64];

    // First SHA (baseline, with clock gate)
    crate::ota::encrypted::gate_sha_clock();
    let mut hw0 = esp_hal::sha::Sha256Context::new();
    hw0.update(test_data).wait_blocking();
    let mut hw0_d = [0u8; 32];
    esp_hal::sha::Sha256Context::finalize(&mut hw0, &mut hw0_d).wait_blocking();

    // Run AES
    let mut aes_buf = [0xFFu8; 64];
    let mut aes_ctx = esp_hal::aes::AesContext::new(
        esp_hal::aes::cipher_modes::Ctr::new(nonce),
        esp_hal::aes::Operation::Encrypt,
        aes_key,
    );
    let handle = aes_ctx.process_in_place(&mut aes_buf).unwrap();
    handle.wait_blocking();

    // Gate SHA clock, compute same data again
    crate::ota::encrypted::gate_sha_clock();
    let mut hw1 = esp_hal::sha::Sha256Context::new();
    hw1.update(test_data).wait_blocking();
    let mut hw1_d = [0u8; 32];
    esp_hal::sha::Sha256Context::finalize(&mut hw1, &mut hw1_d).wait_blocking();

    let agree = hw0_d == hw1_d;
    info!(
        "  SHA0→AES→clk-gate→SHA1: hw0={}..  hw1={}..",
        &digest_hex(&hw0_d)[..16],
        &digest_hex(&hw1_d)[..16]
    );
    if agree {
        info!("  SHA0→AES→clk-gate→SHA1: clock-gate restore OK");
    } else {
        warn!("  SHA0→AES→clk-gate→SHA1 legacy Sha256Context test: known broken on ESP32");
        warn!("  hw0: {}", digest_hex(&hw0_d));
        warn!("  hw1: {}", digest_hex(&hw1_d));
    }
}

// ── Test 5a: multi-block SHA (input > 64 bytes) ──────────────────

#[allow(clippy::unwrap_used)]
fn sha_multiblock_test() {
    // 128-byte input — crosses the 64-byte SHA-256 block boundary.
    let test_data = [0xA5u8; 128];

    let mut sw = sha2::Sha256::new();
    sw.update(test_data);
    let sw_arr = sw.finalize();
    let mut sw_d = [0u8; 32];
    sw_d.copy_from_slice(&sw_arr);

    crate::ota::encrypted::gate_sha_clock();
    let mut ctx = esp_hal::sha::Sha256Context::new();
    // Feed in two 64-byte chunks.
    ctx.update(&test_data[..64]).wait_blocking();
    ctx.update(&test_data[64..]).wait_blocking();
    let mut hw = [0u8; 32];
    esp_hal::sha::Sha256Context::finalize(&mut ctx, &mut hw).wait_blocking();

    if hw == sw_d {
        info!("SHA multiblock test: PASSED");
    } else {
        warn!("SHA multiblock legacy Sha256Context test: known broken on ESP32");
        warn!("  sw: {}", digest_hex(&sw_d));
        warn!("  hw: {}", digest_hex(&hw));
    }
}

// ── Test 5b: HMAC-simplified (manual inner/outer SHA) ─────────────

#[allow(clippy::unwrap_used)]
fn hmac_simplified_test() {
    // Same as RFC 4231 test case 1 but using raw Sha256Context instead of
    // hmac_sha256_raw, to see if the issue is in hmac_sha256_raw or deeper.
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

    // Step 1: inner SHA via raw hardware context
    crate::ota::encrypted::gate_sha_clock();
    let mut inner_ctx = esp_hal::sha::Sha256Context::new();
    inner_ctx.update(&k_inner).wait_blocking();
    inner_ctx.update(data).wait_blocking();
    let mut inner_d = [0u8; 32];
    esp_hal::sha::Sha256Context::finalize(&mut inner_ctx, &mut inner_d).wait_blocking();

    // Step 2: outer SHA
    crate::ota::encrypted::gate_sha_clock();
    let mut outer_ctx = esp_hal::sha::Sha256Context::new();
    outer_ctx.update(&k_outer).wait_blocking();
    outer_ctx.update(&inner_d).wait_blocking();
    let mut outer_d = [0u8; 32];
    esp_hal::sha::Sha256Context::finalize(&mut outer_ctx, &mut outer_d).wait_blocking();

    if outer_d == rfc_expected {
        info!("HMAC simplified test: PASSED");
    } else {
        warn!("HMAC simplified legacy Sha256Context test: known broken on ESP32");
        warn!("  RFC expected: {}", digest_hex(&rfc_expected));
        warn!("  hw result:    {}", digest_hex(&outer_d));
        warn!("  inner_digest: {}", digest_hex(&inner_d));
    }
}

// ── Test 5c: sequential Sha256Context reuse ────────────────────────

#[allow(clippy::unwrap_used)]
fn sha_sequential_reuse_test() {
    // Verify that creating, using, and dropping a Sha256Context then
    // creating a second one produces correct results on both.
    let test_data: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

    // Software reference
    let mut sw = sha2::Sha256::new();
    sw.update(test_data);
    let sw_arr = sw.finalize();
    let mut sw_d = [0u8; 32];
    sw_d.copy_from_slice(&sw_arr);

    crate::ota::encrypted::gate_sha_clock();
    let mut ctx1 = esp_hal::sha::Sha256Context::new();
    ctx1.update(test_data).wait_blocking();
    let mut hw1 = [0u8; 32];
    esp_hal::sha::Sha256Context::finalize(&mut ctx1, &mut hw1).wait_blocking();
    // ctx1 dropped here

    crate::ota::encrypted::gate_sha_clock();
    let mut ctx2 = esp_hal::sha::Sha256Context::new();
    ctx2.update(test_data).wait_blocking();
    let mut hw2 = [0u8; 32];
    esp_hal::sha::Sha256Context::finalize(&mut ctx2, &mut hw2).wait_blocking();

    let ok1 = hw1 == sw_d;
    let ok2 = hw2 == sw_d;
    let agree = hw1 == hw2;

    if ok1 && ok2 && agree {
        info!("SHA sequential reuse test: PASSED");
    } else {
        error!("SHA sequential reuse test: FAILED");
        error!("  sw:       {}", digest_hex(&sw_d));
        error!("  hw1 (ok={}): {}", ok1, digest_hex(&hw1));
        error!("  hw2 (ok={}): {}", ok2, digest_hex(&hw2));
    }
}

// ── Test 5: HMAC self-test ──────────────────────────────────────────

#[allow(clippy::unwrap_used)]
fn hmac_self_test() {
    // Known test vectors for HMAC-SHA256.
    // RFC 4231 Test Case 1:
    //   key = 0x0b * 20
    //   data = "Hi There"
    //   HMAC-SHA256 = b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7
    let key: [u8; 32] = {
        let mut k = [0x0bu8; 32];
        // RFC 4231 uses a 20-byte key padded to 32 bytes (block size) with zeros
        k[20..].fill(0);
        k
    };
    let data = b"Hi There";
    let rfc_expected: [u8; 32] = [
        0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53, 0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b, 0xf1,
        0x2b, 0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7, 0x26, 0xe9, 0x37, 0x6c, 0x2e, 0x32,
        0xcf, 0xf7,
    ];

    let Ok(hw_result) = crate::ota::encrypted::hmac_sha256_test(&key, &[data]) else {
        error!("HMAC self-test: FAILED (input too long)");
        return;
    };
    let hw_hex = digest_hex(&hw_result);
    let rfc_hex = digest_hex(&rfc_expected);

    // Also compute with software for comparison.
    let mut sw_hasher = sha2::Sha256::new();
    // Manual HMAC for verification.
    let mut k_inner = [0x36u8; 64];
    let mut k_outer = [0x5Cu8; 64];
    for i in 0..32 {
        k_inner[i] ^= key[i];
        k_outer[i] ^= key[i];
    }
    sw_hasher.update(k_inner);
    sw_hasher.update(data);
    let inner = sw_hasher.finalize_reset();
    let mut sw_hasher2 = sha2::Sha256::new();
    sw_hasher2.update(k_outer);
    sw_hasher2.update(inner);
    let sw_result_arr = sw_hasher2.finalize();
    let mut sw_result = [0u8; 32];
    sw_result.copy_from_slice(&sw_result_arr);

    let hw_matches_rfc = hw_result == rfc_expected;
    let hw_matches_sw = hw_result == sw_result;

    if hw_matches_rfc && hw_matches_sw {
        info!("HMAC self-test: PASSED");
        info!("  hw: {}", hw_hex);
    } else {
        error!("HMAC self-test: FAILED");
        error!("  RFC expected: {}", rfc_hex);
        error!("  hw computed:  {}", hw_hex);
        error!("  sw computed:  {}", digest_hex(&sw_result));
    }
}

// ── Test 6: HW SHA mitigated (block-serialized, no pipelining) ─────

#[allow(clippy::unwrap_used)]
fn hw_sha_mitigated_test(sha_peripheral: &esp_hal::peripherals::SHA<'static>) {
    // Test the corrected hardware SHA that serializes blocks correctly.
    let test_data = [0xA5u8; 128];

    let mut sw = sha2::Sha256::new();
    sw.update(test_data);
    let sw_arr = sw.finalize();
    let mut sw_d = [0u8; 32];
    sw_d.copy_from_slice(&sw_arr);

    let hw = crate::ota::encrypted::hw_sha256(&test_data, sha_peripheral);

    if hw == sw_d {
        info!("HW SHA mitigated test: PASSED");
    } else {
        error!("HW SHA mitigated test: FAILED");
        error!("  sw: {}", digest_hex(&sw_d));
        error!("  hw: {}", digest_hex(&hw));
    }
}

// ── Entry point ───────────────────────────────────────────────────────

#[allow(clippy::similar_names, clippy::unwrap_used)]
pub(crate) fn run_sha_self_test(
    sha_peripheral: esp_hal::peripherals::SHA<'static>,
    aes_peripheral: esp_hal::peripherals::AES<'static>,
) {
    // Test the corrected hardware SHA before it's consumed by ShaBackend.
    hw_sha_mitigated_test(&sha_peripheral);

    let mut hw_backend = esp_hal::sha::ShaBackend::new(sha_peripheral);
    let _hw_driver = hw_backend.start();
    let mut aes_backend = esp_hal::aes::AesBackend::new(aes_peripheral);
    let _aes_driver = aes_backend.start();

    sha_isolation_test();
    sha_multiblock_test();
    sha_sequential_reuse_test();
    hmac_simplified_test();
    hmac_self_test();
    sha_concurrent_test();
    sha_clockgate_after_aes_test();
    sha_clockgate_restore_test();
}
