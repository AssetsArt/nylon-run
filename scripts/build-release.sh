#!/bin/bash
# Build nyrun release binary for the current platform.
#
# Usage:
#   ./scripts/build-release.sh                         # build for current platform
#   ./scripts/build-release.sh --output dist            # custom output directory
#   ./scripts/build-release.sh --target x86_64-unknown-linux-gnu  # cross-compile
#
# Output:
#   <output_dir>/nyrun              – stripped release binary
#   <output_dir>/checksums.sha256   – SHA256 checksums

set -euo pipefail

# ─── Configuration ────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTPUT_DIR="dist"
TARGET=""
JOBS=""

# ─── Colors ───────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
NC='\033[0m'

log()   { echo -e "${CYAN}[build-release]${NC} $*"; }
ok()    { echo -e "${GREEN}[build-release]${NC} $*"; }
warn()  { echo -e "${YELLOW}[build-release]${NC} $*"; }
error() { echo -e "${RED}[build-release]${NC} $*" >&2; }

# ─── Parse arguments ─────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --output|-o)
            OUTPUT_DIR="$2"; shift 2
            ;;
        --target|-t)
            TARGET="$2"; shift 2
            ;;
        --jobs|-j)
            JOBS="$2"; shift 2
            ;;
        --help|-h)
            head -12 "$0" | tail -11 | sed 's/^# \?//'
            exit 0
            ;;
        *)
            error "Unknown option: $1"
            exit 1
            ;;
    esac
done

# ─── Platform detection ──────────────────────────────────────────────
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux)  PLATFORM="linux" ;;
    Darwin) PLATFORM="darwin" ;;
    *)      error "Unsupported OS: $OS"; exit 1 ;;
esac

case "$ARCH" in
    x86_64|amd64)   ARCH_LABEL="amd64" ;;
    aarch64|arm64)  ARCH_LABEL="arm64" ;;
    *)              error "Unsupported architecture: $ARCH"; exit 1 ;;
esac

log "Platform: ${PLATFORM}/${ARCH_LABEL}"

# ─── Setup output directory ──────────────────────────────────────────
OUTPUT_PATH="$PROJECT_ROOT/$OUTPUT_DIR"
mkdir -p "$OUTPUT_PATH"

# ─── Build arguments ─────────────────────────────────────────────────
CARGO_ARGS=(--release)
if [[ -n "$TARGET" ]]; then
    CARGO_ARGS+=(--target "$TARGET")
fi
if [[ -n "$JOBS" ]]; then
    CARGO_ARGS+=(--jobs "$JOBS")
fi

# Determine target directory
if [[ -n "$TARGET" ]]; then
    TARGET_DIR="$PROJECT_ROOT/target/$TARGET/release"
else
    TARGET_DIR="$PROJECT_ROOT/target/release"
fi

# ─── Build ────────────────────────────────────────────────────────────
log "Building nyrun..."
cd "$PROJECT_ROOT"

if cargo build "${CARGO_ARGS[@]}" 2>&1; then
    if [[ -f "$TARGET_DIR/nyrun" ]]; then
        cp "$TARGET_DIR/nyrun" "$OUTPUT_PATH/nyrun"
        ok "Built nyrun"
    else
        error "Binary not found at $TARGET_DIR/nyrun"
        exit 1
    fi
else
    error "Build failed"
    exit 1
fi

# ─── Strip binary ────────────────────────────────────────────────────
log "Stripping binary..."
if file "$OUTPUT_PATH/nyrun" 2>/dev/null | grep -q "ELF\|Mach-O"; then
    strip "$OUTPUT_PATH/nyrun" 2>/dev/null || warn "Could not strip binary"
fi

# ─── Package ─────────────────────────────────────────────────────────
ARTIFACT="nyrun-${PLATFORM}-${ARCH_LABEL}"
log "Packaging ${ARTIFACT}.tar.gz..."
cd "$OUTPUT_PATH"
tar czf "${ARTIFACT}.tar.gz" nyrun

# ─── Generate checksums ──────────────────────────────────────────────
log "Generating checksums..."
if command -v sha256sum &>/dev/null; then
    sha256sum "${ARTIFACT}.tar.gz" > "${ARTIFACT}.tar.gz.sha256"
elif command -v shasum &>/dev/null; then
    shasum -a 256 "${ARTIFACT}.tar.gz" > "${ARTIFACT}.tar.gz.sha256"
fi

cd "$PROJECT_ROOT"

# ─── Summary ─────────────────────────────────────────────────────────
echo ""
log "═══════════════════════════════════════════════════"
log "  Build Release Summary"
log "═══════════════════════════════════════════════════"
echo ""

BINARY_SIZE=$(du -h "$OUTPUT_PATH/nyrun" | cut -f1)
ARCHIVE_SIZE=$(du -h "$OUTPUT_PATH/${ARTIFACT}.tar.gz" | cut -f1)

ok "  nyrun              ($BINARY_SIZE)"
ok "  ${ARTIFACT}.tar.gz ($ARCHIVE_SIZE)"

echo ""
log "Output: $OUTPUT_PATH/"
log "Checksums: $OUTPUT_PATH/${ARTIFACT}.tar.gz.sha256"
echo ""
ok "Build complete"
