//! cTrader's feed arm: the daemon's own dedicated data socket, then the arming disclosure.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_marketdata::{Bar, BookLevel, BookUpdate, L2Book, QuoteTick, TradeTick};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{DataDelay, StreamStatus};
use vike_tradehub_core::MakerConfig;

use super::FeedCtors;
use super::live::{QuoteFeed, Tracked};
use super::price_tap::{PriceTap, TapSink};
use crate::venue_arming::arming::{
    ctrader_arming, data_only_arming, mounts_exec_live, other_live_accounts,
    with_labelled_mount_remedy, with_other_live_accounts,
};

/// **The sink in front of the mount's `MakerSink`: it keeps a symbol other than the mount's OUT of the
/// mount's bar synthesizer.** A `MakerSink` folds EVERY quote's mid into ITS token's synthesized bars
/// whatever symbol the quote names (`crates/vike-tradehub-core/src/run/sink.rs`'s `MakerSink::quote`), and
/// a quote subscription for a symbol the control door admitted (decision 0174) delivers that symbol's
/// quotes through this same sink, so a foreign quote would corrupt the mounted symbol's paper-fill bars.
/// So a verb naming the MOUNT symbol goes to the `MakerSink` untouched (the mounted path is
/// byte-identical to the one before any-symbol), and a verb naming any other symbol goes to the plain
/// core-lane sink, which forwards quote, trade, book and stream status to the core (the admitted
/// symbol's engine takes its mark there) and synthesizes nothing. The symbol a quote carries is the
/// broker's name for the id the subscription resolved, so it equals the mount's token exactly.
struct MountScopedSink {
    mount: Arc<dyn LiveDataSink>,
    foreign: Arc<dyn LiveDataSink>,
    /// The symbols the mounts trade (every ctrader mount is pinned to the one wired symbol).
    mounted: Vec<String>,
}

impl MountScopedSink {
    fn pick(&self, symbol: &str) -> &dyn LiveDataSink {
        if self.mounted.iter().any(|m| m == symbol) {
            self.mount.as_ref()
        } else {
            self.foreign.as_ref()
        }
    }
}

impl LiveDataSink for MountScopedSink {
    fn seed_bars(&self, venue: &str, symbol: &str, interval: &str, bars: Vec<Bar>) {
        self.pick(symbol).seed_bars(venue, symbol, interval, bars);
    }
    fn close_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.pick(symbol).close_bar(venue, symbol, interval, bar);
    }
    fn forming_bar(&self, venue: &str, symbol: &str, interval: &str, bar: Bar) {
        self.pick(symbol).forming_bar(venue, symbol, interval, bar);
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
        bids: Vec<BookLevel>,
        asks: Vec<BookLevel>,
        ts: i64,
    ) {
        self.pick(symbol).l2_snapshot(venue, symbol, tick_size, bids, asks, ts);
    }
    fn quote(&self, venue: &str, symbol: &str, quote: QuoteTick) {
        self.pick(symbol).quote(venue, symbol, quote);
    }
    fn trade(&self, venue: &str, symbol: &str, trade: TradeTick) {
        self.pick(symbol).trade(venue, symbol, trade);
    }
    fn book(&self, venue: &str, symbol: &str, book: Arc<L2Book>) {
        self.pick(symbol).book(venue, symbol, book);
    }
    fn book_update(&self, venue: &str, symbol: &str, update: BookUpdate) {
        self.pick(symbol).book_update(venue, symbol, update);
    }
    fn stream_status(&self, venue: &str, symbol: &str, stream: &str, status: StreamStatus) {
        self.pick(symbol).stream_status(venue, symbol, stream, status);
    }
    fn data_delay(&self, venue: &str, symbol: &str, stream: &str, delay: DataDelay) {
        self.pick(symbol).data_delay(venue, symbol, stream, delay);
    }
}

/// cTrader's feed: the gatewayhub's dedicated data socket (its connect + auth is SYNCHRONOUS at the
/// open, and a failure refuses to start), with closed bars SYNTHESIZED from quote mids by a
/// `MakerSink`, and one quote subscription per distinct symbol across this venue's mounts.
///
/// The feed is held as a [`QuoteFeed`] (as alpaca's): a tap in front of the sinks records the last price
/// per symbol, and the [`QuoteFeed::watcher`] subscribes the QUOTE lane of a symbol the control door
/// admits (decision 0174), so a foreign cTrader position has a mark. The data socket subscribes any
/// symbol its handshake listed at runtime (`CtraderData::subscribe_quotes`), on the one multiplexed
/// connection, so an admitted symbol costs a subscription and no socket; the venue's own subscription
/// cap is UNMEASURED.
pub(super) fn wire_ctrader_feed(
    cfg: &MakerConfig,
    cfgs: &[&MakerConfig],
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<QuoteFeed, String> {
    // The feed is a credentialed stream at the gatewayhub, over its OWN dedicated data socket
    // (`connect_and_auth`, data-only: no `EventSender`), isolated from the exec socket cTrader's mount
    // (`crates/bridges/ctrader/src/adapter.rs`'s `CtraderVenueAdapter`) opens, the same
    // one-connection-per-plane rule every recon client follows. The sink is a `MakerSink` (the polymarket
    // shape), NOT a bare `CoreLaneSink`: cTrader's live trendbar lane only ever emits FORMING snapshots
    // through this client (`conn::on_inbound` — no close fires), so real venue bars can
    // never drive the per-CLOSED-bar watchdogs; the `MakerSink` synthesizes closed bars
    // from quote mids at the mount interval instead, and every ctrader mount shares its
    // one window (`check_ctrader_intervals` refused disagreement before the core
    // spawned). Trendbars are deliberately NOT subscribed — the sink would no-op their
    // seed/forming verbs anyway, and mixing bid-priced venue history into a mid-priced
    // synth series helps nothing.
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
    let maker = Arc::new(vike_tradehub_core::MakerSink::new(
        handle,
        cfg.venue.clone(),
        cfg.token_id.clone(),
        cfg.interval.clone(),
        cfg.interval_ms,
    )) as Arc<dyn LiveDataSink>;
    // A symbol other than the mount's goes to the plain core lane, never into the mount's synth
    // (`MountScopedSink`); the tap in front only RECORDS the last price per symbol so the control door can
    // tell whether a symbol it admitted has a price yet (every verb is forwarded unchanged).
    let foreign = Arc::new(vike_tradehub_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn LiveDataSink>;
    let scoped = Arc::new(MountScopedSink {
        mount: maker,
        foreign,
        mounted: cfgs.iter().map(|c| c.token_id.clone()).collect(),
    }) as Arc<dyn LiveDataSink>;
    let tap = Arc::new(PriceTap::default());
    let sink = wrap(Arc::new(TapSink::new(scoped, Arc::clone(&tap))) as Arc<dyn LiveDataSink>);
    let mut data = Tracked::new(make.client("ctrader", None, sink)?);
    // One QUOTE subscription per distinct symbol (every ctrader mount was pinned to the
    // one wired symbol by `ctrader_plan`, so this is one subscription today; the loop is
    // the same dedup shape as every other arm). The quote stream is the whole feed: it
    // drives `on_quote_tick` AND the bar synth above.
    let mut quote_keys: Vec<&str> = Vec::new();
    for c in cfgs {
        if !quote_keys.contains(&c.token_id.as_str()) {
            quote_keys.push(&c.token_id);
            data.subscribe_quotes(&c.token_id).map_err(|e| {
                format!(
                    "ctrader subscribe_quotes {}: {e:?} — the daemon refuses to start rather \
                     than mount a feed-less live core; check the DEMO token pair and the \
                     gatewayhub's reach of demo.ctraderapi.com, then restart",
                    c.token_id
                )
            })?;
        }
    }
    Ok(QuoteFeed::new(data, tap))
}

/// cTrader's arming disclosure: exec LIVE or PAPER (or paper by `data_only` declaration), with the
/// venue's other live accounts folded in.
pub(super) fn announce_ctrader_arming(
    cfg: &MakerConfig,
    cfgs: &[&MakerConfig],
    live_venues: &std::collections::HashSet<String>,
    venue_arming: &[vike_config::VenueArming],
    mount_accounts: &[AccountLabel],
    data_only: &std::collections::HashSet<String>,
) {
    let exec_live = mounts_exec_live("ctrader", mount_accounts, live_venues);
    let arming = with_other_live_accounts(
        if data_only.contains("ctrader") && !exec_live {
            // Paper-by-declaration — here it REPLACES the "restart to retry the handshake"
            // remedy, which would be doubly false: no handshake failed, and a restart would
            // change nothing (the withhold is deliberate and repeats every boot).
            data_only_arming(ctrader_arming(false), "ctrader")
        } else {
            with_labelled_mount_remedy(ctrader_arming(exec_live), "ctrader", mount_accounts)
        },
        "ctrader",
        other_live_accounts("ctrader", venue_arming, live_venues, mount_accounts),
    );
    match &arming.remedy {
        None => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE assemble_runtime core up; ctrader feed subscribed (spot quotes over the \
             dedicated data socket; closed bars SYNTHESIZED from quote mids) — ⚠ EXEC IS \
             LIVE ON {network}: orders from this mount are REAL orders against the \
             ctrader {network} account whose tokens were found",
            network = arming.network
        ),
        Some(remedy) => tracing::warn!(
            venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
            tick_size = cfg.tick_size, qty = cfg.qty, seed_cash = cfg.seed_cash,
            mounts = cfgs.len(),
            requote_lanes = arming.requote_lanes, quote_source = arming.quote_source,
            exec = arming.exec, network = arming.network, live_venues = ?live_venues,
            "LIVE assemble_runtime core up; ctrader feed subscribed (spot quotes over the \
             dedicated data socket; closed bars SYNTHESIZED from quote mids) — but EXEC \
             IS PAPER for ctrader and NO ORDER FROM THIS MOUNT WILL EVER REACH THE \
             VENUE: {remedy}",
        ),
    }
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

    impl LiveDataSink for Rec {
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
        fn quote(&self, _: &str, s: &str, _: QuoteTick) {
            self.note("quote", s);
        }
        fn trade(&self, _: &str, s: &str, _: TradeTick) {
            self.note("trade", s);
        }
        fn book(&self, _: &str, s: &str, _: Arc<L2Book>) {
            self.note("book", s);
        }
        fn stream_status(&self, _: &str, s: &str, _: &str, _: StreamStatus) {
            self.note("status", s);
        }
    }

    fn quote() -> QuoteTick {
        QuoteTick {
            ts: 1,
            local_ts: 0,
            bid: 1.0,
            ask: 1.2,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: String::new(),
        }
    }

    /// **A foreign symbol never reaches the mount's synthesizer.** The mount's own verbs go to the
    /// `MakerSink` stand-in exactly as before; every verb naming another symbol (a quote, a stream
    /// status, a mark) goes to the plain core lane stand-in and to nothing else.
    #[test]
    fn a_foreign_symbols_verbs_never_reach_the_mounts_sink() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = MountScopedSink {
            mount: Arc::new(Rec { name: "maker", log: Arc::clone(&log) }),
            foreign: Arc::new(Rec { name: "core", log: Arc::clone(&log) }),
            mounted: vec!["EURUSD".to_string()],
        };
        sink.quote("ctrader", "EURUSD", quote());
        sink.quote("ctrader", "GBPUSD", quote());
        sink.mark_tick("ctrader", "GBPUSD", 1.1, 2);
        sink.stream_status("ctrader", "GBPUSD", "quotes", StreamStatus::GapStart { at_ts_ms: 1 });
        sink.stream_status(
            "ctrader",
            "EURUSD",
            "quotes",
            StreamStatus::Live { gap_started_ts_ms: None },
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
}
