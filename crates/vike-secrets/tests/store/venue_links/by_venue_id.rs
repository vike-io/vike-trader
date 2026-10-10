//! Every reader and writer answers from venue_id when the text column lies.

use super::*;
use std::assert_matches;

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
    assert_matches!(
        err.kind,
        vike_secrets::DbErrorKind::AccountLabelTaken { .. },
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
    assert_matches!(
        err.kind,
        DbErrorKind::AmbiguousUnlabelledAccount { .. },
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
    assert_matches!(
        err.kind,
        DbErrorKind::AccountLabelHeldAsBook { .. },
        "the guard must find the book's holder through `venue_id`, got {err}"
    );
}

/// `set_venue_account_id`'s holder check: one book, two accounts of one venue, refused BY NAME.
///
/// ⚠ This writer runs NO write funnel — it is one `UPDATE` of one row, and the venue handshake
/// reaches it at a daemon's boot — and it answers from the number like every other check.
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
    assert_matches!(
        err.kind, DbErrorKind::BookHeldByAnother { holder, .. } if holder == demo,
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
    assert_matches!(
        err.kind, DbErrorKind::BookHeldByAnother { holder, .. } if holder == live,
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

/// Every venue-scoped credential row, alias rows included, carries the venue's number.
///
/// ⚠ `populated` keeps it from passing on an empty question: the shared fixture files no
/// venue-scoped credential, so [`planted`] adds one, and this asserts the row is there.
///
/// ⚠ **The number is ALL such a row carries**: `credential` has no text venue. What is left to
/// ask is that the row [`planted`] filed for `ctrader` names `ctrader` by its number — the only
/// place that venue is written down — and that no other row of the table names a venue at all
/// (every other credential the fixture files is account-scoped).
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

/// **`account`, `credential` and `venue_setting` carry no text venue at all, and every reader still
/// answers** — asked of a store born on the shipped batch, through the readers.
#[test]
fn the_shipped_shape_carries_no_text_venue_and_readers_still_answer() {
    let fx = planted();
    let conn = fx.conn();
    for table in ["account", "credential", "venue_setting"] {
        assert!(!has_text_venue(&conn, table), "`{table}` must carry no text venue column");
    }
    assert!(has_text_venue(&conn, "venue_arming"), "…and `venue_arming` keeps its one");
    let mut venues: Vec<String> = fx.accounts().into_iter().map(|a| a.venue).collect();
    venues.sort();
    assert_eq!(venues, ["binance", "binance", "dukascopy"]);
    let rows = settings_rows(&fx);
    assert_eq!(rows.venue.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(), ["polymarket"]);
    assert_eq!(rows.arming.iter().map(|r| r.venue.as_str()).collect::<Vec<_>>(), ["binance"]);
}

/// **`credential`'s scope check** (`CHECK (account_id IS NULL OR venue_id IS NULL)`), asked of the
/// table the engine holds: it must REFUSE a row naming both an account and a venue, and ADMIT each
/// of the three kinds §5.1 allows.
///
/// ⚠ The DDL gate cannot hold it: its drift key is `credential.check:account_id,venue_id` on both
/// sides (§3's exactly-one check names the same two columns), so deleting the DDL's check changes
/// no key and the gate stays green. This is the behavioural guard.
#[test]
fn credentials_scope_check_refuses_both_and_admits_each_kind() {
    let fx = planted();
    let account = fx.id_of("binance", "demo");
    let conn = fx.conn();
    let insert = |account_id: Option<i64>, venue: Option<&str>, name: &str| {
        conn.execute(
            "INSERT INTO credential (account_id, venue_id, field, value, name) \
             VALUES (?1, (SELECT id FROM venue WHERE name = ?2), 'F', 'v', ?3)",
            (account_id, venue, name),
        )
    };
    let both =
        insert(Some(account), Some("binance"), "check-both").expect_err("both set must refuse");
    assert!(
        both.to_string().contains("CHECK constraint failed"),
        "the scope check refuses: {both}"
    );
    insert(Some(account), None, "check-account").expect("an account-scoped row is admitted");
    insert(None, Some("okx"), "check-venue").expect("a venue-scoped row is admitted");
    insert(None, None, "check-infrastructure").expect("an infrastructure row is admitted");
}

/// **A venue-scoped key filed under a venue the roster does not hold is REFUSED by name, not filed as
/// infrastructure.** A venue-scoped `credential` row names its venue by number alone, looked up by
/// name in the INSERT itself, so a venue the `venue` table lacks would get a NULL number and no text
/// — a row naming no venue, which every reader takes for an infrastructure key. Driven through the
/// credential door a rotating grant persists through, where a refusal is an error naming the key.
#[test]
fn a_venue_scoped_key_for_a_venue_off_the_roster_is_refused_by_name() {
    let fx = Fixture::seeded();
    let key = "an-off-roster-venue-key";
    let classify = |name: &str| -> vike_secrets::Classification {
        if name == key {
            return vike_secrets::Classification {
                placement: vike_secrets::Placement::Venue("no-such-venue".to_string()),
                field: "API_KEY".to_string(),
                secret: true,
                recognised: true,
            };
        }
        crate::support::classify(name)
    };
    let err = vike_secrets::save_credentials_to_store(
        fx.dir(),
        vike_secrets::Table::Credential,
        &[(key.to_string(), "an-off-roster-value".to_string())],
        Some(&classify),
    )
    .expect_err("a key naming no roster venue must not land");
    // The door returns the store's refusal as text (`std::io::Error`), so the refusal is asked of
    // its rendering: `SchemaRefusal::VenueNotOnRoster`'s, which names the key and the venue.
    let text = err.to_string();
    assert!(
        text.contains(&format!(
            "{key}: the classifier filed this key under venue \"no-such-venue\", which this store's \
             venue table does not hold"
        )),
        "refused by name, as the venue it was filed under: {text}"
    );
    assert!(text.contains("NOTHING WAS WRITTEN"), "…saying nothing landed: {text}");
    assert!(!text.contains("an-off-roster-value"), "…never by value: {text}");
    let conn = fx.conn();
    let filed: i64 = conn
        .query_row("SELECT COUNT(*) FROM credential WHERE name = ?1", [key], |r| r.get(0))
        .expect("count");
    assert_eq!(filed, 0, "…and nothing was filed, as infrastructure or otherwise");
}
