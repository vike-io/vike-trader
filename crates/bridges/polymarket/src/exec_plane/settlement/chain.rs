//! `chain` — the **Polygon on-chain settlement watcher**: the missing oracle that explains a
//! Polymarket position → cash movement no CLOB endpoint ever reports.
//!
//! ## The blind spot this closes
//! [`crate::exec_plane::recon_client`]'s module doc states it: Polymarket settlement is a two-step ON-CHAIN
//! flow — a market RESOLVES, then winning tokens are REDEEMED for USDC — and **redemption produces
//! no `/data/trades` fill**. Reconcile therefore sees a position vanish and cash appear with
//! nothing explaining it, which is exactly why the venue shipped `quarantine`-first (#646). And
//! [`crate::exec_plane::settlement::resolve`]'s doc states the sibling gap: `/positions.redeemable` alone cannot tell a
//! resolved LOSER from a position still trading.
//!
//! Both gaps have the same cure: read the settlement facts from the chain that produced them. This
//! module is a MINIMAL JSON-RPC reader over the crate's existing `ureq` stack — **no `ethers`, no
//! `alloy`, no `web3`, no new workspace dependency of any kind** (the `deny.toml` `[bans]` gate
//! rejects a second HTTP/TLS/curve stack, and `hex`/`serde_json`/`ureq` were already this crate's
//! deps). It READS only: `eth_blockNumber` / `eth_call` / `eth_getLogs` / `eth_getTransactionReceipt`.
//! It never signs and never sends a transaction — that stays [`crate::exec_plane::settlement::redeem_relayer`]'s job.
//!
//! ## ⚠ It is OPT-IN and DEFAULT-OFF
//! [`ChainWatchPoller::spawn`] starts the watcher thread only when its caller passes `enabled`, and
//! NO composition root starts it — decision 0095's D4: code nothing starts takes its values as
//! PARAMETERS and gets no settings row until something starts it. What a reader dials is a
//! [`ChainRpcSettings`], supplied by that caller too. Not started (the only state today) ⇒ nothing
//! is constructed, no socket is opened, and every consumer seam below falls back to its
//! pre-existing behaviour byte-for-byte. (The `POLY_CHAIN_WATCH`/`POLY_CHAIN_RPC_URL`/
//! `POLY_CHAIN_MAX_SPAN`/`POLY_CHAIN_PROXY` variables and this module's own walk of the credential
//! store were how it was configured before that decision; a set variable refuses startup now.)
//!
//! ## The event signatures were DERIVED, then verified THREE ways (never guessed)
//! A wrong `topic0` matches nothing, which looks exactly like "no redemptions happened" — so each
//! one below was (1) keccak-derived from the canonical contract source's declaration, (2) probed
//! against the DEPLOYED runtime bytecode (`eth_getCode`, the same method PR #647 used to pin
//! `convertPositions`), and (3) decoded against REAL Polygon mainnet logs whose ABI head/tail
//! offsets self-check. All three agreed for every signature (2026-07-23, mainnet):
//!
//! | contract | event (canonical signature) | topic0 | in bytecode | live logs decoded |
//! |---|---|---|---|---|
//! | CTF [`crate::exec_plane::settlement::redeem::CTF_ADDRESS`] | `ConditionResolution(bytes32,address,bytes32,uint256,uint256[])` | `b44d84d3…` | YES | 12 |
//! | CTF | `PayoutRedemption(address,address,bytes32,bytes32,uint256[],uint256)` | `2682012a…` | YES | 244 |
//! | NegRiskAdapter [`crate::exec_plane::settlement::redeem::NEG_RISK_ADAPTER`] | `PayoutRedemption(address,bytes32,uint256[],uint256)` | `9140a6a2…` | YES | 33 |
//! | CTF (ERC-1155) | `TransferSingle(address,address,address,uint256,uint256)` | `c3d58168…` | YES | ✓ |
//! | CTF (ERC-1155) | `TransferBatch(address,address,address,uint256[],uint256[])` | `4a39dc06…` | YES | ✓ |
//!
//! The two ERC-1155 topics are the universally-published constants, which is what validates the
//! keccak methodology itself (they reproduce byte-for-byte from the signature literals here).
//! Indexed-ness is NOT encoded in `topic0`, so it was pinned from the live logs' topic COUNT and
//! cross-checked: the NegRiskAdapter's `topic2` was proven to be the **conditionId** because the
//! CTF's own `PayoutRedemption` in the SAME transaction carries that exact bytes32 in its
//! unambiguous `conditionId` data word (tx `0xc6c3f62a…`).
//!
//! The three view-function selectors are pinned the same way — keccak-derived and each found as a
//! `PUSH4` immediate in the CTF dispatch table: `payoutDenominator(bytes32)` = `dd34de67`,
//! `payoutNumerators(bytes32,uint256)` = `0504c814`, `getOutcomeSlotCount(bytes32)` = `d42dc0c2`.
//!
//! ## ⚠ THE TRAP: the on-chain `redeemer` is NOT our wallet
//! Filtering `PayoutRedemption` by `topics[1] == funder` finds **nothing** for a Polymarket
//! account, and "nothing" is indistinguishable from "no redemptions". Verified live against this
//! repo's own mainnet account: its largest redemption (`0xafc036c2…`, 15.62 USDC, conditionId
//! `0x8241ea50…`) carries `redeemer = 0xada100db00ca00073811820692005400218fce1f` — a SHARED
//! Polymarket relayer proxy, not the funder. The funder's own tokens move in a sibling ERC-1155
//! `TransferBatch` in the same transaction.
//!
//! So the account-scoped anchor is the **ERC-1155 transfer OUT of the funder** (`topics[2] ==
//! funder`), joined by `transactionHash` to the `PayoutRedemption` in the same transaction. That
//! join is what [`ChainWatcher::poll_once`] performs, and it is why this module scans transfers
//! rather than redemptions.
//!
//! ## Two independent readers, for two different jobs
//! 1. **`eth_call` payout numerators** ([`PolygonRpc::condition_resolution`]) — a point query per
//!    conditionId we HOLD. Range-free (works on any RPC, no log-window cap, no archive node), and
//!    it is the AUTHORITATIVE winner source. This is what [`crate::exec_plane::settlement::resolve`] consumes.
//! 2. **`eth_getLogs` redemption scan** ([`ChainWatcher`]) — the account-scoped ledger of realised
//!    settlements, with the exact USDC payout. This is what [`crate::exec_plane::recon_client`] consumes, as
//!    settlement `FillReport`s that make a redeem-shaped divergence EXPLAINED instead of
//!    quarantined.
//!
//! ## ⚠ FINDING: `/positions.redeemable` does NOT mean "won" (a live refutation)
//! [`crate::exec_plane::settlement::resolve`]'s winner rule assumes the data-api flags only the WINNING leg. A live read of
//! this repo's mainnet account (2026-07-23) refutes it: **all four** held positions came back
//! `redeemable: true`, three of them with `curPrice: 0` / `cashPnl: -100%` — plain losers. Each
//! wallet holds ONE leg per condition, so `resolve::ambiguous_conditions` (which needs 2+ redeemable
//! legs of the SAME condition) does not fire, and `resolve::winning_tokens` would have settled all
//! four at 1.0 — **fabricating ≈29 USDC of profit that never existed**. `eth_call` agreed with the
//! data-api's `curPrice` on 4/4:
//!
//! | market | held index | chain numerators | verdict |
//! |---|---|---|---|
//! | SOL Up/Down (`0x13bf6efb…`) | 1 | `[1, 0]` | LOSER |
//! | DOGE Up/Down (`0xbf336239…`) | 0 | `[0, 1]` | LOSER |
//! | DOGE Up/Down (`0x5a71ff88…`) | 0 | `[0, 1]` | LOSER |
//! | BNB Up/Down (`0xf361b0aa…`) | 1 | `[0, 1]` | WINNER |
//!
//! `redeemable` is therefore best read as "this condition RESOLVED and the position can be
//! redeemed (possibly for zero)" — a resolution flag, not a winner flag. That makes the chain read
//! the only correct payout source, and [`crate::exec_plane::settlement::resolve`]'s chain seam a money-safety fix, not just
//! a completeness one.
//!
//! ## Phase C: the shared decode library (drift unification)
//! The 5 decoders above were built for this watcher's own narrow settlement need; a SEPARATE
//! Python on-chain daemon (`vike_db_data_jobs/apps/polymarket/onchain_decode.py`) independently
//! keccak-derived and decodes a 12-event superset of the SAME contracts/topics — a duplicated,
//! drifting decode surface. The section below (search "Phase C decoders") extends this module with
//! the events Rust was missing — `OrderFilled` (both the legacy V1 8-param and the unified V2
//! 10-param ABI), CTF `PositionSplit`/`PositionsMerge`, NegRiskAdapter `PositionsConverted`, and
//! the USDC.e ERC-20 `Transfer` funding leg — so a future Rust collector shares ONE decode instead
//! of re-deriving/re-verifying these layouts a second time. Every new decoder mirrors
//! `onchain_decode.py`'s semantics exactly (word offsets, side/role assignment, 6-dp USDC scaling)
//! and is fixture-tested against a REAL mainnet log, the same discipline as the 5 above. Purely
//! additive: nothing here is wired into [`ChainWatcher`]/[`ChainOracle`], so the settlement
//! watcher's behavior is unchanged byte-for-byte.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use vike_bridge_core::poller::{STOP_POLL_SLICE, StopHandle, sleep_stop_aware, spawn_poller};

use crate::exec_plane::settlement::redeem::CTF_ADDRESS;

mod abi;
mod decode;
mod rpc;

use abi::clean;
pub use abi::{data_words, u256_word_to_decimal, word_address, word_u128};
pub use decode::{
    FillRole, OrderFill, OrderFillAbi, PositionEvent, PositionEventKind, PositionsConverted,
    TOPIC_CTF_POSITION_MERGE, TOPIC_CTF_POSITION_SPLIT, TOPIC_NEG_RISK_POSITIONS_CONVERTED,
    TOPIC_ORDER_FILLED_V1, TOPIC_ORDER_FILLED_V2, TOPIC_USDC_TRANSFER, UsdcTransfer,
    V1_EXCHANGE_NEG, V1_EXCHANGE_STD, V2_EXCHANGE, decode_condition_resolution,
    decode_ctf_position_merge, decode_ctf_position_split, decode_ctf_redemption,
    decode_neg_risk_redemption, decode_order_fill_v1, decode_order_fill_v2,
    decode_positions_converted, decode_redemption, decode_token_transfer, decode_uint_result,
    decode_usdc_transfer,
};
pub use rpc::{PolygonRpc, join_settlement};

// ---------------------------------------------------------------------------------------------
// Pinned wire constants (see the module doc for how each was derived and verified)
// ---------------------------------------------------------------------------------------------

/// `ConditionResolution(bytes32 indexed conditionId, address indexed oracle, bytes32 indexed
/// questionId, uint outcomeSlotCount, uint[] payoutNumerators)` on the CTF.
pub const TOPIC_CONDITION_RESOLUTION: &str =
    "0xb44d84d3289691f71497564b85d4233648d9dbae8cbdbb4329f301c3a0185894";

/// `PayoutRedemption(address indexed redeemer, IERC20 indexed collateralToken, bytes32 indexed
/// parentCollectionId, bytes32 conditionId, uint[] indexSets, uint payout)` on the CTF.
pub const TOPIC_CTF_PAYOUT_REDEMPTION: &str =
    "0x2682012a4a4f1973119f1c9b90745d1bd91fa2bab387344f044cb3586864d18d";

/// `PayoutRedemption(address indexed redeemer, bytes32 indexed conditionId, uint256[] amounts,
/// uint256 payout)` on the NegRiskAdapter.
pub const TOPIC_NEG_RISK_PAYOUT_REDEMPTION: &str =
    "0x9140a6a270ef945260c03894b3c6b3b2695e9d5101feef0ff24fec960cfd3224";

/// ERC-1155 `TransferSingle(address,address,address,uint256,uint256)`.
pub const TOPIC_TRANSFER_SINGLE: &str =
    "0xc3d58168c5ae7397731d063d5bbf3d657854427343f4c083240f7aacaa2d0f62";

/// ERC-1155 `TransferBatch(address,address,address,uint256[],uint256[])`.
pub const TOPIC_TRANSFER_BATCH: &str =
    "0x4a39dc06d4c0dbc64b70af90fd698a233a518aa5d07e595d983b8c0526c8f7fb";

/// `payoutDenominator(bytes32)` — `0` until the condition resolves.
pub const SEL_PAYOUT_DENOMINATOR: &str = "0xdd34de67";
/// `payoutNumerators(bytes32,uint256)` — the per-outcome-slot payout weight.
pub const SEL_PAYOUT_NUMERATORS: &str = "0x0504c814";
/// `getOutcomeSlotCount(bytes32)` — how many slots the condition has (2 for a binary market).
pub const SEL_OUTCOME_SLOT_COUNT: &str = "0xd42dc0c2";

/// USDC / CTF outcome tokens are 6-decimal, so a wire base-unit value divides by this.
const USDC_DECIMALS: f64 = 1_000_000.0;

/// Tolerance on the `Σ(qty · price) == payout` identity [`join_settlement`] proves its slot mapping
/// with — half a base unit (5e-7 USDC), i.e. below anything the 6-decimal wire can express.
pub const PAYOUT_EPSILON: f64 = 5e-7;

/// Polygon PoS. Named here for the health check only — the `137` constants in [`crate::exec_plane::l1`] /
/// [`crate::exec_plane::order`] / [`crate::exec_plane::settlement::redeem_relayer`] are EIP-712 signing domains and are unrelated.
pub const POLYGON_CHAIN_ID: u64 = 137;

/// The default JSON-RPC endpoint: keyless, public, and — verified 2026-07-23 — serving all four
/// methods this module uses INCLUDING archive receipts. Deliberately NOT a keyed provider URL; a
/// caller points [`ChainRpcSettings::rpc_url`] at its own node or paid endpoint.
///
/// (`polygon-rpc.com`, the historical default, now answers `API key disabled` and is unusable;
/// `polygon-bor-rpc.publicnode.com` and `1rpc.io/matic` serve calls and recent logs but refuse
/// archive receipts.)
pub const DEFAULT_RPC_URL: &str = "https://polygon.drpc.org";

/// Conservative default `eth_getLogs` window. `1rpc.io` caps at 50 blocks, `drpc` at 10 000; 45
/// blocks (~90 s of Polygon) is under every free cap seen and is far wider than one poll interval.
pub const DEFAULT_MAX_SPAN: u64 = 45;

/// Default watcher cadence — settlement is a human-timescale event, mirroring
/// [`crate::exec_plane::settlement::resolve::DEFAULT_POLL_INTERVAL`].
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// How far back the FIRST poll looks when no cursor exists yet (~10 min of Polygon).
pub const DEFAULT_COLD_LOOKBACK_BLOCKS: u64 = 300;

/// **What a Polygon JSON-RPC reader dials** — supplied by whichever composition root starts the
/// settlement code (D4 of decision 0095: code nothing starts takes its values as PARAMETERS and gets
/// no settings row until something starts it). `Default` is the documented constants.
///
/// ⚠ `Debug` is written by hand: a paid endpoint's URL can embed its key, so it prints the URL only
/// when it is the keyless public [`DEFAULT_RPC_URL`].
#[derive(Clone, PartialEq, Eq)]
pub struct ChainRpcSettings {
    /// The JSON-RPC endpoint. A paid endpoint's URL can embed its key — never log it whole.
    pub rpc_url: String,
    /// Max blocks per `eth_getLogs` window (free endpoints cap it hard; the scan CHUNKS anything
    /// wider rather than failing). `0` means the default.
    pub max_span: u64,
    /// Route the RPC through the venue's declared egress ([`crate::egress::proxy_url`]). Off by
    /// default: Polygon RPCs are not geo-blocked, so the tunnel is only wanted on a host whose DNS
    /// is (the UA case [`crate::exec_plane::exec`] documents).
    pub via_proxy: bool,
}

impl Default for ChainRpcSettings {
    fn default() -> Self {
        ChainRpcSettings {
            rpc_url: DEFAULT_RPC_URL.to_string(),
            max_span: DEFAULT_MAX_SPAN,
            via_proxy: false,
        }
    }
}

impl std::fmt::Debug for ChainRpcSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rpc_url = if self.rpc_url == DEFAULT_RPC_URL { DEFAULT_RPC_URL } else { "<redacted>" };
        f.debug_struct("ChainRpcSettings")
            .field("rpc_url", &rpc_url)
            .field("max_span", &self.max_span)
            .field("via_proxy", &self.via_proxy)
            .finish()
    }
}

// ---------------------------------------------------------------------------------------------
// Decoded types
// ---------------------------------------------------------------------------------------------

/// Which contract settled a redemption — it selects the event layout, and mirrors
/// [`crate::exec_plane::settlement::redeem_relayer::RedeemKind`]'s binary/neg-risk split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedeemVenue {
    /// The Gnosis ConditionalTokens contract ([`CTF_ADDRESS`]) — a binary market redeem.
    Ctf,
    /// The [`NEG_RISK_ADAPTER`] — a multi-outcome (neg-risk) redeem.
    NegRisk,
}

/// One decoded `PayoutRedemption`. `payout_usdc` is the REALISED collateral in whole USDC — the
/// number that explains the cash half of a settlement.
#[derive(Debug, Clone, PartialEq)]
pub struct Redemption {
    pub venue: RedeemVenue,
    /// The on-chain `msg.sender`. ⚠ For a Polymarket account this is a shared relayer proxy, NOT
    /// the funder — see the module doc's TRAP section.
    pub redeemer: String,
    pub condition_id: String,
    pub payout_usdc: f64,
    /// CTF: the `indexSets` argument (`[1, 2]` for a binary redeem). NegRisk: the per-slot AMOUNTS
    /// (base units), the semantics [`crate::exec_plane::settlement::redeem`] pins from the contract source.
    pub slot_values: Vec<u128>,
    pub tx_hash: String,
    pub block: u64,
    /// Block timestamp in ms when the RPC supplies `blockTimestamp` (most do), else `0`.
    pub ts_ms: i64,
}

/// One decoded `ConditionResolution` log. The same payout vector [`PolygonRpc::condition_resolution`]
/// reads via `eth_call`, in event form — useful for a historical scan, not needed for the live path.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolutionEvent {
    pub condition_id: String,
    pub oracle: String,
    pub question_id: String,
    pub outcome_slot_count: u128,
    pub payout_numerators: Vec<u128>,
    pub tx_hash: String,
    pub block: u64,
}

/// One decoded ERC-1155 transfer (`TransferSingle` normalised into the batch shape). `ids` are
/// DECIMAL token-id strings — the same spelling as a `/positions.asset` and therefore the vike
/// `symbol`.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenTransfer {
    pub operator: String,
    pub from: String,
    pub to: String,
    pub ids: Vec<String>,
    pub values: Vec<u128>,
    pub tx_hash: String,
    pub block: u64,
}

/// The chain's verdict on one condition, read from the CTF's own payout mappings. `denominator == 0`
/// means NOT RESOLVED — the CTF's own sentinel (`payoutDenominator` is written only by
/// `reportPayouts`).
#[derive(Debug, Clone, PartialEq)]
pub struct ChainResolution {
    pub condition_id: String,
    pub denominator: u128,
    pub numerators: Vec<u128>,
}

impl ChainResolution {
    /// The CTF's own resolution test.
    pub fn is_resolved(&self) -> bool {
        self.denominator > 0 && !self.numerators.is_empty()
    }

    /// The payout of ONE outcome slot, in collateral per token: `numerator / denominator`. `None`
    /// when unresolved or the index is out of range — never a guessed 0.0, because "unknown" and
    /// "worthless" must not be the same value in a settlement path.
    ///
    /// A binary market pays `[1,0]` or `[0,1]` (⇒ 1.0 / 0.0, matching
    /// [`crate::exec_plane::settlement::resolve::WINNER_PAYOUT`]/[`LOSER_PAYOUT`](crate::exec_plane::settlement::resolve::LOSER_PAYOUT)); a SPLIT
    /// resolution (`[1,1]` over denominator 2) pays 0.5 to BOTH legs, which this expresses exactly
    /// and the `redeemable`-flag heuristic cannot express at all.
    pub fn payout_for_index(&self, index: u32) -> Option<f64> {
        if !self.is_resolved() {
            return None;
        }
        let n = *self.numerators.get(index as usize)?;
        Some(n as f64 / self.denominator as f64)
    }

    /// The single winning slot when there is exactly one — `None` for unresolved or split payouts.
    pub fn winner_index(&self) -> Option<u32> {
        if !self.is_resolved() {
            return None;
        }
        let mut winner = None;
        for (i, n) in self.numerators.iter().enumerate() {
            if *n > 0 {
                if winner.is_some() {
                    return None; // split payout: no single winner
                }
                winner = Some(i as u32);
            }
        }
        winner
    }
}

/// One realised, account-scoped settlement: the join of a funder token OUTFLOW with the
/// `PayoutRedemption` in the same transaction. This is the row that EXPLAINS a position → cash
/// movement, and the row [`crate::exec_plane::recon_client`] turns into a settlement `FillReport`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChainSettlement {
    pub condition_id: String,
    /// The ERC-1155 outcome token (decimal) — the vike `symbol` for this venue.
    pub token_id: String,
    /// Tokens redeemed, in whole shares (base units / 1e6).
    pub qty: f64,
    /// Collateral per token this leg realised: `payout_for_index` when the chain resolution is
    /// known, else derived from the redemption's total payout when unambiguous.
    pub price: f64,
    /// The transaction's TOTAL payout in whole USDC (the same for every leg of one redemption).
    pub payout_usdc: f64,
    pub venue: RedeemVenue,
    pub tx_hash: String,
    pub block: u64,
    pub ts_ms: i64,
}

// ---------------------------------------------------------------------------------------------
// The oracle (what the consumer seams read)
// ---------------------------------------------------------------------------------------------

/// The shared, in-memory chain view: resolved conditions (cached `eth_call` verdicts) plus the
/// account-scoped ledger of observed settlements. `Arc`-shared and interior-mutable so
/// [`crate::exec_plane::settlement::resolve`] and [`crate::exec_plane::recon_client`] can hold `&self` clones while the watcher thread
/// writes.
///
/// Only RESOLVED verdicts are cached: an unresolved condition is re-read next time, because "not
/// resolved yet" is by definition temporary. A resolved one never un-resolves on chain, so caching
/// it is sound and keeps the per-tick `eth_call` count at zero once a watchlist settles.
pub struct ChainOracle {
    rpc: PolygonRpc,
    resolutions: Mutex<BTreeMap<String, ChainResolution>>,
    settlements: Mutex<Vec<ChainSettlement>>,
}

impl ChainOracle {
    pub fn new(rpc: PolygonRpc) -> Self {
        ChainOracle {
            rpc,
            resolutions: Mutex::new(BTreeMap::new()),
            settlements: Mutex::new(Vec::new()),
        }
    }

    /// From the caller's [`ChainRpcSettings`] ([`PolygonRpc::new`]).
    pub fn from_settings(settings: &ChainRpcSettings) -> Self {
        Self::new(PolygonRpc::new(settings))
    }

    pub fn rpc(&self) -> &PolygonRpc {
        &self.rpc
    }

    /// The cached-or-fetched verdict. `None` on an RPC failure (logged) — a transport blip must
    /// leave the caller on its pre-existing behaviour, never assert "unresolved".
    pub fn resolution(&self, condition_id: &str) -> Option<ChainResolution> {
        let key = clean(condition_id);
        if let Some(hit) = self.resolutions.lock().unwrap().get(&key) {
            return Some(hit.clone());
        }
        match self.rpc.condition_resolution(condition_id) {
            Ok(r) => {
                if r.is_resolved() {
                    self.resolutions.lock().unwrap().insert(key, r.clone());
                    Some(r)
                } else {
                    Some(r) // fresh, uncached: it may resolve at any time
                }
            }
            Err(e) => {
                tracing::warn!(target: "vike_polymarket::chain", %condition_id, %e, "chain resolution read failed");
                None
            }
        }
    }

    /// Seed a verdict without a network call — the test seam, and the path a historical
    /// [`ResolutionEvent`] scan would feed.
    pub fn insert_resolution(&self, r: ChainResolution) {
        self.resolutions.lock().unwrap().insert(clean(&r.condition_id), r);
    }

    /// Record observed settlements, DEDUPED on `(tx_hash, token_id)` so a re-scanned block window
    /// never double-books.
    pub fn record_settlements(&self, rows: impl IntoIterator<Item = ChainSettlement>) -> usize {
        let mut guard = self.settlements.lock().unwrap();
        let mut added = 0;
        for r in rows {
            if guard.iter().any(|s| s.tx_hash == r.tx_hash && s.token_id == r.token_id) {
                continue;
            }
            guard.push(r);
            added += 1;
        }
        added
    }

    /// Every observed settlement, oldest first.
    pub fn settlements(&self) -> Vec<ChainSettlement> {
        self.settlements.lock().unwrap().clone()
    }

    /// Settlements at or after `since_ms`. `since_ms <= 0` returns everything (the reconcile
    /// lookback's "no cutoff" spelling).
    pub fn settlements_since(&self, since_ms: i64) -> Vec<ChainSettlement> {
        self.settlements
            .lock()
            .unwrap()
            .iter()
            .filter(|s| since_ms <= 0 || s.ts_ms == 0 || s.ts_ms >= since_ms)
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------
// The watcher
// ---------------------------------------------------------------------------------------------

/// One poll's outcome — settlements found and the block cursor reached.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ChainTick {
    pub from_block: u64,
    pub to_block: u64,
    pub transfers: usize,
    pub settlements: Vec<ChainSettlement>,
}

/// Scans Polygon for the funder's own settlements. See the module doc's TRAP section for why the
/// scan anchors on the funder's ERC-1155 OUTFLOW rather than on `PayoutRedemption.redeemer`.
pub struct ChainWatcher {
    oracle: Arc<ChainOracle>,
    funder_topic: String,
    cursor: Option<u64>,
    cold_lookback: u64,
}

impl ChainWatcher {
    /// `funder` is the address whose holdings move — the account's `FUNDER` / the deposit wallet, the same
    /// key [`crate::exec_plane::recon_client`] reads `/positions` with.
    pub fn new(oracle: Arc<ChainOracle>, funder: &str) -> Result<Self, String> {
        let f = clean(funder);
        if f.len() != 40 {
            return Err(format!("funder must be a 20-byte address, got {funder:?}"));
        }
        Ok(ChainWatcher {
            oracle,
            funder_topic: format!("0x{}{}", "0".repeat(24), f),
            cursor: None,
            cold_lookback: DEFAULT_COLD_LOOKBACK_BLOCKS,
        })
    }

    /// Start the next scan at an explicit block instead of the cold lookback.
    pub fn resume_from(&mut self, block: u64) {
        self.cursor = Some(block);
    }

    pub fn cursor(&self) -> Option<u64> {
        self.cursor
    }

    /// The funder-outflow filter: `[[TransferSingle, TransferBatch], null, funder]` — topic1 is the
    /// operator (any), topic2 is `from`.
    fn transfer_topics(&self) -> Value {
        serde_json::json!([
            [TOPIC_TRANSFER_SINGLE, TOPIC_TRANSFER_BATCH],
            Value::Null,
            self.funder_topic
        ])
    }

    /// ONE scan pass: funder outflows → their transactions' receipts → the `PayoutRedemption` in
    /// each → per-token [`ChainSettlement`]s, recorded in the oracle. A transaction with no
    /// redemption log is an ordinary trade/transfer and is skipped silently — that filter is what
    /// makes this a SETTLEMENT watcher rather than a transfer log.
    pub fn poll_once(&mut self) -> Result<ChainTick, String> {
        let head = self.oracle.rpc().block_number()?;
        let from = self.cursor.unwrap_or_else(|| head.saturating_sub(self.cold_lookback));
        if from > head {
            self.cursor = Some(head + 1);
            return Ok(ChainTick { from_block: from, to_block: head, ..Default::default() });
        }
        let tick = self.scan_range(from, head)?;
        self.cursor = Some(head + 1);
        Ok(tick)
    }

    /// One EXPLICIT block range, cursor untouched — the historical-replay twin of
    /// [`poll_once`](Self::poll_once) (and what it delegates to). Bounded on both ends, so replaying
    /// a known past settlement costs the range, not "everything since then".
    pub fn scan_range(&self, from: u64, to: u64) -> Result<ChainTick, String> {
        let logs = self.oracle.rpc().get_logs(CTF_ADDRESS, self.transfer_topics(), from, to)?;
        let mut by_tx: BTreeMap<String, Vec<TokenTransfer>> = BTreeMap::new();
        for l in &logs {
            match decode_token_transfer(l) {
                Ok(t) => by_tx.entry(t.tx_hash.clone()).or_default().push(t),
                Err(e) => {
                    tracing::warn!(target: "vike_polymarket::chain", %e, "undecodable transfer log skipped")
                }
            }
        }
        let transfers = by_tx.values().map(|v| v.len()).sum();
        let mut settlements = Vec::new();
        for (tx, ts) in &by_tx {
            let Some(receipt) = self.oracle.rpc().transaction_receipt(tx)? else { continue };
            let Some(redemption) = receipt
                .get("logs")
                .and_then(|l| l.as_array())
                .and_then(|l| l.iter().find_map(decode_redemption))
            else {
                continue; // not a settlement transaction
            };
            let resolution =
                self.oracle.resolution(&redemption.condition_id).filter(|r| r.is_resolved());
            settlements.extend(join_settlement(&redemption, ts, resolution.as_ref()));
        }
        let added = self.oracle.record_settlements(settlements.clone());
        if added > 0 {
            tracing::info!(
                target: "vike_polymarket::chain",
                from, to, added,
                "chain watcher: recorded on-chain settlements"
            );
        }
        Ok(ChainTick { from_block: from, to_block: to, transfers, settlements })
    }
}

pub struct ChainWatchPoller;

impl ChainWatchPoller {
    /// Spawn the watcher thread. Returns `None` — starting NOTHING, opening no socket — unless
    /// `funder` is non-empty AND the caller passes `enabled` (D4 of decision 0095: no composition
    /// root starts it, so it reads no setting of its own). Not started ⇒ byte-identical to before
    /// this module existed.
    pub fn spawn(
        oracle: Arc<ChainOracle>,
        funder: String,
        interval: Duration,
        enabled: bool,
    ) -> Option<StopHandle> {
        if funder.trim().is_empty() || !enabled {
            return None;
        }
        let mut watcher = match ChainWatcher::new(Arc::clone(&oracle), funder.trim()) {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!(target: "vike_polymarket::chain", %e, "chain watcher not started");
                return None;
            }
        };
        Some(spawn_poller("vike-polymarket-chain", move |stop| {
            while !stop.load(Ordering::Relaxed) {
                if let Err(e) = watcher.poll_once() {
                    tracing::warn!(target: "vike_polymarket::chain", %e, "chain watcher tick failed");
                }
                if sleep_stop_aware(&stop, interval, STOP_POLL_SLICE) {
                    break;
                }
            }
        }))
    }
}

#[path = "chain_tests.rs"]
#[cfg(test)]
mod chain_tests;
