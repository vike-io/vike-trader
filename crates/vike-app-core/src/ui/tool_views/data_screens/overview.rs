//! Overview — the Data Manager's landing screen: what needs attention, before the filing cabinet.

#[cfg(doc)]
use super::by_venue::by_venue;
use super::{
    ATTENTION_ROWS, Act, GO_TO, PARTIAL_NO_KEY, caption, day, refresh, series_title, stale_slice,
    task, tile,
};
use crate::tools;
use crate::ui::tool_views::ToolCtx;
use crate::ui::tool_views::data_rail::{self, DataDest};
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::state::{self, Load};
use vike_ui_theme::components::{Tokens, section};
use vike_ui_theme::fmt::fmt_bytes;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::TextRole;

/// **Overview** — the window's landing screen: what needs attention, before the filing cabinet.
///
/// Returns a destination when the operator clicked through to one.
///
/// `tv` is taken for the same reason [`by_venue`] takes it, and now for a second: this screen's
/// Refresh raises the ONE `stored_refresh` out-flag the stored grid's Refresh raises, and its
/// per-row Backfill extends the ONE `stored_backfill` OUT vector that grid's bulk Backfill fills.
/// A control here and its twin over there therefore cannot come to mean different things.
pub fn overview(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    counts: &data_rail::RailCounts,
) -> Option<DataDest> {
    let t = Tokens::of(ui.ctx());
    let tree = ctx.stored.tree;
    let (stale_tree, span) = stale_slice(tree, ctx.stored.gaps);
    let stale: usize = stale_tree.iter().map(|v| v.total.series).sum();
    let partial_instruments = ctx.stored.partials.len();
    let bytes: u64 = tree.iter().map(|v| v.total.bytes).sum();
    let (gap_rows, stale_rows, partial_rows) = ATTENTION_ROWS;
    let mut go = None;

    // Refresh. The store strip above this panel carries none and neither did this screen, which
    // left the LANDING screen — the one Browse destination an operator arrives on without choosing
    // it — with no route to re-read the index every number below is folded from. REAL: the same OUT
    // flag the stored grid's Refresh and By venue's raise, disabled while a load is in flight for
    // the same reason theirs are — a second request only queues work answering the question already
    // asked.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(t.metrics.gap, t.metrics.gap);
        if refresh(ui, ctx.stored.loading) {
            tv.stored_refresh = true;
        }
        ui.label(caption(
            &t,
            if ctx.stored.loading {
                "Reading the store's manifest index…"
            } else {
                "Every number below is folded from that one load — nothing on this screen fetches."
            },
        ));
    });
    ui.add_space(space::MD);

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(space::LG, space::LG);
        tile(
            ui,
            &t,
            &counts.gaps.to_string(),
            "SERIES WITH GAPS",
            &format!("of {}", counts.series),
            counts.gaps > 0,
        );
        tile(ui, &t, &stale.to_string(), "STALE SERIES", "> 35% of the span behind", stale > 0);
        tile(
            ui,
            &t,
            &partial_instruments.to_string(),
            "PARTIAL INSTRUMENTS",
            "a day some kinds miss",
            partial_instruments > 0,
        );
        tile(ui, &t, &fmt_bytes(bytes), "ON DISK", &format!("{} series", counts.series), false);
    });

    ui.add_space(space::SM);
    section::strip_rule(ui);

    // How many rows this panel COULD show against how many it will. Printing both is what stops a
    // capped list from reading as the whole of the outstanding work — the failure a bare list of
    // three has, where seven series have gaps.
    let outstanding = counts.gaps + stale + partial_instruments;
    let shown =
        counts.gaps.min(gap_rows) + stale.min(stale_rows) + partial_instruments.min(partial_rows);
    ui.label(caption(
        &t,
        format!(
            "NEEDS ATTENTION   ·   {}   ·   judged against {} → {}",
            if outstanding == 0 {
                "nothing outstanding".to_string()
            } else if outstanding > shown {
                format!("{shown} of {outstanding} rows")
            } else {
                format!("{outstanding} row{}", if outstanding == 1 { "" } else { "s" })
            },
            day(span.0),
            day(span.1)
        ),
    ));
    ui.add_space(space::MD);

    // The rows are the REAL outstanding items, named from the maps the window already holds — not a
    // fixed list. An empty list is the good state and says so, rather than rendering an empty box
    // that reads as a failed load.
    //
    // ⚠ A gap row and a stale row take the RAIL's icon for the destination they belong to
    // (`DataDest::icon`), because those two findings ARE rail destinations and a second spelling of
    // a destination's mark rots exactly as its label does. A partial-day row takes `icons::WARNING`
    // instead: no destination owns that finding, so its icon is severity, not identity.
    for (key, ranges) in ctx.stored.gaps.iter().take(gap_rows) {
        let days: i64 = ranges.iter().map(|(a, b)| (b - a) / 86_400_000 + 1).sum();
        if let Some(d) = task(
            ui,
            tv,
            DataDest::HasGaps.icon(),
            &series_title(&key.venue, &key.symbol, key.kind.as_str(), key.interval.as_deref()),
            &format!(
                "{} missing day{} across {} range{}.",
                days,
                if days == 1 { "" } else { "s" },
                ranges.len(),
                if ranges.len() == 1 { "" } else { "s" }
            ),
            // The gap map is keyed by exactly the `SeriesKey` the backfill planner takes, so this
            // row's Backfill is the design's pill with the real thing behind it — no re-derivation,
            // no second identity for the same series.
            &[Act::Backfill(key.clone()), Act::Go(DataDest::HasGaps)],
        ) {
            go = Some(d);
        }
    }
    // Stale rows, named out of the SAME filtered slice the tile above counted — so the number and
    // the rows under it can never come from two different folds of the tree.
    let mut stale_left = stale_rows;
    'stale: for v in &stale_tree {
        for s in &v.symbols {
            for r in &s.series {
                if stale_left == 0 {
                    break 'stale;
                }
                stale_left -= 1;
                let behind = (span.1 - r.cov.last_ts).max(0) / 86_400_000;
                if let Some(d) = task(
                    ui,
                    tv,
                    DataDest::Stale.icon(),
                    &series_title(&v.venue, &s.symbol, r.kind.as_str(), r.interval.as_deref()),
                    &format!(
                        "Last row {} — {} day{} behind the {} this store reaches, past the share \
                         of the window that reads as an abandoned feed rather than a quiet one.",
                        day(r.cov.last_ts),
                        behind,
                        if behind == 1 { "" } else { "s" },
                        day(span.1)
                    ),
                    // The tree carries all four fields the grid keys a row by, so a stale row builds
                    // the same `SeriesKey` the grid would and queues the same planner.
                    &[
                        Act::Backfill(vike_data_manager::SeriesKey {
                            venue: v.venue.clone(),
                            symbol: s.symbol.clone(),
                            kind: r.kind,
                            interval: r.interval.clone(),
                        }),
                        Act::Go(DataDest::Stale),
                    ],
                ) {
                    go = Some(d);
                }
            }
        }
    }
    for (key, days) in ctx.stored.partials.iter().take(partial_rows) {
        let kinds: Vec<&str> =
            days.iter().flat_map(|d| d.missing_kinds.iter().map(|k| k.as_str())).collect();
        if let Some(d) = task(
            ui,
            tv,
            icons::WARNING,
            &format!("{} / {}", key.venue, key.label),
            &format!(
                "{} day{} where some kinds have data and others do not ({}).",
                days.len(),
                if days.len() == 1 { "" } else { "s" },
                kinds.first().copied().unwrap_or("—")
            ),
            &[Act::Go(DataDest::AllSeries), Act::Dead("Backfill", PARTIAL_NO_KEY)],
        ) {
            go = Some(d);
        }
    }
    if outstanding == 0 {
        // Still asking and asked-and-nothing are two renderings (spec §4.2), not two sentences in
        // one grey.
        state::view(
            ui,
            if ctx.stored.loading {
                Load::Loading("Still loading — nothing to report yet.")
            } else {
                Load::Empty("Nothing needs attention.")
            },
        );
    }

    // The per-row Backfill's ONLY feedback. `crates/vike-app-core/src/ui/tool_views/stored.rs`'s
    // `stored_tool_content` renders this same line over the grid, and without it here a queued run
    // is indistinguishable from a click that did nothing — the live-but-inert failure this screen
    // exists to avoid, arrived at from the other side.
    if !ctx.stored.backfill_status.is_empty() {
        ui.add_space(space::SM);
        ui.label(
            egui::RichText::new(ctx.stored.backfill_status)
                .font(t.font(TextRole::Body))
                .color(t.theme.text2),
        );
    }

    ui.add_space(space::MD);
    section::strip_rule(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(t.metrics.gap, t.metrics.gap);
        ui.label(caption(&t, "GO TO"));
        for d in GO_TO {
            if ui.add(ActionButton::secondary((d.icon(), d.label()))).clicked() {
                go = Some(d);
            }
        }
    });
    ui.add_space(space::MD);
    ui.label(caption(
        &t,
        "Venues armed is not a tile: an account's tier and active flag live on the node's \
         database, which this window does not read.",
    ));
    go
}
