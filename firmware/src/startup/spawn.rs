use defmt::info;
use embassy_executor::{SpawnError, SpawnToken, Spawner};

use crate::app_bus;
use crate::startup::hw_context::HWContext;
use crate::tasks::{
    display as display_task, led_pwm as led_pwm_task, mqtt as mqtt_task, ota as ota_task,
    ota_confirm as ota_confirm_task, radio_433 as radio_433_task, time_sync as time_sync_task,
};
use ota_core::OtaState;

static MQTT_HEALTH_RX_STORAGE: static_cell::StaticCell<app_bus::MqttHealthReceiver> =
    static_cell::StaticCell::new();

#[allow(clippy::expect_used, clippy::trivially_copy_pass_by_ref)]
fn spawn_task<S>(
    spawner: &Spawner,
    token: Result<SpawnToken<S>, SpawnError>,
    message: &'static str,
) {
    spawner.spawn(token.expect(message));
}

#[allow(clippy::expect_used)]
#[allow(clippy::too_many_lines)]
pub(crate) fn spawn_tasks(spawner: &Spawner, context: HWContext) {
    let HWContext {
        led_channel,
        network_stack,
        display,
        ui_input,
        flash_mutex,
        #[cfg(feature = "pulse_sw")]
        radio,
        #[cfg(feature = "pulse_rmt")]
        shared_radio,
        #[cfg(feature = "pulse_rmt")]
        rmt_rx,
    } = context;

    // Create OTA state watch sender and receivers before any task needs them.
    let ota_state_sender = app_bus::OTA_STATE_WATCH.sender();
    ota_state_sender.send(OtaState::Inactive);
    let ota_state_rx = app_bus::OTA_STATE_WATCH
        .receiver()
        .expect("Failed to get OTA state receiver for mqtt");
    let ota_state_rx_radio = app_bus::OTA_STATE_WATCH
        .receiver()
        .expect("Failed to get OTA state receiver for radio");
    let ota_state_rx_router = app_bus::OTA_STATE_WATCH
        .receiver()
        .expect("Failed to get OTA state receiver for radio router");
    let ota_state_rx_ui = app_bus::OTA_STATE_WATCH
        .receiver()
        .expect("Failed to get OTA state receiver for UI input");
    let ota_state_rx_display = app_bus::OTA_STATE_WATCH
        .receiver()
        .expect("Failed to get OTA state receiver for display");
    let ota_state_sender_confirm = app_bus::OTA_STATE_WATCH.sender();

    // MQTT health watch for OTA confirmation + stand-down detection.
    let mqtt_health_sender = app_bus::MQTT_HEALTH_WATCH.sender();
    let mqtt_health_receiver_confirm = app_bus::MQTT_HEALTH_WATCH
        .receiver()
        .expect("Failed to get MQTT health receiver for OTA confirmation");
    let mqtt_health_receiver_ref: &'static mut app_bus::MqttHealthReceiver =
        MQTT_HEALTH_RX_STORAGE.init_with(move || mqtt_health_receiver_confirm);
    let mqtt_health_receiver_ota = app_bus::MQTT_HEALTH_WATCH
        .receiver()
        .expect("Failed to get MQTT health receiver for OTA task");

    if let Some(channel) = led_channel {
        info!("LED hardware configured! Spawning task...");
        spawn_task(
            spawner,
            led_pwm_task::led_pwm_task(channel),
            "Failed to spawn LED task",
        );
    }

    #[cfg(feature = "pulse_sw")]
    {
        let settings_receiver = app_bus::RADIO_SETTINGS_WATCH
            .receiver()
            .expect("Failed to get settings receiver");
        spawn_task(
            spawner,
            radio_433_task::radio_433_rx_task(
                radio,
                app_bus::READING_WATCH.sender(),
                app_bus::RADIO_TELEMETRY_CHANNEL.sender(),
                app_bus::RADIO_SETTINGS_WATCH.sender(),
                settings_receiver,
                ota_state_rx_radio,
            ),
            "Failed to spawn radio task",
        );
    }

    #[cfg(feature = "pulse_rmt")]
    {
        let settings_receiver = app_bus::RADIO_SETTINGS_WATCH
            .receiver()
            .expect("Failed to get settings receiver");
        spawn_task(
            spawner,
            radio_433_task::radio_433_settings_task(shared_radio, settings_receiver),
            "Failed to spawn radio settings task",
        );
        spawn_task(
            spawner,
            radio_433_task::radio_433_rx_task(
                shared_radio,
                rmt_rx,
                app_bus::READING_WATCH.sender(),
                app_bus::RADIO_TELEMETRY_CHANNEL.sender(),
                app_bus::RADIO_SETTINGS_WATCH.sender(),
                ota_state_rx_radio,
            ),
            "Failed to spawn radio task",
        );
    }

    spawn_task(
        spawner,
        radio_433_task::radio_433_event_router_task(
            app_bus::RADIO_TELEMETRY_CHANNEL.receiver(),
            app_bus::MQTT_COMMAND_CHANNEL.sender(),
            ota_state_rx_router,
        ),
        "Failed to spawn radio event router task",
    );

    spawn_task(
        spawner,
        app_bus::app_command_dispatch_task(),
        "Failed to spawn app command dispatch task",
    );

    // Spawn confirmation before MQTT/OTA command handling so post-reboot
    // PendingVerify state is observed before any retained OTA manifest can be
    // forwarded again.
    spawn_task(
        spawner,
        ota_confirm_task::ota_confirm_task(
            ota_state_sender_confirm,
            mqtt_health_receiver_ref,
            app_bus::MQTT_COMMAND_CHANNEL.sender(),
            flash_mutex,
        ),
        "Failed to spawn OTA confirm task",
    );

    spawn_task(
        spawner,
        mqtt_task::mqtt_task(
            network_stack,
            app_bus::MQTT_COMMAND_CHANNEL.receiver(),
            app_bus::OTA_COMMAND_CHANNEL.sender(),
            ota_state_rx,
            mqtt_health_sender,
        ),
        "Failed to spawn mqtt task",
    );

    spawn_task(
        spawner,
        ota_task::ota_task(
            app_bus::OTA_COMMAND_CHANNEL.receiver(),
            ota_state_sender,
            mqtt_health_receiver_ota,
            network_stack,
            flash_mutex,
        ),
        "Failed to spawn OTA task",
    );

    spawn_task(
        spawner,
        display_task::ui_input_task(
            ui_input,
            app_bus::APP_EVENT_CHANNEL.sender(),
            ota_state_rx_ui,
        ),
        "Failed to spawn ui input task",
    );

    spawn_task(
        spawner,
        display_task::display_task(
            display,
            app_bus::READING_WATCH
                .receiver()
                .expect("Failed to get reading receiver"),
            app_bus::APP_EVENT_CHANNEL.receiver(),
            app_bus::RADIO_SETTINGS_WATCH
                .receiver()
                .expect("Failed to get settings receiver for display"),
            app_bus::POWER_SETTINGS_WATCH
                .receiver()
                .expect("Failed to get power settings receiver for display"),
            app_bus::APP_COMMAND_CHANNEL.sender(),
            ota_state_rx_display,
        ),
        "Failed to spawn display task",
    );

    spawn_task(
        spawner,
        time_sync_task::time_sync_task(network_stack),
        "Failed to spawn time sync task",
    );
}
