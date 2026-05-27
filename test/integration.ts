import {
  Aptos,
  AptosConfig,
  Network,
  Account,
  Ed25519PrivateKey,
} from "@aptos-labs/ts-sdk";

const MVLITE_URL = process.env.MVLITE_URL || "http://127.0.0.1:8090";
let passed = 0;
let failed = 0;

async function test(name: string, fn: () => Promise<void>) {
  try {
    await fn();
    console.log(`  [PASS] ${name}`);
    passed++;
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    console.log(`  [FAIL] ${name}: ${msg}`);
    failed++;
  }
}

function assert(condition: boolean, message: string) {
  if (!condition) throw new Error(message);
}

async function main() {
  console.log(`\n=== mvlite SDK integration test ===`);
  console.log(`Target: ${MVLITE_URL}\n`);

  const config = new AptosConfig({
    network: Network.CUSTOM,
    fullnode: `${MVLITE_URL}/v1`,
  });
  const aptos = new Aptos(config);

  // --- read-only tests ---

  await test("GET /v1/ ledger info", async () => {
    const info = await aptos.getLedgerInfo();
    assert(info.chain_id > 0, `chain_id should be > 0, got ${info.chain_id}`);
    console.log(`         chain_id=${info.chain_id}, version=${info.ledger_version}`);
  });

  await test("GET /v1/estimate_gas_price", async () => {
    const res = await fetch(`${MVLITE_URL}/v1/estimate_gas_price`);
    const data = await res.json();
    assert(data.gas_estimate > 0, `gas_estimate should be > 0`);
    console.log(`         gas_estimate=${data.gas_estimate}`);
  });

  await test("GET /v1/accounts/0x1 (framework account)", async () => {
    const info = await aptos.getAccountInfo({ accountAddress: "0x1" });
    assert(info.sequence_number !== undefined, "sequence_number missing");
    assert(info.authentication_key !== undefined, "authentication_key missing");
    console.log(`         seq=${info.sequence_number}`);
  });

  await test("GET /v1/accounts/0x1/resource (Account)", async () => {
    const res = await fetch(
      `${MVLITE_URL}/v1/accounts/0x1/resource/0x1::account::Account`
    );
    assert(res.status === 200, `status ${res.status}`);
    const data = await res.json();
    assert(data.data !== undefined, "data field missing");
    console.log(`         guid_creation_num=${data.data.guid_creation_num}`);
  });

  // --- faucet ---

  await test("POST /mint (fund account)", async () => {
    const res = await fetch(
      `${MVLITE_URL}/mint?address=0x42&amount=1000000000`,
      { method: "POST" }
    );
    assert(res.status === 200, `status ${res.status}`);
    const data = await res.json();
    assert(data.status === "ok", `expected ok, got ${data.status}`);
    console.log(`         funded 0x42 with 1 MOVE`);
  });

  // --- view function (BCS, via SDK) ---

  await test("POST /v1/view (BCS via SDK)", async () => {
    const result = await aptos.view({
      payload: {
        function: "0x1::coin::balance",
        typeArguments: ["0x1::aptos_coin::AptosCoin"],
        functionArguments: ["0x1"],
      },
    });
    assert(Array.isArray(result), "expected array result");
    console.log(`         result=${JSON.stringify(result)}`);
  });

  // --- transaction flow ---

  const privateKey = new Ed25519PrivateKey(
    "0x0000000000000000000000000000000000000000000000000000000000000001"
  );
  const account = Account.fromPrivateKey({ privateKey });
  const addr = account.accountAddress.toString();

  await test("Fund test account for tx", async () => {
    const res = await fetch(
      `${MVLITE_URL}/mint?address=${addr}&amount=10000000000`,
      { method: "POST" }
    );
    assert(res.status === 200, `status ${res.status}`);
    console.log(`         funded ${addr.slice(0, 10)}...`);
  });

  await test("Build transaction (SDK)", async () => {
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
    assert(tx !== undefined, "transaction build returned undefined");
    console.log(`         built successfully`);
  });

  await test("Sign + submit + wait transaction", async () => {
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

    const senderAuth = aptos.transaction.sign({
      signer: account,
      transaction: tx,
    });

    const committed = await aptos.transaction.submit.simple({
      transaction: tx,
      senderAuthenticator: senderAuth,
    });

    assert(committed.hash !== undefined, "no hash in submit response");
    console.log(`         submitted: ${committed.hash.slice(0, 16)}...`);

    const result = await aptos.waitForTransaction({
      transactionHash: committed.hash,
    });

    assert(result.success === true, `tx failed: ${result.vm_status}`);
    console.log(`         confirmed: success=${result.success}`);
  });

  // --- summary ---

  console.log(`\n=== Results: ${passed} passed, ${failed} failed ===`);
  if (failed > 0) {
    process.exit(1);
  }
}

main().catch((e) => {
  console.error("Fatal error:", e);
  process.exit(1);
});
