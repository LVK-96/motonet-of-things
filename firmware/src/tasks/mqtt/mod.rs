use core::time::Duration as CoreDuration;

use app_core::runtime_policy::{
    MQTT_MIN_BACKOFF_SECS, next_mqtt_backoff_secs, should_reconnect_before_publish,
};
use defmt::{debug, info, trace};
use embassy_futures::select::{Either, select};
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::{Read as AsyncRead, Write as AsyncWrite};
use embedded_tls::{Aes128GcmSha256, TlsConnection};
use rust_mqtt::buffer::BumpBuffer;
use rust_mqtt::client::Client;

use crate::app_bus::{self, AppCommand};
use crate::messages::RadioReading;
use crate::power;
use crate::secrets::MQTT_USE_TLS;

mod publish;
mod session;

type PlainClient<'a> = Client<'a, TcpSocket<'a>, BumpBuffer<'a>, 4, 2, 2, 0>;
type TlsSocket<'a> = TlsConnection<'a, TcpSocket<'a>, Aes128GcmSha256>;
type TlsClient<'a> = Client<'a, TlsSocket<'a>, BumpBuffer<'a>, 4, 2, 2, 0>;

const MQTT_TLS_RECORD_READ_BUF_SIZE: usize = 16640;
const MQTT_TLS_RECORD_WRITE_BUF_SIZE: usize = 4096;

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

async fn run_connected_loop<'a, N>(
    client: &mut Client<'a, N, BumpBuffer<'a>, 4, 2, 2, 0>,
    receiver: &app_bus::MqttCommandReceiver,
    mut deferred_reading: Option<RadioReading>,
) -> Option<RadioReading>
where
    N: AsyncRead + AsyncWrite,
{
    info!("MQTT: Ready");
    let mut last_activity = Instant::now();

    loop {
        let Some(work) = pending_or_next_work(&mut deferred_reading, receiver).await else {
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

                match publish::publish_reading(client, reading).await {
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
                if publish::ping(client).await.is_err() {
                    break; // Exit inner loop to reconnect
                }

                last_activity = Instant::now();
            }
        }
    }

    deferred_reading
}

#[embassy_executor::task]
#[allow(
    clippy::expect_used,
    clippy::large_stack_arrays,
    clippy::too_many_lines
)]
pub async fn mqtt_task(
    network_stack: embassy_net::Stack<'static>,
    receiver: app_bus::MqttCommandReceiver,
) {
    let mut backoff_secs = MQTT_MIN_BACKOFF_SECS;
    let mut deferred_reading: Option<RadioReading> = None;

    // TODO: Persist MQTT sessions
    loop {
        info!(
            "MQTT: Opening new session (tls={}, backoff={}s, deferred_reading={})",
            MQTT_USE_TLS,
            backoff_secs,
            deferred_reading.is_some()
        );

        // Create buffers on the stack for this session
        let mut rx_buf = [0u8; 1024];
        let mut tx_buf = [0u8; 1024];
        let mut mqtt_buf = [0u8; 1024];
        let mut mqtt_buffer = BumpBuffer::new(&mut mqtt_buf);

        if MQTT_USE_TLS {
            let mut tls_read_buf = [0u8; MQTT_TLS_RECORD_READ_BUF_SIZE];
            let mut tls_write_buf = [0u8; MQTT_TLS_RECORD_WRITE_BUF_SIZE];
            let mut client = TlsClient::new(&mut mqtt_buffer);

            if session::establish_mqtt_session_tls(
                network_stack,
                &mut rx_buf,
                &mut tx_buf,
                &mut tls_read_buf,
                &mut tls_write_buf,
                &mut client,
            )
            .await
            .is_err()
            {
                info!(
                    "MQTT: Session setup failed (tls=true), retrying in {}s",
                    backoff_secs
                );
                Timer::after(Duration::from_secs(backoff_secs)).await;
                backoff_secs = next_mqtt_backoff_secs(backoff_secs);
                continue;
            }

            backoff_secs = MQTT_MIN_BACKOFF_SECS;
            deferred_reading = run_connected_loop(&mut client, &receiver, deferred_reading).await;
        } else {
            let mut client = PlainClient::new(&mut mqtt_buffer);
            if session::establish_mqtt_session_plain(
                network_stack,
                &mut rx_buf,
                &mut tx_buf,
                &mut client,
            )
            .await
            .is_err()
            {
                info!(
                    "MQTT: Session setup failed (tls=false), retrying in {}s",
                    backoff_secs
                );
                Timer::after(Duration::from_secs(backoff_secs)).await;
                backoff_secs = next_mqtt_backoff_secs(backoff_secs);
                continue;
            }

            backoff_secs = MQTT_MIN_BACKOFF_SECS;
            deferred_reading = run_connected_loop(&mut client, &receiver, deferred_reading).await;
        }

        // Backoff before reconnecting
        info!(
            "MQTT: Connection lost or reconnect requested, retrying in {}s (deferred_reading={})",
            backoff_secs,
            deferred_reading.is_some()
        );
        Timer::after(Duration::from_secs(backoff_secs)).await;
        backoff_secs = next_mqtt_backoff_secs(backoff_secs);
    }
}
