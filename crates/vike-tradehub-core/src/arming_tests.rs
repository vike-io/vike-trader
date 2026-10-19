//! The arming table's readers: the startup preflight's stop laid over a row (drift register R36),
//! the live-intent predicate, the lock set and the shared-book report — all over HAND-BUILT tables,
//! so the readers are judged apart from any venue.

use std::collections::BTreeMap;

use vike_config::{ArmingBlock, VenueArming};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use super::*;
use crate::{VenueFacts, armed_live_venues, symbol_grid::GridSource};

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

fn row(
    venue: &'static str,
    label: AccountLabel,
    tier: Option<AccountTier>,
    effective: Option<AccountTier>,
    block: ArmingBlock,
    book: Option<&str>,
) -> ArmingRow {
    ArmingRow {
        arming: VenueArming { venue, label, tier, effective, block, account_ids: vec![1] },
        book: book.map(str::to_string),
    }
}

fn present() -> VenueFacts {
    VenueFacts {
        adapter_present: true,
        addresses_accounts: true,
        takes_recon_trigger: false,
        grid_source: GridSource::NoGrid,
        process_exclusive: None,
        clock_decl_word: "not-wired",
    }
}

fn table(rows: Vec<ArmingRow>) -> ArmingTable {
    let facts: BTreeMap<String, VenueFacts> =
        rows.iter().map(|r| (r.arming.venue.to_string(), present())).collect();
    ArmingTable::new(facts, rows)
}

/// A policy that names `venues` as hard-FAILED by the startup preflight.
fn failed(venues: &[&str]) -> AdapterPolicy {
    AdapterPolicy {
        preflight_failed: venues.iter().map(|v| (*v).to_string()).collect(),
        ..AdapterPolicy::default()
    }
}

fn answer(rows: &[VenueArming]) -> Vec<(Option<AccountTier>, ArmingBlock)> {
    rows.iter().map(|r| (r.effective, r.block)).collect()
}

/// **R36, case 1: the stop sits ABOVE the one-sidecar override.** The table's block already
/// includes `SidecarHeldElsewhere` (the backend applied the override); a venue the preflight
/// hard-FAILED reads `PreflightFailed` for that account, not the sidecar sentence — the order
/// `account_arming_under` always had (stop, then override).
#[test]
fn the_preflight_stop_is_above_the_sidecar_override() {
    let table = table(vec![
        row(
            "dukascopy",
            AccountLabel::Default,
            Some(AccountTier::Demo),
            None,
            ArmingBlock::SidecarHeldElsewhere,
            None,
        ),
        row(
            "dukascopy",
            alt(),
            Some(AccountTier::Demo),
            Some(AccountTier::Demo),
            ArmingBlock::None,
            None,
        ),
    ]);
    let policy = failed(&["dukascopy"]);
    assert_eq!(
        answer(&venue_account_arming(&table, "dukascopy", Some(&policy))),
        vec![(None, ArmingBlock::PreflightFailed), (None, ArmingBlock::PreflightFailed),],
        "both accounts of a failed venue stop with the preflight's word"
    );
    // CONTROL: with no stop the table's own answers stand.
    assert_eq!(
        answer(&venue_account_arming(&table, "dukascopy", Some(&AdapterPolicy::default()))),
        vec![
            (None, ArmingBlock::SidecarHeldElsewhere),
            (Some(AccountTier::Demo), ArmingBlock::None),
        ]
    );
    assert_eq!(
        answer(&venue_account_arming(&table, "dukascopy", None)),
        answer(&venue_account_arming(&table, "dukascopy", Some(&AdapterPolicy::default()))),
        "no policy is no stop"
    );
}

/// **R36, case 2: a label the venue cannot address stays `NoAccountSupport`** — the label
/// precondition answers before the stop, as it answers before the tier.
#[test]
fn the_label_precondition_is_above_the_preflight_stop() {
    let table = table(vec![row("bybit", alt(), None, None, ArmingBlock::NoAccountSupport, None)]);
    let rows = venue_account_arming(&table, "bybit", Some(&failed(&["bybit"])));
    assert_eq!(answer(&rows), vec![(None, ArmingBlock::NoAccountSupport)]);
}

/// **R36, case 3: a `paper` account runs whatever the preflight said** — the stop is for the
/// `demo` and `live` accounts of the venue; and an account the table itself does not run keeps its
/// own cause (it has no tier to stop).
#[test]
fn a_paper_account_and_a_table_refusal_are_not_stopped() {
    let table = table(vec![
        row(
            "bybit",
            AccountLabel::Default,
            Some(AccountTier::Paper),
            Some(AccountTier::Paper),
            ArmingBlock::PaperTier,
            None,
        ),
        row("bybit", alt(), None, None, ArmingBlock::TierConflict, None),
    ]);
    let rows = venue_account_arming(&table, "bybit", Some(&failed(&["bybit"])));
    assert_eq!(
        answer(&rows),
        vec![(Some(AccountTier::Paper), ArmingBlock::PaperTier), (None, ArmingBlock::TierConflict)]
    );
}

/// The stop is per VENUE: another venue's accounts, armed or not, are untouched, and an armed
/// account of the failed one stops with its tier still stated (`vike-backend venues` prints it).
#[test]
fn the_stop_names_one_venue_and_keeps_the_accounts_tier() {
    let table = table(vec![
        row(
            "bybit",
            AccountLabel::Default,
            Some(AccountTier::Demo),
            Some(AccountTier::Demo),
            ArmingBlock::None,
            None,
        ),
        row(
            "okx",
            AccountLabel::Default,
            Some(AccountTier::Live),
            Some(AccountTier::Live),
            ArmingBlock::None,
            None,
        ),
    ]);
    let policy = failed(&["bybit"]);
    let all = venue_arming(&table, &policy);
    assert_eq!(
        answer(&all),
        vec![(None, ArmingBlock::PreflightFailed), (Some(AccountTier::Live), ArmingBlock::None)]
    );
    assert_eq!(all[0].tier, Some(AccountTier::Demo), "the account row's tier is still stated");
    // The predicates read the SAME overlaid answer.
    assert!(!venue_would_go_live(&table, "bybit", Some(&policy)));
    assert!(venue_would_go_live(&table, "okx", Some(&policy)));
    assert!(venue_would_go_live(&table, "bybit", None), "no stop, an armed default account");
    assert_eq!(armed_live_venues(&table, &policy), vec!["okx".to_string()]);
}

/// A venue the table carries no row for is not live, and `venue_would_go_live` reads the DEFAULT
/// account only: a labelled account that arms does not make the venue's default account armed.
#[test]
fn the_live_intent_predicate_reads_the_default_account_only() {
    let table = table(vec![row(
        "okx",
        alt(),
        Some(AccountTier::Demo),
        Some(AccountTier::Demo),
        ArmingBlock::None,
        None,
    )]);
    assert!(!venue_would_go_live(&table, "okx", None));
    assert!(!venue_would_go_live(&table, "binance", None));
}

/// The shared-book report reads the books the TABLE carries: two running accounts on one book form
/// a pair, an account that does not run (after the stop, or never) contributes nothing, and an
/// account whose book the backend could not name contributes nothing either — nothing is said where
/// nothing can be told.
#[test]
fn the_shared_book_report_reads_the_tables_books() {
    let both = |book_alt: Option<&str>| {
        table(vec![
            row(
                "bybit",
                AccountLabel::Default,
                Some(AccountTier::Demo),
                Some(AccountTier::Demo),
                ArmingBlock::None,
                Some("592123"),
            ),
            row(
                "bybit",
                alt(),
                Some(AccountTier::Demo),
                Some(AccountTier::Demo),
                ArmingBlock::None,
                book_alt,
            ),
        ])
    };
    let same = both(Some("592123"));
    let rows = venue_account_arming(&same, "bybit", None);
    let found = shared_books_for(&same, &rows);
    assert_eq!(found.len(), 1, "one pair, one finding: {found:?}");
    assert_eq!(found[0].book, "592123");

    let different = both(Some("770044"));
    assert!(
        shared_books_for(&different, &venue_account_arming(&different, "bybit", None)).is_empty()
    );
    let unknown = both(None);
    assert!(shared_books_for(&unknown, &venue_account_arming(&unknown, "bybit", None)).is_empty());

    // After the preflight stop neither account runs, so neither has a book this process trades.
    let stopped = venue_account_arming(&same, "bybit", Some(&failed(&["bybit"])));
    assert!(shared_books_for(&same, &stopped).is_empty());
}

/// The text half of the unaddressable-account report is pure over rows, and the table half reads
/// the overlaid rows: a table with no `NoAccountSupport` row says nothing.
#[test]
fn the_unaddressable_report_is_silent_without_a_refused_row() {
    let quiet = table(vec![row(
        "bybit",
        AccountLabel::Default,
        Some(AccountTier::Demo),
        Some(AccountTier::Demo),
        ArmingBlock::None,
        None,
    )]);
    assert_eq!(unaddressable_accounts_message(&quiet, Some(&AdapterPolicy::default())), None);
    assert_eq!(unaddressable_accounts_message(&quiet, None), None, "no policy names nothing");

    let refused = table(vec![row("bybit", alt(), None, None, ArmingBlock::NoAccountSupport, None)]);
    let said = unaddressable_accounts_message(&refused, Some(&AdapterPolicy::default()))
        .expect("a refused labelled account is said out loud");
    assert!(said.contains("bybit") && said.contains("ALT"), "{said}");
}

/// **Whether a stated account leverage may reach the margin rate** (decision 0181), PURE: applied
/// only when stated, on a confirmable venue, with the venue's own `Ok` covering it; every other case
/// is a refusal with a reason, and an unstated one is not an event at all.
#[test]
fn an_account_leverage_is_applied_only_when_the_venue_confirmed_it() {
    let ok = |cap| Some(Ok::<f64, String>(cap));
    let demo = AccountTier::Demo;
    assert_eq!(
        judge_account_leverage("hyperliquid", demo, None, None, ok(20.0).as_ref()),
        LeverageVerdict::Unstated
    );
    assert_eq!(
        judge_account_leverage("hyperliquid", demo, Some(10.0), None, ok(20.0).as_ref()),
        LeverageVerdict::Applied { leverage: 10.0 }
    );
    assert_eq!(
        judge_account_leverage(
            "hyperliquid",
            AccountTier::Live,
            Some(20.0),
            None,
            ok(20.0).as_ref()
        ),
        LeverageVerdict::Applied { leverage: 20.0 },
        "equal to the venue's figure is allowed, and hyperliquid keeps its live tier"
    );
    assert_eq!(
        judge_account_leverage("hyperliquid", demo, Some(2.5), None, ok(20.0).as_ref()),
        LeverageVerdict::Applied { leverage: 2.5 },
        "hyperliquid takes a fraction, exactly as shipped"
    );
    let refused = |venue, requested, answer: Option<&Result<f64, String>>| {
        matches!(
            judge_account_leverage(venue, demo, requested, None, answer),
            LeverageVerdict::Refused(_)
        )
    };
    assert!(refused("hyperliquid", Some(25.0), ok(20.0).as_ref()), "above the venue's figure");
    assert!(refused("hyperliquid", Some(10.0), None), "no confirmation");
    assert!(
        refused("hyperliquid", Some(10.0), Some(&Err("down".to_string()))),
        "the venue said no"
    );
    assert!(refused("hyperliquid", Some(10.0), ok(f64::NAN).as_ref()), "an unusable figure");
    assert!(refused("deribit", Some(10.0), ok(50.0).as_ref()), "a venue with no leverage number");
}

/// **A venue that POSTS a leverage is trusted only when it holds EXACTLY the whole number the node
/// asked it to** (decision 0182): equal is applied; lower, higher, absent, an error, a different
/// ask, a closed tier, a closed row and a fraction are each a refusal that leaves 1x.
#[test]
fn a_posting_venue_must_hold_exactly_the_figure_it_was_asked_for() {
    let ok = |held| Some(Ok::<f64, String>(held));
    let judge = |venue, tier, stated, asked, answer: Option<&Result<f64, String>>| {
        judge_account_leverage(venue, tier, stated, asked, answer)
    };
    let demo = AccountTier::Demo;
    for venue in ["bybit", "okx", "binance"] {
        assert_eq!(
            judge(venue, demo, Some(10.0), Some(10.0), ok(10.0).as_ref()),
            LeverageVerdict::Applied { leverage: 10.0 },
            "{venue}: held == asked"
        );
        assert_eq!(
            judge(venue, demo, Some(10.0), Some(4.0), ok(4.0).as_ref()),
            LeverageVerdict::Applied { leverage: 4.0 },
            "{venue}: a ceiling lowered the ask, and the venue holds the lowered figure"
        );
        let refused = |what: &str, got: LeverageVerdict| {
            assert!(matches!(got, LeverageVerdict::Refused(_)), "{venue}: {what}: {got:?}");
        };
        refused("held below", judge(venue, demo, Some(10.0), Some(10.0), ok(5.0).as_ref()));
        refused("held ABOVE", judge(venue, demo, Some(10.0), Some(10.0), ok(20.0).as_ref()));
        refused(
            "a stale answer for another ask",
            judge(venue, demo, Some(10.0), Some(4.0), ok(10.0).as_ref()),
        );
        refused(
            "no answer (an older gatewayhub)",
            judge(venue, demo, Some(10.0), Some(10.0), None),
        );
        refused(
            "the lane's refusal",
            judge(venue, demo, Some(10.0), Some(10.0), Some(&Err("isolated".to_string()))),
        );
        refused("nothing was asked", judge(venue, demo, Some(10.0), None, ok(10.0).as_ref()));
        refused(
            "an unusable answer",
            judge(venue, demo, Some(10.0), Some(10.0), ok(f64::NAN).as_ref()),
        );
        refused(
            "a LIVE row is ignored",
            judge(venue, AccountTier::Live, Some(10.0), Some(10.0), ok(10.0).as_ref()),
        );
        refused("a planted fraction", judge(venue, demo, Some(2.5), Some(2.0), ok(2.0).as_ref()));
        assert_eq!(
            judge(venue, demo, None, None, ok(10.0).as_ref()),
            LeverageVerdict::Unstated,
            "{venue}: nothing stated is not an event"
        );
    }
    // aster is coded and closed; deribit has no leverage number, said in its own words.
    for venue in ["aster", "deribit"] {
        let got = judge(venue, demo, Some(10.0), Some(10.0), ok(10.0).as_ref());
        assert!(matches!(got, LeverageVerdict::Refused(_)), "{venue}: {got:?}");
    }
    let LeverageVerdict::Refused(why) = judge("deribit", demo, Some(5.0), None, None) else {
        panic!("deribit must be refused");
    };
    assert!(why.contains("no leverage number"), "{why}");
}
