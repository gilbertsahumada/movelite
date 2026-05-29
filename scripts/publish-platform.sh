#!/usr/bin/env bash
set -euo pipefail

# Publish a single platform package from a pre-built binary.
#
# Used to bootstrap the platform package names that can't be built on the
# local machine (Linux / Intel-Mac binaries come from the Build binaries
# workflow). Download the artifact, then point this script at the binary.
#
# Usage:
#   ./scripts/publish-platform.sh <platform> <binary-path> [version]
#   ./scripts/publish-platform.sh linux-x64 ~/Downloads/movelite-linux-x64/movelite
#
# Dry run by default; set PUBLISH=1 to actually publish.
#   PUBLISH=1 ./scripts/publish-platform.sh linux-x64 ./movelite

PLATFORM="${1:?usage: publish-platform.sh <platform> <binary-path> [version]}"
BINARY="${2:?usage: publish-platform.sh <platform> <binary-path> [version]}"
VERSION="${3:-0.1.0}"
PUBLISH="${PUBLISH:-0}"

cd "$(dirname "${BASH_SOURCE[0]}")/.."

case "$PLATFORM" in
  darwin-arm64|darwin-x64|linux-x64|linux-arm64) ;;
  *) echo "Unsupported platform: $PLATFORM" >&2; exit 1 ;;
esac

if [ ! -f "$BINARY" ]; then
  echo "Binary not found: $BINARY" >&2
  exit 1
fi

if ! npm whoami > /dev/null 2>&1; then
  echo "Not logged in to npm. Run: npm login" >&2
  exit 1
fi

echo "→ Staging movelite-${PLATFORM}@${VERSION} from ${BINARY}"
bash scripts/build-npm-package.sh "$PLATFORM" "$VERSION" "$BINARY" > /dev/null
echo "  build/movelite-${PLATFORM}/ ready"

echo
echo "→ Pack contents (dry run)"
(cd "build/movelite-${PLATFORM}" && npm pack --dry-run 2>&1 | grep -E "name:|version:|size|total files")

echo
if [ "$PUBLISH" != "1" ]; then
  echo "DRY RUN — nothing published. Re-run with PUBLISH=1 to publish."
  exit 0
fi

echo "→ Publishing movelite-${PLATFORM}@${VERSION} (npm will prompt for OTP)"
(cd "build/movelite-${PLATFORM}" && npm publish --access public)
echo "  movelite-${PLATFORM}@${VERSION} live"
