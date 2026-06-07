#!/bin/bash
# Simple MQTT test setup for Rubicson sensor development
# Supports plaintext (1883) and server-TLS-only (8883) test flows.

set -euo pipefail

BROKER_PORT_PLAIN=1883
BROKER_PORT_TLS=8883
TOPIC="sensors/rubicson/#"
DEVICE_ID="${DEVICE_ID:-test-sensor}"
OTA_MANIFEST_FILE="${OTA_MANIFEST_FILE:-}"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
TLS_DIR="$SCRIPT_DIR/.mqtt-test-tls"
CA_KEY="$TLS_DIR/ca.key"
CA_CERT="$TLS_DIR/ca.crt"
CA_DER="$TLS_DIR/ca.der"
SERVER_KEY="$TLS_DIR/server.key"
SERVER_CSR="$TLS_DIR/server.csr"
SERVER_CERT="$TLS_DIR/server.crt"
SERVER_EXT="$TLS_DIR/server.ext"
CA_SRL="$TLS_DIR/ca.srl"
TLS_PARAMS_FILE="$TLS_DIR/.tls-params"
TLS_SERVER_CN="${MQTT_TEST_TLS_SERVER_CN:-localhost}"
TLS_SERVER_SAN="${MQTT_TEST_TLS_SERVER_SAN:-DNS:localhost,IP:127.0.0.1}"
TLS_KEY_TYPE="${MQTT_TEST_TLS_KEY_TYPE:-ec}"

CONF_FILE=""
BROKER_PID=""
SERVE_TMPDIR=""

usage() {
    cat <<EOF
Usage:
  $0 [broker|sub|pub|ota-smoke|ota-build|ota-serve|ota-send|all] [plain|tls]
  $0 [plain|tls]

Commands:
  broker     Start broker only
  sub        Start subscriber (auto-starts broker if needed)
  pub        Publish a telemetry test message
  ota-smoke  Publish an OTA smoke-test manifest to motonet/\$DEVICE_ID/cmd/ota
  ota-build  Build firmware and package into target/ota/firmware.bin
  ota-serve  Serve firmware.bin over HTTP (for device OTA download)
  ota-send   Build, sign, and publish a real OTA manifest via MQTT
  all        Start broker + subscriber (default)

  (The legacy 'ota' command is an alias for 'ota-smoke'.)

Modes (for broker, sub, pub, ota-smoke, ota-send, all):
  plain   Use plaintext MQTT on port $BROKER_PORT_PLAIN (default)
  tls     Use server-TLS-only MQTT on port $BROKER_PORT_TLS (local CA + server cert)

Examples:
  $0
  $0 broker
  $0 pub plain
  $0 ota-smoke plain
  $0 ota-build
  $0 ota-serve --port 9000
  $0 ota-send plain --url http://192.168.1.10:8000/firmware.bin
  DEVICE_ID=test-sensor $0 ota-send tls --url https://... --output manifest.json
  $0 all tls
  $0 sub tls

OTA ota-smoke customization:
  DEVICE_ID          Target device id (default: test-sensor)
  OTA_MANIFEST_FILE  Optional manifest JSON file. If unset, sends a dummy manifest-shaped payload.

OTA ota-send options:
  --file FILE        Firmware binary (default: target/ota/firmware.bin)
  --url URL          Download URL for the device (required)
  --version VER      Firmware version (default: git describe --always --dirty)
  --build NUM        Build number (default: git rev-list --count HEAD)
  --seed-hex-file F  Ed25519 signing seed hex file (default: tools/ota/keys/dev_ed25519.seed.hex)
  --output FILE      Save signed manifest JSON to file (does not publish)

OTA ota-serve options:
  --file FILE        Firmware binary to serve (default: target/ota/firmware.bin)
  --port PORT        HTTP port (default: 8000)

TLS customization (mode=tls):
  MQTT_TEST_TLS_SERVER_CN   Certificate Common Name (default: localhost)
  MQTT_TEST_TLS_SERVER_SAN  subjectAltName list (default: DNS:localhost,IP:127.0.0.1)
  MQTT_TEST_TLS_KEY_TYPE    Cert key type: ec or rsa (default: ec)
EOF
}

cleanup() {
    if [[ -n "$BROKER_PID" ]]; then
        kill "$BROKER_PID" 2>/dev/null || true
    fi
    if [[ -n "$CONF_FILE" && -f "$CONF_FILE" ]]; then
        rm -f "$CONF_FILE"
    fi
    if [[ -n "$SERVE_TMPDIR" && -d "$SERVE_TMPDIR" ]]; then
        rm -rf "$SERVE_TMPDIR"
    fi
}
trap cleanup EXIT INT TERM

current_broker_port() {
    if [[ "$MODE" == "tls" ]]; then
        echo "$BROKER_PORT_TLS"
    else
        echo "$BROKER_PORT_PLAIN"
    fi
}

is_local_port_open() {
    local port="$1"
    timeout 1 bash -c "exec 3<>/dev/tcp/127.0.0.1/$port" >/dev/null 2>&1
}

require_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "Error: required command not found: $1" >&2
        exit 1
    fi
}

ensure_tls_assets() {
    case "$TLS_KEY_TYPE" in
        ec|rsa)
            ;;
        *)
            echo "Error: MQTT_TEST_TLS_KEY_TYPE must be 'ec' or 'rsa' (got '$TLS_KEY_TYPE')" >&2
            exit 1
            ;;
    esac

    local desired_tls_params="CN=$TLS_SERVER_CN;SAN=$TLS_SERVER_SAN;KEY=$TLS_KEY_TYPE"

    if [[ -f "$CA_CERT" && -f "$CA_KEY" && -f "$CA_DER" && -f "$SERVER_CERT" && -f "$SERVER_KEY" ]] \
        && [[ -f "$TLS_PARAMS_FILE" ]] \
        && [[ "$(cat "$TLS_PARAMS_FILE")" == "$desired_tls_params" ]]; then
        return
    fi

    require_cmd openssl
    mkdir -p "$TLS_DIR"
    rm -f "$CA_KEY" "$CA_CERT" "$CA_DER" "$SERVER_KEY" "$SERVER_CSR" "$SERVER_CERT" "$SERVER_EXT" "$CA_SRL" "$TLS_PARAMS_FILE"

    echo "Generating local TLS assets in $TLS_DIR (key_type=$TLS_KEY_TYPE) ..."
    if [[ "$TLS_KEY_TYPE" == "ec" ]]; then
        openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -sha256 -days 3650 -nodes \
            -subj "/CN=local-mqtt-test-ca" \
            -keyout "$CA_KEY" \
            -out "$CA_CERT" >/dev/null 2>&1

        openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -sha256 -nodes \
            -subj "/CN=$TLS_SERVER_CN" \
            -keyout "$SERVER_KEY" \
            -out "$SERVER_CSR" >/dev/null 2>&1
    else
        openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
            -subj "/CN=local-mqtt-test-ca" \
            -keyout "$CA_KEY" \
            -out "$CA_CERT" >/dev/null 2>&1

        openssl req -new -newkey rsa:2048 -sha256 -nodes \
            -subj "/CN=$TLS_SERVER_CN" \
            -keyout "$SERVER_KEY" \
            -out "$SERVER_CSR" >/dev/null 2>&1
    fi

    cat >"$SERVER_EXT" <<EOF
subjectAltName = $TLS_SERVER_SAN
keyUsage = digitalSignature,keyEncipherment
extendedKeyUsage = serverAuth
EOF

    openssl x509 -req -sha256 -days 825 \
        -in "$SERVER_CSR" \
        -CA "$CA_CERT" \
        -CAkey "$CA_KEY" \
        -CAcreateserial \
        -out "$SERVER_CERT" \
        -extfile "$SERVER_EXT" >/dev/null 2>&1

    openssl x509 -in "$CA_CERT" -outform DER -out "$CA_DER" >/dev/null 2>&1

    printf '%s\n' "$desired_tls_params" > "$TLS_PARAMS_FILE"
    rm -f "$SERVER_CSR" "$SERVER_EXT" "$CA_SRL"
    chmod 600 "$CA_KEY" "$SERVER_KEY"
    echo "TLS assets ready: $CA_CERT, $CA_DER, $SERVER_CERT, $SERVER_KEY"
}

write_broker_conf() {
    CONF_FILE="$(mktemp)"
    if [[ "$MODE" == "tls" ]]; then
        cat >"$CONF_FILE" <<EOF
listener $BROKER_PORT_TLS 0.0.0.0
allow_anonymous true
max_keepalive 300
certfile $SERVER_CERT
keyfile $SERVER_KEY
require_certificate false
EOF
    else
        cat >"$CONF_FILE" <<EOF
listener $BROKER_PORT_PLAIN 0.0.0.0
allow_anonymous true
max_keepalive 300
EOF
    fi
}

start_broker_background() {
    require_cmd mosquitto
    if [[ "$MODE" == "tls" ]]; then
        ensure_tls_assets
        echo "Starting Mosquitto TLS broker on port $BROKER_PORT_TLS (all interfaces)..."
    else
        echo "Starting Mosquitto broker on port $BROKER_PORT_PLAIN (all interfaces)..."
    fi

    write_broker_conf
    mosquitto -c "$CONF_FILE" -v &
    BROKER_PID=$!
    sleep 1

    if ! kill -0 "$BROKER_PID" 2>/dev/null; then
        echo "Error: broker failed to start" >&2
        exit 1
    fi

    echo "Broker running (PID $BROKER_PID)"
}

ensure_broker_running_for_sub() {
    local port
    port="$(current_broker_port)"
    if is_local_port_open "$port"; then
        echo "Using existing broker on port $port"
        return
    fi

    echo "No broker detected on port $port, starting one automatically..."
    start_broker_background
}

run_subscriber() {
    require_cmd mosquitto_sub
    if [[ "$MODE" == "tls" ]]; then
        ensure_tls_assets
        echo "Subscribing to $TOPIC over TLS on port $BROKER_PORT_TLS ..."
        mosquitto_sub -h localhost -p "$BROKER_PORT_TLS" --cafile "$CA_CERT" -t "$TOPIC" -v -F '%I %t %p'
    else
        echo "Subscribing to $TOPIC on port $BROKER_PORT_PLAIN ..."
        mosquitto_sub -h localhost -p "$BROKER_PORT_PLAIN" -t "$TOPIC" -v -F '%I %t %p'
    fi
}

run_publisher() {
    require_cmd mosquitto_pub
    if [[ "$MODE" == "tls" ]]; then
        ensure_tls_assets
        echo "Publishing test message over TLS ..."
        mosquitto_pub -h localhost -p "$BROKER_PORT_TLS" --cafile "$CA_CERT" \
            -t "sensors/rubicson/1234/temperature" \
            -m '{"id":1234,"ch":1,"temp":22.5,"batt":"ok"}'
    else
        echo "Publishing test message ..."
        mosquitto_pub -h localhost -p "$BROKER_PORT_PLAIN" \
            -t "sensors/rubicson/1234/temperature" \
            -m '{"id":1234,"ch":1,"temp":22.5,"batt":"ok"}'
    fi
    echo "Done"
}

ota_command_topic() {
    printf 'motonet/%s/cmd/ota\n' "$DEVICE_ID"
}

default_ota_manifest() {
    cat <<EOF
{"schema":1,"key_id":1001,"target":"motonet-of-things/esp32","chip":"esp32-wroom","version":"0.0.0-smoke","build":1,"force":false,"url":"http://127.0.0.1:8000/firmware.bin","size":1234567,"sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","signature":"00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000"}
EOF
}

# ---- helper: print LAN URLs for serving ----
suggested_lan_urls() {
    local port="${1:-8000}"
    local filename="${2:-firmware.bin}"
    echo "Suggested LAN URLs:"
    if command -v ip >/dev/null 2>&1; then
        ip -4 addr show scope global 2>/dev/null | grep inet | awk '{print $2}' | cut -d/ -f1 | while read -r ip; do
            echo "  http://$ip:$port/$filename"
        done
    elif command -v hostname >/dev/null 2>&1; then
        hostname -I 2>/dev/null | tr ' ' '\n' | while read -r ip; do
            [[ -n "$ip" ]] && echo "  http://$ip:$port/$filename"
        done
    else
        echo "  (could not detect LAN IPs)"
    fi
}

# ---- ota-smoke ----
run_ota_smoke() {
    require_cmd mosquitto_pub

    local ota_topic
    ota_topic="$(ota_command_topic)"

    local pub_args=(-h localhost)
    if [[ "$MODE" == "tls" ]]; then
        ensure_tls_assets
        echo "Publishing OTA smoke manifest over TLS to $ota_topic ..."
        pub_args+=(-p "$BROKER_PORT_TLS" --cafile "$CA_CERT")
    else
        echo "Publishing OTA smoke manifest to $ota_topic ..."
        pub_args+=(-p "$BROKER_PORT_PLAIN")
    fi

    pub_args+=(-t "$ota_topic")
    if [[ -n "$OTA_MANIFEST_FILE" ]]; then
        pub_args+=(-f "$OTA_MANIFEST_FILE")
    else
        pub_args+=(-m "$(default_ota_manifest)")
    fi

    mosquitto_pub "${pub_args[@]}"
    echo "Done"
}

# ---- ota-build ----
run_ota_build() {
    local project_root
    project_root="$(cd "$SCRIPT_DIR/.." && pwd)"

    require_cmd cargo
    require_cmd espflash

    echo "Building firmware (release)..."
    (cd "$project_root/firmware" && cargo build --release)

    echo "Preparing OTA image directory..."
    mkdir -p "$project_root/target/ota"

    echo "Saving OTA firmware image..."
    espflash save-image --chip esp32 \
        --partition-table "$project_root/firmware/partitions.csv" \
        --target-app-partition ota_0 \
        "$project_root/target/xtensa-esp32-none-elf/release/esp32-rust-project" \
        "$project_root/target/ota/firmware.bin"

    echo "OTA firmware saved to $project_root/target/ota/firmware.bin"
}

# ---- ota-serve ----
run_ota_serve() {
    local file=""
    local port="8000"
    local project_root
    project_root="$(cd "$SCRIPT_DIR/.." && pwd)"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --file) file="$2"; shift 2 ;;
            --port) port="$2"; shift 2 ;;
            *) echo "Unknown option: $1" >&2; exit 1 ;;
        esac
    done

    if [[ -z "$file" ]]; then
        file="$project_root/target/ota/firmware.bin"
    fi

    if [[ ! -f "$file" ]]; then
        echo "Error: firmware file not found: $file" >&2
        echo "Run '$0 ota-build' first or specify --file." >&2
        exit 1
    fi

    require_cmd python3

    SERVE_TMPDIR="$(mktemp -d)"
    cp "$file" "$SERVE_TMPDIR/firmware.bin"

    echo "=============================================="
    echo " Serving OTA firmware over HTTP"
    echo " File:    $file ($(wc -c < "$file") bytes)"
    echo " Port:    $port"
    echo " TempDir: $SERVE_TMPDIR"
    echo "=============================================="
    echo ""
    echo " WARNING: This serves the firmware openly over HTTP."
    echo "          Only use on a trusted local network!"
    echo ""
    suggested_lan_urls "$port" "firmware.bin"
    echo ""
    echo " Press Ctrl+C to stop the server."
    echo "=============================================="

    python3 -m http.server "$port" --directory "$SERVE_TMPDIR" --bind 0.0.0.0
}

# ---- ota-send ----
run_ota_send() {
    local mode="$1"
    shift

    local file=""
    local url=""
    local version=""
    local build_num=""
    local seed_hex_file=""
    local output=""
    local project_root
    project_root="$(cd "$SCRIPT_DIR/.." && pwd)"
    seed_hex_file="$project_root/tools/ota/keys/dev_ed25519.seed.hex"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --file) file="$2"; shift 2 ;;
            --url) url="$2"; shift 2 ;;
            --version) version="$2"; shift 2 ;;
            --build) build_num="$2"; shift 2 ;;
            --seed-hex-file) seed_hex_file="$2"; shift 2 ;;
            --output) output="$2"; shift 2 ;;
            *) echo "Unknown option: $1" >&2; exit 1 ;;
        esac
    done

    # Default file
    if [[ -z "$file" ]]; then
        if [[ -f "$project_root/target/ota/firmware.bin" ]]; then
            file="$project_root/target/ota/firmware.bin"
        else
            echo "Error: --file not specified and $project_root/target/ota/firmware.bin not found" >&2
            echo "Run '$0 ota-build' first or specify --file." >&2
            exit 1
        fi
    fi
    echo "Using firmware file: $file"

    # URL is required
    if [[ -z "$url" ]]; then
        echo "Error: --url is required" >&2
        echo ""
        suggested_lan_urls 8000 "firmware.bin"
        exit 1
    fi

    # Default version
    if [[ -z "$version" ]]; then
        version="$(cd "$project_root" && git describe --always --dirty 2>/dev/null || echo "unknown")"
    fi

    # Default build
    if [[ -z "$build_num" ]]; then
        build_num="$(cd "$project_root" && git rev-list --count HEAD 2>/dev/null || echo "0")"
    fi

    # Compute size and sha256
    local size sha256
    size="$(wc -c < "$file")"
    require_cmd sha256sum
    sha256="$(sha256sum "$file" | cut -d' ' -f1)"

    # Build unsigned JSON
    require_cmd jq
    local unsigned_json
    unsigned_json="$(jq -cn \
        --arg version "$version" \
        --argjson build "$build_num" \
        --arg url "$url" \
        --argjson size "$size" \
        --arg sha256 "$sha256" \
        '{schema:1, key_id:1001, target:"motonet-of-things/esp32", chip:"esp32-wroom", $version, $build, force:false, $url, $size, $sha256}')"

    # Sign with Ed25519 seed
    require_cmd openssl
    require_cmd xxd

    if [[ ! -f "$seed_hex_file" ]]; then
        echo "Error: seed file not found: $seed_hex_file" >&2
        exit 1
    fi

    local tmpkey_pem tmpkey_der
    tmpkey_pem="$(mktemp)"
    tmpkey_der="$(mktemp)"

    local seed_hex
    seed_hex="$(tr -d '[:space:]' < "$seed_hex_file")"

    # PKCS8 DER for Ed25519 private key:
    # 302e (SEQUENCE 46) 020100 (INTEGER 0) 300506032b6570 (SEQUENCE + OID 1.3.101.112)
    # 0422 (OCTET STRING 34) 0420 (OCTET STRING 32) + seed (32 bytes)
    local der_hex="302e020100300506032b657004220420${seed_hex}"
    printf '%s' "$der_hex" | xxd -r -p > "$tmpkey_der"

    if ! openssl pkey -inform DER -in "$tmpkey_der" -out "$tmpkey_pem" 2>/dev/null; then
        echo "Error: failed to parse Ed25519 seed as PKCS8 key" >&2
        rm -f "$tmpkey_pem" "$tmpkey_der"
        exit 1
    fi

    local signature_hex unsigned_file
    unsigned_file="$(mktemp)"
    printf '%s' "$unsigned_json" > "$unsigned_file"
    signature_hex="$(openssl pkeyutl -sign -rawin -in "$unsigned_file" -inkey "$tmpkey_pem" | xxd -p | tr -d '\n')"
    rm -f "$unsigned_file"

    rm -f "$tmpkey_pem" "$tmpkey_der"

    # Build final signed manifest
    local manifest_json
    manifest_json="$(printf '%s' "$unsigned_json" | jq -c --arg sig "$signature_hex" '. + {signature: $sig}')"

    # Save to output file if requested
    if [[ -n "$output" ]]; then
        printf '%s\n' "$manifest_json" > "$output"
        echo "Manifest saved to: $output"
        echo "(Not publishing; use without --output to publish via MQTT)"
        return 0
    fi

    # Publish via MQTT
    require_cmd mosquitto_pub

    local ota_topic pub_args
    ota_topic="$(ota_command_topic)"

    pub_args=(-h localhost)
    if [[ "$mode" == "tls" ]]; then
        ensure_tls_assets
        echo "Publishing OTA manifest over TLS to $ota_topic ..."
        pub_args+=(-p "$BROKER_PORT_TLS" --cafile "$CA_CERT")
    else
        echo "Publishing OTA manifest to $ota_topic ..."
        pub_args+=(-p "$BROKER_PORT_PLAIN")
    fi

    pub_args+=(-t "$ota_topic" -m "$manifest_json" -r)
    mosquitto_pub "${pub_args[@]}"
    echo "Done"
}

COMMAND="${1:-all}"
MODE="${2:-plain}"

if [[ "${1:-}" == "plain" || "${1:-}" == "tls" ]]; then
    COMMAND="all"
    MODE="$1"
fi

if [[ "$COMMAND" == "help" || "$COMMAND" == "--help" || "$COMMAND" == "-h" ]]; then
    usage
    exit 0
fi

case "$COMMAND" in
    broker|sub|pub|ota|ota-smoke|all|ota-build|ota-serve|ota-send)
        ;;
    *)
        usage
        exit 1
        ;;
esac

# Mode validation: skip for commands that don't use MQTT mode
case "$COMMAND" in
    ota-build|ota-serve)
        ;;
    *)
        case "$MODE" in
            plain|tls)
                ;;
            *)
                usage
                exit 1
                ;;
        esac
        ;;
esac

case "$COMMAND" in
    broker)
        start_broker_background
        wait "$BROKER_PID"
        ;;
    sub)
        ensure_broker_running_for_sub
        run_subscriber
        ;;
    pub)
        run_publisher
        ;;
    ota|ota-smoke)
        run_ota_smoke
        ;;
    ota-build)
        run_ota_build
        ;;
    ota-serve)
        run_ota_serve "${@:2}"
        ;;
    ota-send)
        run_ota_send "$MODE" "${@:3}"
        ;;
    all)
        echo "Starting broker in background and subscriber in foreground..."
        echo "Press Ctrl+C to stop"
        echo

        start_broker_background
        echo "Listening on $TOPIC..."
        echo "---"

        run_subscriber
        ;;
esac
