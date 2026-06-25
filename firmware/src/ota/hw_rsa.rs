use core::convert::TryInto;

use defmt::{debug, warn};
use embedded_tls::{RsaVerifier, TlsError};
use esp_hal::Blocking;
use esp_hal::rsa::{Rsa, RsaModularExponentiation, operand_sizes::Op2048};
use rsa_verify_core::{
    RsaVerifyError, compute_m_prime, compute_r_squared_mod_n, parse_pkcs1_rsa2048_public_key_der,
    rsa2048_be_bytes_to_le_words, rsa2048_exponent_be_to_le_words, rsa2048_le_words_to_be_bytes,
    verify_pkcs1v15_sha256_encoded_message, verify_pss_sha256_encoded_message,
};

pub(crate) static OTA_RSA_VERIFIER: Esp32RsaVerifier = Esp32RsaVerifier;

pub(crate) struct Esp32RsaVerifier;

impl Esp32RsaVerifier {
    fn rsa_public_operation(
        public_key_der: &[u8],
        signature: &[u8],
    ) -> Result<[u8; 256], TlsError> {
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
            let mut rsa = Rsa::new(rsa_peripheral);
            let mut operation = RsaModularExponentiation::<Op2048, Blocking>::new(
                &mut rsa, &exponent, &modulus, m_prime,
            );
            operation.start_exponentiation(&signature_words, &r_squared);
            operation.read_results(&mut output_words);
        });

        Ok(rsa2048_le_words_to_be_bytes(&output_words))
    }
}

impl RsaVerifier for Esp32RsaVerifier {
    fn verify_pss_sha256(
        &self,
        public_key_der: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), TlsError> {
        debug!("TLS RSA: verifying RSA-PSS-SHA256 signature with ESP32 accelerator");
        let encoded_message = Self::rsa_public_operation(public_key_der, signature)?;
        verify_pss_sha256_encoded_message(message, &encoded_message).map_err(map_rsa_error)
    }

    fn verify_pkcs1v15_sha256(
        &self,
        public_key_der: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), TlsError> {
        debug!("TLS RSA: verifying RSA-PKCS1v15-SHA256 signature with ESP32 accelerator");
        let encoded_message = Self::rsa_public_operation(public_key_der, signature)?;
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
