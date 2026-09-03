//! `outcome_settlement` — HIP-4 **outcome-market** local settlement for Hyperliquid.
//!
//! HL's HIP-4 outcome markets (mainnet 2026-05-02) are fully-collateralized binary contracts: you
//! hold a side token (`Yes`/`No`) as a SPOT balance, and when the authorized oracle posts the
//! result the venue converts each token into quote (USDH) at a fixed fraction. Nothing in this
//! workspace ever closed the LOCAL book for that: a settled side token's position and its
//! unrealized PnL linger in `Account` forever — and for a LOSING side, permanently (a zero-value
//! token is never sold, so no trading event would ever retire it). This module emits the terminal
//! synthetic fill that flattens the local position at its settlement payout.
//!
//! It is the Hyperliquid twin of `vike_polymarket::resolve`, and deliberately mirrors that module's
//! shape one-for-one — pure derivation core + trait seam (`OutcomeDeps` ~ `ResolveDeps`), reusable
//! `settle_once` pass, stop-aware Drop-joining handle, at-most-once ledger, ONE bare `Event::Fill`
//! per settled position. It invents NO endpoint: all three reads (`outcomeMeta`,
//! `spotClearinghouseState`, `settledOutcome`) are keyless `/info` requests the existing
//! [`crate::transport::HyperliquidTransport`] already serves.
//!
//! **Opt-in, default OFF:** [`OutcomePoller::spawn`] returns `None` unless `VIKE_HL_OUTCOME=1`
//! ([`hl_outcome_enabled`] — the EXACT string `"1"`, the `VIKE_RECONCILE`/`VIKE_PM_RESOLVE`
//! discipline) AND a non-empty wallet address. Unset ⇒ no thread, no events, byte-identical to
//! before this module existed. It takes no on-chain action and moves no money — it only writes into
//! our own core — so there is no kill switch beyond shutting the handle. Cadence is deliberately
//! slow (default [`DEFAULT_POLL_INTERVAL`], 60s): settlement is an oracle posting a result, a
//! human-timescale event, and a tight loop buys nothing while spending the shared IP weight budget.
//!
//! ## Wire ground truth (verified, NOT invented)
//!
//! Field names below were verified against the official Hyperliquid docs and two independent
//! integrator references (the official `hyperliquid-python-sdk` has NO HIP-4 support at all as of
//! this writing, so it could not serve as the oracle):
//!
//! - **`POST /info {"type":"outcomeMeta"}`** → `{outcomes: [...], questions: [...]}`. Each
//!   `outcomes[]` row carries `outcome` (the numeric outcome id), `name`, `description`, and
//!   `sideSpecs` — an array of `{name}` labelling each side, ordinarily `[{"name":"Yes"},
//!   {"name":"No"}]`. Each `questions[]` row (a CATEGORICAL question grouping several outcomes)
//!   carries `question`, `name`, `description`, `fallbackOutcome`, `namedOutcomes` and
//!   `settledNamedOutcomes`.
//! - **Asset encoding** (docs, "Asset IDs"): for outcome id `outcome` and side index `side`,
//!   `encoding = 10 * outcome + side`; the spot COIN string is `#<encoding>`, the TOKEN name is
//!   `+<encoding>`, and the numeric asset id is `100_000_000 + encoding`. The doc's own worked
//!   example: outcome `1`, side `0` → encoding `10` → `#10` / `+10` / `100000010`.
//! - **`POST /info {"type":"spotClearinghouseState","user":…}`** → `balances[]` rows of
//!   `{coin, token, hold, total, entryNtl}` (all numerics decimal STRINGS). Outcome side tokens
//!   surface here under the `+<encoding>` coin string.
//! - **`POST /info {"type":"settledOutcome","outcome":<id>}`** → the SETTLEMENT ORACLE:
//!   `{spec: {outcome, name, description, sideSpecs, quoteToken}, settleFraction: "0.0",
//!   details: "price:76876.9"}`. `settleFraction` is a decimal STRING like every other HL numeric,
//!   and `spec` is the same row shape `outcomeMeta.outcomes[]` carries (plus `quoteToken`), so it is
//!   parsed by the same [`parse_outcome_row`].
//! - **Settlement** (HIP-4 spec): side 0 converts to `settleFraction` quote tokens per unit and
//!   side 1 to `1 - settleFraction`; a "binary yes" resolution is `settleFraction = 1` and a
//!   "binary no" is `settleFraction = 0`. Quote is **USDH**, not USDC. Settlement is AUTOMATIC —
//!   there is no `claim`/`redeem`/`settle` exchange action, which is exactly why the local book
//!   needs this module: the venue moves the money and nothing tells our core.
//!
//! ## Where the settled signal comes from (and where it does NOT)
//!
//! `outcomeMeta` carries **no settlement field at all** — an outcome row is only
//! `outcome`/`name`/`description`/`sideSpecs`, and a settled outcome is *removed from the next
//! `outcomeMeta` response* rather than flagged in it. The authoritative settlement read is the
//! separate **`settledOutcome`** info request above, which is the ONLY source of `settleFraction`
//! this module will act on. Consequences of that split, which shape the tick:
//!
//! - `outcomeMeta` is used ONLY as a cheap *skip filter*: an outcome still listed live there is
//!   known-unsettled, so its `settledOutcome` query is skipped. If that removal semantic ever failed
//!   to hold, the failure direction is a **delayed** settlement, never a wrong one — the fraction
//!   still has to come from `settledOutcome`. See [`settlement_candidates`].
//! - A `settledOutcome` response without a parseable `settleFraction` reads as NOT settled
//!   ([`parse_settled_outcome`] → `Ok(None)`), and a failed query for one outcome is skipped for
//!   that tick rather than aborting the pass. Both fail CLOSED: no fraction ⇒ no fill ⇒ nothing
//!   fabricated.
//! - `questions[].settledNamedOutcomes` IS parsed but carries no fraction, so it stays a
//!   corroborating diagnostic only and never drives a fill.
//!
//! ## Why a bare `Event::Fill` (and no new Event variant)
//!
//! `ExecutionEngine::on_event` handles `Event::Fill` BEFORE any registry lookup: it symbol-filters,
//! dedups on `trade_id`, folds `Account::apply_fill` (position + realized PnL → `closed_pnls`), and
//! returns. No `client_order_id` registration is required — exactly right, because a settlement has
//! no order behind it. The `OrderPartiallyFilled`/`OrderFilled` wraps are deliberately NOT emitted:
//! those drive the order FSM, need a registered coid, and an unregistered one would be dropped into
//! the engine's `dropped_unknown_coid` audit counter. Downstream, `journal_mat`'s order fold ignores
//! `Event::Fill`, so a settlement creates no phantom `exec_order` row — only the `exec_fill`
//! trade-log row, which is precisely what a settlement is.
//!
//! ## At-most-once and deterministic fill ids
//!
//! The synthetic `trade_id` is DERIVED, never counter-allocated:
//! `hlsettle:<outcome>:<side>:<16-hex FNV-1a-64 of "<outcome>|<side>|<wallet-lowercased>">`
//! ([`settlement_trade_id`]). The same settled position on the same wallet therefore produces the
//! SAME id on every process, every restart, every replay — so the engine's own `seen_trade_ids` is
//! an independent second guard within a session, and a replayed journal cannot double-settle. FNV-1a
//! is written inline (a dozen lines) rather than pulling a hash dep; it is used purely as an
//! identity tag, never as a security primitive. The [`SettlementLedger`] file is the guard ACROSS
//! restarts, keyed on the same `(outcome, side)` pair scoped to the wallet, and is marked ONLY after
//! the lane accepts the event — a failed send stays unmarked and retries next tick.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::Duration;

use std::io::Write;

use serde_json::{json, Value};

use vike_bridge_core::json::{json_num, json_str};
use vike_bridge_core::poller::{sleep_stop_aware, spawn_poller, StopHandle, STOP_POLL_SLICE};
use vike_exec::EventSender;
use vike_model::events::{Event, FillEvent, LiquiditySide, TradeId};
use vike_model::now_ms;

use crate::consts::VENUE;
use crate::transport::HyperliquidTransport;

// ---------------------------------------------------------------------------------------------
// Constants + the verified encoding
// ---------------------------------------------------------------------------------------------

/// Default poll cadence. Settlement is an oracle posting a result — a human-timescale event — so a
/// minute-scale rhythm (matching `vike_polymarket::resolve`) is deliberate, not lazy.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Base of the outcome asset-id space (docs, "Asset IDs"): `asset_id = 100_000_000 + encoding`.
pub const OUTCOME_ASSET_BASE: u64 = 100_000_000;

/// The side index of the FIRST `sideSpecs` entry (ordinarily `Yes`) — the side that converts to
/// `settleFraction` quote units per token at settlement.
pub const SIDE_FIRST: u32 = 0;
/// The side index of the SECOND `sideSpecs` entry (ordinarily `No`) — converts to
/// `1 - settleFraction`.
pub const SIDE_SECOND: u32 = 1;

/// The settlement-fraction key on a `settledOutcome` response — the documented, verified name. It
/// appears on THAT response only, never on an `outcomeMeta` outcome row (module doc).
pub const SETTLE_FRACTION_KEY: &str = "settleFraction";

/// The HIP-4 asset encoding (docs, "Asset IDs"): `encoding = 10 * outcome + side`.
///
/// Worked example straight from the docs: outcome `1`, side `0` → `10`.
pub fn encoding(outcome: u32, side: u32) -> u64 {
    10 * u64::from(outcome) + u64::from(side)
}

/// The spot COIN string for an outcome side — `#<encoding>`. This is the order-placement /
/// `l2Book` identifier, and the string this module uses as the vike `symbol` (see
/// [`SettlementFill::coin`]).
pub fn spot_coin(outcome: u32, side: u32) -> String {
    format!("#{}", encoding(outcome, side))
}

/// The TOKEN name for an outcome side — `+<encoding>`. This is the string an outcome side token
/// carries in `spotClearinghouseState.balances[].coin`.
pub fn token_name(outcome: u32, side: u32) -> String {
    format!("+{}", encoding(outcome, side))
}

/// The numeric asset id for an outcome side — `100_000_000 + encoding`.
pub fn asset_id(outcome: u32, side: u32) -> u64 {
    OUTCOME_ASSET_BASE + encoding(outcome, side)
}

/// `VIKE_HL_OUTCOME=1` is the opt-in gate — default OFF. The EXACT string `"1"`, not a fuzzy truthy
/// parse (the `VIKE_RECONCILE` / `VIKE_PM_RESOLVE` discipline).
pub fn hl_outcome_enabled() -> bool {
    std::env::var("VIKE_HL_OUTCOME").as_deref() == Ok("1")
}

// ---------------------------------------------------------------------------------------------
// Parsed wire shapes
// ---------------------------------------------------------------------------------------------

/// One outcome row — an `outcomeMeta.outcomes[]` entry, or the `spec` object a `settledOutcome`
/// response wraps (the same shape). It carries NO settlement information: settlement lives only on
/// [`SettledOutcome`] (module doc).
#[derive(Debug, Clone, PartialEq)]
pub struct OutcomeSpec {
    /// The numeric outcome id (the `outcome` field), the `encoding` input.
    pub outcome: u32,
    /// Human name (`name`) — e.g. `"Recurring"` or a full question string.
    pub name: String,
    /// The pipe-delimited spec blob (`description`) — e.g.
    /// `class:priceBinary|underlying:BTC|expiry:20260504-0600|targetPrice:78213|period:1d`.
    pub description: String,
    /// Side labels in wire order, from `sideSpecs[].name` — index IS the side index.
    pub sides: Vec<String>,
    /// The quote token this outcome settles into (`quoteToken`), when the venue surfaced it —
    /// present on a `settledOutcome` `spec`, absent on an `outcomeMeta` row.
    pub quote_token: Option<String>,
}

impl OutcomeSpec {
    /// A binary outcome has exactly two sides. Payout is only derivable for these: for a
    /// categorical outcome (three or more sides) the single `settleFraction` scalar does not
    /// determine each side's payout, so such an outcome is never settled locally (see
    /// [`derive_outcome_settlements`]).
    pub fn is_binary(&self) -> bool {
        self.sides.len() == 2
    }
}

/// A parsed `settledOutcome` response — the AUTHORITATIVE settlement record for one outcome, and
/// the only thing in this module that can produce a fill.
#[derive(Debug, Clone, PartialEq)]
pub struct SettledOutcome {
    /// The `spec` object: the same row shape `outcomeMeta.outcomes[]` carries.
    pub spec: OutcomeSpec,
    /// `settleFraction` — side 0 converts to this many quote units per token, side 1 to `1 - this`.
    pub settle_fraction: f64,
    /// The venue's free-form resolution note (`details`), e.g. `"price:76876.9"`. Carried for
    /// diagnostics only; never parsed for control flow.
    pub details: String,
}

/// One `outcomeMeta.questions[]` row — a CATEGORICAL question grouping several outcomes.
/// `settled_named_outcomes` is the verified settled-set signal; it is corroborating only (it carries
/// no fraction), never sufficient to emit a fill on its own.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionSpec {
    pub question: u32,
    pub name: String,
    pub description: String,
    pub fallback_outcome: Option<u32>,
    pub named_outcomes: Vec<u32>,
    pub settled_named_outcomes: Vec<u32>,
}

/// The parsed `outcomeMeta` body.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutcomeMeta {
    pub outcomes: Vec<OutcomeSpec>,
    pub questions: Vec<QuestionSpec>,
}

impl OutcomeMeta {
    /// Outcome ids some question reports as settled (`questions[].settledNamedOutcomes`) — the
    /// verified-but-fractionless settled signal.
    pub fn settled_by_question(&self) -> HashSet<u32> {
        self.questions.iter().flat_map(|q| q.settled_named_outcomes.iter().copied()).collect()
    }
}

/// One `spotClearinghouseState.balances[]` row. All venue numerics are decimal STRINGS; `total` is
/// the full balance and `hold` the portion locked by resting orders.
#[derive(Debug, Clone, PartialEq)]
pub struct SpotBalance {
    pub coin: String,
    pub total: f64,
    pub hold: f64,
}

impl SpotBalance {
    /// The `(outcome, side)` this balance is an outcome side token of, decoded from a `+<encoding>`
    /// coin string; `None` for an ordinary spot coin (`USDC`, `PURR`, …). Inverse of
    /// [`token_name`]: `outcome = encoding / 10`, `side = encoding % 10`.
    pub fn outcome_side(&self) -> Option<(u32, u32)> {
        decode_token_name(&self.coin)
    }
}

/// Decode a `+<encoding>` outcome TOKEN name back to `(outcome, side)`. Returns `None` for any coin
/// string that is not a `+`-prefixed integer, or whose encoding exceeds the `u32` outcome space.
pub fn decode_token_name(coin: &str) -> Option<(u32, u32)> {
    let enc: u64 = coin.strip_prefix('+')?.parse().ok()?;
    let outcome = u32::try_from(enc / 10).ok()?;
    let side = u32::try_from(enc % 10).ok()?;
    Some((outcome, side))
}

/// Parse an `outcomeMeta` body. Missing/malformed sections degrade to empty rather than erroring —
/// only a body that is not JSON at all is an `Err` (the crate's `parse_*` discipline). Rows without
/// a numeric `outcome` / `question` id are skipped: an unidentifiable row can never be settled.
pub fn parse_outcome_meta(body: &str) -> Result<OutcomeMeta, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let outcomes = v
        .get("outcomes")
        .and_then(|o| o.as_array())
        .map(|arr| arr.iter().filter_map(parse_outcome_row).collect())
        .unwrap_or_default();
    let questions = v
        .get("questions")
        .and_then(|q| q.as_array())
        .map(|arr| arr.iter().filter_map(parse_question_row).collect())
        .unwrap_or_default();
    Ok(OutcomeMeta { outcomes, questions })
}

/// One `outcomes[]` row → [`OutcomeSpec`]; `None` when the row carries no usable `outcome` id.
fn parse_outcome_row(row: &Value) -> Option<OutcomeSpec> {
    let outcome = u32::try_from(row.get("outcome")?.as_u64()?).ok()?;
    let sides = row
        .get("sideSpecs")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .map(|s| s.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    Some(OutcomeSpec {
        outcome,
        name: row.get("name").map(json_str).unwrap_or_default(),
        description: row.get("description").map(json_str).unwrap_or_default(),
        sides,
        quote_token: row.get("quoteToken").map(json_str).filter(|q| !q.is_empty()),
    })
}

/// Parse a `settledOutcome` response body.
///
/// `Ok(Some(_))` ONLY when the body carries both a usable `spec` row and a numeric
/// `settleFraction`; `Ok(None)` when either is missing — which is how a not-yet-settled outcome
/// reads, and is the fail-closed direction (no fraction ⇒ no fill). Only a body that is not JSON at
/// all is an `Err`, matching the crate's `parse_*` discipline.
///
/// `json_num` accepts both the decimal-STRING form the documented response uses
/// (`"settleFraction": "0.0"`) and a bare JSON number, so the parse is insensitive to that choice.
pub fn parse_settled_outcome(body: &str) -> Result<Option<SettledOutcome>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let Some(spec) = v.get("spec").and_then(parse_outcome_row) else {
        return Ok(None);
    };
    let Some(settle_fraction) = v.get(SETTLE_FRACTION_KEY).and_then(json_num) else {
        return Ok(None);
    };
    Ok(Some(SettledOutcome {
        spec,
        settle_fraction,
        details: v.get("details").map(json_str).unwrap_or_default(),
    }))
}

/// One `questions[]` row → [`QuestionSpec`]; `None` when the row carries no usable `question` id.
fn parse_question_row(row: &Value) -> Option<QuestionSpec> {
    let question = u32::try_from(row.get("question")?.as_u64()?).ok()?;
    Some(QuestionSpec {
        question,
        name: row.get("name").map(json_str).unwrap_or_default(),
        description: row.get("description").map(json_str).unwrap_or_default(),
        fallback_outcome: row
            .get("fallbackOutcome")
            .and_then(|f| f.as_u64())
            .and_then(|f| u32::try_from(f).ok()),
        named_outcomes: u32_array(row.get("namedOutcomes")),
        settled_named_outcomes: u32_array(row.get("settledNamedOutcomes")),
    })
}

/// A JSON array of outcome ids → `Vec<u32>` (absent/malformed ⇒ empty).
fn u32_array(v: Option<&Value>) -> Vec<u32> {
    v.and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter().filter_map(|x| x.as_u64()).filter_map(|x| u32::try_from(x).ok()).collect()
        })
        .unwrap_or_default()
}

/// Parse a `spotClearinghouseState` body's `balances[]`. Rows without a `coin` are skipped; absent
/// `total`/`hold` default to `0.0` (the venue omitting a numeric means none held).
pub fn parse_spot_balances(body: &str) -> Result<Vec<SpotBalance>, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    Ok(v.get("balances")
        .and_then(|b| b.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|row| {
                    Some(SpotBalance {
                        coin: row.get("coin").map(json_str).filter(|c| !c.is_empty())?,
                        total: row.get("total").and_then(json_num).unwrap_or(0.0),
                        hold: row.get("hold").and_then(json_num).unwrap_or(0.0),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

// ---------------------------------------------------------------------------------------------
// The pure derivation core
// ---------------------------------------------------------------------------------------------

/// One derived settlement: a held side token of a SETTLED outcome, with the payout its quantity
/// converts at and the deterministic synthetic fill id that closes it.
#[derive(Debug, Clone, PartialEq)]
pub struct SettlementFill {
    /// The outcome id.
    pub outcome: u32,
    /// The side index within `sideSpecs`.
    pub side: u32,
    /// The `#<encoding>` spot coin — the vike `symbol` this settlement fill lands on. The `#` form
    /// (not the `+` token name) is deliberate: it is the order-placement identifier, so it is the
    /// symbol the local position was built under by the trading path.
    pub coin: String,
    /// The side label from `sideSpecs[side].name` (e.g. `"Yes"`), carried for logging/diagnostics.
    pub side_name: String,
    /// The held quantity being closed (the balance `total`).
    pub qty: f64,
    /// Quote (USDH) units each token converts to: side 0 → `settleFraction`, side 1 →
    /// `1 - settleFraction`.
    pub payout: f64,
    /// The deterministic synthetic fill id — see [`settlement_trade_id`].
    pub trade_id: TradeId,
}

/// Payout per token for `side` at `settle_fraction` (HIP-4): side 0 converts to `settleFraction`
/// quote units, side 1 to `1 - settleFraction`. A "binary yes" is `settleFraction = 1` (side 0 pays
/// 1, side 1 pays 0); a "binary no" is `settleFraction = 0` (the mirror). `None` for any other side
/// index — only a binary outcome's payouts are determined by this one scalar.
pub fn payout_for_side(side: u32, settle_fraction: f64) -> Option<f64> {
    match side {
        SIDE_FIRST => Some(settle_fraction),
        SIDE_SECOND => Some(1.0 - settle_fraction),
        _ => None,
    }
}

/// The deterministic synthetic fill id for one settled position:
/// `hlsettle:<outcome>:<side>:<16-hex FNV-1a-64 of "<outcome>|<side>|<wallet lowercased>">`.
///
/// Derived, never counter-allocated — the same settled position on the same wallet yields the SAME
/// id on every process and every restart, which is what makes the engine's `seen_trade_ids` an
/// independent second guard and a journal replay non-duplicating. The wallet is lowercased first so
/// a checksummed `0xAbC…` and its lowercase form are ONE identity (HL addresses are case-insensitive
/// hex and the venue itself requires lowercased address fields on the wire).
///
/// Minted by vike, so it is built with [`TradeId::prefixed`] off the static `"hlsettle:"` tag —
/// non-empty by construction, no fallible path, and the rendered string is BYTE-IDENTICAL to the
/// `format!` this used to return (the id is a persisted dedup key: changing a character would make
/// an already-folded settlement look new and double-book it).
pub fn settlement_trade_id(outcome: u32, side: u32, wallet: &str) -> TradeId {
    let seed = format!("{outcome}|{side}|{}", wallet.to_ascii_lowercase());
    TradeId::prefixed(
        "hlsettle:",
        format_args!("{outcome}:{side}:{:016x}", fnv1a64(seed.as_bytes())),
    )
}

/// FNV-1a 64-bit. Inlined (rather than a new dep) because it is used purely as an IDENTITY tag for
/// synthetic fill ids — never as a security primitive, and never on a hot path.
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(PRIME);
    }
    h
}

/// Which outcome ids are worth a `settledOutcome` query this tick: those this wallet still HOLDS a
/// non-zero side token of, minus those `outcomeMeta` still lists as live.
///
/// The subtraction is purely an economy measure — a live-listed outcome is known-unsettled, so
/// querying it would spend an `/info` request to learn nothing. Correctness does not rest on it:
/// the fraction can only ever come from `settledOutcome`, so if a settled outcome lingered in
/// `outcomeMeta` the effect would be a DELAYED settlement, never a wrong one. Deduped and sorted so
/// the query order (and thus the tick) is deterministic.
pub fn settlement_candidates(meta: &OutcomeMeta, balances: &[SpotBalance]) -> Vec<u32> {
    let live: HashSet<u32> = meta.outcomes.iter().map(|o| o.outcome).collect();
    let mut ids: Vec<u32> = balances
        .iter()
        .filter(|b| b.total != 0.0)
        .filter_map(|b| b.outcome_side())
        .map(|(outcome, _)| outcome)
        .filter(|o| !live.contains(o))
        .collect::<HashSet<u32>>()
        .into_iter()
        .collect();
    ids.sort_unstable();
    ids
}

/// **The pure core.** Given the `settledOutcome` records resolved this tick and the wallet's spot
/// balances, derive one [`SettlementFill`] per `(outcome, side)` that is BOTH settled and held.
///
/// Rules, each of which fails CLOSED (no fill) rather than fabricating a realized PnL:
/// 1. A balance is a candidate only if its `coin` decodes as a `+<encoding>` outcome token
///    ([`decode_token_name`]) and its `total` is non-zero — a flat token has nothing to close.
/// 2. Its outcome must appear in `settled` — i.e. a `settledOutcome` response actually carried a
///    `settleFraction` for it. Nothing else in the protocol counts: `outcomeMeta` has no settlement
///    field, and `questions[].settledNamedOutcomes` carries no fraction.
/// 3. The outcome must be binary ([`OutcomeSpec::is_binary`]) and the side index must be one the
///    fraction determines ([`payout_for_side`]) — a categorical outcome's per-side payout is not
///    derivable from one scalar.
///
/// Output is deterministic and ordered by `(outcome, side)` so repeated runs over the same snapshot
/// produce byte-identical results.
pub fn derive_outcome_settlements(
    settled: &[SettledOutcome],
    balances: &[SpotBalance],
    wallet: &str,
) -> Vec<SettlementFill> {
    let mut out: Vec<SettlementFill> = balances
        .iter()
        .filter_map(|bal| {
            let (outcome, side) = bal.outcome_side()?;
            if bal.total == 0.0 {
                return None;
            }
            let record = settled.iter().find(|s| s.spec.outcome == outcome)?;
            if !record.spec.is_binary() {
                return None;
            }
            let payout = payout_for_side(side, record.settle_fraction)?;
            Some(SettlementFill {
                outcome,
                side,
                coin: spot_coin(outcome, side),
                side_name: record.spec.sides.get(side as usize).cloned().unwrap_or_default(),
                qty: bal.total,
                payout,
                trade_id: settlement_trade_id(outcome, side, wallet),
            })
        })
        .collect();
    out.sort_by_key(|s| (s.outcome, s.side));
    out
}

/// The synthetic closing [`FillEvent`] for a derived settlement.
///
/// `signed_qty` is the LOCAL signed position being closed (positive = long). The fill is its exact
/// inverse — `side = -sign(signed_qty)`, `last_qty = |signed_qty|` — so `Account::fold` reduces the
/// position to zero and books the realized PnL of the closed portion into `closed_pnls`. A long that
/// settled at 1.0 realizes `(1.0 - avg_px) * qty`; a long that settled at 0.0 realizes
/// `-avg_px * qty`.
///
/// Field construction mirrors the crate's existing `FillEvent` sites, with three deliberate
/// differences: `trade_id` is the derived [`settlement_trade_id`], `liquidity_side` is empty (a
/// settlement is neither maker nor taker — the model's documented "venue did not surface it"), and
/// `mark_price` stays `None` so a settlement never writes the price board (the position is flat
/// afterwards, so there is nothing left to mark). `commission` is `0.0`: HL charges settlement fees
/// on the venue side, and this module only mirrors the position close — it is not a fee oracle.
pub fn settlement_fill_event(fill: &SettlementFill, signed_qty: f64, ts: i64) -> FillEvent {
    let side = vike_model::closing_side(signed_qty);
    FillEvent {
        trade_id: fill.trade_id.clone(),
        client_order_id: format!("hlsettle:{}:{}", fill.outcome, fill.side),
        venue: VENUE.to_string().into(),
        symbol: fill.coin.clone().into(),
        side,
        last_qty: signed_qty.abs(),
        last_px: fill.payout,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: LiquiditySide::Unknown,
        ts,
        mark_price: None,
        position_side: "BOTH".to_string().into(),
    }
}

// ---------------------------------------------------------------------------------------------
// Ledger
// ---------------------------------------------------------------------------------------------

/// The at-most-once key: one settlement per held `(outcome, side)` of one wallet. Keyed per SIDE,
/// not per outcome — a wallet holding BOTH legs of one settled market has TWO distinct local
/// positions to close (one at `settleFraction`, one at `1 - settleFraction`), so a per-outcome key
/// would silently drop the second.
pub fn settlement_key(outcome: u32, side: u32, wallet: &str) -> String {
    format!("{}:{outcome}:{side}", wallet.to_ascii_lowercase())
}

/// Persisted set of already-settled keys — the at-most-once store across process restarts. A
/// structural mirror of `vike_polymarket::resolve::SettlementLedger` (one key per line, thread-safe,
/// best-effort persistence: a write failure warns but never kills the poller, and the in-memory set
/// still guards the running session).
pub struct SettlementLedger {
    path: PathBuf,
    seen: Mutex<HashSet<String>>,
}

impl SettlementLedger {
    /// Open, loading any existing keys. A missing/unreadable file is an empty ledger.
    pub fn open(path: PathBuf) -> Self {
        let mut seen = HashSet::new();
        if let Ok(txt) = std::fs::read_to_string(&path) {
            for line in txt.lines() {
                let k = line.trim();
                if !k.is_empty() {
                    seen.insert(k.to_string());
                }
            }
        }
        SettlementLedger { path, seen: Mutex::new(seen) }
    }

    pub fn contains(&self, outcome: u32, side: u32, wallet: &str) -> bool {
        self.seen.lock().unwrap().contains(&settlement_key(outcome, side, wallet))
    }

    /// Record this position as settled (in-memory + append to disk). Idempotent; a persist error
    /// warns only — the in-memory guard still holds for the session.
    pub fn mark(&self, outcome: u32, side: u32, wallet: &str) {
        let key = settlement_key(outcome, side, wallet);
        let newly = self.seen.lock().unwrap().insert(key.clone());
        if !newly {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{key}") {
                    tracing::warn!(%key, %e, "hl_outcome: ledger persist failed (in-memory guard holds)");
                }
            }
            Err(e) => tracing::warn!(%key, %e, "hl_outcome: ledger open-for-append failed"),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The tick
// ---------------------------------------------------------------------------------------------

/// Test seam: everything the loop needs from the outside world. [`ProdOutcomeDeps`] is the real
/// transport wiring; tests inject a scripted stub so no test touches the network.
pub trait OutcomeDeps {
    /// Keyless `POST /info {"type":"outcomeMeta"}`, parsed.
    fn fetch_outcome_meta(&self) -> Result<OutcomeMeta, String>;

    /// Keyless `POST /info {"type":"spotClearinghouseState","user":wallet}`, `balances[]` parsed.
    fn fetch_spot_balances(&self, wallet: &str) -> Result<Vec<SpotBalance>, String>;

    /// Keyless `POST /info {"type":"settledOutcome","outcome":<id>}` — the settlement oracle.
    /// `Ok(None)` means "not settled" (no `settleFraction` in the response); `Err` is a transport or
    /// JSON failure, which [`settle_once`] skips for that outcome only.
    fn fetch_settled_outcome(&self, outcome: u32) -> Result<Option<SettledOutcome>, String>;

    /// OPTIONAL local-book override: the LOCAL signed position for `coin` (the `#<encoding>` spot
    /// coin), when the caller can supply it (e.g. reading the core's `CoreSnapshot`). Default `None`
    /// ⇒ fall back to the venue balance, treated as a long — correct whenever the local book was
    /// built from this wallet's own trading.
    ///
    /// It exists because the two can legitimately disagree: the venue balance may include tokens
    /// this process never traded (bought in the HL UI, transferred in), and settling THAT quantity
    /// would push the local position negative instead of flat. `Some(size)` closes exactly `size`;
    /// `Some(0.0)` means "locally flat" and the settlement is skipped (nothing to close).
    fn local_position(&self, _coin: &str) -> Option<f64> {
        None
    }
}

/// The production [`OutcomeDeps`]: real keyless `/info` reads over the shared transport, no
/// local-book override.
pub struct ProdOutcomeDeps {
    transport: HyperliquidTransport,
}

impl ProdOutcomeDeps {
    pub fn new(transport: HyperliquidTransport) -> Self {
        ProdOutcomeDeps { transport }
    }
}

impl OutcomeDeps for ProdOutcomeDeps {
    fn fetch_outcome_meta(&self) -> Result<OutcomeMeta, String> {
        let body =
            self.transport.info(&json!({ "type": "outcomeMeta" })).map_err(|e| e.msg)?.to_string();
        parse_outcome_meta(&body)
    }

    fn fetch_spot_balances(&self, wallet: &str) -> Result<Vec<SpotBalance>, String> {
        let body = self
            .transport
            .info(&json!({ "type": "spotClearinghouseState", "user": wallet }))
            .map_err(|e| e.msg)?
            .to_string();
        parse_spot_balances(&body)
    }

    fn fetch_settled_outcome(&self, outcome: u32) -> Result<Option<SettledOutcome>, String> {
        let body = self
            .transport
            .info(&json!({ "type": "settledOutcome", "outcome": outcome }))
            .map_err(|e| e.msg)?
            .to_string();
        parse_settled_outcome(&body)
    }
}

/// One [`settle_once`] pass's outcome. `settled`/`failed` carry the `#<encoding>` coins (the settled
/// unit); `skipped_flat` carries settlements skipped because the LOCAL book was already flat.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct OutcomeTickReport {
    pub settled: Vec<String>,
    pub failed: Vec<String>,
    pub skipped_flat: Vec<String>,
    /// Outcome ids whose `settledOutcome` query errored this tick — skipped, retried next tick.
    pub unresolved: Vec<u32>,
}

/// ONE fetch → derive → emit pass: the reusable core the poller thread calls on a timer.
///
/// `emit` is the event sink, returning `false` when the lane is gone — the same
/// `|e| events.blocking_send(e).is_ok()` shape the venue's user-data pump uses, which keeps this
/// fold testable against a plain `Vec` with no core thread.
///
/// The pass is: fetch `outcomeMeta` + this wallet's balances → [`settlement_candidates`] → one
/// `settledOutcome` query per candidate → [`derive_outcome_settlements`] → emit.
///
/// Either of the two snapshot fetches failing aborts the tick with an empty report (a partial
/// snapshot must never settle anything). A single `settledOutcome` query failing skips only THAT
/// outcome — the others still settle, and the failed one retries next tick. The ledger is marked
/// ONLY after the lane accepts the event, so a failed send is left unmarked and retried next tick.
/// Because [`derive_outcome_settlements`] is deterministic and the trade ids are derived, a retried
/// tick re-emits an IDENTICAL event the engine's `seen_trade_ids` absorbs.
pub fn settle_once(
    deps: &dyn OutcomeDeps,
    wallet: &str,
    ledger: &SettlementLedger,
    emit: &mut dyn FnMut(Event) -> bool,
) -> OutcomeTickReport {
    let mut report = OutcomeTickReport::default();

    let meta = match deps.fetch_outcome_meta() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(%e, "hl_outcome: outcomeMeta fetch failed this tick");
            return report;
        }
    };
    let balances = match deps.fetch_spot_balances(wallet) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(%e, "hl_outcome: spotClearinghouseState fetch failed this tick");
            return report;
        }
    };

    // The settlement oracle: one keyless query per held-but-no-longer-live outcome. A failure here
    // is per-outcome, not per-tick — the rest of the pass still settles.
    let mut settled: Vec<SettledOutcome> = Vec::new();
    for outcome in settlement_candidates(&meta, &balances) {
        match deps.fetch_settled_outcome(outcome) {
            Ok(Some(record)) => settled.push(record),
            // Held, absent from outcomeMeta, yet the oracle reports no fraction. Nothing to act on.
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(outcome, %e, "hl_outcome: settledOutcome fetch failed (skipped)");
                report.unresolved.push(outcome);
            }
        }
    }

    let ts = now_ms();
    for fill in derive_outcome_settlements(&settled, &balances, wallet) {
        if ledger.contains(fill.outcome, fill.side, wallet) {
            continue;
        }
        let signed_qty = deps.local_position(&fill.coin).unwrap_or(fill.qty);
        if signed_qty == 0.0 {
            // Locally flat — nothing to close. Mark it settled anyway so a permanently-flat token is
            // not re-derived every tick for the rest of the session.
            ledger.mark(fill.outcome, fill.side, wallet);
            report.skipped_flat.push(fill.coin.clone());
            continue;
        }
        let event = Event::Fill(settlement_fill_event(&fill, signed_qty, ts));
        if emit(event) {
            ledger.mark(fill.outcome, fill.side, wallet);
            tracing::info!(
                outcome = fill.outcome,
                side = fill.side,
                side_name = %fill.side_name,
                coin = %fill.coin,
                payout = fill.payout,
                qty = signed_qty,
                "hl_outcome: settled outcome position locally"
            );
            report.settled.push(fill.coin);
        } else {
            tracing::warn!(coin = %fill.coin, "hl_outcome: event lane gone — settlement not marked");
            report.failed.push(fill.coin);
        }
    }
    report
}

// ---------------------------------------------------------------------------------------------
// Poller
// ---------------------------------------------------------------------------------------------

/// Owner-side handle: stop-aware shutdown of the poller thread, `Drop`-joining — the shared
/// [`StopHandle`] scaffold (`vike_bridge_core::poller`), so a dropped handle never leaks the
/// background thread (the workspace's discipline — polymarket's `ResolveHandle`,
/// `AutoRedeemHandle`, `raw_tap.rs`).
pub type OutcomeHandle = StopHandle;

pub struct OutcomePoller;

impl OutcomePoller {
    /// Spawn the outcome-settlement poller thread. Returns `None` (never starts anything) unless a
    /// non-empty `wallet` address is supplied AND [`hl_outcome_enabled`] — so an unset
    /// `VIKE_HL_OUTCOME` leaves behavior byte-identical to before this module existed.
    ///
    /// The thread loop, every `interval`: one [`settle_once`] pass against the real
    /// [`ProdOutcomeDeps`], emitting settlement fills into `events` (the same lossless ingest lane
    /// the user-data pump pushes fills into). The only failure mode is the core lane being gone, and
    /// once that happens every later tick fails identically — the operator's remedy is shutting the
    /// handle, not per-key back-off.
    pub fn spawn(
        transport: HyperliquidTransport,
        wallet: String,
        ledger_path: PathBuf,
        interval: Duration,
        events: EventSender,
    ) -> Option<OutcomeHandle> {
        if wallet.trim().is_empty() {
            return None;
        }
        if !hl_outcome_enabled() {
            return None;
        }

        Some(spawn_poller("vike-hyperliquid-outcome", move |stop| {
            let deps = ProdOutcomeDeps::new(transport);
            let ledger = SettlementLedger::open(ledger_path);
            let mut emit = |e: Event| events.blocking_send(e).is_ok();

            while !stop.load(Ordering::Relaxed) {
                settle_once(&deps, &wallet, &ledger, &mut emit);
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

    const WALLET: &str = "0x1234567890abcdef1234567890abcdef12345678";

    /// The docs' own `outcomeMeta` sample (two independent integrator references reproduce this
    /// exact shape). Note there is NO settlement field anywhere on it — that is the point: a settled
    /// outcome is REMOVED from this response, and its fraction comes from `settledOutcome`.
    fn meta_body() -> String {
        r#"{
          "outcomes": [
            {
              "outcome": 9,
              "name": "Who will win the HL 100 meter dash?",
              "description": "This race is yet to be scheduled.",
              "sideSpecs": [{ "name": "Hypurr" }, { "name": "Usain Bolt" }]
            },
            {
              "outcome": 3151,
              "name": "Recurring",
              "description": "class:priceBinary|underlying:HYPE|expiry:20260404-1145|targetPrice:38|period:15m",
              "sideSpecs": [{ "name": "Yes" }, { "name": "No" }]
            }
          ],
          "questions": [
            {
              "question": 1,
              "name": "What will Hypurr eat the most of in Feb 2026?",
              "description": "Hypurr has committed to weighing and recording daily food intake.",
              "fallbackOutcome": 13,
              "namedOutcomes": [10, 11, 12],
              "settledNamedOutcomes": [11]
            }
          ]
        }"#
        .to_string()
    }

    /// `spotClearinghouseState` with an ordinary USDC row plus BOTH legs of outcome 3151
    /// (`+31510` = side 0, `+31511` = side 1) and one leg of the UNSETTLED outcome 9 (`+90`).
    fn balances_body() -> String {
        r#"{"balances":[
            {"coin":"USDC","token":0,"hold":"0.0","total":"14.62","entryNtl":"0.0"},
            {"coin":"+31510","token":900,"hold":"0.0","total":"25.0","entryNtl":"12.5"},
            {"coin":"+31511","token":901,"hold":"0.0","total":"10.0","entryNtl":"4.0"},
            {"coin":"+90","token":50,"hold":"0.0","total":"7.0","entryNtl":"3.5"}
        ]}"#
        .to_string()
    }

    /// The docs' own `settledOutcome` response sample, re-targeted at `outcome` and `fraction`.
    /// `settleFraction` is a decimal STRING exactly as documented.
    fn settled_body(outcome: u32, fraction: &str) -> String {
        format!(
            r#"{{
              "spec": {{
                "outcome": {outcome},
                "name": "Recurring",
                "description": "class:priceBinary|underlying:BTC|expiry:20260526-0600|targetPrice:77363|period:1d",
                "sideSpecs": [{{ "name": "Yes" }}, {{ "name": "No" }}],
                "quoteToken": "USDC"
              }},
              "settleFraction": "{fraction}",
              "details": "price:76876.9"
            }}"#
        )
    }

    /// `outcomeMeta` after outcome 3151 settled: the venue has REMOVED it, leaving only the live
    /// outcome 9. This is the shape the candidate filter is built against.
    fn live_meta_without_3151() -> OutcomeMeta {
        parse_outcome_meta(
            r#"{"outcomes":[{"outcome":9,"sideSpecs":[{"name":"Hypurr"},{"name":"Usain Bolt"}]}]}"#,
        )
        .unwrap()
    }

    /// The settled record for outcome 3151 at `fraction`.
    fn settled_3151(fraction: &str) -> Vec<SettledOutcome> {
        vec![parse_settled_outcome(&settled_body(3151, fraction)).unwrap().unwrap()]
    }

    // --- encoding ------------------------------------------------------------------------------

    #[test]
    fn encoding_matches_the_documented_worked_example() {
        // docs: outcome 1, side 0 -> encoding 10 -> "#10" / "+10" / 100000010.
        assert_eq!(encoding(1, 0), 10);
        assert_eq!(spot_coin(1, 0), "#10");
        assert_eq!(token_name(1, 0), "+10");
        assert_eq!(asset_id(1, 0), 100_000_010);
        // side 1 of the same outcome.
        assert_eq!(encoding(1, 1), 11);
        assert_eq!(token_name(1, 1), "+11");
    }

    #[test]
    fn decode_token_name_inverts_the_encoding() {
        for (outcome, side) in [(1u32, 0u32), (1, 1), (3151, 0), (3151, 1), (0, 0)] {
            assert_eq!(decode_token_name(&token_name(outcome, side)), Some((outcome, side)));
        }
        // Ordinary spot coins are not outcome tokens.
        assert_eq!(decode_token_name("USDC"), None);
        assert_eq!(decode_token_name("PURR"), None);
        // The "#" spot-coin form is NOT the token name form.
        assert_eq!(decode_token_name("#10"), None);
        assert_eq!(decode_token_name("+notanumber"), None);
    }

    // --- parsing -------------------------------------------------------------------------------

    #[test]
    fn parse_outcome_meta_reads_the_documented_shape() {
        let meta = parse_outcome_meta(&meta_body()).expect("valid json");
        assert_eq!(meta.outcomes.len(), 2);

        let dash = &meta.outcomes[0];
        assert_eq!(dash.outcome, 9);
        assert_eq!(dash.name, "Who will win the HL 100 meter dash?");
        assert_eq!(dash.sides, vec!["Hypurr".to_string(), "Usain Bolt".to_string()]);
        assert!(dash.is_binary());
        assert_eq!(dash.quote_token, None, "outcomeMeta rows carry no quoteToken");

        let recurring = &meta.outcomes[1];
        assert_eq!(recurring.outcome, 3151);
        assert!(recurring.description.starts_with("class:priceBinary|underlying:HYPE"));
        assert_eq!(recurring.sides, vec!["Yes".to_string(), "No".to_string()]);

        let q = &meta.questions[0];
        assert_eq!(q.question, 1);
        assert_eq!(q.fallback_outcome, Some(13));
        assert_eq!(q.named_outcomes, vec![10, 11, 12]);
        assert_eq!(q.settled_named_outcomes, vec![11]);
        assert_eq!(meta.settled_by_question(), HashSet::from([11]));
    }

    #[test]
    fn parse_settled_outcome_reads_the_documented_response() {
        let s = parse_settled_outcome(&settled_body(95, "0.0")).unwrap().expect("settled");
        assert_eq!(s.spec.outcome, 95);
        assert_eq!(s.spec.sides, vec!["Yes".to_string(), "No".to_string()]);
        assert_eq!(s.spec.quote_token, Some("USDC".to_string()));
        assert_eq!(s.settle_fraction, 0.0, "documented as a decimal STRING");
        assert_eq!(s.details, "price:76876.9");
    }

    #[test]
    fn settle_fraction_is_read_from_either_a_string_or_a_bare_number() {
        // Documented as a decimal string; a bare number is tolerated too.
        let spec = r#""spec":{"outcome":1,"sideSpecs":[{"name":"Yes"},{"name":"No"}]}"#;
        let s = parse_settled_outcome(&format!(r#"{{{spec},"settleFraction":"0.25"}}"#))
            .unwrap()
            .unwrap();
        assert_eq!(s.settle_fraction, 0.25);
        let n = parse_settled_outcome(&format!(r#"{{{spec},"settleFraction":0.25}}"#))
            .unwrap()
            .unwrap();
        assert_eq!(n.settle_fraction, 0.25);
    }

    #[test]
    fn parse_settled_outcome_fails_closed_without_a_fraction_or_spec() {
        // An unsettled/unknown outcome: a well-formed body carrying no fraction is NOT settled.
        let no_fraction = r#"{"spec":{"outcome":1,"sideSpecs":[{"name":"Yes"},{"name":"No"}]}}"#;
        assert_eq!(parse_settled_outcome(no_fraction).unwrap(), None);
        assert_eq!(parse_settled_outcome(r#"{"settleFraction":"1.0"}"#).unwrap(), None);
        assert_eq!(parse_settled_outcome("{}").unwrap(), None);
        assert_eq!(parse_settled_outcome("null").unwrap(), None);
        // Only a non-JSON body is an error.
        assert!(parse_settled_outcome("not json").is_err());
    }

    // --- candidate selection -------------------------------------------------------------------

    #[test]
    fn candidates_are_held_outcomes_the_live_meta_no_longer_lists() {
        let bals = parse_spot_balances(&balances_body()).unwrap();

        // While BOTH outcomes are still live, nothing is worth querying.
        let live_all = parse_outcome_meta(&meta_body()).unwrap();
        assert!(settlement_candidates(&live_all, &bals).is_empty());

        // Once 3151 is removed from outcomeMeta it becomes the one candidate — deduped across its
        // two held legs. Outcome 9 is still live, and USDC is not an outcome token.
        assert_eq!(settlement_candidates(&live_meta_without_3151(), &bals), vec![3151]);
    }

    #[test]
    fn a_flat_outcome_token_is_never_a_candidate() {
        let bals =
            parse_spot_balances(r#"{"balances":[{"coin":"+31510","hold":"0","total":"0.0"}]}"#)
                .unwrap();
        assert!(settlement_candidates(&live_meta_without_3151(), &bals).is_empty());
    }

    #[test]
    fn parse_outcome_meta_degrades_on_missing_sections_and_errors_only_on_bad_json() {
        assert_eq!(parse_outcome_meta("{}").unwrap(), OutcomeMeta::default());
        // A row without an `outcome` id is unidentifiable ⇒ skipped, not an error.
        let m = parse_outcome_meta(r#"{"outcomes":[{"name":"nameless"},{"outcome":4}]}"#).unwrap();
        assert_eq!(m.outcomes.len(), 1);
        assert_eq!(m.outcomes[0].outcome, 4);
        assert!(parse_outcome_meta("not json").is_err());
    }

    #[test]
    fn parse_spot_balances_reads_coin_total_hold() {
        let bals = parse_spot_balances(&balances_body()).expect("valid json");
        assert_eq!(bals.len(), 4);
        assert_eq!(bals[0].coin, "USDC");
        assert_eq!(bals[0].total, 14.62);
        assert_eq!(bals[0].outcome_side(), None);
        assert_eq!(bals[1].outcome_side(), Some((3151, 0)));
        assert_eq!(bals[1].total, 25.0);
        assert_eq!(bals[2].outcome_side(), Some((3151, 1)));
        assert_eq!(bals[3].outcome_side(), Some((9, 0)));
        assert!(parse_spot_balances("nope").is_err());
        assert!(parse_spot_balances("{}").unwrap().is_empty());
    }

    // --- payout --------------------------------------------------------------------------------

    #[test]
    fn payout_follows_the_hip4_settle_fraction_rule() {
        // "binary yes": settleFraction = 1 ⇒ side 0 pays 1, side 1 pays 0.
        assert_eq!(payout_for_side(0, 1.0), Some(1.0));
        assert_eq!(payout_for_side(1, 1.0), Some(0.0));
        // "binary no": settleFraction = 0 ⇒ the mirror.
        assert_eq!(payout_for_side(0, 0.0), Some(0.0));
        assert_eq!(payout_for_side(1, 0.0), Some(1.0));
        // A fractional settlement splits the quote unit.
        assert_eq!(payout_for_side(0, 0.25), Some(0.25));
        assert_eq!(payout_for_side(1, 0.25), Some(0.75));
        // Beyond the binary side pair the scalar determines nothing.
        assert_eq!(payout_for_side(2, 1.0), None);
    }

    // --- the derivation core -------------------------------------------------------------------

    #[test]
    fn derives_both_settled_legs_and_skips_the_unsettled_outcome() {
        let bals = parse_spot_balances(&balances_body()).unwrap();
        let fills = derive_outcome_settlements(&settled_3151("1.0"), &bals, WALLET);

        // Outcome 9 is unsettled (no fraction) and USDC is not an outcome token ⇒ only 3151's two
        // legs settle, ordered by (outcome, side).
        assert_eq!(fills.len(), 2);
        assert_eq!((fills[0].outcome, fills[0].side), (3151, 0));
        assert_eq!(fills[0].coin, "#31510");
        assert_eq!(fills[0].side_name, "Yes");
        assert_eq!(fills[0].qty, 25.0);
        assert_eq!(fills[0].payout, 1.0, "settleFraction 1.0 ⇒ the Yes leg won");
        assert_eq!((fills[1].outcome, fills[1].side), (3151, 1));
        assert_eq!(fills[1].side_name, "No");
        assert_eq!(fills[1].qty, 10.0);
        assert_eq!(fills[1].payout, 0.0, "the No leg is worthless");
    }

    #[test]
    fn no_settled_record_derives_nothing() {
        // The oracle returned nothing for any held outcome ⇒ nothing settles, however much is held.
        let bals = parse_spot_balances(&balances_body()).unwrap();
        assert!(derive_outcome_settlements(&[], &bals, WALLET).is_empty());
    }

    #[test]
    fn a_zero_balance_leg_is_not_settled() {
        // Partial holding: only the losing leg is held, the winning leg is flat.
        let bals = parse_spot_balances(
            r#"{"balances":[
                {"coin":"+31510","hold":"0.0","total":"0.0"},
                {"coin":"+31511","hold":"0.0","total":"10.0"}
            ]}"#,
        )
        .unwrap();
        let fills = derive_outcome_settlements(&settled_3151("1.0"), &bals, WALLET);
        assert_eq!(fills.len(), 1, "the flat leg has nothing to close");
        assert_eq!(fills[0].side, 1);
        assert_eq!(fills[0].payout, 0.0);
    }

    #[test]
    fn a_held_token_with_no_settled_record_is_skipped() {
        let bals =
            parse_spot_balances(r#"{"balances":[{"coin":"+77770","hold":"0","total":"5.0"}]}"#)
                .unwrap();
        assert!(derive_outcome_settlements(&settled_3151("1.0"), &bals, WALLET).is_empty());
    }

    #[test]
    fn a_non_binary_settled_outcome_is_never_settled_from_one_scalar() {
        let settled = vec![parse_settled_outcome(
            r#"{"spec":{"outcome":5,"sideSpecs":[{"name":"A"},{"name":"B"},{"name":"C"}]},
                    "settleFraction":"1.0"}"#,
        )
        .unwrap()
        .unwrap()];
        assert!(!settled[0].spec.is_binary());
        let bals = parse_spot_balances(r#"{"balances":[{"coin":"+50","hold":"0","total":"3.0"}]}"#)
            .unwrap();
        assert!(derive_outcome_settlements(&settled, &bals, WALLET).is_empty());
    }

    #[test]
    fn settled_named_outcomes_alone_never_settles_a_position() {
        // The question reports outcome 11 settled, but only `settledOutcome` carries a fraction —
        // so this signal is corroborating and can never, on its own, emit a fill.
        let meta = parse_outcome_meta(
            r#"{"outcomes":[],
                "questions":[{"question":1,"namedOutcomes":[11],"settledNamedOutcomes":[11]}]}"#,
        )
        .unwrap();
        assert_eq!(meta.settled_by_question(), HashSet::from([11]));
        let bals =
            parse_spot_balances(r#"{"balances":[{"coin":"+110","hold":"0","total":"9.0"}]}"#)
                .unwrap();
        assert!(
            derive_outcome_settlements(&[], &bals, WALLET).is_empty(),
            "a fractionless settled signal is corroborating only"
        );
    }

    // --- fill id determinism -------------------------------------------------------------------

    #[test]
    fn settlement_trade_id_is_deterministic_and_wallet_case_insensitive() {
        let a = settlement_trade_id(3151, 0, WALLET);
        let b = settlement_trade_id(3151, 0, WALLET);
        assert_eq!(a, b, "same inputs ⇒ same id, every process and restart");
        assert_eq!(
            a,
            settlement_trade_id(3151, 0, &WALLET.to_ascii_uppercase()),
            "a checksummed address and its lowercase form are ONE identity"
        );
        assert!(a.starts_with("hlsettle:3151:0:"), "id is prefixed + readable: {a}");
        assert_eq!(a.len(), "hlsettle:3151:0:".len() + 16, "16 hex digits of FNV-1a-64");
    }

    #[test]
    fn settlement_trade_id_separates_sides_outcomes_and_wallets() {
        let base = settlement_trade_id(3151, 0, WALLET);
        assert_ne!(base, settlement_trade_id(3151, 1, WALLET), "sides differ");
        assert_ne!(base, settlement_trade_id(3152, 0, WALLET), "outcomes differ");
        assert_ne!(
            base,
            settlement_trade_id(3151, 0, "0x00000000000000000000000000000000deadbeef"),
            "wallets differ"
        );
    }

    #[test]
    fn derived_fills_carry_the_deterministic_ids() {
        let bals = parse_spot_balances(&balances_body()).unwrap();
        let a = derive_outcome_settlements(&settled_3151("1.0"), &bals, WALLET);
        let b = derive_outcome_settlements(&settled_3151("1.0"), &bals, WALLET);
        assert_eq!(a, b, "the pure core is deterministic over one snapshot");
        assert_eq!(a[0].trade_id, settlement_trade_id(3151, 0, WALLET));
    }

    // --- the fill event ------------------------------------------------------------------------

    #[test]
    fn settlement_fill_event_is_the_inverse_of_the_local_position() {
        let bals = parse_spot_balances(&balances_body()).unwrap();
        let fills = derive_outcome_settlements(&settled_3151("1.0"), &bals, WALLET);

        // A long 25 winning leg closes with a SELL 25 @ 1.0.
        let ev = settlement_fill_event(&fills[0], 25.0, 1_700_000_000_000);
        assert_eq!(ev.side, -1);
        assert_eq!(ev.last_qty, 25.0);
        assert_eq!(ev.last_px, 1.0);
        assert_eq!(ev.symbol.as_str(), "#31510", "the # spot coin is the vike symbol");
        assert_eq!(ev.venue.as_str(), VENUE);
        assert_eq!(ev.trade_id, fills[0].trade_id);
        assert_eq!(ev.commission, 0.0);
        assert_eq!(
            ev.liquidity_side,
            LiquiditySide::Unknown,
            "a settlement is neither maker nor taker"
        );
        assert!(ev.mark_price.is_none(), "a settlement never writes the price board");
        assert_eq!(ev.ts, 1_700_000_000_000);

        // A SHORT position closes with a BUY of the same size.
        let short = settlement_fill_event(&fills[1], -10.0, 1);
        assert_eq!(short.side, 1);
        assert_eq!(short.last_qty, 10.0);
        assert_eq!(short.last_px, 0.0);
    }

    // --- ledger --------------------------------------------------------------------------------

    #[test]
    fn ledger_marks_are_idempotent_and_survive_reopen() {
        let dir = std::env::temp_dir().join(format!("hl_outcome_ledger_{}", std::process::id()));
        let path = dir.join("settled.txt");
        let _ = std::fs::remove_file(&path);

        let ledger = SettlementLedger::open(path.clone());
        assert!(!ledger.contains(3151, 0, WALLET));
        ledger.mark(3151, 0, WALLET);
        ledger.mark(3151, 0, WALLET); // idempotent
        assert!(ledger.contains(3151, 0, WALLET));
        assert!(!ledger.contains(3151, 1, WALLET), "the sibling leg is a distinct key");
        drop(ledger);

        let reopened = SettlementLedger::open(path.clone());
        assert!(reopened.contains(3151, 0, WALLET), "the mark survives a restart");
        // One line per key, not two.
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- the tick ------------------------------------------------------------------------------

    /// `outcomeMeta` as the venue serves it once outcome 3151 has settled: 3151 is REMOVED, only
    /// the live outcome 9 remains. This is the tick-level twin of [`live_meta_without_3151`].
    fn live_meta_body() -> String {
        r#"{"outcomes":[{"outcome":9,"name":"Who will win the HL 100 meter dash?",
            "sideSpecs":[{"name":"Hypurr"},{"name":"Usain Bolt"}]}]}"#
            .to_string()
    }

    /// Scripted stub: canned bodies + a canned settlement oracle + an optional local-book map, no
    /// network. By default the oracle reports outcome 3151 settled at `1.0` (the Yes leg won) and
    /// every other outcome unsettled.
    struct StubDeps {
        meta: String,
        balances: String,
        settled: Vec<(u32, String)>,
        local: Option<Vec<(String, f64)>>,
        fail_meta: bool,
        fail_settled: bool,
    }
    impl StubDeps {
        fn new(meta: String, balances: String) -> Self {
            StubDeps {
                meta,
                balances,
                settled: vec![(3151, "1.0".to_string())],
                local: None,
                fail_meta: false,
                fail_settled: false,
            }
        }
        fn with_local(mut self, local: Vec<(String, f64)>) -> Self {
            self.local = Some(local);
            self
        }
        /// No outcome is settled — the oracle answers every query with "not settled".
        fn with_nothing_settled(mut self) -> Self {
            self.settled.clear();
            self
        }
        /// Every `settledOutcome` query errors.
        fn failing_settled(mut self) -> Self {
            self.fail_settled = true;
            self
        }
    }
    impl OutcomeDeps for StubDeps {
        fn fetch_outcome_meta(&self) -> Result<OutcomeMeta, String> {
            if self.fail_meta {
                return Err("boom".to_string());
            }
            parse_outcome_meta(&self.meta)
        }
        fn fetch_spot_balances(&self, _wallet: &str) -> Result<Vec<SpotBalance>, String> {
            parse_spot_balances(&self.balances)
        }
        fn fetch_settled_outcome(&self, outcome: u32) -> Result<Option<SettledOutcome>, String> {
            if self.fail_settled {
                return Err("settledOutcome boom".to_string());
            }
            match self.settled.iter().find(|(o, _)| *o == outcome) {
                Some((o, fraction)) => parse_settled_outcome(&settled_body(*o, fraction)),
                None => Ok(None),
            }
        }
        fn local_position(&self, coin: &str) -> Option<f64> {
            self.local
                .as_ref()
                .map(|m| m.iter().find(|(c, _)| c == coin).map(|(_, q)| *q).unwrap_or(0.0))
        }
    }

    /// Collecting sink that always accepts.
    fn sink(out: &mut Vec<Event>) -> impl FnMut(Event) -> bool + '_ {
        move |e| {
            out.push(e);
            true
        }
    }

    fn tmp_ledger(tag: &str) -> (PathBuf, SettlementLedger) {
        let dir = std::env::temp_dir().join(format!("hl_outcome_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settled.txt");
        (dir, SettlementLedger::open(path))
    }

    #[test]
    fn settle_once_emits_one_fill_per_settled_leg() {
        let (dir, ledger) = tmp_ledger("emit");
        let deps = StubDeps::new(live_meta_body(), balances_body());
        let mut events = Vec::new();

        let report = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));

        assert_eq!(report.settled, vec!["#31510".to_string(), "#31511".to_string()]);
        assert!(report.failed.is_empty());
        assert_eq!(events.len(), 2);
        match &events[0] {
            Event::Fill(f) => {
                assert_eq!(f.symbol.as_str(), "#31510");
                assert_eq!(f.last_px, 1.0);
                assert_eq!(f.last_qty, 25.0);
                assert_eq!(f.side, -1);
            }
            other => panic!("expected a bare Event::Fill, got {other:?}"),
        }
        // No OrderFilled/OrderPartiallyFilled wraps are emitted (module doc).
        assert!(events.iter().all(|e| matches!(e, Event::Fill(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settle_once_is_idempotent_across_ticks() {
        let (dir, ledger) = tmp_ledger("idem");
        let deps = StubDeps::new(live_meta_body(), balances_body());
        let mut events = Vec::new();

        let first = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
        assert_eq!(first.settled.len(), 2);
        // The very same snapshot on the next tick settles NOTHING more.
        let second = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
        assert!(second.settled.is_empty());
        assert_eq!(events.len(), 2, "a rerun emits no duplicate fill");

        // And a fresh ledger reopened from the same file still suppresses them (restart guard).
        let path = dir.join("settled.txt");
        let reopened = SettlementLedger::open(path);
        let third = settle_once(&deps, WALLET, &reopened, &mut sink(&mut events));
        assert!(third.settled.is_empty());
        assert_eq!(events.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_send_leaves_the_key_unmarked_for_the_next_tick() {
        let (dir, ledger) = tmp_ledger("failsend");
        let deps = StubDeps::new(live_meta_body(), balances_body());

        // Lane gone: every emit rejects.
        let mut rejected = 0usize;
        let report = settle_once(&deps, WALLET, &ledger, &mut |_e| {
            rejected += 1;
            false
        });
        assert_eq!(report.failed, vec!["#31510".to_string(), "#31511".to_string()]);
        assert_eq!(rejected, 2);
        assert!(!ledger.contains(3151, 0, WALLET), "a failed send must not mark");

        // Lane back: the same tick's work is retried and settles.
        let mut events = Vec::new();
        let retry = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
        assert_eq!(retry.settled.len(), 2);
        assert_eq!(events.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_locally_flat_position_is_skipped_not_settled() {
        let (dir, ledger) = tmp_ledger("flat");
        // Local book knows nothing about either leg ⇒ both report 0.0.
        let deps = StubDeps::new(live_meta_body(), balances_body()).with_local(vec![]);
        let mut events = Vec::new();

        let report = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
        assert!(events.is_empty(), "nothing to close ⇒ no fabricated fill");
        assert_eq!(report.skipped_flat, vec!["#31510".to_string(), "#31511".to_string()]);
        assert!(ledger.contains(3151, 0, WALLET), "marked so it is not re-derived every tick");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_local_book_override_sizes_the_fill_not_the_venue_balance() {
        let (dir, ledger) = tmp_ledger("override");
        // Venue balance is 25 (some transferred in); the local book only ever traded 4.
        let deps = StubDeps::new(live_meta_body(), balances_body())
            .with_local(vec![("#31510".to_string(), 4.0)]);
        let mut events = Vec::new();

        settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
        assert_eq!(events.len(), 1, "the other leg is locally flat");
        match &events[0] {
            Event::Fill(f) => {
                assert_eq!(f.last_qty, 4.0, "closes the LOCAL size, not the venue balance")
            }
            other => panic!("expected Event::Fill, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_fetch_aborts_the_tick_without_settling_anything() {
        let (dir, ledger) = tmp_ledger("fetchfail");
        let mut deps = StubDeps::new(live_meta_body(), balances_body());
        deps.fail_meta = true;
        let mut events = Vec::new();

        let report = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
        assert_eq!(report, OutcomeTickReport::default());
        assert!(events.is_empty(), "a partial snapshot must never settle");
        assert!(!ledger.contains(3151, 0, WALLET));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_outcome_the_oracle_reports_unsettled_never_settles() {
        // Held, and gone from outcomeMeta — but `settledOutcome` carries no fraction, so nothing is
        // acted on. This is the fail-closed direction the whole design rests on.
        let (dir, ledger) = tmp_ledger("notsettled");
        let deps = StubDeps::new(live_meta_body(), balances_body()).with_nothing_settled();
        let mut events = Vec::new();

        let report = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
        assert_eq!(report, OutcomeTickReport::default());
        assert!(events.is_empty(), "no fraction ⇒ no fill ⇒ nothing fabricated");
        assert!(!ledger.contains(3151, 0, WALLET));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_settled_outcome_query_is_skipped_and_retried_not_fatal() {
        let (dir, ledger) = tmp_ledger("oraclefail");
        let deps = StubDeps::new(live_meta_body(), balances_body()).failing_settled();
        let mut events = Vec::new();

        let report = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
        assert_eq!(report.unresolved, vec![3151], "the outcome is reported, not settled");
        assert!(report.settled.is_empty());
        assert!(events.is_empty());
        assert!(!ledger.contains(3151, 0, WALLET), "an unresolved outcome must not mark");

        // The oracle recovers: the next tick settles normally.
        let ok = StubDeps::new(live_meta_body(), balances_body());
        let retry = settle_once(&ok, WALLET, &ledger, &mut sink(&mut events));
        assert_eq!(retry.settled.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_outcome_still_live_in_meta_is_never_queried_or_settled() {
        // outcome 9 is held (`+90`) and still live ⇒ it is not even a candidate.
        let (dir, ledger) = tmp_ledger("stilllive");
        let deps = StubDeps::new(meta_body(), balances_body());
        let mut events = Vec::new();

        let report = settle_once(&deps, WALLET, &ledger, &mut sink(&mut events));
        assert_eq!(report, OutcomeTickReport::default(), "everything held is still live");
        assert!(events.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- the opt-in gate -----------------------------------------------------------------------

    #[test]
    fn spawn_returns_none_without_a_wallet() {
        // Wallet is checked BEFORE the env gate, so this holds regardless of VIKE_HL_OUTCOME.
        let (tx, _rx) = vike_exec::event_channel(16);
        let handle = OutcomePoller::spawn(
            HyperliquidTransport::new(crate::config::Network::Testnet),
            "   ".to_string(),
            std::env::temp_dir().join("unused_hl_outcome.txt"),
            DEFAULT_POLL_INTERVAL,
            tx,
        );
        assert!(handle.is_none(), "no wallet ⇒ no thread");
    }

    #[test]
    fn the_env_gate_is_the_exact_string_one() {
        // Read-only assertion about the CURRENT process env: this test never mutates it (setting
        // env vars is process-global and would race the rest of the suite). The gate's contract is
        // simply that it mirrors the exact-"1" read.
        let expected = std::env::var("VIKE_HL_OUTCOME").as_deref() == Ok("1");
        assert_eq!(hl_outcome_enabled(), expected);
    }
}
