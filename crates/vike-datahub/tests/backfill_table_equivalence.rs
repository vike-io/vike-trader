//! **The refactor changed nothing** — 0059 Phase 3's byte-identity proof for the wire verb's
//! dispatch table, built on `crates/vike-config/tests/mirror.rs`'s
//! `the_mirror_changes_no_effective_value` precedent: construct the SAME thing two ways over the
//! same fixtures and demand equality, including the rendered error text, with a companion test
//! proving the new path is genuinely live so the equality cannot be vacuous.
//!
//! Phase 3 replaced `real_backfill_table`'s six hand-written closures — six `Arc::clone`s naming
//! `vike_backfill::backfill_binance_klines` and five siblings — with a fold over
//! `vike_backfill::kline_source::KLINE_SOURCES`. [`pre_collapse_table`] below is those six
//! closures, kept verbatim as a FROZEN BASELINE, and the tests hold the two tables equal.
//!
//! # What this can compare, and what it cannot
//!
//! The observable surface of a `BackfillTable` with no network is: the ORDERED `supported()` list
//! (which is what the unknown-venue refusal prints and what `Welcome` advertises), `get()` for
//! every venue on the roster and off it, and the RESULT of invoking an entry — for the one entry
//! that can answer without a network.
//!
//! That entry is **hyperliquid**, and it is not a lucky accident: its `fetch` refuses a symbol the
//! one-symbol seam cannot express (`identity_coin_for` — HL spot's unified `BASE/QUOTE` name
//! against a `@<pairIndex>` coin) BEFORE any I/O, so both tables can be driven through it and their
//! `Err` strings compared byte for byte. The other five would page a venue, so they are compared
//! structurally only.
//!
//! ⚠ **Stated rather than implied: nothing here can catch an impl whose `fetch` pages the WRONG
//! VENUE** — `BinanceKlines::fetch` calling bybit's pager would pass every assertion in this file.
//! Neither could anything before the collapse; what the collapse removed is the class of bug where
//! the supervisor and the wire verb named DIFFERENT functions for one venue, because there is now
//! one row and both fold it. `crates/vike-ops/tests/collector_dispatch_gate.rs` is what keeps a
//! second list from growing back here.
//!
//! ⚠ **The frozen baseline cannot rot silently.** A seventh `KlineSource` lands in
//! `real_backfill_table`'s output and not in [`pre_collapse_table`], so
//! [`the_registry_fold_builds_the_table_the_six_closures_did`] goes red naming it — a bite, not
//! drift. The failure message says to add the row here too.
//!
//! Behind `backfill-serve` like its siblings: that is the feature under which
//! `real_backfill_table` exists at all.
#![cfg(feature = "backfill-serve")]

use std::sync::Arc;

use tempfile::TempDir;
use vike_data::DataFusionHist;
use vike_datahub::backfill::{BackfillTable, real_backfill_table};

/// A fresh `DataFusionHist` over a temp dir. Every table below gets its OWN — a shared store would
/// let the second of two identical calls answer `Ok(0)` off a spent commit key, and the comparison
/// would pass for the wrong reason.
fn empty_store() -> (TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    (dir, Arc::new(store))
}

/// **THE FROZEN BASELINE**: `real_backfill_table`'s body exactly as it stood before 0059 Phase 3 —
/// six `Arc::clone`s and six closures, each naming one venue's `backfill_<venue>_klines` entry
/// point.
///
/// ⚠ Every spelling is preserved deliberately, because the spellings were part of what the old
/// table had to get right and the fold no longer has to: binance/bybit/okx at the CRATE ROOT
/// (`crates/vike-backfill/src/lib.rs` re-exports exactly those three), aster/deribit/hyperliquid by
/// MODULE PATH, and hyperliquid naming the one-symbol ADAPTER
/// (`backfill_hyperliquid_klines_by_symbol`) rather than the collector proper, which takes a
/// separate venue `coin`.
///
/// ⚠ **Do not "simplify" this into a fold.** It is the independent second construction the
/// comparison is made against; a version of it that read `KLINE_SOURCES` would compare the fold to
/// itself, which is the shape of a test that cannot fail. When a seventh venue is registered, ADD
/// ITS CLOSURE HERE — one line pointing at that venue's own entry point.
fn pre_collapse_table(store: Arc<DataFusionHist>) -> BackfillTable {
    let binance = Arc::clone(&store);
    let bybit = Arc::clone(&store);
    let okx = Arc::clone(&store);
    let aster = Arc::clone(&store);
    let deribit = Arc::clone(&store);
    let hyperliquid = store;
    BackfillTable::new(vec![
        (
            "binance".to_string(),
            Box::new(move |symbol, interval, start, end| {
                vike_backfill::backfill_binance_klines(&binance, symbol, interval, start, end)
                    .map_err(|e| e.to_string())
            }),
        ),
        (
            "bybit".to_string(),
            Box::new(move |symbol, interval, start, end| {
                vike_backfill::backfill_bybit_klines(&bybit, symbol, interval, start, end)
                    .map_err(|e| e.to_string())
            }),
        ),
        (
            "okx".to_string(),
            Box::new(move |symbol, interval, start, end| {
                vike_backfill::backfill_okx_klines(&okx, symbol, interval, start, end)
                    .map_err(|e| e.to_string())
            }),
        ),
        (
            "aster".to_string(),
            Box::new(move |symbol, interval, start, end| {
                vike_backfill::aster::backfill_aster_klines(&aster, symbol, interval, start, end)
                    .map_err(|e| e.to_string())
            }),
        ),
        (
            "deribit".to_string(),
            Box::new(move |symbol, interval, start, end| {
                vike_backfill::deribit::backfill_deribit_klines(
                    &deribit, symbol, interval, start, end,
                )
                .map_err(|e| e.to_string())
            }),
        ),
        (
            "hyperliquid".to_string(),
            Box::new(move |symbol, interval, start, end| {
                vike_backfill::hyperliquid::backfill_hyperliquid_klines_by_symbol(
                    &hyperliquid,
                    symbol,
                    interval,
                    start,
                    end,
                )
                .map_err(|e| e.to_string())
            }),
        ),
    ])
}

/// **The equality.** Same venues, same order, same presence for every venue on the canonical roster
/// and for spellings that are on no roster at all.
#[test]
fn the_registry_fold_builds_the_table_the_six_closures_did() {
    let (_d1, s1) = empty_store();
    let (_d2, s2) = empty_store();
    let before = pre_collapse_table(s1);
    let after = real_backfill_table(s2);

    assert_eq!(
        before.supported(),
        after.supported(),
        "the fold and the six closures disagree about WHICH venues, or in what ORDER. Order is \
         observable — it is what the unknown-venue refusal prints and what `Welcome` advertises. \
         If a seventh `KlineSource` was just registered, add its closure to `pre_collapse_table` \
         (one line, naming that venue's own `backfill_<venue>_klines`); the baseline is this \
         test's independent second construction and must stay hand-spelled."
    );

    for venue in vike_model::VENUES {
        assert_eq!(
            before.get(venue).is_some(),
            after.get(venue).is_some(),
            "the two tables disagree about whether {venue} is dispatchable"
        );
    }
    for absent in ["", "BINANCE", "binance ", "not-a-venue"] {
        assert!(before.get(absent).is_none(), "baseline matched {absent:?}");
        assert!(after.get(absent).is_none(), "the fold matched {absent:?}");
    }
}

/// **The behavioural cell.** The one entry that answers with no network answers IDENTICALLY through
/// both constructions — same `Err`, byte for byte.
///
/// The spellings are hyperliquid's own refusal set: two spot pairs (`/`, whose coin is
/// `@<pairIndex>` — `PURR/USDC` included, the one pair whose coin IS its name and which is refused
/// anyway because a `/` becomes a nested directory in the store), a raw venue coin (`@`), and the
/// empty symbol. Each proves the whole chain — table lookup, closure, the venue's
/// `KlineSource::fetch`, `identity_coin_for`, the `CollectError::Refused` rendering and the
/// `map_err(|e| e.to_string())` the wire does — is intact on both sides.
///
/// ⚠ **Scope it honestly: this is a ROUTE check, not two independent implementations.** After Phase
/// 3 the baseline's `backfill_hyperliquid_klines_by_symbol` and the fold's
/// `backfill_kline_source(…, HyperliquidKlines, …)` converge on the same body BY DESIGN — that
/// convergence is the refactor's claim, and what this proves is that the table still reaches it
/// from both constructions. A mutation inside `HyperliquidKlines::fetch` moves both sides together
/// and is invisible here; what IS caught is a fold that hands hyperliquid's key the wrong closure
/// or no closure at all (MEASURED: keying the fold on `collector_name()` instead of `venue()`
/// reddens this test along with the other three in this file).
#[test]
fn the_one_entry_that_answers_offline_answers_identically() {
    let (_d1, s1) = empty_store();
    let (_d2, s2) = empty_store();
    let before = pre_collapse_table(s1);
    let after = real_backfill_table(s2);

    let old = before.get("hyperliquid").expect("baseline dispatches hyperliquid");
    let new = after.get("hyperliquid").expect("the fold dispatches hyperliquid");

    for symbol in ["HYPE/USDC", "PURR/USDC", "@107", ""] {
        let a = old(symbol, "1m", 0, 1);
        let b = new(symbol, "1m", 0, 1);
        assert_eq!(a, b, "the two tables render {symbol:?} differently");
        let err = a.expect_err("this seam refuses every one of these spellings");
        assert!(
            err.starts_with("refused: hyperliquid:"),
            "a refusal must not be re-rendered as a venue fetch failure: {err}"
        );
    }
}

/// **The anti-vacuity companion** — the `a_key_only_in_the_store_is_resolved_from_it` half of the
/// mirror precedent: prove the NEW path is the one actually answering, so the equality above is a
/// claim about the fold rather than about two copies of the old code.
///
/// It asserts the real table's venue list IS `KLINE_SOURCES`' own, derived at run time from the
/// registry — which the baseline, being six hand-written closures, cannot be.
#[test]
fn the_real_table_is_the_registry_and_not_a_second_list() {
    let (_dir, store) = empty_store();
    let table = real_backfill_table(store);
    assert_eq!(
        table.supported(),
        vike_backfill::kline_source::venues(),
        "`real_backfill_table` is supposed to FOLD `KLINE_SOURCES`; if these differ it has grown a \
         list of its own again"
    );
    assert_eq!(
        table.supported().len(),
        vike_backfill::kline_source::KLINE_SOURCES.len(),
        "one row, one entry — a venue registered twice would silently lose its second closure"
    );
}

/// The registry the wire verb folds is the SAME one the supervisor binds against — the property
/// that made the collapse worth doing, asserted rather than assumed.
///
/// Before Phase 3 this could not be written at all: the supervisor's rows lived in `vike-backfill`
/// and the wire table's in `vike-datahub`, and only a text scan could compare them. Now both come
/// from one static, and every venue the table serves is a collector the supervisor can name in a
/// TOML roster.
#[test]
fn every_wire_venue_is_a_collector_the_supervisor_can_name() {
    let (_dir, store) = empty_store();
    let table = real_backfill_table(store);
    for venue in table.supported() {
        let source = vike_backfill::kline_source::source_by_venue(venue)
            .unwrap_or_else(|| panic!("the wire table serves {venue}, which the registry disowns"));
        assert!(
            vike_backfill::kline_source::source_by_collector_name(source.collector_name())
                .is_some(),
            "{venue}'s row is not reachable by the collector name a supervisor roster spells"
        );
    }
}
