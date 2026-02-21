use core::time::Duration;

use crate::config_rules::{POWER_SLEEP_DURATION_MIN_SECS, predictive_sleep_window_cap_secs};

pub const PREDICTIVE_SLEEP_MAX_MEASUREMENT_AGE_SECS: u64 = 20;
pub const MQTT_MIN_BACKOFF_SECS: u64 = 1;
pub const MQTT_MAX_BACKOFF_SECS: u64 = 60;
pub const MQTT_RECONNECT_BEFORE_PUBLISH_IDLE_SECS: u64 = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PredictiveSleepDecision {
    QueueNotEmpty,
    MeasurementTooOld,
    PredictiveSleepDisabled,
    UiIdleDeadlineNotElapsed { idle_remaining_secs: u32 },
    Sleep { sleep_secs: u8 },
}

#[must_use]
pub fn predictive_sleep_decision(
    queue_empty: bool,
    time_since_measurement_receive: Duration,
    predictive_sleep_enabled: bool,
    configured_sleep_secs: u8,
    now_secs: u32,
    ui_idle_deadline_secs: u32,
) -> PredictiveSleepDecision {
    if !queue_empty {
        return PredictiveSleepDecision::QueueNotEmpty;
    }

    if time_since_measurement_receive
        > Duration::from_secs(PREDICTIVE_SLEEP_MAX_MEASUREMENT_AGE_SECS)
    {
        return PredictiveSleepDecision::MeasurementTooOld;
    }

    if !predictive_sleep_enabled {
        return PredictiveSleepDecision::PredictiveSleepDisabled;
    }

    if now_secs < ui_idle_deadline_secs {
        return PredictiveSleepDecision::UiIdleDeadlineNotElapsed {
            idle_remaining_secs: ui_idle_deadline_secs - now_secs,
        };
    }

    let max_sleep_secs = predictive_sleep_window_cap_secs(time_since_measurement_receive);
    let sleep_secs = configured_sleep_secs.clamp(POWER_SLEEP_DURATION_MIN_SECS, max_sleep_secs);
    PredictiveSleepDecision::Sleep { sleep_secs }
}

#[must_use]
pub fn next_ui_idle_deadline_secs(
    predictive_sleep_enabled: bool,
    rearm_idle_countdown: bool,
    now_secs: u32,
    current_deadline_secs: u32,
    ui_idle_timeout_secs: u8,
) -> u32 {
    if !predictive_sleep_enabled {
        return 0;
    }

    let max_deadline = now_secs.saturating_add(u32::from(ui_idle_timeout_secs));
    if rearm_idle_countdown {
        max_deadline
    } else {
        current_deadline_secs.min(max_deadline)
    }
}

#[must_use]
pub fn should_reconnect_before_publish(idle_secs: u64) -> bool {
    idle_secs > MQTT_RECONNECT_BEFORE_PUBLISH_IDLE_SECS
}

#[must_use]
pub fn next_mqtt_backoff_secs(current_backoff_secs: u64) -> u64 {
    current_backoff_secs
        .saturating_mul(2)
        .min(MQTT_MAX_BACKOFF_SECS)
}

#[cfg(test)]
mod tests {
    use super::{
        MQTT_MAX_BACKOFF_SECS, MQTT_RECONNECT_BEFORE_PUBLISH_IDLE_SECS, PredictiveSleepDecision,
        next_mqtt_backoff_secs, next_ui_idle_deadline_secs, predictive_sleep_decision,
        should_reconnect_before_publish,
    };
    use core::time::Duration;

    #[test]
    fn predictive_sleep_gate_blocks_when_queue_has_pending_readings() {
        let decision = predictive_sleep_decision(true, Duration::from_secs(0), true, 45, 100, 100);
        assert_eq!(decision, PredictiveSleepDecision::Sleep { sleep_secs: 45 });

        let blocked = predictive_sleep_decision(false, Duration::from_secs(0), true, 45, 100, 100);
        assert_eq!(blocked, PredictiveSleepDecision::QueueNotEmpty);
    }

    #[test]
    fn predictive_sleep_gate_blocks_when_measurement_is_too_old() {
        let decision = predictive_sleep_decision(true, Duration::from_secs(21), true, 45, 100, 100);
        assert_eq!(decision, PredictiveSleepDecision::MeasurementTooOld);
    }

    #[test]
    fn predictive_sleep_gate_allows_measurement_age_boundary() {
        let decision = predictive_sleep_decision(true, Duration::from_secs(20), true, 45, 100, 100);
        assert_eq!(decision, PredictiveSleepDecision::Sleep { sleep_secs: 30 });
    }

    #[test]
    fn predictive_sleep_gate_blocks_when_feature_is_disabled() {
        let decision = predictive_sleep_decision(true, Duration::from_secs(0), false, 45, 100, 100);
        assert_eq!(decision, PredictiveSleepDecision::PredictiveSleepDisabled);
    }

    #[test]
    fn predictive_sleep_gate_blocks_until_ui_idle_deadline_elapsed() {
        let decision = predictive_sleep_decision(true, Duration::from_secs(0), true, 45, 100, 101);
        assert_eq!(
            decision,
            PredictiveSleepDecision::UiIdleDeadlineNotElapsed {
                idle_remaining_secs: 1
            }
        );
    }

    #[test]
    fn predictive_sleep_gate_uses_valid_sleep_path() {
        let decision = predictive_sleep_decision(true, Duration::from_secs(0), true, 45, 101, 100);
        assert_eq!(decision, PredictiveSleepDecision::Sleep { sleep_secs: 45 });
    }

    #[test]
    fn restore_idle_deadline_preserves_shorter_existing_deadline() {
        let deadline = next_ui_idle_deadline_secs(true, false, 1_000, 1_020, 60);
        assert_eq!(deadline, 1_020);
    }

    #[test]
    fn restore_idle_deadline_caps_existing_deadline_to_new_timeout_window() {
        let deadline = next_ui_idle_deadline_secs(true, false, 1_000, 1_090, 60);
        assert_eq!(deadline, 1_060);
    }

    #[test]
    fn runtime_settings_update_rearms_idle_deadline() {
        let deadline = next_ui_idle_deadline_secs(true, true, 1_000, 0, 60);
        assert_eq!(deadline, 1_060);
    }

    #[test]
    fn predictive_sleep_disable_clears_idle_deadline() {
        let deadline = next_ui_idle_deadline_secs(false, false, 1_000, 1_020, 60);
        assert_eq!(deadline, 0);
    }

    #[test]
    fn reconnect_before_publish_triggers_only_after_idle_threshold() {
        assert!(!should_reconnect_before_publish(
            MQTT_RECONNECT_BEFORE_PUBLISH_IDLE_SECS
        ));
        assert!(should_reconnect_before_publish(
            MQTT_RECONNECT_BEFORE_PUBLISH_IDLE_SECS + 1
        ));
    }

    #[test]
    fn mqtt_backoff_doubles_and_caps_at_maximum() {
        assert_eq!(next_mqtt_backoff_secs(1), 2);
        assert_eq!(next_mqtt_backoff_secs(2), 4);
        assert_eq!(next_mqtt_backoff_secs(16), 32);
        assert_eq!(next_mqtt_backoff_secs(32), MQTT_MAX_BACKOFF_SECS);
        assert_eq!(
            next_mqtt_backoff_secs(MQTT_MAX_BACKOFF_SECS),
            MQTT_MAX_BACKOFF_SECS
        );
    }
}
