use defmt::info;
use embassy_executor::{SpawnError, SpawnToken, Spawner};

use crate::app_bus;
use crate::startup::hw_context::HWContext;
use crate::tasks::{
    display as display_task, led_pwm as led_pwm_task, mqtt as mqtt_task,
    ota_payload_receive as ota_payload_receive_task, radio_433 as radio_433_task,
    time_sync as time_sync_task,
};

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
        #[cfg(feature = "pulse_sw")]
        radio,
        #[cfg(feature = "pulse_rmt")]
        shared_radio,
        #[cfg(feature = "pulse_rmt")]
        rmt_rx,
    } = context;

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
            ),
            "Failed to spawn radio task",
        );
    }

    spawn_task(
        spawner,
        radio_433_task::radio_433_event_router_task(
            app_bus::RADIO_TELEMETRY_CHANNEL.receiver(),
            app_bus::MQTT_COMMAND_CHANNEL.sender(),
        ),
        "Failed to spawn radio event router task",
    );

    spawn_task(
        spawner,
        app_bus::app_command_dispatch_task(),
        "Failed to spawn app command dispatch task",
    );

    spawn_task(
        spawner,
        mqtt_task::mqtt_task(network_stack, app_bus::MQTT_COMMAND_CHANNEL.receiver()),
        "Failed to spawn mqtt task",
    );

    spawn_task(
        spawner,
        ota_payload_receive_task::ota_payload_receive_task(network_stack),
        "Failed to spawn OTA payload receiver task",
    );

    spawn_task(
        spawner,
        display_task::ui_input_task(ui_input, app_bus::APP_EVENT_CHANNEL.sender()),
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
        ),
        "Failed to spawn display task",
    );

    spawn_task(
        spawner,
        time_sync_task::time_sync_task(network_stack),
        "Failed to spawn time sync task",
    );
}
