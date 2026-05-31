#![no_std]

use defmt::error;
use embassy_time::{Duration, Timer};

pub mod app_bus;
pub mod display_driver;
pub mod messages;
pub mod network;
pub mod ota;
pub mod power;
pub mod pulse_capture;
pub mod radio_433;
pub mod secrets;
pub mod startup;
pub mod tasks;
pub mod telemetry;
pub mod time_sync;
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
