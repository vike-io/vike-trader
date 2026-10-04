//! **The claim ordering, end to end through the fold** — the equivalence the venue mount
//! contract's research named as its fourth risk: `vike-mount` claims a process-exclusive resource
//! AFTER a positive resolve and the holder check and BEFORE the bridge's mount, and keeps the
//! claim only for a `Live` outcome. That was the legacy dukascopy arm's own order — credentials,
//! then the holder, then the claim, then the start, then keep — and each test below walks it.
//!
//! Planted rows, not the real one: dukascopy's mount starts a JVM, which no offline test can do.
//! The claim set is process-global and keyed on the RESOURCE, and every test in this binary shares
//! it — and a KEPT claim lasts for the life of the process — so each planted row declares a
//! resource no other test names, and every probe asks for that row's own resource. Two tests that
//! shared one would pass or fail by scheduling order; a probe by any other name would find the
//! resource free whatever the fold did. (`crate::contract`'s own planted exclusive rows declare
//! their own two resources for the same reason.)

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use vike_bridge_core::venue_mount::{
    ExecOutcome, HeldBelowLive, LiveExec, MountInputs, MountOutcome, MountRequest, PaperCause,
    Resolution, Tier, VenueDeclaration, VenueMount,
};
use vike_bridge_core::venue_mount_fixture::{PLANTED_DECLARATION, planted_exclusive};
use vike_config::{VenueMode, VenuePolicy};
use vike_model::accounts::account_keys::AccountLabel;

use crate::{MountPolicy, VenueRow};

struct NoopClient;
impl vike_exec::ExecutionClient for NoopClient {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// What the planted venue's credentials and start do.
const NO_CREDENTIALS: u8 = 0;
const START_FAILS: u8 = 1;
const STARTS: u8 = 2;

/// A process-exclusive venue whose resolution and start are scripted, counting its mounts.
struct Scripted {
    venue: &'static str,
    /// This row's OWN resource — the claim key, and the only name its probes may use.
    resource: &'static str,
    phase: AtomicU8,
    mounts: AtomicUsize,
}

impl Scripted {
    const fn new(venue: &'static str, resource: &'static str, phase: u8) -> Self {
        Scripted { venue, resource, phase: AtomicU8::new(phase), mounts: AtomicUsize::new(0) }
    }
    fn set(&self, phase: u8) {
        self.phase.store(phase, Ordering::SeqCst);
    }
    fn mounts(&self) -> usize {
        self.mounts.load(Ordering::SeqCst)
    }
    /// Whether this row's resource is FREE right now. The probe takes the claim and drops it at
    /// once, which releases it — so asking changes nothing.
    fn resource_is_free(&self) -> bool {
        super::claim(self.resource).is_some()
    }
}

impl VenueMount for Scripted {
    fn venue(&self) -> &'static str {
        self.venue
    }
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: true,
            process_exclusive: Some(planted_exclusive(self.resource)),
            ..PLANTED_DECLARATION
        }
    }
    fn resolve(&self, _inputs: &MountInputs<'_>) -> Resolution {
        if self.phase.load(Ordering::SeqCst) == NO_CREDENTIALS {
            Resolution::Paper(PaperCause::NoCredentials)
        } else {
            Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            }
        }
    }
    fn mount(&self, _req: MountRequest<'_>) -> MountOutcome {
        self.mounts.fetch_add(1, Ordering::SeqCst);
        if self.phase.load(Ordering::SeqCst) != STARTS {
            return MountOutcome::paper();
        }
        MountOutcome {
            exec: ExecOutcome::Live(LiveExec {
                client: Box::new(NoopClient),
                bound_tier: Tier::Demo,
                grid: None,
                contract_size: None,
                margin_mode: None,
                leg_grids: Vec::new(),
            }),
            recon: None,
            identity: None,
        }
    }
}

fn budget() -> vike_exec::ProfileRisk {
    vike_exec::ProfileRisk {
        max_notional_per_order: Some(1_000_000.0),
        max_total_exposure: Some(1_000_000.0),
        ..Default::default()
    }
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// One mount of `venue` for `label` through the REAL fold, against a one-row planted registry.
fn mount(
    registry: &'static [VenueRow],
    venue: &str,
    label: &AccountLabel,
    policy: &MountPolicy,
    live: &mut HashSet<String>,
) {
    let (tx, _rx) = vike_exec::event_channel(8);
    crate::make_engine_for_account(
        registry,
        venue,
        "EURUSD",
        label,
        &[],
        &HashMap::new(),
        &tx,
        live,
        false,
        None,
        None,
        Some(&budget()),
        Some(policy),
    )
    .expect("a budgeted mount never refuses");
}

static ORDERED: Scripted =
    Scripted::new("ctrader", "the claim-order test's resource", NO_CREDENTIALS);
static ORDERED_REG: [VenueRow; 1] = [VenueRow::Mount(&ORDERED)];

/// The legacy order, one gate at a time: an account with no credentials takes no claim; a failed
/// start releases the claim it took; a running start keeps it; and once kept, the next mount is
/// refused by the backstop BEFORE the bridge is asked (Review Focus 3, through the fold).
#[test]
fn the_claim_follows_credentials_then_start_and_is_kept_only_for_a_running_start() {
    let policy = MountPolicy {
        venues: VenuePolicy::default().declare("ctrader", VenueMode::Demo),
        ..Default::default()
    };
    let mut live = HashSet::new();

    // 1. No credentials: the probe says paper, the bridge is asked (it logs its own refusal, if
    //    any) and no claim is taken.
    mount(&ORDERED_REG, "ctrader", &AccountLabel::Default, &policy, &mut live);
    assert_eq!(ORDERED.mounts(), 1);
    assert!(ORDERED.resource_is_free(), "no credentials ⇒ nothing was claimed");

    // 2. Credentials, and a start that fails: the claim was taken and RELEASED.
    ORDERED.set(START_FAILS);
    mount(&ORDERED_REG, "ctrader", &AccountLabel::Default, &policy, &mut live);
    assert_eq!(ORDERED.mounts(), 2);
    assert!(live.is_empty());
    assert!(ORDERED.resource_is_free(), "a failed start leaves the resource claimable");

    // 3. A running start: the claim is KEPT for the life of the process.
    ORDERED.set(STARTS);
    mount(&ORDERED_REG, "ctrader", &AccountLabel::Default, &policy, &mut live);
    assert_eq!(ORDERED.mounts(), 3);
    assert!(live.contains("ctrader"));
    assert!(!ORDERED.resource_is_free(), "a running resource keeps its claim");

    // 4. A second mount in the same process: refused by the backstop; the bridge is NOT asked.
    let mut again = HashSet::new();
    mount(&ORDERED_REG, "ctrader", &AccountLabel::Default, &policy, &mut again);
    assert_eq!(ORDERED.mounts(), 3, "the backstop refuses before a second start");
    assert!(again.is_empty());
}

static HELD: Scripted = Scripted::new("ig", "the holder test's resource", STARTS);
static HELD_REG: [VenueRow; 1] = [VenueRow::Mount(&HELD)];

/// The HOLDER comes before the claim: with a labelled account named, the DEFAULT account yields —
/// refused before the bridge is asked and before any claim — and the named account takes the
/// resource (the one-sidecar rule, `crate::exclusive`'s `holder`, over a planted venue).
#[test]
fn the_named_account_holds_the_resource_and_the_default_account_yields_before_the_claim() {
    let policy = MountPolicy {
        venues: VenuePolicy::default().declare("ig", VenueMode::Demo).declare_account(
            "ig",
            &alt(),
            VenueMode::Demo,
        ),
        ..Default::default()
    };
    let mut live = HashSet::new();
    mount(&HELD_REG, "ig", &AccountLabel::Default, &policy, &mut live);
    assert_eq!(HELD.mounts(), 0, "the yielding account never reaches the bridge");
    assert!(live.is_empty());
    assert!(HELD.resource_is_free(), "…and takes no claim");

    mount(&HELD_REG, "ig", &alt(), &policy, &mut live);
    assert_eq!(HELD.mounts(), 1);
    assert!(live.contains("ig#ALT"), "the named account is the one mounted live: {live:?}");
    assert!(!HELD.resource_is_free(), "…and it holds the resource");
}
