use defmt::{info, warn};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{Receiver, Sender};
use esp_hal::Async;
use esp_hal::gpio::Level;
use esp_hal::rmt::{Channel as RmtChannel, PulseCode, Rx};

use crate::messages::{RadioReading, RadioSettings};
use crate::pulse_capture::apply_pending_settings;
use crate::radio_433::Radio433;

pub struct PulseCapture<'d, R: Radio433> {
    channel: RmtChannel<'d, Async, Rx>,
    radio: &'d mut R,
    sender: Sender<'static, CriticalSectionRawMutex, RadioReading, 2>,
    settings_receiver: Receiver<'static, CriticalSectionRawMutex, RadioSettings, 2>,
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

impl<'d, R: Radio433> PulseCapture<'d, R> {
    pub fn new(
        channel: RmtChannel<'d, Async, Rx>,
        radio: &'d mut R,
        sender: Sender<'static, CriticalSectionRawMutex, RadioReading, 2>,
        settings_receiver: Receiver<'static, CriticalSectionRawMutex, RadioSettings, 2>,
    ) -> Self {
        Self {
            channel,
            radio,
            sender,
            settings_receiver,
        }
    }

    pub async fn run(&mut self) -> ! {
        // Each symbol encodes two level-duration entries.
        let mut symbols = [PulseCode::default(); 256];

        info!("PulseCapture(RMT): Ready, waiting for signal...");

        loop {
            let symbol_count = match self.channel.receive(&mut symbols).await {
                Ok(count) => count,
                Err(e) => {
                    warn!("RMT receive failed: {:?}", e);
                    apply_pending_settings(&mut *self.radio, &mut self.settings_receiver).await;
                    continue;
                }
            };

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

            if gap_count >= 36 {
                match decode_result {
                    Ok((_row, reading)) => {
                        let rssi = self.radio.get_rssi_dbm().await.unwrap_or(-128);
                        let detection_threshold = self.radio.get_detection_threshold();

                        info!("Decoded: {:?}, RSSI={}dBm", reading, rssi);

                        self.sender.send(RadioReading {
                            inner: reading,
                            rssi,
                            detection_threshold,
                        });
                    }
                    Err(e) => {
                        info!("Decode failed: {:?}", e);
                    }
                }
            } else {
                info!("Not enough gaps for decoding (need 36, got {})", gap_count);
            }

            apply_pending_settings(&mut *self.radio, &mut self.settings_receiver).await;
        }
    }
}
