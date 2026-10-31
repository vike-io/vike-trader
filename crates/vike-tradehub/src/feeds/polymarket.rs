//! Polymarket's feed arm (compiled into every build since W15; the venue itself starts only where the gatewayhub carries its `polymarket` feature): one book-driven feed + `MakerSink` per token.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_tradehub_core::MakerConfig;

use super::FeedCtors;
use super::live::{QuoteFeed, Tracked};
use super::price_tap::{PriceTap, TapSink};

/// The venue's slug, spelled once for the price-only wiring in `feeds.rs` (which names no venue as a literal
/// beyond its feed arms).
pub(super) const VENUE: &str = "polymarket";

/// Polymarket's PRICE-ONLY feed (any-symbol price lane, decision 0174): the strategy-lane dial with NOTHING
/// subscribed and a [`PriceTap`] in front of the core's sink, for a box that mounts no Polymarket token.
///
/// The control door admits an outcome token the account's engine was not started on (the engine is mounted on no
/// symbol, so every token is one) and prices it through the feed's [`QuoteFeed::watcher`]: the first order on a
/// token subscribes its QUOTE lane (the venue's book-derived best bid and ask, one pump per token on the dial),
/// and the price is read back from the same tap.
///
/// * **LAZY.** Wiring subscribes nothing. A refused dial (no strategy-lane key, a datahub that advertises no
///   polymarket lane: the probe in `wire_price_only_feeds`) leaves no watcher, so the door answers `this node
///   runs no price feed for polymarket` and refuses an opening order on a new token (fail closed).
/// * **Quotes only**, into a plain [`PriceTap`] (the quote's mid, aged from its arrival). A one-sided book is no
///   two-sided quote and the tap prices nothing from it. A MOUNT's token feed (`wire_polymarket_feeds`, one dial
///   and one `MakerSink` per token) is a different object: when a box mounts a Polymarket token this feed is not
///   wired (the venue is then not price-only), so such a box prices a new token nowhere and the door says so.
pub(super) fn wire_polymarket_price_feed(
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<QuoteFeed, String> {
    let core = Arc::new(vike_tradehub_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn LiveDataSink>;
    let tap = Arc::new(PriceTap::default());
    let sink = wrap(Arc::new(TapSink::new(core, Arc::clone(&tap))) as Arc<dyn LiveDataSink>);
    let feeds = Tracked::new(make.client(VENUE, None, sink)?);
    Ok(QuoteFeed::new(feeds, tap))
}

/// Polymarket's feeds: one feed + `MakerSink` per DISTINCT token (a sink folds every price it
/// sees into its token's synth bars), each driven by a single book subscription. Any subscription
/// failure refuses the mount.
pub(super) fn wire_polymarket_feeds(
    cfgs: &[&MakerConfig],
    handle: &CoreHandle,
    live_venues: &std::collections::HashSet<String>,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<Vec<Tracked>, String> {
    // Polymarket's feed serves TICKS, not bars (`subscribe_bars` is Unsupported), so wire it
    // through `MakerSink` (NOT the bare CoreLaneSink): its inner CoreLaneSink forwards the
    // book-derived L1 quote onto the tick/market lanes (drives the maker's on_quote_tick /
    // on_order_book), AND its `TickBarSynthesizer` emits synth bars onto the bar lane (drives
    // the account-wide margin/drawdown sweeps the HL path gets from real bars). One
    // `subscribe_book` drives all of it. Keyless-public, but egresses through the SAME Dublin
    // SOCKS tunnel the exec side uses (US-geo-blocked otherwise) — that egress is the gatewayhub's now.
    // One feed + `MakerSink` per DISTINCT token (split-plane I10): a `MakerSink` folds
    // EVERY price it sees into ITS token's synth bars, so two tokens through one sink
    // would corrupt both mounts' paper-fill bar lanes — see `LiveFeeds::Polymarket`. Two
    // mounts on ONE token share one entry (their interval agreement was refused-or-proven
    // by `check_poly_token_intervals` before the core spawned).
    //
    // ⚠ One strategy-lane dial (one datahub connection) per TOKEN, because each token has its own sink:
    // the lane's connection cap (`vike-datahub`'s `SL_MAX_STREAM_CONNS`) counts them with every other venue's.
    let mut list: Vec<Tracked> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for c in cfgs {
        if seen.contains(&c.token_id.as_str()) {
            continue;
        }
        seen.push(&c.token_id);
        let sink = wrap(Arc::new(vike_tradehub_core::MakerSink::new(
            handle,
            c.venue.clone(),
            c.token_id.clone(),
            c.interval.clone(),
            c.interval_ms,
        )) as Arc<dyn LiveDataSink>);
        let mut feeds = Tracked::new(make.client("polymarket", None, sink)?);
        feeds
            .subscribe_book(&c.token_id)
            .map_err(|e| format!("subscribe_book {}: {e:?}", c.token_id))?;
        tracing::info!(
            venue = %c.venue, symbol = %c.token_id, interval = %c.interval,
            mounts = cfgs.len(), tokens = seen.len(),
            live_venues = ?live_venues,
            "LIVE assemble_runtime core up; Polymarket feed subscribed (book → derived L1 quote) — real \
             orders require flags.poly_exec + POLY_PRIVATE_KEY + Dublin egress"
        );
        list.push(feeds);
    }
    Ok(list)
}
