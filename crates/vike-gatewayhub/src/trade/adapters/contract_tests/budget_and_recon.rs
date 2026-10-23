//! Around the start call: missing budget, an account that does not run, reconnect trigger, bound tier.
use super::*;

/// The pre-connect refusal fires BEFORE the bridge is asked for a session — and only for a
/// missing budget: the same row with one IS started.
#[test]
fn an_armed_row_without_a_budget_refuses_before_its_start_is_called() {
    static ROW: Planted = Planted::new(
        "deribit",
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
        true,
    );
    static REG: [VenueRow; 1] = [VenueRow::Adapter(&ROW)];
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let armed = policy("deribit", AccountTier::Demo);
    let out = start_one(&REG, "deribit", "BTC-PERPETUAL", &tx, &mut live, None, Some(&armed));
    assert!(matches!(out, Err(vike_tradehub_core::EngineError::MissingRiskBudget { .. })));
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 0, "no venue session before the refusal");

    start_one(&REG, "deribit", "BTC-PERPETUAL", &tx, &mut live, Some(&budget()), Some(&armed))
        .expect("a budgeted mount starts");
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 1, "with a budget the bridge is asked to mount");
}

/// An account no row names does not run, and returns above the bridge — a `live` one reaches it.
#[test]
fn a_disarmed_row_is_never_started() {
    static ROW: Planted = Planted::new(
        "aster",
        Resolution::Armed { tier: AccountTier::Live, held_below_live: None },
        true,
    );
    static REG: [VenueRow; 1] = [VenueRow::Adapter(&ROW)];
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let _ = start_one(
        &REG,
        "aster",
        "BTCUSDT.P",
        &tx,
        &mut live,
        None,
        Some(&vike_tradehub_core::AdapterPolicy::default()),
    );
    assert_eq!(
        ROW.mounts.load(Ordering::SeqCst),
        0,
        "an account that does not run returns above the bridge"
    );

    let armed = policy("aster", AccountTier::Live);
    start_one(&REG, "aster", "BTCUSDT.P", &tx, &mut live, Some(&budget()), Some(&armed))
        .expect("an armed, budgeted mount starts");
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 1, "an armed tier reaches the bridge");
}

/// The reconnect trigger is handed to a row whose declaration takes it, and to no other. Both rows
/// ARM (a `demo` account, budgeted) and then build no client, so the bridge IS asked to start —
/// which is where the trigger is handed over — and the account does not run (decision 0156).
#[test]
fn the_reconnect_trigger_reaches_only_a_row_that_takes_it() {
    static ROW: Planted = Planted::new(
        "okx",
        Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
        false,
    );
    static REG: [VenueRow; 1] = [VenueRow::Adapter(&ROW)];
    static TAKES: Planted = Planted {
        takes_trigger: true,
        ..Planted::new(
            "binance",
            Resolution::Armed { tier: AccountTier::Demo, held_below_live: None },
            false,
        )
    };
    static TAKES_REG: [VenueRow; 1] = [VenueRow::Adapter(&TAKES)];
    let (tx, _rx) = vike_exec::event_channel(8);
    let (trigger, _keep) = std::sync::mpsc::channel();
    let mut live = HashSet::new();
    let risk = budget();
    let armed = policy("okx", AccountTier::Demo);
    let vars = HashMap::new();
    let rig = Rig::new(&REG, &vars, Some(&armed));
    let mut env = rig.env(&tx, &mut live);
    env.recon_enabled = true;
    env.recon_trigger = Some(trigger.clone());
    env.risk_profile = Some(&risk);
    env.policy = Some(&armed);
    let _ = vike_tradehub_core::make_engine(&mut env, "okx", "BTC-USDT-SWAP");
    assert_eq!(ROW.mounts.load(Ordering::SeqCst), 1);
    assert!(!ROW.saw_trigger.load(Ordering::SeqCst), "takes_recon_trigger is false");

    let armed = policy("binance", AccountTier::Demo);
    let rig = Rig::new(&TAKES_REG, &vars, Some(&armed));
    let mut env = rig.env(&tx, &mut live);
    env.recon_enabled = true;
    env.recon_trigger = Some(trigger);
    env.risk_profile = Some(&risk);
    env.policy = Some(&armed);
    let _ = vike_tradehub_core::make_engine(&mut env, "binance", "BTCUSDT");
    assert_eq!(TAKES.mounts.load(Ordering::SeqCst), 1);
    assert!(TAKES.saw_trigger.load(Ordering::SeqCst), "takes_recon_trigger is true");
}
