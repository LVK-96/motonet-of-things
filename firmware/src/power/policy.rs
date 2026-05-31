use core::sync::atomic::Ordering;
use core::time::Duration;

use app_core::runtime_policy::{
    PredictiveSleepDecision, next_ui_idle_deadline_secs, predictive_sleep_decision,
};
use defmt::info;

use crate::messages::PowerSettings;

fn apply_settings(settings: PowerSettings, persist: bool, rearm_idle_countdown: bool) {
    let clamped = super::clamp_settings(settings);
    super::PREDICTIVE_SLEEP_ENABLED.store(clamped.predictive_sleep_enabled, Ordering::Relaxed);
    super::SLEEP_DURATION_SECS.store(clamped.sleep_duration_secs, Ordering::Relaxed);
    super::UI_IDLE_TIMEOUT_SECS.store(clamped.ui_idle_timeout_secs, Ordering::Relaxed);
    if persist {
        super::persistence::persist_settings(clamped);
    }

    let now = super::now_secs();
    let current_deadline = super::UI_IDLE_DEADLINE_SECS.load(Ordering::Relaxed);
    let next_deadline = next_ui_idle_deadline_secs(
        clamped.predictive_sleep_enabled,
        rearm_idle_countdown,
        now,
        current_deadline,
        clamped.ui_idle_timeout_secs,
    );
    super::UI_IDLE_DEADLINE_SECS.store(next_deadline, Ordering::Relaxed);

    info!(
        "PowerSave: settings applied (enabled={}, sleep={}s, ui_idle={}s)",
        clamped.predictive_sleep_enabled, clamped.sleep_duration_secs, clamped.ui_idle_timeout_secs
    );
}

pub(super) fn set_settings(settings: PowerSettings) {
    apply_settings(settings, true, true);
}

pub(super) fn restore_settings_after_reset(settings: PowerSettings) {
    apply_settings(settings, false, false);
}

pub(super) fn get_settings() -> PowerSettings {
    PowerSettings {
        predictive_sleep_enabled: super::PREDICTIVE_SLEEP_ENABLED.load(Ordering::Relaxed),
        sleep_duration_secs: super::sleep_duration_secs(),
        ui_idle_timeout_secs: super::ui_idle_timeout_secs(),
    }
}

pub(super) fn predictive_sleep_enabled() -> bool {
    super::PREDICTIVE_SLEEP_ENABLED.load(Ordering::Relaxed)
}

pub(super) fn notify_ui_activity() {
    if predictive_sleep_enabled() {
        super::rearm_ui_idle_deadline();
    }
}

pub(super) fn maybe_sleep_after_publish(
    queue_empty: bool,
    time_since_mesaurement_receive: Duration,
) {
    let settings = get_settings();
    let now = super::now_secs();
    let idle_deadline = super::UI_IDLE_DEADLINE_SECS.load(Ordering::Relaxed);
    let decision = predictive_sleep_decision(
        queue_empty,
        time_since_mesaurement_receive,
        settings.predictive_sleep_enabled,
        settings.sleep_duration_secs,
        now,
        crate::ota::ota_update_in_progress(),
        idle_deadline,
    );

    match decision {
        PredictiveSleepDecision::QueueNotEmpty => {
            info!("PowerSave: skip deep sleep (telemetry queue still has buffered readings)");
        }
        PredictiveSleepDecision::MeasurementTooOld => {
            info!(
                "PowerSave: skip deep sleep (Next measurement in less than {}s)",
                60 - time_since_mesaurement_receive.as_secs()
            );
        }
        PredictiveSleepDecision::PredictiveSleepDisabled => {
            info!("PowerSave: skip deep sleep (predictive sleep disabled)");
        }
        PredictiveSleepDecision::UiIdleDeadlineNotElapsed {
            idle_remaining_secs,
        } => {
            info!(
                "PowerSave: skip deep sleep (UI idle deadline {}s remaining)",
                idle_remaining_secs
            );
        }
        PredictiveSleepDecision::OTAUpdateInProgress => {
            info!("PowerSave: skip deep sleep (OTA update in progress)");
        }
        PredictiveSleepDecision::Sleep { sleep_secs } => {
            info!(
                "PowerSave: entering deep sleep for {}s (timer + button wake)",
                sleep_secs
            );
            super::sleep::enter_deep_sleep(sleep_secs);
        }
    }
}
