// NTP time synchronization module.
//
// Provides functionality to sync time from NTP servers and maintain
// a monotonic clock reference for timestamping events.

use embassy_net::Stack;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use embassy_time::{Duration, Instant, Timer};
use eu_dst::Timezone;
use sntpc::{NtpContext, NtpTimestampGenerator};
use sntpc_net_embassy::UdpSocketWrapper;

const NTP_PORT: u16 = 123;

/// How often to resync time (1 hour)
const RESYNC_INTERVAL_SECS: u64 = 3600;

/// Timezone for Finland (EET/EEST)
const TIMEZONE: Timezone = Timezone::EET;

/// Time reference combining NTP seconds with local monotonic time
#[derive(Clone, Copy, Debug, defmt::Format)]
pub struct TimeReference {
    pub unix_secs: u64,
    // Local instant when this was captured
    pub captured_at: Instant,
}

impl TimeReference {
    /// Get the current Unix timestamp based on the stored reference
    #[must_use]
    pub fn now_unix_secs(&self) -> u64 {
        let elapsed = self.captured_at.elapsed().as_secs();
        self.unix_secs + elapsed
    }

    /// Format as HH:MM:SS string (local time with automatic DST)
    pub fn format_time(&self, buf: &mut heapless::String<16>) {
        use core::fmt::Write;
        let secs = self.now_unix_secs();
        let (hours, minutes, seconds) = TIMEZONE.to_local_hms(secs);
        let _ = write!(buf, "{hours:02}:{minutes:02}:{seconds:02}");
    }
}

/// Global watch for sharing time reference with other tasks
pub static TIME_WATCH: Watch<CriticalSectionRawMutex, Option<TimeReference>, 2> = Watch::new();

/// Timestamp generator for sntpc using embassy-time
#[derive(Clone, Copy)]
struct EmbassyTimestampGen {
    start_secs: u64,
    start_micros: u32,
}

impl EmbassyTimestampGen {
    fn new() -> Self {
        let now = Instant::now();
        #[allow(clippy::cast_possible_truncation)]
        let start_micros = (now.as_micros() % 1_000_000) as u32;
        Self {
            start_secs: now.as_secs(),
            start_micros,
        }
    }
}

impl NtpTimestampGenerator for EmbassyTimestampGen {
    fn init(&mut self) {
        let now = Instant::now();
        self.start_secs = now.as_secs();
        #[allow(clippy::cast_possible_truncation)]
        let micros = (now.as_micros() % 1_000_000) as u32;
        self.start_micros = micros;
    }

    fn timestamp_sec(&self) -> u64 {
        Instant::now().as_secs() - self.start_secs
    }

    fn timestamp_subsec_micros(&self) -> u32 {
        #[allow(clippy::cast_possible_truncation)]
        let now_micros = (Instant::now().as_micros() % 1_000_000) as u32;
        now_micros.wrapping_sub(self.start_micros)
    }
}

/// Perform a single NTP time sync
async fn sync_time_once(stack: Stack<'static>) -> Option<TimeReference> {
    // Wait for network to be ready
    stack.wait_config_up().await;

    // Create UDP socket for NTP
    let mut rx_meta = [PacketMetadata::EMPTY; 1];
    let mut rx_buffer = [0u8; 256];
    let mut tx_meta = [PacketMetadata::EMPTY; 1];
    let mut tx_buffer = [0u8; 256];

    let mut socket = UdpSocket::new(
        stack,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );

    // Bind to any available port
    if socket.bind(0).is_err() {
        defmt::warn!("NTP: Failed to bind UDP socket");
        return None;
    }

    let socket_wrapper = UdpSocketWrapper::new(socket);
    let server_addr = core::net::SocketAddr::new(
        core::net::IpAddr::V4(core::net::Ipv4Addr::new(216, 239, 35, 0)),
        NTP_PORT,
    );

    let context = NtpContext::new(EmbassyTimestampGen::new());
    let captured_at = Instant::now();

    match sntpc::get_time(server_addr, &socket_wrapper, context).await {
        Ok(result) => {
            // sntpc returns Unix timestamp directly (seconds since 1970-01-01)
            let unix_secs = u64::from(result.seconds);

            defmt::info!("NTP: Synced! unix_secs={}", unix_secs);

            Some(TimeReference {
                unix_secs,
                captured_at,
            })
        }
        Err(e) => {
            defmt::warn!("NTP: Sync failed: {:?}", defmt::Debug2Format(&e));
            None
        }
    }
}

/// NTP time sync task - syncs time periodically
pub async fn time_sync_loop(stack: Stack<'static>) {
    defmt::info!("NTP: Time sync task started");

    let sender = TIME_WATCH.sender();

    // Initial sync with retries
    loop {
        if let Some(time_ref) = sync_time_once(stack).await {
            sender.send(Some(time_ref));
            break;
        }
        defmt::info!("NTP: Retrying in 10s...");
        Timer::after(Duration::from_secs(10)).await;
    }

    // Periodic resync
    loop {
        Timer::after(Duration::from_secs(RESYNC_INTERVAL_SECS)).await;

        if let Some(time_ref) = sync_time_once(stack).await {
            sender.send(Some(time_ref));
        }
    }
}
