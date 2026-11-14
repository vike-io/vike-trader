//! `AmendMode` — how a venue CHANGES a resting order: in place at the venue (`Native`), only by
//! cancelling it and placing a new one (`CancelThenNew`), or not at all (`Refused`). One answer per
//! venue per resting KIND (limit, stop, stop-limit, take-profit, trailing, trailing-limit, trailing take-profit), read by the ONE amend state machine
//! above the bridges (`crates/vike-core/src/runtime/apply/amend.rs`'s module doc) so the operator, the
//! GUI and a strategy see one logical "modify" whatever the venue does underneath.
//!
//! It is the sibling of [`crate::venue_amend`] and answers a different question. `AmendSemantics` says
//! what an amend's QUANTITY means once an amend exists; this says whether one exists for THIS KIND of
//! order, and if not, what takes its place.
//!
//! # DERIVED, not a second per-venue table
//!
//! The answer is computed from two facts a venue row already declares, plus a short exception list:
//!
//! * a kind outside `VenueCapabilities::supported_order_kinds` cannot be placed, so it cannot be replaced
//!   either: `Refused`;
//! * `VenueCapabilities::supports_modify` says the adapter wires a native amend: `Native`, for every kind it
//!   lists, because that adapter's own `modify` answers (an amend it cannot do is a non-terminal
//!   `OrderModifyRejected` in its words, never silence);
//! * a venue with no native amend places and cancels all the same (`submit` and `cancel` are the two
//!   REQUIRED methods of `ExecutionClient`), so the replacement is available everywhere a kind can be
//!   placed: `CancelThenNew`.
//!
//! The exceptions are the pairs where `supports_modify` is true for the VENUE but the adapter is
//! known not to move that KIND, or not to move its TRIGGER (`NO_NATIVE_KIND`), and the venues that are
//! refused whatever (`REFUSED_VENUES`). Each carries its reason, and `venue_amend_mode_tests` pins the
//! rule against the two rows it reads, so a row that moves breaks a test rather than this answer silently.
//!
//! # What a modify moves, per kind
//!
//! `new_price` is a `stop`'s TRIGGER (`vike_model::modified_price_is_trigger`) and every other kind's
//! limit price; [`amend_mode`](crate::venue_amend_mode::amend_mode) answers that move. `new_trigger_price` (`vike_execution::OrderIntent::Modify`,
//! landed by `vike_model::modified_prices`) moves the TRIGGER of a stop-limit or a take-profit, and
//! [`trigger_amend_mode`](crate::venue_amend_mode::trigger_amend_mode) answers that one. In place it is the client's `modify_trigger`
//! (`vike_execution::ExecutionClient::modify_trigger`, whose default refuses in words); by cancel-then-new the
//! replacement is placed at the new trigger (`vike_execution::Engine::plan_replacement`). An adapter
//! that amends the kind in place but overrides no `modify_trigger` is a `Moves::Trigger` row, and its
//! trigger move is REFUSED in words, not turned into a cancel-then-new: decision 0244 (item 8) leaves a
//! stop-limit's and a take-profit's trigger to the adapter's own answer, and on cTrader a new-first
//! replacement of an exit is impossible (its venue refuses a second reduce-only order on a position).
//! `vike_execution::Engine::trigger_amend_mode_of` reads this answer for a modify that names a trigger,
//! and the core refuses in words a trigger move it answers `Refused`. A stop's trigger is its one price,
//! so its two answers are one; a limit has no trigger.
//!
//! # `Native` is the PRICE's answer; a SIZE is the adapter's
//!
//! `Native` says the adapter's own `modify` is sent and its answer settles the amend: `OrderModified` for what moved, a
//! non-terminal `OrderModifyRejected` in its words for what did not. The price (a stop's trigger) moves in place on
//! every `Native` cell. A SIZE does not on two venues whose exits belong to a whole trade or order (both MEASURED on
//! their demo accounts, 2026-11-09):
//!
//! * fxcm, a bound stop or take-profit (`crates/bridges/fxcm/src/execution/exit/group.rs`'s `ExitGroup`): the size moves in WHOLE
//!   trades, by venue orders the bridge adds (a new order on the oldest free trade) or deletes, never by an edit of one,
//!   and a size inside a trade is refused in words (`ORA-20134`). A resting entry limit's size is refused
//!   (`crates/bridges/fxcm/src/execution/event_mapper.rs`'s `plan_amend`).
//! * dukascopy, a stop-loss or a take-profit, a price on the whole JForex order
//!   (`crates/bridges/dukascopy/jforex-bridge/src/main/java/vike/jforex/StrategyBridge.java`'s `handleStopModify`): the
//!   size is refused in words ("a stop's size cannot be amended: cancel it and place a new stop").
//!
//! There is no size axis here because no consumer acts wrongly on these cells. `crates/vike-core/src/runtime/apply/amend.rs`
//! sends the modify and publishes the adapter's answer (a cancel-then-new would do worse: fxcm refuses a second stop on a
//! trade, and a dukascopy stop is a price on the order, not an order). The two that read `Native` as an in-place SHRINK,
//! [`crate::venue_caps::order_list_refusal`] and `crates/vike-core/src/runtime/apply/contingency.rs`'s `send_ouo_size`,
//! never meet these venues' exits: both venues are rows of [`crate::venue_caps::WHOLE_EXIT_VENUES`], so a list with an OUO leg
//! there is refused whole before its entry. A venue leaves that table only with a size answer here first.
//!
//! A TRAILING kind (`trailing`, `trailing_limit`, `trailing_take_profit`, each its own [`AmendKind`](crate::venue_amend_mode::AmendKind)) is never modified at a
//! venue: no adapter moves a resting trailing order in place, and a cancel-then-new would restart its
//! trail from the price at placement, so the stop could step back. [`amend_mode`](crate::venue_amend_mode::amend_mode) and
//! [`trigger_amend_mode`](crate::venue_amend_mode::trigger_amend_mode) are `Refused` for all three on every venue, and the engine refuses the modify in
//! words (`vike_execution::execution_engine::TRAILING_AMEND_REASON`). A client that declares its own amend
//! convention (the paper book) is answered by the engine before this table and moves the size in place.

use vike_model::OrderType;

/// The resting kinds an amend can name. The word is the `OrderType::as_str` word of the kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AmendKind {
    /// a resting priced order
    Limit,
    /// a resting stop-loss trigger: the amend's price is the TRIGGER
    Stop,
    /// a stop trigger whose fill is a limit
    StopLimit,
    /// a take-profit trigger
    TakeProfit,
    /// a trailing stop: a modify moves its size only (the module doc)
    Trailing,
    /// a trailing stop whose fill is a limit at its offset from the stop
    TrailingLimit,
    /// a trailing if-touched order (IB `TRAIL MIT`, `TRAIL LIT` with a limit offset): it trails the
    /// market on the profit side and fires on a move back to its trigger
    TrailingTakeProfit,
}

impl AmendKind {
    /// Every kind, in declaration order.
    pub const ALL: [AmendKind; 7] = [
        AmendKind::Limit,
        AmendKind::Stop,
        AmendKind::StopLimit,
        AmendKind::TakeProfit,
        AmendKind::Trailing,
        AmendKind::TrailingLimit,
        AmendKind::TrailingTakeProfit,
    ];

    /// The kind an order type rests as, or `None` for a kind that does not rest (`market`, or an
    /// unrecognised word). A market-to-limit rests only as the LIMIT its remainder becomes at its first
    /// fill's price (the law; ibkr's `MTL` and deribit's `market_limit` at the venue), so a change of it
    /// moves that limit and it is a [`AmendKind::Limit`]. A venue whose row lists it must amend a limit
    /// natively: a cancel-then-new replacement keeps the order's kind and would execute at market again
    /// (`venue_amend_mode_tests`' `a_market_to_limit_rests_as_its_limit_and_is_never_placed_again`).
    #[must_use]
    pub fn of(order_type: &OrderType) -> Option<AmendKind> {
        match order_type {
            OrderType::Limit | OrderType::MarketToLimit => Some(AmendKind::Limit),
            OrderType::Stop => Some(AmendKind::Stop),
            OrderType::StopLimit => Some(AmendKind::StopLimit),
            OrderType::TakeProfit => Some(AmendKind::TakeProfit),
            OrderType::Trailing => Some(AmendKind::Trailing),
            OrderType::TrailingLimit => Some(AmendKind::TrailingLimit),
            OrderType::TrailingTakeProfit => Some(AmendKind::TrailingTakeProfit),
            // a pegged order rests, but its price is the venue's to move: no amend kind (an amend
            // could change its size only, and no row lists the kind yet)
            OrderType::Market | OrderType::Pegged | OrderType::Unrecognized(_) => None,
        }
    }

    /// The `OrderType::as_str` word (the spelling `VenueCapabilities::supported_order_kinds` uses).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            AmendKind::Limit => "limit",
            AmendKind::Stop => "stop",
            AmendKind::StopLimit => "stop_limit",
            AmendKind::TakeProfit => "take_profit",
            AmendKind::Trailing => "trailing",
            AmendKind::TrailingLimit => "trailing_limit",
            AmendKind::TrailingTakeProfit => "trailing_take_profit",
        }
    }
}

/// How a venue changes a resting order of one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AmendMode {
    /// The adapter amends it at the venue: `ExecutionClient::modify` is sent and the venue's own
    /// answer (`OrderModified` or a non-terminal `OrderModifyRejected`) settles it. In place for the
    /// PRICE; a size is what the adapter can do, in its words (the module doc's fxcm and dukascopy).
    Native,
    /// The venue cannot change it in place. The node cancels the order and places a new one with the
    /// changed terms, under a new order client id linked to the old one
    /// (`vike_model::OrderRecord::replaces`). A reduce-only stop is placed FIRST and the old one is
    /// cancelled once the new one is acknowledged; anything else is cancelled first.
    CancelThenNew,
    /// No change is possible: the modify is refused in words and the order is untouched.
    Refused,
}

/// What an exception row says the adapter does not move in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Moves {
    /// The order at all: its `new_qty` / `new_price` (so its trigger neither).
    Price,
    /// Only the TRIGGER of a stop-limit or a take-profit (`new_trigger_price`); its limit moves in place.
    Trigger,
}

/// Pairs where the VENUE amends (`supports_modify`) but its adapter does not move THIS kind
/// ([`Moves::Price`]), or moves it but not its trigger ([`Moves::Trigger`]). Each is read from the
/// adapter, and the adapter's own page names it. (Bullets and rows are laid out so that one venue's
/// flip edits lines no other venue's flip touches, and each applies or reverts alone.)
///
/// * binance: a perpetual stop, stop-limit and take-profit are Algo Service orders, "modification of
///   untriggered conditional orders is not supported" (`crates/bridges/binance/src/execution/rest/perp.rs`'s
///   `VenueRestBinancePerp::modify_order` answers a non-terminal advisory, `algo.rs`'s `MODIFY_REFUSAL`, and
///   sends nothing; spot amends a `LIMIT` only).
///
/// * aster: `PUT /fapi/v3/order` is LIMIT-only (`crates/bridges/aster/src/execution/rest/perp.rs`'s `modify_order`).
/// * oanda: the replace (`PUT /orders/@{order_client_id}`) rebuilds a resting limit or stop only, and a
///   market-if-touched take-profit's modify answers `OrderModifyRejected`
///   (`crates/bridges/oanda/src/execution/mod.rs`'s `build_replace_body_on`).
///
/// * the TRIGGER of every other natively amended stop-limit and take-profit: its adapter overrides no
///   `vike_execution::ExecutionClient::modify_trigger`, so the trigger move is refused in words (decision
///   0244, item 8: those two kinds' triggers are the adapter's own answer, not a cancel-then-new). A
///   bridge that sends one in place drops its row.
///
/// * bybit: no row. A stop is amended in place (its one price goes out as the amend's `triggerPrice`,
///   `crates/bridges/bybit/src/execution/rest/mod.rs`'s `VenueRestBybit::amend`). A stop-limit moves its limit in
///   place (the seam's one price is the `price` the amend carries; `bybit_stop_limit_smoke`), and a
///   stop-limit's and a take-profit's trigger move in place (`triggerPrice` on the same endpoint,
///   `modify_trigger`); both proven on the demo host by `bybit_trigger_amend_smoke`, 2026-11-08.
const NO_NATIVE_KIND: &[(&str, AmendKind, Moves)] = &[
    ("binance", AmendKind::Stop, Moves::Price),
    ("binance", AmendKind::StopLimit, Moves::Price),
    ("binance", AmendKind::TakeProfit, Moves::Price),
    ("aster", AmendKind::Stop, Moves::Price),
    ("oanda", AmendKind::TakeProfit, Moves::Price),
    ("deribit", AmendKind::StopLimit, Moves::Trigger),
    ("deribit", AmendKind::TakeProfit, Moves::Trigger),
    ("hyperliquid", AmendKind::StopLimit, Moves::Trigger),
    ("hyperliquid", AmendKind::TakeProfit, Moves::Trigger),
    ("ctrader", AmendKind::StopLimit, Moves::Trigger),
    ("ibkr", AmendKind::StopLimit, Moves::Trigger),
];

/// Venues refused whatever the kind: alpaca is outside the order-modify wave by the owner's word
/// (`crates/bridges/alpaca/CLAUDE.md`), so a cancel-and-replace there would be new behaviour on a
/// venue nobody is to touch.
const REFUSED_VENUES: &[&str] = &["alpaca"];

/// How `venue` changes a resting order of `kind`. An unrecognised venue has no kinds, so it is
/// [`AmendMode::Refused`] (fail closed). A trailing kind is [`AmendMode::Refused`] on every venue (the
/// module doc: a cancel-then-new would restart its trail).
#[must_use]
pub fn amend_mode(venue: &str, kind: AmendKind) -> AmendMode {
    let capabilities = crate::capabilities_for(venue);
    if matches!(
        kind,
        AmendKind::Trailing | AmendKind::TrailingLimit | AmendKind::TrailingTakeProfit
    ) || REFUSED_VENUES.contains(&venue)
        || !capabilities.supported_order_kinds.contains(&kind.word())
    {
        return AmendMode::Refused;
    }
    if capabilities.supports_modify && !NO_NATIVE_KIND.contains(&(venue, kind, Moves::Price)) {
        AmendMode::Native
    } else {
        AmendMode::CancelThenNew
    }
}

/// How `venue` moves the TRIGGER of a resting order of `kind`: a modify that names
/// `new_trigger_price`. A stop's trigger is its one price, so it answers as [`amend_mode`]; a limit has
/// no trigger and is [`AmendMode::Refused`] (the doors refuse a trigger price on it first). A stop-limit
/// or a take-profit is [`amend_mode`]'s answer, except that an adapter which amends it in place but does
/// not move its trigger REFUSES the trigger move ([`AmendMode::Refused`], in its words): decision 0244
/// leaves those two kinds' triggers to the adapter, so no cancel-then-new is made of them. A trailing
/// kind's trigger is its activation, which no modify moves: [`AmendMode::Refused`].
#[must_use]
pub fn trigger_amend_mode(venue: &str, kind: AmendKind) -> AmendMode {
    match (kind, amend_mode(venue, kind)) {
        (
            AmendKind::Limit
            | AmendKind::Trailing
            | AmendKind::TrailingLimit
            | AmendKind::TrailingTakeProfit,
            _,
        ) => AmendMode::Refused,
        (_, AmendMode::Native) if NO_NATIVE_KIND.contains(&(venue, kind, Moves::Trigger)) => {
            AmendMode::Refused
        }
        (_, mode) => mode,
    }
}

/// The mode for an ORDER TYPE: `None` for a type that does not rest.
#[must_use]
pub fn amend_mode_for(venue: &str, order_type: &OrderType) -> Option<AmendMode> {
    AmendKind::of(order_type).map(|kind| amend_mode(venue, kind))
}

/// What a modify of a `kind` order does on a venue whose mode is `mode`, in the words an operator
/// reads (the control door's acknowledgement, `vike-cli trade order modify --help`, the node's note).
/// `reduce_only` picks the stop wording: a reduce-only stop is placed first, so it has no gap.
#[must_use]
pub fn amend_words(mode: AmendMode, kind: AmendKind, reduce_only: bool) -> &'static str {
    match (mode, kind, reduce_only) {
        (AmendMode::Native, _, _) => {
            "changed in place at the venue (a size it cannot change in place is answered in the \
             adapter's own words)"
        }
        (AmendMode::CancelThenNew, AmendKind::Limit, _) => {
            "this venue cannot change a resting limit order in place: it is CANCELLED and a new one \
             placed, so the order LOSES ITS PLACE IN THE QUEUE and gets a new order client id (linked \
             to the old one)"
        }
        (AmendMode::CancelThenNew, _, true) => {
            "this venue cannot move a resting stop in place: the NEW stop is placed first and the old \
             one is cancelled once the new one is acknowledged, so the position is never without a \
             stop (the order gets a new order client id, linked to the old one)"
        }
        (AmendMode::CancelThenNew, _, false) => {
            "this venue cannot change a resting trigger order in place: it is CANCELLED and a new one \
             placed (there is a moment with neither), under a new order client id linked to the old one"
        }
        (AmendMode::Refused, _, _) => {
            "this venue cannot change this kind of resting order: cancel it and place a new one"
        }
    }
}

#[path = "venue_amend_mode_tests.rs"]
#[cfg(test)]
mod venue_amend_mode_tests;
