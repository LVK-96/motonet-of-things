//! Flash-aware OTA download that streams firmware directly into the inactive
//! partition, verifies integrity, activates the new slot, and reboots.
//!
//! The pipeline is split into four phases so the top-level
//! [`download_and_write_to_flash`] reads as a sequence of named steps:
//!
//! 1. [`prepare_inactive_slot`] — size check + sector-aligned erase.
//! 2. [`fetch_and_stream_to_flash`] — HTTP fetch, response validation,
//!    and on-the-fly hash/prefix/flash write via [`StreamToFlash`].
//! 3. [`post_verify`] + [`verify_flash_readback`] — manifest and
//!    readback checks against the streamed result.
//! 4. Activate the new slot, mark the app `New`, and reboot.

use core::convert::Infallible;
use core::fmt::Write as _;
use core::mem::MaybeUninit;
use core::net::{IpAddr, Ipv4Addr};
use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicBool, Ordering};

use defmt::{Debug2Format, info, warn};
use embassy_net::{
    Stack,
    tcp::client::{TcpClient, TcpClientState},
};
use embedded_io_async::BufRead as _;
use embedded_nal_async::{AddrType, Dns};
use embedded_storage::ReadStorage;
use embedded_storage::Storage;
use embedded_storage::nor_flash::NorFlash;
use esp_bootloader_esp_idf::partitions::{
    Error as PartitionError, FlashRegion, PARTITION_TABLE_MAX_LEN,
};
use esp_hal::system::software_reset;
use esp_storage::{FlashStorage, FlashStorageError};
use heapless::String;
use ota_core::{EspImagePrefixError, OtaManifest, validate_esp_image_prefix};
use reqwless::client::HttpClient;
use reqwless::headers::TransferEncoding;
use reqwless::request::Method;
use reqwless::response::StatusCode;
use sha2::{Digest, Sha256};

use crate::network;
use crate::ota::OtaBootMetadata;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Buffer size for each direction of the OTA download TCP socket.
const OTA_TCP_BUF_SIZE: usize = 4096;
/// ESP32 flash sector size used for erase alignment.
const FLASH_SECTOR_SIZE: u32 = 4096;
/// Maximum HTTP header bytes we'll buffer for a response.
const HTTP_HEADER_BUF_SIZE: usize = 1024;
/// Number of leading bytes of the image retained for ESP prefix
/// validation. The basic ESP image header lives in the first 8 bytes.
const ESP_IMAGE_PREFIX_LEN: usize = 64;
/// Number of bytes read back from flash for post-write verification.
const FLASH_READBACK_LEN: usize = 256;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Combined error for the flash-write OTA pipeline.
#[derive(Debug)]
pub enum OtaFlashWriteError {
    Partition(PartitionError),
    Flash(FlashStorageError),
    HttpConnect,
    HttpStatus(u16),
    MissingContentLength,
    ContentLengthMismatch,
    ChunkedRejected,
    ReadFailed,
    SizeMismatch,
    ShaMismatch,
    InvalidImagePrefix(EspImagePrefixError),
    FlashVerifyFailed,
    /// Manifest `size` does not fit in the inactive OTA slot even after
    /// sector alignment.
    ImageTooLargeForSlot {
        size: u32,
        slot_size: u32,
    },
}

impl From<PartitionError> for OtaFlashWriteError {
    fn from(e: PartitionError) -> Self {
        Self::Partition(e)
    }
}

impl From<FlashStorageError> for OtaFlashWriteError {
    fn from(e: FlashStorageError) -> Self {
        Self::Flash(e)
    }
}

impl From<EspImagePrefixError> for OtaFlashWriteError {
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
/// so we never need to perform a real DNS lookup. Using a dedicated adapter
/// keeps the boundary explicit so a real DNS resolver can be slotted in
/// later without changing the download pipeline.
struct Ipv4LiteralDns;

impl Dns for Ipv4LiteralDns {
    type Error = Infallible;

    async fn get_host_by_name(
        &self,
        host: &str,
        _addr_type: AddrType,
    ) -> Result<IpAddr, Infallible> {
        // The URL policy in ota-core already validated this is a
        // dotted-decimal IPv4 address; defensively fall back to 0.0.0.0 if
        // the contract is ever violated.
        let mut octets = [0u8; 4];
        for (i, part) in host.split('.').enumerate() {
            if i >= 4 {
                break;
            }
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

static TCP_CLIENT_READY: AtomicBool = AtomicBool::new(false);
static mut TCP_CLIENT_BUF: MaybeUninit<TcpClientState<1, OTA_TCP_BUF_SIZE, OTA_TCP_BUF_SIZE>> =
    MaybeUninit::uninit();

fn tcp_client_state() -> &'static mut TcpClientState<1, OTA_TCP_BUF_SIZE, OTA_TCP_BUF_SIZE> {
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
// SHA-256 hex helper
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
// Streaming state
// ---------------------------------------------------------------------------

/// Streaming state for writing the HTTP response body to the inactive
/// flash partition. Captures the first 64 bytes for ESP prefix
/// validation, hashes the body, and writes chunks to flash.
struct StreamToFlash<'a, F: Storage> {
    region: FlashRegion<'a, F>,
    first_bytes: [u8; ESP_IMAGE_PREFIX_LEN],
    first_filled: usize,
    prefix_validated: bool,
    total: usize,
    flash_offset: u32,
    hasher: Sha256,
}

/// Output of [`StreamToFlash`] — everything the post-download verifiers
/// need without holding on to the flash region borrow.
struct StreamResult {
    total: usize,
    digest: [u8; 32],
    first_bytes: [u8; ESP_IMAGE_PREFIX_LEN],
    first_filled: usize,
}

impl<'a, F: Storage> StreamToFlash<'a, F> {
    fn new(region: FlashRegion<'a, F>) -> Self {
        Self {
            region,
            first_bytes: [0u8; ESP_IMAGE_PREFIX_LEN],
            first_filled: 0,
            prefix_validated: false,
            total: 0,
            flash_offset: 0,
            hasher: Sha256::new(),
        }
    }

    /// Ingest a body chunk: capture prefix bytes, validate the ESP image
    /// prefix as soon as enough bytes are available, hash, and write to
    /// flash. The prefix check happens **before** the first `Storage::write`
    /// so a corrupt or hostile image never touches the inactive slot.
    #[allow(clippy::cast_possible_truncation)]
    fn process_chunk(&mut self, chunk: &[u8]) -> Result<(), OtaFlashWriteError> {
        // Capture the first 64 bytes for ESP prefix validation.
        let remaining = ESP_IMAGE_PREFIX_LEN.saturating_sub(self.first_filled);
        let copy = chunk.len().min(remaining);
        if copy > 0 {
            self.first_bytes[self.first_filled..self.first_filled + copy]
                .copy_from_slice(&chunk[..copy]);
            self.first_filled += copy;
        }
        if !self.prefix_validated && self.first_filled >= 8 {
            validate_esp_image_prefix(&self.first_bytes[..self.first_filled])?;
            self.prefix_validated = true;
        }

        // Hash and write to flash.
        self.hasher.update(chunk);
        Storage::write(&mut self.region, self.flash_offset, chunk)
            .map_err(OtaFlashWriteError::from)?;
        self.flash_offset += chunk.len() as u32;
        self.total += chunk.len();
        Ok(())
    }

    fn finalize(self) -> StreamResult {
        let digest: [u8; 32] = self.hasher.finalize().into();
        StreamResult {
            total: self.total,
            digest,
            first_bytes: self.first_bytes,
            first_filled: self.first_filled,
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 1: prepare the inactive slot
// ---------------------------------------------------------------------------

/// Verify the manifest's `size` fits in the inactive partition (with one
/// sector of tail padding) and erase the sectors the image will occupy.
///
/// `partition_size()` is the partition size in bytes as reported by
/// `esp_bootloader_esp_idf`; the `as u32` cast is sound for the ESP32-WROOM
/// 4 MiB flash layout this firmware targets.
#[allow(clippy::cast_possible_truncation)]
fn prepare_inactive_slot<F: NorFlash>(
    region: &mut FlashRegion<'_, F>,
    image_bytes: u32,
) -> Result<(), OtaFlashWriteError> {
    let slot_size = region.partition_size() as u32;
    // Reserve one sector at the tail of the partition so we never
    // accidentally span a partial sector.
    let max_writable = slot_size.saturating_sub(FLASH_SECTOR_SIZE);
    if image_bytes > max_writable {
        return Err(OtaFlashWriteError::ImageTooLargeForSlot {
            size: image_bytes,
            slot_size,
        });
    }
    let aligned = image_bytes.div_ceil(FLASH_SECTOR_SIZE) * FLASH_SECTOR_SIZE;
    info!(
        "OTA: erasing {} bytes ({} sectors) for {} byte image...",
        aligned,
        aligned / FLASH_SECTOR_SIZE,
        image_bytes
    );
    NorFlash::erase(region, 0, aligned).map_err(OtaFlashWriteError::from)?;
    info!("OTA: erase complete");
    Ok(())
}

// ---------------------------------------------------------------------------
// Phase 2: HTTP fetch + stream
// ---------------------------------------------------------------------------

/// Issue the GET, validate the response metadata against the expected
/// image size, and stream the body into the flash region. On return the
/// region borrow is released and the caller has a [`StreamResult`] for
/// post-verification.
async fn fetch_and_stream_to_flash(
    network_stack: Stack<'static>,
    url: &str,
    region: FlashRegion<'_, FlashStorage<'static>>,
    expected_size: usize,
) -> Result<StreamResult, OtaFlashWriteError> {
    let state = tcp_client_state();
    let tcp = TcpClient::new(network_stack, state);
    let dns = Ipv4LiteralDns;
    let mut client = HttpClient::new(&tcp, &dns);

    info!("OTA: downloading {} ({} bytes)", url, expected_size);

    // Issue the request.
    let mut request = client
        .request(Method::GET, url)
        .await
        .map_err(|_| OtaFlashWriteError::HttpConnect)?;

    let mut header_buf = [0u8; HTTP_HEADER_BUF_SIZE];
    let response = request
        .send(&mut header_buf)
        .await
        .map_err(|_| OtaFlashWriteError::HttpConnect)?;

    validate_response_metadata(&response, expected_size)?;

    // Stream the body to flash.
    let mut stream = StreamToFlash::new(region);
    let mut body = response.body().reader();
    loop {
        let buf = body
            .fill_buf()
            .await
            .map_err(|_| OtaFlashWriteError::ReadFailed)?;
        if buf.is_empty() {
            break;
        }
        let chunk = buf.len();
        stream.process_chunk(&buf[..chunk])?;
        body.consume(chunk);
    }
    Ok(stream.finalize())
}

/// Validate the HTTP response status, `Content-Length`, and transfer
/// encoding against the expected image size. Generic over the connection
/// type so it works for any `reqwless::Response` regardless of the
/// underlying transport.
fn validate_response_metadata<C>(
    response: &reqwless::response::Response<'_, '_, C>,
    expected_size: usize,
) -> Result<(), OtaFlashWriteError>
where
    C: embedded_io_async::Read,
{
    if response.status != StatusCode(200) {
        warn!("OTA: HTTP status {}", response.status.0);
        return Err(OtaFlashWriteError::HttpStatus(response.status.0));
    }
    let content_length = response
        .content_length
        .ok_or(OtaFlashWriteError::MissingContentLength)?;
    if content_length != expected_size {
        warn!(
            "OTA: Content-Length mismatch (expected {}, got {})",
            expected_size, content_length
        );
        return Err(OtaFlashWriteError::ContentLengthMismatch);
    }
    if response
        .transfer_encoding
        .contains(&TransferEncoding::Chunked)
    {
        warn!("OTA: chunked transfer encoding rejected");
        return Err(OtaFlashWriteError::ChunkedRejected);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Phase 3: post-download verification
// ---------------------------------------------------------------------------

/// Verify the streamed result matches the manifest's `size` and `sha256`.
fn post_verify(
    result: &StreamResult,
    expected_size: usize,
    expected_sha: &str,
) -> Result<(), OtaFlashWriteError> {
    if result.total != expected_size {
        warn!(
            "OTA: body size mismatch (expected {}, got {})",
            expected_size, result.total
        );
        return Err(OtaFlashWriteError::SizeMismatch);
    }
    let digest_hex = sha256_hex(&result.digest);
    if !digest_hex.as_str().eq_ignore_ascii_case(expected_sha) {
        warn!(
            "OTA: SHA-256 mismatch (expected {}, got {})",
            expected_sha, digest_hex
        );
        return Err(OtaFlashWriteError::ShaMismatch);
    }
    Ok(())
}

/// Read back the first [`FLASH_READBACK_LEN`] bytes of the inactive
/// partition and compare against the in-memory copy we streamed.
fn verify_flash_readback<F: ReadStorage>(
    region: &mut FlashRegion<'_, F>,
    result: &StreamResult,
) -> Result<(), OtaFlashWriteError> {
    let readback_len = FLASH_READBACK_LEN.min(result.total);
    let mut readback = [0u8; FLASH_READBACK_LEN];
    ReadStorage::read(region, 0, &mut readback[..readback_len])
        .map_err(OtaFlashWriteError::from)?;
    let compare_len = result.first_filled.min(readback_len);
    if readback[..compare_len] != result.first_bytes[..compare_len] {
        warn!(
            "OTA: flash readback mismatch (first {} bytes differ)",
            compare_len
        );
        return Err(OtaFlashWriteError::FlashVerifyFailed);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Download the OTA firmware image and stream it directly to the inactive
/// flash partition. On success, activates the new slot and reboots.
///
/// The ESP image prefix (first 8 bytes) is validated as soon as those bytes
/// are available, before any data is written to flash, so a corrupt manifest
/// never touches the inactive slot.
///
/// The `partition_table_buf` is a caller-provided buffer used temporarily
/// for reading the partition table. It must be at least
/// [`PARTITION_TABLE_MAX_LEN`] bytes.
///
/// # Errors
///
/// Returns [`OtaFlashWriteError`] on any flash, HTTP, or verification failure.
/// On success the function never returns — it reboots the chip.
#[allow(clippy::cast_possible_truncation)]
pub async fn download_and_write_to_flash(
    network_stack: Stack<'static>,
    manifest: &OtaManifest,
    flash: &mut FlashStorage<'static>,
    partition_table_buf: &mut [u8; PARTITION_TABLE_MAX_LEN],
) -> Result<Infallible, OtaFlashWriteError> {
    network::wait_for_config_up(network_stack).await;

    let mut boot_metadata = OtaBootMetadata::new(flash, partition_table_buf)?;

    // Phase 1+2: prepare the slot, fetch the image, and stream it in.
    // The `region` borrow is scoped to this block so the readback in
    // phase 3 can re-open the partition independently.
    let stream_result = {
        let (mut region, slot) = boot_metadata
            .inactive_partition()
            .map_err(OtaFlashWriteError::from)?;

        info!(
            "OTA: writing to inactive slot {:?} (size={})",
            Debug2Format(&slot),
            region.partition_size()
        );

        prepare_inactive_slot(&mut region, manifest.size)?;
        fetch_and_stream_to_flash(network_stack, &manifest.url, region, manifest.size as usize)
            .await?
    };

    // Phase 3: post-download verification.
    post_verify(&stream_result, manifest.size as usize, &manifest.sha256)?;

    {
        let (mut region, _) = boot_metadata
            .inactive_partition()
            .map_err(OtaFlashWriteError::from)?;
        verify_flash_readback(&mut region, &stream_result)?;
    }
    info!(
        "OTA: flash write complete ({} bytes, sha={}, prefix_ok)",
        stream_result.total,
        sha256_hex(&stream_result.digest),
    );
    info!(
        "OTA: readback verified (first {} bytes match)",
        stream_result.first_filled.min(FLASH_READBACK_LEN)
    );

    // Phase 4: activate and reboot.
    boot_metadata
        .activate_next_partition()
        .map_err(OtaFlashWriteError::from)?;
    boot_metadata
        .mark_current_app_new()
        .map_err(OtaFlashWriteError::from)?;

    info!(
        "OTA: new slot activated ({} bytes written), rebooting...",
        stream_result.total
    );

    software_reset()
}
