//! **The engine takes the venue's balance before the door opens** (decision 0246): the boot's reconcile driver is
//! mounted by `first_balance::mount_reconcile_driver`, which holds the start until every leg's first balance has
//! reached its engine (bounded), tells the door which accounts it is still waiting for, and leaves a failed read
//! refused BY NAME with the reason while every other account trades. A real core thread and a real driver; the venue
//! is a scripted reconcile client.

use super::first_balance::{BootWait, mount_reconcile_driver};
use super::*;
use crate::remote_backend::facts::LeaseFacts;
use crate::server::symbol_admission::SymbolFacts;
use vike_exec::recon::ReconClient;
use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};
use vike_tradehub_core::{ReconConfig, ReconLeg};

/// A venue whose balance read answers as told, but only once `release` is raised.
struct Venue {
    balance: Result<Option<f64>, String>,
    release: Arc<AtomicBool>,
}

impl Venue {
    /// A venue that answers at once.
    fn answering(balance: Result<Option<f64>, String>) -> Box<dyn ReconClient> {
        Box::new(Venue { balance, release: Arc::new(AtomicBool::new(true)) })
    }

    /// A venue that answers `balance` only when the returned flag is raised.
    fn held_back(balance: f64) -> (Box<dyn ReconClient>, Arc<AtomicBool>) {
        let release = Arc::new(AtomicBool::new(false));
        (Box::new(Venue { balance: Ok(Some(balance)), release: Arc::clone(&release) }), release)
    }
}

impl ReconClient for Venue {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_balance(&self) -> Result<Option<f64>, String> {
        while !self.release.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.balance.clone()
    }
}

/// A live core with ONE `binance` engine on a seed of 1000, the shape `stop_and_deadlines` builds.
fn core() -> CoreHandle {
    use vike_exec::testing::RecordingClient;
    use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate};
    let engine = ExecutionEngine::new(
        Account::new(1_000.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(vike_model::RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    vike_core::spawn_core(engine, vike_core::CoreConfig::default())
}

fn quick_pass() -> ReconConfig {
    ReconConfig { startup_delay: Duration::from_millis(50), ..ReconConfig::default() }
}

fn wait_of(bound: Duration, stop: Option<&AtomicBool>) -> BootWait<'_> {
    BootWait { progress: None, stop, bound }
}

fn until(secs: u64, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if ready() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    ready()
}

/// Whether the published `binance` engine block is on the venue's figure (`Authoritative`), and its equity.
fn engine_state(handle: &CoreHandle) -> Option<(vike_exec::BalanceMode, f64)> {
    handle
        .snapshot()
        .portfolio
        .venues
        .iter()
        .find(|b| b.route_key == "binance")
        .map(|b| (b.balance_mode, b.equity))
}

/// **The start does not return, and so the door does not open, until the venue's balance is on the engine.**
/// The venue answers slowly: while it has not, the mount is still waiting, the door holds the account (and says
/// why) and the engine is on its seed; the moment the venue answers the mount returns, the door lets the account
/// trade, and the engine is on the venue's figure.
///
/// ⚠ KILL PROOF: make `mount_reconcile_driver` spawn the driver and return (the pre-0246 boot): the observer
/// finds the mount already returned and the door not holding the account.
#[test]
fn the_mount_returns_only_when_the_venues_balance_has_reached_the_engine() {
    let handle = core();
    let facts = LeaseFacts::default();
    let (client, release) = Venue::held_back(4_999.625);
    let leg = ReconLeg::sole_account_of("binance", client);
    let returned = AtomicBool::new(false);
    // (the mount was still waiting, the door held the account) as seen half a second into the wait.
    let seen = std::sync::Mutex::new(None::<(bool, bool)>);

    let driver = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(500));
            *seen.lock().expect("the observation") = Some((
                !returned.load(Ordering::SeqCst),
                facts.awaiting_balance("binance").is_some(),
            ));
            release.store(true, Ordering::SeqCst);
        });
        let driver = mount_reconcile_driver(
            &handle,
            vec![leg],
            quick_pass(),
            None,
            Some(&facts),
            &wait_of(Duration::from_secs(30), None),
        );
        returned.store(true, Ordering::SeqCst);
        driver
    });

    assert_eq!(
        *seen.lock().expect("the observation"),
        Some((true, true)),
        "half a second in, the venue had not answered: the start must still be waiting and the door holding \
         the account"
    );
    assert_eq!(facts.awaiting_balance("binance"), None, "the balance landed: the door is open");
    assert!(
        until(10, || {
            engine_state(&handle)
                .is_some_and(|(mode, _)| mode == vike_exec::BalanceMode::Authoritative)
        }),
        "the engine never took the venue's balance: {:?}",
        engine_state(&handle)
    );
    let (_, equity) = engine_state(&handle).expect("published");
    assert!((equity - 4_999.625).abs() < 1e-6, "the venue's figure, not the seed: {equity}");
    driver.shutdown();
    handle.shutdown_and_join();
}

/// **A failed balance read keeps THAT account refused, by name and with the venue's words, and costs the other
/// account nothing.** Two accounts of one node: the default one's venue answers, the ALT one's balance read fails
/// (a 401). The start returns as soon as the pass has been over both legs (an error is an answer, the bound is for a
/// venue that says nothing), and the door holds ALT with the reason while `binance` is open.
#[test]
fn a_failed_balance_read_refuses_that_account_with_the_reason_and_the_other_account_trades() {
    let handle = core();
    let facts = LeaseFacts::default();
    let legs = vec![
        ReconLeg::sole_account_of("binance", Venue::answering(Ok(Some(4_999.625)))),
        ReconLeg::account(
            "binance",
            "binance#ALT",
            Venue::answering(Err("balance 401 invalid API-key".to_string())),
        ),
    ];

    let started = std::time::Instant::now();
    let driver = mount_reconcile_driver(
        &handle,
        legs,
        quick_pass(),
        None,
        Some(&facts),
        &wait_of(Duration::from_secs(120), None),
    );
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "a read the venue answered with an error does not sit out the bound: the start ends once the pass has been \
         over both legs"
    );

    assert_eq!(facts.awaiting_balance("binance"), None, "the account whose balance landed trades");
    let held = facts.awaiting_balance("binance#ALT").expect("the failed read keeps ALT refused");
    assert!(
        held.why.contains("balance fetch failed: balance 401 invalid API-key"),
        "the venue's words reach the door: {held:?}"
    );
    driver.shutdown();
    handle.shutdown_and_join();
}

/// **A stop request ends the start's wait at once** (a SIGTERM during the boot must not sit out the bound), and
/// leaves the door holding the account the venue never answered.
#[test]
fn a_stop_request_ends_the_wait_and_the_door_keeps_holding_the_account() {
    let handle = core();
    let facts = LeaseFacts::default();
    let (client, release) = Venue::held_back(4_999.625);
    let leg = ReconLeg::sole_account_of("binance", client);
    let stop = AtomicBool::new(false);
    let returned = AtomicBool::new(false);
    let waiting_when_stopped = AtomicBool::new(false);

    let started = std::time::Instant::now();
    let driver = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            waiting_when_stopped.store(!returned.load(Ordering::SeqCst), Ordering::SeqCst);
            stop.store(true, Ordering::SeqCst);
        });
        let driver = mount_reconcile_driver(
            &handle,
            vec![leg],
            quick_pass(),
            None,
            Some(&facts),
            &wait_of(Duration::from_secs(300), Some(&stop)),
        );
        returned.store(true, Ordering::SeqCst);
        driver
    });

    assert!(waiting_when_stopped.load(Ordering::SeqCst), "premise: the start was waiting");
    assert!(started.elapsed() < Duration::from_secs(60), "the stop request did not end the wait");
    assert!(facts.awaiting_balance("binance").is_some(), "the account is still held");
    release.store(true, Ordering::SeqCst);
    driver.shutdown();
    handle.shutdown_and_join();
}

/// A mount with no reconcile leg (nothing armed, every venue paper) waits for nothing and announces nothing.
#[test]
fn a_mount_with_no_leg_waits_for_nothing_and_holds_nothing() {
    let handle = core();
    let facts = LeaseFacts::default();
    let started = std::time::Instant::now();
    let driver = mount_reconcile_driver(
        &handle,
        Vec::new(),
        quick_pass(),
        None,
        Some(&facts),
        &wait_of(Duration::from_secs(300), None),
    );
    assert!(started.elapsed() < Duration::from_secs(60), "nothing to wait for");
    assert_eq!(facts.awaiting_balance("binance"), None);
    driver.shutdown();
    handle.shutdown_and_join();
}
