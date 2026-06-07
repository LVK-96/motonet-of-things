use defmt::{Debug2Format, info, warn};
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use esp_bootloader_esp_idf::partitions::PARTITION_TABLE_MAX_LEN;
use esp_storage::FlashStorage;
use ota_core::{Ed25519ManifestVerifier, OtaManifest, OtaState, validate_ota_url_policy};

use crate::app_bus;
use crate::app_bus::{MqttHealth, MqttHealthReceiver};
use crate::ota::{OtaUpdateGuard, flash_write};

/// Maximum time to wait for MQTT to stand down after broadcasting
/// `OtaState::Downloading` before proceeding with the HTTP download.
const MQTT_STANDDOWN_TIMEOUT_SECS: u64 = 5;

#[embassy_executor::task]
pub async fn ota_task(
    receiver: app_bus::OtaCommandReceiver,
    ota_state_sender: app_bus::OtaStateSender,
    mut mqtt_health_receiver: MqttHealthReceiver,
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

        // 4. Block sleep for the entire OTA window via the atomic guard.
        //    Then race the stand-down timeout against the actual
        //    MqttHealth::Disconnected transition so we don't burn the
        //    full 5 s on a fast MQTT teardown.
        let _ota_guard = OtaUpdateGuard::begin_download();
        wait_for_mqtt_stand_down(&mut mqtt_health_receiver, MQTT_STANDDOWN_TIMEOUT_SECS).await;

        // 5. Switch to the Applying phase so observers can show
        //    "OTA applying..." during the flash write.
        ota_state_sender.send(OtaState::Applying);

        // 6. Lock flash and stream the firmware into the inactive partition.
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
                Err(e) => warn!("OTA: flash write failed: {:?}", Debug2Format(&e)),
                // The `Ok` arm of `Result<Infallible, _>` is uninhabited —
                // `download_and_write_to_flash` either errors out or
                // calls `software_reset()` which never returns. Matching
                // on the never value is exhaustive and proves this arm
                // is unreachable.
                Ok(unreachable) => match unreachable {},
            }
        }

        // 7. Return to Inactive (the guard also resets on drop, but we
        //    need the watch observers to see the transition too).
        ota_state_sender.send(OtaState::Inactive);
        info!("OTA: returned to Inactive");
    }
}

/// Wait until MQTT reports `MqttHealth::Disconnected`, or until
/// `timeout_secs` elapses since the first observation of a non-disconnected
/// state. Logs a warning if the timeout expires first.
async fn wait_for_mqtt_stand_down(
    mqtt_health_receiver: &mut MqttHealthReceiver,
    timeout_secs: u64,
) {
    if mqtt_health_receiver.try_get() == Some(MqttHealth::Disconnected) {
        return;
    }

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let now = Instant::now();
        if now >= deadline {
            warn!(
                "OTA: MQTT did not stand down within {}s, proceeding anyway",
                timeout_secs
            );
            return;
        }
        let remaining = deadline - now;
        match select(Timer::after(remaining), mqtt_health_receiver.changed()).await {
            Either::First(()) => {
                warn!(
                    "OTA: MQTT did not stand down within {}s, proceeding anyway",
                    timeout_secs
                );
                return;
            }
            Either::Second(_new_value) => {
                if mqtt_health_receiver.try_get() == Some(MqttHealth::Disconnected) {
                    return;
                }
            }
        }
    }
}
