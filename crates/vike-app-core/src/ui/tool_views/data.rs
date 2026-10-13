//! The Data-manager tool body — the Symbols (DataSet) editor, the Cached-Series catalog of live
//! feeds, the provider sub-tabs, and the Stored sub-tab (which delegates to
//! [`super::stored_tool_content`]). Moved verbatim from `vike-app`'s `main.rs` (tool-view
//! extraction batch 2); the adaptations are mechanical: the seven threaded read-only params now
//! arrive grouped in [`ToolCtx`], and the table closures that were pure arithmetic became named
//! functions so they finally get unit tests — [`fmt_series_dt`], and [`feed_cell`], the Cached-feeds
//! table's cell text (its column layout is the kit's data table now).
//!
//! `chart::fmt_thousands` is spelled `vike_ui_theme::fmt::fmt_thousands` here — the SAME function
//! (vike-chart re-exports it from vike-ui-theme), reached directly since this crate already
//! depends on the leaf.
//!
//! # The action strips
//!
//! Five destinations open with an ACTION STRIP — the actions that make sense on what is below it
//! and, on the four that have more than one cut of their data, a segmented control saying which
//! cut is on screen. (Cached feeds is the fifth: one cut, so a strip of verbs and no segments.)
//! The strips live here rather than in [`super::data_rail`] because they are BODY furniture: the
//! rail owns which destination is showing, a strip owns what that destination is showing OF.
//!
//! ⚠ **Every strip sits ABOVE the shared grid and none of them reaches into it.**
//! `vike_data_manager::stored_catalog_grid` is mounted by vike-studio as well, against a panel
//! `crates/vike-studio/src/panes/data_browser.rs` records as ~340px wide while the grid's fixed columns
//! already sum to ~690px — so a column added for this window clips that one, and no test covers
//! Studio's Data tab. Anything a strip wants that the grid cannot give it is either derived beside
//! the grid (the Stale sort writes `GridState::sort`, which is already a `pub` field) or rendered
//! as this module's own list instead (the cross-kind partial-day view).
//!
//! ⚠ **The strips' own state lives in egui's temp memory**, keyed off the body `Ui`'s id, and NOT
//! on [`tools::ToolView`]. A `ToolView` field is the natural home for per-window view state —
//! `stored_grid`, `data_sel` and the `ds_*` family all are — and this is a deliberate second-best:
//! the in-tree precedent is `vike_data_manager::stored_catalog_ui`, whose `SortState` rides the
//! same map, and `crates/vike-app-core/src/ui/tool_views/fx_picker.rs`'s `fx_picker_popup` query box.
//! What it costs is that a filter resets to its default when egui evicts the entry, which is the
//! right trade for a filter and would be the wrong one for anything an operator would have to
//! re-enter.

mod activity_log;
mod cached_feeds;
mod datasets;
mod gaps;
mod members;
mod provider_keys;
mod providers;

use super::ToolCtx;
use super::backend_settings::{BackendSettingsState, SettingsEditState, SettingsWriteRequest};
use super::data_rail::{self, DataDest};
use super::{BackendPicker, backend_tab, stored_tool_content, strip_row, venue_label};
use crate::backend::backend_conn::BackendAction;
use crate::backend::backend_editor::{self, EditorState};
use crate::tools;
use activity_log::data_activity_log;
use cached_feeds::data_cached_feeds;
use datasets::data_datasets;
use gaps::{data_has_gaps, data_stale};
use providers::data_providers;
use std::collections::HashMap;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::{Tokens, section};
use vike_ui_theme::icons::{self, Icon};
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::data;

#[cfg(test)]
use activity_log::{LOG_SEGMENTS, LogPlane, log_plane};
#[cfg(any(test, doc))]
use cached_feeds::{feed_cell, fmt_series_dt};
#[cfg(test)]
use datasets::{dataset_groups, ds_load_form};
#[cfg(doc)]
use gaps::partial_days_list;
#[cfg(test)]
use gaps::{
    GAP_SEGMENTS, GapMode, STALE_SORTS, backfillable, gapped_series_keys, span_label,
    stale_cutoff_ms, stale_series_keys,
};
#[cfg(test)]
use members::{
    MEMBER_SEGMENTS, MemberCut, member_cell, member_is_stale, member_keys, member_series,
    provider_names_a_venue,
};
#[cfg(test)]
use providers::{PROVIDER_SEGMENTS, ProviderPlane};

/// **Does this frame need the credential-store reads**
/// (`crates/vike-desktop/src/main.rs`'s `workspace_credentials` and `workspace_account_grids`)
/// **and the `catalog.set_credentialed_venues` push built from them?** Which destination reads the
/// credential store is this module's fact, so the comparison lives here, in the CI-tested crate,
/// rather than as a second copy in the shell that silently stops matching the day a destination
/// is added.
///
/// ⚠ Until this existed, the merge that folded `WinKind::Connections` into `WinKind::Data`
/// (Connections-merges-into-Data-Manager) left that read UNCONDITIONAL on `WinKind::Data` — it
/// used to be scoped to the standalone Connections window alone, which was the only thing that
/// opened this credential read at all. [`DataDest::reads_credentials`] is the one site now, and
/// `only_credentials_backend_and_instruments_read_the_credential_store` in that module holds it to
/// exactly those three destinations.
#[must_use]
pub fn data_tab_reads_credentials(tv: &tools::ToolView) -> bool {
    tv.data_dest.reads_credentials()
}

/// The Data-manager tool body: the Cached-Series catalog + the DataSet Symbols editor + the
/// provider sub-tabs. [`DataDest::AllSeries`] and its two filtered siblings hand off to
/// [`super::stored_tool_content`], which reads the same [`ToolCtx`].
///
/// ⚠ **"Its two filtered siblings" is no longer the whole truth**, and the exception is the one
/// worth knowing: [`DataDest::HasGaps`] hands off only in its per-series mode. Its cross-kind cut
/// is rendered by [`partial_days_list`] instead, because the partial-day map is keyed one level
/// ABOVE a grid row and the shared grid has no view for it — that function's doc is the argument.
#[expect(clippy::too_many_arguments)] // the Credentials/Backend destinations' inputs, carried
// through from the shell the same way the old standalone Connections window received them — see
// `data_body`'s doc for why these ten are not reachable from `tv`/`ctx` alone.
pub fn data_tool_content(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    vars: &HashMap<String, String>,
    grids: &vike_connections::AccountGrids,
    picker: &BackendPicker<'_>,
    action: &mut Option<BackendAction>,
    editor: &mut EditorState,
    registry: &mut Option<backend_editor::RegistryUpdate>,
    settings: &BackendSettingsState,
    settings_refresh: &mut bool,
    settings_edit: &mut SettingsEditState,
    settings_write: &mut Option<SettingsWriteRequest>,
) {
    use egui::{Align, Layout, vec2};
    let t = Tokens::of(ui.ctx());
    ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);

    // The store every destination below is reading, stated once, above all of them. It belongs to
    // no destination — the same argument `connections.rs`'s `strip_row` makes for the live
    // connection — and until it existed the answer was discoverable only by noticing that Delete
    // had greyed out.
    let counts = data_rail::RailCounts::from_ctx(ctx);
    data_rail::store_bar(ui, ctx, &counts);
    ui.add_space(t.metrics.gap);

    // ⚠ The footline is a BOTTOM PANEL, declared BEFORE the row below it, and that ordering is the
    // whole of why it reaches the screen.
    //
    // Two earlier attempts did not. The first appended it after the rail/body row, which claims
    // `ui.available_height()` — all of it — so the strip landed below the window's own floor;
    // `connections.rs`'s `strip_row` doc carries that identical defect's history. The second subtracted
    // a MEASURED reservation from the row's height, and it was still invisible: arithmetic against
    // `available_height()` does not reserve anything egui will honour, it just makes the row
    // shorter and leaves the leftover to whatever draws next. A panel reserves. It is also what
    // `crates/vike-app-core/src/ui/workspace/state.rs` already says about the app's own status bar —
    // added before the central panel, for this reason.
    egui::Panel::bottom("dm_footline")
        .frame(
            egui::Frame::new()
                .fill(t.theme.surface)
                .stroke(egui::Stroke::new(stroke::HAIRLINE, t.theme.border))
                .inner_margin(egui::Margin::symmetric(t.metrics.pad as i8, space::SM as i8)),
        )
        .show(ui, |ui| data_rail::footline(ui, ctx, &counts));

    let full = ui.available_width();
    let rail_w = data::RAIL_W.min(full * data::RAIL_MAX_FRAC);
    let body_h = ui.available_height();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(vec2(rail_w, body_h), Layout::top_down(Align::Min), |ui| {
            ui.spacing_mut().item_spacing.y = space::HAIR;
            // Scrolls: the rail (thirteen rows and its footer note) is taller than a short window,
            // and unscrolled its last lines were painted over the footline below.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .id_salt("dm_rail")
                .show(ui, |ui| data_rail::rail(ui, &mut tv.data_dest, &counts));
        });
        ui.separator();
        // ⚠ The body is allocated the REMAINING width explicitly rather than letting egui infer it.
        // The shared grid's fixed columns sum to ~738px with the checkbox and Partial cells
        // (`crates/vike-studio/src/panes/data_browser.rs` records the same number from the other mount),
        // so a body that silently inherits a narrow width clips cells instead of shrinking them.
        ui.allocate_ui_with_layout(
            vec2((full - rail_w - data::RAIL_SEPARATOR_RESERVE).max(data::BODY_MIN_W), body_h),
            Layout::top_down(Align::Min),
            |ui| {
                ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);
                section::breadcrumb(
                    ui,
                    &["Data Manager", tv.data_dest.label()],
                    &dest_summary(ctx, tv.data_dest, &counts),
                );
                data_body(
                    ui,
                    ctx,
                    tv,
                    vars,
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
fn dest_summary(ctx: &ToolCtx<'_>, dest: DataDest, counts: &data_rail::RailCounts) -> String {
    match dest {
        DataDest::AllSeries => {
            let (rows, bytes) = ctx
                .stored
                .tree
                .iter()
                .fold((0_u64, 0_u64), |(r, b), v| (r + v.total.rows, b + v.total.bytes));
            format!(
                "{} series · {} rows · {}",
                counts.series,
                vike_ui_theme::fmt::fmt_count_compact(rows),
                vike_util::bytes::iec_bytes(bytes)
            )
        }
        DataDest::Overview => "what needs attention, before the filing cabinet".to_string(),
        DataDest::ByVenue => format!("{} venues hold series", counts.venues),
        DataDest::Store => "what is mounted, and what each mount can do".to_string(),
        DataDest::HasGaps => format!("{} of {} series carry a hole", counts.gaps, counts.series),
        DataDest::Stale => "lagging more than 35% of the tree-wide span".to_string(),
        DataDest::CachedFeeds => format!("{} live · binance WebSocket", counts.feeds),
        DataDest::Providers => "where history, events and live ticks come from".to_string(),
        DataDest::ActivityLog => "this session only — not persisted".to_string(),
        DataDest::DataSets => format!("{} saved symbol sets", counts.datasets),
        DataDest::Credentials => "credential presence, per venue and account".to_string(),
        DataDest::Backend => "which box this app is observing".to_string(),
        DataDest::Instruments => {
            super::instruments::instruments_summary(&ctx.catalog.rows(), ctx.catalog.total())
        }
    }
}

/// One destination's body. An exhaustive `match` — the point of [`DataDest`].
///
/// ⚠ The predecessor was an `if/else` chain comparing `tv.data_subtab` against five values, THREE of
/// them bare literals (`== 0`, `== 1`, `== 5`; `crate::ui::startup::DATA_SUBTAB_STORED` existed and was
/// not used at the Stored arm), ending in an `else` that indexed a `[&str; 7]` with the raw value —
/// so an out-of-range write panicked the window on render. Neither shape is expressible now.
///
/// Each destination whose body is more than a call or two is its own function below
/// ([`data_instruments`], [`data_cached_feeds`], [`data_activity_log`], [`data_datasets`],
/// [`data_has_gaps`], [`data_stale`], [`data_providers`]); this function is the dispatcher, and the
/// arms that stayed here are the ones that only hand off. The frame's tokens are read once here and
/// passed down.
///
/// ⚠ **The ten params after `tv` are NOT reachable from `tv`/`ctx` alone**, unlike everything
/// else this function and [`data_tool_content`] read. `DataDest::Credentials`/`DataDest::Backend`
/// call `vike_connections::connections_ui`/[`backend_tab`]/[`strip_row`] directly — the same
/// functions the old standalone Connections window's own tool-entry point called — and those
/// functions need the credential-store reads (`vars`/`grids`) and the backend-picker/editor/
/// settings state (`picker`/`action`/`editor`/`registry`/`settings`/`settings_refresh`/
/// `settings_edit`/`settings_write`) that window held as its OWN per-call parameters, not as
/// fields on `tools::ToolView` or [`ToolCtx`]. ⚠ **This already compiles, end to end** —
/// `crates/vike-desktop/src/tool_window.rs`'s `WinKind::Data` arm builds and supplies all ten values to
/// `data_tool_content` every frame (the same shell-side construction the old `Connections` arm
/// used to do, moved over when `WinKind::Connections` was deleted) — this wiring is complete, not
/// owed to anything still pending.
#[expect(clippy::too_many_arguments)] // see the doc above — ten pass-through params, one tool body
fn data_body(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    vars: &HashMap<String, String>,
    grids: &vike_connections::AccountGrids,
    picker: &BackendPicker<'_>,
    action: &mut Option<BackendAction>,
    editor: &mut EditorState,
    registry: &mut Option<backend_editor::RegistryUpdate>,
    settings: &BackendSettingsState,
    settings_refresh: &mut bool,
    settings_edit: &mut SettingsEditState,
    settings_write: &mut Option<SettingsWriteRequest>,
) {
    let t = Tokens::of(ui.ctx());

    // ⚠ **TAB-INDEPENDENT in its own way.** The old standalone Connections window's own
    // tool-entry point ran this fetch trigger unconditionally whenever the window was open AT
    // ALL, regardless of which of its two tabs was showing — fixing a documented bug where
    // staying on the default tab left the settings slot `Idle` forever, because the trigger used
    // to be tab-gated before that fix: in that window, the status line's digested Backend half
    // AND the Backend segment's badge were BOTH drawn on both tabs, so a fetch that only started
    // once the Backend tab was actually visited left those two permanently blank on the tab an
    // operator was looking at. Here, "the Connections window is open" has no single equivalent:
    // Credentials and Backend are now two of thirteen destinations, not two always-coexisting
    // tabs of one window.
    //
    // ⚠ **The gate is `Credentials | Backend`, and this used to justify that as "both render
    // backend-settings state" — which stopped being true the day the standalone window's status
    // line and tab-badge chrome were deleted with it.** `BackendDigest`/`backend_settings_section`
    // render on EXACTLY ONE of these thirteen destinations now: `backend_settings_section` is
    // reached only through `backend_tab`, which only `DataDest::Backend`'s arm calls
    // (`connections.rs`'s own module doc states this plainly). `DataDest::Credentials` renders no
    // backend-settings state of any kind. The gate still covers both destinations, but for the
    // PRE-WARMING reason the paragraph above gives, not because both render the answer: Credentials
    // and Backend are rail-adjacent siblings in the CONFIGURE group, so keeping the fetch warm
    // while either is on screen means a switch from one to the other never shows a cold `Idle`
    // slot. A per-frame settings fetch behind, say, the DataSets editor would still be the exact
    // defect `crates/vike-app-core/src/ui/tool_views/data_rail.rs`'s `DataDest::reads_credentials`
    // doc warns against for the credential read, paid by a destination that renders no such
    // thing — this gate just does not extend that far.
    // Placed HERE (before the dispatch match, reading the CURRENT `tv.data_dest` the match below
    // is about to use — not a value from before the rail's own click handling ran) rather than in
    // `data_tool_content`, so the two stay impossible to disagree about which destination is
    // actually on screen this frame.
    if matches!(tv.data_dest, DataDest::Credentials | DataDest::Backend)
        && super::should_fetch_settings(picker.active.is_some(), settings)
    {
        *settings_refresh = true;
    }

    // The ambient backend-connection row, drawn directly above EITHER destination, identically —
    // see `connections.rs`'s `strip_row` doc for why it belongs to neither. Same `Credentials |
    // Backend` gate as the fetch trigger above, but for its OWN reason rather than the same one:
    // `strip_row` genuinely IS drawn on both (`connections.rs`'s module doc says so outright),
    // unlike the digest the fetch trigger above warms.
    if matches!(tv.data_dest, DataDest::Credentials | DataDest::Backend) {
        strip_row(ui, picker, action, &mut tv.data_dest, editor);
        section::strip_rule(ui);
    }

    match tv.data_dest {
        DataDest::Overview => {
            // The landing screen returns a destination when the operator clicks through, so the
            // "Go to" row and the per-row actions are real navigation rather than decoration.
            if let Some(d) =
                super::data_screens::overview(ui, ctx, tv, &data_rail::RailCounts::from_ctx(ctx))
            {
                tv.data_dest = d;
            }
        }
        DataDest::ByVenue => {
            // The per-venue rollup returns a destination for the same reason Overview does: its Arm
            // button is NAVIGATION, not arming. This screen owns no ceiling, reads no credential and
            // writes no policy — it hands you to the one screen that does.
            if let Some(d) = super::data_screens::by_venue(ui, ctx, tv) {
                tv.data_dest = d;
            }
        }
        DataDest::Instruments => data_instruments(ui, ctx),
        DataDest::Store => super::data_screens::store(ui, ctx),
        DataDest::CachedFeeds => data_cached_feeds(ui, ctx, tv, &t),
        DataDest::ActivityLog => data_activity_log(ui, ctx, tv, &t),
        DataDest::DataSets => data_datasets(ui, ctx, tv, &t),
        DataDest::AllSeries => {
            // ===== The stored inventory, unfiltered — the filing cabinet =====
            //
            // ⚠ This arm used to be `AllSeries | HasGaps | Stale`, ONE render at three
            // `ViewFilter`s, and the comment on it said so approvingly. The design says they are
            // three SCREENS, and the difference is not cosmetic: a filter is a claim about which
            // rows you are looking at, while a screen also owns the actions that make sense on
            // those rows and the derivation that decides which rows they are. Stale needs to say
            // what its cut-off date is and offer to update everything past it; Has gaps needs a
            // second cut the grid cannot render at all. Neither belongs on a screen also serving
            // the other two, and neither fits in a rail label.
            //
            // The GRID is still one render, and that is the part worth keeping — see this
            // module's header for why nothing here reaches into it.
            tv.stored_grid.active_view = vike_data_manager::ViewFilter::All;
            super::stored_tool_content(ui, ctx, tv);
        }
        DataDest::HasGaps => data_has_gaps(ui, ctx, tv, &t),
        DataDest::Stale => data_stale(ui, ctx, tv, &t),
        DataDest::Credentials => {
            // ===== Credentials — credential presence and the credential EDITOR (from the old
            // Connections/Credentials tab, moved here 2026-10-05) =====
            //
            // This screen arms nothing: an account trades at its own `account.tier` while
            // `account.active = 1`, on the NODE's database, which this desktop does not write. The
            // per-venue arming grid that stood above the editor wrote this PC's LOCAL database,
            // which the daemon never reads, and was deleted with the venue ceiling.
            //
            // ⚠ **The OUTER `ScrollArea` is load-bearing, not decoration.** `data_tool_content`
            // allocates this screen's body as one FIXED-height region with no scroll area of its
            // own, and `connections_ui` (the account picker, the P/D/L rail and the detail pane
            // with its ✎ edit buttons) has none either: without this area a body taller than the
            // window lays out below the visible region, reachable by no click.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .id_salt("credentials_body")
                .show(ui, |ui| {
                    // The grid is the shell's per-frame read of the account rows and their
                    // credentials (`crates/vike-desktop/src/main.rs`'s `workspace_account_grids`),
                    // carrying the verdict of those reads; beside it the live feed-status map and
                    // the one-shot account preselection (`tv.connections_account` is `take`n here,
                    // same contract as the standalone Connections window had).
                    let live: HashMap<String, vike_model::feed_status::ConnectionState> = ctx
                        .feed_statuses
                        .iter()
                        .map(|(venue, handle)| {
                            let s = handle.lock().unwrap().clone();
                            (venue.clone(), vike_model::feed_status::parse_feed_status(&s))
                        })
                        .collect();
                    let preselect = tv.connections_account.take();
                    // The exact call the old Connections window's Credentials tab made.
                    vike_connections::connections_ui(ui, grids, &live, ctx.credentials, preselect);
                });
        }
        DataDest::Backend => {
            // The exact call the old Connections window's Backend tab made — `filter` is the same
            // `tools::ToolView` field that tab read (`tv.connections_settings_filter`), per window.
            backend_tab(
                ui,
                vars,
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
        DataDest::Providers => {
            // Scrolls: the Event plane's "Data providers" group (admin key, two key rows, a status
            // line) makes the body taller than the fixed-height region this screen is given, and
            // like the Credentials destination above it has no scroll area of its own to fall back on.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .id_salt("providers_body")
                .show(ui, |ui| {
                    // Leave the scroll bar its own strip: the long legend lines wrap at the body's
                    // width and ended UNDER the bar.
                    let bar = ui.spacing().scroll;
                    let reserve = bar.bar_width + bar.bar_inner_margin + bar.bar_outer_margin;
                    ui.set_max_width(ui.available_width() - reserve);
                    data_providers(ui, ctx, tv, &t, picker.active);
                });
        }
    }
}

/// The Instruments destination: the venue catalog's rows and the per-venue Refresh, which only
/// REQUESTS a fetch (the comment below says why it never fetches on the frame thread).
fn data_instruments(ui: &mut egui::Ui, ctx: &ToolCtx<'_>) {
    // ⚠ The click NEVER fetches here. `instruments_screen` reports which venue was pressed
    // and `CatalogRefresh::request` spawns the venue REST call on its own thread, so the
    // frame returns while the socket is still open — the same shape as
    // `crates/vike-desktop/src/app_methods.rs`'s `spawn_backend_settings_fetch`. The result
    // lands in `rows()` on a later frame, woken by the repaint this passes in.
    let now = vike_model::now_ms();
    let rows = ctx.catalog.rows();
    if let Some(venue) = super::instruments::instruments_screen(ui, &rows, ctx.catalog.total(), now)
    {
        let ectx = ui.ctx().clone();
        // The refusal is already on screen (the button is disabled with its reason), so a
        // lost race between the disable and the click needs no second report.
        let _ = ctx.catalog.request(&venue, now, move || ectx.request_repaint());
    }
}

// ===== Strip furniture — the shapes every action strip is built from =====
//
// The segmented control and the button are the kit's (`vike_ui_theme::components`): this window's
// two button shapes and two segmented-control shapes are one component each now (spec §4.3). What
// stays here is how THIS window words them.

/// The kit button for a strip action: `label`, led by `icon` when it has one.
fn action<'a>(icon: Option<Icon>, label: &'a str) -> ActionButton<'a> {
    match icon {
        Some(i) => ActionButton::secondary((i, label)),
        None => ActionButton::secondary(label),
    }
}

/// An action with nothing behind it, rendered the ONE honest way: disabled, with the reason on
/// hover (spec §4.2). A live button that silently does nothing is the failure this prevents — an
/// operator clicks it, nothing happens, and the window has told them neither that it refused nor
/// why. `why` is the whole payload, so it names the thing that does not exist rather than "not
/// implemented".
fn dead_action(ui: &mut egui::Ui, icon: Option<Icon>, label: &str, why: &str) {
    ui.add(action(icon, label).disabled_because(why));
}

/// A strip's trailing note — the design's `.legend`: the Caption role, dim. A refusal rule or a
/// derivation is stated here once instead of hiding in ten tooltips.
fn legend(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::of(ui.ctx());
    ui.label(egui::RichText::new(text).font(t.font(TextRole::Caption)).color(t.theme.text3));
}

/// A [`legend`] that must be read before its screen is trusted: it leads with `icons::WARNING`.
fn warning_legend(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::of(ui.ctx());
    let words = egui::RichText::new(text).font(t.font(TextRole::Caption)).color(t.theme.text3);
    ui.label(icons::WARNING.before(ui.style(), words));
}

#[path = "data_tests.rs"]
#[cfg(test)]
mod data_tests;
