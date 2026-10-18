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
    let done =
        readmit(&[target("bybit", "ETHUSDT"), target("bybit", "DELISTEDUSDT")], &Facts, |c| {
            sent.push(c)
        });
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
    readmit(&targets(&[snap]), &Facts, |c| sent.push(c));
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
    let done = readmit(&targets(std::slice::from_ref(&snap)), &TimedOut, |c| sent.push(c));
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
        readmit_and_price(&targets(std::slice::from_ref(&snap)), &facts, |c| sent.push(c));
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
    let (done, left_at_boot) = readmit_and_price(&wanted, &facts, |c| sent.push(c));
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

/// **The retry loop runs no round when the process is stopping or the registry is gone** (`drive`, the
/// thread's body, driven here on the test thread: no spawn, no race). The rounds are COUNTED, so a loop
/// that ignored the stop flag or the `Weak` would fail: with the registry alive and the flag down, the
/// same call runs a round per pause.
#[test]
fn the_retry_loop_runs_no_round_when_stopping_or_when_the_registry_is_gone() {
    use crate::remote_backend::facts::LeaseFacts;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    let pauses = [Duration::ZERO; 3];
    let work = || vec![missed("ETHUSDT", false, 0)];
    let rounds = std::cell::Cell::new(0);
    let run = |_: &LeaseFacts, round: &[Missed]| {
        rounds.set(rounds.get() + 1);
        round.to_vec()
    };

    let facts = Arc::new(LeaseFacts::default());
    let left = drive(work(), &Arc::downgrade(&facts), &AtomicBool::new(false), &pauses, far(), run);
    assert_eq!(
        (rounds.get(), left),
        (3, work()),
        "control: a live registry and a quiet process run every round"
    );

    rounds.set(0);
    let left = drive(work(), &Arc::downgrade(&facts), &AtomicBool::new(true), &pauses, far(), run);
    assert_eq!((rounds.get(), left), (0, work()), "the stop flag is up: not one round");

    let gone = Arc::new(LeaseFacts::default());
    let weak = Arc::downgrade(&gone);
    drop(gone);
    let left = drive(work(), &weak, &AtomicBool::new(false), &pauses, far(), run);
    assert_eq!((rounds.get(), left), (0, work()), "the registry is dropped: not one round");
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
    let done = readmit(&got, &Venue, |c| sent.push(c));
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
    let done = readmit(&targets(&[]), &asked, |_| sent += 1);
    assert_eq!((done, sent), (Readmitted::default(), 0));
    assert_eq!(asked.0.load(std::sync::atomic::Ordering::SeqCst), 0);
}
