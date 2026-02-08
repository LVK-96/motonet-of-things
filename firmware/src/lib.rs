#![no_std]

use defmt::error;
use embassy_time::{Duration, Timer};

pub mod display;
pub mod display_ui;
pub mod led_pwm_task;
pub mod messages;
pub mod mqtt_client;
pub mod network;
pub mod pulse_capture;
pub mod radio_433;
pub mod radio_433_task;
pub mod secrets;
pub mod time_sync;
pub mod time_sync_task;
pub mod ui_input;

/// Helper to retry an operation until it succeeds
pub async fn with_retry<T, E>(name: &str, mut f: impl FnMut() -> Result<T, E>) -> T
where
    E: defmt::Format,
{
    loop {
        match f() {
            Ok(val) => break val,
            Err(e) => {
                error!("{} failed: {:?}. Retrying in 5s...", name, e);
                Timer::after(Duration::from_secs(5)).await;
            }
        }
    }
}
