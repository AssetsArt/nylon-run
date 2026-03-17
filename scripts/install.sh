#!/bin/sh
# Nylon Run — one-line installer
#
# Usage:
#   curl -fsSL https://mesh.nylon.sh/install | sh
#   curl -fsSL https://mesh.nylon.sh/install | sh -s -- --version v0.1.0
#
# Options:
#   --version <tag>   Install a specific version (default: latest)
#   --prefix <path>   Install prefix (default: /usr/local)
#   --help            Show this help

set -eu

REPO="AssetsArt/nylon-run"
VERSION=""
PREFIX="/usr/local"
BIN_DIR=""

# ─── Colors ───────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

log()   { printf "${CYAN}[nyrun]${NC} %s\n" "$*"; }
ok()    { printf "${GREEN}[nyrun]${NC} %s\n" "$*"; }
warn()  { printf "${YELLOW}[nyrun]${NC} %s\n" "$*"; }
error() { printf "${RED}[nyrun]${NC} %s\n" "$*" >&2; }
fatal() { error "$*"; exit 1; }

# ─── Parse arguments ─────────────────────────────────────────────────
while [ $# -gt 0 ]; do
    case "$1" in
        --version)  VERSION="$2"; shift 2 ;;
        --prefix)   PREFIX="$2"; shift 2 ;;
        --help|-h)  sed -n '2,12p' "$0" | sed 's/^# \?//'; exit 0 ;;
        *)          fatal "Unknown option: $1" ;;
    esac
done

BIN_DIR="${PREFIX}/bin"

# ─── Platform detection ──────────────────────────────────────────────
detect_platform() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Linux)  PLATFORM="linux" ;;
        Darwin) PLATFORM="darwin" ;;
        *)      fatal "Unsupported OS: $OS" ;;
    esac

    case "$ARCH" in
        x86_64|amd64)   ARCH="amd64" ;;
        aarch64|arm64)  ARCH="arm64" ;;
        *)              fatal "Unsupported architecture: $ARCH" ;;
    esac

    log "Detected platform: ${PLATFORM}/${ARCH}"
}

# ─── Fetch latest version ────────────────────────────────────────────
resolve_version() {
    if [ -n "$VERSION" ]; then
        log "Using specified version: ${VERSION}"
        return
    fi

    log "Fetching latest version..."

    if command -v curl >/dev/null 2>&1; then
        VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')
    elif command -v wget >/dev/null 2>&1; then
        VERSION=$(wget -qO- "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')
    else
        fatal "Neither curl nor wget found. Please install one of them."
    fi

    if [ -z "$VERSION" ]; then
        fatal "Could not determine latest version. Use --version to specify."
    fi

    log "Latest version: ${VERSION}"
}

# ─── Download ─────────────────────────────────────────────────────────
download() {
    url="$1"
    dest="$2"

    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$url" -o "$dest"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$dest" "$url"
    fi
}

# ─── Check root ───────────────────────────────────────────────────────
need_root() {
    if [ "$(id -u)" -ne 0 ]; then
        if command -v sudo >/dev/null 2>&1; then
            SUDO="sudo"
        else
            fatal "This installer needs root privileges. Please run with sudo."
        fi
    else
        SUDO=""
    fi
}

# ─── Install ──────────────────────────────────────────────────────────
install_nyrun() {
    TMPDIR=$(mktemp -d)
    trap 'rm -rf "$TMPDIR"' EXIT

    ARTIFACT="nyrun-${PLATFORM}-${ARCH}"
    BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"

    # Download binary
    BINARY_URL="${BASE_URL}/${ARTIFACT}.tar.gz"
    log "Downloading ${ARTIFACT}.tar.gz..."
    download "$BINARY_URL" "$TMPDIR/${ARTIFACT}.tar.gz" || fatal "Failed to download binary"

    # Verify checksum
    SHA_URL="${BINARY_URL}.sha256"
    if download "$SHA_URL" "$TMPDIR/${ARTIFACT}.tar.gz.sha256" 2>/dev/null; then
        log "Verifying checksum..."
        EXPECTED=$(awk '{print $1}' "$TMPDIR/${ARTIFACT}.tar.gz.sha256")
        if command -v sha256sum >/dev/null 2>&1; then
            ACTUAL=$(sha256sum "$TMPDIR/${ARTIFACT}.tar.gz" | awk '{print $1}')
        elif command -v shasum >/dev/null 2>&1; then
            ACTUAL=$(shasum -a 256 "$TMPDIR/${ARTIFACT}.tar.gz" | awk '{print $1}')
        else
            warn "No sha256 tool found, skipping checksum verification"
            ACTUAL="$EXPECTED"
        fi
        if [ "$EXPECTED" != "$ACTUAL" ]; then
            fatal "Checksum mismatch! Expected: ${EXPECTED}, Got: ${ACTUAL}"
        fi
        ok "Checksum verified"
    fi

    # Extract
    log "Extracting..."
    tar xzf "$TMPDIR/${ARTIFACT}.tar.gz" -C "$TMPDIR"

    # Install binary
    log "Installing to ${BIN_DIR}..."
    $SUDO mkdir -p "$BIN_DIR"
    $SUDO install -m 755 "$TMPDIR/nyrun" "$BIN_DIR/nyrun"

    # Create nyrun working directory
    $SUDO mkdir -p /var/run/nyrun
    $SUDO chmod 755 /var/run/nyrun
}

# ─── Main ─────────────────────────────────────────────────────────────
main() {
    echo ""
    printf "${BOLD}${CYAN}  ┌──────────────────────────┐${NC}\n"
    printf "${BOLD}${CYAN}  │     Nylon Run Installer    │${NC}\n"
    printf "${BOLD}${CYAN}  └──────────────────────────┘${NC}\n"
    echo ""

    detect_platform
    resolve_version
    need_root
    install_nyrun

    echo ""
    printf "${BOLD}${GREEN}  ✓ nyrun ${VERSION} installed successfully!${NC}\n"
    echo ""
    log "Quick start:"
    log "  nyrun bin ./my-app                    # manage a process"
    log "  nyrun run ./my-app --p 80:8000        # process + reverse proxy"
    log "  nyrun ls                              # list processes"
    echo ""
    log "Generate systemd service:"
    log "  sudo nyrun startup"
    echo ""
    log "Docs: https://github.com/${REPO}"
    echo ""
}

main
