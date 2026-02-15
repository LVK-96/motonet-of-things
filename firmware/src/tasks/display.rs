use core::fmt::Write;

use defmt::{error, info};
use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::{channel, watch};
use embassy_time::{Duration, Timer};
use esp_hal::Blocking;
use esp_hal::i2c::master::I2c;
use heapless::String;
use karu_menu::{
    ActionItem, Menu, MenuEntry, MenuEvent, MenuRenderer, NumericItem, ScrollableViewport,
    UiEvent as MenuUiEvent,
};

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
    Radio(RadioState),
}

#[derive(Clone, Copy)]
enum RadioState {
    Overview,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKey {
    Waiting,
    Main(MainFrameKey),
    Radio(RadioFrameKey),
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct MainFrameKey {
    temp_deci: i16,
    sensor_id: u8,
    channel: u8,
    battery_ok: bool,
    time_secs: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RadioFrameKey {
    Overview {
        rssi: Option<i16>,
        detection_threshold: u8,
    },
    Settings(SettingsFrameKey),
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SettingsFrameKey {
    nav_index: u8,
    editing: bool,
    threshold: u8,
    magn: u8,
    bandwidth_index: u8,
    carrier_sense: u8,
}

const SETTINGS_MENU_CAPACITY: usize = 5;
const THRESHOLD_ITEM_INDEX: usize = 0;
const MAGN_ITEM_INDEX: usize = 1;
const BANDWIDTH_ITEM_INDEX: usize = 2;
const CARRIER_SENSE_ITEM_INDEX: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsAction {
    Save,
}

type SettingsMenu = Menu<MenuEntry<u8, SettingsAction>, SETTINGS_MENU_CAPACITY>;

fn format_db(value: u8) -> String<32> {
    let mut out = String::new();
    let _ = write!(out, "{value}dB");
    out
}

fn format_magn_target(value: u8) -> String<32> {
    let mut out = String::new();
    let magnitude_db = match value {
        0 => 24,
        1 => 27,
        2 => 30,
        3 => 33,
        4 => 36,
        5 => 38,
        6 => 40,
        _ => 42,
    };
    let _ = write!(out, "{magnitude_db}dB");
    out
}

fn format_bandwidth(value: u8) -> String<32> {
    let mut out = String::new();
    let bandwidth_khz = channel_bandwidth_hz(value) / 1000;
    let _ = write!(out, "{bandwidth_khz}kHz");
    out
}

fn menu_event_from_ui_event(event: UiEvent) -> MenuUiEvent {
    match event {
        UiEvent::NextScreen => MenuUiEvent::NextScreen,
        UiEvent::PrevScreen => MenuUiEvent::PrevScreen,
        UiEvent::Select => MenuUiEvent::Select,
    }
}

fn reset_settings_menu_for_entry(menu: &mut SettingsMenu) {
    if menu.is_editing() {
        let _ = menu.handle_event(MenuUiEvent::Select);
    }
    let _ = menu.set_selected(0);
}

fn add_menu_item(menu: &mut SettingsMenu, item: MenuEntry<u8, SettingsAction>, item_label: &str) {
    if menu.add(item).is_err() {
        error!("Settings menu full while adding {}", item_label);
    }
}

fn build_settings_menu(initial: RadioSettings) -> SettingsMenu {
    let mut menu = Menu::new(ScrollableViewport::new(128, 64));

    add_menu_item(
        &mut menu,
        MenuEntry::numeric(NumericItem::new(
            "Threshold",
            initial.detection_threshold_db,
            DETECTION_THRESHOLD_MIN_DB..=DETECTION_THRESHOLD_MAX_DB,
            DETECTION_THRESHOLD_STEP_DB,
            format_db,
        )),
        "Threshold",
    );
    add_menu_item(
        &mut menu,
        MenuEntry::numeric(NumericItem::new(
            "Magn Tgt",
            initial.magn_target,
            MAGN_TARGET_MIN..=MAGN_TARGET_MAX,
            1,
            format_magn_target,
        )),
        "Magn Tgt",
    );
    add_menu_item(
        &mut menu,
        MenuEntry::numeric(NumericItem::new(
            "Bandwidth",
            initial.channel_bandwidth_index,
            CHANNEL_BANDWIDTH_MIN_INDEX..=CHANNEL_BANDWIDTH_MAX_INDEX,
            1,
            format_bandwidth,
        )),
        "Bandwidth",
    );
    add_menu_item(
        &mut menu,
        MenuEntry::numeric(NumericItem::new(
            "CS Thrsh",
            initial.carrier_sense_threshold,
            CARRIER_SENSE_MIN..=CARRIER_SENSE_MAX,
            1,
            format_db,
        )),
        "CS Thrsh",
    );
    add_menu_item(
        &mut menu,
        MenuEntry::action(ActionItem::new("Save", SettingsAction::Save)),
        "Save",
    );

    menu
}

fn numeric_value_at(menu: &SettingsMenu, index: usize) -> Option<u8> {
    menu.items().get(index).and_then(|entry| match entry {
        MenuEntry::Numeric(item) => Some(item.pending()),
        MenuEntry::Command(_) => None,
    })
}

fn sync_pending_from_menu(menu: &SettingsMenu, pending: &mut RadioSettings) {
    pending.detection_threshold_db =
        numeric_value_at(menu, THRESHOLD_ITEM_INDEX).unwrap_or(pending.detection_threshold_db);
    pending.magn_target = numeric_value_at(menu, MAGN_ITEM_INDEX).unwrap_or(pending.magn_target);
    pending.channel_bandwidth_index =
        numeric_value_at(menu, BANDWIDTH_ITEM_INDEX).unwrap_or(pending.channel_bandwidth_index);
    pending.carrier_sense_threshold =
        numeric_value_at(menu, CARRIER_SENSE_ITEM_INDEX).unwrap_or(pending.carrier_sense_threshold);
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
    mut settings_receiver: watch::Receiver<'static, CriticalSectionRawMutex, RadioSettings, 2>,
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

    let mut pending_settings = settings_receiver
        .try_get()
        .unwrap_or(DEFAULT_RADIO_SETTINGS);
    let mut settings_menu = build_settings_menu(pending_settings);
    sync_pending_from_menu(&settings_menu, &mut pending_settings);

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
                        UiEvent::NextScreen | UiEvent::PrevScreen => DisplayState::Radio(RadioState::Overview),
                        UiEvent::Select => {
                            reset_settings_menu_for_entry(&mut settings_menu);
                            DisplayState::Radio(RadioState::Settings)
                        }
                    },
                    DisplayState::Radio(RadioState::Overview) => match event {
                        UiEvent::NextScreen | UiEvent::PrevScreen => DisplayState::Main,
                        UiEvent::Select => {
                            reset_settings_menu_for_entry(&mut settings_menu);
                            DisplayState::Radio(RadioState::Settings)
                        }
                    },
                    DisplayState::Radio(RadioState::Settings) => {
                        let menu_event =
                            settings_menu.handle_event(menu_event_from_ui_event(event));
                        if matches!(menu_event, MenuEvent::ValueChanged(_)) {
                            sync_pending_from_menu(&settings_menu, &mut pending_settings);
                        }

                        match menu_event {
                            MenuEvent::ActionSelected {
                                action: SettingsAction::Save,
                                ..
                            } => {
                                sync_pending_from_menu(&settings_menu, &mut pending_settings);
                                settings_menu.commit_all();
                                settings_sender.send(pending_settings);
                                let bandwidth_khz =
                                    channel_bandwidth_hz(pending_settings.channel_bandwidth_index)
                                        / 1000;
                                info!(
                                    "Settings saved: threshold={} dB, magn_target={}, bandwidth={} kHz, carrier_sense={}",
                                    pending_settings.detection_threshold_db,
                                    pending_settings.magn_target,
                                    bandwidth_khz,
                                    pending_settings.carrier_sense_threshold
                                );
                                DisplayState::Radio(RadioState::Overview)
                            }
                            _ => DisplayState::Radio(RadioState::Settings),
                        }
                    }
                };
            }
            Either3::Third(()) => {
                // Tick to allow clock-driven updates on the main screen.
            }
        }

        if let Some(current_settings) = settings_receiver.try_get()
            && current_settings != pending_settings
            && !matches!(state, DisplayState::Radio(RadioState::Settings))
        {
            pending_settings = current_settings;
            settings_menu = build_settings_menu(pending_settings);
        }

        let current_time = time_receiver.try_get().flatten();
        let frame = match state {
            DisplayState::Main => {
                last_reading.map_or(FrameKey::Waiting, |reading| {
                    FrameKey::Main(MainFrameKey {
                        temp_deci: temp_to_deci(reading.inner.temperature_c),
                        sensor_id: reading.inner.id,
                        channel: reading.inner.channel,
                        battery_ok: reading.inner.battery_ok,
                        time_secs: current_time.map(|t| t.now_unix_secs()),
                    })
                })
            }
            DisplayState::Radio(RadioState::Overview) => {
                let rssi = last_reading.map(|r| r.rssi);
                let detection_threshold = last_reading
                    .map_or(pending_settings.detection_threshold_db, |r| {
                        r.detection_threshold
                    });
                FrameKey::Radio(RadioFrameKey::Overview {
                    rssi,
                    detection_threshold,
                })
            }
            DisplayState::Radio(RadioState::Settings) => FrameKey::Radio(RadioFrameKey::Settings(SettingsFrameKey {
                nav_index: u8::try_from(settings_menu.selected_index()).map_or(u8::MAX, |v| v),
                editing: settings_menu.is_editing(),
                threshold: pending_settings.detection_threshold_db,
                magn: pending_settings.magn_target,
                bandwidth_index: pending_settings.channel_bandwidth_index,
                carrier_sense: pending_settings.carrier_sense_threshold,
            })),
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
            DisplayState::Radio(RadioState::Overview) => {
                let rssi = last_reading.map(|r| r.rssi);
                let det_threshold = last_reading
                    .map_or(pending_settings.detection_threshold_db, |r| {
                        r.detection_threshold
                    });
                if let Err(e) = display.show_radio_info(rssi, det_threshold) {
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
                    let renderer = MenuRenderer::new(&settings_menu);
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
            last_frame = Some(frame);
        }
    }
}
