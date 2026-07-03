#!/bin/bash
# Trigger OTA on a device by publishing a release manifest via MQTT
# Prerequisites: mosquitto_pub, jq, curl

set -euo pipefail
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

DEVICE_ID="${DEVICE_ID:-esp32-rubicson}"
GITHUB_REPOSITORY="${GITHUB_REPOSITORY:-}"
MQTT_HOST="${MQTT_HOST:-}"
MQTT_PORT="${MQTT_PORT:-8883}"
MQTT_USERNAME="${MQTT_USERNAME:-}"
MQTT_PASSWORD="${MQTT_PASSWORD:-}"
MQTT_CAFILE="${MQTT_CAFILE:-}"
CLEAR_RETAINED=false

release_tag=""
manifest_url=""
repo_arg=""

usage() {
    cat <<'EOF'
Usage: ota-trigger.sh [OPTIONS]

Trigger OTA on a device by publishing a release manifest via MQTT.

Options:
  --release TAG        GitHub release tag (e.g. v0.1.5)
  --repo OWNER/REPO    GitHub repository (default: from git remote)
  --manifest-url URL   Direct URL to firmware.manifest.json
  --device-id ID       MQTT device topic suffix (default: esp32-rubicson)

MQTT (or set via env vars):
  --mqtt-host HOST     MQTT broker hostname
  --mqtt-port PORT     MQTT broker port (default: 8883)
  --mqtt-username USER
  --mqtt-password PASS
  --mqtt-cafile FILE   CA certificate file (PEM)
  --clear-retained     Clear existing retained command before publishing

Examples:
  ota-trigger.sh --release v0.1.5 --device-id esp32-rubicson
  ota-trigger.sh --release v0.1.5 --mqtt-host mqtt.example.com
EOF
    exit 0
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --release) release_tag="$2"; shift 2 ;;
        --repo) repo_arg="$2"; shift 2 ;;
        --manifest-url) manifest_url="$2"; shift 2 ;;
        --device-id) DEVICE_ID="$2"; shift 2 ;;
        --mqtt-host) MQTT_HOST="$2"; shift 2 ;;
        --mqtt-port) MQTT_PORT="$2"; shift 2 ;;
        --mqtt-username) MQTT_USERNAME="$2"; shift 2 ;;
        --mqtt-password) MQTT_PASSWORD="$2"; shift 2 ;;
        --mqtt-cafile) MQTT_CAFILE="$2"; shift 2 ;;
        --clear-retained) CLEAR_RETAINED=true; shift ;;
        -h|--help) usage ;;
        *) echo "Error: unknown option: $1" >&2; usage >&2; exit 1 ;;
    esac
done

require_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "Error: required command not found: $1" >&2
        exit 1
    fi
}
require_cmd curl
require_cmd jq
require_cmd mosquitto_pub

if [[ -z "$manifest_url" && -z "$release_tag" ]]; then
    echo "Error: specify --release or --manifest-url" >&2
    usage >&2
    exit 1
fi

if [[ -n "$release_tag" ]]; then
    # Infer repo from git remote if not provided
    if [[ -z "$repo_arg" ]]; then
        if [[ -n "$GITHUB_REPOSITORY" ]]; then
            repo_arg="$GITHUB_REPOSITORY"
        else
            remote_url="$(git -C "$REPO_ROOT" config --get remote.origin.url 2>/dev/null || true)"
            if [[ -z "$remote_url" ]]; then
                echo "Error: cannot infer GitHub repository. Set --repo OWNER/REPO or GITHUB_REPOSITORY env." >&2
                exit 1
            fi
            repo_arg="$(echo "$remote_url" | sed -E 's|.*github\.com[/:]||; s|\.git$||')"
        fi
    fi
    manifest_url="https://github.com/${repo_arg}/releases/download/${release_tag}/firmware.manifest.json"
fi

if [[ -z "$MQTT_HOST" ]]; then
    echo "Error: MQTT host not set. Use --mqtt-host or MQTT_HOST env." >&2
    exit 1
fi

manifest_file="$(mktemp)"
trap 'rm -f "$manifest_file"' EXIT

echo "=== Downloading manifest ==="
echo "  URL: $manifest_url"
curl -sfL -o "$manifest_file" "$manifest_url" || {
    echo "Error: failed to download manifest from $manifest_url" >&2
    exit 1
}

# Validate it's a valid JSON manifest
if ! jq -e '.schema and .version and .url and .signature' "$manifest_file" >/dev/null 2>&1; then
    echo "Error: downloaded file is not a valid OTA manifest (missing required fields)" >&2
    jq '.' "$manifest_file" 2>/dev/null || echo "(not valid JSON)"
    exit 1
fi

manifest_version="$(jq -r '.version // "?"' "$manifest_file")"
manifest_build="$(jq -r '.build // "?"' "$manifest_file")"
manifest_image_url="$(jq -r '.url // "?"' "$manifest_file")"
echo "  Version: $manifest_version (build $manifest_build)"
echo "  Image URL: $manifest_image_url"
echo "  Manifest OK"

topic="motonet/${DEVICE_ID}/cmd/ota"
echo ""
echo "=== Target device ==="
echo "  Device ID: $DEVICE_ID"
echo "  MQTT topic: $topic"
echo "  MQTT broker: $MQTT_HOST:$MQTT_PORT"

mqtt_args=(
    -h "$MQTT_HOST"
    -p "$MQTT_PORT"
    -q 1
    -r
    -i "ota-trigger-$$"
    -u "$MQTT_USERNAME"
    -P "$MQTT_PASSWORD"
    -t "$topic"
)

if [[ -n "$MQTT_CAFILE" ]]; then
    if [[ ! -f "$MQTT_CAFILE" ]]; then
        echo "Error: CA file not found: $MQTT_CAFILE" >&2
        exit 1
    fi
    mqtt_args+=(--cafile "$MQTT_CAFILE")
fi

if [[ "$CLEAR_RETAINED" == true ]]; then
    echo ""
    echo "=== Clearing retained command ==="
    mosquitto_pub -n "${mqtt_args[@]}" 2>&1 || true
    sleep 1
fi

echo ""
echo "=== Publishing OTA manifest ==="
mosquitto_pub -f "$manifest_file" "${mqtt_args[@]}"

echo "  Published to $topic"
echo ""
echo "============================================"
echo " OTA Triggered"
echo "============================================"
echo " Release:    ${release_tag:-<direct URL>}"
echo " Device:     $DEVICE_ID"
echo " Version:    $manifest_version (build $manifest_build)"
echo " Topic:      $topic"
echo "============================================"
echo ""
echo "The device should pick up the manifest and apply the update."
echo "Monitor status on: motonet/${DEVICE_ID}/ota/status"
EOF
