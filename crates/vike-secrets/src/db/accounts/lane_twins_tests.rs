//! The lane-twin table and the rows it derives.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_table_opens_binance_at_demo_and_live_on_a_valid_perp_lane`] | a paper tier, a second venue or a label the grammar refuses opened by an edit nobody meant; the owner's `live` opening lost |
//! | [`a_demo_default_row_that_states_a_leverage_derives_one_twin`] | the perp lane having no account to run under, or the twin not carrying the keys, the cap or the figure |
//! | [`a_live_default_row_that_states_a_leverage_derives_one_live_twin`] | the mainnet twin carrying another tier, another account's keys, a looser cap or no figure |
//! | [`a_row_that_states_no_leverage_derives_nothing`] | a spot-only account getting a second engine it never asked for |
//! | [`a_live_row_that_states_no_leverage_is_byte_identical_to_before`] | the opening of `live` growing a twin (a second real-money engine) nobody asked for |
//! | [`only_the_default_demo_or_live_row_of_a_listed_venue_derives`] | a paper, labelled or other-venue row growing a twin |
//! | [`a_demo_sibling_and_a_live_parent_never_collide_on_the_twins_label`] | the demo twin's inactive row, or its keys, answering for the live twin (or the reverse) |
//! | [`two_active_tiers_derive_no_twin_of_either`] | a twin of a parent that does not run (the demo and live rows both active) |
//! | [`a_real_row_with_the_twins_label_makes_a_live_twin_stand_down_too`] | the node mounting an operator's own `PERP` account as the mainnet lane |
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
fn the_table_opens_binance_at_demo_and_live_on_a_valid_perp_lane() {
    assert_eq!(LANE_TWINS.len(), 1, "one venue has two lanes under one row today");
    let binance = lane_twin("binance").expect("listed");
    assert_eq!(
        binance.tiers,
        &[AccountTier::Demo, AccountTier::Live],
        "demo, and live (the owner's ruling, 2026-10-19); a paper row has no venue to run a lane on"
    );
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

/// The account 4 of the owner's live opening: binance, LIVE, default label, a stated leverage.
fn live_account(leverage: Option<f64>) -> Account {
    Account { leverage: leverage.and_then(lev), ..row(4, "binance", AccountTier::Live, None) }
}

/// **The owner opened `live` for the twin (2026-10-19)**: a live default row that STATES a leverage derives
/// ONE twin, at `live`, with the parent's id (so its keys), activity, exposure ceiling and figure; the
/// parent loses the figure in the view exactly as a demo parent does.
#[test]
fn a_live_default_row_that_states_a_leverage_derives_one_live_twin() {
    let (derived, marked) = Accounts::Known(vec![live_account(Some(3.0))]).derive_lane_twins();
    assert_eq!(marked, vec![("binance".to_string(), "PERP".to_string())]);
    let derived = rows(derived);
    assert_eq!(derived.len(), 2);
    let (parent, twin) = (&derived[0], &derived[1]);
    assert_eq!(
        (parent.label.as_deref(), parent.tier, parent.leverage),
        (None, AccountTier::Live, None)
    );
    // The twin signs with the LIVE parent's keys (same id), at LIVE, with the parent's cap and activity.
    assert_eq!(twin.id, 4);
    assert_eq!((twin.venue.as_str(), twin.tier, twin.active), ("binance", AccountTier::Live, true));
    assert_eq!(twin.label.as_deref(), Some("PERP"));
    assert_eq!(twin.leverage, lev(3.0));
    assert_eq!(
        twin.max_exposure, parent.max_exposure,
        "the twin never gets a looser exposure ceiling than its parent"
    );
    assert_eq!(twin.max_exposure, MaxExposure::new(5000.0));
    assert_eq!((&twin.venue_account_id, &twin.last_verified_at), (&None, &None));

    // The arming questions: both engines are LIVE accounts; the figure belongs to the twin alone.
    let accounts = Accounts::Known(derived);
    assert_eq!(accounts.active_tier("binance", None), Some(ActiveTier::Tier(AccountTier::Live)));
    assert_eq!(
        accounts.active_tier("binance", Some("PERP")),
        Some(ActiveTier::Tier(AccountTier::Live))
    );
    assert_eq!(accounts.leverage_of("binance", None), None);
    assert_eq!(accounts.leverage_of("binance", Some("PERP")), lev(3.0));
    assert_eq!(accounts.max_exposure_of("binance", Some("PERP")), MaxExposure::new(5000.0));

    // A deactivated live parent derives a deactivated twin: the account's switch stops both engines.
    let off = rows(
        Accounts::Known(vec![Account { active: false, ..live_account(Some(3.0)) }])
            .with_lane_twins(),
    );
    assert!(off.iter().all(|r| !r.active));
}

/// **(a) A live row that states no leverage is untouched**: no twin, no marker, the rows as stored. The
/// capability opens; the owner states the figure later, and nothing here ever does.
#[test]
fn a_live_row_that_states_no_leverage_is_byte_identical_to_before() {
    for stored in [
        vec![live_account(None)],
        vec![live_account(None), row(5, "binance", AccountTier::Paper, None)],
        // (id-ordered, as the store returns them: the derived view is id-ordered too)
        vec![account_three(None), live_account(None)],
    ] {
        let (derived, marked) = Accounts::Known(stored.clone()).derive_lane_twins();
        assert_eq!(derived, Accounts::Known(stored.clone()), "{stored:?}");
        assert!(marked.is_empty(), "{stored:?}");
    }
    // A demo sibling that states a figure beside a live parent that does not: the live account grew
    // nothing. (Were the demo row ACTIVE too it would be a tier conflict, which derives nothing.)
    let demo_sibling =
        vec![Account { active: false, ..account_three(Some(2.0)) }, live_account(None)];
    let (derived, _) = Accounts::Known(demo_sibling).derive_lane_twins();
    assert_eq!(
        derived.active_tier("binance", Some("PERP")),
        Some(ActiveTier::Inactive),
        "an inactive demo twin only: the live parent grew nothing"
    );
    assert_eq!(derived.leverage_of("binance", Some("PERP")), None);
    assert_eq!(derived.active_tier("binance", None), Some(ActiveTier::Tier(AccountTier::Live)));
}

#[test]
fn only_the_default_demo_or_live_row_of_a_listed_venue_derives() {
    let with = |a: Account| Accounts::Known(vec![a]).with_lane_twins();
    let stated = |a: Account| Account { leverage: lev(3.0), ..a };
    // A paper tier is not open; a labelled account is a book of its own; another venue has no second
    // lane (bybit and okx stay demo-only for the leverage itself, aster closed). Each keeps its stated
    // figure and gets no twin. (A labelled LIVE binance row also gets no twin, but the view no longer keeps
    // its figure: the live opening is for the twin only, `a_live_figure_is_honoured_for_the_twin_only`.)
    for stored in [
        stated(row(3, "binance", AccountTier::Paper, None)),
        stated(row(3, "binance", AccountTier::Demo, Some("ALT"))),
        stated(row(3, "bybit", AccountTier::Demo, None)),
        stated(row(3, "bybit", AccountTier::Live, None)),
        stated(row(3, "okx", AccountTier::Live, None)),
        stated(row(3, "aster", AccountTier::Demo, None)),
        stated(row(3, "aster", AccountTier::Live, None)),
    ] {
        assert_eq!(with(stored.clone()), Accounts::Known(vec![stored.clone()]), "{stored:?}");
    }
    let labelled_live = stated(row(3, "binance", AccountTier::Live, Some("ALT")));
    let view = rows(with(labelled_live.clone()));
    assert_eq!(view, vec![Account { leverage: None, ..labelled_live }], "no twin, no figure");
    // …while both open tiers of the listed venue do.
    for tier in [AccountTier::Demo, AccountTier::Live] {
        let derived = rows(with(stated(row(3, "binance", tier, None))));
        assert_eq!(derived.len(), 2, "{tier:?}");
        assert_eq!((derived[1].tier, derived[1].label.as_deref()), (tier, Some("PERP")));
    }
}

/// **(d) A demo sibling and a live parent never collide on `binance#PERP`.** The store keeps one default
/// row per `(venue, tier)`, so the demo and the live parent are two rows; the twins derived from them are
/// two rows of one label at two tiers, exactly the shape one account's demo and live rows have, and the
/// ACTIVE row decides everything: the tier, the figure, the ceiling and the KEYS. The inactive sibling's
/// twin is inactive and answers nothing.
#[test]
fn a_demo_sibling_and_a_live_parent_never_collide_on_the_twins_label() {
    let perp = AccountLabel::parse("PERP").expect("a label");
    let mut book = CredentialBook::new();
    for (id, key) in [(3, "demo"), (4, "live")] {
        book.insert_account(id, "API_KEY", format!("{key}-key"));
        book.insert_account(id, "API_SECRET", format!("{key}-secret"));
    }
    let directory = |stored: Vec<Account>| {
        AccountDirectory::read(
            Ok::<_, String>(Accounts::Known(stored)),
            Ok::<_, String>(None),
            Ok::<_, String>(book.clone()),
        )
    };

    // The owner switched to live: the demo default row is inactive and still states 5, the live one is
    // active and states 2.
    let demo_off = Account { active: false, ..account_three(Some(5.0)) };
    let dir = directory(vec![demo_off, live_account(Some(2.0))]);
    assert!(dir.is_derived_lane_twin("binance", "PERP"));
    let derived = dir.rows().and_then(Result::ok).expect("rows");
    assert_eq!(
        derived.active_tier("binance", Some("PERP")),
        Some(ActiveTier::Tier(AccountTier::Live))
    );
    assert_eq!(derived.active_tier("binance", None), Some(ActiveTier::Tier(AccountTier::Live)));
    assert_eq!(
        derived.leverage_of("binance", Some("PERP")),
        lev(2.0),
        "the live figure, never the inactive demo sibling's 5"
    );
    let actives: Vec<(i64, AccountTier)> = derived
        .known()
        .expect("rows")
        .iter()
        .filter(|r| r.active && r.label.as_deref() == Some("PERP"))
        .map(|r| (r.id, r.tier))
        .collect();
    assert_eq!(actives, [(4, AccountTier::Live)], "exactly ONE active twin of the label");
    // The live twin signs with the LIVE parent's keys; at `demo` the label answers nothing (its demo row
    // is inactive), so the demo sibling's keys cannot be reached through the live twin's name.
    let live = dir.credentials_for("binance", &perp, AccountTier::Live);
    assert_eq!(live.get("API_KEY"), Some("live-key"));
    assert_eq!(live.account_id(), Some(4));
    assert!(!dir.credentials_for("binance", &perp, AccountTier::Demo).holds_any());
    // …and the default (spot) engine is the live parent's too.
    let spot = dir.credentials_for("binance", &AccountLabel::Default, AccountTier::Live);
    assert_eq!(spot.account_id(), Some(4));

    // The mirror: the demo parent active, the live one off (and stating a figure of its own).
    let live_off = Account { active: false, ..live_account(Some(2.0)) };
    let dir = directory(vec![account_three(Some(5.0)), live_off]);
    let derived = dir.rows().and_then(Result::ok).expect("rows");
    assert_eq!(
        derived.active_tier("binance", Some("PERP")),
        Some(ActiveTier::Tier(AccountTier::Demo))
    );
    assert_eq!(derived.leverage_of("binance", Some("PERP")), lev(5.0));
    let demo = dir.credentials_for("binance", &perp, AccountTier::Demo);
    assert_eq!((demo.get("API_KEY"), demo.account_id()), (Some("demo-key"), Some(3)));
    assert!(
        !dir.credentials_for("binance", &perp, AccountTier::Live).holds_any(),
        "an inactive live twin answers nothing: the demo twin never signs as it"
    );
}

/// **Two ACTIVE tiers are a conflict and derive no twin of either**: the default account does not run
/// (decision 0156), so a twin carrying its keys on either network would trade an account that does not.
/// Both rows keep their figure as stored; the conflict is the parent's refusal, in its own words.
#[test]
fn two_active_tiers_derive_no_twin_of_either() {
    let stored = vec![account_three(Some(5.0)), live_account(Some(2.0))];
    let (derived, marked) = Accounts::Known(stored.clone()).derive_lane_twins();
    // The demo parent keeps its figure; the LIVE one's is dropped from the view (no twin, so nothing at live
    // takes it: the live opening is for the twin only).
    let mut expected = stored;
    expected[1].leverage = None;
    assert_eq!(derived, Accounts::Known(expected));
    assert!(marked.is_empty());
    assert_eq!(derived.active_tier("binance", None), Some(ActiveTier::Conflict));
    assert_eq!(derived.active_tier("binance", Some("PERP")), Some(ActiveTier::NoRow));
}

/// A real account row that spelled the label wins over a derived MAINNET twin as it does over a demo one:
/// the live parent keeps its figure and derives nothing, whatever the real row's tier or activity.
#[test]
fn a_real_row_with_the_twins_label_makes_a_live_twin_stand_down_too() {
    for (tier, active) in [
        (AccountTier::Demo, true),
        (AccountTier::Demo, false),
        (AccountTier::Live, true),
        (AccountTier::Live, false),
        (AccountTier::Paper, true),
    ] {
        let real = Account { active, ..row(9, "binance", tier, Some("PERP")) };
        let stored = vec![live_account(Some(3.0)), real];
        let (derived, marked) = Accounts::Known(stored.clone()).derive_lane_twins();
        // No twin. The LIVE parent's figure is dropped from the view (the owner opened the live twin only, and
        // there is none), nothing else moves.
        let mut expected = stored;
        expected[0].leverage = None;
        assert_eq!(derived, Accounts::Known(expected), "{tier:?} active={active}");
        assert!(marked.is_empty(), "{tier:?} active={active}");
    }
}

/// **At `live` only the TWIN's figure is honoured** (re-review): a stated leverage on a LIVE row that is not
/// the one the twin is derived from (a labelled account, or the default one when the twin stood down) is
/// dropped from the view and returned as ignored; the demo tier and every figure that feeds a twin are
/// untouched.
#[test]
fn a_live_figure_is_honoured_for_the_twin_only() {
    let live = |id, label: Option<&str>, x: Option<f64>| Account {
        leverage: x.and_then(lev),
        ..row(id, "binance", AccountTier::Live, label)
    };
    let demo = |id, label: Option<&str>, x: f64| Account {
        leverage: lev(x),
        ..row(id, "binance", AccountTier::Demo, label)
    };

    // The default live row feeds the twin: nothing is ignored, the twin carries the figure.
    let (view, marked, ignored) =
        Accounts::Known(vec![live(1, None, Some(5.0))]).derive_lane_twins_ignoring();
    assert_eq!(marked, [("binance".to_string(), "PERP".to_string())]);
    assert!(ignored.is_empty(), "{ignored:?}");
    assert_eq!(view.leverage_of("binance", Some("PERP")), lev(5.0));

    // A LABELLED live row's figure is ignored (and the default one still feeds the twin); a labelled DEMO row's
    // and the default demo row's are untouched.
    let (view, _, ignored) = Accounts::Known(vec![
        live(1, None, Some(5.0)),
        live(2, Some("ALT"), Some(7.0)),
        demo(3, Some("ALT"), 4.0),
    ])
    .derive_lane_twins_ignoring();
    assert_eq!(
        ignored,
        [IgnoredLiveLeverage {
            id: 2,
            venue: "binance".into(),
            label: Some("ALT".into()),
            figure: 7.0
        }]
    );
    assert_eq!(view.leverage_of("binance", Some("PERP")), lev(5.0), "the twin keeps its figure");
    let live_alt = rows(view.clone()).into_iter().find(|r| r.id == 2).expect("kept as a row");
    assert_eq!(live_alt.leverage, None, "…but the labelled live row reads as nothing stated");
    assert_eq!(
        rows(view).into_iter().find(|r| r.id == 3).expect("demo").leverage,
        lev(4.0),
        "a labelled DEMO row is untouched"
    );

    // The twin stood down (a real live `PERP` row): the default live row's figure is ignored too.
    let (view, marked, ignored) =
        Accounts::Known(vec![live(1, None, Some(5.0)), live(9, Some("PERP"), Some(2.0))])
            .derive_lane_twins_ignoring();
    assert!(marked.is_empty());
    assert_eq!(ignored.iter().map(|i| i.id).collect::<Vec<_>>(), [1, 9]);
    assert_eq!(view.leverage_of("binance", None), None);
    assert_eq!(view.leverage_of("binance", Some("PERP")), None);

    // Another venue is not a lane-twin venue: bybit's table is closed at live and its rows are not rewritten.
    let bybit = Account { leverage: lev(3.0), ..row(4, "bybit", AccountTier::Live, None) };
    let (view, _, ignored) = Accounts::Known(vec![bybit]).derive_lane_twins_ignoring();
    assert!(ignored.is_empty());
    assert_eq!(view.leverage_of("bybit", None), lev(3.0));
}

/// **`set-leverage` refuses a labelled LIVE row of a lane-twin venue, in words** and takes what it took:
/// the default live row (the twin's parent), any demo row, and nothing a closed venue or a fraction would be.
#[test]
fn the_verb_refuses_a_labelled_live_row_and_takes_the_twins_parent() {
    use super::super::row_leverage_refusal as refusal;
    let (live, demo) = (AccountTier::Live, AccountTier::Demo);
    assert_eq!(refusal("binance", live, None, 5.0), None, "the default live row feeds the twin");
    assert_eq!(
        refusal("binance", demo, Some("ALT"), 5.0),
        None,
        "a labelled demo row is as before"
    );
    assert_eq!(refusal("binance", demo, None, 5.0), None);
    let why = refusal("binance", live, Some("ALT"), 5.0).expect("a labelled live row is refused");
    for want in ["DEFAULT", "binance#PERP", "`ALT`", "ignored"] {
        assert!(why.contains(want), "{want:?} missing: {why}");
    }
    assert!(refusal("bybit", live, None, 5.0).is_some(), "bybit stays closed at live");
    assert!(refusal("binance", live, None, 2.5).is_some(), "a fraction is refused as before");
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

    // Nothing stated, a closed tier (paper) and an unanswerable table: nothing marked.
    for none in [
        Accounts::Known(vec![account_three(None)]),
        Accounts::Known(vec![live_account(None)]),
        Accounts::Known(vec![Account {
            leverage: lev(3.0),
            ..row(3, "binance", AccountTier::Paper, None)
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
