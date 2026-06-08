#!/bin/bash
# Encrypted OTA packaging tool — encrypts firmware, builds signed v2 manifest.
#
# Arguments (all required unless noted):
#   --input FILE                  Plaintext firmware binary
#   --output FILE                 Encrypted container output
#   --manifest FILE               Signed manifest JSON output
#   --url URL                     Download URL placed in manifest
#   --version VERSION             Firmware version string
#   --build NUM                   Monotonic build number
#   --signing-seed-hex-file FILE  Ed25519 seed (64 hex chars)
#   --ota-key-id NUM              Encryption key id (u32)
#   --ota-master-key-hex-file F   Master key (64 hex chars); env fallback
#   --nonce-prefix-hex HEX        Optional 24-char nonce prefix (auto-gen if unset)
#
# Environment:
#   OTA_ENCRYPTION_MASTER_KEY_HEX   Master key fallback (64 hex chars)

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
CHUNK_SIZE=4096
TARGET="motonet-of-things/esp32"
CHIP="esp32-wroom"

WORKDIR=""
cleanup() {
    if [[ -n "${WORKDIR:-}" && -d "$WORKDIR" ]]; then
        rm -rf "$WORKDIR"
    fi
}
trap cleanup EXIT
WORKDIR="$(mktemp -d)" || { echo "Error: failed to create temporary directory" >&2; exit 1; }

usage() {
    cat <<'EOF'
Usage: ota-pack.sh [OPTIONS]

Required:
  --input FILE                  Plaintext firmware binary
  --output FILE                 Encrypted container output
  --manifest FILE               Signed manifest JSON output
  --url URL                     Download URL placed in manifest
  --version VERSION             Firmware version string
  --build NUM                   Monotonic build number
  --signing-seed-hex-file FILE  Ed25519 seed (64 hex chars)
  --ota-key-id NUM              Encryption key id (u32)

Required (one of):
  --ota-master-key-hex-file F   Master key file (64 hex chars)
  OTA_ENCRYPTION_MASTER_KEY_HEX env var (64 hex chars)

Optional:
  --signing-key-id NUM          Manifest signing key_id (default 1001)
  --nonce-prefix-hex HEX        24-char hex nonce prefix (auto-gen if unset)

Examples:
  # dev manifest (key_id 1001)
  ota-pack.sh \
    --input target/ota/firmware.bin \
    --output target/ota/firmware.bin.enc \
    --manifest target/ota/firmware.manifest.json \
    --url https://example.com/firmware.bin.enc \
    --version 0.2.0 --build 42 \
    --signing-seed-hex-file tools/ota/keys/dev_ed25519.seed.hex \
    --ota-key-id 1 \
    --ota-master-key-hex-file tools/ota/keys/dev_master.hex

  OTA_ENCRYPTION_MASTER_KEY_HEX=... ota-pack.sh ...
EOF
}

require_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "Error: required command not found: $1" >&2
        exit 1
    fi
}

validate_hex_chars() {
    local name="$1"
    local value="$2"
    local expected_len="$3"
    if [[ ${#value} -ne $expected_len ]]; then
        echo "Error: $name must be $((expected_len / 2)) bytes ($expected_len hex chars), got ${#value}" >&2
        exit 1
    fi
    if [[ ! "$value" =~ ^[0-9a-fA-F]+$ ]]; then
        echo "Error: $name must contain only hex characters" >&2
        exit 1
    fi
}

# ── Parse args ────────────────────────────────────────────────────────────

input=""
output=""
manifest_out=""
url=""
version=""
build_num=""
seed_hex_file=""
ota_key_id=""
master_key_hex=""
signing_key_id=""
nonce_prefix=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --input) input="$2"; shift 2 ;;
        --output) output="$2"; shift 2 ;;
        --manifest) manifest_out="$2"; shift 2 ;;
        --url) url="$2"; shift 2 ;;
        --version) version="$2"; shift 2 ;;
        --build) build_num="$2"; shift 2 ;;
        --signing-key-id) signing_key_id="$2"; shift 2 ;;
        --signing-seed-hex-file) seed_hex_file="$2"; shift 2 ;;
        --ota-key-id) ota_key_id="$2"; shift 2 ;;
        --ota-master-key-hex-file) master_key_hex="$(tr -d '[:space:]' < "$2")"; shift 2 ;;
        --nonce-prefix-hex) nonce_prefix="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Error: unknown option: $1" >&2; usage >&2; exit 1 ;;
    esac
done

# ── Validate args ──────────────────────────────────────────────────────────

if [[ -z "$input" || -z "$output" || -z "$manifest_out" || -z "$url" || -z "$version" || -z "$build_num" || -z "$seed_hex_file" || -z "$ota_key_id" ]]; then
    echo "Error: missing required arguments" >&2
    usage >&2
    exit 1
fi

if [[ ! -f "$input" ]]; then
    echo "Error: input file not found: $input" >&2
    exit 1
fi

if [[ ! -f "$seed_hex_file" ]]; then
    echo "Error: signing seed file not found: $seed_hex_file" >&2
    exit 1
fi

if [[ ! "$build_num" =~ ^[0-9]+$ ]]; then
    echo "Error: --build must be a non-negative integer, got: $build_num" >&2
    exit 1
fi

if [[ ! "$ota_key_id" =~ ^[0-9]+$ ]]; then
    echo "Error: --ota-key-id must be a non-negative integer, got: $ota_key_id" >&2
    exit 1
fi

# Signing key_id (default 1001 for dev, must be 1 for release)
if [[ -z "$signing_key_id" ]]; then
    signing_key_id=1001
fi
if [[ ! "$signing_key_id" =~ ^[0-9]+$ ]]; then
    echo "Error: --signing-key-id must be a non-negative integer, got: $signing_key_id" >&2
    exit 1
fi

# Master key resolution
if [[ -z "$master_key_hex" ]]; then
    master_key_hex="${OTA_ENCRYPTION_MASTER_KEY_HEX:-}"
fi
if [[ -z "$master_key_hex" ]]; then
    echo "Error: no master key provided (use --ota-master-key-hex-file or OTA_ENCRYPTION_MASTER_KEY_HEX)" >&2
    exit 1
fi
master_key_hex="$(tr -d '[:space:]' <<<"$master_key_hex")"
validate_hex_chars "master key" "$master_key_hex" 64

# Seed file
seed_hex="$(tr -d '[:space:]' < "$seed_hex_file")"
validate_hex_chars "signing seed" "$seed_hex" 64

# Nonce prefix
if [[ -z "$nonce_prefix" ]]; then
    require_cmd openssl
    nonce_prefix="$(openssl rand -hex 12)"
fi
if [[ ${#nonce_prefix} -ne 24 ]]; then
    echo "Error: nonce prefix must be 12 bytes (24 hex chars), got ${#nonce_prefix}" >&2
    exit 1
fi
if [[ ! "$nonce_prefix" =~ ^[0-9a-fA-F]{24}$ ]]; then
    echo "Error: nonce prefix must be hex" >&2
    exit 1
fi

# Ensure tools
require_cmd openssl
require_cmd jq
require_cmd xxd
require_cmd sha256sum

# OpenSSL pkeyutl -rawin required for Ed25519 signing
if ! openssl pkeyutl -help 2>&1 | grep -q -- '-rawin'; then
    echo "Error: openssl does not support 'pkeyutl -rawin' (need OpenSSL >= 1.1.1)" >&2
    exit 1
fi

# ── Read input, compute sizes and hashes ───────────────────────────────────

cp "$input" "$WORKDIR/plaintext.bin"
image_size="$(wc -c < "$WORKDIR/plaintext.bin")"

if [[ "$image_size" -eq 0 ]]; then
    echo "Error: input file is empty" >&2
    exit 1
fi

image_sha256="$(sha256sum "$WORKDIR/plaintext.bin" | cut -d' ' -f1)"

num_chunks="$(( (image_size + CHUNK_SIZE - 1) / CHUNK_SIZE ))"
download_size="$(( 16 + image_size + num_chunks * 36 ))"

# ── Derive encryption keys ─────────────────────────────────────────────────

# Derive aes_key = HMAC-SHA256(master_key, "motonet-ota aes-256-ctr key v1" || key_id_be)
aes_key="$(
    (
        printf '%s' "motonet-ota aes-256-ctr key v1"
        printf '%08x' "$ota_key_id" | xxd -r -p
    ) | openssl dgst -sha256 -mac HMAC -macopt hexkey:"$master_key_hex" -binary | xxd -p | tr -d '\n'
)"

# Derive hmac_key = HMAC-SHA256(master_key, "motonet-ota hmac-sha256 key v1" || key_id_be)
hmac_key="$(
    (
        printf '%s' "motonet-ota hmac-sha256 key v1"
        printf '%08x' "$ota_key_id" | xxd -r -p
    ) | openssl dgst -sha256 -mac HMAC -macopt hexkey:"$master_key_hex" -binary | xxd -p | tr -d '\n'
)"

# ── Build unsigned canonical manifest JSON ─────────────────────────────────

unsigned_json="$(jq -cn \
    --argjson schema 2 \
    --argjson key_id "$signing_key_id" \
    --arg target "$TARGET" \
    --arg chip "$CHIP" \
    --arg version "$version" \
    --argjson build "$build_num" \
    --argjson force false \
    --arg url "$url" \
    --argjson download_size "$download_size" \
    --argjson image_size "$image_size" \
    --arg image_sha256 "$image_sha256" \
    --arg enc_alg "AES-256-CTR-HMAC-SHA256" \
    --argjson enc_key_id "$ota_key_id" \
    --argjson enc_chunk_size "$CHUNK_SIZE" \
    --arg enc_nonce_prefix "$nonce_prefix" \
    '{schema:$schema,key_id:$key_id,target:$target,chip:$chip,version:$version,build:$build,force:$force,url:$url,download_size:$download_size,image_size:$image_size,image_sha256:$image_sha256,enc:{alg:$enc_alg,key_id:$enc_key_id,chunk_size:$enc_chunk_size,nonce_prefix:$enc_nonce_prefix}}')"

# Compute manifest_digest = SHA256(canonical unsigned manifest JSON)
manifest_digest="$(printf '%s' "$unsigned_json" | sha256sum | cut -d' ' -f1)"
printf '%s' "$manifest_digest" | xxd -r -p > "$WORKDIR/manifest_digest.bin"

compute_chunk_hmac() {
    local chunk_idx="$1"
    local plain_offset="$2"
    local plain_len="$3"
    local cipher_file="$4"

    (
        printf '%s' "MOTONET-OTA-CHUNK-v2"
        cat "$WORKDIR/manifest_digest.bin"
        printf '%08x' "$chunk_idx" | xxd -r -p
        printf '%08x' "$plain_offset" | xxd -r -p
        printf '%08x' "$plain_len" | xxd -r -p
        cat "$cipher_file"
    ) | openssl dgst -sha256 -mac HMAC -macopt hexkey:"$hmac_key" -binary | xxd -p | tr -d '\n'
}

# ── Sign manifest with Ed25519 ─────────────────────────────────────────────

# PKCS8 DER for Ed25519 private key:
#   302e (SEQUENCE 46) 020100 (INTEGER 0) 300506032b6570 (SEQUENCE + OID 1.3.101.112)
#   0422 (OCTET STRING 34) 0420 (OCTET STRING 32) + seed (32 bytes)
der_hex="302e020100300506032b657004220420${seed_hex}"
printf '%s' "$der_hex" | xxd -r -p > "$WORKDIR/signing_key.der"
openssl pkey -inform DER -in "$WORKDIR/signing_key.der" -out "$WORKDIR/signing_key.pem" \
    || { echo "Error: failed to parse Ed25519 seed as PKCS8 key" >&2; exit 1; }

printf '%s' "$unsigned_json" > "$WORKDIR/unsigned.json"
signature_hex="$(openssl pkeyutl -sign -rawin -in "$WORKDIR/unsigned.json" -inkey "$WORKDIR/signing_key.pem" | xxd -p | tr -d '\n')"
validate_hex_chars "signature" "$signature_hex" 128

# Build final signed manifest JSON (signature last field)
signed_json="$(printf '%s' "$unsigned_json" | jq -c --arg sig "$signature_hex" '. + {signature:$sig}')"

# ── Write encrypted container ──────────────────────────────────────────────

# Header: magic[8]="MOTOTA2\0", version[1]=2, reserved[7]=0x00
printf '%s' '4d4f544f544132000200000000000000' | xxd -r -p > "$output"

for ((chunk_idx = 0; chunk_idx < num_chunks; chunk_idx++)); do
    plain_offset="$(( chunk_idx * CHUNK_SIZE ))"

    if [[ $chunk_idx -eq $((num_chunks - 1)) ]]; then
        plain_len="$(( image_size - plain_offset ))"
    else
        plain_len="$CHUNK_SIZE"
    fi

    # Extract plaintext slice
    dd if="$WORKDIR/plaintext.bin" bs=1 skip="$plain_offset" count="$plain_len" \
        of="$WORKDIR/chunk_plain.bin" 2>/dev/null

    # Encrypt with AES-256-CTR: IV = nonce_prefix || chunk_index_be
    iv_hex="${nonce_prefix}$(printf '%08x' "$chunk_idx")"
    openssl enc -aes-256-ctr -K "$aes_key" -iv "$iv_hex" \
        -in "$WORKDIR/chunk_plain.bin" -out "$WORKDIR/chunk_cipher.bin"

    # Compute HMAC tag
    tag="$(compute_chunk_hmac "$chunk_idx" "$plain_offset" "$plain_len" "$WORKDIR/chunk_cipher.bin")"

    # Write: len_be[4] || ciphertext || tag[32]
    printf '%08x' "$plain_len" | xxd -r -p >> "$output"
    cat "$WORKDIR/chunk_cipher.bin" >> "$output"
    printf '%s' "$tag" | xxd -r -p >> "$output"
done

# ── Write manifest ─────────────────────────────────────────────────────────

printf '%s\n' "$signed_json" > "$manifest_out"

# ── Self-check ─────────────────────────────────────────────────────────────

echo "Self-check: verifying output..." >&2

# 1. Verify file size formula
actual_size="$(wc -c < "$output")"
if [[ "$actual_size" -ne "$download_size" ]]; then
    echo "ERROR: file size mismatch: expected $download_size, got $actual_size" >&2
    exit 1
fi
echo "  File size OK ($actual_size bytes)" >&2

# 2. Verify header
header_hex="$(dd if="$output" bs=1 count=16 2>/dev/null | xxd -p | tr -d '\n')"
expected_header="4d4f544f544132000200000000000000"
if [[ "$header_hex" != "$expected_header" ]]; then
    echo "ERROR: header mismatch" >&2
    echo "  Got:      $header_hex" >&2
    echo "  Expected: $expected_header" >&2
    exit 1
fi
echo "  Header OK" >&2

# 3. Verify all chunks: tags, decrypt, accumulate plaintext
pos=16
> "$WORKDIR/check_decrypted.bin"  # truncate

for ((chunk_idx = 0; chunk_idx < num_chunks; chunk_idx++)); do
    # Read plaintext length
    len_be="$(dd if="$output" bs=1 skip="$pos" count=4 2>/dev/null | xxd -p | tr -d '\n')"
    chunk_plain_len="$(( 16#$len_be ))"
    pos=$((pos + 4))

    # Validate length
    if [[ $chunk_idx -eq $((num_chunks - 1)) ]]; then
        expected_len="$(( image_size - chunk_idx * CHUNK_SIZE ))"
    else
        expected_len="$CHUNK_SIZE"
    fi
    if [[ "$chunk_plain_len" -ne "$expected_len" ]]; then
        echo "ERROR: chunk $chunk_idx length mismatch: expected $expected_len, got $chunk_plain_len" >&2
        exit 1
    fi

    # Read ciphertext
    dd if="$output" bs=1 skip="$pos" count="$chunk_plain_len" \
        of="$WORKDIR/check_cipher.bin" 2>/dev/null
    pos=$((pos + chunk_plain_len))

    # Read tag
    dd if="$output" bs=1 skip="$pos" count=32 \
        of="$WORKDIR/check_tag.bin" 2>/dev/null
    pos=$((pos + 32))
    tag_actual="$(xxd -p "$WORKDIR/check_tag.bin" | tr -d '\n')"

    # Recompute expected tag
    plain_offset="$(( chunk_idx * CHUNK_SIZE ))"
    tag_expected="$(compute_chunk_hmac "$chunk_idx" "$plain_offset" "$chunk_plain_len" "$WORKDIR/check_cipher.bin")"

    if [[ "$tag_actual" != "$tag_expected" ]]; then
        echo "ERROR: chunk $chunk_idx tag mismatch" >&2
        exit 1
    fi

    # Decrypt
    iv_hex="${nonce_prefix}$(printf '%08x' "$chunk_idx")"
    openssl enc -d -aes-256-ctr -K "$aes_key" -iv "$iv_hex" \
        -in "$WORKDIR/check_cipher.bin" -out "$WORKDIR/check_plain.bin"

    cat "$WORKDIR/check_plain.bin" >> "$WORKDIR/check_decrypted.bin"
done

# 4. Verify total decrypted size
decrypted_size="$(wc -c < "$WORKDIR/check_decrypted.bin")"
if [[ "$decrypted_size" -ne "$image_size" ]]; then
    echo "ERROR: decrypted size mismatch: expected $image_size, got $decrypted_size" >&2
    exit 1
fi

# 5. Verify decrypted SHA256 matches image_sha256
decrypted_sha256="$(sha256sum "$WORKDIR/check_decrypted.bin" | cut -d' ' -f1)"
if [[ "$decrypted_sha256" != "$image_sha256" ]]; then
    echo "ERROR: decrypted SHA256 mismatch" >&2
    echo "  Expected: $image_sha256" >&2
    echo "  Got:      $decrypted_sha256" >&2
    exit 1
fi

echo "  All $num_chunks chunks verified OK" >&2
echo "  Decrypted SHA256 matches image_sha256" >&2
echo "" >&2
echo "Self-check PASSED" >&2
echo "" >&2

# ── Summary ────────────────────────────────────────────────────────────────

cat <<EOF
============================================
 OTA Package Created
============================================
 Input:          $input ($image_size bytes)
 Output:         $output ($download_size bytes)
 Manifest:       $manifest_out
 Version:        $version
 Build:          $build_num
 URL:            $url
 Chunks:         $num_chunks x $CHUNK_SIZE
 Nonce prefix:   $nonce_prefix
 Image SHA256:   $image_sha256
============================================
EOF
