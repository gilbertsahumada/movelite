#!/usr/bin/env bash
set -euo pipefail

# Build movelite by compiling within the aptos-core workspace.
# This is required because the aptos-* crates have workspace-internal
# deps that can't be resolved from outside the workspace.
#
# The script:
#   1. Clones aptos-core at the last Apache 2.0 commit (shallow)
#   2. Symlinks movelite as a workspace member
#   3. Compiles with cargo build -p movelite
#   4. Copies the binary to ./target/

APTOS_CORE_DIR=".aptos-core"
APTOS_COMMIT="e33e3c1b9e8c4780b488df66fed58ee990de8b16"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "=== movelite build ==="
echo ""

# Step 1: Set up aptos-core if the source clone is not present.
#
# Gate on `.git`, not the directory: CI caches `.aptos-core/target` for build
# speed, and restoring that cache recreates `.aptos-core/` WITHOUT a clone. A
# bare directory check would then skip cloning, and the remote guard below would
# resolve `git config remote.origin.url` from movelite's parent .git instead.
# Clone in place (init + fetch) so a cache-restored `target/` is preserved.
if [ ! -d "$SCRIPT_DIR/$APTOS_CORE_DIR/.git" ]; then
    echo "Setting up aptos-core at Apache 2.0 commit $APTOS_COMMIT..."
    mkdir -p "$SCRIPT_DIR/$APTOS_CORE_DIR"
    cd "$SCRIPT_DIR/$APTOS_CORE_DIR"
    git init -q
    git remote add origin https://github.com/aptos-labs/aptos-core.git 2>/dev/null \
        || git remote set-url origin https://github.com/aptos-labs/aptos-core.git
    git fetch --depth 1 origin "$APTOS_COMMIT"
    git checkout -q FETCH_HEAD
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
# Apply movelite's trace hooks to aptos-core (idempotent). These add the
# move-vm/framework callbacks the tracing gas meter relies on (POST
# /transactions/trace). The hooks are inert default no-ops for every other gas
# meter, so this does not change normal execution. See patches/README.md.
for PATCH in "$SCRIPT_DIR"/patches/aptos-core/*.patch; do
    [ -e "$PATCH" ] || continue
    if git apply --reverse --check "$PATCH" 2>/dev/null; then
        echo "Trace patch already applied: $(basename "$PATCH")"
    elif git apply --check "$PATCH" 2>/dev/null; then
        git apply "$PATCH"
        echo "Applied trace patch: $(basename "$PATCH")"
    else
        echo "Trace patch does not apply cleanly: $(basename "$PATCH")" >&2
        echo "Clean it or remove $SCRIPT_DIR/$APTOS_CORE_DIR and rebuild." >&2
        exit 1
    fi
done

# Guard against unexpected edits to aptos-core: after the patches are applied,
# the only diff in tracked source should be the patches themselves (plus the
# Cargo manifest churn added below for the workspace member).
if ! git diff --quiet -- ':!Cargo.toml' ':!Cargo.lock' ':!third_party/move/move-vm/types/src/gas.rs' ':!third_party/move/move-vm/runtime/src/interpreter.rs' ':!aptos-move/framework/src/natives/event.rs' ':!aptos-move/aptos-transaction-simulation-session/src/session.rs'; then
    echo "Cached aptos-core has unexpected uncommitted changes outside the trace patches." >&2
    echo "Clean it or remove $SCRIPT_DIR/$APTOS_CORE_DIR." >&2
    exit 1
fi

cleanup_workspace_member() {
    cd "$SCRIPT_DIR/$APTOS_CORE_DIR"
    git restore Cargo.toml Cargo.lock >/dev/null 2>&1 || true
    rm -rf "$SCRIPT_DIR/$APTOS_CORE_DIR/movelite"
}
trap cleanup_workspace_member EXIT

# Step 2: Add movelite as workspace member (idempotent)
if ! grep -q '"movelite"' Cargo.toml; then
    # Add movelite to workspace members
    sed -i.bak 's/"vm-validator",/"vm-validator",\n    "movelite",/' Cargo.toml
    rm -f Cargo.toml.bak
    echo "Added movelite to workspace members."
fi

# Step 3: Symlink movelite src into aptos-core. movelite/Cargo.toml uses
# `../.aptos-core/aptos-move/...` for the standalone path-dep, which doubles
# up to `.aptos-core/.aptos-core/...` once copied inside the workspace.
# Rewrite during copy so the path resolves correctly from the new location.
rm -rf "$SCRIPT_DIR/$APTOS_CORE_DIR/movelite"
mkdir -p "$SCRIPT_DIR/$APTOS_CORE_DIR/movelite"
sed 's|\.\./\.aptos-core/|\.\./|g' "$SCRIPT_DIR/Cargo.toml" > "$SCRIPT_DIR/$APTOS_CORE_DIR/movelite/Cargo.toml"
ln -sf "$SCRIPT_DIR/src" "$SCRIPT_DIR/$APTOS_CORE_DIR/movelite/src"

# Step 4: Build
echo ""
echo "Building movelite..."
BUILD_LOG="$SCRIPT_DIR/target/movelite-build.log"
mkdir -p "$SCRIPT_DIR/target"
if ! cargo build -p movelite --release --features trace_patches > "$BUILD_LOG" 2>&1; then
    cat "$BUILD_LOG" >&2
    exit 1
fi
tail -5 "$BUILD_LOG"

# Step 5: Copy binary
cp "$SCRIPT_DIR/$APTOS_CORE_DIR/target/release/movelite" "$SCRIPT_DIR/target/movelite"

echo ""
echo "Build complete: ./target/movelite"
echo "Run: ./target/movelite start --port 8090"
