//! The resting-order record + the frozen OrderRequest intent.
//! Exact ports of `core/orders.py::OrderResting` and `core/order_intent.py::OrderRequest`
//! (+ `OrderResting::from_request`, the 6-kind B2 dispatch).

use serde::{Deserialize, Serialize};

use crate::time::utc_day;

/// Resting-order kind. Serialized as the Python strings; the kinds appended from `stop_limit` on
/// have no Python twin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderKind {
    Market,
    MarketClose,
    LimitClose,
    Limit,
    Stop,
    Trailing,
    /// A stop at `price` (the trigger) whose fill is a limit at [`OrderResting::limit`]. Once
    /// triggered, a limit this event does not reach turns the order, in place, into a plain
    /// [`OrderKind::Limit`] at that limit (`crate::order_fill_price`'s law).
    StopLimit,
    /// An if-touched order at `price` (the trigger): touched from the favourable side (a buy at or
    /// below, a sell at or above). With no [`OrderResting::limit`] it fills at the activation
    /// price (market-if-touched); with one, it takes the limit part of [`OrderKind::StopLimit`]
    /// (limit-if-touched).
    TakeProfit,
    /// A market order whose unfilled remainder rests, in place, as the plain [`OrderKind::Limit`]
    /// at the price of its first fill. An event that fills it whole fills it as
    /// [`OrderKind::Market`] (`crate::order_fill_price`'s law).
    MarketToLimit,
    /// An opening-auction order ([`TimeInForce::AtOpen`]): it takes part ONCE, at the open of the bar
    /// the caller's session calendar names (the first bar of the next session), at `price` or better
    /// when it carries one. An auction that does not fill it is over: the order ends, it never rests.
    AuctionOpen,
    /// The closing-auction twin of [`OrderKind::AuctionOpen`] ([`TimeInForce::AtClose`]): the close of
    /// the session's last bar. Not [`OrderKind::MarketClose`], which is the close of EVERY bar.
    AuctionClose,
    /// A [`OrderKind::Trailing`] stop whose fill is a limit: it fires by the trailing law, and at the
    /// stop level P it fired at its limit becomes P − [`OrderResting::limit_offset`] for a sell, P +
    /// it for a buy ([`OrderResting::trailing_limit_at`]); from there it is the limit part of
    /// [`OrderKind::StopLimit`], the fire price its activation (`crate::order_fill_price`'s law).
    TrailingLimit,
    /// The if-touched twin of [`OrderKind::Trailing`] (IB `TRAIL MIT`, and `TRAIL LIT` with a
    /// [`OrderResting::limit_offset`]): a sell tracks the LOWEST price since it rests and fires when the
    /// price rises to that low plus its trail, a buy mirrors it. With an offset its limit is set off the
    /// trigger it fired at ([`OrderResting::trailing_limit_at`]), then the limit part of
    /// [`OrderKind::StopLimit`] (`crate::order_fill_price`'s law).
    TrailingTakeProfit,
    /// A market-to-limit at the opening auction (IB `MTL` with TIF `AUC`): filled as a market order at
    /// the open of the bar the caller's session calendar names, exactly as a market
    /// [`OrderKind::AuctionOpen`]; what the lane leaves rests in place as the plain [`OrderKind::Limit`]
    /// at the price the auction booked ([`OrderResting::rest_as_limit`]).
    AuctionOpenToLimit,
}

/// A pending order (mutable resting record — engines ratchet `extreme` / cap `size` in place).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderResting {
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
    /// The order may only REDUCE the position it meets at its fill: the simulator caps it to that
    /// position and drops it (in words) when the position is flat or on its own side (it is set by
    /// `OrderBrokerSimulator::submit_stop_reduce`). `OrderResting::from_request` leaves it `false`; the paper
    /// book sets it from its request and reads it ONLY to tell an order-list entry from an exit
    /// inside one bar (`vike_fills::fill_resolution::resolve_intrabar_fills_path_first`), never to
    /// cap or refuse. Serde: `default` + skipped when `false`, so every pre-existing fixture and
    /// persisted body is byte-identical.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reduce_only: bool,
    /// The LIMIT price of a [`OrderKind::StopLimit`] or of a limit-if-touched
    /// [`OrderKind::TakeProfit`]; `price` stays the trigger. `None` on every other kind, and taken
    /// back to `None` when a triggered order turns into a plain `Limit` at it
    /// ([`OrderResting::rest_as_limit`]). Serde: `default` + skipped when `None`, so every
    /// pre-existing fixture and persisted body is byte-identical
    /// (`working_order_without_limit_is_byte_identical`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    /// WHEN the order comes alive ([`OrderRelease`]); `None` is at once, which every constructor
    /// produces. `OrderResting::from_request` copies [`OrderRequest::release`]. A held order takes
    /// part in no event before its release holds ([`OrderResting::is_held`]): that deferral is each
    /// fill lane's, never `crate::order_fill_price`'s. Serde: `default` + skipped when `None`, so
    /// every pre-existing fixture and persisted body is byte-identical
    /// (`working_order_without_release_is_byte_identical`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<OrderRelease>,
    /// The ACTIVATION level of a trailing kind ([`OrderKind::Trailing`], [`OrderKind::TrailingLimit`]):
    /// nothing trails until the price reaches it (a sell's from below, a buy's from above), and
    /// `extreme` is `None` until then. `None` trails at once. `OrderResting::from_request` copies a
    /// trailing request's [`OrderRequest::trigger_price`]; `None` on every other kind.
    /// `crate::order_fill_price` never reads it: a lane that holds an unactivated order decides the
    /// activation before the law (the simulator's path walk). Serde: `default` + skipped when
    /// `None`, so every pre-existing fixture and persisted body is byte-identical
    /// (`working_order_without_activation_or_offset_is_byte_identical`). The live core's conditional
    /// book journals it, and `limit_offset`, in `vike_execution::ConditionalKindTerms` (an arm the legacy
    /// stop/trailing shape cannot state, which an older build skips instead of arming a plain stop).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation: Option<f64>,
    /// How far a [`OrderKind::TrailingLimit`]'s limit sits from the stop level it fires at, zero or
    /// more, in price ([`OrderResting::trailing_limit_at`]); `None` on every other kind. Serde: as
    /// [`OrderResting::activation`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_offset: Option<f64>,
    /// An ALL-OR-NONE limit ([`OrderAttributes::all_or_none`]): it fills only on an event that can fill
    /// all of it, a lane's own rule (a volume cap holds it whole). Serde: `default` + skipped when
    /// `false`, so every pre-existing fixture and persisted body is byte-identical
    /// (`working_order_without_the_attribute_fields_is_byte_identical`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all_or_none: bool,
    /// A DISCRETIONARY limit's amount ([`OrderAttributes::discretionary_amount`]): it executes up to
    /// `price` + this for a buy, `price` − this for a sell ([`OrderResting::discretion_limit`]), and books
    /// as a taker ([`OrderResting::books_as_maker`]). Serde: as [`OrderResting::activation`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discretionary_amount: Option<f64>,
    /// A PERCENT trail, above 0 and below 100, in place of `trail` ([`OrderResting::trail_distance`], the
    /// one place it becomes a price); `None` on every order whose trail is a price. Serde: as
    /// [`OrderResting::activation`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail_percent: Option<f64>,
    /// A stop's ADJUSTMENT ([`OrderStopAdjust`]): once the price reaches its level, the order becomes its
    /// target in place, once (`crate::order_fill_price`'s law). Boxed: it is rare and 48 bytes, and
    /// every resting order of every book carries the slot (`a_working_order_is_224_bytes`). Serde: as
    /// [`OrderResting::activation`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjust: Option<Box<OrderStopAdjust>>,
    /// ENGINE-LOCAL identity handle — `0` means "not yet stamped", which is what every
    /// constructor produces and what every path that does not opt into an identity-keyed
    /// side-table leaves it at. Stamped (monotonically, per replay) by the vike-backtest
    /// queue-position model so a CANCELED order's queue state can never be adopted by a
    /// later order that merely happens to rest at the same (side, price); a re-submit builds
    /// a fresh `OrderResting`, hence a fresh identity. `#[serde(skip)]`: never on the wire,
    /// so the persisted/fixture schema is unchanged and a round-trip resets it to `0`.
    #[serde(skip)]
    pub qid: u64,
}

impl OrderResting {
    pub fn new(kind: OrderKind, side: i32, size: f64) -> Self {
        OrderResting {
            kind,
            side,
            size,
            price: None,
            trail: None,
            extreme: None,
            weight: 0.0,
            stop: None,
            reduce_only: false,
            limit: None,
            release: None,
            activation: None,
            limit_offset: None,
            all_or_none: false,
            discretionary_amount: None,
            trail_percent: None,
            adjust: None,
            qid: 0,
        }
    }

    /// The limit a [`OrderKind::TrailingLimit`] that fired at the stop level `stop` rests at: `stop`
    /// less its [`OrderResting::limit_offset`] for a sell, plus it for a buy (IB `TRAIL LIMIT`'s
    /// `lmtPriceOffset`); a limit-offset [`OrderKind::TrailingTakeProfit`]'s, at its trigger, the same
    /// (IB `TRAIL LIT`). The ONE place the offset is applied, so every fill lane sets the same
    /// [`OrderResting::limit`] at the fire.
    #[must_use]
    pub fn trailing_limit_at(&self, stop: f64) -> f64 {
        let offset = self.limit_offset.expect("trailing_limit requires limit_offset");
        if self.side > 0 { stop + offset } else { stop - offset }
    }

    /// The trailing DISTANCE, in price, at the extreme `extreme`: `trail` itself, or `extreme` ×
    /// [`OrderResting::trail_percent`] / 100. The ONE place a percent becomes a price, so every fill
    /// lane trails a percent order alike; a price trail's distance is its `trail`, bit for bit.
    #[inline]
    #[must_use]
    pub fn trail_distance(&self, extreme: f64) -> f64 {
        match self.trail_percent {
            Some(p) => extreme * p / 100.0,
            None => self.trail.expect("trailing requires trail"),
        }
    }

    /// The worst price a `Limit` at `price` may fill at: `price` itself, or `price` moved by its
    /// [`OrderResting::discretionary_amount`] toward the market (L + d for a buy, L − d for a sell). The
    /// ONE place the discretion is applied, so every fill lane prices it alike.
    #[inline]
    #[must_use]
    pub fn discretion_limit(&self, price: f64) -> f64 {
        match self.discretionary_amount {
            None => price,
            Some(d) if self.side > 0 => price + d,
            Some(d) => price - d,
        }
    }

    /// Whether the order is still HELD at `ts`: it has a [`OrderRelease`] that does not hold yet, its price
    /// condition (if any) read through `price_of` ([`OrderRelease::holds`]). The one test every fill lane
    /// applies before an event may touch the order.
    #[inline]
    #[must_use]
    pub fn is_held(&self, ts: i64, price_of: impl FnOnce(&str) -> Option<f64>) -> bool {
        self.release.is_some_and(|r| !r.holds(ts, price_of))
    }

    /// A triggered stop-limit, limit-if-touched or trailing-limit whose limit was not reached becomes,
    /// in place, the plain resting `Limit` at that limit (what a venue releases). The ONE conversion
    /// every fill lane applies, so the order's later fills read the `Limit` law and book as a maker. A
    /// market-to-limit's remainder takes it too, once the lane has set `limit` to the price of the
    /// first fill.
    pub fn rest_as_limit(&mut self) {
        self.kind = OrderKind::Limit;
        self.price = self.limit.take();
    }

    /// Whether this order's fill books as a MAKER: by ORDER KIND, the one rule every working-order
    /// fill site reads, so a marketable limit books as a maker and a `LimitClose` as a taker. The
    /// caveat is `crate::money::fees`' module doc. A DISCRETIONARY limit is a taker: its executions
    /// inside the discretion take liquidity.
    #[inline]
    #[must_use]
    pub fn books_as_maker(&self) -> bool {
        self.kind == OrderKind::Limit && self.discretionary_amount.is_none()
    }

    /// Whether a limit bounds this order's fill price: a `Limit`, a `StopLimit`, a `TrailingLimit`, a
    /// `TakeProfit` that carries a limit (limit-if-touched), a `TrailingTakeProfit` that carries a limit
    /// offset (IB `TRAIL LIT`), and an auction order that carries one (`price`, its limit:
    /// limit-on-open, limit-on-close). Such a fill never prices worse than its limit, so it pays no
    /// fixed slippage. A `LimitClose` is not one: it fills at the close and books as a taker.
    #[inline]
    #[must_use]
    pub fn is_limit_bounded(&self) -> bool {
        match self.kind {
            OrderKind::Limit | OrderKind::StopLimit | OrderKind::TrailingLimit => true,
            OrderKind::TakeProfit => self.limit.is_some(),
            OrderKind::TrailingTakeProfit => self.limit_offset.is_some(),
            OrderKind::AuctionOpen | OrderKind::AuctionClose => self.price.is_some(),
            _ => false,
        }
    }
}

/// OrderResting time-in-force. Maps to FIX `TimeInForce(59)`; each venue maps it to its own TIF at the
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
    /// post-only / maker-only (FIX ExecInst 6, "participate don't initiate"): the order RESTS until
    /// canceled, and if it would take liquidity when it arrives at the venue it must NOT execute —
    /// the venue (or the paper book, or the simulator) refuses it with
    /// [`RejectCause::PostOnlyWouldCross`](crate::events::RejectCause::PostOnlyWouldCross). It is a
    /// variant of this type, not a flag beside it, because every venue spells it as the order's
    /// lifetime or kind (Binance `GTX`, Bybit `PostOnly`, Hyperliquid `Alo`, OKX `ordType`) and
    /// because the venues that have it take no expiry with it, so "post-only AND good-till-date" is
    /// unrepresentable instead of refused venue by venue. Meaningful on a LIMIT order only. Appended
    /// LAST: `ALL` and the declaration order of the five older variants are unchanged.
    PostOnly,
    /// at the opening auction (FIX 2, "at the opening"; IB `OPG`): a market or limit order that
    /// takes part ONCE, in the auction that opens the next session, and is over after it, filled or
    /// not. [`time_in_force_expired`] never expires it: the auction is its deadline, and only a book that knows
    /// the session can enforce that (the simulator, through its session calendar; the paper book has
    /// none and refuses it). Appended after `PostOnly`, with `AtClose`: `ALL` and the older variants'
    /// order are unchanged.
    AtOpen,
    /// at the closing auction (FIX 7, "at the close"; IB `MOC`/`LOC`): [`TimeInForce::AtOpen`]'s twin
    /// at the auction that closes the session.
    AtClose,
}

impl TimeInForce {
    /// Every variant, in declaration order. A HAND list, not complete by construction: its
    /// completeness is held by the test `time_in_force_all_is_every_variant_in_declaration_order`
    /// beside this module, whose exhaustive `match` stops compiling when a variant is added.
    pub const ALL: &'static [TimeInForce] = &[
        TimeInForce::Gtc,
        TimeInForce::Ioc,
        TimeInForce::Fok,
        TimeInForce::Gtd,
        TimeInForce::Day,
        TimeInForce::PostOnly,
        TimeInForce::AtOpen,
        TimeInForce::AtClose,
    ];

    /// An AUCTION lifetime ([`TimeInForce::AtOpen`], [`TimeInForce::AtClose`]): the one lifetime that
    /// means something on a MARKET order too (market-on-open), so [`lifetime_refusal`] and the
    /// capability preflight judge it on a market order as well as on a limit order.
    #[inline]
    #[must_use]
    pub const fn is_auction(self) -> bool {
        matches!(self, TimeInForce::AtOpen | TimeInForce::AtClose)
    }

    /// The operator's word for this lifetime: lowercase, hyphenated (`post-only`). The words
    /// [`TimeInForce::parse`] reads and the node's `submit` carries on its wire.
    #[must_use]
    pub const fn as_word(self) -> &'static str {
        match self {
            TimeInForce::Gtc => "gtc",
            TimeInForce::Ioc => "ioc",
            TimeInForce::Fok => "fok",
            TimeInForce::Gtd => "gtd",
            TimeInForce::Day => "day",
            TimeInForce::PostOnly => "post-only",
            TimeInForce::AtOpen => "at-open",
            TimeInForce::AtClose => "at-close",
        }
    }

    /// The INPUT door for a lifetime typed by an operator (the CLI) or sent by a caller (the node's
    /// `submit`): trimmed, ASCII case-insensitive, an underscore accepted for the hyphen (`post_only`
    /// beside `post-only`, `at_open` beside `at-open`). Anything
    /// else is REFUSED, naming the accepted words — never lowered to the default `Gtc`, which would
    /// rest an order the operator meant to expire.
    pub fn parse(input: &str) -> Result<TimeInForce, TimeInForceError> {
        let word = input.trim();
        TimeInForce::ALL
            .iter()
            .copied()
            .find(|t| {
                let w = t.as_word();
                w.eq_ignore_ascii_case(word) || w.replace('-', "_").eq_ignore_ascii_case(word)
            })
            .ok_or_else(|| TimeInForceError { input: input.to_owned() })
    }

    /// The accepted words of [`TimeInForce::parse`], comma-separated, for refusal messages.
    #[must_use]
    pub fn accepted_words() -> String {
        TimeInForce::ALL.iter().map(|t| t.as_word()).collect::<Vec<_>>().join(", ")
    }
}

/// [`TimeInForce::parse`] refused its input: not one of the lifetime words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeInForceError {
    /// the refused input, verbatim
    pub input: String,
}

impl core::fmt::Display for TimeInForceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write_unknown_word(f, "time in force", &self.input, &TimeInForce::accepted_words())
    }
}

/// The one sentence a refused order WORD gets ([`TimeInForceError`], [`OrderTypeError`]): the
/// input verbatim, and every word that is accepted.
fn write_unknown_word(
    f: &mut core::fmt::Formatter<'_>,
    what: &str,
    input: &str,
    accepted: &str,
) -> core::fmt::Result {
    write!(f, "unknown {what} {input:?}: expected one of {accepted}")
}

impl std::error::Error for TimeInForceError {}

/// What KIND of order an [`OrderRequest`] is — the platform's own vocabulary, lowercase on the
/// wire (`"market"`, `"limit"`, `"stop"`, `"stop_limit"`, `"take_profit"`, `"market_to_limit"`,
/// `"trailing"`, `"trailing_limit"`, `"pegged"`, `"trailing_take_profit"`). `stop` is a stop-loss trigger, `take_profit` a take-profit
/// trigger, `stop_limit` a stop whose fill is a limit, `trailing` a stop that trails the market by
/// [`OrderRequest::trail`] (or [`OrderRequest::trail_percent`]), `trailing_limit` that trailing stop whose fill is a limit
/// [`OrderRequest::limit_offset`] past its stop, and `trailing_take_profit` the if-touched twin of
/// `trailing`; all six are TRIGGER kinds
/// ([`OrderType::is_trigger`]), the last three the TRAILING kinds ([`OrderType::is_trailing`]).
/// `market_to_limit` is a market order whose unfilled remainder rests as a limit at its first fill's
/// price, and `pegged` an order the venue keeps priced off a [`OrderPeg`]. Venue spellings
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
///   accepted) and [`OrderType::from_stored`] (a venue's report): an EXACT match of the ten
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
    /// an unpriced taker whose unfilled remainder rests as a limit at its first fill's price. An
    /// older build reads the word as `Unrecognized` and refuses to send it.
    MarketToLimit,
    /// a trailing stop: it trails the best price since it activated by `trail` (in price, never a
    /// percent), from `trigger_price` when it states one (its activation level), at once when not,
    /// and fires as a market order. An older build reads the word as `Unrecognized` and refuses to
    /// send it.
    Trailing,
    /// a [`OrderType::Trailing`] stop whose fill is a limit `limit_offset` past the stop level it
    /// fired at (worse by the offset: below it for a sell, above it for a buy). Read by an older
    /// build as `trailing` is.
    TrailingLimit,
    /// a pegged order: its price follows [`OrderRequest::peg`]'s anchor, moved by the peg's offset,
    /// as the market moves. It states no price and no trigger. Where the venue holds the peg (ibkr's
    /// row, on the anchors it lists) it is sent as itself; everywhere else the emulated route is the
    /// engine's, `vike_execution::emulated` (decision 0275): plain limits, one at a time, re-priced on
    /// every move. Polymarket, aster and an IOC or FOK lifetime keep their refusal by name. An older
    /// build reads the word as `Unrecognized` and refuses to send it.
    Pegged,
    /// the if-touched twin of a [`OrderType::Trailing`] stop (IB `TRAIL MIT`): a sell tracks the
    /// LOWEST price since it rests and fires when the price rises `trail` (or `trail_percent`) above
    /// it, a buy mirrors it. With a `limit_offset` (zero or more) its fill is a limit that far past
    /// the trigger (IB `TRAIL LIT`), as `take_profit` is market- or limit-if-touched by its price. It
    /// states no price and, in this wave, no activation. An older build reads the word as
    /// `Unrecognized` and refuses to send it.
    TrailingTakeProfit,
    /// a word outside the ten, kept verbatim (see the type doc). MUST stay the LAST variant:
    /// serde tries the named variants first and falls back to this one.
    #[serde(untagged)]
    Unrecognized(String),
}

impl OrderType {
    /// The ten recognised kinds, in declaration order. A HAND list: its completeness is held by
    /// the test `order_type_known_is_every_recognised_variant_in_declaration_order` beside this
    /// module, whose exhaustive `match` stops compiling when a variant is added.
    pub const KNOWN: &'static [OrderType] = &[
        OrderType::Market,
        OrderType::Limit,
        OrderType::Stop,
        OrderType::StopLimit,
        OrderType::TakeProfit,
        OrderType::MarketToLimit,
        OrderType::Trailing,
        OrderType::TrailingLimit,
        OrderType::Pegged,
        OrderType::TrailingTakeProfit,
    ];

    /// The wire word: the canonical lowercase word, or an `Unrecognized` kind's text verbatim.
    pub fn as_str(&self) -> &str {
        match self {
            OrderType::Market => "market",
            OrderType::Limit => "limit",
            OrderType::Stop => "stop",
            OrderType::StopLimit => "stop_limit",
            OrderType::TakeProfit => "take_profit",
            OrderType::MarketToLimit => "market_to_limit",
            OrderType::Trailing => "trailing",
            OrderType::TrailingLimit => "trailing_limit",
            OrderType::Pegged => "pegged",
            OrderType::TrailingTakeProfit => "trailing_take_profit",
            OrderType::Unrecognized(text) => text,
        }
    }

    /// One of the ten kinds this platform knows (not [`OrderType::Unrecognized`]).
    pub fn is_recognized(&self) -> bool {
        !matches!(self, OrderType::Unrecognized(_))
    }

    /// A TRIGGER kind: `stop`, `stop_limit`, `take_profit`, `trailing`, `trailing_limit`,
    /// `trailing_take_profit` — exactly `vike_venues::venue_caps`'s `TRIGGER_KINDS`, which the
    /// capability preflight classifies by.
    pub fn is_trigger(&self) -> bool {
        matches!(
            self,
            OrderType::Stop
                | OrderType::StopLimit
                | OrderType::TakeProfit
                | OrderType::Trailing
                | OrderType::TrailingLimit
                | OrderType::TrailingTakeProfit
        )
    }

    /// A TRAILING kind: `trailing`, `trailing_limit`, `trailing_take_profit`. The kinds whose `trail`
    /// (or `trail_percent`) is live (every other request's `trail` is backtest-only) and whose
    /// `trigger_price` is an OPTIONAL activation level rather than the trigger itself
    /// ([`order_terms_refusal`]; the trailing take-profit takes none in this wave).
    pub fn is_trailing(&self) -> bool {
        matches!(
            self,
            OrderType::Trailing | OrderType::TrailingLimit | OrderType::TrailingTakeProfit
        )
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
    /// case-insensitive. Anything that is not one of the ten words is refused; the error's
    /// `Display` names the accepted words.
    pub fn parse(input: &str) -> Result<OrderType, OrderTypeError> {
        let word = input.trim();
        OrderType::KNOWN
            .iter()
            .find(|k| k.as_str().eq_ignore_ascii_case(word))
            .cloned()
            .ok_or_else(|| OrderTypeError { input: input.to_owned() })
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

/// [`OrderType::parse`] refused its input: not one of the ten order-type words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderTypeError {
    /// the refused input, verbatim
    pub input: String,
}

impl core::fmt::Display for OrderTypeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write_unknown_word(f, "order type", &self.input, &OrderType::accepted_words())
    }
}

impl std::error::Error for OrderTypeError {}

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
/// - core emulator (`OrderEmulationBook`) / backtest: trade/bar prices (last)
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

/// WHEN a held order may be released: sent to its venue by the live core, made live in a book by
/// the simulator. Every condition it states must hold, an AND: the time `at` (FIX EffectiveTime
/// (168), IB `goodAfterTime`) and, for a CONDITIONAL order (the owner's ruling: a time release plus
/// a price condition, IB's price condition), a [`OrderReleasePrice`].
///
/// The core's held table and the fill lanes' deferral go through this one type and this one
/// predicate ([`OrderRelease::holds`]), so a condition added here reaches every holder.
/// `deny_unknown_fields` is what makes that growth safe: a build that does not know a condition
/// REFUSES the whole record (a journal rollback door, like a new variant) instead of dropping the
/// condition and releasing on time alone.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderRelease {
    /// not before this instant, epoch ms: the order takes part in events stamped at or after it
    pub at: i64,
    /// and not before this price condition is met; `None` (every release written before the field,
    /// skipped on the wire) is time alone, byte for byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<OrderReleasePrice>,
}

/// A price condition of a [`OrderRelease`]: `symbol`'s price at or beyond `level` on the `side` it names.
/// A level, not an edge: it is met while the price stands there, so a price already past the level at
/// `at` releases the order then (IB's price condition reads the same way).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderReleasePrice {
    /// the instrument whose price is read, on the venue of the order it holds back (or on `venue`)
    pub symbol: ustr::Ustr,
    /// the price level
    pub level: f64,
    /// which side of `level` meets the condition
    pub side: OrderReleasePriceSide,
    /// The venue whose price is read, a roster id ([`price_source_refusal`]), when it is NOT the
    /// order's own: a PRICE SOURCE only, never traded. `None` is the order's own venue, the
    /// condition every build before the field wrote, byte for byte; a door writes a condition naming
    /// the order's own venue as `None`. ⚠ `deny_unknown_fields` above: a build that does not know the
    /// key cannot parse a record carrying it, so a journal holding one is stamped a format step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue: Option<ustr::Ustr>,
}

/// Why `venue` cannot be a [`OrderReleasePrice::venue`], in words, or `None`: it must be a roster id
/// exactly (`crate::VENUES`, lower case, no alias), since the core finds that venue's engines by it.
/// Whether this node runs a price feed for it is the node's question, at accept.
#[must_use]
pub fn price_source_refusal(venue: &str) -> Option<String> {
    crate::venues::roster_id(venue).is_none().then(|| {
        format!(
            "the price venue `{venue}` is not a venue this platform knows (one of {})",
            crate::VENUES.join(", ")
        )
    })
}

/// Which side of a [`OrderReleasePrice::level`] meets it, the level itself included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderReleasePriceSide {
    /// the price is at or above the level
    Above,
    /// the price is at or below the level
    Below,
}

impl OrderReleasePrice {
    /// Whether `price` meets the condition. A price that is no finite number meets nothing.
    #[inline]
    #[must_use]
    pub fn met_by(self, price: f64) -> bool {
        price.is_finite()
            && match self.side {
                OrderReleasePriceSide::Above => price >= self.level,
                OrderReleasePriceSide::Below => price <= self.level,
            }
    }
}

impl OrderRelease {
    /// Whether every condition holds at `now_ms`: a clock or an event stamped at or after `at`, AND,
    /// when a [`OrderReleasePrice`] is stated, its symbol's price as `price_of` answers it meets it. A
    /// price `price_of` cannot answer (`None`) holds nothing back from a time-only release and
    /// releases no conditional one. `price_of` is asked only when the time holds.
    #[inline]
    #[must_use]
    pub fn holds(self, now_ms: i64, price_of: impl FnOnce(&str) -> Option<f64>) -> bool {
        now_ms >= self.at
            && self.price.is_none_or(|c| price_of(c.symbol.as_str()).is_some_and(|p| c.met_by(p)))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The ONE time-in-force expiry law (dedup A4).
//
// Extracted so the two sites that decide "is this resting order expired?" — the
// backtest paper exchange (`crates/vike-paper/src/client/expiry.rs`'s `OrderExpiry` — `vike_backtest::paper`
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
/// - `Gtc` / `Ioc` / `Fok` / `PostOnly`: never expired by this predicate. (`Ioc`/`Fok` are
///   immediate-execution semantics, not a resting deadline; they are enforced — if at all — on the
///   arrival bar, not here. `PostOnly` rests like `Gtc`: it is judged once, at arrival.)
/// - `Gtd`: expired once `now_ms >= deadline`, an INCLUSIVE boundary (at the deadline the order is
///   expired). A `Gtd` with no `gtd_expiry` has no deadline and never expires.
/// - `Day`: expired once `now_ms` lands on a strictly later UTC day than `day_anchor_ms` (the
///   order's session anchor). Same-day (including the anchor itself) is never expired.
/// - `AtOpen` / `AtClose`: never expired by this predicate. Their deadline is the auction itself,
///   which only a book that knows the session enforces (it ends the order after its one auction).
///
/// `day_anchor_ms` is meaningful only for `Day`; callers pass any value (e.g. `now_ms`) for the
/// other variants. The caller owns choosing the anchor — the backtest anchors on the first bar the
/// order sees; the live core anchors on the order's creation wall-clock.
pub fn time_in_force_expired(
    tif: TimeInForce,
    gtd_expiry: Option<i64>,
    day_anchor_ms: i64,
    now_ms: i64,
) -> bool {
    match tif {
        TimeInForce::Gtd => gtd_expiry.is_some_and(|deadline| now_ms >= deadline),
        TimeInForce::Day => utc_day(now_ms) > utc_day(day_anchor_ms),
        TimeInForce::Gtc
        | TimeInForce::Ioc
        | TimeInForce::Fok
        | TimeInForce::PostOnly
        | TimeInForce::AtOpen
        | TimeInForce::AtClose => false,
    }
}

/// Why the lifetime an operator (or a caller of the node's `submit`) asked for cannot be sent, in
/// words, or `None`. The ONE rule the CLI (before it asks the node) and the node's door (before it
/// lowers the order) both apply, so a refusal reads the same wherever it is raised:
///
/// - a lifetime other than the default `gtc` belongs to a LIMIT order: a market order has no resting
///   price to expire or protect, and a stop's lifetime is its trigger's business (no adapter reads a
///   request TIF there), so the word would be dropped silently. The auction lifetimes
///   ([`TimeInForce::is_auction`]) are the exception: they belong to a market OR a limit order
///   (market-on-open, limit-on-close), and `at-open` to a `market_to_limit` too (the opening
///   auction's market-to-limit, IB `AUC`: its remainder rests as a limit at the auction's price; not
///   `at-close`, after which nothing of the session is left for a remainder), and to nothing else;
/// - `gtd` needs an expiry in the future (`gtd_expiry`, epoch ms) and nothing but `gtd` takes one;
/// - a [`OrderRelease`] must be in the future (a past one is a clock in the wrong unit far more often
///   than an intent), and a `gtd` order must not expire at or before its release.
///
/// Whether the VENUE can do the lifetime is a different question, answered by its capability row
/// (`vike_venues::preflight_order`'s `TIF_UNSUPPORTED`), not here.
#[must_use]
pub fn lifetime_refusal(
    order_type: &OrderType,
    tif: TimeInForce,
    gtd_expiry: Option<i64>,
    release: Option<OrderRelease>,
    now_ms: i64,
) -> Option<String> {
    let auction_kind = matches!(order_type, OrderType::Market | OrderType::Limit)
        || (tif == TimeInForce::AtOpen && *order_type == OrderType::MarketToLimit);
    if tif.is_auction() && !auction_kind {
        let also = if tif == TimeInForce::AtOpen { " (or a market_to_limit order)" } else { "" };
        return Some(format!(
            "time in force `{}` belongs to a market or a limit order{also}; this is a {order_type} \
             order",
            tif.as_word()
        ));
    }
    if tif != TimeInForce::Gtc && !tif.is_auction() && *order_type != OrderType::Limit {
        return Some(format!(
            "time in force `{}` belongs to a limit order; this is a {order_type} order",
            tif.as_word()
        ));
    }
    let expiry = match (tif, gtd_expiry) {
        (TimeInForce::Gtd, None) => Some("time in force `gtd` needs an expiry".to_string()),
        (TimeInForce::Gtd, Some(deadline)) if deadline <= now_ms => {
            Some(format!("the `gtd` expiry {deadline} is not in the future (now {now_ms})"))
        }
        (TimeInForce::Gtd, Some(_)) | (_, None) => None,
        (other, Some(_)) => {
            Some(format!("an expiry belongs to time in force `gtd`, not `{}`", other.as_word()))
        }
    };
    if expiry.is_some() {
        return expiry;
    }
    let at = release?.at;
    if at <= now_ms {
        return Some(format!("the release {at} is not in the future (now {now_ms})"));
    }
    match (tif, gtd_expiry) {
        (TimeInForce::Gtd, Some(deadline)) if deadline <= at => Some(format!(
            "the `gtd` expiry {deadline} is not after the release {at}: the order would expire \
             before it is released"
        )),
        _ => None,
    }
}

/// Why an order's PRICES cannot be sent, in words, or `None`. The ONE rule the node's door, the
/// CLI, the MCP tool and the strategy drain apply beside [`lifetime_refusal`], so a trigger kind
/// never leaves without the trigger it fires at (an adapter would send it as the plain order its
/// `price` makes). It refuses ONLY:
///
/// - a price or trigger price that is not a finite number;
/// - a trigger kind ([`OrderType::is_trigger`]) with no trigger price above zero;
/// - a `stop_limit` with no limit price above zero;
/// - a trigger kind that STATES a limit price (`price`) of zero or below (a `take_profit` that
///   states none is a market-if-touched order and stays admitted);
/// - a `market_to_limit` that states a limit price at all: its limit is the price of its first
///   fill, so a stated one would be dropped or read as something else;
/// - a TRAILING kind ([`OrderType::is_trailing`]) that states a price (its stop trails the market and
///   a `trailing_limit`'s limit is its offset from that stop, so a price would be dropped), that has
///   not exactly one distance (a `trail` above zero, a finite distance in price, or a
///   `trail_percent` above 0 and below 100), or whose activation (`trigger_price`, optional) is not
///   above zero; a `trailing_limit` with no `limit_offset` of zero or more;
/// - a `trailing_take_profit` that states an activation (`trigger_price`: it would run the other way
///   from a trailing stop's, and no venue page says how), or a negative `limit_offset` (its offset is
///   optional: none is IB's `TRAIL MIT`, one `TRAIL LIT`);
/// - a `limit_offset` on any kind but `trailing_limit` and `trailing_take_profit`, a `trail_percent` on
///   any kind that does not trail;
/// - a `pegged` order with no `peg`, or one that states a price or a trigger price (the venue prices
///   it off the peg's anchor), or whose peg offset is not a finite number; a `peg` on any other kind.
///
/// `trail` is read for the three trailing kinds ONLY: on every other request it keeps its backtest-only
/// meaning and is never judged here. A caller whose carrier has no trail yet passes `None`, and a
/// trailing kind is refused there in this rule's words; so is a `pegged` order where the carrier has no
/// peg. A caller whose carrier has no percent passes `None` for `trail_percent`.
///
/// The lifetime terms of the other wave-2 orders (an auction lifetime, a [`OrderRelease`]) are
/// [`lifetime_refusal`]'s.
///
/// Nothing else: a market or limit order that also carries a trigger price is left alone (the
/// pre-trade gate sizes one), a plain `limit` at zero or below is left alone too (a combo net can be
/// negative, `vike_client::verbs::unsizeable_submit_refusal`'s doc), and whether the VENUE takes the
/// kind is its capability row's question (`vike_venues::preflight_order`), not this one's.
#[must_use]
pub fn order_terms_refusal(
    order_type: &OrderType,
    price: Option<f64>,
    trigger_price: Option<f64>,
    trail: Option<f64>,
    limit_offset: Option<f64>,
    peg: Option<&OrderPeg>,
    trail_percent: Option<f64>,
) -> Option<String> {
    for (what, px) in [
        ("price", price),
        ("trigger price", trigger_price),
        ("limit offset", limit_offset),
        ("peg offset", peg.map(|p| p.offset)),
        ("trail percent", trail_percent),
    ] {
        if let Some(px) = px
            && !px.is_finite()
        {
            return Some(format!("the {what} {px} is not a finite number"));
        }
    }
    if let Some(why) = peg_terms_refusal(order_type, price, trigger_price, peg) {
        return Some(why);
    }
    if *order_type == OrderType::MarketToLimit
        && let Some(px) = price
    {
        return Some(format!(
            "a {order_type} order takes no limit price (its limit is the price of its first fill); \
             this one states {px}"
        ));
    }
    if let Some(offset) = limit_offset
        && !matches!(order_type, OrderType::TrailingLimit | OrderType::TrailingTakeProfit)
    {
        return Some(format!(
            "a limit offset belongs to a trailing_limit order (or a trailing_take_profit one); this \
             {order_type} order states {offset}"
        ));
    }
    if order_type.is_trailing() {
        return trailing_terms_refusal(
            order_type,
            price,
            trigger_price,
            trail,
            limit_offset,
            trail_percent,
        );
    }
    if let Some(p) = trail_percent {
        return Some(format!(
            "a trail percent belongs to a trailing order; this {order_type} order states {p}"
        ));
    }
    if order_type.is_trigger() && !trigger_price.is_some_and(|t| t > 0.0) {
        return Some(format!("a {order_type} order needs a trigger price above zero"));
    }
    if *order_type == OrderType::StopLimit && !price.is_some_and(|p| p > 0.0) {
        return Some(format!("a {order_type} order needs a limit price above zero"));
    }
    if order_type.is_trigger()
        && let Some(px) = price
        && px <= 0.0
    {
        return Some(format!("the limit price {px} of a {order_type} order is not above zero"));
    }
    None
}

impl OrderRequest {
    /// [`order_terms_refusal`] over this request's own terms (kind, price, trigger, trail, limit
    /// offset, peg and trail percent): the whole-request call a bridge's last-line floor makes.
    #[must_use]
    pub fn terms_refusal(&self) -> Option<String> {
        order_terms_refusal(
            &self.order_type,
            self.price,
            self.trigger_price,
            self.trail,
            self.limit_offset,
            self.peg.as_ref(),
            self.trail_percent,
        )
    }
}

/// [`order_terms_refusal`]'s rules for a pegged order and for a peg (the numbers already finite).
fn peg_terms_refusal(
    order_type: &OrderType,
    price: Option<f64>,
    trigger_price: Option<f64>,
    peg: Option<&OrderPeg>,
) -> Option<String> {
    if *order_type != OrderType::Pegged {
        return peg.map(|p| {
            format!(
                "a peg belongs to a pegged order; this {order_type} order states one (to the {} \
                 price)",
                p.anchor.as_word()
            )
        });
    }
    if peg.is_none() {
        return Some(format!(
            "a {order_type} order needs its peg: an anchor ({}) and an offset",
            OrderPegAnchor::accepted_words()
        ));
    }
    [("price", price), ("trigger price", trigger_price)].into_iter().find_map(|(what, px)| {
        px.map(|px| {
            format!(
                "a {order_type} order takes no {what} (the venue prices it off its peg); this one \
                 states {px}"
            )
        })
    })
}

/// [`order_terms_refusal`]'s rules for a trailing kind (the numbers already finite, but `trail`).
fn trailing_terms_refusal(
    order_type: &OrderType,
    price: Option<f64>,
    trigger_price: Option<f64>,
    trail: Option<f64>,
    limit_offset: Option<f64>,
    trail_percent: Option<f64>,
) -> Option<String> {
    let take_profit = *order_type == OrderType::TrailingTakeProfit;
    if let Some(px) = price {
        return Some(if take_profit {
            format!(
                "a {order_type} order takes no price (its trigger trails the market by its trail, \
                 and its limit, when it has one, is its limit offset from that trigger); this one \
                 states {px}"
            )
        } else {
            format!(
                "a {order_type} order takes no price (its stop trails the market by its trail, and \
                 a trailing_limit's limit is its limit offset from that stop); this one states {px}"
            )
        });
    }
    match (trail, trail_percent) {
        (Some(t), _) if !t.is_finite() => {
            return Some(format!("the trail {t} is not a finite number"));
        }
        (Some(_), Some(_)) => {
            return Some(format!(
                "a {order_type} order trails by a distance or by a percent, not both"
            ));
        }
        (Some(t), None) if t > 0.0 => {}
        (None, Some(p)) if p > 0.0 && p < 100.0 => {}
        (None, Some(p)) => {
            return Some(format!(
                "the trail percent {p} of a {order_type} order is not above 0 and below 100"
            ));
        }
        _ => return Some(format!("a {order_type} order needs a trail distance above zero")),
    }
    if take_profit && let Some(t) = trigger_price {
        return Some(format!(
            "a {order_type} order takes no activation price in this build (it would run the other way \
             from a trailing stop's); this one states {t}"
        ));
    }
    if let Some(t) = trigger_price
        && t <= 0.0
    {
        return Some(format!("the activation price {t} of a {order_type} order is not above zero"));
    }
    match (order_type, limit_offset) {
        (OrderType::TrailingLimit, None) => {
            Some(format!("a {order_type} order needs a limit offset of zero or more"))
        }
        (_, Some(o)) if o < 0.0 => {
            Some(format!("the limit offset {o} of a {order_type} order is below zero"))
        }
        _ => None,
    }
}

/// Why an order's DISPLAY quantity ([`OrderRequest::display_quantity`]) cannot be sent, in words, or `None`
/// (`None` too when it names none). The ONE rule the node's door, the CLI and the MCP tool apply beside
/// [`order_terms_refusal`]. It refuses:
///
/// - a display on anything but a `limit` order: only a resting limit has a part to hide;
/// - a display that is not a number of zero or more (NaN included). A display of ZERO is a HIDDEN
///   order: the venue shows none of it;
/// - a display of at least the order's quantity, which hides nothing.
///
/// Whether the VENUE can show a slice is its capability row's question (`vike_venues::venue_caps`'s
/// `supports_display_quantity`), and whether it can hide a whole order its `hidden` attribute word's, not
/// this one's.
#[must_use]
pub fn display_refusal(
    order_type: &OrderType,
    qty: f64,
    display_qty: Option<f64>,
) -> Option<String> {
    let shown = display_qty?;
    if *order_type != OrderType::Limit {
        return Some(format!(
            "a display quantity belongs to a limit order; this is a {order_type} order"
        ));
    }
    if shown.is_nan() || shown < 0.0 {
        return Some(format!("the display quantity {shown} is not zero or more"));
    }
    (shown >= qty).then(|| {
        format!(
            "the display quantity {shown} is not below the order's quantity {qty}: it hides nothing"
        )
    })
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
/// `trigger_price` the trigger level (on a trailing kind, its OPTIONAL activation level). The
/// contingency slots carry a linked group's links ([`build_bracket`], [`build_order_list`]).
/// The five trailing fields
/// (`weight`/`stop`/`trail`/`extreme`/`on_close`) are backtest-only additive fields — the live path
/// ignores them — except `trail` on the three trailing kinds ([`OrderType::is_trailing`]), where it is
/// the live trailing distance, beside [`OrderRequest::limit_offset`].
///
/// `Default` is derived so callers can build with `OrderRequest { ..Default::default() }` on the
/// hot path (the live runtime's per-order construction) without a serde round-trip. Every field's
/// Rust `Default` equals its `#[serde(default)]`, pinned by `default_literal_matches_serde` below.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OrderRequest {
    pub order_client_id: String,
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
    pub timestamp: i64,
    // --- reserved contingency (OCO/brackets) ---
    #[serde(default)]
    pub parent_order_id: Option<String>,
    #[serde(default)]
    pub linked_order_ids: Vec<String>,
    #[serde(default)]
    pub order_list_id: Option<String>,
    /// How this leg's fill acts on `linked_order_ids` ([`ContingencyType`]); the bytes the `String`
    /// it replaced carried (`"OTO"`, `"OCO"`).
    #[serde(default)]
    pub contingency_type: Option<ContingencyType>,
    // --- backtest-only additive fields ---
    #[serde(default)]
    pub weight: f64,
    #[serde(default)]
    pub stop: Option<f64>,
    /// The trailing DISTANCE, in price (never a percent: that is [`OrderRequest::trail_percent`]). LIVE on the three trailing kinds
    /// ([`OrderType::is_trailing`]), which [`order_terms_refusal`] refuses without one above zero; on
    /// every other request the backtest-only field it always was (`OrderResting::from_request` lowers
    /// any request that carries one to a trailing stop), byte for byte.
    #[serde(default)]
    pub trail: Option<f64>,
    #[serde(default)]
    pub extreme: Option<f64>,
    #[serde(default)]
    pub on_close: bool,
    /// Combo legs — EMPTY means "not a combo", i.e. every existing order (see [`OrderComboLeg`]).
    ///
    /// Present ⇒ this ONE request IS the whole multi-leg order (never N children): `symbol` names
    /// the venue's combo instrument once the adapter has resolved it (it may be empty at submit
    /// time), `price` is the SIGNED NET limit per combo unit, `qty` is combo UNITS, and
    /// `order_type` stays [`OrderType::Limit`]/[`OrderType::Market`] as usual.
    ///
    /// Serde: `default` + `skip_serializing_if` empty, so every pre-existing wire body, fixture and
    /// journal record stays BYTE-IDENTICAL (`order_request_without_combo_legs_is_byte_identical`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub combo_legs: Vec<OrderComboLeg>,
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
    /// row that `vike_venues::capabilities_for` / `preflight_order_at` / `amend_semantics` are facts about.
    /// Collapsing the two into one per-account string makes the capability lookup read a name
    /// `vike_model::VENUES` does not contain, whereupon `preflight_order_at`'s unknown-venue
    /// affordance answers `Ok(())` and every capability check is skipped in silence. So `venue` stays
    /// canonical and this is the second coordinate, exactly as `vike_execution::Engine` already
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
    /// A RECORD, never a gate: it neither refuses nor holds an order, and no adapter reads it, except the ibkr
    /// bridge's one rule for an order that OPENS exposure off a non-real-time stream (a market order, or a price
    /// outside a 5 percent band around the delayed mark, is refused: decision 0140's 2026-10-27 amendment, E07).
    /// `None` means no label was stated for that stream (a venue that does not label its data, or
    /// none reported yet) — not "real-time".
    ///
    /// Serde: `default` + `skip_serializing_if` `None`, so every pre-existing wire body, fixture
    /// and journal record stays BYTE-IDENTICAL
    /// (`order_request_without_data_delay_is_byte_identical`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_delay: Option<crate::StreamDelay>,
    /// The quantity the venue SHOWS of this order (an iceberg's visible slice, FIX DisplayQuantity
    /// 1138); `None` shows all of it. Where the venue's capability row does not wire it
    /// (`vike_venues::venue_caps`'s `supports_display_quantity`) the emulated route is the engine's,
    /// `vike_execution::emulated` (decision 0275: one plain limit of the visible size at a time), and a
    /// hidden order (0) is refused at the core edge; the order is never sent at its full size.
    ///
    /// Serde: `default` + `skip_serializing_if` `None`, so every pre-existing wire body, fixture
    /// and journal record stays BYTE-IDENTICAL
    /// (`order_request_without_display_or_release_is_byte_identical`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_quantity: Option<f64>,
    /// Hold the order until its [`OrderRelease`] holds; `None` sends it now. The live core holds it in
    /// its own table and sends it then, through the ordinary submit path (so the pre-trade gate
    /// judges it at release); the simulator defers it in every fill lane
    /// ([`OrderResting::is_held`]). [`lifetime_refusal`] refuses one in the past.
    ///
    /// Serde: as [`OrderRequest::display_quantity`]. ⚠ A build older than the field drops the key
    /// silently and sends the order NOW (the node's wire needs a feature word for it); a build that
    /// has the field refuses a record whose `OrderRelease` carries a condition it does not know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<OrderRelease>,
    /// A [`OrderType::TrailingLimit`]'s limit offset, zero or more, in price: at the stop level P it
    /// fires at, its limit is P − this for a sell and P + this for a buy. `None` on every other kind,
    /// and [`order_terms_refusal`] refuses one there.
    ///
    /// Serde: `default` + `skip_serializing_if` `None`, so every pre-existing wire body, fixture
    /// and journal record stays BYTE-IDENTICAL (`order_request_without_limit_offset_is_byte_identical`).
    /// ⚠ A build older than the field drops the key, and reads the kind as `Unrecognized` and
    /// refuses it, so no order goes out without its offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_offset: Option<f64>,
    /// A [`OrderType::Pegged`] order's peg: the anchor the venue prices it off and the offset from
    /// it. `None` on every other kind, and [`order_terms_refusal`] refuses one there.
    ///
    /// Serde: `default` + `skip_serializing_if` `None`, so every pre-existing wire body, fixture
    /// and journal record stays BYTE-IDENTICAL (`order_request_without_peg_is_byte_identical`). ⚠ A
    /// build older than the field drops the key, and reads the kind as `Unrecognized` and refuses
    /// it, so no order goes out without its peg.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peg: Option<OrderPeg>,
    /// A LIMIT order's attributes ([`OrderAttributes`]: all-or-none, sweep-to-fill, a discretionary
    /// amount), judged by [`attributes_refusal`] at the doors and by the venue's attribute words
    /// (`vike_venues::venue_caps`'s `ORDER_ATTRIBUTES`) at the core edge. `None` on every other order.
    ///
    /// Serde: `default` + `skip_serializing_if` `None`, so every pre-existing wire body, fixture and
    /// journal record stays BYTE-IDENTICAL (`order_request_without_the_attribute_keys_is_byte_identical`).
    /// ⚠ A build older than the field DROPS the key silently and sends a plain limit (the node's wire
    /// carries it under a feature word for that reason).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<OrderAttributes>,
    /// A trailing kind's distance as a PERCENT of the extreme, above 0 and below 100, in place of
    /// `trail` (exactly one of the two: [`order_terms_refusal`]); `trail` keeps its meaning, a price,
    /// byte for byte. Serde: as [`OrderRequest::attributes`]; an older build drops it, and a trailing
    /// order with only a percent then has no trail and is refused by its terms rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail_percent: Option<f64>,
    /// A `stop` or `stop_limit`'s ADJUSTMENT ([`OrderStopAdjust`]): once the price reaches its level, the
    /// order becomes its target (another stop, a stop-limit or a trailing stop), once. Judged by
    /// [`stop_adjust_refusal`]. Serde: as [`OrderRequest::attributes`]; an older build drops it silently and
    /// keeps the plain stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjust: Option<OrderStopAdjust>,
}

/// The ATTRIBUTES a limit order may carry beside its kind (IB's order attributes): fields, not
/// kinds, so a limit stays a limit everywhere it is read. Each member is skipped when empty, and
/// `deny_unknown_fields` makes a build that does not know a later member REFUSE the record instead
/// of dropping it. A HIDDEN order is no member: it is a display of zero ([`OrderRequest::display_quantity`]).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderAttributes {
    /// Fill all of it in one execution or none of it (IB `allOrNone`); a resting order, unlike `fok`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all_or_none: bool,
    /// Take every venue's liquidity at once up to the limit (IB `sweepToFill`); on one book, a limit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sweep_to_fill: bool,
    /// The market sees the limit L; the order executes up to L + this for a buy, L − this for a sell
    /// (IB `discretionaryAmt`), above zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discretionary_amount: Option<f64>,
}

/// A stop's ADJUSTMENT (IB's adjustable stop; the auto trailing stop is its [`OrderStopAdjustTo::Trailing`]
/// target): once the price reaches `at` (a sell stop's at or above it, a buy stop's at or below), the
/// order becomes `to` in place, once. `deny_unknown_fields`, as [`OrderRelease`]: a build that does not know
/// a term refuses the record instead of dropping it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderStopAdjust {
    /// the level that triggers the adjustment, on the protected side of the stop
    pub at: f64,
    /// what the order becomes there
    pub to: OrderStopAdjustTo,
}

/// The target of a [`OrderStopAdjust`]. Serde: the snake_case variant word as the key of its terms
/// (`{"stop_limit":{"stop":..,"limit":..}}`); `deny_unknown_fields` on every variant's terms.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum OrderStopAdjustTo {
    /// a stop at `stop` (IB `adjustedOrderType` `STP`)
    Stop { stop: f64 },
    /// a stop-limit, the trigger `stop`, the limit `limit` (`STP LMT`)
    StopLimit { stop: f64, limit: f64 },
    /// a trailing stop whose stop starts at `stop` and trails by exactly one of `trail` (in price)
    /// and `trail_percent` (`TRAIL`, IB's `adjustableTrailingUnit` 0 or 100)
    Trailing {
        stop: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trail: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trail_percent: Option<f64>,
    },
}

impl OrderStopAdjustTo {
    /// The target's stop level.
    #[must_use]
    pub fn stop(self) -> f64 {
        match self {
            OrderStopAdjustTo::Stop { stop }
            | OrderStopAdjustTo::StopLimit { stop, .. }
            | OrderStopAdjustTo::Trailing { stop, .. } => stop,
        }
    }
}

/// Why a limit order's [`OrderAttributes`] cannot be sent, in words, or `None` (`None` too when it has
/// none). The ONE rule every door applies beside [`order_terms_refusal`]. It refuses:
///
/// - any attribute on anything but a `limit` order, or with an auction lifetime (an auction clears
///   at one price: nothing is left to fill whole, sweep or take at discretion);
/// - all-or-none with `ioc` or `fok` (all-or-none that must execute at once IS `fok`: use it);
/// - sweep-to-fill with `post-only` (a sweep takes liquidity), and a discretionary amount with it (an
///   execution inside the discretion takes liquidity: a discretion is a taker by design);
/// - a discretionary amount that is not a finite number above zero.
///
/// Whether the VENUE takes an attribute is its capability row's question (`vike_venues::venue_caps`'s
/// `ORDER_ATTRIBUTES` words), not this one's.
#[must_use]
pub fn attributes_refusal(
    order_type: &OrderType,
    tif: TimeInForce,
    attributes: Option<&OrderAttributes>,
) -> Option<String> {
    let a = attributes?;
    if *order_type != OrderType::Limit {
        return Some(format!(
            "an order attribute (all-or-none, sweep-to-fill, discretion) belongs to a limit order; \
             this is a {order_type} order"
        ));
    }
    let word = tif.as_word();
    if tif.is_auction() {
        return Some(format!(
            "an order attribute takes no auction lifetime (`{word}`): an auction clears at one price"
        ));
    }
    if a.all_or_none && matches!(tif, TimeInForce::Ioc | TimeInForce::Fok) {
        return Some(format!(
            "an all-or-none order rests until it can fill whole, so it takes no time in force \
             `{word}` (all-or-none at once is `fok`)"
        ));
    }
    if a.sweep_to_fill && tif == TimeInForce::PostOnly {
        return Some(format!(
            "a sweep-to-fill order takes liquidity at once, so it takes no time in force `{word}`"
        ));
    }
    if a.discretionary_amount.is_some() && tif == TimeInForce::PostOnly {
        return Some(format!(
            "a discretionary order takes liquidity inside its discretion, so it takes no time in \
             force `{word}`"
        ));
    }
    match a.discretionary_amount {
        Some(d) if !(d.is_finite() && d > 0.0) => {
            Some(format!("the discretionary amount {d} is not a finite number above zero"))
        }
        _ => None,
    }
}

/// Why a stop's [`OrderStopAdjust`] cannot be sent, in words, or `None` (`None` too when it has none). The
/// ONE rule every door applies beside [`order_terms_refusal`]. `side` is the order's (+1 buy / -1
/// sell), `trigger_price` its stop. It refuses:
///
/// - an adjustment on anything but a `stop` or a `stop_limit`;
/// - an `at` that is not a finite number above zero, or not on the PROTECTED side of the stop (a sell
///   stop's `at` above its stop, a buy stop's below): the stop would fire before the price got there;
/// - a target whose stop is not a finite number above zero, whose terms [`order_terms_refusal`]
///   refuses (a stop-limit target's limit; a trailing target needs exactly one distance), or whose
///   stop is not on the near side of `at` (a sell's below it, a buy's above it): it would fire at once.
#[must_use]
pub fn stop_adjust_refusal(
    order_type: &OrderType,
    side: i32,
    trigger_price: Option<f64>,
    adjust: Option<&OrderStopAdjust>,
) -> Option<String> {
    let adj = adjust?;
    if !matches!(order_type, OrderType::Stop | OrderType::StopLimit) {
        return Some(format!(
            "an adjustment belongs to a stop or a stop_limit order; this is a {order_type} order"
        ));
    }
    let at = adj.at;
    if !(at.is_finite() && at > 0.0) {
        return Some(format!("the adjustment level {at} is not a finite number above zero"));
    }
    let stop = trigger_price.unwrap_or(f64::NAN);
    let protected = if side < 0 { at > stop } else { at < stop };
    if !protected {
        return Some(format!(
            "the adjustment level {at} is not on the protected side of the stop {stop} (a sell \
             stop's above it, a buy stop's below): the stop would fire before the price reached it"
        ));
    }
    let to = adj.to.stop();
    if !(to.is_finite() && to > 0.0) {
        return Some(format!(
            "the adjustment's target stop {to} is not a finite number above zero"
        ));
    }
    let target = match adj.to {
        OrderStopAdjustTo::Stop { .. } => None,
        OrderStopAdjustTo::StopLimit { stop, limit } => order_terms_refusal(
            &OrderType::StopLimit,
            Some(limit),
            Some(stop),
            None,
            None,
            None,
            None,
        ),
        OrderStopAdjustTo::Trailing { trail, trail_percent, .. } => {
            order_terms_refusal(&OrderType::Trailing, None, None, trail, None, None, trail_percent)
        }
    };
    if let Some(why) = target {
        return Some(format!("the adjustment's target: {why}"));
    }
    let near = if side < 0 { to < at } else { to > at };
    (!near).then(|| {
        format!(
            "the adjustment's target stop {to} is not on the near side of its level {at} (a sell's \
             below it, a buy's above it): it would fire at once"
        )
    })
}

/// Which price of the touch an order follows: a NATIVE pegged order's anchor ([`OrderPeg`]) and the
/// anchor the emulated route prices its child off (the emulated route is the engine's,
/// `vike_execution::emulated`). Serde is the word, snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderPegAnchor {
    /// The order's OWN side of the touch: the best bid for a buy, the best ask for a sell (IB's
    /// `PEG BEST`, on IBKR ATS).
    Best,
    /// The midpoint of the touch (IB's `PEG MID`).
    Mid,
    /// The FAR side of the touch: the best ask for a buy, the best bid for a sell (IB's `PEG MKT`).
    Market,
    /// The order's own side of the national best bid and offer (IB's relative order, `REL`,
    /// "pegged to primary"). An executor reads one book, so it prices this one as [`OrderPegAnchor::Best`];
    /// the two differ at the venue only.
    Primary,
}

impl OrderPegAnchor {
    /// Every anchor, in declaration order.
    pub const ALL: [OrderPegAnchor; 4] = [
        OrderPegAnchor::Best,
        OrderPegAnchor::Mid,
        OrderPegAnchor::Market,
        OrderPegAnchor::Primary,
    ];

    /// The anchor's word, as serde spells it.
    pub fn as_word(self) -> &'static str {
        match self {
            Self::Best => "best",
            Self::Mid => "mid",
            Self::Market => "market",
            Self::Primary => "primary",
        }
    }

    /// The anchor a word names, exactly (`"mid"`, never `"Mid"`).
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.as_word() == word)
    }

    /// The words of [`OrderPegAnchor::parse`], comma-separated, for refusal messages.
    pub fn accepted_words() -> String {
        Self::ALL.map(Self::as_word).join(", ")
    }

    /// The price for `side` (+1 buy / -1 sell) on a touch of `bid` / `ask`, moved by `offset`
    /// toward the far side: a positive offset is aggressive (a buy pays more, a sell asks less), a negative
    /// one passive. `None` on a touch it cannot price (a side missing or not finite, a crossed book) or a
    /// price that is not positive. Not snapped to the grid: the caller rounds by side.
    pub fn price(self, side: i32, bid: f64, ask: f64, offset: f64) -> Option<f64> {
        if !(bid.is_finite() && ask.is_finite() && bid > 0.0 && bid <= ask) {
            return None;
        }
        let anchor = match self {
            Self::Best | Self::Primary if side > 0 => bid,
            Self::Best | Self::Primary => ask,
            Self::Market if side > 0 => ask,
            Self::Market => bid,
            Self::Mid => (bid + ask) / 2.0,
        };
        let px = anchor + f64::from(side) * offset;
        (px.is_finite() && px > 0.0).then_some(px)
    }
}

/// A [`OrderType::Pegged`] order's peg: the venue keeps the order at `anchor`'s price moved by
/// `offset`, in price units, signed as in [`OrderPegAnchor::price`] (a positive offset is aggressive; each
/// venue bridge maps the sign onto its own). ⚠ `deny_unknown_fields`: a term added here later is
/// REFUSED by a build that does not know it, never dropped so the order goes out without it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderPeg {
    pub anchor: OrderPegAnchor,
    pub offset: f64,
}

/// How a contingent order's fill acts on the orders [`OrderRequest::linked_order_ids`] names (FIX
/// `ContingencyType`, tag 1385). RUST-NATIVE.
///
/// - `Oto`, one-triggers-other: the fill ARMS the order's children, the orders whose
///   `parent_order_id` names it (held off the venue until then). A bracket's entry.
/// - `Oco`, one-cancels-other: the fill CANCELS every linked order that is not its child. A bracket's
///   exits.
/// - `Ouo`, one-updates-other: a fill of `q` REDUCES every linked order that is not its child by `q`,
///   and one reduced to nothing is cancelled. A ladder's stop and targets.
///
/// Serde is the upper-case word, the exact bytes the `String` this replaced carried for `"OTO"` and
/// `"OCO"` (`bracket_leg_shaped_request_is_byte_identical_too`). ⚠ A word outside the three does not
/// parse, so a record from a build that knows a fourth fails to read here: a fourth word owes a rollback
/// door of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ContingencyType {
    Oto,
    Oco,
    Ouo,
}

/// One combo leg: a venue instrument and its SIGNED ratio per combo unit.
///
/// `ratio` +2 = buy 2 units of this leg per combo BOUGHT; -1 = sell 1 per combo bought. Selling the
/// combo flips every leg (the venue convention, and LEAN's `GroupOrderManager` ratio).
///
/// ⚠ **`deny_unknown_fields` (the owner's ruling 14 of 2026-11-11):** a key a later build adds to a
/// leg is REFUSED by a build that does not know it, never dropped, wherever a leg is read: the
/// tradehub's `Combo` command (`vike_tradehub_client::wire::OrderComboWire::legs`, this type
/// itself), the gateway lease's order intent and the command JOURNAL, whose read of a record
/// carrying such a leg fails to parse rather than replaying the combo without the key. So a new leg
/// field owes a journal format step and a wire feature word. Every build so far wrote only these three keys (`symbol` and `ratio` since the
/// combo shipped, `limit` since format 23), so no journal any build wrote is refused by it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderComboLeg {
    pub symbol: String,
    pub ratio: i32,
    /// This leg's OWN limit price (a combo leg limit, LEAN's `ComboLegLimitOrder`): a price of the
    /// leg's instrument, never a net. The leg BUYS when `side × ratio > 0` and crosses at an ask at or
    /// below it, and SELLS otherwise and crosses at a bid at or above it ([`combo_legs_cross`]). Every
    /// leg of a combo states one or none does, and never beside [`OrderCombo::net_limit`]
    /// ([`OrderCombo::validate`]). `None` on every combo priced by its net.
    ///
    /// Serde: `default` + `skip_serializing_if` `None`, so every leg written before the field keeps
    /// its bytes. ⚠ A build older than the field drops the key and lowers the spec as a combo MARKET
    /// order (no net limit, no leg limit): `vike_journal`'s format step refuses such a journal there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

/// Why a [`OrderCombo`] is not a legal combo. Returned by [`OrderCombo::validate`] and by
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
    InvalidQuantity(f64),
    /// `net_limit`, when present, must be finite (its SIGN is free — credit combos are negative)
    NonFiniteNetLimit(f64),
    /// a net limit AND leg limits: a combo is priced by its net or by its legs, never both
    NetAndLegLimits,
    /// leg `i` states no limit while another leg does: every leg states one or none does
    MissingLegLimit(usize),
    /// leg `i`'s limit is not finite
    NonFiniteLegLimit(usize, f64),
    /// two legs name this one instrument: the per-leg books (fills, marks, risk) are keyed by
    /// symbol, so its legs could not be told apart
    RepeatedLegSymbol(String),
    /// a lifetime other than GTC on a BOOK that models only a resting GTC combo (the simulator's,
    /// the paper book's): [`OrderCombo::book_lifetime_refusal`], never [`OrderCombo::validate`]
    NotGtc(TimeInForce),
}

impl core::fmt::Display for ComboError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooFewLegs(n) => write!(f, "combo needs 2..=N legs, got {n}"),
            Self::ZeroRatio(i) => write!(f, "combo leg {i} has ratio 0"),
            Self::EmptyLegSymbol(i) => write!(f, "combo leg {i} has an empty symbol"),
            Self::InvalidSide(s) => write!(f, "combo side must be +1 or -1, got {s}"),
            Self::InvalidQuantity(q) => write!(f, "combo qty must be finite and > 0, got {q}"),
            Self::NonFiniteNetLimit(p) => write!(f, "combo net_limit must be finite, got {p}"),
            Self::NetAndLegLimits => {
                write!(f, "a combo states a net limit or a limit per leg, never both")
            }
            Self::MissingLegLimit(i) => write!(
                f,
                "combo leg {i} states no limit while another leg does: every leg states one or \
                 none does"
            ),
            Self::NonFiniteLegLimit(i, p) => {
                write!(f, "combo leg {i}'s limit must be finite, got {p}")
            }
            Self::RepeatedLegSymbol(s) => {
                write!(f, "a combo names each leg's instrument once, and {s} twice")
            }
            Self::NotGtc(tif) => {
                write!(f, "a combo rests as a GTC order, not a {} one", tif.as_word())
            }
        }
    }
}

impl std::error::Error for ComboError {}

/// An atomic multi-leg order at a NET price. RUST-NATIVE (no Python twin).
///
/// Ports the LEAN combo-order vocabulary (`Orders/ComboOrder*.cs`) onto the vike model. A combo is
/// ONE order everywhere downstream — one order client id, one [`crate::events::Event`] stream, one
/// `OrderRecord` — because v1 is venue-native (Deribit combo books) and the VENUE is the group
/// manager.
///
/// INVARIANTS (enforced by [`OrderCombo::validate`], which [`build_combo`] and `Deserialize` both
/// run — there is no way to obtain an unvalidated spec from outside this module's constructors):
/// ≥ 2 legs, every `ratio != 0`, every leg symbol non-empty, `side ∈ {-1, +1}`, `qty` finite > 0,
/// `net_limit` finite when present, [`OrderComboLeg::limit`] on every leg or on none, finite, never
/// beside a `net_limit`, and no instrument named by two legs. The lifetime is the venue's to take (a
/// book's limit is [`OrderCombo::book_lifetime_refusal`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "OrderComboUnchecked")]
pub struct OrderCombo {
    pub venue: String,
    /// +1 buy the combo / -1 sell it (legs flip sign with it, venue-side). NEVER 0.
    pub side: i32,
    /// combo units (leg qty = |ratio| × qty, on the leg's own grid)
    pub qty: f64,
    /// 2..=N legs, ordered — leg order is the venue-creation order (Deribit normalizes it)
    pub legs: Vec<OrderComboLeg>,
    /// NET limit per combo unit, SIGNED: Σ(ratio_i × px_i). Debit > 0, credit < 0, zero legal
    /// (Deribit futures spreads quote far-minus-near — negative in backwardation).
    /// `None` = combo MARKET order.
    ///
    /// NOTHING anywhere may clamp this to ≥ 0 — and, equally binding, no consumer may use the
    /// SIGNED net as a MAGNITUDE. A risk/notional/margin check that wants a size must take
    /// `net_limit.abs()` (or a per-leg gross Σ|ratio_i·px_i|·qty); feeding the signed net into a
    /// `notional` makes every credit combo look negative-sized (see `vike_execution::RiskGate::check`).
    pub net_limit: Option<f64>,
    #[serde(default)]
    pub time_in_force: TimeInForce,
    /// As [`OrderBracket::account`], and carried onto the one request [`build_combo`] builds (decision
    /// 0254: every leg of a group on one account; a combo is one order). `None` resolves as it did
    /// before the field. Serde: absent when `None`, so every spec written before it keeps its bytes.
    /// ⚠ A build older than the field drops the key and routes the combo by its venue alone:
    /// `vike_journal`'s format step refuses such a journal there.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::accounts::account_keys::wire_account_option"
    )]
    pub account: Option<crate::accounts::account_keys::AccountLabel>,
    /// The combo's ONE order client id when its sender minted it (a remote combo through the node's
    /// door, as a remote submit's): the runtime places the combo under it instead of minting one, so a
    /// resend under the same id is refused whole. `None` = the runtime mints the id, as before the
    /// field (a strategy's combo). Serde: absent when `None`, so every spec written before it keeps its
    /// bytes. ⚠ A build older than the field drops the key and mints an id of its own on replay:
    /// `vike_journal`'s format step refuses such a journal there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_client_id: Option<String>,
}

/// Deserialization shadow of [`OrderCombo`] — exists ONLY so `#[serde(try_from)]` can route every
/// decoded spec through [`OrderCombo::validate`]. Keep the fields in sync with `OrderCombo`.
#[derive(Deserialize)]
struct OrderComboUnchecked {
    venue: String,
    side: i32,
    qty: f64,
    legs: Vec<OrderComboLeg>,
    net_limit: Option<f64>,
    #[serde(default)]
    time_in_force: TimeInForce,
    #[serde(default, with = "crate::accounts::account_keys::wire_account_option")]
    account: Option<crate::accounts::account_keys::AccountLabel>,
    #[serde(default)]
    order_client_id: Option<String>,
}

impl TryFrom<OrderComboUnchecked> for OrderCombo {
    type Error = ComboError;
    fn try_from(w: OrderComboUnchecked) -> Result<Self, Self::Error> {
        let spec = Self {
            venue: w.venue,
            side: w.side,
            qty: w.qty,
            legs: w.legs,
            net_limit: w.net_limit,
            time_in_force: w.time_in_force,
            account: w.account,
            order_client_id: w.order_client_id,
        };
        spec.validate()?;
        Ok(spec)
    }
}

impl OrderCombo {
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
            if self.legs[..i].iter().any(|e| e.symbol == leg.symbol) {
                return Err(ComboError::RepeatedLegSymbol(leg.symbol.clone()));
            }
        }
        if self.side != 1 && self.side != -1 {
            return Err(ComboError::InvalidSide(self.side));
        }
        if !self.qty.is_finite() || self.qty <= 0.0 {
            return Err(ComboError::InvalidQuantity(self.qty));
        }
        if let Some(p) = self.net_limit
            && !p.is_finite()
        {
            return Err(ComboError::NonFiniteNetLimit(p));
        }
        if self.legs.iter().any(|l| l.limit.is_some()) {
            if self.net_limit.is_some() {
                return Err(ComboError::NetAndLegLimits);
            }
            for (i, leg) in self.legs.iter().enumerate() {
                match leg.limit {
                    None => return Err(ComboError::MissingLegLimit(i)),
                    Some(p) if !p.is_finite() => return Err(ComboError::NonFiniteLegLimit(i, p)),
                    Some(_) => {}
                }
            }
        }
        Ok(())
    }

    /// The lifetime rule of the two BOOKS, the simulator's and the paper book's, which model only a
    /// resting GTC combo: `Some(NotGtc)` for any other lifetime, `None` for GTC. A limit of those
    /// books, not of the order ([`Self::validate`] takes every lifetime: deribit takes IOC, FOK and
    /// GTD combos natively), so both books call this one rule and no venue path does.
    pub fn book_lifetime_refusal(&self) -> Option<ComboError> {
        (self.time_in_force != TimeInForce::Gtc).then_some(ComboError::NotGtc(self.time_in_force))
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

/// [`combo_net`] over real [`OrderComboLeg`]s, with the book side decided ONCE instead of at each call
/// site: `px(symbol, want_ask)` is asked for the ASK when the combo would BUY that leg and the BID
/// when it would SELL it (`want_ask = side * ratio > 0`).
///
/// Leg order is preserved verbatim (the venue-creation order), so the fold order matches
/// [`combo_net`]'s contract.
pub fn combo_net_from_legs(
    side: i32,
    legs: &[OrderComboLeg],
    px: impl Fn(&str, bool) -> f64,
) -> f64 {
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
/// `side == 0` is NOT a buy: the workspace convention is strictly ±1 (see [`OrderCombo::validate`]),
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

/// Build the ONE [`OrderRequest`] that carries a whole [`OrderCombo`] — the combo twin of
/// [`build_bracket`], and STATELESS the same way: the order client id is MINTED BY THE RUNTIME and passed in,
/// so id minting stays in its one place.
///
/// `symbol` is left EMPTY: the venue adapter resolves the combo instrument name at submit
/// (Deribit `private/create_combo`) and substitutes it. `price` carries the SIGNED net limit
/// verbatim — never absolute-valued, never clamped. A combo priced by its legs
/// ([`OrderComboLeg::limit`]) is a `Limit` with NO `price`: its limits ride on `combo_legs`, and a reader
/// that knows only the net refuses a priceless limit combo instead of sending it at market (the
/// deribit bridge's `kind_refusal`).
///
/// FALLIBLE on purpose: [`OrderCombo::validate`] runs first, so a 0-leg spec can never lower to an
/// `OrderRequest` with an EMPTY `combo_legs` — which is the "this is NOT a combo" sentinel and
/// would otherwise hand the venue an ordinary limit order on an empty symbol at the (possibly
/// negative) net price.
pub fn build_combo(spec: &OrderCombo, order_client_id: &str) -> Result<OrderRequest, ComboError> {
    spec.validate()?;
    Ok(OrderRequest {
        order_client_id: order_client_id.to_string(),
        venue: spec.venue.clone(),
        // resolved by the venue adapter at submit (PR-4) — see the doc comment above.
        symbol: String::new(),
        side: spec.side,
        qty: spec.qty,
        order_type: if spec.net_limit.is_some() || spec.legs.iter().any(|l| l.limit.is_some()) {
            OrderType::Limit
        } else {
            OrderType::Market
        },
        price: spec.net_limit,
        time_in_force: spec.time_in_force,
        combo_legs: spec.legs.clone(),
        account: spec.account.clone(),
        ..Default::default()
    })
}

impl OrderResting {
    /// Map a frozen `OrderRequest` intent to a live mutable resting [`OrderResting`].
    /// Dispatch priority (byte-identical to the Python oracle's `order_request_to_working`):
    /// 1. a trailing kind → its own kind (`activation` = trigger_price, a trailing_limit's or a
    ///    trailing_take_profit's `limit_offset`, its `trail_percent`), and `trail.is_some()` on any other
    ///    request → trailing (no activation, the backtest-only meaning); 2. on_close+market → market_close;
    /// 3. on_close+limit → limit_close; 4. stop → stop (price = trigger_price, carries adjust);
    /// 5. stop_limit / take_profit → the same kind (price = trigger_price, limit = price; a stop_limit
    ///    carries adjust);
    /// 6. an auction lifetime on a market or limit → auction_open / auction_close (price = the
    ///    limit's price, `None` for a market; carries stop); 7. market_to_limit → market_to_limit, or
    ///    auction_open_to_limit at `at-open` (carries stop); 8. limit → limit (carries stop, all-or-none and
    ///    the discretionary amount); 9. else market (carries stop).
    ///
    /// Every kind carries `release`. Steps 5 to 7, the trailing kinds of step 1, the release and the
    /// attributes have no Python twin. Before step 5, both trigger kinds fell through to the last step and
    /// the paper book filled them AT ONCE as market orders. A trailing kind with no distance lowers to a
    /// trailing order the fill law cannot read: [`order_terms_refusal`] refuses one before this is asked.
    pub fn from_request(req: &OrderRequest) -> OrderResting {
        let mut o = working_kind(req);
        o.release = req.release;
        o
    }
}

/// [`OrderResting::from_request`]'s dispatch, before the release every kind carries.
fn working_kind(req: &OrderRequest) -> OrderResting {
    let side = req.side;
    let qty = req.qty;
    let trailing = match req.order_type {
        OrderType::Trailing => Some(OrderKind::Trailing),
        OrderType::TrailingLimit => Some(OrderKind::TrailingLimit),
        OrderType::TrailingTakeProfit => Some(OrderKind::TrailingTakeProfit),
        _ => req.trail.map(|_| OrderKind::Trailing),
    };
    if let Some(kind) = trailing {
        let offset_kind = matches!(kind, OrderKind::TrailingLimit | OrderKind::TrailingTakeProfit);
        return OrderResting {
            kind,
            side,
            size: qty,
            price: None,
            trail: req.trail,
            extreme: req.extreme,
            weight: req.weight,
            stop: None,
            reduce_only: false,
            limit: None,
            release: None,
            activation: req.trigger_price.filter(|_| req.order_type.is_trailing()),
            limit_offset: req.limit_offset.filter(|_| offset_kind),
            all_or_none: false,
            discretionary_amount: None,
            trail_percent: req.trail_percent.filter(|_| req.order_type.is_trailing()),
            adjust: None,
            qid: 0,
        };
    }
    if req.on_close && req.order_type == OrderType::Market {
        let mut o = OrderResting::new(OrderKind::MarketClose, side, qty);
        o.weight = req.weight;
        return o;
    }
    if req.on_close && req.order_type == OrderType::Limit {
        let mut o = OrderResting::new(OrderKind::LimitClose, side, qty);
        o.price = req.price;
        o.weight = req.weight;
        return o;
    }
    if req.order_type == OrderType::Stop {
        let mut o = OrderResting::new(OrderKind::Stop, side, qty);
        o.price = req.trigger_price;
        o.weight = req.weight;
        o.adjust = req.adjust.map(Box::new);
        return o;
    }
    let trigger_kind = match req.order_type {
        OrderType::StopLimit => Some(OrderKind::StopLimit),
        OrderType::TakeProfit => Some(OrderKind::TakeProfit),
        _ => None,
    };
    if let Some(kind) = trigger_kind {
        let mut o = OrderResting::new(kind, side, qty);
        o.price = req.trigger_price;
        o.limit = req.price;
        o.weight = req.weight;
        o.adjust = req.adjust.filter(|_| kind == OrderKind::StopLimit).map(Box::new);
        return o;
    }
    let entry_kind = if req.time_in_force.is_auction()
        && matches!(req.order_type, OrderType::Market | OrderType::Limit)
    {
        Some(if req.time_in_force == TimeInForce::AtOpen {
            OrderKind::AuctionOpen
        } else {
            OrderKind::AuctionClose
        })
    } else if req.order_type == OrderType::MarketToLimit {
        Some(if req.time_in_force == TimeInForce::AtOpen {
            OrderKind::AuctionOpenToLimit
        } else {
            OrderKind::MarketToLimit
        })
    } else {
        None
    };
    if let Some(kind) = entry_kind {
        let mut o = OrderResting::new(kind, side, qty);
        // a market auction order is unpriced whatever `price` it carries
        o.price = if req.order_type == OrderType::Limit { req.price } else { None };
        o.weight = req.weight;
        o.stop = req.stop;
        return o;
    }
    if req.order_type == OrderType::Limit {
        let mut o = OrderResting::new(OrderKind::Limit, side, qty);
        o.price = req.price;
        o.weight = req.weight;
        o.stop = req.stop;
        if let Some(a) = req.attributes {
            o.all_or_none = a.all_or_none;
            o.discretionary_amount = a.discretionary_amount;
        }
        return o;
    }
    // market (default)
    let mut o = OrderResting::new(OrderKind::Market, side, qty);
    o.weight = req.weight;
    o.stop = req.stop;
    o
}

/// A bracket order: an entry plus a protective stop-loss and take-profit that arm when the entry
/// fills, and cancel each other when either exits. RUST-NATIVE (no Python twin).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderBracket {
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
    /// WHICH ACCOUNT of `venue` the bracket is for; `None` = the sender named none (a mounted
    /// strategy's bracket, whose route is its mount's engine, and every spec written before the field).
    ///
    /// Carried onto all three legs by [`build_bracket`], and read by the core's `lower_bracket` exactly
    /// as `lower_submit` reads [`OrderRequest::account`]: a venue that runs two engines refuses an
    /// account-less bracket as ambiguous. Serde is [`OrderRequest::account`]'s pair for the same reason
    /// (`Some(Default)` and `None` route differently), and an account-less spec serialises
    /// BYTE-IDENTICALLY to the pre-field one (it is journaled), while an old record reads back `None`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::accounts::account_keys::wire_account_option"
    )]
    pub account: Option<crate::accounts::account_keys::AccountLabel>,
    /// The sender's group STEM (a v2 id through its unique part,
    /// `crate::orders::client_id::OrderClientIdStem`), so a bracket resent after an unclear reply
    /// names the same three leg ids and is refused whole; `None` = the runtime mints the ids, as
    /// it does for every bracket today. Additive: absent on the wire and in the journal when
    /// `None`. The core's `lower_bracket` derives the three legs from it (`…EN`, `…SL`, `…TP1`, each
    /// with its intent read off the routed book) and refuses the bracket whole, nothing sent, when it
    /// is no v2 stem or a leg of it is in use or remembered from before a restart (decision 0270).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_client_id: Option<String>,
}

/// Build a linked bracket as three [`OrderRequest`]s — `[entry, stop_loss, take_profit]` — filling
/// the reserved contingency slots: the entry is the OTO parent (fills → arms the exits); the two
/// exits are OCO siblings (either fills → cancels the other), reduce-only and opposite-side, sized
/// to the entry.
///
/// STATELESS by design — the vike answer to a stateful order-factory `bracket()`: the three
/// order client ids are MINTED BY THE RUNTIME and passed in, so id minting stays in its one place and this
/// function only wires the linkage. The entry order client id doubles as the shared `order_list_id`.
pub fn build_bracket(
    spec: &OrderBracket,
    entry_order_client_id: &str,
    stop_loss_order_client_id: &str,
    take_profit_order_client_id: &str,
) -> [OrderRequest; 3] {
    let list_id = Some(entry_order_client_id.to_string());
    let exit_side = -spec.side;
    let entry = OrderRequest {
        order_client_id: entry_order_client_id.to_string(),
        venue: spec.venue.clone(),
        symbol: spec.symbol.clone(),
        side: spec.side,
        qty: spec.qty,
        order_type: if spec.entry_price.is_some() { OrderType::Limit } else { OrderType::Market },
        price: spec.entry_price,
        account: spec.account.clone(),
        order_list_id: list_id.clone(),
        contingency_type: Some(ContingencyType::Oto),
        linked_order_ids: vec![
            stop_loss_order_client_id.to_string(),
            take_profit_order_client_id.to_string(),
        ],
        ..Default::default()
    };
    let stop_loss = OrderRequest {
        order_client_id: stop_loss_order_client_id.to_string(),
        venue: spec.venue.clone(),
        symbol: spec.symbol.clone(),
        side: exit_side,
        qty: spec.qty,
        order_type: OrderType::Stop,
        trigger_price: Some(spec.stop_loss),
        account: spec.account.clone(),
        reduce_only: true,
        parent_order_id: Some(entry_order_client_id.to_string()),
        order_list_id: list_id.clone(),
        contingency_type: Some(ContingencyType::Oco),
        linked_order_ids: vec![take_profit_order_client_id.to_string()],
        ..Default::default()
    };
    let take_profit = OrderRequest {
        order_client_id: take_profit_order_client_id.to_string(),
        venue: spec.venue.clone(),
        symbol: spec.symbol.clone(),
        side: exit_side,
        qty: spec.qty,
        order_type: OrderType::Limit,
        price: Some(spec.take_profit),
        account: spec.account.clone(),
        reduce_only: true,
        parent_order_id: Some(entry_order_client_id.to_string()),
        order_list_id: list_id,
        contingency_type: Some(ContingencyType::Oco),
        linked_order_ids: vec![stop_loss_order_client_id.to_string()],
        ..Default::default()
    };
    [entry, stop_loss, take_profit]
}

/// One leg of an [`OrderList`]: an order, and its links to the list's other legs BY INDEX.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderListLeg {
    /// +1 buy / -1 sell
    pub side: i32,
    pub qty: f64,
    pub order_type: OrderType,
    #[serde(default)]
    pub price: Option<f64>,
    #[serde(default)]
    pub trigger_price: Option<f64>,
    /// A trailing leg's distance ([`OrderType::Trailing`], its activation in `trigger_price` as a
    /// trailing order's is), `None` for every other kind. Additive: absent on the wire and in the
    /// journal when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trail: Option<f64>,
    #[serde(default)]
    pub reduce_only: bool,
    #[serde(default)]
    pub time_in_force: TimeInForce,
    /// OTO: the EARLIER leg whose fill arms this one, held off the venue until then; `None` = the leg
    /// is live at once.
    #[serde(default)]
    pub parent: Option<usize>,
    /// How this leg's fill acts on [`Self::linked`]: `Oco` or `Ouo`, set exactly when `linked` is not
    /// empty (`Oto` is [`Self::parent`]'s link, never a sibling's).
    #[serde(default)]
    pub contingency: Option<ContingencyType>,
    /// The legs this leg's fill cancels (`Oco`) or reduces (`Ouo`). Each names this leg back with the
    /// same type, and none is its parent or its child.
    #[serde(default)]
    pub linked: Vec<usize>,
    /// The client's own order id for this leg, so a client that retries a list after an unclear reply
    /// places nothing twice (the runtime refuses a list naming an id it already holds). A list names
    /// every leg's id or none ([`OrderList::client_order_client_ids`]); `None` everywhere = the runtime mints
    /// them. Additive: absent on the wire and in the journal when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_client_id: Option<String>,
}

/// A group of linked orders on ONE symbol of ONE account (decision 0254: every leg of a group on one
/// account): an OCO pair, an OTO chain, an OUO group, a bracket, or a ladder ([`build_ladder`]).
/// RUST-NATIVE. [`build_order_list`] lowers it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderList {
    pub venue: String,
    pub symbol: String,
    pub legs: Vec<OrderListLeg>,
    /// As [`OrderBracket::account`], and carried onto every leg the same way.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::accounts::account_keys::wire_account_option"
    )]
    pub account: Option<crate::accounts::account_keys::AccountLabel>,
}

impl OrderList {
    /// The order ids the list's legs name ([`OrderListLeg::order_client_id`]), in leg order: `Ok(None)`
    /// when no leg names one (the runtime mints them), refused in words when only some do, or when one
    /// is empty or named twice.
    pub fn client_order_client_ids(&self) -> Result<Option<Vec<String>>, String> {
        let named: Vec<&str> =
            self.legs.iter().filter_map(|l| l.order_client_id.as_deref()).collect();
        if named.is_empty() {
            return Ok(None);
        }
        if named.len() != self.legs.len() {
            return Err(format!(
                "an order list names every leg's order id or none: {} of its {} legs name one",
                named.len(),
                self.legs.len()
            ));
        }
        if let Some(i) = named.iter().position(|c| c.is_empty()) {
            return Err(format!("leg {i}'s order id is empty"));
        }
        if let Some((i, c)) = named.iter().enumerate().find(|(i, c)| named[..*i].contains(c)) {
            return Err(format!("leg {i}'s order id {c} is named twice"));
        }
        Ok(Some(named.into_iter().map(str::to_string).collect()))
    }
}

/// Build an [`OrderList`] as one [`OrderRequest`] per leg, in leg order, filling the contingency
/// slots as [`build_bracket`] does: a leg's `linked_order_ids` are its children (the legs whose
/// `parent` it is), then its [`OrderListLeg::linked`] legs; its `contingency_type` is its
/// [`OrderListLeg::contingency`], or `Oto` for a leg with children and no link; the first leg's order client id is
/// the shared `order_list_id`. A list shaped as a bracket builds [`build_bracket`]'s requests
/// (`an_order_list_shaped_as_a_bracket_builds_the_bracket`).
///
/// STATELESS like [`build_bracket`]: the order client ids, one per leg, are MINTED BY THE RUNTIME, or are the
/// ones the legs name ([`OrderList::client_order_client_ids`]), which `order_client_ids` must then be. FALLIBLE: a list
/// whose links the contingency book cannot resolve, or whose legs name only some ids, is refused in
/// words before anything is built. The terms of each order are its own question
/// (`order_terms_refusal`, the venue's capability row), not this one's.
pub fn build_order_list(
    spec: &OrderList,
    order_client_ids: &[String],
) -> Result<Vec<OrderRequest>, String> {
    let legs = &spec.legs;
    if legs.len() < 2 {
        return Err(format!("an order list needs two legs or more, it has {}", legs.len()));
    }
    if order_client_ids.len() != legs.len() {
        return Err(format!("{} order ids for {} legs", order_client_ids.len(), legs.len()));
    }
    if spec.client_order_client_ids()?.is_some_and(|named| named != order_client_ids) {
        return Err("the order ids are not the ones the list's legs name".to_string());
    }
    let child_of = |child: usize, parent: usize| legs[child].parent == Some(parent);
    let has_child = |i: usize| (0..legs.len()).any(|c| child_of(c, i));
    for (i, leg) in legs.iter().enumerate() {
        if leg.parent.is_some_and(|p| p >= i) {
            return Err(format!("leg {i}'s parent is not an earlier leg"));
        }
        match (leg.contingency, leg.linked.is_empty()) {
            (None, true) | (Some(ContingencyType::Oco | ContingencyType::Ouo), false) => {}
            _ => return Err(format!("leg {i} needs OCO or OUO exactly when it links other legs")),
        }
        for (n, &j) in leg.linked.iter().enumerate() {
            if j >= legs.len()
                || j == i
                || child_of(i, j)
                || child_of(j, i)
                || leg.linked[..n].contains(&j)
            {
                return Err(format!("leg {i} cannot link leg {j}"));
            }
            if !legs[j].linked.contains(&i) || legs[j].contingency != leg.contingency {
                return Err(format!("leg {j} does not link leg {i} back with the same type"));
            }
        }
        if leg.parent.is_none() && leg.linked.is_empty() && !has_child(i) {
            return Err(format!("leg {i} is linked to nothing"));
        }
    }
    let list_id = Some(order_client_ids[0].clone());
    Ok(legs
        .iter()
        .enumerate()
        .map(|(i, leg)| {
            let children: Vec<usize> = (0..legs.len()).filter(|&c| child_of(c, i)).collect();
            OrderRequest {
                order_client_id: order_client_ids[i].clone(),
                venue: spec.venue.clone(),
                symbol: spec.symbol.clone(),
                side: leg.side,
                qty: leg.qty,
                order_type: leg.order_type.clone(),
                price: leg.price,
                trigger_price: leg.trigger_price,
                trail: leg.trail,
                reduce_only: leg.reduce_only,
                time_in_force: leg.time_in_force,
                account: spec.account.clone(),
                parent_order_id: leg.parent.map(|p| order_client_ids[p].clone()),
                order_list_id: list_id.clone(),
                contingency_type: leg
                    .contingency
                    .or((!children.is_empty()).then_some(ContingencyType::Oto)),
                linked_order_ids: children
                    .iter()
                    .chain(&leg.linked)
                    .map(|&j| order_client_ids[j].clone())
                    .collect(),
                ..Default::default()
            }
        })
        .collect())
}

/// One partial take-profit of a [`LadderSpec`]: a limit exit of `qty` at `price`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LadderTarget {
    pub price: f64,
    pub qty: f64,
}

/// A bracket ladder: an entry, ONE protective stop for its whole size, and N partial take-profits.
/// RUST-NATIVE. [`build_ladder`] lowers it to the [`OrderList`] it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LadderSpec {
    pub venue: String,
    pub symbol: String,
    /// entry side: +1 long / -1 short (every exit is the opposite side, reduce-only)
    pub side: i32,
    /// the entry's size, and the stop's
    pub qty: f64,
    /// entry order: `None` = market entry, `Some(px)` = limit entry
    pub entry_price: Option<f64>,
    /// the stop's trigger price
    pub stop_loss: f64,
    /// the partial take-profits, one or more, whose sizes sum to no more than `qty`
    pub targets: Vec<LadderTarget>,
    /// As [`OrderBracket::account`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::accounts::account_keys::wire_account_option"
    )]
    pub account: Option<crate::accounts::account_keys::AccountLabel>,
}

/// The [`OrderList`] a [`LadderSpec`] is: leg 0 the entry, the OTO parent of every exit; leg 1 the
/// stop for the whole size; legs 2.. the targets in order. The stop and each target are linked OUO, so
/// a target's fill shrinks the stop by what it filled and the stop's fill shrinks every target by its
/// own; targets are not linked to one another. STATELESS: it builds no request, so it mints no id
/// ([`build_order_list`] takes the runtime's). Refused in words: no target, a target size that is not a
/// finite number above zero, targets that sum past the entry's size.
pub fn build_ladder(spec: &LadderSpec) -> Result<OrderList, String> {
    if spec.targets.is_empty() {
        return Err("a ladder needs one take-profit target or more".to_string());
    }
    if let Some((i, t)) =
        spec.targets.iter().enumerate().find(|(_, t)| !(t.qty.is_finite() && t.qty > 0.0))
    {
        return Err(format!("target {i}'s size {} is not a finite number above zero", t.qty));
    }
    let total: f64 = spec.targets.iter().map(|t| t.qty).sum();
    // relative slack: target sizes that add up to the entry's in decimals carry a float residue
    if total > spec.qty * (1.0 + 1e-9) {
        return Err(format!("the targets sum to {total}, past the entry's size {}", spec.qty));
    }
    let exit = |order_type, price, trigger_price, qty, linked| OrderListLeg {
        side: -spec.side,
        qty,
        order_type,
        price,
        trigger_price,
        trail: None,
        reduce_only: true,
        time_in_force: TimeInForce::default(),
        parent: Some(0),
        contingency: Some(ContingencyType::Ouo),
        linked,
        order_client_id: None,
    };
    let mut legs = vec![
        OrderListLeg {
            side: spec.side,
            qty: spec.qty,
            order_type: if spec.entry_price.is_some() {
                OrderType::Limit
            } else {
                OrderType::Market
            },
            price: spec.entry_price,
            trigger_price: None,
            trail: None,
            reduce_only: false,
            time_in_force: TimeInForce::default(),
            parent: None,
            contingency: None,
            linked: Vec::new(),
            order_client_id: None,
        },
        exit(
            OrderType::Stop,
            None,
            Some(spec.stop_loss),
            spec.qty,
            (2..spec.targets.len() + 2).collect(),
        ),
    ];
    legs.extend(
        spec.targets.iter().map(|t| exit(OrderType::Limit, Some(t.price), None, t.qty, vec![1])),
    );
    Ok(OrderList {
        venue: spec.venue.clone(),
        symbol: spec.symbol.clone(),
        legs,
        account: spec.account.clone(),
    })
}

/// The per-leg crossing law of a combo priced by its legs ([`OrderComboLeg::limit`]; LEAN's
/// `ComboLegLimitFill`), the ONE law the simulator and the paper book share: `Some(prices)`, one per
/// leg in leg order, when EVERY leg crosses its own limit at the same moment, `None` otherwise (all or
/// nothing). `px(symbol, want_ask)` is [`combo_net_from_legs`]'s: the ASK of a leg the combo buys
/// (`side × ratio > 0`), which crosses at or below its limit, the BID of a leg it sells, which crosses
/// at or above it. A leg fills at that price, at or better than its limit (a marketable limit's touch).
/// A leg with no limit, a side that is not ±1, or a price or limit that is not finite crosses nothing.
pub fn combo_legs_cross(
    side: i32,
    legs: &[OrderComboLeg],
    px: impl Fn(&str, bool) -> f64,
) -> Option<Vec<f64>> {
    if legs.is_empty() || (side != 1 && side != -1) {
        return None;
    }
    legs.iter()
        .map(|leg| {
            let limit = leg.limit.filter(|l| l.is_finite())?;
            let buys = side * leg.ratio.signum() > 0;
            let p = px(&leg.symbol, buys);
            let crosses = if buys { p <= limit } else { p >= limit };
            (p.is_finite() && crosses).then_some(p)
        })
        .collect()
}

/// An instruction to EXERCISE `qty` contracts of a held option (`symbol`) on ONE account of `venue`
/// (decision 0254): not an order, so it has no price, no lifetime and no order id. RUST-NATIVE. The
/// simulator exercises an American option whose underlying the run carries; every venue refuses one
/// by name (decision 0234 keeps IBKR's option design closed, deribit's options are European and
/// exercise themselves at expiry, alpaca is deferred). [`ExerciseSpec::validate`] holds its terms.
///
/// ⚠ `deny_unknown_fields`: a term added later (IB's `exerciseAction` lapse, an override) is REFUSED
/// by a build that does not know it, never dropped so the option is exercised without it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExerciseSpec {
    pub venue: String,
    /// the option's instrument
    pub symbol: String,
    /// contracts to exercise, finite and above zero (whether the account holds that many long is
    /// the book's question)
    pub qty: f64,
    /// As [`OrderBracket::account`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::accounts::account_keys::wire_account_option"
    )]
    pub account: Option<crate::accounts::account_keys::AccountLabel>,
}

impl ExerciseSpec {
    /// Why this exercise cannot be asked for at all, in words: no option named, or a quantity that is
    /// not a finite number above zero. Whether the account holds the option, and whether its venue or
    /// the simulator exercises one, is the asker's question.
    pub fn validate(&self) -> Result<(), String> {
        if self.symbol.is_empty() {
            return Err(
                "an exercise names the option it exercises; this one names none".to_string()
            );
        }
        if !(self.qty.is_finite() && self.qty > 0.0) {
            return Err(format!(
                "an exercise of {} contracts of {}: the quantity is not a finite number above zero",
                self.qty, self.symbol
            ));
        }
        Ok(())
    }
}

#[path = "order_tests.rs"]
#[cfg(test)]
mod order_tests;
