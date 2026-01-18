//! UI input abstraction for rotary encoder (TRA/TRB)

use core::future::Future;
use embassy_time::{Duration, Timer};
use esp_hal::gpio::Input;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiEvent {
    NextScreen,
    PrevScreen,
}

pub trait UiInput {
    fn next_event(&mut self, cw: UiEvent, ccw: UiEvent) -> impl Future<Output = UiEvent>;
}

pub struct EC11RotaryEncoderInput {
    pin_a: Input<'static>,
    pin_b: Input<'static>,
}

impl EC11RotaryEncoderInput {
    pub fn new(pin_a: Input<'static>, pin_b: Input<'static>) -> Self {
        Self { pin_a, pin_b }
    }
}

impl UiInput for EC11RotaryEncoderInput {
    async fn next_event(&mut self, cw: UiEvent, ccw: UiEvent) -> UiEvent {
        loop {
            // Wait for rising edge on A
            self.pin_a.wait_for_rising_edge().await;

            // Sample B immediately at the moment of the edge (before bounce settles)
            let b_at_edge = self.pin_b.is_low();

            // Debounce - wait for contacts to settle
            Timer::after(Duration::from_millis(2)).await;

            // Confirm A is still high (valid edge, not noise)
            if !self.pin_a.is_high() {
                continue;
            }

            // Direction based on B state at the moment of A's rising edge:
            // B Low at A rising -> CW
            // B High at A rising -> CCW
            let event = if b_at_edge { cw } else { ccw };

            // Cooldown to prevent double-firing on same detent
            Timer::after(Duration::from_millis(200)).await;

            return event;
        }
    }
}
