use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use vike_bridge_core::venue_mount::{
    BookIdentity, ClockDecl, DeclaredGridSource, ExecOutcome, HeldBelowLive, LiveExec, MountInputs,
    MountOutcome, MountRequest, PaperCause, ProcessExclusive, Resolution, Tier, VenueDeclaration,
    VenueMount,
};
use vike_config::{ArmingBlock, VenueMode, VenuePolicy};

use crate::VenueRow;

struct NoopClient;
impl vike_exec::ExecutionClient for NoopClient {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

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
            process_exclusive: self.exclusive.then_some(ProcessExclusive {
                resource: "a planted resource",
                held_by_another: |label, holder| format!("{label} yields to {holder}"),
                already_claimed: |label| format!("{label}: already claimed"),
            }),
            takes_recon_trigger: self.takes_trigger,
            grid_source: DeclaredGridSource::InHand,
            book_identity: BookIdentity::Undeterminable { why: "a planted venue names no book" },
            clock: ClockDecl::NotWired {
                reason: "a planted venue reads no clock",
                unmeasured_risk: None,
            },
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
    for (cause, block) in [
        (PaperCause::NoCredentials, ArmingBlock::NoCredentials),
        (PaperCause::LiveCredentialsAbsent, ArmingBlock::LiveCredentialsAbsent),
        (PaperCause::ExecFlagUnset, ArmingBlock::ExecFlagUnset),
        (PaperCause::LiveOnlyArm, ArmingBlock::LiveOnlyArm),
        (PaperCause::SdkAbsent, ArmingBlock::SdkAbsent),
        (PaperCause::AccountNotInStore, ArmingBlock::AccountNotInStore),
        (PaperCause::NoLiveArm, ArmingBlock::NoLiveArm),
    ] {
        assert_eq!(fold(Resolution::Paper(cause), true), (VenueMode::Paper, block), "{cause:?}");
        assert_eq!(fold(Resolution::Paper(cause), false), (VenueMode::Paper, block), "{cause:?}");
    }
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

// Planted on `fxcm`, not `dukascopy`: the claim set is process-global, and no real test claims
// under this id, so a parallel test cannot hold it.
static EXCLUSIVE: Planted = Planted {
    exclusive: true,
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
// so it must not share an id with any other exclusive row here.
static EXCLUSIVE_LIVE: Planted = Planted {
    exclusive: true,
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
