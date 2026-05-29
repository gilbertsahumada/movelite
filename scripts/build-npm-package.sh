#!/usr/bin/env bash
set -euo pipefail

# Build a platform-specific npm package from a compiled binary.
#
# Usage: build-npm-package.sh <platform> <version> <binary-path>
# Example: build-npm-package.sh darwin-arm64 0.1.0 ./target/movelite
#
# Output: build/movelite-<platform>/ ready for `npm publish`.

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

PKG_NAME="movelite-${PLATFORM}"
OUT_DIR="build/${PKG_NAME}"

OS="${PLATFORM%-*}"
ARCH="${PLATFORM#*-}"

rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR/bin"
cp "$BINARY" "$OUT_DIR/bin/movelite"
chmod +x "$OUT_DIR/bin/movelite"

cat > "$OUT_DIR/package.json" <<EOF
{
  "name": "${PKG_NAME}",
  "version": "${VERSION}",
  "description": "Pre-built movelite binary for ${PLATFORM}",
  "repository": {
    "type": "git",
    "url": "https://github.com/gilbertsahumada/movelite.git"
  },
  "license": "Apache-2.0",
  "os": ["${OS}"],
  "cpu": ["${ARCH}"],
  "files": ["bin/"]
}
EOF

cat > "$OUT_DIR/README.md" <<EOF
# ${PKG_NAME}

Pre-built movelite binary for \`${PLATFORM}\`.

Installed automatically as an optional dependency of [\`movelite\`](https://www.npmjs.com/package/movelite). Do not install directly.
EOF

echo "Built ${OUT_DIR}"
