use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver as ChannelReceiver, Sender as ChannelSender};
use embassy_sync::mutex::Mutex;
use embassy_sync::watch::{Receiver as WatchReceiver, Sender as WatchSender, Watch};
use esp_storage::FlashStorage;
use static_cell::StaticCell;

use ota_core::OtaState;

use crate::messages::{PowerSettings, RadioReading, RadioSettings};
use crate::power;
use crate::radio_settings;
use crate::ui_input::UiEvent;

const READINGS_WATCH_DEPTH: usize = 2;
const RADIO_SETTINGS_WATCH_DEPTH: usize = 2;
const POWER_SETTINGS_WATCH_DEPTH: usize = 2;
const OTA_STATE_WATCH_DEPTH: usize = 6;
const MQTT_HEALTH_WATCH_DEPTH: usize = 2;
const APP_EVENT_CHANNEL_DEPTH: usize = 8;
const APP_COMMAND_CHANNEL_DEPTH: usize = 16;
const MQTT_COMMAND_CHANNEL_DEPTH: usize = 16;
const OTA_COMMAND_CHANNEL_DEPTH: usize = 1;
const RADIO_TELEMETRY_CHANNEL_DEPTH: usize = 16;

pub const OTA_MANIFEST_MAX_BYTES: usize = ota_core::MAX_MANIFEST_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiInputEvent {
    Navigation(UiEvent),
    ApplySettings {
        radio: RadioSettings,
        power: PowerSettings,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum AppEvent {
    UiInput(UiInputEvent),
    RadioFrameDecoded(RadioReading),
    TimeUpdated(u64),
}

#[derive(Clone, Copy, Debug)]
pub enum AppCommand {
    ApplySettings {
        radio: RadioSettings,
        power: PowerSettings,
    },
    PublishTelemetry(RadioReading),
    OtaConfirmed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlDispatch {
    ApplySettings {
        radio: RadioSettings,
        power: PowerSettings,
    },
    Ignore,
}

/// Health signal from the MQTT task, observed by the OTA confirmation gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MqttHealth {
    Disconnected,
    HeartbeatPublished,
}

pub type ReadingSender =
    WatchSender<'static, CriticalSectionRawMutex, RadioReading, READINGS_WATCH_DEPTH>;
pub type ReadingReceiver =
    WatchReceiver<'static, CriticalSectionRawMutex, RadioReading, READINGS_WATCH_DEPTH>;
pub type RadioSettingsSender =
    WatchSender<'static, CriticalSectionRawMutex, RadioSettings, RADIO_SETTINGS_WATCH_DEPTH>;
pub type RadioSettingsReceiver =
    WatchReceiver<'static, CriticalSectionRawMutex, RadioSettings, RADIO_SETTINGS_WATCH_DEPTH>;
pub type PowerSettingsSender =
    WatchSender<'static, CriticalSectionRawMutex, PowerSettings, POWER_SETTINGS_WATCH_DEPTH>;
pub type PowerSettingsReceiver =
    WatchReceiver<'static, CriticalSectionRawMutex, PowerSettings, POWER_SETTINGS_WATCH_DEPTH>;
pub type OtaStateSender =
    WatchSender<'static, CriticalSectionRawMutex, OtaState, OTA_STATE_WATCH_DEPTH>;
pub type OtaStateReceiver =
    WatchReceiver<'static, CriticalSectionRawMutex, OtaState, OTA_STATE_WATCH_DEPTH>;
pub type MqttHealthSender =
    WatchSender<'static, CriticalSectionRawMutex, MqttHealth, MQTT_HEALTH_WATCH_DEPTH>;
pub type MqttHealthReceiver =
    WatchReceiver<'static, CriticalSectionRawMutex, MqttHealth, MQTT_HEALTH_WATCH_DEPTH>;
pub type AppEventSender =
    ChannelSender<'static, CriticalSectionRawMutex, AppEvent, APP_EVENT_CHANNEL_DEPTH>;
pub type AppEventReceiver =
    ChannelReceiver<'static, CriticalSectionRawMutex, AppEvent, APP_EVENT_CHANNEL_DEPTH>;
pub type AppCommandSender =
    ChannelSender<'static, CriticalSectionRawMutex, AppCommand, APP_COMMAND_CHANNEL_DEPTH>;
pub type AppCommandReceiver =
    ChannelReceiver<'static, CriticalSectionRawMutex, AppCommand, APP_COMMAND_CHANNEL_DEPTH>;
pub type MqttCommandSender =
    ChannelSender<'static, CriticalSectionRawMutex, AppCommand, MQTT_COMMAND_CHANNEL_DEPTH>;
pub type MqttCommandReceiver =
    ChannelReceiver<'static, CriticalSectionRawMutex, AppCommand, MQTT_COMMAND_CHANNEL_DEPTH>;
pub type OtaManifestBytes = heapless::Vec<u8, OTA_MANIFEST_MAX_BYTES>;
pub type OtaCommandSender =
    ChannelSender<'static, CriticalSectionRawMutex, OtaManifestBytes, OTA_COMMAND_CHANNEL_DEPTH>;
pub type OtaCommandReceiver =
    ChannelReceiver<'static, CriticalSectionRawMutex, OtaManifestBytes, OTA_COMMAND_CHANNEL_DEPTH>;
pub type RadioTelemetrySender =
    ChannelSender<'static, CriticalSectionRawMutex, RadioReading, RADIO_TELEMETRY_CHANNEL_DEPTH>;
pub type RadioTelemetryReceiver =
    ChannelReceiver<'static, CriticalSectionRawMutex, RadioReading, RADIO_TELEMETRY_CHANNEL_DEPTH>;

pub static READING_WATCH: Watch<CriticalSectionRawMutex, RadioReading, READINGS_WATCH_DEPTH> =
    Watch::new();
pub static RADIO_SETTINGS_WATCH: Watch<
    CriticalSectionRawMutex,
    RadioSettings,
    RADIO_SETTINGS_WATCH_DEPTH,
> = Watch::new();
pub static POWER_SETTINGS_WATCH: Watch<
    CriticalSectionRawMutex,
    PowerSettings,
    POWER_SETTINGS_WATCH_DEPTH,
> = Watch::new();
pub static OTA_STATE_WATCH: Watch<CriticalSectionRawMutex, OtaState, OTA_STATE_WATCH_DEPTH> =
    Watch::new();
pub static MQTT_HEALTH_WATCH: Watch<CriticalSectionRawMutex, MqttHealth, MQTT_HEALTH_WATCH_DEPTH> =
    Watch::new();
pub static APP_EVENT_CHANNEL: Channel<CriticalSectionRawMutex, AppEvent, APP_EVENT_CHANNEL_DEPTH> =
    Channel::new();
pub static APP_COMMAND_CHANNEL: Channel<
    CriticalSectionRawMutex,
    AppCommand,
    APP_COMMAND_CHANNEL_DEPTH,
> = Channel::new();
pub static MQTT_COMMAND_CHANNEL: Channel<
    CriticalSectionRawMutex,
    AppCommand,
    MQTT_COMMAND_CHANNEL_DEPTH,
> = Channel::new();
pub static OTA_COMMAND_CHANNEL: Channel<
    CriticalSectionRawMutex,
    OtaManifestBytes,
    OTA_COMMAND_CHANNEL_DEPTH,
> = Channel::new();
pub static RADIO_TELEMETRY_CHANNEL: Channel<
    CriticalSectionRawMutex,
    RadioReading,
    RADIO_TELEMETRY_CHANNEL_DEPTH,
> = Channel::new();
pub static FLASH: StaticCell<Mutex<CriticalSectionRawMutex, FlashStorage<'static>>> =
    StaticCell::new();

#[must_use]
pub fn route_event(event: AppEvent) -> Option<AppCommand> {
    match event {
        AppEvent::UiInput(UiInputEvent::ApplySettings { radio, power }) => {
            Some(AppCommand::ApplySettings { radio, power })
        }
        AppEvent::RadioFrameDecoded(reading) => Some(AppCommand::PublishTelemetry(reading)),
        AppEvent::UiInput(UiInputEvent::Navigation(_)) | AppEvent::TimeUpdated(_) => None,
    }
}

#[must_use]
pub fn route_event_to_control_command(event: AppEvent) -> Option<AppCommand> {
    match route_event(event) {
        Some(AppCommand::ApplySettings { radio, power }) => {
            Some(AppCommand::ApplySettings { radio, power })
        }
        Some(AppCommand::PublishTelemetry(_) | AppCommand::OtaConfirmed) | None => None,
    }
}

#[must_use]
pub fn route_event_to_mqtt_command(event: AppEvent) -> Option<AppCommand> {
    match route_event(event) {
        Some(AppCommand::PublishTelemetry(reading)) => Some(AppCommand::PublishTelemetry(reading)),
        Some(AppCommand::OtaConfirmed | AppCommand::ApplySettings { .. }) | None => None,
    }
}

#[must_use]
pub fn classify_control_dispatch(command: AppCommand) -> ControlDispatch {
    match command {
        AppCommand::ApplySettings { radio, power } => {
            ControlDispatch::ApplySettings { radio, power }
        }
        AppCommand::PublishTelemetry(_) | AppCommand::OtaConfirmed => ControlDispatch::Ignore,
    }
}

#[embassy_executor::task]
pub async fn app_command_dispatch_task() {
    let app_command_receiver = APP_COMMAND_CHANNEL.receiver();
    let radio_settings_sender = RADIO_SETTINGS_WATCH.sender();
    let power_settings_sender = POWER_SETTINGS_WATCH.sender();

    loop {
        match classify_control_dispatch(app_command_receiver.receive().await) {
            ControlDispatch::ApplySettings {
                radio,
                power: power_settings,
            } => {
                radio_settings_sender.send(radio);
                radio_settings::persist_settings(radio);
                power_settings_sender.send(power_settings);
                power::set_settings(power_settings);
            }
            ControlDispatch::Ignore => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppCommand, AppEvent, ControlDispatch, UiInputEvent, classify_control_dispatch,
        route_event, route_event_to_control_command, route_event_to_mqtt_command,
    };
    use crate::messages::{
        DEFAULT_POWER_SETTINGS, DEFAULT_RADIO_SETTINGS, PowerSettings, RadioReading, RadioSettings,
    };
    use crate::ui_input::UiEvent;
    use rubicson::RubicsonReading;

    fn sample_radio_settings() -> RadioSettings {
        RadioSettings {
            detection_threshold_db: DEFAULT_RADIO_SETTINGS.detection_threshold_db,
            magn_target: DEFAULT_RADIO_SETTINGS.magn_target,
            channel_bandwidth_index: DEFAULT_RADIO_SETTINGS.channel_bandwidth_index,
            carrier_sense_threshold: DEFAULT_RADIO_SETTINGS.carrier_sense_threshold,
        }
    }

    fn sample_power_settings() -> PowerSettings {
        PowerSettings {
            predictive_sleep_enabled: DEFAULT_POWER_SETTINGS.predictive_sleep_enabled,
            sleep_duration_secs: DEFAULT_POWER_SETTINGS.sleep_duration_secs,
            ui_idle_timeout_secs: DEFAULT_POWER_SETTINGS.ui_idle_timeout_secs,
        }
    }

    fn sample_reading() -> RadioReading {
        RadioReading {
            inner: RubicsonReading {
                id: 0x23,
                channel: 2,
                battery_ok: true,
                temperature_c: 18.6,
                crc_ok: true,
            },
            rssi: -74,
            detection_threshold: 12,
        }
    }

    #[test]
    fn ui_input_event_routes_to_apply_settings_command() {
        let radio = sample_radio_settings();
        let power = sample_power_settings();
        let event = AppEvent::UiInput(UiInputEvent::ApplySettings { radio, power });

        let command = route_event(event);
        assert!(matches!(
            command,
            Some(AppCommand::ApplySettings { radio: routed_radio, power: routed_power })
                if routed_radio.detection_threshold_db == radio.detection_threshold_db
                    && routed_radio.channel_bandwidth_index == radio.channel_bandwidth_index
                    && routed_power.predictive_sleep_enabled == power.predictive_sleep_enabled
                    && routed_power.sleep_duration_secs == power.sleep_duration_secs
        ));
    }

    #[test]
    fn radio_frame_decoded_routes_to_publish_telemetry_command() {
        let reading = sample_reading();
        let event = AppEvent::RadioFrameDecoded(reading);

        let command = route_event(event);
        assert!(matches!(
            command,
            Some(AppCommand::PublishTelemetry(routed))
                if routed.inner.id == reading.inner.id
                    && routed.inner.channel == reading.inner.channel
                    && routed.rssi == reading.rssi
                    && routed.detection_threshold == reading.detection_threshold
        ));
    }

    #[test]
    fn ui_apply_settings_routes_to_control_path_only() {
        let radio = sample_radio_settings();
        let power = sample_power_settings();
        let event = AppEvent::UiInput(UiInputEvent::ApplySettings { radio, power });

        assert!(matches!(
            route_event_to_control_command(event),
            Some(AppCommand::ApplySettings { .. })
        ));
        assert!(route_event_to_mqtt_command(event).is_none());
    }

    #[test]
    fn radio_frame_routes_to_mqtt_path_only() {
        let reading = sample_reading();
        let event = AppEvent::RadioFrameDecoded(reading);

        assert!(matches!(
            route_event_to_mqtt_command(event),
            Some(AppCommand::PublishTelemetry(routed))
                if routed.inner.id == reading.inner.id
                    && routed.inner.channel == reading.inner.channel
        ));
        assert!(route_event_to_control_command(event).is_none());
    }

    #[test]
    fn control_dispatch_ignores_publish_telemetry_commands() {
        let reading = sample_reading();
        assert!(matches!(
            classify_control_dispatch(AppCommand::PublishTelemetry(reading)),
            ControlDispatch::Ignore
        ));
    }

    #[test]
    fn unrelated_event_variants_are_ignored_without_panicking() {
        let events = [
            AppEvent::UiInput(UiInputEvent::Navigation(
                crate::ui_input::UiEvent::NextScreen,
            )),
            AppEvent::TimeUpdated(1_707_000_000),
        ];

        for event in events {
            assert!(route_event(event).is_none());
        }
    }
}
