//! The drawdown latch and the published totals fold PER CURRENCY GROUP (decision 0183, item 13).
//!
//! Every engine books in its own account's currency, so a curve that adds a EUR engine's figures to a
//! USD engine's is no money in either: a EUR loss hides behind a dollar gain, or two small
//! losses in different currencies read as one big one. The latch keeps `(base, pnl, peak)` per
//! group (engine 0's group in `pnl_curve_peak`, every other in `foreign_curves`) and trips
//! when ANY group breaches; the published totals are the PRIMARY group's alone (the first LIVE engine's:
//! `snapshot::build` folds engine 0's and the publish site restates it when engine 0 is a tombstone),
//! `Portfolio::ledger_totals` answers the rest and `Portfolio::group_curves` lets a push alert watch them. A node of ONE currency, which is every node today,
//! is covered by `drawdown_latch.rs` and `crates/vike-core/tests/drawdown_measures_own_pnl.rs`,
//! unchanged; the last test here pins that its notes and state did not move.

use super::*;
use crate::runtime::test_support::{core_of, mount_of, sim_engine_with};
use vike_exec::LedgerTotals;

/// A flat engine on `venue` whose account the node TOLD it is in `ccy` (as the node does at assembly).
fn engine_in(venue: &str, ccy: &str) -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        "EURUSD",
    );
    e.set_account_ccy(ccy).expect("a currency code");
    e
}

/// A core whose PRIMARY is the sim USD engine and whose extras are `(venue, currency)` engines, every
/// engine seeded with `seed`. The booked loss is set per engine by the test (`account.realized_pnl`).
fn core_in(seed: f64, extras: &[(&str, &str)]) -> CoreThread<RecordingClient> {
    let extras = extras.iter().map(|(venue, ccy)| (seed, engine_in(venue, ccy))).collect();
    let mut core = core_of(
        sim_engine_with(RecordingClient::default()),
        extras,
        CoreConfig { seed_cash: seed, ..CoreConfig::default() },
    );
    core.config.seed_cash = seed;
    core
}

/// [`core_in`] with the PRIMARY engine in `primary` too (`(venue, currency)`), so a test can put engine 0 in
/// any group.
fn core_led_by(
    seed: f64,
    primary: (&str, &str),
    extras: &[(&str, &str)],
) -> CoreThread<RecordingClient> {
    let extras = extras.iter().map(|(venue, ccy)| (seed, engine_in(venue, ccy))).collect();
    core_of(
        engine_in(primary.0, primary.1),
        extras,
        CoreConfig { seed_cash: seed, ..CoreConfig::default() },
    )
}

/// Stop the running account `key` (phase one of a stop: the engine becomes a tombstone).
fn stop_account(core: &mut CoreThread<RecordingClient>, key: &str, now_ms: i64) {
    core.apply_account_change(
        vike_exec::AccountChange::StopEngine {
            not_running: vike_exec::NotRunningAccount {
                venue: key.to_string(),
                account: None,
                route_key: key.to_string(),
                tier: None,
                reason: "account inactive".to_string(),
            },
        },
        now_ms,
    );
}

/// Start account `venue` again, as a NEW engine seeded 1000 and told it is in `ccy`.
fn restart_as(core: &mut CoreThread<RecordingClient>, venue: &str, ccy: &str, now_ms: i64) {
    core.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
    core.apply_account_change(
        vike_exec::AccountChange::add_engine(1_000.0, engine_in(venue, ccy)),
        now_ms,
    );
}

/// Publish through the real guarded path and return the portfolio that landed in the cell.
fn published(core: &mut CoreThread<RecordingClient>) -> Arc<CoreSnapshot> {
    core.publish_guarded();
    core.snapshot.load_full()
}

/// A mount that does nothing: the Residual row's arithmetic is the subject, not a strategy.
struct Idle;
impl Strategy<LiveBroker> for Idle {}

fn latch_notes(core: &CoreThread<RecordingClient>) -> Vec<String> {
    core.recent.iter().filter(|e| e.starts_with("DRAWDOWN LATCH")).map(|e| e.to_string()).collect()
}

fn all_reducing(core: &CoreThread<RecordingClient>) -> bool {
    core.engine.trading_state == TradingState::Reducing
        && core.extra_engines.iter().all(|(_, e)| e.trading_state == TradingState::Reducing)
}

/// ⚠ **A loss in one currency is not diluted by the capital in another.** USD flat, EUR down 300 of
/// its own 1000 (30%) against a 20% rule. Summed across currencies the curve reads 300 of 2000, a 15%
/// drawdown, and the latch would sit quiet while the EUR account lost 30%.
///
/// FAILS on a latch that folds both engines as one curve.
#[test]
fn a_eur_loss_trips_the_latch_though_the_summed_curve_would_not() {
    let mut core = core_in(1_000.0, &[("oanda", "EUR")]);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.engine.trading_state, TradingState::Active, "PREMISE: flat at the peak");

    core.extra_engines[0].1.account.realized_pnl = -300.0;
    core.sweep_drawdown_latch(0.2);
    assert!(all_reducing(&core), "EUR is 30% down: the latch is node-wide: {:?}", core.recent);
    let notes = latch_notes(&core);
    assert_eq!(notes.len(), 1, "one group breached, one note: {notes:?}");
    assert!(
        notes[0].starts_with("DRAWDOWN LATCH (EUR group):"),
        "the note names the group: {notes:?}"
    );
    assert!(notes[0].contains("drawdown=30.00%"), "30% of the EUR group's own 1000: {notes:?}");
    assert!(notes[0].contains("capital_base=1000.00"), "the EUR base, not the sum: {notes:?}");
}

/// ⚠ **A gain in one currency does not hide a loss in another.** USD up 500 (the old sum reads +200
/// on 2000: no drawdown at all), EUR down 300 of 1000.
#[test]
fn a_dollar_gain_does_not_hide_a_eur_loss() {
    let mut core = core_in(1_000.0, &[("oanda", "EUR")]);
    core.sweep_drawdown_latch(0.2);
    core.engine.account.realized_pnl = 500.0;
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.engine.trading_state, TradingState::Active, "PREMISE: USD up, EUR flat");

    core.extra_engines[0].1.account.realized_pnl = -300.0;
    core.sweep_drawdown_latch(0.2);
    assert!(all_reducing(&core), "the EUR group is 30% under its own peak: {:?}", core.recent);
}

/// And the primary's own loss still trips when a EUR gain sits beside it: the dollar group is judged
/// on its own, with the note naming it once there are several groups.
#[test]
fn a_dollar_loss_trips_though_a_eur_gain_sits_beside_it() {
    let mut core = core_in(1_000.0, &[("oanda", "EUR")]);
    core.sweep_drawdown_latch(0.2);
    core.extra_engines[0].1.account.realized_pnl = 900.0;
    core.engine.account.realized_pnl = -300.0;
    core.sweep_drawdown_latch(0.2);
    assert!(all_reducing(&core), "USD is 30% down: {:?}", core.recent);
    assert!(latch_notes(&core)[0].starts_with("DRAWDOWN LATCH (USD group):"), "{:?}", core.recent);
}

/// Each group's peak is its own: EUR rises to 1500 then falls to 1100 (26.7% under ITS peak) while the
/// sum of both curves (2500 -> 2100) is 16% under its. Nothing may measure EUR against the sum's peak.
#[test]
fn every_group_keeps_its_own_high_water_mark() {
    let mut core = core_in(1_000.0, &[("oanda", "EUR")]);
    core.extra_engines[0].1.account.realized_pnl = 500.0;
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.pnl_curve_peak, Some(1_000.0), "the dollar group's peak is its own capital");
    assert_eq!(core.foreign_curves.len(), 1);
    assert_eq!(core.foreign_curves[0].ccy, "EUR");
    assert_eq!(core.foreign_curves[0].peak, Some(1_500.0));
    assert_eq!(core.engine.trading_state, TradingState::Active);

    core.extra_engines[0].1.account.realized_pnl = 100.0;
    core.sweep_drawdown_latch(0.2);
    assert!(all_reducing(&core), "1100 under a 1500 peak: {:?}", core.recent);
    assert_eq!(core.foreign_curves[0].peak, Some(1_500.0), "a drop does not move the peak");
}

/// Three groups: only the one that breaches is named, and the other two stay quiet.
#[test]
fn three_groups_are_judged_one_by_one() {
    let mut core = core_in(1_000.0, &[("oanda", "EUR"), ("ig", "GBP")]);
    core.sweep_drawdown_latch(0.2);
    core.engine.account.realized_pnl = -50.0;
    core.extra_engines[0].1.account.realized_pnl = -100.0;
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.engine.trading_state, TradingState::Active, "5% and 10% under a 20% rule");
    assert_eq!(core.foreign_curves.len(), 2, "EUR and GBP each have their state");

    core.extra_engines[1].1.account.realized_pnl = -250.0;
    core.sweep_drawdown_latch(0.2);
    assert!(all_reducing(&core), "GBP is 25% down: {:?}", core.recent);
    let notes = latch_notes(&core);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("DRAWDOWN LATCH (GBP group):"), "{notes:?}");
}

/// A STOPPED EUR engine keeps its booked loss in the EUR group: the latch still sees it.
#[test]
fn a_retired_eur_engine_still_counts_in_its_group() {
    let mut core = core_in(1_000.0, &[("oanda", "EUR")]);
    core.sweep_drawdown_latch(0.2);
    core.apply_account_change(
        vike_exec::AccountChange::StopEngine {
            not_running: vike_exec::NotRunningAccount {
                venue: "oanda".to_string(),
                account: None,
                route_key: "oanda".to_string(),
                tier: None,
                reason: "account inactive".to_string(),
            },
        },
        10,
    );
    assert!(core.is_retired(1), "PREMISE: the EUR engine is a tombstone");
    core.extra_engines[0].1.account.realized_pnl = -300.0;
    core.sweep_drawdown_latch(0.2);
    assert_eq!(
        core.engine.trading_state,
        TradingState::Reducing,
        "the stopped engine's loss is still a EUR fact: {:?}",
        core.recent
    );
}

/// A seed that joins or leaves the base moves the peak of ITS group alone.
#[test]
fn a_seed_shifts_only_its_own_groups_peak() {
    let mut core = core_in(1_000.0, &[("oanda", "EUR")]);
    core.sweep_drawdown_latch(0.2);
    assert_eq!((core.pnl_curve_peak, core.foreign_curves[0].peak), (Some(1_000.0), Some(1_000.0)));

    let eur = core.extra_engines[0].1.ledger_ccy_key();
    core.shift_curve_peak(eur, 250.0);
    assert_eq!(core.foreign_curves[0].peak, Some(1_250.0));
    assert_eq!(core.pnl_curve_peak, Some(1_000.0), "the dollar peak did not move");

    core.shift_curve_peak(core.engine.ledger_ccy_key(), -100.0);
    assert_eq!(core.pnl_curve_peak, Some(900.0));
    assert_eq!(core.foreign_curves[0].peak, Some(1_250.0));

    core.shift_curve_peak("JPY".into(), 5.0); // a group the latch has not seen: nothing to move
    assert_eq!((core.pnl_curve_peak, core.foreign_curves[0].peak), (Some(900.0), Some(1_250.0)));
}

/// The published totals are the latch's, bit for bit, group by group: the primary group is the
/// snapshot's `capital_base` / `pnl_total`, and every other group's row is the latch's pair for it.
#[test]
fn the_published_groups_equal_the_latchs_bit_for_bit() {
    let mut core = core_in(1_000.0, &[("oanda", "EUR"), ("ig", "GBP")]);
    core.engine.account.realized_pnl = 12.345_678_9;
    core.extra_engines[0].1.account.realized_pnl = -7.654_321_1;
    core.extra_engines[0].1.account.fees_paid = 0.123_456_7;
    // NOT π: `clippy::approx_constant` is deny-by-default; any un-round decimal serves.
    core.extra_engines[1].1.account.funding_paid = 3.847_216_5;
    // An UNREALIZED term in every group, priced off a live quote: the fold the latch runs
    // (`resolved_own_pnl`) and the one the snapshot publishes (`unrealized` of each block) must agree to
    // the bit on a term that is not a plain account scalar.
    set_position(&mut core.engine, "sim", "BTCUSDT", 2.0, 100.125);
    core.engine.price_board.set_quote("sim", "BTCUSDT", 110.375, 110.875, 1);
    set_position(&mut core.extra_engines[0].1, "oanda", "EURUSD", 3.0, 1.103_7);
    core.extra_engines[0].1.price_board.set_quote("oanda", "EURUSD", 1.208_9, 1.209_4, 1);
    set_position(&mut core.extra_engines[1].1, "ig", "EURUSD", -4.0, 1.211_3);
    core.extra_engines[1].1.price_board.set_quote("ig", "EURUSD", 1.198_7, 1.199_2, 1);

    let snap = crate::snapshot::build(
        1,
        &core.engine,
        &core.extra_engines,
        core.config.seed_cash,
        core.config.price_cfg,
        vike_model::MarginCallConfig::default().mm_requirement,
        &core.recent,
        &core.bars,
        std::sync::Arc::new([]),
        &core.fault,
        0,
        0,
        vike_exec::ReconBlock::default(),
        Vec::new(),
    );
    let terms = core.drawdown_terms();
    let pf = &snap.portfolio;
    assert!(
        pf.venues.iter().all(|v| v.unrealized != 0.0),
        "PREMISE: every group holds an unrealized term: {:?}",
        pf.venues.iter().map(|v| v.unrealized).collect::<Vec<_>>()
    );
    assert_eq!(pf.capital_base.to_bits(), terms.base.to_bits(), "primary base");
    assert_eq!(pf.pnl_total().to_bits(), terms.pnl.to_bits(), "primary pnl");
    assert_eq!(
        pf.drawdown_curve().to_bits(),
        (terms.base + terms.pnl).to_bits(),
        "the curve an alert reads is the one the latch acts on"
    );
    let rows = pf.ledger_totals();
    assert_eq!(rows.len(), 3, "USD, EUR, GBP");
    assert_eq!(terms.foreign.len(), 2);
    for (k, (ccy, _base, pnl)) in terms.foreign.iter().enumerate() {
        assert_eq!(rows[k + 1].ccy, *ccy, "group order agrees");
        assert_eq!(rows[k + 1].pnl.to_bits(), pnl.to_bits(), "{ccy} pnl: published == latch");
    }
    // The totals the snapshot publishes are the primary group's alone.
    assert_eq!(pf.equity_total.to_bits(), rows[0].equity.to_bits());
    assert_eq!(pf.margin_used_total.to_bits(), rows[0].margin_used.to_bits());
    assert_eq!(
        pf.equity_total.to_bits(),
        pf.venues[0].equity.to_bits(),
        "the EUR and GBP engines are not in it"
    );
}

/// The equity sampler's TOTAL row is the PRIMARY group's alone: it is persisted as the node's equity
/// curve, and a row that added a EUR engine to a dollar one would be a series in no currency. Every
/// engine keeps its own row, in its own currency, and every OTHER group is a series of its own
/// (`TOTAL_EUR`), so the foreign equity is not lost and no stored row is a sum across currencies. A
/// node of one currency pushes no group row at all.
#[test]
fn the_equity_samplers_total_row_adds_the_primary_currency_only() {
    let captured = Arc::new(Mutex::new(Vec::<EquitySample>::new()));
    let cap = Arc::clone(&captured);
    let mut core = core_in(1_000.0, &[("oanda", "EUR")]);
    core.config.on_equity_sample =
        Some(Box::new(move |rows: &[EquitySample]| cap.lock().unwrap().extend_from_slice(rows)));
    core.engine.account.realized_pnl = 5.0;
    core.extra_engines[0].1.account.realized_pnl = -300.0;

    core.sample_equity(1_000);
    let rows = captured.lock().unwrap();
    let row = |venue: &str| rows.iter().find(|r| r.venue == venue).expect("a row").clone();
    let (total, usd, eur) = (row("TOTAL"), row("sim"), row("oanda"));
    assert_eq!(eur.realized, -300.0, "the EUR engine keeps its own row");
    assert_eq!(
        total.realized.to_bits(),
        usd.realized.to_bits(),
        "the EUR -300 is not in the TOTAL"
    );
    assert_eq!(total.equity.to_bits(), usd.equity.to_bits());
    assert_eq!(total.unrealized.to_bits(), usd.unrealized.to_bits());
    // The EUR group is its own series, and equals its one engine's row.
    let group = row("TOTAL_EUR");
    assert_eq!(group.equity.to_bits(), eur.equity.to_bits());
    assert_eq!(group.realized.to_bits(), eur.realized.to_bits());
    assert_eq!(rows.iter().filter(|r| r.venue.starts_with("TOTAL")).count(), 2);
}

/// One currency: the batch is the engines' rows and the one `TOTAL`, exactly as before.
#[test]
fn one_currency_samples_no_group_row() {
    let captured = Arc::new(Mutex::new(Vec::<EquitySample>::new()));
    let cap = Arc::clone(&captured);
    let mut core = core_in(1_000.0, &[("bybit", "USD")]);
    core.config.on_equity_sample =
        Some(Box::new(move |rows: &[EquitySample]| cap.lock().unwrap().extend_from_slice(rows)));
    core.sample_equity(1_000);
    let venues: Vec<String> = captured.lock().unwrap().iter().map(|r| r.venue.clone()).collect();
    assert_eq!(venues, ["sim", "bybit", "TOTAL"]);
}

/// A foreign account ADDED HOT joins its own group: no other group's peak moves with its seed (the
/// dollar group's peak is not raised by a EUR engine's capital), and the new group is judged on the
/// very next sweep against its own base.
#[test]
fn a_foreign_account_added_hot_moves_no_other_groups_peak() {
    let mut core = core_in(1_000.0, &[]);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.pnl_curve_peak, Some(1_000.0), "PREMISE: the dollar group's peak");

    core.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
    let mut eur = engine_in("oanda", "EUR");
    eur.account.realized_pnl = -300.0; // 30% of the 1000 it joins with
    core.apply_account_change(vike_exec::AccountChange::add_engine(1_000.0, eur), 10);

    assert_eq!(core.pnl_curve_peak, Some(1_000.0), "the EUR seed did not raise the dollar peak");
    assert!(core.foreign_curves.is_empty(), "a group the latch has not swept has no state yet");
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.foreign_curves.len(), 1);
    assert_eq!(core.foreign_curves[0].peak, Some(1_000.0), "seeded at its own base");
    assert!(all_reducing(&core), "30% under its own base: {:?}", core.recent);
    assert!(latch_notes(&core)[0].starts_with("DRAWDOWN LATCH (EUR group):"));
}

/// ⚠ **A restart that changes the account's currency group must not zero the old group's capital**: the
/// stopped EUR engine's seed and peak stay while its loss still counts in that group, or the group
/// disarms ("capital base 0.00 is not positive") and a loss that is still a fact is no longer judged.
#[test]
fn a_restart_into_another_group_keeps_the_stopped_groups_base_and_judges_its_loss() {
    let mut core = core_in(1_000.0, &[("oanda", "EUR")]);
    core.sweep_drawdown_latch(0.2);
    core.extra_engines[0].1.account.realized_pnl = -150.0; // 15%: under the 20% rule
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.engine.trading_state, TradingState::Active, "PREMISE: 15% under a 20% rule");
    core.apply_account_change(
        vike_exec::AccountChange::StopEngine {
            not_running: vike_exec::NotRunningAccount {
                venue: "oanda".to_string(),
                account: None,
                route_key: "oanda".to_string(),
                tier: None,
                reason: "account inactive".to_string(),
            },
        },
        10,
    );
    // The account starts again, now told it is a DOLLAR account: another group than its tombstone's.
    core.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
    core.apply_account_change(
        vike_exec::AccountChange::add_engine(1_000.0, engine_in("oanda", "USD")),
        20,
    );

    let terms = core.drawdown_terms();
    let eur =
        terms.foreign.iter().find(|(c, _, _)| *c == "EUR").expect("the EUR group is still there");
    assert_eq!(eur.1, 1_000.0, "the tombstone's seed stays in the base of its group");
    assert_eq!(eur.2, -150.0, "and so does its loss");
    assert_eq!(
        terms.base, 2_000.0,
        "the dollar group: the primary and the new account, one seed each"
    );
    assert_eq!(core.foreign_curves[0].peak, Some(1_000.0), "the EUR peak was not shifted down");
    core.sweep_drawdown_latch(0.2);
    assert!(
        !core.recent.iter().any(|e| e.contains("DISARMED")),
        "no group disarms: {:?}",
        core.recent
    );

    // The tombstone's loss is still judged: another 100 EUR is 25% of its group.
    core.extra_engines[0].1.account.realized_pnl = -250.0;
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.engine.trading_state, TradingState::Reducing, "{:?}", core.recent);
    assert!(latch_notes(&core).iter().any(|n| n.starts_with("DRAWDOWN LATCH (EUR group):")));
}

/// The same restart INTO THE SAME group is what it always was: the tombstone's seed leaves the base and the
/// peak with it, the new seed joins, and nothing disarms.
#[test]
fn a_restart_into_the_same_group_still_re_states_the_capital() {
    let mut core = core_in(1_000.0, &[("oanda", "EUR")]);
    core.sweep_drawdown_latch(0.2);
    core.extra_engines[0].1.account.realized_pnl = -150.0;
    core.apply_account_change(
        vike_exec::AccountChange::StopEngine {
            not_running: vike_exec::NotRunningAccount {
                venue: "oanda".to_string(),
                account: None,
                route_key: "oanda".to_string(),
                tier: None,
                reason: "account inactive".to_string(),
            },
        },
        10,
    );
    core.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
    core.apply_account_change(
        vike_exec::AccountChange::add_engine(1_000.0, engine_in("oanda", "EUR")),
        20,
    );
    let terms = core.drawdown_terms();
    let eur = terms.foreign.iter().find(|(c, _, _)| *c == "EUR").expect("the EUR group");
    assert_eq!(eur.1, 1_000.0, "one seed in the base: the tombstone's left, the new one joined");
    assert_eq!(eur.2, -150.0);
    assert_eq!(
        core.foreign_curves[0].peak,
        Some(1_000.0),
        "-1000 for the old seed, +1000 for the new"
    );
}

// ---- a NON-dollar engine's capital is the wallet it adopted, not the configured seed (decision 0183) --------

/// `core.extra_engines[k]` has adopted the venue wallet `wallet` (what a reconcile pass or an account frame does).
fn adopts(core: &mut CoreThread<RecordingClient>, k: usize, wallet: f64) {
    let account = &mut core.extra_engines[k].1.account;
    account.balance = wallet;
    account.balance_mode = BalanceMode::Authoritative;
}

/// ⚠ **FXCM's EUR demo account (a 50,000 EUR wallet, seed_cash 1000) is judged against its wallet.** Told EUR it
/// is a group of its own, and judged against ONE seed a 250 EUR move on a 100,000-unit position (0.5% of its
/// money) read as 25% and put the whole node, live accounts included, in liquidate-only. 250 EUR must NOT trip a
/// 20% rule; 12,000 EUR (24% of the wallet) must, and the dollar group keeps its own seed throughout.
///
/// FAILS on a latch whose base for the EUR group is the configured seed.
#[test]
fn a_eur_group_is_judged_against_the_wallet_it_adopted_not_the_seed() {
    let mut core = core_in(1_000.0, &[("fxcm", "EUR")]);
    adopts(&mut core, 0, 50_000.0);
    core.sweep_drawdown_latch(0.2);
    let terms = core.drawdown_terms();
    assert_eq!(terms.base, 1_000.0, "the dollar group keeps its configured seed");
    assert_eq!(terms.foreign[0].1, 50_000.0, "the EUR base is the wallet");
    assert_eq!(core.seed_of(1), 50_000.0, "the one capital-base slot every reader folds");
    assert_eq!(core.foreign_curves[0].peak, Some(50_000.0));

    core.extra_engines[0].1.account.realized_pnl = -250.0; // 25% of the seed, 0.5% of the wallet
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.engine.trading_state, TradingState::Active, "{:?}", core.recent);
    assert!(latch_notes(&core).is_empty(), "{:?}", core.recent);
    let snap = published(&mut core);
    let rows: Vec<(&str, f64, f64)> = snap
        .portfolio
        .group_curves
        .iter()
        .map(|g| (LedgerTotals::label_of(g.ccy), g.curve, g.peak))
        .collect();
    assert_eq!(rows, [("EUR", 49_750.0, 50_000.0)], "the alert watches the same curve");

    core.extra_engines[0].1.account.realized_pnl = -12_000.0; // 24% of the wallet
    core.sweep_drawdown_latch(0.2);
    assert!(all_reducing(&core), "{:?}", core.recent);
    let notes = latch_notes(&core);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("DRAWDOWN LATCH (EUR group):"), "{notes:?}");
    assert!(notes[0].contains("capital_base=50000.00"), "{notes:?}");
    assert!(notes[0].contains("drawdown=24.00%"), "{notes:?}");
}

/// ⚠ **The dollar group is unchanged**: an `Authoritative` dollar engine holding a big shared wallet (the CI box's
/// bybit demo, 53,647) keeps its CONFIGURED seed, so the loss that tripped the latch before still trips it, and
/// nothing is recorded as adopted. Pinned beside a EUR group that DOES adopt.
#[test]
fn the_dollar_groups_base_stays_the_configured_seeds_whatever_wallet_its_engines_hold() {
    let mut core = core_in(1_000.0, &[("bybit", "USD"), ("okx", "USDT"), ("fxcm", "EUR")]);
    adopts(&mut core, 0, 53_647.0);
    adopts(&mut core, 1, 9_000.0);
    adopts(&mut core, 2, 50_000.0);
    core.sweep_drawdown_latch(0.2);
    let terms = core.drawdown_terms();
    assert_eq!(terms.base, 3_000.0, "sim + bybit + okx, one configured seed each");
    assert_eq!((core.seed_of(1), core.seed_of(2)), (1_000.0, 1_000.0));
    assert_eq!(core.pnl_curve_peak, Some(3_000.0));
    assert_eq!(terms.foreign[0].1, 50_000.0, "only the EUR engine adopted");
    assert_eq!(core.base_adopted.iter().copied().collect::<Vec<_>>(), [3]);

    core.engine.account.realized_pnl = -700.0; // 23.3% of the dollar group's 3000
    core.sweep_drawdown_latch(0.2);
    assert!(all_reducing(&core), "{:?}", core.recent);
    assert!(latch_notes(&core)[0].starts_with("DRAWDOWN LATCH (USD group):"), "{:?}", core.recent);
}

/// The base is taken ONCE: a third party's later deposit or withdrawal in the wallet moves neither the base,
/// the peak nor the curve, which is what keeps the account's wallet out of the latch's number.
#[test]
fn the_adopted_base_is_taken_once_and_a_later_wallet_move_changes_nothing() {
    let mut core = core_in(1_000.0, &[("fxcm", "EUR")]);
    adopts(&mut core, 0, 50_000.0);
    core.sweep_drawdown_latch(0.2);
    adopts(&mut core, 0, 80_000.0); // a deposit
    core.sweep_drawdown_latch(0.2);
    adopts(&mut core, 0, 20_000.0); // a withdrawal
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.seed_of(1), 50_000.0);
    assert_eq!(core.foreign_curves[0].peak, Some(50_000.0));
    assert_eq!(core.foreign_curves[0].curve, 50_000.0);
    assert_eq!(core.engine.trading_state, TradingState::Active, "{:?}", core.recent);
}

/// A wallet that is adopted AFTER the group's first sweep (the reconcile pass lands seconds after the start)
/// moves the group's peak by the difference: an addition is not a new high, and a profit booked before the
/// adoption stays in the peak.
#[test]
fn a_wallet_adopted_after_the_first_sweep_moves_the_peak_with_the_base() {
    let mut core = core_in(1_000.0, &[("fxcm", "EUR")]);
    core.extra_engines[0].1.account.realized_pnl = 300.0;
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.foreign_curves[0].peak, Some(1_300.0), "PREMISE: on the seed, 300 up");

    adopts(&mut core, 0, 50_000.0);
    core.extra_engines[0].1.account.realized_pnl = 50.0; // 250 off the peak: 25% of the SEED
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.foreign_curves[0].peak, Some(50_300.0), "1300 + (50000 - 1000)");
    assert_eq!(core.foreign_curves[0].curve, 50_050.0);
    assert_eq!(core.engine.trading_state, TradingState::Active, "{:?}", core.recent);
}

/// What is NOT adopted: a `Delta` engine (its seed is its capital), a wallet that is no positive number (it
/// stays on the seed and is looked at again), and a STOPPED engine (its successor states the capital).
#[test]
fn only_a_running_authoritative_non_dollar_engine_with_a_positive_wallet_adopts() {
    let mut core = core_in(1_000.0, &[("a", "EUR"), ("b", "EUR"), ("c", "GBP"), ("d", "GBP")]);
    core.extra_engines[0].1.account.balance = 50_000.0; // Delta: a paper or never-reconciled engine
    adopts(&mut core, 1, 0.0);
    adopts(&mut core, 2, f64::NAN);
    adopts(&mut core, 3, 40_000.0);
    stop_account(&mut core, "d", 10);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.seed_of(1), 1_000.0, "Delta");
    assert_eq!(core.seed_of(2), 1_000.0, "an empty wallet");
    assert_eq!(core.seed_of(3), 1_000.0, "no number");
    assert_eq!(core.seed_of(4), 1_000.0, "a tombstone");
    assert!(core.base_adopted.is_empty());

    adopts(&mut core, 1, 7_000.0); // the empty wallet is funded: looked at again
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.seed_of(2), 7_000.0);
    assert_eq!(core.base_adopted.iter().copied().collect::<Vec<_>>(), [2]);
}

/// An adopting engine 0 (the node led by the EUR account) writes the same slot the snapshot folds, so the
/// published `capital_base` and the curve an alert reads stay the number the latch acts on, bit for bit.
#[test]
fn an_adopting_primary_keeps_the_published_curve_and_the_latchs_the_same_number() {
    let mut core = core_led_by(1_000.0, ("fxcm", "EUR"), &[("bybit", "USD")]);
    core.engine.account.balance = 50_000.0;
    core.engine.account.balance_mode = BalanceMode::Authoritative;
    core.sweep_drawdown_latch(0.2);
    core.engine.account.realized_pnl = -250.0;
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.engine.trading_state, TradingState::Active, "{:?}", core.recent);

    let terms = core.drawdown_terms();
    assert_eq!(terms.base, 50_000.0);
    let snap = published(&mut core);
    assert_eq!(snap.portfolio.capital_base.to_bits(), terms.base.to_bits());
    assert_eq!(
        snap.portfolio.drawdown_curve().to_bits(),
        (terms.base + terms.pnl).to_bits(),
        "the headline curve is the latch's"
    );
}

/// A restart INTO THE SAME group after an adoption: the tombstone's adopted base leaves the base and the peak
/// together (the base slot holds what the peak was shifted by), the new engine joins on its seed and adopts its
/// own wallet on the next sweep. Nothing disarms and nothing trips.
#[test]
fn a_restart_into_the_same_group_takes_the_adopted_base_out_with_the_tombstone() {
    let mut core = core_in(1_000.0, &[("fxcm", "EUR")]);
    adopts(&mut core, 0, 50_000.0);
    core.sweep_drawdown_latch(0.2);
    core.extra_engines[0].1.account.realized_pnl = -150.0;
    stop_account(&mut core, "fxcm", 10);

    core.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
    core.apply_account_change(
        vike_exec::AccountChange::add_engine(1_000.0, engine_in("fxcm", "EUR")),
        20,
    );
    assert_eq!(core.foreign_curves[0].peak, Some(1_000.0), "-50000 (adopted) + 1000 (new seed)");
    let eur = core.drawdown_terms().foreign.remove(0);
    assert_eq!((eur.1, eur.2), (1_000.0, -150.0), "one base in the group: the new engine's seed");

    adopts(&mut core, 1, 49_000.0);
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.seed_of(2), 49_000.0);
    assert_eq!(core.foreign_curves[0].peak, Some(49_000.0));
    assert_eq!(core.engine.trading_state, TradingState::Active, "{:?}", core.recent);
    assert!(!core.recent.iter().any(|e| e.contains("DISARMED")), "{:?}", core.recent);
}

/// A PRICE-ONLY run judges each group too: the engine no mount drives is priced by its own bounded cfg and
/// the EUR group trips alone; a stale mark no longer makes the curve unmeasurable, the engine is valued at its
/// last known price and the group trips just the same (the margin call is the one that skips it).
#[test]
fn a_price_only_run_judges_each_group_and_a_stale_engine_at_its_last_known_price() {
    let build = || {
        let mut eur = engine_in("bybit", "EUR");
        eur.symbol = "ETHUSDT".to_string();
        core_of(
            sim_engine_with(RecordingClient::default()),
            vec![(100.0, eur)],
            CoreConfig {
                seed_cash: 100.0,
                max_drawdown: Some(0.2),
                price_only: Some(PriceOnlyConfig {
                    venues: ["bybit".to_string()].into_iter().collect(),
                    sweep_interval: std::time::Duration::from_secs(1),
                    max_price_age: std::time::Duration::from_secs(60),
                    price_source: std::collections::BTreeMap::new(),
                }),
                ..CoreConfig::default()
            },
        )
    };
    // Fresh: long 1 at 100 on a seed of 100, the market at 60 (a 40% loss of the EUR group's capital).
    let mut core = build();
    set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 1.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 60.0, 60.5, 1);
    core.arm_boundary_timers(0);
    core.drive_due_timers(2_000);
    assert_eq!(core.engine.trading_state, TradingState::Reducing, "{:?}", core.recent);
    assert!(latch_notes(&core)[0].starts_with("DRAWDOWN LATCH (EUR group):"), "{:?}", core.recent);

    // Stale: the same loss on a quote older than the bound: valued at its last known price, the EUR group
    // trips as it does fresh.
    let mut core = build();
    set_position(&mut core.extra_engines[0].1, "bybit", "ETHUSDT", 1.0, 100.0);
    core.extra_engines[0].1.price_board.set_quote("bybit", "ETHUSDT", 60.0, 60.5, 1);
    core.arm_boundary_timers(0);
    core.drive_due_timers(70_000);
    assert_eq!(core.engine.trading_state, TradingState::Reducing, "{:?}", core.recent);
    assert!(latch_notes(&core)[0].starts_with("DRAWDOWN LATCH (EUR group):"), "{:?}", core.recent);
}

/// A node of ONE currency: the state the latch leaves, and its notes, are what they always were.
#[test]
fn one_currency_leaves_no_foreign_state_and_the_notes_read_as_before() {
    let mut core = core_in(1_000.0, &[("bybit", "USD"), ("okx", "USDT")]);
    core.sweep_drawdown_latch(0.2);
    core.engine.account.realized_pnl = -900.0;
    core.sweep_drawdown_latch(0.2);
    assert!(all_reducing(&core));
    assert!(core.foreign_curves.is_empty(), "USD, USDT and the unsaid are one group");
    assert!(!core.ledger_mixed, "one group: nothing below runs");
    let snap = published(&mut core);
    assert!(snap.portfolio.group_curves.is_empty(), "no other group, nothing published for one");
    assert_eq!(
        snap.portfolio.primary_ccy_stated, None,
        "the totals are the first block's, as before"
    );
    let notes = latch_notes(&core);
    assert_eq!(notes.len(), 1);
    assert!(
        notes[0].starts_with("DRAWDOWN LATCH: liquidate-only (peak=3000.00 curve=2100.00"),
        "{notes:?}"
    );
}

// ---- the groups a push alert cannot see through the primary's curve, and what "primary" means ----------

/// ⚠ **The curve and peak of every group but the primary's are PUBLISHED**, in the group's own money, so a
/// push alert can watch what the latch watches (it trips the whole node when ANY group breaches, and the
/// primary's curve alone would stay flat through it). The primary's own group is not listed: its curve is
/// `Portfolio::drawdown_curve`.
#[test]
fn the_published_snapshot_carries_every_other_groups_curve_and_peak() {
    let mut core = core_in(1_000.0, &[("oanda", "EUR"), ("ig", "GBP")]);
    core.sweep_drawdown_latch(0.2);
    core.extra_engines[0].1.account.realized_pnl = -150.0; // 15% under its peak: no trip at 20%
    core.extra_engines[1].1.account.realized_pnl = 40.0; // a new high of its own
    core.sweep_drawdown_latch(0.2);
    assert_eq!(core.engine.trading_state, TradingState::Active, "PREMISE: no group trips");

    let snap = published(&mut core);
    let rows: Vec<(&str, f64, f64)> = snap
        .portfolio
        .group_curves
        .iter()
        .map(|g| (LedgerTotals::label_of(g.ccy), g.curve, g.peak))
        .collect();
    assert_eq!(rows, [("EUR", 850.0, 1_000.0), ("GBP", 1_040.0, 1_040.0)]);

    // The same numbers the next sweep moves: a deeper EUR loss is the published curve of the next publish.
    core.extra_engines[0].1.account.realized_pnl = -300.0;
    core.sweep_drawdown_latch(0.2);
    let snap = published(&mut core);
    assert_eq!(snap.portfolio.group_curves[0].curve, 700.0);
    assert_eq!(snap.portfolio.group_curves[0].peak, 1_000.0, "a drop does not move the peak");
}

/// ⚠ **The primary group is the group of the first LIVE engine**, not of the first engine ever: engine 0 is
/// the EUR account `oanda`, engine 1 a dollar account. When `oanda` stops and starts again as a DOLLAR
/// account the headline must be the dollar group (two live dollar accounts), and the stopped EUR engine, a
/// tombstone that cannot trade, is a group listed beside it, not the headline reading 950 EUR while every
/// live account sits under "other ledgers".
///
/// FAILS on a node whose primary group stays engine 0's.
#[test]
fn the_headline_follows_the_first_live_engine_when_engine_zero_restarts_into_another_group() {
    let mut core = core_led_by(1_000.0, ("oanda", "EUR"), &[("bybit", "USD")]);
    core.engine.account.realized_pnl = -50.0;
    core.extra_engines[0].1.account.realized_pnl = 20.0;
    core.sweep_drawdown_latch(0.2);
    let before = published(&mut core);
    assert_eq!(LedgerTotals::label_of(before.portfolio.primary_ccy()), "EUR", "PREMISE: EUR leads");
    assert_eq!(
        before.portfolio.equity_total.to_bits(),
        before.portfolio.venues[0].equity.to_bits()
    );

    stop_account(&mut core, "oanda", 10);
    assert!(core.is_retired(0), "PREMISE: engine 0 is a tombstone");
    restart_as(&mut core, "oanda", "USD", 20);
    core.sweep_drawdown_latch(0.2);
    let snap = published(&mut core);
    let pf = &snap.portfolio;
    assert_eq!(pf.venues.len(), 3, "the tombstone, the dollar account, the restarted one");
    assert_eq!(
        LedgerTotals::label_of(pf.venues[0].ledger_ccy),
        "EUR",
        "PREMISE: the tombstone's group"
    );
    assert_eq!(LedgerTotals::label_of(pf.primary_ccy()), "USD", "the first LIVE engine's group");
    let usd = vike_model::py_sum([pf.venues[1].equity, pf.venues[2].equity]);
    assert_eq!(
        pf.equity_total.to_bits(),
        usd.to_bits(),
        "both live dollar accounts, not the tombstone"
    );
    assert_ne!(pf.equity_total.to_bits(), pf.venues[0].equity.to_bits());
    assert_eq!(pf.capital_base, 2_000.0, "one seed each of the two live dollar accounts");
    let rows = pf.ledger_totals();
    assert_eq!(rows.len(), 2);
    assert_eq!(LedgerTotals::label_of(rows[0].ccy), "USD", "the headline group leads the rows");
    // The tombstone's group is still a fact, and the alert can watch it: its seed and loss stay in it.
    let groups: Vec<(&str, f64, f64)> =
        pf.group_curves.iter().map(|g| (LedgerTotals::label_of(g.ccy), g.curve, g.peak)).collect();
    assert_eq!(groups, [("EUR", 950.0, 1_000.0)]);
}

/// The other way round: engine 0 (a dollar account) restarts into EUR while engine 1 is a dollar account:
/// the first live engine is still dollar, so the headline stays dollar, and the stopped dollar engine (with its
/// loss) stays in it; the restarted EUR account is a group beside it.
#[test]
fn the_headline_stays_dollar_when_engine_zero_restarts_into_eur_beside_a_live_dollar_account() {
    let mut core = core_led_by(1_000.0, ("oanda", "USD"), &[("bybit", "USD")]);
    assert!(!core.ledger_mixed, "PREMISE: one currency");
    core.engine.account.realized_pnl = -50.0;
    core.sweep_drawdown_latch(0.2);
    stop_account(&mut core, "oanda", 10);
    restart_as(&mut core, "oanda", "EUR", 20);
    assert!(core.ledger_mixed, "the hot add made a second group");
    core.sweep_drawdown_latch(0.2);

    let snap = published(&mut core);
    let pf = &snap.portfolio;
    assert_eq!(LedgerTotals::label_of(pf.primary_ccy()), "USD");
    let usd = vike_model::py_sum([pf.venues[0].equity, pf.venues[1].equity]);
    assert_eq!(pf.equity_total.to_bits(), usd.to_bits(), "the tombstone is still a dollar fact");
    assert_eq!(pf.capital_base, 2_000.0, "the EUR account's seed is not in it");
    let groups: Vec<(&str, f64, f64)> =
        pf.group_curves.iter().map(|g| (LedgerTotals::label_of(g.ccy), g.curve, g.peak)).collect();
    assert_eq!(groups, [("EUR", 1_000.0, 1_000.0)]);
}

/// The equity sampler's TOTAL follows a NON-DOLLAR primary the same way: the EUR account leads, so `TOTAL` is
/// its group's alone, the dollar accounts (USD and USDT: one group) are `TOTAL_USD` and the pound account is
/// `TOTAL_GBP`: three series, none of them a sum across currencies.
#[test]
fn the_equity_samplers_total_row_follows_a_non_dollar_primary_through_three_groups() {
    let captured = Arc::new(Mutex::new(Vec::<EquitySample>::new()));
    let cap = Arc::clone(&captured);
    let mut core =
        core_led_by(1_000.0, ("oanda", "EUR"), &[("bybit", "USD"), ("ig", "GBP"), ("okx", "USDT")]);
    core.config.on_equity_sample =
        Some(Box::new(move |rows: &[EquitySample]| cap.lock().unwrap().extend_from_slice(rows)));
    core.engine.account.realized_pnl = -300.0;
    core.extra_engines[0].1.account.realized_pnl = 5.0;
    core.extra_engines[1].1.account.realized_pnl = 7.0;
    core.extra_engines[2].1.account.realized_pnl = 11.0;

    core.sample_equity(1_000);
    let rows = captured.lock().unwrap();
    let venues: Vec<&str> = rows.iter().map(|r| r.venue.as_str()).collect();
    assert_eq!(venues, ["oanda", "bybit", "ig", "okx", "TOTAL", "TOTAL_USD", "TOTAL_GBP"]);
    let row = |venue: &str| rows.iter().find(|r| r.venue == venue).expect("a row").clone();
    let (eur, usd, gbp, usdt) = (row("oanda"), row("bybit"), row("ig"), row("okx"));
    let total = row("TOTAL");
    assert_eq!(total.equity.to_bits(), eur.equity.to_bits(), "TOTAL is the EUR group alone");
    assert_eq!(total.realized.to_bits(), eur.realized.to_bits());
    let dollars = row("TOTAL_USD");
    assert_eq!(dollars.equity.to_bits(), vike_model::py_sum([usd.equity, usdt.equity]).to_bits());
    assert_eq!(
        dollars.realized.to_bits(),
        vike_model::py_sum([usd.realized, usdt.realized]).to_bits()
    );
    assert_eq!(row("TOTAL_GBP").equity.to_bits(), gbp.equity.to_bits());
}

// ---- the Residual row (`account realized - sum of the mounts`) is one currency's too ------------------

/// A core with two idle mounts: mount 0 on the dollar `sim` engine, mount 1 on the `oanda` engine (in `ccy`).
fn core_with_two_mounts(ccy: &str) -> CoreThread<RecordingClient> {
    let core = core_of(
        sim_engine_with(RecordingClient::default()),
        vec![(1_000.0, engine_in("oanda", ccy))],
        CoreConfig {
            seed_cash: 1_000.0,
            strategy: Some(mount_of("sim", "BTCUSDT", "1m", Box::new(Idle))),
            extra_mounts: vec![mount_of("oanda", "EURUSD", "1m", Box::new(Idle))],
            ..CoreConfig::default()
        },
    );
    assert_eq!((core.mount_eng(0), core.mount_eng(1)), (0, 1), "PREMISE: one mount per engine");
    core
}

/// ⚠ **The Residual row adds the PRIMARY currency's accounts and mounts only** (decision 0183, item 13): it
/// is `account realized - sum of the mounts` and must be zero when every fill belongs to a mount. Summed
/// across currencies a EUR account's figure is added to a dollar one, so the row reads a number in no
/// currency; the mount rows themselves stay one per mount, each in its own account's money.
///
/// FAILS on a row that sums every engine (the EUR -304.5 is no dollar fact), and on one that sums the engines of
/// the primary group but every mount's attribution.
#[test]
fn the_residual_row_adds_the_primary_currency_only() {
    let mut core = core_with_two_mounts("EUR");
    core.engine.account.realized_pnl = 9.0; // the dollar account's
    core.extra_engines[0].1.account.realized_pnl = -304.5; // the EUR account's
    core.mount_attr[0].realized_pnl = 9.0;
    core.mount_attr[1].realized_pnl = -304.5;
    assert_eq!(core.account_net_realized(), 9.0, "the EUR account is another currency");

    let first = published(&mut core);
    let rows = &first.mounts;
    assert_eq!(rows.len(), 3, "two mounts and the residual");
    assert_eq!(rows[1].realized_pnl, -304.5, "the EUR mount keeps its own row, in EUR");
    assert_eq!(rows[2].realized_pnl, 0.0, "every dollar fill belongs to a dollar mount");

    // A move in the EUR account alone changes no dollar figure: the cached rows are still the answer.
    core.extra_engines[0].1.account.realized_pnl = -400.0;
    let same = published(&mut core);
    assert!(
        std::ptr::eq(first.mounts.as_ptr(), same.mounts.as_ptr()),
        "nothing in the dollar group moved, so the rows are reused"
    );
    assert_eq!(same.mounts[2].realized_pnl, 0.0);

    // A dollar ticket no mount minted is the residual, and only that: +2.
    core.engine.account.realized_pnl = 11.0;
    let moved = published(&mut core);
    assert_eq!(moved.mounts[2].realized_pnl, 2.0);
    assert_eq!(
        moved.mounts[2].realized_pnl.to_bits(),
        core.mount_views()[2].realized_pnl.to_bits()
    );
}

/// The control: two accounts of ONE currency (USD and USDT are one group) are added, as they always were.
#[test]
fn the_residual_row_of_one_currency_still_adds_every_account() {
    let mut core = core_with_two_mounts("USDT");
    core.engine.account.realized_pnl = 9.0;
    core.extra_engines[0].1.account.realized_pnl = -304.5;
    core.mount_attr[0].realized_pnl = 9.0;
    core.mount_attr[1].realized_pnl = -304.5;
    assert_eq!(core.account_net_realized(), -295.5, "both accounts, one currency");
    assert_eq!(core.mount_views()[2].realized_pnl, 0.0);
    core.extra_engines[0].1.account.realized_pnl = -400.0; // an unattributed move in the second account
    assert_eq!(core.mount_views()[2].realized_pnl, -95.5);
}
