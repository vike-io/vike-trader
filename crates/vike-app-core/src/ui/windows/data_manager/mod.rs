//! The Data-manager tool body — the Symbols (DataSet) editor, the Cached-Series catalog of live
//! feeds, the provider sub-tabs, and the Stored sub-tab (which delegates to
//! [`stored_tool_content`]). Moved verbatim from `vike-app`'s `main.rs` (tool-view
//! extraction batch 2); the adaptations are mechanical: the seven threaded read-only params now
//! arrive grouped in [`WindowToolContext`], and the table closures that were pure arithmetic became named
//! functions so they finally get unit tests — `fmt_series_dt`, and `feed_cell`, the Cached-feeds
//! table's cell text (its column layout is the kit's data table now).
//!
//! `chart::format_thousands` is spelled `vike_ui_theme::fmt::format_thousands` here — the SAME function
//! (vike-chart re-exports it from vike-ui-theme), reached directly since this crate already
//! depends on the leaf.
//!
//! # The action strips
//!
//! Five destinations open with an ACTION STRIP — the actions that make sense on what is below it
//! and, on the four that have more than one cut of their data, a segmented control saying which
//! cut is on screen. (Cached feeds is the fifth: one cut, so a strip of verbs and no segments.)
//! The strips live here rather than in [`rail`] because they are BODY furniture: the
//! rail owns which destination is showing, a strip owns what that destination is showing OF.
//!
//! ⚠ **Every strip sits ABOVE the shared grid and none of them reaches into it.**
//! `vike_data_manager::inventory_grid` is mounted by vike-studio as well, against a panel
//! `crates/vike-studio/src/panes/data_browser.rs` records as ~340px wide while the grid's fixed columns
//! already sum to ~690px — so a column added for this window clips that one, and no test covers
//! Studio's Data tab. Anything a strip wants that the grid cannot give it is either derived beside
//! the grid (the Stale sort writes `DataManagerInventoryGridState::sort`, which is already a `pub` field) or rendered
//! as this module's own list instead (the cross-kind partial-day view).
//!
//! ⚠ **The strips' own state lives in egui's temp memory**, keyed off the body `Ui`'s id, and NOT
//! on [`WindowView`]. A `WindowView` field is the natural home for per-window view state —
//! `stored_grid`, `data_selection` and the `ds_*` family all are — and this is a deliberate second-best:
//! the in-tree precedent is `vike_data_manager::inventory_list`, whose `DataManagerInventorySort` rides the
//! same map, and `crates/vike-app-core/src/ui/windows/chart_study_picker.rs`'s `chart_study_picker_popup` query box.
//! What it costs is that a filter resets to its default when egui evicts the entry, which is the
//! right trade for a filter and would be the wrong one for anything an operator would have to
//! re-enter.
//!
//! # Overview, By venue and Store: the three NEW destinations
//!
//! The rail's other five re-house content that already existed. These three did not exist in any
//! form, and they are Phase 2 of the approved design.
//!
//! # ⚠ Every number here is DERIVED from what the window already loaded
//!
//! Not one of these screens fetches anything. `DataManagerInventoryContext` already carries the venue tree, the
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
//! `crates/vike-app-core/src/ui/windows/data_manager/rail.rs`'s `DataManagerDestination::reads_credentials` keeps off
//! it. So there is no tile.
//!
//! # ⚠ Every CONTROL here is either REAL or visibly dead — never a live button that does nothing
//!
//! These screens carry actions as well as numbers now, and the rule they follow is the one
//! `crates/vike-app-core/src/ui/windows/data_manager/mod.rs`'s `DataManagerDestination::CachedFeeds` arm already follows: an
//! action with no backend is drawn through the kit's `ThemeButtonAction::disabled_because`, which is the
//! only way to disable a kit button and takes the reason — naming WHY on hover — rather than a
//! live-looking button that swallows the click.
//! The old Cached Series toolbar shipped TEN live-looking buttons with nothing behind them, and
//! that is the specific failure this window was redesigned to remove — a decorative button added
//! here would be the redesign undoing itself on the screens that replaced it.
//!
//! What is REAL: Overview's Refresh and By venue's (the SAME `WindowView::stored_refresh`
//! out-flag that `crates/vike-app-core/src/ui/windows/data_manager/inventory/mod.rs`'s `stored_tool_content` sets, so
//! no two Refresh buttons in this window can drift into meaning different things); Overview's
//! per-row `Backfill` (the SAME `WindowView::stored_backfill` OUT vector the stored grid's
//! bulk Backfill extends — one drain, one planner, one status line, and this screen renders that
//! line rather than letting a queued run look like a click that did nothing); Overview's `GO TO`
//! strip and per-row jumps; By venue's `DataManagerVenueRollupScope` filter (a pure fold — `venue_rows` — over
//! the tree this module already holds) and its per-row `Keys` (pure navigation to
//! `DataManagerDestination::Gateway`, the screen that holds the credential editor); Gateway's own
//! Refresh (reads the credential store again, `DataManagerCredentialReads`). What is DEAD, each
//! carrying its own reason: `STORE_ACTIONS`, and the single row action on Overview that names a finding the
//! backfill planner cannot be keyed from (`PARTIAL_NO_KEY`).

mod activity_log;
mod cached_feeds;
mod gaps;
pub mod instruments;
mod inventory;
mod overview;
mod providers;
pub mod rail;
mod store;
mod symbol_sets;
mod venue_rollup;

pub use inventory::{save_polymarket_proxy, seed_polymarket_proxy_box, stored_tool_content};

use super::WindowToolContext;
use super::backend::settings::{
    BackendSettingsEditState, BackendSettingsState, BackendSettingsWriteRequest,
};
use super::{BackendPickerSources, backend_tab, strip_row, venue_label};
use crate::backend::connection::BackendAction;
use crate::backend::editor::{self, BackendEditorState};
use crate::ui::windows::WindowView;
use activity_log::data_activity_log;
use cached_feeds::data_cached_feeds;
use gaps::{data_has_gaps, data_stale};
use providers::data_providers;
use rail::DataManagerDestination;
use std::collections::HashMap;
use std::sync::Arc;
use symbol_sets::data_symbol_sets;
use vike_account_manager::AccountManagerGrids;
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::{ThemeTokens, section};
use vike_ui_theme::icons::{self, ThemeIcon};
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::data;

#[cfg(test)]
use activity_log::{DataManagerActivityLogPlane, LOG_SEGMENTS, log_plane};
#[cfg(any(test, doc))]
use cached_feeds::{feed_cell, fmt_series_dt};
#[cfg(test)]
use gaps::DataManagerGapCut;
#[cfg(doc)]
use gaps::partial_days_list;
#[cfg(test)]
use gaps::{
    GAP_SEGMENTS, STALE_SORTS, backfillable, gapped_series_keys, span_label, stale_series_keys,
};
#[cfg(test)]
use providers::{DataManagerProviderPlane, PROVIDER_SEGMENTS, fetch_dot};
#[cfg(test)]
use symbol_sets::members::{
    DataManagerSymbolSetMemberCut, MEMBER_SEGMENTS, member_cell, member_is_stale, member_keys,
    member_series,
};
#[cfg(test)]
use symbol_sets::{suggest_symbols, symbol_set_groups, symbol_set_load_form};
#[cfg(test)]
use vike_data::datasets::provider_names_a_venue;

/// **The Data window's credential-store reads, held between frames** — the credential map and the
/// account grid (`crates/vike-desktop/src/main.rs`'s `workspace_credentials` and
/// `workspace_account_grids`) and, on a fresh read only, the `catalog.set_credentialed_venues` push
/// the shell builds from them. Which destination reads the store is this module's fact
/// ([`DataManagerDestination::reads_credentials`]), so the decision lives here, in the CI-tested
/// crate, and the shell only supplies the read.
///
/// Both reads open the settings database and read every credential, and a destination that renders
/// them used to pay that on EVERY frame it was on screen. Now the store is read when such a
/// destination OPENS, and again only after this app wrote a credential (`writes`, the editor's
/// `vike_account_manager::credential_writes`) or on the Gateway screen's Refresh; leaving the
/// destination drops what was held, so the next visit reads afresh. A change made OUTSIDE this app
/// (`vike-cli`, another process) shows on the next visit or Refresh, not by itself.
///
/// ⚠ What is held is the read's own answer, its verdict included: a store that exists and cannot
/// be read stays an UNREADABLE grid (and the shell's read logs its `tracing::error!` each time it
/// runs), never an empty "no credentials". The values never leave memory through here: no `Debug`.
#[derive(Clone, Default)]
pub struct DataManagerCredentialReads {
    /// The last read, and the editor's write count it was taken at.
    held: Option<(Arc<DataManagerCredentialRead>, u64)>,
    /// Raised by the Gateway screen's Refresh: the next frame reads again.
    pub(crate) refresh: bool,
    /// Whether the last frame was on the Backend page, so arriving there is seen once.
    on_backend: bool,
}

/// One read of the credential store, as [`data_tool_content`] takes it.
pub struct DataManagerCredentialRead {
    /// The account grid, carrying the verdict of its own reads (`workspace_account_grids`).
    pub grids: AccountManagerGrids,
}

impl DataManagerCredentialReads {
    /// This frame's read for `destination`: the held one, taken through `read` first when nothing is
    /// held, a Refresh asked or `writes` moved since; the `bool` says whether `read` ran. A
    /// destination that renders no credential reads nothing, drops what was held and gets the
    /// UNREAD grid, whose every cell is unknown rather than a measured absence.
    ///
    /// Arriving at the Backend page re-reads this PC's device key (one row,
    /// `crate::backend::registry::fresh_device_key`), so a key made in a terminal meanwhile
    /// (`vike-cli backend mint-missing`) shows there without a restart.
    pub fn read(
        &mut self,
        destination: DataManagerDestination,
        writes: u64,
        read: impl FnOnce() -> DataManagerCredentialRead,
    ) -> (Arc<DataManagerCredentialRead>, bool) {
        let on_backend = destination == DataManagerDestination::Backend;
        if on_backend && !self.on_backend {
            crate::backend::registry::fresh_device_key();
        }
        self.on_backend = on_backend;
        if !destination.reads_credentials() {
            self.held = None;
            let unread = DataManagerCredentialRead { grids: AccountManagerGrids::unread() };
            return (Arc::new(unread), false);
        }
        let refresh = std::mem::take(&mut self.refresh);
        if let Some((held, at)) = &self.held
            && *at == writes
            && !refresh
        {
            return (Arc::clone(held), false);
        }
        let fresh = Arc::new(read());
        self.held = Some((Arc::clone(&fresh), writes));
        (fresh, true)
    }
}

/// The Data-manager tool body: the Cached-Series catalog + the DataSet Symbols editor + the
/// provider sub-tabs. [`DataManagerDestination::AllSeries`] and its two filtered siblings hand off to
/// [`stored_tool_content`], which reads the same [`WindowToolContext`].
///
/// ⚠ **"Its two filtered siblings" is no longer the whole truth**, and the exception is the one
/// worth knowing: [`DataManagerDestination::HasGaps`] hands off only in its per-series mode. Its cross-kind cut
/// is rendered by `partial_days_list` instead, because the partial-day map is keyed one level
/// ABOVE a grid row and the shared grid has no view for it — that function's doc is the argument.
#[expect(clippy::too_many_arguments)] // the Gateway/Backend destinations' inputs, carried
// through from the shell the same way the old standalone Connections window received them — see
// `data_body`'s doc for why these nine are not reachable from `tv`/`context` alone.
pub fn data_tool_content(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    tv: &mut WindowView,
    grids: &vike_account_manager::AccountManagerGrids,
    picker: &BackendPickerSources<'_>,
    action: &mut Option<BackendAction>,
    editor: &mut BackendEditorState,
    registry: &mut Option<editor::BackendRegistryUpdate>,
    settings: &BackendSettingsState,
    settings_refresh: &mut bool,
    settings_edit: &mut BackendSettingsEditState,
    settings_write: &mut Option<BackendSettingsWriteRequest>,
) {
    use egui::{Align, Layout, vec2};
    let t = ThemeTokens::of(ui.ctx());
    ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);

    // The store every destination below is reading, stated once, above all of them. It belongs to
    // no destination — the same argument `backend/mod.rs`'s `strip_row` makes for the live
    // connection — and until it existed the answer was discoverable only by noticing that Delete
    // had greyed out.
    let counts = rail::DataManagerRailCounts::from_context(context);
    rail::store_bar(ui, context, &counts);
    ui.add_space(t.metrics.gap);

    // ⚠ The footline is a BOTTOM PANEL, declared BEFORE the row below it, and that ordering is the
    // whole of why it reaches the screen.
    //
    // Two earlier attempts did not. The first appended it after the rail/body row, which claims
    // `ui.available_height()` — all of it — so the strip landed below the window's own floor;
    // `backend/mod.rs`'s `strip_row` doc carries that identical defect's history. The second subtracted
    // a MEASURED reservation from the row's height, and it was still invisible: arithmetic against
    // `available_height()` does not reserve anything egui will honour, it just makes the row
    // shorter and leaves the leftover to whatever draws next. A panel reserves. It is also what
    // `crates/vike-app-core/src/ui/window/state/mod.rs` already says about the app's own status bar —
    // added before the central panel, for this reason.
    egui::Panel::bottom("data_manager_footline")
        .frame(
            egui::Frame::new()
                .fill(t.theme.surface)
                .stroke(egui::Stroke::new(stroke::HAIRLINE, t.theme.border))
                .inner_margin(egui::Margin::symmetric(t.metrics.padding as i8, space::SM as i8)),
        )
        .show(ui, |ui| rail::footline(ui, context, &counts));

    let full = ui.available_width();
    let rail_w = data::RAIL_WIDTH.min(full * data::RAIL_MAX_FRACTION);
    let body_h = ui.available_height();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(vec2(rail_w, body_h), Layout::top_down(Align::Min), |ui| {
            ui.spacing_mut().item_spacing.y = space::HAIR;
            // Scrolls: the rail (thirteen rows) is taller than a short window,
            // and unscrolled its last lines were painted over the footline below.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .id_salt("data_manager_rail")
                .show(ui, |ui| rail::rail(ui, &mut tv.data_destination, &counts));
        });
        ui.separator();
        // ⚠ The body is allocated the REMAINING width explicitly rather than letting egui infer it.
        // The shared grid's fixed columns sum to ~738px with the checkbox and Partial cells
        // (`crates/vike-studio/src/panes/data_browser.rs` records the same number from the other mount),
        // so a body that silently inherits a narrow width clips cells instead of shrinking them. The
        // width is what the row has LEFT after the rail, the separator and the item spacing after
        // each: a reserve written down beside them was short by that spacing (4 points at the
        // default density) and the window's body scrolled sideways.
        ui.allocate_ui_with_layout(
            vec2(ui.available_width().max(data::BODY_MIN_WIDTH), body_h),
            Layout::top_down(Align::Min),
            |ui| {
                ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);
                section::breadcrumb(
                    ui,
                    &["Data Manager", tv.data_destination.label()],
                    &destination_summary(context, tv.data_destination, &counts),
                );
                data_body(
                    ui,
                    context,
                    tv,
                    grids,
                    picker,
                    action,
                    editor,
                    registry,
                    settings,
                    settings_refresh,
                    settings_edit,
                    settings_write,
                );
            },
        );
    });
}

/// The right-aligned half of a destination's breadcrumb — what THIS screen holds, in its own terms.
///
/// Per-destination rather than one global total: the crumb's job is to say what you are looking at,
/// and "430 series" over the Stale screen would name the tree rather than the 23 rows on it.
fn destination_summary(
    context: &WindowToolContext<'_>,
    destination: DataManagerDestination,
    counts: &rail::DataManagerRailCounts,
) -> String {
    match destination {
        DataManagerDestination::AllSeries => {
            let (rows, bytes) = context
                .stored
                .tree
                .iter()
                .fold((0_u64, 0_u64), |(r, b), v| (r + v.total.rows, b + v.total.bytes));
            format!(
                "{} series · {} rows · {}",
                counts.series,
                vike_ui_theme::fmt::format_count_compact(rows),
                vike_utility::bytes::iec_bytes(bytes)
            )
        }
        DataManagerDestination::Overview => {
            "what needs attention, before the filing cabinet".to_string()
        }
        DataManagerDestination::ByVenue => format!("{} venues hold series", counts.venues),
        DataManagerDestination::Store => "what is mounted, and what each mount can do".to_string(),
        DataManagerDestination::HasGaps if counts.gaps_counting => {
            format!("counting the series with gaps among {}\u{2026}", counts.series)
        }
        DataManagerDestination::HasGaps => format!(
            "{} of {} series carry a hole{}",
            counts.gaps,
            counts.series,
            gaps::not_checked_tail(counts.gaps_unknown)
        ),
        DataManagerDestination::Stale => "lagging more than 35% of the tree-wide span".to_string(),
        DataManagerDestination::CachedFeeds => format!("{} live · binance WebSocket", counts.feeds),
        DataManagerDestination::Providers => {
            "where history, events and live ticks come from".to_string()
        }
        DataManagerDestination::ActivityLog => "this session only — not persisted".to_string(),
        DataManagerDestination::SymbolSets => format!("{} saved symbol sets", counts.symbol_sets),
        DataManagerDestination::Gateway => "credential presence, per venue and account".to_string(),
        DataManagerDestination::Backend => "which box this app is observing".to_string(),
        DataManagerDestination::Instruments => instruments::instruments_summary(
            &context.catalog_refresh.rows(),
            context.catalog_refresh.total(),
        ),
    }
}

/// One destination's body. An exhaustive `match` — the point of [`DataManagerDestination`].
///
/// ⚠ The predecessor was an `if/else` chain comparing `tv.data_subtab` against five values, THREE of
/// them bare literals (`== 0`, `== 1`, `== 5`; `crate::ui::startup::DATA_SUBTAB_STORED` existed and was
/// not used at the Stored arm), ending in an `else` that indexed a `[&str; 7]` with the raw value —
/// so an out-of-range write panicked the window on render. Neither shape is expressible now.
///
/// Each destination whose body is more than a call or two is its own function below
/// ([`data_instruments`], [`data_cached_feeds`], [`data_activity_log`], [`data_symbol_sets`],
/// [`data_has_gaps`], [`data_stale`], [`data_providers`]); this function is the dispatcher, and the
/// arms that stayed here are the ones that only hand off. The frame's tokens are read once here and
/// passed down.
///
/// ⚠ **The ten params after `tv` are NOT reachable from `tv`/`context` alone**, unlike everything
/// else this function and [`data_tool_content`] read. `DataManagerDestination::Gateway`/`DataManagerDestination::Backend`
/// call `vike_account_manager::account_manager_ui`/[`backend_tab`]/[`strip_row`] directly — the same
/// functions the old standalone Connections window's own tool-entry point called — and those
/// functions need the credential-store read (`grids`) and the backend-picker/editor/
/// settings state (`picker`/`action`/`editor`/`registry`/`settings`/`settings_refresh`/
/// `settings_edit`/`settings_write`) that window held as its OWN per-call parameters, not as
/// fields on `WindowView` or [`WindowToolContext`]. ⚠ **This already compiles, end to end** —
/// `crates/vike-desktop/src/tool_window.rs`'s `WindowKind::Data` arm builds and supplies all nine values to
/// `data_tool_content` every frame (the same shell-side construction the old `Connections` arm
/// used to do, moved over when `WindowKind::Connections` was deleted) — this wiring is complete, not
/// owed to anything still pending.
#[expect(clippy::too_many_arguments)] // see the doc above — nine pass-through params, one tool body
fn data_body(
    ui: &mut egui::Ui,
    context: &WindowToolContext<'_>,
    tv: &mut WindowView,
    grids: &vike_account_manager::AccountManagerGrids,
    picker: &BackendPickerSources<'_>,
    action: &mut Option<BackendAction>,
    editor: &mut BackendEditorState,
    registry: &mut Option<editor::BackendRegistryUpdate>,
    settings: &BackendSettingsState,
    settings_refresh: &mut bool,
    settings_edit: &mut BackendSettingsEditState,
    settings_write: &mut Option<BackendSettingsWriteRequest>,
) {
    let t = ThemeTokens::of(ui.ctx());

    // ⚠ **TAB-INDEPENDENT in its own way.** The old standalone Connections window's own
    // tool-entry point ran this fetch trigger unconditionally whenever the window was open AT
    // ALL, regardless of which of its two tabs was showing — fixing a documented bug where
    // staying on the default tab left the settings slot `Idle` forever, because the trigger used
    // to be tab-gated before that fix: in that window, the status line's digested Backend half
    // AND the Backend segment's badge were BOTH drawn on both tabs, so a fetch that only started
    // once the Backend tab was actually visited left those two permanently blank on the tab an
    // operator was looking at. Here, "the Connections window is open" has no single equivalent:
    // Gateway and Backend are now two of thirteen destinations, not two always-coexisting
    // tabs of one window.
    //
    // ⚠ **The gate is `Gateway | Backend`, and this used to justify that as "both render
    // backend-settings state" — which stopped being true the day the standalone window's status
    // line and tab-badge chrome were deleted with it.** `BackendSettingsDigest`/`backend_settings_section`
    // render on EXACTLY ONE of these thirteen destinations now: `backend_settings_section` is
    // reached only through `backend_tab`, which only `DataManagerDestination::Backend`'s arm calls
    // (`backend/mod.rs`'s own module doc states this plainly). `DataManagerDestination::Gateway` renders no
    // backend-settings state of any kind. The gate still covers both destinations, but for the
    // PRE-WARMING reason the paragraph above gives, not because both render the answer: Gateway
    // and Backend are rail-adjacent siblings in the CONFIGURE group, so keeping the fetch warm
    // while either is on screen means a switch from one to the other never shows a cold `Idle`
    // slot. A per-frame settings fetch behind, say, the DataSets editor would still be the exact
    // defect `crates/vike-app-core/src/ui/windows/data_manager/rail.rs`'s `DataManagerDestination::reads_credentials`
    // doc warns against for the credential read, paid by a destination that renders no such
    // thing — this gate just does not extend that far.
    // Placed HERE (before the dispatch match, reading the CURRENT `tv.data_destination` the match below
    // is about to use — not a value from before the rail's own click handling ran) rather than in
    // `data_tool_content`, so the two stay impossible to disagree about which destination is
    // actually on screen this frame.
    if matches!(
        tv.data_destination,
        DataManagerDestination::Gateway | DataManagerDestination::Backend
    ) && super::should_fetch_settings(picker.active.is_some(), settings)
    {
        *settings_refresh = true;
    }

    // The ambient backend-connection row, drawn directly above EITHER destination, identically —
    // see `backend/mod.rs`'s `strip_row` doc for why it belongs to neither. Same `Gateway |
    // Backend` gate as the fetch trigger above, but for its OWN reason rather than the same one:
    // `strip_row` genuinely IS drawn on both (`backend/mod.rs`'s module doc says so outright),
    // unlike the digest the fetch trigger above warms.
    if matches!(
        tv.data_destination,
        DataManagerDestination::Gateway | DataManagerDestination::Backend
    ) {
        strip_row(ui, picker, action, &mut tv.data_destination, editor);
        section::strip_rule(ui);
    }

    match tv.data_destination {
        DataManagerDestination::Overview => {
            // The landing screen returns a destination when the operator clicks through, so the
            // "Go to" row and the per-row actions are real navigation rather than decoration.
            if let Some(d) = overview::overview(
                ui,
                context,
                tv,
                &rail::DataManagerRailCounts::from_context(context),
            ) {
                tv.data_destination = d;
            }
        }
        DataManagerDestination::ByVenue => {
            // The per-venue rollup returns a destination for the same reason Overview does: its Arm
            // button is NAVIGATION, not arming. This screen owns no ceiling, reads no credential and
            // writes no policy — it hands you to the one screen that does.
            if let Some(d) = venue_rollup::by_venue(ui, context, tv) {
                tv.data_destination = d;
            }
        }
        DataManagerDestination::Instruments => data_instruments(ui, context),
        DataManagerDestination::Store => store::store(ui, context),
        DataManagerDestination::CachedFeeds => data_cached_feeds(ui, context, tv, &t),
        DataManagerDestination::ActivityLog => data_activity_log(ui, context, tv, &t),
        DataManagerDestination::SymbolSets => data_symbol_sets(ui, context, tv, &t),
        DataManagerDestination::AllSeries => {
            // ===== The stored inventory, unfiltered — the filing cabinet =====
            //
            // ⚠ This arm used to be `AllSeries | HasGaps | Stale`, ONE render at three
            // `DataManagerInventoryFilter`s, and the comment on it said so approvingly. The design says they are
            // three SCREENS, and the difference is not cosmetic: a filter is a claim about which
            // rows you are looking at, while a screen also owns the actions that make sense on
            // those rows and the derivation that decides which rows they are. Stale needs to say
            // what its cut-off date is and offer to update everything past it; Has gaps needs a
            // second cut the grid cannot render at all. Neither belongs on a screen also serving
            // the other two, and neither fits in a rail label.
            //
            // The GRID is still one render, and that is the part worth keeping — see this
            // module's header for why nothing here reaches into it.
            tv.stored_grid.active_view = vike_data_manager::DataManagerInventoryFilter::All;
            stored_tool_content(ui, context, tv);
        }
        DataManagerDestination::HasGaps => data_has_gaps(ui, context, tv, &t),
        DataManagerDestination::Stale => data_stale(ui, context, tv, &t),
        DataManagerDestination::Gateway => {
            // ===== Gateway — credential presence and the credential EDITOR (from the old
            // Connections/Credentials tab, moved here 2026-10-05, named Gateway by decision 0164) =====
            //
            // This screen arms nothing: an account trades at its own `account.tier` while
            // `account.active = 1`, on the NODE's database, which this desktop does not write. The
            // per-venue arming grid that stood above the editor wrote this PC's LOCAL database,
            // which the daemon never reads, and was deleted with the venue ceiling.
            //
            // ⚠ **The OUTER `ScrollArea` is load-bearing, not decoration.** `data_tool_content`
            // allocates this screen's body as one FIXED-height region with no scroll area of its
            // own, and `account_manager_ui` (the account picker, the P/D/L rail and the detail pane
            // with its ✎ edit buttons) has none either: without this area a body taller than the
            // window lays out below the visible region, reachable by no click.
            egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("gateway_body").show(
                ui,
                |ui| {
                    // The store is read when this screen opens and after this app's own writes
                    // (`DataManagerCredentialReads`), so a change made elsewhere shows on a Refresh.
                    let refresh = ThemeButtonAction::secondary((icons::REFRESH, "Refresh"));
                    if ui.add(refresh).on_hover_text(CREDENTIALS_REFRESH_HOVER).clicked() {
                        tv.data_credentials.refresh = true;
                        ui.ctx().request_repaint();
                    }
                    // The grid is the shell's held read of the account rows and their
                    // credentials (`crates/vike-desktop/src/main.rs`'s `workspace_account_grids`),
                    // carrying the verdict of those reads; beside it the live feed-status map and
                    // the one-shot account preselection (`tv.gateway_account` is `take`n here,
                    // same contract as the standalone Connections window had).
                    let live: HashMap<String, vike_model::feed_status::DataClientConnectionState> =
                        context
                            .feed_statuses
                            .iter()
                            .map(|(venue, handle)| (venue.clone(), handle.lock().unwrap().state))
                            .collect();
                    let preselect = tv.gateway_account.take();
                    // The exact call the old Connections window's Credentials tab made.
                    vike_account_manager::account_manager_ui(
                        ui,
                        grids,
                        &live,
                        context.credentials,
                        preselect,
                    );
                },
            );
        }
        DataManagerDestination::Backend => {
            // The exact call the old Connections window's Backend tab made — `filter` is the same
            // `WindowView` field that tab read (`tv.connections_settings_filter`), per window.
            backend_tab(
                ui,
                &context.credentials,
                picker,
                action,
                editor,
                registry,
                settings,
                settings_refresh,
                settings_edit,
                settings_write,
                &mut tv.connections_settings_filter,
            );
        }
        DataManagerDestination::Providers => {
            // Scrolls: the Event plane's "Data providers" group (signer line, two key rows, a status
            // line) makes the body taller than the fixed-height region this screen is given, and
            // like the Gateway destination above it has no scroll area of its own to fall back on.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .id_salt("providers_body")
                .show(ui, |ui| {
                    // Leave the scroll bar its own strip: the long legend lines wrap at the body's
                    // width and ended UNDER the bar.
                    let bar = ui.spacing().scroll;
                    let reserve = bar.bar_width + bar.bar_inner_margin + bar.bar_outer_margin;
                    ui.set_max_width(ui.available_width() - reserve);
                    data_providers(ui, context, tv, &t, picker.active);
                });
        }
    }
}

/// The Instruments destination: the venue catalog's rows and the per-venue Refresh, which only
/// REQUESTS a fetch (the comment below says why it never fetches on the frame thread).
fn data_instruments(ui: &mut egui::Ui, context: &WindowToolContext<'_>) {
    // ⚠ The click NEVER fetches here. `instruments_screen` reports which venue was pressed
    // and `DatahubCatalogRefresh::request` spawns the venue REST call on its own thread, so the
    // frame returns while the socket is still open — the same shape as
    // `crates/vike-desktop/src/app_methods.rs`'s `spawn_backend_settings_fetch`. The result
    // lands in `rows()` on a later frame, woken by the repaint this passes in.
    let now = vike_model::now_ms();
    let rows = context.catalog_refresh.rows();
    if let Some(venue) =
        instruments::instruments_screen(ui, &rows, context.catalog_refresh.total(), now)
    {
        let ectx = ui.ctx().clone();
        // The refusal is already on screen (the button is disabled with its reason), so a
        // lost race between the disable and the click needs no second report.
        let _ = context.catalog_refresh.request(&venue, now, move || ectx.request_repaint());
    }
}

// ===== Strip furniture — the shapes every action strip is built from =====
//
// The segmented control and the button are the kit's (`vike_ui_theme::components`): this window's
// two button shapes and two segmented-control shapes are one component each now (spec §4.3). What
// stays here is how THIS window words them.

/// The kit button for a strip action: `label`, led by `icon` when it has one.
pub(super) fn action<'a>(icon: Option<ThemeIcon>, label: &'a str) -> ThemeButtonAction<'a> {
    match icon {
        Some(i) => ThemeButtonAction::secondary((i, label)),
        None => ThemeButtonAction::secondary(label),
    }
}

/// An action with nothing behind it, rendered the ONE honest way: disabled, with the reason on
/// hover (spec §4.2). A live button that silently does nothing is the failure this prevents — an
/// operator clicks it, nothing happens, and the window has told them neither that it refused nor
/// why. `why` is the whole payload, so it names the thing that does not exist rather than "not
/// implemented".
fn dead_action(ui: &mut egui::Ui, icon: Option<ThemeIcon>, label: &str, why: &str) {
    ui.add(action(icon, label).disabled_because(why));
}

/// A strip's trailing note — the design's `.legend`/`.lead`: the Caption role, dim. A refusal rule or
/// a derivation is stated here once instead of hiding in ten tooltips. The Stored window's inspector
/// (`inventory`) states every empty state and refusal rule in it too.
pub(super) fn legend(ui: &mut egui::Ui, text: &str) {
    let t = ThemeTokens::of(ui.ctx());
    ui.label(
        egui::RichText::new(text).font(t.font(ThemeTextRole::Caption)).color(t.theme.text_tertiary),
    );
}

/// A [`legend`] that must be read before its screen is trusted: it leads with `icons::WARNING`.
pub(super) fn warning_legend(ui: &mut egui::Ui, text: &str) {
    let t = ThemeTokens::of(ui.ctx());
    let words =
        egui::RichText::new(text).font(t.font(ThemeTextRole::Caption)).color(t.theme.text_tertiary);
    ui.label(icons::WARNING.before(ui.style(), words));
}

/// Format an epoch-ms bound as a UTC date, or `—` for the zero-width window an empty tree produces.
fn day(timestamp: i64) -> String {
    if timestamp <= 0 {
        "—".to_string()
    } else {
        vike_model::time::epoch_ms_to_utc_date(timestamp)
    }
}

/// The Refresh both Overview and By venue carry. It raises the SAME `stored_refresh` flag as the
/// stored grid's Refresh, and it is disabled — with the reason — while a load is in flight.
fn refresh(ui: &mut egui::Ui, loading: bool) -> bool {
    let b = ThemeButtonAction::secondary((icons::REFRESH, "Refresh"));
    let b = if loading { b.disabled_because(LOADING_WHY) } else { b };
    ui.add(b).on_hover_text(REFRESH_HOVER).clicked()
}

const REFRESH_HOVER: &str = "Read the store again, as Refresh on All series does.";

/// The Gateway screen's Refresh: what it reads and why a change can need it.
const CREDENTIALS_REFRESH_HOVER: &str = "Read the credential store again. It is read when this \
     screen opens and after a Save or Remove here; a change made outside this app shows after a \
     Refresh.";

/// Why every Refresh in this window is dark while a load runs: a second request would only queue
/// work answering the question already asked.
pub(super) const LOADING_WHY: &str = "An inventory load is already in flight.";

/// A line of the Caption role in the caption grey — these screens' legends, notes and headings.
fn caption(t: &ThemeTokens, s: impl Into<String>) -> egui::RichText {
    egui::RichText::new(s).font(t.font(ThemeTextRole::Caption)).color(t.theme.text_tertiary)
}

#[path = "data_manager_tests.rs"]
#[cfg(test)]
mod data_manager_tests;
