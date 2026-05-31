use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use app_core::config_rules::clamp_power_config;
use app_core::domain::PowerConfigView;
use embassy_time::Instant;

use crate::messages::{
    DEFAULT_POWER_SETTINGS, POWER_SLEEP_DURATION_MAX_SECS, POWER_SLEEP_DURATION_MIN_SECS,
    POWER_UI_IDLE_TIMEOUT_MAX_SECS, POWER_UI_IDLE_TIMEOUT_MIN_SECS, PowerSettings,
};

mod persistence;
mod policy;
mod sleep;

static PREDICTIVE_SLEEP_ENABLED: AtomicBool =
    AtomicBool::new(DEFAULT_POWER_SETTINGS.predictive_sleep_enabled);
static SLEEP_DURATION_SECS: AtomicU8 = AtomicU8::new(DEFAULT_POWER_SETTINGS.sleep_duration_secs);
static UI_IDLE_TIMEOUT_SECS: AtomicU8 = AtomicU8::new(DEFAULT_POWER_SETTINGS.ui_idle_timeout_secs);
static UI_IDLE_DEADLINE_SECS: AtomicU32 = AtomicU32::new(0);

fn to_power_config_view(settings: PowerSettings) -> PowerConfigView {
    PowerConfigView {
        predictive_sleep_enabled: settings.predictive_sleep_enabled,
        sleep_duration_secs: settings.sleep_duration_secs,
        ui_idle_timeout_secs: settings.ui_idle_timeout_secs,
    }
}

fn from_power_config_view(view: PowerConfigView) -> PowerSettings {
    PowerSettings {
        predictive_sleep_enabled: view.predictive_sleep_enabled,
        sleep_duration_secs: view.sleep_duration_secs,
        ui_idle_timeout_secs: view.ui_idle_timeout_secs,
    }
}

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
    from_power_config_view(clamp_power_config(to_power_config_view(settings)))
}

#[must_use]
pub fn load_settings_or_default() -> PowerSettings {
    persistence::load_settings_or_default()
}

pub fn set_settings(settings: PowerSettings) {
    policy::set_settings(settings);
}

pub fn restore_settings_after_reset(settings: PowerSettings) {
    policy::restore_settings_after_reset(settings);
}

#[must_use]
pub fn get_settings() -> PowerSettings {
    policy::get_settings()
}

#[must_use]
pub fn predictive_sleep_enabled() -> bool {
    policy::predictive_sleep_enabled()
}

pub fn notify_ui_activity() {
    policy::notify_ui_activity();
}

#[must_use]
pub fn wake_reason_class() -> &'static str {
    sleep::wake_reason_class()
}

pub fn log_wakeup_cause() {
    sleep::log_wakeup_cause();
}

pub fn maybe_sleep_after_publish(
    queue_empty: bool,
    time_since_mesaurement_receive: core::time::Duration,
) {
    policy::maybe_sleep_after_publish(queue_empty, time_since_mesaurement_receive);
}
