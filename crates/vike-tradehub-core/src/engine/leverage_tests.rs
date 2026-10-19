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
    for venue in POSTING {
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
