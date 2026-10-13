//! THE TIER INTERLOCK: a bridge past a `demo` account's tier (resolve, bind or identity) leaves
//! the account NOT RUNNING — and so does a `live` account whose bridge would bind DEMO (the
//! no-downgrade rule). Never a paper engine in its place (decision 0156).
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
/// `MountInputs::tier` says (the defects the interlock exists for). It carries a
/// reconcile client, an identity report and an exclusive resource, so a test sees all three
/// dropped.
struct Overreach {
    venue: &'static str,
    resource: &'static str,
    resolution: Resolution,
    binds: AccountTier,
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
    tier: AccountTier,
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
        halt_admit: vike_model::HaltAdmit::Admit,
        static_default: vike_model::FeeSchedule::Free,
    }
}

const RESOLVES_LIVE_RESOURCE: &str = "the resolve-half interlock's resource";
static RESOLVES_LIVE_DETACHED: AtomicBool = AtomicBool::new(false);
static RESOLVES_LIVE: Overreach = Overreach {
    venue: "okx",
    resource: RESOLVES_LIVE_RESOURCE,
    resolution: Resolution::Armed { tier: AccountTier::Live, held_below_live: None },
    binds: AccountTier::Live,
    detached: &RESOLVES_LIVE_DETACHED,
    mounts: AtomicUsize::new(0),
};
static RESOLVES_LIVE_REG: [VenueRow; 1] = [VenueRow::Mount(&RESOLVES_LIVE)];

/// The account [`crate::contract::contract_parts`] reported as NOT RUNNING, or a panic naming
/// what came back instead.
fn parts_stopped(
    result: Result<crate::engine::MountParts, crate::MountError>,
) -> crate::NotRunning {
    match result {
        Err(crate::MountError::NotRunning(nr)) => nr,
        Err(other) => panic!("expected NOT RUNNING, got the refusal: {other}"),
        Ok(parts) => panic!("expected NOT RUNNING, got parts bound at {}", parts.bound_tier),
    }
}

/// Resolve half: a `resolve` answering LIVE under a `demo` tier does NOT RUN (no session, no
/// claim, `MountFailed`) while the arming projection still shows the wrong answer; under `live` it
/// mounts live. Before decision 0156 it mounted PAPER under the demo account's name.
#[test]
fn a_resolve_that_answers_live_under_a_demo_tier_does_not_run_and_opens_no_session() {
    let vars = HashMap::new();
    assert_eq!(
        crate::venue_arming_under(&RESOLVES_LIVE_REG, "okx", &vars, no_creds(), AccountTier::Demo),
        (Some(AccountTier::Live), ArmingBlock::None),
        "the projection is NOT clamped, so `vike-backend venues` shows the bridge's wrong answer"
    );
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let stopped = not_running(mount_one(
        &RESOLVES_LIVE_REG,
        "okx",
        "BTC-USDT-SWAP",
        &tx,
        &mut live,
        Some(&budget()),
        Some(&policy("okx", AccountTier::Demo)),
    ));
    assert_eq!((stopped.tier, stopped.block), (Some(AccountTier::Demo), ArmingBlock::MountFailed));
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

    let (engine, _recon) = mount_one(
        &RESOLVES_LIVE_REG,
        "okx",
        "BTC-USDT-SWAP",
        &tx,
        &mut live,
        Some(&budget()),
        Some(&policy("okx", AccountTier::Live)),
    )
    .unwrap_or_else(|e| panic!("a live tier mounts: {e}"));
    assert!(live.contains("okx"), "under a live tier the same answer mounts live");
    assert_eq!(engine.tier, AccountTier::Live);
    assert_eq!(RESOLVES_LIVE.mounts.load(Ordering::SeqCst), 1);
}

const BINDS_LIVE_RESOURCE: &str = "the mount-half interlock's resource";
static BINDS_LIVE_DETACHED: AtomicBool = AtomicBool::new(false);
static BINDS_LIVE: Overreach = Overreach {
    venue: "bybit",
    resource: BINDS_LIVE_RESOURCE,
    resolution: Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
    binds: AccountTier::Live,
    detached: &BINDS_LIVE_DETACHED,
    mounts: AtomicUsize::new(0),
};
static BINDS_LIVE_REG: [VenueRow; 1] = [VenueRow::Mount(&BINDS_LIVE)];

/// Mount half: resolving DEMO but binding LIVE under a `demo` tier is shut down and does NOT RUN
/// (`MountFailed`: no live entry, no engine; claim released). Under a `live` tier the same bridge
/// never reaches `mount`: its DEMO resolve is the no-downgrade refusal, `LiveCredentialsAbsent`.
#[test]
fn a_mount_that_binds_live_under_a_demo_tier_is_shut_down_and_does_not_run() {
    let vars = HashMap::new();
    let (tx, _rx) = vike_exec::event_channel(8);
    let risk = budget();
    let demo = policy("bybit", AccountTier::Demo);

    // (1) The parts: none — the account does not run, and the bridge's client is shut down.
    let stopped = parts_stopped(crate::contract::contract_parts(
        &BINDS_LIVE,
        call(&BINDS_LIVE_REG, "bybit", &vars, &tx, &risk, &demo, AccountTier::Demo),
    ));
    assert_eq!((stopped.tier, stopped.block), (Some(AccountTier::Demo), ArmingBlock::MountFailed));
    assert!(BINDS_LIVE_DETACHED.load(Ordering::SeqCst), "the refused client is shut down");
    assert!(crate::exclusive::claim(BINDS_LIVE_RESOURCE).is_some(), "the claim was released");

    // (2) The engine: none, and no live entry.
    let mut live = HashSet::new();
    let stopped = not_running(mount_one(
        &BINDS_LIVE_REG,
        "bybit",
        "BTCUSDT",
        &tx,
        &mut live,
        Some(&risk),
        Some(&demo),
    ));
    assert_eq!(stopped.block, ArmingBlock::MountFailed);
    assert!(live.is_empty(), "no live entry under a demo tier");

    // (3) Under a `live` tier: a DEMO resolve does not run before `mount` is ever called.
    let mounts = BINDS_LIVE.mounts.load(Ordering::SeqCst);
    let stopped = not_running(mount_one(
        &BINDS_LIVE_REG,
        "bybit",
        "BTCUSDT",
        &tx,
        &mut live,
        Some(&risk),
        Some(&policy("bybit", AccountTier::Live)),
    ));
    assert_eq!(
        (stopped.tier, stopped.block),
        (Some(AccountTier::Live), ArmingBlock::LiveCredentialsAbsent)
    );
    assert!(live.is_empty(), "a live account never trades what its bridge resolved as demo");
    assert_eq!(BINDS_LIVE.mounts.load(Ordering::SeqCst), mounts, "the bridge is never asked");
}

/// EXEC keeps to the tier, IDENTITY does not: a client bound at `resolves` with recon
/// (`live_exec`) or a paper exec (hyperliquid's `userRole` answer surviving a failed `meta`),
/// each reporting identity at `identity_tier` whatever `MountInputs::tier` says.
struct IdentityOverreach {
    venue: &'static str,
    resolves: AccountTier,
    live_exec: bool,
    identity_tier: AccountTier,
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
    resolves: AccountTier::Demo,
    live_exec: true,
    identity_tier: AccountTier::Live,
    detached: &IDENTITY_LIVE_WITH_CLIENT_DETACHED,
    mounts: AtomicUsize::new(0),
};
static IDENTITY_LIVE_WITH_CLIENT_REG: [VenueRow; 1] = [VenueRow::Mount(&IDENTITY_LIVE_WITH_CLIENT)];

static IDENTITY_LIVE_PAPER_DETACHED: AtomicBool = AtomicBool::new(false);
static IDENTITY_LIVE_PAPER: IdentityOverreach = IdentityOverreach {
    venue: "hyperliquid",
    resolves: AccountTier::Demo,
    live_exec: false,
    identity_tier: AccountTier::Live,
    detached: &IDENTITY_LIVE_PAPER_DETACHED,
    mounts: AtomicUsize::new(0),
};
static IDENTITY_LIVE_PAPER_REG: [VenueRow; 1] = [VenueRow::Mount(&IDENTITY_LIVE_PAPER)];

static IDENTITY_DEMO_DETACHED: AtomicBool = AtomicBool::new(false);
static IDENTITY_DEMO: IdentityOverreach = IdentityOverreach {
    venue: "okx",
    resolves: AccountTier::Demo,
    live_exec: true,
    identity_tier: AccountTier::Demo,
    detached: &IDENTITY_DEMO_DETACHED,
    mounts: AtomicUsize::new(0),
};
static IDENTITY_DEMO_REG: [VenueRow; 1] = [VenueRow::Mount(&IDENTITY_DEMO)];

/// The live-tier control: a bridge resolving LIVE whose paper outcome carries a LIVE identity.
static IDENTITY_LIVE_AT_LIVE_DETACHED: AtomicBool = AtomicBool::new(false);
static IDENTITY_LIVE_AT_LIVE: IdentityOverreach = IdentityOverreach {
    venue: "hyperliquid",
    resolves: AccountTier::Live,
    live_exec: false,
    identity_tier: AccountTier::Live,
    detached: &IDENTITY_LIVE_AT_LIVE_DETACHED,
    mounts: AtomicUsize::new(0),
};
static IDENTITY_LIVE_AT_LIVE_REG: [VenueRow; 1] = [VenueRow::Mount(&IDENTITY_LIVE_AT_LIVE)];

/// Identity half: a LIVE-tier identity under a `demo` tier is refused whole (NOT RUNNING,
/// `MountFailed`, client shut down) even when the exec kept to the tier — otherwise the mount
/// records it as a LIVE-tier account. Controls: a DEMO identity under the same tier is kept (the
/// guard is the identity's tier), and a LIVE one under `live` passes the guard (the guard is the
/// account's tier) — though a mount that built no client there does not run either.
#[test]
fn an_identity_at_the_live_tier_under_a_demo_tier_is_refused_and_the_account_does_not_run() {
    let vars = HashMap::new();
    let (tx, _rx) = vike_exec::event_channel(8);
    let risk = budget();

    // A live client bound to DEMO beside a LIVE identity.
    let demo = policy("binance", AccountTier::Demo);
    let stopped = parts_stopped(crate::contract::contract_parts(
        &IDENTITY_LIVE_WITH_CLIENT,
        call(
            &IDENTITY_LIVE_WITH_CLIENT_REG,
            "binance",
            &vars,
            &tx,
            &risk,
            &demo,
            AccountTier::Demo,
        ),
    ));
    assert_eq!(IDENTITY_LIVE_WITH_CLIENT.mounts.load(Ordering::SeqCst), 1, "the bridge was asked");
    assert_eq!(stopped.block, ArmingBlock::MountFailed, "the account does not run");
    assert!(
        IDENTITY_LIVE_WITH_CLIENT_DETACHED.load(Ordering::SeqCst),
        "the refused client is shut down"
    );

    // A paper exec beside a LIVE identity — the shape that was never shut down at all.
    let demo = policy("hyperliquid", AccountTier::Demo);
    let stopped = parts_stopped(crate::contract::contract_parts(
        &IDENTITY_LIVE_PAPER,
        call(&IDENTITY_LIVE_PAPER_REG, "hyperliquid", &vars, &tx, &risk, &demo, AccountTier::Demo),
    ));
    assert_eq!(IDENTITY_LIVE_PAPER.mounts.load(Ordering::SeqCst), 1, "the bridge was asked");
    assert_eq!(stopped.block, ArmingBlock::MountFailed);

    // Control 1: a DEMO identity under the same tier is kept, beside its live client.
    let demo = policy("okx", AccountTier::Demo);
    let parts = crate::contract::contract_parts(
        &IDENTITY_DEMO,
        call(&IDENTITY_DEMO_REG, "okx", &vars, &tx, &risk, &demo, AccountTier::Demo),
    )
    .unwrap_or_else(|e| panic!("a demo identity under a demo tier mounts: {e}"));
    assert_eq!(parts.bound_tier, AccountTier::Demo, "the demo-bound client is kept");
    assert_eq!(
        parts.identity,
        Some(("0xplanted".to_string(), "a planted answer", AccountTier::Demo)),
        "…and the DEMO identity is recorded"
    );
    assert!(!IDENTITY_DEMO_DETACHED.load(Ordering::SeqCst), "nothing was shut down");

    // Control 2: under a `live` tier the LIVE identity passes the guard, so the bridge's paper
    // outcome is not refused as beyond the tier — it is a mount that built no client, and the
    // account does not run (`MountFailed`); its identity is recorded on that path
    // (`crate::book_identity::record_confirmation`), not dropped.
    let live = policy("hyperliquid", AccountTier::Live);
    let stopped = parts_stopped(crate::contract::contract_parts(
        &IDENTITY_LIVE_AT_LIVE,
        call(
            &IDENTITY_LIVE_AT_LIVE_REG,
            "hyperliquid",
            &vars,
            &tx,
            &risk,
            &live,
            AccountTier::Live,
        ),
    ));
    assert_eq!((stopped.tier, stopped.block), (Some(AccountTier::Live), ArmingBlock::MountFailed));
    assert_eq!(IDENTITY_LIVE_AT_LIVE.mounts.load(Ordering::SeqCst), 1, "the bridge was asked");
    assert!(!IDENTITY_LIVE_AT_LIVE_DETACHED.load(Ordering::SeqCst), "nothing was refused");
}

// ---- THE NO-DOWNGRADE RULE: a `live` account never trades demo ----

/// A bridge that falls back to its DEMO tier when the account is `live` and only demo keys are
/// stored. Aster's testnet fallback (`HeldBelowLive::LiveCredentialsAbsent`) and every demo-only
/// arm (`HeldBelowLive::DemoOnlyArm`) have this shape: `resolve` answers DEMO and `mount` binds
/// DEMO whatever `MountInputs::tier` says.
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
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: Some(self.held) }
    }
    fn mount(&self, _req: MountRequest<'_>) -> MountOutcome {
        self.mounts.fetch_add(1, Ordering::SeqCst);
        MountOutcome {
            exec: ExecOutcome::Live(LiveExec {
                client: Box::new(Detaching(self.detached)),
                bound_tier: AccountTier::Demo,
                grid: None,
                contract_size: None,
                margin_mode: None,
                leg_grids: vec![],
            }),
            recon: Some(Box::new(EmptyRecon)),
            identity: Some(IdentityReport {
                book: "0xdemo".to_string(),
                evidence: "a planted demo answer",
                tier: AccountTier::Demo,
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

/// **A `live` account whose bridge would bind DEMO does NOT RUN, never demo, never paper** — for
/// the testnet fallback and the demo-only arm alike: the projection says it does not run,
/// `LiveCredentialsAbsent`, `contract_parts` never calls the bridge's `mount` (no demo session
/// under the live account's name), no live entry. Control: the SAME bridge under a `demo` tier
/// mounts demo.
#[test]
fn a_live_account_whose_bridge_would_bind_demo_does_not_run_never_demo() {
    let vars = HashMap::new();
    let (tx, _rx) = vike_exec::event_channel(8);
    let risk = budget();
    let rows: [(&'static DemoFallback, &'static [VenueRow], &str); 2] =
        [(&TESTNET_FALLBACK, &TESTNET_FALLBACK_REG, "aster"), (&DEMO_ONLY, &DEMO_ONLY_REG, "ibkr")];
    for (row, reg, venue) in rows {
        let live_tier = policy(venue, AccountTier::Live);
        let projected = crate::venue_account_arming(reg, venue, &vars, Some(&live_tier));
        assert_eq!(
            (projected[0].tier, projected[0].effective, projected[0].block),
            (Some(AccountTier::Live), None, ArmingBlock::LiveCredentialsAbsent),
            "{venue}: the projection says the live account does not run, naming the live key set"
        );
        let stopped = parts_stopped(crate::contract::contract_parts(
            row,
            call(reg, venue, &vars, &tx, &risk, &live_tier, AccountTier::Live),
        ));
        assert_eq!(stopped.block, ArmingBlock::LiveCredentialsAbsent, "{venue}");
        assert_eq!(row.mounts.load(Ordering::SeqCst), 0, "{venue}: the bridge is never asked");

        let mut live = HashSet::new();
        let stopped = not_running(mount_one(
            reg,
            venue,
            "BTCUSDT",
            &tx,
            &mut live,
            Some(&risk),
            Some(&live_tier),
        ));
        assert_eq!(stopped.block, ArmingBlock::LiveCredentialsAbsent, "{venue}");
        assert!(live.is_empty(), "{venue}: no live entry for a live account that does not run");
        assert_eq!(row.mounts.load(Ordering::SeqCst), 0, "{venue}: still never asked");

        // The control: a `demo` account on the same bridge trades demo.
        let demo_tier = policy(venue, AccountTier::Demo);
        let (engine, _recon) =
            mount_one(reg, venue, "BTCUSDT", &tx, &mut live, Some(&risk), Some(&demo_tier))
                .unwrap_or_else(|e| panic!("{venue}: a demo tier mounts: {e}"));
        assert!(live.contains(venue), "{venue}: a demo account mounts its demo tier");
        assert_eq!(engine.tier, AccountTier::Demo, "{venue}");
        assert!(!row.detached.load(Ordering::SeqCst), "{venue}: nothing was shut down");
    }
}

static RESOLVES_LIVE_BINDS_DEMO_DETACHED: AtomicBool = AtomicBool::new(false);
static RESOLVES_LIVE_BINDS_DEMO: Overreach = Overreach {
    venue: "okx",
    resource: "the no-downgrade mount half's resource",
    resolution: Resolution::Armed { tier: AccountTier::Live, held_below_live: None },
    binds: AccountTier::Demo,
    detached: &RESOLVES_LIVE_BINDS_DEMO_DETACHED,
    mounts: AtomicUsize::new(0),
};
static RESOLVES_LIVE_BINDS_DEMO_REG: [VenueRow; 1] = [VenueRow::Mount(&RESOLVES_LIVE_BINDS_DEMO)];

/// The no-downgrade rule's MOUNT half: a bridge that resolves LIVE for a `live` account but whose
/// `mount` binds DEMO is shut down and the account does NOT RUN (`MountFailed`) — no engine, no
/// live entry, no reconcile handle.
#[test]
fn a_mount_that_binds_demo_for_a_live_account_is_shut_down_and_does_not_run() {
    let vars = HashMap::new();
    let (tx, _rx) = vike_exec::event_channel(8);
    let risk = budget();
    let live_tier = policy("okx", AccountTier::Live);
    let stopped = parts_stopped(crate::contract::contract_parts(
        &RESOLVES_LIVE_BINDS_DEMO,
        call(
            &RESOLVES_LIVE_BINDS_DEMO_REG,
            "okx",
            &vars,
            &tx,
            &risk,
            &live_tier,
            AccountTier::Live,
        ),
    ));
    assert_eq!(RESOLVES_LIVE_BINDS_DEMO.mounts.load(Ordering::SeqCst), 1, "the bridge was asked");
    assert_eq!(
        (stopped.tier, stopped.block),
        (Some(AccountTier::Live), ArmingBlock::MountFailed),
        "a DEMO bind for a live account does not run"
    );
    assert!(RESOLVES_LIVE_BINDS_DEMO_DETACHED.load(Ordering::SeqCst), "the client is shut down");
}

/// The default account with nothing stored — the planted venues here read no credential.
fn no_creds() -> vike_secrets::account_credentials::AccountCredentials<'static> {
    vike_secrets::account_credentials::AccountCredentials::none()
}
