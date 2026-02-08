use defmt::{info, warn};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Receiver;

use crate::messages::RadioSettings;
use crate::radio_433::Radio433;

#[cfg(all(feature = "pulse_sw", feature = "pulse_rmt"))]
compile_error!("Enable only one pulse capture backend: pulse_sw or pulse_rmt");

#[cfg(not(any(feature = "pulse_sw", feature = "pulse_rmt")))]
compile_error!("Enable one pulse capture backend: pulse_sw or pulse_rmt");

#[cfg(feature = "pulse_sw")]
#[path = "pulse_capture_sw.rs"]
mod backend;

#[cfg(feature = "pulse_rmt")]
#[path = "pulse_capture_rmt.rs"]
mod backend;

pub(crate) async fn apply_pending_settings<R: Radio433>(
    radio: &mut R,
    settings_receiver: &mut Receiver<'static, CriticalSectionRawMutex, RadioSettings, 2>,
) {
    if let Some(settings) = settings_receiver.try_get() {
        let current_threshold = radio.get_detection_threshold();
        if settings.detection_threshold_db != current_threshold {
            info!(
                "Applying new detection threshold: {} dB (was {} dB)",
                settings.detection_threshold_db, current_threshold
            );
            if let Err(e) = radio
                .set_detection_threshold(settings.detection_threshold_db)
                .await
            {
                warn!("Failed to set detection threshold: {:?}", e);
            }
        }

        let current_magn_target = radio.get_filter_level();
        if settings.magn_target != current_magn_target {
            info!(
                "Applying new magn target: {} (was {})",
                settings.magn_target, current_magn_target
            );
            if let Err(e) = radio.set_filter_level(settings.magn_target).await {
                warn!("Failed to set filter level: {:?}", e);
            }
        }
    }
}

/// Dump gap buffer to logs for offline analysis.
#[allow(dead_code)]
pub(crate) fn dump_gaps(gaps: &[u32]) {
    info!("=== GAP DUMP START ({} gaps) ===", gaps.len());
    // Print 10 gaps per line for readability
    for chunk_start in (0..gaps.len()).step_by(10) {
        let chunk_end = (chunk_start + 10).min(gaps.len());
        match chunk_end - chunk_start {
            10 => info!(
                "{}: {} {} {} {} {} {} {} {} {} {}",
                chunk_start,
                gaps[chunk_start],
                gaps[chunk_start + 1],
                gaps[chunk_start + 2],
                gaps[chunk_start + 3],
                gaps[chunk_start + 4],
                gaps[chunk_start + 5],
                gaps[chunk_start + 6],
                gaps[chunk_start + 7],
                gaps[chunk_start + 8],
                gaps[chunk_start + 9]
            ),
            9 => info!(
                "{}: {} {} {} {} {} {} {} {} {}",
                chunk_start,
                gaps[chunk_start],
                gaps[chunk_start + 1],
                gaps[chunk_start + 2],
                gaps[chunk_start + 3],
                gaps[chunk_start + 4],
                gaps[chunk_start + 5],
                gaps[chunk_start + 6],
                gaps[chunk_start + 7],
                gaps[chunk_start + 8]
            ),
            8 => info!(
                "{}: {} {} {} {} {} {} {} {}",
                chunk_start,
                gaps[chunk_start],
                gaps[chunk_start + 1],
                gaps[chunk_start + 2],
                gaps[chunk_start + 3],
                gaps[chunk_start + 4],
                gaps[chunk_start + 5],
                gaps[chunk_start + 6],
                gaps[chunk_start + 7]
            ),
            7 => info!(
                "{}: {} {} {} {} {} {} {}",
                chunk_start,
                gaps[chunk_start],
                gaps[chunk_start + 1],
                gaps[chunk_start + 2],
                gaps[chunk_start + 3],
                gaps[chunk_start + 4],
                gaps[chunk_start + 5],
                gaps[chunk_start + 6]
            ),
            6 => info!(
                "{}: {} {} {} {} {} {}",
                chunk_start,
                gaps[chunk_start],
                gaps[chunk_start + 1],
                gaps[chunk_start + 2],
                gaps[chunk_start + 3],
                gaps[chunk_start + 4],
                gaps[chunk_start + 5]
            ),
            5 => info!(
                "{}: {} {} {} {} {}",
                chunk_start,
                gaps[chunk_start],
                gaps[chunk_start + 1],
                gaps[chunk_start + 2],
                gaps[chunk_start + 3],
                gaps[chunk_start + 4]
            ),
            4 => info!(
                "{}: {} {} {} {}",
                chunk_start,
                gaps[chunk_start],
                gaps[chunk_start + 1],
                gaps[chunk_start + 2],
                gaps[chunk_start + 3]
            ),
            3 => info!(
                "{}: {} {} {}",
                chunk_start,
                gaps[chunk_start],
                gaps[chunk_start + 1],
                gaps[chunk_start + 2]
            ),
            2 => info!(
                "{}: {} {}",
                chunk_start,
                gaps[chunk_start],
                gaps[chunk_start + 1]
            ),
            1 => info!("{}: {}", chunk_start, gaps[chunk_start]),
            _ => {}
        }
    }
    info!("=== GAP DUMP END ===");
}

pub use backend::PulseCapture;
