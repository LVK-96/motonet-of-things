use defmt::info;

use crate::app_bus;

#[embassy_executor::task]
pub async fn ota_task(receiver: app_bus::OtaCommandReceiver) {
    loop {
        let manifest = receiver.receive().await;
        info!(
            "OTA: received signed manifest command over MQTT ({} bytes)",
            manifest.len()
        );
    }
}
