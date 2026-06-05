//! OTA manifest validation and canonical JSON construction.
//!
//! Signature verification is abstracted behind [`SignatureVerifier`] and backed
//! by an Ed25519 verifier for trusted manifest keyrings.

use ed25519_dalek::{Signature, Verifier as DalekVerifier, VerifyingKey};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u8 = 1;
pub const TARGET: &str = "motonet-of-things/esp32";
pub const CHIP: &str = "esp32-wroom";

pub const MAX_MANIFEST_BYTES: usize = 1024;
pub const MAX_URL_LEN: usize = 512;
pub const MAX_VERSION_LEN: usize = 32;
pub const MAX_TARGET_LEN: usize = 48;
pub const MAX_CHIP_LEN: usize = 24;
pub const MAX_REDIRECTS: usize = 3;
pub const SHA256_HEX_LEN: usize = 64;
pub const ED25519_SIGNATURE_HEX_LEN: usize = 128;
pub const DEV_TEST_KEY_ID: u32 = 1001;
pub const RELEASE_KEY_ID: u32 = 1;
pub const DEV_TEST_PUBLIC_KEY_HEX: &str =
    "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OtaManifest {
    pub schema: u8,
    pub key_id: u32,
    pub target: String,
    pub chip: String,
    pub version: String,
    pub build: u32,
    pub force: bool,
    pub url: String,
    pub size: u32,
    pub sha256: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    Oversized,
    Malformed,
    WrongSchema,
    WrongTarget,
    WrongChip,
    FieldTooLong(&'static str),
    InvalidSha256,
    MissingSignature,
    SignatureRejected,
}

pub trait SignatureVerifier {
    fn verify(&self, key_id: u32, canonical_manifest: &[u8], signature: &str) -> bool;
}

#[derive(Debug, Clone)]
pub struct Ed25519ManifestVerifier {
    trusted_keys: &'static [(u32, [u8; 32])],
}

impl Ed25519ManifestVerifier {
    #[must_use]
    pub const fn new(trusted_keys: &'static [(u32, [u8; 32])]) -> Self {
        Self { trusted_keys }
    }

    #[must_use]
    pub fn dev_ota() -> Self {
        Self::new(&[(DEV_TEST_KEY_ID, DEV_TEST_PUBLIC_KEY_BYTES)])
    }

    #[must_use]
    pub fn dev_test() -> Self {
        Self::dev_ota()
    }

    #[must_use]
    pub const fn release_ota() -> Self {
        Self::new(&[(RELEASE_KEY_ID, RELEASE_PUBLIC_KEY_BYTES)])
    }
}

// Public key only. The matching release signing key is expected to live in CI secrets.
const RELEASE_PUBLIC_KEY_BYTES: [u8; 32] = [
    0x9b, 0x50, 0x66, 0x3b, 0x6d, 0x52, 0x22, 0xf1, 0x2f, 0x1f, 0x1a, 0x3d, 0x1d, 0x3d, 0x3a, 0x0d,
    0x94, 0x07, 0xa7, 0x47, 0x7d, 0x69, 0x67, 0x26, 0xdf, 0x07, 0x23, 0x5d, 0x00, 0x2a, 0x7d, 0xfc,
];

const DEV_TEST_PUBLIC_KEY_BYTES: [u8; 32] = [
    0x8a, 0x88, 0xe3, 0xdd, 0x74, 0x09, 0xf1, 0x95, 0xfd, 0x52, 0xdb, 0x2d, 0x3c, 0xba, 0x5d, 0x72,
    0xca, 0x67, 0x09, 0xbf, 0x1d, 0x94, 0x12, 0x1b, 0xf3, 0x74, 0x88, 0x01, 0xb4, 0x0f, 0x6f, 0x5c,
];

impl SignatureVerifier for Ed25519ManifestVerifier {
    fn verify(&self, key_id: u32, canonical_manifest: &[u8], signature: &str) -> bool {
        let Some((_, public_key)) = self.trusted_keys.iter().find(|(id, _)| *id == key_id) else {
            return false;
        };
        let Ok(signature_bytes) = hex_signature(signature) else {
            return false;
        };
        let Ok(verifying_key) = VerifyingKey::from_bytes(public_key) else {
            return false;
        };
        let signature = Signature::from_bytes(&signature_bytes);
        verifying_key.verify(canonical_manifest, &signature).is_ok()
    }
}

fn hex_signature(value: &str) -> Result<[u8; 64], ()> {
    if value.len() != ED25519_SIGNATURE_HEX_LEN {
        return Err(());
    }
    let mut bytes = [0_u8; 64];
    hex_into(value, &mut bytes)?;
    Ok(bytes)
}

fn hex_into(value: &str, out: &mut [u8]) -> Result<(), ()> {
    if value.len() != out.len() * 2 {
        return Err(());
    }
    for (chunk, byte) in value.as_bytes().chunks_exact(2).zip(out.iter_mut()) {
        *byte = (hex_nibble_runtime(chunk[0])? << 4) | hex_nibble_runtime(chunk[1])?;
    }
    Ok(())
}

fn hex_nibble_runtime(byte: u8) -> Result<u8, ()> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(()),
    }
}

impl OtaManifest {
    /// Parse a signed manifest and validate the schema, device identity, limits,
    /// hash shape, and presence of a signature.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] when the manifest is too large, malformed, or
    /// fails shape/device validation.
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::Oversized);
        }

        let manifest: Self = serde_json::from_slice(bytes).map_err(|_| ManifestError::Malformed)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Parse and verify a signed manifest with the provided verifier.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] when parsing/validation fails or the signature
    /// is rejected.
    pub fn parse_and_verify(
        bytes: &[u8],
        verifier: &impl SignatureVerifier,
    ) -> Result<Self, ManifestError> {
        let manifest = Self::parse(bytes)?;
        let canonical = manifest.canonical_unsigned_json()?;
        if verifier.verify(manifest.key_id, canonical.as_bytes(), &manifest.signature) {
            Ok(manifest)
        } else {
            Err(ManifestError::SignatureRejected)
        }
    }

    /// Validate manifest schema, target identity, fixed limits, and hash shape.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] for any validation failure.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema != SCHEMA_VERSION {
            return Err(ManifestError::WrongSchema);
        }
        if self.target != TARGET {
            return Err(ManifestError::WrongTarget);
        }
        if self.chip != CHIP {
            return Err(ManifestError::WrongChip);
        }
        validate_len("target", &self.target, MAX_TARGET_LEN)?;
        validate_len("chip", &self.chip, MAX_CHIP_LEN)?;
        validate_len("version", &self.version, MAX_VERSION_LEN)?;
        validate_len("url", &self.url, MAX_URL_LEN)?;
        if self.sha256.len() != SHA256_HEX_LEN
            || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ManifestError::InvalidSha256);
        }
        if self.signature.is_empty() {
            return Err(ManifestError::MissingSignature);
        }
        if self.signature.len() != ED25519_SIGNATURE_HEX_LEN
            || !self.signature.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ManifestError::SignatureRejected);
        }
        Ok(())
    }

    /// Return the exact bytes covered by the manifest signature.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Malformed`] if canonical JSON construction fails.
    pub fn canonical_unsigned_json(&self) -> Result<String, ManifestError> {
        canonical_unsigned_json(self).map_err(|_| ManifestError::Malformed)
    }

    /// Return canonical signed JSON including the `signature` field.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Malformed`] if canonical JSON construction fails.
    pub fn canonical_signed_json(&self) -> Result<String, ManifestError> {
        canonical_signed_json(self).map_err(|_| ManifestError::Malformed)
    }
}

fn validate_len(field: &'static str, value: &str, max: usize) -> Result<(), ManifestError> {
    if value.len() > max {
        Err(ManifestError::FieldTooLong(field))
    } else {
        Ok(())
    }
}

fn json_string(value: &str) -> Result<String, serde_json::Error> {
    serde_json::to_string(value)
}

fn canonical_unsigned_json(manifest: &OtaManifest) -> Result<String, serde_json::Error> {
    Ok(format!(
        "{{\"schema\":{},\"key_id\":{},\"target\":{},\"chip\":{},\"version\":{},\"build\":{},\"force\":{},\"url\":{},\"size\":{},\"sha256\":{}}}",
        manifest.schema,
        manifest.key_id,
        json_string(&manifest.target)?,
        json_string(&manifest.chip)?,
        json_string(&manifest.version)?,
        manifest.build,
        manifest.force,
        json_string(&manifest.url)?,
        manifest.size,
        json_string(&manifest.sha256)?,
    ))
}

fn canonical_signed_json(manifest: &OtaManifest) -> Result<String, serde_json::Error> {
    let unsigned = canonical_unsigned_json(manifest)?;
    let prefix = unsigned.strip_suffix('}').ok_or_else(|| {
        serde_json::Error::io(std::io::Error::other("canonical object missing terminator"))
    })?;
    Ok(format!(
        "{},\"signature\":{}}}",
        prefix,
        json_string(&manifest.signature)?
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> OtaManifest {
        OtaManifest {
            schema: SCHEMA_VERSION,
            key_id: DEV_TEST_KEY_ID,
            target: TARGET.to_owned(),
            chip: CHIP.to_owned(),
            version: "0.2.0".to_owned(),
            build: 42,
            force: false,
            url: "http://192.168.1.10:8000/firmware.bin".to_owned(),
            size: 1_234_567,
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned(),
            signature: "00".repeat(64),
        }
    }

    #[test]
    fn canonical_unsigned_json_is_stable_and_excludes_signature() {
        let canonical = manifest()
            .canonical_unsigned_json()
            .expect("canonical json");
        assert_eq!(
            canonical,
            "{\"schema\":1,\"key_id\":1001,\"target\":\"motonet-of-things/esp32\",\"chip\":\"esp32-wroom\",\"version\":\"0.2.0\",\"build\":42,\"force\":false,\"url\":\"http://192.168.1.10:8000/firmware.bin\",\"size\":1234567,\"sha256\":\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"}"
        );
        assert!(!canonical.contains("signature"));
    }

    #[test]
    fn parser_accepts_canonical_signed_manifest() {
        let signed = manifest().canonical_signed_json().expect("signed json");
        let parsed = OtaManifest::parse(signed.as_bytes()).expect("parse manifest");
        assert_eq!(parsed.version, "0.2.0");
    }

    #[test]
    fn validation_rejects_wrong_shape() {
        let mut wrong_schema = manifest();
        wrong_schema.schema = 2;
        assert_eq!(wrong_schema.validate(), Err(ManifestError::WrongSchema));

        let mut wrong_target = manifest();
        wrong_target.target = "other".to_owned();
        assert_eq!(wrong_target.validate(), Err(ManifestError::WrongTarget));

        let mut wrong_chip = manifest();
        wrong_chip.chip = "esp32-s3".to_owned();
        assert_eq!(wrong_chip.validate(), Err(ManifestError::WrongChip));

        let mut bad_hash = manifest();
        bad_hash.sha256 = "not-a-sha".to_owned();
        assert_eq!(bad_hash.validate(), Err(ManifestError::InvalidSha256));
    }

    #[test]
    fn parser_rejects_oversized_and_malformed_manifests() {
        let oversized = vec![b' '; MAX_MANIFEST_BYTES + 1];
        assert_eq!(
            OtaManifest::parse(&oversized),
            Err(ManifestError::Oversized)
        );
        assert_eq!(OtaManifest::parse(b"{"), Err(ManifestError::Malformed));
    }

    fn sign_manifest(mut manifest: OtaManifest) -> OtaManifest {
        use ed25519_dalek::{Signer, SigningKey};

        let mut seed = [0_u8; 32];
        hex_into(
            include_str!("../../../tools/ota/keys/dev_ed25519.seed.hex").trim(),
            &mut seed,
        )
        .expect("valid dev test seed hex");
        let signing_key = SigningKey::from_bytes(&seed);
        let canonical = manifest
            .canonical_unsigned_json()
            .expect("canonical manifest");
        manifest.signature = hex_encode(&signing_key.sign(canonical.as_bytes()).to_bytes());
        manifest
    }

    fn hex_encode(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
        encoded
    }

    #[test]
    fn ed25519_verifier_accepts_dev_test_signed_manifest() {
        let signed_manifest = sign_manifest(manifest());
        let signed = signed_manifest
            .canonical_signed_json()
            .expect("signed json");

        let parsed =
            OtaManifest::parse_and_verify(signed.as_bytes(), &Ed25519ManifestVerifier::dev_test())
                .expect("verified manifest");

        assert_eq!(parsed.key_id, DEV_TEST_KEY_ID);
        assert_eq!(parsed.version, "0.2.0");
    }

    #[test]
    fn ed25519_verifier_rejects_tampered_manifest() {
        let signed_manifest = sign_manifest(manifest());
        let signed = signed_manifest
            .canonical_signed_json()
            .expect("signed json");

        let tamper_cases: &[fn(&mut OtaManifest)] = &[
            |manifest| manifest.schema = 2,
            |manifest| manifest.key_id = 42,
            |manifest| manifest.target = "other-target".to_owned(),
            |manifest| manifest.chip = "esp32-s3".to_owned(),
            |manifest| manifest.version = "0.2.1".to_owned(),
            |manifest| manifest.build += 1,
            |manifest| manifest.force = !manifest.force,
            |manifest| manifest.url = "http://192.168.1.10:8000/other.bin".to_owned(),
            |manifest| manifest.size += 1,
            |manifest| {
                manifest.sha256 =
                    "1123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned();
            },
        ];

        for tamper in tamper_cases {
            let mut tampered = OtaManifest::parse(signed.as_bytes()).expect("parse signed json");
            tamper(&mut tampered);
            let tampered_json = tampered.canonical_signed_json().expect("signed json");
            assert!(
                OtaManifest::parse_and_verify(
                    tampered_json.as_bytes(),
                    &Ed25519ManifestVerifier::dev_test()
                )
                .is_err()
            );
        }
    }

    #[test]
    fn parser_rejects_unknown_fields() {
        assert_eq!(
            OtaManifest::parse(
                br#"{"schema":1,"key_id":1001,"target":"motonet-of-things/esp32","chip":"esp32-wroom","version":"0.2.0","build":42,"force":false,"url":"http://192.168.1.10:8000/firmware.bin","size":1234567,"sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","signature":"00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000","extra":true}"#,
            ),
            Err(ManifestError::Malformed)
        );
    }

    #[test]
    fn ed25519_verifier_rejects_wrong_key_id() {
        let mut signed_manifest = sign_manifest(manifest());
        signed_manifest.key_id = 42;
        let signed = signed_manifest
            .canonical_signed_json()
            .expect("signed json");

        assert_eq!(
            OtaManifest::parse_and_verify(signed.as_bytes(), &Ed25519ManifestVerifier::dev_test()),
            Err(ManifestError::SignatureRejected)
        );
    }

    struct StubVerifier {
        expected_canonical: String,
        expected_signature: String,
    }

    impl SignatureVerifier for StubVerifier {
        fn verify(&self, key_id: u32, canonical_manifest: &[u8], signature: &str) -> bool {
            key_id == DEV_TEST_KEY_ID
                && canonical_manifest == self.expected_canonical.as_bytes()
                && signature == self.expected_signature
        }
    }

    #[test]
    fn release_verifier_does_not_trust_dev_test_key() {
        let mut signed_manifest = sign_manifest(manifest());
        signed_manifest.key_id = RELEASE_KEY_ID;
        let signed = signed_manifest
            .canonical_signed_json()
            .expect("signed json");

        assert_eq!(
            OtaManifest::parse_and_verify(
                signed.as_bytes(),
                &Ed25519ManifestVerifier::release_ota()
            ),
            Err(ManifestError::SignatureRejected)
        );
    }

    #[test]
    fn signature_verification_is_behind_interface() {
        let signed_manifest = manifest();
        let signature = signed_manifest.signature.clone();
        let verifier = StubVerifier {
            expected_canonical: signed_manifest
                .canonical_unsigned_json()
                .expect("canonical json"),
            expected_signature: signature,
        };
        let signed = signed_manifest
            .canonical_signed_json()
            .expect("signed json");
        assert!(OtaManifest::parse_and_verify(signed.as_bytes(), &verifier).is_ok());

        let mut tampered = manifest();
        tampered.build = 7;
        let signed = tampered.canonical_signed_json().expect("signed json");
        assert_eq!(
            OtaManifest::parse_and_verify(signed.as_bytes(), &verifier),
            Err(ManifestError::SignatureRejected)
        );
    }
}
