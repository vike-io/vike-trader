//! The gate on `policy.accounts.*` — the per-ACCOUNT arming ceilings and the shared-BOOK rule,
//! both now FOLDED by the mount (`vike_mount::account_ceiling` / `make_engine_accounts`).
//!
//! `venues_table.rs` is the sibling for the per-VENUE half and stays the authority for it. This
//! file asserts the three things this change has to prove:
//!
//! 1. **A single-account box is unchanged.** Every existing shape of `policy.toml` — no file, no
//!    `[venues]` table, a `[venues]` table — resolves exactly as it does today, and an `[accounts]`
//!    table changes no venue-level answer at all.
//! 2. **The new table is refused by NAME when it is wrong**, in both of the places
//!    `deny_unknown_fields` structurally cannot reach: the venue and the label.
//! 3. **The shared-book rule says what it claims to say**, including the near-misses that are NOT
//!    findings — the loudest of which is two accounts on ONE SYMBOL, which the rule this section
//!    replaced REFUSED and which is an ordinary spread.
//!
//! ⚠ It also pins the DISCLOSURE the fold bought: the accounts table is a real `Policy` leaf (so it
//! has a `provenance` row, a `config show` cell and a template line), and a present `[accounts]`
//! table now warns about NOTHING — the inverse of the admission this file used to hold, deleted in
//! the same change as the fold it described.

use std::collections::HashMap;
use std::path::Path;

use vike_config::load::POLICY_FILE;
use vike_config::{ArmedBook, VenueMode, VenuePolicy, load, shared_books};
use vike_model::VENUES;
use vike_model::account_keys::AccountLabel;

fn env() -> HashMap<String, String> {
    HashMap::new()
}

fn write(dir: &Path, body: &str) {
    std::fs::write(dir.join(POLICY_FILE), body).expect("write policy.toml");
}

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).unwrap_or_else(|e| panic!("{text} must be a legal label: {e}"))
}

/// A venue ceiling plus two accounts under it — the shape the requirement describes.
const VENUE_AND_ACCOUNTS: &str = "\
[venues]
hyperliquid = \"live\"
bybit = \"demo\"

[accounts.hyperliquid]
ALT = \"live\"
TEST = \"demo\"
";

// ------------------------------------------------------------------------------------------------
// 1. A single-account box is unchanged
// ------------------------------------------------------------------------------------------------

/// **The step-1 contract, through the real loader.** An `[accounts]` table changes NO venue-level
/// answer: the ceilings that come out are identical to the ones the same file without the table
/// produces, venue by venue over the whole roster.
///
/// Asserted as an equality between two REAL loads rather than against a written-out expectation,
/// because the claim is "these two files resolve the same", and a hand-written expectation could
/// drift with the default and keep agreeing with itself.
#[test]
fn an_accounts_table_changes_no_venue_level_answer() {
    let with = tempfile::tempdir().unwrap();
    write(with.path(), VENUE_AND_ACCOUNTS);
    let with = load(Some(with.path()), &env()).expect("loads");

    let without = tempfile::tempdir().unwrap();
    write(without.path(), "[venues]\nhyperliquid = \"live\"\nbybit = \"demo\"\n");
    let without = load(Some(without.path()), &env()).expect("loads");

    for venue in VENUES {
        assert_eq!(
            with.policy.venues.get(venue),
            without.policy.venues.get(venue),
            "{venue}: an [accounts] table moved a VENUE ceiling"
        );
    }
    assert_eq!(with.policy.venues.len(), without.policy.venues.len());
    // …and the table really was read, so the equality above is not vacuous.
    assert!(with.policy.venues.accounts_declared(), "the fixture must actually declare accounts");
    assert!(!without.policy.venues.accounts_declared());
}

/// The DEFAULT account — the one a single-account box has — resolves to its venue's ceiling
/// exactly, for every roster venue and every existing shape of the file. This is what "a box with
/// today's `venue = \"mode\"` lines resolves exactly as now" means once accounts exist.
#[test]
fn the_default_account_resolves_to_its_venue_ceiling_everywhere() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), VENUE_AND_ACCOUNTS);
    let armed = load(Some(dir.path()), &env()).expect("loads").policy.venues;

    // …and the two files that have no venues table at all.
    let bare = tempfile::tempdir().unwrap();
    write(bare.path(), "max_leverage = 3.0\n");
    let bare = load(Some(bare.path()), &env()).expect("loads").policy.venues;
    let none = load(None, &env()).expect("defaults load").policy.venues;

    for policy in [&armed, &bare, &none] {
        for venue in VENUES {
            assert_eq!(
                policy.account(venue, &AccountLabel::Default),
                policy.get(venue),
                "{venue}: the default account is the venue's own ceiling"
            );
        }
    }
    // The no-file and no-table answers are still paper, which is the safest-absence rule.
    for venue in VENUES {
        assert_eq!(bare.account(venue, &AccountLabel::Default), VenueMode::Paper, "{venue}");
        assert_eq!(none.account(venue, &AccountLabel::Default), VenueMode::Paper, "{venue}");
    }
}

/// **A second account must be NAMED to be armed.** A `hyperliquid = "live"` line was written when
/// that venue had one account; reading it as consent for an account that did not exist then is the
/// silent escalation the ceiling exists to prevent.
#[test]
fn a_labelled_account_with_no_line_of_its_own_is_paper() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), VENUE_AND_ACCOUNTS);
    let p = load(Some(dir.path()), &env()).expect("loads").policy.venues;

    assert_eq!(p.get("hyperliquid"), VenueMode::Live);
    assert_eq!(p.account("hyperliquid", &label("ALT")), VenueMode::Live, "named, so armed");
    assert_eq!(p.account("hyperliquid", &label("TEST")), VenueMode::Demo);
    assert_eq!(
        p.account("hyperliquid", &label("UNNAMED")),
        VenueMode::Paper,
        "an account the file never names inherits NOTHING from the venue line"
    );
    // …and the same account under a venue the file never names is paper too.
    assert_eq!(p.account("okx", &label("ALT")), VenueMode::Paper);
}

/// **The ceiling still only ever refuses, one level down.** A venue's line caps every account under
/// it, so `bybit = "paper"` disarms an account stated `live`.
#[test]
fn a_venue_ceiling_caps_every_account_under_it() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "[venues]\nbybit = \"paper\"\nbinance = \"demo\"\n\n[accounts.bybit]\nALT = \"live\"\n\n\
         [accounts.binance]\nALT = \"live\"\n",
    );
    let p = load(Some(dir.path()), &env()).expect("loads").policy.venues;

    assert_eq!(p.account("bybit", &label("ALT")), VenueMode::Paper, "a paper venue disarms all");
    assert_eq!(p.account("binance", &label("ALT")), VenueMode::Demo, "capped to the venue's demo");
    // The stated lines are still what the file said — the cap is applied on READ, so a raised
    // venue ceiling later frees the account without the operator rewriting it.
    let stated: Vec<(&str, &str, VenueMode)> = p.accounts().collect();
    assert_eq!(
        stated,
        vec![("binance", "ALT", VenueMode::Live), ("bybit", "ALT", VenueMode::Live)]
    );
}

/// An `[accounts]` table states no VENUE ceiling, so it must not set `is_declared` — that flag is
/// what `vike_mount::venue_arming_migration` self-silences on, and silencing it here would tell a
/// deployment it had made an arming decision it has not made.
#[test]
fn an_accounts_table_declares_no_venue_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "[accounts.bybit]\nALT = \"live\"\n");
    let p = load(Some(dir.path()), &env()).expect("loads").policy.venues;
    assert!(p.accounts_declared(), "the account line was read");
    assert!(!p.is_declared(), "…and it is not a venue arming decision");
    for venue in VENUES {
        assert_eq!(p.get(venue), VenueMode::Paper, "{venue}");
    }
}

/// The VENUE ceiling map still serializes as the flat `{venue: mode}` object `provenance` walks —
/// the per-account entries are `#[serde(skip)]` on `VenuePolicy`, so no leaf appears THERE and every
/// `policy.venues.<venue>` row is untouched.
///
/// # ⚠ Where the accounts table IS disclosed, and why it took this shape
///
/// It is one leaf up, on `Policy` itself: that type's hand-written `Serialize` emits
/// `VenuePolicy::accounts_by_venue` as its `accounts` field, which is what
/// `vike_config::provenance::setting_keys`' `policy.accounts` row, `vike-cli config show`'s cell and
/// `settings/policy.example.toml`'s line all hang off.
///
/// This test carried the argument for why NONE of that was available while the table bound nothing,
/// and both of its reasons were discharged rather than waived:
///
/// * **Mechanically**, an EMPTY map contributed no leaf at all, so a row added without one failed as
///   STALE. `VenuePolicy` is `#[serde(transparent)]` and cannot carry a second serialized field, so
///   the projection went up to `Policy` — and `crates/vike-config/tests/provenance.rs`'s walk now
///   treats an empty object as a leaf of its own, which closes a real hole in that gate (any
///   empty-by-default map field escaped it entirely) rather than making room for this one.
/// * **Semantically**, a row would have made `config show` report an EFFECTIVE value for a table
///   nothing folded. That is no longer true: `vike_mount::account_ceiling` folds it at the mount,
///   above the credential read, and `make_engine_accounts` mounts one engine per active account.
///
/// The load-time "binds nothing yet" warning went with them, in the same change — which is the whole
/// point of having written the condition down: the three disclosures and the fold are one event.
#[test]
fn the_venue_ceilings_still_serialize_as_a_flat_map() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), VENUE_AND_ACCOUNTS);
    let p = load(Some(dir.path()), &env()).expect("loads").policy.venues;

    let table = toml::Table::try_from(&p).expect("serializes");
    assert_eq!(table.len(), VENUES.len(), "the roster, and not one key more");
    assert_eq!(table["hyperliquid"].as_str(), Some("live"));
    assert!(!table.contains_key("ALT"), "an account is not a venue");
}

/// …and the ACCOUNTS table is a real leaf on `Policy`, carrying the file's own nesting — the
/// disclosure surface `provenance`'s `policy.accounts` row names.
#[test]
fn the_accounts_table_is_a_policy_leaf_in_the_files_own_shape() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), VENUE_AND_ACCOUNTS);
    let policy = load(Some(dir.path()), &env()).expect("loads").policy;

    let table = toml::Table::try_from(&policy).expect("serializes");
    let accounts = table["accounts"].as_table().expect("an accounts table");
    let hl = accounts["hyperliquid"].as_table().expect("hyperliquid's accounts");
    assert_eq!(hl["ALT"].as_str(), Some("live"));
    assert_eq!(hl["TEST"].as_str(), Some("demo"));
    assert!(!accounts.contains_key("bybit"), "a venue with no named account gets no entry");

    // …and a DEFAULT policy serializes the key as an EMPTY table rather than omitting it — which is
    // the leaf `provenance`'s completeness gate matches its row against.
    let default = toml::Table::try_from(vike_config::Policy::default()).expect("serializes");
    assert!(
        default["accounts"].as_table().expect("still a table").is_empty(),
        "the default names no account"
    );
}

/// The out-of-crate builder states what it names and ignores what it cannot address — the twin of
/// `VenuePolicy::declare`, and the only way a caller outside this crate can build one.
#[test]
fn declare_account_states_one_account_and_ignores_a_non_venue() {
    let p = VenuePolicy::default()
        .declare("bybit", VenueMode::Live)
        .declare_account("bybit", &label("ALT"), VenueMode::Live)
        .declare_account("Bybit", &label("SHOUT"), VenueMode::Live)
        .declare_account("bybit", &AccountLabel::Default, VenueMode::Live);

    assert_eq!(p.account("bybit", &label("ALT")), VenueMode::Live);
    assert_eq!(
        p.account("bybit", &label("SHOUT")),
        VenueMode::Paper,
        "a shouted venue states none"
    );
    let stated: Vec<(&str, &str, VenueMode)> = p.accounts().collect();
    assert_eq!(stated, vec![("bybit", "ALT", VenueMode::Live)], "the default label is not stored");
}

// ------------------------------------------------------------------------------------------------
// 2. Refused by name
// ------------------------------------------------------------------------------------------------

/// Both halves `deny_unknown_fields` cannot see, refused by NAME with the key the operator typed.
///
/// ⚠ The dangerous direction is the reason: an accepted `[accounts.bybitt]` or a silently
/// uppercased `alt` reads to its author as a ceiling that is armed, and it is not.
#[test]
fn an_unknown_account_venue_and_an_illegal_label_are_each_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();

    write(dir.path(), "[accounts.bybitt]\nALT = \"paper\"\n");
    let msg = load(Some(dir.path()), &env()).expect_err("a non-venue must be refused").to_string();
    assert!(msg.contains("accounts.bybitt"), "{msg}");
    assert!(msg.contains("bybit"), "the suggestion must name the venue meant: {msg}");

    write(dir.path(), "[accounts.bybit]\nalt = \"paper\"\n");
    let msg = load(Some(dir.path()), &env()).expect_err("a lowercase label is refused").to_string();
    assert!(msg.contains("accounts.bybit.alt"), "{msg}");

    write(dir.path(), "[accounts.bybit]\nDEFAULT = \"live\"\n");
    let msg =
        load(Some(dir.path()), &env()).expect_err("the reserved label is refused").to_string();
    assert!(msg.contains("accounts.bybit.DEFAULT"), "{msg}");

    // …and the MODE is still refused by serde, one level deeper than before, with the legal set.
    write(dir.path(), "[accounts.bybit]\nALT = \"real\"\n");
    let msg = load(Some(dir.path()), &env()).expect_err("an illegal mode is refused").to_string();
    assert!(msg.contains("paper"), "the legal spellings must be in the message: {msg}");
}

/// **A present `[accounts]` table warns about NOTHING**, and that is the exact inverse of what this
/// test asserted while the table bound nothing.
///
/// The old admission said the ceilings were "validated and stored but NOT YET ENFORCED". They are
/// enforced now — `vike_mount::account_ceiling` folds them above the credential read and
/// `make_engine_accounts` mounts one engine per active account — so a loader still printing it would
/// be the mirror image of the defect it was written for: an operator told that an armed ceiling is
/// inert.
///
/// Removing it also un-blocks the template. `crates/vike-cli/tests/settings_examples.rs`'s
/// `every_template_line_is_one_the_loader_accepts` uncomments every line of
/// `settings/policy.example.toml` and REQUIRES the result to load without warnings, which no
/// `accounts` line could have satisfied while the admission stood.
#[test]
fn an_accounts_table_binds_and_therefore_warns_about_nothing() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), VENUE_AND_ACCOUNTS);
    let settings = load(Some(dir.path()), &env()).expect("loads");
    assert!(
        settings.warnings.is_empty(),
        "an [accounts] table binds its ceilings now, so nothing is owed an admission: {:?}",
        settings.warnings
    );
    // …and the table really was READ, so the emptiness above is not vacuous.
    assert!(settings.policy.venues.accounts_declared());
    assert_eq!(settings.policy.venues.account("hyperliquid", &label("ALT")), VenueMode::Live);

    // The same for every other shape of file, which is where it always was.
    let quiet = tempfile::tempdir().unwrap();
    write(quiet.path(), "[venues]\nbybit = \"live\"\n");
    assert!(load(Some(quiet.path()), &env()).expect("loads").warnings.is_empty());
    // ⚠ `None` is the ONE shape that warns, and about something else entirely: it means no project
    // was resolved at all, so no `[accounts]` table could have been read to bind or not bind (see
    // `vike_config::NO_SETTINGS_DIRECTORY_WARNING`). Asserted as EQUALITY so an `[accounts]`
    // admission coming back here would still fail this test.
    assert_eq!(
        load(None, &env()).expect("defaults load").warnings,
        vec![vike_config::NO_SETTINGS_DIRECTORY_WARNING.to_string()]
    );
}

/// The sealed-policy property, asserted for the new key rather than assumed from the type: nothing
/// in the environment can state an account ceiling.
#[test]
fn nothing_in_the_environment_can_set_an_account_ceiling() {
    let vars: HashMap<String, String> = [
        ("VIKE_ACCOUNTS", "bybit.ALT=live"),
        ("VIKE_POLICY_ACCOUNTS_BYBIT_ALT", "live"),
        ("BYBIT_ALT", "live"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
    .collect();
    let p = load(None, &vars).expect("loads").policy.venues;
    assert!(!p.accounts_declared());
    assert_eq!(p.account("bybit", &label("ALT")), VenueMode::Paper);
}

// ------------------------------------------------------------------------------------------------
// ------------------------------------------------------------------------------------------------
// 3. The shared-BOOK rule (⚠ this section replaced a SYMBOL-collision rule — see below)
// ------------------------------------------------------------------------------------------------

fn book(venue: &'static str, lbl: AccountLabel, book: &str, mode: VenueMode) -> ArmedBook {
    ArmedBook { venue, label: lbl, book: book.to_string(), mode }
}

/// **THE rule.** Two ACTIVE accounts of one venue resolving to ONE book are reported, and the
/// report names both accounts and the book.
#[test]
fn two_active_accounts_of_one_venue_on_one_book_are_reported() {
    let books = [
        book("hyperliquid", AccountLabel::Default, "0xabc", VenueMode::Live),
        book("hyperliquid", label("ALT"), "0xabc", VenueMode::Live),
    ];
    let found = shared_books(&books);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].venue, "hyperliquid");
    assert_eq!(found[0].book, "0xabc");
    assert_eq!(found[0].first, AccountLabel::Default);
    assert_eq!(found[0].second, label("ALT"));

    let why = found[0].why();
    assert!(why.contains("hyperliquid") && why.contains("0xabc") && why.contains("ALT"), "{why}");

    // …and a DEMO pair is reported too: a demo account is a real book with real rejections, and two
    // engines double-folding it is the same defect with play money.
    let demo = [
        book("hyperliquid", AccountLabel::Default, "0xabc", VenueMode::Demo),
        book("hyperliquid", label("ALT"), "0xabc", VenueMode::Demo),
    ];
    assert_eq!(shared_books(&demo).len(), 1);
}

/// ⚠ **THE HEADLINE CORRECTION: one SYMBOL, two accounts, is not a finding at all.**
///
/// The rule this section replaced refused exactly this arming — and refusing it was the defect.
/// Long BTC on account A and short BTC on account B is an ordinary spread: two accounts are two
/// wallets (hyperliquid SUB-accounts have their own addresses), they hold separate positions, and
/// the venue nets nothing between them. The symbol never enters this rule, so there is no shape of
/// input in which it could come back — [`ArmedBook`] carries no symbol field to compare.
#[test]
fn two_accounts_of_one_venue_on_one_symbol_are_not_a_finding() {
    // Two DISTINCT books at one venue — which is what two accounts on one symbol are. The old rule
    // capped the second to paper here; this one says nothing.
    let spread = [
        book("hyperliquid", AccountLabel::Default, "0xaaa", VenueMode::Live),
        book("hyperliquid", label("SHORT"), "0xbbb", VenueMode::Live),
    ];
    assert!(shared_books(&spread).is_empty(), "{:?}", shared_books(&spread));
}

/// The four near-misses that are NOT shared books — each is a way the rule could be written too
/// loosely, and each would fire on an arming an operator is entitled to.
#[test]
fn the_four_near_misses_are_not_shared_books() {
    // 1. Different venues: two exchanges hold two ledgers even when the identifier string is
    //    genuinely equal (one EVM address is a real account on hyperliquid AND on aster).
    let across = [
        book("hyperliquid", AccountLabel::Default, "0xabc", VenueMode::Live),
        book("aster", label("ALT"), "0xabc", VenueMode::Live),
    ];
    assert!(shared_books(&across).is_empty(), "{:?}", shared_books(&across));

    // 2. Different books on one venue: the whole point of a second account.
    let split = [
        book("hyperliquid", AccountLabel::Default, "0xaaa", VenueMode::Live),
        book("hyperliquid", label("ALT"), "0xbbb", VenueMode::Live),
    ];
    assert!(shared_books(&split).is_empty());

    // 3. PAPER shares nothing — the paper exchange fills locally and touches no venue.
    let paper = [
        book("hyperliquid", AccountLabel::Default, "0xabc", VenueMode::Paper),
        book("hyperliquid", label("ALT"), "0xabc", VenueMode::Paper),
    ];
    assert!(shared_books(&paper).is_empty());
    let one_paper = [
        book("hyperliquid", AccountLabel::Default, "0xabc", VenueMode::Live),
        book("hyperliquid", label("ALT"), "0xabc", VenueMode::Paper),
    ];
    assert!(shared_books(&one_paper).is_empty(), "one live account shares with nobody");

    // 4. The SAME account twice is a duplicate row, not two engines. Reporting it here would send an
    //    operator looking for a second account that does not exist.
    let dupe = [
        book("hyperliquid", label("ALT"), "0xabc", VenueMode::Live),
        book("hyperliquid", label("ALT"), "0xabc", VenueMode::Live),
    ];
    assert!(shared_books(&dupe).is_empty());
}

/// The report is deterministic and deduplicated: three accounts on one book are three PAIRS, in a
/// stable order, whichever order the rows arrive in.
#[test]
fn the_report_is_deterministic_and_pairs_are_reported_once() {
    let mut books = vec![
        book("hyperliquid", label("C"), "0xabc", VenueMode::Live),
        book("hyperliquid", AccountLabel::Default, "0xabc", VenueMode::Live),
        book("hyperliquid", label("B"), "0xabc", VenueMode::Live),
    ];
    let first = shared_books(&books);
    assert_eq!(first.len(), 3, "three accounts are three pairs: {first:?}");
    books.reverse();
    assert_eq!(shared_books(&books), first, "the order of the input must not change the report");

    // The default account sorts first, and every pair is (lower, higher) exactly once.
    let pairs: Vec<(String, String)> =
        first.iter().map(|c| (c.first.to_string(), c.second.to_string())).collect();
    assert_eq!(
        pairs,
        vec![
            ("DEFAULT".to_string(), "B".to_string()),
            ("DEFAULT".to_string(), "C".to_string()),
            ("B".to_string(), "C".to_string()),
        ]
    );

    // An empty arming shares nothing, and a single account cannot share with itself.
    assert!(shared_books(&[]).is_empty());
    assert!(shared_books(&books[..1]).is_empty());
}

/// The book is compared EXACTLY, and normalization is the PRODUCER's job
/// (`vike_mount::book_identity` trims and lowercases before it builds an [`ArmedBook`]). A rule that
/// case-folded here would be a second copy of that knowledge, and the two copies would be free to
/// disagree; a rule that did NOT would be wrong for an EVM address, which is the same account in
/// either case. So the knowledge lives at one end, and this test pins WHICH end.
#[test]
fn the_book_comparison_is_exact_because_the_producer_normalizes() {
    let books = [
        book("hyperliquid", AccountLabel::Default, "0xABC", VenueMode::Live),
        book("hyperliquid", label("ALT"), "0xabc", VenueMode::Live),
    ];
    assert!(
        shared_books(&books).is_empty(),
        "this rule compares text; `vike_mount::book_identity::normalize_book` is what makes two \
         spellings of one address arrive equal"
    );
}

// ------------------------------------------------------------------------------------------------
// 4. The CAP on the per-pair report — the arithmetic that keeps fifty accounts from being 1,225
//    startup lines. Tested HERE and not at the emission site because that site cannot be reached by
//    a test: two accounts can only both be ACTIVE on a venue with a real live arm, so a test that
//    got as far as the `warn!` would dial the venue on its next statement. Splitting the emission
//    from the arithmetic is what buys this coverage at all.
// ------------------------------------------------------------------------------------------------

/// `n` accounts of one venue, all on ONE book — the pathological shape, and the reason the cap
/// exists: this is `n(n-1)/2` pairs.
fn one_book_with(n: usize) -> Vec<vike_config::SharedBook> {
    let books: Vec<ArmedBook> = (0..n)
        .map(|i| book("hyperliquid", label(&format!("A{i}")), "0xabc", VenueMode::Live))
        .collect();
    shared_books(&books)
}

/// ⚠ **THE MOTIVATING NUMBER, asserted rather than asserted-about.** Fifty accounts of one wallet
/// is 1,225 findings — so the cap is not a style preference, and a reader who doubts it can read
/// the count here instead of taking the doc comment's word.
#[test]
fn fifty_accounts_on_one_book_really_are_over_a_thousand_pairs() {
    assert_eq!(one_book_with(50).len(), 50 * 49 / 2);
}

/// Under the cap, NOTHING changes: every pair is shown in full and the summary says nothing. This
/// is every box in production today, and it is the half that would go unnoticed if it broke.
#[test]
fn a_report_under_the_cap_is_the_report_it_has_always_been() {
    let pairs = one_book_with(3); // 3 pairs, cap 10
    let report =
        vike_config::shared_book_report(pairs.clone(), vike_config::SHARED_BOOK_REPORT_CAP);
    assert_eq!(report.shown, pairs, "every pair keeps its own line");
    assert_eq!(report.suppressed, 0);
    assert!(report.books.is_empty(), "no summary is owed when nothing was dropped");
}

/// Over the cap, the per-pair CONTENT survives for the ones shown — the cap truncates the list, it
/// does not summarise the lines it keeps.
#[test]
fn a_report_over_the_cap_keeps_the_shown_pairs_verbatim() {
    let pairs = one_book_with(50);
    let report = vike_config::shared_book_report(pairs.clone(), 10);
    assert_eq!(report.shown.len(), 10);
    assert_eq!(report.shown, pairs[..10], "the kept lines are untouched, in order");
    assert_eq!(report.suppressed, pairs.len() - 10);
}

/// ⚠ **The summary counts ACCOUNTS, not pairs, and that is the whole point of it.** Fifty accounts
/// on one book produce 1,225 pairs; the number an operator needs is 50. A summary that reported
/// the pair count would be restating the flood it replaced.
#[test]
fn the_summary_counts_accounts_per_book_not_pairs() {
    let report = vike_config::shared_book_report(one_book_with(50), 10);
    assert_eq!(report.books, vec![("0xabc".to_string(), 50)]);
}

/// Several books at once: each is counted separately and the WORST leads, so the line reads
/// worst-first rather than alphabetically.
#[test]
fn several_books_are_counted_separately_and_the_worst_leads() {
    let mut books: Vec<ArmedBook> = (0..4)
        .map(|i| book("hyperliquid", label(&format!("Z{i}")), "0xsmall", VenueMode::Live))
        .collect();
    books.extend(
        (0..9).map(|i| book("hyperliquid", label(&format!("A{i}")), "0xbig", VenueMode::Live)),
    );
    let report = vike_config::shared_book_report(shared_books(&books), 2);
    assert_eq!(
        report.books,
        vec![("0xbig".to_string(), 9), ("0xsmall".to_string(), 4)],
        "descending by account count — the worst book is the one to look at first"
    );
    // …and the two books' pairs are genuinely both in there: 36 + 6 = 42, less the 2 shown.
    assert_eq!(report.suppressed, 9 * 8 / 2 + 4 * 3 / 2 - 2);
}

/// A zero cap is the aggregate ALONE — a legitimate caller choice, and the reason the cap is a
/// parameter rather than read from the constant inside the function.
#[test]
fn a_zero_cap_leaves_only_the_summary() {
    let report = vike_config::shared_book_report(one_book_with(3), 0);
    assert!(report.shown.is_empty());
    assert_eq!(report.suppressed, 3);
    assert_eq!(report.books, vec![("0xabc".to_string(), 3)]);
}

/// The empty finding stays empty and summarises nothing — the quiet answer every single-account
/// box gets, which is nearly all of them.
#[test]
fn no_shared_books_is_a_silent_report() {
    let report = vike_config::shared_book_report(Vec::new(), vike_config::SHARED_BOOK_REPORT_CAP);
    assert!(report.shown.is_empty());
    assert_eq!(report.suppressed, 0);
    assert!(report.books.is_empty());
}

// ------------------------------------------------------------------------------------------------
// 5. The per-ACCOUNT exposure ceiling — `[account_exposure]`
//
// A SEPARATE table from `[accounts]` above, and the reason is `DEFAULT`: that table refuses the
// reserved spelling because an account's MODE ceiling is the venue's own line, while a FIGURE has no
// venue line to inherit. Folding the two together would have left the unlabelled account — the only
// account most boxes have — unable to state its own number, forever.
// ------------------------------------------------------------------------------------------------

/// Load a policy from `body` and hand back the resolved [`VenuePolicy`].
fn policy_of(body: &str) -> VenuePolicy {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), body);
    load(Some(dir.path()), &env()).expect("the fixture must load").policy.venues
}

/// Load a policy from `body` expecting a REFUSAL, and hand back its rendered text.
fn refusal_of(body: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), body);
    load(Some(dir.path()), &env()).expect_err("the fixture must be refused").to_string()
}

/// ⚠ **THE HEADLINE: the unlabelled account can state its own figure**, which is the whole reason
/// this is a table of its own rather than a field inside `[accounts]`. That table refuses `DEFAULT`
/// by design, so an exposure folded into it could never have reached the account most boxes have.
#[test]
fn the_default_account_can_state_its_own_exposure() {
    let p = policy_of(
        "[venues]\nbinance = \"live\"\n\n[account_exposure.binance]\nDEFAULT = 50000.0\n",
    );
    assert_eq!(p.account_exposure("binance", &AccountLabel::Default), Some(50000.0));
}

/// A labelled account's own figure, and the default account's, are INDEPENDENT — naming one says
/// nothing about the other.
#[test]
fn a_labelled_account_and_the_default_carry_separate_figures() {
    let p = policy_of(
        "[venues]\nbinance = \"live\"\n\n[accounts.binance]\nALT = \"live\"\n\n\
         [account_exposure.binance]\nDEFAULT = 50000.0\nALT = 5000.0\n",
    );
    assert_eq!(p.account_exposure("binance", &AccountLabel::Default), Some(50000.0));
    assert_eq!(p.account_exposure("binance", &label("ALT")), Some(5000.0));
}

/// ⚠ **An account with no line has NO figure — it does not inherit the venue's or the box's.** The
/// fold belongs to the mount, which narrows the box-wide `max_account_exposure` with this; a
/// resolver that inherited here would apply the box figure twice and look like it had done
/// something.
#[test]
fn an_account_with_no_line_states_no_figure() {
    let p = policy_of("max_account_exposure = 10000.0\n\n[venues]\nbinance = \"live\"\n");
    assert_eq!(p.account_exposure("binance", &AccountLabel::Default), None);
    assert_eq!(p.account_exposure("binance", &label("ALT")), None);
}

/// ⚠ **A LABELLED account must be armed before it can be capped**, and the refusal names the fix.
/// Two reasons pointing the same way: the ceiling would mean nothing (a labelled account with no
/// mode line resolves to `paper`, so it trades nothing to cap) and it would be LOST (the mirror
/// writes one `venue_arming` row per account `[accounts]` names, so a figure with no mode line
/// beside it has no row to ride).
#[test]
fn an_exposure_for_an_unarmed_labelled_account_is_refused_by_name() {
    let text =
        refusal_of("[venues]\nbinance = \"live\"\n\n[account_exposure.binance]\nALT = 5000.0\n");
    assert!(text.contains("ALT"), "names the account: {text}");
    assert!(text.contains("accounts.binance"), "…and the table that arms it: {text}");
    assert!(text.contains("paper"), "…and why the ceiling would cap nothing: {text}");
}

/// …and the DEFAULT account is exempt from that rule, because it needs no `[accounts]` line — its
/// mode is the venue's own. Without this the test above would be satisfied by refusing every line.
#[test]
fn the_default_account_needs_no_accounts_line_to_be_capped() {
    let p =
        policy_of("[venues]\nbinance = \"live\"\n\n[account_exposure.binance]\nDEFAULT = 1.0\n");
    assert_eq!(p.account_exposure("binance", &AccountLabel::Default), Some(1.0));
}

/// ⚠ **A non-positive figure is REFUSED rather than stored.** The fold is a `min`, so a zero or
/// negative ceiling would become the BINDING one and refuse every order on that account — which
/// reads as a broken venue rather than as a settings mistake.
#[test]
fn a_non_positive_exposure_is_refused_because_min_would_make_it_binding() {
    for bad in ["0.0", "-1.0"] {
        let text = refusal_of(&format!(
            "[venues]\nbinance = \"live\"\n\n[account_exposure.binance]\nDEFAULT = {bad}\n"
        ));
        assert!(text.contains("positive"), "must say what a usable figure is ({bad}): {text}");
        assert!(text.contains("min"), "…and why a zero binds rather than disables: {text}");
    }
}

/// The venue is refused by name, exactly as `[accounts]`' twin refusal does — an operator greps for
/// the key they typed, and the two tables are typed in different places.
#[test]
fn an_exposure_table_naming_no_venue_is_refused_by_name() {
    let text = refusal_of("[account_exposure.binanace]\nDEFAULT = 5000.0\n");
    assert!(text.contains("binanace"), "names the offender: {text}");
    assert!(text.contains("binance"), "…and the roster, which includes the near miss: {text}");
}

/// A label the wire reader will not take is refused. ⚠ The message must NOT repeat `[accounts]`'
/// "the venue's own line" advice — here `DEFAULT` is a legal subject, and saying otherwise would
/// send an operator to delete a line that is correct.
#[test]
fn an_illegal_exposure_label_is_refused_without_forbidding_default() {
    let text =
        refusal_of("[venues]\nbinance = \"live\"\n\n[account_exposure.binance]\nalt = 5.0\n");
    assert!(text.contains("alt"), "names the label: {text}");
    assert!(
        text.contains("DEFAULT") && text.contains("allowed"),
        "…and says DEFAULT is allowed here, unlike in `[accounts]`: {text}"
    );
}

/// ⚠ **A box that writes no `[account_exposure]` table is byte-identical to before it existed.**
/// The one property every existing deployment depends on, asserted rather than assumed.
#[test]
fn a_policy_with_no_exposure_table_states_nothing() {
    let p = policy_of(VENUE_AND_ACCOUNTS);
    assert!(!p.account_exposures_declared());
    for v in vike_model::VENUES {
        assert_eq!(p.account_exposure(v, &AccountLabel::Default), None, "{v}");
    }
}
