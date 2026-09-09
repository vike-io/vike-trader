//! The collector registry: config name → the EXISTING `backfill_*` entry point, plus the venue it
//! writes under and the series kind it produces.
//!
//! THE WHOLE POINT is that the supervisor reimplements nothing. Every row here is a function this
//! crate already exposes and that a one-shot `*_backfill` bin already calls, so the supervisor
//! inherits paging, the `{venue}:{symbol}:{interval}:{start}-{end}` idempotency commit key, and the
//! still-forming-candle guard (`crate::klines::drop_forming_tail`) verbatim — a supervised backfill
//! and a hand-run one are the same code path.
//!
//! WIRED TODAY: the three keyless crypto kline collectors, chosen because they are the only
//! library-level entry points that already share ONE signature
//! (`fn(&DataFusionHist, symbol, interval, start_ms, end_ms) -> Result<usize, CollectError>`, all of
//! them thin bindings over `crate::klines::backfill_klines`) and need no credentials. This is the
//! same trio `vike_app_core::backfill_plan::SUPPORTED_BACKFILL_VENUES` names for the Data Manager's
//! bulk-backfill button — the supervisor is the unattended twin of that button.
//!
//! ADDING ONE is a config-and-registry matter, not a code change to the loop: append a
//! [`Collector`] row whose `backfill` matches [`BackfillFn`]. Collectors with a different arity
//! (e.g. `backfill_hyperliquid_klines`, which takes a separate `coin` argument, or the
//! `--symbols vike=source`-mapped EOD/IBKR collectors) need a small adapter first — deliberately
//! left out rather than papered over with a lossy default mapping.

use vike_data::DataFusionHist;

use crate::error::CollectError;

/// The one signature the registry dispatches: `(store, symbol, interval, start_ms, end_ms)` →
/// rows appended. Exactly the shape `backfill_binance_klines` / `backfill_bybit_klines` /
/// `backfill_okx_klines` already have.
pub type BackfillFn = fn(&DataFusionHist, &str, &str, i64, i64) -> Result<usize, CollectError>;

/// One registry row: the config-facing name, the venue partition the collector writes under (NOT
/// operator-overridable — the fn hard-codes it, so a mismatch would make the gap lookup and the
/// ingest target disagree), the series kind it produces, and the entry point itself.
#[derive(Debug, Clone, Copy)]
pub struct Collector {
    /// The `collector = "..."` value in the TOML roster.
    pub name: &'static str,
    /// `venue=` partition the collector appends under (its module's own `VENUE` const).
    pub venue: &'static str,
    /// `kind=` partition the collector appends under. Every wired collector produces `"bar"`.
    pub kind: &'static str,
    /// The existing backfill entry point.
    pub backfill: BackfillFn,
}

/// Every collector the supervisor can drive. A `static` (not a `const`) so
/// [`collector_by_name`] can hand out a genuinely `'static` borrow.
pub static COLLECTORS: &[Collector] = &[
    Collector {
        name: "binance_klines",
        venue: crate::binance::VENUE,
        kind: "bar",
        backfill: crate::binance::backfill_binance_klines,
    },
    Collector {
        name: "bybit_klines",
        venue: crate::bybit::VENUE,
        kind: "bar",
        backfill: crate::bybit::backfill_bybit_klines,
    },
    Collector {
        name: "okx_klines",
        venue: crate::okx::VENUE,
        kind: "bar",
        backfill: crate::okx::backfill_okx_klines,
    },
];

/// Look a collector up by its config name. `None` = unknown (the config validator turns that into a
/// startup error naming the known set, never a silent skip).
pub fn collector_by_name(name: &str) -> Option<&'static Collector> {
    COLLECTORS.iter().find(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_collector_name_is_unique() {
        let names: BTreeSet<&str> = COLLECTORS.iter().map(|c| c.name).collect();
        assert_eq!(names.len(), COLLECTORS.len(), "duplicate collector name in the registry");
    }

    #[test]
    fn lookup_finds_each_row_and_rejects_the_unknown() {
        for c in COLLECTORS {
            let found = collector_by_name(c.name).expect("registered collector is findable");
            assert_eq!(found.name, c.name);
            assert_eq!(found.venue, c.venue);
            assert_eq!(found.kind, c.kind);
        }
        assert!(collector_by_name("does_not_exist").is_none());
        assert!(collector_by_name("").is_none());
    }

    #[test]
    fn each_row_names_its_own_modules_venue_const() {
        // The venue string is NOT free text — it must be the very const the backfill fn writes
        // under, or `series_gaps` would probe a different partition than the ingest fills.
        assert_eq!(collector_by_name("binance_klines").unwrap().venue, crate::binance::VENUE);
        assert_eq!(collector_by_name("bybit_klines").unwrap().venue, crate::bybit::VENUE);
        assert_eq!(collector_by_name("okx_klines").unwrap().venue, crate::okx::VENUE);
    }

    #[test]
    fn every_wired_collector_produces_bars() {
        // The gap-heal path keys `SeriesId.kind` off this; a non-bar collector would need its own
        // interval handling (bars sub-partition by interval, ticks do not).
        for c in COLLECTORS {
            assert_eq!(c.kind, "bar", "{} declares a kind the heal path can't route", c.name);
        }
    }
}
