//! Network setup and `WiFi` connection management

extern crate alloc;
use alloc::string::String;

use defmt::{error, info, warn};
use embassy_executor::Spawner;
use embassy_net::{Ipv4Address, Ipv4Cidr, Runner, StackResources, StaticConfigV4};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::peripherals::WIFI;
use esp_radio::{
    Controller,
    wifi::{self, ClientConfig, ModeConfig, WifiController, WifiDevice},
};
use static_cell::StaticCell;

use crate::power;
use crate::secrets::{
    WIFI_BSSID_HINT, WIFI_CHANNEL_HINT, WIFI_DNS1_IP, WIFI_DNS2_IP, WIFI_GATEWAY_IP, WIFI_PASSWORD,
    WIFI_SSID, WIFI_STATIC_IP, WIFI_SUBNET_PREFIX,
};

fn ipv4(raw: [u8; 4]) -> Ipv4Address {
    Ipv4Address::new(raw[0], raw[1], raw[2], raw[3])
}

#[allow(clippy::default_trait_access)]
fn build_stack_config() -> embassy_net::Config {
    WIFI_STATIC_IP.map_or_else(
        || embassy_net::Config::dhcpv4(embassy_net::DhcpConfig::default()),
        |static_ip| {
            let mut static_cfg = StaticConfigV4 {
                address: Ipv4Cidr::new(ipv4(static_ip), WIFI_SUBNET_PREFIX),
                gateway: WIFI_GATEWAY_IP.map(ipv4),
                dns_servers: Default::default(),
            };
            if let Some(dns1) = WIFI_DNS1_IP {
                let _ = static_cfg.dns_servers.push(ipv4(dns1));
            }
            if let Some(dns2) = WIFI_DNS2_IP {
                let _ = static_cfg.dns_servers.push(ipv4(dns2));
            }
            embassy_net::Config::ipv4_static(static_cfg)
        },
    )
}

fn build_client_config() -> ClientConfig {
    let mut client_config = ClientConfig::default()
        .with_ssid(String::from(WIFI_SSID))
        .with_password(String::from(WIFI_PASSWORD));

    if let Some(channel) = WIFI_CHANNEL_HINT {
        client_config = client_config.with_channel(channel);
    }

    if let Some(bssid) = WIFI_BSSID_HINT {
        client_config = client_config.with_bssid(bssid);
    }

    client_config
}

/// Initialize `WiFi` and return the network stack
///
/// This function:
/// 1. Creates the `WiFi` controller and interfaces
/// 2. Sets up the network stack with DHCP
/// 3. Spawns the network runner task
/// 4. Connects to `WiFi` and waits for DHCP
///
/// Returns a static reference to the network stack for use in other tasks.
///
/// # Panics
///
/// Panics if `WiFi` controller creation fails or if task spawning fails.
#[allow(clippy::expect_used, clippy::too_many_lines)]
pub async fn setup_wifi(
    controller: &'static mut Controller<'static>,
    wifi_device: WIFI<'static>,
    spawner: &Spawner,
) -> embassy_net::Stack<'static> {
    // Create network stack using the STA interface
    static STACK_RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();

    info!("NET[{}]: Setting up WiFi", power::wake_reason_class());

    let wifi_config = wifi::Config::default();
    let (mut wifi_controller, interfaces) =
        wifi::new(controller, wifi_device, wifi_config).expect("Failed to create WiFi controller");

    let resources = STACK_RESOURCES.init(StackResources::new());
    let stack_config = build_stack_config();

    let (stack, runner) = embassy_net::new(
        interfaces.sta,
        stack_config,
        resources,
        1234u64, // Random seed
    );

    // Spawn network runner task
    spawner
        .spawn(net_task(runner))
        .expect("Failed to spawn network task");

    // Configure, start, and connect to WiFi with retry
    info!(
        "NET[{}]: Connecting to WiFi SSID {} (channel_hint_set={}, bssid_hint_set={}, static_ip_set={})",
        power::wake_reason_class(),
        WIFI_SSID,
        WIFI_CHANNEL_HINT.is_some(),
        WIFI_BSSID_HINT.is_some(),
        WIFI_STATIC_IP.is_some()
    );
    loop {
        let client_config = build_client_config();

        if let Err(e) = wifi_controller.set_config(&ModeConfig::Client(client_config)) {
            warn!(
                "NET[{}]: WiFi config failed: {:?}, retrying...",
                power::wake_reason_class(),
                defmt::Debug2Format(&e)
            );
            Timer::after(Duration::from_secs(1)).await;
            continue;
        }

        let wifi_start_at = Instant::now();
        match wifi_controller.start_async().await {
            Ok(()) => {
                info!(
                    "NET[{}]: WiFi start complete in {}ms",
                    power::wake_reason_class(),
                    wifi_start_at.elapsed().as_millis()
                );
            }
            Err(e) => {
                warn!(
                    "NET[{}]: WiFi start failed after {}ms: {:?}, retrying...",
                    power::wake_reason_class(),
                    wifi_start_at.elapsed().as_millis(),
                    defmt::Debug2Format(&e)
                );
                Timer::after(Duration::from_secs(1)).await;
                continue;
            }
        }

        let assoc_at = Instant::now();
        match wifi_controller.connect_async().await {
            Ok(()) => {
                info!(
                    "NET[{}]: WiFi associated in {}ms",
                    power::wake_reason_class(),
                    assoc_at.elapsed().as_millis()
                );
                break;
            }
            Err(e) => {
                warn!(
                    "NET[{}]: WiFi association failed after {}ms: {:?}, retrying...",
                    power::wake_reason_class(),
                    assoc_at.elapsed().as_millis(),
                    defmt::Debug2Format(&e)
                );
                Timer::after(Duration::from_secs(1)).await;
            }
        }
    }

    // Wait for DHCP (or static IP stack readiness).
    let config_up_at = Instant::now();
    info!(
        "NET[{}]: Waiting for IPv4 config",
        power::wake_reason_class()
    );
    stack.wait_config_up().await;
    info!(
        "NET[{}]: IPv4 config ready in {}ms",
        power::wake_reason_class(),
        config_up_at.elapsed().as_millis()
    );
    if let Some(config) = stack.config_v4() {
        info!(
            "NET[{}]: Got IP {}",
            power::wake_reason_class(),
            defmt::Debug2Format(&config.address)
        );
    }

    // Spawn WiFi connection monitor task
    if spawner
        .spawn(wifi_connection_task(wifi_controller))
        .is_err()
    {
        error!("Failed to spawn WiFi connection task");
    }

    stack
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await;
}

#[embassy_executor::task]
async fn wifi_connection_task(mut controller: WifiController<'static>) {
    info!(
        "NET[{}]: WiFi connection monitor started",
        power::wake_reason_class()
    );

    loop {
        // Check if still connected
        if !matches!(controller.is_connected(), Ok(true)) {
            warn!(
                "NET[{}]: WiFi disconnected, reconnecting...",
                power::wake_reason_class()
            );

            loop {
                let reconnect_at = Instant::now();
                match controller.connect_async().await {
                    Ok(()) => {
                        info!(
                            "NET[{}]: WiFi reconnected in {}ms",
                            power::wake_reason_class(),
                            reconnect_at.elapsed().as_millis()
                        );
                        break;
                    }
                    Err(e) => {
                        warn!(
                            "NET[{}]: WiFi reconnection failed after {}ms: {:?}, retrying in 2s...",
                            power::wake_reason_class(),
                            reconnect_at.elapsed().as_millis(),
                            defmt::Debug2Format(&e)
                        );
                        Timer::after(Duration::from_secs(2)).await;
                    }
                }
            }
        }

        // Check connection status periodically
        Timer::after(Duration::from_secs(5)).await;
    }
}
