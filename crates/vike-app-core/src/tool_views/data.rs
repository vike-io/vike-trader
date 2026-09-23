//! The Data-manager tool body — the Symbols (DataSet) editor, the Cached-Series catalog of live
//! feeds, the provider sub-tabs, and the Stored sub-tab (which delegates to
//! [`super::stored_tool_content`]). Moved verbatim from `vike-app`'s `main.rs` (tool-view
//! extraction batch 2); the adaptations are mechanical: the seven threaded read-only params now
//! arrive grouped in [`ToolCtx`], and the two table-layout closures that were pure arithmetic
//! ([`column_edges`], [`fmt_series_dt`]) became named functions so they finally get unit tests.
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
//! `crates/vike-studio/src/data_browser.rs` records as ~340px wide while the grid's fixed columns
//! already sum to ~690px — so a column added for this window clips that one, and no test covers
//! Studio's Data tab. Anything a strip wants that the grid cannot give it is either derived beside
//! the grid (the Stale sort writes `GridState::sort`, which is already a `pub` field) or rendered
//! as this module's own list instead (the cross-kind partial-day view).
//!
//! ⚠ **The strips' own state lives in egui's temp memory**, keyed off the body `Ui`'s id, and NOT
//! on [`tools::ToolView`]. A `ToolView` field is the natural home for per-window view state —
//! `stored_grid`, `data_sel` and the `ds_*` family all are — and this is a deliberate second-best:
//! the in-tree precedent is `vike_data_manager::stored_catalog_ui`, whose `SortState` rides the
//! same map, and `crates/vike-app-core/src/tool_views/fx_picker.rs`'s `fx_picker_popup` query box.
//! What it costs is that a filter resets to its default when egui evicts the entry, which is the
//! right trade for a filter and would be the wrong one for anything an operator would have to
//! re-enter.

use super::ToolCtx;
use super::data_rail::{self, DataDest};
use crate::tools;
use vike_data::datasets;
use vike_data_manager::SeriesKey;
use vike_ui_theme::fmt::fmt_thousands;
use vike_ui_theme::font;
use vike_ui_theme::palette;

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
/// `only_venue_arming_reads_the_credential_store` in that module holds it to exactly one destination.
#[must_use]
pub fn data_tab_reads_arming(tv: &tools::ToolView) -> bool {
    tv.data_dest.reads_arming()
}

/// The Data-manager tool body: the Cached-Series catalog + the DataSet Symbols editor + the
/// provider sub-tabs. [`DataDest::AllSeries`] and its two filtered siblings hand off to
/// [`super::stored_tool_content`], which reads the same [`ToolCtx`].
///
/// ⚠ **"Its two filtered siblings" is no longer the whole truth**, and the exception is the one
/// worth knowing: [`DataDest::HasGaps`] hands off only in its per-series mode. Its cross-kind cut
/// is rendered by [`partial_days_list`] instead, because the partial-day map is keyed one level
/// ABOVE a grid row and the shared grid has no view for it — that function's doc is the argument.
pub fn data_tool_content(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    arming: Option<&super::VenueArmingInputs>,
) {
    use egui::{Align, Layout, vec2};
    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);

    // The store every destination below is reading, stated once, above all of them. It belongs to
    // no destination — the same argument `connections.rs`'s `ambient_strip` makes for the live
    // connection — and until it existed the answer was discoverable only by noticing that Delete
    // had greyed out.
    let counts = data_rail::RailCounts::from_ctx(ctx);
    data_rail::store_bar(ui, ctx, &counts);
    ui.add_space(6.0);

    // ⚠ The footline is a BOTTOM PANEL, declared BEFORE the row below it, and that ordering is the
    // whole of why it reaches the screen.
    //
    // Two earlier attempts did not. The first appended it after the rail/body row, which claims
    // `ui.available_height()` — all of it — so the strip landed below the window's own floor;
    // `connections.rs`'s `strip_reservation` documents that identical defect. The second subtracted
    // a MEASURED reservation from the row's height, and it was still invisible: arithmetic against
    // `available_height()` does not reserve anything egui will honour, it just makes the row
    // shorter and leaves the leftover to whatever draws next. A panel reserves. It is also what
    // `crates/vike-app-core/src/workspace/state.rs` already says about the app's own status bar —
    // added before the central panel, for this reason.
    egui::Panel::bottom("dm_footline")
        .frame(
            egui::Frame::new()
                .fill(palette::SURFACE)
                .stroke(egui::Stroke::new(1.0, palette::BORDER))
                .inner_margin(egui::Margin::symmetric(9, 4)),
        )
        .show(ui, |ui| data_rail::footline(ui, ctx, &counts));

    let full = ui.available_width();
    let rail_w = data_rail::RAIL_W.min(full * 0.34);
    let body_h = ui.available_height();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(vec2(rail_w, body_h), Layout::top_down(Align::Min), |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            data_rail::rail(ui, &mut tv.data_dest, &counts);
        });
        ui.separator();
        // ⚠ The body is allocated the REMAINING width explicitly rather than letting egui infer it.
        // The shared grid's fixed columns sum to ~738px with the checkbox and Partial cells
        // (`crates/vike-studio/src/data_browser.rs` records the same number from the other mount),
        // so a body that silently inherits a narrow width clips cells instead of shrinking them.
        ui.allocate_ui_with_layout(
            vec2((full - rail_w - 14.0).max(120.0), body_h),
            Layout::top_down(Align::Min),
            |ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                data_rail::crumb(
                    ui,
                    tv.data_dest.label(),
                    &dest_summary(ctx, tv.data_dest, &counts),
                );
                data_body(ui, ctx, tv, arming);
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
        DataDest::VenueArming => "the ceiling, the credentials, and what the mount did".to_string(),
        DataDest::Instruments => {
            super::instruments::instruments_summary(&ctx.catalog.rows(), ctx.catalog.total())
        }
    }
}

/// One destination's body. An exhaustive `match` — the point of [`DataDest`].
///
/// ⚠ The predecessor was an `if/else` chain comparing `tv.data_subtab` against five values, THREE of
/// them bare literals (`== 0`, `== 1`, `== 5`; `crate::startup::DATA_SUBTAB_STORED` existed and was
/// not used at the Stored arm), ending in an `else` that indexed a `[&str; 7]` with the raw value —
/// so an out-of-range write panicked the window on render. Neither shape is expressible now.
fn data_body(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    arming: Option<&super::VenueArmingInputs>,
) {
    use egui::RichText;
    use egui::{Align, Align2, Color32, FontFamily, FontId, Layout, Sense, vec2};
    const ZEBRA: Color32 = Color32::from_rgba_premultiplied(33, 37, 44, 90);
    let (feeds, dsets) = (ctx.feeds, ctx.dsets);

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
        DataDest::Instruments => {
            // ⚠ The click NEVER fetches here. `instruments_screen` reports which venue was pressed
            // and `CatalogRefresh::request` spawns the venue REST call on its own thread, so the
            // frame returns while the socket is still open — the same shape as
            // `crates/vike-desktop/src/app_methods.rs`'s `spawn_backend_settings_fetch`. The result
            // lands in `rows()` on a later frame, woken by the repaint this passes in.
            let now = vike_model::now_ms();
            let rows = ctx.catalog.rows();
            if let Some(venue) =
                super::instruments::instruments_screen(ui, &rows, ctx.catalog.total(), now)
            {
                let ectx = ui.ctx().clone();
                // The refusal is already on screen (the button is disabled with its reason), so a
                // lost race between the disable and the click needs no second report.
                let _ = ctx.catalog.request(&venue, now, move || ectx.request_repaint());
            }
        }
        DataDest::Store => super::data_screens::store(ui, ctx),
        DataDest::CachedFeeds => {
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
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                if pill(ui, "↻ Refresh", true).clicked() {
                    let ts =
                        vike_chart::to_naive(chrono::Utc::now().timestamp_millis(), ctx.display_tz)
                            .map(|dt| dt.format("%H:%M:%S").to_string())
                            .unwrap_or_default();
                    tv.data_log.push(format!("{ts}  Refreshed · {} series", feeds.len()));
                }
                for (label, why) in FEED_ACTIONS_BEFORE_REMOVE {
                    dead_pill(ui, label, why);
                }
                // The design's `Remove feed`, in the design's position, LIVE — see the arm's
                // header comment. Selection-gated because it stops exactly one subscription.
                let can_stop = tv.data_sel.is_some();
                if pill(ui, "⨯ Remove feed", can_stop)
                    .on_hover_text("Stop the selected subscription; its cached bars go with it")
                    .on_disabled_hover_text(
                        "Pick a row first — this stops ONE subscription, the one selected",
                    )
                    .clicked()
                {
                    tv.data_delete = tv.data_sel.take();
                }
                for (label, why) in FEED_ACTIONS_AFTER_REMOVE {
                    dead_pill(ui, label, why);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{} series", feeds.len()))
                            .size(11.0)
                            .color(palette::TEXT3),
                    );
                });
            });
            legend(
                ui,
                "A feed is started by a chart window subscribing a series, never from this \
                 catalogue — so the one lifecycle verb here is the one that ends a subscription.",
            );
            ui.add_space(4.0);

            // columns: Symbol | Timeframe | Bars | From | To | Source
            let headers = ["Symbol", "Timeframe", "Bars", "From", "To", "Source"];
            let weights = [1.2, 0.9, 0.8, 1.6, 1.6, 1.0];
            let rights = [false, false, true, false, false, false];
            let mono_c = [false, false, true, true, true, false];
            let full = ui.available_width();
            let edges = column_edges(full, &weights);
            let put = |p: &egui::Painter,
                       rect: egui::Rect,
                       i: usize,
                       txt: &str,
                       col: Color32,
                       edges: &[f32]| {
                let f = if mono_c[i] {
                    FontId::new(13.0, FontFamily::Monospace)
                } else {
                    FontId::new(13.0, FontFamily::Proportional)
                };
                if rights[i] {
                    p.text(
                        egui::pos2(rect.left() + edges[i + 1] - 6.0, rect.center().y),
                        Align2::RIGHT_CENTER,
                        txt,
                        f,
                        col,
                    );
                } else {
                    p.text(
                        egui::pos2(rect.left() + edges[i] + 2.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        txt,
                        f,
                        col,
                    );
                }
            };
            // header
            let (hrect, _) = ui.allocate_exact_size(vec2(full, 22.0), Sense::hover());
            {
                let p = ui.painter();
                for (i, h) in headers.iter().enumerate() {
                    put(p, hrect, i, h, palette::TEXT3, &edges);
                }
                p.hline(hrect.x_range(), hrect.bottom(), egui::Stroke::new(1.0, palette::BORDER));
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("data_tbl").show(
                ui,
                |ui| {
                    if feeds.is_empty() {
                        ui.add_space(8.0);
                        ui.weak("No cached series — open a chart to start a feed.");
                    }
                    for (ri, (key, n, first, last)) in feeds.iter().enumerate() {
                        let (sym, tf) = key.split_once('@').unwrap_or((key.as_str(), ""));
                        let sel = tv.data_sel.as_deref() == Some(key.as_str());
                        let (rect, resp) = ui.allocate_exact_size(vec2(full, 22.0), Sense::click());
                        let p = ui.painter();
                        if sel {
                            p.rect_filled(rect, 0.0, palette::SURFACE);
                        } else if !ri.is_multiple_of(2) {
                            p.rect_filled(rect, 0.0, ZEBRA);
                        }
                        let bars = fmt_thousands(*n as f64).trim_end_matches(".00").to_string();
                        let vals = [
                            sym.to_string(),
                            tf.to_string(),
                            bars,
                            fmt_series_dt(*first),
                            fmt_series_dt(*last),
                            "Binance".to_string(),
                        ];
                        for (i, v) in vals.iter().enumerate() {
                            put(
                                p,
                                rect,
                                i,
                                v,
                                if i == 0 { palette::TEXT } else { palette::TEXT2 },
                                &edges,
                            );
                        }
                        if resp.clicked() {
                            tv.data_sel = Some(key.clone());
                        }
                    }
                },
            );
        }
        DataDest::ActivityLog => {
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
            let mut find: String =
                ui.data_mut(|d| d.get_temp::<String>(find_id)).unwrap_or_default();
            let mut clear = false;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                if let Some(i) = segmented(ui, &LOG_SEGMENTS, Some(log_filter_index(filter))) {
                    filter = log_filter_of_index(i);
                }
                ui.label(RichText::new("Find").size(11.0).color(palette::TEXT3));
                ui.add(
                    egui::TextEdit::singleline(&mut find)
                        .desired_width(180.0)
                        .hint_text("substring, case-insensitive"),
                );
                dead_pill(
                    ui,
                    "⤓ Export",
                    "Nothing in a tool body writes a file — the window carries no out-slot for one \
                     — so this log reaches disk only through the JSON file sink, which is a \
                     different sink with its own level.",
                );
                if pill(ui, "🧹 Clear", !tv.data_log.is_empty())
                    .on_hover_text(
                        "Drop every line — this log lives in memory and nothing else holds a copy",
                    )
                    .on_disabled_hover_text("The log is already empty")
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
            ui.label(font::bold("ACTIVITY LOG").size(10.0).color(palette::TEXT3));
            legend(ui, &format!("{} of {} lines shown", shown.len(), tv.data_log.len()));
            ui.add_space(2.0);
            egui::Frame::new()
                .fill(palette::SURFACE)
                .stroke(egui::Stroke::new(1.0, palette::BORDER))
                .corner_radius(4.0)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .id_salt("data_log")
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            ui.set_min_height(54.0);
                            if tv.data_log.is_empty() {
                                ui.label(
                                    RichText::new(format!(
                                        "Ready — {} live feeds (Binance WebSocket).",
                                        feeds.len()
                                    ))
                                    .monospace()
                                    .size(11.0)
                                    .color(palette::TEXT2),
                                );
                            } else if shown.is_empty() {
                                ui.label(
                                    RichText::new(
                                        "No line matches this filter — the log is not empty.",
                                    )
                                    .monospace()
                                    .size(11.0)
                                    .color(palette::TEXT3),
                                );
                            }
                            for line in &shown {
                                ui.label(
                                    RichText::new(*line)
                                        .monospace()
                                        .size(11.0)
                                        .color(palette::TEXT2),
                                );
                            }
                        });
                });
            ui.add_space(6.0);
            ui.label(
            RichText::new(
                "This is the in-memory session log — it is not persisted and is not the JSON file \
                 log, whose level is VIKE_LOG_FILE_LEVEL.",
            )
            .size(11.0)
            .color(palette::TEXT3),
        );
        }
        DataDest::DataSets => {
            // ===== DataSets — the DataSet tree + editor (the old "Symbols" tab) =====
            //
            // The member-symbol strip's parts sit at MODULE scope with their siblings —
            // `MemberCut`/`MEMBER_SEGMENTS`/`MEMBER_CUTS` beside `GapMode`, and
            // `Member`/`provider_names_a_venue`/`member_series`/`member_is_stale`/`member_keys`
            // beside `stale_series_keys` — so this arm holds RENDERING and no derivation.
            //
            // ⚠ They were declared block-local INSIDE this arm first, and what the move bought is
            // worth recording, because it is exactly what the locality cost: the `#[cfg(test)]`
            // module at the bottom of this file can now REACH them. The roster gate
            // (`every_segment_roster_is_the_inverse_of_its_enum_discriminants`) covers `MemberCut`
            // instead of the enum/array pairing being held by nothing but the two declarations
            // sitting next to each other, and every derivation has tests where it previously had a
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
            let group_has = |g: &str, d: &datasets::DataSet| match g {
                "All" => true,
                "Binance" => d.provider == "binance",
                "Dukascopy" => d.provider == "dukascopy",
                _ => d.user, // My DataSets
            };
            let full = ui.available_width();
            let tree_w = (full * 0.26).clamp(170.0, 290.0);
            ui.horizontal_top(|ui| {
                // ----- LEFT: + New DataSet + grouped tree -----
                ui.allocate_ui_with_layout(
                    vec2(tree_w, ui.available_height()),
                    Layout::top_down(Align::Min),
                    |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    font::semibold("＋ New DataSet")
                                        .color(palette::TEXT)
                                        .size(13.0),
                                )
                                .fill(palette::SURFACE)
                                .stroke(egui::Stroke::new(1.0, palette::BORDER))
                                .min_size(vec2(tree_w - 6.0, 32.0)),
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
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical().id_salt("ds_tree").show(ui, |ui| {
                            for g in ["All", "Binance", "Dukascopy", "My DataSets"] {
                                ui.label(font::semibold(g).size(13.0).color(palette::TEXT));
                                for d in dsets.sets.iter().filter(|d| group_has(g, d)) {
                                    let on = tv.ds_name == d.name;
                                    if ui
                                        .selectable_label(
                                            on,
                                            RichText::new(format!("    {}", d.name))
                                                .size(13.0)
                                                .color(if on {
                                                    palette::TEXT
                                                } else {
                                                    palette::TEXT2
                                                }),
                                        )
                                        .clicked()
                                    {
                                        ds_load_form(tv, d);
                                    }
                                }
                                ui.add_space(2.0);
                            }
                        });
                    },
                );
                ui.add_space(12.0);
                // ----- RIGHT: editor form -----
                ui.allocate_ui_with_layout(
                    vec2(full - tree_w - 16.0, ui.available_height()),
                    Layout::top_down(Align::Min),
                    |ui| {
                        let lbl_w = 92.0;
                        let row =
                            |ui: &mut egui::Ui,
                             label: &str,
                             body: &mut dyn FnMut(&mut egui::Ui)| {
                                ui.horizontal(|ui| {
                                    ui.allocate_ui_with_layout(
                                        vec2(lbl_w, 22.0),
                                        Layout::left_to_right(Align::Center),
                                        |ui| {
                                            ui.label(
                                                RichText::new(label)
                                                    .size(13.0)
                                                    .color(palette::TEXT2),
                                            );
                                        },
                                    );
                                    body(ui);
                                });
                            };
                        row(ui, "Name", &mut |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut tv.ds_name).desired_width(260.0),
                            );
                        });
                        row(ui, "Provider", &mut |ui| {
                            let resp = ui.add(
                                egui::Button::new(
                                    RichText::new(tv.ds_provider.clone())
                                        .color(palette::TEXT)
                                        .size(13.0),
                                )
                                .fill(palette::SURFACE)
                                .stroke(egui::Stroke::new(1.0, palette::BORDER))
                                .min_size(vec2(200.0, 26.0)),
                            );
                            egui::Popup::menu(&resp).show(|ui| {
                                ui.set_min_width(196.0);
                                for p in [
                                    "Auto",
                                    "binance",
                                    "bybit",
                                    "okx",
                                    "coinbase",
                                    "kraken",
                                    "yahoo",
                                    "dukascopy",
                                ] {
                                    if ui.selectable_label(tv.ds_provider == p, p).clicked() {
                                        tv.ds_provider = p.to_string();
                                    }
                                }
                            });
                        });
                        row(ui, "Interval", &mut |ui| {
                            let resp = ui.add(
                                egui::Button::new(
                                    RichText::new(tv.ds_interval.clone())
                                        .color(palette::TEXT)
                                        .size(13.0),
                                )
                                .fill(palette::SURFACE)
                                .stroke(egui::Stroke::new(1.0, palette::BORDER))
                                .min_size(vec2(120.0, 26.0)),
                            );
                            egui::Popup::menu(&resp).show(|ui| {
                                ui.set_min_width(116.0);
                                for iv in
                                    ["1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "1d", "1w"]
                                {
                                    if ui.selectable_label(tv.ds_interval == iv, iv).clicked() {
                                        tv.ds_interval = iv.to_string();
                                    }
                                }
                            });
                        });
                        row(ui, "Benchmark", &mut |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut tv.ds_benchmark)
                                    .hint_text("optional, e.g. SPY / BTCUSDT (else equal-weight)")
                                    .desired_width(360.0),
                            );
                        });
                        ui.add_space(8.0);
                        // "Symbols in this DataSet — select one to Test" (read-only selectable list)
                        ui.label(
                            RichText::new("Symbols in this DataSet — select one to Test")
                                .size(13.0)
                                .color(palette::TEXT2),
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
                        let members: Vec<Member> = syms
                            .iter()
                            .map(|s| (s.clone(), member_series(ctx.stored.tree, &ds_provider, s)))
                            .collect();
                        let stale_n = members
                            .iter()
                            .filter(|(_, st)| member_is_stale(st, gfirst, glast))
                            .count();
                        let unstored_n = members.iter().filter(|(_, st)| st.is_empty()).count();
                        // ----- the design's member strip: `all members` / `stale only` -----
                        //
                        // ⚠ Rendered BEFORE the list is cut, so a click takes effect on the frame
                        // it happens on rather than the next one — the ordering `GapMode`'s strip
                        // uses, and the whole reason `shown` is computed below this block instead
                        // of above it. Reversed, the control lights up a segment while the rows
                        // below it still answer the other one, which reads as a stuck filter.
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                            if let Some(i) =
                                segmented(ui, &MEMBER_SEGMENTS, Some(member_cut as usize))
                            {
                                member_cut = MEMBER_CUTS[i];
                            }
                            // The window is on the strip for the reason the Stale screen states
                            // it: the cut-off is a property of the TREE, so it moves every time
                            // the newest row anywhere in the store does. A `stale only` list with
                            // no window beside it reads as a fixed calendar date.
                            ui.label(RichText::new("tree window").size(11.0).color(palette::TEXT3));
                            ui.label(
                                RichText::new(span_label(gfirst, glast))
                                    .monospace()
                                    .size(11.0)
                                    .color(palette::TEXT2),
                            );
                        });
                        let shown: Vec<&Member> = match member_cut {
                            MemberCut::AllMembers => members.iter().collect(),
                            MemberCut::StaleOnly => members
                                .iter()
                                .filter(|(_, st)| member_is_stale(st, gfirst, glast))
                                .collect(),
                        };
                        egui::Frame::new()
                            .fill(palette::SURFACE)
                            .stroke(egui::Stroke::new(1.0, palette::BORDER))
                            .corner_radius(4.0)
                            .inner_margin(6.0)
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .max_height(92.0)
                                    .id_salt("ds_syms")
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| {
                                        ui.set_min_height(72.0);
                                        if shown.is_empty() {
                                            // ⚠ TWO different emptinesses, said apart. "No
                                            // symbols" and "none of them is stale" send an
                                            // operator to different places, and one message
                                            // serving both would report an empty set when the
                                            // filter is what emptied it.
                                            ui.weak(if members.is_empty() {
                                                "  (no symbols yet)"
                                            } else {
                                                "  (no member is stale against this window)"
                                            });
                                        }
                                        for (s, stored) in &shown {
                                            let on = tv.ds_sym_sel.as_deref() == Some(s.as_str());
                                            // `· stale` is the design's own row marker.
                                            // `· not stored` is this window's addition and earns
                                            // its place: it is WHY such a member can never appear
                                            // under `stale only` — it holds nothing to be behind —
                                            // and without it that absence looks like a filter bug.
                                            let mark = if member_is_stale(stored, gfirst, glast) {
                                                "  · stale"
                                            } else if stored.is_empty() {
                                                "  · not stored"
                                            } else {
                                                ""
                                            };
                                            if ui
                                                .selectable_label(
                                                    on,
                                                    RichText::new(format!("{s}{mark}"))
                                                        .monospace()
                                                        .size(13.0)
                                                        .color(if on {
                                                            palette::TEXT
                                                        } else {
                                                            palette::TEXT2
                                                        }),
                                                )
                                                .clicked()
                                            {
                                                tv.ds_sym_sel = Some((*s).clone());
                                            }
                                        }
                                    });
                            });
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
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("Symbols (comma or newline separated)")
                                .size(13.0)
                                .color(palette::TEXT2),
                        );
                        ui.add(
                            egui::TextEdit::multiline(&mut tv.ds_symbols_text)
                                .hint_text("BTCUSDT, ETHUSDT, SOLUSDT…")
                                .desired_rows(4)
                                .desired_width(f32::INFINITY),
                        );
                        ui.add_space(8.0);
                        // Ask the AI
                        egui::Frame::new()
                            .stroke(egui::Stroke::new(1.0, palette::BORDER))
                            .corner_radius(4.0)
                            .inner_margin(8.0)
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new("Ask the AI").size(12.0).color(palette::TEXT2),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut tv.ds_ai_prompt)
                                        .hint_text("e.g. top 10 liquid crypto majors")
                                        .desired_width(f32::INFINITY),
                                );
                                if ui
                                    .add(
                                        egui::Button::new(
                                            font::semibold("Suggest").color(palette::TEXT),
                                        )
                                        .fill(palette::SURFACE)
                                        .stroke(egui::Stroke::new(1.0, palette::BORDER))
                                        .min_size(vec2(0.0, 28.0)),
                                    )
                                    .clicked()
                                {
                                    let s = datasets::suggest_symbols(&tv.ds_ai_prompt);
                                    if !s.is_empty() {
                                        tv.ds_symbols_text = s.join(", ");
                                    }
                                }
                            });
                        ui.add_space(10.0);
                        // Backfill N symbols / Save / Test symbol / Test DataSet / Delete
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
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
                            // ⚠ Deliberately NOT `pill`, this window's shared button shape, and
                            // the reason is local rather than a disagreement with it: `pill`
                            // floors at 28pt while every other button in THIS strip floors at
                            // 30pt, so borrowing it would paint one short button mid-row. The
                            // fill, the stroke and the disabled ink are `pill`'s.
                            //
                            // Per MEMBER first, then flattened — the two-step is what lets the
                            // hover distinguish a symbol that produced NO key at all from a series
                            // the planner will skip. Flattening straight to keys loses the former,
                            // and it is the one an operator can act on: it means the form above is
                            // incomplete, not that the venue is unsupported.
                            let per_member: Vec<Vec<SeriesKey>> = shown
                                .iter()
                                .map(|(s, stored)| {
                                    member_keys(stored, &ds_provider, &ds_interval, s)
                                })
                                .collect();
                            let keys: Vec<SeriesKey> =
                                per_member.iter().flatten().cloned().collect();
                            // The SAME client-side gate the two sibling strips print, through the
                            // same function: `kind == "bar"` with an interval, and no venue term.
                            // `backfillable`'s own doc carries why the design mock's "binance,
                            // bybit and okx only" line is stale and must not be restated — here
                            // included, and a tooltip counts.
                            let fillable = backfillable(&keys);
                            let no_key_n = per_member.iter().filter(|k| k.is_empty()).count();
                            let noun = if shown.len() == 1 { "symbol" } else { "symbols" };
                            let queueable = !keys.is_empty();
                            if ui
                                .add_enabled(
                                    queueable,
                                    egui::Button::new(
                                        font::semibold(format!(
                                            "⤓ Backfill {} {noun}",
                                            shown.len()
                                        ))
                                        .color(
                                            if queueable { palette::TEXT } else { palette::TEXT3 },
                                        ),
                                    )
                                    .fill(palette::SURFACE)
                                    .stroke(egui::Stroke::new(1.0, palette::BORDER))
                                    .min_size(vec2(0.0, 30.0)),
                                )
                                .on_hover_text(format!(
                                    "Queue the {} {noun} this cut shows — {} series in all. \
                                     {fillable} will plan a job (kind=bar with an interval); the \
                                     other {} are counted as skips, never silently dropped. \
                                     {no_key_n} of the {noun} shown resolve to no series at all \
                                     and are not queued. Progress lands in the status line beside \
                                     Refresh.",
                                    shown.len(),
                                    keys.len(),
                                    keys.len().saturating_sub(fillable),
                                ))
                                .on_disabled_hover_text(if shown.is_empty() {
                                    "This cut shows no member symbols, so there is nothing to \
                                     queue."
                                        .to_string()
                                } else {
                                    // ⚠ Names the two values rather than a verdict: the same
                                    // refusal covers `Auto` and a blank interval, and an operator
                                    // told only "cannot backfill" has to guess which of the two
                                    // fields above to go and fix.
                                    format!(
                                        "None of these {} {noun} resolves to a series to fetch. \
                                         This store holds nothing for them, and the set declares \
                                         Provider `{ds_provider}` at interval `{ds_interval}` — a \
                                         key needs a real venue AND an interval, and `Auto` names \
                                         no venue. Set both on the form above, or Refresh the \
                                         store.",
                                        shown.len(),
                                    )
                                })
                                .clicked()
                            {
                                tv.stored_backfill.extend(keys.iter().cloned());
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        font::semibold("💾 Save").color(palette::TEXT),
                                    )
                                    .fill(palette::SURFACE)
                                    .stroke(egui::Stroke::new(1.0, palette::BORDER))
                                    .min_size(vec2(0.0, 30.0)),
                                )
                                .clicked()
                            {
                                tv.ds_save = true;
                            }
                            // the list pick if any, else the first symbol in the editor
                            let test_sym = tv.ds_sym_sel.clone().or_else(|| {
                                datasets::parse_symbols(&tv.ds_symbols_text).into_iter().next()
                            });
                            let has_sym = test_sym.is_some();
                            if ui
                                .add_enabled(
                                    has_sym,
                                    egui::Button::new(RichText::new("▶ Test symbol").color(
                                        if has_sym { palette::TEXT } else { palette::TEXT3 },
                                    ))
                                    .fill(palette::SURFACE)
                                    .stroke(egui::Stroke::new(1.0, palette::BORDER))
                                    .min_size(vec2(0.0, 30.0)),
                                )
                                .clicked()
                            {
                                tv.ds_test = test_sym;
                            }
                            let _ = ui.add_enabled(
                                false,
                                egui::Button::new(
                                    RichText::new("▶ Test DataSet").color(palette::TEXT3),
                                )
                                .fill(palette::SURFACE)
                                .stroke(egui::Stroke::new(1.0, palette::BORDER))
                                .min_size(vec2(0.0, 30.0)),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let deletable = tv.ds_sel.is_some();
                                if ui
                                    .add_enabled(
                                        deletable,
                                        egui::Button::new(RichText::new("🗑 Delete").color(
                                            if deletable { palette::TEXT } else { palette::TEXT3 },
                                        ))
                                        .fill(palette::SURFACE)
                                        .stroke(egui::Stroke::new(1.0, palette::BORDER))
                                        .min_size(vec2(0.0, 30.0)),
                                    )
                                    .clicked()
                                {
                                    tv.ds_delete = tv.ds_sel.take();
                                    tv.ds_name.clear();
                                }
                            });
                        });
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
        DataDest::HasGaps => {
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
            let mut mode: GapMode =
                ui.data_mut(|d| d.get_temp::<GapMode>(mode_id)).unwrap_or_default();
            let gapped = gapped_series_keys(ctx.stored.tree, ctx.stored.gaps);
            let ranges: usize =
                gapped.iter().filter_map(|k| ctx.stored.gaps.get(k)).map(Vec::len).sum();
            let (gfirst, glast) = vike_data_manager::global_span(ctx.stored.tree);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                if let Some(i) = segmented(ui, &GAP_SEGMENTS, Some(mode as usize)) {
                    mode = GAP_MODES[i];
                }
                ui.label(RichText::new("Window").size(11.0).color(palette::TEXT3));
                ui.label(
                    RichText::new(span_label(gfirst, glast))
                        .monospace()
                        .size(11.0)
                        .color(palette::TEXT2),
                );
                if matches!(mode, GapMode::PerSeries) {
                    // Live, and it is the SAME out-slot the grid's own bulk bar fills: the keys go
                    // to `tools::ToolView::stored_backfill`, which the shell drains into its
                    // background backfill spawn. The only thing this button adds is not having to
                    // tick 7 checkboxes to say "all of them".
                    let fillable = backfillable(&gapped);
                    if pill(ui, &format!("⤓ Backfill all {}", gapped.len()), !gapped.is_empty())
                        .on_hover_text(format!(
                            "Queue all {} gapped series. {fillable} will plan a job (kind=bar with \
                             an interval); the other {} are counted as skips, never silently \
                             dropped. Progress lands in the status line beside Refresh.",
                            gapped.len(),
                            gapped.len().saturating_sub(fillable),
                        ))
                        .on_disabled_hover_text(
                            "No series carries a known hole — there is nothing to fill",
                        )
                        .clicked()
                    {
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
                    data_rail::strip_rule(ui);
                    tv.stored_grid.active_view = vike_data_manager::ViewFilter::HasGaps;
                    super::stored_tool_content(ui, ctx, tv);
                }
                GapMode::CrossKind => partial_days_list(ui, ctx),
            }
        }
        DataDest::Stale => {
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
            // ⚠ The pressed segment is DERIVED from the grid's live sort, never stored beside it.
            // `vike_data_manager::GridState::sort` is also written by the grid's own column
            // headers, so a header click can move it to a value neither preset names — and then
            // BOTH segments render unpressed, which is the truth. Storing the pick separately
            // would leave a segment lit while the grid sorted by something else.
            let pressed = STALE_SORTS.iter().position(|(_, _, s)| *s == tv.stored_grid.sort);
            // The segment labels and hovers are read back OUT of `STALE_SORTS` rather than
            // declared a second time, so the preset a segment claims and the preset it applies
            // cannot drift apart — there is one roster, not a pair of parallel arrays.
            let segs: [(&str, &str); 2] =
                [(STALE_SORTS[0].0, STALE_SORTS[0].1), (STALE_SORTS[1].0, STALE_SORTS[1].1)];
            let mut sort_pick = None;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                sort_pick = segmented(ui, &segs, pressed);
                ui.label(RichText::new("tree window").size(11.0).color(palette::TEXT3));
                ui.label(
                    RichText::new(span_label(gfirst, glast))
                        .monospace()
                        .size(11.0)
                        .color(palette::TEXT2),
                );
                let fillable = backfillable(&stale);
                if pill(ui, &format!("⟳ Update all {}", stale.len()), !stale.is_empty())
                    .on_hover_text(format!(
                        "Queue all {} stale series. {fillable} will plan a job (kind=bar with an \
                         interval); the other {} are counted as skips, never silently dropped. \
                         Progress lands in the status line beside Refresh.",
                        stale.len(),
                        stale.len().saturating_sub(fillable),
                    ))
                    .on_disabled_hover_text(
                        "Nothing is stale against this tree window — there is nothing to update",
                    )
                    .clicked()
                {
                    tv.stored_backfill.extend(stale.iter().cloned());
                }
            });
            if let Some(i) = sort_pick {
                // Written ONLY on a click — see the `pressed` comment above. Writing it every
                // frame would silently undo a column-header sort on the very next frame.
                tv.stored_grid.sort = STALE_SORTS[i].2;
            }
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
                    None => {
                        "The tree spans no time yet, so nothing can be judged stale.".to_string()
                    }
                },
            );
            legend(
                ui,
                "⚠ The sort applies WITHIN each venue block — the shared grid groups its rows by \
                 venue before sorting them, so `oldest first` is oldest-first per venue and not \
                 one global order.",
            );
            data_rail::strip_rule(ui);
            tv.stored_grid.active_view = vike_data_manager::ViewFilter::Stale;
            super::stored_tool_content(ui, ctx, tv);
        }
        DataDest::VenueArming => {
            // ===== Venues — per-venue arming (the `policy.venues.<venue>` ceiling + its switch) =====
            //
            // ⚠ The change journal and the timestamp come off `ctx.credentials`, which is NOT a misuse
            // of the credential channel: that field carries the ledger and the instant the binary's ONE
            // boot walk produced (`vike_connections::CredentialWrite`), and this write records into the
            // same ledger under a `set_setting` kind. The credential STORE path in that struct is not
            // read here — a policy write touches no credential at all.
            super::venues_tab_content(
                ui,
                arming,
                &mut tv.arm_edit,
                ctx.credentials.journal,
                ctx.credentials.now_ms,
            );
        }
        DataDest::Providers => {
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
            // is `vike_backfill::kline_source::KLINE_SOURCES`, and `crate::backfill_plan`'s module
            // doc argues at length why this crate has no `vike-backfill` edge and must not grow
            // one: layer 80 reaching layer 55 would drag `venue-backfill`'s bridge crates into the
            // GUI's tree to feed a table nothing here decides with. Guessing the matrix locally
            // would be worse than omitting it — a client-side roster that disagrees with the
            // server's reads as a capability the operator does not have, or hides one they do.
            let plane_id = ui.id().with("dm_providers_plane");
            let mut plane: ProviderPlane =
                ui.data_mut(|d| d.get_temp::<ProviderPlane>(plane_id)).unwrap_or_default();
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                if let Some(i) = segmented(ui, &PROVIDER_SEGMENTS, Some(plane as usize)) {
                    plane = PROVIDER_PLANES[i];
                }
                dead_pill(
                    ui,
                    "＋ Add source",
                    "A source is not a row an operator adds: each one is a linked collector or a \
                     configured endpoint, and which exist is decided by the build and by the \
                     daemon this window talks to. Nothing here can register one.",
                );
            });
            ui.add_space(4.0);
            match plane {
                ProviderPlane::Historical => {
                    legend(
                        ui,
                        "Where a BACKFILL draws history from. What each one can serve is the \
                         server's answer, not this window's — see the note below.",
                    );
                    ui.add_space(4.0);
                    // The store this window is reading is the one fact it holds first-hand.
                    // `StoredCtx::delete_unavailable` is the signal, and it is exactly the right
                    // one for THIS screen rather than a convenient proxy: `crate::stored_mode`'s
                    // `stored_mode` sets it on — and only on — the `BackfillRoute::Wire` arm, so
                    // the value that grays Delete is the same value that decides which route a
                    // backfill takes. A Historical-sources screen asking "is the datahub the one
                    // answering" is asking that question directly.
                    let remote = ctx.stored.delete_unavailable.is_some();
                    source_row(
                        ui,
                        if remote { palette::status::CONNECTED } else { palette::status::MUTED },
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
                        if remote { palette::status::MUTED } else { palette::status::CONNECTED },
                        "local store",
                        "already-held history",
                        &format!(
                            "{} series across {} venues, read from disk",
                            held.series, held.venues
                        ),
                    );
                    source_row(
                        ui,
                        palette::status::MUTED,
                        "venue REST",
                        "kind=bar klines",
                        "reached THROUGH the route above, never directly from this window",
                    );
                    ui.add_space(6.0);
                    legend(
                        ui,
                        "The one client-side gate a backfill applies is kind=bar with an interval; \
                         every other selected series is counted as a skip. WHICH venues have a \
                         collector is the server's roster, and an off-roster venue is still sent \
                         so the refusal that comes back names the server's own supported set.",
                    );
                }
                ProviderPlane::Event => {
                    legend(
                        ui,
                        "Where the Calendar and News tools fetch from. Each status below is that \
                         fetcher's OWN last line, not a probe this screen ran.",
                    );
                    ui.add_space(4.0);
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
                        // `crates/vike-ops/tests/settings_registry.rs`, which then judges its
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
                    ui.add_space(6.0);
                    legend(
                        ui,
                        "An absent provider key blanks that one row and nothing else — the keyless \
                         fetches still land, and no venue credential is involved in any of them.",
                    );
                }
                ProviderPlane::Streaming => {
                    legend(
                        ui,
                        "Where a live tick arrives from. One row per venue that has actually \
                         REGISTERED a feed-status handle with this process.",
                    );
                    ui.add_space(4.0);
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
                        ui.weak(
                            "No venue has registered a feed-status handle in this process — \
                             nothing is streaming, and nothing is claimed to be.",
                        );
                    }
                    for (venue, status) in &venues {
                        source_row(
                            ui,
                            crate::status_dot::feed_dot_color(status),
                            venue,
                            "live market data",
                            if status.trim().is_empty() { "—" } else { status.as_str() },
                        );
                    }
                    ui.add_space(6.0);
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
            data_rail::strip_rule(ui);
            if let Some(value) = vike_data_manager::polymarket_proxy_ui(ui, &mut tv.stored_proxy) {
                tv.stored_proxy_save = Some(value);
            }
        }
    }
}

// ===== Strip furniture — the two widget shapes every action strip is built from =====

/// One segmented control — the design's `.seg`: attached buttons, at most one pressed, no "off".
/// Returns the index that was clicked this frame, or `None`. Each option carries its own hover
/// text, because a two-word segment label cannot say what the cut actually is.
///
/// ⚠ `egui::SelectableLabel` does NOT exist in egui 0.36; `egui::Button::selectable` is what
/// replaced it. But that constructor also applies `frame_when_inactive(selected)`, which drops the
/// FRAME on every unpressed segment — and a segmented control whose unpressed halves have no
/// border is a row of floating words with one boxed word in it. The `.frame_when_inactive(true)`
/// below is what puts the border back, and it MUST come after the constructor or the
/// constructor's own call is the one that wins.
///
/// ⚠ `selected` is an `Option` rather than a plain index because the pressed segment is DERIVED
/// from live state wherever live state exists, instead of being stored a second time beside it.
/// The Stale sort reads `vike_data_manager::GridState::sort`, and the grid's own column headers
/// can move that to a value no segment names; `None` then renders every segment unpressed, which
/// is the honest picture of exactly that state rather than a lie about which cut is showing.
fn segmented(
    ui: &mut egui::Ui,
    options: &[(&str, &str)],
    selected: Option<usize>,
) -> Option<usize> {
    let mut picked = None;
    ui.horizontal(|ui| {
        // 1px rather than 0: egui draws each segment's own 1px stroke, so butting them flush would
        // paint two strokes on the shared edge and read as a heavier rule between the pair.
        ui.spacing_mut().item_spacing.x = 1.0;
        for (i, (label, why)) in options.iter().enumerate() {
            let on = selected == Some(i);
            let resp = ui
                .add(
                    egui::Button::selectable(
                        on,
                        egui::RichText::new(*label).size(11.5).color(if on {
                            palette::TEXT
                        } else {
                            palette::TEXT2
                        }),
                    )
                    .frame_when_inactive(true)
                    .fill(if on { palette::CARD } else { palette::SURFACE })
                    .stroke(egui::Stroke::new(
                        1.0,
                        if on { palette::ACCENT } else { palette::BORDER },
                    ))
                    .corner_radius(2.0)
                    .min_size(egui::vec2(0.0, 24.0)),
                )
                .on_hover_text(*why);
            if resp.clicked() {
                picked = Some(i);
            }
        }
    });
    picked
}

/// One framed action pill — this window's single button shape.
///
/// Promoted out of the `CachedFeeds` arm, where it was a closure, once four destinations needed
/// it. Unchanged otherwise: same fill, same stroke, same 28pt floor, same dim ink when disabled.
fn pill(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(
            egui::RichText::new(label)
                .color(if enabled { palette::TEXT } else { palette::TEXT3 })
                .size(13.0),
        )
        .fill(palette::SURFACE)
        .stroke(egui::Stroke::new(1.0, palette::BORDER))
        .min_size(egui::vec2(0.0, 28.0)),
    )
}

/// An action with nothing behind it, rendered the ONE honest way: disabled, with the reason on its
/// disabled-hover.
///
/// A live button that silently does nothing is the failure this shape exists to prevent — an
/// operator clicks it, nothing happens, and the window has told them neither that it refused nor
/// why. `why` is not decoration: it is the whole payload, so it names the thing that does not
/// exist rather than saying "not implemented".
fn dead_pill(ui: &mut egui::Ui, label: &str, why: &str) {
    let _ = pill(ui, label, false).on_disabled_hover_text(why);
}

/// A strip's trailing note — the design's `.legend`: small, dim, and the place a refusal rule or a
/// derivation gets stated once instead of hiding in ten tooltips.
fn legend(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(10.5).color(palette::TEXT3));
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

/// Design order, positions 1–6.
const FEED_ACTIONS_BEFORE_REMOVE: [(&str, &str); 6] = [
    (
        "⏸ Pause",
        "No suspend state exists. `crate::feed_lifecycle`'s model has a series subscribed or not \
         subscribed, and a paused socket is neither — nothing would hold the gap it left.",
    ),
    (
        "▶ Resume",
        "The twin of Pause: with nothing suspended there is nothing to resume. Reopening a chart \
         on the series re-subscribes it, which is the whole of the restart path today.",
    ),
    (
        "⟳ Restart",
        "No single verb does it. `crate::feed_lifecycle`'s reaper frees a subscription's slot only \
         once no window references it, so a restart is Remove feed plus reopening the chart.",
    ),
    (
        "⇄ Reconnect",
        "The bridge reconnects itself — the WS pump owns its own retry — and nothing in this \
         window can poke that loop from outside it.",
    ),
    (
        "🗑 Drop cache",
        "The cached bars ARE the subscription's own ring; there is no verb that empties one \
         without stopping the feed, which is what Remove feed already does.",
    ),
    (
        "＋ Add feed",
        "A feed is created by a chart window subscribing a series, and `tools::ToolView` carries no \
         out-slot for starting one from here. Open a chart on the series instead.",
    ),
];

/// Design order, positions 8–10 (position 7 is the live `Remove feed`).
const FEED_ACTIONS_AFTER_REMOVE: [(&str, &str); 3] = [
    (
        "⏱ Rate limit",
        "No client-side throttle exists: the venue's own limits govern this stream, and a knob \
         here would suggest this window could change them.",
    ),
    (
        "⤓ Export",
        "Nothing in a tool body writes a file — the window has no out-slot for one — so there is \
         no path from this table to disk.",
    ),
    (
        "⤒ Backfill",
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
    tree: &[crate::inventory::VenueNode],
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
    tree: &[crate::inventory::VenueNode],
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
/// `crate::backfill_plan::plan_backfill_jobs`'s one client-side gate: `kind == "bar"` AND an
/// interval to request it at. Everything else is counted as a skip by the planner, never silently
/// dropped, and this is the number that lets the button say so BEFORE it is clicked.
///
/// ⚠ **There is no venue term here, and the design mock's hover text says there is one.** That
/// mock reads "bulk backfill serves kind=bar on binance/bybit/okx only", which was true of this
/// tree until `backfill_plan`'s `SUPPORTED_BACKFILL_VENUES` was DELETED — its module doc carries
/// the argument: the roster is the SERVER's (`vike_backfill::kline_source::KLINE_SOURCES`, folded
/// into `crates/vike-datahub/src/backfill.rs`'s `real_backfill_table`), an off-roster venue is
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
/// the same trade `crate::backfill_plan`'s module doc makes about venue gates in general: do not
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
/// backfill under a name no venue trades. `crate::stored_load` records the same class of bug from
/// the load side, which is why `a_grouped_node_is_never_matched_as_a_member_symbol` exists rather
/// than the skip resting on this paragraph.
fn member_series(
    tree: &[crate::inventory::VenueNode],
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
///   `crate::backfill_plan::plan_backfill_jobs` targets the holes rather than re-fetching a default
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
/// (`crates/vike-app-core/src/tool_views/stored.rs` renders `backfill_status`), so the classifier
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

/// The log filter's segments, in render order. Index 0 is "no filter"; the rest map 1:1 onto
/// [`LogPlane`], and [`log_filter_of_index`] is the ONE place that mapping is spelled.
const LOG_SEGMENTS: [(&str, &str); 4] = [
    ("All", "Every line this session has produced"),
    ("Feeds", "Live-socket events: a subscription started, stopped or the catalogue refreshed"),
    ("Backfill", "Bulk backfill planning and outcomes — empty until a backfill logs a line"),
    ("Store", "Stored-inventory work: loads, rollups, deletes, gap and stale sweeps"),
];

/// Segment index → filter. `None` is "All".
fn log_filter_of_index(i: usize) -> Option<LogPlane> {
    match i {
        1 => Some(LogPlane::Feeds),
        2 => Some(LogPlane::Backfill),
        3 => Some(LogPlane::Store),
        _ => None,
    }
}

/// Filter → segment index, the exact inverse of [`log_filter_of_index`].
fn log_filter_index(f: Option<LogPlane>) -> usize {
    match f {
        None => 0,
        Some(LogPlane::Feeds) => 1,
        Some(LogPlane::Backfill) => 2,
        Some(LogPlane::Store) => 3,
    }
}

/// Which cut the Has-gaps destination is showing.
///
/// ⚠ The discriminants index [`GAP_SEGMENTS`] through `mode as usize`, and [`GAP_MODES`] is the
/// inverse — the same pairing [`ProviderPlane`] documents, for the same reason.
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
const GAP_SEGMENTS: [(&str, &str); 2] = [
    (
        "per-series gaps",
        "A missing day INSIDE one series' own covered span — the grid's HasGaps view",
    ),
    (
        "cross-kind partial days",
        "A day where an instrument holds some of its kinds and not others — invisible per series, \
         because each series is contiguous on its own",
    ),
];

/// Segment index → mode, the inverse of `mode as usize`.
const GAP_MODES: [GapMode; 2] = [GapMode::PerSeries, GapMode::CrossKind];

/// Which cut of the SELECTED DataSet's member symbols is on screen.
///
/// ⚠ **This is not the group tabs down the left-hand tree.** `All`/`Binance`/`Dukascopy`/
/// `My DataSets` filter the DataSet LIST — which sets you can see. This filters the SYMBOLS INSIDE
/// the one set that is open. Two controls, two rosters, and the design puts them on opposite halves
/// of the split for that reason.
///
/// ⚠ The discriminants index [`MEMBER_SEGMENTS`] through `cut as usize`, and [`MEMBER_CUTS`] is the
/// inverse — the same pairing [`GapMode`] and [`ProviderPlane`] document, for the same reason. It
/// was held by nothing but proximity while this enum was block-local inside the DataSets arm;
/// `every_segment_roster_is_the_inverse_of_its_enum_discriminants` covers it now, which is what
/// promoting it to module scope was for.
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
const MEMBER_SEGMENTS: [(&str, &str); 2] = [
    (
        "all members",
        "Every symbol this DataSet names, whether or not the local store holds anything for it",
    ),
    (
        "stale only",
        "Members whose stored series lag the tree-wide window by more than 35% — the same \
         `is_stale` cut the Stale destination applies, asked per member symbol",
    ),
];

/// Segment index → cut, the inverse of `cut as usize`.
const MEMBER_CUTS: [MemberCut; 2] = [MemberCut::AllMembers, MemberCut::StaleOnly];

/// The Stale screen's two sort presets: `(label, hover, the state it writes)`.
///
/// ⚠ ONE roster, deliberately: the strip reads the labels back out of this array rather than
/// declaring them again beside it, so the preset a segment CLAIMS and the preset it APPLIES cannot
/// drift. The states are written straight into `vike_data_manager::GridState::sort`, which is a
/// `pub` field the grid already reads — no grid change is involved, and the grid's own column
/// headers keep writing the same field.
///
/// ⚠ `oldest first` is `Updated` ASCENDING because `cmp_flat_row` compares `cov.last_ts` for that
/// column, so ascending puts the smallest — the least recently written — first. `most rows first`
/// is `Rows` DESCENDING for the mirror-image reason. Getting either direction backwards produces a
/// screen that is wrong in exactly the way nobody checks, so the derivation is written down.
const STALE_SORTS: [(&str, &str, vike_data_manager::SortState); 2] = [
    (
        "oldest first",
        "Least recently written series first — the ones furthest behind the tree",
        vike_data_manager::SortState {
            column: vike_data_manager::SortColumn::Updated,
            ascending: true,
        },
    ),
    (
        "most rows first",
        "Largest series first — what a re-fetch would cost the most to redo",
        vike_data_manager::SortState {
            column: vike_data_manager::SortColumn::Rows,
            ascending: false,
        },
    ),
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
/// `crate::backfill_plan::plan_backfill_jobs` needs a series' `interval` to request anything, and
/// a partial DAY names a kind with no interval at all — so this map cannot produce a job however
/// much backend existed behind it. These rows are EVIDENCE: they tell you a join over that window
/// would run on a kind that is not there, which is a thing to know before trusting a backtest, not
/// a button to press.
fn partial_days_list(ui: &mut egui::Ui, ctx: &ToolCtx<'_>) {
    // The remote-mode disclosure first: a datahub that did not answer the cross-kind coverage verb
    // leaves this map EMPTY, and an empty list then reads as "nothing is partial" — the exact
    // false negative `StoredCtx::partials_note` exists to prevent. It is rendered above the list,
    // never beside it.
    if let Some(note) = ctx.stored.partials_note {
        ui.label(egui::RichText::new(format!("⚠ {note}")).size(11.0).color(palette::WARN));
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
    data_rail::strip_rule(ui);
    if ctx.stored.partials.is_empty() {
        ui.weak(
            "No instrument has a day where some kinds are present and others are not — or the \
             coverage report has not landed yet, which the note above says when it applies.",
        );
        return;
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("dm_partial_days").show(
        ui,
        |ui| {
            for (key, days) in ctx.stored.partials.iter() {
                if days.is_empty() {
                    continue;
                }
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(260.0, 20.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(
                                egui::RichText::new(format!("{} / {}", key.venue, key.label))
                                    .monospace()
                                    .size(12.0)
                                    .color(palette::TEXT),
                            );
                            if key.grouped {
                                ui.label(
                                    egui::RichText::new("grouped").size(10.0).color(palette::TEXT3),
                                );
                            }
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(240.0, 20.0),
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
                                .size(11.5)
                                .color(palette::TEXT2),
                            );
                        },
                    );
                    ui.allocate_ui_with_layout(
                        egui::vec2(170.0, 20.0),
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
                                    .monospace()
                                    .size(11.0)
                                    .color(palette::TEXT3),
                            );
                        },
                    );
                    dead_pill(
                        ui,
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
///
/// ⚠ The discriminants are load-bearing: the arm casts with `plane as usize` to index
/// [`PROVIDER_SEGMENTS`], and [`PROVIDER_PLANES`] is the inverse. Reordering the variants
/// therefore reorders the segments, which is fine — reordering ONE of the two arrays without the
/// other is what would break, so they sit together and are declared in this order.
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
const PROVIDER_SEGMENTS: [(&str, &str); 3] = [
    ("Historical", "Where a backfill draws already-past data from"),
    ("Event", "Where the Calendar and News tools fetch scheduled and published events from"),
    ("Streaming", "Where a live tick arrives from, right now, in this process"),
];

/// Segment index → plane, the inverse of `plane as usize`. See [`ProviderPlane`]'s doc.
const PROVIDER_PLANES: [ProviderPlane; 3] =
    [ProviderPlane::Historical, ProviderPlane::Event, ProviderPlane::Streaming];

/// One source row: a state dot, the source's name, what it serves, and what this window can say
/// about it right now.
///
/// Fixed columns rather than an `egui::Grid` because the three planes supply different numbers of
/// rows and a `Grid` sizes its columns per instance — so switching segments would shift the name
/// column sideways, which reads as a different table rather than a different cut of one.
fn source_row(ui: &mut egui::Ui, dot: egui::Color32, name: &str, serves: &str, state: &str) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("\u{25CF}").size(9.0).color(dot));
        ui.allocate_ui_with_layout(
            egui::vec2(116.0, 18.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(egui::RichText::new(name).monospace().size(12.0).color(palette::TEXT));
            },
        );
        ui.allocate_ui_with_layout(
            egui::vec2(168.0, 18.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(egui::RichText::new(serves).size(11.5).color(palette::TEXT2));
            },
        );
        ui.label(egui::RichText::new(state).size(11.0).color(palette::TEXT3));
    });
}

/// Dot colour for a background fetcher's own published status line.
///
/// ⚠ An EMPTY line is `MUTED`, not a fault: a fetcher that has not run yet publishes nothing, and
/// the two states an operator must be able to tell apart on this screen are "it failed" and "it
/// has not happened". `crate::status_dot::feed_dot_color` classifies the non-empty case through
/// the ONE shared parser, so a fetcher line and a feed line are never judged by two rules.
fn fetch_dot(status: &str) -> egui::Color32 {
    if status.trim().is_empty() {
        palette::status::MUTED
    } else {
        crate::status_dot::feed_dot_color(status)
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
    if rows > 0 { palette::status::CONNECTED } else { palette::status::MUTED }
}

/// The text beside [`count_dot`]: the row count, plus what that fetch needs in order to land one.
fn count_state(rows: usize, needs: &str) -> String {
    if rows > 0 {
        format!("{rows} rows held · {needs}")
    } else {
        format!("no rows held · {needs}")
    }
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

/// Cumulative x-offsets of a weighted table's columns, in the painter's local space: `edges[i]` is
/// column `i`'s left edge and `edges[i + 1]` its right edge, so the returned vec is one longer than
/// `weights`. The available width loses a fixed 8pt of right-hand slack before it is split, so the
/// last column never paints flush against the scrollbar. Degenerate inputs (empty `weights`, a
/// narrower-than-slack `full`) produce a `[0.0]`/negative-width layout rather than a panic — egui
/// simply paints nothing readable, which is what the pre-extraction closure did too.
fn column_edges(full: f32, weights: &[f32]) -> Vec<f32> {
    let total: f32 = weights.iter().sum();
    let unit = (full - 8.0) / total;
    let mut edges = Vec::with_capacity(weights.len() + 1);
    edges.push(0.0_f32);
    let mut acc = 0.0_f32;
    for w in weights {
        acc += w * unit;
        edges.push(acc);
    }
    edges
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

#[cfg(test)]
mod tests {
    use super::{
        GAP_MODES, GAP_SEGMENTS, GapMode, LOG_SEGMENTS, LogPlane, MEMBER_CUTS, MEMBER_SEGMENTS,
        MemberCut, PROVIDER_PLANES, PROVIDER_SEGMENTS, ProviderPlane, STALE_SORTS, backfillable,
        column_edges, ds_load_form, fmt_series_dt, gapped_series_keys, log_filter_index,
        log_filter_of_index, log_plane, member_is_stale, member_keys, member_series,
        provider_names_a_venue, span_label, stale_cutoff_ms, stale_series_keys,
    };
    use crate::inventory::{SeriesRow, SymbolNode, VenueNode};
    use crate::tools::ToolView;
    use vike_data::SeriesCoverage;
    use vike_data::datasets::DataSet;
    use vike_data_manager::{GapMap, SeriesKey, SortColumn};

    /// The exact weights the Cached-Series header uses — the layout under test.
    const WEIGHTS: [f32; 6] = [1.2, 0.9, 0.8, 1.6, 1.6, 1.0];

    /// One day in epoch-ms — every timestamp below is built from it so the arithmetic in the
    /// assertions is readable rather than a wall of thirteen-digit literals.
    const DAY: i64 = 86_400_000;

    /// A one-venue tree: `binance` with two symbols, each carrying one series whose `last_ts` is
    /// the caller's. `first_ts` is pinned at day 0 so the tree-wide window is the caller's to set
    /// through the LATEST row.
    fn tree(rows: &[(&str, &str, Option<&str>, i64)]) -> Vec<VenueNode> {
        let symbols: Vec<SymbolNode> = rows
            .iter()
            .map(|(symbol, kind, interval, last_day)| SymbolNode {
                symbol: (*symbol).to_string(),
                grouped: false,
                series: vec![SeriesRow {
                    kind: (*kind).to_string(),
                    interval: interval.map(str::to_string),
                    cov: SeriesCoverage {
                        first_ts: 0,
                        last_ts: last_day * DAY,
                        ..Default::default()
                    },
                }],
                total: Default::default(),
            })
            .collect();
        vec![VenueNode { venue: "binance".to_string(), symbols, total: Default::default() }]
    }

    /// One planted row for [`member_tree`]: venue, symbol, grouped, kind, interval, last-day index.
    ///
    /// ⚠ A `type` rather than the tuple spelled at the parameter: `clippy::type_complexity` counts
    /// six components as too many to read at a call site, and `-D warnings` makes that a build
    /// failure rather than advice. The sibling `Member` alias exists for the same lint one screen
    /// up — this one was written as a bare tuple and the lane was the first thing to say so.
    type MemberRow<'a> = (&'a str, &'a str, bool, &'a str, Option<&'a str>, i64);

    /// A MULTI-venue tree for the member-symbol derivations: one row per SERIES, as
    /// `(venue, symbol, grouped, kind, interval, last_day)`, folded into venue → symbol nodes in
    /// the order given.
    ///
    /// ⚠ `tree` above cannot serve these and the reason is the whole point of the tests below: it
    /// builds exactly ONE venue, always named `binance`, with one series per symbol and
    /// `grouped: false` on every node — and the provider scoping, the `Auto` span and the
    fn member_tree(rows: &[MemberRow<'_>]) -> Vec<VenueNode> {
        let mut venues: Vec<VenueNode> = Vec::new();
        for (venue, symbol, grouped, kind, interval, last_day) in rows {
            let series = SeriesRow {
                kind: (*kind).to_string(),
                interval: interval.map(str::to_string),
                cov: SeriesCoverage { first_ts: 0, last_ts: last_day * DAY, ..Default::default() },
            };
            // ⚠ Indices, not `iter_mut().find()`: the borrow that a `find` holds over the match
            // scrutinee extends into the `None` arm that wants to push, which NLL rejects. A
            // `position` returns a plain `usize` and holds nothing.
            let vi = match venues.iter().position(|v| v.venue == *venue) {
                Some(i) => i,
                None => {
                    venues.push(VenueNode {
                        venue: (*venue).to_string(),
                        symbols: Vec::new(),
                        total: Default::default(),
                    });
                    venues.len() - 1
                }
            };
            let v = &mut venues[vi];
            // Keyed on (label, GROUPED) exactly as `build_tree` keys it — a grouped node and a
            // per-symbol one may share a label and are different instruments.
            let si =
                match v.symbols.iter().position(|s| s.symbol == *symbol && s.grouped == *grouped) {
                    Some(i) => i,
                    None => {
                        v.symbols.push(SymbolNode {
                            symbol: (*symbol).to_string(),
                            grouped: *grouped,
                            series: Vec::new(),
                            total: Default::default(),
                        });
                        v.symbols.len() - 1
                    }
                };
            v.symbols[si].series.push(series);
        }
        venues
    }

    fn key(symbol: &str, kind: &str, interval: Option<&str>) -> SeriesKey {
        SeriesKey {
            venue: "binance".to_string(),
            symbol: symbol.to_string(),
            kind: kind.to_string(),
            interval: interval.map(str::to_string),
        }
    }

    fn dataset() -> DataSet {
        DataSet {
            name: "Majors".to_string(),
            provider: "binance".to_string(),
            interval: "5m".to_string(),
            benchmark: "BTCUSDT".to_string(),
            symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
            user: true,
        }
    }

    #[test]
    fn column_edges_span_the_available_width_minus_slack() {
        let edges = column_edges(708.0, &WEIGHTS);
        // One more edge than columns: [left of col 0, .., right of col 5].
        assert_eq!(edges.len(), WEIGHTS.len() + 1);
        assert_eq!(edges[0], 0.0);
        // The last edge is the full width less the fixed 8pt right-hand slack.
        assert!((edges[6] - 700.0).abs() < 1e-3, "last edge = {}", edges[6]);
        // Monotonic, and each column's width is proportional to its weight.
        for pair in edges.windows(2) {
            assert!(pair[1] > pair[0]);
        }
        let w0 = edges[1] - edges[0];
        let w3 = edges[4] - edges[3];
        assert!((w3 / w0 - 1.6 / 1.2).abs() < 1e-4);
    }

    #[test]
    fn column_edges_tolerates_degenerate_inputs() {
        // No columns ⇒ just the origin edge (and no NaN leaking out of the 0-total divide).
        assert_eq!(column_edges(500.0, &[]), [0.0]);
        // Narrower than the slack ⇒ negative widths, but still one edge per boundary, no panic.
        let edges = column_edges(4.0, &WEIGHTS);
        assert_eq!(edges.len(), WEIGHTS.len() + 1);
        assert!(edges[6] < 0.0);
    }

    #[test]
    fn series_dt_renders_utc_and_dashes_the_empty_sentinel() {
        assert_eq!(fmt_series_dt(1_700_000_000_000), "2023-11-14 22:13");
        // The "no coverage" sentinel and anything before the epoch stay an em-dash.
        assert_eq!(fmt_series_dt(0), "—");
        assert_eq!(fmt_series_dt(-1), "—");
    }

    #[test]
    fn load_form_copies_the_dataset_into_the_editor_and_clears_the_symbol_pick() {
        let mut tv = ToolView { ds_sym_sel: Some("STALE".to_string()), ..Default::default() };
        ds_load_form(&mut tv, &dataset());
        assert_eq!(tv.ds_sel.as_deref(), Some("Majors"));
        assert_eq!(tv.ds_name, "Majors");
        assert_eq!(tv.ds_provider, "binance");
        assert_eq!(tv.ds_interval, "5m");
        assert_eq!(tv.ds_benchmark, "BTCUSDT");
        // Symbols land in the multiline editor as the comma-joined text the tab edits.
        assert_eq!(tv.ds_symbols_text, "BTCUSDT, ETHUSDT");
        // A previous tab's symbol pick must NOT survive a DataSet switch (it would "Test" a
        // symbol that is not in the newly-loaded set).
        assert_eq!(tv.ds_sym_sel, None);
    }

    // ===== The action strips' pure folds =====

    #[test]
    fn stale_keys_are_the_series_lagging_more_than_a_third_of_the_window() {
        // A 100-day window, so the 35% threshold falls at 35 days behind the newest row: a series
        // whose last row is at day 64 is 36 days behind and stale, one at day 66 is 34 and is not.
        // The window is passed EXPLICITLY rather than through `global_span`, which reads each
        // venue's rollup — the fixture leaves those at their defaults because the fold under test
        // walks the series rows and never looks at a rollup.
        let t = tree(&[
            ("BTCUSDT", "bar", Some("1m"), 100),
            ("ETHUSDT", "trade", None, 64),
            ("SOLUSDT", "bar", Some("1h"), 66),
        ]);
        let stale = stale_series_keys(&t, 0, 100 * DAY);
        assert_eq!(stale, vec![key("ETHUSDT", "trade", None)]);
        // The predicate is the shared crate's, so the two cannot disagree about a row.
        assert!(vike_data_manager::is_stale(64 * DAY, 0, 100 * DAY));
        assert!(!vike_data_manager::is_stale(66 * DAY, 0, 100 * DAY));
    }

    #[test]
    fn a_zero_width_window_makes_nothing_stale() {
        // An empty or single-instant tree: `global_span` answers `(0, 0)` and `is_stale` refuses a
        // window whose end is not after its start, so the screen must offer to update NOTHING
        // rather than every row it can see.
        let t = tree(&[("BTCUSDT", "bar", Some("1m"), 100)]);
        assert!(stale_series_keys(&t, 0, 0).is_empty());
        assert_eq!(stale_cutoff_ms(0, 0), None);
    }

    #[test]
    fn the_stale_cutoff_is_the_thirty_five_percent_point_of_the_window() {
        // 100 days wide ⇒ the cut-off sits 35 days before the newest row. This is the date the
        // strip prints, so it has to be the same arithmetic `is_stale` applies.
        assert_eq!(stale_cutoff_ms(0, 100 * DAY), Some(65 * DAY));
        // ...and a row exactly ON the cut-off is NOT stale (`is_stale` is a strict `>`), which is
        // why the legend says "before" rather than "on or before".
        assert!(!vike_data_manager::is_stale(65 * DAY, 0, 100 * DAY));
    }

    #[test]
    fn gapped_keys_skip_a_series_that_was_fetched_and_found_whole() {
        // ⚠ The distinction this test exists for: `GapMap` holds an entry for every series whose
        // gaps were FETCHED, so a whole series sits in the map with an EMPTY list. Counting the
        // map's length — which the rail badge does deliberately, for cost — would put that series
        // on a screen whose whole claim is that every row on it has a hole.
        let t = tree(&[
            ("BTCUSDT", "bar", Some("1m"), 100),
            ("ETHUSDT", "trade", None, 100),
            ("SOLUSDT", "quote", None, 100),
        ]);
        let mut gaps = GapMap::new();
        gaps.insert(key("BTCUSDT", "bar", Some("1m")), vec![(10 * DAY, 12 * DAY)]);
        gaps.insert(key("ETHUSDT", "trade", None), Vec::new());
        // SOLUSDT is absent from the map entirely — never fetched, treated as gap-free.
        assert_eq!(gaps.len(), 2, "the map itself holds two entries");
        assert_eq!(gapped_series_keys(&t, &gaps), vec![key("BTCUSDT", "bar", Some("1m"))]);
    }

    #[test]
    fn backfillable_is_bar_with_an_interval_and_carries_no_venue_term() {
        // Mirrors `crate::backfill_plan::plan_backfill_jobs`' ONE client-side gate. A bar series
        // with no interval cannot be requested at any resolution, so it is a skip like the rest.
        let keys = [
            key("BTCUSDT", "bar", Some("1m")),
            key("ETHUSDT", "bar", None),
            key("SOLUSDT", "trade", None),
            key("XRPUSDT", "quote", None),
        ];
        assert_eq!(backfillable(&keys), 1);
        // ⚠ And the property the design mock's hover text would break: the venue is NOT part of
        // the gate. An off-roster venue is still planned and sent, so the server's own refusal
        // names the server's supported set — see `backfillable`'s doc.
        let off_roster = [SeriesKey {
            venue: "a-venue-this-build-never-heard-of".to_string(),
            symbol: "BTCUSDT".to_string(),
            kind: "bar".to_string(),
            interval: Some("1m".to_string()),
        }];
        assert_eq!(backfillable(&off_roster), 1);
    }

    #[test]
    fn span_label_states_the_window_or_says_it_has_none() {
        assert_eq!(span_label(0, 10 * DAY), "1970-01-01 → 1970-01-11 · 10 d");
        // A zero-width or inverted window prints no dates at all rather than a one-day span that
        // was never measured.
        assert_eq!(span_label(0, 0), "no span yet");
        assert_eq!(span_label(10 * DAY, 0), "no span yet");
    }

    #[test]
    fn log_plane_classifies_both_of_todays_producers_as_feeds() {
        // These are the EXACT two shapes written today — this module's Refresh, and
        // `crates/vike-desktop/src/app_ui.rs`'s `data_delete` drain. If either ever reads as
        // something else, the log's default `Feeds` segment silently stops showing the only lines
        // that exist.
        assert_eq!(log_plane("17:42:06  Refreshed · 14 series"), LogPlane::Feeds);
        assert_eq!(log_plane("17:05:12  Stopped BTCUSDT@1m"), LogPlane::Feeds);
    }

    #[test]
    fn log_plane_routes_the_store_and_backfill_vocabulary() {
        assert_eq!(log_plane("17:37:55  bybit 1m bar — 4,320 rows appended"), LogPlane::Backfill);
        assert_eq!(log_plane("17:36:02  okx rollup refreshed"), LogPlane::Store);
        assert_eq!(log_plane("16:51:44  gap sweep — 7 series"), LogPlane::Store);
        // ⚠ Precedence, stated as a test rather than only in prose: a backfill line routinely
        // names the store it wrote into, and "what did this do" is the more specific question.
        assert_eq!(log_plane("17:19:55  backfill wrote to the store"), LogPlane::Backfill);
    }

    #[test]
    fn the_log_filter_index_round_trips_through_every_segment() {
        for i in 0..LOG_SEGMENTS.len() {
            assert_eq!(log_filter_index(log_filter_of_index(i)), i, "segment {i}");
        }
        // Index 0 is the only "no filter" segment, and an out-of-range index degrades to it rather
        // than to an arbitrary plane.
        assert_eq!(log_filter_of_index(0), None);
        assert_eq!(log_filter_of_index(99), None);
    }

    #[test]
    fn every_segment_roster_is_the_inverse_of_its_enum_discriminants() {
        // ⚠ These pairs are what `plane as usize` / `mode as usize` rely on. A variant reordered
        // in one place and not the other would silently render the wrong segment as pressed while
        // showing the right screen, which is the kind of defect nobody reports.
        assert_eq!(PROVIDER_SEGMENTS.len(), PROVIDER_PLANES.len());
        for (i, plane) in PROVIDER_PLANES.iter().enumerate() {
            assert_eq!(*plane as usize, i, "provider segment {i}");
        }
        assert_eq!(GAP_SEGMENTS.len(), GAP_MODES.len());
        for (i, mode) in GAP_MODES.iter().enumerate() {
            assert_eq!(*mode as usize, i, "gap segment {i}");
        }
        // ⚠ `MemberCut` — the DataSet member strip — was BLOCK-LOCAL inside the `DataSets` arm
        // when it landed, so this gate could not see it and the pairing was held by nothing but
        // the enum and its two arrays sitting next to each other. That is precisely the coupling
        // this test exists to replace, which is why the items were promoted to module scope.
        assert_eq!(MEMBER_SEGMENTS.len(), MEMBER_CUTS.len());
        for (i, cut) in MEMBER_CUTS.iter().enumerate() {
            assert_eq!(*cut as usize, i, "member segment {i}");
        }
        // The defaults are the leftmost segment in all three, so a fresh screen opens on the cut
        // its rail label names rather than on a second one an operator has to notice.
        assert_eq!(ProviderPlane::default() as usize, 0);
        assert_eq!(GapMode::default() as usize, 0);
        // ...and for the member strip specifically: an evicted temp-memory entry falls back to
        // `Default`, so the cut a DataSet opens on must be the one that hides nothing.
        assert_eq!(MemberCut::default() as usize, 0);
        assert_eq!(MEMBER_SEGMENTS[0].0, "all members");
        assert_eq!(MEMBER_SEGMENTS[1].0, "stale only");
    }

    // ===== The DataSet member-symbol derivations =====
    //
    // ⚠ These had NO tests while they were block-local inside the `DataSets` arm — the
    // `#[cfg(test)]` module cannot reach a block-local item — and closing that is what the move to
    // module scope was for. Each test below pins a claim that previously lived only in a doc
    // comment.

    #[test]
    fn provider_names_a_venue_refuses_only_the_sentinel_and_the_blank() {
        assert!(provider_names_a_venue("binance"));
        assert!(provider_names_a_venue("dukascopy"));
        // `Auto` is what the form's dropdown OPENS on, so it is the common case, not the odd one.
        assert!(!provider_names_a_venue("Auto"));
        assert!(!provider_names_a_venue("AUTO"), "the sentinel is case-blind");
        assert!(!provider_names_a_venue(""));
        assert!(!provider_names_a_venue("  \t "), "whitespace is blank");
        // ⚠ The sentinel is refused as a WHOLE value, never as a substring — a venue whose name
        // merely begins with those four letters is a venue.
        assert!(provider_names_a_venue("autotrader"));
    }

    #[test]
    fn an_unstored_member_is_not_stale_because_it_has_no_last_ts_to_judge() {
        // The same 100-day window the sibling stale tests use: 35% puts the cut-off at day 65.
        let behind = vec![(key("DOGEUSDT", "bar", Some("1m")), 40 * DAY)];
        let fresh = vec![(key("BTCUSDT", "bar", Some("1m")), 100 * DAY)];
        assert!(member_is_stale(&behind, 0, 100 * DAY));
        assert!(!member_is_stale(&fresh, 0, 100 * DAY));
        // ⚠ THE CLAIM the `· not stored` row marker and the strip's legend both rest on: a member
        // the store holds NOTHING for is not stale. There is no `last_ts` to judge, and "never
        // fetched" is a different condition from "fallen behind" — conflating them would make
        // `stale only` quietly mean two things. An `any` over an empty slice is `false` by
        // construction, so this test exists for the OTHER direction: a future "treat unstored as
        // maximally stale" edit has to argue with something rather than reading as a fix.
        assert!(!member_is_stale(&[], 0, 100 * DAY));
        // ...and it stays false with no window at all, so an empty tree cannot make every member
        // on every DataSet stale at once.
        assert!(!member_is_stale(&[], 0, 0));
    }

    #[test]
    fn a_member_is_stale_when_any_one_of_its_series_is() {
        // `any`, not `all`: a symbol whose 1m bars are current but whose trade tape stopped months
        // ago IS behind, and the screen must not hide it because one series of several is fresh.
        let mixed = vec![
            (key("ETHUSDT", "bar", Some("1m")), 100 * DAY),
            (key("ETHUSDT", "trade", None), 20 * DAY),
        ];
        assert!(member_is_stale(&mixed, 0, 100 * DAY));
    }

    #[test]
    fn member_series_matches_case_insensitively_in_both_directions() {
        let t = member_tree(&[
            ("binance", "btcusdt", false, "bar", Some("1m"), 100),
            ("bybit", "BTCUSDT", false, "bar", Some("1m"), 40),
        ]);
        // ⚠ `datasets::parse_symbols` UPPER-CASES whatever the operator typed, while the store's
        // spelling is the store's. Match either side exactly and a venue that writes `btcusdt`
        // reads as "not stored" on this screen while holding a year of bars.
        assert_eq!(member_series(&t, "binance", "BTCUSDT").len(), 1);
        assert_eq!(member_series(&t, "bybit", "btcusdt").len(), 1);
        // The PROVIDER is compared the same way, and trimmed — the field is free text on a form.
        assert_eq!(member_series(&t, "  BINANCE ", "BTCUSDT").len(), 1);
    }

    #[test]
    fn auto_spans_every_venue_while_a_named_provider_refuses_the_others() {
        let t = member_tree(&[
            ("binance", "BTCUSDT", false, "bar", Some("1m"), 100),
            ("bybit", "BTCUSDT", false, "bar", Some("1m"), 40),
            ("okx", "ETHUSDT", false, "bar", Some("1m"), 40),
        ]);
        // `Auto` is what the form opens on and it means "wherever this lives", so it spans venues.
        let spanned = member_series(&t, "Auto", "BTCUSDT");
        let venues: Vec<&str> = spanned.iter().map(|(k, _)| k.venue.as_str()).collect();
        assert_eq!(venues, vec!["binance", "bybit"]);
        // An empty provider is the same sentinel by another spelling: a set naming no venue cannot
        // scope to one.
        assert_eq!(member_series(&t, "", "BTCUSDT").len(), 2);
        // ⚠ A named provider refuses the other venues' rows OUTRIGHT. The member reads as "not
        // stored" at that venue even though the symbol exists elsewhere — which is what the set
        // declared, and what makes the synthesized backfill key below the right answer for it.
        assert!(member_series(&t, "okx", "BTCUSDT").is_empty());
    }

    #[test]
    fn a_grouped_node_is_never_matched_as_a_member_symbol() {
        // ⚠ A GROUPED node's `symbol` field holds the GROUP NAME, not a symbol
        // (`vike_data_manager::SymbolNode::grouped`'s own doc). A member called `MAJORS` must
        // therefore NOT pick up a grouped node of the same name: it would be judged stale on this
        // screen and queued for backfill under a name no venue trades. `crate::stored_load`
        // records the same class of bug from the load side, which is why the skip is pinned here
        // rather than resting on a comment.
        let t = member_tree(&[
            ("binance", "MAJORS", true, "bar", Some("1m"), 10),
            ("binance", "MAJORS", false, "bar", Some("1m"), 100),
        ]);
        let hit = member_series(&t, "binance", "MAJORS");
        assert_eq!(hit.len(), 1, "only the per-symbol node may match");
        // ...and it is the PER-SYMBOL one. The grouped node's day-10 row would have flipped this
        // member to stale against a 100-day window, so the skip is load-bearing, not tidiness.
        assert_eq!(hit[0].1, 100 * DAY);
        assert!(!member_is_stale(&hit, 0, 100 * DAY));
        // With ONLY the grouped node present, the member resolves to nothing at all — and then
        // reads as `· not stored`, which is the honest answer: no venue holds `MAJORS`.
        let grouped_only = member_tree(&[("binance", "MAJORS", true, "bar", Some("1m"), 10)]);
        assert!(member_series(&grouped_only, "binance", "MAJORS").is_empty());
    }

    #[test]
    fn member_keys_prefer_the_stored_series_over_the_sets_declaration() {
        // BRANCH 1. Real keys carry the venue, kind and interval the store actually holds, and are
        // what `crate::backfill_plan::plan_backfill_jobs` can look gap ranges up for — a
        // synthesized key would instead re-fetch a default lookback over data already on disk.
        let stored = vec![
            (key("BTCUSDT", "bar", Some("1m")), 100 * DAY),
            (key("BTCUSDT", "trade", None), 100 * DAY),
        ];
        let keys = member_keys(&stored, "binance", "5m", "BTCUSDT");
        assert_eq!(keys, vec![key("BTCUSDT", "bar", Some("1m")), key("BTCUSDT", "trade", None)]);
        // ⚠ BOTH are returned, the non-`bar` one included: the PLANNER counts that one as a skip
        // and the button's hover reports it, so a kind this window cannot fetch is surfaced rather
        // than silently dropped here.
        assert_eq!(backfillable(&keys), 1);
        // The set's own interval is NOT imposed on a stored series — `5m` appears nowhere.
        assert!(keys.iter().all(|k| k.interval.as_deref() != Some("5m")));
    }

    #[test]
    fn an_unstored_member_synthesizes_one_bar_key_from_the_sets_own_declaration() {
        // BRANCH 2 — the case the Has-gaps and Stale strips never meet, because they queue rows
        // that are in the store by construction. A DataSet is a WISHLIST, and its most useful day
        // is the one where none of its symbols has been fetched yet.
        let keys = member_keys(&[], "binance", "5m", "SOLUSDT");
        assert_eq!(keys, vec![key("SOLUSDT", "bar", Some("5m"))]);
        // `bar` is not a guess: it is the only kind an INTERVAL can mean, and it is exactly what
        // the downstream gate asks for.
        assert_eq!(backfillable(&keys), 1);
        // Both fields are trimmed on the way in, so a padded form value still makes a clean key.
        assert_eq!(member_keys(&[], " binance ", " 5m ", "SOLUSDT"), keys);
        // ⚠ NO client-side venue roster, here either: an off-roster venue still produces a key, is
        // still sent, and is answered by the SERVER's refusal naming the server's own set. That is
        // the property `backfillable`'s doc argues for and the design mock's hover text broke.
        let off_roster = member_keys(&[], "a-venue-this-build-never-heard-of", "1m", "BTCUSDT");
        assert_eq!(off_roster.len(), 1);
        assert_eq!(backfillable(&off_roster), 1);
    }

    #[test]
    fn auto_or_a_blank_interval_synthesizes_nothing_and_the_member_counts_as_a_skip() {
        // BRANCH 3. ⚠ `Auto` is a UI sentinel, not a venue: a key carrying it would reach the
        // datahub as a venue nobody has, and the refusal coming back would read as "the server
        // cannot do this" when the truth is that this window sent a non-name. The button withholds
        // the key and its disabled hover names both form fields instead.
        assert!(member_keys(&[], "Auto", "5m", "BTCUSDT").is_empty());
        assert!(member_keys(&[], "auto", "5m", "BTCUSDT").is_empty());
        assert!(member_keys(&[], "", "5m", "BTCUSDT").is_empty());
        assert!(member_keys(&[], "   ", "5m", "BTCUSDT").is_empty());
        // An absent interval names no bar, so a real venue is not sufficient on its own.
        assert!(member_keys(&[], "binance", "", "BTCUSDT").is_empty());
        assert!(member_keys(&[], "binance", "  ", "BTCUSDT").is_empty());
        // ⚠ ...and the branch ORDER, which is the part a reader could get backwards: a STORED
        // series is still returned under `Auto` with no interval at all, because the store supplies
        // the venue and the resolution the set declined to name. Only the SYNTHESIS needs them.
        let stored = vec![(key("BTCUSDT", "bar", Some("1m")), 100 * DAY)];
        assert_eq!(
            member_keys(&stored, "Auto", "", "BTCUSDT"),
            vec![key("BTCUSDT", "bar", Some("1m"))]
        );
    }

    #[test]
    fn the_stale_sort_presets_point_the_way_their_labels_claim() {
        // ⚠ `cmp_flat_row` compares `cov.last_ts` for `Updated` and `cov.rows` for `Rows`, so
        // "oldest first" is ASCENDING and "most rows first" is DESCENDING. Either direction
        // inverted produces a screen that is precisely backwards and looks fine.
        let (label, _, sort) = STALE_SORTS[0];
        assert_eq!(label, "oldest first");
        assert_eq!(sort.column, SortColumn::Updated);
        assert!(sort.ascending, "oldest first means the smallest last_ts leads");
        let (label, _, sort) = STALE_SORTS[1];
        assert_eq!(label, "most rows first");
        assert_eq!(sort.column, SortColumn::Rows);
        assert!(!sort.ascending, "most rows first means the largest row count leads");
    }
}
