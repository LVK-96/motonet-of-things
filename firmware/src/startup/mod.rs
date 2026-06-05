#[cfg(not(feature = "ota-rollback-test"))]
use core::future::pending;

#[cfg(feature = "ota-rollback-test")]
use defmt::warn;
use embassy_executor::Spawner;
#[cfg(feature = "ota-rollback-test")]
use embassy_time::{Duration, Timer};

#[cfg(not(feature = "ota-rollback-test"))]
mod hardware;
#[cfg(not(feature = "ota-rollback-test"))]
mod hw_context;
#[cfg(not(feature = "ota-rollback-test"))]
mod spawn;

#[cfg(not(feature = "ota-rollback-test"))]
pub async fn run(spawner: Spawner) -> ! {
    let hw_context = hw_context::hw_setup(&spawner).await;
    spawn::spawn_tasks(&spawner, hw_context);

    loop {
        pending::<()>().await;
    }
}

#[cfg(feature = "ota-rollback-test")]
pub async fn run(_spawner: Spawner) -> ! {
    rollback_test_reboot().await
}

#[cfg(feature = "ota-rollback-test")]
async fn rollback_test_reboot() -> ! {
    crate::ota::arm_rollback_test_pending_confirmation();
    warn!("OTA rollback-test build: intentionally not confirming app valid; rebooting soon");
    Timer::after(Duration::from_secs(10)).await;
    esp_hal::system::software_reset()
}
