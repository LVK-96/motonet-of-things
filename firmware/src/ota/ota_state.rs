use core::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum OtaState {
    Inactive = 0,
    Downloading = 1,
    Applying = 2,
    PendingConfirmation = 3,
}

impl OtaState {
    const fn as_u8(self) -> u8 {
        self as u8
    }

    const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Downloading,
            2 => Self::Applying,
            3 => Self::PendingConfirmation,
            _ => Self::Inactive,
        }
    }
}

static OTA_STATE: AtomicU8 = AtomicU8::new(OtaState::Inactive.as_u8());

pub fn ota_state() -> OtaState {
    OtaState::from_u8(OTA_STATE.load(Ordering::Relaxed))
}

pub fn set_ota_state(state: OtaState) {
    OTA_STATE.store(state.as_u8(), Ordering::Relaxed);
}

#[must_use]
pub fn ota_confirmation_pending() -> bool {
    ota_state() == OtaState::PendingConfirmation
}

#[must_use]
pub fn ota_sleep_blocked() -> bool {
    ota_state() != OtaState::Inactive
}

#[must_use]
pub fn ota_update_in_progress() -> bool {
    matches!(ota_state(), OtaState::Downloading | OtaState::Applying)
}

#[cfg(feature = "ota-rollback-test")]
pub fn arm_rollback_test_pending_confirmation() {
    set_ota_state(OtaState::PendingConfirmation);
}

#[must_use]
pub struct OtaUpdateGuard;

impl OtaUpdateGuard {
    pub fn begin(state: OtaState) -> Self {
        set_ota_state(state);
        Self
    }

    pub fn begin_download() -> Self {
        Self::begin(OtaState::Downloading)
    }

    pub fn begin_apply() -> Self {
        Self::begin(OtaState::Applying)
    }
}

#[must_use]
pub struct PendingConfirmationGuard;

impl PendingConfirmationGuard {
    pub fn begin() -> Self {
        set_ota_state(OtaState::PendingConfirmation);
        Self
    }

    pub fn confirm(self) {
        set_ota_state(OtaState::Inactive);
        core::mem::forget(self);
    }
}

impl Drop for PendingConfirmationGuard {
    fn drop(&mut self) {
        set_ota_state(OtaState::PendingConfirmation);
    }
}

impl Drop for OtaUpdateGuard {
    fn drop(&mut self) {
        set_ota_state(OtaState::Inactive);
    }
}
