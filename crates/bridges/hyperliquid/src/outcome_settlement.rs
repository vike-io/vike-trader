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
//! It is the Hyperliquid twin of `vike_polymarket::exec_plane::settlement::resolve`, and deliberately mirrors that module's
//! shape one-for-one — pure derivation core + trait seam (`OutcomeDeps` ~ `ResolveDeps`), reusable
//! `settle_once` pass, stop-aware Drop-joining handle, at-most-once ledger, ONE bare `Event::Fill`
//! per settled position. It invents NO endpoint: all three reads (`outcomeMeta`,
//! `spotClearinghouseState`, `settledOutcome`) are keyless `/info` requests the existing
//! [`crate::transport::HyperliquidTransport`] already serves.
//!
//! **Opt-in, default OFF:** [`OutcomePoller::spawn`] returns `None` unless its caller passes
//! `enabled` (D4 of decision 0095: no composition root starts it, so it reads no setting of its
//! own; the `VIKE_HL_OUTCOME` variable it read before is retired and refuses startup) AND a
//! non-empty wallet address. Not started ⇒ no thread, no events, byte-identical to before this
//! module existed. It takes no on-chain action and moves no money — it only writes into our own core
//! — so there is no kill switch beyond shutting the handle. Cadence is deliberately slow (default
//! [`DEFAULT_POLL_INTERVAL`], 60s): settlement is an oracle posting a result, a human-timescale
//! event, and a tight loop buys nothing while spending the shared IP weight budget.
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
//! the engine's `dropped_unknown_coid` audit counter. Downstream, `vike_journal::materialize`'s order fold ignores
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
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::Duration;

use std::io::Write;

use serde_json::{Value, json};

use vike_bridge_core::json::{json_num, json_str};
use vike_bridge_core::poller::{STOP_POLL_SLICE, StopHandle, sleep_stop_aware, spawn_poller};
use vike_exec::EventSender;
use vike_model::events::{Event, FillEvent, LiquiditySide, TradeId};
use vike_model::now_ms;

use crate::consts::VENUE;
use crate::transport::HyperliquidTransport;

// ---------------------------------------------------------------------------------------------
// Constants + the verified encoding
// ---------------------------------------------------------------------------------------------

/// Default poll cadence. Settlement is an oracle posting a result — a human-timescale event — so a
/// minute-scale rhythm (matching `vike_polymarket::exec_plane::settlement::resolve`) is deliberate, not lazy.
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
/// structural mirror of `vike_polymarket::SettlementLedger` (one key per line, thread-safe,
/// best-effort persistence: a write failure warns but never kills the poller, and the in-memory set
/// still guards the running session).
pub struct SettlementLedger {
    path: PathBuf,
    seen: Mutex<HashSet<String>>,
}

impl SettlementLedger {
    /// Open, loading any existing keys. A missing/unreadable file is an empty ledger.
    pub fn open(path: PathBuf) -> Self {
        let seen = vike_bridge_core::poller::load_ledger_keys(&path);
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

pub struct OutcomePoller;

impl OutcomePoller {
    /// Spawn the outcome-settlement poller thread. Returns `None` (never starts anything) unless a
    /// non-empty `wallet` address is supplied AND its caller passes `enabled` (D4 of decision 0095)
    /// — so not starting it leaves behavior byte-identical to before this module existed.
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
        enabled: bool,
    ) -> Option<StopHandle> {
        if wallet.trim().is_empty() {
            return None;
        }
        if !enabled {
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

#[path = "outcome_settlement_tests.rs"]
#[cfg(test)]
mod outcome_settlement_tests;
