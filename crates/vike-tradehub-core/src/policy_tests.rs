//! `policy`'s unit tests: each settings row reaches `AdapterPolicy`'s projection, or none; and the
//! account table it carries is the directory `vike_config::account_arming` reads (the tier tests live there).

use super::*;

/// `policy.halt_admit` row → `Policy` → `AdapterPolicy` → the mode `make_engine` puts in force,
/// landing DIFFERENTLY per venue (why it is carried, not assumed everywhere).
#[test]
fn a_configured_verify_reaches_the_one_venue_that_can_honour_it_and_degrades_elsewhere() {
    let policy = Policy { halt_admit: HaltAdmit::Verify, ..Policy::default() };
    let mount = AdapterPolicy::from(&policy);
    assert_eq!(mount.halt_admit, HaltAdmit::Verify, "the projection must carry it verbatim");

    // cTrader honours it: the only adapter with a position book at its halt boundary.
    assert_eq!(
        vike_model::effective_halt_admit(mount.halt_admit, "ctrader"),
        (HaltAdmit::Verify, None)
    );
    // Everywhere else it degrades WITH a REASON: a silent degrade is the defect this removes.
    for venue in vike_model::VENUES.iter().filter(|v| **v != "ctrader") {
        let (effective, why) = vike_model::effective_halt_admit(mount.halt_admit, venue);
        assert_eq!(effective, HaltAdmit::Admit, "{venue}");
        assert!(why.is_some(), "{venue} degraded with nothing to tell the operator");
    }
}

/// `Default` IS the no-policy value: passing it and passing nothing are interchangeable.
#[test]
fn the_default_is_the_no_policy_answer() {
    assert_eq!(AdapterPolicy::default().market_slippage, None);
    assert_eq!(AdapterPolicy::default().halt_admit, HaltAdmit::Admit);
    assert_eq!(
        vike_config::account_arming::account_tier(
            Some(&AdapterPolicy::default().accounts),
            "bybit",
            &AccountLabel::Default
        ),
        Err(vike_config::ArmingBlock::NoAccountRow),
        "no account table read: no account runs"
    );
    assert_eq!(AdapterPolicy::from(&Policy::default()), AdapterPolicy::default());
}

/// `policy.max_leverage` is CARRIED, as a ceiling and nothing else: a written row arrives on the
/// mount verbatim, and no row is `None` — NO ceiling — on the mount as on the policy (the old `1.0`
/// default is why it was once dropped: carried, it would have clamped every deployment with no row
/// to 1×). `the_leverage_ceiling_narrows_the_profile_for_the_gate_and_never_sets_or_raises` (in
/// `contract_tests/arming.rs`) drives the carried value through the real mount.
#[test]
fn the_leverage_ceiling_is_carried_and_absence_is_no_ceiling() {
    let written = AdapterPolicy::from(&Policy { max_leverage: Some(3.0), ..Policy::default() });
    assert_eq!(written.max_leverage, Some(3.0), "a stored row reaches the mount verbatim");
    assert_ne!(written, AdapterPolicy::default(), "the value is observable on the mount's side");

    assert_eq!(AdapterPolicy::from(&Policy::default()).max_leverage, None);
    assert_eq!(AdapterPolicy::default().max_leverage, None, "no row, no ceiling");
}

/// **THE RELATIONSHIP GATE, stated behaviourally rather than in a comment.**
///
/// `policy` and a run profile's `[risk]` both carry `max_notional_per_order` and judge different
/// acts: the profile's reaches `vike_model::RiskLimits` (EVERY order the core admits); this one
/// guards three EDGE surfaces a human types at and reaches no `RiskLimits`. The difference is the
/// `max_notional_per_order: _` binding; its other records (that binding's `//`, a
/// `vike_config::ceilings::PRE_TRADE_CEILINGS` row) are prose, and folding the field in leaves
/// every gate over that table green.
///
/// So the drop is asserted where it is OBSERVABLE: two policies differing ONLY in that key
/// project onto EQUAL mounts (`AdapterPolicy` is `PartialEq` over every field), which stops
/// holding the moment any field carries the value.
///
/// ⚠ RED is the gate doing its job, not a test to relax: folding this ceiling into `RiskLimits`
/// MOVES `crate::require_live_risk_budget`'s pre-connect refusal (its text names the profile's
/// `[risk]`; the module doc calls that "a decision, not plumbing"). Decide, state it, and update
/// `PRE_TRADE_CEILINGS` (its `refuses_live_mount_when_absent` column is gated against that
/// refusal's source) in the same PR.
#[test]
fn a_policy_per_order_ceiling_reaches_nothing_the_mount_carries() {
    let bare = |n| AdapterPolicy::from(&Policy { max_notional_per_order: n, ..Policy::default() });
    assert_eq!(
        bare(Some(1.0)),
        bare(Some(9_999_999.0)),
        "two policies differing ONLY in `max_notional_per_order` projected onto DIFFERENT \
             mounts — the value is now observable here, which means it is being carried"
    );
    assert_eq!(
        bare(Some(1.0)),
        AdapterPolicy::default(),
        "a policy whose only line is `max_notional_per_order` must project onto exactly the \
             no-file mount: this key arms nothing the venue mount applies"
    );

    // …and with every OTHER carried field distinctive, so the claim is not an artefact of two
    // defaults: a future field carrying the ceiling ALONGSIDE these is still caught.
    let loud = |n| {
        AdapterPolicy::from(&Policy {
            max_notional_per_order: n,
            max_account_exposure: Some(12_345.0),
            max_sizing_equity: Some(6_789.0),
            market_slippage: Some(0.002),
            halt_admit: HaltAdmit::Verify,
            ..Policy::default()
        })
    };
    assert_eq!(
        loud(None),
        loud(Some(42.0)),
        "with every carried ceiling armed, adding `max_notional_per_order` still must change \
             nothing the mount sees"
    );
    // The control: `loud` differs from `bare`, so the comparison above can fail for its reason.
    assert_ne!(loud(None), bare(None), "the `loud` fixture must not collapse to the default");
}

// ---- the account table the policy carries: the builder, the enumeration, the exposure figure ----

/// One `account` row, built by hand: the shapes `AdapterPolicy::with_account` cannot plant (an
/// inactive row, a `max_exposure`).
fn row(
    id: i64,
    venue: &str,
    tier: AccountTier,
    label: Option<&str>,
    active: bool,
    max_exposure: Option<f64>,
) -> vike_secrets::Account {
    vike_secrets::Account {
        id,
        venue: venue.to_string(),
        tier,
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: max_exposure.and_then(vike_secrets::MaxExposure::new),
        leverage: None,
    }
}

/// The builder: `with_account` turns an UNREAD directory into a KNOWN one, numbers its rows, and
/// keeps them `id`-ordered; `known_accounts` lists every labelled row of the venue, inactive ones
/// included, so an inactive labelled account shows as `AccountInactive` rather than vanishing.
#[test]
fn planted_rows_are_known_numbered_and_enumerated() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let old = AccountLabel::parse("OLD").expect("a legal label");
    let elsewhere = AccountLabel::parse("ELSEWHERE").expect("a legal label");
    let policy = AdapterPolicy::default()
        .with_account("bybit", &AccountLabel::Default, AccountTier::Demo)
        .with_account("bybit", &alt, AccountTier::Live)
        .with_account_row(row(7, "bybit", AccountTier::Demo, Some("OLD"), false, None))
        .with_account("okx", &elsewhere, AccountTier::Demo);
    let ids: Vec<i64> = match policy.accounts.rows() {
        Some(Ok(accounts)) => {
            accounts.known().expect("a known table").iter().map(|a| a.id).collect()
        }
        other => panic!("the builder plants a KNOWN table: {other:?}"),
    };
    assert_eq!(ids, vec![1, 2, 7, 8], "numbered above the highest, id-ordered");
    assert_eq!(
        vike_config::account_arming::known_accounts("bybit", Some(&policy.accounts)),
        vec![AccountLabel::Default, alt, old.clone()],
        "default first, then every labelled row of THIS venue, active or not"
    );
    assert_eq!(
        vike_config::account_arming::account_tier(Some(&policy.accounts), "bybit", &old),
        Err(vike_config::ArmingBlock::AccountInactive)
    );
}

/// **The per-account exposure figure**: the tightest `max_exposure` among the ACTIVE rows of
/// `(venue, label)`; an inactive row's figure is not read, and no figure is unbounded.
#[test]
fn the_account_exposure_figure_is_the_tightest_active_rows() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let policy = AdapterPolicy::default()
        .with_account_row(row(1, "bybit", AccountTier::Demo, None, true, Some(5_000.0)))
        .with_account_row(row(2, "bybit", AccountTier::Paper, None, true, Some(3_000.0)))
        .with_account_row(row(3, "bybit", AccountTier::Demo, None, false, Some(1.0)))
        .with_account_row(row(4, "bybit", AccountTier::Live, Some("ALT"), true, None));
    let of = |label| crate::arming::account_max_exposure(Some(&policy), "bybit", label);
    assert_eq!(of(&AccountLabel::Default), Some(3_000.0), "the tightest ACTIVE row");
    assert_eq!(of(&alt), None, "no figure on its rows: unbounded");
    assert_eq!(crate::arming::account_max_exposure(None, "bybit", &AccountLabel::Default), None);
}
