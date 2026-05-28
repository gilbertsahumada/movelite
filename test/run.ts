import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import net from "node:net";

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, "..");
const binary = resolve(repoRoot, "target", "mvlite");

async function getFreePort(): Promise<number> {
  return new Promise((resolvePort, reject) => {
    const server = net.createServer();
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        server.close(() => reject(new Error("failed to allocate port")));
        return;
      }
      const port = address.port;
      server.close(() => resolvePort(port));
    });
    server.on("error", reject);
  });
}

async function waitForReady(url: string): Promise<void> {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`${url}/v1`);
      if (res.ok) return;
    } catch {
      // Server is still starting.
    }
    await new Promise((resolveWait) => setTimeout(resolveWait, 250));
  }
  throw new Error(`mvlite did not become ready at ${url}`);
}

async function runIntegration(env: NodeJS.ProcessEnv): Promise<number> {
  const child = spawn(process.execPath, ["./node_modules/tsx/dist/cli.mjs", "integration.ts"], {
    cwd: __dirname,
    env,
    stdio: "inherit",
  });

  return new Promise((resolveCode) => {
    child.on("exit", (code) => resolveCode(code ?? 1));
  });
}

function stop(child: ChildProcess): Promise<void> {
  return new Promise((resolveStop) => {
    if (child.exitCode !== null || child.signalCode !== null) {
      resolveStop();
      return;
    }
    child.once("exit", () => resolveStop());
    child.kill("SIGTERM");
    setTimeout(() => {
      if (child.exitCode === null && child.signalCode === null) {
        child.kill("SIGKILL");
      }
    }, 2_000).unref();
  });
}

async function main() {
  if (process.env.MVLITE_URL) {
    process.exit(await runIntegration(process.env));
  }

  if (!existsSync(binary)) {
    console.error(`Missing ${binary}. Run ./build.sh before npm test.`);
    process.exit(1);
  }

  const port = await getFreePort();
  const token = "integration-test-token";
  const sessionDir = mkdtempSync(resolve(tmpdir(), "mvlite-test-"));
  const url = `http://127.0.0.1:${port}`;

  const server = spawn(binary, [
    "start",
    "--port",
    String(port),
    "--auth-token",
    token,
    "--session-dir",
    sessionDir,
    "--reset",
  ], {
    cwd: repoRoot,
    stdio: ["ignore", "inherit", "inherit"],
  });

  try {
    await waitForReady(url);
    const code = await runIntegration({
      ...process.env,
      MVLITE_URL: url,
      MVLITE_TOKEN: token,
    });
    process.exitCode = code;
  } finally {
    await stop(server);
    rmSync(sessionDir, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
