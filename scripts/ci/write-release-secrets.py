#!/usr/bin/env python3
import base64
import binascii
import ipaddress
import json
import os
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FIRMWARE = ROOT / "firmware"
SECRETS_RS = FIRMWARE / "src" / "secrets.rs"
OTA_CA = FIRMWARE / "ota-tls-ca.der"
MQTT_CA = FIRMWARE / "mqtt-tls-ca.der"

PLACEHOLDERS = {
    "YOUR_WIFI_SSID",
    "YOUR_WIFI_PASSWORD",
    "MQTT_PASSWORD_PLACEHOLDER",
}


def die(message: str) -> None:
    raise SystemExit(f"error: {message}")


def reject_placeholder(name: str, value: str) -> None:
    if value in PLACEHOLDERS or "PLACEHOLDER" in value:
        die(f"{name} contains a placeholder value")


def reject_control_chars(name: str, value: str) -> None:
    if any(ord(ch) < 0x20 for ch in value):
        die(f"{name} must be a single-line string without control characters")


def env_required(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        die(f"missing required environment variable {name}")
    reject_placeholder(name, value)
    reject_control_chars(name, value)
    return value


def env_default(name: str, default: str) -> str:
    value = os.environ.get(name) or default
    reject_placeholder(name, value)
    reject_control_chars(name, value)
    return value


def env_optional(name: str) -> str:
    value = os.environ.get(name, "")
    if value:
        reject_placeholder(name, value)
        reject_control_chars(name, value)
    return value


def rust_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def parse_bool(name: str, default: str = "false") -> bool:
    value = os.environ.get(name, default).strip().lower()
    if value in {"1", "true", "yes", "on"}:
        return True
    if value in {"0", "false", "no", "off"}:
        return False
    die(f"{name} must be true or false")


def parse_port(name: str, default: str) -> int:
    raw = os.environ.get(name, default).strip()
    if not re.fullmatch(r"[0-9]+", raw):
        die(f"{name} must be a TCP port number")
    port = int(raw)
    if not 1 <= port <= 65535:
        die(f"{name} must be between 1 and 65535")
    return port


def parse_uint(name: str, default: str, min_value: int, max_value: int) -> int:
    raw = os.environ.get(name, default).strip()
    if not re.fullmatch(r"[0-9]+", raw):
        die(f"{name} must be an integer")
    value = int(raw)
    if not min_value <= value <= max_value:
        die(f"{name} must be between {min_value} and {max_value}")
    return value


def parse_ipv4(name: str) -> str:
    raw = env_required(name).strip()
    try:
        ip = ipaddress.IPv4Address(raw)
    except ValueError as exc:
        die(f"{name} must be an IPv4 address: {exc}")
    return f"[{', '.join(str(part) for part in ip.packed)}]"


def parse_optional_ipv4(name: str) -> str:
    raw = env_optional(name).strip()
    if not raw:
        return "None"
    try:
        ip = ipaddress.IPv4Address(raw)
    except ValueError as exc:
        die(f"{name} must be an IPv4 address: {exc}")
    return f"Some([{', '.join(str(part) for part in ip.packed)}])"


def parse_optional_channel() -> str:
    raw = env_optional("WIFI_CHANNEL_HINT").strip()
    if not raw:
        return "None"
    if not re.fullmatch(r"[0-9]+", raw):
        die("WIFI_CHANNEL_HINT must be a Wi-Fi channel number")
    channel = int(raw)
    if not 1 <= channel <= 14:
        die("WIFI_CHANNEL_HINT must be between 1 and 14")
    return f"Some({channel})"


def parse_optional_bssid() -> str:
    raw = env_optional("WIFI_BSSID_HINT").strip()
    if not raw:
        return "None"
    compact = re.sub(r"[:-]", "", raw)
    if not re.fullmatch(r"[0-9a-fA-F]{12}", compact):
        die("WIFI_BSSID_HINT must be 6 hex bytes, e.g. aa:bb:cc:dd:ee:ff")
    parts = [f"0x{compact[i:i + 2].lower()}" for i in range(0, 12, 2)]
    return f"Some([{', '.join(parts)}])"


def parse_optional_str(name: str, default: str | None = None) -> str:
    raw = os.environ.get(name, "" if default is None else default)
    if raw.strip().lower() in {"", "none", "null"}:
        return "None"
    reject_placeholder(name, raw)
    reject_control_chars(name, raw)
    return f"Some({rust_string(raw)})"


def parse_hex_key(name: str) -> str:
    compact = re.sub(r"\s+", "", os.environ.get(name, ""))
    if not compact:
        die(f"missing required environment variable {name}")
    if not re.fullmatch(r"[0-9a-fA-F]{64}", compact):
        die(f"{name} must be exactly 32 bytes encoded as 64 hex characters")
    bytes_literal = [f"0x{compact[i:i + 2].lower()}" for i in range(0, 64, 2)]
    lines = [
        "    " + ", ".join(bytes_literal[start:start + 16]) + ","
        for start in range(0, len(bytes_literal), 16)
    ]
    return "[\n" + "\n".join(lines) + "\n]"


def decode_required_b64(name: str, output: Path) -> None:
    compact = re.sub(r"\s+", "", os.environ.get(name, ""))
    if not compact:
        die(f"missing required environment variable {name}")
    try:
        data = base64.b64decode(compact, validate=True)
    except binascii.Error as exc:
        die(f"{name} must be valid base64: {exc}")
    if not data:
        die(f"{name} decoded to an empty certificate")
    output.write_bytes(data)


def decode_optional_b64(name: str, output: Path) -> bool:
    compact = re.sub(r"\s+", "", os.environ.get(name, ""))
    if not compact:
        return False
    try:
        data = base64.b64decode(compact, validate=True)
    except binascii.Error as exc:
        die(f"{name} must be valid base64: {exc}")
    if not data:
        die(f"{name} decoded to an empty certificate")
    output.write_bytes(data)
    return True


def main() -> None:
    wifi_ssid = env_required("WIFI_SSID")
    wifi_password = env_required("WIFI_PASSWORD")
    device_id = env_default("DEVICE_ID", "esp32-rubicson")
    mqtt_broker_ip = parse_ipv4("MQTT_BROKER_IP")
    mqtt_broker_port = parse_port("MQTT_BROKER_PORT", "1883")
    mqtt_client_id = env_default("MQTT_CLIENT_ID", device_id)
    mqtt_username = parse_optional_str("MQTT_USERNAME", "sensor-node")
    mqtt_password = env_required("MQTT_PASSWORD")
    mqtt_use_tls = parse_bool("MQTT_USE_TLS", "false")
    mqtt_broker_hostname = env_required("MQTT_BROKER_HOSTNAME")
    ntp_servers = env_default(
        "NTP_SERVER_IPV4_LIST",
        "216.239.35.0,216.239.35.4,216.239.35.8,216.239.35.12",
    )
    wifi_subnet_prefix = parse_uint("WIFI_SUBNET_PREFIX", "24", 0, 32)
    ota_master_key = parse_hex_key("OTA_ENCRYPTION_MASTER_KEY_HEX")
    ota_key_id = parse_uint("OTA_ENCRYPTION_KEY_ID", "1", 0, 4_294_967_295)

    FIRMWARE.mkdir(parents=True, exist_ok=True)
    decode_required_b64("OTA_TLS_CA_CERT_DER_B64", OTA_CA)

    if mqtt_use_tls:
        if not decode_optional_b64("MQTT_TLS_CA_CERT_DER_B64", MQTT_CA):
            die("MQTT_TLS_CA_CERT_DER_B64 is required when MQTT_USE_TLS is true")
        mqtt_ca = 'include_bytes!("../mqtt-tls-ca.der")'
    else:
        mqtt_ca = "&[]"

    SECRETS_RS.write_text(
        f'''// Generated by scripts/ci/write-release-secrets.py for release CI.
// Do not edit in CI; change GitHub environment secrets/vars instead.

// WiFi credentials
pub const WIFI_SSID: &str = {rust_string(wifi_ssid)};
pub const WIFI_PASSWORD: &str = {rust_string(wifi_password)};

// Optional WiFi reconnect hints (use None to disable)
pub const WIFI_CHANNEL_HINT: Option<u8> = {parse_optional_channel()};
pub const WIFI_BSSID_HINT: Option<[u8; 6]> = {parse_optional_bssid()};

// Optional static IPv4 config for faster reconnect (set to None to use DHCP)
pub const WIFI_STATIC_IP: Option<[u8; 4]> = {parse_optional_ipv4("WIFI_STATIC_IP")};
pub const WIFI_SUBNET_PREFIX: u8 = {wifi_subnet_prefix};
pub const WIFI_GATEWAY_IP: Option<[u8; 4]> = {parse_optional_ipv4("WIFI_GATEWAY_IP")};
pub const WIFI_DNS1_IP: Option<[u8; 4]> = {parse_optional_ipv4("WIFI_DNS1_IP")};
pub const WIFI_DNS2_IP: Option<[u8; 4]> = {parse_optional_ipv4("WIFI_DNS2_IP")};

// Optional comma-separated NTP IPv4 list for failover.
pub const NTP_SERVER_IPV4_LIST: &str = {rust_string(ntp_servers)};

// MQTT broker/device configuration
pub const DEVICE_ID: &str = {rust_string(device_id)};
pub const MQTT_BROKER_IP: [u8; 4] = {mqtt_broker_ip};
pub const MQTT_BROKER_PORT: u16 = {mqtt_broker_port};
pub const MQTT_CLIENT_ID: &str = {rust_string(mqtt_client_id)};
pub const MQTT_USERNAME: Option<&str> = {mqtt_username};
pub const MQTT_PASSWORD: Option<&str> = Some({rust_string(mqtt_password)});

// MQTT over TLS (server authentication only)
pub const MQTT_USE_TLS: bool = {str(mqtt_use_tls).lower()};
pub const MQTT_BROKER_HOSTNAME: &str = {rust_string(mqtt_broker_hostname)};
pub const MQTT_TLS_CA_CERT_DER: &[u8] = {mqtt_ca};
// Used only when wall clock is not synchronized yet.
// 2025-01-01 00:00:00 UTC
pub const MQTT_TLS_FALLBACK_UNIX_TIME_SECS: u64 = 1_735_689_600;

// OTA HTTPS CA certificate (DER format) for verifying release asset hosts.
pub const OTA_TLS_CA_CERT_DER: &[u8] = include_bytes!("../ota-tls-ca.der");

// Release firmware must verify the OTA HTTPS server certificate.
pub const OTA_TLS_ALLOW_INVALID_CA: bool = false;

// Master key for v2 encrypted OTA (32 raw bytes).
pub const OTA_ENCRYPTION_KEY_ID: u32 = {ota_key_id};
pub const OTA_ENCRYPTION_MASTER_KEY: [u8; 32] = {ota_master_key};
''',
        encoding="utf-8",
    )
    print("Generated firmware/src/secrets.rs for release CI")


if __name__ == "__main__":
    main()
