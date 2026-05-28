#!/usr/bin/env bash
set -euo pipefail

# Publish mvlite + 4 platform packages to npm at the same version.
# Expects compiled binaries at artifacts/mvlite-<platform>/mvlite (CI layout).
#
# Usage: publish-npm.sh <version>

VERSION="$1"
PLATFORMS=("darwin-arm64" "darwin-x64" "linux-x64" "linux-arm64")

for platform in "${PLATFORMS[@]}"; do
  binary="artifacts/mvlite-${platform}/mvlite"
  if [ ! -f "$binary" ]; then
    echo "Missing binary: $binary" >&2
    exit 1
  fi
  bash scripts/build-npm-package.sh "$platform" "$VERSION" "$binary"
  (cd "build/mvlite-${platform}" && npm publish --access public --provenance)
done

node -e "
  const fs = require('fs');
  const path = 'npm/mvlite/package.json';
  const pkg = JSON.parse(fs.readFileSync(path, 'utf8'));
  pkg.version = '${VERSION}';
  for (const k of Object.keys(pkg.optionalDependencies)) {
    pkg.optionalDependencies[k] = '${VERSION}';
  }
  fs.writeFileSync(path, JSON.stringify(pkg, null, 2) + '\n');
"

(cd npm/mvlite && npm publish --access public --provenance)

echo "Released mvlite@${VERSION}"
