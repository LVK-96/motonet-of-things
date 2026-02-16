#![cfg_attr(not(test), no_std)]

pub mod config_rules;
pub mod display_model;
pub mod domain;
pub mod ntp_servers;

#[cfg(test)]
#[path = "../../../firmware/src/persistence/rtc_schema.rs"]
mod rtc_schema_tests;
