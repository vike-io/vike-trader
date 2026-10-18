//! The account is a ROW, and a credential is its account's: what the two credential writers file,
//! and what they refuse (decision 0156 item 6).
//!
//! The name-addressed writer (`save_credentials_to_store*`) files a new name as a DEPLOYMENT row and
//! mints no account; the owner-addressed writer (`save_owned_credentials*`) files a FIELD against an
//! account that already exists (or a venue's application row), and never creates one.

use std::assert_matches;

use super::*;
use vike_model::AccountTier;
use vike_secrets::{CredentialOwner, DbError, DbErrorKind, OwnedWrite, UpsertIntent};

/// One owner-addressed write, its answer handed back.
fn owned(
    fx: &Fixture,
    owner: &CredentialOwner,
    fields: &[(&str, &str)],
    intent: UpsertIntent,
) -> std::io::Result<OwnedWrite> {
    let fields: Vec<(String, String)> =
        fields.iter().map(|(f, v)| ((*f).to_string(), (*v).to_string())).collect();
    vike_secrets::save_owned_credentials_unjournalled(fx.dir(), owner, &fields, intent)
}

/// The refusal an owner-addressed write carries as its `io::Error`'s SOURCE — the typed half a wire
/// words its operator's answer from.
fn refusal(e: &std::io::Error) -> &DbErrorKind {
    &e.get_ref()
        .and_then(|inner| inner.downcast_ref::<DbError>())
        .unwrap_or_else(|| panic!("the refusal carries no DbError: {e}"))
        .kind
}

/// Every credential row as `(name, value, account_id, venue_id, field, superseded_at)`, by id.
type Row = (String, String, Option<i64>, Option<i64>, String, Option<String>);

fn rows(fx: &Fixture) -> Vec<Row> {
    let conn = fx.conn();
    let mut stmt = conn
        .prepare(
            "SELECT name, value, account_id, venue_id, field, superseded_at FROM credential \
             ORDER BY id",
        )
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
        .expect("query")
        .map(|row| row.expect("row"))
        .collect()
}

/// The number the store's `venue` roster gives `venue`.
fn venue_number(fx: &Fixture, venue: &str) -> i64 {
    fx.conn()
        .query_row("SELECT id FROM venue WHERE name = ?1", [venue], |r| r.get(0))
        .unwrap_or_else(|e| panic!("the roster carries {venue}: {e}"))
}

// ---------------------------------------------------------------------------------------------
// The name-addressed writer
// ---------------------------------------------------------------------------------------------

/// **A name-addressed write files every NEW name as a DEPLOYMENT row and mints NO account** —
/// whatever tier, venue or index its name spells.
///
/// The live box's 67 names include `BINANCE_DEMO_API_KEY`, `HYPERLIQUID_LIVE_PRIVATE_KEY` and
/// dukascopy's `DEMO1`/`DEMO2` pair; the retired schema-2 fill read an account and a tier out of
/// each and CREATED the account rows. Decision 0156 item 6: an account is the operator's to create,
/// with its tier, and a credential write chooses neither. So every row is `account_id` and
/// `venue_id` NULL, `field` the whole name, live — and the `account` table stays empty.
#[test]
fn a_name_addressed_write_files_a_deployment_row_and_mints_no_account() {
    let fx = Fixture::live_shaped();
    assert!(
        fx.accounts().is_empty(),
        "a name-addressed write minted an account: {:?}",
        fx.accounts()
    );

    let filed = rows(&fx);
    assert_eq!(filed.len(), LIVE_CREDENTIAL_KEYS.len(), "one row per name");
    for (name, _, account_id, venue_id, field, superseded) in &filed {
        assert_eq!((*account_id, *venue_id), (None, None), "{name}: a deployment row");
        assert_eq!(field, name, "{name}: a deployment row's field is its whole name");
        assert_eq!(*superseded, None, "{name}: the live row");
    }

    // …and it is in the map, unchanged, which is the half that matters.
    let map = crate::support::store_from(fx.arg()).read_all().expect("read").secrets.into_map();
    assert_eq!(
        map.get("CLOUDFLARE_API_TOKEN").map(String::as_str),
        Some(fake_value("CLOUDFLARE_API_TOKEN").as_str()),
        "a name round-trips verbatim"
    );
    // A deployment row belongs to no account, so no account's credentials hold it.
    let book = crate::support::store_in(fx.dir()).credential_book().expect("read");
    assert_eq!(book, vike_secrets::account_credentials::CredentialBook::new());
}

// ---------------------------------------------------------------------------------------------
// The owner-addressed writer
// ---------------------------------------------------------------------------------------------

/// **A field is filed against the EXISTING account it names**: `account_id` the account, `field`
/// the field, no venue (an account's row names its venue through the account), live, under the
/// writer's own name — `{venue}#{id}.{FIELD}`, which carries no tier and no label. The answer names
/// the account's venue and tier and every name written, in the order given.
#[test]
fn the_owner_addressed_writer_files_a_field_against_an_existing_account() {
    let fx = Fixture::initialised();
    let id = fx.create_account("binance", AccountTier::Live, Some("HEDGE"));

    let done = owned(
        &fx,
        &CredentialOwner::Account(id),
        &[("API_SECRET", "a-secret"), ("API_KEY", "a-key")],
        UpsertIntent::Add,
    )
    .expect("an existing account takes its fields");
    let secret_name = vike_secrets::account_credential_name("binance", id, "API_SECRET");
    let key_name = vike_secrets::account_credential_name("binance", id, "API_KEY");
    assert_eq!(
        done,
        OwnedWrite {
            venue: "binance".to_string(),
            tier: Some(AccountTier::Live),
            names: vec![secret_name.clone(), key_name.clone()],
        }
    );
    assert_eq!(key_name, format!("binance#{id}.API_KEY"), "the writer's own name scheme");
    for word in ["LIVE", "HEDGE"] {
        assert!(!key_name.to_ascii_uppercase().contains(word), "{key_name} spells {word}");
    }

    assert_eq!(
        rows(&fx),
        vec![
            (secret_name, "a-secret".to_string(), Some(id), None, "API_SECRET".to_string(), None),
            (key_name, "a-key".to_string(), Some(id), None, "API_KEY".to_string(), None),
        ]
    );
    assert_eq!(fx.accounts().len(), 1, "no account was created by the write");
    let book = crate::support::store_in(fx.dir()).credential_book().expect("read");
    assert_eq!(book.account(id).and_then(|f| f.get("API_KEY")), Some("a-key"));
}

/// **An account id no row carries is REFUSED — `NoSuchAccount` — and NO account is created**: a
/// credential write never invents the account it was addressed to, and never chooses a tier.
/// Nothing is written, the refusal names the id and never the value, and it rides the `io::Error`
/// as its typed SOURCE.
#[test]
fn an_unknown_account_is_refused_and_no_account_is_created() {
    let fx = Fixture::initialised();
    let e = owned(
        &fx,
        &CredentialOwner::Account(4242),
        &[("API_KEY", "value-never-printed")],
        UpsertIntent::Add,
    )
    .expect_err("no account 4242");
    assert_matches!(refusal(&e), DbErrorKind::NoSuchAccount { id: 4242 }, "{e}");
    let said = e.to_string();
    assert!(said.contains("4242"), "the refusal names the id: {said}");
    assert!(!said.contains("value-never-printed"), "…and never the value: {said}");
    assert!(fx.accounts().is_empty(), "NO account was created");
    assert!(rows(&fx).is_empty(), "nothing was written");
}

/// **A VENUE's application row**: `venue_id` the venue's number, no account, named
/// `{venue}.{FIELD}`; the answer carries no tier. A venue the roster does not carry is REFUSED
/// (`UnknownVenue`), and writes nothing.
#[test]
fn a_venue_row_is_filed_against_the_venues_number_and_an_unknown_venue_is_refused() {
    let fx = Fixture::initialised();
    let done = owned(
        &fx,
        &CredentialOwner::Venue("ctrader".to_string()),
        &[("CLIENT_ID", "an-app-id")],
        UpsertIntent::Add,
    )
    .expect("a roster venue takes its application row");
    let name = vike_secrets::venue_credential_name("ctrader", "CLIENT_ID");
    assert_eq!(
        done,
        OwnedWrite { venue: "ctrader".to_string(), tier: None, names: vec![name.clone()] }
    );
    assert_eq!(name, "ctrader.CLIENT_ID");
    let number = venue_number(&fx, "ctrader");
    assert_eq!(
        rows(&fx),
        vec![(name, "an-app-id".to_string(), None, Some(number), "CLIENT_ID".to_string(), None)]
    );
    assert!(!has_text_venue(&fx.conn(), "credential"), "the venue is its number alone");
    assert!(fx.accounts().is_empty(), "a venue row mints no account");

    let before = rows(&fx);
    let e = owned(
        &fx,
        &CredentialOwner::Venue("nosuchvenue".to_string()),
        &[("CLIENT_ID", "x")],
        UpsertIntent::Add,
    )
    .expect_err("no such venue");
    assert_matches!(refusal(&e), DbErrorKind::UnknownVenue { venue } if venue == "nosuchvenue", "{e}");
    assert_eq!(rows(&fx), before, "nothing was written");
}

/// **`Replace` writes only fields the owner HOLDS**: a field it does not hold is refused
/// (`NotHeld`, naming the name the row would have taken) and the whole batch writes nothing — a
/// rotation of a key that is already there never invents one.
#[test]
fn replace_refuses_a_field_the_account_does_not_hold() {
    let fx = Fixture::initialised();
    let id = fx.account_with("okx", AccountTier::Demo, None, &["API_KEY"]);
    let before = rows(&fx);

    let e = owned(
        &fx,
        &CredentialOwner::Account(id),
        &[("API_KEY", "rotated"), ("API_PASSPHRASE", "pp-never-printed")],
        UpsertIntent::Replace,
    )
    .expect_err("the account holds no passphrase");
    let want = vike_secrets::account_credential_name("okx", id, "API_PASSPHRASE");
    assert_matches!(
        refusal(&e),
        DbErrorKind::NotHeld { key, table: Table::Credential } if *key == want,
        "{e}"
    );
    assert!(!e.to_string().contains("pp-never-printed"), "{e}");
    assert_eq!(rows(&fx), before, "the batch is ONE transaction: the known field moved neither");
}

/// **A field the account already holds is REPLACED IN PLACE and keeps its name** — under either
/// word, and a LEGACY name an older store filed it under stays a legacy name: the name is a label
/// nothing reads. One row per field, before and after.
#[test]
fn a_held_field_is_replaced_in_place_and_keeps_its_name() {
    for intent in [UpsertIntent::Replace, UpsertIntent::Add] {
        let fx = Fixture::initialised();
        let id = fx.account_with("bybit", AccountTier::Demo, None, &["API_KEY"]);
        fx.plant_legacy_credential(id, "BYBIT_DEMO_API_SECRET", "API_SECRET");
        let before = rows(&fx);

        let done = owned(
            &fx,
            &CredentialOwner::Account(id),
            &[("API_KEY", "rotated-key"), ("API_SECRET", "rotated-secret")],
            intent,
        )
        .expect("held fields are replaced");
        let key_name = vike_secrets::account_credential_name("bybit", id, "API_KEY");
        assert_eq!(
            done.names,
            [key_name.clone(), "BYBIT_DEMO_API_SECRET".to_string()],
            "{intent:?}"
        );

        let after = rows(&fx);
        assert_eq!(after.len(), before.len(), "{intent:?}: no row was added");
        for (b, a) in before.iter().zip(&after) {
            assert_eq!((&a.0, a.2, a.3, &a.4, &a.5), (&b.0, b.2, b.3, &b.4, &b.5), "{intent:?}");
        }
        let book = crate::support::store_in(fx.dir()).credential_book().expect("read");
        let fields = book.account(id).expect("the account's fields");
        assert_eq!(fields.get("API_KEY"), Some("rotated-key"), "{intent:?}");
        assert_eq!(fields.get("API_SECRET"), Some("rotated-secret"), "{intent:?}");
    }
}

/// **A field that is not a store field word is REFUSED (`FieldMalformed`) before the store is
/// asked** — lower case, empty, or opening or closing with `_` — and nothing is written. The refusal
/// names the field (a NAME) and never the value.
#[test]
fn a_malformed_field_is_refused_and_writes_nothing() {
    let fx = Fixture::initialised();
    let id = fx.create_account("binance", AccountTier::Demo, None);
    for bad in ["api_key", "", "_API_KEY", "API_KEY_", "API KEY", "API-KEY"] {
        let e = owned(
            &fx,
            &CredentialOwner::Account(id),
            &[("API_SECRET", "fine"), (bad, "value-never-printed")],
            UpsertIntent::Add,
        )
        .expect_err("a malformed field is refused");
        assert_matches!(
            refusal(&e),
            DbErrorKind::FieldMalformed { field } if field == bad,
            "{bad:?}: {e}"
        );
        assert!(!e.to_string().contains("value-never-printed"), "{bad:?}: {e}");
    }
    assert!(rows(&fx).is_empty(), "no batch wrote anything — not even its well-formed half");
}

/// **No store: the owner-addressed write is REFUSED naming the creator, and creates nothing** — the
/// same routing as every other writer; a multi-line value is refused before any store is asked.
#[test]
fn an_owner_addressed_write_with_no_store_is_refused_and_creates_none() {
    let fx = Fixture::empty();
    let e = owned(&fx, &CredentialOwner::Account(1), &[("API_KEY", "nowhere")], UpsertIntent::Add)
        .expect_err("no store, no write");
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
    assert!(e.to_string().contains("secrets init"), "the refusal names the way in: {e}");
    assert!(!fx.db().exists(), "a write must never bring a database into existence");

    let lines = owned(&fx, &CredentialOwner::Account(1), &[("API_KEY", "a\nb")], UpsertIntent::Add)
        .expect_err("a credential is ONE line");
    assert_eq!(lines.kind(), std::io::ErrorKind::InvalidInput);
}
