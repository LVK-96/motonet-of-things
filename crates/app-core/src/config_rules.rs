use crate::domain::{PowerConfigView, RadioConfigView};

pub const DETECTION_THRESHOLD_MIN_DB: u8 = 4;
pub const DETECTION_THRESHOLD_MAX_DB: u8 = 16;
pub const DETECTION_THRESHOLD_STEP_DB: u8 = 4;
pub const MAGN_TARGET_MIN: u8 = 0;
pub const MAGN_TARGET_MAX: u8 = 7;
pub const CHANNEL_BANDWIDTH_MIN_INDEX: u8 = 0;
pub const CHANNEL_BANDWIDTH_MAX_INDEX: u8 = 3;
pub const CARRIER_SENSE_MIN: u8 = 0;
pub const CARRIER_SENSE_MAX: u8 = 7;
pub const DEFAULT_DETECTION_THRESHOLD_DB: u8 = DETECTION_THRESHOLD_MIN_DB;
pub const DEFAULT_MAGN_TARGET: u8 = 3;
pub const DEFAULT_CHANNEL_BANDWIDTH_INDEX: u8 = 1;
pub const DEFAULT_CHANNEL_BANDWIDTH_HZ: u32 = 203_000;
pub const DEFAULT_CARRIER_SENSE_THRESHOLD: u8 = CARRIER_SENSE_MAX;

pub const POWER_SLEEP_DURATION_MIN_SECS: u8 = 1;
pub const POWER_SLEEP_DURATION_MAX_SECS: u8 = 59;
pub const POWER_UI_IDLE_TIMEOUT_MIN_SECS: u8 = 5;
pub const POWER_UI_IDLE_TIMEOUT_MAX_SECS: u8 = 180;
pub const POWER_DEFAULT_PREDICTIVE_SLEEP_ENABLED: bool = true;
pub const POWER_DEFAULT_SLEEP_DURATION_SECS: u8 = 45;
pub const POWER_DEFAULT_UI_IDLE_TIMEOUT_SECS: u8 = 60;

#[must_use]
pub fn clamp_radio_config(settings: RadioConfigView) -> RadioConfigView {
    RadioConfigView {
        detection_threshold_db: settings
            .detection_threshold_db
            .clamp(DETECTION_THRESHOLD_MIN_DB, DETECTION_THRESHOLD_MAX_DB),
        magn_target: settings.magn_target.clamp(MAGN_TARGET_MIN, MAGN_TARGET_MAX),
        channel_bandwidth_index: settings
            .channel_bandwidth_index
            .clamp(CHANNEL_BANDWIDTH_MIN_INDEX, CHANNEL_BANDWIDTH_MAX_INDEX),
        carrier_sense_threshold: settings
            .carrier_sense_threshold
            .clamp(CARRIER_SENSE_MIN, CARRIER_SENSE_MAX),
    }
}

#[must_use]
pub fn clamp_power_config(settings: PowerConfigView) -> PowerConfigView {
    PowerConfigView {
        predictive_sleep_enabled: settings.predictive_sleep_enabled,
        sleep_duration_secs: settings
            .sleep_duration_secs
            .clamp(POWER_SLEEP_DURATION_MIN_SECS, POWER_SLEEP_DURATION_MAX_SECS),
        ui_idle_timeout_secs: settings.ui_idle_timeout_secs.clamp(
            POWER_UI_IDLE_TIMEOUT_MIN_SECS,
            POWER_UI_IDLE_TIMEOUT_MAX_SECS,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CARRIER_SENSE_MAX, CARRIER_SENSE_MIN, CHANNEL_BANDWIDTH_MAX_INDEX,
        CHANNEL_BANDWIDTH_MIN_INDEX, DEFAULT_CARRIER_SENSE_THRESHOLD,
        DEFAULT_CHANNEL_BANDWIDTH_INDEX, DEFAULT_DETECTION_THRESHOLD_DB, DEFAULT_MAGN_TARGET,
        DETECTION_THRESHOLD_MAX_DB, DETECTION_THRESHOLD_MIN_DB, DETECTION_THRESHOLD_STEP_DB,
        MAGN_TARGET_MAX, MAGN_TARGET_MIN, POWER_DEFAULT_SLEEP_DURATION_SECS,
        POWER_DEFAULT_UI_IDLE_TIMEOUT_SECS, POWER_SLEEP_DURATION_MAX_SECS,
        POWER_SLEEP_DURATION_MIN_SECS, POWER_UI_IDLE_TIMEOUT_MAX_SECS,
        POWER_UI_IDLE_TIMEOUT_MIN_SECS, clamp_power_config, clamp_radio_config,
    };
    use crate::domain::{PowerConfigView, RadioConfigView};

    #[test]
    fn clamp_radio_settings_to_allowed_ranges() {
        let clamped = clamp_radio_config(RadioConfigView {
            detection_threshold_db: 0,
            magn_target: 255,
            channel_bandwidth_index: 250,
            carrier_sense_threshold: 200,
        });

        assert_eq!(clamped.detection_threshold_db, DETECTION_THRESHOLD_MIN_DB);
        assert_eq!(clamped.magn_target, MAGN_TARGET_MAX);
        assert_eq!(clamped.channel_bandwidth_index, CHANNEL_BANDWIDTH_MAX_INDEX);
        assert_eq!(clamped.carrier_sense_threshold, CARRIER_SENSE_MAX);

        let clamped_min = clamp_radio_config(RadioConfigView {
            detection_threshold_db: 2,
            magn_target: MAGN_TARGET_MIN,
            channel_bandwidth_index: CHANNEL_BANDWIDTH_MIN_INDEX,
            carrier_sense_threshold: CARRIER_SENSE_MIN,
        });

        assert_eq!(
            clamped_min.detection_threshold_db,
            DETECTION_THRESHOLD_MIN_DB
        );
    }

    #[test]
    fn clamp_power_settings_to_allowed_ranges() {
        let clamped = clamp_power_config(PowerConfigView {
            predictive_sleep_enabled: true,
            sleep_duration_secs: 0,
            ui_idle_timeout_secs: 255,
        });

        assert_eq!(clamped.sleep_duration_secs, POWER_SLEEP_DURATION_MIN_SECS);
        assert_eq!(clamped.ui_idle_timeout_secs, POWER_UI_IDLE_TIMEOUT_MAX_SECS);

        let clamped_max = clamp_power_config(PowerConfigView {
            predictive_sleep_enabled: false,
            sleep_duration_secs: 200,
            ui_idle_timeout_secs: 1,
        });

        assert_eq!(
            clamped_max.sleep_duration_secs,
            POWER_SLEEP_DURATION_MAX_SECS
        );
        assert_eq!(
            clamped_max.ui_idle_timeout_secs,
            POWER_UI_IDLE_TIMEOUT_MIN_SECS
        );
    }

    #[test]
    fn preserves_valid_settings_values() {
        let radio = clamp_radio_config(RadioConfigView {
            detection_threshold_db: DETECTION_THRESHOLD_MAX_DB,
            magn_target: 4,
            channel_bandwidth_index: 2,
            carrier_sense_threshold: 3,
        });
        assert_eq!(radio.detection_threshold_db, DETECTION_THRESHOLD_MAX_DB);
        assert_eq!(radio.magn_target, 4);
        assert_eq!(radio.channel_bandwidth_index, 2);
        assert_eq!(radio.carrier_sense_threshold, 3);

        let power = clamp_power_config(PowerConfigView {
            predictive_sleep_enabled: false,
            sleep_duration_secs: 45,
            ui_idle_timeout_secs: 60,
        });
        assert!(!power.predictive_sleep_enabled);
        assert_eq!(power.sleep_duration_secs, 45);
        assert_eq!(power.ui_idle_timeout_secs, 60);
    }

    #[test]
    fn shared_rule_constants_cover_power_and_radio_defaults() {
        assert!(
            (DETECTION_THRESHOLD_MIN_DB..=DETECTION_THRESHOLD_MAX_DB)
                .contains(&DEFAULT_DETECTION_THRESHOLD_DB)
        );
        assert_eq!(
            DEFAULT_DETECTION_THRESHOLD_DB % DETECTION_THRESHOLD_STEP_DB,
            0
        );
        assert!((MAGN_TARGET_MIN..=MAGN_TARGET_MAX).contains(&DEFAULT_MAGN_TARGET));
        assert!(
            (CHANNEL_BANDWIDTH_MIN_INDEX..=CHANNEL_BANDWIDTH_MAX_INDEX)
                .contains(&DEFAULT_CHANNEL_BANDWIDTH_INDEX)
        );
        assert!((CARRIER_SENSE_MIN..=CARRIER_SENSE_MAX).contains(&DEFAULT_CARRIER_SENSE_THRESHOLD));
        assert!(
            (POWER_SLEEP_DURATION_MIN_SECS..=POWER_SLEEP_DURATION_MAX_SECS)
                .contains(&POWER_DEFAULT_SLEEP_DURATION_SECS)
        );
        assert!(
            (POWER_UI_IDLE_TIMEOUT_MIN_SECS..=POWER_UI_IDLE_TIMEOUT_MAX_SECS)
                .contains(&POWER_DEFAULT_UI_IDLE_TIMEOUT_SECS)
        );
    }
}
