use defmt::{info, warn};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Sender as ChannelSender;
use embassy_sync::mutex::Mutex;
use embassy_sync::watch::Sender as WatchSender;
use embassy_time::{Duration, Timer, Instant};
use esp_hal::Async;
use esp_hal::gpio::Level;
use esp_hal::rmt::{Channel as RmtChannel, Error, PulseCode, Rx};

use crate::messages::RadioReading;
use crate::radio_433::Radio433;
use crate::telemetry::{TelemetryEnqueueOutcome, TelemetryPipelineAdapter, now_ms};

pub struct PulseCapture<'d, R: Radio433 + 'static> {
    channel: RmtChannel<'d, Async, Rx>,
    radio: &'static Mutex<CriticalSectionRawMutex, R>,
    sender: WatchSender<'static, CriticalSectionRawMutex, RadioReading, 2>,
    mqtt_sender: ChannelSender<'static, CriticalSectionRawMutex, RadioReading, 16>,
    telemetry_adapter: TelemetryPipelineAdapter<32>,
}

struct PulseDistanceIter<'a> {
    symbols: &'a [PulseCode],
    symbol_idx: usize,
    part: u8,
    done: bool,
}

impl<'a> PulseDistanceIter<'a> {
    fn new(symbols: &'a [PulseCode], symbol_count: usize) -> Self {
        let count = symbol_count.min(symbols.len());
        Self {
            symbols: &symbols[..count],
            symbol_idx: 0,
            part: 0,
            done: false,
        }
    }
}

impl Iterator for PulseDistanceIter<'_> {
    type Item = u32;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.done && self.symbol_idx < self.symbols.len() {
            let code = self.symbols[self.symbol_idx];

            if self.part == 0 {
                self.part = 1;
                let len1 = code.length1();
                if len1 == 0 {
                    self.done = true;
                    return None;
                }
                if code.level1() == Level::Low {
                    return Some(u32::from(len1));
                }
                continue;
            }

            self.part = 0;
            self.symbol_idx += 1;
            let len2 = code.length2();
            if len2 == 0 {
                self.done = true;
                return None;
            }
            if code.level2() == Level::Low {
                return Some(u32::from(len2));
            }
        }

        None
    }
}

impl<'d, R: Radio433 + 'static> PulseCapture<'d, R> {
    pub fn new(
        channel: RmtChannel<'d, Async, Rx>,
        radio: &'static Mutex<CriticalSectionRawMutex, R>,
        sender: WatchSender<'static, CriticalSectionRawMutex, RadioReading, 2>,
        mqtt_sender: ChannelSender<'static, CriticalSectionRawMutex, RadioReading, 16>,
    ) -> Self {
        Self {
            channel,
            radio,
            sender,
            mqtt_sender,
            telemetry_adapter: TelemetryPipelineAdapter::new(),
        }
    }

    pub async fn run(&mut self) -> ! {
        // Each symbol encodes two level-duration entries.
        let mut symbols = [PulseCode::default(); 512];

        info!("PulseCapture(RMT): Ready, waiting for signal...");

        loop {
            let symbol_count = match self.channel.receive(&mut symbols).await {
                Ok(count) => count,
                Err(Error::ReceiverError) => {
                    // ReceiverError, assume buffer overflow and treat as end of capture
                    // Even if the error was something else, we'll just try to decode what we got
                    info!("RMT ReceiverError, treating as end of capture...");
                    symbols.len()
                }
                Err(e) => {
                    warn!("RMT error: {:?}", e);
                    continue;
                }
            };

            if symbol_count >= (12 * 36) {
                // Valid messages consist of 12 packets of 36 bits (symbols)
                let mut gap_count = 0usize;
                let decode_result = rubicson::decode_gaps_iter(
                    PulseDistanceIter::new(&symbols, symbol_count).inspect(|_| {
                        gap_count += 1;
                    }),
                );

                info!(
                    "RMT capture complete: {} symbols -> {} low gaps",
                    symbol_count, gap_count
                );

                match decode_result {
                    Ok((_row, reading)) => {
                        let (rssi, detection_threshold) = {
                            let mut radio = self.radio.lock().await;
                            let rssi = radio.get_rssi_dbm().await.unwrap_or(-128);
                            let detection_threshold = radio.get_detection_threshold();
                            (rssi, detection_threshold)
                        };

                        info!("Decoded: {:?}, RSSI={}dBm", reading, rssi);

                        let radio_reading = RadioReading {
                            inner: reading,
                            rssi,
                            detection_threshold,
                            received_at: Instant::now(),
                        };
                        self.sender.send(radio_reading);
                        match self.telemetry_adapter.enqueue_for_channel(
                            radio_reading,
                            now_ms(),
                            &self.mqtt_sender,
                        ) {
                            TelemetryEnqueueOutcome::Queued
                            | TelemetryEnqueueOutcome::DroppedByPolicy => {}
                            TelemetryEnqueueOutcome::RejectedAsDuplicate => {
                                info!(
                                    "Telemetry duplicate rejected for sensor {}",
                                    radio_reading.inner.id
                                );
                            }
                        }
                        // Delay 45s before accepting the next signal
                        // The rubicson sensor sends once a minute, so it should be safe to
                        // have a long inactive period here.
                        // Also the pulse decode is early exit on a valid packet
                        // Since the sensor sends 12 packets in a burst
                        // we want to avoid capturing the next packet in the same burst
                        // which would cause duplicate readings
                        Timer::after(Duration::from_secs(45)).await;
                    }
                    Err(e) => {
                        info!("Decode failed: {:?}", e);
                    }
                }
            }
        }
    }
}
