use app_core::display_model::{DisplayFrameInput, FrameKey, derive_frame};
use app_core::domain::{PowerConfigView, RadioConfigView, SensorReading, UiScreenState};

use crate::messages::{PowerSettings, RadioReading, RadioSettings};
use crate::tasks::display::state::{DisplayState, RadioState};

#[must_use]
pub(crate) fn to_ui_screen_state(state: DisplayState) -> UiScreenState {
    match state {
        DisplayState::Main => UiScreenState::Main,
        DisplayState::Radio(RadioState::Overview) => UiScreenState::RadioOverview,
        DisplayState::Radio(RadioState::Settings) => UiScreenState::RadioSettings,
    }
}

#[must_use]
pub(crate) fn to_sensor_reading(reading: RadioReading) -> SensorReading {
    SensorReading {
        sensor_id: reading.inner.id,
        channel: reading.inner.channel,
        battery_ok: reading.inner.battery_ok,
        temperature_c: reading.inner.temperature_c,
        rssi_dbm: reading.rssi,
        detection_threshold_db: reading.detection_threshold,
    }
}

#[must_use]
pub(crate) fn to_radio_config_view(settings: RadioSettings) -> RadioConfigView {
    RadioConfigView {
        detection_threshold_db: settings.detection_threshold_db,
        magn_target: settings.magn_target,
        channel_bandwidth_index: settings.channel_bandwidth_index,
        carrier_sense_threshold: settings.carrier_sense_threshold,
    }
}

#[must_use]
pub(crate) fn to_power_config_view(settings: PowerSettings) -> PowerConfigView {
    PowerConfigView {
        predictive_sleep_enabled: settings.predictive_sleep_enabled,
        sleep_duration_secs: settings.sleep_duration_secs,
        ui_idle_timeout_secs: settings.ui_idle_timeout_secs,
    }
}

#[must_use]
pub(crate) fn from_radio_config_view(config: RadioConfigView) -> RadioSettings {
    RadioSettings {
        detection_threshold_db: config.detection_threshold_db,
        magn_target: config.magn_target,
        channel_bandwidth_index: config.channel_bandwidth_index,
        carrier_sense_threshold: config.carrier_sense_threshold,
    }
}

#[must_use]
pub(crate) fn from_power_config_view(config: PowerConfigView) -> PowerSettings {
    PowerSettings {
        predictive_sleep_enabled: config.predictive_sleep_enabled,
        sleep_duration_secs: config.sleep_duration_secs,
        ui_idle_timeout_secs: config.ui_idle_timeout_secs,
    }
}

#[must_use]
pub(crate) fn derive_frame_key(
    state: DisplayState,
    reading: Option<RadioReading>,
    radio: RadioSettings,
    power: PowerSettings,
    time_secs: Option<u64>,
    settings_nav_index: usize,
    settings_editing: bool,
) -> FrameKey {
    derive_frame(DisplayFrameInput {
        screen: to_ui_screen_state(state),
        reading: reading.map(to_sensor_reading),
        radio: to_radio_config_view(radio),
        power: to_power_config_view(power),
        time_secs,
        settings_nav_index: u8::try_from(settings_nav_index).map_or(u8::MAX, |v| v),
        settings_editing,
    })
}

#[cfg(test)]
mod tests {
    use app_core::display_model::{FrameKey, MainFrameKey, RadioFrameKey, SettingsFrameKey};
    use rubicson::RubicsonReading;

    use super::derive_frame_key;
    use crate::messages::{DEFAULT_POWER_SETTINGS, DEFAULT_RADIO_SETTINGS, RadioReading};
    use crate::tasks::display::state::{DisplayState, RadioState};

    fn sample_reading() -> RadioReading {
        RadioReading {
            inner: RubicsonReading {
                id: 0x2A,
                channel: 3,
                battery_ok: true,
                temperature_c: 21.4,
                crc_ok: true,
            },
            rssi: -70,
            detection_threshold: 12,
        }
    }

    #[test]
    fn frame_key_bridge_calls_derive_for_main_screen() {
        let frame = derive_frame_key(
            DisplayState::Main,
            Some(sample_reading()),
            DEFAULT_RADIO_SETTINGS,
            DEFAULT_POWER_SETTINGS,
            Some(1_710_000_000),
            0,
            false,
        );

        assert_eq!(
            frame,
            FrameKey::Main(MainFrameKey {
                temp_deci: 214,
                sensor_id: 0x2A,
                channel: 3,
                battery_ok: true,
                time_secs: Some(1_710_000_000),
            })
        );
    }

    #[test]
    fn frame_key_bridge_passes_settings_state_and_clamps_nav_index() {
        let frame = derive_frame_key(
            DisplayState::Radio(RadioState::Settings),
            Some(sample_reading()),
            DEFAULT_RADIO_SETTINGS,
            DEFAULT_POWER_SETTINGS,
            None,
            usize::MAX,
            true,
        );

        assert_eq!(
            frame,
            FrameKey::Radio(RadioFrameKey::Settings(SettingsFrameKey {
                nav_index: u8::MAX,
                editing: true,
                threshold: DEFAULT_RADIO_SETTINGS.detection_threshold_db,
                magn: DEFAULT_RADIO_SETTINGS.magn_target,
                bandwidth_index: DEFAULT_RADIO_SETTINGS.channel_bandwidth_index,
                carrier_sense: DEFAULT_RADIO_SETTINGS.carrier_sense_threshold,
                predictive_sleep_enabled: DEFAULT_POWER_SETTINGS.predictive_sleep_enabled,
                sleep_duration_secs: DEFAULT_POWER_SETTINGS.sleep_duration_secs,
                ui_idle_timeout_secs: DEFAULT_POWER_SETTINGS.ui_idle_timeout_secs,
            }))
        );
    }
}
