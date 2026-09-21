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

use super::ToolCtx;
use super::data_rail::{self, DataDest};
use crate::tools;
use vike_data::datasets;
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
                super::data_screens::overview(ui, ctx, &data_rail::RailCounts::from_ctx(ctx))
            {
                tv.data_dest = d;
            }
        }
        DataDest::ByVenue => super::data_screens::by_venue(ui, ctx),
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
            let pill = |ui: &mut egui::Ui, label: &str, enabled: bool| -> egui::Response {
                ui.add_enabled(
                    enabled,
                    egui::Button::new(
                        RichText::new(label)
                            .color(if enabled { palette::TEXT } else { palette::TEXT3 })
                            .size(13.0),
                    )
                    .fill(palette::SURFACE)
                    .stroke(egui::Stroke::new(1.0, palette::BORDER))
                    .min_size(vec2(0.0, 28.0)),
                )
            };
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                if pill(ui, "↻ Refresh", true).clicked() {
                    let ts =
                        vike_chart::to_naive(chrono::Utc::now().timestamp_millis(), ctx.display_tz)
                            .map(|dt| dt.format("%H:%M:%S").to_string())
                            .unwrap_or_default();
                    tv.data_log.push(format!("{ts}  Refreshed · {} series", feeds.len()));
                }
                // data-engine actions with no backing in the live-only Rust app → disabled (honest)
                for label in [
                    "⟳ Update all",
                    "⤓ Download / Extend…",
                    "⤒ Import CSV…",
                    "🔍 Inspect",
                    "🩹 Repair gaps",
                    "🧼 Clean data",
                    "📌 Pin / Unpin",
                    "⚙ Instruments…",
                    "✂ Truncate…",
                    "🧹 Remove inactive…",
                ] {
                    let _ = pill(ui, label, false);
                }
                let can_del = tv.data_sel.is_some();
                if pill(ui, "🗑 Delete", can_del).clicked() {
                    tv.data_delete = tv.data_sel.take();
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{} series", feeds.len()))
                            .size(11.0)
                            .color(palette::TEXT3),
                    );
                });
            });
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
            ui.label(font::bold("ACTIVITY LOG").size(10.0).color(palette::TEXT3));
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
                            }
                            for line in &tv.data_log {
                                ui.label(
                                    RichText::new(line)
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
                        let syms = datasets::parse_symbols(&tv.ds_symbols_text);
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
                                        if syms.is_empty() {
                                            ui.weak("  (no symbols yet)");
                                        }
                                        for s in &syms {
                                            let on = tv.ds_sym_sel.as_deref() == Some(s.as_str());
                                            if ui
                                                .selectable_label(
                                                    on,
                                                    RichText::new(s).monospace().size(13.0).color(
                                                        if on {
                                                            palette::TEXT
                                                        } else {
                                                            palette::TEXT2
                                                        },
                                                    ),
                                                )
                                                .clicked()
                                            {
                                                tv.ds_sym_sel = Some(s.clone());
                                            }
                                        }
                                    });
                            });
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
                        // Save / Test symbol / Test DataSet / Delete
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
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
        }
        DataDest::AllSeries | DataDest::HasGaps | DataDest::Stale => {
            // ===== The stored inventory, at three filters =====
            //
            // These are ONE render at three `ViewFilter`s, not three screens. The rail sets the filter
            // that `views_sidebar`'s "Smart views" group used to set from inside the body — which is
            // the whole point of promoting them: a smart view you have to already be in the Stored tab
            // to discover is a smart view nobody applies.
            tv.stored_grid.active_view = match tv.data_dest {
                DataDest::HasGaps => vike_data_manager::ViewFilter::HasGaps,
                DataDest::Stale => vike_data_manager::ViewFilter::Stale,
                _ => vike_data_manager::ViewFilter::All,
            };
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
            ui.label(font::semibold("Providers").size(14.0).color(palette::TEXT));
            ui.add_space(4.0);
            for (i, p) in
                ["binance (live)", "bybit", "okx", "coinbase", "kraken", "yahoo", "dukascopy"]
                    .iter()
                    .enumerate()
            {
                ui.label(RichText::new(format!("  • {p}")).size(13.0).color(if i == 0 {
                    palette::TEXT
                } else {
                    palette::TEXT3
                }));
            }
            ui.add_space(6.0);
            ui.weak("Only Binance streams in this build; the rest are config placeholders.");

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
    use super::{column_edges, ds_load_form, fmt_series_dt};
    use crate::tools::ToolView;
    use vike_data::datasets::DataSet;

    /// The exact weights the Cached-Series header uses — the layout under test.
    const WEIGHTS: [f32; 6] = [1.2, 0.9, 0.8, 1.6, 1.6, 1.0];

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
}
