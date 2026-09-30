//! **The `account` table gets a reader** — and every test here is about a way the reader could be
//! wrong while every other test in this crate stayed green.
//!
//! Schema 2 (`vike_secrets::schema`) created a real `account` table and, as the change that landed
//! it says in its own subject line, *no reader notices*: the only `SELECT … FROM account` in the
//! tree was inside the migration that fills it. `vike_secrets::read_accounts` and
//! `vike_secrets::resolve_accounts_in` are that reader. This file is the proof that it ANSWERS, and
//! the proof that its three states stay three.
//!
//! | test | what would be true without it |
//! |---|---|
//! | [`the_table_holds_one_row_per_account_and_the_ownerless_keys_add_none`] | a reader that counts credential rows, or that gives a venue-scoped key an account of its own |
//! | [`dukascopy_is_two_rows_of_one_venue_at_one_tier_and_only_the_id_tells_them_apart`] | the whole premise: two accounts that `(venue, tier, label)` cannot separate, separated by `id` |
//! | [`no_migrated_account_carries_a_label_and_the_reader_synthesises_none`] | a reader rendering `DEFAULT` — or `DEMO1` — for a `NULL` the owner refused to write |
//! | [`every_migrated_account_has_a_null_venue_account_id`] | *the book is unknown* read as *this account has no book*, and the §11 debt going unrecorded |
//! | [`a_file_store_cannot_answer_while_its_credentials_answer_perfectly_well`] | ⚠ the one that matters: *no accounts* reported for a store holding every credential it ever held |
//! | [`a_schema_1_database_says_it_predates_the_table`] | a raw `no such table: account` handed to an unmigrated box as if the store had malfunctioned |
//! | [`active_for_venue_never_collapses_cannot_ask_into_none_found`] | the same merge, made one convenience method lower down |
//! | [`the_reader_creates_no_database_on_a_box_that_has_none`] | a READ that turns *this box has no database* into *this box has an empty database* — every venue silently on paper |
//! | [`nothing_the_reader_returns_is_a_credential_value`] | an account row that grew a value, or an error message that quoted one |
//!
//! # The fixture is small ON PURPOSE
//!
//! `tests/database_migration.rs` carries the live box's whole 67-name store, because a MIGRATION is
//! only interesting on the fifty-seven names outside the generated grid. A READER is not: what it
//! has to get right is the SHAPE of the `account` table, and the interesting shape is nine keys
//! wide — two accounts of one venue at one tier, an ordinary account, an account at another tier,
//! and the two key classes that own no account at all.
//!
//! ⚠ Values are obviously fake and are never asserted on beyond *no value came back*.

use std::path::{Path, PathBuf};

use vike_secrets::{Accounts, Backend, NoAccountTable};

// ---------------------------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------------------------

/// Nine names covering the four classifications an account reader can be wrong about.
///
/// * `DUKASCOPY_DEMO{1,2}_*` — **two accounts of ONE venue at ONE tier**, the case
///   `docs/superpowers/specs/2026-09-14-the-credential-schema.md` §1 is about and the reason the
///   account is a row at all. `UNIQUE (venue, tier, label)` cannot separate them, because both
///   labels are `NULL`.
/// * `BINANCE_DEMO_*` — an ordinary single account, two keys, one row.
/// * `HYPERLIQUID_LIVE_PRIVATE_KEY` — a second TIER, so a reader that keyed on venue alone folds it
///   into something else.
/// * `CTRADER_CLIENT_ID` — **venue-scoped** (§5.1): an application credential shared by every
///   account of the venue. It owns no account row.
/// * `CLOUDFLARE_API_TOKEN` — **infrastructure**: the deployment's own credential, no venue, no
///   account row.
const FIXTURE_KEYS: [&str; 9] = [
    "BINANCE_DEMO_API_KEY",
    "BINANCE_DEMO_API_SECRET",
    "CLOUDFLARE_API_TOKEN",
    "CTRADER_CLIENT_ID",
    "DUKASCOPY_DEMO1_LOGIN",
    "DUKASCOPY_DEMO1_PASSWORD",
    "DUKASCOPY_DEMO2_LOGIN",
    "DUKASCOPY_DEMO2_PASSWORD",
    "HYPERLIQUID_LIVE_PRIVATE_KEY",
];

/// The four accounts [`FIXTURE_KEYS`] describes, as `(venue, tier)` — dukascopy twice, deliberately.
///
/// Written out rather than derived from the classifier below, because deriving it would make this
/// file's central assertion a restatement of its own helper: if [`classify`] were wrong about
/// dukascopy, a derived expectation would be wrong in exactly the same way and the test would pass.
const EXPECTED_ACCOUNTS: [(&str, &str); 4] =
    [("binance", "demo"), ("dukascopy", "demo"), ("dukascopy", "demo"), ("hyperliquid", "live")];

/// Nothing in this fixture is a node key.
fn is_node_key(_key: &str) -> bool {
    false
}

/// The account classification the production callers pass
/// (`vike_bridge_core::credentials::classify_credential_name`), spelled here because this crate
/// cannot link the crate that owns it — the same seam, and the same reason, as
/// `tests/database_migration.rs`'s own copy.
///
/// ⚠ Deliberately simpler than the production rule, and that is what makes it a test: every
/// assertion below is about what the READER does with an `account` table, never about how a name
/// reached one. `crates/vike-bridge-core/tests/credential_classification.rs` holds the real rows.
fn classify(name: &str) -> vike_secrets::Classification {
    use vike_secrets::{Classification, Placement};

    // The hand-map: two accounts of one venue at one tier. The DISCRIMINATOR reaches no column —
    // it is how the classifier says *these are two* without the index token becoming an identity
    // again, which is the defect §1 is about.
    for (prefix, disc) in [("DUKASCOPY_DEMO1_", "DEMO1"), ("DUKASCOPY_DEMO2_", "DEMO2")] {
        if let Some(field) = name.strip_prefix(prefix) {
            return account("dukascopy", "demo", Some(disc), field);
        }
    }
    // cTrader's OAuth APPLICATION pair — venue-scoped, no tier token, no account of its own.
    if name == "CTRADER_CLIENT_ID" {
        return Classification {
            placement: Placement::Venue("ctrader".to_string()),
            field: "CLIENT_ID".to_string(),
            secret: true,
            recognised: true,
            pending_move: None,
        };
    }
    for (prefix, venue, tier) in
        [("BINANCE_DEMO_", "binance", "demo"), ("HYPERLIQUID_LIVE_", "hyperliquid", "live")]
    {
        if let Some(field) = name.strip_prefix(prefix) {
            return account(venue, tier, None, field);
        }
    }
    // `CLOUDFLARE_API_TOKEN` lands here: the deployment's own credential, filed as infrastructure
    // with `account_id` and `venue` both NULL, and therefore owning no account row.
    Classification::unrecognised(name)
}

fn account(
    venue: &str,
    tier: &str,
    discriminator: Option<&str>,
    field: &str,
) -> vike_secrets::Classification {
    vike_secrets::Classification {
        placement: vike_secrets::Placement::Account(vike_secrets::AccountKey {
            venue: venue.to_string(),
            tier: tier.to_string(),
            // ⚠ NEVER a label. The owner refused the provisional `DEMO1`/`DEMO2` spellings at the
            // spec's signature — *labels are informative and optional, `id` is the identity* — so
            // the fixture writes what the migration writes: NULL.
            label: None,
            discriminator: discriminator.map(str::to_string),
        }),
        field: field.to_string(),
        secret: true,
        recognised: true,
        pending_move: None,
    }
}

/// A fake value that is stable per key, so an assertion can prove a value did NOT come back without
/// any real credential existing anywhere near this file.
fn fake_value(key: &str) -> String {
    format!("value-for-{key}")
}

struct Fixture {
    _dir: tempfile::TempDir,
    settings: PathBuf,
}

impl Fixture {
    /// A settings directory holding a credential FILE with [`FIXTURE_KEYS`] and no database.
    fn file_store() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = dir.path().join("settings");
        std::fs::create_dir_all(&settings).expect("settings dir");
        let text: String =
            FIXTURE_KEYS.iter().map(|k| format!("{k}={}\n", fake_value(k))).collect();
        std::fs::write(settings.join("secrets.env"), text).expect("write fixture store");
        Fixture { _dir: dir, settings }
    }

    /// …and the same directory after `vike-cli secrets migrate` would have run over it.
    fn migrated() -> Fixture {
        let fx = Fixture::file_store();
        match vike_secrets::migrate(fx.arg(), is_node_key, &classify) {
            Ok(_) => fx,
            Err(e) => panic!("migration refused: {e}"),
        }
    }

    fn arg(&self) -> Option<&str> {
        Some(self.settings.to_str().expect("utf-8 temp path"))
    }

    fn dir(&self) -> &Path {
        &self.settings
    }

    fn db(&self) -> PathBuf {
        self.settings.join("db").join("vike.db")
    }

    fn accounts(&self) -> Accounts {
        vike_secrets::resolve_accounts_in(self.dir()).expect("the store opened")
    }
}

/// Every `(venue, tier)` the reader answered with, sorted — the multiset, so dukascopy's two rows
/// are two entries and a reader that deduplicated them fails here.
fn venue_tiers(accounts: &Accounts) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = accounts
        .known()
        .expect("the store answered")
        .iter()
        .map(|a| (a.venue.clone(), a.tier.clone()))
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------------------------
// PROOF 1 — the table answers, with one row per ACCOUNT
// ---------------------------------------------------------------------------------------------

/// **Nine credential names, four account rows** — and the two that own no account contribute none.
///
/// A reader that simply counted rows of `credential`, or that gave `CTRADER_CLIENT_ID` and
/// `CLOUDFLARE_API_TOKEN` accounts of their own, passes nothing here: the count is 4 against 9
/// names, and the multiset of `(venue, tier)` pins WHICH four rather than how many.
#[test]
fn the_table_holds_one_row_per_account_and_the_ownerless_keys_add_none() {
    let fx = Fixture::migrated();
    let accounts = fx.accounts();

    let expected: Vec<(String, String)> =
        EXPECTED_ACCOUNTS.iter().map(|(v, t)| ((*v).to_string(), (*t).to_string())).collect();
    assert_eq!(venue_tiers(&accounts), expected, "the account rows the reader answered with");

    // Every row carries the identity the schema says is the identity — distinct, and never zero.
    let rows = accounts.known().expect("answered");
    let mut ids: Vec<i64> = rows.iter().map(|a| a.id).collect();
    let before = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), before, "two account rows shared an id: {rows:?}");
}

// ---------------------------------------------------------------------------------------------
// PROOF 2 — ⚠ the whole premise: two accounts the NAME-shaped key cannot separate
// ---------------------------------------------------------------------------------------------

/// **Dukascopy is TWO rows of one venue at one tier, both unlabelled, and only `id` tells them
/// apart.**
///
/// This is the assertion the schema exists for. `vike_model::account_keys::accounts_in_store` — the
/// name-parsing reader every caller uses today — answers `None` for both of these keys, because the
/// grammar deliberately does not retro-fit a venue that bakes an account INDEX into its tier token.
/// The database says what the names cannot — and since 2026-09-15 it is what the MOUNT reads:
/// `vike_mount`'s `dukascopy` module maps one of these rows onto one Dukascopy broker through its
/// credential-key owner prefix, so this test's two rows are the input to a live routing decision
/// rather than a reader's curiosity. (⚠ This sentence used to end
/// "`vike_config::ArmingBlock::NoAccountSupport` is what an operator gets instead"; that refusal is
/// retired for this venue.)
///
/// ⚠ It also pins what is NOT there: `(venue, tier, label)` is IDENTICAL across the two rows, so
/// `UNIQUE (venue, tier, label)` constrains nothing here — `vike_secrets::schema::DDL`'s own note
/// says so — and any reader that keyed accounts on that tuple would silently answer with one
/// account where there are two. Keying on `id` is not a preference.
#[test]
fn dukascopy_is_two_rows_of_one_venue_at_one_tier_and_only_the_id_tells_them_apart() {
    let fx = Fixture::migrated();
    let accounts = fx.accounts();

    let duka = accounts.active_for_venue("dukascopy").expect("the store answered");
    assert_eq!(duka.len(), 2, "the two measured dukascopy demo accounts: {duka:?}");

    assert!(duka.iter().all(|a| a.tier == "demo"), "{duka:?}");
    assert!(duka.iter().all(|a| a.label.is_none()), "a label was synthesised: {duka:?}");
    assert_ne!(duka[0].id, duka[1].id, "the two accounts share an id: {duka:?}");

    // The tuple that CANNOT tell them apart — asserted so a future reader keyed on it fails here
    // rather than in a mount.
    assert_eq!(
        (&duka[0].venue, &duka[0].tier, &duka[0].label),
        (&duka[1].venue, &duka[1].tier, &duka[1].label),
        "`(venue, tier, label)` separated the two rows — if this is now true, say so in the DDL's \
         label note before keying anything on it"
    );
}

// ---------------------------------------------------------------------------------------------
// PROOF 3 — the two columns the migration leaves NULL, pinned as debts rather than as answers
// ---------------------------------------------------------------------------------------------

/// **No migrated account carries a label, and the reader invents none.**
///
/// The owner refused the provisional `DEMO1`/`DEMO2` labels outright at the spec's signature —
/// *labels are informative and optional, `id` is the identity*. Two spellings would quietly put one
/// back: rendering `RESERVED_DEFAULT_LABEL` for a `NULL` (a string
/// `vike_model::account_keys::AccountLabel::parse` refuses as reserved, so it would not round-trip),
/// or rendering the index token (an identity in the one column this schema exists to stop carrying
/// one). `Option<String>` is how the reader declines both.
#[test]
fn no_migrated_account_carries_a_label_and_the_reader_synthesises_none() {
    let fx = Fixture::migrated();
    let accounts = fx.accounts();
    let rows = accounts.known().expect("answered");

    assert!(
        rows.iter().all(|a| a.label.is_none()),
        "a label appeared on a migrated row; the migration writes none: {rows:?}"
    );
    // …and the refused spellings are nowhere in the answer at all.
    let rendered = format!("{rows:?}");
    for refused in ["DEFAULT", "DEMO1", "DEMO2"] {
        assert!(
            !rendered.contains(refused),
            "the reader rendered the refused label spelling {refused}: {rendered}"
        );
    }
}

/// **⚠ `venue_account_id` is NULL on EVERY migrated row — the BOOK is unknown, not absent.**
///
/// Not a dukascopy peculiarity: `vike_secrets::schema`'s module doc records that §11's steps 3 and
/// 4 are deliberately not performed, so the ten book keys are still `credential` rows carrying
/// their legacy names and NOTHING folds them into this column. §12 names the handshake write path
/// as owed.
///
/// This test is the DEBT, written down where it will be read. It is expected to go red the day the
/// fold lands, and going red is what it is for — at that moment a reader that had started treating
/// `None` as *this account has no book* would begin answering wrongly, and nothing else in the tree
/// would notice.
#[test]
fn every_migrated_account_has_a_null_venue_account_id() {
    let fx = Fixture::migrated();
    let accounts = fx.accounts();
    let rows = accounts.known().expect("answered");

    assert!(
        rows.iter().all(|a| a.venue_account_id.is_none()),
        "a book identifier appeared: the fold of spec §11 steps 3-4 has landed, so this test and \
         every caller reading `venue_account_id` as *unknown* must be revisited: {rows:?}"
    );
    // The two other columns nothing in this tree writes yet, pinned for the same reason.
    assert!(rows.iter().all(|a| a.last_verified_at.is_none()), "{rows:?}");
    assert!(rows.iter().all(|a| a.parent_id.is_none()), "{rows:?}");
    // …and `active` DEFAULTs to 1, which is what makes `active_for_venue` answer at all.
    assert!(rows.iter().all(|a| a.active), "{rows:?}");
}

// ---------------------------------------------------------------------------------------------
// PROOF 4 — ⚠ THE ONE THAT MATTERS: "cannot ask" is not "none found"
// ---------------------------------------------------------------------------------------------

/// **A file store says it CANNOT ANSWER — while every one of its credentials answers perfectly
/// well.**
///
/// `vike_secrets::Backend` is a per-RUN choice and a box that has not migrated has no `account`
/// table on it. The failure this is written against is the reader answering `Known(vec![])` there:
/// a caller would read *this store has no accounts*, which downstream is indistinguishable from
/// *this venue has no credentials* — and THAT is the live gate, every venue silently on paper with
/// `secrets.env` sitting on disk looking exactly right.
///
/// The two halves are asserted together on purpose. Either alone is satisfiable by a broken reader:
/// the third state without the credentials would be a reader that had simply stopped working, and
/// the credentials without the third state is the bug.
#[test]
fn a_file_store_cannot_answer_while_its_credentials_answer_perfectly_well() {
    let fx = Fixture::file_store();
    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Files);

    // Half one: the credentials are all there.
    let resolved = vike_secrets::resolve_project(fx.arg()).expect("the file store opened");
    let names: Vec<&str> = resolved.secrets.keys().collect();
    assert_eq!(names.len(), FIXTURE_KEYS.len(), "the file store's own names: {names:?}");

    // Half two: and the account question is nonetheless UNANSWERABLE, naming the store that is
    // answering so the caller knows where to go instead.
    let accounts = fx.accounts();
    assert_eq!(accounts.known(), None, "a file store answered with a row list: {accounts:?}");
    let why = accounts.unanswerable().expect("a reason");
    match why {
        NoAccountTable::FileStore { file } => {
            assert!(file.ends_with("secrets.env"), "{file:?}");
        }
        other => panic!("a file store reported {other:?}"),
    }
    assert!(why.to_string().contains("key names"), "{why}");
}

/// **A schema-1 database says it PREDATES the table**, rather than handing back the engine's
/// `no such table: account`.
///
/// `vike_secrets::READABLE_SCHEMA_VERSIONS` keeps a schema-1 store readable deliberately — it is
/// what lets a v2 binary deploy to a box that has not migrated — so this is a live configuration
/// and not a corrupt one. A raw SQLite error would describe it as a malfunction; it is a
/// classification, and `vike-cli secrets migrate` is the answer.
#[test]
fn a_schema_1_database_says_it_predates_the_table() {
    let fx = Fixture::file_store();
    let rows: Vec<(String, String)> =
        FIXTURE_KEYS.iter().map(|k| ((*k).to_string(), fake_value(k))).collect();
    std::fs::create_dir_all(fx.db().parent().expect("db dir")).expect("db dir");
    vike_secrets::plant_schema_1(&fx.db(), &rows, &[]).expect("plant a schema-1 store");

    // The credentials answer — out of the DATABASE this time, which is the half that makes the
    // third state necessary rather than merely tidy.
    let resolved = vike_secrets::resolve_project(fx.arg()).expect("the schema-1 store opened");
    assert_eq!(resolved.secrets.keys().count(), FIXTURE_KEYS.len());

    let accounts = fx.accounts();
    assert_eq!(accounts.known(), None, "a schema-1 store answered with rows: {accounts:?}");
    match accounts.unanswerable().expect("a reason") {
        NoAccountTable::OlderSchema { found, path } => {
            assert_eq!(*found, 1);
            assert!(path.ends_with("vike.db"), "{path:?}");
        }
        other => panic!("a schema-1 store reported {other:?}"),
    }
}

/// **The same refusal, one convenience method lower down.**
///
/// `active_for_venue` is where a caller is most likely to write `.unwrap_or_default()` and turn
/// *cannot ask* into *none found*, so the distinction is asserted at that call shape too — and
/// beside it the answer that genuinely IS "none found", so the two are visibly different values
/// rather than different documentation.
#[test]
fn active_for_venue_never_collapses_cannot_ask_into_none_found() {
    let unmigrated = Fixture::file_store();
    let unanswerable = unmigrated.accounts();
    assert_eq!(
        unanswerable.active_for_venue("dukascopy"),
        None,
        "an unmigrated box answered a per-venue account question"
    );

    let migrated = Fixture::migrated();
    let accounts = migrated.accounts();
    // A venue this store genuinely holds no account for — `Some(empty)`, which is an ANSWER.
    let okx = accounts.active_for_venue("okx").expect("the store answered");
    assert!(okx.is_empty(), "{okx:?}");
    // …and one it does.
    assert_eq!(accounts.active_for_venue("binance").expect("answered").len(), 1);
}

// ---------------------------------------------------------------------------------------------
// PROOF 5 — the reader writes nothing, and returns nothing that is a secret
// ---------------------------------------------------------------------------------------------

/// **Reading the accounts of a box with no database creates no database.**
///
/// A reader that opened read-WRITE would create the file, and `vike_secrets::backend_at` is one
/// `is_file`: from that moment the box answers `Database` for every process on it, the credential
/// file beside it is never read again, and every venue drops to paper while the store sits there
/// looking correct. `open_for_read`'s `SQLITE_OPEN_READ_ONLY` is what prevents it; this is the
/// assertion that the account path goes through it.
#[test]
fn the_reader_creates_no_database_on_a_box_that_has_none() {
    let fx = Fixture::file_store();
    let _ = fx.accounts();
    let _ = vike_secrets::resolve_accounts(fx.arg()).expect("the store opened");

    assert!(!fx.db().exists(), "the reader created {}", fx.db().display());
    assert_eq!(vike_secrets::workspace_backend_from(fx.arg()), Backend::Files);
    // …and the file it did not read is still exactly what it was.
    assert_eq!(
        vike_secrets::resolve_project(fx.arg()).expect("still readable").secrets.keys().count(),
        FIXTURE_KEYS.len()
    );
}

/// **Nothing the reader returns is a credential value** — not a row, not a reason, not a `Debug`.
///
/// An account row carries identity and nothing else, and `read_accounts` selects from `account`
/// alone. The assertion is over the rendered `Debug` of the whole answer rather than over the
/// struct's fields, because the failure it is written against is a field ADDED later — a `value`
/// convenience, a joined `credential` row — which a field-by-field check would not see.
#[test]
fn nothing_the_reader_returns_is_a_credential_value() {
    let migrated = Fixture::migrated();
    let rendered = format!("{:?}", migrated.accounts());
    assert!(!rendered.contains("value-for-"), "a credential value reached the reader: {rendered}");

    // The unanswerable arms name paths and a schema number, never a value.
    let files = Fixture::file_store();
    let accounts = files.accounts();
    let why = accounts.unanswerable().expect("a reason");
    assert!(!why.to_string().contains("value-for-"), "{why}");
    assert!(!format!("{why:?}").contains("value-for-"), "{why:?}");
}
