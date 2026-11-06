//! `AmendMode` — how a venue CHANGES a resting order: in place at the venue (`Native`), only by
//! cancelling it and placing a new one (`CancelThenNew`), or not at all (`Refused`). One answer per
//! venue per resting KIND (limit, stop, stop-limit, take-profit), read by the ONE amend state machine
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
//! * a kind outside `VenueCaps::supported_order_kinds` cannot be placed, so it cannot be replaced
//!   either: `Refused`;
//! * `VenueCaps::supports_modify` says the adapter wires a native amend: `Native`, for every kind it
//!   lists, because that adapter's own `modify` answers (an amend it cannot do is a non-terminal
//!   `OrderModifyRejected` in its words, never silence);
//! * a venue with no native amend places and cancels all the same (`submit` and `cancel` are the two
//!   REQUIRED methods of `ExecutionClient`), so the replacement is available everywhere a kind can be
//!   placed: `CancelThenNew`.
//!
//! The exceptions are the pairs where `supports_modify` is true for the VENUE but the adapter is
//! known not to move that KIND (`NO_NATIVE_KIND`), and the venues that are refused whatever
//! (`REFUSED_VENUES`). Each carries its reason, and `venue_amend_mode_tests` pins the rule against
//! the two rows it reads, so a row that moves breaks a test rather than this answer silently.
//!
//! # What `new_price` means, per kind
//!
//! The modify seam carries one price. For a `stop` it is the TRIGGER (`vike_model::modified_price_is_trigger`);
//! for a `limit` it is the limit price. A `stop_limit` and a `take_profit` carry two prices or only a
//! trigger and the seam cannot say which one moves, so their rows stay what they were (the adapter's
//! own advisory answers) and are not part of the unification: see the decision record.

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
}

impl AmendKind {
    /// Every kind, in declaration order.
    pub const ALL: [AmendKind; 4] =
        [AmendKind::Limit, AmendKind::Stop, AmendKind::StopLimit, AmendKind::TakeProfit];

    /// The kind an order type rests as, or `None` for a kind that does not rest (`market`, or a word
    /// outside the five).
    #[must_use]
    pub fn of(order_type: &OrderType) -> Option<AmendKind> {
        match order_type {
            OrderType::Limit => Some(AmendKind::Limit),
            OrderType::Stop => Some(AmendKind::Stop),
            OrderType::StopLimit => Some(AmendKind::StopLimit),
            OrderType::TakeProfit => Some(AmendKind::TakeProfit),
            OrderType::Market | OrderType::Unrecognized(_) => None,
        }
    }

    /// The `OrderType::as_str` word (the spelling `VenueCaps::supported_order_kinds` uses).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            AmendKind::Limit => "limit",
            AmendKind::Stop => "stop",
            AmendKind::StopLimit => "stop_limit",
            AmendKind::TakeProfit => "take_profit",
        }
    }
}

/// How a venue changes a resting order of one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AmendMode {
    /// The adapter amends it at the venue: `ExecutionClient::modify` is sent and the venue's own
    /// answer (`OrderModified` or a non-terminal `OrderModifyRejected`) settles it.
    Native,
    /// The venue cannot change it in place. The node cancels the order and places a new one with the
    /// changed terms, under a new client order id linked to the old one
    /// (`vike_model::ManagedOrder::replaces`). A reduce-only stop is placed FIRST and the old one is
    /// cancelled once the new one is acknowledged; anything else is cancelled first.
    CancelThenNew,
    /// No change is possible: the modify is refused in words and the order is untouched.
    Refused,
}

/// Pairs where the VENUE amends (`supports_modify`) but its adapter does not move THIS kind. Each is
/// read from the adapter, and the adapter's own page names it.
///
/// * binance: a perpetual stop is an Algo Service order, "modification of untriggered conditional
///   orders is not supported" (`crates/bridges/binance/src/perp.rs`'s `BinancePerpRest::modify_order`
///   answers a non-terminal advisory and sends nothing).
/// * aster: `PUT /fapi/v3/order` is LIMIT-only (`crates/bridges/aster/src/perp.rs`'s `modify_order`).
/// * okx: a stop rests on the algo endpoint, which `amend-order` does not reach
///   (`crates/bridges/okx/src/perp.rs`'s `modify_order` sends `amend-order` by `clOrdId` only).
/// * bybit: `modify_order` carries `qty` and `price` and no `triggerPrice`, so a conditional order's
///   trigger cannot move (`crates/bridges/bybit/src/perp.rs`'s `modify_order`).
const NO_NATIVE_KIND: &[(&str, AmendKind)] = &[
    ("binance", AmendKind::Stop),
    ("aster", AmendKind::Stop),
    ("okx", AmendKind::Stop),
    ("bybit", AmendKind::Stop),
];

/// Venues refused whatever the kind: alpaca is outside the order-modify wave by the owner's word
/// (`crates/bridges/alpaca/CLAUDE.md`), so a cancel-and-replace there would be new behaviour on a
/// venue nobody is to touch.
const REFUSED_VENUES: &[&str] = &["alpaca"];

/// How `venue` changes a resting order of `kind`. An unrecognised venue has no kinds, so it is
/// [`AmendMode::Refused`] (fail closed).
#[must_use]
pub fn amend_mode(venue: &str, kind: AmendKind) -> AmendMode {
    let caps = crate::caps_for(venue);
    if REFUSED_VENUES.contains(&venue) || !caps.supported_order_kinds.contains(&kind.word()) {
        return AmendMode::Refused;
    }
    if caps.supports_modify && !NO_NATIVE_KIND.contains(&(venue, kind)) {
        AmendMode::Native
    } else {
        AmendMode::CancelThenNew
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
        (AmendMode::Native, _, _) => "changed in place at the venue",
        (AmendMode::CancelThenNew, AmendKind::Limit, _) => {
            "this venue cannot change a resting limit order in place: it is CANCELLED and a new one \
             placed, so the order LOSES ITS PLACE IN THE QUEUE and gets a new client order id (linked \
             to the old one)"
        }
        (AmendMode::CancelThenNew, _, true) => {
            "this venue cannot move a resting stop in place: the NEW stop is placed first and the old \
             one is cancelled once the new one is acknowledged, so the position is never without a \
             stop (the order gets a new client order id, linked to the old one)"
        }
        (AmendMode::CancelThenNew, _, false) => {
            "this venue cannot change a resting trigger order in place: it is CANCELLED and a new one \
             placed (there is a moment with neither), under a new client order id linked to the old one"
        }
        (AmendMode::Refused, _, _) => {
            "this venue cannot change this kind of resting order: cancel it and place a new one"
        }
    }
}

#[path = "venue_amend_mode_tests.rs"]
#[cfg(test)]
mod venue_amend_mode_tests;
