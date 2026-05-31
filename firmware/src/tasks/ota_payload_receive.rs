use crate::network;
use defmt::{info, warn, Debug2Format};
use embassy_time::{Duration};
use embassy_net::{IpListenEndpoint, tcp::TcpSocket};

const OTA_TCP_PORT: u16 = 7777;

#[embassy_executor::task]
pub async fn ota_payload_receive_task(
    network_stack: embassy_net::Stack<'static>
) {
    network::wait_for_config_up(network_stack).await;
    ota_tcp_server(network_stack).await;
}

async fn ota_tcp_server(
    network_stack: embassy_net::Stack<'static>
) {
    let mut rx_buf = [0u8; 4096];
    let mut tx_buf = [0u8; 4096];

    let mut socket = TcpSocket::new(network_stack, &mut rx_buf, &mut tx_buf);
    socket.set_timeout(Some(Duration::from_secs(10)));

    if let Some(config) = network_stack.config_v4() {
        info!("Starting OTA TCP server @ {}:{}", Debug2Format(&config.address), OTA_TCP_PORT);
    }

    loop {
        // Close any existing connection before accepting a new one
        // We will only handle one OTA connecttion at a time
        socket.abort();

        let accept_connection = socket
            .accept(IpListenEndpoint {
                addr: None,
                port: OTA_TCP_PORT,
            })
            .await;

        if let Err(e) = accept_connection {
            warn!("Failed to accept OTA TCP connection: {:?}", Debug2Format(&e));
            continue;
        }

        serve_one_ota_connection(&mut socket).await;
    }
}

async fn serve_one_ota_connection(socket:&mut TcpSocket<'_>) {
    info!("Accepted OTA TCP connection");
    let mut payload_buf = [0u8; 1024];
    loop {
        match socket.read(&mut payload_buf).await {
            Ok(0) => {
                info!("OTA TCP client disconnected");
                break;
            }
            Ok(n) => {
                let payload = &payload_buf[..n];
                // TODO: OTA define OTA payload format and process accordingly
                info!("Received OTA payload chunk: {}", Debug2Format(&payload));
                info!("Received {} bytes of OTA payload", n);
            }
            Err(e) => {
                warn!("Error receiving OTA payload: {:?}", e);
                break;
            }
        }
    }
}
