//! dukascopy's PRICE-ONLY feed arm (any-symbol price lane, decision 0174): the strategy-lane dial with NOTHING
//! subscribed and a [`PriceTap`] in front of the core's sink.
//!
//! dukascopy is never a mount (no profile names it, `FEED_ARMS` has no row for it), so unlike the other price-only
//! venues' twins there is no mounted `wire_dukascopy_feed` beside this one. The control door opens a pair's QUOTE
//! lane through the feed's [`QuoteFeed::watcher`] when an order names a pair the engine was not started on, and
//! reads its price back through the same handle:
//!
//! * **LAZY, end to end.** Wiring subscribes nothing. The datahub's strategy lane opens a credentialed stream for
//!   the dial (`Scope::Write`, `cs_venue=dukascopy`), and the gatewayhub's end of it
//!   (`crates/vike-gatewayhub/src/data/dukascopy_quotes.rs`) is built with nothing dialled, because it logs in to
//!   nothing: a subscription is one `subscribe` command to the JForex sidecar the trade lane already started.
//! * **FAIL-CLOSED.** A refused dial (no strategy-lane key, a datahub that advertises no dukascopy lane: the probe
//!   in `wire_price_only_feeds`) leaves no watcher, so the door answers `this node runs no price feed for
//!   dukascopy` and the start goes on; a gateway with no sidecar running, or a sidecar jar that predates the price
//!   lane, refuses the subscription in words.
//! * **Quotes only**, and every one the gatewayhub emits is already a REAL-TIME, two-sided, uncrossed quote
//!   (`vike_dukascopy::quotes::accept` judges each tick against its own time before the stream sees it), so
//!   the plain [`PriceTap`] (the quote's mid) is the right tap and no label has to be checked here: there is no
//!   delayed mode in JForex to refuse. A quiet pair's last quote ages out at the tap's own bound
//!   (`feeds/price_tap.rs`'s `MAX_PRICE_AGE`) and the door answers "none yet".

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::LiveDataSink;

use super::FeedCtors;
use super::live::{QuoteFeed, Tracked};
use super::price_tap::{PriceTap, TapSink};

/// The venue's slug, spelled once for the price-only wiring in `feeds.rs` (which names no venue as a literal
/// beyond its feed arms: a file naming the whole roster is a per-venue TABLE to `new_venue_gate`, and this set
/// is not one).
pub(super) const VENUE: &str = "dukascopy";

/// dukascopy's price-only feed: the dial and the price tap, nothing subscribed.
pub(super) fn wire_dukascopy_price_feed(
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<QuoteFeed, String> {
    // The tap in front of the core's sink records the last two-sided quote per pair; every verb is forwarded to
    // the core unchanged (the core keeps information-only labels, decision 0140).
    let core = Arc::new(vike_tradehub_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn LiveDataSink>;
    let tap = Arc::new(PriceTap::default());
    let sink = wrap(Arc::new(TapSink::new(core, Arc::clone(&tap))) as Arc<dyn LiveDataSink>);
    // Constructed through the [`FeedCtors`] seam, as every arm's: production is the strategy-lane dial.
    let feeds = Tracked::new(make.client(VENUE, None, sink)?);
    Ok(QuoteFeed::new(feeds, tap))
}
