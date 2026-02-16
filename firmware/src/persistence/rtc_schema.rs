#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerSettingsPayload {
    pub predictive_sleep_enabled: bool,
    pub sleep_duration_secs: u8,
    pub ui_idle_timeout_secs: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RfProfilePayload {
    pub profile_index: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedValue<T> {
    pub value: T,
    pub schema_version: u8,
    pub needs_migration: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    UnknownSchema,
    UnsupportedVersion,
    ChecksumMismatch,
}

pub const POWER_SCHEMA_VERSION: u8 = 1;
pub const LEGACY_POWER_SCHEMA_VERSION: u8 = 0;
pub const RF_PROFILE_SCHEMA_VERSION: u8 = 2;
pub const LEGACY_RF_PROFILE_SCHEMA_VERSION: u8 = 1;

const POWER_MAGIC_CURRENT: u8 = 0xD1;
const POWER_MAGIC_LEGACY: u8 = 0xA5;
const RF_PROFILE_MAGIC_CURRENT: u8 = 0xD2;
const RF_PROFILE_MAGIC_LEGACY: u8 = 0xC7;
const RF_PROFILE_VERSION_LEGACY_WORD: u8 = 1;
const CHECKSUM_SEED_CURRENT: u8 = 0x73;
const CHECKSUM_SEED_POWER_LEGACY: u8 = 0x5C;
const CHECKSUM_SEED_RF_LEGACY: u8 = 0x39;

#[must_use]
pub fn encode_power_settings(settings: PowerSettingsPayload) -> u32 {
    let byte0 = POWER_MAGIC_CURRENT;
    let byte1 =
        (u8::from(settings.predictive_sleep_enabled) << 7) | (settings.sleep_duration_secs & 0x7F);
    let byte2 = settings.ui_idle_timeout_secs;
    encode_word([byte0, byte1, byte2], CHECKSUM_SEED_CURRENT)
}

pub fn decode_power_settings(word: u32) -> Result<DecodedValue<PowerSettingsPayload>, DecodeError> {
    let [byte0, byte1, byte2, byte3] = word.to_le_bytes();

    if byte0 == POWER_MAGIC_CURRENT {
        validate_checksum([byte0, byte1, byte2], byte3, CHECKSUM_SEED_CURRENT)?;
        return Ok(DecodedValue {
            value: PowerSettingsPayload {
                predictive_sleep_enabled: (byte1 & 0x80) != 0,
                sleep_duration_secs: byte1 & 0x7F,
                ui_idle_timeout_secs: byte2,
            },
            schema_version: POWER_SCHEMA_VERSION,
            needs_migration: false,
        });
    }

    if byte0 == POWER_MAGIC_LEGACY {
        validate_checksum([byte0, byte1, byte2], byte3, CHECKSUM_SEED_POWER_LEGACY)?;
        return Ok(DecodedValue {
            value: PowerSettingsPayload {
                predictive_sleep_enabled: (byte1 & 0x80) != 0,
                sleep_duration_secs: byte1 & 0x7F,
                ui_idle_timeout_secs: byte2,
            },
            schema_version: LEGACY_POWER_SCHEMA_VERSION,
            needs_migration: true,
        });
    }

    Err(DecodeError::UnknownSchema)
}

#[must_use]
pub fn encode_rf_profile(profile: RfProfilePayload) -> u32 {
    encode_word(
        [
            RF_PROFILE_MAGIC_CURRENT,
            profile.profile_index,
            RF_PROFILE_SCHEMA_VERSION,
        ],
        CHECKSUM_SEED_CURRENT,
    )
}

pub fn decode_rf_profile(word: u32) -> Result<DecodedValue<RfProfilePayload>, DecodeError> {
    let [byte0, byte1, byte2, byte3] = word.to_le_bytes();

    if byte0 == RF_PROFILE_MAGIC_CURRENT {
        validate_checksum([byte0, byte1, byte2], byte3, CHECKSUM_SEED_CURRENT)?;
        if byte2 != RF_PROFILE_SCHEMA_VERSION {
            return Err(DecodeError::UnsupportedVersion);
        }
        return Ok(DecodedValue {
            value: RfProfilePayload {
                profile_index: byte1,
            },
            schema_version: RF_PROFILE_SCHEMA_VERSION,
            needs_migration: false,
        });
    }

    if byte0 == RF_PROFILE_MAGIC_LEGACY {
        validate_checksum([byte0, byte1, byte2], byte3, CHECKSUM_SEED_RF_LEGACY)?;
        if byte2 != RF_PROFILE_VERSION_LEGACY_WORD {
            return Err(DecodeError::UnsupportedVersion);
        }
        return Ok(DecodedValue {
            value: RfProfilePayload {
                profile_index: byte1,
            },
            schema_version: LEGACY_RF_PROFILE_SCHEMA_VERSION,
            needs_migration: true,
        });
    }

    Err(DecodeError::UnknownSchema)
}

#[must_use]
fn checksum(seed: u8, payload: [u8; 3]) -> u8 {
    payload
        .iter()
        .fold(seed, |acc, byte| acc.rotate_left(1).wrapping_add(*byte))
}

#[must_use]
fn encode_word(payload: [u8; 3], seed: u8) -> u32 {
    let checksum_byte = checksum(seed, payload);
    u32::from_le_bytes([payload[0], payload[1], payload[2], checksum_byte])
}

fn validate_checksum(payload: [u8; 3], expected_checksum: u8, seed: u8) -> Result<(), DecodeError> {
    if checksum(seed, payload) == expected_checksum {
        Ok(())
    } else {
        Err(DecodeError::ChecksumMismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DecodeError, LEGACY_POWER_SCHEMA_VERSION, LEGACY_RF_PROFILE_SCHEMA_VERSION,
        POWER_SCHEMA_VERSION, RF_PROFILE_SCHEMA_VERSION, RfProfilePayload, decode_power_settings,
        decode_rf_profile, encode_power_settings, encode_rf_profile,
    };

    #[test]
    fn roundtrip_power_and_rf_profile_with_schema_versions() {
        let power_encoded = encode_power_settings(super::PowerSettingsPayload {
            predictive_sleep_enabled: true,
            sleep_duration_secs: 45,
            ui_idle_timeout_secs: 60,
        });
        let power_decoded = decode_power_settings(power_encoded)
            .expect("power payload should decode after encoding");
        assert_eq!(power_decoded.schema_version, POWER_SCHEMA_VERSION);
        assert!(!power_decoded.needs_migration);
        assert!(power_decoded.value.predictive_sleep_enabled);
        assert_eq!(power_decoded.value.sleep_duration_secs, 45);
        assert_eq!(power_decoded.value.ui_idle_timeout_secs, 60);

        let rf_encoded = encode_rf_profile(RfProfilePayload { profile_index: 3 });
        let rf_decoded =
            decode_rf_profile(rf_encoded).expect("rf profile should decode after encoding");
        assert_eq!(rf_decoded.schema_version, RF_PROFILE_SCHEMA_VERSION);
        assert!(!rf_decoded.needs_migration);
        assert_eq!(rf_decoded.value.profile_index, 3);
    }

    #[test]
    fn checksum_mismatch_uses_fallback_behavior() {
        let power_encoded = encode_power_settings(super::PowerSettingsPayload {
            predictive_sleep_enabled: false,
            sleep_duration_secs: 30,
            ui_idle_timeout_secs: 50,
        });
        let power_corrupted = power_encoded ^ u32::from(0x55u8);
        let power_fallback = super::PowerSettingsPayload {
            predictive_sleep_enabled: true,
            sleep_duration_secs: 45,
            ui_idle_timeout_secs: 60,
        };
        let restored_power =
            decode_power_settings(power_corrupted).map_or(power_fallback, |decoded| decoded.value);
        assert_eq!(restored_power, power_fallback);

        let rf_encoded = encode_rf_profile(RfProfilePayload { profile_index: 2 });
        let rf_corrupted = rf_encoded ^ u32::from(0x55u8);
        let restored_rf = decode_rf_profile(rf_corrupted)
            .ok()
            .map(|decoded| decoded.value.profile_index);
        assert_eq!(restored_rf, None);
    }

    #[test]
    fn legacy_schema_versions_are_distinct_from_current_versions() {
        assert_ne!(POWER_SCHEMA_VERSION, LEGACY_POWER_SCHEMA_VERSION);
        assert_ne!(RF_PROFILE_SCHEMA_VERSION, LEGACY_RF_PROFILE_SCHEMA_VERSION);

        let error = DecodeError::ChecksumMismatch;
        assert_eq!(error, DecodeError::ChecksumMismatch);
    }
}
