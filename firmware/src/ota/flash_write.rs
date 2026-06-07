//! Flash-aware OTA download that streams firmware directly into the inactive
//! partition, verifies integrity, activates the new slot, and reboots.

use core::fmt::Write as _;

use defmt::{info, warn};
use embassy_net::{Stack, tcp::client::TcpClient};
use embedded_io_async::BufRead as _;
use embedded_storage::ReadStorage;
use embedded_storage::Storage;
use embedded_storage::nor_flash::NorFlash;
use esp_bootloader_esp_idf::partitions::{Error as PartitionError, PARTITION_TABLE_MAX_LEN};
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
use crate::ota::download::{Ipv4LiteralDns, tcp_client_state};

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
// SHA-256 hex helpers
// ---------------------------------------------------------------------------

fn sha256_hex(digest: &[u8; 32]) -> String<64> {
    let mut out = String::new();
    for byte in digest {
        write!(out, "{byte:02x}").ok();
    }
    out
}

// ---------------------------------------------------------------------------
// Flash-write download
// ---------------------------------------------------------------------------

/// Download the OTA firmware image and stream it directly to the inactive
/// flash partition. On success, activates the new slot and reboots.
///
/// The `partition_table_buf` is a caller-provided buffer used temporarily
/// for reading the partition table. It must be at least
/// [`PARTITION_TABLE_MAX_LEN`] bytes.
///
/// # Errors
///
/// Returns [`OtaFlashWriteError`] on any flash, HTTP, or verification failure.
/// On success this function never returns (it reboots the chip).
#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]
pub async fn download_and_write_to_flash(
    network_stack: Stack<'static>,
    manifest: &OtaManifest,
    flash: &mut FlashStorage<'static>,
    partition_table_buf: &mut [u8; PARTITION_TABLE_MAX_LEN],
) -> Result<(), OtaFlashWriteError> {
    network::wait_for_config_up(network_stack).await;

    // ── 1.  Parse partition table + get inactive slot ──────────────────
    let mut boot_metadata = OtaBootMetadata::new(flash, partition_table_buf)?;

    // ── 2-5.  Erase, download, verify (region operations) ──────────────
    let total = {
        let (mut region, slot) = boot_metadata
            .inactive_partition()
            .map_err(OtaFlashWriteError::from)?;

        info!(
            "OTA: writing to inactive slot {:?} (size={})",
            slot,
            region.partition_size()
        );

        // ── Erase sectors needed for the image ─────────────────────
        let image_bytes = manifest.size;
        // ESP32 flash sector size = 4096 bytes
        let sector: u32 = 4096;
        let erase_len = if region.partition_size() > 0 {
            let aligned = image_bytes.div_ceil(sector) * sector;
            aligned.min((region.partition_size() - 1) as u32)
        } else {
            return Err(OtaFlashWriteError::Partition(
                esp_bootloader_esp_idf::partitions::Error::OutOfBounds,
            ));
        };
        info!(
            "OTA: erasing {} bytes ({} sectors) for {} byte image...",
            erase_len,
            erase_len / sector,
            image_bytes
        );
        NorFlash::erase(&mut region, 0, erase_len).map_err(OtaFlashWriteError::from)?;
        info!("OTA: erase complete");

        // ── Download via HTTP, write to flash, hash on the fly ─────────
        let state = tcp_client_state();
        let tcp = TcpClient::new(network_stack, state);
        let dns = Ipv4LiteralDns;
        let mut client = HttpClient::new(&tcp, &dns);

        let url = manifest.url.as_str();
        let expected_size = manifest.size as usize;
        let expected_sha = manifest.sha256.as_str();

        info!("OTA: downloading {} ({} bytes)", url, expected_size);

        // ── issue GET request ──────────────────────────────────────────
        let mut request = client
            .request(Method::GET, url)
            .await
            .map_err(|_| OtaFlashWriteError::HttpConnect)?;

        let mut header_buf = [0u8; 1024];
        let response = request
            .send(&mut header_buf)
            .await
            .map_err(|_| OtaFlashWriteError::HttpConnect)?;

        // ── validate response metadata ─────────────────────────────────
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

        // ── stream body → flash + hash ─────────────────────────────────
        let mut body = response.body().reader();
        let mut first_bytes = [0u8; 64];
        let mut first_filled: usize = 0;
        let mut total: usize = 0;
        let mut flash_offset: u32 = 0;
        let mut hasher = Sha256::new();

        loop {
            let buf = body
                .fill_buf()
                .await
                .map_err(|_| OtaFlashWriteError::ReadFailed)?;
            if buf.is_empty() {
                break;
            }
            let chunk = buf.len();

            // Capture the first 64 bytes for ESP prefix validation.
            let remaining = 64usize.saturating_sub(first_filled);
            let copy = chunk.min(remaining);
            if copy > 0 {
                first_bytes[first_filled..first_filled + copy].copy_from_slice(&buf[..copy]);
                first_filled += copy;
            }

            // Write chunk to flash and hash.
            hasher.update(&buf[..chunk]);

            Storage::write(&mut region, flash_offset, &buf[..chunk])
                .map_err(OtaFlashWriteError::from)?;

            flash_offset += chunk as u32;
            total += chunk;
            body.consume(chunk);
        }

        // ── Post-download verification ─────────────────────────────────
        if total != expected_size {
            warn!(
                "OTA: body size mismatch (expected {}, got {})",
                expected_size, total
            );
            return Err(OtaFlashWriteError::SizeMismatch);
        }

        let digest = hasher.finalize();
        let digest_hex = sha256_hex(&digest.into());
        if !digest_hex.as_str().eq_ignore_ascii_case(expected_sha) {
            warn!(
                "OTA: SHA-256 mismatch (expected {}, got {})",
                expected_sha, digest_hex
            );
            return Err(OtaFlashWriteError::ShaMismatch);
        }

        validate_esp_image_prefix(&first_bytes[..first_filled])?;

        // ── Read back first 256 bytes from flash and compare ───────────
        let readback_len = 256.min(total);
        let mut readback = [0u8; 256];
        ReadStorage::read(&mut region, 0, &mut readback[..readback_len])
            .map_err(OtaFlashWriteError::from)?;

        // Compare with saved prefix (first 64 bytes).
        let compare_len = first_filled.min(readback_len);
        if readback[..compare_len] != first_bytes[..compare_len] {
            warn!(
                "OTA: flash readback mismatch (first {} bytes differ)",
                compare_len
            );
            return Err(OtaFlashWriteError::FlashVerifyFailed);
        }

        info!(
            "OTA: flash write complete ({} bytes, sha={}, prefix_ok)",
            total, digest_hex
        );
        info!("OTA: readback verified ({} bytes match)", compare_len);

        // Region goes out of scope here, releasing the borrow on
        // boot_metadata so activate_next_partition can take &mut self.
        total
    };

    // ── 6.  Activate the new slot ──────────────────────────────────────
    boot_metadata
        .activate_next_partition()
        .map_err(OtaFlashWriteError::from)?;
    boot_metadata
        .mark_current_app_new()
        .map_err(OtaFlashWriteError::from)?;

    info!(
        "OTA: new slot activated ({} bytes written), rebooting...",
        total
    );

    // ── 7.  Reboot ─────────────────────────────────────────────────────
    software_reset();
}
