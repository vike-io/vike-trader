//! Every reader and writer answers from venue_id when the text column lies.

use super::*;

#[test]
fn the_account_reader_answers_from_venue_id_when_the_text_column_lies() {
    let fx = planted();
    let honest: Vec<(i64, String)> = fx.accounts().into_iter().map(|a| (a.id, a.venue)).collect();
    falsify_the_text_column(&fx);

    let read: Vec<(i64, String)> = fx.accounts().into_iter().map(|a| (a.id, a.venue)).collect();
    assert_eq!(read, honest, "`read_accounts` must name each account's venue from `venue_id`");
    // ⚠ …and the LITERAL venues, because the comparison above is against the reader's OWN pre-lie
    // read: a reader that answered wrongly both times would agree with itself.
    assert_eq!(
        read.iter().map(|(_, venue)| venue.as_str()).collect::<Vec<_>>(),
        ["binance", "binance", "dukascopy"],
        "the planted accounts are binance demo, binance live and dukascopy demo"
    );
}

#[test]
fn the_arming_reader_answers_from_venue_id_when_the_text_column_lies() {
    let fx = planted();
    falsify_the_text_column(&fx);

    let rows = settings_rows(&fx);
    assert_eq!(
        rows.arming.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(),
        ["binance"],
        "the arming row's venue must come from `venue_id`"
    );
}

#[test]
fn the_venue_setting_reader_answers_from_venue_id_when_the_text_column_lies() {
    let fx = planted();
    falsify_the_text_column(&fx);

    let rows = settings_rows(&fx);
    assert_eq!(
        rows.venue.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(),
        ["polymarket"],
        "the venue setting's venue must come from `venue_id`"
    );
}

#[test]
fn the_label_guard_finds_the_other_account_by_venue_id() {
    let fx = planted();
    fx.edit(AccountEdit::Create { venue: "binance", tier: "demo", label: Some("HEDGE") })
        .expect("the first HEDGE");
    falsify_the_text_column(&fx);

    let second =
        fx.edit(AccountEdit::Create { venue: "binance", tier: "demo", label: Some("HEDGE") });
    let err = second.expect_err("a second binance/demo/HEDGE must be refused");
    assert!(
        matches!(err.kind, vike_secrets::DbErrorKind::AccountLabelTaken { .. }),
        "the guard must find the first HEDGE through `venue_id`, got {err}"
    );
}

#[test]
fn a_venue_setting_rewrite_finds_its_row_by_venue_id() {
    let fx = planted();
    falsify_the_text_column(&fx);

    let previous =
        vike_secrets::set_venue_setting_in(fx.dir(), "polymarket", None, "PROXY_ENABLED", "true")
            .expect("the rewrite");
    assert_eq!(
        previous.as_deref(),
        Some("false"),
        "the writer must find the existing row by `venue_id` and report its old value"
    );
}

/// The label guard's UNLABELLED branch: a second unlabelled `(binance, demo)` row is the ambiguity
/// no index can refuse (NULL labels are distinct), so only the guard's own query stands in front of
/// it, and that query must find the first row by its number.
#[test]
fn the_unlabelled_guard_finds_the_other_account_by_venue_id() {
    let fx = planted();
    falsify_the_text_column(&fx);

    let second = fx.edit(AccountEdit::Create { venue: "binance", tier: "demo", label: None });
    let err = second.expect_err("a second unlabelled binance/demo must be refused");
    assert!(
        matches!(err.kind, DbErrorKind::AmbiguousUnlabelledAccount { .. }),
        "the guard must find the unlabelled binance/demo row through `venue_id`, got {err}"
    );
}

/// The label guard's BOOK branch: a label equal to an active account's book at the same venue is
/// refused, because a mount resolves a policy address against the book first.
#[test]
fn the_book_guard_finds_the_label_held_as_a_book_by_venue_id() {
    let fx = planted();
    let demo = fx.id_of("binance", "demo");
    vike_secrets::set_venue_account_id_in(
        fx.dir(),
        demo,
        Some("HEDGE"),
        false,
        BookSource::Operator,
    )
    .expect("the demo account's book");
    falsify_the_text_column(&fx);

    let labelled =
        fx.edit(AccountEdit::Create { venue: "binance", tier: "live", label: Some("HEDGE") });
    let err = labelled.expect_err("a label that is another binance account's book must be refused");
    assert!(
        matches!(err.kind, DbErrorKind::AccountLabelHeldAsBook { .. }),
        "the guard must find the book's holder through `venue_id`, got {err}"
    );
}

/// `set_venue_account_id`'s holder check: one book, two accounts of one venue, refused BY NAME.
///
/// ⚠ This writer runs NO repair funnel — it is one `UPDATE` of one row, and the venue handshake
/// reaches it at a daemon's boot — so its check also has to answer on a store no writer has carried
/// yet. On a carried store, which this is, it answers from the number like every other check.
#[test]
fn setting_a_book_finds_its_holder_by_venue_id() {
    let fx = planted();
    let demo = fx.id_of("binance", "demo");
    let live = fx.id_of("binance", "live");
    vike_secrets::set_venue_account_id_in(fx.dir(), demo, Some("B1"), false, BookSource::Operator)
        .expect("the demo account's book");
    falsify_the_text_column(&fx);

    let err = vike_secrets::set_venue_account_id_in(
        fx.dir(),
        live,
        Some("B1"),
        false,
        BookSource::Operator,
    )
    .expect_err("a book another active binance account names must be refused");
    assert!(
        matches!(err.kind, DbErrorKind::BookHeldByAnother { holder, .. } if holder == demo),
        "the check must find the demo account through `venue_id` and name it, got {err}"
    );
}

/// A RE-ACTIVATION's holder check: while the row was off, another row of its venue took its book.
#[test]
fn reactivating_finds_the_book_holder_by_venue_id() {
    let fx = planted();
    let demo = fx.id_of("binance", "demo");
    let live = fx.id_of("binance", "live");
    vike_secrets::set_venue_account_id_in(fx.dir(), demo, Some("B1"), false, BookSource::Operator)
        .expect("the demo account's book");
    fx.edit(AccountEdit::SetActive { id: demo, active: false }).expect("deactivate it");
    vike_secrets::set_venue_account_id_in(fx.dir(), live, Some("B1"), false, BookSource::Operator)
        .expect("an inactive row frees its book");
    falsify_the_text_column(&fx);

    let err = fx
        .edit(AccountEdit::SetActive { id: demo, active: true })
        .expect_err("re-activating onto a book another active binance account holds is refused");
    assert!(
        matches!(err.kind, DbErrorKind::BookHeldByAnother { holder, .. } if holder == live),
        "the check must find the live account through `venue_id` and name it, got {err}"
    );
}

/// The one-row arming writer decides UPDATE-or-INSERT from the rows it reads, then `UPDATE`s by
/// venue and label. Both halves must name the venue by number, or the write either lands nowhere
/// or collides with the row it should have changed.
#[test]
fn an_arming_rewrite_finds_its_row_by_venue_id() {
    let fx = planted();
    falsify_the_text_column(&fx);

    let written = vike_secrets::write_setting_row_in(
        fx.dir(),
        vike_secrets::RowChange::Arming {
            venue: "binance".to_string(),
            label: None,
            mode: "live".to_string(),
        },
        std::time::Duration::from_secs(3),
        |_, _, _| Ok(()),
    )
    .expect("the arming rewrite");
    assert_eq!(written.old_value.as_deref(), Some("demo"), "it found the row it replaced");

    let arming: Vec<(String, String)> =
        settings_rows(&fx).arming.into_iter().map(|r| (r.venue, r.mode)).collect();
    assert_eq!(
        arming,
        [("binance".to_string(), "live".to_string())],
        "…and the UPDATE landed on that row, not on nothing and not beside it"
    );
}

/// Decision 0095's migration reads the arming rows by number: a lying text column must not hide a
/// pre-0095 `live` ceiling of a switched venue, which a later read would take as MAINNET.
#[test]
fn a_lying_text_column_does_not_hide_a_live_ceiling_from_decision_0095() {
    let fx = planted();
    vike_secrets::write_settings_in(
        fx.dir(),
        &StoredSettings {
            arming: vec![ArmingRow {
                venue: "binance".to_string(),
                label: None,
                mode: "live".to_string(),
                max_exposure: None,
            }],
            ..Default::default()
        },
    )
    .expect("a binance `live` ceiling");
    vike_secrets::live_means_mainnet::unmark_live_means_mainnet(fx.dir());
    falsify_the_text_column(&fx);

    assert!(
        vike_secrets::live_means_mainnet::live_means_mainnet_pending(fx.dir()).expect("the probe"),
        "the read-only probe must see binance's `live` row through `venue_id`"
    );
    let vike_secrets::live_means_mainnet::LiveMeansMainnet::Applied { rewrites, .. } =
        apply_0095(&fx)
    else {
        panic!("a pending store is migrated");
    };
    assert_eq!(rewrites.iter().map(|r| r.key()).collect::<Vec<_>>(), ["policy.venues.binance"]);
    assert_eq!(
        settings_rows(&fx).arming.iter().map(|r| r.mode.as_str()).collect::<Vec<_>>(),
        ["demo"],
        "…and the row it reported is the row it rewrote"
    );
}

/// Every venue-scoped credential row, alias rows included, carries the venue's number.
///
/// ⚠ `populated` keeps it from passing on an empty question: the shared fixture files no
/// venue-scoped credential, so [`planted`] adds one, and this asserts the row is there.
///
/// ⚠ **Since the plan's second release the number is ALL such a row carries**: `credential` has no
/// text venue, so "a text venue with no number" cannot be asked of it any more. What is left to ask
/// is that the row [`planted`] filed for `ctrader` names `ctrader` by its number — the only place
/// that venue is written down — and that no other row of the table names a venue at all (every
/// other credential the fixture files is account-scoped).
#[test]
fn every_venue_scoped_credential_row_names_its_venue_by_number() {
    let fx = planted();
    let conn = fx.conn();
    let mut stmt = conn
        .prepare(
            "SELECT c.name, v.name FROM credential c JOIN venue v ON v.id = c.venue_id \
             ORDER BY c.name",
        )
        .expect("prepare");
    let named: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        named,
        [("CTRADER_CLIENT_ID".to_string(), "ctrader".to_string())],
        "the fixture's one venue-scoped credential names its venue by number, and nothing else \
         names one"
    );
}
