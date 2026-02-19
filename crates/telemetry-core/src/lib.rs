#![cfg_attr(not(test), no_std)]

pub mod dedupe;
pub mod queue;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TelemetryRecord {
    pub sensor_id: u8,
    pub channel: u8,
    pub temperature_deci_c: i16,
    pub battery_ok: bool,
    pub timestamp: u64,
}
