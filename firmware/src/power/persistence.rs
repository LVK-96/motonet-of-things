use app_core::rtc_schema;
use defmt::{info, warn};

use crate::messages::{DEFAULT_POWER_SETTINGS, PowerSettings};

#[esp_hal::ram(unstable(rtc_slow, persistent))]
static mut PERSISTED_POWER_WORD: u32 = 0;

fn to_rtc_payload(settings: PowerSettings) -> rtc_schema::PowerSettingsPayload {
    rtc_schema::PowerSettingsPayload {
        predictive_sleep_enabled: settings.predictive_sleep_enabled,
        sleep_duration_secs: settings.sleep_duration_secs,
        ui_idle_timeout_secs: settings.ui_idle_timeout_secs,
    }
}

fn from_rtc_payload(payload: rtc_schema::PowerSettingsPayload) -> PowerSettings {
    PowerSettings {
        predictive_sleep_enabled: payload.predictive_sleep_enabled,
        sleep_duration_secs: payload.sleep_duration_secs,
        ui_idle_timeout_secs: payload.ui_idle_timeout_secs,
    }
}

fn settings_within_bounds(settings: PowerSettings) -> bool {
    (crate::messages::POWER_SLEEP_DURATION_MIN_SECS
        ..=crate::messages::POWER_SLEEP_DURATION_MAX_SECS)
        .contains(&settings.sleep_duration_secs)
        && (crate::messages::POWER_UI_IDLE_TIMEOUT_MIN_SECS
            ..=crate::messages::POWER_UI_IDLE_TIMEOUT_MAX_SECS)
            .contains(&settings.ui_idle_timeout_secs)
}

fn read_persisted_word() -> u32 {
    critical_section::with(|_| unsafe {
        core::ptr::read_volatile(core::ptr::addr_of!(PERSISTED_POWER_WORD))
    })
}

fn write_persisted_word(word: u32) {
    critical_section::with(|_| unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(PERSISTED_POWER_WORD), word);
    });
}

pub(super) fn persist_settings(settings: PowerSettings) {
    let word = rtc_schema::encode_power_settings(to_rtc_payload(super::clamp_settings(settings)));
    write_persisted_word(word);
    let verify = read_persisted_word();
    let raw = word.to_le_bytes();
    let verify_raw = verify.to_le_bytes();
    info!(
        "PowerSave: wrote persisted RTC_SLOW=0x{:08x} [{}, {}, {}, {}]",
        word, raw[0], raw[1], raw[2], raw[3]
    );
    info!(
        "PowerSave: verify persisted RTC_SLOW=0x{:08x} [{}, {}, {}, {}]",
        verify, verify_raw[0], verify_raw[1], verify_raw[2], verify_raw[3]
    );
}

#[must_use]
pub fn load_settings_or_default() -> PowerSettings {
    let word = read_persisted_word();
    let raw = word.to_le_bytes();
    match rtc_schema::decode_power_settings(word) {
        Ok(decoded) => {
            let settings = from_rtc_payload(decoded.value);
            if settings_within_bounds(settings) {
                if decoded.needs_migration {
                    let migrated_word = rtc_schema::encode_power_settings(decoded.value);
                    write_persisted_word(migrated_word);
                    info!(
                        "PowerSave: migrated legacy persisted schema to v{}",
                        rtc_schema::POWER_SCHEMA_VERSION
                    );
                }

                info!(
                    "PowerSave: restored settings (enabled={}, sleep={}s, ui_idle={}s)",
                    settings.predictive_sleep_enabled,
                    settings.sleep_duration_secs,
                    settings.ui_idle_timeout_secs
                );
                settings
            } else {
                info!(
                    "PowerSave: persisted RTC_SLOW=0x{:08x} [{}, {}, {}, {}]",
                    word, raw[0], raw[1], raw[2], raw[3]
                );
                info!("PowerSave: persisted settings out of range, using defaults");
                DEFAULT_POWER_SETTINGS
            }
        }
        Err(decode_error) => {
            info!(
                "PowerSave: persisted RTC_SLOW=0x{:08x} [{}, {}, {}, {}]",
                word, raw[0], raw[1], raw[2], raw[3]
            );
            match decode_error {
                rtc_schema::DecodeError::ChecksumMismatch => {
                    warn!("PowerSave: persisted checksum mismatch, using defaults");
                }
                _ => {
                    info!("PowerSave: no valid persisted settings, using defaults");
                }
            }
            DEFAULT_POWER_SETTINGS
        }
    }
}
