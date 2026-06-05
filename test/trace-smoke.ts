// Smoke test for POST /v1/transactions/trace.
// Builds + signs a real transfer txn, sends its BCS to the trace endpoint,
// prints the returned call tree. Usage: MOVELITE_URL=... tsx trace-smoke.ts
import {
  Aptos,
  AptosConfig,
  Network,
  Account,
  Ed25519PrivateKey,
} from "@aptos-labs/ts-sdk";

const MOVELITE_URL = process.env.MOVELITE_URL || "http://127.0.0.1:8090";
const MOVELITE_TOKEN = process.env.MOVELITE_TOKEN;

function headers(extra: Record<string, string> = {}): Record<string, string> {
  return MOVELITE_TOKEN ? { "x-movelite-token": MOVELITE_TOKEN, ...extra } : extra;
}

async function main() {
  const config = new AptosConfig({ network: Network.CUSTOM, fullnode: `${MOVELITE_URL}/v1` });
  const aptos = new Aptos(config);

  const account = Account.fromPrivateKey({
    privateKey: new Ed25519PrivateKey(
      "0x0000000000000000000000000000000000000000000000000000000000000001"
    ),
  });
  const addr = account.accountAddress.toString();

  await fetch(`${MOVELITE_URL}/mint?address=${addr}&amount=10000000000`, {
    method: "POST",
    headers: headers(),
  });
  await fetch(`${MOVELITE_URL}/mint?address=0x42&amount=1`, {
    method: "POST",
    headers: headers(),
  });

  const tx = await aptos.transaction.build.simple({
    sender: account.accountAddress,
    data: {
      function: "0x1::aptos_account::transfer",
      typeArguments: [],
      functionArguments: [
        "0x0000000000000000000000000000000000000000000000000000000000000042",
        100,
      ],
    },
  });

  const senderAuthenticator = aptos.transaction.sign({ signer: account, transaction: tx });
  const signedBcs = aptos.transaction.getSigningMessage; // noop ref to keep import
  void signedBcs;
  const bytes = (aptos as any).transaction
    ? // generateSignedTransaction returns the BCS-serialized SignedTransaction
      (await import("@aptos-labs/ts-sdk")).generateSignedTransaction({
        transaction: tx,
        senderAuthenticator,
      })
    : new Uint8Array();

  const res = await fetch(`${MOVELITE_URL}/v1/transactions/trace`, {
    method: "POST",
    headers: headers({ "content-type": "application/x.aptos.signed_transaction+bcs" }),
    body: bytes,
  });

  const text = await res.text();
  console.log(`status=${res.status}`);
  try {
    console.log(JSON.stringify(JSON.parse(text), null, 2));
  } catch {
    console.log(text);
  }
}

main().catch((e) => {
  console.error("fatal:", e);
  process.exit(1);
});
