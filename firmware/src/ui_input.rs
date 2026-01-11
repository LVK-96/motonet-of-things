//! UI input abstraction for rotary encoder (TRA/TRB)

use embassy_time::{Duration, Timer};
use esp_hal::gpio::Input;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiEvent {
    NextScreen,
    PrevScreen,
}

pub trait UiInput {
    fn next_event(&mut self) -> impl Future<Output = UiEvent>;
}

pub struct EC11RotaryEncoderInput {
    pin_a: Input<'static>,
    pin_b: Input<'static>,
    last_state: u8,
}

impl EC11RotaryEncoderInput {
    pub fn new(pin_a: Input<'static>, pin_b: Input<'static>) -> Self {
        let a = pin_a.is_high();
        let b = pin_b.is_high();
        let last_state = (a as u8) << 1 | (b as u8);
        Self { pin_a, pin_b, last_state }
    }
}

impl UiInput for EC11RotaryEncoderInput {
    async fn next_event(&mut self) -> UiEvent {
        loop {
            self.pin_a.wait_for_rising_edge().await;
            let a: u8 = 1; // Rising edge -> a is 1
            let b = self.pin_b.is_high() as u8;
            let state = (a << 1) | b;
            if state != self.last_state {
                let last_a = (self.last_state >> 1) & 0b1;
                let a_rose = last_a == 0 && a == 1;
                self.last_state = state;

                // 00->01->11->10->00 is one direction, reverse is the other
                // We'll just check for A rising/falling for simplicity
                if a_rose && b == 0 {
                    return UiEvent::NextScreen;
                } else if a_rose && b == 1 {
                    return UiEvent::PrevScreen;
                }
            }
            Timer::after(Duration::from_millis(2)).await;
        }
    }
}
