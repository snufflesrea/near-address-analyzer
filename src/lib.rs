use near_sdk::json_types::U128;
use near_sdk::serde_json::{json, Value};
use near_sdk::store::{IterableMap, LookupMap, LookupSet};
use near_sdk::{
    env, near, require, AccountId, BorshStorageKey, NearToken, PanicOnDefault, Promise,
};

const NS_PER_SECOND: u64 = 1_000_000_000;
const NS_PER_DAY: u64 = 86_400 * NS_PER_SECOND;
const MAX_TXS_PER_REPORT: usize = 200;
const MAX_FLAGGED_STORED: usize = 20;
/// A pending request can be cancelled (refunded) or replaced after this long.
const REQUEST_TTL_NS: u64 = 3_600 * NS_PER_SECOND; // 1 hour

/// A transfer is suspicious if its value is strictly greater than this.
fn threshold_yocto() -> u128 {
    NearToken::from_near(1000).as_yoctonear()
}

fn to_near_string(yocto: u128) -> String {
    let one = 10u128.pow(24);
    format!("{}.{:02}", yocto / one, (yocto % one) / 10u128.pow(22))
}

/// NEP-297 style event so oracles can watch the logs.
fn emit(event: &str, data: Value) {
    env::log_str(&format!(
        "EVENT_JSON:{}",
        json!({
            "standard": "address_analyzer",
            "version": "1.0.0",
            "event": event,
            "data": [data]
        })
    ));
}

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
enum StorageKey {
    Oracles,
    Records,
    Pending,
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

#[near(serializers = [borsh])]
#[derive(Clone)]
pub struct PendingRequest {
    requester: AccountId,
    requested_at_ns: u64,
    fee_yocto: u128,
}

/// What `analyze_address` returns.
#[near(serializers = [json])]
pub struct Analysis {
    pub account_id: AccountId,
    /// false = no report has been submitted for this address yet
    pub analyzed: bool,
    /// true = someone requested an analysis and the oracle has not answered yet
    pub pending_request: bool,
    pub created_at_ns: Option<U128>,
    pub age_seconds: Option<u64>,
    pub age_days: Option<u64>,
    pub has_suspicious_tx: Option<bool>,
    pub suspicious_tx_count: Option<u32>,
    pub largest_tx_near: Option<String>,
    pub flagged_tx_hashes: Vec<String>,
    pub last_updated_ns: Option<U128>,
}

#[near(serializers = [json])]
pub struct PendingRequestView {
    pub account_id: AccountId,
    pub requester: AccountId,
    pub requested_at_ns: U128,
    /// Fee escrowed for the oracle, in yoctoNEAR
    pub fee: U128,
    /// true = older than the TTL; the requester may cancel for a refund
    pub expired: bool,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    owner: AccountId,
    oracles: LookupSet<AccountId>,
    records: LookupMap<AccountId, AddressRecord>,
    pending: IterableMap<AccountId, PendingRequest>,
    request_fee: u128,
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
            pending: IterableMap::new(StorageKey::Pending),
            request_fee: NearToken::from_millinear(20).as_yoctonear(), // 0.02 NEAR
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

    /// Set the fee (yoctoNEAR) users must attach to request an analysis.
    pub fn set_request_fee(&mut self, fee: U128) {
        self.assert_owner();
        self.request_fee = fee.0;
    }

    // ---------- user ----------

    /// Ask the oracle to analyze `account_id`. Called by the user's wallet.
    /// Attach at least `get_request_fee()`; any excess is refunded immediately.
    /// The fee is escrowed and paid to the oracle that fulfills the request.
    /// If nobody fulfills it within the TTL, the requester can `cancel_request`.
    #[payable]
    pub fn request_analysis(&mut self, account_id: AccountId) {
        let requester = env::predecessor_account_id();
        let deposit = env::attached_deposit().as_yoctonear();
        let fee = self.request_fee;
        require!(
            deposit >= fee,
            format!("Attach at least {} yoctoNEAR to request an analysis", fee)
        );
        let now = env::block_timestamp();

        if let Some(existing) = self.pending.get(&account_id) {
            require!(
                now >= existing.requested_at_ns + REQUEST_TTL_NS,
                "An analysis is already pending for this address"
            );
            // Stale request: refund the old requester and replace it.
            let old = existing.clone();
            Promise::new(old.requester).transfer(NearToken::from_yoctonear(old.fee_yocto));
        }

        let excess = deposit - fee;
        if excess > 0 {
            Promise::new(requester.clone()).transfer(NearToken::from_yoctonear(excess));
        }

        self.pending.insert(
            account_id.clone(),
            PendingRequest {
                requester: requester.clone(),
                requested_at_ns: now,
                fee_yocto: fee,
            },
        );

        emit(
            "analysis_requested",
            json!({
                "account_id": account_id.as_str(),
                "requester": requester.as_str(),
                "fee": fee.to_string(),
                "requested_at_ns": now.to_string()
            }),
        );
    }

    /// Refund an unanswered request after the TTL. Only the original requester.
    pub fn cancel_request(&mut self, account_id: AccountId) {
        let req = match self.pending.get(&account_id) {
            Some(r) => r.clone(),
            None => env::panic_str("No pending request for this address"),
        };
        require!(
            env::predecessor_account_id() == req.requester,
            "Only the requester can cancel"
        );
        require!(
            env::block_timestamp() >= req.requested_at_ns + REQUEST_TTL_NS,
            "Request has not expired yet"
        );
        self.pending.remove(&account_id);
        Promise::new(req.requester.clone()).transfer(NearToken::from_yoctonear(req.fee_yocto));
        emit(
            "request_cancelled",
            json!({
                "account_id": account_id.as_str(),
                "requester": req.requester.as_str()
            }),
        );
    }

    // ---------- oracle ----------

    /// Called by a trusted oracle. Each report is a full SNAPSHOT that replaces
    /// the previous record, so repeated or duplicate reports are safe.
    ///
    /// - `created_at_ns`: block timestamp (ns) of the account's creation / first activity.
    /// - `largest_transfer`: the largest single transfer (yoctoNEAR) the oracle saw.
    /// - `large_transfers`: ALL transfers the oracle considers large (at most 200).
    ///   The contract re-applies the rule itself: only amounts > 1000 NEAR count.
    ///
    /// If a request is pending for this address, it is cleared and the escrowed
    /// fee is paid to the calling oracle.
    pub fn submit_report(
        &mut self,
        account_id: AccountId,
        created_at_ns: U128,
        largest_transfer: U128,
        large_transfers: Vec<TxSummary>,
    ) {
        let oracle = env::predecessor_account_id();
        require!(
            self.oracles.contains(&oracle),
            "Only an authorized oracle can submit reports"
        );
        require!(
            large_transfers.len() <= MAX_TXS_PER_REPORT,
            "Too many transfers in one report"
        );
        let now = env::block_timestamp();
        require!(
            created_at_ns.0 <= now as u128,
            "created_at_ns is in the future"
        );

        let mut largest = largest_transfer.0;
        let mut suspicious_count: u32 = 0;
        let mut flagged: Vec<String> = vec![];
        for tx in large_transfers {
            if tx.amount.0 > largest {
                largest = tx.amount.0;
            }
            if tx.amount.0 > threshold_yocto() {
                suspicious_count = suspicious_count.saturating_add(1);
                if flagged.len() < MAX_FLAGGED_STORED {
                    flagged.push(tx.tx_hash);
                }
            }
        }

        self.records.insert(
            account_id.clone(),
            AddressRecord {
                created_at_ns: created_at_ns.0 as u64,
                suspicious_count,
                largest_yocto: largest,
                flagged_hashes: flagged,
                updated_at_ns: now,
            },
        );

        // Fulfil a pending request, if any, and pay the oracle.
        if let Some(req) = self.pending.remove(&account_id) {
            Promise::new(oracle.clone()).transfer(NearToken::from_yoctonear(req.fee_yocto));
            emit(
                "analysis_completed",
                json!({
                    "account_id": account_id.as_str(),
                    "requester": req.requester.as_str(),
                    "oracle": oracle.as_str(),
                    "has_suspicious_tx": suspicious_count > 0
                }),
            );
        }
    }

    // ---------- views ----------

    /// Main entry point: returns address age and suspicious-transaction status.
    pub fn analyze_address(&self, account_id: AccountId) -> Analysis {
        let pending_request = self.pending.contains_key(&account_id);
        match self.records.get(&account_id) {
            None => Analysis {
                account_id,
                analyzed: false,
                pending_request,
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
                    pending_request,
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

    pub fn get_pending_request(&self, account_id: AccountId) -> Option<PendingRequestView> {
        self.pending
            .get(&account_id)
            .map(|r| Self::view_of(&account_id, r))
    }

    /// Oracle polling endpoint (alternative to watching event logs).
    pub fn get_pending_requests(&self, from_index: u32, limit: u32) -> Vec<PendingRequestView> {
        self.pending
            .iter()
            .skip(from_index as usize)
            .take(limit as usize)
            .map(|(id, r)| Self::view_of(id, r))
            .collect()
    }

    pub fn get_pending_count(&self) -> u32 {
        self.pending.len()
    }

    pub fn get_request_fee(&self) -> U128 {
        U128(self.request_fee)
    }

    pub fn get_request_ttl_ns(&self) -> U128 {
        U128(REQUEST_TTL_NS as u128)
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

    fn view_of(account_id: &AccountId, r: &PendingRequest) -> PendingRequestView {
        PendingRequestView {
            account_id: account_id.clone(),
            requester: r.requester.clone(),
            requested_at_ns: U128(r.requested_at_ns as u128),
            fee: U128(r.fee_yocto),
            expired: env::block_timestamp() >= r.requested_at_ns + REQUEST_TTL_NS,
        }
    }
}
