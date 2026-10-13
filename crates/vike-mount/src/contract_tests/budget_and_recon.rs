//! Around the mount call: missing budget, an account that does not run, reconnect trigger, bound tier.
use super::*;

/// The pre-connect refusal fires BEFORE the bridge is asked for a session — and only for a
/// missing budget: the same row with one IS mounted.
#[test]
fn an_armed_row_without_a_budget_refuses_before_its_mount_is_called() {
    static ROW: Planted = Planted::new(
        "deribit",
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
        true,
    );
    static REG: [VenueRow; 1] = [VenueRow::Mount(&ROW)];
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let armed = policy("deribit", AccountTier::Demo);
    let out = mount_one(&REG, "deribit", "BTC-PERPETUAL", &tx, &mut live, None, Some(&armed));
    assert!(matches!(out, Err(crate::MountError::MissingRiskBudget { .. })));
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 0, "no venue session before the refusal");

    mount_one(&REG, "deribit", "BTC-PERPETUAL", &tx, &mut live, Some(&budget()), Some(&armed))
        .expect("a budgeted mount starts");
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 1, "with a budget the bridge is asked to mount");
}

/// An account no row names does not run, and returns above the bridge — a `live` one reaches it.
#[test]
fn a_disarmed_row_is_never_mounted() {
    static ROW: Planted = Planted::new(
        "aster",
        Resolution::Armed { tier: AccountTier::Live, held_below_live: None },
        true,
    );
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
    assert_eq!(
        ROW.mounts.load(Ordering::SeqCst),
        0,
        "an account that does not run returns above the bridge"
    );

    let armed = policy("aster", AccountTier::Live);
    mount_one(&REG, "aster", "BTCUSDT.P", &tx, &mut live, Some(&budget()), Some(&armed))
        .expect("an armed, budgeted mount starts");
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 1, "an armed tier reaches the bridge");
}

/// The reconnect trigger is handed to a row whose declaration takes it, and to no other. Both rows
/// ARM (a `demo` account, budgeted) and then build no client, so the bridge IS asked to mount —
/// which is where the trigger is handed over — and the account does not run (decision 0156).
#[test]
fn the_reconnect_trigger_reaches_only_a_row_that_takes_it() {
    static ROW: Planted = Planted::new(
        "okx",
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
        false,
    );
    static REG: [VenueRow; 1] = [VenueRow::Mount(&ROW)];
    static TAKES: Planted = Planted {
        takes_trigger: true,
        ..Planted::new(
            "binance",
            Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
            false,
        )
    };
    static TAKES_REG: [VenueRow; 1] = [VenueRow::Mount(&TAKES)];
    let (tx, _rx) = vike_exec::event_channel(8);
    let (trigger, _keep) = std::sync::mpsc::channel();
    let mut live = HashSet::new();
    let risk = budget();
    let armed = policy("okx", AccountTier::Demo);
    let vars = HashMap::new();
    let mut env = crate::MountEnv::new(&REG, &vars, &tx, &mut live);
    env.recon_enabled = true;
    env.recon_trigger = Some(trigger.clone());
    env.risk_profile = Some(&risk);
    env.policy = Some(&armed);
    let _ = crate::make_engine(&mut env, "okx", "BTC-USDT-SWAP");
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 1);
    assert!(!ROW.saw_trigger.load(Ordering::SeqCst), "takes_recon_trigger is false");

    let armed = policy("binance", AccountTier::Demo);
    let mut env = crate::MountEnv::new(&TAKES_REG, &vars, &tx, &mut live);
    env.recon_enabled = true;
    env.recon_trigger = Some(trigger);
    env.risk_profile = Some(&risk);
    env.policy = Some(&armed);
    let _ = crate::make_engine(&mut env, "binance", "BTCUSDT");
    assert_eq!(TAKES.mounts.load(Ordering::SeqCst), 1);
    assert!(TAKES.saw_trigger.load(Ordering::SeqCst), "takes_recon_trigger is true");
}

/// The spec's Finding 1, generic: the tier the parts carry is the BOUND tier — at both tiers, so
/// a fold that carried one fixed tier fails one half.
#[test]
fn the_identity_is_recorded_at_the_bound_tier() {
    for bound_tier in [AccountTier::Live, AccountTier::Demo] {
        let live = LiveExec {
            client: Box::new(NoopClient),
            bound_tier,
            grid: None,
            contract_size: None,
            margin_mode: None,
            leg_grids: vec![],
        };
        let parts = crate::contract::parts_from_live(
            "BTCUSDT.P",
            &[],
            live,
            None,
            None,
            DeclaredGridSource::PerSymbolFetch,
            vike_model::FeeSchedule::Free,
        );
        assert_eq!(parts.bound_tier, bound_tier, "the bound tier, never a CEX conjunct");
    }
}
