//! Deribit's feed arm: the keyless four-verb `DataClient` built through `StrategyLaneFactories`, then the arming.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, DataClientSink};
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::MountPlacement;

use super::StrategyLaneFactories;
use super::live::StrategyLaneQuote;
use super::price_tap::PriceTap;
use crate::venue_arming::arming::{
    deribit_arming, mounts_execution_live, other_live_accounts, with_labelled_mount_remedy,
    with_other_live_accounts,
};

/// Deribit's feed: the keyless four-verb feed built through the [`StrategyLaneFactories`] seam on the
/// venue-agnostic `CoreLaneSink`, with a bar subscription per distinct `(symbol, interval)` and a
/// quote/trade/book triple per distinct symbol across this venue's mounts. Any subscription
/// failure refuses the mount.
pub(super) fn wire_deribit_feed(
    configs: &[&MountPlacement],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn DataClientSink>) -> Arc<dyn DataClientSink>,
    make: &dyn StrategyLaneFactories,
) -> Result<StrategyLaneQuote, String> {
    // The KEYLESS shape (the hyperliquid arm's, widened): one `DataClient` onto the
    // venue-agnostic `CoreLaneSink` (through `wrap` like every other arm), no
    // config to thread because nothing here authenticates.
    //
    // FOUR verbs, and each is subscribed because `vike_venues::venue_caps::DERIBIT`
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
    //              at `DataClientSink::book`, behind the maker's `on_order_book`.
    // ⚠ `subscribe_depth` is NOT wired here and must not be "added for completeness". The
    // VENUE serves it (the caps row declares `depth: true` since the datahub began serving
    // this venue's DOM, 2026-10-04), but this arm is a maker, and the conflating DOM lane
    // emits `DataClientSink::l2_snapshot`, a DEFAULT NO-OP in every sink this daemon owns (the
    // exact trap `StrategyLaneCex`'s doc records). The lossless `book` verb above is this arm's L2
    // surface.
    //
    // ⚠ MOUNT-FAILURE SEMANTICS are alpaca's, not ctrader's: constructing the dial does no network
    // work and each `subscribe_*` is acknowledged by the datahub, whose venue client dials with the
    // shared driver's stop-aware backoff, so an unreachable host surfaces as a reconnect-looping feed in
    // the log, never a mount error. Nothing SYNCHRONOUS exists here to fail but the strategy lane itself.
    //
    // The shared `feeds.rs` `dial` with the plain tap, the venue's price-only row's (`feeds.rs`'s
    // `PRICE_ONLY_ARMS`): a perpetual's last price, as Hyperliquid's arm. A book key the GUI or the recorder
    // also holds is one real subscription at the datahub's broker.
    let (mut feeds, tap) = super::dial("deribit", None, PriceTap::default, handle, wrap, make)?;
    // One BAR subscription per distinct (symbol, interval) across this venue's mounts —
    // each is its own chart socket, so two intervals genuinely both fire — and one
    // QUOTE/TRADE/BOOK triple per distinct symbol. The HL arm's dedup shape; every
    // interval here was proven mappable by `deribit_plan`.
    let mut bar_keys: Vec<(&str, &str)> = Vec::new();
    let mut tick_keys: Vec<&str> = Vec::new();
    for c in configs {
        if !bar_keys.contains(&(c.symbol.as_str(), c.interval.as_str())) {
            bar_keys.push((&c.symbol, &c.interval));
            feeds.subscribe_bars(&c.symbol, &c.interval).map_err(|e| {
                format!("deribit subscribe_bars {}@{}: {e:?}", c.symbol, c.interval)
            })?;
        }
        if !tick_keys.contains(&c.symbol.as_str()) {
            tick_keys.push(&c.symbol);
            feeds
                .subscribe_quotes(&c.symbol)
                .map_err(|e| format!("deribit subscribe_quotes {}: {e:?}", c.symbol))?;
            feeds
                .subscribe_trades(&c.symbol)
                .map_err(|e| format!("deribit subscribe_trades {}: {e:?}", c.symbol))?;
            feeds
                .subscribe_book(&c.symbol)
                .map_err(|e| format!("deribit subscribe_book {}: {e:?}", c.symbol))?;
        }
    }
    Ok(StrategyLaneQuote::new(feeds, tap))
}

/// Deribit's arming disclosure: exec LIVE or PAPER, with the venue's other live accounts folded
/// in. Takes no `data_only` set — the feed is keyless, so there is no declared data-only state.
pub(super) fn announce_deribit_arming(
    config: &MountPlacement,
    configs: &[&MountPlacement],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::ArmingAccount],
    mount_accounts: &[AccountLabel],
) {
    let arming = with_other_live_accounts(
        with_labelled_mount_remedy(
            deribit_arming(mounts_execution_live("deribit", mount_accounts, live_venues)),
            "deribit",
            mount_accounts,
        ),
        "deribit",
        other_live_accounts("deribit", venue_arming, live_venues, mount_accounts),
    );
    let message = match &arming.remedy {
        None => format!(
            "LIVE assemble_runtime core up; deribit feed subscribed (keyless MAINNET bars + \
             quotes + trades + L2 book) — ⚠ EXEC IS LIVE ON {network}: orders from this \
             mount are REAL orders against the deribit {network} account whose \
             credentials were found, priced off the MAINNET book this feed reads",
            network = arming.network
        ),
        Some(remedy) => format!(
            "LIVE assemble_runtime core up; deribit feed subscribed (keyless MAINNET bars + \
             quotes + trades + L2 book) — but EXEC IS NOT LIVE for deribit and NO ORDER FROM \
             THIS MOUNT WILL EVER REACH THE VENUE. To arm it: {remedy}",
        ),
    };
    warn_arming_banner!(config.venue, config, configs, arming, live_venues, message);
}
