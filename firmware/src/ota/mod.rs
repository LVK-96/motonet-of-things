use core::mem::MaybeUninit;
use core::ptr::addr_of_mut;

mod boot_metadata;
pub mod encrypted;
pub mod flash_write;
pub(crate) mod hw_rsa;

/// AES peripheral handle, initialised during `hw_setup` before any task runs.
/// SAFETY: initialised exactly once before any task accesses it.
#[allow(improper_ctypes_definitions)]
static mut AES_PERIPHERAL: MaybeUninit<esp_hal::peripherals::AES<'static>> = MaybeUninit::uninit();
/// SHA peripheral handle, initialised during `hw_setup` before any task runs.
/// SAFETY: initialised exactly once before any task accesses it.
#[allow(improper_ctypes_definitions)]
static mut SHA_PERIPHERAL: MaybeUninit<esp_hal::peripherals::SHA<'static>> = MaybeUninit::uninit();
/// RSA peripheral handle, initialised during `hw_setup` before any task runs.
/// SAFETY: initialised exactly once before any task accesses it.
#[allow(improper_ctypes_definitions)]
static mut RSA_PERIPHERAL: MaybeUninit<esp_hal::peripherals::RSA<'static>> = MaybeUninit::uninit();

/// # Safety
/// Call exactly once during `hw_setup`, before any task accesses the peripheral.
pub(crate) unsafe fn init_crypto_peripherals(
    aes: esp_hal::peripherals::AES<'static>,
    sha: esp_hal::peripherals::SHA<'static>,
    rsa: esp_hal::peripherals::RSA<'static>,
) {
    unsafe {
        addr_of_mut!(AES_PERIPHERAL).write(MaybeUninit::new(aes));
        addr_of_mut!(SHA_PERIPHERAL).write(MaybeUninit::new(sha));
        addr_of_mut!(RSA_PERIPHERAL).write(MaybeUninit::new(rsa));
    }
}

/// # Safety
/// Must only be called after `init_crypto_peripherals` has been called.
pub(crate) unsafe fn take_aes() -> esp_hal::peripherals::AES<'static> {
    unsafe {
        (*addr_of_mut!(AES_PERIPHERAL))
            .assume_init_ref()
            .clone_unchecked()
    }
}

/// # Safety
/// Must only be called after `init_crypto_peripherals` has been called.
pub(crate) unsafe fn take_sha() -> esp_hal::peripherals::SHA<'static> {
    unsafe {
        (*addr_of_mut!(SHA_PERIPHERAL))
            .assume_init_ref()
            .clone_unchecked()
    }
}

/// # Safety
/// Must only be called after `init_crypto_peripherals` has been called.
pub(crate) unsafe fn take_rsa() -> esp_hal::peripherals::RSA<'static> {
    unsafe {
        (*addr_of_mut!(RSA_PERIPHERAL))
            .assume_init_ref()
            .clone_unchecked()
    }
}

/// Get a reference to the SHA peripheral without consuming it.
///
/// # Safety
/// Must only be called after `init_crypto_peripherals`.
pub(crate) unsafe fn sha_ref() -> &'static esp_hal::peripherals::SHA<'static> {
    unsafe { (*addr_of_mut!(SHA_PERIPHERAL)).assume_init_ref() }
}

pub use boot_metadata::OtaBootMetadata;
pub use flash_write::OtaFlashWriteError;
pub use ota_core::{
    OtaState, OtaUpdateGuard, PendingConfirmationGuard, arm_rollback_test_pending_confirmation,
    ota_confirmation_pending, ota_sleep_blocked, ota_state, ota_update_in_progress, set_ota_state,
};
