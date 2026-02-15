use crate::domain::{PowerConfigView, RadioConfigView, SensorReading, UiScreenState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKey {
    Waiting,
    Main(MainFrameKey),
    Radio(RadioFrameKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MainFrameKey {
    pub temp_deci: i16,
    pub sensor_id: u8,
    pub channel: u8,
    pub battery_ok: bool,
    pub time_secs: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RadioFrameKey {
    Overview {
        rssi: Option<i16>,
        detection_threshold: u8,
    },
    Settings(SettingsFrameKey),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsFrameKey {
    pub nav_index: u8,
    pub editing: bool,
    pub threshold: u8,
    pub magn: u8,
    pub bandwidth_index: u8,
    pub carrier_sense: u8,
    pub predictive_sleep_enabled: bool,
    pub sleep_duration_secs: u8,
    pub ui_idle_timeout_secs: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct DisplayFrameInput {
    pub screen: UiScreenState,
    pub reading: Option<SensorReading>,
    pub radio: RadioConfigView,
    pub power: PowerConfigView,
    pub time_secs: Option<u64>,
    pub settings_nav_index: u8,
    pub settings_editing: bool,
}

#[must_use]
pub fn derive_frame(input: DisplayFrameInput) -> FrameKey {
    match input.screen {
        UiScreenState::Waiting => FrameKey::Waiting,
        UiScreenState::Main => input.reading.map_or(FrameKey::Waiting, |reading| {
            FrameKey::Main(MainFrameKey {
                temp_deci: temp_to_deci(reading.temperature_c),
                sensor_id: reading.sensor_id,
                channel: reading.channel,
                battery_ok: reading.battery_ok,
                time_secs: input.time_secs,
            })
        }),
        UiScreenState::RadioOverview => FrameKey::Radio(RadioFrameKey::Overview {
            rssi: input.reading.map(|r| r.rssi_dbm),
            detection_threshold: input.reading.map_or(input.radio.detection_threshold_db, |r| {
                r.detection_threshold_db
            }),
        }),
        UiScreenState::RadioSettings => FrameKey::Radio(RadioFrameKey::Settings(SettingsFrameKey {
            nav_index: input.settings_nav_index,
            editing: input.settings_editing,
            threshold: input.radio.detection_threshold_db,
            magn: input.radio.magn_target,
            bandwidth_index: input.radio.channel_bandwidth_index,
            carrier_sense: input.radio.carrier_sense_threshold,
            predictive_sleep_enabled: input.power.predictive_sleep_enabled,
            sleep_duration_secs: input.power.sleep_duration_secs,
            ui_idle_timeout_secs: input.power.ui_idle_timeout_secs,
        })),
    }
}

fn temp_to_deci(temp_c: f32) -> i16 {
    #[allow(clippy::cast_possible_truncation)]
    {
        (temp_c * 10.0) as i16
    }
}

#[cfg(test)]
mod tests {
    use super::{DisplayFrameInput, FrameKey, MainFrameKey, RadioFrameKey, SettingsFrameKey, derive_frame};
    use crate::domain::{PowerConfigView, RadioConfigView, SensorReading, UiScreenState};

    fn sample_radio() -> RadioConfigView {
        RadioConfigView {
            detection_threshold_db: 16,
            magn_target: 7,
            channel_bandwidth_index: 1,
            carrier_sense_threshold: 0,
        }
    }

    fn sample_power() -> PowerConfigView {
        PowerConfigView {
            predictive_sleep_enabled: true,
            sleep_duration_secs: 45,
            ui_idle_timeout_secs: 60,
        }
    }

    fn sample_reading() -> SensorReading {
        SensorReading {
            sensor_id: 12,
            channel: 1,
            battery_ok: true,
            temperature_c: -2.3,
            rssi_dbm: -73,
            detection_threshold_db: 12,
        }
    }

    #[test]
    fn derive_frame_waiting_without_reading() {
        let frame = derive_frame(DisplayFrameInput {
            screen: UiScreenState::Main,
            reading: None,
            radio: sample_radio(),
            power: sample_power(),
            time_secs: None,
            settings_nav_index: 0,
            settings_editing: false,
        });

        assert_eq!(frame, FrameKey::Waiting);
    }

    #[test]
    fn derive_frame_main_from_reading() {
        let frame = derive_frame(DisplayFrameInput {
            screen: UiScreenState::Main,
            reading: Some(sample_reading()),
            radio: sample_radio(),
            power: sample_power(),
            time_secs: Some(1_700_000_000),
            settings_nav_index: 0,
            settings_editing: false,
        });

        assert_eq!(
            frame,
            FrameKey::Main(MainFrameKey {
                temp_deci: -23,
                sensor_id: 12,
                channel: 1,
                battery_ok: true,
                time_secs: Some(1_700_000_000),
            })
        );
    }

    #[test]
    fn derive_frame_radio_overview_from_reading_and_settings() {
        let frame = derive_frame(DisplayFrameInput {
            screen: UiScreenState::RadioOverview,
            reading: Some(sample_reading()),
            radio: sample_radio(),
            power: sample_power(),
            time_secs: None,
            settings_nav_index: 0,
            settings_editing: false,
        });

        assert_eq!(
            frame,
            FrameKey::Radio(RadioFrameKey::Overview {
                rssi: Some(-73),
                detection_threshold: 12,
            })
        );
    }

    #[test]
    fn derive_frame_radio_settings_uses_pending_values() {
        let frame = derive_frame(DisplayFrameInput {
            screen: UiScreenState::RadioSettings,
            reading: Some(sample_reading()),
            radio: sample_radio(),
            power: sample_power(),
            time_secs: None,
            settings_nav_index: 3,
            settings_editing: true,
        });

        assert_eq!(
            frame,
            FrameKey::Radio(RadioFrameKey::Settings(SettingsFrameKey {
                nav_index: 3,
                editing: true,
                threshold: 16,
                magn: 7,
                bandwidth_index: 1,
                carrier_sense: 0,
                predictive_sleep_enabled: true,
                sleep_duration_secs: 45,
                ui_idle_timeout_secs: 60,
            }))
        );
    }
}
