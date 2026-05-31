use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Receiver;

use crate::messages::RadioReading;

pub(crate) mod adapters;
pub mod display;
pub mod led_pwm;
pub mod mqtt;
pub mod network_supervisor;
pub mod radio_433;
pub mod time_sync;
pub mod ota_payload_receive;

pub type TelemetryReceiver = Receiver<'static, CriticalSectionRawMutex, RadioReading, 16>;
