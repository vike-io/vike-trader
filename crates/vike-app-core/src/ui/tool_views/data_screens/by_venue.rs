//! By venue — the per-venue rollup, its scope filter and the pure row fold behind it.

use super::{caption, day, refresh};
use crate::tools;
use crate::ui::tool_views::ToolCtx;
use crate::ui::tool_views::data_rail::DataDest;
use vike_data_manager::model::VenueNode;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::segmented::{self, Segment};
use vike_ui_theme::components::state::{self, Load};
use vike_ui_theme::components::{Status, Tokens};
use vike_ui_theme::fmt::fmt_count_compact;
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::TextRole;
use vike_util::bytes::iec_bytes;

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
/// Returns a destination when a row's `Keys` button was pressed. `tv` is taken because Refresh sets
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
    // temp memory is exactly that: `crates/vike-desktop/src/app_ui.rs`'s `persist_egui_memory`
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
                        "KEYS",
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
                            (iec_bytes(r.bytes), false),
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
                        // Keys. REAL navigation, and NOT an arming control: no screen of this
                        // desktop arms an account (its tier and `active` flag live on the node's
                        // database), so the action opens the credential editor. It is offered on a
                        // roster row too — a venue holding nothing may still want keys.
                        if ui
                            .add(ActionButton::secondary("Keys"))
                            .on_hover_text(
                                "Open the Gateway screen — this venue's stored keys. An \
                                 account trades at its own tier while active, on the node.",
                            )
                            .clicked()
                        {
                            go = Some(DataDest::Gateway);
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
                        (iec_bytes(t_bytes), false),
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
                    // HISTORY and KEYS have no total.
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
