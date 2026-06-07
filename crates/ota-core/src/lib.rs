#![cfg_attr(not(test), no_std)]

//! Platform-independent OTA policy, state, manifest validation, and canonical JSON construction.
//!
//! Hardware-specific adapters (flash writes, boot metadata, reboot, networking)
//! live outside this crate.

extern crate alloc;

use alloc::{format, string::String};
use core::fmt::Write as _;
use core::sync::atomic::{AtomicU8, Ordering};

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
pub const MQTT_TOPIC_MAX_LEN: usize = 96;
pub const OTA_CONFIRMATION_DELAY_SECS: u32 = 30;
pub const SHA256_HEX_LEN: usize = 64;
pub const ED25519_SIGNATURE_HEX_LEN: usize = 128;
pub const DEV_TEST_KEY_ID: u32 = 1001;
pub const RELEASE_KEY_ID: u32 = 1;
pub const DEV_TEST_PUBLIC_KEY_HEX: &str =
    "8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c";

static OTA_STATE: AtomicU8 = AtomicU8::new(OtaState::Inactive.as_u8());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OtaState {
    Inactive = 0,
    Downloading = 1,
    Applying = 2,
    PendingConfirmation = 3,
}

impl OtaState {
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl From<u8> for OtaState {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Downloading,
            2 => Self::Applying,
            3 => Self::PendingConfirmation,
            _ => Self::Inactive,
        }
    }
}

#[must_use]
pub fn ota_state() -> OtaState {
    OTA_STATE.load(Ordering::Relaxed).into()
}

pub fn set_ota_state(state: OtaState) {
    OTA_STATE.store(state.as_u8(), Ordering::Relaxed);
}

#[must_use]
pub fn ota_confirmation_pending() -> bool {
    ota_state() == OtaState::PendingConfirmation
}

#[must_use]
pub fn ota_sleep_blocked() -> bool {
    ota_state() != OtaState::Inactive
}

#[must_use]
pub fn ota_update_in_progress() -> bool {
    matches!(ota_state(), OtaState::Downloading | OtaState::Applying)
}

pub fn arm_rollback_test_pending_confirmation() {
    set_ota_state(OtaState::PendingConfirmation);
}

#[must_use]
pub struct OtaUpdateGuard;

impl OtaUpdateGuard {
    pub fn begin_download() -> Self {
        set_ota_state(OtaState::Downloading);
        Self
    }

    pub fn begin_apply() -> Self {
        set_ota_state(OtaState::Applying);
        Self
    }
}

impl Drop for OtaUpdateGuard {
    fn drop(&mut self) {
        set_ota_state(OtaState::Inactive);
    }
}

#[must_use]
pub struct PendingConfirmationGuard;

impl PendingConfirmationGuard {
    pub fn begin() -> Self {
        set_ota_state(OtaState::PendingConfirmation);
        Self
    }

    pub fn confirm(self) {
        set_ota_state(OtaState::Inactive);
        core::mem::forget(self);
    }
}

impl Drop for PendingConfirmationGuard {
    fn drop(&mut self) {
        set_ota_state(OtaState::PendingConfirmation);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OtaConfirmationGate {
    required_uptime_secs: u32,
    wifi_connected: bool,
    mqtt_connected: bool,
    heartbeat_published: bool,
}

impl OtaConfirmationGate {
    #[must_use]
    pub const fn new(required_uptime_secs: u32) -> Self {
        Self {
            required_uptime_secs,
            wifi_connected: false,
            mqtt_connected: false,
            heartbeat_published: false,
        }
    }

    pub fn note_wifi_connected(&mut self) {
        self.wifi_connected = true;
    }

    pub fn note_wifi_disconnected(&mut self) {
        self.wifi_connected = false;
        self.mqtt_connected = false;
        self.heartbeat_published = false;
    }

    pub fn note_mqtt_connected(&mut self) {
        self.mqtt_connected = true;
    }

    pub fn note_mqtt_disconnected(&mut self) {
        self.mqtt_connected = false;
        self.heartbeat_published = false;
    }

    pub fn note_heartbeat_published(&mut self) {
        self.heartbeat_published = true;
    }

    #[must_use]
    pub const fn required_uptime_secs(&self) -> u32 {
        self.required_uptime_secs
    }

    #[must_use]
    pub const fn ready_to_confirm(&self, uptime_secs: u32) -> bool {
        self.wifi_connected
            && self.mqtt_connected
            && self.heartbeat_published
            && uptime_secs >= self.required_uptime_secs
    }
}

impl Default for OtaConfirmationGate {
    fn default() -> Self {
        Self::new(OTA_CONFIRMATION_DELAY_SECS)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicError {
    DeviceIdTooLong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaUrlPolicyError {
    InvalidScheme,
    UserInfoNotAllowed,
    HostMustBeIpv4,
    MissingPort,
    EmptyPath,
    QueryNotAllowed,
    FragmentNotAllowed,
}

/// Validate the firmware OTA download URL policy.
///
/// Only local, explicit-port HTTP URLs are accepted:
/// `http://<ipv4>:<port>/<path>`.
///
/// # Errors
///
/// Returns [`OtaUrlPolicyError`] when the URL is outside the firmware's
/// intentionally narrow OTA download policy.
pub fn validate_ota_url_policy(url: &str) -> Result<(), OtaUrlPolicyError> {
    let Some(rest) = url.strip_prefix("http://") else {
        return Err(OtaUrlPolicyError::InvalidScheme);
    };
    if rest.contains('#') {
        return Err(OtaUrlPolicyError::FragmentNotAllowed);
    }
    if rest.contains('?') {
        return Err(OtaUrlPolicyError::QueryNotAllowed);
    }

    let Some((authority, path)) = rest.split_once('/') else {
        return Err(OtaUrlPolicyError::EmptyPath);
    };
    if authority.contains('@') {
        return Err(OtaUrlPolicyError::UserInfoNotAllowed);
    }
    if path.is_empty() {
        return Err(OtaUrlPolicyError::EmptyPath);
    }
    let Some((host, port)) = authority.rsplit_once(':') else {
        return Err(OtaUrlPolicyError::MissingPort);
    };
    if !valid_port(port) {
        return Err(OtaUrlPolicyError::MissingPort);
    }
    validate_ipv4_host(host)
}

fn valid_port(port: &str) -> bool {
    if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let mut value: u32 = 0;
    for byte in port.bytes() {
        value = value * 10 + u32::from(byte - b'0');
        if value > 65_535 {
            return false;
        }
    }
    value != 0
}

fn validate_ipv4_host(host: &str) -> Result<(), OtaUrlPolicyError> {
    if host.is_empty() || host.split('.').count() != 4 {
        return Err(OtaUrlPolicyError::HostMustBeIpv4);
    }
    for octet in host.split('.') {
        if octet.is_empty() || octet.len() > 3 || !octet.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(OtaUrlPolicyError::HostMustBeIpv4);
        }
        let mut value: u16 = 0;
        for byte in octet.bytes() {
            value = value * 10 + u16::from(byte - b'0');
        }
        if value > 255 {
            return Err(OtaUrlPolicyError::HostMustBeIpv4);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EspImagePrefixError {
    TooShort,
    WrongMagic,
    InvalidSegmentCount,
    InvalidFlashMode,
    InvalidFlashSizeFrequency,
    ZeroEntryAddress,
}

/// Validate a retained ESP image prefix for conservative OTA plausibility.
///
/// The function accepts any byte slice but only inspects the basic ESP image
/// header fields present in the first 8 bytes.
///
/// # Errors
///
/// Returns [`EspImagePrefixError`] when a required header field is implausible.
pub fn validate_esp_image_prefix(prefix: &[u8]) -> Result<(), EspImagePrefixError> {
    if prefix.len() < 8 {
        return Err(EspImagePrefixError::TooShort);
    }
    if prefix[0] != 0xe9 {
        return Err(EspImagePrefixError::WrongMagic);
    }
    if !(1..=16).contains(&prefix[1]) {
        return Err(EspImagePrefixError::InvalidSegmentCount);
    }
    if !matches!(prefix[2], 0x00..=0x03) {
        return Err(EspImagePrefixError::InvalidFlashMode);
    }
    let frequency = prefix[3] & 0x0f;
    let size = prefix[3] >> 4;
    if !matches!(frequency, 0x0 | 0x1 | 0x2 | 0xf) || !matches!(size, 0x0..=0x8) {
        return Err(EspImagePrefixError::InvalidFlashSizeFrequency);
    }
    let entry = u32::from_le_bytes([prefix[4], prefix[5], prefix[6], prefix[7]]);
    if entry == 0 {
        return Err(EspImagePrefixError::ZeroEntryAddress);
    }
    Ok(())
}

/// Build the retained online/offline status topic for a device.
///
/// # Errors
///
/// Returns [`TopicError::DeviceIdTooLong`] if the device id does not fit the
/// fixed MQTT topic buffer.
pub fn status_topic(device_id: &str) -> Result<heapless::String<MQTT_TOPIC_MAX_LEN>, TopicError> {
    let mut topic = heapless::String::new();
    write!(topic, "motonet/{device_id}/status").map_err(|_| TopicError::DeviceIdTooLong)?;
    Ok(topic)
}

/// Build the signed OTA manifest command topic for a device.
///
/// # Errors
///
/// Returns [`TopicError::DeviceIdTooLong`] if the device id does not fit the
/// fixed MQTT topic buffer.
pub fn ota_command_topic(
    device_id: &str,
) -> Result<heapless::String<MQTT_TOPIC_MAX_LEN>, TopicError> {
    let mut topic = heapless::String::new();
    write!(topic, "motonet/{device_id}/cmd/ota").map_err(|_| TopicError::DeviceIdTooLong)?;
    Ok(topic)
}

/// Build the OTA status topic a device publishes post-reboot confirmation
/// results to.
///
/// # Errors
///
/// Returns [`TopicError::DeviceIdTooLong`] if the device id does not fit the
/// fixed MQTT topic buffer.
pub fn ota_status_topic(
    device_id: &str,
) -> Result<heapless::String<MQTT_TOPIC_MAX_LEN>, TopicError> {
    let mut topic = heapless::String::new();
    write!(topic, "motonet/{device_id}/ota/status").map_err(|_| TopicError::DeviceIdTooLong)?;
    Ok(topic)
}

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
        canonical_unsigned_json(self)
    }

    /// Return canonical signed JSON including the `signature` field.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError::Malformed`] if canonical JSON construction fails.
    pub fn canonical_signed_json(&self) -> Result<String, ManifestError> {
        canonical_signed_json(self)
    }
}

fn validate_len(field: &'static str, value: &str, max: usize) -> Result<(), ManifestError> {
    if value.len() > max {
        Err(ManifestError::FieldTooLong(field))
    } else {
        Ok(())
    }
}

fn json_string(value: &str) -> Result<String, ManifestError> {
    serde_json::to_string(value).map_err(|_| ManifestError::Malformed)
}

fn canonical_unsigned_json(manifest: &OtaManifest) -> Result<String, ManifestError> {
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

fn canonical_signed_json(manifest: &OtaManifest) -> Result<String, ManifestError> {
    Ok(format!(
        "{{\"schema\":{},\"key_id\":{},\"target\":{},\"chip\":{},\"version\":{},\"build\":{},\"force\":{},\"url\":{},\"size\":{},\"sha256\":{},\"signature\":{}}}",
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
        json_string(&manifest.signature)?
    ))
}

// ── Runtime coordination helpers ──────────────────────────────────────────

/// MQTT-side action for an incoming OTA manifest command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OtaManifestDeliveryAction {
    /// Forward a live/non-retained command to the OTA task.
    ForwardOnly,
    /// Forward a retained command once, then clear the broker's retained copy.
    ForwardAndClearRetained,
    /// Clear an obsolete retained command without re-running OTA.
    ClearRetainedOnly,
}

/// Decide how MQTT should handle an incoming OTA manifest command.
///
/// During post-reboot confirmation, a retained command is the manifest that
/// triggered the just-booted image. Forwarding it would start OTA again before
/// the image can confirm, so clear it without handoff.
#[must_use]
pub const fn classify_ota_manifest_delivery(
    state: OtaState,
    retained: bool,
) -> OtaManifestDeliveryAction {
    match (state, retained) {
        (OtaState::PendingConfirmation, true) => OtaManifestDeliveryAction::ClearRetainedOnly,
        (_, true) => OtaManifestDeliveryAction::ForwardAndClearRetained,
        (_, false) => OtaManifestDeliveryAction::ForwardOnly,
    }
}

/// Returns `true` when MQTT is permitted to connect and publish.
#[must_use]
pub const fn is_mqtt_allowed(state: OtaState) -> bool {
    matches!(state, OtaState::Inactive | OtaState::PendingConfirmation)
}

/// Returns `true` when radio capture is permitted.
#[must_use]
pub const fn is_radio_capture_allowed(state: OtaState) -> bool {
    matches!(state, OtaState::Inactive)
}

/// Returns `true` when UI input handling is permitted.
#[must_use]
pub const fn is_ui_input_allowed(state: OtaState) -> bool {
    matches!(state, OtaState::Inactive)
}

/// Returns an optional status message for the display during OTA operations.
#[must_use]
pub const fn display_message(state: OtaState) -> Option<&'static str> {
    match state {
        OtaState::Downloading => Some("OTA download..."),
        OtaState::Applying => Some("OTA applying..."),
        _ => None,
    }
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
    fn ota_topics_are_derived_from_device_id() {
        assert_eq!(
            status_topic("test-sensor").map(|topic| topic.to_string()),
            Ok("motonet/test-sensor/status".to_owned())
        );
        assert_eq!(
            ota_command_topic("test-sensor").map(|topic| topic.to_string()),
            Ok("motonet/test-sensor/cmd/ota".to_owned())
        );
        assert_eq!(
            ota_status_topic("test-sensor").map(|topic| topic.to_string()),
            Ok("motonet/test-sensor/ota/status".to_owned())
        );
    }

    #[test]
    fn ota_state_distinguishes_update_from_pending_confirmation() {
        set_ota_state(OtaState::Inactive);
        assert!(!ota_sleep_blocked());
        assert!(!ota_update_in_progress());

        set_ota_state(OtaState::Downloading);
        assert!(ota_sleep_blocked());
        assert!(ota_update_in_progress());

        set_ota_state(OtaState::PendingConfirmation);
        assert!(ota_sleep_blocked());
        assert!(ota_confirmation_pending());
        assert!(!ota_update_in_progress());

        set_ota_state(OtaState::Inactive);
    }

    #[test]
    fn confirmation_requires_all_health_signals_and_delay() {
        let mut gate = OtaConfirmationGate::default();
        assert!(!gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));

        gate.note_wifi_connected();
        assert!(!gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));

        gate.note_mqtt_connected();
        assert!(!gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));

        gate.note_heartbeat_published();
        assert!(!gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS - 1));
        assert!(gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));
    }

    #[test]
    fn lost_connections_clear_dependent_health() {
        let mut gate = OtaConfirmationGate::default();
        gate.note_wifi_connected();
        gate.note_mqtt_connected();
        gate.note_heartbeat_published();
        assert!(gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));

        gate.note_mqtt_disconnected();
        gate.note_mqtt_connected();
        assert!(!gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));
        gate.note_heartbeat_published();
        assert!(gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));

        gate.note_wifi_disconnected();
        gate.note_wifi_connected();
        assert!(!gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));
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
    fn ota_url_policy_accepts_strict_local_http_ipv4_with_port_and_path() {
        assert_eq!(
            validate_ota_url_policy("http://192.168.1.10:8000/firmware.bin"),
            Ok(())
        );
    }

    #[test]
    fn ota_url_policy_rejects_https_scheme() {
        assert_eq!(
            validate_ota_url_policy("https://192.168.1.10:8000/firmware.bin"),
            Err(OtaUrlPolicyError::InvalidScheme)
        );
    }

    #[test]
    fn ota_url_policy_rejects_hostnames() {
        assert_eq!(
            validate_ota_url_policy("http://ota.local:8000/firmware.bin"),
            Err(OtaUrlPolicyError::HostMustBeIpv4)
        );
    }

    #[test]
    fn ota_url_policy_rejects_missing_port() {
        assert_eq!(
            validate_ota_url_policy("http://192.168.1.10/firmware.bin"),
            Err(OtaUrlPolicyError::MissingPort)
        );
    }

    #[test]
    fn ota_url_policy_rejects_invalid_port_numbers() {
        assert_eq!(
            validate_ota_url_policy("http://192.168.1.10:0/firmware.bin"),
            Err(OtaUrlPolicyError::MissingPort)
        );
        assert_eq!(
            validate_ota_url_policy("http://192.168.1.10:65536/firmware.bin"),
            Err(OtaUrlPolicyError::MissingPort)
        );
        assert_eq!(
            validate_ota_url_policy("http://192.168.1.10:999999999999/firmware.bin"),
            Err(OtaUrlPolicyError::MissingPort)
        );
    }

    #[test]
    fn ota_url_policy_accepts_max_valid_port() {
        assert_eq!(
            validate_ota_url_policy("http://192.168.1.10:65535/firmware.bin"),
            Ok(())
        );
    }

    #[test]
    fn ota_url_policy_rejects_empty_path() {
        assert_eq!(
            validate_ota_url_policy("http://192.168.1.10:8000"),
            Err(OtaUrlPolicyError::EmptyPath)
        );
        assert_eq!(
            validate_ota_url_policy("http://192.168.1.10:8000/"),
            Err(OtaUrlPolicyError::EmptyPath)
        );
    }

    #[test]
    fn ota_url_policy_rejects_query() {
        assert_eq!(
            validate_ota_url_policy("http://192.168.1.10:8000/firmware.bin?x=1"),
            Err(OtaUrlPolicyError::QueryNotAllowed)
        );
    }

    #[test]
    fn ota_url_policy_rejects_fragment() {
        assert_eq!(
            validate_ota_url_policy("http://192.168.1.10:8000/firmware.bin#frag"),
            Err(OtaUrlPolicyError::FragmentNotAllowed)
        );
    }

    #[test]
    fn ota_url_policy_rejects_userinfo() {
        assert_eq!(
            validate_ota_url_policy("http://user@192.168.1.10:8000/firmware.bin"),
            Err(OtaUrlPolicyError::UserInfoNotAllowed)
        );
    }

    #[test]
    fn ota_url_policy_allows_at_sign_in_path() {
        assert_eq!(
            validate_ota_url_policy("http://192.168.1.10:8000/releases/build@2.bin"),
            Ok(())
        );
    }

    fn valid_esp_prefix() -> [u8; 64] {
        let mut prefix = [0_u8; 64];
        prefix[0] = 0xe9;
        prefix[1] = 4;
        prefix[2] = 0x02;
        prefix[3] = 0x1f;
        prefix[4..8].copy_from_slice(&0x4008_0000_u32.to_le_bytes());
        prefix
    }

    #[test]
    fn esp_image_prefix_accepts_plausible_esp_app_image_header() {
        let prefix = valid_esp_prefix();
        assert_eq!(validate_esp_image_prefix(&prefix), Ok(()));
    }

    #[test]
    fn esp_image_prefix_rejects_too_short_prefix() {
        assert_eq!(
            validate_esp_image_prefix(&valid_esp_prefix()[..7]),
            Err(EspImagePrefixError::TooShort)
        );
    }

    #[test]
    fn esp_image_prefix_rejects_wrong_magic() {
        let mut prefix = valid_esp_prefix();
        prefix[0] = 0xea;
        assert_eq!(
            validate_esp_image_prefix(&prefix),
            Err(EspImagePrefixError::WrongMagic)
        );
    }

    #[test]
    fn esp_image_prefix_rejects_invalid_segment_count() {
        let mut prefix = valid_esp_prefix();
        prefix[1] = 0;
        assert_eq!(
            validate_esp_image_prefix(&prefix),
            Err(EspImagePrefixError::InvalidSegmentCount)
        );

        let mut prefix = valid_esp_prefix();
        prefix[1] = 17;
        assert_eq!(
            validate_esp_image_prefix(&prefix),
            Err(EspImagePrefixError::InvalidSegmentCount)
        );
    }

    #[test]
    fn esp_image_prefix_rejects_invalid_flash_mode() {
        let mut prefix = valid_esp_prefix();
        prefix[2] = 0xff;
        assert_eq!(
            validate_esp_image_prefix(&prefix),
            Err(EspImagePrefixError::InvalidFlashMode)
        );
    }

    #[test]
    fn esp_image_prefix_rejects_invalid_flash_size_frequency() {
        let mut prefix = valid_esp_prefix();
        prefix[3] = 0xff;
        assert_eq!(
            validate_esp_image_prefix(&prefix),
            Err(EspImagePrefixError::InvalidFlashSizeFrequency)
        );
    }

    #[test]
    fn esp_image_prefix_accepts_known_large_flash_size_nibble() {
        let mut prefix = valid_esp_prefix();
        prefix[3] = 0x70;
        assert_eq!(validate_esp_image_prefix(&prefix), Ok(()));
    }

    #[test]
    fn esp_image_prefix_rejects_zero_entry_address() {
        let mut prefix = valid_esp_prefix();
        prefix[4..8].copy_from_slice(&0_u32.to_le_bytes());
        assert_eq!(
            validate_esp_image_prefix(&prefix),
            Err(EspImagePrefixError::ZeroEntryAddress)
        );
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

    // ── Runtime coordination helpers ───────────────────────────────────

    #[test]
    fn inactive_state_allows_regular_runtime_work() {
        assert!(is_mqtt_allowed(OtaState::Inactive));
        assert!(is_radio_capture_allowed(OtaState::Inactive));
        assert!(is_ui_input_allowed(OtaState::Inactive));
        assert_eq!(display_message(OtaState::Inactive), None);
    }

    #[test]
    fn downloading_state_quiets_runtime_work_and_shows_status() {
        assert!(!is_mqtt_allowed(OtaState::Downloading));
        assert!(!is_radio_capture_allowed(OtaState::Downloading));
        assert!(!is_ui_input_allowed(OtaState::Downloading));
        assert_eq!(
            display_message(OtaState::Downloading),
            Some("OTA download...")
        );
    }

    #[test]
    fn applying_state_quiets_runtime_work_and_shows_status() {
        assert!(!is_mqtt_allowed(OtaState::Applying));
        assert!(!is_radio_capture_allowed(OtaState::Applying));
        assert!(!is_ui_input_allowed(OtaState::Applying));
        assert_eq!(display_message(OtaState::Applying), Some("OTA applying..."));
    }

    #[test]
    fn pending_confirmation_quiets_runtime_work_with_no_display_message() {
        assert!(is_mqtt_allowed(OtaState::PendingConfirmation));
        assert!(!is_radio_capture_allowed(OtaState::PendingConfirmation));
        assert!(!is_ui_input_allowed(OtaState::PendingConfirmation));
        assert_eq!(display_message(OtaState::PendingConfirmation), None);
    }

    #[test]
    fn retained_manifest_during_pending_confirmation_is_cleared_without_forwarding() {
        assert_eq!(
            classify_ota_manifest_delivery(OtaState::PendingConfirmation, true),
            OtaManifestDeliveryAction::ClearRetainedOnly
        );
    }

    #[test]
    fn retained_manifest_while_inactive_is_forwarded_once_and_cleared() {
        assert_eq!(
            classify_ota_manifest_delivery(OtaState::Inactive, true),
            OtaManifestDeliveryAction::ForwardAndClearRetained
        );
    }

    #[test]
    fn live_manifest_is_forwarded_without_retained_clear() {
        assert_eq!(
            classify_ota_manifest_delivery(OtaState::Inactive, false),
            OtaManifestDeliveryAction::ForwardOnly
        );
    }

    #[test]
    fn live_manifest_during_active_phases_is_forwarded_without_retained_clear() {
        // A live (non-retained) command during Downloading or Applying is
        // forwarded; we don't try to clear a retained copy that doesn't
        // exist.
        assert_eq!(
            classify_ota_manifest_delivery(OtaState::Downloading, false),
            OtaManifestDeliveryAction::ForwardOnly
        );
        assert_eq!(
            classify_ota_manifest_delivery(OtaState::Applying, false),
            OtaManifestDeliveryAction::ForwardOnly
        );
    }

    #[test]
    fn retained_manifest_during_active_phases_is_forwarded_once_and_cleared() {
        // A retained command during Downloading or Applying is forwarded
        // (the channel is depth-1, so the OTA task will only see the
        // latest) and the retained copy is cleared to avoid re-delivery
        // on reconnect.
        assert_eq!(
            classify_ota_manifest_delivery(OtaState::Downloading, true),
            OtaManifestDeliveryAction::ForwardAndClearRetained
        );
        assert_eq!(
            classify_ota_manifest_delivery(OtaState::Applying, true),
            OtaManifestDeliveryAction::ForwardAndClearRetained
        );
    }
}
