//! cTrader's feed arm: the daemon's own dedicated data socket, then the arming disclosure.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::DataClientSink;
use vike_marketdata::{Bar, L2Book, L2BookLevel, L2BookUpdate, TickQuote, TickTrade};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{StreamDelay, StreamStatus};
use vike_tradehub_core::MountPlacement;

use super::StrategyLaneFactories;
use super::live::{DataClientTracking, StrategyLaneQuote};
use super::price_tap::{DataClientSinkPriceTap, PriceTap};
use crate::venue_arming::arming::{
    ctrader_arming, data_only_arming, mounts_execution_live, other_live_accounts,
    with_labelled_mount_remedy, with_other_live_accounts,
};

/// **The sink in front of the mount's `TickSink`: it keeps a symbol other than the mount's OUT of the
/// mount's bar synthesizer.** A `TickSink` folds EVERY quote's mid into ITS token's synthesized bars
/// whatever symbol the quote names (`crates/vike-tradehub-core/src/run/sink.rs`'s `TickSink::quote`), and
/// a quote subscription for a symbol the control door admitted (decision 0174) delivers that symbol's
/// quotes through this same sink, so a foreign quote would corrupt the mounted symbol's paper-fill bars.
/// So a verb naming the MOUNT symbol goes to the `TickSink` untouched (the mounted path is
/// byte-identical to the one before any-symbol), and a verb naming any other symbol goes to the plain
/// core-lane sink, which forwards quote, trade, book and stream status to the core (the admitted
/// symbol's engine takes its mark there) and synthesizes nothing. The symbol a quote carries is the
/// broker's name for the id the subscription resolved, so it equals the mount's token exactly.
///
/// A BAR verb of the mount symbol at an interval other than the mounts' one synthesized window (an input's
/// higher timeframe, decision 0271: the venue's own trendbars, `crates/bridges/ctrader/src/feed/bar.rs`)
/// goes to the plain core lane too, where the core files it into the inputs; the `TickSink`'s bar verbs are
/// no-ops, so through it that input was never fed. At the mounts' own interval a bar verb still goes to the
/// `TickSink`.
struct MountScopedSink {
    mount: Arc<dyn DataClientSink>,
    foreign: Arc<dyn DataClientSink>,
    /// The symbols the mounts trade (every ctrader mount names the symbol its default account states).
    mounted: Vec<String>,
    /// The mounts' one synthesized window (`check_ctrader_intervals` refused two before the core spawned).
    interval: String,
}

impl MountScopedSink {
    fn pick(&self, symbol: &str) -> &dyn DataClientSink {
        if self.mounted.iter().any(|m| m == symbol) {
            self.mount.as_ref()
        } else {
            self.foreign.as_ref()
        }
    }

    /// [`Self::pick`] for a bar verb: another interval is never the mount's.
    fn pick_bars(&self, symbol: &str, interval: &str) -> &dyn DataClientSink {
        if interval == self.interval { self.pick(symbol) } else { self.foreign.as_ref() }
    }
}

impl DataClientSink for MountScopedSink {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.pick_bars(symbol, interval).seed_bars(venue, symbol, interval, bars);
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.pick_bars(symbol, interval).close_bar(venue, symbol, interval, bar);
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.pick_bars(symbol, interval).forming_bar(venue, symbol, interval, bar);
    }
    fn mark_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.pick(symbol).mark_tick(venue, symbol, px, ts);
    }
    fn bar_close_tick(&self, venue: &str, symbol: &str, px: f64, ts: i64) {
        self.pick(symbol).bar_close_tick(venue, symbol, px, ts);
    }
    fn l2_snapshot(
        &self,
        venue: &str,
        symbol: &str,
        tick_size: f64,
        bids: Vec<L2BookLevel>,
        asks: Vec<L2BookLevel>,
        ts: i64,
    ) {
        self.pick(symbol).l2_snapshot(venue, symbol, tick_size, bids, asks, ts);
    }
    fn quote(&self, venue: &str, symbol: &str, quote: TickQuote) {
        self.pick(symbol).quote(venue, symbol, quote);
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TickTrade) {
        self.pick(symbol).trade(venue, symbol, trade);
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        self.pick(symbol).book(venue, symbol, book);
    }
    fn book_update(&self, venue: &str, symbol: &str, update: L2BookUpdate) {
        self.pick(symbol).book_update(venue, symbol, update);
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.pick(symbol).stream_status(venue, symbol, stream, status);
    }
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: StreamDelay) {
        self.pick(symbol).data_delay(venue, symbol, stream, delay);
    }
}

/// cTrader's feed: the gatewayhub's dedicated data socket (its connect + auth is SYNCHRONOUS at the
/// open, and a failure refuses to start), with closed bars SYNTHESIZED from quote mids by a
/// `TickSink`, and one quote subscription per distinct symbol across this venue's mounts.
///
/// The feed is held as a [`StrategyLaneQuote`] (as alpaca's): a tap in front of the sinks records the last price
/// per symbol, and the [`StrategyLaneQuote::watcher`] subscribes the QUOTE lane of a symbol the control door
/// admits (decision 0174), so a foreign cTrader position has a mark. The data socket subscribes any
/// symbol its handshake listed at runtime (a quote key's `DataClientBusSpecHoldCtrader::ask`), on the one multiplexed
/// connection, so an admitted symbol costs a subscription and no socket; the venue's own subscription
/// cap is UNMEASURED.
pub(super) fn wire_ctrader_feed(
    config: &MountPlacement,
    configs: &[&MountPlacement],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn DataClientSink>) -> Arc<dyn DataClientSink>,
    make: &dyn StrategyLaneFactories,
) -> Result<StrategyLaneQuote, String> {
    // The feed is a credentialed stream at the gatewayhub, over its OWN dedicated data socket
    // (`connect_and_auth`, data-only: no `EventSender`), isolated from the exec socket cTrader's mount
    // (`crates/bridges/ctrader/src/adapter.rs`'s `VenueAdapterCtrader`) opens, the same
    // one-connection-per-plane rule every recon client follows. The sink is a `TickSink` (the polymarket
    // shape), NOT a bare `CoreLaneSink`: the mount's own closed bars are synthesized from quote mids at
    // the mount interval, and every ctrader mount shares its one window (`check_ctrader_intervals`
    // refused disagreement before the core spawned). The mount's own trendbars are deliberately NOT
    // subscribed: the venue's are bid-priced, and mixing them into a mid-priced synth series helps
    // nothing. A trendbar series the bar keeper opens for an INPUT (decision 0271; the bridge closes a
    // live trendbar when the next period begins, `crates/bridges/ctrader/src/feed/bar.rs`) is
    // another interval or symbol, and `MountScopedSink` hands it to the plain core lane.
    //
    // ⚠ MOUNT-FAILURE SEMANTICS, and they deliberately DIVERGE from exec: the handshake is SYNCHRONOUS
    // at the gatewayhub's open, so a connect/auth failure is the open's refusal, reaches this arm as the
    // first `subscribe_quotes`'s error text, and FAILS `live_runtime` — the daemon refuses
    // to start. The exec side demotes a failed ctrader connect to paper for the session
    // (a working paper mount is still a mount), but a live daemon whose FEED never
    // connected would hold a core that ticks nothing and watches nothing, the
    // silent-do-nothing this repo refuses to ship. Loud at startup beats dead at 3am.
    //
    // The rotation home (the project state directory, where a refreshed token pair is written back) is
    // the gatewayhub's now: it is the one writer of the rotated pair.
    let maker = Arc::new(vike_tradehub_core::TickSink::new(
        handle,
        config.venue.clone(),
        config.symbol.clone(),
        config.interval.clone(),
        config.interval_ms,
    )) as Arc<dyn DataClientSink>;
    // A symbol other than the mount's goes to the plain core lane, never into the mount's synth
    // (`MountScopedSink`); the tap in front only RECORDS the last price per symbol so the control door can
    // tell whether a symbol it admitted has a price yet (every verb is forwarded unchanged).
    let foreign = Arc::new(vike_tradehub_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn DataClientSink>;
    let scoped = Arc::new(MountScopedSink {
        mount: maker,
        foreign,
        mounted: configs.iter().map(|c| c.symbol.clone()).collect(),
        interval: config.interval.clone(),
    }) as Arc<dyn DataClientSink>;
    let tap = Arc::new(PriceTap::default());
    let sink =
        wrap(Arc::new(DataClientSinkPriceTap::new(scoped, Arc::clone(&tap)))
            as Arc<dyn DataClientSink>);
    let mut data = DataClientTracking::new(make.client("ctrader", None, sink)?);
    // One QUOTE subscription per distinct symbol (every ctrader mount was pinned to the
    // one stated symbol by `venue_feed_plan`, so this is one subscription today; the loop is
    // the same dedup shape as every other arm). The quote stream is the whole feed: it
    // drives `on_quote_tick` AND the bar synth above.
    let mut quote_keys: Vec<&str> = Vec::new();
    for c in configs {
        if !quote_keys.contains(&c.symbol.as_str()) {
            quote_keys.push(&c.symbol);
            // Retried on a transient and refused on a cap: this stream is the mount's bars too, so there is no
            // "runs on candle closes without quotes" here (`super::subscribe_quotes_surviving`) to fall back to.
            super::subscribe_quotes_retrying("ctrader", &mut data, &c.symbol, make).map_err(
                |refusal| {
                    format!(
                        "{refusal} — the daemon refuses to start rather than mount a feed-less \
                         live core; check the DEMO token pair and the gatewayhub's reach of the \
                         ctrader demo trade socket (`vike-cli config url ls --venue ctrader`), then \
                         restart"
                    )
                },
            )?;
        }
    }
    Ok(StrategyLaneQuote::new(data, tap))
}

/// cTrader's arming disclosure: exec LIVE or PAPER (or paper by `data_only` declaration), with the
/// venue's other live accounts folded in.
pub(super) fn announce_ctrader_arming(
    config: &MountPlacement,
    configs: &[&MountPlacement],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::ArmingAccount],
    mount_accounts: &[AccountLabel],
    data_only: &std::collections::HashSet<String>,
) {
    let execution_live = mounts_execution_live("ctrader", mount_accounts, live_venues);
    let arming = with_other_live_accounts(
        if data_only.contains("ctrader") && !execution_live {
            // Paper-by-declaration — here it REPLACES the "restart to retry the handshake"
            // remedy, which would be doubly false: no handshake failed, and a restart would
            // change nothing (the withhold is deliberate and repeats every boot).
            data_only_arming(ctrader_arming(false), "ctrader")
        } else {
            with_labelled_mount_remedy(ctrader_arming(execution_live), "ctrader", mount_accounts)
        },
        "ctrader",
        other_live_accounts("ctrader", venue_arming, live_venues, mount_accounts),
    );
    let message = match &arming.remedy {
        None => format!(
            "LIVE assemble_runtime core up; ctrader feed subscribed (spot quotes over the \
             dedicated data socket; closed bars SYNTHESIZED from quote mids) — ⚠ EXEC IS \
             LIVE ON {network}: orders from this mount are REAL orders against the \
             ctrader {network} account whose tokens were found",
            network = arming.network
        ),
        Some(remedy) => format!(
            "LIVE assemble_runtime core up; ctrader feed subscribed (spot quotes over the \
             dedicated data socket; closed bars SYNTHESIZED from quote mids) — but EXEC \
             IS NOT LIVE for ctrader and NO ORDER FROM THIS MOUNT WILL EVER REACH THE \
             VENUE: {remedy}",
        ),
    };
    warn_arming_banner!(config.venue, config, configs, arming, live_venues, message);
}

#[cfg(test)]
mod scoped_sink_tests {
    use std::sync::Mutex;

    use super::*;

    /// A sink that records `"<name>:<verb>:<symbol>"` for the verbs the door's price lane carries.
    struct Rec {
        name: &'static str,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Rec {
        fn note(&self, verb: &str, symbol: &str) {
            self.log.lock().unwrap().push(format!("{}:{verb}:{symbol}", self.name));
        }
    }

    impl DataClientSink for Rec {
        fn seed_bars(&self, _: &str, s: &str, _: &str, _: Vec<Bar>) {
            self.note("seed", s);
        }
        fn close_bar(&self, _: &str, s: &str, _: &str, _: Bar) {
            self.note("close", s);
        }
        fn forming_bar(&self, _: &str, s: &str, _: &str, _: Bar) {
            self.note("forming", s);
        }
        fn mark_tick(&self, _: &str, s: &str, _: f64, _: i64) {
            self.note("mark", s);
        }
        fn quote(&self, _: &str, s: &str, _: TickQuote) {
            self.note("quote", s);
        }
        fn trade(&self, _: &str, s: &str, _: TickTrade) {
            self.note("trade", s);
        }
        fn book(&self, _: &str, s: &str, _: Arc<L2Book>) {
            self.note("book", s);
        }
        fn stream_status(&self, _: &str, s: &str, _: &str, _: StreamStatus) {
            self.note("status", s);
        }
    }

    /// The sink of a `1h` mount on EURUSD over two recorders, `maker` and `core`.
    fn scoped(log: &Arc<Mutex<Vec<String>>>) -> MountScopedSink {
        MountScopedSink {
            mount: Arc::new(Rec { name: "maker", log: Arc::clone(log) }),
            foreign: Arc::new(Rec { name: "core", log: Arc::clone(log) }),
            mounted: vec!["EURUSD".to_string()],
            interval: "1h".to_string(),
        }
    }

    fn bar() -> Bar {
        Bar {
            timestamp: 0,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    fn quote() -> TickQuote {
        TickQuote {
            timestamp: 1,
            local_timestamp: 0,
            bid: 1.0,
            ask: 1.2,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: String::new(),
        }
    }

    /// **A foreign symbol never reaches the mount's synthesizer.** The mount's own verbs go to the
    /// `TickSink` stand-in exactly as before; every verb naming another symbol (a quote, a stream
    /// status, a mark) goes to the plain core lane stand-in and to nothing else.
    #[test]
    fn a_foreign_symbols_verbs_never_reach_the_mounts_sink() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = scoped(&log);
        sink.quote("ctrader", "EURUSD", quote());
        sink.quote("ctrader", "GBPUSD", quote());
        sink.mark_tick("ctrader", "GBPUSD", 1.1, 2);
        sink.stream_status(
            "ctrader",
            "GBPUSD",
            "quotes",
            StreamStatus::GapStart { at_timestamp_ms: 1 },
        );
        sink.stream_status(
            "ctrader",
            "EURUSD",
            "quotes",
            StreamStatus::Live { gap_started_timestamp_ms: None },
        );
        // A different case is another name, never the mount's.
        sink.quote("ctrader", "eurusd", quote());
        assert_eq!(
            *log.lock().unwrap(),
            [
                "maker:quote:EURUSD",
                "core:quote:GBPUSD",
                "core:mark:GBPUSD",
                "core:status:GBPUSD",
                "maker:status:EURUSD",
                "core:quote:eurusd",
            ]
        );
    }

    /// **An input's trendbars of the mount symbol at another interval reach the core lane** (decision 0271): the
    /// `TickSink`'s bar verbs are no-ops, so a `4h` input of a `1h` mount was never fed. At the mount's own
    /// interval a bar verb still goes to the `TickSink`, and another symbol's to the core lane at any interval.
    #[test]
    fn a_bar_verb_at_another_interval_goes_to_the_core_lane() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = scoped(&log);
        sink.seed_bars("ctrader", "EURUSD", "4h", vec![bar()]);
        sink.forming_bar("ctrader", "EURUSD", "4h", bar());
        sink.close_bar("ctrader", "EURUSD", "4h", bar());
        sink.seed_bars("ctrader", "EURUSD", "1h", vec![bar()]);
        sink.forming_bar("ctrader", "EURUSD", "1h", bar());
        sink.close_bar("ctrader", "EURUSD", "1h", bar());
        sink.close_bar("ctrader", "GBPUSD", "1h", bar());
        assert_eq!(
            *log.lock().unwrap(),
            [
                "core:seed:EURUSD",
                "core:forming:EURUSD",
                "core:close:EURUSD",
                "maker:seed:EURUSD",
                "maker:forming:EURUSD",
                "maker:close:EURUSD",
                "core:close:GBPUSD",
            ]
        );
    }
}
