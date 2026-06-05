use core::time::Duration as CoreDuration;

use app_core::runtime_policy::{
    MQTT_MIN_BACKOFF_SECS, next_mqtt_backoff_secs, should_reconnect_before_publish,
};
use defmt::{debug, info, trace, warn};
use embassy_futures::select::{Either3, select3};
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::{Read as AsyncRead, Write as AsyncWrite};
use embedded_tls::{Aes128GcmSha256, TlsConnection};
use rust_mqtt::buffer::BumpBuffer;
use rust_mqtt::client::Client;
use rust_mqtt::client::event::{Event, Publish};

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

enum ReadingOutcome {
    Continue,
    Reconnect(RadioReading),
}

fn telemetry_from_command(command: AppCommand) -> Option<RadioReading> {
    match command {
        AppCommand::PublishTelemetry(reading) => Some(reading),
        AppCommand::ApplySettings { .. } => None,
    }
}

async fn handle_reading<'a, N>(
    client: &mut Client<'a, N, BumpBuffer<'a>, 4, 2, 2, 0>,
    receiver: &app_bus::MqttCommandReceiver,
    reading: RadioReading,
    last_activity: &mut Instant,
) -> ReadingOutcome
where
    N: AsyncRead + AsyncWrite,
{
    let time_since_last = last_activity.elapsed().as_secs();
    debug!("MQTT: Got reading after {}s idle", time_since_last);

    if should_reconnect_before_publish(time_since_last) {
        info!(
            "MQTT: Reconnecting before publish after {}s idle",
            time_since_last
        );
        return ReadingOutcome::Reconnect(reading);
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
            *last_activity = Instant::now();
            ReadingOutcome::Continue
        }
        publish::PublishOutcome::Dropped => ReadingOutcome::Continue,
        publish::PublishOutcome::Reconnect(reading) => ReadingOutcome::Reconnect(reading),
    }
}

fn copy_ota_manifest(
    publish: &Publish<'_, 0>,
    ota_topic: &str,
) -> Option<app_bus::OtaManifestBytes> {
    if publish.topic.as_ref().as_ref() != ota_topic {
        trace!(
            "MQTT: Ignoring incoming publish on {}",
            publish.topic.as_ref().as_ref()
        );
        return None;
    }

    let payload = publish.message.as_bytes();
    if payload.len() > app_bus::OTA_MANIFEST_MAX_BYTES {
        warn!(
            "MQTT: Dropping oversized OTA manifest command ({} bytes, max {})",
            payload.len(),
            app_bus::OTA_MANIFEST_MAX_BYTES
        );
        return None;
    }

    let mut owned = app_bus::OtaManifestBytes::new();
    if owned.extend_from_slice(payload).is_err() {
        warn!("MQTT: Failed to copy OTA manifest command into owned buffer");
        return None;
    }

    Some(owned)
}

async fn handle_mqtt_event(
    event: Event<'_, 0>,
    ota_topic: &str,
    ota_sender: &app_bus::OtaCommandSender,
) {
    let manifest = match event {
        Event::Publish(publish) => copy_ota_manifest(&publish, ota_topic),
        Event::Pingresp => {
            debug!("MQTT: Received PINGRESP");
            None
        }
        Event::PublishAcknowledged(_) => {
            debug!("MQTT: Publish acknowledged by broker");
            None
        }
        Event::Suback(_) | Event::Unsuback(_) | Event::Ignored | Event::Duplicate => None,
        Event::PublishRejected(_)
        | Event::PublishReceived(_)
        | Event::PublishReleased(_)
        | Event::PublishComplete(_) => None,
    };

    if let Some(manifest) = manifest {
        info!("MQTT: Received OTA manifest command, handing off to OTA task");
        ota_sender.send(manifest).await;
    }
}

async fn run_connected_loop<'a, N>(
    client: &mut Client<'a, N, BumpBuffer<'a>, 4, 2, 2, 0>,
    receiver: &app_bus::MqttCommandReceiver,
    ota_sender: &app_bus::OtaCommandSender,
    mut deferred_reading: Option<RadioReading>,
) -> Option<RadioReading>
where
    N: AsyncRead + AsyncWrite,
{
    const PING_INTERVAL_SECS: u64 = 90; // Ping every 90s (keepalive is 120s)

    let Ok(ota_topic) = session::ota_cmd_topic() else {
        warn!("MQTT: Cannot build OTA command topic; reconnecting");
        return deferred_reading;
    };

    info!("MQTT: Ready");
    let mut last_activity = Instant::now();

    loop {
        if let Some(reading) = deferred_reading.take() {
            debug!("MQTT: Retrying deferred reading after reconnect");
            match handle_reading(client, receiver, reading, &mut last_activity).await {
                ReadingOutcome::Continue => continue,
                ReadingOutcome::Reconnect(reading) => {
                    deferred_reading = Some(reading);
                    break;
                }
            }
        }

        let timeout = Duration::from_secs(PING_INTERVAL_SECS);
        match select3(
            receiver.receive(),
            client.poll_header(),
            Timer::after(timeout),
        )
        .await
        {
            Either3::First(command) => {
                let Some(reading) = telemetry_from_command(command) else {
                    trace!("MQTT: Ignoring non-telemetry command");
                    continue;
                };
                match handle_reading(client, receiver, reading, &mut last_activity).await {
                    ReadingOutcome::Continue => {}
                    ReadingOutcome::Reconnect(reading) => {
                        deferred_reading = Some(reading);
                        break;
                    }
                }
            }
            Either3::Second(header) => {
                let header = match header {
                    Ok(header) => header,
                    Err(e) => {
                        warn!(
                            "MQTT: Poll header failed: {:?}, reconnecting...",
                            defmt::Debug2Format(&e)
                        );
                        break;
                    }
                };
                match client.poll_body(header).await {
                    Ok(event) => {
                        handle_mqtt_event(event, ota_topic.as_str(), ota_sender).await;
                        unsafe { client.buffer_mut().reset() };
                        last_activity = Instant::now();
                    }
                    Err(e) => {
                        warn!(
                            "MQTT: Poll body failed: {:?}, reconnecting...",
                            defmt::Debug2Format(&e)
                        );
                        break;
                    }
                }
            }
            Either3::Third(()) => {
                if publish::ping(client).await.is_err() {
                    break;
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
    ota_sender: app_bus::OtaCommandSender,
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
        let mut mqtt_buf = [0u8; 2048];
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
                &ota_sender,
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
            deferred_reading =
                run_connected_loop(&mut client, &receiver, &ota_sender, deferred_reading).await;
        } else {
            let mut client = PlainClient::new(&mut mqtt_buffer);
            if session::establish_mqtt_session_plain(
                network_stack,
                &mut rx_buf,
                &mut tx_buf,
                &mut client,
                &ota_sender,
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
            deferred_reading =
                run_connected_loop(&mut client, &receiver, &ota_sender, deferred_reading).await;
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
