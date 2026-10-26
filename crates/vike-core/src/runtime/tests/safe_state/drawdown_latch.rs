//! The drawdown latch measures the daemon's OWN PnL, and the margin-call sweep prices on one basis.

use super::*;
use crate::runtime::test_support::{core_of, engine_on, sim_engine_with};
use std::assert_matches;

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
        vike_model::MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 };

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

// ---- the PRICE-ONLY sweep: the margin call and the drawdown latch off the clock ---------------
//
// An engine no mount drives (`CoreConfig::price_only`) never sees a closed bar of its own series, and
// both sweeps run only from `drive_strategy`'s closed bar. The boundary timer calls them instead; the
// fake clock is the `now` handed to `drive_due_timers`.

/// A `(primary sim, extra bybit)` core where `bybit` is price-only, seeds 100 each (capital base 200).
fn price_only_core(
    margin: Option<vike_model::MarginCallConfig>,
    drawdown: Option<f64>,
) -> CoreThread<RecordingClient> {
    let extra = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "ETHUSDT",
    );
    core_of(
        sim_engine_with(RecordingClient::default()),
        vec![(100.0, extra)],
        CoreConfig {
            seed_cash: 100.0,
            margin_call: margin,
            max_drawdown: drawdown,
            price_only: Some(PriceOnlyConfig {
                venues: ["bybit".to_string()].into_iter().collect(),
                sweep_interval: std::time::Duration::from_secs(1),
                max_price_age: std::time::Duration::from_secs(60),
                price_source: std::collections::BTreeMap::new(),
            }),
            ..CoreConfig::default()
        },
    )
}

fn skip_notes(core: &CoreThread<RecordingClient>) -> usize {
    core.recent.iter().filter(|e| e.contains("price-only risk sweeps SKIP")).count()
}

/// ⚠ **THE MARGIN CALL NEVER ACTS ON A STALE NUMBER.** `CoreConfig::price_cfg` is the permissive default (no
/// age on any rung), so before the bound a position whose pump died was marked, at every sweep, off
/// the last quote it ever got. A breached position whose only quote is older than
/// `max_price_age` is NOT liquidated, and the skip is said ONCE however many sweeps pass; a fresh
/// quote brings the engine back in the same breath. (The drawdown latch does not skip it: it values
/// the engine at its last known price, see the tests at the end of this file.)
#[test]
fn a_price_only_engine_with_a_stale_mark_is_skipped_once_and_resumes_on_a_fresh_one() {
    let margin =
        vike_model::MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 };
    // (No latch here: a stale engine no longer blinds it, it is valued at its last known price, see the
    // tests at the end of this file. This one is the MARGIN CALL's skip.)
    let mut core = price_only_core(Some(margin), None);
    // long 10 at 100 on a seed of 100, the market at 40 (breached) - stamped at t=1.
    set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 40.0, 40.5, 1);
    core.arm_boundary_timers(0);

    // 70 s later the quote is 70 s old, past the 60 s bound: three sweeps, nothing acts.
    core.drive_due_timers(70_000);
    core.drive_due_timers(101_000);
    core.drive_due_timers(132_000);
    assert!(
        core.extra_engines[0].1.client.submissions.is_empty(),
        "no liquidation off a stale quote"
    );
    assert_eq!(skip_notes(&core), 1, "said once, not at every sweep: {:?}", core.recent);

    // The pump comes back: a fresh quote, the same breach - the next sweep acts.
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 40.0, 40.5, 133_000);
    core.drive_due_timers(134_000);
    assert_eq!(core.extra_engines[0].1.client.submissions.len(), 1, "liquidated on the fresh mark");

    // A LATER outage is a new episode and is said again.
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 40.0, 40.5, 134_000);
    core.drive_due_timers(260_000);
    assert_eq!(skip_notes(&core), 2, "{:?}", core.recent);
}

/// A rung older than the bound is no price even when the permissive `price_cfg` would still pick it:
/// an old MARK (rung 1) beside a fresh quote is priced off the QUOTE for a price-only engine.
#[test]
fn a_price_only_engine_prices_off_the_fresh_rung_not_an_old_mark() {
    let margin =
        vike_model::MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 };
    let mut core = price_only_core(Some(margin), None);
    set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 10.0, 100.0);
    // the old mark says healthy (100), the fresh quote says breached (40)
    core.extra_engines[0].1.price_board.set_mark("bybit", "ETHUSDT", 100.0, 1);
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 40.0, 40.5, 99_000);
    core.arm_boundary_timers(0);
    core.drive_due_timers(100_000);
    assert_eq!(core.extra_engines[0].1.client.submissions.len(), 1, "priced off the fresh quote");
}

/// ⚠ **A price-only engine priced off ANOTHER network's book feeds neither sweep.** Deribit's price
/// feed is the public MAINNET host while its exec signs on the testnet: the basis is phantom PnL that,
/// with leverage, trips the ACCOUNT-WIDE drawdown latch (Reducing on every engine, mounted ones too).
/// The engine here trades `demo` while its feed dials `live`: a 50% "loss" liquidates nothing and
/// latches nothing; the SAME book on a matching network does both. A paper engine has no network and is
/// swept as ever.
#[test]
fn a_price_only_engine_on_another_network_than_its_feed_is_out_of_both_sweeps() {
    use vike_model::AccountTier;
    let margin =
        vike_model::MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 };
    let run = |tier: AccountTier, feed: AccountTier| {
        let mut core = price_only_core(Some(margin), Some(0.2));
        core.config.price_only.as_mut().expect("price-only").price_source =
            [("bybit".to_string(), feed)].into_iter().collect();
        core.extra_engines[0].1.tier = tier;
        set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 10.0, 100.0);
        core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 40.0, 40.5, 1);
        core.arm_boundary_timers(0);
        core.drive_due_timers(1_100);
        core.drive_due_timers(2_200);
        core
    };
    let crossed = run(AccountTier::Demo, AccountTier::Live);
    assert!(crossed.extra_engines[0].1.client.submissions.is_empty(), "no margin call");
    assert_eq!(crossed.engine.trading_state, TradingState::Active, "no latch");
    assert_eq!(crossed.extra_engines[0].1.trading_state, TradingState::Active);
    assert_eq!(skip_notes(&crossed), 1, "said once: {:?}", crossed.recent);

    let same = run(AccountTier::Demo, AccountTier::Demo);
    // (the close of the first sweep stays in flight, unfilled, and covers the breach at the second: ONE
    // order, not one per sweep - the margin call nets the closes already out, third review)
    assert_eq!(same.extra_engines[0].1.client.submissions.len(), 1, "same network: swept");
    assert_eq!(same.engine.trading_state, TradingState::Reducing);

    let paper = run(AccountTier::Paper, AccountTier::Live);
    assert_eq!(paper.extra_engines[0].1.client.submissions.len(), 1, "a paper book has no network");
}

/// ⚠ **A breached price-only engine is liquidated by the TIMER, with no bar**, and so is the venue a mount
/// drives (owner ruling 2026-10-25, the tests at the end of this file): the primary (`sim`) is breached
/// just as hard and the clock sweeps it too, each in its own book. This test used to pin the opposite
/// ("an engine a mount drives is swept by its closed bar only, never by this timer").
#[test]
fn the_price_only_timer_liquidates_a_breached_engine_without_a_bar() {
    let margin =
        vike_model::MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 };
    let mut core = price_only_core(Some(margin), None);
    for (eng, venue, symbol) in
        [(&mut core.extra_engines[0].1, "bybit", "ETHUSDT"), (&mut core.engine, "sim", "BTCUSDT")]
    {
        // long 10 at 100 on a seed of 100, the market at 40: equity −500 against a requirement of 20
        set_position(eng, venue, symbol, 10.0, 100.0);
        eng.price_board.set_quote(venue, symbol, 40.0, 40.5, 1);
    }
    core.arm_boundary_timers(0);
    assert!(!core.timers.is_empty(), "a price-only core arms its sweep");

    core.drive_due_timers(500);
    assert!(core.extra_engines[0].1.client.submissions.is_empty(), "not due before the interval");
    core.drive_due_timers(1_100);
    let sent = &core.extra_engines[0].1.client.submissions;
    assert_eq!(sent.len(), 1, "the breached price-only engine is liquidated at the first sweep");
    assert!(sent[0].reduce_only, "…as a reduce-only close");
    assert_eq!(
        core.engine.client.submissions.len(),
        1,
        "the mounted primary is liquidated by the same sweep, in its own book"
    );

    core.drive_due_timers(1_500);
    assert_eq!(core.extra_engines[0].1.client.submissions.len(), 1, "no sweep between intervals");
    assert!(!core.timers.is_empty(), "the sweep re-arms");
}

/// The drawdown latch rides the same timer: a 50% loss of the daemon's own capital on the price-only
/// engine flips EVERY engine to liquidate-only at the sweep, with no bar anywhere.
#[test]
fn the_price_only_timer_runs_the_drawdown_latch() {
    let mut core = price_only_core(None, Some(0.2));
    set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 90.0, 90.5, 1);
    core.arm_boundary_timers(0);

    core.drive_due_timers(500);
    assert_eq!(core.engine.trading_state, TradingState::Active, "not due yet");
    core.drive_due_timers(1_100);
    assert_eq!(core.extra_engines[0].1.trading_state, TradingState::Reducing);
    assert_eq!(core.engine.trading_state, TradingState::Reducing, "the latch is account-wide");
    assert!(core.recent.iter().any(|e| e.starts_with("DRAWDOWN LATCH:")), "{:?}", core.recent);
}

/// Without `price_only` nothing is armed: the wheel stays empty and a huge `now` fires nothing, so
/// the default core is byte-identical (the feature costs a config `None` and nothing else).
#[test]
fn no_price_only_config_arms_no_sweep() {
    let mut core = test_core();
    core.config.margin_call = Some(vike_model::MarginCallConfig::default());
    core.config.max_drawdown = Some(0.2);
    let before = core.recent.len();
    core.arm_boundary_timers(0);
    assert!(core.timers.is_empty());
    core.drive_due_timers(999_999);
    assert_eq!(core.recent.len(), before, "{:?}", core.recent);
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
/// - the ACCOUNT slot itself — read by the pre-trade gate, the marks-priced margin fold,
///   `equity_all`, the trailing-stop seed, `LiveBroker.price` and `check_margin_call_priced`
///   priced at the account marks — WAS exposed. The non-vacuity half below shows that basis genuinely flipping from
///   Healthy to Liquidate once the mark ages out, which is exactly what a crashing close would
///   have done at any time before this fix.
#[test]
fn a_closed_bar_cannot_hand_the_margin_call_sweep_a_candle_close_basis() {
    let cfg =
        vike_model::MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 };
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
    assert_eq!(at_marks(&core.engine.account, equity, &cfg), vike_exec::MarginCall::Healthy);

    // NON-VACUITY: age the mark past the ownership window and drive the SAME bar. The close now
    // legitimately reclaims the slot, and the account basis flips to a liquidation — i.e. the
    // assertion above is pinning a real difference, not an inert one.
    core.engine.now_ms = 1_000 + core.config.mark_staleness_ms + 1;
    core.drive_strategy(&key, &crash);
    assert_eq!(core.engine.account.mark_of("sim", "BTCUSDT"), Some(10.0), "a silent mark releases");
    let equity = core.engine.account.equity_all(640.0);
    assert_matches!(
        at_marks(&core.engine.account, equity, &cfg),
        vike_exec::MarginCall::Liquidate(_),
        "the account basis really does swing on which concept holds the slot"
    );
}

/// The margin-call law priced at the account's own marks (the account basis this file pins).
fn at_marks(
    account: &vike_exec::Account,
    equity: f64,
    cfg: &vike_model::MarginCallConfig,
) -> vike_exec::MarginCall {
    vike_exec::check_margin_call_priced(account, equity, cfg, |(v, s, _side), _p| {
        account.mark_of(v, s)
    })
}

// ---- multi-currency: the margin-call sweep keeps running on a STALE rate -----------------------
//
// A conversion rate older than `DEFAULT_QUOTE_RATE_TTL_MS` denies OPENING orders on its currency's
// symbols (`quote-rate-stale`), but the account's valuation keeps the last rate and the margin-call
// sweep keeps running on it: skipping a yen position because its rate aged would be the
// "unpriceable contributes nothing" shortcut, which hides losses. The liquidation the sweep releases
// is a covered reduce, so the gate that denies an opening order on the same symbol lets it through.
// (No alert naming the stale rate is built, and none is asserted.) Dyadic figures throughout.

/// 128 yen to the dollar.
const YEN_R: f64 = 1.0 / 128.0;
/// One millisecond past the window: the rate admitted at 0 is stale here.
const STALE_NOW: i64 = vike_exec::DEFAULT_QUOTE_RATE_TTL_MS + 1;

/// A USD core long 100 000 `USD_JPY` @ 150 whose rate (`YEN_R`) was observed at 0, the market at
/// 100 yen (a bid of 100): the position is down 50 yen a unit, 39 062.5 USD in all, and its
/// maintenance requirement is 5% of `100 000 * 100 / 128`, 3 906.25.
fn yen_core(seed: f64) -> CoreThread<RecordingClient> {
    let mut core = test_core();
    core.config.seed_cash = seed;
    core.engine.quote_asset = "USD".into();
    core.engine.now_ms = 0;
    let admitted = core.engine.admit_symbol(&vike_exec::SymbolAdmission {
        route_key: "sim".into(),
        symbol: "USD_JPY".into(),
        grid: vike_model::SymbolGrid {
            tick_size: Some(0.25),
            lot_size: Some(1.0),
            ..vike_model::SymbolGrid::default()
        },
        contract_value: None,
        initial_mark: None,
        quote_ccy: Some("JPY".into()),
        quote_rate: Some(YEN_R),
        rate_as_of_ms: Some(0),
        im_requirement: None,
    });
    assert_eq!(admitted, vike_exec::AdmitOutcome::Admitted);
    set_position(&mut core.engine, "sim", "USD_JPY", 100_000.0, 150.0);
    core.engine.price_board.set_quote("sim", "USD_JPY", 100.0, 100.5, 1);
    core
}

/// What a breach releases: `(symbol, side, qty, reduce_only)` of every order that reached the client.
fn released(core: &CoreThread<RecordingClient>) -> Vec<(String, i32, f64, bool)> {
    core.engine
        .client
        .submissions
        .iter()
        .map(|r| (r.symbol.clone(), r.side, r.qty, r.reduce_only))
        .collect()
}

/// **A breach on a stale rate still liquidates, and the plan is the fresh rate's plan.** In the very
/// state where the gate denies an opening order for the rate's age (the non-vacuity half), the
/// sweep releases one reduce-only close of the whole position, and it reaches the venue.
#[test]
fn a_stale_rate_does_not_stop_the_margin_call_sweep() {
    let cfg =
        vike_model::MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 };

    // Fresh: the reference plan.
    let mut fresh = yen_core(10_000.0);
    fresh.engine.now_ms = 1_000;
    assert_eq!(fresh.engine.account.quote_leg("USD_JPY", 1_000), (YEN_R, false));
    fresh.sweep_margin_call(&cfg, 1_000);
    assert!(fresh.recent.iter().any(|e| e.starts_with("MARGIN CALL")), "{:?}", fresh.recent);
    let reference = released(&fresh);
    assert_eq!(reference, vec![("USD_JPY".to_string(), -1, 100_000.0, true)]);

    // Stale: the same book, the same market, the clock a millisecond past the window.
    let mut core = yen_core(10_000.0);
    core.engine.now_ms = STALE_NOW;
    assert_eq!(
        core.engine.account.quote_leg("USD_JPY", STALE_NOW),
        (YEN_R, true),
        "the rate is stale, and it is still the rate the account values with"
    );
    // …so an OPENING order on the symbol is denied for the rate, by name.
    let opening = vike_model::OrderRequest {
        client_order_id: "o-open".into(),
        venue: "sim".into(),
        symbol: "USD_JPY".into(),
        side: 1,
        qty: 100.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ..Default::default()
    };
    let mut outbox = vike_exec::Outbox::default();
    core.engine.submit_order(&opening, STALE_NOW, &mut outbox);
    let denial = outbox.0.iter().find_map(|e| match e {
        Event::OrderDenied(d) => Some(d.reason.to_string()),
        _ => None,
    });
    assert_eq!(denial.as_deref(), Some("quote-rate-stale"));
    assert!(core.engine.client.submissions.is_empty(), "the denied opening reached no venue");

    // The sweep runs on that same stale rate and releases the fresh plan.
    core.sweep_margin_call(&cfg, STALE_NOW);
    assert!(
        core.recent.iter().any(|e| e.starts_with("MARGIN CALL")),
        "a breach on a stale rate still liquidates: {:?}",
        core.recent
    );
    assert_eq!(released(&core), reference, "…with the plan a fresh rate gives");
}

/// The other half: staleness alone liquidates nothing. A healthy yen book on a stale rate is left
/// alone, so the sweep neither skips the position nor invents a breach for it.
#[test]
fn a_stale_rate_on_a_healthy_book_liquidates_nothing() {
    let cfg =
        vike_model::MarginCallConfig { mm_requirement: 0.05, warn_fraction: 0.05, buffer: 0.10 };
    // equity = 100 000 - 39 062.5 = 60 937.5 against a 3 906.25 requirement
    let mut core = yen_core(100_000.0);
    core.engine.now_ms = STALE_NOW;
    assert!(core.engine.account.quote_leg("USD_JPY", STALE_NOW).1, "the rate is stale");
    core.sweep_margin_call(&cfg, STALE_NOW);
    assert!(
        !core.recent.iter().any(|e| e.starts_with("MARGIN CALL") || e.contains("WARNING")),
        "{:?}",
        core.recent
    );
    assert!(core.engine.client.submissions.is_empty());
}

/// **CHARACTERIZATION of v0.1.80 (pinned as it IS, not as a goal): a STOPPED account stays in the
/// drawdown curve, and a restart that ADOPTS its position counts it twice.** A stop is a zero step (the
/// tombstone keeps its booked PnL and its open position's unrealized PnL in the sums, so a loss is never
/// forgiven by stopping the account), the account starting again flat changes no number, and the one
/// place a position is counted by two engines is the adoption (`hybrid`/`synthesize`/an operator
/// confirm; the default `quarantine` adopts nothing). That over-count is FAIL-SAFE: the latch fires
/// earlier than the true loss warrants, never later. Removing it is a design with three requirements
/// (`crates/vike-exec/src/execution_engine/valuation.rs`'s `resolved_own_pnl`); until one is built this
/// test is the contract, and it must be changed on purpose, not by accident.
#[test]
fn a_stopped_engine_stays_in_the_curve_and_an_adoption_counts_twice() {
    let mut core = dd_core_with_venue_wallet(1_000.0, 1_000.0);
    // The latch's own own-PnL fold: `py_sum` over the primary, then the extras (`sweep_drawdown_latch`).
    let own_pnl = |core: &CoreThread<RecordingClient>| {
        let cfg = core.config.price_cfg;
        vike_model::py_sum(
            std::iter::once(core.engine.resolved_own_pnl(&cfg))
                .chain(core.extra_engines.iter().map(|(_, e)| e.resolved_own_pnl(&cfg))),
        )
    };
    set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 95.0, 95.5, 1);
    core.sweep_drawdown_latch(0.04);
    assert_eq!(own_pnl(&core), -50.0, "PREMISE: 10 x (95 - 100), 2.5% of the 2000 base");
    assert_eq!(core.pnl_curve_peak, Some(2_000.0));

    // A STOP is a zero step: the tombstone's -50 stays, the peak and the state do not move.
    core.apply_account_change(
        vike_exec::AccountChange::StopEngine {
            not_running: vike_exec::NotRunningAccount {
                venue: "bybit".to_string(),
                account: None,
                route_key: "bybit".to_string(),
                tier: None,
                reason: "account inactive".to_string(),
            },
        },
        10,
    );
    assert!(core.is_retired(1), "PREMISE: the stop tombstoned the bybit engine");
    core.sweep_drawdown_latch(0.04);
    assert_eq!(own_pnl(&core), -50.0, "the stop forgave nothing and moved nothing");
    assert_eq!(core.pnl_curve_peak, Some(2_000.0));
    assert_eq!(core.engine.trading_state, TradingState::Active);

    // The account STARTS AGAIN, flat: the seed moves out of the base and the peak, no number changes.
    core.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
    core.apply_account_change(
        vike_exec::AccountChange::add_engine(
            1_000.0,
            ExecutionEngine::new(
                Account::new(1.0, "bybit", None, BalanceMode::Delta),
                RiskGate::new(RiskLimits::new()),
                RecordingClient::default(),
                "bybit",
                "ETHUSDT",
            ),
        ),
        20,
    );
    assert_eq!(core.extra_engines.len(), 2, "PREMISE: the new engine joined beside the tombstone");
    core.sweep_drawdown_latch(0.04);
    assert_eq!(own_pnl(&core), -50.0, "the new engine holds nothing: still the tombstone's -50");
    assert_eq!(core.pnl_curve_peak, Some(2_000.0));
    assert_eq!(core.engine.trading_state, TradingState::Active);

    // The reconcile ADOPTS the venue's position into the new engine: the SAME 10 ETH, now held by two
    // engines, and both are summed.
    set_position(&mut core.extra_engines[1].1, "bybit", "ETHUSDT", 10.0, 100.0);
    core.extra_engines[1].1.price_board.set_quote("bybit", "ETHUSDT", 95.0, 95.5, 2);
    assert_eq!(own_pnl(&core), -100.0, "KNOWN OVER-COUNT: one real -50, counted by both engines");
    core.sweep_drawdown_latch(0.04);
    assert_eq!(
        core.engine.trading_state,
        TradingState::Reducing,
        "fail-safe: a real 2.5% loss trips the 4% rule because it reads 5%"
    );
    let warn = core.recent.iter().find(|e| e.starts_with("DRAWDOWN LATCH:")).expect("said");
    assert!(warn.contains("own_pnl=-100.00"), "{warn}");
}

// ---- a STALE engine no longer blinds the latch: it is valued at its LAST KNOWN price (owner ruling 2026-10-25) ----
//
// A price-only engine holding a position whose price was older than the bound used to make the whole node's
// latch evaluation "unmeasurable", so the latch acted for NO account while one pump was quiet: over an FX
// weekend (the book closes Friday 22:00 UTC, OANDA's EURUSD stays Friday's) and in an open market with a dead
// pump (Dukascopy's USDJPY, no tick for two minutes, 2026-10-25 21:22 UTC: «price-only risk sweeps SKIP the
// dukascopy engine ... the margin call and the drawdown latch do not act on it»). The latch now values such an
// engine at the last price it had, says so ONCE per episode with the age, and keeps measuring; the MARGIN CALL
// still skips it (it must not liquidate on a stale price), and an engine on another network than its feed
// stays out of the curve.

/// A node of two PRICE-ONLY engines, `a` the primary (seed `seeds.0`) and `b` (seed `seeds.1`), with the margin
/// call armed and a 20% latch.
fn two_leg_core(
    a: (&str, &str),
    b: (&str, &str),
    seeds: (f64, f64),
) -> CoreThread<RecordingClient> {
    core_of(
        engine_on(a.0, a.1, RecordingClient::default()),
        vec![(seeds.1, engine_on(b.0, b.1, RecordingClient::default()))],
        CoreConfig {
            seed_cash: seeds.0,
            margin_call: Some(vike_model::MarginCallConfig {
                mm_requirement: 0.05,
                warn_fraction: 0.05,
                buffer: 0.10,
            }),
            max_drawdown: Some(0.2),
            price_only: Some(PriceOnlyConfig {
                venues: [a.0, b.0].iter().map(|v| v.to_string()).collect(),
                sweep_interval: std::time::Duration::from_secs(30),
                max_price_age: std::time::Duration::from_secs(60),
                price_source: std::collections::BTreeMap::new(),
            }),
            ..CoreConfig::default()
        },
    )
}

fn skip_notes_of(core: &CoreThread<RecordingClient>, venue: &str) -> usize {
    let tag = format!("price-only risk sweeps SKIP the {venue} engine");
    core.recent.iter().filter(|e| e.contains(&tag)).count()
}

/// ⚠ **THE FX WEEKEND.** Equity 100 000: OANDA (seed 20 000) is long 100 000 EURUSD at 1.0 on Friday's
/// 22:00 quote, Bybit (seed 80 000) is long 1 BTC at 80 000. On Saturday BTC is 56 000, 24 000 down, 24% of the
/// equity against a 20% latch, while OANDA's only price is a day old. The old rule called the whole curve
/// unmeasurable because of EURUSD and the latch stayed Active; now EURUSD is valued at Friday's price (flat)
/// and the latch trips on the Bybit loss. OANDA's margin call is still skipped, and said once.
#[test]
fn the_latch_trips_on_the_bybit_loss_while_oandas_price_is_fridays() {
    const FRIDAY_CLOSE: i64 = 1_000;
    const SATURDAY: i64 = 100_000_000;
    let mut core = two_leg_core(("oanda", "EURUSD"), ("bybit", "BTCUSDT"), (20_000.0, 80_000.0));
    set_position(&mut core.engine, "oanda", "EURUSD", 100_000.0, 1.0);
    core.engine.price_board.set_quote("oanda", "EURUSD", 1.0, 1.0001, FRIDAY_CLOSE);
    set_position(&mut core.extra_engines[0].1, "bybit", "BTCUSDT", 1.0, 80_000.0);
    core.extra_engines[0].1.price_board.set_quote(
        "bybit",
        "BTCUSDT",
        56_000.0,
        56_001.0,
        SATURDAY - 5_000,
    );
    core.arm_boundary_timers(0);

    core.drive_due_timers(SATURDAY);
    assert_eq!(core.engine.trading_state, TradingState::Reducing, "{:?}", core.recent);
    assert_eq!(core.extra_engines[0].1.trading_state, TradingState::Reducing, "…account-wide");
    let latch = latch_note(&core);
    assert!(
        latch.contains("capital_base=100000.00") && latch.contains("drawdown=24.00%"),
        "{latch}"
    );
    assert_eq!(
        skip_notes_of(&core, "oanda"),
        1,
        "the margin call's skip, said once: {:?}",
        core.recent
    );
    assert!(core.engine.client.submissions.is_empty(), "nothing liquidated off Friday's price");
    assert!(core.extra_engines[0].1.client.submissions.is_empty());
}

fn latch_note(core: &CoreThread<RecordingClient>) -> String {
    core.recent
        .iter()
        .find(|e| e.starts_with("DRAWDOWN LATCH"))
        .map(|e| e.to_string())
        .unwrap_or_default()
}

/// ⚠ **THE DEAD PUMP IN AN OPEN MARKET.** Bybit (seed 99 900) is fresh and 30% down on 10 BTC-like units; okx
/// (seed 100) holds a position whose last price is ten minutes old and WOULD be a margin call at that price
/// (equity -900 against a maintenance of 450). The latch trips on the fresh loss (and counts okx at its last
/// known price too); the margin call skips okx, so nothing is liquidated off the stale number, and the skip
/// line carries the age.
#[test]
fn a_fresh_engines_loss_trips_the_latch_while_another_engines_price_is_ten_minutes_old() {
    const NOW: i64 = 700_000;
    let mut core = two_leg_core(("bybit", "BTCUSDT"), ("okx", "ETHUSDT"), (99_900.0, 100.0));
    set_position(&mut core.engine, "bybit", "BTCUSDT", 10.0, 10_000.0);
    core.engine.price_board.set_quote("bybit", "BTCUSDT", 7_000.0, 7_001.0, NOW - 5_000);
    set_position(&mut core.extra_engines[0].1, "okx", "ETHUSDT", 100.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("okx", "ETHUSDT", 90.0, 90.5, NOW - 600_000);
    core.arm_boundary_timers(0);

    core.drive_due_timers(NOW);
    assert_eq!(core.engine.trading_state, TradingState::Reducing, "{:?}", core.recent);
    assert_eq!(core.extra_engines[0].1.trading_state, TradingState::Reducing, "…account-wide");
    assert!(
        core.extra_engines[0].1.client.submissions.is_empty(),
        "the margin call does not liquidate okx off a ten-minute-old price"
    );
    assert!(core.engine.client.submissions.is_empty(), "bybit is healthy");
    let skip = core.recent.iter().find(|e| e.contains("SKIP the okx engine")).expect("said");
    assert!(
        skip.contains("last known price") && skip.contains("600 s old"),
        "the skip says what the latch does and how old the price is: {skip}"
    );
}

/// **ONE WARN PER EPISODE, not per sweep.** A quiet engine across ten sweeps is one line in the ring and one
/// WARN in the log, naming the engine, the age and that the latch uses the last known value; a fresh quote ends
/// the episode and the next outage is a new one.
#[test]
#[tracing_test::traced_test]
fn a_stale_engine_is_said_once_per_episode_with_its_age() {
    let mut core = two_leg_core(("bybit", "BTCUSDT"), ("okx", "ETHUSDT"), (1_000.0, 1_000.0));
    set_position(&mut core.extra_engines[0].1, "okx", "ETHUSDT", 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("okx", "ETHUSDT", 100.0, 100.5, 1);
    core.arm_boundary_timers(0);
    let warns = |wanted: usize| {
        move |lines: &[&str]| {
            let got = lines.iter().filter(|l| l.contains("last known price")).count();
            if got == wanted { Ok(()) } else { Err(format!("{got} WARNs, wanted {wanted}")) }
        }
    };
    let mut now = 70_000;
    for _ in 0..6 {
        core.drive_due_timers(now);
        now += 30_100;
    }
    assert_eq!(skip_notes_of(&core, "okx"), 1, "six sweeps, one line: {:?}", core.recent);
    logs_assert(warns(1));
    let line = core.recent.iter().find(|e| e.contains("SKIP the okx engine")).expect("said");
    assert!(
        line.contains("s old") && line.contains("ETHUSDT") && line.contains("last known price")
    );

    // the pump comes back, then goes quiet again: a NEW episode
    core.extra_engines[0].1.price_board.set_quote("okx", "ETHUSDT", 100.0, 100.5, now - 1_000);
    for _ in 0..4 {
        core.drive_due_timers(now);
        now += 30_100;
    }
    assert_eq!(skip_notes_of(&core, "okx"), 2, "{:?}", core.recent);
    logs_assert(warns(2));
}

/// **THE WARN SAYS ONLY WHAT IS TRUE** (third review, 5). The log line used to claim, whatever the config, that "the
/// drawdown latch values it at its last known price": with no `max_drawdown` there is no latch to value anything,
/// and an operator reading it would believe a loss limit was running. The ring line was already built from the
/// config; the WARN now is too. A configured latch is said (the test above pins its words), an absent one is not.
#[test]
#[tracing_test::traced_test]
fn the_stale_warn_claims_the_latch_only_when_a_latch_is_configured() {
    let mut core = two_leg_core(("bybit", "BTCUSDT"), ("okx", "ETHUSDT"), (1_000.0, 1_000.0));
    core.config.max_drawdown = None;
    set_position(&mut core.extra_engines[0].1, "okx", "ETHUSDT", 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("okx", "ETHUSDT", 100.0, 100.5, 1);
    core.arm_boundary_timers(0);
    core.drive_due_timers(70_000);

    let ring = core.recent.iter().find(|e| e.contains("SKIP the okx engine")).expect("said");
    assert!(!ring.contains("drawdown latch"), "no latch is configured: {ring}");
    logs_assert(|lines: &[&str]| {
        let warns: Vec<&&str> =
            lines.iter().filter(|l| l.contains("holds a position whose price")).collect();
        match warns.as_slice() {
            [w] if w.contains("drawdown latch") || w.contains("last known price") => {
                Err(format!("the WARN claims a latch that is not configured: {w}"))
            }
            [_] => Ok(()),
            other => Err(format!("{} WARNs, wanted 1", other.len())),
        }
    });
}

/// **SKIP_NETWORK STAYS OUT OF THE CURVE.** An engine that trades a real network other than the one its feed
/// dials is phantom basis; it is excluded (its seed and its PnL) whether its price is fresh or stale. Here okx
/// is `demo` against a `live` feed and holds a 90% loss at a stale price: the latch stays Active. The control,
/// the same book on a paper account (no network), is valued at its last known price and trips.
#[test]
fn an_engine_on_another_network_stays_out_of_the_curve_even_when_it_is_stale() {
    use vike_model::AccountTier::{Demo, Live, Paper};
    let run = |tier| {
        let mut core = two_leg_core(("bybit", "BTCUSDT"), ("okx", "ETHUSDT"), (1_000.0, 1_000.0));
        core.config.price_only.as_mut().expect("clock").price_source =
            [("okx".to_string(), Live)].into_iter().collect();
        core.extra_engines[0].1.tier = tier;
        set_position(&mut core.extra_engines[0].1, "okx", "ETHUSDT", 10.0, 100.0);
        core.extra_engines[0].1.price_board.set_quote("okx", "ETHUSDT", 10.0, 10.5, 1);
        core.arm_boundary_timers(0);
        core.drive_due_timers(70_000);
        core
    };
    let crossed = run(Demo);
    assert_eq!(crossed.engine.trading_state, TradingState::Active, "{:?}", crossed.recent);
    assert!(
        crossed.recent.iter().any(|e| e.contains("dials another network")),
        "{:?}",
        crossed.recent
    );
    let paper = run(Paper);
    assert_eq!(paper.engine.trading_state, TradingState::Reducing, "{:?}", paper.recent);
}

/// **A STOPPED engine's frozen board does not blind the latch, and is not a "dead pump".** A stopped
/// account keeps its position (the operator's to close) and its board never moves again, so it always reads
/// stale. It is valued at its last known price (a loss is never forgiven by stopping the account) and is not
/// said as a skip: the stop was said when it happened. A stopped engine of a MOUNTED venue was already so;
/// a stopped PRICE-ONLY one made the whole evaluation unmeasurable for ever.
#[test]
fn a_stopped_price_only_engines_last_known_loss_counts_and_blinds_nothing() {
    let mut core = two_leg_core(("bybit", "BTCUSDT"), ("okx", "ETHUSDT"), (1_000.0, 1_000.0));
    core.config.max_drawdown = Some(0.1);
    set_position(&mut core.extra_engines[0].1, "okx", "ETHUSDT", 10.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("okx", "ETHUSDT", 70.0, 70.5, 1); // frozen at t=1
    core.retired_engines.push(1);
    core.arm_boundary_timers(0);
    core.drive_due_timers(70_000);
    assert_eq!(core.engine.trading_state, TradingState::Reducing, "{:?}", core.recent);
    assert!(latch_note(&core).contains("own_pnl=-300.00"), "{}", latch_note(&core));
    assert_eq!(skip_notes_of(&core, "okx"), 0, "a stop is no dead pump: {:?}", core.recent);
}
