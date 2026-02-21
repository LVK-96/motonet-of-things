use core::time::Duration as CoreDuration;

use app_core::runtime_policy::{
    MQTT_MIN_BACKOFF_SECS, next_mqtt_backoff_secs, should_reconnect_before_publish,
};
use defmt::{debug, info, trace};
use embassy_futures::select::{Either, select};
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant, Timer};
use rust_mqtt::buffer::BumpBuffer;
use rust_mqtt::client::Client;

use crate::app_bus::{self, AppCommand};
use crate::messages::RadioReading;
use crate::power;

mod publish;
mod session;

type MqttClient<'a> = Client<'a, TcpSocket<'a>, BumpBuffer<'a>, 4, 2, 2>;

#[derive(Clone, Copy, Debug)]
enum MqttWork {
    Reading(RadioReading),
    KeepAlivePing,
}

fn telemetry_from_command(command: AppCommand) -> Option<RadioReading> {
    match command {
        AppCommand::PublishTelemetry(reading) => Some(reading),
        AppCommand::ApplySettings { .. } => None,
    }
}

async fn pending_or_next_work(
    deferred_reading: &mut Option<RadioReading>,
    receiver: &app_bus::MqttCommandReceiver,
) -> Option<MqttWork> {
    const PING_INTERVAL_SECS: u64 = 90; // Ping every 90s (keepalive is 120s)

    if let Some(reading) = deferred_reading.take() {
        debug!("MQTT: Retrying deferred reading after reconnect");
        return Some(MqttWork::Reading(reading));
    }

    let timeout = Duration::from_secs(PING_INTERVAL_SECS);
    match select(receiver.receive(), Timer::after(timeout)).await {
        Either::First(command) => {
            let Some(reading) = telemetry_from_command(command) else {
                trace!("MQTT: Ignoring non-telemetry command");
                return None;
            };
            Some(MqttWork::Reading(reading))
        }
        Either::Second(()) => Some(MqttWork::KeepAlivePing),
    }
}

#[embassy_executor::task]
#[allow(clippy::expect_used, clippy::too_many_lines)]
pub async fn mqtt_task(
    network_stack: embassy_net::Stack<'static>,
    receiver: app_bus::MqttCommandReceiver,
) {
    let mut backoff_secs = MQTT_MIN_BACKOFF_SECS;
    let mut deferred_reading: Option<RadioReading> = None;

    // TODO: Persist MQTT sessions
    loop {
        // Create buffers on the stack for this session
        let mut rx_buf = [0u8; 1024];
        let mut tx_buf = [0u8; 1024];
        let mut mqtt_buf = [0u8; 1024];
        let mut mqtt_buffer = BumpBuffer::new(&mut mqtt_buf);
        let mut client = Client::new(&mut mqtt_buffer);

        if session::establish_mqtt_session(network_stack, &mut rx_buf, &mut tx_buf, &mut client)
            .await
            .is_err()
        {
            Timer::after(Duration::from_secs(backoff_secs)).await;
            backoff_secs = next_mqtt_backoff_secs(backoff_secs);
            continue;
        }

        backoff_secs = MQTT_MIN_BACKOFF_SECS;
        info!("MQTT: Ready");
        let mut last_activity = Instant::now();

        loop {
            let Some(work) = pending_or_next_work(&mut deferred_reading, &receiver).await else {
                continue;
            };

            match work {
                MqttWork::Reading(reading) => {
                    let time_since_last = last_activity.elapsed().as_secs();
                    debug!("MQTT: Got reading after {}s idle", time_since_last);

                    if should_reconnect_before_publish(time_since_last) {
                        info!(
                            "MQTT: Reconnecting before publish after {}s idle",
                            time_since_last
                        );
                        deferred_reading = Some(reading);
                        break; // Exit inner loop to reconnect
                    }

                    match publish::publish_reading(&mut client, reading).await {
                        publish::PublishOutcome::Published => {
                            debug!("MQTT: Publish successful (QoS 1, awaiting PUBACK)");
                            info!("MQTT: Publish confirmed, checking deep sleep policy");
                            let time_since_measurement = Instant::now() - reading.received_at;
                            power::maybe_sleep_after_publish(
                                receiver.is_empty(),
                                CoreDuration::from_secs(time_since_measurement.as_secs()),
                            );
                            last_activity = Instant::now();
                        }
                        publish::PublishOutcome::Dropped => {}
                        publish::PublishOutcome::Reconnect(reading) => {
                            deferred_reading = Some(reading);
                            break; // Exit inner loop to reconnect
                        }
                    }
                }
                MqttWork::KeepAlivePing => {
                    if publish::ping(&mut client).await.is_err() {
                        break; // Exit inner loop to reconnect
                    }

                    last_activity = Instant::now();
                }
            }
        }

        // Backoff before reconnecting
        Timer::after(Duration::from_secs(backoff_secs)).await;
        backoff_secs = next_mqtt_backoff_secs(backoff_secs);
    }
}
