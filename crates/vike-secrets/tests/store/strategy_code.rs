//! **`strategy_code`: insert-or-get by `(mount_id, strategy_name)`, never reused, `0` never handed
//! out, and a refusal in words at the cap** — through the public writer and reader, with the table
//! itself asked by SQL where the claim is about the table.

use std::collections::BTreeSet;

use vike_model::orders::client_id::OrderClientIdStrategyCode;
use vike_secrets::StoreErrorKind;
use vike_secrets::strategy_code::{StrategyCodeRow, strategy_code_for, strategy_name_for};

use crate::support::{Fixture, sql};

const MOUNT: &str = "bybit__BTCUSDT__30m";

fn code(fx: &Fixture, mount: &str, name: &str) -> i64 {
    strategy_code_for(fx.dir(), mount, name).unwrap_or_else(|e| panic!("{mount}/{name}: {e}"))
}

fn rows(fx: &Fixture) -> i64 {
    fx.conn().query_row("SELECT count(*) FROM strategy_code", [], |r| r.get(0)).expect("count")
}

#[test]
fn the_same_key_answers_the_same_code_across_reopens() {
    let fx = Fixture::initialised();
    let first = code(&fx, MOUNT, "ema_cross");
    // Every call opens its own connection, so each one below is a reopen of the store.
    for _ in 0..3 {
        assert_eq!(code(&fx, MOUNT, "ema_cross"), first, "a re-mount or a restart keeps the code");
    }
    assert_eq!(rows(&fx), 1, "asking again wrote no second row");
    assert_eq!(
        strategy_name_for(fx.dir(), first).expect("read"),
        Some(StrategyCodeRow { mount_id: MOUNT.into(), strategy_name: "ema_cross".into() })
    );
}

#[test]
fn two_keys_never_share_a_code_and_zero_is_never_one() {
    let fx = Fixture::initialised();
    let a = code(&fx, MOUNT, "ema_cross");
    let b = code(&fx, MOUNT, "rsi_revert");
    let c = code(&fx, "binance__ETHUSDT__1h", "ema_cross");
    assert_eq!(a, 1, "the first code is 0001: 0000 is \"no strategy\"");
    assert_eq!(BTreeSet::from([a, b, c]).len(), 3, "another name or another mount is another code");
    assert_eq!(strategy_name_for(fx.dir(), 0).expect("read"), None, "0 names no strategy");
    assert_eq!(strategy_name_for(fx.dir(), 4).expect("read"), None, "a code no row holds");
}

#[test]
fn a_code_is_never_handed_out_twice_whatever_the_sequence() {
    let fx = Fixture::initialised();
    let a = code(&fx, MOUNT, "a");
    let b = code(&fx, MOUNT, "b");
    let c = code(&fx, MOUNT, "c");
    // No path here deletes a row; an operator with `sqlite3` can. The code stays spent.
    fx.conn().execute("DELETE FROM strategy_code WHERE id = ?1", [c]).expect("hand delete");
    let d = code(&fx, MOUNT, "d");
    let c_again = code(&fx, MOUNT, "c");
    assert_eq!(
        (code(&fx, MOUNT, "a"), code(&fx, MOUNT, "b")),
        (a, b),
        "held keys keep their codes"
    );
    let all = [a, b, c, d, c_again];
    assert_eq!(BTreeSet::from(all).len(), all.len(), "no code came back: {all:?}");
    assert!(d > c && c_again > d, "a freed code is skipped, never reissued: {all:?}");
    assert_eq!(
        strategy_name_for(fx.dir(), c).expect("read"),
        None,
        "the deleted code names nothing"
    );
}

#[test]
fn the_cap_refuses_in_words_and_writes_nothing() {
    let fx = Fixture::initialised();
    let held = code(&fx, MOUNT, "held");
    // Stand the AUTOINCREMENT mark one short of the cap: the next new key gets ZZZZ.
    fx.conn()
        .execute(
            "UPDATE sqlite_sequence SET seq = ?1 WHERE name = 'strategy_code'",
            [OrderClientIdStrategyCode::MAX_ID - 1],
        )
        .expect("plant the mark");
    assert_eq!(
        code(&fx, MOUNT, "last"),
        OrderClientIdStrategyCode::MAX_ID,
        "ZZZZ is the last code handed out"
    );

    let err = strategy_code_for(fx.dir(), MOUNT, "one_too_many").expect_err("past the cap");
    assert!(
        matches!(&err.kind, StoreErrorKind::StrategyCodesExhausted { mount_id, strategy_name }
            if mount_id == MOUNT && strategy_name == "one_too_many"),
        "{err:?}"
    );
    let words = err.to_string();
    for said in
        ["every strategy code", "never reuses one", "one_too_many", MOUNT, "NOTHING WAS WRITTEN"]
    {
        assert!(words.contains(said), "the refusal must say {said:?}: {words}");
    }
    assert_eq!(rows(&fx), 2, "the refusal wrote no row");
    assert_eq!(code(&fx, MOUNT, "held"), held, "a key that holds a code still answers at the cap");
    assert_eq!(code(&fx, MOUNT, "last"), OrderClientIdStrategyCode::MAX_ID);
}

#[test]
fn the_table_refuses_zero_and_a_code_past_zzzz() {
    let fx = Fixture::initialised();
    code(&fx, MOUNT, "creates_the_table");
    let insert = |id: i64| {
        fx.conn().execute(
            "INSERT INTO strategy_code (id, mount_id, strategy_name, assigned_at) \
             VALUES (?1, 'm', ?2, datetime('now'))",
            rusqlite::params![id, format!("by_hand_{id}")],
        )
    };
    insert(0).expect_err("0 is \"no strategy\" and never a row");
    insert(OrderClientIdStrategyCode::MAX_ID + 1)
        .expect_err("the CHECK and OrderClientIdStrategyCode::MAX_ID are one number");
    insert(OrderClientIdStrategyCode::MAX_ID).expect("ZZZZ itself is a code");
}

#[test]
fn two_connections_never_get_one_code() {
    let fx = Fixture::initialised();
    let dir = fx.dir().to_path_buf();
    let answers: Vec<(i64, i64)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let dir = dir.clone();
                s.spawn(move || {
                    let own = strategy_code_for(&dir, MOUNT, &format!("own_{i}")).expect("own");
                    let shared = strategy_code_for(&dir, MOUNT, "shared").expect("shared");
                    (own, shared)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("thread")).collect()
    });
    let own: BTreeSet<i64> = answers.iter().map(|(o, _)| *o).collect();
    let shared: BTreeSet<i64> = answers.iter().map(|(_, s)| *s).collect();
    assert_eq!(own.len(), 8, "eight keys, eight codes: {answers:?}");
    assert_eq!(shared.len(), 1, "one key, one code, whoever asked first: {answers:?}");
    assert!(own.is_disjoint(&shared), "{answers:?}");
    assert_eq!(rows(&fx), 9);
}

#[test]
fn a_store_born_before_the_table_gains_it_on_the_first_call() {
    let fx = Fixture::empty();
    let older = vike_secrets::DDL
        .split("CREATE TABLE IF NOT EXISTS strategy_code")
        .next()
        .expect("the batch before the table");
    assert_ne!(older, vike_secrets::DDL, "the cut must find the table in `DDL`");
    let conn = sql::plant_ddl(&fx.db(), older);
    sql::stamp_version(&conn, vike_secrets::SCHEMA_VERSION);
    drop(conn);

    assert_eq!(strategy_name_for(fx.dir(), 1).expect("no table is no row"), None);
    assert_eq!(code(&fx, MOUNT, "ema_cross"), 1);
    assert_eq!(
        strategy_name_for(fx.dir(), 1).expect("read").map(|r| r.strategy_name),
        Some("ema_cross".into())
    );
}

#[test]
fn no_store_is_refused_and_none_is_created() {
    let fx = Fixture::empty();
    let err = strategy_code_for(fx.dir(), MOUNT, "ema_cross").expect_err("no store");
    assert!(matches!(err.kind, StoreErrorKind::NoSettingsDatabase), "{err:?}");
    assert!(!fx.db().exists(), "the writer created a database");
    assert_eq!(strategy_name_for(fx.dir(), 1).expect("no store reads empty"), None);
}
