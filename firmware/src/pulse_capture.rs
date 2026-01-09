use defmt::info;
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
        // The rubicson sensor sends 12 * 36 bit packets (432 bits)
        // We collect the gap durations in a buffer of 512 u32s
        let mut pulse_buffer = [0u32; 512];
        let mut pulse_count = 0;

        // Wait for a 1
        self.pin.wait_for_high().await;

        // Wait for falling edge
        self.pin.wait_for_low().await;
        // Timestamp of the first falling edge we detected
        let mut last_falling_edge = Instant::now();

        // pin is the raw OOK demodulated signal
        // i.e. when the radio is receiving a carrier -> 1, otherwise 0
        loop {
            // Wait for rising edge (start of next pulse)
            // Previous state was Low
            self.pin.wait_for_high().await;
            let rising_edge = Instant::now();

            // Data in encoded in the gap between pulses, long = 1, short = 0
            // Here we collect the gap durations in pulse_buffer
            // rubicson::decode_gaps will try to decode the gaps
            // once the buffer fills up
            let gap = rising_edge.duration_since(last_falling_edge);
            let micros = gap.as_micros() as u32;

            if pulse_count < pulse_buffer.len() {
                pulse_buffer[pulse_count] = micros;
                pulse_count += 1;
            } else {
                // Overflow -> decode and reset
                match rubicson::decode_gaps(&pulse_buffer) {
                    Ok(r) => {
                        info!("Decoded: {:?}", r);
                    }
                    Err(e) => {
                        info!("Decode error: {:?}", e);
                    }
                }
                pulse_count = 0;
            }

            info!("Gap: {}", micros);

            // Wait for falling edge (end of pulse)
            self.pin.wait_for_low().await;
            last_falling_edge = Instant::now(); // Timestamp of falling edge (start of gap)
        }
    }
}
