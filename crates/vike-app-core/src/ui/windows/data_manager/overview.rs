//! Overview — the Data Manager's landing screen: what needs attention, before the filing cabinet.

use super::rail::{self, DataManagerDestination};
#[cfg(doc)]
use super::venue_rollup::by_venue;
use super::{caption, day, refresh};
use crate::ui::windows::WindowToolContext;
use crate::ui::windows::WindowView;
use vike_data_manager::DataManagerInventoryVenue;
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::state::{self, ThemeEmptyPane};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, section};
use vike_ui_theme::icons::{self, ThemeIcon};
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::data;
use vike_utility::bytes::iec_bytes;

/// **Overview** — the window's landing screen: what needs attention, before the filing cabinet.
///
/// Returns a destination when the operator clicked through to one.
///
/// `tv` is taken for the same reason [`by_venue`] takes it, and now for a second: this screen's
/// Refresh raises the ONE `stored_refresh` out-flag the stored grid's Refresh raises, and its
/// per-row Backfill extends the ONE `stored_backfill` OUT vector that grid's bulk Backfill fills.
/// A control here and its twin over there therefore cannot come to mean different things.
pub(super) fn overview(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    tv: &mut WindowView,
    counts: &rail::DataManagerRailCounts,
) -> Option<DataManagerDestination> {
    let t = ThemeTokens::of(ui.ctx());
    let tree = context.stored.tree;
    let (stale_tree, span) = stale_slice(tree, context.stored.gaps);
    let stale: usize = stale_tree.iter().map(|v| v.total.series).sum();
    let partial_instruments = context.stored.partials.len();
    let bytes: u64 = tree.iter().map(|v| v.total.bytes).sum();
    // ⚠ The attention rows wait for EVERY count: gap rows lead the list, so drawing the stale rows
    // first would push them down under the pointer the moment the gap pass ended.
    let (gaps_counting, partials_counting) =
        (context.stored.gaps_counting(), context.stored.partials_counting());
    let counting = gaps_counting || partials_counting;
    let (gap_rows, stale_rows, partial_rows) = if counting { (0, 0, 0) } else { ATTENTION_ROWS };
    let mut go = None;

    // Refresh. The store strip above this panel carries none and neither did this screen, which
    // left the LANDING screen — the one Browse destination an operator arrives on without choosing
    // it — with no route to re-read the index every number below is folded from. REAL: the same OUT
    // flag the stored grid's Refresh and By venue's raise, disabled while a load is in flight for
    // the same reason theirs are — a second request only queues work answering the question already
    // asked.
    let mut refreshed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(t.metrics.gap, t.metrics.gap);
        if refresh(ui, context.stored.loading) {
            tv.stored_refresh = true;
            refreshed = true;
        }
        ui.label(caption(
            &t,
            if context.stored.loading {
                "Reading the store's manifest index…"
            } else {
                "Every number below comes from the last read of the store; Refresh reads it again."
            },
        ));
    });
    ui.add_space(space::MD);
    let mut split_warnings = open_split_warnings(ui, context, tv, refreshed);
    if counting {
        split_warnings.clear();
    }

    // A count still being counted shows the checking mark and says so, never a number that grows
    // as the answers land.
    let counted = |counting: bool, n: usize, sub: String| {
        if counting {
            (vike_data_manager::CHECKING_MARK.to_string(), COUNTING.to_string())
        } else {
            (n.to_string(), sub)
        }
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(space::LG, space::LG);
        let (n, sub) = counted(gaps_counting, counts.gaps, format!("of {}", counts.series));
        tile(ui, &t, &n, "SERIES WITH GAPS", &sub, counts.gaps > 0 && !gaps_counting);
        let (n, sub) = counted(context.stored.listing(), stale, "> 35% of the span behind".into());
        tile(ui, &t, &n, "STALE SERIES", &sub, stale > 0);
        let (n, sub) =
            counted(partials_counting, partial_instruments, "a day some kinds miss".into());
        let warn = partial_instruments > 0 && !partials_counting;
        tile(ui, &t, &n, "PARTIAL INSTRUMENTS", &sub, warn);
        tile(ui, &t, &iec_bytes(bytes), "ON DISK", &format!("{} series", counts.series), false);
    });

    ui.add_space(space::SM);
    section::strip_rule(ui);

    // How many rows this panel COULD show against how many it will. Printing both is what stops a
    // capped list from reading as the whole of the outstanding work — the failure a bare list of
    // three has, where seven series have gaps.
    let outstanding = counts.gaps + stale + partial_instruments + split_warnings.len();
    let shown = counts.gaps.min(gap_rows)
        + stale.min(stale_rows)
        + partial_instruments.min(partial_rows)
        + split_warnings.len();
    ui.label(caption(
        &t,
        format!(
            "NEEDS ATTENTION   ·   {}   ·   judged against {} → {}",
            if counting {
                COUNTING.to_string()
            } else if outstanding == 0 {
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
    // (`DataManagerDestination::icon`), because those two findings ARE rail destinations and a second spelling of
    // a destination's mark rots exactly as its label does. A partial-day row takes `icons::WARNING`
    // instead: no destination owns that finding, so its icon is severity, not identity.
    let gapped =
        context.stored.gaps.iter().filter(|(_, ranges)| vike_data_manager::has_gaps(ranges));
    for (key, ranges) in gapped.take(gap_rows) {
        let days: i64 = ranges.iter().map(|(a, b)| (b - a) / vike_model::MS_PER_DAY + 1).sum();
        if let Some(d) = task(
            ui,
            tv,
            DataManagerDestination::HasGaps.icon(),
            &series_title(&key.venue, &key.symbol, key.kind.word(), key.interval.as_deref()),
            &format!(
                "{} missing day{} across {} range{}.",
                days,
                if days == 1 { "" } else { "s" },
                ranges.len(),
                if ranges.len() == 1 { "" } else { "s" }
            ),
            // The gap map is keyed by exactly the `DataManagerInventorySeriesKey` the backfill planner takes, so this
            // row's Backfill is the design's pill with the real thing behind it — no re-derivation,
            // no second identity for the same series.
            &[
                DataManagerAttentionAction::Backfill(key.clone()),
                DataManagerAttentionAction::Go(DataManagerDestination::HasGaps),
            ],
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
                let behind = (span.1 - r.coverage.last_timestamp).max(0) / vike_model::MS_PER_DAY;
                if let Some(d) = task(
                    ui,
                    tv,
                    DataManagerDestination::Stale.icon(),
                    &series_title(&v.venue, &s.symbol, r.kind.word(), r.interval.as_deref()),
                    &format!(
                        "Last row {} — {} day{} behind the {} this store reaches, past the share \
                         of the window that reads as an abandoned feed rather than a quiet one.",
                        day(r.coverage.last_timestamp),
                        behind,
                        if behind == 1 { "" } else { "s" },
                        day(span.1)
                    ),
                    // The tree carries every field the grid keys a row by, so a stale row builds
                    // the same `DataManagerInventorySeriesKey` the grid would and queues the same planner.
                    &[
                        DataManagerAttentionAction::Backfill(
                            vike_data_manager::DataManagerInventorySeriesKey::of(&v.venue, s, r),
                        ),
                        DataManagerAttentionAction::Go(DataManagerDestination::Stale),
                    ],
                ) {
                    go = Some(d);
                }
            }
        }
    }
    // A share series that jumped overnight like a split no split row covers: the row says the command that fills the
    // row, and the Instruments screen holds the venue's Refresh, which runs it.
    for warning in &split_warnings {
        let title = format!("{} / {}", warning.venue, warning.symbol);
        let acts = [DataManagerAttentionAction::Go(DataManagerDestination::Instruments)];
        if let Some(d) = task(ui, tv, icons::WARNING, &title, &warning.message(), &acts) {
            go = Some(d);
        }
    }
    for (key, days) in context.stored.partials.iter().take(partial_rows) {
        let kinds: Vec<&str> =
            days.iter().flat_map(|d| d.missing_kinds.iter().map(|k| k.word())).collect();
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
            &[
                DataManagerAttentionAction::Go(DataManagerDestination::AllSeries),
                DataManagerAttentionAction::Dead("Backfill", PARTIAL_NO_KEY),
            ],
        ) {
            go = Some(d);
        }
    }
    if counting || outstanding == 0 {
        // Still asking and asked-and-nothing are two renderings (spec §4.2), not two sentences in
        // one grey.
        state::view(
            ui,
            if counting {
                ThemeEmptyPane::Loading(
                    "Still counting — the rows land together once every answer is in.",
                )
            } else if context.stored.loading {
                ThemeEmptyPane::Loading("Still loading — nothing to report yet.")
            } else {
                ThemeEmptyPane::Empty("Nothing needs attention.")
            },
        );
    }

    // The per-row Backfill's ONLY feedback. `crates/vike-app-core/src/ui/windows/data_manager/inventory/mod.rs`'s
    // `stored_tool_content` renders this same line over the grid, and without it here a queued run
    // is indistinguishable from a click that did nothing — the live-but-inert failure this screen
    // exists to avoid, arrived at from the other side.
    if !context.stored.backfill_status.is_empty() {
        ui.add_space(space::SM);
        ui.label(
            egui::RichText::new(context.stored.backfill_status)
                .font(t.font(ThemeTextRole::Body))
                .color(t.theme.text_secondary),
        );
    }

    ui.add_space(space::MD);
    section::strip_rule(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(t.metrics.gap, t.metrics.gap);
        ui.label(caption(&t, "GO TO"));
        for d in GO_TO {
            if ui.add(ThemeButtonAction::secondary((d.icon(), d.label()))).clicked() {
                go = Some(d);
            }
        }
    });
    go
}

/// What a tile, and the attention list's header, say while their count is still being counted.
const COUNTING: &str = "counting\u{2026}";

/// The datahub's open split warnings (`crate::data::split_warnings`): read in the background through the shell's dial,
/// again at `refreshed`, and repainted for once the list grows old.
fn open_split_warnings(
    ui: &egui::Ui,
    context: &WindowToolContext<'_>,
    tv: &mut WindowView,
    refreshed: bool,
) -> Vec<vike_datahub_client::catalog::SplitWarning> {
    use crate::data::split_warnings::DataManagerSplitWarningsView;
    let (address, dial) =
        (context.execution_history.datahub_address, context.execution_history.dial);
    let fetch = address.map(|address| {
        let address = address.to_string();
        move || {
            dial(address)
                .with_reply_timeout(crate::data::data_manager::RPC_DEADLINE)
                .split_warnings()
                .map_err(|e| e.to_string())
        }
    });
    let now = std::time::Instant::now();
    tv.split_warnings.poll(address.unwrap_or_default(), now, refreshed, fetch, ui.ctx());
    ui.ctx().request_repaint_after(DataManagerSplitWarningsView::REREAD);
    tv.split_warnings.open().to_vec()
}

/// The STALE slice of `tree` — the same fold the Stale destination paints — and the tree-wide
/// window it was judged against.
///
/// ⚠ This pays one `filter_tree` clone of the whole tree, which is why the RAIL deliberately carries
/// no stale count (`rail::DataManagerRailCounts`' doc argues that). Paying it on the ONE screen that shows
/// the number is the trade; paying it on every frame of every screen to render a rail count is not.
///
/// ⚠ It hands back the SLICE and not just its count, and that is what holds the cost at one clone:
/// Overview renders the tile (a count) AND the first few stale rows (their identities) out of this
/// one call. Counting here and filtering again to name the rows would pay the clone twice a frame,
/// and the two answers could disagree the moment a background load landed between them.
fn stale_slice(
    tree: &[DataManagerInventoryVenue],
    gaps: &vike_data_manager::DataManagerInventoryGapMap,
) -> (Vec<DataManagerInventoryVenue>, (i64, i64)) {
    let span = vike_data_manager::global_span(tree);
    (
        vike_data_manager::filter_tree(
            tree,
            &vike_data_manager::DataManagerInventoryFilter::Stale,
            gaps,
        ),
        span,
    )
}

/// `venue / symbol / kind`, with the interval appended when the series carries one.
///
/// One spelling for every attention row: a gap row and a stale row name the same series the same
/// way, and both name it the way the stored grid's own selection is keyed — venue, label, kind,
/// interval. Dropping the interval (which the first pass did) makes two series that differ only by
/// bar size render as one row said twice.
fn series_title(venue: &str, symbol: &str, kind: &str, interval: Option<&str>) -> String {
    match interval {
        Some(iv) => format!("{venue} / {symbol} / {kind} · {iv}"),
        None => format!("{venue} / {symbol} / {kind}"),
    }
}

/// One big-number tile. The number stands alone, so it is mono (spec §3.3), at the Display role.
fn tile(ui: &mut egui::Ui, t: &ThemeTokens, n: &str, label: &str, sub: &str, warn: bool) {
    egui::Frame::new()
        .fill(t.theme.surface)
        .stroke(egui::Stroke::new(stroke::HAIRLINE, t.theme.border))
        .corner_radius(egui::CornerRadius::same(RADIUS))
        .inner_margin(egui::Margin::symmetric(space::XL as i8, space::LG as i8))
        .show(ui, |ui| {
            ui.set_min_width(data::TILE_MIN_WIDTH);
            ui.vertical(|ui| {
                let ink = if warn { ThemeStatus::Warning.color() } else { t.theme.text };
                ui.label(egui::RichText::new(n).font(t.mono(ThemeTextRole::Display)).color(ink));
                ui.label(caption(t, label));
                if !sub.is_empty() {
                    ui.add_space(space::XS);
                    ui.label(caption(t, sub));
                }
            });
        });
}

/// What Overview's per-row Backfill actually does, stated where the operator meets it.
///
/// It names the SKIP rather than hiding it: the shell's planner drops a series whose kind no
/// collector serves — a `book` series is the standing example, and the design's own mockup draws
/// that row with a dead `No source` pill — and the status line then reports it as skipped rather
/// than as done. A build with no store engine behind it says so on the same line.
const BACKFILL_HOVER: &str = "Queue a backfill of this one series, as Backfill on All series does. A \
                              kind no source serves is reported as skipped in the status line \
                              under these rows.";

/// Why a partial-day row's Backfill is DARK while a gap row's and a stale row's are live.
///
/// ⚠ This is the one place on Overview where the design draws an action this screen cannot honestly
/// perform, and the answer is a disabled button naming what is missing — never an omission (which
/// teaches nothing) and never a live button that swallows the click.
const PARTIAL_NO_KEY: &str = "A partial day names the kinds an instrument is missing that day, not \
                              one series to backfill. Open the instrument in All series and queue \
                              the series from there.";

/// One action button on an attention row: what the operator can DO about the row, or a named
/// refusal.
///
/// There is deliberately no variant for "the design draws a button here and this pass did not wire
/// it". A control either reaches a real backend (`DataManagerAttentionAction::Go`, `DataManagerAttentionAction::Backfill`) or states on hover what
/// is missing (`DataManagerAttentionAction::Dead`) — this module's doc carries what the old toolbar's ten live-looking
/// buttons cost.
enum DataManagerAttentionAction {
    /// Jump to the screen that owns this row's next step. The button's label is that destination's
    /// own `DataManagerDestination::label`, never a second spelling of it.
    Go(DataManagerDestination),
    /// Queue exactly this series through `WindowView::stored_backfill` — the SAME OUT vector
    /// the stored grid's bulk Backfill extends, so both controls reach one planner, one spawn and
    /// one status line rather than a second path this screen would have to own.
    Backfill(vike_data_manager::DataManagerInventorySeriesKey),
    /// A design action with no backend this row could key it from: drawn disabled, with the reason
    /// as hover text. `(label, why)`.
    Dead(&'static str, &'static str),
}

/// One attention row: an icon, an identity, a ONE-SENTENCE reason, and its actions.
///
/// ⚠ `actions[0]` paints RIGHTMOST — the strip is laid out right-to-left, so the row's BEST
/// available act sits at the panel edge where the design puts it and the rest file in to its left.
fn task(
    ui: &mut egui::Ui,
    tv: &mut WindowView,
    icon: ThemeIcon,
    title: &str,
    why: &str,
    actions: &[DataManagerAttentionAction],
) -> Option<DataManagerDestination> {
    let t = ThemeTokens::of(ui.ctx());
    let mut go = None;
    ui.horizontal(|ui| {
        ui.label(
            icon.rich().size(t.text.px(ThemeTextRole::Body)).color(ThemeStatus::Warning.color()),
        );
        ui.vertical(|ui| {
            // A series identity, read character by character: mono (spec §3.3).
            ui.label(
                egui::RichText::new(title).font(t.mono(ThemeTextRole::Body)).color(t.theme.text),
            );
            ui.label(caption(&t, why));
        });
        if actions.is_empty() {
            return;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for act in actions {
                match act {
                    DataManagerAttentionAction::Go(destination) => {
                        if ui
                            .add(ThemeButtonAction::secondary(destination.label()))
                            .on_hover_text(format!("Open {}.", destination.label()))
                            .clicked()
                        {
                            go = Some(*destination);
                        }
                    }
                    DataManagerAttentionAction::Backfill(key) => {
                        if ui
                            .add(ThemeButtonAction::secondary("Backfill"))
                            .on_hover_text(BACKFILL_HOVER)
                            .clicked()
                        {
                            tv.stored_backfill.push(key.clone());
                        }
                    }
                    DataManagerAttentionAction::Dead(label, why) => {
                        ui.add(ThemeButtonAction::secondary(*label).disabled_because(why));
                    }
                }
            }
        });
    });
    ui.add_space(space::SM);
    go
}

/// The destinations Overview's "GO TO" row offers, in the design's order.
///
/// ⚠ **`AllSeries` LEADS this strip**, and an earlier pass dropped it deliberately on the argument
/// that every attention row above already links into the grid, so a pill would be a third route to
/// one destination. That argument fails in exactly the state this screen is built around: an empty
/// attention list is the GOOD state, and in it no row links anywhere at all — which left the screen
/// holding the most content in the window reachable from the landing screen only through the rail.
/// The design carries the pill, first in the row; so does this.
///
/// The labels and icons are `DataManagerDestination`'s own (`label()`/`icon()`), never restated here: the rail
/// paints those same two, and a second spelling of a destination's name is exactly the copy that
/// rots. ⚠ Which is why the first pill reads *All series* where the design's mockup writes
/// *Stored* — one destination, one name, and the rail's is the name the operator already learned.
const GO_TO: [DataManagerDestination; 7] = [
    DataManagerDestination::AllSeries,
    DataManagerDestination::HasGaps,
    DataManagerDestination::Stale,
    DataManagerDestination::Gateway,
    DataManagerDestination::SymbolSets,
    DataManagerDestination::Providers,
    DataManagerDestination::ActivityLog,
];

/// How many rows of each attention family the list prints before it defers to the screen that owns
/// the whole set — gaps, stale, partial-day, in that order.
///
/// Caps rather than a scroll area: this is the LANDING screen, and a landing screen free to grow
/// into a second grid is the filing cabinet again. The header prints the shown count against the
/// outstanding one, so a capped list can never be read as the whole of the outstanding work.
const ATTENTION_ROWS: (usize, usize, usize) = (3, 2, 2);

#[path = "overview_tests.rs"]
#[cfg(test)]
mod overview_tests;
