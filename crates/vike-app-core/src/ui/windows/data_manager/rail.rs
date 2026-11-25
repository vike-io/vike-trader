//! The Data Manager's LEFT RAIL — the window's one navigation — plus the store bar above it.
//!
//! # What this replaces
//!
//! The Data Manager carried SEVEN sub-tabs across the top and then, inside exactly one of them, a
//! SECOND navigation: `crates/vike-data-manager/src/inventory/filter.rs`'s `views_sidebar`, a rail of
//! Views / Venues / Smart views / Watchlists. Two navigations stacked, and the inner one was where
//! the work happened. This is that inner rail promoted to be the only one.
//!
//! Three of the seven tabs — Historical / Event / Streaming Providers — were rendered by the same
//! trailing `else` arm and printed the same hardcoded list of seven names. They are one destination
//! now.
//!
//! # ⚠ The constraint that was written down and was not true
//!
//! `crates/vike-app-core/src/ui/windows/view.rs`'s `WindowView` used to carry `data_subtab: usize`, and two
//! comments called it "a persisted INDEX (the workspace file carries it)" and forbade inserting a
//! tab anywhere but the end. **Neither was true.**
//! `crates/vike-app-core/src/ui/window/workspace/mod.rs`'s `WindowSnapshot` carries no sub-tab field of any kind,
//! its `capture` filters to `WindowKind::Chart`, `WindowView` derives `Clone` and nothing else, and
//! `crates/vike-desktop/src/app_ui/mod.rs`'s `persist_egui_memory` returns `false`. That module's own doc
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
//! "trips a test". Nothing in `crates/vike-app-core/tests/` ever constructed a `WindowView` or called
//! the dispatch, and `vike-desktop` is excluded from the CI roster, so those guards ran in no lane
//! at all.
//!
//! [`DataManagerDestination`] removes the need for one. The old dispatch compared against five values of which
//! THREE were bare literals (`== 0`, `== 1`, `== 5` — note `DATA_SUBTAB_STORED` existed and was not
//! used at the Stored arm), and its trailing `else` indexed a `[&str; 7]` with the raw value, so an
//! out-of-range write panicked the window on render. An enum plus an exhaustive `match` makes both
//! classes unrepresentable.

use crate::ui::windows::WindowToolContext;
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::rail::{ThemeRailItem, navigation_rail};
use vike_ui_theme::components::{ThemeStatus, ThemeTokens, chip, section};
use vike_ui_theme::icons::{self, ThemeIcon};
use vike_ui_theme::maps::{self, ThemeMapRow};
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::ThemeTextRole;

/// Which rail group a destination sits under. The groups are the operator's three reasons for
/// opening this window, and they are the rail's only structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DataManagerRailGroup {
    /// Look at what is stored.
    Browse,
    /// Look at what is running, and where it comes from.
    Live,
    /// Change something.
    Configure,
}

impl DataManagerRailGroup {
    /// This group's row of `ui-theme.toml`'s `rail_group` map (`vike_ui_theme::maps::rail_group`), whose
    /// `word` is the heading the rail prints over the group. Exhaustive: a group without a row does not
    /// compile, and `every_group_has_its_own_rail_group_row_and_every_row_its_group` holds the other
    /// direction. Which group a destination sits under is [`DataManagerDestination::group`]'s, in code.
    fn row(self) -> &'static ThemeMapRow {
        match self {
            Self::Browse => &maps::rail_group::BROWSE,
            Self::Live => &maps::rail_group::LIVE,
            Self::Configure => &maps::rail_group::CONFIGURE,
        }
    }

    /// The heading the rail prints over this group: its row's `word`.
    pub(crate) fn label(self) -> &'static str {
        self.row().word.expect("a rail_group row carries the group's heading")
    }
}

/// One destination in the Data Manager's rail — the replacement for the old `data_subtab: usize`.
///
/// ⚠ **Ordering is presentation, not identity.** Nothing persists a `DataManagerDestination` (see the module doc),
/// so [`DataManagerDestination::ALL`] may be reordered freely; it is the rail's paint order and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DataManagerDestination {
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
    /// The symbol set (symbol universe) editor — the old "Symbols" tab.
    SymbolSets,
    /// The gateway screen: per-venue credential presence and the credential editor — moved in from the
    /// standalone Connections window's Credentials tab, 2026-10-05, and named for the gateway
    /// (decision 0164 verdict 3). It arms nothing: an account's tier and `active` flag live on the
    /// node's database, not on this screen.
    Gateway,
    /// Which backend box this app observes, and its connection state — moved in from the
    /// standalone Connections window's Backend tab, 2026-10-05.
    Backend,
    /// The cross-venue instrument catalog the symbol picker searches — per-venue counts, the
    /// refresh stamp `vike_catalog::persist` designed, and the button that re-fetches one venue.
    Instruments,
    /// Which stores are mounted, what each can do, and the layout on disk.
    Store,
}

impl DataManagerDestination {
    /// Every destination, in rail paint order. Grouped by `DataManagerDestination::group`, which the rail
    /// renders as a heading whenever it changes — so a destination joins a group by sitting beside
    /// its siblings here, and there is no second list to keep in step.
    pub const ALL: [DataManagerDestination; 13] = [
        Self::Overview,
        Self::AllSeries,
        Self::HasGaps,
        Self::Stale,
        Self::ByVenue,
        Self::CachedFeeds,
        Self::Providers,
        Self::ActivityLog,
        Self::SymbolSets,
        Self::Gateway,
        Self::Backend,
        Self::Instruments,
        Self::Store,
    ];

    /// This destination's row of `ui-theme.toml`'s `data_destination` map (`vike_ui_theme::maps::data_destination`): its
    /// `word` is the label the rail prints and its `icon` the glyph beside it, so which words and which glyph
    /// a destination wears is a line of the TOML and nothing here. Exhaustive: a destination without a row
    /// does not compile, and `every_destination_has_its_own_data_destination_row_and_every_row_its_destination`
    /// holds the other direction. (Which group it sits under, and whether it reads the credential
    /// store, are logic and stay below.)
    fn row(self) -> &'static ThemeMapRow {
        match self {
            Self::Overview => &maps::data_destination::OVERVIEW,
            Self::AllSeries => &maps::data_destination::ALL_SERIES,
            Self::HasGaps => &maps::data_destination::HAS_GAPS,
            Self::Stale => &maps::data_destination::STALE,
            Self::ByVenue => &maps::data_destination::BY_VENUE,
            Self::CachedFeeds => &maps::data_destination::CACHED_FEEDS,
            Self::Providers => &maps::data_destination::PROVIDERS,
            Self::ActivityLog => &maps::data_destination::ACTIVITY_LOG,
            Self::SymbolSets => &maps::data_destination::SYMBOL_SETS,
            Self::Gateway => &maps::data_destination::GATEWAY,
            Self::Backend => &maps::data_destination::BACKEND,
            Self::Instruments => &maps::data_destination::INSTRUMENTS,
            Self::Store => &maps::data_destination::STORE,
        }
    }

    /// The destination's name in the rail: its row's `word`.
    pub fn label(self) -> &'static str {
        self.row().word.expect("a data_destination row carries the destination's label")
    }

    /// The destination's icon, painted dim beside the label — one registry icon per destination,
    /// drawn from the icon family so the column keeps one weight and one line height: its row's `icon`.
    pub fn icon(self) -> ThemeIcon {
        self.row().icon().expect("a data_destination row names an icon of the registry")
    }

    pub(crate) fn group(self) -> DataManagerRailGroup {
        match self {
            Self::Overview | Self::AllSeries | Self::HasGaps | Self::Stale | Self::ByVenue => {
                DataManagerRailGroup::Browse
            }
            Self::CachedFeeds | Self::Providers | Self::ActivityLog => DataManagerRailGroup::Live,
            Self::SymbolSets | Self::Gateway | Self::Backend | Self::Instruments | Self::Store => {
                DataManagerRailGroup::Configure
            }
        }
    }

    /// Does this destination need the credential-store reads
    /// (`crates/vike-desktop/src/main.rs`'s `workspace_credentials` and `workspace_account_grids`)?
    /// `super::DataManagerCredentialReads::read` gates both reads (held from its arrival, never per
    /// frame) AND the `catalog.set_credentialed_venues` push on it, so this stays the ONE site that
    /// owns the comparison — the call site in `crates/vike-desktop/src/tool_window.rs`
    /// deliberately spells no literal of its own.
    ///
    /// Two of thirteen, each independently real — not guessed, each traced to its actual
    /// consumer:
    /// * [`Self::Gateway`] passes the `grids` (the account rows and their credentials, with
    ///   the verdict of those reads) straight into `account_manager_ui` (`data/mod.rs`'s dispatch match) —
    ///   the account rail, the P/D/L dots and the store-unreadable banner all read them directly.
    /// * [`Self::Instruments`] takes no `grids` as a dispatch parameter, but its
    ///   row text ("would arm it" vs. "it is armed", `docs/decisions/0066` decision 9) reads
    ///   `context.catalog_refresh`'s credentialed-venues set, which only a fresh credential read can push
    ///   (`DatahubCatalogRefresh::set_credentialed_venues`'s own doc). Its dependency is on that PUSH
    ///   happening, not on the param — gating the read without also gating the push would
    ///   leave this destination showing a stale set.
    ///
    /// [`Self::Backend`] read the store too until decision 0288's cutover: its form checked typed
    /// shared-key NAMES against it, and a record names no key any more. Every other destination
    /// takes `grids` only because [`super::data_tool_content`]'s signature is one dispatcher for all
    /// thirteen — none reads it.
    pub fn reads_credentials(self) -> bool {
        matches!(self, Self::Gateway | Self::Instruments)
    }
}

/// The counts the rail renders beside each destination.
///
/// ⚠ **Every field here is cheap to produce, and that is a design constraint rather than an
/// accident.** `crates/vike-data-manager/src/inventory/filter.rs`'s `filter_tree` deep-clones the whole tree on
/// every call, so a rail count that needed a filter would pay a per-frame clone to render a
/// number. `gaps` is the gap map's LENGTH (it holds an entry only for a series that has one) and
/// the rest are already-owned slice lengths.
///
/// There is deliberately **no stale count**: staleness is a predicate over the tree-wide span, so
/// counting it means filtering, and the Stale destination is where that cost is already being paid.
#[derive(Debug, Clone, Copy, Default)]
pub struct DataManagerRailCounts {
    /// Total stored series across every venue.
    pub series: usize,
    /// Series with at least one known missing day. Not `DataManagerInventoryGapMap::len()`: an entry with an empty list
    /// is a probe that failed.
    pub gaps: usize,
    /// Series whose gap probe failed, so that they are neither gapped nor gap-free
    /// (`vike_data_manager::gaps_unknown`).
    pub gaps_unknown: usize,
    /// The gap count is still being counted (`DataManagerInventoryContext::gaps_counting`): the rail draws none.
    pub gaps_counting: bool,
    /// Live cached bar feeds.
    pub feeds: usize,
    /// Saved symbol sets (symbol universes).
    pub symbol_sets: usize,
    /// Venues holding at least one stored series.
    pub venues: usize,
}

impl DataManagerRailCounts {
    /// Build from the render context. Pure, and every read is a length or a walk of the gap map, which
    /// holds only the series with something to say.
    pub fn from_context(context: &WindowToolContext<'_>) -> Self {
        let gaps_unknown = vike_data_manager::gaps_unknown(context.stored.gaps);
        Self {
            series: context.stored.tree.iter().map(|v| v.total.series).sum(),
            gaps: context.stored.gaps.len() - gaps_unknown,
            gaps_unknown,
            gaps_counting: context.stored.gaps_counting(),
            feeds: context.feeds.len(),
            symbol_sets: context.symbol_set_store.sets.len(),
            venues: context.stored.tree.iter().filter(|v| v.total.series > 0).count(),
        }
    }

    /// The count the rail draws beside one destination, or `None` where it draws none — a
    /// destination whose number would be noise, or whose own screen is where the number lives.
    ///
    /// ⚠ `None` on Has gaps for a gap-free store is a KNOWN zero left undrawn, not an unknown count.
    /// An empty good state reads better without a `0` the eye has to dismiss, which is why this has
    /// always hidden it. The kit rail draws nothing for `None` either way.
    fn count(&self, destination: DataManagerDestination) -> Option<u64> {
        let n = |v: usize| Some(v as u64);
        match destination {
            DataManagerDestination::AllSeries => n(self.series),
            // Not known while it is counted: a count that grows as probes answer reads as final.
            DataManagerDestination::HasGaps => {
                (self.gaps > 0 && !self.gaps_counting).then_some(self.gaps as u64)
            }
            DataManagerDestination::CachedFeeds => n(self.feeds),
            DataManagerDestination::SymbolSets => n(self.symbol_sets),
            DataManagerDestination::ByVenue => n(self.venues),
            // Stale: see `DataManagerRailCounts`' doc — counting it costs a tree clone per frame.
            // Overview: its own tiles ARE the counts; a count would say one of them twice.
            // Instruments: its own count is the CACHE's, which the rail does not hold — and a
            // count sourced from anywhere else would be a second number about the same list.
            // Gateway/Backend: neither has a natural single count — a per-venue presence grid
            // and a single box's connection state, same reasoning as Store.
            DataManagerDestination::Overview
            | DataManagerDestination::Stale
            | DataManagerDestination::Providers
            | DataManagerDestination::ActivityLog
            | DataManagerDestination::Gateway
            | DataManagerDestination::Backend
            | DataManagerDestination::Instruments
            | DataManagerDestination::Store => None,
        }
    }
}

/// Paint the rail and return whether the selection changed this frame.
///
/// Group headings are emitted whenever `DataManagerDestination::group` changes between consecutive entries of
/// [`DataManagerDestination::ALL`], so the grouping has exactly one source.
pub fn rail(
    ui: &mut egui::Ui,
    destination: &mut DataManagerDestination,
    counts: &DataManagerRailCounts,
) -> bool {
    // The kit's navigation rail took its shape FROM this rail (owner decision 2): group headings,
    // an icon, a label, a mono count, the 2 px accent edge, and the selected row reported as a
    // Label. What stays here is what is the Data Manager's own — which rows, in which groups.
    let items = DataManagerDestination::ALL.map(|d| ThemeRailItem {
        value: d,
        group: d.group().label(),
        icon: d.icon(),
        label: d.label(),
        count: counts.count(d),
    });
    // No footer under the rail: the "NO CEILING" note that stood here was design commentary
    // (how many tabs the window once had), not text for the operator, and the owner saw it.
    navigation_rail(ui, destination, &items)
}

// ⚠ `footline_reservation` used to live here and is DELETED. It measured the footline's height so
// the rail/body row could be laid out short by it — and the footline was STILL invisible, because
// subtracting from `ui.available_height()` reserves nothing egui honours: it only makes the row
// shorter and leaves the remainder to whatever draws next, inside a region that has already been
// sized. `egui::Panel::bottom`, declared before the row, is what actually reserves.

/// The window's foot status line — what the last refresh produced, and what is outstanding.
pub(crate) fn footline(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    counts: &DataManagerRailCounts,
) {
    use egui::{Align, Layout, RichText};
    let t = ThemeTokens::of(ui.ctx());
    let caption = |s: String| RichText::new(s).font(t.font(ThemeTextRole::Caption));
    // The FILL and the border come from the bottom PANEL this is shown inside
    // (`crates/vike-app-core/src/ui/windows/data_manager/mod.rs`'s `data_tool_content`), not from here — one
    // frame, owned by the thing that reserves the space.
    //
    // ⚠ A raised fill rather than a hairline and some dim text, because the first cut drew exactly
    // that at the window's bottom edge on a near-black ground and it could not be seen in the
    // capture — which read as "the layout is wrong" while the layout was also wrong, and cost a
    // whole render to tell the two apart.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::XL;
        ui.label(caption(format!("{} series indexed", counts.series)).color(t.theme.text_tertiary));
        let pending = context.stored.progress.gaps_pending.len();
        if pending > 0 {
            let words = format!("\u{2022} checking {pending} series for gaps\u{2026}");
            ui.label(caption(words).color(t.theme.text_tertiary));
        }
        if counts.gaps > 0 {
            // The bullet separates this item from the one before it; the has-gaps icon leads the
            // item itself, a space's width after the bullet rather than the row's item gap.
            let warn = ThemeStatus::Warning.color();
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::SM;
                ui.label(caption("\u{2022}".to_string()).color(warn));
                let words = caption(format!("{} with gaps", counts.gaps)).color(warn);
                ui.label(icons::HAS_GAPS.before(ui.style(), words));
            });
        }
        if counts.gaps_unknown > 0 {
            let words = format!("\u{2022} {} not checked for gaps", counts.gaps_unknown);
            ui.label(caption(words).color(ThemeStatus::Warning.color()))
                .on_hover_text(vike_data_manager::GAPS_NOT_CHECKED);
        }
        if let Some(note) = context.stored.partials_note {
            ui.label(
                caption("\u{2022} partial column unavailable".to_string())
                    .color(t.theme.text_tertiary),
            )
            .on_hover_text(note);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(caption(format!("{} live feeds", counts.feeds)).color(t.theme.text_tertiary));
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
/// `crates/vike-app-core/src/ui/windows/backend/mod.rs`'s `strip_row` makes for the live
/// connection.
///
/// WHICH store is mounted is read off the resolved datahub address (`WindowToolContext::execution_history`'s
/// `datahub_address`, the value the shell's `stored_mode` decides the grid from), and the read-only
/// verdict off `DataManagerInventoryContext::delete_unavailable`, which the shell already sets for exactly that
/// reason — never from a second probe that could disagree with either.
pub fn store_bar(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    counts: &DataManagerRailCounts,
) {
    let t = ThemeTokens::of(ui.ctx());
    let (rows, bytes) = context
        .stored
        .tree
        .iter()
        .fold((0_u64, 0_u64), |(r, b), v| (r + v.total.rows, b + v.total.bytes));
    let remote = context.execution_history.datahub_address.is_some();
    let caption = |s: String| {
        egui::RichText::new(s).font(t.font(ThemeTextRole::Caption)).color(t.theme.text_tertiary)
    };

    // ⚠ The kit's context bar spans the available width itself. A bar that shrink-wraps its content
    // renders as a narrow pill in the top-left corner — which is what the first GPU capture of this
    // window showed. A store identity that looks like a chip reads as one more control; spanning
    // the width is what makes it the window's header.
    section::context_bar(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(t.metrics.gap, space::SM);

            // The status dot, then the PICKER — a bordered control, not a label. The dot is a
            // STATUS, never the accent (spec §2): a remote, read-only store is Warning, and with no
            // datahub resolved nothing is mounted (this build has no local store engine) and it is
            // Muted.
            chip::status_dot(
                ui,
                if remote { ThemeStatus::Warning } else { ThemeStatus::Muted },
                "",
            );
            // ⚠ The picker names the store's KIND and nothing else — no box name, no address, no
            // path. Two reasons, and the first is a hard gate:
            //
            // `crates/vike-ops/tests/hygiene/shipped_box_name_gate.rs` forbids a box name in a string
            // LITERAL, because `scripts/refuse_box_paths.sh` greps the raw bytes of every published
            // binary and publishes NOTHING on a hit — at TAG time, which cannot be re-run, only
            // re-cut. A first cut of this bar hardcoded a box name here and the gate caught it,
            // which is the cheap place to find out.
            //
            // The second is that the address was INVENTED. Neither the store root nor the datahub
            // address is threaded into `WindowToolContext` yet, so a concrete path here would be a
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
            let name = if remote { "remote datahub" } else { "no datahub" };
            ui.add(ThemeButtonAction::secondary(name).disabled_because(PICKER_WHY));

            // The readouts — series / used / capability: dim, and in the sentence face (a number
            // inside a phrase). The capability is a word, so it is never the accent (spec §2).
            let access = if remote { "read \u{00B7} list" } else { "nothing mounted" };
            ui.label(caption(format!("{} series", counts.series)));
            ui.label(caption(format!(
                "{} rows \u{00B7} {}",
                vike_ui_theme::fmt::format_count_compact(rows),
                vike_utility::bytes::iec_bytes(bytes)
            )));
            ui.label(caption(access.to_string()));

            if context.stored.delete_unavailable.is_some() {
                chip::badge(ui, "read-only", ThemeStatus::Warning).on_hover_text(READ_ONLY_WHY);
            }
            if context.stored.loading {
                ui.label(caption("loading\u{2026}".to_string()));
            }

            // The OTHER mount, right-aligned: the archive. It listed the other store KIND beside it
            // (a "local store" under a remote mount, and the reverse) until 2026-11-12; this build
            // has no local store engine, and the picker already names the datahub mounted or not.
            //
            // ⚠ DISABLED, for the third reason argued at the picker above — and they were the same
            // dead control after the picker was fixed alone: live `Button`s whose `Response` was
            // dropped with `let _ =`, hovering "not built yet" over a click that switched nothing.
            // The hover now gives the true reason instead of promising a switch.
            // `crates/vike-app-core/tests/screens/store_bar_screen.rs` holds both halves.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Kind-only here too, for the reason argued at the picker above.
                ui.add(
                    ThemeButtonAction::secondary("\u{25CB} archive")
                        .disabled_because(OTHER_MOUNT_WHY),
                );
            });
        });
    });
}

/// The store picker's disabled reason. `crates/vike-app-core/tests/screens/store_bar_screen.rs` reads it
/// back off the tree.
const PICKER_WHY: &str = "Which store every destination below is reading. It is resolved once at \
                          startup and cannot be switched from here.";
/// The store bar's `read-only` badge, and why an ADMIN sees it too: it is not the device's role but
/// this window's design. The desktop sends no delete (`crate::data::data_manager::mode::DELETE_LOCAL_ONLY`)
/// and dials its Backfill without a key on purpose (`crate::data::backfill::run_datahub_backfill`'s
/// note), so a keyed datahub refuses it whatever the role.
const READ_ONLY_WHY: &str = "Read-only by design, whatever this device's role on the server (admin \
     included): this window cannot delete stored series, and its Backfill is not signed, so a datahub \
     that asks for keys refuses it. Fetch or remove stored series on the server with `vike-cli data \
     history fetch` or `vike-cli data history rm`.";
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
        let mut seen: Vec<DataManagerDestination> = Vec::new();
        for d in DataManagerDestination::ALL {
            assert!(!seen.contains(&d), "{d:?} listed twice in DataManagerDestination::ALL");
            seen.push(d);
        }
        assert_eq!(seen.len(), DataManagerDestination::ALL.len());
    }

    /// `DataManagerDestination::ALL` must be sorted BY GROUP, because [`rail`] emits a heading whenever the group
    /// changes between consecutive entries — an interleaved order would print a group twice.
    #[test]
    fn all_is_contiguous_by_group() {
        let mut runs: Vec<DataManagerRailGroup> = Vec::new();
        for d in DataManagerDestination::ALL {
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

    /// Exactly two destinations pay for the credential-store reads (`workspace_credentials`,
    /// `workspace_account_grids`) and the `catalog.set_credentialed_venues` push built from them —
    /// see [`DataManagerDestination::reads_credentials`]'s doc for each one's real dependency. The shell gates the
    /// reads and the push on this, so a third `true` here would put a credential-store open (and
    /// its `tracing::error!` on an unreadable store) behind a destination that reads neither the
    /// `grids` nor the catalog's credentialed set.
    #[test]
    fn only_gateway_and_instruments_read_the_credential_store() {
        let reads: Vec<DataManagerDestination> =
            DataManagerDestination::ALL.into_iter().filter(|d| d.reads_credentials()).collect();
        assert_eq!(
            reads,
            vec![DataManagerDestination::Gateway, DataManagerDestination::Instruments]
        );
    }

    /// The default destination is a Browse one — the window opens on what is stored, not on an
    /// editor. (It opened on the DataSet editor before the rail, because that was index 0.)
    #[test]
    fn the_default_destination_is_browse() {
        assert_eq!(DataManagerDestination::default().group(), DataManagerRailGroup::Browse);
    }

    /// No rail count may need a tree filter to produce — see [`DataManagerRailCounts`]' doc for why.
    #[test]
    fn stale_carries_no_count() {
        let c = DataManagerRailCounts {
            series: 430,
            gaps: 7,
            feeds: 14,
            symbol_sets: 6,
            venues: 6,
            ..Default::default()
        };
        assert_eq!(
            c.count(DataManagerDestination::Stale),
            None,
            "a stale count costs a per-frame tree clone"
        );
        assert_eq!(c.count(DataManagerDestination::AllSeries), Some(430));
        assert_eq!(c.count(DataManagerDestination::HasGaps), Some(7));
        let counting = DataManagerRailCounts { gaps_counting: true, ..c };
        assert_eq!(
            counting.count(DataManagerDestination::HasGaps),
            None,
            "not known until counted"
        );
        assert_eq!(
            counting.count(DataManagerDestination::AllSeries),
            Some(430),
            "the list is known"
        );
    }

    /// A gap-free store draws no has-gaps count at all, rather than a `0` the eye has to dismiss.
    #[test]
    fn a_gap_free_store_draws_no_gap_count() {
        let c = DataManagerRailCounts {
            series: 430,
            feeds: 14,
            symbol_sets: 6,
            venues: 6,
            ..Default::default()
        };
        assert_eq!(c.count(DataManagerDestination::HasGaps), None);
    }

    /// A variant's name as its row's key spells it: `SymbolSets` is `SYMBOL_SETS`.
    fn upper_snake(name: &str) -> String {
        let mut key = String::new();
        for (i, c) in name.chars().enumerate() {
            if i > 0 && c.is_ascii_uppercase() {
                key.push('_');
            }
            key.push(c.to_ascii_uppercase());
        }
        key
    }

    /// The label and the glyph each destination wears, pinned to what `DataManagerDestination::label` and
    /// `DataManagerDestination::icon` answered before they moved to `ui-theme.toml`'s `data_destination` map. Changing a
    /// row of the table changes the rail, and this test is what says so by name; it also holds that
    /// every destination is listed, so a new one is not left out of the pin.
    #[test]
    fn a_destination_wears_the_label_and_the_glyph_the_data_destination_map_gives_it() {
        let pinned = [
            (DataManagerDestination::Overview, "Overview", icons::OVERVIEW),
            (DataManagerDestination::AllSeries, "All series", icons::ALL_SERIES),
            (DataManagerDestination::HasGaps, "Has gaps", icons::HAS_GAPS),
            (DataManagerDestination::Stale, "Stale", icons::STALE),
            (DataManagerDestination::ByVenue, "By venue", icons::BY_VENUE),
            (DataManagerDestination::CachedFeeds, "Cached feeds", icons::CACHED_FEEDS),
            (DataManagerDestination::Providers, "Providers", icons::PROVIDERS),
            (DataManagerDestination::ActivityLog, "Activity log", icons::ACTIVITY_LOG),
            (DataManagerDestination::SymbolSets, "Symbol sets", icons::DATASETS),
            (DataManagerDestination::Gateway, "Gateway", icons::GATEWAY),
            (DataManagerDestination::Backend, "Backend", icons::BACKEND),
            (DataManagerDestination::Instruments, "Instruments", icons::INSTRUMENTS),
            (DataManagerDestination::Store, "Store", icons::STORE),
        ];
        assert_eq!(
            pinned.len(),
            DataManagerDestination::ALL.len(),
            "a destination is missing from the pin"
        );
        for (destination, label, glyph) in pinned {
            assert_eq!(destination.label(), label, "{destination:?}: the label");
            assert_eq!(destination.icon(), glyph, "{destination:?}: the glyph");
        }
    }

    /// The map is the rail's own: every destination reads the row of its own name, no two share one,
    /// no row of the `data_destination` map is left without a destination, and every row carries the label
    /// and the glyph `label` and `icon` read from it.
    #[test]
    fn every_destination_has_its_own_data_destination_row_and_every_row_its_destination() {
        for d in DataManagerDestination::ALL {
            assert_eq!(d.row().key, upper_snake(&format!("{d:?}")), "{d:?} reads another row");
            assert_eq!(
                DataManagerDestination::ALL.iter().filter(|o| o.row() == d.row()).count(),
                1,
                "{d:?}'s row is shared"
            );
        }
        for row in maps::data_destination::ALL {
            assert!(
                DataManagerDestination::ALL.iter().any(|d| d.row() == *row),
                "{} is no destination",
                row.key
            );
            assert!(row.word.is_some_and(|w| !w.is_empty()), "{}: no label", row.key);
            assert!(row.icon().is_some(), "{}: no glyph of the registry", row.key);
        }
    }

    /// The headings the rail prints over its groups, pinned to what `DataManagerRailGroup::label` answered before
    /// it moved to `ui-theme.toml`'s `rail_group` map; and the map is the rail's own — every group
    /// reads the row of its own name, no two share one, and no row is left without a group.
    #[test]
    fn every_group_has_its_own_rail_group_row_and_every_row_its_group() {
        let all = [
            (DataManagerRailGroup::Browse, "BROWSE"),
            (DataManagerRailGroup::Live, "LIVE"),
            (DataManagerRailGroup::Configure, "CONFIGURE"),
        ];
        for (g, heading) in all {
            assert_eq!(g.label(), heading, "{g:?}: the heading");
            assert_eq!(g.row().key, upper_snake(&format!("{g:?}")), "{g:?} reads another row");
            assert_eq!(all.iter().filter(|(o, _)| o.row() == g.row()).count(), 1, "{g:?}'s row");
        }
        for row in maps::rail_group::ALL {
            assert!(all.iter().any(|(g, _)| g.row() == *row), "{} is no rail group", row.key);
        }
    }
}
