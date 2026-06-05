# CLAUDE.md

Behavioral guidelines to reduce common LLM coding mistakes. Merge with project-specific instructions as needed.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

## 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:
```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.

---

## Project-Specific Context

**What is movelite?** A lightweight Move VM binary that embeds the AptosVM directly (no consensus, no P2P) and exposes an Aptos-compatible REST API. Designed as an anvil-like tool for Movement L1 development.

### Key Architecture
```
movelite (Rust binary)
  ├── src/main.rs              # CLI entry (clap)
  ├── src/server.rs            # axum HTTP server (Aptos REST endpoints)
  └── src/session_wrapper.rs   # Mutex<Session> wrapper over aptos-transaction-simulation-session
```

### Build System

movelite depends on `aptos-transaction-simulation-session` from aptos-core. Due to workspace-internal deps that can't resolve from outside, the build runs **inside the aptos-core workspace**:

```bash
./build.sh  # clones aptos-core, adds movelite as workspace member, compiles
```

The aptos-core dependency is pinned to commit `e33e3c1b9e` — the last commit under Apache 2.0 before Aptos changed their license on Nov 21, 2025.

### Key Commands
```bash
./build.sh                          # Build the binary
./target/movelite start --port 8090   # Start server
./target/movelite version             # Show version
```

### Release process

Releases are cut by GitHub Actions (`.github/workflows/release.yml`). Two triggers:

- **`release` published** — for normal releases. Cut a GitHub Release and the
  workflow publishes the released tag (`gh release create v0.1.0 --generate-notes`).
  A bare `git push --tags` does **not** publish — only publishing a Release does.
- **`workflow_dispatch`** with a `version` input — for hotfixes or re-runs without
  a Release (builds the current default branch at the given version).

Both paths run the same matrix: build movelite on each of the 4 target platforms (`darwin-{arm64,x64}`, `linux-{x64,arm64}`) using native runners, then a `publish` job downloads the binaries and pushes one platform package per target to npm plus the main `movelite` shim package. A prerelease version (`0.2.0-rc.1`) publishes under the `next` dist-tag instead of `latest`.

Publishing authenticates via npm **Trusted Publishers** (GitHub Actions OIDC, `id-token: write`) — no `NPM_TOKEN` secret. Each of the 5 packages (`movelite` + the 4 `movelite-<platform>`) needs a Trusted Publisher on npmjs.com bound to the workflow file `release.yml`. npm provenance is enabled.

### License
Apache 2.0. Uses aptos-core code from the pre-license-change era (commit `e33e3c1b9e`, Nov 20 2025). See LICENSE for details.
