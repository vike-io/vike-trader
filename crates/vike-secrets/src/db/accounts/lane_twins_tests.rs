//! The lane-twin table and the rows it derives.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_table_opens_binance_at_demo_only_on_a_valid_perp_lane`] | a `live` tier, a second venue or a label the grammar refuses opened by an edit nobody meant |
//! | [`a_demo_default_row_that_states_a_leverage_derives_one_twin`] | the perp lane having no account to run under, or the twin not carrying the keys, the cap or the figure |
//! | [`a_row_that_states_no_leverage_derives_nothing`] | a spot-only account getting a second engine it never asked for |
//! | [`only_the_default_demo_row_of_a_listed_venue_derives`] | a live, paper, labelled or other-venue row growing a twin |
//! | [`an_inactive_parent_derives_an_inactive_twin`] | a deactivated account whose perp engine keeps running |
//! | [`a_real_row_with_the_twins_label_makes_it_stand_down`] | the node mounting an operator's own `PERP` account as a derived lane |
//! | [`the_view_stays_id_ordered_and_the_parent_comes_first`] | a consumer that reads "the lowest id" or "the first row" meeting the twin first |
//! | [`the_arming_questions_answer_for_two_accounts`] | the engine assembly and the gatewayhub's arming table asking the rows and hearing one account |
//! | [`the_twin_signs_with_the_parents_keys_and_the_stored_read_lists_one_account`] | a twin with no keys, or a listing that shows an account nobody created |

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use super::super::{AccountLeverage, ActiveTier, MaxExposure};
use super::*;
use crate::account_credentials::CredentialBook;
use crate::account_directory::AccountDirectory;

fn row(id: i64, venue: &str, tier: AccountTier, label: Option<&str>) -> Account {
    Account {
        id,
        venue: venue.to_string(),
        tier,
        label: label.map(str::to_string),
        venue_account_id: Some("book-1".to_string()),
        parent_id: None,
        active: true,
        last_verified_at: Some("2026-10-18T00:00:00Z".to_string()),
        max_exposure: MaxExposure::new(5000.0),
        leverage: None,
    }
}

fn lev(x: f64) -> Option<AccountLeverage> {
    AccountLeverage::new(x)
}

fn rows(a: Accounts) -> Vec<Account> {
    match a {
        Accounts::Known(rows) => rows,
        Accounts::Unanswerable(why) => panic!("unanswerable: {why:?}"),
    }
}

/// The account 3 of the 2026-10-18 drill: binance, demo, default label, a stated leverage.
fn account_three(leverage: Option<f64>) -> Account {
    Account { leverage: leverage.and_then(lev), ..row(3, "binance", AccountTier::Demo, None) }
}

#[test]
fn the_table_opens_binance_at_demo_only_on_a_valid_perp_lane() {
    assert_eq!(LANE_TWINS.len(), 1, "one venue has two lanes under one row today");
    let binance = lane_twin("binance").expect("listed");
    assert_eq!(binance.tiers, &[AccountTier::Demo], "a live tier is CLOSED by the table");
    assert!(binance.symbol.ends_with(".P"), "the twin trades the perpetual lane");
    assert!(
        AccountLabel::parse(binance.label).is_ok(),
        "the label is a label the grammar accepts, or no route key could carry it"
    );
    assert!(vike_model::VENUES.contains(&binance.venue));
    for other in ["bybit", "okx", "aster", "hyperliquid", "deribit", "oanda", ""] {
        assert!(lane_twin(other).is_none(), "{other:?} has no second lane under one row");
    }
    assert_eq!(lane_twin_symbol("binance", "PERP"), Some("BTCUSDT.P"));
    assert_eq!(lane_twin_symbol("binance", "ALT"), None, "only the table's label is the twin");
    assert_eq!(lane_twin_symbol("bybit", "PERP"), None);
    assert!(is_lane_twin_label("binance", "PERP") && !is_lane_twin_label("binance", "ALT"));
}

#[test]
fn a_demo_default_row_that_states_a_leverage_derives_one_twin() {
    let derived = rows(Accounts::Known(vec![account_three(Some(5.0))]).with_lane_twins());
    assert_eq!(derived.len(), 2);
    let (parent, twin) = (&derived[0], &derived[1]);
    // The parent is the row as stored, less the perpetual-only figure.
    assert_eq!(parent.label, None);
    assert_eq!(parent.leverage, None, "the spot engine is never asked to apply a futures setting");
    assert_eq!(parent.venue_account_id.as_deref(), Some("book-1"));
    // The twin signs with the parent's keys (same id), at the parent's tier, cap and activity.
    assert_eq!(twin.id, 3);
    assert_eq!((twin.venue.as_str(), twin.tier, twin.active), ("binance", AccountTier::Demo, true));
    assert_eq!(twin.label.as_deref(), Some("PERP"));
    assert_eq!(twin.leverage, lev(5.0));
    assert_eq!(twin.max_exposure, parent.max_exposure);
    // It is no book of its own and nothing handshook as it.
    assert_eq!((&twin.venue_account_id, &twin.last_verified_at), (&None, &None));
    assert_eq!(twin.parent_id, None);
}

#[test]
fn a_row_that_states_no_leverage_derives_nothing() {
    let stored = vec![
        account_three(None),
        row(4, "binance", AccountTier::Paper, None),
        row(5, "bybit", AccountTier::Demo, None),
    ];
    assert_eq!(rows(Accounts::Known(stored.clone()).with_lane_twins()), stored);
    assert_eq!(Accounts::Known(Vec::new()).with_lane_twins(), Accounts::Known(Vec::new()));
}

#[test]
fn only_the_default_demo_row_of_a_listed_venue_derives() {
    let with = |a: Account| Accounts::Known(vec![a]).with_lane_twins();
    let stated = |a: Account| Account { leverage: lev(3.0), ..a };
    // Live and paper tiers are not open; a labelled account is a book of its own; another venue has
    // no second lane. Each keeps its stated figure and gets no twin.
    for stored in [
        stated(row(3, "binance", AccountTier::Live, None)),
        stated(row(3, "binance", AccountTier::Paper, None)),
        stated(row(3, "binance", AccountTier::Demo, Some("ALT"))),
        stated(row(3, "bybit", AccountTier::Demo, None)),
        stated(row(3, "aster", AccountTier::Demo, None)),
    ] {
        assert_eq!(with(stored.clone()), Accounts::Known(vec![stored.clone()]), "{stored:?}");
    }
}

#[test]
fn an_inactive_parent_derives_an_inactive_twin() {
    let parent = Account { active: false, ..account_three(Some(2.0)) };
    let derived = rows(Accounts::Known(vec![parent]).with_lane_twins());
    assert_eq!(derived.len(), 2);
    assert!(!derived[0].active && !derived[1].active, "the account's switch stops both engines");
    let accounts = Accounts::Known(derived);
    assert_eq!(accounts.active_tier("binance", None), Some(ActiveTier::Inactive));
    assert_eq!(accounts.active_tier("binance", Some("PERP")), Some(ActiveTier::Inactive));
}

#[test]
fn a_real_row_with_the_twins_label_makes_it_stand_down() {
    // Any tier, either activity: the operator's own account under that label wins.
    for (tier, active) in [
        (AccountTier::Demo, true),
        (AccountTier::Demo, false),
        (AccountTier::Live, true),
        (AccountTier::Paper, true),
    ] {
        let real = Account { active, ..row(9, "binance", tier, Some("PERP")) };
        let stored = vec![account_three(Some(5.0)), real];
        assert_eq!(
            rows(Accounts::Known(stored.clone()).with_lane_twins()),
            stored,
            "{tier:?} active={active}: nothing derived, the parent keeps its figure (and the \
             refusal that goes with it)"
        );
    }
}

#[test]
fn the_view_stays_id_ordered_and_the_parent_comes_first() {
    let derived = rows(
        Accounts::Known(vec![
            row(1, "bybit", AccountTier::Demo, None),
            account_three(Some(5.0)),
            row(7, "okx", AccountTier::Demo, None),
        ])
        .with_lane_twins(),
    );
    let ids: Vec<(i64, Option<&str>)> =
        derived.iter().map(|r| (r.id, r.label.as_deref())).collect();
    assert_eq!(ids, [(1, None), (3, None), (3, Some("PERP")), (7, None)]);
}

#[test]
fn the_arming_questions_answer_for_two_accounts() {
    let accounts = Accounts::Known(vec![account_three(Some(5.0))]).with_lane_twins();
    assert_eq!(accounts.active_tier("binance", None), Some(ActiveTier::Tier(AccountTier::Demo)));
    assert_eq!(
        accounts.active_tier("binance", Some("PERP")),
        Some(ActiveTier::Tier(AccountTier::Demo))
    );
    assert_eq!(accounts.leverage_of("binance", None), None);
    assert_eq!(accounts.leverage_of("binance", Some("PERP")), lev(5.0));
    assert_eq!(accounts.max_exposure_of("binance", Some("PERP")), MaxExposure::new(5000.0));
    // Unlabelled rows of the venue: still exactly one.
    assert_eq!(accounts.active_for_venue("binance").map(|v| v.len()), Some(2));
}

/// **A derived row is MARKED, and the mark is the only way to tell it from an operator's own account.**
/// The list `derive_lane_twins` returns is what `AccountDirectory::is_derived_lane_twin` answers from:
/// the derived pair is in it, a row the table holds under the same label is not, and the stored read
/// derives (and so marks) nothing.
#[test]
fn a_derived_twin_is_marked_and_a_real_row_under_the_label_is_not() {
    let (derived_rows, marked) =
        Accounts::Known(vec![account_three(Some(5.0))]).derive_lane_twins();
    assert_eq!(marked, vec![("binance".to_string(), "PERP".to_string())]);
    assert_eq!(derived_rows.known().map(<[Account]>::len), Some(2));

    // Nothing stated, a closed tier and an unanswerable table: nothing marked.
    for none in [
        Accounts::Known(vec![account_three(None)]),
        Accounts::Known(vec![Account {
            leverage: lev(3.0),
            ..row(3, "binance", AccountTier::Live, None)
        }]),
    ] {
        assert!(none.derive_lane_twins().1.is_empty());
    }
    let unanswerable = Accounts::Unanswerable(crate::NoAccountTable::NoStore { db: "x".into() });
    assert!(unanswerable.derive_lane_twins().1.is_empty());

    // A real row under the label: the twin stands down, so the pair is NOT marked.
    let real = row(9, "binance", AccountTier::Demo, Some("PERP"));
    let stored = vec![account_three(Some(5.0)), real];
    assert!(Accounts::Known(stored.clone()).derive_lane_twins().1.is_empty());

    let read = |rows: Vec<Account>, derive: bool| {
        let args = (
            Ok::<_, String>(Accounts::Known(rows)),
            Ok::<_, String>(None),
            Ok::<_, String>(CredentialBook::new()),
        );
        if derive {
            AccountDirectory::read(args.0, args.1, args.2)
        } else {
            AccountDirectory::read_stored(args.0, args.1, args.2)
        }
    };
    assert!(read(vec![account_three(Some(5.0))], true).is_derived_lane_twin("binance", "PERP"));
    assert!(!read(vec![account_three(Some(5.0))], true).is_derived_lane_twin("binance", "ALT"));
    assert!(!read(vec![account_three(Some(5.0))], true).is_derived_lane_twin("bybit", "PERP"));
    assert!(!read(vec![account_three(Some(5.0))], false).is_derived_lane_twin("binance", "PERP"));
    assert!(
        !read(stored, true).is_derived_lane_twin("binance", "PERP"),
        "the operator's own PERP account is not a derived twin"
    );
}

/// **F5: a parent that does not run has no lane to run.** An active demo default row that states a
/// leverage beside an active LIVE default row is a tier conflict (decision 0156): the default account is
/// NOT RUNNING, and a twin carrying its keys on the demo hosts would trade an account that does not.
#[test]
fn a_tier_conflicted_default_account_derives_no_twin() {
    let live = row(4, "binance", AccountTier::Live, None);
    let stored = vec![account_three(Some(5.0)), live.clone()];
    let (derived, marked) = Accounts::Known(stored.clone()).derive_lane_twins();
    assert_eq!(derived, Accounts::Known(stored), "the rows stay as stored, the figure included");
    assert!(marked.is_empty());
    assert_eq!(derived.active_tier("binance", None), Some(ActiveTier::Conflict));
    assert_eq!(derived.active_tier("binance", Some("PERP")), Some(ActiveTier::NoRow));

    // The conflict is about ACTIVE rows: with the live one off, the demo row derives its twin again.
    let (derived, marked) =
        Accounts::Known(vec![account_three(Some(5.0)), Account { active: false, ..live }])
            .derive_lane_twins();
    assert_eq!(marked.len(), 1);
    assert_eq!(
        derived.active_tier("binance", Some("PERP")),
        Some(ActiveTier::Tier(AccountTier::Demo))
    );
}

#[test]
fn the_twin_signs_with_the_parents_keys_and_the_stored_read_lists_one_account() {
    let mut book = CredentialBook::new();
    book.insert_account(3, "API_KEY", "demo-key");
    book.insert_account(3, "API_SECRET", "demo-secret");
    let read = |stored: bool| {
        let args = (
            Ok::<_, String>(Accounts::Known(vec![account_three(Some(5.0))])),
            Ok::<_, String>(None),
            Ok::<_, String>(book.clone()),
        );
        if stored {
            AccountDirectory::read_stored(args.0, args.1, args.2)
        } else {
            AccountDirectory::read(args.0, args.1, args.2)
        }
    };
    let perp = AccountLabel::parse("PERP").expect("a label");

    let derived = read(false);
    let spot = derived.credentials_for("binance", &AccountLabel::Default, AccountTier::Demo);
    let twin = derived.credentials_for("binance", &perp, AccountTier::Demo);
    assert_eq!(spot.get("API_KEY"), Some("demo-key"));
    assert_eq!(twin.get("API_KEY"), Some("demo-key"));
    assert_eq!(twin.get("API_SECRET"), Some("demo-secret"));
    assert_eq!(twin.account_id(), Some(3), "one account id behind both engines");
    assert_eq!(twin.fingerprint(), spot.fingerprint(), "a rotated key moves both engines");
    // The twin is a demo account's: no tier but the row's.
    assert!(!derived.credentials_for("binance", &perp, AccountTier::Live).holds_any());

    // The stored read is the table as it is: one account, no twin, the figure where it was written.
    let stored = read(true);
    assert!(!stored.credentials_for("binance", &perp, AccountTier::Demo).holds_any());
    let stored_rows = stored.rows().and_then(Result::ok).and_then(Accounts::known).expect("rows");
    assert_eq!(stored_rows.len(), 1);
    assert_eq!(stored_rows[0].leverage, lev(5.0));
}
