//! **The reconcile manager stamps each pass with the admitted symbols its client was told about, and the
//! core's restore step judges only what that client reads** (decisions 0173 and 0174).
//!
//! The pieces are proven where they live (`ReconClient::admitted_symbols` in `vike-exec`, the lease's
//! list in `vike-gatewayhub-client`, the restore scope in `vike-core`); this is the seam between them,
//! through a real spawned core and the real manager: an ownership file that remembers an order on an
//! ADMITTED symbol (`ETHUSDT`, not the engine's mounted `BTCUSDT`), a client that reads nothing at the
//! venue (so every restored order is absent from its reports), and the one thing that differs between
//! the two tests, what that client says it was told.
//!
//! * told: the venue's silence is the venue's word, the order is GONE;
//! * not told: the silence says nothing about a symbol nobody fetched, the order is HELD.
//!
//! The order belongs to a mount that is not mounted (a `pending_owners` entry), which is judged by the
//! same scope and needs no strategy: a gone verdict is a note, a held one is another.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use vike_core::order_owners::{ORDER_OWNERS_FILE, OrderOwnerLog};
use vike_core::{CoreConfig, CoreHandle, spawn_core};
use vike_exec::recon::{ReconClient, ReconPolicy};
use vike_exec::testing::RecordingClient;
use vike_exec::{Command, SymbolAdmission};
use vike_model::{FillReport, OrderStatusReport, PositionStatusReport, SymbolGrid};
use vike_tradehub_core::{ReconConfig, ReconLeg, spawn_recon};

use crate::kit::engines::dyn_engine;

const NOW: i64 = 1_700_000_000_000;
const COID: &str = "co-eth-1";

/// A client whose venue holds nothing, and that says it was told `admitted`.
struct Listed {
    admitted: Option<Vec<String>>,
}

impl ReconClient for Listed {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(Vec::new())
    }
    fn admitted_symbols(&self) -> Option<Vec<String>> {
        self.admitted.clone()
    }
}

/// A core over a state directory whose ownership file remembers one `q` order on `ETHUSDT`, with the
/// symbol admitted into the engine; the reconcile driver is running over [`Listed`].
fn run_one_pass(admitted: Option<Vec<String>>) -> (CoreHandle, tempfile::TempDir, Vec<String>) {
    let dir = tempfile::Builder::new().prefix("vike-recon-admitted-scope-").tempdir().unwrap();
    let line = format!(
        "{{\"kind\":\"own\",\"coid\":\"{COID}\",\"mount_id\":\"ghost_mount\",\"ts\":{NOW},\
         \"venue\":\"binance\",\"symbol\":\"ETHUSDT\",\"tag\":\"q\"}}\n"
    );
    std::fs::write(dir.path().join(ORDER_OWNERS_FILE), line).unwrap();

    let t = Arc::new(AtomicI64::new(NOW));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.load(Ordering::Relaxed)),
        order_owners: Some(OrderOwnerLog::in_state_dir(dir.path())),
        restore_venue_gate: |_| true,
        ..CoreConfig::default()
    };
    let handle =
        spawn_core(dyn_engine("binance", "BTCUSDT", Box::new(RecordingClient::default())), config);
    handle.send_command(Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: "binance".into(),
        symbol: "ETHUSDT".into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.001), ..Default::default() },
        contract_value: None,
        initial_mark: None,
        quote_ccy: None,
        quote_rate: None,
        rate_as_of_ms: None,
        im_requirement: None,
    })));
    let driver = spawn_recon(
        &handle,
        vec![ReconLeg::sole_account_of("binance", Box::new(Listed { admitted }))],
        ReconConfig {
            policy: ReconPolicy::default(),
            lookback_ms: 60_000,
            startup_delay: Duration::from_millis(0),
            ..ReconConfig::default()
        },
        None,
    );
    let cell = handle.snapshot_cell();
    let deadline = Instant::now() + Duration::from_secs(5);
    let notes = loop {
        let notes: Vec<String> = cell
            .load_full()
            .recent_events
            .iter()
            .filter(|l| l.contains("RESTORE binance"))
            .map(ToString::to_string)
            .collect();
        if !notes.is_empty() || Instant::now() > deadline {
            break notes;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    driver.shutdown();
    (handle, dir, notes)
}

#[test]
fn a_client_that_was_told_the_symbol_has_its_silence_read_as_gone() {
    let (handle, dir, notes) = run_one_pass(Some(vec!["ETHUSDT".to_string()]));
    handle.shutdown_and_join();
    assert!(notes.iter().any(|l| l.contains("gone 1") && l.contains(COID)), "{notes:?}");
    let file = std::fs::read_to_string(dir.path().join(ORDER_OWNERS_FILE)).unwrap();
    assert!(file.contains("forget"), "the file forgot it: {file}");
}

#[test]
fn a_client_that_was_not_told_the_symbol_has_its_orders_held_not_retired() {
    let (handle, dir, notes) = run_one_pass(Some(Vec::new()));
    handle.shutdown_and_join();
    assert!(notes.iter().any(|l| l.contains("HELD") && l.contains(COID)), "{notes:?}");
    assert!(!notes.iter().any(|l| l.contains("gone 1")), "never retired: {notes:?}");
    let file = std::fs::read_to_string(dir.path().join(ORDER_OWNERS_FILE)).unwrap();
    assert!(!file.contains("forget"), "the entry survives: {file}");
}

/// A client that keeps no list (every venue but the three that admit symbols) is judged by the
/// engine's own symbols, exactly as before the claim existed.
#[test]
fn a_client_with_no_list_is_judged_by_the_engines_symbols_as_before() {
    let (handle, _dir, notes) = run_one_pass(None);
    handle.shutdown_and_join();
    assert!(notes.iter().any(|l| l.contains("gone 1")), "{notes:?}");
}
