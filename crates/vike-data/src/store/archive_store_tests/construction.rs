//! Construction, the read-only refusals, and every kind this store cannot hold.

use super::*;

// ---- construction ------------------------------------------------------------------------

#[test]
fn from_dir_picks_up_every_parquet_file_sorted_and_merges_across_files() {
    let dir = tempfile::tempdir().unwrap();
    let day1: Vec<Row<'_>> =
        vec![("TOK_A", 100, 100, 1, "book", "none", 0.0, 0.0, "[[0.5,1.0]]", "[]", 0.01, "")];
    let day2: Vec<Row<'_>> =
        vec![("TOK_A", 200, 200, 1, "price_change", "buy", 0.6, 2.0, "", "", 0.01, "")];
    write_temp_parquet(dir.path(), "btc5m_2026-07-27.parquet", &write_family_parquet(&day1, None));
    write_temp_parquet(dir.path(), "btc5m_2026-07-28.parquet", &write_family_parquet(&day2, None));
    // A non-Parquet file in the same directory must be ignored.
    std::fs::write(dir.path().join("manifest.json"), b"{}").unwrap();

    let store = ArchiveParquetHistStore::from_dir(dir.path()).unwrap();
    assert_eq!(store.files().len(), 2, "only the two .parquet files, not manifest.json");

    let out = store.scan_book_updates(VENUE, "TOK_A", TsRange::all()).unwrap();
    assert_eq!(out.len(), 2, "rows merged across both day files");
    assert_eq!(out[0].ts, 100);
    assert_eq!(out[1].ts, 200);
}

#[test]
fn writes_are_rejected() {
    let store = ArchiveParquetHistStore::from_files(Vec::new());
    assert!(store.append_bars(VENUE, "T", "1m", &[], None).is_err());
    assert!(store.append_quotes(VENUE, "T", &[], None).is_err());
    assert!(store.append_trades(VENUE, "T", &[], None).is_err());
    assert!(store.append_book_updates(VENUE, "T", &[], None).is_err());
    assert!(store.resample_quotes_to_bars(VENUE, "T", "1m", TsRange::all(), None).is_err());
    assert!(store.resample_trades_to_bars(VENUE, "T", "1m", TsRange::all(), None).is_err());
}

/// Every lane this store CANNOT hold says so, says WHICH, and says it on the WRITE side too.
///
/// ⚠ `scan_funding` was the first (`docs/decisions/0080-the-account-funding-kind-takes-the-qualified-name.md`
/// verdict 6, 2026-09-21) and stood alone here under a sibling test that PINNED the other four
/// as still answering `[]`. That pin is now deleted, because the four joined it: the three
/// account arms are this store's own (`ArchiveParquetHistStore::scan_equity`'s doc carries the
/// argument) and `scan_chain` came through the trait default, which refuses for every impl that
/// never wrote an arm.
///
/// The message must NAME the kind in the store's own `kind=` vocabulary — an operator reading
/// `hist unsupported: …` has to learn which question was refused, since the cure is a different
/// store rather than a fix here.
///
/// ⚠ **The two APPEND cases are here because for one review round NOTHING IN THE TREE executed
/// those two default bodies**, and they are the more dangerous half rather than a symmetry:
/// `crate::ChainRecorder`'s `record` logs a store error and drops the batch, so under the old
/// `Ok(0)` a recorder aimed at a chain-less store had nothing to log — it recorded for hours and
/// reported success every cadence bucket. The tree declares exactly THREE overrides of either
/// verb — `crate::DataFusionHist`, `crate::test_support::MemHistStore` and
/// `crates/vike-datahub-client/src/remote.rs`'s `RemoteHistStore` — and every existing test
/// drove one of them, while `writes_are_rejected` above drives this store's OWN read-only
/// rejections and names neither of these two. This store inherits both defaults whole and this
/// test calls them, so reverting either body to `Ok(0)` fails right here.
///
/// ⚠ **The ACCOUNT-plane rows are a COMPLETENESS check over `vike_model::ACCOUNT_KINDS`, not a
/// hand-typed list** — the per-roster playbook's shape. Two breaks it exists to catch, neither
/// of which reddened anything while the list was free literals: a FIFTH account kind landing on
/// an empty trait default, which this store would fabricate `[]` for; and a RENAME, which this
/// family has already had once (0079's `funding` → `exec_funding`) and which would otherwise
/// leave `no_account_plane` naming a kind that no longer exists while a test carrying the same
/// stale literal still passed.
#[test]
fn every_kind_this_store_cannot_hold_refuses_and_names_itself() {
    let store = ArchiveParquetHistStore::from_files(Vec::new());
    let cases: Vec<(&str, Result<(), DataError>)> = vec![
        ("equity", store.scan_equity(VENUE, "T", TsRange::all()).map(|_| ())),
        ("exec_fill", store.scan_exec_fills(VENUE, "T").map(|_| ())),
        ("exec_order", store.scan_exec_orders(VENUE, "T").map(|_| ())),
        ("exec_funding", store.scan_funding(VENUE, "T", TsRange::all()).map(|_| ())),
        ("chain", store.scan_chain(VENUE, "T", TsRange::all()).map(|_| ())),
        // The two WRITE halves, both inherited from the trait — this store declares no arm for
        // either, which is why the default bodies execute here and nowhere else.
        ("exec_funding", store.append_funding(VENUE, "T", &[], None).map(|_| ())),
        ("chain", store.append_chain_snapshot(VENUE, "T", &[], None).map(|_| ())),
        // Derived over `scan_chain`, so it must PROPAGATE the refusal rather than fold an
        // inherited empty into a confident "nothing was recorded at or before ts".
        ("chain", store.chain_as_of(VENUE, "T", 1_000).map(|_| ())),
    ];

    // THE ROSTER TIE. `vike_model::ACCOUNT_KINDS` is the one declaration of which kinds hold the
    // operator's own activity, and `crates/vike-ops/tests/venues/store_kind_gate/floor.rs`'s
    // `the_store_agrees_with_the_declared_account_plane` already holds it equal to
    // `STORE_KINDS`. Every entry must be driven above; an undriven one is this store answering
    // `[]` for an account kind with nothing noticing, which is the whole defect below.
    let driven: std::collections::BTreeSet<&str> = cases.iter().map(|(k, _)| *k).collect();
    let undriven: Vec<&str> =
        vike_model::ACCOUNT_KINDS.iter().copied().filter(|k| !driven.contains(k)).collect();
    assert!(
        undriven.is_empty(),
        "the account plane grew or was renamed: {undriven:?} is in vike_model::ACCOUNT_KINDS \
             and is driven by no case here. Add its verb — and check this store REFUSES it rather \
             than inheriting an empty trait default, which is what an unlisted kind gets."
    );

    for (kind, got) in cases {
        // Composed, not spelled: `no_account_plane`'s doc argues why a `"kind=` literal must
        // not appear in this crate outside the path builders.
        let needle = ["kind", kind].join("=");
        match got {
            Err(DataError::Unsupported(msg)) => assert!(
                msg.contains(&needle),
                "the refusal must name the kind it refused as `{needle}`; got: {msg}"
            ),
            other => panic!(
                "an archive of market Parquet must SAY it cannot hold {needle}, not answer \
                     empty; got {other:?}"
            ),
        }
    }

    // ---- THE ANTI-VACUITY CONTROL, and it is INSIDE this test on purpose -------------------
    //
    // What the refusals above are worth depends entirely on this store not refusing
    // EVERYTHING: the claim is that they are a property of the account plane and of the kinds
    // this backend has no lane for, NOT of a handle built from zero files. Only an `is_ok()` on
    // the SAME `store` binding carries that.
    //
    // ⚠ For one review round it did not. The control was a sibling `#[test]` —
    // `bars_and_properties_are_empty_and_that_empty_is_a_real_answer`, itself the survivor of
    // `bars_and_account_series_are_always_empty_not_faked`, which asserted seven empties and
    // lost five as each lane stopped faking one (`scan_quotes` first — a real derived-L1 verb
    // now, and the module doc carries what believing its empty cost — then `scan_funding`, then
    // the three account lanes and `scan_chain`). It built its OWN `from_files(Vec::new())`, so
    // the pairing its doc claimed was two handles and nothing joining them: seeding either one
    // would have left the claim written and held by nothing, both tests green.
    //
    // These two lanes keep their empty `Ok` deliberately —
    // `ArchiveParquetHistStore::scan_equity`'s doc argues the asymmetry, and
    // `crates/vike-backtest/src/hist_replay.rs`'s per-tick `properties_source` is the caller a
    // refusal would make WORSE.
    assert_eq!(store.load_bars(VENUE, "T", "1m", TsRange::all()).unwrap(), vec![]);
    assert_eq!(store.scan_symbol_properties(VENUE, "T", TsRange::all()).unwrap(), vec![]);
    // Derived over `scan_symbol_properties`, so it inherits that empty rather than a refusal.
    assert_eq!(store.properties_as_of(VENUE, "T", 1_000).unwrap(), None);
}

/// ⚠ **The same latent defect is still live on the LAST TWO kinds**, and this test records it
/// rather than leaving the asymmetry to be rediscovered — it is the successor of the pin that
/// covered the four lanes the test above now proves refuse.
///
/// `crate::store::hist`'s cohort section carries the argument for why `cohort` and `perp_metrics` were
/// left: their defaults are FORWARDED by `crates/vike-user-research/src/contract.rs`'s
/// sanctioned study surface, so the refusal reaches user code, which is a different decision
/// from the one made here and wants its own evidence.
///
/// This is a PIN, not a fix: it fails the day somebody makes one of them refuse, which is the
/// day to delete its line here and note the record that did it.
#[test]
fn the_last_two_kinds_still_answer_empty_rather_than_refusing() {
    let store = ArchiveParquetHistStore::from_files(Vec::new());
    assert!(store.scan_cohort(VENUE, "T", TsRange::all()).is_ok());
    assert!(store.scan_perp_metrics(VENUE, "T", TsRange::all()).is_ok());
}
