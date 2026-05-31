use app_core::domain::{PowerConfigView, RadioConfigView, SensorReading};

use crate::messages::{PowerSettings, RadioReading, RadioSettings};

impl From<RadioReading> for SensorReading {
    fn from(reading: RadioReading) -> Self {
        Self {
            sensor_id: reading.inner.id,
            channel: reading.inner.channel,
            battery_ok: reading.inner.battery_ok,
            temperature_c: reading.inner.temperature_c,
            rssi_dbm: reading.rssi,
            detection_threshold_db: reading.detection_threshold,
        }
    }
}

impl From<RadioSettings> for RadioConfigView {
    fn from(settings: RadioSettings) -> Self {
        Self {
            detection_threshold_db: settings.detection_threshold_db,
            magn_target: settings.magn_target,
            channel_bandwidth_index: settings.channel_bandwidth_index,
            carrier_sense_threshold: settings.carrier_sense_threshold,
        }
    }
}

impl From<RadioConfigView> for RadioSettings {
    fn from(config: RadioConfigView) -> Self {
        Self {
            detection_threshold_db: config.detection_threshold_db,
            magn_target: config.magn_target,
            channel_bandwidth_index: config.channel_bandwidth_index,
            carrier_sense_threshold: config.carrier_sense_threshold,
        }
    }
}

impl From<PowerSettings> for PowerConfigView {
    fn from(settings: PowerSettings) -> Self {
        Self {
            predictive_sleep_enabled: settings.predictive_sleep_enabled,
            sleep_duration_secs: settings.sleep_duration_secs,
            ui_idle_timeout_secs: settings.ui_idle_timeout_secs,
        }
    }
}

impl From<PowerConfigView> for PowerSettings {
    fn from(config: PowerConfigView) -> Self {
        Self {
            predictive_sleep_enabled: config.predictive_sleep_enabled,
            sleep_duration_secs: config.sleep_duration_secs,
            ui_idle_timeout_secs: config.ui_idle_timeout_secs,
        }
    }
}
