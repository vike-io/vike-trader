//! The deribit splice: scripted frames through the REAL arm reach the mounted strategy.

use super::*;

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
        // A scripted double with no status handle — nothing to disclose to.
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
        // A scripted double with no status handle — nothing to disclose to.
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
/// `crates/vike-tradehub/tests/daemon/support.rs`'s `own_sentinel` documents: a paper
/// mount is HALT-armed by design, so a test expecting fills must not inherit the operator's kill
/// switch off whatever box runs it.
fn own_sentinel() -> PathBuf {
    std::env::temp_dir()
        .join(format!("vike-tradehub-feed-splice-seam-{}", std::process::id()))
        .join("HALT")
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
    // The all-paper default policy is inert here — this call exercises the deribit arm, which never
    // reads it.
    let plan = venue_feed_plan(&cfg, &HashMap::new(), &vike_mount::MountPolicy::default())
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
        // No venue rows: every feed keeps its charter mark-stream default.
        &std::collections::BTreeMap::new(),
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
