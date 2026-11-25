//! `OrderBroker::contract_value` on a DECLARED mount: every leg answers the value its ACCOUNT books it by,
//! so a strategy that folds its own PnL (the position executor) and the ledger cannot disagree.
//!
//! FAILS BEFORE `LiveBroker::leg_values`: a named leg on a declared mount read `None`, so a
//! `ControllerHarness` executor folded a 50-point future at a multiplier of 1.

use vike_execution::{Command, SymbolAdmission};
use vike_model::{ContractValue, SymbolGrid, TripleBarrier};
use vike_strategy::{ControllerHarness, PositionExecutorOutcome};
use vike_strategy_lab::takers::momentum::MomentumController;

use super::*;

/// Leg B is a 50-point future: one unit of its quantity is worth 50 of its price.
const FUTURE: ContractValue = ContractValue::linear(50.0);
/// Leg C is inverse: one unit is 100 of its quote currency.
const INVERSE: ContractValue = ContractValue::inverse(100.0);
/// Neither declared by the mount nor known to the account.
const NOWHERE: &str = "XRPUSDT";

/// Leg A at the account's default unit, leg B a future and leg C inverse — as the ACCOUNT books them.
fn valued_engine(client: ProbeClient) -> Engine<ProbeClient> {
    let mut e = engine(VENUE, LEG_A, client);
    e.account.admit_contract_value(LEG_B, FUTURE);
    e.account.admit_contract_value(LEG_C, INVERSE);
    e
}

/// Mounted on leg A, declaring legs B and C on the same venue.
fn mount_of(strategy: Box<dyn Strategy<LiveBroker> + Send>) -> Mount {
    Mount {
        order_client_id_code: None,
        account: None,
        symbols: vec![MountLeg::same_venue(LEG_B), MountLeg::same_venue(LEG_C)],
        controller_id: None,
        underlying_symbol: None,
        venue: VENUE.into(),
        symbol: LEG_A.into(),
        interval: INTERVAL.into(),
        strategy,
    }
}

type Reads = [Option<ContractValue>; 4];

/// What `contract_value` answered for legs A, B, C and an undeclared name, on every bar.
struct Values(Arc<Mutex<Vec<Reads>>>);

impl Strategy<LiveBroker> for Values {
    fn on_bar(&mut self, b: &mut LiveBroker, _bar: &Bar) {
        let reads = [LEG_A, LEG_B, LEG_C, NOWHERE].map(|s| OrderBroker::contract_value(&*b, s));
        self.0.lock().unwrap().push(reads);
    }
}

fn admit_leg_b_at(value: ContractValue) -> Command {
    Command::AdmitSymbol(Box::new(SymbolAdmission {
        route_key: VENUE.into(),
        symbol: LEG_B.into(),
        grid: SymbolGrid { tick_size: Some(0.01), lot_size: Some(0.001), ..SymbolGrid::default() },
        contract_value: Some(value),
        initial_mark: None,
        initial_mark_ms: None,
        quote_currency: None,
        quote_rate: None,
        rate_as_of_ms: None,
        initial_margin_requirement: None,
        release: false,
        margin_mode: None,
    }))
}

/// Each declared leg reads its account's own value on the mount's bar AND on a leg's bar: linear 1
/// for leg A, the 50-point future for leg B, the inverse value for leg C, and `None` for a symbol the
/// mount never declared. An admission that restates leg B's value is what the next dispatch reads.
#[test]
fn every_declared_leg_reads_the_value_its_account_books_it_by() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let w = wires();
    let probe = Box::new(Values(Arc::clone(&seen)));
    let handle = spawn_core(valued_engine(w.client), config(mount_of(probe)));
    close_bar(&handle, LEG_A, 60_000, PRICE_A);
    close_bar(&handle, LEG_B, 120_000, PRICE_B);
    // One FIFO lane: the admission folds before the next bar dispatches.
    handle.send_command(admit_leg_b_at(ContractValue::linear(25.0)));
    close_bar(&handle, LEG_A, 180_000, PRICE_A);
    handle.shutdown_and_join();

    let seen = seen.lock().unwrap();
    let declared = [Some(ContractValue::linear(1.0)), Some(FUTURE), Some(INVERSE), None];
    assert_eq!(seen.len(), 3, "three bars reach the mount: {seen:?}");
    assert_eq!(seen[0], declared, "on the mount's own bar");
    assert_eq!(seen[1], declared, "on leg B's bar");
    assert_eq!(
        seen[2],
        [Some(ContractValue::linear(1.0)), Some(ContractValue::linear(25.0)), Some(INVERSE), None],
        "an admission that restates a leg's value re-resolves the table"
    );
}

/// What `instrument_grid` answered for legs B and C on every bar.
struct Grids(Arc<Mutex<Vec<[Option<vike_model::Grid>; 2]>>>);

impl Strategy<LiveBroker> for Grids {
    fn on_bar(&mut self, b: &mut LiveBroker, _bar: &Bar) {
        let reads = [LEG_B, LEG_C].map(|s| OrderBroker::instrument_grid(&*b, s));
        self.0.lock().unwrap().push(reads);
    }
}

/// A declared LEG reads the grid its engine's gate rounds its orders on (one broker plan, step 12: a
/// leg read `None`, so the order language could not snap a leg's level or count a trail on it): none
/// while the gate holds no row of its own for the leg, the row once an admission installs it, on the
/// mount's bar and on the leg's.
#[test]
fn a_declared_leg_reads_the_grid_its_engine_rounds_it_on() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let w = wires();
    let probe = Box::new(Grids(Arc::clone(&seen)));
    let handle = spawn_core(valued_engine(w.client), config(mount_of(probe)));
    close_bar(&handle, LEG_A, 60_000, PRICE_A);
    handle.send_command(admit_leg_b_at(ContractValue::linear(25.0)));
    close_bar(&handle, LEG_B, 120_000, PRICE_B);
    handle.shutdown_and_join();

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "two bars reach the mount: {seen:?}");
    assert_eq!(seen[0], [None, None], "no row of its own for either leg yet");
    let ticks = seen[1][0].map(|g| (g.tick_size, g.lot_size));
    assert_eq!(ticks, Some((Some(0.01), Some(0.001))), "leg B's admitted row: {seen:?}");
    assert_eq!(seen[1][1], None, "leg C still holds no row of its own");
}

/// The live core's `ControllerHarness`, observed: its terminal outcomes copied out after each hook.
struct Recorded {
    inner: ControllerHarness<MomentumController>,
    outcomes: Arc<Mutex<Vec<PositionExecutorOutcome>>>,
}

impl Recorded {
    fn publish(&self) {
        *self.outcomes.lock().unwrap() = self.inner.outcomes().to_vec();
    }
}

impl Strategy<LiveBroker> for Recorded {
    fn on_bar(&mut self, b: &mut LiveBroker, bar: &Bar) {
        self.inner.on_bar(b, bar);
        self.publish();
    }
    fn on_fill(&mut self, b: &mut LiveBroker, f: &Fill) {
        self.inner.on_fill(b, f);
        self.publish();
    }
    fn on_order_event(&mut self, b: &mut LiveBroker, event: &OrderEvent) {
        self.inner.on_order_event(b, event);
    }
}

/// A long round trip on the 50-point future leg, entered at 50 and closed at 55: the executor
/// realizes 50 × (55 − 50) × 1 = 250, the ledger's number, not the 5 it folded at a unit of 1.
#[test]
fn the_harness_values_a_future_legs_pnl_at_its_account_value() {
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    // Momentum threshold 0: the second leg-B bar opens long; take-profit +5 closes it.
    let controller =
        MomentumController::new(1.0, 0.0, TripleBarrier::new(Some(5.0), None, None, None));
    let recorded = Recorded {
        inner: ControllerHarness::new(controller, VENUE, 10_000_000),
        outcomes: Arc::clone(&outcomes),
    };
    let w = wires();
    let handle = spawn_core(valued_engine(w.client), config(mount_of(Box::new(recorded))));
    let event = handle.event_sender();

    close_bar(&handle, LEG_B, 60_000, 50.0); // the momentum reference
    close_bar(&handle, LEG_B, 120_000, 51.0); // +1: the entry market order
    let order_client_ids = wait_for_submissions(&w.submissions, 1);
    event
        .blocking_send(Event::Fill(fill(&order_client_ids[0], VENUE, LEG_B, 1, 1.0, 50.0)))
        .unwrap();
    close_bar(&handle, LEG_B, 180_000, 56.0); // through the 55 target: the close market order
    let order_client_ids = wait_for_submissions(&w.submissions, 2);
    event
        .blocking_send(Event::Fill(fill(&order_client_ids[1], VENUE, LEG_B, -1, 1.0, 55.0)))
        .unwrap();
    handle.shutdown_and_join();

    let outcomes = outcomes.lock().unwrap();
    assert_eq!(outcomes.len(), 1, "one closed round trip on leg B: {outcomes:?}");
    assert_eq!((outcomes[0].entry_price, outcomes[0].exit_price), (50.0, 55.0));
    assert_eq!(outcomes[0].realized_pnl, 250.0, "50 × (55 − 50) × 1, at the account's value");
}
