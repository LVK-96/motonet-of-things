#!/bin/bash
# Local OTA test - HTTP server on port 8000, firmware from target/ota-local/
set -euo pipefail
cd /home/leo/git/motonet-of-things

# espflash/monitor can leave the terminal with NL->CRLF output mapping disabled.
# When that happens, every following echo starts where the previous line ended.
reset_tty_output() {
	if [[ -t 1 ]]; then
		stty opost onlcr </dev/tty 2>/dev/null || true
	fi
}

say() {
	reset_tty_output
	printf '\r%s\n' "$*"
}

# Cleanup function
cleanup() {
	set +e
	pkill -f 'espflash monitor.*ttyUSB0' 2>/dev/null || true
	reset_tty_output
	# Don't kill HTTP server - it's shared
}
trap cleanup EXIT

reset_tty_output

# Ensure HTTP server is running
if ! pgrep -f 'python3 -m http.server 8000' >/dev/null 2>&1; then
	cd target/ota-local
	python3 -m http.server 8000 --bind 0.0.0.0 &
	cd /home/leo/git/motonet-of-things
	sleep 1
fi
say "HTTP server: $(pgrep -f 'http.server' | head -1)"

# Flash firmware to ota_1
say "=== Flashing firmware to ota_1 ==="
espflash flash --chip esp32 --port /dev/ttyUSB0 \
	--partition-table firmware/partitions.csv \
	--target-app-partition ota_1 \
	target/xtensa-esp32-none-elf/release/esp32-rust-project 2>&1 | tail -3

reset_tty_output

# Start serial monitor
LOG="target/ota-stackfix-test/serial-local-http-$(date +%Y%m%d-%H%M%S).log"
say "=== Starting serial monitor ==="
espflash monitor --chip esp32 --port /dev/ttyUSB0 --non-interactive \
	--log-format defmt \
	--elf target/xtensa-esp32-none-elf/release/esp32-rust-project \
	>"$LOG" 2>/dev/null </dev/null &
MON_PID=$!
say "Monitor PID: $MON_PID, Log: $LOG"

# Wait for MQTT: Ready
say "=== Waiting for device to boot and connect ==="
for i in $(seq 1 80); do
	sleep 2
	if strings "$LOG" 2>/dev/null | rg -q 'MQTT: Ready'; then
		say "MQTT Ready at $((i * 2))s"
		break
	fi
done

# Helper: extract readable message from log lines (handles both defmt and ESP-IDF formats)
fmt_log() {
	sed -E 's/^[[:space:]]*//' |
		sed -E 's/^[0-9]+\.[0-9]+ \[[A-Z]+[[:space:]]*\] //' |
		sed -E 's/^\[[0-9T:-]+Z?[[:space:]]+[A-Z]+[[:space:]]*\] //' |
		sed -E 's/^I \([0-9]+\) //' |
		sed -E 's/ \([^)]*:[0-9]+\)$//' |
		head -n "${1:-80}"
}

# Prepare MQTT CA
openssl x509 -inform DER -in firmware/mosquitto-ca.der -out /tmp/mqtt-ca.pem 2>/dev/null

# Clear any retained command first
mosquitto_pub -q 1 -r -n -i "pi-preclear-$$" \
	-h "mqtt.home.kivikunnas.xyz" -p 8883 \
	--cafile /tmp/mqtt-ca.pem \
	-u "sensor-node" -P "J4V52494sFXEDwbmcbHZ97ou" \
	-t "motonet/test-sensor/cmd/ota" 2>/dev/null || true

sleep 2

# Publish OTA manifest
say "=== Publishing OTA manifest ==="
mosquitto_pub -q 1 -r \
	-f "target/ota-stackfix-test/firmware.manifest.json" \
	-i "pi-ota-$$" \
	-h "mqtt.home.kivikunnas.xyz" -p 8883 \
	--cafile /tmp/mqtt-ca.pem \
	-u "sensor-node" -P "J4V52494sFXEDwbmcbHZ97ou" \
	-t "motonet/test-sensor/cmd/ota" 2>&1

say "Manifest published. Waiting for OTA..."
# Wait for OTA result
status=timeout
deadline=$((SECONDS + 360))
while ((SECONDS < deadline)); do
	text=$(strings "$LOG" 2>/dev/null || true)
	if printf '%s\n' "$text" | rg -q 'PANIC|Guru|Failed to allocate'; then
		status=panic
		break
	fi
	if printf '%s\n' "$text" | rg -q 'OTA confirm:.*valid|MQTT: OTA confirmation published|Loaded app from partition at offset 0x10000'; then
		status=success
		break
	fi
	if printf '%s\n' "$text" | rg -q 'OTA: flash write failed|OutOfMemory|manifest rejected|SignatureRejected|HttpConnect'; then
		status=failure_seen
		break
	fi
	sleep 3
done

say ""
say "========== RESULT =========="
say "test_status=$status"
say "==========================="
say ""
say "--- Key events ---"
reset_tty_output
strings "$LOG" | rg 'StackGuard|MQTT: Ready|OTA confirm|OTA:|downloading|flash|write|erase|chunk|redirect|HTTP|TLS|error|failed|OutOfMemory|valid|signature|manifest|sig|alloc' | fmt_log 80

# Keep monitor running for a bit more to catch post-OTA boot
if [[ "$status" == "success" ]]; then
	say ""
	say "Waiting for post-OTA boot..."
	sleep 30
	say "--- Post-OTA boot ---"
	reset_tty_output
	strings "$LOG" | rg 'StackGuard|MQTT: Ready|OTA confirm|Loaded app' | fmt_log 10
fi
