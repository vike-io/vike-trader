//! OANDA's feed arm: the two-lane `DataClient` built through `StrategyLaneFactories`, then the arming.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, DataClientSink};
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::MountPlacement;

use super::StrategyLaneFactories;
use super::live::StrategyLaneQuote;
use super::price_tap::PriceTap;
use crate::venue_arming::arming::{
    data_only_arming, mounts_execution_live, oanda_arming, other_live_accounts,
    with_labelled_mount_remedy, with_other_live_accounts,
};

/// OANDA's feed: the two-lane `DataClient` built through the [`StrategyLaneFactories`] seam (streamed quotes +
/// polled candle closes) on the venue-agnostic `CoreLaneSink`, with a bar subscription per
/// distinct `(symbol, interval)` and a quote subscription per distinct symbol across this venue's
/// mounts. Any subscription failure refuses the mount, except a CAP refusal of one symbol's quote stream
/// (that symbol runs without quotes and the start goes on) and a TRANSIENT one, which is asked again
/// (`super::subscribe_quotes_surviving`).
///
/// The feed is held as a [`StrategyLaneQuote`] (as alpaca's): a tap in front of the core's sink records the last
/// price per symbol, and the [`StrategyLaneQuote::watcher`] subscribes the QUOTE lane of a symbol the control door
/// admits (decision 0174), so a foreign OANDA position has a mark. One `/pricing/stream` per symbol (the
/// bridge's `market_feed` module doc), so the venue's session cap bounds the admitted symbols (UNMEASURED).
pub(super) fn wire_oanda_feed(
    configs: &[&MountPlacement],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn DataClientSink>) -> Arc<dyn DataClientSink>,
    make: &dyn StrategyLaneFactories,
) -> Result<StrategyLaneQuote, String> {
    // The credentialed-data shape again, but over a client that owns NO socket: quotes
    // arrive on a chunked-HTTP `/pricing/stream` reader thread and bars on a candles-REST
    // POLL thread, both spawned by the client's own `DataClientRegistry`. Everything lands on
    // the venue-agnostic `CoreLaneSink` (through `wrap` like every other arm),
    // NOT a `TickSink`: this venue's poll lane emits REAL `close_bar`s on the lossless
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
    //
    // The shared `feeds.rs` `dial` (production: a credentialed stream at the gatewayhub, over the PRACTICE
    // session of the venue's default `demo` account; the splice test drives this very arm with scripted
    // frames on a weekend the real venue is closed for), with the tap of the venue's price-only row
    // (`feeds.rs`'s `PRICE_ONLY_ARMS`). It ages a quote from the VENUE's time: the bridge's REST first price
    // arrives up to its bound old, and aged from arrival it would stay a door price after the core's
    // price-only sweep calls it stale.
    let (mut feeds, tap) = super::dial("oanda", None, PriceTap::venue_clocked, handle, wrap, make)?;
    // One BAR subscription per distinct (symbol, interval) across this venue's mounts —
    // each is its OWN poll thread at its own cadence, so unlike ctrader two intervals
    // genuinely both fire — and one QUOTE subscription per distinct symbol. The HL arm's
    // dedup shape; an interval the venue has no candle series for is the bridge's refusal of the bar
    // subscription (`oanda_plan` no longer holds the table), and refuses the mount here.
    let mut bar_keys: Vec<(&str, &str)> = Vec::new();
    let mut quote_keys: Vec<&str> = Vec::new();
    for c in configs {
        if !bar_keys.contains(&(c.symbol.as_str(), c.interval.as_str())) {
            bar_keys.push((&c.symbol, &c.interval));
            feeds
                .subscribe_bars(&c.symbol, &c.interval)
                .map_err(|e| format!("oanda subscribe_bars {}@{}: {e:?}", c.symbol, c.interval))?;
        }
        if !quote_keys.contains(&c.symbol.as_str()) {
            quote_keys.push(&c.symbol);
            super::subscribe_quotes_surviving("oanda", &mut feeds, &c.symbol, make)?;
        }
    }
    Ok(StrategyLaneQuote::new(feeds, tap))
}

/// OANDA's arming disclosure: exec LIVE or PAPER (or paper by `data_only` declaration), with the
/// venue's other live accounts folded in.
pub(super) fn announce_oanda_arming(
    config: &MountPlacement,
    configs: &[&MountPlacement],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::ArmingAccount],
    mount_accounts: &[AccountLabel],
    data_only: &std::collections::HashSet<String>,
) {
    let execution_live = mounts_execution_live("oanda", mount_accounts, live_venues);
    let arming = with_other_live_accounts(
        if data_only.contains("oanda") && !execution_live {
            // The DECLARED exception: paper-by-declaration, not the gate disagreement the
            // ordinary remedy reports. Guarded on `!execution_live` so a build where the withhold
            // failed to hold still announces `EXEC IS LIVE` honestly.
            data_only_arming(oanda_arming(false), "oanda")
        } else {
            with_labelled_mount_remedy(oanda_arming(execution_live), "oanda", mount_accounts)
        },
        "oanda",
        other_live_accounts("oanda", venue_arming, live_venues, mount_accounts),
    );
    let message = match &arming.remedy {
        None => format!(
            "LIVE assemble_runtime core up; oanda feed subscribed (streamed L1 quotes + POLLED \
             candle closes, both Bearer-authed) — ⚠ EXEC IS LIVE ON {network}: orders \
             from this mount are REAL orders against the oanda {network} account whose \
             token was found",
            network = arming.network
        ),
        Some(remedy) => format!(
            "LIVE assemble_runtime core up; oanda feed subscribed (streamed L1 quotes + POLLED \
             candle closes, both Bearer-authed) — but EXEC IS NOT LIVE for oanda and NO \
             ORDER FROM THIS MOUNT WILL EVER REACH THE VENUE: {remedy}",
        ),
    };
    warn_arming_banner!(config.venue, config, configs, arming, live_venues, message);
}
