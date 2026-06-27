use core::convert::TryInto;
use core::ptr::{read_volatile, write_volatile};

use defmt::{debug, info, warn};
use embedded_tls::{RsaVerifier, TlsError};
use esp_hal::Blocking;
use esp_hal::peripherals::RSA;
use esp_hal::rsa::Rsa;
use rsa_verify_core::{
    RsaVerifyError, compute_m_prime, compute_r_squared_mod_n, parse_pkcs1_rsa2048_public_key_der,
    rsa2048_be_bytes_to_le_words, rsa2048_exponent_be_to_le_words, rsa2048_le_words_to_be_bytes,
    verify_pkcs1v15_sha256_encoded_message, verify_pss_sha256_encoded_message,
};

pub(crate) static OTA_RSA_VERIFIER: Esp32RsaVerifier = Esp32RsaVerifier;

pub(crate) struct Esp32RsaVerifier;

impl Esp32RsaVerifier {
    fn rsa_public_operation_2048(
        public_key_der: &[u8],
        signature: &[u8],
    ) -> Result<[u8; 256], TlsError> {
        info!(
            "TLS RSA: public_key_der_len={}, signature_len={}",
            public_key_der.len(),
            signature.len()
        );
        let public_key =
            parse_pkcs1_rsa2048_public_key_der(public_key_der).map_err(map_rsa_error)?;
        let signature_be: &[u8; 256] = signature.try_into().map_err(|_| TlsError::DecodeError)?;

        let modulus = rsa2048_be_bytes_to_le_words(&public_key.modulus_be);
        let exponent = rsa2048_exponent_be_to_le_words(&public_key.exponent_be);
        let signature_words = rsa2048_be_bytes_to_le_words(signature_be);
        let m_prime = compute_m_prime(&modulus);
        let r_squared = compute_r_squared_mod_n(&modulus);
        let mut output_words = [0u32; 64];

        critical_section::with(|_| {
            // SAFETY: `init_crypto_peripherals` stores the RSA peripheral during boot before
            // tasks can run. The critical section serializes this short blocking hardware use.
            let rsa_peripheral = unsafe { crate::ota::take_rsa() };
            output_words = rsa_modexp_words(
                rsa_peripheral,
                &exponent,
                &modulus,
                m_prime,
                &signature_words,
                &r_squared,
            );
        });

        Ok(rsa2048_le_words_to_be_bytes(&output_words))
    }
}

const RSA_BASE: usize = 0x3ff0_2000;
const RSA_M_MEM: usize = RSA_BASE;
const RSA_Z_MEM: usize = RSA_BASE + 0x200;
const RSA_Y_MEM: usize = RSA_BASE + 0x400;
const RSA_X_MEM: usize = RSA_BASE + 0x600;
const RSA_M_PRIME: usize = RSA_BASE + 0x800;
const RSA_MODEXP_MODE: usize = RSA_BASE + 0x804;
const RSA_MODEXP_START: usize = RSA_BASE + 0x808;
const RSA_INTERRUPT: usize = RSA_BASE + 0x814;
const RSA_CLEAN: usize = RSA_BASE + 0x818;
const RSA_MEM_WORDS: usize = 128;
#[cfg(feature = "rsa-self-test")]
const RSA_2048_WORDS: usize = 64;

/// Run a raw ESP32 RSA-2048 modular exponentiation.
///
/// `esp-hal` 1.1.1 models the original ESP32 RSA memory blocks as only 32
/// words wide even though the hardware windows are 128 words wide. Its safe
/// iterator-based writes therefore truncate RSA-2048 operands to 1024 bits.
/// Use direct volatile MMIO for the RSA memory windows and keep `Rsa` only as
/// the peripheral clock/reset guard.
#[cfg(feature = "rsa-self-test")]
pub(crate) fn rsa2048_modexp_words(
    rsa_peripheral: RSA<'static>,
    exponent: &[u32; RSA_2048_WORDS],
    modulus: &[u32; RSA_2048_WORDS],
    m_prime: u32,
    base: &[u32; RSA_2048_WORDS],
    r_squared: &[u32; RSA_2048_WORDS],
) -> [u32; RSA_2048_WORDS] {
    rsa_modexp_words(rsa_peripheral, exponent, modulus, m_prime, base, r_squared)
}

fn rsa_modexp_words<const WORDS: usize>(
    rsa_peripheral: RSA<'static>,
    exponent: &[u32; WORDS],
    modulus: &[u32; WORDS],
    m_prime: u32,
    base: &[u32; WORDS],
    r_squared: &[u32; WORDS],
) -> [u32; WORDS] {
    let _rsa = Rsa::<Blocking>::new(rsa_peripheral);
    let mut output = [0u32; WORDS];

    // SAFETY: These are the documented ESP32 RSA accelerator registers. The
    // caller serializes access with a critical section in the OTA path; startup
    // self-test runs before tasks can concurrently use RSA.
    unsafe {
        while read_reg(RSA_CLEAN) & 1 == 0 {}
        write_reg(RSA_INTERRUPT, 1);
        write_reg(RSA_MODEXP_MODE, (WORDS as u32 / 16) - 1);
        write_reg(RSA_M_PRIME, m_prime);
        write_block(RSA_M_MEM, modulus);
        write_block(RSA_Y_MEM, exponent);
        write_block(RSA_X_MEM, base);
        write_block(RSA_Z_MEM, r_squared);
        write_reg(RSA_MODEXP_START, 1);
        while read_reg(RSA_INTERRUPT) & 1 == 0 {}
        read_block(RSA_Z_MEM, &mut output);
        write_reg(RSA_INTERRUPT, 1);
    }

    debug!("TLS RSA: raw output low word 0x{:08x}", output[0]);
    output
}

unsafe fn read_reg(address: usize) -> u32 {
    unsafe { read_volatile(address as *const u32) }
}

unsafe fn write_reg(address: usize, value: u32) {
    unsafe { write_volatile(address as *mut u32, value) }
}

unsafe fn write_block<const WORDS: usize>(address: usize, words: &[u32; WORDS]) {
    for index in 0..RSA_MEM_WORDS {
        let value = if index < WORDS { words[index] } else { 0 };
        unsafe { write_volatile((address + index * 4) as *mut u32, value) };
    }
}

unsafe fn read_block<const WORDS: usize>(address: usize, words: &mut [u32; WORDS]) {
    for (index, word) in words.iter_mut().enumerate() {
        *word = unsafe { read_volatile((address + index * 4) as *const u32) };
    }
}

impl RsaVerifier for Esp32RsaVerifier {
    fn verify_pss_sha256(
        &self,
        public_key_der: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), TlsError> {
        info!("TLS RSA: verifying RSA-PSS-SHA256 signature with ESP32 accelerator");
        let encoded_message = Self::rsa_public_operation_2048(public_key_der, signature)?;
        verify_pss_sha256_encoded_message(message, &encoded_message).map_err(map_rsa_error)
    }

    fn verify_pkcs1v15_sha256(
        &self,
        public_key_der: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), TlsError> {
        info!("TLS RSA: verifying RSA-PKCS1v15-SHA256 signature with ESP32 accelerator");
        let encoded_message = Self::rsa_public_operation_2048(public_key_der, signature)?;
        verify_pkcs1v15_sha256_encoded_message(message, &encoded_message).map_err(map_rsa_error)
    }
}

fn map_rsa_error(error: RsaVerifyError) -> TlsError {
    match error {
        RsaVerifyError::InvalidSignature => TlsError::InvalidSignature,
        RsaVerifyError::MalformedDer | RsaVerifyError::UnsupportedKey => {
            warn!("TLS RSA: unsupported or invalid RSA key");
            TlsError::DecodeError
        }
    }
}
