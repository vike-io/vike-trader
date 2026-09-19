//! **The ONE kline registry** — the seam
//! `docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md`'s Phase 3 asked
//! for, in the shape [`crate::funding_rate::FundingRateSource`] and [`crate::eod::EodSource`]
//! already have: a venue is one [`KlineSource`] impl, and every dispatch path folds the SAME
//! [`KLINE_SOURCES`] static rather than re-listing the venues.
//!
//! # What this replaced, and why it was worth replacing
//!
//! 0059 VERIFIED four hand-copied kline rosters and measured what the copying cost: aster, deribit
//! and hyperliquid each shipped a complete, tested, capability-declared collector that NO dispatch
//! table named, for months. Phase 2 dispatched them and PAID THE COPY COST AGAIN while doing it: it
//! wrote three rows in the supervisor's registry (`supervisor/registry.rs`'s `COLLECTORS`) and
//! three more entries in the datahub's table (`crates/vike-datahub/src/backfill.rs`'s
//! `real_backfill_table`), because those two named the same six collectors independently, in crates
//! that cannot see each other. The other two of 0059's four rosters it deliberately left alone,
//! annotating each with the reason it could not simply take a row — and those annotations are the
//! measurement this phase acted on.
//!
//! **TWO of those four collapsed into this file; the other two did not, and their reasons are
//! different from each other.** Both non-collapses are recorded at their own sites and in
//! `crates/vike-ops/tests/collector_dispatch_gate.rs`'s `ROSTERS_NOT_GATED`:
//!
//! * `crates/vike-app-core/src/backfill_plan.rs`'s `SUPPORTED_BACKFILL_VENUES` was DELETED rather
//!   than folded in. It was a dead gate whose only consumer was the Local backfill route, whose
//!   executor is a tombstone — and `vike-app-core` (layer 80) takes no `vike-backfill` edge at all,
//!   deliberately, so folding it in would have re-created the dependency two rulings removed.
//! * `crates/vike-backtest/src/fetch.rs`'s `FETCHABLE_VENUES` is a DIFFERENT PRODUCER behind a wall
//!   the layer gate enforces: `vike-backtest` is layer 50 and `vike-backfill` is 55, and the edge
//!   already runs 55 → 50 (`vike-backfill`'s optional `vike-backtest` dep under
//!   `poly-ch-backtest`), so a normal edge the other way is both a layer inversion AND a cycle. It
//!   could not name this trait at any price.
//!
//! # The shape, and the one way it differs from the two registries it copies
//!
//! [`FundingRateSource`](crate::funding_rate::FundingRateSource) and
//! [`EodSource`](crate::eod::EodSource) both make `fetch` **store-free** — the trait returns rows,
//! and the store write (the commit key, the dedup, the `append_*`) lives in ONE non-trait function
//! that takes the source as `&dyn`. That split is the reason "adding a venue is one impl" is true
//! of them, and it is what the kline side lacked: the old dispatch unit FUSED fetch and ingest, so
//! the supervisor's row type named the CONCRETE store outright
//! (`fn(&DataFusionHist, &str, &str, i64, i64)`), the datahub had to capture an
//! `Arc<DataFusionHist>` in six closures to hide that from its own type, and the
//! still-forming-candle guard was a property of the PATH each venue happened to take rather than of
//! the seam. ⚠ Be precise about the testability half, because the two sides differed: the datahub's
//! `BackfillTable::new` has always taken closures and its roundtrip test drives fakes through them,
//! while the supervisor's rows were bare `fn` pointers with no capture — **no test in this tree had
//! ever executed a supervisor dispatch**, and `&dyn KlineSource` is the injection point that makes
//! one possible.
//!
//! So [`KlineSource::fetch`] returns `Vec<Bar>` and touches no store, and [`backfill_kline_source`]
//! is the one ingest — `crate::klines::ingest_klines`, the same body `crate::klines::backfill_klines`
//! has always run, so `drop_forming_tail`, the `{venue}:{symbol}:{interval}:{start}-{end}` commit
//! key and `append_bars` are inherited by every source and cannot be forgotten by one — and since
//! 0059 Phase 1 the forming-bar REFUSAL is inherited the same way, which is what finally closed it
//! on the one-shot bins.
//!
//! ⚠ **The one thing this registry deliberately does NOT copy from those two is their GATE**, and
//! the omission is the point. Both carry a hand-typed `pub const SOURCES: &[&str]` beside a
//! `source_by_name` match, held together only by a test that compares `SOURCES` to a literal copy
//! of itself — so a third impl plus a match arm, with no `SOURCES` edit, compiles, passes every
//! test and vanishes from the bin's `--help`. "Adding a venue is one impl" is three edits in both,
//! and the third is ungated. Here the names are DERIVED from the static
//! ([`collector_names`]/[`venues`] fold it; nothing is hand-typed beside it), and
//! `crates/vike-ops/tests/collector_dispatch_gate.rs` walks from the collector MODULES to this
//! static in both directions.
//!
//! # Adding a venue
//!
//! 1. Write `pub fn backfill_<venue>_klines` in `crates/vike-backfill/src/<venue>.rs` as usual —
//!    it stays the one-shot bin's entry point and is what the gate derives the population from.
//! 2. Add a unit-struct impl of [`KlineSource`] beside it. `fetch` is that venue's bridge call and
//!    nothing else; a request the one-symbol seam cannot express is a `CollectError::Refused`
//!    there (`crate::hyperliquid::HyperliquidKlines` is the worked example), never a guess.
//! 3. Point the free function at it through [`backfill_kline_source`], so the bin and the two
//!    dispatch paths are the same call.
//! 4. Add one row to [`KLINE_SOURCES`].
//!
//! Step 4 is the only edit anything outside the venue module needs, and skipping it reddens the
//! gate rather than shipping an unreachable collector.

use vike_data::DataFusionHist;
use vike_model::Bar;

use crate::error::CollectError;

/// The series `kind` every kline source writes under.
///
/// A CONSTANT rather than a per-row field, and that is a narrowing of what the supervisor registry
/// used to express. The old `Collector.kind` was a per-row `&'static str` that read `"bar"` in all
/// six rows, and its only consumer is `crate::supervisor::config::validate`, which compares a
/// roster's `kind =` against it. The kind is not a property a VENUE gets to choose here: it is a
/// property of [`backfill_kline_source`], which calls `append_bars` and nothing else. A collector
/// family that writes something else gets its own trait and its own registry — which is exactly
/// what `crate::funding_rate` and `crate::eod` already are — rather than a row in this one
/// pretending a kline source could produce quotes.
pub const KLINE_KIND: &str = "bar";

/// One venue's kline history source: the identity a roster names it by, and the FETCH.
///
/// **Store-free by construction** — see the module doc. An impl builds the venue request, calls its
/// own bridge's pager and returns bars; it never sees a `DataFusionHist`, never spells a commit key
/// and never decides whether the last candle is still forming. That is [`backfill_kline_source`]'s
/// job, so every source inherits it identically.
///
/// `Send + Sync` because [`KLINE_SOURCES`] is a `static` of `&'static dyn KlineSource` (a genuinely
/// `'static` borrow the supervisor binds once and holds across its whole loop — the property the
/// old `static COLLECTORS` existed for) and because the datahub's table moves the borrow into a
/// `Box<dyn Fn … + Send + Sync>` closure.
pub trait KlineSource: Send + Sync {
    /// The config-facing collector name — the supervisor roster's `collector = "..."` value
    /// (`"binance_klines"`). DISTINCT from [`Self::venue`] on purpose: the supervisor keys a source
    /// on this, which is what lets one venue eventually carry two collectors, while the wire verb
    /// keys on the venue.
    fn collector_name(&self) -> &str;

    /// The store `venue` partition this source's rows land under — its module's own `VENUE` const,
    /// never free text. It is both the datahub table's key and the first field of the commit key,
    /// so a row whose `venue()` disagreed with what its `fetch` actually pages would make the gap
    /// lookup and the ingest target name different partitions.
    fn venue(&self) -> &str;

    /// Fetch `[start_ms, end_ms]` of `(symbol, interval)` klines. DOES the network I/O and the
    /// venue's own paging; touches NO store.
    ///
    /// `symbol` is the unified vike symbol — the store key. A venue whose own vocabulary differs
    /// maps it here, and REFUSES ([`CollectError::Refused`]) rather than guessing when a one-symbol
    /// seam cannot express the mapping: `crate::hyperliquid::identity_coin_for` is the worked
    /// example and its doc carries the argument.
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, CollectError>;
}

/// **Every kline source this crate ships.** The ONE roster: the supervisor binds its sources
/// against it (`crate::supervisor::config::validate` /
/// `crate::supervisor::run::resolve_collectors`), and `crates/vike-datahub/src/backfill.rs`'s
/// `real_backfill_table` FOLDS it into the wire verb's dispatch table rather than re-listing six
/// closures.
///
/// A `static` (not a `const`) so the lookups below hand out a genuinely `'static` borrow — the
/// supervisor resolves every declared source to one at startup and holds the `Vec` across its whole
/// loop, so an unknown collector is a STARTUP error rather than a per-pass surprise.
///
/// ⚠ **ORDER IS OBSERVABLE.** It is what the datahub's unknown-venue refusal prints and what the
/// `Welcome` frame advertises (`BackfillTable::supported` preserves it), and
/// `crates/vike-datahub/tests/backfill_roundtrip.rs`'s `the_real_table_names_every_kline_venue`
/// pins the rendered order. This is the collectors' own order — the three that shipped first, then
/// the three 0059 Phase 2 dispatched.
///
/// ⚠ **Each row spells `&crate::<venue>::<Type>`, and the gate DERIVES the registered set from that
/// spelling.** `crates/vike-ops/tests/collector_dispatch_gate.rs` is a text scan (it lives in
/// `vike-ops`, which may not take a normal edge to this crate — both are layer 55 — and a dev-edge
/// would drag seven bridge crates into a test build every PR runs). A row written some other way is
/// a row that scan cannot see, and the failure is safe: the venue then reads as
/// WRITTEN-BUT-UNREGISTERED and the gate goes red.
pub static KLINE_SOURCES: &[&'static dyn KlineSource] = &[
    &crate::binance::BinanceKlines,
    &crate::bybit::BybitKlines,
    &crate::okx::OkxKlines,
    &crate::aster::AsterKlines,
    &crate::deribit::DeribitKlines,
    &crate::hyperliquid::HyperliquidKlines,
];

/// Look a source up by its config-facing collector name (`"binance_klines"`). `None` = unknown —
/// the supervisor's validator turns that into a startup error naming [`collector_names`], never a
/// silent skip.
pub fn source_by_collector_name(name: &str) -> Option<&'static dyn KlineSource> {
    KLINE_SOURCES.iter().copied().find(|s| s.collector_name() == name)
}

/// Look a source up by its store `venue` partition — the wire verb's key.
pub fn source_by_venue(venue: &str) -> Option<&'static dyn KlineSource> {
    KLINE_SOURCES.iter().copied().find(|s| s.venue() == venue)
}

/// The collector names [`source_by_collector_name`] accepts, in [`KLINE_SOURCES`] order — for the
/// supervisor validator's "have: …" list. DERIVED from the static; there is deliberately no
/// hand-typed `SOURCES` const beside it (see the module doc's note on the two registries this one
/// copies).
pub fn collector_names() -> Vec<&'static str> {
    KLINE_SOURCES.iter().map(|s| s.collector_name()).collect()
}

/// The venues [`source_by_venue`] accepts, in [`KLINE_SOURCES`] order — the datahub table's
/// declared order and its unknown-venue refusal text. Derived, like [`collector_names`].
pub fn venues() -> Vec<&'static str> {
    KLINE_SOURCES.iter().map(|s| s.venue()).collect()
}

/// **The ingest half** — fetch through `source`, then guard and store. The one place a kline lands
/// in the store, whichever path asked for it: the one-shot bin, the supervisor pass, or the
/// datahub's wire verb.
///
/// It is `crate::klines::ingest_klines`, which is `crate::klines::backfill_klines`'s body unchanged
/// — the still-forming-candle guard (`drop_forming_tail`, against FETCH-time "now", not the
/// requested `end_ms`), the `{venue}:{symbol}:{interval}:{start}-{end}` commit key, and
/// `append_bars` under `source.venue()`. Returns rows written; 0 means the window's commit key was
/// already spent.
///
/// ⚠ An interval `vike_model::time::measures_bar_step` answers `false` for (`1w`, `1M`, `1mo`) is
/// REFUSED by `crate::klines::ingest_klines` before `source.fetch` is called — so no `KlineSource`
/// impl ever sees one, and none needs its own check. This used to read "the guard DECLINES … and
/// the consequence belongs to the CALLER"; the caller it belonged to was every one-shot bin, which
/// decided nothing, and `docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md`
/// Phase 1 moved the decision here. The two automated dispatch paths still refuse EARLIER, on
/// purpose — see `crate::klines::ingest_klines`'s own doc.
pub fn backfill_kline_source(
    hist: &DataFusionHist,
    source: &dyn KlineSource,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<usize, CollectError> {
    crate::klines::ingest_klines(
        hist,
        source.venue(),
        symbol,
        interval,
        start_ms,
        end_ms,
        |sym, iv, s, e| source.fetch(sym, iv, s, e),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_collector_name_is_unique() {
        let names: BTreeSet<&str> = collector_names().into_iter().collect();
        assert_eq!(names.len(), KLINE_SOURCES.len(), "duplicate collector name in the registry");
    }

    /// One venue, one row. The datahub's table is keyed on the venue and built by folding this
    /// static, so two rows sharing a venue would silently give the second one no entry at all.
    #[test]
    fn every_venue_appears_once() {
        let vs: BTreeSet<&str> = venues().into_iter().collect();
        assert_eq!(vs.len(), KLINE_SOURCES.len(), "two rows claim the same venue");
    }

    #[test]
    fn lookup_finds_each_row_by_both_keys_and_rejects_the_unknown() {
        for s in KLINE_SOURCES {
            let by_name = source_by_collector_name(s.collector_name())
                .expect("a registered collector is findable by name");
            assert_eq!(by_name.venue(), s.venue());
            let by_venue =
                source_by_venue(s.venue()).expect("a registered collector is findable by venue");
            assert_eq!(by_venue.collector_name(), s.collector_name());
        }
        assert!(source_by_collector_name("does_not_exist").is_none());
        assert!(source_by_collector_name("").is_none());
        assert!(source_by_venue("does_not_exist").is_none());
        assert!(source_by_venue("").is_none());
    }

    /// The venue string is NOT free text — it must be the very const the module's own collector
    /// writes under, or `series_gaps` would probe a different partition than the ingest fills.
    /// Named one venue at a time rather than folded, for the reason its predecessor was: a fold
    /// would pass over an empty table, and the pairing is the claim.
    #[test]
    fn each_row_names_its_own_modules_venue_const() {
        let venue_of = |name: &str| source_by_collector_name(name).unwrap().venue();
        assert_eq!(venue_of("binance_klines"), crate::binance::VENUE);
        assert_eq!(venue_of("bybit_klines"), crate::bybit::VENUE);
        assert_eq!(venue_of("okx_klines"), crate::okx::VENUE);
        assert_eq!(venue_of("aster_klines"), crate::aster::VENUE);
        assert_eq!(venue_of("deribit_klines"), crate::deribit::VENUE);
        assert_eq!(venue_of("hyperliquid_klines"), crate::hyperliquid::VENUE);
    }

    /// Every kline collector this crate WRITES is registered here.
    ///
    /// ⚠ This is the LOCAL half only, and it is a PIN rather than the gate: it cannot see a
    /// collector module that no row names, which is exactly the hole three collectors sat in.
    /// `crates/vike-ops/tests/collector_dispatch_gate.rs` derives the written set from the real
    /// `src/` tree and compares it to this static in BOTH directions.
    #[test]
    fn every_written_kline_collector_has_a_row() {
        for name in [
            "binance_klines",
            "bybit_klines",
            "okx_klines",
            "aster_klines",
            "deribit_klines",
            "hyperliquid_klines",
        ] {
            assert!(source_by_collector_name(name).is_some(), "no registry row dispatches {name}");
        }
    }

    /// The declared order — what the datahub's refusal text prints and what its `Welcome`
    /// advertises. Pinned here as well as in the datahub's own lane because this static is now the
    /// only place it is decided.
    #[test]
    fn the_declared_order_is_the_collectors_own() {
        assert_eq!(venues(), vec!["binance", "bybit", "okx", "aster", "deribit", "hyperliquid"]);
        assert_eq!(
            collector_names(),
            vec![
                "binance_klines",
                "bybit_klines",
                "okx_klines",
                "aster_klines",
                "deribit_klines",
                "hyperliquid_klines",
            ]
        );
    }

    /// A collector name is `<venue>_klines` for every row — the convention the supervisor's example
    /// roster and every operator runbook spell, and the one a new venue's author copies.
    #[test]
    fn a_collector_name_is_its_venue_plus_klines() {
        for s in KLINE_SOURCES {
            assert_eq!(
                s.collector_name(),
                format!("{}_klines", s.venue()),
                "the row for {:?} breaks the naming convention",
                s.venue()
            );
        }
    }
}
