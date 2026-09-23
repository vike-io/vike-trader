// Shared by `account_lifecycle.rs` and `venue_table.rs`, which cargo compiles as INDEPENDENT test
// binaries. Each uses a subset of `Fixture`'s methods, so per-binary dead-code analysis flags the
// rest — expected, and the same rationale `crates/bridges/ctrader/tests/common/mod.rs` carries for
// the same shape.
#![allow(dead_code)]
//! **Shared integration-test fixture** — extracted from `tests/account_lifecycle.rs` so a second
//! test binary (`tests/venue_table.rs`) can build the same migrated store without a second copy of
//! it to keep in step. A `tests/support/mod.rs` file is not itself a test target — `cargo test`
//! only builds binaries for files directly under `tests/`, so each binary that needs this fixture
//! declares `mod support;` and gets its own compiled copy, the ordinary shape for shared test code.
//!
//! Moved verbatim; nothing here was "improved" on the way out of `account_lifecycle.rs`.

use std::path::{Path, PathBuf};

use vike_secrets::{AccountEdit, Accounts};

/// Six names: one binance demo account with two keys, one binance live account with one, one
/// dukascopy demo account (the UNLABELLED shape the ambiguity guard is about), and one key that
/// owns no account at all.
pub const FIXTURE_KEYS: [&str; 6] = [
    "BINANCE_DEMO_API_KEY",
    "BINANCE_DEMO_API_SECRET",
    "BINANCE_LIVE_API_KEY",
    "CLOUDFLARE_API_TOKEN",
    "DUKASCOPY_DEMO1_LOGIN",
    "DUKASCOPY_DEMO1_PASSWORD",
];

pub fn is_node_key(_key: &str) -> bool {
    false
}

/// The account classification the production caller passes
/// (`vike_bridge_core::credentials::classify_credential_name`), spelled here because this crate
/// cannot link the crate that owns it — the same seam, and the same reason, as
/// `tests/account_book.rs`' own copy.
pub fn classify(name: &str) -> vike_secrets::Classification {
    use vike_secrets::{AccountKey, Classification, Placement};

    let account = |venue: &str, tier: &str, disc: Option<&str>, field: &str| Classification {
        placement: Placement::Account(AccountKey {
            venue: venue.to_string(),
            tier: tier.to_string(),
            label: None,
            discriminator: disc.map(str::to_string),
        }),
        field: field.to_string(),
        secret: true,
        recognised: true,
        pending_move: None,
    };

    if let Some(field) = name.strip_prefix("DUKASCOPY_DEMO1_") {
        return account("dukascopy", "demo", Some("DEMO1"), field);
    }
    for (prefix, venue, tier) in
        [("BINANCE_DEMO_", "binance", "demo"), ("BINANCE_LIVE_", "binance", "live")]
    {
        if let Some(field) = name.strip_prefix(prefix) {
            return account(venue, tier, None, field);
        }
    }
    Classification::unrecognised(name)
}

pub fn fake_value(key: &str) -> String {
    format!("value-for-{key}")
}

pub struct Fixture {
    _dir: tempfile::TempDir,
    settings: PathBuf,
}

impl Fixture {
    pub fn file_store() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        std::fs::write(settings.join("secrets.env"), Fixture::store_text())
            .expect("write fixture store");
        Fixture { _dir: dir, settings }
    }

    pub fn store_text() -> String {
        FIXTURE_KEYS.iter().map(|k| format!("{k}={}\n", fake_value(k))).collect()
    }

    pub fn migrated() -> Fixture {
        let fx = Fixture::file_store();
        match vike_secrets::migrate(fx.arg(), is_node_key, &classify) {
            Ok(_) => fx,
            Err(e) => panic!("migration refused: {e}"),
        }
    }

    pub fn arg(&self) -> Option<&str> {
        Some(self.settings.to_str().expect("utf-8 temp path"))
    }

    pub fn dir(&self) -> &Path {
        &self.settings
    }

    pub fn db(&self) -> PathBuf {
        self.settings.join("db").join("vike.db")
    }

    pub fn store(&self) -> PathBuf {
        self.settings.join("secrets.env")
    }

    pub fn accounts(&self) -> Vec<vike_secrets::Account> {
        match vike_secrets::resolve_accounts_in(self.dir()).expect("the store opened") {
            Accounts::Known(rows) => rows,
            Accounts::Unanswerable(why) => panic!("the store could not be asked: {why}"),
        }
    }

    pub fn row(&self, id: i64) -> Option<vike_secrets::Account> {
        self.accounts().into_iter().find(|a| a.id == id)
    }

    /// The row an operator would pick for a venue/tier, by the cells a listing shows.
    pub fn id_of(&self, venue: &str, tier: &str) -> i64 {
        let rows = self.accounts();
        let hit: Vec<&vike_secrets::Account> =
            rows.iter().filter(|a| a.venue == venue && a.tier == tier).collect();
        assert_eq!(hit.len(), 1, "the fixture must carry ONE {venue}/{tier} row: {hit:?}");
        hit[0].id
    }

    /// The write, through the door a production caller uses — the Backend-aware router, never the
    /// db function directly, so every test here exercises the store choice as well as the write.
    pub fn edit(
        &self,
        edit: AccountEdit<'_>,
    ) -> Result<vike_secrets::AccountWrite, vike_secrets::DbError> {
        vike_secrets::edit_account_in(self.dir(), edit)
    }
}
