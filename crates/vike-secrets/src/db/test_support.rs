//! `test-support` constructors: create an empty current-schema store, and build one `account` row by
//! hand.

use super::open::stamp_schema_version;
use super::*;

/// **Plant a finished, CURRENT-schema, otherwise-empty store** — the primitive
/// `crate::profile_store`'s writers refuse to be (`refuse_absent_store` is the FIRST thing every one
/// of them runs, deliberately: see that function's own doc for why a profile write may never be the
/// thing that brings a credential store into existence). A test that wants to seed profile rows
/// (`store_profile`/`set_active`) into a store with no credentials at all reaches one here in a
/// single call. ⚠ It is not the ONLY path: [`create_store`] creates the same empty store
/// (`vike-cli secrets init`), and a test of the PRODUCTION fresh-install path should take that one.
/// This stays for the profile tests that only need a store to exist.
///
/// Behind `test-support`: this crate's OWN production code creates an empty store only through
/// [`create_store`], and this function does exactly that and nothing else —
/// [`open_for_write`]'s base DDL, then the SAME stamp `create_store` applies on a create
/// (`stamp_schema_version`'s call site is the authority for the ordering: inside the transaction,
/// before commit, never left for `open_for_write` to do on its own — see that function's doc for why
/// the stamp is deferred at all).
///
/// # Errors
/// The engine, or the filesystem. [`StoreErrorKind`] via [`StoreError::sql`]/[`StoreError::io`].
#[cfg(feature = "test-support")]
pub fn create_empty_store_for_test(path: &Path) -> Result<(), StoreError> {
    let (mut conn, created) = open_for_write(path)?;
    if created {
        let tx = conn.transaction().map_err(|e| StoreError::sql(path, e))?;
        stamp_schema_version(path, &tx)?;
        tx.commit().map_err(|e| StoreError::sql(path, e))?;
    }
    Ok(())
}

/// **Leave the store in `settings_dir` as a writer killed inside a commit leaves it**: a stamped journal beside a database file
/// the transaction already wrote into. The pair is copied from a scratch twin while its transaction is open, so no signal, no
/// child and no timing, and no process holds a lock on the result (`tests/store/hot_rollback_journal.rs` has the measurement).
///
/// # Panics
/// When the state could not be built: a test on any other state would prove nothing.
#[cfg(feature = "test-support")]
pub fn plant_hot_journal(settings_dir: &Path) {
    let db = crate::store_locator::db_path_in(settings_dir);
    let twin = db.with_extension("hot-twin");
    let with_journal = |path: &Path| {
        let mut journal = path.as_os_str().to_os_string();
        journal.push("-journal");
        PathBuf::from(journal)
    };
    std::fs::copy(&db, &twin).expect("the twin");
    {
        let conn = Connection::open(&twin).expect("open the twin");
        conn.execute_batch(
            "PRAGMA cache_size = 1;
             BEGIN IMMEDIATE;
             CREATE TABLE killed_writer_filler (x TEXT);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5000)
             INSERT INTO killed_writer_filler SELECT hex(zeroblob(200)) FROM n;",
        )
        .expect("a write big enough to spill to the file");
        std::fs::copy(&twin, &db).expect("the database as the kill left it");
        std::fs::copy(with_journal(&twin), with_journal(&db)).expect("its journal");
    }
    std::fs::remove_file(&twin).expect("the twin goes");
    let head = std::fs::read(with_journal(&db)).expect("the journal");
    assert_eq!(
        head.get(..8),
        Some(&[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7][..]),
        "the journal is not stamped, so the engine would not call it hot"
    );
}

/// **One ACTIVE `account` row planted by SQL in the store of `settings_dir`**, answering its id — for
/// a shape `AccountEdit::Create` refuses and an older store may still hold: above all a second ACTIVE
/// row of a venue under one address at another tier (decision 0274; binance or ibkr `demo` AND `live`,
/// both unlabelled, the `TierConflict` every reader must still answer), or a second unlabelled row of
/// one `(venue, tier)`. The store must exist; the venue roster is topped up first, as a writer's.
///
/// # Panics
/// When the row could not be planted: a test on any other state would prove nothing.
#[cfg(feature = "test-support")]
pub fn plant_account_for_test(
    settings_dir: &Path,
    venue: &str,
    tier: vike_model::AccountTier,
    label: Option<&str>,
) -> i64 {
    let path = crate::store_locator::db_path_in(settings_dir);
    let (mut conn, created) = open_for_write(&path).expect("open the store for the plant");
    assert!(!created, "no store at {}: create it before planting a row", path.display());
    let tx = conn.transaction().expect("a transaction");
    ensure_venue_rows(&tx).expect("the venue roster");
    let planted = tx
        .execute(
            "INSERT INTO account (venue_id, tier, label) SELECT id, ?2, ?3 FROM venue WHERE name = ?1",
            (venue, tier.as_str(), label),
        )
        .unwrap_or_else(|e| panic!("planting a ({venue}, {tier}, {label:?}) row failed: {e}"));
    assert_eq!(planted, 1, "the store's venue roster must carry `{venue}`");
    let id = tx.last_insert_rowid();
    tx.commit().expect("commit the plant");
    id
}

/// **The symbol a TEST's account states for its venue's engine**, `(venue, symbol)`: the symbol each venue's engine was
/// mounted on until the owner's ruling of 2026-11-13 moved it into the account's own row (`account.symbol`), which is
/// also what the deploy's runbook writes for every account (`docs/ops/release.md`'s "The account rows the
/// 2026-11-13 ruling needs"). A fixture that states it runs as the tree ran before the ruling. Polymarket is absent: its engine
/// is account-wide and its accounts state none. ⚠ Test code only (`test-support`): production code names no symbol.
// vike:new-venue:note give `{venue}` a row of `FIXTURE_ACCOUNT_SYMBOLS` (the symbol a test's account of it states: one the venue lists, spelled as `vike-cli secrets account set-symbol` stores it) unless its engine is account-wide (`vike_venues::venue_wiring::EngineMount::Account`), and bump the declared length: crates/vike-secrets/src/db/test_support.rs's `FIXTURE_ACCOUNT_SYMBOLS`
pub const FIXTURE_ACCOUNT_SYMBOLS: [(&str, &str); 13] = [
    ("binance", "BTCUSDT"),
    ("bybit", "BTCUSDT"),
    ("okx", "BTC-USDT-SWAP"),
    ("deribit", "BTC-PERPETUAL"),
    ("hyperliquid", "BTC"),
    ("aster", "BTCUSDT.P"),
    ("alpaca", "AAPL"),
    ("ctrader", "EURUSD"),
    ("ig", "CS.D.EURUSD.MINI.IP"),
    ("oanda", "EURUSD"),
    ("ibkr", "AAPL.SMART.USD"),
    ("fxcm", "EURUSD"),
    ("dukascopy", "EURUSD"),
];

/// [`FIXTURE_ACCOUNT_SYMBOLS`]' symbol for `venue`, or `None` (polymarket, or an id the roster does not carry).
#[must_use]
pub fn fixture_account_symbol(venue: &str) -> Option<&'static str> {
    FIXTURE_ACCOUNT_SYMBOLS.iter().find(|(v, _)| *v == venue).map(|(_, symbol)| *symbol)
}

/// One `account` row, built by hand as the store would read it: no venue account id, no parent, never
/// verified, no exposure or leverage override, and the venue's [`fixture_account_symbol`] as the symbol it
/// states (so it runs as an account did before the 2026-11-13 ruling). vike-cli's `secrets set` tests and
/// vike-config's arming tests each carried a copy until 2026-10-31.
pub fn account_row(
    id: i64,
    venue: &str,
    tier: vike_model::AccountTier,
    label: Option<&str>,
    active: bool,
) -> AccountRow {
    AccountRow {
        id,
        venue: venue.to_string(),
        tier,
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
        symbol: fixture_account_symbol(venue).map(str::to_string),
        facts: Default::default(),
    }
}
