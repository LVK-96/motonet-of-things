#![no_std]

use sha2::{Digest, Sha256};

pub const RSA_2048_LEN: usize = 256;
const SHA256_LEN: usize = 32;
const PSS_TRAILER_FIELD: u8 = 0xbc;
const PSS_EM_LEN: usize = RSA_2048_LEN;
const PSS_MASKED_DB_LEN: usize = PSS_EM_LEN - SHA256_LEN - 1;
const PSS_SALT_LEN: usize = SHA256_LEN;
const PSS_PS_LEN: usize = PSS_MASKED_DB_LEN - PSS_SALT_LEN - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsaVerifyError {
    MalformedDer,
    UnsupportedKey,
    InvalidSignature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rsa2048PublicKey {
    pub modulus_be: [u8; RSA_2048_LEN],
    pub exponent_be: [u8; 4],
}

pub fn parse_pkcs1_rsa2048_public_key_der(der: &[u8]) -> Result<Rsa2048PublicKey, RsaVerifyError> {
    let mut reader = DerReader::new(der);
    let sequence = reader.read_tlv(0x30)?;
    if !reader.is_empty() {
        return Err(RsaVerifyError::MalformedDer);
    }

    let mut sequence_reader = DerReader::new(sequence);
    let modulus_integer = sequence_reader.read_tlv(0x02)?;
    let exponent_integer = sequence_reader.read_tlv(0x02)?;
    if !sequence_reader.is_empty() {
        return Err(RsaVerifyError::MalformedDer);
    }

    let modulus = parse_rsa2048_modulus(modulus_integer)?;
    let exponent = parse_rsa_exponent(exponent_integer)?;

    Ok(Rsa2048PublicKey {
        modulus_be: modulus,
        exponent_be: exponent,
    })
}

pub fn verify_pss_sha256_encoded_message(
    message: &[u8],
    encoded_message: &[u8; RSA_2048_LEN],
) -> Result<(), RsaVerifyError> {
    if encoded_message[RSA_2048_LEN - 1] != PSS_TRAILER_FIELD {
        return Err(RsaVerifyError::InvalidSignature);
    }
    if encoded_message[0] & 0x80 != 0 {
        return Err(RsaVerifyError::InvalidSignature);
    }

    let masked_db = &encoded_message[..PSS_MASKED_DB_LEN];
    let h = &encoded_message[PSS_MASKED_DB_LEN..RSA_2048_LEN - 1];

    let mut db_mask = [0u8; PSS_MASKED_DB_LEN];
    mgf1_sha256(h, &mut db_mask);

    let mut db = [0u8; PSS_MASKED_DB_LEN];
    for index in 0..PSS_MASKED_DB_LEN {
        db[index] = masked_db[index] ^ db_mask[index];
    }
    db[0] &= 0x7f;

    if !db[..PSS_PS_LEN].iter().all(|byte| *byte == 0) || db[PSS_PS_LEN] != 0x01 {
        return Err(RsaVerifyError::InvalidSignature);
    }

    let salt = &db[PSS_PS_LEN + 1..];
    let message_hash = Sha256::digest(message);
    let mut verifier_hash = Sha256::new();
    verifier_hash.update([0u8; 8]);
    verifier_hash.update(message_hash);
    verifier_hash.update(salt);
    let expected_h = verifier_hash.finalize();

    if expected_h.as_slice() == h {
        Ok(())
    } else {
        Err(RsaVerifyError::InvalidSignature)
    }
}

fn mgf1_sha256(seed: &[u8], out: &mut [u8]) {
    let mut counter = 0u32;
    for chunk in out.chunks_mut(SHA256_LEN) {
        let mut hasher = Sha256::new();
        hasher.update(seed);
        hasher.update(counter.to_be_bytes());
        let digest = hasher.finalize();
        let copy_len = chunk.len();
        chunk.copy_from_slice(&digest[..copy_len]);
        counter = counter.wrapping_add(1);
    }
}

fn parse_rsa2048_modulus(integer: &[u8]) -> Result<[u8; RSA_2048_LEN], RsaVerifyError> {
    let modulus_bytes = match integer {
        [0, rest @ ..] if rest.len() == RSA_2048_LEN => rest,
        rest if rest.len() == RSA_2048_LEN => rest,
        _ => return Err(RsaVerifyError::UnsupportedKey),
    };

    let mut modulus = [0u8; RSA_2048_LEN];
    modulus.copy_from_slice(modulus_bytes);
    Ok(modulus)
}

fn parse_rsa_exponent(integer: &[u8]) -> Result<[u8; 4], RsaVerifyError> {
    if integer.is_empty() || integer.len() > 4 {
        return Err(RsaVerifyError::UnsupportedKey);
    }

    let mut exponent = [0u8; 4];
    let start = 4 - integer.len();
    exponent[start..].copy_from_slice(integer);

    if exponent == [0u8; 4] || exponent[3] & 1 == 0 {
        return Err(RsaVerifyError::UnsupportedKey);
    }

    Ok(exponent)
}

struct DerReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> DerReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn read_tlv(&mut self, tag: u8) -> Result<&'a [u8], RsaVerifyError> {
        if self.read_byte()? != tag {
            return Err(RsaVerifyError::MalformedDer);
        }
        let len = self.read_len()?;
        let end = self
            .offset
            .checked_add(len)
            .ok_or(RsaVerifyError::MalformedDer)?;
        if end > self.bytes.len() {
            return Err(RsaVerifyError::MalformedDer);
        }
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }

    fn read_byte(&mut self) -> Result<u8, RsaVerifyError> {
        let byte = self
            .bytes
            .get(self.offset)
            .copied()
            .ok_or(RsaVerifyError::MalformedDer)?;
        self.offset += 1;
        Ok(byte)
    }

    fn read_len(&mut self) -> Result<usize, RsaVerifyError> {
        let first = self.read_byte()?;
        if first & 0x80 == 0 {
            return Ok(usize::from(first));
        }

        let len_len = usize::from(first & 0x7f);
        if len_len == 0 || len_len > core::mem::size_of::<usize>() {
            return Err(RsaVerifyError::MalformedDer);
        }

        let mut len = 0usize;
        for _ in 0..len_len {
            len = (len << 8) | usize::from(self.read_byte()?);
        }
        Ok(len)
    }
}
