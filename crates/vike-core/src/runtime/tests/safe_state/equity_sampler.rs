//! The timer-armed equity sampler.

use super::*;
use crate::runtime::test_support::{engine_on, sim_engine_with};

// ---- portfolio-observer PR-3 T4: the timer-armed equity sampler ----

#[test]
fn any_position_open_ignores_zero_size_entries() {
    let mut core = test_core();
    assert!(!core.any_position_open(), "a fresh engine has no positions");
    set_position(&mut core.engine, "sim", "BTCUSDT", 1.0, 100.0);
    assert!(core.any_position_open(), "a nonzero-size entry is open");
    // closing leaves a ZERO-SIZE entry behind — the map is never shrunk back to empty
    set_position(&mut core.engine, "sim", "BTCUSDT", 0.0, 100.0);
    assert!(!core.engine.account.positions.is_empty(), "the zero-size entry stays in the map");
    assert!(!core.any_position_open(), "a zero-size entry must read as flat, never as open");
}

#[test]
fn any_position_open_scans_extra_engines() {
    let primary = sim_engine_with(RecordingClient::default());
    let extra = engine_on("bybit", "ETHUSDT", RecordingClient::default());
    let market = Arc::new(Conflated {
        state: Mutex::new(ConflatedState::default()),
        drops: AtomicU64::new(0),
    });
    let snapshot =
        Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty(&primary.venue, &primary.symbol)));
    let mut core = assemble_core(
        primary,
        vec![(0.0, extra)],
        CoreConfig::default(),
        market,
        snapshot,
        Arc::new(AtomicU64::new(0)),
    );
    assert!(!core.any_position_open(), "neither engine has a position yet");
    set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 3.0, 50.0);
    assert!(core.any_position_open(), "an open position on an EXTRA engine still counts");
}

/// Drive a fill so a position opens, advance the mock clock past the interval (via direct
/// boundary-method calls — the SAME manual-clock harness `watchdog_wheel_matches_direct_sweep_
/// transitions` above uses for the stuck-order timer), and assert the injected closure
/// received a batch with both a `"TOTAL"` row and at least one per-venue row.
#[test]
fn sampler_arms_on_open_and_fires_samples() {
    let captured = Arc::new(Mutex::new(Vec::<EquitySample>::new()));
    let cap = Arc::clone(&captured);
    let mut core = test_core();
    core.config.equity_sample = Some(Duration::from_millis(1_000));
    core.config.on_equity_sample =
        Some(Box::new(move |rows: &[EquitySample]| cap.lock().unwrap().extend_from_slice(rows)));

    // flat book: the boundary must not arm anything
    core.maintain_equity_timer(0);
    assert!(core.equity_timer.is_none(), "flat book must not arm the sampler");

    // open a position -> the boundary arms the timer for now + interval
    set_position(&mut core.engine, "sim", "BTCUSDT", 1.0, 100.0);
    core.maintain_equity_timer(0);
    assert!(core.equity_timer.is_some(), "an open position must arm the sampler");

    // advance the mock clock PAST the interval and drive the boundary — the timer fires
    core.drive_due_timers(1_000);
    let rows = captured.lock().unwrap();
    assert!(rows.iter().any(|r| r.venue == "TOTAL"), "batch must include a TOTAL row");
    assert!(rows.iter().any(|r| r.venue != "TOTAL"), "batch must include a per-venue row");
}

/// Open then fully CLOSE a position (leaving the zero-size entry `any_position_open_ignores_
/// zero_size_entries` proved is never removed) and assert the timer is cancelled: no further
/// samples fire even long past the interval that would otherwise have re-armed it.
#[test]
fn sampler_disarms_when_flat() {
    let captured = Arc::new(Mutex::new(Vec::<EquitySample>::new()));
    let cap = Arc::clone(&captured);
    let mut core = test_core();
    core.config.equity_sample = Some(Duration::from_millis(1_000));
    core.config.on_equity_sample =
        Some(Box::new(move |rows: &[EquitySample]| cap.lock().unwrap().extend_from_slice(rows)));

    set_position(&mut core.engine, "sim", "BTCUSDT", 1.0, 100.0);
    core.maintain_equity_timer(0);
    assert!(core.equity_timer.is_some(), "armed while open");

    // close the position: a CLOSED position leaves a ZERO-SIZE entry, never removed
    set_position(&mut core.engine, "sim", "BTCUSDT", 0.0, 100.0);
    assert!(!core.engine.account.positions.is_empty(), "zero-size entry stays in the map");
    core.maintain_equity_timer(500); // well before the original deadline (1000)
    assert!(core.equity_timer.is_none(), "flat must cancel the armed timer");

    // advance well past the original deadline and drive the boundary: nothing fires
    core.drive_due_timers(5_000);
    assert!(captured.lock().unwrap().is_empty(), "a cancelled timer must not fire");
}

/// The `"TOTAL"` row `sample_equity` produces must equal `py_sum` of its own per-venue rows
/// AND `CoreSnapshot`'s `equity_total` for the identical engine state — the same cross-venue
/// aggregate law, bit-for-bit (`to_bits()`), computed two ways.
#[test]
fn sample_total_equals_snapshot_law() {
    let mut core = test_core();
    set_position(&mut core.engine, "sim", "BTCUSDT", 2.0, 100.0);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 105.0, 0);
    core.engine.now_ms = 1_234;

    core.sample_equity(1_234);
    let total =
        core.equity_rows.iter().find(|r| r.venue == "TOTAL").expect("TOTAL row present").clone();
    assert!(total.equity != 0.0, "sanity: the priced position must move equity off the seed");

    let snap = crate::snapshot::build(
        1,
        &core.engine,
        &core.extra_engines,
        core.config.seed_cash,
        core.config.price_cfg,
        vike_exec::MarginCallConfig::default().mm_requirement,
        &core.recent,
        &core.bars,
        std::sync::Arc::new([]),
        &core.fault,
        0,
        0,
        vike_exec::ReconBlock::default(),
        Vec::new(),
    );
    assert_eq!(
        total.equity.to_bits(),
        snap.portfolio.equity_total.to_bits(),
        "TOTAL must match CoreSnapshot's equity_total law"
    );

    let per_venue_sum = vike_model::py_sum(
        core.equity_rows.iter().filter(|r| r.venue != "TOTAL").map(|r| r.equity),
    );
    assert_eq!(total.equity.to_bits(), per_venue_sum.to_bits(), "TOTAL == py_sum(per-venue)");
}

/// The warn-once missing-price wiring PR-2's `resolve_equity` doc comment deferred to this
/// sampler: an open position with NO priceable source on the board must be `note()`d as
/// `Missing` on that engine's OWN `PriceBoard` — the same bookkeeping `PriceBoard::note`'s
/// own unit tests (`note_tracks_missing_and_rearms_on_recovery`) exercise directly, wired
/// here through `sample_equity` instead of a bare `note()` call.
#[test]
fn sample_equity_notes_missing_prices_on_the_board() {
    let mut core = test_core();
    set_position(&mut core.engine, "sim", "BTCUSDT", 1.0, 100.0);
    // no mark/quote/trade/bar_close ever set on the board -> resolves Missing
    assert!(
        core.engine.price_board.missing_price_instruments("sim").is_none(),
        "nothing noted before the sampler ever runs"
    );

    core.sample_equity(0);

    assert!(
        core.engine
            .price_board
            .missing_price_instruments("sim")
            .is_some_and(|s| s.contains("BTCUSDT")),
        "an unpriced open position must be noted as missing on that engine's board"
    );
    let row = core.equity_rows.iter().find(|r| r.venue == "sim").expect("per-venue row present");
    assert_eq!(row.missing_prices, 1);
}

/// The recovery half of the same wiring: `PriceBoard::note`'s `Priced` arm (proven directly
/// by `PriceBoard`'s own `note_tracks_missing_and_rearms_on_recovery`) must ALSO be reached
/// through the sampler, not just `Missing`. Without it a symbol that resolves Missing once
/// and later recovers stays stuck in the per-venue missing set forever (the warn-once never
/// re-arms). Sample once with no price on the board (misses), feed the board a price, sample
/// again, and assert the symbol is gone from `missing_price_instruments`.
#[test]
fn sampler_note_clears_missing_on_recovery() {
    let mut core = test_core();
    set_position(&mut core.engine, "sim", "BTCUSDT", 1.0, 100.0);

    core.sample_equity(0);
    assert!(
        core.engine
            .price_board
            .missing_price_instruments("sim")
            .is_some_and(|s| s.contains("BTCUSDT")),
        "unpriced open position must be noted missing on the first sample"
    );

    // the board now has a price for the position -> the NEXT sample must resolve it Priced
    // and CLEAR the stale missing entry, not leave it stuck forever.
    core.engine.price_board.set_mark("sim", "BTCUSDT", 100.0, 1);
    core.sample_equity(1);

    assert!(
        core.engine
            .price_board
            .missing_price_instruments("sim")
            .is_none_or(|s| !s.contains("BTCUSDT")),
        "a position that resolves Priced must be note()'d too, clearing it from the missing set"
    );
}
