use super::*;
// Every bar below is flat at ts 1 with volume 1.0 — this file's former `bar(close)`, field for
// field. The tests read only `close` (the builder's argument) and how many bars a table holds,
// never a field the builder pins.
use vike_marketdata::test_support::flat_bar_unit_volume;

/// An empty-buffer `LiveBroker`, the shape `runtime::strategy_drive` hands a strategy.
/// `bars`/`index` are unread by every verb driven below (none of them looks at history).
fn ctx() -> LiveBroker {
    LiveBroker {
        position: 5.0,
        price: 100.0,
        equity: 10_000.0,
        bars: Arc::new(Vec::new()),
        index: 0,
        now: 0,
        multiplier: 1.0,
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

/// The other half of the partition, and the one every mount `vike-mount` builds today
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

    // The reduce-only market verb.
    b.submit_market_reduce(-1, 1.0);

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
