use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use core::time::Duration;

use app_core::config_rules::clamp_power_config;
use app_core::domain::PowerConfigView;
use defmt::{Debug2Format, info, warn};
use embassy_time::Instant;
use esp_hal::rtc_cntl::{
    Rtc,
    sleep::{Ext0WakeupSource, RtcSleepConfig, TimerWakeupSource, WakeupLevel},
    wakeup_cause,
};
use esp_hal::system::SleepSource;

#[path = "persistence/mod.rs"]
pub(crate) mod persistence;

use crate::messages::{
    DEFAULT_POWER_SETTINGS, POWER_SLEEP_DURATION_MAX_SECS, POWER_SLEEP_DURATION_MIN_SECS,
    POWER_UI_IDLE_TIMEOUT_MAX_SECS, POWER_UI_IDLE_TIMEOUT_MIN_SECS, PowerSettings,
};
use crate::telemetry;
use persistence::rtc_schema;

static PREDICTIVE_SLEEP_ENABLED: AtomicBool =
    AtomicBool::new(DEFAULT_POWER_SETTINGS.predictive_sleep_enabled);
static SLEEP_DURATION_SECS: AtomicU8 = AtomicU8::new(DEFAULT_POWER_SETTINGS.sleep_duration_secs);
static UI_IDLE_TIMEOUT_SECS: AtomicU8 = AtomicU8::new(DEFAULT_POWER_SETTINGS.ui_idle_timeout_secs);
static UI_IDLE_DEADLINE_SECS: AtomicU32 = AtomicU32::new(0);

const DEEP_SLEEP_LOG_FLUSH_DELAY_US: u32 = 20_000;
#[esp_hal::ram(unstable(rtc_slow, persistent))]
static mut PERSISTED_POWER_WORD: u32 = 0;

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

fn to_rtc_payload(settings: PowerSettings) -> rtc_schema::PowerSettingsPayload {
    rtc_schema::PowerSettingsPayload {
        predictive_sleep_enabled: settings.predictive_sleep_enabled,
        sleep_duration_secs: settings.sleep_duration_secs,
        ui_idle_timeout_secs: settings.ui_idle_timeout_secs,
    }
}

fn from_rtc_payload(payload: rtc_schema::PowerSettingsPayload) -> PowerSettings {
    PowerSettings {
        predictive_sleep_enabled: payload.predictive_sleep_enabled,
        sleep_duration_secs: payload.sleep_duration_secs,
        ui_idle_timeout_secs: payload.ui_idle_timeout_secs,
    }
}

fn settings_within_bounds(settings: PowerSettings) -> bool {
    (POWER_SLEEP_DURATION_MIN_SECS..=POWER_SLEEP_DURATION_MAX_SECS)
        .contains(&settings.sleep_duration_secs)
        && (POWER_UI_IDLE_TIMEOUT_MIN_SECS..=POWER_UI_IDLE_TIMEOUT_MAX_SECS)
            .contains(&settings.ui_idle_timeout_secs)
}

fn read_persisted_word() -> u32 {
    critical_section::with(|_| unsafe {
        core::ptr::read_volatile(core::ptr::addr_of!(PERSISTED_POWER_WORD))
    })
}

fn write_persisted_word(word: u32) {
    critical_section::with(|_| unsafe {
        core::ptr::write_volatile(core::ptr::addr_of_mut!(PERSISTED_POWER_WORD), word);
    });
}

fn persist_settings(settings: PowerSettings) {
    let word = rtc_schema::encode_power_settings(to_rtc_payload(clamp_settings(settings)));
    write_persisted_word(word);
    let verify = read_persisted_word();
    let raw = word.to_le_bytes();
    let verify_raw = verify.to_le_bytes();
    info!(
        "PowerSave: wrote persisted RTC_SLOW=0x{:08x} [{}, {}, {}, {}]",
        word, raw[0], raw[1], raw[2], raw[3]
    );
    info!(
        "PowerSave: verify persisted RTC_SLOW=0x{:08x} [{}, {}, {}, {}]",
        verify, verify_raw[0], verify_raw[1], verify_raw[2], verify_raw[3]
    );
}

#[must_use]
pub fn load_settings_or_default() -> PowerSettings {
    let word = read_persisted_word();
    let raw = word.to_le_bytes();
    match rtc_schema::decode_power_settings(word) {
        Ok(decoded) => {
            let settings = from_rtc_payload(decoded.value);
            if !settings_within_bounds(settings) {
                info!(
                    "PowerSave: persisted RTC_SLOW=0x{:08x} [{}, {}, {}, {}]",
                    word, raw[0], raw[1], raw[2], raw[3]
                );
                info!("PowerSave: persisted settings out of range, using defaults");
                DEFAULT_POWER_SETTINGS
            } else {
                if decoded.needs_migration {
                    let migrated_word = rtc_schema::encode_power_settings(decoded.value);
                    write_persisted_word(migrated_word);
                    info!(
                        "PowerSave: migrated legacy persisted schema to v{}",
                        rtc_schema::POWER_SCHEMA_VERSION
                    );
                }

                info!(
                    "PowerSave: restored settings (enabled={}, sleep={}s, ui_idle={}s)",
                    settings.predictive_sleep_enabled,
                    settings.sleep_duration_secs,
                    settings.ui_idle_timeout_secs
                );
                settings
            }
        }
        Err(decode_error) => {
            info!(
                "PowerSave: persisted RTC_SLOW=0x{:08x} [{}, {}, {}, {}]",
                word, raw[0], raw[1], raw[2], raw[3]
            );
            match decode_error {
                rtc_schema::DecodeError::ChecksumMismatch => {
                    warn!("PowerSave: persisted checksum mismatch, using defaults");
                }
                _ => {
                    info!("PowerSave: no valid persisted settings, using defaults");
                }
            }
            DEFAULT_POWER_SETTINGS
        }
    }
}

fn apply_settings(settings: PowerSettings, persist: bool, rearm_idle_countdown: bool) {
    let clamped = clamp_settings(settings);
    PREDICTIVE_SLEEP_ENABLED.store(clamped.predictive_sleep_enabled, Ordering::Relaxed);
    SLEEP_DURATION_SECS.store(clamped.sleep_duration_secs, Ordering::Relaxed);
    UI_IDLE_TIMEOUT_SECS.store(clamped.ui_idle_timeout_secs, Ordering::Relaxed);
    if persist {
        persist_settings(clamped);
    }

    // Deadline handling differs between runtime settings updates and boot-time restore.
    if clamped.predictive_sleep_enabled {
        if rearm_idle_countdown {
            rearm_ui_idle_deadline();
        } else {
            let now = now_secs();
            let preserved_deadline = UI_IDLE_DEADLINE_SECS.load(Ordering::Relaxed);
            let max_deadline = now.saturating_add(u32::from(ui_idle_timeout_secs()));
            UI_IDLE_DEADLINE_SECS.store(preserved_deadline.min(max_deadline), Ordering::Relaxed);
        }
    } else {
        UI_IDLE_DEADLINE_SECS.store(0, Ordering::Relaxed);
    }

    info!(
        "PowerSave: settings applied (enabled={}, sleep={}s, ui_idle={}s)",
        clamped.predictive_sleep_enabled, clamped.sleep_duration_secs, clamped.ui_idle_timeout_secs
    );
}

pub fn set_settings(settings: PowerSettings) {
    apply_settings(settings, true, true);
}

pub fn restore_settings_after_reset(settings: PowerSettings) {
    apply_settings(settings, false, false);
}

pub fn get_settings() -> PowerSettings {
    PowerSettings {
        predictive_sleep_enabled: PREDICTIVE_SLEEP_ENABLED.load(Ordering::Relaxed),
        sleep_duration_secs: sleep_duration_secs(),
        ui_idle_timeout_secs: ui_idle_timeout_secs(),
    }
}

pub fn predictive_sleep_enabled() -> bool {
    PREDICTIVE_SLEEP_ENABLED.load(Ordering::Relaxed)
}

pub fn notify_ui_activity() {
    if predictive_sleep_enabled() {
        rearm_ui_idle_deadline();
    }
}

#[must_use]
pub fn wake_reason_class() -> &'static str {
    match wakeup_cause() {
        SleepSource::Undefined => "cold_boot",
        SleepSource::Timer => "timer_wake",
        SleepSource::Ext0
        | SleepSource::Ext1
        | SleepSource::Gpio
        | SleepSource::Uart
        | SleepSource::TouchPad => "ui_wake",
        _ => "other",
    }
}

pub fn log_wakeup_cause() {
    let cause = wakeup_cause();
    let class = wake_reason_class();
    info!(
        "Reset reason: {}",
        Debug2Format(&esp_hal::system::reset_reason())
    );
    info!("Wake cause: {:?} ({})", cause, class);
    if matches!(cause, SleepSource::Undefined) {
        info!("PowerSave: cold boot (not waking from deep sleep)");
    } else {
        info!("PowerSave: exited deep sleep (wake class: {})", class);
    }
}

pub fn maybe_sleep_after_publish(queue_empty: bool, has_pending_retry: bool) {
    if !telemetry::predictive_sleep_pipeline_safe(queue_empty, has_pending_retry) {
        if !queue_empty {
            info!("PowerSave: skip deep sleep (telemetry queue still has buffered readings)");
        } else {
            info!("PowerSave: skip deep sleep (pending telemetry retry)");
        }
        return;
    }

    let settings = get_settings();
    if !settings.predictive_sleep_enabled {
        info!("PowerSave: skip deep sleep (predictive sleep disabled)");
        return;
    }

    // Do not sleep while the UI-idle deadline has not elapsed.
    let now = now_secs();
    let idle_deadline = UI_IDLE_DEADLINE_SECS.load(Ordering::Relaxed);
    if now < idle_deadline {
        let idle_remaining = idle_deadline - now;
        info!(
            "PowerSave: skip deep sleep (UI idle deadline {}s remaining)",
            idle_remaining
        );
        return;
    }

    let sleep_secs = settings
        .sleep_duration_secs
        .clamp(POWER_SLEEP_DURATION_MIN_SECS, POWER_SLEEP_DURATION_MAX_SECS);
    info!(
        "PowerSave: entering deep sleep for {}s (timer + button wake)",
        sleep_secs
    );

    let mut rtc = Rtc::new(unsafe { esp_hal::peripherals::LPWR::steal() });
    let timer = TimerWakeupSource::new(Duration::from_secs(u64::from(sleep_secs)));

    // Wakeup source for UI input in deep sleep: EC11 push button on GPIO27.
    // Rotary A/B are not configured as deep-sleep wake sources.
    // Safe here because deep sleep resets the app and this function never returns.
    let button_pin = unsafe { esp_hal::peripherals::GPIO27::steal() };
    let ext0 = Ext0WakeupSource::new(button_pin, WakeupLevel::Low);

    let mut sleep_cfg = RtcSleepConfig::deep();
    sleep_cfg.set_rtc_slowmem_pd_en(false);
    sleep_cfg.set_rtc_fastmem_pd_en(true);

    info!("PowerSave: entering deep sleep now");
    warn!("PowerSave: deep sleeping now (rtc_slowmem retained)");
    // Give RTT/defmt a short window to flush before reset-on-deep-sleep.
    esp_hal::rom::ets_delay_us(DEEP_SLEEP_LOG_FLUSH_DELAY_US);
    rtc.sleep(&sleep_cfg, &[&timer, &ext0]);
    warn!("PowerSave: deep sleep returned unexpectedly");
}
