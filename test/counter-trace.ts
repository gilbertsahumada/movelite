// Deploy the counter package + trace an `increment` against movelite.
// Produces the real sample JSON for movehat. Requires the package compiled at
// test/counter/build (see README). Usage: MOVELITE_URL/MOVELITE_TOKEN env.
import {
  Aptos,
  AptosConfig,
  Network,
  Account,
  Ed25519PrivateKey,
  generateSignedTransaction,
} from "@aptos-labs/ts-sdk";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const MOVELITE_URL = process.env.MOVELITE_URL || "http://127.0.0.1:8090";
const MOVELITE_TOKEN = process.env.MOVELITE_TOKEN;
const buildDir = resolve(import.meta.dirname, "counter", "build", "counter");

function headers(extra: Record<string, string> = {}): Record<string, string> {
  return MOVELITE_TOKEN ? { "x-movelite-token": MOVELITE_TOKEN, ...extra } : extra;
}

async function main() {
  const config = new AptosConfig({ network: Network.CUSTOM, fullnode: `${MOVELITE_URL}/v1` });
  const aptos = new Aptos(config);

  const publisher = Account.fromPrivateKey({
    privateKey: new Ed25519PrivateKey(
      "0x0000000000000000000000000000000000000000000000000000000000000001"
    ),
  });
  const addr = publisher.accountAddress.toString();

  await fetch(`${MOVELITE_URL}/mint?address=${addr}&amount=100000000000`, {
    method: "POST",
    headers: headers(),
  });

  // --- publish the counter package ---
  const metadataBytes = readFileSync(resolve(buildDir, "package-metadata.bcs"));
  const moduleBytecode = readFileSync(resolve(buildDir, "bytecode_modules", "counter.mv"));

  const publishTx = await aptos.publishPackageTransaction({
    account: publisher.accountAddress,
    metadataBytes,
    moduleBytecode: [moduleBytecode],
  });
  const publishAuth = aptos.transaction.sign({ signer: publisher, transaction: publishTx });
  const publishCommitted = await aptos.transaction.submit.simple({
    transaction: publishTx,
    senderAuthenticator: publishAuth,
  });
  await aptos.waitForTransaction({ transactionHash: publishCommitted.hash });
  console.log(`published counter at ${addr}`);

  // --- trace an increment ---
  const incTx = await aptos.transaction.build.simple({
    sender: publisher.accountAddress,
    data: {
      function: `${addr}::counter::increment`,
      typeArguments: [],
      functionArguments: [5],
    },
  });
  const incAuth = aptos.transaction.sign({ signer: publisher, transaction: incTx });
  const bytes = generateSignedTransaction({ transaction: incTx, senderAuthenticator: incAuth });

  const res = await fetch(`${MOVELITE_URL}/v1/transactions/trace`, {
    method: "POST",
    headers: headers({ "content-type": "application/x.aptos.signed_transaction+bcs" }),
    body: bytes,
  });
  const trace = await res.json();
  console.log(`trace status=${res.status}`);
  const out = resolve(import.meta.dirname, "..", "docs", "trace-sample.json");
  writeFileSync(out, JSON.stringify(trace, null, 2));
  console.log(`saved ${out}`);
  console.log(JSON.stringify(trace, null, 2));
}

main().catch((e) => {
  console.error("fatal:", e);
  process.exit(1);
});
