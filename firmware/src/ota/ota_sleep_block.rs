use core::sync::atomic::{AtomicBool, Ordering};

static OTA_UPDATE_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

pub fn ota_update_in_progress() -> bool {
    OTA_UPDATE_IN_PROGRESS.load(Ordering::Relaxed)
}

#[must_use]
pub struct OtaUpdateGuard;

impl OtaUpdateGuard {
    pub fn begin() -> Self {
        OTA_UPDATE_IN_PROGRESS.store(true, Ordering::Relaxed);
        Self
    }
}

impl Drop for OtaUpdateGuard {
    fn drop(&mut self) {
        OTA_UPDATE_IN_PROGRESS.store(false, Ordering::Relaxed);
    }
}
