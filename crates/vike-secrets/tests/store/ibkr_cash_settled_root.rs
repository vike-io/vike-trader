//! **`ibkr_cash_settled_root`: the seed creates the table with the release's roots or touches nothing, a root is added
//! and removed one row at a time, and a box with no store or no table is never written** — through the public writers
//! and reader.

use vike_secrets::StoreErrorKind;
use vike_secrets::ibkr_cash_settled_root::{
    RELEASE_SEED, add_ibkr_cash_settled_root, read_ibkr_cash_settled_roots,
    remove_ibkr_cash_settled_root, seed_ibkr_cash_settled_roots,
};

use crate::support::Fixture;

fn seeded_order() -> Vec<String> {
    RELEASE_SEED.iter().map(|r| (*r).to_string()).collect()
}

#[test]
fn the_seed_creates_the_table_with_the_release_roots_in_their_order_once() {
    let fx = Fixture::initialised();
    assert_eq!(read_ibkr_cash_settled_roots(fx.dir()).expect("read"), None, "not seeded yet");
    assert!(seed_ibkr_cash_settled_roots(fx.dir()).expect("seed"), "the first seed creates it");
    assert_eq!(read_ibkr_cash_settled_roots(fx.dir()).expect("read"), Some(seeded_order()));
    assert!(
        !seed_ibkr_cash_settled_roots(fx.dir()).expect("seed again"),
        "a second seed is a no-op"
    );
    assert_eq!(read_ibkr_cash_settled_roots(fx.dir()).expect("read"), Some(seeded_order()));
}

/// The operator's edits survive every later seed: a removed root stays removed, an added one stays, and an EMPTY
/// table is not refilled.
#[test]
fn the_seed_never_puts_back_what_the_operator_removed() {
    let fx = Fixture::initialised();
    seed_ibkr_cash_settled_roots(fx.dir()).expect("seed");
    assert_eq!(remove_ibkr_cash_settled_root(fx.dir(), "YM").expect("remove"), Some(true));
    assert_eq!(remove_ibkr_cash_settled_root(fx.dir(), "YM").expect("again"), Some(false));
    assert_eq!(add_ibkr_cash_settled_root(fx.dir(), "VX").expect("add"), Some(true));
    assert_eq!(add_ibkr_cash_settled_root(fx.dir(), "VX").expect("again"), Some(false));
    seed_ibkr_cash_settled_roots(fx.dir()).expect("seed again");
    let mut want: Vec<String> = seeded_order().into_iter().filter(|r| r != "YM").collect();
    want.push("VX".into());
    assert_eq!(read_ibkr_cash_settled_roots(fx.dir()).expect("read"), Some(want));

    for root in RELEASE_SEED.iter().copied().chain(["VX"]) {
        remove_ibkr_cash_settled_root(fx.dir(), root).expect("remove");
    }
    seed_ibkr_cash_settled_roots(fx.dir()).expect("seed over an empty table");
    assert_eq!(read_ibkr_cash_settled_roots(fx.dir()).expect("read"), Some(Vec::new()));
}

/// Before the seed there is no table, and neither writer makes one.
#[test]
fn add_and_remove_write_nothing_before_the_seed() {
    let fx = Fixture::initialised();
    assert_eq!(add_ibkr_cash_settled_root(fx.dir(), "ES").expect("add"), None);
    assert_eq!(remove_ibkr_cash_settled_root(fx.dir(), "ES").expect("remove"), None);
    assert_eq!(read_ibkr_cash_settled_roots(fx.dir()).expect("read"), None, "still no table");
}

/// The table's `CHECK` refuses what is not a root, and an `ON CONFLICT` does not swallow it.
#[test]
fn a_root_that_is_not_upper_case_letters_and_digits_is_refused_by_the_table() {
    let fx = Fixture::initialised();
    seed_ibkr_cash_settled_roots(fx.dir()).expect("seed");
    for bad in ["", "es", "E.S", "ES ", "ÉS"] {
        assert!(add_ibkr_cash_settled_root(fx.dir(), bad).is_err(), "{bad:?} is refused");
    }
    assert_eq!(read_ibkr_cash_settled_roots(fx.dir()).expect("read"), Some(seeded_order()));
}

#[test]
fn no_store_is_refused_and_none_is_created() {
    let fx = Fixture::empty();
    for err in [
        seed_ibkr_cash_settled_roots(fx.dir()).map(drop).expect_err("seed"),
        add_ibkr_cash_settled_root(fx.dir(), "ES").map(drop).expect_err("add"),
        remove_ibkr_cash_settled_root(fx.dir(), "ES").map(drop).expect_err("remove"),
    ] {
        assert!(matches!(err.kind, StoreErrorKind::NoSettingsDatabase), "{err}");
    }
    assert!(!fx.db().exists(), "nothing created the database");
    assert_eq!(read_ibkr_cash_settled_roots(fx.dir()).expect("no store reads none"), None);
}
