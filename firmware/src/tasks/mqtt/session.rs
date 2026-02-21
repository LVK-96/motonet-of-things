use defmt::{info, warn};
use embassy_net::Ipv4Address;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant};
use rust_mqtt::Bytes;
use rust_mqtt::buffer::BumpBuffer;
use rust_mqtt::client::Client;
use rust_mqtt::client::options::{ConnectOptions, PublicationOptions, WillOptions};
use rust_mqtt::config::{KeepAlive, SessionExpiryInterval};
use rust_mqtt::types::{MqttBinary, MqttString, QoS, TopicName};

use crate::network;
use crate::power;
use crate::secrets::{MQTT_BROKER_IP, MQTT_BROKER_PORT, MQTT_CLIENT_ID};

use super::MqttClient;

const MQTT_SOCKET_TIMEOUT_SECS: u64 = 30;
const STATUS_TOPIC: &str = "sensors/rubicson/status";

fn broker_addr() -> (Ipv4Address, u16) {
    (
        Ipv4Address::new(
            MQTT_BROKER_IP[0],
            MQTT_BROKER_IP[1],
            MQTT_BROKER_IP[2],
            MQTT_BROKER_IP[3],
        ),
        MQTT_BROKER_PORT,
    )
}

fn connect_options() -> Result<ConnectOptions<'static>, ()> {
    let lwt = WillOptions {
        will_qos: QoS::AtMostOnce,
        will_retain: true,
        will_topic: MqttString::try_from(STATUS_TOPIC).map_err(|_| ())?,
        will_payload: MqttBinary::try_from(b"offline" as &[u8]).map_err(|_| ())?,
        will_delay_interval: 0,
        is_payload_utf8: true,
        message_expiry_interval: None,
        content_type: None,
        response_topic: None,
        correlation_data: None,
    };

    Ok(ConnectOptions {
        // We intentionally use non-persistent sessions for this publisher task.
        clean_start: true,
        keep_alive: KeepAlive::Seconds(120),
        session_expiry_interval: SessionExpiryInterval::EndOnDisconnect,
        user_name: None,
        password: None,
        will: Some(lwt),
    })
}

async fn connect_tcp<'a>(
    network_stack: embassy_net::Stack<'static>,
    rx_buffer: &'a mut [u8],
    tx_buffer: &'a mut [u8],
) -> Result<TcpSocket<'a>, ()> {
    network::wait_for_config_up(network_stack).await;

    let broker_addr = broker_addr();
    info!(
        "MQTT[{}]: Connecting to broker {}...",
        broker_addr,
        power::wake_reason_class()
    );

    let mut socket = TcpSocket::new(network_stack, rx_buffer, tx_buffer);
    socket.set_timeout(Some(Duration::from_secs(MQTT_SOCKET_TIMEOUT_SECS)));
    socket.set_nagle_enabled(false);

    let tcp_connect_at = Instant::now();
    if let Err(e) = socket.connect(broker_addr).await {
        warn!(
            "MQTT[{}]: failed to connect to broker {} after {}ms: {:?}",
            broker_addr,
            power::wake_reason_class(),
            tcp_connect_at.elapsed().as_millis(),
            e
        );
        return Err(());
    }

    info!(
        "MQTT[{}]: connected to broker {} in {}ms",
        broker_addr,
        power::wake_reason_class(),
        tcp_connect_at.elapsed().as_millis()
    );

    Ok(socket)
}

async fn connect_broker<'a>(client: &mut MqttClient<'a>, socket: TcpSocket<'a>) -> Result<(), ()> {
    let connect_options = connect_options()?;
    let client_id = MqttString::try_from(MQTT_CLIENT_ID).ok();
    let mqtt_connect_at = Instant::now();

    let session_present = client
        .connect(socket, &connect_options, client_id)
        .await
        .map_err(|e| {
            warn!(
                "MQTT[{}]: Broker connect failed after {}ms: {:?}",
                power::wake_reason_class(),
                mqtt_connect_at.elapsed().as_millis(),
                defmt::Debug2Format(&e)
            );
        })?;

    info!(
        "MQTT[{}]: Connected to broker in {}ms (session_present={})",
        power::wake_reason_class(),
        mqtt_connect_at.elapsed().as_millis(),
        session_present
    );
    unsafe { client.buffer().reset() };
    Ok(())
}

async fn publish_online_status(client: &mut MqttClient<'_>) -> Result<(), ()> {
    let status_topic = MqttString::try_from(STATUS_TOPIC).map_err(|_| ())?;
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

/// Sets up the MQTT client, including TCP connection and MQTT broker handshake.
pub(super) async fn establish_mqtt_session<'a>(
    network_stack: embassy_net::Stack<'static>,
    rx_buffer: &'a mut [u8],
    tx_buffer: &'a mut [u8],
    client: &mut Client<'a, TcpSocket<'a>, BumpBuffer<'a>, 4, 2, 2>,
) -> Result<(), ()> {
    let socket = connect_tcp(network_stack, rx_buffer, tx_buffer).await?;
    connect_broker(client, socket).await?;
    publish_online_status(client).await
}
