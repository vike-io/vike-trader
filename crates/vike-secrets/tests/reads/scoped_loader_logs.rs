//! **The ONE place a store finding becomes a log line** — `Resolved::report` and
//! `ScopedSecrets::report` (one body, `report_findings`), and the `warn_once` they log through.
//!
//! `scoped_read.rs` holds the DATA half (the finding survives the narrowing). This file holds the
//! half that LOGS: the reads themselves log nothing, and a caller that reports what a read found
//! must get the same lines, at the same levels, whichever read shape it took.
//!
//! | test | the property |
//! |---|---|
//! | [`an_exposed_store_is_logged_once_by_both_reports_and_the_shared_dedupe`] | the permission finding reaches the log from either report, exactly once per process, and `warn_once` shares that one dedupe set |
//! | [`a_clean_store_logs_nothing_and_no_read_logs_at_all`] | a 0600 store is silent: no finding, no line — and a READ never logs, whatever it found |
//! | [`an_unreadable_store_is_loud_in_the_report_and_the_live_gate_in_the_answer`] | a store that EXISTS and will not open is an ERROR line and an empty answer whose declared names read as the live gate, never a silent empty answer |
//! | [`the_no_live_keys_scope_reports_how_many_names_it_withheld`] | the scope's one `warn` line, its exact text pinned |
//!
//! ⚠ **Key NAMES only, and no credential name is spelled in this file**: every name comes from the
//! shared fixture's accounts (`support::Fixture::key`) or `support::FIXTURE_DEPLOYMENT_KEY`, so
//! `crates/vike-model/src/scan.rs` finds no literal here to read as a lookup (the reason
//! `scoped_read.rs` gives for its own bindings). Every value is
//! `support::fake_value`'s derived string, and no assertion prints one.
//!
//! The capture is `vike_log::capture::captured`, the workspace's one scoped `tracing` capture
//! (`crates/vike-ops/tests/hygiene/tracing_capture_gate.rs`). The dedupe set is process-wide, which
//! is why each test seeds its OWN store: the message embeds the store's path, so a sibling test's
//! line cannot swallow this one's.

use tracing::Level;
use vike_log::capture::captured;
use vike_secrets::{CredentialScope, KeyScope, Lookup, Resolved, ScopedSecrets, Source};

use crate::support::{self, FIXTURE_DEPLOYMENT_KEY, Fixture};

/// One account's key and secret, both in the seeded store: the shared fixture's binance demo pair.
fn scope(fx: &Fixture) -> KeyScope {
    KeyScope::of([fx.key("binance", "demo", "API_KEY"), fx.key("binance", "demo", "API_SECRET")])
}

/// Make the store readable beyond its owner — the one finding there is. Unix only: the mode bits do
/// not exist elsewhere, and `permission_warning` answers `None` there by design.
#[cfg(unix)]
fn widen(fx: &Fixture) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(fx.db(), std::fs::Permissions::from_mode(0o644))
        .expect("widen the store's mode");
}

/// A database this code will not read: the schema stamp is not `SCHEMA_VERSION`.
fn unreadable() -> Fixture {
    let fx = Fixture::seeded();
    let conn = fx.conn();
    support::sql::stamp_version(&conn, vike_secrets::SCHEMA_VERSION + 1);
    drop(conn);
    fx
}

/// **The permission finding is logged, once per process, through the one dedupe.**
///
/// Three reports of the same exposed store in one capture, by every door the finding has: the
/// scoped read's report, the whole-map read's report, and a direct `warn_once` of the finding's
/// text. ONE line comes out of the three. Two dedupe sets would print it twice, and a report that
/// dropped the call would print it never; both redden this test.
#[cfg(unix)]
#[test]
fn an_exposed_store_is_logged_once_by_both_reports_and_the_shared_dedupe() {
    let fx = Fixture::seeded();
    widen(&fx);
    let store = support::store_in(fx.dir());

    let (finding, events) = captured(|| {
        let scoped = store.read(&scope(&fx)).expect("the store opens");
        let whole = store.read_all().expect("the store opens");
        assert_eq!(scoped.warning, whole.warning, "both reads carry the same finding as data");
        scoped.report();
        whole.report();
        let finding = scoped.warning.as_ref().expect("a 0644 store is reported").to_string();
        vike_secrets::warn_once(&finding);
        finding
    });

    assert_eq!(
        events.len(),
        1,
        "one finding, three doors, ONE line — a second dedupe set prints it twice and a report \
         that skipped the log call prints it never: {events:?}"
    );
    let line = &events[0];
    assert_eq!(line.level, Level::WARN);
    assert_eq!(line.message, finding, "the line is the finding's own text");
    assert!(line.message.contains("readable beyond its owner"), "{}", line.message);
    assert!(line.message.contains("vike.db"), "it names the store: {}", line.message);
    assert!(!line.message.contains("value-for-"), "paths and a mode, never a value");
}

/// A store only its owner can read has no finding, so its reports log nothing at all — and the
/// READS log nothing whatever they find: a finding is data until somebody reports it.
#[test]
fn a_clean_store_logs_nothing_and_no_read_logs_at_all() {
    let fx = Fixture::seeded();
    let store = support::store_in(fx.dir());
    let (_, events) = captured(|| {
        store.read(&scope(&fx)).expect("the store opens").report();
        store.read_all().expect("the store opens").report();
    });
    assert!(events.is_empty(), "a clean store is silent: {events:?}");

    let bad = unreadable();
    let narrow = support::scoped_store_in(bad.dir(), CredentialScope::NoLiveKeys);
    let (_, events) = captured(|| {
        let _ = narrow.read_all();
        let _ = narrow.read(&scope(&fx));
        let _ = narrow.present_names(&scope(&fx));
        let _ = narrow.accounts();
        let _ = narrow.account_keys();
        let _ = narrow.credential_book();
        let _ = narrow.account_field(
            "binance",
            &vike_model::accounts::account_keys::AccountLabel::Default,
            vike_model::AccountTier::Demo,
            "API_KEY",
        );
        let _ = narrow.hub_keys(|_| true);
    });
    assert!(events.is_empty(), "a READ never logs, not even an error: {events:?}");
}

/// **A store that EXISTS and will not open is loud, and its answer reads as the live gate.**
///
/// The read hands the error back; a caller that degrades builds the EMPTY answer that says so
/// (`ScopedSecrets::unreadable` / `Resolved::unreadable`): a declared name answers
/// `AbsentFromStore` (every venue stays paper) while an undeclared one still answers `NotDeclared`,
/// so the two failures stay distinguishable. Its report is the ERROR line that makes the
/// degradation visible — the same text for both read shapes.
#[test]
fn an_unreadable_store_is_loud_in_the_report_and_the_live_gate_in_the_answer() {
    let seeded = Fixture::seeded();
    let scope = scope(&seeded);
    let fx = unreadable();
    let store = support::store_in(fx.dir());

    let e = store.read(&scope).expect_err("the read hands the error back");
    let scoped = ScopedSecrets::unreadable(&scope, store.scope(), &e);
    assert_eq!(scoped.source, Source::None, "nothing answered");
    assert!(scoped.is_empty(), "and nothing is held");
    assert!(!scoped.health.is_readable(), "the verdict says so");
    let declared = seeded.key("binance", "demo", "API_KEY");
    assert_eq!(scoped.get(&declared), Lookup::AbsentFromStore, "declared: the live gate");
    assert!(scoped.get(FIXTURE_DEPLOYMENT_KEY).is_undeclared(), "undeclared stays a scope defect");

    let (_, events) = captured(|| scoped.report());
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].level, Level::ERROR);
    assert_eq!(
        events[0].message,
        format!(
            "credential store could not be opened, continuing with NO credentials (every venue \
             stays paper): {e}"
        ),
        "the line's text is pinned"
    );
    assert!(!events[0].message.contains("value-for-"), "{}", events[0].message);

    let whole = Resolved::unreadable(store.scope(), &store.read_all().expect_err("loud"));
    let (_, again) = captured(|| whole.report());
    assert_eq!(again.len(), 1, "{again:?}");
    assert_eq!(again[0].message, events[0].message, "one spelling for both read shapes");

    // Under the no-live-keys scope the same failure names the scope.
    let narrow = Resolved::unreadable(CredentialScope::NoLiveKeys, &e);
    let (_, events) = captured(|| narrow.report());
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].level, Level::ERROR);
    assert_eq!(
        events[0].message,
        format!(
            "credential store could not be opened under the demo-only scope, continuing with NO \
             credentials: {e}"
        )
    );
}

/// **The no-live-keys scope reports how many names it withheld** — ONE `warn` line per report, its
/// text pinned. The fixture's live account owns one name, and it is the one withheld.
#[test]
fn the_no_live_keys_scope_reports_how_many_names_it_withheld() {
    let fx = Fixture::seeded();
    let read = support::scoped_store_in(fx.dir(), CredentialScope::NoLiveKeys)
        .read_all()
        .expect("the store opens");
    assert_eq!(read.withheld, 1, "the fixture's live account owns one name");
    let (_, events) = captured(|| read.report());
    let lines: Vec<&str> = events.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(
        lines,
        ["DEMO-ONLY credential scope (VIKE_NO_LIVE_KEYS=true): the store withheld 1 \
             name(s) of live-tier accounts and of aster/polymarket; a request for any of them \
             reads as absent"],
        "{events:?}"
    );
    assert_eq!(events[0].level, Level::WARN);

    // The Full scope reports no such line.
    let full = support::store_in(fx.dir()).read_all().expect("the store opens");
    let (_, events) = captured(|| full.report());
    assert!(events.is_empty(), "{events:?}");
}
