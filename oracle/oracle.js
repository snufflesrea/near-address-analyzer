// NEAR Address Analyzer: oracle service.
// Polls the contract for pending requests, fetches each address's history
// from the NearBlocks API, and calls submit_report.
//
// Run:  cd oracle && npm install && cp .env.example .env && npm start
// Needs Node 20.6+ (uses --env-file and the built-in fetch).
const { connect, keyStores, KeyPair } = require("near-api-js");

const env = (k, d) => process.env[k] ?? d;
const NETWORK = env("NEAR_NETWORK", "testnet");
const CONTRACT_ID = process.env.CONTRACT_ID;
const ORACLE_ID = process.env.ORACLE_ACCOUNT_ID;
const KEY = process.env.ORACLE_PRIVATE_KEY; // "ed25519:..."
if (!CONTRACT_ID || !ORACLE_ID || !KEY) {
  console.error("Set CONTRACT_ID, ORACLE_ACCOUNT_ID and ORACLE_PRIVATE_KEY (see .env.example)");
  process.exit(1);
}

const RPC = env("RPC_URL", `https://rpc.${NETWORK}.near.org`);
const API = env(
  "NEARBLOCKS_URL",
  NETWORK === "mainnet" ? "https://api.nearblocks.io/v1" : "https://api-testnet.nearblocks.io/v1"
);
const POLL_MS = Number(env("POLL_INTERVAL_MS", 10000));
const DELAY_MS = Number(env("REQUEST_DELAY_MS", 1500)); // be gentle with the free API tier
const MAX_PAGES = Number(env("MAX_PAGES", 20)); // 25 txns per page
const HEADERS = process.env.NEARBLOCKS_API_KEY
  ? { Authorization: `Bearer ${process.env.NEARBLOCKS_API_KEY}` }
  : {};
const THRESHOLD = 1000n * 10n ** 24n; // 1000 NEAR in yoctoNEAR (matches the contract)

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// NearBlocks v1 returns deposits as JS numbers (e.g. 1.0426e+24), so parse carefully.
const toBig = (v) => {
  try {
    if (typeof v === "string" && !/[e.]/i.test(v)) return BigInt(v);
    return BigInt(Math.trunc(Number(v) || 0));
  } catch {
    return 0n;
  }
};

async function nb(path) {
  for (let i = 0; i < 5; i++) {
    const res = await fetch(API + path, { headers: HEADERS });
    if (res.status === 404) return null;
    if (res.status === 429) {
      await sleep(DELAY_MS * 2 ** (i + 1));
      continue;
    }
    if (!res.ok) throw new Error(`NearBlocks ${res.status} for ${path}`);
    return res.json();
  }
  throw new Error("NearBlocks rate limit exceeded");
}

// Returns { createdNs, largest, large } for one address.
async function analyze(accountId) {
  const info = await nb(`/account/${accountId}`);
  let createdNs = info?.account?.[0]?.created?.block_timestamp ?? null;

  const large = new Map(); // tx hash -> amount (deduped)
  let largest = 0n;
  let oldest = null;

  for (let page = 1; page <= MAX_PAGES; page++) {
    await sleep(DELAY_MS);
    const data = await nb(`/account/${accountId}/txns?page=${page}&per_page=25&order=desc`);
    const txns = data?.txns ?? [];
    for (const t of txns) {
      if (t.outcomes?.status === false) continue; // ignore failed transactions
      const deposit = toBig(
        t.actions_agg?.deposit ?? (t.actions || []).reduce((s, a) => s + toBig(a.deposit), 0n)
      );
      if (deposit > largest) largest = deposit;
      if (deposit > THRESHOLD) large.set(t.transaction_hash, deposit);
      oldest = t.block_timestamp ?? oldest;
    }
    if (txns.length < 25) break;
    if (page === MAX_PAGES) {
      console.warn(`  ${accountId}: history truncated at ${MAX_PAGES} pages; raise MAX_PAGES for older activity`);
    }
  }

  // Fallbacks when the account endpoint has no creation info.
  if (!createdNs) createdNs = oldest ?? String(BigInt(Date.now() - 60_000) * 1_000_000n);

  const top = [...large.entries()].sort((a, b) => (b[1] > a[1] ? 1 : -1)).slice(0, 200);
  return { createdNs: String(createdNs), largest, large: top };
}

async function main() {
  const keyStore = new keyStores.InMemoryKeyStore();
  await keyStore.setKey(NETWORK, ORACLE_ID, KeyPair.fromString(KEY));
  const near = await connect({ networkId: NETWORK, nodeUrl: RPC, keyStore });
  const account = await near.account(ORACLE_ID);
  console.log(`Oracle ${ORACLE_ID} watching ${CONTRACT_ID} on ${NETWORK} (poll ${POLL_MS} ms)`);

  const retryAt = new Map(); // account -> timestamp; backoff after a failure

  for (;;) {
    try {
      const pending = await account.viewFunction({
        contractId: CONTRACT_ID,
        methodName: "get_pending_requests",
        args: { from_index: 0, limit: 20 },
      });

      for (const req of pending) {
        const id = req.account_id;
        if ((retryAt.get(id) ?? 0) > Date.now()) continue;
        try {
          console.log(`Analyzing ${id} (requested by ${req.requester})`);
          const r = await analyze(id);
          await account.functionCall({
            contractId: CONTRACT_ID,
            methodName: "submit_report",
            args: {
              account_id: id,
              created_at_ns: r.createdNs,
              largest_transfer: r.largest.toString(),
              large_transfers: r.large.map(([hash, amt]) => ({ tx_hash: hash, amount: amt.toString() })),
            },
            gas: 100n * 10n ** 12n, // 100 Tgas
            attachedDeposit: 0n,
          });
          console.log(`  reported ${id}: ${r.large.length} transfer(s) over 1000 NEAR`);
        } catch (e) {
          console.error(`  failed for ${id}: ${e.message}`);
          retryAt.set(id, Date.now() + 60_000);
        }
      }
    } catch (e) {
      console.error("poll failed:", e.message);
    }
    await sleep(POLL_MS);
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
