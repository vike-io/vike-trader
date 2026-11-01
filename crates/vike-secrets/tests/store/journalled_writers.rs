//! **The public writers record their change by construction, and the record is the one the
//! hand-composed call sites wrote.**
//!
//! Before the journalled writers became THE writers, each caller wrote the store and then built its
//! own `Change` beside it: `vike-cli secrets set`'s `record_write`, `node`'s
//! `record_credential_write`, `secrets account`'s `record_account_lifecycle`, `secrets book`'s
//! `record_book_write`, the tradehub wire's `journal_*` and ctrader's `record_rotation`. Each test
//! here performs ONE write through the journalled writer and compares the line it appended with the
//! line the hand-composed record of the SAME act appends through `ChangeJournal::append` — every
//! field but `seq` (a per-process tiebreaker that differs between any two appends by design).

use std::path::Path;

use vike_change_journal::{
    Actor, CHANGES_SUBDIR, Change, ChangeJournal, Outcome, Proc, month_file_name,
};
use vike_secrets::{
    AccountEdit, AccountJournal, BookSource, CredentialJournal, CredentialOwner, Table,
    UpsertIntent,
};

use crate::support::{self, Fixture};

/// Two DEPLOYMENT names a name-addressed write files — what `vike-cli secrets set` writes by name.
const DEPLOYMENT_KEYS: [&str; 2] = ["CLOUDFLARE_API_TOKEN", "CLOUDFLARE_ZONE_ID"];

/// 2026-08-21T00:00:00Z — the anchor the change journal's own suite stamps.
const T: i64 = 1_787_356_800_000;

fn proc() -> Proc {
    Proc::new("vike-cli", 4242, "9.9.9")
}

/// The one line the journal beside `settings` holds, as JSON with `seq` removed.
fn the_line(settings: &Path) -> serde_json::Value {
    let ledger = settings.join("state").join(CHANGES_SUBDIR).join(month_file_name(T));
    let raw = std::fs::read_to_string(&ledger).expect("the change journal was written");
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one record: {raw}");
    let mut v: serde_json::Value = serde_json::from_str(lines[0]).expect("one JSON object");
    v.as_object_mut().expect("an object").remove("seq");
    v
}

/// The line the HAND-COMPOSED record of the same act appends, in a journal of its own.
fn hand_line(change: &Change) -> serde_json::Value {
    let tmp = tempfile::tempdir().expect("tempdir");
    let settings = tmp.path().join("settings");
    ChangeJournal::in_state_dir(&settings.join("state"), proc())
        .append(T, change)
        .expect("the hand-composed append lands");
    the_line(&settings)
}

/// No journal file beside `settings` at all.
fn no_ledger(settings: &Path) -> bool {
    !settings.join("state").join(CHANGES_SUBDIR).join(month_file_name(T)).exists()
}

/// `vike-cli secrets set`'s record: a credential write of ONE key, with its venue and tier.
#[test]
fn the_credential_write_record_is_the_one_secrets_set_composed() {
    let fx = Fixture::initialised();
    let key = DEPLOYMENT_KEYS[0];
    let (_, journal_error) = vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &[(key.to_string(), support::fake_value(key))],
        UpsertIntent::Add,
        CredentialJournal {
            actor: Actor::cli("vike-cli"),
            venue: "binance",
            tier: "DEMO",
            proc: proc(),
            now_ms: T,
        },
    )
    .expect("the write lands");
    assert!(journal_error.is_none(), "{journal_error:?}");
    let hand = Change::credential_write(
        Outcome::Applied,
        Actor::cli("vike-cli"),
        vike_secrets::DB_FILE,
        "binance",
        "DEMO",
        &[key],
    );
    assert_eq!(the_line(fx.dir()), hand_line(&hand));
    let raw = serde_json::to_string(&the_line(fx.dir())).expect("json");
    assert!(!raw.contains(&support::fake_value(key)), "a write journals no value: {raw}");
}

/// The hub-key and tradehub-wire shapes: the record carries whatever venue and tier the caller
/// names — `""`/`""` for a hub key, `multi`/`untiered` for a write the wire cannot decompose — and
/// ctrader's labelled rotation, which names tier `""`, is the same arm.
#[test]
fn the_venue_and_tier_cells_are_the_callers_verbatim() {
    use vike_change_journal::{TIER_UNTIERED, VENUE_MULTI};
    for (venue, tier) in [("", ""), (VENUE_MULTI, TIER_UNTIERED), ("ctrader", "")] {
        let fx = Fixture::initialised();
        let keys = DEPLOYMENT_KEYS;
        let rows: Vec<(String, String)> =
            keys.iter().map(|k| (k.to_string(), support::fake_value(k))).collect();
        let (_, journal_error) = vike_secrets::save_credentials_to_store(
            fx.dir(),
            Table::Credential,
            &rows,
            UpsertIntent::Add,
            CredentialJournal {
                actor: Actor::venue("ctrader"),
                venue,
                tier,
                proc: proc(),
                now_ms: T,
            },
        )
        .expect("the write lands");
        assert!(journal_error.is_none(), "{journal_error:?}");
        let hand = Change::credential_write(
            Outcome::Applied,
            Actor::venue("ctrader"),
            vike_secrets::DB_FILE,
            venue,
            tier,
            &keys,
        );
        assert_eq!(the_line(fx.dir()), hand_line(&hand), "{venue:?}/{tier:?}");
    }
}

/// **The owner-addressed writer's record** (decision 0156 item 6): a `credential_write` naming the
/// NAMES written — the writer's own for a new field, the stored one for a field the account held —
/// the ACCOUNT's venue and its tier WORD (`AccountTier::as_str`), never a value; a venue's
/// application row records `TIER_UNTIERED`. Each is the line the same `Change` composed by hand
/// appends.
#[test]
fn the_owned_credential_write_record_names_the_names_the_venue_and_the_tier_word() {
    use vike_change_journal::TIER_UNTIERED;
    let fx = Fixture::initialised();
    let id = fx.create_account("bybit", vike_model::AccountTier::Live, Some("HEDGE"));
    fx.plant_legacy_credential(id, "BYBIT_LIVE_API_SECRET__HEDGE", "API_SECRET");
    let (done, journal_error) = vike_secrets::save_owned_credentials(
        fx.dir(),
        &CredentialOwner::Account(id),
        &[
            ("API_KEY".to_string(), "value-never-journalled".to_string()),
            ("API_SECRET".to_string(), "secret-never-journalled".to_string()),
        ],
        UpsertIntent::Add,
        AccountJournal { actor: Actor::cli("vike-cli"), proc: proc(), now_ms: T },
    )
    .expect("the write lands");
    assert!(journal_error.is_none(), "{journal_error:?}");
    let new_name = vike_secrets::account_credential_name("bybit", id, "API_KEY");
    assert_eq!(done.names, [new_name.as_str(), "BYBIT_LIVE_API_SECRET__HEDGE"]);
    let hand = Change::credential_write(
        Outcome::Applied,
        Actor::cli("vike-cli"),
        vike_secrets::DB_FILE,
        "bybit",
        vike_model::AccountTier::Live.as_str(),
        &[new_name.as_str(), "BYBIT_LIVE_API_SECRET__HEDGE"],
    );
    assert_eq!(the_line(fx.dir()), hand_line(&hand));
    let raw = serde_json::to_string(&the_line(fx.dir())).expect("json");
    assert!(!raw.contains("never-journalled"), "a write journals no value: {raw}");

    // A venue's application row: the venue, and no tier.
    let fx = Fixture::initialised();
    let (done, journal_error) = vike_secrets::save_owned_credentials(
        fx.dir(),
        &CredentialOwner::Venue("ctrader".to_string()),
        &[("CLIENT_ID".to_string(), "an-app-id".to_string())],
        UpsertIntent::Add,
        AccountJournal { actor: Actor::cli("vike-cli"), proc: proc(), now_ms: T },
    )
    .expect("the write lands");
    assert!(journal_error.is_none(), "{journal_error:?}");
    let names: Vec<&str> = done.names.iter().map(String::as_str).collect();
    let hand = Change::credential_write(
        Outcome::Applied,
        Actor::cli("vike-cli"),
        vike_secrets::DB_FILE,
        "ctrader",
        TIER_UNTIERED,
        &names,
    );
    assert_eq!(the_line(fx.dir()), hand_line(&hand));
}

/// A REFUSED owner-addressed write — an account no row carries — writes no record, and creates no
/// account.
#[test]
fn a_refused_owned_credential_write_records_nothing() {
    let fx = Fixture::initialised();
    let refused = vike_secrets::save_owned_credentials(
        fx.dir(),
        &CredentialOwner::Account(4242),
        &[("API_KEY".to_string(), "v".to_string())],
        UpsertIntent::Add,
        support::account_journal(),
    );
    assert!(refused.is_err());
    assert!(no_ledger(fx.dir()), "a write that did not land must not leave a record");
    assert!(fx.accounts().is_empty(), "no account was created");
}

/// A REFUSED credential write writes no record (the write did not land).
#[test]
fn a_refused_credential_write_records_nothing() {
    let fx = Fixture::initialised();
    let refused = vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &[("BINANCE_DEMO_API_KEY".to_string(), "two\nlines".to_string())],
        UpsertIntent::Add,
        support::credential_journal(),
    );
    assert!(refused.is_err());
    assert!(no_ledger(fx.dir()), "a write that did not land must not leave a record");
}

/// `vike-cli secrets account`'s and the tradehub wire's lifecycle record, for a create, a rename
/// and a deactivation; and NOTHING for an edit that changed nothing.
#[test]
fn the_lifecycle_record_is_the_one_the_account_verbs_composed() {
    let fx = Fixture::initialised();
    let journal = || AccountJournal { actor: Actor::cli("vike-cli"), proc: proc(), now_ms: T };

    let (done, journal_error) = vike_secrets::edit_account_in(
        fx.dir(),
        AccountEdit::Create {
            venue: "binance",
            tier: vike_model::AccountTier::Demo,
            label: Some("ALT"),
        },
        journal(),
    )
    .expect("create");
    assert!(journal_error.is_none(), "{journal_error:?}");
    assert!(done.changed);
    let row = done.after.as_ref().expect("a created row");
    let keys: Vec<&str> = done.keys.iter().map(String::as_str).collect();
    let hand = Change::account_lifecycle(
        Outcome::Applied,
        Actor::cli("vike-cli"),
        vike_secrets::DB_FILE,
        done.verb,
        row.id,
        &row.venue,
        row.tier,
        done.before.as_ref().and_then(|b| b.label.as_deref()),
        done.after.as_ref().and_then(|a| a.label.as_deref()),
        done.after.as_ref().is_some_and(|a| a.active),
        &keys,
    );
    assert_eq!(the_line(fx.dir()), hand_line(&hand));

    // Repeating the deactivation of an inactive row changes nothing — and records nothing more.
    let id = row.id;
    let (first, _) = vike_secrets::edit_account_in(
        fx.dir(),
        AccountEdit::SetActive { id, active: false },
        AccountJournal { now_ms: T, ..journal() },
    )
    .expect("deactivate");
    assert!(first.changed);
    let ledger = fx.dir().join("state").join(CHANGES_SUBDIR).join(month_file_name(T));
    let lines_before = std::fs::read_to_string(&ledger).expect("ledger").lines().count();
    let (again, journal_error) = vike_secrets::edit_account_in(
        fx.dir(),
        AccountEdit::SetActive { id, active: false },
        journal(),
    )
    .expect("a repeat is not a refusal");
    assert!(!again.changed, "nothing changed");
    assert!(journal_error.is_none());
    assert_eq!(
        std::fs::read_to_string(&ledger).expect("ledger").lines().count(),
        lines_before,
        "a no-op edit leaves no ledger line"
    );
}

/// `vike-cli secrets book`'s and the tradehub wire's book record — and a refused book write, and a
/// write that changed nothing, record nothing.
#[test]
fn the_book_record_is_the_one_the_book_verbs_composed() {
    let fx = Fixture::initialised();
    let (created, _) = vike_secrets::edit_account_in(
        fx.dir(),
        AccountEdit::Create {
            venue: "dukascopy",
            tier: vike_model::AccountTier::Demo,
            label: None,
        },
        AccountJournal { actor: Actor::cli("vike-cli"), proc: proc(), now_ms: T - 1 },
    )
    .expect("create");
    let id = created.after.as_ref().expect("row").id;
    // The create's record lands in the same month file; start the comparison from a clean one.
    let ledger = fx.dir().join("state").join(CHANGES_SUBDIR).join(month_file_name(T));
    std::fs::remove_file(&ledger).expect("clear the create's record");

    let (done, journal_error) = vike_secrets::set_venue_account_id_in(
        fx.dir(),
        id,
        Some("12345"),
        false,
        BookSource::Operator,
        AccountJournal { actor: Actor::cli("vike-cli"), proc: proc(), now_ms: T },
    )
    .expect("the book lands");
    assert!(journal_error.is_none(), "{journal_error:?}");
    assert!(done.changed);
    let hand = Change::account_book(
        Outcome::Applied,
        Actor::cli("vike-cli"),
        vike_secrets::DB_FILE,
        done.before.id,
        &done.before.venue,
        done.before.tier,
        done.before.venue_account_id.as_deref(),
        done.venue_account_id.as_deref(),
    );
    assert_eq!(the_line(fx.dir()), hand_line(&hand));

    // The same book again changes nothing: no second line.
    let (again, _) = vike_secrets::set_venue_account_id_in(
        fx.dir(),
        id,
        Some("12345"),
        false,
        BookSource::Operator,
        support::account_journal(),
    )
    .expect("a repeat is not a refusal");
    assert!(!again.changed);
    assert_eq!(std::fs::read_to_string(&ledger).expect("ledger").lines().count(), 1);

    // A refused write (no such row) records nothing either.
    let refused = vike_secrets::set_venue_account_id_in(
        fx.dir(),
        id + 1000,
        Some("999"),
        false,
        BookSource::Operator,
        support::account_journal(),
    );
    assert!(refused.is_err());
    assert_eq!(std::fs::read_to_string(&ledger).expect("ledger").lines().count(), 1);
}

/// A box with no settings database: every journalled writer REFUSES, creates nothing, and records
/// nothing — no journal directory appears beside a store that is not there.
#[test]
fn with_no_store_every_writer_refuses_and_records_nothing() {
    let fx = Fixture::empty();
    assert!(
        vike_secrets::save_credentials_to_store(
            fx.dir(),
            Table::Credential,
            &[("BINANCE_DEMO_API_KEY".to_string(), "v".to_string())],
            UpsertIntent::Add,
            support::credential_journal(),
        )
        .is_err()
    );
    assert!(
        vike_secrets::retire_credential_in_store(
            fx.dir(),
            Table::Credential,
            "BINANCE_DEMO_API_KEY",
            support::credential_journal(),
        )
        .is_err()
    );
    assert!(
        vike_secrets::save_owned_credentials(
            fx.dir(),
            &CredentialOwner::Account(1),
            &[("API_KEY".to_string(), "v".to_string())],
            UpsertIntent::Add,
            support::account_journal(),
        )
        .is_err()
    );
    assert!(
        vike_secrets::edit_account_in(
            fx.dir(),
            AccountEdit::SetActive { id: 1, active: true },
            support::account_journal(),
        )
        .is_err()
    );
    assert!(
        vike_secrets::set_venue_account_id_in(
            fx.dir(),
            1,
            Some("1"),
            false,
            BookSource::Operator,
            support::account_journal(),
        )
        .is_err()
    );
    assert!(!fx.db().exists(), "no database was created");
    assert!(!fx.dir().join("state").exists(), "no journal was started beside a missing store");
}
