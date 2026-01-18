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
    /// Configured SNR threshold in dB (e.g., 16)
    /// This is the minimum signal-to-noise ratio required for detection.
    pub snr_threshold: u8,
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
    pub fn as_str(&self) -> &'static str {
        match self {
            SignalQuality::Excellent => "Excellent",
            SignalQuality::Good => "Good",
            SignalQuality::Fair => "Fair",
            SignalQuality::Poor => "Poor",
        }
    }
}
