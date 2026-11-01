//! **`VIKE_NO_LIVE_KEYS` reaches the store through the root's one environment sweep, can only
//! NARROW, and binds EVERY read the handle offers.**
//!
//! `vike_secrets::open_credential_store` takes two facts out of a process's environment sweep: the
//! settings directory and the scope switch, parsed STRICTLY. This drives the answers the switch has
//! over ONE real settings database holding demo AND live ACCOUNTS:
//!
//! * unset or `false` — every row (the control);
//! * `true` (any case) — no credential of a `live`-tier account and none of the aster/polymarket
//!   families, through the whole map, the declared-name read, the presence read and the account
//!   credential book alike, so a live account's mount finds nothing, while the demo account beside
//!   it still loads;
//! * anything else — REFUSED: no store is opened, never a guess. (The retired `demo` word of the
//!   old switch is one of those values.)
//!
//! The env maps here are built by hand: nothing reads or writes this process's real environment.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::credentials::{StoreHealth, api_credentials};
use vike_secrets::{CredentialScope, KeyScope};

/// The planted store's NAMES, as the writers filed them: each account's rows under
/// `vike_secrets::account_credential_name`, and one DEPLOYMENT row by name.
struct Planted {
    settings: PathBuf,
    /// Every name the store holds, sorted.
    all: Vec<String>,
    /// The names the no-live-keys scope leaves, sorted.
    kept: Vec<String>,
    binance_demo_key: String,
    binance_live_key: String,
}

/// Create one unlabelled account and attach `fields` to it (each value obviously fake); answers the
/// names the writer filed.
fn account(settings: &Path, venue: &str, tier: AccountTier, fields: &[&str]) -> Vec<String> {
    let id = vike_secrets::edit_account_in_unjournalled(
        settings,
        vike_secrets::AccountEdit::Create { venue, tier, label: None },
    )
    .expect("the account is created")
    .after
    .expect("a create leaves a row")
    .id;
    let rows: Vec<(String, String)> =
        fields.iter().map(|f| ((*f).to_string(), format!("fake-{venue}-{tier}-{f}"))).collect();
    vike_secrets::save_owned_credentials_unjournalled(
        settings,
        &vike_secrets::CredentialOwner::Account(id),
        &rows,
        vike_secrets::UpsertIntent::Add,
    )
    .expect("the account exists, so the write lands")
    .names
}

/// The settings database holding a binance demo AND live account, a hyperliquid demo account, an
/// aster DEMO account (withheld by its venue family, whatever its tier) and the `POLY_PRIVATE_KEY`
/// deployment row (withheld by its name family): created EMPTY the way a fresh box creates one
/// (`vike-cli secrets init`), then filled through the writers production uses.
fn planted_store(root: &Path) -> Planted {
    let settings = root.join("settings");
    std::fs::create_dir_all(&settings).expect("settings dir");
    vike_secrets::create_store(settings.to_str()).expect("secrets init");
    let binance_demo = account(&settings, "binance", AccountTier::Demo, &["API_KEY", "API_SECRET"]);
    let binance_live = account(&settings, "binance", AccountTier::Live, &["API_KEY", "API_SECRET"]);
    let hyperliquid_demo = account(&settings, "hyperliquid", AccountTier::Demo, &["PRIVATE_KEY"]);
    let aster_demo = account(&settings, "aster", AccountTier::Demo, &["API_KEY"]);
    vike_secrets::save_credentials_to_store_unjournalled(
        &settings,
        vike_secrets::Table::Credential,
        &[("POLY_PRIVATE_KEY".to_string(), "fake-poly".to_string())],
        vike_secrets::UpsertIntent::Add,
    )
    .expect("the store exists, so the write lands");
    assert!(settings.join("db").join("vike.db").is_file(), "the database must exist");

    let mut kept: Vec<String> = binance_demo.iter().chain(&hyperliquid_demo).cloned().collect();
    kept.sort_unstable();
    let mut all: Vec<String> = kept
        .iter()
        .chain(&binance_live)
        .chain(&aster_demo)
        .cloned()
        .chain(["POLY_PRIVATE_KEY".to_string()])
        .collect();
    all.sort_unstable();
    Planted {
        settings,
        all,
        kept,
        binance_demo_key: binance_demo[0].clone(),
        binance_live_key: binance_live[0].clone(),
    }
}

fn env(settings: &Path, switch: Option<&str>) -> HashMap<String, String> {
    let mut env =
        HashMap::from([("VIKE_SETTINGS_DIR".to_string(), settings.display().to_string())]);
    if let Some(s) = switch {
        env.insert("VIKE_NO_LIVE_KEYS".to_string(), s.to_string());
    }
    env
}

fn sorted<'a>(names: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut v: Vec<&str> = names.collect();
    v.sort_unstable();
    v
}

/// The directory a root builds out of `store`, and whether the binance account at `tier` loads.
fn binance_loads(store: &vike_secrets::CredentialStore, tier: AccountTier) -> bool {
    let directory =
        AccountDirectory::read(store.accounts(), store.account_keys(), store.credential_book());
    api_credentials(&directory.credentials_for("binance", &AccountLabel::Default, tier), "binance")
        .is_some()
}

#[test]
fn unset_reads_every_name_and_true_withholds_every_live_or_real_money_one() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let planted = planted_store(tmp.path());
    let settings = &planted.settings;

    // The CONTROL: no switch, every name — including the live ones the scope exists to keep out.
    for control in [None, Some("false"), Some("FALSE")] {
        let store = vike_secrets::open_credential_store(&env(settings, control)).expect("accepted");
        assert_eq!(store.scope(), CredentialScope::Full, "{control:?}");
        let all = store.read_all().expect("the store opens");
        assert_eq!(
            sorted(all.secrets.keys()),
            planted.all.iter().map(String::as_str).collect::<Vec<_>>(),
            "the unscoped read lost names"
        );
        assert!(
            binance_loads(&store, AccountTier::Live),
            "the control must be able to load the live account, or the scoped case proves nothing"
        );
    }

    for on in ["true", "True", "TRUE"] {
        let store =
            vike_secrets::open_credential_store(&env(settings, Some(on))).expect("accepted");
        assert_eq!(store.scope(), CredentialScope::NoLiveKeys, "{on:?}");
        let read = store.read_all().expect("the store opens");
        assert_eq!(read.health, StoreHealth::Readable);
        assert_eq!(
            read.withheld,
            planted.all.len() - planted.kept.len(),
            "the withheld count is the store's"
        );
        assert_eq!(
            sorted(read.secrets.keys()),
            planted.kept.iter().map(String::as_str).collect::<Vec<_>>(),
            "the no-live-keys scope returned a name it must withhold, or lost a demo one"
        );
        assert!(
            !binance_loads(&store, AccountTier::Live),
            "⚠ the LIVE account's credentials loaded under the no-live-keys scope"
        );
        assert!(
            binance_loads(&store, AccountTier::Demo),
            "the DEMO account must still load under the scope"
        );
    }
}

/// **The scope binds the DECLARED-name read and the presence read too** — the doors that used to
/// ignore it. A live account's name declared by the caller is ABSENT under the scope (the live
/// gate's answer), never present, and the scoped read counts what it withheld.
#[test]
fn the_scope_binds_the_scoped_and_the_presence_reads() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let planted = planted_store(tmp.path());
    let settings = &planted.settings;
    let live = planted.binance_live_key.as_str();
    let demo = planted.binance_demo_key.as_str();
    let keys = KeyScope::of([live, demo, "POLY_PRIVATE_KEY"]);

    let full = vike_secrets::open_credential_store(&env(settings, None)).expect("accepted");
    let scoped = full.read(&keys).expect("opens");
    assert_eq!(scoped.len(), 3, "the control holds all three declared names");
    assert_eq!(scoped.withheld, 0);
    assert_eq!(full.present_names(&keys).expect("opens").len(), 3);

    let narrow =
        vike_secrets::open_credential_store(&env(settings, Some("true"))).expect("accepted");
    let scoped = narrow.read(&keys).expect("opens");
    assert_eq!(sorted(scoped.names()), [demo]);
    assert_eq!(scoped.withheld, 2, "two declared names were held and withheld");
    assert_eq!(scoped.get(live).declared(), Ok(None), "absent, not undeclared");
    assert_eq!(scoped.get("POLY_PRIVATE_KEY").declared(), Ok(None));
    let present = narrow.present_names(&keys).expect("opens");
    assert_eq!(present.iter().map(String::as_str).collect::<Vec<_>>(), [demo]);
}

/// **A switch value outside `true`/`false` opens NO store** — the error names the variable and the
/// two accepted words. `1`, `yes`, the retired `demo`, a typo, an empty or blank value, and a
/// padded `true` are all refused.
#[test]
fn an_unknown_switch_value_refuses_rather_than_guessing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let planted = planted_store(tmp.path());
    for bogus in ["1", "0", "yes", "no", "demo", "ture", "", "   ", " true", "true "] {
        let err = vike_secrets::open_credential_store(&env(&planted.settings, Some(bogus)))
            .expect_err("a word outside true/false must be refused");
        let msg = err.to_string();
        assert!(msg.contains("VIKE_NO_LIVE_KEYS"), "the refusal names the variable: {msg}");
        assert!(msg.contains("`true` or `false`"), "…and the accepted words: {msg}");
        assert_eq!(err.value, bogus, "the refused value is carried verbatim");
    }
}

/// `CredentialScope::parse`'s table, pinned without a store: `None` is `Full`.
#[test]
fn the_parse_table() {
    assert_eq!(CredentialScope::parse(None), Ok(CredentialScope::Full));
    assert_eq!(CredentialScope::parse(Some("true")), Ok(CredentialScope::NoLiveKeys));
    assert_eq!(CredentialScope::parse(Some("tRuE")), Ok(CredentialScope::NoLiveKeys));
    assert_eq!(CredentialScope::parse(Some("false")), Ok(CredentialScope::Full));
    assert_eq!(CredentialScope::parse(Some("False")), Ok(CredentialScope::Full));
    assert!(CredentialScope::parse(Some("")).is_err());
    assert!(CredentialScope::parse(Some("1")).is_err());
    assert_eq!(vike_secrets::NO_LIVE_KEYS_ENV, "VIKE_NO_LIVE_KEYS");
    assert_eq!(CredentialScope::default(), CredentialScope::Full);
}
