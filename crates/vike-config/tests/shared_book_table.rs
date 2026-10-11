//! The gate on the shared-BOOK rule: two ACTIVE accounts of one venue that resolve to ONE trading
//! book are reported, and the per-pair report is capped so fifty accounts do not print 1,225 lines.
//!
//! Both halves are pure functions of `vike_config::venue_accounts` (`shared_books`,
//! `shared_book_report`), so this file needs no settings rows at all: every fixture is a list of
//! [`ArmedBook`]s, whose `mode` is the TIER the account mounted at (its `account` row's own
//! `tier`), and `vike_mount::book_identity` is what builds them in production.
//!
//! 1. **The shared-book rule says what it claims to say**, including the near-misses that are NOT
//!    findings — the loudest of which is two accounts on ONE SYMBOL, which is an ordinary spread.
//! 2. **The cap on the report** keeps the arithmetic honest: it truncates the pairs, never their
//!    content, and its summary counts ACCOUNTS per book rather than pairs.

use vike_config::{ArmedBook, VenueMode, shared_book_report, shared_books};
use vike_model::accounts::account_keys::AccountLabel;

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).unwrap_or_else(|e| panic!("{text} must be a legal label: {e}"))
}

// ------------------------------------------------------------------------------------------------
// 1. The shared-BOOK rule (⚠ this section replaced a SYMBOL-collision rule — see below)
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
// 2. The CAP on the per-pair report — the arithmetic that keeps fifty accounts from being 1,225
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
    let report = shared_book_report(pairs.clone(), 10);
    assert_eq!(report.shown.len(), 10);
    assert_eq!(report.shown, pairs[..10], "the kept lines are untouched, in order");
    assert_eq!(report.suppressed, pairs.len() - 10);
}

/// ⚠ **The summary counts ACCOUNTS, not pairs, and that is the whole point of it.** Fifty accounts
/// on one book produce 1,225 pairs; the number an operator needs is 50. A summary that reported
/// the pair count would be restating the flood it replaced.
#[test]
fn the_summary_counts_accounts_per_book_not_pairs() {
    let report = shared_book_report(one_book_with(50), 10);
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
    let report = shared_book_report(shared_books(&books), 2);
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
    let report = shared_book_report(one_book_with(3), 0);
    assert!(report.shown.is_empty());
    assert_eq!(report.suppressed, 3);
    assert_eq!(report.books, vec![("0xabc".to_string(), 3)]);
}

/// The empty finding stays empty and summarises nothing — the quiet answer every single-account
/// box gets, which is nearly all of them.
#[test]
fn no_shared_books_is_a_silent_report() {
    let report = shared_book_report(Vec::new(), vike_config::SHARED_BOOK_REPORT_CAP);
    assert!(report.shown.is_empty());
    assert_eq!(report.suppressed, 0);
    assert!(report.books.is_empty());
}
