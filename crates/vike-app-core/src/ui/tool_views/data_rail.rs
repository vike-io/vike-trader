//! The Data Manager's LEFT RAIL — the window's one navigation — plus the store bar above it.
//!
//! # What this replaces
//!
//! The Data Manager carried SEVEN sub-tabs across the top and then, inside exactly one of them, a
//! SECOND navigation: `crates/vike-data-manager/src/view.rs`'s `views_sidebar`, a rail of
//! Views / Venues / Smart views / Watchlists. Two navigations stacked, and the inner one was where
//! the work happened. This is that inner rail promoted to be the only one.
//!
//! Three of the seven tabs — Historical / Event / Streaming Providers — were rendered by the same
//! trailing `else` arm and printed the same hardcoded list of seven names. They are one destination
//! now.
//!
//! # ⚠ The constraint that was written down and was not true
//!
//! `crates/vike-app-core/src/tools.rs`'s `ToolView` used to carry `data_subtab: usize`, and two
//! comments called it "a persisted INDEX (the workspace file carries it)" and forbade inserting a
//! tab anywhere but the end. **Neither was true.**
//! `crates/vike-app-core/src/ui/workspace/persist.rs`'s `WinSnap` carries no sub-tab field of any kind,
//! its `capture` filters to `WinKind::Chart`, `ToolView` derives `Clone` and nothing else, and
//! `crates/vike-desktop/src/main.rs`'s `persist_egui_memory` returns `false`. That module's own doc
//! says tool windows are "launcher-recreatable and carry runtime state, so they are deliberately
//! skipped".
//!
//! So nothing had to be migrated, and the destinations are free to reorder. The comments were
//! deleted with the index they described — a false constraint costs every future reader the same
//! wasted caution.
//!
//! # ⚠ And the safety net was not real either
//!
//! The old dispatch opened with two `debug_assert_eq!(TABS[…], …)` guards, and
//! `crates/vike-app-core/src/ui/startup.rs`'s `DATA_SUBTAB_STORED` doc claimed a reordered tab list
//! "trips a test". Nothing in `crates/vike-app-core/tests/` ever constructed a `ToolView` or called
//! the dispatch, and `vike-desktop` is excluded from the CI roster, so those guards ran in no lane
//! at all.
//!
//! [`DataDest`] removes the need for one. The old dispatch compared against five values of which
//! THREE were bare literals (`== 0`, `== 1`, `== 5` — note `DATA_SUBTAB_STORED` existed and was not
//! used at the Stored arm), and its trailing `else` indexed a `[&str; 7]` with the raw value, so an
//! out-of-range write panicked the window on render. An enum plus an exhaustive `match` makes both
//! classes unrepresentable.

use crate::ui::tool_views::ToolCtx;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::rail::{RailItem, nav_rail};
use vike_ui_theme::components::{Status, Tokens, chip, section};
use vike_ui_theme::icons::{self, Icon};
use vike_ui_theme::type_scale::TextRole;

/// Which rail group a destination sits under. The groups are the operator's three reasons for
/// opening this window, and they are the rail's only structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailGroup {
    /// Look at what is stored.
    Browse,
    /// Look at what is running, and where it comes from.
    Live,
    /// Change something.
    Configure,
}

impl RailGroup {
    pub fn label(self) -> &'static str {
        match self {
            Self::Browse => "BROWSE",
            Self::Live => "LIVE",
            Self::Configure => "CONFIGURE",
        }
    }
}

/// One destination in the Data Manager's rail — the replacement for the old `data_subtab: usize`.
///
/// ⚠ **Ordering is presentation, not identity.** Nothing persists a `DataDest` (see the module doc),
/// so [`DataDest::ALL`] may be reordered freely; it is the rail's paint order and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DataDest {
    /// What needs attention — the window's landing screen.
    #[default]
    Overview,
    /// The stored inventory grid — the destination with the most content.
    AllSeries,
    /// The stored grid filtered to series with a known hole in their own timeline.
    HasGaps,
    /// The stored grid filtered to series lagging far behind the tree-wide maximum.
    Stale,
    /// One row per venue — rollups, no individual series.
    ByVenue,
    /// The live-feed catalogue.
    CachedFeeds,
    /// Where backfilled data can come from — the merge of three identical tabs.
    Providers,
    /// The in-memory session log, promoted out of the Cached-feeds body.
    ActivityLog,
    /// The DataSet (symbol universe) editor — the old "Symbols" tab.
    DataSets,
    /// Per-venue arming: the ceiling, the credentials and what the mount actually did.
    VenueArming,
    /// The cross-venue instrument catalog the symbol picker searches — per-venue counts, the
    /// refresh stamp `vike_catalog::persist` designed, and the button that re-fetches one venue.
    Instruments,
    /// Which stores are mounted, what each can do, and the layout on disk.
    Store,
}

impl DataDest {
    /// Every destination, in rail paint order. Grouped by [`DataDest::group`], which the rail
    /// renders as a heading whenever it changes — so a destination joins a group by sitting beside
    /// its siblings here, and there is no second list to keep in step.
    pub const ALL: [DataDest; 12] = [
        Self::Overview,
        Self::AllSeries,
        Self::HasGaps,
        Self::Stale,
        Self::ByVenue,
        Self::CachedFeeds,
        Self::Providers,
        Self::ActivityLog,
        Self::DataSets,
        Self::VenueArming,
        Self::Instruments,
        Self::Store,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::AllSeries => "All series",
            Self::HasGaps => "Has gaps",
            Self::Stale => "Stale",
            Self::ByVenue => "By venue",
            Self::CachedFeeds => "Cached feeds",
            Self::Providers => "Providers",
            Self::ActivityLog => "Activity log",
            Self::DataSets => "DataSets",
            Self::VenueArming => super::VENUES_TAB_LABEL,
            Self::Instruments => "Instruments",
            Self::Store => "Store",
        }
    }

    /// The destination's icon, painted dim beside the label — one registry icon per destination,
    /// drawn from the icon family so the column keeps one weight and one line height.
    pub fn icon(self) -> Icon {
        match self {
            Self::Overview => icons::OVERVIEW,
            Self::AllSeries => icons::ALL_SERIES,
            Self::HasGaps => icons::HAS_GAPS,
            Self::Stale => icons::STALE,
            Self::ByVenue => icons::BY_VENUE,
            Self::CachedFeeds => icons::CACHED_FEEDS,
            Self::Providers => icons::PROVIDERS,
            Self::ActivityLog => icons::ACTIVITY_LOG,
            Self::DataSets => icons::DATASETS,
            Self::VenueArming => icons::VENUE_ARMING,
            Self::Instruments => icons::INSTRUMENTS,
            Self::Store => icons::STORE,
        }
    }

    pub fn group(self) -> RailGroup {
        match self {
            Self::Overview | Self::AllSeries | Self::HasGaps | Self::Stale | Self::ByVenue => {
                RailGroup::Browse
            }
            Self::CachedFeeds | Self::Providers | Self::ActivityLog => RailGroup::Live,
            Self::DataSets | Self::VenueArming | Self::Instruments | Self::Store => {
                RailGroup::Configure
            }
        }
    }

    /// Does this destination need the per-frame credential-store read that the venue-arming screen
    /// and no other one does? The shell gates [`super::VenueArmingInputs`] on this, so it stays the
    /// ONE site that owns the comparison — the call site in `crates/vike-desktop/src/main.rs`
    /// deliberately spells no literal of its own.
    pub fn reads_arming(self) -> bool {
        matches!(self, Self::VenueArming)
    }
}

/// The counts the rail renders beside each destination.
///
/// ⚠ **Every field here is cheap to produce, and that is a design constraint rather than an
/// accident.** `crates/vike-data-manager/src/view.rs`'s `filter_tree` deep-clones the whole tree on
/// every call, so a rail count that needed a filter would pay a per-frame clone to render a
/// number. `gaps` is the gap map's LENGTH (it holds an entry only for a series that has one) and
/// the rest are already-owned slice lengths.
///
/// There is deliberately **no stale count**: staleness is a predicate over the tree-wide span, so
/// counting it means filtering, and the Stale destination is where that cost is already being paid.
#[derive(Debug, Clone, Copy, Default)]
pub struct RailCounts {
    /// Total stored series across every venue.
    pub series: usize,
    /// Series with at least one known missing day — `GapMap::len()`.
    pub gaps: usize,
    /// Live cached bar feeds.
    pub feeds: usize,
    /// Saved DataSets (symbol universes).
    pub datasets: usize,
    /// Venues holding at least one stored series.
    pub venues: usize,
}

impl RailCounts {
    /// Build from the render context. Pure, and every read is O(1) or a slice length.
    pub fn from_ctx(ctx: &ToolCtx<'_>) -> Self {
        Self {
            series: ctx.stored.tree.iter().map(|v| v.total.series).sum(),
            gaps: ctx.stored.gaps.len(),
            feeds: ctx.feeds.len(),
            datasets: ctx.dsets.sets.len(),
            venues: ctx.stored.tree.iter().filter(|v| v.total.series > 0).count(),
        }
    }

    /// The count the rail draws beside one destination, or `None` where it draws none — a
    /// destination whose number would be noise, or whose own screen is where the number lives.
    ///
    /// ⚠ `None` on Has gaps for a gap-free store is a KNOWN zero left undrawn, not an unknown count.
    /// An empty good state reads better without a `0` the eye has to dismiss, which is why this has
    /// always hidden it. The kit rail draws nothing for `None` either way.
    fn count(&self, dest: DataDest) -> Option<u64> {
        let n = |v: usize| Some(v as u64);
        match dest {
            DataDest::AllSeries => n(self.series),
            DataDest::HasGaps => (self.gaps > 0).then_some(self.gaps as u64),
            DataDest::CachedFeeds => n(self.feeds),
            DataDest::DataSets => n(self.datasets),
            DataDest::ByVenue => n(self.venues),
            // Stale: see `RailCounts`' doc — counting it costs a tree clone per frame.
            // Overview: its own tiles ARE the counts; a count would say one of them twice.
            // Instruments: its own count is the CACHE's, which the rail does not hold — and a
            // count sourced from anywhere else would be a second number about the same list.
            DataDest::Overview
            | DataDest::Stale
            | DataDest::Providers
            | DataDest::ActivityLog
            | DataDest::VenueArming
            | DataDest::Instruments
            | DataDest::Store => None,
        }
    }
}

/// The rail's width. Wide enough for `Cached feeds` plus a three-digit count without eliding, and
/// no wider — `crates/vike-studio/src/data_browser.rs` records that the shared grid's fixed columns
/// already sum to ~690px, so every pixel the rail takes is one the grid clips.
pub const RAIL_W: f32 = 178.0;

/// Paint the rail and return whether the selection changed this frame.
///
/// Group headings are emitted whenever [`DataDest::group`] changes between consecutive entries of
/// [`DataDest::ALL`], so the grouping has exactly one source.
pub fn rail(ui: &mut egui::Ui, dest: &mut DataDest, counts: &RailCounts) -> bool {
    // The kit's navigation rail took its shape FROM this rail (owner decision 2): group headings,
    // an icon, a label, a mono count, the 2 px accent edge, and the selected row reported as a
    // Label. What stays here is what is the Data Manager's own — which rows, in which groups.
    let items = DataDest::ALL.map(|d| RailItem {
        value: d,
        group: d.group().label(),
        icon: d.icon(),
        label: d.label(),
        count: counts.count(d),
    });
    let changed = nav_rail(ui, dest, &items);
    footer_note(ui);
    changed
}

/// The rail's footer note. It is the one place in the window that can say why the rail exists, and
/// it sits here rather than in a tooltip because a tooltip is a thing you have to already suspect.
fn footer_note(ui: &mut egui::Ui) {
    let t = Tokens::of(ui.ctx());
    let caption =
        |s: &str| egui::RichText::new(s).font(t.font(TextRole::Caption)).color(t.theme.text3);
    section::strip_rule(ui);
    ui.horizontal(|ui| {
        ui.add_space(t.metrics.pad);
        ui.vertical(|ui| {
            ui.label(caption("NO CEILING"));
            ui.label(caption(
                "A thirteenth destination is one more row here. A thirteenth tab would be a \
                 redesign — which is how this window got to seven.",
            ));
        });
    });
}

// ⚠ `footline_reservation` used to live here and is DELETED. It measured the footline's height so
// the rail/body row could be laid out short by it — and the footline was STILL invisible, because
// subtracting from `ui.available_height()` reserves nothing egui honours: it only makes the row
// shorter and leaves the remainder to whatever draws next, inside a region that has already been
// sized. `egui::Panel::bottom`, declared before the row, is what actually reserves.

/// The window's foot status line — what the last refresh produced, and what is outstanding.
pub fn footline(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, counts: &RailCounts) {
    use egui::{Align, Layout, RichText};
    let t = Tokens::of(ui.ctx());
    let caption = |s: String| RichText::new(s).font(t.font(TextRole::Caption));
    // The FILL and the border come from the bottom PANEL this is shown inside
    // (`crates/vike-app-core/src/ui/tool_views/data.rs`'s `data_tool_content`), not from here — one
    // frame, owned by the thing that reserves the space.
    //
    // ⚠ A raised fill rather than a hairline and some dim text, because the first cut drew exactly
    // that at the window's bottom edge on a near-black ground and it could not be seen in the
    // capture — which read as "the layout is wrong" while the layout was also wrong, and cost a
    // whole render to tell the two apart.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.label(caption(format!("{} series indexed", counts.series)).color(t.theme.text3));
        if counts.gaps > 0 {
            // The bullet separates this item from the one before it; the has-gaps icon leads the
            // item itself, a space's width after the bullet rather than the row's item gap.
            let warn = Status::Warning.color();
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                ui.label(caption("\u{2022}".to_string()).color(warn));
                let words = caption(format!("{} with gaps", counts.gaps)).color(warn);
                ui.label(icons::HAS_GAPS.before(ui.style(), words));
            });
        }
        if let Some(note) = ctx.stored.partials_note {
            ui.label(
                caption("\u{2022} partial column unavailable".to_string()).color(t.theme.text3),
            )
            .on_hover_text(note);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(caption(format!("{} live feeds", counts.feeds)).color(t.theme.text3));
        });
    });
}

/// The store bar — one line above the rail and the body, naming the store every destination below
/// it is reading.
///
/// ⚠ **This is the fact the window never stated.** An operator discovered they were on a read-only
/// datahub when Delete greyed out and the Partial column came back empty — two separate surfaces,
/// each explaining itself in its own hover text and neither naming the store. Store identity belongs
/// to no single destination, which is why it sits above all of them; it is the same argument
/// `crates/vike-app-core/src/ui/tool_views/connections.rs`'s `ambient_strip` makes for the live
/// connection.
///
/// The read-only verdict is read off `StoredCtx::delete_unavailable`, which the shell already sets
/// for exactly that reason, rather than from a second probe that could disagree with it.
pub fn store_bar(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, counts: &RailCounts) {
    let t = Tokens::of(ui.ctx());
    let (rows, bytes) = ctx
        .stored
        .tree
        .iter()
        .fold((0_u64, 0_u64), |(r, b), v| (r + v.total.rows, b + v.total.bytes));
    let remote = ctx.stored.delete_unavailable.is_some();
    let caption =
        |s: String| egui::RichText::new(s).font(t.font(TextRole::Caption)).color(t.theme.text3);

    // ⚠ The kit's context bar spans the available width itself. A bar that shrink-wraps its content
    // renders as a narrow pill in the top-left corner — which is what the first GPU capture of this
    // window showed. A store identity that looks like a chip reads as one more control; spanning
    // the width is what makes it the window's header.
    section::context_bar(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(t.metrics.gap, 4.0);

            // The status dot, then the PICKER — a bordered control, not a label. The dot is a
            // STATUS, never the accent (spec §2): a local, writable store is Ok and a remote,
            // read-only one is Warning.
            chip::status_dot(ui, if remote { Status::Warning } else { Status::Ok }, "");
            // ⚠ The picker names the store's KIND and nothing else — no box name, no address, no
            // path. Two reasons, and the first is a hard gate:
            //
            // `crates/vike-ops/tests/shipped_box_name_gate.rs` forbids a box name in a string
            // LITERAL, because `scripts/refuse_box_paths.sh` greps the raw bytes of every published
            // binary and publishes NOTHING on a hit — at TAG time, which cannot be re-run, only
            // re-cut. A first cut of this bar hardcoded a box name here and the gate caught it,
            // which is the cheap place to find out.
            //
            // The second is that the address was INVENTED. Neither the store root nor the datahub
            // address is threaded into `ToolCtx` yet, so a concrete path here would be a
            // plausible-looking guess on the one surface whose entire job is to state which store
            // you are reading. Kind-only is the honest subset; the path joins it when the value
            // does.
            // ⚠ A THIRD honesty problem, and the one that survived two passes of the two above: this
            // was a live `Button` wearing a \u{25BE} caret whose `Response` was dropped on the floor
            // with `let _ =`. So it invited a click, promised a menu, and did nothing — the exact
            // failure the Store destination's own three actions are rendered DISABLED to avoid,
            // sitting one strip above them. Nothing in this binary can switch the mount: the store
            // is resolved once at boot and no destination below can move it. The caret is gone
            // with the affordance, because a caret on a disabled control still promises a menu that
            // does not exist.
            let name = if remote { "remote datahub" } else { "local store" };
            ui.add(ActionButton::secondary(name).disabled_because(PICKER_WHY));

            // The readouts — series / used / capability: dim, and in the sentence face (a number
            // inside a phrase). The capability is a word, so it is never the accent (spec §2).
            let access = if remote { "read \u{00B7} list" } else { "read + write" };
            ui.label(caption(format!("{} series", counts.series)));
            ui.label(caption(format!(
                "{} rows \u{00B7} {}",
                vike_ui_theme::fmt::fmt_count_compact(rows),
                vike_ui_theme::fmt::fmt_bytes(bytes)
            )));
            ui.label(caption(access.to_string()));

            if let Some(reason) = ctx.stored.delete_unavailable {
                chip::badge(ui, "read-only", Status::Warning).on_hover_text(reason);
            }
            if ctx.stored.loading {
                ui.label(caption("loading\u{2026}".to_string()));
            }

            // The OTHER mounts, right-aligned: this window can read more than one store, and until
            // the bar existed it never said so.
            //
            // ⚠ DISABLED, for the third reason argued at the picker above — and they were the same
            // dead control after the picker was fixed alone: live `Button`s whose `Response` was
            // dropped with `let _ =`, hovering "not built yet" over a click that switched nothing.
            // The hover now gives the true reason instead of promising a switch.
            // `crates/vike-app-core/tests/store_bar_screen.rs` holds both halves.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Kind-only here too, for the reason argued at the picker above.
                let other = if remote { "local store" } else { "remote datahub" };
                for (text, dot) in [("archive", false), (other, true)] {
                    let label = format!("{} {text}", if dot { "\u{25CF}" } else { "\u{25CB}" });
                    ui.add(ActionButton::secondary(label).disabled_because(OTHER_MOUNT_WHY));
                }
            });
        });
    });
}

/// The store picker's disabled reason. `crates/vike-app-core/tests/store_bar_screen.rs` reads it
/// back off the tree.
const PICKER_WHY: &str = "Which store every destination below is reading. It is resolved once at \
                          startup and cannot be switched from here.";
/// The other-mount controls' disabled reason. Same test, same wording as before.
const OTHER_MOUNT_WHY: &str = "Not the store this window is reading. The mount is resolved once at \
                               startup and cannot be switched from here.";

#[cfg(test)]
mod tests {
    use super::*;

    /// The rail's grouping has ONE source, and every destination is reachable from it.
    ///
    /// The old tab strip's index↔label agreement was checked by two `debug_assert_eq!`s that ran in
    /// no CI lane (see the module doc). This is the replacement, and unlike them it runs: this crate
    /// is in the derived CI roster.
    #[test]
    fn every_destination_is_in_all_exactly_once() {
        let mut seen: Vec<DataDest> = Vec::new();
        for d in DataDest::ALL {
            assert!(!seen.contains(&d), "{d:?} listed twice in DataDest::ALL");
            seen.push(d);
        }
        assert_eq!(seen.len(), DataDest::ALL.len());
    }

    /// `DataDest::ALL` must be sorted BY GROUP, because [`rail`] emits a heading whenever the group
    /// changes between consecutive entries — an interleaved order would print a group twice.
    #[test]
    fn all_is_contiguous_by_group() {
        let mut runs: Vec<RailGroup> = Vec::new();
        for d in DataDest::ALL {
            if runs.last() != Some(&d.group()) {
                assert!(
                    !runs.contains(&d.group()),
                    "{:?} reappears after another group — the rail would print its heading twice",
                    d.group()
                );
                runs.push(d.group());
            }
        }
        assert_eq!(runs.len(), 3, "three groups: Browse, Live, Configure");
    }

    /// Exactly one destination pays for the credential-store read, and it is the arming screen.
    ///
    /// The shell gates `super::VenueArmingInputs` on this, so a second `true` here would put a
    /// per-frame credential read behind a destination that does not render one.
    #[test]
    fn only_venue_arming_reads_the_credential_store() {
        let reads: Vec<DataDest> = DataDest::ALL.into_iter().filter(|d| d.reads_arming()).collect();
        assert_eq!(reads, vec![DataDest::VenueArming]);
    }

    /// The default destination is a Browse one — the window opens on what is stored, not on an
    /// editor. (It opened on the DataSet editor before the rail, because that was index 0.)
    #[test]
    fn the_default_destination_is_browse() {
        assert_eq!(DataDest::default().group(), RailGroup::Browse);
    }

    /// No rail count may need a tree filter to produce — see [`RailCounts`]' doc for why.
    #[test]
    fn stale_carries_no_count() {
        let c = RailCounts { series: 430, gaps: 7, feeds: 14, datasets: 6, venues: 6 };
        assert_eq!(c.count(DataDest::Stale), None, "a stale count costs a per-frame tree clone");
        assert_eq!(c.count(DataDest::AllSeries), Some(430));
        assert_eq!(c.count(DataDest::HasGaps), Some(7));
    }

    /// A gap-free store draws no has-gaps count at all, rather than a `0` the eye has to dismiss.
    #[test]
    fn a_gap_free_store_draws_no_gap_count() {
        let c = RailCounts { series: 430, gaps: 0, feeds: 14, datasets: 6, venues: 6 };
        assert_eq!(c.count(DataDest::HasGaps), None);
    }
}
