//! A REAL settings database planted in a test's own temp directory, for the credentialed lanes' suites on
//! both sides of the crate boundary: the lib's `src/rest/{ctrader,ig,oanda}_tests.rs` and
//! `src/data/accounts_tests.rs` (their reads are `pub(crate)`: they return plaintext keys), and the
//! `server` group's `tests/lanes/{ig,oanda}_credential.rs` (the same lanes through the real server).
//!
//! Every fixture plants ACCOUNT ROWS and writes their FIELDS (decision 0156 item 6): no credential NAME is
//! composed or spelled, and every VALUE is an obviously fake, distinctive string, so "no value reached a
//! log line" is an assertion that could actually fail. The store is created with `vike_secrets::create_store`
//! (the one creator) and seeded through the writers' record-less `test-support` bodies, against a
//! directory the CALLER made (a `tempfile` dir): nothing here names or reaches a real settings directory.

use vike_model::AccountTier;

/// One planted account: its venue, tier and label, and the `(field, value)` pairs filed against it.
pub type Planted = (&'static str, AccountTier, Option<&'static str>, Vec<(String, String)>);

/// Bytes that are not a database where the database belongs: a store that EXISTS and will not open
/// (`chmod 000` proves nothing as root, which CI is).
pub fn plant_unreadable_store(settings: &std::path::Path) {
    std::fs::create_dir_all(settings.join("db")).expect("the db directory");
    std::fs::write(vike_secrets::db_path_in(settings), b"not a sqlite database at all")
        .expect("plant the garbage");
}

/// Make the settings DATABASE — the only credential store — hold exactly `accounts`, 0600 on unix so the
/// permission finding is only what a test asks for. The store is created the one way a store comes into
/// being (`vike-cli secrets init`'s library half); each account is created with its tier FIRST and its
/// fields are then written against it by owner and field — the one way an account's credential reaches a
/// store. A store being REPLACED (a rotation, a removal) is rebuilt in the test's own temp directory: the
/// writer adds and replaces fields and never removes one.
pub fn write_store(settings: &std::path::Path, accounts: &[Planted]) {
    let db_dir = settings.join("db");
    if db_dir.exists() {
        std::fs::remove_dir_all(&db_dir).expect("drop this test's own previous store");
    }
    vike_secrets::create_store(Some(settings.to_str().expect("a utf-8 temp path")))
        .expect("create the empty store");
    for &(venue, tier, label, ref fields) in accounts {
        let created = vike_secrets::edit_account_in_unjournalled(
            settings,
            vike_secrets::AccountEdit::Create { venue, tier, label },
        )
        .expect("create the account");
        let id = created.after.expect("a create returns its row").id;
        if !fields.is_empty() {
            vike_secrets::save_owned_credentials_unjournalled(
                settings,
                &vike_secrets::CredentialOwner::Account(id),
                fields,
                vike_secrets::UpsertIntent::Add,
            )
            .expect("write the account's fields");
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            vike_secrets::db_path_in(settings),
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("chmod");
    }
}

/// The OANDA practice account and its neighbours (drift register row R7's credential half).
pub mod oanda {
    use vike_model::AccountTier;

    use super::Planted;

    /// The token's FIELD on the practice account.
    pub const TOKEN_FIELD: &str = vike_venues::credential_keys::API_KEY;
    /// The oanda account's other field — the neighbour on the SAME account the read must leave.
    pub const ACCOUNT_ID_FIELD: &str = "ACCOUNT_ID";

    /// The practice account's token.
    pub const PRACTICE_VALUE: &str = "fake-practice-token-7q4z";

    /// A store's worth of accounts around the practice account, with the practice account's token set to
    /// `token` (`None` = the account holds no token). The NEIGHBOURS the one-field read must leave
    /// behind: the practice account's own account id, the LIVE oanda account's pair, the PAPER oanda
    /// account's key, a LABELLED demo oanda account's key, and another venue's key. Every value
    /// distinctive.
    pub fn store_rows(token: Option<&str>) -> Vec<Planted> {
        let pair = |field: &str, value: &str| (field.to_string(), value.to_string());
        let mut practice = vec![pair(ACCOUNT_ID_FIELD, "fake-account-id-2k9d")];
        if let Some(token) = token {
            practice.push(pair(TOKEN_FIELD, token));
        }
        vec![
            ("oanda", AccountTier::Demo, None, practice),
            (
                "oanda",
                AccountTier::Live,
                None,
                vec![
                    pair(TOKEN_FIELD, "fake-live-token-8m3x"),
                    pair(ACCOUNT_ID_FIELD, "fake-live-account-5j1c"),
                ],
            ),
            ("oanda", AccountTier::Paper, None, vec![pair(TOKEN_FIELD, "fake-sim-token-3w7n")]),
            (
                "oanda",
                AccountTier::Demo,
                Some("ALT"),
                vec![pair(TOKEN_FIELD, "fake-labelled-token-9p5r")],
            ),
            ("binance", AccountTier::Live, None, vec![pair(TOKEN_FIELD, "fake-binance-key-4t8y")]),
        ]
    }

    /// Every neighbour, the practice account holding NO token.
    pub fn neighbours() -> Vec<Planted> {
        store_rows(None)
    }

    /// The practice token plus every neighbour.
    pub fn full_store_rows() -> Vec<Planted> {
        store_rows(Some(PRACTICE_VALUE))
    }

    /// Every planted value, the practice token included — what no log line may carry.
    pub fn every_value() -> Vec<String> {
        let mut values: Vec<String> = store_rows(None)
            .into_iter()
            .flat_map(|(_, _, _, fields)| fields.into_iter().map(|(_, v)| v))
            .collect();
        values.push(PRACTICE_VALUE.to_string());
        values
    }
}
