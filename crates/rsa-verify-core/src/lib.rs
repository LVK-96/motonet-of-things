#![no_std]

use sha2::{Digest, Sha256};

pub const RSA_2048_LEN: usize = 256;
const SHA256_LEN: usize = 32;
const PSS_TRAILER_FIELD: u8 = 0xbc;
const PSS_EM_LEN: usize = RSA_2048_LEN;
const PSS_MASKED_DB_LEN: usize = PSS_EM_LEN - SHA256_LEN - 1;
const PSS_SALT_LEN: usize = SHA256_LEN;
const PSS_PS_LEN: usize = PSS_MASKED_DB_LEN - PSS_SALT_LEN - 1;
const SHA256_DIGEST_INFO_PREFIX: &[u8; 19] = &[
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
    0x00, 0x04, 0x20,
];

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

pub fn rsa2048_be_bytes_to_le_words(bytes: &[u8; RSA_2048_LEN]) -> [u32; 64] {
    let mut words = [0u32; 64];
    let mut index = 0;
    while index < 64 {
        let byte_offset = RSA_2048_LEN - ((index + 1) * 4);
        words[index] = u32::from_be_bytes([
            bytes[byte_offset],
            bytes[byte_offset + 1],
            bytes[byte_offset + 2],
            bytes[byte_offset + 3],
        ]);
        index += 1;
    }
    words
}

pub fn rsa2048_le_words_to_be_bytes(words: &[u32; 64]) -> [u8; RSA_2048_LEN] {
    let mut bytes = [0u8; RSA_2048_LEN];
    let mut index = 0;
    while index < 64 {
        let byte_offset = RSA_2048_LEN - ((index + 1) * 4);
        bytes[byte_offset..byte_offset + 4].copy_from_slice(&words[index].to_be_bytes());
        index += 1;
    }
    bytes
}

pub fn rsa2048_exponent_be_to_le_words(bytes: &[u8; 4]) -> [u32; 64] {
    let mut words = [0u32; 64];
    words[0] = u32::from_be_bytes(*bytes);
    words
}

pub fn compute_m_prime(modulus_le: &[u32; 64]) -> u32 {
    let modulus_limb = modulus_le[0];
    let mut inverse = 1u32;
    let mut iteration = 0;
    while iteration < 5 {
        inverse = inverse.wrapping_mul(2u32.wrapping_sub(modulus_limb.wrapping_mul(inverse)));
        iteration += 1;
    }
    inverse.wrapping_neg()
}

pub fn compute_r_squared_mod_n(modulus_le: &[u32; 64]) -> [u32; 64] {
    let mut value = [0u32; 64];
    value[0] = 1;

    let mut bit = 0;
    while bit < RSA_2048_LEN * 16 {
        double_mod(&mut value, modulus_le);
        bit += 1;
    }

    value
}

fn double_mod(value: &mut [u32; 64], modulus: &[u32; 64]) {
    let mut carry = 0u64;
    let mut index = 0;
    while index < 64 {
        let doubled = (u64::from(value[index]) << 1) | carry;
        value[index] = doubled as u32;
        carry = doubled >> 32;
        index += 1;
    }

    if carry != 0 || cmp_le_words(value, modulus) != core::cmp::Ordering::Less {
        sub_assign_le_words(value, modulus);
    }
}

fn cmp_le_words(left: &[u32; 64], right: &[u32; 64]) -> core::cmp::Ordering {
    let mut index = 64;
    while index > 0 {
        index -= 1;
        if left[index] < right[index] {
            return core::cmp::Ordering::Less;
        }
        if left[index] > right[index] {
            return core::cmp::Ordering::Greater;
        }
    }
    core::cmp::Ordering::Equal
}

fn sub_assign_le_words(left: &mut [u32; 64], right: &[u32; 64]) {
    let mut borrow = 0u64;
    let mut index = 0;
    while index < 64 {
        let rhs = u64::from(right[index]) + borrow;
        let lhs = u64::from(left[index]);
        left[index] = lhs.wrapping_sub(rhs) as u32;
        borrow = u64::from(lhs < rhs);
        index += 1;
    }
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

pub fn verify_pkcs1v15_sha256_encoded_message(
    message: &[u8],
    encoded_message: &[u8; RSA_2048_LEN],
) -> Result<(), RsaVerifyError> {
    if encoded_message[0] != 0x00 || encoded_message[1] != 0x01 {
        return Err(RsaVerifyError::InvalidSignature);
    }

    let separator_index = RSA_2048_LEN - SHA256_DIGEST_INFO_PREFIX.len() - SHA256_LEN - 1;
    if !encoded_message[2..separator_index]
        .iter()
        .all(|byte| *byte == 0xff)
        || encoded_message[separator_index] != 0x00
    {
        return Err(RsaVerifyError::InvalidSignature);
    }

    let prefix_start = separator_index + 1;
    let digest_start = prefix_start + SHA256_DIGEST_INFO_PREFIX.len();
    if &encoded_message[prefix_start..digest_start] != SHA256_DIGEST_INFO_PREFIX {
        return Err(RsaVerifyError::InvalidSignature);
    }

    let expected_digest = Sha256::digest(message);
    if encoded_message[digest_start..] == expected_digest[..] {
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
