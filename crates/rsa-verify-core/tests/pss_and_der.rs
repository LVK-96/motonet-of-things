use rsa_verify_core::{
    Rsa2048PublicKey, RsaVerifyError, compute_m_prime, compute_r_squared_mod_n,
    parse_pkcs1_rsa2048_public_key_der, rsa2048_be_bytes_to_le_words,
    rsa2048_exponent_be_to_le_words, rsa2048_le_words_to_be_bytes,
    verify_pkcs1v15_sha256_encoded_message, verify_pss_sha256_encoded_message,
};

const RSA2048_PKCS1_DER: &[u8] = include_bytes!("fixtures/rsa2048-pkcs1.der");
const MESSAGE: &[u8] = b"motonet rsa pss verifier test message";
const ENCODED_MESSAGE: [u8; 256] = include!("fixtures/pss_sha256_em.rs");

#[test]
fn parses_pkcs1_rsa2048_public_key_der() {
    let key = parse_pkcs1_rsa2048_public_key_der(RSA2048_PKCS1_DER).unwrap();

    assert_eq!(key.modulus_be.len(), 256);
    assert_eq!(key.exponent_be, [0, 1, 0, 1]);
}

#[test]
fn verifies_valid_pss_sha256_encoded_message() {
    verify_pss_sha256_encoded_message(MESSAGE, &ENCODED_MESSAGE).unwrap();
}

#[test]
fn rejects_tampered_pss_sha256_encoded_message() {
    let mut em = ENCODED_MESSAGE;
    em[42] ^= 0x01;

    assert_eq!(
        verify_pss_sha256_encoded_message(MESSAGE, &em),
        Err(RsaVerifyError::InvalidSignature)
    );
}

#[test]
fn verifies_pkcs1v15_sha256_encoded_message() {
    use sha2::Digest;

    let message = b"certificate tbs bytes";
    let mut encoded = [0xff; 256];
    encoded[0] = 0x00;
    encoded[1] = 0x01;
    let digest_info_prefix = [
        0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01,
        0x05, 0x00, 0x04, 0x20,
    ];
    let separator_index = encoded.len() - digest_info_prefix.len() - 32 - 1;
    encoded[separator_index] = 0x00;
    let digest_info_start = separator_index + 1;
    encoded[digest_info_start..digest_info_start + digest_info_prefix.len()]
        .copy_from_slice(&digest_info_prefix);
    encoded[digest_info_start + digest_info_prefix.len()..]
        .copy_from_slice(&sha2::Sha256::digest(message));

    assert_eq!(
        verify_pkcs1v15_sha256_encoded_message(message, &encoded),
        Ok(())
    );

    encoded[255] ^= 0x01;
    assert_eq!(
        verify_pkcs1v15_sha256_encoded_message(message, &encoded),
        Err(RsaVerifyError::InvalidSignature)
    );
}

#[test]
fn rejects_malformed_der() {
    assert_eq!(
        parse_pkcs1_rsa2048_public_key_der(&[0x30, 0x01, 0x00]),
        Err(RsaVerifyError::MalformedDer)
    );
}

#[test]
fn public_key_struct_is_fixed_size() {
    let key = Rsa2048PublicKey {
        modulus_be: [0u8; 256],
        exponent_be: [0u8, 1, 0, 1],
    };
    assert_eq!(key.modulus_be.len(), 256);
}

#[test]
fn derives_esp32_rsa_hardware_parameters_from_public_key() {
    let key = parse_pkcs1_rsa2048_public_key_der(RSA2048_PKCS1_DER).unwrap();

    let modulus_le = rsa2048_be_bytes_to_le_words(&key.modulus_be);
    assert_eq!(
        &modulus_le[..4],
        &[0x33b3f6e9, 0x21454951, 0x2009d3bd, 0x299cec73]
    );

    assert_eq!(rsa2048_le_words_to_be_bytes(&modulus_le), key.modulus_be);

    let exponent_le = rsa2048_exponent_be_to_le_words(&key.exponent_be);
    assert_eq!(exponent_le[0], 65_537);
    assert!(exponent_le[1..].iter().all(|word| *word == 0));

    assert_eq!(compute_m_prime(&modulus_le), 0x4bf1bea7);

    let r_squared = compute_r_squared_mod_n(&modulus_le);
    assert_eq!(
        &r_squared[..8],
        &[
            0x590778ba, 0xc104172d, 0x7a7f9d53, 0xbbe3ef5c, 0xabd19361, 0xf3bd779d, 0x2b2991d3,
            0xe46e65b9,
        ]
    );
    assert_eq!(
        &r_squared[56..],
        &[
            0x9a0f749d, 0xd8338d65, 0xea512d9a, 0xf7422745, 0x0b7af41f, 0x9d635745, 0x6766cc6f,
            0x756e6db0,
        ]
    );
}
