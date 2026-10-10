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

use super::ToolCtx;
use super::backend_settings::{BackendSettingsState, SettingsEditState, SettingsWriteRequest};
use super::data_rail::{self, DataDest};
use super::{BackendPicker, backend_tab, strip_row};
use crate::backend::backend_conn::BackendAction;
use crate::backend::backend_editor::{self, EditorState};
use crate::tools;
use std::collections::HashMap;
use vike_data::datasets;
use vike_data_manager::SeriesKey;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::input::{self, Field};
use vike_ui_theme::components::overlay;
use vike_ui_theme::components::rail::{RailItem, nav_rail};
use vike_ui_theme::components::segmented::{self, Segment};
use vike_ui_theme::components::state::{self, Load};
use vike_ui_theme::components::table::{self, Column};
use vike_ui_theme::components::{Status, Tokens, section};
use vike_ui_theme::fmt::fmt_thousands;
use vike_ui_theme::icons::{self, Icon};
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::data;

/// **Does this frame need [`super::VenueArmingInputs`]?** — the shell's gate for the per-frame
/// credential-store read and settings load the venue-arming screen needs and no other one does.
///
/// It lives here, in the CI-tested crate, rather than as a comparison in the desktop shell's
/// `main.rs`: which destination reads the credential store is this module's fact, and a second copy
/// in the shell silently stops matching the day a destination is added.
///
/// ⚠ This used to compare `tv.data_subtab` against a `DATA_SUBTAB_VENUES: usize = 6` declared right
/// here, whose doc called itself "the LAST tab, appended rather than inserted so no persisted
/// `ToolView::data_subtab` changes meaning". **Nothing persisted it** — see
/// [`super::data_rail`]'s module doc for the measurement — so the constant, the append-only rule and
/// the index are all gone. [`DataDest::reads_arming`] is the one site now, and
/// `only_credentials_reads_the_credential_store` in that module holds it to exactly one destination.
#[must_use]
pub fn data_tab_reads_arming(tv: &tools::ToolView) -> bool {
    tv.data_dest.reads_arming()
}

/// **Does this frame need the CHECKED credential-store read**
/// (`crates/vike-desktop/src/main.rs`'s `workspace_credentials_checked`) **and the
/// `catalog.set_credentialed_venues` push built from it?** The twin of
/// [`data_tab_reads_arming`], same reason: which destination reads the credential store is this
/// module's fact, so the comparison lives here rather than as a second copy in the shell.
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
    arming: Option<&super::VenueArmingInputs>,
    vars: &HashMap<String, String>,
    health: &vike_connections::StoreHealth,
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
            data_rail::rail(ui, &mut tv.data_dest, &counts);
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
                    arming,
                    vars,
                    health,
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
                vike_ui_theme::fmt::fmt_bytes(bytes)
            )
        }
        DataDest::Overview => "what needs attention, before the filing cabinet".to_string(),
        DataDest::ByVenue => format!("{} venues hold series", counts.venues),
        DataDest::Store => "what is mounted, and what each mount can do".to_string(),
        DataDest::HasGaps => format!("{} of {} series carry a hole", counts.gaps, counts.series),
        DataDest::Stale => "lagging more than 35% of the tree-wide span".to_string(),
        DataDest::CachedFeeds => format!("{} live · binance WebSocket", counts.feeds),
        DataDest::Providers => "the sources a backfill can draw from".to_string(),
        DataDest::ActivityLog => "this session only — not persisted".to_string(),
        DataDest::DataSets => format!("{} saved symbol sets", counts.datasets),
        DataDest::Credentials => {
            "ceiling, credential presence, and what the mount actually did".to_string()
        }
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
/// ⚠ **The ten params after `arming` are NOT reachable from `tv`/`ctx` alone**, unlike everything
/// else this function and [`data_tool_content`] read. `DataDest::Credentials`/`DataDest::Backend`
/// call `vike_connections::connections_ui`/[`backend_tab`]/[`strip_row`] directly — the same
/// functions the old standalone Connections window's own tool-entry point called — and those
/// functions need the credential-store read (`vars`/`health`) and the backend-picker/editor/
/// settings state (`picker`/`action`/`editor`/`registry`/`settings`/`settings_refresh`/
/// `settings_edit`/`settings_write`) that window held as its OWN per-call parameters, not as
/// fields on `tools::ToolView` or [`ToolCtx`]. ⚠ **This already compiles, end to end** —
/// `crates/vike-desktop/src/main.rs`'s `WinKind::Data` arm builds and supplies all ten values to
/// `data_tool_content` every frame (the same shell-side construction the old `Connections` arm
/// used to do, moved over when `WinKind::Connections` was deleted) — this wiring is complete, not
/// owed to anything still pending.
#[expect(clippy::too_many_arguments)] // see the doc above — ten pass-through params, one tool body
fn data_body(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    arming: Option<&super::VenueArmingInputs>,
    vars: &HashMap<String, String>,
    health: &vike_connections::StoreHealth,
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
    // defect `crates/vike-app-core/src/ui/tool_views/data_rail.rs`'s `DataDest::reads_arming` doc
    // warns against for the credential read, paid by a destination that renders no such thing —
    // this gate just does not extend that far.
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
            // ===== Credentials — ceiling + mount result (the old VenueArming destination's own
            // content, now removed) + credential presence (from the old Connections/Credentials
            // tab), folded into one screen 2026-10-05 =====
            //
            // ⚠ **A single OUTER `ScrollArea` wraps both pieces, and that is load-bearing, not
            // decoration.** `data_tool_content` allocates this screen's body as one FIXED-height
            // region with no scroll area of its own. `venues_tab_content`'s grid draws into its own
            // inner `ScrollArea` directly below; `connections_ui` (the account picker, the S/D/L
            // rail and the detail pane with its ✎ edit buttons — the actual credential EDITOR) has
            // none. Without this outer area, the grid's inner one used to claim the destination's
            // entire fixed height (even shrunk to its content now — see `venues_tab_content`'s own
            // doc), and `connections_ui` had nowhere left to lay out but below the visible region —
            // reachable by no click. This area gives the fixed-height body somewhere to put BOTH in
            // sequence, scrolled as one.
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .id_salt("credentials_body")
                .show(ui, |ui| {
                    // ⚠ The change journal and the timestamp come off `ctx.credentials`, which is
                    // NOT a misuse of the credential channel: that field carries the ledger and the
                    // instant the binary's ONE boot walk produced
                    // (`vike_connections::CredentialWrite`), and this write records into the same
                    // ledger under a `set_setting` kind. The credential STORE path in that struct is
                    // not read here — a policy write touches no credential at all.
                    super::venues_tab_content(
                        ui,
                        arming,
                        &mut tv.arm_edit,
                        ctx.credentials.journal,
                        ctx.credentials.now_ms,
                    );
                    section::strip_rule(ui);
                    // The exact construction the old standalone Connections window's tool-entry
                    // point did before calling `connections_ui` — one grid per account the store
                    // holds, the live feed-status map, and the one-shot account preselection
                    // (`tv.connections_account` is `take`n here, same contract as the standalone
                    // Connections window had).
                    let grids = vike_connections::AccountGrids::from_vars(vars);
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
                    vike_connections::connections_ui(
                        ui,
                        &grids,
                        &live,
                        health,
                        ctx.credentials,
                        preselect,
                    );
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
        DataDest::Providers => data_providers(ui, ctx, tv, &t),
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

/// The Cached feeds destination: the live-feed catalog, its action strip and its table.
fn data_cached_feeds(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView, t: &Tokens) {
    use egui::RichText;
    use egui::{Align, Layout, vec2};
    let feeds = ctx.feeds;
    // ===== Cached feeds — the live-feed catalog (the only one with real data) =====
    //
    // ⚠ **The action set below is the DESIGN's, and it REPLACED a different one.** This
    // strip used to carry ten dead data-ENGINE verbs — Update all, Download / Extend…,
    // Import CSV…, Inspect, Repair gaps, Clean data, Pin / Unpin, Instruments…, Truncate…,
    // Remove inactive… — a wishlist for a stored-series manager, rendered on the one
    // screen in this window that shows no stored series at all. The design replaces them
    // with the FEED-plane verbs a live-socket catalogue actually has, and every one of
    // them now states, on its disabled-hover, the thing that does not exist behind it.
    // That is the difference between a strip that is honest and a strip that is merely
    // grey: the old ten were dimmed and said nothing, so "why can I not click this" had no
    // answer anywhere in the window.
    //
    // ⚠ **One of the design's ten is NOT dead here, and the design's own mock dims it:**
    // `Remove feed`. `tools::ToolView::data_delete` is a live out-slot —
    // `crates/vike-desktop/src/app_ui.rs` drains it, stops that subscription and logs the
    // stop — so the verb works today. It shipped spelled `🗑 Delete`, which on a screen
    // about sockets reads as deleting DATA; what it does is end a subscription. The LABEL
    // moves onto the verb and the behaviour does not move at all. Matching the mock by
    // dimming a button that works would be the opposite of the honesty rule, not an
    // instance of it.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);
        if ui.add(ActionButton::secondary((icons::REFRESH, "Refresh"))).clicked() {
            let ts = vike_chart::to_naive(chrono::Utc::now().timestamp_millis(), ctx.display_tz)
                .map(|dt| dt.format("%H:%M:%S").to_string())
                .unwrap_or_default();
            tv.data_log.push(format!("{ts}  Refreshed · {} series", feeds.len()));
        }
        for (icon, label, why) in FEED_ACTIONS_BEFORE_REMOVE {
            dead_action(ui, icon, label, why);
        }
        // The design's `Remove feed`, in the design's position, LIVE — see the arm's
        // header comment. Selection-gated because it stops exactly one subscription.
        let remove = ActionButton::secondary((icons::REMOVE, "Remove feed"));
        let remove = if tv.data_sel.is_some() {
            remove
        } else {
            remove.disabled_because(
                "Pick a row first — this stops ONE subscription, the one selected",
            )
        };
        if ui
            .add(remove)
            .on_hover_text("Stop the selected subscription; its cached bars go with it")
            .clicked()
        {
            tv.data_delete = tv.data_sel.take();
        }
        for (icon, label, why) in FEED_ACTIONS_AFTER_REMOVE {
            dead_action(ui, icon, label, why);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!("{} series", feeds.len()))
                    .font(t.font(TextRole::Caption))
                    .color(t.theme.text3),
            );
        });
    });
    legend(
        ui,
        "A feed is started by a chart window subscribing a series, never from this \
                 catalogue — so the one lifecycle verb here is the one that ends a subscription.",
    );
    ui.add_space(space::SM);

    // The kit's data table (owner decision 3): every cell is text. The selected row is
    // derived from `data_sel`, so a feed that vanishes simply selects nothing.
    let selected = tv.data_sel.as_deref().and_then(|k| feeds.iter().position(|f| f.0 == k));
    let seen = table::data_table(ui, "data_tbl", &FEED_COLUMNS, feeds.len(), selected, |r, c| {
        feed_cell(&feeds[r], c, ctx.directory)
    });
    if let Some(r) = seen.clicked {
        tv.data_sel = Some(feeds[r].0.clone());
    }
    if feeds.is_empty() {
        state::view(ui, Load::Empty("No cached series — open a chart to start a feed."));
    }
}

/// The Activity log destination: this session's in-memory log, with a plane filter, a find box,
/// Export (dead, and says why) and Clear.
fn data_activity_log(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView, t: &Tokens) {
    use egui::RichText;
    use egui::vec2;
    let feeds = ctx.feeds;
    // ===== Activity log — promoted out of the Cached-feeds body =====
    //
    // It was a 110px-tall box under that table, which is the wrong home for the only record of
    // what this window has done: a reconnect, a bulk backfill's outcome and a delete all land
    // here, and all three were read through a letterbox. As its own destination it gets the
    // window.
    //
    // The strip is the design's: a segmented plane filter, a find box, Export and Clear.
    // Two of the four are REAL — the filter and Clear both act on `tv.data_log`, which
    // this module owns outright — and the other two are argued where they are rendered.
    let filter_id = ui.id().with("dm_log_filter");
    let find_id = ui.id().with("dm_log_find");
    let mut filter: Option<LogPlane> =
        ui.data_mut(|d| d.get_temp::<Option<LogPlane>>(filter_id)).unwrap_or_default();
    let mut find: String = ui.data_mut(|d| d.get_temp::<String>(find_id)).unwrap_or_default();
    let mut clear = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);
        segmented::segmented(ui, &mut filter, &LOG_SEGMENTS);
        ui.label(RichText::new("Find").font(t.font(TextRole::Caption)).color(t.theme.text3));
        ui.scope(|ui| {
            ui.spacing_mut().text_edit_width = data::LOG_FIND_W;
            input::text(
                ui,
                &mut find,
                Field { hint: "substring, case-insensitive", ..Field::default() },
            );
        });
        dead_action(
            ui,
            Some(icons::EXPORT),
            "Export",
            "Nothing in a tool body writes a file — the window carries no out-slot for one \
                     — so this log reaches disk only through the JSON file sink, which is a \
                     different sink with its own level.",
        );
        let clear_b = ActionButton::secondary((icons::CLEAR, "Clear"));
        let clear_b = if tv.data_log.is_empty() {
            clear_b.disabled_because("The log is already empty")
        } else {
            clear_b
        };
        if ui
            .add(clear_b)
            .on_hover_text(
                "Drop every line — this log lives in memory and nothing else holds a copy",
            )
            .clicked()
        {
            clear = true;
        }
    });
    ui.data_mut(|d| d.insert_temp(filter_id, filter));
    ui.data_mut(|d| d.insert_temp(find_id, find.clone()));
    if clear {
        tv.data_log.clear();
    }

    // The filter runs over the line TEXT, because nothing tags a line — `log_plane`'s doc
    // carries why that seam is where it is and what it would take to move it.
    let needle = find.trim().to_ascii_lowercase();
    let shown: Vec<&String> = tv
        .data_log
        .iter()
        .filter(|l| filter.is_none_or(|p| log_plane(l.as_str()) == p))
        .filter(|l| needle.is_empty() || l.to_ascii_lowercase().contains(&needle))
        .collect();
    // The count is stated even when nothing is filtered out, so an EMPTY plane reads as
    // "0 of 14 lines" — a filter that matched nothing — rather than as an empty log.
    ui.label(RichText::new("ACTIVITY LOG").size(t.text.px(TextRole::Caption)).color(t.theme.text3));
    legend(ui, &format!("{} of {} lines shown", shown.len(), tv.data_log.len()));
    ui.add_space(space::XS);
    // Every line is read character by character (timestamps, keys): mono, the Body role.
    let line_font = t.mono(TextRole::Body);
    // The footnote is drawn AFTER the log panel, and the panel's `ScrollArea` takes every pixel the
    // body has left (`auto_shrink([false, false])`), so the footnote used to land below the body's
    // bottom edge — on the foot line (a real-GPU capture, 2026-10-05). Its height is reserved
    // first, and the panel is shown in a child capped to what is left.
    const FOOTNOTE: &str = "This is the in-memory session log — it is not persisted and is not the \
                            JSON file log, whose level is VIKE_LOG_FILE_LEVEL.";
    let footnote_h = ui
        .painter()
        .layout(FOOTNOTE.to_owned(), t.font(TextRole::Caption), t.theme.text3, ui.available_width())
        .rect
        .height()
        + space::MD
        + ui.spacing().item_spacing.y;
    ui.scope(|ui| {
        ui.set_max_height((ui.available_height() - footnote_h).max(0.0));
        egui::Frame::new()
            .fill(t.theme.surface)
            .stroke(egui::Stroke::new(stroke::HAIRLINE, t.theme.border))
            .corner_radius(egui::CornerRadius::same(RADIUS))
            .inner_margin(t.metrics.gap)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .id_salt("data_log")
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.set_min_height(data::LOG_MIN_H);
                        if tv.data_log.is_empty() {
                            // A line, not an empty state: the log is ready, not empty-handed.
                            ui.label(
                                RichText::new(format!(
                                    "Ready — {} live feeds (Binance WebSocket).",
                                    feeds.len()
                                ))
                                .font(line_font.clone())
                                .color(t.theme.text2),
                            );
                        } else if shown.is_empty() {
                            ui.label(
                                RichText::new(
                                    "No line matches this filter — the log is not empty.",
                                )
                                .font(line_font.clone())
                                .color(t.theme.text3),
                            );
                        }
                        for line in &shown {
                            ui.label(
                                RichText::new(*line).font(line_font.clone()).color(t.theme.text2),
                            );
                        }
                    });
            });
    });
    ui.add_space(space::MD);
    legend(ui, FOOTNOTE);
}

/// The DataSets destination: the DataSet tree on the left and the editor form on the right. The
/// member cut's state is read and written back HERE, beside the `Ui` whose id keys it, and lent to
/// the editor column by `&mut`.
fn data_datasets(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView, t: &Tokens) {
    use egui::{Align, Layout, vec2};
    let dsets = ctx.dsets;
    // ===== DataSets — the DataSet tree + editor (the old "Symbols" tab) =====
    //
    // The member-symbol strip's parts sit at MODULE scope with their siblings —
    // `MemberCut`/`MEMBER_SEGMENTS` beside `GapMode`, and
    // `Member`/`provider_names_a_venue`/`member_series`/`member_is_stale`/`member_keys`
    // beside `stale_series_keys` — so this arm holds RENDERING and no derivation.
    //
    // ⚠ They were declared block-local INSIDE this arm first, and what the move bought is
    // worth recording, because it is exactly what the locality cost: the `#[cfg(test)]`
    // module at the bottom of this file can now REACH them. The roster gate
    // (`every_segment_roster_offers_each_value_once_and_leads_with_the_default`) covers
    // `MemberCut`'s segments, and every derivation has tests where it previously had a
    // doc comment asserting the same thing to nobody. The move was pure — nothing here
    // ever closed over an arm local — so the only thing that changed is what is CHECKED.

    // The member cut rides egui temp memory keyed off the BODY `Ui`'s id, like every other
    // strip's state on this screen — the module doc argues why, and `tools.rs` is not this
    // arm's to extend. Read HERE, beside the `Ui` whose id keys it, because the control
    // itself is rendered two closures deep inside the editor column, where `ui.id()` is a
    // different id entirely and would key a second, unrelated entry.
    let member_cut_id = ui.id().with("dm_ds_member_cut");
    let mut member_cut: MemberCut =
        ui.data_mut(|d| d.get_temp::<MemberCut>(member_cut_id)).unwrap_or_default();

    if tv.ds_name.is_empty()
        && tv.ds_sel.is_none()
        && let Some(d) = dsets.sets.first()
    {
        ds_load_form(tv, d);
    }
    let groups = dataset_groups(&dsets.sets, ctx.directory);
    let full = ui.available_width();
    let tree_w = (full * data::DS_TREE_FRAC).clamp(data::DS_TREE_MIN_W, data::DS_TREE_MAX_W);
    ui.horizontal_top(|ui| {
        // ----- LEFT: + New DataSet + grouped tree -----
        ui.allocate_ui_with_layout(
            vec2(tree_w, ui.available_height()),
            Layout::top_down(Align::Min),
            |ui| {
                datasets_tree(ui, tv, dsets, &groups, tree_w, t);
            },
        );
        ui.add_space(space::XL);
        // ----- RIGHT: editor form -----
        ui.allocate_ui_with_layout(
            vec2(full - tree_w - space::XL2, ui.available_height()),
            Layout::top_down(Align::Min),
            |ui| {
                datasets_editor(ui, ctx, tv, t, &mut member_cut);
            },
        );
    });
    // Written back ONCE, after every closure that could have moved it has returned — the
    // shape `GapMode`'s strip uses, and the reason its read sits at the top of this arm.
    // The module doc states what egui temp memory costs: the cut falls back to
    // `all members` if egui evicts the entry, which is the right trade for a FILTER and
    // would be the wrong one for anything an operator had typed.
    ui.data_mut(|d| d.insert_temp(member_cut_id, member_cut));
}

/// DataSets, left column: `+ New DataSet` and the grouped tree of saved sets.
fn datasets_tree(
    ui: &mut egui::Ui,
    tv: &mut tools::ToolView,
    dsets: &datasets::Store,
    groups: &[DsGroup],
    tree_w: f32,
    t: &Tokens,
) {
    if ui
        .add_sized(
            [tree_w - space::MD, t.metrics.control_h],
            ActionButton::secondary("+ New DataSet"),
        )
        .clicked()
    {
        tv.ds_sel = None;
        tv.ds_name = dsets.fresh_name();
        tv.ds_provider = "Auto".into();
        tv.ds_interval = "1m".into();
        tv.ds_benchmark.clear();
        tv.ds_symbols_text.clear();
    }
    ui.add_space(space::MD);
    egui::ScrollArea::vertical().id_salt("ds_tree").show(ui, |ui| {
        // The kit rail (owner decision 2b): one heading per group, a row per
        // set, and the open set reported as a Label. A set listed under two
        // groups is one value, so both of its rows light together, as they
        // always did.
        let mut items: Vec<RailItem<'_, Option<usize>>> = Vec::new();
        for g in groups {
            for (i, d) in dsets.sets.iter().enumerate() {
                if g.has(d) {
                    items.push(RailItem {
                        value: Some(i),
                        group: g.label.as_str(),
                        icon: icons::DATASETS,
                        label: d.name.as_str(),
                        count: None,
                    });
                }
            }
        }
        let mut open = dsets.sets.iter().position(|d| d.name == tv.ds_name);
        if nav_rail(ui, &mut open, &items)
            && let Some(i) = open
        {
            ds_load_form(tv, &dsets.sets[i]);
        }
    });
}

/// DataSets, editor column — the form rows: Name, Provider, Interval, Benchmark.
fn datasets_form_rows(ui: &mut egui::Ui, tv: &mut tools::ToolView, t: &Tokens) {
    use egui::RichText;
    use egui::{Align, Layout, vec2};
    let lbl_w = data::DS_LABEL_W;
    let row = |ui: &mut egui::Ui, label: &str, body: &mut dyn FnMut(&mut egui::Ui)| {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                vec2(lbl_w, t.metrics.control_h),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.label(
                        RichText::new(label).font(t.font(TextRole::Body)).color(t.theme.text2),
                    );
                },
            );
            body(ui);
        });
    };
    row(ui, "Name", &mut |ui| {
        ui.scope(|ui| {
            ui.spacing_mut().text_edit_width = data::DS_NAME_FIELD_W;
            input::text(ui, &mut tv.ds_name, Field::default());
        });
    });
    // A dropdown is a kit button carrying a caret; its menu marks the current
    // value with a check.
    row(ui, "Provider", &mut |ui| {
        let resp = ui.add_sized(
            [data::DS_PROVIDER_W, t.metrics.control_h],
            ActionButton::secondary((tv.ds_provider.as_str(), icons::DISCLOSE_OPEN)),
        );
        egui::Popup::menu(&resp).show(|ui| {
            ui.set_min_width(data::DS_PROVIDER_MENU_MIN_W);
            for p in ["Auto", "binance", "bybit", "okx", "coinbase", "kraken", "yahoo", "dukascopy"]
            {
                let current = (tv.ds_provider == p).then_some(icons::CHECK);
                if overlay::menu_item(ui, current, p, None).clicked() {
                    tv.ds_provider = p.to_string();
                }
            }
        });
    });
    row(ui, "Interval", &mut |ui| {
        let resp = ui.add_sized(
            [data::DS_INTERVAL_W, t.metrics.control_h],
            ActionButton::secondary((tv.ds_interval.as_str(), icons::DISCLOSE_OPEN)),
        );
        egui::Popup::menu(&resp).show(|ui| {
            ui.set_min_width(data::DS_INTERVAL_MENU_MIN_W);
            for iv in ["1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "1d", "1w"] {
                let current = (tv.ds_interval == iv).then_some(icons::CHECK);
                if overlay::menu_item(ui, current, iv, None).clicked() {
                    tv.ds_interval = iv.to_string();
                }
            }
        });
    });
    row(ui, "Benchmark", &mut |ui| {
        ui.scope(|ui| {
            ui.spacing_mut().text_edit_width = data::DS_BENCHMARK_FIELD_W;
            input::text(
                ui,
                &mut tv.ds_benchmark,
                Field {
                    hint: "optional, e.g. SPY / BTCUSDT (else equal-weight)",
                    ..Field::default()
                },
            );
        });
    });
}

/// DataSets, right column: the form rows, the member strip and list, the symbols editor and the
/// action strip, in that order. `member_cut` is this destination's own state, read once by
/// [`data_datasets`] and lent here.
fn datasets_editor(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    t: &Tokens,
    member_cut: &mut MemberCut,
) {
    use egui::RichText;
    use egui::vec2;
    datasets_form_rows(ui, tv, t);
    ui.add_space(space::LG);
    // "Symbols in this DataSet — select one to Test" (read-only list)
    ui.label(
        RichText::new("Symbols in this DataSet — select one to Test")
            .font(t.font(TextRole::Body))
            .color(t.theme.text2),
    );
    // ⚠ Recomputed from the TEXT every frame rather than cached: the
    // multiline editor further down is the writer, and an operator can retype
    // the whole membership between two frames.
    let syms = datasets::parse_symbols(&tv.ds_symbols_text);
    // The set's own declaration, captured ONCE. Both the Provider and the
    // Interval popup are rendered ABOVE, so these are this frame's values, and
    // nothing below writes either — so the member resolution here and the bulk
    // button at the foot of this column cannot be reading different ones.
    let ds_provider = tv.ds_provider.clone();
    let ds_interval = tv.ds_interval.clone();
    // Staleness is DERIVED from what this screen already holds — the tree, and
    // the same tree-wide window the Stale destination judges against. No new
    // I/O path and no second fetch: `vike_data_manager::global_span` and
    // `is_stale` are the two functions `stale_series_keys` calls.
    let (gfirst, glast) = vike_data_manager::global_span(ctx.stored.tree);
    // ONE resolution per member, reused by the filter, the counts, the row
    // markers and the bulk button — so the number on the button and the rows in
    // the list cannot disagree. That is the property `stale_series_keys`'s and
    // `gapped_series_keys`'s docs argue for at length on the sibling strips,
    // and it is the reason this is a `Vec` computed once here rather than a
    // predicate re-evaluated at each site that asks.
    let members: Vec<Member> =
        syms.iter().map(|s| (s.clone(), member_series(ctx.stored.tree, &ds_provider, s))).collect();
    let stale_n = members.iter().filter(|(_, st)| member_is_stale(st, gfirst, glast)).count();
    let unstored_n = members.iter().filter(|(_, st)| st.is_empty()).count();
    // ----- the design's member strip: `all members` / `stale only` -----
    //
    // ⚠ Rendered BEFORE the list is cut, so a click takes effect on the frame
    // it happens on rather than the next one — the ordering `GapMode`'s strip
    // uses, and the whole reason `shown` is computed below this block instead
    // of above it. Reversed, the control lights up a segment while the rows
    // below it still answer the other one, which reads as a stuck filter.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);
        segmented::segmented(ui, member_cut, &MEMBER_SEGMENTS);
        // The window is on the strip for the reason the Stale screen states
        // it: the cut-off is a property of the TREE, so it moves every time
        // the newest row anywhere in the store does. A `stale only` list with
        // no window beside it reads as a fixed calendar date.
        ui.label(RichText::new("tree window").font(t.font(TextRole::Caption)).color(t.theme.text3));
        ui.label(
            RichText::new(span_label(gfirst, glast))
                .font(t.mono(TextRole::Caption))
                .color(t.theme.text2),
        );
    });
    let shown: Vec<&Member> = match *member_cut {
        MemberCut::AllMembers => members.iter().collect(),
        MemberCut::StaleOnly => {
            members.iter().filter(|(_, st)| member_is_stale(st, gfirst, glast)).collect()
        }
    };
    // The kit table (owner decision 3): header, zebra, the selected row's
    // accent edge. Capped at the header and five rows, as the old 92 px box was.
    let selected = shown.iter().position(|(s, _)| tv.ds_sym_sel.as_deref() == Some(s.as_str()));
    let table_h = data::DS_MEMBER_ROWS as f32 * t.metrics.row_h;
    let seen = ui
        .allocate_ui(egui::vec2(ui.available_width(), table_h), |ui| {
            table::data_table(ui, "ds_syms", &MEMBER_COLUMNS, shown.len(), selected, |r, c| {
                member_cell(shown[r], c, gfirst, glast)
            })
        })
        .inner;
    if let Some(r) = seen.clicked {
        tv.ds_sym_sel = Some(shown[r].0.clone());
    }
    if shown.is_empty() {
        // ⚠ TWO different emptinesses, said apart. "No symbols" and "none of
        // them is stale" send an operator to different places, and one
        // message serving both would report an empty set when the filter is
        // what emptied it.
        state::view(
            ui,
            Load::Empty(if members.is_empty() {
                "No symbols yet."
            } else {
                "No member is stale against this window."
            }),
        );
    }
    legend(
        ui,
        &format!(
            "{} of {} member symbols shown · {stale_n} stale · {unstored_n} \
                                 not in this store. Stale is the tree-wide 35% cut the Stale \
                                 destination applies, asked per member: a symbol this store holds \
                                 nothing for is NOT stale — it has nothing to be behind — so \
                                 `stale only` hides it rather than leading with it.",
            shown.len(),
            members.len(),
        ),
    );
    datasets_symbols_editor(ui, tv, t);
    datasets_action_strip(ui, tv, t, &shown, &ds_provider, &ds_interval);
}

/// DataSets, editor column — the multiline symbols editor and the Ask-the-AI box.
fn datasets_symbols_editor(ui: &mut egui::Ui, tv: &mut tools::ToolView, t: &Tokens) {
    use egui::RichText;
    ui.add_space(space::LG);
    ui.label(
        RichText::new("Symbols (comma or newline separated)")
            .font(t.font(TextRole::Body))
            .color(t.theme.text2),
    );
    // Still egui's own multiline editor: the kit has no multiline input (a
    // knob on this PR's report). Symbols are read character by character: mono.
    ui.add(
        egui::TextEdit::multiline(&mut tv.ds_symbols_text)
            .font(t.mono(TextRole::Body))
            .hint_text(RichText::new("BTCUSDT, ETHUSDT, SOLUSDT…").color(t.theme.text3))
            .desired_rows(4)
            .desired_width(f32::INFINITY),
    );
    ui.add_space(space::LG);
    // Ask the AI
    egui::Frame::new()
        .stroke(egui::Stroke::new(stroke::HAIRLINE, t.theme.border))
        .corner_radius(egui::CornerRadius::same(RADIUS))
        .inner_margin(t.metrics.pad)
        .show(ui, |ui| {
            ui.label(RichText::new("Ask the AI").font(t.font(TextRole::Body)).color(t.theme.text2));
            ui.scope(|ui| {
                let w = ui.available_width();
                ui.spacing_mut().text_edit_width = w;
                input::text(
                    ui,
                    &mut tv.ds_ai_prompt,
                    Field { hint: "e.g. top 10 liquid crypto majors", ..Field::default() },
                );
            });
            if ui.add(ActionButton::secondary("Suggest")).clicked() {
                let s = datasets::suggest_symbols(&tv.ds_ai_prompt);
                if !s.is_empty() {
                    tv.ds_symbols_text = s.join(", ");
                }
            }
        });
}

/// DataSets, editor column — the strip of Backfill N / Save / Test symbol / Test DataSet / Delete,
/// over the cut the member list shows (`shown`) and the set's own provider and interval, which the
/// caller captured once so the list and this strip cannot read different ones.
fn datasets_action_strip(
    ui: &mut egui::Ui,
    tv: &mut tools::ToolView,
    t: &Tokens,
    shown: &[&Member],
    ds_provider: &str,
    ds_interval: &str,
) {
    use egui::{Align, Layout};
    ui.add_space(space::XL);
    // Backfill N symbols / Save / Test symbol / Test DataSet / Delete
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = t.metrics.gap;
        // ----- the design's `Backfill 12 symbols` — LIVE -----
        //
        // It leads the strip because the design's does (`Backfill …`, then
        // `Test DataSet`, then `Save`). The remaining buttons keep the order
        // they had: the design's strip is a SUBSET of this one, so reordering
        // the rest would be churn a reviewer cannot check against anything.
        //
        // It fills the SAME out-slot the Has-gaps and Stale strips fill —
        // `tools::ToolView::stored_backfill`, drained by
        // `crates/vike-desktop/src/app_methods.rs`'s
        // `maybe_spawn_stored_backfill` — so nothing new is wired to make it
        // real. N is the count the CURRENT member cut shows, which is what
        // joins the two controls into one gesture: filter to `stale only`,
        // then backfill exactly those.
        //
        // Per MEMBER first, then flattened — the two-step is what lets the
        // hover distinguish a symbol that produced NO key at all from a series
        // the planner will skip. Flattening straight to keys loses the former,
        // and it is the one an operator can act on: it means the form above is
        // incomplete, not that the venue is unsupported.
        let per_member: Vec<Vec<SeriesKey>> = shown
            .iter()
            .map(|(s, stored)| member_keys(stored, ds_provider, ds_interval, s))
            .collect();
        let keys: Vec<SeriesKey> = per_member.iter().flatten().cloned().collect();
        // The SAME client-side gate the two sibling strips print, through the
        // same function: `kind == "bar"` with an interval, and no venue term.
        // `backfillable`'s own doc carries why the design mock's "binance,
        // bybit and okx only" line is stale and must not be restated — here
        // included, and a tooltip counts.
        let fillable = backfillable(&keys);
        let no_key_n = per_member.iter().filter(|k| k.is_empty()).count();
        let noun = if shown.len() == 1 { "symbol" } else { "symbols" };
        let queueable = !keys.is_empty();
        let label = format!("Backfill {} {noun}", shown.len());
        let refusal = if shown.is_empty() {
            NO_MEMBERS_TO_QUEUE.to_string()
        } else {
            // ⚠ Names the two values rather than a verdict: the same refusal
            // covers `Auto` and a blank interval, and an operator told only
            // "cannot backfill" has to guess which of the two fields above to
            // go and fix.
            format!(
                "None of these {} {noun} resolves to a series to fetch. This \
                                     store holds nothing for them, and the set declares Provider \
                                     `{ds_provider}` at interval `{ds_interval}` — a key needs a \
                                     real venue AND an interval, and `Auto` names no venue. Set \
                                     both on the form above, or Refresh the store.",
                shown.len(),
            )
        };
        let hover = format!(
            "Queue the {} {noun} this cut shows — {} series in all. \
                                 {fillable} will plan a job (kind=bar with an interval); the other \
                                 {} are counted as skips, never silently dropped. {no_key_n} of \
                                 the {noun} shown resolve to no series at all and are not queued. \
                                 Progress lands in the status line beside Refresh.",
            shown.len(),
            keys.len(),
            keys.len().saturating_sub(fillable),
        );
        let backfill = action(Some(icons::BACKFILL), &label);
        let backfill = if queueable { backfill } else { backfill.disabled_because(&refusal) };
        if ui.add(backfill).on_hover_text(hover).clicked() {
            tv.stored_backfill.extend(keys.iter().cloned());
        }
        // Save is the view's primary action (spec §4.3).
        if ui.add(ActionButton::primary((icons::SAVE, "Save"))).clicked() {
            tv.ds_save = true;
        }
        // the list pick if any, else the first symbol in the editor
        let test_sym = tv
            .ds_sym_sel
            .clone()
            .or_else(|| datasets::parse_symbols(&tv.ds_symbols_text).into_iter().next());
        let test = ActionButton::secondary((icons::RUN, "Test symbol"));
        let test = if test_sym.is_some() { test } else { test.disabled_because(NO_SYMBOL_TO_TEST) };
        if ui.add(test).clicked() {
            tv.ds_test = test_sym;
        }
        ui.add(
            ActionButton::secondary((icons::RUN, "Test DataSet")).disabled_because(NO_DATASET_TEST),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Danger: this deletes at once — there is no confirm behind it.
            let delete = ActionButton::danger((icons::DELETE, "Delete"));
            let delete = if tv.ds_sel.is_some() {
                delete
            } else {
                delete.disabled_because(NOTHING_SAVED_TO_DELETE)
            };
            if ui.add(delete).clicked() {
                tv.ds_delete = tv.ds_sel.take();
                tv.ds_name.clear();
            }
        });
    });
}

/// The Has gaps destination: two different questions about missing data (a per-series hole, and a
/// cross-kind partial day) on one screen, behind a segmented control.
fn data_has_gaps(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView, t: &Tokens) {
    use egui::RichText;
    use egui::vec2;
    // ===== Has gaps — two different questions about missing data, on one screen =====
    //
    // The two cuts are NOT two filters over one set. A per-series gap is a hole inside one
    // series' own timeline; a cross-kind partial day is a day on which an INSTRUMENT holds
    // some of its kinds and not others, which is invisible per series because each series
    // is perfectly contiguous on its own. `vike_data_manager::PartialDayMap`'s doc argues
    // that distinction at length and it is the whole reason this destination has a
    // segmented control rather than a checkbox: the second cut has no rows in the grid to
    // filter, so it is rendered here, by this module, over `StoredCtx::partials`.
    let mode_id = ui.id().with("dm_gap_mode");
    let mut mode: GapMode = ui.data_mut(|d| d.get_temp::<GapMode>(mode_id)).unwrap_or_default();
    let gapped = gapped_series_keys(ctx.stored.tree, ctx.stored.gaps);
    let ranges: usize = gapped.iter().filter_map(|k| ctx.stored.gaps.get(k)).map(Vec::len).sum();
    let (gfirst, glast) = vike_data_manager::global_span(ctx.stored.tree);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);
        segmented::segmented(ui, &mut mode, &GAP_SEGMENTS);
        ui.label(RichText::new("Window").font(t.font(TextRole::Caption)).color(t.theme.text3));
        ui.label(
            RichText::new(span_label(gfirst, glast))
                .font(t.mono(TextRole::Caption))
                .color(t.theme.text2),
        );
        if matches!(mode, GapMode::PerSeries) {
            // Live, and it is the SAME out-slot the grid's own bulk bar fills: the keys go
            // to `tools::ToolView::stored_backfill`, which the shell drains into its
            // background backfill spawn. The only thing this button adds is not having to
            // tick 7 checkboxes to say "all of them".
            let fillable = backfillable(&gapped);
            let label = format!("Backfill all {}", gapped.len());
            let hover = format!(
                "Queue all {} gapped series. {fillable} will plan a job (kind=bar with an \
                         interval); the other {} are counted as skips, never silently dropped. \
                         Progress lands in the status line beside Refresh.",
                gapped.len(),
                gapped.len().saturating_sub(fillable),
            );
            let backfill = action(Some(icons::BACKFILL), &label);
            let backfill = if gapped.is_empty() {
                backfill
                    .disabled_because("No series carries a known hole — there is nothing to fill")
            } else {
                backfill
            };
            if ui.add(backfill).on_hover_text(hover).clicked() {
                tv.stored_backfill.extend(gapped.iter().cloned());
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(mode_id, mode));
    match mode {
        GapMode::PerSeries => {
            legend(
                ui,
                &format!(
                    "{} series carry a hole, {ranges} range(s) in total. A series that \
                             merely starts late is NOT a gap — every range here is missing from \
                             inside an otherwise covered span.",
                    gapped.len(),
                ),
            );
            section::strip_rule(ui);
            tv.stored_grid.active_view = vike_data_manager::ViewFilter::HasGaps;
            super::stored_tool_content(ui, ctx, tv);
        }
        GapMode::CrossKind => partial_days_list(ui, ctx),
    }
}

/// The Stale destination: the one screen whose threshold is a moving target, so its strip states
/// the tree window and the cut-off it judged against.
fn data_stale(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView, t: &Tokens) {
    use egui::RichText;
    use egui::vec2;
    // ===== Stale — the one screen whose threshold is a MOVING TARGET =====
    //
    // `vike_data_manager::is_stale` is `behind / span > 0.35` against the tree-wide window,
    // so the cut-off is a property of the TREE and not of the calendar: it moves every time
    // the newest row anywhere in the store moves. A screen that showed only the rows and
    // not the date it judged them against would be read as a fixed "older than X" list,
    // and tomorrow's answer would quietly be a different one. So the strip states the
    // window and the resulting cut-off, computed the same way the filter below computes
    // its rows.
    let (gfirst, glast) = vike_data_manager::global_span(ctx.stored.tree);
    let stale = stale_series_keys(ctx.stored.tree, gfirst, glast);
    // ⚠ The pressed segment is DERIVED from the grid's live sort, never stored beside it:
    // the grid's own column headers also write `GridState::sort`, and a value neither
    // preset names renders both segments unpressed — which is the truth. Storing the pick
    // separately would leave a segment lit while the grid sorted by something else.
    let mut picked =
        STALE_SORTS.iter().map(|s| s.value).find(|v| *v == Some(tv.stored_grid.sort)).flatten();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);
        if segmented::segmented(ui, &mut picked, &STALE_SORTS)
            && let Some(sort) = picked
        {
            // Written ONLY on a click: writing every frame would undo a header sort.
            tv.stored_grid.sort = sort;
        }
        ui.label(RichText::new("tree window").font(t.font(TextRole::Caption)).color(t.theme.text3));
        ui.label(
            RichText::new(span_label(gfirst, glast))
                .font(t.mono(TextRole::Caption))
                .color(t.theme.text2),
        );
        let fillable = backfillable(&stale);
        let label = format!("Update all {}", stale.len());
        let hover = format!(
            "Queue all {} stale series. {fillable} will plan a job (kind=bar with an \
                     interval); the other {} are counted as skips, never silently dropped. \
                     Progress lands in the status line beside Refresh.",
            stale.len(),
            stale.len().saturating_sub(fillable),
        );
        let update = action(Some(icons::BACKFILL), &label);
        let update = if stale.is_empty() {
            update.disabled_because(
                "Nothing is stale against this tree window — there is nothing to update",
            )
        } else {
            update
        };
        if ui.add(update).on_hover_text(hover).clicked() {
            tv.stored_backfill.extend(stale.iter().cloned());
        }
    });
    legend(
        ui,
        &match stale_cutoff_ms(gfirst, glast) {
            Some(cut) => format!(
                "{} of {} series are stale: last row before {} (35% of this {}-day window, \
                         and the date moves as the tree does).",
                stale.len(),
                data_rail::RailCounts::from_ctx(ctx).series,
                vike_model::time::epoch_ms_to_utc_date(cut),
                (glast - gfirst) / 86_400_000,
            ),
            None => "The tree spans no time yet, so nothing can be judged stale.".to_string(),
        },
    );
    warning_legend(
        ui,
        "The sort applies WITHIN each venue block — the shared grid groups its rows by \
                 venue before sorting them, so `oldest first` is oldest-first per venue and not \
                 one global order.",
    );
    section::strip_rule(ui);
    tv.stored_grid.active_view = vike_data_manager::ViewFilter::Stale;
    super::stored_tool_content(ui, ctx, tv);
}

/// The Providers destination: the three source planes behind a segmented control, then the
/// Polymarket egress proxy box.
fn data_providers(ui: &mut egui::Ui, ctx: &ToolCtx<'_>, tv: &mut tools::ToolView, t: &Tokens) {
    use egui::vec2;
    // ===== Providers — the merge of three tabs that rendered ONE list =====
    //
    // `Historical Providers`, `Event Providers` and `Streaming Providers` were three top-level
    // tabs sharing a single trailing `else` arm, so all three printed this same hardcoded list
    // of seven names. Three tabs, one list, and no way to tell from the outside — which is why
    // they are one destination and this comment exists.
    //
    // ⚠ **The three tab NAMES survive as the segmented control below, and they now mean
    // something.** The list they used to share was seven hardcoded venue names with one
    // marked `(live)` — it answered none of the three tabs' questions, because a venue is
    // not a source: the same venue answers a historical backfill, a live socket and no
    // event feed at all, through three different things. Each segment therefore renders
    // the sources of ITS plane, with whatever state this window can actually observe for
    // them — and where it can observe none, it says so instead of colouring a dot.
    //
    // ⚠ **What this screen deliberately does NOT render is the design's capability MATRIX**
    // (source × store kind, lit where that source serves that kind). The authority for it
    // is `crates/vike-datahub/src/backfill.rs`'s `KLINE_SOURCES`, and `crate::data::
    // backfill_plan`'s module doc argues at length why this crate has no `vike-datahub`
    // edge and must not grow one: reaching it would drag the six kline bridge crates its
    // rows name into the GUI's tree to feed a table nothing here decides with. Guessing
    // the matrix locally would be worse than omitting it — a client-side roster that
    // disagrees with the server's reads as a capability the operator does not have, or
    // hides one they do.
    let plane_id = ui.id().with("dm_providers_plane");
    let mut plane: ProviderPlane =
        ui.data_mut(|d| d.get_temp::<ProviderPlane>(plane_id)).unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(t.metrics.gap, t.metrics.gap);
        segmented::segmented(ui, &mut plane, &PROVIDER_SEGMENTS);
        dead_action(
            ui,
            None,
            "+ Add source",
            "A source is not a row an operator adds: each one is a linked collector or a \
                     configured endpoint, and which exist is decided by the build and by the \
                     daemon this window talks to. Nothing here can register one.",
        );
    });
    ui.add_space(space::SM);
    match plane {
        ProviderPlane::Historical => providers_historical(ui, ctx),
        ProviderPlane::Event => providers_event(ui, ctx),
        ProviderPlane::Streaming => providers_streaming(ui, ctx),
    }
    ui.data_mut(|d| d.insert_temp(plane_id, plane));

    // The Polymarket egress proxy, moved here from the stored grid's body. This is the
    // destination for "where can data come from, and why is none arriving" — and for
    // Polymarket the usual answer is geo-blocking rather than anything about the store.
    //
    // ⚠ Its old site carried a hard-won constraint: it had to sit ABOVE the stored body's
    // loading early-return, or it vanished on exactly the screen an operator reaches it
    // from — an empty or still-loading store. This destination has no early return at all,
    // so the property holds by construction rather than by remembering it.
    section::strip_rule(ui);
    if let Some(value) = vike_data_manager::polymarket_proxy_ui(ui, &mut tv.stored_proxy) {
        tv.stored_proxy_save = Some(value);
    }
}

/// Providers, Historical plane: where a BACKFILL draws history from.
fn providers_historical(ui: &mut egui::Ui, ctx: &ToolCtx<'_>) {
    legend(
        ui,
        "Where a BACKFILL draws history from. What each one can serve is the \
                         server's answer, not this window's — see the note below.",
    );
    ui.add_space(space::SM);
    // The store this window is reading is the one fact it holds first-hand.
    // `StoredCtx::delete_unavailable` is the signal, and it is exactly the right
    // one for THIS screen rather than a convenient proxy: `crate::data::stored_mode`'s
    // `stored_mode` sets it on — and only on — the `BackfillRoute::Wire` arm, so
    // the value that grays Delete is the same value that decides which route a
    // backfill takes. A Historical-sources screen asking "is the datahub the one
    // answering" is asking that question directly.
    let remote = ctx.stored.delete_unavailable.is_some();
    source_row(
        ui,
        if remote { Status::Ok.color() } else { Status::Muted.color() },
        "datahub",
        "the wire backfill verb",
        if remote {
            "answering — this grid is a REMOTE store"
        } else {
            "not in use — this grid is the LOCAL store"
        },
    );
    let held = data_rail::RailCounts::from_ctx(ctx);
    source_row(
        ui,
        if remote { Status::Muted.color() } else { Status::Ok.color() },
        "local store",
        "already-held history",
        &format!("{} series across {} venues, read from disk", held.series, held.venues),
    );
    source_row(
        ui,
        Status::Muted.color(),
        "venue REST",
        "kind=bar klines",
        "reached THROUGH the route above, never directly from this window",
    );
    ui.add_space(space::MD);
    legend(
        ui,
        "The one client-side gate a backfill applies is kind=bar with an interval; \
                         every other selected series is counted as a skip. WHICH venues have a \
                         collector is the server's roster, and an off-roster venue is still sent \
                         so the refusal that comes back names the server's own supported set.",
    );
}

/// Providers, Event plane: where the Calendar and News tools fetch from.
fn providers_event(ui: &mut egui::Ui, ctx: &ToolCtx<'_>) {
    legend(
        ui,
        "Where the Calendar and News tools fetch from. Each status below is that \
                         fetcher's OWN last line, not a probe this screen ran.",
    );
    ui.add_space(space::SM);
    // Every row is real: the two status strings are what the background fetchers
    // published, and the three counts are the rows they landed. A fetcher that has
    // not run yet publishes an empty string, which renders as "not fetched yet"
    // rather than as a failure — the two are different states and look different.
    source_row(
        ui,
        fetch_dot(&ctx.td.cal_status),
        "forexfactory",
        "economic calendar",
        &fetch_state(&ctx.td.cal_status),
    );
    source_row(
        ui,
        fetch_dot(&ctx.td.news_status),
        "rss",
        "news headlines",
        &fetch_state(&ctx.td.news_status),
    );
    source_row(
        ui,
        count_dot(ctx.td.cal_earnings.len()),
        "finnhub",
        "earnings calendar",
        // ⚠ The provider-key NAMES are deliberately not spelled here. An
        // env-shaped literal in a `src/` file is harvested by
        // `crates/vike-ops/tests/settings_secrets/settings_registry.rs`, which then judges its
        // LAYER from the file it found it in — so a mention in a render body can
        // score a row `Library` and trip that gate's ratchet, for a string nothing
        // reads. `crate::tools`' own `FINNHUB_API_KEY`/`FMP_API_KEY` constants are
        // where those names live; the Connections tool is where a key's presence
        // is actually shown.
        &count_state(ctx.td.cal_earnings.len(), "needs a Finnhub key"),
    );
    source_row(
        ui,
        count_dot(ctx.td.cal_dividends.len()),
        "fmp",
        "dividends calendar",
        &count_state(ctx.td.cal_dividends.len(), "needs an FMP key"),
    );
    source_row(
        ui,
        count_dot(ctx.td.cal_ipos.len()),
        "nasdaq",
        "IPO calendar",
        &count_state(ctx.td.cal_ipos.len(), "keyless"),
    );
    ui.add_space(space::MD);
    legend(
        ui,
        "An absent provider key blanks that one row and nothing else — the keyless \
                         fetches still land, and no venue credential is involved in any of them.",
    );
}

/// Providers, Streaming plane: where a live tick arrives from.
fn providers_streaming(ui: &mut egui::Ui, ctx: &ToolCtx<'_>) {
    legend(
        ui,
        "Where a live tick arrives from. One row per venue that has actually \
                         REGISTERED a feed-status handle with this process.",
    );
    ui.add_space(space::SM);
    // ⚠ A venue with no producer is ABSENT from `feed_statuses` and is therefore
    // absent here too — it is deliberately not listed as `Unknown`. `Unknown` is a
    // state a producer can publish (a line this window's parser cannot classify);
    // "no producer registered" is a different fact, and rendering the second as
    // the first invents a stream that was never opened. The count line below is
    // what states the absence, once, in terms of the roster.
    let mut venues: Vec<(String, String)> = ctx
        .feed_statuses
        .iter()
        .map(|(venue, handle)| (venue.clone(), handle.lock().unwrap().clone()))
        .collect();
    venues.sort_by(|a, b| a.0.cmp(&b.0));
    if venues.is_empty() {
        state::view(
            ui,
            Load::Empty(
                "No venue has registered a feed-status handle in this process — \
                                 nothing is streaming, and nothing is claimed to be.",
            ),
        );
    }
    for (venue, status) in &venues {
        source_row(
            ui,
            crate::ui::status_dot::feed_dot_color(status),
            venue,
            "live market data",
            if status.trim().is_empty() { "—" } else { status.as_str() },
        );
    }
    ui.add_space(space::MD);
    legend(
        ui,
        &format!(
            "{} venue(s) publish a status line; {} of {} roster venues publish \
                             none, so they are absent above rather than shown as Unknown. {} bar \
                             feed(s) are cached right now — the Cached feeds destination lists \
                             them.",
            venues.len(),
            vike_model::VENUES.len().saturating_sub(venues.len()),
            vike_model::VENUES.len(),
            ctx.feeds.len(),
        ),
    );
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

// ===== The Cached-feeds action set =====
//
// The design's ten feed-plane verbs, split in TWO because the one that is live — `Remove feed` —
// sits seventh in the design's order and a single loop cannot render a live button in the middle
// of a dead run. Splitting the array is the smaller evil: the alternative is an `Option<&str>` per
// row where `None` means "this one is real", which puts the interesting fact in the absence of a
// string. Keep the two halves in the design's order; the join point is the live verb.
//
// ⚠ Each `why` names the thing that DOES NOT EXIST, never "not implemented yet". The reader of a
// disabled-hover is deciding whether to go looking for another way to do it, and "no verb exists"
// and "this window cannot reach the verb" send them to different places.

/// Design order, positions 1–6: icon, words, why. `Add feed` keeps its ASCII `+` in the words and
/// takes no registry icon, as the Store screen's `+ Mount store…` does.
const FEED_ACTIONS_BEFORE_REMOVE: [(Option<Icon>, &str, &str); 6] = [
    (
        Some(icons::PAUSE),
        "Pause",
        "No suspend state exists. `crate::ui::feed_lifecycle`'s model has a series subscribed or not \
         subscribed, and a paused socket is neither — nothing would hold the gap it left.",
    ),
    (
        Some(icons::RUN),
        "Resume",
        "The twin of Pause: with nothing suspended there is nothing to resume. Reopening a chart \
         on the series re-subscribes it, which is the whole of the restart path today.",
    ),
    (
        Some(icons::RESTART),
        "Restart",
        "No single verb does it. `crate::ui::feed_lifecycle`'s reaper frees a subscription's slot only \
         once no window references it, so a restart is Remove feed plus reopening the chart.",
    ),
    (
        Some(icons::RECONNECT),
        "Reconnect",
        "The bridge reconnects itself — the WS pump owns its own retry — and nothing in this \
         window can poke that loop from outside it.",
    ),
    (
        Some(icons::CLEAR),
        "Drop cache",
        "The cached bars ARE the subscription's own ring; there is no verb that empties one \
         without stopping the feed, which is what Remove feed already does.",
    ),
    (
        None,
        "+ Add feed",
        "A feed is created by a chart window subscribing a series, and `tools::ToolView` carries no \
         out-slot for starting one from here. Open a chart on the series instead.",
    ),
];

/// Design order, positions 8–10 (position 7 is the live `Remove feed`): icon, words, why.
const FEED_ACTIONS_AFTER_REMOVE: [(Option<Icon>, &str, &str); 3] = [
    (
        Some(icons::RATE_LIMIT),
        "Rate limit",
        "No client-side throttle exists: the venue's own limits govern this stream, and a knob \
         here would suggest this window could change them.",
    ),
    (
        Some(icons::EXPORT),
        "Export",
        "Nothing in a tool body writes a file — the window has no out-slot for one — so there is \
         no path from this table to disk.",
    ),
    (
        Some(icons::BACKFILL),
        "Backfill",
        "These rows are LIVE feeds, not stored series. The bulk backfill plans over the stored \
         inventory's keys, which is the All-series, Has-gaps and Stale destinations.",
    ),
];

// ===== The pure folds the strips render =====
//
// Each is O(series) over the already-loaded tree and clones nothing but the keys it returns. That
// is deliberate rather than incidental: `vike_data_manager::filter_tree` — which
// `vike_data_manager::stored_catalog_grid` itself calls once per frame, and which any
// count-by-filtering would call a second time — DEEP-CLONES the whole tree. A strip that wanted a
// count that way would be paying a full clone of everything to render one number directly above a
// grid that had just paid for the same clone. `super::data_rail::RailCounts`' own doc makes the
// same argument from the rail's side, which is why there is no stale badge there at all.
//
// The predicates below are the same ones `filter_tree` applies, so the number on the strip and the
// rows in the grid cannot disagree.

/// Every series the `Stale` view will show, as the grid's own key type.
///
/// The predicate is `vike_data_manager::is_stale` against the tree-wide window, which is exactly
/// what `filter_tree`'s `Stale` arm applies per series — so `stale_series_keys(..).len()` equals
/// the row count the grid below the strip is about to render.
fn stale_series_keys(
    tree: &[vike_data_manager::model::VenueNode],
    global_first: i64,
    global_last: i64,
) -> Vec<SeriesKey> {
    let mut out = Vec::new();
    for v in tree {
        for sym in &v.symbols {
            for r in &sym.series {
                if vike_data_manager::is_stale(r.cov.last_ts, global_first, global_last) {
                    out.push(SeriesKey {
                        venue: v.venue.clone(),
                        symbol: sym.symbol.clone(),
                        kind: r.kind.clone(),
                        interval: r.interval.clone(),
                    });
                }
            }
        }
    }
    out
}

/// Every series the `HasGaps` view will show, as the grid's own key type.
///
/// ⚠ The predicate is a NON-EMPTY gap list, not mere presence in the map, and the difference is
/// why this is not `gaps.len()`. `vike_data_manager::GapMap` holds an entry for every series whose
/// gaps were FETCHED — a series fetched and found whole sits in the map with an empty vector — and
/// `filter_tree` keeps it only when `has_gaps` says the list is non-empty.
/// `super::data_rail::RailCounts::gaps` deliberately takes the cheap `len()` because a rail badge
/// may not walk the tree every frame; a strip standing directly above the rows it is counting may
/// not be approximate.
fn gapped_series_keys(
    tree: &[vike_data_manager::model::VenueNode],
    gaps: &vike_data_manager::GapMap,
) -> Vec<SeriesKey> {
    let mut out = Vec::new();
    for v in tree {
        for sym in &v.symbols {
            for r in &sym.series {
                let key = SeriesKey {
                    venue: v.venue.clone(),
                    symbol: sym.symbol.clone(),
                    kind: r.kind.clone(),
                    interval: r.interval.clone(),
                };
                if gaps.get(&key).is_some_and(|g| vike_data_manager::has_gaps(g)) {
                    out.push(key);
                }
            }
        }
    }
    out
}

/// How many of `keys` a bulk backfill would actually PLAN a job for, mirroring
/// `crate::data::backfill_plan::plan_backfill_jobs`'s one client-side gate: `kind == "bar"` AND an
/// interval to request it at. Everything else is counted as a skip by the planner, never silently
/// dropped, and this is the number that lets the button say so BEFORE it is clicked.
///
/// ⚠ **There is no venue term here, and the design mock's hover text says there is one.** That
/// mock reads "bulk backfill serves kind=bar on binance/bybit/okx only", which was true of this
/// tree until `backfill_plan`'s `SUPPORTED_BACKFILL_VENUES` was DELETED — its module doc carries
/// the argument: the roster is the SERVER's (`crates/vike-datahub/src/backfill.rs`'s
/// `KLINE_SOURCES`, folded into that file's own `real_backfill_table`), an off-roster venue is
/// still planned and sent, and the refusal that comes back names the server's own supported set.
/// A client-side venue gate would misreport a capable server as unsupporting, so this window must
/// not restate one — including in a tooltip.
fn backfillable(keys: &[SeriesKey]) -> usize {
    keys.iter().filter(|k| k.kind == "bar" && k.interval.is_some()).count()
}

/// The instant a series' last row must fall before to read as stale, given the tree-wide window —
/// `vike_data_manager::is_stale`'s `behind / span > 0.35` solved for `last_ts`.
///
/// Rendered on the Stale strip because the threshold is a RELATIVE judgement about the tree and
/// reads as a fixed calendar date: it moves every time the tree's newest row does, so the screen
/// states the date it is using rather than leaving an operator to infer one that will be wrong
/// tomorrow. A zero-width window (the empty tree) has no cut-off at all.
///
/// ⚠ **The arithmetic is integer where `is_stale`'s is `f64`, and that is deliberate.** `0.35` has
/// no exact binary representation, so `(span as f64 * 0.35) as i64` over a 100-day window lands a
/// millisecond off the true boundary and the constant wobbles with the span's magnitude — a thing
/// that cannot be asserted about and does not need to be, since this value is rendered at DAY
/// resolution. `* 35 / 100` in `i128` is exact for every span an epoch-ms window can hold. The two
/// can therefore disagree by under a millisecond about a series whose last row falls exactly on
/// the boundary; the FILTER is still `is_stale` alone, so what the grid shows is never decided
/// here — only the date printed above it.
fn stale_cutoff_ms(global_first: i64, global_last: i64) -> Option<i64> {
    if global_last <= global_first {
        return None;
    }
    let span = i128::from(global_last - global_first);
    Some(global_last - (span * 35 / 100) as i64)
}

// ===== The DataSet member-symbol derivations =====
//
// The same shape as `stale_series_keys`/`gapped_series_keys` directly above and for the same
// reason — the strip's numbers and the rows under it must be one derivation, not two — but asked
// of a DataSet's MEMBERSHIP rather than of the tree. A DataSet names symbols; the store holds
// series; these four functions are the whole of the join between them, and the `DataDest::DataSets`
// arm renders their output without repeating any of it.
//
// ⚠ They were block-local inside that arm when the member strip landed, which put them out of
// reach of the `#[cfg(test)]` module below. That is the only reason they are here: a derivation
// whose claims live solely in its doc comment is a claim nobody re-checks.

/// One member symbol and every STORED series it resolves to:
/// `(symbol, [(series key, last_row_ms)])`.
///
/// Named rather than spelled out at both sites that annotate one. Written literally the nesting
/// sits just under `clippy::type_complexity`'s default threshold — passing today, and close enough
/// that one more field in the tuple would redden a `-D warnings` lane for a reason no reader would
/// guess from the diff. It also reads better: the `i64` is a `last_ts`, and nothing in the bare
/// tuple says so.
type Member = (String, Vec<(SeriesKey, i64)>);

/// `true` when a DataSet's `provider` field names an actual venue rather than the `Auto` sentinel
/// the form's dropdown opens on.
///
/// ⚠ `Auto` is a UI sentinel and NOT a venue, so it may never reach a backfill key: the datahub
/// would be asked for a venue nobody has and would answer with a refusal naming its own roster,
/// which reads as "the server cannot do this" when the truth is that this window sent it a
/// non-name. Withholding the key and saying so on the button's disabled hover is the honest half of
/// the same trade `crate::data::backfill_plan`'s module doc makes about venue gates in general: do not
/// invent a client-side roster, but do not send something that is not a venue either.
fn provider_names_a_venue(provider: &str) -> bool {
    let p = provider.trim();
    !p.is_empty() && !p.eq_ignore_ascii_case("Auto")
}

/// Every STORED series one member symbol resolves to, as `(key, last_row_ms)`.
///
/// Scoped to the DataSet's provider when that names a venue, and to every venue when it is `Auto`
/// — which is what `Auto` means on the form the member list sits under.
///
/// ⚠ A GROUPED node is skipped, and `vike_data_manager::SymbolNode::grouped`'s own doc is the
/// argument: a grouped node's `symbol` field holds the GROUP NAME, not a symbol. Matching a member
/// against it is a false positive that would then be counted as stale on this screen and queued for
/// backfill under a name no venue trades. `crate::data::stored_load` records the same class of bug from
/// the load side, which is why `a_grouped_node_is_never_matched_as_a_member_symbol` exists rather
/// than the skip resting on this paragraph.
fn member_series(
    tree: &[vike_data_manager::model::VenueNode],
    provider: &str,
    symbol: &str,
) -> Vec<(SeriesKey, i64)> {
    let scoped = provider_names_a_venue(provider);
    let mut out = Vec::new();
    for v in tree {
        if scoped && !v.venue.eq_ignore_ascii_case(provider.trim()) {
            continue;
        }
        for sym in &v.symbols {
            if sym.grouped || !sym.symbol.eq_ignore_ascii_case(symbol) {
                continue;
            }
            for r in &sym.series {
                out.push((
                    SeriesKey {
                        venue: v.venue.clone(),
                        symbol: sym.symbol.clone(),
                        kind: r.kind.clone(),
                        interval: r.interval.clone(),
                    },
                    r.cov.last_ts,
                ));
            }
        }
    }
    out
}

/// Is this member behind? — ANY of its stored series stale against the tree-wide window, which is
/// `vike_data_manager::is_stale`: the predicate [`stale_series_keys`] applies per SERIES, asked
/// here per MEMBER.
///
/// ⚠ A member the store holds nothing for is NOT stale, and that is a claim rather than an
/// oversight: `is_stale` judges a `last_ts`, and a symbol with no rows has none to judge. It is a
/// different condition — never fetched, not fallen behind — so the list marks it `· not stored` and
/// the legend counts it separately, rather than letting `stale only` quietly mean two things at
/// once. `an_unstored_member_is_not_stale_because_it_has_no_last_ts_to_judge` is that claim as a
/// test; the empty-slice fold returns `false` by construction, and the test exists so a future
/// "treat unstored as maximally stale" edit has to argue with something.
fn member_is_stale(stored: &[(SeriesKey, i64)], gfirst: i64, glast: i64) -> bool {
    stored.iter().any(|(_, last)| vike_data_manager::is_stale(*last, gfirst, glast))
}

/// The keys a bulk backfill queues for ONE member symbol — store first, the set's own declaration
/// second, nothing third.
///
/// * **Stored series win.** They are real keys with real gap ranges, so
///   `crate::data::backfill_plan::plan_backfill_jobs` targets the holes rather than re-fetching a default
///   lookback window, and a non-`bar` one among them is counted as a skip by that planner — exactly
///   what the Has-gaps and Stale strips already rely on.
/// * **A member with nothing stored falls back to the SET'S OWN declaration** —
///   `(provider, interval)` at `kind = "bar"`, the only kind an interval can mean. This is the case
///   the two sibling strips never meet: they queue rows that are in the store by construction,
///   while a DataSet is a WISHLIST whose most useful day is the one where none of its symbols has
///   been fetched yet. The planner gives such a key a single default-lookback window, which is
///   precisely "go and get this".
/// * **Otherwise nothing**, and the caller counts the member as a skip: `Auto` names no venue (see
///   [`provider_names_a_venue`]) and an empty interval names no bar.
fn member_keys(
    stored: &[(SeriesKey, i64)],
    provider: &str,
    interval: &str,
    symbol: &str,
) -> Vec<SeriesKey> {
    if !stored.is_empty() {
        return stored.iter().map(|(k, _)| k.clone()).collect();
    }
    let iv = interval.trim();
    if provider_names_a_venue(provider) && !iv.is_empty() {
        return vec![SeriesKey {
            venue: provider.trim().to_string(),
            symbol: symbol.to_string(),
            kind: "bar".to_string(),
            interval: Some(iv.to_string()),
        }];
    }
    Vec::new()
}

/// The DataSet member list's columns. Every cell is text, so it is the kit's data table.
///
/// ⚠ The symbol column is Inter, not mono: the kit's only monospace column is right-aligned, and a
/// left-aligned mono column is on the PR's report of missing knobs (owner decision 3).
const MEMBER_COLUMNS: [Column<'static>; 2] = [
    Column {
        title: "Member",
        weight: data::DS_MEMBERS_MEMBER_WEIGHT,
        min_w: data::DS_MEMBERS_MEMBER_MIN_W,
        numeric: false,
    },
    Column {
        title: "In this store",
        weight: data::DS_MEMBERS_STORED_WEIGHT,
        min_w: data::DS_MEMBERS_STORED_MIN_W,
        numeric: false,
    },
];

/// One member-list cell: the symbol, or why it is marked. `stale` is the design's own marker. `not
/// stored` is this window's addition and earns its column: it is WHY such a member can never appear
/// under `stale only` — it holds nothing to be behind — and without it that absence looks like a
/// filter bug. Stale wins over empty, as the old list's suffixes did.
fn member_cell(m: &Member, col: usize, gfirst: i64, glast: i64) -> String {
    let (symbol, stored) = m;
    match col {
        0 => symbol.clone(),
        _ if member_is_stale(stored, gfirst, glast) => "stale".to_string(),
        _ if stored.is_empty() => "not stored".to_string(),
        _ => String::new(),
    }
}

/// Why Test symbol is dark: nothing names a symbol to open.
const NO_SYMBOL_TO_TEST: &str =
    "This DataSet names no symbol yet — type one under Symbols, or pick one in the list above.";
/// Why Test DataSet is ALWAYS dark: the window's one test slot (`tools::ToolView::ds_test`) opens a
/// chart for ONE symbol, and nothing opens a whole set.
const NO_DATASET_TEST: &str = "Nothing opens a whole DataSet — the one test this window can run opens \
                               a chart for a single symbol. Use Test symbol.";
/// Why Delete is dark: a set that was never saved has nothing on disk to delete.
const NOTHING_SAVED_TO_DELETE: &str =
    "This DataSet is not saved yet — there is nothing on disk to delete.";
/// Why Backfill is dark with no member shown.
const NO_MEMBERS_TO_QUEUE: &str = "This cut shows no member symbols, so there is nothing to queue.";

/// Which plane an activity-log line belongs to — the classification the log's segmented filter
/// runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogPlane {
    /// Live sockets: a subscription started, stopped, reconnected, or the catalogue refreshed.
    Feeds,
    /// A bulk backfill's planning and outcome.
    Backfill,
    /// The stored inventory: a load, a rollup, a delete, a gap or stale sweep.
    Store,
}

/// Classify one activity-log line.
///
/// ⚠ **This is a HEURISTIC over free text, and it is one because no producer tags its lines.**
/// `tools::ToolView::data_log` is a `Vec<String>` and both writers today push a bare sentence —
/// this module's Refresh, and `crates/vike-desktop/src/app_ui.rs`'s `data_delete` drain, which
/// logs the feed it stopped. The structural fix is a tag on the entry (a typed line, or a leading
/// marker every producer writes), which needs a change to that `Vec<String>` in
/// `crates/vike-app-core/src/tools.rs` and to the other producer — neither of which is this
/// module's to make. So the filter classifies what it can see, and this doc is the record of why
/// the seam is where it is.
///
/// ⚠ **A consequence worth stating rather than discovering: today every line reads `Feeds`.** Both
/// producers are feed-plane events, so `Backfill` and `Store` are empty filters until something
/// writes such a line — which is the honest answer, not a broken one. The needles below are
/// deliberately the vocabulary the stored/backfill side ALREADY uses in its status strings
/// (`crates/vike-app-core/src/ui/tool_views/stored.rs` renders `backfill_status`), so the classifier
/// is right the day one of them starts logging instead of needing to be revisited then.
///
/// Backfill is checked before Store because a backfill line routinely names the store it wrote to,
/// and "what did this do" is the more specific question of the two.
fn log_plane(line: &str) -> LogPlane {
    let l = line.to_ascii_lowercase();
    const BACKFILL: [&str; 5] = ["backfill", "backfilled", "queued", "appended", "unsupported"];
    const STORE: [&str; 8] =
        ["store", "deleted", "rollup", "gap", "stale", "scanned", "inventory", "truncat"];
    if BACKFILL.iter().any(|n| l.contains(n)) {
        LogPlane::Backfill
    } else if STORE.iter().any(|n| l.contains(n)) {
        LogPlane::Store
    } else {
        LogPlane::Feeds
    }
}

/// The log filter's segments, in render order. The first is "no filter" (`None`); the rest name
/// one [`LogPlane`] each. The segment carries its value, so no index maps one onto the other.
const LOG_SEGMENTS: [Segment<'static, Option<LogPlane>>; 4] = [
    Segment { value: None, label: "All", why: "Every line this session has produced" },
    Segment {
        value: Some(LogPlane::Feeds),
        label: "Feeds",
        why: "Live-socket events: a subscription started, stopped or the catalogue refreshed",
    },
    Segment {
        value: Some(LogPlane::Backfill),
        label: "Backfill",
        why: "Bulk backfill planning and outcomes — empty until a backfill logs a line",
    },
    Segment {
        value: Some(LogPlane::Store),
        label: "Store",
        why: "Stored-inventory work: loads, rollups, deletes, gap and stale sweeps",
    },
];

/// Which cut the Has-gaps destination is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum GapMode {
    /// A hole inside ONE series' own timeline — the grid at `ViewFilter::HasGaps`.
    #[default]
    PerSeries,
    /// A day on which an instrument has some of its kinds and not others — this module's own list
    /// over `StoredCtx::partials`, because the shared grid has no view for it.
    CrossKind,
}

/// The Has-gaps segments, in render order.
const GAP_SEGMENTS: [Segment<'static, GapMode>; 2] = [
    Segment {
        value: GapMode::PerSeries,
        label: "per-series gaps",
        why: "A missing day INSIDE one series' own covered span — the grid's HasGaps view",
    },
    Segment {
        value: GapMode::CrossKind,
        label: "cross-kind partial days",
        why: "A day where an instrument holds some of its kinds and not others — invisible per \
              series, because each series is contiguous on its own",
    },
];

/// Which cut of the SELECTED DataSet's member symbols is on screen.
///
/// ⚠ **This is not the DataSet list down the left-hand side.** `All`/`Binance`/`Dukascopy`/
/// `My DataSets` group the DataSet LIST — which sets you can see. This filters the SYMBOLS INSIDE
/// the one set that is open. Two controls, two rosters, and the design puts them on opposite halves
/// of the split for that reason.
///
/// It sits at module scope, rather than inside the DataSets arm where it was born, so
/// `every_segment_roster_offers_each_value_once_and_leads_with_the_default` can hold its roster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum MemberCut {
    /// Every symbol the set names.
    #[default]
    AllMembers,
    /// Only members carrying at least one STORED series that `vike_data_manager::is_stale` judges
    /// behind the tree-wide window.
    StaleOnly,
}

/// The DataSet member strip's segments, in render order.
const MEMBER_SEGMENTS: [Segment<'static, MemberCut>; 2] = [
    Segment {
        value: MemberCut::AllMembers,
        label: "all members",
        why: "Every symbol this DataSet names, whether or not the local store holds anything for it",
    },
    Segment {
        value: MemberCut::StaleOnly,
        label: "stale only",
        why: "Members whose stored series lag the tree-wide window by more than 35% — the same \
              `is_stale` cut the Stale destination applies, asked per member symbol",
    },
];

/// The Stale screen's two sort presets: each segment carries the state it writes.
///
/// ⚠ ONE roster, deliberately: a segment's label, hover and the preset it applies are one value,
/// so the preset a segment CLAIMS and the preset it APPLIES cannot drift. The states are written
/// straight into `vike_data_manager::GridState::sort`, which is a `pub` field the grid already
/// reads — no grid change is involved, and the grid's own column headers keep writing the same
/// field. The value is an `Option` so the live sort can match NEITHER preset: a `None` current value
/// renders both segments unpressed.
///
/// ⚠ `oldest first` is `Updated` ASCENDING because `cmp_flat_row` compares `cov.last_ts` for that
/// column, so ascending puts the smallest — the least recently written — first. `most rows first`
/// is `Rows` DESCENDING for the mirror-image reason. Getting either direction backwards produces a
/// screen that is wrong in exactly the way nobody checks, so the derivation is written down.
const STALE_SORTS: [Segment<'static, Option<vike_data_manager::SortState>>; 2] = [
    Segment {
        value: Some(vike_data_manager::SortState {
            column: vike_data_manager::SortColumn::Updated,
            ascending: true,
        }),
        label: "oldest first",
        why: "Least recently written series first — the ones furthest behind the tree",
    },
    Segment {
        value: Some(vike_data_manager::SortState {
            column: vike_data_manager::SortColumn::Rows,
            ascending: false,
        }),
        label: "most rows first",
        why: "Largest series first — what a re-fetch would cost the most to redo",
    },
];

/// `"2025-09-20 → 2026-09-15 · 360 d"` — the tree-wide window a strip is judging against, or the
/// honest absence of one. Both Has-gaps and Stale render it because both are relative judgements
/// about the tree, and neither is interpretable without knowing which tree.
fn span_label(global_first: i64, global_last: i64) -> String {
    if global_last <= global_first {
        return "no span yet".to_string();
    }
    format!(
        "{} → {} · {} d",
        vike_model::time::epoch_ms_to_utc_date(global_first),
        vike_model::time::epoch_ms_to_utc_date(global_last),
        (global_last - global_first) / 86_400_000,
    )
}

/// The Has-gaps destination's SECOND cut: one row per instrument holding a cross-kind partial day.
///
/// ⚠ **This is rendered here rather than through the shared grid because the shared grid has no
/// view for it, and giving it one is out of bounds.** `vike_data_manager::stored_catalog_grid` is
/// mounted by vike-studio against a narrower panel (this module's header carries the measurement),
/// and a `ViewFilter` variant for partial days would need a row identity the grid does not have:
/// the partial map is keyed per INSTRUMENT (`vike_data::InstrumentKey`), one level ABOVE the
/// per-series key every grid row carries. There is no series to select.
///
/// ⚠ **Every Fill here is disabled, and the reason is structural rather than "not built yet".**
/// `crate::data::backfill_plan::plan_backfill_jobs` needs a series' `interval` to request anything, and
/// a partial DAY names a kind with no interval at all — so this map cannot produce a job however
/// much backend existed behind it. These rows are EVIDENCE: they tell you a join over that window
/// would run on a kind that is not there, which is a thing to know before trusting a backtest, not
/// a button to press.
fn partial_days_list(ui: &mut egui::Ui, ctx: &ToolCtx<'_>) {
    let t = Tokens::of(ui.ctx());
    // The remote-mode disclosure first: a datahub that did not answer the cross-kind coverage verb
    // leaves this map EMPTY, and an empty list then reads as "nothing is partial" — the exact
    // false negative `StoredCtx::partials_note` exists to prevent. It is rendered above the list,
    // never beside it.
    if let Some(note) = ctx.stored.partials_note {
        let words =
            egui::RichText::new(note).font(t.font(TextRole::Body)).color(Status::Warning.color());
        ui.label(icons::WARNING.before(ui.style(), words));
    }
    let total_days: usize = ctx.stored.partials.values().map(Vec::len).sum();
    legend(
        ui,
        &format!(
            "{} instrument(s) hold a partial day, {total_days} day(s) in total. A partial day is \
             not a gap in any one series — every series involved is contiguous on its own.",
            ctx.stored.partials.len(),
        ),
    );
    section::strip_rule(ui);
    if ctx.stored.partials.is_empty() {
        state::view(
            ui,
            Load::Empty(
                "No instrument has a day where some kinds are present and others are not — or the \
                 coverage report has not landed yet, which the note above says when it applies.",
            ),
        );
        return;
    }
    // Each row holds a button, so each is the density's control height (spec §3.4).
    let row_h = t.metrics.control_h;
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("dm_partial_days").show(
        ui,
        |ui| {
            for (key, days) in ctx.stored.partials.iter() {
                if days.is_empty() {
                    continue;
                }
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(data::PARTIAL_W_SERIES, row_h),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new(format!("{} / {}", key.venue, key.label))
                                    .font(t.mono(TextRole::Body))
                                    .color(t.theme.text),
                            );
                            if key.grouped {
                                ui.label(
                                    egui::RichText::new("grouped")
                                        .font(t.font(TextRole::Caption))
                                        .color(t.theme.text3),
                                );
                            }
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(data::PARTIAL_W_SUMMARY, row_h),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            // `partial_days_label` is the shared crate's own one-line summary —
                            // "3 partial days (book, quote)" — so this screen and the grid's
                            // Partial column word the same fact identically.
                            ui.label(
                                egui::RichText::new(vike_data_manager::partial_days_label(
                                    ctx.stored.partials,
                                    key,
                                ))
                                .font(t.font(TextRole::Body))
                                .color(t.theme.text2),
                            );
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(data::PARTIAL_W_SPAN, row_h),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            let first = days.first().map(|d| d.start_ms()).unwrap_or(0);
                            let last = days.last().map(|d| d.start_ms()).unwrap_or(0);
                            let span = if days.len() == 1 {
                                vike_model::time::epoch_ms_to_utc_date(first)
                            } else {
                                format!(
                                    "{} … {}",
                                    vike_model::time::epoch_ms_to_utc_date(first),
                                    vike_model::time::epoch_ms_to_utc_date(last),
                                )
                            };
                            ui.label(
                                egui::RichText::new(span)
                                    .font(t.mono(TextRole::Caption))
                                    .color(t.theme.text3),
                            );
                        },
                    );
                    dead_action(
                        ui,
                        None,
                        "Fill",
                        "A partial day names a KIND with no interval, and the backfill planner \
                         needs a series' interval to request anything — so no job can be built \
                         from this row by any backend. A kind no venue-direct source serves (book \
                         above all) can only be re-recorded live.",
                    );
                });
            }
        },
    );
}

/// Which plane of source the Providers destination is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ProviderPlane {
    /// Where a BACKFILL can draw history from.
    #[default]
    Historical,
    /// Where the calendar and news tools fetch scheduled/published events from.
    Event,
    /// Where a live tick arrives from.
    Streaming,
}

/// The Providers segments, in render order — the three old tab NAMES, now meaning three different
/// answers rather than three routes to one list.
const PROVIDER_SEGMENTS: [Segment<'static, ProviderPlane>; 3] = [
    Segment {
        value: ProviderPlane::Historical,
        label: "Historical",
        why: "Where a backfill draws already-past data from",
    },
    Segment {
        value: ProviderPlane::Event,
        label: "Event",
        why: "Where the Calendar and News tools fetch scheduled and published events from",
    },
    Segment {
        value: ProviderPlane::Streaming,
        label: "Streaming",
        why: "Where a live tick arrives from, right now, in this process",
    },
];

/// One source row: a state dot, the source's name, what it serves, and what this window can say
/// about it right now.
///
/// Fixed columns rather than an `egui::Grid` because the three planes supply different numbers of
/// rows and a `Grid` sizes its columns per instance — so switching segments would shift the name
/// column sideways, which reads as a different table rather than a different cut of one.
fn source_row(ui: &mut egui::Ui, dot: egui::Color32, name: &str, serves: &str, state_line: &str) {
    let t = Tokens::of(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("\u{25CF}").font(t.font(TextRole::Caption)).color(dot));
        ui.allocate_ui_with_layout(
            egui::vec2(data::SOURCE_W_NAME, t.metrics.row_h),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(
                    egui::RichText::new(name).font(t.mono(TextRole::Body)).color(t.theme.text),
                );
            },
        );
        ui.allocate_ui_with_layout(
            egui::vec2(data::SOURCE_W_SERVES, t.metrics.row_h),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(
                    egui::RichText::new(serves).font(t.font(TextRole::Body)).color(t.theme.text2),
                );
            },
        );
        ui.label(
            egui::RichText::new(state_line).font(t.font(TextRole::Caption)).color(t.theme.text3),
        );
    });
}

/// Dot colour for a background fetcher's own published status line.
///
/// ⚠ An EMPTY line is `MUTED`, not a fault: a fetcher that has not run yet publishes nothing, and
/// the two states an operator must be able to tell apart on this screen are "it failed" and "it
/// has not happened". `crate::ui::status_dot::feed_dot_color` classifies the non-empty case through
/// the ONE shared parser, so a fetcher line and a feed line are never judged by two rules.
fn fetch_dot(status: &str) -> egui::Color32 {
    if status.trim().is_empty() {
        Status::Muted.color()
    } else {
        crate::ui::status_dot::feed_dot_color(status)
    }
}

/// The text beside [`fetch_dot`] — the fetcher's own line, or the honest absence of one.
fn fetch_state(status: &str) -> String {
    if status.trim().is_empty() {
        "not fetched yet this session".to_string()
    } else {
        status.to_string()
    }
}

/// Dot colour for a fetch whose only observable is HOW MANY ROWS it landed. Zero is `MUTED` for
/// exactly [`fetch_dot`]'s reason: an empty result and a failure look identical from here, so
/// neither is painted as the other.
fn count_dot(rows: usize) -> egui::Color32 {
    if rows > 0 { Status::Ok.color() } else { Status::Muted.color() }
}

/// The text beside [`count_dot`]: the row count, plus what that fetch needs in order to land one.
fn count_state(rows: usize, needs: &str) -> String {
    if rows > 0 {
        format!("{rows} rows held · {needs}")
    } else {
        format!("no rows held · {needs}")
    }
}

/// One heading of the DataSet tree: every set, the sets of one provider, or the user's own.
#[derive(Debug)]
struct DsGroup {
    /// The heading: the provider's name as the settings database spells it, else its key.
    label: String,
    /// `None` for "All" and "My DataSets"; the provider KEY a set names otherwise.
    provider: Option<String>,
    /// "My DataSets": the user's own sets.
    mine: bool,
}

impl DsGroup {
    /// Whether set `d` is listed under this heading.
    fn has(&self, d: &datasets::DataSet) -> bool {
        match &self.provider {
            Some(p) => d.provider == *p,
            None => !self.mine || d.user,
        }
    }
}

/// The DataSet tree's headings (I15(a) of the Trade window plan): "All", then one per provider the
/// sets actually name, by key, each spelled through [`super::venue_label`] — so the settings
/// database spells it, or the key does, and no venue list lives in this file — then "My DataSets".
/// `Auto` is the form's sentinel rather than a venue ([`provider_names_a_venue`]), so it heads no
/// group; its sets are under "All".
fn dataset_groups(
    sets: &[datasets::DataSet],
    directory: Option<&vike_tradehub_client::wire::WireDirectory>,
) -> Vec<DsGroup> {
    let mut keys: Vec<&str> =
        sets.iter().map(|d| d.provider.as_str()).filter(|p| provider_names_a_venue(p)).collect();
    keys.sort_unstable();
    keys.dedup();
    let fixed =
        |label: &str, mine: bool| DsGroup { label: label.to_string(), provider: None, mine };
    let providers = keys.into_iter().map(|k| DsGroup {
        label: super::venue_label(directory, k).to_string(),
        provider: Some(k.to_string()),
        mine: false,
    });
    std::iter::once(fixed("All", false))
        .chain(providers)
        .chain([fixed("My DataSets", true)])
        .collect()
}

/// Load a DataSet into the Symbols-tab editor working copy.
fn ds_load_form(tv: &mut tools::ToolView, d: &datasets::DataSet) {
    tv.ds_sel = Some(d.name.clone());
    tv.ds_name = d.name.clone();
    tv.ds_provider = d.provider.clone();
    tv.ds_interval = d.interval.clone();
    tv.ds_benchmark = d.benchmark.clone();
    tv.ds_symbols_text = d.symbols.join(", ");
    tv.ds_sym_sel = None;
}

/// Cached feeds' columns. Every cell is text, so it is the kit's data table. Bars and the two
/// bounds are numeric: mono and right-aligned.
const FEED_COLUMNS: [Column<'static>; 6] = [
    Column {
        title: "Symbol",
        weight: data::FEEDS_SYMBOL_WEIGHT,
        min_w: data::FEEDS_SYMBOL_MIN_W,
        numeric: false,
    },
    Column {
        title: "Timeframe",
        weight: data::FEEDS_TIMEFRAME_WEIGHT,
        min_w: data::FEEDS_TIMEFRAME_MIN_W,
        numeric: false,
    },
    Column {
        title: "Bars",
        weight: data::FEEDS_BARS_WEIGHT,
        min_w: data::FEEDS_BARS_MIN_W,
        numeric: true,
    },
    Column {
        title: "From",
        weight: data::FEEDS_FROM_WEIGHT,
        min_w: data::FEEDS_FROM_MIN_W,
        numeric: true,
    },
    Column {
        title: "To",
        weight: data::FEEDS_TO_WEIGHT,
        min_w: data::FEEDS_TO_MIN_W,
        numeric: true,
    },
    Column {
        title: "Source",
        weight: data::FEEDS_SOURCE_WEIGHT,
        min_w: data::FEEDS_SOURCE_MIN_W,
        numeric: false,
    },
];

/// One Cached-feeds cell. The key is the `[venue:]SYMBOL@interval` the chart windows subscribe
/// under (`crate::ui::workspace::series_key`). The Source column is the feed's own venue
/// (`crate::backend::venue_routing::venue_of_key`), spelled as the settings database spells it, else
/// its key (I15(b)); the Symbol column is the symbol alone.
fn feed_cell(
    feed: &(String, usize, i64, i64),
    col: usize,
    directory: Option<&vike_tradehub_client::wire::WireDirectory>,
) -> String {
    let (key, n, first, last) = feed;
    let (sym, tf) = key.split_once('@').unwrap_or((key.as_str(), ""));
    match col {
        0 => sym.split_once(':').map_or(sym, |(_, s)| s).to_string(),
        1 => tf.to_string(),
        2 => fmt_thousands(*n as f64).trim_end_matches(".00").to_string(),
        3 => fmt_series_dt(*first),
        4 => fmt_series_dt(*last),
        _ => {
            let venue = crate::backend::venue_routing::venue_of_key(key);
            super::venue_label(directory, venue).to_string()
        }
    }
}

/// Format a series-coverage epoch-millisecond bound for the Cached-Series table. Non-positive (the
/// "no coverage yet" sentinel the feed cache reports for an empty series) and un-representable
/// values render as an em-dash rather than a misleading 1970 timestamp. Always UTC — this column
/// pins the raw store bound, unlike the activity log, which stamps in the display timezone.
fn fmt_series_dt(ms: i64) -> String {
    if ms > 0 {
        chrono::DateTime::from_timestamp_millis(ms)
            .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default()
    } else {
        "—".into()
    }
}

#[path = "data_tests.rs"]
#[cfg(test)]
mod data_tests;
