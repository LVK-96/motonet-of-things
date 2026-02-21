use defmt::{error, info};
use embassy_executor::Spawner;

use crate::app_bus;
use crate::startup::composition::StartupContext;
use crate::tasks::{
    display as display_task, led_pwm as led_pwm_task, mqtt as mqtt_task,
    radio_433 as radio_433_task, time_sync as time_sync_task,
};

#[allow(clippy::expect_used)]
#[allow(clippy::too_many_lines)]
pub(crate) fn spawn_tasks(spawner: &Spawner, context: StartupContext) {
    let StartupContext {
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
        if let Err(e) = spawner.spawn(led_pwm_task::led_pwm_task(channel)) {
            error!("Failed to spawn LED task: {}", defmt::Debug2Format(&e));
        }
    }

    #[cfg(feature = "pulse_sw")]
    {
        let settings_receiver = app_bus::RADIO_SETTINGS_WATCH
            .receiver()
            .expect("Failed to get settings receiver");
        spawner
            .spawn(radio_433_task::radio_433_rx_task(
                radio,
                app_bus::READING_WATCH.sender(),
                app_bus::RADIO_TELEMETRY_CHANNEL.sender(),
                app_bus::RADIO_SETTINGS_WATCH.sender(),
                settings_receiver,
            ))
            .expect("Failed to spawn radio task");
    }

    #[cfg(feature = "pulse_rmt")]
    {
        let settings_receiver = app_bus::RADIO_SETTINGS_WATCH
            .receiver()
            .expect("Failed to get settings receiver");
        spawner
            .spawn(radio_433_task::radio_433_settings_task(
                shared_radio,
                settings_receiver,
            ))
            .expect("Failed to spawn radio settings task");
        spawner
            .spawn(radio_433_task::radio_433_rx_task(
                shared_radio,
                rmt_rx,
                app_bus::READING_WATCH.sender(),
                app_bus::RADIO_TELEMETRY_CHANNEL.sender(),
                app_bus::RADIO_SETTINGS_WATCH.sender(),
            ))
            .expect("Failed to spawn radio task");
    }

    spawner
        .spawn(radio_433_task::radio_433_event_router_task(
            app_bus::RADIO_TELEMETRY_CHANNEL.receiver(),
            app_bus::MQTT_COMMAND_CHANNEL.sender(),
        ))
        .expect("Failed to spawn radio event router task");

    spawner
        .spawn(app_bus::app_command_dispatch_task())
        .expect("Failed to spawn app command dispatch task");

    spawner
        .spawn(mqtt_task::mqtt_task(
            network_stack,
            app_bus::MQTT_COMMAND_CHANNEL.receiver(),
        ))
        .expect("Failed to spawn mqtt task");

    spawner
        .spawn(display_task::ui_input_task(
            ui_input,
            app_bus::APP_EVENT_CHANNEL.sender(),
        ))
        .expect("Failed to spawn ui input task");

    spawner
        .spawn(display_task::display_task(
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
        ))
        .expect("Failed to spawn display task");

    spawner
        .spawn(time_sync_task::time_sync_task(network_stack))
        .expect("Failed to spawn time sync task");
}
