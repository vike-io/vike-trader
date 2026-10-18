//! The resting-order record + the frozen OrderRequest intent.
//! Exact ports of `core/orders.py::WorkingOrder` and `core/order_intent.py::OrderRequest`
//! (+ `order_request_to_working`, the 6-kind B2 dispatch).

use serde::{Deserialize, Serialize};

use crate::time::utc_day;

/// Resting-order kind. Serialized as the Python strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderKind {
    Market,
    MarketClose,
    LimitClose,
    Limit,
    Stop,
    Trailing,
}

/// A pending order (mutable resting record — engines ratchet `extreme` / cap `size` in place).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkingOrder {
    pub kind: OrderKind,
    /// +1 buy / -1 sell
    pub side: i32,
    pub size: f64,
    /// limit/stop trigger
    #[serde(default)]
    pub price: Option<f64>,
    /// trailing distance (absolute)
    #[serde(default)]
    pub trail: Option<f64>,
    /// running best price since submission (trailing only)
    #[serde(default)]
    pub extreme: Option<f64>,
    /// Transaction.Weight: cross-symbol fill priority (higher fills first)
    #[serde(default)]
    pub weight: f64,
    /// protective stop to arm when this entry fills (risk sizing; portfolio only)
    #[serde(default)]
    pub stop: Option<f64>,
    /// ENGINE-LOCAL identity handle — `0` means "not yet stamped", which is what every
    /// constructor produces and what every path that does not opt into an identity-keyed
    /// side-table leaves it at. Stamped (monotonically, per replay) by the vike-backtest
    /// queue-position model so a CANCELED order's queue state can never be adopted by a
    /// later order that merely happens to rest at the same (side, price); a re-submit builds
    /// a fresh `WorkingOrder`, hence a fresh identity. `#[serde(skip)]`: never on the wire,
    /// so the persisted/fixture schema is unchanged and a round-trip resets it to `0`.
    #[serde(skip)]
    pub qid: u64,
}

impl WorkingOrder {
    pub fn new(kind: OrderKind, side: i32, size: f64) -> Self {
        WorkingOrder {
            kind,
            side,
            size,
            price: None,
            trail: None,
            extreme: None,
            weight: 0.0,
            stop: None,
            qid: 0,
        }
    }
}

/// WorkingOrder time-in-force. Maps to FIX `TimeInForce(59)`; each venue maps it to its own TIF at the
/// edge. Default `Gtc`. Additive field — existing fixtures without it deserialize as `Gtc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TimeInForce {
    /// good-till-canceled (FIX 1)
    #[default]
    Gtc,
    /// immediate-or-cancel (FIX 3)
    Ioc,
    /// fill-or-kill (FIX 4)
    Fok,
    /// good-till-date — see [`OrderRequest::gtd_expiry`] (FIX 6)
    Gtd,
    /// day (FIX 0)
    Day,
}

impl TimeInForce {
    /// Every variant, in declaration order. A HAND list, not complete by construction: its
    /// completeness is held by the test `time_in_force_all_is_every_variant_in_declaration_order`
    /// beside this module, whose exhaustive `match` stops compiling when a variant is added.
    pub const ALL: &'static [TimeInForce] =
        &[TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::Gtd, TimeInForce::Day];
}

/// What KIND of order an [`OrderRequest`] is — the platform's own vocabulary, lowercase on the
/// wire (`"market"`, `"limit"`, `"stop"`, `"stop_limit"`, `"take_profit"`). `stop` is a
/// stop-loss trigger, `take_profit` a take-profit trigger, `stop_limit` a stop whose fill is a
/// limit; all three are TRIGGER kinds ([`OrderType::is_trigger`]). Venue spellings
/// (`STOP_MARKET`, `LMT`, …) are never this type's business: each adapter maps the enum to its
/// own words in a `match` of its own.
///
/// [`OrderType::Unrecognized`] carries a word this platform does not know, VERBATIM. It exists so
/// that data written before this type existed, and a venue's own text on an order adopted from
/// it, survive a read and a re-write byte for byte; the core's capability preflight
/// (`vike_venues::venue_caps`'s `preflight_order_at`) refuses to SEND one.
///
/// **Two doors in, deliberately different:**
///
/// - **STORED data** — serde (`Deserialize`: the journal, snapshots, wire bytes a server already
///   accepted) and [`OrderType::from_stored`] (a venue's report): an EXACT match of the five
///   lowercase words, anything else becomes `Unrecognized(original)`. Never refuses, never
///   normalises, so re-serializing yields the bytes that were read (`"LIMIT"` stays `"LIMIT"`).
/// - **OPERATOR / CALLER input** — [`OrderType::parse`]: trims and ignores ASCII case
///   (`" Limit "` is `Limit`), and REFUSES anything else, naming the accepted words. No synonyms:
///   a venue word such as `stop_market` is not ours. It never yields `Unrecognized`.
///
/// `Default` is `Market` — what every engine's "else" branch has always treated an order as.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderType {
    /// an unpriced taker
    #[default]
    Market,
    /// a priced order at `price`
    Limit,
    /// a stop-loss trigger at `trigger_price`
    Stop,
    /// a stop trigger at `trigger_price` whose fill is a limit at `price`
    StopLimit,
    /// a take-profit trigger at `trigger_price`
    TakeProfit,
    /// a word outside the five, kept verbatim (see the type doc). MUST stay the LAST variant:
    /// serde tries the named variants first and falls back to this one.
    #[serde(untagged)]
    Unrecognized(String),
}

impl OrderType {
    /// The five recognised kinds, in declaration order. A HAND list: its completeness is held by
    /// the test `order_type_known_is_every_recognised_variant_in_declaration_order` beside this
    /// module, whose exhaustive `match` stops compiling when a variant is added.
    pub const KNOWN: &'static [OrderType] = &[
        OrderType::Market,
        OrderType::Limit,
        OrderType::Stop,
        OrderType::StopLimit,
        OrderType::TakeProfit,
    ];

    /// The wire word: the canonical lowercase word, or an `Unrecognized` kind's text verbatim.
    pub fn as_str(&self) -> &str {
        match self {
            OrderType::Market => "market",
            OrderType::Limit => "limit",
            OrderType::Stop => "stop",
            OrderType::StopLimit => "stop_limit",
            OrderType::TakeProfit => "take_profit",
            OrderType::Unrecognized(text) => text,
        }
    }

    /// One of the five kinds this platform knows (not [`OrderType::Unrecognized`]).
    pub fn is_recognized(&self) -> bool {
        !matches!(self, OrderType::Unrecognized(_))
    }

    /// A TRIGGER kind: `stop`, `stop_limit`, `take_profit` — exactly
    /// `vike_venues::venue_caps`'s `TRIGGER_KINDS`, which the capability preflight classifies
    /// by.
    pub fn is_trigger(&self) -> bool {
        matches!(self, OrderType::Stop | OrderType::StopLimit | OrderType::TakeProfit)
    }

    /// The STORED door (see the type doc): an exact match of a canonical word, else
    /// `Unrecognized(text)` verbatim. The same verdict as `Deserialize`, for text that arrives as
    /// a `&str` (a venue report's order type) rather than through serde.
    pub fn from_stored(text: &str) -> OrderType {
        OrderType::KNOWN
            .iter()
            .find(|k| k.as_str() == text)
            .cloned()
            .unwrap_or_else(|| OrderType::Unrecognized(text.to_owned()))
    }

    /// The INPUT door (see the type doc): an operator's or a caller's word, trimmed and ASCII
    /// case-insensitive. Anything that is not one of the five words is refused; the error's
    /// `Display` names the accepted words.
    pub fn parse(input: &str) -> Result<OrderType, UnknownOrderType> {
        let word = input.trim();
        OrderType::KNOWN
            .iter()
            .find(|k| k.as_str().eq_ignore_ascii_case(word))
            .cloned()
            .ok_or_else(|| UnknownOrderType { input: input.to_owned() })
    }

    /// The accepted words of [`OrderType::parse`], comma-separated, for refusal messages.
    pub fn accepted_words() -> String {
        OrderType::KNOWN.iter().map(OrderType::as_str).collect::<Vec<_>>().join(", ")
    }
}

impl core::fmt::Display for OrderType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// [`OrderType::parse`] refused its input: not one of the five order-type words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownOrderType {
    /// the refused input, verbatim
    pub input: String,
}

impl core::fmt::Display for UnknownOrderType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "unknown order type {:?}: expected one of {}",
            self.input,
            OrderType::accepted_words()
        )
    }
}

impl std::error::Error for UnknownOrderType {}

/// The price SERIES a trigger order's `trigger_price` is evaluated against (a full
/// trigger-type taxonomy, reduced to the three sources the reachable venues actually expose).
///
/// Unmodeled, this was a live-vs-backtest divergence: last=99, mark=101, SL=100 fires on a
/// mark-triggering venue (hyperliquid) but on no last-triggering one, and no emulator/backtest
/// could reproduce the timing. `Option<TriggerBy>` on [`OrderRequest`] models it; `None` means
/// "the venue's default source" — what each adapter produced before the field existed:
///
/// - binance perp: no `workingType` sent → venue default `CONTRACT_PRICE` (last)
/// - bybit:        hardcoded `triggerBy: "LastPrice"` (last)
/// - okx:          no `slTriggerPxType` sent → venue default `last`
/// - hyperliquid:  no field exists — triggers evaluate against MARK by venue law
/// - core emulator (`ConditionalBook`) / backtest: trade/bar prices (last)
///
/// `Some(source)` asks the venue adapter to honor that source; an adapter that cannot express it
/// on its wire must synthesize a terminal `OrderRejected` (deny loudly — never silently
/// substitute a different trigger series; the roster-gate philosophy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerBy {
    /// last traded price (the default series at every venue except hyperliquid)
    Last,
    /// venue mark price (perp fair price; hyperliquid's ONLY trigger series)
    Mark,
    /// venue underlying index price
    Index,
}

// ─────────────────────────────────────────────────────────────────────────────
// The ONE time-in-force expiry law (dedup A4).
//
// Extracted so the two sites that decide "is this resting order expired?" — the
// backtest paper exchange (`crates/vike-paper/src/client/expiry.rs`'s `Expiry` — `vike_backtest::paper`
// until the paper exchange left that crate — native venue-style
// terminalization as `OrderExpired`) and the live core's managed GTD/DAY sweep
// (`vike_core`'s `sweep_gtd_expiry`, which cancels the venue-resting order) — read
// the SAME decision from one place, closing the divergence where the live sweep
// enforced Gtd only and skipped Day. The TERMINAL vocabulary stays per-site (paper
// owns its book so it mints `OrderExpired`; the live sweep must round-trip a venue
// cancel, so its terminal is the venue's authoritative `OrderCanceled`) — this law
// is only the boolean "expired?" predicate, never the terminalization.
// ─────────────────────────────────────────────────────────────────────────────

/// Is a resting order with time-in-force `tif` expired as of `now_ms`? The single expiry decision
/// both the backtest paper book and the live managed sweep route through.
///
/// - `Gtc` / `Ioc` / `Fok`: never expired by this predicate. (`Ioc`/`Fok` are immediate-execution
///   semantics, not a resting deadline; they are enforced — if at all — on the arrival bar, not
///   here.)
/// - `Gtd`: expired once `now_ms >= deadline`, an INCLUSIVE boundary (at the deadline the order is
///   expired). A `Gtd` with no `gtd_expiry` has no deadline and never expires.
/// - `Day`: expired once `now_ms` lands on a strictly later UTC day than `day_anchor_ms` (the
///   order's session anchor). Same-day (including the anchor itself) is never expired.
///
/// `day_anchor_ms` is meaningful only for `Day`; callers pass any value (e.g. `now_ms`) for the
/// other variants. The caller owns choosing the anchor — the backtest anchors on the first bar the
/// order sees; the live core anchors on the order's creation wall-clock.
pub fn tif_expired(
    tif: TimeInForce,
    gtd_expiry: Option<i64>,
    day_anchor_ms: i64,
    now_ms: i64,
) -> bool {
    match tif {
        TimeInForce::Gtd => gtd_expiry.is_some_and(|deadline| now_ms >= deadline),
        TimeInForce::Day => utc_day(now_ms) > utc_day(day_anchor_ms),
        TimeInForce::Gtc | TimeInForce::Ioc | TimeInForce::Fok => false,
    }
}

/// An operator's side WORD → the [`OrderRequest::side`] sign: `buy` / `b` / `long` → `+1`,
/// `sell` / `s` / `short` → `-1`, ASCII case-insensitively; any other word is `None`, and each
/// surface words its own refusal.
///
/// The ONE vocabulary every text surface that places an order reads a side through — the CLI's
/// order verbs and the Telegram `/submit` — so a word one surface accepts the other cannot refuse.
/// `side` itself stays `i32` (`docs/decisions/0018-typization-in-tiers-side-stays-i32.md`); this
/// is only the spelling an operator types.
#[must_use]
pub fn side_from_word(word: &str) -> Option<i32> {
    const BUY: [&str; 3] = ["buy", "b", "long"];
    const SELL: [&str; 3] = ["sell", "s", "short"];
    if BUY.iter().any(|w| word.eq_ignore_ascii_case(w)) {
        Some(1)
    } else if SELL.iter().any(|w| word.eq_ignore_ascii_case(w)) {
        Some(-1)
    } else {
        None
    }
}

/// A submission intent — what a strategy / manual ticket / (later) bracket builds.
///
/// `side` is +1 buy / -1 sell. `order_type` is an [`OrderType`] (the trigger kinds take a
/// `trigger_price` and are market/limit by whether `price` is set; a venue whose row does not
/// declare a kind has it refused by the capability preflight); `price` is the limit price,
/// `trigger_price` the trigger level. The contingency slots are RESERVED for OCO/brackets.
/// The five trailing fields (`weight`/`stop`/`trail`/`extreme`/`on_close`) are backtest-only
/// additive fields — the live path ignores them.
///
/// `Default` is derived so callers can build with `OrderRequest { ..Default::default() }` on the
/// hot path (the live runtime's per-order construction) without a serde round-trip. Every field's
/// Rust `Default` equals its `#[serde(default)]`, pinned by `default_literal_matches_serde` below.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OrderRequest {
    pub client_order_id: String,
    pub venue: String,
    /// canonical symbol (resolver maps to the venue symbol at the edge)
    pub symbol: String,
    pub side: i32,
    pub qty: f64,
    /// the order kind; serde field name `order_type`, the lowercase word (see [`OrderType`])
    pub order_type: OrderType,
    #[serde(default)]
    pub price: Option<f64>,
    #[serde(default)]
    pub trigger_price: Option<f64>,
    #[serde(default)]
    pub reduce_only: bool,
    /// Time-in-force (default GTC). Venues map it to their own TIF at the edge.
    #[serde(default)]
    pub time_in_force: TimeInForce,
    /// Good-till-date expiry (epoch ms) — only meaningful for [`TimeInForce::Gtd`].
    #[serde(default)]
    pub gtd_expiry: Option<i64>,
    #[serde(default)]
    pub ts: i64,
    // --- reserved contingency (OCO/brackets) ---
    #[serde(default)]
    pub parent_order_id: Option<String>,
    #[serde(default)]
    pub linked_order_ids: Vec<String>,
    #[serde(default)]
    pub order_list_id: Option<String>,
    /// 'OTO' | 'OCO' | 'OUO' later
    #[serde(default)]
    pub contingency_type: Option<String>,
    // --- backtest-only additive fields ---
    #[serde(default)]
    pub weight: f64,
    #[serde(default)]
    pub stop: Option<f64>,
    #[serde(default)]
    pub trail: Option<f64>,
    #[serde(default)]
    pub extreme: Option<f64>,
    #[serde(default)]
    pub on_close: bool,
    /// Combo legs — EMPTY means "not a combo", i.e. every existing order (see [`ComboLeg`]).
    ///
    /// Present ⇒ this ONE request IS the whole multi-leg order (never N children): `symbol` names
    /// the venue's combo instrument once the adapter has resolved it (it may be empty at submit
    /// time), `price` is the SIGNED NET limit per combo unit, `qty` is combo UNITS, and
    /// `order_type` stays [`OrderType::Limit`]/[`OrderType::Market`] as usual.
    ///
    /// Serde: `default` + `skip_serializing_if` empty, so every pre-existing wire body, fixture and
    /// journal record stays BYTE-IDENTICAL (`order_request_without_combo_legs_is_byte_identical`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub combo_legs: Vec<ComboLeg>,
    /// Requested margin/trade mode for THIS order (see [`crate::MarginMode`] and the per-venue
    /// `vike_venues::venue_margin_support` capability map).
    ///
    /// `None` (the default) means "the venue's current default behavior" — every adapter produces
    /// EXACTLY the wire bytes it produced before this field existed (OKX: the hardcoded
    /// `tdMode:"cross"`). `Some(mode)` asks the venue adapter to honor that mode; an adapter that
    /// cannot honor the requested mode on its trading surface must synthesize a terminal
    /// `OrderRejected` (deny loudly — never silently coerce, the roster-gate philosophy).
    ///
    /// Serde: `default` + `skip_serializing_if` `None`, so every pre-existing wire body, fixture
    /// and journal record stays BYTE-IDENTICAL
    /// (`order_request_without_margin_mode_is_byte_identical`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin_mode: Option<crate::MarginMode>,
    /// Requested trigger price SOURCE for trigger orders (`stop`/`take_profit`) — see
    /// [`TriggerBy`] for the per-venue `None` defaults. Meaningless on non-trigger orders
    /// (adapters ignore it there, exactly as they ignore `trigger_price`).
    ///
    /// `None` (the default) means "the venue's current default series" — every adapter produces
    /// EXACTLY the wire bytes it produced before this field existed. `Some(source)` asks the
    /// adapter to honor that source; an adapter that cannot express it must synthesize a terminal
    /// `OrderRejected` (deny loudly — never silently coerce).
    ///
    /// Serde: `default` + `skip_serializing_if` `None`, so every pre-existing wire body, fixture
    /// and journal record stays BYTE-IDENTICAL
    /// (`order_request_without_trigger_by_is_byte_identical`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_by: Option<TriggerBy>,
    /// WHICH ACCOUNT of `venue` this order is for. `None` means the sender named none, and resolves
    /// exactly as it did before this field existed.
    ///
    /// ⚠ NOT a route key. The payload's `venue` does TWO jobs — routing, and selecting the capability
    /// row that `vike_venues::caps_for` / `preflight_order_at` / `amend_semantics` are facts about.
    /// Collapsing the two into one per-account string makes the capability lookup read a name
    /// `vike_model::VENUES` does not contain, whereupon `preflight_order_at`'s unknown-venue
    /// affordance answers `Ok(())` and every capability check is skipped in silence. So `venue` stays
    /// canonical and this is the second coordinate, exactly as `vike_exec::ExecutionEngine` already
    /// splits `venue` from `route_key`.
    ///
    /// ⚠ **Serde goes through `crate::accounts::account_keys::wire_account_option`, NOT through
    /// `AccountLabel`'s own impls, and that is load-bearing rather than a style choice.** Those
    /// impls REFUSE to serialize `AccountLabel::Default` — correctly, for every carrier that can
    /// render the default account as an absent field. This one cannot: `Some(Default)` and `None`
    /// ROUTE DIFFERENTLY (on a two-account venue the first reaches the unlabelled book and the
    /// second is refused as ambiguous), and `vike_core`'s journal is required to record which
    /// account an order was for. Before the pair existed, a journaled submit naming `DEFAULT`
    /// reached an `.expect` on that refusal and killed the core's fold thread — the module doc on
    /// `wire_account_option` carries the incident and names the gate.
    ///
    /// A `Named` label's bytes are UNCHANGED by the pair, so every document already written reads
    /// back identically; the only value whose representation moved is the one no writer could
    /// emit.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::accounts::account_keys::wire_account_option"
    )]
    pub account: Option<crate::accounts::account_keys::AccountLabel>,
    /// The DELAY LABEL of the market data this order was priced off (decision 0140): what the
    /// venue said about `(venue, symbol)`'s stream when the live core passed the order through its
    /// pre-trade checks. The live core STAMPS it (`vike_core`'s submit path, beside the capability
    /// preflight) from the label its feeds last reported; a caller does not set it.
    ///
    /// A RECORD, never a gate: it neither refuses nor holds an order, and no adapter reads it.
    /// `None` means no label was stated for that stream (a venue that does not label its data, or
    /// none reported yet) — not "real-time".
    ///
    /// Serde: `default` + `skip_serializing_if` `None`, so every pre-existing wire body, fixture
    /// and journal record stays BYTE-IDENTICAL
    /// (`order_request_without_data_delay_is_byte_identical`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_delay: Option<crate::DataDelay>,
}

/// One combo leg: a venue instrument and its SIGNED ratio per combo unit.
///
/// `ratio` +2 = buy 2 units of this leg per combo BOUGHT; -1 = sell 1 per combo bought. Selling the
/// combo flips every leg (the venue convention, and LEAN's `GroupOrderManager` ratio).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComboLeg {
    pub symbol: String,
    pub ratio: i32,
}

/// Why a [`ComboSpec`] is not a legal combo. Returned by [`ComboSpec::validate`] and by
/// [`build_combo`]; also the `Deserialize` error, so an invalid spec cannot enter from a journal
/// record or a wire body either.
#[derive(Debug, Clone, PartialEq)]
pub enum ComboError {
    /// fewer than 2 legs — a 0-leg spec would lower to a NON-combo `OrderRequest` (an empty
    /// `combo_legs` IS the "not a combo" sentinel), a 1-leg spec is not an atomic multi-leg order
    TooFewLegs(usize),
    /// a leg with `ratio == 0` contributes nothing to the net and no quantity to the venue
    ZeroRatio(usize),
    /// a leg with an empty instrument name
    EmptyLegSymbol(usize),
    /// `side` must be exactly +1 or -1 (the workspace-wide convention; 0 is NOT a buy)
    InvalidSide(i32),
    /// `qty` (combo units) must be finite and > 0
    InvalidQty(f64),
    /// `net_limit`, when present, must be finite (its SIGN is free — credit combos are negative)
    NonFiniteNetLimit(f64),
}

impl core::fmt::Display for ComboError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooFewLegs(n) => write!(f, "combo needs 2..=N legs, got {n}"),
            Self::ZeroRatio(i) => write!(f, "combo leg {i} has ratio 0"),
            Self::EmptyLegSymbol(i) => write!(f, "combo leg {i} has an empty symbol"),
            Self::InvalidSide(s) => write!(f, "combo side must be +1 or -1, got {s}"),
            Self::InvalidQty(q) => write!(f, "combo qty must be finite and > 0, got {q}"),
            Self::NonFiniteNetLimit(p) => write!(f, "combo net_limit must be finite, got {p}"),
        }
    }
}

impl std::error::Error for ComboError {}

/// An atomic multi-leg order at a NET price. RUST-NATIVE (no Python twin).
///
/// Ports the LEAN combo-order vocabulary (`Orders/ComboOrder*.cs`) onto the vike model. A combo is
/// ONE order everywhere downstream — one coid, one [`crate::events::Event`] stream, one
/// `ManagedOrder` — because v1 is venue-native (Deribit combo books) and the VENUE is the group
/// manager.
///
/// INVARIANTS (enforced by [`ComboSpec::validate`], which [`build_combo`] and `Deserialize` both
/// run — there is no way to obtain an unvalidated spec from outside this module's constructors):
/// ≥ 2 legs, every `ratio != 0`, every leg symbol non-empty, `side ∈ {-1, +1}`, `qty` finite > 0,
/// `net_limit` finite when present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ComboSpecWire")]
pub struct ComboSpec {
    pub venue: String,
    /// +1 buy the combo / -1 sell it (legs flip sign with it, venue-side). NEVER 0.
    pub side: i32,
    /// combo units (leg qty = |ratio| × qty, on the leg's own grid)
    pub qty: f64,
    /// 2..=N legs, ordered — leg order is the venue-creation order (Deribit normalizes it)
    pub legs: Vec<ComboLeg>,
    /// NET limit per combo unit, SIGNED: Σ(ratio_i × px_i). Debit > 0, credit < 0, zero legal
    /// (Deribit futures spreads quote far-minus-near — negative in backwardation).
    /// `None` = combo MARKET order.
    ///
    /// NOTHING anywhere may clamp this to ≥ 0 — and, equally binding, no consumer may use the
    /// SIGNED net as a MAGNITUDE. A risk/notional/margin check that wants a size must take
    /// `net_limit.abs()` (or a per-leg gross Σ|ratio_i·px_i|·qty); feeding the signed net into a
    /// `notional` makes every credit combo look negative-sized (see `vike_exec::RiskGate::check`).
    pub net_limit: Option<f64>,
    #[serde(default)]
    pub time_in_force: TimeInForce,
}

/// Deserialization shadow of [`ComboSpec`] — exists ONLY so `#[serde(try_from)]` can route every
/// decoded spec through [`ComboSpec::validate`]. Keep the fields in sync with `ComboSpec`.
#[derive(Deserialize)]
struct ComboSpecWire {
    venue: String,
    side: i32,
    qty: f64,
    legs: Vec<ComboLeg>,
    net_limit: Option<f64>,
    #[serde(default)]
    time_in_force: TimeInForce,
}

impl TryFrom<ComboSpecWire> for ComboSpec {
    type Error = ComboError;
    fn try_from(w: ComboSpecWire) -> Result<Self, Self::Error> {
        let spec = Self {
            venue: w.venue,
            side: w.side,
            qty: w.qty,
            legs: w.legs,
            net_limit: w.net_limit,
            time_in_force: w.time_in_force,
        };
        spec.validate()?;
        Ok(spec)
    }
}

impl ComboSpec {
    /// Check every invariant documented on the struct. Cheap, allocation-free, and the ONE place
    /// the combo invariants live — `build_combo` and `Deserialize` both call it.
    pub fn validate(&self) -> Result<(), ComboError> {
        if self.legs.len() < 2 {
            return Err(ComboError::TooFewLegs(self.legs.len()));
        }
        for (i, leg) in self.legs.iter().enumerate() {
            if leg.ratio == 0 {
                return Err(ComboError::ZeroRatio(i));
            }
            if leg.symbol.is_empty() {
                return Err(ComboError::EmptyLegSymbol(i));
            }
        }
        if self.side != 1 && self.side != -1 {
            return Err(ComboError::InvalidSide(self.side));
        }
        if !self.qty.is_finite() || self.qty <= 0.0 {
            return Err(ComboError::InvalidQty(self.qty));
        }
        if let Some(p) = self.net_limit
            && !p.is_finite()
        {
            return Err(ComboError::NonFiniteNetLimit(p));
        }
        Ok(())
    }
}

/// The NET price of one combo unit: `Σ(ratio_i × px_i)` — the ONE sign law every layer shares
/// (model, paper fill, risk, venue adapter). SIGNED: a credit structure yields a NEGATIVE net.
///
/// Naive left-to-right fold in leg order deliberately (leg order is the venue-creation order, so
/// the sum order is part of the contract — do NOT reorder or compensate).
///
/// PRICE SIDE is the CALLER's contract and this function cannot check it: `px` must be the price
/// the combo would ACTUALLY trade that leg at, per leg. Buying the combo (`side == +1`) pays the
/// ASK on `+ratio` legs and receives the BID on `-ratio` legs; selling flips both. Feeding mid or
/// last uniformly yields an optimistic net — use [`combo_net_from_legs`], which picks the side for
/// you from the combo side and each leg's own ratio sign.
pub fn combo_net(legs: &[(i32, f64)]) -> f64 {
    let mut net = 0.0;
    for (ratio, px) in legs {
        net += f64::from(*ratio) * *px;
    }
    net
}

/// [`combo_net`] over real [`ComboLeg`]s, with the book side decided ONCE instead of at each call
/// site: `px(symbol, want_ask)` is asked for the ASK when the combo would BUY that leg and the BID
/// when it would SELL it (`want_ask = side * ratio > 0`).
///
/// Leg order is preserved verbatim (the venue-creation order), so the fold order matches
/// [`combo_net`]'s contract.
pub fn combo_net_from_legs(side: i32, legs: &[ComboLeg], px: impl Fn(&str, bool) -> f64) -> f64 {
    let mut net = 0.0;
    for leg in legs {
        let want_ask = side.signum() * leg.ratio.signum() > 0;
        net += f64::from(leg.ratio) * px(&leg.symbol, want_ask);
    }
    net
}

/// LEAN `ComboLimitFill`'s aggregate crossing law: `Some(net)` when the achievable net crosses the
/// limit, `None` otherwise.
///
/// buy (`side == +1`): fills when `net <= limit`; sell (`side == -1`): fills when `net >= limit`.
/// Credit combos make BOTH `net` and `limit` negative — there is no `≥ 0` clamp anywhere on this
/// path, by design.
///
/// `side == 0` is NOT a buy: the workspace convention is strictly ±1 (see [`ComboSpec::validate`]),
/// so a zero/garbage side returns `None` (never fill) rather than silently taking a branch. A NaN
/// net or limit also returns `None` — both comparisons are false, which is the safe verdict.
pub fn combo_net_cross(side: i32, limit: f64, legs: &[(i32, f64)]) -> Option<f64> {
    let net = combo_net(legs);
    let crosses = match side.signum() {
        1 => net <= limit,
        -1 => net >= limit,
        _ => false,
    };
    if crosses { Some(net) } else { None }
}

/// Build the ONE [`OrderRequest`] that carries a whole [`ComboSpec`] — the combo twin of
/// [`build_bracket`], and STATELESS the same way: the coid is MINTED BY THE RUNTIME and passed in,
/// so id minting stays in its one place.
///
/// `symbol` is left EMPTY: the venue adapter resolves the combo instrument name at submit
/// (Deribit `private/create_combo`) and substitutes it. `price` carries the SIGNED net limit
/// verbatim — never absolute-valued, never clamped.
///
/// FALLIBLE on purpose: [`ComboSpec::validate`] runs first, so a 0-leg spec can never lower to an
/// `OrderRequest` with an EMPTY `combo_legs` — which is the "this is NOT a combo" sentinel and
/// would otherwise hand the venue an ordinary limit order on an empty symbol at the (possibly
/// negative) net price.
pub fn build_combo(spec: &ComboSpec, coid: &str) -> Result<OrderRequest, ComboError> {
    spec.validate()?;
    Ok(OrderRequest {
        client_order_id: coid.to_string(),
        venue: spec.venue.clone(),
        // resolved by the venue adapter at submit (PR-4) — see the doc comment above.
        symbol: String::new(),
        side: spec.side,
        qty: spec.qty,
        order_type: if spec.net_limit.is_some() { OrderType::Limit } else { OrderType::Market },
        price: spec.net_limit,
        time_in_force: spec.time_in_force,
        combo_legs: spec.legs.clone(),
        ..Default::default()
    })
}

/// Map a frozen `OrderRequest` intent to a live mutable resting [`WorkingOrder`].
/// Dispatch priority (byte-identical to `order_request_to_working`):
/// 1. `trail.is_some()` → trailing; 2. on_close+market → market_close;
/// 3. on_close+limit → limit_close; 4. stop → stop (price = trigger_price);
/// 5. limit → limit (carries stop); 6. else market (carries stop).
pub fn order_request_to_working(req: &OrderRequest) -> WorkingOrder {
    let side = req.side;
    let qty = req.qty;
    if req.trail.is_some() {
        return WorkingOrder {
            kind: OrderKind::Trailing,
            side,
            size: qty,
            price: None,
            trail: req.trail,
            extreme: req.extreme,
            weight: req.weight,
            stop: None,
            qid: 0,
        };
    }
    if req.on_close && req.order_type == OrderType::Market {
        let mut o = WorkingOrder::new(OrderKind::MarketClose, side, qty);
        o.weight = req.weight;
        return o;
    }
    if req.on_close && req.order_type == OrderType::Limit {
        let mut o = WorkingOrder::new(OrderKind::LimitClose, side, qty);
        o.price = req.price;
        o.weight = req.weight;
        return o;
    }
    if req.order_type == OrderType::Stop {
        let mut o = WorkingOrder::new(OrderKind::Stop, side, qty);
        o.price = req.trigger_price;
        o.weight = req.weight;
        return o;
    }
    if req.order_type == OrderType::Limit {
        let mut o = WorkingOrder::new(OrderKind::Limit, side, qty);
        o.price = req.price;
        o.weight = req.weight;
        o.stop = req.stop;
        return o;
    }
    // market (default)
    let mut o = WorkingOrder::new(OrderKind::Market, side, qty);
    o.weight = req.weight;
    o.stop = req.stop;
    o
}

/// A bracket order: an entry plus a protective stop-loss and take-profit that arm when the entry
/// fills, and cancel each other when either exits. RUST-NATIVE (no Python twin).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BracketSpec {
    pub venue: String,
    pub symbol: String,
    /// entry side: +1 long / -1 short (the two exits are the opposite side, reduce-only)
    pub side: i32,
    pub qty: f64,
    /// entry order: `None` = market entry, `Some(px)` = limit entry
    pub entry_price: Option<f64>,
    /// stop-loss trigger price
    pub stop_loss: f64,
    /// take-profit limit price
    pub take_profit: f64,
}

/// Build a linked bracket as three [`OrderRequest`]s — `[entry, stop_loss, take_profit]` — filling
/// the reserved contingency slots: the entry is the OTO parent (fills → arms the exits); the two
/// exits are OCO siblings (either fills → cancels the other), reduce-only and opposite-side, sized
/// to the entry.
///
/// STATELESS by design — the vike answer to a stateful order-factory `bracket()`: the three
/// coids are MINTED BY THE RUNTIME and passed in, so id minting stays in its one place and this
/// function only wires the linkage. The entry coid doubles as the shared `order_list_id`.
pub fn build_bracket(
    spec: &BracketSpec,
    entry_coid: &str,
    sl_coid: &str,
    tp_coid: &str,
) -> [OrderRequest; 3] {
    let list_id = Some(entry_coid.to_string());
    let exit_side = -spec.side;
    let entry = OrderRequest {
        client_order_id: entry_coid.to_string(),
        venue: spec.venue.clone(),
        symbol: spec.symbol.clone(),
        side: spec.side,
        qty: spec.qty,
        order_type: if spec.entry_price.is_some() { OrderType::Limit } else { OrderType::Market },
        price: spec.entry_price,
        order_list_id: list_id.clone(),
        contingency_type: Some("OTO".to_string()),
        linked_order_ids: vec![sl_coid.to_string(), tp_coid.to_string()],
        ..Default::default()
    };
    let stop_loss = OrderRequest {
        client_order_id: sl_coid.to_string(),
        venue: spec.venue.clone(),
        symbol: spec.symbol.clone(),
        side: exit_side,
        qty: spec.qty,
        order_type: OrderType::Stop,
        trigger_price: Some(spec.stop_loss),
        reduce_only: true,
        parent_order_id: Some(entry_coid.to_string()),
        order_list_id: list_id.clone(),
        contingency_type: Some("OCO".to_string()),
        linked_order_ids: vec![tp_coid.to_string()],
        ..Default::default()
    };
    let take_profit = OrderRequest {
        client_order_id: tp_coid.to_string(),
        venue: spec.venue.clone(),
        symbol: spec.symbol.clone(),
        side: exit_side,
        qty: spec.qty,
        order_type: OrderType::Limit,
        price: Some(spec.take_profit),
        reduce_only: true,
        parent_order_id: Some(entry_coid.to_string()),
        order_list_id: list_id,
        contingency_type: Some("OCO".to_string()),
        linked_order_ids: vec![sl_coid.to_string()],
        ..Default::default()
    };
    [entry, stop_loss, take_profit]
}

#[path = "order_tests.rs"]
#[cfg(test)]
mod order_tests;
