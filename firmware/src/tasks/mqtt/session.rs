use defmt::{info, warn};
use embassy_net::Ipv4Address;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant};
use embedded_io_async::{Read as AsyncRead, Write as AsyncWrite};
use embedded_tls::pki::CertVerifier;
use embedded_tls::{
    Aes128GcmSha256, Certificate, CryptoProvider, MaxFragmentLength, TlsConfig, TlsConnection,
    TlsContext, TlsError,
};
use rand_core::RngCore;
use rust_mqtt::Bytes;
use rust_mqtt::buffer::BumpBuffer;
use rust_mqtt::client::Client;
use rust_mqtt::client::options::{ConnectOptions, PublicationOptions, WillOptions};
use rust_mqtt::config::{KeepAlive, SessionExpiryInterval};
use rust_mqtt::types::{MqttBinary, MqttString, QoS, TopicName};

use crate::network;
use crate::power;
use crate::secrets::{
    MQTT_BROKER_HOSTNAME, MQTT_BROKER_IP, MQTT_BROKER_PORT, MQTT_CLIENT_ID, MQTT_PASSWORD,
    MQTT_TLS_CA_CERT_DER, MQTT_TLS_FALLBACK_UNIX_TIME_SECS, MQTT_USERNAME,
};
use crate::time_sync::TIME_WATCH;

use super::{PlainClient, TlsClient};

const MQTT_SOCKET_TIMEOUT_SECS: u64 = 30;
const STATUS_TOPIC: &str = "sensors/rubicson/status";
const MQTT_TLS_CERT_VERIFY_BUF_SIZE: usize = 4096;

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
    let user_name = MQTT_USERNAME
        .map(|name| {
            MqttString::try_from(name).map_err(|_| {
                warn!("MQTT: Invalid username in secrets (not valid MQTT UTF-8 string)");
            })
        })
        .transpose()?;
    let password = MQTT_PASSWORD
        .map(|password| {
            MqttBinary::try_from(password.as_bytes()).map_err(|_| {
                warn!("MQTT: Invalid password in secrets (binary conversion failed)");
            })
        })
        .transpose()?;

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
        user_name,
        password,
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

async fn connect_broker_and_publish_online<'a, N>(
    client: &mut Client<'a, N, BumpBuffer<'a>, 4, 2, 2>,
    transport: N,
) -> Result<(), ()>
where
    N: AsyncRead + AsyncWrite,
{
    let connect_options = connect_options()?;
    let client_id = MqttString::try_from(MQTT_CLIENT_ID).map_or_else(
        |_| {
            warn!("MQTT: Invalid client_id in secrets; omitting client_id in CONNECT");
            None
        },
        Some,
    );
    info!(
        "MQTT[{}]: Sending CONNECT (client_id_set={}, username_set={}, password_set={})",
        power::wake_reason_class(),
        client_id.is_some(),
        MQTT_USERNAME.is_some(),
        MQTT_PASSWORD.is_some()
    );
    let mqtt_connect_at = Instant::now();

    let session_present = client
        .connect(transport, &connect_options, client_id)
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

/// Sets up the MQTT client over plain TCP.
pub(super) async fn establish_mqtt_session_plain<'a>(
    network_stack: embassy_net::Stack<'static>,
    rx_buffer: &'a mut [u8],
    tx_buffer: &'a mut [u8],
    client: &mut PlainClient<'a>,
) -> Result<(), ()> {
    info!("MQTT: Establishing plaintext MQTT session");
    let socket = connect_tcp(network_stack, rx_buffer, tx_buffer).await?;
    connect_broker_and_publish_online(client, socket).await
}

struct MqttTlsClock;

impl embedded_tls::TlsClock for MqttTlsClock {
    fn now() -> Option<u64> {
        let now = TIME_WATCH
            .anon_receiver()
            .try_get()
            .flatten()
            .map_or(MQTT_TLS_FALLBACK_UNIX_TIME_SECS, |time_ref| {
                time_ref.now_unix_secs()
            });
        Some(now)
    }
}

struct MqttTlsRng(esp_hal::rng::Rng);

impl MqttTlsRng {
    fn new() -> Self {
        Self(esp_hal::rng::Rng::new())
    }
}

impl RngCore for MqttTlsRng {
    fn next_u32(&mut self) -> u32 {
        self.0.next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        self.0.next_u64()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.fill_bytes(dest);
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.0.try_fill_bytes(dest)
    }
}

impl rand_core::CryptoRng for MqttTlsRng {}

struct MqttTlsProvider {
    rng: MqttTlsRng,
    verifier: CertVerifier<Aes128GcmSha256, MqttTlsClock, MQTT_TLS_CERT_VERIFY_BUF_SIZE>,
}

impl MqttTlsProvider {
    fn new() -> Self {
        Self {
            rng: MqttTlsRng::new(),
            verifier: CertVerifier::new(),
        }
    }
}

impl CryptoProvider for MqttTlsProvider {
    type CipherSuite = Aes128GcmSha256;
    type Signature = [u8; 0];

    fn rng(&mut self) -> impl embedded_tls::CryptoRngCore {
        &mut self.rng
    }

    fn verifier(
        &mut self,
    ) -> Result<&mut impl embedded_tls::TlsVerifier<Self::CipherSuite>, TlsError> {
        Ok(&mut self.verifier)
    }
}

/// Sets up the MQTT client over TLS using server certificate validation.
pub(super) async fn establish_mqtt_session_tls<'a>(
    network_stack: embassy_net::Stack<'static>,
    rx_buffer: &'a mut [u8],
    tx_buffer: &'a mut [u8],
    tls_record_read_buffer: &'a mut [u8],
    tls_record_write_buffer: &'a mut [u8],
    client: &mut TlsClient<'a>,
) -> Result<(), ()> {
    #[allow(clippy::const_is_empty)]
    if MQTT_TLS_CA_CERT_DER.is_empty() {
        warn!("MQTT[TLS]: CA cert DER is empty, cannot verify broker certificate");
        return Err(());
    }

    info!(
        "MQTT[TLS]: Establishing TLS session (server_name={}, ca_der_len={} bytes)",
        MQTT_BROKER_HOSTNAME,
        MQTT_TLS_CA_CERT_DER.len()
    );
    let socket = connect_tcp(network_stack, rx_buffer, tx_buffer).await?;
    let mut tls_socket =
        TlsConnection::new(socket, tls_record_read_buffer, tls_record_write_buffer);
    let tls_config = TlsConfig::new()
        .with_ca(Certificate::X509(MQTT_TLS_CA_CERT_DER))
        .with_server_name(MQTT_BROKER_HOSTNAME)
        .with_max_fragment_length(MaxFragmentLength::Bits11);

    let tls_open_at = Instant::now();
    if let Err(e) = tls_socket
        .open(TlsContext::new(&tls_config, MqttTlsProvider::new()))
        .await
    {
        warn!(
            "MQTT[TLS]: TLS handshake failed after {}ms: {:?} (server_name={}, ca_der_len={} bytes)",
            tls_open_at.elapsed().as_millis(),
            defmt::Debug2Format(&e),
            MQTT_BROKER_HOSTNAME,
            MQTT_TLS_CA_CERT_DER.len()
        );
        return Err(());
    }

    info!(
        "MQTT[TLS]: TLS handshake complete in {}ms",
        tls_open_at.elapsed().as_millis()
    );
    connect_broker_and_publish_online(client, tls_socket).await
}
