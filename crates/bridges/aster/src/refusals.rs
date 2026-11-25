//! aster's refusals, declared once (`vike_utility::refusal_enum!`, decision 0301): each has its id and its ONE text,
//! and code and tests decide on the variant, never on the words. Every id is published in the workspace's
//! `refusals.json` (`crates/vike-backend/tests/refusal_catalog.rs`).
//!
//! A refusal leaves this crate as its text where the shared seam carries a `String` (the data-client spec's `plan`,
//! [`crate::feed::tick::spawn_aster_feed_tick`]): its `Display`, byte for byte the words it had before it was typed.
//! The family's refusals (a spot name asked for funding, an order on the other lane) are binance's
//! (`vike_binance::family`), and an order's refusal is step 2's `RejectCause`: neither is here.

use vike_venues::PERP_SUFFIX;

vike_utility::refusal_enum! {
    /// Why aster's market-data feed refuses a subscription before any lane spawns (`crate::feed`'s
    /// `DataClientSpecAster`, `crate::feed::tick`'s `pump_wire`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum AsterFeedRefusal with ids AsterFeedRefusalId {
        /// A lane aster streams nothing for (quotes, the lossless book); `lane` is its word.
        NoLane { lane: String } => "aster.feed.no_lane": "aster serves no {lane} lane",
        /// A bare (spot) name asked of the tick pump, which streams the perpetual's book only.
        SpotTickPump { symbol: String } => "aster.feed.spot_tick_pump":
            "aster's tick pump streams the USDⓈ-M perpetual book only, and {symbol:?} is the spot pair: it would \
             carry the perpetual's prices under the spot's name. The perpetual is {symbol}{PERP_SUFFIX}.",
    }
}
