use super::*;
use std::cell::RefCell;

fn properties(tick: f64, step: f64) -> vike_model::SymbolProperties {
    vike_model::SymbolProperties { tick_size: tick, step_size: step, ..Default::default() }
}

/// THE BYTE-IDENTICAL PROPERTY, and the one that cannot be observed from the RESULT: an empty
/// map comes back from "no legs", from "every leg was the primary" and from "every lookup
/// failed" alike, so only the CALL COUNT distinguishes a mount that touched the venue from one
/// that did not.
///
/// NON-VACUOUS: the closure panics rather than merely counting, so any future edit that probes
/// the venue "just to warm a cache" — the exact laziness bug `recon_if_enabled` exists to
/// prevent one layer up — fails here instead of costing a silent round trip at every mount.
#[test]
fn an_empty_declaration_never_touches_the_venue() {
    let grids = declared_symbol_grids("BTCUSDT", &[], |_| {
        panic!("a mount with no declared legs must not ask the venue anything")
    });
    assert!(grids.is_empty());
}

/// A declared leg gets ITS OWN grid, keyed by symbol.
///
/// NON-VACUOUS: it asserts the ETH values (`0.01`/`0.0001`), not merely that a row exists, so
/// a helper that inserted the PRIMARY's properties under the leg's name — the shape of the bug
/// being fixed — fails.
#[test]
fn a_declared_leg_carries_its_own_tick_and_lot() {
    let legs = vec!["ETHUSDT".to_string()];
    let grids = declared_symbol_grids("BTCUSDT", &legs, |s| match s {
        "ETHUSDT" => Some(properties(0.01, 0.0001)),
        _ => None,
    });
    let eth = grids.get("ETHUSDT").expect("the declared leg is gridded");
    assert_eq!(eth.tick_size, Some(0.01));
    assert_eq!(eth.lot_size, Some(0.0001));
    assert_eq!(grids.len(), 1);
}

/// The mounted symbol is never given a row of its own: the scalars already ARE its grid, and a
/// second copy could only drift from them. A caller that passes the primary through is
/// therefore identical to one that filters it out first.
///
/// NON-VACUOUS: the closure records what it was asked, so this fails on a helper that skipped
/// the ROW while still paying for the LOOKUP — the network cost being avoided, not just the
/// duplicate key.
#[test]
fn the_mounted_symbol_is_never_re_gridded() {
    let asked = RefCell::new(Vec::new());
    let legs = vec!["BTCUSDT".to_string(), "  ".to_string(), String::new()];
    let grids = declared_symbol_grids("BTCUSDT", &legs, |s| {
        asked.borrow_mut().push(s.to_string());
        Some(properties(0.1, 0.001))
    });
    assert!(grids.is_empty(), "no row for the mounted symbol, none for blanks");
    assert!(asked.borrow().is_empty(), "and no lookup was paid for either");
}

/// A repeated leg is looked up ONCE. Two mounts declaring the same hedge symbol is the ordinary
/// case, and on a venue whose lookup is a network call the duplicate would be a duplicate round
/// trip.
#[test]
fn a_repeated_leg_is_looked_up_once() {
    let calls = RefCell::new(0usize);
    let legs = vec!["ETHUSDT".to_string(), "ETHUSDT".to_string()];
    let grids = declared_symbol_grids("BTCUSDT", &legs, |_| {
        *calls.borrow_mut() += 1;
        Some(properties(0.01, 0.0001))
    });
    assert_eq!(grids.len(), 1);
    assert_eq!(*calls.borrow(), 1);
}

/// A failed lookup leaves NO row rather than an all-`None` one. The difference is invisible to
/// `grid_for` (both resolve to the scalars) but not to `warn_ungridded_legs`, which is the
/// only thing that tells an operator the leg is on the wrong instrument's grid.
///
/// NON-VACUOUS: `SymbolProperties::default()` folds to an all-`None` `SymbolGrid`, so a helper
/// that inserted `unwrap_or_default()` would produce a map of length 1 here and pass every
/// `grid_for` assertion — the emptiness IS the observable.
#[test]
fn a_failed_lookup_leaves_no_row_at_all() {
    let legs = vec!["NOPE".to_string()];
    let grids = declared_symbol_grids("BTCUSDT", &legs, |_| None);
    assert!(grids.is_empty());
}

/// THE END-TO-END PROPERTY, composed exactly as `make_engine_with_legs` composes it: a
/// TWO-symbol mount rounds each leg onto ITS OWN lot.
///
/// The mount is binance `BTCUSDT` (lot `0.01`) with `ETHUSDT` (lot `0.001`) declared. An
/// `ETHUSDT` order of `0.004` is exactly four ETH lots and must survive untouched.
///
/// NON-VACUOUS — IT FAILS WITH THE CHANGE REVERTED: drop the `grid_by_symbol` assignment (or
/// make `declared_symbol_grids` return an empty map) and the order is rounded on BTC's lot
/// instead — `round_to(0.004, Some(0.01))` is `0.4` ties-even'd to `0`, i.e. `0.0` — so the gate
/// denies it `"non-positive-size"`. That pre-change verdict is asserted directly below rather
/// than merely claimed, and by REASON rather than by `!ok`, so a future weakening cannot
/// satisfy it by denying for some unrelated cause.
///
/// The values are chosen so every division is EXACT in `f64` (`0.004 / 0.001` is exactly `4.0`
/// — the scale factor is a power of two, so both literals round to doubles with the same
/// mantissa): a ties-even test whose ratio lands a half-ULP either side of `.5` would be a
/// coin-flip, not a pin.
#[test]
fn a_two_symbol_mount_judges_each_leg_on_its_own_lot() {
    let btc = properties(0.1, 0.01);
    let eth = properties(0.01, 0.001);
    let legs = vec!["ETHUSDT".to_string()];

    let mut limits = vike_exec::RiskLimits::from_properties(&btc);
    limits.grid_by_symbol = declared_symbol_grids("BTCUSDT", &legs, |s| match s {
        "ETHUSDT" => Some(eth),
        _ => None,
    });

    let order = vike_model::OrderRequest {
        client_order_id: "t".into(),
        venue: "binance".into(),
        symbol: "ETHUSDT".into(),
        side: 1,
        qty: 0.004,
        order_type: "market".into(),
        ..Default::default()
    };
    let verdict =
        vike_exec::RiskGate::new(limits.clone()).check(&order, &vike_exec::RiskContext::default());
    assert!(verdict.ok, "ETH's own 0.001 lot admits it: {verdict:?}");
    let admitted = verdict.request.expect("an admitted verdict carries the request");
    assert_eq!(admitted.qty.to_bits(), 0.004_f64.to_bits(), "qty {} != 0.004", admitted.qty);

    // …and the SAME limits with the map emptied — literally the pre-change mount — deny it.
    // Asserted here rather than trusted, because "this test would fail without the change" is
    // the claim, and this is the cheapest way to make the claim itself machine-checked.
    limits.grid_by_symbol.clear();
    let pre_change =
        vike_exec::RiskGate::new(limits).check(&order, &vike_exec::RiskContext::default());
    assert!(!pre_change.ok);
    assert_eq!(pre_change.reason, "non-positive-size");
}

/// The mounted symbol keeps the scalars — an override must not leak across symbols, and this is
/// the mount-side composition of `an_override_does_not_leak_to_other_symbols`.
#[test]
fn the_mounted_symbol_still_uses_the_mount_scalars() {
    let legs = vec!["ETHUSDT".to_string()];
    let mut limits = vike_exec::RiskLimits::from_properties(&properties(0.1, 0.01));
    limits.grid_by_symbol =
        declared_symbol_grids("BTCUSDT", &legs, |_| Some(properties(0.01, 0.001)));
    let g = limits.grid_for("BTCUSDT");
    assert_eq!(g.lot_size, Some(0.01));
    assert_eq!(g.tick_size, Some(0.1));
}

/// An UNGRIDDED leg on a mount that really fetched a grid is reported; the same leg on a mount
/// with no grid at all is not. Exercised through the predicate the warning is gated on, since
/// `tracing` output is not assertable here without a subscriber.
#[test]
fn only_a_mount_with_a_real_grid_has_anything_to_report() {
    assert!(carries_a_venue_grid(&vike_exec::RiskLimits::from_properties(&properties(0.1, 0.001))));
    assert!(
        !carries_a_venue_grid(&vike_exec::RiskLimits::new()),
        "a paper mount's all-None scalars leak nothing onto a leg, so a warning would be noise"
    );
    // `warn_ungridded_legs` must be inert on both of those, and on an empty declaration — this
    // only proves it does not panic or index out of bounds, which is what a log-only helper can
    // be tested for.
    let legs = vec!["ETHUSDT".to_string()];
    warn_ungridded_legs(
        "binance",
        DeclaredGridSource::PerSymbolFetch,
        "BTCUSDT",
        &legs,
        &IndexMap::new(),
        &vike_exec::RiskLimits::from_properties(&properties(0.1, 0.001)),
    );
    warn_ungridded_legs(
        "binance",
        DeclaredGridSource::PerSymbolFetch,
        "BTCUSDT",
        &[],
        &IndexMap::new(),
        &vike_exec::RiskLimits::new(),
    );
}
