//! [`targets`] and [`readmit`]: a restart re-tells each engine the symbols it holds beyond its own,
//! before the first reconcile pass, and a venue that cannot answer costs an error line, not a start.

use super::*;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, ExecutionEngine, Outbox, RiskGate, SymbolAdmission};
use vike_model::{OrderRequest, RiskLimits, SymbolGrid};

fn admission(route_key: &str, symbol: &str) -> SymbolAdmission {
    SymbolAdmission {
        route_key: route_key.into(),
        symbol: symbol.into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.001), ..SymbolGrid::default() },
        contract_value: None,
        initial_mark: None,
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
    }
}

fn limit(coid: &str, symbol: &str) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "bybit".into(),
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    }
}

/// What the journal restores after a run that traded ETHUSDT on a BTCUSDT bybit engine: the REAL
/// `snapshot_state`, with nothing edited. It lists the admitted symbol in `extra_symbols` and carries
/// its `on_demand` grid row, which is exactly why a symbol listed there is not thereby "known".
fn restored() -> vike_exec::EngineSnapshot {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTCUSDT",
    );
    engine.admit_symbol(&admission("bybit", "ETHUSDT"));
    // An admitted symbol with no mark takes no opening order: the door seeds one; the fixture does it.
    engine.price_board.set_mark("bybit", "ETHUSDT", 100.0, 1);
    let mut outbox = Outbox::default();
    engine.submit_order(&limit("own", "BTCUSDT"), 1_000, &mut outbox);
    engine.submit_order(&limit("foreign", "ETHUSDT"), 1_000, &mut outbox);
    let snap = engine.snapshot_state();
    assert_eq!(snap.extra_symbols, vec!["ETHUSDT".to_string()], "the real snapshot lists ETH");
    snap
}

fn target(route_key: &str, symbol: &str) -> Target {
    Target { route_key: route_key.into(), venue: "bybit".into(), symbol: symbol.into() }
}

#[test]
fn the_symbols_an_engine_holds_beyond_its_own_are_the_targets() {
    assert_eq!(targets(&[restored()]), vec![target("bybit", "ETHUSDT")]);
}

#[test]
fn a_labelled_engine_is_addressed_by_its_route_key() {
    let mut snap = restored();
    snap.route_key = Some("bybit#ALT".into());
    assert_eq!(targets(&[snap]), vec![target("bybit#ALT", "ETHUSDT")]);
}

#[test]
fn a_declared_leg_and_a_repeat_are_not_targets() {
    // A DECLARED leg is configuration the mount states again at boot: listed in `extra_symbols` with a
    // grid row that is NOT on-demand. (An ADMITTED symbol is listed there too, with an on-demand row —
    // `restored()` is that case, and it is a target.)
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTCUSDT",
    );
    engine.extra_symbols.push("ETHUSDT".into());
    engine.gate.limits.grid_by_symbol.insert(
        "ETHUSDT".into(),
        vike_model::SymbolGrid { tick_size: Some(0.5), lot_size: Some(0.5), ..Default::default() },
    );
    engine.submit_order(&limit("leg", "ETHUSDT"), 1_000, &mut Outbox::default());
    let leg = engine.snapshot_state();
    assert!(!leg.registry.is_empty(), "the fixture holds an order on the leg");
    assert!(
        targets(&[leg]).is_empty(),
        "the mount states a declared leg again; nothing to refresh"
    );
    let again = targets(&[restored(), restored()]);
    assert_eq!(again, vec![target("bybit", "ETHUSDT")], "one admission per account and symbol");
}

/// A terminal order and a flat book hold nothing, yet the symbol an earlier run ADMITTED is still a
/// target: it is restored into the engine with no mark, and only the boot pass subscribes its price (it
/// was NOT a target before, and the CI box's restored flat ETHUSDT was denied `no-reference-price` for ever).
#[test]
fn an_admitted_symbol_is_a_target_though_the_engine_holds_nothing_in_it() {
    let mut snap = restored();
    for (_, order) in &mut snap.registry {
        order.status = vike_model::OrderStatus::Canceled;
    }
    snap.account.positions.clear();
    assert_eq!(targets(&[snap]), vec![target("bybit", "ETHUSDT")]);
}

/// The engine's own symbol and a declared leg stay out even when the engine holds nothing: only an
/// admission (an on-demand row) is a target.
#[test]
fn a_flat_engine_with_no_admission_has_no_target() {
    let mut snap = restored();
    snap.registry.clear();
    snap.account.positions.clear();
    snap.extra_symbols.clear();
    snap.limits.grid_by_symbol.clear();
    assert!(targets(&[snap]).is_empty());
}

struct Facts;

impl SymbolFacts for Facts {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, String> {
        if symbol == "ETHUSDT" {
            Ok(admission(route_key, symbol))
        } else {
            Err(format!("{venue}: no instrument {symbol} in the venue's instrument list"))
        }
    }

    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(Some(100.0))
    }
}

#[test]
fn every_answerable_target_is_re_sent_and_the_rest_are_reported() {
    let mut sent = Vec::new();
    let done = readmit(
        &[target("bybit", "ETHUSDT"), target("bybit", "DELISTEDUSDT")],
        &Facts,
        &Pace::default(),
        |c| sent.push(c),
    );
    assert_eq!(done.sent, 1);
    assert_eq!(done.failed.len(), 1);
    assert!(done.failed[0].2.contains("no instrument DELISTEDUSDT"), "{:?}", done.failed);
    assert!(matches!(&sent[..], [Command::AdmitSymbol(a)] if a.symbol == "ETHUSDT"));
}

/// The point of it: an engine rebuilt from the journal and sent the re-admission is the engine the
/// run had (same admitted symbol, same grid, so the same `state_hash`).
#[test]
fn the_re_admitted_engine_is_the_engine_the_run_had() {
    let live = restored_engine();
    let snap = live.snapshot_state();
    // The REAL boot: a freshly mounted engine (knowing nothing of ETH) takes the journal's state back.
    let mut rebuilt = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTCUSDT",
    );
    rebuilt.restore_runtime_state(&snap);
    assert!(rebuilt.accepts_symbol("ETHUSDT"), "the restore put the admission back");
    let mut sent = Vec::new();
    readmit(&targets(&[snap]), &Facts, &Pace::default(), |c| sent.push(c));
    for command in sent {
        let Command::AdmitSymbol(a) = command else { unreachable!() };
        rebuilt.admit_symbol(&a);
    }
    // The admitted state is what must agree (the order registry is a CANDIDATE list at boot, not
    // restored, so the whole-engine hash is not comparable).
    let (got, live) = (rebuilt.snapshot_state(), live.snapshot_state());
    assert_eq!(got.extra_symbols, live.extra_symbols);
    assert_eq!(got.limits.grid_by_symbol, live.limits.grid_by_symbol);
    assert_eq!((got.account.mult, got.account.inverse), (live.account.mult, live.account.inverse));
}

fn restored_engine() -> ExecutionEngine<RecordingClient> {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTCUSDT",
    );
    engine.admit_symbol(&admission("bybit", "ETHUSDT"));
    // An admitted symbol with no mark takes no opening order: the door seeds one; the fixture does it.
    engine.price_board.set_mark("bybit", "ETHUSDT", 100.0, 1);
    let mut outbox = Outbox::default();
    engine.submit_order(&limit("foreign", "ETHUSDT"), 1_000, &mut outbox);
    engine
}

/// A facts source whose gatewayhub never answered within the boot bound (`LeaseFacts::begin_boot`).
struct TimedOut;

impl SymbolFacts for TimedOut {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(&self, route_key: &str, _: &str, symbol: &str) -> Result<SymbolAdmission, String> {
        Err(format!("gatewayhub: no answer within 60s for `{symbol}` of {route_key}"))
    }

    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Err("never asked: a symbol that was not admitted has no price subscription".into())
    }

    fn admitted(&self, _: &str, _: &str, symbol: &str) {
        panic!("a symbol whose facts timed out is not admitted: {symbol}");
    }
}

/// **A timed-out look admits nothing and drops nothing.** The symbol lands in `failed` (fail closed: no
/// `AdmitSymbol`, no `admitted`, so the daemon's reconcile client is not told it and the restore planner
/// HOLDS its orders), and the engine the journal restored keeps the admission and the grid row it had
/// (what the journal restored on the symbol stays known to the engine, never silently dropped).
#[test]
fn a_timed_out_look_admits_nothing_and_the_restored_symbol_stays_as_the_journal_left_it() {
    let snap = restored();
    let mut rebuilt = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTCUSDT",
    );
    rebuilt.restore_runtime_state(&snap);
    let mut sent = Vec::new();
    let done = readmit(&targets(std::slice::from_ref(&snap)), &TimedOut, &Pace::default(), |c| {
        sent.push(c)
    });
    assert!(sent.is_empty(), "no admission is sent for a symbol whose facts did not come");
    assert_eq!((done.sent, done.admitted.len()), (0, 0));
    assert_eq!(done.failed.len(), 1, "{:?}", done.failed);
    assert_eq!((done.failed[0].0.as_str(), done.failed[0].1.as_str()), ("bybit", "ETHUSDT"));
    assert!(done.failed[0].2.contains("no answer within"), "{:?}", done.failed);

    let after = rebuilt.snapshot_state();
    assert!(rebuilt.accepts_symbol("ETHUSDT"), "the restored admission stays");
    assert_eq!(after.extra_symbols, snap.extra_symbols);
    assert_eq!(after.limits.grid_by_symbol, snap.limits.grid_by_symbol);
}

// ---------------------------------------------------------------------------------------------
// a transient miss at boot heals without a restart
// ---------------------------------------------------------------------------------------------

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::{Duration, Instant};

/// A lease that stalls its first `look_misses` facts looks and refuses its first `price_misses` price
/// asks (a gatewayhub that was slow once), then answers; it records what it was told and priced, and
/// keeps the engine generation the test moves (`stop`/`start` of the account's engine).
#[derive(Default)]
struct Stalls {
    look_misses: AtomicUsize,
    price_misses: AtomicUsize,
    /// The first `commit_misses` calls of `tell_daemon` fail (a stalled commit); `commits` counts them all.
    commit_misses: AtomicUsize,
    commits: AtomicUsize,
    generation: AtomicUsize,
    /// A start or stop of the engine that lands while the look is in flight.
    bump_during_look: std::sync::atomic::AtomicBool,
    told: std::sync::Mutex<Vec<String>>,
    priced: std::sync::Mutex<Vec<String>>,
}

fn spend(counter: &AtomicUsize) -> bool {
    counter.load(SeqCst) > 0 && counter.fetch_sub(1, SeqCst) > 0
}

impl SymbolFacts for Stalls {
    fn admission(&self, rk: &str, _: &str, symbol: &str) -> Result<SymbolAdmission, String> {
        if self.bump_during_look.load(SeqCst) {
            self.generation.fetch_add(1, SeqCst);
        }
        if spend(&self.look_misses) {
            Err(format!("gatewayhub: no answer within 60s for `{symbol}` of {rk}"))
        } else {
            Ok(admission(rk, symbol))
        }
    }
    fn price(&self, _: &str, _: &str, symbol: &str, _: bool) -> Result<Option<f64>, String> {
        if spend(&self.price_misses) {
            return Err("boot: the lease did not answer".into());
        }
        self.priced.lock().unwrap().push(symbol.to_string());
        Ok(Some(100.0))
    }
    fn admitted(&self, _: &str, _: &str, symbol: &str) {
        self.told.lock().unwrap().push(symbol.to_string());
    }
    fn tell_daemon(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
        self.commits.fetch_add(1, SeqCst);
        if spend(&self.commit_misses) {
            Err("gatewayhub: the commit did not answer".into())
        } else {
            Ok(())
        }
    }
    fn generation(&self, _: &str) -> u64 {
        self.generation.load(SeqCst) as u64
    }
}

fn missed(symbol: &str, told: bool, generation: u64) -> Missed {
    Missed { target: target("bybit", symbol), told, generation }
}

fn far() -> Instant {
    Instant::now() + Duration::from_secs(3600)
}

/// **A stalled look at boot is retried, and the restored symbol is neither dropped nor left untold.**
/// The boot's pass misses ETHUSDT (its restored resting order is HELD by the core's restore planner
/// while the daemon was not told the symbol); the engine keeps the admission and the grid the journal
/// restored, and the retry, at the runtime bound, sends the `AdmitSymbol`, tells the daemon and opens
/// the price pump: nothing waits for a restart.
#[test]
fn a_stalled_look_at_boot_is_retried_and_the_symbol_is_admitted_told_and_priced() {
    let snap = restored();
    let mut rebuilt = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTCUSDT",
    );
    rebuilt.restore_runtime_state(&snap);
    let facts = Stalls { look_misses: AtomicUsize::new(1), ..Stalls::default() };
    let mut sent = Vec::new();

    let (done, left_at_boot) =
        readmit_and_price(&targets(std::slice::from_ref(&snap)), &facts, &Pace::default(), |c| {
            sent.push(c);
        });
    assert_eq!(done.sent, 0);
    assert_eq!(left_at_boot, vec![missed("ETHUSDT", false, 0)]);
    assert!(
        sent.is_empty() && facts.told.lock().unwrap().is_empty(),
        "nothing admitted, nothing told"
    );
    assert!(
        rebuilt.accepts_symbol("ETHUSDT"),
        "the restored admission stays while the symbol is missed"
    );

    let mut waits = Vec::new();
    let left = retry_missed(
        left_at_boot,
        &[Duration::from_secs(5), Duration::from_secs(15)],
        far(),
        |pause| {
            waits.push(pause);
            true
        },
        |round| Some(retry_round(round, &facts, |c| sent.push(c))),
    );
    assert!(left.is_empty(), "{left:?}");
    assert_eq!(waits, [Duration::from_secs(5)], "healed on the first retry, no more asked");
    assert!(matches!(&sent[..], [Command::AdmitSymbol(a)] if a.symbol == "ETHUSDT"));
    assert_eq!(*facts.told.lock().unwrap(), ["ETHUSDT"], "the daemon was told");
    assert_eq!(*facts.priced.lock().unwrap(), ["ETHUSDT"], "the pump was opened");
}

/// An admitted symbol whose price feed failed is missing as well, and the retry sends NO second
/// `AdmitSymbol`; but it RE-COMMITS to the daemon (a stalled commit is exactly how the price failed),
/// and the symbol counts as healed only once the commit took AND the price came: a probe lookup alone
/// would have left the reconcile client untold and the restored orders HELD, with nothing said.
#[test]
fn an_admitted_symbol_with_no_price_feed_is_retried_for_its_commit_and_its_price_alone() {
    let facts = Stalls {
        price_misses: AtomicUsize::new(1),
        commit_misses: AtomicUsize::new(1),
        ..Stalls::default()
    };
    let wanted = [target("bybit", "ETHUSDT")];
    let mut sent = Vec::new();
    let (done, left_at_boot) =
        readmit_and_price(&wanted, &facts, &Pace::default(), |c| sent.push(c));
    assert_eq!((done.sent, left_at_boot.clone()), (1, vec![missed("ETHUSDT", true, 0)]));
    assert_eq!(facts.commits.load(SeqCst), 0, "the boot's commit is `admitted`'s, swallowed");

    let round_one = retry_round(&left_at_boot, &facts, |c| sent.push(c));
    assert_eq!(round_one, left_at_boot, "the commit stalled again: still missing, still HELD");
    assert_eq!(facts.commits.load(SeqCst), 1, "the retry commits again");
    assert!(facts.priced.lock().unwrap().is_empty(), "no price counted before the commit took");

    let round_two = retry_round(&round_one, &facts, |c| sent.push(c));
    assert!(round_two.is_empty(), "{round_two:?}");
    assert_eq!(facts.commits.load(SeqCst), 2);
    assert_eq!(*facts.priced.lock().unwrap(), ["ETHUSDT"]);
    assert_eq!(sent.len(), 1, "the boot's admission is the only one: the retry sends none");
}

/// The retry is bounded: it asks once per pause, gives up with what is still missing, and ends at once
/// when the process is stopping, the facts source is gone, or the schedule's deadline has passed.
#[test]
fn the_retry_is_bounded_and_ends_with_the_process() {
    let pauses = [Duration::ZERO; 3];
    let missing = vec![missed("ETHUSDT", false, 0)];

    let mut asked = 0;
    let left = retry_missed(
        missing.clone(),
        &pauses,
        far(),
        |_| true,
        |round| {
            asked += 1;
            Some(round.to_vec())
        },
    );
    assert_eq!((asked, left), (3, missing.clone()), "one ask per pause, then it gives up");

    let mut asked = 0;
    let left = retry_missed(
        missing.clone(),
        &pauses,
        far(),
        |_| false,
        |_| {
            asked += 1;
            None
        },
    );
    assert_eq!((asked, left), (0, missing.clone()), "a stopping process asks nothing");

    let left = retry_missed(missing.clone(), &pauses, far(), |_| true, |_| None);
    assert_eq!(left, missing, "a gone facts source ends it");

    let past = Instant::now();
    let left = retry_missed(missing.clone(), &pauses, past, |_| true, |_| panic!("past deadline"));
    assert_eq!(left, missing, "the deadline ends the schedule");
    assert!(
        retry_missed(Vec::new(), &pauses, far(), |_| panic!("nothing to wait for"), |_| None)
            .is_empty()
    );
}

/// **A retry that fires after the account's engine was stopped, or stopped and started anew, touches
/// nothing**: no `AdmitSymbol` (the new engine, a paper one or a live one without a ceiling, would take a
/// symbol nobody asked it for, past the door's gates), nobody told (no `admitted` entry to leak a
/// pump), no price subscription. Both the look-first and the price-only shapes.
#[test]
fn a_retry_after_the_engine_changed_sends_nothing_tells_nothing_and_opens_no_pump() {
    for (told, bumps) in [(false, 1), (false, 2), (true, 1), (true, 2)] {
        let facts = Stalls::default();
        let work = [missed("ETHUSDT", told, 0)];
        facts.generation.fetch_add(bumps, SeqCst); // stop (1), then start anew (2)
        let mut sent = Vec::new();
        let left = retry_round(&work, &facts, |c| sent.push(c));
        assert!(left.is_empty(), "the symbol is dropped, not retried: {left:?}");
        assert!(sent.is_empty(), "told={told} bumps={bumps}: nothing sent");
        assert!(facts.told.lock().unwrap().is_empty(), "nobody told");
        assert!(facts.priced.lock().unwrap().is_empty(), "no pump");
    }
}

/// The engine changes while the look is in flight (the check immediately before the send): the look's
/// answer is thrown away.
#[test]
fn an_engine_change_during_the_look_cancels_the_send() {
    let facts = Stalls { bump_during_look: true.into(), ..Stalls::default() };
    let mut sent = Vec::new();
    let left = retry_round(&[missed("ETHUSDT", false, 0)], &facts, |c| sent.push(c));
    assert!(left.is_empty() && sent.is_empty());
    assert!(facts.told.lock().unwrap().is_empty() && facts.priced.lock().unwrap().is_empty());
}

/// A `drive` round that finds the symbol still missing, notes whether it was asked LOUD in `asked`, and
/// raises `stop` once `stop_at` rounds have run (the tail has no end of its own, so a test ends it).
fn still_missing<'a>(
    asked: &'a std::cell::RefCell<Vec<bool>>,
    stop: &'a std::sync::atomic::AtomicBool,
    stop_at: usize,
) -> impl FnMut(
    &crate::remote_backend::facts::LeaseFacts,
    &[Missed],
    bool,
) -> (Vec<Missed>, Option<String>)
+ 'a {
    move |_, round, loud| {
        asked.borrow_mut().push(loud);
        if asked.borrow().len() >= stop_at {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        (round.to_vec(), None)
    }
}

/// **The retry loop runs no round when the process is stopping or the registry is gone** (`drive`, the
/// thread's body, driven here on the test thread: no spawn, no race). The rounds are COUNTED, so a loop
/// that ignored the stop flag or the `Weak` would fail: with the registry alive and the flag down, the
/// same call runs a round per pause, the three fast ones LOUD and then the slow tail's, QUIET, until the
/// test raises the flag (the tail has no end of its own).
#[test]
fn the_retry_loop_runs_no_round_when_stopping_or_when_the_registry_is_gone() {
    use crate::remote_backend::facts::LeaseFacts;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    let pauses = [Duration::ZERO; 3];
    let work = || vec![missed("ETHUSDT", false, 0)];
    let asked = std::cell::RefCell::new(Vec::new());

    let facts = Arc::new(LeaseFacts::default());
    let stop = AtomicBool::new(false);
    let left = drive(
        work(),
        &Arc::downgrade(&facts),
        &stop,
        &pauses,
        Duration::ZERO,
        far(),
        still_missing(&asked, &stop, 5),
    );
    assert_eq!(left, work());
    assert_eq!(
        *asked.borrow(),
        [true, true, true, false, false],
        "control: a live registry and a quiet process run every fast round loud, then the tail's \
         rounds quiet, until the flag goes up"
    );

    asked.borrow_mut().clear();
    let stop = AtomicBool::new(true);
    let left = drive(
        work(),
        &Arc::downgrade(&facts),
        &stop,
        &pauses,
        Duration::ZERO,
        far(),
        still_missing(&asked, &stop, usize::MAX),
    );
    assert_eq!((asked.borrow().len(), left), (0, work()), "the stop flag is up: not one round");

    let gone = Arc::new(LeaseFacts::default());
    let weak = Arc::downgrade(&gone);
    drop(gone);
    // A tail pause of an hour: a loop that waited it out before noticing the dropped registry would still
    // be in that wait when the helper thread raises the flag, two seconds on (and `drive` then ends on it).
    let (stop, done) = (AtomicBool::new(false), AtomicBool::new(false));
    let (left, took) = std::thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..40 {
                if done.load(Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            stop.store(true, Relaxed);
        });
        let began = Instant::now();
        let left = drive(
            work(),
            &weak,
            &stop,
            &pauses,
            Duration::from_secs(3600),
            far(),
            still_missing(&asked, &stop, usize::MAX),
        );
        let took = began.elapsed();
        done.store(true, Relaxed);
        (left, took)
    });
    assert_eq!((asked.borrow().len(), left), (0, work()), "the registry is dropped: not one round");
    assert!(
        took < Duration::from_secs(1),
        "a dropped registry ends it at once, not after the tail's pause: {took:?}"
    );
}

/// **The slow tail** (`retry_tail`), driven with a fake wait that records the pauses it was asked for (the
/// clock: 14 pauses of ten minutes is two hours twenty of simulated time, in no real time) and a fake
/// attempt that counts rounds. The symbol stays missing for 13 rounds (a closed market) and heals on the
/// 14th: every round was asked after the SLOW pause, the log has NO error (the one ERROR belongs to the end
/// of the fast schedule), exactly one WARN per hour of rounds (the 6th and the 12th) naming the symbol and
/// the reason, and the heal's own line.
#[test]
fn the_slow_tail_asks_every_ten_minutes_says_one_warn_an_hour_and_stops_when_healed() {
    let mut waits = Vec::new();
    let mut asked = 0;
    let (left, events) = vike_log::capture::captured(|| {
        retry_tail(
            vec![missed("ETHUSDT", false, 0)],
            TAIL_PAUSE,
            |pause| {
                waits.push(pause);
                true
            },
            |round| {
                asked += 1;
                Some(if asked < 14 {
                    (round.to_vec(), Some("the market is closed".to_string()))
                } else {
                    (Vec::new(), None)
                })
            },
        )
    });
    assert!(left.is_empty(), "{left:?}");
    assert_eq!(
        waits, [TAIL_PAUSE; 14],
        "one slow wait before each of the 14 rounds, none after the heal"
    );
    assert!(waits.iter().sum::<Duration>() >= Duration::from_secs(2 * 3600), "simulated two hours");
    assert!(at(&events, Level::ERROR).is_empty(), "a quiet tail: {events:?}");
    let warns = at(&events, Level::WARN);
    let hourly: Vec<_> =
        warns.iter().filter(|e| e.message.contains("still not re-admitted after")).collect();
    assert_eq!(hourly.len(), 2, "at most one line an hour (rounds 6 and 12): {events:?}");
    assert!(
        hourly[0].message.contains("after 6 slow retries")
            && hourly[1].message.contains("after 12 slow retries")
            && hourly.iter().all(|e| e.message.contains("bybit ETHUSDT")
                && e.message.contains("the market is closed")),
        "{hourly:?}"
    );
    assert_eq!(
        warns
            .iter()
            .filter(|e| e.message.contains("re-admitted symbols the boot had missed"))
            .count(),
        1,
        "the heal is said once: {events:?}"
    );
}

/// The tail ends with the process, with the facts source and with the work: a stop in its wait, an attempt
/// that answers `None` (the registry is gone) and nothing missing at the start each end it without another
/// round, and it returns what was left.
#[test]
fn the_slow_tail_ends_on_stop_on_a_gone_registry_and_when_nothing_is_missing() {
    let missing = vec![missed("ETHUSDT", false, 0)];

    let (mut waits, mut asked) = (0, 0);
    let left = retry_tail(
        missing.clone(),
        TAIL_PAUSE,
        |_| {
            waits += 1;
            waits <= 3 // the process stops during the fourth wait
        },
        |round| {
            asked += 1;
            Some((round.to_vec(), None))
        },
    );
    assert_eq!((waits, asked, left), (4, 3, missing.clone()), "a stop in the wait ends it");

    let mut asked = 0;
    let left = retry_tail(
        missing.clone(),
        TAIL_PAUSE,
        |_| true,
        |_| {
            asked += 1;
            None
        },
    );
    assert_eq!((asked, left), (1, missing), "a gone facts source ends it");

    assert!(
        retry_tail(
            Vec::new(),
            TAIL_PAUSE,
            |_| panic!("nothing to wait for"),
            |_| panic!("no round")
        )
        .is_empty()
    );
}

/// **The drive carries the one ERROR through to the tail and keeps the tail quiet**: two fast rounds, then
/// six tail rounds with the test's stop going up in the sixth. Exactly one ERROR in the whole run ("STILL not
/// re-admitted", at the end of the fast schedule), one hourly WARN after the sixth slow round, and the
/// rounds' own per-symbol lines are the round function's business (a quiet round says them at debug).
#[test]
fn the_drive_says_one_error_at_the_end_of_the_fast_schedule_and_one_warn_after_six_tail_rounds() {
    use crate::remote_backend::facts::LeaseFacts;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    let facts = Arc::new(LeaseFacts::default());
    let stop = AtomicBool::new(false);
    let mut rounds = 0;
    let (left, events) = vike_log::capture::captured(|| {
        drive(
            vec![missed("ETHUSDT", false, 0)],
            &Arc::downgrade(&facts),
            &stop,
            &[Duration::ZERO; 2],
            Duration::ZERO,
            far(),
            |_, round, _| {
                rounds += 1;
                if rounds == 8 {
                    stop.store(true, Relaxed);
                }
                (round.to_vec(), Some("no price yet".to_string()))
            },
        )
    });
    assert_eq!((rounds, left), (8, vec![missed("ETHUSDT", false, 0)]));
    let said = |level: Level, what: &str| {
        at(&events, level).iter().filter(|e| e.message.contains(what)).count()
    };
    assert_eq!(at(&events, Level::ERROR).len(), 1, "{events:?}");
    assert_eq!(said(Level::ERROR, "STILL not re-admitted"), 1, "{events:?}");
    assert_eq!(said(Level::WARN, "still not re-admitted after 6 slow retries"), 1, "{events:?}");
    assert_eq!(at(&events, Level::WARN).len(), 1, "{events:?}");
}

/// **The real thread outlives the fast schedule and ends promptly on the process's stop.** `spawn_retry` on
/// the real registry with no lease for the symbol: it stays missing through the one fast round, and the
/// thread must still be there afterwards, in the tail's ten-minute wait (before the tail it had returned and
/// said "STILL not re-admitted" for good); raising the stop flag ends it within the wait's 250 ms slice and it
/// hands back what is still missing.
#[test]
fn the_retry_thread_outlives_the_fast_schedule_and_ends_promptly_on_stop() {
    use crate::remote_backend::facts::LeaseFacts;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    static ONE_PAUSE: [Duration; 1] = [Duration::ZERO];
    let facts = Arc::new(LeaseFacts::default());
    let stop = Arc::new(AtomicBool::new(false));
    let handle = spawn_retry(
        vec![missed("ETHUSDT", false, 0)],
        &facts,
        Arc::clone(&stop),
        &ONE_PAUSE,
        |_| {},
    )
    .expect("spawn");
    std::thread::sleep(Duration::from_millis(500));
    assert!(!handle.is_finished(), "the tail keeps the thread alive after the fast round");
    let began = Instant::now();
    stop.store(true, Relaxed);
    let left = handle.join().expect("the thread");
    assert_eq!(left, vec![missed("ETHUSDT", false, 0)], "nothing healed: no lease exists");
    assert!(
        began.elapsed() < Duration::from_secs(3),
        "ended on the stop flag: {:?}",
        began.elapsed()
    );
}

/// **The thread** (`spawn_retry`) on the REAL registry: an account whose engine was stopped before the
/// retry fired has its symbol dropped and nothing goes through the command door; the thread is joined.
#[test]
fn the_retry_thread_drops_a_symbol_whose_engine_generation_moved_and_sends_nothing() {
    use crate::remote_backend::facts::LeaseFacts;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    static ONE_PAUSE: [Duration; 1] = [Duration::ZERO];
    let (tx, rx) = std::sync::mpsc::channel::<Command>();
    let facts = Arc::new(LeaseFacts::default());
    facts.engine_stopped("bybit");
    let left = spawn_retry(
        vec![missed("ETHUSDT", false, 0)],
        &facts,
        Arc::new(AtomicBool::new(false)),
        &ONE_PAUSE,
        move |c| drop(tx.send(c)),
    )
    .expect("spawn")
    .join()
    .expect("the thread");
    assert!(left.is_empty(), "{left:?}");
    assert!(rx.try_recv().is_err(), "nothing went through the command door");
}

// ---------------------------------------------------------------------------------------------
// hyperliquid: a case-sensitive coin, and a start with the journal off
// ---------------------------------------------------------------------------------------------

/// What the journal restores after a run that traded `kPEPE` on a `BTC` hyperliquid engine.
fn restored_hyperliquid() -> vike_exec::EngineSnapshot {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "hyperliquid", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "hyperliquid",
        "BTC",
    );
    engine.admit_symbol(&admission("hyperliquid", "kPEPE"));
    // As `restored_engine` above: an admitted symbol with no mark takes no opening order.
    engine.price_board.set_mark("hyperliquid", "kPEPE", 100.0, 1);
    let mut order = limit("foreign", "kPEPE");
    order.venue = "hyperliquid".into();
    engine.submit_order(&order, 1_000, &mut Outbox::default());
    engine.snapshot_state()
}

/// **The boot re-admission keeps a coin's case**: `kPEPE` is re-asked as `kPEPE`, never `KPEPE`, so
/// a restart finds the instrument row it admitted from.
#[test]
fn a_restored_hyperliquid_coin_is_re_admitted_in_the_case_the_venue_lists_it() {
    let got = targets(&[restored_hyperliquid()]);
    assert_eq!(
        got,
        vec![Target {
            route_key: "hyperliquid".into(),
            venue: "hyperliquid".into(),
            symbol: "kPEPE".into()
        }]
    );
    struct Venue;
    impl SymbolFacts for Venue {
        fn generation(&self, _: &str) -> u64 {
            0
        }
        fn admission(
            &self,
            rk: &str,
            venue: &str,
            symbol: &str,
        ) -> Result<SymbolAdmission, String> {
            if symbol == "kPEPE" {
                Ok(admission(rk, symbol))
            } else {
                Err(format!("{venue}: no instrument {symbol} in the venue's instrument list"))
            }
        }
        fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
            Ok(None)
        }
    }
    let mut sent = Vec::new();
    let done = readmit(&got, &Venue, &Pace::default(), |c| sent.push(c));
    assert_eq!((done.sent, done.failed.len()), (1, 0));
    assert!(matches!(&sent[..], [Command::AdmitSymbol(a)] if a.symbol == "kPEPE"));
}

/// **With the journal off (the the CI box default) there is nothing restored, so nothing is asked and
/// nothing sent**: no engine, no target, no facts lookup. A foreign position the venue holds is the
/// reconcile pass's to report; closing it is a reduce-only order, which the door admits first.
#[test]
fn with_no_journal_nothing_is_re_admitted_and_nothing_is_asked() {
    assert!(targets(&[]).is_empty());
    struct Asked(std::sync::atomic::AtomicUsize);
    impl SymbolFacts for Asked {
        fn generation(&self, _: &str) -> u64 {
            0
        }
        fn admission(&self, _: &str, _: &str, _: &str) -> Result<SymbolAdmission, String> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err("asked".into())
        }
        fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
            Ok(None)
        }
    }
    let asked = Asked(Default::default());
    let mut sent = 0;
    let done = readmit(&targets(&[]), &asked, &Pace::default(), |_| sent += 1);
    assert_eq!((done, sent), (Readmitted::default(), 0));
    assert_eq!(asked.0.load(std::sync::atomic::Ordering::SeqCst), 0);
}

// ---- the pass reports itself and ends on its budget (v0.1.71 review, finding 6) ----

thread_local! {
    /// The milliseconds this test's thread has "spent" (the pass runs on the test's own thread).
    static SPENT_MS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// A clock the facts below move: a base instant plus the milliseconds this thread has "spent".
fn fake_now() -> Instant {
    static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *BASE.get_or_init(Instant::now) + Duration::from_millis(SPENT_MS.with(std::cell::Cell::get))
}

/// The wait of a test pass: it moves the fake clock and sleeps nothing.
fn fake_sleep(pause: Duration) {
    SPENT_MS.with(|spent| spent.set(spent.get() + pause.as_millis() as u64));
}

/// Facts that answer the gatewayhub's per-venue lookup bucket (`burst` tokens, one back per 500 ms of the
/// fake clock): a spent bucket answers the transient `FACTS_BUSY` wording with no admission.
struct Bucketed {
    burst: f64,
    state: std::sync::Mutex<(f64, u64)>,
    asks: AtomicUsize,
}

impl Bucketed {
    fn new(burst: u32) -> Self {
        Bucketed {
            burst: f64::from(burst),
            state: std::sync::Mutex::new((f64::from(burst), 0)),
            asks: AtomicUsize::new(0),
        }
    }
}

impl SymbolFacts for Bucketed {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, String> {
        self.asks.fetch_add(1, SeqCst);
        let now = SPENT_MS.with(std::cell::Cell::get);
        let mut state = self.state.lock().unwrap();
        state.0 = (state.0 + (now - state.1) as f64 / 500.0).min(self.burst);
        state.1 = now;
        if state.0 < 1.0 {
            return Err(format!(
                "{venue}: {} (per-symbol lookups are rate-limited to protect the venue's request \
                 budget; retry shortly)",
                vike_gatewayhub_client::proto::FACTS_BUSY
            ));
        }
        state.0 -= 1.0;
        Ok(admission(route_key, symbol))
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(Some(100.0))
    }
}

fn many(n: usize) -> Vec<Target> {
    (0..n).map(|i| target("bybit", &format!("S{i}USDT"))).collect()
}

/// **A restore of more symbols than the bucket's burst is paced to the refill, not failed**: 25 symbols
/// against a burst of 10 admit all 25 in one pass (15 waits of 500 ms on the fake clock), none held back for
/// the retry thread.
#[test]
fn a_restore_past_the_lookup_burst_is_paced_to_the_refill_not_failed() {
    SPENT_MS.with(|spent| spent.set(0));
    let facts = Bucketed::new(10);
    let quiet = |_: &str| {};
    let pace = Pace::with_clock(Duration::from_secs(60), &quiet, fake_now, fake_sleep);
    let mut sent = Vec::new();
    let (done, missed) = readmit_and_price(&many(25), &facts, &pace, |c| sent.push(c));
    assert_eq!((done.sent, done.failed.len(), missed.len()), (25, 0, 0), "{:?}", done.failed);
    assert_eq!(sent.len(), 25);
    assert_eq!(SPENT_MS.with(std::cell::Cell::get), 15 * 500, "paced to the refill, no longer");
}

/// A bucket that stays spent is waited on [`MAX_PACED_ASKS`] times for ONE symbol, then the symbol is a
/// miss for the retry thread (not an endless wait), and the retry heals it once the bucket has refilled.
#[test]
fn a_bucket_that_stays_spent_ends_in_a_miss_the_retry_heals() {
    SPENT_MS.with(|spent| spent.set(0));
    let facts = Bucketed::new(0);
    let quiet = |_: &str| {};
    let pace = Pace::with_clock(Duration::from_secs(60), &quiet, fake_now, fake_sleep);
    let (done, missed) = readmit_and_price(&many(1), &facts, &pace, |_| {});
    assert_eq!(done.sent, 0);
    assert_eq!(missed.len(), 1);
    assert_eq!(facts.asks.load(SeqCst), 1 + MAX_PACED_ASKS as usize);
    assert!(done.failed[0].2.contains(vike_gatewayhub_client::proto::FACTS_BUSY));

    // Time passes (the retry thread's pause), one token is back: the retry admits it.
    fake_sleep(Duration::from_secs(5));
    let mut sent = Vec::new();
    let left = retry_round(&missed, &Bucketed::new(10), |c| sent.push(c));
    assert!(left.is_empty() && sent.len() == 1, "{left:?}");
}

/// A pass whose budget is gone does not pause for a busy gatewayhub: it ends as before.
#[test]
fn a_spent_budget_does_not_wait_for_a_busy_gatewayhub() {
    SPENT_MS.with(|spent| spent.set(0));
    let facts = Bucketed::new(0);
    let quiet = |_: &str| {};
    let pace = Pace::with_clock(Duration::ZERO, &quiet, fake_now, fake_sleep);
    let (done, _) = readmit_and_price(&many(2), &facts, &pace, |_| {});
    assert_eq!((done.sent, facts.asks.load(SeqCst)), (0, 0));
    assert!(done.failed.iter().all(|(_, _, why)| why == OVER_BUDGET));
}

/// Facts whose every look "takes" `look_ms` on the fake clock and answers.
struct Slow {
    look_ms: u64,
}

impl SymbolFacts for Slow {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(&self, route_key: &str, _: &str, symbol: &str) -> Result<SymbolAdmission, String> {
        SPENT_MS.with(|spent| spent.set(spent.get() + self.look_ms));
        Ok(admission(route_key, symbol))
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(Some(100.0))
    }
}

fn five() -> Vec<Target> {
    ["AUSDT", "BUSDT", "CUSDT", "DUSDT", "EUSDT"].map(|s| target("bybit", s)).to_vec()
}

/// Every symbol is announced BEFORE its round trip as `readmitting <venue> <k>/<n>` (then `pricing ...`),
/// so the `starting` stream moves with the pass; and a pass inside its budget changes nothing else.
#[test]
fn each_symbol_is_announced_before_its_round_trip() {
    // through the REAL reporter the daemon prints `{"kind":"starting"}` lines from
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = std::sync::Arc::clone(&lines);
    let progress = crate::server::starting::StartProgress::new()
        .with_reporter(move |text| sink.lock().unwrap().push(text.to_string()));
    let report = |text: &str| progress.phase(text);
    let pace = Pace::new(Duration::from_secs(3600), &report);
    let wanted = [target("bybit", "ETHUSDT"), target("bybit#B", "ETHUSDT")];
    let mut sent = Vec::new();
    let (done, missed) = readmit_and_price(&wanted, &Facts, &pace, |c| sent.push(c));
    assert_eq!((done.sent, missed.len()), (2, 0));
    assert_eq!(
        *lines.lock().unwrap(),
        [
            "readmitting bybit 1/2",
            "readmitting bybit 2/2",
            "pricing bybit 1/2",
            "pricing bybit 2/2"
        ]
    );
}

/// **The pass ends on its budget** (a fake clock: each look costs 20 s of a 45 s budget): the symbols
/// not reached are NOT asked, come back as failed with [`OVER_BUDGET`], and `readmit_and_price` turns them
/// into misses the retry thread admits; the symbols admitted but not yet priced are misses too (`told`).
#[test]
fn the_pass_ends_on_its_budget_and_hands_the_rest_to_the_retry() {
    let facts = Slow { look_ms: 20_000 };
    let quiet = |_: &str| {};
    let pace = Pace::with_clock(Duration::from_secs(45), &quiet, fake_now, fake_sleep);

    let mut sent = Vec::new();
    let (done, missed) = readmit_and_price(&five(), &facts, &pace, |c| sent.push(c));
    // looks at 0 s, 20 s, 40 s; the 60 s mark is past the budget
    assert_eq!(done.sent, 3);
    assert_eq!(sent.len(), 3);
    assert_eq!(done.failed.len(), 2);
    assert!(done.failed.iter().all(|(_, _, why)| why == OVER_BUDGET), "{:?}", done.failed);
    assert_eq!(
        missed,
        [
            missed_one("DUSDT", false),
            missed_one("EUSDT", false),
            // admitted, but the budget was gone before their price subscription
            missed_one("AUSDT", true),
            missed_one("BUSDT", true),
            missed_one("CUSDT", true),
        ]
    );
}

fn missed_one(symbol: &str, told: bool) -> Missed {
    missed(symbol, told, 0)
}

/// A pass with no budget (the default, and every test above) asks everything however long it takes.
#[test]
fn the_default_pace_has_no_budget() {
    let (done, missed) =
        readmit_and_price(&five(), &Slow { look_ms: 3_600_000 }, &Pace::default(), |_| {});
    assert_eq!((done.sent, missed.len()), (5, 0));
}

// ---------------------------------------------------------------------------------------------
// a STOPPED engine's tombstone in the journal is no engine to re-admit into (v0.1.77 boot on the CI box)
// ---------------------------------------------------------------------------------------------

use tracing::Level;
use vike_log::capture::CapturedEvent;

/// A lease registry as `LeaseFacts` answers it: a route key in `leased` has an open lease and is
/// answered, any other has none (the daemon's own wording). Records every route key it was asked.
struct Leases {
    leased: &'static [&'static str],
    asked: std::sync::Mutex<Vec<String>>,
}

impl Leases {
    fn of(leased: &'static [&'static str]) -> Self {
        Leases { leased, asked: std::sync::Mutex::new(Vec::new()) }
    }
}

impl SymbolFacts for Leases {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(
        &self,
        route_key: &str,
        venue: &str,
        symbol: &str,
    ) -> Result<SymbolAdmission, String> {
        self.asked.lock().unwrap().push(route_key.to_string());
        if self.leased.contains(&route_key) {
            Ok(admission(route_key, symbol))
        } else {
            Err(format!(
                "`{route_key}` has no open trade lease at the gatewayhub on this node, so {venue} \
                 cannot be asked about `{symbol}`"
            ))
        }
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(Some(100.0))
    }
}

fn at(events: &[CapturedEvent], level: Level) -> Vec<&CapturedEvent> {
    events.iter().filter(|e| e.level == level).collect()
}

/// The journal of an account cycled `active` off and on: the stopped PRIMARY's tombstone first (still
/// listing the symbol it had admitted), then the engine that replaced it, which admitted the same one.
fn cycled_journal() -> Vec<vike_exec::EngineSnapshot> {
    let mut tombstone = restored();
    tombstone.route_key = Some(format!("bybit{RETIRED_ROUTE_KEY_SUFFIX}"));
    vec![tombstone, restored()]
}

/// **The retired engine is skipped quietly; the engine beside it is re-admitted as before.** The tombstone
/// holds an admitted symbol and no lease exists for its key, yet it is no target: not asked, not told, no
/// failure to retry, nothing above `info` (one line), while the running engine's symbol is sent.
#[test]
fn a_stopped_engines_tombstone_is_not_asked_not_told_and_not_an_error() {
    let facts = Leases::of(&["bybit"]);
    let mut sent = Vec::new();
    let ((wanted, (done, left)), events) = vike_log::capture::captured(|| {
        let wanted = targets(&cycled_journal());
        let pass = readmit_and_price(&wanted, &facts, &Pace::default(), |c| sent.push(c));
        (wanted, pass)
    });
    assert_eq!(wanted, vec![target("bybit", "ETHUSDT")], "only the running engine is a target");
    assert_eq!((done.sent, done.failed.len(), left.len()), (1, 0, 0), "{done:?} {left:?}");
    assert!(matches!(&sent[..], [Command::AdmitSymbol(a)] if a.route_key == "bybit"));
    assert_eq!(*facts.asked.lock().unwrap(), ["bybit"], "the tombstone's key is never asked");
    assert!(at(&events, Level::ERROR).is_empty(), "{events:?}");
    assert!(at(&events, Level::WARN).is_empty(), "{events:?}");
    let said = at(&events, Level::INFO);
    assert_eq!(said.len(), 1, "one line for the tombstone: {events:?}");
    assert!(said[0].message.contains("bybit#stopped"), "{:?}", said[0].message);
}

/// **A running engine whose lease is missing is still an error, said as before, and still retried**:
/// the boot's own line per symbol, `say_missed`, then a line per symbol per retry round and the closing
/// "STILL not re-admitted". The quiet above is for the tombstone's key alone.
#[test]
fn a_running_engine_without_a_lease_still_errors_and_is_retried() {
    let facts = Leases::of(&[]);
    let ((done, left_at_boot), boot) = vike_log::capture::captured(|| {
        readmit_and_price(&targets(&[restored()]), &facts, &Pace::default(), |_| {})
    });
    assert_eq!((done.sent, done.failed.len()), (0, 1), "{done:?}");
    assert_eq!(left_at_boot, vec![missed("ETHUSDT", false, 0)]);
    let errors = at(&boot, Level::ERROR);
    assert_eq!(errors.len(), 1, "{boot:?}");
    assert!(
        errors[0].message.contains("could not refresh `ETHUSDT` in `bybit`'s engine")
            && errors[0].message.contains("no open trade lease"),
        "{:?}",
        errors[0].message
    );

    let (left, retry) = vike_log::capture::captured(|| {
        say_missed(&left_at_boot);
        retry_missed(
            left_at_boot.clone(),
            &[Duration::ZERO; 2],
            far(),
            |_| true,
            |round| Some(retry_round(round, &facts, |_| {})),
        )
    });
    assert_eq!(left, left_at_boot, "no lease appeared: still missing after every round");
    let said =
        |what: &str| at(&retry, Level::ERROR).iter().filter(|e| e.message.contains(what)).count();
    assert_eq!(said("these symbols were NOT re-admitted"), 1, "{retry:?}");
    assert_eq!(said("boot retry: could not refresh"), 2, "one line per retry round: {retry:?}");
    assert_eq!(said("STILL not re-admitted"), 1, "{retry:?}");
    assert_eq!(facts.asked.lock().unwrap().len(), 3, "the boot's ask and one per round");
}

/// The tombstone is told apart by its suffix, which is safe only because no running engine can carry it:
/// an account label is `[A-Z0-9]+`, so neither `venue#stopped` nor `venue#LABEL#stopped` names an account.
#[test]
fn no_running_engines_route_key_can_end_in_the_retired_suffix() {
    use vike_model::accounts::account_keys::label_of_route_key;
    for key in ["bybit#stopped", "bybit#ALT#stopped"] {
        assert!(key.ends_with(RETIRED_ROUTE_KEY_SUFFIX), "{key}");
        assert!(label_of_route_key("bybit", key).is_none(), "{key} names an account");
    }
    for key in ["bybit", "bybit#ALT", "bybit#STOPPED"] {
        assert!(!key.ends_with(RETIRED_ROUTE_KEY_SUFFIX), "{key} is a real account's key");
        assert!(label_of_route_key("bybit", key).is_some(), "{key}");
    }
}

/// **The suffix is the one the REAL core writes** (`vike_core`'s `CoreThread::write_snap`, through the
/// constant this crate imports): a core that stops its primary and appends a new engine under the same
/// key journals the stopped one as `<key>#stopped`, the boot restore hands that snapshot back, and
/// [`targets`] finds nothing to re-admit in it though it lists an admitted symbol. If the core ever
/// tombstones under a different shape, this fails here instead of the boot going back to seven error
/// lines.
#[test]
fn the_real_cores_tombstone_of_a_replaced_primary_is_no_target() {
    use vike_core::replay::restore_for_boot;
    use vike_core::{CoreConfig, JournalConfig, spawn_core_multi};
    use vike_journal::JournalFileConfig;

    let engine = || {
        ExecutionEngine::new(
            Account::new(1.0, "bybit", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "bybit",
            "BTCUSDT",
        )
    };
    let dir = tempfile::tempdir().expect("a journal dir");
    let journal = JournalConfig {
        dir: dir.path().join("journal"),
        file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
        snapshot_every: 1_000,
    };
    let wait = |what: &str, done: &dyn Fn() -> bool| {
        let until = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    };

    // The primary has admitted ETHUSDT (as `restored()` does), then its account is stopped and started again.
    let mut primary = engine();
    primary.admit_symbol(&admission("bybit", "ETHUSDT"));
    let handle = spawn_core_multi(
        primary,
        Vec::new(),
        CoreConfig { seed_cash: 1_000.0, journal: Some(journal.clone()), ..CoreConfig::default() },
    );
    let events = handle.event_sender();
    events
        .stop_account_engine(vike_exec::NotRunningAccount {
            venue: "bybit".into(),
            account: None,
            route_key: "bybit".into(),
            tier: None,
            reason: "account inactive".into(),
        })
        .expect("the core takes the stop");
    wait("the stop", &|| {
        handle.snapshot().recent_events.iter().any(|l| l.contains("ACCOUNT STOPPED"))
    });
    events.add_account_engine(500.0, engine()).expect("the core takes the new engine");
    wait("the new engine", &|| handle.snapshot().portfolio.venues.len() == 2);
    handle.shutdown_and_join();

    let scratch = tempfile::tempdir().expect("restore scratch");
    let restore = restore_for_boot(Some(&journal), scratch.path(), vike_model::now_ms())
        .expect("the journal reads");
    let tombstone = format!("bybit{RETIRED_ROUTE_KEY_SUFFIX}");
    let held: Vec<&vike_exec::EngineSnapshot> = restore
        .restored_engines()
        .iter()
        .filter(|s| s.route_key.as_deref() == Some(tombstone.as_str()))
        .collect();
    assert_eq!(held.len(), 1, "the journal holds the stopped primary as `{tombstone}`");
    assert_eq!(held[0].extra_symbols, ["ETHUSDT"], "and it still lists the admitted symbol");
    assert!(targets(restore.restored_engines()).is_empty(), "nothing to re-admit in a tombstone");
}

// ---------------------------------------------------------------------------------------------
// The symbol's own margin rate (decision 0182's per-symbol twin): the leverage the daemon's commit
// confirmed is restated as a SECOND admission, and only a confirmed figure sends one.
// ---------------------------------------------------------------------------------------------

/// A source whose commit confirmed `figure` for every symbol, and which stamps nothing itself (its facts
/// were looked up before the commit, as the production source's first look is).
struct Confirming {
    figure: Option<f64>,
    /// The `admission` asks, so a test can say a source that confirmed nothing was not asked more.
    asks: AtomicUsize,
}

impl Confirming {
    fn new(figure: Option<f64>) -> Self {
        Confirming { figure, asks: AtomicUsize::new(0) }
    }
}

impl SymbolFacts for Confirming {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(&self, rk: &str, _: &str, symbol: &str) -> Result<SymbolAdmission, String> {
        self.asks.fetch_add(1, SeqCst);
        Ok(admission(rk, symbol))
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(Some(100.0))
    }
    fn confirmed_leverage(&self, _: &str, _: &str) -> Option<f64> {
        self.figure
    }
}

fn admitted_rates(sent: &[Command]) -> Vec<(String, Option<f64>)> {
    sent.iter()
        .map(|c| match c {
            Command::AdmitSymbol(a) => (a.symbol.clone(), a.im_requirement),
            other => panic!("only admissions are sent here, got {other:?}"),
        })
        .collect()
}

/// **The boot restates a confirmed leverage**: the re-admission goes out as it always did, and a figure
/// the commit confirmed follows it as a second admission carrying `1 / figure`, which the rebuilt engine
/// writes to the symbol's own margin row (the engine-wide rate is untouched).
#[test]
fn the_boot_restates_the_leverage_the_commit_confirmed() {
    let mut sent = Vec::new();
    let done = readmit(
        &[target("bybit", "ETHUSDT")],
        &Confirming::new(Some(10.0)),
        &Pace::default(),
        |c| sent.push(c),
    );
    assert_eq!(done.sent, 1, "one re-admission, the second is a restatement of it");
    assert_eq!(
        admitted_rates(&sent),
        [("ETHUSDT".to_string(), None), ("ETHUSDT".to_string(), Some(0.1))],
        "the admission, then the same symbol at 1 / 10"
    );
    let mut rebuilt = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits { im_requirement: Some(1.0), ..RiskLimits::new() }),
        RecordingClient::default(),
        "bybit",
        "BTCUSDT",
    );
    for command in sent {
        let Command::AdmitSymbol(a) = command else { unreachable!() };
        assert!(!matches!(rebuilt.admit_symbol(&a), vike_exec::AdmitOutcome::Refused(_)));
    }
    assert_eq!(rebuilt.gate.limits.im_for("ETHUSDT"), Some(0.1), "the symbol's own rate");
    assert_eq!(
        rebuilt.gate.limits.im_for("BTCUSDT"),
        Some(1.0),
        "the mounted symbol keeps the engine's"
    );
}

/// **A source that confirmed nothing sends nothing more**: one admission per symbol, exactly the pass
/// that existed before the field, and a figure that is no leverage (below 1x, not a number) is no figure.
#[test]
fn nothing_confirmed_sends_one_admission_and_a_bad_figure_is_nothing() {
    for figure in [None, Some(0.5), Some(0.0), Some(-3.0), Some(f64::NAN), Some(f64::INFINITY)] {
        let mut sent = Vec::new();
        readmit(&[target("bybit", "ETHUSDT")], &Confirming::new(figure), &Pace::default(), |c| {
            sent.push(c)
        });
        assert_eq!(admitted_rates(&sent), [("ETHUSDT".to_string(), None)], "figure {figure:?}");
    }
}

/// **A figure the admission already carries is not sent twice** (the facts source stamps the last
/// commit's figure on every answer, so a restated rate rides the first admission and the second would be
/// the same bytes), and a figure that DIFFERS is restated.
#[test]
fn a_figure_the_admission_already_states_is_not_restated_and_a_different_one_is() {
    struct Stamping(f64);
    impl SymbolFacts for Stamping {
        fn generation(&self, _: &str) -> u64 {
            0
        }
        fn admission(&self, rk: &str, _: &str, symbol: &str) -> Result<SymbolAdmission, String> {
            Ok(SymbolAdmission { im_requirement: Some(1.0 / 10.0), ..admission(rk, symbol) })
        }
        fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
            Ok(Some(100.0))
        }
        fn confirmed_leverage(&self, _: &str, _: &str) -> Option<f64> {
            Some(self.0)
        }
    }
    let mut same = Vec::new();
    readmit(&[target("bybit", "ETHUSDT")], &Stamping(10.0), &Pace::default(), |c| same.push(c));
    assert_eq!(admitted_rates(&same), [("ETHUSDT".to_string(), Some(0.1))]);
    let mut changed = Vec::new();
    readmit(&[target("bybit", "ETHUSDT")], &Stamping(5.0), &Pace::default(), |c| changed.push(c));
    assert_eq!(
        admitted_rates(&changed),
        [("ETHUSDT".to_string(), Some(0.1)), ("ETHUSDT".to_string(), Some(0.2))],
        "the commit now confirms 5x: the engine's row is replaced"
    );
}

/// **The retry restates too**: a symbol the boot missed is admitted by the retry and then restated once
/// (the commit that followed its admission and the retry's own re-commit confirm the same figure), and a
/// symbol whose admission already went out (only its price missing) is restated against the facts the
/// source holds. A source that confirmed nothing is asked nothing extra.
#[test]
fn the_retry_restates_a_confirmed_leverage_once_and_asks_nothing_when_none_was_confirmed() {
    let mut sent = Vec::new();
    let facts = Confirming::new(Some(4.0));
    let left = retry_round(&[missed("ETHUSDT", false, 0)], &facts, |c| sent.push(c));
    assert!(left.is_empty(), "{left:?}");
    assert_eq!(
        admitted_rates(&sent),
        [("ETHUSDT".to_string(), None), ("ETHUSDT".to_string(), Some(0.25))],
        "the admission, then the restatement: not one more for the re-commit"
    );

    let mut told_sent = Vec::new();
    let facts = Confirming::new(Some(4.0));
    retry_round(&[missed("ETHUSDT", true, 0)], &facts, |c| told_sent.push(c));
    assert_eq!(
        admitted_rates(&told_sent),
        [("ETHUSDT".to_string(), Some(0.25))],
        "a told symbol sends no admission of its own; the re-commit's figure is restated"
    );

    let quiet = Confirming::new(None);
    let mut quiet_sent = Vec::new();
    retry_round(&[missed("ETHUSDT", true, 0)], &quiet, |c| quiet_sent.push(c));
    assert!(quiet_sent.is_empty());
    assert_eq!(
        quiet.asks.load(SeqCst),
        0,
        "nothing confirmed, so the source is not asked for facts"
    );
}

// ---------------------------------------------------------------------------------------------
// A symbol quoted in another currency than the account's (decision 0183, phase 1) across a restart.
// ---------------------------------------------------------------------------------------------

/// `symbol` quoted in GBP, `rate` account units per pound, asked at `as_of_ms` on the engine's clock.
fn priced(symbol: &str, rate: f64, as_of_ms: i64) -> SymbolAdmission {
    SymbolAdmission {
        quote_ccy: Some("GBP".into()),
        quote_rate: Some(rate),
        rate_as_of_ms: Some(as_of_ms),
        ..admission("bybit", symbol)
    }
}

fn bare_engine() -> ExecutionEngine<RecordingClient> {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTCUSDT",
    );
    // The node tells an account that converts its currency at assembly; a foreign-quoted admission is
    // refused `account-ccy-unset` until then.
    engine.set_account_ccy("USD").expect("a currency code");
    engine
}

/// A source that prices every symbol it is asked for in GBP at `rate`, asked at `as_of_ms`.
struct Pricing {
    rate: f64,
    as_of_ms: i64,
}

impl SymbolFacts for Pricing {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(&self, rk: &str, _: &str, symbol: &str) -> Result<SymbolAdmission, String> {
        Ok(SymbolAdmission { route_key: rk.into(), ..priced(symbol, self.rate, self.as_of_ms) })
    }
    fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
        Ok(Some(0.85))
    }
}

/// **A restart keeps the rate row, and the boot renews it.** The journal restores a foreign-quoted symbol with
/// its currency and the rate it was last priced at (the engine's own restore); the symbol is a re-admission
/// target like any admitted one; the boot's pass re-reads the rate and the rebuilt engine takes the fresh one.
/// A boot that cannot reach the rate source leaves the old row, which the engine calls stale at 120 s: an
/// opening order is then denied (`quote-rate-stale`) and a reduce never is.
#[test]
fn a_restart_keeps_a_foreign_symbols_rate_row_and_the_boot_renews_it() {
    let mut live = bare_engine();
    live.now_ms = 1_000;
    assert!(!matches!(
        live.admit_symbol(&priced("EURGBP", 1.35, 1_000)),
        vike_exec::AdmitOutcome::Refused(_)
    ));
    let snap = live.snapshot_state();

    let mut rebuilt = bare_engine();
    rebuilt.restore_runtime_state(&snap);
    assert!(rebuilt.accepts_symbol("EURGBP"), "the restore put the admission back");
    assert_eq!(rebuilt.account.quote_ccy_of("EURGBP"), Some("GBP"), "the currency survived");
    assert_eq!(rebuilt.account.quote_rate_of("EURGBP"), 1.35, "and so did the rate row");

    // No rate source reachable: the old row stands, and goes stale after the engine's window.
    let window = vike_exec::DEFAULT_QUOTE_RATE_TTL_MS;
    assert_eq!(rebuilt.account.quote_leg("EURGBP", 1_000 + window), (1.35, false));
    assert_eq!(rebuilt.account.quote_leg("EURGBP", 1_000 + window + 1), (1.35, true));

    // The boot's pass: the symbol is a target and the re-admission carries a fresh rate.
    let wanted = targets(std::slice::from_ref(&snap));
    assert_eq!(wanted, vec![target("bybit", "EURGBP")]);
    let mut sent = Vec::new();
    let done =
        readmit(&wanted, &Pricing { rate: 1.36, as_of_ms: 500_000 }, &Pace::default(), |c| {
            sent.push(c);
        });
    assert_eq!(done.sent, 1);
    rebuilt.now_ms = 500_000;
    for command in sent {
        let Command::AdmitSymbol(a) = command else { unreachable!() };
        assert!(!matches!(rebuilt.admit_symbol(&a), vike_exec::AdmitOutcome::Refused(_)), "{a:?}");
    }
    assert_eq!(rebuilt.account.quote_rate_of("EURGBP"), 1.36);
    assert_eq!(rebuilt.account.quote_leg("EURGBP", 500_000), (1.36, false), "fresh again");
}

/// **The same restart on a ctrader engine and a YEN pair**: the rate row is a FRACTION of a dollar (`1/150`,
/// never 150) and survives the journal restore bit for bit, so 100,000 units at 150 are still 100,000 dollars of
/// notional after the restart (not 15,000,000), and the boot's re-admission renews the row at the fresh
/// `1/150.10` the cTrader rate source states (`crate::quote_rate`'s orientation).
#[test]
fn a_ctrader_yen_pair_keeps_its_fractional_rate_row_across_a_restart() {
    let engine_at = || {
        let mut engine = ExecutionEngine::new(
            Account::new(1.0, "ctrader", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "ctrader",
            "EURUSD",
        );
        engine.set_account_ccy("USD").expect("a currency code");
        engine
    };
    let yen = |rate: f64, as_of_ms: i64| SymbolAdmission {
        quote_ccy: Some("JPY".into()),
        quote_rate: Some(rate),
        rate_as_of_ms: Some(as_of_ms),
        ..admission("ctrader", "USDJPY")
    };
    let mut live = engine_at();
    live.now_ms = 1_000;
    assert!(!matches!(
        live.admit_symbol(&yen(1.0 / 150.0, 1_000)),
        vike_exec::AdmitOutcome::Refused(_)
    ));
    let snap = live.snapshot_state();

    let mut rebuilt = engine_at();
    rebuilt.restore_runtime_state(&snap);
    assert_eq!(rebuilt.account.quote_ccy_of("USDJPY"), Some("JPY"));
    assert_eq!(rebuilt.account.quote_rate_of("USDJPY").to_bits(), (1.0_f64 / 150.0).to_bits());
    let notional = rebuilt.account.contract_value_of("USDJPY").order_notional(100_000.0, 150.0);
    assert!((notional - 100_000.0).abs() < 1e-6, "{notional}: dollars, not 15,000,000 yen");

    let wanted = targets(std::slice::from_ref(&snap));
    assert_eq!(
        wanted,
        vec![Target {
            route_key: "ctrader".into(),
            venue: "ctrader".into(),
            symbol: "USDJPY".into()
        }]
    );
    struct Yen;
    impl SymbolFacts for Yen {
        fn generation(&self, _: &str) -> u64 {
            0
        }
        fn admission(&self, rk: &str, _: &str, symbol: &str) -> Result<SymbolAdmission, String> {
            Ok(SymbolAdmission {
                route_key: rk.into(),
                quote_ccy: Some("JPY".into()),
                quote_rate: Some(1.0 / 150.10),
                rate_as_of_ms: Some(500_000),
                ..admission(rk, symbol)
            })
        }
        fn price(&self, _: &str, _: &str, _: &str, _: bool) -> Result<Option<f64>, String> {
            Ok(Some(150.1))
        }
    }
    let mut sent = Vec::new();
    let done = readmit(&wanted, &Yen, &Pace::default(), |c| sent.push(c));
    assert_eq!(done.sent, 1);
    rebuilt.now_ms = 500_000;
    for command in sent {
        let Command::AdmitSymbol(a) = command else { unreachable!() };
        assert!(!matches!(rebuilt.admit_symbol(&a), vike_exec::AdmitOutcome::Refused(_)), "{a:?}");
    }
    assert_eq!(rebuilt.account.quote_rate_of("USDJPY").to_bits(), (1.0_f64 / 150.10).to_bits());
    assert_eq!(rebuilt.account.quote_leg("USDJPY", 500_000), (1.0 / 150.10, false), "fresh again");
}

// ---------------------------------------------------------------------------------------------
// The binance lane twin (`binance#PERP`, mounted on `BTCUSDT.P`): its price lane is opened at the boot,
// and what it admitted on demand is re-admitted into IT and not into the spot engine
// ---------------------------------------------------------------------------------------------

/// The rows of a node whose binance default account (demo) states `leverage` or not, read the way a root
/// reads them: while it states one the table DERIVES the twin `binance#PERP` and MARKS it.
fn twin_rows(leverage: Option<f64>) -> AccountDirectory {
    let parent = vike_secrets::Account {
        id: 3,
        venue: "binance".to_string(),
        tier: vike_model::AccountTier::Demo,
        label: None,
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage: leverage.and_then(vike_secrets::AccountLeverage::new),
    };
    AccountDirectory::read(
        Ok::<_, String>(vike_secrets::Accounts::Known(vec![parent])),
        Ok::<_, String>(None),
        Ok::<_, String>(vike_secrets::account_credentials::CredentialBook::new()),
    )
}

fn running(keys: &[&str]) -> HashSet<String> {
    keys.iter().map(|key| (*key).to_string()).collect()
}

fn twin_target() -> Target {
    Target { route_key: "binance#PERP".into(), venue: "binance".into(), symbol: "BTCUSDT.P".into() }
}

/// **A derived twin that came up is priced once at the boot**: it is the only target, it is announced as ONE
/// `pricing binance 1/1` step on the real `starting` reporter, and its lane is opened once, on a short-lived
/// thread of its own.
#[test]
fn a_running_derived_twin_is_announced_and_warmed_once() {
    let wanted = twin_targets(&twin_rows(Some(5.0)), &running(&["binance", "binance#PERP"]));
    assert_eq!(wanted, vec![twin_target()]);

    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = std::sync::Arc::clone(&lines);
    let progress = crate::server::starting::StartProgress::new()
        .with_reporter(move |text| sink.lock().unwrap().push(text.to_string()));
    let report = |text: &str| progress.phase(text);
    let pace = Pace::new(Duration::from_secs(3600), &report);

    let (tx, rx) = std::sync::mpsc::channel::<(Target, Option<String>)>();
    let tx = std::sync::Mutex::new(tx);
    let handles = warm_twins(&wanted, &pace, move |t| {
        let thread = std::thread::current().name().map(str::to_string);
        tx.lock().unwrap().send((t.clone(), thread)).unwrap();
    });
    assert_eq!(handles.len(), 1);
    for handle in handles {
        handle.join().expect("the warm-up ends quietly");
    }
    assert_eq!(
        rx.try_iter().collect::<Vec<_>>(),
        [(twin_target(), Some("twin-price-warm".to_string()))],
        "one warm-up, on the twin's own symbol"
    );
    assert_eq!(*lines.lock().unwrap(), ["pricing binance 1/1"]);
}

/// **Nothing is priced for a spot-only account**: an account that states no leverage derives no twin, a twin
/// whose engine did not come up has no lease to price over, an unread table derives nothing and a real account
/// that merely spells `PERP` is not the twin. And a boot with no twin starts no thread.
#[test]
fn a_spot_only_account_and_a_twin_that_did_not_start_have_nothing_to_warm() {
    let both = running(&["binance", "binance#PERP"]);
    assert!(twin_targets(&twin_rows(None), &both).is_empty(), "no leverage stated: no twin");
    assert!(
        twin_targets(&twin_rows(Some(5.0)), &running(&["binance"])).is_empty(),
        "derived, but its engine did not come up"
    );
    assert!(twin_targets(&AccountDirectory::unread(), &both).is_empty(), "an unread table");
    let perp = AccountLabel::parse("PERP").expect("a legal label");
    let real = vike_tradehub_core::AdapterPolicy::default()
        .with_account("binance", &AccountLabel::Default, vike_model::AccountTier::Demo)
        .with_account("binance", &perp, vike_model::AccountTier::Demo)
        .accounts;
    assert!(twin_targets(&real, &both).is_empty(), "a real PERP account is not a derived twin");

    let none = warm_twins(&[], &Pace::default(), |_| panic!("no twin, no warm-up"));
    assert!(none.is_empty());
}

/// **A warm-up that never returns holds nothing**: `warm_twins` has returned while the warm-up is still
/// parked inside its closure, so a lease or a datahub that never answers cannot hold `ready`.
#[test]
fn a_warm_up_that_never_returns_holds_nothing() {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::channel;

    let (entered_tx, entered_rx) = channel::<()>();
    let (release_tx, release_rx) = channel::<()>();
    let (entered_tx, release_rx) = (Mutex::new(entered_tx), Mutex::new(release_rx));
    let finished = std::sync::Arc::new(AtomicBool::new(false));
    let done = std::sync::Arc::clone(&finished);
    let handles = warm_twins(&[twin_target()], &Pace::default(), move |_| {
        entered_tx.lock().unwrap().send(()).unwrap();
        // Parked until the test lets go (bounded, so a failed assertion cannot hang the run).
        let _ = release_rx.lock().unwrap().recv_timeout(Duration::from_secs(30));
        done.store(true, SeqCst);
    });
    entered_rx.recv_timeout(Duration::from_secs(10)).expect("the warm-up started");
    assert!(!finished.load(SeqCst), "the boot is past the step while the warm-up still runs");
    release_tx.send(()).unwrap();
    for handle in handles {
        handle.join().expect("the warm-up ends quietly");
    }
    assert!(finished.load(SeqCst));
}

/// **A warm-up that fails is a warning and nothing more**: the production closure (`LeaseFacts::warm_price`)
/// over a registry that has no price feed for the venue cannot open the lane; the thread ends without a
/// panic, and the door's own ask at the first order stands.
#[test]
fn a_warm_up_that_fails_is_a_warning_and_the_boot_goes_on() {
    let facts = std::sync::Arc::new(crate::remote_backend::facts::LeaseFacts::default());
    let handles = warm_twins(&[twin_target()], &Pace::default(), {
        let facts = std::sync::Arc::clone(&facts);
        move |t: &Target| facts.warm_price(&t.route_key, &t.venue, &t.symbol)
    });
    for handle in handles {
        handle.join().expect("a failed warm-up ends its thread quietly");
    }
}

/// A source that answers every symbol and records `(route key, symbol)` of each look and each price ask.
#[derive(Default)]
struct Asked {
    looked: std::sync::Mutex<Vec<(String, String)>>,
    priced: std::sync::Mutex<Vec<(String, String)>>,
}

impl SymbolFacts for Asked {
    fn generation(&self, _: &str) -> u64 {
        0
    }
    fn admission(&self, route_key: &str, _: &str, symbol: &str) -> Result<SymbolAdmission, String> {
        self.looked.lock().unwrap().push((route_key.into(), symbol.into()));
        Ok(admission(route_key, symbol))
    }
    fn price(
        &self,
        route_key: &str,
        _: &str,
        symbol: &str,
        _: bool,
    ) -> Result<Option<f64>, String> {
        self.priced.lock().unwrap().push((route_key.into(), symbol.into()));
        Ok(Some(100.0))
    }
}

/// **A `.P` symbol the twin admitted on demand is re-admitted into the TWIN.** The REAL core journals a spot
/// engine that admitted `ETHUSDT` and the twin `binance#PERP` that admitted `ETHUSDT.P`; the boot restore
/// hands both snapshots back, [`targets`] keys each by its own route key, and the pass looks the symbol up,
/// sends its `AdmitSymbol` and subscribes its price under `binance#PERP`, never under the spot key.
#[test]
fn a_perp_symbol_the_twin_admitted_is_readmitted_under_the_twins_route_key() {
    use vike_core::replay::restore_for_boot;
    use vike_core::{CoreConfig, JournalConfig, spawn_core_multi};
    use vike_journal::JournalFileConfig;

    let engine = |route_key: &str, mounted: &str, admitted: &str| {
        let mut engine = ExecutionEngine::new(
            Account::new(1.0, "binance", None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            RecordingClient::default(),
            "binance",
            mounted,
        );
        engine.route_key = route_key.into();
        engine.admit_symbol(&admission(route_key, admitted));
        engine
    };
    let dir = tempfile::tempdir().expect("a journal dir");
    let journal = JournalConfig {
        dir: dir.path().join("journal"),
        file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
        snapshot_every: 1_000,
    };
    let handle = spawn_core_multi(
        engine("binance", "BTCUSDT", "ETHUSDT"),
        vec![(500.0, engine("binance#PERP", "BTCUSDT.P", "ETHUSDT.P"))],
        CoreConfig { seed_cash: 1_000.0, journal: Some(journal.clone()), ..CoreConfig::default() },
    );
    handle.shutdown_and_join();

    let scratch = tempfile::tempdir().expect("restore scratch");
    let restore = restore_for_boot(Some(&journal), scratch.path(), vike_model::now_ms())
        .expect("the journal reads");
    let on_binance = |route_key: &str, symbol: &str| Target {
        route_key: route_key.into(),
        venue: "binance".into(),
        symbol: symbol.into(),
    };
    let mut wanted = targets(restore.restored_engines());
    wanted.sort_by(|a, b| a.route_key.cmp(&b.route_key));
    assert_eq!(
        wanted,
        vec![on_binance("binance", "ETHUSDT"), on_binance("binance#PERP", "ETHUSDT.P")]
    );

    let facts = Asked::default();
    let mut sent = Vec::new();
    let (done, missed) = readmit_and_price(&wanted, &facts, &Pace::default(), |c| sent.push(c));
    assert_eq!((done.sent, missed.len()), (2, 0), "{:?}", done.failed);
    let twin = ("binance#PERP".to_string(), "ETHUSDT.P".to_string());
    let spot_by_mistake = ("binance".to_string(), "ETHUSDT.P".to_string());
    let landed: Vec<(String, String)> = sent
        .iter()
        .map(|command| {
            let Command::AdmitSymbol(a) = command else { unreachable!() };
            (a.route_key.clone(), a.symbol.clone())
        })
        .collect();
    for seen in [&landed, &*facts.looked.lock().unwrap(), &*facts.priced.lock().unwrap()] {
        assert!(seen.contains(&twin), "{seen:?}");
        assert!(!seen.contains(&spot_by_mistake), "never into the spot engine: {seen:?}");
    }
}

// ---------------------------------------------------------------------------------------------
// a restored engine the boot did not rebuild, holding something (review r2, finding 5)
// ---------------------------------------------------------------------------------------------

/// **A journaled engine that holds a resting order or a position but is not among the engines this boot
/// built is said at ERROR and listed NOT RUNNING with what it holds**, so the position is not left at the venue
/// with nothing watching it and only a WARN in the core's log. An engine that runs, a flat one and a stopped
/// engine's tombstone are not flagged; an account the boot listed already keeps its reason and gains what it
/// holds.
#[test]
fn a_restored_engine_that_holds_something_and_was_not_rebuilt_is_listed_with_what_it_holds() {
    let mut twin = restored();
    (twin.venue, twin.route_key) = ("binance".into(), Some("binance#PERP".into()));
    let mut tombstone = restored();
    tombstone.route_key = Some("bybit#stopped".into());
    let flat = ExecutionEngine::new(
        Account::new(1.0, "okx", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "okx",
        "BTC-USDT-SWAP",
    )
    .snapshot_state();
    let holds = restored_exposure(&[twin, tombstone, flat, restored()]);
    let keys: Vec<&str> = holds.iter().map(|h| h.route_key.as_str()).collect();
    assert_eq!(
        keys,
        ["binance#PERP", "bybit"],
        "the flat engine and the tombstone hold nothing to flag"
    );
    assert!(holds[0].live_orders > 0, "the fixture's orders are resting");

    // Both run: nothing to say.
    let mut listed = Vec::new();
    let running = vec!["binance#PERP".to_string(), "bybit".to_string()];
    assert!(flag_unmanaged_exposure(&holds, &running, &mut listed).is_empty() && listed.is_empty());

    // The twin was not rebuilt: flagged, listed with its orders. `bybit` runs and is left alone.
    let running = vec!["bybit".to_string()];
    let flagged = flag_unmanaged_exposure(&holds, &running, &mut listed);
    assert_eq!(flagged, ["binance#PERP"]);
    let entry = &listed[0];
    assert_eq!((entry.venue.as_str(), entry.route_key.as_str()), ("binance", "binance#PERP"));
    assert_eq!(entry.account.as_ref().and_then(|a| a.text()), Some("PERP"));
    assert!(
        entry.reason.contains("NOT managed") && entry.reason.contains("order(s) resting"),
        "{}",
        entry.reason
    );

    // An account the boot listed already keeps its reason and gains what it holds.
    let mut listed = vec![vike_exec::NotRunningAccount {
        venue: "binance".into(),
        account: AccountLabel::parse("PERP").ok(),
        route_key: "binance#PERP".into(),
        tier: None,
        reason: "account inactive".into(),
    }];
    flag_unmanaged_exposure(&holds, &running, &mut listed);
    assert_eq!(listed.len(), 1);
    assert!(listed[0].reason.starts_with("account inactive; restored state is NOT managed"));
}

// ---------------------------------------------------------------------------------------------
// the engine's OWN symbol, held (decision 0197)
// ---------------------------------------------------------------------------------------------

fn own(route_key: &str, symbol: &str) -> Target {
    Target {
        route_key: route_key.into(),
        venue: route_key.split('#').next().unwrap().into(),
        symbol: symbol.into(),
    }
}

/// [`restored`] at `tier`: the journal's engine holds a live order in its own `BTCUSDT` and one in `ETHUSDT`.
fn restored_at(tier: Option<AccountTier>) -> vike_exec::EngineSnapshot {
    let mut snap = restored();
    snap.tier = tier;
    snap
}

/// **A restored default engine that holds an order in its OWN symbol is a boot target for the lane keeper**, at
/// the `demo` and the `live` tier, and [`targets`] still does not list the symbol (the re-admission pass has
/// nothing to say about the engine's own symbol).
#[test]
fn a_restored_hold_in_the_engines_own_symbol_is_an_own_hold_target() {
    for tier in [AccountTier::Demo, AccountTier::Live] {
        let snap = restored_at(Some(tier));
        assert_eq!(own_hold_targets(std::slice::from_ref(&snap)), vec![own("bybit", "BTCUSDT")]);
        assert_eq!(
            targets(&[snap]),
            vec![target("bybit", "ETHUSDT")],
            "the pass skips the own symbol"
        );
    }
}

/// **A restored LABELLED engine that holds something in its own symbol is a boot target too** (decision 0198), with
/// its own route key: `hyperliquid#LIVE`'s hold is not the default engine's. The tier rule is the same.
#[test]
fn a_restored_hold_of_a_labelled_engine_is_an_own_hold_target_under_its_route_key() {
    for tier in [AccountTier::Demo, AccountTier::Live] {
        let mut labelled = restored_at(Some(tier));
        labelled.route_key = Some("bybit#ALT".into());
        assert_eq!(
            own_hold_targets(std::slice::from_ref(&labelled)),
            vec![own("bybit#ALT", "BTCUSDT")]
        );
        labelled.tier = Some(AccountTier::Paper);
        assert!(own_hold_targets(&[labelled]).is_empty(), "paper");
    }
    // The default engine and the labelled one of one venue are two targets, one lane (the registry dedupes it).
    let default = restored_at(Some(AccountTier::Live));
    let mut labelled = restored_at(Some(AccountTier::Live));
    labelled.route_key = Some("bybit#ALT".into());
    assert_eq!(
        own_hold_targets(&[default, labelled]),
        vec![own("bybit", "BTCUSDT"), own("bybit#ALT", "BTCUSDT")]
    );
}

/// Nothing held in the own symbol, or an engine that cannot be priced, wants no boot lane: a flat engine,
/// an order in a FOREIGN symbol only, a `paper` engine, a snapshot that states no tier and a stopped engine's
/// tombstone.
#[test]
fn nothing_held_in_the_own_symbol_wants_no_boot_lane() {
    let mut flat = restored_at(Some(AccountTier::Demo));
    flat.registry.clear();
    flat.account.positions.clear();
    assert!(own_hold_targets(&[flat]).is_empty(), "flat");

    let mut foreign_only = restored_at(Some(AccountTier::Demo));
    foreign_only.registry.retain(|(_, order)| order.request.symbol != "BTCUSDT");
    assert!(!foreign_only.registry.is_empty(), "the fixture still holds the foreign order");
    assert!(
        own_hold_targets(&[foreign_only]).is_empty(),
        "a foreign symbol is the pass's, not this"
    );

    let mut ended = restored_at(Some(AccountTier::Demo));
    for (_, order) in &mut ended.registry {
        order.status = vike_model::OrderStatus::Canceled;
    }
    assert!(own_hold_targets(&[ended]).is_empty(), "a terminal order holds nothing");

    assert!(own_hold_targets(&[restored_at(Some(AccountTier::Paper))]).is_empty(), "paper");
    assert!(own_hold_targets(&[restored_at(None)]).is_empty(), "no tier stated");
    let mut tombstone = restored_at(Some(AccountTier::Demo));
    tombstone.route_key = Some("bybit#stopped".into());
    assert!(own_hold_targets(&[tombstone]).is_empty(), "a stopped engine's tombstone");
}

fn position(symbol: &str, size: f64) -> vike_exec::PositionView {
    vike_exec::PositionView {
        venue: "bybit".into(),
        symbol: symbol.into(),
        position_side: vike_model::events::PositionSide::Both,
        size,
        avg_px: 100.0,
        unrealized: 0.0,
        mark_source: None,
        leverage: 0.0,
        liq_price: 0.0,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    }
}

fn block(
    route_key: &str,
    symbol: &str,
    tier: Option<AccountTier>,
    positions: Vec<vike_exec::PositionView>,
) -> vike_exec::VenueBlock {
    vike_exec::VenueBlock {
        venue: route_key.split('#').next().unwrap().into(),
        route_key: route_key.into(),
        symbol: symbol.into(),
        tier,
        positions,
        ..Default::default()
    }
}

fn order(
    account: Option<&str>,
    symbol: &str,
    status: vike_model::OrderStatus,
) -> vike_exec::OrderView {
    vike_exec::OrderView {
        data_delay: None,
        client_order_id: "o".to_string(),
        venue: "bybit".to_string(),
        account: account.map(|a| AccountLabel::parse(a).expect("a legal label")),
        symbol: symbol.to_string(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        reason: None,
    }
}

/// **The published snapshot's view**: a `demo`/`live` engine, default or labelled (decision 0198), is wanted while
/// it holds a non-flat position or a LIVE order in its own symbol, and not otherwise - flat, a position only in
/// another symbol, an ended order, another account's order, a `paper` engine, an engine with no symbol.
#[test]
fn the_published_engines_that_hold_something_in_their_own_symbol_are_wanted() {
    use vike_model::OrderStatus::{Accepted, Canceled};
    let demo = Some(AccountTier::Demo);
    let blocks = vec![
        block("bybit", "BTCUSDT", demo, vec![position("BTCUSDT", 0.5)]),
        block("okx", "BTC-USDT-SWAP", Some(AccountTier::Live), vec![]),
        block("binance", "BTCUSDT", demo, vec![position("ETHUSDT", 2.0), position("BTCUSDT", 0.0)]),
        block("oanda", "EURUSD", Some(AccountTier::Paper), vec![position("EURUSD", 1.0)]),
        block("bybit#ALT", "BTCUSDT", demo, vec![position("BTCUSDT", 1.0)]),
        block("ig", "", demo, vec![position("X", 1.0)]),
        block("hyperliquid#LIVE", "BTC", Some(AccountTier::Live), vec![]),
        block("bybit#FLAT", "BTCUSDT", demo, vec![position("ETHUSDT", 1.0)]),
        block(
            "okx#PAPER",
            "BTC-USDT-SWAP",
            Some(AccountTier::Paper),
            vec![position("BTC-USDT-SWAP", 1.0)],
        ),
    ];
    let mut orders = vec![order(None, "BTC-USDT-SWAP", Accepted)];
    orders[0].venue = "okx".into();
    let mut resting = order(Some("LIVE"), "BTC", Accepted);
    resting.venue = "hyperliquid".into();
    orders.push(resting);
    assert_eq!(
        own_symbol_holds(&blocks, &orders),
        vec![
            own("bybit", "BTCUSDT"),
            own("okx", "BTC-USDT-SWAP"),
            own("bybit#ALT", "BTCUSDT"),
            own("hyperliquid#LIVE", "BTC"),
        ],
        "a position in the own symbol, and a live order in it (a labelled engine's order is read by its own \
         account); every other row holds nothing wanted"
    );

    // An ended order and another account's order do not count.
    let mut ended = order(None, "BTCUSDT", Canceled);
    ended.venue = "binance".into();
    let mut other_account = order(Some("ALT"), "BTCUSDT", Accepted);
    other_account.venue = "binance".into();
    assert!(own_symbol_holds(&blocks[2..3], &[ended, other_account]).is_empty());

    // Only the NEWEST block of a route key speaks: a restarted account's old block is history.
    let restarted = vec![
        block("bybit", "BTCUSDT", demo, vec![position("BTCUSDT", 9.0)]),
        block("bybit", "BTCUSDT", demo, vec![]),
    ];
    assert!(own_symbol_holds(&restarted, &[]).is_empty());
}
