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
        inputs: None,
        closed_trades: None,
        position: 5.0,
        average_price: 0.0,
        price: 100.0,
        equity: 10_000.0,
        bars: Arc::new(Vec::new()),
        index: 0,
        now: 0,
        contract_value: vike_model::ContractValue::default(),
        quote_rate: 1.0,
        lot_size: 0.0,
        tick_size: 0.0,
        min_qty: 0.0,
        min_notional: 0.0,
        submissions: Vec::new(),
        modifications: Vec::new(),
        cancels: Vec::new(),
        positions: Vec::new(),
        prices: Vec::new(),
        bar_views: Vec::new(),
        leg_values: None,
        brackets: Vec::new(),
        order_lists: Vec::new(),
        combos: Vec::new(),
        order_emulation_arms: Vec::new(),
        mass_cancel: false,
    }
}

/// The shape `declared_views` hands a DECLARED-multi mount: a row per declared instrument in
/// every table, and — the point of this fixture — nothing at all for `"SOMETHING_ELSE"`.
fn declared_context() -> LiveBroker {
    LiveBroker {
        data_delay: None,
        inputs: None,
        closed_trades: None,
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
    let b = declared_context();
    assert_eq!(OrderBroker::position(&b, "ETHUSDT"), -7.0);
    assert_eq!(OrderBroker::price(&b, "ETHUSDT"), 50.0);
    assert_eq!(OrderBroker::bars(&b, "ETHUSDT").last().map(|x| x.close), Some(50.0));
    assert_eq!(OrderBroker::bars(&b, "BTCUSDT").last().map(|x| x.close), Some(100.0));
}

/// The seam's average read takes `OrderBroker::position`'s partition (decision 0277): the dispatching
/// series' average for `""` and for it by name, NaN for another declared leg (the core carries no
/// average for it), `0.0` for a symbol the mount does not carry; any name on a single-symbol mount.
#[test]
fn the_average_price_read_takes_the_positions_partition() {
    use vike_model::OrderBroker;
    let b = LiveBroker { average_price: 101.5, ..declared_context() };
    assert_eq!(OrderBroker::average_price(&b, ""), 101.5);
    assert_eq!(OrderBroker::average_price(&b, "BTCUSDT"), 101.5, "the dispatching series");
    assert!(OrderBroker::average_price(&b, "ETHUSDT").is_nan(), "a leg's average is not carried");
    assert_eq!(OrderBroker::average_price(&b, "SOLUSDT"), 0.0, "not carried: nothing held");
    let single = LiveBroker { average_price: 101.5, ..ctx() };
    assert_eq!(OrderBroker::average_price(&single, "ANY"), 101.5);
}

/// The gate's grid reaches a strategy whole, for the mount's own symbol: `""` and its own name on
/// any mount, any name on a single-symbol one; a declared LEG answers its own row from the leg table
/// (one broker plan, step 12: it answered `None`), none when the gate holds no row of its own for it;
/// a gate with no grid at all answers `None`, not an all-empty grid.
#[test]
fn the_instrument_grid_is_the_gates_own_for_the_mounts_symbol() {
    let gridded = |b: LiveBroker| LiveBroker {
        tick_size: 0.01,
        lot_size: 0.001,
        min_qty: 0.002,
        min_notional: 5.0,
        ..b
    };
    let want = vike_model::Grid {
        tick_size: Some(0.01),
        lot_size: Some(0.001),
        min_notional: Some(5.0),
        min_qty: Some(0.002),
    };
    assert_eq!(OrderBroker::instrument_grid(&ctx(), ""), None, "no grid at all");
    assert_eq!(OrderBroker::instrument_grid(&gridded(ctx()), ""), Some(want));
    assert_eq!(OrderBroker::instrument_grid(&gridded(ctx()), "BTCUSDT"), Some(want));
    let declared = gridded(declared_context());
    assert_eq!(OrderBroker::instrument_grid(&declared, ""), Some(want));
    assert_eq!(OrderBroker::instrument_grid(&declared, "BTCUSDT"), Some(want), "the own name");
    assert_eq!(OrderBroker::instrument_grid(&declared, "ETHUSDT"), None, "no leg table");
    let leg = vike_model::Grid {
        tick_size: Some(0.05),
        lot_size: Some(0.01),
        min_notional: None,
        min_qty: None,
    };
    let legs = Arc::from(vec![
        ("BTCUSDT".to_string(), vike_model::ContractValue::default(), None, None),
        ("ETHUSDT".to_string(), vike_model::ContractValue::default(), Some(leg), None),
        ("SOLUSDT".to_string(), vike_model::ContractValue::default(), None, None),
    ]);
    let declared = LiveBroker { leg_values: Some(legs), ..declared };
    assert_eq!(OrderBroker::instrument_grid(&declared, "ETHUSDT"), Some(leg), "the leg's own row");
    assert_eq!(OrderBroker::instrument_grid(&declared, "SOLUSDT"), None, "no row of its own");
    assert_eq!(OrderBroker::instrument_grid(&declared, "ELSE"), None, "undeclared");
    assert_eq!(
        OrderBroker::instrument_grid(&declared, ""),
        Some(want),
        "the own grid stays the gate's"
    );
}

/// A declared mount answers a NAMED symbol's contract value from its leg table (the account's own
/// value for that leg), `""` and every name on a single-symbol mount from the mount's own field, and
/// an undeclared symbol `None`.
#[test]
fn a_declared_mount_answers_each_legs_contract_value() {
    use vike_model::ContractValue;
    let own = ContractValue::linear(2.0);
    let legs: Arc<[LegFacts]> = Arc::from(vec![
        ("BTCUSDT".to_string(), own, None, None),
        ("ETHUSDT".to_string(), ContractValue::inverse(100.0), None, None),
    ]);
    let b = LiveBroker { contract_value: own, leg_values: Some(legs), ..declared_context() };
    assert_eq!(OrderBroker::contract_value(&b, ""), Some(own));
    assert_eq!(OrderBroker::contract_value(&b, "BTCUSDT"), Some(own));
    assert_eq!(OrderBroker::contract_value(&b, "ETHUSDT"), Some(ContractValue::inverse(100.0)));
    assert_eq!(OrderBroker::contract_value(&b, "SOMETHING_ELSE"), None, "undeclared");
    let single = LiveBroker { contract_value: own, ..ctx() };
    assert_eq!(OrderBroker::contract_value(&single, "ANY"), Some(own), "a single mount: any name");
}

/// **THE DECISION.** A symbol a DECLARED mount does not carry reads EMPTY — never the
/// dispatching series' numbers, which is the defect, and never the account's, which this broker
/// cannot see. `0.0` / `0.0` / `&[]` is the literal truth about a mount whose order path REFUSES
/// that same symbol. `OrderBroker::position`'s doc argues it against the alternatives.
#[test]
fn an_uncarried_symbol_reads_empty_on_a_declared_mount() {
    let b = declared_context();
    // The fixture's scalars are the dispatching series' — 5.0 and 100.0. Neither may leak out.
    assert_eq!(OrderBroker::position(&b, "SOMETHING_ELSE"), 0.0);
    assert_eq!(OrderBroker::price(&b, "SOMETHING_ELSE"), 0.0);
    assert!(OrderBroker::bars(&b, "SOMETHING_ELSE").is_empty());
}

/// The other half of the partition, and every mount whose strategy declares no
/// `vike_model::Strategy::symbols` (its `MountSpec::legs` is empty): an UNDECLARED mount keeps the permissive
/// single-symbol licence — the argument is ignored, every read answers about the mount, and a
/// name it never heard of is NOT an uncarried symbol.
#[test]
fn an_undeclared_mount_still_ignores_the_symbol_argument() {
    let mut b = ctx();
    b.bars = Arc::new(vec![flat_bar_unit_volume(1, 100.0)]);
    for symbol in ["", "BTCUSDT", "SOMETHING_ELSE"] {
        assert_eq!(OrderBroker::position(&b, symbol), 5.0, "position({symbol:?})");
        assert_eq!(OrderBroker::price(&b, symbol), 100.0, "price({symbol:?})");
        assert_eq!(OrderBroker::bars(&b, symbol).len(), 1, "bars({symbol:?})");
    }
}

/// `""` means "my mount" on a DECLARED mount too — the [`LiveBroker::named`] convention, which
/// live bars depend on (`Bar::symbol` is `None`, so `unwrap_or_default()` passes `""`). Reading
/// it as an uncarried symbol would answer flat to every strategy written against a live bar.
#[test]
fn the_empty_symbol_is_the_mount_even_when_declared() {
    let b = declared_context();
    assert_eq!(OrderBroker::position(&b, ""), 5.0);
    assert_eq!(OrderBroker::price(&b, ""), 100.0);
    assert_eq!(
        OrderBroker::bars(&b, ""),
        OrderBroker::bars(&b, "BTCUSDT"),
        "the mount's own history"
    );
}

/// **On a LEG's dispatch `""` is still the MOUNT's own symbol** (decision 0277's rule 1, one broker
/// plan step 12): the scalars describe the dispatching LEG there, so answering `""` from them read the
/// leg's position, price, history and value as the mount's own. The own symbol's rows answer instead,
/// exactly as naming it does.
#[test]
fn the_empty_symbol_is_the_mount_on_a_legs_dispatch() {
    use vike_model::ContractValue;
    let own = ContractValue::linear(2.0);
    // Dispatched by ETHUSDT (the scalars: 5.0 at 100.0, one bar); the mount's own BTCUSDT has rows.
    let b = LiveBroker {
        positions: vec![("BTCUSDT".into(), -3.0)],
        prices: vec![("BTCUSDT".into(), 200.0)],
        bar_views: vec![
            ("BTCUSDT".into(), Arc::new(vec![flat_bar_unit_volume(1, 200.0); 2])),
            ("ETHUSDT".into(), Arc::new(vec![flat_bar_unit_volume(1, 100.0)])),
        ],
        leg_values: Some(Arc::from(vec![
            ("BTCUSDT".to_string(), own, None, None),
            ("ETHUSDT".to_string(), ContractValue::inverse(100.0), None, None),
        ])),
        contract_value: ContractValue::inverse(100.0),
        ..ctx()
    };
    assert_eq!(OrderBroker::position(&b, ""), -3.0, "not the leg's 5.0");
    assert_eq!(OrderBroker::price(&b, ""), 200.0, "not the leg's 100.0");
    assert_eq!(OrderBroker::bars(&b, "").len(), 2, "the mount's own history, not the leg's");
    assert_eq!(OrderBroker::contract_value(&b, ""), Some(own), "the mount's own value");
    assert_eq!(
        OrderBroker::position(&b, ""),
        OrderBroker::position(&b, "BTCUSDT"),
        "`\"\"` is the own name"
    );
    // The leg itself still reads the scalars it dispatched with.
    assert_eq!(OrderBroker::position(&b, "ETHUSDT"), 5.0);
}

/// A buffered submission that carries a TAG carries a SYMBOL only when its verb named one: the
/// portable order seam's (`OrderBroker::place`, `deny`, decision 0277). Every other tagged verb (the
/// inherent ones) carries none, and the seam's `""` (the
/// mount's own, what a maker passes) carries none either, so it allocates nothing.
///
/// ⚠ This test used to be the producer-side pin for a `debug_assert!` in
/// `runtime::strategy_drive`'s `drain_broker`, on the theory that the tag→order client id registry keyed on
/// the MOUNT's symbol while the ORDER routed through `resolve_intent_symbol` — so a
/// symbol-carrying tagged submit would insert under one key and be looked up under another.
/// **That reasoning was wrong in its premise**: the registry keyed on the DRAIN's symbol, not
/// the mount's, so the two disagreed whenever the dispatching series was not the mount's own,
/// with no symbol-carrying submit needed. `CoreThread::tag_key` now keys on the mount's own
/// series for the insert AND both lookups, which makes the key independent of a submission's
/// `symbol` altogether — so the assert was removed rather than left standing over a hazard it
/// no longer describes.
///
/// What this still pins is the CONTRACT the tag lane rests on: a tag names one order of the mount
/// and never an instrument, so `cancel_tagged(tag)`/`modify_tagged(tag)` and the seam's
/// `amend(tag)`/`cancel(tag)` are mount-scoped by construction. The seam's routing story is the
/// drain's one rule for every submission (`resolve_intent_symbol`); a NEW tagged verb that names a
/// symbol joins the seam rows below.
///
/// EXHAUSTIVE over a CLOSED producer set: every `BufferedSubmit` in the workspace is pushed
/// by one of the sites in THIS file, and all of them are driven below. A new
/// submission-producing verb must be added to this sweep.
#[test]
fn a_tagged_order_carries_only_the_symbol_its_verb_named() {
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

    // The reduce-only market verb, the reduce-only stop, and the two trigger-kind verbs.
    b.submit_market_reduce(-1, 1.0);
    b.submit_stop_reduce(-1, 1.0, 95.0);
    b.submit_stop_limit(-1, 1.0, 95.0, 94.5);
    b.submit_take_profit(-1, 1.0, 105.0, Some(105.5));
    b.submit_take_profit(-1, 1.0, 105.0, None);

    // The inherent TAGGED verbs (the portable seam's tagged rows are driven at the end).
    b.submit_limit_tagged("bid", 1, 1.0, 99.0);
    b.submit_market_tagged("flat", -1, 1.0);

    // The seam's UNTAGGED plain orders, NAMING a symbol — so this sweep genuinely observes a
    // symbol-carrying row and the assertion below cannot pass vacuously.
    use vike_model::{OrderBroker, OrderPlacement};
    OrderBroker::place(&mut b, "ETHUSDT", "", 1, 1.0, OrderPlacement::Market, false);
    let limit = OrderPlacement::Limit { price: 101.0 };
    OrderBroker::place(&mut b, "ETHUSDT", "", -1, 1.0, limit, false);

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

    // The portable order seam: `""` (the mount's own, a maker's) carries none; a named symbol rides
    // the row as an untagged order's does, the working kinds' included.
    let mut seam = ctx();
    let limit = OrderPlacement::Limit { price: 99.0 };
    OrderBroker::place(&mut seam, "", "own", 1, 1.0, limit, false);
    OrderBroker::place(&mut seam, "ETHUSDT", "leg", 1, 1.0, limit, false);
    let iceberg = OrderPlacement::Iceberg { price: 99.0, display: 0.5 };
    OrderBroker::place(&mut seam, "ETHUSDT", "ice", 1, 1.0, iceberg, false);
    OrderBroker::deny(&mut seam, "ETHUSDT", "no", 1.0, "refused by its caller");
    let rows: Vec<_> =
        seam.submissions.iter().map(|s| (s.tag.as_deref(), s.symbol.as_deref())).collect();
    let leg = Some("ETHUSDT");
    assert_eq!(
        rows,
        [(Some("own"), None), (Some("leg"), leg), (Some("ice"), leg), (Some("no"), leg)]
    );
}

/// `submit_stop_reduce` buffers a STOP-kind submission carrying its trigger and the `reduce_only`
/// flag, untagged and on the mount's own symbol, in the ORDER lane (`submissions`): not in the core's
/// own conditional book, which `submit_stop` feeds and which fires a market.
#[test]
fn submit_stop_reduce_buffers_a_flagged_stop_with_its_trigger_in_the_order_lane() {
    let mut b = ctx();
    b.submit_stop_reduce(-1, 2.0, 95.0);

    assert!(b.order_emulation_arms.is_empty(), "not the core's own book");
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
    b.submit_limit_time_in_force(
        "ETHUSDT",
        -1,
        1.0,
        101.0,
        TimeInForce::Gtd,
        Some(4_102_444_800_000),
    );
    use vike_model::{OrderBroker, OrderPlacement};
    OrderBroker::place(&mut b, "", "", 1, 1.0, OrderPlacement::Limit { price: 98.0 }, false);
    OrderBroker::place(&mut b, "", "", 1, 1.0, OrderPlacement::Market, false);
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

/// **ONE sizing law for every contract kind** (decision 0170, item 9, amended 2026-11-08): `order_target_value`,
/// `order_target_percent` and `set_holdings` size by what ONE unit of the mounted contract is worth in the
/// ACCOUNT's money at the broker's price (`vike_model::ContractValue::unit_value`), the money
/// [`OrderBroker::equity`] reports. A linear contract is worth `price · per_unit`, bit for bit what it always was
/// ([`linear_law`] is the old law, frozen here); an inverse one is worth its face, and the price never enters.
mod sizing_every_kind {
    use super::*;
    use vike_model::ContractValue;

    /// The law the verbs had before the inverse arm, frozen as the linear reference: `target / (price · u)`,
    /// `0.0` on a non-positive denominator.
    fn linear_law(target: f64, price: f64, u: f64) -> f64 {
        let denominator = price * u;
        if denominator > 0.0 { target / denominator } else { 0.0 }
    }

    /// A FLAT broker on a contract worth `cv` at `price` with `equity` (the account's money), no lot grid.
    fn on(cv: ContractValue, price: f64, equity: f64) -> LiveBroker {
        LiveBroker { contract_value: cv, price, equity, position: 0.0, ..ctx() }
    }

    /// The one order a verb buffered, signed by its side; `None` when it buffered nothing.
    fn sent(b: &LiveBroker) -> Option<f64> {
        match b.submissions.as_slice() {
            [] => None,
            [s] => Some(f64::from(s.side) * s.qty),
            more => panic!("one order at most, got {}", more.len()),
        }
    }

    fn value(cv: ContractValue, price: f64, target: f64) -> Option<f64> {
        let mut b = on(cv, price, 0.0);
        b.order_target_value(target);
        sent(&b)
    }

    fn percent(cv: ContractValue, price: f64, equity: f64, pct: f64) -> Option<f64> {
        let mut b = on(cv, price, equity);
        b.order_target_percent(pct);
        sent(&b)
    }

    fn holdings(
        cv: ContractValue,
        price: f64,
        equity: f64,
        pos: f64,
        lot: f64,
        pct: f64,
    ) -> Option<f64> {
        let mut b = LiveBroker { position: pos, lot_size: lot, ..on(cv, price, equity) };
        b.set_holdings(pct);
        sent(&b)
    }

    /// Deribit `BTC-PERPETUAL` (inverse, face 1 USD, a coin ledger under decision 0258): a value of 1,000 in
    /// the account's money is 1,000 contracts AT ANY PRICE, and 30 % of an equity of 2 BTC valued at P (the
    /// equity `OrderBroker::equity` reports, `2·P` dollars) is `0.3 · 2 · P` contracts. Before the law this verb
    /// refused an inverse mount and buffered nothing.
    #[test]
    fn deribit_btc_perpetual_sizes_by_its_face_at_any_price() {
        let perp = ContractValue::inverse(1.0);
        for p in [60_000.0, 100_000.0, 3.5] {
            assert_eq!(value(perp, p, 1_000.0), Some(1_000.0), "value at {p}");
        }
        assert_eq!(percent(perp, 60_000.0, 2.0 * 60_000.0, 0.3), Some(36_000.0));
        assert_eq!(percent(perp, 100_000.0, 2.0 * 100_000.0, 0.3), Some(60_000.0));
        // a short target is a sell of the same size
        assert_eq!(value(perp, 60_000.0, -1_000.0), Some(-1_000.0));
        // a coin-margined contract whose face is 100 USD: ten of them are 1,000
        assert_eq!(value(ContractValue::inverse(100.0), 60_000.0, 1_000.0), Some(10.0));
    }

    /// `set_holdings` walks the lot grid with the same unit: deribit's grid is 10 contracts, so 30 % of
    /// 120,005 (36,001.5 contracts) lands UNDER, at 36,000, and from a long of 1,000 the order is 35,000.
    #[test]
    fn deribit_set_holdings_lands_at_or_under_the_target_on_the_lot_grid() {
        let perp = ContractValue::inverse(1.0);
        assert_eq!(holdings(perp, 60_000.0, 120_005.0, 0.0, 10.0, 0.3), Some(36_000.0));
        assert_eq!(holdings(perp, 60_000.0, 120_005.0, 1_000.0, 10.0, 0.3), Some(35_000.0));
        // exactly on the grid, and the price does not move it
        assert_eq!(holdings(perp, 100_000.0, 120_000.0, 0.0, 10.0, 0.3), Some(36_000.0));
        // at the target already: nothing
        assert_eq!(holdings(perp, 60_000.0, 120_000.0, 36_000.0, 10.0, 0.3), None);
    }

    /// Every LINEAR kind the workspace trades, through all three verbs, is the old law bit for bit: spot
    /// (binance `BTCUSDT`), an equity (alpaca/ibkr `AAPL`), a USDC option priced by its premium (deribit), a
    /// polymarket outcome share (price in 0..1), a forex pair in units (oanda, fxcm, ctrader, dukascopy), a
    /// forex pair in contracts of 10,000 units (IG's `contractSize`), and a future with an exchange multiplier
    /// (ibkr `ES`, 50). okx and bybit linear quantities reach the engine in base units (`per_unit` 1).
    #[test]
    fn every_linear_kind_is_the_old_law_bit_for_bit() {
        let kinds: [(&str, f64, f64, f64); 7] = [
            ("spot BTCUSDT", 1.0, 50_000.0, 1_000.0),
            ("equity AAPL", 1.0, 200.0, 1_000.0),
            ("usdc option premium", 1.0, 512.0, 1_024.0),
            ("polymarket share", 1.0, 0.25, 10.0),
            ("fx EURUSD in units", 1.0, 1.25, 125_000.0),
            ("fx EURUSD in IG contracts", 10_000.0, 1.25, 125_000.0),
            ("future ES x50", 50.0, 5_000.0, 500_000.0),
        ];
        for (kind, u, p, target) in kinds {
            let cv = ContractValue::linear(u);
            let want = linear_law(target, p, u);
            assert_eq!(
                value(cv, p, target).map(f64::to_bits),
                Some(want.to_bits()),
                "{kind}: value"
            );
            let pct = linear_law(0.5 * 20_000.0, p, u);
            assert_eq!(
                percent(cv, p, 20_000.0, 0.5).map(f64::to_bits),
                Some(pct.to_bits()),
                "{kind}: percent"
            );
            // without a lot grid set_holdings is order_target_percent
            assert_eq!(
                holdings(cv, p, 20_000.0, 0.0, 0.0, 0.5).map(f64::to_bits),
                Some(pct.to_bits()),
                "{kind}"
            );
            // on a grid it lands at or under: the old walk over the unit `price · u`
            let lot = 1.0;
            let walk = vike_model::amount_to_order(0.0, 0.5 * 20_000.0, p * u, lot);
            let got = holdings(cv, p, 20_000.0, 0.0, lot, 0.5).unwrap_or(0.0);
            assert_eq!(got.to_bits(), walk.to_bits(), "{kind}: lot walk");
        }
        // the readable numbers behind three rows
        assert_eq!(value(ContractValue::linear(1.0), 50_000.0, 1_000.0), Some(0.02));
        assert_eq!(value(ContractValue::linear(10_000.0), 1.25, 125_000.0), Some(10.0));
        assert_eq!(value(ContractValue::linear(1.0), 0.25, 10.0), Some(40.0));
    }

    /// No usable price is still a no-op for every kind (the oracle guard), never a target of zero units.
    #[test]
    fn no_usable_price_sizes_nothing_for_any_kind() {
        for cv in
            [ContractValue::linear(1.0), ContractValue::linear(50.0), ContractValue::inverse(1.0)]
        {
            for p in [0.0, -1.0] {
                let mut b = LiveBroker { position: 3.0, ..on(cv, p, 10_000.0) };
                b.order_target_value(1_000.0);
                b.order_target_percent(0.5);
                b.set_holdings(0.5);
                b.lot_size = 1.0;
                b.set_holdings(0.5);
                assert!(b.submissions.is_empty(), "{cv:?} at {p}");
            }
        }
    }

    /// Decision 0183's factor is a parameter of the law: USDJPY in IG contracts of 10,000 units on a USD
    /// account at 128, factor 1/128, is 10,000 dollars a contract through every verb; a factor the engine
    /// does not hold (`NaN`, `Account::quote_rate_of` of an unrated foreign symbol) sizes NOTHING for
    /// any kind, never at par. `order_target(units)` still trades.
    #[test]
    fn the_quote_factor_is_applied_and_a_missing_one_refuses() {
        let jpy = |rate| LiveBroker {
            quote_rate: rate,
            ..on(ContractValue::linear(10_000.0), 128.0, 20_000.0)
        };
        let mut b = jpy(1.0 / 128.0);
        b.order_target_value(100_000.0);
        assert_eq!(sent(&b), Some(10.0));
        let mut b = jpy(1.0 / 128.0);
        b.order_target_percent(0.5);
        assert_eq!(sent(&b), Some(1.0));
        let mut b = LiveBroker { lot_size: 1.0, ..jpy(1.0 / 128.0) };
        b.set_holdings(0.75); // 15,000 dollars: one contract, under the target
        assert_eq!(sent(&b), Some(1.0));

        for cv in [ContractValue::linear(10_000.0), ContractValue::inverse(1.0)] {
            let mut b = LiveBroker { quote_rate: f64::NAN, ..on(cv, 128.0, 20_000.0) };
            b.order_target_value(100_000.0);
            b.order_target_percent(0.5);
            b.set_holdings(0.5);
            b.lot_size = 1.0;
            b.set_holdings(0.5);
            assert!(b.submissions.is_empty(), "{cv:?}: no factor, no size");
            b.order_target(3.0);
            assert_eq!(sent(&b), Some(3.0), "units still trade");
        }
    }

    /// The same law through the REAL core: what each lane hands the broker (the mount's own engine's
    /// equity and the contract value of its own symbol) is what the verbs size with.
    mod through_the_core {
        use std::sync::{Arc, Mutex};

        use super::*;
        use vike_execution::testing::{RecordingClient, engine_on};
        use vike_execution::{Account, BalanceMode, Engine, RiskGate};
        use vike_model::RiskLimits;

        use crate::runtime::test_support::core_of;
        use crate::testkit::mount_of;

        struct Idle;
        impl Strategy<LiveBroker> for Idle {}

        type Seen = Arc<Mutex<Vec<(f64, Option<f64>)>>>;

        /// One tick at `price` on `(venue, symbol)`: `verb` runs on the mount's broker, which records the
        /// equity it saw and the one order it buffered, then CLEARS the buffer (nothing reaches the gate:
        /// the subject is the size, not the venue's answer).
        fn tick(
            core: &mut CoreThread<RecordingClient>,
            venue: &str,
            symbol: &str,
            price: f64,
            verb: fn(&mut LiveBroker),
        ) -> (f64, Option<f64>) {
            let seen: Seen = Arc::default();
            let s = Arc::clone(&seen);
            core.drive_strategy_tick(venue, symbol, price, 8_000, move |_, b| {
                verb(b);
                s.lock().unwrap().push((b.equity, sent(b)));
                b.submissions.clear();
            });
            let got = seen.lock().unwrap().clone();
            assert_eq!(got.len(), 1, "the mount heard the tick once");
            got[0]
        }

        fn mounted(engine: Engine<RecordingClient>, seed: f64) -> CoreThread<RecordingClient> {
            let (venue, symbol) = (engine.venue.clone(), engine.symbol.clone());
            let config = CoreConfig {
                seed_cash: seed,
                strategy: Some(mount_of(&venue, &symbol, "1m", Box::new(Idle))),
                ..Default::default()
            };
            core_of(engine, Vec::new(), config)
        }

        /// Deribit `BTC-PERPETUAL` on a COIN LEDGER holding 2 BTC: the equity the broker sees is the coin
        /// at the contract's own price (`2·P` dollars), a value of 1,000 is 1,000 contracts at any price,
        /// and 30 % is `0.3 · 2 · P` contracts.
        #[test]
        fn a_deribit_coin_ledger_sizes_by_face_through_the_core() {
            let mut grid = indexmap::IndexMap::new();
            grid.insert("BTC-PERPETUAL".to_string(), ContractValue::inverse(1.0));
            let engine = Engine::new(
                Account::new(1.0, "deribit", Some(grid), BalanceMode::Delta),
                RiskGate::new(RiskLimits::new()),
                RecordingClient::default(),
                "deribit",
                "BTC-PERPETUAL",
            );
            let mut core = mounted(engine, 0.0);
            assert!(core.engine.account.coin_key().is_some(), "precondition: a coin ledger");
            core.engine.account.balance = 2.0; // BTC
            for p in [60_000.0, 100_000.0] {
                let (equity, size) = tick(&mut core, "deribit", "BTC-PERPETUAL", p, |b| {
                    b.order_target_value(1_000.0)
                });
                assert_eq!(equity, 2.0 * p, "the coin at the contract's own price");
                assert_eq!(size, Some(1_000.0), "value at {p}");
                let (_, size) =
                    tick(&mut core, "deribit", "BTC-PERPETUAL", p, |b| b.order_target_percent(0.3));
                assert_eq!(size, Some(0.3 * (2.0 * p)), "percent at {p}");
            }
        }

        /// USDJPY on a USD account, 10,000 units a contract, at 128 with the 0183 factor 1/128: one contract
        /// is 10,000 dollars, so 100,000 dollars is 10 contracts and half of 20,000 is one.
        #[test]
        fn a_pair_quoted_in_another_currency_sizes_through_its_factor() {
            let mut engine = engine_on("sim", "USDJPY", RecordingClient::default());
            engine.set_account_currency("USD").unwrap();
            engine
                .account
                .admit_quoted(
                    "USDJPY",
                    "JPY",
                    Some(ContractValue::linear(10_000.0)),
                    Some((1.0 / 128.0, 8_000)),
                    "USD",
                    8_000,
                )
                .unwrap();
            let mut core = mounted(engine, 20_000.0);
            let (_, size) =
                tick(&mut core, "sim", "USDJPY", 128.0, |b| b.order_target_value(100_000.0));
            assert_eq!(size, Some(10.0));
            let (_, size) =
                tick(&mut core, "sim", "USDJPY", 128.0, |b| b.order_target_percent(0.5));
            assert_eq!(size, Some(1.0));
        }

        /// EURUSD MOUNTED on a EUR account (decision 0183's mounted-symbol amendment), declared and NOT YET
        /// RATED: the engine holds no factor, so no verb sizes (it never sizes at par); once the rate lands
        /// (0.5 EUR per USD) a value of 1,250 EUR at 1.25 is 2,000 units.
        #[test]
        fn a_foreign_quote_without_its_factor_sizes_nothing_until_the_rate_lands() {
            let mut engine = engine_on("sim", "EURUSD", RecordingClient::default());
            engine.set_account_currency("EUR").unwrap();
            engine.declare_mount_quote_currency("USD").unwrap();
            let mut core = mounted(engine, 20_000.0);
            for verb in [
                (|b: &mut LiveBroker| b.order_target_value(1_250.0)) as fn(&mut LiveBroker),
                |b| b.order_target_percent(0.5),
                |b| b.set_holdings(0.5),
                |b| {
                    b.lot_size = 1.0;
                    b.set_holdings(0.5)
                },
            ] {
                assert_eq!(
                    tick(&mut core, "sim", "EURUSD", 1.25, verb).1,
                    None,
                    "unrated: refused"
                );
            }
            core.engine
                .account
                .admit_quoted("EURUSD", "USD", None, Some((0.5, 8_000)), "EUR", 8_000)
                .unwrap();
            let (_, size) =
                tick(&mut core, "sim", "EURUSD", 1.25, |b| b.order_target_value(1_250.0));
            assert_eq!(size, Some(2_000.0));
        }
    }
}

mod lifetime_through_the_core {
    use std::sync::{Arc, Mutex};

    use super::*;
    use vike_execution::testing::RecordingClient;
    use vike_marketdata::TickQuote;
    use vike_model::TimeInForce;

    use crate::runtime::test_support::core_of;
    use crate::testkit::mount_of;
    use vike_execution::testing::engine_on;

    /// Rests ONE post-only buy at the bid on every quote.
    struct PostOnlyAtTheBid {
        seen: Arc<Mutex<usize>>,
    }
    impl Strategy<LiveBroker> for PostOnlyAtTheBid {
        fn on_quote_tick(&mut self, broker: &mut LiveBroker, q: &TickQuote) {
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
            quote: TickQuote {
                timestamp: 1,
                local_timestamp: 1,
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
    use vike_marketdata::TickQuote;
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
        /// `OrderBroker::instrument_grid` for the mount's own symbol: the gate's whole grid.
        grid: Option<vike_model::Grid>,
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
                grid: vike_model::OrderBroker::instrument_grid(b, ""),
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
        fn on_quote_tick(&mut self, b: &mut LiveBroker, _q: &TickQuote) {
            self.record("tick", b);
        }
        fn on_schedule(&mut self, b: &mut LiveBroker, _tag: &str) {
            self.record("schedule", b);
        }
        fn on_feed_status(&mut self, b: &mut LiveBroker, _status: FeedStatus) {
            self.record("feed_status", b);
        }
        fn on_mark(&mut self, b: &mut LiveBroker, _mark: &TickMark) {
            self.record("mark", b);
        }
        fn on_reference_quote(&mut self, b: &mut LiveBroker, _venue: &str, _q: &TickQuote) {
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
    ) -> Mount {
        Mount {
            order_client_id_code: None,
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
        let mut schedule = MountSchedule::new();
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
            vike_execution::PositionEntry { size: 2.0, average_price: 100.0, ..Default::default() },
        );
        core.engine.account.set_mark_from("sim", "BTCUSDT", 123.0, MarkSource::VenueMark, 0);
        // the underlying's own mark: a lane that read it in place of the mount symbol's says 7.0
        core.engine.account.set_mark_from("sim", "SPOT", 7.0, MarkSource::VenueMark, 0);
        core.engine.gate.limits.lot_size = Some(0.5);
        core.engine.gate.limits.tick_size = Some(0.25);
        core.engine.gate.limits.min_qty = Some(0.5);
        core.engine.gate.limits.min_notional = Some(10.0);
        core.engine.now_ms = 5_000;
        let equity = core.engine(0).sizing_equity(core.seed_of(0), &core.config.price_config);

        core.drive_strategy_feed_status("sim", "BTCUSDT", FeedStatus::Live);
        core.drive_strategy_flow(
            "sim",
            "BTCUSDT",
            FlowToxicity { bid: 0.25, ask: 0.75, timestamp: 1 },
        );
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
        let q = TickQuote {
            timestamp: 8_000,
            local_timestamp: 8_000,
            bid: 129.9,
            ask: 130.1,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: "BTCUSDT".into(),
        };
        core.drive_strategy_tick("sim", "BTCUSDT", 130.0, 8_000, |s, ctx| s.on_quote_tick(ctx, &q));
        let tick_equity = core.engine(0).sizing_equity(core.seed_of(0), &core.config.price_config);

        let at = |lane, mount, price, equity, now| Seen {
            lane,
            mount,
            price,
            position: 2.0,
            equity,
            lot_size: 0.5,
            grid: Some(vike_model::Grid {
                tick_size: Some(0.25),
                lot_size: Some(0.5),
                min_notional: Some(10.0),
                min_qty: Some(0.5),
            }),
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
