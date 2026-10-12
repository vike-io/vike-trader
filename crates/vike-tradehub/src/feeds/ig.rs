//! IG's feed arm: the two-verb Lightstreamer `Feeds`, then the arming disclosure.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_mount::MakerMountConfig;

use crate::venue_arming::arming::{
    data_only_arming, ig_arming, other_live_accounts, with_other_live_accounts,
};

/// IG's feed: the Lightstreamer `Feeds` on the venue-agnostic `CoreLaneSink` — exactly two verbs,
/// a bar subscription per distinct `(epic, interval)` and a quote subscription per distinct epic
/// across this venue's mounts. Any subscription failure refuses the mount.
pub(super) fn wire_ig_feed(
    config: &vike_ig::IgConfig,
    cfgs: &[&MakerMountConfig],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
) -> Result<vike_ig::market_feed::Feeds, String> {
    // The credentialed-data shape over a THIRD kind of transport: Lightstreamer TLCP text
    // frames. Everything lands on the venue-agnostic `CoreLaneSink` (through
    // `wrap` like every other arm), NOT a `MakerSink`: IG's candle lane emits REAL
    // `close_bar`s on the lossless lane (`CandleFold` closes on `CONS_END` or on a `UTM`
    // advance, whichever arrives first — the second signal exists so a lost frame cannot
    // silently drop a close), so the margin/drawdown watchdogs sweep on the venue's own
    // candle closes and nothing needs synthesizing.
    //
    // ⚠ EXACTLY TWO VERBS, and this is where the arm must not be padded out to look like
    // its neighbours: `subscribe_trades`/`subscribe_book`/`subscribe_depth` are all caps
    // refusals on this venue (`venue_caps::IG` — a dealer publishes no tape and no
    // ladder), so calling one here would turn a structural venue fact into a mount error.
    // The maker requotes on `on_quote_tick` alone; `ig_arming` discloses exactly that.
    //
    // ⚠ MOUNT-FAILURE SEMANTICS are alpaca's, not ctrader's: `Feeds::new` performs NO
    // network work and each `subscribe_*` only spawns a thread that then logs in and dials
    // with the driver's stop-aware backoff, so a wrong password or an unreachable gateway
    // surfaces as a reconnect-looping feed in the log, never a mount error. The credential
    // half is already refused up front by `ig_plan`, which is where the honest gate lives.
    //
    // ⚠ ONE IG REST SESSION PER SUBSCRIPTION — more than the exec side opens, and IG
    // publishes no rate gate this workspace paces against
    // (`vike_model::venues::venue_rate_limits`'s `IG` declares none), so keep the mount set small.
    // Unlike the exec lane, these sessions DO re-login after token expiry.
    let sink = wrap(Arc::new(vike_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn LiveDataSink>);
    let mut feeds = vike_ig::market_feed::Feeds::new(sink, || {}, config.clone());
    // One BAR subscription per distinct (epic, interval) across this venue's mounts — each
    // is its own TLCP session, so unlike ctrader two intervals genuinely both fire — and
    // one QUOTE subscription per distinct epic. The HL arm's dedup shape; every interval
    // here was proven streamable by `ig_plan`.
    let mut bar_keys: Vec<(&str, &str)> = Vec::new();
    let mut quote_keys: Vec<&str> = Vec::new();
    for c in cfgs {
        if !bar_keys.contains(&(c.token_id.as_str(), c.interval.as_str())) {
            bar_keys.push((&c.token_id, &c.interval));
            feeds
                .subscribe_bars(&c.token_id, &c.interval)
                .map_err(|e| format!("ig subscribe_bars {}@{}: {e:?}", c.token_id, c.interval))?;
        }
        if !quote_keys.contains(&c.token_id.as_str()) {
            quote_keys.push(&c.token_id);
            feeds
                .subscribe_quotes(&c.token_id)
                .map_err(|e| format!("ig subscribe_quotes {}: {e:?}", c.token_id))?;
        }
    }
    Ok(feeds)
}

/// IG's arming disclosure: exec LIVE or PAPER (or paper by `data_only` declaration), with the
/// venue's other live accounts folded in.
pub(super) fn announce_ig_arming(
    cfg: &MakerMountConfig,
    cfgs: &[&MakerMountConfig],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::VenueArming],
    data_only: &std::collections::HashSet<String>,
) {
    let exec_live = live_venues.contains("ig");
    let arming = with_other_live_accounts(
        if data_only.contains("ig") && !exec_live {
            // The DECLARED exception — same shape as the alpaca/oanda branches above.
            data_only_arming(ig_arming(false), "ig")
        } else {
            ig_arming(exec_live)
        },
        "ig",
        other_live_accounts("ig", venue_arming, live_venues),
    );
    match &arming.remedy {
        None => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE build_node core up; ig feed subscribed (Lightstreamer L1 quotes + \
             streamed candle closes, both over per-subscription IG logins) — ⚠ EXEC IS \
             LIVE ON {network}: orders from this mount are REAL orders against the ig \
             {network} account whose credentials were found",
            network = arming.network
        ),
        Some(remedy) => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE build_node core up; ig feed subscribed (Lightstreamer L1 quotes + \
             streamed candle closes, both over per-subscription IG logins) — but EXEC IS \
             PAPER for ig and NO ORDER FROM THIS MOUNT WILL EVER REACH THE VENUE: {remedy}",
        ),
    }
}
