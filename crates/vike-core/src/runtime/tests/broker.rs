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

    use crate::runtime::test_support::{core_of, engine_on, mount_of};

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
