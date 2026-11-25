//! The Has gaps and Stale destinations, the pure folds their strips render, and the partial-day list.

use super::rail;
use super::{action, dead_action, legend, warning_legend};
use crate::ui::windows::WindowToolContext;
use crate::ui::windows::WindowView;
use vike_data_manager::DataManagerInventorySeriesKey;
use vike_model::StoreKindId;

use vike_ui_theme::components::segmented::{self, ThemeSegment};
use vike_ui_theme::components::state::{self, ThemeEmptyPane};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, section};
use vike_ui_theme::icons;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::data;

/// The Has gaps destination: two different questions about missing data (a per-series hole, and a
/// cross-kind partial day) on one screen, behind a segmented control.
pub(super) fn data_has_gaps(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    tv: &mut WindowView,
    t: &ThemeTokens,
) {
    use egui::RichText;
    use egui::vec2;
    // ===== Has gaps — two different questions about missing data, on one screen =====
    //
    // The two cuts are NOT two filters over one set. A per-series gap is a hole inside one
    // series' own timeline; a cross-kind partial day is a day on which an INSTRUMENT holds
    // some of its kinds and not others, which is invisible per series because each series
    // is perfectly contiguous on its own. `vike_data_manager::DataManagerInventoryPartialDayMap`'s doc argues
    // that distinction at length and it is the whole reason this destination has a
    // segmented control rather than a checkbox: the second cut has no rows in the grid to
    // filter, so it is rendered here, by this module, over `DataManagerInventoryContext::partials`.
    let mode_id = ui.scope_id().with("data_manager_gap_mode");
    let mut mode: DataManagerGapCut =
        ui.data_mut(|d| d.get_temp::<DataManagerGapCut>(mode_id)).unwrap_or_default();
    let gapped = gapped_series_keys(context.stored.tree, context.stored.gaps);
    let ranges: usize =
        gapped.iter().filter_map(|k| context.stored.gaps.get(k)).map(Vec::len).sum();
    let (gfirst, glast) = vike_data_manager::global_span(context.stored.tree);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);
        segmented::segmented(ui, &mut mode, &GAP_SEGMENTS);
        ui.label(
            RichText::new("Window")
                .font(t.font(ThemeTextRole::Caption))
                .color(t.theme.text_tertiary),
        );
        ui.label(
            RichText::new(span_label(gfirst, glast))
                .font(t.mono(ThemeTextRole::Caption))
                .color(t.theme.text_secondary),
        );
        if matches!(mode, DataManagerGapCut::PerSeries) {
            // Live, and it is the SAME out-slot the grid's own bulk bar fills: the keys go
            // to `WindowView::stored_backfill`, which the shell drains into its
            // background backfill spawn. The only thing this button adds is not having to
            // tick 7 checkboxes to say "all of them".
            let fillable = backfillable(&gapped);
            let label = format!("Backfill all {}", gapped.len());
            let hover = format!(
                "Queue all {} gapped series. {fillable} will plan a job (kind=bar with an \
                         interval); the other {} are counted as skips, never silently dropped. \
                         Progress lands in the status line beside Refresh.",
                gapped.len(),
                gapped.len().saturating_sub(fillable),
            );
            let backfill = action(Some(icons::BACKFILL), &label);
            let backfill = if context.stored.gaps_counting() {
                backfill.disabled_because(GAPS_COUNTING)
            } else if gapped.is_empty() {
                backfill
                    .disabled_because("No series carries a known hole — there is nothing to fill")
            } else {
                backfill
            };
            if ui.add(backfill).on_hover_text(hover).clicked() {
                tv.stored_backfill.extend(gapped.iter().cloned());
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(mode_id, mode));
    // ⚠ A cut whose pass is still out shows a counting state, not the rows found so far: they would
    // grow under the pointer, and a short list would read as the whole of it.
    match mode {
        DataManagerGapCut::PerSeries if context.stored.gaps_counting() => {
            state::view(ui, ThemeEmptyPane::Loading(GAPS_COUNTING));
        }
        DataManagerGapCut::CrossKind if context.stored.partials_counting() => {
            state::view(ui, ThemeEmptyPane::Loading(vike_data_manager::PARTIALS_CHECKING));
        }
        DataManagerGapCut::PerSeries => {
            legend(
                ui,
                &format!(
                    "{} series carry a hole, {ranges} range(s) in total{}. A series that \
                             merely starts late is NOT a gap — every range here is missing from \
                             inside an otherwise covered span.",
                    gapped.len(),
                    not_checked_tail(vike_data_manager::gaps_unknown(context.stored.gaps)),
                ),
            );
            section::strip_rule(ui);
            tv.stored_grid.active_view = vike_data_manager::DataManagerInventoryFilter::HasGaps;
            super::stored_tool_content(ui, context, tv);
        }
        DataManagerGapCut::CrossKind => partial_days_list(ui, context),
    }
}

/// What the Has gaps cut and its Backfill say while the gap probes are still out.
const GAPS_COUNTING: &str =
    "Counting the series with gaps\u{2026} the list lands once every probe has answered.";

/// What a sentence about gaps adds when `n` series' gap probes failed: `Has gaps` lists only known holes,
/// so without this a failed check reads as a whole series.
pub(super) fn not_checked_tail(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => "; the gap check of 1 more series failed, so its gaps are not known".to_string(),
        n => format!("; the gap check of {n} more series failed, so their gaps are not known"),
    }
}

/// The Stale destination: the one screen whose threshold is a moving target, so its strip states
/// the tree window and the cut-off it judged against.
pub(super) fn data_stale(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    tv: &mut WindowView,
    t: &ThemeTokens,
) {
    use egui::RichText;
    use egui::vec2;
    // ===== Stale — the one screen whose threshold is a MOVING TARGET =====
    //
    // `vike_data_manager::is_stale` is `behind / span > 0.35` against the tree-wide window,
    // so the cut-off is a property of the TREE and not of the calendar: it moves every time
    // the newest row anywhere in the store moves. A screen that showed only the rows and
    // not the date it judged them against would be read as a fixed "older than X" list,
    // and tomorrow's answer would quietly be a different one. So the strip states the
    // window and the resulting cut-off, computed the same way the filter below computes
    // its rows.
    let (gfirst, glast) = vike_data_manager::global_span(context.stored.tree);
    let stale = stale_series_keys(context.stored.tree, gfirst, glast);
    // ⚠ The pressed segment is DERIVED from the grid's live sort, never stored beside it:
    // the grid's own column headers also write `DataManagerInventoryGridState::sort`, and a value neither
    // preset names renders both segments unpressed — which is the truth. Storing the pick
    // separately would leave a segment lit while the grid sorted by something else.
    let mut picked =
        STALE_SORTS.iter().map(|s| s.value).find(|v| *v == Some(tv.stored_grid.sort)).flatten();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);
        if segmented::segmented(ui, &mut picked, &STALE_SORTS)
            && let Some(sort) = picked
        {
            // Written ONLY on a click: writing every frame would undo a header sort.
            tv.stored_grid.sort = sort;
        }
        ui.label(
            RichText::new("tree window")
                .font(t.font(ThemeTextRole::Caption))
                .color(t.theme.text_tertiary),
        );
        ui.label(
            RichText::new(span_label(gfirst, glast))
                .font(t.mono(ThemeTextRole::Caption))
                .color(t.theme.text_secondary),
        );
        let fillable = backfillable(&stale);
        let label = format!("Update all {}", stale.len());
        let hover = format!(
            "Queue all {} stale series. {fillable} will plan a job (kind=bar with an \
                     interval); the other {} are counted as skips, never silently dropped. \
                     Progress lands in the status line beside Refresh.",
            stale.len(),
            stale.len().saturating_sub(fillable),
        );
        let update = action(Some(icons::BACKFILL), &label);
        let update = if stale.is_empty() {
            update.disabled_because(
                "Nothing is stale against this tree window — there is nothing to update",
            )
        } else {
            update
        };
        if ui.add(update).on_hover_text(hover).clicked() {
            tv.stored_backfill.extend(stale.iter().cloned());
        }
    });
    // Staleness is judged against the tree alone, so its count is final the moment the inventory
    // lands, and is still being counted until then.
    legend(
        ui,
        // The cut-off is the Stale rule itself (`vike_data_manager::stale_cutoff_ms`, which
        // `is_stale` reads), so the date printed here and the rows below are one judgement; it is
        // stated because it reads as a fixed date and moves every time the tree's newest row does.
        &match vike_data_manager::stale_cutoff_ms(gfirst, glast) {
            _ if context.stored.listing() => {
                "Counting the stale series\u{2026} the store's inventory has not answered yet."
                    .to_string()
            }
            Some(cut) => format!(
                "{} of {} series are stale: last row before {} (35% of this {}-day window, \
                         and the date moves as the tree does).",
                stale.len(),
                rail::DataManagerRailCounts::from_context(context).series,
                vike_model::time::epoch_ms_to_utc_date(cut),
                (glast - gfirst) / vike_model::MS_PER_DAY,
            ),
            None => "The tree spans no time yet, so nothing can be judged stale.".to_string(),
        },
    );
    warning_legend(
        ui,
        "The sort applies WITHIN each venue block — the shared grid groups its rows by \
                 venue before sorting them, so `oldest first` is oldest-first per venue and not \
                 one global order.",
    );
    section::strip_rule(ui);
    tv.stored_grid.active_view = vike_data_manager::DataManagerInventoryFilter::Stale;
    super::stored_tool_content(ui, context, tv);
}

// ===== The pure folds the strips render =====
//
// Each is O(series) over the already-loaded tree and clones nothing but the keys it returns. That
// is deliberate rather than incidental: `vike_data_manager::filter_tree` — which
// `vike_data_manager::inventory_grid` itself calls once per frame, and which any
// count-by-filtering would call a second time — DEEP-CLONES the whole tree. A strip that wanted a
// count that way would be paying a full clone of everything to render one number directly above a
// grid that had just paid for the same clone. `super::rail::DataManagerRailCounts`' own doc makes the
// same argument from the rail's side, which is why there is no stale badge there at all.
//
// The predicates below are the same ones `filter_tree` applies, so the number on the strip and the
// rows in the grid cannot disagree.

/// Every series the `Stale` view will show, as the grid's own key type.
///
/// The predicate is `vike_data_manager::is_stale` against the tree-wide window, which is exactly
/// what `filter_tree`'s `Stale` arm applies per series — so `stale_series_keys(..).len()` equals
/// the row count the grid below the strip is about to render.
pub(super) fn stale_series_keys(
    tree: &[vike_data_manager::DataManagerInventoryVenue],
    global_first: i64,
    global_last: i64,
) -> Vec<DataManagerInventorySeriesKey> {
    let mut out = Vec::new();
    for v in tree {
        for symbol_name in &v.symbols {
            for r in &symbol_name.series {
                if vike_data_manager::is_stale(r.coverage.last_timestamp, global_first, global_last)
                {
                    out.push(DataManagerInventorySeriesKey::of(&v.venue, symbol_name, r));
                }
            }
        }
    }
    out
}

/// Every series the `HasGaps` view will show, as the grid's own key type.
///
/// ⚠ The predicate is a NON-EMPTY gap list, not mere presence in the map, and the difference is
/// why this is not `gaps.len()`. `vike_data_manager::DataManagerInventoryGapMap` holds an entry for every series whose
/// gaps were FETCHED — a series fetched and found whole sits in the map with an empty vector — and
/// `filter_tree` keeps it only when `has_gaps` says the list is non-empty.
/// `super::rail::DataManagerRailCounts::gaps` deliberately takes the cheap `len()` because a rail badge
/// may not walk the tree every frame; a strip standing directly above the rows it is counting may
/// not be approximate.
pub(super) fn gapped_series_keys(
    tree: &[vike_data_manager::DataManagerInventoryVenue],
    gaps: &vike_data_manager::DataManagerInventoryGapMap,
) -> Vec<DataManagerInventorySeriesKey> {
    let mut out = Vec::new();
    for v in tree {
        for symbol_name in &v.symbols {
            for r in &symbol_name.series {
                let key = DataManagerInventorySeriesKey::of(&v.venue, symbol_name, r);
                if gaps.get(&key).is_some_and(|g| vike_data_manager::has_gaps(g)) {
                    out.push(key);
                }
            }
        }
    }
    out
}

/// How many of `keys` a bulk backfill would actually PLAN a job for, mirroring
/// `crate::data::backfill::plan::plan_backfill_jobs`'s one client-side gate: `kind == "bar"` AND an
/// interval to request it at. Everything else is counted as a skip by the planner, never silently
/// dropped, and this is the number that lets the button say so BEFORE it is clicked.
///
/// ⚠ **There is no venue term here, and the design mock's hover text says there is one.** That
/// mock reads "bulk backfill serves kind=bar on binance/bybit/okx only", which was true of this
/// tree until `backfill::plan`'s `SUPPORTED_BACKFILL_VENUES` was DELETED — its module doc carries
/// the argument: the roster is the SERVER's (`crates/vike-gatewayhub/src/rest/kline_sources.rs`'s `KLINE_SOURCES`, whose order
/// the datahub's own `real_backfill_table` folds through the gatewayhub client's `KLINE_VENUES`), an off-roster venue is
/// still planned and sent, and the refusal that comes back names the server's own supported set.
/// A client-side venue gate would misreport a capable server as unsupporting, so this window must
/// not restate one — including in a tooltip.
pub(super) fn backfillable(keys: &[DataManagerInventorySeriesKey]) -> usize {
    keys.iter().filter(|k| k.kind == StoreKindId::Bar && k.interval.is_some()).count()
}

/// The Has-gaps segments, in render order.
pub(super) const GAP_SEGMENTS: [ThemeSegment<'static, DataManagerGapCut>; 2] = [
    ThemeSegment {
        value: DataManagerGapCut::PerSeries,
        label: "per-series gaps",
        why: "A missing trading day INSIDE one series' own covered span",
    },
    ThemeSegment {
        value: DataManagerGapCut::CrossKind,
        label: "cross-kind partial days",
        why: "A day where an instrument holds some of its kinds and not others — invisible per \
              series, because each series is contiguous on its own",
    },
];

/// The Stale screen's two sort presets: each segment carries the state it writes.
///
/// ⚠ ONE roster, deliberately: a segment's label, hover and the preset it applies are one value,
/// so the preset a segment CLAIMS and the preset it APPLIES cannot drift. The states are written
/// straight into `vike_data_manager::DataManagerInventoryGridState::sort`, which is a `pub` field the grid already
/// reads — no grid change is involved, and the grid's own column headers keep writing the same
/// field. The value is an `Option` so the live sort can match NEITHER preset: a `None` current value
/// renders both segments unpressed.
///
/// ⚠ `oldest first` is `Updated` ASCENDING because `cmp_flat_row` compares `cov.last_timestamp` for that
/// column, so ascending puts the smallest — the least recently written — first. `most rows first`
/// is `Rows` DESCENDING for the mirror-image reason. Getting either direction backwards produces a
/// screen that is wrong in exactly the way nobody checks, so the derivation is written down.
pub(super) const STALE_SORTS: [ThemeSegment<
    'static,
    Option<vike_data_manager::DataManagerInventorySort>,
>; 2] = [
    ThemeSegment {
        value: Some(vike_data_manager::DataManagerInventorySort {
            column: vike_data_manager::DataManagerInventorySortColumn::Updated,
            ascending: true,
        }),
        label: "oldest first",
        why: "Least recently written series first — the ones furthest behind the tree",
    },
    ThemeSegment {
        value: Some(vike_data_manager::DataManagerInventorySort {
            column: vike_data_manager::DataManagerInventorySortColumn::Rows,
            ascending: false,
        }),
        label: "most rows first",
        why: "Largest series first — what a re-fetch would cost the most to redo",
    },
];

/// `"2025-09-20 → 2026-09-15 · 360 d"` — the tree-wide window a strip is judging against, or the
/// honest absence of one. Both Has-gaps and Stale render it because both are relative judgements
/// about the tree, and neither is interpretable without knowing which tree.
pub(super) fn span_label(global_first: i64, global_last: i64) -> String {
    if global_last <= global_first {
        return "no span yet".to_string();
    }
    format!(
        "{} → {} · {} d",
        vike_model::time::epoch_ms_to_utc_date(global_first),
        vike_model::time::epoch_ms_to_utc_date(global_last),
        (global_last - global_first) / vike_model::MS_PER_DAY,
    )
}

/// The Has-gaps destination's SECOND cut: one row per instrument holding a cross-kind partial day.
///
/// ⚠ **This is rendered here rather than through the shared grid because the shared grid has no
/// view for it, and giving it one is out of bounds.** `vike_data_manager::inventory_grid` is
/// mounted by vike-studio against a narrower panel (this module's header carries the measurement),
/// and a `DataManagerInventoryFilter` variant for partial days would need a row identity the grid does not have:
/// the partial map is keyed per INSTRUMENT (`vike_data::InstrumentCoverageKey`), one level ABOVE the
/// per-series key every grid row carries. There is no series to select.
///
/// ⚠ **Every Fill here is disabled, and the reason is structural rather than "not built yet".**
/// `crate::data::backfill::plan::plan_backfill_jobs` needs a series' `interval` to request anything, and
/// a partial DAY names a kind with no interval at all — so this map cannot produce a job however
/// much backend existed behind it. These rows are EVIDENCE: they tell you a join over that window
/// would run on a kind that is not there, which is a thing to know before trusting a backtest, not
/// a button to press.
pub(super) fn partial_days_list(ui: &mut egui::Ui, context: &WindowToolContext<'_>) {
    let t = ThemeTokens::of(ui.ctx());
    // The remote-mode disclosure first: a datahub that did not answer the cross-kind coverage verb
    // leaves this map EMPTY, and an empty list then reads as "nothing is partial" — the exact
    // false negative `DataManagerInventoryContext::partials_note` exists to prevent. It is rendered above the list,
    // never beside it.
    if let Some(note) = context.stored.partials_note {
        let words = egui::RichText::new(note)
            .font(t.font(ThemeTextRole::Body))
            .color(ThemeStatus::Warning.color());
        ui.label(icons::WARNING.before(ui.style(), words));
        if let Some(cause) = &context.stored.progress.coverage_cause {
            legend(ui, &format!("Cause: {cause}"));
        }
    }
    let total_days: usize = context.stored.partials.values().map(Vec::len).sum();
    legend(
        ui,
        &format!(
            "{} instrument(s) hold a partial day, {total_days} day(s) in total. A partial day is \
             not a gap in any one series — every series involved is contiguous on its own.",
            context.stored.partials.len(),
        ),
    );
    section::strip_rule(ui);
    if context.stored.partials.is_empty() {
        state::view(
            ui,
            ThemeEmptyPane::Empty(
                "No instrument has a day where some kinds are present and others are not — or the \
                 coverage report has not landed yet, which the note above says when it applies.",
            ),
        );
        return;
    }
    // Each row holds a button, so each is the density's control height (spec §3.4).
    let row_h = t.metrics.control_height;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("data_manager_partial_days")
        .show(ui, |ui| {
            for (key, days) in context.stored.partials.iter() {
                if days.is_empty() {
                    continue;
                }
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(data::PARTIAL_WIDTH_SERIES, row_h),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new(format!("{} / {}", key.venue, key.label))
                                    .font(t.mono(ThemeTextRole::Body))
                                    .color(t.theme.text),
                            );
                            if key.grouped {
                                ui.label(
                                    egui::RichText::new("grouped")
                                        .font(t.font(ThemeTextRole::Caption))
                                        .color(t.theme.text_tertiary),
                                );
                            }
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(data::PARTIAL_WIDTH_SUMMARY, row_h),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            // `partial_days_label` is the shared crate's own one-line summary —
                            // "3 partial days (book, quote)" — so this screen and the grid's
                            // Partial column word the same fact identically.
                            ui.label(
                                egui::RichText::new(vike_data_manager::partial_days_label(
                                    context.stored.partials,
                                    key,
                                ))
                                .font(t.font(ThemeTextRole::Body))
                                .color(t.theme.text_secondary),
                            );
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(data::PARTIAL_WIDTH_SPAN, row_h),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            let first = days.first().map(|d| d.start_ms()).unwrap_or(0);
                            let last = days.last().map(|d| d.start_ms()).unwrap_or(0);
                            let span = if days.len() == 1 {
                                vike_model::time::epoch_ms_to_utc_date(first)
                            } else {
                                format!(
                                    "{} … {}",
                                    vike_model::time::epoch_ms_to_utc_date(first),
                                    vike_model::time::epoch_ms_to_utc_date(last),
                                )
                            };
                            ui.label(
                                egui::RichText::new(span)
                                    .font(t.mono(ThemeTextRole::Caption))
                                    .color(t.theme.text_tertiary),
                            );
                        },
                    );
                    dead_action(
                        ui,
                        None,
                        "Fill",
                        "A partial day names a KIND with no interval, and the backfill planner \
                         needs a series' interval to request anything — so no job can be built \
                         from this row by any backend. A kind no venue-direct source serves (book \
                         above all) can only be re-recorded live.",
                    );
                });
            }
        });
}

/// Which cut of "what is missing" a Data Manager view lists: the inspector's two-way segmented
/// control (egui id `data_manager_inspector_cut`) and the Has-gaps destination's strip (this file, egui id
/// `data_manager_gap_mode`), one type with each view's choice in egui TEMP memory under its own id. The two
/// options are different QUESTIONS rather than two filters over one set (see the argument at the
/// inspector's render site, and `vike_data_manager::DataManagerInventoryPartialDayMap`'s own doc).
///
/// [`Self::PerSeries`] is the default because the inspector is a series-scoped pane reached by
/// clicking a series row, and because it is the cut the Backfill action below it can actually act
/// on — a cross-kind partial day is a tick-kind day, which the backfill verb, filling bar series
/// only, cannot be asked to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum DataManagerGapCut {
    /// A hole inside ONE series' own timeline — the grid at `DataManagerInventoryFilter::HasGaps`, and the SELECTED
    /// series' `DataManagerInventoryContext::gaps` in the inspector.
    #[default]
    PerSeries,
    /// A day on which an instrument has some of its kinds and not others — `DataManagerInventoryContext::partials`,
    /// keyed one level above `gaps`: the Has-gaps destination's own list, because the shared grid
    /// has no view for it, and the selected series' INSTRUMENT in the inspector.
    CrossKind,
}
