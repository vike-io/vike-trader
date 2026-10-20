//! **An account's own leverage reaches ITS engine's margin rate, and nothing else** (decision 0181):
//! the shared tail's fold over a scripted venue, judged on the engine it builds.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`no_stated_leverage_leaves_the_engine_at_1x`] | an upgrade re-margining every account that never asked |
//! | [`a_confirmed_leverage_is_applied_to_that_engine_only`] | the figure leaking to another account of the venue, or not applied at all |
//! | [`a_leverage_the_venue_did_not_confirm_is_never_applied`] | an unconfirmed, over-the-venue-maximum or non-confirmable-venue figure reaching the gate |
//! | [`the_existing_leverage_ceilings_still_cap_it`] | the account's figure loosening `policy.max_leverage` or a profile's `[risk] max_leverage` |
//! | [`the_exposure_ceilings_still_bind_beside_it`] | the leverage moving `max_account_exposure` |
//! | [`a_later_start_reads_the_row_afresh`] | a hot start (`account.active` off and on) keeping the first row it saw |
//! | [`a_started_engine_judges_an_order_on_its_seed_until_the_first_balance_sync`] | the 2026-10-18 okx drill misread: an applied 4x denying an order because the engine's equity was still `seed_cash` |
//! | [`a_live_twin_whose_leverage_the_venue_holds_starts_at_it`] | the mainnet lane twin (binance `PERP` at `live`) not applying the owner's figure, or running on the wrong tier / route key |
//! | [`a_live_twin_the_venue_did_not_confirm_does_not_start_at_all`] | a mainnet twin trading at 1x beside a real account left on a leverage nobody chose (the demo rule), or a refused twin lighting the LIVE record |
//! | [`a_refused_live_twin_ends_its_lease_and_the_backend_forgets_the_start`] | a refused mainnet twin keeping its gatewayhub session, user-data stream and live-account lock up while the account reads NOT RUNNING |
//! | [`a_live_twin_with_nothing_to_ask_is_refused_before_the_venue_is_called`] | the bridge's start writing its default to the real account for a twin that has no figure to hand over |
//! | [`a_live_row_that_states_no_leverage_has_no_twin_and_asks_nothing`] | the opening of `live` changing a live account that never stated a leverage |
//! | [`a_demo_twin_keeps_the_demo_rule`] | the strict live rule leaking to the demo twin, or to any account that is not a derived twin |
//! | [`a_live_twin_never_gets_a_looser_ceiling_than_its_parent`] | the twin's exposure, notional, margin or kill-switch gates being looser than the parent's on mainnet |
//! | [`a_live_twin_is_refused_without_a_risk_budget_like_its_parent_and_not_when_inactive`] | a second real-money engine starting without the `[risk]` budget or the `account.active` switch |
//! | [`only_a_derived_live_twin_must_hold_its_leverage`] | the strict predicate matching a real `PERP` account, a demo twin, bybit or okx at live, or an unread directory |

use std::collections::{BTreeMap, HashSet};

use vike_config::{ArmingBlock, VenueArming};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use crate::symbol_grid::GridSource;
use crate::testutil::NoopClient;
use crate::{
    AdapterBind, AdapterEnv, AdapterPolicy, ArmingRow, ArmingTable, EngineError, ScriptedBackend,
    ScriptedCall, VenueFacts, make_engine_for_account,
};

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

fn route_key(venue: &str, label: &AccountLabel) -> String {
    crate::account_route_key(venue, label)
}

/// A table that arms the default and the `ALT` account of `venue` at `demo`.
fn table_of(venue: &'static str) -> ArmingTable {
    let facts = BTreeMap::from([(
        venue.to_string(),
        VenueFacts {
            adapter_present: true,
            addresses_accounts: true,
            takes_recon_trigger: false,
            grid_source: GridSource::NoGrid,
            process_exclusive: None,
            clock_decl_word: "not-wired",
        },
    )]);
    let row = |label: AccountLabel, id: i64| ArmingRow {
        arming: VenueArming {
            venue,
            label,
            tier: Some(AccountTier::Demo),
            effective: Some(AccountTier::Demo),
            block: ArmingBlock::None,
            account_ids: vec![id],
        },
        book: None,
    };
    ArmingTable::new(facts, vec![row(AccountLabel::Default, 1), row(alt(), 2)])
}

/// An ACTIVE `demo` row of `venue` stating `leverage` and `max_exposure`.
fn row(
    id: i64,
    venue: &str,
    label: &AccountLabel,
    leverage: Option<f64>,
    max_exposure: Option<f64>,
) -> vike_secrets::Account {
    vike_secrets::Account {
        id,
        venue: venue.to_string(),
        tier: AccountTier::Demo,
        label: label.text().map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: max_exposure.and_then(vike_secrets::MaxExposure::new),
        leverage: leverage.and_then(vike_secrets::AccountLeverage::new),
    }
}

fn bind(cap: Option<Result<f64, String>>) -> AdapterBind {
    AdapterBind {
        client: Box::new(NoopClient),
        recon: None,
        bound_tier: AccountTier::Demo,
        grid: None,
        contract_value: None,
        margin_mode: None,
        leg_grids: Vec::new(),
        confirmed_book: None,
        leverage_cap: cap,
        account_ccy: None,
        account_ccy_assumed: false,
    }
}

fn budget() -> vike_model::ProfileRisk {
    vike_model::ProfileRisk {
        max_notional_per_order: Some(1_000_000.0),
        max_total_exposure: Some(1_000_000.0),
        ..Default::default()
    }
}

/// The margin rate and the account exposure ceiling of the engine a start of `label` builds.
fn start(
    venue: &'static str,
    label: &AccountLabel,
    policy: &AdapterPolicy,
    cap: Option<Result<f64, String>>,
    risk: &vike_model::ProfileRisk,
) -> (Option<f64>, Option<f64>) {
    let table = table_of(venue);
    let backend =
        ScriptedBackend::new(table.clone()).script_start(&route_key(venue, label), Ok(bind(cap)));
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let mut env = AdapterEnv::new(&backend, &table, &tx, &mut live);
    env.risk_profile = Some(risk);
    env.policy = Some(policy);
    match make_engine_for_account(&mut env, venue, "BTC", label, &[]) {
        Ok((engine, _)) => {
            (engine.gate.limits.im_requirement, engine.gate.limits.max_account_exposure)
        }
        Err(EngineError::NotRunning(stopped)) => panic!("the account did not run: {stopped:?}"),
        Err(other) => panic!("the start was refused: {other}"),
    }
}

fn policy_with(venue: &str, rows: &[vike_secrets::Account]) -> AdapterPolicy {
    let mut policy = AdapterPolicy::default();
    for r in rows {
        assert_eq!(r.venue, venue);
        policy = policy.with_account_row(r.clone());
    }
    policy
}

#[test]
fn no_stated_leverage_leaves_the_engine_at_1x() {
    let policy =
        policy_with("hyperliquid", &[row(1, "hyperliquid", &AccountLabel::Default, None, None)]);
    for cap in [None, Some(Ok(20.0)), Some(Err("down".to_string()))] {
        let (im, _) = start("hyperliquid", &AccountLabel::Default, &policy, cap, &budget());
        assert_eq!(im, Some(1.0), "nothing stated: today's 1x, whatever the venue said");
    }
}

#[test]
fn a_confirmed_leverage_is_applied_to_that_engine_only() {
    let policy = policy_with(
        "hyperliquid",
        &[
            row(1, "hyperliquid", &AccountLabel::Default, Some(10.0), None),
            row(2, "hyperliquid", &alt(), None, None),
        ],
    );
    let (im, _) = start("hyperliquid", &AccountLabel::Default, &policy, Some(Ok(20.0)), &budget());
    assert_eq!(im, Some(0.1), "10x: the engine margins an order at a tenth of its notional");
    let (im, _) = start("hyperliquid", &alt(), &policy, Some(Ok(20.0)), &budget());
    assert_eq!(im, Some(1.0), "another account of the same venue is untouched");
    // A figure equal to the venue's maximum is allowed.
    let policy = policy_with(
        "hyperliquid",
        &[row(1, "hyperliquid", &AccountLabel::Default, Some(20.0), None)],
    );
    let (im, _) = start("hyperliquid", &AccountLabel::Default, &policy, Some(Ok(20.0)), &budget());
    assert_eq!(im, Some(0.05));
}

#[test]
fn a_leverage_the_venue_did_not_confirm_is_never_applied() {
    let policy = policy_with(
        "hyperliquid",
        &[row(1, "hyperliquid", &AccountLabel::Default, Some(10.0), None)],
    );
    let cases = [
        ("above the venue maximum", Some(Ok(5.0))),
        ("the venue refused", Some(Err("no".to_string()))),
        ("no confirmation came", None),
        ("an unusable confirmation", Some(Ok(f64::NAN))),
        ("a sub-1 confirmation", Some(Ok(0.5))),
    ];
    for (what, cap) in cases {
        let (im, _) = start("hyperliquid", &AccountLabel::Default, &policy, cap, &budget());
        assert_eq!(im, Some(1.0), "{what}: refused, the engine stays at 1x");
    }
    // A venue with no leverage number (deribit) ignores the figure even beside a confirmation.
    let policy =
        policy_with("deribit", &[row(1, "deribit", &AccountLabel::Default, Some(10.0), None)]);
    let (im, _) = start("deribit", &AccountLabel::Default, &policy, Some(Ok(50.0)), &budget());
    assert_eq!(im, Some(1.0), "deribit has no leverage number to confirm");
}

#[test]
fn the_existing_leverage_ceilings_still_cap_it() {
    let policy = policy_with(
        "hyperliquid",
        &[row(1, "hyperliquid", &AccountLabel::Default, Some(10.0), None)],
    );
    let mut ceilinged = policy.clone();
    ceilinged.max_leverage = Some(5.0);
    let (im, _) =
        start("hyperliquid", &AccountLabel::Default, &ceilinged, Some(Ok(20.0)), &budget());
    assert_eq!(im, Some(0.2), "policy.max_leverage caps the account's figure");
    let profile = vike_model::ProfileRisk { max_leverage: Some(4.0), ..budget() };
    let (im, _) = start("hyperliquid", &AccountLabel::Default, &policy, Some(Ok(20.0)), &profile);
    assert_eq!(im, Some(0.25), "a run profile's [risk] max_leverage caps it too");
    let profile = vike_model::ProfileRisk { max_leverage: Some(50.0), ..budget() };
    let (im, _) = start("hyperliquid", &AccountLabel::Default, &policy, Some(Ok(20.0)), &profile);
    assert_eq!(im, Some(0.1), "a looser profile figure does not raise the account's");
}

#[test]
fn the_exposure_ceilings_still_bind_beside_it() {
    let policy = policy_with(
        "hyperliquid",
        &[row(1, "hyperliquid", &AccountLabel::Default, Some(10.0), Some(100.0))],
    );
    let (im, exposure) =
        start("hyperliquid", &AccountLabel::Default, &policy, Some(Ok(20.0)), &budget());
    assert_eq!(im, Some(0.1));
    assert_eq!(exposure, Some(100.0), "the account's own 100 USD ceiling is exactly as it was");
    let mut boxed = policy.clone();
    boxed.max_account_exposure = Some(50.0);
    let (_, exposure) =
        start("hyperliquid", &AccountLabel::Default, &boxed, Some(Ok(20.0)), &budget());
    assert_eq!(exposure, Some(50.0), "the tighter box-wide ceiling still wins");
}

#[test]
fn a_later_start_reads_the_row_afresh() {
    let before =
        policy_with("hyperliquid", &[row(1, "hyperliquid", &AccountLabel::Default, None, None)]);
    let after = policy_with(
        "hyperliquid",
        &[row(1, "hyperliquid", &AccountLabel::Default, Some(10.0), None)],
    );
    let (first, _) =
        start("hyperliquid", &AccountLabel::Default, &before, Some(Ok(20.0)), &budget());
    let (second, _) =
        start("hyperliquid", &AccountLabel::Default, &after, Some(Ok(20.0)), &budget());
    assert_eq!((first, second), (Some(1.0), Some(0.1)));
}

// ---- venues that POST a leverage (decision 0182) -------------------------------------------------

/// [`table_of`] at `tier`.
fn table_at(venue: &'static str, tier: AccountTier) -> ArmingTable {
    let facts = BTreeMap::from([(
        venue.to_string(),
        VenueFacts {
            adapter_present: true,
            addresses_accounts: true,
            takes_recon_trigger: false,
            grid_source: GridSource::NoGrid,
            process_exclusive: None,
            clock_decl_word: "not-wired",
        },
    )]);
    let row = |label: AccountLabel, id: i64| ArmingRow {
        arming: VenueArming {
            venue,
            label,
            tier: Some(tier),
            effective: Some(tier),
            block: ArmingBlock::None,
            account_ids: vec![id],
        },
        book: None,
    };
    ArmingTable::new(facts, vec![row(AccountLabel::Default, 1), row(alt(), 2)])
}

/// An ACTIVE row of `venue` at `tier` stating `leverage`.
fn row_at(
    tier: AccountTier,
    id: i64,
    venue: &str,
    label: &AccountLabel,
    leverage: Option<f64>,
) -> vike_secrets::Account {
    vike_secrets::Account { tier, ..row(id, venue, label, leverage, None) }
}

/// What a start of `label` at `tier` leaves: the engine's margin rate, and the whole-number figure
/// the START ASKED THE VENUE TO HOLD (what the node put on the wire; `None` = no ask).
fn start_asking(
    venue: &'static str,
    tier: AccountTier,
    label: &AccountLabel,
    policy: &AdapterPolicy,
    cap: Option<Result<f64, String>>,
    risk: &vike_model::ProfileRisk,
) -> (Option<f64>, Option<f64>) {
    let table = table_at(venue, tier);
    let bind = AdapterBind { bound_tier: tier, ..bind(cap) };
    let backend =
        ScriptedBackend::new(table.clone()).script_start(&route_key(venue, label), Ok(bind));
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let mut env = AdapterEnv::new(&backend, &table, &tx, &mut live);
    env.risk_profile = Some(risk);
    env.policy = Some(policy);
    let im = match make_engine_for_account(&mut env, venue, "BTC", label, &[]) {
        Ok((engine, _)) => engine.gate.limits.im_requirement,
        Err(EngineError::NotRunning(stopped)) => panic!("the account did not run: {stopped:?}"),
        Err(other) => panic!("the start was refused: {other}"),
    };
    let asked = backend.calls().iter().find_map(|call| match call {
        ScriptedCall::Start { account_leverage, .. } => Some(*account_leverage),
        _ => None,
    });
    (im, asked.expect("the start reached the backend"))
}

const POSTING: [&str; 3] = ["bybit", "okx", "binance"];

#[test]
fn a_posting_venue_is_asked_for_the_whole_number_and_applied_only_when_it_holds_it() {
    for venue in POSTING {
        let policy = policy_with(venue, &[row(1, venue, &AccountLabel::Default, Some(10.0), None)]);
        let demo = AccountTier::Demo;
        let (im, asked) =
            start_asking(venue, demo, &AccountLabel::Default, &policy, Some(Ok(10.0)), &budget());
        assert_eq!(asked, Some(10.0), "{venue}: the figure is on the wire");
        assert_eq!(
            im,
            Some(0.1),
            "{venue}: the venue holds exactly 10, so the engine margins at 10x"
        );
        // The same ask, answered with anything else, stays at 1x (and the ask is still made).
        let cases = [
            ("held below", Some(Ok(5.0))),
            ("held above", Some(Ok(20.0))),
            ("the lane refused", Some(Err("isolated margin".to_string()))),
            ("an older gatewayhub", None),
            ("an unusable answer", Some(Ok(f64::NAN))),
        ];
        for (what, cap) in cases {
            let (im, asked) =
                start_asking(venue, demo, &AccountLabel::Default, &policy, cap, &budget());
            assert_eq!(asked, Some(10.0), "{venue}: {what}");
            assert_eq!(im, Some(1.0), "{venue}: {what}: refused, the engine stays at 1x");
        }
    }
}

#[test]
fn the_ask_is_the_floor_of_the_lowest_of_the_stated_figure_and_both_ceilings() {
    let venue = "bybit";
    let policy = policy_with(venue, &[row(1, venue, &AccountLabel::Default, Some(10.0), None)]);
    let demo = AccountTier::Demo;
    let run = |policy: &AdapterPolicy, risk: &vike_model::ProfileRisk, held: f64| {
        start_asking(venue, demo, &AccountLabel::Default, policy, Some(Ok(held)), risk)
    };
    // policy.max_leverage 2.5 with stated 10: the ask is 2, and the venue holding 2 is applied at 2x.
    let mut ceilinged = policy.clone();
    ceilinged.max_leverage = Some(2.5);
    assert_eq!(run(&ceilinged, &budget(), 2.0), (Some(0.5), Some(2.0)));
    // …and a venue still holding the un-floored 10 (a stale answer) is NOT trusted.
    assert_eq!(run(&ceilinged, &budget(), 10.0), (Some(1.0), Some(2.0)));
    // A run profile's own figure lowers it too; a looser one does not raise it.
    let profile = vike_model::ProfileRisk { max_leverage: Some(4.0), ..budget() };
    assert_eq!(run(&policy, &profile, 4.0), (Some(0.25), Some(4.0)));
    let profile = vike_model::ProfileRisk { max_leverage: Some(50.0), ..budget() };
    assert_eq!(run(&policy, &profile, 10.0), (Some(0.1), Some(10.0)));
}

#[test]
fn an_account_that_states_nothing_or_sits_on_a_closed_row_sends_nothing() {
    let default = AccountLabel::Default;
    // Nothing stated: no ask, whatever the venue says.
    for venue in POSTING {
        let policy = policy_with(venue, &[row(1, venue, &default, None, None)]);
        let (im, asked) =
            start_asking(venue, AccountTier::Demo, &default, &policy, Some(Ok(10.0)), &budget());
        assert_eq!((im, asked), (Some(1.0), None), "{venue}: nothing stated");
    }
    // A LIVE row of a demo-only venue is ignored by the engine: no ask, and 1x beside a "confirmation".
    // (binance is no longer one: the owner opened its `live` tier, 2026-10-19 — see the lane-twin tests.)
    for venue in ["bybit", "okx"] {
        let policy =
            policy_with(venue, &[row_at(AccountTier::Live, 1, venue, &default, Some(10.0))]);
        let (im, asked) =
            start_asking(venue, AccountTier::Live, &default, &policy, Some(Ok(10.0)), &budget());
        assert_eq!((im, asked), (Some(1.0), None), "{venue}: a live row is closed");
    }
    // aster is coded and closed; deribit has no leverage number: neither is asked, neither applied.
    for venue in ["aster", "deribit"] {
        let policy = policy_with(venue, &[row(1, venue, &default, Some(10.0), None)]);
        let (im, asked) =
            start_asking(venue, AccountTier::Demo, &default, &policy, Some(Ok(10.0)), &budget());
        assert_eq!((im, asked), (Some(1.0), None), "{venue}: closed");
    }
    // A fraction planted on a whole-number venue is ignored, not floored behind the operator's back.
    let policy = policy_with("binance", &[row(1, "binance", &default, Some(2.5), None)]);
    let (im, asked) =
        start_asking("binance", AccountTier::Demo, &default, &policy, Some(Ok(2.0)), &budget());
    assert_eq!((im, asked), (Some(1.0), None), "a planted fraction");
    // Hyperliquid is exactly as shipped: no ask (the venue holds the leverage), the cap rule unchanged.
    let policy = policy_with("hyperliquid", &[row(1, "hyperliquid", &default, Some(10.0), None)]);
    let (im, asked) = start_asking(
        "hyperliquid",
        AccountTier::Demo,
        &default,
        &policy,
        Some(Ok(20.0)),
        &budget(),
    );
    assert_eq!(
        (im, asked),
        (Some(0.1), None),
        "hyperliquid sends no ask and applies against its cap"
    );
}

#[test]
fn another_account_of_a_posting_venue_is_not_asked_and_not_applied() {
    let venue = "okx";
    let policy = policy_with(
        venue,
        &[
            row(1, venue, &AccountLabel::Default, Some(10.0), None),
            row(2, venue, &alt(), None, None),
        ],
    );
    let (im, asked) =
        start_asking(venue, AccountTier::Demo, &alt(), &policy, Some(Ok(10.0)), &budget());
    assert_eq!((im, asked), (Some(1.0), None), "the sibling states nothing: today's start");
}

// ---- what judges an order right after a hot start (the okx drill of 2026-10-18) -------------------

type Engine = vike_exec::ExecutionEngine<Box<dyn vike_exec::ExecutionClient + Send>>;

/// The okx perpetual the drill traded. The adapter hands the engine BASE units (`contract_value`
/// `None`: 1.0), so 0.1 BTC at 72872.9 is a notional of 7287.29.
const OKX_PERP: &str = "BTC-USDT-SWAP";

/// The engine a start of the default okx account builds at `demo`, stating `stated` leverage and
/// confirmed by a venue that holds exactly that number.
fn okx_engine(stated: Option<f64>) -> Engine {
    let venue = "okx";
    let default = AccountLabel::Default;
    let policy = policy_with(venue, &[row(1, venue, &default, stated, None)]);
    let table = table_at(venue, AccountTier::Demo);
    let bind = AdapterBind { bound_tier: AccountTier::Demo, ..bind(stated.map(Ok)) };
    let backend =
        ScriptedBackend::new(table.clone()).script_start(&route_key(venue, &default), Ok(bind));
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let mut env = AdapterEnv::new(&backend, &table, &tx, &mut live);
    let risk = budget();
    env.risk_profile = Some(&risk);
    env.policy = Some(&policy);
    match make_engine_for_account(&mut env, venue, OKX_PERP, &default, &[]) {
        Ok((engine, _)) => engine,
        Err(EngineError::NotRunning(stopped)) => panic!("the account did not run: {stopped:?}"),
        Err(other) => panic!("the start was refused: {other}"),
    }
}

/// The gate's reason when `engine` refuses a BUY of `qty` BTC at 72872.9, or `None` when it lets it by.
fn refused(engine: &mut Engine, coid: &str, qty: f64) -> Option<String> {
    let request = vike_model::OrderRequest {
        client_order_id: coid.into(),
        venue: "okx".into(),
        symbol: OKX_PERP.into(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(72_872.9),
        ..Default::default()
    };
    let mut outbox = vike_exec::Outbox::default();
    engine.submit_order(&request, 1_000, &mut outbox);
    outbox.0.iter().find_map(|event| match event {
        vike_model::events::Event::OrderDenied(denied) => Some(denied.reason.to_string()),
        _ => None,
    })
}

/// **The drill's order was margined at the applied 4x; what denied it was the equity.** A hot start
/// (`account.active` off and on) appends a NEW engine in `Delta` mode: its equity is `seed_cash` (1000
/// by default) until the reconcile driver's first pass lands the venue's balance, which had not
/// happened 17 s and 21 s after two hot starts of that evening; the drill's order came 3 s after one.
/// The same order passes once the balance is in, and 1x would have denied it then too.
#[test]
fn a_started_engine_judges_an_order_on_its_seed_until_the_first_balance_sync() {
    let mut engine = okx_engine(Some(4.0));
    assert_eq!(engine.gate.limits.im_for(OKX_PERP), Some(0.25), "the confirmed 4x is in force");
    // What the core imposes on an engine it appends (`add_engine_hot`): `seed_cash`, and no balance yet.
    engine.equity_seed = 1_000.0;
    assert_eq!(engine.account.balance_mode, vike_exec::BalanceMode::Delta);

    // 0.1 BTC needs 7287.29 / 4 = 1821.82 of margin: the seed's 1000 does not fund it...
    assert_eq!(refused(&mut engine, "need-1", 0.1).as_deref(), Some("insufficient-margin"));
    // ...while an order the seed does fund is margined at that same 4x (0.04 BTC: 728.73 < 1000, and
    // 2914.92 at 1x would not be).
    assert_eq!(refused(&mut engine, "small", 0.04), None);

    // The first pass lands the venue's own figure (4999.6 USDT on the drill's account).
    engine.account.balance = 4_999.625;
    engine.account.balance_mode = vike_exec::BalanceMode::Authoritative;
    assert_eq!(refused(&mut engine, "need-2", 0.1), None, "the same order passes at 4x");
    // 0.3 BTC needs 21861.87 / 4 = 5465.47 > 4999.6: the second order of the drill, rightly refused.
    assert_eq!(refused(&mut engine, "big", 0.3).as_deref(), Some("insufficient-margin"));

    // Without the stated leverage the engine margins at 1x and refuses the 0.1 BTC order on the same
    // balance: the figure is what lets it by.
    let mut plain = okx_engine(None);
    plain.account.balance = 4_999.625;
    plain.account.balance_mode = vike_exec::BalanceMode::Authoritative;
    assert_eq!(refused(&mut plain, "need-3", 0.1).as_deref(), Some("insufficient-margin"));
}

// ---- the account's currency (decision 0183, item 1) ----------------------------------------------

/// The ledger label (`quote_asset`) of the engine a start of `venue`'s default account at `tier` builds when
/// the venue states `ccy` as the account's currency and `ceiling` is the box-wide account ceiling.
fn ledger_label(
    venue: &'static str,
    tier: AccountTier,
    ccy: Option<&str>,
    ceiling: Option<f64>,
) -> String {
    labelled_under(venue, tier, ccy, ceiling, &[venue], MOUNT)
}

/// The symbol the default oanda and ctrader accounts mount (`vike_venues::venue_wiring`), quoted in USD.
const MOUNT: &str = "EURUSD";

/// [`ledger_label`] when the root named `converting` as the venues that may hold a symbol quoted in another
/// currency, and the engine is mounted on `mount`.
fn labelled_under(
    venue: &'static str,
    tier: AccountTier,
    ccy: Option<&str>,
    ceiling: Option<f64>,
    converting: &[&str],
    mount: &str,
) -> String {
    built_under(venue, tier, ccy, ceiling, converting, mount).0.quote_asset.clone()
}

/// The engine [`labelled_under`] reads the ledger label of.
fn built_under(
    venue: &'static str,
    tier: AccountTier,
    ccy: Option<&str>,
    ceiling: Option<f64>,
    converting: &[&str],
    mount: &str,
) -> crate::EngineAndRecon {
    built_as(&AccountLabel::Default, venue, tier, ccy, ceiling, converting, mount)
}

/// [`built_under`] for the account `label` (its row is `ALT`'s, id 2, when it is not the default): the engine
/// is mounted on `mount`, which is what a labelled account's OWN row names, not the wired symbol.
fn built_as(
    label: &AccountLabel,
    venue: &'static str,
    tier: AccountTier,
    ccy: Option<&str>,
    ceiling: Option<f64>,
    converting: &[&str],
    mount: &str,
) -> crate::EngineAndRecon {
    built_stated(label, venue, tier, (ccy, false), ceiling, converting, mount)
}

/// [`built_under`] for a start whose backend says whether the currency it states is only ASSUMED: the default
/// account, the engine mounted on [`MOUNT`] (the wired symbol of fxcm, as of oanda and ctrader).
fn built_with(
    venue: &'static str,
    tier: AccountTier,
    ccy: Option<&str>,
    assumed: bool,
    ceiling: Option<f64>,
    converting: &[&str],
) -> crate::EngineAndRecon {
    built_stated(&AccountLabel::Default, venue, tier, (ccy, assumed), ceiling, converting, MOUNT)
}

/// The engine of the account `label` mounted on `mount`, whose backend states `stated` = (the currency, whether
/// it is only ASSUMED).
fn built_stated(
    label: &AccountLabel,
    venue: &'static str,
    tier: AccountTier,
    (ccy, assumed): (Option<&str>, bool),
    ceiling: Option<f64>,
    converting: &[&str],
    mount: &str,
) -> crate::EngineAndRecon {
    let table = table_at(venue, tier);
    let bind = AdapterBind {
        bound_tier: tier,
        account_ccy: ccy.map(str::to_string),
        account_ccy_assumed: assumed,
        ..bind(None)
    };
    let backend =
        ScriptedBackend::new(table.clone()).script_start(&route_key(venue, label), Ok(bind));
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let mut env = AdapterEnv::new(&backend, &table, &tx, &mut live);
    let id = if label.text().is_some() { 2 } else { 1 };
    let mut policy = policy_with(venue, &[row_at(tier, id, venue, label, None)]);
    policy.max_account_exposure = ceiling;
    policy.quote_converting_venues = converting.iter().map(|v| (*v).to_string()).collect();
    let risk = budget();
    env.risk_profile = Some(&risk);
    env.policy = Some(&policy);
    match make_engine_for_account(&mut env, venue, mount, label, &[]) {
        Ok(built) => built,
        Err(other) => panic!("the start did not build an engine: {other}"),
    }
}

/// **The anchor is a STATEMENT to the engine, not only a label**: even a venue that states `USDT` (the
/// constructor's own guess) leaves an engine that was TOLD its currency, so a foreign-quoted symbol is not
/// refused `account-ccy-unset`; an account that does not convert is never told.
///
/// A mount quoted in USD (`EURUSD`, ig's `CS.D.EURUSD.MINI.IP`) is told the venue's `USD`; a `USDT` statement
/// is not the mount's money, so the mount rule (`unanchorable_mount`) leaves such an engine untold.
#[test]
fn the_anchored_engine_was_told_its_currency() {
    const EPIC: &str = "CS.D.EURUSD.MINI.IP";
    let told = |venue, mount, ccy, ceiling| {
        built_under(venue, AccountTier::Demo, ccy, ceiling, &[venue], mount).0.account_ccy_known()
    };
    assert!(told("ig", EPIC, Some("USD"), Some(25_000.0)));
    assert!(!told("ig", EPIC, Some("USDT"), Some(25_000.0)), "USDT is not the mount's USD");
    assert!(told("oanda", MOUNT, Some("usd"), Some(25_000.0)));
    assert!(
        !told("ig", EPIC, Some("USDT"), None),
        "no ceiling: the account does not convert, nothing is told"
    );
    assert!(!told("oanda", MOUNT, None, Some(25_000.0)), "the venue stated none");
}

/// **The engine a start builds TELLS its tier**, so the journal's snapshot names it and a boot restore can
/// refuse another tier's book (`vike_core`'s `apply_restored_engines`): at `demo` and at `live`, whatever
/// the tier is, and a bare `ExecutionEngine::new` (a seed, not a statement) says nothing.
#[test]
fn a_built_engine_tells_the_journal_its_tier() {
    for tier in [AccountTier::Demo, AccountTier::Live] {
        let (engine, _) = built_under("oanda", tier, None, None, &[], MOUNT);
        assert_eq!(engine.tier, tier);
        assert_eq!(engine.snapshot_state().tier, Some(tier), "{tier:?}");
    }
    // A `paper` account's engine is a different assembly (`paper_engine`) and says `paper`: the case that
    // lets a boot restore refuse its book when the account's `demo` successor takes the route key.
    let (paper, _) = crate::paper_fallback::paper_engine(
        "oanda",
        "EURUSD",
        &AccountLabel::Default,
        &[],
        None,
        None,
    );
    assert_eq!(paper.snapshot_state().tier, Some(AccountTier::Paper));
    assert_eq!(
        vike_exec::ExecutionEngine::new(
            vike_exec::Account::new(1.0, "oanda", None, vike_exec::BalanceMode::Delta),
            vike_exec::RiskGate::new(vike_model::RiskLimits::new()),
            Box::new(crate::testutil::NoopClient) as Box<dyn vike_exec::ExecutionClient + Send>,
            "oanda",
            "EURUSD",
        )
        .snapshot_state()
        .tier,
        None,
        "a seed is not a statement"
    );
}

/// **The ledger is anchored to the venue's currency for an account that converts, and for no other**: OANDA's
/// practice account with an account-wide ceiling takes the currency the venue states (upper-cased); every
/// other start, whatever it was told, keeps the `"USDT"` label it has always had, because relabelling an
/// engine whose journal says `"USDT"` skips that journal's restore once (`apply_restored_engines`).
#[test]
fn the_ledger_currency_is_anchored_only_for_an_account_that_converts() {
    let default_label = "USDT";
    assert_eq!(ledger_label("oanda", AccountTier::Demo, Some("usd"), Some(25_000.0)), "USD");
    // Was `"EUR"`, which PINNED the defect: the mount (`EURUSD`) is quoted in USD, so a ledger labelled EUR
    // booked it at par in another money. The account that really is EUR converts on a EUR-quoted mount only
    // (`an_account_is_anchored_only_to_the_money_its_mount_is_quoted_in`).
    assert_eq!(
        ledger_label("oanda", AccountTier::Demo, Some("EUR"), Some(25_000.0)),
        default_label,
        "the mount is quoted in USD, not in the stated EUR: the ledger is not relabelled"
    );
    assert_eq!(
        ledger_label("oanda", AccountTier::Demo, None, Some(25_000.0)),
        default_label,
        "the venue stated none: the label stays"
    );
    assert_eq!(
        ledger_label("oanda", AccountTier::Demo, Some(" "), Some(25_000.0)),
        default_label,
        "a blank currency is none"
    );
    assert_eq!(
        ledger_label("oanda", AccountTier::Demo, Some("USD"), None),
        default_label,
        "no account-wide ceiling: the account does not convert, so nothing is relabelled"
    );
    assert_eq!(
        ledger_label("oanda", AccountTier::Live, Some("USD"), Some(25_000.0)),
        default_label,
        "a live account is never relabelled"
    );
    assert_eq!(
        labelled_under("bybit", AccountTier::Demo, Some("USD"), Some(25_000.0), &["oanda"], MOUNT),
        default_label,
        "a venue the root did not name has no rate source"
    );
    assert_eq!(
        labelled_under("oanda", AccountTier::Demo, Some("USD"), Some(25_000.0), &[], MOUNT),
        default_label,
        "a root that names no converting venue (every other caller) relabels nothing"
    );
}

/// What a start of `venue`'s default account (the venue states `ccy`, a ceiling is armed, the root names the
/// venue as converting) mounted on `mount` leaves: its ledger label, whether the engine was TOLD its currency,
/// and the engine's verdict on `symbol`, quoted in `quote_ccy` at `rate` account units per unit of it.
fn mounted_on(
    venue: &'static str,
    ccy: &str,
    mount: &str,
    (symbol, quote_ccy, rate): (&str, &str, f64),
) -> (String, bool, vike_exec::AdmitOutcome) {
    let tier = AccountTier::Demo;
    let (mut engine, _) = built_under(venue, tier, Some(ccy), Some(25_000.0), &[venue], mount);
    engine.now_ms = 1_000;
    let admission = foreign_admission(venue, symbol, quote_ccy, rate, 1_000);
    let verdict = engine.admit_symbol(&admission);
    (engine.quote_asset.clone(), engine.account_ccy_known(), verdict)
}

/// The admission of `symbol`, quoted in `quote_ccy`, that a node builds from a facts statement of `rate` (account
/// units per unit of `quote_ccy`) observed at `as_of_ms`.
fn foreign_admission(
    venue: &str,
    symbol: &str,
    quote_ccy: &str,
    rate: f64,
    as_of_ms: i64,
) -> vike_exec::SymbolAdmission {
    vike_exec::SymbolAdmission {
        route_key: route_key(venue, &AccountLabel::Default),
        symbol: symbol.to_string(),
        grid: vike_model::SymbolGrid {
            tick_size: Some(0.00001),
            lot_size: Some(1.0),
            ..vike_model::SymbolGrid::default()
        },
        contract_value: None,
        initial_mark: None,
        quote_ccy: Some(quote_ccy.to_string()),
        quote_rate: Some(rate),
        rate_as_of_ms: Some(as_of_ms),
        im_requirement: None,
    }
}

/// **The ledger is anchored to the account's currency only when the engine's own MOUNTED symbol is quoted in
/// it** (decision 0183, item 1). The engine books its mounted symbol at par, so an account that states EUR on
/// a mount quoted in USD (`EURUSD`) would count dollars as euros, wrong by the pair's rate on every number the
/// mount produces, while every OTHER symbol was converted into that EUR ledger. Such an engine is left
/// untold: its label stays and a foreign-quoted symbol is refused `account-ccy-unset`. Both directions are
/// pinned: the mismatch is fenced, and a mount in the account's own money converts as before.
#[test]
fn an_account_is_anchored_only_to_the_money_its_mount_is_quoted_in() {
    use vike_exec::AdmitOutcome::{Admitted, Refused};
    // Each row: venue, the currency the venue states, the mount, a symbol to admit (name, its quote
    // currency, account units per unit of it), the ledger label and the engine's verdict.
    type Row = (
        &'static str,
        &'static str,
        &'static str,
        (&'static str, &'static str, f64),
        &'static str,
        vike_exec::AdmitOutcome,
    );
    let rows: &[Row] = &[
        // THE DEFECT, fenced: a EUR account on a USD-quoted mount is not a EUR ledger.
        ("oanda", "EUR", "EURUSD", ("EURGBP", "GBP", 1.17), "USDT", Refused("account-ccy-unset")),
        ("oanda", "GBP", "EURUSD", ("USDJPY", "JPY", 0.0049), "USDT", Refused("account-ccy-unset")),
        // A USD mount in a yen-quoted pair: the mount is booked in yen, the account is dollars.
        ("oanda", "USD", "USDJPY", ("EURGBP", "GBP", 1.35), "USDT", Refused("account-ccy-unset")),
        // UNCHANGED: a USD account on a USD-quoted mount, and each account on a mount in its own money.
        ("oanda", "USD", "EURUSD", ("EURGBP", "GBP", 1.35), "USD", Admitted),
        ("oanda", "GBP", "EURGBP", ("EURUSD", "USD", 0.74), "GBP", Admitted),
        ("oanda", "EUR", "GBPEUR", ("GBPUSD", "USD", 0.92), "EUR", Admitted),
        ("oanda", "JPY", "USDJPY", ("EURUSD", "USD", 150.0), "JPY", Admitted),
        // The spelling does not matter: OANDA's own `BASE_QUOTE`, any case, padded.
        ("oanda", "USD", "EUR_USD", ("EURGBP", "GBP", 1.35), "USD", Admitted),
        ("oanda", "USD", "eurusd", ("EURGBP", "GBP", 1.35), "USD", Admitted),
        ("oanda", "USD", "SPX500_USD", ("EURGBP", "GBP", 1.35), "USD", Admitted),
        ("oanda", "EUR", "EUR_USD", ("EURGBP", "GBP", 1.17), "USDT", Refused("account-ccy-unset")),
        // A name that says no money is not guessed at: the ledger is not anchored.
        ("oanda", "USD", "US500", ("EURGBP", "GBP", 1.35), "USDT", Refused("account-ccy-unset")),
        ("oanda", "USD", "EUR/USD", ("EURGBP", "GBP", 1.35), "USDT", Refused("account-ccy-unset")),
        // Another venue whose mounts are pairs, once it states a currency (ctrader's deposit asset).
        ("ctrader", "EUR", "EURUSD", ("EURGBP", "GBP", 1.17), "USDT", Refused("account-ccy-unset")),
        ("ctrader", "USD", "EURUSD", ("EURGBP", "GBP", 1.35), "USD", Admitted),
        ("ctrader", "EUR", "GBPEUR", ("GBPUSD", "USD", 0.92), "EUR", Admitted),
        ("fxcm", "USD", "EURUSD", ("EURGBP", "GBP", 1.35), "USD", Admitted),
        ("fxcm", "USD", "USDJPY", ("EURGBP", "GBP", 1.35), "USDT", Refused("account-ccy-unset")),
        ("fxcm", "JPY", "USDJPY", ("EURUSD", "USD", 150.0), "JPY", Admitted),
        // ig mounts an EPIC: its pair token names the money. UNCHANGED: the default account on the wired
        // EURUSD epic (and a labelled one on a USD-quoted epic, any case) anchors USD.
        ("ig", "USD", "CS.D.EURUSD.MINI.IP", ("EURGBP", "GBP", 1.35), "USD", Admitted),
        ("ig", "USD", "cs.d.gbpusd.mini.ip", ("EURGBP", "GBP", 1.35), "USD", Admitted),
        // THE DEFECT (ig was exempt): a USD account mounted on a yen-quoted epic booked the mount at par in yen.
        (
            "ig",
            "USD",
            "CS.D.USDJPY.MINI.IP",
            ("EURGBP", "GBP", 1.35),
            "USDT",
            Refused("account-ccy-unset"),
        ),
        (
            "ig",
            "USD",
            "CS.D.EURGBP.MINI.IP",
            ("EURUSD", "USD", 1.0),
            "USDT",
            Refused("account-ccy-unset"),
        ),
        ("ig", "JPY", "CS.D.USDJPY.MINI.IP", ("EURUSD", "USD", 150.0), "JPY", Admitted),
        // An epic that names no recognisable pair is not guessed at: not derivable, not anchored.
        (
            "ig",
            "USD",
            "IX.D.DAX.DAILY.IP",
            ("EURGBP", "GBP", 1.35),
            "USDT",
            Refused("account-ccy-unset"),
        ),
        (
            "ig",
            "USD",
            "CS.D.BITCOIN.CFD.IP",
            ("EURGBP", "GBP", 1.35),
            "USDT",
            Refused("account-ccy-unset"),
        ),
    ];
    for (venue, ccy, mount, foreign, label, verdict) in rows {
        let (got_label, told, got_verdict) = mounted_on(venue, ccy, mount, *foreign);
        assert_eq!(got_label, *label, "{venue}: stated {ccy}, mounted on `{mount}`");
        assert_eq!(told, *label != "USDT", "{venue}: stated {ccy}, mounted on `{mount}`: told");
        assert_eq!(
            &got_verdict, verdict,
            "{venue}: stated {ccy}, mounted on `{mount}`: {foreign:?}"
        );
    }
}

/// **A LABELLED account is judged on the symbol ITS row mounts, never on the venue's wired one.** A labelled
/// row may name any symbol (the wired `EURUSD` belongs to the default account), so a USD account labelled `ALT`
/// and mounted on `USDJPY` books its mount in yen: its ledger is not a dollar ledger whatever the venue states,
/// and the engine refuses every foreign-quoted symbol `account-ccy-unset`, in words naming the stated currency, the
/// mount and its money. Beside it, the default account on `EURUSD` and the labelled one on a USD-quoted mount are
/// anchored as before. Run for each venue whose mounts are pairs; the rule reads the real mount (`symbol` of the
/// assembly), the venue's wired symbol only says whether the venue's mounts are named by their money.
#[test]
fn a_labelled_account_is_anchored_by_its_own_mount_and_not_the_wired_symbol() {
    use vike_exec::AdmitOutcome::{Admitted, Refused};
    let label = alt();
    let tier = AccountTier::Demo;
    for venue in ["fxcm", "oanda", "ctrader"] {
        let start = |label: &AccountLabel, ccy, mount| {
            let (mut engine, _) =
                built_as(label, venue, tier, Some(ccy), Some(25_000.0), &[venue], mount);
            engine.now_ms = 1_000;
            engine
        };
        let foreign = foreign_admission(venue, "EURGBP", "GBP", 1.35, 1_000);

        let mut yen = start(&label, "USD", "USDJPY");
        assert_eq!(yen.quote_asset, "USDT", "{venue}: USD account on a yen-quoted mount");
        assert!(!yen.account_ccy_known(), "{venue}");
        assert_eq!(yen.admit_symbol(&foreign), Refused("account-ccy-unset"), "{venue}");
        let why = super::unanchorable_mount(venue, "USDJPY", "USD").expect("not a USD ledger");
        assert!(
            ["`USDJPY`", "JPY", "USD"].iter().all(|word| why.contains(word)),
            "{venue}: the words name the mount and both moneys: {why}"
        );

        let mut dollars = start(&label, "USD", "GBPUSD");
        assert_eq!(dollars.quote_asset, "USD", "{venue}: labelled, mounted on a USD-quoted pair");
        assert_eq!(dollars.admit_symbol(&foreign), Admitted, "{venue}");

        let yen_ledger = start(&label, "JPY", "USDJPY");
        assert_eq!(yen_ledger.quote_asset, "JPY", "{venue}: a yen account on its yen mount");

        let mut default = start(&AccountLabel::Default, "USD", "EURUSD");
        assert_eq!(default.quote_asset, "USD", "{venue}: the default account on the wired EURUSD");
        assert_eq!(default.admit_symbol(&foreign), Admitted, "{venue}");
    }
}

/// **The mount rule reads a name it can read and refuses to guess otherwise**: six letters are a pair quoted in
/// the last three, `BASE_QUOTE` is quoted after its underscore, an epic is quoted in the quote of the pair token
/// inside it, and nothing else says its money.
#[test]
fn a_mounts_name_says_its_money_only_when_it_is_a_pair() {
    use super::mount_money;
    for (name, money) in [
        ("EURUSD", "USD"),
        ("eurusd", "USD"),
        (" GBPJPY ", "JPY"),
        ("EUR_USD", "USD"),
        ("XAU_USD", "USD"),
        ("SPX500_USD", "USD"),
        ("DE30_EUR", "EUR"),
        ("CS.D.EURUSD.MINI.IP", "USD"),
        ("cs.d.usdjpy.mini.ip", "JPY"),
        ("CS.D.GBPJPY.TODAY.IP", "JPY"),
    ] {
        assert_eq!(mount_money(name).as_deref(), Some(money), "`{name}`");
    }
    for name in [
        "",
        "EUR",
        "US500",
        "EUR/USD",
        "EUR-USD",
        "BTCUSDT",
        "BTC-USDT-SWAP",
        "AAPL.SMART.USD",
        "IX.D.DAX.DAILY.IP",
        "IX.D.NASDAQ.CASH.IP",
        "CS.D.BITCOIN.CFD.IP",
        "EURUSD.IP",
        "EURUSD1",
        "EUR_US",
        "EUR_USDT",
        "_USD",
        "EUR_USD_X",
        "EUR_US1",
        "ÉURUSD",
    ] {
        assert_eq!(mount_money(name), None, "`{name}` says no money");
    }
}

/// **The rule has an answer in words, for the venues whose mounts name their money**: it names the stated
/// currency, the mount and the mount's money; ig's epic carries its pair token (`CS.D.USDJPY.MINI.IP` is
/// quoted in JPY) and is judged by it; a venue whose wired symbol names no money at all (ibkr's
/// `AAPL.SMART.USD`) is not judged here, because its own fence stands in the root's door.
#[test]
fn the_mount_rule_judges_the_venues_whose_mounts_name_their_money() {
    use super::unanchorable_mount;
    let why =
        unanchorable_mount("oanda", "EURUSD", "EUR").expect("a USD mount is not a EUR ledger");
    for word in ["`EURUSD`", "USD", "EUR"] {
        assert!(why.contains(word), "{word} in: {why}");
    }
    assert_eq!(unanchorable_mount("oanda", "EURUSD", "USD"), None);
    assert_eq!(
        unanchorable_mount("oanda", "EURUSD", "usd"),
        None,
        "the code is compared whatever its case"
    );
    let why = unanchorable_mount("ctrader", "US500", "USD").expect("a name that says no money");
    assert!(why.contains("`US500`") && why.contains("does not say"), "{why}");
    assert_eq!(
        unanchorable_mount("ibkr", "AAPL.SMART.EUR", "USD"),
        None,
        "ibkr keeps its own fence"
    );
    // ig: the epic's pair token is the mount's money (the wired epic is USD-quoted, so the default account
    // is unchanged); a yen-quoted epic and an epic naming no pair are not a USD ledger.
    assert_eq!(unanchorable_mount("ig", "CS.D.EURUSD.MINI.IP", "USD"), None);
    assert_eq!(unanchorable_mount("ig", "cs.d.gbpusd.mini.ip", "usd"), None);
    let why = unanchorable_mount("ig", "CS.D.USDJPY.MINI.IP", "USD").expect("a yen-quoted epic");
    for word in ["`CS.D.USDJPY.MINI.IP`", "JPY", "USD"] {
        assert!(why.contains(word), "{word} in: {why}");
    }
    let why = unanchorable_mount("ig", "IX.D.DAX.DAILY.IP", "USD").expect("an epic naming no pair");
    assert!(why.contains("does not say"), "{why}");
    // The other end of that arrangement: the wiring still says which venues are judged by their names.
    let named_by_money = |venue: &str| {
        vike_venues::venue_wiring::wired_symbol(venue)
            .is_some_and(|s| super::mount_money(s).is_some())
    };
    assert!(named_by_money("oanda") && named_by_money("ctrader") && named_by_money("dukascopy"));
    assert!(named_by_money("ig") && !named_by_money("ibkr"));
}

/// **The tier half of the rule is one function, read by the assembly and the door alike**: a practice account
/// with a ceiling armed is allowed, and each other case is refused in its own words (the venue half, which
/// venues have a rate source, is the root's: `vike_tradehub::server::symbol_admission`).
#[test]
fn the_conversion_rule_names_each_way_to_be_refused() {
    use vike_model::AccountTier::{Demo, Live, Paper};
    assert_eq!(crate::quote_conversion_tier_refusal(Some(Demo), true), None);
    let words = |tier, capped| crate::quote_conversion_tier_refusal(tier, capped).expect("refused");
    assert!(words(Some(Live), true).contains("`demo` account only"));
    assert!(words(Some(Live), true).contains("`live`"));
    assert!(words(Some(Paper), true).contains("`paper`"));
    assert!(words(None, true).contains("does not say what kind of account"));
    assert!(words(Some(Demo), false).contains("exposure ceiling"));
    assert!(words(Some(Demo), false).contains("policy.max_account_exposure"));
}

// ---- FXCM's account currency: the venue's word, else an ASSUMED USD, and only for an account that converts ----
//
// Owner ruling 2026-10-19: "check the currency by API; if you can't, USD". The venue's lane hands the node the
// currency and whether it is assumed (`AdapterBind::account_ccy_assumed`); what the node does with it is
// `ledger_anchor`, the engine-assembly rule these tests hold. The opt-in is the existing one (a `demo` tier and
// `policy.max_account_exposure`), so an assumed USD can never reach a live or paper account or one with no ceiling.

use super::{LedgerAnchor, NotTold, ledger_anchor};

/// **An API-reported EUR account: EUR is used, as stated; no fallback and no flag**, on an engine mounted on a
/// pair quoted in EUR. On the wired `EURUSD` mount (quoted in USD, booked at par) the mount fence holds the anchor
/// back whatever the statement, so that engine keeps its label and refuses every foreign-quoted admission.
#[test]
fn an_api_reported_eur_account_is_anchored_to_eur_and_not_flagged() {
    assert_eq!(ledger_anchor(true, Some("EUR"), false), LedgerAnchor::Stated("EUR"));
    let (engine, _) = built_stated(
        &AccountLabel::Default,
        "fxcm",
        AccountTier::Demo,
        (Some("EUR"), false),
        Some(25_000.0),
        &["fxcm"],
        "GBPEUR",
    );
    assert_eq!(engine.quote_asset, "EUR");
    assert!(engine.account_ccy_known());
    let (fenced, _) =
        built_with("fxcm", AccountTier::Demo, Some("EUR"), false, Some(25_000.0), &["fxcm"]);
    assert_eq!(
        fenced.quote_asset, "USDT",
        "EUR on a USD-quoted mount is not anchored: the label stays"
    );
    assert!(!fenced.account_ccy_known());
}

/// **The API is silent, a `demo` account with an exposure ceiling: USD, ASSUMED and flagged**, anchored like a
/// stated one so a foreign-quoted symbol is not refused `account-ccy-unset`.
#[test]
fn a_silent_api_on_a_capped_demo_account_is_anchored_to_assumed_usd_and_flagged() {
    assert_eq!(ledger_anchor(true, Some("USD"), true), LedgerAnchor::Assumed("USD"));
    assert_ne!(
        LedgerAnchor::Assumed("USD"),
        LedgerAnchor::Stated("USD"),
        "an assumption is a different fact from a statement, whatever the code"
    );
    let (engine, _) =
        built_with("fxcm", AccountTier::Demo, Some("USD"), true, Some(25_000.0), &["fxcm"]);
    assert_eq!(engine.quote_asset, "USD");
    assert!(engine.account_ccy_known());
}

/// **The API is silent and the account is `live`: refused**, in the sense the engine is told nothing and keeps
/// its label (the old refusal of a foreign-quoted symbol stands), whether or not a ceiling is armed.
#[test]
fn a_silent_api_on_a_live_account_is_never_anchored() {
    let converts = crate::quote_conversion_tier_refusal(Some(AccountTier::Live), true).is_none();
    assert!(!converts, "a live account does not convert");
    assert_eq!(
        ledger_anchor(converts, Some("USD"), true),
        LedgerAnchor::NotTold(NotTold::AccountDoesNotConvert)
    );
    for ceiling in [Some(25_000.0), None] {
        let (engine, _) =
            built_with("fxcm", AccountTier::Live, Some("USD"), true, ceiling, &["fxcm"]);
        assert_eq!(engine.quote_asset, "USDT", "a live account is never relabelled ({ceiling:?})");
        assert!(!engine.account_ccy_known());
    }
}

/// **The API is silent and no account-wide ceiling is set: refused**, a `demo` account included.
#[test]
fn a_silent_api_with_no_exposure_ceiling_is_never_anchored() {
    let converts = crate::quote_conversion_tier_refusal(Some(AccountTier::Demo), false).is_none();
    assert!(!converts, "no ceiling, no conversion");
    assert_eq!(
        ledger_anchor(converts, Some("USD"), true),
        LedgerAnchor::NotTold(NotTold::AccountDoesNotConvert)
    );
    let (engine, _) = built_with("fxcm", AccountTier::Demo, Some("USD"), true, None, &["fxcm"]);
    assert_eq!(engine.quote_asset, "USDT");
    assert!(!engine.account_ccy_known());
    // A ceiling of zero is not a ceiling either.
    let (engine, _) =
        built_with("fxcm", AccountTier::Demo, Some("USD"), true, Some(0.0), &["fxcm"]);
    assert!(!engine.account_ccy_known());
}

/// No currency stated is no anchor, assumed flag or not; a blank code is none; a venue the root did not name has
/// no rate source and is never relabelled.
#[test]
fn no_currency_and_no_rate_source_anchor_nothing() {
    assert_eq!(ledger_anchor(true, None, true), LedgerAnchor::NotTold(NotTold::NoCurrency));
    assert_eq!(ledger_anchor(true, Some("  "), false), LedgerAnchor::NotTold(NotTold::NoCurrency));
    assert_eq!(ledger_anchor(true, Some(" eur "), false), LedgerAnchor::Stated("eur"));
    let (engine, _) =
        built_with("fxcm", AccountTier::Demo, Some("USD"), true, Some(25_000.0), &["oanda"]);
    assert!(!engine.account_ccy_known(), "fxcm is not a converting venue to this root");
}

/// **An ASSUMED account currency is warned in the owner's words, naming the venue**: "account currency not
/// stated by <venue>: assuming USD" (the node assumes it for a converting venue whose API states none, and FXCM's
/// lane has always assumed it); a currency the venue STATED is anchored at `info` and never warned about.
#[tracing_test::traced_test]
#[test]
fn an_assumed_account_currency_is_warned_naming_the_venue_and_a_stated_one_is_not() {
    let (engine, _) =
        built_with("ctrader", AccountTier::Demo, Some("USD"), true, Some(25_000.0), &["ctrader"]);
    assert_eq!(engine.quote_asset, "USD");
    assert!(logs_contain("account currency not stated by ctrader: assuming USD"));
    assert!(logs_contain("ASSUMED account currency"), "the line the docs and the drills look for");

    let (engine, _) =
        built_with("oanda", AccountTier::Demo, Some("USD"), false, Some(25_000.0), &["oanda"]);
    assert_eq!(engine.quote_asset, "USD");
    assert!(!logs_contain("account currency not stated by oanda"), "the venue stated it");
}

// ---- the engine's rate windows, pinned by VALUE through the words it refuses in -----------------

/// The clock the window test's engines run on, in ms.
const NOW: i64 = 1_000_000;

/// The engine a start of the default oanda account (USD, a ceiling, mounted on `EURUSD`) builds, its clock at [`NOW`].
fn usd_engine() -> Engine {
    let tier = AccountTier::Demo;
    let (mut engine, _) =
        built_under("oanda", tier, Some("USD"), Some(25_000.0), &["oanda"], MOUNT);
    engine.now_ms = NOW;
    engine
}

/// **The windows of a conversion rate are part of what a replay reproduces, so they are pinned by value** (decision
/// 0183): the engine's own tests state each one relative to its constant, which a change would move with it. Here
/// the numbers are literals and the observable is the refusal word a node's admission gets back: a rate stays
/// good for 120 s and not a millisecond more (`quote-rate-stale`), may be stamped 5 s ahead of the engine's clock
/// and not a millisecond more (`quote-rate-future`), may move by a factor of 1.5 in one update and not more
/// (`quote-rate-jump`), and a refused jump is confirmed by the same rate stamped a full second later, not before.
#[test]
fn the_rate_windows_are_two_minutes_five_seconds_one_second_and_one_and_a_half() {
    use vike_exec::AdmitOutcome::{Admitted, Refreshed, Refused};
    let ask = |engine: &mut Engine, rate: f64, as_of_ms: i64| {
        engine.admit_symbol(&foreign_admission("oanda", "EURGBP", "GBP", rate, as_of_ms))
    };
    // age
    assert_eq!(ask(&mut usd_engine(), 1.35, NOW - 120_000), Admitted);
    assert_eq!(ask(&mut usd_engine(), 1.35, NOW - 120_001), Refused("quote-rate-stale"));
    // a stamp ahead of the clock
    assert_eq!(ask(&mut usd_engine(), 1.35, NOW + 5_000), Admitted);
    assert_eq!(ask(&mut usd_engine(), 1.35, NOW + 5_001), Refused("quote-rate-future"));
    // the step one update may take
    let mut held = usd_engine();
    assert_eq!(ask(&mut held, 1.0, NOW), Admitted);
    assert_eq!(ask(&mut held, 1.5, NOW), Refreshed, "x1.5 is one move");
    let mut jumped = usd_engine();
    assert_eq!(ask(&mut jumped, 1.0, NOW), Admitted);
    assert_eq!(ask(&mut jumped, 1.5001, NOW), Refused("quote-rate-jump"), "more is a jump");
    // the sight that confirms a jump
    let mut engine = usd_engine();
    assert_eq!(ask(&mut engine, 1.0, NOW), Admitted);
    assert_eq!(ask(&mut engine, 2.0, NOW + 10), Refused("quote-rate-jump"));
    assert_eq!(
        ask(&mut engine, 2.0, NOW + 10 + 999),
        Refused("quote-rate-jump"),
        "999 ms: the same sight"
    );
    assert_eq!(
        ask(&mut engine, 2.0, NOW + 10 + 1_000),
        Refreshed,
        "1,000 ms later: a second sight"
    );
}

// ---- a MAINNET lane twin (the owner opened binance's `live` tier for it, 2026-10-19) ---------------------
//
// The twin is `binance#PERP`, an engine DERIVED in memory from the default account's row while that row
// states a leverage (`vike_secrets::Accounts::derive_lane_twins`). At `demo` a leverage the venue did not
// confirm leaves it trading at 1x. At `live` the start WRITES that leverage to the real account, so it does
// not start at all (`live_twin_must_hold_its_leverage`). Everything else about its start is the parent's.

fn perp() -> AccountLabel {
    AccountLabel::parse("PERP").expect("a legal label")
}

/// The arming table of a box whose binance default account, its derived lane twin `PERP` and a labelled
/// `ALT` account are all armed at `tier`.
fn twin_table(tier: AccountTier) -> ArmingTable {
    let facts = BTreeMap::from([(
        "binance".to_string(),
        VenueFacts {
            adapter_present: true,
            addresses_accounts: true,
            takes_recon_trigger: false,
            grid_source: GridSource::NoGrid,
            process_exclusive: None,
            clock_decl_word: "not-wired",
        },
    )]);
    let row = |label: AccountLabel| ArmingRow {
        arming: VenueArming {
            venue: "binance",
            label,
            tier: Some(tier),
            effective: Some(tier),
            block: ArmingBlock::None,
            account_ids: vec![3],
        },
        book: None,
    };
    ArmingTable::new(facts, vec![row(AccountLabel::Default), row(perp()), row(alt())])
}

/// The policy of a box whose store holds `rows`, read the way a root reads the store: the lane twin is
/// DERIVED from them (`AccountDirectory::read`; `AdapterPolicy::with_account_row` derives none).
fn twin_policy_of(rows: Vec<vike_secrets::Account>) -> AdapterPolicy {
    AdapterPolicy {
        accounts: vike_secrets::account_directory::AccountDirectory::read(
            Ok::<_, String>(vike_secrets::Accounts::Known(rows)),
            Ok::<_, String>(None),
            Ok::<_, String>(vike_secrets::account_credentials::CredentialBook::new()),
        ),
        ..AdapterPolicy::default()
    }
}

/// [`twin_policy_of`] for binance's DEFAULT account (id 3, active) at `tier`, stating `stated` and
/// `max_exposure`.
fn twin_policy(tier: AccountTier, stated: Option<f64>, max_exposure: Option<f64>) -> AdapterPolicy {
    twin_policy_of(vec![vike_secrets::Account {
        tier,
        max_exposure: max_exposure.and_then(vike_secrets::MaxExposure::new),
        ..row(3, "binance", &AccountLabel::Default, stated, None)
    }])
}

/// What one start of a binance account left behind.
struct TwinStart {
    result: Result<crate::EngineAndRecon, EngineError>,
    calls: Vec<ScriptedCall>,
    live: HashSet<String>,
}

/// A start of binance's `label` account on `symbol` at `tier` over `policy`, the venue answering `cap`.
fn start_twin(
    tier: AccountTier,
    label: &AccountLabel,
    symbol: &str,
    policy: &AdapterPolicy,
    cap: Option<Result<f64, String>>,
    risk: Option<&vike_model::ProfileRisk>,
) -> TwinStart {
    let table = twin_table(tier);
    // The scripted outcomes are keyed by route key: the default account, the twin and `ALT` each get one.
    let outcome = || Ok(AdapterBind { bound_tier: tier, ..bind(cap.clone()) });
    let backend = ScriptedBackend::new(table.clone())
        .script_start("binance", outcome())
        .script_start("binance#PERP", outcome())
        .script_start("binance#ALT", outcome());
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut live = HashSet::new();
    let result = {
        let mut env = AdapterEnv::new(&backend, &table, &tx, &mut live);
        env.risk_profile = risk;
        env.policy = Some(policy);
        make_engine_for_account(&mut env, "binance", symbol, label, &[])
    };
    TwinStart { result, calls: backend.calls(), live }
}

/// The figure each start put on the wire (`Some(None)` = a start that asked nothing; `None` = no start).
fn asked(calls: &[ScriptedCall]) -> Option<Option<f64>> {
    calls.iter().find_map(|call| match call {
        ScriptedCall::Start { account_leverage, .. } => Some(*account_leverage),
        _ => None,
    })
}

fn started(result: Result<crate::EngineAndRecon, EngineError>) -> Engine {
    match result {
        Ok((engine, _)) => engine,
        Err(other) => panic!("the start was refused: {other:?}"),
    }
}

/// The reason a start was refused, which must be "this account does not run".
fn not_running(result: Result<crate::EngineAndRecon, EngineError>) -> crate::NotRunning {
    match result {
        Err(EngineError::NotRunning(stopped)) => stopped,
        Err(other) => panic!("the start was refused for another reason: {other:?}"),
        Ok(_) => panic!("the account started"),
    }
}

#[test]
fn a_live_twin_whose_leverage_the_venue_holds_starts_at_it() {
    let policy = twin_policy(AccountTier::Live, Some(5.0), None);
    assert!(
        policy.accounts.is_derived_lane_twin("binance", "PERP"),
        "a live row's twin is derived"
    );
    let risk = budget();

    let twin =
        start_twin(AccountTier::Live, &perp(), "BTCUSDT.P", &policy, Some(Ok(5.0)), Some(&risk));
    assert_eq!(
        asked(&twin.calls),
        Some(Some(5.0)),
        "the owner's figure is on the wire, for the twin"
    );
    assert!(twin.live.contains("binance#PERP"), "a running twin joins the LIVE record");
    let engine = started(twin.result);
    assert_eq!(engine.gate.limits.im_requirement, Some(0.2), "margined at the confirmed 5x");
    assert_eq!(
        (engine.route_key.as_str(), engine.tier),
        ("binance#PERP", AccountTier::Live),
        "the twin's own route key, at LIVE: a demo twin's engine is never this one"
    );

    // The parent: the same row, the figure stripped in the derived view: a spot engine at 1x, no ask.
    let parent = start_twin(
        AccountTier::Live,
        &AccountLabel::Default,
        "BTCUSDT",
        &policy,
        None,
        Some(&risk),
    );
    assert_eq!(
        asked(&parent.calls),
        Some(None),
        "the spot engine is never asked to hold a futures figure"
    );
    let parent = started(parent.result);
    assert_eq!(parent.gate.limits.im_requirement, Some(1.0));
    assert_eq!((parent.route_key.as_str(), parent.tier), ("binance", AccountTier::Live));
}

#[test]
fn a_live_twin_the_venue_did_not_confirm_does_not_start_at_all() {
    let policy = twin_policy(AccountTier::Live, Some(5.0), None);
    let risk = budget();
    let cases = [
        ("held below", Some(Ok(3.0))),
        ("held above", Some(Ok(9.0))),
        ("the lane refused", Some(Err("isolated margin".to_string()))),
        ("an older gatewayhub", None),
        ("an unusable answer", Some(Ok(f64::NAN))),
        ("a sub-1 answer", Some(Ok(0.5))),
    ];
    for (what, cap) in cases {
        let twin = start_twin(AccountTier::Live, &perp(), "BTCUSDT.P", &policy, cap, Some(&risk));
        assert_eq!(asked(&twin.calls), Some(Some(5.0)), "{what}: the venue was asked");
        assert!(
            !twin.live.contains("binance#PERP"),
            "{what}: a refused twin never lights the LIVE record"
        );
        let stopped = not_running(twin.result);
        assert_eq!(stopped.block, ArmingBlock::AdapterFailed, "{what}");
        assert_eq!(stopped.tier, Some(AccountTier::Live), "{what}");
        assert_eq!(stopped.route_key(), "binance#PERP", "{what}");
    }
}

#[test]
fn a_live_twin_with_nothing_to_ask_is_refused_before_the_venue_is_called() {
    // A fraction planted on the row (the verb refuses one; a hand-edited store may carry it): the twin
    // is derived, no whole number can be asked, and the bridge's start would write ITS default.
    let policy = twin_policy(AccountTier::Live, Some(2.5), None);
    assert!(policy.accounts.is_derived_lane_twin("binance", "PERP"));
    let risk = budget();
    let twin =
        start_twin(AccountTier::Live, &perp(), "BTCUSDT.P", &policy, Some(Ok(2.0)), Some(&risk));
    assert!(twin.calls.is_empty(), "the venue was never asked anything: {:?}", twin.calls);
    assert!(!twin.live.contains("binance#PERP"));
    assert_eq!(not_running(twin.result).block, ArmingBlock::AdapterFailed);
}

/// **A refused start gives its lease back** (review r2, finding 2). The venue did not confirm the figure, so
/// the shared tail refuses the twin AFTER the gatewayhub started its adapter: the client it was handed is
/// told `begin_detach` then `detach` (which ends the lease and its mainnet session), and the backend is told
/// once to forget the start. A twin that DOES start is neither detached nor released.
#[test]
fn a_refused_live_twin_ends_its_lease_and_the_backend_forgets_the_start() {
    use std::sync::{Arc, Mutex};
    let policy = twin_policy(AccountTier::Live, Some(5.0), None);
    let risk = budget();
    let table = twin_table(AccountTier::Live);
    let run = |cap: Option<Result<f64, String>>| {
        let log = Arc::new(Mutex::new(Vec::new()));
        let backend = ScriptedBackend::new(table.clone()).script_start(
            "binance#PERP",
            Ok(AdapterBind {
                client: Box::new(crate::testutil::DetachProbe(Arc::clone(&log))),
                bound_tier: AccountTier::Live,
                ..bind(cap)
            }),
        );
        let (tx, _rx) = vike_exec::event_channel(8);
        let mut live = HashSet::new();
        let result = {
            let mut env = AdapterEnv::new(&backend, &table, &tx, &mut live);
            env.risk_profile = Some(&risk);
            env.policy = Some(&policy);
            make_engine_for_account(&mut env, "binance", "BTCUSDT.P", &perp(), &[])
        };
        let released = backend
            .calls()
            .into_iter()
            .filter(|call| matches!(call, ScriptedCall::ReleaseRefusedStart { .. }))
            .collect::<Vec<_>>();
        let teardown = log.lock().expect("the log").clone();
        (result, teardown, released)
    };

    let (result, teardown, released) = run(Some(Ok(3.0)));
    assert_eq!(not_running(result).block, ArmingBlock::AdapterFailed);
    assert_eq!(teardown, ["begin_detach", "detach"], "the refused lease is ended, in both phases");
    assert_eq!(
        released,
        [ScriptedCall::ReleaseRefusedStart {
            venue: "binance".to_string(),
            account: perp(),
            route_key: "binance#PERP".to_string(),
        }],
        "the backend is told once to forget the start"
    );

    let (result, teardown, released) = run(Some(Ok(5.0)));
    assert!(result.is_ok(), "a confirmed twin starts");
    assert!(teardown.is_empty() && released.is_empty(), "…and keeps its lease: {teardown:?}");
}

#[test]
fn a_live_row_that_states_no_leverage_has_no_twin_and_asks_nothing() {
    let policy = twin_policy(AccountTier::Live, None, Some(100.0));
    assert!(!policy.accounts.is_derived_lane_twin("binance", "PERP"));
    let risk = budget();
    // The default live account starts as it always did: no ask, 1x, its own route key.
    for cap in [None, Some(Ok(10.0))] {
        let spot = start_twin(
            AccountTier::Live,
            &AccountLabel::Default,
            "BTCUSDT",
            &policy,
            cap,
            Some(&risk),
        );
        assert_eq!(asked(&spot.calls), Some(None), "no figure stated, no ask");
        let engine = started(spot.result);
        assert_eq!(engine.gate.limits.im_requirement, Some(1.0));
        assert_eq!(engine.route_key, "binance");
    }
    // …and there is no perpetual engine to start: the twin's label is no account of this box.
    let twin = start_twin(AccountTier::Live, &perp(), "BTCUSDT.P", &policy, None, Some(&risk));
    assert!(twin.calls.is_empty(), "nothing was started or asked: {:?}", twin.calls);
    assert_eq!(not_running(twin.result).block, ArmingBlock::NoAccountRow);
}

#[test]
fn a_demo_twin_keeps_the_demo_rule() {
    let risk = budget();
    // Demo: a figure the venue did not confirm leaves the twin trading at 1x, exactly as before.
    let policy = twin_policy(AccountTier::Demo, Some(5.0), None);
    for cap in [None, Some(Err("down".to_string())), Some(Ok(3.0))] {
        let twin = start_twin(AccountTier::Demo, &perp(), "BTCUSDT.P", &policy, cap, Some(&risk));
        assert_eq!(asked(&twin.calls), Some(Some(5.0)));
        let engine = started(twin.result);
        assert_eq!(engine.gate.limits.im_requirement, Some(1.0));
        assert_eq!((engine.route_key.as_str(), engine.tier), ("binance#PERP", AccountTier::Demo));
    }
    // A planted fraction on a demo twin is ignored: it starts, nothing is asked.
    let policy = twin_policy(AccountTier::Demo, Some(2.5), None);
    let twin =
        start_twin(AccountTier::Demo, &perp(), "BTCUSDT.P", &policy, Some(Ok(2.0)), Some(&risk));
    assert_eq!(asked(&twin.calls), Some(None));
    assert_eq!(started(twin.result).gate.limits.im_requirement, Some(1.0));

    // A labelled LIVE account that states a figure (a hand-edited store; the verb refuses it) is NOT asked
    // anything: the owner opened the live TWIN only (re-review), so the view drops the figure and the account
    // starts exactly as it did before, at 1x, whatever the venue would have answered. The directory says so.
    let policy = twin_policy_of(vec![vike_secrets::Account {
        tier: AccountTier::Live,
        ..row(5, "binance", &alt(), Some(5.0), None)
    }]);
    assert!(!policy.accounts.is_derived_lane_twin("binance", "ALT"));
    assert_eq!(policy.accounts.ignored_live_leverages().len(), 1, "said, once, by the node");
    for cap in [None, Some(Ok(5.0)), Some(Err("down".to_string()))] {
        let labelled = start_twin(AccountTier::Live, &alt(), "BTCUSDT", &policy, cap, Some(&risk));
        assert_eq!(asked(&labelled.calls), Some(None), "no figure is asked of the venue");
        assert_eq!(started(labelled.result).gate.limits.im_requirement, Some(1.0));
    }

    // The default LIVE row that states a figure is the twin's parent: the figure is the twin's, asked for the
    // twin only; the spot engine asks nothing and nothing is ignored.
    let policy = twin_policy(AccountTier::Live, Some(5.0), None);
    assert!(policy.accounts.ignored_live_leverages().is_empty());
    let twin =
        start_twin(AccountTier::Live, &perp(), "BTCUSDT.P", &policy, Some(Ok(5.0)), Some(&risk));
    assert_eq!(asked(&twin.calls), Some(Some(5.0)));
    let spot = start_twin(
        AccountTier::Live,
        &AccountLabel::Default,
        "BTCUSDT",
        &policy,
        None,
        Some(&risk),
    );
    assert_eq!(asked(&spot.calls), Some(None));
}

/// The reason a BUY of `qty` at `price` on `symbol` is denied by `engine`'s gate, or `None`.
fn denial(engine: &mut Engine, symbol: &str, coid: &str, qty: f64, price: f64) -> Option<String> {
    let request = vike_model::OrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: symbol.into(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(price),
        ..Default::default()
    };
    let mut outbox = vike_exec::Outbox::default();
    engine.submit_order(&request, 1_000, &mut outbox);
    outbox.0.iter().find_map(|event| match event {
        vike_model::events::Event::OrderDenied(denied) => Some(denied.reason.to_string()),
        _ => None,
    })
}

/// Land a balance the gate's margin lane can fund an order from (the first reconcile pass's job).
fn fund(engine: &mut Engine) {
    engine.account.balance = 1_000_000.0;
    engine.account.balance_mode = vike_exec::BalanceMode::Authoritative;
}

/// Hand the engine the first mark of `symbol`, as the node's mark lane does. The derived twin carries the mark
/// floor (`RiskGate::require_mark`): until one has arrived it denies an opening order on its mounted symbol
/// `no-mark-for-mounted-symbol`, before any ceiling is asked. Its parent, the spot engine, is priced by the venue's
/// quote route and is not armed.
fn mark(engine: &mut Engine, symbol: &str, px: f64) {
    engine.price_board.set_mark("binance", symbol, px, 1);
    engine.account.set_mark_from("binance", symbol, px, vike_exec::MarkSource::VenueMark, 1);
}

#[test]
fn a_live_twin_never_gets_a_looser_ceiling_than_its_parent() {
    let risk = vike_model::ProfileRisk {
        max_notional_per_order: Some(2_500.0),
        max_total_exposure: Some(9_000.0),
        max_leverage: Some(10.0),
        ..Default::default()
    };
    // (the box-wide account ceiling, the account's own, the tighter of the two)
    let ceilings = [
        (None, Some(100.0), Some(100.0)),
        (Some(50.0), Some(100.0), Some(50.0)),
        (Some(500.0), Some(100.0), Some(100.0)),
        (Some(50.0), None, Some(50.0)),
        (None, None, None),
    ];
    // (policy.max_leverage, what the venue then holds)
    for (leverage_ceiling, held) in [(None, 10.0), (Some(4.0), 4.0)] {
        for (box_cap, own_cap, want) in ceilings {
            let mut policy = twin_policy(AccountTier::Live, Some(10.0), own_cap);
            policy.max_account_exposure = box_cap;
            policy.max_leverage = leverage_ceiling;
            let what =
                format!("box {box_cap:?}, own {own_cap:?}, leverage ceiling {leverage_ceiling:?}");
            let default = AccountLabel::Default;
            let parent =
                start_twin(AccountTier::Live, &default, "BTCUSDT", &policy, None, Some(&risk));
            let mut parent = started(parent.result);
            let twin = start_twin(
                AccountTier::Live,
                &perp(),
                "BTCUSDT.P",
                &policy,
                Some(Ok(held)),
                Some(&risk),
            );
            let mut twin = started(twin.result);
            // The limits: the twin carries exactly the parent's account-wide ceilings, and is never
            // margined more loosely (a larger im_requirement is the tighter gate).
            let (p, t) = (&parent.gate.limits, &twin.gate.limits);
            assert_eq!(
                t.max_account_exposure, want,
                "{what}: the tighter of the box's and the account's"
            );
            assert_eq!(t.max_account_exposure, p.max_account_exposure, "{what}");
            assert_eq!(t.max_notional_per_order, Some(2_500.0), "{what}");
            assert_eq!(t.max_notional_per_order, p.max_notional_per_order, "{what}");
            assert_eq!(t.max_total_exposure, p.max_total_exposure, "{what}");
            assert!(
                t.im_requirement >= p.im_requirement,
                "{what}: {:?} vs {:?}",
                t.im_requirement,
                p.im_requirement
            );
            assert!(
                t.im_requirement.is_some_and(|im| im >= 0.1),
                "{what}: never above the run profile's 10x"
            );

            // The gate itself, on the same orders: the twin denies exactly what its parent denies.
            fund(&mut parent);
            fund(&mut twin);
            // The twin is priced the way production prices it (its own symbol gets no tick): once marked it
            // judges exactly as its parent, which the orders below are priced like.
            mark(&mut twin, "BTCUSDT.P", 30_000.0);
            let mut orders = vec![(0.1, 30_000.0)]; // 3000 > max_notional_per_order
            if let Some(cap) = want {
                orders.push((cap * 1.5 / 30_000.0, 30_000.0)); // over the account-wide ceiling
                orders.push((cap * 0.5 / 30_000.0, 30_000.0)); // inside it
            }
            for (i, (qty, price)) in orders.into_iter().enumerate() {
                let (a, b) = (
                    denial(&mut parent, "BTCUSDT", &format!("p-{i}"), qty, price),
                    denial(&mut twin, "BTCUSDT.P", &format!("t-{i}"), qty, price),
                );
                assert_eq!(a, b, "{what}: order {i} ({qty} @ {price})");
                // The gate's reason is a word, then its figures (`over-account-exposure (projected …)`).
                let word = b.as_deref().map(|reason| reason.split(' ').next().unwrap_or(reason));
                match i {
                    0 => assert_eq!(word, Some("over-max-notional"), "{what}: {b:?}"),
                    1 => assert_eq!(word, Some("over-account-exposure"), "{what}: {b:?}"),
                    _ => assert_eq!(b, None, "{what}: inside every ceiling"),
                }
            }

            // The kill switch: a halted twin opens nothing, as its parent.
            parent.trading_state = vike_model::TradingState::Halted;
            twin.trading_state = vike_model::TradingState::Halted;
            let (a, b) = (
                denial(&mut parent, "BTCUSDT", "p-halt", 0.0001, 30_000.0),
                denial(&mut twin, "BTCUSDT.P", "t-halt", 0.0001, 30_000.0),
            );
            let word = |reason: &Option<String>| {
                reason.as_deref().map(|r| r.split(' ').next().unwrap_or(r).to_string())
            };
            assert_eq!(
                (word(&a).as_deref(), word(&b).as_deref()),
                (Some("halted"), Some("halted")),
                "{what}: {a:?} / {b:?}"
            );
        }
    }
}

#[test]
fn a_live_twin_is_refused_without_a_risk_budget_like_its_parent_and_not_when_inactive() {
    let policy = twin_policy(AccountTier::Live, Some(5.0), None);
    // No run profile at all: a LIVE start has no safe default to guess, for the twin as for its parent.
    for (label, symbol, cap) in
        [(perp(), "BTCUSDT.P", Some(Ok(5.0))), (AccountLabel::Default, "BTCUSDT", None)]
    {
        let start = start_twin(AccountTier::Live, &label, symbol, &policy, cap, None);
        match start.result {
            Err(EngineError::MissingRiskBudget { venue, .. }) => assert_eq!(venue, "binance"),
            Err(other) => panic!("{label}: refused for another reason: {other:?}"),
            Ok(_) => panic!("{label}: a LIVE engine started with no risk budget"),
        }
        assert!(
            !start.calls.iter().any(|c| matches!(c, ScriptedCall::Start { .. })),
            "{label}: refused BEFORE the venue was asked"
        );
    }
    // The account's switch: an inactive row derives an inactive twin, and an inactive account does not run.
    let off = twin_policy_of(vec![vike_secrets::Account {
        active: false,
        tier: AccountTier::Live,
        ..row(3, "binance", &AccountLabel::Default, Some(5.0), None)
    }]);
    let risk = budget();
    let start =
        start_twin(AccountTier::Live, &perp(), "BTCUSDT.P", &off, Some(Ok(5.0)), Some(&risk));
    assert!(start.calls.is_empty(), "nothing is asked of an account that is switched off");
    assert_eq!(not_running(start.result).block, ArmingBlock::AccountInactive);
}

#[test]
fn only_a_derived_live_twin_must_hold_its_leverage() {
    let live = AccountTier::Live;
    let derived = twin_policy(live, Some(5.0), None);
    let must = |policy: Option<&AdapterPolicy>, venue, label: &AccountLabel, tier| {
        crate::live_twin_must_hold_its_leverage(policy, venue, label, tier)
    };
    assert!(must(Some(&derived), "binance", &perp(), live));
    // Not at demo, not for the default or a labelled account, not without a directory.
    assert!(!must(Some(&derived), "binance", &perp(), AccountTier::Demo));
    assert!(!must(Some(&derived), "binance", &AccountLabel::Default, live));
    assert!(!must(Some(&derived), "binance", &alt(), live));
    assert!(!must(None, "binance", &perp(), live));
    assert!(!must(Some(&AdapterPolicy::default()), "binance", &perp(), live));
    // A REAL account that spelled `PERP` is no derived twin.
    let real = twin_policy_of(vec![
        vike_secrets::Account {
            tier: live,
            ..row(3, "binance", &AccountLabel::Default, Some(5.0), None)
        },
        vike_secrets::Account { tier: live, ..row(9, "binance", &perp(), Some(5.0), None) },
    ]);
    assert!(!real.accounts.is_derived_lane_twin("binance", "PERP"));
    assert!(!must(Some(&real), "binance", &perp(), live));
    // bybit and okx keep `live` closed by the table: no twin and no strictness there.
    for venue in ["bybit", "okx"] {
        assert!(!must(Some(&derived), venue, &perp(), live), "{venue}");
    }
}

/// **The mark floor (`RiskGate::require_mark`) is armed on the DERIVED lane twin and on no other engine**,
/// through the real `make_engine_for_account` (the floor exists for the twin, whose mounted symbol the venue's
/// quote route never prices). Not on the default spot engine of the same row, which that route prices (a floor
/// there would hold every opening order of a mainnet spot account while the price lane is down), not on a
/// labelled account that is no twin, and not on a REAL account that spelled `PERP`, which the row does not
/// derive: it is judged by the predicate that names a twin everywhere else, `is_derived_lane_twin`, never the
/// label.
#[test]
fn the_mark_floor_is_armed_for_the_derived_lane_twin_and_no_other_engine() {
    let risk = budget();
    let armed = |policy: &AdapterPolicy, label: &AccountLabel, symbol: &str| {
        let start = start_twin(AccountTier::Demo, label, symbol, policy, None, Some(&risk));
        started(start.result).gate.require_mark
    };
    // The default row states a leverage (so the twin is derived) and a labelled `ALT` account is beside it.
    let derived = twin_policy_of(vec![
        row(3, "binance", &AccountLabel::Default, Some(5.0), None),
        row(4, "binance", &alt(), None, None),
    ]);
    assert!(derived.accounts.is_derived_lane_twin("binance", "PERP"));
    assert!(armed(&derived, &perp(), "BTCUSDT.P"), "the derived twin");
    assert!(!armed(&derived, &AccountLabel::Default, "BTCUSDT"), "its parent, the spot engine");
    assert!(!armed(&derived, &alt(), "BTCUSDT"), "a labelled account that is no twin");

    // A spot-only row states no leverage and derives no twin: nothing is armed there either.
    let spot_only = twin_policy(AccountTier::Demo, None, None);
    assert!(!armed(&spot_only, &AccountLabel::Default, "BTCUSDT"));

    // A REAL account that spelled `PERP` is no twin, whatever its label says.
    let real = twin_policy_of(vec![
        row(3, "binance", &AccountLabel::Default, Some(5.0), None),
        row(9, "binance", &perp(), Some(5.0), None),
    ]);
    assert!(!real.accounts.is_derived_lane_twin("binance", "PERP"));
    assert!(!armed(&real, &perp(), "BTCUSDT"), "a real account labelled PERP");
}
