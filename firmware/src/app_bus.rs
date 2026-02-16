use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, Receiver as ChannelReceiver, Sender as ChannelSender};
use embassy_sync::watch::{Receiver as WatchReceiver, Sender as WatchSender, Watch};

use crate::messages::{PowerSettings, RadioReading, RadioSettings};
use crate::power;
use crate::ui_input::UiEvent;

const READINGS_WATCH_DEPTH: usize = 2;
const RADIO_SETTINGS_WATCH_DEPTH: usize = 2;
const POWER_SETTINGS_WATCH_DEPTH: usize = 2;
const APP_EVENT_CHANNEL_DEPTH: usize = 8;
const APP_COMMAND_CHANNEL_DEPTH: usize = 16;
const MQTT_COMMAND_CHANNEL_DEPTH: usize = 16;
const RADIO_TELEMETRY_CHANNEL_DEPTH: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MqttSessionState {
    Connected,
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkState {
    Up,
    Down,
}

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
    MqttSessionState(MqttSessionState),
    TimeUpdated(u64),
    NetworkState(NetworkState),
}

#[derive(Clone, Copy, Debug)]
pub enum AppCommand {
    ApplySettings {
        radio: RadioSettings,
        power: PowerSettings,
    },
    PublishTelemetry(RadioReading),
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
pub static RADIO_TELEMETRY_CHANNEL: Channel<
    CriticalSectionRawMutex,
    RadioReading,
    RADIO_TELEMETRY_CHANNEL_DEPTH,
> = Channel::new();

#[must_use]
pub fn route_event(event: AppEvent) -> Option<AppCommand> {
    match event {
        AppEvent::UiInput(UiInputEvent::ApplySettings { radio, power }) => {
            Some(AppCommand::ApplySettings { radio, power })
        }
        AppEvent::RadioFrameDecoded(reading) => Some(AppCommand::PublishTelemetry(reading)),
        AppEvent::UiInput(UiInputEvent::Navigation(_))
        | AppEvent::MqttSessionState(_)
        | AppEvent::TimeUpdated(_)
        | AppEvent::NetworkState(_) => None,
    }
}

#[embassy_executor::task]
pub async fn app_command_dispatch_task() {
    let app_command_receiver = APP_COMMAND_CHANNEL.receiver();
    let mqtt_command_sender = MQTT_COMMAND_CHANNEL.sender();
    let radio_settings_sender = RADIO_SETTINGS_WATCH.sender();
    let power_settings_sender = POWER_SETTINGS_WATCH.sender();

    loop {
        match app_command_receiver.receive().await {
            AppCommand::ApplySettings {
                radio,
                power: power_settings,
            } => {
                radio_settings_sender.send(radio);
                power_settings_sender.send(power_settings);
                power::set_settings(power_settings);
            }
            AppCommand::PublishTelemetry(reading) => {
                mqtt_command_sender
                    .send(AppCommand::PublishTelemetry(reading))
                    .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AppCommand, AppEvent, MqttSessionState, NetworkState, UiInputEvent, route_event};
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
    fn unrelated_event_variants_are_ignored_without_panicking() {
        let events = [
            AppEvent::UiInput(UiInputEvent::Navigation(UiEvent::NextScreen)),
            AppEvent::MqttSessionState(MqttSessionState::Connected),
            AppEvent::TimeUpdated(1_707_000_000),
            AppEvent::NetworkState(NetworkState::Down),
        ];

        for event in events {
            assert!(route_event(event).is_none());
        }
    }
}
