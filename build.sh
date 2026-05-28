#!/usr/bin/env bash
set -euo pipefail

# Build mvlite by compiling within the aptos-core workspace.
# This is required because the aptos-* crates have workspace-internal
# deps that can't be resolved from outside the workspace.
#
# The script:
#   1. Clones aptos-core at the last Apache 2.0 commit (shallow)
#   2. Symlinks mvlite as a workspace member
#   3. Compiles with cargo build -p mvlite
#   4. Copies the binary to ./target/

APTOS_CORE_DIR=".aptos-core"
APTOS_COMMIT="e33e3c1b9e8c4780b488df66fed58ee990de8b16"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "=== mvlite build ==="
echo ""

# Step 1: Clone aptos-core if not present
if [ ! -d "$SCRIPT_DIR/$APTOS_CORE_DIR" ]; then
    echo "Cloning aptos-core at Apache 2.0 commit $APTOS_COMMIT..."
    git clone --depth 1 https://github.com/aptos-labs/aptos-core.git "$SCRIPT_DIR/$APTOS_CORE_DIR"
    cd "$SCRIPT_DIR/$APTOS_CORE_DIR"
    git fetch --depth 1 origin "$APTOS_COMMIT"
    git checkout "$APTOS_COMMIT"
    cd "$SCRIPT_DIR"
    echo "Done."
else
    echo "Using cached aptos-core at $APTOS_CORE_DIR"
fi

cd "$SCRIPT_DIR/$APTOS_CORE_DIR"
if [ "$(git config --get remote.origin.url)" != "https://github.com/aptos-labs/aptos-core.git" ]; then
    echo "Unexpected aptos-core remote: $(git config --get remote.origin.url)" >&2
    exit 1
fi
if [ "$(git rev-parse HEAD)" != "$APTOS_COMMIT" ]; then
    echo "Cached aptos-core is not at $APTOS_COMMIT." >&2
    echo "Current: $(git rev-parse HEAD)" >&2
    exit 1
fi
if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "Cached aptos-core has uncommitted changes. Clean it or remove $SCRIPT_DIR/$APTOS_CORE_DIR." >&2
    exit 1
fi

cleanup_workspace_member() {
    cd "$SCRIPT_DIR/$APTOS_CORE_DIR"
    git restore Cargo.toml Cargo.lock >/dev/null 2>&1 || true
    rm -rf "$SCRIPT_DIR/$APTOS_CORE_DIR/mvlite"
}
trap cleanup_workspace_member EXIT

# Step 2: Add mvlite as workspace member (idempotent)
if ! grep -q '"mvlite"' Cargo.toml; then
    # Add mvlite to workspace members
    sed -i.bak 's/"vm-validator",/"vm-validator",\n    "mvlite",/' Cargo.toml
    rm -f Cargo.toml.bak
    echo "Added mvlite to workspace members."
fi

# Step 3: Symlink mvlite src into aptos-core. mvlite/Cargo.toml uses
# `../.aptos-core/aptos-move/...` for the standalone path-dep, which doubles
# up to `.aptos-core/.aptos-core/...` once copied inside the workspace.
# Rewrite during copy so the path resolves correctly from the new location.
rm -rf "$SCRIPT_DIR/$APTOS_CORE_DIR/mvlite"
mkdir -p "$SCRIPT_DIR/$APTOS_CORE_DIR/mvlite"
sed 's|\.\./\.aptos-core/|\.\./|g' "$SCRIPT_DIR/Cargo.toml" > "$SCRIPT_DIR/$APTOS_CORE_DIR/mvlite/Cargo.toml"
ln -sf "$SCRIPT_DIR/src" "$SCRIPT_DIR/$APTOS_CORE_DIR/mvlite/src"

# Step 4: Build
echo ""
echo "Building mvlite..."
BUILD_LOG="$SCRIPT_DIR/target/mvlite-build.log"
mkdir -p "$SCRIPT_DIR/target"
if ! cargo build -p mvlite --release > "$BUILD_LOG" 2>&1; then
    cat "$BUILD_LOG" >&2
    exit 1
fi
tail -5 "$BUILD_LOG"

# Step 5: Copy binary
cp "$SCRIPT_DIR/$APTOS_CORE_DIR/target/release/mvlite" "$SCRIPT_DIR/target/mvlite"

echo ""
echo "Build complete: ./target/mvlite"
echo "Run: ./target/mvlite start --port 8090"
