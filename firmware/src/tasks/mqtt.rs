use core::fmt::Write;

use defmt::{debug, info, trace, warn};
use embassy_futures::select::{Either, select};
use embassy_net::Ipv4Address;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant, Timer};
use rust_mqtt::Bytes;
use rust_mqtt::buffer::BumpBuffer;
use rust_mqtt::client::Client;
use rust_mqtt::client::options::{ConnectOptions, PublicationOptions, WillOptions};
use rust_mqtt::config::{KeepAlive, SessionExpiryInterval};
use rust_mqtt::types::{MqttBinary, MqttString, QoS, TopicName};
use telemetry_core::publish_state::{BeginPublishError, PublishOutcome, PublishPipelineState};

use crate::messages::RadioReading;
use crate::power;
use crate::secrets::{MQTT_BROKER_IP, MQTT_BROKER_PORT, MQTT_CLIENT_ID};
use crate::tasks::TelemetryReceiver;

/// Sets up the MQTT client, including TCP connection and MQTT broker handshake.
async fn establish_mqtt_session<'a>(
    network_stack: embassy_net::Stack<'static>,
    rx_buffer: &'a mut [u8],
    tx_buffer: &'a mut [u8],
    client: &mut Client<'a, TcpSocket<'a>, BumpBuffer<'a>, 4, 2, 2>,
) -> Result<(), ()> {
    const MQTT_SOCKET_TIMEOUT_SECS: u64 = 30;

    // Wait for network to be up
    network_stack.wait_config_up().await;

    info!(
        "MQTT[{}]: Connecting to broker...",
        power::wake_reason_class()
    );

    let mut socket = TcpSocket::new(network_stack, rx_buffer, tx_buffer);
    socket.set_timeout(Some(Duration::from_secs(MQTT_SOCKET_TIMEOUT_SECS)));
    socket.set_nagle_enabled(false);

    let broker_addr = (
        Ipv4Address::new(
            MQTT_BROKER_IP[0],
            MQTT_BROKER_IP[1],
            MQTT_BROKER_IP[2],
            MQTT_BROKER_IP[3],
        ),
        MQTT_BROKER_PORT,
    );

    let tcp_connect_at = Instant::now();
    if let Err(e) = socket.connect(broker_addr).await {
        warn!(
            "MQTT[{}]: TCP connect failed after {}ms: {:?}",
            power::wake_reason_class(),
            tcp_connect_at.elapsed().as_millis(),
            e
        );
        return Err(());
    }

    info!(
        "MQTT[{}]: TCP connected in {}ms",
        power::wake_reason_class(),
        tcp_connect_at.elapsed().as_millis()
    );

    let lwt = WillOptions {
        will_qos: QoS::AtMostOnce,
        will_retain: true,
        will_topic: MqttString::try_from("sensors/rubicson/status").map_err(|_| ())?,
        will_payload: MqttBinary::try_from(b"offline" as &[u8]).map_err(|_| ())?,
        will_delay_interval: 0,
        is_payload_utf8: true,
        message_expiry_interval: None,
        content_type: None,
        response_topic: None,
        correlation_data: None,
    };

    let connect_options = ConnectOptions {
        clean_start: true,
        keep_alive: KeepAlive::Seconds(120),
        session_expiry_interval: SessionExpiryInterval::EndOnDisconnect,
        user_name: None,
        password: None,
        will: Some(lwt),
    };
    let client_id = MqttString::try_from(MQTT_CLIENT_ID).ok();

    let mqtt_connect_at = Instant::now();
    if let Err(e) = client.connect(socket, &connect_options, client_id).await {
        warn!(
            "MQTT[{}]: Broker connect failed after {}ms: {:?}",
            power::wake_reason_class(),
            mqtt_connect_at.elapsed().as_millis(),
            defmt::Debug2Format(&e)
        );
        return Err(());
    }

    info!(
        "MQTT[{}]: Connected to broker in {}ms",
        power::wake_reason_class(),
        mqtt_connect_at.elapsed().as_millis()
    );
    unsafe { client.buffer().reset() };

    // Publish "online" status message
    let status_topic = MqttString::try_from("sensors/rubicson/status").map_err(|_| ())?;
    let status_topic_name = unsafe { TopicName::new_unchecked(status_topic) };
    let online_options = PublicationOptions {
        retain: true,
        topic: status_topic_name,
        qos: QoS::AtMostOnce,
    };
    let online_publish_at = Instant::now();
    if let Err(e) = client
        .publish(&online_options, Bytes::from(b"online" as &[u8]))
        .await
    {
        warn!(
            "MQTT[{}]: Failed to publish online status after {}ms: {:?}",
            power::wake_reason_class(),
            online_publish_at.elapsed().as_millis(),
            defmt::Debug2Format(&e)
        );
        return Err(());
    }

    info!(
        "MQTT[{}]: Published online status in {}ms",
        power::wake_reason_class(),
        online_publish_at.elapsed().as_millis()
    );
    Ok(())
}

#[embassy_executor::task]
#[allow(clippy::expect_used, clippy::too_many_lines)]
pub async fn mqtt_task(network_stack: embassy_net::Stack<'static>, receiver: TelemetryReceiver) {
    // Backoff parameters
    const MIN_BACKOFF_SECS: u64 = 1;
    const MAX_BACKOFF_SECS: u64 = 60;
    const PING_INTERVAL_SECS: u64 = 90; // Ping every 90s (keepalive is 120s)
    // For minute-level sensor cadence, prefer a fresh session when the link has been idle
    // for a while. This avoids a first publish attempt on a stale TCP connection.
    const RECONNECT_BEFORE_PUBLISH_IDLE_SECS: u64 = 20;
    let mut backoff_secs = MIN_BACKOFF_SECS;
    let mut publish_state = PublishPipelineState::<RadioReading>::new();

    loop {
        // Create buffers on the stack for this session
        let mut rx_buf = [0u8; 1024];
        let mut tx_buf = [0u8; 1024];
        let mut mqtt_buf = [0u8; 1024];
        let mut mqtt_buffer = BumpBuffer::new(&mut mqtt_buf);
        let mut client = Client::new(&mut mqtt_buffer);

        if establish_mqtt_session(network_stack, &mut rx_buf, &mut tx_buf, &mut client)
            .await
            .is_err()
        {
            Timer::after(Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
            continue;
        }

        backoff_secs = MIN_BACKOFF_SECS;
        info!("MQTT: Ready");
        publish_state.clear_reconnect_required();
        let mut last_activity = Instant::now();

        loop {
            enum MqttWork {
                Reading(RadioReading),
                KeepAlivePing,
            }

            let work = if let Some(reading) = publish_state.begin_retry() {
                debug!("MQTT: Retrying buffered reading after reconnect");
                MqttWork::Reading(reading)
            } else {
                // Wait for either a reading or ping timeout
                let timeout = Duration::from_secs(PING_INTERVAL_SECS);
                match select(receiver.receive(), Timer::after(timeout)).await {
                    Either::First(reading) => {
                        if let Err(BeginPublishError::Busy) = publish_state.begin_new(reading) {
                            warn!("MQTT: Deferring dequeued reading while retry item is pending");
                            continue;
                        }
                        MqttWork::Reading(reading)
                    }
                    Either::Second(()) => MqttWork::KeepAlivePing,
                }
            };

            match work {
                MqttWork::Reading(reading) => {
                    let time_since_last = last_activity.elapsed().as_secs();
                    debug!("MQTT: Got reading after {}s idle", time_since_last);
                    if time_since_last > RECONNECT_BEFORE_PUBLISH_IDLE_SECS {
                        info!(
                            "MQTT: Reconnecting before publish after {}s idle",
                            time_since_last
                        );
                        publish_state.complete_in_flight(PublishOutcome::RetryLater);
                    } else {
                        unsafe { client.buffer().reset() };

                        let mut topic: heapless::String<64> = heapless::String::new();
                        if write!(topic, "sensors/rubicson/{}/temperature", reading.inner.id)
                            .is_err()
                        {
                            publish_state.complete_in_flight(PublishOutcome::Dropped);
                            warn!("MQTT: Dropping reading due to topic format error");
                            continue;
                        }

                        let mut payload: heapless::String<96> = heapless::String::new();
                        let batt = if reading.inner.battery_ok {
                            "ok"
                        } else {
                            "low"
                        };
                        if write!(
                            payload,
                            "id={},ch={},temp={:.1},batt={},rssi={},snr={}",
                            reading.inner.id,
                            reading.inner.channel,
                            reading.inner.temperature_c,
                            batt,
                            reading.rssi,
                            reading.detection_threshold
                        )
                        .is_err()
                        {
                            publish_state.complete_in_flight(PublishOutcome::Dropped);
                            warn!("MQTT: Dropping reading due to payload format error");
                            continue;
                        }

                        trace!(
                            "MQTT: Publishing to {} : {}",
                            topic.as_str(),
                            payload.as_str()
                        );

                        let topic_name = match MqttString::from_slice(topic.as_str()) {
                            Ok(s) => unsafe { TopicName::new_unchecked(s) },
                            Err(_) => {
                                publish_state.complete_in_flight(PublishOutcome::Dropped);
                                warn!("MQTT: Dropping reading due to invalid topic");
                                continue;
                            }
                        };

                        let pub_options = PublicationOptions {
                            retain: false,
                            topic: topic_name,
                            qos: QoS::AtMostOnce,
                        };

                        if let Err(e) = client
                            .publish(&pub_options, Bytes::from(payload.as_bytes()))
                            .await
                        {
                            warn!(
                                "MQTT: Publish failed: {:?}, reconnecting...",
                                defmt::Debug2Format(&e)
                            );
                            publish_state.complete_in_flight(PublishOutcome::RetryLater);
                        } else {
                            publish_state.complete_in_flight(PublishOutcome::Published);
                            debug!("MQTT: Publish successful!");
                            info!("MQTT: Publish confirmed, checking deep sleep policy");
                            power::maybe_sleep_after_frame();
                            last_activity = Instant::now();
                        }
                    }
                }
                MqttWork::KeepAlivePing => {
                    debug!("MQTT: Sending periodic ping");
                    unsafe { client.buffer().reset() };

                    if let Err(e) = client.ping().await {
                        warn!(
                            "MQTT: Ping failed: {:?}, reconnecting...",
                            defmt::Debug2Format(&e)
                        );
                        publish_state.mark_ping_failed();
                    } else {
                        debug!("MQTT: Ping sent successfully");
                        last_activity = Instant::now();
                    }
                }
            }

            if publish_state.reconnect_required() {
                break;
            }
        }

        // Backoff before reconnecting
        Timer::after(Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
    }
}
