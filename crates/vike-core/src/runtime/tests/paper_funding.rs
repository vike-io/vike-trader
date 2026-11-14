//! The funding-rate lane on a PAPER engine (`runtime/funding.rs`), through the real dispatch: a paper
//! position pays its venue's public rate once per settlement, at the settlement's own time, an inverse one
//! in its settle coin, a restored one after a restart too (every settlement it missed, from the venue's
//! stored history, once), and a live engine never from the rate.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use super::*;
use vike_execution::testing::{RecordingClient, engine_on};
use vike_execution::{Account, FundingRateUpdate, PositionEntry, RiskGate};
use vike_marketdata::FundingRate;
use vike_model::events::{FundingEvent, PositionSide};
use vike_model::{AccountTier, ContractValue, RiskLimits};

use crate::runtime::test_support::core_of;

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT.P";
const MARK: f64 = 60_000.0;
/// A settlement (2026-11-08 16:00 UTC) and the next one, eight hours on.
const T1: i64 = 1_794_153_600_000;
const T2: i64 = T1 + 8 * 3_600_000;
/// The core's clock before `T1`.
const BEFORE: i64 = T1 - 600_000;

/// A paper engine on `venue`/`symbol` (an inverse contract when `inverse`) told `tier`, holding `size` at
/// [`MARK`] with the venue's mark there.
fn engine(
    venue: &str,
    symbol: &str,
    inverse: bool,
    tier: AccountTier,
    size: f64,
) -> Engine<RecordingClient> {
    let mut e = if inverse {
        let mut grid = indexmap::IndexMap::new();
        grid.insert(symbol.to_string(), ContractValue::inverse(1.0));
        Engine::new(
            Account::new(1.0, venue, Some(grid), BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            venue,
            symbol,
        )
    } else {
        engine_on(venue, symbol, RecordingClient::default())
    };
    e.set_tier(tier);
    hold(&mut e, size);
    e
}

/// Put `size` of the engine's own symbol on its book at [`MARK`], with the venue's mark there.
fn hold(e: &mut Engine<RecordingClient>, size: f64) {
    let (venue, symbol) = (ustr::Ustr::from(e.venue.as_str()), ustr::Ustr::from(e.symbol.as_str()));
    e.account.positions.insert(
        (venue, symbol, PositionSide::Both),
        PositionEntry { size, average_price: MARK, ..PositionEntry::default() },
    );
    e.account.set_mark_from(&venue, &symbol, MARK, vike_execution::MarkSource::VenueMark, 0);
}

/// A core over `engine` with the funding law handed in (`law_on`) and a clock the test moves.
fn core(
    engine: Engine<RecordingClient>,
    law_on: bool,
) -> (CoreThread<RecordingClient>, Arc<AtomicI64>) {
    let now = Arc::new(AtomicI64::new(BEFORE));
    let read = Arc::clone(&now);
    let config = CoreConfig {
        clock: Box::new(move || read.load(Ordering::Relaxed)),
        // the law the hosts hand in, the simulator's one formula
        funding_law: law_on.then_some(ContractValue::funding as FundingLaw),
        ..CoreConfig::default()
    };
    (core_of(engine, Vec::new(), config), now)
}

/// One funding observation of the engine's symbol: `rate` for the settlement at `next_timestamp`.
fn tick(c: &mut CoreThread<RecordingClient>, rate: f64, next_ts: i64) {
    tick_priced(c, rate, next_ts, None);
}

fn tick_priced(c: &mut CoreThread<RecordingClient>, rate: f64, next_ts: i64, price: Option<f64>) {
    let (venue, symbol) = (c.engine.venue.clone(), c.engine.symbol.clone());
    c.dispatch(Ingest::FundingRate(Box::new(FundingRateUpdate {
        venue,
        symbol: symbol.clone(),
        funding: FundingRate {
            timestamp: c.engine.now_ms,
            symbol,
            rate,
            interval_ms: None,
            next_timestamp: Some(next_ts),
            predicted: None,
            price,
        },
    })));
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * b.abs().max(1.0)
}

/// **A paper long crossing ONE settlement pays rate x notional ONCE**, at the last rate the venue stated
/// for it, when the venue rolls to the next settlement; the same observation again books nothing.
#[test]
fn a_paper_long_crossing_one_settlement_pays_rate_times_notional_once() {
    let (mut c, _) = core(engine(VENUE, SYMBOL, false, AccountTier::Paper, 1.0), true);
    tick(&mut c, 0.0001, T1);
    tick(&mut c, 0.0002, T1); // the rate moves before the settlement
    assert_eq!(c.engine.account.funding_paid, 0.0, "nothing is due before the venue rolls");
    tick(&mut c, 0.0003, T2); // the venue rolled: T1 happened, at 0.0002
    let paid = 1.0 * MARK * 0.0002;
    assert!(close(c.engine.account.funding_paid, -paid), "{}", c.engine.account.funding_paid);
    assert!(close(c.engine.account.balance, -paid), "the cash pays it too");
    tick(&mut c, 0.0003, T2);
    tick(&mut c, 0.0004, T2);
    assert!(close(c.engine.account.funding_paid, -paid), "booked once per settlement");
    assert!(
        c.recent.iter().any(|n| n.starts_with("PAPER FUNDING binance BTCUSDT.P")),
        "the ring says it"
    );
    // A SHORT receives it.
    let (mut s, _) = core(engine(VENUE, SYMBOL, false, AccountTier::Paper, -2.0), true);
    tick(&mut s, 0.0001, T1);
    tick(&mut s, 0.0001, T2);
    assert!(close(s.engine.account.funding_paid, 2.0 * MARK * 0.0001), "a short is paid");
}

/// **The settlement is booked AT its time, on the position held then** (the timer armed at the feed's
/// `next_timestamp`), not when the venue's next observation arrives: a position closed after the settlement still
/// paid, and the venue's later roll books nothing more.
#[test]
fn the_timer_books_the_settlement_at_its_time_on_the_position_held_then() {
    let (mut c, now) = core(engine(VENUE, SYMBOL, false, AccountTier::Paper, 1.0), true);
    tick(&mut c, 0.0001, T1);
    c.drive_due_timers(T1 - 1_000);
    assert_eq!(c.engine.account.funding_paid, 0.0, "not before its time");
    now.store(T1 + 1_000, Ordering::Relaxed);
    c.drive_due_timers(T1 + 1_000);
    let paid = MARK * 0.0001;
    assert!(close(c.engine.account.funding_paid, -paid), "{}", c.engine.account.funding_paid);
    // The position is closed 27 s after the settlement; the venue rolls a minute later.
    hold(&mut c.engine, 0.0);
    tick(&mut c, 0.0002, T2);
    assert!(close(c.engine.account.funding_paid, -paid), "the roll books nothing more");
}

/// **A settlement the venue brings FORWARD is booked at the new time** (binance and okx drop to an hourly
/// interval when the rate hits its cap): the clock moves to the earlier `next_timestamp`, its timer is armed there, and
/// the older, later time books nothing more. An observation of a settlement already booked changes nothing.
#[test]
fn a_settlement_brought_forward_is_booked_at_its_new_time() {
    let (mut c, now) = core(engine(VENUE, SYMBOL, false, AccountTier::Paper, 1.0), true);
    tick(&mut c, 0.0001, T2);
    tick(&mut c, 0.003, T1); // capped: the next settlement is now T1, eight hours sooner
    now.store(T1 + 1_000, Ordering::Relaxed);
    c.drive_due_timers(T1 + 1_000);
    let paid = MARK * 0.003;
    assert!(close(c.engine.account.funding_paid, -paid), "{}", c.engine.account.funding_paid);
    tick(&mut c, 0.003, T1); // a late observation of the booked settlement
    tick(&mut c, 0.003, T1 + 3_600_000); // the venue rolls to the next hour
    assert!(close(c.engine.account.funding_paid, -paid), "T1 is booked once");
}

/// **An inverse (coin-margined) paper engine pays in its settle coin** (decision 0258): the rate on the
/// position's dollars, divided by the price, booked as coin on its coin ledger.
#[test]
fn an_inverse_paper_engine_pays_in_its_settle_coin() {
    let (mut c, _) =
        core(engine("deribit", "BTC-PERPETUAL", true, AccountTier::Paper, 1_000.0), true);
    assert!(c.engine.account.coin_key().is_some(), "an inverse mount keeps coin books");
    tick(&mut c, 0.0001, T1);
    tick(&mut c, 0.0001, T2);
    let coin = 1_000.0 * 0.0001 / MARK; // 0.1 USD of funding, in BTC
    assert!(close(c.engine.account.funding_paid, -coin), "{}", c.engine.account.funding_paid);
    assert!(close(c.engine.account.balance, -coin), "the coin wallet pays it");
}

/// **A restart between two ticks still pays on the restored position**: the funding clock rides the
/// engine's snapshot, so the first observation after the restart finds the settlement the venue rolled
/// past while the node was down and books it on the position the restore put back; and a settlement
/// booked before the restart is never booked again.
#[test]
fn a_restart_between_two_ticks_still_pays_on_the_restored_position() {
    let (mut before, _) = core(engine(VENUE, SYMBOL, false, AccountTier::Paper, 1.0), true);
    tick(&mut before, 0.0001, T1);
    let snap = before.engine.snapshot_state();
    assert_eq!(snap.funding_clocks.len(), 1, "the clock is in the snapshot");

    // The boot restore: the mount builds a fresh paper engine and puts the journaled state back on it.
    let mut fresh = engine(VENUE, SYMBOL, false, AccountTier::Paper, 0.0);
    fresh.restore_runtime_state(&snap);
    hold(&mut fresh, 1.0); // (the same position the snapshot carries, and the venue's mark)
    let (mut after, _) = core(fresh, true);
    tick(&mut after, 0.0002, T2); // the venue rolled while the node was down
    let paid = MARK * 0.0001;
    assert!(
        close(after.engine.account.funding_paid, -paid),
        "{}",
        after.engine.account.funding_paid
    );

    // Booked, then restarted: the next restart books nothing for T1 again.
    let snap = after.engine.snapshot_state();
    let mut again = engine(VENUE, SYMBOL, false, AccountTier::Paper, 1.0);
    again.restore_runtime_state(&snap);
    let (mut third, _) = core(again, true);
    tick(&mut third, 0.0002, T2);
    assert!(close(third.engine.account.funding_paid, -paid), "T1 is not booked twice");
}

/// **A crash between the funding event's journal write and the next `Snapshot` books it once.** The restore
/// starts from the older snapshot (the clock still waiting for `T1`) and refolds the journal tail, which
/// holds the booked funding event; folding it marks the settlement booked, so the venue's roll after the
/// restart books nothing more.
#[test]
fn a_refolded_funding_event_marks_its_settlement_booked() {
    let (mut c, _) = core(engine(VENUE, SYMBOL, false, AccountTier::Paper, 1.0), true);
    tick(&mut c, 0.0001, T1);
    let old_snap = c.engine.snapshot_state();
    let mut restored = engine(VENUE, SYMBOL, false, AccountTier::Paper, 0.0);
    restored.restore_runtime_state(&old_snap);
    hold(&mut restored, 1.0);
    let (mut r, _) = core(restored, true);
    // The journal tail's record, re-applied as the refold re-applies it.
    let paid = MARK * 0.0001;
    r.dispatch(Ingest::Event(Event::Funding(FundingEvent {
        venue: ustr::Ustr::from(VENUE),
        symbol: ustr::Ustr::from(SYMBOL),
        position_side: PositionSide::Both,
        funding_rate: 0.0001,
        amount: -paid,
        mark_price: Some(MARK),
        timestamp: T1,
        route_key: None,
        currency: None,
    })));
    tick(&mut r, 0.0002, T2);
    assert!(close(r.engine.account.funding_paid, -paid), "{}", r.engine.account.funding_paid);
}

/// **A live (or demo) engine is never charged from the rate**: its venue's own settlement events are its
/// funding, and charging from the rate too would count it twice. It keeps no funding clock and arms no
/// timer. And a core with no law handed in (every replay core) books nothing even for a paper engine.
#[test]
fn a_live_engine_is_never_charged_from_the_rate_and_no_law_books_nothing() {
    for tier in [AccountTier::Live, AccountTier::Demo] {
        let (mut c, _) = core(engine(VENUE, SYMBOL, false, tier, 1.0), true);
        tick(&mut c, 0.0001, T1);
        tick(&mut c, 0.0001, T2);
        c.drive_due_timers(T2 + 1_000);
        assert_eq!(c.engine.account.funding_paid, 0.0, "{tier:?}");
        assert!(c.engine.funding_clocks().is_empty(), "{tier:?}: no clock");
        assert!(c.timers.is_empty(), "{tier:?}: no timer");
    }
    let (mut c, _) = core(engine(VENUE, SYMBOL, false, AccountTier::Paper, 1.0), false);
    tick(&mut c, 0.0001, T1);
    tick(&mut c, 0.0001, T2);
    assert_eq!(c.engine.account.funding_paid, 0.0, "no law, no funding");
    assert!(c.engine.funding_clocks().is_empty());
}

/// **A node with no paper engine does no work per funding observation**: its flag is clear, so the
/// observation returns at its first line (no clock, no timer, nothing booked), and the flag follows the engine
/// set: a core holding a paper account sets it, and stopping that account clears it again.
#[test]
fn a_node_with_no_paper_engine_does_no_work_per_funding_tick() {
    let (mut c, _) = core(engine(VENUE, SYMBOL, false, AccountTier::Live, 1.0), true);
    assert!(!c.any_paper_engine, "a live node");
    tick(&mut c, 0.0001, T1);
    tick(&mut c, 0.0001, T2);
    assert!(c.timers.is_empty() && c.engine.funding_clocks().is_empty());

    let alt_key = format!("{VENUE}#ALT");
    let mut alt = engine(VENUE, SYMBOL, false, AccountTier::Paper, 1.0);
    alt.route_key = alt_key.clone();
    let config = CoreConfig {
        clock: Box::new(|| BEFORE),
        funding_law: Some(ContractValue::funding as FundingLaw),
        ..CoreConfig::default()
    };
    let live = engine(VENUE, SYMBOL, false, AccountTier::Live, 1.0);
    let mut c = core_of(live, vec![(1_000.0, alt)], config);
    assert!(c.any_paper_engine, "a paper account is mounted");
    let not_running = vike_execution::ArmingNotRunningView {
        account: vike_model::accounts::account_keys::AccountNamed::of(
            VENUE,
            vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a label"),
        ),
        tier: None,
        reason: "account inactive".to_string(),
        block: Some(vike_model::ArmingBlock::AccountInactive),
    };
    assert_eq!(not_running.route_key(), alt_key);
    c.apply_account_change(AccountChange::StopEngine { not_running }, BEFORE);
    assert!(!c.any_paper_engine, "its one paper account stopped");
    tick(&mut c, 0.0001, T1);
    tick(&mut c, 0.0001, T2);
    assert!(c.timers.is_empty(), "no timer");
    assert!(c.extra_engines[0].1.funding_clocks().is_empty(), "the stopped engine keeps no clock");
}

const T0: i64 = T1 - 8 * 3_600_000;
const T3: i64 = T2 + 8 * 3_600_000;
const T4: i64 = T3 + 8 * 3_600_000;

/// A core over `engine` whose host hands in the venue's stored funding history `stored` (`(ts, rate,
/// mark)`), and how many times it was asked.
fn core_with_history(
    engine: Engine<RecordingClient>,
    stored: Vec<(i64, f64, Option<f64>)>,
) -> (CoreThread<RecordingClient>, Arc<AtomicI64>) {
    let (mut c, _) = core(engine, true);
    let asked = Arc::new(AtomicI64::new(0));
    let count = Arc::clone(&asked);
    c.config.funding_history = Some(Box::new(
        move |venue: &str,
              symbol: &str,
              after: i64,
              before: i64|
              -> Result<Vec<(i64, f64, Option<f64>)>, String> {
            assert_eq!((venue, symbol), (VENUE, SYMBOL));
            count.fetch_add(1, Ordering::Relaxed);
            Ok(stored.iter().copied().filter(|p| p.0 > after && p.0 < before).collect())
        },
    ));
    (c, asked)
}

/// A paper engine holding 1.0 that booked the settlement at `T0` and was told of `T1`, snapshotted (the
/// node then goes down), restored onto a fresh engine with the same position.
fn restored_after_t0() -> Engine<RecordingClient> {
    let (mut before, _) = core(engine(VENUE, SYMBOL, false, AccountTier::Paper, 1.0), true);
    tick(&mut before, 0.0001, T0);
    tick(&mut before, 0.0001, T1); // the venue rolled: T0 booked, T1 told
    assert_eq!(before.engine.funding_clocks()[0].settled_timestamp, T0);
    let mut fresh = engine(VENUE, SYMBOL, false, AccountTier::Paper, 0.0);
    fresh.restore_runtime_state(&before.engine.snapshot_state());
    hold(&mut fresh, 1.0);
    fresh
}

/// **A paper engine down across THREE settlements books all three at the restart, from the venue's
/// stored rates, once.** The first observation after the restart (the venue's next settlement is `T4`)
/// asks the stored history once and books `T1`, `T2` and `T3` on the restored position, each at its
/// stored rate and stored mark (else the engine's), not at the rate stated before the node went down;
/// later observations book nothing more, and the venue's next roll books `T4` as before.
#[test]
fn a_paper_engine_down_across_three_settlements_books_three_from_the_stored_rates_once() {
    let stored = vec![
        (T0, 0.0009, None),               // booked before the restart: never again
        (T1 + 2, 0.0002, Some(60_100.0)), // the store stamps it a few ms off
        (T2, 0.0003, None),
        (T2, 0.0003, None), // the same settlement twice (an older series): booked once
        (T3, -0.0001, Some(59_900.0)),
    ];
    let (mut c, asked) = core_with_history(restored_after_t0(), stored);
    let paid_before = c.engine.account.funding_paid;
    tick(&mut c, 0.0004, T4);
    let owed = 60_100.0 * 0.0002 + MARK * 0.0003 + 59_900.0 * -0.0001;
    let paid = c.engine.account.funding_paid - paid_before;
    assert!(close(paid, -owed), "{paid} vs {}", -owed);
    assert_eq!(asked.load(Ordering::Relaxed), 1, "the history is asked once");
    tick(&mut c, 0.0005, T4);
    tick(&mut c, 0.0005, T4);
    assert!(close(c.engine.account.funding_paid - paid_before, -owed), "booked once");
    assert_eq!(asked.load(Ordering::Relaxed), 1, "and asked once");
    tick(&mut c, 0.0001, T4 + 8 * 3_600_000); // the venue rolls past T4 while the node is up
    let with_t4 = owed + MARK * 0.0005;
    assert!(close(c.engine.account.funding_paid - paid_before, -with_t4), "T4 as before");
}

/// **A second restart after the catch-up books nothing more**: the clock counts every caught-up
/// settlement passed and rides the snapshot.
#[test]
fn a_second_restart_after_the_catch_up_books_nothing_more() {
    let stored = vec![(T1, 0.0002, None), (T2, 0.0003, None), (T3, 0.0001, None)];
    let (mut c, _) = core_with_history(restored_after_t0(), stored.clone());
    tick(&mut c, 0.0004, T4);
    let paid = c.engine.account.funding_paid;
    let mut again = engine(VENUE, SYMBOL, false, AccountTier::Paper, 0.0);
    again.restore_runtime_state(&c.engine.snapshot_state());
    hold(&mut again, 1.0);
    let (mut second, asked) = core_with_history(again, stored);
    tick(&mut second, 0.0004, T4);
    assert!(
        close(second.engine.account.funding_paid, paid),
        "{}",
        second.engine.account.funding_paid
    );
    assert_eq!(asked.load(Ordering::Relaxed), 0, "nothing was missed, nothing is asked");
}

/// **A missed settlement the store lacks books nothing and is named**: the store holds `T1` and `T3`
/// only, so `T2` (inside a spacing twice the eight hours around it) is booked at no rate, never a guessed
/// one, said in one ring line, and not booked later either.
#[test]
fn a_missed_settlement_the_store_lacks_books_nothing_and_warns() {
    let stored = vec![(T1, 0.0002, None), (T3, 0.0001, None)];
    let (mut c, _) = core_with_history(restored_after_t0(), stored);
    let paid_before = c.engine.account.funding_paid;
    tick(&mut c, 0.0004, T4);
    let owed = MARK * (0.0002 + 0.0001);
    assert!(close(c.engine.account.funding_paid - paid_before, -owed), "T1 and T3 only");
    let said: Vec<_> =
        c.recent.iter().filter(|n| n.starts_with("PAPER FUNDING NOT BOOKED")).collect();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains(&format!("[{T2}]")), "T2 is named: {}", said[0]);
    // An empty store names every missed settlement, the one the clock was told of first.
    let (mut empty, _) = core_with_history(restored_after_t0(), Vec::new());
    let paid_before = empty.engine.account.funding_paid;
    tick(&mut empty, 0.0004, T4);
    assert_eq!(empty.engine.account.funding_paid, paid_before, "nothing is guessed");
    assert!(
        empty.recent.iter().any(|n| n.contains(&format!("[{T1}, {T2}, {T3}]"))),
        "{:?}",
        empty.recent
    );
}

/// **A venue that charges funding on another price than its mark states it, and that price is used**
/// (Hyperliquid's oracle).
#[test]
fn a_stated_funding_price_is_charged_on_instead_of_the_mark() {
    let (mut c, _) = core(engine("hyperliquid", "BTC", false, AccountTier::Paper, 1.0), true);
    tick_priced(&mut c, 0.0000125, T1, Some(60_030.0));
    tick_priced(&mut c, 0.0000125, T1 + 3_600_000, Some(60_100.0));
    let paid = 60_030.0 * 0.0000125;
    assert!(close(c.engine.account.funding_paid, -paid), "{}", c.engine.account.funding_paid);
}
