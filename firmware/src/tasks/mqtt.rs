use core::fmt::Write;

use defmt::{debug, info, trace, warn};
use embassy_futures::select::{Either, select};
use embassy_net::Ipv4Address;
use embassy_net::tcp::TcpSocket;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Receiver;
use embassy_time::{Duration, Instant, Timer};
use rust_mqtt::Bytes;
use rust_mqtt::buffer::BumpBuffer;
use rust_mqtt::client::Client;
use rust_mqtt::client::options::{ConnectOptions, PublicationOptions, WillOptions};
use rust_mqtt::config::{KeepAlive, SessionExpiryInterval};
use rust_mqtt::types::{MqttBinary, MqttString, QoS, TopicName};

use crate::messages::RadioReading;
use crate::secrets::{MQTT_BROKER_IP, MQTT_BROKER_PORT, MQTT_CLIENT_ID};

/// Sets up the MQTT client, including TCP connection and MQTT broker handshake.
async fn establish_mqtt_session<'a>(
    network_stack: embassy_net::Stack<'static>,
    rx_buffer: &'a mut [u8],
    tx_buffer: &'a mut [u8],
    client: &mut Client<'a, TcpSocket<'a>, BumpBuffer<'a>, 4, 2, 2>,
) -> Result<(), ()> {
    // Wait for network to be up
    network_stack.wait_config_up().await;

    info!("MQTT: Connecting to broker...");

    let mut socket = TcpSocket::new(network_stack, rx_buffer, tx_buffer);
    socket.set_timeout(Some(Duration::from_secs(120)));
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

    if let Err(e) = socket.connect(broker_addr).await {
        warn!("MQTT: TCP connect failed: {:?}", e);
        return Err(());
    }

    info!("MQTT: TCP connected");

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

    if let Err(e) = client.connect(socket, &connect_options, client_id).await {
        warn!("MQTT: Broker connect failed: {:?}", defmt::Debug2Format(&e));
        return Err(());
    }

    info!("MQTT: Connected to broker!");
    unsafe { client.buffer().reset() };

    // Publish "online" status message
    let status_topic = MqttString::try_from("sensors/rubicson/status").map_err(|_| ())?;
    let status_topic_name = unsafe { TopicName::new_unchecked(status_topic) };
    let online_options = PublicationOptions {
        retain: true,
        topic: status_topic_name,
        qos: QoS::AtMostOnce,
    };
    if let Err(e) = client
        .publish(&online_options, Bytes::from(b"online" as &[u8]))
        .await
    {
        warn!(
            "MQTT: Failed to publish online status: {:?}",
            defmt::Debug2Format(&e)
        );
        return Err(());
    }

    info!("MQTT: Published online status");
    Ok(())
}

#[embassy_executor::task]
#[allow(clippy::expect_used, clippy::too_many_lines)]
pub async fn mqtt_task(
    network_stack: embassy_net::Stack<'static>,
    receiver: Receiver<'static, CriticalSectionRawMutex, RadioReading, 16>,
) {
    // Backoff parameters
    const MIN_BACKOFF_SECS: u64 = 1;
    const MAX_BACKOFF_SECS: u64 = 60;
    const PING_INTERVAL_SECS: u64 = 90; // Ping every 90s (keepalive is 120s)
    let mut backoff_secs = MIN_BACKOFF_SECS;

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
        let mut last_activity = Instant::now();

        loop {
            // Wait for either a reading or ping timeout
            let timeout = Duration::from_secs(PING_INTERVAL_SECS);

            match select(receiver.receive(), Timer::after(timeout)).await {
                Either::First(reading) => {
                    // Got a reading - publish it
                    let time_since_last = last_activity.elapsed().as_secs();
                    debug!("MQTT: Got reading after {}s idle", time_since_last);

                    // Send a ping first if we've been idle for a while
                    if time_since_last > 10 {
                        debug!(
                            "MQTT: Sending ping before publish (idle {}s)",
                            time_since_last
                        );
                        unsafe { client.buffer().reset() };
                        if let Err(e) = client.ping().await {
                            warn!(
                                "MQTT: Pre-publish ping failed: {:?}, reconnecting...",
                                defmt::Debug2Format(&e)
                            );
                            break;
                        }
                        // Small delay after ping
                        Timer::after(Duration::from_millis(100)).await;
                    }

                    last_activity = Instant::now();

                    // Reset buffer before publish
                    unsafe { client.buffer().reset() };

                    // Format topic: sensors/rubicson/{id}/temperature
                    let mut topic: heapless::String<64> = heapless::String::new();
                    if write!(topic, "sensors/rubicson/{}/temperature", reading.inner.id).is_err() {
                        continue;
                    }

                    // Format payload: id={id},ch={channel},temp={temp},batt={ok/low},rssi={rssi},snr={snr}
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
                        continue;
                    }

                    trace!(
                        "MQTT: Publishing to {} : {}",
                        topic.as_str(),
                        payload.as_str()
                    );

                    // Create TopicName from the topic string
                    let topic_name = match MqttString::from_slice(topic.as_str()) {
                        Ok(s) => unsafe { TopicName::new_unchecked(s) },
                        Err(_) => continue,
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
                        break; // Break inner loop to reconnect
                    }

                    debug!("MQTT: Publish successful!");
                }
                Either::Second(()) => {
                    // Ping timeout - send keepalive
                    debug!("MQTT: Sending periodic ping");
                    unsafe { client.buffer().reset() };

                    if let Err(e) = client.ping().await {
                        warn!(
                            "MQTT: Ping failed: {:?}, reconnecting...",
                            defmt::Debug2Format(&e)
                        );
                        break;
                    }
                    debug!("MQTT: Ping sent successfully");
                    last_activity = Instant::now();
                }
            }
        }

        // Backoff before reconnecting
        Timer::after(Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
    }
}
