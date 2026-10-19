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
