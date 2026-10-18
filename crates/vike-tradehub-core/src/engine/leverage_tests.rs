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

use std::collections::{BTreeMap, HashSet};

use vike_config::{ArmingBlock, VenueArming};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use crate::symbol_grid::GridSource;
use crate::testutil::NoopClient;
use crate::{
    AdapterBind, AdapterEnv, AdapterPolicy, ArmingRow, ArmingTable, EngineError, ScriptedBackend,
    VenueFacts, make_engine_for_account,
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
    // A venue the platform cannot confirm against ignores the figure even beside a confirmation.
    let policy = policy_with("bybit", &[row(1, "bybit", &AccountLabel::Default, Some(10.0), None)]);
    let (im, _) = start("bybit", &AccountLabel::Default, &policy, Some(Ok(50.0)), &budget());
    assert_eq!(im, Some(1.0), "bybit is not a confirmable venue");
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
