//! The CROSS-KIND partial days, one level above the per-series gap map: per instrument, the days
//! where some of its kinds have data and others do not ([`DataManagerInventoryPartialDayMap`]), the
//! one-line label a cell or tooltip shows ([`partial_days_label`]), and the bridge from a tree node
//! to the coverage report's key ([`instrument_key_of`]). Pure: no egui.

use std::collections::{BTreeMap, BTreeSet};

use vike_model::StoreKindId;

use super::DataManagerInventorySymbol;

/// Per-INSTRUMENT cross-kind partial days, keyed by `vike_data::InstrumentCoverageKey` — one level ABOVE
/// [`DataManagerInventoryGapMap`]'s per-series identity.
///
/// ⚠ Keyed on the WHOLE `InstrumentCoverageKey`, not on `(venue, label)`: that key also carries `grouped`,
/// and a grouped series and a per-symbol one can share a name. Dropping the flag merged exactly what
/// `coverage` deliberately keeps apart — they are different directories with different manifests, and
/// a family mid-migration genuinely has its history in both.
///
/// **Why a second map rather than more entries in [`DataManagerInventoryGapMap`].** They answer different questions and
/// neither derives from the other. `DataManagerInventoryGapMap` says *"this series is missing days 3–4"* — a hole in ONE
/// kind's own timeline. This says *"on day 11 the instrument has trades but no book"* — which is
/// invisible per-series, because each series is perfectly contiguous on its own.
///
/// That distinction is the difference between a complete backfill and a useless one. A Polymarket
/// venue-fill restores the trade tape and nothing else (no venue-direct source serves `book` — see
/// `vike_backfill::capabilities`), so the trade series closes its gap, the book series never had one, and
/// only the JOIN shows that a market-making backtest over that window would run on no book at all.
///
/// An absent key means "nothing partial" — a caller that has not fetched coverage passes an empty
/// map and gets today's behavior unchanged.
///
/// [`DataManagerInventoryGapMap`]: crate::DataManagerInventoryGapMap
pub type DataManagerInventoryPartialDayMap =
    BTreeMap<vike_data::InstrumentCoverageKey, Vec<vike_data::InstrumentCoveragePartialDay>>;

/// Build a [`DataManagerInventoryPartialDayMap`] from a store's cross-kind coverage report.
///
/// Takes the already-computed report rather than a store handle, so this crate stays DataFusion-free
/// and the function is unit-testable without opening anything.
pub fn partial_days_from_coverage(
    report: &[vike_data::InstrumentCoverage],
) -> DataManagerInventoryPartialDayMap {
    report
        .iter()
        .filter_map(|c| {
            let partial = c.partial_days();
            (!partial.is_empty()).then(|| (c.key.clone(), partial))
        })
        .collect()
}

/// Does this instrument have any day where some kinds have data and others do not?
#[cfg(test)]
fn has_partial_days(
    map: &DataManagerInventoryPartialDayMap,
    key: &vike_data::InstrumentCoverageKey,
) -> bool {
    map.get(key).is_some_and(|v| !v.is_empty())
}

/// A one-line summary of what an instrument is missing — e.g. `"3 partial days (book, quote)"` — for
/// a grid cell or tooltip. Empty string when nothing is partial, so a caller renders it
/// unconditionally without a branch.
pub fn partial_days_label(
    map: &DataManagerInventoryPartialDayMap,
    key: &vike_data::InstrumentCoverageKey,
) -> String {
    let Some(days) = map.get(key).filter(|v| !v.is_empty()) else {
        return String::new();
    };
    let mut kinds: BTreeSet<StoreKindId> = BTreeSet::new();
    for d in days {
        kinds.extend(d.missing_kinds.iter().copied());
    }
    format!(
        "{} partial day{} ({})",
        days.len(),
        if days.len() == 1 { "" } else { "s" },
        kinds.into_iter().map(StoreKindId::word).collect::<Vec<_>>().join(", ")
    )
}

#[path = "partial_day_tests.rs"]
#[cfg(test)]
mod partial_day_tests;

/// The [`vike_data::InstrumentCoverageKey`] a tree node denotes — the bridge from the Data Manager's
/// venue→symbol tree to the cross-kind coverage report, which is keyed by instrument rather than by
/// series.
///
/// This needs [`DataManagerInventorySymbol::grouped`], which is why the tree carries it: a grouped series and a
/// per-symbol one can share a label, and `coverage` treats them as different instruments.
pub fn instrument_key_of(
    venue: &str,
    symbol: &DataManagerInventorySymbol,
) -> vike_data::InstrumentCoverageKey {
    vike_data::InstrumentCoverageKey {
        venue: venue.to_string(),
        label: symbol.symbol.clone(),
        grouped: symbol.grouped,
    }
}

/// The cross-kind warning for a tree node, ready to render — `""` when nothing is partial.
///
/// The Data Manager's existing gap column answers "is this SERIES missing days". This answers "does
/// this INSTRUMENT have days where some kinds are present and others are not" — the case a
/// per-series view structurally cannot show, because each series is contiguous on its own.
pub fn symbol_partial_label(
    map: &DataManagerInventoryPartialDayMap,
    venue: &str,
    symbol: &DataManagerInventorySymbol,
) -> String {
    partial_days_label(map, &instrument_key_of(venue, symbol))
}

#[path = "instrument_key_tests.rs"]
#[cfg(test)]
mod instrument_key_tests;
