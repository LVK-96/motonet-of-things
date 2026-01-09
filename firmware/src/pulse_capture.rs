use defmt::info;
use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::gpio::Input;
use rubicson;

#[derive(Debug)]
pub enum CaptureError {
    Timeout,
}

pub struct PulseCapture<'d> {
    pin: Input<'d>,
}

impl<'d> PulseCapture<'d> {
    pub fn new(pin: Input<'d>) -> Self {
        Self { pin }
    }

    pub async fn run(&mut self) -> ! {
        // The rubicson sensor sends 12 * 36 bit packets (432 bits)
        // We collect the gap durations in a buffer of 512 u32s
        let mut pulse_buffer = [0u32; 512];
        let mut pulse_count = 0;

        // Stats for periodic reporting
        let mut edge_count = 0u32;
        let mut timeout_count = 0u32;
        let mut last_stats_report = Instant::now();

        info!("PulseCapture: GDO0 initial state = {}", self.pin.is_high());
        info!("PulseCapture: entering main loop...");

        let mut last_edge = Instant::now();

        // Main loop: capture gaps between pulses with timeouts
        loop {
            let is_high = self.pin.is_high();

            // Wait for any edge change with 1 second timeout
            let edge_result = if is_high {
                select(
                    self.pin.wait_for_low(),
                    Timer::after(Duration::from_secs(1)),
                )
                .await
            } else {
                select(
                    self.pin.wait_for_high(),
                    Timer::after(Duration::from_secs(1)),
                )
                .await
            };

            let now = Instant::now();

            match edge_result {
                Either::First(()) => {
                    // Got an edge
                    let duration = now.duration_since(last_edge);
                    let micros = duration.as_micros() as u32;
                    edge_count += 1;

                    // If we were low and now high, this is a rising edge (end of gap)
                    if !is_high {
                        // Store gap in buffer
                        if pulse_count < pulse_buffer.len() {
                            pulse_buffer[pulse_count] = micros;
                            pulse_count += 1;
                        } else {
                            // Overflow -> decode and reset
                            match rubicson::decode_gaps(&pulse_buffer) {
                                Ok(r) => {
                                    info!("Decoded: {:?}", r);
                                }
                                Err(_e) => {
                                    // Decode errors are common, don't spam
                                }
                            }
                            pulse_count = 0;
                        }

                        // Log gaps in Rubicson timing range (800-4500 µs)
                        if micros >= 800 && micros <= 4500 {
                            info!("Gap: {} us", micros);
                        }
                    }
                    last_edge = now;
                }
                Either::Second(()) => {
                    // Timeout - no edge activity
                    timeout_count += 1;
                    // Reset buffer on silence (signal lost)
                    if pulse_count > 0 {
                        pulse_count = 0;
                    }
                }
            }

            // Periodic stats report every 5 seconds
            let elapsed = now.duration_since(last_stats_report);
            if elapsed > Duration::from_secs(5) {
                let current_state = if self.pin.is_high() { "HIGH" } else { "LOW" };
                info!(
                    "Stats (5s): edges={}, timeouts={}, state={}, buf={}",
                    edge_count, timeout_count, current_state, pulse_count
                );
                edge_count = 0;
                timeout_count = 0;
                last_stats_report = now;
            }
        }
    }
}
