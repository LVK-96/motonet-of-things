use embassy_time::Instant;
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
        let mut last_edge = Instant::now();
        let mut current_bits = [0u8; 5]; // 36 bits fits in 5 bytes
        let mut bit_index = 0;

        // Wait for initial long gap/silence to sync?
        // Or just start.

        loop {
            // Wait for rising edge (start of pulse)
            self.pin.wait_for_high().await;
            let rising_edge = Instant::now();

            // Calculate time since last rising edge (Pulse + Gap from previous bit)
            // Or measure Gap: from last falling edge to this rising edge.

            // Let's track:
            // 1. Wait for High (Start Pulse)
            // 2. Wait for Low (End Pulse) -> Record Pulse Width
            // 3. Wait for High (Start Next Pulse) -> Record Gap Width (Low duration)

            // Actually, we are at Rising Edge now.
            // Previous state was Low (Gap).
            let gap_duration = rising_edge.duration_since(last_edge);

            // Validate Gap
            // Short: ~1000us -> 0
            // Long: ~2000us -> 1
            // Sync/Reset: > 3000us -> Start of new packet / End of old

            let micros = gap_duration.as_micros();

            if micros > 4000 {
                // End of packet or noise. Reset.
                if bit_index >= 36 {
                    // Try to decode what we have
                    match rubicson::decode_rubicson(&current_bits, bit_index) {
                        Ok(reading) => {
                            defmt::info!("Rubicson: {:?}", reading);
                        }
                        Err(_e) => {
                            // defmt::debug!("Decode failed: {:?}", e);
                        }
                    }
                }
                // Reset
                bit_index = 0;
                current_bits = [0u8; 5];
            } else if micros > 1500 {
                // Long gap -> 1
                add_bit(&mut current_bits, bit_index, 1);
                bit_index += 1;
            } else if micros > 500 {
                // Short gap -> 0
                add_bit(&mut current_bits, bit_index, 0);
                bit_index += 1;
            }

            // Limit buffer
            if bit_index >= 36 * 2 {
                bit_index = 0; // Overflow
            }

            // Wait for end of pulse (Low)
            self.pin.wait_for_low().await;
            last_edge = Instant::now(); // Timestamp of falling edge (start of gap)
        }
    }
}

fn add_bit(buf: &mut [u8], index: usize, val: u8) {
    if index / 8 >= buf.len() {
        return;
    }
    if val != 0 {
        buf[index / 8] |= 1 << (7 - (index % 8)); // Big endian (MSB first) filling?
        // rtl_433 says:
        // data is grouped into 9 nibbles.
        // usually transmission is MSB first.
        // Let's assume MSB first.
    }
}
