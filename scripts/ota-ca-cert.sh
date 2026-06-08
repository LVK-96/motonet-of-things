#!/usr/bin/env bash
# Fetch and export a TLS CA/trust-anchor certificate for OTA HTTPS downloads.
#
# Typical use:
#   scripts/ota-ca-cert.sh list github.com
#   scripts/ota-ca-cert.sh rust github.com > /tmp/ota_ca.rs
#   scripts/ota-ca-cert.sh der release-assets.githubusercontent.com --out ota-ca.der

set -euo pipefail

usage() {
    cat <<'EOF'
Usage:
  scripts/ota-ca-cert.sh list HOST [--port PORT]
  scripts/ota-ca-cert.sh rust HOST [--port PORT] [--anchor auto|system-root|chain-last|index:N] [--const NAME]
  scripts/ota-ca-cert.sh der  HOST [--port PORT] [--anchor auto|system-root|chain-last|index:N] --out FILE
  scripts/ota-ca-cert.sh pem  HOST [--port PORT] [--anchor auto|system-root|chain-last|index:N] --out FILE

Commands:
  list   Print the server-provided certificate chain.
  rust   Print a Rust byte-slice const for firmware/src/secrets.rs.
  der    Write selected certificate in DER format.
  pem    Write selected certificate in PEM format.

Anchor selection:
  auto         Use the system trust root that issued the top server-chain cert;
               fall back to the top server-chain cert if no system root is found.
  system-root  Require finding that issuing root in the local system CA bundle.
  chain-last   Use the last certificate sent by the server.
  index:N      Use certificate N from the server chain (1 = leaf).

Environment:
  SSL_CERT_FILE  Optional CA bundle path used for system-root lookup.

Examples:
  scripts/ota-ca-cert.sh list github.com
  scripts/ota-ca-cert.sh rust github.com --const OTA_TLS_CA_CERT_DER
  scripts/ota-ca-cert.sh rust release-assets.githubusercontent.com

Note:
  GitHub may use different CA chains for github.com and release asset hosts.
  The firmware currently has a single OTA_TLS_CA_CERT_DER, so choose the CA for
  the host that the device will actually connect to, or extend firmware to trust
  multiple OTA CAs.
EOF
}

require_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "Error: required command not found: $1" >&2
        exit 1
    fi
}

find_ca_bundle() {
    if [[ -n "${SSL_CERT_FILE:-}" && -f "${SSL_CERT_FILE:-}" ]]; then
        printf '%s\n' "$SSL_CERT_FILE"
        return 0
    fi

    local candidates=(
        /etc/ssl/certs/ca-certificates.crt
        /etc/pki/tls/certs/ca-bundle.crt
        /etc/ssl/cert.pem
    )
    local path
    for path in "${candidates[@]}"; do
        if [[ -f "$path" ]]; then
            printf '%s\n' "$path"
            return 0
        fi
    done
    return 1
}

fetch_chain() {
    local host="$1"
    local port="$2"
    local dir="$3"

    openssl s_client \
        -connect "$host:$port" \
        -servername "$host" \
        -showcerts </dev/null 2>/dev/null \
        | awk -v dir="$dir" '
            /BEGIN CERTIFICATE/ { i++; file=sprintf("%s/cert%d.pem", dir, i) }
            file != "" { print > file }
            /END CERTIFICATE/ { file="" }
        '

    compgen -G "$dir/cert*.pem" >/dev/null || {
        echo "Error: no certificates fetched for $host:$port" >&2
        exit 1
    }
}

chain_count() {
    local dir="$1"
    find "$dir" -maxdepth 1 -name 'cert*.pem' -printf '%f\n' \
        | sed -E 's/cert([0-9]+)\.pem/\1/' \
        | sort -n \
        | tail -1
}

cert_name() {
    local file="$1"
    local field="$2"
    openssl x509 -in "$file" -noout -"$field" -nameopt RFC2253 | sed "s/^$field=//"
}

print_cert_summary() {
    local file="$1"
    local idx="$2"
    echo "[$idx] $(cert_name "$file" subject)"
    echo "    issuer:  $(cert_name "$file" issuer)"
    echo "    sha256:  $(openssl x509 -in "$file" -noout -fingerprint -sha256 | sed 's/^sha256 Fingerprint=//')"
    echo "    dates:   $(openssl x509 -in "$file" -noout -dates | tr '\n' ' ')"
    local san
    san="$(openssl x509 -in "$file" -noout -ext subjectAltName 2>/dev/null | sed '1d' | tr '\n' ' ' | sed 's/^ *//;s/ *$//' || true)"
    if [[ -n "$san" ]]; then
        echo "    san:     $san"
    fi
}

split_ca_bundle() {
    local bundle="$1"
    local dir="$2"
    awk -v dir="$dir" '
        /BEGIN CERTIFICATE/ { i++; file=sprintf("%s/ca%d.pem", dir, i) }
        file != "" { print > file }
        /END CERTIFICATE/ { file="" }
    ' "$bundle"
}

find_system_root_for_chain() {
    local top_chain_cert="$1"
    local workdir="$2"
    local bundle
    bundle="$(find_ca_bundle)" || return 1

    local issuer
    issuer="$(cert_name "$top_chain_cert" issuer)"

    local ca_dir="$workdir/system-cas"
    mkdir -p "$ca_dir"
    split_ca_bundle "$bundle" "$ca_dir"

    local ca subject
    while IFS= read -r -d '' ca; do
        subject="$(cert_name "$ca" subject || true)"
        if [[ "$subject" == "$issuer" ]]; then
            printf '%s\n' "$ca"
            return 0
        fi
    done < <(find "$ca_dir" -maxdepth 1 -name 'ca*.pem' -print0)

    return 1
}

select_anchor() {
    local anchor="$1"
    local chain_dir="$2"
    local workdir="$3"
    local count="$4"

    case "$anchor" in
        index:*)
            local idx="${anchor#index:}"
            if [[ ! "$idx" =~ ^[0-9]+$ || "$idx" -lt 1 || "$idx" -gt "$count" ]]; then
                echo "Error: invalid anchor index '$idx' (chain has $count certs)" >&2
                exit 1
            fi
            printf '%s\n' "$chain_dir/cert$idx.pem"
            ;;
        chain-last)
            printf '%s\n' "$chain_dir/cert$count.pem"
            ;;
        system-root)
            find_system_root_for_chain "$chain_dir/cert$count.pem" "$workdir" || {
                echo "Error: could not find issuing root in system CA bundle" >&2
                exit 1
            }
            ;;
        auto)
            find_system_root_for_chain "$chain_dir/cert$count.pem" "$workdir" || {
                echo "Warning: could not find issuing root in system CA bundle; using server chain cert $count" >&2
                printf '%s\n' "$chain_dir/cert$count.pem"
            }
            ;;
        *)
            echo "Error: unknown anchor '$anchor'" >&2
            exit 1
            ;;
    esac
}

emit_rust_const() {
    local pem="$1"
    local const_name="$2"
    local host="$3"
    local anchor="$4"
    local der="$5"

    openssl x509 -in "$pem" -outform DER -out "$der"
    {
        echo "// Generated by scripts/ota-ca-cert.sh rust $host --anchor $anchor"
        echo "// subject: $(cert_name "$pem" subject)"
        echo "// issuer:  $(cert_name "$pem" issuer)"
        printf 'pub const %s: &[u8] = &[\n' "$const_name"
        od -An -tx1 -v "$der" | awk '
            { for (i = 1; i <= NF; i++) {
                printf "0x%s, ", $i;
                n++;
                if (n % 12 == 0) printf "\n";
            }}
            END { if (n % 12 != 0) printf "\n"; }
        '
        echo "];"
    }
}

main() {
    require_cmd openssl
    require_cmd awk
    require_cmd od

    if [[ $# -lt 2 || "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
        usage
        exit 0
    fi

    local command="$1"
    local host="$2"
    shift 2

    local port="443"
    local anchor="auto"
    local const_name="OTA_TLS_CA_CERT_DER"
    local out=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --port) port="$2"; shift 2 ;;
            --anchor) anchor="$2"; shift 2 ;;
            --const) const_name="$2"; shift 2 ;;
            --out) out="$2"; shift 2 ;;
            *) echo "Error: unknown option: $1" >&2; usage >&2; exit 1 ;;
        esac
    done

    local tmp
    tmp="$(mktemp -d)"
    trap "rm -rf '$tmp'" EXIT

    local chain_dir="$tmp/chain"
    mkdir -p "$chain_dir"
    fetch_chain "$host" "$port" "$chain_dir"

    local count
    count="$(chain_count "$chain_dir")"

    case "$command" in
        list)
            echo "Certificate chain for $host:$port ($count certs):"
            local i
            for ((i = 1; i <= count; i++)); do
                print_cert_summary "$chain_dir/cert$i.pem" "$i"
            done
            ;;
        rust|der|pem)
            local selected
            selected="$(select_anchor "$anchor" "$chain_dir" "$tmp" "$count")"

            case "$command" in
                rust)
                    emit_rust_const "$selected" "$const_name" "$host" "$anchor" "$tmp/selected.der"
                    ;;
                der)
                    [[ -n "$out" ]] || { echo "Error: der requires --out FILE" >&2; exit 1; }
                    openssl x509 -in "$selected" -outform DER -out "$out"
                    echo "Wrote DER certificate to $out" >&2
                    ;;
                pem)
                    [[ -n "$out" ]] || { echo "Error: pem requires --out FILE" >&2; exit 1; }
                    cp "$selected" "$out"
                    echo "Wrote PEM certificate to $out" >&2
                    ;;
            esac
            ;;
        *)
            echo "Error: unknown command: $command" >&2
            usage >&2
            exit 1
            ;;
    esac
}

main "$@"
