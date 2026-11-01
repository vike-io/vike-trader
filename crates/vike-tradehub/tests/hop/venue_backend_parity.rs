//! **R1 over the wire: what every roster venue says about every account, at every tier, in every credential
//! state, is the same through `RemoteBackend` over a real gatewayhub as the recorded golden** (wave 2, W19;
//! drift register R1, decision 0156).
//!
//! W9a recorded the arming golden over the unchanged tree and held it equal across the in-process seam
//! (`crates/vike-gatewayhub/tests/trade_roster/arming_parity.rs`, whose `BridgeBackend` is the seam minus the
//! socket and says "the wire twin of these tests is W19's"). W9b moved the venue half into the gatewayhub and
//! kept the golden byte-identical in-process. THIS is the wire twin: the same fixtures (the roster x the three
//! tiers x the four credential states, the account-table scenarios, the fan-out through the REAL
//! `make_engine_accounts`, the box-wide answers), but every answer is reached the way production reaches it:
//!
//! ```text
//! fold -> RemoteBackend -> TCP -> gatewayhub server (the real accept loop, the real trade sessions)
//!      -> BridgeTrade over the real REGISTRY, built from the policy's `account` rows and credentials
//! ```
//!
//! and the text it renders is compared with the gatewayhub's golden, `include_str!`ed, never copied and never
//! rewritten: a mismatch prints the first lines that differ and fails.
//!
//! **What the golden holds that does not cross the wire, and is therefore projected away** (stated here, so
//! the comparison is not widened by accident):
//!
//! * the policy-less credential PROBE column of `[projection]` (`venue_arming_under` is the gatewayhub's own
//!   function, not a backend verb; `arming_parity.rs` holds it);
//! * the captured `ARMING:` LOG lines of `[start]`: the bridge says them in the GATEWAYHUB's journal now
//!   (decision 0164; `report_not_running` has no verb), so the tradehub's log no longer carries them. The
//!   outcome line of every start is compared in full, with its block.
//!
//! **A paper-only box asks nobody** (R21): `RemoteBackend::connect` neither waits nor dials when no account
//! is at `demo`/`live`, and answers the paper table. That table lists every roster venue as present, which
//! the in-process default build did not for the three optional venues (a recorded R21 change), so the one
//! box-wide answer that reads the facts (`wired_markets`) is asked of a gatewayhub here, and the paper-only
//! answer is pinned on its own.
//!
//! Runs in the default build and, by looking at what the gatewayhub's table says is present, in the build
//! with the three optional venues; a mixed build has no golden and FAILS (a green over nothing is worse).

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use vike_config::ArmingBlock;
use vike_exec::EventSender;
use vike_exec::recon::ReconClient;
use vike_gatewayhub::registry::trade::REGISTRY;
use vike_gatewayhub::testkit::gateway::{fast_timings, trade_keys};
use vike_gatewayhub::trade::adapters::bridge_trade::BridgeTrade;
use vike_gatewayhub::trade::adapters::snapshot::TradeSnapshot;
use vike_gatewayhub::trade::lane::{AdapterRefusal, StartedAdapter, TradeLane, TradeTarget};
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::proto;
use vike_gatewayhub_client::test_support::fake_peer;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub::remote_backend::RemoteBackend;
use vike_tradehub_core::{AdapterPolicy, VenueBackend as _};

use crate::hop_rig;
// The golden's credential fixtures, the account-row builders and the matrix itself are the gatewayhub's
// roster golden's too, so each has one copy compiled once for both crates — the credential sets in the
// gatewayhub's `testkit`, the builders and the matrix (credential states, account-table scenarios, render)
// in tradehub-core's lib (`test-support`): one source, no copy to drift. Until 2026-10-31 this binary
// `#[path]`-included the gatewayhub's own files, then restated the scenarios and the render.
use hop_rig::GwServer;
use vike_gatewayhub::testkit::arming_fixtures::{armed_fields, live_backend_settings};
use vike_gatewayhub::testkit::trade_rig::{fast_trade_timings, quick_config};
use vike_tradehub_core::test_support::account_book::keyed_policy;
use vike_tradehub_core::test_support::arming_parity::{
    ParityApi, render_everything, start_outcome,
};

const GOLDEN_DEFAULT: &str =
    include_str!("../../../vike-gatewayhub/tests/trade_roster/arming_parity/golden_default.txt");
const GOLDEN_ALL: &str =
    include_str!("../../../vike-gatewayhub/tests/trade_roster/arming_parity/golden_all_venues.txt");

// ============================================================================================
// THE ONE BLOCK THAT NAMES HOW AN ANSWER IS REACHED: over the wire.
// ============================================================================================

/// The one gatewayhub of this binary: a real server whose trade lane is a [`Switch`], so each fixture aims it
/// at a `BridgeTrade` built from ITS rows and credentials without starting a server per answer (a start and a
/// drain per fixture is minutes over the matrix). Every fixture still dials it afresh, with a client id of its
/// own, and a start that does not run ends its lease before the next fixture opens.
struct Wire {
    switch: Arc<Switch>,
    addr: String,
    /// Kept so the server lives for the process (it has no owner to stop it, as the datahub's lane tests'
    /// server threads have none).
    _gw: GwServer,
}

/// A trade lane that answers as whichever `BridgeTrade` it was last aimed at.
struct Switch {
    current: Mutex<Arc<BridgeTrade>>,
}

impl Switch {
    fn lane(&self) -> Arc<BridgeTrade> {
        Arc::clone(&self.current.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn aim(&self, lane: BridgeTrade) {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(lane);
    }
}

impl TradeLane for Switch {
    fn venues(&self) -> Vec<String> {
        self.lane().venues()
    }
    fn account_tier(&self, venue: &str, account: Option<&str>) -> Result<AccountTier, ArmingBlock> {
        self.lane().account_tier(venue, account)
    }
    fn arming_table(&self) -> proto::ArmingTable {
        self.lane().arming_table()
    }
    fn preflight(&self, venues: &[String], step: proto::PreflightStep) -> proto::PreflightAnswer {
        self.lane().preflight(venues, step)
    }
    fn start_adapter(
        &self,
        target: &TradeTarget,
        start: &proto::AdapterStart,
        events: EventSender,
        trigger: Option<Sender<()>>,
    ) -> Result<StartedAdapter, AdapterRefusal> {
        self.lane().start_adapter(target, start, events, trigger)
    }
    fn record_identity(
        &self,
        target: &TradeTarget,
        bound_tier: AccountTier,
        recon: Option<&dyn ReconClient>,
    ) -> Result<(), String> {
        self.lane().record_identity(target, bound_tier, recon)
    }
    fn resolves_by_coid(&self, venue: &str) -> bool {
        self.lane().resolves_by_coid(venue)
    }
    fn recon_degraded(&self, venue: &str) -> Option<String> {
        self.lane().recon_degraded(venue)
    }
}

fn lane_for(vars: &HashMap<String, String>, policy: &AdapterPolicy) -> BridgeTrade {
    BridgeTrade::with_snapshot(
        REGISTRY,
        vars.clone(),
        TradeSnapshot {
            accounts: policy.accounts.clone(),
            venue_settings: policy.venue_settings.clone(),
            venue_settings_unread: None,
            preflight_failed: policy.preflight_failed.clone(),
        },
    )
}

fn wire() -> &'static Wire {
    static WIRE: OnceLock<Wire> = OnceLock::new();
    WIRE.get_or_init(|| {
        let switch = Arc::new(Switch {
            current: Mutex::new(Arc::new(lane_for(&HashMap::new(), &AdapterPolicy::default()))),
        });
        let gw = GwServer::start(Arc::clone(&switch) as Arc<dyn TradeLane>, quick_config());
        Wire { switch, addr: gw.addr.to_string(), _gw: gw }
    })
}

/// A `RemoteBackend` connected to the gatewayhub as it answers for `policy`'s rows and credentials, handed
/// to `f` and dropped before this returns.
fn with_wire<R>(
    vars: &HashMap<String, String>,
    policy: &AdapterPolicy,
    f: impl FnOnce(&RemoteBackend) -> R,
) -> R {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let wire = wire();
    wire.switch.aim(lane_for(vars, policy));
    let link = GatewayLink::new(wire.addr.clone(), trade_keys()).with_timings(fast_timings());
    let backend = RemoteBackend::connect(
        link,
        policy.accounts.clone(),
        format!("vike-tradehub-parity-{}", NEXT.fetch_add(1, Ordering::SeqCst)),
        Duration::from_secs(10),
    )
    .with_timings(fast_trade_timings());
    let out = f(&backend);
    drop(backend);
    out
}

mod api {
    use super::*;

    /// Every answer reached over the wire, through a `RemoteBackend` dialled afresh per fixture.
    pub struct OverTheWire;

    impl ParityApi for OverTheWire {
        /// The arming snapshot of one assembly, fetched over the wire.
        fn table(
            &self,
            vars: &HashMap<String, String>,
            policy: &AdapterPolicy,
        ) -> vike_tradehub_core::ArmingTable {
            with_wire(vars, policy, |b| b.arming_table().expect("the wire backend answers"))
        }

        /// The probe column is the gatewayhub's own function, not a backend verb: projected away.
        fn probe(
            &self,
            _venue: &str,
            _vars: &HashMap<String, String>,
            _fields: &[(&'static str, &'static str)],
            _tier: AccountTier,
        ) -> Option<String> {
            None
        }

        /// The fan-out of one venue through the REAL start, reduced to the line the golden prints; its
        /// `ARMING:` lines are said in the gatewayhub's journal, so none is observed here.
        fn start(
            &self,
            venue: &str,
            vars: &HashMap<String, String>,
            policy: &AdapterPolicy,
            account_symbols: &[(AccountLabel, String)],
        ) -> (String, Vec<String>) {
            let outcome = with_wire(vars, policy, |backend| {
                let (tx, _rx) = vike_exec::event_channel(16);
                let mut live = HashSet::new();
                let table = backend.arming_table().expect("the wire backend answers");
                let mut env = vike_tradehub_core::AdapterEnv::new(backend, &table, &tx, &mut live);
                env.recon_enabled = true;
                env.policy = Some(policy);
                start_outcome(vike_tradehub_core::make_engine_accounts(
                    &mut env,
                    venue,
                    account_symbols,
                    &[],
                ))
            });
            (outcome, Vec::new())
        }

        /// The wired markets a build mounts, as the table of a gatewayhub that was ASKED says them (a policy
        /// with one demo account, so the backend dials): `venue/symbol/rank`.
        fn wired(&self) -> Vec<vike_tradehub_core::WiredMarket> {
            let asked = keyed_policy("binance", AccountTier::Demo, armed_fields("binance"));
            self.table(&HashMap::new(), &asked).wired_markets()
        }
    }
}

/// The `[projection]` and `[scenario]` and `[start]` and `[box]` text, in the golden's order, minus the
/// two projections the module doc states (the render is the in-process golden's own, shared).
fn render_everything_over_the_wire() -> String {
    render_everything(&api::OverTheWire, armed_fields, live_backend_settings, "dukascopy")
}

// The golden with the two projections the module doc names applied: the probe column of `[projection]`
/// and the captured log lines of `[start]` (a log line, and any continuation of a multi-line message,
/// up to the next line that opens a section tag).
fn projected(golden: &str) -> String {
    let mut out = String::new();
    let mut in_log = false;
    for line in golden.lines() {
        if line.starts_with('[') {
            in_log = false;
        }
        if in_log {
            continue;
        }
        if line.contains("   log ") && line.starts_with("[start]") {
            in_log = true;
            continue;
        }
        if line.starts_with("[projection]") {
            let cut = line.find(" | probe ").unwrap_or(line.len());
            out.push_str(&line[..cut]);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Which golden the gatewayhub's registry in THIS build calls for, from what its table says is present.
fn golden_for_this_build() -> Option<&'static str> {
    let facts = lane_for(&HashMap::new(), &AdapterPolicy::default()).arming_table_quiet();
    let present = |v: &str| facts.facts.get(v).is_some_and(|f| f.adapter_present);
    let optional = ["ibkr", "fxcm", "polymarket"].map(present);
    if optional.iter().all(|p| !p) {
        Some(GOLDEN_DEFAULT)
    } else if optional.iter().all(|p| *p) {
        Some(GOLDEN_ALL)
    } else {
        None
    }
}

fn first_difference(actual: &str, recorded: &str) -> String {
    let (a, g): (Vec<&str>, Vec<&str>) = (actual.lines().collect(), recorded.lines().collect());
    let at = a.iter().zip(&g).position(|(x, y)| x != y).unwrap_or_else(|| a.len().min(g.len()));
    let mut text = format!(
        "first difference at line {} (lines: over the wire {}, recorded {}):\n",
        at + 1,
        a.len(),
        g.len()
    );
    for i in at..(at + 4).min(a.len().max(g.len())) {
        let _ = writeln!(
            text,
            "  line {}:\n    wire:     {:?}\n    recorded: {:?}",
            i + 1,
            a.get(i),
            g.get(i)
        );
    }
    text
}

/// **Every arming outcome the golden records is the one a tradehub reads over the wire**: the row of
/// every venue x credential state x tier, the account-table scenarios, the outcome of the real fan-out
/// for each (a not-running account carries the bridge's block word through `TradeOpen` / `AdapterStart`
/// to the same `ArmingBlock`), and the box-wide answers.
#[test]
fn every_arming_outcome_is_the_recorded_one_over_the_wire() {
    // A mix has no golden, and a test that returns green over a mix compared NOTHING: refuse it loudly.
    let golden = golden_for_this_build().expect(
        "the gatewayhub is built with some but not all of ibkr, fxcm, polymarket: no golden exists for \n         that mix; run this binary in the default build or with all three",
    );
    let actual = render_everything_over_the_wire();

    // ANTI-VACUITY: the matrix reaches every outcome class it exists to hold, over the wire, or equality
    // below is between two renders that both say "nothing runs".
    for needle in [
        "block=None",
        "block=PaperTier",
        "block=NoCredentials",
        "block=NoAccountRow",
        "block=AccountInactive",
        "block=TierConflict",
        "block=SidecarHeldElsewhere",
        "block=PreflightFailed",
        "block=LiveCredentialsAbsent",
        "refused MissingRiskBudget",
    ] {
        assert!(actual.contains(needle), "the wire matrix never reaches `{needle}`");
    }
    let recorded = projected(golden);
    if actual != recorded {
        println!("-----BEGIN VENUE-BACKEND-PARITY-ACTUAL-----");
        print!("{actual}");
        println!("-----END VENUE-BACKEND-PARITY-ACTUAL-----");
        std::io::Write::flush(&mut std::io::stdout()).expect("flush stdout");
        // The lane merges this stream with the build tool's chatter on stderr; let the text drain.
        std::thread::sleep(Duration::from_millis(500));
        panic!("{}", first_difference(&actual, &recorded));
    }
    println!(
        "venue_backend_parity: {} lines over the wire equal the recorded golden ({} bytes)",
        actual.lines().count(),
        actual.len()
    );
}

/// **A paper-only box asks nobody and runs with no gatewayhub** (R21, decision 0156): connecting with only
/// `paper` accounts neither dials nor waits (the address is nothing that listens), answers the paper
/// table, and a `demo`/`live` account it holds reads `GatewayUnavailable` and never a paper stand-in once a
/// gatewayhub is needed and absent.
#[test]
fn a_paper_only_box_neither_dials_nor_waits_and_a_live_account_is_never_a_paper_stand_in() {
    let dead = fake_peer::dead_addr();
    let link = GatewayLink::new(dead.clone(), trade_keys()).with_timings(fast_timings());

    // Paper only: no wait at all (the wait below would be seconds if it dialled), no failure, the paper rows.
    let paper = keyed_policy("binance", AccountTier::Paper, armed_fields("binance"));
    let started = std::time::Instant::now();
    let backend = RemoteBackend::connect(
        link.clone(),
        paper.accounts.clone(),
        "vike-tradehub-parity-paper".to_string(),
        Duration::from_secs(30),
    );
    assert!(started.elapsed() < Duration::from_secs(5), "a paper-only box waited for a gatewayhub");
    assert_eq!(backend.gateway_down(), None, "a paper-only box has no gatewayhub to miss");
    let table = backend.arming_table().expect("the paper table");
    let row = table.row_of_account("binance", &AccountLabel::Default).expect("the row");
    assert_eq!(
        (row.arming.tier, row.arming.block),
        (Some(AccountTier::Paper), ArmingBlock::PaperTier)
    );

    // A demo account with the gatewayhub away (a short wait): NOT RUNNING `GatewayUnavailable`, never paper.
    let demo = keyed_policy("binance", AccountTier::Demo, armed_fields("binance"));
    let backend = RemoteBackend::connect(
        link,
        demo.accounts.clone(),
        "vike-tradehub-parity-demo".to_string(),
        Duration::from_millis(300),
    );
    assert!(backend.gateway_down().is_some(), "the miss is recorded");
    let table = backend.arming_table().expect("the unavailable table");
    let row = table.row_of_account("binance", &AccountLabel::Default).expect("the row");
    assert_eq!(row.arming.effective, None, "no tier it runs at");
    assert_eq!(row.arming.block, ArmingBlock::GatewayUnavailable);
}

/// **W9c over the wire: the gatewayhub outlives every tradehub, and a tradehub restart still sees every
/// edit made since the daemon started.** In the tradehub the account rows, the credentials and the
/// deployment names were read once, at ITS boot, so a credential filed, a tier edited or a key rotated took
/// effect at the next restart. The venue half is the gatewayhub's now and the production lane
/// (`RefreshedTrade`) re-reads its source at each step a restart drives; here a REAL `RemoteBackend` is
/// connected, dropped and connected again (a tradehub restart) against one gatewayhub whose rows are edited
/// in between, and what each connect is told is the edited world's.
#[test]
fn a_tradehub_restart_over_the_wire_sees_the_edits_made_since_the_gatewayhub_started() {
    let rows = Arc::new(Mutex::new(keyed_policy("binance", AccountTier::Demo, &[])));
    let source: vike_gatewayhub::trade::adapters::refreshed::TradeSource = {
        let rows = Arc::clone(&rows);
        Box::new(move || {
            let policy = rows.lock().unwrap_or_else(PoisonError::into_inner).clone();
            (
                HashMap::new(),
                TradeSnapshot {
                    accounts: policy.accounts.clone(),
                    venue_settings: policy.venue_settings.clone(),
                    venue_settings_unread: None,
                    preflight_failed: policy.preflight_failed.clone(),
                },
            )
        })
    };
    let gw = GwServer::start(
        Arc::new(vike_gatewayhub::trade::adapters::refreshed::RefreshedTrade::new(
            REGISTRY, source,
        )),
        quick_config(),
    );
    static NEXT: AtomicU64 = AtomicU64::new(0);
    // One tradehub start: connect, read binance's default row, drop the backend.
    let restart = || {
        let asked = keyed_policy("binance", AccountTier::Demo, &[]);
        let link = GatewayLink::new(gw.addr.to_string(), trade_keys()).with_timings(fast_timings());
        let backend = RemoteBackend::connect(
            link,
            asked.accounts,
            format!("vike-tradehub-w9c-{}", NEXT.fetch_add(1, Ordering::SeqCst)),
            Duration::from_secs(10),
        );
        let table = backend.arming_table().expect("the wire backend answers");
        let row = table.row_of_account("binance", &AccountLabel::Default).expect("the row");
        (row.arming.tier, row.arming.effective, row.arming.block)
    };

    // The daemon started with the account row and no key: the account does not run.
    assert_eq!(restart(), (Some(AccountTier::Demo), None, ArmingBlock::NoCredentials));

    // The operator files the key. The daemon is NOT restarted; the next tradehub start sees a running account.
    *rows.lock().unwrap_or_else(PoisonError::into_inner) =
        keyed_policy("binance", AccountTier::Demo, armed_fields("binance"));
    assert_eq!(
        restart(),
        (Some(AccountTier::Demo), Some(AccountTier::Demo), ArmingBlock::None),
        "a key filed since the daemon started reaches the next tradehub start"
    );

    // The operator moves the account to live: the next start reads the tier from the rows, not from the past.
    *rows.lock().unwrap_or_else(PoisonError::into_inner) =
        keyed_policy("binance", AccountTier::Live, armed_fields("binance"));
    assert_eq!(
        restart(),
        (Some(AccountTier::Live), Some(AccountTier::Live), ArmingBlock::None),
        "a tier edited since the daemon started reaches the next tradehub start"
    );

    // And the key is withdrawn again: the account stops running at the next start, with the reason.
    *rows.lock().unwrap_or_else(PoisonError::into_inner) =
        keyed_policy("binance", AccountTier::Live, &[]);
    assert_eq!(
        restart(),
        (Some(AccountTier::Live), None, ArmingBlock::LiveCredentialsAbsent),
        "a key withdrawn since the daemon started stops the account at the next start"
    );
}
