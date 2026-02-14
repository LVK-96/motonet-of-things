use defmt::{debug, info, trace, warn};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Receiver;

use crate::messages::{RadioSettings, channel_bandwidth_hz};
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
            if let Err(e) = radio.set_detection_threshold(settings.detection_threshold_db) {
                warn!("Failed to set detection threshold: {:?}", e);
            }
        }

        let current_magn_target = radio.get_filter_level();
        if settings.magn_target != current_magn_target {
            info!(
                "Applying new magn target: {} (was {})",
                settings.magn_target, current_magn_target
            );
            if let Err(e) = radio.set_filter_level(settings.magn_target) {
                warn!("Failed to set filter level: {:?}", e);
            }
        }

        let current_bandwidth_index = radio.get_channel_bandwidth_index();
        if settings.channel_bandwidth_index != current_bandwidth_index {
            let new_bandwidth = channel_bandwidth_hz(settings.channel_bandwidth_index) / 1000;
            let old_bandwidth = channel_bandwidth_hz(current_bandwidth_index) / 1000;
            info!(
                "Applying new channel bandwidth: {} kHz (was {} kHz)",
                new_bandwidth, old_bandwidth
            );
            if let Err(e) = radio.set_channel_bandwidth_index(settings.channel_bandwidth_index) {
                warn!("Failed to set channel bandwidth: {:?}", e);
            }
        }

        let current_carrier_sense = radio.get_carrier_sense_threshold();
        if settings.carrier_sense_threshold != current_carrier_sense {
            info!(
                "Applying new carrier sense threshold: {} dB (was {} dB)",
                settings.carrier_sense_threshold, current_carrier_sense
            );
            if let Err(e) = radio.set_carrier_sense_threshold(settings.carrier_sense_threshold) {
                warn!("Failed to set carrier sense threshold: {:?}", e);
            }
        }
    }
}

/// Dump gap buffer to logs for offline analysis.
#[allow(dead_code)]
pub(crate) fn dump_gaps(gaps: &[u32]) {
    debug!("=== GAP DUMP START ({} gaps) ===", gaps.len());
    // Print 10 gaps per line for readability
    for chunk_start in (0..gaps.len()).step_by(10) {
        let chunk_end = (chunk_start + 10).min(gaps.len());
        match chunk_end - chunk_start {
            10 => trace!(
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
            9 => trace!(
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
            8 => trace!(
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
            7 => trace!(
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
            6 => trace!(
                "{}: {} {} {} {} {} {}",
                chunk_start,
                gaps[chunk_start],
                gaps[chunk_start + 1],
                gaps[chunk_start + 2],
                gaps[chunk_start + 3],
                gaps[chunk_start + 4],
                gaps[chunk_start + 5]
            ),
            5 => trace!(
                "{}: {} {} {} {} {}",
                chunk_start,
                gaps[chunk_start],
                gaps[chunk_start + 1],
                gaps[chunk_start + 2],
                gaps[chunk_start + 3],
                gaps[chunk_start + 4]
            ),
            4 => trace!(
                "{}: {} {} {} {}",
                chunk_start,
                gaps[chunk_start],
                gaps[chunk_start + 1],
                gaps[chunk_start + 2],
                gaps[chunk_start + 3]
            ),
            3 => trace!(
                "{}: {} {} {}",
                chunk_start,
                gaps[chunk_start],
                gaps[chunk_start + 1],
                gaps[chunk_start + 2]
            ),
            2 => trace!(
                "{}: {} {}",
                chunk_start,
                gaps[chunk_start],
                gaps[chunk_start + 1]
            ),
            1 => trace!("{}: {}", chunk_start, gaps[chunk_start]),
            _ => {}
        }
    }
    debug!("=== GAP DUMP END ===");
}

pub use backend::PulseCapture;
