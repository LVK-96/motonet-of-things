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

usage() {
    cat <<EOF
Usage:
  $0 [broker|sub|pub|ota|all] [plain|tls]
  $0 [plain|tls]

Commands:
  broker  Start broker only
  sub     Start subscriber (auto-starts broker if needed)
  pub     Publish a telemetry test message
  ota     Publish an OTA smoke-test manifest to motonet/\$DEVICE_ID/cmd/ota
  all     Start broker + subscriber (default)

Modes:
  plain   Use plaintext MQTT on port $BROKER_PORT_PLAIN (default)
  tls     Use server-TLS-only MQTT on port $BROKER_PORT_TLS (local CA + server cert)

Examples:
  $0
  $0 broker
  $0 pub plain
  $0 ota plain
  DEVICE_ID=test-sensor $0 ota tls
  OTA_MANIFEST_FILE=manifest.json $0 ota plain
  $0 all tls
  $0 sub tls

OTA smoke-test customization:
  DEVICE_ID          Target device id (default: test-sensor)
  OTA_MANIFEST_FILE  Optional manifest JSON file. If unset, sends a dummy manifest-shaped payload.

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

run_ota_publisher() {
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
    broker|sub|pub|ota|all)
        ;;
    *)
        usage
        exit 1
        ;;
esac

case "$MODE" in
    plain|tls)
        ;;
    *)
        usage
        exit 1
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
    ota)
        run_ota_publisher
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
