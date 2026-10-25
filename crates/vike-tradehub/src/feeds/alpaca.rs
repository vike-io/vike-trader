//! Alpaca's feed arm: one authenticated multiplexed WS client, then the arming disclosure.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::MakerConfig;

use super::FeedCtors;
use super::live::{QuoteFeed, Tracked};
use super::price_tap::{PriceTap, TapSink};
use crate::venue_arming::arming::{
    alpaca_arming, data_only_arming, mounts_exec_live, other_live_accounts,
    with_labelled_mount_remedy, with_other_live_accounts,
};

/// Alpaca's feed: ONE authenticated multiplexed client on the venue-agnostic `CoreLaneSink` —
/// bars per distinct `(symbol, interval)`, quotes and trades per distinct symbol across this
/// venue's mounts. Any subscription failure refuses the mount.
pub(super) fn wire_alpaca_feed(
    cfgs: &[&MakerConfig],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<QuoteFeed, String> {
    // The CREDENTIALED-DATA shape (split-plane I9): ONE authenticated WS client serves
    // every lane through the venue-agnostic `CoreLaneSink` (through `wrap` like
    // the HL arm) — real 1m bar CLOSES drive the margin/drawdown watchdogs (a SAFETY
    // dependency, like the CEX kline lane), quotes drive `on_quote_tick` (alpaca has no
    // book lane — `subscribe_book` is a caps refusal — so the maker's only requote verb
    // here is the quote), and trade prints feed the PriceBoard's last-trade rung.
    //
    // The client is a CREDENTIALED STREAM at the gatewayhub (the datahub opens it under its control key),
    // which logs in as the venue's DEFAULT `demo` account: this arm resolves and holds no credential.
    // ABSENT credentials are the gatewayhub's refusal of the open, and it reaches this arm as the
    // first `subscribe_*`'s error text (the link is down, naming the missing fields), which refuses the
    // mount — a feed-less live mount would quote into the void forever. A wrong secret surfaces as a
    // reconnect-looping feed in the log, never a mount error, as before.
    //
    // The tap in front of the core's sink only RECORDS the last price per symbol, so the control door
    // can tell whether a symbol it admitted has a price yet (decision 0174, as Hyperliquid's and
    // deribit's arms); every verb is forwarded unchanged. A symbol admitted at the door subscribes its
    // QUOTE lane on this same dial through the feed's watcher (`QuoteFeed`).
    let core = Arc::new(vike_tradehub_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn LiveDataSink>;
    let tap = Arc::new(PriceTap::default());
    let sink = wrap(Arc::new(TapSink::new(core, Arc::clone(&tap))) as Arc<dyn LiveDataSink>);
    let mut client = Tracked::new(make.client("alpaca", None, sink)?);
    // One BAR subscription per distinct (symbol, interval) across this venue's mounts
    // (interval is "1m" per mount — `alpaca_plan` refused anything else), one QUOTE+TRADE
    // pair per distinct symbol — the HL arm's dedup shape.
    let mut bar_keys: Vec<(&str, &str)> = Vec::new();
    let mut tick_keys: Vec<&str> = Vec::new();
    for c in cfgs {
        if !bar_keys.contains(&(c.token_id.as_str(), c.interval.as_str())) {
            bar_keys.push((&c.token_id, &c.interval));
            client.subscribe_bars(&c.token_id, &c.interval).map_err(|e| {
                format!("alpaca subscribe_bars {}@{}: {e:?}", c.token_id, c.interval)
            })?;
        }
        if !tick_keys.contains(&c.token_id.as_str()) {
            tick_keys.push(&c.token_id);
            client
                .subscribe_quotes(&c.token_id)
                .map_err(|e| format!("alpaca subscribe_quotes {}: {e:?}", c.token_id))?;
            client
                .subscribe_trades(&c.token_id)
                .map_err(|e| format!("alpaca subscribe_trades {}: {e:?}", c.token_id))?;
        }
    }
    Ok(QuoteFeed::new(client, tap))
}

/// Alpaca's arming disclosure: exec LIVE or PAPER (or paper by `data_only` declaration), with the
/// venue's other live accounts folded in.
pub(super) fn announce_alpaca_arming(
    cfg: &MakerConfig,
    cfgs: &[&MakerConfig],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::VenueArming],
    mount_accounts: &[AccountLabel],
    data_only: &std::collections::HashSet<String>,
) {
    let exec_live = mounts_exec_live("alpaca", mount_accounts, live_venues);
    let arming = with_other_live_accounts(
        if data_only.contains("alpaca") && !exec_live {
            // The DECLARED exception: paper-by-declaration, not the gate disagreement the
            // ordinary remedy reports. Guarded on `!exec_live` so a build where the withhold
            // failed to hold still announces `EXEC IS LIVE` honestly.
            data_only_arming(alpaca_arming(false), "alpaca")
        } else {
            with_labelled_mount_remedy(alpaca_arming(exec_live), "alpaca", mount_accounts)
        },
        "alpaca",
        other_live_accounts("alpaca", venue_arming, live_venues, mount_accounts),
    );
    match &arming.remedy {
        None => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE assemble_runtime core up; alpaca feed subscribed (1m bars + quotes + trades, \
             credentialed WS) — ⚠ EXEC IS LIVE ON {network}: orders from this mount are \
             REAL orders against the alpaca {network} account whose credentials were found",
            network = arming.network
        ),
        Some(remedy) => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE assemble_runtime core up; alpaca feed subscribed (1m bars + quotes + trades, \
             credentialed WS) — but EXEC IS PAPER for alpaca and NO ORDER FROM THIS MOUNT \
             WILL EVER REACH THE VENUE: {remedy}",
        ),
    }
}
