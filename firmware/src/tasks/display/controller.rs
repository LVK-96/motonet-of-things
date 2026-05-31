use core::fmt::Write;

use app_core::config_rules::{clamp_power_config, clamp_radio_config};
use defmt::{error, info};
use embassy_futures::select::{Either3, select3};
use embassy_time::{Duration, Timer};
use heapless::String;
use karu_menu::{
    ActionItem, Menu, MenuEntry, MenuEvent, NumericItem, ScrollableViewport, UiEvent as MenuUiEvent,
};

use crate::app_bus::{AppCommand, AppEvent, AppEventReceiver, ReadingReceiver, UiInputEvent};
use crate::messages::{
    CARRIER_SENSE_MAX, CARRIER_SENSE_MIN, CHANNEL_BANDWIDTH_MAX_INDEX, CHANNEL_BANDWIDTH_MIN_INDEX,
    DEFAULT_POWER_SETTINGS, DEFAULT_RADIO_SETTINGS, DETECTION_THRESHOLD_MAX_DB,
    DETECTION_THRESHOLD_MIN_DB, DETECTION_THRESHOLD_STEP_DB, MAGN_TARGET_MAX, MAGN_TARGET_MIN,
    POWER_SLEEP_DURATION_MAX_SECS, POWER_SLEEP_DURATION_MIN_SECS, POWER_UI_IDLE_TIMEOUT_MAX_SECS,
    POWER_UI_IDLE_TIMEOUT_MIN_SECS, PowerSettings, RadioReading, RadioSettings,
    channel_bandwidth_hz,
};
use crate::power;
use crate::tasks::display::frame_map;
use crate::tasks::display::frame_model::FrameKey;
use crate::ui_input::UiEvent;

use super::state::{
    DisplayState, SettingsMenuOutcome, Transition, reduce_navigation, reduce_settings_menu,
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum DisplayLoopEvent {
    Reading(RadioReading),
    Navigation(UiEvent),
    Tick,
    Ignore,
}

pub(crate) async fn next_display_loop_event(
    receiver: &mut ReadingReceiver,
    app_event_receiver: &AppEventReceiver,
) -> DisplayLoopEvent {
    match select3(
        receiver.changed(),
        app_event_receiver.receive(),
        Timer::after(Duration::from_millis(250)),
    )
    .await
    {
        Either3::First(reading) => DisplayLoopEvent::Reading(reading),
        Either3::Second(app_event) => navigation_event_from_app_event(app_event)
            .map_or(DisplayLoopEvent::Ignore, DisplayLoopEvent::Navigation),
        Either3::Third(()) => DisplayLoopEvent::Tick,
    }
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
pub(crate) enum SettingsAction {
    Save,
}

pub(crate) type SettingsMenu = Menu<MenuEntry<u8, SettingsAction>, SETTINGS_MENU_CAPACITY>;

pub(crate) struct DisplayController {
    state: DisplayState,
    last_reading: Option<RadioReading>,
    pending_radio_settings: RadioSettings,
    pending_power_settings: PowerSettings,
    settings_menu: SettingsMenu,
    last_frame: Option<FrameKey>,
}

impl DisplayController {
    #[must_use]
    pub(crate) fn new(
        initial_radio_settings: Option<RadioSettings>,
        initial_power_settings: Option<PowerSettings>,
    ) -> Self {
        let pending_radio_settings = initial_radio_settings.unwrap_or(DEFAULT_RADIO_SETTINGS);
        let pending_power_settings = initial_power_settings.unwrap_or(DEFAULT_POWER_SETTINGS);

        let settings_menu = build_settings_menu(pending_radio_settings, pending_power_settings);
        let mut current_radio = pending_radio_settings;
        let mut current_power = pending_power_settings;
        sync_pending_from_menu(&settings_menu, &mut current_radio, &mut current_power);

        Self {
            state: DisplayState::Main,
            last_reading: None,
            pending_radio_settings: current_radio,
            pending_power_settings: current_power,
            settings_menu,
            last_frame: None,
        }
    }

    #[must_use]
    pub(crate) fn state(&self) -> DisplayState {
        self.state
    }

    #[must_use]
    pub(crate) fn last_reading(&self) -> Option<RadioReading> {
        self.last_reading
    }

    #[must_use]
    pub(crate) fn settings_menu(&self) -> &SettingsMenu {
        &self.settings_menu
    }

    #[must_use]
    pub(crate) fn radio_overview_values(&self) -> (Option<i16>, u8) {
        let rssi = self.last_reading.map(|reading| reading.rssi);
        let detection_threshold = self.last_reading.map_or(
            self.pending_radio_settings.detection_threshold_db,
            |reading| reading.detection_threshold,
        );

        (rssi, detection_threshold)
    }

    pub(crate) fn handle_event(&mut self, event: DisplayLoopEvent) -> Option<AppCommand> {
        match event {
            DisplayLoopEvent::Reading(reading) => {
                self.last_reading = Some(reading);
                None
            }
            DisplayLoopEvent::Navigation(event) => {
                power::notify_ui_activity();

                let transition = if matches!(
                    self.state,
                    DisplayState::Radio(super::state::RadioState::Settings)
                ) {
                    let menu_event = self
                        .settings_menu
                        .handle_event(menu_event_from_ui_event(event));
                    reduce_settings_menu(self.state, settings_outcome_from_menu_event(menu_event))
                } else {
                    reduce_navigation(self.state, event)
                };

                self.apply_transition(transition)
            }
            DisplayLoopEvent::Tick | DisplayLoopEvent::Ignore => None,
        }
    }

    pub(crate) fn sync_from_watchers(
        &mut self,
        radio_settings: Option<RadioSettings>,
        power_settings: Option<PowerSettings>,
    ) {
        if matches!(
            self.state,
            DisplayState::Radio(super::state::RadioState::Settings)
        ) {
            return;
        }

        let mut should_rebuild_menu = false;

        if let Some(settings) = radio_settings
            && settings != self.pending_radio_settings
        {
            self.pending_radio_settings = settings;
            should_rebuild_menu = true;
        }

        if let Some(settings) = power_settings
            && settings != self.pending_power_settings
        {
            self.pending_power_settings = settings;
            should_rebuild_menu = true;
        }

        if should_rebuild_menu {
            self.settings_menu =
                build_settings_menu(self.pending_radio_settings, self.pending_power_settings);
        }
    }

    #[must_use]
    pub(crate) fn derive_frame_key(&self, time_secs: Option<u64>) -> FrameKey {
        frame_map::derive_frame_key(
            self.state,
            self.last_reading,
            self.pending_radio_settings,
            self.pending_power_settings,
            time_secs,
            self.settings_menu.selected_index(),
            self.settings_menu.is_editing(),
        )
    }

    #[must_use]
    pub(crate) fn should_render(&self, frame: FrameKey) -> bool {
        Some(frame) != self.last_frame
    }

    pub(crate) fn mark_rendered(&mut self, frame: FrameKey) {
        self.last_frame = Some(frame);
    }

    fn apply_transition(&mut self, transition: Transition) -> Option<AppCommand> {
        if transition.effects.reset_settings_menu {
            reset_settings_menu_for_entry(&mut self.settings_menu);
        }

        if transition.effects.sync_pending_settings {
            sync_pending_from_menu(
                &self.settings_menu,
                &mut self.pending_radio_settings,
                &mut self.pending_power_settings,
            );
        }

        self.state = transition.state;

        if transition.effects.save_settings {
            self.pending_radio_settings =
                RadioSettings::from(clamp_radio_config(self.pending_radio_settings.into()));
            self.pending_power_settings =
                PowerSettings::from(clamp_power_config(self.pending_power_settings.into()));

            self.settings_menu.commit_all();

            let bandwidth_khz =
                channel_bandwidth_hz(self.pending_radio_settings.channel_bandwidth_index) / 1000;
            info!(
                "Settings saved: threshold={} dB, magn_target={}, bandwidth={} kHz, carrier_sense={}, predictive_sleep={}, sleep={}s, ui_idle={}s",
                self.pending_radio_settings.detection_threshold_db,
                self.pending_radio_settings.magn_target,
                bandwidth_khz,
                self.pending_radio_settings.carrier_sense_threshold,
                self.pending_power_settings.predictive_sleep_enabled,
                self.pending_power_settings.sleep_duration_secs,
                self.pending_power_settings.ui_idle_timeout_secs
            );

            Some(AppCommand::ApplySettings {
                radio: self.pending_radio_settings,
                power: self.pending_power_settings,
            })
        } else {
            None
        }
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

fn settings_outcome_from_menu_event(event: MenuEvent<SettingsAction>) -> SettingsMenuOutcome {
    match event {
        MenuEvent::ValueChanged(_) => SettingsMenuOutcome::ValueChanged,
        MenuEvent::ActionSelected {
            action: SettingsAction::Save,
            ..
        } => SettingsMenuOutcome::SaveSelected,
        _ => SettingsMenuOutcome::Unchanged,
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
        numeric_value_at(menu, PREDICTIVE_SLEEP_ITEM_INDEX).is_some_and(|value| value != 0);
    pending_power.sleep_duration_secs = numeric_value_at(menu, SLEEP_DURATION_ITEM_INDEX)
        .unwrap_or(pending_power.sleep_duration_secs);
    pending_power.ui_idle_timeout_secs = numeric_value_at(menu, UI_IDLE_TIMEOUT_ITEM_INDEX)
        .unwrap_or(pending_power.ui_idle_timeout_secs);
}

#[cfg(test)]
mod tests {
    use super::{DisplayController, DisplayLoopEvent};
    use crate::app_bus::AppCommand;
    use crate::messages::{DEFAULT_POWER_SETTINGS, DEFAULT_RADIO_SETTINGS};
    use crate::tasks::display::state::{DisplayState, RadioState};
    use crate::ui_input::UiEvent;

    #[test]
    fn settings_menu_save_flow_emits_apply_settings_command() {
        let mut controller =
            DisplayController::new(Some(DEFAULT_RADIO_SETTINGS), Some(DEFAULT_POWER_SETTINGS));

        let _ = controller.handle_event(DisplayLoopEvent::Navigation(UiEvent::Select));

        for _ in 0..7 {
            let _ = controller.handle_event(DisplayLoopEvent::Navigation(UiEvent::NextScreen));
        }

        let command = controller.handle_event(DisplayLoopEvent::Navigation(UiEvent::Select));

        assert!(matches!(
            command,
            Some(AppCommand::ApplySettings {
                radio: DEFAULT_RADIO_SETTINGS,
                power: DEFAULT_POWER_SETTINGS,
            })
        ));
        assert_eq!(
            controller.state(),
            DisplayState::Radio(RadioState::Overview)
        );
    }
}
