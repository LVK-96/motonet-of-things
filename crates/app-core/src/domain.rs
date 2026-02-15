#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SensorReading {
    pub sensor_id: u8,
    pub channel: u8,
    pub battery_ok: bool,
    pub temperature_c: f32,
    pub rssi_dbm: i16,
    pub detection_threshold_db: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RadioConfigView {
    pub detection_threshold_db: u8,
    pub magn_target: u8,
    pub channel_bandwidth_index: u8,
    pub carrier_sense_threshold: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerConfigView {
    pub predictive_sleep_enabled: bool,
    pub sleep_duration_secs: u8,
    pub ui_idle_timeout_secs: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiScreenState {
    Waiting,
    Main,
    RadioOverview,
    RadioSettings,
}
