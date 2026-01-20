use defmt::info;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{Receiver, Sender};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::gpio::Input;
use rubicson;

use crate::messages::{RadioReading, RadioSettings};
use crate::radio_433::Radio433;

pub struct PulseCapture<'d, R: Radio433> {
    pin: Input<'d>,
    radio: &'d mut R,
    sender: Sender<'static, CriticalSectionRawMutex, RadioReading, 2>,
    settings_receiver: Receiver<'static, CriticalSectionRawMutex, RadioSettings, 2>,
}

/// Timeout for considering a transmission ended
const TRANSMISSION_END_TIMEOUT_US: u64 = 4500;

/// Dump gap buffer to logs for offline analysis
#[allow(dead_code)]
fn dump_gaps(gaps: &[u32]) {
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

impl<'d, R: Radio433> PulseCapture<'d, R> {
    pub fn new(
        pin: Input<'d>,
        radio: &'d mut R,
        sender: Sender<'static, CriticalSectionRawMutex, RadioReading, 2>,
        settings_receiver: Receiver<'static, CriticalSectionRawMutex, RadioSettings, 2>,
    ) -> Self {
        Self {
            pin,
            radio,
            sender,
            settings_receiver,
        }
    }

    pub async fn run(&mut self) -> ! {
        // Buffer for gap durations
        // 12 repetitions × 36 bits = 432 gaps, plus some margin
        let mut gap_buffer = [0u32; 512];

        info!("PulseCapture: Ready, waiting for signal...");

        loop {
            // === PHASE 1: SLEEP until signal arrives ===
            // Wait for first edge (low->high transition = start of pulse)
            self.pin.wait_for_rising_edge().await;
            let capture_start = Instant::now();

            info!("Signal detected, capturing...");

            // === PHASE 2: CAPTURE all gaps until silence ===
            let mut gap_count = 0;

            loop {
                // Wait for falling edge (end of pulse, start of gap)
                let fall_result = select(
                    self.pin.wait_for_falling_edge(),
                    Timer::after(Duration::from_micros(TRANSMISSION_END_TIMEOUT_US)),
                )
                .await;

                if matches!(fall_result, Either::Second(())) {
                    // Timeout waiting for falling edge - transmission ended
                    break;
                }

                let gap_start = Instant::now();

                // Wait for rising edge (end of gap, start of next pulse)
                let rise_result = select(
                    self.pin.wait_for_rising_edge(),
                    Timer::after(Duration::from_micros(TRANSMISSION_END_TIMEOUT_US)),
                )
                .await;

                if matches!(rise_result, Either::Second(())) {
                    // Timeout waiting for rising edge - transmission ended
                    break;
                }

                let gap_end = Instant::now();
                let gap_us = u32::try_from(gap_end.duration_since(gap_start).as_micros())
                    .unwrap_or(u32::MAX);

                // Store gap if buffer not full
                if gap_count < gap_buffer.len() {
                    gap_buffer[gap_count] = gap_us;
                    gap_count += 1;
                }
            }

            // === PHASE 3: DECODE captured data ===
            let capture_duration = Instant::now().duration_since(capture_start);
            info!(
                "Capture complete: {} gaps in {} ms",
                gap_count,
                capture_duration.as_millis()
            );

            if gap_count >= 36 {
                // Uncomment to dump gaps for offline analysis
                // dump_gaps(&gap_buffer[..gap_count]);

                // We have at least one packet worth of data try to decode
                match rubicson::decode_gaps(&gap_buffer[..gap_count]) {
                    Ok((_row, reading)) => {
                        // Capture RSSI immediately after successful decode
                        let rssi = self.radio.get_rssi_dbm().await.unwrap_or(-128);
                        let detection_threshold = self.radio.get_detection_threshold();

                        info!("Decoded: {:?}, RSSI={}dBm", reading, rssi);

                        let radio_reading = RadioReading {
                            inner: reading,
                            rssi,
                            detection_threshold,
                        };
                        self.sender.send(radio_reading);
                    }
                    Err(e) => {
                        info!("Decode failed: {:?}", e);
                    }
                }
            } else {
                info!("Not enough gaps for decoding (need 36, got {})", gap_count);
            }

            // Delay 1s before listening again (debounce)
            // Also check for settings changes during this quiet period
            Timer::after(Duration::from_millis(1000)).await;

            // Apply any pending settings changes
            if let Some(settings) = self.settings_receiver.try_get() {
                let current_threshold = self.radio.get_detection_threshold();
                if settings.detection_threshold_db != current_threshold {
                    info!(
                        "Applying new detection threshold: {} dB (was {} dB)",
                        settings.detection_threshold_db, current_threshold
                    );
                    if let Err(e) = self
                        .radio
                        .set_detection_threshold(settings.detection_threshold_db)
                        .await
                    {
                        info!("Failed to set detection threshold: {:?}", e);
                    }
                }

                let current_magn_target = self.radio.get_filter_level();
                if settings.magn_target != current_magn_target {
                    info!(
                        "Applying new magn target: {} (was {})",
                        settings.magn_target, current_magn_target
                    );
                    if let Err(e) = self.radio.set_filter_level(settings.magn_target).await {
                        info!("Failed to set filter level: {:?}", e);
                    }
                }
            }

            info!("Ready, waiting for signal...");
        }
    }
}
