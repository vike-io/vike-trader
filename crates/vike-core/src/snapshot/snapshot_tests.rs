use super::*;
use vike_exec::MarkSource;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, PositionEntry, RiskGate, RiskLimits};

/// One-position engine: venue "binance", symbol "BTC", long 1 @ 100, Delta mode, no
/// board prices/marks (mirrors vike-exec's `resolve_equity.rs` integration-test convention).
fn engine_with_position() -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTC",
    );
    e.account.positions.insert(
        ("binance".into(), "BTC".into(), "LONG".into()),
        PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() },
    );
    e
}

fn build(e: &ExecutionEngine<RecordingClient>, seed: f64) -> CoreSnapshot {
    CoreSnapshot::build(
        1,
        e,
        &[],
        seed,
        PriceCfg::default(),
        vike_exec::MarginCallConfig::default().mm_requirement,
        &std::collections::VecDeque::new(),
        &indexmap::IndexMap::new(),
        &[],
        &None,
        0,
        0,
        ReconBlock::default(),
        &[],
    )
}

fn build_snapshot_with_quote_only_position() -> CoreSnapshot {
    let mut e = engine_with_position();
    // bid/ask on the board, NO mark -> resolver falls through to the side-appropriate quote
    e.price_board.set_quote("binance", "BTC", 104.0, 106.0, 1);
    build(&e, 1_000.0)
}

fn build_snapshot_no_feed_and_legacy() -> (CoreSnapshot, f64) {
    let e = engine_with_position();
    let legacy_equity_total = e.account.equity_all(1_000.0);
    (build(&e, 1_000.0), legacy_equity_total)
}

/// ⚠ **The premise every cross-venue REPORT rests on, pinned against the real builder.**
/// `build` binds `let acc = &engine.account` — the PRIMARY — into the scalar
/// `Portfolio::realized_pnl`/`fees_paid` and into `CoreSnapshot::positions`. On the CI box the
/// primary is the untraded binance paper engine (`vike_tradehub::wired_markets::WIRED_MARKETS` lists it first) and
/// the traded mount is a NON-primary engine, so those three scalars structurally cannot see the
/// venue that moves — while `equity_total`, `orders` and the `*_total` folds can. This test is
/// the machine-checked statement of that asymmetry, so `vike-tradehub`'s `summary_line` tests
/// may hand-build the same shape without hand-waving that `build` really produces it.
#[test]
fn the_primary_mirroring_scalars_cannot_see_a_non_primary_engines_fills() {
    let primary = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTC",
    );
    let mut secondary = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTC",
    );
    secondary.account.realized_pnl = 12.5;
    secondary.account.fees_paid = 0.75;
    secondary.account.positions.insert(
        ("bybit".into(), "BTC".into(), "LONG".into()),
        PositionEntry { size: -0.25, avg_px: 100.0, ..Default::default() },
    );
    let snap = CoreSnapshot::build(
        1,
        &primary,
        &[(1_000.0, secondary)],
        1_000.0,
        PriceCfg::default(),
        vike_exec::MarginCallConfig::default().mm_requirement,
        &std::collections::VecDeque::new(),
        &indexmap::IndexMap::new(),
        &[],
        &None,
        0,
        0,
        ReconBlock::default(),
        &[],
    );
    // The primary-mirroring half: blind to the venue that traded.
    assert_eq!(snap.portfolio.realized_pnl, 0.0, "the scalar mirrors the PRIMARY engine");
    assert_eq!(snap.portfolio.fees_paid, 0.0, "ditto — binance paid no fees");
    assert!(snap.positions.is_empty(), "`positions` mirrors the PRIMARY venue's rows");
    // The cross-venue half: sees it.
    assert_eq!(snap.portfolio.realized_pnl_total(), 12.5);
    assert_eq!(snap.portfolio.fees_paid_total(), 0.75);
    assert_eq!(snap.portfolio.position_count(), 1);
    assert_eq!(snap.portfolio.net_position("BTC"), -0.25);
}

#[test]
fn snapshot_prices_positions_through_resolver() {
    // build an engine with a position + a board bid/ask but NO mark;
    // snapshot equity should reflect the resolver-valued position, and the
    // PositionView should carry unrealized + a Some(mark_source).
    let snap = build_snapshot_with_quote_only_position();
    let vb = &snap.portfolio.venues[0];
    assert_eq!(vb.missing_prices, 0);
    assert!(vb.unrealized != 0.0);
    assert!(snap.positions[0].mark_source.is_some());
    assert!(snap.positions[0].unrealized != 0.0);
}

#[test]
fn margin_fields_inert_when_gate_off() {
    // default RiskLimits (im_requirement None, empty im_by_symbol) → the margin machinery
    // contributes nothing: margin_used/ratio 0, free_bp == equity, no leverage/liq on legs.
    let snap = build_snapshot_with_quote_only_position();
    assert_eq!(snap.portfolio.margin_used_total, 0.0);
    let vb = &snap.portfolio.venues[0];
    assert_eq!(vb.margin_used, 0.0);
    assert_eq!(vb.margin_ratio, 0.0);
    assert_eq!(vb.free_bp.to_bits(), vb.equity.to_bits());
    assert!(snap.positions.iter().all(|p| p.leverage == 0.0 && p.liq_price == 0.0));
}

#[test]
fn snapshot_margin_used_counts_no_override_position_matching_the_gate() {
    // DIVERGENCE FIX (dedup A1): per-symbol margin armed for BTC ONLY (SetMargin writes
    // im_by_symbol, leaves the global im_requirement unset). A second position (ETH) has no
    // per-symbol override. The OLD snapshot fold used `im_for(s)` with no fallback and SKIPPED
    // ETH, understating margin_used vs what the pre-trade gate enforces. The published number
    // must now include ETH at the account's max armed rate.
    let mut im_by_symbol: indexmap::IndexMap<String, f64> = indexmap::IndexMap::new();
    im_by_symbol.insert("BTC".to_string(), 0.2);
    let limits = RiskLimits { im_by_symbol, ..RiskLimits::new() }; // im_requirement stays None
    let mut e = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "binance",
        "BTC",
    );
    // BTC long 1 @ 100, board-marked 100 → 1·100·1·0.2 = 20 (has its own rate). The
    // margin fold is resolver-priced now, so the tests feed the BOARD's mark slot (the
    // live write-sites store both `account.marks` and the board together).
    e.account.positions.insert(
        ("binance".into(), "BTC".into(), "LONG".into()),
        PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() },
    );
    e.price_board.set_mark("binance", "BTC", 100.0, 0);
    // ETH long 2 @ 50, board-marked 50, NO per-symbol override → falls back to max armed
    // (0.2): 2·50·1·0.2 = 20. Old fold skipped it (would have published 20 total).
    e.account.positions.insert(
        ("binance".into(), "ETH".into(), "LONG".into()),
        PositionEntry { size: 2.0, avg_px: 50.0, ..Default::default() },
    );
    e.price_board.set_mark("binance", "ETH", 50.0, 0);

    let snap = build(&e, 10_000.0);
    let vb = &snap.portfolio.venues[0];
    // 20 (BTC) + 20 (ETH now counted) = 40 — the gate's own basis, not the old skipped 20.
    assert_eq!(vb.margin_used.to_bits(), 40.0_f64.to_bits());
    // and the gate, checking a BTC order, computes the SAME 40 for the existing book
    // (the gate's own resolver-priced fold — the same authority the snapshot publishes):
    let gate_used = e.resolved_margin_in_use_by(&PriceCfg::default(), |(_v, s, _side), p| {
        p.margin_mode.is_cross().then(|| e.gate.limits.im_for(s).unwrap_or(0.2))
    });
    assert_eq!(vb.margin_used.to_bits(), gate_used.to_bits());
}

#[test]
fn snapshot_margin_used_with_global_im_requirement_never_consults_the_fallback() {
    // The byte-identity guard for the dedup A1 fix: with a GLOBAL `im_requirement` set,
    // `im_for(s)` is Some for EVERY symbol, so the new max-armed-rate fallback is never
    // consulted and the published number is exactly what the pre-fix fold produced —
    // no drift from the extraction. (The fallback only ever engages in the per-symbol-only
    // arm proven above; this pins the far more common global-margin path as unchanged.)
    let limits = RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() };
    let mut e = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "binance",
        "BTC",
    );
    // Two positions, neither with a per-symbol override → both price off im_requirement 0.1
    // (board-marked: the fold reads the resolver, not `account.marks`).
    e.account.positions.insert(
        ("binance".into(), "BTC".into(), "LONG".into()),
        PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() },
    );
    e.price_board.set_mark("binance", "BTC", 100.0, 0); // 1·100·1·0.1 = 10
    e.account.positions.insert(
        ("binance".into(), "ETH".into(), "LONG".into()),
        PositionEntry { size: 2.0, avg_px: 50.0, ..Default::default() },
    );
    e.price_board.set_mark("binance", "ETH", 50.0, 0); // 2·50·1·0.1 = 10
    let snap = build(&e, 10_000.0);
    // 10 + 10 = 20, folded off the global rate with the fallback untouched.
    assert_eq!(snap.portfolio.venues[0].margin_used.to_bits(), 20.0_f64.to_bits());
}

/// MAJOR-3 (the liquidation law's partition in the PUBLISHED margin number): only Cross
/// positions price into `VenueBlock::margin_used` — an Isolated position is backed by its
/// own wallet and a Cash position is fully funded, so folding them in overstated a mixed
/// account's shared margin (and understated free_bp). All-cross accounts are unchanged
/// (the dedup-A1 pins above still pass byte-identically — filter no-op).
#[test]
fn snapshot_margin_used_excludes_isolated_and_cash_positions() {
    use vike_model::MarginMode;
    let limits = RiskLimits { im_requirement: Some(0.1), ..RiskLimits::new() };
    let mut e = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "binance",
        "BTC",
    );
    // cross BTC long 1 @ 100, board-marked → 1·100·1·0.1 = 10 (the only shared-pool row;
    // the fold is resolver-priced, so the board carries the prices)
    e.account.positions.insert(
        ("binance".into(), "BTC".into(), "LONG".into()),
        PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() },
    );
    e.price_board.set_mark("binance", "BTC", 100.0, 0);
    // isolated ETH long 2 @ 50, board-marked, wallet 10 → would add 2·50·1·0.1 = 10 if counted
    e.account.positions.insert(
        ("binance".into(), "ETH".into(), "LONG".into()),
        PositionEntry {
            size: 2.0,
            avg_px: 50.0,
            margin_mode: MarginMode::Isolated,
            isolated_margin: Some(10.0),
        },
    );
    e.price_board.set_mark("binance", "ETH", 50.0, 0);
    // cash SOL long 3 @ 10, board-marked → would add 3·10·1·0.1 = 3 if counted
    e.account.positions.insert(
        ("binance".into(), "SOL".into(), "LONG".into()),
        PositionEntry {
            size: 3.0,
            avg_px: 10.0,
            margin_mode: MarginMode::Cash,
            ..Default::default()
        },
    );
    e.price_board.set_mark("binance", "SOL", 10.0, 0);

    let snap = build(&e, 10_000.0);
    let vb = &snap.portfolio.venues[0];
    // ONLY the cross row: 10 — not the unfiltered 23 (10 + iso 10 + cash 3).
    assert_eq!(vb.margin_used.to_bits(), 10.0_f64.to_bits());
    // and it matches the admitting gate's own partitioned resolver-priced fold
    // (same authority, same law, same price basis):
    let gate_used = e.resolved_margin_in_use_by(&PriceCfg::default(), |(_v, s, _side), p| {
        p.margin_mode.is_cross().then(|| e.gate.limits.im_for(s).unwrap_or(0.1))
    });
    assert_eq!(vb.margin_used.to_bits(), gate_used.to_bits());
}

#[test]
fn liq_badge_routes_by_margin_mode_at_the_one_rate() {
    // The scope-parameterized law's badge: Isolated → closed-form at the REAL maintenance
    // rate (the `im * 0.5` hardcode is dead), Cross → the shared-pool estimate, Cash → none.
    // im 0.2 on purpose: im·0.5 = 0.1 ≠ the maint rate 0.05, so the isolated assertion
    // below can only pass through the REAL-rate call, never the retired `im * 0.5` shape.
    let limits = RiskLimits { im_requirement: Some(0.2), ..RiskLimits::new() };
    let mut e = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "binance",
        "BTC",
    );
    // cross BTC long 1 @ 100 (marked 100)
    e.account.positions.insert(
        ("binance".into(), "BTC".into(), "LONG".into()),
        PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() },
    );
    e.account.set_mark_from("binance", "BTC", 100.0, MarkSource::VenueMark, 0);
    // isolated ETH long 2 @ 50 (marked 50), wallet 10
    e.account.positions.insert(
        ("binance".into(), "ETH".into(), "LONG".into()),
        PositionEntry {
            size: 2.0,
            avg_px: 50.0,
            margin_mode: vike_model::MarginMode::Isolated,
            isolated_margin: Some(10.0),
        },
    );
    e.account.set_mark_from("binance", "ETH", 50.0, MarkSource::VenueMark, 0);
    // cash SOL long 3 @ 10 (marked 10)
    e.account.positions.insert(
        ("binance".into(), "SOL".into(), "LONG".into()),
        PositionEntry {
            size: 3.0,
            avg_px: 10.0,
            margin_mode: vike_model::MarginMode::Cash,
            ..Default::default()
        },
    );
    e.account.set_mark_from("binance", "SOL", 10.0, MarkSource::VenueMark, 0);

    // seed 60 → equity 60; the ONE rate in the test builder is the watchdog default 0.05
    let mm = vike_exec::MarginCallConfig::default().mm_requirement;
    let snap = build(&e, 60.0);
    let by_sym = |s: &str| snap.positions.iter().find(|p| p.symbol == s).unwrap();

    // Cross: pool equity = 60 − (wallet 10 + iso upnl 0) = 50; cross notional = BTC's 100
    // only (SOL is Cash — outside every pool); others = 0.
    let want_cross = vike_model::cross_liquidation_price_est(50.0, 1.0, 100.0, 1.0, 0.0, mm);
    assert!(want_cross > 0.0, "scenario sanity: the cross badge must be live");
    assert_eq!(by_sym("BTC").liq_price.to_bits(), want_cross.to_bits());
    // Isolated: closed-form at im 0.2 and the REAL maint rate 0.05 (im·0.5 would be 0.1):
    let want_iso = vike_model::liquidation_price(50.0, 1, 0.2, mm);
    assert_eq!(by_sym("ETH").liq_price.to_bits(), want_iso.to_bits());
    // Cash: never liquidates → no badge.
    assert_eq!(by_sym("SOL").liq_price, 0.0);
}

// ---- The branches of `CoreSnapshot::build`'s margin helpers that only an ARMED gate (or a
// non-positive equity) reaches. Each test builds its precondition by hand, asserts it, and where a
// neighbouring branch could produce the same number it carries a control that rules it out. Every
// expected value is computed in the comment from the documented law, not read off the builder.

/// An engine on binance/BTC under `limits`. The snapshot's margin gate is armed when
/// `im_requirement` is set or `im_by_symbol` holds any row (`margin_figures`' `margin_on`).
fn engine_with_limits(limits: RiskLimits) -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "binance",
        "BTC",
    )
}

/// A global initial-margin rate `im`: the gate armed for every symbol.
fn armed(im: f64) -> RiskLimits {
    RiskLimits { im_requirement: Some(im), ..RiskLimits::new() }
}

/// Hold `entry` on binance `symbol` (keyed LONG).
fn hold(e: &mut ExecutionEngine<RecordingClient>, symbol: &str, entry: PositionEntry) {
    e.account.positions.insert(("binance".into(), symbol.into(), "LONG".into()), entry);
}

/// `build` at an explicit maintenance rate. The liq-badge tests pass `0.5` so that every expected
/// badge is exact binary arithmetic (`1 − mm = 0.5`): a rate chosen for the arithmetic, not a
/// realistic one.
fn build_at_mm(e: &ExecutionEngine<RecordingClient>, seed: f64, mm_rate: f64) -> CoreSnapshot {
    CoreSnapshot::build(
        1,
        e,
        &[],
        seed,
        PriceCfg::default(),
        mm_rate,
        &std::collections::VecDeque::new(),
        &indexmap::IndexMap::new(),
        &[],
        &None,
        0,
        0,
        ReconBlock::default(),
        &[],
    )
}

/// The ARMED values of the three block figures `margin_fields_inert_when_gate_off` can only show
/// as zeros, plus a leg's leverage. im 0.25, haircut 0.125, seed 1000, cross BTC long 10 @ 100
/// priced at 100 on the board (so it adds no uPnL): margin_used = 10·100·1·0.25 = 250, equity
/// 1000, margin_ratio = 250 / 1000 = 0.25, free_bp = `free_buying_power` = 1000 − 250 + 0 −
/// 1000·0.125 = 625 (the haircut is the engine's `required_free_bp_pct`), leverage = 1 / 0.25 = 4.
#[test]
fn an_armed_gate_publishes_free_bp_margin_ratio_and_leverage() {
    let mut e = engine_with_limits(RiskLimits { required_free_bp_pct: 0.125, ..armed(0.25) });
    hold(&mut e, "BTC", PositionEntry { size: 10.0, avg_px: 100.0, ..Default::default() });
    e.price_board.set_mark("binance", "BTC", 100.0, 0);
    let snap = build(&e, 1_000.0);
    let vb = &snap.portfolio.venues[0];
    assert_eq!(vb.equity.to_bits(), 1_000.0_f64.to_bits(), "precondition: no uPnL");
    assert_eq!(vb.margin_used.to_bits(), 250.0_f64.to_bits());
    assert_eq!(snap.portfolio.margin_used_total.to_bits(), 250.0_f64.to_bits());
    assert_eq!(vb.margin_ratio.to_bits(), 0.25_f64.to_bits());
    assert_eq!(vb.free_bp.to_bits(), 625.0_f64.to_bits());
    assert_eq!(snap.positions[0].leverage.to_bits(), 4.0_f64.to_bits());
}

/// `margin_ratio`'s `equity > 0` guard, and the armed `free_bp`'s floor, at a NON-POSITIVE equity.
/// im 0.25, cross BTC long 1 @ 100 priced at 50 on the board: uPnL −50 and margin_used =
/// 1·50·1·0.25 = 12.5, a non-zero numerator, so an unguarded ratio would be +inf at equity 0 and
/// negative below it. seed 50 → equity exactly 0; seed 20 → equity −30. At both the ratio is 0 and
/// free_bp = max(equity − 12.5 + 0 − equity·0, 0) = 0.
#[test]
fn an_armed_gate_at_non_positive_equity_publishes_zero_ratio_and_zero_free_bp() {
    let mut e = engine_with_limits(armed(0.25));
    hold(&mut e, "BTC", PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() });
    e.price_board.set_mark("binance", "BTC", 50.0, 0);
    for (seed, equity) in [(50.0, 0.0), (20.0, -30.0)] {
        let snap = build(&e, seed);
        let vb = &snap.portfolio.venues[0];
        assert_eq!(vb.equity, equity, "precondition: seed {seed}");
        assert_eq!(vb.margin_used.to_bits(), 12.5_f64.to_bits(), "precondition: seed {seed}");
        assert_eq!(vb.margin_ratio, 0.0, "seed {seed}: no ratio over a non-positive equity");
        assert_eq!(vb.free_bp, 0.0, "seed {seed}: free buying power floors at 0");
    }
}

/// The gate-OFF `free_bp` is `equity.max(0)`, so a NEGATIVE equity publishes 0, not the equity
/// (`margin_fields_inert_when_gate_off` pins the positive case, where the two agree). Default
/// limits, cross BTC long 1 @ 100 priced at 50, seed 20 → equity 20 − 50 = −30.
#[test]
fn a_gate_off_free_bp_floors_a_negative_equity_at_zero() {
    let mut e = engine_with_limits(RiskLimits::new());
    hold(&mut e, "BTC", PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() });
    e.price_board.set_mark("binance", "BTC", 50.0, 0);
    let snap = build(&e, 20.0);
    let vb = &snap.portfolio.venues[0];
    assert_eq!(vb.equity, -30.0, "precondition: a negative equity");
    assert_eq!(vb.margin_used, 0.0, "precondition: the gate is off");
    assert_eq!(vb.free_bp, 0.0);
    assert_eq!(vb.margin_ratio, 0.0);
}

/// The position view's `_ => (0.0, 0.0)` arm under an ARMED gate: a leg whose rate is 0 (the
/// `im > 0` guard) and a FLAT leg (the `size != 0` guard) publish no leverage and no liq price,
/// while BTC beside them, at the global 0.25, publishes leverage 4 — the control that the badge
/// arm is reachable here. The flat leg is ISOLATED on purpose: `0.0_f64.signum()` is `+1`, so
/// without the flat guard the closed-form law would price it as a long and publish a badge.
#[test]
fn an_armed_gate_publishes_no_badge_for_a_zero_rate_leg_or_a_flat_leg() {
    let mut im_by_symbol = indexmap::IndexMap::new();
    im_by_symbol.insert("SOL".to_string(), 0.0);
    let mut e = engine_with_limits(RiskLimits { im_by_symbol, ..armed(0.25) });
    hold(&mut e, "BTC", PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() });
    hold(&mut e, "SOL", PositionEntry { size: 1.0, avg_px: 10.0, ..Default::default() });
    let flat_isolated = PositionEntry {
        size: 0.0,
        avg_px: 50.0,
        margin_mode: vike_model::MarginMode::Isolated,
        isolated_margin: None,
    };
    hold(&mut e, "ETH", flat_isolated);
    let snap = build(&e, 1_000.0);
    let by_sym = |s: &str| snap.positions.iter().find(|p| p.symbol == s).unwrap();
    assert_eq!(by_sym("BTC").leverage.to_bits(), 4.0_f64.to_bits(), "control: the gate is armed");
    for s in ["SOL", "ETH"] {
        assert_eq!((by_sym(s).leverage, by_sym(s).liq_price), (0.0, 0.0), "{s}");
    }
}

/// A CROSS leg with no ACCOUNT mark publishes liq 0.0 even while the board prices it, because the
/// badge reads `Account::mark_of`. Control: the same leg with the account mark written publishes
/// the shared-pool estimate, so the 0.0 is the unmarked branch and not a healthy-pool 0 from the
/// law. mm 0.5, im 0.25, seed 60, cross BTC long 1 @ 100 priced at 100 → equity 60, margin_used
/// 25. Marked: P = (mm·others − pool + q·M·mark) / (M·(q − mm·|q|)) = (0 − 60 + 100) / 0.5 = 80.
#[test]
fn a_cross_leg_without_an_account_mark_publishes_no_liq_price() {
    let mut e = engine_with_limits(armed(0.25));
    hold(&mut e, "BTC", PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() });
    e.price_board.set_mark("binance", "BTC", 100.0, 0);
    let unmarked = build_at_mm(&e, 60.0, 0.5);
    let margin_used = unmarked.portfolio.venues[0].margin_used;
    assert_eq!(margin_used.to_bits(), 25.0_f64.to_bits(), "precondition: the board prices it");
    assert_eq!(unmarked.positions[0].leverage.to_bits(), 4.0_f64.to_bits(), "precondition");
    assert_eq!(unmarked.positions[0].liq_price, 0.0);
    e.account.set_mark_from("binance", "BTC", 100.0, MarkSource::VenueMark, 0);
    let marked = build_at_mm(&e, 60.0, 0.5);
    assert_eq!(marked.positions[0].liq_price.to_bits(), 80.0_f64.to_bits(), "control");
}

/// The liq-badge pool loop SKIPS a flat leg under an armed gate: a flat ISOLATED leg that still
/// carries a wallet of 10 does not take it out of the cross pool. mm 0.5, im 0.25, seed 60, no
/// board price (equity = seed); cross BTC long 1 @ 100 with ACCOUNT mark 100 (the slot the pool
/// reads). Pool 60 → BTC's badge (0 − 60 + 100) / 0.5 = 80. Folding the flat wallet in would make
/// the pool 50 and the badge (0 − 50 + 100) / 0.5 = 100.
#[test]
fn the_liq_pool_skips_a_flat_isolated_legs_wallet() {
    let mut e = engine_with_limits(armed(0.25));
    hold(&mut e, "BTC", PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() });
    e.account.set_mark_from("binance", "BTC", 100.0, MarkSource::VenueMark, 0);
    let flat_with_wallet = PositionEntry {
        size: 0.0,
        avg_px: 50.0,
        margin_mode: vike_model::MarginMode::Isolated,
        isolated_margin: Some(10.0),
    };
    hold(&mut e, "ETH", flat_with_wallet);
    let snap = build_at_mm(&e, 60.0, 0.5);
    assert_eq!(snap.portfolio.venues[0].equity, 60.0, "precondition: equity is the seed");
    let btc = snap.positions.iter().find(|p| p.symbol == "BTC").unwrap();
    assert_eq!(btc.liq_price.to_bits(), 80.0_f64.to_bits());
}

/// An ISOLATED leg with NO wallet (`isolated_margin: None`) still walls its own uPnL off the cross
/// pool, with a wallet of 0. mm 0.5, im 0.25, seed 60; cross BTC long 1 @ 100 with account mark
/// 100 and no board price; isolated ETH long 2 @ 50 priced at 60 on the board → uPnL
/// (60 − 50)·2 = 20, equity 80. Pool = 80 − (0 + 20) = 60 → BTC's badge (0 − 60 + 100) / 0.5 = 80
/// (a pool that kept the uPnL would give 40; a wallet other than 0 would move it too). ETH's own
/// badge is the closed form at its rate, long: 50·(1 − 0.25) / (1 − 0.5) = 75, leverage 4.
#[test]
fn the_liq_pool_walls_off_a_walletless_isolated_legs_upnl() {
    let mut e = engine_with_limits(armed(0.25));
    hold(&mut e, "BTC", PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() });
    e.account.set_mark_from("binance", "BTC", 100.0, MarkSource::VenueMark, 0);
    let walletless = PositionEntry {
        size: 2.0,
        avg_px: 50.0,
        margin_mode: vike_model::MarginMode::Isolated,
        isolated_margin: None,
    };
    hold(&mut e, "ETH", walletless);
    e.price_board.set_mark("binance", "ETH", 60.0, 0);
    let snap = build_at_mm(&e, 60.0, 0.5);
    assert_eq!(snap.portfolio.venues[0].equity, 80.0, "precondition: ETH's uPnL is in equity");
    let by_sym = |s: &str| snap.positions.iter().find(|p| p.symbol == s).unwrap();
    assert_eq!(by_sym("ETH").unrealized, 20.0, "precondition");
    assert_eq!(by_sym("BTC").liq_price.to_bits(), 80.0_f64.to_bits());
    assert_eq!(by_sym("ETH").liq_price.to_bits(), 75.0_f64.to_bits());
    assert_eq!(by_sym("ETH").leverage.to_bits(), 4.0_f64.to_bits());
}

/// Per-symbol margin ONLY (BTC 0.25, no global rate): the no-override ETH leg is priced into
/// margin_used at the fallback (the max armed rate, 0.25) and its notional sits in the cross pool,
/// but its own view publishes no leverage and no liq price, because `im_for("ETH")` is `None` under
/// an armed gate (`Some(None)`), which falls to the `_` arm. mm 0.5, seed 60; BTC long 1 @ 100 and
/// ETH long 2 @ 50, each priced and account-marked at its entry → equity 60, margin_used
/// 25 + 25 = 50, cross notional 100 + 100. BTC's badge has ETH's 100 as "others":
/// (0.5·100 − 60 + 100) / 0.5 = 180. Were ETH badged at 0.25 it would read (50 − 60 + 100) / 1 = 90.
#[test]
fn a_no_override_leg_under_per_symbol_margin_is_priced_but_gets_no_badge() {
    let mut im_by_symbol = indexmap::IndexMap::new();
    im_by_symbol.insert("BTC".to_string(), 0.25);
    let mut e = engine_with_limits(RiskLimits { im_by_symbol, ..RiskLimits::new() });
    for (s, size, px) in [("BTC", 1.0, 100.0), ("ETH", 2.0, 50.0)] {
        hold(&mut e, s, PositionEntry { size, avg_px: px, ..Default::default() });
        e.price_board.set_mark("binance", s, px, 0);
        e.account.set_mark_from("binance", s, px, MarkSource::VenueMark, 0);
    }
    let snap = build_at_mm(&e, 60.0, 0.5);
    let vb = &snap.portfolio.venues[0];
    assert_eq!(vb.equity, 60.0, "precondition");
    assert_eq!(vb.margin_used.to_bits(), 50.0_f64.to_bits(), "ETH is priced at the fallback");
    let by_sym = |s: &str| snap.positions.iter().find(|p| p.symbol == s).unwrap();
    assert_eq!(by_sym("BTC").leverage.to_bits(), 4.0_f64.to_bits(), "control: BTC is badged");
    assert_eq!(by_sym("BTC").liq_price.to_bits(), 180.0_f64.to_bits(), "ETH is in BTC's pool");
    assert_eq!((by_sym("ETH").leverage, by_sym("ETH").liq_price), (0.0, 0.0));
}

#[test]
fn snapshot_surfaces_the_engines_fee_schedule() {
    // fee model follow-up 1: the mount tags each engine with its resolved fee schedule and the
    // snapshot must surface it read-only on the per-venue block for the GUI cost display.
    let mut e = engine_with_position();
    let sched = vike_model::FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 };
    e.fee_schedule = Some(sched);
    let snap = build(&e, 1_000.0);
    assert_eq!(snap.portfolio.venues[0].fee_schedule, Some(sched));
    // an untagged engine surfaces None (default path unchanged).
    let untagged = engine_with_position();
    assert_eq!(build(&untagged, 1_000.0).portfolio.venues[0].fee_schedule, None);
}

/// The per-position `margin_mode`/`isolated_margin` carrier surfaces on `PositionView`
/// (feat/margin-mode-field): a default position reads Cross/None; an isolated position flows
/// its mode AND its allocated wallet straight from the account's `PositionEntry`.
#[test]
fn position_view_surfaces_the_margin_mode_carrier() {
    use vike_model::MarginMode;
    let mut e = engine_with_position(); // seeds a cross BTC long 1 @ 100
    e.account.positions.insert(
        ("binance".into(), "ETH".into(), "BOTH".into()),
        PositionEntry {
            size: 2.0,
            avg_px: 50.0,
            margin_mode: MarginMode::Isolated,
            isolated_margin: Some(40.0),
        },
    );
    let snap = build(&e, 1_000.0);
    let btc = snap.positions.iter().find(|p| p.symbol == "BTC").unwrap();
    assert_eq!(btc.margin_mode, MarginMode::Cross, "default position is cross");
    assert_eq!(btc.isolated_margin, None);
    let eth = snap.positions.iter().find(|p| p.symbol == "ETH").unwrap();
    assert_eq!(eth.margin_mode, MarginMode::Isolated);
    assert_eq!(eth.isolated_margin, Some(40.0));
}

#[test]
fn snapshot_no_feed_equity_is_byte_identical() {
    // no board prices, no marks -> equity_total bits identical to the legacy law.
    let (snap, legacy_equity_total) = build_snapshot_no_feed_and_legacy();
    assert_eq!(snap.portfolio.equity_total.to_bits(), legacy_equity_total.to_bits());
    assert_eq!(snap.portfolio.missing_prices_total, snap.positions.len() as u32);
    assert!(snap.positions.iter().all(|p| p.mark_source.is_none()));
}

/// `CoreSnapshot::build` (portfolio-observer PR-4 T5) must thread the given `&[MountView]`
/// straight into `snap.mounts`, preserving each mount's `ready` flag — the runtime's own
/// readiness state (Pending vs. Ready) surfaced for the GUI/control-boundary read side.
#[test]
fn snapshot_exposes_mount_readiness() {
    let e = engine_with_position();
    let mounts = vec![
        MountView {
            kind: MountRowKind::Mount,
            venue: "sim".into(),
            symbol: "BTC".into(),
            interval: "1m".into(),
            ready: false,
            position: 0.0,
            realized_pnl: 0.0,
            unrealized_pnl: 0.0,
            notional: 0.0,
            budget: None,
            latched: false,
            params: None,
        },
        MountView {
            kind: MountRowKind::Mount,
            venue: "sim".into(),
            symbol: "ETH".into(),
            interval: "5m".into(),
            ready: true,
            position: 0.0,
            realized_pnl: 0.0,
            unrealized_pnl: 0.0,
            notional: 0.0,
            budget: None,
            latched: false,
            params: None,
        },
    ];
    let snap = CoreSnapshot::build(
        1,
        &e,
        &[],
        1_000.0,
        PriceCfg::default(),
        vike_exec::MarginCallConfig::default().mm_requirement,
        &std::collections::VecDeque::new(),
        &indexmap::IndexMap::new(),
        &mounts,
        &None,
        0,
        0,
        ReconBlock::default(),
        &[],
    );
    assert_eq!(snap.mounts, mounts, "build() must thread the mount views through verbatim");
    assert!(!snap.mounts[0].ready, "a Pending mount must surface ready: false");
    assert!(snap.mounts[1].ready, "a Ready mount must surface ready: true");
}

/// An engine whose account carries a bespoke multiplier grid (the deribit-options shape:
/// contract size != 1), holding a position in exactly ONE of the graded symbols.
fn engine_with_multipliers() -> ExecutionEngine<RecordingClient> {
    let mut mults: indexmap::IndexMap<String, f64> = indexmap::IndexMap::new();
    mults.insert("BTC-PERP".to_string(), 10.0);
    // deliberately NEVER traded — the options confirm-ticket case
    mults.insert("BTC-30AUG26-90000-C".to_string(), 0.1);
    let mut e = ExecutionEngine::new(
        Account::new(1.0, "deribit", Some(mults), BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "deribit",
        "BTC-PERP",
    );
    e.account.positions.insert(
        ("deribit".into(), "BTC-PERP".into(), "LONG".into()),
        PositionEntry { size: 1.0, avg_px: 100.0, ..Default::default() },
    );
    e
}

#[test]
fn snapshot_exposes_the_engine_contract_multiplier() {
    let e = engine_with_multipliers();
    let snap = build(&e, 1_000.0);
    // The snapshot must agree with the ONE authority, `Account::multiplier_of`.
    assert_eq!(snap.multiplier_of("deribit", "BTC-PERP"), 10.0);
    assert_eq!(snap.multiplier_of("deribit", "BTC-PERP"), e.account.multiplier_of("BTC-PERP"));
    assert_eq!(snap.portfolio.venues[0].multiplier_of("BTC-PERP"), 10.0);
}

#[test]
fn snapshot_multiplier_resolves_for_a_symbol_with_no_position() {
    // THE motivating case: the deribit options confirm-ticket prices a contract the account
    // has never traded. If this ever regresses to "position-only", the pre-submit notional cap
    // silently under-measures options notional again (the #458/#468/#477 bug class).
    let e = engine_with_multipliers();
    let snap = build(&e, 1_000.0);
    assert!(
        snap.position("deribit", "BTC-30AUG26-90000-C").is_none(),
        "precondition: the account holds no position in this symbol"
    );
    assert_eq!(snap.multiplier_of("deribit", "BTC-30AUG26-90000-C"), 0.1);
}

#[test]
fn snapshot_multiplier_defaults_to_one_for_ungraded_symbols_and_venues() {
    let e = engine_with_multipliers();
    let snap = build(&e, 1_000.0);
    // absent from the grid → the account's legacy scalar default (1.0 here)
    assert_eq!(snap.multiplier_of("deribit", "ETH-PERP"), 1.0);
    assert_eq!(snap.portfolio.venues[0].multiplier_default, 1.0);
    // unknown venue → 1.0 (documented permissive answer, NOT proof of an unlevered instrument)
    assert_eq!(snap.multiplier_of("nosuchvenue", "BTC-PERP"), 1.0);
    // an engine built with no grid at all publishes an empty map, every symbol 1.0
    let plain = build(&engine_with_position(), 1_000.0);
    assert!(plain.portfolio.venues[0].multipliers.is_empty());
    assert_eq!(plain.multiplier_of("binance", "BTC"), 1.0);
}

#[test]
fn snapshot_multiplier_grid_is_shared_by_pointer_not_deep_cloned() {
    // The publish path is coalesced only while busy — it also runs on every idle transition, so
    // per event on a sporadic feed — and must not deep-clone per venue per tick: two
    // successive builds share ONE allocation with the engine's account.
    let e = engine_with_multipliers();
    let a = build(&e, 1_000.0);
    let b = build(&e, 1_000.0);
    assert!(std::sync::Arc::ptr_eq(
        &a.portfolio.venues[0].multipliers,
        &b.portfolio.venues[0].multipliers
    ));
    assert!(std::sync::Arc::ptr_eq(
        &a.portfolio.venues[0].multipliers,
        &e.account.multiplier_grid()
    ));
}

#[test]
fn snapshot_exposure_helpers_price_off_the_published_marks() {
    let mut e = engine_with_position(); // binance BTC long 1 @ 100 (mult 1)
    e.account.positions.insert(
        ("binance".into(), "ETH".into(), "BOTH".into()),
        PositionEntry { size: -2.0, avg_px: 60.0, ..Default::default() },
    );
    e.account.set_mark_from("binance", "BTC", 105.0, MarkSource::VenueMark, 0);
    e.account.set_mark_from("binance", "ETH", 50.0, MarkSource::VenueMark, 0);
    let snap = build(&e, 1_000.0);
    // BTC 1·105 = +105 ; ETH -2·50 = -100.
    assert_eq!(snap.net_exposure("binance", "BTC").to_bits(), 105.0_f64.to_bits());
    assert_eq!(snap.net_exposure("binance", "ETH").to_bits(), (-100.0_f64).to_bits());
    // cross-venue total for BTC = 105 (only binance holds it).
    assert_eq!(snap.net_exposure_total("BTC").to_bits(), 105.0_f64.to_bits());
    // gross never nets long vs short: 105 + 100 = 205.
    assert_eq!(snap.gross_exposure().to_bits(), 205.0_f64.to_bits());
    // unknown venue / symbol → 0.0.
    assert_eq!(snap.net_exposure("okx", "BTC"), 0.0);
    assert_eq!(snap.net_exposure("binance", "NOPE"), 0.0);
}

#[test]
fn snapshot_exposure_folds_the_contract_multiplier() {
    // deribit BTC-PERP has contract multiplier 10 → notional folds the multiplier.
    let mut e = engine_with_multipliers(); // deribit BTC-PERP long 1 @ 100, mult 10
    e.account.set_mark_from("deribit", "BTC-PERP", 100.0, MarkSource::VenueMark, 0);
    let snap = build(&e, 1_000.0);
    assert_eq!(snap.net_exposure("deribit", "BTC-PERP").to_bits(), 1_000.0_f64.to_bits());
    assert_eq!(snap.gross_exposure().to_bits(), 1_000.0_f64.to_bits());
}

/// The block names what its engine trades and what stands behind it (Trade window spec §4.3): the
/// primary symbol, then `extra_symbols`, which is exactly `accepts_symbol`'s set, and the mode.
#[test]
fn a_venue_block_names_its_symbols_and_mode() {
    let mut e = engine_with_position();
    e.extra_symbols = vec!["ETH".into()];
    e.mode = vike_exec::EngineMode::Demo;
    let snap = build(&e, 1_000.0);
    let vb = &snap.portfolio.venues[0];
    assert_eq!(vb.symbol, "BTC");
    assert_eq!(vb.extra_symbols, vec!["ETH".to_string()]);
    assert_eq!(vb.mode, Some(vike_exec::EngineMode::Demo));
    for s in ["BTC", "ETH", "SOL"] {
        assert_eq!(vb.trades(s), e.accepts_symbol(s), "{s}: the block and the engine agree");
    }
}

/// The unnamed default block says nothing, so a reader treats it as "not said".
#[test]
fn a_default_venue_block_names_no_symbol_and_no_mode() {
    let vb = VenueBlock::default();
    assert!(vb.symbol.is_empty() && vb.extra_symbols.is_empty());
    assert_eq!(vb.mode, None);
    assert!(!vb.trades("BTC"));
}

/// Every order names the account whose book it rests in: none for the default account, the label
/// for a labelled one. Otherwise a two-account venue's ladders would mix (Trade window spec §4.3).
#[test]
fn an_order_names_the_account_its_engine_holds() {
    let req = vike_model::OrderRequest {
        client_order_id: "c-1".into(),
        venue: "binance".into(),
        symbol: "BTC".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(99.0),
        ..Default::default()
    };
    let mut e = engine_with_position();
    e.submit_order(&req, 0, &mut vike_exec::Outbox::default());
    assert_eq!(build(&e, 1_000.0).orders[0].account, None, "the default account names none");
    e.route_key = "binance#SUB".into();
    let snap = build(&e, 1_000.0);
    assert_eq!(snap.orders[0].account.as_ref().map(ToString::to_string).as_deref(), Some("SUB"));
}
