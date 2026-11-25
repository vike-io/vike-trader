use super::*;
use vike_execution::MarkSource;
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, PositionEntry, RiskGate};
use vike_model::RiskLimits;
use vike_model::events::PositionSide;

/// One-position engine: venue "binance", symbol "BTC", long 1 @ 100, Delta mode, no
/// board prices/marks (mirrors vike-execution's `resolve_equity.rs` integration-test convention).
fn engine_with_position() -> Engine<RecordingClient> {
    let mut e = Engine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTC",
    );
    e.account.positions.insert(
        ("binance".into(), "BTC".into(), PositionSide::Long),
        PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() },
    );
    e
}

/// `build` over `primary` plus `extras` at seed `seed` and maintenance rate `mm_rate`, every other input
/// empty (no mounts, no recon, no portfolio history) — the one spelling of the fourteen-argument call
/// these suites share, `account_fields_tests`' `build_of` included.
pub(super) fn build_with(
    primary: &Engine<RecordingClient>,
    extras: &[(f64, Engine<RecordingClient>)],
    seed: f64,
    mm_rate: f64,
) -> CoreSnapshot {
    build(
        1,
        primary,
        extras,
        seed,
        PriceBoardConfig::default(),
        mm_rate,
        &std::collections::VecDeque::new(),
        &indexmap::IndexMap::new(),
        std::sync::Arc::new([]),
        &None,
        ReconcileBlock::default(),
        Vec::new(),
        &mut OrderWindows::default(),
    )
}

/// [`build_with`] one engine at the default maintenance rate.
fn build_snap(e: &Engine<RecordingClient>, seed: f64) -> CoreSnapshot {
    build_with(e, &[], seed, vike_model::MarginCallConfig::default().maintenance_margin_requirement)
}

fn build_snapshot_with_quote_only_position() -> CoreSnapshot {
    let mut e = engine_with_position();
    // bid/ask on the board, NO mark -> resolver falls through to the side-appropriate quote
    e.price_board.set_quote("binance", "BTC", 104.0, 106.0, 1);
    build_snap(&e, 1_000.0)
}

fn build_snapshot_no_feed_and_legacy() -> (CoreSnapshot, f64) {
    let e = engine_with_position();
    let legacy_equity_total = e.account.equity_all(1_000.0);
    (build_snap(&e, 1_000.0), legacy_equity_total)
}

/// ⚠ **The premise every cross-venue REPORT rests on, pinned against the real builder.**
/// `build` binds `let account = &engine.account` — the PRIMARY — into the scalar
/// `Portfolio::realized_pnl`/`fees_paid` and into `CoreSnapshot::positions`. On the CI box the
/// primary is the untraded binance paper engine (`crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` lists it first) and
/// the traded mount is a NON-primary engine, so those three scalars structurally cannot see the
/// venue that moves — while `equity_total`, `orders` and the `*_total` folds can. This test is
/// the machine-checked statement of that asymmetry, so `vike-tradehub`'s `summary_line` tests
/// may hand-build the same shape without hand-waving that `build` really produces it.
#[test]
fn the_primary_mirroring_scalars_cannot_see_a_non_primary_engines_fills() {
    let primary = Engine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTC",
    );
    let mut secondary = Engine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTC",
    );
    secondary.account.realized_pnl = 12.5;
    secondary.account.fees_paid = 0.75;
    secondary.account.positions.insert(
        ("bybit".into(), "BTC".into(), PositionSide::Long),
        PositionEntry { size: -0.25, average_price: 100.0, ..Default::default() },
    );
    let snap = build(
        1,
        &primary,
        &[(1_000.0, secondary)],
        1_000.0,
        PriceBoardConfig::default(),
        vike_model::MarginCallConfig::default().maintenance_margin_requirement,
        &std::collections::VecDeque::new(),
        &indexmap::IndexMap::new(),
        std::sync::Arc::new([]),
        &None,
        ReconcileBlock::default(),
        Vec::new(),
        &mut OrderWindows::default(),
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

/// `trade health`'s "N open position(s) have no priceable source" and the GUI's "unpriced" badge read
/// `VenueBlock::missing_prices`: a FLAT row (size 0, what a closed position leaves behind, with no price
/// on the board) is not an open position, so it adds nothing to the count; the open row beside it does.
#[test]
fn a_flat_unpriced_row_adds_nothing_to_missing_prices() {
    let mut e = engine_with_position();
    e.account.positions.insert(
        ("binance".into(), "ETH".into(), PositionSide::Long),
        PositionEntry { size: 0.0, average_price: 0.0, ..Default::default() },
    );
    let snap = build_snap(&e, 1_000.0);
    assert_eq!(snap.positions.len(), 2, "the flat row is still published");
    assert_eq!(snap.portfolio.venues[0].missing_prices, 1, "only the open BTC row is unpriced");
    assert_eq!(snap.portfolio.missing_prices_total, 1);
}

#[test]
fn margin_fields_inert_when_gate_off() {
    // default RiskLimits (initial_margin_requirement None, empty initial_margin_by_symbol) → the margin machinery
    // contributes nothing: margin_used/ratio 0, free_buying_power == equity, no leverage/liq on legs.
    let snap = build_snapshot_with_quote_only_position();
    assert_eq!(snap.portfolio.margin_used_total, 0.0);
    let vb = &snap.portfolio.venues[0];
    assert_eq!(vb.margin_used, 0.0);
    assert_eq!(vb.margin_ratio, 0.0);
    assert_eq!(vb.free_buying_power.to_bits(), vb.equity.to_bits());
    assert!(snap.positions.iter().all(|p| p.leverage == 0.0 && p.liquidation_price == 0.0));
}

#[test]
fn snapshot_margin_used_counts_no_override_position_matching_the_gate() {
    // DIVERGENCE FIX (dedup A1): per-symbol margin armed for BTC ONLY (SetMargin writes
    // initial_margin_by_symbol, leaves the global initial_margin_requirement unset). A second position (ETH) has no
    // per-symbol override. The OLD snapshot fold used `initial_margin_for(s)` with no fallback and SKIPPED
    // ETH, understating margin_used vs what the pre-trade gate enforces. The published number
    // must now include ETH at the account's max armed rate.
    let mut im_by_symbol: indexmap::IndexMap<String, f64> = indexmap::IndexMap::new();
    im_by_symbol.insert("BTC".to_string(), 0.2);
    let limits = RiskLimits { initial_margin_by_symbol: im_by_symbol, ..RiskLimits::new() }; // initial_margin_requirement stays None
    let mut e = Engine::new(
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
        ("binance".into(), "BTC".into(), PositionSide::Long),
        PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() },
    );
    e.price_board.set_mark("binance", "BTC", 100.0, 0);
    // ETH long 2 @ 50, board-marked 50, NO per-symbol override → falls back to max armed
    // (0.2): 2·50·1·0.2 = 20. Old fold skipped it (would have published 20 total).
    e.account.positions.insert(
        ("binance".into(), "ETH".into(), PositionSide::Long),
        PositionEntry { size: 2.0, average_price: 50.0, ..Default::default() },
    );
    e.price_board.set_mark("binance", "ETH", 50.0, 0);

    let snap = build_snap(&e, 10_000.0);
    let vb = &snap.portfolio.venues[0];
    // 20 (BTC) + 20 (ETH now counted) = 40 — the gate's own basis, not the old skipped 20.
    assert_eq!(vb.margin_used.to_bits(), 40.0_f64.to_bits());
    // and the gate, checking a BTC order, computes the SAME 40 for the existing book
    // (the gate's own resolver-priced fold — the same authority the snapshot publishes):
    let gate_used =
        e.resolved_margin_in_use_by(&PriceBoardConfig::default(), |(_v, s, _side), p| {
            p.margin_mode.is_cross().then(|| e.gate.limits.initial_margin_for(s).unwrap_or(0.2))
        });
    assert_eq!(vb.margin_used.to_bits(), gate_used.to_bits());
}

#[test]
fn snapshot_margin_used_with_global_initial_margin_requirement_never_consults_the_fallback() {
    // The byte-identity guard for the dedup A1 fix: with a GLOBAL `initial_margin_requirement` set,
    // `initial_margin_for(s)` is Some for EVERY symbol, so the new max-armed-rate fallback is never
    // consulted and the published number is exactly what the pre-fix fold produced —
    // no drift from the extraction. (The fallback only ever engages in the per-symbol-only
    // arm proven above; this pins the far more common global-margin path as unchanged.)
    let limits = RiskLimits { initial_margin_requirement: Some(0.1), ..RiskLimits::new() };
    let mut e = Engine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "binance",
        "BTC",
    );
    // Two positions, neither with a per-symbol override → both price off initial_margin_requirement 0.1
    // (board-marked: the fold reads the resolver, not `account.marks`).
    e.account.positions.insert(
        ("binance".into(), "BTC".into(), PositionSide::Long),
        PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() },
    );
    e.price_board.set_mark("binance", "BTC", 100.0, 0); // 1·100·1·0.1 = 10
    e.account.positions.insert(
        ("binance".into(), "ETH".into(), PositionSide::Long),
        PositionEntry { size: 2.0, average_price: 50.0, ..Default::default() },
    );
    e.price_board.set_mark("binance", "ETH", 50.0, 0); // 2·50·1·0.1 = 10
    let snap = build_snap(&e, 10_000.0);
    // 10 + 10 = 20, folded off the global rate with the fallback untouched.
    assert_eq!(snap.portfolio.venues[0].margin_used.to_bits(), 20.0_f64.to_bits());
}

/// MAJOR-3 (the liquidation law's partition in the PUBLISHED margin number): only Cross
/// positions price into `VenueBlock::margin_used` — an Isolated position is backed by its
/// own wallet and a Cash position is fully funded, so folding them in overstated a mixed
/// account's shared margin (and understated free_buying_power). All-cross accounts are unchanged
/// (the dedup-A1 pins above still pass byte-identically — filter no-op).
#[test]
fn snapshot_margin_used_excludes_isolated_and_cash_positions() {
    use vike_model::MarginMode;
    let limits = RiskLimits { initial_margin_requirement: Some(0.1), ..RiskLimits::new() };
    let mut e = Engine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "binance",
        "BTC",
    );
    // cross BTC long 1 @ 100, board-marked → 1·100·1·0.1 = 10 (the only shared-pool row;
    // the fold is resolver-priced, so the board carries the prices)
    e.account.positions.insert(
        ("binance".into(), "BTC".into(), PositionSide::Long),
        PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() },
    );
    e.price_board.set_mark("binance", "BTC", 100.0, 0);
    // isolated ETH long 2 @ 50, board-marked, wallet 10 → would add 2·50·1·0.1 = 10 if counted
    e.account.positions.insert(
        ("binance".into(), "ETH".into(), PositionSide::Long),
        PositionEntry {
            size: 2.0,
            average_price: 50.0,
            margin_mode: MarginMode::Isolated,
            isolated_margin: Some(10.0),
        },
    );
    e.price_board.set_mark("binance", "ETH", 50.0, 0);
    // cash SOL long 3 @ 10, board-marked → would add 3·10·1·0.1 = 3 if counted
    e.account.positions.insert(
        ("binance".into(), "SOL".into(), PositionSide::Long),
        PositionEntry {
            size: 3.0,
            average_price: 10.0,
            margin_mode: MarginMode::Cash,
            ..Default::default()
        },
    );
    e.price_board.set_mark("binance", "SOL", 10.0, 0);

    let snap = build_snap(&e, 10_000.0);
    let vb = &snap.portfolio.venues[0];
    // ONLY the cross row: 10 — not the unfiltered 23 (10 + iso 10 + cash 3).
    assert_eq!(vb.margin_used.to_bits(), 10.0_f64.to_bits());
    // and it matches the admitting gate's own partitioned resolver-priced fold
    // (same authority, same law, same price basis):
    let gate_used =
        e.resolved_margin_in_use_by(&PriceBoardConfig::default(), |(_v, s, _side), p| {
            p.margin_mode.is_cross().then(|| e.gate.limits.initial_margin_for(s).unwrap_or(0.1))
        });
    assert_eq!(vb.margin_used.to_bits(), gate_used.to_bits());
}

#[test]
fn liquidation_badge_routes_by_margin_mode_at_the_one_rate() {
    // The scope-parameterized law's badge: Isolated → closed-form at the REAL maintenance
    // rate (the `initial_margin * 0.5` hardcode is dead), Cross → the shared-pool estimate, Cash → none.
    // im 0.2 on purpose: im·0.5 = 0.1 ≠ the maint rate 0.05, so the isolated assertion
    // below can only pass through the REAL-rate call, never the retired `initial_margin * 0.5` shape.
    let limits = RiskLimits { initial_margin_requirement: Some(0.2), ..RiskLimits::new() };
    let mut e = Engine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "binance",
        "BTC",
    );
    // cross BTC long 1 @ 100 (marked 100)
    e.account.positions.insert(
        ("binance".into(), "BTC".into(), PositionSide::Long),
        PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() },
    );
    e.account.set_mark_from("binance", "BTC", 100.0, MarkSource::VenueMark, 0);
    e.price_board.set_mark("binance", "BTC", 100.0, 0);
    // isolated ETH long 2 @ 50 (marked 50), wallet 10
    e.account.positions.insert(
        ("binance".into(), "ETH".into(), PositionSide::Long),
        PositionEntry {
            size: 2.0,
            average_price: 50.0,
            margin_mode: vike_model::MarginMode::Isolated,
            isolated_margin: Some(10.0),
        },
    );
    e.account.set_mark_from("binance", "ETH", 50.0, MarkSource::VenueMark, 0);
    e.price_board.set_mark("binance", "ETH", 50.0, 0);
    // cash SOL long 3 @ 10 (marked 10)
    e.account.positions.insert(
        ("binance".into(), "SOL".into(), PositionSide::Long),
        PositionEntry {
            size: 3.0,
            average_price: 10.0,
            margin_mode: vike_model::MarginMode::Cash,
            ..Default::default()
        },
    );
    e.account.set_mark_from("binance", "SOL", 10.0, MarkSource::VenueMark, 0);
    e.price_board.set_mark("binance", "SOL", 10.0, 0);

    // seed 60 → equity 60; the ONE rate in the test builder is the watchdog default 0.05
    let mm = vike_model::MarginCallConfig::default().maintenance_margin_requirement;
    let snap = build_snap(&e, 60.0);
    let by_symbol = |s: &str| snap.positions.iter().find(|p| p.symbol == s).unwrap();

    // Cross: pool equity = 60 − (wallet 10 + iso upnl 0) = 50; cross notional = BTC's 100
    // only (SOL is Cash — outside every pool); others = 0.
    let want_cross =
        vike_model::ContractValue::linear(1.0).cross_liquidation_price(50.0, 1.0, 100.0, 0.0, mm);
    assert!(want_cross > 0.0, "scenario sanity: the cross badge must be live");
    assert_eq!(by_symbol("BTC").liquidation_price.to_bits(), want_cross.to_bits());
    // Isolated: closed-form at im 0.2 and the REAL maint rate 0.05 (im·0.5 would be 0.1):
    let want_iso = vike_model::liquidation_price(50.0, 1, 0.2, mm);
    assert_eq!(by_symbol("ETH").liquidation_price.to_bits(), want_iso.to_bits());
    // Cash: never liquidates → no badge.
    assert_eq!(by_symbol("SOL").liquidation_price, 0.0);
}

// ---- The branches of `build`'s margin helpers that only an ARMED gate (or a
// non-positive equity) reaches. Each test builds its precondition by hand, asserts it, and where a
// neighbouring branch could produce the same number it carries a control that rules it out. Every
// expected value is computed in the comment from the documented law, not read off the builder.

/// An engine on binance/BTC under `limits`. The snapshot's margin gate is armed when
/// `initial_margin_requirement` is set or `initial_margin_by_symbol` holds any row (`margin_figures`' `margin_on`).
fn engine_with_limits(limits: RiskLimits) -> Engine<RecordingClient> {
    Engine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(limits),
        RecordingClient::default(),
        "binance",
        "BTC",
    )
}

/// A global initial-margin rate `initial_margin`: the gate armed for every symbol.
fn armed(initial_margin: f64) -> RiskLimits {
    RiskLimits { initial_margin_requirement: Some(initial_margin), ..RiskLimits::new() }
}

/// Hold `entry` on binance `symbol` (keyed LONG).
fn hold(e: &mut Engine<RecordingClient>, symbol: &str, entry: PositionEntry) {
    e.account.positions.insert(("binance".into(), symbol.into(), PositionSide::Long), entry);
}

/// `build` at an explicit maintenance rate. The liq-badge tests pass `0.5` so that every expected
/// badge is exact binary arithmetic (`1 − mm = 0.5`): a rate chosen for the arithmetic, not a
/// realistic one.
fn build_at_mm(e: &Engine<RecordingClient>, seed: f64, mm_rate: f64) -> CoreSnapshot {
    build_with(e, &[], seed, mm_rate)
}

/// The ARMED values of the three block figures `margin_fields_inert_when_gate_off` can only show
/// as zeros, plus a leg's leverage. im 0.25, haircut 0.125, seed 1000, cross BTC long 10 @ 100
/// priced at 100 on the board (so it adds no uPnL): margin_used = 10·100·1·0.25 = 250, equity
/// 1000, margin_ratio = 250 / 1000 = 0.25, free_buying_power = `free_buying_power` = 1000 − 250 + 0 −
/// 1000·0.125 = 625 (the haircut is the engine's `required_free_buying_power_percent`), leverage = 1 / 0.25 = 4.
#[test]
fn an_armed_gate_publishes_free_bp_margin_ratio_and_leverage() {
    let mut e =
        engine_with_limits(RiskLimits { required_free_buying_power_percent: 0.125, ..armed(0.25) });
    hold(&mut e, "BTC", PositionEntry { size: 10.0, average_price: 100.0, ..Default::default() });
    e.price_board.set_mark("binance", "BTC", 100.0, 0);
    let snap = build_snap(&e, 1_000.0);
    let vb = &snap.portfolio.venues[0];
    assert_eq!(vb.equity.to_bits(), 1_000.0_f64.to_bits(), "precondition: no uPnL");
    assert_eq!(vb.margin_used.to_bits(), 250.0_f64.to_bits());
    assert_eq!(snap.portfolio.margin_used_total.to_bits(), 250.0_f64.to_bits());
    assert_eq!(vb.margin_ratio.to_bits(), 0.25_f64.to_bits());
    assert_eq!(vb.free_buying_power.to_bits(), 625.0_f64.to_bits());
    assert_eq!(snap.positions[0].leverage.to_bits(), 4.0_f64.to_bits());
}

/// Rest a limit BUY at 100 on binance `symbol`, accepted by the venue.
fn rest(e: &mut Engine<RecordingClient>, order_client_id: &str, symbol: &str, qty: f64) {
    let mut mo = vike_model::OrderRecord::new(vike_model::OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    });
    mo.status = vike_model::OrderStatus::Accepted;
    e.registry.insert(order_client_id.to_string(), mo);
}

/// **The published `free_buying_power` is what the gate judges against, with orders resting.** im 0.5, seed 1000, BTC
/// marked 100, a resting BUY 10 at 100: 10·100·0.5 = 500 committed, so the gate holds 500 free. It was
/// published as 1000, and an operator sizing to that number was refused `insufficient-margin`. An order
/// needing 600 is refused and one needing 400 goes out, either side of the number now on the block.
/// `margin_used` stays the POSITION margin.
#[test]
fn the_published_free_bp_counts_what_resting_orders_committed() {
    let mut e = engine_with_limits(armed(0.5));
    e.equity_seed = 1_000.0;
    e.price_board.set_mark("binance", "BTC", 100.0, 0);
    rest(&mut e, "resting", "BTC", 10.0);

    let snap = build_snap(&e, 1_000.0);
    let vb = &snap.portfolio.venues[0];
    assert_eq!(vb.margin_used, 0.0, "no position: the published margin is the position margin");
    assert_eq!(
        vb.free_buying_power.to_bits(),
        500.0_f64.to_bits(),
        "free_buying_power {}",
        vb.free_buying_power
    );

    let order = |order_client_id: &str, qty: f64| vike_model::OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        symbol: "BTC".into(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    };
    e.submit_order(&order("over", 12.0), 1, &mut vike_execution::EventBusOutbox::default());
    e.submit_order(&order("under", 8.0), 1, &mut vike_execution::EventBusOutbox::default());
    let sent: Vec<&str> = e.client.submissions.iter().map(|s| s.order_client_id.as_str()).collect();
    assert_eq!(sent, ["under"], "600 is over the 500 published, 400 is not");
}

/// **The published `free_buying_power` takes the gate's ceiling.** `max_sizing_equity` 800 over an equity of 1000, haircut
/// 0.1, long 2 at 100 on BTC at im 0.5 (100 of margin) and a resting BUY 4 at 100 (200 committed): the gate holds
/// 800 − 300 + 0 − 800·0.1 = 420 free. It was published as 1000 − 100 − 100 = 800. The EQUITY keeps the real
/// figure, a report of the account (`Engine::sizing_equity`'s doc), and so does `margin_ratio`.
#[test]
fn the_published_free_bp_takes_the_sizing_ceiling_and_the_equity_does_not() {
    let mut e = engine_with_limits(RiskLimits {
        max_sizing_equity: Some(800.0),
        required_free_buying_power_percent: 0.1,
        ..armed(0.5)
    });
    hold(&mut e, "BTC", PositionEntry { size: 2.0, average_price: 100.0, ..Default::default() });
    e.price_board.set_mark("binance", "BTC", 100.0, 0);
    rest(&mut e, "resting", "BTC", 4.0);
    let snap = build_snap(&e, 1_000.0);
    let vb = &snap.portfolio.venues[0];
    assert_eq!(vb.equity.to_bits(), 1_000.0_f64.to_bits(), "equity is the uncapped report");
    assert_eq!(vb.margin_used.to_bits(), 100.0_f64.to_bits());
    assert_eq!(vb.margin_ratio.to_bits(), 0.1_f64.to_bits());
    assert_eq!(
        vb.free_buying_power.to_bits(),
        420.0_f64.to_bits(),
        "free_buying_power {}",
        vb.free_buying_power
    );
}

/// **An inverse isolated position publishes no liquidation badge**: `liquidation_price` is the LINEAR
/// closed form (`vike_model::liquidation_price`'s doc), so 60,000 long at im 0.25, maintenance 0.05 printed
/// 47,368.42 for a contract whose notional does not contain the price. It withholds as the cross arm does
/// (0.0), keeps the leverage, and a linear isolated position beside it keeps its number.
#[test]
fn an_inverse_isolated_position_publishes_no_liquidation_price() {
    use vike_model::ContractValue;
    let mut grid: indexmap::IndexMap<String, ContractValue> = indexmap::IndexMap::new();
    grid.insert("BTC-PERPETUAL".to_string(), ContractValue::inverse(10.0));
    let mut e = Engine::new(
        Account::new(1.0, "deribit", Some(grid), BalanceMode::Delta),
        RiskGate::new(armed(0.25)),
        RecordingClient::default(),
        "deribit",
        "BTC-PERPETUAL",
    );
    let isolated = |avg_px: f64| PositionEntry {
        size: 30.0,
        average_price: avg_px,
        margin_mode: vike_model::MarginMode::Isolated,
        isolated_margin: Some(10.0),
    };
    for (symbol, avg_px) in [("BTC-PERPETUAL", 60_000.0), ("ETH_USDC-PERPETUAL", 2_000.0)] {
        e.account
            .positions
            .insert(("deribit".into(), symbol.into(), PositionSide::Both), isolated(avg_px));
    }
    let mm = 0.05;
    let snap = build_with(&e, &[], 1_000.0, mm);
    let by_symbol = |s: &str| snap.positions.iter().find(|p| p.symbol == s).unwrap();
    assert_eq!(
        by_symbol("BTC-PERPETUAL").liquidation_price,
        0.0,
        "no linear level for an inverse contract"
    );
    assert_eq!(by_symbol("BTC-PERPETUAL").leverage.to_bits(), 4.0_f64.to_bits(), "leverage stays");
    let linear = vike_model::liquidation_price(2_000.0, 1, 0.25, mm);
    assert!(linear > 0.0, "scenario sanity: the linear badge is live");
    assert_eq!(by_symbol("ETH_USDC-PERPETUAL").liquidation_price.to_bits(), linear.to_bits());
}

/// `margin_ratio`'s `equity > 0` guard, and the armed `free_buying_power`'s floor, at a NON-POSITIVE equity.
/// im 0.25, cross BTC long 1 @ 100 priced at 50 on the board: uPnL −50 and margin_used =
/// 1·50·1·0.25 = 12.5, a non-zero numerator, so an unguarded ratio would be +inf at equity 0 and
/// negative below it. seed 50 → equity exactly 0; seed 20 → equity −30. At both the ratio is 0 and
/// free_buying_power = max(equity − 12.5 + 0 − equity·0, 0) = 0.
#[test]
fn an_armed_gate_at_non_positive_equity_publishes_zero_ratio_and_zero_free_bp() {
    let mut e = engine_with_limits(armed(0.25));
    hold(&mut e, "BTC", PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() });
    e.price_board.set_mark("binance", "BTC", 50.0, 0);
    for (seed, equity) in [(50.0, 0.0), (20.0, -30.0)] {
        let snap = build_snap(&e, seed);
        let vb = &snap.portfolio.venues[0];
        assert_eq!(vb.equity, equity, "precondition: seed {seed}");
        assert_eq!(vb.margin_used.to_bits(), 12.5_f64.to_bits(), "precondition: seed {seed}");
        assert_eq!(vb.margin_ratio, 0.0, "seed {seed}: no ratio over a non-positive equity");
        assert_eq!(vb.free_buying_power, 0.0, "seed {seed}: free buying power floors at 0");
    }
}

/// The gate-OFF `free_buying_power` is `equity.max(0)`, so a NEGATIVE equity publishes 0, not the equity
/// (`margin_fields_inert_when_gate_off` pins the positive case, where the two agree). Default
/// limits, cross BTC long 1 @ 100 priced at 50, seed 20 → equity 20 − 50 = −30.
#[test]
fn a_gate_off_free_bp_floors_a_negative_equity_at_zero() {
    let mut e = engine_with_limits(RiskLimits::new());
    hold(&mut e, "BTC", PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() });
    e.price_board.set_mark("binance", "BTC", 50.0, 0);
    let snap = build_snap(&e, 20.0);
    let vb = &snap.portfolio.venues[0];
    assert_eq!(vb.equity, -30.0, "precondition: a negative equity");
    assert_eq!(vb.margin_used, 0.0, "precondition: the gate is off");
    assert_eq!(vb.free_buying_power, 0.0);
    assert_eq!(vb.margin_ratio, 0.0);
}

/// The position view's `_ => (0.0, 0.0)` arm under an ARMED gate: a leg whose rate is 0 (the
/// `initial_margin > 0` guard) and a FLAT leg (the `size != 0` guard) publish no leverage and no liq price,
/// while BTC beside them, at the global 0.25, publishes leverage 4 — the control that the badge
/// arm is reachable here. The flat leg is ISOLATED on purpose: `0.0_f64.signum()` is `+1`, so
/// without the flat guard the closed-form law would price it as a long and publish a badge.
#[test]
fn an_armed_gate_publishes_no_badge_for_a_zero_rate_leg_or_a_flat_leg() {
    let mut im_by_symbol = indexmap::IndexMap::new();
    im_by_symbol.insert("SOL".to_string(), 0.0);
    let mut e =
        engine_with_limits(RiskLimits { initial_margin_by_symbol: im_by_symbol, ..armed(0.25) });
    hold(&mut e, "BTC", PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() });
    hold(&mut e, "SOL", PositionEntry { size: 1.0, average_price: 10.0, ..Default::default() });
    let flat_isolated = PositionEntry {
        size: 0.0,
        average_price: 50.0,
        margin_mode: vike_model::MarginMode::Isolated,
        isolated_margin: None,
    };
    hold(&mut e, "ETH", flat_isolated);
    let snap = build_snap(&e, 1_000.0);
    let by_symbol = |s: &str| snap.positions.iter().find(|p| p.symbol == s).unwrap();
    assert_eq!(
        by_symbol("BTC").leverage.to_bits(),
        4.0_f64.to_bits(),
        "control: the gate is armed"
    );
    for s in ["SOL", "ETH"] {
        assert_eq!((by_symbol(s).leverage, by_symbol(s).liquidation_price), (0.0, 0.0), "{s}");
    }
}

/// **A CROSS leg is badged at the price it can be closed at, never at the account slot's MID** (owner,
/// 2026-11-15): the badge reads `Engine::resolved_position_price` (a long at the bid), the price its equity
/// and margin read, not `Account::mark_of`. With the slot holding the quote's mid 110 and the board nothing,
/// there is no badge (0.0, the unpriced branch); with the board's quote 100 / 120 the long is badged at the
/// bid. mm 0.5, im 0.25, seed 60, cross BTC long 1 @ 100 at the bid 100 → equity 60, margin_used 25:
/// P = (mm·others − pool + q·M·px) / (M·(q − mm·|q|)) = (0 − 60 + 100) / 0.5 = 80. At the slot's mid 110 it
/// read (0 − 60 + 110) / 0.5 = 100.
#[test]
fn a_cross_leg_is_badged_at_its_bid_never_at_the_account_slots_mid() {
    let mut e = engine_with_limits(armed(0.25));
    hold(&mut e, "BTC", PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() });
    // The slot holds the quote's MID, as the core's tick lane (`drive_strategy_tick`) writes it.
    e.account.set_mark_from("binance", "BTC", 110.0, MarkSource::TradeTick, 0);
    let unpriced = build_at_mm(&e, 60.0, 0.5);
    assert_eq!(unpriced.positions[0].leverage.to_bits(), 4.0_f64.to_bits(), "precondition");
    assert_eq!(unpriced.positions[0].liquidation_price, 0.0, "no board price: no badge");
    e.price_board.set_quote("binance", "BTC", 100.0, 120.0, 0);
    let priced = build_at_mm(&e, 60.0, 0.5);
    let margin_used = priced.portfolio.venues[0].margin_used;
    assert_eq!(margin_used.to_bits(), 25.0_f64.to_bits(), "precondition: priced at the bid");
    assert_eq!(priced.positions[0].liquidation_price.to_bits(), 80.0_f64.to_bits(), "the bid");
}

/// The liq-badge pool loop SKIPS a flat leg under an armed gate: a flat ISOLATED leg that still
/// carries a wallet of 10 does not take it out of the cross pool. mm 0.5, im 0.25, seed 60; cross
/// BTC long 1 @ 100 priced at 100 on the board (the price the pool reads), so equity = seed. Pool 60 → BTC's badge (0 − 60 + 100) / 0.5 = 80. Folding the flat wallet in would make
/// the pool 50 and the badge (0 − 50 + 100) / 0.5 = 100.
#[test]
fn the_liquidation_pool_skips_a_flat_isolated_legs_wallet() {
    let mut e = engine_with_limits(armed(0.25));
    hold(&mut e, "BTC", PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() });
    e.account.set_mark_from("binance", "BTC", 100.0, MarkSource::VenueMark, 0);
    e.price_board.set_mark("binance", "BTC", 100.0, 0);
    let flat_with_wallet = PositionEntry {
        size: 0.0,
        average_price: 50.0,
        margin_mode: vike_model::MarginMode::Isolated,
        isolated_margin: Some(10.0),
    };
    hold(&mut e, "ETH", flat_with_wallet);
    let snap = build_at_mm(&e, 60.0, 0.5);
    assert_eq!(snap.portfolio.venues[0].equity, 60.0, "precondition: equity is the seed");
    let btc = snap.positions.iter().find(|p| p.symbol == "BTC").unwrap();
    assert_eq!(btc.liquidation_price.to_bits(), 80.0_f64.to_bits());
}

/// An ISOLATED leg with NO wallet (`isolated_margin: None`) still walls its own uPnL off the cross
/// pool, with a wallet of 0. mm 0.5, im 0.25, seed 60; cross BTC long 1 @ 100 priced at 100 on the
/// board (uPnL 0); isolated ETH long 2 @ 50 priced at 60 on the board → uPnL
/// (60 − 50)·2 = 20, equity 80. Pool = 80 − (0 + 20) = 60 → BTC's badge (0 − 60 + 100) / 0.5 = 80
/// (a pool that kept the uPnL would give 40; a wallet other than 0 would move it too). ETH's own
/// badge is the closed form at its rate, long: 50·(1 − 0.25) / (1 − 0.5) = 75, leverage 4.
#[test]
fn the_liquidation_pool_walls_off_a_walletless_isolated_legs_unrealized_pnl() {
    let mut e = engine_with_limits(armed(0.25));
    hold(&mut e, "BTC", PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() });
    e.account.set_mark_from("binance", "BTC", 100.0, MarkSource::VenueMark, 0);
    e.price_board.set_mark("binance", "BTC", 100.0, 0);
    let walletless = PositionEntry {
        size: 2.0,
        average_price: 50.0,
        margin_mode: vike_model::MarginMode::Isolated,
        isolated_margin: None,
    };
    hold(&mut e, "ETH", walletless);
    e.price_board.set_mark("binance", "ETH", 60.0, 0);
    let snap = build_at_mm(&e, 60.0, 0.5);
    assert_eq!(snap.portfolio.venues[0].equity, 80.0, "precondition: ETH's uPnL is in equity");
    let by_symbol = |s: &str| snap.positions.iter().find(|p| p.symbol == s).unwrap();
    assert_eq!(by_symbol("ETH").unrealized, 20.0, "precondition");
    assert_eq!(by_symbol("BTC").liquidation_price.to_bits(), 80.0_f64.to_bits());
    assert_eq!(by_symbol("ETH").liquidation_price.to_bits(), 75.0_f64.to_bits());
    assert_eq!(by_symbol("ETH").leverage.to_bits(), 4.0_f64.to_bits());
}

/// Per-symbol margin ONLY (BTC 0.25, no global rate): the no-override ETH leg is priced into
/// margin_used at the fallback (the max armed rate, 0.25) and its notional sits in the cross pool,
/// but its own view publishes no leverage and no liq price, because `initial_margin_for("ETH")` is `None` under
/// an armed gate (`Some(None)`), which falls to the `_` arm. mm 0.5, seed 60; BTC long 1 @ 100 and
/// ETH long 2 @ 50, each priced and account-marked at its entry → equity 60, margin_used
/// 25 + 25 = 50, cross notional 100 + 100. BTC's badge has ETH's 100 as "others":
/// (0.5·100 − 60 + 100) / 0.5 = 180. Were ETH badged at 0.25 it would read (50 − 60 + 100) / 1 = 90.
#[test]
fn a_no_override_leg_under_per_symbol_margin_is_priced_but_gets_no_badge() {
    let mut im_by_symbol = indexmap::IndexMap::new();
    im_by_symbol.insert("BTC".to_string(), 0.25);
    let mut e = engine_with_limits(RiskLimits {
        initial_margin_by_symbol: im_by_symbol,
        ..RiskLimits::new()
    });
    for (s, size, px) in [("BTC", 1.0, 100.0), ("ETH", 2.0, 50.0)] {
        hold(&mut e, s, PositionEntry { size, average_price: px, ..Default::default() });
        e.price_board.set_mark("binance", s, px, 0);
        e.account.set_mark_from("binance", s, px, MarkSource::VenueMark, 0);
    }
    let snap = build_at_mm(&e, 60.0, 0.5);
    let vb = &snap.portfolio.venues[0];
    assert_eq!(vb.equity, 60.0, "precondition");
    assert_eq!(vb.margin_used.to_bits(), 50.0_f64.to_bits(), "ETH is priced at the fallback");
    let by_symbol = |s: &str| snap.positions.iter().find(|p| p.symbol == s).unwrap();
    assert_eq!(by_symbol("BTC").leverage.to_bits(), 4.0_f64.to_bits(), "control: BTC is badged");
    assert_eq!(
        by_symbol("BTC").liquidation_price.to_bits(),
        180.0_f64.to_bits(),
        "ETH is in BTC's pool"
    );
    assert_eq!((by_symbol("ETH").leverage, by_symbol("ETH").liquidation_price), (0.0, 0.0));
}

#[test]
fn snapshot_surfaces_the_engines_fee_schedule() {
    // fee model follow-up 1: the mount tags each engine with its resolved fee schedule and the
    // snapshot must surface it read-only on the per-venue block for the GUI cost display.
    let mut e = engine_with_position();
    let sched = vike_model::FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 };
    e.fee_schedule = Some(sched);
    let snap = build_snap(&e, 1_000.0);
    assert_eq!(snap.portfolio.venues[0].fee_schedule, Some(sched));
    // an untagged engine surfaces None (default path unchanged).
    let untagged = engine_with_position();
    assert_eq!(build_snap(&untagged, 1_000.0).portfolio.venues[0].fee_schedule, None);
}

/// The per-position `margin_mode`/`isolated_margin` carrier surfaces on `PositionView`
/// (feat/margin-mode-field): a default position reads Cross/None; an isolated position flows
/// its mode AND its allocated wallet straight from the account's `PositionEntry`.
#[test]
fn position_view_surfaces_the_margin_mode_carrier() {
    use vike_model::MarginMode;
    let mut e = engine_with_position(); // seeds a cross BTC long 1 @ 100
    e.account.positions.insert(
        ("binance".into(), "ETH".into(), PositionSide::Both),
        PositionEntry {
            size: 2.0,
            average_price: 50.0,
            margin_mode: MarginMode::Isolated,
            isolated_margin: Some(40.0),
        },
    );
    let snap = build_snap(&e, 1_000.0);
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

/// `build` (portfolio-observer PR-4 T5) must thread the given `Vec<MountView>`
/// straight into `snap.mounts`, preserving each mount's `ready` flag — the runtime's own
/// readiness state (Pending vs. Ready) surfaced for the GUI/control-boundary read side.
#[test]
fn snapshot_exposes_mount_readiness() {
    let e = engine_with_position();
    let mounts = vec![
        MountView {
            kind: MountViewKind::Mount,
            mount_id: "sim__BTC__1m".into(),
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
            currency: Default::default(),
            legs: None,
        },
        MountView {
            kind: MountViewKind::Mount,
            mount_id: "sim__ETH__5m".into(),
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
            currency: Default::default(),
            legs: None,
        },
    ];
    let snap = build(
        1,
        &e,
        &[],
        1_000.0,
        PriceBoardConfig::default(),
        vike_model::MarginCallConfig::default().maintenance_margin_requirement,
        &std::collections::VecDeque::new(),
        &indexmap::IndexMap::new(),
        mounts.clone().into(),
        &None,
        ReconcileBlock::default(),
        Vec::new(),
        &mut OrderWindows::default(),
    );
    assert_eq!(*snap.mounts, mounts[..], "build() must thread the mount views through verbatim");
    assert!(!snap.mounts[0].ready, "a Pending mount must surface ready: false");
    assert!(snap.mounts[1].ready, "a Ready mount must surface ready: true");
}

/// One mount row whose four `String`s are real heap buffers, so a test can tell a MOVED row from
/// a cloned one by the address of those buffers.
fn mount_row(symbol: &str) -> MountView {
    MountView {
        kind: MountViewKind::Mount,
        mount_id: format!("sim__{symbol}__1m"),
        venue: "sim".into(),
        symbol: symbol.into(),
        interval: "1m".into(),
        ready: true,
        position: 0.0,
        realized_pnl: 0.0,
        unrealized_pnl: 0.0,
        notional: 0.0,
        budget: None,
        latched: false,
        params: None,
        currency: Default::default(),
        legs: None,
    }
}

/// `build` takes the mount list (an `Arc`) and the held exits BY VALUE and stores them:
/// `build` runs on every publish, per event on a sporadic feed, and a second copy of every row and
/// its four `String`s is the cost this pins away. The `Arc` handed in IS the one published, each
/// row's `String` buffers keep their addresses, and so does the held-exit buffer; a clone would hand
/// the snapshot fresh ones.
#[test]
fn build_moves_the_mount_rows_and_held_exits_it_is_given() {
    let e = engine_with_position();
    let rows = vec![mount_row("BTC"), mount_row("ETH")];
    let strings_at: Vec<[*const u8; 4]> = rows
        .iter()
        .map(|m| [m.mount_id.as_ptr(), m.venue.as_ptr(), m.symbol.as_ptr(), m.interval.as_ptr()])
        .collect();
    let mounts: std::sync::Arc<[MountView]> = rows.into();
    let list = std::sync::Arc::clone(&mounts);
    let held = vec![OrderHeldView {
        order_client_id: "held-1".into(),
        venue: "sim".into(),
        symbol: "BTC".into(),
        side: -1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(90.0),
        trigger_price: None,
        parent_order_id: Some("entry-1".into()),
        account: None,
        release: None,
        waiting: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        display_quantity: None,
    }];
    let held_at = held.as_ptr();
    let held_id_at = held[0].order_client_id.as_ptr();

    let snap = build(
        1,
        &e,
        &[],
        1_000.0,
        PriceBoardConfig::default(),
        vike_model::MarginCallConfig::default().maintenance_margin_requirement,
        &std::collections::VecDeque::new(),
        &indexmap::IndexMap::new(),
        mounts,
        &None,
        ReconcileBlock::default(),
        held,
        &mut OrderWindows::default(),
    );

    assert!(std::sync::Arc::ptr_eq(&snap.mounts, &list), "the list must be the one handed in");
    for (row, at) in snap.mounts.iter().zip(&strings_at) {
        assert_eq!(
            [row.mount_id.as_ptr(), row.venue.as_ptr(), row.symbol.as_ptr(), row.interval.as_ptr()],
            *at,
            "a row's Strings must be the ones handed in, not copies"
        );
    }
    assert_eq!(snap.held_exits.as_ptr(), held_at, "the held-exit buffer must be the one handed in");
    assert_eq!(snap.held_exits[0].order_client_id.as_ptr(), held_id_at);
    assert_eq!(snap.mounts.len(), 2);
    assert_eq!(snap.held_exits.len(), 1);
}

/// An engine whose account carries a bespoke multiplier grid (the deribit-options shape:
/// contract size != 1), holding a position in exactly ONE of the graded symbols.
fn engine_with_multipliers() -> Engine<RecordingClient> {
    let mut mults: indexmap::IndexMap<String, vike_model::ContractValue> =
        indexmap::IndexMap::new();
    mults.insert("BTC-PERP".to_string(), vike_model::ContractValue::linear(10.0));
    // deliberately NEVER traded — the options confirm-ticket case
    mults.insert("BTC-30AUG26-90000-C".to_string(), vike_model::ContractValue::linear(0.1));
    let mut e = Engine::new(
        Account::new(1.0, "deribit", Some(mults), BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "deribit",
        "BTC-PERP",
    );
    e.account.positions.insert(
        ("deribit".into(), "BTC-PERP".into(), PositionSide::Long),
        PositionEntry { size: 1.0, average_price: 100.0, ..Default::default() },
    );
    e
}

#[test]
fn snapshot_exposes_the_engine_contract_value() {
    use vike_model::ContractValue;
    let e = engine_with_multipliers();
    let snap = build_snap(&e, 1_000.0);
    // The snapshot must agree with the ONE authority, `Account::contract_value_of`.
    assert_eq!(snap.contract_value_of("deribit", "BTC-PERP"), ContractValue::linear(10.0));
    assert_eq!(
        snap.contract_value_of("deribit", "BTC-PERP"),
        e.account.contract_value_of("BTC-PERP")
    );
    assert_eq!(snap.portfolio.venues[0].contract_value_of("BTC-PERP"), ContractValue::linear(10.0));
}

/// Decision 0170: an INVERSE value rides the published block as a KIND.
#[test]
fn snapshot_publishes_an_inverse_value() {
    use vike_model::ContractValue;
    let mut grid: indexmap::IndexMap<String, ContractValue> = indexmap::IndexMap::new();
    grid.insert("BTC-PERPETUAL".to_string(), ContractValue::inverse(1.0));
    let e = Engine::new(
        Account::new(1.0, "deribit", Some(grid), BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "deribit",
        "BTC-PERPETUAL",
    );
    let snap = build_snap(&e, 1_000.0);
    assert_eq!(snap.contract_value_of("deribit", "BTC-PERPETUAL"), ContractValue::inverse(1.0));
}

#[test]
fn snapshot_multiplier_resolves_for_a_symbol_with_no_position() {
    // THE motivating case: the deribit options confirm-ticket prices a contract the account
    // has never traded. If this ever regresses to "position-only", the pre-submit notional cap
    // silently under-measures options notional again (the #458/#468/#477 bug class).
    let e = engine_with_multipliers();
    let snap = build_snap(&e, 1_000.0);
    assert!(
        snap.position("deribit", "BTC-30AUG26-90000-C").is_none(),
        "precondition: the account holds no position in this symbol"
    );
    assert_eq!(
        snap.contract_value_of("deribit", "BTC-30AUG26-90000-C"),
        vike_model::ContractValue::linear(0.1)
    );
}

#[test]
fn snapshot_multiplier_defaults_to_one_for_ungraded_symbols_and_venues() {
    let e = engine_with_multipliers();
    let snap = build_snap(&e, 1_000.0);
    // absent from the grid → the account's legacy scalar default (1.0 here)
    let one = vike_model::ContractValue::default();
    assert_eq!(snap.contract_value_of("deribit", "ETH-PERP"), one);
    assert_eq!(snap.portfolio.venues[0].contract_value_default, one);
    // unknown venue → the default (documented permissive answer, NOT proof of a plain instrument)
    assert_eq!(snap.contract_value_of("nosuchvenue", "BTC-PERP"), one);
    // an engine built with no grid at all publishes an empty map, every symbol the default
    let plain = build_snap(&engine_with_position(), 1_000.0);
    assert!(plain.portfolio.venues[0].contract_values.is_empty());
    assert_eq!(plain.contract_value_of("binance", "BTC"), one);
}

/// The published capability IS the engine's fail-closed flag: an engine the mount made fail closed on
/// symbols (the node door's `Admits` venues) publishes `admits_symbols`, any other does not.
#[test]
fn snapshot_publishes_whether_the_engine_admits_symbols_on_demand() {
    let mut e = engine_with_position();
    assert!(!build_snap(&e, 1_000.0).portfolio.venues[0].admits_symbols);
    e.gate.limits.deny_unadmitted_symbols = true;
    assert!(build_snap(&e, 1_000.0).portfolio.venues[0].admits_symbols);
}

#[test]
fn snapshot_multiplier_grid_is_shared_by_pointer_not_deep_cloned() {
    // The publish path is coalesced only while busy — it also runs on every idle transition, so
    // per event on a sporadic feed — and must not deep-clone per venue per tick: two
    // successive builds share ONE allocation with the engine's account.
    let e = engine_with_multipliers();
    let a = build_snap(&e, 1_000.0);
    let b = build_snap(&e, 1_000.0);
    assert!(std::sync::Arc::ptr_eq(
        &a.portfolio.venues[0].contract_values,
        &b.portfolio.venues[0].contract_values
    ));
    assert!(std::sync::Arc::ptr_eq(
        &a.portfolio.venues[0].contract_values,
        &e.account.contract_value_grid()
    ));
}

/// The block names what its engine trades and its account's tier (Trade window spec §4.3): the
/// primary symbol, then `extra_symbols`, which is exactly `accepts_symbol`'s set, and the tier.
#[test]
fn a_venue_block_names_its_symbols_and_tier() {
    let mut e = engine_with_position();
    e.extra_symbols = vec!["ETH".into()];
    e.tier = vike_model::AccountTier::Demo;
    let snap = build_snap(&e, 1_000.0);
    let vb = &snap.portfolio.venues[0];
    assert_eq!(vb.symbol, "BTC");
    assert_eq!(vb.extra_symbols, vec!["ETH".to_string()]);
    assert_eq!(vb.tier, Some(vike_model::AccountTier::Demo));
    for s in ["BTC", "ETH", "SOL"] {
        assert_eq!(vb.trades(s), e.accepts_symbol(s), "{s}: the block and the engine agree");
    }
}

/// The unnamed default block says nothing, so a reader treats it as "not said".
#[test]
fn a_default_venue_block_names_no_symbol_and_no_tier() {
    let vb = VenueBlock::default();
    assert!(vb.symbol.is_empty() && vb.extra_symbols.is_empty());
    assert_eq!(vb.tier, None);
    assert!(!vb.trades("BTC"));
}

/// Every order names the account whose book it rests in: none for the default account, the label
/// for a labelled one. Otherwise a two-account venue's ladders would mix (Trade window spec §4.3).
#[test]
fn an_order_names_the_account_its_engine_holds() {
    let req = vike_model::OrderRequest {
        order_client_id: "c-1".into(),
        venue: "binance".into(),
        symbol: "BTC".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(99.0),
        ..Default::default()
    };
    let mut e = engine_with_position();
    e.submit_order(&req, 0, &mut vike_execution::EventBusOutbox::default());
    assert_eq!(build_snap(&e, 1_000.0).orders[0].account, None, "the default account names none");
    e.route_key = "binance#SUB".into();
    let snap = build_snap(&e, 1_000.0);
    assert_eq!(snap.orders[0].account.as_ref().map(ToString::to_string).as_deref(), Some("SUB"));
}

/// A refused order publishes WHY it was refused (`OrderView::reason`): the reason its
/// `OrderRejected` carried. Every other order, and a refusal that carried none, publishes `None`.
#[test]
fn a_rejected_order_publishes_the_reason_its_rejection_carried() {
    use vike_execution::EventHandler;
    use vike_model::events::{Event, OrderRejected};
    let req = |order_client_id: &str| vike_model::OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        symbol: "BTC".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(99.0),
        ..Default::default()
    };
    let mut e = engine_with_position();
    let mut outbox = vike_execution::EventBusOutbox::default();
    for order_client_id in ["c-ok", "c-bad", "c-mute"] {
        e.submit_order(&req(order_client_id), 0, &mut outbox);
    }
    let reject = |order_client_id: &str, reason: &str| {
        Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: order_client_id.into(),
            reason: reason.into(),
            timestamp: 1,
            refusal_id: None,
        })
    };
    e.on_event(&reject("c-bad", "INVALID_REQUEST: more digits than symbol allows"), &mut outbox);
    e.on_event(&reject("c-mute", ""), &mut outbox);

    let snap = build_snap(&e, 1_000.0);
    let reason_of = |order_client_id: &str| {
        snap.orders.iter().find(|o| o.order_client_id == order_client_id).unwrap().reason.as_deref()
    };
    assert_eq!(reason_of("c-bad"), Some("INVALID_REQUEST: more digits than symbol allows"));
    assert_eq!(reason_of("c-mute"), None, "a refusal with no words publishes none");
    assert_eq!(reason_of("c-ok"), None, "an order that was not refused has no reason");
}

/// A LIVE order whose modify was refused publishes that reason too (`order ls` REASON), and the
/// order stays live with its terms; an order that was never refused still has none.
#[test]
fn a_live_order_publishes_the_reason_its_modify_was_refused() {
    use vike_execution::EventHandler;
    use vike_model::events::{Event, OrderAccepted, OrderModifyRejected, OrderSubmitted};
    let req = |order_client_id: &str| vike_model::OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        symbol: "BTC".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(99.0),
        ..Default::default()
    };
    let mut e = engine_with_position();
    let mut outbox = vike_execution::EventBusOutbox::default();
    for order_client_id in ["c-1", "c-2"] {
        e.submit_order(&req(order_client_id), 0, &mut outbox);
        e.on_event(
            &Event::OrderSubmitted(OrderSubmitted {
                order_client_id: order_client_id.into(),
                timestamp: 1,
            }),
            &mut outbox,
        );
        e.on_event(
            &Event::OrderAccepted(OrderAccepted {
                order_client_id: order_client_id.into(),
                venue_order_id: Some("v".into()),
                timestamp: 2,
            }),
            &mut outbox,
        );
    }
    e.on_event(
        &Event::OrderModifyRejected(OrderModifyRejected {
            order_client_id: "c-1".into(),
            reason: "risk: insufficient-margin".into(),
            timestamp: 3,
            refusal_id: None,
        }),
        &mut outbox,
    );

    let snap = build_snap(&e, 1_000.0);
    let find = |order_client_id: &str| {
        snap.orders.iter().find(|o| o.order_client_id == order_client_id).unwrap()
    };
    assert_eq!(find("c-1").reason.as_deref(), Some("risk: insufficient-margin"));
    assert_eq!(find("c-1").status, vike_model::OrderStatus::Accepted, "non-terminal");
    assert_eq!(find("c-2").reason, None);
}

/// A modify the node sent and the venue has not answered publishes its LIMIT price beside the order
/// (`OrderView::pending_price`, what the control door sizes a qty-only modify at), whichever surface
/// sent it; the answer clears it, and an order with no modify in flight publishes `None`.
#[test]
fn an_order_publishes_the_price_of_a_modify_in_flight() {
    use vike_execution::EventHandler;
    use vike_model::events::{Event, OrderAccepted, OrderModified, OrderSubmitted};
    let req = |order_client_id: &str| vike_model::OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        symbol: "BTC".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(99.0),
        ..Default::default()
    };
    let mut e = engine_with_position();
    let mut outbox = vike_execution::EventBusOutbox::default();
    for order_client_id in ["c-1", "c-2"] {
        e.submit_order(&req(order_client_id), 0, &mut outbox);
        e.on_event(
            &Event::OrderSubmitted(OrderSubmitted {
                order_client_id: order_client_id.into(),
                timestamp: 1,
            }),
            &mut outbox,
        );
        e.on_event(
            &Event::OrderAccepted(OrderAccepted {
                order_client_id: order_client_id.into(),
                venue_order_id: Some("v".into()),
                timestamp: 2,
            }),
            &mut outbox,
        );
    }
    e.modify_order("c-1", None, Some(120.0), 3, &mut outbox);

    let snap = build_snap(&e, 1_000.0);
    let find = |snap: &CoreSnapshot, order_client_id: &str| {
        snap.orders.iter().find(|o| o.order_client_id == order_client_id).unwrap().clone()
    };
    assert_eq!(find(&snap, "c-1").pending_price, Some(120.0), "sent, not answered");
    assert_eq!(find(&snap, "c-1").price, Some(99.0), "the resting price has not moved yet");
    assert_eq!(find(&snap, "c-2").pending_price, None, "no modify in flight on the other order");

    e.on_event(
        &Event::OrderModified(OrderModified {
            order_client_id: "c-1".into(),
            venue_order_id: None,
            new_qty: None,
            new_price: Some(120.0),
            new_trigger_price: None,
            timestamp: 4,
        }),
        &mut outbox,
    );
    let snap = build_snap(&e, 1_000.0);
    assert_eq!(find(&snap, "c-1").pending_price, None, "the venue answered");
    assert_eq!(find(&snap, "c-1").price, Some(120.0), "and the resting price moved");
}

// ---- an engine that admitted symbols (decision 0173): the rows `build` publishes ----

/// binance/BTC plus `n` admitted symbols, each holding a position (every third one short, every
/// fifth one also holding the opposite leg), with account marks on the even ones and a different
/// price-board rung per symbol so every `mark_source` shows up. Admitted through the engine's own
/// `admit_symbol`, the body of `Command::AdmitSymbol`.
fn engine_with_admitted_symbols(n: usize, limits: RiskLimits) -> Engine<RecordingClient> {
    let mut e = engine_with_limits(limits);
    hold(&mut e, "BTC", PositionEntry { size: 2.0, average_price: 100.0, ..Default::default() });
    e.account.set_mark_from("binance", "BTC", 101.0, MarkSource::VenueMark, 0);
    e.price_board.set_mark("binance", "BTC", 101.0, 0);
    for i in 0..n {
        let symbol = format!("ADM{i}USDT");
        let admission = vike_execution::SymbolAdmission {
            route_key: "binance".into(),
            symbol: symbol.clone(),
            grid: vike_model::SymbolGrid {
                tick_size: Some(0.01),
                lot_size: Some(0.001),
                ..Default::default()
            },
            contract_value: None,
            initial_mark: None,
            initial_mark_ms: None,
            quote_currency: None,
            quote_rate: None,
            rate_as_of_ms: None,
            initial_margin_requirement: None,
            release: false,
            margin_mode: None,
        };
        assert_eq!(e.admit_symbol(&admission), vike_execution::AdmitOutcome::Admitted);
        let sign = if i % 3 == 0 { -1.0 } else { 1.0 };
        hold(
            &mut e,
            &symbol,
            PositionEntry {
                size: sign * (1.0 + i as f64),
                average_price: 50.0 + i as f64,
                ..Default::default()
            },
        );
        if i % 5 == 0 {
            e.account.positions.insert(
                ("binance".into(), symbol.as_str().into(), PositionSide::Short),
                PositionEntry { size: -0.5, average_price: 60.0, ..Default::default() },
            );
        }
        if i % 2 == 0 {
            e.account.set_mark_from("binance", &symbol, 55.0 + i as f64, MarkSource::VenueMark, 0);
        }
        match i % 4 {
            0 => e.price_board.set_mark("binance", &symbol, 55.0 + i as f64, 0),
            1 => e.price_board.set_quote("binance", &symbol, 54.0 + i as f64, 56.0 + i as f64, 0),
            2 => e.price_board.set_last_trade("binance", &symbol, 52.0 + i as f64, 0),
            _ => {} // no price at all: counted in `missing_prices`
        }
    }
    e
}

/// The rows `build` publishes for an engine that admitted 0, 1 and 16 symbols are the account's
/// own: one `PositionView` per position in insertion order and one mark per mark slot, naming the
/// venue and the symbol the account's interned keys spell (compared as `Symbol::to_string` renders them),
/// with the unrealized figure and its source the resolver gave. The
/// gate-off and armed cases both run, because the armed one prices every leg again for its badge: there
/// every leg's leverage and cross liquidation estimate are recomputed here at the price the position is
/// valued at (`Engine::resolved_position_price`: a long at the bid, a short at the ask, owner 2026-11-15),
/// never the account slot's mark.
#[test]
fn admitted_symbols_publish_the_rows_the_account_holds() {
    for n in [0, 1, 16] {
        for limits in [RiskLimits::new(), armed(0.25)] {
            let is_armed = limits.initial_margin_requirement.is_some();
            let e = engine_with_admitted_symbols(n, limits);
            let mm = 0.5;
            let snap = build_at_mm(&e, 1_000.0, mm);
            let config = PriceBoardConfig::default();
            let re = e.resolve_equity(1_000.0, &config);
            let ctx = format!("{n} admitted, armed={is_armed}");

            assert_eq!(snap.positions.len(), e.account.positions.len(), "{ctx}");
            assert_eq!(snap.portfolio.venues[0].positions, snap.positions, "{ctx}: the mirror");
            let listed = &snap.portfolio.venues[0].extra_symbols;
            assert_eq!(*listed, e.extra_symbols, "{ctx}");
            let again = build_at_mm(&e, 1_000.0, mm);
            let shared = listed.is_shared_with(&again.portfolio.venues[0].extra_symbols);
            assert_eq!(shared, n > 0, "{ctx}: two publishes share the symbols while there are any");

            // The cross pool, accumulated in the order `margin_figures` walks the legs.
            let mut cross_total = 0.0;
            for ((v, s, _), p) in &e.account.positions {
                if p.size != 0.0
                    && let Some(mark) = e.resolved_position_price(v, s, p.size, &config)
                {
                    cross_total += e.account.contract_value_of(s).gross_notional(p.size, mark);
                }
            }
            for (i, (((v, s, side), p), rp)) in
                e.account.positions.iter().zip(&re.per_position).enumerate()
            {
                let row = &snap.positions[i];
                assert_eq!(row.venue, v.to_string(), "{ctx}: leg {i}");
                assert_eq!(row.symbol, s.to_string(), "{ctx}: leg {i}");
                assert_eq!(row.position_side, *side, "{ctx}: leg {i}");
                assert_eq!(row.size.to_bits(), p.size.to_bits(), "{ctx}: leg {i}");
                assert_eq!(
                    row.average_price.to_bits(),
                    p.average_price.to_bits(),
                    "{ctx}: leg {i}"
                );
                assert_eq!(row.unrealized.to_bits(), rp.unrealized.to_bits(), "{ctx}: leg {i}");
                assert_eq!(row.mark_source, rp.mark_source, "{ctx}: leg {i}");
                let (want_leverage, want_liquidation): (f64, f64) =
                    match (is_armed, e.resolved_position_price(v, s, p.size, &config)) {
                        (true, Some(mark)) => {
                            let own = e.account.contract_value_of(s).gross_notional(p.size, mark);
                            (
                                4.0,
                                vike_model::ContractValue::linear(1.0).cross_liquidation_price(
                                    re.equity,
                                    p.size,
                                    mark,
                                    cross_total - own,
                                    mm,
                                ),
                            )
                        }
                        (true, None) => (4.0, 0.0),
                        (false, _) => (0.0, 0.0),
                    };
                assert_eq!(
                    row.leverage.to_bits(),
                    want_leverage.to_bits(),
                    "{ctx}: leg {i} leverage"
                );
                assert_eq!(
                    row.liquidation_price.to_bits(),
                    want_liquidation.to_bits(),
                    "{ctx}: leg {i} liq"
                );
            }

            let want_marks: Vec<(String, String, f64)> = e
                .account
                .marks_iter()
                .map(|((v, s), px)| (v.to_string(), s.to_string(), *px))
                .collect();
            let got_marks: Vec<(String, String, f64)> =
                snap.marks.iter().map(|(v, s, px)| (v.to_string(), s.to_string(), *px)).collect();
            assert_eq!(got_marks, want_marks, "{ctx}: the marks");
            assert_eq!(snap.marks.len(), 1 + n.div_ceil(2), "{ctx}: BTC plus the even symbols");
        }
    }
}

/// FNV-1a over the `Debug` text of everything `build` publishes about positions, marks, venue
/// blocks and orders, for an engine that admitted `n` symbols: the bytes a reader formatting a
/// snapshot sees.
fn published_text_hash(n: usize, armed_gate: bool) -> (u64, usize) {
    let limits = if armed_gate { armed(0.25) } else { RiskLimits::new() };
    let e = engine_with_admitted_symbols(n, limits);
    let snap = build_at_mm(&e, 1_000.0, 0.5);
    let dump = format!(
        "{:?}\n{:?}\n{:?}\n{:?}\n{:?}",
        snap.positions, snap.marks, snap.portfolio, snap.orders, snap.balance
    );
    // `VenueBlock::admits_symbols` (the capability gate) is a field the string builder never had; it is
    // `false` for every engine here (no fail-closed flag), so cutting it keeps the pins about the rest.
    let dump = dump.replace(", admits_symbols: false", "");
    // ...and so are `VenueBlock::quote_rates` / `quote_rates_in`, the conversion-rate table: EMPTY for
    // an engine with no symbol quoted in a foreign currency (every engine here), so cutting the two
    // empty fields keeps the pins proving that such an engine publishes what it always did.
    let dump = dump.replace(", quote_rates: [], quote_rates_in: \"\"", "");
    // ...and `VenueBlock::ledger_currency`, the currency group: EMPTY (the dollar group) for every engine here.
    let dump = dump.replace(", ledger_currency: \"\"", "");
    // ...and `VenueBlock::currency_assumed` (the ledger currency is only an assumed USD): `false` for every engine
    // here, none was told an assumption.
    let dump = dump.replace(", currency_assumed: false", "");
    // ...and `Portfolio::primary_currency_stated`, the primary group an OBSERVER is told: `None` on every
    // portfolio the core builds itself (its first block is the primary's).
    let dump = dump.replace(", primary_ccy_stated: None", "");
    // ...and `Portfolio::group_curves`, the other currency groups' curves: EMPTY on a node of one currency,
    // and `snapshot::build` leaves it empty (the publish site fills it for a node of several).
    let dump = dump.replace(", group_curves: []", "");
    // ...and the three health counters: refused colliding fills and the foreign-currency ledger's two.
    // All three read `0` for every engine here.
    let dump = dump
        .replace(", colliding_fills_refused: 0, fees_unconverted: 0, fees_at_stale_rate: 0", "");
    // ...and the order client id's account code and shape (decision 0270): not said for every engine here.
    let dump = dump.replace(", account_code: None, order_client_id_shape: None", "");
    let mut h = vike_utility::fnv::Fnv1a64::new();
    h.write(dump.as_bytes());
    (h.finish(), dump.len())
}

/// The published snapshot is byte-identical to the one the builder made when positions and marks
/// carried `String`s and the extra symbols were a `Vec<String>`: the hashes below were taken from
/// that build (the parent of the commit that introduced `Interned` and `SymbolList`) over this very
/// scenario, and `Debug` is what a reader formatting a snapshot sees. A change that moves one is a
/// change of what the core publishes, not of how it is held.
#[test]
fn the_published_text_is_what_the_string_builder_published() {
    // RE-PINNED 2026-11-13 for the owner's phase-4 rename (P4.5c): the published text spells the key
    // words out (the same values; the lengths grew by exactly the longer names). The old values are in git.
    // Re-pinned again 2026-11-14 on the package tree (P4.5b's words beside P4.5c's): 31 bytes longer in every row.
    // Re-pinned 2026-11-15, the (16, armed) row ONLY, for the owner's ruling that a position is valued at the price it
    // can be closed at: the liquidation badge reads `Engine::resolved_position_price` (a long at the bid, a short at
    // the ask), no longer the account slot, so the legs this scenario prices by a quote or a trade and not by an
    // account mark gained or moved their badge. `admitted_symbols_publish_the_rows_the_account_holds` recomputes
    // every one of those badges from the resolver; the other five rows did not move.
    let pinned: [(usize, bool, u64, usize); 6] = [
        (0, false, 0xac68_f19f_713e_3a1e, 1207),
        (0, true, 0xfe3f_3597_8e41_2e6a, 1224),
        (1, false, 0xe09d_fff5_12c5_9aed, 2181),
        (1, true, 0xbe9c_0835_19c4_8a30, 2215),
        (16, false, 0x3eb2_ed91_4b38_6f7c, 11003),
        (16, true, 0x5547_0d5f_339e_b85e, 11091),
    ];
    // Every row is measured before the verdict, so a move names all six at once.
    let moved: Vec<String> = pinned
        .iter()
        .filter_map(|&(n, armed_gate, hash, len)| {
            let got = published_text_hash(n, armed_gate);
            (got != (hash, len)).then(|| {
                format!(
                    "{n} admitted, armed={armed_gate}: {:#x}, {} (want {hash:#x}, {len})",
                    got.0, got.1
                )
            })
        })
        .collect();
    assert!(moved.is_empty(), "{} of 6 rows moved:\n{}", moved.len(), moved.join("\n"));
}

/// **The block says when its ledger currency is only the owner's ASSUMED USD** (decision 0189, A52):
/// `VenueBlock::currency_assumed` is what the engine was told (`Engine::set_account_currency_assumed`), `false` for an
/// engine that was never told anything and for one told a stated currency, and it moves nothing else the block
/// publishes (the currency group stays the engine's own key).
#[test]
fn the_block_says_when_its_ledger_currency_is_only_assumed() {
    let mut e = engine_with_limits(RiskLimits::new());
    let block =
        |e: &Engine<RecordingClient>| build_at_mm(e, 1_000.0, 0.5).portfolio.venues[0].clone();
    assert!(!block(&e).currency_assumed, "an engine told nothing assumes nothing");
    e.set_account_currency("USD").expect("a currency code");
    assert!(!block(&e).currency_assumed, "a told currency is a stated one until said otherwise");
    let stated = block(&e);
    e.set_account_currency_assumed(true);
    let assumed = block(&e);
    assert!(assumed.currency_assumed);
    assert_eq!(
        vike_execution::VenueBlock { currency_assumed: false, ..assumed },
        stated,
        "the flag changes nothing else the block publishes"
    );
    e.set_account_currency_assumed(false);
    assert!(!block(&e).currency_assumed, "told again as stated");
}

/// The foreign-currency ledger's two health counters (`Account::fees_unconverted`,
/// `Account::fees_at_stale_rate`) and the refused-colliding-fill counter
/// (`Account::colliding_fills_refused`) ride the published block, so a reader of the core's snapshot can
/// see an amount the ledger left out or booked at a rate it could not vouch for, and a genuine fill the
/// account dropped. An account with no foreign-quoted symbol and no collision publishes `0` for all.
#[test]
fn the_block_publishes_the_fx_ledgers_health_counters() {
    let mut e = engine_with_limits(RiskLimits::new());
    e.set_account_currency("USD").expect("a currency code");
    let published = |e: &Engine<RecordingClient>| {
        let b = build_at_mm(e, 1_000.0, 0.5).portfolio.venues[0].clone();
        (b.fees_unconverted, b.fees_at_stale_rate)
    };
    assert_eq!(published(&e), (0, 0), "no foreign-quoted symbol: nothing to count");
    // the collision counter is the account's own field (counted by `fill_dedup_tests`); this pins that
    // the block reads it
    e.account.colliding_fills_refused = 2;
    let block = build_at_mm(&e, 1_000.0, 0.5).portfolio.venues[0].clone();
    assert_eq!(block.colliding_fills_refused, 2);
    e.account.colliding_fills_refused = 0;

    let yen = vike_execution::SymbolAdmission {
        route_key: "binance".into(),
        symbol: "USDJPY".into(),
        grid: vike_model::SymbolGrid {
            tick_size: Some(0.001),
            lot_size: Some(1.0),
            ..Default::default()
        },
        contract_value: None,
        initial_mark: None,
        initial_mark_ms: None,
        quote_currency: Some("JPY".into()),
        quote_rate: Some(1.0 / 150.0),
        rate_as_of_ms: Some(0),
        initial_margin_requirement: None,
        release: false,
        margin_mode: None,
    };
    assert_eq!(e.admit_symbol(&yen), vike_execution::AdmitOutcome::Admitted);
    let funding = |currency_value: Option<&str>| vike_model::events::FundingEvent {
        venue: "binance".into(),
        symbol: "USDJPY".into(),
        position_side: PositionSide::Both,
        funding_rate: 0.0001,
        amount: -1.0,
        mark_price: None,
        timestamp: 0,
        route_key: None,
        currency: currency_value.map(Into::into),
    };
    // no currency stated on a yen symbol: left out and counted
    e.account.apply_funding(&funding(None));
    assert_eq!(published(&e), (1, 0));
    // stated in yen but with no time: booked at the rate, flagged
    e.account.apply_funding(&funding(Some("JPY")));
    assert_eq!(published(&e), (1, 1));
}

/// **The block says how a client mints an id for its account** (decision 0270): once the root told the engine its
/// account code and its broker code's length, the block carries the code and the venue's UNTAGGED fit for that
/// length (a client never carries an instance tag): Binance's compact shape for a code of 8. An engine told nothing
/// says neither, the block it always was.
#[test]
fn a_told_engine_publishes_its_account_code_and_the_clients_shape() {
    use vike_model::orders::client_id::{OrderClientIdAccountCode, OrderClientIdShape};
    let mut e = engine_with_limits(RiskLimits::new());
    let untold = build_snap(&e, 1_000.0);
    assert_eq!(
        (untold.portfolio.venues[0].account_code, untold.portfolio.venues[0].order_client_id_shape),
        (None, None)
    );
    e.account_code = OrderClientIdAccountCode::of_id(4);
    e.broker_code_len = 8;
    let told = build_snap(&e, 1_000.0);
    let block = &told.portfolio.venues[0];
    assert_eq!(block.account_code, Some(OrderClientIdAccountCode::of_id(4)));
    let fit = vike_venues::venue_order_client_id_budget::order_client_id_shape_for("binance", 8, 0);
    assert_eq!(
        block.order_client_id_shape,
        Some(OrderClientIdShape { account: false, u_width: 12, max_gen: 9, tag: false })
    );
    assert_eq!(block.order_client_id_shape, Some(OrderClientIdShape { tag: false, ..fit }));
}
