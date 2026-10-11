//! `policy`'s unit tests: each settings row reaches `MountPolicy`'s projection, or none; and the
//! account table it carries answers each account's tier (`crate::arming`'s `account_tier`).

use super::*;

/// `policy.halt_admit` row → `Policy` → `MountPolicy` → the mode `make_engine` puts in force,
/// landing DIFFERENTLY per venue (why it is carried, not assumed everywhere).
#[test]
fn a_configured_verify_reaches_the_one_venue_that_can_honour_it_and_degrades_elsewhere() {
    let policy = Policy { halt_admit: HaltAdmit::Verify, ..Policy::default() };
    let mount = MountPolicy::from(&policy);
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
    assert_eq!(MountPolicy::default().market_slippage, None);
    assert_eq!(MountPolicy::default().halt_admit, HaltAdmit::Admit);
    assert_eq!(
        crate::arming::account_tier(Some(&MountPolicy::default()), "bybit", &AccountLabel::Default),
        (VenueMode::Paper, vike_config::ArmingBlock::NoAccountRow),
        "no account table read: every account is paper"
    );
    assert_eq!(MountPolicy::from(&Policy::default()), MountPolicy::default());
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
/// project onto EQUAL mounts (`MountPolicy` is `PartialEq` over every field), which stops
/// holding the moment any field carries the value.
///
/// ⚠ RED is the gate doing its job, not a test to relax: folding this ceiling into `RiskLimits`
/// MOVES `crate::require_live_risk_budget`'s pre-connect refusal (its text names the profile's
/// `[risk]`; the module doc calls that "a decision, not plumbing"). Decide, state it, and update
/// `PRE_TRADE_CEILINGS` (its `refuses_live_mount_when_absent` column is gated against that
/// refusal's source) in the same PR.
#[test]
fn a_policy_per_order_ceiling_reaches_nothing_the_mount_carries() {
    let bare = |n| MountPolicy::from(&Policy { max_notional_per_order: n, ..Policy::default() });
    assert_eq!(
        bare(Some(1.0)),
        bare(Some(9_999_999.0)),
        "two policies differing ONLY in `max_notional_per_order` projected onto DIFFERENT \
             mounts — the value is now observable here, which means it is being carried"
    );
    assert_eq!(
        bare(Some(1.0)),
        MountPolicy::default(),
        "a policy whose only line is `max_notional_per_order` must project onto exactly the \
             no-file mount: this key arms nothing the venue mount applies"
    );

    // …and with every OTHER carried field distinctive, so the claim is not an artefact of two
    // defaults: a future field carrying the ceiling ALONGSIDE these is still caught.
    let loud = |n| {
        MountPolicy::from(&Policy {
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

// ---- the account table: each account's tier, `crate::arming`'s `account_tier` ----

/// One `account` row, built by hand: the shapes `MountPolicy::with_account` cannot plant (an
/// inactive row, a `max_exposure`).
fn row(
    id: i64,
    venue: &str,
    tier: VenueMode,
    label: Option<&str>,
    active: bool,
    max_exposure: Option<f64>,
) -> vike_secrets::Account {
    vike_secrets::Account {
        id,
        venue: venue.to_string(),
        tier: tier.as_str().to_string(),
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: max_exposure.and_then(vike_secrets::MaxExposure::new),
    }
}

/// `account_tier` for the DEFAULT account of `venue`.
fn tier_of(policy: Option<&MountPolicy>, venue: &str) -> (VenueMode, vike_config::ArmingBlock) {
    crate::arming::account_tier(policy, venue, &AccountLabel::Default)
}

/// **Every answer the account table can give, one account at a time.** PAPER is the default of
/// every state but one: exactly one non-paper tier among the ACTIVE rows of `(venue, label)`.
#[test]
fn the_account_table_answers_each_accounts_tier() {
    use vike_config::ArmingBlock as Block;
    let paper = |block| (VenueMode::Paper, block);
    let default = AccountLabel::Default;

    // Nobody asked: no policy, an UNREAD table, a box with no store.
    assert_eq!(tier_of(None, "bybit"), paper(Block::NoAccountRow), "no policy");
    assert_eq!(tier_of(Some(&MountPolicy::default()), "bybit"), paper(Block::NoAccountRow));
    let no_store = MountPolicy {
        accounts: AccountDirectory::from_rows(
            vike_secrets::Accounts::Unanswerable(vike_secrets::NoAccountTable::NoStore {
                db: std::path::PathBuf::from("planted/vike.db"),
            }),
            None,
        ),
        ..MountPolicy::default()
    };
    assert_eq!(tier_of(Some(&no_store), "bybit"), paper(Block::NoAccountRow), "no store");

    // A store that EXISTS and would not open: loud at the root, paper here, its own block.
    type Keys = Option<std::collections::BTreeMap<i64, vike_secrets::AccountKeys>>;
    let unreadable = MountPolicy {
        accounts: AccountDirectory::read(
            Err::<vike_secrets::Accounts, &str>("database is locked"),
            Err::<Keys, &str>("database is locked"),
        ),
        ..MountPolicy::default()
    };
    assert_eq!(tier_of(Some(&unreadable), "bybit"), paper(Block::AccountNotInStore));

    // The table answered.
    let other_venue = MountPolicy::default().with_account("okx", &default, VenueMode::Live);
    assert_eq!(tier_of(Some(&other_venue), "bybit"), paper(Block::NoAccountRow), "no row");
    let inactive = MountPolicy::default().with_account_row(row(
        1,
        "bybit",
        VenueMode::Live,
        None,
        false,
        None,
    ));
    assert_eq!(tier_of(Some(&inactive), "bybit"), paper(Block::AccountInactive));
    let paper_only = MountPolicy::default().with_account("bybit", &default, VenueMode::Paper);
    assert_eq!(tier_of(Some(&paper_only), "bybit"), paper(Block::PaperTier));
    let demo_and_paper = paper_only.clone().with_account("bybit", &default, VenueMode::Demo);
    assert_eq!(
        tier_of(Some(&demo_and_paper), "bybit"),
        (VenueMode::Demo, Block::None),
        "a paper row never conflicts"
    );
    let two_demo = MountPolicy::default()
        .with_account("bybit", &default, VenueMode::Demo)
        .with_account("bybit", &default, VenueMode::Demo);
    assert_eq!(
        tier_of(Some(&two_demo), "bybit"),
        (VenueMode::Demo, Block::None),
        "two rows of ONE tier (dukascopy's two demo books)"
    );
    let demo_and_live = MountPolicy::default()
        .with_account("bybit", &default, VenueMode::Demo)
        .with_account("bybit", &default, VenueMode::Live);
    assert_eq!(tier_of(Some(&demo_and_live), "bybit"), paper(Block::TierConflict), "no pick");
    let live_beside_inactive_demo = MountPolicy::default()
        .with_account_row(row(1, "bybit", VenueMode::Demo, None, false, None))
        .with_account("bybit", &default, VenueMode::Live);
    assert_eq!(
        tier_of(Some(&live_beside_inactive_demo), "bybit"),
        (VenueMode::Live, Block::None),
        "an INACTIVE row never conflicts — deactivating one is the remedy"
    );
}

/// A LABELLED row answers for its label only: it arms nothing for the DEFAULT account, and the
/// DEFAULT account's rows arm nothing for it.
#[test]
fn a_labelled_row_answers_for_its_own_label_only() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let labelled = MountPolicy::default().with_account("bybit", &alt, VenueMode::Live);
    assert_eq!(
        crate::arming::account_tier(Some(&labelled), "bybit", &alt),
        (VenueMode::Live, vike_config::ArmingBlock::None)
    );
    assert_eq!(
        tier_of(Some(&labelled), "bybit"),
        (VenueMode::Paper, vike_config::ArmingBlock::NoAccountRow),
        "a labelled row is not the default account's"
    );
    let default_only =
        MountPolicy::default().with_account("bybit", &AccountLabel::Default, VenueMode::Live);
    assert_eq!(
        crate::arming::account_tier(Some(&default_only), "bybit", &alt),
        (VenueMode::Paper, vike_config::ArmingBlock::NoAccountRow),
        "the default account's row is not a labelled one's"
    );
    assert_eq!(crate::arming::venue_tier(Some(&default_only), "bybit"), VenueMode::Live);
}

/// The builder: `with_account` turns an UNREAD directory into a KNOWN one, numbers its rows, and
/// keeps them `id`-ordered; `known_accounts` lists every labelled row of the venue, inactive ones
/// included, so an inactive labelled account shows as `AccountInactive` rather than vanishing.
#[test]
fn planted_rows_are_known_numbered_and_enumerated() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let old = AccountLabel::parse("OLD").expect("a legal label");
    let elsewhere = AccountLabel::parse("ELSEWHERE").expect("a legal label");
    let policy = MountPolicy::default()
        .with_account("bybit", &AccountLabel::Default, VenueMode::Demo)
        .with_account("bybit", &alt, VenueMode::Live)
        .with_account_row(row(7, "bybit", VenueMode::Demo, Some("OLD"), false, None))
        .with_account("okx", &elsewhere, VenueMode::Demo);
    let ids: Vec<i64> = match policy.accounts.rows() {
        Some(Ok(accounts)) => {
            accounts.known().expect("a known table").iter().map(|a| a.id).collect()
        }
        other => panic!("the builder plants a KNOWN table: {other:?}"),
    };
    assert_eq!(ids, vec![1, 2, 7, 8], "numbered above the highest, id-ordered");
    assert_eq!(
        crate::known_accounts("bybit", &std::collections::HashMap::new(), Some(&policy)),
        vec![AccountLabel::Default, alt, old.clone()],
        "default first, then every labelled row of THIS venue, active or not"
    );
    assert_eq!(
        crate::arming::account_tier(Some(&policy), "bybit", &old),
        (VenueMode::Paper, vike_config::ArmingBlock::AccountInactive)
    );
}

/// **The per-account exposure figure**: the tightest `max_exposure` among the ACTIVE rows of
/// `(venue, label)`; an inactive row's figure is not read, and no figure is unbounded.
#[test]
fn the_account_exposure_figure_is_the_tightest_active_rows() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let policy = MountPolicy::default()
        .with_account_row(row(1, "bybit", VenueMode::Demo, None, true, Some(5_000.0)))
        .with_account_row(row(2, "bybit", VenueMode::Paper, None, true, Some(3_000.0)))
        .with_account_row(row(3, "bybit", VenueMode::Demo, None, false, Some(1.0)))
        .with_account_row(row(4, "bybit", VenueMode::Live, Some("ALT"), true, None));
    let of = |label| crate::arming::account_max_exposure(Some(&policy), "bybit", label);
    assert_eq!(of(&AccountLabel::Default), Some(3_000.0), "the tightest ACTIVE row");
    assert_eq!(of(&alt), None, "no figure on its rows: unbounded");
    assert_eq!(crate::arming::account_max_exposure(None, "bybit", &AccountLabel::Default), None);
}
