#!/usr/bin/env bash
set -euo pipefail

# Publish movelite + movelite-darwin-arm64 from a local macOS arm64 machine.
#
# This is the bootstrap path used to claim the package names on npm
# without setting up an NPM_TOKEN in GitHub Actions first. Once these
# two packages exist, you can configure Trusted Publishers on each
# and let GHA handle subsequent releases (including the linux/x64
# platform packages that this script cannot build).
#
# Usage:
#   ./scripts/publish-local.sh               # dry run, no publish
#   PUBLISH=1 ./scripts/publish-local.sh     # actually publish
#   VERSION=0.1.1 PUBLISH=1 ./scripts/publish-local.sh

VERSION="${VERSION:-0.1.0}"
PLATFORM="darwin-arm64"
PUBLISH="${PUBLISH:-0}"

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "Invalid semver version: $VERSION" >&2
  exit 1
fi

cd "$(dirname "${BASH_SOURCE[0]}")/.."

bold() { printf "\033[1m%s\033[0m\n" "$*"; }
ok()   { printf "  \033[32m✓\033[0m %s\n" "$*"; }
warn() { printf "  \033[33m⚠\033[0m %s\n" "$*"; }
fail() { printf "  \033[31m✖\033[0m %s\n" "$*"; exit 1; }

bold "→ Prerequisites"

if [ "$(uname -ms)" != "Darwin arm64" ]; then
  fail "This script targets macOS arm64. Current: $(uname -ms)"
fi
ok "platform: macOS arm64"

if ! command -v npm > /dev/null; then fail "npm not found"; fi
if ! npm whoami > /dev/null 2>&1; then
  fail "Not logged in to npm. Run: npm login"
fi
ok "npm user: $(npm whoami)"

if ! command -v cargo > /dev/null; then fail "cargo not found"; fi
ok "cargo: $(cargo --version | head -1)"

if ! git diff --quiet || ! git diff --cached --quiet; then
  warn "Working tree has uncommitted changes. The publish reads from the tree as-is."
else
  ok "working tree clean"
fi

CURRENT_BRANCH=$(git branch --show-current)
if [ "$CURRENT_BRANCH" != "main" ]; then
  warn "Not on main (current: $CURRENT_BRANCH). Continuing anyway."
else
  ok "branch: main"
fi

echo
bold "→ Build movelite binary"

if [ ! -f "target/movelite" ]; then
  warn "target/movelite not found. Building (this can take ~15 min on first run)..."
  bash build.sh
else
  ok "target/movelite exists (skip build; delete the file to force rebuild)"
fi
./target/movelite --help > /dev/null || fail "Binary smoke test failed (--help)"
ok "binary smoke test passed"

echo
bold "→ Stage platform package movelite-${PLATFORM}@${VERSION}"
bash scripts/build-npm-package.sh "$PLATFORM" "$VERSION" target/movelite > /dev/null
ok "build/movelite-${PLATFORM}/ ready"

echo
bold "→ Stage main shim movelite@${VERSION}"
node - "$VERSION" <<'NODE'
const fs = require("fs");
const path = "npm/movelite/package.json";
const version = process.argv[2];
const pkg = JSON.parse(fs.readFileSync(path, "utf8"));
pkg.version = version;
for (const k of Object.keys(pkg.optionalDependencies)) {
  pkg.optionalDependencies[k] = version;
}
fs.writeFileSync(path, JSON.stringify(pkg, null, 2) + "\n");
NODE
ok "npm/movelite/package.json bumped to ${VERSION}"

echo
bold "→ Pack contents (dry run on both packages)"
(cd "build/movelite-${PLATFORM}" && npm pack --dry-run 2>&1 | grep -E "name:|version:|size|total files")
echo
(cd npm/movelite && npm pack --dry-run 2>&1 | grep -E "name:|version:|size|total files")

echo
if [ "$PUBLISH" != "1" ]; then
  bold "DRY RUN"
  echo "  Nothing was published. To actually publish, re-run with:"
  echo "    PUBLISH=1 ./scripts/publish-local.sh"
  echo
  echo "  Note: local publish cannot attach provenance (requires CI OIDC)."
  echo "  These two packages will publish unsigned. After publish, configure"
  echo "  Trusted Publishers on npmjs.com for both, and future releases via"
  echo "  GHA will be tokenless AND provenance-signed."
  exit 0
fi

echo
bold "→ Publishing for real"
warn "About to publish. Press Ctrl-C in 5 seconds to abort."
sleep 5

echo
bold "→ Publish movelite-${PLATFORM}@${VERSION}"
(cd "build/movelite-${PLATFORM}" && npm publish --access public)
ok "movelite-${PLATFORM}@${VERSION} live"

echo
bold "→ Publish movelite@${VERSION}"
(cd npm/movelite && npm publish --access public)
ok "movelite@${VERSION} live"

echo
bold "Verify"
echo "  npm view movelite version              # expect: ${VERSION}"
echo "  npm view movelite-${PLATFORM} version  # expect: ${VERSION}"
echo
warn "Not published (need GHA + native runners or a future local publish from each platform):"
echo "  - movelite-darwin-x64@${VERSION}"
echo "  - movelite-linux-x64@${VERSION}"
echo "  - movelite-linux-arm64@${VERSION}"
echo
echo "Users on those platforms will get a clean 'missing platform package' error from"
echo "the shim and can build from source. Cover them in a follow-up release via GHA."
