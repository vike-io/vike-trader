//! **The gateway twin: one scripted venue, driven by the same orders through the OLD in-process path and
//! through the HOP, must leave the same core behind** (wave 2, W19; drift register R9, R10, R11).
//!
//! Both runs spawn a real `vike_core` core over one engine and a journal, and feed it through the
//! `live-event-forward` relay the tradehub composes (`tests/support/hop_rig.rs`'s `CoreRig`). The venue is
//! one scripted [`World`] whose whole behaviour is one command loop, run under the shared `ExecActor`:
//!
//! * **in process** (what the tradehub was before the move): the engine's exec client IS that `ExecActor`,
//!   the venue's events go straight into the core's lane as `Ingest::Event`;
//! * **over the hop** (what it is now): the engine's exec client is the `RemoteExecClient` the tradehub's
//!   own `RemoteBackend` returns from `start_adapter`, dialled over TCP to a REAL gatewayhub server
//!   (`vike_gatewayhub::server::accept::serve_with_trade`) whose lane runs the very same `ExecActor` over the
//!   very same loop; every event takes the ring, the trade link and the client's reader, and reaches the
//!   core as `Ingest::GatewayEvent` with its ring position.
//!
//! The script is sequential (each order waits for the core to reach the state before the next command), so
//! the two runs see the same messages in the same order, and a fixed core clock makes the journals comparable
//! record by record. What must be equal:
//!
//! * **the event stream** the core's lane received (R9: every variant bit-exact, in order, nothing added,
//!   nothing lost, `route_key` stamped once);
//! * **the order FSM end states**: status, filled quantity, average fill price, venue order id of every
//!   order, and the account's balance and positions;
//! * **the journal, modulo the cursor stamp** (R10, v18): every record of the hop's journal equals the
//!   in-process one once an `Ingest::GatewayEvent` is read as the `Ingest::Event` it wraps and the snapshot's
//!   `gateway_cursors` are dropped; and the hop's journal DOES carry the cursors (so the comparison is not
//!   between two journals that both say nothing about the hop);
//! * **funding exactly once** (R11): the funding payment the venue makes after one fill folds once, in both
//!   runs, and into the same balance.
//!
//! Already covered elsewhere and reused, not repeated: the wire's variant coverage
//! (`crates/vike-gatewayhub-client/tests/trade_event_roundtrip.rs`, a proptest over all 20 variants), the
//! journal's cursor rules (`crates/vike-core/tests/journal/gateway_cursor.rs`), the ring and the replay
//! (`crates/vike-gatewayhub/tests/trade_ring.rs`).

use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use vike_core::{CoreConfig, JournalConfig};
use vike_exec::Ingest;
use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord};
use vike_model::OrderStatus;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub::remote_backend::RemoteBackend;
use vike_tradehub_core::{AdapterStartQuery, VenueBackend};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/hop_rig.rs"]
mod hop_rig;

use hop_rig::{
    CoreRig, Fate, Hop, SYMBOL, VENUE, World, accounts, accounts_with, arming, fast_trade_timings,
    live_lane, order, start_query_for,
};

const WAIT: Duration = Duration::from_secs(10);

/// The route key of the account appended late.
const ALT_KEY: &str = "binance#ALT";

/// Everything one run leaves behind that the other must equal.
#[derive(Debug)]
struct Outcome {
    /// Every order the core holds, in full (status, quantities, prices, venue order id).
    orders: Vec<String>,
    /// The account's balance, bit for bit.
    balance_bits: u64,
    /// The account's positions.
    positions: String,
    /// The venue events the core's lane received, as JSON, in arrival order.
    events: Vec<String>,
    /// The journal, one JSON line per record, with a gateway event read as the event it wraps.
    journal: Vec<String>,
    /// How many journal records were gateway events (the cursor stamp that was normalised away).
    gateway_records: usize,
    /// How many gateway events carried a cursor the relay saw.
    cursors_seen: usize,
    /// Whether the core said it STOPPED the account appended late (decision 0169, slice S2).
    stop_said: bool,
    /// How many engine blocks the core publishes at the end: the default account's, and the one appended late
    /// and stopped again (decision 0169, slices S1 and S2: a stop tombstones, it removes nothing).
    engine_blocks: usize,
}

fn core_config(dir: &Path) -> CoreConfig {
    CoreConfig {
        seed_cash: 10_000.0,
        // A FIXED clock: every message dispatches at the same instant in both runs, so a record's `now_ms`
        // cannot differ for a reason that is not the hop. Nothing in this script waits on time.
        clock: Box::new(|| 1_700_000_000_000_i64),
        coid_session: Some(("twin".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1 << 20, flush_every: 8 },
            snapshot_every: 5,
        }),
        ..CoreConfig::default()
    }
}

/// The script: A rests, B fills in two parts and pays funding, C is refused, A is repriced then cancelled,
/// D rests. Each step waits for the core to reach the state, so the message order is the script's.
fn drive(rig: &CoreRig, world: &World) {
    world.fate("B", Fate::PartialThenFill);
    world.fate("C", Fate::Reject);
    rig.submit(order("A", 100.0));
    assert!(rig.wait_status("A", OrderStatus::Accepted, WAIT), "A rests: {:?}", rig.orders());
    let before_funding = rig.core().snapshot().balance;
    rig.submit(order("B", 101.0));
    assert!(rig.wait_status("B", OrderStatus::Filled, WAIT), "B fills: {:?}", rig.orders());
    // The funding payment is the LAST event of B's burst: wait for the core to have folded it, or the next
    // command could overtake it on the slower path and the two runs would journal a different order.
    assert!(
        hop_rig::wait_until(WAIT, || rig.core().snapshot().balance != before_funding),
        "the funding payment folded"
    );
    rig.submit(order("C", 102.0));
    assert!(rig.wait_status("C", OrderStatus::Rejected, WAIT), "C refused: {:?}", rig.orders());
    rig.modify("A", None, Some(99.0));
    assert!(
        hop_rig::wait_until(WAIT, || {
            rig.core()
                .snapshot()
                .orders
                .iter()
                .any(|o| o.client_order_id == "A" && o.price == Some(99.0))
        }),
        "A repriced: {:?}",
        rig.core().snapshot().orders
    );
    rig.cancel("A");
    assert!(rig.wait_status("A", OrderStatus::Canceled, WAIT), "A cancelled: {:?}", rig.orders());
    rig.submit(order("D", 103.0));
    assert!(rig.wait_status("D", OrderStatus::Accepted, WAIT), "D rests: {:?}", rig.orders());
}

/// The journal at `dir`, with a gateway event read as the event it wraps and the snapshots' cursors dropped.
fn normalised_journal(dir: &Path) -> (Vec<String>, usize) {
    let mut gateway = 0;
    let lines = CommandJournal::read_all(dir)
        .expect("the journal reads")
        .into_iter()
        .map(|mut rec| {
            match &mut rec {
                JournalRecord::Cmd { msg, .. } => {
                    if let Ingest::GatewayEvent(g) = msg {
                        gateway += 1;
                        let event = g.event.clone();
                        *msg = Ingest::Event(event);
                    }
                }
                JournalRecord::Snap { gateway_cursors, .. } => gateway_cursors.clear(),
                _ => {}
            }
            serde_json::to_string(&rec).expect("a record serialises")
        })
        .collect();
    (lines, gateway)
}

fn outcome(mut rig: CoreRig, dir: &Path) -> Outcome {
    let snap = rig.core().snapshot();
    let mut orders: Vec<String> = snap.orders.iter().map(|o| format!("{o:?}")).collect();
    orders.sort();
    let balance_bits = snap.balance.to_bits();
    let positions = format!("{:?}", snap.positions);
    let engine_blocks = snap.portfolio.venues.len();
    let stop_said = snap.recent_events.iter().any(|l| l.contains("ACCOUNT STOPPED"));
    rig.shutdown();
    let events = rig
        .relayed
        .lock()
        .expect("the relay's record")
        .iter()
        .map(|e| serde_json::to_string(e).expect("an event serialises"))
        .collect();
    let cursors_seen = rig.cursors.lock().expect("the relay's cursors").len();
    let (journal, gateway_records) = normalised_journal(dir);
    Outcome {
        orders,
        balance_bits,
        positions,
        events,
        journal,
        gateway_records,
        cursors_seen,
        stop_said,
        engine_blocks,
    }
}

/// Stop the account appended late, the way the account watcher does, and wait for the core to say so.
fn stop_the_late_account(rig: &CoreRig) {
    rig.core()
        .event_sender()
        .stop_account_engine(vike_exec::NotRunningAccount {
            venue: VENUE.to_string(),
            account: Some(AccountLabel::parse("ALT").expect("a legal label")),
            route_key: ALT_KEY.to_string(),
            tier: None,
            reason: "account inactive".to_string(),
        })
        .expect("the core takes the stop");
    assert!(
        hop_rig::wait_until(WAIT, || {
            rig.core().snapshot().recent_events.iter().any(|l| l.contains("ACCOUNT STOPPED"))
        }),
        "the core stopped the late account"
    );
    rig.core().event_sender().detach_account_engine(ALT_KEY).expect("the core takes the detach");
    assert!(
        hop_rig::wait_until(WAIT, || {
            rig.core().snapshot().recent_events.iter().any(|l| l.contains("ACCOUNT DETACHED"))
        }),
        "and detached its client"
    );
}

fn query(label: &AccountLabel) -> AdapterStartQuery<'_> {
    AdapterStartQuery {
        venue: VENUE,
        account: label,
        tier: vike_model::AccountTier::Demo,
        symbol: SYMBOL,
        declared_legs: &[],
        recon_enabled: true,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::default(),
        withhold: None,
        route_key: VENUE,
        properties_rec: None,
    }
}

/// The old path: the engine drives the venue's `ExecActor` itself.
fn in_process() -> Outcome {
    let dir = tempfile::tempdir().expect("a journal dir");
    let world = World::new();
    let (events, source) = live_lane();
    let client = Box::new(world.actor(events.clone()));
    let rig = CoreRig::start(client, source, core_config(dir.path()));
    drive(&rig, &world);
    // The late account: a second account of the venue appended to the RUNNING core. Its lane is read and
    // dropped (it trades nothing here), so it cannot fold into the first account's book.
    let (alt_events, alt_source) = live_lane();
    alt_source.discard();
    rig.add_account(Box::new(world.actor(alt_events.clone())), ALT_KEY);
    assert!(rig.wait_engines(2, WAIT), "the late account joined the core in process");
    stop_the_late_account(&rig);
    outcome(rig, dir.path())
}

/// The new path: the engine drives the `RemoteExecClient` the tradehub's `RemoteBackend` returns, over TCP,
/// to a real gatewayhub server whose lane runs the same `ExecActor`.
fn over_the_hop() -> Outcome {
    let dir = tempfile::tempdir().expect("a journal dir");
    let world = World::new();
    let hop = Hop::start(std::sync::Arc::clone(&world), arming(false));
    let backend = RemoteBackend::connect(
        hop.link(),
        accounts(),
        "vike-tradehub-twin".to_string(),
        Duration::from_secs(5),
    )
    .with_timings(fast_trade_timings());
    let (events, source) = live_lane();
    let label = AccountLabel::Default;
    let bind = backend.start_adapter(query(&label), &events, None).expect("the adapter starts");
    let rig = CoreRig::start(bind.client, source, core_config(dir.path()));
    drive(&rig, &world);
    // The same late account, started the way the account watcher starts it: over a backend whose arming table
    // is asked for AGAIN (`RemoteBackend::refreshed`), through the real lease and adapter start.
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let fresh = backend.refreshed(accounts_with(&alt), std::collections::HashSet::new());
    let (alt_events, alt_source) = live_lane();
    alt_source.discard();
    let alt_bind = fresh
        .start_adapter(start_query_for(&alt, ALT_KEY), &alt_events, None)
        .expect("the late account's adapter starts over the hop");
    rig.add_account(alt_bind.client, ALT_KEY);
    assert!(rig.wait_engines(2, WAIT), "the late account joined the core over the hop");
    stop_the_late_account(&rig);
    let out = outcome(rig, dir.path());
    // The venue saw exactly the orders that were submitted, once each, over the hop.
    assert_eq!(world.seen_submits(), ["A", "B", "C", "D"], "each submit reached the venue once");
    drop(backend);
    drop(hop);
    out
}

struct Runs {
    process: Outcome,
    hop: Outcome,
}

/// Both runs, computed once for the whole binary.
fn runs() -> &'static Runs {
    static RUNS: OnceLock<Runs> = OnceLock::new();
    RUNS.get_or_init(|| Runs { process: in_process(), hop: over_the_hop() })
}

fn first_difference(a: &[String], b: &[String]) -> String {
    let at = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or_else(|| a.len().min(b.len()));
    format!(
        "first difference at index {at} (lengths {} and {}):\n  in process: {:?}\n  over the hop: {:?}",
        a.len(),
        b.len(),
        a.get(at),
        b.get(at)
    )
}

/// **R9: the core's lane receives the same events, in the same order, bit for bit**, through the hop as it
/// did straight from the venue's thread. Nothing is added by the ring or the client, nothing is lost, the
/// route key is stamped once.
#[test]
fn the_event_stream_the_core_receives_is_the_same_over_the_hop() {
    let runs = runs();
    assert!(
        runs.process.events.len() >= 14,
        "the script produces a real stream (submitted, accepted, fills, funding, reject, modify, cancel): {}",
        runs.process.events.len()
    );
    assert!(
        runs.hop.events == runs.process.events,
        "{}",
        first_difference(&runs.process.events, &runs.hop.events)
    );
}

/// **The order FSM ends in the same place for every order**: status, quantities, prices and the venue's
/// order id, plus the account's balance (funding included) and positions.
#[test]
fn every_order_and_the_account_end_in_the_same_state_over_the_hop() {
    let runs = runs();
    assert_eq!(runs.hop.orders, runs.process.orders, "the order registry");
    assert_eq!(runs.hop.positions, runs.process.positions, "the positions");
    assert_eq!(
        f64::from_bits(runs.hop.balance_bits),
        f64::from_bits(runs.process.balance_bits),
        "the balance"
    );
    // The end states are the script's, so equality is not between two runs that both did nothing.
    let joined = runs.process.orders.join("\n");
    for needle in ["Filled", "Rejected", "Canceled", "Accepted"] {
        assert!(joined.contains(needle), "the script reaches {needle}: {joined}");
    }
}

/// **R10, v18: the journal is the same record for record once the cursor stamp is read away.** And the hop's
/// journal really holds the stamp (a gateway event per venue event, each with the position the relay saw),
/// while the in-process one holds none, so the equality is not vacuous.
#[test]
fn the_journal_is_the_same_modulo_the_cursor_stamp() {
    let runs = runs();
    assert_eq!(runs.process.gateway_records, 0, "the old path journals plain events");
    assert!(
        runs.hop.gateway_records >= 14,
        "every venue event over the hop is journaled as a gateway event: {}",
        runs.hop.gateway_records
    );
    assert_eq!(
        runs.hop.gateway_records, runs.hop.cursors_seen,
        "one cursor per journaled gateway event"
    );
    assert!(
        runs.hop.journal == runs.process.journal,
        "{}",
        first_difference(&runs.process.journal, &runs.hop.journal)
    );
}

/// **R11: the funding payment folds exactly once** on both paths, into the same balance, and is one record
/// in each journal.
#[test]
fn a_funding_payment_folds_once_over_the_hop() {
    let runs = runs();
    let funding = |o: &Outcome| o.events.iter().filter(|e| e.contains("\"FundingEvent\"")).count();
    assert_eq!(funding(&runs.process), 1, "the venue pays funding once");
    assert_eq!(funding(&runs.hop), 1, "and once over the hop");
    let funding_records =
        |o: &Outcome| o.journal.iter().filter(|l| l.contains("FundingEvent")).count();
    assert_eq!(funding_records(&runs.process), 1);
    assert_eq!(funding_records(&runs.hop), 1);
    assert_eq!(runs.hop.balance_bits, runs.process.balance_bits, "one payment, one balance");
}

/// **Decision 0169, slice S1: an account appended to the RUNNING core leaves the same core over the hop.** Both
/// runs end by adding a second account (in process: a second `ExecActor`; over the hop: the real lease and
/// adapter start over a refreshed backend), and everything above still holds, the journal included (it now
/// carries the `Snap` the core writes at the add, naming both engines).
#[test]
fn a_late_account_add_leaves_the_same_core_over_the_hop() {
    let runs = runs();
    assert_eq!(runs.process.engine_blocks, 2, "the process run publishes both engines");
    assert_eq!(runs.hop.engine_blocks, 2, "and so does the hop run");
    assert!(
        runs.process.journal.iter().filter(|l| l.contains("ALT")).count() >= 1,
        "the journal names the appended engine (its route key rides the Snap)"
    );
}

/// **Decision 0169, slice S2: the account appended late is then STOPPED, and the core is the same over the hop.**
/// The stop cancels nothing (the account holds no order), tombstones the engine (both runs still publish two
/// blocks), says so, and leaves every comparison above equal, the journal's `Snap` included.
#[test]
fn a_late_account_stop_leaves_the_same_core_over_the_hop() {
    let runs = runs();
    assert!(runs.process.stop_said, "the process run says it stopped the account");
    assert!(runs.hop.stop_said, "and so does the hop run");
    assert_eq!(runs.process.engine_blocks, runs.hop.engine_blocks);
    assert_eq!(runs.hop.engine_blocks, 2, "a stop removes no engine");
}

/// The relay's record names real variants (a guard against a script that quietly produced nothing).
#[test]
fn the_stream_carries_the_whole_order_lifecycle() {
    let runs = runs();
    for tag in [
        "OrderSubmitted",
        "OrderAccepted",
        "FillEvent",
        "OrderFilled",
        "OrderRejected",
        "OrderModified",
        "OrderCanceled",
    ] {
        assert!(
            runs.hop.events.iter().any(|e| e.contains(&format!("\"{tag}\""))),
            "the stream carries {tag}"
        );
    }
    // And the events are venue events, not wrappers: no cursor field leaks into the payload.
    assert!(
        !runs.hop.events.iter().any(|e| e.contains("gw_epoch")),
        "a cursor never rides inside the event"
    );
}
