//! The drawdown latch measures the daemon's OWN PnL, and the margin-call sweep prices on one basis.

use super::*;
use crate::runtime::test_support::{core_of, sim_engine_with};

// ---- the drawdown latch measures the daemon's OWN PnL, never a wallet -----------------------
//
// The the CI box shape, measured 2026-08-17: nine paper `Delta` blocks at 1000 seed each plus one
// `Authoritative` bybit block carrying 53647.10600813 — the whole UNIFIED wallet of a SHARED demo
// account — with the HWM latched at ~62647. `sweep_drawdown_latch` used to fold
// `Σ ExecutionEngine::resolved_equity`, which includes that wallet, so (1) a third party
// withdrawing from it trips this daemon into liquidate-only and (2) a 25% loss on the daemon's own
// ~9000 of book is 3.6% of 62647 and never trips it. The tests below pin BOTH directions.

/// A `(primary sim Delta, extra bybit)` two-engine core with `seed_cash` on both, so the latch's
/// capital base is `2 · seed`. The bybit engine's account is flipped to `Authoritative` with
/// `wallet` USDT through the REAL adoption path (`Account::apply_account_state`, the same fold a
/// live venue `AccountState` frame and `CoreThread::reconcile_reports` both take) rather than by
/// assigning the field — so the test exercises the mode the defect lives in.
fn dd_core_with_venue_wallet(seed: f64, wallet: f64) -> CoreThread<RecordingClient> {
    let primary = sim_engine_with(RecordingClient::default());
    let mut extra = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "ETHUSDT",
    );
    dd_set_wallet(&mut extra, wallet);
    let mut core = core_of(
        primary,
        vec![(seed, extra)],
        CoreConfig { seed_cash: seed, ..CoreConfig::default() },
    );
    core.config.seed_cash = seed;
    core
}

/// Adopt `wallet` USDT authoritatively onto `eng` — the venue's attested balance for the WHOLE
/// account the credentials open. Absolute, not additive (see `Account::apply_account_state`), so
/// calling it twice is exactly what a third-party deposit/withdrawal looks like from here.
fn dd_set_wallet(eng: &mut ExecutionEngine<RecordingClient>, wallet: f64) {
    eng.account.apply_account_state(
        &vike_model::events::AccountState {
            venue: "bybit".into(),
            balances: vec![("USDT".to_string(), wallet)],
            ts: 0,
            route_key: None,
        },
        "USDT",
    );
    assert_eq!(
        eng.account.balance_mode,
        BalanceMode::Authoritative,
        "the frame must flip the mode"
    );
}

/// ⚠ **A THIRD PARTY moving money in a shared venue wallet must not trip this daemon.** The
/// daemon's own book is FLAT throughout: no position, no fill, no fee, no funding. Only the
/// venue-attested balance changes, by −23647.
///
/// FAILS on the pre-fix latch: `Σ resolved_equity` seeds an HWM of `1000 + 53647.10600813` and
/// then reads `1000 + 30000`, a 43.3% drawdown, so the whole core latches liquidate-only because
/// somebody else took their money out.
#[test]
fn a_third_party_wallet_movement_does_not_move_the_drawdown_latch() {
    let mut core = dd_core_with_venue_wallet(1_000.0, 53_647.10600813);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(
        core.pnl_curve_peak,
        Some(2_000.0),
        "the HWM is the CONFIGURED capital base (2 × 1000), not the 54647 the account holds"
    );
    // ...and now somebody else withdraws 23647 from the shared account.
    dd_set_wallet(&mut core.extra_engines[0].1, 30_000.0);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(
        core.engine.trading_state,
        TradingState::Active,
        "a wallet this daemon does not own moved; its own PnL did not, so nothing may latch"
    );
    assert_eq!(
        core.extra_engines[0].1.trading_state,
        TradingState::Active,
        "and the venue engine stays Active too — the latch flips every engine or none"
    );
    assert!(
        !core.recent.iter().any(|e| e.starts_with("DRAWDOWN LATCH")),
        "no latch warning, and no DISARMED warning either: {:?}",
        core.recent
    );
    assert_eq!(core.pnl_curve_peak, Some(2_000.0), "the HWM did not move either");
}

/// The other half, and the one that must NOT be lost in the process: a real loss on the daemon's
/// OWN book latches at the configured fraction of its own capital, even while a large adopted
/// wallet sits beside it. 25% of 2000 of capital, with 53647 of somebody else's money in the same
/// account.
///
/// FAILS on the pre-fix latch: `Σ resolved_equity` moves from 54647.1 to 54147.1, a 0.91%
/// drawdown, so a 20% rule never fires — the exact invisibility the the CI box measurement showed.
#[test]
fn a_real_loss_on_the_daemons_own_book_latches_at_the_threshold() {
    let mut core = dd_core_with_venue_wallet(1_000.0, 53_647.10600813);
    // The daemon's own position, on the VENUE engine (the live mount — where the loss really is).
    set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 100.0, 100.5, 1);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.engine.trading_state, TradingState::Active, "flat at the HWM seed");
    assert_eq!(core.pnl_curve_peak, Some(2_000.0), "HWM = the capital base, own PnL still 0");

    // The long is valued at the BID, which drops to 50: own PnL = 10·(50−100) = −500, i.e. 25% of
    // the 2000 capital base. Nothing about the venue WALLET changed.
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 50.0, 50.5, 2);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(
        core.engine.trading_state,
        TradingState::Reducing,
        "a 25% loss of the daemon's own capital must latch a 20% rule"
    );
    assert_eq!(
        core.extra_engines[0].1.trading_state,
        TradingState::Reducing,
        "every engine latches, so the venue mount is liquidate-only too"
    );
    let warn = core
        .recent
        .iter()
        .find(|e| e.starts_with("DRAWDOWN LATCH:"))
        .expect("the latch warning lands in the ring");
    assert!(warn.contains("capital_base=2000.00"), "warning names the base: {warn}");
    assert!(warn.contains("own_pnl=-500.00"), "…and the daemon's own PnL: {warn}");
    assert!(warn.contains("drawdown=25.00%"), "…and the fraction it is 25% of: {warn}");
}

/// STARTUP + RESTART. Before any PnL exists the curve IS the capital base, so the latch is armed
/// against configured capital from the FIRST sweep (the old code seeded from the first OBSERVED
/// equity, i.e. from whatever the wallet held at that instant). Across a restart the four
/// `Account` PnL terms come back through `AccountSnapshot`, so the curve RESUMES underwater — and
/// because the peak seeds at `max(capital_base, curve)` rather than at the curve, a restart
/// forgives no loss booked below configured capital.
#[test]
fn the_drawdown_peak_seeds_at_configured_capital_not_at_an_underwater_restart_curve() {
    // A core whose book is ALREADY down 500 at the first sweep — the post-restart shape.
    let mut core = dd_core_with_venue_wallet(1_000.0, 53_647.10600813);
    set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 50.0, 50.5, 1);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(
        core.pnl_curve_peak,
        Some(2_000.0),
        "the peak seeds at the capital base, NOT at the 1500 curve it restarted underwater on"
    );
    assert_eq!(
        core.engine.trading_state,
        TradingState::Reducing,
        "so the pre-restart 25% loss is still a 25% drawdown and still latches"
    );
}

/// ⚠ The one failure direction worse than measuring against the wrong number: measuring against
/// NOTHING. A non-positive capital base leaves no denominator for the fraction, so the latch cannot
/// arm — and it says so ONCE rather than sitting silent. (`RunProfile::validate` refuses this
/// combination at load; this is the belt for a `CoreConfig` assembled directly.)
#[test]
fn a_non_positive_capital_base_disarms_loudly_and_only_once() {
    let mut core = dd_core_with_venue_wallet(0.0, 53_647.10600813);
    set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 50.0, 50.5, 1);
    core.sweep_drawdown_latch(0.2);
    core.sweep_drawdown_latch(0.2);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(
        core.engine.trading_state,
        TradingState::Active,
        "no fraction exists, so nothing can be judged breached"
    );
    let notes: Vec<_> =
        core.recent.iter().filter(|e| e.starts_with("DRAWDOWN LATCH DISARMED")).collect();
    assert_eq!(notes.len(), 1, "said once, not once per closed bar: {:?}", core.recent);
    assert!(notes[0].contains("seed_cash"), "and it names the knob to set: {}", notes[0]);
}

/// Margin-call sweep prices BOTH sides of the breach test through the resolver (risk-lane
/// completion): equity AND maintenance margin read the same board, so a STALE `Account.marks`
/// scalar can no longer sit on one side of the comparison.
///
/// Part 1 — the #518 asymmetry, healed: stale mark 100, fresh quote 40. The OLD split basis
/// judged margin off the stale mark (10·100·0.05 = 50) against a fresh-quote equity (40) and
/// LIQUIDATED an account that is genuinely healthy at the real price (maintenance
/// 10·40·0.05 = 20 ≤ equity 40 with room). One basis ⇒ Healthy.
/// Part 2 — a genuine quote-lane breach still liquidates, with the plan itself priced off the
/// quote (candidates and per-unit margin at 40, not the stale 100).
#[test]
fn margin_call_sweep_prices_margin_and_equity_on_one_basis() {
    let cfg =
        vike_exec::MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 };

    // Part 1: stale-mark overstatement no longer manufactures a liquidation.
    let mut core = test_core();
    core.config.seed_cash = 640.0;
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0); // stale mark — no longer consulted
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 1); // fresh crash
    // one basis: equity = 640 + 10·(40−100) = 40; margin_used = 10·40·0.05 = 20 → healthy
    core.sweep_margin_call(&cfg, 3);
    assert!(
        !core.recent.iter().any(|e| e.starts_with("MARGIN CALL")),
        "a stale mark must not overstate margin against fresh-quote equity: {:?}",
        core.recent
    );
    assert!(core.engine.client.submissions.is_empty(), "no liquidation order for a healthy book");

    // Part 2: a genuine breach on the one basis liquidates, plan priced off the quote.
    let mut core = test_core();
    core.config.seed_cash = 100.0;
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 0); // stale mark — no longer consulted
    core.engine.price_board.set_quote("sim", "BTCUSDT", 40.0, 40.5, 1);
    // equity = 100 − 600 = −500; margin_used = 20 → breach (remaining ≤ 0, LEAN buffer holds);
    // excess 520 at per-unit 40·0.05 = 2 → capped at the full 10 units.
    core.sweep_margin_call(&cfg, 3);
    assert!(
        core.recent.iter().any(|e| e.starts_with("MARGIN CALL")),
        "a genuine quote-lane breach must still liquidate: {:?}",
        core.recent
    );
    assert_eq!(
        core.engine.client.submissions.len(),
        1,
        "one reduce-only liquidation order reaches the client"
    );
    let liq = &core.engine.client.submissions[0];
    assert!(liq.reduce_only, "liquidation is reduce-only");
    assert_eq!(liq.qty, 10.0, "the plan prices the close off the quote basis (full close)");
}

/// **The closed-bar lane meets the margin-call sweep** — the exact ten-line window this round of
/// the fix is about. `drive_strategy` writes the bar close into the account mark slot and then
/// calls `sweep_margin_call`, so before the law moved inside `Account::set_mark_from` a crashing
/// candle could displace a fresh venue mark and be the basis of a liquidation decision in the
/// same call.
///
/// Two bases are asserted separately, because they are NOT the same code path and only one of
/// them was ever exposed:
/// - the RUNTIME sweep is board-priced (#518/#524): it resolves through `price_board`, whose
///   mark and bar_close are separate source-tagged slots, so it was already immune. Pinned here
///   so a future change that re-points it at `Account.marks` fails loudly.
/// - the ACCOUNT slot itself — read by the pre-trade gate, `margin_in_use_by`, `equity_all`, the
///   trailing-stop seed, `LiveBroker.price` and the legacy account-priced `check_margin_call`
///   entry — WAS exposed. The non-vacuity half below shows that basis genuinely flipping from
///   Healthy to Liquidate once the mark ages out, which is exactly what a crashing close would
///   have done at any time before this fix.
#[test]
fn a_closed_bar_cannot_hand_the_margin_call_sweep_a_candle_close_basis() {
    let cfg =
        vike_exec::MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 };
    let key: SeriesKey = ("sim".to_string(), "BTCUSDT".to_string(), "1m".to_string());
    // long 10 @ 100, seeded 640: healthy at the mark (used 50 vs equity 640), and deeply
    // underwater on the account basis if a crash close were to take the slot (equity −260).
    let crash = Bar {
        ts: 60_000,
        open: 10.0,
        high: 10.0,
        low: 10.0,
        close: 10.0,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };

    let mut core = test_core();
    core.config.seed_cash = 640.0;
    core.config.margin_call = Some(cfg);
    core.engine.now_ms = 1_000;
    core.engine.account.set_mark_staleness_ms(core.config.mark_staleness_ms);
    set_position(&mut core.engine, "sim", "BTCUSDT", 10.0, 100.0);
    core.engine.account.set_mark_from("sim", "BTCUSDT", 100.0, MarkSource::VenueMark, 1_000);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 100.0, 1_000);

    core.drive_strategy(&key, &crash);

    assert_eq!(
        core.engine.account.mark_of("sim", "BTCUSDT"),
        Some(100.0),
        "the closed-bar lane must not displace a fresh venue mark"
    );
    assert!(
        !core.recent.iter().any(|e| e.starts_with("MARGIN CALL")),
        "the sweep that runs ten lines later must not liquidate on a candle-close basis: {:?}",
        core.recent
    );
    assert!(core.engine.client.submissions.is_empty());
    // the account basis, at the mark: used 10·100·0.05 = 50 vs equity 640 → healthy
    let equity = core.engine.account.equity_all(640.0);
    assert_eq!(
        vike_exec::check_margin_call(&core.engine.account, equity, &cfg),
        vike_exec::MarginCall::Healthy
    );

    // NON-VACUITY: age the mark past the ownership window and drive the SAME bar. The close now
    // legitimately reclaims the slot, and the account basis flips to a liquidation — i.e. the
    // assertion above is pinning a real difference, not an inert one.
    core.engine.now_ms = 1_000 + core.config.mark_staleness_ms + 1;
    core.drive_strategy(&key, &crash);
    assert_eq!(core.engine.account.mark_of("sim", "BTCUSDT"), Some(10.0), "a silent mark releases");
    let equity = core.engine.account.equity_all(640.0);
    assert!(
        matches!(
            vike_exec::check_margin_call(&core.engine.account, equity, &cfg),
            vike_exec::MarginCall::Liquidate(_)
        ),
        "the account basis really does swing on which concept holds the slot"
    );
}
