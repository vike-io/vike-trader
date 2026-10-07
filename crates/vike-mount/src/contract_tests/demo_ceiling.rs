//! THE CEILING INTERLOCK: a bridge past a `demo` ceiling (resolve, bind or identity) mounts paper.
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

struct StubRecon;
impl vike_exec::recon::ReconClient for StubRecon {
    fn fetch_order_status_reports(
        &self,
        _since: i64,
    ) -> Result<Vec<vike_model::OrderStatusReport>, String> {
        Ok(vec![])
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<vike_model::FillReport>, String> {
        Ok(vec![])
    }
    fn fetch_position_status_reports(
        &self,
    ) -> Result<Vec<vike_model::PositionStatusReport>, String> {
        Ok(vec![])
    }
}

/// A bridge past its ceiling: `resolve` answers `resolution` and `mount` binds `binds` whatever
/// `MountInputs::live_permitted` says (the two defects the interlock exists for). It carries a
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
            recon: Some(Box::new(StubRecon)),
            identity: Some(IdentityReport {
                book: "0xplanted".to_string(),
                evidence: "a planted answer",
                tier: self.binds,
            }),
        }
    }
}

static DEFAULT_ACCOUNT: AccountLabel = AccountLabel::Default;

/// `contract_parts`' inputs for a DEFAULT-account mount of `venue` at `mode`.
fn call<'a>(
    registry: &'static [VenueRow],
    venue: &'a str,
    vars: &'a HashMap<String, String>,
    events: &'a vike_exec::EventSender,
    risk: &'a vike_exec::ProfileRisk,
    policy: &'a crate::MountPolicy,
    mode: VenueMode,
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
        mode,
        live_permitted: crate::ceiling_permits_live(mode),
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

/// Resolve half: a `resolve` answering LIVE under a `demo` ceiling mounts PAPER (no session, no
/// claim) while the arming projection still shows the wrong answer; under `live` it mounts live.
#[test]
fn a_resolve_that_answers_live_under_a_demo_ceiling_mounts_paper_without_a_session() {
    let vars = HashMap::new();
    assert_eq!(
        crate::venue_arming_under(&RESOLVES_LIVE_REG, "okx", &vars, VenueMode::Demo),
        (VenueMode::Live, ArmingBlock::None),
        "the projection is NOT clamped, so the arming screen shows the bridge's wrong answer"
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
    assert!(live.is_empty(), "no live entry under a demo ceiling");
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
    .expect("a live ceiling mounts");
    assert!(live.contains("okx"), "under a live ceiling the same answer mounts live");
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

/// Mount half: resolving DEMO but binding LIVE under a `demo` ceiling is shut down and mounted
/// PAPER (no live entry, identity or reconcile handle; claim released); under `live` it is kept.
#[test]
fn a_mount_that_binds_live_under_a_demo_ceiling_is_shut_down_and_mounts_paper() {
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
    assert!(live.is_empty(), "no live entry under a demo ceiling");

    // (3) The positive half: a live ceiling keeps the live-bound outcome.
    mount_one(
        &BINDS_LIVE_REG,
        "bybit",
        "BTCUSDT",
        &tx,
        &mut live,
        Some(&risk),
        Some(&policy("bybit", VenueMode::Live)),
    )
    .expect("a live ceiling mounts");
    assert!(live.contains("bybit"), "under a live ceiling the live-bound outcome is kept");
}

/// EXEC keeps to the ceiling, IDENTITY does not: a DEMO-bound live client with recon
/// (`live_exec`) or a paper exec (hyperliquid's `userRole` answer surviving a failed `meta`),
/// each reporting identity at `identity_tier` whatever `MountInputs::live_permitted` says.
struct IdentityOverreach {
    venue: &'static str,
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
        Resolution::Armed { tier: Tier::Demo, held_below_live: None }
    }
    fn mount(&self, _req: MountRequest<'_>) -> MountOutcome {
        self.mounts.fetch_add(1, Ordering::SeqCst);
        let (exec, recon) = if self.live_exec {
            let live = LiveExec {
                client: Box::new(Detaching(self.detached)),
                bound_tier: Tier::Demo,
                grid: None,
                contract_size: None,
                margin_mode: None,
                leg_grids: vec![],
            };
            (
                ExecOutcome::Live(live),
                Some(Box::new(StubRecon) as Box<dyn vike_exec::recon::ReconClient>),
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
    live_exec: true,
    identity_tier: Tier::Live,
    detached: &IDENTITY_LIVE_WITH_CLIENT_DETACHED,
    mounts: AtomicUsize::new(0),
};
static IDENTITY_LIVE_WITH_CLIENT_REG: [VenueRow; 1] = [VenueRow::Mount(&IDENTITY_LIVE_WITH_CLIENT)];

static IDENTITY_LIVE_PAPER_DETACHED: AtomicBool = AtomicBool::new(false);
static IDENTITY_LIVE_PAPER: IdentityOverreach = IdentityOverreach {
    venue: "hyperliquid",
    live_exec: false,
    identity_tier: Tier::Live,
    detached: &IDENTITY_LIVE_PAPER_DETACHED,
    mounts: AtomicUsize::new(0),
};
static IDENTITY_LIVE_PAPER_REG: [VenueRow; 1] = [VenueRow::Mount(&IDENTITY_LIVE_PAPER)];

static IDENTITY_DEMO_DETACHED: AtomicBool = AtomicBool::new(false);
static IDENTITY_DEMO: IdentityOverreach = IdentityOverreach {
    venue: "okx",
    live_exec: true,
    identity_tier: Tier::Demo,
    detached: &IDENTITY_DEMO_DETACHED,
    mounts: AtomicUsize::new(0),
};
static IDENTITY_DEMO_REG: [VenueRow; 1] = [VenueRow::Mount(&IDENTITY_DEMO)];

/// Identity half: a LIVE-tier identity under a `demo` ceiling is refused whole (PAPER, no
/// identity, no reconcile handle, client shut down) even when the exec kept to the ceiling —
/// otherwise `parts_from_outcome` records it as a LIVE-tier account. Controls: a DEMO identity
/// under the same ceiling is kept (the guard is the tier), and a LIVE one under `live` (the
/// guard is the ceiling).
#[test]
fn an_identity_at_the_live_tier_under_a_demo_ceiling_is_refused_and_the_mount_is_paper() {
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

    // Control 1: a DEMO identity under the same ceiling is kept, beside its live client.
    let demo = policy("okx", VenueMode::Demo);
    let parts = crate::contract::contract_parts(
        &IDENTITY_DEMO,
        call(&IDENTITY_DEMO_REG, "okx", &vars, &tx, &risk, &demo, VenueMode::Demo),
    )
    .expect("a demo identity under a demo ceiling mounts");
    assert!(parts.live, "the demo-bound client is kept");
    assert_eq!(
        parts.identity,
        Some(("0xplanted".to_string(), "a planted answer", VenueMode::Demo)),
        "…and the DEMO identity is recorded"
    );
    assert!(!IDENTITY_DEMO_DETACHED.load(Ordering::SeqCst), "nothing was shut down");

    // Control 2: under a `live` ceiling the LIVE identity is kept.
    let live = policy("hyperliquid", VenueMode::Live);
    let parts = crate::contract::contract_parts(
        &IDENTITY_LIVE_PAPER,
        call(&IDENTITY_LIVE_PAPER_REG, "hyperliquid", &vars, &tx, &risk, &live, VenueMode::Live),
    )
    .expect("a live ceiling mounts");
    assert_eq!(
        parts.identity,
        Some(("0xplanted".to_string(), "a planted answer", VenueMode::Live)),
        "under a live ceiling the LIVE identity is recorded"
    );
}
