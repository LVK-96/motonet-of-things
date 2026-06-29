use core::convert::TryInto;

#[cfg(feature = "rsa-self-test")]
use defmt::error;
use defmt::{debug, info, warn};
use embedded_tls::{RsaVerifier, TlsError};
use esp_hal::rsa::{Rsa, RsaModularExponentiation, operand_sizes::Op2048};
use rsa_verify_core::{
    RSA_2048_LEN, Rsa2048PublicKey, RsaVerifyError, compute_m_prime, compute_r_squared_mod_n,
    parse_pkcs1_rsa2048_public_key_der, rsa2048_be_bytes_to_le_words,
    rsa2048_exponent_be_to_le_words, rsa2048_le_words_to_be_bytes,
    verify_pkcs1v15_sha256_encoded_message, verify_pss_sha256_encoded_message,
};

const RSA_2048_WORDS: usize = 64;

pub(crate) static OTA_RSA_VERIFIER: Esp32RsaVerifier = Esp32RsaVerifier;

pub(crate) struct Esp32RsaVerifier;

impl Esp32RsaVerifier {
    fn verify_sha256_signature(
        public_key_der: &[u8],
        message: &[u8],
        signature: &[u8],
        verify_encoded_message: fn(&[u8], &[u8; RSA_2048_LEN]) -> Result<(), RsaVerifyError>,
    ) -> Result<(), TlsError> {
        let encoded_message = critical_section::with(|_| {
            // SAFETY: `init_crypto_peripherals` stores the RSA peripheral before tasks run.
            let rsa = unsafe { super::take_rsa() };
            rsa_public_operation_2048(rsa, public_key_der, signature)
        })?;

        verify_encoded_message(message, &encoded_message).map_err(map_rsa_error)
    }
}

fn rsa_public_operation_2048(
    rsa: esp_hal::peripherals::RSA<'static>,
    public_key_der: &[u8],
    signature: &[u8],
) -> Result<[u8; RSA_2048_LEN], TlsError> {
    info!(
        "TLS RSA: public_key_der_len={}, signature_len={}",
        public_key_der.len(),
        signature.len()
    );

    let public_key: Rsa2048PublicKey =
        parse_pkcs1_rsa2048_public_key_der(public_key_der).map_err(map_rsa_error)?;
    let signature_be: &[u8; RSA_2048_LEN] =
        signature.try_into().map_err(|_| TlsError::DecodeError)?;

    let modulus = rsa2048_be_bytes_to_le_words(&public_key.modulus_be);
    let exponent = rsa2048_exponent_be_to_le_words(&public_key.exponent_be);
    let base = rsa2048_be_bytes_to_le_words(signature_be);
    let m_prime = compute_m_prime(&modulus);
    let r_squared = compute_r_squared_mod_n(&modulus);

    let mut rsa = Rsa::new(rsa);
    let mut operation =
        RsaModularExponentiation::<Op2048, _>::new(&mut rsa, &exponent, &modulus, m_prime);
    operation.start_exponentiation(&base, &r_squared);

    let mut result = [0u32; RSA_2048_WORDS];
    operation.read_results(&mut result);

    debug!("TLS RSA: raw output low word 0x{:08x}", result[0]);
    Ok(rsa2048_le_words_to_be_bytes(&result))
}

#[cfg(feature = "rsa-self-test")]
const SELF_TEST_MODULUS: [u32; RSA_2048_WORDS] = [
    0xd1f69f69, 0xbaa512f1, 0xcf1f5fd5, 0x59cc0cf9, 0xd1f6586f, 0xc9c70747, 0xfa958725, 0xabb4d39d,
    0x176f653f, 0x96f427bd, 0x5d395fa4, 0x6a7d04c0, 0xe10666cc, 0x8fdc84b1, 0xeeee998e, 0xe9df8f03,
    0x4b02e975, 0xdff66ca0, 0x301fb3cc, 0x35356005, 0x2c85beca, 0x5cf3c447, 0xba6c3d0c, 0x02d96581,
    0xc97049ed, 0xb01ff457, 0x6c1553a8, 0xc94f1b93, 0x96306c1a, 0xc581715f, 0x09aa7a72, 0xebeee5ff,
    0xf37d2955, 0x80a9a0a9, 0xbeafc8fe, 0x099742ed, 0x44dcfbc3, 0x1b1db004, 0x8f910cbc, 0xe376c0c1,
    0xae3c68e7, 0x0f5f5b6d, 0x8f6f7732, 0x43ca8204, 0xf18b9201, 0x3c63c9d1, 0x33e7d090, 0x7506678f,
    0x13636f47, 0x36de5d8b, 0xb933b371, 0xafa3ccbc, 0x2c6aa83b, 0xcb838e4b, 0x222d1016, 0x4e9832bb,
    0xef8a47d3, 0x1d8d3166, 0x54986c79, 0x4594b217, 0x330ed8a9, 0x2b671e1d, 0x142a3a69, 0xbd8883b7,
];

#[cfg(feature = "rsa-self-test")]
const SELF_TEST_SIGNATURE: [u32; RSA_2048_WORDS] = [
    0xcfaa8f49, 0x10e428fc, 0xfe05d513, 0x6f8944e6, 0x2b6a4fb3, 0x63ae3a44, 0x5fb771ef, 0xfa6cab2f,
    0xe8cde3ee, 0x5b36b4b7, 0xd31fecd3, 0x96c9ac23, 0x6934aae3, 0xbbbdae47, 0x88b32bcc, 0x2e6d5f6b,
    0x3c131a09, 0x3fa0f6c3, 0x07fafb9f, 0x3e4433c5, 0x673f85ce, 0x567ff25c, 0x17a94511, 0x8d79ab4e,
    0x54e2c782, 0xc6911f0e, 0x27c53706, 0x26e2b194, 0x2e6531ef, 0xaec1e8ed, 0x8e86b60f, 0x1641c7d3,
    0xb1bbd47f, 0x13bac9d0, 0x0ba7934d, 0xf862291b, 0xea917ed6, 0xce0c90dc, 0xd027a571, 0xd80d4509,
    0x3e61908a, 0x8cb9b0a8, 0xf4633cef, 0xbc5ac1b3, 0xf036134e, 0x1dff3d8f, 0xaf54eb2a, 0xb9b41818,
    0x4c711472, 0x87e53446, 0x2b88518d, 0xcb518f97, 0x14d004d5, 0x56d1ebfb, 0x6ba2aff7, 0x13353041,
    0x427b40f1, 0xe9018a49, 0x729cd94f, 0xf2a5dada, 0xad08ee7f, 0x77abd7cf, 0xc7c93fb9, 0x4f4710de,
];

#[cfg(feature = "rsa-self-test")]
const SELF_TEST_EXPECTED: [u32; RSA_2048_WORDS] = [
    0x54207631, 0x20544553, 0x53454c46, 0x52534120, 0x20485720, 0x53503332, 0x45542045, 0x4f544f4e,
    0x0000004d, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000,
    0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000,
    0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000,
    0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000,
    0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000,
    0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000,
    0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000, 0x00000000,
];

#[cfg(feature = "rsa-self-test")]
pub(crate) fn run_rsa_self_test(rsa: esp_hal::peripherals::RSA<'static>) {
    info!("RSA self-test: starting hardware RSA-2048 public operation");

    let public_key_der = self_test_public_key_der();
    let signature = self_test_words_to_be_bytes(&SELF_TEST_SIGNATURE);
    let expected = self_test_words_to_be_bytes(&SELF_TEST_EXPECTED);

    let output = match rsa_public_operation_2048(rsa, &public_key_der, &signature) {
        Ok(output) => output,
        Err(_) => {
            error!("RSA self-test: FAILED");
            panic!("RSA hardware self-test failed");
        }
    };

    if output == expected {
        info!("RSA self-test: PASSED");
    } else {
        error!("RSA self-test: FAILED");
        panic!("RSA hardware self-test failed");
    }
}

#[cfg(feature = "rsa-self-test")]
fn self_test_public_key_der() -> [u8; 270] {
    let modulus = self_test_words_to_be_bytes(&SELF_TEST_MODULUS);
    let mut der = [0u8; 270];

    der[..8].copy_from_slice(&[0x30, 0x82, 0x01, 0x0a, 0x02, 0x82, 0x01, 0x01]);
    der[8] = 0;
    der[9..265].copy_from_slice(&modulus);
    der[265..].copy_from_slice(&[0x02, 0x03, 0x01, 0x00, 0x01]);

    der
}

#[cfg(feature = "rsa-self-test")]
fn self_test_words_to_be_bytes(words: &[u32; RSA_2048_WORDS]) -> [u8; RSA_2048_LEN] {
    let mut bytes = [0u8; RSA_2048_LEN];

    for (word, chunk) in words.iter().rev().zip(bytes.chunks_exact_mut(4)) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }

    bytes
}

impl RsaVerifier for Esp32RsaVerifier {
    fn verify_pkcs1v15_sha256(
        &self,
        public_key_der: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), TlsError> {
        info!("TLS RSA: verifying RSA-PKCS1v15-SHA256 signature with ESP32 accelerator");
        Self::verify_sha256_signature(
            public_key_der,
            message,
            signature,
            verify_pkcs1v15_sha256_encoded_message,
        )
    }

    fn verify_pss_sha256(
        &self,
        public_key_der: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), TlsError> {
        info!("TLS RSA: verifying RSA-PSS-SHA256 signature with ESP32 accelerator");
        Self::verify_sha256_signature(
            public_key_der,
            message,
            signature,
            verify_pss_sha256_encoded_message,
        )
    }

    fn verify_pss_sha384(
        &self,
        _public_key_der: &[u8],
        _message: &[u8],
        _signature: &[u8],
    ) -> Result<(), TlsError> {
        Err(TlsError::InvalidSignatureScheme)
    }

    fn verify_pss_sha512(
        &self,
        _public_key_der: &[u8],
        _message: &[u8],
        _signature: &[u8],
    ) -> Result<(), TlsError> {
        Err(TlsError::InvalidSignatureScheme)
    }
}

fn map_rsa_error(error: RsaVerifyError) -> TlsError {
    warn!("RSA verification error: {:?}", defmt::Debug2Format(&error));
    match error {
        RsaVerifyError::MalformedDer | RsaVerifyError::UnsupportedKey => {
            warn!("TLS RSA: malformed or unsupported RSA key");
            TlsError::InvalidCertificate
        }
        RsaVerifyError::InvalidSignature => TlsError::InvalidSignature,
    }
}
