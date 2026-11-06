use super::*;
// Every bar below is flat at ts 1 with volume 1.0 — this file's former `bar(close)`, field for
// field. The tests read only `close` (the builder's argument) and how many bars a table holds,
// never a field the builder pins.
use vike_marketdata::test_support::flat_bar_unit_volume;

/// An empty-buffer `LiveBroker`, the shape `runtime::strategy_drive` hands a strategy.
/// `bars`/`index` are unread by every verb driven below (none of them looks at history).
fn ctx() -> LiveBroker {
    LiveBroker {
        data_delay: None,
        position: 5.0,
        price: 100.0,
        equity: 10_000.0,
        bars: Arc::new(Vec::new()),
        index: 0,
        now: 0,
        contract_value: vike_model::ContractValue::default(),
        lot_size: 0.0,
        submissions: Vec::new(),
        modifications: Vec::new(),
        cancels: Vec::new(),
        positions: Vec::new(),
        prices: Vec::new(),
        bar_views: Vec::new(),
        brackets: Vec::new(),
        conditionals: Vec::new(),
        mass_cancel: false,
    }
}

/// The shape `declared_views` hands a DECLARED-multi mount: a row per declared instrument in
/// every table, and — the point of this fixture — nothing at all for `"SOMETHING_ELSE"`.
fn declared_ctx() -> LiveBroker {
    LiveBroker {
        data_delay: None,
        positions: vec![("ETHUSDT".into(), -7.0)],
        prices: vec![("ETHUSDT".into(), 50.0)],
        bar_views: vec![
            ("BTCUSDT".into(), Arc::new(vec![flat_bar_unit_volume(1, 100.0)])),
            ("ETHUSDT".into(), Arc::new(vec![flat_bar_unit_volume(1, 50.0)])),
        ],
        ..ctx()
    }
}

/// A DECLARED mount answers about the symbol NAMED — including `bars`, which discarded its
/// argument until this method got its table.
#[test]
fn a_declared_mount_answers_per_symbol() {
    let b = declared_ctx();
    assert_eq!(Broker::position(&b, "ETHUSDT"), -7.0);
    assert_eq!(Broker::price(&b, "ETHUSDT"), 50.0);
    assert_eq!(Broker::bars(&b, "ETHUSDT").last().map(|x| x.close), Some(50.0));
    assert_eq!(Broker::bars(&b, "BTCUSDT").last().map(|x| x.close), Some(100.0));
}

/// **THE DECISION.** A symbol a DECLARED mount does not carry reads EMPTY — never the
/// dispatching series' numbers, which is the defect, and never the account's, which this broker
/// cannot see. `0.0` / `0.0` / `&[]` is the literal truth about a mount whose order path REFUSES
/// that same symbol. `Broker::position`'s doc argues it against the alternatives.
#[test]
fn an_uncarried_symbol_reads_empty_on_a_declared_mount() {
    let b = declared_ctx();
    // The fixture's scalars are the dispatching series' — 5.0 and 100.0. Neither may leak out.
    assert_eq!(Broker::position(&b, "SOMETHING_ELSE"), 0.0);
    assert_eq!(Broker::price(&b, "SOMETHING_ELSE"), 0.0);
    assert!(Broker::bars(&b, "SOMETHING_ELSE").is_empty());
}

/// The other half of the partition, and the one every mount `vike-tradehub-core` builds today
/// (`MountSpec::legs` is empty on all of them): an UNDECLARED mount keeps the permissive
/// single-symbol licence — the argument is ignored, every read answers about the mount, and a
/// name it never heard of is NOT an uncarried symbol.
#[test]
fn an_undeclared_mount_still_ignores_the_symbol_argument() {
    let mut b = ctx();
    b.bars = Arc::new(vec![flat_bar_unit_volume(1, 100.0)]);
    for sym in ["", "BTCUSDT", "SOMETHING_ELSE"] {
        assert_eq!(Broker::position(&b, sym), 5.0, "position({sym:?})");
        assert_eq!(Broker::price(&b, sym), 100.0, "price({sym:?})");
        assert_eq!(Broker::bars(&b, sym).len(), 1, "bars({sym:?})");
    }
}

/// `""` means "my mount" on a DECLARED mount too — the [`LiveBroker::named`] convention, which
/// live bars depend on (`Bar::symbol` is `None`, so `unwrap_or_default()` passes `""`). Reading
/// it as an uncarried symbol would answer flat to every strategy written against a live bar.
#[test]
fn the_empty_symbol_is_the_mount_even_when_declared() {
    let b = declared_ctx();
    assert_eq!(Broker::position(&b, ""), 5.0);
    assert_eq!(Broker::price(&b, ""), 100.0);
    assert!(Broker::bars(&b, "").is_empty(), "the fixture's dispatching series is empty");
}

/// A buffered submission that carries a TAG never also carries a SYMBOL.
///
/// ⚠ This test used to be the producer-side pin for a `debug_assert!` in
/// `runtime::strategy_drive`'s `drain_broker`, on the theory that the tag→coid registry keyed on
/// the MOUNT's symbol while the ORDER routed through `resolve_intent_symbol` — so a
/// symbol-carrying tagged submit would insert under one key and be looked up under another.
/// **That reasoning was wrong in its premise**: the registry keyed on the DRAIN's symbol, not
/// the mount's, so the two disagreed whenever the dispatching series was not the mount's own,
/// with no symbol-carrying submit needed. `CoreThread::tag_key` now keys on the mount's own
/// series for the insert AND both lookups, which makes the key independent of a submission's
/// `symbol` altogether — so the assert was removed rather than left standing over a hazard it
/// no longer describes.
///
/// What this still pins is the `HftBroker` CONTRACT the tag lane rests on: a tag names a quote
/// and never an instrument, so `cancel_tagged(tag)`/`modify_tagged(tag)` are mount-scoped by
/// construction. A tagged verb that started naming a symbol would be a new surface needing its
/// own routing story (and would make one tag able to mean two orders on one mount, which the
/// symbol-less verbs cannot express) — this sweep is what forces that conversation.
///
/// EXHAUSTIVE over a CLOSED producer set: every `BufferedSubmit` in the workspace is pushed
/// by one of the sites in THIS file, and all of them are driven below. A new
/// submission-producing verb must be added to this sweep.
#[test]
fn tagged_orders_never_carry_a_symbol() {
    let mut b = ctx();

    // The inherent sizing verbs — `order_target_value`/`order_target_percent` funnel into
    // `order_target`, and `set_holdings` has TWO push arms (no lot grid → the oracle
    // `order_target_percent` path; a lot grid → its own `BufferedSubmit`). Drive both.
    b.order_target(50.0);
    b.order_target_value(1_000.0);
    b.order_target_percent(0.25);
    b.set_holdings(0.5);
    b.lot_size = 1.0;
    b.set_holdings(0.9);

    // The reduce-only market verb, and the reduce-only stop.
    b.submit_market_reduce(-1, 1.0);
    b.submit_stop_reduce(-1, 1.0, 95.0);

    // The two TAGGED verbs — the only sites in the workspace that set `tag: Some`, both
    // through the inherent method and through the delegating `HftBroker` impl the makers use.
    b.submit_limit_tagged("bid", 1, 1.0, 99.0);
    b.submit_market_tagged("flat", -1, 1.0);
    HftBroker::submit_limit_tagged(&mut b, "ask", -1, 1.0, 101.0);

    // The portable `Broker` verbs, NAMING a symbol — so this sweep genuinely observes a
    // symbol-carrying row and the assertion below cannot pass vacuously.
    Broker::submit_market(&mut b, "ETHUSDT", 1, 1.0);
    Broker::submit_limit(&mut b, "ETHUSDT", -1, 1.0, 101.0);

    assert!(
        b.submissions.iter().any(|s| s.tag.is_some()),
        "sweep observed no TAGGED submission — the invariant below would hold vacuously"
    );
    assert!(
        b.submissions.iter().any(|s| s.symbol.is_some()),
        "sweep observed no symbol-carrying submission — `symbol` is evidently never set, so \
             the invariant below would hold vacuously"
    );

    for s in &b.submissions {
        assert!(
            s.tag.is_none() || s.symbol.is_none(),
            "a {} submission tagged {:?} also named symbol {:?}: the tag registry keys on the \
                 MOUNT's symbol, so this row would insert under one key and be looked up under \
                 another",
            s.order_type,
            s.tag,
            s.symbol,
        );
    }
}

/// `submit_stop_reduce` buffers a STOP-kind submission carrying its trigger and the `reduce_only`
/// flag, untagged and on the mount's own symbol, in the ORDER lane (`submissions`): not in the core's
/// own conditional book, which `submit_stop` feeds and which fires a plain market.
#[test]
fn submit_stop_reduce_buffers_a_flagged_stop_with_its_trigger_in_the_order_lane() {
    let mut b = ctx();
    b.submit_stop_reduce(-1, 2.0, 95.0);

    assert!(b.conditionals.is_empty(), "not the core's own book");
    let [s] = b.submissions.as_slice() else { panic!("exactly one submission") };
    assert_eq!(s.order_type, vike_model::OrderType::Stop);
    assert_eq!((s.side, s.qty, s.trigger, s.price), (-1, 2.0, Some(95.0), None));
    assert!(s.reduce_only && s.symbol.is_none() && s.tag.is_none());
}

// ---- the order's LIFETIME (strategy verbs) ----

/// The two lifetime verbs buffer a LIMIT order that carries the lifetime, and every verb that names none is
/// the order as it always was (`Gtc`, no expiry): the lifetime is an additive field, not a new default.
#[test]
fn the_lifetime_verbs_buffer_a_limit_order_that_carries_it() {
    use vike_model::TimeInForce;
    let mut b = ctx();
    b.submit_limit_post_only("", 1, 2.0, 99.5);
    b.submit_limit_tif("ETHUSDT", -1, 1.0, 101.0, TimeInForce::Gtd, Some(4_102_444_800_000));
    Broker::submit_limit(&mut b, "", 1, 1.0, 98.0);
    Broker::submit_market(&mut b, "", 1, 1.0);
    b.order_target(7.0);
    let got: Vec<_> = b
        .submissions
        .iter()
        .map(|s| (s.order_type.clone(), s.price, s.time_in_force, s.gtd_expiry))
        .collect();
    let limit = vike_model::OrderType::Limit;
    assert_eq!(got[0], (limit.clone(), Some(99.5), TimeInForce::PostOnly, None));
    assert_eq!(got[1], (limit.clone(), Some(101.0), TimeInForce::Gtd, Some(4_102_444_800_000)));
    assert_eq!(b.submissions[1].symbol.as_deref(), Some("ETHUSDT"), "the verb names its symbol");
    assert_eq!(got[2], (limit, Some(98.0), TimeInForce::Gtc, None));
    for s in &b.submissions[3..] {
        assert_eq!((s.time_in_force, s.gtd_expiry), (TimeInForce::Gtc, None));
    }
}

mod lifetime_through_the_core {
    use std::sync::{Arc, Mutex};

    use super::*;
    use vike_exec::testing::RecordingClient;
    use vike_marketdata::QuoteTick;
    use vike_model::TimeInForce;

    use crate::runtime::test_support::core_of;
    use crate::testkit::mount_of;
    use vike_exec::testing::engine_on;

    /// Rests ONE post-only buy at the bid on every quote.
    struct PostOnlyAtTheBid {
        seen: Arc<Mutex<usize>>,
    }
    impl Strategy<LiveBroker> for PostOnlyAtTheBid {
        fn on_quote_tick(&mut self, broker: &mut LiveBroker, q: &QuoteTick) {
            *self.seen.lock().unwrap() += 1;
            broker.submit_limit_post_only("", 1, 1.0, q.bid);
        }
    }

    /// A core on the ROSTER venue `venue` (so the capability preflight really runs), the strategy
    /// mounted on it, then one quote.
    fn quote_once(venue: &str, symbol: &str) -> CoreThread<RecordingClient> {
        let strategy = Box::new(PostOnlyAtTheBid { seen: Arc::new(Mutex::new(0)) });
        let config = CoreConfig {
            strategy: Some(mount_of(venue, symbol, "1m", strategy)),
            ..Default::default()
        };
        let mut c =
            core_of(engine_on(venue, symbol, RecordingClient::default()), Vec::new(), config);
        c.dispatch(Ingest::Quote(Box::new(QuoteUpdate {
            venue: venue.into(),
            symbol: symbol.into(),
            quote: QuoteTick {
                ts: 1,
                local_ts: 1,
                bid: 100.0,
                ask: 100.1,
                bid_size: 10.0,
                ask_size: 10.0,
                symbol: String::new(),
            },
        })));
        c
    }

    /// A strategy's post-only order reaches the venue's client as a post-only LIMIT order on a venue that maps it.
    #[test]
    fn a_strategys_post_only_order_reaches_the_client_as_post_only() {
        let c = quote_once("okx", "BTC-USDT-SWAP");
        assert_eq!(
            c.engine.client.submissions.len(),
            1,
            "it cleared pre-trade and reached the client"
        );
        let sent = &c.engine.client.submissions[0];
        assert_eq!(sent.time_in_force, TimeInForce::PostOnly);
        assert_eq!(sent.order_type, vike_model::OrderType::Limit);
        assert_eq!(sent.price, Some(100.0));
    }

    /// ...and is refused BY NAME on a venue with no post-only order: a terminal rejection the strategy and the
    /// operator can read, never a plain GTC limit that could take liquidity.
    #[test]
    fn a_strategys_post_only_order_is_refused_by_name_where_the_venue_has_none() {
        let c = quote_once("oanda", "EUR_USD");
        assert!(c.engine.client.submissions.is_empty(), "nothing reaches the client");
        let order = c.engine.registry.values().next().expect("the refusal is registered");
        assert_eq!(order.status, vike_model::OrderStatus::Rejected);
        assert!(
            c.recent.iter().any(|n| n.contains("TIF_UNSUPPORTED: tif=PostOnly venue=oanda")),
            "{:?}",
            c.recent
        );
    }
}

/// What every non-bar lane hands its mount (`crates/vike-core/src/runtime/strategy_drive/hooks.rs`'s
/// `step_mount_hook`), pinned per lane on ONE core. Each lane held a hand copy of that body until they
/// shared one, so this is the witness that none of them moved: the lane's own price, else the MOUNT
/// symbol's standing mark (never the underlying's, never the foreign venue's), the mount symbol's
/// position, the sizing equity and lot of the mount's engine, the closed bars of the mount's own series,
/// the lane's own clock, the warmup gate on the tick lane alone, and the drain on the mount's own
/// `(venue, symbol)`.
mod non_bar_lanes {
    use std::sync::{Arc, Mutex};

    use super::*;
    use vike_marketdata::QuoteTick;
    use vike_model::MS_PER_DAY;
    use vike_model::time::schedule::TimeRule;

    use crate::runtime::test_support::core_with;

    /// One hook delivery: the lane, the mount, and the broker's scalars as the hook saw them.
    #[derive(Debug, Clone, Copy, PartialEq)]
    struct Seen {
        lane: &'static str,
        mount: &'static str,
        price: f64,
        position: f64,
        equity: f64,
        lot_size: f64,
        bars: usize,
        index: usize,
        now: i64,
    }

    struct Probe {
        mount: &'static str,
        warmup: usize,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Probe {
        /// Record what `lane` handed this mount, then rest one buy under a per-lane tag (so no tag
        /// entry overwrites another) for the lane's drain to place.
        fn record(&self, lane: &'static str, b: &mut LiveBroker) {
            self.seen.lock().unwrap().push(Seen {
                lane,
                mount: self.mount,
                price: b.price,
                position: b.position,
                equity: b.equity,
                lot_size: b.lot_size,
                bars: b.bars.len(),
                index: b.index,
                now: b.now,
            });
            b.submit_limit_tagged(lane, 1, 1.0, 100.0);
        }
    }

    impl Strategy<LiveBroker> for Probe {
        fn warmup(&self) -> usize {
            self.warmup
        }
        fn on_quote_tick(&mut self, b: &mut LiveBroker, _q: &QuoteTick) {
            self.record("tick", b);
        }
        fn on_schedule(&mut self, b: &mut LiveBroker, _tag: &str) {
            self.record("schedule", b);
        }
        fn on_feed_status(&mut self, b: &mut LiveBroker, _status: FeedStatus) {
            self.record("feed_status", b);
        }
        fn on_mark(&mut self, b: &mut LiveBroker, _mark: &MarkTick) {
            self.record("mark", b);
        }
        fn on_reference_quote(&mut self, b: &mut LiveBroker, _venue: &str, _q: &QuoteTick) {
            self.record("reference", b);
        }
        fn on_flow(&mut self, b: &mut LiveBroker, _flow: FlowToxicity) {
            self.record("flow", b);
        }
        fn on_params_updated(&mut self, b: &mut LiveBroker, _params: &vike_model::StrategyParams) {
            self.record("params", b);
        }
    }

    /// A mount on `(sim, BTCUSDT, 1m)` under its own controller id.
    fn probe_mount(
        id: &'static str,
        warmup: usize,
        symbols: Vec<MountLeg>,
        underlying: Option<&str>,
        seen: &Arc<Mutex<Vec<Seen>>>,
    ) -> StrategyMount {
        StrategyMount {
            account: None,
            symbols,
            controller_id: Some(id.to_string()),
            underlying_symbol: underlying.map(str::to_string),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            strategy: Box::new(Probe { mount: id, warmup, seen: Arc::clone(seen) }),
        }
    }

    /// A modern UTC midnight, so the daily schedule crosses one instant a day later.
    const DAY0: i64 = 20_650 * MS_PER_DAY;

    #[test]
    fn every_non_bar_lane_hands_its_mount_the_same_broker_and_drains_on_its_own_series() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut schedule = LiveSchedule::new();
        schedule.on(TimeRule::daily_at_utc(0, 0), "daily");
        let mut core = core_with(CoreConfig {
            seed_cash: 10_000.0,
            // hears every lane: watches SPOT (the mark lane), declares a leg on okx (the reference lane)
            strategy: Some(probe_mount(
                "probe",
                0,
                vec![MountLeg::at("BTC-USDT-SWAP", "okx")],
                Some("SPOT"),
                &seen,
            )),
            // shares the pair, so it hears the own-pair lanes and the tick, and never reaches warmup
            extra_mounts: vec![probe_mount("late", 99, Vec::new(), None, &seen)],
            mount_schedules: [("probe".to_string(), schedule)].into(),
            ..CoreConfig::default()
        });
        core.bars.insert(
            ("sim".into(), "BTCUSDT".into(), "1m".into()),
            BarSeries {
                closed: Arc::new((1..=3).map(|ts| flat_bar_unit_volume(ts, 100.0)).collect()),
                forming: None,
            },
        );
        core.engine.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), vike_model::events::PositionSide::Both),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
        core.engine.account.set_mark_from("sim", "BTCUSDT", 123.0, MarkSource::VenueMark, 0);
        // the underlying's own mark: a lane that read it in place of the mount symbol's says 7.0
        core.engine.account.set_mark_from("sim", "SPOT", 7.0, MarkSource::VenueMark, 0);
        core.engine.gate.limits.lot_size = Some(0.5);
        core.engine.now_ms = 5_000;
        let equity = core.eng(0).sizing_equity(core.seed_of(0), &core.config.price_cfg);

        core.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Live);
        core.drive_strategy_flow("sim", "BTCUSDT", FlowToxicity { bid: 0.25, ask: 0.75, ts: 1 });
        core.drive_strategy_params(&ParamsUpdate {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            mount_id: Some("probe".into()),
            params: vike_model::StrategyParams::Xemm(vike_model::XemmParams::default()),
        });
        core.drive_strategy_mark("sim", "SPOT", 7.5, 6_000);
        core.drive_strategy_reference_quote("okx", "BTC-USDT-SWAP", 99.0, 101.0, 1.0, 1.0, 7_000);
        core.drive_schedule(DAY0); // establishes the latch, fires nothing
        core.drive_schedule(DAY0 + MS_PER_DAY);
        // LAST: the tick lane first marks the engine at its own price
        let q = QuoteTick {
            ts: 8_000,
            local_ts: 8_000,
            bid: 129.9,
            ask: 130.1,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: "BTCUSDT".into(),
        };
        core.drive_strategy_tick("sim", "BTCUSDT", 130.0, 8_000, |s, ctx| s.on_quote_tick(ctx, &q));
        let tick_equity = core.eng(0).sizing_equity(core.seed_of(0), &core.config.price_cfg);

        let at = |lane, mount, price, equity, now| Seen {
            lane,
            mount,
            price,
            position: 2.0,
            equity,
            lot_size: 0.5,
            bars: 3,
            index: 2,
            now,
        };
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                at("feed_status", "probe", 123.0, equity, 5_000),
                at("feed_status", "late", 123.0, equity, 5_000),
                at("flow", "probe", 123.0, equity, 5_000),
                at("flow", "late", 123.0, equity, 5_000),
                at("params", "probe", 123.0, equity, 5_000),
                at("mark", "probe", 123.0, equity, 6_000),
                at("reference", "probe", 123.0, equity, 7_000),
                at("schedule", "probe", 123.0, equity, DAY0 + MS_PER_DAY),
                // `late` hears the tick too, below its warmup: the gate is the tick lane's alone
                at("tick", "probe", 130.0, tick_equity, 8_000),
            ]
        );
        let sent: Vec<_> = core
            .engine
            .client
            .submissions
            .iter()
            .map(|r| (r.venue.as_str(), r.symbol.as_str()))
            .collect();
        assert_eq!(
            sent,
            vec![("sim", "BTCUSDT"); 9],
            "every lane drains on the mount's own series"
        );
    }
}
