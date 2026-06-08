use app_core::config_rules::clamp_radio_config;
use app_core::rtc_schema;
use defmt::{info, warn};

use crate::messages::{DEFAULT_RADIO_SETTINGS, RadioSettings};

#[esp_hal::ram(unstable(rtc_slow, persistent))]
static mut PERSISTED_RADIO_SETTINGS_WORD: u32 = 0;

fn to_rtc_payload(settings: RadioSettings) -> rtc_schema::RadioSettingsPayload {
    rtc_schema::RadioSettingsPayload {
        detection_threshold_db: settings.detection_threshold_db,
        magn_target: settings.magn_target,
        channel_bandwidth_index: settings.channel_bandwidth_index,
        carrier_sense_threshold: settings.carrier_sense_threshold,
    }
}

fn from_rtc_payload(payload: rtc_schema::RadioSettingsPayload) -> RadioSettings {
    RadioSettings {
        detection_threshold_db: payload.detection_threshold_db,
        magn_target: payload.magn_target,
        channel_bandwidth_index: payload.channel_bandwidth_index,
        carrier_sense_threshold: payload.carrier_sense_threshold,
    }
}

fn clamp_settings(settings: RadioSettings) -> RadioSettings {
    RadioSettings::from(clamp_radio_config(settings.into()))
}

fn read_persisted_word() -> u32 {
    critical_section::with(|_| unsafe {
        core::ptr::read_volatile(core::ptr::addr_of!(PERSISTED_RADIO_SETTINGS_WORD))
    })
}

fn write_persisted_word(word: u32) {
    critical_section::with(|_| unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(PERSISTED_RADIO_SETTINGS_WORD), word);
    });
}

pub fn persist_settings(settings: RadioSettings) {
    let clamped = clamp_settings(settings);
    let word = rtc_schema::encode_radio_settings(to_rtc_payload(clamped));
    write_persisted_word(word);
    let verify = read_persisted_word();
    let bandwidth_khz =
        crate::messages::channel_bandwidth_hz(clamped.channel_bandwidth_index) / 1000;
    info!(
        "RadioSettings: wrote persisted RTC_SLOW=0x{:08x} [threshold={}dB, magn_target={}, bandwidth={}kHz, carrier_sense={}]",
        word,
        clamped.detection_threshold_db,
        clamped.magn_target,
        bandwidth_khz,
        clamped.carrier_sense_threshold
    );
    info!("RadioSettings: verify persisted RTC_SLOW=0x{:08x}", verify);
}

#[must_use]
pub fn load_settings() -> Option<RadioSettings> {
    let word = read_persisted_word();
    match rtc_schema::decode_radio_settings(word) {
        Ok(decoded) => {
            let settings = clamp_settings(from_rtc_payload(decoded.value));
            if settings != from_rtc_payload(decoded.value) {
                let migrated_word = rtc_schema::encode_radio_settings(to_rtc_payload(settings));
                write_persisted_word(migrated_word);
                warn!("RadioSettings: clamped invalid persisted settings and rewrote RTC_SLOW");
            }
            Some(settings)
        }
        Err(rtc_schema::DecodeError::ChecksumMismatch) => {
            warn!("RadioSettings: persisted RTC_SLOW checksum mismatch; using defaults");
            None
        }
        Err(_) => None,
    }
}

#[must_use]
pub fn load_settings_or_default() -> RadioSettings {
    load_settings().unwrap_or(DEFAULT_RADIO_SETTINGS)
}
