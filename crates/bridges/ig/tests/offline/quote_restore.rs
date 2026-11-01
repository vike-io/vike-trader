//! **A restart keeps the rate row at IG's contract size** (decision 0183): the engine's snapshot writes the
//! BASE contract value of an admitted epic (IG's `contractSize`, 10,000 units a contract) and the rate of its
//! quote currency, never their product, and a restore recomputes the effective value with the function the
//! admission used. The engine's own suites pin the mechanism with a 10x unit (`crates/vike-exec/tests/engine/
//! quote_snapshot.rs`'s `GBPJPY`); this is the same law at the one size IG states, driven through the public
//! engine API with the shapes the gatewayhub's IG answer produces (`IgQuotedFacts` -> `SymbolAdmission`).
//! Nothing here is IG's own figure.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, AdmitOutcome, BalanceMode, DEFAULT_QUOTE_RATE_TTL_MS, ExecutionEngine, RiskGate,
    SymbolAdmission, state_hash,
};
use vike_model::{ContractValue, RiskLimits, SymbolGrid};

const MOUNTED: &str = "CS.D.EURUSD.MINI.IP";
const USDJPY: &str = "CS.D.USDJPY.MINI.IP";
const T0: i64 = 1_000_000;
/// Dollars per yen at 150.00: a FRACTION of a dollar, never 150.
const R: f64 = 1.0 / 150.0;

fn engine() -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, "ig", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "ig",
        MOUNTED,
    );
    e.set_account_ccy("USD").expect("a currency code");
    e.now_ms = T0;
    e
}

/// The admission the node builds from the gatewayhub's IG answer for a yen epic: IG's contract value
/// (linear, 10,000), the pip grid, the currency and the rate with its moment.
fn yen_epic(rate: f64, as_of_ms: i64) -> SymbolAdmission {
    SymbolAdmission {
        route_key: "ig".into(),
        symbol: USDJPY.into(),
        grid: SymbolGrid {
            tick_size: Some(0.01),
            lot_size: Some(0.1),
            min_qty: Some(0.1),
            ..SymbolGrid::default()
        },
        contract_value: Some(ContractValue::linear(10_000.0)),
        initial_mark: None,
        quote_ccy: Some("JPY".into()),
        quote_rate: Some(rate),
        rate_as_of_ms: Some(as_of_ms),
        im_requirement: None,
        release: false,
        margin_mode: None,
    }
}

fn per_unit_bits(e: &ExecutionEngine<RecordingClient>) -> u64 {
    e.account.contract_value_of(USDJPY).per_unit.to_bits()
}

/// Ten contracts of the yen epic at 150.00 are a hundred thousand DOLLARS of notional, before and after a
/// snapshot round trip, and the effective value is the base times the rate to the bit (never the base
/// alone, which would be 15,000,000, and never the rate applied twice).
#[test]
fn a_restore_keeps_ten_thousand_times_the_rate_not_the_base_and_not_the_square() {
    let mut live = engine();
    assert_eq!(live.admit_symbol(&yen_epic(R, T0)), AdmitOutcome::Admitted);
    let want = ContractValue::linear(10_000.0).in_account(R).expect("representable").per_unit;
    assert_eq!(per_unit_bits(&live), want.to_bits());
    let notional = live.account.contract_value_of(USDJPY).order_notional(10.0, 150.0);
    assert!((notional - 100_000.0).abs() < 1e-6, "{notional}");

    let snap = live.snapshot_state();
    let base = snap.account.mult.iter().find(|(s, _)| s == USDJPY).map(|(_, v)| *v);
    assert_eq!(base, Some(10_000.0), "the snapshot writes the BASE contract size");
    let fx = snap.account.fx.as_ref().expect("a foreign epic writes the fx table");
    assert_eq!(fx.account_ccy, "USD");
    assert_eq!(fx.symbols, vec![(USDJPY.to_string(), "JPY".to_string())]);
    assert_eq!(
        fx.rates.iter().map(|r| (r.ccy.as_str(), r.rate, r.as_of_ms)).collect::<Vec<_>>(),
        [("JPY", R, T0)]
    );

    // through the JSON the journal writes, as the node's restart reads it
    let json = serde_json::to_string(&snap).expect("serializes");
    let parsed: vike_exec::EngineSnapshot = serde_json::from_str(&json).expect("parses");
    let back = ExecutionEngine::from_snapshot(&parsed, RecordingClient::default());
    assert_eq!(per_unit_bits(&back), want.to_bits(), "restored to the bit");
    let restored = back.account.contract_value_of(USDJPY).order_notional(10.0, 150.0);
    assert!((restored - 100_000.0).abs() < 1e-6, "{restored}");
    assert_eq!(back.snapshot_state(), snap);
    assert_eq!(state_hash(&[back.snapshot_state()]), state_hash(std::slice::from_ref(&snap)));
}

/// A mount-built engine takes the yen epic back with the snapshot's rate row. The rate is as old as the
/// snapshot, so past the engine's window it reads STALE (an opening order would be denied, a reducing one
/// passes), and the node's re-admission, which carries a fresh rate and the unchanged contract size, lifts it
/// without disturbing the base.
#[test]
fn a_restart_brings_the_rate_row_back_stale_and_a_readmission_lifts_it() {
    let mut live = engine();
    assert_eq!(live.admit_symbol(&yen_epic(R, T0)), AdmitOutcome::Admitted);
    let snap = live.snapshot_state();

    let boot_ms = T0 + DEFAULT_QUOTE_RATE_TTL_MS + 5_000;
    let mut booted = engine();
    booted.now_ms = boot_ms;
    booted.restore_runtime_state(&snap);
    assert!(booted.accepts_symbol(USDJPY), "the epic came back");
    assert_eq!(booted.account.quote_ccy_of(USDJPY), Some("JPY"));
    assert_eq!(booted.account.quote_rate_of(USDJPY).to_bits(), R.to_bits(), "the snapshot's rate");
    assert_eq!(per_unit_bits(&booted), per_unit_bits(&live));
    let (_, rows) = booted.account.quote_rate_table(boot_ms);
    assert!(rows.iter().any(|row| row.ccy == "JPY" && row.stale), "{rows:?}");

    // the node's re-admission: a fresh rate (the yen is a touch weaker), IG's contract size unchanged
    let r2 = 1.0 / 150.50;
    booted.now_ms = boot_ms + 1_000;
    assert_eq!(booted.admit_symbol(&yen_epic(r2, boot_ms + 1_000)), AdmitOutcome::Refreshed);
    let want = ContractValue::linear(10_000.0).in_account(r2).expect("representable").per_unit;
    assert_eq!(per_unit_bits(&booted), want.to_bits());
    let (_, rows) = booted.account.quote_rate_table(boot_ms + 1_000);
    assert!(rows.iter().any(|row| row.ccy == "JPY" && !row.stale), "{rows:?}");
}
