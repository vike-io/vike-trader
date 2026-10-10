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
//! The design's fourth tile is *venues armed*. Producing it means reading the credential store, and
//! `crates/vike-app-core/src/ui/tool_views/data_rail.rs`'s `DataDest::reads_arming` gates that read to
//! the venue-arming screen alone — a per-frame credential read behind the window's LANDING screen
//! is a cost every operator pays on every open, to render a number they came for once. The tile is
//! a link to that screen instead.
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
//! the tree this module already holds) and its per-row `Arm` (pure navigation to
//! `DataDest::Credentials`, the screen that actually owns arming). What is DEAD, each carrying its
//! own reason: [`STORE_ACTIONS`], and the single row action on Overview that names a finding the
//! backfill planner cannot be keyed from (`PARTIAL_NO_KEY`).

use super::ToolCtx;
use super::data_rail::{self, DataDest};
use crate::tools;
use vike_data_manager::model::VenueNode;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::segmented::{self, Segment};
use vike_ui_theme::components::state::{self, Load};
use vike_ui_theme::components::table::{self, Column};
use vike_ui_theme::components::{Status, Tokens, chip, section};
use vike_ui_theme::fmt::{fmt_bytes, fmt_count_compact};
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
    DataDest::Credentials,
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
            &series_title(&key.venue, &key.symbol, &key.kind, key.interval.as_deref()),
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
                    &series_title(&v.venue, &s.symbol, &r.kind, r.interval.as_deref()),
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
                            kind: r.kind.clone(),
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
            days.iter().flat_map(|d| d.missing_kinds.iter().map(String::as_str)).collect();
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
        "Venues armed is not a tile: producing it reads the credential store, which this window \
         pays for only on the Credentials screen.",
    ));
    go
}

/// Which venues the By-venue table lists — the design's segmented control.
///
/// ⚠ The three options are not three cosmetic filters; they answer three different questions, and
/// the middle one is why the control exists at all. The TREE is built from what is on disk, so a
/// venue that stored nothing has NO node in it and simply does not appear — which leaves "is aster
/// missing, or did aster store nothing?" a question this screen could not answer.
/// [`VenueScope::Roster`] answers it by drawing the roster (`vike_model::VENUES`) instead of the
/// tree, so a venue holding nothing is a dim zero row rather than an absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VenueScope {
    /// Only venues the store holds at least one series for — the tree exactly as it is built.
    #[default]
    HoldsSeries,
    /// Every venue on `vike_model::VENUES`, whether it stored anything or not.
    Roster,
    /// Only venues with at least one series carrying a known missing day.
    HasGaps,
}

impl VenueScope {
    /// The control's options, in paint order. Ordering is presentation only — nothing persists a
    /// `VenueScope` (see [`by_venue`] for where the selection lives and why).
    pub const ALL: [VenueScope; 3] = [Self::HoldsSeries, Self::Roster, Self::HasGaps];

    pub fn label(self) -> &'static str {
        match self {
            Self::HoldsSeries => "Holds series",
            Self::Roster => "All roster venues",
            Self::HasGaps => "Has gaps",
        }
    }

    /// What the option actually selects, as hover text. Each states the SOURCE of its row set,
    /// because "this venue is missing" and "this venue stored nothing" look identical without it.
    fn hint(self) -> &'static str {
        match self {
            Self::HoldsSeries => {
                "Venues the store holds at least one series for — the rows the inventory tree built."
            }
            Self::Roster => {
                "Every venue with a bridge crate, drawn from the roster rather than the tree: one \
                 that stored nothing is a dim zero row instead of an absence."
            }
            Self::HasGaps => {
                "Venues with at least one series carrying a known missing day, counted from the \
                 same gap map the stored grid paints."
            }
        }
    }

    /// What to print when this scope selects nothing — never a blank panel, which reads as a failed
    /// load rather than as an answer.
    fn empty_note(self) -> &'static str {
        match self {
            Self::HoldsSeries => "No stored data.",
            Self::Roster => "The venue roster is empty, which should be impossible.",
            Self::HasGaps => "No venue holds a series with a known gap.",
        }
    }
}

/// One row of the By-venue table — a venue's rollup, flattened so a ROSTER venue with no node in
/// the tree can occupy a row on equal terms with a stored one.
///
/// Borrowed rather than owned (`venue: &'a str`): the stored rows point into the tree and the
/// roster rows into `vike_model::VENUES`, so the whole table costs no allocation per frame beyond
/// the `Vec` itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueRow<'a> {
    pub venue: &'a str,
    pub symbols: usize,
    pub series: usize,
    pub rows: u64,
    pub bytes: u64,
    /// Series under this venue that the gap map names.
    pub gaps: usize,
    /// Series under this venue whose last row lags past the tree-wide stale threshold.
    pub stale: usize,
}

impl VenueRow<'_> {
    /// Does the store hold anything at all for this venue? `false` only for a roster row.
    pub fn stored(&self) -> bool {
        self.series > 0
    }
}

/// The By-venue table's row set for one [`VenueScope`] — PURE, so the filter is unit-testable
/// without a `Ui`.
///
/// ⚠ `span` is passed in rather than recomputed here: staleness is judged against the TREE-WIDE
/// window, so deriving it per call would let a filtered view judge against its own narrower span
/// and report a different stale count for the same series. The caller takes `global_span` once.
pub fn venue_rows<'a>(
    tree: &'a [VenueNode],
    gaps: &vike_data_manager::GapMap,
    span: (i64, i64),
    scope: VenueScope,
) -> Vec<VenueRow<'a>> {
    let (lo, hi) = span;
    let mut out: Vec<VenueRow<'a>> = tree
        .iter()
        .map(|v| {
            // Per-venue gap and stale counts, folded from the maps the window already holds.
            let gap_series = gaps.keys().filter(|k| k.venue == v.venue).count();
            let stale = v
                .symbols
                .iter()
                .flat_map(|s| s.series.iter())
                .filter(|r| vike_data_manager::is_stale(r.cov.last_ts, lo, hi))
                .count();
            VenueRow {
                venue: v.venue.as_str(),
                symbols: v.symbols.len(),
                series: v.total.series,
                rows: v.total.rows,
                bytes: v.total.bytes,
                gaps: gap_series,
                stale,
            }
        })
        .filter(|r| match scope {
            VenueScope::HoldsSeries => r.series > 0,
            VenueScope::HasGaps => r.gaps > 0,
            VenueScope::Roster => true,
        })
        .collect();

    if scope == VenueScope::Roster {
        // The roster venues the tree built no node for. `vike_model::VENUES` is the ONE canonical
        // roster (its own module doc is the authority), so this screen grows a row on the day a
        // bridge crate lands rather than on the day somebody remembers to add one here.
        //
        // `.copied()` rather than a bare `for venue in VENUES`: the const is a `&[&str]`, so
        // iterating it by reference hands out `&&str` — which compares fine against `r.venue` and
        // then does NOT type-check as the field, because the row borrows one level less deeply.
        for venue in vike_model::VENUES.iter().copied() {
            if !out.iter().any(|r| r.venue == venue) {
                out.push(VenueRow {
                    venue,
                    symbols: 0,
                    series: 0,
                    rows: 0,
                    bytes: 0,
                    gaps: 0,
                    stale: 0,
                });
            }
        }
        // `build_tree` emits BTreeMap order; re-sorting by name files the roster-only rows into the
        // same order rather than appending them as a second block at the bottom.
        out.sort_by(|a, b| a.venue.cmp(b.venue));
    }
    out
}

/// One venue's HISTORY cell, from the load the window holds — `crate::data::history_column`'s
/// `column_cell`, with the borrow the table needs.
fn history_cell_of(ctx: &ToolCtx<'_>, venue: &str) -> (String, bool, String) {
    crate::data::history_column::column_cell(ctx.stored.history, venue)
}

/// **By venue** — one row per venue, no individual series.
///
/// Returns a destination when a row's `Arm` button was pressed. `tv` is taken because Refresh sets
/// the same `stored_refresh` OUT flag the stored grid's own Refresh sets, and one flag with one
/// drain is what stops the window's two Refresh buttons from coming to mean different things.
pub fn by_venue(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
) -> Option<DataDest> {
    let t = Tokens::of(ui.ctx());
    let tree = ctx.stored.tree;
    let span = vike_data_manager::global_span(tree);
    let mut go = None;

    // WHERE THE SELECTED SCOPE LIVES, and why it is not a `ToolView` field. It is a VIEW filter —
    // it selects rows and changes nothing on disk — so it should die with the window, and egui's
    // temp memory is exactly that: `crates/vike-desktop/src/main.rs`'s `persist_egui_memory`
    // returns false, so nothing writes it to the workspace file. The in-tree precedent is
    // `vike_data_manager::stored_catalog_ui`, whose `SortState` rides the same map. The id is
    // salted with `ui.id()` rather than being a bare string, so two Data Manager windows filter
    // INDEPENDENTLY; a global id would let the second window silently retune the first.
    let scope_id = ui.id().with("dm_by_venue_scope");
    let mut scope = ui.data_mut(|d| d.get_temp::<VenueScope>(scope_id)).unwrap_or_default();

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(t.metrics.gap, t.metrics.gap);
        // Refresh. REAL: the same OUT flag `stored.rs`'s `stored_tool_content` raises, drained by
        // the shell, so this button re-runs the ONE background inventory load rather than a second
        // one this screen would have to own. Disabled while a load is in flight for the same reason
        // the grid's is — a second request only queues work answering the question already asked.
        if refresh(ui, ctx.stored.loading) {
            tv.stored_refresh = true;
        }
        let segs = VenueScope::ALL.map(|s| Segment { value: s, label: s.label(), why: s.hint() });
        segmented::segmented(ui, &mut scope, &segs);
        ui.label(caption(&t, format!("window  {} → {}", day(span.0), day(span.1))));
    });
    ui.data_mut(|d| d.insert_temp(scope_id, scope));
    ui.add_space(space::MD);

    // The HISTORY column's caption when its cells are THIS build's table rather than the server's
    // — a datahub older than the history-channels read (`crate::data::history_column`). Shown
    // above the table, the Partial column's `PARTIALS_UNSERVED` shape.
    if let Some(note) = ctx.stored.history.and_then(|h| h.caption()) {
        let line = egui::RichText::new(format!("HISTORY: {note}"))
            .font(t.font(TextRole::Body))
            .color(Status::Warning.color());
        ui.label(icons::WARNING.before(ui.style(), line));
    }

    let rows = venue_rows(tree, ctx.stored.gaps, span, scope);
    if rows.is_empty() {
        state::view(
            ui,
            if ctx.stored.loading {
                Load::Loading("Loading stored data…")
            } else {
                Load::Empty(scope.empty_note())
            },
        );
        return go;
    }

    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("dm_by_venue").show(
        ui,
        |ui| {
            let warn_col = Status::Warning.color();
            // A venue is a word (Inter); every number beside it stands alone (mono) — spec §3.3.
            let venue_font = t.font(TextRole::Body);
            let number_font = t.mono(TextRole::Body);
            egui::Grid::new("dm_by_venue_grid")
                .num_columns(9)
                .spacing([space::XL2, space::SM])
                .striped(true)
                .show(ui, |ui| {
                    for h in [
                        "VENUE", "SYMBOLS", "SERIES", "ROWS", "SIZE", "GAPS", "STALE", "HISTORY",
                        "ARM",
                    ] {
                        ui.label(caption(&t, h));
                    }
                    ui.end_row();

                    let (mut t_sym, mut t_ser, mut t_rows, mut t_bytes) =
                        (0usize, 0usize, 0u64, 0u64);
                    let (mut t_gap, mut t_stale) = (0usize, 0usize);
                    for r in &rows {
                        // A roster row (nothing stored) is drawn dim throughout, so the eye reads
                        // "classified, holds nothing" rather than "a venue with zeros in it".
                        let dim = !r.stored();
                        let body = if dim { t.theme.text3 } else { t.theme.text2 };
                        ui.label(
                            egui::RichText::new(r.venue).font(venue_font.clone()).color(if dim {
                                t.theme.text3
                            } else {
                                t.theme.text
                            }),
                        );
                        for (val, warn) in [
                            (r.symbols.to_string(), false),
                            (r.series.to_string(), false),
                            (fmt_count_compact(r.rows), false),
                            (fmt_bytes(r.bytes), false),
                            (r.gaps.to_string(), r.gaps > 0),
                            (r.stale.to_string(), r.stale > 0),
                        ] {
                            ui.label(
                                egui::RichText::new(val).font(number_font.clone()).color(if warn {
                                    warn_col
                                } else {
                                    body
                                }),
                            );
                        }
                        // HISTORY — how far back this venue's built channels go, and what the
                        // datahub says about its own lanes (`crate::data::history_column`). A word
                        // column, so Inter; the full sentences are the hover.
                        let (label, warn, hover) = history_cell_of(ctx, r.venue);
                        ui.label(
                            egui::RichText::new(label).font(venue_font.clone()).color(if warn {
                                warn_col
                            } else {
                                body
                            }),
                        )
                        .on_hover_text(hover);
                        // Arm. REAL, and deliberately NOT an arming control: this screen owns no
                        // ceiling, no credential read and no policy write, so the honest action is
                        // to open the screen that owns all three. It is offered on a roster row
                        // too — a venue holding nothing is precisely one you may want to arm.
                        if ui
                            .add(ActionButton::secondary("Arm"))
                            .on_hover_text(
                                "Open the Credentials screen — the ceiling, the credentials, and \
                                 what this venue's mount actually did.",
                            )
                            .clicked()
                        {
                            go = Some(DataDest::Credentials);
                        }
                        ui.end_row();

                        t_sym += r.symbols;
                        t_ser += r.series;
                        t_rows += r.rows;
                        t_bytes += r.bytes;
                        t_gap += r.gaps;
                        t_stale += r.stale;
                    }

                    // ⚠ The total row totals the rows SHOWN, not the store, and says so in its own
                    // label. The design's stored grid carries the opposite wart — rollups printed
                    // from the unfiltered total above a filtered grid — and its own note calls that
                    // out; a total that cannot be reconciled with the rows under it is worse than a
                    // narrower one that can.
                    ui.label(
                        egui::RichText::new(format!("TOTAL \u{00B7} {} shown", rows.len()))
                            .font(venue_font.clone())
                            .color(t.theme.text),
                    );
                    for (val, warn) in [
                        (t_sym.to_string(), false),
                        (t_ser.to_string(), false),
                        (fmt_count_compact(t_rows), false),
                        (fmt_bytes(t_bytes), false),
                        (t_gap.to_string(), t_gap > 0),
                        (t_stale.to_string(), t_stale > 0),
                    ] {
                        ui.label(
                            egui::RichText::new(val).font(number_font.clone()).color(if warn {
                                warn_col
                            } else {
                                t.theme.text
                            }),
                        );
                    }
                    // HISTORY and ARM have no total.
                    ui.label("");
                    ui.label("");
                    ui.end_row();
                });
        },
    );

    ui.add_space(space::SM);
    ui.label(caption(
        &t,
        format!(
            "{} \u{00B7} {} venues hold series \u{00B7} {} on the roster",
            scope.label(),
            tree.iter().filter(|v| v.total.series > 0).count(),
            vike_model::VENUES.len(),
        ),
    ));
    go
}

/// The Store screen's actions, each carrying the reason it is DARK.
///
/// ⚠ They are disabled rather than absent, and that is the design's call rather than an oversight
/// waiting to be tidied. An absent button teaches nothing; a dim one with its reason on hover tells
/// an operator that mounting, verifying and compacting are jobs this window knows about and does
/// not perform — which is the answer they came for. The one thing that may NEVER happen here is a
/// live button: see this module's doc for what the ten of those in the old toolbar cost.
///
/// ⚠ The plus sign is ASCII `+`, not the design's fullwidth one: no face the app bundles draws the
/// fullwidth forms block. A symbol last assumed present shipped as blank squares — window controls
/// on Linux, first reported from a screenshot — and `crates/vike-ops/tests/gui/ui_glyph_coverage.rs`
/// now fails any character the GUI prints that no bundled face draws.
pub const STORE_ACTIONS: [(&str, &str); 3] = [
    (
        "+ Mount store…",
        "Nothing in this build mounts a second store. The one this window reads is resolved once, \
         at boot, and is fixed for the life of the process — pointing it elsewhere means the \
         store-root setting (or the settings-directory override that beats the project walk), and \
         then a restart.",
    ),
    (
        "Verify",
        "Verifying means re-reading every manifest against the part files beside it. No verb in \
         this build does that, and a button that merely re-ran the file-index would report a store \
         it never checked as clean.",
    ),
    (
        "Compact",
        "Compaction rewrites the store's part files. It is a write job with no implementation here, \
         and it could not run at all against a read-only mount.",
    ),
];

/// **Store** — which stores are mounted, what each can do, and what the layout on disk holds.
pub fn store(ui: &mut egui::Ui, ctx: &ToolCtx<'_>) {
    let t = Tokens::of(ui.ctx());
    let remote = ctx.stored.delete_unavailable.is_some();
    let tree = ctx.stored.tree;
    // An access mode is words (Inter); a count and a size stand alone (mono) — spec §3.3.
    let words = |s: &str, ink: egui::Color32| {
        egui::RichText::new(s).font(t.font(TextRole::Body)).color(ink)
    };
    let number = |s: String, ink: egui::Color32| {
        egui::RichText::new(s).font(t.mono(TextRole::Body)).color(ink)
    };

    ui.label(caption(&t, "MOUNTED STORES"));
    ui.add_space(space::SM);
    egui::Grid::new("dm_stores")
        .num_columns(4)
        .spacing([space::XL2, space::MD])
        .striped(true)
        .show(ui, |ui| {
            for h in ["STORE", "ACCESS", "SERIES", "SIZE"] {
                ui.label(caption(&t, h));
            }
            ui.end_row();

            let series: usize = tree.iter().map(|v| v.total.series).sum();
            let bytes: u64 = tree.iter().map(|v| v.total.bytes).sum();
            // The CURRENT mount is the one the grid is reading, and which one that is comes off
            // `delete_unavailable` — the same field the grid greys Delete from — rather than a second
            // probe that could disagree with it.
            let (cur, other) = if remote {
                ("remote datahub", "local store")
            } else {
                ("local store", "remote datahub")
            };
            // Mounted is a STATUS: a status dot beside the store's name, never the name drawn in the
            // accent (spec §2).
            chip::status_dot(ui, Status::Ok, cur);
            let access = if remote { "read · list" } else { "read · write · list" };
            ui.label(words(access, t.theme.text2));
            ui.label(number(series.to_string(), t.theme.text2));
            ui.label(number(fmt_bytes(bytes), t.theme.text2));
            ui.end_row();

            chip::status_dot(ui, Status::Muted, other);
            ui.label(words("not mounted", t.theme.text3));
            ui.label(number("—".to_string(), t.theme.text3));
            ui.label(number("—".to_string(), t.theme.text3));
            ui.end_row();
        });

    ui.add_space(space::LG);
    // The design's store actions. Every one is dead, every one says why on hover, and the sentence
    // beneath them says it once more in the open — a hover text is not discoverable, and an
    // operator scanning for "can I compact this" deserves the answer without hunting for it.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(t.metrics.gap, t.metrics.gap);
        for (label, why) in STORE_ACTIONS {
            ui.add(ActionButton::secondary(label).disabled_because(why));
        }
    });
    ui.add_space(space::XS);
    ui.label(caption(
        &t,
        "None of the three is built — mounting, verifying and compacting are still CLI-only jobs. \
         Each button names the one it is waiting for.",
    ));

    ui.add_space(space::MD);
    let warning = |s: String| {
        egui::RichText::new(s).font(t.font(TextRole::Body)).color(Status::Warning.color())
    };
    if let Some(reason) = ctx.stored.delete_unavailable {
        // The whole reason store identity is in the header. A read-only mount costs exactly two
        // things, and before this screen each announced itself separately, in its own hover text.
        let line = warning(format!("On this mount: {reason}"));
        ui.label(icons::WARNING.before(ui.style(), line));
    }
    if let Some(note) = ctx.stored.partials_note {
        let line = warning(note.to_string());
        ui.label(icons::WARNING.before(ui.style(), line));
    }

    section::strip_rule(ui);
    ui.label(caption(&t, "LAYOUT ON DISK"));
    ui.add_space(space::SM);

    // Per-KIND totals, folded from the same tree. No new walk: `kind` is already on every
    // `SeriesRow`, so this is a group-by over data the grid rendered a moment ago.
    let mut by_kind: std::collections::BTreeMap<&str, (usize, u64, u64)> =
        std::collections::BTreeMap::new();
    for v in tree {
        for s in &v.symbols {
            for r in &s.series {
                let e = by_kind.entry(r.kind.as_str()).or_default();
                e.0 += 1;
                e.1 += r.cov.rows;
                e.2 += r.cov.bytes;
            }
        }
    }
    if by_kind.is_empty() {
        state::view(ui, Load::Empty("Nothing stored."));
        return;
    }
    // The kit's data table: every cell is text. Capped in width so four columns do not stretch
    // across the whole body.
    let kinds: Vec<(&str, (usize, u64, u64))> = by_kind.into_iter().collect();
    let kinds_w = ui.available_width().min(data::KINDS_TABLE_MAX_W);
    ui.allocate_ui(egui::vec2(kinds_w, ui.available_height()), |ui| {
        table::data_table(ui, "dm_kinds", &KIND_COLUMNS, kinds.len(), None, |r, c| {
            let (kind, (n, rows, bytes)) = kinds[r];
            match c {
                0 => kind.to_string(),
                1 => n.to_string(),
                2 => fmt_count_compact(rows),
                _ => fmt_bytes(bytes),
            }
        })
    });
}

#[path = "data_screens_tests.rs"]
#[cfg(test)]
mod data_screens_tests;
