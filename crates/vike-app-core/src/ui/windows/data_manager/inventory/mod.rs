//! The Data Manager's "Stored" sub-tab — the local `HistoryStore` inventory grid. Moved verbatim from
//! `vike-app`'s `main.rs` (tool-view extraction batch 2); the adaptations are mechanical: the five
//! threaded read-only params now arrive grouped in [`crate::ui::windows::DataManagerInventoryContext`] (plus the DataSet store,
//! which supplies the grid's Watchlists), and the header's rows/bytes fold became the named,
//! unit-tested [`tree_totals`].
//!
//! `chart::format_thousands` is spelled `vike_ui_theme::fmt::format_thousands` here — the SAME function
//! (vike-chart re-exports it from vike-ui-theme), reached directly since this crate already
//! depends on the leaf. The byte total beside it is `vike_utility::bytes::iec_bytes`.

use crate::ui::windows::WindowToolContext;
use crate::ui::windows::WindowView;
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::state::{self, ThemeEmptyPane};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, section};
use vike_ui_theme::fmt::format_thousands;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::stored;
use vike_utility::bytes::iec_bytes;

mod inspector;
mod polymarket_proxy;

use inspector::inspector;
pub use polymarket_proxy::{save_polymarket_proxy, seed_polymarket_proxy_box};

/// The stored-inventory body: `inventory_grid` (dense venue-grouped grid + checkbox
/// multi-select + bulk-action bar, over `vike_data_manager::build_inventory_tree`'s output) wrapped
/// with the app-coupled controls the shared crate deliberately excludes — a Refresh button, a
/// byte/row totals summary, and a confirm-gated delete (driven by either a single row-open,
/// unchanged, or the grid's multi-select "Delete" bulk action). Pure render + `tv`-field OUT
/// actions — the actual store I/O (the background load, the delete(s), the chart-open) all
/// happen in `App` (see `refresh_stored` and the `stored_*` drains in `App::ui`), never here.
///
/// ⚠ It used to mount `vike_data_manager::views_sidebar` too, in a left panel of its own. That
/// sidebar is GONE — see the comment at its old site below, and
/// [`crate::ui::windows::data_manager::rail`] for what replaced it. The caller now owns
/// `tv.stored_grid.active_view`, so this function is called by three rail destinations (All series,
/// Has gaps, Stale) that differ only in the filter they set before calling it.
pub fn stored_tool_content(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    tv: &mut WindowView,
) {
    use egui::{Align, Layout, RichText};
    let t = ThemeTokens::of(ui.ctx());
    let (tree, gaps, loading) = (context.stored.tree, context.stored.gaps, context.stored.loading);

    let (total_rows, total_bytes) = tree_totals(tree);

    ui.horizontal(|ui| {
        let refresh = ThemeButtonAction::secondary((icons::REFRESH, "Refresh"));
        let refresh = if loading { refresh.disabled_because(super::LOADING_WHY) } else { refresh };
        if ui.add(refresh).clicked() {
            tv.stored_refresh = true;
        }
        // The mode gate (the #1378 seam close) and the selection gate, each with its reason. With
        // no series open, Delete used to be dark and say nothing.
        let refusal = match (context.stored.delete_unavailable, &tv.stored_last_selection) {
            (Some(reason), _) => Some(reason),
            (None, None) => Some(NOTHING_OPENED),
            (None, Some(_)) => None,
        };
        let delete = ThemeButtonAction::secondary((icons::DELETE, "Delete"));
        let delete = match refusal {
            Some(why) => delete.disabled_because(why),
            None => delete,
        };
        if ui.add(delete).on_hover_text("Delete the last-opened series (irreversible)").clicked() {
            tv.stored_confirm_delete =
                tv.stored_last_selection.clone().map(|s| (s.venue, s.symbol, s.kind, s.interval));
        }
        let status = |s: &str| {
            egui::RichText::new(s).font(t.font(ThemeTextRole::Body)).color(t.theme.text_tertiary)
        };
        if loading {
            ui.label(status("Loading…"));
        }
        // A column that cannot be filled says so HERE, on the strip, and not on a line of its own:
        // the answer that brings it can land after the rows are drawn, and a new line above them
        // would push every one of them down. The note and its cause are the hover.
        let unknown = vike_data_manager::gaps_unknown(gaps);
        let unavailable = [
            context.stored.partials_note.map(|note| {
                (
                    "Partial column unavailable".to_string(),
                    note,
                    &context.stored.progress.coverage_cause,
                )
            }),
            (unknown > 0).then(|| {
                let words = format!("Gaps not known for {unknown} series");
                (words, vike_data_manager::GAPS_NOT_CHECKED, &context.stored.progress.gaps_cause)
            }),
        ];
        for (words, note, cause) in unavailable.into_iter().flatten() {
            let hover = match cause {
                Some(cause) => format!("{note}\n\nCause: {cause}"),
                None => note.to_string(),
            };
            let words = RichText::new(words)
                .font(t.font(ThemeTextRole::Body))
                .color(ThemeStatus::Warning.color());
            ui.label(icons::WARNING.before(ui.style(), words)).on_hover_text(hover);
        }
        // dm-bulk-backfill: the grid's bulk Backfill/Update status ("Backfilling N series (M
        // skipped)…" while `App::maybe_spawn_stored_backfill`'s worker is running, then the final
        // "N backfilled, M failed, K skipped" once it lands) — empty before the first bulk click.
        if !context.stored.backfill_status.is_empty() {
            ui.label(status(context.stored.backfill_status));
        }
        // The coverage LEGEND, right-aligned. It is the grid crate's own, painted in the bars' own
        // colours (owner decision 4). A nested horizontal keeps each mark left of its word inside
        // this right-to-left run.
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!(
                    "{} rows · {}",
                    format_thousands(total_rows as f64).trim_end_matches(".00"),
                    iec_bytes(total_bytes),
                ))
                .font(t.font(ThemeTextRole::Caption))
                .color(t.theme.text_tertiary),
            );
            ui.separator();
            ui.horizontal(vike_data_manager::coverage_legend);
        });
    });
    // Remote mode's honest Partial-column disclosure (the #1378 seam close) is on the strip above:
    // a column the datahub could not fill carries a visible mark, never a silent empty.
    section::strip_rule(ui);

    // First-shown trigger: the first time this tab renders with an empty tree and no load already
    // in flight, request a load once per window, and once more each time this desktop's device key
    // moves (see `stored_auto_requested`'s doc on `WindowView`) — a manual Refresh click always works
    // regardless of this flag.
    let epoch = crate::backend::registry::device_key_epoch();
    if tv.stored_auto_requested != Some(epoch) && !loading && tree.is_empty() {
        tv.stored_auto_requested = Some(epoch);
        tv.stored_refresh = true;
    }

    // ⚠ The Polymarket egress proxy box MOVED to the Providers destination
    // (`crates/vike-app-core/src/ui/windows/data_manager/mod.rs`'s `data_body`). It is not lost, and the reason
    // it sat here is PRESERVED rather than overridden: it had to stay above this function's
    // loading early-return, because the screen an operator reaches it from is precisely an empty or
    // still-loading store — which is what "Polymarket data is not arriving" looks like.
    //
    // Providers keeps that property and strengthens it. That destination has no early return at
    // all, it is reachable while this one is still loading, and "where can data come from, and why
    // is none arriving" is the question it exists to answer. Here it was the second thing on the
    // window's busiest screen, above the grid the screen is named for.
    if loading && tree.is_empty() {
        state::view(ui, ThemeEmptyPane::Loading("Loading stored data…"));
        return;
    }

    // ⚠ THE UNREACHABLE-vs-EMPTY SPLIT, and the reason it is a branch of its own rather than a
    // label added below.
    //
    // An empty tree has always had TWO causes — a store with nothing recorded, and a store nothing
    // could read — and until `DataManagerInventoryContext::load_error` existed they produced the identical picture:
    // the grid below, with "No stored data · 0 rows · 0 B" in its header. So a dead tunnel, a
    // datahub that was not running, a key the server refused and a genuinely fresh install all
    // looked the same, and the load logged nothing at any level that could separate them either.
    // Whichever one an operator guessed, the screen agreed with them.
    //
    // The two must therefore render DIFFERENTLY, and the difference has to be the first thing on
    // the screen rather than a footnote under an empty grid that reads as an answer. So this
    // returns instead of falling through: showing an empty grid AND a failure reason would be the
    // same ambiguity in two panes, and the grid's own totals row ("0 rows") is a claim about the
    // store that nothing here is in a position to make.
    if let Some(why) = context.stored.load_error {
        state::view(ui, ThemeEmptyPane::Unreachable("The stored catalog could not be read"));
        ui.vertical_centered(|ui| {
            // The reason VERBATIM from the load — `data_manager::load`'s own text, which names which verb
            // went unanswered and says whether the store refused or simply went quiet. It is
            // deliberately not paraphrased here: the wording an operator needs to act on is the one
            // in the log, and two spellings of one failure is how the two stop matching.
            ui.label(
                RichText::new(why).font(t.font(ThemeTextRole::Body)).color(t.theme.text_secondary),
            );
            ui.label(
                RichText::new(
                    "This is NOT an empty store — nothing could be read from it, so the grid has \
                     nothing to show. Check that the datahub is running and reachable, then press \
                     Refresh.",
                )
                .font(t.font(ThemeTextRole::Caption))
                .color(t.theme.text_tertiary),
            );
        });
        return;
    }

    // ⚠ There is NO sidebar here any more, and its absence is the redesign.
    //
    // This body used to open an `egui::Panel::left` holding `vike_data_manager::views_sidebar` — a
    // second navigation (Views / Venues / Smart views / Watchlists) nested inside ONE of the Data
    // Manager's seven sub-tabs. Two navigations stacked, and the inner one was where the work
    // happened: a smart view you had to already be in the Stored tab to discover is a smart view
    // nobody applies.
    //
    // `crates/vike-app-core/src/ui/windows/data_manager/rail.rs` is that rail, promoted to be the window's
    // only one. It owns `tv.stored_grid.active_view` now — `data_tool_content` sets it from the
    // destination before calling this function — so this body renders the grid and nothing else.
    // ── grid | inspector ──
    //
    // The inspector is concept D's contribution: a PERMANENT detail pane, so clicking a series
    // costs nothing and you can walk a hundred of them. It also gives the honest disclosures a
    // home — the coverage range, the parts count, the gap list and the partial days all live in
    // hover text today, which is where a fact goes to not be read.
    //
    // ⚠ It is rendered HERE, app-side, off `tv.stored_last_selection`, and NOT inside
    // `vike_data_manager::inventory_grid`. That crate is mounted by vike-studio too, whose
    // `crates/vike-studio/src/panes/data_browser.rs` records that the shared grid's fixed columns
    // already sum to ~690px against a 340px panel; growing the shared render would clip Studio, and
    // no test covers its Data tab. The selection is already app-owned, so the split costs the shared
    // crate nothing.
    let insp_w =
        (ui.available_width() * stored::INSPECTOR_FRACTION).clamp(0.0, stored::INSPECTOR_MAX_WIDTH);
    let grid_w =
        (ui.available_width() - insp_w - stored::INSPECTOR_RESERVE).max(stored::GRID_MIN_WIDTH);
    let response = ui
        .horizontal_top(|ui| {
            let response = ui
                .allocate_ui_with_layout(
                    egui::vec2(grid_w, ui.available_height()),
                    Layout::top_down(Align::Min),
                    |ui| {
                        vike_data_manager::inventory_grid(
                            ui,
                            tree,
                            &mut tv.stored_grid,
                            gaps,
                            context.stored.partials,
                            context.stored.delete_unavailable,
                            &context.stored.progress.gaps_pending,
                            context.stored.progress.coverage_pending,
                        )
                    },
                )
                .inner;
            if insp_w > stored::INSPECTOR_MIN_WIDTH {
                ui.separator();
                ui.allocate_ui_with_layout(
                    egui::vec2(insp_w, ui.available_height()),
                    Layout::top_down(Align::Min),
                    |ui| inspector(ui, context, tv),
                );
            }
            response
        })
        .inner;
    if let Some(selection) = response.opened {
        tv.stored_open =
            Some((selection.venue.clone(), selection.symbol.clone(), selection.interval.clone()));
        tv.stored_last_selection = Some(selection);
    }
    if let Some(action) = response.bulk {
        match action {
            vike_data_manager::DataManagerInventoryGridBulkAction::Delete => {
                tv.stored_confirm_bulk_delete =
                    Some(tv.stored_grid.selected.iter().cloned().collect());
            }
            // dm-bulk-backfill: Backfill and Update alias to the SAME OUT slot — MVP has no
            // separate "ignore gaps, always refetch to now" mode (see
            // `App::maybe_spawn_stored_backfill`'s doc). Non-destructive (network fetch + idempotent
            // ingest), so unlike Delete this needs no confirm modal — it's drained straight into the
            // App's background backfill spawn (`main.rs`'s per-window loop, mirrors
            // `stored_bulk_delete`'s deferred-mutation shape).
            vike_data_manager::DataManagerInventoryGridBulkAction::Backfill
            | vike_data_manager::DataManagerInventoryGridBulkAction::Update => {
                tv.stored_backfill.extend(tv.stored_grid.selected.iter().cloned());
            }
        }
    }

    // Destructive-action confirm modal (mandatory — Delete never fires without it). Mirrors the
    // app's other confirm-style popups: a small centered `egui::Window`, Cancel/Delete only.
    if let Some((venue, symbol, kind, interval)) = tv.stored_confirm_delete.clone() {
        let mut open = true;
        let mut confirmed = false;
        let mut canceled = false;
        egui::Window::new("Delete stored series?")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                let kind_label = match &interval {
                    Some(iv) => format!("{kind}/{iv}"),
                    None => kind.to_string(),
                };
                ui.label(format!(
                    "Delete {kind_label} for {venue}:{symbol}? This removes the data on disk \
                     and cannot be undone."
                ));
                ui.add_space(space::LG);
                ui.horizontal(|ui| {
                    if ui.add(ThemeButtonAction::secondary("Cancel")).clicked() {
                        canceled = true;
                    }
                    if ui.add(ThemeButtonAction::danger("Delete")).clicked() {
                        confirmed = true;
                    }
                });
            });
        if confirmed {
            tv.stored_delete = Some((venue, symbol, kind, interval));
        }
        if confirmed || canceled || !open {
            tv.stored_confirm_delete = None;
        }
    }

    // The grid's multi-select bulk-delete counterpart to the modal above: same shape, N series
    // instead of one, confirmed/canceled the same way.
    if let Some(keys) = tv.stored_confirm_bulk_delete.clone() {
        let mut open = true;
        let mut confirmed = false;
        let mut canceled = false;
        egui::Window::new("Delete selected series?")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "Delete {} selected series? This removes the data on disk and cannot be \
                     undone.",
                    keys.len()
                ));
                ui.add_space(space::LG);
                ui.horizontal(|ui| {
                    if ui.add(ThemeButtonAction::secondary("Cancel")).clicked() {
                        canceled = true;
                    }
                    if ui.add(ThemeButtonAction::danger("Delete")).clicked() {
                        confirmed = true;
                    }
                });
            });
        if confirmed {
            tv.stored_bulk_delete = keys;
            tv.stored_grid.selected.clear();
        }
        if confirmed || canceled || !open {
            tv.stored_confirm_bulk_delete = None;
        }
    }
}

/// Why the stored strip's Delete is dark with no series open: it acts on the last one opened.
const NOTHING_OPENED: &str =
    "Open a series in the grid first — Delete acts on the last one opened.";

/// Sum a venue-grouped inventory tree into the header's `(rows, bytes)` totals. Reads each venue's
/// PRE-AGGREGATED `total` rather than re-walking its symbol/series children, so it stays O(venues)
/// and cannot double-count a series that appears under two nodes. An empty tree totals `(0, 0)` —
/// what the header shows before the first background load lands.
fn tree_totals(tree: &[vike_data_manager::DataManagerInventoryVenue]) -> (u64, u64) {
    tree.iter().fold((0u64, 0u64), |(r, b), v| (r + v.total.rows, b + v.total.bytes))
}

#[cfg(test)]
mod tests {
    use super::tree_totals;
    use vike_data_manager::DataManagerInventoryRollup;
    use vike_data_manager::DataManagerInventoryVenue;

    fn venue(name: &str, rows: u64, bytes: u64) -> DataManagerInventoryVenue {
        DataManagerInventoryVenue {
            venue: name.to_string(),
            symbols: Vec::new(),
            total: DataManagerInventoryRollup { rows, bytes, ..Default::default() },
        }
    }

    #[test]
    fn totals_sum_every_venue() {
        let tree = [venue("binance", 10, 100), venue("okx", 5, 50), venue("bybit", 1, 7)];
        assert_eq!(tree_totals(&tree), (16, 157));
    }

    #[test]
    fn totals_of_an_empty_tree_are_zero() {
        assert_eq!(tree_totals(&[]), (0, 0));
    }
}
