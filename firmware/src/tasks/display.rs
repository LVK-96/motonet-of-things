use core::fmt::Write;

use app_core::config_rules::{clamp_power_config, clamp_radio_config};
use app_core::display_model::{DisplayFrameInput, FrameKey, derive_frame};
use app_core::domain::{PowerConfigView, RadioConfigView, SensorReading, UiScreenState};
use defmt::{error, info};
use embassy_futures::select::{Either3, select3};
use embassy_time::{Duration, Timer};
use esp_hal::Blocking;
use esp_hal::i2c::master::I2c;
use heapless::String;
use karu_menu::{
    ActionItem, Menu, MenuEntry, MenuEvent, MenuRenderer, NumericItem, ScrollableViewport,
    UiEvent as MenuUiEvent,
};

use crate::app_bus::{self, AppCommand, AppEvent, UiInputEvent};
use crate::display::{Display, Sh1106Display};
use crate::messages::{
    CARRIER_SENSE_MAX, CARRIER_SENSE_MIN, CHANNEL_BANDWIDTH_MAX_INDEX, CHANNEL_BANDWIDTH_MIN_INDEX,
    DEFAULT_POWER_SETTINGS, DEFAULT_RADIO_SETTINGS, DETECTION_THRESHOLD_MAX_DB,
    DETECTION_THRESHOLD_MIN_DB, DETECTION_THRESHOLD_STEP_DB, MAGN_TARGET_MAX, MAGN_TARGET_MIN,
    POWER_SLEEP_DURATION_MAX_SECS, POWER_SLEEP_DURATION_MIN_SECS, POWER_UI_IDLE_TIMEOUT_MAX_SECS,
    POWER_UI_IDLE_TIMEOUT_MIN_SECS, PowerSettings, RadioReading, RadioSettings,
    channel_bandwidth_hz,
};
use crate::power;
use crate::time_sync::TIME_WATCH;
use crate::ui_input::{EC11RotaryEncoderInput, UiEvent, UiInput};

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

const SETTINGS_MENU_CAPACITY: usize = 8;
const THRESHOLD_ITEM_INDEX: usize = 0;
const MAGN_ITEM_INDEX: usize = 1;
const BANDWIDTH_ITEM_INDEX: usize = 2;
const CARRIER_SENSE_ITEM_INDEX: usize = 3;
const PREDICTIVE_SLEEP_ITEM_INDEX: usize = 4;
const SLEEP_DURATION_ITEM_INDEX: usize = 5;
const UI_IDLE_TIMEOUT_ITEM_INDEX: usize = 6;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsAction {
    Save,
}

type SettingsMenu = Menu<MenuEntry<u8, SettingsAction>, SETTINGS_MENU_CAPACITY>;

fn to_ui_screen_state(state: DisplayState) -> UiScreenState {
    match state {
        DisplayState::Main => UiScreenState::Main,
        DisplayState::Radio(RadioState::Overview) => UiScreenState::RadioOverview,
        DisplayState::Radio(RadioState::Settings) => UiScreenState::RadioSettings,
    }
}

fn to_sensor_reading(reading: RadioReading) -> SensorReading {
    SensorReading {
        sensor_id: reading.inner.id,
        channel: reading.inner.channel,
        battery_ok: reading.inner.battery_ok,
        temperature_c: reading.inner.temperature_c,
        rssi_dbm: reading.rssi,
        detection_threshold_db: reading.detection_threshold,
    }
}

fn to_radio_config_view(settings: RadioSettings) -> RadioConfigView {
    RadioConfigView {
        detection_threshold_db: settings.detection_threshold_db,
        magn_target: settings.magn_target,
        channel_bandwidth_index: settings.channel_bandwidth_index,
        carrier_sense_threshold: settings.carrier_sense_threshold,
    }
}

fn to_power_config_view(settings: PowerSettings) -> PowerConfigView {
    PowerConfigView {
        predictive_sleep_enabled: settings.predictive_sleep_enabled,
        sleep_duration_secs: settings.sleep_duration_secs,
        ui_idle_timeout_secs: settings.ui_idle_timeout_secs,
    }
}

fn from_radio_config_view(config: RadioConfigView) -> RadioSettings {
    RadioSettings {
        detection_threshold_db: config.detection_threshold_db,
        magn_target: config.magn_target,
        channel_bandwidth_index: config.channel_bandwidth_index,
        carrier_sense_threshold: config.carrier_sense_threshold,
    }
}

fn from_power_config_view(config: PowerConfigView) -> PowerSettings {
    PowerSettings {
        predictive_sleep_enabled: config.predictive_sleep_enabled,
        sleep_duration_secs: config.sleep_duration_secs,
        ui_idle_timeout_secs: config.ui_idle_timeout_secs,
    }
}

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

fn format_on_off(value: u8) -> String<32> {
    let mut out = String::new();
    let _ = write!(out, "{}", if value == 0 { "Off" } else { "On" });
    out
}

fn format_seconds(value: u8) -> String<32> {
    let mut out = String::new();
    let _ = write!(out, "{value}s");
    out
}

fn menu_event_from_ui_event(event: UiEvent) -> MenuUiEvent {
    match event {
        UiEvent::NextScreen => MenuUiEvent::NextScreen,
        UiEvent::PrevScreen => MenuUiEvent::PrevScreen,
        UiEvent::Select => MenuUiEvent::Select,
    }
}

fn navigation_event_from_app_event(event: AppEvent) -> Option<UiEvent> {
    match event {
        AppEvent::UiInput(UiInputEvent::Navigation(ui_event)) => Some(ui_event),
        _ => None,
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

fn build_settings_menu(initial_radio: RadioSettings, initial_power: PowerSettings) -> SettingsMenu {
    let mut menu = Menu::new(ScrollableViewport::new(128, 64));

    add_menu_item(
        &mut menu,
        MenuEntry::numeric(NumericItem::new(
            "Threshold",
            initial_radio.detection_threshold_db,
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
            initial_radio.magn_target,
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
            initial_radio.channel_bandwidth_index,
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
            initial_radio.carrier_sense_threshold,
            CARRIER_SENSE_MIN..=CARRIER_SENSE_MAX,
            1,
            format_db,
        )),
        "CS Thrsh",
    );
    add_menu_item(
        &mut menu,
        MenuEntry::numeric(NumericItem::new(
            "Pred Slp",
            u8::from(initial_power.predictive_sleep_enabled),
            0..=1,
            1,
            format_on_off,
        )),
        "Pred Slp",
    );
    add_menu_item(
        &mut menu,
        MenuEntry::numeric(NumericItem::new(
            "Sleep",
            initial_power.sleep_duration_secs,
            POWER_SLEEP_DURATION_MIN_SECS..=POWER_SLEEP_DURATION_MAX_SECS,
            1,
            format_seconds,
        )),
        "Sleep",
    );
    add_menu_item(
        &mut menu,
        MenuEntry::numeric(NumericItem::new(
            "UI Idle",
            initial_power.ui_idle_timeout_secs,
            POWER_UI_IDLE_TIMEOUT_MIN_SECS..=POWER_UI_IDLE_TIMEOUT_MAX_SECS,
            1,
            format_seconds,
        )),
        "UI Idle",
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

fn sync_pending_from_menu(
    menu: &SettingsMenu,
    pending_radio: &mut RadioSettings,
    pending_power: &mut PowerSettings,
) {
    pending_radio.detection_threshold_db = numeric_value_at(menu, THRESHOLD_ITEM_INDEX)
        .unwrap_or(pending_radio.detection_threshold_db);
    pending_radio.magn_target =
        numeric_value_at(menu, MAGN_ITEM_INDEX).unwrap_or(pending_radio.magn_target);
    pending_radio.channel_bandwidth_index = numeric_value_at(menu, BANDWIDTH_ITEM_INDEX)
        .unwrap_or(pending_radio.channel_bandwidth_index);
    pending_radio.carrier_sense_threshold = numeric_value_at(menu, CARRIER_SENSE_ITEM_INDEX)
        .unwrap_or(pending_radio.carrier_sense_threshold);
    pending_power.predictive_sleep_enabled =
        numeric_value_at(menu, PREDICTIVE_SLEEP_ITEM_INDEX).is_some_and(|v| v != 0);
    pending_power.sleep_duration_secs = numeric_value_at(menu, SLEEP_DURATION_ITEM_INDEX)
        .unwrap_or(pending_power.sleep_duration_secs);
    pending_power.ui_idle_timeout_secs = numeric_value_at(menu, UI_IDLE_TIMEOUT_ITEM_INDEX)
        .unwrap_or(pending_power.ui_idle_timeout_secs);
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

    let mut state = DisplayState::Main;
    let mut last_reading: Option<RadioReading> = None;

    let mut pending_radio_settings = radio_settings_receiver
        .try_get()
        .unwrap_or(DEFAULT_RADIO_SETTINGS);
    let mut pending_power_settings = power_settings_receiver
        .try_get()
        .unwrap_or(DEFAULT_POWER_SETTINGS);
    let mut settings_menu = build_settings_menu(pending_radio_settings, pending_power_settings);
    sync_pending_from_menu(
        &settings_menu,
        &mut pending_radio_settings,
        &mut pending_power_settings,
    );

    // Skip full redraws when nothing visual has changed.
    let mut last_frame: Option<FrameKey> = None;

    loop {
        match select3(
            receiver.changed(),
            app_event_receiver.receive(),
            Timer::after(Duration::from_millis(250)),
        )
        .await
        {
            Either3::First(reading) => {
                last_reading = Some(reading);
            }
            Either3::Second(app_event) => {
                let Some(event) = navigation_event_from_app_event(app_event) else {
                    continue;
                };
                power::notify_ui_activity();
                // Handle UI events based on current state
                state = match state {
                    DisplayState::Main => match event {
                        UiEvent::NextScreen | UiEvent::PrevScreen => {
                            DisplayState::Radio(RadioState::Overview)
                        }
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
                            sync_pending_from_menu(
                                &settings_menu,
                                &mut pending_radio_settings,
                                &mut pending_power_settings,
                            );
                        }

                        match menu_event {
                            MenuEvent::ActionSelected {
                                action: SettingsAction::Save,
                                ..
                            } => {
                                sync_pending_from_menu(
                                    &settings_menu,
                                    &mut pending_radio_settings,
                                    &mut pending_power_settings,
                                );
                                pending_radio_settings =
                                    from_radio_config_view(clamp_radio_config(
                                        to_radio_config_view(pending_radio_settings),
                                    ));
                                pending_power_settings =
                                    from_power_config_view(clamp_power_config(
                                        to_power_config_view(pending_power_settings),
                                    ));
                                settings_menu.commit_all();
                                if let Some(command) = app_bus::route_event(AppEvent::UiInput(
                                    UiInputEvent::ApplySettings {
                                        radio: pending_radio_settings,
                                        power: pending_power_settings,
                                    },
                                )) {
                                    if let AppCommand::ApplySettings { .. } = command {
                                        app_command_sender.send(command).await;
                                    }
                                }
                                let bandwidth_khz = channel_bandwidth_hz(
                                    pending_radio_settings.channel_bandwidth_index,
                                ) / 1000;
                                info!(
                                    "Settings saved: threshold={} dB, magn_target={}, bandwidth={} kHz, carrier_sense={}, predictive_sleep={}, sleep={}s, ui_idle={}s",
                                    pending_radio_settings.detection_threshold_db,
                                    pending_radio_settings.magn_target,
                                    bandwidth_khz,
                                    pending_radio_settings.carrier_sense_threshold,
                                    pending_power_settings.predictive_sleep_enabled,
                                    pending_power_settings.sleep_duration_secs,
                                    pending_power_settings.ui_idle_timeout_secs
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

        if let Some(current_radio_settings) = radio_settings_receiver.try_get()
            && current_radio_settings != pending_radio_settings
            && !matches!(state, DisplayState::Radio(RadioState::Settings))
        {
            pending_radio_settings = current_radio_settings;
            settings_menu = build_settings_menu(pending_radio_settings, pending_power_settings);
        }

        if let Some(current_power_settings) = power_settings_receiver.try_get()
            && current_power_settings != pending_power_settings
            && !matches!(state, DisplayState::Radio(RadioState::Settings))
        {
            pending_power_settings = current_power_settings;
            settings_menu = build_settings_menu(pending_radio_settings, pending_power_settings);
        }

        let current_time = time_receiver.try_get().flatten();
        let frame = derive_frame(DisplayFrameInput {
            screen: to_ui_screen_state(state),
            reading: last_reading.map(to_sensor_reading),
            radio: to_radio_config_view(pending_radio_settings),
            power: to_power_config_view(pending_power_settings),
            time_secs: current_time.map(|time_ref| time_ref.now_unix_secs()),
            settings_nav_index: u8::try_from(settings_menu.selected_index()).map_or(u8::MAX, |v| v),
            settings_editing: settings_menu.is_editing(),
        });

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
                    .map_or(pending_radio_settings.detection_threshold_db, |r| {
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
