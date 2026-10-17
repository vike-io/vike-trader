//! **The startup preflight report is the same over the gatewayhub hop as in process** (wave 2, W11b).
//!
//! A refactor changes no working behaviour: the go/no-go table an operator reads before the first
//! live order must not change because the venue half moved into a daemon. Each case here renders the
//! report TWICE from one scripted world of venues, through the keep half's own `run_preflight_gated`:
//!
//! * **in process**: a [`VenueBackend`] answering the four preflight verbs straight from the script
//!   (what `BridgeTrade` behind the in-process backend answered before the move), and
//! * **over the hop**: [`RemoteBackend`] against a real gatewayhub server on loopback whose scripted
//!   trade lane answers the way `BridgeTrade` does (the wire's mirror DTOs, built by the lane's own
//!   `probe::*_result` mapping), the same script behind it.
//!
//! and requires `PreflightReport::lines()` (the text, status and remedy of every row), `go()` and the
//! per-venue dispositions to be BYTE-IDENTICAL. Time is a fake clock both sides read, advanced only by
//! the script (a venue read "takes" its `elapsed_ms`; the daemon's lane advances the SAME clock while the
//! fold waits on it), so a round trip, a resample and a budget are exact on both sides. The clock starts
//! far from the wall clock on purpose: a backend that stamped a reading with its own `now` would be
//! billions of ms out and fail loudly.
//!
//! What it holds, one test each: the clock risk class and its remedy (a nonce-window venue is never
//! FAIL-capable), the reason of a declared clock gap, the resample and the round trip, a read that
//! fails after a good sample, the network leg's round count and down-since, an unreachable gatewayhub
//! (a WARN on every row, a green one on none), and the whole report. The last case runs the REAL
//! `run_startup_preflight` (whose probes read the wall clock) and compares every verdict, remedy and
//! number-free row.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_bridge_core::venue_adapter::ClockRisk;
use vike_config::ArmingBlock;
use vike_gatewayhub::testkit::data::ScriptedData;
use vike_gatewayhub::testkit::gateway::LoopbackGateway;
use vike_gatewayhub::testkit::rest::ScriptedRest;
use vike_gatewayhub::testkit::trade::ScriptedTrade;
use vike_gatewayhub::trade::adapters::probe as daemon;
use vike_gatewayhub_client::proto as wire;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub::remote_backend::RemoteBackend;
use vike_tradehub_core::preflight::{
    CANARY_CLOCK_WARN_MS, CheckStatus, ClockPolicy, CredentialGap, DEFAULT_CLOCK_FAIL_MS,
    DEFAULT_CLOCK_WARN_MS, FnProbes, PreflightConfig, PreflightReport, ServerTimeGap,
    clock_budget_for, credential_budget_for, run_preflight_gated,
};
use vike_tradehub_core::startup::run_startup_preflight;
use vike_tradehub_core::{
    AdapterBind, AdapterPolicy, AdapterStartQuery, ArmingTable, BackendError, IdentityRecord,
    NetReading, PreflightPlan, PreflightQuery, VenueBackend,
};

// ---- The scripted world ----------------------------------------------------------------------------

/// The thresholds the tradehub judges a declared clock risk by (the in-process truth).
fn policy_of(risk: ClockRisk) -> ClockPolicy {
    match risk {
        ClockRisk::SignedTimestamp => ClockPolicy {
            warn_ms: DEFAULT_CLOCK_WARN_MS,
            fail_ms: Some(DEFAULT_CLOCK_FAIL_MS),
            remedy: risk.remedy(),
        },
        ClockRisk::NonceWindow | ClockRisk::NoTimestamp => {
            ClockPolicy { warn_ms: CANARY_CLOCK_WARN_MS, fail_ms: None, remedy: risk.remedy() }
        }
    }
}

/// The wire's class of a risk.
fn class_of(risk: ClockRisk) -> wire::ClockRiskClass {
    match risk {
        ClockRisk::SignedTimestamp => wire::ClockRiskClass::SignedTimestamp,
        ClockRisk::NonceWindow => wire::ClockRiskClass::NonceWindow,
        ClockRisk::NoTimestamp => wire::ClockRiskClass::NoTimestamp,
    }
}

/// One scripted clock read of a venue.
#[derive(Debug, Clone)]
enum Clock {
    /// The venue answers: the read takes `elapsed_ms` and the venue's clock is `skew_ms` ahead of the
    /// MIDDLE of the read.
    Read {
        elapsed_ms: i64,
        skew_ms: i64,
    },
    Unreachable(&'static str),
    NotChecked(&'static str),
    Unmeasured {
        reason: &'static str,
        at_stake: &'static str,
    },
}

#[derive(Debug, Clone)]
enum Cred {
    Accepted,
    Rejected(&'static str),
    Silent { waited_ms: u64, detail: &'static str },
}

#[derive(Debug, Clone)]
struct VenueScript {
    venue: &'static str,
    /// `Some` = the venue publishes a clock (a wired endpoint) and declares this class.
    risk: Option<ClockRisk>,
    /// The reads in order; the last repeats.
    clock: Vec<Clock>,
    /// `Some` = an authed read was built for the venue's default account.
    cred: Option<Cred>,
}

#[derive(Debug, Clone, Copy)]
enum Net {
    Unknown,
    Alive { rounds: u64 },
    Dead { since_ms: i64 },
}

#[derive(Debug, Clone)]
struct World {
    venues: Vec<VenueScript>,
    net: Net,
}

fn venue(venue: &'static str, risk: Option<ClockRisk>, clock: Vec<Clock>) -> VenueScript {
    VenueScript { venue, risk, clock, cred: None }
}

impl VenueScript {
    fn cred(mut self, cred: Cred) -> Self {
        self.cred = Some(cred);
        self
    }
}

/// The fake wall clock: moves only when a scripted read "takes" time.
#[derive(Debug, Clone)]
struct FakeClock(Arc<AtomicI64>);

/// Far from any real wall clock (2023-11-14), so a stray `now_ms()` cannot pass for it.
const EPOCH_MS: i64 = 1_700_000_000_000;

impl FakeClock {
    fn new() -> Self {
        FakeClock(Arc::new(AtomicI64::new(EPOCH_MS)))
    }
    /// A clock frozen at the REAL now: for the one case that runs the real orchestrator.
    fn at_real_now() -> Self {
        FakeClock(Arc::new(AtomicI64::new(vike_model::now_ms())))
    }
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn advance(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

/// The world's reads, with the cursor each venue's clock script is at.
struct Reads {
    world: World,
    clock: FakeClock,
    cursor: Mutex<HashMap<&'static str, usize>>,
}

impl Reads {
    fn new(world: &World, clock: &FakeClock) -> Arc<Self> {
        Arc::new(Reads {
            world: world.clone(),
            clock: clock.clone(),
            cursor: Mutex::new(HashMap::new()),
        })
    }

    fn script(&self, venue: &str) -> Option<&VenueScript> {
        self.world.venues.iter().find(|v| v.venue == venue)
    }

    /// ONE clock read: takes its scripted time on the fake clock, answers an ABSOLUTE stamp.
    fn clock_read(&self, venue: &str) -> Result<i64, ServerTimeGap> {
        let Some(script) = self.script(venue) else {
            return Err(ServerTimeGap::Unreachable(format!("{venue} is not on the world")));
        };
        let step = {
            let mut cursor = self.cursor.lock().expect("cursor");
            let at = cursor.entry(script.venue).or_insert(0);
            let step = script.clock[(*at).min(script.clock.len() - 1)].clone();
            *at += 1;
            step
        };
        match step {
            Clock::Read { elapsed_ms, skew_ms } => {
                let started = self.clock.now();
                self.clock.advance(elapsed_ms);
                Ok(started + elapsed_ms / 2 + skew_ms)
            }
            Clock::Unreachable(why) => Err(ServerTimeGap::Unreachable(why.to_string())),
            Clock::NotChecked(reason) => Err(ServerTimeGap::NotChecked(reason)),
            Clock::Unmeasured { reason, at_stake } => {
                Err(ServerTimeGap::UnmeasuredRisk { reason, at_stake })
            }
        }
    }

    fn credential_read(&self, venue: &str) -> Result<(), CredentialGap> {
        match self.script(venue).and_then(|v| v.cred.clone()) {
            Some(Cred::Accepted) => Ok(()),
            Some(Cred::Rejected(why)) => Err(CredentialGap::Rejected(why.to_string())),
            Some(Cred::Silent { waited_ms, detail }) => {
                Err(CredentialGap::Unanswered { waited_ms, detail: detail.to_string() })
            }
            None => {
                Err(CredentialGap::Rejected(format!("no credential probe was planned for {venue}")))
            }
        }
    }

    fn net(&self) -> Net {
        self.world.net
    }

    /// The plan: the wired venues among `asked` with their risk, how many are wired, and the venues
    /// a credential read exists for.
    fn plan(&self, asked: &[String]) -> (Vec<(&'static str, ClockRisk)>, usize, Vec<String>) {
        let in_query = |v: &&VenueScript| asked.iter().any(|a| a == v.venue);
        let risks: Vec<(&'static str, ClockRisk)> = self
            .world
            .venues
            .iter()
            .filter(in_query)
            .filter_map(|v| v.risk.map(|r| (v.venue, r)))
            .collect();
        let mut credential_venues: Vec<String> = self
            .world
            .venues
            .iter()
            .filter(|v| v.cred.is_some())
            .map(|v| v.venue.to_string())
            .collect();
        credential_venues.sort();
        let wired = risks.len();
        (risks, wired, credential_venues)
    }
}

// ---- In process: the verbs answered straight from the script ---------------------------------------

/// The venue port answered from the script: what `BridgeTrade` behind the in-process backend answered.
/// Only the preflight verbs exist; nothing else is asked of it.
struct InProcess {
    reads: Arc<Reads>,
}

impl VenueBackend for InProcess {
    fn arming_table(&self) -> Result<ArmingTable, BackendError> {
        Err(BackendError::Unavailable("the preflight asks no table".to_string()))
    }

    fn preflight_plan(&self, q: &PreflightQuery<'_>) -> Result<PreflightPlan, BackendError> {
        let (risks, wired, credential_venues) = self.reads.plan(q.clock_venues);
        Ok(PreflightPlan {
            clock_policies: risks
                .into_iter()
                .map(|(venue, risk)| (venue.to_string(), policy_of(risk)))
                .collect(),
            wired_clock_reads: wired,
            credential_venues,
        })
    }

    fn server_time_ms(&self, venue: &str, _tier: AccountTier) -> Result<i64, ServerTimeGap> {
        self.reads.clock_read(venue)
    }

    fn authed_read(&self, venue: &str) -> Result<(), CredentialGap> {
        self.reads.credential_read(venue)
    }

    fn net_reading(&self) -> NetReading {
        match self.reads.net() {
            Net::Unknown => NetReading::Unknown,
            Net::Alive { rounds } => NetReading::Alive { rounds },
            Net::Dead { since_ms } => NetReading::Dead { since_ms },
        }
    }

    fn report_not_running(&self, _venue: &str, _rows: &[vike_config::VenueArming]) {}

    fn start_adapter(
        &self,
        _q: AdapterStartQuery<'_>,
        _events: &vike_exec::EventSender,
        _trigger: Option<std::sync::mpsc::Sender<()>>,
    ) -> Result<AdapterBind, BackendError> {
        Err(BackendError::Unavailable("the preflight starts nothing".to_string()))
    }

    fn record_identity(
        &self,
        _q: &IdentityRecord<'_>,
        _recon: Option<&dyn vike_exec::recon::ReconClient>,
    ) -> Result<(), BackendError> {
        Ok(())
    }
}

// ---- Over the hop: a gatewayhub whose trade lane answers the way `BridgeTrade` does -----------------

fn proto_table(world: &World) -> wire::ArmingTable {
    let mut facts = BTreeMap::new();
    let mut rows = Vec::new();
    for script in &world.venues {
        facts.insert(
            script.venue.to_string(),
            wire::VenueFacts {
                adapter_present: true,
                addresses_accounts: true,
                takes_recon_trigger: false,
                grid_source: wire::GridSource::NoGrid,
                process_exclusive: None,
                clock_decl_word: if script.risk.is_some() { "wired" } else { "not-wired" }
                    .to_string(),
            },
        );
        rows.push(wire::ArmingRow {
            venue: script.venue.to_string(),
            label: None,
            tier: Some(AccountTier::Demo),
            effective: Some(AccountTier::Demo),
            block_word: ArmingBlock::None.word().to_string(),
            account_ids: vec![1],
            book: None,
        });
    }
    wire::ArmingTable { facts, rows }
}

fn policy_for(world: &World) -> AdapterPolicy {
    world.venues.iter().fold(AdapterPolicy::default(), |p, v| {
        p.with_account(v.venue, &AccountLabel::Default, AccountTier::Demo)
    })
}

fn daemon_gap(gap: ServerTimeGap) -> daemon::ServerTimeGap {
    match gap {
        ServerTimeGap::Unreachable(why) => daemon::ServerTimeGap::Unreachable(why),
        ServerTimeGap::NotChecked(reason) => daemon::ServerTimeGap::NotChecked(reason),
        ServerTimeGap::UnmeasuredRisk { reason, at_stake } => {
            daemon::ServerTimeGap::UnmeasuredRisk { reason, at_stake }
        }
    }
}

fn daemon_credential_gap(gap: CredentialGap) -> daemon::CredentialGap {
    match gap {
        CredentialGap::Rejected(why) => daemon::CredentialGap::Rejected(why),
        CredentialGap::Unanswered { waited_ms, detail } => {
            daemon::CredentialGap::Unanswered { waited_ms, detail }
        }
    }
}

/// What `BridgeTrade::preflight` answers to one question, from the script: ONE bounded read per venue
/// named, in the wire's words (the lane's own `probe::*_result` mapping), advancing the SAME fake clock
/// the fold reads while it does.
fn lane_answer(
    reads: &Reads,
    venues: &[String],
    step: wire::PreflightStep,
) -> wire::PreflightAnswer {
    let legs = match step {
        wire::PreflightStep::Plan => {
            let (risks, wired, credential_venues) = reads.plan(venues);
            return wire::PreflightAnswer {
                legs: Vec::new(),
                plan: Some(wire::PreflightLists {
                    clock_risks: risks
                        .into_iter()
                        .map(|(venue, risk)| {
                            (
                                venue.to_string(),
                                wire::ClockRiskInfo {
                                    class: class_of(risk),
                                    remedy: risk.remedy().to_string(),
                                },
                            )
                        })
                        .collect(),
                    wired_clock_reads: u32::try_from(wired).expect("a few venues"),
                    credential_venues,
                }),
            };
        }
        wire::PreflightStep::Clock => venues
            .iter()
            .map(|venue| {
                let started = reads.clock.now();
                let read = reads.clock_read(venue).map_err(daemon_gap);
                let ended = reads.clock.now();
                let rtt_ms = u64::try_from(ended - started).unwrap_or(0);
                daemon::clock_result(venue, read, ended, rtt_ms)
            })
            .collect(),
        wire::PreflightStep::Credential => venues
            .iter()
            .map(|venue| {
                daemon::credential_result(
                    venue,
                    None,
                    reads.credential_read(venue).map_err(daemon_credential_gap),
                )
            })
            .collect(),
        wire::PreflightStep::Network => vec![daemon::network_result(match reads.net() {
            Net::Unknown => daemon::NetReading::Unknown,
            Net::Alive { rounds } => daemon::NetReading::Alive { rounds },
            Net::Dead { since_ms } => daemon::NetReading::Dead { since_ms },
        })],
        wire::PreflightStep::Batch => unreachable!("the tradehub never asks the batch"),
    };
    wire::PreflightAnswer { legs, plan: None }
}

/// A running gatewayhub over the scripted lane, and the remote backend dialled to it.
struct Hop {
    gw: LoopbackGateway,
    backend: Arc<RemoteBackend>,
}

fn hop(world: &World, clock: &FakeClock) -> Hop {
    let venues: Vec<&str> = world.venues.iter().map(|v| v.venue).collect();
    let reads = Reads::new(world, clock);
    let lane = ScriptedTrade::new(&venues)
        .with_arming(proto_table(world))
        .with_preflight(move |venues, step| lane_answer(&reads, venues, step));
    let gw = LoopbackGateway::start_trade(ScriptedData::new(&[]), ScriptedRest::new(), lane);
    let backend = Arc::new(RemoteBackend::connect(
        gw.trade_link(),
        policy_for(world).accounts,
        "vike-tradehub-parity".to_string(),
        Duration::from_secs(5),
    ));
    Hop { gw, backend }
}

// ---- The keep half's run, with the clock injected --------------------------------------------------

/// `run_startup_preflight`'s glue (the plan, the probes over the backend's verbs, the derived budgets,
/// the network read) with the LOCAL CLOCK injected: the orchestrator builds its probes over the real
/// wall clock, and a byte comparison needs both sides on the same fake one. `after_plan` runs between
/// the plan and the first read (a case that takes the gatewayhub away there).
fn report_of(
    backend: &Arc<dyn VenueBackend>,
    table: &ArmingTable,
    world: &World,
    clock: &FakeClock,
    after_plan: impl FnOnce(),
) -> PreflightReport {
    let clock_venues: Vec<String> = vike_model::VENUES
        .iter()
        .filter(|v| world.venues.iter().any(|s| s.venue == **v))
        .filter(|v| vike_tradehub_core::venue_would_go_live(table, v, None))
        .map(|v| (*v).to_string())
        .collect();
    let plan =
        backend.preflight_plan(&PreflightQuery { clock_venues: &clock_venues }).expect("the plan");
    after_plan();
    let wired = plan.wired_clock_reads;
    let probes = FnProbes::new()
        .with_now_ms({
            let clock = clock.clone();
            move || clock.now()
        })
        .with_venue_server_time_ms({
            let backend = Arc::clone(backend);
            move |venue: &str| backend.server_time_ms(venue, AccountTier::Demo)
        })
        .with_venue_authed_read({
            let backend = Arc::clone(backend);
            move |venue: &str| backend.authed_read(venue)
        });
    let cfg = PreflightConfig {
        credential_budget_ms: credential_budget_for(plan.credential_venues.len()),
        clock_venues,
        credential_venues: plan.credential_venues,
        clock_policies: plan.clock_policies,
        clock_budget_ms: clock_budget_for(wired),
        ..PreflightConfig::default()
    };
    let net = backend.net_reading();
    run_preflight_gated(&HashMap::new(), &cfg, &probes, Some(&net))
}

/// The report in process and over the hop, from one world.
fn both(world: &World) -> (PreflightReport, PreflightReport) {
    let remote_clock = FakeClock::new();
    let hop = hop(world, &remote_clock);
    let table = hop.backend.arming_table().expect("the table crossed the hop");

    let local_clock = FakeClock::new();
    let local: Arc<dyn VenueBackend> =
        Arc::new(InProcess { reads: Reads::new(world, &local_clock) });
    let in_process = report_of(&local, &table, world, &local_clock, || {});

    let remote: Arc<dyn VenueBackend> = hop.backend.clone();
    let over_the_hop = report_of(&remote, &table, world, &remote_clock, || {});
    (in_process, over_the_hop)
}

/// The two reports are one: every row's text, status and remedy, the go bit, each venue's fate.
#[track_caller]
fn assert_same(world: &World) {
    let (local, remote) = both(world);
    assert!(!local.checks.is_empty(), "the script ran something");
    assert_eq!(
        remote.lines(),
        local.lines(),
        "the report text over the gatewayhub hop must be the in-process report, byte for byte"
    );
    assert_eq!(remote.go(), local.go(), "the process go/no-go");
    for script in &world.venues {
        assert_eq!(
            remote.venue_disposition(script.venue),
            local.venue_disposition(script.venue),
            "{}: stopped or live",
            script.venue
        );
    }
    assert_eq!(remote, local, "the whole structure, not only its lines");
}

// ---- The cases -------------------------------------------------------------------------------------

/// **The clock risk class and its remedy.** A nonce-window venue 3 s out WARNs with its own sentence
/// (it can never FAIL: its cliff is a day away); a signed-timestamp venue at the same skew FAILs and
/// stops. Judged by one global pair, the nonce venue would be stopped over a fault it does not have.
#[test]
fn the_clock_risk_class_and_its_remedy_cross_the_hop() {
    let world = World {
        venues: vec![
            venue(
                "binance",
                Some(ClockRisk::SignedTimestamp),
                vec![Clock::Read { elapsed_ms: 20, skew_ms: 3_000 }],
            ),
            venue(
                "hyperliquid",
                Some(ClockRisk::NonceWindow),
                vec![Clock::Read { elapsed_ms: 20, skew_ms: 3_000 }],
            ),
            venue(
                "deribit",
                Some(ClockRisk::NoTimestamp),
                vec![Clock::Read { elapsed_ms: 10, skew_ms: 1_200 }],
            ),
        ],
        net: Net::Alive { rounds: 3 },
    };
    let (local, remote) = both(&world);
    // The fixture would prove nothing if the classes did not split the verdicts in process.
    assert_eq!(local.degraded_venues(), vec!["binance".to_string()]);
    assert_eq!(
        remote.degraded_venues(),
        local.degraded_venues(),
        "only the signed venue is stopped"
    );
    assert_same(&world);
}

/// **The reason of a declared clock gap.** `NotChecked` and `UnmeasuredRisk` say WHY there is no clock
/// leg, and what a drifted clock costs at that venue.
#[test]
fn the_reason_of_a_declared_clock_gap_crosses_the_hop() {
    let world = World {
        venues: vec![
            venue(
                "ig",
                None,
                vec![Clock::NotChecked("ig publishes no server-time endpoint (session tokens)")],
            ),
            venue(
                "polymarket",
                None,
                vec![Clock::Unmeasured {
                    reason: "polymarket publishes no server-time endpoint",
                    at_stake: "its L1 signature binds the timestamp into every order",
                }],
            ),
        ],
        net: Net::Alive { rounds: 2 },
    };
    assert_same(&world);
}

/// **The resample and the round trip.** A first reading whose ±rtt/2 band straddles the warn threshold
/// is read again, the tightest round trip wins, and the row says `rtt 20 ms, best of 3 sample(s)`; a
/// read that fails after a good sample keeps the good sample.
#[test]
fn the_resample_and_the_round_trip_cross_the_hop() {
    let world = World {
        venues: vec![
            venue(
                "binance",
                Some(ClockRisk::SignedTimestamp),
                vec![
                    Clock::Read { elapsed_ms: 200, skew_ms: 520 },
                    Clock::Read { elapsed_ms: 60, skew_ms: 505 },
                    Clock::Read { elapsed_ms: 20, skew_ms: 512 },
                ],
            ),
            venue(
                "okx",
                Some(ClockRisk::SignedTimestamp),
                vec![
                    Clock::Read { elapsed_ms: 200, skew_ms: 520 },
                    Clock::Unreachable("read timed out"),
                ],
            ),
        ],
        net: Net::Alive { rounds: 4 },
    };
    let (local, _) = both(&world);
    assert!(
        local.lines().iter().any(|l| l.contains("rtt 20 ms, best of 3 sample(s)")),
        "the fixture resamples in process: {:?}",
        local.lines()
    );
    assert_same(&world);
}

/// **The network leg's numbers.** `internet up after 7 probe round(s)`; `internet DOWN since epoch
/// ms ...` (a global FAIL: no go); and a probe that has not completed a round (a WARN).
#[test]
fn the_network_rounds_and_the_down_since_cross_the_hop() {
    let up = World {
        venues: vec![venue(
            "binance",
            Some(ClockRisk::SignedTimestamp),
            vec![Clock::Read { elapsed_ms: 20, skew_ms: 5 }],
        )],
        net: Net::Alive { rounds: 7 },
    };
    assert_same(&up);
    let down = World { net: Net::Dead { since_ms: 1_699_999_990_123 }, ..up.clone() };
    let (local, _) = both(&down);
    assert!(!local.go(), "a down network is a no-go in process");
    assert_same(&down);
    let unknown = World { net: Net::Unknown, ..up };
    assert_same(&unknown);
}

/// **A gatewayhub that goes away after the plan is a WARN on every row and a green one on none**: each
/// clock leg did not answer, each credential leg is a silence (it stops nothing), the network is
/// unknown, and no venue is stopped over something nobody measured.
#[test]
fn an_unreachable_gatewayhub_is_a_warn_on_every_row_and_green_on_none() {
    let world = World {
        venues: vec![
            venue(
                "binance",
                Some(ClockRisk::SignedTimestamp),
                vec![Clock::Read { elapsed_ms: 20, skew_ms: 5 }],
            )
            .cred(Cred::Accepted),
            venue("ig", None, vec![Clock::NotChecked("ig publishes no server-time endpoint")]),
        ],
        net: Net::Alive { rounds: 3 },
    };
    let clock = FakeClock::new();
    let mut hop = hop(&world, &clock);
    let table = hop.backend.arming_table().expect("the table");
    let remote: Arc<dyn VenueBackend> = hop.backend.clone();
    let report = report_of(&remote, &table, &world, &clock, || {
        hop.gw.stop().expect("the gatewayhub stops");
    });
    let lines = report.lines();
    assert!(
        report.checks.iter().all(|c| c.status == CheckStatus::Warn),
        "no row is green or red when nothing was measured: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("clock_skew (binance)") && l.contains("did not answer"))
    );
    assert!(
        lines.iter().any(|l| l.contains("credentials (binance)") && l.contains("did not answer"))
    );
    assert!(report.go(), "an unmeasured venue stops nothing");
    assert!(report.degraded_venues().is_empty());
}

/// **The whole report**, every outcome at once: signed, nonce and no-timestamp classes, a resample,
/// declared gaps of both kinds, a failed read, and the credential outcomes (accepted, refused, silent).
#[test]
fn the_whole_report_crosses_the_hop_byte_for_byte() {
    assert_same(&full_world());
}

fn full_world() -> World {
    World {
        venues: vec![
            venue(
                "binance",
                Some(ClockRisk::SignedTimestamp),
                vec![
                    Clock::Read { elapsed_ms: 200, skew_ms: 520 },
                    Clock::Read { elapsed_ms: 60, skew_ms: 505 },
                    Clock::Read { elapsed_ms: 20, skew_ms: 512 },
                ],
            )
            .cred(Cred::Accepted),
            venue(
                "bybit",
                Some(ClockRisk::SignedTimestamp),
                vec![Clock::Read { elapsed_ms: 20, skew_ms: 3_000 }],
            )
            .cred(Cred::Rejected("retCode 10003 API key is invalid")),
            venue(
                "hyperliquid",
                Some(ClockRisk::NonceWindow),
                vec![Clock::Read { elapsed_ms: 20, skew_ms: 3_000 }],
            )
            .cred(Cred::Silent { waited_ms: 4_000, detail: "no answer from the info endpoint" }),
            venue(
                "deribit",
                Some(ClockRisk::NoTimestamp),
                vec![Clock::Read { elapsed_ms: 10, skew_ms: 1_200 }],
            ),
            venue(
                "okx",
                Some(ClockRisk::SignedTimestamp),
                vec![Clock::Unreachable("read timed out")],
            ),
            venue("ig", None, vec![Clock::NotChecked("ig publishes no server-time endpoint")]),
            venue(
                "polymarket",
                None,
                vec![Clock::Unmeasured {
                    reason: "polymarket publishes no server-time endpoint",
                    at_stake: "its L1 signature binds the timestamp into every order",
                }],
            )
            .cred(Cred::Accepted),
        ],
        net: Net::Alive { rounds: 7 },
    }
}

/// **The REAL orchestrator** (`run_startup_preflight`, whose probes read the wall clock) over both
/// backends: every row's name, venue, status and remedy, and the text of every row that carries no
/// measured number, are identical. (The numbers in a measured clock row are the wall clock's.)
#[test]
fn the_real_orchestrator_renders_the_same_verdicts_over_the_hop() {
    // Every skew is far from its thresholds and no read takes time, so the verdicts do not depend on
    // the milliseconds the wall clock moves while the preflight runs.
    let at = |name: &'static str, risk: ClockRisk, skew_ms: i64| {
        venue(name, Some(risk), vec![Clock::Read { elapsed_ms: 0, skew_ms }])
    };
    let world = World {
        venues: vec![
            at("binance", ClockRisk::SignedTimestamp, 5).cred(Cred::Accepted),
            at("bybit", ClockRisk::SignedTimestamp, 3_000)
                .cred(Cred::Rejected("retCode 10003 API key is invalid")),
            at("hyperliquid", ClockRisk::NonceWindow, 3_000)
                .cred(Cred::Silent { waited_ms: 4_000, detail: "no answer" }),
            at("deribit", ClockRisk::NoTimestamp, 1_200),
            venue("ig", None, vec![Clock::NotChecked("ig publishes no server-time endpoint")]),
            venue(
                "polymarket",
                None,
                vec![Clock::Unmeasured {
                    reason: "polymarket publishes no server-time endpoint",
                    at_stake: "its L1 signature binds the timestamp into every order",
                }],
            ),
        ],
        net: Net::Alive { rounds: 7 },
    };
    let policy = policy_for(&world);
    let run = |backend: &Arc<dyn VenueBackend>, table: &ArmingTable| {
        run_startup_preflight(backend, table, &HashMap::new(), &[], Some(&policy))
    };

    let remote_clock = FakeClock::at_real_now();
    let hop = hop(&world, &remote_clock);
    let table = hop.backend.arming_table().expect("the table");
    let remote: Arc<dyn VenueBackend> = hop.backend.clone();
    let over_the_hop = run(&remote, &table);

    let local_clock = FakeClock::at_real_now();
    let local: Arc<dyn VenueBackend> =
        Arc::new(InProcess { reads: Reads::new(&world, &local_clock) });
    let in_process = run(&local, &table);

    assert_eq!(over_the_hop.checks.len(), in_process.checks.len());
    for (remote, local) in over_the_hop.checks.iter().zip(&in_process.checks) {
        assert_eq!(
            (&remote.name, &remote.venue, remote.status, &remote.remediation),
            (&local.name, &local.venue, local.status, &local.remediation),
            "{local:?}"
        );
        if !local.message.starts_with("clock skew") {
            assert_eq!(remote.message, local.message, "{}", local.name);
        }
    }
    assert_eq!(over_the_hop.go(), in_process.go());
    assert_eq!(over_the_hop.degraded_venues(), in_process.degraded_venues());
    assert_eq!(in_process.degraded_venues(), vec!["bybit".to_string()]);
}
