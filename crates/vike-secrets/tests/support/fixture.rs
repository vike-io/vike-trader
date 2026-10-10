//! The `Fixture` — a throwaway settings directory holding NO store (`Fixture::empty`), the EMPTY
//! store `vike-cli secrets init` creates (`Fixture::initialised`), or a store SEEDED with
//! rows through the one credential writer (`Fixture::seeded`, `Fixture::seeded_with`) — and the
//! doors a test reads and edits it through.
//!
//! A store is seeded the way a box gets one: `vike_secrets::create_store` creates it EMPTY, and
//! `vike_secrets::save_credentials_to_store`
//! — the `vike-cli secrets set` path — writes the rows. There is no other way a credential reaches a
//! store, so there is no other way a fixture plants one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use vike_secrets::{AccountEdit, Accounts, Classification, Table};

use super::classify::{classify, fake_value, is_node_key};

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

/// `(name, fake value)` for every key, in the order given — the rows a seeded store holds.
pub fn fake_rows<K: AsRef<str>>(keys: impl IntoIterator<Item = K>) -> Vec<(String, String)> {
    keys.into_iter().map(|k| (k.as_ref().to_string(), fake_value(k.as_ref()))).collect()
}

/// The NAMES a `SecretMap` holds, sorted — `SecretMap` deliberately exposes `keys` and no per-key
/// `get`, so an assertion about which names survived says so here, once.
pub fn key_names(map: &vike_secrets::SecretMap) -> BTreeSet<String> {
    map.keys().map(str::to_string).collect()
}

pub struct Fixture {
    _dir: tempfile::TempDir,
    settings: PathBuf,
}

impl Fixture {
    /// A settings directory with NOTHING in it — no database. The live gate's own shape, and the
    /// starting point every other constructor adds a store to.
    pub fn empty() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        Fixture { _dir: dir, settings }
    }

    /// The EMPTY store `vike-cli secrets init` creates: the current schema, stamped, no row.
    pub fn initialised() -> Fixture {
        let fx = Fixture::empty();
        fx.init();
        fx
    }

    /// A store holding exactly `rows`: created EMPTY, then written through the one writer. A name
    /// `is_node_key` claims lands in `node_key`, every other one in `credential`, classified by
    /// `classify` — and a refused write panics with the refusal.
    pub fn seeded_with<K: AsRef<str>, V: AsRef<str>>(
        rows: impl IntoIterator<Item = (K, V)>,
        is_node_key: impl Fn(&str) -> bool,
        classify: &dyn Fn(&str) -> Classification,
    ) -> Fixture {
        let fx = Fixture::initialised();
        fx.write(rows, is_node_key, classify);
        fx
    }

    /// The shared six-name store ([`FIXTURE_KEYS`]), through the shared classifier.
    pub fn seeded() -> Fixture {
        Fixture::seeded_with(fake_rows(FIXTURE_KEYS), is_node_key, &classify)
    }

    /// Create the EMPTY store in this directory — `vike_secrets::create_store`, the one creator.
    pub fn init(&self) -> vike_secrets::StoreCreation {
        vike_secrets::create_store(self.arg())
            .unwrap_or_else(|e| panic!("create_store refused: {e}"))
    }

    /// Write `rows` into this directory's store through `save_credentials_to_store` — node keys
    /// (by `is_node_key`) and credentials (classified by `classify`) in one call each — panicking
    /// with the refusal if either write is refused.
    pub fn write<K: AsRef<str>, V: AsRef<str>>(
        &self,
        rows: impl IntoIterator<Item = (K, V)>,
        is_node_key: impl Fn(&str) -> bool,
        classify: &dyn Fn(&str) -> Classification,
    ) {
        let mut node = Vec::new();
        let mut credentials = Vec::new();
        for (k, v) in rows {
            let row = (k.as_ref().to_string(), v.as_ref().to_string());
            if is_node_key(&row.0) { node.push(row) } else { credentials.push(row) }
        }
        if !node.is_empty() {
            self.try_write(Table::NodeKey, &node, None)
                .unwrap_or_else(|e| panic!("the node-key write was refused: {e}"));
        }
        if !credentials.is_empty() {
            self.try_write(Table::Credential, &credentials, Some(classify))
                .unwrap_or_else(|e| panic!("the credential write was refused: {e}"));
        }
    }

    /// One `save_credentials_to_store` call, its answer handed back — for a test whose subject is
    /// the refusal.
    pub fn try_write(
        &self,
        table: Table,
        rows: &[(String, String)],
        classify: Option<&dyn Fn(&str) -> Classification>,
    ) -> std::io::Result<vike_secrets::Backend> {
        vike_secrets::save_credentials_to_store(self.dir(), table, rows, classify)
    }

    /// Write one `key=<fake value>` credential row through `classify` — the way a key the shared
    /// classifier has never seen reaches a test.
    pub fn add_key(&self, key: &str, classify: &dyn Fn(&str) -> Classification) {
        self.write([(key, fake_value(key))], |_| false, classify);
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

    /// **A connection to the settings database** — the `Connection::open(fx.db()).expect("open")`
    /// preamble in front of nearly every raw-SQL assertion. It opens whatever is there, so call it
    /// after the store exists.
    pub fn conn(&self) -> rusqlite::Connection {
        super::sql::open(&self.db())
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
