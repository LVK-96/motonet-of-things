//! Shared TLS record buffers for network clients that never run concurrently.
//!
//! MQTT stands down before OTA starts its HTTP download, so both paths can use
//! one pair of reclaimed-memory TLS buffers instead of carrying separate 20 KiB
//! buffer sets in their async task futures/statics.

use core::mem::MaybeUninit;
use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicBool, Ordering};

/// Buffer size for HTTPS/TLS record-layer reads.
///
/// GitHub release asset redirects commonly send full-size TLS records;
/// embedded-tls warns below 16,640 bytes and handshakes can fail before the
/// HTTP request is sent.
pub const TLS_READ_BUF_SIZE: usize = 16_640;
/// Buffer size for compact client writes such as MQTT packets and HTTP GETs.
pub const TLS_WRITE_BUF_SIZE: usize = 4_096;

static TLS_WORKSPACE_IN_USE: AtomicBool = AtomicBool::new(false);

#[esp_hal::ram(reclaimed)]
static mut TLS_READ_BUF: MaybeUninit<[u8; TLS_READ_BUF_SIZE]> = MaybeUninit::uninit();
#[esp_hal::ram(reclaimed)]
static mut TLS_WRITE_BUF: MaybeUninit<[u8; TLS_WRITE_BUF_SIZE]> = MaybeUninit::uninit();

/// Exclusive borrow of the shared TLS record buffers.
pub struct TlsWorkspaceGuard;

impl TlsWorkspaceGuard {
    /// Acquire the shared TLS workspace if it is currently free.
    pub fn try_acquire() -> Option<Self> {
        TLS_WORKSPACE_IN_USE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .ok()
    }

    /// Borrow both TLS buffers for the lifetime of this guard borrow.
    pub fn buffers(&mut self) -> (&mut [u8; TLS_READ_BUF_SIZE], &mut [u8; TLS_WRITE_BUF_SIZE]) {
        // SAFETY: the atomic guard guarantees only one TlsWorkspaceGuard exists
        // at a time, and the returned references are tied to &mut self.
        unsafe {
            (
                (*addr_of_mut!(TLS_READ_BUF)).assume_init_mut(),
                (*addr_of_mut!(TLS_WRITE_BUF)).assume_init_mut(),
            )
        }
    }
}

impl Drop for TlsWorkspaceGuard {
    fn drop(&mut self) {
        TLS_WORKSPACE_IN_USE.store(false, Ordering::Release);
    }
}
