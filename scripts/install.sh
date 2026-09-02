#!/bin/sh
# Install IO Gateway from a GitHub Release into the current user's home directory.
#
# This script intentionally never uses sudo. It is suitable for Linux and macOS
# desktops with either x86_64 or ARM64 CPUs.

set -eu

REPOSITORY="${IO_GATEWAY_REPOSITORY:-giofahreza/io-gateway}"
VERSION="${IO_GATEWAY_VERSION:-latest}"
GATEWAY_PORT="${IO_GATEWAY_PORT:-8319}"
PORT_IS_EXPLICIT=0
AUTOSTART_MODE="${IO_GATEWAY_AUTOSTART:-auto}"
IOGW_MODE="${IO_GATEWAY_INSTALL_IOGW:-auto}"
INTERACTIVE_MODE="${IO_GATEWAY_INTERACTIVE:-auto}"
START_NOW_MODE="${IO_GATEWAY_START_NOW:-auto}"
AUTOSTART_SET_BY_CLI=0
IOGW_SET_BY_CLI=0
INTERACTIVE_SET_BY_CLI=0
START_NOW_SET_BY_CLI=0
# --no-start used to mean "install only" before start-now and autostart became
# separate choices. Keep that clean-install behavior for this spelling only;
# IO_GATEWAY_START_NOW=no remains independent from IO_GATEWAY_AUTOSTART.
LEGACY_NO_START_FLAG=0
TMP_DIR=""

if [ -n "${IO_GATEWAY_PORT:-}" ]; then
    PORT_IS_EXPLICIT=1
fi

usage() {
    printf '%s\n' \
        'Usage: install.sh [options]' \
        '' \
        'Installs the matching IO Gateway GitHub Release for this computer.' \
        '' \
        'Options:' \
        '  --version <tag>  Install a release tag such as v0.1.18 (or 0.1.18).' \
        '  --port <port>    Local port for a new configuration (default: 8319).' \
        '  --with-iogw      Install or update the iogw terminal management client.' \
        '  --without-iogw   Install the gateway only; retain an existing iogw binary.' \
        '  --autostart      Enable the user service / LaunchAgent at sign-in.' \
        '                   (--start is accepted as a legacy alias.)' \
        '  --no-autostart   Disable an installer-managed user service / LaunchAgent.' \
        '  --start-now      Start the gateway now without changing autostart.' \
        '  --no-start       Do not start the gateway now; preserves existing autostart.' \
        '  --interactive    Require terminal setup questions.' \
        '  --non-interactive  Use flags, environment, and safe defaults without questions.' \
        '  --help           Show this help.' \
        '' \
        'Environment overrides:' \
        '  IO_GATEWAY_VERSION      Same as --version.' \
        '  IO_GATEWAY_PORT         Same as --port for a new configuration.' \
        '  IO_GATEWAY_INSTALL_IOGW auto, yes, or no (default: auto).' \
        '  IO_GATEWAY_AUTOSTART    auto, yes, or no (default: auto).' \
        '  IO_GATEWAY_INTERACTIVE  auto, yes, or no (default: auto).' \
        '  IO_GATEWAY_START_NOW    auto, yes, or no (default: auto).' \
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

is_valid_port() {
    case "$1" in
        ''|*[!0-9]*) return 1 ;;
    esac
    [ "$1" -ge 1 ] 2>/dev/null && [ "$1" -le 65535 ] 2>/dev/null
}

validate_choice_mode() {
    choice_name=$1
    choice_value=$2
    case "$choice_value" in
        auto|yes|no) ;;
        *) die "${choice_name} must be auto, yes, or no." ;;
    esac
}

tty_is_available() {
    [ -t 1 ] && ( : </dev/tty ) 2>/dev/null
}

ask_yes_no() {
    question=$1
    default=$2
    answer=''

    case "$default" in
        yes) suffix='[Y/n]' ;;
        no) suffix='[y/N]' ;;
        *) die 'internal installer error: invalid prompt default.' ;;
    esac

    while :; do
        printf '%s ' "${question} ${suffix}" >/dev/tty
        if ! IFS= read -r answer </dev/tty; then
            answer=''
        fi
        case "$answer" in
            '') printf '%s' "$default"; return 0 ;;
            y|Y|yes|YES|Yes) printf '%s' yes; return 0 ;;
            n|N|no|NO|No) printf '%s' no; return 0 ;;
            *) printf '%s\n' 'Please answer yes or no.' >/dev/tty ;;
        esac
    done
}

ask_port() {
    default_port=$1
    selected_port=''

    while :; do
        printf 'Gateway port [%s] ' "$default_port" >/dev/tty
        if ! IFS= read -r selected_port </dev/tty; then
            selected_port=''
        fi
        [ -n "$selected_port" ] || selected_port=$default_port
        if is_valid_port "$selected_port"; then
            printf '%s' "$selected_port"
            return 0
        fi
        printf '%s\n' 'Choose a whole-number TCP port from 1 through 65535.' >/dev/tty
    done
}

# Return 0 when the local TCP port appears available, 1 when it is already
# listening, and 2 when this machine has no supported inspection tool. The
# gateway itself still verifies the bind when it starts, so this is an early,
# user-friendly preflight rather than a race-free reservation.
local_port_availability() {
    port_to_check=$1

    # Linux exposes listeners without requiring a separate package. Inspect
    # both families: a wildcard listener on either one may prevent a local
    # gateway from binding its chosen TCP port.
    if [ -r /proc/net/tcp ] || [ -r /proc/net/tcp6 ]; then
        port_hex=$(printf '%04X' "$port_to_check")
        for proc_tcp_file in /proc/net/tcp /proc/net/tcp6; do
            [ -r "$proc_tcp_file" ] || continue
            if awk -v port_hex="$port_hex" \
                'NR > 1 && $4 == "0A" && $2 ~ (":" port_hex "$") { found = 1; exit } END { exit !found }' \
                "$proc_tcp_file"; then
                return 1
            fi
        done
        return 0
    fi

    if command -v ss >/dev/null 2>&1; then
        if ss_result=$(ss -H -ltn "sport = :${port_to_check}" 2>/dev/null); then
            [ -z "$ss_result" ] && return 0
            return 1
        fi
    fi

    if command -v lsof >/dev/null 2>&1; then
        if lsof -nP -iTCP:"${port_to_check}" -sTCP:LISTEN >/dev/null 2>&1; then
            return 1
        fi
        return 0
    fi

    if command -v nc >/dev/null 2>&1; then
        if nc -z -w 1 127.0.0.1 "$port_to_check" >/dev/null 2>&1; then
            return 1
        fi
        return 0
    fi

    return 2
}

ensure_selected_port_is_available() {
    while :; do
        if local_port_availability "$GATEWAY_PORT"; then
            return 0
        fi
        port_check_status=$?

        if [ "$port_check_status" -eq 2 ]; then
            warn "could not check whether 127.0.0.1:${GATEWAY_PORT} is already in use; the gateway will verify it when it starts."
            return 0
        fi

        if [ "$INTERACTIVE" -eq 1 ] && [ "$PORT_IS_EXPLICIT" -eq 0 ]; then
            printf '%s\n' "Port ${GATEWAY_PORT} is already in use. Choose another local TCP port." >/dev/tty
            GATEWAY_PORT=$(ask_port "$GATEWAY_PORT")
            continue
        fi

        die "port ${GATEWAY_PORT} is already in use on 127.0.0.1. Choose another port."
    done
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
# bind its selected port and stay alive. Fresh installs always use this local
# endpoint; wait for it before reporting onboarding success.
gateway_health_check() {
    if command -v curl >/dev/null 2>&1; then
        curl --fail --silent --show-error --max-time 2 \
            "http://127.0.0.1:${GATEWAY_PORT}/health" >/dev/null 2>&1
    elif command -v wget >/dev/null 2>&1; then
        wget --quiet --timeout=2 --output-document=/dev/null \
            "http://127.0.0.1:${GATEWAY_PORT}/health" >/dev/null 2>&1
    else
        return 1
    fi
}

wait_for_gateway_health() {
    health_attempt=0
    while [ "$health_attempt" -lt 20 ]; do
        if gateway_health_check; then
            return 0
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
        --port)
            [ "$#" -ge 2 ] || die '--port needs a TCP port number.'
            GATEWAY_PORT=$2
            PORT_IS_EXPLICIT=1
            shift 2
            ;;
        --port=*)
            GATEWAY_PORT=${1#--port=}
            PORT_IS_EXPLICIT=1
            shift
            ;;
        --with-iogw)
            if [ "$IOGW_SET_BY_CLI" -eq 1 ] && [ "$IOGW_MODE" != yes ]; then
                die '--with-iogw conflicts with --without-iogw.'
            fi
            IOGW_MODE=yes
            IOGW_SET_BY_CLI=1
            shift
            ;;
        --without-iogw|--no-iogw)
            if [ "$IOGW_SET_BY_CLI" -eq 1 ] && [ "$IOGW_MODE" != no ]; then
                die '--without-iogw conflicts with --with-iogw.'
            fi
            IOGW_MODE=no
            IOGW_SET_BY_CLI=1
            shift
            ;;
        --autostart|--start)
            if [ "$AUTOSTART_SET_BY_CLI" -eq 1 ] && [ "$AUTOSTART_MODE" != yes ]; then
                die '--autostart conflicts with --no-autostart.'
            fi
            AUTOSTART_MODE=yes
            AUTOSTART_SET_BY_CLI=1
            shift
            ;;
        --no-autostart)
            if [ "$AUTOSTART_SET_BY_CLI" -eq 1 ] && [ "$AUTOSTART_MODE" != no ]; then
                die '--no-autostart conflicts with --autostart.'
            fi
            AUTOSTART_MODE=no
            AUTOSTART_SET_BY_CLI=1
            shift
            ;;
        --start-now)
            if [ "$START_NOW_SET_BY_CLI" -eq 1 ] && [ "$START_NOW_MODE" != yes ]; then
                die '--start-now conflicts with --no-start.'
            fi
            START_NOW_MODE=yes
            START_NOW_SET_BY_CLI=1
            shift
            ;;
        --no-start)
            if [ "$START_NOW_SET_BY_CLI" -eq 1 ] && [ "$START_NOW_MODE" != no ]; then
                die '--no-start conflicts with --start-now.'
            fi
            START_NOW_MODE=no
            START_NOW_SET_BY_CLI=1
            LEGACY_NO_START_FLAG=1
            shift
            ;;
        --interactive)
            if [ "$INTERACTIVE_SET_BY_CLI" -eq 1 ] && [ "$INTERACTIVE_MODE" != yes ]; then
                die '--interactive conflicts with --non-interactive.'
            fi
            INTERACTIVE_MODE=yes
            INTERACTIVE_SET_BY_CLI=1
            shift
            ;;
        --non-interactive|--yes)
            if [ "$INTERACTIVE_SET_BY_CLI" -eq 1 ] && [ "$INTERACTIVE_MODE" != no ]; then
                die '--non-interactive conflicts with --interactive.'
            fi
            INTERACTIVE_MODE=no
            INTERACTIVE_SET_BY_CLI=1
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

# Command-line values intentionally override environment defaults. Validate
# only after parsing so, for example, --autostart can replace a stale
# IO_GATEWAY_AUTOSTART value from a shell profile.
validate_choice_mode IO_GATEWAY_AUTOSTART "$AUTOSTART_MODE"
validate_choice_mode IO_GATEWAY_INSTALL_IOGW "$IOGW_MODE"
validate_choice_mode IO_GATEWAY_INTERACTIVE "$INTERACTIVE_MODE"
validate_choice_mode IO_GATEWAY_START_NOW "$START_NOW_MODE"

is_valid_port "$GATEWAY_PORT" \
    || die "invalid port: ${GATEWAY_PORT}. Choose a whole-number TCP port from 1 through 65535."

INTERACTIVE=0
case "$INTERACTIVE_MODE" in
    yes)
        tty_is_available || die '--interactive requires a controlling terminal; use --non-interactive for automation.'
        INTERACTIVE=1
        ;;
    auto)
        if tty_is_available; then
            INTERACTIVE=1
        fi
        ;;
esac

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

configured_local_port() {
    configured_listen=$(sed -n 's/^[[:space:]]*"listen"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$CONFIG_PATH" | sed -n '1p')
    case "$configured_listen" in
        127.0.0.1:*|0.0.0.0:*|localhost:*|'[::1]:'*|'[::]:'*)
            configured_port=${configured_listen##*:}
            if is_valid_port "$configured_port"; then
                printf '%s' "$configured_port"
                return 0
            fi
            ;;
    esac
    return 1
}

linux_service_path() {
    printf '%s' "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/io-gateway.service"
}

macos_launchagent_path() {
    printf '%s' "$HOME/Library/LaunchAgents/us.io-gateway.plist"
}

# Only change a service file that is clearly one created by this installer.
# Older releases did not have the marker, so recognize their stable label and
# the exact binary/config paths as well. A manually maintained unit/plist is
# deliberately left alone.
is_managed_linux_service() {
    service_file=$(linux_service_path)
    [ -f "$service_file" ] || return 1
    if grep -F '# Managed by IO Gateway installer' "$service_file" >/dev/null 2>&1; then
        return 0
    fi
    grep -F 'Description=IO Gateway' "$service_file" >/dev/null 2>&1 \
        && grep -F 'ExecStart=' "$service_file" >/dev/null 2>&1 \
        && grep -F "$GATEWAY_BINARY" "$service_file" >/dev/null 2>&1 \
        && grep -F "$CONFIG_PATH" "$service_file" >/dev/null 2>&1
}

is_managed_macos_launchagent() {
    plist_file=$(macos_launchagent_path)
    [ -f "$plist_file" ] || return 1
    if grep -F '<!-- Managed by IO Gateway installer -->' "$plist_file" >/dev/null 2>&1; then
        return 0
    fi
    grep -F '<string>us.io-gateway</string>' "$plist_file" >/dev/null 2>&1 \
        && grep -F "$GATEWAY_BINARY" "$plist_file" >/dev/null 2>&1 \
        && grep -F "$CONFIG_PATH" "$plist_file" >/dev/null 2>&1
}

has_existing_user_service() {
    [ ! -f "$AUTOSTART_DISABLED_MARKER" ] || return 1
    case "$PLATFORM" in
        linux)
            is_managed_linux_service
            ;;
        macos)
            is_managed_macos_launchagent
            ;;
        *) return 1 ;;
    esac
}

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
AUTOSTART_DISABLED_MARKER="$CONFIG_DIR/.io-gateway-autostart-disabled"
DIRECT_PID_FILE="$CONFIG_DIR/io-gateway-direct.pid"
DIRECT_LOG_FILE="$CONFIG_DIR/io-gateway.log"
DIRECT_ERROR_LOG_FILE="$CONFIG_DIR/io-gateway-error.log"

CONFIG_EXISTS=0
if [ -e "$CONFIG_PATH" ]; then
    CONFIG_EXISTS=1
fi

if [ "$CONFIG_EXISTS" -eq 0 ]; then
    if [ "$INTERACTIVE" -eq 1 ]; then
        printf '%s\n' '' >/dev/tty
        printf '%s\n' 'IO Gateway first-run setup. The gateway stays bound to 127.0.0.1 for safety.' >/dev/tty
        if [ "$PORT_IS_EXPLICIT" -eq 0 ]; then
            GATEWAY_PORT=$(ask_port "$GATEWAY_PORT")
        fi
    fi
elif [ "$PORT_IS_EXPLICIT" -eq 1 ]; then
    warn "--port is ignored because ${CONFIG_PATH} already exists; its listen setting is preserved."
fi

if [ "$CONFIG_EXISTS" -eq 1 ]; then
    configured_port=$(configured_local_port || true)
    if [ -n "$configured_port" ]; then
        GATEWAY_PORT=$configured_port
    fi
else
    ensure_selected_port_is_available
fi

case "$IOGW_MODE" in
    yes) INSTALL_IOGW=1 ;;
    no) INSTALL_IOGW=0 ;;
    auto)
        if [ "$CONFIG_EXISTS" -eq 0 ] && [ "$INTERACTIVE" -eq 1 ]; then
            if [ "$(ask_yes_no 'Install the iogw terminal management client and TUI?' yes)" = yes ]; then
                INSTALL_IOGW=1
            else
                INSTALL_IOGW=0
            fi
        elif [ "$CONFIG_EXISTS" -eq 1 ]; then
            # An upgrade refreshes an existing client but never adds one the
            # user previously chose not to install.
            if [ -x "$IOGW_BINARY" ]; then
                INSTALL_IOGW=1
            else
                INSTALL_IOGW=0
            fi
        else
            INSTALL_IOGW=1
        fi
        ;;
esac

case "$AUTOSTART_MODE" in
    yes) AUTOSTART=1 ;;
    no) AUTOSTART=0 ;;
    auto)
        if [ "$CONFIG_EXISTS" -eq 0 ] && [ "$LEGACY_NO_START_FLAG" -eq 1 ]; then
            # Keep the historical --no-start behavior for a clean install:
            # do not create a background service unless --autostart was
            # explicitly requested as well.
            AUTOSTART=0
        elif [ "$CONFIG_EXISTS" -eq 0 ] && [ "$INTERACTIVE" -eq 1 ]; then
            case "$PLATFORM" in
                linux) autostart_question='Start IO Gateway automatically with a systemd user service at sign-in?' ;;
                macos) autostart_question='Start IO Gateway automatically with a LaunchAgent at sign-in?' ;;
            esac
            if [ "$(ask_yes_no "$autostart_question" yes)" = yes ]; then
                AUTOSTART=1
            else
                AUTOSTART=0
            fi
        elif [ "$CONFIG_EXISTS" -eq 1 ] && has_existing_user_service; then
            AUTOSTART=1
        elif [ "$CONFIG_EXISTS" -eq 0 ]; then
            AUTOSTART=1
        else
            AUTOSTART=0
        fi
        ;;
esac

# Starting immediately is independent from starting at the next sign-in. The
# prompt is intentionally first-install-only; an upgrade retains its existing
# running/service state unless an explicit option requests otherwise.
case "$START_NOW_MODE" in
    yes)
        START_NOW=1
        ;;
    no)
        START_NOW=0
        ;;
    auto)
        START_NOW=0
        if [ "$CONFIG_EXISTS" -eq 0 ] && [ "$INTERACTIVE" -eq 1 ]; then
            if [ "$(ask_yes_no 'Start IO Gateway now after installation?' yes)" = yes ]; then
                START_NOW=1
            fi
        elif [ "$AUTOSTART" -eq 1 ]; then
            # Preserve the installer’s existing non-interactive and upgrade
            # behavior: an enabled service is also started/restarted now.
            START_NOW=1
        fi
        ;;
esac

# A first-run "no" is a durable preference: later upgrades must not quietly
# recreate the service. Explicit --no-autostart does the same for an existing
# installation. We persist the marker only after the archive, binary, and
# configuration have been installed successfully.
AUTOSTART_DISABLE_REQUESTED=0
if [ "$AUTOSTART" -eq 0 ] \
    && { [ "$AUTOSTART_MODE" = no ] \
        || { [ "$CONFIG_EXISTS" -eq 0 ] && [ "$INTERACTIVE" -eq 1 ]; } \
        || { [ "$CONFIG_EXISTS" -eq 0 ] && [ "$LEGACY_NO_START_FLAG" -eq 1 ]; }; }; then
    AUTOSTART_DISABLE_REQUESTED=1
fi

if [ "$INTERACTIVE" -eq 0 ] && [ "$CONFIG_EXISTS" -eq 0 ] \
    && { [ "$IOGW_MODE" = auto ] || [ "$AUTOSTART_MODE" = auto ] || [ "$PORT_IS_EXPLICIT" -eq 0 ]; }; then
    note 'No interactive terminal detected; using the default local setup choices. Use --interactive or explicit flags to choose them.'
fi

install_binary "$EXTRACT_DIR/io-gateway" "$GATEWAY_BINARY"
if [ "$INSTALL_IOGW" -eq 1 ]; then
    install_binary "$EXTRACT_DIR/iogw" "$IOGW_BINARY"
fi

CREATED_CONFIG=0
if [ ! -e "$CONFIG_PATH" ]; then
    PROXY_KEY="iogw_$(random_hex)"
    CONFIG_TMP="$CONFIG_DIR/.config.json.install.$$"
    umask 077
    sed \
        -e "s|\"listen\": \"0.0.0.0:8319\"|\"listen\": \"127.0.0.1:${GATEWAY_PORT}\"|" \
        -e "s|\"proxy_api_key\": \"your-shared-proxy-key\"|\"proxy_api_key\": \"${PROXY_KEY}\"|" \
        -e 's|"enabled": true|"enabled": false|' \
        -e 's|"api_key": "your-admin-api-key"|"api_key": ""|' \
        -e 's|"totp_secret": "PASTE_BASE32_SECRET_FROM_GOOGLE_AUTHENTICATOR_SETUP"|"totp_secret": ""|' \
        -e "s|\"redirect_uri\": \"http://127.0.0.1:8319/login/qwen/callback\"|\"redirect_uri\": \"http://127.0.0.1:${GATEWAY_PORT}/login/qwen/callback\"|" \
        "$EXTRACT_DIR/config.example.json" > "$CONFIG_TMP"

    if ! grep -F "\"listen\": \"127.0.0.1:${GATEWAY_PORT}\"" "$CONFIG_TMP" >/dev/null 2>&1 \
        || ! grep -F "\"proxy_api_key\": \"${PROXY_KEY}\"" "$CONFIG_TMP" >/dev/null 2>&1 \
        || ! grep -F '"enabled": false' "$CONFIG_TMP" >/dev/null 2>&1 \
        || ! grep -F '"totp_secret": ""' "$CONFIG_TMP" >/dev/null 2>&1 \
        || ! grep -F "\"redirect_uri\": \"http://127.0.0.1:${GATEWAY_PORT}/login/qwen/callback\"" "$CONFIG_TMP" >/dev/null 2>&1; then
        rm -f "$CONFIG_TMP"
        die 'the release config example changed unexpectedly; refusing to create an unsafe config.'
    fi

    chmod 600 "$CONFIG_TMP"
    mv "$CONFIG_TMP" "$CONFIG_PATH"
    mkdir -p "$CONFIG_DIR/auths"
    chmod 700 "$CONFIG_DIR/auths"
    CREATED_CONFIG=1
    note "Created a localhost-only config on port ${GATEWAY_PORT} at ${CONFIG_PATH}."
else
    note "Keeping existing config and credentials at ${CONFIG_DIR}."
fi

if [ "$AUTOSTART" -eq 1 ]; then
    rm -f "$AUTOSTART_DISABLED_MARKER"
elif [ "$AUTOSTART_DISABLE_REQUESTED" -eq 1 ]; then
    (umask 077 && : > "$AUTOSTART_DISABLED_MARKER")
fi

SERVICE_STARTED=0
SERVICE_ENABLED=0
DIRECT_STARTED=0
GATEWAY_ALREADY_RUNNING=0

disable_linux_service() {
    SERVICE_FILE=$(linux_service_path)
    SERVICE_NAME=io-gateway.service

    [ -e "$SERVICE_FILE" ] || return 0
    if ! is_managed_linux_service; then
        warn "${SERVICE_FILE} is not managed by this installer; it was left unchanged. Disable it manually if it starts IO Gateway."
        return 1
    fi
    if ! command -v systemctl >/dev/null 2>&1; then
        warn 'could not disable the existing systemd user service because systemctl is unavailable.'
        return 1
    fi

    if ! systemctl --user disable "$SERVICE_NAME" >/dev/null 2>&1; then
        warn 'could not disable the existing systemd user service.'
        return 1
    fi
    if systemctl --user is-active --quiet "$SERVICE_NAME" \
        && ! systemctl --user stop "$SERVICE_NAME" >/dev/null 2>&1; then
        warn 'disabled the systemd user service, but could not stop its running process.'
        return 1
    fi
    note 'Disabled the systemd user service.'
    return 0
}

disable_macos_service() {
    PLIST_FILE=$(macos_launchagent_path)
    LAUNCH_LABEL=us.io-gateway
    LAUNCH_DOMAIN="gui/$(id -u)"

    [ -e "$PLIST_FILE" ] || return 0
    if ! is_managed_macos_launchagent; then
        warn "${PLIST_FILE} is not managed by this installer; it was left unchanged. Disable it manually if it starts IO Gateway."
        return 1
    fi

    if command -v launchctl >/dev/null 2>&1; then
        if ! launchctl bootout "$LAUNCH_DOMAIN/$LAUNCH_LABEL" >/dev/null 2>&1 \
            && ! launchctl bootout "$LAUNCH_DOMAIN" "$PLIST_FILE" >/dev/null 2>&1; then
            warn 'could not stop the existing LaunchAgent; removing its plist prevents it from starting at the next sign-in.'
        fi
    else
        warn 'launchctl is unavailable; removing the installer-managed plist prevents it from starting at the next sign-in.'
    fi
    rm -f "$PLIST_FILE"
    note 'Removed the installer-managed LaunchAgent.'
    return 0
}

start_linux_service() {
    SERVICE_FILE=$(linux_service_path)
    SERVICE_DIR=$(dirname "$SERVICE_FILE")
    SERVICE_NAME=io-gateway.service

    command -v systemctl >/dev/null 2>&1 || return 1
    if [ -e "$SERVICE_FILE" ] && ! is_managed_linux_service; then
        warn "${SERVICE_FILE} is not managed by this installer; it was left unchanged."
        return 1
    fi
    if [ ! -e "$SERVICE_FILE" ]; then
        mkdir -p "$SERVICE_DIR"
        SERVICE_TMP="$SERVICE_DIR/.io-gateway.service.install.$$"
        GATEWAY_ESCAPED=$(systemd_escape_argument "$GATEWAY_BINARY")
        CONFIG_ESCAPED=$(systemd_escape_argument "$CONFIG_PATH")
        CONFIG_DIR_ESCAPED=$(systemd_escape_argument "$CONFIG_DIR")
        umask 077
        {
            printf '%s\n' '# Managed by IO Gateway installer'
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

    [ "$START_NOW" -eq 1 ] || return 0

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
    PLIST_FILE=$(macos_launchagent_path)
    LAUNCH_LABEL=us.io-gateway
    LAUNCH_DOMAIN="gui/$(id -u)"

    command -v launchctl >/dev/null 2>&1 || return 1
    if [ -e "$PLIST_FILE" ] && ! is_managed_macos_launchagent; then
        warn "${PLIST_FILE} is not managed by this installer; it was left unchanged."
        return 1
    fi
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
            printf '%s\n' '<!-- Managed by IO Gateway installer -->'
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

    [ "$START_NOW" -eq 1 ] || return 0

    if launchctl bootstrap "$LAUNCH_DOMAIN" "$PLIST_FILE" >/dev/null 2>&1; then
        return 0
    fi
    if launchctl kickstart -k "$LAUNCH_DOMAIN/$LAUNCH_LABEL" >/dev/null 2>&1; then
        return 0
    fi
    return 1
}

# Launch a one-off local gateway when the user wants it running now without a
# sign-in service. The PID file is informational after success; on this same
# attempt it also lets us clean up the process we just launched if it never
# becomes healthy.
start_direct_gateway() {
    direct_port_status=0
    if local_port_availability "$GATEWAY_PORT"; then
        :
    else
        direct_port_status=$?
        if [ "$direct_port_status" -eq 1 ] && gateway_health_check; then
            note "A gateway is already healthy at http://127.0.0.1:${GATEWAY_PORT}/."
            return 2
        fi
        if [ "$direct_port_status" -eq 1 ]; then
            warn "cannot start directly: port ${GATEWAY_PORT} is already in use."
        else
            warn "could not recheck whether 127.0.0.1:${GATEWAY_PORT} is in use before starting; continuing and letting the gateway verify its bind."
        fi
        [ "$direct_port_status" -eq 2 ] || return 1
    fi

    DIRECT_PID_TMP="$CONFIG_DIR/.io-gateway-direct.pid.install.$$"
    rm -f "$DIRECT_PID_TMP"
    (
        umask 077
        nohup "$GATEWAY_BINARY" --config "$CONFIG_PATH" \
            >> "$DIRECT_LOG_FILE" 2>> "$DIRECT_ERROR_LOG_FILE" < /dev/null &
        direct_pid=$!
        printf '%s\n' "$direct_pid" > "$DIRECT_PID_TMP"
        mv "$DIRECT_PID_TMP" "$DIRECT_PID_FILE"
    )

    if wait_for_gateway_health; then
        return 0
    fi

    direct_pid=$(sed -n '1p' "$DIRECT_PID_FILE" 2>/dev/null || true)
    case "$direct_pid" in
        ''|*[!0-9]*) ;;
        *)
            if kill -0 "$direct_pid" 2>/dev/null; then
                kill "$direct_pid" 2>/dev/null || true
            fi
            ;;
    esac
    rm -f "$DIRECT_PID_FILE"
    warn "the directly started gateway did not become healthy within 20 seconds; inspect ${DIRECT_ERROR_LOG_FILE}."
    return 1
}

if [ "$AUTOSTART_DISABLE_REQUESTED" -eq 1 ]; then
    case "$PLATFORM" in
        linux) disable_linux_service || true ;;
        macos) disable_macos_service || true ;;
    esac
fi

if [ "$AUTOSTART" -eq 1 ]; then
    case "$PLATFORM" in
        linux)
            if start_linux_service; then
                SERVICE_ENABLED=1
                if [ "$START_NOW" -eq 0 ]; then
                    note 'Enabled the systemd user service; it will start at your next sign-in.'
                elif [ "$CREATED_CONFIG" -eq 0 ] || wait_for_gateway_health; then
                    SERVICE_STARTED=1
                    note 'Started the systemd user service.'
                else
                    warn "the systemd user service was accepted, but the gateway did not become healthy at http://127.0.0.1:${GATEWAY_PORT}/health within 20 seconds."
                    warn 'Run systemctl --user status io-gateway.service to inspect the service, then start it manually after fixing the error.'
                fi
            else
                warn 'could not start a systemd user service (common in containers and some SSH sessions).'
            fi
            ;;
        macos)
            if start_macos_service; then
                SERVICE_ENABLED=1
                if [ "$START_NOW" -eq 0 ]; then
                    note 'Enabled the LaunchAgent; it will start at your next sign-in.'
                elif [ "$CREATED_CONFIG" -eq 0 ] || wait_for_gateway_health; then
                    SERVICE_STARTED=1
                    note 'Started the LaunchAgent.'
                else
                    warn "the LaunchAgent was accepted, but the gateway did not become healthy at http://127.0.0.1:${GATEWAY_PORT}/health within 20 seconds."
                    warn 'Inspect ~/Library/Logs or the io-gateway log files, then start it manually after fixing the error.'
                fi
            else
                warn 'could not start a macOS LaunchAgent in this session.'
            fi
            ;;
    esac
elif [ "$START_NOW" -eq 1 ]; then
    if start_direct_gateway; then
        DIRECT_STARTED=1
        note "Started IO Gateway in the background (PID recorded in ${DIRECT_PID_FILE})."
    else
        direct_start_status=$?
        if [ "$direct_start_status" -eq 2 ]; then
            GATEWAY_ALREADY_RUNNING=1
        else
            warn 'could not start IO Gateway in the background.'
        fi
    fi
fi

# An upgrade with --no-start deliberately leaves a currently running gateway
# alone. Report that state accurately instead of offering a redundant manual
# start command.
if [ "$SERVICE_STARTED" -eq 0 ] \
    && [ "$DIRECT_STARTED" -eq 0 ] \
    && [ "$GATEWAY_ALREADY_RUNNING" -eq 0 ] \
    && gateway_health_check; then
    GATEWAY_ALREADY_RUNNING=1
    note "An existing IO Gateway is already healthy at http://127.0.0.1:${GATEWAY_PORT}/."
fi

printf '\n'
note "Installed ${TAG} for ${TARGET}."
note "Gateway binary: ${GATEWAY_BINARY}"
if [ "$INSTALL_IOGW" -eq 1 ]; then
    note "Management client: ${IOGW_BINARY}"
elif [ -x "$IOGW_BINARY" ]; then
    note "Management client: keeping existing ${IOGW_BINARY}"
else
    note 'Management client: not installed (re-run with --with-iogw to add the iogw TUI).'
fi
note "Config: ${CONFIG_PATH}"

case ":${PATH:-}:" in
    *":${BIN_DIR}:"*) ;;
    *)
        printf '%s\n' "Add ${BIN_DIR} to PATH to run io-gateway and iogw by name."
        ;;
esac

if [ "$CREATED_CONFIG" -eq 1 ]; then
    printf '%s\n' "The first-run gateway is bound to 127.0.0.1:${GATEWAY_PORT}; admin authentication is disabled only for local setup."
    printf '%s\n' 'Before changing listen to a LAN/public address, configure a TOTP secret and enable admin_auth in config.json.'
    printf '%s\n' 'The generated client API key is stored in proxy_api_key in config.json; keep that file private.'
fi

if [ "$SERVICE_STARTED" -eq 1 ] || [ "$DIRECT_STARTED" -eq 1 ] || [ "$GATEWAY_ALREADY_RUNNING" -eq 1 ]; then
    if [ "$CREATED_CONFIG" -eq 1 ]; then
        printf '%s\n' "Open http://127.0.0.1:${GATEWAY_PORT}/ to finish provider setup."
    else
        printf '%s\n' "The gateway uses its existing listen setting in ${CONFIG_PATH}; open that configured address to continue setup."
    fi
elif [ "$SERVICE_ENABLED" -eq 1 ]; then
    printf '%s\n' 'The gateway is configured to start automatically at your next sign-in.'
    printf '%s\n' 'Start it now with:'
    printf '  "%s" --config "%s"\n' "$GATEWAY_BINARY" "$CONFIG_PATH"
else
    printf '%s\n' 'Start the gateway with:'
    printf '  "%s" --config "%s"\n' "$GATEWAY_BINARY" "$CONFIG_PATH"
    if [ "$CREATED_CONFIG" -eq 1 ]; then
        printf '%s\n' "Then open http://127.0.0.1:${GATEWAY_PORT}/ to finish provider setup."
    else
        printf '%s\n' "Then open the address configured in ${CONFIG_PATH} to continue setup."
    fi
fi
