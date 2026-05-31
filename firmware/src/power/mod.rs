use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use app_core::config_rules::clamp_power_config;
use embassy_time::Instant;

use crate::messages::{
    DEFAULT_POWER_SETTINGS, POWER_SLEEP_DURATION_MAX_SECS, POWER_SLEEP_DURATION_MIN_SECS,
    POWER_UI_IDLE_TIMEOUT_MAX_SECS, POWER_UI_IDLE_TIMEOUT_MIN_SECS, PowerSettings,
};

mod persistence;
mod policy;
mod sleep;

pub use persistence::load_settings_or_default;
pub use policy::{
    get_settings, maybe_sleep_after_publish, notify_ui_activity, predictive_sleep_enabled,
    restore_settings_after_reset, set_settings,
};
pub use sleep::{log_wakeup_cause, wake_reason_class};

static PREDICTIVE_SLEEP_ENABLED: AtomicBool =
    AtomicBool::new(DEFAULT_POWER_SETTINGS.predictive_sleep_enabled);
static SLEEP_DURATION_SECS: AtomicU8 = AtomicU8::new(DEFAULT_POWER_SETTINGS.sleep_duration_secs);
static UI_IDLE_TIMEOUT_SECS: AtomicU8 = AtomicU8::new(DEFAULT_POWER_SETTINGS.ui_idle_timeout_secs);
static UI_IDLE_DEADLINE_SECS: AtomicU32 = AtomicU32::new(0);

fn ui_idle_timeout_secs() -> u8 {
    UI_IDLE_TIMEOUT_SECS.load(Ordering::Relaxed).clamp(
        POWER_UI_IDLE_TIMEOUT_MIN_SECS,
        POWER_UI_IDLE_TIMEOUT_MAX_SECS,
    )
}

fn sleep_duration_secs() -> u8 {
    SLEEP_DURATION_SECS
        .load(Ordering::Relaxed)
        .clamp(POWER_SLEEP_DURATION_MIN_SECS, POWER_SLEEP_DURATION_MAX_SECS)
}

fn now_secs() -> u32 {
    u32::try_from(Instant::now().as_secs()).map_or(u32::MAX, |secs| secs)
}

fn rearm_ui_idle_deadline() {
    let now = now_secs();
    let deadline = now.saturating_add(u32::from(ui_idle_timeout_secs()));
    UI_IDLE_DEADLINE_SECS.store(deadline, Ordering::Relaxed);
}

fn clamp_settings(settings: PowerSettings) -> PowerSettings {
    PowerSettings::from(clamp_power_config(settings.into()))
}
