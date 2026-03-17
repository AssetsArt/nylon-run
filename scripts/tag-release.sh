#!/bin/bash
# Create and push a release tag to trigger GitHub Actions.
#
# Usage:
#   ./scripts/tag-release.sh 0.1.0              # creates and pushes v0.1.0
#   ./scripts/tag-release.sh 0.1.0 --dry-run    # show what would happen
#   ./scripts/tag-release.sh 0.1.0 --force      # overwrite existing tag
#
# Options:
#   --dry-run   Print commands without executing
#   --force     Overwrite existing tag

set -euo pipefail

VERSION=""
DRY_RUN=false
FORCE=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) DRY_RUN=true; shift ;;
        --force)   FORCE=true; shift ;;
        --help|-h) sed -n '2,10p' "$0" | sed 's/^# \?//'; exit 0 ;;
        -*)        echo "Unknown option: $1" >&2; exit 1 ;;
        *)         VERSION="$1"; shift ;;
    esac
done

if [[ -z "$VERSION" ]]; then
    echo "Usage: $0 <version> [--dry-run] [--force]" >&2
    exit 1
fi

TAG="v${VERSION}"

run() {
    if $DRY_RUN; then
        echo "[dry-run] $*"
    else
        echo "=> $*"
        "$@"
    fi
}

echo "Tag: ${TAG}"
echo ""

# If tag already exists and --force wasn't given, ask to overwrite
if ! $FORCE && git rev-parse "$TAG" >/dev/null 2>&1; then
    read -rp "Tag '${TAG}' already exists. Overwrite? [y/N] " answer
    if [[ "$answer" =~ ^[Yy]$ ]]; then
        FORCE=true
    else
        echo "Aborted."
        exit 1
    fi
fi

FORCE_FLAG=""
if $FORCE; then
    FORCE_FLAG="-f"
fi

run git tag $FORCE_FLAG "$TAG"
run git push $FORCE_FLAG origin "$TAG"

if ! $DRY_RUN; then
    echo ""
    echo "Done! Tag ${TAG} pushed to origin."
    echo "GitHub Actions will build and create the release."
    echo "Track progress: https://github.com/AssetsArt/nylon-run/actions"
fi
