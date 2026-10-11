//! OANDA's feed arm: the two-lane `DataClient` built through `FeedCtors`, then the arming.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_mount::MakerMountConfig;

use super::FeedCtors;
use crate::venue_arming::arming::{
    data_only_arming, oanda_arming, other_live_accounts, with_other_live_accounts,
};

/// OANDA's feed: the two-lane `DataClient` built through the [`FeedCtors`] seam (streamed quotes +
/// polled candle closes) on the venue-agnostic `CoreLaneSink`, with a bar subscription per
/// distinct `(symbol, interval)` and a quote subscription per distinct symbol across this venue's
/// mounts. Any subscription failure refuses the mount.
pub(super) fn wire_oanda_feed(
    config: &vike_oanda::OandaConfig,
    cfgs: &[&MakerMountConfig],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<Box<dyn DataClient + Send>, String> {
    // The credentialed-data shape again, but over a client that owns NO socket: quotes
    // arrive on a chunked-HTTP `/pricing/stream` reader thread and bars on a candles-REST
    // POLL thread, both spawned by the client's own `FeedRegistry`. Everything lands on
    // the venue-agnostic `CoreLaneSink` (through `wrap` like every other arm),
    // NOT a `MakerSink`: this venue's poll lane emits REAL `close_bar`s on the lossless
    // lane, so the margin/drawdown watchdogs sweep on the venue's own candle closes and
    // nothing needs synthesizing (the ctrader arm's synth exists only because cTrader
    // never fires a close).
    //
    // ⚠ MOUNT-FAILURE SEMANTICS, and they are ctrader's OPPOSITE — deliberately.
    // `Feeds::with_config` performs NO network work, and each `subscribe_*` only spawns a
    // thread that then dials with its own stop-aware backoff, so a bad token or an
    // unreachable host surfaces as a reconnect-looping feed in the log, never a mount
    // error. That is the alpaca shape and it is the right one here: there is nothing
    // SYNCHRONOUS to fail, so a startup refusal would have to be invented rather than
    // observed. The credential half is already refused up front by `oanda_plan`, which is
    // where the honest gate for this venue lives.
    let sink = wrap(Arc::new(vike_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn LiveDataSink>);
    // Constructed through the [`FeedCtors`] seam (the deribit arm's shape): production
    // is the trait's default — the real `vike_oanda::market_feed::Feeds` over the plan's
    // resolved PRACTICE session, byte-identical to the inline constructor this line used
    // to spell — and the deterministic splice test (`src/tradehub_cli/tests/feed_splice.rs`)
    // substitutes a scripted constructor HERE, which is what lets it drive this very arm
    // with scripted frames on a weekend the real venue is closed for.
    let mut feeds = make.oanda(config, sink);
    // One BAR subscription per distinct (symbol, interval) across this venue's mounts —
    // each is its OWN poll thread at its own cadence, so unlike ctrader two intervals
    // genuinely both fire — and one QUOTE subscription per distinct symbol. The HL arm's
    // dedup shape; every interval here was proven mappable by `oanda_plan`.
    let mut bar_keys: Vec<(&str, &str)> = Vec::new();
    let mut quote_keys: Vec<&str> = Vec::new();
    for c in cfgs {
        if !bar_keys.contains(&(c.token_id.as_str(), c.interval.as_str())) {
            bar_keys.push((&c.token_id, &c.interval));
            feeds.subscribe_bars(&c.token_id, &c.interval).map_err(|e| {
                format!("oanda subscribe_bars {}@{}: {e:?}", c.token_id, c.interval)
            })?;
        }
        if !quote_keys.contains(&c.token_id.as_str()) {
            quote_keys.push(&c.token_id);
            feeds
                .subscribe_quotes(&c.token_id)
                .map_err(|e| format!("oanda subscribe_quotes {}: {e:?}", c.token_id))?;
        }
    }
    Ok(feeds)
}

/// OANDA's arming disclosure: exec LIVE or PAPER (or paper by `data_only` declaration), with the
/// venue's other live accounts folded in.
pub(super) fn announce_oanda_arming(
    cfg: &MakerMountConfig,
    cfgs: &[&MakerMountConfig],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::VenueArming],
    data_only: &std::collections::HashSet<String>,
) {
    let exec_live = live_venues.contains("oanda");
    let arming = with_other_live_accounts(
        if data_only.contains("oanda") && !exec_live {
            // The DECLARED exception: paper-by-declaration, not the gate disagreement the
            // ordinary remedy reports. Guarded on `!exec_live` so a build where the withhold
            // failed to hold still announces `EXEC IS LIVE` honestly.
            data_only_arming(oanda_arming(false), "oanda")
        } else {
            oanda_arming(exec_live)
        },
        "oanda",
        other_live_accounts("oanda", venue_arming, live_venues),
    );
    match &arming.remedy {
        None => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE build_node core up; oanda feed subscribed (streamed L1 quotes + POLLED \
             candle closes, both Bearer-authed) — ⚠ EXEC IS LIVE ON {network}: orders \
             from this mount are REAL orders against the oanda {network} account whose \
             token was found",
            network = arming.network
        ),
        Some(remedy) => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE build_node core up; oanda feed subscribed (streamed L1 quotes + POLLED \
             candle closes, both Bearer-authed) — but EXEC IS PAPER for oanda and NO \
             ORDER FROM THIS MOUNT WILL EVER REACH THE VENUE: {remedy}",
        ),
    }
}
