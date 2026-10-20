//! fxcm's PRICE-ONLY feed arm (any-symbol price lane, decision 0174): the strategy-lane dial with NOTHING
//! subscribed and a [`PriceTap`] in front of the core's sink.
//!
//! fxcm is never a mount (no profile names it: the venue has no market-data seam at all, `FEED_ARMS` has no row for
//! it), so there is no mounted `wire_fxcm_feed` beside this one. The control door opens a pair's QUOTE lane through
//! the feed's [`QuoteFeed::watcher`] when an order names a pair the engine was not started on, and reads its price
//! back through the same handle. The twin of `feeds/dukascopy.rs`:
//!
//! * **LAZY, end to end.** Wiring subscribes nothing. The datahub's strategy lane opens a credentialed stream for
//!   the dial (`Scope::Write`, `cs_venue=fxcm`), and the gatewayhub's end of it
//!   (`crates/vike-gatewayhub/src/data/fxcm_quotes.rs`) is built with nothing dialled, because it logs in to
//!   nothing: a subscription is a pair added to the list the account's already-running reconcile session reads off
//!   the venue's Offers table every couple of seconds.
//! * **FAIL-CLOSED.** A refused dial (no strategy-lane key, a datahub that advertises no fxcm lane: the probe in
//!   `wire_price_only_feeds`) leaves no watcher, so the door answers `this node runs no price feed for fxcm` and the
//!   start goes on; a gateway with no reconcile session (reconciliation off, a failed login, a shim without
//!   `fc_offers`) refuses the subscription, or prices nothing, in words.
//! * **Quotes only**, and every one the gatewayhub emits is already a LIVE two-sided quote of an open market, no
//!   older than two minutes by its own time (`vike_fxcm::quotes::judge`), and no label has to be checked here. The
//!   tap is [`PriceTap::venue_clocked`], as OANDA's: the gatewayhub RE-DELIVERS a live quote at every poll (its
//!   `ts` is the quote's own moment, `asked - age`, and `local_ts` the receipt), so a tap that aged a quote from
//!   its arrival would hold a frozen quote fresh for 60 s after the last delivery on top of its own age (up to
//!   ~180 s at the door and in an admission's first mark). Aged from the venue's time, a quote older than
//!   `MAX_PRICE_AGE` (`feeds/price_tap.rs`) is no price however recently it arrived, and the door answers "none yet".
//! * **The cap is the gatewayhub's** (`vike_fxcm::quotes::MAX_PRICED_SYMBOLS`): a subscription past it is refused
//!   in words there, and the idle reaper (`LeaseFacts::close_idle_pumps`) frees the place of a pair no account admitted.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::LiveDataSink;

use super::FeedCtors;
use super::live::{QuoteFeed, Tracked};
use super::price_tap::{PriceTap, TapSink};

/// The venue's slug, spelled once for the price-only wiring in `feeds.rs` (which names no venue as a literal
/// beyond its feed arms: a file naming the whole roster is a per-venue TABLE to `new_venue_gate`, and this set
/// is not one).
pub(super) const VENUE: &str = "fxcm";

/// fxcm's price-only feed: the dial and the price tap, nothing subscribed.
pub(super) fn wire_fxcm_price_feed(
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
    let tap = Arc::new(PriceTap::venue_clocked());
    let sink = wrap(Arc::new(TapSink::new(core, Arc::clone(&tap))) as Arc<dyn LiveDataSink>);
    // Constructed through the [`FeedCtors`] seam, as every arm's: production is the strategy-lane dial.
    let feeds = Tracked::new(make.client(VENUE, None, sink)?);
    Ok(QuoteFeed::new(feeds, tap))
}
