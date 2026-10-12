//! The write INTENTS and the DELETE (decision 0144): `UpsertIntent::Replace` refuses a name the
//! store does not hold and names it, `UpsertIntent::Add` files a new name where the real classifier
//! says, a replace leaves a row's placement alone — and `retire_credential_in_store` retires ONE
//! name so that every reader reads it as absent, keeps the row, and lets the name come back.

use super::*;
use vike_secrets::UpsertIntent;

/// A name no fixture here seeds — the subject of the Replace refusal.
const NEW_NAME: &str = "OKX_DEMO_API_SECRET";
/// The binance demo pair every delete case starts from, written through the PRODUCTION writer.
const KEY: &str = "BINANCE_DEMO_API_KEY";
const SECRET: &str = "BINANCE_DEMO_API_SECRET";
/// A distinctive value, so a leak into a message is a substring nothing else contains.
const KEY_VALUE: &str = "kv-never-printed-7Qx";

/// An EMPTY store, then the binance demo pair through `UpsertIntent::Add` — the real classifier.
fn binance_demo() -> crate::support::Fixture {
    let fx = crate::support::Fixture::initialised();
    vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &[(KEY.to_string(), KEY_VALUE.to_string()), (SECRET.to_string(), "sv-1".to_string())],
        UpsertIntent::Add,
    )
    .expect("an Add of two grid names lands");
    fx
}

/// Every credential row as `(id, name, value, account_id, venue_id, field, superseded_at)`.
type Row = (i64, String, String, Option<i64>, Option<i64>, String, Option<String>);

fn rows(fx: &crate::support::Fixture) -> Vec<Row> {
    let conn = fx.conn();
    let mut stmt = conn
        .prepare(
            "SELECT id, name, value, account_id, venue_id, field, superseded_at \
             FROM credential ORDER BY id",
        )
        .expect("prepare");
    stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
    })
    .expect("query")
    .map(|row| row.expect("row"))
    .collect()
}

fn names(fx: &crate::support::Fixture) -> BTreeSet<String> {
    key_names(&vike_secrets::read_table(&fx.db(), Table::Credential).expect("read"))
}

fn value_of(fx: &crate::support::Fixture, name: &str) -> Option<String> {
    vike_secrets::read_table(&fx.db(), Table::Credential).expect("read").into_map().remove(name)
}

// ---------------------------------------------------------------------------------------------
// The intents
// ---------------------------------------------------------------------------------------------

/// **`Replace` refuses a name the store does not hold, NAMES it, and writes nothing** — in both
/// tables, over the whole batch (a known name beside the unknown one is not written either).
#[test]
fn replace_refuses_a_name_the_store_does_not_hold_and_names_it() {
    let fx = binance_demo();
    let before = rows(&fx);

    let refused = vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &[
            (KEY.to_string(), "would-be-rotated".to_string()),
            (NEW_NAME.to_string(), "nv-never-printed".to_string()),
        ],
        UpsertIntent::Replace,
    )
    .expect_err("a Replace may create no row");
    let said = refused.to_string();
    assert!(said.contains(NEW_NAME), "the refusal names the key: {said}");
    assert!(!said.contains("nv-never-printed"), "…and never its value: {said}");
    assert_eq!(rows(&fx), before, "the batch is ONE transaction: nothing was written");

    let node = vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::NodeKey,
        &[("VIKE_TRADEHUB_OBSERVE_KEY".to_string(), "nk-never-printed".to_string())],
        UpsertIntent::Replace,
    )
    .expect_err("a Replace creates no node-key row either");
    assert!(node.to_string().contains("VIKE_TRADEHUB_OBSERVE_KEY"), "{node}");
    assert!(
        vike_secrets::read_table(&fx.db(), Table::NodeKey).expect("read").is_empty(),
        "the node-key table is still empty"
    );
}

/// **`Add` files a new name under the account, venue and field the REAL classifier derives** — the
/// production classifier is the one inside the store now, so the row agrees with it cell for cell.
#[test]
fn add_files_a_new_name_under_the_classifiers_account_and_field() {
    let fx = crate::support::Fixture::initialised();
    vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &[(NEW_NAME.to_string(), "added".to_string())],
        UpsertIntent::Add,
    )
    .expect("an Add of a grid name lands");

    let class = vike_secrets::credentials::classify_credential_name(NEW_NAME);
    let vike_secrets::Placement::Account(account) = &class.placement else {
        panic!("the classifier files a grid key under an account: {class:?}")
    };
    let filed = rows(&fx);
    assert_eq!(filed.len(), 1, "{filed:?}");
    let (_, name, value, account_id, venue_id, field, superseded) = &filed[0];
    assert_eq!((name.as_str(), value.as_str()), (NEW_NAME, "added"));
    assert_eq!(field, &class.field, "the field is the classifier's");
    assert_eq!(*venue_id, None, "an account-scoped row names its venue through the account");
    assert_eq!(*superseded, None, "it is the live row");
    let row = fx.row(account_id.expect("filed against an account")).expect("the account row");
    assert_eq!((row.venue.as_str(), row.tier), (account.venue.as_str(), account.tier));
}

/// **A replace of a KNOWN key changes its value and nothing else** — under either word, the row's
/// account, venue and field stay exactly where the first write filed them.
#[test]
fn a_replace_of_a_known_key_leaves_its_placement_untouched() {
    for intent in [UpsertIntent::Replace, UpsertIntent::Add] {
        let fx = binance_demo();
        let before = rows(&fx);
        vike_secrets::save_credentials_to_store(
            fx.dir(),
            Table::Credential,
            &[(KEY.to_string(), "rotated".to_string())],
            intent,
        )
        .expect("a known key is replaced");
        let after = rows(&fx);
        assert_eq!(after.len(), before.len(), "{intent:?}: no row was added");
        for (b, a) in before.iter().zip(&after) {
            if b.1 == KEY {
                assert_eq!(a.2, "rotated", "{intent:?}: the value changed");
                assert_eq!((a.0, a.3, a.4, &a.5, &a.6), (b.0, b.3, b.4, &b.5, &b.6), "{intent:?}");
            } else {
                assert_eq!(a, b, "{intent:?}: every other row is untouched");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The DELETE
// ---------------------------------------------------------------------------------------------

/// **A deleted key reads as ABSENT through every reader, and its row survives, value intact.**
///
/// The readers: the whole-table read, the scoped read, the presence read, the demo-only read, the
/// project resolver every loader sits on, and the account book's key names.
#[test]
fn a_deleted_key_reads_as_absent_through_every_reader_and_its_row_survives() {
    let fx = binance_demo();
    let id = fx.id_of("binance", "demo");
    let landed = vike_secrets::retire_credential_in_store(fx.dir(), Table::Credential, KEY)
        .expect("a live name is retired");
    assert_eq!(landed, Backend::Database(fx.db()));

    let db = fx.db();
    let scope = vike_secrets::KeyScope::of([KEY, SECRET]);
    let expect: BTreeSet<String> = [SECRET.to_string()].into();
    assert_eq!(names(&fx), expect, "read_table");
    assert_eq!(
        key_names(&vike_secrets::read_table_scoped(&db, Table::Credential, &scope).expect("read")),
        expect,
        "read_table_scoped"
    );
    assert_eq!(
        vike_secrets::read_present_names_scoped(&db, Table::Credential, &scope).expect("read"),
        expect,
        "read_present_names_scoped"
    );
    assert_eq!(
        key_names(&vike_secrets::read_credentials_demo_only(&db).expect("read").0),
        expect,
        "read_credentials_demo_only"
    );
    assert_eq!(
        key_names(&vike_secrets::resolve_project(fx.arg()).expect("read").secrets),
        expect,
        "resolve_project"
    );
    let book = vike_secrets::read_account_keys(&db).expect("read");
    assert_eq!(book.get(&id).map(|k| k.names.clone()), Some(vec![SECRET.to_string()]));

    // The row is EVIDENCE: still there, value and placement intact, marked and nothing else.
    let kept: Vec<Row> = rows(&fx).into_iter().filter(|r| r.1 == KEY).collect();
    assert_eq!(kept.len(), 1, "a delete removes no row: {kept:?}");
    assert_eq!(kept[0].2, KEY_VALUE, "the value survives for recovery");
    assert_eq!(kept[0].6.as_deref(), Some("retired-by-the-operator"));
}

/// **A delete touches the ONE row it was given** — every other row is byte-identical after it.
#[test]
fn a_delete_touches_no_other_row() {
    let fx = binance_demo();
    let before = rows(&fx);
    vike_secrets::retire_credential_in_store(fx.dir(), Table::Credential, KEY).expect("retire");
    let after = rows(&fx);
    assert_eq!(after.len(), before.len());
    for (b, a) in before.iter().zip(&after) {
        if b.1 == KEY {
            assert_eq!((a.0, &a.1, &a.2, a.3, a.4, &a.5), (b.0, &b.1, &b.2, b.3, b.4, &b.5));
        } else {
            assert_eq!(a, b, "a row the delete was not given changed");
        }
    }
}

/// **A deleted name can be ADDED again, and the new value is the one every reader answers** — the
/// partial unique indexes stop constraining the retired row, and the predicate hides it.
#[test]
fn a_deleted_key_can_be_added_again_and_the_old_value_never_comes_back() {
    let fx = binance_demo();
    vike_secrets::retire_credential_in_store(fx.dir(), Table::Credential, KEY).expect("retire");
    assert_eq!(value_of(&fx, KEY), None);

    vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::Credential,
        &[(KEY.to_string(), "re-added".to_string())],
        UpsertIntent::Add,
    )
    .expect("the retired name may be added again");
    assert_eq!(value_of(&fx, KEY).as_deref(), Some("re-added"));
    assert_eq!(rows(&fx).iter().filter(|r| r.1 == KEY).count(), 2, "the retired row stays");

    // …and deleted again: the name is absent, and NEITHER old value answers for it.
    vike_secrets::retire_credential_in_store(fx.dir(), Table::Credential, KEY).expect("again");
    assert_eq!(value_of(&fx, KEY), None, "no retired value may resurface");
}

/// **A name that is not live is refused BY NAME** — absent, or already deleted — and nothing is
/// written.
#[test]
fn a_delete_of_a_name_that_is_not_live_is_refused_naming_it() {
    let fx = binance_demo();
    let before = rows(&fx);
    let absent = vike_secrets::retire_credential_in_store(fx.dir(), Table::Credential, NEW_NAME)
        .expect_err("an absent name has nothing to delete");
    assert!(absent.to_string().contains(NEW_NAME), "{absent}");
    assert_eq!(rows(&fx), before, "nothing was written");

    vike_secrets::retire_credential_in_store(fx.dir(), Table::Credential, KEY).expect("retire");
    let twice = vike_secrets::retire_credential_in_store(fx.dir(), Table::Credential, KEY)
        .expect_err("a deleted name is not live");
    assert!(twice.to_string().contains(KEY), "{twice}");
    assert!(!twice.to_string().contains(KEY_VALUE), "…and never its value: {twice}");
}

/// **A NODE key is not deleted by name** — whichever table the caller names — and it still reads.
#[test]
fn a_node_key_is_refused_whichever_table_is_named() {
    let fx = binance_demo();
    vike_secrets::save_credentials_to_store(
        fx.dir(),
        Table::NodeKey,
        &[("VIKE_TRADEHUB_OBSERVE_KEY".to_string(), "nk".to_string())],
        UpsertIntent::Add,
    )
    .expect("a node key lands");
    for table in [Table::Credential, Table::NodeKey] {
        let refused =
            vike_secrets::retire_credential_in_store(fx.dir(), table, "VIKE_TRADEHUB_OBSERVE_KEY")
                .expect_err("a node key is minted, never deleted by name");
        let said = refused.to_string();
        assert!(said.contains("VIKE_TRADEHUB_OBSERVE_KEY") && said.contains("setup"), "{said}");
    }
    assert_eq!(
        key_names(&vike_secrets::read_table(&fx.db(), Table::NodeKey).expect("read")),
        ["VIKE_TRADEHUB_OBSERVE_KEY".to_string()].into(),
        "the node key is untouched"
    );
}

/// **No store: the delete is REFUSED naming the creator, and creates nothing.**
#[test]
fn a_delete_with_no_store_is_refused_and_creates_none() {
    let fx = crate::support::Fixture::empty();
    let refused = vike_secrets::retire_credential_in_store(fx.dir(), Table::Credential, KEY)
        .expect_err("no store, nothing to delete from");
    assert!(refused.to_string().contains("vike-cli secrets init"), "{refused}");
    assert!(!fx.db().exists(), "a delete must never bring a database into existence");
}

/// **The LIVE spelling of a credential that an alias also answers for is refused, naming the
/// alias; the alias goes first, then the live name, and then neither answers.**
#[test]
fn the_live_spelling_waits_for_its_alias_and_then_both_read_as_absent() {
    let fx = Fixture::seeded_with([("ALPACA_DEMO_API_KEY", "one-key")], is_node_key, &classify);
    fx.write([("ALPACA_SANDBOX_API_KEY", "one-key")], is_node_key, &classify);
    assert!(names(&fx).contains("ALPACA_SANDBOX_API_KEY"), "the precondition: the alias answers");

    let refused = vike_secrets::retire_credential_in_store(
        fx.dir(),
        Table::Credential,
        "ALPACA_DEMO_API_KEY",
    )
    .expect_err("deleting the live spelling alone would leave the account armed");
    let said = refused.to_string();
    assert!(said.contains("ALPACA_SANDBOX_API_KEY"), "the refusal names the alias: {said}");
    assert!(!said.contains("one-key"), "…and no value: {said}");

    vike_secrets::retire_credential_in_store(fx.dir(), Table::Credential, "ALPACA_SANDBOX_API_KEY")
        .expect("the alias is deleted on its own");
    assert!(!names(&fx).contains("ALPACA_SANDBOX_API_KEY"));
    assert!(names(&fx).contains("ALPACA_DEMO_API_KEY"), "the live spelling is still there");

    vike_secrets::retire_credential_in_store(fx.dir(), Table::Credential, "ALPACA_DEMO_API_KEY")
        .expect("…and then the live one");
    let left = names(&fx);
    assert!(
        !left.contains("ALPACA_DEMO_API_KEY") && !left.contains("ALPACA_SANDBOX_API_KEY"),
        "neither spelling may answer: {left:?}"
    );
}

/// **An account whose keys were all deleted is refused REMOVAL by name** — the retired rows still
/// name it — rather than with `FOREIGN KEY constraint failed`; deactivating it still works.
#[test]
fn an_account_whose_keys_were_deleted_is_refused_removal_by_name() {
    let fx = binance_demo();
    let id = fx.id_of("binance", "demo");
    for name in [KEY, SECRET] {
        vike_secrets::retire_credential_in_store(fx.dir(), Table::Credential, name)
            .expect("retire");
    }
    let refused =
        fx.edit(vike_secrets::AccountEdit::Remove { id }).expect_err("retired rows hold the row");
    assert!(
        matches!(
            &refused.kind,
            vike_secrets::DbErrorKind::AccountHoldsRetiredCredentials { keys, .. }
                if keys == &vec![KEY.to_string(), SECRET.to_string()]
        ),
        "{refused}"
    );
    assert!(fx.row(id).is_some(), "the account row is still there");
    fx.edit(vike_secrets::AccountEdit::SetActive { id, active: false })
        .expect("deactivating is the act that remains");
}

/// **The journalled delete records the NAME under its own kind, and no value.**
#[test]
fn the_journalled_delete_records_the_name_under_credential_retire() {
    use vike_secrets::change_journal::{
        Actor, CHANGES_SUBDIR, KIND_CREDENTIAL_RETIRE, Proc, month_file_name,
    };
    const T: i64 = 1_787_356_800_000;
    let fx = binance_demo();
    let (_, journal_error) = vike_secrets::retire_credential_in_store_journalled(
        fx.dir(),
        Table::Credential,
        KEY,
        vike_secrets::CredentialJournal {
            actor: Actor::cli("vike-cli"),
            venue: "binance",
            tier: "DEMO",
            proc: Proc::new("vike-cli", 1, "0.0.0"),
            now_ms: T,
        },
    )
    .expect("retire");
    assert!(journal_error.is_none(), "{journal_error:?}");
    let ledger = fx.dir().join("state").join(CHANGES_SUBDIR).join(month_file_name(T));
    let raw = std::fs::read_to_string(&ledger).expect("the change journal was written");
    assert_eq!(raw.lines().count(), 1, "{raw}");
    let v: serde_json::Value = serde_json::from_str(raw.trim_end()).expect("one JSON object");
    assert_eq!(v["kind"], KIND_CREDENTIAL_RETIRE);
    assert_eq!(v["target"]["keys"], serde_json::json!([KEY]));
    assert_eq!(v["target"]["count"], 1);
    assert!(!raw.contains(KEY_VALUE), "a delete journals no value: {raw}");

    // A refused delete writes no record.
    let _ = vike_secrets::retire_credential_in_store_journalled(
        fx.dir(),
        Table::Credential,
        NEW_NAME,
        vike_secrets::CredentialJournal {
            actor: Actor::cli("vike-cli"),
            venue: "okx",
            tier: "DEMO",
            proc: Proc::new("vike-cli", 1, "0.0.0"),
            now_ms: T,
        },
    )
    .expect_err("an absent name");
    let raw = std::fs::read_to_string(&ledger).expect("read");
    assert_eq!(raw.lines().count(), 1, "a refused delete records nothing: {raw}");
}
