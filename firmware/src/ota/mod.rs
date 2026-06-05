mod boot_metadata;
mod ota_state;

pub use boot_metadata::OtaBootMetadata;
#[cfg(feature = "ota-rollback-test")]
pub use ota_state::arm_rollback_test_pending_confirmation;
pub use ota_state::{
    OtaState, OtaUpdateGuard, PendingConfirmationGuard, ota_confirmation_pending,
    ota_sleep_blocked, ota_state, ota_update_in_progress, set_ota_state,
};
