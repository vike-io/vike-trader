//! The DETERMINISTIC venue-feed splice test — scripted frames in, a paper fill out, no network,
//! the default CI lane (never `#[ignore]`d).
//!
//! `crates/vike-tradehub/tests/venue_feed_splice_smoke.rs` proves the splice LIVE (the shipped
//! binary against real Deribit mainnet, `#[ignore]`d, run explicitly); its module doc records why
//! a scripted CI test used to be structurally unavailable — reason (1): `wire_venue_feeds`
//! constructed each venue's feed object itself, so the only caller-injectable seam (`wrap`)
//! wrapped the SINK, never the stream. [`FeedCtors`] is the extraction that closed that reason,
//! and this module is the test it exists for.
//!
//! # What runs REAL here
//!
//! - the REAL [`super::venue_feed_plan`] deribit gate, over an EMPTY credential map (the keyless
//!   arm's own property);
//! - the REAL [`super::wire_venue_feeds`] deribit arm: it builds the sink chain
//!   (`wrap` over [`vike_core::CoreLaneSink`]) onto a REAL paper multi-mount core
//!   (`vike_run::build_paper_multi_strategy_core_with` — the exact mount seam
//!   `crates/vike-tradehub/tests/daemon/multi_mount_profile.rs` drives) and makes every
//!   `subscribe_*` call with the mount's own key;
//! - the REAL venue decode: each scripted lane feeds documented-grammar Deribit frames through
//!   `vike_deribit::market_data`'s own `parse_chart_bar`/`parse_quote` and the venue's
//!   [`BarFolder`] close inference (the successor bucket's first push IS the close signal) — the
//!   same functions the live `bars_main`/`quotes_main` lanes run;
//! - the REAL shared session driver: every scripted lane rides
//!   `vike_bridge_core::market_pump`'s `run_market_session` over a [`ScriptedStream`] (a real
//!   `MarketStream` impl), on a [`FeedRegistry`] thread with the production stop/join
//!   bookkeeping — the subscribe send and the frame loop are the production code path, not a
//!   hand-rolled iteration;
//! - the REAL core: `CoreLaneSink` → ingest lanes → the runtime dispatching on the mount's own
//!   `(venue, symbol, interval)` key → the mounted `buy_hold` submitting → the paper book
//!   filling.
//!
//! # What is SUBSTITUTED, and why that is not the smoke's "green fake"
//!
//! Exactly one thing: the venue's socket. The smoke's reason (3) refused substituting the whole
//! LANE — parse, fold, sink emission — behind a private seam, because that bypasses the labelling
//! the splice is about. Here the scripted lanes keep the venue's own labelling rule (emissions
//! carry the `"deribit"` const plus the SUBSCRIBED symbol — `quotes_main` and its siblings label
//! from the subscription, never from the frame) and serve frames FOR the subscribed channel, the
//! way the venue serves what was asked for. So a cross-labelled subscription in the arm (the
//! `cex_feed_wiring_pin.rs` failure class the smoke names) makes the scripted lane emit under the
//! wrong key, the core dispatches none of it to the mount, and the FILL assert fails — the exact
//! defect this file was red against when it was planted (`subscribe_bars` handed a foreign
//! literal instead of `c.token_id`).
//!
//! # What one green run does NOT prove
//!
//! The dial: hosts, TLS, reconnect/backoff against a real venue, the REST warmup seed — that
//! stays the smoke's job, which stays `#[ignore]`d and live. And one venue's arm is one venue's
//! arm: the other arms still rest on their `*_plan`/`*_arming` unit tests, the wiring text pins,
//! and the smoke.
//!
//! # The SECOND case: the OANDA data-only splice (the first credentialed-data conversion)
//!
//! The smoke's credentialed-data venues (alpaca/ctrader/oanda/ig) were structural GAPs: their
//! feed credentials arm exec from the same store, so no store could let the feed mount while a
//! validation run's exec stayed paper — and their live splices cannot even run on a weekend (FX
//! and equities close). The `data_only` profile declaration is the seam that unblocks both, and
//! [`a_data_only_oanda_mount_keeps_exec_paper_and_scripted_frames_reach_the_strategy`] is its
//! deterministic proof, one level DEEPER than the deribit case: it drives
//! [`super::live_mount_with`] — the REAL mount path (every venue's plan gate, the credential
//! WITHHOLD, the REAL `vike_run::build_live_multi_strategy_core` twelve-venue node, the REAL
//! wire arm) — over a FAKE-key map (never real credentials, never the real store) and a scripted
//! [`FeedCtors::oanda`] constructor, then asserts on `build_node`'s own `live_venues` record —
//! the exec ARMING STATE, not the banner — that the declaration kept exec paper. The fill assert
//! doubles as a BEHAVIOURAL paper-exec proof: a live exec client ignores the core's `on_bar`
//! fill seam (its fills arrive on a user-data stream this test never scripts), so only the paper
//! book can fill the scripted bars.
//!
//! What is substituted is the venue client behind [`FeedCtors::oanda`] (production: the real
//! `vike_oanda::market_feed::Feeds`); the scripted double keeps the venue's own decode
//! (`vike_oanda::parse_candles`, `vike_oanda::market_data::decode_pricing_frame`), its own
//! channel derivations (`to_oanda_instrument`/`granularity`), and its labelling rule (the
//! SUBSCRIBED series, never the frame's `EUR_USD` spelling), over documented-grammar frames —
//! the same anti-green-fake line the deribit case draws. The dial (fxPractice hosts, the Bearer
//! header, reconnect) stays the weekday live smoke's job.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_bridge_core::market_pump::{
    FrameOutcome, MarketPumpOpts, PumpBackoff, run_market_session,
};
use vike_bridge_core::scripted::ScriptedStream;
use vike_data::{DataClient, FeedRegistry, LiveDataError, LiveDataSink, SubscriptionId};
use vike_deribit::market_data::{
    chart_channel, parse_chart_bar, parse_quote, public_subscribe_frame, quote_channel,
};
use vike_deribit::market_feed::BarFolder;
use vike_run::{
    MakerMountConfig, PaperHalt, PaperMountOpts, StrategyMountSpec,
    build_paper_multi_strategy_core_with,
};

use super::{FeedCtors, venue_feed_plan, wire_venue_feeds};

/// The label every scripted lane stamps — the same const the real lanes stamp
/// (`crates/bridges/deribit/src/market_feed.rs`'s `VENUE`).
const VENUE: &str = "deribit";

/// One `chart.trades.*` push in the venue's documented wire shape (the grammar
/// `vike_deribit::market_data`'s own `CHART_FRAME` fixture pins; `tick` = bar-open ms). Built FOR
/// the given channel, the way the venue serves the channel that was subscribed — so a mutant arm
/// that subscribes the wrong series gets frames, and emissions, for THAT wrong series, and the
/// core dispatches none of them to the mount.
fn chart_frame(channel: &str, tick: i64, px: f64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","method":"subscription","params":{{"channel":"{channel}","data":{{"volume":0.25,"tick":{tick},"open":{px},"low":{px},"high":{px},"cost":1.0,"close":{px}}}}}}}"#
    )
}

/// One `quote.*` push in the venue's documented wire shape (the `QUOTE_FRAME` fixture's grammar):
/// both sides present — a one-sided quote is ignored by the real decoder, and this test wants the
/// decoder's ACCEPT path.
fn quote_frame(channel: &str, instrument: &str, ts: i64) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","method":"subscription","params":{{"channel":"{channel}","data":{{"timestamp":{ts},"instrument_name":"{instrument}","best_bid_price":99.5,"best_bid_amount":40.0,"best_ask_price":100.5,"best_ask_amount":50.0}}}}}}"#
    )
}

/// The scripted session's knobs: the venue's real subscribe frame, no watchdogs (a scripted
/// session has no wall-clock to age them against), and a backoff nothing reaches — each lane runs
/// ONE session, and script exhaustion ends it with the same `Closed` shape a real disconnect
/// takes (the [`ScriptedStream`] module doc's pin: that `Err` is expected, not a failure).
fn opts(subscribe: &str) -> MarketPumpOpts<'_> {
    MarketPumpOpts {
        subscribe: Some(subscribe),
        keepalive: None,
        ack_timeout: None,
        idle_threshold: None,
        read_timeout: Duration::from_millis(100),
        backoff: PumpBackoff::Fixed(Duration::from_millis(100)),
        connect_timeout: None,
    }
}

/// The bars lane, scripted: the venue's REAL resolution map, channel derivation, frame decode
/// ([`parse_chart_bar`]) and close inference ([`BarFolder`]) over one [`ScriptedStream`] session
/// — `bars_main`'s live half with the socket swapped for the script (and no REST warmup: the
/// runtime's `BarSeed` arm stores history and drives no strategy, so seeding would prove
/// nothing). Emissions are labelled exactly as the real lane labels them: the [`VENUE`] const
/// plus the SUBSCRIBED key. The pushes open ascending buckets, so the session closes enough bars
/// to both trigger the mounted `buy_hold` (its first event) and fill its market order (paper
/// market fills are next-bar).
fn scripted_bars_lane(
    symbol: String,
    interval: String,
    sink: Arc<dyn LiveDataSink>,
    stop: Arc<AtomicBool>,
) {
    let resolution = vike_deribit::data::resolution_code(&interval)
        .expect("the wired interval maps — deribit_plan proved it before the arm ran");
    let channel = chart_channel(&symbol, resolution);
    let frames: Vec<String> =
        (1..=4).map(|i| chart_frame(&channel, 60_000 * i, 100.0 + i as f64)).collect();
    let sub = public_subscribe_frame(&[channel]);
    let mut stream = ScriptedStream::from_texts(frames);
    let mut folder = BarFolder::new();
    let _ = run_market_session(
        &mut stream,
        &opts(&sub),
        &stop,
        &vike_model::now_ms,
        &mut |txt| {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(txt) else {
                return FrameOutcome::Ignore;
            };
            let Some(bar) = parse_chart_bar(&v) else {
                return FrameOutcome::Ignore;
            };
            let Some(roll) = folder.fold(bar) else {
                return FrameOutcome::Ignore;
            };
            if let Some(c) = roll.closed {
                sink.close_bar(VENUE, &symbol, &interval, c);
            }
            sink.forming_bar(VENUE, &symbol, &interval, roll.forming);
            FrameOutcome::Confirm
        },
        &mut || {},
    );
}

/// The quotes lane, scripted: the venue's real channel derivation and frame decode
/// ([`parse_quote`]) over one session — `quotes_main`'s live half, same labelling rule (the
/// subscribed symbol, never the frame's), same receive-time stamp.
fn scripted_quotes_lane(symbol: String, sink: Arc<dyn LiveDataSink>, stop: Arc<AtomicBool>) {
    let channel = quote_channel(&symbol);
    let frames = vec![quote_frame(&channel, &symbol, 60_000)];
    let sub = public_subscribe_frame(&[channel]);
    let mut stream = ScriptedStream::from_texts(frames);
    let _ = run_market_session(
        &mut stream,
        &opts(&sub),
        &stop,
        &vike_model::now_ms,
        &mut |txt| {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(txt) else {
                return FrameOutcome::Ignore;
            };
            let Some(mut q) = parse_quote(&v) else {
                return FrameOutcome::Ignore;
            };
            q.local_ts = vike_model::now_ms();
            sink.quote(VENUE, &symbol, q);
            FrameOutcome::Confirm
        },
        &mut || {},
    );
}

/// The scripted stand-in [`FeedCtors::deribit`] hands the REAL arm: records every subscribe the
/// arm makes (the LABEL half of the splice) and serves the bar + quote lanes one scripted session
/// each of documented-grammar frames through the venue's own public decode (the DATA half).
/// Lanes run on [`FeedRegistry`] threads exactly like the real `Feeds` (same spawn/stop/join
/// bookkeeping), so the teardown the arm's caller performs is the production path too.
struct ScriptedDeribitFeed {
    sink: Arc<dyn LiveDataSink>,
    subs: Arc<Mutex<Vec<(&'static str, String)>>>,
    registry: FeedRegistry,
}

impl ScriptedDeribitFeed {
    fn record(&self, verb: &'static str, key: String) {
        self.subs.lock().expect("subs record").push((verb, key));
    }

    fn spawn(
        &mut self,
        name: String,
        body: impl FnOnce(Arc<AtomicBool>) + Send + 'static,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.registry.spawn(name, body).map_err(|e| LiveDataError::Subscribe(e.to_string()))
    }
}

impl DataClient for ScriptedDeribitFeed {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.record("bars", format!("{symbol}@{interval}"));
        let sink = Arc::clone(&self.sink);
        let (symbol, interval) = (symbol.to_string(), interval.to_string());
        self.spawn(format!("scripted-deribit-{symbol}@{interval}"), move |stop| {
            scripted_bars_lane(symbol, interval, sink, stop)
        })
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.record("quotes", symbol.to_string());
        let sink = Arc::clone(&self.sink);
        let symbol = symbol.to_string();
        self.spawn(format!("scripted-deribit-{symbol}-quotes"), move |stop| {
            scripted_quotes_lane(symbol, sink, stop)
        })
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Label-recorded, no scripted frames: prints feed the `PriceBoard`'s last-trade rung and
        // drive no strategy verb, so the fill assert gains nothing from scripting them — the
        // trades decode is `vike_deribit::market_data`'s own fixture suite's job. The subscribe
        // LABEL is still the arm's own choice and still asserted.
        self.record("trades", symbol.to_string());
        self.spawn(format!("scripted-deribit-{symbol}-trades"), |_stop| {})
    }

    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Same verdict as trades: `buy_hold` has no `on_order_book`, and the book fold's
        // invariants are `market_data_conformance.rs`'s job. Label recorded, asserted below.
        self.record("book", symbol.to_string());
        self.spawn(format!("scripted-deribit-{symbol}-book"), |_stop| {})
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.registry.stop_join(id);
    }

    fn begin_shutdown(&mut self) {
        self.registry.raise_stops();
    }

    fn shutdown(&mut self) {
        self.registry.shutdown();
    }
}

/// The test's [`FeedCtors`] impl: the deribit method hands back the scripted double; every other
/// venue keeps the trait's production default (this test never reaches one — its plan is
/// deribit's).
struct ScriptedCtors {
    subs: Arc<Mutex<Vec<(&'static str, String)>>>,
}

impl FeedCtors for ScriptedCtors {
    fn deribit(&self, sink: Arc<dyn LiveDataSink>) -> Box<dyn DataClient + Send> {
        Box::new(ScriptedDeribitFeed {
            sink,
            subs: Arc::clone(&self.subs),
            registry: FeedRegistry::new(),
        })
    }
}

/// A HALT sentinel path THIS TEST owns and never creates — the same pinning
/// `crates/vike-tradehub/tests/daemon/multi_mount_profile.rs`'s `own_sentinel` documents: a paper
/// mount is HALT-armed by design, so a test expecting fills must not inherit the operator's kill
/// switch off whatever box runs it.
fn own_sentinel() -> PathBuf {
    std::env::temp_dir()
        .join(format!("vike-tradehub-feed-splice-seam-{}", std::process::id()))
        .join("HALT")
}

fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// THE SPLICE, deterministically: scripted Deribit frames through the REAL `wire_venue_feeds`
/// deribit arm reach the core labelled for the mount's dispatch key, and the mounted strategy
/// acts. The fill is the assert because this core has no other feed and no other order source —
/// so a fill in the `(deribit, wired-symbol)` paper book is EQUIVALENT to "a scripted venue
/// event crossed sink → core lane → the mount's own key → `buy_hold` → the paper book" (the
/// smoke's own argument, made deterministic). Red-proven against the planted cross-label mutant
/// (the arm's `subscribe_bars` handed a foreign literal): the scripted lane then emits under the
/// wrong key, no bar reaches the mount or its paper book, and the fill wait times out.
#[test]
fn a_scripted_deribit_frame_reaches_the_mounted_strategy_through_the_arms_own_wiring() {
    let symbol = super::wired_symbol_for("deribit").expect("build_node mounts deribit");
    let mut cfg = MakerMountConfig::crypto("deribit", symbol, 0.5, 10.0);
    cfg.interval = "1m".to_string();
    cfg.interval_ms = 60_000;

    // The REAL gate, over an EMPTY credential map — the keyless arm's own property.
    // `hyperliquid_mainnet: false` is inert here — this call exercises the deribit arm, which never
    // reads it.
    let plan = venue_feed_plan(&cfg, &HashMap::new(), false)
        .expect("keyless deribit plans on an empty store");

    // The paper-seam mount (multi_mount_profile.rs's shape): `buy_hold` on the deribit dispatch
    // key, HALT pinned to a sentinel this test owns.
    let sentinel = own_sentinel();
    assert!(
        !sentinel.exists(),
        "the pinned sentinel must NOT exist, or the mount refuses opening orders: {}",
        sentinel.display()
    );
    let strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send> =
        Box::new(vike_strategy::BuyHold::new(0.001, None));
    let mount = build_paper_multi_strategy_core_with(
        vec![StrategyMountSpec { strategy, spec: cfg.mount_spec() }],
        PaperMountOpts { halt: PaperHalt::Pinned(sentinel), ..Default::default() },
    );

    // The REAL arm, over the scripted constructor. `wrap` is the identity — exactly
    // `live_mount`'s no-recording default.
    let subs: Arc<Mutex<Vec<(&'static str, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let ctors = ScriptedCtors { subs: Arc::clone(&subs) };
    let wrap = |sink: Arc<dyn LiveDataSink>| sink;
    let live_venues: HashSet<String> = HashSet::new();
    // No `data_only` declaration — deribit's data plane is keyless, and the empty set is every
    // undeclared mount's value (the byte-identical default this test keeps pinning).
    let data_only: HashSet<String> = HashSet::new();
    let mut feeds = wire_venue_feeds(
        &plan,
        &[&cfg],
        &mount.handle,
        &live_venues,
        &[],
        &data_only,
        &wrap,
        &ctors,
    )
    .expect("the deribit arm subscribes a scripted feed without error");

    // THE SPLICE. Deliberately FIRST, before the label asserts below: this is the behavioural
    // half, and it must be the one that fails on a cross-labelled arm (a declaration check alone
    // would be the pin-without-a-gate shape this repo distrusts).
    let book = mount
        .fills
        .iter()
        .find(|((v, s), _)| v == "deribit" && s == symbol)
        .map(|(_, f)| Arc::clone(f))
        .expect("the paper mount built a (deribit, wired-symbol) book");
    assert!(
        wait_until(10, || !book.lock().expect("fills").is_empty()),
        "no paper fill within 10s: no scripted venue event reached the mounted strategy through \
         the arm's own wiring (subscriptions the arm made: {:?})",
        subs.lock().expect("subs")
    );

    // The LABEL half: the arm subscribed THIS mount's series — every declared verb, the mount's
    // own key, once each (the per-series dedup rule; one mount ⇒ one of each; `subscribe_depth`
    // deliberately absent, the caps refusal the arm documents).
    let got = subs.lock().expect("subs").clone();
    let expect: Vec<(&'static str, String)> = vec![
        ("bars", format!("{symbol}@1m")),
        ("quotes", symbol.to_string()),
        ("trades", symbol.to_string()),
        ("book", symbol.to_string()),
    ];
    assert_eq!(got, expect, "the arm's subscription set IS the mount's own dispatch key");

    // Teardown through the arm's own handle — the production `shutdown` dispatch.
    feeds.shutdown();
    mount.handle.shutdown_and_join();
}

// ─── The OANDA data-only case (the first credentialed-data GAP conversion) ────────────────────

/// The label the scripted oanda lanes stamp — the same const the real lanes stamp
/// (`crates/bridges/oanda/src/market_feed.rs`'s `VENUE`).
const OANDA_VENUE: &str = "oanda";

/// One wire-faithful `candles` response (UNIX datetime format, `price=M` midpoints — the grammar
/// `vike_oanda::parse_candles`' own fixtures pin): `n` COMPLETE ascending 1m candles from
/// `first_bucket_s`, plus the still-forming tail candle a real response always carries (dropped
/// by the lossless-lane decode, kept here for wire fidelity). Prices are decimal STRINGS and
/// `time` epoch-seconds strings, exactly as the venue serves them.
fn candles_response(first_bucket_s: i64, n: i64) -> serde_json::Value {
    let mut candles: Vec<serde_json::Value> = (0..n)
        .map(|i| {
            let px = format!("{:.5}", 1.09 + 0.0001 * i as f64);
            serde_json::json!({
                "complete": true,
                "volume": 120,
                "time": format!("{}.000000000", first_bucket_s + 60 * i),
                "mid": { "o": px, "h": px, "l": px, "c": px }
            })
        })
        .collect();
    candles.push(serde_json::json!({
        "complete": false,
        "volume": 7,
        "time": format!("{}.000000000", first_bucket_s + 60 * n),
        "mid": { "o": "1.09100", "h": "1.09100", "l": "1.09100", "c": "1.09100" }
    }));
    serde_json::json!({ "instrument": "EUR_USD", "granularity": "M1", "candles": candles })
}

/// One wire-faithful pricing-stream `PRICE` line (the grammar
/// `vike_oanda::market_data`'s own `price_frame` fixture pins): two-sided, best-first ladders,
/// UNIX datetime format — the decoder's ACCEPT path.
fn oanda_price_line() -> String {
    r#"{"type":"PRICE","time":"60.500000000","instrument":"EUR_USD","bids":[{"price":"1.09000","liquidity":10000000}],"asks":[{"price":"1.09010","liquidity":10000000}],"status":"tradeable","tradeable":true}"#
        .to_string()
}

/// The scripted stand-in [`FeedCtors::oanda`] hands the REAL arm: records every subscribe the
/// arm makes (the LABEL half) and serves each lane one batch of documented-grammar frames
/// through the venue's own decode (the DATA half), on [`FeedRegistry`] threads with the
/// production stop/join bookkeeping. The venue's real derivations run too —
/// `vike_oanda::granularity` must map the subscribed interval and
/// `vike_oanda::to_oanda_instrument` names the wire instrument — so the scripted frames
/// are FOR the subscribed series the way the venue serves what was asked for, and the emissions
/// keep the real lanes' labelling rule: the [`OANDA_VENUE`] const plus the SUBSCRIBED series,
/// never the frame's own `EUR_USD` spelling.
struct ScriptedOandaFeed {
    sink: Arc<dyn LiveDataSink>,
    subs: Arc<Mutex<Vec<(&'static str, String)>>>,
    registry: FeedRegistry,
}

impl DataClient for ScriptedOandaFeed {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        self.subs.lock().expect("subs").push(("bars", format!("{symbol}@{interval}")));
        // The venue's REAL derivations — the same calls `bars_main`'s poll makes. `granularity`
        // was proven mappable by `oanda_plan` before the arm ran; a double that skipped it could
        // serve bars for an interval the venue has no candle series for.
        let gran = vike_oanda::granularity(interval)
            .expect("the wired interval maps — oanda_plan proved it before the arm ran");
        assert_eq!(gran, "M1", "this scripted lane serves 1m candles");
        let instrument = vike_oanda::to_oanda_instrument(symbol);
        assert_eq!(instrument, "EUR_USD", "the wired symbol lowers to the venue instrument form");
        let sink = Arc::clone(&self.sink);
        let (series, interval) = (symbol.to_string(), interval.to_string());
        self.registry
            .spawn(format!("scripted-oanda-{series}@{interval}"), move |_stop| {
                // The venue's REAL lossless-lane decode over the wire-faithful response, emitted
                // exactly as `bars_main` emits fresh closes (no seed: the runtime's `BarSeed` arm
                // stores history and drives no strategy, the deribit case's verdict). Four closes
                // are enough to trigger the mounted `buy_hold` (its first event) AND fill its
                // market order (paper fills are next-bar, through the core's `on_bar` seam).
                for b in vike_oanda::parse_candles(&candles_response(60, 4)) {
                    sink.close_bar(OANDA_VENUE, &series, &interval, b);
                }
            })
            .map_err(|e| LiveDataError::Subscribe(e.to_string()))
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.subs.lock().expect("subs").push(("quotes", symbol.to_string()));
        let sink = Arc::clone(&self.sink);
        let series = symbol.to_string();
        self.registry
            .spawn(format!("scripted-oanda-{series}-quotes"), move |_stop| {
                // The venue's REAL pricing decode + the real lane's labelling rule: relabel with
                // the SUBSCRIBED series and stamp receive time (`quotes_main`'s fold does both).
                let v: serde_json::Value =
                    serde_json::from_str(&oanda_price_line()).expect("wire-faithful PRICE line");
                match vike_oanda::market_data::decode_pricing_frame(&v) {
                    vike_oanda::market_data::PricingFrame::Quote(mut q) => {
                        q.symbol = series.clone();
                        q.local_ts = vike_model::now_ms();
                        sink.quote(OANDA_VENUE, &series, q);
                    }
                    other => panic!("the fixture must decode as a quote, got {other:?}"),
                }
            })
            .map_err(|e| LiveDataError::Subscribe(e.to_string()))
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // The real client's caps refusal (`live_data.trades = false` — no trade tape exists on
        // this venue), kept so an arm padded out to "look like its neighbours" fails the mount
        // loudly here exactly as it would in production.
        let _ = symbol;
        Err(LiveDataError::Unsupported("oanda serves no trade tape"))
    }

    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        // Same caps refusal as trades (`live_data.book = false` — an unsequenced snapshot ladder
        // is not an L2 book).
        let _ = symbol;
        Err(LiveDataError::Unsupported("oanda serves no L2 book"))
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.registry.stop_join(id);
    }

    fn begin_shutdown(&mut self) {
        self.registry.raise_stops();
    }

    fn shutdown(&mut self) {
        self.registry.shutdown();
    }
}

/// The data-only test's [`FeedCtors`] impl: the oanda method hands back the scripted double AND
/// records the credentials the REAL arm handed it — the feed half of the declaration's contract
/// (the plan resolved the store's keys BEFORE the withhold took them from exec). Every other
/// venue keeps the trait's production default (this test never reaches one — its plan is
/// oanda's).
struct ScriptedOandaCtors {
    subs: Arc<Mutex<Vec<(&'static str, String)>>>,
    got_creds: Arc<Mutex<Option<(String, String)>>>,
}

impl FeedCtors for ScriptedOandaCtors {
    fn oanda(
        &self,
        config: &vike_oanda::OandaConfig,
        sink: Arc<dyn LiveDataSink>,
    ) -> Box<dyn DataClient + Send> {
        *self.got_creds.lock().expect("got_creds") =
            Some((config.api_token.clone(), config.account_id.clone()));
        Box::new(ScriptedOandaFeed {
            sink,
            subs: Arc::clone(&self.subs),
            registry: FeedRegistry::new(),
        })
    }
}

/// A machine policy whose per-venue ARMING CEILING permits oanda, and which sets nothing else.
///
/// ⚠ **`Policy::default()` would make both tests below pass for the wrong reason.** That default
/// caps EVERY venue at `paper`, and a capped venue returns the paper engine above the arm entirely
/// — so the mutant these tests hunt (a build whose `data_only` declaration silently arms exec
/// anyway) would die on the ceiling rather than on the withhold, and the `live_venues.is_empty()`
/// record would prove nothing about the declaration. Arming oanda is what leaves `data_only` as the
/// only thing standing between the fake credentials and a live exec client.
fn oanda_armed_policy() -> vike_config::Policy {
    vike_config::Policy {
        venues: vike_config::VenuePolicy::default().declare("oanda", vike_config::VenueMode::Demo),
        ..vike_config::Policy::default()
    }
}

/// THE DATA-ONLY SEAM, deterministically, over the REAL mount path: a `data_only = true` oanda
/// profile + a FAKE-key credentialed map through [`super::live_mount_with`] — every venue's
/// plan gate, the credential WITHHOLD, the real twelve-venue `build_node`, the real oanda wire
/// arm — must (1) keep exec on the paper fallback, asserted against `build_node`'s own
/// `live_venues` ARMING RECORD (the state `vike_mount::make_engine` writes when it constructs a
/// real exec client — never the banner), (2) hand the FEED the store's credentials (the ctor
/// records what the plan resolved), and (3) still splice: scripted venue frames through the
/// arm's own wiring reach the mounted strategy, whose order the PAPER book fills — a live exec
/// client ignores the core's `on_bar` fill seam, so the fill is behavioural proof of (1) on top
/// of the record.
///
/// The arming assert comes FIRST: it is the seam's core property, and the mutant it exists to
/// catch — a build where the declaration silently arms exec anyway — must fail HERE, on the
/// record, before any timing-dependent wait can muddy the verdict. (Under that mutant the fake
/// keys would spawn a real exec client whose auth then fails against the practice host — the
/// fill wait would eventually fail too, but the record fails first and names the venue.)
///
/// Residual this test accepts, shared with every live-path paper mount: `make_engine`'s paper
/// client resolves the process-wide HALT sentinel (`vike_bridge_core::halt`'s
/// `halt_path_from_env`), so a box with an armed kill switch at the resolved path would refuse
/// the fill — the CI runners' checkouts carry none, and a HALT file there would be halting the
/// box's real daemons first.
#[test]
fn a_data_only_oanda_mount_keeps_exec_paper_and_scripted_frames_reach_the_strategy() {
    let symbol = super::wired_symbol_for("oanda").expect("build_node mounts oanda");

    // The operator's own spelling of the seam — parsed + validated through the daemon's real
    // profile load path, so this test also covers the declaration's parse.
    let toml = format!(
        "venue = \"oanda\"\nsymbol = \"{symbol}\"\ninterval = \"1m\"\ninterval_ms = 60000\n\
         data_only = true\n\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\nsize = 1.0\n"
    );
    let row = crate::config::DaemonProfile::from_toml_str(&toml)
        .expect("the data-only profile parses and validates");
    row.validate_for_live().expect("…and passes the pure live gate");
    let cfg = row.to_mount_config();
    let spec = row.to_mount_spec();
    let strategy = row.resolve_strategy(&cfg).expect("buy_hold resolves from the registry");
    let mounts = vec![super::ResolvedMount { row, cfg, spec, strategy }];

    // The credentialed store: FAKE keys in a plain map — never real ones, never the real store.
    // Spelled through the venue's own key-name authority so a key-grid rename reddens here.
    let (key_k, acct_k) = vike_oanda::oanda_env_var_names(vike_bridge_core::Environment::Demo);
    let vars: HashMap<String, String> = HashMap::from([
        (key_k, "fake-data-only-token".to_string()),
        (acct_k, "101-004-0000000-001".to_string()),
    ]);

    let subs: Arc<Mutex<Vec<(&'static str, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let got_creds: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
    let ctors = ScriptedOandaCtors { subs: Arc::clone(&subs), got_creds: Arc::clone(&got_creds) };

    // An operator risk BUDGET is supplied — not because the green path needs one (every venue is
    // paper, and `require_live_risk_budget` asks nothing of a paper mount), but so the MUTANT
    // this test exists to catch dies on the ARMING ASSERT below and not one gate earlier: a
    // build whose declaration silently arms exec would otherwise refuse the whole mount at
    // `vike_mount::require_live_risk_budget` (no budget for a live venue), which is a catch, but
    // of the wrong property — proven live on the CI box before this budget was added (the mutant's
    // first red was `MissingRiskBudget`, not the record assert). Budget fields only; the
    // venue-owned grid fields stay `None`, so the paper engines' `VenueFetched` merge is
    // untouched and the caps sit far above the scripted 1-unit fill.
    let budget = vike_exec::ProfileRisk {
        max_notional_per_order: Some(1_000_000.0),
        max_total_exposure: Some(10_000_000.0),
        ..vike_exec::ProfileRisk::default()
    };

    let lock_dir = tempfile::tempdir().expect("a throwaway state dir for the B11 lock claims");
    let (handle, mut teardown, live_venues, live_locks) = super::live_mount_with(
        mounts,
        Some(budget),
        None, // no run profile: this seam asserts the withhold, not the [guards] wiring
        &oanda_armed_policy(),
        vike_config::Flags::default(),
        vars,
        lock_dir.path(),
        &ctors,
    )
    .expect("the data-only live mount stands up — no real store, no network");

    // (1) THE ARMING STATE — the seam's core property, asserted on the record itself.
    assert!(
        live_venues.is_empty(),
        "the `data_only` declaration must keep EVERY venue on the paper fallback — build_node \
         recorded a live exec client for {live_venues:?}, so the withhold did not hold"
    );

    // (1b) …AND NO ACCOUNT LOCK WAS CLAIMED FOR IT. The B11 claims read the map AFTER the
    // withhold, so a `data_only` venue — which this daemon deliberately leaves on paper — must not
    // take the sentinel that refuses a second live process on that account. Asserted on the
    // sentinel FILES as well as on the count: a claim's only trace is the file it creates, and the
    // count alone would still pass a build that claimed and then dropped.
    assert!(
        live_locks.is_empty(),
        "a `data_only` venue arms nothing, so it must claim no live-account lock — {} claimed",
        live_locks.len()
    );
    let sentinels: Vec<String> = std::fs::read_dir(lock_dir.path())
        .expect("the throwaway state dir is readable")
        .map(|e| e.expect("dir entry").file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(vike_ops::live_lock::LIVE_LOCK_PREFIX))
        .collect();
    assert!(
        sentinels.is_empty(),
        "no LIVE-<venue>.lock may exist for a mount that armed nothing: {sentinels:?}"
    );

    // (2) The feed half: the arm handed the constructor the credentials the plan resolved from
    // the store — the same keys the withhold took away from exec.
    assert_eq!(
        got_creds.lock().expect("got_creds").clone(),
        Some(("fake-data-only-token".to_string(), "101-004-0000000-001".to_string())),
        "the feed must authenticate with the store's own credentials — the plan resolved them \
         BEFORE the withhold, and the arm must thread them through"
    );

    // (3) THE SPLICE + the paper-exec behaviour: scripted candle closes through the real arm
    // reach the mounted `buy_hold` on its own dispatch key, and the PAPER book fills its order
    // through the core's `on_bar` seam.
    let filled = wait_until(10, || {
        handle
            .snapshot()
            .orders
            .iter()
            .any(|o| o.venue == "oanda" && o.symbol == symbol && o.filled_qty > 0.0)
    });
    assert!(
        filled,
        "no filled oanda order within 10s: no scripted venue event crossed the arm's own wiring \
         into the paper book (subscriptions the arm made: {:?}; orders seen: {:?})",
        subs.lock().expect("subs"),
        handle.snapshot().orders
    );

    // The LABEL half: the arm subscribed THIS mount's series — the two verbs the venue serves,
    // the mount's own key, once each (trades/book are caps refusals the arm must not call).
    let got = subs.lock().expect("subs").clone();
    let expect: Vec<(&'static str, String)> =
        vec![("bars", format!("{symbol}@1m")), ("quotes", symbol.to_string())];
    assert_eq!(got, expect, "the arm's subscription set IS the mount's own dispatch key");

    // Teardown through the production handles: every feed joined, the live-event forwarder
    // stopped, then the core — `main`'s order, without the deadline scaffolding.
    for f in &mut teardown.feeds {
        f.shutdown();
    }
    teardown.forwarder_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    handle.shutdown_and_join();
}

/// **THE READY BANNER IS THE MOUNT'S RECORD, NOT THE PROFILE'S — over the REAL mount path.**
///
/// [`super::ready_mode_line`]'s own unit tests prove the RENDERING (a nine-venue record renders
/// nine venues, sorted). What they cannot reach is the value it is rendered FROM, and that is where
/// the defect lived: the banner was built from the daemon's `mount_venues`, the DISTINCT venues the
/// profile mounts a strategy on, while `vike_run::build_node` arms a real exec client wherever the
/// CREDENTIAL STORE answers — a set the profile does not decide. MEASURED on the CI box, one startup:
/// `live_venues={…nine venues…}` under `"mode":"LIVE (venue=bybit)"`.
///
/// So this drives the REAL [`super::live_mount_with`] — the same call the daemon makes — with a
/// profile naming exactly ONE venue, and asserts the banner rendered from the returned ARMING
/// RECORD does not name it. The two sets genuinely differ here: the profile's is `{oanda}` and the
/// record is EMPTY (the `data_only` declaration withheld the store's keys from exec), so a banner
/// built from the profile says `LIVE (venue=oanda)` and a banner built from the record says
/// `LIVE (venue=none)`. Under the pre-fix binary this test reads the first.
///
/// ⚠ **Why the record is empty rather than the several-venue shape the CI box measured, and why that is
/// not a weakening.** The direction under test is "the banner follows the record, whatever the
/// profile says", and any DIFFERENCE between the two sets tests it. A several-venue record cannot
/// be produced hermetically: every arming branch in `vike_mount::make_engine_with_legs` performs
/// venue I/O on the way to its `live_venues.insert` — a blocking `SymbolProperties` pre-fetch
/// (bybit/okx/binance/alpaca/ibkr), a synchronous authed handshake (ctrader/ibkr/deribit), or an
/// exec actor that dials on spawn (alpaca/ig/oanda) — so "arm several venues" and "touch no network
/// with fake keys" are mutually exclusive in this process. The N-venue half is carried by
/// `the_banner_names_every_armed_venue_not_the_one_the_profile_mounts`, over the the CI box record
/// verbatim; the two together cover both halves of the claim.
#[test]
fn the_ready_banner_names_the_arming_record_not_the_venue_the_profile_mounts() {
    let symbol = super::wired_symbol_for("oanda").expect("build_node mounts oanda");

    // The PRECONDITION, keyed on the profile — not on the renderer, and not on the record the
    // assertions below are about. If a future edit made this profile mount some other venue (or no
    // venue), the test's whole premise would be gone and it must FAIL saying so, never pass quietly
    // because the string it was hunting for happened to be absent.
    let toml = format!(
        "venue = \"oanda\"\nsymbol = \"{symbol}\"\ninterval = \"1m\"\ninterval_ms = 60000\n\
         data_only = true\n\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\nsize = 1.0\n"
    );
    let row = crate::config::DaemonProfile::from_toml_str(&toml)
        .expect("the single-mount profile parses and validates");
    row.validate_for_live().expect("…and passes the pure live gate");
    let cfg = row.to_mount_config();
    assert_eq!(cfg.venue, "oanda", "this test's premise is a profile that mounts exactly oanda");
    assert!(
        vike_model::VENUES.contains(&cfg.venue.as_str()),
        "the profile's venue must be a real roster id, or `LIVE (venue=oanda)` was never a shape \
         the pre-fix banner could print and this test proves nothing"
    );
    let spec = row.to_mount_spec();
    let strategy = row.resolve_strategy(&cfg).expect("buy_hold resolves from the registry");
    let profile_venue = cfg.venue.clone();
    let mounts = vec![super::ResolvedMount { row, cfg, spec, strategy }];

    // FAKE keys in a plain map — never real ones, never the real store. Present so the venue's plan
    // gate passes and the mount reaches `build_node` at all; the `data_only` declaration is what
    // then keeps them from exec.
    let (key_k, acct_k) = vike_oanda::oanda_env_var_names(vike_bridge_core::Environment::Demo);
    let vars: HashMap<String, String> = HashMap::from([
        (key_k, "fake-banner-token".to_string()),
        (acct_k, "101-004-0000000-001".to_string()),
    ]);

    let subs: Arc<Mutex<Vec<(&'static str, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let got_creds: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
    let ctors = ScriptedOandaCtors { subs, got_creds };

    let budget = vike_exec::ProfileRisk {
        max_notional_per_order: Some(1_000_000.0),
        max_total_exposure: Some(10_000_000.0),
        ..vike_exec::ProfileRisk::default()
    };

    let lock_dir = tempfile::tempdir().expect("a throwaway state dir for the B11 lock claims");
    let (handle, mut teardown, live_venues, _live_locks) = super::live_mount_with(
        mounts,
        Some(budget),
        None,
        &oanda_armed_policy(),
        vike_config::Flags::default(),
        vars,
        lock_dir.path(),
        &ctors,
    )
    .expect("the live mount stands up — no real store, no network");

    // The two sets DIFFER — the whole premise. Asserted before the banner so a build in which they
    // happened to coincide fails here, naming the reason, instead of passing the banner assert for
    // the wrong reason.
    assert!(
        !live_venues.contains(&profile_venue),
        "premise gone: build_node armed the profile's own venue ({profile_venue}), so this mount \
         can no longer tell a banner built from the record apart from one built from the profile"
    );

    // THE BANNER, rendered exactly as `main` renders it, from the mount's own record.
    let mode = super::ready_mode_line(true, &live_venues);
    assert!(
        !mode.contains(&profile_venue),
        "the ready banner named the venue the PROFILE mounts rather than what ARMED: {mode:?} \
         (build_node's arming record was {live_venues:?})"
    );
    assert_eq!(
        mode, "LIVE (venue=none)",
        "nothing armed, so the banner must say so — record {live_venues:?}"
    );

    for f in &mut teardown.feeds {
        f.shutdown();
    }
    teardown.forwarder_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    handle.shutdown_and_join();
}

// ===============================================================================================
// THE B11 LIVE-ACCOUNT LOCK COVERS THE ARMED SET (fix/locks-cover-the-armed-set)
//
// The defect these two tests are the gate for, measured on the CI box: the daemon claimed one
// `LIVE-<venue>.lock` per RUN-PROFILE mount while `vike_run::build_node` armed nine authenticated
// exec sessions from the credential store. A second process with a different profile locked a
// venue the first never locked, saw no conflict, and both traded one account.
//
// Both drive the REAL [`super::live_mount_with`] — every venue's plan gate, the `data_only`
// withhold, the claims, `build_node` — and both stay OFFLINE, by the same device the
// arming-ceiling suite in `vike-mount` uses: NO operator risk budget is supplied, so the first
// venue the mount classifies as live-intent is refused by `vike_mount::require_live_risk_budget`
// BEFORE its exec client is constructed. That refusal is not incidental scaffolding here — it is
// the ORDERING DISCRIMINATOR the second test reads (see its doc).
// ===============================================================================================

/// A credential map that arms THREE venues none of which the profile below mounts, spelled through
/// each venue's own key-name authority where one exists so a key-grid rename reddens here rather
/// than silently disarming the fixture. Fake values throughout — never real keys, never the real
/// store.
fn three_armed_venues_creds() -> HashMap<String, String> {
    let (oanda_key, oanda_acct) =
        vike_oanda::oanda_env_var_names(vike_bridge_core::Environment::Demo);
    let (ig_key, ig_ident, ig_pass) =
        vike_ig::ig_env_var_names(vike_bridge_core::Environment::Demo);
    HashMap::from([
        (oanda_key, "fake-oanda-token".to_string()),
        (oanda_acct, "101-004-0000000-001".to_string()),
        (ig_key, "fake-ig-key".to_string()),
        (ig_ident, "fake-ig-user".to_string()),
        (ig_pass, "fake-ig-pass".to_string()),
        ("BINANCE_DEMO_API_KEY".to_string(), "fake-binance-key".to_string()),
        ("BINANCE_DEMO_API_SECRET".to_string(), "fake-binance-secret".to_string()),
    ])
}

/// The machine ceiling for both tests: ig and oanda ARMED at `demo`, binance left at the `paper`
/// default [`vike_config::VenuePolicy::default`] gives every undeclared venue.
///
/// Binance is the load-bearing row, and it is the one that is NOT declared: it carries live
/// credentials in [`three_armed_venues_creds`] and must still claim NO lock, because a
/// paper-capped venue arms nothing and claiming its account sentinel would refuse a legitimate
/// second process over a venue this deployment deliberately does not trade.
fn ig_and_oanda_armed_policy() -> vike_config::Policy {
    vike_config::Policy {
        venues: vike_config::VenuePolicy::default()
            .declare("ig", vike_config::VenueMode::Demo)
            .declare("oanda", vike_config::VenueMode::Demo),
        ..vike_config::Policy::default()
    }
}

/// A single-mount deribit profile — the KEYLESS venue, so the profile's own venue arms nothing and
/// the "profile set" and the "armed set" are DISJOINT, which is the premise both tests rest on.
fn deribit_only_mount() -> Vec<super::ResolvedMount> {
    let symbol = super::wired_symbol_for("deribit").expect("build_node mounts deribit");
    let toml = format!(
        "venue = \"deribit\"\nsymbol = \"{symbol}\"\ninterval = \"1m\"\ninterval_ms = 60000\n\n\
         [strategy]\nname = \"buy_hold\"\n\n[strategy.params]\nsize = 1.0\n"
    );
    let row = crate::config::DaemonProfile::from_toml_str(&toml)
        .expect("the deribit profile parses and validates");
    row.validate_for_live().expect("…and passes the pure live gate");
    let cfg = row.to_mount_config();
    let spec = row.to_mount_spec();
    let strategy = row.resolve_strategy(&cfg).expect("buy_hold resolves from the registry");
    vec![super::ResolvedMount { row, cfg, spec, strategy }]
}

/// The `LIVE-<venue>.lock` sentinels present in `dir`, sorted — a claim's only observable trace.
fn sentinels(dir: &std::path::Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .expect("the throwaway state dir is readable")
        .map(|e| e.expect("dir entry").file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(vike_ops::live_lock::LIVE_LOCK_PREFIX))
        .collect();
    out.sort();
    out
}

/// **THE SET: the locks cover what ARMS, not what the profile mounts.**
///
/// A profile naming exactly ONE venue (deribit, keyless — it arms nothing) beside a credential
/// store that arms THREE, of which the machine's ceiling permits TWO. The claims must be `ig` and
/// `oanda` and nothing else:
///
/// * NOT `deribit` — the profile's own venue, and the ONLY venue the pre-fix loop over
///   `mount_venues` would have locked. Its absence is what makes this a regression test rather
///   than a coincidence: the two sets are disjoint by construction.
/// * NOT `binance` — live credentials, `paper` ceiling. A ceiling can only ever REFUSE an arming,
///   and a venue that cannot arm must not hold an account lock.
///
/// Asserted on the sentinel FILES, which is the only trace a claim leaves, and on the mount's own
/// refusal so the file assertion cannot be read against a run that never reached the claims.
#[test]
fn the_live_account_locks_cover_the_armed_set_not_the_profiles_mount_set() {
    let vars = three_armed_venues_creds();
    let policy = ig_and_oanda_armed_policy();

    // ANTI-VACUITY, and it must FAIL rather than skip: without this the assertions below would
    // pass unchanged against a fixture whose credentials arm nothing at all.
    let armed = vike_run::armed_live_venues(&vars, &vike_run::MountPolicy::from(&policy));
    assert_eq!(
        armed,
        vec!["ig".to_string(), "oanda".to_string()],
        "the fixture must arm exactly ig+oanda under this ceiling, or this test proves nothing"
    );
    // ⚠ `armed` is ROUTE KEYS now, one per ACCOUNT. With one account per venue a route key IS the
    // venue id, which is why the two literals above still read as venue names.
    assert!(
        !armed.iter().any(|k| k == "deribit"),
        "the profile's own venue must arm NOTHING, or the two sets are not disjoint and the \
         pre-fix loop would have passed this test too"
    );

    let lock_dir = tempfile::tempdir().expect("a throwaway state dir for the B11 lock claims");
    // NO risk budget on purpose — see the section header. The mount refuses at the first
    // live-intent venue, before any exec client is constructed, so this stays offline.
    let err = super::live_mount_with(
        deribit_only_mount(),
        None,
        None,
        &policy,
        vike_config::Flags::default(),
        vars,
        lock_dir.path(),
        &super::ProdFeedCtors,
    )
    .err()
    .unwrap_or_else(|| {
        panic!(
            "no operator risk budget ⇒ the first live-intent venue must refuse the mount; a \
             green mount here means nothing armed, and the sentinel assertions below would \
             prove nothing"
        )
    });
    assert!(
        err.contains("max_notional_per_order") || err.contains("max_total_exposure"),
        "the mount must have reached `build_node`'s live arms and refused there — got: {err}"
    );

    assert_eq!(
        sentinels(lock_dir.path()),
        vec!["LIVE-ig.lock".to_string(), "LIVE-oanda.lock".to_string()],
        "one sentinel per ARMED venue: not the profile's deribit, and not the paper-capped binance"
    );
}

/// **THE ORDER: the claims are made before any exec client exists — read off the ERROR, not a
/// comment.**
///
/// A second process already holds `LIVE-oanda.lock`. This mount arms ig+oanda and supplies NO risk
/// budget, so the two refusals it can produce are distinguishable and they are produced at
/// different points in the sequence:
///
/// * the LOCK refusal fires from [`super::live_mount_with`]'s safety-gate-#6 block, before
///   `vike_run::build_node` is called at all;
/// * the RISK-BUDGET refusal fires from inside `vike_mount::make_engine_with_legs`, i.e. once the
///   mount is already walking its venue arms.
///
/// So the error's IDENTITY is the ordering assertion: move the claims below `build_node` — which
/// is exactly where `vike-app` had them, and the reason a refused lock there was no longer
/// side-effect-free (bybit/okx/aster each post `set_leverage` at startup) — and this test goes red
/// with the risk-budget message instead. A comment could not have caught that; this does.
#[test]
fn the_account_locks_are_claimed_before_build_node_walks_a_single_venue_arm() {
    let vars = three_armed_venues_creds();
    let policy = ig_and_oanda_armed_policy();
    let lock_dir = tempfile::tempdir().expect("a throwaway state dir for the B11 lock claims");

    // The OTHER live process, standing in for the accident this lock exists to refuse: a stale
    // unit, a second terminal, a fat GUI beside the daemon.
    let _held = vike_ops::live_lock::LiveLock::acquire(lock_dir.path(), "oanda")
        .expect("the first claim on a fresh dir wins");

    let err = super::live_mount_with(
        deribit_only_mount(),
        None,
        None,
        &policy,
        vike_config::Flags::default(),
        vars,
        lock_dir.path(),
        &super::ProdFeedCtors,
    )
    .err()
    .unwrap_or_else(|| panic!("a held account lock must refuse the whole live mount"));

    assert!(
        err.contains("already trades the oanda account"),
        "the refusal must be the LOCK's, raised before `build_node` reached a venue arm — got: \
         {err}"
    );
    assert!(
        !err.contains("max_notional_per_order"),
        "a risk-budget refusal here means the claims were made AFTER the mount walked its venue \
         arms, i.e. after exec clients existed — got: {err}"
    );
}
