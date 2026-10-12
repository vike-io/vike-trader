//! Deribit's feed arm: the keyless four-verb `DataClient` built through `FeedCtors`, then the arming.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_mount::MakerMountConfig;

use super::FeedCtors;
use crate::venue_arming::arming::{deribit_arming, other_live_accounts, with_other_live_accounts};

/// Deribit's feed: the keyless four-verb `DataClient` built through the [`FeedCtors`] seam on the
/// venue-agnostic `CoreLaneSink`, with a bar subscription per distinct `(symbol, interval)` and a
/// quote/trade/book triple per distinct symbol across this venue's mounts. Any subscription
/// failure refuses the mount.
pub(super) fn wire_deribit_feed(
    cfgs: &[&MakerMountConfig],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<Box<dyn DataClient + Send>, String> {
    // The KEYLESS shape (the hyperliquid arm's, widened): one `DataClient` onto the
    // venue-agnostic `CoreLaneSink` (through `wrap` like every other arm), no
    // config to thread because nothing here authenticates.
    //
    // FOUR verbs, and each is subscribed because `vike_model::venues::venue_caps::DERIBIT`
    // declares it — never a lane the row does not (the row's fifth, `depth`, is left unsubscribed
    // on purpose, below):
    //   • bars   → REAL venue candle closes (`BarFolder` infers the close from the
    //              successor's first push) on the lossless `close_bar` lane, so the
    //              margin-call watchdog and the 25% drawdown latch sweep on this venue's
    //              own closes. A SAFETY dependency, like the CEX kline lane.
    //   • quotes → the venue-throttled `quote.{inst}` channel, a NATIVE L1 stream behind
    //              the maker's `on_quote_tick`.
    //   • trades → executed prints, feeding the `PriceBoard`'s last-trade rung.
    //   • book   → the `change_id`-chained L2, folded to one standing `L2Book` and pushed
    //              at `LiveDataSink::book`, behind the maker's `on_order_book`.
    // ⚠ `subscribe_depth` is NOT wired here and must not be "added for completeness". The
    // VENUE serves it (the caps row declares `depth: true` since the datahub began serving
    // this venue's DOM, 2026-10-04), but this arm is a maker, and the conflating DOM lane
    // emits `LiveDataSink::l2_snapshot`, a DEFAULT NO-OP in every sink this daemon owns (the
    // exact trap `CexTicks`' doc records). The lossless `book` verb above is this arm's L2
    // surface.
    //
    // ⚠ MOUNT-FAILURE SEMANTICS are alpaca's, not ctrader's: `Feeds::new` does no network
    // work and each `subscribe_*` only spawns a thread that dials with the shared driver's
    // stop-aware backoff, so an unreachable host surfaces as a reconnect-looping feed in
    // the log, never a mount error. Nothing SYNCHRONOUS exists here to fail.
    let sink = wrap(Arc::new(vike_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn LiveDataSink>);
    // Constructed through the [`FeedCtors`] seam: production is the trait's default —
    // the real `vike_deribit::market_feed::Feeds`, byte-identical to the inline
    // constructor this line used to spell — and the deterministic splice test
    // (`src/tradehub_cli/tests/feed_splice.rs`) substitutes a scripted constructor HERE, which
    // is what lets it drive this very arm with scripted frames.
    let mut feeds = make.deribit(sink);
    // One BAR subscription per distinct (symbol, interval) across this venue's mounts —
    // each is its own chart socket, so two intervals genuinely both fire — and one
    // QUOTE/TRADE/BOOK triple per distinct symbol. The HL arm's dedup shape; every
    // interval here was proven mappable by `deribit_plan`.
    let mut bar_keys: Vec<(&str, &str)> = Vec::new();
    let mut tick_keys: Vec<&str> = Vec::new();
    for c in cfgs {
        if !bar_keys.contains(&(c.token_id.as_str(), c.interval.as_str())) {
            bar_keys.push((&c.token_id, &c.interval));
            feeds.subscribe_bars(&c.token_id, &c.interval).map_err(|e| {
                format!("deribit subscribe_bars {}@{}: {e:?}", c.token_id, c.interval)
            })?;
        }
        if !tick_keys.contains(&c.token_id.as_str()) {
            tick_keys.push(&c.token_id);
            feeds
                .subscribe_quotes(&c.token_id)
                .map_err(|e| format!("deribit subscribe_quotes {}: {e:?}", c.token_id))?;
            feeds
                .subscribe_trades(&c.token_id)
                .map_err(|e| format!("deribit subscribe_trades {}: {e:?}", c.token_id))?;
            feeds
                .subscribe_book(&c.token_id)
                .map_err(|e| format!("deribit subscribe_book {}: {e:?}", c.token_id))?;
        }
    }
    Ok(feeds)
}

/// Deribit's arming disclosure: exec LIVE or PAPER, with the venue's other live accounts folded
/// in. Takes no `data_only` set — the feed is keyless, so there is no declared data-only state.
pub(super) fn announce_deribit_arming(
    cfg: &MakerMountConfig,
    cfgs: &[&MakerMountConfig],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::VenueArming],
) {
    let arming = with_other_live_accounts(
        deribit_arming(live_venues.contains("deribit")),
        "deribit",
        other_live_accounts("deribit", venue_arming, live_venues),
    );
    match &arming.remedy {
        None => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE build_node core up; deribit feed subscribed (keyless MAINNET bars + \
             quotes + trades + L2 book) — ⚠ EXEC IS LIVE ON {network}: orders from this \
             mount are REAL orders against the deribit {network} account whose \
             credentials were found, priced off the MAINNET book this feed reads",
            network = arming.network
        ),
        Some(remedy) => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE build_node core up; deribit feed subscribed (keyless MAINNET bars + \
             quotes + trades + L2 book) — but EXEC IS PAPER for deribit and NO ORDER FROM \
             THIS MOUNT WILL EVER REACH THE VENUE. To arm it: {remedy}",
        ),
    }
}
