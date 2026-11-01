//! The Data Manager's three NEW destinations — Overview, By venue and Store.
//!
//! The rail's other five re-house content that already existed. These three did not exist in any
//! form, and they are Phase 2 of the approved design.
//!
//! # ⚠ Every number here is DERIVED from what the window already loaded
//!
//! Not one of these screens fetches anything. `StoredCtx` already carries the venue tree, the
//! per-series gap map and the cross-kind partial-day map, because the stored grid needs all three;
//! these screens are folds over the same data. That is deliberate — a landing screen that issues
//! its own I/O is a landing screen that can be slow or wrong in a way the screen behind it is not.
//!
//! The one number the design asks for and this module does NOT render is **free disk space**. It is
//! the only tile that needs a syscall nothing else in this window makes, and an invented figure on
//! the screen whose job is to tell you whether you can afford a backfill is worse than no figure.
//!
//! # ⚠ What Overview deliberately does not show
//!
//! The design's fourth tile is *venues armed*. This desktop cannot answer it: an account trades at
//! its own `account.tier` while `account.active = 1`, on the NODE's database, and nothing in this
//! window reads that. Even the credential half would mean a per-frame credential read behind the
//! window's LANDING screen, which
//! `crates/vike-app-core/src/ui/tool_views/data_rail.rs`'s `DataDest::reads_credentials` keeps off
//! it. So there is no tile.
//!
//! # ⚠ Every CONTROL here is either REAL or visibly dead — never a live button that does nothing
//!
//! These screens carry actions as well as numbers now, and the rule they follow is the one
//! `crates/vike-app-core/src/ui/tool_views/data.rs`'s `DataDest::CachedFeeds` arm already follows: an
//! action with no backend is drawn through the kit's `ActionButton::disabled_because`, which is the
//! only way to disable a kit button and takes the reason — naming WHY on hover — rather than a
//! live-looking button that swallows the click.
//! The old Cached Series toolbar shipped TEN live-looking buttons with nothing behind them, and
//! that is the specific failure this window was redesigned to remove — a decorative button added
//! here would be the redesign undoing itself on the screens that replaced it.
//!
//! What is REAL: Overview's Refresh and By venue's (the SAME `tools::ToolView::stored_refresh`
//! out-flag that `crates/vike-app-core/src/ui/tool_views/stored.rs`'s `stored_tool_content` sets, so
//! no two Refresh buttons in this window can drift into meaning different things); Overview's
//! per-row `Backfill` (the SAME `tools::ToolView::stored_backfill` OUT vector the stored grid's
//! bulk Backfill extends — one drain, one planner, one status line, and this screen renders that
//! line rather than letting a queued run look like a click that did nothing); Overview's `GO TO`
//! strip and per-row jumps; By venue's [`VenueScope`] filter (a pure fold — [`venue_rows`] — over
//! the tree this module already holds) and its per-row `Keys` (pure navigation to
//! `DataDest::Gateway`, the screen that holds the credential editor). What is DEAD, each
//! carrying its own reason: [`STORE_ACTIONS`], and the single row action on Overview that names a finding the
//! backfill planner cannot be keyed from (`PARTIAL_NO_KEY`).

mod by_venue;
mod overview;
mod store;

pub use by_venue::{VenueRow, VenueScope, by_venue, venue_rows};
pub use overview::overview;
pub use store::{STORE_ACTIONS, store};

use super::data_rail::DataDest;
use crate::tools;
use vike_data_manager::model::VenueNode;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::table::Column;
use vike_ui_theme::components::{Status, Tokens};
use vike_ui_theme::icons::{self, Icon};
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::data;

/// The STALE slice of `tree` — the same fold the Stale destination paints — and the tree-wide
/// window it was judged against.
///
/// ⚠ This pays one `filter_tree` clone of the whole tree, which is why the RAIL deliberately carries
/// no stale count (`data_rail::RailCounts`' doc argues that). Paying it on the ONE screen that shows
/// the number is the trade; paying it on every frame of every screen to render a rail count is not.
///
/// ⚠ It hands back the SLICE and not just its count, and that is what holds the cost at one clone:
/// Overview renders the tile (a count) AND the first few stale rows (their identities) out of this
/// one call. Counting here and filtering again to name the rows would pay the clone twice a frame,
/// and the two answers could disagree the moment a background load landed between them.
fn stale_slice(
    tree: &[VenueNode],
    gaps: &vike_data_manager::GapMap,
) -> (Vec<VenueNode>, (i64, i64)) {
    let span = vike_data_manager::global_span(tree);
    (vike_data_manager::filter_tree(tree, &vike_data_manager::ViewFilter::Stale, gaps), span)
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

/// Format an epoch-ms bound as a UTC date, or `—` for the zero-width window an empty tree produces.
fn day(ts: i64) -> String {
    if ts <= 0 { "—".to_string() } else { vike_model::time::epoch_ms_to_utc_date(ts) }
}

/// The Refresh both Overview and By venue carry. It raises the SAME `stored_refresh` flag as the
/// stored grid's Refresh, and it is disabled — with the reason — while a load is in flight.
fn refresh(ui: &mut egui::Ui, loading: bool) -> bool {
    let b = ActionButton::secondary((icons::REFRESH, "Refresh"));
    let b = if loading { b.disabled_because(LOADING_WHY) } else { b };
    ui.add(b).on_hover_text(REFRESH_HOVER).clicked()
}

const REFRESH_HOVER: &str =
    "Re-read the store's manifest index — the same load the stored grid's Refresh runs.";

/// Why every Refresh in this window is dark while a load runs: a second request would only queue
/// work answering the question already asked.
pub(super) const LOADING_WHY: &str = "An inventory load is already in flight.";

/// The Store screen's layout-by-kind table. Every cell is text, so it is the kit's data table.
const KIND_COLUMNS: [Column<'static>; 4] = [
    Column {
        title: "KIND",
        weight: data::STORE_KIND_KIND_WEIGHT,
        min_w: data::STORE_KIND_KIND_MIN_W,
        numeric: false,
    },
    Column {
        title: "SERIES",
        weight: data::STORE_KIND_SERIES_WEIGHT,
        min_w: data::STORE_KIND_SERIES_MIN_W,
        numeric: true,
    },
    Column {
        title: "ROWS",
        weight: data::STORE_KIND_ROWS_WEIGHT,
        min_w: data::STORE_KIND_ROWS_MIN_W,
        numeric: true,
    },
    Column {
        title: "SIZE",
        weight: data::STORE_KIND_SIZE_WEIGHT,
        min_w: data::STORE_KIND_SIZE_MIN_W,
        numeric: true,
    },
];

/// A line of the Caption role in the caption grey — these screens' legends, notes and headings.
fn caption(t: &Tokens, s: impl Into<String>) -> egui::RichText {
    egui::RichText::new(s).font(t.font(TextRole::Caption)).color(t.theme.text3)
}

/// One big-number tile. The number stands alone, so it is mono (spec §3.3), at the Display role.
fn tile(ui: &mut egui::Ui, t: &Tokens, n: &str, label: &str, sub: &str, warn: bool) {
    egui::Frame::new()
        .fill(t.theme.surface)
        .stroke(egui::Stroke::new(stroke::HAIRLINE, t.theme.border))
        .corner_radius(egui::CornerRadius::same(RADIUS))
        .inner_margin(egui::Margin::symmetric(space::XL as i8, space::LG as i8))
        .show(ui, |ui| {
            ui.set_min_width(data::TILE_MIN_W);
            ui.vertical(|ui| {
                let ink = if warn { Status::Warning.color() } else { t.theme.text };
                ui.label(egui::RichText::new(n).font(t.mono(TextRole::Display)).color(ink));
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
const BACKFILL_HOVER: &str = "Queue this one series — the same planner and the same background run \
                              the stored grid's bulk Backfill uses. A kind no collector serves is \
                              reported as skipped, and a build with nothing behind it says so in \
                              the status line under these rows.";

/// Why a partial-day row's Backfill is DARK while a gap row's and a stale row's are live.
///
/// ⚠ This is the one place on Overview where the design draws an action this screen cannot honestly
/// perform, and the answer is a disabled button naming what is missing — never an omission (which
/// teaches nothing) and never a live button that swallows the click.
const PARTIAL_NO_KEY: &str = "A partial day is an INSTRUMENT-level finding: it names the kinds \
                              missing on that day, not the kind-and-interval series the backfill \
                              planner is keyed by. Open the instrument on the stored grid and \
                              queue the series from there.";

/// One action button on an attention row: what the operator can DO about the row, or a named
/// refusal.
///
/// There is deliberately no variant for "the design draws a button here and this pass did not wire
/// it". A control either reaches a real backend (`Act::Go`, `Act::Backfill`) or states on hover what
/// is missing (`Act::Dead`) — this module's doc carries what the old toolbar's ten live-looking
/// buttons cost.
enum Act {
    /// Jump to the screen that owns this row's next step. The button's label is that destination's
    /// own `DataDest::label`, never a second spelling of it.
    Go(DataDest),
    /// Queue exactly this series through `tools::ToolView::stored_backfill` — the SAME OUT vector
    /// the stored grid's bulk Backfill extends, so both controls reach one planner, one spawn and
    /// one status line rather than a second path this screen would have to own.
    Backfill(vike_data_manager::SeriesKey),
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
    tv: &mut tools::ToolView,
    icon: Icon,
    title: &str,
    why: &str,
    actions: &[Act],
) -> Option<DataDest> {
    let t = Tokens::of(ui.ctx());
    let mut go = None;
    ui.horizontal(|ui| {
        ui.label(icon.rich().size(t.text.px(TextRole::Body)).color(Status::Warning.color()));
        ui.vertical(|ui| {
            // A series identity, read character by character: mono (spec §3.3).
            ui.label(egui::RichText::new(title).font(t.mono(TextRole::Body)).color(t.theme.text));
            ui.label(caption(&t, why));
        });
        if actions.is_empty() {
            return;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for act in actions {
                match act {
                    Act::Go(dest) => {
                        if ui
                            .add(ActionButton::secondary(dest.label()))
                            .on_hover_text(format!("Open {}.", dest.label()))
                            .clicked()
                        {
                            go = Some(*dest);
                        }
                    }
                    Act::Backfill(key) => {
                        if ui
                            .add(ActionButton::secondary("Backfill"))
                            .on_hover_text(BACKFILL_HOVER)
                            .clicked()
                        {
                            tv.stored_backfill.push(key.clone());
                        }
                    }
                    Act::Dead(label, why) => {
                        ui.add(ActionButton::secondary(*label).disabled_because(why));
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
/// The labels and icons are `DataDest`'s own (`label()`/`icon()`), never restated here: the rail
/// paints those same two, and a second spelling of a destination's name is exactly the copy that
/// rots. ⚠ Which is why the first pill reads *All series* where the design's mockup writes
/// *Stored* — one destination, one name, and the rail's is the name the operator already learned.
const GO_TO: [DataDest; 7] = [
    DataDest::AllSeries,
    DataDest::HasGaps,
    DataDest::Stale,
    DataDest::Gateway,
    DataDest::DataSets,
    DataDest::Providers,
    DataDest::ActivityLog,
];

/// How many rows of each attention family the list prints before it defers to the screen that owns
/// the whole set — gaps, stale, partial-day, in that order.
///
/// Caps rather than a scroll area: this is the LANDING screen, and a landing screen free to grow
/// into a second grid is the filing cabinet again. The header prints the shown count against the
/// outstanding one, so a capped list can never be read as the whole of the outstanding work.
const ATTENTION_ROWS: (usize, usize, usize) = (3, 2, 2);

#[path = "data_screens_tests.rs"]
#[cfg(test)]
mod data_screens_tests;
