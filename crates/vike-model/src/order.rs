//! The resting-order record + the frozen OrderRequest intent.
//! Exact ports of `core/orders.py::WorkingOrder` and `core/order_intent.py::OrderRequest`
//! (+ `order_request_to_working`, the 6-kind B2 dispatch).

use serde::{Deserialize, Serialize};

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

/// The price SERIES a trigger order's `trigger_price` is evaluated against (Nautilus
/// `TriggerType`, reduced to the three sources the reachable venues actually expose).
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
// backtest paper exchange (`vike_backtest::paper::Expiry`, native venue-style
// terminalization as `OrderExpired`) and the live core's managed GTD/DAY sweep
// (`vike_core`'s `sweep_gtd_expiry`, which cancels the venue-resting order) — read
// the SAME decision from one place, closing the divergence where the live sweep
// enforced Gtd only and skipped Day. The TERMINAL vocabulary stays per-site (paper
// owns its book so it mints `OrderExpired`; the live sweep must round-trip a venue
// cancel, so its terminal is the venue's authoritative `OrderCanceled`) — this law
// is only the boolean "expired?" predicate, never the terminalization.
// ─────────────────────────────────────────────────────────────────────────────

/// Milliseconds in one UTC calendar day — the [`TimeInForce::Day`] session boundary. A plain UTC
/// day is the "session" concept here: neither the backtest nor the live core models a per-venue
/// exchange-session calendar.
pub const MS_PER_DAY: i64 = 86_400_000;

/// UTC calendar-day index of an epoch-ms timestamp (floor division, so pre-1970 ts stay monotone).
pub fn utc_day(ts_ms: i64) -> i64 {
    ts_ms.div_euclid(MS_PER_DAY)
}

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

/// A submission intent — what a strategy / manual ticket / (later) bracket builds.
///
/// `side` is +1 buy / -1 sell. `order_type` in {market, limit, stop, take_profit} (the last two
/// are trigger orders — `stop`=stop-loss, `take_profit`=take-profit; both take a `trigger_price`
/// and are market/limit by whether `price` is set; venues that don't support triggers ignore
/// them); `price` is the limit price, `trigger_price` the trigger level. The contingency slots are
/// RESERVED for OCO/brackets.
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
    /// "market" | "limit" | "stop" | "take_profit"
    pub order_type: String,
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
    /// `order_type` stays `"limit"`/`"market"` as usual.
    ///
    /// Serde: `default` + `skip_serializing_if` empty, so every pre-existing wire body, fixture and
    /// journal record stays BYTE-IDENTICAL (`order_request_without_combo_legs_is_byte_identical`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub combo_legs: Vec<ComboLeg>,
    /// Requested margin/trade mode for THIS order (see [`crate::MarginMode`] and the per-venue
    /// [`crate::venue_margin_support`] capability map).
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
        order_type: if spec.net_limit.is_some() { "limit" } else { "market" }.to_string(),
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
    if req.on_close && req.order_type == "market" {
        let mut o = WorkingOrder::new(OrderKind::MarketClose, side, qty);
        o.weight = req.weight;
        return o;
    }
    if req.on_close && req.order_type == "limit" {
        let mut o = WorkingOrder::new(OrderKind::LimitClose, side, qty);
        o.price = req.price;
        o.weight = req.weight;
        return o;
    }
    if req.order_type == "stop" {
        let mut o = WorkingOrder::new(OrderKind::Stop, side, qty);
        o.price = req.trigger_price;
        o.weight = req.weight;
        return o;
    }
    if req.order_type == "limit" {
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
/// STATELESS by design — the vike answer to Nautilus's stateful `OrderFactory.bracket()`: the three
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
        order_type: if spec.entry_price.is_some() { "limit" } else { "market" }.to_string(),
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
        order_type: "stop".to_string(),
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
        order_type: "limit".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tif_defaults_gtc_and_serializes_screaming() {
        assert_eq!(TimeInForce::default(), TimeInForce::Gtc);
        assert_eq!(serde_json::to_string(&TimeInForce::Gtc).unwrap(), "\"GTC\"");
        assert_eq!(serde_json::to_string(&TimeInForce::Ioc).unwrap(), "\"IOC\"");
        assert_eq!(serde_json::to_string(&TimeInForce::Day).unwrap(), "\"DAY\"");
    }

    #[test]
    fn utc_day_floors_across_the_epoch() {
        assert_eq!(utc_day(0), 0);
        assert_eq!(utc_day(MS_PER_DAY - 1), 0);
        assert_eq!(utc_day(MS_PER_DAY), 1);
        assert_eq!(utc_day(-1), -1, "pre-epoch floors down, so days stay monotone");
    }

    #[test]
    fn gtc_ioc_fok_never_expire() {
        // deadline/anchor deliberately in the deep past — none of these three ever expires.
        for tif in [TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok] {
            assert!(
                !tif_expired(tif, Some(1), 0, 10 * MS_PER_DAY),
                "{tif:?} must never expire by this predicate"
            );
        }
    }

    #[test]
    fn gtd_expires_inclusively_at_the_deadline() {
        // before, at, after — inclusive boundary at `now == deadline`.
        assert!(!tif_expired(TimeInForce::Gtd, Some(2_000), 0, 1_999), "before the deadline");
        assert!(
            tif_expired(TimeInForce::Gtd, Some(2_000), 0, 2_000),
            "AT the deadline (inclusive)"
        );
        assert!(tif_expired(TimeInForce::Gtd, Some(2_000), 0, 2_001), "after the deadline");
    }

    #[test]
    fn gtd_without_a_deadline_never_expires() {
        assert!(!tif_expired(TimeInForce::Gtd, None, 0, i64::MAX / 2));
    }

    #[test]
    fn day_expires_when_now_crosses_the_utc_day_boundary_from_its_anchor() {
        let anchor = 12 * 3_600_000; // mid-day on UTC day 0
        assert!(!tif_expired(TimeInForce::Day, None, anchor, anchor), "the anchor moment is alive");
        assert!(
            !tif_expired(TimeInForce::Day, None, anchor, MS_PER_DAY - 1),
            "still alive through the end of the anchor's UTC day"
        );
        assert!(
            tif_expired(TimeInForce::Day, None, anchor, MS_PER_DAY),
            "expired on the first ms of the next UTC day"
        );
    }

    #[test]
    fn day_survives_its_whole_anchor_day_at_a_modern_clock() {
        // guards the seconds-vs-ms / born-expired class: a modern anchor lives out its own day.
        let today = 20_650 * MS_PER_DAY;
        assert!(!tif_expired(TimeInForce::Day, None, today, today), "born on its day, not expired");
        assert!(
            !tif_expired(TimeInForce::Day, None, today, today + MS_PER_DAY - 1),
            "alive to the last ms of the anchor day"
        );
        assert!(
            tif_expired(TimeInForce::Day, None, today, today + MS_PER_DAY),
            "expired the next UTC day"
        );
    }

    #[test]
    fn day_boundary_holds_across_a_leap_day() {
        // 2024-02-29 is UTC day 19782; 2024-03-01 is 19783 — a Day order anchored on the leap day
        // survives it and expires the next calendar day, exactly like any other pair of days.
        let leap = 19_782 * MS_PER_DAY;
        assert_eq!(utc_day(leap + 23 * 3_600_000), 19_782, "23:00 on the leap day is the same day");
        assert!(!tif_expired(TimeInForce::Day, None, leap, leap + 23 * 3_600_000));
        assert!(tif_expired(TimeInForce::Day, None, leap, leap + MS_PER_DAY), "the day after");
    }

    #[test]
    fn order_request_tif_is_additive() {
        // a serialized request WITHOUT time_in_force deserializes as GTC (back-compat)
        let r: OrderRequest = serde_json::from_str(
            r#"{"client_order_id":"c","venue":"v","symbol":"s","side":1,"qty":1.0,"order_type":"market"}"#,
        )
        .unwrap();
        assert_eq!(r.time_in_force, TimeInForce::Gtc);
        assert_eq!(r.gtd_expiry, None);
    }

    #[test]
    fn default_literal_matches_serde() {
        // Piece-1 refactor guard: the live runtime builds OrderRequest via `..Default::default()`
        // instead of a `serde_json` round-trip. This pins that the two produce byte-identical
        // values — i.e. every field's Rust `Default` equals its `#[serde(default)]`. If a future
        // field's two defaults diverge, this fails before the hot-path construction can drift.
        let via_literal = OrderRequest {
            client_order_id: "c".into(),
            venue: "v".into(),
            symbol: "s".into(),
            side: 1,
            qty: 2.0,
            order_type: "limit".into(),
            price: Some(3.0),
            reduce_only: true,
            ts: 42,
            ..Default::default()
        };
        let via_serde: OrderRequest = serde_json::from_value(serde_json::json!({
            "client_order_id": "c", "venue": "v", "symbol": "s",
            "side": 1, "qty": 2.0, "order_type": "limit", "price": 3.0,
            "reduce_only": true, "ts": 42,
        }))
        .unwrap();
        assert_eq!(via_literal, via_serde);
    }

    #[test]
    fn build_bracket_wires_oto_oco_linkage() {
        let spec = BracketSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1, // long entry
            qty: 3.0,
            entry_price: Some(100.0), // limit entry
            stop_loss: 95.0,
            take_profit: 110.0,
        };
        let [entry, sl, tp] = build_bracket(&spec, "E", "S", "T");

        // shared order-list id = the entry coid; the three orders all carry it
        assert_eq!(entry.order_list_id.as_deref(), Some("E"));
        assert_eq!(sl.order_list_id.as_deref(), Some("E"));
        assert_eq!(tp.order_list_id.as_deref(), Some("E"));

        // entry: OTO parent, limit, long, links to both exits
        assert_eq!(entry.contingency_type.as_deref(), Some("OTO"));
        assert_eq!(entry.order_type, "limit");
        assert_eq!(entry.side, 1);
        assert_eq!(entry.price, Some(100.0));
        assert_eq!(entry.parent_order_id, None);
        assert_eq!(entry.linked_order_ids, vec!["S".to_string(), "T".to_string()]);

        // stop-loss: OCO child of entry, opposite side, reduce-only, stop @ trigger, links to TP
        assert_eq!(sl.contingency_type.as_deref(), Some("OCO"));
        assert_eq!(sl.parent_order_id.as_deref(), Some("E"));
        assert_eq!(sl.side, -1);
        assert!(sl.reduce_only);
        assert_eq!(sl.order_type, "stop");
        assert_eq!(sl.trigger_price, Some(95.0));
        assert_eq!(sl.linked_order_ids, vec!["T".to_string()]);

        // take-profit: OCO child of entry, opposite side, reduce-only, limit @ tp, links to SL
        assert_eq!(tp.contingency_type.as_deref(), Some("OCO"));
        assert_eq!(tp.parent_order_id.as_deref(), Some("E"));
        assert_eq!(tp.side, -1);
        assert!(tp.reduce_only);
        assert_eq!(tp.order_type, "limit");
        assert_eq!(tp.price, Some(110.0));
        assert_eq!(tp.linked_order_ids, vec!["S".to_string()]);
    }

    // ---- combo orders (PR-1: vocabulary + invariants) ----

    fn call_spread() -> ComboSpec {
        ComboSpec {
            venue: "deribit".into(),
            side: 1,
            qty: 2.0,
            legs: vec![
                ComboLeg { symbol: "BTC-27MAR26-100000-C".into(), ratio: 1 },
                ComboLeg { symbol: "BTC-27MAR26-120000-C".into(), ratio: -1 },
            ],
            net_limit: Some(0.015),
            time_in_force: TimeInForce::Gtc,
        }
    }

    #[test]
    fn order_request_without_combo_legs_is_byte_identical() {
        // THE compatibility pin: an ordinary (non-combo) request must serialize EXACTLY as it did
        // before `combo_legs` existed — `skip_serializing_if = "Vec::is_empty"` means the key is
        // absent from the JSON entirely, so every existing fixture / journal record / wire body
        // round-trips unchanged. If this string ever needs editing, a persisted schema broke.
        let req = OrderRequest {
            client_order_id: "c".into(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 2.0,
            order_type: "limit".into(),
            price: Some(3.0),
            ts: 42,
            ..Default::default()
        };
        let js = serde_json::to_string(&req).unwrap();
        assert_eq!(
            js,
            r#"{"client_order_id":"c","venue":"sim","symbol":"BTCUSDT","side":1,"qty":2.0,"order_type":"limit","price":3.0,"trigger_price":null,"reduce_only":false,"time_in_force":"GTC","gtd_expiry":null,"ts":42,"parent_order_id":null,"linked_order_ids":[],"order_list_id":null,"contingency_type":null,"weight":0.0,"stop":null,"trail":null,"extreme":null,"on_close":false}"#
        );
        assert!(!js.contains("combo_legs"));
        // and an OLD payload (no combo_legs key at all) still deserializes
        let back: OrderRequest = serde_json::from_str(&js).unwrap();
        assert_eq!(back, req);
        assert!(back.combo_legs.is_empty());
    }

    #[test]
    fn order_request_without_margin_mode_is_byte_identical() {
        // The margin-mode compatibility pin (twin of the combo_legs pin above): an ordinary
        // request with no requested mode serializes EXACTLY as before the field existed — the
        // `margin_mode` key is absent entirely, so every fixture / journal record / wire body
        // (and the journal `state_hash` fence) round-trips unchanged.
        let req = OrderRequest {
            client_order_id: "c".into(),
            venue: "okx".into(),
            symbol: "BTC-USDT-SWAP".into(),
            side: 1,
            qty: 2.0,
            order_type: "limit".into(),
            price: Some(3.0),
            ts: 42,
            ..Default::default()
        };
        let js = serde_json::to_string(&req).unwrap();
        assert!(!js.contains("margin_mode"), "{js}");
        // an OLD payload (no margin_mode key at all) still deserializes as None
        let back: OrderRequest = serde_json::from_str(&js).unwrap();
        assert_eq!(back, req);
        assert_eq!(back.margin_mode, None);
        // and a requested mode round-trips
        let mut iso = req;
        iso.margin_mode = Some(crate::MarginMode::Isolated);
        let ijs = serde_json::to_string(&iso).unwrap();
        assert!(ijs.contains(r#""margin_mode":"Isolated""#), "{ijs}");
        assert_eq!(serde_json::from_str::<OrderRequest>(&ijs).unwrap(), iso);
    }

    #[test]
    fn order_request_without_trigger_by_is_byte_identical() {
        // The trigger-source compatibility pin (twin of the margin_mode pin above): a request
        // with no requested source serializes EXACTLY as before the field existed — the
        // `trigger_by` key is absent entirely, so every fixture / journal record / wire body
        // (and the journal `state_hash` fence) round-trips unchanged.
        let req = OrderRequest {
            client_order_id: "c".into(),
            venue: "bybit".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 2.0,
            order_type: "stop".into(),
            trigger_price: Some(95.0),
            ts: 42,
            ..Default::default()
        };
        let js = serde_json::to_string(&req).unwrap();
        assert!(!js.contains("trigger_by"), "{js}");
        // an OLD payload (no trigger_by key at all) still deserializes as None
        let back: OrderRequest = serde_json::from_str(&js).unwrap();
        assert_eq!(back, req);
        assert_eq!(back.trigger_by, None);
        // and a requested source round-trips
        let mut mark = req;
        mark.trigger_by = Some(TriggerBy::Mark);
        let mjs = serde_json::to_string(&mark).unwrap();
        assert!(mjs.contains(r#""trigger_by":"Mark""#), "{mjs}");
        assert_eq!(serde_json::from_str::<OrderRequest>(&mjs).unwrap(), mark);
    }

    #[test]
    fn combo_request_serde_roundtrips() {
        let req = build_combo(&call_spread(), "K").unwrap();
        let js = serde_json::to_string(&req).unwrap();
        assert!(js.contains("combo_legs"), "{js}");
        let back: OrderRequest = serde_json::from_str(&js).unwrap();
        assert_eq!(back, req);

        let spec = call_spread();
        let sjs = serde_json::to_string(&spec).unwrap();
        assert_eq!(serde_json::from_str::<ComboSpec>(&sjs).unwrap(), spec);
    }

    #[test]
    fn build_combo_carries_signed_net_limit_and_legs() {
        let req = build_combo(&call_spread(), "K").unwrap();
        assert_eq!(req.client_order_id, "K");
        assert_eq!(req.venue, "deribit");
        assert_eq!(req.symbol, "", "combo instrument is resolved by the adapter at submit");
        assert_eq!(req.side, 1);
        assert_eq!(req.qty, 2.0);
        assert_eq!(req.order_type, "limit");
        assert_eq!(req.price, Some(0.015));
        assert_eq!(req.combo_legs.len(), 2);
        assert_eq!(req.combo_legs[1].ratio, -1);

        // market combo: no net limit
        let mut spec = call_spread();
        spec.net_limit = None;
        let m = build_combo(&spec, "K2").unwrap();
        assert_eq!(m.order_type, "market");
        assert_eq!(m.price, None);

        // CREDIT combo: the negative net limit passes through verbatim — no clamp, no abs()
        let mut credit = call_spread();
        credit.side = -1;
        credit.net_limit = Some(-0.0125);
        let c = build_combo(&credit, "K3").unwrap();
        assert_eq!(c.price, Some(-0.0125));
        assert_eq!(c.side, -1);
    }

    #[test]
    fn combo_net_is_signed_sum_of_ratio_times_price() {
        // debit call spread: +1 @ 50, -1 @ 30  =>  net +20 (a DEBIT, positive)
        assert_eq!(combo_net(&[(1, 50.0), (-1, 30.0)]), 20.0);
        // credit put spread: -1 @ 50, +1 @ 30  =>  net -20 (a CREDIT, negative)
        assert_eq!(combo_net(&[(-1, 50.0), (1, 30.0)]), -20.0);
        // ratios scale
        assert_eq!(combo_net(&[(2, 10.0), (-1, 5.0)]), 15.0);
        // a balanced spread nets exactly zero — legal
        assert_eq!(combo_net(&[(1, 5.0), (-1, 5.0)]), 0.0);
        assert_eq!(combo_net(&[]), 0.0);
    }

    #[test]
    fn combo_cross_buy_fills_at_or_below_limit() {
        let legs = [(1, 50.0), (-1, 30.0)]; // net = +20
        assert_eq!(combo_net_cross(1, 20.0, &legs), Some(20.0)); // exactly at the limit
        assert_eq!(combo_net_cross(1, 25.0, &legs), Some(20.0)); // better than the limit
        assert_eq!(combo_net_cross(1, 19.0, &legs), None); // too expensive
    }

    #[test]
    fn combo_cross_sell_fills_at_or_above_limit() {
        let legs = [(1, 50.0), (-1, 30.0)]; // net = +20
        assert_eq!(combo_net_cross(-1, 20.0, &legs), Some(20.0)); // exactly at the limit
        assert_eq!(combo_net_cross(-1, 15.0, &legs), Some(20.0)); // better than the limit
        assert_eq!(combo_net_cross(-1, 25.0, &legs), None); // not enough credit
    }

    #[test]
    fn combo_cross_handles_credit_negative_nets() {
        // A CREDIT structure: net is NEGATIVE on both sides of the law — nothing clamps to >= 0.
        let legs = [(-1, 50.0), (1, 30.0)]; // net = -20
        assert!(combo_net(&legs) < 0.0);
        // buying a credit combo at a -10 limit: -20 <= -10 => fills
        assert_eq!(combo_net_cross(1, -10.0, &legs), Some(-20.0));
        // ...but not at -30 (we'd need net <= -30)
        assert_eq!(combo_net_cross(1, -30.0, &legs), None);
        // selling it: net >= limit
        assert_eq!(combo_net_cross(-1, -30.0, &legs), Some(-20.0));
        assert_eq!(combo_net_cross(-1, -10.0, &legs), None);
        // a zero net crosses a zero limit from BOTH sides
        assert_eq!(combo_net_cross(1, 0.0, &[(1, 5.0), (-1, 5.0)]), Some(0.0));
        assert_eq!(combo_net_cross(-1, 0.0, &[(1, 5.0), (-1, 5.0)]), Some(0.0));
    }

    #[test]
    fn combo_cross_never_fills_on_a_zero_side_or_nan() {
        // REGRESSION: `side >= 0` treated 0 (the i32 default, e.g. a garbage/partial spec) as a
        // BUY and could report a crossing. ±1 is the workspace-wide law; anything else = no fill.
        let legs = [(1, 50.0), (-1, 30.0)]; // net = +20, crosses a buy limit of 25
        assert_eq!(combo_net_cross(1, 25.0, &legs), Some(20.0), "sanity: +1 does cross");
        assert_eq!(combo_net_cross(0, 25.0, &legs), None, "side 0 is NOT a buy");
        assert_eq!(combo_net_cross(0, -25.0, &legs), None);
        // NaN anywhere => both comparisons false => no fill (the safe verdict)
        assert_eq!(combo_net_cross(1, f64::NAN, &legs), None);
        assert_eq!(combo_net_cross(-1, f64::NAN, &legs), None);
        assert_eq!(combo_net_cross(1, 25.0, &[(1, f64::NAN), (-1, 30.0)]), None);
    }

    #[test]
    fn combo_net_fold_is_left_to_right_in_leg_order() {
        // The doc says the naive left-to-right fold IN LEG ORDER is part of the contract. Pin it
        // with values whose sum is association-sensitive: (a+b)+c != a+(b+c) in f64.
        let a = 1e16_f64;
        let legs = [(1, a), (1, 1.0), (-1, a)];
        // left-to-right: ((0 + 1e16) + 1) - 1e16 == 0.0 (the 1 is lost to rounding)
        assert_eq!(combo_net(&legs), 0.0);
        // a reordered fold would give 1.0 — proving the order is observable, not cosmetic
        let reordered = [(1, a), (-1, a), (1, 1.0)];
        assert_eq!(combo_net(&reordered), 1.0);
        assert_ne!(
            combo_net(&legs).to_bits(),
            combo_net(&reordered).to_bits(),
            "leg order must NOT be normalized away"
        );
    }

    #[test]
    fn combo_net_from_legs_picks_ask_for_bought_legs_and_bid_for_sold() {
        // Bridges ComboSpec.legs -> combo_net, deciding the book side ONCE.
        let legs = call_spread().legs; // +1 100k call, -1 120k call
        let quote = |_sym: &str, want_ask: bool| if want_ask { 0.030 } else { 0.010 };
        // BUYING the spread: pay the ASK on the +1 leg, receive the BID on the -1 leg.
        assert_eq!(combo_net_from_legs(1, &legs, quote), 0.030 - 0.010);
        // SELLING it flips both sides: receive the BID on the +1 leg, pay the ASK on the -1.
        assert_eq!(combo_net_from_legs(-1, &legs, quote), 0.010 - 0.030);
        // and it agrees with combo_net fed the same per-leg prices, in the same leg order
        assert_eq!(combo_net_from_legs(1, &legs, quote), combo_net(&[(1, 0.030), (-1, 0.010)]));
    }

    #[test]
    fn combo_spec_rejects_degenerate_specs() {
        // REGRESSION (the leg-count invariant): a 0-leg spec used to lower to an ordinary-looking
        // NON-combo OrderRequest (empty `combo_legs` IS the "not a combo" sentinel) on an EMPTY
        // symbol at a NEGATIVE price. Both build_combo and Deserialize must refuse it.
        let mut zero = call_spread();
        zero.legs.clear();
        zero.net_limit = Some(-5.0);
        assert_eq!(zero.validate(), Err(ComboError::TooFewLegs(0)));
        assert_eq!(build_combo(&zero, "K"), Err(ComboError::TooFewLegs(0)));

        let mut one = call_spread();
        one.legs.truncate(1);
        assert_eq!(build_combo(&one, "K"), Err(ComboError::TooFewLegs(1)));

        let mut zero_ratio = call_spread();
        zero_ratio.legs[1].ratio = 0;
        assert_eq!(build_combo(&zero_ratio, "K"), Err(ComboError::ZeroRatio(1)));

        let mut empty_sym = call_spread();
        empty_sym.legs[0].symbol.clear();
        assert_eq!(build_combo(&empty_sym, "K"), Err(ComboError::EmptyLegSymbol(0)));

        for bad in [0, 2, -3] {
            let mut s = call_spread();
            s.side = bad;
            assert_eq!(build_combo(&s, "K"), Err(ComboError::InvalidSide(bad)));
        }

        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let mut s = call_spread();
            s.qty = bad;
            assert!(matches!(build_combo(&s, "K"), Err(ComboError::InvalidQty(_))));
        }

        let mut nan_limit = call_spread();
        nan_limit.net_limit = Some(f64::NAN);
        assert!(matches!(build_combo(&nan_limit, "K"), Err(ComboError::NonFiniteNetLimit(_))));

        // ...while the legal shapes still build, including a CREDIT and a MARKET combo
        assert!(call_spread().validate().is_ok());
        let mut credit = call_spread();
        credit.side = -1;
        credit.net_limit = Some(-0.0125);
        assert_eq!(build_combo(&credit, "K").unwrap().price, Some(-0.0125));
        let mut mkt = call_spread();
        mkt.net_limit = None;
        assert!(build_combo(&mkt, "K").is_ok());
    }

    #[test]
    fn combo_spec_deserialize_enforces_the_invariants() {
        // A REPLAYED journal record / wire body cannot smuggle an invalid spec past the gate.
        let js = r#"{"venue":"deribit","side":1,"qty":2.0,"legs":[],"net_limit":-5.0}"#;
        let err = serde_json::from_str::<ComboSpec>(js).unwrap_err().to_string();
        assert!(err.contains("2..=N legs"), "{err}");

        let one = r#"{"venue":"deribit","side":1,"qty":2.0,
            "legs":[{"symbol":"BTC-PERPETUAL","ratio":1}],"net_limit":1.0}"#;
        assert!(serde_json::from_str::<ComboSpec>(one).is_err());

        let side0 = r#"{"venue":"deribit","side":0,"qty":2.0,
            "legs":[{"symbol":"A","ratio":1},{"symbol":"B","ratio":-1}],"net_limit":1.0}"#;
        assert!(serde_json::from_str::<ComboSpec>(side0).is_err());

        // ...and a VALID one still decodes, with `time_in_force` ABSENT defaulting to Gtc
        // (the additive-field claim for ComboSpec itself).
        let ok = r#"{"venue":"deribit","side":-1,"qty":2.0,
            "legs":[{"symbol":"A","ratio":1},{"symbol":"B","ratio":-1}],"net_limit":-5.0}"#;
        let spec: ComboSpec = serde_json::from_str(ok).unwrap();
        assert_eq!(spec.time_in_force, TimeInForce::Gtc);
        assert_eq!(spec.net_limit, Some(-5.0), "credit net survives deserialize unclamped");
        assert_eq!(spec.legs.len(), 2);
    }

    #[test]
    fn bracket_leg_shaped_request_is_byte_identical_too() {
        // Second compatibility pin: the byte-identity claim must hold for the CONTINGENCY-carrying
        // payloads that actually dominate the journal, not just a plain limit order.
        let req = OrderRequest {
            client_order_id: "S".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 3.0,
            order_type: "stop".into(),
            trigger_price: Some(95.0),
            reduce_only: true,
            ts: 7,
            parent_order_id: Some("E".into()),
            linked_order_ids: vec!["T".into()],
            order_list_id: Some("E".into()),
            contingency_type: Some("OCO".into()),
            on_close: true,
            ..Default::default()
        };
        let js = serde_json::to_string(&req).unwrap();
        assert_eq!(
            js,
            r#"{"client_order_id":"S","venue":"binance","symbol":"BTCUSDT","side":-1,"qty":3.0,"order_type":"stop","price":null,"trigger_price":95.0,"reduce_only":true,"time_in_force":"GTC","gtd_expiry":null,"ts":7,"parent_order_id":"E","linked_order_ids":["T"],"order_list_id":"E","contingency_type":"OCO","weight":0.0,"stop":null,"trail":null,"extreme":null,"on_close":true}"#
        );
        assert!(!js.contains("combo_legs"));
        assert_eq!(serde_json::from_str::<OrderRequest>(&js).unwrap(), req);
    }

    #[test]
    fn build_bracket_market_entry_short() {
        let spec = BracketSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: -1, // short entry
            qty: 1.0,
            entry_price: None, // market entry
            stop_loss: 105.0,
            take_profit: 90.0,
        };
        let [entry, sl, tp] = build_bracket(&spec, "E", "S", "T");
        assert_eq!(entry.order_type, "market");
        assert_eq!(entry.price, None);
        // exits are the opposite (long) side, sized to the entry
        assert_eq!(sl.side, 1);
        assert_eq!(tp.side, 1);
        assert_eq!(sl.qty, 1.0);
        assert_eq!(tp.qty, 1.0);
    }
}
