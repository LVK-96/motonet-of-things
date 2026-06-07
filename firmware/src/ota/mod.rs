mod boot_metadata;
pub mod download;
pub mod flash_write;

pub use boot_metadata::OtaBootMetadata;
pub use download::OtaDownloadError;
pub use flash_write::OtaFlashWriteError;
pub use ota_core::{
    OtaState, OtaUpdateGuard, PendingConfirmationGuard, arm_rollback_test_pending_confirmation,
    ota_confirmation_pending, ota_sleep_blocked, ota_state, ota_update_in_progress, set_ota_state,
};
