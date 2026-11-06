//! The DataSet member-symbol derivations: the stored series each member resolves to, and the cut.

#[cfg(doc)]
use super::gaps::stale_series_keys;
use vike_data::StoreKindId;
use vike_data_manager::SeriesKey;
use vike_ui_theme::components::segmented::Segment;
use vike_ui_theme::components::table::Column;
use vike_ui_theme::value::data;

// ===== The DataSet member-symbol derivations =====
//
// The same shape as `stale_series_keys`/`gapped_series_keys` directly above and for the same
// reason — the strip's numbers and the rows under it must be one derivation, not two — but asked
// of a DataSet's MEMBERSHIP rather than of the tree. A DataSet names symbols; the store holds
// series; these four functions are the whole of the join between them, and the `DataDest::DataSets`
// arm renders their output without repeating any of it.
//
// ⚠ They were block-local inside that arm when the member strip landed, which put them out of
// reach of the `#[cfg(test)]` module below. That is the only reason they are here: a derivation
// whose claims live solely in its doc comment is a claim nobody re-checks.

/// One member symbol and every STORED series it resolves to:
/// `(symbol, [(series key, last_row_ms)])`.
///
/// Named rather than spelled out at both sites that annotate one. Written literally the nesting
/// sits just under `clippy::type_complexity`'s default threshold — passing today, and close enough
/// that one more field in the tuple would redden a `-D warnings` lane for a reason no reader would
/// guess from the diff. It also reads better: the `i64` is a `last_ts`, and nothing in the bare
/// tuple says so.
pub(super) type Member = (String, Vec<(SeriesKey, i64)>);

/// `true` when a DataSet's `provider` field names an actual venue rather than the `Auto` sentinel
/// the form's dropdown opens on.
///
/// ⚠ `Auto` is a UI sentinel and NOT a venue, so it may never reach a backfill key: the datahub
/// would be asked for a venue nobody has and would answer with a refusal naming its own roster,
/// which reads as "the server cannot do this" when the truth is that this window sent it a
/// non-name. Withholding the key and saying so on the button's disabled hover is the honest half of
/// the same trade `crate::data::backfill_plan`'s module doc makes about venue gates in general: do not
/// invent a client-side roster, but do not send something that is not a venue either.
pub(super) fn provider_names_a_venue(provider: &str) -> bool {
    let p = provider.trim();
    !p.is_empty() && !p.eq_ignore_ascii_case("Auto")
}

/// Every STORED series one member symbol resolves to, as `(key, last_row_ms)`.
///
/// Scoped to the DataSet's provider when that names a venue, and to every venue when it is `Auto`
/// — which is what `Auto` means on the form the member list sits under.
///
/// ⚠ A GROUPED node is skipped, and `vike_data_manager::SymbolNode::grouped`'s own doc is the
/// argument: a grouped node's `symbol` field holds the GROUP NAME, not a symbol. Matching a member
/// against it is a false positive that would then be counted as stale on this screen and queued for
/// backfill under a name no venue trades. `crate::data::stored_load` records the same class of bug from
/// the load side, which is why `a_grouped_node_is_never_matched_as_a_member_symbol` exists rather
/// than the skip resting on this paragraph.
pub(super) fn member_series(
    tree: &[vike_data_manager::model::VenueNode],
    provider: &str,
    symbol: &str,
) -> Vec<(SeriesKey, i64)> {
    let scoped = provider_names_a_venue(provider);
    let mut out = Vec::new();
    for v in tree {
        if scoped && !v.venue.eq_ignore_ascii_case(provider.trim()) {
            continue;
        }
        for sym in &v.symbols {
            if sym.grouped || !sym.symbol.eq_ignore_ascii_case(symbol) {
                continue;
            }
            for r in &sym.series {
                out.push((
                    SeriesKey {
                        venue: v.venue.clone(),
                        symbol: sym.symbol.clone(),
                        kind: r.kind,
                        interval: r.interval.clone(),
                    },
                    r.cov.last_ts,
                ));
            }
        }
    }
    out
}

/// Is this member behind? — ANY of its stored series stale against the tree-wide window, which is
/// `vike_data_manager::is_stale`: the predicate [`stale_series_keys`] applies per SERIES, asked
/// here per MEMBER.
///
/// ⚠ A member the store holds nothing for is NOT stale, and that is a claim rather than an
/// oversight: `is_stale` judges a `last_ts`, and a symbol with no rows has none to judge. It is a
/// different condition — never fetched, not fallen behind — so the list marks it `· not stored` and
/// the legend counts it separately, rather than letting `stale only` quietly mean two things at
/// once. `an_unstored_member_is_not_stale_because_it_has_no_last_ts_to_judge` is that claim as a
/// test; the empty-slice fold returns `false` by construction, and the test exists so a future
/// "treat unstored as maximally stale" edit has to argue with something.
pub(super) fn member_is_stale(stored: &[(SeriesKey, i64)], gfirst: i64, glast: i64) -> bool {
    stored.iter().any(|(_, last)| vike_data_manager::is_stale(*last, gfirst, glast))
}

/// The keys a bulk backfill queues for ONE member symbol — store first, the set's own declaration
/// second, nothing third.
///
/// * **Stored series win.** They are real keys with real gap ranges, so
///   `crate::data::backfill_plan::plan_backfill_jobs` targets the holes rather than re-fetching a default
///   lookback window, and a non-`bar` one among them is counted as a skip by that planner — exactly
///   what the Has-gaps and Stale strips already rely on.
/// * **A member with nothing stored falls back to the SET'S OWN declaration** —
///   `(provider, interval)` at `kind = "bar"`, the only kind an interval can mean. This is the case
///   the two sibling strips never meet: they queue rows that are in the store by construction,
///   while a DataSet is a WISHLIST whose most useful day is the one where none of its symbols has
///   been fetched yet. The planner gives such a key a single default-lookback window, which is
///   precisely "go and get this".
/// * **Otherwise nothing**, and the caller counts the member as a skip: `Auto` names no venue (see
///   [`provider_names_a_venue`]) and an empty interval names no bar.
pub(super) fn member_keys(
    stored: &[(SeriesKey, i64)],
    provider: &str,
    interval: &str,
    symbol: &str,
) -> Vec<SeriesKey> {
    if !stored.is_empty() {
        return stored.iter().map(|(k, _)| k.clone()).collect();
    }
    let iv = interval.trim();
    if provider_names_a_venue(provider) && !iv.is_empty() {
        return vec![SeriesKey {
            venue: provider.trim().to_string(),
            symbol: symbol.to_string(),
            kind: StoreKindId::Bar,
            interval: Some(iv.to_string()),
        }];
    }
    Vec::new()
}

/// The DataSet member list's columns. Every cell is text, so it is the kit's data table.
///
/// ⚠ The symbol column is Inter, not mono: the kit's only monospace column is right-aligned, and a
/// left-aligned mono column is on the PR's report of missing knobs (owner decision 3).
pub(super) const MEMBER_COLUMNS: [Column<'static>; 2] = [
    Column {
        title: "Member",
        weight: data::DS_MEMBERS_MEMBER_WEIGHT,
        min_w: data::DS_MEMBERS_MEMBER_MIN_W,
        numeric: false,
    },
    Column {
        title: "In this store",
        weight: data::DS_MEMBERS_STORED_WEIGHT,
        min_w: data::DS_MEMBERS_STORED_MIN_W,
        numeric: false,
    },
];

/// One member-list cell: the symbol, or why it is marked. `stale` is the design's own marker. `not
/// stored` is this window's addition and earns its column: it is WHY such a member can never appear
/// under `stale only` — it holds nothing to be behind — and without it that absence looks like a
/// filter bug. Stale wins over empty, as the old list's suffixes did.
pub(super) fn member_cell(m: &Member, col: usize, gfirst: i64, glast: i64) -> String {
    let (symbol, stored) = m;
    match col {
        0 => symbol.clone(),
        _ if member_is_stale(stored, gfirst, glast) => "stale".to_string(),
        _ if stored.is_empty() => "not stored".to_string(),
        _ => String::new(),
    }
}

/// Which cut of the SELECTED DataSet's member symbols is on screen.
///
/// ⚠ **This is not the DataSet list down the left-hand side.** `All`/`Binance`/`Dukascopy`/
/// `My DataSets` group the DataSet LIST — which sets you can see. This filters the SYMBOLS INSIDE
/// the one set that is open. Two controls, two rosters, and the design puts them on opposite halves
/// of the split for that reason.
///
/// It sits at module scope, rather than inside the DataSets arm where it was born, so
/// `every_segment_roster_offers_each_value_once_and_leads_with_the_default` can hold its roster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum MemberCut {
    /// Every symbol the set names.
    #[default]
    AllMembers,
    /// Only members carrying at least one STORED series that `vike_data_manager::is_stale` judges
    /// behind the tree-wide window.
    StaleOnly,
}

/// The DataSet member strip's segments, in render order.
pub(super) const MEMBER_SEGMENTS: [Segment<'static, MemberCut>; 2] = [
    Segment {
        value: MemberCut::AllMembers,
        label: "all members",
        why: "Every symbol this DataSet names, whether or not the local store holds anything for it",
    },
    Segment {
        value: MemberCut::StaleOnly,
        label: "stale only",
        why: "Members whose stored series lag the tree-wide window by more than 35% — the same \
              `is_stale` cut the Stale destination applies, asked per member symbol",
    },
];
