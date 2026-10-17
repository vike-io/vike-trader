//! **The claim ordering, end to end through the fold**: `vike-tradehub-core` claims a process-exclusive
//! resource AFTER a positive resolve and the holder check and BEFORE the bridge's start, and keeps
//! it only for a `Live` outcome — credentials, holder, claim, start, keep (the legacy dukascopy
//! arm's order). Each test below walks it.
//!
//! Planted rows (dukascopy's real start starts a JVM). The claim set is process-global, keyed on
//! the RESOURCE, shared by every test in this binary, and a KEPT claim lasts as long as the started engine (W9c: it rode the process until then) — so each
//! planted row declares a resource no other test names and every probe asks for its own row's.
//! Shared, two tests would pass or fail by scheduling order; a probe by another name would find
//! the resource free whatever the fold did. (`crate::trade::adapters::contract`'s planted rows do the same.)

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use vike_bridge_core::venue_adapter::{
    AdapterInputs, AdapterOutcome, AdapterRequest, ExecOutcome, HeldBelowLive, LiveExec,
    PaperCause, Resolution, VenueAdapter, VenueDeclaration,
};
use vike_bridge_core::venue_adapter_fixture::{PLANTED_DECLARATION, planted_exclusive};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use crate::trade::adapters::registry::VenueRow;
use crate::trade::adapters::testutil::{NoopClient, snapshot_of};
use vike_tradehub_core::AdapterPolicy;

/// What the planted venue's credentials and start do.
const NO_CREDENTIALS: u8 = 0;
const START_FAILS: u8 = 1;
const STARTS: u8 = 2;

/// A process-exclusive venue whose resolution and start are scripted, counting its starts.
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
    /// Whether this row's resource is FREE now; the probe claims and drops at once, so asking
    /// changes nothing.
    fn resource_is_free(&self) -> bool {
        super::claim(self.resource).is_some()
    }
}

impl VenueAdapter for Scripted {
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
    fn resolve(&self, _inputs: &AdapterInputs<'_>) -> Resolution {
        if self.phase.load(Ordering::SeqCst) == NO_CREDENTIALS {
            Resolution::Paper(PaperCause::NoCredentials)
        } else {
            Resolution::Armed {
                tier: AccountTier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            }
        }
    }
    fn start(&self, _req: AdapterRequest<'_>) -> AdapterOutcome {
        self.mounts.fetch_add(1, Ordering::SeqCst);
        if self.phase.load(Ordering::SeqCst) != STARTS {
            return AdapterOutcome::paper();
        }
        AdapterOutcome {
            exec: ExecOutcome::Live(LiveExec {
                client: Box::new(NoopClient),
                bound_tier: AccountTier::Demo,
                grid: None,
                contract_value: None,
                margin_mode: None,
                leg_grids: Vec::new(),
            }),
            recon: None,
            identity: None,
        }
    }
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// One start of `venue` for `label` through the REAL fold, against a one-row planted registry:
/// `None` when the account runs, else its NOT RUNNING answer (decision 0156) — never a paper
/// engine standing in for it.
/// A started engine is pushed onto `kept`: the claim of a running resource lives as long as its engine
/// (its client), so a test that wants the resource HELD keeps the engine, and one that drops it hands
/// the resource back.
fn start_adapter(
    registry: &'static [VenueRow],
    venue: &str,
    label: &AccountLabel,
    policy: &AdapterPolicy,
    live: &mut HashSet<String>,
    kept: &mut Vec<vike_tradehub_core::EngineAndRecon>,
) -> Option<vike_config::ArmingBlock> {
    let (tx, _rx) = vike_exec::event_channel(8);
    let (vars, risk) = (HashMap::new(), crate::trade::adapters::contract_tests::budget());
    let rig = crate::trade::adapters::testutil::Rig::new(registry, &vars, Some(policy));
    let mut env = rig.env(&tx, live);
    env.risk_profile = Some(&risk);
    env.policy = Some(policy);
    match vike_tradehub_core::make_engine_for_account(&mut env, venue, "EURUSD", label, &[]) {
        Ok(started) => {
            kept.push(started);
            None
        }
        Err(vike_tradehub_core::EngineError::NotRunning(stopped)) => Some(stopped.block),
        Err(other) => panic!("a budgeted mount never refuses otherwise: {other}"),
    }
}

static ORDERED: Scripted =
    Scripted::new("ctrader", "the claim-order test's resource", NO_CREDENTIALS);
static ORDERED_REG: [VenueRow; 1] = [VenueRow::Adapter(&ORDERED)];

/// The legacy order, one gate at a time: an account with no credentials takes no claim; a failed
/// start releases the claim it took; a running start keeps it; and once kept, the next start is
/// refused by the backstop BEFORE the bridge is asked (Review Focus 3, through the fold). Every
/// step but the running start leaves the account NOT RUNNING, each with its reason.
#[test]
fn the_claim_follows_credentials_then_start_and_is_kept_only_for_a_running_start() {
    use vike_config::ArmingBlock as Block;
    let policy =
        AdapterPolicy::default().with_account("ctrader", &AccountLabel::Default, AccountTier::Demo);
    let mut live = HashSet::new();
    let mut kept = Vec::new();

    // 1. No credentials: the account does not run, the bridge is never asked to start (its
    //    resolve said why), and nothing is claimed.
    let step = start_adapter(
        &ORDERED_REG,
        "ctrader",
        &AccountLabel::Default,
        &policy,
        &mut live,
        &mut kept,
    );
    assert_eq!(step, Some(Block::NoCredentials));
    assert_eq!(ORDERED.mounts(), 0);
    assert!(ORDERED.resource_is_free(), "no credentials ⇒ nothing was claimed");

    // 2. Credentials, and a start that fails: the claim was taken and RELEASED.
    ORDERED.set(START_FAILS);
    let step = start_adapter(
        &ORDERED_REG,
        "ctrader",
        &AccountLabel::Default,
        &policy,
        &mut live,
        &mut kept,
    );
    assert_eq!(step, Some(Block::AdapterFailed));
    assert_eq!(ORDERED.mounts(), 1);
    assert!(live.is_empty());
    assert!(ORDERED.resource_is_free(), "a failed start leaves the resource claimable");

    // 3. A running start: the claim is HELD by the started engine's client.
    ORDERED.set(STARTS);
    let step = start_adapter(
        &ORDERED_REG,
        "ctrader",
        &AccountLabel::Default,
        &policy,
        &mut live,
        &mut kept,
    );
    assert_eq!(step, None, "it runs");
    assert_eq!(ORDERED.mounts(), 2);
    assert!(live.contains("ctrader"));
    assert!(!ORDERED.resource_is_free(), "a running resource keeps its claim");

    // 4. A second start in the same process: refused by the backstop; the bridge is NOT asked.
    let mut again = HashSet::new();
    let step = start_adapter(
        &ORDERED_REG,
        "ctrader",
        &AccountLabel::Default,
        &policy,
        &mut again,
        &mut kept,
    );
    assert_eq!(step, Some(Block::AdapterFailed));
    assert_eq!(ORDERED.mounts(), 2, "the backstop refuses before a second start");
    assert!(again.is_empty());

    // 5. The engine goes (the adapter ended): the resource is claimable and the next start runs.
    kept.clear();
    assert!(ORDERED.resource_is_free(), "the adapter ended, so the claim ended with it");
    let step = start_adapter(
        &ORDERED_REG,
        "ctrader",
        &AccountLabel::Default,
        &policy,
        &mut again,
        &mut kept,
    );
    assert_eq!(step, None, "the next start after the adapter ended runs");
    assert_eq!(ORDERED.mounts(), 3);
}

static HELD: Scripted = Scripted::new("ig", "the holder test's resource", STARTS);
static HELD_REG: [VenueRow; 1] = [VenueRow::Adapter(&HELD)];

/// The HOLDER comes before the claim: with an ACTIVE labelled account row, the DEFAULT account
/// yields before the bridge and before any claim — NOT RUNNING, `SidecarHeldElsewhere` — and the
/// named one takes the resource (`crate::trade::adapters::exclusive`'s `holder`, over a planted venue).
#[test]
fn the_named_account_holds_the_resource_and_the_default_account_yields_before_the_claim() {
    let policy = AdapterPolicy::default()
        .with_account("ig", &AccountLabel::Default, AccountTier::Demo)
        .with_account("ig", &alt(), AccountTier::Demo);
    let mut live = HashSet::new();
    let mut kept = Vec::new();
    let step =
        start_adapter(&HELD_REG, "ig", &AccountLabel::Default, &policy, &mut live, &mut kept);
    assert_eq!(step, Some(vike_config::ArmingBlock::SidecarHeldElsewhere));
    assert_eq!(HELD.mounts(), 0, "the yielding account never reaches the bridge");
    assert!(live.is_empty());
    assert!(HELD.resource_is_free(), "…and takes no claim");

    let step = start_adapter(&HELD_REG, "ig", &alt(), &policy, &mut live, &mut kept);
    assert_eq!(step, None, "the named account runs");
    assert_eq!(HELD.mounts(), 1);
    assert!(live.contains("ig#ALT"), "the named account is the one mounted live: {live:?}");
    assert!(!HELD.resource_is_free(), "…and it holds the resource");
}

static BOOKS: Scripted =
    Scripted::new("dukascopy", "the account-row holder test's resource", STARTS);
static BOOKS_REG: [VenueRow; 1] = [VenueRow::Adapter(&BOOKS)];

/// **The holder comes from the `account` table**, dukascopy-shaped: the second book's row carries
/// its BOOK number as its `label` (and as its `venue_account_id`). ACTIVE, it holds the resource
/// and the DEFAULT account yields; the SAME row INACTIVE arms nothing, so the default keeps it —
/// deactivating the row is how an operator hands the sidecar back.
#[test]
fn an_active_labelled_book_row_holds_the_resource_and_an_inactive_one_does_not() {
    let book = AccountLabel::parse("3716974").expect("a book number is a legal label");
    let row = |active| vike_secrets::Account {
        id: 2,
        venue: "dukascopy".to_string(),
        tier: AccountTier::Demo,
        label: Some("3716974".to_string()),
        venue_account_id: Some("3716974".to_string()),
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
    };
    let base = AdapterPolicy::default().with_account(
        "dukascopy",
        &AccountLabel::Default,
        AccountTier::Demo,
    );
    let vars = HashMap::new();

    let active = base.clone().with_account_row(row(true));
    assert_eq!(super::holder(&BOOKS_REG, "dukascopy", &vars, Some(&snapshot_of(&active))), book);
    assert!(!super::holds(
        &BOOKS_REG,
        "dukascopy",
        &AccountLabel::Default,
        &vars,
        Some(&snapshot_of(&active))
    ));

    let inactive = base.with_account_row(row(false));
    assert_eq!(
        super::holder(&BOOKS_REG, "dukascopy", &vars, Some(&snapshot_of(&inactive))),
        AccountLabel::Default,
        "an inactive labelled row takes nothing from the default account"
    );
    assert_eq!(BOOKS.mounts(), 0, "the holder is a pure question: nothing was mounted");
}
