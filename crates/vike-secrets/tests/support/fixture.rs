//! The `Fixture` — a throwaway settings directory holding NO store (`Fixture::empty`), the EMPTY
//! store `vike-cli secrets init` creates (`Fixture::initialised`), or a store SEEDED through the
//! writers production uses (`Fixture::seeded`, `Fixture::seeded_with`) — and the doors a test reads
//! and edits it through.
//!
//! A store is seeded the way a box gets one: `vike_secrets::create_store` creates it EMPTY, the
//! operator creates each ACCOUNT with its tier (`AccountEdit::Create`), and the account's keys are
//! written against it by FIELD (`vike_secrets::save_owned_credentials_unjournalled`, decision 0156
//! item 6). A name-addressed write (`vike_secrets::save_credentials_to_store_unjournalled`) files a
//! DEPLOYMENT row (no account, no venue) or a node key, and mints no account.
//!
//! ⚠ **The `plant_aged_*` doors are the one exception, and say so.** An older store holds shapes no
//! writer makes any more — a second unlabelled account of one `(venue, tier)`, a credential row under
//! a LEGACY name filed against an account, an ALIAS row — because the retired schema-2 fill made them
//! and nothing converts a store (decision 0117). The readers must keep answering for those shapes, so
//! a test that is about one plants it by SQL, the only way left to make it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use vike_model::AccountTier;
use vike_secrets::{AccountEdit, Accounts, CredentialOwner, Table, UpsertIntent};

use super::values::{fake_value, is_node_key};

/// The shared store's ACCOUNTS, `(venue, tier, fields)`, each created unlabelled and holding one
/// row per field: one binance demo account with two keys, one binance live account with one, and one
/// dukascopy demo account (the UNLABELLED shape the ambiguity guard is about).
pub const FIXTURE_ACCOUNTS: [(&str, AccountTier, &[&str]); 3] = [
    ("binance", AccountTier::Demo, &["API_KEY", "API_SECRET"]),
    ("binance", AccountTier::Live, &["API_KEY"]),
    ("dukascopy", AccountTier::Demo, &["LOGIN", "PASSWORD"]),
];

/// The shared store's one DEPLOYMENT row — a credential no account owns.
pub const FIXTURE_DEPLOYMENT_KEY: &str = "CLOUDFLARE_API_TOKEN";

/// The `superseded_at` marker the retired schema-2 fill wrote on an ALIAS row (a second spelling of
/// one account field). Spelled here because no production code writes it any more; the readers'
/// widened predicate (`crates/vike-secrets/src/db/read.rs`'s `live_predicate`) still answers such a
/// row.
const AGED_ALIAS_MARK: &str = "alias-of-the-canonical-tier-spelling";

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

    /// A store holding exactly `rows`, BY NAME: created EMPTY, then written through the
    /// name-addressed writer. A name `is_node_key` claims lands in `node_key`, every other one in
    /// `credential` as a DEPLOYMENT row — and a refused write panics with the refusal.
    pub fn seeded_with<K: AsRef<str>, V: AsRef<str>>(
        rows: impl IntoIterator<Item = (K, V)>,
        is_node_key: impl Fn(&str) -> bool,
    ) -> Fixture {
        let fx = Fixture::initialised();
        fx.write(rows, is_node_key);
        fx
    }

    /// The shared store: [`FIXTURE_ACCOUNTS`], each created and its fields attached, and the
    /// [`FIXTURE_DEPLOYMENT_KEY`] deployment row.
    pub fn seeded() -> Fixture {
        let fx = Fixture::initialised();
        for (venue, tier, fields) in FIXTURE_ACCOUNTS {
            fx.account_with(venue, tier, None, fields);
        }
        fx.write([(FIXTURE_DEPLOYMENT_KEY, fake_value(FIXTURE_DEPLOYMENT_KEY))], is_node_key);
        fx
    }

    /// Create the EMPTY store in this directory — `vike_secrets::create_store`, the one creator.
    pub fn init(&self) -> vike_secrets::StoreCreation {
        vike_secrets::create_store(self.arg())
            .unwrap_or_else(|e| panic!("create_store refused: {e}"))
    }

    /// Write `rows` BY NAME into this directory's store through
    /// `save_credentials_to_store_unjournalled` — node keys (by `is_node_key`) and deployment rows
    /// in one call each, both `UpsertIntent::Add` — panicking with the refusal if either write is
    /// refused.
    pub fn write<K: AsRef<str>, V: AsRef<str>>(
        &self,
        rows: impl IntoIterator<Item = (K, V)>,
        is_node_key: impl Fn(&str) -> bool,
    ) {
        let mut node = Vec::new();
        let mut credentials = Vec::new();
        for (k, v) in rows {
            let row = (k.as_ref().to_string(), v.as_ref().to_string());
            if is_node_key(&row.0) { node.push(row) } else { credentials.push(row) }
        }
        if !node.is_empty() {
            self.try_write(Table::NodeKey, &node, UpsertIntent::Add)
                .unwrap_or_else(|e| panic!("the node-key write was refused: {e}"));
        }
        if !credentials.is_empty() {
            self.try_write(Table::Credential, &credentials, UpsertIntent::Add)
                .unwrap_or_else(|e| panic!("the credential write was refused: {e}"));
        }
    }

    /// One name-addressed write, its answer handed back — for a test whose subject is the refusal.
    pub fn try_write(
        &self,
        table: Table,
        rows: &[(String, String)],
        intent: UpsertIntent,
    ) -> std::io::Result<vike_secrets::Backend> {
        vike_secrets::save_credentials_to_store_unjournalled(self.dir(), table, rows, intent)
    }

    /// **Create an account** — the operator's act, through the production router — and answer its
    /// id.
    pub fn create_account(&self, venue: &str, tier: AccountTier, label: Option<&str>) -> i64 {
        self.edit(AccountEdit::Create { venue, tier, label })
            .unwrap_or_else(|e| panic!("creating ({venue}, {tier}, {label:?}) was refused: {e}"))
            .after
            .expect("a create returns its row")
            .id
    }

    /// **Attach `fields` to account `id`** through the owner-addressed writer (`UpsertIntent::Add`),
    /// each holding `fake_value` of the NAME the writer files it under; answers those names, in the
    /// order given.
    pub fn attach(&self, id: i64, fields: &[&str]) -> Vec<String> {
        let venue = self.row(id).unwrap_or_else(|| panic!("no account {id}")).venue;
        let rows: Vec<(String, String)> = fields
            .iter()
            .map(|f| {
                let name = vike_secrets::account_credential_name(&venue, id, f);
                ((*f).to_string(), fake_value(&name))
            })
            .collect();
        let done = vike_secrets::save_owned_credentials_unjournalled(
            self.dir(),
            &CredentialOwner::Account(id),
            &rows,
            UpsertIntent::Add,
        )
        .unwrap_or_else(|e| panic!("attaching {fields:?} to account {id} was refused: {e}"));
        let expected: Vec<String> =
            fields.iter().map(|f| vike_secrets::account_credential_name(&venue, id, f)).collect();
        assert_eq!(done.names, expected, "the writer filed new fields under its own names");
        done.names
    }

    /// **Attach `fields` to `venue`'s APPLICATION rows** (`CredentialOwner::Venue` — cTrader's OAuth
    /// client, shared by every account of the venue), each holding `fake_value` of the name the
    /// writer files it under; answers those names.
    pub fn attach_venue(&self, venue: &str, fields: &[&str]) -> Vec<String> {
        let rows: Vec<(String, String)> = fields
            .iter()
            .map(|f| ((*f).to_string(), fake_value(&vike_secrets::venue_credential_name(venue, f))))
            .collect();
        vike_secrets::save_owned_credentials_unjournalled(
            self.dir(),
            &CredentialOwner::Venue(venue.to_string()),
            &rows,
            UpsertIntent::Add,
        )
        .unwrap_or_else(|e| panic!("attaching {fields:?} to venue {venue} was refused: {e}"))
        .names
    }

    /// [`Self::create_account`] then [`Self::attach`]; answers the id.
    pub fn account_with(
        &self,
        venue: &str,
        tier: AccountTier,
        label: Option<&str>,
        fields: &[&str],
    ) -> i64 {
        let id = self.create_account(venue, tier, label);
        if !fields.is_empty() {
            self.attach(id, fields);
        }
        id
    }

    /// The NAME account `(venue, tier)`'s `field` is stored under — `account_credential_name` over
    /// [`Self::id_of`].
    pub fn key(&self, venue: &str, tier: &str, field: &str) -> String {
        vike_secrets::account_credential_name(venue, self.id_of(venue, tier), field)
    }

    /// **An UNLABELLED account row as an older store holds one, planted by SQL** — a second row of
    /// one `(venue, tier)` is the shape the retired fill made for dukascopy's two demo accounts and
    /// `AccountEdit::Create` refuses (`AmbiguousUnlabelledAccount`). Each `(name, field)` in `rows`
    /// is planted against it under that LEGACY name ([`Self::plant_legacy_credential`]). Answers the
    /// id.
    pub fn plant_aged_account(&self, venue: &str, tier: AccountTier, rows: &[(&str, &str)]) -> i64 {
        let conn = self.conn();
        let planted = conn
            .execute(
                "INSERT INTO account (venue_id, tier, label) SELECT id, ?2, NULL FROM venue \
                 WHERE name = ?1",
                (venue, tier.as_str()),
            )
            .unwrap_or_else(|e| panic!("planting an aged ({venue}, {tier}) row failed: {e}"));
        assert_eq!(planted, 1, "the store's venue roster must carry `{venue}`");
        let id = conn.last_insert_rowid();
        drop(conn);
        for (name, field) in rows {
            self.plant_legacy_credential(id, name, field);
        }
        id
    }

    /// **A credential row under a LEGACY name, filed against account `id` at `field`, as the retired
    /// fill filed it** (`DUKASCOPY_DEMO1_LOGIN` at `LOGIN`), holding `fake_value(name)`. Planted by
    /// SQL: the owner-addressed writer names a NEW row `{venue}#{id}.{FIELD}`, and a name-addressed
    /// write files no account at all.
    pub fn plant_legacy_credential(&self, id: i64, name: &str, field: &str) {
        self.conn()
            .execute(
                "INSERT INTO credential (account_id, field, value, name) VALUES (?1, ?2, ?3, ?4)",
                (id, field, fake_value(name), name),
            )
            .unwrap_or_else(|e| panic!("planting {name} on account {id} failed: {e}"));
    }

    /// **An ALIAS row as an older store holds one**: `alias`, a second spelling of the LIVE row
    /// `live` at the same owner and field with the same value, filed superseded under the retired
    /// fill's marker. No writer files one any more; the readers still answer it under its own name.
    pub fn plant_aged_alias(&self, live: &str, alias: &str) {
        let planted = self
            .conn()
            .execute(
                "INSERT INTO credential (account_id, venue_id, field, value, name, secret, \
                 superseded_at) \
                 SELECT account_id, venue_id, field, value, ?2, secret, ?3 FROM credential \
                 WHERE name = ?1 AND superseded_at IS NULL",
                (live, alias, AGED_ALIAS_MARK),
            )
            .unwrap_or_else(|e| panic!("planting the alias {alias} of {live} failed: {e}"));
        assert_eq!(planted, 1, "`{live}` must be a live row to alias");
    }

    /// Every credential NAME the whole-table read answers, sorted.
    pub fn credential_names(&self) -> BTreeSet<String> {
        key_names(
            &vike_secrets::read_table(
                &self.db(),
                Table::Credential,
                vike_secrets::CredentialScope::Full,
            )
            .map(|(m, _)| m)
            .expect("read the credential table"),
        )
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
        match super::store::store_in(self.dir()).accounts().expect("the store opened") {
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
            rows.iter().filter(|a| a.venue == venue && a.tier.as_str() == tier).collect();
        assert_eq!(hit.len(), 1, "the fixture must carry ONE {venue}/{tier} row: {hit:?}");
        hit[0].id
    }

    /// The write, through the door a production caller uses — the Backend-aware router, never the
    /// db function directly, so every test here exercises the store choice as well as the write.
    pub fn edit(
        &self,
        edit: AccountEdit<'_>,
    ) -> Result<vike_secrets::AccountWrite, vike_secrets::DbError> {
        vike_secrets::edit_account_in_unjournalled(self.dir(), edit)
    }
}
