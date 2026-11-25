//! **IBKR's cash-settled futures roots at a future's admission, the table created at its first need** (decision 0234,
//! amended 2026-11-16).
//!
//! The owner's ruling of 2026-11-16: one venue's step never sits in the shared server start. The server install's start
//! used to seed the `ibkr_cash_settled_root` table on every box; now IBKR's own wiring creates it, here, at the first
//! dated future the gatewayhub is asked to admit (`bridge_trade.rs`'s `ibkr_roots_for`), and a box that trades no IBKR
//! future never holds the table. The snapshot's read (`crate::gatewayhub_cli`'s `ibkr_cash_settled_roots`) creates
//! nothing: it hands the directory over when the table is absent.
//!
//! The rules are the seed's, unchanged (`vike_secrets::ibkr_cash_settled_root::seed_ibkr_cash_settled_roots`): the table
//! is created holding `RELEASE_SEED` only while it is ABSENT, in ONE transaction; a table that exists, an emptied one
//! included, is never filled again and answers its own rows (an empty table refuses every future). A store that cannot
//! be written or read refuses the future in words. `vike-cli config ibkr-cash-settled-roots seed|add|rm` stay the
//! operator's verbs over the same table.

use std::path::Path;

use super::snapshot::IbkrCashSettledRoots;

/// **The roots a dated future is judged against**: the snapshot's rows, its refusal, or (no table yet) the table
/// created now in the snapshot's settings directory and its rows read back ([`created_roots`]).
///
/// # Errors
/// The words every IBKR future is then refused with.
pub fn roots_for_future(roots: &IbkrCashSettledRoots) -> Result<Vec<String>, String> {
    match roots {
        IbkrCashSettledRoots::Read(roots) => Ok(roots.clone()),
        IbkrCashSettledRoots::Unread(why) => Err(why.clone()),
        IbkrCashSettledRoots::NotCreated(settings_dir) => created_roots(settings_dir),
    }
}

/// Create the table holding the release's roots when it is ABSENT (the seed's own rule and transaction; a table that
/// exists is left exactly as it is), then read its rows. A table this call created is said once, at `info`.
///
/// # Errors
/// The table could not be created, or its rows could not be read: the refusal of every IBKR future, in words.
fn created_roots(settings_dir: &Path) -> Result<Vec<String>, String> {
    use vike_secrets::ibkr_cash_settled_root::{
        RELEASE_SEED, read_ibkr_cash_settled_roots, seed_ibkr_cash_settled_roots,
    };
    match seed_ibkr_cash_settled_roots(settings_dir) {
        Ok(true) => tracing::info!(
            "ibkr: created `ibkr_cash_settled_root` with the release's roots at the first future admission: {}",
            RELEASE_SEED.join(", ")
        ),
        Ok(false) => {}
        Err(e) => {
            return Err(format!(
                "the `ibkr_cash_settled_root` table could not be created with the release's roots, so no IBKR \
                 future is admitted: {e}"
            ));
        }
    }
    match read_ibkr_cash_settled_roots(settings_dir) {
        Ok(Some(roots)) => Ok(roots),
        // Unreachable but by an operator dropping the table between the two calls: said, never guessed.
        Ok(None) => Err("the settings database holds no `ibkr_cash_settled_root` table, so no IBKR future is \
                         admitted: `vike-cli config ibkr-cash-settled-roots seed` creates it with the roots the \
                         release ships"
            .to_string()),
        Err(e) => Err(format!(
            "the `ibkr_cash_settled_root` rows could not be read, so no IBKR future is admitted: {e}"
        )),
    }
}

// Inline, so this ONE file is the credential-writer row (`crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs`'s
// `WRITER_CALLERS`): the suite makes its scratch stores with `vike_secrets::create_store`.
#[cfg(test)]
mod tests {
    use super::*;
    use vike_secrets::ibkr_cash_settled_root::{RELEASE_SEED, read_ibkr_cash_settled_roots};

    /// An EMPTY settings database (no `ibkr_cash_settled_root` table), as a fresh server start leaves it.
    fn fresh_store() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("a temp settings dir");
        vike_secrets::create_store(dir.path().to_str()).expect("the empty store");
        dir
    }

    fn seeded() -> Vec<String> {
        RELEASE_SEED.iter().map(|r| (*r).to_string()).collect()
    }

    /// The first IBKR future admission on a store with no table creates it holding the release's roots and admits on
    /// them; a second admission finds the table and answers its rows.
    #[test]
    fn the_first_future_admission_creates_the_table_with_the_seed() {
        let dir = fresh_store();
        assert_eq!(read_ibkr_cash_settled_roots(dir.path()).expect("read"), None, "a fresh store");
        let absent = IbkrCashSettledRoots::NotCreated(dir.path().to_path_buf());
        let mut first = roots_for_future(&absent).expect("created");
        first.sort();
        let mut want = seeded();
        want.sort();
        assert_eq!(first, want);
        assert!(
            read_ibkr_cash_settled_roots(dir.path()).expect("read").is_some(),
            "the table is there"
        );
        let mut again =
            roots_for_future(&absent).expect("the same snapshot, the table already there");
        again.sort();
        assert_eq!(again, want);
    }

    /// An emptied table stays empty: an admission under a snapshot taken before the table existed creates nothing over
    /// it, and answers no root, so every future is refused.
    #[test]
    fn an_emptied_table_is_never_filled_again() {
        let dir = fresh_store();
        let absent = IbkrCashSettledRoots::NotCreated(dir.path().to_path_buf());
        roots_for_future(&absent).expect("created");
        vike_secrets::hold_write_lock(dir.path())
            .execute_batch("DELETE FROM ibkr_cash_settled_root; COMMIT;")
            .expect("the operator empties the table");
        assert_eq!(roots_for_future(&absent), Ok(Vec::new()));
        assert_eq!(read_ibkr_cash_settled_roots(dir.path()).expect("read"), Some(Vec::new()));
    }

    /// The snapshot's own answers pass through unchanged, and a store that cannot be written refuses in words.
    #[test]
    fn read_rows_and_refusals_pass_through_and_a_bad_store_refuses_in_words() {
        let rows = vec!["ES".to_string()];
        assert_eq!(roots_for_future(&IbkrCashSettledRoots::Read(rows.clone())), Ok(rows));
        let why = "no settings directory".to_string();
        assert_eq!(roots_for_future(&IbkrCashSettledRoots::Unread(why.clone())), Err(why));

        let dir = tempfile::TempDir::new().expect("a temp settings dir");
        let db = vike_secrets::db_path_in(dir.path());
        std::fs::create_dir_all(db.parent().expect("db/")).expect("db/");
        std::fs::write(&db, b"not a SQLite database").expect("plant");
        let refused = roots_for_future(&IbkrCashSettledRoots::NotCreated(dir.path().to_path_buf()))
            .expect_err("an unreadable store admits no future");
        assert!(refused.contains("could not be created with the release's roots"), "{refused}");
    }
}
