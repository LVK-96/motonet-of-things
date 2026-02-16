//! Network setup and `WiFi` connection management

extern crate alloc;
use alloc::string::String;
use core::sync::atomic::{AtomicBool, Ordering};

use defmt::{info, warn};
use embassy_executor::Spawner;
use embassy_net::{Ipv4Address, Ipv4Cidr, Runner, StackResources, StaticConfigV4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
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

static CONFIG_UP_SEEN: AtomicBool = AtomicBool::new(false);
// embassy-net uses a single state waker for config waits; serialize callers here.
static CONFIG_UP_WAIT_LOCK: Mutex<CriticalSectionRawMutex, ()> = Mutex::new(());

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

/// Initialize the Wi-Fi network stack and spawn the stack runner.
///
/// This does not block on Wi-Fi association. A dedicated supervisor task
/// handles connect/reconnect loops so the rest of startup can proceed.
///
/// # Panics
///
/// Panics if Wi-Fi controller creation fails or if task spawning fails.
#[allow(clippy::expect_used)]
pub fn setup_network_stack(
    controller: &'static mut Controller<'static>,
    wifi_device: WIFI<'static>,
    spawner: &Spawner,
) -> (embassy_net::Stack<'static>, WifiController<'static>) {
    // Create network stack using the STA interface
    static STACK_RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();

    info!("NET[{}]: Setting up WiFi stack", power::wake_reason_class());

    let wifi_config = wifi::Config::default();
    let (wifi_controller, interfaces) =
        wifi::new(controller, wifi_device, wifi_config).expect("Failed to create WiFi controller");

    let resources = STACK_RESOURCES.init(StackResources::new());
    let stack_config = build_stack_config();

    let (stack, runner) = embassy_net::new(
        interfaces.sta,
        stack_config,
        resources,
        1234u64, // Random seed
    );

    spawner
        .spawn(net_task(runner))
        .expect("Failed to spawn network task");
    info!(
        "NET[{}]: Stack runner spawned; WiFi association handled by supervisor",
        power::wake_reason_class()
    );

    (stack, wifi_controller)
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn connect_until_associated(controller: &mut WifiController<'static>) {
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

        if let Err(e) = controller.set_config(&ModeConfig::Client(client_config)) {
            warn!(
                "NET[{}]: WiFi config failed: {:?}, retrying...",
                power::wake_reason_class(),
                defmt::Debug2Format(&e)
            );
            Timer::after(Duration::from_secs(1)).await;
            continue;
        }

        let wifi_start_at = Instant::now();
        match controller.start_async().await {
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
        match controller.connect_async().await {
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
}

pub(crate) async fn wait_for_ipv4_config(stack: embassy_net::Stack<'static>) {
    let config_up_at = Instant::now();
    info!(
        "NET[{}]: Waiting for IPv4 config",
        power::wake_reason_class()
    );
    wait_for_config_up(stack).await;
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
}

pub async fn wait_for_config_up(stack: embassy_net::Stack<'static>) {
    if CONFIG_UP_SEEN.load(Ordering::Relaxed) && stack.is_config_up() {
        return;
    }

    let _guard = CONFIG_UP_WAIT_LOCK.lock().await;
    if stack.is_config_up() {
        CONFIG_UP_SEEN.store(true, Ordering::Relaxed);
        return;
    }

    stack.wait_config_up().await;
    CONFIG_UP_SEEN.store(true, Ordering::Relaxed);
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await;
}

pub(crate) async fn reconnect_until_connected(controller: &mut WifiController<'static>) {
    loop {
        let reconnect_at = Instant::now();
        match controller.connect_async().await {
            Ok(()) => {
                info!(
                    "NET[{}]: WiFi reconnected in {}ms",
                    power::wake_reason_class(),
                    reconnect_at.elapsed().as_millis()
                );
                return;
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
