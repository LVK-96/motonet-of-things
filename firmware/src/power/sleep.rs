use core::time::Duration;

use defmt::{Debug2Format, info, warn};
use esp_hal::rtc_cntl::{
    Rtc,
    sleep::{Ext0WakeupSource, RtcSleepConfig, TimerWakeupSource, WakeupLevel},
    wakeup_cause,
};
use esp_hal::system::SleepSource;

const DEEP_SLEEP_LOG_FLUSH_DELAY_US: u32 = 20_000;

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

pub(super) fn enter_deep_sleep(sleep_secs: u8) {
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

    warn!("PowerSave: deep sleeping now (rtc_slowmem retained)");
    // Give RTT/defmt a short window to flush before reset-on-deep-sleep.
    esp_hal::rom::ets_delay_us(DEEP_SLEEP_LOG_FLUSH_DELAY_US);
    rtc.sleep(&sleep_cfg, &[&timer, &ext0]);
    warn!("PowerSave: deep sleep returned unexpectedly");
}
