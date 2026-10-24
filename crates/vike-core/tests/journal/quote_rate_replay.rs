//! A journaled QUOTE-CURRENCY RATE replays deterministically (multi-currency ledger, phase 1).
//!
//! The rate rides the journaled `Command::AdmitSymbol` (`SymbolAdmission::quote_rate`), so a replay
//! prices every fill with the rate that PRECEDED it in the journal and needs no venue. This suite
//! pins that on a USD account holding a JPY-quoted symbol: admit it at one rate, open, close a part,
//! REFRESH the rate (a second admission), close the rest. Four shapes, because the replay base (the
//! first `Snap`) can sit on either side of every one of those records: before the first admission
//! (all of it is re-applied from the tail), after the first admission, after the open and the first
//! close, and after the refresh (the base `Snap` already carries the fx table, and `from_snapshot`
//! rebuilds the effective contract value from it).
//!
//! Every number is a dyadic rational (`1/128`, `65/8192`, prices and quantities that are whole or
//! half numbers), so each product the engine forms is exact in binary and the oracle below is
//! compared by `to_bits`, never with a tolerance (decision 0021). The oracle is the formula, not the
//! engine: `qty * (exit - entry) * unit * rate_at_the_closing_fold`.
//!
//! It also pins the journal VERSION STAMPS the new records need (`latest_segment_version`): a
//! journal with no admission stays at the base 18, an ordinary admission raises it to 19, and an
//! admission that names a quote currency (or states a margin rate) raises it to 20, so an older
//! build refuses a segment whose rates it would silently ignore.

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{Command, EngineSnapshot, ExecutionEngine, SymbolAdmission};
use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord};
use vike_model::events::{Event, FillEvent, PositionSide, TradeId};
use vike_model::{ContractValue, SymbolGrid};

use crate::kit::engines::engine_on;
use crate::kit::events::sim_bare_fill;

const JPY_PAIR: &str = "USD_JPY";
/// Account-currency units per ONE yen at the first rate: 128 yen to the dollar.
const R1: f64 = 1.0 / 128.0;
/// The refreshed rate: 65/8192 (about 126.03 yen to the dollar), a 1.6% move, well inside one step.
const R2: f64 = 65.0 / 8192.0;
const AS_OF_1: i64 = 10;
const AS_OF_2: i64 = 20;

/// A USD account on the sim venue: `quote_asset` is set BEFORE the core spawns, so the engine's
/// ledger currency is the dollar and a yen-quoted symbol is a FOREIGN one.
fn usd_engine() -> ExecutionEngine<RecordingClient> {
    let mut engine = engine_on("sim", "BTCUSDT", RecordingClient::default());
    engine.quote_asset = "USD".into();
    engine
}

fn jpy_admission(rate: f64, as_of_ms: i64) -> Command {
    Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: "sim".into(),
        symbol: JPY_PAIR.into(),
        grid: SymbolGrid { tick_size: Some(0.25), lot_size: Some(1.0), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: Some(150.0),
        quote_ccy: Some("JPY".into()),
        quote_rate: Some(rate),
        rate_as_of_ms: Some(as_of_ms),
        im_requirement: None,
    }))
}

/// A bare external fill on the JPY pair (empty coid): it folds into the position and the realized
/// PnL straight from the money lane, so a replay re-derives it from the journaled event alone.
fn jpy_fill(tid: &str, side: i32, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::new(tid).expect("test trade ids are non-empty"),
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: JPY_PAIR.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: Some(px),
        position_side: PositionSide::Both,
    })
}

enum Step {
    Cmd(Command),
    Ev(Event),
}

/// The five journaled records of the scenario, in order: admit at `R1`, open 1000 @ 150, close 400 @
/// 151 (at `R1`), REFRESH to `R2`, close the other 600 @ 152 (at `R2`).
fn scenario() -> Vec<Step> {
    vec![
        Step::Cmd(jpy_admission(R1, AS_OF_1)),
        Step::Ev(jpy_fill("o1", 1, 1000.0, 150.0)),
        Step::Ev(jpy_fill("c1", -1, 400.0, 151.0)),
        Step::Cmd(jpy_admission(R2, AS_OF_2)),
        Step::Ev(jpy_fill("c2", -1, 600.0, 152.0)),
    ]
}

/// The realized PnL of each closing fill, from the FORMULA: `qty * (exit - entry) * unit * rate`,
/// the rate being the one in force when that fill folded. `unit` is the default 1.0.
fn oracle_closed() -> [f64; 2] {
    [400.0 * (151.0 - 150.0) * 1.0 * R1, 600.0 * (152.0 - 150.0) * 1.0 * R2]
}

/// A journaled core over `dir`. The clock hands out `k` for message `k`, so a rate stamped at
/// `AS_OF_*` is never stale against it.
fn journaled(dir: &Path, snapshot_every: u64) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        coid_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every,
        }),
        ..CoreConfig::default()
    }
}

/// Run `steps` through a journaled core, behind `pad` leading BTC fills, and shut it down (the exit
/// `Snap` is the fence target).
fn run(dir: &Path, pad: usize, snapshot_every: u64, steps: Vec<Step>) {
    let handle = spawn_core(usd_engine(), journaled(dir, snapshot_every));
    let sender = handle.event_sender();
    for i in 0..pad {
        sender.blocking_send(sim_bare_fill(&format!("pad{i}"), 1.0, 100.0)).unwrap();
    }
    for step in steps {
        match step {
            Step::Cmd(c) => handle.send_command(c),
            Step::Ev(e) => sender.blocking_send(e).unwrap(),
        }
    }
    handle.shutdown_and_join();
}

/// The first `Snap` of the journal at `dir`: the replay base.
fn base_snap(dir: &Path) -> EngineSnapshot {
    CommandJournal::read_all(dir)
        .unwrap()
        .into_iter()
        .find_map(|r| match r {
            JournalRecord::Snap { mut engines, .. } => engines.pop(),
            _ => None,
        })
        .expect("the journal holds a Snap")
}

/// Replay `dir` and check everything the oracle says about the yen book.
fn assert_replays_to_the_oracle(dir: &Path) -> u64 {
    let out = vike_core::replay::replay_offline(dir).expect("the fence holds");
    assert!(out.records > 0 && out.snaps_compared >= 1);
    let account = &out.engines[0].account;

    let [first, second] = oracle_closed();
    assert_eq!(first.to_bits(), 3.125_f64.to_bits(), "the oracle itself: 400 yen at 1/128");
    assert_eq!(second.to_bits(), 9.521_484_375_f64.to_bits(), "…and 1200 yen at 65/8192");
    assert_eq!(
        account.closed_pnls.iter().map(|p| p.to_bits()).collect::<Vec<_>>(),
        vec![first.to_bits(), second.to_bits()],
        "each close booked at the rate in force when IT folded: the refresh between them never \
         re-priced the first"
    );
    let want = first + second;
    assert_eq!(account.realized_pnl.to_bits(), want.to_bits(), "realized PnL is the oracle's sum");
    assert_ne!(
        account.realized_pnl.to_bits(),
        (1_000.0 * 1.5 * R2).to_bits(),
        "and it is not a single close priced at the final rate"
    );
    assert_eq!(account.fees_paid.to_bits(), 0.0_f64.to_bits());

    let jpy_sizes: Vec<f64> = account
        .positions
        .iter()
        .filter(|((_, s, _), _)| s.as_str() == JPY_PAIR)
        .map(|(_, p)| p.size)
        .collect();
    assert_eq!(jpy_sizes, vec![0.0], "1000 opened, 400 + 600 closed: flat");

    let fx = account.fx.as_ref().expect("the replayed account holds the fx table");
    assert_eq!(fx.account_ccy, "USD");
    assert_eq!(fx.symbols, vec![(JPY_PAIR.to_string(), "JPY".to_string())]);
    assert_eq!(fx.rates.len(), 1);
    assert_eq!(fx.rates[0].ccy, "JPY");
    assert_eq!(fx.rates[0].rate.to_bits(), R2.to_bits(), "the refreshed rate stands");
    assert_eq!(fx.rates[0].as_of_ms, AS_OF_2);
    assert_eq!(fx.rates[0].pending, None);
    out.final_hash
}

fn replays_to_the_oracle(tag: &str, pad: usize, snapshot_every: u64) -> u64 {
    let dir = Scratch::reserved(tag);
    run(&dir, pad, snapshot_every, scenario());
    assert_replays_to_the_oracle(&dir)
}

/// The whole scenario sits AFTER the base `Snap` (four pad fills trip it): every admission and
/// every fill is re-applied by the replay.
#[test]
fn a_rate_admitted_after_the_replay_base_is_re_applied_and_fenced() {
    replays_to_the_oracle("quote-rate-tail", 4, 4);
}

/// The base `Snap` is taken right after the FIRST admission: it holds the fx table at `R1` and no
/// position yet; the open, the close, the refresh and the last close are the tail.
#[test]
fn a_base_taken_after_the_first_admission_carries_the_rate_and_the_tail_refreshes_it() {
    let dir = Scratch::reserved("quote-rate-base-admit");
    run(&dir, 0, 1, scenario());
    let base = base_snap(&dir);
    let fx = base.account.fx.as_ref().expect("the base Snap holds the fx table");
    assert_eq!(fx.rates[0].rate.to_bits(), R1.to_bits());
    assert_eq!(fx.rates[0].as_of_ms, AS_OF_1);
    assert!(base.account.positions.is_empty(), "nothing has filled at the base");
    assert_replays_to_the_oracle(&dir);
}

/// The base `Snap` is taken after the open and the first close: the position left (600) and the
/// rate in force (`R1`) ride the snapshot; the refresh and the last close are the tail, so the
/// closing fill must fold at the rate the TAIL's admission brought.
#[test]
fn a_base_holding_a_rate_and_a_position_is_re_priced_by_a_refresh_in_the_tail() {
    let dir = Scratch::reserved("quote-rate-base-close");
    run(&dir, 0, 3, scenario());
    let base = base_snap(&dir);
    let fx = base.account.fx.as_ref().expect("the base Snap holds the fx table");
    assert_eq!(fx.rates[0].rate.to_bits(), R1.to_bits(), "the base still holds the OLD rate");
    let left = base
        .account
        .positions
        .iter()
        .find(|((_, s, _), _)| s.as_str() == JPY_PAIR)
        .map(|(_, p)| p.size);
    assert_eq!(left, Some(600.0), "the base holds what is left of the position");
    assert_replays_to_the_oracle(&dir);
}

/// The base `Snap` is taken right after the refresh: the admission is BEFORE the base on both
/// counts, and only the last close is replayed, at the rate the snapshot restored.
#[test]
fn an_admission_before_the_replay_base_rides_the_snapshot() {
    let dir = Scratch::reserved("quote-rate-base-refresh");
    run(&dir, 0, 4, scenario());
    let base = base_snap(&dir);
    let fx = base.account.fx.as_ref().expect("the base Snap holds the fx table");
    assert_eq!(fx.rates[0].rate.to_bits(), R2.to_bits(), "the base already holds the NEW rate");
    assert_eq!(fx.rates[0].as_of_ms, AS_OF_2);
    assert_replays_to_the_oracle(&dir);
}

/// Building the same journal twice gives the same state hash, and the hash is the oracle's: the rate
/// is a journaled input, not something a run reads from its surroundings.
#[test]
fn the_same_journal_built_twice_replays_to_the_same_state_hash() {
    let a = replays_to_the_oracle("quote-rate-twice-a", 0, 3);
    let b = replays_to_the_oracle("quote-rate-twice-b", 0, 3);
    assert_eq!(a, b, "two identical runs, two identical hashes");
    let c = replays_to_the_oracle("quote-rate-twice-c", 0, 1);
    assert_eq!(a, c, "and where the base Snap falls does not change what the replay reaches");
}

/// How many `AdmitSymbol` records the journal at `dir` holds.
fn admissions_journaled(dir: &Path) -> usize {
    CommandJournal::read_all(dir)
        .unwrap()
        .into_iter()
        .filter(|r| {
            matches!(
                r,
                JournalRecord::Cmd { msg: vike_exec::Ingest::Command(Command::AdmitSymbol(_)), .. }
            )
        })
        .count()
}

/// A rate that is not a number never reaches the journal. `serde_json` writes a non-finite float as
/// `null`, which a replay reads back as no rate at all: an admission naming a currency would be refused
/// both ways, but one naming NONE would be refused live (`quote-ccy-missing`) and ADMITTED AT PAR by the
/// replay. The core refuses the command before it can be journaled, so the replay never sees it, and the
/// fence (the live exit `Snap` against the re-fold) holds with the symbol it must not have.
#[test]
fn a_rate_that_is_not_a_number_is_refused_and_never_journaled() {
    let dir = Scratch::reserved("quote-rate-nan");
    let with_ccy = jpy_admission(f64::NAN, AS_OF_1);
    // the shape that would replay as an ordinary account-quoted admission of ETHUSDT
    let Command::AdmitSymbol(mut par_lookalike) = jpy_admission(f64::NAN, AS_OF_1) else {
        unreachable!()
    };
    par_lookalike.quote_ccy = None;
    par_lookalike.rate_as_of_ms = None;
    par_lookalike.symbol = "ETHUSDT".into();
    run(
        &dir,
        0,
        1024,
        vec![
            Step::Cmd(with_ccy),
            Step::Cmd(Command::AdmitSymbol(par_lookalike)),
            Step::Cmd(jpy_admission(R1, AS_OF_1)),
            Step::Ev(jpy_fill("o1", 1, 1000.0, 150.0)),
        ],
    );
    assert_eq!(admissions_journaled(&dir), 1, "only the admission with a real rate was journaled");

    let out = vike_core::replay::replay_offline(&dir).expect("the fence holds");
    let engine = &out.engines[0];
    assert_eq!(engine.extra_symbols, [JPY_PAIR], "ETHUSDT was never admitted, live or replayed");
    let fx = engine.account.fx.as_ref().expect("the admission with a rate built the fx table");
    assert_eq!(fx.rates[0].rate.to_bits(), R1.to_bits());
}

// ---- the journal version stamps --------------------------------------------------------------

/// A journal over `steps` (no pad, no cadence snap) and the version its newest segment is stamped.
fn stamp_after(tag: &str, steps: Vec<Step>) -> Option<u32> {
    let dir = Scratch::reserved(tag);
    run(&dir, 0, 1024, steps);
    CommandJournal::latest_segment_version(&dir).expect("the journal directory reads")
}

fn ordinary_admission() -> Command {
    Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: "sim".into(),
        symbol: "ETHUSDT".into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.0001), ..SymbolGrid::default() },
        contract_value: Some(ContractValue::linear(10.0)),
        initial_mark: Some(3000.0),
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
    }))
}

#[test]
fn a_journal_with_no_admission_stays_at_the_base_version() {
    let stamp = stamp_after("quote-rate-v18", vec![Step::Ev(sim_bare_fill("t0", 1.0, 100.0))]);
    assert_eq!(stamp, Some(18));
}

#[test]
fn an_ordinary_admission_raises_the_header_to_19_and_no_further() {
    let stamp = stamp_after("quote-rate-v19", vec![Step::Cmd(ordinary_admission())]);
    assert_eq!(stamp, Some(19));
}

#[test]
fn an_admission_naming_a_quote_currency_raises_the_header_to_20() {
    let stamp = stamp_after("quote-rate-v20", vec![Step::Cmd(jpy_admission(R1, AS_OF_1))]);
    assert_eq!(stamp, Some(20));
}

/// The stamp is decided by the COMMAND, before it is folded: an admission that names a currency but
/// carries no rate is refused by the engine (`no-quote-rate`), and the segment is stamped 20 all the
/// same. That is the direction to err in: the record is in the file, and a v19 reader would parse
/// it and ignore the currency it names.
#[test]
fn a_refused_quoted_admission_still_raises_the_header_to_20() {
    let Command::AdmitSymbol(mut admission) = jpy_admission(R1, AS_OF_1) else { unreachable!() };
    admission.quote_rate = None;
    admission.rate_as_of_ms = None;
    let stamp =
        stamp_after("quote-rate-v20-refused", vec![Step::Cmd(Command::AdmitSymbol(admission))]);
    assert_eq!(stamp, Some(20));
}

/// A per-symbol margin rate is a new meaning of an old record too (a v19 reader would margin the
/// symbol at the engine-wide rate): stating one raises the stamp to 20 with no quote currency.
#[test]
fn an_admission_stating_a_margin_rate_raises_the_header_to_20() {
    let Command::AdmitSymbol(mut admission) = ordinary_admission() else { unreachable!() };
    admission.im_requirement = Some(0.5);
    let stamp =
        stamp_after("quote-rate-v20-margin", vec![Step::Cmd(Command::AdmitSymbol(admission))]);
    assert_eq!(stamp, Some(20));
}
