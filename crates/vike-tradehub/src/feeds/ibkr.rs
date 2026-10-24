//! ibkr's PRICE-ONLY feed arm (any-symbol price lane, decision 0174): the strategy-lane dial with NOTHING
//! subscribed and a [`PriceTap::real_time_only`] in front of the core's sink.
//!
//! ibkr is never a mount (no profile names it, `FEED_ARMS` has no row for it), so unlike the other price-only
//! venues' twins there is no mounted `wire_ibkr_feed` beside this one. The control door opens a stock's QUOTE lane
//! through the feed's [`QuoteFeed::watcher`] when an order names a stock the engine was not started on, and reads
//! its price back through the same handle:
//!
//! * **LAZY, end to end.** Wiring subscribes nothing. The datahub's strategy lane opens a credentialed stream for
//!   the dial (`Scope::Write`, `cs_venue=ibkr`), and the gatewayhub's end of it
//!   (`crates/vike-gatewayhub/src/data/credentialed.rs`'s `build_credentialed`) is built with nothing dialled: the
//!   IB Gateway is dialled by the first symbol the door asks a price for, through the gate's data lane.
//! * **FAIL-CLOSED.** A refused dial (no strategy-lane key, a datahub that advertises no ibkr lane: the probe in
//!   `wire_price_only_feeds`) leaves no watcher, so the door answers `this node runs no price feed for ibkr` and
//!   the start goes on; a gateway that cannot be dialled refuses the subscription in words; a delayed or frozen
//!   stream is refused by name and an unlabelled one is never a price ([`PriceTap::door_price`]).
//! * **Quotes only**, so the tap prices from a two-sided quote of a stream labelled real-time and from nothing
//!   else (a trade print or a folded book is not a mark here).

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::LiveDataSink;

use super::FeedCtors;
use super::live::{QuoteFeed, Tracked};
use super::price_tap::{PriceTap, TapSink};

/// The venue's slug, spelled once for the price-only wiring in `feeds.rs` (which names no venue as a literal
/// beyond its feed arms: a file naming the whole roster is a per-venue TABLE to `new_venue_gate`, and this set
/// is not one).
pub(super) const VENUE: &str = "ibkr";

/// ibkr's price-only feed: the dial and the real-time-only price tap, nothing subscribed.
pub(super) fn wire_ibkr_price_feed(
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<QuoteFeed, String> {
    // The tap in front of the core's sink records the last two-sided quote and the stream's label; every verb is
    // forwarded to the core unchanged (the core keeps information-only labels, decision 0140).
    let core = Arc::new(vike_tradehub_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn LiveDataSink>;
    let tap = Arc::new(PriceTap::real_time_only());
    let sink = wrap(Arc::new(TapSink::new(core, Arc::clone(&tap))) as Arc<dyn LiveDataSink>);
    // Constructed through the [`FeedCtors`] seam, as every arm's: production is the strategy-lane dial.
    let feeds = Tracked::new(make.client(VENUE, None, sink)?);
    Ok(QuoteFeed::new(feeds, tap))
}
