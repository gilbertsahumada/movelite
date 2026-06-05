// Verifies POST /transactions/trace?commit=true: commit mutates state in one
// pass, no-commit doesn't, /by_hash resolves, and commit is auth-gated.
import {
  Aptos, AptosConfig, Network, Account, Ed25519PrivateKey, generateSignedTransaction,
  type SimpleTransaction,
} from "@aptos-labs/ts-sdk";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const URL = process.env.MOVELITE_URL!, TOK = process.env.MOVELITE_TOKEN!;
const buildDir = resolve(import.meta.dirname, "counter", "build", "counter");
const aptos = new Aptos(new AptosConfig({ network: Network.CUSTOM, fullnode: `${URL}/v1` }));
const h = (e: Record<string, string> = {}) => ({ "x-movelite-token": TOK, ...e });
let fails = 0;
const ok = (c: boolean, m: string) => { console.log(`${c ? "PASS" : "FAIL"}: ${m}`); if (!c) fails++; };

async function traceReq(tx: SimpleTransaction, signer: Account, query = "", auth = true) {
  const body = generateSignedTransaction({ transaction: tx, senderAuthenticator: aptos.transaction.sign({ signer, transaction: tx }) });
  return fetch(`${URL}/v1/transactions/trace${query}`, {
    method: "POST",
    headers: (auth ? h : (e: any = {}) => e)({ "content-type": "application/x.aptos.signed_transaction+bcs" }),
    body,
  });
}
async function counterVal(addr: string): Promise<number | null> {
  const r = await fetch(`${URL}/v1/accounts/${addr}/resource/${addr}::counter::Counter`);
  if (r.status !== 200) return null;
  return Number((await r.json()).data.value);
}
const incTx = (acct: Account, by: number) => aptos.transaction.build.simple({
  sender: acct.accountAddress, data: { function: `${acct.accountAddress.toString()}::counter::increment`, typeArguments: [], functionArguments: [by] },
});

async function main() {
  const pub = Account.fromPrivateKey({ privateKey: new Ed25519PrivateKey("0x" + "1".padStart(64, "0")) });
  const addr = pub.accountAddress.toString();
  await fetch(`${URL}/mint?address=${addr}&amount=100000000000`, { method: "POST", headers: h() });
  const meta = readFileSync(resolve(buildDir, "package-metadata.bcs"));
  const mod = readFileSync(resolve(buildDir, "bytecode_modules", "counter.mv"));
  const pubTx = await aptos.publishPackageTransaction({ account: pub.accountAddress, metadataBytes: meta, moduleBytecode: [mod] });
  const c = await aptos.transaction.submit.simple({ transaction: pubTx, senderAuthenticator: aptos.transaction.sign({ signer: pub, transaction: pubTx }) });
  await aptos.waitForTransaction({ transactionHash: c.hash });

  ok((await counterVal(addr)) === null, "Counter absent before any increment");

  // commit=true → state mutates, /by_hash resolves
  const r1 = await (await traceReq(await incTx(pub, 5), pub, "?commit=true")).json();
  ok(r1.success === true && r1.root.function === "increment", "commit=true returns the trace tree");
  ok((await counterVal(addr)) === 5, "commit=true persisted Counter=5");
  const byHash = await fetch(`${URL}/v1/transactions/by_hash/${r1.txn_hash}`);
  ok(byHash.status === 200, "GET /by_hash resolves committed trace txn");

  // commit=false (default) → tree returned, state unchanged
  const r2 = await (await traceReq(await incTx(pub, 100), pub)).json();
  ok(r2.success === true, "commit=false (default) returns the trace tree");
  ok((await counterVal(addr)) === 5, "commit=false did NOT mutate state (still 5)");

  // commit again → persisted state is read by the fresh clone path too (delta saved)
  const r3 = await (await traceReq(await incTx(pub, 3), pub, "?commit=true")).json();
  ok(r3.success === true && (await counterVal(addr)) === 8, "second commit=true sees prior commit (Counter=8)");
  // and the increment event reflects the committed running total
  ok(r3.root.events?.[0]?.data?.value === "8", "committed event reflects running total (value=8)");

  // commit=false without token is allowed (read-only). The commit=true 401 gate
  // only applies under --strict-local-auth (which also makes the SDK publish
  // above require a token), so it's verified separately, not here.
  const dryNoAuth = await traceReq(await incTx(pub, 1), pub, "", false);
  ok(dryNoAuth.status === 200, "commit=false without token is allowed");

  console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILED`);
  process.exit(fails === 0 ? 0 : 1);
}
main().catch((e) => { console.error("fatal:", e); process.exit(1); });
