//! Post-OTA health confirmation task.
//!
//! After a successful OTA update and reboot, the new image remains unconfirmed
//! as `New` or `PendingVerify`. This task waits for health
//! signals (Wi‑Fi up, MQTT connected + heartbeat published, minimum uptime)
//! and then calls [`OtaBootMetadata::mark_current_app_valid`] to confirm the
//! image. If confirmation is skipped, the bootloader will roll back to the
//! previous image on the next reset.

use defmt::{info, warn};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use esp_bootloader_esp_idf::partitions::PARTITION_TABLE_MAX_LEN;
use esp_storage::FlashStorage;
use ota_core::OtaState;

use crate::app_bus;
use crate::ota::OtaBootMetadata;

const CONFIRMATION_WINDOW_SECS: u32 = ota_core::OTA_CONFIRMATION_DELAY_SECS;

#[embassy_executor::task]
pub async fn ota_confirm_task(
    ota_state_sender: app_bus::OtaStateSender,
    mqtt_health_receiver: &'static mut app_bus::MqttHealthReceiver,
    mqtt_command_sender: app_bus::MqttCommandSender,
    flash_mutex: &'static Mutex<CriticalSectionRawMutex, FlashStorage<'static>>,
) {
    // ── Check bootloader state ────────────────────────────────────────
    ota_state_sender.send(OtaState::PendingConfirmation);

    let boot_metadata_pending = {
        let mut flash = flash_mutex.lock().await;
        let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];

        match OtaBootMetadata::new(&mut *flash, &mut buf) {
            Ok(mut meta) => match meta.current_app_pending_confirmation() {
                Ok(pending) => pending,
                Err(e) => {
                    warn!(
                        "OTA confirm: cannot read boot state: {:?}",
                        defmt::Debug2Format(&e)
                    );
                    false
                }
            },
            Err(e) => {
                warn!(
                    "OTA confirm: cannot open boot metadata: {:?}",
                    defmt::Debug2Format(&e)
                );
                false
            }
        }
    };

    if !boot_metadata_pending {
        info!("OTA confirm: image already confirmed, nothing to do");
        ota_state_sender.send(OtaState::Inactive);
        return;
    }

    info!("OTA confirm: new image pending verification, entering confirmation mode");

    // Warm‑up delay: give peripheral tasks (Wi‑Fi, MQTT) time to spin up.
    Timer::after(embassy_time::Duration::from_secs(2)).await;

    // ── Wait for health signals ───────────────────────────────────────
    let boot_at = Instant::now();

    // Wait for MQTT heartbeat + uptime window in a single loop.
    loop {
        let health = mqtt_health_receiver
            .try_get()
            .unwrap_or(app_bus::MqttHealth::Disconnected);
        if health == app_bus::MqttHealth::HeartbeatPublished {
            let elapsed = u32::try_from(boot_at.elapsed().as_secs()).unwrap_or(u32::MAX);
            if elapsed >= CONFIRMATION_WINDOW_SECS {
                info!(
                    "OTA confirm: health gate passed (uptime={}s, mqtt=heartbeat_published), confirming image",
                    elapsed
                );
                break;
            }
            let remaining = (CONFIRMATION_WINDOW_SECS - elapsed).max(1);
            info!(
                "OTA confirm: heartbeat seen, waiting {}s for uptime window ({}s / {}s)",
                remaining, elapsed, CONFIRMATION_WINDOW_SECS
            );
            Timer::after(Duration::from_secs(u64::from(remaining))).await;
            // After the timer, re-check (health might have dropped).
            continue;
        }
        // Health not yet confirmed; wait for a state change.
        mqtt_health_receiver.changed().await;
    }

    // ── Confirm the image ─────────────────────────────────────────────
    {
        let mut flash = flash_mutex.lock().await;
        let mut buf = [0u8; PARTITION_TABLE_MAX_LEN];

        match OtaBootMetadata::new(&mut *flash, &mut buf) {
            Ok(mut meta) => {
                if let Err(e) = meta.mark_current_app_valid() {
                    warn!(
                        "OTA confirm: mark_current_app_valid failed: {:?}",
                        defmt::Debug2Format(&e)
                    );
                    // Stay in PendingConfirmation; bootloader handles
                    // rollback on next power cycle.
                    return;
                }
            }
            Err(e) => {
                warn!(
                    "OTA confirm: cannot open boot metadata for confirmation: {:?}",
                    defmt::Debug2Format(&e)
                );
                return;
            }
        }
    }

    info!("OTA confirm: image marked valid, publishing confirmation via MQTT");
    ota_state_sender.send(OtaState::Inactive);
    mqtt_command_sender
        .send(crate::app_bus::AppCommand::OtaConfirmed)
        .await;
}
