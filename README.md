# movelite

Lightweight Move VM for local development. An anvil-like experience for Movement L1.

## What is this?

movelite embeds the AptosVM directly as a Rust library -- no consensus, no P2P networking, no full node. It exposes an Aptos-compatible REST API so existing tools (movehat, @aptos-labs/ts-sdk) can talk to it without modification.

```
Your tests (TypeScript)
    |
    | HTTP (same REST API as a real node)
    |
    v
movelite (this binary)
    |
    | direct, in-memory
    |
    v
AptosVM (Move VM)
```

## Why?

Movement's local node (`movement node run-localnet`) takes ~15 seconds to boot. movelite boots in <1 second because it skips consensus, P2P, mempool, and all the infrastructure a real validator needs. For local development and testing, you don't need any of that.

| | Movement node | movelite |
|---|---|---|
| Boot time | ~15s | <1s |
| Consensus | Full BFT | None (single-process) |
| State | Disk (RocksDB) | In-memory + JSON delta |
| Mining | Block interval | Instant |
| Network | P2P + mempool | None |

## Quick start

### Install (recommended)

```bash
npm install -g movelite
movelite start --port 8090
```

Pre-compiled binaries are published to npm for `darwin-arm64`, `darwin-x64`, `linux-x64`, and `linux-arm64`. The right binary is selected automatically via npm's `optionalDependencies`.

### Build from source

Requires Rust 1.93+ and Git.

```bash
git clone https://github.com/gilbertsahumada/movelite.git
cd movelite
./build.sh
```

The build script clones `aptos-core` (pinned to the last Apache 2.0 commit) and compiles movelite as a workspace member. First build takes ~10-15 minutes; subsequent builds are fast (~3s).

### Run

```bash
# Start with clean genesis (no network connection needed)
./target/movelite start --port 8090

# Or fork from a remote network
./target/movelite start --port 8090 --fork-url https://testnet.movementnetwork.xyz/v1
```

#### Options

| Flag | Description |
|---|---|
| `--port <u16>` | Port to listen on (default `8090`) |
| `--fork-url <url>` | Fork from a remote network's REST endpoint |
| `--fork-version <u64>` | Remote ledger version to fork from (default `0`) |
| `--chain-id <u8>` | Override the chain id reported to clients and written to genesis state |
| `--session-dir <path>` | Persist session state in this directory instead of a per-process tempdir |
| `--reset` | Delete the selected session directory before starting |
| `--auth-token <token>` | Token for movelite-only mutating endpoints (`/mint`); auto-generated and printed at startup if omitted |
| `--no-auth` | Disable local auth checks |
| `--strict-local-auth` | Also require the token for Aptos-compatible mutating endpoints (submit, trace `?commit=true`) |

Authenticated endpoints expect the token in the `x-movelite-token` header.

### Test

```bash
# Fund an account
curl -X POST "http://localhost:8090/mint?address=0x42&amount=1000000000" \
  -H "x-movelite-token: <token printed at startup>"

# Query account
curl http://localhost:8090/v1/accounts/0x1

# View function
curl -X POST http://localhost:8090/v1/view \
  -H "Content-Type: application/json" \
  -d '{"function":"0x1::coin::balance","type_arguments":["0x1::aptos_coin::AptosCoin"],"arguments":["0x42"]}'
```

## REST API

movelite implements a subset of the [Aptos REST API](https://aptos.dev/en/build/apis/fullnode-rest-api):

| Endpoint | Method | Description | Status |
|---|---|---|---|
| `/v1/` | GET | Ledger info (chain_id, version, etc.) | Done |
| `/v1/accounts/:address` | GET | Account data (sequence number, auth key) | Done |
| `/v1/accounts/:address/resources` | GET | Common framework resources subset | Done |
| `/v1/accounts/:address/resource/:type` | GET | Specific account resource | Done |
| `/v1/accounts/:address/module/:module_name` | GET | Module bytecode + ABI | Done |
| `/v1/estimate_gas_price` | GET | Gas price estimate | Done |
| `/v1/view` | POST | Execute view function (BCS or JSON args) | Done |
| `/v1/transactions` | POST | Submit signed transaction (BCS body) | Done |
| `/v1/transactions/simulate` | POST | Simulate transaction without commit | Done |
| `/v1/transactions/trace` | POST | Foundry-style execution trace (`?commit=true` to also commit) | Done |
| `/v1/transactions/by_hash/:hash` | GET | Committed transaction by hash | Done |
| `/v1/transactions/wait_by_hash/:hash` | GET | Same as `by_hash` (no polling needed; commits are synchronous) | Done |
| `/mint` | POST | Fund account (faucet, requires `x-movelite-token` by default) | Done |

Transaction endpoints accept BCS bodies only (`Content-Type: application/x.aptos.signed_transaction+bcs`); JSON transaction submission returns `415`. Responses are JSON. Requests time out after 30s and bodies are capped at 2MB.

### Execution traces

`POST /v1/transactions/trace` executes a BCS-signed transaction through an instrumented VM and returns a Foundry-style call tree (per-frame gas, args, events, abort stack). By default it runs on a throwaway clone of the session (never mutates state); pass `?commit=true` to also commit the result like a submit:

```bash
curl -X POST "http://localhost:8090/v1/transactions/trace" \
  -H "Content-Type: application/x.aptos.signed_transaction+bcs" \
  --data-binary @signed-txn.bcs
```

When `?commit=true` is used with `--strict-local-auth` enabled, the request also requires the `x-movelite-token` header (see [Options](#options) above), since committing mutates state.

## Integration with movehat

movelite is auto-detected by [movehat](https://github.com/gilbertsahumada/movehat) `>=0.2.7`. If a movelite binary is on `PATH` (or installed via `npm install movelite`), movehat spawns it instead of the Movement node:

```typescript
harness = await Harness.createLocal({ ... });
// Uses movelite if available (<1s boot), falls back to Movement node otherwise (~15s).
```

Opt out per call with `Harness.createLocal({ useMvlite: false })`.

## How it works

movelite uses the `aptos-transaction-simulation-session` crate from aptos-core. This crate:

1. **Creates a genesis state** with the full Aptos/Move framework (all `0x1::*` modules)
2. **Drives the AptosVM directly** -- `execute_transaction()`, `execute_view_function()`, `fund_account()`
3. **Persists state to disk** as JSON delta files (config.json + delta.json)

The HTTP server (axum) wraps the session behind a `Mutex` and translates REST requests into session method calls. VM work runs on a blocking thread pool so the async runtime stays responsive; requests are bounded by a 30s timeout, a 2MB body limit, and a global concurrency cap.

## License

Apache 2.0.

This project uses code from [aptos-labs/aptos-core](https://github.com/aptos-labs/aptos-core) at commit `e33e3c1b9e` (November 20, 2025), which was published under the Apache License 2.0. The subsequent license change by Aptos Foundation (November 21, 2025) does not apply retroactively to code published under Apache 2.0.

## Status

Early development. Not production-ready. Contributions welcome.

## Related

- [movehat](https://github.com/gilbertsahumada/movehat) -- Hardhat-like development framework for Movement L1
- [Anvil](https://book.getfoundry.sh/anvil/) -- The Ethereum equivalent (Foundry's local node)
- [Movement Network](https://movementnetwork.xyz/) -- The L1 blockchain movelite targets
