//! Network setup and WiFi connection management

extern crate alloc;
use alloc::string::String;

use defmt::{info, warn};
use embassy_executor::Spawner;
use embassy_net::{Runner, StackResources};
use embassy_time::{Duration, Timer};
use esp_radio::wifi::{self, ClientConfig, ModeConfig, WifiController, WifiDevice};
use static_cell::StaticCell;

use crate::secrets::{WIFI_PASSWORD, WIFI_SSID};

/// Initialize WiFi and return the network stack
/// 
/// This function:
/// 1. Creates the WiFi controller and interfaces
/// 2. Sets up the network stack with DHCP
/// 3. Spawns the network runner task
/// 4. Connects to WiFi and waits for DHCP
/// 
/// Returns a static reference to the network stack for use in other tasks.
pub async fn setup_wifi(
    radio_controller: &'static mut esp_radio::Controller<'static>,
    wifi_peripheral: esp_hal::peripherals::WIFI<'static>,
    spawner: &Spawner,
) -> &'static embassy_net::Stack<'static> {
    info!("Setting up WiFi...");
    
    let wifi_config = wifi::Config::default();
    let (mut wifi_controller, interfaces) =
        wifi::new(radio_controller, wifi_peripheral, wifi_config).unwrap();

    // Create network stack using the STA interface
    static STACK_RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    let resources = STACK_RESOURCES.init(StackResources::new());

    let (stack, runner) = embassy_net::new(
        interfaces.sta,
        embassy_net::Config::dhcpv4(Default::default()),
        resources,
        1234u64, // Random seed
    );

    // Store stack in static for tasks
    static STACK: StaticCell<embassy_net::Stack<'static>> = StaticCell::new();
    let stack = STACK.init(stack);

    // Spawn network runner task
    spawner.spawn(net_task(runner)).unwrap();

    // Configure WiFi as station
    let client_config = ClientConfig::default()
        .with_ssid(String::from(WIFI_SSID))
        .with_password(String::from(WIFI_PASSWORD));
    wifi_controller
        .set_config(&ModeConfig::Client(client_config))
        .unwrap();

    // Start WiFi
    info!("Starting WiFi...");
    wifi_controller.start_async().await.unwrap();

    // Connect to WiFi
    info!("Connecting to WiFi SSID: {}", WIFI_SSID);
    loop {
        match wifi_controller.connect_async().await {
            Ok(()) => {
                info!("WiFi connected!");
                break;
            }
            Err(e) => {
                warn!(
                    "WiFi connection failed: {:?}, retrying...",
                    defmt::Debug2Format(&e)
                );
                Timer::after(Duration::from_secs(1)).await;
            }
        }
    }

    // Wait for DHCP
    info!("Waiting for DHCP...");
    stack.wait_config_up().await;
    if let Some(config) = stack.config_v4() {
        info!("Got IP: {}", defmt::Debug2Format(&config.address));
    }

    // Spawn WiFi connection monitor task
    spawner.spawn(wifi_connection_task(wifi_controller)).unwrap();

    stack
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await;
}

#[embassy_executor::task]
async fn wifi_connection_task(mut controller: WifiController<'static>) {
    info!("WiFi connection monitor started");
    
    loop {
        // Check if still connected
        match controller.is_connected() {
            Ok(true) => {
                // Still connected, all good
            }
            Ok(false) | Err(_) => {
                warn!("WiFi disconnected, reconnecting...");
                
                loop {
                    match controller.connect_async().await {
                        Ok(()) => {
                            info!("WiFi reconnected!");
                            break;
                        }
                        Err(e) => {
                            warn!(
                                "WiFi reconnection failed: {:?}, retrying in 2s...",
                                defmt::Debug2Format(&e)
                            );
                            Timer::after(Duration::from_secs(2)).await;
                        }
                    }
                }
            }
        }
        
        // Check connection status periodically
        Timer::after(Duration::from_secs(5)).await;
    }
}
