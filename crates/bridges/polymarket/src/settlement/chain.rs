//! `chain` — the **Polygon on-chain settlement watcher**: the missing oracle that explains a
//! Polymarket position → cash movement no CLOB endpoint ever reports.
//!
//! ## The blind spot this closes
//! [`crate::recon_client`]'s module doc states it: Polymarket settlement is a two-step ON-CHAIN
//! flow — a market RESOLVES, then winning tokens are REDEEMED for USDC — and **redemption produces
//! no `/data/trades` fill**. Reconcile therefore sees a position vanish and cash appear with
//! nothing explaining it, which is exactly why the venue shipped `quarantine`-first (#646). And
//! [`crate::resolve`]'s doc states the sibling gap: `/positions.redeemable` alone cannot tell a
//! resolved LOSER from a position still trading.
//!
//! Both gaps have the same cure: read the settlement facts from the chain that produced them. This
//! module is a MINIMAL JSON-RPC reader over the crate's existing `ureq` stack — **no `ethers`, no
//! `alloy`, no `web3`, no new workspace dependency of any kind** (the `deny.toml` `[bans]` gate
//! rejects a second HTTP/TLS/curve stack, and `hex`/`serde_json`/`ureq` were already this crate's
//! deps). It READS only: `eth_blockNumber` / `eth_call` / `eth_getLogs` / `eth_getTransactionReceipt`.
//! It never signs and never sends a transaction — that stays [`crate::redeem_relayer`]'s job.
//!
//! ## ⚠ It is OPT-IN and DEFAULT-OFF
//! [`chain_watch_enabled`] gates every spawned thread on `POLY_CHAIN_WATCH=1` — the EXACT string
//! `"1"`, the `VIKE_RECONCILE`/`POLY_AUTO_REDEEM`/`POLY_RECONCILE` idiom. Unset (the default) ⇒
//! nothing is constructed, no socket is opened, and every consumer seam below falls back to its
//! pre-existing behaviour byte-for-byte.
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
//! | CTF [`crate::redeem::CTF_ADDRESS`] | `ConditionResolution(bytes32,address,bytes32,uint256,uint256[])` | `b44d84d3…` | YES | 12 |
//! | CTF | `PayoutRedemption(address,address,bytes32,bytes32,uint256[],uint256)` | `2682012a…` | YES | 244 |
//! | NegRiskAdapter [`crate::redeem::NEG_RISK_ADAPTER`] | `PayoutRedemption(address,bytes32,uint256[],uint256)` | `9140a6a2…` | YES | 33 |
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
//!    it is the AUTHORITATIVE winner source. This is what [`crate::resolve`] consumes.
//! 2. **`eth_getLogs` redemption scan** ([`ChainWatcher`]) — the account-scoped ledger of realised
//!    settlements, with the exact USDC payout. This is what [`crate::recon_client`] consumes, as
//!    settlement `FillReport`s that make a redeem-shaped divergence EXPLAINED instead of
//!    quarantined.
//!
//! ## ⚠ FINDING: `/positions.redeemable` does NOT mean "won" (a live refutation)
//! [`crate::resolve`]'s winner rule assumes the data-api flags only the WINNING leg. A live read of
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
//! the only correct payout source, and [`crate::resolve`]'s chain seam a money-safety fix, not just
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

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use vike_bridge_core::poller::{sleep_stop_aware, spawn_poller, StopHandle, STOP_POLL_SLICE};

use crate::order::Side;
use crate::redeem::CTF_ADDRESS;

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

/// Polygon PoS. Named here for the health check only — the `137` constants in [`crate::l1`] /
/// [`crate::order`] / [`crate::redeem_relayer`] are EIP-712 signing domains and are unrelated.
pub const POLYGON_CHAIN_ID: u64 = 137;

/// The default JSON-RPC endpoint: keyless, public, and — verified 2026-07-23 — serving all four
/// methods this module uses INCLUDING archive receipts. Deliberately NOT a keyed provider URL; set
/// [`RPC_URL_ENV`] to point at your own node or paid endpoint.
///
/// (`polygon-rpc.com`, the historical default, now answers `API key disabled` and is unusable;
/// `polygon-bor-rpc.publicnode.com` and `1rpc.io/matic` serve calls and recent logs but refuse
/// archive receipts.)
pub const DEFAULT_RPC_URL: &str = "https://polygon.drpc.org";

/// `POLY_CHAIN_WATCH=1` — the opt-in gate. Default OFF.
pub const CHAIN_WATCH_ENV: &str = "POLY_CHAIN_WATCH";
/// `POLY_CHAIN_RPC_URL` — override [`DEFAULT_RPC_URL`].
pub const RPC_URL_ENV: &str = "POLY_CHAIN_RPC_URL";
/// `POLY_CHAIN_MAX_SPAN` — max blocks per `eth_getLogs` window (free endpoints cap this hard; the
/// scan CHUNKS anything wider rather than failing).
pub const MAX_SPAN_ENV: &str = "POLY_CHAIN_MAX_SPAN";
/// `POLY_CHAIN_PROXY=1` — route the RPC through the venue's SOCKS tunnel ([`crate::egress::proxy_url`]).
/// Default OFF: Polygon RPCs are not geo-blocked, so the tunnel is only wanted on a host whose DNS
/// is (the UA case [`crate::exec`] documents).
pub const CHAIN_PROXY_ENV: &str = "POLY_CHAIN_PROXY";

/// Conservative default `eth_getLogs` window. `1rpc.io` caps at 50 blocks, `drpc` at 10 000; 45
/// blocks (~90 s of Polygon) is under every free cap seen and is far wider than one poll interval.
pub const DEFAULT_MAX_SPAN: u64 = 45;

/// Default watcher cadence — settlement is a human-timescale event, mirroring
/// [`crate::resolve::DEFAULT_POLL_INTERVAL`].
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// How far back the FIRST poll looks when no cursor exists yet (~10 min of Polygon).
pub const DEFAULT_COLD_LOOKBACK_BLOCKS: u64 = 300;

/// The exact-`"1"` opt-in, read from the process env first and the workspace `.env` second — the
/// same two-tier rule [`crate::recon_client::poly_reconcile_enabled`] uses, and for the same reason
/// (every other Polymarket knob lives in the `.env`).
pub fn chain_watch_enabled() -> bool {
    env_flag(CHAIN_WATCH_ENV)
}

fn env_flag(key: &str) -> bool {
    chain_var(key).as_deref() == Some("1")
}

/// The narrow workspace-`.env` fallback for THIS module's four keys. A fixed allow-list, mirroring
/// [`crate::exec`]'s `PROXY_KEYS` discipline: the `.env` is the credential store, so nothing but
/// these can leak out through this path. Read once and cached.
const CHAIN_KEYS: [&str; 4] = [CHAIN_WATCH_ENV, RPC_URL_ENV, MAX_SPAN_ENV, CHAIN_PROXY_ENV];

fn dotenv_chain_vars() -> &'static HashMap<String, String> {
    static CACHE: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        let all = vike_bridge_core::credentials::load_workspace_dotenv();
        CHAIN_KEYS
            .iter()
            .filter_map(|k| all.get(*k).map(|v| ((*k).to_string(), v.clone())))
            .collect()
    })
}

/// Process env FIRST, workspace `.env` second; only the FIRST token is taken, because the live
/// `.env` annotates values with trailing `#` comments the shared parser does not strip (the bug
/// [`crate::egress::proxy_url`] was fixed for).
fn chain_var(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .or_else(|| dotenv_chain_vars().get(key).cloned())
        .map(|v| crate::config::first_token(&v).to_string())
        .filter(|v| !v.is_empty())
}

// ---------------------------------------------------------------------------------------------
// Pure hex / ABI helpers (no dependency beyond `hex`, already a crate dep)
// ---------------------------------------------------------------------------------------------

/// Strip `0x` and lowercase.
fn clean(s: &str) -> String {
    s.strip_prefix("0x").unwrap_or(s).to_ascii_lowercase()
}

/// Split an ABI data blob into 32-byte words (hex, no `0x`). A trailing partial word is dropped —
/// real ABI data is always word-aligned, and a truncated body must not panic.
pub fn data_words(data: &str) -> Vec<String> {
    let d = clean(data);
    d.as_bytes()
        .chunks(64)
        .filter(|c| c.len() == 64)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect()
}

/// One 32-byte word → `u128`. The high 16 bytes MUST be zero: every value this module reads
/// (payouts, 6-decimal token amounts, slot counts, index sets) fits comfortably, and silently
/// truncating a genuinely-256-bit value would fabricate a number. Token IDS are the one true
/// 256-bit field and go through [`u256_word_to_decimal`] instead.
pub fn word_u128(w: &str) -> Result<u128, String> {
    let w = clean(w);
    if w.len() != 64 {
        return Err(format!("expected a 32-byte word, got {} hex chars", w.len()));
    }
    if w[..32].bytes().any(|b| b != b'0') {
        return Err(format!("word exceeds u128: 0x{w}"));
    }
    u128::from_str_radix(&w[32..], 16).map_err(|e| format!("bad hex word: {e}"))
}

/// One 32-byte word → a lower-case `0x…` 20-byte address (the right-aligned convention every
/// indexed `address` topic uses).
pub fn word_address(w: &str) -> Result<String, String> {
    let w = clean(w);
    if w.len() != 64 {
        return Err(format!("expected a 32-byte word, got {} hex chars", w.len()));
    }
    Ok(format!("0x{}", &w[24..]))
}

/// One 32-byte word → the DECIMAL string form of the uint256 — which is exactly how Polymarket
/// spells an ERC-1155 outcome token id everywhere else in this crate (`/positions.asset`, the CLOB
/// `asset_id`, and therefore the vike `symbol`). Schoolbook base-256 → base-10 division, so no
/// bigint dependency is added for the one genuinely 256-bit field on this wire.
pub fn u256_word_to_decimal(w: &str) -> Result<String, String> {
    let w = clean(w);
    if w.len() != 64 {
        return Err(format!("expected a 32-byte word, got {} hex chars", w.len()));
    }
    let mut bytes = hex::decode(&w).map_err(|e| format!("bad hex word: {e}"))?;
    let mut digits = Vec::new();
    while bytes.iter().any(|b| *b != 0) {
        let mut rem = 0u32;
        for b in bytes.iter_mut() {
            let cur = (rem << 8) | u32::from(*b);
            *b = (cur / 10) as u8;
            rem = cur % 10;
        }
        digits.push(b'0' + rem as u8);
    }
    if digits.is_empty() {
        return Ok("0".to_string());
    }
    digits.reverse();
    Ok(String::from_utf8(digits).expect("ascii digits"))
}

/// A dynamic `uint256[]` living at `words[head]`'s offset → its elements. The offset is measured in
/// BYTES from the start of the data section (so `/32` is the word index), which is what makes the
/// decoders self-checking: a wrong layout assumption lands on a nonsense offset and errors instead
/// of returning plausible garbage.
fn dyn_array(words: &[String], head: usize) -> Result<Vec<u128>, String> {
    let off = word_u128(words.get(head).ok_or("array head word missing")?)? as usize;
    if !off.is_multiple_of(32) {
        return Err(format!("array offset {off} is not word-aligned"));
    }
    let at = off / 32;
    let len = word_u128(words.get(at).ok_or("array length word missing")?)? as usize;
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        out.push(word_u128(words.get(at + 1 + i).ok_or("array element word missing")?)?);
    }
    Ok(out)
}

/// The same, for a `uint256[]` whose elements are 256-bit token IDs (decimal strings).
fn dyn_array_ids(words: &[String], head: usize) -> Result<Vec<String>, String> {
    let off = word_u128(words.get(head).ok_or("array head word missing")?)? as usize;
    if !off.is_multiple_of(32) {
        return Err(format!("array offset {off} is not word-aligned"));
    }
    let at = off / 32;
    let len = word_u128(words.get(at).ok_or("array length word missing")?)? as usize;
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        out.push(u256_word_to_decimal(words.get(at + 1 + i).ok_or("array element word missing")?)?);
    }
    Ok(out)
}

fn log_str(l: &Value, key: &str) -> String {
    l.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn log_topic(l: &Value, i: usize) -> Result<String, String> {
    l.get("topics")
        .and_then(|t| t.as_array())
        .and_then(|t| t.get(i))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("log has no topic[{i}]"))
}

fn log_u64(l: &Value, key: &str) -> u64 {
    l.get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| u64::from_str_radix(clean(s).as_str(), 16).ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------------------------
// Decoded types
// ---------------------------------------------------------------------------------------------

/// Which contract settled a redemption — it selects the event layout, and mirrors
/// [`crate::redeem_relayer::RedeemKind`]'s binary/neg-risk split.
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
    /// (base units), the semantics [`crate::redeem`] pins from the contract source.
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
    /// [`crate::resolve::WINNER_PAYOUT`]/[`LOSER_PAYOUT`](crate::resolve::LOSER_PAYOUT)); a SPLIT
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
/// movement, and the row [`crate::recon_client`] turns into a settlement `FillReport`.
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
// Pure decoders (fixture-tested against REAL mainnet logs)
// ---------------------------------------------------------------------------------------------

/// CTF `PayoutRedemption` log → [`Redemption`]. Topics: `[topic0, redeemer, collateralToken,
/// parentCollectionId]`; data head is `[conditionId, offset(indexSets), payout]` with the array in
/// the tail — so the offset is ALWAYS `0x60` (three head words), which every one of the 244 live
/// logs sampled confirmed and which this decoder re-checks through [`dyn_array`].
pub fn decode_ctf_redemption(l: &Value) -> Result<Redemption, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(TOPIC_CTF_PAYOUT_REDEMPTION) {
        return Err(format!("not a CTF PayoutRedemption log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    Ok(Redemption {
        venue: RedeemVenue::Ctf,
        redeemer: word_address(&log_topic(l, 1)?)?,
        condition_id: format!("0x{}", w.first().ok_or("no conditionId word")?),
        payout_usdc: word_u128(w.get(2).ok_or("no payout word")?)? as f64 / USDC_DECIMALS,
        slot_values: dyn_array(&w, 1)?,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
        ts_ms: log_u64(l, "blockTimestamp") as i64 * 1000,
    })
}

/// NegRiskAdapter `PayoutRedemption` log → [`Redemption`]. Topics: `[topic0, redeemer, conditionId]`
/// (the conditionId identity is proven in the module doc); data head is `[offset(amounts), payout]`
/// ⇒ offset `0x40`, again re-checked structurally.
pub fn decode_neg_risk_redemption(l: &Value) -> Result<Redemption, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(TOPIC_NEG_RISK_PAYOUT_REDEMPTION) {
        return Err(format!("not a NegRiskAdapter PayoutRedemption log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    Ok(Redemption {
        venue: RedeemVenue::NegRisk,
        redeemer: word_address(&log_topic(l, 1)?)?,
        condition_id: log_topic(l, 2)?,
        payout_usdc: word_u128(w.get(1).ok_or("no payout word")?)? as f64 / USDC_DECIMALS,
        slot_values: dyn_array(&w, 0)?,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
        ts_ms: log_u64(l, "blockTimestamp") as i64 * 1000,
    })
}

/// Either redemption shape, dispatched on `topic0`. `None` for any other log.
pub fn decode_redemption(l: &Value) -> Option<Redemption> {
    match log_topic(l, 0).ok()?.to_ascii_lowercase() {
        t if t == TOPIC_CTF_PAYOUT_REDEMPTION => decode_ctf_redemption(l).ok(),
        t if t == TOPIC_NEG_RISK_PAYOUT_REDEMPTION => decode_neg_risk_redemption(l).ok(),
        _ => None,
    }
}

/// CTF `ConditionResolution` log → [`ResolutionEvent`]. Topics: `[topic0, conditionId, oracle,
/// questionId]`; data head is `[outcomeSlotCount, offset(payoutNumerators)]` ⇒ offset `0x40`.
pub fn decode_condition_resolution(l: &Value) -> Result<ResolutionEvent, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(TOPIC_CONDITION_RESOLUTION) {
        return Err(format!("not a ConditionResolution log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    Ok(ResolutionEvent {
        condition_id: log_topic(l, 1)?,
        oracle: word_address(&log_topic(l, 2)?)?,
        question_id: log_topic(l, 3)?,
        outcome_slot_count: word_u128(w.first().ok_or("no outcomeSlotCount word")?)?,
        payout_numerators: dyn_array(&w, 1)?,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
    })
}

/// ERC-1155 `TransferSingle`/`TransferBatch` → [`TokenTransfer`], the single form normalised into
/// one-element vectors so the caller has ONE shape to reason about.
pub fn decode_token_transfer(l: &Value) -> Result<TokenTransfer, String> {
    let t0 = log_topic(l, 0)?.to_ascii_lowercase();
    let w = data_words(&log_str(l, "data"));
    let (ids, values) = if t0 == TOPIC_TRANSFER_SINGLE {
        (
            vec![u256_word_to_decimal(w.first().ok_or("no id word")?)?],
            vec![word_u128(w.get(1).ok_or("no value word")?)?],
        )
    } else if t0 == TOPIC_TRANSFER_BATCH {
        (dyn_array_ids(&w, 0)?, dyn_array(&w, 1)?)
    } else {
        return Err(format!("not an ERC-1155 transfer log (topic0 {t0})"));
    };
    if ids.len() != values.len() {
        return Err(format!("ids/values length mismatch ({} vs {})", ids.len(), values.len()));
    }
    Ok(TokenTransfer {
        operator: word_address(&log_topic(l, 1)?)?,
        from: word_address(&log_topic(l, 2)?)?,
        to: word_address(&log_topic(l, 3)?)?,
        ids,
        values,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
    })
}

/// One `eth_call` result word → `u128` (the shape every view function here returns).
pub fn decode_uint_result(hex: &str) -> Result<u128, String> {
    let w = data_words(hex);
    word_u128(w.first().ok_or("empty eth_call result")?)
}

// ---------------------------------------------------------------------------------------------
// Phase C decoders — the shared decode library (drift unification, see the module doc). Pure,
// additive, and NOT consumed by [`ChainWatcher`]/[`ChainOracle`] below.
// ---------------------------------------------------------------------------------------------

/// `OrderFilled(bytes32,address,address,uint8,uint256,uint256,uint256,uint256,bytes32,bytes32)` on
/// the unified V2 CTF Exchange ([`V2_EXCHANGE`]) — the sole `OrderFilled` emitter since the
/// ~Apr-2026 V1→V2 migration. Mirrors `onchain_decode.py`'s `ORDERFILLED_TOPIC0`.
pub const TOPIC_ORDER_FILLED_V2: &str =
    "0xd543adfd945773f1a62f74f0ee55a5e3b9b1a28262980ba90b1a89f2ea84d8ee";
/// `OrderFilled(bytes32,address,address,uint256,uint256,uint256,uint256,uint256)` — the legacy
/// 8-param layout on [`V1_EXCHANGE_STD`]/[`V1_EXCHANGE_NEG`] (pre-~Apr-2026).
pub const TOPIC_ORDER_FILLED_V1: &str =
    "0xd0a08e8c493f9c94f29311604c9de1b4e8c8d4c06bd0c789af57f2d65bfec0f6";
/// CTF `PositionSplit(address,bytes32,bytes32,uint256[])` — mints a full outcome-token set against
/// collateral.
pub const TOPIC_CTF_POSITION_SPLIT: &str =
    "0x2e6bb91f8cbcda0c93623c54d0403a43514fabc40084ec96b6d5379a74786298";
/// CTF `PositionsMerge(address,bytes32,bytes32,uint256[])` — burns a full outcome-token set back
/// into collateral.
pub const TOPIC_CTF_POSITION_MERGE: &str =
    "0x6f13ca62553fcc2bcd2372180a43949c1e4cebba603901ede2f4e14f36b282ca";
/// NegRiskAdapter `PositionsConverted(address,bytes32,uint256,uint256)` — converts YES tokens
/// across a neg-risk market's outcome set. `indexSet` is a BITMAP over the outcome slots, NOT a
/// per-slot amount (see the memory note `polymarket-negrisk-set-facts`).
pub const TOPIC_NEG_RISK_POSITIONS_CONVERTED: &str =
    "0xb03d19dddbc72a87e735ff0ea3b57bef133ebe44e1894284916a84044deb367e";
/// Standard ERC-20 `Transfer(address,address,uint256)` — the universally-published constant,
/// reused here for the USDC.e collateral leg ([`crate::redeem::USDC_E_ADDRESS`]).
pub const TOPIC_USDC_TRANSFER: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// The unified V2 CTF Exchange — every market's `OrderFilled` emitter since the ~Apr-2026
/// migration (the same literal `onchain_decode.py` calls `EXCHANGE`/`NEGRISK_EXCHANGE`).
pub const V2_EXCHANGE: &str = "0xe111180000d2663c0091e4f400237545b87b996b";
/// V1 (legacy) CTF Exchange — standard/binary markets, pre-~Apr-2026.
pub const V1_EXCHANGE_STD: &str = "0x4bfb41d5b3570defd03c39a9a4d8de6bd8b8982e";
/// V1 (legacy) NegRisk CTF Exchange, pre-~Apr-2026.
pub const V1_EXCHANGE_NEG: &str = "0xc5d563a36ae78145c45a50134d48a1215220f80a";

/// Which `OrderFilled` ABI generation decoded a fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderFillAbi {
    /// The 10-param layout on the unified [`V2_EXCHANGE`] — `side`-tagged, single `tokenId`.
    V2,
    /// The 8-param legacy layout on [`V1_EXCHANGE_STD`]/[`V1_EXCHANGE_NEG`] —
    /// `makerAssetId`/`takerAssetId`, side inferred from which asset id is the zero (USDC) leg.
    V1,
}

/// Which side of the fill this row is — mirrors the Python daemon's MAKER-ONLY row model
/// (`onchain_decode.py`'s module doc): the indexed `maker` is always the order owner, and
/// recording the taker leg too would double-count a mint-matched trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRole {
    /// The on-chain `taker` topic IS the exchange contract itself — this maker's order aggressed.
    Taker,
    /// The on-chain `taker` topic is a real counter-wallet — this maker's resting order was hit.
    Maker,
}

/// One decoded `OrderFilled` fill, MAKER perspective — [`decode_order_fill_v2`]/
/// [`decode_order_fill_v1`] normalise both ABI generations into this one shape, mirroring
/// `onchain_decode.py`'s `decode_orderfilled`/`decode_orderfilled_v1` row.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderFill {
    pub abi: OrderFillAbi,
    pub order_hash: String,
    pub maker: String,
    pub taker: String,
    /// The ERC-1155 outcome token (decimal) — the vike `symbol` for this venue.
    pub token_id: String,
    pub side: Side,
    pub size: f64,
    pub price: f64,
    pub fee_usdc: f64,
    pub role: FillRole,
    pub tx_hash: String,
    pub block: u64,
    /// Block timestamp in ms when the RPC supplies `blockTimestamp` (most do), else `0`.
    pub ts_ms: i64,
}

/// V2 (10-param) `OrderFilled` on [`V2_EXCHANGE`]. Topics: `[topic0, orderHash, maker, taker]`;
/// data is `[side, tokenId, makerAmt, takerAmt, fee, builder, metadata]` — 7 fixed words, no
/// dynamic array. `side` 0 = BUY (maker pays USDC, gets tokens) / 1 = SELL (maker pays tokens,
/// gets USDC) — the same convention [`crate::order::Side::code`] encodes for order signing. A
/// zero-size fill is rejected, mirroring the Python decoder's `None` return.
pub fn decode_order_fill_v2(l: &Value) -> Result<OrderFill, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(TOPIC_ORDER_FILLED_V2) {
        return Err(format!("not a V2 OrderFilled log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    let side =
        if word_u128(w.first().ok_or("no side word")?)? == 0 { Side::Buy } else { Side::Sell };
    let token_id = u256_word_to_decimal(w.get(1).ok_or("no tokenId word")?)?;
    let maker_amt = word_u128(w.get(2).ok_or("no makerAmt word")?)? as f64 / USDC_DECIMALS;
    let taker_amt = word_u128(w.get(3).ok_or("no takerAmt word")?)? as f64 / USDC_DECIMALS;
    let fee = word_u128(w.get(4).ok_or("no fee word")?)? as f64 / USDC_DECIMALS;
    let (size, usd) = match side {
        Side::Buy => (taker_amt, maker_amt),
        Side::Sell => (maker_amt, taker_amt),
    };
    if size == 0.0 {
        return Err("zero-size fill".to_string());
    }
    let taker = word_address(&log_topic(l, 3)?)?;
    let role =
        if taker.eq_ignore_ascii_case(V2_EXCHANGE) { FillRole::Taker } else { FillRole::Maker };
    Ok(OrderFill {
        abi: OrderFillAbi::V2,
        order_hash: log_topic(l, 1)?,
        maker: word_address(&log_topic(l, 2)?)?,
        taker,
        token_id,
        side,
        size,
        price: usd / size,
        fee_usdc: fee,
        role,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
        ts_ms: log_u64(l, "blockTimestamp") as i64 * 1000,
    })
}

/// V1 (8-param, legacy) `OrderFilled` on [`V1_EXCHANGE_STD`]/[`V1_EXCHANGE_NEG`]. Topics:
/// `[topic0, orderHash, maker, taker]`; data is `[makerAssetId, takerAssetId, makerAmt, takerAmt,
/// fee]`. `makerAssetId == 0` (the USDC collateral asset id) ⇒ BUY (maker pays USDC, token =
/// takerAssetId); `takerAssetId == 0` ⇒ SELL. Neither leg being the USDC asset id is not a
/// collateral fill (a token/token match) and is rejected, mirroring the Python decoder's `None`.
pub fn decode_order_fill_v1(l: &Value) -> Result<OrderFill, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(TOPIC_ORDER_FILLED_V1) {
        return Err(format!("not a V1 OrderFilled log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    let maker_asset = w.first().ok_or("no makerAssetId word")?;
    let taker_asset = w.get(1).ok_or("no takerAssetId word")?;
    let maker_amt = word_u128(w.get(2).ok_or("no makerAmt word")?)? as f64 / USDC_DECIMALS;
    let taker_amt = word_u128(w.get(3).ok_or("no takerAmt word")?)? as f64 / USDC_DECIMALS;
    let fee = word_u128(w.get(4).ok_or("no fee word")?)? as f64 / USDC_DECIMALS;
    let (token_id, side, size, usd) = if word_u128(maker_asset)? == 0 {
        (u256_word_to_decimal(taker_asset)?, Side::Buy, taker_amt, maker_amt)
    } else if word_u128(taker_asset)? == 0 {
        (u256_word_to_decimal(maker_asset)?, Side::Sell, maker_amt, taker_amt)
    } else {
        return Err("neither asset leg is USDC — not a collateral fill".to_string());
    };
    if size == 0.0 {
        return Err("zero-size fill".to_string());
    }
    let taker = word_address(&log_topic(l, 3)?)?;
    let role = if taker.eq_ignore_ascii_case(V1_EXCHANGE_STD)
        || taker.eq_ignore_ascii_case(V1_EXCHANGE_NEG)
    {
        FillRole::Taker
    } else {
        FillRole::Maker
    };
    Ok(OrderFill {
        abi: OrderFillAbi::V1,
        order_hash: log_topic(l, 1)?,
        maker: word_address(&log_topic(l, 2)?)?,
        taker,
        token_id,
        side,
        size,
        price: usd / size,
        fee_usdc: fee,
        role,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
        ts_ms: log_u64(l, "blockTimestamp") as i64 * 1000,
    })
}

/// Which position-accounting event a [`PositionEvent`] decoded — CTF `PositionSplit` mints a full
/// outcome-token set against collateral; `PositionsMerge` burns one back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEventKind {
    Split,
    Merge,
}

/// One decoded CTF `PositionSplit`/`PositionsMerge` log — the position-accounting twin of a
/// redemption. Topics: `[topic0, stakeholder, parentCollectionId, conditionId]`; data head is
/// `[collateralToken, offset(partition), amount, ...]` — offset is ALWAYS `0x60` (three head
/// words), the same shape [`decode_ctf_redemption`] documents. `amount` is the full-SET mint/burn
/// total, not a per-outcome value.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionEvent {
    pub kind: PositionEventKind,
    pub stakeholder: String,
    pub condition_id: String,
    pub amount: f64,
    pub tx_hash: String,
    pub block: u64,
}

fn decode_ctf_position_event(
    l: &Value,
    topic0: &str,
    kind: PositionEventKind,
) -> Result<PositionEvent, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(topic0) {
        return Err(format!("not a CTF {kind:?} log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    Ok(PositionEvent {
        kind,
        stakeholder: word_address(&log_topic(l, 1)?)?,
        condition_id: log_topic(l, 3)?,
        amount: word_u128(w.get(2).ok_or("no amount word")?)? as f64 / USDC_DECIMALS,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
    })
}

/// CTF `PositionSplit` → [`PositionEvent`].
pub fn decode_ctf_position_split(l: &Value) -> Result<PositionEvent, String> {
    decode_ctf_position_event(l, TOPIC_CTF_POSITION_SPLIT, PositionEventKind::Split)
}

/// CTF `PositionsMerge` → [`PositionEvent`].
pub fn decode_ctf_position_merge(l: &Value) -> Result<PositionEvent, String> {
    decode_ctf_position_event(l, TOPIC_CTF_POSITION_MERGE, PositionEventKind::Merge)
}

/// One decoded NegRiskAdapter `PositionsConverted` log — converts YES tokens across a neg-risk
/// market's outcome set. Topics: `[topic0, stakeholder, marketId, indexSet]`; data is a single
/// word `[amount]`. `index_set` is a BITMAP over the market's outcome slots, NOT a per-slot amount.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionsConverted {
    pub stakeholder: String,
    pub market_id: String,
    /// Bitmap over outcome slots — NOT per-slot amounts (see `polymarket-negrisk-set-facts`).
    pub index_set: u128,
    pub amount: f64,
    pub tx_hash: String,
    pub block: u64,
}

/// NegRiskAdapter `PositionsConverted` → [`PositionsConverted`].
pub fn decode_positions_converted(l: &Value) -> Result<PositionsConverted, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(TOPIC_NEG_RISK_POSITIONS_CONVERTED) {
        return Err(format!("not a NegRiskAdapter PositionsConverted log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    Ok(PositionsConverted {
        stakeholder: word_address(&log_topic(l, 1)?)?,
        market_id: log_topic(l, 2)?,
        index_set: word_u128(&log_topic(l, 3)?)?,
        amount: word_u128(w.first().ok_or("no amount word")?)? as f64 / USDC_DECIMALS,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
    })
}

/// One decoded USDC.e ERC-20 `Transfer` — the funding/withdrawal leg no CLOB endpoint reports
/// (deposits, a redemption payout landing back in the funder's wallet, on-chain withdrawals).
/// Standard ERC-20 shape: topics `[topic0, from, to]`, data `[value]` (6-dp, [`USDC_DECIMALS`]).
#[derive(Debug, Clone, PartialEq)]
pub struct UsdcTransfer {
    pub from: String,
    pub to: String,
    pub value_usdc: f64,
    pub tx_hash: String,
    pub block: u64,
}

/// USDC.e `Transfer` → [`UsdcTransfer`].
pub fn decode_usdc_transfer(l: &Value) -> Result<UsdcTransfer, String> {
    let t0 = log_topic(l, 0)?;
    if !t0.eq_ignore_ascii_case(TOPIC_USDC_TRANSFER) {
        return Err(format!("not a USDC Transfer log (topic0 {t0})"));
    }
    let w = data_words(&log_str(l, "data"));
    Ok(UsdcTransfer {
        from: word_address(&log_topic(l, 1)?)?,
        to: word_address(&log_topic(l, 2)?)?,
        value_usdc: word_u128(w.first().ok_or("no value word")?)? as f64 / USDC_DECIMALS,
        tx_hash: log_str(l, "transactionHash"),
        block: log_u64(l, "blockNumber"),
    })
}

// ---------------------------------------------------------------------------------------------
// Joining a redemption to the funder's own tokens
// ---------------------------------------------------------------------------------------------

/// PURE join: one transaction's funder token-outflows + its redemption + (optionally) the chain's
/// payout numerators → the per-token [`ChainSettlement`] rows.
///
/// Pricing, in strict precedence — and **never a guess**:
/// 1. `resolution` known ⇒ each leg is priced at `payout_for_index(i)`, where `i` is the token's
///    position in the transfer's id vector, **and the mapping is then PROVEN, not assumed**: the
///    identity `Σ(qtyᵢ · priceᵢ) == payout` must hold to [`PAYOUT_EPSILON`]. That check is what
///    makes this safe — the CTF builds its ERC-1155 batch in `indexSets` order, so vector position
///    equals outcome slot for a `[1, 2]` redeem but NOT for a single-leg `[2]` redeem (observed
///    live). A mismatch falls through to rule 2 instead of booking a wrong price.
/// 2. If exactly ONE leg has a non-zero amount, that leg absorbs the whole payout (`payout / qty`)
///    and every other leg prices at 0.0 — arithmetically forced, so it is correct whatever the slot
///    ordering turns out to be.
/// 3. Otherwise the transaction is SKIPPED with a warning. Two moved legs and one total is
///    under-determined, and a fabricated realised PnL is worse than a missing one — the same
///    refuse-to-guess rule [`crate::resolve::ambiguous_conditions`] applies.
pub fn join_settlement(
    redemption: &Redemption,
    transfers: &[TokenTransfer],
    resolution: Option<&ChainResolution>,
) -> Vec<ChainSettlement> {
    let mut legs: Vec<(String, u128)> = Vec::new();
    for t in transfers {
        for (id, v) in t.ids.iter().zip(t.values.iter()) {
            legs.push((id.clone(), *v));
        }
    }
    if legs.is_empty() {
        return Vec::new();
    }
    let non_zero: Vec<usize> =
        legs.iter().enumerate().filter(|(_, (_, v))| *v > 0).map(|(i, _)| i).collect();
    // Rule 1, with its proof obligation.
    let by_resolution = resolution.filter(|r| r.is_resolved()).and_then(|r| {
        let prices: Vec<f64> =
            (0..legs.len()).map(|i| r.payout_for_index(i as u32).unwrap_or(0.0)).collect();
        let implied: f64 =
            legs.iter().zip(&prices).map(|((_, v), px)| (*v as f64 / USDC_DECIMALS) * px).sum();
        if (implied - redemption.payout_usdc).abs() <= PAYOUT_EPSILON {
            Some(prices)
        } else {
            tracing::warn!(
                target: "vike_polymarket::chain",
                condition_id = %redemption.condition_id,
                tx = %redemption.tx_hash,
                implied, payout = redemption.payout_usdc,
                "chain watcher: slot mapping failed its payout identity — falling back"
            );
            None
        }
    });
    let prices: Vec<f64> = match by_resolution {
        Some(p) => p,
        None if non_zero.len() == 1 => {
            let qty = legs[non_zero[0]].1 as f64 / USDC_DECIMALS;
            let px = if qty > 0.0 { redemption.payout_usdc / qty } else { 0.0 };
            (0..legs.len()).map(|i| if i == non_zero[0] { px } else { 0.0 }).collect()
        }
        None => {
            tracing::warn!(
                target: "vike_polymarket::chain",
                condition_id = %redemption.condition_id,
                tx = %redemption.tx_hash,
                legs = legs.len(),
                "chain watcher: payout split across legs is under-determined without a chain \
                 resolution — settlement not synthesised (refusing to guess)"
            );
            return Vec::new();
        }
    };
    legs.into_iter()
        .zip(prices)
        .filter(|((_, v), _)| *v > 0)
        .map(|((token_id, v), price)| ChainSettlement {
            condition_id: redemption.condition_id.clone(),
            token_id,
            qty: v as f64 / USDC_DECIMALS,
            price,
            payout_usdc: redemption.payout_usdc,
            venue: redemption.venue,
            tx_hash: redemption.tx_hash.clone(),
            block: redemption.block,
            ts_ms: redemption.ts_ms,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The JSON-RPC client
// ---------------------------------------------------------------------------------------------

/// A MINIMAL Polygon JSON-RPC reader over the crate's existing blocking `ureq` stack. Four read
/// methods, no signing, no transaction submission, no new dependency.
pub struct PolygonRpc {
    url: String,
    agent: ureq::Agent,
    max_span: u64,
}

impl Default for PolygonRpc {
    fn default() -> Self {
        Self::new()
    }
}

impl PolygonRpc {
    /// Build from the environment: [`RPC_URL_ENV`] (default [`DEFAULT_RPC_URL`]),
    /// [`MAX_SPAN_ENV`] (default [`DEFAULT_MAX_SPAN`]), [`CHAIN_PROXY_ENV`] (default direct).
    pub fn new() -> Self {
        let url = chain_var(RPC_URL_ENV).unwrap_or_else(|| DEFAULT_RPC_URL.to_string());
        let max_span = chain_var(MAX_SPAN_ENV)
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_MAX_SPAN);
        PolygonRpc { url, agent: Self::build_agent(), max_span }
    }

    /// An explicit endpoint (the smoke and the offline tests use this).
    pub fn with_url(url: impl Into<String>) -> Self {
        PolygonRpc { url: url.into(), agent: Self::build_agent(), max_span: DEFAULT_MAX_SPAN }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn max_span(&self) -> u64 {
        self.max_span
    }

    /// A fresh agent, tunnelled only when [`CHAIN_PROXY_ENV`] is on. Deliberately NOT
    /// [`crate::exec`]'s `agent()`: that one tunnels unconditionally, and a Polygon RPC is not
    /// geo-blocked, so paying the tunnel by default would add a failure mode for no benefit.
    fn build_agent() -> ureq::Agent {
        let mut b = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent("vike-trader-rust");
        if env_flag(CHAIN_PROXY_ENV) {
            if let Some(url) = crate::egress::proxy_url() {
                if let Ok(p) = ureq::Proxy::new(&url) {
                    b = b.proxy(Some(p));
                }
            }
        }
        b.build().new_agent()
    }

    /// One JSON-RPC round trip. A JSON-RPC `error` member is a hard `Err` (a silently-`None`
    /// result would read as "no events", the exact failure this module exists to avoid).
    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let body =
            serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let mut resp = self
            .agent
            .post(&self.url)
            .content_type("application/json")
            .send(body.to_string())
            .map_err(|e| format!("rpc {method}: network: {e}"))?;
        let text =
            resp.body_mut().read_to_string().map_err(|e| format!("rpc {method}: body: {e}"))?;
        let v: Value =
            serde_json::from_str(&text).map_err(|e| format!("rpc {method}: bad json: {e}"))?;
        if let Some(err) = v.get("error") {
            return Err(format!("rpc {method}: {err}"));
        }
        v.get("result").cloned().ok_or_else(|| format!("rpc {method}: no result member"))
    }

    pub fn chain_id(&self) -> Result<u64, String> {
        let r = self.call("eth_chainId", serde_json::json!([]))?;
        let s = r.as_str().ok_or("eth_chainId: not a string")?;
        u64::from_str_radix(clean(s).as_str(), 16).map_err(|e| format!("eth_chainId: {e}"))
    }

    pub fn block_number(&self) -> Result<u64, String> {
        let r = self.call("eth_blockNumber", serde_json::json!([]))?;
        let s = r.as_str().ok_or("eth_blockNumber: not a string")?;
        u64::from_str_radix(clean(s).as_str(), 16).map_err(|e| format!("eth_blockNumber: {e}"))
    }

    pub fn eth_call(&self, to: &str, data: &str) -> Result<String, String> {
        let r = self.call("eth_call", serde_json::json!([{ "to": to, "data": data }, "latest"]))?;
        r.as_str().map(|s| s.to_string()).ok_or_else(|| "eth_call: not a string".to_string())
    }

    /// `eth_getLogs`, CHUNKED at [`max_span`](Self::max_span) blocks so a free endpoint's window
    /// cap is a cadence detail rather than a hard failure. `topics` is passed through verbatim, so
    /// a caller can use the `[[a,b], null, x]` OR/positional form.
    pub fn get_logs(
        &self,
        address: &str,
        topics: Value,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        let mut lo = from_block;
        while lo <= to_block {
            let hi = to_block.min(lo + self.max_span.saturating_sub(1));
            let filter = serde_json::json!({
                "address": address,
                "topics": topics,
                "fromBlock": format!("0x{lo:x}"),
                "toBlock": format!("0x{hi:x}"),
            });
            let r = self.call("eth_getLogs", serde_json::json!([filter]))?;
            let arr = r.as_array().ok_or("eth_getLogs: result is not an array")?;
            out.extend(arr.iter().cloned());
            lo = hi + 1;
        }
        Ok(out)
    }

    pub fn transaction_receipt(&self, tx_hash: &str) -> Result<Option<Value>, String> {
        let r = self.call("eth_getTransactionReceipt", serde_json::json!([tx_hash]))?;
        Ok(if r.is_null() { None } else { Some(r) })
    }

    /// **The authoritative winner read**: the CTF's own payout mappings for one condition, via
    /// `eth_call` — no log window, no archive node, and correct for a SPLIT resolution that no
    /// flag-based heuristic can express. `denominator == 0` ⇒ not resolved (and the numerators are
    /// then not fetched at all).
    pub fn condition_resolution(&self, condition_id: &str) -> Result<ChainResolution, String> {
        let cid = clean(condition_id);
        if cid.len() != 64 {
            return Err(format!("conditionId must be 32 bytes, got {} hex chars", cid.len()));
        }
        let denominator = decode_uint_result(
            &self.eth_call(CTF_ADDRESS, &format!("{SEL_PAYOUT_DENOMINATOR}{cid}"))?,
        )?;
        if denominator == 0 {
            return Ok(ChainResolution {
                condition_id: condition_id.to_string(),
                denominator: 0,
                numerators: Vec::new(),
            });
        }
        let slots = decode_uint_result(
            &self.eth_call(CTF_ADDRESS, &format!("{SEL_OUTCOME_SLOT_COUNT}{cid}"))?,
        )?;
        let mut numerators = Vec::with_capacity(slots as usize);
        for i in 0..slots {
            let arg = format!("{i:064x}");
            numerators.push(decode_uint_result(
                &self.eth_call(CTF_ADDRESS, &format!("{SEL_PAYOUT_NUMERATORS}{cid}{arg}"))?,
            )?);
        }
        Ok(ChainResolution { condition_id: condition_id.to_string(), denominator, numerators })
    }
}

// ---------------------------------------------------------------------------------------------
// The oracle (what the consumer seams read)
// ---------------------------------------------------------------------------------------------

/// The shared, in-memory chain view: resolved conditions (cached `eth_call` verdicts) plus the
/// account-scoped ledger of observed settlements. `Arc`-shared and interior-mutable so
/// [`crate::resolve`] and [`crate::recon_client`] can hold `&self` clones while the watcher thread
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

    /// From the environment ([`PolygonRpc::new`]).
    pub fn from_env() -> Self {
        Self::new(PolygonRpc::new())
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
    /// `funder` is the address whose holdings move — `POLY_FUNDER` / the deposit wallet, the same
    /// key [`crate::recon_client`] reads `/positions` with.
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

/// Owner-side handle: stop-aware shutdown, `Drop`-joining — the shared [`StopHandle`] scaffold
/// (`vike_bridge_core::poller`), [`crate::resolve::ResolveHandle`]'s contract exactly, so a dropped
/// handle never leaks the thread.
pub type ChainWatchHandle = StopHandle;

pub struct ChainWatchPoller;

impl ChainWatchPoller {
    /// Spawn the watcher thread. Returns `None` — starting NOTHING, opening no socket — unless
    /// `funder` is non-empty AND [`chain_watch_enabled`]. Unset (the default) ⇒ byte-identical to
    /// before this module existed.
    pub fn spawn(
        oracle: Arc<ChainOracle>,
        funder: String,
        interval: Duration,
    ) -> Option<ChainWatchHandle> {
        if funder.trim().is_empty() || !chain_watch_enabled() {
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

#[cfg(test)]
mod tests {
    use super::*;
    // Test-only: the sibling contract address the neg-risk fixture must have been emitted by.
    use crate::redeem::NEG_RISK_ADAPTER;

    // Every fixture below is a REAL Polygon mainnet log, captured verbatim 2026-07-23 (the block
    // numbers and tx hashes are checkable on any explorer). They belong to third-party wallets on
    // purpose — this repo's own account never appears in a committed fixture; its verification
    // lives in the `#[ignore]`d live smoke, which reads `POLY_FUNDER` from the workspace `.env`.

    /// CTF `PayoutRedemption`, tx `0x3f12d5d3…`, payout 59.076955 USDC, indexSets `[1, 2]`.
    const CTF_REDEMPTION: &str = r#"{"address":"0x4d97dcd97ec945f40cf65f87097ace5ea0476045","topics":["0x2682012a4a4f1973119f1c9b90745d1bd91fa2bab387344f044cb3586864d18d","0x0000000000000000000000005d4aba8ad45bb5eab3499a0294b42da5d1e455d3","0x0000000000000000000000002791bca1f2de4661ed88a30c99a7a9449aa84174","0x0000000000000000000000000000000000000000000000000000000000000000"],"data":"0x62530e00e2f67d9757e0b06e168e9929e0661daff1276354a3018f1568120c2f0000000000000000000000000000000000000000000000000000000000000060000000000000000000000000000000000000000000000000000000000385715b000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002","blockNumber":"0x5683395","transactionHash":"0x3f12d5d3285700b3b818784725b19c5ac4fcdfc987eff668175aa82aaf41c3cc","blockTimestamp":"0x6a618b45","logIndex":"0x440","removed":false}"#;

    /// NegRiskAdapter `PayoutRedemption`, tx `0x92ebcfb0…`, payout 11.0 USDC, amounts `[0, 11.0]`.
    const NEG_RISK_REDEMPTION: &str = r#"{"address":"0xd91e80cf2e7be2e162c6513ced06f1dd0da35296","topics":["0x9140a6a270ef945260c03894b3c6b3b2695e9d5101feef0ff24fec960cfd3224","0x00000000000000000000000041792a63ad17e7a210c808de4177e64a561eccee","0x95dbea2403eefccc30a0b4f276e0dd94d8030ac0f826aa2702ebd835cd75985c"],"data":"0x00000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000a7d8c0000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a7d8c0","blockNumber":"0x568339f","transactionHash":"0x92ebcfb0cb9cd1c984d747eeaa6b693103b51f67a822628464a6d9b89b658d0f","blockTimestamp":"0x6a618b54","logIndex":"0x3c2","removed":false}"#;

    /// CTF `ConditionResolution`, tx `0x84b7768d…`, 2 slots, numerators `[1, 0]`.
    const CONDITION_RESOLUTION: &str = r#"{"address":"0x4d97dcd97ec945f40cf65f87097ace5ea0476045","topics":["0xb44d84d3289691f71497564b85d4233648d9dbae8cbdbb4329f301c3a0185894","0xda4bfca4a2f26cce689ac3e2b89cc60a17fb0b994ee79f61ec8beb65f6886236","0x00000000000000000000000065070be91477460d8a7aeeb94ef92fe056c2f2a7","0xb7576115f85ca1f02c40bd07ae7373640423e87fcffd4c2a8986af23f2a01438"],"data":"0x00000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000040000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000","blockNumber":"0x56833e9","transactionHash":"0x84b7768df7b4837c89179e776d0575e2c9dfce8f68fa8807de92f423ff40ace7","logIndex":"0x258","removed":false}"#;

    /// ERC-1155 `TransferBatch` of TWO outcome tokens, values `[5.0, 0.0]`, tx `0xc6c3f62a…`.
    const TRANSFER_BATCH: &str = r#"{"address":"0x4d97dcd97ec945f40cf65f87097ace5ea0476045","topics":["0x4a39dc06d4c0dbc64b70af90fd698a233a518aa5d07e595d983b8c0526c8f7fb","0x000000000000000000000000ada2005600dec949baf300f4c6120000bdb6eaab","0x00000000000000000000000041e7aa1b047f13ad96f26ca49602fb403c90b78d","0x000000000000000000000000ada2005600dec949baf300f4c6120000bdb6eaab"],"data":"0x000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000a0000000000000000000000000000000000000000000000000000000000000000247212f7902c30bd802ea461cc340b9fdb9ad0b35c1ce8aa75807ee264579f66455252c4605d295420c9f788919cbb436e4c17c7690a6dbe43b73bedac1c3069e000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000004c4b400000000000000000000000000000000000000000000000000000000000000000","blockNumber":"0x568339e","transactionHash":"0xc6c3f62a83b1dc2518a78f64878dfe479d4b5ffc98cf0a40f12ae63affdf60e4","removed":false}"#;

    /// ERC-1155 `TransferSingle` burn of one outcome token, value 5.0, same tx.
    const TRANSFER_SINGLE: &str = r#"{"address":"0x4d97dcd97ec945f40cf65f87097ace5ea0476045","topics":["0xc3d58168c5ae7397731d063d5bbf3d657854427343f4c083240f7aacaa2d0f62","0x000000000000000000000000d91e80cf2e7be2e162c6513ced06f1dd0da35296","0x000000000000000000000000d91e80cf2e7be2e162c6513ced06f1dd0da35296","0x0000000000000000000000000000000000000000000000000000000000000000"],"data":"0x47212f7902c30bd802ea461cc340b9fdb9ad0b35c1ce8aa75807ee264579f66400000000000000000000000000000000000000000000000000000000004c4b40","blockNumber":"0x568339e","transactionHash":"0xc6c3f62a83b1dc2518a78f64878dfe479d4b5ffc98cf0a40f12ae63affdf60e4","removed":false}"#;

    fn log(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    // --- word helpers ---------------------------------------------------------------------------

    #[test]
    fn u256_word_decodes_to_the_decimal_token_id_spelling() {
        // The token id from TRANSFER_SINGLE, in the decimal form `/positions.asset` uses.
        assert_eq!(
            u256_word_to_decimal(
                "47212f7902c30bd802ea461cc340b9fdb9ad0b35c1ce8aa75807ee264579f664"
            )
            .unwrap(),
            "32172845847072308101858152476368078090585804255392742427414441920393186506340"
        );
        assert_eq!(u256_word_to_decimal(&"0".repeat(64)).unwrap(), "0");
        assert_eq!(u256_word_to_decimal(&format!("{:064x}", 1u8)).unwrap(), "1");
        // max uint256 — the schoolbook division must not overflow or truncate.
        assert_eq!(
            u256_word_to_decimal(&"f".repeat(64)).unwrap(),
            "115792089237316195423570985008687907853269984665640564039457584007913129639935"
        );
        assert!(u256_word_to_decimal("dead").is_err(), "a short word is rejected, not padded");
    }

    #[test]
    fn word_u128_refuses_to_truncate_a_wide_value() {
        assert_eq!(word_u128(&format!("{:064x}", 5_000_000u64)).unwrap(), 5_000_000);
        assert!(word_u128(&"f".repeat(64)).is_err(), "a >u128 value must error, never wrap");
        assert!(word_u128("0x00").is_err());
    }

    #[test]
    fn word_address_takes_the_right_aligned_20_bytes() {
        assert_eq!(
            word_address("0x0000000000000000000000005d4aba8ad45bb5eab3499a0294b42da5d1e455d3")
                .unwrap(),
            "0x5d4aba8ad45bb5eab3499a0294b42da5d1e455d3"
        );
    }

    // --- decoders over REAL mainnet logs --------------------------------------------------------

    #[test]
    fn decodes_a_real_ctf_redemption() {
        let r = decode_ctf_redemption(&log(CTF_REDEMPTION)).unwrap();
        assert_eq!(r.venue, RedeemVenue::Ctf);
        assert_eq!(r.redeemer, "0x5d4aba8ad45bb5eab3499a0294b42da5d1e455d3");
        assert_eq!(
            r.condition_id,
            "0x62530e00e2f67d9757e0b06e168e9929e0661daff1276354a3018f1568120c2f"
        );
        // 0x0385715b base units = 59.076955 USDC
        assert_eq!(r.payout_usdc.to_bits(), 59.076955f64.to_bits());
        assert_eq!(r.slot_values, vec![1, 2], "the binary redeem's indexSets");
        assert_eq!(r.block, 0x5683395);
        assert_eq!(r.ts_ms, 0x6a618b45 * 1000);
        assert_eq!(r.tx_hash, "0x3f12d5d3285700b3b818784725b19c5ac4fcdfc987eff668175aa82aaf41c3cc");
    }

    #[test]
    fn decodes_a_real_neg_risk_redemption() {
        let r = decode_neg_risk_redemption(&log(NEG_RISK_REDEMPTION)).unwrap();
        assert_eq!(r.venue, RedeemVenue::NegRisk);
        assert_eq!(r.redeemer, "0x41792a63ad17e7a210c808de4177e64a561eccee");
        // The NegRiskAdapter indexes the conditionId as topic2 — proven in the module doc against
        // the CTF's own conditionId word in the same transaction.
        assert_eq!(
            r.condition_id,
            "0x95dbea2403eefccc30a0b4f276e0dd94d8030ac0f826aa2702ebd835cd75985c"
        );
        assert_eq!(r.payout_usdc.to_bits(), 11.0f64.to_bits());
        // per-slot AMOUNTS (the semantics redeem.rs pins from the contract source), not index sets
        assert_eq!(r.slot_values, vec![0, 11_000_000]);
        assert_eq!(
            r.payout_usdc,
            r.slot_values[1] as f64 / 1e6,
            "payout == the winning slot amount"
        );
    }

    #[test]
    fn dispatches_either_redemption_shape_and_rejects_anything_else() {
        assert_eq!(decode_redemption(&log(CTF_REDEMPTION)).unwrap().venue, RedeemVenue::Ctf);
        assert_eq!(
            decode_redemption(&log(NEG_RISK_REDEMPTION)).unwrap().venue,
            RedeemVenue::NegRisk
        );
        assert!(decode_redemption(&log(TRANSFER_BATCH)).is_none());
        assert!(decode_redemption(&log(CONDITION_RESOLUTION)).is_none());
        // The wrong-topic guards are what stop a mis-derived topic0 from being decoded as a
        // plausible-but-wrong redemption.
        assert!(decode_ctf_redemption(&log(NEG_RISK_REDEMPTION)).is_err());
        assert!(decode_neg_risk_redemption(&log(CTF_REDEMPTION)).is_err());
    }

    #[test]
    fn decodes_a_real_condition_resolution() {
        let r = decode_condition_resolution(&log(CONDITION_RESOLUTION)).unwrap();
        assert_eq!(
            r.condition_id,
            "0xda4bfca4a2f26cce689ac3e2b89cc60a17fb0b994ee79f61ec8beb65f6886236"
        );
        assert_eq!(r.oracle, "0x65070be91477460d8a7aeeb94ef92fe056c2f2a7");
        assert_eq!(r.outcome_slot_count, 2);
        assert_eq!(r.payout_numerators, vec![1, 0], "outcome slot 0 won");
        assert!(decode_condition_resolution(&log(CTF_REDEMPTION)).is_err());
    }

    #[test]
    fn decodes_both_erc1155_transfer_shapes_into_one_form() {
        let b = decode_token_transfer(&log(TRANSFER_BATCH)).unwrap();
        assert_eq!(b.from, "0x41e7aa1b047f13ad96f26ca49602fb403c90b78d");
        assert_eq!(b.to, "0xada2005600dec949baf300f4c6120000bdb6eaab");
        assert_eq!(b.ids.len(), 2);
        assert_eq!(b.values, vec![5_000_000, 0]);

        let s = decode_token_transfer(&log(TRANSFER_SINGLE)).unwrap();
        assert_eq!(s.to, "0x0000000000000000000000000000000000000000", "a burn");
        assert_eq!(s.ids.len(), 1);
        assert_eq!(s.values, vec![5_000_000]);
        // The single form's id is the FIRST of the batch's — same transaction, same token.
        assert_eq!(s.ids[0], b.ids[0]);

        assert!(decode_token_transfer(&log(CTF_REDEMPTION)).is_err());
    }

    // --- ChainResolution semantics --------------------------------------------------------------

    #[test]
    fn unresolved_condition_prices_nothing() {
        let r = ChainResolution { condition_id: "0xa".into(), denominator: 0, numerators: vec![] };
        assert!(!r.is_resolved());
        assert_eq!(r.payout_for_index(0), None, "unknown must NOT read as worthless");
        assert_eq!(r.winner_index(), None);
    }

    #[test]
    fn binary_resolution_prices_winner_one_and_loser_zero() {
        let r =
            ChainResolution { condition_id: "0xa".into(), denominator: 1, numerators: vec![0, 1] };
        assert!(r.is_resolved());
        assert_eq!(r.payout_for_index(0).unwrap().to_bits(), 0.0f64.to_bits());
        assert_eq!(r.payout_for_index(1).unwrap().to_bits(), 1.0f64.to_bits());
        assert_eq!(r.winner_index(), Some(1));
        assert_eq!(r.payout_for_index(9), None, "out-of-range slot is unknown, not zero");
    }

    /// A SPLIT resolution (`[1,1]/2`) pays both legs 0.5 — a payout no `redeemable`-flag heuristic
    /// can express, and the reason the numerators are read rather than a boolean.
    #[test]
    fn split_resolution_prices_both_legs_at_half_and_names_no_winner() {
        let r =
            ChainResolution { condition_id: "0xa".into(), denominator: 2, numerators: vec![1, 1] };
        assert_eq!(r.payout_for_index(0).unwrap().to_bits(), 0.5f64.to_bits());
        assert_eq!(r.payout_for_index(1).unwrap().to_bits(), 0.5f64.to_bits());
        assert_eq!(r.winner_index(), None, "a split has no single winner");
    }

    // --- the join ------------------------------------------------------------------------------

    fn transfer(ids: &[&str], values: &[u128]) -> TokenTransfer {
        TokenTransfer {
            operator: "0x0".into(),
            from: "0xfunder".into(),
            to: "0x0".into(),
            ids: ids.iter().map(|s| (*s).to_string()).collect(),
            values: values.to_vec(),
            tx_hash: "0xtx".into(),
            block: 1,
        }
    }

    fn redemption(payout: f64) -> Redemption {
        Redemption {
            venue: RedeemVenue::Ctf,
            redeemer: "0xrelayer".into(),
            condition_id: "0xcond".into(),
            payout_usdc: payout,
            slot_values: vec![1, 2],
            tx_hash: "0xtx".into(),
            block: 1,
            ts_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn join_prices_each_leg_from_the_chain_resolution() {
        let r = ChainResolution {
            condition_id: "0xcond".into(),
            denominator: 1,
            numerators: vec![0, 1],
        };
        let out = join_settlement(
            &redemption(11.0),
            &[transfer(&["tokYes", "tokNo"], &[4_000_000, 11_000_000])],
            Some(&r),
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].token_id, "tokYes");
        assert_eq!(out[0].price.to_bits(), 0.0f64.to_bits(), "slot 0 lost");
        assert_eq!(out[0].qty.to_bits(), 4.0f64.to_bits());
        assert_eq!(out[1].price.to_bits(), 1.0f64.to_bits(), "slot 1 won");
        assert_eq!(out[1].qty.to_bits(), 11.0f64.to_bits());
        assert_eq!(out[1].payout_usdc.to_bits(), 11.0f64.to_bits());
    }

    /// Without a chain resolution, a SINGLE non-zero leg is arithmetically forced: it absorbed the
    /// whole payout. This is the neg-risk shape seen live (`amounts [0, 11.0]`, payout 11.0).
    #[test]
    fn join_infers_the_price_when_exactly_one_leg_moved() {
        let out =
            join_settlement(&redemption(11.0), &[transfer(&["a", "b"], &[0, 11_000_000])], None);
        assert_eq!(out.len(), 1, "zero-amount legs are not settlements");
        assert_eq!(out[0].token_id, "b");
        assert_eq!(out[0].price.to_bits(), 1.0f64.to_bits());
    }

    /// A loser-only redeem (payout 0) still yields a settlement row — at price 0.0. This is the
    /// case `/positions.redeemable` cannot distinguish from "still trading" at all.
    #[test]
    fn join_settles_a_zero_payout_loser_at_zero() {
        let out = join_settlement(&redemption(0.0), &[transfer(&["a"], &[5_000_000])], None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].price.to_bits(), 0.0f64.to_bits());
        assert_eq!(out[0].qty.to_bits(), 5.0f64.to_bits());
    }

    /// Two legs moved and no chain resolution ⇒ the split is under-determined, so NOTHING is
    /// synthesised. Refusing to guess is the same rule `resolve::ambiguous_conditions` encodes.
    #[test]
    fn join_refuses_to_guess_an_underdetermined_split() {
        let out = join_settlement(
            &redemption(11.0),
            &[transfer(&["a", "b"], &[4_000_000, 11_000_000])],
            None,
        );
        assert!(out.is_empty(), "two moved legs + one total is not solvable — emit nothing");
    }

    /// **The slot-mapping proof.** A single-leg redeem (`indexSets [2]`, observed live) transfers
    /// ONE token that is outcome slot **1**, but it sits at vector position 0 — so pricing by
    /// position would read the resolution's slot-0 numerator and book a loser at 1.0. The payout
    /// identity catches it: `5 shares × 1.0 != 0 USDC`, so rule 1 is rejected and the
    /// arithmetically-forced single-leg rule prices it correctly at 0.0.
    #[test]
    fn join_rejects_a_slot_mapping_that_fails_the_payout_identity() {
        let mut r = redemption(0.0);
        r.slot_values = vec![2]; // a single-leg redeem of slot 1
                                 // The chain says slot 0 won — so the held slot-1 token is worthless and payout is 0.
        let res = ChainResolution {
            condition_id: "0xcond".into(),
            denominator: 1,
            numerators: vec![1, 0],
        };
        let out = join_settlement(&r, &[transfer(&["tokSlot1"], &[5_000_000])], Some(&res));
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].price.to_bits(),
            0.0f64.to_bits(),
            "position-0 pricing would have said 1.0; the payout identity forced the right answer"
        );
    }

    /// The identity ACCEPTS a correct mapping — the same shape, but with the winner really at
    /// vector position 0 and a matching payout.
    #[test]
    fn join_accepts_a_slot_mapping_that_satisfies_the_payout_identity() {
        let res = ChainResolution {
            condition_id: "0xcond".into(),
            denominator: 1,
            numerators: vec![1, 0],
        };
        let out =
            join_settlement(&redemption(5.0), &[transfer(&["tokSlot0"], &[5_000_000])], Some(&res));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].price.to_bits(), 1.0f64.to_bits());
        assert_eq!(out[0].payout_usdc.to_bits(), 5.0f64.to_bits());
    }

    /// A resolution that neither matches the payout NOR leaves a single moved leg is skipped —
    /// rule 3, the refuse-to-guess floor.
    #[test]
    fn join_skips_when_the_identity_fails_and_two_legs_moved() {
        let res = ChainResolution {
            condition_id: "0xcond".into(),
            denominator: 1,
            numerators: vec![1, 0],
        };
        let out = join_settlement(
            &redemption(99.0), // matches neither leg's arithmetic
            &[transfer(&["a", "b"], &[4_000_000, 11_000_000])],
            Some(&res),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn join_of_nothing_is_nothing() {
        assert!(join_settlement(&redemption(1.0), &[], None).is_empty());
        assert!(join_settlement(&redemption(1.0), &[transfer(&[], &[])], None).is_empty());
    }

    // --- the oracle ------------------------------------------------------------------------------

    fn oracle() -> ChainOracle {
        // The URL is never dialled: every test below seeds the oracle instead of fetching.
        ChainOracle::new(PolygonRpc::with_url("http://127.0.0.1:1/never-dialled"))
    }

    #[test]
    fn oracle_caches_a_seeded_resolution_and_normalises_the_key() {
        let o = oracle();
        o.insert_resolution(ChainResolution {
            condition_id: "0xABCD".into(),
            denominator: 1,
            numerators: vec![1, 0],
        });
        // Case- and prefix-insensitive lookup, without a network call.
        assert_eq!(o.resolution("0xabcd").unwrap().numerators, vec![1, 0]);
        assert_eq!(o.resolution("ABCD").unwrap().winner_index(), Some(0));
    }

    #[test]
    fn oracle_dedups_settlements_on_tx_and_token() {
        let o = oracle();
        let s = ChainSettlement {
            condition_id: "0xc".into(),
            token_id: "tok".into(),
            qty: 5.0,
            price: 1.0,
            payout_usdc: 5.0,
            venue: RedeemVenue::Ctf,
            tx_hash: "0xtx".into(),
            block: 1,
            ts_ms: 1_000,
        };
        assert_eq!(o.record_settlements([s.clone()]), 1);
        assert_eq!(
            o.record_settlements([s.clone()]),
            0,
            "a re-scanned window must not double-book"
        );
        let other = ChainSettlement { token_id: "tok2".into(), ..s };
        assert_eq!(o.record_settlements([other]), 1, "the sibling leg is a distinct row");
        assert_eq!(o.settlements().len(), 2);
        assert_eq!(o.settlements_since(0).len(), 2);
        assert_eq!(o.settlements_since(2_000).len(), 0, "older than the cutoff");
        assert_eq!(o.settlements_since(1_000).len(), 2, "at the cutoff is included");
    }

    // --- gating ----------------------------------------------------------------------------------

    #[test]
    fn watcher_rejects_a_malformed_funder() {
        let o = Arc::new(oracle());
        assert!(ChainWatcher::new(Arc::clone(&o), "not-an-address").is_err());
        assert!(ChainWatcher::new(Arc::clone(&o), "0xdead").is_err());
        let w = ChainWatcher::new(o, "0x107C01D04Fd68557ACd52E89dD01972b22803aD5").unwrap();
        assert!(w.cursor().is_none());
        assert_eq!(
            w.funder_topic, "0x000000000000000000000000107c01d04fd68557acd52e89dd01972b22803ad5",
            "the address is left-padded and lower-cased for the topic filter"
        );
    }

    /// The empty-funder arm runs BEFORE the env gate, so this needs no env mutation and cannot race
    /// the flag test below (`resolve::spawn_returns_none_without_a_proxy_address`'s idiom).
    #[test]
    fn spawn_returns_none_without_a_funder() {
        let o = Arc::new(oracle());
        assert!(ChainWatchPoller::spawn(o, "   ".into(), Duration::from_secs(60)).is_none());
    }

    #[test]
    fn chain_watch_is_off_by_default_and_exact() {
        std::env::remove_var(CHAIN_WATCH_ENV);
        // Only assert the unset case when the dev box's workspace `.env` does not itself carry the
        // key — the `.env` tier is a real (documented) input, not something the test may deny.
        let dotenv_has_key = dotenv_chain_vars().contains_key(CHAIN_WATCH_ENV);
        if !dotenv_has_key {
            assert!(!chain_watch_enabled(), "unset ⇒ OFF");
            // An EMPTY process value falls through to the `.env` tier by design, so it is only a
            // meaningful "off" case when that tier is empty too.
            std::env::set_var(CHAIN_WATCH_ENV, "");
            assert!(!chain_watch_enabled(), "empty must not enable the watcher");
        }
        for off in ["0", "true", "yes", "on", "11", "1x"] {
            std::env::set_var(CHAIN_WATCH_ENV, off);
            assert!(!chain_watch_enabled(), "{off} must not enable the watcher");
        }
        std::env::set_var(CHAIN_WATCH_ENV, "1");
        assert!(chain_watch_enabled());
        // The live `.env` annotates values with trailing comments the shared parser keeps.
        std::env::set_var(CHAIN_WATCH_ENV, "1   # on-chain settlement watcher");
        assert!(chain_watch_enabled());
        std::env::remove_var(CHAIN_WATCH_ENV);
    }

    #[test]
    fn rpc_defaults_are_keyless_and_overridable() {
        let r = PolygonRpc::with_url(DEFAULT_RPC_URL);
        assert_eq!(r.url(), DEFAULT_RPC_URL);
        assert_eq!(r.max_span(), DEFAULT_MAX_SPAN);
        // No API key, no query string, no credential of any kind in the default endpoint.
        assert!(!DEFAULT_RPC_URL.contains('?') && !DEFAULT_RPC_URL.contains("key"));
    }

    /// The pinned constants, asserted as literals so a future edit to a signature string cannot
    /// silently change a topic (a wrong topic0 matches NOTHING, which reads as "no redemptions").
    #[test]
    fn pinned_topics_and_selectors() {
        assert_eq!(
            TOPIC_CONDITION_RESOLUTION,
            "0xb44d84d3289691f71497564b85d4233648d9dbae8cbdbb4329f301c3a0185894"
        );
        assert_eq!(
            TOPIC_CTF_PAYOUT_REDEMPTION,
            "0x2682012a4a4f1973119f1c9b90745d1bd91fa2bab387344f044cb3586864d18d"
        );
        assert_eq!(
            TOPIC_NEG_RISK_PAYOUT_REDEMPTION,
            "0x9140a6a270ef945260c03894b3c6b3b2695e9d5101feef0ff24fec960cfd3224"
        );
        // The two ERC-1155 topics are the universally published constants — they validate the
        // keccak methodology that produced the three Polymarket-specific ones above.
        assert_eq!(
            TOPIC_TRANSFER_SINGLE,
            "0xc3d58168c5ae7397731d063d5bbf3d657854427343f4c083240f7aacaa2d0f62"
        );
        assert_eq!(
            TOPIC_TRANSFER_BATCH,
            "0x4a39dc06d4c0dbc64b70af90fd698a233a518aa5d07e595d983b8c0526c8f7fb"
        );
        assert_eq!(SEL_PAYOUT_DENOMINATOR, "0xdd34de67");
        assert_eq!(SEL_PAYOUT_NUMERATORS, "0x0504c814");
        assert_eq!(SEL_OUTCOME_SLOT_COUNT, "0xd42dc0c2");
        // Each fixture's topic0 IS the pinned constant — the constants and the live wire agree.
        assert_eq!(log(CTF_REDEMPTION)["topics"][0], TOPIC_CTF_PAYOUT_REDEMPTION);
        assert_eq!(log(NEG_RISK_REDEMPTION)["topics"][0], TOPIC_NEG_RISK_PAYOUT_REDEMPTION);
        assert_eq!(log(CONDITION_RESOLUTION)["topics"][0], TOPIC_CONDITION_RESOLUTION);
        assert_eq!(log(TRANSFER_BATCH)["topics"][0], TOPIC_TRANSFER_BATCH);
        assert_eq!(log(TRANSFER_SINGLE)["topics"][0], TOPIC_TRANSFER_SINGLE);
        // And each fixture is emitted by the address this crate already pins for that contract.
        assert_eq!(log(CTF_REDEMPTION)["address"], CTF_ADDRESS.to_ascii_lowercase());
        assert_eq!(log(NEG_RISK_REDEMPTION)["address"], NEG_RISK_ADAPTER.to_ascii_lowercase());
    }

    // =============================================================================================
    // Phase C decoders — fixtures. Every one below is a REAL Polygon mainnet log, sourced from
    // the latency box's live ClickHouse tape (`data_polymarket.polymarket_trades` /
    // `polymarket_position_events`) by `transaction_hash` + `log_index`, then fetched verbatim via
    // `eth_getTransactionReceipt` against the same `polygon.drpc.org` endpoint `PolygonRpc` uses
    // (captured 2026-07-25). Expected values were hand-computed from `onchain_decode.py`'s own
    // logic (word offsets, side/role assignment) — see the Phase C decoder doc comments for the
    // arithmetic — and independently cross-checked against the ClickHouse row the same
    // transaction/log_index already decoded to (`polymarket_trades`/`polymarket_position_events`),
    // so each fixture is proven against TWO independent decodes, not just this crate's own.
    // =============================================================================================

    /// V2 `OrderFilled` on [`V2_EXCHANGE`], tx `0x21d27b0f…`, SELL 4.18 @ 0.94 (role: maker). Matches
    /// the latency box `polymarket_trades` row for the same tx/log_index exactly (size/price/role/proxy_wallet).
    const ORDER_FILL_V2: &str = r#"{"address":"0xe111180000d2663c0091e4f400237545b87b996b","topics":["0xd543adfd945773f1a62f74f0ee55a5e3b9b1a28262980ba90b1a89f2ea84d8ee","0xbfbc54d2a0c2b5b92f572efba77f1c1fa61550d04600fc4f0f1ce420efa08da0","0x000000000000000000000000a253d75c2dfb2c6650291d5e8d54076d2fb40181","0x00000000000000000000000092c9ad93ba0e400ffc8d716edf70a588d246bde3"],"data":"0x0000000000000000000000000000000000000000000000000000000000000001cd997ada6ab1e2cc68c6566e5142cdc885a275d4fa5a1e13b3fdccf22a3f234000000000000000000000000000000000000000000000000000000000003fc82000000000000000000000000000000000000000000000000000000000003bf470000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000","blockNumber":"0x56a1d06","transactionHash":"0x21d27b0fde210ccb5e6480e3f479f342743aa1f854199d57cafab2553d9a516e","transactionIndex":"0x38","blockHash":"0x6fd9b83c01f7d00dec9a7e69fc8427b0b98127e116b02108eeab4cece60ec924","blockTimestamp":"0x6a64696e","logIndex":"0x215","removed":false}"#;

    /// V1 (legacy) `OrderFilled` on [`V1_EXCHANGE_STD`], tx `0x31d5dca4…`, BUY 17.7 @ 0.42 (role:
    /// maker). Matches the latency box `polymarket_wallet_trades_v2` row for the same tx/log_index exactly.
    const ORDER_FILL_V1: &str = r#"{"address":"0x4bfb41d5b3570defd03c39a9a4d8de6bd8b8982e","topics":["0xd0a08e8c493f9c94f29311604c9de1b4e8c8d4c06bd0c789af57f2d65bfec0f6","0xcad66ce0a358cde360f7565f226a7749c1fa08648b57ec3ffd0bf6c0f8a46b8b","0x00000000000000000000000000000003a358014c7c0e227483dbe01619871000","0x000000000000000000000000e55b90febea370d4611a4b0a9ff201183268b24e"],"data":"0x0000000000000000000000000000000000000000000000000000000000000000fdee4e7856e0a259cea17232d8e548f7309655d49c1eac7fe5813a188c3b4b2c0000000000000000000000000000000000000000000000000000000000716f1000000000000000000000000000000000000000000000000000000000010e14a000000000000000000000000000000000000000000000000000000000001b0210","blockNumber":"0x4d93911","transactionHash":"0x31d5dca405136cb4ed09bcf817d2b6c2ce1331b18d4b5738eae2e76f7e73e391","transactionIndex":"0x64","blockHash":"0xc3f40fe4974c608f80d6e9ddca33c61bddc2c6bee0e95f8e6efac8f65a53d26e","blockTimestamp":"0x695e9f3d","logIndex":"0x54d","removed":false}"#;

    /// CTF `PositionSplit`, tx `0xc6d1c08c…`, 20 USDC minted. Matches the latency box
    /// `polymarket_position_events` (kind=split) row for the same tx/log_index exactly.
    const CTF_SPLIT: &str = r#"{"address":"0x4d97dcd97ec945f40cf65f87097ace5ea0476045","topics":["0x2e6bb91f8cbcda0c93623c54d0403a43514fabc40084ec96b6d5379a74786298","0x00000000000000000000000020d2309cd92b797ae7ca175ed828ed8a27fbe29d","0x0000000000000000000000000000000000000000000000000000000000000000","0xb907d819a95244f9e19dc779e3db2a33133a864d22e9ce222d09a16b48759f49"],"data":"0x0000000000000000000000002791bca1f2de4661ed88a30c99a7a9449aa8417400000000000000000000000000000000000000000000000000000000000000600000000000000000000000000000000000000000000000000000000001312d00000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002","blockNumber":"0x52f9360","transactionHash":"0xc6d1c08c7d75a49750e6b282d5f176d79e876419918f78f8ce87ac7141cd9715","transactionIndex":"0x90","blockHash":"0xfe5dcbc7d7e72591cef4b5e98a5d36033f7bdf08ace1ff609d39bf2270e0da1e","blockTimestamp":"0x6a0955e4","logIndex":"0x6f8","removed":false}"#;

    /// CTF `PositionsMerge`, tx `0x19ca1abb…`, 21.12 USDC returned. Matches the latency box
    /// `polymarket_position_events` (kind=merge) row for the same tx/log_index exactly.
    const CTF_MERGE: &str = r#"{"address":"0x4d97dcd97ec945f40cf65f87097ace5ea0476045","topics":["0x6f13ca62553fcc2bcd2372180a43949c1e4cebba603901ede2f4e14f36b282ca","0x000000000000000000000000ada100874d00e3331d00f2007a9c336a65009718","0x0000000000000000000000000000000000000000000000000000000000000000","0x2c1e877e19fe8ebc146e5260c33c9e1eaffd8752f4c3398b49345fbd58fa0e99"],"data":"0x0000000000000000000000002791bca1f2de4661ed88a30c99a7a9449aa8417400000000000000000000000000000000000000000000000000000000000000600000000000000000000000000000000000000000000000000000000001424400000000000000000000000000000000000000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002","blockNumber":"0x52f9360","transactionHash":"0x19ca1abbc7db11bab75038afe52b915efeac7ba2afac7b610ce2771ff14ea8d1","transactionIndex":"0x8d","blockHash":"0xfe5dcbc7d7e72591cef4b5e98a5d36033f7bdf08ace1ff609d39bf2270e0da1e","blockTimestamp":"0x6a0955e4","logIndex":"0x6db","removed":false}"#;

    /// NegRiskAdapter `PositionsConverted`, tx `0xdb22e481…`, 10 USDC, indexSet `0x400` (bit 10).
    /// Matches the latency box `polymarket_position_events` (kind=convert) row for the same tx/log_index.
    const NEG_RISK_CONVERT: &str = r#"{"address":"0xd91e80cf2e7be2e162c6513ced06f1dd0da35296","topics":["0xb03d19dddbc72a87e735ff0ea3b57bef133ebe44e1894284916a84044deb367e","0x000000000000000000000000ada2005600dec949baf300f4c6120000bdb6eaab","0xc93c202f8849d124e5929d3ef5378d9bd7fe1612b8a30c625c6576b0785adf00","0x0000000000000000000000000000000000000000000000000000000000000400"],"data":"0x0000000000000000000000000000000000000000000000000000000000989680","blockNumber":"0x52f9360","transactionHash":"0xdb22e48179afdd38b97cf157c90f63c9ddcd7be837a951e0d7ad2cc6ec123d2b","transactionIndex":"0x50","blockHash":"0xfe5dcbc7d7e72591cef4b5e98a5d36033f7bdf08ace1ff609d39bf2270e0da1e","blockTimestamp":"0x6a0955e4","logIndex":"0x30c","removed":false}"#;

    /// USDC.e `Transfer`, tx `0xe1fa9d91…` — the collateral leg of a CTF redemption (CTF → the
    /// redeemer wallet). The SAME transaction's `polymarket_position_events` (kind=redeem) row names
    /// this exact wallet as the redeemer, proving this Transfer is that redemption's payout landing.
    const USDC_TRANSFER: &str = r#"{"address":"0x2791bca1f2de4661ed88a30c99a7a9449aa84174","topics":["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef","0x0000000000000000000000004d97dcd97ec945f40cf65f87097ace5ea0476045","0x0000000000000000000000006c7e2de5b9d1565793f3757ab568b973413c6816"],"data":"0x00000000000000000000000000000000000000000000000000000000004d1088","blockNumber":"0x52f9360","transactionHash":"0xe1fa9d91dd04fa508bb1d674194b4ece3635ff0b41cf71863d55529667bc48c7","transactionIndex":"0x49","blockHash":"0xfe5dcbc7d7e72591cef4b5e98a5d36033f7bdf08ace1ff609d39bf2270e0da1e","blockTimestamp":"0x6a0955e4","logIndex":"0x288","removed":false}"#;

    #[test]
    fn phase_c_pinned_topics_and_addresses() {
        assert_eq!(log(ORDER_FILL_V2)["topics"][0], TOPIC_ORDER_FILLED_V2);
        assert_eq!(log(ORDER_FILL_V2)["address"], V2_EXCHANGE);
        assert_eq!(log(ORDER_FILL_V1)["topics"][0], TOPIC_ORDER_FILLED_V1);
        assert_eq!(log(ORDER_FILL_V1)["address"], V1_EXCHANGE_STD);
        assert_eq!(log(CTF_SPLIT)["topics"][0], TOPIC_CTF_POSITION_SPLIT);
        assert_eq!(log(CTF_SPLIT)["address"], CTF_ADDRESS.to_ascii_lowercase());
        assert_eq!(log(CTF_MERGE)["topics"][0], TOPIC_CTF_POSITION_MERGE);
        assert_eq!(log(CTF_MERGE)["address"], CTF_ADDRESS.to_ascii_lowercase());
        assert_eq!(log(NEG_RISK_CONVERT)["topics"][0], TOPIC_NEG_RISK_POSITIONS_CONVERTED);
        assert_eq!(log(NEG_RISK_CONVERT)["address"], NEG_RISK_ADAPTER.to_ascii_lowercase());
        assert_eq!(log(USDC_TRANSFER)["topics"][0], TOPIC_USDC_TRANSFER);
        assert_eq!(
            log(USDC_TRANSFER)["address"],
            crate::redeem::USDC_E_ADDRESS.to_ascii_lowercase()
        );
    }

    #[test]
    fn decodes_a_real_v2_order_fill() {
        let f = decode_order_fill_v2(&log(ORDER_FILL_V2)).unwrap();
        assert_eq!(f.abi, OrderFillAbi::V2);
        assert_eq!(
            f.order_hash,
            "0xbfbc54d2a0c2b5b92f572efba77f1c1fa61550d04600fc4f0f1ce420efa08da0"
        );
        assert_eq!(f.maker, "0xa253d75c2dfb2c6650291d5e8d54076d2fb40181");
        assert_eq!(f.taker, "0x92c9ad93ba0e400ffc8d716edf70a588d246bde3");
        assert_eq!(
            f.token_id,
            "92995309462039665251203590892224533684024782310651466832592596015862972883776"
        );
        assert_eq!(f.side, Side::Sell);
        assert_eq!(f.size.to_bits(), 4.18f64.to_bits());
        // maker_amt=4.18 (SELL: maker gives tokens, gets USDC) / taker_amt=3.9292 -> price=0.94.
        assert_eq!(f.price.to_bits(), 0.9400000000000001f64.to_bits());
        assert_eq!(f.fee_usdc.to_bits(), 0.0f64.to_bits());
        assert_eq!(f.role, FillRole::Maker, "taker topic is a real wallet, not the exchange");
        assert_eq!(f.block, 90_840_326);
        assert_eq!(f.ts_ms, 1_784_965_486_000);
        assert_eq!(f.tx_hash, "0x21d27b0fde210ccb5e6480e3f479f342743aa1f854199d57cafab2553d9a516e");
    }

    #[test]
    fn decodes_a_real_v1_order_fill() {
        let f = decode_order_fill_v1(&log(ORDER_FILL_V1)).unwrap();
        assert_eq!(f.abi, OrderFillAbi::V1);
        assert_eq!(
            f.order_hash,
            "0xcad66ce0a358cde360f7565f226a7749c1fa08648b57ec3ffd0bf6c0f8a46b8b"
        );
        assert_eq!(f.maker, "0x00000003a358014c7c0e227483dbe01619871000".to_lowercase());
        assert_eq!(f.taker, "0xe55b90febea370d4611a4b0a9ff201183268b24e");
        assert_eq!(
            f.token_id,
            "114856201873541567677341731370365137173235058495071385195611596664202155739948"
        );
        assert_eq!(f.side, Side::Buy);
        assert_eq!(f.size.to_bits(), 17.7f64.to_bits());
        // makerAssetId==0 -> BUY: usd=makerAmt=7.434 / size=takerAmt=17.7 -> price=0.42000000000000004.
        assert_eq!(f.price.to_bits(), 0.42000000000000004f64.to_bits());
        assert_eq!(f.fee_usdc.to_bits(), 1.77f64.to_bits());
        assert_eq!(f.role, FillRole::Maker, "taker topic is a real wallet, not a V1 exchange");
        assert_eq!(f.block, 81_344_785);
        assert_eq!(f.ts_ms, 1_767_808_829_000);
    }

    #[test]
    fn v1_and_v2_order_fill_decoders_reject_each_others_topic() {
        assert!(decode_order_fill_v2(&log(ORDER_FILL_V1)).is_err());
        assert!(decode_order_fill_v1(&log(ORDER_FILL_V2)).is_err());
        assert!(decode_order_fill_v2(&log(CTF_SPLIT)).is_err());
    }

    #[test]
    fn decodes_a_real_ctf_position_split() {
        let e = decode_ctf_position_split(&log(CTF_SPLIT)).unwrap();
        assert_eq!(e.kind, PositionEventKind::Split);
        assert_eq!(e.stakeholder, "0x20d2309cd92b797ae7ca175ed828ed8a27fbe29d");
        assert_eq!(
            e.condition_id,
            "0xb907d819a95244f9e19dc779e3db2a33133a864d22e9ce222d09a16b48759f49"
        );
        assert_eq!(e.amount.to_bits(), 20.0f64.to_bits());
        assert_eq!(e.block, 87_004_000);
        assert!(
            decode_ctf_position_split(&log(CTF_MERGE)).is_err(),
            "wrong topic0 must be rejected"
        );
    }

    #[test]
    fn decodes_a_real_ctf_position_merge() {
        let e = decode_ctf_position_merge(&log(CTF_MERGE)).unwrap();
        assert_eq!(e.kind, PositionEventKind::Merge);
        assert_eq!(e.stakeholder, "0xada100874d00e3331d00f2007a9c336a65009718");
        assert_eq!(
            e.condition_id,
            "0x2c1e877e19fe8ebc146e5260c33c9e1eaffd8752f4c3398b49345fbd58fa0e99"
        );
        assert_eq!(e.amount.to_bits(), 21.12f64.to_bits());
        assert_eq!(e.block, 87_004_000);
        assert!(
            decode_ctf_position_merge(&log(CTF_SPLIT)).is_err(),
            "wrong topic0 must be rejected"
        );
    }

    #[test]
    fn decodes_a_real_positions_converted() {
        let c = decode_positions_converted(&log(NEG_RISK_CONVERT)).unwrap();
        assert_eq!(c.stakeholder, "0xada2005600dec949baf300f4c6120000bdb6eaab");
        assert_eq!(
            c.market_id,
            "0xc93c202f8849d124e5929d3ef5378d9bd7fe1612b8a30c625c6576b0785adf00"
        );
        // topics[3] = 0x...0400 = 1024 -- a BITMAP over outcome slots, not a per-slot amount.
        assert_eq!(c.index_set, 1024);
        assert_eq!(c.amount.to_bits(), 10.0f64.to_bits());
        assert_eq!(c.block, 87_004_000);
        assert!(decode_positions_converted(&log(CTF_SPLIT)).is_err());
    }

    #[test]
    fn decodes_a_real_usdc_transfer() {
        let t = decode_usdc_transfer(&log(USDC_TRANSFER)).unwrap();
        assert_eq!(
            t.from, "0x4d97dcd97ec945f40cf65f87097ace5ea0476045",
            "the CTF contract paying out"
        );
        assert_eq!(t.to, "0x6c7e2de5b9d1565793f3757ab568b973413c6816", "the redeemer wallet");
        // 0x4d1088 base units = 5.050504 USDC.
        assert_eq!(t.value_usdc.to_bits(), 5.050504f64.to_bits());
        assert_eq!(t.block, 87_004_000);
        assert!(decode_usdc_transfer(&log(CTF_SPLIT)).is_err());
    }
}
