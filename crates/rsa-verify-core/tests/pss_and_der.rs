use rsa_verify_core::{
    Rsa2048PublicKey, RsaVerifyError, parse_pkcs1_rsa2048_public_key_der,
    verify_pss_sha256_encoded_message,
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
