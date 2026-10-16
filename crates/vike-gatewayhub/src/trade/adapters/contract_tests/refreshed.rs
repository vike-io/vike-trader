//! **`RefreshedTrade` reads its source at the steps a tradehub restart drives, and a start runs on what
//! that read returned** (W9c): the table the tradehub opens with, the preflight, the tier check of a
//! `TradeOpen` and the `AdapterStart`; never at the bookkeeping that follows a start.

use std::sync::atomic::AtomicUsize;

use vike_bridge_core::venue_adapter::{ExecOutcome, IdentityReport, LiveExec};
use vike_bridge_core::venue_adapter_fixture::PLANTED_DECLARATION;
use vike_gatewayhub_client::proto::{AdapterStart, CredentialLeg, LegResult, PreflightStep};

use super::*;
use crate::trade::adapters::refreshed::RefreshedTrade;
use crate::trade::lane::{TradeLane, TradeTarget};

/// What the bridge saw of the deployment map at its last `start`.
static SEEN: Mutex<Vec<Option<String>>> = Mutex::new(Vec::new());

/// A venue whose `start` records the `MARK` entry of the map it was handed, and comes up empty.
struct Seeing;

impl VenueAdapter for Seeing {
    fn venue(&self) -> &'static str {
        "ig"
    }
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> Resolution {
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None }
    }
    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome {
        SEEN.lock().expect("the log").push(req.inputs.secrets.get("MARK").cloned());
        AdapterOutcome::paper()
    }
}

static SEEING: Seeing = Seeing;
static SEEING_REG: [VenueRow; 1] = [VenueRow::Adapter(&SEEING)];

fn start_args() -> AdapterStart {
    AdapterStart {
        rid: 1,
        symbol: "EURUSD".into(),
        declared_legs: Vec::new(),
        recon_enabled: false,
        takes_trigger: false,
        risk: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::Admit,
        withhold: None,
        route_key: "ig".into(),
    }
}

/// Each of the four steps reads the source once; the three bookkeeping verbs read it never; and the
/// bridge's `start` is handed the map that step's read returned, not an older one.
#[test]
fn every_step_a_tradehub_restart_drives_reads_the_source_and_a_start_runs_on_that_read() {
    static READS: AtomicUsize = AtomicUsize::new(0);
    let source: crate::trade::adapters::refreshed::TradeSource = Box::new(|| {
        let n = READS.fetch_add(1, Ordering::SeqCst);
        let mut vars = HashMap::new();
        vars.insert("MARK".to_string(), format!("generation-{n}"));
        (vars, snapshot_of(&policy("ig", AccountTier::Demo)))
    });
    let lane = RefreshedTrade::new(&SEEING_REG, source);
    let reads = || READS.load(Ordering::SeqCst);
    assert_eq!(reads(), 1, "the first generation is read when the lane is built");

    assert_eq!(lane.account_tier("ig", None), Ok(AccountTier::Demo));
    assert_eq!(reads(), 2, "the tier check of a TradeOpen reads the source");
    let _ = lane.arming_table();
    assert_eq!(reads(), 3, "the arming table reads the source");
    let _ = lane.preflight(&[], PreflightStep::Plan);
    assert_eq!(reads(), 4, "the preflight's plan reads the source");
    let _ = lane.preflight(&[], PreflightStep::Clock);
    let _ = lane.preflight(&[], PreflightStep::Credential);
    assert_eq!(
        reads(),
        4,
        "the later steps of a preflight read nothing: they run on the current generation (the plan's, while no other verb refreshed in between)"
    );

    let (tx, _rx) = vike_exec::event_channel(8);
    let target = TradeTarget { venue: "ig".to_string(), account: None, tier: AccountTier::Demo };
    let refused = lane.start_adapter(&target, &start_args(), tx, None);
    assert!(refused.is_err(), "the planted venue comes up empty");
    assert_eq!(reads(), 5, "the start reads the source");
    assert_eq!(
        SEEN.lock().expect("the log").as_slice(),
        [Some("generation-4".to_string())],
        "the bridge ran on the map that start's own read returned"
    );

    let _ = lane.venues();
    let _ = lane.resolves_by_coid("ig");
    let _ = lane.recon_degraded("ig");
    assert_eq!(reads(), 5, "bookkeeping on a start already made reads nothing");
}

/// A venue that comes UP and states which book the account trades (hyperliquid's `userRole`).
struct Confirming;

impl VenueAdapter for Confirming {
    fn venue(&self) -> &'static str {
        "ig"
    }
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }
    }
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> Resolution {
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None }
    }
    fn start(&self, _req: AdapterRequest<'_>) -> AdapterOutcome {
        AdapterOutcome {
            exec: ExecOutcome::Live(LiveExec {
                client: Box::new(crate::trade::adapters::testutil::NoopClient),
                bound_tier: AccountTier::Demo,
                grid: None,
                contract_size: None,
                margin_mode: None,
                leg_grids: Vec::new(),
            }),
            recon: None,
            identity: Some(IdentityReport {
                book: "the-confirmed-book".to_string(),
                evidence: "`userRole`",
                tier: AccountTier::Demo,
            }),
        }
    }
}

static CONFIRMING: Confirming = Confirming;
static CONFIRMING_REG: [VenueRow; 1] = [VenueRow::Adapter(&CONFIRMING)];

/// The book a venue confirmed at a start waits for `record_identity` (the tradehub makes it after the fee
/// schedule); a refresh between the two (another account's `TradeOpen`, a table read) must not lose it.
#[test]
fn a_confirmed_book_outlives_the_refresh_between_the_start_and_the_identity_record() {
    let lane = RefreshedTrade::new(
        &CONFIRMING_REG,
        Box::new(|| (HashMap::new(), snapshot_of(&policy("ig", AccountTier::Demo)))),
    );
    let (tx, _rx) = vike_exec::event_channel(8);
    let target = TradeTarget { venue: "ig".to_string(), account: None, tier: AccountTier::Demo };
    let started =
        lane.start_adapter(&target, &start_args(), tx, None).expect("the planted venue comes up");
    assert_eq!(lane.current_confirmed_books(), 1, "the start parked the venue's confirmed book");
    let _ = lane.arming_table();
    let _ = lane.account_tier("ig", None);
    assert_eq!(lane.current_confirmed_books(), 1, "…and two refreshes later it is still waiting");
    drop(started);
}

/// **A refresh between a preflight's plan and a later step does not strand the step** (review 15,
/// finding 2). The later steps run on the CURRENT generation, and another verb (a second tradehub's tier
/// check here) replaces it after the plan built the credential probes: the new generation has none, so the
/// question must rebuild them from its own snapshot rather than answer "no credential probe was planned".
/// That words an unbuilt table and would read, at the tradehub, as a refusal of a venue nobody refused.
/// The mix of snapshots is documented in `RefreshedTrade`'s module doc.
#[test]
fn a_refresh_between_the_plan_and_a_credential_question_does_not_strand_the_question() {
    let lane = RefreshedTrade::new(
        &SEEING_REG,
        Box::new(|| (HashMap::new(), snapshot_of(&policy("ig", AccountTier::Demo)))),
    );
    let venues = vec!["ig".to_string()];
    let planned = lane.preflight(&venues, PreflightStep::Plan);
    assert!(planned.plan.is_some(), "the plan step answers the venue lists");

    // Another verb refreshes the lane: the plan's generation is no longer the current one.
    assert_eq!(lane.account_tier("ig", None), Ok(AccountTier::Demo));

    let legs = lane.preflight(&venues, PreflightStep::Credential).legs;
    assert!(
        matches!(&legs[..], [LegResult::Credential { venue, leg: CredentialLeg::Rejected(why), .. }]
            if venue == "ig" && !why.contains("was planned")),
        "the question rebuilt its probes on the new generation: {legs:?}"
    );
}
