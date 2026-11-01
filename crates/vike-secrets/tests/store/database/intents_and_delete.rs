//! The write INTENTS and the DELETE (decision 0144): `UpsertIntent::Replace` refuses a name the
//! store does not hold and names it, `UpsertIntent::Add` files a new name as a DEPLOYMENT row (no
//! account, no venue — decision 0156 item 6), a replace leaves a row's placement alone — and
//! `retire_credential_in_store_unjournalled` retires ONE name so that every reader reads it as
//! absent, keeps the row, and lets the name come back.

use super::*;
use vike_secrets::UpsertIntent;

/// A name no fixture here seeds — the subject of the Replace refusal.
const NEW_NAME: &str = "OKX_DEMO_API_SECRET";
/// A distinctive value, so a leak into a message is a substring nothing else contains.
const KEY_VALUE: &str = "kv-never-printed-7Qx";

/// The binance demo account every delete case starts from, and the NAMES its two keys are stored
/// under.
struct BinanceDemo {
    fx: crate::support::Fixture,
    id: i64,
    key: String,
    secret: String,
}

/// An EMPTY store, the binance demo ACCOUNT created, and its pair written against it through the
/// production owner-addressed writer.
fn binance_demo() -> BinanceDemo {
    let fx = crate::support::Fixture::initialised();
    let id = fx.create_account("binance", vike_model::AccountTier::Demo, None);
    let done = vike_secrets::save_owned_credentials_unjournalled(
        fx.dir(),
        &vike_secrets::CredentialOwner::Account(id),
        &[
            ("API_KEY".to_string(), KEY_VALUE.to_string()),
            ("API_SECRET".to_string(), "sv-1".to_string()),
        ],
        UpsertIntent::Add,
    )
    .expect("an Add of two fields of an existing account lands");
    let [key, secret]: [String; 2] = done.names.try_into().expect("two names");
    BinanceDemo { fx, id, key, secret }
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
    fx.credential_names()
}

fn value_of(fx: &crate::support::Fixture, name: &str) -> Option<String> {
    vike_secrets::read_table(&fx.db(), Table::Credential, vike_secrets::CredentialScope::Full)
        .map(|(m, _)| m)
        .expect("read")
        .into_map()
        .remove(name)
}

// ---------------------------------------------------------------------------------------------
// The intents
// ---------------------------------------------------------------------------------------------

/// **`Replace` refuses a name the store does not hold, NAMES it, and writes nothing** — in both
/// tables, over the whole batch (a known name beside the unknown one is not written either).
#[test]
fn replace_refuses_a_name_the_store_does_not_hold_and_names_it() {
    let BinanceDemo { fx, key, .. } = binance_demo();
    let before = rows(&fx);

    let refused = vike_secrets::save_credentials_to_store_unjournalled(
        fx.dir(),
        Table::Credential,
        &[
            (key.clone(), "would-be-rotated".to_string()),
            (NEW_NAME.to_string(), "nv-never-printed".to_string()),
        ],
        UpsertIntent::Replace,
    )
    .expect_err("a Replace may create no row");
    let said = refused.to_string();
    assert!(said.contains(NEW_NAME), "the refusal names the key: {said}");
    assert!(!said.contains("nv-never-printed"), "…and never its value: {said}");
    assert_eq!(rows(&fx), before, "the batch is ONE transaction: nothing was written");

    let node = vike_secrets::save_credentials_to_store_unjournalled(
        fx.dir(),
        Table::HubKey,
        &[("VIKE_TRADEHUB_OBSERVE_KEY".to_string(), "nk-never-printed".to_string())],
        UpsertIntent::Replace,
    )
    .expect_err("a Replace creates no hub-key row either");
    assert!(node.to_string().contains("VIKE_TRADEHUB_OBSERVE_KEY"), "{node}");
    assert!(
        vike_secrets::read_table(&fx.db(), Table::HubKey, vike_secrets::CredentialScope::Full)
            .map(|(m, _)| m)
            .expect("read")
            .is_empty(),
        "the hub-key table is still empty"
    );
}

/// **`Add` files a new NAME as a DEPLOYMENT row and nothing else** — `account_id` and `venue_id`
/// NULL, its `field` the whole name — and mints no account, whatever tier its name spells
/// (decision 0156 item 6: an account is the operator's to create, with its tier).
#[test]
fn add_files_a_new_name_as_a_deployment_row_and_mints_no_account() {
    let fx = crate::support::Fixture::initialised();
    vike_secrets::save_credentials_to_store_unjournalled(
        fx.dir(),
        Table::Credential,
        &[(NEW_NAME.to_string(), "added".to_string())],
        UpsertIntent::Add,
    )
    .expect("an Add of a new name lands");

    let filed = rows(&fx);
    assert_eq!(filed.len(), 1, "{filed:?}");
    let (_, name, value, account_id, venue_id, field, superseded) = &filed[0];
    assert_eq!((name.as_str(), value.as_str()), (NEW_NAME, "added"));
    assert_eq!((*account_id, *venue_id), (None, None), "a deployment row: no account, no venue");
    assert_eq!(field, NEW_NAME, "a deployment row's field is its whole name");
    assert_eq!(*superseded, None, "it is the live row");
    assert!(fx.accounts().is_empty(), "a name-addressed write mints NO account");
}

/// **A replace of a KNOWN key changes its value and nothing else** — under either word, the row's
/// account, venue and field stay exactly where the first write filed them. A name-addressed write
/// of an ACCOUNT's row's name rotates that row in place; it never re-files it as a deployment row.
#[test]
fn a_replace_of_a_known_key_leaves_its_placement_untouched() {
    for intent in [UpsertIntent::Replace, UpsertIntent::Add] {
        let BinanceDemo { fx, id, key, .. } = binance_demo();
        let before = rows(&fx);
        vike_secrets::save_credentials_to_store_unjournalled(
            fx.dir(),
            Table::Credential,
            &[(key.clone(), "rotated".to_string())],
            intent,
        )
        .expect("a known key is replaced");
        let after = rows(&fx);
        assert_eq!(after.len(), before.len(), "{intent:?}: no row was added");
        for (b, a) in before.iter().zip(&after) {
            if b.1 == key {
                assert_eq!(a.2, "rotated", "{intent:?}: the value changed");
                assert_eq!((a.0, a.3, a.4, &a.5, &a.6), (b.0, b.3, b.4, &b.5, &b.6), "{intent:?}");
                assert_eq!(a.3, Some(id), "{intent:?}: still the account's row");
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
/// The readers: the whole-table read, the scoped read, the presence read, the no-live-keys read,
/// the handle every read goes through, the account book's key names and the account's credential
/// book.
#[test]
fn a_deleted_key_reads_as_absent_through_every_reader_and_its_row_survives() {
    let BinanceDemo { fx, id, key, secret } = binance_demo();
    let landed =
        vike_secrets::retire_credential_in_store_unjournalled(fx.dir(), Table::Credential, &key)
            .expect("a live name is retired");
    assert_eq!(landed, Backend::Database(fx.db()));

    let db = fx.db();
    let scope = vike_secrets::KeyScope::of([key.as_str(), secret.as_str()]);
    let expect: BTreeSet<String> = [secret.clone()].into();
    assert_eq!(names(&fx), expect, "read_table");
    assert_eq!(
        key_names(
            &vike_secrets::read_table_scoped(
                &db,
                Table::Credential,
                &scope,
                vike_secrets::CredentialScope::Full
            )
            .map(|(m, _)| m)
            .expect("read")
        ),
        expect,
        "read_table_scoped"
    );
    assert_eq!(
        vike_secrets::read_present_names_scoped(
            &db,
            Table::Credential,
            &scope,
            vike_secrets::CredentialScope::Full
        )
        .expect("read"),
        expect,
        "read_present_names_scoped"
    );
    assert_eq!(
        key_names(
            &vike_secrets::read_table(
                &db,
                Table::Credential,
                vike_secrets::CredentialScope::NoLiveKeys
            )
            .expect("read")
            .0
        ),
        expect,
        "read_table under the no-live-keys scope"
    );
    assert_eq!(
        key_names(&crate::support::store_from(fx.arg()).read_all().expect("read").secrets),
        expect,
        "resolve_project"
    );
    let book =
        vike_secrets::read_account_keys(&db, vike_secrets::CredentialScope::Full).expect("read");
    assert_eq!(book.get(&id).map(|k| k.names.clone()), Some(vec![secret.clone()]));
    let credentials = crate::support::store_in(fx.dir()).credential_book().expect("read");
    let fields = credentials.account(id).expect("the account still holds its secret");
    assert_eq!(fields.fields().collect::<Vec<_>>(), ["API_SECRET"], "the credential book");

    // The row is EVIDENCE: still there, value and placement intact, marked and nothing else.
    let kept: Vec<Row> = rows(&fx).into_iter().filter(|r| r.1 == key).collect();
    assert_eq!(kept.len(), 1, "a delete removes no row: {kept:?}");
    assert_eq!(kept[0].2, KEY_VALUE, "the value survives for recovery");
    assert_eq!(kept[0].3, Some(id), "…and its account");
    assert_eq!(kept[0].6.as_deref(), Some("retired-by-the-operator"));
}

/// **A delete touches the ONE row it was given** — every other row is byte-identical after it.
#[test]
fn a_delete_touches_no_other_row() {
    let BinanceDemo { fx, key, .. } = binance_demo();
    let before = rows(&fx);
    vike_secrets::retire_credential_in_store_unjournalled(fx.dir(), Table::Credential, &key)
        .expect("retire");
    let after = rows(&fx);
    assert_eq!(after.len(), before.len());
    for (b, a) in before.iter().zip(&after) {
        if b.1 == key {
            assert_eq!((a.0, &a.1, &a.2, a.3, a.4, &a.5), (b.0, &b.1, &b.2, b.3, b.4, &b.5));
        } else {
            assert_eq!(a, b, "a row the delete was not given changed");
        }
    }
}

/// **A deleted field can be written again, and the new value is the one every reader answers** —
/// the partial unique indexes stop constraining the retired row, and the predicate hides it.
#[test]
fn a_deleted_key_can_be_added_again_and_the_old_value_never_comes_back() {
    let BinanceDemo { fx, id, key, .. } = binance_demo();
    vike_secrets::retire_credential_in_store_unjournalled(fx.dir(), Table::Credential, &key)
        .expect("retire");
    assert_eq!(value_of(&fx, &key), None);

    let again = vike_secrets::save_owned_credentials_unjournalled(
        fx.dir(),
        &vike_secrets::CredentialOwner::Account(id),
        &[("API_KEY".to_string(), "re-added".to_string())],
        UpsertIntent::Add,
    )
    .expect("the retired field may be added again");
    assert_eq!(
        again.names,
        std::slice::from_ref(&key),
        "under the same name the writer gave it before"
    );
    assert_eq!(value_of(&fx, &key).as_deref(), Some("re-added"));
    assert_eq!(rows(&fx).iter().filter(|r| r.1 == key).count(), 2, "the retired row stays");

    // …and deleted again: the name is absent, and NEITHER old value answers for it.
    vike_secrets::retire_credential_in_store_unjournalled(fx.dir(), Table::Credential, &key)
        .expect("again");
    assert_eq!(value_of(&fx, &key), None, "no retired value may resurface");
}

/// **A name that is not live is refused BY NAME** — absent, or already deleted — and nothing is
/// written.
#[test]
fn a_delete_of_a_name_that_is_not_live_is_refused_naming_it() {
    let BinanceDemo { fx, key, .. } = binance_demo();
    let before = rows(&fx);
    let absent = vike_secrets::retire_credential_in_store_unjournalled(
        fx.dir(),
        Table::Credential,
        NEW_NAME,
    )
    .expect_err("an absent name has nothing to delete");
    assert!(absent.to_string().contains(NEW_NAME), "{absent}");
    assert_eq!(rows(&fx), before, "nothing was written");

    vike_secrets::retire_credential_in_store_unjournalled(fx.dir(), Table::Credential, &key)
        .expect("retire");
    let twice =
        vike_secrets::retire_credential_in_store_unjournalled(fx.dir(), Table::Credential, &key)
            .expect_err("a deleted name is not live");
    assert!(twice.to_string().contains(&key), "{twice}");
    assert!(!twice.to_string().contains(KEY_VALUE), "…and never its value: {twice}");
}

/// **A hub key is not deleted by name** — whichever table the caller names — and it still reads.
#[test]
fn a_hub_key_is_refused_whichever_table_is_named() {
    let BinanceDemo { fx, .. } = binance_demo();
    vike_secrets::save_credentials_to_store_unjournalled(
        fx.dir(),
        Table::HubKey,
        &[("VIKE_TRADEHUB_OBSERVE_KEY".to_string(), "nk".to_string())],
        UpsertIntent::Add,
    )
    .expect("a hub key lands");
    for table in [Table::Credential, Table::HubKey] {
        let refused = vike_secrets::retire_credential_in_store_unjournalled(
            fx.dir(),
            table,
            "VIKE_TRADEHUB_OBSERVE_KEY",
        )
        .expect_err("a hub key is minted, never deleted by name");
        let said = refused.to_string();
        assert!(said.contains("VIKE_TRADEHUB_OBSERVE_KEY") && said.contains("setup"), "{said}");
    }
    assert_eq!(
        key_names(
            &vike_secrets::read_table(&fx.db(), Table::HubKey, vike_secrets::CredentialScope::Full)
                .map(|(m, _)| m)
                .expect("read")
        ),
        ["VIKE_TRADEHUB_OBSERVE_KEY".to_string()].into(),
        "the hub key is untouched"
    );
}

/// **No store: the delete is REFUSED naming the creator, and creates nothing.**
#[test]
fn a_delete_with_no_store_is_refused_and_creates_none() {
    let fx = crate::support::Fixture::empty();
    let refused = vike_secrets::retire_credential_in_store_unjournalled(
        fx.dir(),
        Table::Credential,
        NEW_NAME,
    )
    .expect_err("no store, nothing to delete from");
    assert!(refused.to_string().contains("vike-cli secrets init"), "{refused}");
    assert!(!fx.db().exists(), "a delete must never bring a database into existence");
}

/// **The LIVE spelling of a credential that an alias also answers for is refused, naming the
/// alias; the alias goes first, then the live name, and then neither answers.**
///
/// No writer files an alias any more (the schema-2 fill that did is retired), but an older store
/// holds them, and the delete must not leave the account armed with the value under the other
/// name — so the pair is planted the way that store holds it.
#[test]
fn the_live_spelling_waits_for_its_alias_and_then_both_read_as_absent() {
    let fx = crate::support::Fixture::initialised();
    fx.plant_aged_account(
        "alpaca",
        vike_model::AccountTier::Demo,
        &[("ALPACA_DEMO_API_KEY", "API_KEY")],
    );
    fx.plant_aged_alias("ALPACA_DEMO_API_KEY", "ALPACA_SANDBOX_API_KEY");
    assert!(names(&fx).contains("ALPACA_SANDBOX_API_KEY"), "the precondition: the alias answers");

    let refused = vike_secrets::retire_credential_in_store_unjournalled(
        fx.dir(),
        Table::Credential,
        "ALPACA_DEMO_API_KEY",
    )
    .expect_err("deleting the live spelling alone would leave the account armed");
    let said = refused.to_string();
    assert!(said.contains("ALPACA_SANDBOX_API_KEY"), "the refusal names the alias: {said}");
    assert!(!said.contains(&fake_value("ALPACA_DEMO_API_KEY")), "…and no value: {said}");

    vike_secrets::retire_credential_in_store_unjournalled(
        fx.dir(),
        Table::Credential,
        "ALPACA_SANDBOX_API_KEY",
    )
    .expect("the alias is deleted on its own");
    assert!(!names(&fx).contains("ALPACA_SANDBOX_API_KEY"));
    assert!(names(&fx).contains("ALPACA_DEMO_API_KEY"), "the live spelling is still there");

    vike_secrets::retire_credential_in_store_unjournalled(
        fx.dir(),
        Table::Credential,
        "ALPACA_DEMO_API_KEY",
    )
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
    let BinanceDemo { fx, id, key, secret } = binance_demo();
    for name in [&key, &secret] {
        vike_secrets::retire_credential_in_store_unjournalled(fx.dir(), Table::Credential, name)
            .expect("retire");
    }
    let refused =
        fx.edit(vike_secrets::AccountEdit::Remove { id }).expect_err("retired rows hold the row");
    assert!(
        matches!(
            &refused.kind,
            vike_secrets::DbErrorKind::AccountHoldsRetiredCredentials { keys, .. }
                if keys == &vec![key.clone(), secret.clone()]
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
    use vike_change_journal::{
        Actor, CHANGES_SUBDIR, KIND_CREDENTIAL_RETIRE, Proc, month_file_name,
    };
    const T: i64 = 1_787_356_800_000;
    let BinanceDemo { fx, key, .. } = binance_demo();
    let (_, journal_error) = vike_secrets::retire_credential_in_store(
        fx.dir(),
        Table::Credential,
        &key,
        vike_secrets::CredentialJournal {
            actor: Actor::cli("vike-cli"),
            venue: "binance",
            tier: "demo",
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
    assert_eq!(v["target"]["keys"], serde_json::json!([key]));
    assert_eq!(v["target"]["count"], 1);
    assert!(!raw.contains(KEY_VALUE), "a delete journals no value: {raw}");

    // A refused delete writes no record.
    let _ = vike_secrets::retire_credential_in_store(
        fx.dir(),
        Table::Credential,
        NEW_NAME,
        vike_secrets::CredentialJournal {
            actor: Actor::cli("vike-cli"),
            venue: "okx",
            tier: "demo",
            proc: Proc::new("vike-cli", 1, "0.0.0"),
            now_ms: T,
        },
    )
    .expect_err("an absent name");
    let raw = std::fs::read_to_string(&ledger).expect("read");
    assert_eq!(raw.lines().count(), 1, "a refused delete records nothing: {raw}");
}
