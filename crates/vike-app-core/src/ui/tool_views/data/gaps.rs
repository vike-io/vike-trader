//! The Has gaps and Stale destinations, the pure folds their strips render, and the partial-day list.

use super::{action, dead_action, legend, warning_legend};
use crate::tools;
use crate::ui::tool_views::ToolCtx;
use crate::ui::tool_views::data_rail;
use vike_data::StoreKindId;
use vike_data_manager::SeriesKey;
use vike_ui_theme::components::segmented::{self, Segment};
use vike_ui_theme::components::state::{self, Load};
use vike_ui_theme::components::{Status, Tokens, section};
use vike_ui_theme::icons;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::data;

/// The Has gaps destination: two different questions about missing data (a per-series hole, and a
/// cross-kind partial day) on one screen, behind a segmented control.
pub(super) fn data_has_gaps(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    t: &Tokens,
) {
    use egui::RichText;
    use egui::vec2;
    // ===== Has gaps — two different questions about missing data, on one screen =====
    //
    // The two cuts are NOT two filters over one set. A per-series gap is a hole inside one
    // series' own timeline; a cross-kind partial day is a day on which an INSTRUMENT holds
    // some of its kinds and not others, which is invisible per series because each series
    // is perfectly contiguous on its own. `vike_data_manager::PartialDayMap`'s doc argues
    // that distinction at length and it is the whole reason this destination has a
    // segmented control rather than a checkbox: the second cut has no rows in the grid to
    // filter, so it is rendered here, by this module, over `StoredCtx::partials`.
    let mode_id = ui.id().with("dm_gap_mode");
    let mut mode: GapMode = ui.data_mut(|d| d.get_temp::<GapMode>(mode_id)).unwrap_or_default();
    let gapped = gapped_series_keys(ctx.stored.tree, ctx.stored.gaps);
    let ranges: usize = gapped.iter().filter_map(|k| ctx.stored.gaps.get(k)).map(Vec::len).sum();
    let (gfirst, glast) = vike_data_manager::global_span(ctx.stored.tree);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);
        segmented::segmented(ui, &mut mode, &GAP_SEGMENTS);
        ui.label(RichText::new("Window").font(t.font(TextRole::Caption)).color(t.theme.text3));
        ui.label(
            RichText::new(span_label(gfirst, glast))
                .font(t.mono(TextRole::Caption))
                .color(t.theme.text2),
        );
        if matches!(mode, GapMode::PerSeries) {
            // Live, and it is the SAME out-slot the grid's own bulk bar fills: the keys go
            // to `tools::ToolView::stored_backfill`, which the shell drains into its
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
            let backfill = if gapped.is_empty() {
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
    match mode {
        GapMode::PerSeries => {
            legend(
                ui,
                &format!(
                    "{} series carry a hole, {ranges} range(s) in total. A series that \
                             merely starts late is NOT a gap — every range here is missing from \
                             inside an otherwise covered span.",
                    gapped.len(),
                ),
            );
            section::strip_rule(ui);
            tv.stored_grid.active_view = vike_data_manager::ViewFilter::HasGaps;
            super::stored_tool_content(ui, ctx, tv);
        }
        GapMode::CrossKind => partial_days_list(ui, ctx),
    }
}

/// The Stale destination: the one screen whose threshold is a moving target, so its strip states
/// the tree window and the cut-off it judged against.
pub(super) fn data_stale(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    t: &Tokens,
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
    let (gfirst, glast) = vike_data_manager::global_span(ctx.stored.tree);
    let stale = stale_series_keys(ctx.stored.tree, gfirst, glast);
    // ⚠ The pressed segment is DERIVED from the grid's live sort, never stored beside it:
    // the grid's own column headers also write `GridState::sort`, and a value neither
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
        ui.label(RichText::new("tree window").font(t.font(TextRole::Caption)).color(t.theme.text3));
        ui.label(
            RichText::new(span_label(gfirst, glast))
                .font(t.mono(TextRole::Caption))
                .color(t.theme.text2),
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
    legend(
        ui,
        &match stale_cutoff_ms(gfirst, glast) {
            Some(cut) => format!(
                "{} of {} series are stale: last row before {} (35% of this {}-day window, \
                         and the date moves as the tree does).",
                stale.len(),
                data_rail::RailCounts::from_ctx(ctx).series,
                vike_model::time::epoch_ms_to_utc_date(cut),
                (glast - gfirst) / 86_400_000,
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
    tv.stored_grid.active_view = vike_data_manager::ViewFilter::Stale;
    super::stored_tool_content(ui, ctx, tv);
}

// ===== The pure folds the strips render =====
//
// Each is O(series) over the already-loaded tree and clones nothing but the keys it returns. That
// is deliberate rather than incidental: `vike_data_manager::filter_tree` — which
// `vike_data_manager::stored_catalog_grid` itself calls once per frame, and which any
// count-by-filtering would call a second time — DEEP-CLONES the whole tree. A strip that wanted a
// count that way would be paying a full clone of everything to render one number directly above a
// grid that had just paid for the same clone. `super::data_rail::RailCounts`' own doc makes the
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
    tree: &[vike_data_manager::model::VenueNode],
    global_first: i64,
    global_last: i64,
) -> Vec<SeriesKey> {
    let mut out = Vec::new();
    for v in tree {
        for sym in &v.symbols {
            for r in &sym.series {
                if vike_data_manager::is_stale(r.cov.last_ts, global_first, global_last) {
                    out.push(SeriesKey {
                        venue: v.venue.clone(),
                        symbol: sym.symbol.clone(),
                        kind: r.kind,
                        interval: r.interval.clone(),
                    });
                }
            }
        }
    }
    out
}

/// Every series the `HasGaps` view will show, as the grid's own key type.
///
/// ⚠ The predicate is a NON-EMPTY gap list, not mere presence in the map, and the difference is
/// why this is not `gaps.len()`. `vike_data_manager::GapMap` holds an entry for every series whose
/// gaps were FETCHED — a series fetched and found whole sits in the map with an empty vector — and
/// `filter_tree` keeps it only when `has_gaps` says the list is non-empty.
/// `super::data_rail::RailCounts::gaps` deliberately takes the cheap `len()` because a rail badge
/// may not walk the tree every frame; a strip standing directly above the rows it is counting may
/// not be approximate.
pub(super) fn gapped_series_keys(
    tree: &[vike_data_manager::model::VenueNode],
    gaps: &vike_data_manager::GapMap,
) -> Vec<SeriesKey> {
    let mut out = Vec::new();
    for v in tree {
        for sym in &v.symbols {
            for r in &sym.series {
                let key = SeriesKey {
                    venue: v.venue.clone(),
                    symbol: sym.symbol.clone(),
                    kind: r.kind,
                    interval: r.interval.clone(),
                };
                if gaps.get(&key).is_some_and(|g| vike_data_manager::has_gaps(g)) {
                    out.push(key);
                }
            }
        }
    }
    out
}

/// How many of `keys` a bulk backfill would actually PLAN a job for, mirroring
/// `crate::data::backfill_plan::plan_backfill_jobs`'s one client-side gate: `kind == "bar"` AND an
/// interval to request it at. Everything else is counted as a skip by the planner, never silently
/// dropped, and this is the number that lets the button say so BEFORE it is clicked.
///
/// ⚠ **There is no venue term here, and the design mock's hover text says there is one.** That
/// mock reads "bulk backfill serves kind=bar on binance/bybit/okx only", which was true of this
/// tree until `backfill_plan`'s `SUPPORTED_BACKFILL_VENUES` was DELETED — its module doc carries
/// the argument: the roster is the SERVER's (`crates/vike-datahub/src/backfill.rs`'s
/// `KLINE_SOURCES`, folded into that file's own `real_backfill_table`), an off-roster venue is
/// still planned and sent, and the refusal that comes back names the server's own supported set.
/// A client-side venue gate would misreport a capable server as unsupporting, so this window must
/// not restate one — including in a tooltip.
pub(super) fn backfillable(keys: &[SeriesKey]) -> usize {
    keys.iter().filter(|k| k.kind == StoreKindId::Bar && k.interval.is_some()).count()
}

/// The instant a series' last row must fall before to read as stale, given the tree-wide window —
/// `vike_data_manager::is_stale`'s `behind / span > 0.35` solved for `last_ts`.
///
/// Rendered on the Stale strip because the threshold is a RELATIVE judgement about the tree and
/// reads as a fixed calendar date: it moves every time the tree's newest row does, so the screen
/// states the date it is using rather than leaving an operator to infer one that will be wrong
/// tomorrow. A zero-width window (the empty tree) has no cut-off at all.
///
/// ⚠ **The arithmetic is integer where `is_stale`'s is `f64`, and that is deliberate.** `0.35` has
/// no exact binary representation, so `(span as f64 * 0.35) as i64` over a 100-day window lands a
/// millisecond off the true boundary and the constant wobbles with the span's magnitude — a thing
/// that cannot be asserted about and does not need to be, since this value is rendered at DAY
/// resolution. `* 35 / 100` in `i128` is exact for every span an epoch-ms window can hold. The two
/// can therefore disagree by under a millisecond about a series whose last row falls exactly on
/// the boundary; the FILTER is still `is_stale` alone, so what the grid shows is never decided
/// here — only the date printed above it.
pub(super) fn stale_cutoff_ms(global_first: i64, global_last: i64) -> Option<i64> {
    if global_last <= global_first {
        return None;
    }
    let span = i128::from(global_last - global_first);
    Some(global_last - (span * 35 / 100) as i64)
}

/// Which cut the Has-gaps destination is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum GapMode {
    /// A hole inside ONE series' own timeline — the grid at `ViewFilter::HasGaps`.
    #[default]
    PerSeries,
    /// A day on which an instrument has some of its kinds and not others — this module's own list
    /// over `StoredCtx::partials`, because the shared grid has no view for it.
    CrossKind,
}

/// The Has-gaps segments, in render order.
pub(super) const GAP_SEGMENTS: [Segment<'static, GapMode>; 2] = [
    Segment {
        value: GapMode::PerSeries,
        label: "per-series gaps",
        why: "A missing day INSIDE one series' own covered span — the grid's HasGaps view",
    },
    Segment {
        value: GapMode::CrossKind,
        label: "cross-kind partial days",
        why: "A day where an instrument holds some of its kinds and not others — invisible per \
              series, because each series is contiguous on its own",
    },
];

/// The Stale screen's two sort presets: each segment carries the state it writes.
///
/// ⚠ ONE roster, deliberately: a segment's label, hover and the preset it applies are one value,
/// so the preset a segment CLAIMS and the preset it APPLIES cannot drift. The states are written
/// straight into `vike_data_manager::GridState::sort`, which is a `pub` field the grid already
/// reads — no grid change is involved, and the grid's own column headers keep writing the same
/// field. The value is an `Option` so the live sort can match NEITHER preset: a `None` current value
/// renders both segments unpressed.
///
/// ⚠ `oldest first` is `Updated` ASCENDING because `cmp_flat_row` compares `cov.last_ts` for that
/// column, so ascending puts the smallest — the least recently written — first. `most rows first`
/// is `Rows` DESCENDING for the mirror-image reason. Getting either direction backwards produces a
/// screen that is wrong in exactly the way nobody checks, so the derivation is written down.
pub(super) const STALE_SORTS: [Segment<'static, Option<vike_data_manager::SortState>>; 2] = [
    Segment {
        value: Some(vike_data_manager::SortState {
            column: vike_data_manager::SortColumn::Updated,
            ascending: true,
        }),
        label: "oldest first",
        why: "Least recently written series first — the ones furthest behind the tree",
    },
    Segment {
        value: Some(vike_data_manager::SortState {
            column: vike_data_manager::SortColumn::Rows,
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
        (global_last - global_first) / 86_400_000,
    )
}

/// The Has-gaps destination's SECOND cut: one row per instrument holding a cross-kind partial day.
///
/// ⚠ **This is rendered here rather than through the shared grid because the shared grid has no
/// view for it, and giving it one is out of bounds.** `vike_data_manager::stored_catalog_grid` is
/// mounted by vike-studio against a narrower panel (this module's header carries the measurement),
/// and a `ViewFilter` variant for partial days would need a row identity the grid does not have:
/// the partial map is keyed per INSTRUMENT (`vike_data::InstrumentKey`), one level ABOVE the
/// per-series key every grid row carries. There is no series to select.
///
/// ⚠ **Every Fill here is disabled, and the reason is structural rather than "not built yet".**
/// `crate::data::backfill_plan::plan_backfill_jobs` needs a series' `interval` to request anything, and
/// a partial DAY names a kind with no interval at all — so this map cannot produce a job however
/// much backend existed behind it. These rows are EVIDENCE: they tell you a join over that window
/// would run on a kind that is not there, which is a thing to know before trusting a backtest, not
/// a button to press.
pub(super) fn partial_days_list(ui: &mut egui::Ui, ctx: &ToolCtx<'_>) {
    let t = Tokens::of(ui.ctx());
    // The remote-mode disclosure first: a datahub that did not answer the cross-kind coverage verb
    // leaves this map EMPTY, and an empty list then reads as "nothing is partial" — the exact
    // false negative `StoredCtx::partials_note` exists to prevent. It is rendered above the list,
    // never beside it.
    if let Some(note) = ctx.stored.partials_note {
        let words =
            egui::RichText::new(note).font(t.font(TextRole::Body)).color(Status::Warning.color());
        ui.label(icons::WARNING.before(ui.style(), words));
    }
    let total_days: usize = ctx.stored.partials.values().map(Vec::len).sum();
    legend(
        ui,
        &format!(
            "{} instrument(s) hold a partial day, {total_days} day(s) in total. A partial day is \
             not a gap in any one series — every series involved is contiguous on its own.",
            ctx.stored.partials.len(),
        ),
    );
    section::strip_rule(ui);
    if ctx.stored.partials.is_empty() {
        state::view(
            ui,
            Load::Empty(
                "No instrument has a day where some kinds are present and others are not — or the \
                 coverage report has not landed yet, which the note above says when it applies.",
            ),
        );
        return;
    }
    // Each row holds a button, so each is the density's control height (spec §3.4).
    let row_h = t.metrics.control_h;
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("dm_partial_days").show(
        ui,
        |ui| {
            for (key, days) in ctx.stored.partials.iter() {
                if days.is_empty() {
                    continue;
                }
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(data::PARTIAL_W_SERIES, row_h),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new(format!("{} / {}", key.venue, key.label))
                                    .font(t.mono(TextRole::Body))
                                    .color(t.theme.text),
                            );
                            if key.grouped {
                                ui.label(
                                    egui::RichText::new("grouped")
                                        .font(t.font(TextRole::Caption))
                                        .color(t.theme.text3),
                                );
                            }
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(data::PARTIAL_W_SUMMARY, row_h),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            // `partial_days_label` is the shared crate's own one-line summary —
                            // "3 partial days (book, quote)" — so this screen and the grid's
                            // Partial column word the same fact identically.
                            ui.label(
                                egui::RichText::new(vike_data_manager::partial_days_label(
                                    ctx.stored.partials,
                                    key,
                                ))
                                .font(t.font(TextRole::Body))
                                .color(t.theme.text2),
                            );
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(data::PARTIAL_W_SPAN, row_h),
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
                                    .font(t.mono(TextRole::Caption))
                                    .color(t.theme.text3),
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
        },
    );
}
