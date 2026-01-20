//! UI input abstraction for rotary encoder (TRA/TRB)

use core::future::Future;
use embassy_time::{Duration, Timer};
use esp_hal::gpio::Input;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiEvent {
    NextScreen,
    PrevScreen,
    Select,
}

pub trait UiInput {
    fn next_event(&mut self, cw: UiEvent, ccw: UiEvent) -> impl Future<Output = UiEvent>;
}

pub struct EC11RotaryEncoderInput {
    a: Input<'static>,
    b: Input<'static>,
    sw: Input<'static>,
}

impl EC11RotaryEncoderInput {
    #[must_use]
    pub fn new(pin_a: Input<'static>, pin_b: Input<'static>, pin_sw: Input<'static>) -> Self {
        Self {
            a: pin_a,
            b: pin_b,
            sw: pin_sw,
        }
    }
}

impl UiInput for EC11RotaryEncoderInput {
    async fn next_event(&mut self, cw: UiEvent, ccw: UiEvent) -> UiEvent {
        use embassy_futures::select::{Either3, select3};

        loop {
            // Wait for either: button press, or rotation (rising edge on A)
            match select3(
                self.sw.wait_for_falling_edge(),
                self.a.wait_for_rising_edge(),
                core::future::pending::<()>(), // placeholder for third arm
            )
            .await
            {
                Either3::First(()) => {
                    // Button pressed (active low)
                    Timer::after(Duration::from_millis(50)).await; // Debounce
                    if self.sw.is_low() {
                        // Wait for release to avoid repeated triggers
                        self.sw.wait_for_rising_edge().await;
                        Timer::after(Duration::from_millis(50)).await; // Debounce release
                        return UiEvent::Select;
                    }
                }
                Either3::Second(()) => {
                    // Rotation detected - sample B immediately
                    let b_at_edge = self.b.is_low();

                    // Debounce - wait for contacts to settle
                    Timer::after(Duration::from_millis(2)).await;

                    // Confirm A is still high (valid edge, not noise)
                    if !self.a.is_high() {
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
                Either3::Third(()) => unreachable!(),
            }
        }
    }
}
