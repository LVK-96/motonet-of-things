use crate::ui_input::UiEvent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DisplayState {
    Main,
    Radio(RadioState),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RadioState {
    Overview,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsMenuOutcome {
    Unchanged,
    ValueChanged,
    SaveSelected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TransitionEffects {
    pub reset_settings_menu: bool,
    pub sync_pending_settings: bool,
    pub save_settings: bool,
}

impl TransitionEffects {
    pub const NONE: Self = Self {
        reset_settings_menu: false,
        sync_pending_settings: false,
        save_settings: false,
    };

    const RESET_SETTINGS_MENU: Self = Self {
        reset_settings_menu: true,
        sync_pending_settings: false,
        save_settings: false,
    };

    const SYNC_PENDING: Self = Self {
        reset_settings_menu: false,
        sync_pending_settings: true,
        save_settings: false,
    };

    const SAVE_SETTINGS: Self = Self {
        reset_settings_menu: false,
        sync_pending_settings: true,
        save_settings: true,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Transition {
    pub state: DisplayState,
    pub effects: TransitionEffects,
}

impl Transition {
    const fn new(state: DisplayState, effects: TransitionEffects) -> Self {
        Self { state, effects }
    }
}

#[must_use]
pub(crate) fn reduce_navigation(state: DisplayState, event: UiEvent) -> Transition {
    match state {
        DisplayState::Main => match event {
            UiEvent::NextScreen | UiEvent::PrevScreen => Transition::new(
                DisplayState::Radio(RadioState::Overview),
                TransitionEffects::NONE,
            ),
            UiEvent::Select => Transition::new(
                DisplayState::Radio(RadioState::Settings),
                TransitionEffects::RESET_SETTINGS_MENU,
            ),
        },
        DisplayState::Radio(RadioState::Overview) => match event {
            UiEvent::NextScreen | UiEvent::PrevScreen => {
                Transition::new(DisplayState::Main, TransitionEffects::NONE)
            }
            UiEvent::Select => Transition::new(
                DisplayState::Radio(RadioState::Settings),
                TransitionEffects::RESET_SETTINGS_MENU,
            ),
        },
        DisplayState::Radio(RadioState::Settings) => Transition::new(
            DisplayState::Radio(RadioState::Settings),
            TransitionEffects::NONE,
        ),
    }
}

#[must_use]
pub(crate) fn reduce_settings_menu(
    state: DisplayState,
    outcome: SettingsMenuOutcome,
) -> Transition {
    if !matches!(state, DisplayState::Radio(RadioState::Settings)) {
        return Transition::new(state, TransitionEffects::NONE);
    }

    match outcome {
        SettingsMenuOutcome::Unchanged => Transition::new(state, TransitionEffects::NONE),
        SettingsMenuOutcome::ValueChanged => {
            Transition::new(state, TransitionEffects::SYNC_PENDING)
        }
        SettingsMenuOutcome::SaveSelected => Transition::new(
            DisplayState::Radio(RadioState::Overview),
            TransitionEffects::SAVE_SETTINGS,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DisplayState, RadioState, SettingsMenuOutcome, TransitionEffects, reduce_navigation,
        reduce_settings_menu,
    };
    use crate::ui_input::UiEvent;

    #[test]
    fn screen_transition_reducer_toggles_main_and_radio_overview() {
        let to_radio = reduce_navigation(DisplayState::Main, UiEvent::NextScreen);
        assert_eq!(to_radio.state, DisplayState::Radio(RadioState::Overview));
        assert_eq!(to_radio.effects, TransitionEffects::NONE);

        let to_main = reduce_navigation(to_radio.state, UiEvent::PrevScreen);
        assert_eq!(to_main.state, DisplayState::Main);
        assert_eq!(to_main.effects, TransitionEffects::NONE);
    }

    #[test]
    fn screen_transition_reducer_select_enters_settings_and_resets_menu() {
        let transition = reduce_navigation(DisplayState::Main, UiEvent::Select);
        assert_eq!(transition.state, DisplayState::Radio(RadioState::Settings));
        assert!(transition.effects.reset_settings_menu);
        assert!(!transition.effects.sync_pending_settings);
        assert!(!transition.effects.save_settings);
    }

    #[test]
    fn settings_menu_save_flow_transitions_and_requests_save() {
        let transition = reduce_settings_menu(
            DisplayState::Radio(RadioState::Settings),
            SettingsMenuOutcome::SaveSelected,
        );

        assert_eq!(transition.state, DisplayState::Radio(RadioState::Overview));
        assert!(!transition.effects.reset_settings_menu);
        assert!(transition.effects.sync_pending_settings);
        assert!(transition.effects.save_settings);
    }

    #[test]
    fn settings_menu_value_change_keeps_screen_and_syncs_pending() {
        let transition = reduce_settings_menu(
            DisplayState::Radio(RadioState::Settings),
            SettingsMenuOutcome::ValueChanged,
        );

        assert_eq!(transition.state, DisplayState::Radio(RadioState::Settings));
        assert!(!transition.effects.reset_settings_menu);
        assert!(transition.effects.sync_pending_settings);
        assert!(!transition.effects.save_settings);
    }
}
