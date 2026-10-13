use super::*;

fn syms(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// The SPOT lane and the PERP lane are different lanes of one venue id, and the derivation must
/// land on the one the SYMBOL names. This is the whole reason the `vike-catalog` edge exists:
/// keyed off `venue` alone, a `.P` slice would be charged the spot rates.
#[test]
fn the_perp_suffix_selects_its_own_lane() {
    let spot = CostModel::resolve("binance", &syms(&["BTCUSDT"]), None);
    let perp = CostModel::resolve("binance", &syms(&["BTCUSDT.P"]), None);
    assert_eq!(spot.lane(), Some("binance"));
    assert_eq!(perp.lane(), Some("binance-perp"));
    // Not merely a different KEY — a different SCHEDULE. (The rates themselves are
    // `vike_model::money::fees`' to state; this asserts only that the two disagree.)
    assert_ne!(spot.schedule, perp.schedule, "the two lanes are priced differently");
}

/// The perp lane's maker and taker rates DIFFER, which is the property a search would rank
/// under; the spot lane's do not. Asserted as a relation rather than by copying the numbers out
/// of `vike_model::money::fees`, which is their one authority.
#[test]
fn the_perp_lane_splits_maker_from_taker_and_the_spot_lane_does_not() {
    let perp = CostModel::resolve("binance", &syms(&["BTCUSDT.P"]), None);
    assert!(perp.maker_rate < perp.taker_rate, "the perp lane rewards resting");
    let spot = CostModel::resolve("binance", &syms(&["BTCUSDT"]), None);
    assert_eq!(
        spot.maker_rate.to_bits(),
        spot.taker_rate.to_bits(),
        "binance SPOT prices maker and taker identically — the most likely Studio lane is the \
             one where a derived split buys nothing, which is why the stamp reports the mix too"
    );
}

/// PRECEDENCE, leg 1: a caller's flat rate wins and NOTHING is derived. The schedule staying
/// `None` is the load-bearing half — see `CostModel::resolve`'s second warning.
#[test]
fn an_override_suppresses_the_derivation() {
    let m = CostModel::resolve("binance", &syms(&["BTCUSDT.P"]), Some(0.001));
    assert_eq!(m.source_token(), "override");
    assert_eq!(m.source, CostSource::Override { fee_rate: 0.001 });
    assert_eq!(m.schedule, None, "an override must not also derive a schedule");
    assert_eq!(m.maker_rate, 0.001);
    assert_eq!(m.taker_rate, 0.001);
    assert_eq!(m.lane(), None, "an overridden run resolved no lane");
}

/// …and an override of ZERO is still an override. A caller who deliberately prices a run at
/// zero said something, and it must not read as "nothing was derived".
#[test]
fn an_explicit_zero_is_an_override_not_an_absence() {
    let m = CostModel::resolve("binance", &syms(&["BTCUSDT"]), Some(0.0));
    assert_eq!(m.source_token(), "override");
    assert_eq!(m.reason(), None);
}

/// PRECEDENCE, leg 3: two symbols on two lanes cannot share one schedule, so nothing is
/// derived and the REASON names both lanes. Guessing either one would be the bare-venue bug.
#[test]
fn a_slice_straddling_two_lanes_derives_nothing_and_says_why() {
    let m = CostModel::resolve("binance", &syms(&["BTCUSDT", "ETHUSDT.P"]), None);
    assert_eq!(m.source_token(), "none");
    assert_eq!(m.schedule, None);
    let reason = m.reason().expect("a none model always carries its reason");
    assert!(reason.contains("binance-perp"), "the reason names the lanes: {reason}");
    assert!(reason.contains("different fee lanes"), "{reason}");
}

/// A multi-symbol slice that stays on ONE lane derives normally — the refusal above is about
/// disagreement, not about symbol count.
#[test]
fn a_multi_symbol_slice_on_one_lane_still_derives() {
    let m = CostModel::resolve("binance", &syms(&["BTCUSDT", "ETHUSDT"]), None);
    assert_eq!(m.lane(), Some("binance"));
}

/// An empty slice derives nothing rather than panicking on the first symbol.
#[test]
fn an_empty_symbol_list_derives_nothing() {
    let m = CostModel::resolve("binance", &[], None);
    assert_eq!(m.source_token(), "none");
    assert!(m.reason().expect("reason").contains("no symbol"));
}

/// An UNKNOWN venue is `Free` and SAYS `Free` — the fee registry's fail-safe ("never invent a
/// fee"). The variant is what stops that reading as a maker/taker split that happened to be
/// zero.
#[test]
fn an_unknown_venue_derives_the_free_schedule_and_names_it() {
    let m = CostModel::resolve("no-such-venue", &syms(&["ANY"]), None);
    assert_eq!(m.source_token(), "derived");
    assert_eq!(m.variant(), "Free");
    assert_eq!(m.maker_rate, 0.0);
}

/// A lane whose shape has NO flat equivalent reports a zero pair through `maker_taker_rates`,
/// and the variant is the only thing that keeps that honest. IBKR's per-share schedule is the
/// case in the registry today.
#[test]
fn a_shape_with_no_flat_equivalent_is_named_rather_than_read_as_free() {
    let m = CostModel::resolve("ibkr", &syms(&["AAPL"]), None);
    assert_eq!(m.variant(), "PerShareWithFloor");
    assert_eq!(
        (m.maker_rate, m.taker_rate),
        (0.0, 0.0),
        "this shape has no flat pair — the VARIANT is what tells a reader that, not the rates"
    );
}

/// Applying a DERIVED model installs the schedule; applying an override installs nothing, so
/// the caller's `fee_rate` survives into the engine.
#[test]
fn apply_installs_a_schedule_only_for_a_derived_model() {
    let mut derived_params = EngineParams::default();
    CostModel::resolve("bybit", &syms(&["BTCUSDT"]), None).apply(&mut derived_params);
    assert!(derived_params.fee_schedule.is_some(), "a derived model installs its schedule");

    let mut overridden = EngineParams { fee_rate: 0.002, ..EngineParams::default() };
    CostModel::resolve("bybit", &syms(&["BTCUSDT"]), Some(0.002)).apply(&mut overridden);
    assert_eq!(
        overridden.fee_schedule, None,
        "an override installs NO schedule — StrategyEngine::new prefers a schedule over the \
             flat rate, so installing one here would discard the override in silence"
    );
    assert_eq!(overridden.fee_rate, 0.002);
}

/// The list is carried, not summarized: every term `0063` ruled on and this run does not
/// price, each naming itself.
#[test]
fn the_not_modelled_list_names_every_unpriced_term() {
    let joined = NOT_MODELLED.join(" | ");
    for term in ["impact", "risk", "resolution", "snap_to_properties", "attach_funding"] {
        assert!(joined.contains(term), "the not-modelled list names {term}: {joined}");
    }
}

/// **The two CLASSES stay distinguishable**, because a reader does different things about
/// them: a declared residual is not fixable by widening the wire, while a DTO omission is a
/// boolean nobody has built yet. A list that flattened the two would tell a reader that
/// funding is as underivable as an impact calibration, which is false — `0063` ruled the
/// information server-side.
///
/// This is the test that goes red when either boolean SHIPS: its row must leave
/// [`NOT_MODELLED`] at the same time, or the stamp starts under-reporting what it priced.
#[test]
fn a_dto_omission_is_labelled_as_one_and_a_residual_is_not() {
    let omissions: Vec<&str> =
        NOT_MODELLED.iter().copied().filter(|e| e.contains("DTO omission")).collect();
    assert_eq!(omissions.len(), 2, "exactly the two booleans 0063 called expressible");
    for e in &omissions {
        assert!(
            e.starts_with("snap_to_properties") || e.starts_with("attach_funding"),
            "an omission row names its boolean first: {e}"
        );
    }
    for e in NOT_MODELLED.iter().filter(|e| !e.contains("DTO omission")) {
        assert!(
            e.starts_with("impact") || e.starts_with("risk") || e.starts_with("resolution"),
            "a residual row is one of 0063's three, named first: {e}"
        );
    }
}

/// The mix SUMS across the runs it is fed, because a walk-forward's reported numbers come from
/// several out-of-sample windows.
#[test]
fn the_mix_accumulates_across_runs() {
    let a = BacktestResult { maker_fills: 2, taker_fills: 3, fees_paid: 1.5, ..Default::default() };
    let b =
        BacktestResult { maker_fills: 1, taker_fills: 0, fees_paid: 0.25, ..Default::default() };
    let mut mix = FillMix::of(&a);
    mix.add(&b);
    assert_eq!(mix.maker_fills, 3);
    assert_eq!(mix.taker_fills, 3);
    assert_eq!(mix.fees_paid, 1.75);
}
