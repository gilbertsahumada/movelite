# movelite

Lightweight Move VM for local development — anvil-like experience for Movement L1.

## Install

```bash
npm install -g movelite
```

The installer pulls a pre-compiled binary for your platform via optional dependencies. Supported:

- `darwin-arm64` (Apple Silicon)
- `darwin-x64` (Intel Mac)
- `linux-x64`
- `linux-arm64`

## Usage

```bash
movelite start --port 8090
```

movelite listens on `127.0.0.1`. Browser requests from non-loopback origins are
blocked by default; SDKs and CLIs without an `Origin` header keep working. Use
`--strict-local-auth` to require `x-movelite-token` on Aptos-compatible mutating
endpoints too, or `--allow-external-browser-origins` if you intentionally run a
remote-hosted UI against your local server.

See [github.com/gilbertsahumada/movelite](https://github.com/gilbertsahumada/movelite) for endpoints and integration with [movehat](https://www.npmjs.com/package/movehat).

## License

Apache 2.0.

The published binary statically links code from [aptos-labs/aptos-core](https://github.com/aptos-labs/aptos-core) at commit `e33e3c1b9e` (November 20, 2025), which was released under the Apache License 2.0. The subsequent license change by Aptos Foundation (November 21, 2025) does not apply retroactively to code published under Apache 2.0.
