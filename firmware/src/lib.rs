#![no_std]
#![feature(asm_experimental_arch)]

extern crate alloc;

use defmt::error;
use embassy_time::{Duration, Timer};

pub mod app_bus;
pub mod display_driver;
mod domain_map;
pub mod messages;
pub mod network;
pub mod ota;
pub mod power;
pub mod pulse_capture;
pub mod radio_433;
pub mod radio_settings;
pub mod secrets;
pub mod startup;
pub mod tasks;
pub mod telemetry;
pub mod time_sync;
pub mod tls_workspace;
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
