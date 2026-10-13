//! A held arm (`HeldBelowLive::OrdersHeld`): it binds its tier and sends nothing.
use super::*;
use vike_exec::ExecutionClient;
use vike_model::events::Event;

/// What the planted held venue's client was asked to do — only what passed the hold reaches it.
static SUBMITS: AtomicUsize = AtomicUsize::new(0);
static MODIFIES: AtomicUsize = AtomicUsize::new(0);
static CANCELS: AtomicUsize = AtomicUsize::new(0);

/// The bound client of the planted held venue: it COUNTS, so a test can see an order reach it.
/// It refuses nothing itself — the forgetful bridge the mount's hold exists for.
struct CountingClient;

impl ExecutionClient for CountingClient {
    fn submit(&mut self, _request: &vike_model::OrderRequest) {
        SUBMITS.fetch_add(1, Ordering::SeqCst);
    }
    fn modify(&mut self, _order: &vike_model::OrderRequest, _q: Option<f64>, _p: Option<f64>) {
        MODIFIES.fetch_add(1, Ordering::SeqCst);
    }
    fn cancel(&mut self, _client_order_id: &str) {
        CANCELS.fetch_add(1, Ordering::SeqCst);
    }
}

/// A venue whose probe answers `Armed` at LIVE with every order held, and whose `mount` binds live
/// and hands back a client that would send whatever it is given.
struct HeldRow;

const HELD_LIVE: Resolution =
    Resolution::Armed { tier: AccountTier::Live, held_below_live: Some(HeldBelowLive::OrdersHeld) };

impl VenueMount for HeldRow {
    fn venue(&self) -> &'static str {
        "okx"
    }
    fn declaration(&self) -> VenueDeclaration {
        VenueDeclaration { grid_source: DeclaredGridSource::InHand, ..PLANTED_DECLARATION }
    }
    fn resolve(&self, _inputs: &MountInputs<'_>) -> Resolution {
        HELD_LIVE
    }
    fn mount(&self, _req: MountRequest<'_>) -> MountOutcome {
        MountOutcome {
            exec: ExecOutcome::Live(LiveExec {
                client: Box::new(CountingClient),
                bound_tier: AccountTier::Live,
                grid: Some(grid(0.5)),
                contract_size: None,
                margin_mode: None,
                leg_grids: Vec::new(),
            }),
            recon: Some(Box::new(EmptyRecon)),
            identity: None,
        }
    }
}

static HELD: HeldRow = HeldRow;
static HELD_REG: [VenueRow; 1] = [VenueRow::Mount(&HELD)];

fn order(coid: &str) -> vike_model::OrderRequest {
    vike_model::OrderRequest {
        client_order_id: coid.into(),
        venue: "okx".into(),
        symbol: "BTC-USDT-SWAP".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Market,
        ts: 7,
        ..Default::default()
    }
}

/// The fold: a held arm is a row AT the bridge's tier with `OrdersHeld` — at live and at demo —
/// and the no-downgrade rule still beats it (a held DEMO arming under a `live` tier does not run).
#[test]
fn the_fold_honours_a_held_arm_at_its_own_tier() {
    use crate::contract::resolution_to_arming as fold;
    let held = Some(HeldBelowLive::OrdersHeld);
    assert_eq!(
        fold(HELD_LIVE, AccountTier::Live),
        (Some(AccountTier::Live), ArmingBlock::OrdersHeld)
    );
    let held_demo = Resolution::Armed { tier: AccountTier::Demo, held_below_live: held };
    assert_eq!(
        fold(held_demo, AccountTier::Demo),
        (Some(AccountTier::Demo), ArmingBlock::OrdersHeld)
    );
    assert_eq!(
        fold(held_demo, AccountTier::Live),
        (None, ArmingBlock::LiveCredentialsAbsent),
        "a live account never trades demo, held or not"
    );
    let vars = HashMap::new();
    assert_eq!(
        crate::venue_arming_under(&HELD_REG, "okx", &vars, no_creds(), AccountTier::Live),
        (Some(AccountTier::Live), ArmingBlock::OrdersHeld),
        "the projection says the same"
    );
}

/// **A held live arm BINDS LIVE and SENDS NOTHING.** Mounted through the real fold, it is a live
/// engine (in the live set, at the `Live` tier, its reconcile client kept); every submit and modify
/// is answered on the event lane — a terminal `OrderRejected`, a non-terminal
/// `OrderModifyRejected` — and NONE reaches the bridge's client, which would have sent it; a cancel
/// passes, because a hold never traps an operator in an order. Without a live risk budget it is
/// refused like any live mount.
#[test]
fn a_held_live_arm_binds_live_and_sends_nothing() {
    let (tx, mut rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let armed = policy("okx", AccountTier::Live);
    let risk = budget();
    let mounted = {
        let vars = HashMap::new();
        let mut env = crate::MountEnv::new(&HELD_REG, &vars, &tx, &mut live);
        env.recon_enabled = true;
        env.risk_profile = Some(&risk);
        env.policy = Some(&armed);
        crate::make_engine(&mut env, "okx", "BTC-USDT-SWAP")
    };
    let (mut engine, recon) = mounted.expect("a budgeted held arm mounts");
    assert!(live.contains("okx"), "the held arm is recorded live");
    assert_eq!(engine.tier, AccountTier::Live, "it binds the LIVE tier");
    assert!(recon.is_some(), "its reconcile client is kept: data and reconcile run");

    let rows = crate::venue_account_arming(&HELD_REG, "okx", &HashMap::new(), Some(&armed));
    assert_eq!(
        (rows[0].tier, rows[0].effective, rows[0].block),
        (Some(AccountTier::Live), Some(AccountTier::Live), ArmingBlock::OrdersHeld),
        "the arming screen shows the hold"
    );

    let (submits, modifies, cancels) = (
        SUBMITS.load(Ordering::SeqCst),
        MODIFIES.load(Ordering::SeqCst),
        CANCELS.load(Ordering::SeqCst),
    );
    engine.client.submit(&order("held-1"));
    engine.client.submit_batch(&[order("held-2"), order("held-3")]);
    engine.client.modify(&order("held-1"), Some(2.0), None);
    engine.client.cancel("held-1");
    assert_eq!(SUBMITS.load(Ordering::SeqCst), submits, "no submit reached the venue client");
    assert_eq!(MODIFIES.load(Ordering::SeqCst), modifies, "no modify reached the venue client");
    assert_eq!(CANCELS.load(Ordering::SeqCst), cancels + 1, "a cancel passes the hold");

    let mut rejected = Vec::new();
    let mut modify_rejected = Vec::new();
    while let Ok(ingest) = rx.try_recv() {
        match ingest {
            vike_exec::Ingest::Event(Event::OrderRejected(r)) => {
                assert_eq!(r.reason.as_str(), crate::orders_held::ORDERS_HELD_REASON);
                rejected.push(r.client_order_id);
            }
            vike_exec::Ingest::Event(Event::OrderModifyRejected(r)) => {
                modify_rejected.push(r.client_order_id);
            }
            _ => {}
        }
    }
    assert_eq!(rejected, ["held-1", "held-2", "held-3"], "every order gets its terminal");
    assert_eq!(modify_rejected, ["held-1"], "the amend is refused, non-terminally");

    // …and the hold does not exempt it from the live budget rule.
    let mut live = HashSet::new();
    assert!(
        mount_one(&HELD_REG, "okx", "BTC-USDT-SWAP", &tx, &mut live, None, Some(&armed)).is_err(),
        "a held live arm still needs a live risk budget"
    );
}

/// The default account with nothing stored — the planted venues here read no credential.
fn no_creds() -> vike_secrets::account_credentials::AccountCredentials<'static> {
    vike_secrets::account_credentials::AccountCredentials::none()
}
