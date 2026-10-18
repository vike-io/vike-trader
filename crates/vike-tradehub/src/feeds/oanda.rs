//! OANDA's feed arm: the two-lane `DataClient` built through `FeedCtors`, then the arming.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::MakerConfig;

use super::FeedCtors;
use super::live::Tracked;
use crate::venue_arming::arming::{
    data_only_arming, mounts_exec_live, oanda_arming, other_live_accounts,
    with_labelled_mount_remedy, with_other_live_accounts,
};

/// OANDA's feed: the two-lane `DataClient` built through the [`FeedCtors`] seam (streamed quotes +
/// polled candle closes) on the venue-agnostic `CoreLaneSink`, with a bar subscription per
/// distinct `(symbol, interval)` and a quote subscription per distinct symbol across this venue's
/// mounts. Any subscription failure refuses the mount.
pub(super) fn wire_oanda_feed(
    cfgs: &[&MakerConfig],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<Tracked, String> {
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
    // The client builds NO socket at the gatewayhub's open and each `subscribe_*` only starts a
    // thread that then dials with its own stop-aware backoff, so a bad token or an
    // unreachable host surfaces as a reconnect-looping feed in the log, never a mount
    // error. That is the alpaca shape and it is the right one here: there is nothing
    // SYNCHRONOUS to fail. The credential half is the gatewayhub's refusal of the credentialed stream
    // (the account's `API_KEY` + `ACCOUNT_ID` fields), which reaches this arm as the first
    // `subscribe_*`'s error and refuses the mount.
    let sink = wrap(Arc::new(vike_tradehub_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn LiveDataSink>);
    // Constructed through the [`FeedCtors`] seam (the deribit arm's shape): production
    // is the strategy-lane dial — a credentialed stream at the gatewayhub, over the PRACTICE session
    // of the venue's default `demo` account — and the deterministic splice test
    // (`src/tradehub_cli/tests/feed_splice.rs`) substitutes a scripted constructor HERE, which is
    // what lets it drive this very arm with scripted frames on a weekend the real venue is closed for.
    let mut feeds = Tracked::new(make.client("oanda", None, sink)?);
    // One BAR subscription per distinct (symbol, interval) across this venue's mounts —
    // each is its OWN poll thread at its own cadence, so unlike ctrader two intervals
    // genuinely both fire — and one QUOTE subscription per distinct symbol. The HL arm's
    // dedup shape; an interval the venue has no candle series for is the bridge's refusal of the bar
    // subscription (`oanda_plan` no longer holds the table), and refuses the mount here.
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
    cfg: &MakerConfig,
    cfgs: &[&MakerConfig],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::VenueArming],
    mount_accounts: &[AccountLabel],
    data_only: &std::collections::HashSet<String>,
) {
    let exec_live = mounts_exec_live("oanda", mount_accounts, live_venues);
    let arming = with_other_live_accounts(
        if data_only.contains("oanda") && !exec_live {
            // The DECLARED exception: paper-by-declaration, not the gate disagreement the
            // ordinary remedy reports. Guarded on `!exec_live` so a build where the withhold
            // failed to hold still announces `EXEC IS LIVE` honestly.
            data_only_arming(oanda_arming(false), "oanda")
        } else {
            with_labelled_mount_remedy(oanda_arming(exec_live), "oanda", mount_accounts)
        },
        "oanda",
        other_live_accounts("oanda", venue_arming, live_venues, mount_accounts),
    );
    match &arming.remedy {
        None => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE assemble_runtime core up; oanda feed subscribed (streamed L1 quotes + POLLED \
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
            "LIVE assemble_runtime core up; oanda feed subscribed (streamed L1 quotes + POLLED \
             candle closes, both Bearer-authed) — but EXEC IS PAPER for oanda and NO \
             ORDER FROM THIS MOUNT WILL EVER REACH THE VENUE: {remedy}",
        ),
    }
}
