use defmt::{debug, info, trace};
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Sender as ChannelSender;
use embassy_sync::watch::{Receiver, Sender as WatchSender};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::gpio::Input;

use crate::messages::{RadioReading, RadioSettings};
use crate::pulse_capture::apply_pending_settings;
use crate::radio_433::Radio433;

pub struct PulseCapture<'d, R: Radio433> {
    pin: Input<'d>,
    radio: &'d mut R,
    sender: WatchSender<'static, CriticalSectionRawMutex, RadioReading, 2>,
    mqtt_sender: ChannelSender<'static, CriticalSectionRawMutex, RadioReading, 16>,
    settings_receiver: Receiver<'static, CriticalSectionRawMutex, RadioSettings, 2>,
}

/// Timeout for considering a transmission ended
const TRANSMISSION_END_TIMEOUT_US: u64 = 4500;

impl<'d, R: Radio433> PulseCapture<'d, R> {
    pub fn new(
        pin: Input<'d>,
        radio: &'d mut R,
        sender: WatchSender<'static, CriticalSectionRawMutex, RadioReading, 2>,
        mqtt_sender: ChannelSender<'static, CriticalSectionRawMutex, RadioReading, 16>,
        settings_receiver: Receiver<'static, CriticalSectionRawMutex, RadioSettings, 2>,
    ) -> Self {
        Self {
            pin,
            radio,
            sender,
            mqtt_sender,
            settings_receiver,
        }
    }

    #[allow(dead_code)]
    async fn sw_capture(&mut self, gap_buffer: &mut [u32; 512]) -> (Instant, usize) {
        // Sleep until signal arrives
        // Wait for first edge (low->high transition = start of pulse)
        self.pin.wait_for_rising_edge().await;
        let capture_start = Instant::now();

        debug!("Signal detected, capturing...");

        // Capture all gaps until silence
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
            let gap_us =
                u32::try_from(gap_end.duration_since(gap_start).as_micros()).unwrap_or(u32::MAX);

            // Store gap if buffer not full
            if gap_count < gap_buffer.len() {
                gap_buffer[gap_count] = gap_us;
                gap_count += 1;
            }
        }

        (capture_start, gap_count)
    }

    pub async fn run(&mut self) -> ! {
        // Buffer for gap durations
        // 12 repetitions × 36 bits = 432 gaps, plus some margin
        let mut gap_buffer = [0u32; 512];

        info!("PulseCapture: Ready, waiting for signal...");

        loop {
            // Capture raw radio frame
            let (capture_start, gap_count) = self.sw_capture(&mut gap_buffer).await;

            // Decode captured data
            let capture_duration = Instant::now().duration_since(capture_start);
            debug!(
                "Capture complete: {} gaps in {} ms",
                gap_count,
                capture_duration.as_millis()
            );

            if gap_count >= 36 {
                // Uncomment to dump gaps for offline analysis
                // crate::pulse_capture::dump_gaps(&gap_buffer[..gap_count]);

                // We have at least one packet worth of data try to decode
                match rubicson::decode_gaps(&gap_buffer[..gap_count]) {
                    Ok((_row, reading)) => {
                        // Capture RSSI immediately after successful decode
                        let rssi = self.radio.get_rssi_dbm().await.unwrap_or(-128);
                        let detection_threshold = self.radio.get_detection_threshold();

                        debug!("Decoded: {:?}, RSSI={}dBm", reading, rssi);

                        let radio_reading = RadioReading {
                            inner: reading,
                            rssi,
                            detection_threshold,
                        };
                        self.sender.send(radio_reading);
                        let _ = self.mqtt_sender.try_send(radio_reading);
                    }
                    Err(e) => {
                        trace!("Decode failed: {:?}", e);
                    }
                }
            } else {
                trace!("Not enough gaps for decoding (need 36, got {})", gap_count);
            }

            // Delay 1s before listening again (debounce)
            // Also check for settings changes during this quiet period
            Timer::after(Duration::from_millis(1000)).await;

            apply_pending_settings(&mut *self.radio, &mut self.settings_receiver).await;

            debug!("Ready, waiting for signal...");
        }
    }
}
