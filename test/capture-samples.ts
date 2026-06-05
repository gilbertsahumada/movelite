// Captures the three reference trace samples movehat builds its renderer
// against: counter increment (happy path), transfer (deep tree + events), and
// an abort (partial tree). Writes them to test/samples/*.json.
// Requires test/counter compiled (see counter/README via `aptos move compile`).
import {
  Aptos,
  AptosConfig,
  Network,
  Account,
  Ed25519PrivateKey,
  generateSignedTransaction,
  type SimpleTransaction,
} from "@aptos-labs/ts-sdk";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { resolve } from "node:path";

const URL = process.env.MOVELITE_URL || "http://127.0.0.1:8090";
const TOK = process.env.MOVELITE_TOKEN;
const outDir = resolve(import.meta.dirname, "samples");
const buildDir = resolve(import.meta.dirname, "counter", "build", "counter");

const h = (e: Record<string, string> = {}) => (TOK ? { "x-movelite-token": TOK, ...e } : e);
const aptos = new Aptos(new AptosConfig({ network: Network.CUSTOM, fullnode: `${URL}/v1` }));

async function trace(tx: SimpleTransaction, signer: Account): Promise<unknown> {
  const auth = aptos.transaction.sign({ signer, transaction: tx });
  const body = generateSignedTransaction({ transaction: tx, senderAuthenticator: auth });
  const res = await fetch(`${URL}/v1/transactions/trace`, {
    method: "POST",
    headers: h({ "content-type": "application/x.aptos.signed_transaction+bcs" }),
    body,
  });
  return res.json();
}

async function main() {
  mkdirSync(outDir, { recursive: true });
  const publisher = Account.fromPrivateKey({
    privateKey: new Ed25519PrivateKey("0x" + "1".padStart(64, "0")),
  });
  const addr = publisher.accountAddress.toString();
  await fetch(`${URL}/mint?address=${addr}&amount=100000000000`, { method: "POST", headers: h() });

  // 1) counter increment — happy path
  const meta = readFileSync(resolve(buildDir, "package-metadata.bcs"));
  const mod = readFileSync(resolve(buildDir, "bytecode_modules", "counter.mv"));
  const pub = await aptos.publishPackageTransaction({
    account: publisher.accountAddress,
    metadataBytes: meta,
    moduleBytecode: [mod],
  });
  const pubAuth = aptos.transaction.sign({ signer: publisher, transaction: pub });
  const committed = await aptos.transaction.submit.simple({ transaction: pub, senderAuthenticator: pubAuth });
  await aptos.waitForTransaction({ transactionHash: committed.hash });
  const incTx = await aptos.transaction.build.simple({
    sender: publisher.accountAddress,
    data: { function: `${addr}::counter::increment`, typeArguments: [], functionArguments: [5] },
  });
  writeFileSync(resolve(outDir, "counter-increment.json"), JSON.stringify(await trace(incTx, publisher), null, 2));
  console.log("wrote counter-increment.json");

  // 2) transfer — deep tree + Withdraw/Deposit events
  const transferTx = await aptos.transaction.build.simple({
    sender: publisher.accountAddress,
    data: {
      function: "0x1::aptos_account::transfer",
      typeArguments: [],
      functionArguments: ["0x" + "42".padStart(64, "0"), 100],
    },
  });
  writeFileSync(resolve(outDir, "transfer.json"), JSON.stringify(await trace(transferTx, publisher), null, 2));
  console.log("wrote transfer.json");

  // 3) abort — counter::fail_deep aborts a few frames deep (partial tree)
  const abortTx = await aptos.transaction.build.simple({
    sender: publisher.accountAddress,
    data: { function: `${addr}::counter::fail_deep`, typeArguments: [], functionArguments: [99] },
  });
  writeFileSync(resolve(outDir, "abort.json"), JSON.stringify(await trace(abortTx, publisher), null, 2));
  console.log("wrote abort.json");
}

main().catch((e) => {
  console.error("fatal:", e);
  process.exit(1);
});
