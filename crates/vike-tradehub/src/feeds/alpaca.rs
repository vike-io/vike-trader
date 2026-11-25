//! Alpaca's feed arm: one authenticated multiplexed WS client, then the arming disclosure.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, DataClientSink};
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::MountPlacement;

use super::live::StrategyLaneQuote;
use super::price_tap::PriceTap;
use super::{FeedRefusal, StrategyLaneFactories};
use crate::venue_arming::arming::{
    alpaca_arming, data_only_arming, mounts_execution_live, other_live_accounts,
    with_labelled_mount_remedy, with_other_live_accounts,
};

/// Alpaca's feed: ONE authenticated multiplexed client on the venue-agnostic `CoreLaneSink` —
/// bars per distinct `(symbol, interval)`, quotes and trades per distinct symbol across this
/// venue's mounts. Any subscription failure refuses the mount.
pub(super) fn wire_alpaca_feed(
    configs: &[&MountPlacement],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn DataClientSink>) -> Arc<dyn DataClientSink>,
    make: &dyn StrategyLaneFactories,
) -> Result<StrategyLaneQuote, FeedRefusal> {
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
    // QUOTE lane on this same dial through the feed's watcher (`StrategyLaneQuote`).
    let (mut client, tap) = super::dial("alpaca", None, PriceTap::default, handle, wrap, make)?;
    // One BAR subscription per distinct (symbol, interval) across this venue's mounts
    // (interval is "1m" per mount — `alpaca_plan` refused anything else), one QUOTE+TRADE
    // pair per distinct symbol — the HL arm's dedup shape.
    let mut bar_keys: Vec<(&str, &str)> = Vec::new();
    let mut tick_keys: Vec<&str> = Vec::new();
    for c in configs {
        if !bar_keys.contains(&(c.symbol.as_str(), c.interval.as_str())) {
            bar_keys.push((&c.symbol, &c.interval));
            client.subscribe_bars(&c.symbol, &c.interval).map_err(|e| {
                FeedRefusal::subscribe(
                    format_args!("alpaca subscribe_bars {}@{}", c.symbol, c.interval),
                    &e,
                )
            })?;
        }
        if !tick_keys.contains(&c.symbol.as_str()) {
            tick_keys.push(&c.symbol);
            client.subscribe_quotes(&c.symbol).map_err(|e| {
                FeedRefusal::subscribe(format_args!("alpaca subscribe_quotes {}", c.symbol), &e)
            })?;
            client.subscribe_trades(&c.symbol).map_err(|e| {
                FeedRefusal::subscribe(format_args!("alpaca subscribe_trades {}", c.symbol), &e)
            })?;
        }
    }
    Ok(StrategyLaneQuote::new(client, tap))
}

/// Alpaca's arming disclosure: exec LIVE or PAPER (or paper by `data_only` declaration), with the
/// venue's other live accounts folded in.
pub(super) fn announce_alpaca_arming(
    config: &MountPlacement,
    configs: &[&MountPlacement],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::ArmingAccount],
    mount_accounts: &[AccountLabel],
    data_only: &std::collections::HashSet<String>,
) {
    let execution_live = mounts_execution_live("alpaca", mount_accounts, live_venues);
    let arming = with_other_live_accounts(
        if data_only.contains("alpaca") && !execution_live {
            // The DECLARED exception: paper-by-declaration, not the gate disagreement the
            // ordinary remedy reports. Guarded on `!execution_live` so a build where the withhold
            // failed to hold still announces `EXEC IS LIVE` honestly.
            data_only_arming(alpaca_arming(false), "alpaca")
        } else {
            with_labelled_mount_remedy(alpaca_arming(execution_live), "alpaca", mount_accounts)
        },
        "alpaca",
        other_live_accounts("alpaca", venue_arming, live_venues, mount_accounts),
    );
    let message = match &arming.remedy {
        None => format!(
            "LIVE assemble_runtime core up; alpaca feed subscribed (1m bars + quotes + trades, \
             credentialed WS) — ⚠ EXEC IS LIVE ON {network}: orders from this mount are \
             REAL orders against the alpaca {network} account whose credentials were found",
            network = arming.network
        ),
        Some(remedy) => format!(
            "LIVE assemble_runtime core up; alpaca feed subscribed (1m bars + quotes + trades, \
             credentialed WS) — but EXEC IS NOT LIVE for alpaca and NO ORDER FROM THIS MOUNT \
             WILL EVER REACH THE VENUE: {remedy}",
        ),
    };
    warn_arming_banner!(config.venue, config, configs, arming, live_venues, message);
}
