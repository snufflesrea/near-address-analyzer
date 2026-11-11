# NEAR Address Analyzer

A NEAR smart contract (Rust, `near-sdk` 5.5) that analyzes an input account address and returns:

- **Address age**: how long ago the account was created / first active
- **Suspicious transaction flag**: whether the address has any transfer worth **more than 1000 NEAR**

## How it works

A NEAR contract cannot read another account's history on-chain. That data lives in the indexer layer. This project uses a **request / fulfil oracle pattern**:

```
 User wallet                Analyzer contract                 Oracle (off-chain)
     │  1. request_analysis(addr)  │                                  │
     │  + 0.02 NEAR fee ─────────▶ │  logs "analysis_requested"       │
     │                             │ ───────────────────────────────▶ │ 2. sees request
     │                             │                                  │    (event log or
     │                             │                                  │    get_pending_requests)
     │                             │                                  │ 3. fetches history from
     │                             │                                  │    NEAR Lake / NearBlocks
     │                             │ ◀─────────────────────────────── │ 4. submit_report(...)
     │                             │  applies the >1000 NEAR rule,    │
     │                             │  stores result, pays fee to      │
     │                             │  the oracle                      │
     │  5. analyze_address(addr)   │                                  │
     │ ◀────────────────────────── │  age + suspicious flag           │
```

1. The user signs `request_analysis` from their wallet and attaches the fee (default **0.02 NEAR**). The fee is escrowed in the contract.
2. The contract emits an `analysis_requested` event and lists the request in `get_pending_requests`.
3. The oracle fetches the account's creation time and large transfers, then calls `submit_report`.
4. The contract re-applies the rule itself (a transfer **strictly greater than 1000 NEAR** is suspicious), stores the result, clears the request, and pays the escrowed fee to the oracle.
5. Anyone can call the free `analyze_address` view. While a request is open it returns `pending_request: true`, so a frontend can poll until the result appears.

If no oracle answers within **1 hour**, the requester can call `cancel_request` and get the fee back.

## Features

- Address age in seconds and days, computed live from the stored creation timestamp
- Suspicious detection: any transfer **strictly greater than 1000 NEAR**
- Count of suspicious transfers, largest transfer seen, and up to 20 flagged transaction hashes
- Pay-per-request model: the user's fee funds the oracle and deters spam
- Idempotent reports: each `submit_report` is a full snapshot that replaces the previous record, so duplicates and refreshes are safe
- Refund protection: excess deposit is returned immediately, and unanswered requests can be cancelled after the TTL
- Owner-managed list of authorized oracles and an adjustable fee

## Project structure

```
near-address-analyzer/
├── Cargo.toml
├── README.md
├── frontend/
│   └── index.html      # single-file web UI with wallet flow
├── oracle/
│   ├── oracle.js       # request-fulfilling service
│   ├── package.json
│   └── .env.example
└── src/
    └── lib.rs
```

## Prerequisites

```bash
# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# cargo-near (create, build, deploy) and NEAR CLI
cargo install --locked cargo-near
cargo install --locked near-cli-rs
```

## Build

```bash
cargo near build
```

The compiled WASM is written to `target/near/`.

## Deploy (testnet)

```bash
# 1. Create an account for the contract
near account create-account sponsor-by-faucet-service my-analyzer.testnet \
  autogenerate-new-keypair save-to-keychain network-config testnet create

# 2. Deploy and initialize
cargo near deploy build-non-reproducible-wasm my-analyzer.testnet \
  with-init-call new json-args '{"owner_id":"my-analyzer.testnet"}' \
  prepaid-gas '100 Tgas' attached-deposit '0 NEAR' \
  network-config testnet sign-with-keychain send
```

> CLI syntax changes between versions. If a command fails, run `cargo near --help` or `near --help` for the current form.

The contract account pays storage for the records it keeps (roughly 0.002 to 0.01 NEAR per analyzed address), so keep some NEAR in its balance.

## Usage

### Authorize an oracle (owner only)

```bash
near contract call-function as-transaction my-analyzer.testnet add_oracle \
  json-args '{"oracle_id":"oracle.testnet"}' \
  prepaid-gas '30 Tgas' attached-deposit '0 NEAR' \
  sign-as my-analyzer.testnet network-config testnet sign-with-keychain send
```

### Request an analysis (any user)

```bash
near contract call-function as-transaction my-analyzer.testnet request_analysis \
  json-args '{"account_id":"alice.testnet"}' \
  prepaid-gas '30 Tgas' attached-deposit '0.02 NEAR' \
  sign-as user.testnet network-config testnet sign-with-keychain send
```

### See pending requests (oracle)

```bash
near contract call-function as-read-only my-analyzer.testnet get_pending_requests \
  json-args '{"from_index":0,"limit":20}' network-config testnet now
```

Oracles can also watch transaction logs for the event:

```
EVENT_JSON:{"standard":"address_analyzer","version":"1.0.0","event":"analysis_requested","data":[{"account_id":"alice.testnet","requester":"user.testnet","fee":"20000000000000000000000","requested_at_ns":"..."}]}
```

### Submit a report (oracle only)

Amounts are in **yoctoNEAR** (1 NEAR = 10^24 yoctoNEAR). The report is a full snapshot: send all large transfers (at most 200). The example includes one transfer of 1,500 NEAR.

```bash
near contract call-function as-transaction my-analyzer.testnet submit_report \
  json-args '{
    "account_id": "alice.testnet",
    "created_at_ns": "1620000000000000000",
    "largest_transfer": "1500000000000000000000000000",
    "large_transfers": [
      { "tx_hash": "9xKq...abc", "amount": "1500000000000000000000000000" }
    ]
  }' \
  prepaid-gas '100 Tgas' attached-deposit '0 NEAR' \
  sign-as oracle.testnet network-config testnet sign-with-keychain send
```

For a clean address, send `"large_transfers": []` and the largest transfer you saw.

### Analyze an address (anyone, free)

```bash
near contract call-function as-read-only my-analyzer.testnet analyze_address \
  json-args '{"account_id":"alice.testnet"}' \
  network-config testnet now
```

Example response:

```json
{
  "account_id": "alice.testnet",
  "analyzed": true,
  "pending_request": false,
  "created_at_ns": "1620000000000000000",
  "age_seconds": 164000000,
  "age_days": 1898,
  "has_suspicious_tx": true,
  "suspicious_tx_count": 1,
  "largest_tx_near": "1500.00",
  "flagged_tx_hashes": ["9xKq...abc"],
  "last_updated_ns": "1780000000000000000"
}
```

If no report exists for the address, the response has `"analyzed": false` and all other data fields empty. `pending_request` tells you whether an analysis is on its way.

### Cancel an unanswered request (requester)

After 1 hour with no oracle response:

```bash
near contract call-function as-transaction my-analyzer.testnet cancel_request \
  json-args '{"account_id":"alice.testnet"}' \
  prepaid-gas '30 Tgas' attached-deposit '0 NEAR' \
  sign-as user.testnet network-config testnet sign-with-keychain send
```

## Oracle service

`oracle/oracle.js` is a Node.js service that fulfils requests. It polls `get_pending_requests`, fetches each address's creation time and transactions from the NearBlocks API, and calls `submit_report`. The contract pays the escrowed fee to the oracle account.

```bash
cd oracle
npm install
cp .env.example .env     # set CONTRACT_ID, ORACLE_ACCOUNT_ID, ORACLE_PRIVATE_KEY
npm start                # needs Node 20.6+
```

Before it can report, the owner must authorize the oracle account with `add_oracle`.

Notes:
- It scans up to `MAX_PAGES` pages (25 transactions each, newest first, default 20). Older history is not scanned for very busy addresses, and a warning is logged.
- The free NearBlocks tier is rate limited, so requests are spaced by `REQUEST_DELAY_MS`. Set `NEARBLOCKS_API_KEY` for higher limits.
- It uses the NearBlocks **v1** API; v3 is newer, so set `NEARBLOCKS_URL` and adjust field names if v1 is retired.
- Failed transactions are ignored. Keep `.env` private; the key controls the oracle account.

## Frontend

`frontend/index.html` is a single-file web UI with no build step.

1. Open `frontend/index.html` and set `DEFAULT_CONTRACT` to your deployed contract (or type it into the form).
2. Serve it locally:
   ```bash
   cd frontend
   python3 -m http.server 8000
   ```
3. Open http://localhost:8000, pick the network, enter an address, and click **Analyze**.

What happens next:
- **Address already analyzed:** the result shows immediately, with an option to request fresh data.
- **No data yet:** click **Connect wallet and request**. The page connects a wallet (NEAR Wallet Selector with MyNearWallet), asks you to approve `request_analysis` with the fee, then polls every 5 seconds until the oracle's report arrives and shows it.
- Wallets that redirect (like MyNearWallet) reload the page; the page remembers the pending lookup and resumes polling automatically.

Wallet Selector and its modules load from esm.sh, so the page needs internet access. Add more wallets by importing their `@near-wallet-selector/*` module and adding it to `modules`.

## API reference

| Method | Type | Access | Description |
|---|---|---|---|
| `new(owner_id)` | init | once | Initializes the contract; the owner is also added as an oracle |
| `add_oracle(oracle_id)` | call | owner | Authorizes an oracle account |
| `remove_oracle(oracle_id)` | call | owner | Revokes an oracle |
| `set_request_fee(fee)` | call | owner | Sets the request fee in yoctoNEAR |
| `request_analysis(account_id)` | call, payable | anyone | Requests an analysis; attach at least the fee, excess is refunded |
| `cancel_request(account_id)` | call | requester | Refunds an unanswered request after the 1 hour TTL |
| `submit_report(account_id, created_at_ns, largest_transfer, large_transfers)` | call | oracle | Replaces the record with a fresh snapshot, clears any pending request, and pays the fee to the oracle |
| `analyze_address(account_id)` | view | anyone | Returns age, suspicious status, and `pending_request` |
| `get_pending_request(account_id)` | view | anyone | Returns the pending request for an address, if any |
| `get_pending_requests(from_index, limit)` | view | anyone | Lists pending requests (oracle polling) |
| `get_pending_count()` | view | anyone | Number of pending requests |
| `get_request_fee()` | view | anyone | Current fee in yoctoNEAR |
| `get_request_ttl_ns()` | view | anyone | Time before a request can be cancelled |
| `is_oracle(account_id)` | view | anyone | Checks whether an account is an authorized oracle |
| `get_owner()` | view | anyone | Returns the owner account |

### Events

| Event | Emitted when |
|---|---|
| `analysis_requested` | A user requests an analysis |
| `analysis_completed` | An oracle fulfils a pending request |
| `request_cancelled` | A requester cancels an expired request |

## Limitations and security notes

- **Trust in the oracle.** The contract can't verify the data it receives. Only authorize oracles you control or trust, and consider running more than one to cross-check.
- **Oracle liveness.** If the oracle is offline, requests stay pending until the requester cancels after the TTL and gets the fee back.
- **One request per address at a time.** A second request for an address that already has a fresh pending one is rejected, and the deposit is returned.
- **Report size.** A report holds at most 200 large transfers, and only the first 20 flagged hashes are stored.
- **Storage costs.** The oracle receives the full fee, so the contract owner must keep the contract funded for the storage of stored records.
- **Scope of "suspicious".** The rule is a plain value threshold (> 1000 NEAR) and is not a fraud-detection model. It will flag legitimate large transfers too.
- This code is unaudited. Review and test it before any production use.

## Roadmap

- Cancel button in the frontend for expired requests
- Oracle on NEAR Lake for full history without API limits
- Unit and sandbox integration tests
- Owner-configurable threshold and TTL
- Multi-oracle quorum for report verification

## License

MIT
