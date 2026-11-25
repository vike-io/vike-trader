//! The stored grid's permanent detail pane beside the grid: the selected series' facts, its gap
//! cut and its Backfill action. The grid around it is the parent module's.

use crate::ui::windows::WindowToolContext;
use crate::ui::windows::WindowView;
use crate::ui::windows::data_manager::gaps::DataManagerGapCut;
use crate::ui::windows::data_manager::{legend, warning_legend};
use vike_model::StoreKindId;
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::segmented::{self, ThemeSegment};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens};
use vike_ui_theme::fmt::format_thousands;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::stored;
use vike_utility::bytes::iec_bytes;

/// The stored grid's **permanent detail pane** — concept D.
///
/// Renders whatever `tv.stored_last_selection` names, looked up in the tree the grid is already showing.
/// Nothing is fetched: the coverage, the gap ranges and the partial days all come off `DataManagerInventoryContext`,
/// which the grid needed anyway.
///
/// ⚠ **Why the facts here are not in a tooltip.** Every one of them — the exact span, the parts
/// count, the gap list, the cross-kind partial days, the disabled reason on Delete — currently
/// lives in hover text on the grid. A disclosure that requires you to suspect it first is a
/// disclosure nobody reads; the pane is the design's answer, and it costs a click that was already
/// being paid to select the row.
///
/// ⚠ **`tv` is `&mut` and used to be `&`.** The pane now carries an ACTION — the per-series
/// Backfill in its `.acts` row — which writes the one OUT slot the grid's bulk bar already
/// extends. Nothing else about the borrow changed: every fact rendered here is still read off
/// `context`, and this function still performs no I/O.
pub(super) fn inspector(ui: &mut egui::Ui, context: &WindowToolContext<'_>, tv: &mut WindowView) {
    use egui::RichText;
    let t = ThemeTokens::of(ui.ctx());

    let Some(selection) = tv.stored_last_selection.clone() else {
        ui.add_space(space::MD);
        ui.label(
            RichText::new("Select a series").font(t.font(ThemeTextRole::Body)).color(t.theme.text),
        );
        ui.label(
            RichText::new("Its coverage, gaps and partial days appear here — no hover needed.")
                .font(t.font(ThemeTextRole::Caption))
                .color(t.theme.text_tertiary),
        );
        return;
    };

    // Find the row in the tree the grid is rendering, by the selection's whole identity (a grouped
    // and a per-symbol series can share a label). A selection can outlive a refresh that removed
    // the series, so an absent row is a NORMAL state and says so rather than blanking.
    let found = selection.find(context.stored.tree);
    let vike_data_manager::DataManagerInventorySeriesKey { venue, symbol, kind, interval, .. } =
        selection;
    let Some((symbol_node, row)) = found else {
        ui.add_space(space::MD);
        ui.label(
            RichText::new(format!("{venue} / {symbol}"))
                .font(t.mono(ThemeTextRole::Body))
                .color(t.theme.text),
        );
        ui.label(
            RichText::new(
                "No longer in the store — it was deleted, or the last refresh dropped it.",
            )
            .font(t.font(ThemeTextRole::Caption))
            .color(t.theme.text_tertiary),
        );
        return;
    };

    ui.add_space(space::SM);
    ui.label(RichText::new(&symbol).font(t.mono(ThemeTextRole::Title)).color(t.theme.text));
    ui.label(
        RichText::new(match &interval {
            Some(iv) => format!("{venue} · {kind} · {iv}"),
            None => format!("{venue} · {kind}"),
        })
        .font(t.font(ThemeTextRole::Caption))
        .color(t.theme.text_tertiary),
    );
    ui.add_space(space::LG);

    let kv = |ui: &mut egui::Ui, k: &str, v: String| {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                stored::INSPECTOR_KEY_SIZE,
                egui::Layout::left_to_right(egui::Align::Min),
                |ui| {
                    ui.label(
                        RichText::new(k)
                            .font(t.font(ThemeTextRole::Caption))
                            .color(t.theme.text_tertiary),
                    );
                },
            );
            ui.label(
                RichText::new(v).font(t.mono(ThemeTextRole::Body)).color(t.theme.text_secondary),
            );
        });
    };
    kv(ui, "COVERS", vike_data_manager::coverage_label(&row.coverage));
    kv(ui, "ROWS", format_thousands(row.coverage.rows as f64).trim_end_matches(".00").to_string());
    kv(ui, "SIZE", iec_bytes(row.coverage.bytes));
    kv(ui, "PARTS", format!("{} · {} days", row.coverage.parts, row.coverage.dates));

    // ── What is MISSING: the two cuts, and the control that chooses between them ───────────────
    //
    // They are NOT two filters over one set, which is why this is a segmented control rather than
    // one list with a checkbox over it. A per-series gap is a hole inside ONE series' own
    // timeline; a cross-kind partial day is a day on which the INSTRUMENT holds some of its kinds
    // and not others — structurally invisible per series, because each series is perfectly
    // contiguous on its own. `vike_data_manager::DataManagerInventoryPartialDayMap`'s doc argues that distinction at
    // length, and it is the reason both facts were already rendered here unconditionally, stacked,
    // with nothing saying they answer different questions.
    //
    // `crates/vike-app-core/src/ui/windows/data_manager/mod.rs`'s Has-gaps destination makes the same cut over
    // the WHOLE store; this one makes it for the SELECTED series. Each option carries its own
    // count, so the choice is informed BEFORE it is made — an empty cut behind an unlabelled
    // button is indistinguishable from a cut nobody looked at.
    let key = vike_data_manager::DataManagerInventorySeriesKey::of(&venue, symbol_node, row);
    let ranges: &[(i64, i64)] = context.stored.gaps.get(&key).map(Vec::as_slice).unwrap_or(&[]);

    // ⚠ The series key and the instrument key are both built from the TREE NODE the row was found
    // under: they need `DataManagerInventorySymbol::grouped`, and the node's `symbol` is already
    // `SeriesId::label()` (`build_inventory_tree` keys on it), which is the spelling a GROUPED series is
    // identified by. `crates/vike-app-core/src/data/data_manager/mod.rs` records what the other spelling
    // cost: keyed on a grouped series' EMPTY `symbol`, every polymarket group rendered gap-free
    // and no test caught it, because every fixture in that file is `SeriesId::per_symbol`, where
    // the two spellings are indistinguishable.
    let days: &[vike_data::InstrumentCoveragePartialDay] = context
        .stored
        .partials
        .get(&vike_data_manager::instrument_key_of(&venue, symbol_node))
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    // The cut lives in egui TEMP MEMORY keyed off this pane's `Ui` id — the same home
    // `data/mod.rs`'s Has-gaps segmented filter and the grid's own `DataManagerInventorySort` use, and deliberately
    // NOT a new `WindowView` field: a per-frame widget preference that nothing outside this function
    // reads has no business on the struct every tool body threads.
    let cut_id = ui.scope_id().with("data_manager_inspector_cut");
    let mut cut: DataManagerGapCut =
        ui.data_mut(|d| d.get_temp::<DataManagerGapCut>(cut_id)).unwrap_or_default();
    ui.add_space(space::LG);
    let gaps_label = format!("per-series gaps ({})", ranges.len());
    let days_label = format!("cross-kind partial days ({})", days.len());
    let cuts = [
        ThemeSegment {
            value: DataManagerGapCut::PerSeries,
            label: gaps_label.as_str(),
            why: "Holes inside THIS series' own timeline — days it holds nothing, between days it does",
        },
        ThemeSegment {
            value: DataManagerGapCut::CrossKind,
            label: days_label.as_str(),
            why: "Days on which this INSTRUMENT holds some of its kinds and not others — a hole no \
                  single series' own coverage bar can show",
        },
    ];
    // The kit's segmented wraps on a narrow row, which is what this pane's two counted labels
    // need (the property `inspector_segmented` existed for).
    segmented::segmented(ui, &mut cut, &cuts);
    ui.data_mut(|d| d.insert_temp(cut_id, cut));
    ui.add_space(space::SM);

    // ⚠ The cut is STICKY across selections, which is the price of storing it per-pane rather than
    // per-series. So an operator parked on one cut can land on a series that has nothing to show
    // under it — and an empty pane is indistinguishable from a broken one unless it says where the
    // content went. That is what the "…the other cut above" lines below are for; they are the
    // empty state's whole job, not decoration.
    // An answer still on the way is said as such: an empty cut before it lands is no claim at all.
    let progress = context.stored.progress;
    match cut {
        DataManagerGapCut::PerSeries if progress.gaps_pending.contains(&key) => {
            legend(ui, vike_data_manager::GAPS_CHECKING);
        }
        DataManagerGapCut::CrossKind if progress.coverage_pending => {
            legend(ui, vike_data_manager::PARTIALS_CHECKING);
        }
        DataManagerGapCut::PerSeries
            if context.stored.gaps.get(&key).is_some_and(Vec::is_empty) =>
        {
            legend(ui, vike_data_manager::GAPS_NOT_CHECKED);
            if let Some(cause) = &progress.gaps_cause {
                warning_legend(ui, &format!("Cause: {cause}"));
            }
        }
        DataManagerGapCut::PerSeries => {
            if ranges.is_empty() {
                legend(ui, "No gaps of this series' own — contiguous across every day it holds.");
                if !days.is_empty() {
                    legend(
                        ui,
                        &format!(
                            "Its instrument does carry {} cross-kind partial day(s) — the other \
                             cut above.",
                            days.len()
                        ),
                    );
                }
            } else {
                // Spelled out, because the grid paints these as cut-outs in the coverage bar,
                // which says THAT there is a hole and never WHICH days.
                ui.label(
                    icons::HAS_GAPS.before(
                        ui.style(),
                        RichText::new(format!("{} GAP RANGE(S)", ranges.len()))
                            .font(t.font(ThemeTextRole::Caption))
                            .color(ThemeStatus::Warning.color()),
                    ),
                );
                for (a, b) in ranges.iter().take(6) {
                    ui.label(
                        RichText::new(format!(
                            "  {} → {}",
                            vike_model::time::epoch_ms_to_utc_date(*a),
                            vike_model::time::epoch_ms_to_utc_date(*b)
                        ))
                        .font(t.mono(ThemeTextRole::Body))
                        .color(t.theme.text_secondary),
                    );
                }
                if ranges.len() > 6 {
                    legend(ui, &format!("  …and {} more", ranges.len() - 6));
                }
            }
        }
        DataManagerGapCut::CrossKind => {
            if days.is_empty() {
                legend(
                    ui,
                    "No cross-kind partial day — every day this instrument holds, it holds in \
                     every kind it records.",
                );
                if !ranges.is_empty() {
                    legend(
                        ui,
                        &format!(
                            "This series does carry {} gap range(s) of its own — the other cut \
                             above.",
                            ranges.len()
                        ),
                    );
                }
            } else {
                let label = vike_data_manager::symbol_partial_label(
                    context.stored.partials,
                    &venue,
                    symbol_node,
                );
                if !label.is_empty() {
                    ui.label(
                        icons::WARNING.before(
                            ui.style(),
                            RichText::new(label)
                                .font(t.font(ThemeTextRole::Body))
                                .color(ThemeStatus::Warning.color()),
                        ),
                    );
                }
                // ⚠ One row per day naming what is ABSENT — NOT the design's ✓/— matrix with a
                // column per kind — and the reason is that the PRESENT half is not knowable from
                // here. `vike_data::InstrumentCoveragePartialDay` carries `missing_kinds` alone; its complement
                // needs `vike_data::InstrumentCoverage::recorded_kinds`, which no `DataManagerInventoryContext`
                // field reaches. Deriving the columns from the tree node's own series would be
                // WRONG rather than merely approximate: `vike_data::store::coverage::join_coverage`
                // considers only `TICK_KINDS`, while the node also carries bars and properties, so
                // every such column would print a ✓ for a kind the coverage report never looked
                // at. A matrix that cannot be built truthfully is not a narrower matrix — it is a
                // different claim.
                for d in days.iter().take(8) {
                    ui.label(
                        RichText::new(format!(
                            "  {} · no {}",
                            vike_model::time::epoch_ms_to_utc_date(d.start_ms()),
                            d.missing_kinds.iter().map(|k| k.word()).collect::<Vec<_>>().join(", ")
                        ))
                        .font(t.mono(ThemeTextRole::Body))
                        .color(t.theme.text_secondary),
                    );
                }
                if days.len() > 8 {
                    legend(ui, &format!("  …and {} more", days.len() - 8));
                }
                // The design's `.warnbox`, stated for the gate this build actually has rather than
                // for one venue's missing source: the action below requests BAR series only, so no
                // tick-kind day can be ASKED for from this pane whatever the venue. Dukascopy is the
                // one venue where a bar backfill stores ticks at all — its tick lane downloads the
                // bar window's ticks and keeps them as quotes — and the note says so.
                legend(
                    ui,
                    "Some kinds have data on those days and others do not — invisible in any one \
                     series' own bar. Backfill below requests bar series only, so a tick-kind day \
                     closes by recording it live — except on Dukascopy, where a bar backfill also \
                     stores the ticks it downloads for that window.",
                );
            }
        }
    }

    if let Some(note) = context.stored.partials_note {
        ui.add_space(space::MD);
        warning_legend(ui, note);
        if let Some(cause) = &progress.coverage_cause {
            warning_legend(ui, &format!("Cause: {cause}"));
        }
    }

    // ── The action row — the design's `.acts` ─────────────────────────────────────────────────
    //
    // ONE verb, scoped to the SELECTED series, into the very OUT slot the grid's bulk bar already
    // extends (`tv.stored_backfill`, drained by `App::maybe_spawn_stored_backfill`). It is a real
    // job, not a second implementation of one: the planner, the route, the executor and the status
    // line are all the bulk path's, and the only thing this adds is a selection of size one — the
    // case that previously cost a checkbox tick in the grid to express.
    ui.add_space(space::XL);
    // The enabled test is a PREDICTION of what `crate::data::backfill::plan::plan_backfill_jobs` will do
    // with this exact key, not a second rule beside it: `kind == StoreKindId::Bar` with an interval,
    // and deliberately NO venue term. 0059 Phase 3 deleted `SUPPORTED_BACKFILL_VENUES` — which venues
    // have collectors is the SERVER's roster, answered in its own refusal text naming its own
    // supported set — so a client-side venue gate here would gray a button a capable server would
    // have served, and would read in review as coverage.
    let can_backfill = kind == StoreKindId::Bar && interval.is_some();
    ui.horizontal(|ui| {
        // The honest-disabled shape: the reason names the thing that DOES NOT EXIST, never "not
        // implemented". Both refusals below are permanent properties of the series, so they say
        // what would have to change instead of implying a later build will differ (the existing
        // comment, kept).
        let refusal = if kind == StoreKindId::Bar {
            "This bar series carries no interval, so no bar request can name one.".to_string()
        } else {
            format!(
                "The backfill verb fills bar series only — a {kind} series has nothing to request \
                 of any server. Days it is missing close by recording it live (on Dukascopy, a \
                 bar backfill also stores the ticks it downloads)."
            )
        };
        let hover = if ranges.is_empty() {
            "Fetch this series over a default lookback ending now — it carries no known hole to \
             target. The server answers for its own venue roster."
                .to_string()
        } else {
            format!(
                "Fetch exactly this series' {} known gap range(s). The server answers for its own \
                 venue roster.",
                ranges.len()
            )
        };
        let backfill = ThemeButtonAction::secondary("Backfill");
        let backfill = if can_backfill { backfill } else { backfill.disabled_because(&refusal) };
        // De-duplicated because this is a one-click action on a PERMANENTLY-visible pane: the
        // drain is next-frame, and while `maybe_spawn_stored_backfill` folds the vec into a
        // `BTreeSet` anyway (so a double click can never mean two jobs), a duplicate would still
        // skew the skipped/queued tally that run reports.
        if ui.add(backfill).on_hover_text(hover).clicked() && !tv.stored_backfill.contains(&key) {
            tv.stored_backfill.push(key.clone());
        }
    });
    // ⚠ The outcome is deliberately NOT restated here. `context.stored.backfill_status` is the one
    // status slot both the bulk bar and this button feed, and it is already rendered in this tab's
    // header strip; a second copy beside the button could only ever be the same string twice.
    //
    // ⚠ Residual, INHERITED rather than introduced: a click landing while a run is already in
    // flight is a SILENT no-op — `App::maybe_spawn_stored_backfill` returns early on
    // `stored_backfill_running`, and the grid's bulk bar has had that property since it shipped.
    // Closing it means carrying that flag on `DataManagerInventoryContext`, which is not this file's to add.
}
