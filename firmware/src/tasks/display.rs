use defmt::{error, info};
use esp_hal::Blocking;
use esp_hal::i2c::master::I2c;
use karu_menu::MenuRenderer;

use crate::app_bus::{self, AppEvent, UiInputEvent};
use crate::display_driver::{Display, Sh1106Display};
use crate::time_sync::TIME_WATCH;
use crate::ui_input::{EC11RotaryEncoderInput, UiEvent, UiInput};

mod controller;
pub(crate) mod state;

use controller::{DisplayController, DisplayLoopEvent, next_display_loop_event};
use state::{DisplayState, RadioState};

#[embassy_executor::task]
pub async fn ui_input_task(
    mut ui: EC11RotaryEncoderInput,
    app_event_sender: app_bus::AppEventSender,
) {
    loop {
        let event = ui
            .next_event(UiEvent::NextScreen, UiEvent::PrevScreen)
            .await;
        app_event_sender
            .send(AppEvent::UiInput(UiInputEvent::Navigation(event)))
            .await;
    }
}

#[embassy_executor::task]
#[allow(clippy::too_many_lines, clippy::expect_used)]
pub async fn display_task(
    mut display: Sh1106Display<I2c<'static, Blocking>>,
    mut receiver: app_bus::ReadingReceiver,
    app_event_receiver: app_bus::AppEventReceiver,
    mut radio_settings_receiver: app_bus::RadioSettingsReceiver,
    mut power_settings_receiver: app_bus::PowerSettingsReceiver,
    app_command_sender: app_bus::AppCommandSender,
) {
    info!("Display task started");

    // Get a receiver for time updates
    let mut time_receiver = TIME_WATCH.receiver().expect("Failed to get time receiver");

    // Show waiting message
    if display.show_status("Waiting...").is_err() {
        error!("Display: Failed to show status");
    }

    let mut controller = DisplayController::new(
        radio_settings_receiver.try_get(),
        power_settings_receiver.try_get(),
    );

    loop {
        let loop_event = next_display_loop_event(&mut receiver, &app_event_receiver).await;

        if matches!(loop_event, DisplayLoopEvent::Ignore) {
            continue;
        }

        if let Some(command) = controller.handle_event(loop_event) {
            app_command_sender.send(command).await;
        }

        controller.sync_from_watchers(
            radio_settings_receiver.try_get(),
            power_settings_receiver.try_get(),
        );

        let current_time = time_receiver.try_get().flatten();
        let frame =
            controller.derive_frame_key(current_time.map(|time_ref| time_ref.now_unix_secs()));

        if !controller.should_render(frame) {
            continue;
        }

        let render_ok = match controller.state() {
            DisplayState::Main => {
                if let Some(reading) = controller.last_reading() {
                    let mut time_str: heapless::String<16> = heapless::String::new();
                    let timestamp = current_time.map(|time_ref| {
                        time_ref.format_time(&mut time_str);
                        time_str.as_str()
                    });

                    if let Err(e) = display.show_temperature(
                        reading.inner.temperature_c,
                        reading.inner.id,
                        reading.inner.channel,
                        reading.inner.battery_ok,
                        timestamp,
                    ) {
                        error!("Display: Failed to update: {:?}", e);
                        false
                    } else {
                        true
                    }
                } else if let Err(e) = display.show_status("Waiting...") {
                    error!("Display: Failed to show waiting: {:?}", e);
                    false
                } else {
                    true
                }
            }
            DisplayState::Radio(RadioState::Overview) => {
                let (rssi, detection_threshold) = controller.radio_overview_values();

                if let Err(e) = display.show_radio_info(rssi, detection_threshold) {
                    error!("Display: Failed to show radio info: {:?}", e);
                    false
                } else {
                    true
                }
            }
            DisplayState::Radio(RadioState::Settings) => display
                .clear()
                .and_then(|()| display.draw_header("SETTINGS"))
                .and_then(|()| {
                    let renderer = MenuRenderer::new(controller.settings_menu());
                    renderer
                        .render_items()
                        .iter()
                        .try_for_each(|item| display.draw_menu_item(item))?;
                    if let Some(indicator) = renderer.scroll_indicator() {
                        display.draw_scroll_indicator(indicator)?;
                    }
                    display.flush()
                })
                .map_or_else(
                    |e| {
                        error!("Display: Failed to show settings: {:?}", e);
                        false
                    },
                    |()| true,
                ),
        };

        if render_ok {
            controller.mark_rendered(frame);
        }
    }
}
