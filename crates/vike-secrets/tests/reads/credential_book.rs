//! **The ACCOUNT-addressed reads** — `CredentialStore::credential_book`,
//! `CredentialStore::account_field` and `AccountDirectory::credentials_for`: a credential is its
//! account's (`credential.account_id` + `field`) or its venue's (`venue_id` + `field`), read by FIELD,
//! and never found by composing a name (decision 0156 item 6).
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_book_holds_every_live_row_by_account_and_by_venue_and_nothing_else`] | a book carrying a retired value, an alias spelling's or a deployment row — a deleted key still arming a mount |
//! | [`the_book_reads_a_legacy_named_row_by_its_account_and_field`] | an older store's rows (filed under legacy names) unreadable by a mount that no longer composes names |
//! | [`account_field_answers_the_one_active_account_and_none_when_two_answer`] | the narrowest read picking one of two unlabelled accounts by row order, or reading another tier's |
//! | [`credentials_for_picks_the_one_active_row_at_the_tier`] | a mount signing with another tier's, another label's, an inactive or an ambiguous account's keys |
//! | [`two_unlabelled_dukascopy_demo_books_resolve_by_account_id`] | the CI box's two unlabelled demo books refused as ambiguous, so neither dukascopy account runs |
//!
//! ⚠ Values are obviously fake (`support::fake_value`), and no assertion prints one: a
//! `CredentialBook`'s `Debug` is redacted.

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_credentials::CredentialBook;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{AccountEdit, Table, UpsertIntent};

use crate::support::{self, Fixture};

/// The value an account-field read answers, as a plain string — `None` when it answers nothing.
fn field_of(
    fx: &Fixture,
    venue: &str,
    label: &AccountLabel,
    tier: AccountTier,
    field: &str,
) -> Option<String> {
    support::store_in(fx.dir())
        .account_field(venue, label, tier, field)
        .expect("the store opens")
        .map(|v| v.expose().clone())
}

/// The directory a composition root builds out of `fx`'s store: the rows, their key names and
/// every account's credentials.
fn directory(fx: &Fixture) -> AccountDirectory {
    let store = support::store_in(fx.dir());
    AccountDirectory::read(store.accounts(), store.account_keys(), store.credential_book())
}

fn named(label: &str) -> AccountLabel {
    AccountLabel::parse(label).expect("a legal label")
}

/// **The book is every LIVE row, by account and by venue — and nothing superseded, retired or
/// owned by nobody.** Compared WHOLE against the book it must equal, so a row that leaked in fails
/// as surely as one that went missing.
#[test]
fn the_book_holds_every_live_row_by_account_and_by_venue_and_nothing_else() {
    let fx = Fixture::initialised();
    let binance = fx.create_account("binance", AccountTier::Demo, None);
    let [key, secret]: [String; 2] =
        fx.attach(binance, &["API_KEY", "API_SECRET"]).try_into().expect("two names");
    let bybit = fx.create_account("bybit", AccountTier::Live, None);
    let [bybit_key]: [String; 1] = fx.attach(bybit, &["API_KEY"]).try_into().expect("one name");
    let [client_id]: [String; 1] =
        fx.attach_venue("ctrader", &["CLIENT_ID"]).try_into().expect("one name");
    // A DEPLOYMENT row: read by name, out of the whole map — never an account's field.
    fx.write([("CLOUDFLARE_API_TOKEN", "a-deployment-token")], support::is_hub_key);

    // A RETIRED field, then added again: the retired row keeps its value as evidence, and must
    // not answer.
    vike_secrets::retire_credential_in_store_unjournalled(fx.dir(), Table::Credential, &secret)
        .expect("a live name is retired");
    vike_secrets::save_owned_credentials_unjournalled(
        fx.dir(),
        &vike_secrets::CredentialOwner::Account(binance),
        &[("API_SECRET".to_string(), "the-re-added-secret".to_string())],
        UpsertIntent::Add,
    )
    .expect("a retired field may be added again");
    // An ALIAS row an older store holds beside bybit's key, carrying a DIFFERENT value: superseded,
    // so never the account's credential.
    let alias = "BYBIT_LIVE_API_KEY";
    fx.plant_aged_alias(&bybit_key, alias);
    fx.conn()
        .execute("UPDATE credential SET value = 'an-alias-value' WHERE name = ?1", [alias])
        .expect("diverge the alias");

    let mut want = CredentialBook::new();
    want.insert_account(binance, "API_KEY", support::fake_value(&key));
    want.insert_account(binance, "API_SECRET", "the-re-added-secret");
    want.insert_account(bybit, "API_KEY", support::fake_value(&bybit_key));
    want.insert_venue("ctrader", "CLIENT_ID", support::fake_value(&client_id));

    let book = support::store_in(fx.dir()).credential_book().expect("the store opens");
    assert!(
        book == want,
        "the book is not exactly the live account and venue rows (values withheld): {book:?}"
    );
    // …while the whole-map read still answers the deployment row by NAME.
    let map = support::store_in(fx.dir()).read_all().expect("opens").secrets.into_map();
    assert_eq!(map.get("CLOUDFLARE_API_TOKEN").map(String::as_str), Some("a-deployment-token"));
}

/// **An older store's row, filed under a LEGACY name against its account, is read by account and
/// field** — the name is a label nothing reads, so a mount that composes none still finds it. A
/// write to that field through the owner-addressed writer replaces the row in place and keeps the
/// legacy name.
#[test]
fn the_book_reads_a_legacy_named_row_by_its_account_and_field() {
    let fx = Fixture::initialised();
    let duka = fx.plant_aged_account(
        "dukascopy",
        AccountTier::Demo,
        &[("DUKASCOPY_DEMO1_LOGIN", "LOGIN"), ("DUKASCOPY_DEMO1_PASSWORD", "PASSWORD")],
    );
    let book = support::store_in(fx.dir()).credential_book().expect("opens");
    let fields = book.account(duka).expect("the aged account's rows are its own");
    assert_eq!(fields.fields().collect::<Vec<_>>(), ["LOGIN", "PASSWORD"]);
    assert_eq!(fields.get("LOGIN"), Some(support::fake_value("DUKASCOPY_DEMO1_LOGIN").as_str()));

    let done = vike_secrets::save_owned_credentials_unjournalled(
        fx.dir(),
        &vike_secrets::CredentialOwner::Account(duka),
        &[("LOGIN".to_string(), "a-rotated-login".to_string())],
        UpsertIntent::Replace,
    )
    .expect("a field the account holds is replaced");
    assert_eq!(done.names, ["DUKASCOPY_DEMO1_LOGIN"], "the row keeps the name it was stored under");
    let book = support::store_in(fx.dir()).credential_book().expect("opens");
    assert_eq!(book.account(duka).and_then(|f| f.get("LOGIN")), Some("a-rotated-login"));
}

/// **The narrowest read answers ONE field of the ONE active account of `(venue, label, tier)`** —
/// and `None` for another tier, another label, an inactive row, a blank value, and for TWO
/// unlabelled active accounts of one `(venue, tier)`, where picking either would be a guess.
#[test]
fn account_field_answers_the_one_active_account_and_none_when_two_answer() {
    let fx = Fixture::seeded();
    let demo_key = fx.key("binance", "demo", "API_KEY");
    let default = AccountLabel::Default;

    assert_eq!(
        field_of(&fx, "binance", &default, AccountTier::Demo, "API_KEY"),
        Some(support::fake_value(&demo_key)),
        "the one active unlabelled binance demo account's field"
    );
    assert_eq!(
        field_of(&fx, "binance", &default, AccountTier::Live, "API_KEY"),
        Some(support::fake_value(&fx.key("binance", "live", "API_KEY"))),
        "the LIVE account's own field, never the demo one's"
    );
    assert_eq!(field_of(&fx, "binance", &default, AccountTier::Live, "API_SECRET"), None, "absent");
    assert_eq!(field_of(&fx, "okx", &default, AccountTier::Demo, "API_KEY"), None, "no account");

    // A LABELLED account of the same (venue, tier) answers under its label alone.
    let alt = fx.account_with("binance", AccountTier::Demo, Some("ALT"), &["API_KEY"]);
    let alt_key = vike_secrets::account_credential_name("binance", alt, "API_KEY");
    assert_eq!(
        field_of(&fx, "binance", &named("ALT"), AccountTier::Demo, "API_KEY"),
        Some(support::fake_value(&alt_key))
    );
    assert_eq!(
        field_of(&fx, "binance", &default, AccountTier::Demo, "API_KEY"),
        Some(support::fake_value(&demo_key)),
        "the unlabelled account is untouched by a labelled one beside it"
    );

    // A BLANK value is absent.
    vike_secrets::save_owned_credentials_unjournalled(
        fx.dir(),
        &vike_secrets::CredentialOwner::Account(alt),
        &[("API_SECRET".to_string(), "   ".to_string())],
        UpsertIntent::Add,
    )
    .expect("a blank value is stored");
    assert_eq!(field_of(&fx, "binance", &named("ALT"), AccountTier::Demo, "API_SECRET"), None);

    // An INACTIVE row answers nothing.
    fx.edit(AccountEdit::SetActive { id: alt, active: false }).expect("deactivate");
    assert_eq!(field_of(&fx, "binance", &named("ALT"), AccountTier::Demo, "API_KEY"), None);

    // TWO unlabelled active accounts of one (venue, tier) — an older store's shape — answer NONE.
    let first = fx.id_of("dukascopy", "demo");
    fx.plant_aged_account("dukascopy", AccountTier::Demo, &[("DUKASCOPY_DEMO2_LOGIN", "LOGIN")]);
    assert_eq!(
        field_of(&fx, "dukascopy", &default, AccountTier::Demo, "LOGIN"),
        None,
        "two unlabelled active accounts answer one address: picking either would be a guess"
    );
    // …and once one of them is deactivated, the other is THE account again.
    fx.edit(AccountEdit::SetActive { id: first, active: false }).expect("deactivate");
    assert_eq!(
        field_of(&fx, "dukascopy", &default, AccountTier::Demo, "LOGIN"),
        Some(support::fake_value("DUKASCOPY_DEMO2_LOGIN"))
    );
}

/// **`credentials_for` is the ONE active row of `(venue, label, tier)`'s fields, and its venue's.**
/// Another tier, another label, an inactive row, two unlabelled rows, the paper tier and a
/// directory nobody read all answer EMPTY — absent credentials are the live gate.
#[test]
fn credentials_for_picks_the_one_active_row_at_the_tier() {
    let fx = Fixture::seeded();
    let ctrader = fx.account_with("ctrader", AccountTier::Demo, None, &["ACCESS_TOKEN"]);
    fx.attach_venue("ctrader", &["CLIENT_ID"]);
    let paper = fx.account_with("okx", AccountTier::Paper, None, &["API_KEY"]);
    let default = AccountLabel::Default;

    let dir = directory(&fx);
    let demo = dir.credentials_for("binance", &default, AccountTier::Demo);
    assert_eq!(demo.account_id(), Some(fx.id_of("binance", "demo")));
    assert_eq!(
        demo.get("API_KEY"),
        Some(support::fake_value(&fx.key("binance", "demo", "API_KEY")).as_str())
    );
    assert_eq!(demo.missing(&["API_KEY", "API_SECRET"]), Vec::<&str>::new());
    let live = dir.credentials_for("binance", &default, AccountTier::Live);
    assert_eq!(live.account_id(), Some(fx.id_of("binance", "live")));
    assert_eq!(
        live.get("API_KEY"),
        Some(support::fake_value(&fx.key("binance", "live", "API_KEY")).as_str()),
        "the live row's own key, never the demo one's"
    );
    assert_eq!(live.get("API_SECRET"), None, "the live account holds no secret");

    // The venue's application rows ride along with the account's.
    let app = dir.credentials_for("ctrader", &default, AccountTier::Demo);
    assert_eq!(app.account_id(), Some(ctrader));
    assert!(app.get("ACCESS_TOKEN").is_some());
    assert!(app.venue_get("CLIENT_ID").is_some(), "the OAuth client is the venue's");
    assert_eq!(app.get("CLIENT_ID"), None, "…and never read as the account's own field");

    // No row at the address, another label, and the paper tier: empty.
    for (venue, label, tier) in [
        ("okx", default.clone(), AccountTier::Demo),
        ("binance", named("ALT"), AccountTier::Demo),
        ("okx", default.clone(), AccountTier::Paper),
    ] {
        let none = dir.credentials_for(venue, &label, tier);
        assert!(!none.holds_any(), "{venue} {label} {tier}: an account answered");
        assert_eq!(none.account_id(), None, "{venue} {label} {tier}");
    }
    assert!(dir.account_row("okx", &default, AccountTier::Paper).is_none(), "paper reads nothing");
    assert!(
        support::store_in(fx.dir()).credential_book().expect("opens").account(paper).is_some(),
        "the precondition: the paper account DOES hold a row the directory declines to hand out"
    );

    // An INACTIVE row: empty.
    fx.edit(AccountEdit::SetActive { id: fx.id_of("binance", "live"), active: false })
        .expect("deactivate");
    assert!(!directory(&fx).credentials_for("binance", &default, AccountTier::Live).holds_any());

    // TWO unlabelled active rows of one (venue, tier): empty, and `account_row` declines too.
    // (dukascopy is the one venue that does NOT decline: see
    // [`two_unlabelled_dukascopy_demo_books_resolve_by_account_id`].)
    fx.plant_aged_account("binance", AccountTier::Demo, &[("BINANCE_DEMO2_API_KEY", "API_KEY")]);
    let dir = directory(&fx);
    assert!(dir.account_row("binance", &default, AccountTier::Demo).is_none());
    assert!(!dir.credentials_for("binance", &default, AccountTier::Demo).holds_any());

    // A directory nobody read, and one whose credential read failed: empty, never a guess.
    assert!(
        !AccountDirectory::unread()
            .credentials_for("binance", &default, AccountTier::Demo)
            .holds_any()
    );
    let store = support::store_in(fx.dir());
    let failed = AccountDirectory::read(
        store.accounts(),
        store.account_keys(),
        Err::<CredentialBook, _>("the store would not open"),
    );
    assert!(failed.credentials().is_some_and(|c| c.is_err()), "the failure is carried");
    assert!(!failed.credentials_for("binance", &default, AccountTier::Demo).holds_any());
}

/// **`deployment_names` lists the rows that belong to NO account and NO venue — by the row's own
/// columns, never by the shape of its name.** An account's row and a venue's row are left out
/// even when their names look like a deployment name, and a retired deployment row is gone.
#[test]
fn deployment_names_are_the_ownerless_rows_and_nothing_else() {
    let fx = Fixture::initialised();
    let binance = fx.create_account("binance", AccountTier::Demo, None);
    fx.attach(binance, &["API_KEY"]);
    fx.attach_venue("ctrader", &["CLIENT_ID"]);
    // An account row filed under an env-shaped LEGACY name is still the account's.
    fx.plant_legacy_credential(binance, &format!("BINANCE_{}", "DEMO_API_SECRET"), "API_SECRET");
    let pager = format!("VIKE_{}", "ALERT_TELEGRAM_TOKEN");
    let gone = format!("TARDIS_{}", "API_KEY");
    fx.write([(pager.as_str(), "v1"), (gone.as_str(), "v2")], |_| false);
    vike_secrets::retire_credential_in_store_unjournalled(fx.dir(), Table::Credential, &gone)
        .expect("the retire");
    let names = support::store_in(fx.dir()).deployment_names().expect("the store opens");
    assert_eq!(names.into_iter().collect::<Vec<_>>(), vec![pager]);
}

/// **The owner's real dukascopy shape: TWO ACTIVE `demo` rows, BOTH unlabelled, told apart by account
/// id alone, each credential filed against its own `account_id` under its legacy name**
/// (`DUKASCOPY_DEMO1_*` / `DUKASCOPY_DEMO2_*`) — planted exactly as the retired fill made them, since
/// the writer refuses to create a second unlabelled row. The default account is the LOWEST id and
/// reads ITS OWN `LOGIN`/`PASSWORD`, never the other book's, and no label is invented. Any OTHER venue
/// with the same shape stays refused (`account_row` is `None`: picking one of two would be a guess).
#[test]
fn two_unlabelled_dukascopy_demo_books_resolve_by_account_id() {
    let fx = Fixture::initialised();
    let first = fx.plant_aged_account(
        "dukascopy",
        AccountTier::Demo,
        &[("DUKASCOPY_DEMO1_LOGIN", "LOGIN"), ("DUKASCOPY_DEMO1_PASSWORD", "PASSWORD")],
    );
    let second = fx.plant_aged_account(
        "dukascopy",
        AccountTier::Demo,
        &[("DUKASCOPY_DEMO2_LOGIN", "LOGIN"), ("DUKASCOPY_DEMO2_PASSWORD", "PASSWORD")],
    );
    assert!(first < second);

    let dir = directory(&fx);
    let default = AccountLabel::Default;
    let row = dir.account_row("dukascopy", &default, AccountTier::Demo).expect("an account");
    assert_eq!((row.id, row.label.as_deref()), (first, None), "the lowest id, still unlabelled");
    let creds = dir.credentials_for("dukascopy", &default, AccountTier::Demo);
    assert_eq!(creds.account_id(), Some(first));
    assert_eq!(creds.get("LOGIN"), Some(support::fake_value("DUKASCOPY_DEMO1_LOGIN").as_str()));
    assert_eq!(
        creds.get("PASSWORD"),
        Some(support::fake_value("DUKASCOPY_DEMO1_PASSWORD").as_str())
    );

    // A LABEL addresses the other book without touching the first.
    fx.edit(AccountEdit::Rename { id: second, label: Some("EU") }).expect("label the second book");
    let dir = directory(&fx);
    let eu = dir.credentials_for("dukascopy", &named("EU"), AccountTier::Demo);
    assert_eq!(eu.get("LOGIN"), Some(support::fake_value("DUKASCOPY_DEMO2_LOGIN").as_str()));
    let again = dir.credentials_for("dukascopy", &default, AccountTier::Demo);
    assert_eq!(again.get("LOGIN"), Some(support::fake_value("DUKASCOPY_DEMO1_LOGIN").as_str()));
}
