use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use vike_bridge_core::venue_mount::{
    BookIdentity, DeclaredGridSource, ExecOutcome, HeldBelowLive, IdentityReport, LiveExec,
    MountInputs, MountOutcome, MountRequest, PaperCause, ProcessExclusive, Resolution, Tier,
    VenueDeclaration, VenueMount,
};
use vike_bridge_core::venue_mount_fixture::{PLANTED_DECLARATION, PlantedMount, planted_exclusive};
use vike_config::{ArmingBlock, VenueMode, VenuePolicy};
use vike_model::account_keys::AccountLabel;

use crate::VenueRow;

struct NoopClient;
impl vike_exec::ExecutionClient for NoopClient {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// Every `held_by_another` text a planted exclusive row rendered, so a test can see the refusal the
/// fold logged — and the holder it named.
static HELD_BY_ANOTHER: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn grid(tick: f64) -> vike_model::SymbolProperties {
    vike_model::SymbolProperties {
        tick_size: tick,
        step_size: 0.001,
        min_qty: 0.001,
        max_qty: 1_000_000.0,
        min_notional: 1.0,
        ..Default::default()
    }
}

/// A planted venue: its resolution, whether an armed mount goes live, and counters.
struct Planted {
    venue: &'static str,
    resolution: Resolution,
    live: bool,
    addresses_accounts: bool,
    exclusive: bool,
    /// The exclusive resource's name — REQUIRED on an exclusive row, whose declaration panics
    /// without one. The claim set is process-global and keyed on the resource, and under
    /// `cargo test` every test in this binary shares it, so each exclusive row names its OWN
    /// resource and probes its claim by that name; two rows share one only where a test says so.
    resource: Option<&'static str>,
    takes_trigger: bool,
    resolves: AtomicUsize,
    mounts: AtomicUsize,
    saw_trigger: AtomicBool,
}

impl Planted {
    const fn new(venue: &'static str, resolution: Resolution, live: bool) -> Self {
        Planted {
            venue,
            resolution,
            live,
            addresses_accounts: true,
            exclusive: false,
            resource: None,
            takes_trigger: false,
            resolves: AtomicUsize::new(0),
            mounts: AtomicUsize::new(0),
            saw_trigger: AtomicBool::new(false),
        }
    }
}

impl VenueMount for Planted {
    fn venue(&self) -> &'static str {
        self.venue
    }
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration {
            addresses_accounts: self.addresses_accounts,
            process_exclusive: self.exclusive.then(|| ProcessExclusive {
                resource: self.resource.expect("an exclusive planted row names its own resource"),
                held_by_another: |label, holder| {
                    let text = format!("{label} yields to {holder}");
                    HELD_BY_ANOTHER.lock().expect("the planted log").push(text.clone());
                    text
                },
                already_claimed: |label| format!("{label}: already claimed"),
            }),
            takes_recon_trigger: self.takes_trigger,
            grid_source: DeclaredGridSource::InHand,
            ..PLANTED_DECLARATION
        }
    }
    fn resolve(&self, _inputs: &MountInputs<'_>) -> Resolution {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        self.resolution
    }
    fn mount(&self, req: MountRequest<'_>) -> MountOutcome {
        self.mounts.fetch_add(1, Ordering::SeqCst);
        self.saw_trigger.store(req.recon_trigger.is_some(), Ordering::SeqCst);
        match self.resolve(&req.inputs) {
            Resolution::Armed { tier, .. } if self.live => MountOutcome {
                exec: ExecOutcome::Live(LiveExec {
                    client: Box::new(NoopClient),
                    bound_tier: tier,
                    grid: Some(grid(0.5)),
                    contract_size: Some(2.0),
                    margin_mode: Some(vike_model::MarginMode::Isolated),
                    leg_grids: vec![("ETHUSDT".to_string(), grid(0.01))],
                }),
                recon: None,
                identity: None,
            },
            _ => MountOutcome::paper(),
        }
    }
}

fn policy(venue: &str, mode: VenueMode) -> crate::MountPolicy {
    crate::MountPolicy { venues: VenuePolicy::default().declare(venue, mode), ..Default::default() }
}

fn budget() -> vike_exec::ProfileRisk {
    vike_exec::ProfileRisk {
        max_notional_per_order: Some(1_000_000.0),
        max_total_exposure: Some(1_000_000.0),
        ..Default::default()
    }
}

/// Every `PaperCause` names its own arming block, and a held-below-live reason is reported only
/// under a `live` ceiling — the whole of `resolution_to_arming`.
#[test]
fn paper_causes_and_held_reasons_map_to_their_blocks() {
    use crate::contract::resolution_to_arming as fold;
    let table = [
        (PaperCause::NoCredentials, ArmingBlock::NoCredentials),
        (PaperCause::LiveCredentialsAbsent, ArmingBlock::LiveCredentialsAbsent),
        (PaperCause::LiveTierNotWired, ArmingBlock::LiveTierNotWired),
        (PaperCause::ExecFlagUnset, ArmingBlock::ExecFlagUnset),
        (PaperCause::LiveOnlyArm, ArmingBlock::LiveOnlyArm),
        (PaperCause::SdkAbsent, ArmingBlock::SdkAbsent),
        (PaperCause::AccountNotInStore, ArmingBlock::AccountNotInStore),
        (PaperCause::NoLiveArm, ArmingBlock::NoLiveArm),
    ];
    // The table above must be the WHOLE of `PaperCause`. `position` is an exhaustive match with no
    // wildcard, so a new cause fails to compile until it is given a slot — and `seen` is sized to
    // the slots, so a slot with no table row fails the count below rather than slipping out of the
    // loop. A new cause therefore cannot exist without a block, a badge and a sentence.
    fn position(cause: PaperCause) -> usize {
        match cause {
            PaperCause::NoCredentials => 0,
            PaperCause::LiveCredentialsAbsent => 1,
            PaperCause::LiveTierNotWired => 2,
            PaperCause::ExecFlagUnset => 3,
            PaperCause::LiveOnlyArm => 4,
            PaperCause::SdkAbsent => 5,
            PaperCause::AccountNotInStore => 6,
            PaperCause::NoLiveArm => 7,
        }
    }
    let mut seen = [0usize; 8];
    for (cause, block) in table {
        assert_eq!(fold(Resolution::Paper(cause), true), (VenueMode::Paper, block), "{cause:?}");
        assert_eq!(fold(Resolution::Paper(cause), false), (VenueMode::Paper, block), "{cause:?}");
        seen[position(cause)] += 1;
    }
    assert_eq!(seen, [1; 8], "every PaperCause has exactly one row in the table above");
    let demo =
        Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) };
    assert_eq!(fold(demo, true), (VenueMode::Demo, ArmingBlock::DemoOnlyArm), "held under live");
    assert_eq!(fold(demo, false), (VenueMode::Demo, ArmingBlock::None), "nothing refused at demo");
    let testnet = Resolution::Armed {
        tier: Tier::Demo,
        held_below_live: Some(HeldBelowLive::LiveCredentialsAbsent),
    };
    assert_eq!(fold(testnet, true), (VenueMode::Demo, ArmingBlock::LiveCredentialsAbsent));
    assert_eq!(
        fold(Resolution::Armed { tier: Tier::Live, held_below_live: None }, true),
        (VenueMode::Live, ArmingBlock::None)
    );
}

static DEMO_ROW: Planted = Planted::new(
    "binance",
    Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) },
    true,
);
static DEMO_REG: [VenueRow; 1] = [VenueRow::Mount(&DEMO_ROW)];

#[test]
fn a_contract_row_arms_through_its_resolve() {
    let vars = HashMap::new();
    assert_eq!(
        crate::venue_arming_under(&DEMO_REG, "binance", &vars, VenueMode::Live),
        (VenueMode::Demo, ArmingBlock::DemoOnlyArm)
    );
    assert_eq!(
        crate::venue_arming_under(&DEMO_REG, "binance", &vars, VenueMode::Demo),
        (VenueMode::Demo, ArmingBlock::None)
    );
}

static SINGLE: Planted = Planted {
    addresses_accounts: false,
    ..Planted::new("okx", Resolution::Armed { tier: Tier::Demo, held_below_live: None }, false)
};
static SINGLE_REG: [VenueRow; 1] = [VenueRow::Mount(&SINGLE)];

/// Review Focus 5 — and its positive half: the SAME labelled account on a row that DOES address
/// accounts reaches `resolve`, so the refusal is the declaration's, not a blanket one.
#[test]
fn a_labelled_account_is_refused_before_resolve_on_a_single_account_venue() {
    let label = vike_model::account_keys::AccountLabel::parse("ALT").expect("a legal label");
    let before = SINGLE.resolves.load(Ordering::SeqCst);
    let row = crate::arming::account_arming_under(
        &SINGLE_REG,
        "okx",
        &label,
        &HashMap::new(),
        VenueMode::Live,
        None,
    );
    assert_eq!(row, (VenueMode::Paper, ArmingBlock::NoAccountSupport));
    assert_eq!(SINGLE.resolves.load(Ordering::SeqCst), before, "resolve is never asked");

    let asked_before = DEMO_ROW.resolves.load(Ordering::SeqCst);
    let addressable = crate::arming::account_arming_under(
        &DEMO_REG,
        "binance",
        &label,
        &HashMap::new(),
        VenueMode::Live,
        None,
    );
    assert_eq!(addressable, (VenueMode::Demo, ArmingBlock::DemoOnlyArm));
    assert!(
        DEMO_ROW.resolves.load(Ordering::SeqCst) > asked_before,
        "an addressing row asks its resolve for the labelled account"
    );
}

static ABSENT_REG: [VenueRow; 1] = [VenueRow::FeatureAbsent { venue: "ibkr", feature: "ibkr" }];

#[test]
fn a_feature_absent_row_is_paper_with_its_block_and_mounts_the_paper_client() {
    let vars = HashMap::new();
    assert_eq!(
        crate::venue_arming_under(&ABSENT_REG, "ibkr", &vars, VenueMode::Live),
        (VenueMode::Paper, ArmingBlock::FeatureAbsent)
    );
    // …and the ceiling is still read first: a `paper` line is `Disarmed`, as the legacy row said.
    assert_eq!(
        crate::venue_arming_under(&ABSENT_REG, "ibkr", &vars, VenueMode::Paper),
        (VenueMode::Paper, ArmingBlock::Disarmed)
    );
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let armed = policy("ibkr", VenueMode::Live);
    let (_engine, recon) = crate::make_engine(
        &ABSENT_REG,
        "ibkr",
        "AAPL",
        &vars,
        &tx,
        &mut live,
        true,
        None,
        None,
        None,
        Some(&armed),
    )
    .expect("paper never refuses");
    assert!(live.is_empty() && recon.is_none());
}

static LIVE_ROW: Planted =
    Planted::new("bybit", Resolution::Armed { tier: Tier::Demo, held_below_live: None }, true);
static LIVE_REG: [VenueRow; 1] = [VenueRow::Mount(&LIVE_ROW)];

/// The fold of a live outcome: grid, legs, contract size (multiplier) and margin mode — the last
/// is the mount's half of hyperliquid's isolated-only wiring, proved here over a planted venue.
#[test]
fn a_live_outcome_folds_its_grid_legs_multiplier_and_margin_mode_into_the_engine() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let legs = vec!["ETHUSDT".to_string()];
    let armed = policy("bybit", VenueMode::Demo);
    let (engine, _recon) = crate::make_engine_with_legs(
        &LIVE_REG,
        "bybit",
        "BTCUSDT",
        &legs,
        &HashMap::new(),
        &tx,
        &mut live,
        false,
        None,
        None,
        Some(&budget()),
        Some(&armed),
    )
    .expect("a budgeted live mount starts");
    assert!(live.contains("bybit"), "a live outcome is recorded live");
    assert!(engine.gate.limits.grid_by_symbol.contains_key("ETHUSDT"), "the leg grid is folded");
    assert_eq!(engine.gate.limits.tick_size, Some(0.5), "the mounted grid is the venue's");
    assert_eq!(engine.account.multiplier_of("BTCUSDT"), 2.0);
    assert_eq!(engine.account.default_margin_mode_of("BTCUSDT"), vike_model::MarginMode::Isolated);
    assert!(LIVE_ROW.mounts.load(Ordering::SeqCst) >= 1);
}

/// A live outcome that authenticated the LIVE tier — the real-money one. `LIVE_ROW` above is
/// hard-wired to the demo tier, so on its own it can never produce this answer.
static LIVE_TIER_ROW: Planted =
    Planted::new("bybit", Resolution::Armed { tier: Tier::Live, held_below_live: None }, true);
static LIVE_TIER_REG: [VenueRow; 1] = [VenueRow::Mount(&LIVE_TIER_ROW)];

/// An OUTCOME that built no venue client (the credentials are absent): its mount answers
/// `ExecOutcome::Paper`, so the shared tail runs with `live == false`.
static PAPER_OUTCOME_ROW: Planted =
    Planted::new("bybit", Resolution::Paper(PaperCause::NoCredentials), false);
static PAPER_OUTCOME_REG: [VenueRow; 1] = [VenueRow::Mount(&PAPER_OUTCOME_ROW)];

/// Every engine the mount builds publishes what stands behind its orders: a live outcome states
/// its tier, and a mount capped to paper says PAPER (the Trade window design, §4.3).
#[test]
fn a_mount_says_what_stands_behind_its_orders() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let demo = policy("bybit", VenueMode::Demo);
    let (engine, _recon) = crate::make_engine_with_legs(
        &LIVE_REG,
        "bybit",
        "BTCUSDT",
        &[],
        &HashMap::new(),
        &tx,
        &mut live,
        false,
        None,
        None,
        Some(&budget()),
        Some(&demo),
    )
    .expect("a budgeted demo mount starts");
    assert_eq!(engine.mode, vike_exec::EngineMode::Demo);

    let capped = policy("bybit", VenueMode::Paper);
    let (engine, _recon) = crate::make_engine_with_legs(
        &LIVE_REG,
        "bybit",
        "BTCUSDT",
        &[],
        &HashMap::new(),
        &tx,
        &mut live,
        false,
        None,
        None,
        Some(&budget()),
        Some(&capped),
    )
    .expect("a mount capped to paper starts");
    assert_eq!(engine.mode, vike_exec::EngineMode::Paper);
}

/// The mode of the engine `crate::make_engine_with_legs` builds on `registry` for `bybit` under
/// `mount_policy` — the one arrangement the two tests below vary only in their row and ceiling.
fn engine_mode_of(
    registry: &'static [VenueRow],
    mount_policy: crate::MountPolicy,
) -> vike_exec::EngineMode {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let (engine, _recon) = crate::make_engine_with_legs(
        registry,
        "bybit",
        "BTCUSDT",
        &[],
        &HashMap::new(),
        &tx,
        &mut live,
        false,
        None,
        None,
        Some(&budget()),
        Some(&mount_policy),
    )
    .expect("a budgeted mount starts");
    engine.mode
}

/// A live outcome that authenticated the LIVE tier publishes `Live` — the real-money label. The test
/// above runs `LIVE_ROW`, which is hard-wired to the demo tier, so on its own it can only ever see
/// `Demo` and a mutation that wrote `EngineMode::Demo` into `assemble_engine` passed it.
#[test]
fn a_live_outcome_on_the_live_tier_says_live() {
    assert_eq!(
        engine_mode_of(&LIVE_TIER_REG, policy("bybit", VenueMode::Live)),
        vike_exec::EngineMode::Live
    );
}

/// An OUTCOME that built no venue client — here, no credentials — reaches the shared tail with
/// `live == false` and says PAPER there, under a ceiling that would have allowed demo. This is a
/// different road to PAPER from the capped half of the test above, which returns before the tail.
#[test]
fn an_outcome_with_no_venue_client_says_paper_through_the_shared_tail() {
    assert_eq!(
        engine_mode_of(&PAPER_OUTCOME_REG, policy("bybit", VenueMode::Demo)),
        vike_exec::EngineMode::Paper
    );
}

/// The whole table: all eight `(live, tier)` cells of `engine_mode`. A client that was not built is
/// PAPER whatever tier is on record, and a built one is `Demo` only for the demo tier. A live client
/// whose tier went unrecorded reads as real money, and so does one recorded as paper (no mount
/// produces that cell): the label is wrong in the direction that costs least.
#[test]
fn engine_mode_reads_the_verdict_then_the_tier() {
    use vike_exec::EngineMode;
    let cells = [
        ((false, None), EngineMode::Paper),
        ((false, Some(VenueMode::Paper)), EngineMode::Paper),
        ((false, Some(VenueMode::Demo)), EngineMode::Paper),
        ((false, Some(VenueMode::Live)), EngineMode::Paper),
        ((true, None), EngineMode::Live),
        ((true, Some(VenueMode::Paper)), EngineMode::Live),
        ((true, Some(VenueMode::Demo)), EngineMode::Demo),
        ((true, Some(VenueMode::Live)), EngineMode::Live),
    ];
    assert_eq!(cells.len(), 8, "two verdicts times four tiers: none, paper, demo and live");
    for ((live, tier), expected) in cells {
        assert_eq!(crate::engine_mode(live, tier), expected, "engine_mode({live}, {tier:?})");
    }
}

/// The pre-connect refusal fires BEFORE the bridge is asked for a session — and only for a
/// missing budget: the same row with one IS mounted.
#[test]
fn an_armed_row_without_a_budget_refuses_before_its_mount_is_called() {
    static ROW: Planted = Planted::new(
        "deribit",
        Resolution::Armed { tier: Tier::Demo, held_below_live: None },
        true,
    );
    static REG: [VenueRow; 1] = [VenueRow::Mount(&ROW)];
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let armed = policy("deribit", VenueMode::Demo);
    let out = crate::make_engine(
        &REG,
        "deribit",
        "BTC-PERPETUAL",
        &HashMap::new(),
        &tx,
        &mut live,
        false,
        None,
        None,
        None,
        Some(&armed),
    );
    assert!(matches!(out, Err(crate::MountError::MissingRiskBudget { .. })));
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 0, "no venue session before the refusal");

    crate::make_engine(
        &REG,
        "deribit",
        "BTC-PERPETUAL",
        &HashMap::new(),
        &tx,
        &mut live,
        false,
        None,
        None,
        Some(&budget()),
        Some(&armed),
    )
    .expect("a budgeted mount starts");
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 1, "with a budget the bridge is asked to mount");
}

/// A `paper` ceiling returns above the bridge — and a `live` one reaches it.
#[test]
fn a_disarmed_row_is_never_mounted() {
    static ROW: Planted =
        Planted::new("aster", Resolution::Armed { tier: Tier::Live, held_below_live: None }, true);
    static REG: [VenueRow; 1] = [VenueRow::Mount(&ROW)];
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let _ = crate::make_engine(
        &REG,
        "aster",
        "BTCUSDT.P",
        &HashMap::new(),
        &tx,
        &mut live,
        false,
        None,
        None,
        None,
        Some(&crate::MountPolicy::default()),
    );
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 0, "the paper ceiling returns above the bridge");

    let armed = policy("aster", VenueMode::Live);
    crate::make_engine(
        &REG,
        "aster",
        "BTCUSDT.P",
        &HashMap::new(),
        &tx,
        &mut live,
        false,
        None,
        None,
        Some(&budget()),
        Some(&armed),
    )
    .expect("an armed, budgeted mount starts");
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 1, "an armed ceiling reaches the bridge");
}

/// The reconnect trigger is handed to a row whose declaration takes it, and to no other.
#[test]
fn the_reconnect_trigger_reaches_only_a_row_that_takes_it() {
    static ROW: Planted = Planted::new("okx", Resolution::Paper(PaperCause::NoCredentials), false);
    static REG: [VenueRow; 1] = [VenueRow::Mount(&ROW)];
    static TAKES: Planted = Planted {
        takes_trigger: true,
        ..Planted::new("binance", Resolution::Paper(PaperCause::NoCredentials), false)
    };
    static TAKES_REG: [VenueRow; 1] = [VenueRow::Mount(&TAKES)];
    let (tx, _rx) = vike_exec::event_channel(8);
    let (trigger, _keep) = std::sync::mpsc::channel();
    let mut live = HashSet::new();
    let armed = policy("okx", VenueMode::Demo);
    let _ = crate::make_engine(
        &REG,
        "okx",
        "BTC-USDT-SWAP",
        &HashMap::new(),
        &tx,
        &mut live,
        true,
        Some(trigger.clone()),
        None,
        None,
        Some(&armed),
    );
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 1);
    assert!(!ROW.saw_trigger.load(Ordering::SeqCst), "takes_recon_trigger is false");

    let armed = policy("binance", VenueMode::Demo);
    let _ = crate::make_engine(
        &TAKES_REG,
        "binance",
        "BTCUSDT",
        &HashMap::new(),
        &tx,
        &mut live,
        true,
        Some(trigger),
        None,
        None,
        Some(&armed),
    );
    assert_eq!(TAKES.mounts.load(Ordering::SeqCst), 1);
    assert!(TAKES.saw_trigger.load(Ordering::SeqCst), "takes_recon_trigger is true");
}

/// Review Focus 4.
#[test]
fn a_paper_outcome_keeps_the_bridges_recon() {
    struct Stub;
    impl vike_exec::recon::ReconClient for Stub {
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
    let outcome =
        MountOutcome { exec: ExecOutcome::Paper, recon: Some(Box::new(Stub)), identity: None };
    let parts = crate::contract::parts_from_outcome(
        "polymarket",
        "",
        &[],
        outcome,
        DeclaredGridSource::NoGrid,
        vike_model::FeeSchedule::Free,
    );
    assert!(!parts.live && parts.recon.is_some() && parts.record_tier.is_none());
}

/// The spec's Finding 1, generic: the recorded tier is the BOUND tier — at both tiers, so a fold
/// that recorded one fixed tier fails one half.
#[test]
fn the_identity_is_recorded_at_the_bound_tier() {
    for (bound_tier, recorded) in [(Tier::Live, VenueMode::Live), (Tier::Demo, VenueMode::Demo)] {
        let outcome = MountOutcome {
            exec: ExecOutcome::Live(LiveExec {
                client: Box::new(NoopClient),
                bound_tier,
                grid: None,
                contract_size: None,
                margin_mode: None,
                leg_grids: vec![],
            }),
            recon: None,
            identity: None,
        };
        let parts = crate::contract::parts_from_outcome(
            "aster",
            "BTCUSDT.P",
            &[],
            outcome,
            DeclaredGridSource::PerSymbolFetch,
            vike_model::FeeSchedule::Free,
        );
        assert_eq!(parts.record_tier, Some(recorded), "the bound tier, never a CEX conjunct");
    }
}

// Planted on `fxcm`, not `dukascopy`, under a resource no other test claims: the claim set is
// process-global and keyed on the resource, so a parallel test cannot hold it.
static EXCLUSIVE: Planted = Planted {
    exclusive: true,
    resource: Some("the claim-release test's resource"),
    ..Planted::new("fxcm", Resolution::Armed { tier: Tier::Demo, held_below_live: None }, false)
};
static EXCLUSIVE_REG: [VenueRow; 1] = [VenueRow::Mount(&EXCLUSIVE)];

/// Review Focus 3: an armed mount that ends PAPER releases its claim, so the next mount reaches
/// the bridge again.
#[test]
fn the_fold_keeps_the_claim_only_for_a_live_outcome() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let armed = policy("fxcm", VenueMode::Demo);
    for _ in 0..2 {
        let _ = crate::make_engine(
            &EXCLUSIVE_REG,
            "fxcm",
            "EURUSD",
            &HashMap::new(),
            &tx,
            &mut live,
            false,
            None,
            None,
            Some(&budget()),
            Some(&armed),
        );
    }
    assert_eq!(
        EXCLUSIVE.mounts.load(Ordering::SeqCst),
        2,
        "a released claim lets the next mount in"
    );
}

// Planted on `ig` for the claim-KEEPING half: a kept claim lasts for the life of the test process,
// so its resource must not be shared with any other exclusive row here.
static EXCLUSIVE_LIVE: Planted = Planted {
    exclusive: true,
    resource: Some("the claim-keeping test's resource"),
    ..Planted::new("ig", Resolution::Armed { tier: Tier::Demo, held_below_live: None }, true)
};
static EXCLUSIVE_LIVE_REG: [VenueRow; 1] = [VenueRow::Mount(&EXCLUSIVE_LIVE)];

/// …and the other half: a LIVE outcome KEEPS the claim, so a second mount in the same process is
/// refused before its bridge is asked and lands on paper.
#[test]
fn a_live_exclusive_outcome_keeps_the_claim_and_refuses_the_next_mount() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let armed = policy("ig", VenueMode::Demo);
    let mut first = HashSet::new();
    crate::make_engine(
        &EXCLUSIVE_LIVE_REG,
        "ig",
        "EURUSD",
        &HashMap::new(),
        &tx,
        &mut first,
        false,
        None,
        None,
        Some(&budget()),
        Some(&armed),
    )
    .expect("the first mount starts");
    assert!(first.contains("ig"), "the first mount went live and kept the claim");
    let mut second = HashSet::new();
    crate::make_engine(
        &EXCLUSIVE_LIVE_REG,
        "ig",
        "EURUSD",
        &HashMap::new(),
        &tx,
        &mut second,
        false,
        None,
        None,
        Some(&budget()),
        Some(&armed),
    )
    .expect("a refused claim is paper, not an error");
    assert!(second.is_empty(), "the second mount is paper");
    assert_eq!(EXCLUSIVE_LIVE.mounts.load(Ordering::SeqCst), 1, "the kept claim refused it first");
}

// ---- the lookups that answer a contract row from its declaration --------------------------------

/// A book identity no legacy table carries for this id.
const PLANTED_BOOK: BookIdentity = BookIdentity::Named {
    prefix: "PLANTED",
    demo_tiers: &["DEMO"],
    live_tiers: &["LIVE"],
    name_suffixes: &["ACCOUNT_ID"],
    evm_key_suffixes: &[],
};

/// A contract row that is nothing but its declaration.
static DECLARES: PlantedMount = PlantedMount {
    declaration: VenueDeclaration {
        grid_source: DeclaredGridSource::PerSymbolFetch,
        book_identity: PLANTED_BOOK,
        ..PLANTED_DECLARATION
    },
    ..PlantedMount::new("planted", Resolution::Paper(PaperCause::NoLiveArm))
};
static DECLARES_REG: [VenueRow; 1] = [VenueRow::Mount(&DECLARES)];

/// `book_identity_for` and `declared_grid_source` answer a contract row from its bridge's
/// declaration. Neither legacy table carries `planted`, so a lookup that fell through to them would
/// answer `None` and `NoGrid` instead.
#[test]
fn a_contract_rows_book_identity_and_grid_source_come_from_its_declaration() {
    assert_eq!(
        crate::book_identity::book_identity_for(&DECLARES_REG, "planted"),
        Some(PLANTED_BOOK)
    );
    assert_eq!(
        crate::symbol_grid::declared_grid_source(&DECLARES_REG, "planted"),
        DeclaredGridSource::PerSymbolFetch
    );
}

// ---- the claim is keyed on the RESOURCE, not the venue ------------------------------------------

const SHARED: &str = "a resource two planted venues share";

static SHARING_FIRST: Planted = Planted {
    exclusive: true,
    resource: Some(SHARED),
    ..Planted::new("alpaca", Resolution::Armed { tier: Tier::Demo, held_below_live: None }, true)
};
static SHARING_SECOND: Planted = Planted {
    exclusive: true,
    resource: Some(SHARED),
    ..Planted::new("oanda", Resolution::Armed { tier: Tier::Demo, held_below_live: None }, true)
};
static SHARING_REG: [VenueRow; 2] =
    [VenueRow::Mount(&SHARING_FIRST), VenueRow::Mount(&SHARING_SECOND)];

/// Two venues that declare ONE resource exclude each other: the first to go live keeps the claim,
/// and the second is refused before its bridge is asked.
#[test]
fn two_venues_that_declare_one_resource_exclude_each_other() {
    let (tx, _rx) = vike_exec::event_channel(8);
    let both = crate::MountPolicy {
        venues: VenuePolicy::default()
            .declare("alpaca", VenueMode::Demo)
            .declare("oanda", VenueMode::Demo),
        ..Default::default()
    };
    let mut live = HashSet::new();
    for (venue, symbol) in [("alpaca", "AAPL"), ("oanda", "EURUSD")] {
        crate::make_engine(
            &SHARING_REG,
            venue,
            symbol,
            &HashMap::new(),
            &tx,
            &mut live,
            false,
            None,
            None,
            Some(&budget()),
            Some(&both),
        )
        .expect("a refused claim is paper, not an error");
    }
    assert!(live.contains("alpaca"), "the first venue went live and kept the shared resource");
    assert!(!live.contains("oanda"), "the second venue is refused: its resource is taken");
    assert_eq!(SHARING_SECOND.mounts.load(Ordering::SeqCst), 0, "…before its bridge is asked");
}

// ---- the exclusive branches that start nothing --------------------------------------------------

static YIELDS: Planted = Planted {
    exclusive: true,
    resource: Some("the yielding test's resource"),
    ..Planted::new(
        "hyperliquid",
        Resolution::Armed { tier: Tier::Demo, held_below_live: None },
        true,
    )
};
static YIELDS_REG: [VenueRow; 1] = [VenueRow::Mount(&YIELDS)];

/// `SidecarHeldElsewhere`: with a labelled account named, the DEFAULT account yields. The fold
/// computes the holder, renders the bridge's own `held_by_another` refusal with it, and never asks
/// the bridge to mount.
#[test]
fn an_account_that_yields_the_resource_is_refused_with_its_holder_and_never_mounted() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let named = crate::MountPolicy {
        venues: VenuePolicy::default().declare("hyperliquid", VenueMode::Demo).declare_account(
            "hyperliquid",
            &alt,
            VenueMode::Demo,
        ),
        ..Default::default()
    };
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    crate::make_engine(
        &YIELDS_REG,
        "hyperliquid",
        "BTC",
        &HashMap::new(),
        &tx,
        &mut live,
        false,
        None,
        None,
        Some(&budget()),
        Some(&named),
    )
    .expect("a yielding account is paper, not an error");
    assert!(live.is_empty(), "the default account yields");
    assert_eq!(
        YIELDS.mounts.load(Ordering::SeqCst),
        0,
        "the yielding account never reaches the bridge"
    );
    let expected = format!("{} yields to ALT", AccountLabel::Default);
    assert!(
        HELD_BY_ANOTHER.lock().expect("the planted log").contains(&expected),
        "the refusal renders the holder the policy named: `{expected}`"
    );
}

const PAPER_RESOURCE: &str = "the paper-answer test's resource";

static PAPER_EXCLUSIVE: Planted = Planted {
    exclusive: true,
    resource: Some(PAPER_RESOURCE),
    ..Planted::new("deribit", Resolution::Paper(PaperCause::NoCredentials), false)
};
static PAPER_EXCLUSIVE_REG: [VenueRow; 1] = [VenueRow::Mount(&PAPER_EXCLUSIVE)];

/// An exclusive row whose arming answer is PAPER is mounted without touching the claim: the bridge
/// is asked (it logs its own refusal) even while the resource is held elsewhere for the whole
/// mount — a fold that claimed first would have been refused and never asked it.
#[test]
fn a_paper_answer_on_an_exclusive_row_mounts_without_touching_the_claim() {
    let held = crate::exclusive::claim(PAPER_RESOURCE).expect("the test holds the resource");
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    crate::make_engine(
        &PAPER_EXCLUSIVE_REG,
        "deribit",
        "BTC-PERPETUAL",
        &HashMap::new(),
        &tx,
        &mut live,
        false,
        None,
        None,
        None,
        Some(&policy("deribit", VenueMode::Demo)),
    )
    .expect("paper never refuses");
    assert_eq!(PAPER_EXCLUSIVE.mounts.load(Ordering::SeqCst), 1, "the bridge is asked");
    assert!(live.is_empty());
    drop(held);
}

// ---- THE CEILING INTERLOCK ----------------------------------------------------------------------

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

/// A bridge that goes past its ceiling: `resolve` answers `resolution` and `mount` binds `binds`,
/// whatever `MountInputs::live_permitted` says — the two defects the interlock exists for. Its
/// outcome carries a reconcile client and an identity report, and it declares an exclusive
/// resource, so a test can see all three dropped.
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

/// THE CEILING INTERLOCK, resolve half: a bridge whose `resolve` answers LIVE under a `demo`
/// ceiling mounts PAPER — it is never asked for a session and its resource is never claimed —
/// while the arming projection still shows its wrong answer. The positive half: under a `live`
/// ceiling the same answer mounts live.
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
    crate::make_engine(
        &RESOLVES_LIVE_REG,
        "okx",
        "BTC-USDT-SWAP",
        &vars,
        &tx,
        &mut live,
        false,
        None,
        None,
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

    crate::make_engine(
        &RESOLVES_LIVE_REG,
        "okx",
        "BTC-USDT-SWAP",
        &vars,
        &tx,
        &mut live,
        false,
        None,
        None,
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

/// THE CEILING INTERLOCK, mount half: a bridge that resolves DEMO but whose `mount` binds the LIVE
/// tier under a `demo` ceiling is shut down and mounted PAPER — no live entry, no identity record,
/// no reconcile handle, and its claim released. The positive half: under a `live` ceiling the
/// same outcome is kept.
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
    crate::make_engine(
        &BINDS_LIVE_REG,
        "bybit",
        "BTCUSDT",
        &vars,
        &tx,
        &mut live,
        false,
        None,
        None,
        Some(&risk),
        Some(&demo),
    )
    .expect("refused to paper, not an error");
    assert!(live.is_empty(), "no live entry under a demo ceiling");

    // (3) The positive half: a live ceiling keeps the live-bound outcome.
    crate::make_engine(
        &BINDS_LIVE_REG,
        "bybit",
        "BTCUSDT",
        &vars,
        &tx,
        &mut live,
        false,
        None,
        None,
        Some(&risk),
        Some(&policy("bybit", VenueMode::Live)),
    )
    .expect("a live ceiling mounts");
    assert!(live.contains("bybit"), "under a live ceiling the live-bound outcome is kept");
}

/// A bridge whose EXEC outcome keeps to the ceiling and whose IDENTITY report does not: `mount`
/// returns a live client bound to DEMO with a reconcile handle (`live_exec`), or a paper exec — the
/// shape of hyperliquid's confirmed `userRole` answer that survives a failed `meta` — and in both
/// the identity report carries `identity_tier`, whatever `MountInputs::live_permitted` says.
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

/// THE CEILING INTERLOCK also covers the IDENTITY RECORD: a bridge whose `mount` reports an account
/// identity at the LIVE tier under a `demo` ceiling is refused whole — mounted PAPER, no identity
/// recorded, no reconcile handle kept, its client shut down — whether or not its exec outcome kept
/// to the ceiling. Before this, only an exec outcome BOUND to the live tier was refused, and the
/// identity passed through `parts_from_outcome` and was recorded as a LIVE-tier account.
///
/// The controls pin what the refusal is not: a DEMO identity under the same ceiling is kept (the
/// guard is the tier, not the identity), and a LIVE identity under a `live` ceiling is kept (the
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

/// **Every mount is handed the process-wide HALT sentinel's path as DATA** (decision 0099): this is
/// the one place the memoized resolver is consulted for a bridge, so the live exec clients — which
/// no longer resolve it themselves — watch exactly the file the paper books and the daemon's
/// startup advisory name. A path that differed here would be a live venue honouring one sentinel
/// while the operator was told another.
#[test]
fn process_facts_hand_every_mount_the_process_wide_sentinel_path() {
    let facts = crate::contract::mount_process_facts();
    assert!(!facts.halt_path.as_os_str().is_empty(), "an empty sentinel path watches nothing");
    assert_eq!(
        facts.halt_path,
        vike_bridge_core::halt::halt_path_from_env(),
        "the mount's sentinel must be the process's one resolved sentinel"
    );
    // …and the PROBE's view resolves nothing: it starts no client, and the process-wide resolver
    // memoizes and logs, which a question must not do from a root that never declared a project.
    assert!(crate::contract::process_facts().halt_path.as_os_str().is_empty());
    // The directories are the same either way.
    let probe = crate::contract::process_facts();
    assert_eq!((&facts.state_dir, &facts.bin_dir), (&probe.state_dir, &probe.bin_dir));
}

// ── a LABELLED account the fan-out never mounts is still spoken for ──────────────────────────────
//
// `make_engine_accounts` mounts the DEFAULT account unconditionally and a LABELLED account only
// when it armed (`accounts_to_mount`), so a labelled account that resolved paper for want of a
// usable key set never reaches its bridge's `mount` — where the default account's "a live-tier key
// set is stored and unused" line is said — and the same fact on a labelled account was visible on
// `vike-backend venues` and nowhere in the daemon's log. The fold now asks the bridge to speak for
// each such account through `VenueMount::report_unmounted_account` — and only those: not the
// default account (its `mount` says it), not an account that armed (its `mount` says it), and not
// an account whose ceiling is `paper` (the ceiling answers above every bridge, and a disarmed
// account is silent by design). The call returns nothing, so it cannot change what mounts; the test
// holds the mount set unchanged. A planted venue, not a real bridge: the property is the FOLD's,
// and each real bridge's own `mount_tests.rs` holds what it says when asked.

/// A roster venue whose `resolve` is scripted per account label and which records every account it
/// was asked to speak for, with the `live_permitted` bool it was handed.
struct SpeaksForUnmounted {
    spoken_for: Mutex<Vec<(String, bool)>>,
}

impl VenueMount for SpeaksForUnmounted {
    fn venue(&self) -> &'static str {
        "alpaca"
    }
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration { addresses_accounts: true, ..PLANTED_DECLARATION }
    }
    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution {
        match inputs.account.text() {
            Some("ARMED") => Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            },
            Some("UNWIRED" | "LIVECAP") => Resolution::Paper(PaperCause::LiveTierNotWired),
            Some("NOSDK") => Resolution::Paper(PaperCause::SdkAbsent),
            _ => Resolution::Paper(PaperCause::NoCredentials),
        }
    }
    fn mount(&self, _req: MountRequest<'_>) -> MountOutcome {
        MountOutcome::paper()
    }
    fn report_unmounted_account(&self, inputs: &MountInputs<'_>) {
        self.spoken_for
            .lock()
            .expect("lock")
            .push((inputs.account.to_string(), inputs.live_permitted));
    }
}

static SPEAKS: SpeaksForUnmounted = SpeaksForUnmounted { spoken_for: Mutex::new(Vec::new()) };
static SPEAKS_REG: [VenueRow; 1] = [VenueRow::Mount(&SPEAKS)];

/// One fan-out over the planted venue; returns the labels it mounted (the default account first).
fn fan_out(policy: &crate::MountPolicy) -> Vec<String> {
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let mounted = crate::make_engine_accounts(
        &SPEAKS_REG,
        "alpaca",
        &[],
        &[],
        &HashMap::new(),
        &tx,
        &mut live,
        false,
        None,
        None,
        Some(&budget()),
        Some(policy),
    )
    .expect("a planted paper mount never refuses");
    assert!(live.is_empty(), "nothing here may mount live: {live:?}");
    mounted.into_iter().map(|(l, _)| l.to_string()).collect()
}

#[test]
fn only_a_labelled_account_that_is_never_mounted_is_spoken_for() {
    let label = |text: &str| AccountLabel::parse(text).expect("a legal label");
    // The venue line is `live`, so an account line of `live` is permitted and one of `demo` is capped.
    let policy = crate::MountPolicy {
        venues: VenuePolicy::default()
            .declare("alpaca", VenueMode::Live)
            .declare_account("alpaca", &label("UNWIRED"), VenueMode::Demo)
            .declare_account("alpaca", &label("NOCREDS"), VenueMode::Demo)
            .declare_account("alpaca", &label("NOSDK"), VenueMode::Demo)
            .declare_account("alpaca", &label("LIVECAP"), VenueMode::Live)
            .declare_account("alpaca", &label("ARMED"), VenueMode::Demo)
            .declare_account("alpaca", &label("DISARMED"), VenueMode::Paper),
        ..Default::default()
    };
    SPEAKS.spoken_for.lock().expect("lock").clear();
    let mounted = fan_out(&policy);

    // What mounts is exactly what `accounts_to_mount` has always selected: the default account and
    // the one labelled account that armed. Speaking for the others changed nothing.
    assert_eq!(mounted, ["DEFAULT", "ARMED"], "the mount set is unchanged");

    let mut spoken: Vec<(String, bool)> = SPEAKS.spoken_for.lock().expect("lock").clone();
    spoken.sort();
    assert_eq!(
        spoken,
        [
            // A `live` ceiling hands the bridge `live_permitted = true`; a `demo` one, false.
            ("LIVECAP".to_string(), true),
            ("NOCREDS".to_string(), false),
            ("NOSDK".to_string(), false),
            ("UNWIRED".to_string(), false),
        ],
        "exactly the labelled accounts that resolved paper for want of keys (or of the shim), under \
         a ceiling above `paper` — not the default account (its `mount` speaks), not ARMED \
         (mounted), not DISARMED (its ceiling is `paper`)"
    );

    // …and once per start per account: a second fan-out is a second start.
    SPEAKS.spoken_for.lock().expect("lock").clear();
    fan_out(&policy);
    assert_eq!(SPEAKS.spoken_for.lock().expect("lock").len(), 4, "once per account per fan-out");
}
