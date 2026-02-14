use defmt::{error, info};
use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::{channel, watch};
use embassy_time::{Duration, Timer};
use esp_hal::Blocking;
use esp_hal::i2c::master::I2c;

use crate::display::{Display, Sh1106Display};
use crate::messages::{
    CARRIER_SENSE_MAX, CARRIER_SENSE_MIN, CHANNEL_BANDWIDTH_MAX_INDEX, CHANNEL_BANDWIDTH_MIN_INDEX,
    DEFAULT_RADIO_SETTINGS, DETECTION_THRESHOLD_MAX_DB, DETECTION_THRESHOLD_MIN_DB,
    DETECTION_THRESHOLD_STEP_DB, MAGN_TARGET_MAX, MAGN_TARGET_MIN, RadioReading, RadioSettings,
    channel_bandwidth_hz,
};
use crate::time_sync::TIME_WATCH;
use crate::ui_input::{EC11RotaryEncoderInput, UiEvent, UiInput};

#[embassy_executor::task]
pub async fn ui_input_task(
    mut ui: EC11RotaryEncoderInput,
    ui_event_sender: channel::Sender<'static, CriticalSectionRawMutex, UiEvent, 8>,
) {
    loop {
        let event = ui
            .next_event(UiEvent::NextScreen, UiEvent::PrevScreen)
            .await;
        ui_event_sender.send(event).await;
    }
}

#[derive(Clone, Copy)]
enum DisplayState {
    Main,
    Radio,
    Settings { nav_index: u8, editing: bool },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKey {
    Waiting,
    Main {
        temp_deci: i16,
        sensor_id: u8,
        channel: u8,
        battery_ok: bool,
        time_secs: Option<u64>,
    },
    Radio {
        rssi: Option<i16>,
        detection_threshold: u8,
    },
    Settings {
        nav_index: u8,
        editing: bool,
        threshold: u8,
        magn: u8,
        bandwidth_index: u8,
        carrier_sense: u8,
    },
}

fn temp_to_deci(temp_c: f32) -> i16 {
    #[allow(clippy::cast_possible_truncation)]
    {
        (temp_c * 10.0) as i16
    }
}

#[embassy_executor::task]
#[allow(clippy::too_many_lines, clippy::expect_used)]
pub async fn display_task(
    mut display: Sh1106Display<I2c<'static, Blocking>>,
    mut receiver: watch::Receiver<'static, CriticalSectionRawMutex, RadioReading, 2>,
    ui_event_receiver: channel::Receiver<'static, CriticalSectionRawMutex, UiEvent, 8>,
    settings_sender: watch::Sender<'static, CriticalSectionRawMutex, RadioSettings, 2>,
) {
    info!("Display task started");

    // Get a receiver for time updates
    let mut time_receiver = TIME_WATCH.receiver().expect("Failed to get time receiver");

    // Show waiting message
    if display.show_status("Waiting...").is_err() {
        error!("Display: Failed to show status");
    }

    let mut state = DisplayState::Main;
    let mut last_reading: Option<RadioReading> = None;

    // Settings values (pending values, not yet applied)
    let mut pending_threshold: u8 = DEFAULT_RADIO_SETTINGS.detection_threshold_db;
    let mut pending_magn_target: u8 = DEFAULT_RADIO_SETTINGS.magn_target;
    let mut pending_bandwidth_index: u8 = DEFAULT_RADIO_SETTINGS.channel_bandwidth_index;
    let mut pending_carrier_sense: u8 = DEFAULT_RADIO_SETTINGS.carrier_sense_threshold;

    // Send initial settings
    settings_sender.send(RadioSettings {
        detection_threshold_db: pending_threshold,
        magn_target: pending_magn_target,
        channel_bandwidth_index: pending_bandwidth_index,
        carrier_sense_threshold: pending_carrier_sense,
    });

    // Skip full redraws when nothing visual has changed.
    let mut last_frame: Option<FrameKey> = None;

    loop {
        match select3(
            receiver.changed(),
            ui_event_receiver.receive(),
            Timer::after(Duration::from_millis(250)),
        )
        .await
        {
            Either3::First(reading) => {
                last_reading = Some(reading);
            }
            Either3::Second(event) => {
                // Handle UI events based on current state
                state = match state {
                    DisplayState::Main => match event {
                        UiEvent::NextScreen | UiEvent::PrevScreen => DisplayState::Radio,
                        UiEvent::Select => DisplayState::Settings {
                            nav_index: 0,
                            editing: false,
                        },
                    },
                    DisplayState::Radio => match event {
                        UiEvent::NextScreen | UiEvent::PrevScreen => DisplayState::Main,
                        UiEvent::Select => DisplayState::Settings {
                            nav_index: 0,
                            editing: false,
                        },
                    },
                    DisplayState::Settings { nav_index, editing } => match event {
                        UiEvent::Select => {
                            if editing {
                                // Exit editing mode, go back to navigation
                                DisplayState::Settings {
                                    nav_index,
                                    editing: false,
                                }
                            } else if nav_index == 4 {
                                // Save selected - apply settings and exit
                                settings_sender.send(RadioSettings {
                                    detection_threshold_db: pending_threshold,
                                    magn_target: pending_magn_target,
                                    channel_bandwidth_index: pending_bandwidth_index,
                                    carrier_sense_threshold: pending_carrier_sense,
                                });
                                let bandwidth_khz =
                                    channel_bandwidth_hz(pending_bandwidth_index) / 1000;
                                info!(
                                    "Settings saved: threshold={} dB, magn_target={}, bandwidth={} kHz, carrier_sense={}",
                                    pending_threshold, pending_magn_target, bandwidth_khz, pending_carrier_sense
                                );
                                DisplayState::Radio
                            } else {
                                // Enter editing mode for current item
                                DisplayState::Settings {
                                    nav_index,
                                    editing: true,
                                }
                            }
                        }
                        UiEvent::NextScreen => {
                            if editing {
                                // Adjust value up
                                match nav_index {
                                    0 => {
                                        // Detection threshold: 4, 8, 12, 16 dB
                                        pending_threshold =
                                            if pending_threshold >= DETECTION_THRESHOLD_MAX_DB {
                                                DETECTION_THRESHOLD_MAX_DB
                                            } else {
                                                pending_threshold + DETECTION_THRESHOLD_STEP_DB
                                            };
                                    }
                                    1 => {
                                        // Magn target: 0-7
                                        pending_magn_target =
                                            if pending_magn_target >= MAGN_TARGET_MAX {
                                                MAGN_TARGET_MAX
                                            } else {
                                                pending_magn_target + 1
                                            };
                                    }
                                    2 => {
                                        // Channel bandwidth: 0-3 (325/203/162/135 kHz)
                                        pending_bandwidth_index = if pending_bandwidth_index
                                            >= CHANNEL_BANDWIDTH_MAX_INDEX
                                        {
                                            CHANNEL_BANDWIDTH_MAX_INDEX
                                        } else {
                                            pending_bandwidth_index + 1
                                        };
                                    }
                                    3 => {
                                        // Carrier sense threshold: 0-7
                                        pending_carrier_sense =
                                            if pending_carrier_sense >= CARRIER_SENSE_MAX {
                                                CARRIER_SENSE_MAX
                                            } else {
                                                pending_carrier_sense + 1
                                            };
                                    }
                                    _ => {}
                                }
                                DisplayState::Settings { nav_index, editing }
                            } else {
                                // Navigate to next item (wrap around)
                                let next = if nav_index >= 4 { 0 } else { nav_index + 1 };
                                DisplayState::Settings {
                                    nav_index: next,
                                    editing: false,
                                }
                            }
                        }
                        UiEvent::PrevScreen => {
                            if editing {
                                // Adjust value down
                                match nav_index {
                                    0 => {
                                        pending_threshold =
                                            if pending_threshold <= DETECTION_THRESHOLD_MIN_DB {
                                                DETECTION_THRESHOLD_MIN_DB
                                            } else {
                                                pending_threshold - DETECTION_THRESHOLD_STEP_DB
                                            };
                                    }
                                    1 => {
                                        pending_magn_target =
                                            if pending_magn_target == MAGN_TARGET_MIN {
                                                MAGN_TARGET_MIN
                                            } else {
                                                pending_magn_target - 1
                                            };
                                    }
                                    2 => {
                                        pending_bandwidth_index = if pending_bandwidth_index
                                            <= CHANNEL_BANDWIDTH_MIN_INDEX
                                        {
                                            CHANNEL_BANDWIDTH_MIN_INDEX
                                        } else {
                                            pending_bandwidth_index - 1
                                        };
                                    }
                                    3 => {
                                        // Carrier sense threshold: 0-7
                                        pending_carrier_sense =
                                            if pending_carrier_sense <= CARRIER_SENSE_MIN {
                                                CARRIER_SENSE_MIN
                                            } else {
                                                pending_carrier_sense - 1
                                            };
                                    }
                                    _ => {}
                                }
                                DisplayState::Settings { nav_index, editing }
                            } else {
                                // Navigate to previous item (wrap around)
                                let prev = if nav_index == 0 { 4 } else { nav_index - 1 };
                                DisplayState::Settings {
                                    nav_index: prev,
                                    editing: false,
                                }
                            }
                        }
                    },
                };
            }
            Either3::Third(()) => {
                // Tick to allow clock-driven updates on the main screen.
            }
        }

        let current_time = time_receiver.try_get().flatten();
        let frame = match state {
            DisplayState::Main => {
                last_reading.map_or(FrameKey::Waiting, |reading| FrameKey::Main {
                    temp_deci: temp_to_deci(reading.inner.temperature_c),
                    sensor_id: reading.inner.id,
                    channel: reading.inner.channel,
                    battery_ok: reading.inner.battery_ok,
                    time_secs: current_time.map(|t| t.now_unix_secs()),
                })
            }
            DisplayState::Radio => {
                let rssi = last_reading.map(|r| r.rssi);
                let detection_threshold =
                    last_reading.map_or(pending_threshold, |r| r.detection_threshold);
                FrameKey::Radio {
                    rssi,
                    detection_threshold,
                }
            }
            DisplayState::Settings { nav_index, editing } => FrameKey::Settings {
                nav_index,
                editing,
                threshold: pending_threshold,
                magn: pending_magn_target,
                bandwidth_index: pending_bandwidth_index,
                carrier_sense: pending_carrier_sense,
            },
        };

        if Some(frame) == last_frame {
            continue;
        }

        let render_ok = match state {
            DisplayState::Main => {
                if let Some(reading) = last_reading {
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
            DisplayState::Radio => {
                let rssi = last_reading.map(|r| r.rssi);
                let det_threshold =
                    last_reading.map_or(pending_threshold, |r| r.detection_threshold);
                if let Err(e) = display.show_radio_info(rssi, det_threshold) {
                    error!("Display: Failed to show radio info: {:?}", e);
                    false
                } else {
                    true
                }
            }
            DisplayState::Settings { nav_index, editing } => {
                if let Err(e) = display.show_settings_menu(
                    nav_index,
                    editing,
                    pending_threshold,
                    pending_magn_target,
                    pending_bandwidth_index,
                    pending_carrier_sense,
                ) {
                    error!("Display: Failed to show settings: {:?}", e);
                    false
                } else {
                    true
                }
            }
        };

        if render_ok {
            last_frame = Some(frame);
        }
    }
}
