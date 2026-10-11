//! THE TIER INTERLOCK: a bridge past a `demo` account's tier (resolve, bind or identity) mounts
//! paper — and so does a `live` account whose bridge would bind DEMO (the no-downgrade rule).
use super::*;

/// A live client that records its shutdown.
struct Detaching(&'static AtomicBool);
impl vike_exec::ExecutionClient for Detaching {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
    fn detach(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A bridge past its tier: `resolve` answers `resolution` and `mount` binds `binds` whatever
/// `MountInputs::live_permitted` says (the defects the interlock exists for). It carries a
/// reconcile client, an identity report and an exclusive resource, so a test sees all three
/// dropped.
struct Overreach {
    venue: &'static str,
    resource: &'static str,
    resolution: Resolution,
    binds: Tier,
    detached: &'static AtomicBool,
    mounts: AtomicUsize,
}

impl VenueMount for Overreach {
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
        self.resolution
    }
    fn mount(&self, _req: MountRequest<'_>) -> MountOutcome {
        self.mounts.fetch_add(1, Ordering::SeqCst);
        MountOutcome {
            exec: ExecOutcome::Live(LiveExec {
                client: Box::new(Detaching(self.detached)),
                bound_tier: self.binds,
                grid: None,
                contract_size: None,
                margin_mode: None,
                leg_grids: vec![],
            }),
            recon: Some(Box::new(EmptyRecon)),
            identity: Some(IdentityReport {
                book: "0xplanted".to_string(),
                evidence: "a planted answer",
                tier: self.binds,
            }),
        }
    }
}

static DEFAULT_ACCOUNT: AccountLabel = AccountLabel::Default;

/// `contract_parts`' inputs for a DEFAULT-account mount of `venue` at `tier`.
fn call<'a>(
    registry: &'static [VenueRow],
    venue: &'a str,
    vars: &'a HashMap<String, String>,
    events: &'a vike_exec::EventSender,
    risk: &'a vike_model::ProfileRisk,
    policy: &'a crate::MountPolicy,
    tier: VenueMode,
) -> crate::contract::ContractCall<'a> {
    crate::contract::ContractCall {
        registry,
        venue,
        symbol: "BTCUSDT",
        account: &DEFAULT_ACCOUNT,
        declared_legs: &[],
        vars,
        live_events: events,
        recon_enabled: false,
        recon_trigger: None,
        properties_rec: None,
        risk_profile: Some(risk),
        policy: Some(policy),
        tier,
        live_permitted: crate::tier_permits_live(tier),
        halt_admit: vike_model::HaltAdmit::Admit,
        static_default: vike_model::FeeSchedule::Free,
    }
}

const RESOLVES_LIVE_RESOURCE: &str = "the resolve-half interlock's resource";
static RESOLVES_LIVE_DETACHED: AtomicBool = AtomicBool::new(false);
static RESOLVES_LIVE: Overreach = Overreach {
    venue: "okx",
    resource: RESOLVES_LIVE_RESOURCE,
    resolution: Resolution::Armed { tier: Tier::Live, held_below_live: None },
    binds: Tier::Live,
    detached: &RESOLVES_LIVE_DETACHED,
    mounts: AtomicUsize::new(0),
};
static RESOLVES_LIVE_REG: [VenueRow; 1] = [VenueRow::Mount(&RESOLVES_LIVE)];

/// Resolve half: a `resolve` answering LIVE under a `demo` tier mounts PAPER (no session, no
/// claim) while the arming projection still shows the wrong answer; under `live` it mounts live.
#[test]
fn a_resolve_that_answers_live_under_a_demo_tier_mounts_paper_without_a_session() {
    let vars = HashMap::new();
    assert_eq!(
        crate::venue_arming_under(&RESOLVES_LIVE_REG, "okx", &vars, VenueMode::Demo),
        (VenueMode::Live, ArmingBlock::None),
        "the projection is NOT clamped, so `vike-backend venues` shows the bridge's wrong answer"
    );
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    mount_one(
        &RESOLVES_LIVE_REG,
        "okx",
        "BTC-USDT-SWAP",
        &tx,
        &mut live,
        Some(&budget()),
        Some(&policy("okx", VenueMode::Demo)),
    )
    .expect("refused to paper, not an error");
    assert!(live.is_empty(), "no live entry under a demo tier");
    assert_eq!(
        RESOLVES_LIVE.mounts.load(Ordering::SeqCst),
        0,
        "the bridge is never asked to mount"
    );
    assert!(
        crate::exclusive::claim(RESOLVES_LIVE_RESOURCE).is_some(),
        "…and its resource was never claimed"
    );

    mount_one(
        &RESOLVES_LIVE_REG,
        "okx",
        "BTC-USDT-SWAP",
        &tx,
        &mut live,
        Some(&budget()),
        Some(&policy("okx", VenueMode::Live)),
    )
    .expect("a live tier mounts");
    assert!(live.contains("okx"), "under a live tier the same answer mounts live");
    assert_eq!(RESOLVES_LIVE.mounts.load(Ordering::SeqCst), 1);
}

const BINDS_LIVE_RESOURCE: &str = "the mount-half interlock's resource";
static BINDS_LIVE_DETACHED: AtomicBool = AtomicBool::new(false);
static BINDS_LIVE: Overreach = Overreach {
    venue: "bybit",
    resource: BINDS_LIVE_RESOURCE,
    resolution: Resolution::Armed { tier: Tier::Demo, held_below_live: None },
    binds: Tier::Live,
    detached: &BINDS_LIVE_DETACHED,
    mounts: AtomicUsize::new(0),
};
static BINDS_LIVE_REG: [VenueRow; 1] = [VenueRow::Mount(&BINDS_LIVE)];

/// Mount half: resolving DEMO but binding LIVE under a `demo` tier is shut down and mounted PAPER
/// (no live entry, identity or reconcile handle; claim released). Under a `live` tier the same
/// bridge never reaches `mount`: its DEMO resolve is the no-downgrade refusal.
#[test]
fn a_mount_that_binds_live_under_a_demo_tier_is_shut_down_and_mounts_paper() {
    let vars = HashMap::new();
    let (tx, _rx) = vike_exec::event_channel(8);
    let risk = budget();
    let demo = policy("bybit", VenueMode::Demo);

    // (1) The parts: where the identity, the reconcile handle and the live flag would be.
    let parts = crate::contract::contract_parts(
        &BINDS_LIVE,
        call(&BINDS_LIVE_REG, "bybit", &vars, &tx, &risk, &demo, VenueMode::Demo),
    )
    .expect("refused to paper, not an error");
    assert!(!parts.live, "the outcome is paper");
    assert!(parts.identity.is_none(), "no identity is recorded");
    assert!(parts.recon.is_none(), "no reconcile handle is kept");
    assert!(parts.record_tier.is_none(), "no authenticated account is recorded");
    assert!(BINDS_LIVE_DETACHED.load(Ordering::SeqCst), "the refused client is shut down");
    assert!(crate::exclusive::claim(BINDS_LIVE_RESOURCE).is_some(), "the claim was released");

    // (2) The engine: no live entry.
    let mut live = HashSet::new();
    mount_one(&BINDS_LIVE_REG, "bybit", "BTCUSDT", &tx, &mut live, Some(&risk), Some(&demo))
        .expect("refused to paper, not an error");
    assert!(live.is_empty(), "no live entry under a demo tier");

    // (3) Under a `live` tier: a DEMO resolve is held to paper before `mount` is ever called.
    let mounts = BINDS_LIVE.mounts.load(Ordering::SeqCst);
    mount_one(
        &BINDS_LIVE_REG,
        "bybit",
        "BTCUSDT",
        &tx,
        &mut live,
        Some(&risk),
        Some(&policy("bybit", VenueMode::Live)),
    )
    .expect("held to paper, not an error");
    assert!(live.is_empty(), "a live account never trades what its bridge resolved as demo");
    assert_eq!(BINDS_LIVE.mounts.load(Ordering::SeqCst), mounts, "the bridge is never asked");
}

/// EXEC keeps to the tier, IDENTITY does not: a client bound at `resolves` with recon
/// (`live_exec`) or a paper exec (hyperliquid's `userRole` answer surviving a failed `meta`),
/// each reporting identity at `identity_tier` whatever `MountInputs::live_permitted` says.
struct IdentityOverreach {
    venue: &'static str,
    resolves: Tier,
    live_exec: bool,
    identity_tier: Tier,
    detached: &'static AtomicBool,
    mounts: AtomicUsize,
}

impl VenueMount for IdentityOverreach {
    fn venue(&self) -> &'static str {
        self.venue
    }
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }
    }
    fn resolve(&self, _inputs: &MountInputs<'_>) -> Resolution {
        Resolution::Armed { tier: self.resolves, held_below_live: None }
    }
    fn mount(&self, _req: MountRequest<'_>) -> MountOutcome {
        self.mounts.fetch_add(1, Ordering::SeqCst);
        let (exec, recon) = if self.live_exec {
            let live = LiveExec {
                client: Box::new(Detaching(self.detached)),
                bound_tier: self.resolves,
                grid: None,
                contract_size: None,
                margin_mode: None,
                leg_grids: vec![],
            };
            (
                ExecOutcome::Live(live),
                Some(Box::new(EmptyRecon) as Box<dyn vike_exec::recon::ReconClient>),
            )
        } else {
            (ExecOutcome::Paper, None)
        };
        MountOutcome {
            exec,
            recon,
            identity: Some(IdentityReport {
                book: "0xplanted".to_string(),
                evidence: "a planted answer",
                tier: self.identity_tier,
            }),
        }
    }
}

static IDENTITY_LIVE_WITH_CLIENT_DETACHED: AtomicBool = AtomicBool::new(false);
static IDENTITY_LIVE_WITH_CLIENT: IdentityOverreach = IdentityOverreach {
    venue: "binance",
    resolves: Tier::Demo,
    live_exec: true,
    identity_tier: Tier::Live,
    detached: &IDENTITY_LIVE_WITH_CLIENT_DETACHED,
    mounts: AtomicUsize::new(0),
};
static IDENTITY_LIVE_WITH_CLIENT_REG: [VenueRow; 1] = [VenueRow::Mount(&IDENTITY_LIVE_WITH_CLIENT)];

static IDENTITY_LIVE_PAPER_DETACHED: AtomicBool = AtomicBool::new(false);
static IDENTITY_LIVE_PAPER: IdentityOverreach = IdentityOverreach {
    venue: "hyperliquid",
    resolves: Tier::Demo,
    live_exec: false,
    identity_tier: Tier::Live,
    detached: &IDENTITY_LIVE_PAPER_DETACHED,
    mounts: AtomicUsize::new(0),
};
static IDENTITY_LIVE_PAPER_REG: [VenueRow; 1] = [VenueRow::Mount(&IDENTITY_LIVE_PAPER)];

static IDENTITY_DEMO_DETACHED: AtomicBool = AtomicBool::new(false);
static IDENTITY_DEMO: IdentityOverreach = IdentityOverreach {
    venue: "okx",
    resolves: Tier::Demo,
    live_exec: true,
    identity_tier: Tier::Demo,
    detached: &IDENTITY_DEMO_DETACHED,
    mounts: AtomicUsize::new(0),
};
static IDENTITY_DEMO_REG: [VenueRow; 1] = [VenueRow::Mount(&IDENTITY_DEMO)];

/// The live-tier control: a bridge resolving LIVE whose paper outcome carries a LIVE identity.
static IDENTITY_LIVE_AT_LIVE_DETACHED: AtomicBool = AtomicBool::new(false);
static IDENTITY_LIVE_AT_LIVE: IdentityOverreach = IdentityOverreach {
    venue: "hyperliquid",
    resolves: Tier::Live,
    live_exec: false,
    identity_tier: Tier::Live,
    detached: &IDENTITY_LIVE_AT_LIVE_DETACHED,
    mounts: AtomicUsize::new(0),
};
static IDENTITY_LIVE_AT_LIVE_REG: [VenueRow; 1] = [VenueRow::Mount(&IDENTITY_LIVE_AT_LIVE)];

/// Identity half: a LIVE-tier identity under a `demo` tier is refused whole (PAPER, no identity,
/// no reconcile handle, client shut down) even when the exec kept to the tier — otherwise
/// `parts_from_outcome` records it as a LIVE-tier account. Controls: a DEMO identity under the
/// same tier is kept (the guard is the identity's tier), and a LIVE one under `live` (the guard is
/// the account's tier).
#[test]
fn an_identity_at_the_live_tier_under_a_demo_tier_is_refused_and_the_mount_is_paper() {
    let vars = HashMap::new();
    let (tx, _rx) = vike_exec::event_channel(8);
    let risk = budget();

    // A live client bound to DEMO beside a LIVE identity.
    let demo = policy("binance", VenueMode::Demo);
    let parts = crate::contract::contract_parts(
        &IDENTITY_LIVE_WITH_CLIENT,
        call(&IDENTITY_LIVE_WITH_CLIENT_REG, "binance", &vars, &tx, &risk, &demo, VenueMode::Demo),
    )
    .expect("refused to paper, not an error");
    assert_eq!(IDENTITY_LIVE_WITH_CLIENT.mounts.load(Ordering::SeqCst), 1, "the bridge was asked");
    assert!(!parts.live, "the outcome is paper");
    assert!(parts.identity.is_none(), "no LIVE-tier identity is recorded");
    assert!(parts.recon.is_none(), "no reconcile handle is kept");
    assert!(parts.record_tier.is_none(), "no authenticated account is recorded");
    assert!(
        IDENTITY_LIVE_WITH_CLIENT_DETACHED.load(Ordering::SeqCst),
        "the refused client is shut down"
    );

    // A paper exec beside a LIVE identity — the shape that was never shut down at all.
    let demo = policy("hyperliquid", VenueMode::Demo);
    let parts = crate::contract::contract_parts(
        &IDENTITY_LIVE_PAPER,
        call(&IDENTITY_LIVE_PAPER_REG, "hyperliquid", &vars, &tx, &risk, &demo, VenueMode::Demo),
    )
    .expect("refused to paper, not an error");
    assert_eq!(IDENTITY_LIVE_PAPER.mounts.load(Ordering::SeqCst), 1, "the bridge was asked");
    assert!(!parts.live);
    assert!(parts.identity.is_none(), "a paper outcome's LIVE-tier identity is dropped as well");

    // Control 1: a DEMO identity under the same tier is kept, beside its live client.
    let demo = policy("okx", VenueMode::Demo);
    let parts = crate::contract::contract_parts(
        &IDENTITY_DEMO,
        call(&IDENTITY_DEMO_REG, "okx", &vars, &tx, &risk, &demo, VenueMode::Demo),
    )
    .expect("a demo identity under a demo tier mounts");
    assert!(parts.live, "the demo-bound client is kept");
    assert_eq!(
        parts.identity,
        Some(("0xplanted".to_string(), "a planted answer", VenueMode::Demo)),
        "…and the DEMO identity is recorded"
    );
    assert!(!IDENTITY_DEMO_DETACHED.load(Ordering::SeqCst), "nothing was shut down");

    // Control 2: under a `live` tier the LIVE identity is kept.
    let live = policy("hyperliquid", VenueMode::Live);
    let parts = crate::contract::contract_parts(
        &IDENTITY_LIVE_AT_LIVE,
        call(&IDENTITY_LIVE_AT_LIVE_REG, "hyperliquid", &vars, &tx, &risk, &live, VenueMode::Live),
    )
    .expect("a live tier mounts");
    assert_eq!(
        parts.identity,
        Some(("0xplanted".to_string(), "a planted answer", VenueMode::Live)),
        "under a live tier the LIVE identity is recorded"
    );
}

// ---- THE NO-DOWNGRADE RULE: a `live` account never trades demo ----

/// A bridge that falls back to its DEMO tier when the account is `live` and only demo keys are
/// stored. Aster's testnet fallback (`HeldBelowLive::LiveCredentialsAbsent`) and every demo-only
/// arm (`HeldBelowLive::DemoOnlyArm`) have this shape: `resolve` answers DEMO and `mount` binds
/// DEMO whatever `MountInputs::live_permitted` says.
struct DemoFallback {
    venue: &'static str,
    held: HeldBelowLive,
    detached: &'static AtomicBool,
    mounts: AtomicUsize,
}

impl VenueMount for DemoFallback {
    fn venue(&self) -> &'static str {
        self.venue
    }
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }
    }
    fn resolve(&self, _inputs: &MountInputs<'_>) -> Resolution {
        Resolution::Armed { tier: Tier::Demo, held_below_live: Some(self.held) }
    }
    fn mount(&self, _req: MountRequest<'_>) -> MountOutcome {
        self.mounts.fetch_add(1, Ordering::SeqCst);
        MountOutcome {
            exec: ExecOutcome::Live(LiveExec {
                client: Box::new(Detaching(self.detached)),
                bound_tier: Tier::Demo,
                grid: None,
                contract_size: None,
                margin_mode: None,
                leg_grids: vec![],
            }),
            recon: Some(Box::new(EmptyRecon)),
            identity: Some(IdentityReport {
                book: "0xdemo".to_string(),
                evidence: "a planted demo answer",
                tier: Tier::Demo,
            }),
        }
    }
}

static TESTNET_FALLBACK_DETACHED: AtomicBool = AtomicBool::new(false);
static TESTNET_FALLBACK: DemoFallback = DemoFallback {
    venue: "aster",
    held: HeldBelowLive::LiveCredentialsAbsent,
    detached: &TESTNET_FALLBACK_DETACHED,
    mounts: AtomicUsize::new(0),
};
static TESTNET_FALLBACK_REG: [VenueRow; 1] = [VenueRow::Mount(&TESTNET_FALLBACK)];

static DEMO_ONLY_DETACHED: AtomicBool = AtomicBool::new(false);
static DEMO_ONLY: DemoFallback = DemoFallback {
    venue: "ibkr",
    held: HeldBelowLive::DemoOnlyArm,
    detached: &DEMO_ONLY_DETACHED,
    mounts: AtomicUsize::new(0),
};
static DEMO_ONLY_REG: [VenueRow; 1] = [VenueRow::Mount(&DEMO_ONLY)];

/// **A `live` account whose bridge would bind DEMO mounts PAPER, never demo** — for the testnet
/// fallback and the demo-only arm alike: the projection says `(Paper, LiveCredentialsAbsent)`,
/// `contract_parts` never calls the bridge's `mount` (no demo session under the live account's
/// name), no live entry, no identity. Control: the SAME bridge under a `demo` tier mounts demo.
#[test]
fn a_live_account_whose_bridge_would_bind_demo_mounts_paper_never_demo() {
    let vars = HashMap::new();
    let (tx, _rx) = vike_exec::event_channel(8);
    let risk = budget();
    let rows: [(&'static DemoFallback, &'static [VenueRow], &str); 2] =
        [(&TESTNET_FALLBACK, &TESTNET_FALLBACK_REG, "aster"), (&DEMO_ONLY, &DEMO_ONLY_REG, "ibkr")];
    for (row, reg, venue) in rows {
        let live_tier = policy(venue, VenueMode::Live);
        let projected = crate::venue_account_arming(reg, venue, &vars, Some(&live_tier));
        assert_eq!(
            (projected[0].tier, projected[0].effective, projected[0].block),
            (VenueMode::Live, VenueMode::Paper, ArmingBlock::LiveCredentialsAbsent),
            "{venue}: the projection holds the live account at paper, naming the live key set"
        );
        let parts = crate::contract::contract_parts(
            row,
            call(reg, venue, &vars, &tx, &risk, &live_tier, VenueMode::Live),
        )
        .expect("held to paper, not an error");
        assert!(!parts.live, "{venue}: a live account never trades demo");
        assert!(parts.identity.is_none() && parts.record_tier.is_none(), "{venue}: no identity");
        assert_eq!(row.mounts.load(Ordering::SeqCst), 0, "{venue}: the bridge is never asked");

        let mut live = HashSet::new();
        mount_one(reg, venue, "BTCUSDT", &tx, &mut live, Some(&risk), Some(&live_tier))
            .expect("held to paper, not an error");
        assert!(live.is_empty(), "{venue}: no live entry for a live account held at paper");
        assert_eq!(row.mounts.load(Ordering::SeqCst), 0, "{venue}: still never asked");

        // The control: a `demo` account on the same bridge trades demo.
        let demo_tier = policy(venue, VenueMode::Demo);
        mount_one(reg, venue, "BTCUSDT", &tx, &mut live, Some(&risk), Some(&demo_tier))
            .expect("a demo tier mounts");
        assert!(live.contains(venue), "{venue}: a demo account mounts its demo tier");
        assert!(!row.detached.load(Ordering::SeqCst), "{venue}: nothing was shut down");
    }
}

static RESOLVES_LIVE_BINDS_DEMO_DETACHED: AtomicBool = AtomicBool::new(false);
static RESOLVES_LIVE_BINDS_DEMO: Overreach = Overreach {
    venue: "okx",
    resource: "the no-downgrade mount half's resource",
    resolution: Resolution::Armed { tier: Tier::Live, held_below_live: None },
    binds: Tier::Demo,
    detached: &RESOLVES_LIVE_BINDS_DEMO_DETACHED,
    mounts: AtomicUsize::new(0),
};
static RESOLVES_LIVE_BINDS_DEMO_REG: [VenueRow; 1] = [VenueRow::Mount(&RESOLVES_LIVE_BINDS_DEMO)];

/// The no-downgrade rule's MOUNT half: a bridge that resolves LIVE for a `live` account but whose
/// `mount` binds DEMO is shut down and mounted PAPER — no live entry, identity or reconcile handle.
#[test]
fn a_mount_that_binds_demo_for_a_live_account_is_shut_down_and_mounts_paper() {
    let vars = HashMap::new();
    let (tx, _rx) = vike_exec::event_channel(8);
    let risk = budget();
    let live_tier = policy("okx", VenueMode::Live);
    let parts = crate::contract::contract_parts(
        &RESOLVES_LIVE_BINDS_DEMO,
        call(&RESOLVES_LIVE_BINDS_DEMO_REG, "okx", &vars, &tx, &risk, &live_tier, VenueMode::Live),
    )
    .expect("refused to paper, not an error");
    assert_eq!(RESOLVES_LIVE_BINDS_DEMO.mounts.load(Ordering::SeqCst), 1, "the bridge was asked");
    assert!(!parts.live, "a DEMO bind for a live account is paper");
    assert!(parts.identity.is_none(), "no DEMO identity is recorded for a live account");
    assert!(parts.recon.is_none(), "no reconcile handle is kept");
    assert!(RESOLVES_LIVE_BINDS_DEMO_DETACHED.load(Ordering::SeqCst), "the client is shut down");
}
