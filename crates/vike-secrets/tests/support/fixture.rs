//! The `Fixture` — a throwaway settings directory holding a file store (`Fixture::file_store`) or the migrated database built from it (`Fixture::migrated`), and the doors a test reads and edits it through.
//!
//! Beside the shared six-name fixture it carries the CONSTRUCTORS every test file's own `Fixture`
//! re-spelled — `tempdir` + `settings/` + a store (`with_store_text`, `empty`), `migrate_with`, the
//! `Connection::open(fx.db())` preamble (`conn`) and the append-one-key step (`append_key`) — so a
//! test whose fixture differs only in WHICH names it plants states those names and nothing else.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use vike_secrets::{AccountEdit, Accounts, Classification};

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

/// The comment a hand-written fixture store opens with, so a migration that "normalised" the file
/// — dropped the comment, reordered a line — would show up as a byte difference.
pub const HAND_EDITED_HEADER: &str =
    "# a hand-edited store — comments and order are the operator's\n\n";

/// A credential file's text: [`HAND_EDITED_HEADER`], then one `name=value` line per row.
pub fn hand_edited_store_text<K: AsRef<str>, V: AsRef<str>>(
    rows: impl IntoIterator<Item = (K, V)>,
) -> String {
    let mut text = String::from(HAND_EDITED_HEADER);
    for (key, value) in rows {
        text.push_str(&format!("{}={}\n", key.as_ref(), value.as_ref()));
    }
    text
}

/// A credential file's text with one `name=<fake value>` line per key, in the order given, and no
/// header — the body `account_book.rs`, `account_reader.rs` and `Fixture::store_text` each wrote.
pub fn fake_store_text<K: AsRef<str>>(keys: impl IntoIterator<Item = K>) -> String {
    keys.into_iter().map(|k| format!("{}={}\n", k.as_ref(), fake_value(k.as_ref()))).collect()
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
    /// A settings directory with NOTHING in it — no credential file, no node file, no database.
    /// The live gate's own shape, and the starting point every other constructor adds one file to.
    pub fn empty() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        Fixture { _dir: dir, settings }
    }

    /// A settings directory whose credential file holds exactly `text`, and nothing else.
    pub fn with_store_text(text: &str) -> Fixture {
        let fx = Fixture::empty();
        std::fs::write(fx.store(), text).expect("write fixture store");
        fx
    }

    pub fn file_store() -> Fixture {
        Fixture::with_store_text(&Fixture::store_text())
    }

    pub fn store_text() -> String {
        fake_store_text(FIXTURE_KEYS)
    }

    pub fn migrated() -> Fixture {
        let fx = Fixture::file_store();
        match vike_secrets::migrate(
            fx.arg(),
            is_node_key,
            &classify,
            vike_secrets::WhenNothingToCarry::CreateNothing,
        ) {
            Ok(_) => fx,
            Err(e) => panic!("migration refused: {e}"),
        }
    }

    /// Migrate this directory's file store with the caller's own predicate and classifier — the
    /// body every file's `migrate`/`migrated` repeated — panicking with the refusal if it refuses.
    pub fn migrate_with(
        &self,
        is_node_key: impl Fn(&str) -> bool,
        classify: &dyn Fn(&str) -> Classification,
    ) -> vike_secrets::Migration {
        match vike_secrets::migrate(
            self.arg(),
            is_node_key,
            classify,
            vike_secrets::WhenNothingToCarry::CreateNothing,
        ) {
            Ok(migration) => migration,
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

    /// The node file's path (`docs/decisions/0051`) — absent until a test writes one.
    pub fn node(&self) -> PathBuf {
        self.settings.join("node.env")
    }

    /// Plant the node file with exactly `text`.
    pub fn write_node_text(&self, text: &str) {
        std::fs::write(self.node(), text).expect("write fixture node file");
    }

    /// **A connection to the settings database** — the `Connection::open(fx.db()).expect("open")`
    /// preamble in front of nearly every raw-SQL assertion. It opens whatever is there, so call it
    /// after the store exists.
    pub fn conn(&self) -> rusqlite::Connection {
        super::sql::open(&self.db())
    }

    /// Append one `key=<fake value>` line to the file store, leaving every line already there
    /// byte-identical — the way a key the shared classifier has never seen reaches a test.
    pub fn append_key(&self, key: &str) {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(self.store())
            .expect("open the file store for append");
        writeln!(file, "{key}={}", fake_value(key)).expect("append");
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
