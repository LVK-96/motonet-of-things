pub const OTA_CONFIRMATION_DELAY_SECS: u32 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OtaConfirmationGate {
    required_uptime_secs: u32,
    wifi_connected: bool,
    mqtt_connected: bool,
    heartbeat_published: bool,
}

impl OtaConfirmationGate {
    #[must_use]
    pub const fn new(required_uptime_secs: u32) -> Self {
        Self {
            required_uptime_secs,
            wifi_connected: false,
            mqtt_connected: false,
            heartbeat_published: false,
        }
    }

    pub fn note_wifi_connected(&mut self) {
        self.wifi_connected = true;
    }

    pub fn note_wifi_disconnected(&mut self) {
        self.wifi_connected = false;
        self.mqtt_connected = false;
        self.heartbeat_published = false;
    }

    pub fn note_mqtt_connected(&mut self) {
        self.mqtt_connected = true;
    }

    pub fn note_mqtt_disconnected(&mut self) {
        self.mqtt_connected = false;
        self.heartbeat_published = false;
    }

    pub fn note_heartbeat_published(&mut self) {
        self.heartbeat_published = true;
    }

    #[must_use]
    pub const fn required_uptime_secs(&self) -> u32 {
        self.required_uptime_secs
    }

    #[must_use]
    pub const fn ready_to_confirm(&self, uptime_secs: u32) -> bool {
        self.wifi_connected
            && self.mqtt_connected
            && self.heartbeat_published
            && uptime_secs >= self.required_uptime_secs
    }
}

impl Default for OtaConfirmationGate {
    fn default() -> Self {
        Self::new(OTA_CONFIRMATION_DELAY_SECS)
    }
}

#[cfg(test)]
mod tests {
    use super::{OTA_CONFIRMATION_DELAY_SECS, OtaConfirmationGate};

    #[test]
    fn confirmation_requires_all_health_signals_and_delay() {
        let mut gate = OtaConfirmationGate::default();
        assert!(!gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));

        gate.note_wifi_connected();
        assert!(!gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));

        gate.note_mqtt_connected();
        assert!(!gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));

        gate.note_heartbeat_published();
        assert!(!gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS - 1));
        assert!(gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));
    }

    #[test]
    fn wifi_loss_clears_dependent_health() {
        let mut gate = OtaConfirmationGate::default();
        gate.note_wifi_connected();
        gate.note_mqtt_connected();
        gate.note_heartbeat_published();
        assert!(gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));

        gate.note_wifi_disconnected();
        gate.note_wifi_connected();
        assert!(!gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));
    }

    #[test]
    fn mqtt_loss_clears_heartbeat() {
        let mut gate = OtaConfirmationGate::default();
        gate.note_wifi_connected();
        gate.note_mqtt_connected();
        gate.note_heartbeat_published();
        gate.note_mqtt_disconnected();
        gate.note_mqtt_connected();

        assert!(!gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));
        gate.note_heartbeat_published();
        assert!(gate.ready_to_confirm(OTA_CONFIRMATION_DELAY_SECS));
    }
}
