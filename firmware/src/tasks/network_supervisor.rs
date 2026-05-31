use defmt::{info, warn};
use embassy_net::Stack;
use embassy_time::{Duration, Timer};
use esp_radio::wifi::WifiController;

use crate::network;
use crate::power;

const CONNECTION_MONITOR_INTERVAL_SECS: u64 = 5;

#[embassy_executor::task]
pub async fn network_supervisor_task(
    mut wifi_controller: WifiController<'static>,
    stack: Stack<'static>,
) {
    info!(
        "NET[{}]: Network supervisor started",
        power::wake_reason_class()
    );

    network::connect_until_associated(&mut wifi_controller).await;
    network::wait_for_ipv4_config(stack).await;

    loop {
        if !wifi_controller.is_connected() {
            warn!(
                "NET[{}]: WiFi disconnected, reconnecting...",
                power::wake_reason_class()
            );
            network::reconnect_until_connected(&mut wifi_controller).await;
            network::wait_for_ipv4_config(stack).await;
        }

        Timer::after(Duration::from_secs(CONNECTION_MONITOR_INTERVAL_SECS)).await;
    }
}
