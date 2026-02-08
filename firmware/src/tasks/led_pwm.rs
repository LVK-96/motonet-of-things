use embassy_time::{Duration, Timer};
use esp_hal::ledc::{
    LowSpeed,
    channel::{Channel as LedcChannel, ChannelIFace},
};

#[embassy_executor::task]
pub async fn led_pwm_task(channel: LedcChannel<'static, LowSpeed>) {
    let brightness = 15;
    let duration_ms = 2000;
    loop {
        if channel.start_duty_fade(0, brightness, duration_ms).is_ok() {
            wait_fade_done(&channel, duration_ms).await;
        }

        if channel.start_duty_fade(brightness, 0, duration_ms).is_ok() {
            wait_fade_done(&channel, duration_ms).await;
        }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)]
async fn wait_fade_done(channel: &LedcChannel<'static, LowSpeed>, duration_ms: u16) {
    while channel.is_duty_fade_running() {
        Timer::after(Duration::from_millis(u64::from(duration_ms))).await;
    }
}
