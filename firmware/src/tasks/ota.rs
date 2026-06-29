use alloc::boxed::Box;

use core::ptr::addr_of_mut;
use defmt::{Debug2Format, info, warn};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};
use esp_bootloader_esp_idf::partitions::PARTITION_TABLE_MAX_LEN;
use esp_hal::system::software_reset;
use esp_storage::FlashStorage;
use ota_core::{Ed25519ManifestVerifier, OtaManifest, OtaState, validate_ota_url_policy};

#[cfg(feature = "release-ota")]
fn ota_manifest_verifier() -> Ed25519ManifestVerifier {
    Ed25519ManifestVerifier::release_ota()
}

#[cfg(not(feature = "release-ota"))]
fn ota_manifest_verifier() -> Ed25519ManifestVerifier {
    Ed25519ManifestVerifier::dev_test()
}

use crate::app_bus;
use crate::app_bus::{MqttHealth, MqttHealthReceiver};
use crate::ota::flash_write;
use crate::secrets;

const MQTT_STANDDOWN_TIMEOUT_SECS: u64 = 15;

static mut OTA_PARTITION_TABLE_BUF: [u8; PARTITION_TABLE_MAX_LEN] = [0u8; PARTITION_TABLE_MAX_LEN];

fn ota_partition_table_buf() -> &'static mut [u8; PARTITION_TABLE_MAX_LEN] {
    // SAFETY: the single OTA task serializes all OTA flash-write attempts.
    unsafe { &mut *addr_of_mut!(OTA_PARTITION_TABLE_BUF) }
}

#[embassy_executor::task]
pub async fn ota_task(
    receiver: app_bus::OtaCommandReceiver,
    ota_state_sender: app_bus::OtaStateSender,
    mut mqtt_health_receiver: MqttHealthReceiver,
    network_stack: embassy_net::Stack<'static>,
    flash_mutex: &'static Mutex<CriticalSectionRawMutex, FlashStorage<'static>>,
) {
    // Encryption key material.
    let master_key: [u8; 32] = secrets::OTA_ENCRYPTION_MASTER_KEY;

    loop {
        let manifest_bytes = receiver.receive().await;

        let manifest =
            match OtaManifest::parse_and_verify(&manifest_bytes, &ota_manifest_verifier()) {
                Ok(m) => m,
                Err(e) => {
                    warn!("OTA: manifest rejected: {:?}", Debug2Format(&e));
                    continue;
                }
            };

        if let Err(e) = validate_ota_url_policy(&manifest.url) {
            warn!("OTA: URL policy rejected: {:?}", Debug2Format(&e));
            continue;
        }

        info!(
            "OTA: starting download of {} (v{}, download_size={}, image_size={})",
            manifest.url.as_str(),
            manifest.version.as_str(),
            manifest.download_size,
            manifest.image_size
        );

        if manifest.enc.key_id != secrets::OTA_ENCRYPTION_KEY_ID {
            warn!(
                "OTA: enc.key_id {} != configured {} — rejecting",
                manifest.enc.key_id,
                secrets::OTA_ENCRYPTION_KEY_ID
            );
            continue;
        }

        ota_state_sender.send(OtaState::Downloading);
        wait_for_mqtt_stand_down(&mut mqtt_health_receiver, MQTT_STANDDOWN_TIMEOUT_SECS).await;

        ota_state_sender.send(OtaState::Applying);

        {
            let mut flash_guard = flash_mutex.lock().await;
            let partition_table_buf = ota_partition_table_buf();

            if let Err(e) = Box::pin(flash_write::download_and_write_to_flash(
                network_stack,
                &manifest,
                &mut flash_guard,
                partition_table_buf,
                &master_key,
            ))
            .await
            {
                warn!("OTA: flash write failed: {:?}", Debug2Format(&e));
            } else {
                // Ensure all flash writes are committed before
                // triggering a CPU reset.
                unsafe {
                    core::arch::asm!("memw; isync", options(nomem, nostack));
                }
                software_reset();
            }
        }

        ota_state_sender.send(OtaState::Inactive);
        info!("OTA: returned to Inactive");
    }
}

/// Give MQTT a bounded window to observe `OtaState::Downloading` and
/// disconnect before OTA starts using the network stack for HTTP.
async fn wait_for_mqtt_stand_down(
    mqtt_health_receiver: &mut MqttHealthReceiver,
    timeout_secs: u64,
) {
    if mqtt_health_receiver.try_get() == Some(MqttHealth::Disconnected) {
        return;
    }

    Timer::after(Duration::from_secs(timeout_secs)).await;
    if mqtt_health_receiver.try_get() != Some(MqttHealth::Disconnected) {
        warn!(
            "OTA: MQTT did not stand down within {}s, proceeding anyway",
            timeout_secs
        );
    }
}
