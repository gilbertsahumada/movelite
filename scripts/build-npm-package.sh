#!/usr/bin/env bash
set -euo pipefail

# Build a platform-specific npm package from a compiled binary.
#
# Usage: build-npm-package.sh <platform> <version> <binary-path>
# Example: build-npm-package.sh darwin-arm64 0.1.0 ./target/mvlite
#
# Output: build/mvlite-<platform>/ ready for `npm publish`.

PLATFORM="$1"
VERSION="$2"
BINARY="$3"

case "$PLATFORM" in
  darwin-arm64|darwin-x64|linux-x64|linux-arm64) ;;
  *)
    echo "Unsupported platform: $PLATFORM" >&2
    exit 1
    ;;
esac

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "Invalid semver version: $VERSION" >&2
  exit 1
fi

if [ ! -f "$BINARY" ]; then
  echo "Missing binary: $BINARY" >&2
  exit 1
fi

PKG_NAME="mvlite-${PLATFORM}"
OUT_DIR="build/${PKG_NAME}"

OS="${PLATFORM%-*}"
ARCH="${PLATFORM#*-}"

rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR/bin"
cp "$BINARY" "$OUT_DIR/bin/mvlite"
chmod +x "$OUT_DIR/bin/mvlite"

cat > "$OUT_DIR/package.json" <<EOF
{
  "name": "${PKG_NAME}",
  "version": "${VERSION}",
  "description": "Pre-built mvlite binary for ${PLATFORM}",
  "repository": {
    "type": "git",
    "url": "https://github.com/gilbertsahumada/mvlite.git"
  },
  "license": "Apache-2.0",
  "os": ["${OS}"],
  "cpu": ["${ARCH}"],
  "files": ["bin/"]
}
EOF

cat > "$OUT_DIR/README.md" <<EOF
# ${PKG_NAME}

Pre-built mvlite binary for \`${PLATFORM}\`.

Installed automatically as an optional dependency of [\`mvlite\`](https://www.npmjs.com/package/mvlite). Do not install directly.
EOF

echo "Built ${OUT_DIR}"
