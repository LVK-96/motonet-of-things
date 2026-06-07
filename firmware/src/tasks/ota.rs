use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};
use esp_storage::FlashStorage;

use defmt::{Debug2Format, info, warn};
use esp_bootloader_esp_idf::partitions::PARTITION_TABLE_MAX_LEN;
use ota_core::{Ed25519ManifestVerifier, OtaManifest, OtaState, validate_ota_url_policy};

use crate::app_bus;
use crate::ota::{OtaUpdateGuard, flash_write};

#[embassy_executor::task]
pub async fn ota_task(
    receiver: app_bus::OtaCommandReceiver,
    ota_state_sender: app_bus::OtaStateSender,
    network_stack: embassy_net::Stack<'static>,
    flash_mutex: &'static Mutex<CriticalSectionRawMutex, FlashStorage<'static>>,
) {
    loop {
        let manifest_bytes = receiver.receive().await;

        // 1. Parse and verify manifest.
        let manifest = match OtaManifest::parse_and_verify(
            &manifest_bytes,
            &Ed25519ManifestVerifier::dev_test(),
        ) {
            Ok(m) => m,
            Err(e) => {
                warn!("OTA: manifest rejected: {:?}", Debug2Format(&e));
                continue;
            }
        };

        // 2. Validate URL policy.
        if let Err(e) = validate_ota_url_policy(&manifest.url) {
            warn!("OTA: URL policy rejected: {:?}", Debug2Format(&e));
            continue;
        }

        info!(
            "OTA: starting download of {} (v{}, {} bytes)",
            manifest.url.as_str(),
            manifest.version.as_str(),
            manifest.size
        );

        // 3. Broadcast Downloading state so MQTT/radio/display stand down.
        ota_state_sender.send(OtaState::Downloading);

        // 4. Prevent sleep for the entire OTA window, then give MQTT time to
        //    observe the state change and disconnect.
        let _ota_guard = OtaUpdateGuard::begin_download();
        Timer::after(Duration::from_secs(5)).await;

        // 5. Lock flash and stream the firmware into the inactive partition.
        {
            let mut flash_guard = flash_mutex.lock().await;
            let mut partition_table_buf = [0u8; PARTITION_TABLE_MAX_LEN];

            match flash_write::download_and_write_to_flash(
                network_stack,
                &manifest,
                &mut flash_guard,
                &mut partition_table_buf,
            )
            .await
            {
                // On success the function reboots; control never reaches here.
                Ok(()) => {
                    info!("OTA: flash write returned Ok (unexpected)");
                }
                Err(e) => {
                    warn!("OTA: flash write failed: {:?}", Debug2Format(&e));
                }
            }
            // Flash mutex guard is dropped here, releasing the lock.
        }

        // 6. ALWAYS return to Inactive (guard also resets on drop).
        ota_state_sender.send(OtaState::Inactive);
        info!("OTA: returned to Inactive");
    }
}
