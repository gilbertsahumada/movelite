#!/usr/bin/env node
const { spawn } = require("child_process");
const { dirname, join } = require("path");

const PLATFORMS = {
  "darwin-arm64": "mvlite-darwin-arm64",
  "darwin-x64": "mvlite-darwin-x64",
  "linux-x64": "mvlite-linux-x64",
  "linux-arm64": "mvlite-linux-arm64",
};

const key = `${process.platform}-${process.arch}`;
const pkg = PLATFORMS[key];

if (!pkg) {
  console.error(`mvlite: unsupported platform ${key}`);
  console.error(`Supported: ${Object.keys(PLATFORMS).join(", ")}`);
  process.exit(1);
}

let binary;
try {
  const pkgJson = require.resolve(`${pkg}/package.json`);
  binary = join(dirname(pkgJson), "bin", "mvlite");
} catch {
  console.error(`mvlite: missing platform package "${pkg}"`);
  console.error(`Reinstall mvlite or run: npm install ${pkg}`);
  process.exit(1);
}

const child = spawn(binary, process.argv.slice(2), { stdio: "inherit" });
child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  else process.exit(code ?? 1);
});
