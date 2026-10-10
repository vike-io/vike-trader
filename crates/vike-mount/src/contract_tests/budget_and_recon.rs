//! Around the mount call: missing budget, paper ceiling, reconnect trigger, recon and bound tier.
use super::*;

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
    let out = mount_one(&REG, "deribit", "BTC-PERPETUAL", &tx, &mut live, None, Some(&armed));
    assert!(matches!(out, Err(crate::MountError::MissingRiskBudget { .. })));
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 0, "no venue session before the refusal");

    mount_one(&REG, "deribit", "BTC-PERPETUAL", &tx, &mut live, Some(&budget()), Some(&armed))
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
    let _ = mount_one(
        &REG,
        "aster",
        "BTCUSDT.P",
        &tx,
        &mut live,
        None,
        Some(&crate::MountPolicy::default()),
    );
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 0, "the paper ceiling returns above the bridge");

    let armed = policy("aster", VenueMode::Live);
    mount_one(&REG, "aster", "BTCUSDT.P", &tx, &mut live, Some(&budget()), Some(&armed))
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
    let vars = HashMap::new();
    let mut env = crate::MountEnv::new(&REG, &vars, &tx, &mut live);
    env.recon_enabled = true;
    env.recon_trigger = Some(trigger.clone());
    env.policy = Some(&armed);
    let _ = crate::make_engine(&mut env, "okx", "BTC-USDT-SWAP");
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 1);
    assert!(!ROW.saw_trigger.load(Ordering::SeqCst), "takes_recon_trigger is false");

    let armed = policy("binance", VenueMode::Demo);
    let mut env = crate::MountEnv::new(&TAKES_REG, &vars, &tx, &mut live);
    env.recon_enabled = true;
    env.recon_trigger = Some(trigger);
    env.policy = Some(&armed);
    let _ = crate::make_engine(&mut env, "binance", "BTCUSDT");
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
