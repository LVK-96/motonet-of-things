//! Flash-aware OTA download for **v2 encrypted** firmware.
//!
//! The pipeline decrypts and verifies each chunk on the fly using the ESP32
//! hardware AES and SHA accelerators (via `esp-hal` work-queue contexts).
//!
//! 1. [`prepare_inactive_slot`] — size check + sector-aligned erase.
//! 2. [`fetch_and_process_encrypted`] — HTTP fetch, v2 header validation,
//!    chunk HMAC verify → AES-CTR decrypt → flash write + SHA-256 hash.
//! 3. [`post_verify`] + [`verify_flash_readback`] — manifest and
//!    readback checks against the decrypted result.
//! 4. Activate the new slot, mark the app `New`, and reboot.

use core::mem::MaybeUninit;
use core::net::{IpAddr, Ipv4Addr};
use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicBool, Ordering};

use defmt::{Debug2Format, info, warn};
use embassy_net::{
    Stack,
    tcp::client::{TcpClient, TcpClientState},
};
use embedded_nal_async::{AddrType, Dns};
use embedded_storage::ReadStorage;
use embedded_storage::Storage;
use embedded_storage::nor_flash::NorFlash;
use esp_bootloader_esp_idf::partitions::{
    Error as PartitionError, FlashRegion, PARTITION_TABLE_MAX_LEN,
};
use esp_hal::aes::AesBackend;
use esp_hal::rng::Rng;
use esp_hal::sha::ShaBackend;
use esp_storage::{FlashStorage, FlashStorageError};
use heapless::String;
use ota_core::{
    EspImagePrefixError, MAX_REDIRECT_URL_LEN, MAX_REDIRECTS, ManifestError, OtaManifest,
    validate_esp_image_prefix, validate_ota_url_policy,
};
use reqwless::client::{HttpClient, TlsConfig, TlsVerify};
use reqwless::headers::TransferEncoding;
use reqwless::request::{Method, RequestBuilder};
use reqwless::response::StatusCode;
use sha2::Digest;

use crate::network;
use crate::ota::OtaBootMetadata;
use crate::ota::encrypted;
use crate::secrets;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Buffer size for each direction of the OTA download TCP socket.
const OTA_TCP_BUF_SIZE: usize = 4096;
/// ESP32 flash sector size used for erase alignment.
const FLASH_SECTOR_SIZE: u32 = 4096;
/// Maximum HTTP header bytes buffered for a response.
///
/// GitHub release downloads first return a large redirect response with many
/// security/cache headers; 4 KiB is not enough for reqwless to parse it.
const HTTP_HEADER_BUF_SIZE: usize = 16 * 1024;
/// Buffer size for the HTTPS TLS record-layer read path.
///
/// GitHub release asset redirects terminate on hosts that commonly send full-size
/// TLS records; embedded-tls warns below 16,640 bytes and the handshake can fail
/// before the HTTP request is sent. Keep the write side smaller because OTA only
/// sends a compact GET request.
const OTA_TLS_READ_BUF_SIZE: usize = 16640;
const OTA_TLS_WRITE_BUF_SIZE: usize = 4096;
/// Number of leading bytes of the image retained for ESP prefix validation.
const ESP_IMAGE_PREFIX_LEN: usize = 64;
/// Number of bytes read back from flash for post-write verification.
/// The first sector (4096 bytes) covers the ESP image header and
/// early boot data that would cause catastrophic failures if corrupt.
const FLASH_READBACK_LEN: usize = 4096;

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
    RedirectWithoutLocation,
    RedirectLocationTooLong,
    RedirectRejected,
    TooManyRedirects,
    MissingContentLength,
    ContentLengthMismatch,
    ChunkedRejected,
    ReadFailed,
    SizeMismatch,
    ShaMismatch,
    InvalidImagePrefix(EspImagePrefixError),
    FlashVerifyFailed,
    /// Manifest `image_size` does not fit in the inactive OTA slot.
    ImageTooLargeForSlot {
        image_size: u32,
        slot_size: u32,
    },
    /// Bad nonce prefix hex in manifest.
    InvalidNoncePrefix,
    /// HMAC tag verification failed for a chunk.
    HmacMismatch,
    /// AES hardware operation failed during decryption.
    AesError(encrypted::AesError),
    /// OTA v2 stream header too short.
    HeaderTooShort,
    /// OTA v2 stream header has invalid magic.
    HeaderInvalidMagic,
    /// OTA v2 stream header has unsupported version.
    HeaderInvalidVersion,
    /// OTA v2 stream header has non-zero reserved bytes.
    HeaderInvalidReserved,
    /// Chunk truncated before HMAC tag.
    ChunkTruncated,
    /// Manifest canonical JSON construction failed.
    ManifestCanonical(ManifestError),
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

impl From<encrypted::OtaHeaderError> for OtaFlashWriteError {
    fn from(e: encrypted::OtaHeaderError) -> Self {
        match e {
            encrypted::OtaHeaderError::TooShort => Self::HeaderTooShort,
            encrypted::OtaHeaderError::InvalidMagic => Self::HeaderInvalidMagic,
            encrypted::OtaHeaderError::InvalidVersion => Self::HeaderInvalidVersion,
            encrypted::OtaHeaderError::ReservedNonZero => Self::HeaderInvalidReserved,
        }
    }
}

impl From<encrypted::AesError> for OtaFlashWriteError {
    fn from(e: encrypted::AesError) -> Self {
        Self::AesError(e)
    }
}

// ---------------------------------------------------------------------------
// DNS adapter
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum OtaDnsError {
    Lookup,
}

struct OtaDns {
    socket: embassy_net::dns::DnsSocket<'static>,
}

impl OtaDns {
    fn new(stack: Stack<'static>) -> Self {
        Self {
            socket: embassy_net::dns::DnsSocket::new(stack),
        }
    }
}

impl Dns for OtaDns {
    type Error = OtaDnsError;

    async fn get_host_by_name(
        &self,
        host: &str,
        addr_type: AddrType,
    ) -> Result<IpAddr, OtaDnsError> {
        if let Some(addr) = parse_ipv4_literal(host) {
            return Ok(IpAddr::V4(addr));
        }
        self.socket
            .get_host_by_name(host, addr_type)
            .await
            .map_err(|_| OtaDnsError::Lookup)
    }

    async fn get_host_by_address(
        &self,
        _addr: IpAddr,
        _result: &mut [u8],
    ) -> Result<usize, OtaDnsError> {
        Ok(0)
    }
}

fn parse_ipv4_literal(host: &str) -> Option<Ipv4Addr> {
    if host.is_empty() || host.split('.').count() != 4 {
        return None;
    }
    let mut octets = [0u8; 4];
    for (i, part) in host.split('.').enumerate() {
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let mut value: u16 = 0;
        for byte in part.bytes() {
            value = value * 10 + u16::from(byte - b'0');
        }
        let Ok(value) = u8::try_from(value) else {
            return None;
        };
        octets[i] = value;
    }
    Some(Ipv4Addr::from(octets))
}

// ---------------------------------------------------------------------------
// Shared TCP client pool (one concurrent connection)
// ---------------------------------------------------------------------------

static TCP_CLIENT_READY: AtomicBool = AtomicBool::new(false);
static mut TCP_CLIENT_BUF: MaybeUninit<TcpClientState<1, OTA_TCP_BUF_SIZE, OTA_TCP_BUF_SIZE>> =
    MaybeUninit::uninit();
static mut OTA_TLS_READ_BUF: [u8; OTA_TLS_READ_BUF_SIZE] = [0u8; OTA_TLS_READ_BUF_SIZE];
static mut OTA_TLS_WRITE_BUF: [u8; OTA_TLS_WRITE_BUF_SIZE] = [0u8; OTA_TLS_WRITE_BUF_SIZE];
static mut OTA_HTTP_HEADER_BUF: [u8; HTTP_HEADER_BUF_SIZE] = [0u8; HTTP_HEADER_BUF_SIZE];
static mut OTA_CHUNK_BUF: [u8; ota_core::ENC_CHUNK_SIZE as usize] =
    [0u8; ota_core::ENC_CHUNK_SIZE as usize];

fn tcp_client_state() -> &'static mut TcpClientState<1, OTA_TCP_BUF_SIZE, OTA_TCP_BUF_SIZE> {
    let raw: *mut TcpClientState<1, OTA_TCP_BUF_SIZE, OTA_TCP_BUF_SIZE> =
        addr_of_mut!(TCP_CLIENT_BUF).cast();
    if !TCP_CLIENT_READY.swap(true, Ordering::AcqRel) {
        unsafe {
            raw.write(TcpClientState::new());
        }
    }
    unsafe { &mut *raw }
}

fn ota_tls_buffers() -> (
    &'static mut [u8; OTA_TLS_READ_BUF_SIZE],
    &'static mut [u8; OTA_TLS_WRITE_BUF_SIZE],
) {
    unsafe {
        (
            &mut *addr_of_mut!(OTA_TLS_READ_BUF),
            &mut *addr_of_mut!(OTA_TLS_WRITE_BUF),
        )
    }
}

fn http_header_buf() -> &'static mut [u8; HTTP_HEADER_BUF_SIZE] {
    unsafe { &mut *addr_of_mut!(OTA_HTTP_HEADER_BUF) }
}

fn ota_chunk_buf() -> &'static mut [u8; ota_core::ENC_CHUNK_SIZE as usize] {
    // SAFETY: the OTA task serializes downloads; this buffer is borrowed by
    // one encrypted-body processor at a time.
    unsafe { &mut *addr_of_mut!(OTA_CHUNK_BUF) }
}

// ---------------------------------------------------------------------------
// SHA-256 hex helper
// ---------------------------------------------------------------------------

/// Convert a 32-byte SHA-256 digest to a 64-char lowercase hex string.
///
/// Uses a manual hex table so the conversion is infallible.
#[allow(clippy::unwrap_used)]
fn sha256_hex(digest: &[u8; 32]) -> String<64> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut buf = [0u8; 64];
    for (i, byte) in digest.iter().enumerate() {
        buf[i * 2] = HEX[(byte >> 4) as usize];
        buf[i * 2 + 1] = HEX[(byte & 0xF) as usize];
    }
    // SAFETY: `buf` contains only ASCII hex characters (0-9, a-f),
    // all of which are valid UTF-8.
    let s = unsafe { core::str::from_utf8_unchecked(&buf) };
    // `Vec::from_slice` cannot fail: input is exactly 64 bytes, capacity is 64.
    let vec = heapless::Vec::<u8, 64>::from_slice(s.as_bytes()).unwrap();
    // SAFETY: `vec` contains only valid UTF-8 (same hex chars).
    unsafe { String::from_utf8_unchecked(vec) }
}

// ---------------------------------------------------------------------------
// Phase 1: prepare the inactive slot
// ---------------------------------------------------------------------------

#[allow(clippy::cast_possible_truncation)]
fn prepare_inactive_slot<F: NorFlash>(
    region: &mut FlashRegion<'_, F>,
    image_bytes: u32,
) -> Result<(), OtaFlashWriteError> {
    let slot_size = region.partition_size() as u32;
    // Reserve one sector (4 KiB) at the tail of the inactive slot. This
    // headroom prevents us from touching the next partition in the table
    // even when the image is sector-aligned. It also provides a safety
    // margin against erase/write glitches at the partition boundary and
    // mirrors the bootloader's own slot headroom convention.
    let max_writable = slot_size.saturating_sub(FLASH_SECTOR_SIZE);
    if image_bytes > max_writable {
        return Err(OtaFlashWriteError::ImageTooLargeForSlot {
            image_size: image_bytes,
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
// Phase 2: HTTP(S) fetch + encrypted stream processing
// ---------------------------------------------------------------------------

const OTA_HTTP_HEADERS: &[(&str, &str)] = &[
    ("User-Agent", "motonet-of-things-esp32-ota"),
    ("Accept", "application/octet-stream"),
    ("Connection", "close"),
];

fn ota_tls_seed() -> u64 {
    let rng = Rng::new();
    (u64::from(rng.random()) << 32) | u64::from(rng.random())
}

/// Build the TLS verification config, respecting `OTA_TLS_ALLOW_INVALID_CA`.
fn ota_tls_verify() -> TlsVerify<'static> {
    if secrets::OTA_TLS_ALLOW_INVALID_CA {
        TlsVerify::None
    } else {
        TlsVerify::Certificate {
            ca: secrets::OTA_TLS_CA_CERT_DER,
            cert: None,
            key: None,
        }
    }
}

/// Output of the encrypted download and processing phase.
struct EncryptedResult {
    total_plaintext: u32,
    digest: [u8; 32],
    first_bytes: [u8; ESP_IMAGE_PREFIX_LEN],
    first_filled: usize,
}

/// Fetch the v2 encrypted OTA payload and process it chunk-by-chunk:
/// validate the 16-byte header, verify HMAC, decrypt, write plaintext to
/// flash, and accumulate a SHA-256 hash of the plaintext.
#[allow(clippy::too_many_lines)]
async fn fetch_and_process_encrypted(
    network_stack: Stack<'static>,
    manifest: &OtaManifest,
    region: &mut FlashRegion<'_, FlashStorage<'static>>,
    nonce_prefix: &[u8; 12],
    hmac_key: &[u8; encrypted::HMAC_KEY_SIZE],
    aes_key: &[u8; encrypted::AES_KEY_SIZE],
    manifest_digest: &[u8; encrypted::SHA256_OUTPUT_SIZE],
) -> Result<EncryptedResult, OtaFlashWriteError> {
    let state = tcp_client_state();
    let tcp = TcpClient::new(network_stack, state);
    let dns = OtaDns::new(network_stack);
    // Use HttpClient::new for plain HTTP (no TLS) to avoid the
    // PlainBuffered path in reqwless which may have issues with
    // the Python http.server HTTP/1.0 responses.
    let mut client = if manifest.url.starts_with("https://") {
        let (tls_read_buf, tls_write_buf) = ota_tls_buffers();
        let tls_verify = ota_tls_verify();
        let tls_config = TlsConfig::new(ota_tls_seed(), tls_read_buf, tls_write_buf, tls_verify);
        HttpClient::new_with_tls(&tcp, &dns, tls_config)
    } else {
        HttpClient::new(&tcp, &dns)
    };
    let mut current_url: String<MAX_REDIRECT_URL_LEN> = String::new();
    current_url
        .push_str(&manifest.url)
        .map_err(|_| OtaFlashWriteError::RedirectLocationTooLong)?;
    let mut redirects = 0;

    loop {
        if redirects == 0 {
            info!(
                "OTA: downloading {} (download_size={}, redirect {}/{})",
                current_url.as_str(),
                manifest.download_size,
                redirects,
                MAX_REDIRECTS
            );
        } else {
            info!(
                "OTA: downloading redirected HTTPS asset (download_size={}, redirect {}/{})",
                manifest.download_size, redirects, MAX_REDIRECTS
            );
        }

        let next_redirect = {
            let mut request = client
                .request(Method::GET, current_url.as_str())
                .await
                .map_err(|err| {
                    if redirects == 0 {
                        warn!(
                            "OTA: HTTP request/connect failed for {}: {:?}",
                            current_url.as_str(),
                            Debug2Format(&err)
                        );
                    } else {
                        warn!(
                            "OTA: HTTP request/connect failed for redirected HTTPS asset: {:?}",
                            Debug2Format(&err)
                        );
                    }
                    OtaFlashWriteError::HttpConnect
                })?
                .headers(OTA_HTTP_HEADERS);

            let response = request.send(http_header_buf()).await.map_err(|err| {
                if redirects == 0 {
                    warn!(
                        "OTA: HTTP send/response failed for {}: {:?}",
                        current_url.as_str(),
                        Debug2Format(&err)
                    );
                } else {
                    warn!(
                        "OTA: HTTP send/response failed for redirected HTTPS asset: {:?}",
                        Debug2Format(&err)
                    );
                }
                OtaFlashWriteError::HttpConnect
            })?;

            if response.status.is_redirection() {
                let next_url = redirect_location(&response)?;
                info!("OTA: following redirect {}", response.status.0);
                Some(next_url)
            } else {
                validate_response_metadata(&response, manifest.download_size as usize)?;

                // Process the encrypted body.
                let result = process_encrypted_body(
                    response.body().reader(),
                    region,
                    nonce_prefix,
                    hmac_key,
                    aes_key,
                    manifest_digest,
                    manifest.image_size,
                )
                .await?;
                return Ok(result);
            }
        };

        if redirects >= MAX_REDIRECTS {
            warn!("OTA: too many HTTP redirects");
            return Err(OtaFlashWriteError::TooManyRedirects);
        }
        let next_url = next_redirect.ok_or(OtaFlashWriteError::RedirectWithoutLocation)?;
        if !next_url.as_str().starts_with("https://")
            || validate_ota_url_policy(next_url.as_str()).is_err()
        {
            warn!("OTA: rejected redirect target {}", next_url.as_str());
            return Err(OtaFlashWriteError::RedirectRejected);
        }
        current_url = next_url;
        redirects += 1;
    }
}

fn redirect_location<C>(
    response: &reqwless::response::Response<'_, '_, C>,
) -> Result<String<MAX_REDIRECT_URL_LEN>, OtaFlashWriteError>
where
    C: embedded_io_async::Read,
{
    for (name, value) in response.headers() {
        if name.eq_ignore_ascii_case("location") {
            let location = core::str::from_utf8(value)
                .map_err(|_| OtaFlashWriteError::RedirectRejected)?
                .trim_matches(|ch: char| ch.is_ascii_whitespace());
            let mut out = String::new();
            out.push_str(location)
                .map_err(|_| OtaFlashWriteError::RedirectLocationTooLong)?;
            return Ok(out);
        }
    }
    Err(OtaFlashWriteError::RedirectWithoutLocation)
}

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

/// Per-read timeout in seconds for each `fill_buf().await` call so a
/// stalled TCP stream doesn't block the OTA pipeline indefinitely.
/// Read exactly `len` bytes from a `BufRead` into `buf`.
async fn read_exact(
    reader: &mut impl embedded_io_async::BufRead,
    buf: &mut [u8],
) -> Result<(), OtaFlashWriteError> {
    let mut offset = 0;
    while offset < buf.len() {
        let available = reader
            .fill_buf()
            .await
            .map_err(|_| OtaFlashWriteError::ReadFailed)?;
        if available.is_empty() {
            return Err(OtaFlashWriteError::ReadFailed);
        }
        let copy = available.len().min(buf.len() - offset);
        buf[offset..offset + copy].copy_from_slice(&available[..copy]);
        offset += copy;
        reader.consume(copy);
    }
    Ok(())
}

/// Process the v2 encrypted HTTP body: validate header, then for each
/// chunk verify HMAC, decrypt, write to flash, and hash the plaintext.
#[allow(clippy::too_many_arguments)]
async fn process_encrypted_body(
    mut reader: impl embedded_io_async::BufRead,
    region: &mut FlashRegion<'_, FlashStorage<'static>>,
    nonce_prefix: &[u8; 12],
    hmac_key: &[u8; encrypted::HMAC_KEY_SIZE],
    aes_key: &[u8; encrypted::AES_KEY_SIZE],
    manifest_digest: &[u8; encrypted::SHA256_OUTPUT_SIZE],
    image_size: u32,
) -> Result<EncryptedResult, OtaFlashWriteError> {
    // 1. Read and validate the 16-byte header.
    let mut header = [0u8; encrypted::OTA_V2_HEADER_SIZE];
    read_exact(&mut reader, &mut header).await?;
    encrypted::validate_ota_header(&header)?;

    // 2. Stream chunks — exactly ceil(image_size / ENC_CHUNK_SIZE) chunks.
    let num_chunks = image_size.div_ceil(ota_core::ENC_CHUNK_SIZE);
    let mut first_bytes = [0u8; ESP_IMAGE_PREFIX_LEN];
    let mut first_filled = 0usize;
    let mut prefix_validated = false;
    let mut total_plaintext: u32 = 0;
    let mut flash_offset: u32 = 0;
    // Plaintext SHA-256 accumulator (software — hardware SHA_CONTINUE
    // is broken on the target ESP32 revision for multi-block inputs).
    let mut sha_hasher = sha2::Sha256::new();
    // Buffer for reading the wire-format length prefix + HMAC tag.
    let mut len_buf = [0u8; 4];
    let mut tag_buf = [0u8; encrypted::HMAC_TAG_SIZE];
    // Reusable static ciphertext buffer sized to the maximum chunk. Keeping
    // this out of the async future avoids a multi-kilobyte OTA task frame.
    let ciphertext_buf = ota_chunk_buf();

    for chunk_index in 0..num_chunks {
        // Read 4-byte plaintext length (big-endian).
        read_exact(&mut reader, &mut len_buf).await?;
        let ciphertext_len = u32::from_be_bytes(len_buf);

        // Enforce exact expected length per chunk: full chunk for all but
        // the last, remainder for the last.
        let expected_len = if chunk_index == num_chunks - 1 {
            image_size - chunk_index * ota_core::ENC_CHUNK_SIZE
        } else {
            ota_core::ENC_CHUNK_SIZE
        };
        if ciphertext_len != expected_len {
            warn!(
                "OTA: chunk {} length {} != expected {}",
                chunk_index, ciphertext_len, expected_len
            );
            return Err(OtaFlashWriteError::ChunkTruncated);
        }

        let ct = &mut ciphertext_buf[..ciphertext_len as usize];
        read_exact(&mut reader, ct).await?;
        read_exact(&mut reader, &mut tag_buf).await?;

        // Verify HMAC before decrypting.
        if !encrypted::verify_chunk_hmac(
            hmac_key,
            manifest_digest,
            chunk_index,
            flash_offset,
            ciphertext_len,
            ct,
            &tag_buf,
        ) {
            let computed = encrypted::compute_chunk_hmac(
                hmac_key,
                manifest_digest,
                chunk_index,
                flash_offset,
                ciphertext_len,
                ct,
            );
            warn!("OTA: HMAC verification failed for chunk {}", chunk_index);
            warn!("  manifest_digest: {}", sha256_hex(manifest_digest));
            warn!(
                "  chunk_index: {}, flash_offset: {}, ct_len: {}",
                chunk_index, flash_offset, ciphertext_len
            );
            warn!("  tag (from file):    {}", sha256_hex(&tag_buf));
            warn!("  hmac (device computed): {}", sha256_hex(&computed));
            return Err(OtaFlashWriteError::HmacMismatch);
        }

        // Decrypt in-place.
        encrypted::aes_ctr_crypt_in_place(aes_key, nonce_prefix, chunk_index, ct)?;

        let plaintext = ct; // now decrypted

        // Validate ESP image prefix on the first bytes of plaintext, before
        // writing anything to flash.
        if !prefix_validated {
            let copy = plaintext
                .len()
                .min(ESP_IMAGE_PREFIX_LEN.saturating_sub(first_filled));
            first_bytes[first_filled..first_filled + copy].copy_from_slice(&plaintext[..copy]);
            first_filled += copy;
            if first_filled >= 8 {
                validate_esp_image_prefix(&first_bytes[..first_filled])?;
                prefix_validated = true;
            }
        }

        // Write plaintext to flash.
        Storage::write(region, flash_offset, plaintext).map_err(OtaFlashWriteError::from)?;

        // Hash the plaintext (software).
        sha2::Digest::update(&mut sha_hasher, plaintext);

        flash_offset += ciphertext_len;
        total_plaintext += ciphertext_len;
    }

    // 3. Finalize SHA-256.
    let mut digest = [0u8; encrypted::SHA256_OUTPUT_SIZE];
    let result = sha2::Digest::finalize(sha_hasher);
    digest.copy_from_slice(&result);

    Ok(EncryptedResult {
        total_plaintext,
        digest,
        first_bytes,
        first_filled,
    })
}

// ---------------------------------------------------------------------------
// Phase 3: post-download verification
// ---------------------------------------------------------------------------

fn post_verify(result: &EncryptedResult, manifest: &OtaManifest) -> Result<(), OtaFlashWriteError> {
    if result.total_plaintext != manifest.image_size {
        warn!(
            "OTA: image size mismatch (expected {}, got {})",
            manifest.image_size, result.total_plaintext
        );
        return Err(OtaFlashWriteError::SizeMismatch);
    }
    let digest_hex = sha256_hex(&result.digest);
    if !digest_hex
        .as_str()
        .eq_ignore_ascii_case(manifest.image_sha256.as_str())
    {
        warn!(
            "OTA: SHA-256 mismatch (expected {}, got {})",
            Debug2Format(&manifest.image_sha256),
            Debug2Format(&digest_hex)
        );
        return Err(OtaFlashWriteError::ShaMismatch);
    }
    Ok(())
}

fn verify_flash_readback<F: ReadStorage>(
    region: &mut FlashRegion<'_, F>,
    result: &EncryptedResult,
) -> Result<(), OtaFlashWriteError> {
    let readback_len = FLASH_READBACK_LEN.min(result.total_plaintext as usize);
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

/// Download the v2 encrypted OTA firmware and stream it to the inactive
/// flash partition. On success, activates the new slot and reboots.
///
/// The AES and SHA hardware-accelerator backends must already be started
/// (their drivers alive) in the caller. This function creates transient
/// `AesContext` / `Sha256Context` instances that submit work to the
/// shared work queues.
///
/// # Errors
///
/// Returns [`OtaFlashWriteError`] on any flash, HTTP, crypto, or
/// verification failure. On success the function never returns — it
/// reboots the chip.
#[allow(clippy::cast_possible_truncation)]
pub async fn download_and_write_to_flash(
    network_stack: Stack<'static>,
    manifest: &OtaManifest,
    flash: &mut FlashStorage<'static>,
    partition_table_buf: &mut [u8; PARTITION_TABLE_MAX_LEN],
    master_key: &[u8; 32],
) -> Result<(), OtaFlashWriteError> {
    // SAFETY: crypto peripherals are initialised once during hw_setup.
    let aes = unsafe { crate::ota::take_aes() };
    let sha = unsafe { crate::ota::take_sha() };
    let mut aes_backend = AesBackend::new(aes);
    let aes_driver = aes_backend.start();
    let mut sha_backend = ShaBackend::new(sha);
    let sha_driver = sha_backend.start();
    // Derive per-manifest subkeys from the encryption key id.
    let (aes_key, hmac_key) = encrypted::derive_subkeys(master_key, manifest.enc.key_id);

    // Log public manifest metadata for cross-reference with packaging.
    info!(
        "OTA: enc.key_id={}, nonce_prefix={}",
        manifest.enc.key_id,
        manifest.enc.nonce_prefix.as_str()
    );

    // Parse nonce prefix.
    let nonce_prefix = encrypted::parse_nonce_prefix(&manifest.enc.nonce_prefix)
        .map_err(|_e| OtaFlashWriteError::InvalidNoncePrefix)?;

    // Compute manifest digest.
    let canonical = manifest
        .canonical_unsigned_json()
        .map_err(OtaFlashWriteError::ManifestCanonical)?;
    let manifest_digest = encrypted::compute_manifest_digest(&canonical);
    info!("OTA: manifest_digest={}", sha256_hex(&manifest_digest));

    network::wait_for_config_up(network_stack).await;

    let mut boot_metadata = OtaBootMetadata::new(flash, partition_table_buf)?;

    // Phase 1+2: prepare the inactive slot, fetch the encrypted download,
    // decrypt/verify/write chunk-by-chunk.
    let stream_result = {
        let (mut region, slot) = boot_metadata
            .inactive_partition()
            .map_err(OtaFlashWriteError::from)?;

        info!(
            "OTA: writing to inactive slot {:?} (size={})",
            Debug2Format(&slot),
            region.partition_size()
        );

        prepare_inactive_slot(&mut region, manifest.image_size)?;
        fetch_and_process_encrypted(
            network_stack,
            manifest,
            &mut region,
            &nonce_prefix,
            &hmac_key,
            &aes_key,
            &manifest_digest,
        )
        .await?
    };

    // Phase 3: post-download verification.
    post_verify(&stream_result, manifest)?;

    {
        let (mut region, _) = boot_metadata
            .inactive_partition()
            .map_err(OtaFlashWriteError::from)?;
        verify_flash_readback(&mut region, &stream_result)?;
    }
    info!(
        "OTA: flash write complete ({} bytes, sha={}, prefix_ok)",
        stream_result.total_plaintext,
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
        stream_result.total_plaintext
    );

    // Drop hardware crypto backends and their peripheral guards
    // before returning so the caller can reset cleanly.
    drop(aes_driver);
    drop(aes_backend);
    drop(sha_driver);
    drop(sha_backend);

    Ok(())
}
