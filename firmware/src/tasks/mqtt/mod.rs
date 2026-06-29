use core::time::Duration as CoreDuration;

use app_core::runtime_policy::{
    MQTT_MIN_BACKOFF_SECS, next_mqtt_backoff_secs, should_reconnect_before_publish,
};
use defmt::{debug, info, trace, warn};
use embassy_futures::select::{Either4, select4};
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::{Read as AsyncRead, Write as AsyncWrite};
use embedded_tls::{Aes128GcmSha256, TlsConnection};
use ota_core::{
    Ed25519ManifestVerifier, OtaManifest, OtaManifestDeliveryAction, OtaState,
    classify_ota_manifest_delivery, is_mqtt_allowed, is_ota_manifest_payload_candidate,
};
use rust_mqtt::buffer::BumpBuffer;
use rust_mqtt::client::Client;
use rust_mqtt::client::event::{Event, Publish};

use crate::app_bus::{self, AppCommand};
use crate::messages::RadioReading;
use crate::power;
use crate::secrets::MQTT_USE_TLS;
use crate::tls_workspace::TlsWorkspaceGuard;

#[cfg(feature = "release-ota")]
pub(super) fn ota_manifest_verifier() -> Ed25519ManifestVerifier {
    Ed25519ManifestVerifier::release_ota()
}

#[cfg(not(feature = "release-ota"))]
pub(super) fn ota_manifest_verifier() -> Ed25519ManifestVerifier {
    Ed25519ManifestVerifier::dev_test()
}

mod publish;
mod session;

/// Resolve the action for an incoming OTA manifest
#[must_use]
pub(super) fn resolve_ota_manifest_action(
    ota_state: OtaState,
    retained: bool,
    manifest_bytes: &app_bus::OtaManifestBytes,
) -> OtaManifestDeliveryAction {
    let action = classify_ota_manifest_delivery(ota_state, retained);

    if matches!(action, OtaManifestDeliveryAction::ClearRetainedOnly)
        && let Ok(incoming) =
            OtaManifest::parse_and_verify(manifest_bytes, &ota_manifest_verifier())
        && incoming.force
    {
        info!(
            "MQTT: Received retained OTA manifest with force=true during pending confirmation, forwarding"
        );
        OtaManifestDeliveryAction::ForwardAndClearRetained
    } else {
        action
    }
}

type PlainClient<'a> = Client<'a, TcpSocket<'a>, BumpBuffer<'a>, 4, 2, 2, 0>;
type TlsSocket<'a> = TlsConnection<'a, TcpSocket<'a>, Aes128GcmSha256>;
type TlsClient<'a> = Client<'a, TlsSocket<'a>, BumpBuffer<'a>, 4, 2, 2, 0>;

enum ReadingOutcome {
    Continue,
    Reconnect(RadioReading),
}

async fn handle_reading<'a, N>(
    client: &mut Client<'a, N, BumpBuffer<'a>, 4, 2, 2, 0>,
    receiver: &app_bus::MqttCommandReceiver,
    reading: RadioReading,
    last_activity: &mut Instant,
    ota_state: OtaState,
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
            if ota_state == OtaState::Inactive {
                info!("MQTT: Publish confirmed, checking deep sleep policy");
                let time_since_measurement = Instant::now() - reading.received_at;
                power::maybe_sleep_after_publish(
                    receiver.is_empty(),
                    CoreDuration::from_secs(time_since_measurement.as_secs()),
                    ota_state,
                );
            } else {
                info!("MQTT: Publish confirmed, OTA active, skipping deep sleep policy");
            }
            *last_activity = Instant::now();
            ReadingOutcome::Continue
        }
        publish::PublishOutcome::Dropped => ReadingOutcome::Continue,
        publish::PublishOutcome::Reconnect(reading) => ReadingOutcome::Reconnect(reading),
    }
}

struct IncomingOtaManifest {
    bytes: app_bus::OtaManifestBytes,
    retained: bool,
}

fn copy_ota_manifest(publish: &Publish<'_, 0>, ota_topic: &str) -> Option<IncomingOtaManifest> {
    if publish.topic.as_ref().as_ref() != ota_topic {
        trace!(
            "MQTT: Ignoring incoming publish on {}",
            publish.topic.as_ref().as_ref()
        );
        return None;
    }

    let payload = publish.message.as_bytes();
    if !is_ota_manifest_payload_candidate(payload.len()) {
        info!("MQTT: Ignoring empty OTA command payload (retained clear)");
        return None;
    }

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

    Some(IncomingOtaManifest {
        bytes: owned,
        retained: publish.retain,
    })
}

async fn handle_mqtt_event(
    event: Event<'_, 0>,
    ota_topic: &str,
    ota_sender: &app_bus::OtaCommandSender,
    ota_state: OtaState,
) -> bool {
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
        Event::Suback(_)
        | Event::Unsuback(_)
        | Event::Ignored
        | Event::Duplicate
        | Event::PublishRejected(_)
        | Event::PublishReceived(_)
        | Event::PublishReleased(_)
        | Event::PublishComplete(_) => None,
    };

    if let Some(manifest) = manifest {
        let action = resolve_ota_manifest_action(ota_state, manifest.retained, &manifest.bytes);

        match action {
            OtaManifestDeliveryAction::ForwardOnly => {
                info!("MQTT: Received live OTA manifest command, handing off to OTA task");
                ota_sender.send(manifest.bytes).await;
                false
            }
            OtaManifestDeliveryAction::ForwardAndClearRetained => {
                info!(
                    "MQTT: Received retained OTA manifest command, handing off once and clearing retained copy"
                );
                ota_sender.send(manifest.bytes).await;
                true
            }
            OtaManifestDeliveryAction::ClearRetainedOnly => {
                info!(
                    "MQTT: Clearing retained OTA manifest during pending confirmation without re-running OTA"
                );
                true
            }
        }
    } else {
        false
    }
}

#[allow(clippy::too_many_lines)]
async fn run_connected_loop<'a, N>(
    client: &mut Client<'a, N, BumpBuffer<'a>, 4, 2, 2, 0>,
    receiver: &app_bus::MqttCommandReceiver,
    ota_sender: &app_bus::OtaCommandSender,
    ota_state_receiver: &mut app_bus::OtaStateReceiver,
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
            let ota_state = ota_state_receiver.try_get().unwrap_or(OtaState::Inactive);
            match handle_reading(client, receiver, reading, &mut last_activity, ota_state).await {
                ReadingOutcome::Continue => continue,
                ReadingOutcome::Reconnect(reading) => {
                    deferred_reading = Some(reading);
                    break;
                }
            }
        }

        let timeout = Duration::from_secs(PING_INTERVAL_SECS);
        match select4(
            receiver.receive(),
            client.poll_header(),
            Timer::after(timeout),
            ota_state_receiver.changed(),
        )
        .await
        {
            Either4::First(command) => match command {
                AppCommand::PublishTelemetry(reading) => {
                    let ota_state = ota_state_receiver.try_get().unwrap_or(OtaState::Inactive);
                    match handle_reading(client, receiver, reading, &mut last_activity, ota_state)
                        .await
                    {
                        ReadingOutcome::Continue => {}
                        ReadingOutcome::Reconnect(reading) => {
                            deferred_reading = Some(reading);
                            break;
                        }
                    }
                }
                AppCommand::OtaConfirmed => {
                    info!("MQTT: publishing OTA confirmed status");
                    if publish::publish_ota_confirmed(client).await.is_err() {
                        warn!("MQTT: failed to publish OTA confirmation, reconnecting");
                        break;
                    }
                    last_activity = Instant::now();
                }
                AppCommand::ApplySettings { .. } => {
                    trace!("MQTT: Ignoring non-telemetry command");
                }
            },
            Either4::Second(header) => {
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
                        let clear_retained = handle_mqtt_event(
                            event,
                            ota_topic.as_str(),
                            ota_sender,
                            ota_state_receiver.try_get().unwrap_or(OtaState::Inactive),
                        )
                        .await;
                        if clear_retained {
                            publish::clear_ota_retained(client, ota_topic.as_str()).await;
                        }
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
            Either4::Third(()) => {
                if publish::ping(client).await.is_err() {
                    break;
                }

                last_activity = Instant::now();
            }
            Either4::Fourth(state) => {
                if !is_mqtt_allowed(state) {
                    info!("MQTT: OTA state changed; disconnecting");
                    break;
                }
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
    mut ota_state_receiver: app_bus::OtaStateReceiver,
    mqtt_health_sender: app_bus::MqttHealthSender,
) {
    let mut backoff_secs = MQTT_MIN_BACKOFF_SECS;
    let mut deferred_reading: Option<RadioReading> = None;

    // TODO: Persist MQTT sessions
    mqtt_health_sender.send(app_bus::MqttHealth::Disconnected);
    loop {
        while !is_mqtt_allowed(ota_state_receiver.try_get().unwrap_or(OtaState::Inactive)) {
            info!("MQTT: OTA active; standing down");
            ota_state_receiver.changed().await;
        }
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
            let Some(mut tls_workspace) = TlsWorkspaceGuard::try_acquire() else {
                warn!("MQTT: TLS workspace busy, retrying in {}s", backoff_secs);
                Timer::after(Duration::from_secs(backoff_secs)).await;
                backoff_secs = next_mqtt_backoff_secs(backoff_secs);
                continue;
            };
            let (tls_read_buf, tls_write_buf) = tls_workspace.buffers();
            let mut client = TlsClient::new(&mut mqtt_buffer);

            if session::establish_mqtt_session_tls(
                network_stack,
                &mut rx_buf,
                &mut tx_buf,
                tls_read_buf,
                tls_write_buf,
                &mut client,
                &ota_sender,
                ota_state_receiver.try_get().unwrap_or(OtaState::Inactive),
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
            mqtt_health_sender.send(app_bus::MqttHealth::HeartbeatPublished);
            deferred_reading = run_connected_loop(
                &mut client,
                &receiver,
                &ota_sender,
                &mut ota_state_receiver,
                deferred_reading,
            )
            .await;
        } else {
            let mut client = PlainClient::new(&mut mqtt_buffer);
            if session::establish_mqtt_session_plain(
                network_stack,
                &mut rx_buf,
                &mut tx_buf,
                &mut client,
                &ota_sender,
                ota_state_receiver.try_get().unwrap_or(OtaState::Inactive),
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
            mqtt_health_sender.send(app_bus::MqttHealth::HeartbeatPublished);
            deferred_reading = run_connected_loop(
                &mut client,
                &receiver,
                &ota_sender,
                &mut ota_state_receiver,
                deferred_reading,
            )
            .await;
        }

        mqtt_health_sender.send(app_bus::MqttHealth::Disconnected);
        info!(
            "MQTT: Connection lost or reconnect requested, retrying in {}s (deferred_reading={})",
            backoff_secs,
            deferred_reading.is_some()
        );
        Timer::after(Duration::from_secs(backoff_secs)).await;
        backoff_secs = next_mqtt_backoff_secs(backoff_secs);
    }
}
