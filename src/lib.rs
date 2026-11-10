use near_sdk::json_types::U128;
use near_sdk::store::{LookupMap, LookupSet};
use near_sdk::{env, near, require, AccountId, BorshStorageKey, NearToken, PanicOnDefault};

const NS_PER_SECOND: u64 = 1_000_000_000;
const NS_PER_DAY: u64 = 86_400 * NS_PER_SECOND;
const MAX_TXS_PER_REPORT: usize = 200;
const MAX_FLAGGED_STORED: usize = 20;

/// A transaction is suspicious if its value is strictly greater than this.
fn threshold_yocto() -> u128 {
    NearToken::from_near(1000).as_yoctonear()
}

fn to_near_string(yocto: u128) -> String {
    let one = 10u128.pow(24);
    format!("{}.{:02}", yocto / one, (yocto % one) / 10u128.pow(22))
}

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Oracles,
    Records,
}

/// Summary of one transfer, submitted by the oracle/indexer.
#[near(serializers = [borsh, json])]
#[derive(Clone)]
pub struct TxSummary {
    pub tx_hash: String,
    /// Value in yoctoNEAR
    pub amount: U128,
}

#[near(serializers = [borsh])]
#[derive(Clone)]
pub struct AddressRecord {
    created_at_ns: u64,
    suspicious_count: u32,
    largest_yocto: u128,
    flagged_hashes: Vec<String>,
    updated_at_ns: u64,
}

/// What `analyze_address` returns.
#[near(serializers = [json])]
pub struct Analysis {
    pub account_id: AccountId,
    /// false = no report has been submitted for this address yet
    pub analyzed: bool,
    pub created_at_ns: Option<U128>,
    pub age_seconds: Option<u64>,
    pub age_days: Option<u64>,
    pub has_suspicious_tx: Option<bool>,
    pub suspicious_tx_count: Option<u32>,
    pub largest_tx_near: Option<String>,
    pub flagged_tx_hashes: Vec<String>,
    pub last_updated_ns: Option<U128>,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    owner: AccountId,
    oracles: LookupSet<AccountId>,
    records: LookupMap<AccountId, AddressRecord>,
}

#[near]
impl Contract {
    #[init]
    pub fn new(owner_id: AccountId) -> Self {
        let mut oracles = LookupSet::new(StorageKey::Oracles);
        oracles.insert(owner_id.clone());
        Self {
            owner: owner_id,
            oracles,
            records: LookupMap::new(StorageKey::Records),
        }
    }

    // ---------- admin ----------

    pub fn add_oracle(&mut self, oracle_id: AccountId) {
        self.assert_owner();
        self.oracles.insert(oracle_id);
    }

    pub fn remove_oracle(&mut self, oracle_id: AccountId) {
        self.assert_owner();
        self.oracles.remove(&oracle_id);
    }

    // ---------- oracle ----------

    /// Called by a trusted indexer.
    /// - `created_at_ns`: block timestamp (ns) of the account's first activity/creation.
    ///   Only the first report sets it; later reports cannot change it.
    /// - `transfers`: NEW transfers since the last report (the contract does not dedupe).
    /// The contract itself decides which transfers are suspicious (> 1000 NEAR).
    pub fn submit_report(
        &mut self,
        account_id: AccountId,
        created_at_ns: U128,
        transfers: Vec<TxSummary>,
    ) {
        require!(
            self.oracles.contains(&env::predecessor_account_id()),
            "Only an authorized oracle can submit reports"
        );
        require!(
            transfers.len() <= MAX_TXS_PER_REPORT,
            "Too many transfers in one report"
        );

        let now = env::block_timestamp();
        let mut rec = self
            .records
            .get(&account_id)
            .cloned()
            .unwrap_or(AddressRecord {
                created_at_ns: created_at_ns.0 as u64,
                suspicious_count: 0,
                largest_yocto: 0,
                flagged_hashes: vec![],
                updated_at_ns: now,
            });

        for tx in transfers {
            let amount = tx.amount.0;
            if amount > rec.largest_yocto {
                rec.largest_yocto = amount;
            }
            if amount > threshold_yocto() {
                rec.suspicious_count = rec.suspicious_count.saturating_add(1);
                if rec.flagged_hashes.len() < MAX_FLAGGED_STORED {
                    rec.flagged_hashes.push(tx.tx_hash);
                }
            }
        }
        rec.updated_at_ns = now;
        self.records.insert(account_id, rec);
    }

    // ---------- views ----------

    /// Main entry point: returns address age and suspicious-transaction status.
    pub fn analyze_address(&self, account_id: AccountId) -> Analysis {
        match self.records.get(&account_id) {
            None => Analysis {
                account_id,
                analyzed: false,
                created_at_ns: None,
                age_seconds: None,
                age_days: None,
                has_suspicious_tx: None,
                suspicious_tx_count: None,
                largest_tx_near: None,
                flagged_tx_hashes: vec![],
                last_updated_ns: None,
            },
            Some(r) => {
                let age_ns = env::block_timestamp().saturating_sub(r.created_at_ns);
                Analysis {
                    account_id,
                    analyzed: true,
                    created_at_ns: Some(U128(r.created_at_ns as u128)),
                    age_seconds: Some(age_ns / NS_PER_SECOND),
                    age_days: Some(age_ns / NS_PER_DAY),
                    has_suspicious_tx: Some(r.suspicious_count > 0),
                    suspicious_tx_count: Some(r.suspicious_count),
                    largest_tx_near: Some(to_near_string(r.largest_yocto)),
                    flagged_tx_hashes: r.flagged_hashes.clone(),
                    last_updated_ns: Some(U128(r.updated_at_ns as u128)),
                }
            }
        }
    }

    pub fn is_oracle(&self, account_id: AccountId) -> bool {
        self.oracles.contains(&account_id)
    }

    pub fn get_owner(&self) -> AccountId {
        self.owner.clone()
    }
}

impl Contract {
    fn assert_owner(&self) {
        require!(
            env::predecessor_account_id() == self.owner,
            "Only the owner can call this method"
        );
    }
}
