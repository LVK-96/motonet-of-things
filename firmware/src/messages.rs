//! Message types for inter-task communication.
//!
//! This module defines the data structures used to pass information
//! between tasks, particularly radio readings with signal metadata.

use rubicson::RubicsonReading;

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
}

/// Radio settings that can be changed at runtime.
#[derive(Debug, Clone, Copy, defmt::Format)]
pub struct RadioSettings {
    /// Detection threshold in dB (valid values: 4, 8, 12, 16)
    pub detection_threshold_db: u8,
    /// AGC target amplitude level (0-7, corresponding to 24-42 dB)
    pub magn_target: u8,
    /// CC1101 channel bandwidth option index (0-3).
    pub channel_bandwidth_index: u8,
    /// Carrier sense absolute threshold (0-7, relative to MAGN_TARGET)
    /// 0 = at MAGN_TARGET, 1 = +1 dB above, ..., 7 = +7 dB above
    pub carrier_sense_threshold: u8,
}

pub const DEFAULT_RADIO_SETTINGS: RadioSettings = RadioSettings {
    detection_threshold_db: 16,
    magn_target: 7, // 42 dB - matches CC1101 default
    channel_bandwidth_index: DEFAULT_CHANNEL_BANDWIDTH_INDEX,
    carrier_sense_threshold: 0, // At MAGN_TARGET level
};

pub const DETECTION_THRESHOLD_MIN_DB: u8 = 4;
pub const DETECTION_THRESHOLD_MAX_DB: u8 = 16;
pub const DETECTION_THRESHOLD_STEP_DB: u8 = 4;
pub const MAGN_TARGET_MIN: u8 = 0;
pub const MAGN_TARGET_MAX: u8 = 7;
pub const CHANNEL_BANDWIDTH_MIN_INDEX: u8 = 0;
pub const CHANNEL_BANDWIDTH_MAX_INDEX: u8 = 3;
pub const DEFAULT_CHANNEL_BANDWIDTH_INDEX: u8 = 1; // 203 kHz
pub const CARRIER_SENSE_MIN: u8 = 0;
pub const CARRIER_SENSE_MAX: u8 = 7;

impl Default for RadioSettings {
    fn default() -> Self {
        DEFAULT_RADIO_SETTINGS
    }
}

#[must_use]
pub const fn channel_bandwidth_hz(index: u8) -> u32 {
    match index {
        0 => 325_000,
        1 => 203_000,
        2 => 162_000,
        3 => 135_000,
        _ => 203_000,
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
