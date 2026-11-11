# NEAR Address Analyzer

A NEAR smart contract (Rust, `near-sdk` 5.x) that analyzes an input account address and returns:

- **Address age**: how long ago the account was created / first active
- **Suspicious transaction flag**: whether the address has any transfer worth **more than 1000 NEAR**

## Demo

See the NEAR Address Analyzer frontend in action:

<video src="assets/demo.mp4" controls width="100%"></video>

## How it works

A NEAR contract cannot read another account's history on-chain (it can't see when an account was created or what transactions it made). That data lives in the indexer layer. This project therefore uses an **oracle pattern**:

```
┌──────────────┐   1. fetch history    ┌────────────────────┐
│ NEAR Lake /  │ ────────────────────▶ │  Off-chain oracle  │
│ NearBlocks   │                       │  (indexer script)  │
└──────────────┘                       └─────────┬──────────┘
                                                 │ 2. submit_report(...)
                                                 ▼
                                       ┌────────────────────┐
   3. analyze_address(account)         │  Analyzer contract │
 ◀──────────────────────────────────── │  applies the >1000 │
        age + suspicious flag          │  NEAR rule on-chain│
                                       └────────────────────┘
```

1. The oracle looks up an account's first-activity timestamp and its transfers.
2. It calls `submit_report` on the contract.
3. The contract evaluates each transfer against the threshold itself and stores the result. The oracle supplies raw data, not the verdict.
4. Anyone can call the free `analyze_address` view method.

## Features

- Address age in seconds and days, computed live from the stored creation timestamp
- Suspicious detection: any transfer **strictly greater than 1000 NEAR**
- Count of suspicious transfers, largest transfer seen, and up to 20 flagged transaction hashes
- Owner-managed list of authorized oracles
- Creation timestamp is locked after the first report and cannot be changed by later reports

## Project structure

```
near-address-analyzer/
├── Cargo.toml
├── README.md
├── frontend/
│   └── index.html      # single-file web UI, no build step
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

## Usage

### Authorize an oracle (owner only)

```bash
near contract call-function as-transaction my-analyzer.testnet add_oracle \
  json-args '{"oracle_id":"oracle.testnet"}' \
  prepaid-gas '30 Tgas' attached-deposit '0 NEAR' \
  sign-as my-analyzer.testnet network-config testnet sign-with-keychain send
```

### Submit a report (oracle only)

Amounts are in **yoctoNEAR** (1 NEAR = 10^24 yoctoNEAR). The example below includes one transfer of 1,500 NEAR.

```bash
near contract call-function as-transaction my-analyzer.testnet submit_report \
  json-args '{
    "account_id": "alice.testnet",
    "created_at_ns": "1620000000000000000",
    "transfers": [
      { "tx_hash": "9xKq...abc", "amount": "1500000000000000000000000000" },
      { "tx_hash": "Hd3f...xyz", "amount": "5000000000000000000000000" }
    ]
  }' \
  prepaid-gas '100 Tgas' attached-deposit '0 NEAR' \
  sign-as oracle.testnet network-config testnet sign-with-keychain send
```

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

If no report has been submitted for the address, the response is `{"analyzed": false, ...}` with all other fields empty.

## Frontend

`frontend/index.html` is a single-file web UI with no build step. It calls the contract's `analyze_address` view method directly over NEAR JSON-RPC, so no wallet or login is needed.

1. Open `frontend/index.html` and set `DEFAULT_CONTRACT` to your deployed contract (or type it into the form).
2. Serve it locally:
   ```bash
   cd frontend
   python3 -m http.server 8000
   ```
3. Open http://localhost:8000, pick the network, enter an address, and click **Analyze**.

It shows the address age, a suspicious/clean badge, the transfer count, the largest transfer, and links to flagged transactions on NearBlocks. RPC endpoints are listed at the top of the script and tried in order, so you can swap in your own provider. It can be hosted on any static host (GitHub Pages, Netlify, Cloudflare Pages, and so on).

## API reference

| Method | Type | Access | Description |
|---|---|---|---|
| `new(owner_id)` | init | once | Initializes the contract; the owner is also added as an oracle |
| `add_oracle(oracle_id)` | call | owner | Authorizes an oracle account |
| `remove_oracle(oracle_id)` | call | owner | Revokes an oracle |
| `submit_report(account_id, created_at_ns, transfers)` | call | oracle | Submits new transfers for an address (max 200 per call) |
| `analyze_address(account_id)` | view | anyone | Returns age and suspicious-transaction analysis |
| `is_oracle(account_id)` | view | anyone | Checks whether an account is an authorized oracle |
| `get_owner()` | view | anyone | Returns the owner account |

### Types

```
TxSummary { tx_hash: String, amount: U128 }        // amount in yoctoNEAR

Analysis {
  account_id, analyzed,
  created_at_ns, age_seconds, age_days,
  has_suspicious_tx, suspicious_tx_count,
  largest_tx_near, flagged_tx_hashes, last_updated_ns
}
```

## Limitations and security notes

- **Trust in the oracle.** The contract can't verify the data it receives. Only authorize oracles you control or trust, and consider running more than one to cross-check.
- **No deduplication.** The oracle must submit each transfer only once. Track the last processed block per address in your indexer.
- **Batch limits.** A report holds at most 200 transfers, and only the first 20 flagged hashes are stored. Busy addresses need several reports.
- **Reports are not retroactive.** Addresses show `analyzed: false` until an oracle reports on them.
- **Scope of "suspicious".** The rule is a plain value threshold (> 1000 NEAR) and is not a fraud-detection model. It will flag legitimate large transfers too.
- This code is unaudited. Review and test it before any production use.

## Roadmap

- Off-chain oracle/indexer script (NEAR Lake or NearBlocks API)
- Owner-configurable threshold
- Unit and sandbox integration tests
- Multi-oracle quorum for report verification

## License

MIT
