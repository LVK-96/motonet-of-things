//! HTTP OTA download via `reqwless`, with SHA-256 verification and
//! ESP image prefix validation.

use core::convert::Infallible;
use core::fmt::Write as _;
use core::mem::MaybeUninit;
use core::net::{IpAddr, Ipv4Addr};
use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicBool, Ordering};

use defmt::{info, warn};
use embassy_net::{
    Stack,
    tcp::client::{TcpClient, TcpClientState},
};
use embedded_nal_async::{AddrType, Dns};
use heapless::String;
use ota_core::{EspImagePrefixError, OtaManifest, validate_esp_image_prefix};
use reqwless::client::HttpClient;
use reqwless::headers::TransferEncoding;
use reqwless::request::Method;
use reqwless::response::StatusCode;
use sha2::{Digest, Sha256};

use embedded_io_async::BufRead as _;

use crate::network;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Combined error type for the OTA download and preparation pipeline.
#[derive(Debug)]
pub enum OtaDownloadError {
    HttpConnect,
    HttpStatus(u16),
    MissingContentLength,
    ContentLengthMismatch,
    ChunkedRejected,
    ReadFailed,
    SizeMismatch,
    ShaMismatch,
    InvalidImagePrefix(EspImagePrefixError),
}

impl From<EspImagePrefixError> for OtaDownloadError {
    fn from(e: EspImagePrefixError) -> Self {
        Self::InvalidImagePrefix(e)
    }
}

// ---------------------------------------------------------------------------
// IPv4-literal DNS adapter
// ---------------------------------------------------------------------------

/// DNS resolver that parses IPv4 address literals directly.
///
/// The OTA URL policy guarantees the download URL contains an IPv4 host,
/// so we never need to perform a real DNS lookup.
pub(crate) struct Ipv4LiteralDns;

impl Dns for Ipv4LiteralDns {
    type Error = Infallible;

    async fn get_host_by_name(
        &self,
        host: &str,
        _addr_type: AddrType,
    ) -> Result<IpAddr, Infallible> {
        // URL policy already validated this is a dotted-decimal IPv4 address.
        let mut octets = [0u8; 4];
        for (i, part) in host.split('.').enumerate() {
            if i >= 4 {
                break;
            }
            // URL policy already validated dotted-decimal IPv4; never fails.
            octets[i] = part.parse().unwrap_or(0);
        }
        Ok(IpAddr::V4(Ipv4Addr::from(octets)))
    }

    async fn get_host_by_address(
        &self,
        _addr: IpAddr,
        _result: &mut [u8],
    ) -> Result<usize, Infallible> {
        Ok(0)
    }
}

// ---------------------------------------------------------------------------
// Shared TCP client pool (one concurrent connection)
// ---------------------------------------------------------------------------

/// Buffer size for each direction of the OTA download TCP socket.
const OTA_TCP_BUF_SIZE: usize = 4096;

static TCP_CLIENT_READY: AtomicBool = AtomicBool::new(false);
static mut TCP_CLIENT_BUF: MaybeUninit<TcpClientState<1, OTA_TCP_BUF_SIZE, OTA_TCP_BUF_SIZE>> =
    MaybeUninit::uninit();

pub(crate) fn tcp_client_state()
-> &'static mut TcpClientState<1, OTA_TCP_BUF_SIZE, OTA_TCP_BUF_SIZE> {
    // addr_of_mut! for static mut produces a raw pointer (safe in edition 2024);
    // we guard initialization with TCP_CLIENT_READY.
    let raw: *mut TcpClientState<1, OTA_TCP_BUF_SIZE, OTA_TCP_BUF_SIZE> =
        addr_of_mut!(TCP_CLIENT_BUF).cast();
    if !TCP_CLIENT_READY.swap(true, Ordering::AcqRel) {
        // SAFETY: first call, exclusive access before the bool is set.
        unsafe {
            raw.write(TcpClientState::new());
        }
    }
    // SAFETY: initialized either in this call or a previous one (guarded by atomic).
    unsafe { &mut *raw }
}

// ---------------------------------------------------------------------------
// SHA-256 hex helpers
// ---------------------------------------------------------------------------

fn sha256_hex(digest: &[u8; 32]) -> String<64> {
    let mut out = String::new();
    for byte in digest {
        // write! into a fixed-capacity heapless::String is infallible.
        write!(out, "{byte:02x}").ok();
    }
    out
}

// ---------------------------------------------------------------------------
// Download
// ---------------------------------------------------------------------------

/// Download the OTA firmware image referenced by `manifest` and return its
/// first 64 bytes (used for ESP image prefix validation).
///
/// # Errors
///
/// Returns [`OtaDownloadError`] on any HTTP, network, or verification failure.
pub async fn download_and_verify(
    network_stack: Stack<'static>,
    manifest: &OtaManifest,
) -> Result<[u8; 64], OtaDownloadError> {
    network::wait_for_config_up(network_stack).await;

    let state = tcp_client_state();
    let tcp = TcpClient::new(network_stack, state);
    let dns = Ipv4LiteralDns;
    let mut client = HttpClient::new(&tcp, &dns);

    let url = manifest.url.as_str();
    let expected_size = manifest.size as usize;
    let expected_sha = manifest.sha256.as_str();

    info!("OTA: downloading {} ({} bytes)", url, expected_size);

    // ── issue GET request ──────────────────────────────────────────────
    let mut request = client
        .request(Method::GET, url)
        .await
        .map_err(|_| OtaDownloadError::HttpConnect)?;

    let mut header_buf = [0u8; 1024];
    let response = request
        .send(&mut header_buf)
        .await
        .map_err(|_| OtaDownloadError::HttpConnect)?;

    // ── validate response metadata ────────────────────────────────────
    if response.status != StatusCode(200) {
        warn!("OTA: HTTP status {}", response.status.0);
        return Err(OtaDownloadError::HttpStatus(response.status.0));
    }

    let content_length = response
        .content_length
        .ok_or(OtaDownloadError::MissingContentLength)?;
    if content_length != expected_size {
        warn!(
            "OTA: Content-Length mismatch (expected {}, got {})",
            expected_size, content_length
        );
        return Err(OtaDownloadError::ContentLengthMismatch);
    }

    if response
        .transfer_encoding
        .contains(&TransferEncoding::Chunked)
    {
        warn!("OTA: chunked transfer encoding rejected");
        return Err(OtaDownloadError::ChunkedRejected);
    }

    // ── stream body ───────────────────────────────────────────────────
    let mut body = response.body().reader();
    let mut first_bytes = [0u8; 64];
    let mut first_filled: usize = 0;
    let mut total: usize = 0;
    let mut hasher = Sha256::new();

    loop {
        let buf = body
            .fill_buf()
            .await
            .map_err(|_| OtaDownloadError::ReadFailed)?;
        if buf.is_empty() {
            break;
        }
        let chunk = buf.len();

        // Capture the first 64 bytes before they disappear.
        let remaining = 64usize.saturating_sub(first_filled);
        let copy = chunk.min(remaining);
        if copy > 0 {
            first_bytes[first_filled..first_filled + copy].copy_from_slice(&buf[..copy]);
            first_filled += copy;
        }

        hasher.update(&buf[..chunk]);
        total += chunk;
        body.consume(chunk);
    }

    // ── post-conditions ───────────────────────────────────────────────
    if total != expected_size {
        warn!(
            "OTA: body size mismatch (expected {}, got {})",
            expected_size, total
        );
        return Err(OtaDownloadError::SizeMismatch);
    }

    let digest = hasher.finalize();
    let digest_hex = sha256_hex(&digest.into());
    if !digest_hex.as_str().eq_ignore_ascii_case(expected_sha) {
        warn!(
            "OTA: SHA-256 mismatch (expected {}, got {})",
            expected_sha, digest_hex
        );
        return Err(OtaDownloadError::ShaMismatch);
    }

    validate_esp_image_prefix(&first_bytes[..first_filled])?;

    info!(
        "OTA: download complete ({} bytes, sha={})",
        total, digest_hex
    );
    Ok(first_bytes)
}
