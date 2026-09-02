#!/bin/sh
# Install IO Gateway from a GitHub Release into the current user's home directory.
#
# This script intentionally never uses sudo. It is suitable for Linux and macOS
# desktops with either x86_64 or ARM64 CPUs.

set -eu

REPOSITORY="${IO_GATEWAY_REPOSITORY:-giofahreza/io-gateway}"
VERSION="${IO_GATEWAY_VERSION:-latest}"
NO_START=0
TMP_DIR=""

usage() {
    printf '%s\n' \
        'Usage: install.sh [--version <tag>] [--no-start]' \
        '' \
        'Installs the matching IO Gateway GitHub Release for this computer.' \
        '' \
        'Options:' \
        '  --version <tag>  Install a release tag such as v0.1.18 (or 0.1.18).' \
        '  --no-start       Install only; do not create/start a user service.' \
        '  --help           Show this help.' \
        '' \
        'Environment overrides:' \
        '  IO_GATEWAY_VERSION      Same as --version.' \
        '  IO_GATEWAY_BIN_DIR      Directory for io-gateway and iogw.' \
        '  IO_GATEWAY_CONFIG       Exact path for config.json.' \
        '  IO_GATEWAY_CONFIG_DIR   Config directory when IO_GATEWAY_CONFIG is unset.' \
        '  IO_GATEWAY_REPOSITORY   GitHub owner/repository (advanced use).'
}

note() {
    printf '%s\n' "io-gateway installer: $*"
}

warn() {
    printf '%s\n' "io-gateway installer: warning: $*" >&2
}

die() {
    printf '%s\n' "io-gateway installer: error: $*" >&2
    exit 1
}

cleanup() {
    if [ -n "${TMP_DIR:-}" ] && [ -d "$TMP_DIR" ]; then
        rm -rf "$TMP_DIR"
    fi
}

trap 'cleanup' 0
trap 'cleanup; exit 1' 1 2 3 15

download() {
    url=$1
    destination=$2

    if command -v curl >/dev/null 2>&1; then
        curl --fail --location --silent --show-error --retry 3 --connect-timeout 15 \
            --output "$destination" "$url"
    elif command -v wget >/dev/null 2>&1; then
        wget --quiet --tries=3 --timeout=30 --output-document="$destination" "$url"
    else
        die 'curl or wget is required to download a release.'
    fi
}

sha256_file() {
    file=$1

    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$file" | awk '{print $1}'
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 "$file" | awk '{print $NF}'
    else
        die 'sha256sum, shasum, or openssl is required to verify the release.'
    fi
}

random_hex() {
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex 32
    elif [ -r /dev/urandom ] && command -v od >/dev/null 2>&1; then
        od -An -N32 -tx1 /dev/urandom | tr -d ' \n'
    else
        die 'openssl or /dev/urandom with od is required to create the initial API key.'
    fi
}

systemd_escape_argument() {
    # systemd permits double-quoted arguments. Escape the two characters that
    # would change their meaning inside that quoted value.
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

xml_escape() {
    printf '%s' "$1" | sed \
        -e 's/&/\&amp;/g' \
        -e 's/</\&lt;/g' \
        -e 's/>/\&gt;/g' \
        -e 's/"/\&quot;/g' \
        -e "s/'/\&apos;/g"
}

# A service manager accepting a unit does not prove that the process could
# bind its port and stay alive. Fresh installs always use this local endpoint;
# wait for it before telling the user that onboarding has started successfully.
wait_for_gateway_health() {
    health_attempt=0
    while [ "$health_attempt" -lt 20 ]; do
        if command -v curl >/dev/null 2>&1; then
            if curl --fail --silent --show-error --max-time 2 \
                http://127.0.0.1:8319/health >/dev/null 2>&1; then
                return 0
            fi
        elif command -v wget >/dev/null 2>&1; then
            if wget --quiet --timeout=2 --output-document=/dev/null \
                http://127.0.0.1:8319/health >/dev/null 2>&1; then
                return 0
            fi
        else
            return 1
        fi

        health_attempt=$((health_attempt + 1))
        if [ "$health_attempt" -lt 20 ]; then
            sleep 1
        fi
    done

    return 1
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            [ "$#" -ge 2 ] || die '--version needs a release tag.'
            VERSION=$2
            shift 2
            ;;
        --version=*)
            VERSION=${1#--version=}
            shift
            ;;
        --no-start)
            NO_START=1
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            die "unknown option: $1 (run with --help for usage)"
            ;;
    esac
done

[ -n "${HOME:-}" ] || die 'HOME is not set; choose a user account before running the installer.'

case "$REPOSITORY" in
    */*) ;;
    *) die 'IO_GATEWAY_REPOSITORY must be in owner/repository form.' ;;
esac
case "$REPOSITORY" in
    *[!A-Za-z0-9._/-]*|*//*|/*|*/)
        die 'IO_GATEWAY_REPOSITORY contains unsupported characters.'
        ;;
esac

OS_NAME=$(uname -s 2>/dev/null || true)
MACHINE=$(uname -m 2>/dev/null || true)
case "$OS_NAME" in
    Linux)
        case "$MACHINE" in
            x86_64|amd64) TARGET=linux-x86_64 ;;
            aarch64|arm64) TARGET=linux-aarch64 ;;
            *) die "unsupported Linux CPU architecture: ${MACHINE:-unknown}. Releases support x86_64 and ARM64." ;;
        esac
        PLATFORM=linux
        ;;
    Darwin)
        # A terminal running under Rosetta reports x86_64 even on Apple
        # Silicon. Prefer the native ARM64 release when macOS tells us so.
        if [ "$MACHINE" = x86_64 ] \
            && command -v sysctl >/dev/null 2>&1 \
            && [ "$(sysctl -in sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
            MACHINE=arm64
        fi
        case "$MACHINE" in
            x86_64|amd64) TARGET=macos-x86_64 ;;
            aarch64|arm64) TARGET=macos-aarch64 ;;
            *) die "unsupported macOS CPU architecture: ${MACHINE:-unknown}. Releases support x86_64 and ARM64." ;;
        esac
        PLATFORM=macos
        ;;
    *)
        die "unsupported operating system: ${OS_NAME:-unknown}. Use install.ps1 on Windows."
        ;;
esac

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/io-gateway-install.XXXXXX") || die 'could not create a temporary directory.'

if [ "$VERSION" = latest ]; then
    release_json="$TMP_DIR/release.json"
    note "Resolving the latest release from ${REPOSITORY}."
    download "https://api.github.com/repos/${REPOSITORY}/releases/latest" "$release_json"
    TAG=$(tr '\n' ' ' < "$release_json" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
    [ -n "$TAG" ] || die 'could not read tag_name from the GitHub release response.'
else
    case "$VERSION" in
        v[0-9]*) TAG=$VERSION ;;
        [0-9]*) TAG="v$VERSION" ;;
        *) die "invalid release version: $VERSION" ;;
    esac
fi

case "$TAG" in
    v[0-9A-Za-z._-]*) ;;
    *) die "invalid GitHub release tag: $TAG" ;;
esac

ASSET_NAME="io-gateway-${TAG}-${TARGET}.tar.gz"
RELEASE_BASE="https://github.com/${REPOSITORY}/releases/download/${TAG}"
ARCHIVE="$TMP_DIR/$ASSET_NAME"
SUMS_FILE="$TMP_DIR/SHA256SUMS"

note "Downloading ${ASSET_NAME}."
download "$RELEASE_BASE/$ASSET_NAME" "$ARCHIVE"
download "$RELEASE_BASE/SHA256SUMS" "$SUMS_FILE"

EXPECTED_SHA256=$(awk -v filename="$ASSET_NAME" '$2 == filename || $2 == ("*" filename) { print $1; exit }' "$SUMS_FILE")
case "$EXPECTED_SHA256" in
    ????????* ) ;;
    *) die "SHA256SUMS does not contain ${ASSET_NAME}." ;;
esac
case "$EXPECTED_SHA256" in
    *[!0123456789abcdefABCDEF]*) die "SHA256SUMS has an invalid hash for ${ASSET_NAME}." ;;
esac
[ "${#EXPECTED_SHA256}" -eq 64 ] || die "SHA256SUMS has an invalid hash length for ${ASSET_NAME}."

ACTUAL_SHA256=$(sha256_file "$ARCHIVE")
EXPECTED_SHA256=$(printf '%s' "$EXPECTED_SHA256" | tr 'ABCDEF' 'abcdef')
ACTUAL_SHA256=$(printf '%s' "$ACTUAL_SHA256" | tr 'ABCDEF' 'abcdef')
[ "$ACTUAL_SHA256" = "$EXPECTED_SHA256" ] || die "checksum verification failed for ${ASSET_NAME}."
note 'Release checksum verified.'

EXTRACT_DIR="$TMP_DIR/package"
mkdir -p "$EXTRACT_DIR"
command -v tar >/dev/null 2>&1 || die 'tar is required to unpack the release.'
tar -xzf "$ARCHIVE" -C "$EXTRACT_DIR"
for required_file in io-gateway iogw config.example.json; do
    [ -f "$EXTRACT_DIR/$required_file" ] \
        || die "release archive is missing required file: ${required_file}"
done

if [ -n "${IO_GATEWAY_BIN_DIR:-}" ]; then
    BIN_DIR=$IO_GATEWAY_BIN_DIR
else
    BIN_DIR="$HOME/.local/bin"
fi

if [ -n "${IO_GATEWAY_CONFIG:-}" ]; then
    CONFIG_PATH=$IO_GATEWAY_CONFIG
    CONFIG_DIR=$(dirname "$CONFIG_PATH")
elif [ -n "${IO_GATEWAY_CONFIG_DIR:-}" ]; then
    CONFIG_DIR=$IO_GATEWAY_CONFIG_DIR
    CONFIG_PATH="$CONFIG_DIR/config.json"
elif [ "$PLATFORM" = macos ]; then
    if [ -n "${XDG_CONFIG_HOME:-}" ]; then
        CONFIG_DIR="$XDG_CONFIG_HOME/io-gateway"
    else
        CONFIG_DIR="$HOME/Library/Application Support/io-gateway"
    fi
    CONFIG_PATH="$CONFIG_DIR/config.json"
else
    CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/io-gateway"
    CONFIG_PATH="$CONFIG_DIR/config.json"
fi

if [ ! -d "$BIN_DIR" ]; then
    mkdir -p "$BIN_DIR"
fi
if [ ! -d "$CONFIG_DIR" ]; then
    (umask 077 && mkdir -p "$CONFIG_DIR")
fi

# User overrides may be relative paths. Service managers do not inherit the
# installer's current directory, so resolve both directories before their paths
# are written into a systemd unit or LaunchAgent plist.
BIN_DIR=$(cd "$BIN_DIR" && pwd -P)
CONFIG_DIR=$(cd "$CONFIG_DIR" && pwd -P)
CONFIG_PATH="$CONFIG_DIR/$(basename "$CONFIG_PATH")"

if [ -e "$CONFIG_PATH" ] && [ ! -f "$CONFIG_PATH" ]; then
    die "config path exists but is not a regular file: ${CONFIG_PATH}"
fi

install_binary() {
    source_path=$1
    destination_path=$2
    destination_dir=$(dirname "$destination_path")
    destination_name=$(basename "$destination_path")
    staged_path="$destination_dir/.${destination_name}.install.$$"

    rm -f "$staged_path"
    cp "$source_path" "$staged_path"
    chmod 755 "$staged_path"
    mv -f "$staged_path" "$destination_path"
}

GATEWAY_BINARY="$BIN_DIR/io-gateway"
IOGW_BINARY="$BIN_DIR/iogw"
install_binary "$EXTRACT_DIR/io-gateway" "$GATEWAY_BINARY"
install_binary "$EXTRACT_DIR/iogw" "$IOGW_BINARY"

CREATED_CONFIG=0
if [ ! -e "$CONFIG_PATH" ]; then
    PROXY_KEY="iogw_$(random_hex)"
    CONFIG_TMP="$CONFIG_DIR/.config.json.install.$$"
    umask 077
    sed \
        -e 's|"listen": "0.0.0.0:8319"|"listen": "127.0.0.1:8319"|' \
        -e "s|\"proxy_api_key\": \"your-shared-proxy-key\"|\"proxy_api_key\": \"${PROXY_KEY}\"|" \
        -e 's|"enabled": true|"enabled": false|' \
        -e 's|"api_key": "your-admin-api-key"|"api_key": ""|' \
        -e 's|"totp_secret": "PASTE_BASE32_SECRET_FROM_GOOGLE_AUTHENTICATOR_SETUP"|"totp_secret": ""|' \
        "$EXTRACT_DIR/config.example.json" > "$CONFIG_TMP"

    if ! grep -F '"listen": "127.0.0.1:8319"' "$CONFIG_TMP" >/dev/null 2>&1 \
        || ! grep -F "\"proxy_api_key\": \"${PROXY_KEY}\"" "$CONFIG_TMP" >/dev/null 2>&1 \
        || ! grep -F '"enabled": false' "$CONFIG_TMP" >/dev/null 2>&1 \
        || ! grep -F '"totp_secret": ""' "$CONFIG_TMP" >/dev/null 2>&1; then
        rm -f "$CONFIG_TMP"
        die 'the release config example changed unexpectedly; refusing to create an unsafe config.'
    fi

    chmod 600 "$CONFIG_TMP"
    mv "$CONFIG_TMP" "$CONFIG_PATH"
    mkdir -p "$CONFIG_DIR/auths"
    chmod 700 "$CONFIG_DIR/auths"
    CREATED_CONFIG=1
    note "Created a localhost-only config at ${CONFIG_PATH}."
else
    note "Keeping existing config and credentials at ${CONFIG_DIR}."
fi

SERVICE_STARTED=0

start_linux_service() {
    SERVICE_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
    SERVICE_FILE="$SERVICE_DIR/io-gateway.service"
    SERVICE_NAME=io-gateway.service

    command -v systemctl >/dev/null 2>&1 || return 1
    if [ ! -e "$SERVICE_FILE" ]; then
        mkdir -p "$SERVICE_DIR"
        SERVICE_TMP="$SERVICE_DIR/.io-gateway.service.install.$$"
        GATEWAY_ESCAPED=$(systemd_escape_argument "$GATEWAY_BINARY")
        CONFIG_ESCAPED=$(systemd_escape_argument "$CONFIG_PATH")
        CONFIG_DIR_ESCAPED=$(systemd_escape_argument "$CONFIG_DIR")
        umask 077
        {
            printf '%s\n' '[Unit]'
            printf '%s\n' 'Description=IO Gateway'
            printf '%s\n' 'After=network-online.target'
            printf '%s\n' ''
            printf '%s\n' '[Service]'
            printf 'ExecStart="%s" --config "%s"\n' "$GATEWAY_ESCAPED" "$CONFIG_ESCAPED"
            printf 'WorkingDirectory="%s"\n' "$CONFIG_DIR_ESCAPED"
            printf '%s\n' 'Restart=on-failure'
            printf '%s\n' 'RestartSec=3'
            printf '%s\n' ''
            printf '%s\n' '[Install]'
            printf '%s\n' 'WantedBy=default.target'
        } > "$SERVICE_TMP"
        mv "$SERVICE_TMP" "$SERVICE_FILE"
    fi

    if ! systemctl --user daemon-reload >/dev/null 2>&1 \
        || ! systemctl --user enable "$SERVICE_NAME" >/dev/null 2>&1; then
        return 1
    fi

    # `enable --now` does not restart an already-running unit. A release
    # upgrade replaces the binary at the same path, so explicitly restart it
    # when it was active to make the running gateway use the new version.
    if systemctl --user is-active --quiet "$SERVICE_NAME"; then
        systemctl --user restart "$SERVICE_NAME" >/dev/null 2>&1
    else
        systemctl --user start "$SERVICE_NAME" >/dev/null 2>&1
    fi
}

start_macos_service() {
    LAUNCH_DIR="$HOME/Library/LaunchAgents"
    PLIST_FILE="$LAUNCH_DIR/us.io-gateway.plist"
    LAUNCH_LABEL=us.io-gateway
    LAUNCH_DOMAIN="gui/$(id -u)"

    command -v launchctl >/dev/null 2>&1 || return 1
    if [ ! -e "$PLIST_FILE" ]; then
        mkdir -p "$LAUNCH_DIR"
        PLIST_TMP="$LAUNCH_DIR/.us.io-gateway.plist.install.$$"
        GATEWAY_XML=$(xml_escape "$GATEWAY_BINARY")
        CONFIG_XML=$(xml_escape "$CONFIG_PATH")
        CONFIG_DIR_XML=$(xml_escape "$CONFIG_DIR")
        LOG_XML=$(xml_escape "$CONFIG_DIR/io-gateway.log")
        ERROR_LOG_XML=$(xml_escape "$CONFIG_DIR/io-gateway-error.log")
        umask 077
        {
            printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>'
            printf '%s\n' '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
            printf '%s\n' '<plist version="1.0">'
            printf '%s\n' '<dict>'
            printf '%s\n' '  <key>Label</key>'
            printf '  <string>%s</string>\n' "$LAUNCH_LABEL"
            printf '%s\n' '  <key>ProgramArguments</key>'
            printf '%s\n' '  <array>'
            printf '    <string>%s</string>\n' "$GATEWAY_XML"
            printf '%s\n' '    <string>--config</string>'
            printf '    <string>%s</string>\n' "$CONFIG_XML"
            printf '%s\n' '  </array>'
            printf '%s\n' '  <key>WorkingDirectory</key>'
            printf '  <string>%s</string>\n' "$CONFIG_DIR_XML"
            printf '%s\n' '  <key>RunAtLoad</key>'
            printf '%s\n' '  <true/>'
            printf '%s\n' '  <key>KeepAlive</key>'
            printf '%s\n' '  <true/>'
            printf '%s\n' '  <key>StandardOutPath</key>'
            printf '  <string>%s</string>\n' "$LOG_XML"
            printf '%s\n' '  <key>StandardErrorPath</key>'
            printf '  <string>%s</string>\n' "$ERROR_LOG_XML"
            printf '%s\n' '</dict>'
            printf '%s\n' '</plist>'
        } > "$PLIST_TMP"
        mv "$PLIST_TMP" "$PLIST_FILE"
    fi

    if launchctl bootstrap "$LAUNCH_DOMAIN" "$PLIST_FILE" >/dev/null 2>&1; then
        return 0
    fi
    if launchctl kickstart -k "$LAUNCH_DOMAIN/$LAUNCH_LABEL" >/dev/null 2>&1; then
        return 0
    fi
    return 1
}

if [ "$NO_START" -eq 0 ]; then
    case "$PLATFORM" in
        linux)
            if start_linux_service; then
                if [ "$CREATED_CONFIG" -eq 0 ] || wait_for_gateway_health; then
                    SERVICE_STARTED=1
                    note 'Started the systemd user service.'
                else
                    warn 'the systemd user service was accepted, but the gateway did not become healthy at http://127.0.0.1:8319/health within 20 seconds.'
                    warn 'Run systemctl --user status io-gateway.service to inspect the service, then start it manually after fixing the error.'
                fi
            else
                warn 'could not start a systemd user service (common in containers and some SSH sessions).'
            fi
            ;;
        macos)
            if start_macos_service; then
                if [ "$CREATED_CONFIG" -eq 0 ] || wait_for_gateway_health; then
                    SERVICE_STARTED=1
                    note 'Started the LaunchAgent.'
                else
                    warn 'the LaunchAgent was accepted, but the gateway did not become healthy at http://127.0.0.1:8319/health within 20 seconds.'
                    warn 'Inspect ~/Library/Logs or the io-gateway log files, then start it manually after fixing the error.'
                fi
            else
                warn 'could not start a macOS LaunchAgent in this session.'
            fi
            ;;
    esac
fi

printf '\n'
note "Installed ${TAG} for ${TARGET}."
note "Gateway binary: ${GATEWAY_BINARY}"
note "Management client: ${IOGW_BINARY}"
note "Config: ${CONFIG_PATH}"

case ":${PATH:-}:" in
    *":${BIN_DIR}:"*) ;;
    *)
        printf '%s\n' "Add ${BIN_DIR} to PATH to run io-gateway and iogw by name."
        ;;
esac

if [ "$CREATED_CONFIG" -eq 1 ]; then
    printf '%s\n' 'The first-run gateway is bound to 127.0.0.1 and admin authentication is disabled only for local setup.'
    printf '%s\n' 'Before changing listen to a LAN/public address, configure a TOTP secret and enable admin_auth in config.json.'
    printf '%s\n' 'The generated client API key is stored in proxy_api_key in config.json; keep that file private.'
fi

if [ "$SERVICE_STARTED" -eq 1 ]; then
    printf '%s\n' 'Open http://127.0.0.1:8319/ to finish provider setup.'
else
    printf '%s\n' 'Start the gateway with:'
    printf '  "%s" --config "%s"\n' "$GATEWAY_BINARY" "$CONFIG_PATH"
    printf '%s\n' 'Then open http://127.0.0.1:8319/ to finish provider setup.'
fi
