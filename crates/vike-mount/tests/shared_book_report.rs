//! **The shared-BOOK report over REAL arming rows** — `vike_mount::shared_books_for`, the finding
//! `make_engine_accounts` warns about and then mounts both accounts anyway.
//!
//! It is the mount-side half of a rule split across two crates: `vike_config::shared_books` is the
//! pure predicate (unit-tested in `crates/vike-config/tests/venue_accounts_table.rs`), and this file
//! drives it through `vike_mount::venue_account_arming` — the SAME projection the fan-out selects
//! accounts with — over real credential-store keys, so the report can never describe an arming that
//! is not the one being mounted.
//!
//! ⚠ **What is NOT here, and it is the declared blind spot**: the `tracing::warn!` EMISSION itself.
//! Two accounts can only both be ACTIVE on a venue with a real live arm, so a test that reached
//! `make_engine_accounts` would dial the venue on the very next statement. What is gated is the
//! CONTENT (below), and the EFFECT — `a_shared_book_removes_no_account_from_the_mount_set` over
//! `vike_mount::accounts_to_mount`, plus `account_fanout.rs` (both accounts still produce a row).
//! Closing the emission needs a mount-less seam for the report, not a cleverer assertion.
//!
//! The effect half is the one that was missing and it is the one that matters: a deleted `warn!`
//! costs an operator a signal, while a shared book that REFUSED would cost them the account — and
//! that second mutation was measured GREEN across this crate and `vike-run` before
//! `accounts_to_mount` existed.
//!
//! Network-free by construction: `venue_account_arming` and `shared_books_for` are pure — they read
//! the caller's map through each arm's own config loader and open no socket.

use std::collections::HashMap;

use vike_config::{VenueMode, VenuePolicy};
use vike_model::account_keys::AccountLabel;

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// Both accounts of `venue` armed at `demo`, which is what makes them ACTIVE and therefore
/// comparable at all.
fn both_armed(venue: &str) -> VenuePolicy {
    VenuePolicy::default().declare(venue, VenueMode::Demo).declare_account(
        venue,
        &alt(),
        VenueMode::Demo,
    )
}

fn arming_rows(venue: &str, vars: &HashMap<String, String>) -> Vec<vike_config::VenueArming> {
    // The symbol map does not reach an arming answer any more; a one-entry default row is what
    // every caller with no labelled mount passes.
    vike_mount::venue_account_arming(venue, vars, Some(&both_armed(venue)))
}

/// **THE WARNING.** An AGENT wallet configured under one label beside the MASTER whose own key is
/// under another: two engines, one venue book. Reported by name — venue, both labels, the shared
/// address — and both accounts stay armed.
#[test]
fn two_hyperliquid_accounts_on_one_master_are_reported_and_both_stay_armed() {
    let v = vars(&[
        // The master's own key, and its address stated explicitly.
        ("HYPERLIQUID_DEMO_PRIVATE_KEY", "0xmasterkey"),
        ("HYPERLIQUID_DEMO_ACCOUNT_ADDRESS", "0xMASTER"),
        // An AGENT wallet that signs FOR that same master — a different key, the same book.
        ("HYPERLIQUID_DEMO_PRIVATE_KEY__ALT", "0xagentkey"),
        ("HYPERLIQUID_DEMO_ACCOUNT_ADDRESS__ALT", "0xmaster"),
    ]);
    let rows = arming_rows("hyperliquid", &v);
    assert_eq!(rows.len(), 2, "the default account and ALT: {rows:?}");
    assert_ne!(rows[0].effective, VenueMode::Paper, "the default account arms");
    assert_ne!(rows[1].effective, VenueMode::Paper, "…and so does the one sharing its book");

    let found = vike_mount::shared_books_for("hyperliquid", &rows, &v);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].venue, "hyperliquid");
    assert_eq!(found[0].book, "0xmaster", "case-folded by the producer, not by the rule");
    assert_eq!(found[0].first, AccountLabel::Default);
    assert_eq!(found[0].second, alt());

    // The sentence an operator reads has to carry all three, or "hyperliquid has a problem" is all
    // they get — and recognising one's own paste error is the whole point of the report.
    let said = found[0].why();
    assert!(said.contains("hyperliquid"), "names the venue: {said}");
    assert!(said.contains("DEFAULT") && said.contains("ALT"), "names BOTH accounts: {said}");
    assert!(said.contains("0xmaster"), "names the shared book: {said}");
}

/// **…AND THE REPORT REMOVES NO ENGINE.** The finding above is a WARNING; both accounts are
/// mounted, and `vike_mount::accounts_to_mount` — the whole of `make_engine_accounts`' mount
/// decision — must return both labels over exactly the rows that produced it.
///
/// ⚠ **This test exists because the property was gated by nothing.** Dropping every
/// `SharedBook::second` from that fan-out's loop — a shared book REFUSING the operator's second
/// account instead of naming it, which is `docs/decisions/0013-degrade-vs-refuse.md` inverted on
/// the one path where the refusal is silent — was measured GREEN across this crate and `vike-run`.
/// The loop could not be reached: two ACTIVE accounts need a real credential store and the next
/// statement dials the venue. The decision moved out of the loop so this assertion can exist, and
/// `accounts_to_mount`'s signature is the structural half — it takes arming ROWS, which carry no
/// address, so the book is not merely unread there but unavailable.
#[test]
fn a_shared_book_removes_no_account_from_the_mount_set() {
    let v = vars(&[
        ("HYPERLIQUID_DEMO_PRIVATE_KEY", "0xmasterkey"),
        ("HYPERLIQUID_DEMO_ACCOUNT_ADDRESS", "0xMASTER"),
        ("HYPERLIQUID_DEMO_PRIVATE_KEY__ALT", "0xagentkey"),
        ("HYPERLIQUID_DEMO_ACCOUNT_ADDRESS__ALT", "0xmaster"),
    ]);
    let rows = arming_rows("hyperliquid", &v);
    // The premise: these rows really are the shared-book case, so a green here is a verdict about
    // the finding and not about a configuration that never produced one.
    assert_eq!(
        vike_mount::shared_books_for("hyperliquid", &rows, &v).len(),
        1,
        "premise: this configuration IS a finding"
    );

    assert_eq!(
        vike_mount::accounts_to_mount(&rows),
        vec![AccountLabel::Default, alt()],
        "a shared book must not remove an account from the mount set — both engines are built, \
         default account FIRST (every caller binds it at [0])"
    );
}

/// A PAPER account of a venue is still not mounted, so the test above is a statement about the
/// shared-book finding rather than about `accounts_to_mount` returning everything it is handed.
///
/// The default account is the exception and is mounted whatever it resolved: it is the engine the
/// single-account fan-out always returned, and a paper mount is exactly what an unarmed venue is
/// supposed to get.
#[test]
fn an_unarmed_labelled_account_is_still_not_mounted() {
    let v = vars(&[("HYPERLIQUID_DEMO_PRIVATE_KEY", "0xmasterkey")]);
    // ALT is named by no policy line and has no `__ALT` key of its own.
    let rows = vike_mount::venue_account_arming(
        "hyperliquid",
        &v,
        Some(&VenuePolicy::default().declare("hyperliquid", VenueMode::Demo)),
    );
    assert_eq!(
        vike_mount::accounts_to_mount(&rows),
        vec![AccountLabel::Default],
        "an account that armed nothing gets no engine; the default account always does"
    );
}

/// **TWO SUB-ACCOUNTS ARE TWO BOOKS.** The same configuration with a different master address is an
/// ordinary second account, and nothing is said about it — which is the case the report must not
/// fire on, since hyperliquid sub-accounts having their own addresses is exactly why the old
/// symbol-collision rule was wrong.
#[test]
fn two_hyperliquid_sub_accounts_are_not_a_finding() {
    let v = vars(&[
        ("HYPERLIQUID_DEMO_PRIVATE_KEY", "0xmasterkey"),
        ("HYPERLIQUID_DEMO_ACCOUNT_ADDRESS", "0xMASTER"),
        ("HYPERLIQUID_DEMO_PRIVATE_KEY__ALT", "0xsubkey"),
        ("HYPERLIQUID_DEMO_ACCOUNT_ADDRESS__ALT", "0xsub"),
    ]);
    let rows = arming_rows("hyperliquid", &v);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.effective != VenueMode::Paper), "{rows:?}");
    assert!(
        vike_mount::shared_books_for("hyperliquid", &rows, &v).is_empty(),
        "two addresses are two books"
    );
}

/// **A VENUE WHOSE BOOK CANNOT BE DETERMINED OFFLINE SAYS NOTHING**, even with both accounts fully
/// credentialled and both armed. An unprovable suspicion is not a finding, and a placeholder
/// identity would compare equal to every other unknown of the venue and manufacture a pair out of
/// two unknowns.
#[test]
fn an_undeterminable_venue_reports_nothing_and_both_accounts_arm() {
    let v = vars(&[
        ("BYBIT_DEMO_API_KEY", "k"),
        ("BYBIT_DEMO_API_SECRET", "s"),
        ("BYBIT_DEMO_API_KEY__ALT", "k2"),
        ("BYBIT_DEMO_API_SECRET__ALT", "s2"),
    ]);
    let rows = arming_rows("bybit", &v);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows.iter().all(|r| r.effective != VenueMode::Paper), "both arm: {rows:?}");
    assert_eq!(rows[1].route_key(), "bybit#ALT", "…on its own engine");
    assert!(
        vike_mount::shared_books_for("bybit", &rows, &v).is_empty(),
        "bybit's store names no account, so there is nothing to report"
    );

    // …and it says nothing even when the two labels hold the IDENTICAL credential. That is a real
    // shared book and a real (if narrower) operator error, and this table deliberately declines to
    // detect it: the only offline discriminator is the SECRET, and the report prints the book.
    // Declared here rather than left to be discovered — see `book_identity`'s module doc.
    let same = vars(&[
        ("BYBIT_DEMO_API_KEY", "k"),
        ("BYBIT_DEMO_API_SECRET", "s"),
        ("BYBIT_DEMO_API_KEY__ALT", "k"),
        ("BYBIT_DEMO_API_SECRET__ALT", "s"),
    ]);
    let same_rows = arming_rows("bybit", &same);
    assert!(vike_mount::shared_books_for("bybit", &same_rows, &same).is_empty());
}

/// A PAPER account shares nothing — the paper exchange fills locally and touches no venue book. So
/// an unarmed second account cannot produce a finding however its keys are written, which is what
/// keeps a box with no `[accounts]` table permanently silent.
#[test]
fn a_paper_account_is_never_in_a_finding() {
    let v = vars(&[
        ("HYPERLIQUID_DEMO_PRIVATE_KEY", "0xmasterkey"),
        ("HYPERLIQUID_DEMO_ACCOUNT_ADDRESS", "0xMASTER"),
        ("HYPERLIQUID_DEMO_PRIVATE_KEY__ALT", "0xagentkey"),
        ("HYPERLIQUID_DEMO_ACCOUNT_ADDRESS__ALT", "0xmaster"),
    ]);
    // ALT gets NO line of its own, so it resolves paper (`AccountNotNamed`) while the default
    // account stays armed — the shape of every box that has a stray labelled key set.
    let policy = VenuePolicy::default().declare("hyperliquid", VenueMode::Demo);
    let rows = vike_mount::venue_account_arming("hyperliquid", &v, Some(&policy));
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[1].effective, VenueMode::Paper);
    assert!(vike_mount::shared_books_for("hyperliquid", &rows, &v).is_empty());
}

/// ⚠ **THE SPREAD, at the mount: two aster accounts on two masters, both armed, and no finding.**
///
/// The symbol is not merely ignored here — it is UNREPRESENTABLE. Neither `venue_account_arming`
/// nor `shared_books_for` takes one, and `vike_config::ArmedBook` has no field to hold one, so
/// there is no input to this whole path in which two accounts could be described as sharing an
/// instrument. The rule this replaced would have capped `ALT` to paper for exactly this arming.
///
/// Where the symbol DOES still live is `make_engine_accounts`' per-account map, which decides which
/// instrument each engine is MOUNTED on — a different question, and one with no uniqueness claim
/// attached (`crates/vike-core/src/runtime/mount_account_tests.rs` mounts two accounts on ONE
/// symbol and trades both).
#[test]
fn two_aster_accounts_on_two_masters_both_arm_with_no_finding() {
    let v = vars(&[
        ("ASTER_TESTNET_USER", "0xoneMaster"),
        ("ASTER_TESTNET_PRIVATE_KEY", "0xkey"),
        ("ASTER_TESTNET_USER__ALT", "0xotherMaster"),
        ("ASTER_TESTNET_PRIVATE_KEY__ALT", "0xkey2"),
    ]);
    let rows = vike_mount::venue_account_arming("aster", &v, Some(&both_armed("aster")));
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows.iter().all(|r| r.effective != VenueMode::Paper), "both arm: {rows:?}");
    assert_eq!(rows[1].route_key(), "aster#ALT", "…each on its own engine");
    assert!(vike_mount::shared_books_for("aster", &rows, &v).is_empty());

    // …and the SAME two accounts on ONE master ARE a finding, which is what makes the assertion
    // above a verdict rather than a rule that never fires.
    let mut one_master = v.clone();
    one_master.insert("ASTER_TESTNET_USER__ALT".into(), "0xONEMASTER".into());
    let rows = vike_mount::venue_account_arming("aster", &one_master, Some(&both_armed("aster")));
    let found = vike_mount::shared_books_for("aster", &rows, &one_master);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].book, "0xonemaster");
    assert!(rows.iter().all(|r| r.effective != VenueMode::Paper), "…and BOTH still arm: {rows:?}");
}

/// **THE ACCOUNT CEILING MULTIPLIES ON A SHARED BOOK, and the warning says so** —
/// `vike_mount::shared_book_ceiling_note`, the sentence `make_engine_accounts` appends to the
/// shared-book `warn!`.
///
/// The finding above tells an operator that two of their labels are one venue ledger. What it did
/// NOT tell them is the consequence for the newest ceiling: `vike_exec::RiskLimits`'s
/// `max_account_exposure` is armed once per ENGINE, and this shape is two engines over one book, so
/// the ledger can reach a MULTIPLE of the number they wrote before either engine refuses. That is
/// the same N×-looser defect the account axis exists to close, wearing the account label instead of
/// the symbol label — being told half of it is worse than being told none, because the half you are
/// told reads as the whole finding.
///
/// Asserted on the pure note rather than on the emission for the reason this file's own doc gives:
/// the `warn!` is structurally unreachable from a test. What the note must carry is the KEY (so the
/// operator knows which line to edit) and BOTH numbers — theirs, and what the shared book can
/// actually hold.
#[test]
fn the_shared_book_warning_names_the_account_ceiling_and_what_it_multiplies_to() {
    let note = vike_mount::shared_book_ceiling_note(Some(250_000.0));
    assert!(
        note.contains("max_account_exposure"),
        "the operator must be told WHICH key this is about: {note}"
    );
    assert!(note.contains("250000"), "…their own number, so they recognise it: {note}");
    assert!(
        note.contains("500000"),
        "…and what the ONE shared book can hold with the ceiling applied to each engine \
         separately, which is the whole finding: {note}"
    );
    assert!(
        note.contains("PER ENGINE"),
        "…and WHY, in the words the ceiling's own docs use: {note}"
    );
}

/// **…and an UNARMED ceiling adds nothing at all**, so a deployment that wrote no
/// `max_account_exposure` line reads exactly the shared-book warning it always read.
///
/// The byte-identical claim for this axis, at the one site where it is a STRING rather than a
/// verdict: an empty note concatenates to nothing, so the emitted message is unchanged.
#[test]
fn an_unarmed_account_ceiling_adds_nothing_to_the_shared_book_warning() {
    assert_eq!(
        vike_mount::shared_book_ceiling_note(None),
        "",
        "no ceiling ⇒ no sentence ⇒ the warning is the one every existing deployment sees"
    );
}
