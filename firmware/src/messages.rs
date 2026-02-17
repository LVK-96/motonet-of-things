//! Message types for inter-task communication.
//!
//! This module defines the data structures used to pass information
//! between tasks, particularly radio readings with signal metadata.

use embassy_time::Instant;
use rubicson::RubicsonReading;
use telemetry_core::TelemetryRecord;

pub use app_core::config_rules::{
    CARRIER_SENSE_MAX, CARRIER_SENSE_MIN, CHANNEL_BANDWIDTH_MAX_INDEX, CHANNEL_BANDWIDTH_MIN_INDEX,
    DEFAULT_CARRIER_SENSE_THRESHOLD, DEFAULT_CHANNEL_BANDWIDTH_HZ, DEFAULT_CHANNEL_BANDWIDTH_INDEX,
    DEFAULT_DETECTION_THRESHOLD_DB, DEFAULT_MAGN_TARGET, DETECTION_THRESHOLD_MAX_DB,
    DETECTION_THRESHOLD_MIN_DB, DETECTION_THRESHOLD_STEP_DB, MAGN_TARGET_MAX, MAGN_TARGET_MIN,
    POWER_DEFAULT_PREDICTIVE_SLEEP_ENABLED, POWER_DEFAULT_SLEEP_DURATION_SECS,
    POWER_DEFAULT_UI_IDLE_TIMEOUT_SECS, POWER_SLEEP_DURATION_MAX_SECS,
    POWER_SLEEP_DURATION_MIN_SECS, POWER_UI_IDLE_TIMEOUT_MAX_SECS, POWER_UI_IDLE_TIMEOUT_MIN_SECS,
};

/// A sensor reading bundled with radio signal metadata.
///
/// This struct wraps a decoded `RubicsonReading` with additional
/// information about the radio reception conditions.
#[derive(Debug, Clone, Copy)]
pub struct RadioReading {
    /// The decoded sensor data (temperature, ID, channel, battery, CRC)
    pub inner: RubicsonReading,
    /// Received Signal Strength Indicator in dBm (e.g., -75)
    pub rssi: i16,
    /// Configured detection threshold in dB (e.g., 16)
    /// This is the minimum signal-to-noise ratio required for detection.
    pub detection_threshold: u8,
    /// The time when the reading was received relative to system start
    pub received_at: Instant,
}

impl RadioReading {
    #[must_use]
    pub fn to_telemetry_record(self) -> TelemetryRecord {
        let scaled_temperature = self.inner.temperature_c * 10.0;
        let rounded_temperature = if scaled_temperature >= 0.0 {
            scaled_temperature + 0.5
        } else {
            scaled_temperature - 0.5
        };

        let temperature_deci_c = if rounded_temperature > f32::from(i16::MAX) {
            i16::MAX
        } else if rounded_temperature < f32::from(i16::MIN) {
            i16::MIN
        } else {
            rounded_temperature as i16
        };

        TelemetryRecord {
            sensor_id: self.inner.id,
            channel: self.inner.channel,
            temperature_deci_c,
            battery_ok: self.inner.battery_ok,
        }
    }
}

/// Radio settings that can be changed at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub struct RadioSettings {
    /// Detection threshold in dB (valid values: 4, 8, 12, 16)
    pub detection_threshold_db: u8,
    /// AGC target amplitude level (0-7, corresponding to 24-42 dB)
    pub magn_target: u8,
    /// CC1101 channel bandwidth option index (0-3).
    pub channel_bandwidth_index: u8,
    /// Carrier sense absolute threshold (0-7, relative to `MAGN_TARGET`)
    /// 0 = at `MAGN_TARGET`, 1 = +1 dB above, ..., 7 = +7 dB above
    pub carrier_sense_threshold: u8,
}

pub const DEFAULT_RADIO_SETTINGS: RadioSettings = RadioSettings {
    detection_threshold_db: DEFAULT_DETECTION_THRESHOLD_DB,
    magn_target: DEFAULT_MAGN_TARGET, // 42 dB - matches CC1101 default
    channel_bandwidth_index: DEFAULT_CHANNEL_BANDWIDTH_INDEX,
    carrier_sense_threshold: DEFAULT_CARRIER_SENSE_THRESHOLD, // At MAGN_TARGET level
};

impl Default for RadioSettings {
    fn default() -> Self {
        DEFAULT_RADIO_SETTINGS
    }
}

/// Power policy settings that can be changed at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub struct PowerSettings {
    /// Enable predictive deep sleep between expected sensor transmissions.
    pub predictive_sleep_enabled: bool,
    /// Deep-sleep duration used once idle countdown has elapsed.
    pub sleep_duration_secs: u8,
    /// UI idle timeout before predictive sleep is allowed.
    pub ui_idle_timeout_secs: u8,
}

pub const DEFAULT_POWER_SETTINGS: PowerSettings = PowerSettings {
    predictive_sleep_enabled: POWER_DEFAULT_PREDICTIVE_SLEEP_ENABLED,
    sleep_duration_secs: POWER_DEFAULT_SLEEP_DURATION_SECS,
    ui_idle_timeout_secs: POWER_DEFAULT_UI_IDLE_TIMEOUT_SECS,
};

impl Default for PowerSettings {
    fn default() -> Self {
        DEFAULT_POWER_SETTINGS
    }
}

#[must_use]
pub const fn channel_bandwidth_hz(index: u8) -> u32 {
    match index {
        0 => 325_000,
        1 => 203_000,
        2 => 162_000,
        3 => 135_000,
        _ => DEFAULT_CHANNEL_BANDWIDTH_HZ,
    }
}

#[must_use]
pub const fn channel_bandwidth_index(bandwidth_hz: u32) -> u8 {
    match bandwidth_hz {
        325_000 => 0,
        203_000 => 1,
        162_000 => 2,
        135_000 => 3,
        _ => DEFAULT_CHANNEL_BANDWIDTH_INDEX,
    }
}

/// Signal quality classification based on RSSI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalQuality {
    /// Excellent signal (> -60 dBm)
    Excellent,
    /// Good signal (-60 to -75 dBm)
    Good,
    /// Fair signal (-75 to -90 dBm)
    Fair,
    /// Poor signal (< -90 dBm)
    Poor,
}

impl SignalQuality {
    /// Classify signal quality based on RSSI value.
    #[must_use]
    pub fn from_rssi(rssi: i16) -> Self {
        if rssi > -60 {
            SignalQuality::Excellent
        } else if rssi > -75 {
            SignalQuality::Good
        } else if rssi > -90 {
            SignalQuality::Fair
        } else {
            SignalQuality::Poor
        }
    }

    /// Get a human-readable label for the signal quality.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            SignalQuality::Excellent => "Excellent",
            SignalQuality::Good => "Good",
            SignalQuality::Fair => "Fair",
            SignalQuality::Poor => "Poor",
        }
    }
}
