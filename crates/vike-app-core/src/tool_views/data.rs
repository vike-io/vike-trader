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
use crate::tools;
use vike_data::datasets;
use vike_ui_theme::fmt::fmt_thousands;
use vike_ui_theme::font;
use vike_ui_theme::palette as theme;

/// The Venues (per-venue arming) sub-tab's index — the LAST tab, appended rather than inserted so
/// no persisted `ToolView::data_subtab` changes meaning (see the `TABS` array's own note).
///
/// Named here rather than beside [`crate::startup::DATA_SUBTAB_STORED`] because unlike that one it
/// is not a workspace-restore target; it exists so the shell can ask
/// [`data_tab_reads_arming`] whether to pay for this frame's credential-store read.
pub const DATA_SUBTAB_VENUES: usize = 6;

/// **Does this frame need [`super::VenueArmingInputs`]?** — the shell's gate for the per-frame
/// credential-store read and settings load the Venues tab needs and no other sub-tab does.
///
/// It lives here, in the CI-tested crate, rather than as an index comparison in `vike-app`'s
/// `main.rs`: the index is this module's fact, and a literal `6` in the shell is a second copy that
/// silently stops matching the day a tab is added.
#[must_use]
pub fn data_tab_reads_arming(tv: &tools::ToolView) -> bool {
    tv.data_subtab == DATA_SUBTAB_VENUES
}

/// The Data-manager tool body: the Cached-Series catalog + the DataSet Symbols editor + the
/// provider sub-tabs. Sub-tab 5 ("Stored") hands off to [`super::stored_tool_content`], which
/// reads the same [`ToolCtx`].
pub fn data_tool_content(
    ui: &mut egui::Ui,
    ctx: &ToolCtx<'_>,
    tv: &mut tools::ToolView,
    arming: Option<&super::VenueArmingInputs>,
) {
    use egui::RichText;
    use egui::{Align, Align2, Color32, FontFamily, FontId, Layout, Sense, vec2};
    const SURF: Color32 = theme::SURFACE;
    const ZEBRA: Color32 = Color32::from_rgba_premultiplied(33, 37, 44, 90);
    const BORD: Color32 = theme::BORDER;
    const TEXT: Color32 = theme::TEXT;
    const T2: Color32 = theme::TEXT2;
    const T3: Color32 = theme::TEXT3;
    let (feeds, dsets) = (ctx.feeds, ctx.dsets);
    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);

    debug_assert_eq!(TABS[crate::startup::DATA_SUBTAB_STORED], "Stored");
    debug_assert_eq!(TABS[DATA_SUBTAB_VENUES], super::VENUES_TAB_LABEL);
    // ⚠ APPENDED, never inserted: `ToolView::data_subtab` is a persisted INDEX (the workspace file
    // carries it, and `crate::startup::DATA_SUBTAB_STORED` names one), so putting a new tab
    // anywhere but the end would silently move every saved layout to a different sub-tab.
    const TABS: [&str; 7] = [
        "Symbols",
        "Cached Series",
        "Historical Providers",
        "Event Providers",
        "Streaming Providers",
        "Stored",
        super::VENUES_TAB_LABEL,
    ];
    ui.horizontal(|ui| {
        for (i, name) in TABS.iter().enumerate() {
            let on = i == tv.data_subtab;
            if ui
                .selectable_label(
                    on,
                    RichText::new(*name).size(13.0).color(if on { TEXT } else { T3 }),
                )
                .clicked()
            {
                tv.data_subtab = i;
            }
        }
    });
    ui.add_space(6.0);

    if tv.data_subtab == 1 {
        // ===== Cached Series — the live-feed catalog (the only sub-tab with real data) =====
        let pill = |ui: &mut egui::Ui, label: &str, enabled: bool| -> egui::Response {
            ui.add_enabled(
                enabled,
                egui::Button::new(
                    RichText::new(label).color(if enabled { TEXT } else { T3 }).size(13.0),
                )
                .fill(SURF)
                .stroke(egui::Stroke::new(1.0, BORD))
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
                ui.label(RichText::new(format!("{} series", feeds.len())).size(11.0).color(T3));
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
                put(p, hrect, i, h, T3, &edges);
            }
            p.hline(hrect.x_range(), hrect.bottom(), egui::Stroke::new(1.0, BORD));
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
                        p.rect_filled(rect, 0.0, SURF);
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
                        put(p, rect, i, v, if i == 0 { TEXT } else { T2 }, &edges);
                    }
                    if resp.clicked() {
                        tv.data_sel = Some(key.clone());
                    }
                }
            },
        );
        ui.add_space(8.0);
        ui.label(font::bold("ACTIVITY LOG").size(10.0).color(T3));
        egui::Frame::new()
            .fill(SURF)
            .stroke(egui::Stroke::new(1.0, BORD))
            .corner_radius(4.0)
            .inner_margin(6.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(110.0)
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
                                .color(T2),
                            );
                        }
                        for line in &tv.data_log {
                            ui.label(RichText::new(line).monospace().size(11.0).color(T2));
                        }
                    });
            });
    } else if tv.data_subtab == 0 {
        // ===== Symbols — the DataSet tree + editor (vike DataManager Symbols tab) =====
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
                                font::semibold("＋ New DataSet").color(TEXT).size(13.0),
                            )
                            .fill(SURF)
                            .stroke(egui::Stroke::new(1.0, BORD))
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
                            ui.label(font::semibold(g).size(13.0).color(TEXT));
                            for d in dsets.sets.iter().filter(|d| group_has(g, d)) {
                                let on = tv.ds_name == d.name;
                                if ui
                                    .selectable_label(
                                        on,
                                        RichText::new(format!("    {}", d.name))
                                            .size(13.0)
                                            .color(if on { TEXT } else { T2 }),
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
                        |ui: &mut egui::Ui, label: &str, body: &mut dyn FnMut(&mut egui::Ui)| {
                            ui.horizontal(|ui| {
                                ui.allocate_ui_with_layout(
                                    vec2(lbl_w, 22.0),
                                    Layout::left_to_right(Align::Center),
                                    |ui| {
                                        ui.label(RichText::new(label).size(13.0).color(T2));
                                    },
                                );
                                body(ui);
                            });
                        };
                    row(ui, "Name", &mut |ui| {
                        ui.add(egui::TextEdit::singleline(&mut tv.ds_name).desired_width(260.0));
                    });
                    row(ui, "Provider", &mut |ui| {
                        let resp = ui.add(
                            egui::Button::new(
                                RichText::new(tv.ds_provider.clone()).color(TEXT).size(13.0),
                            )
                            .fill(SURF)
                            .stroke(egui::Stroke::new(1.0, BORD))
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
                                RichText::new(tv.ds_interval.clone()).color(TEXT).size(13.0),
                            )
                            .fill(SURF)
                            .stroke(egui::Stroke::new(1.0, BORD))
                            .min_size(vec2(120.0, 26.0)),
                        );
                        egui::Popup::menu(&resp).show(|ui| {
                            ui.set_min_width(116.0);
                            for iv in ["1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "1d", "1w"]
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
                            .color(T2),
                    );
                    let syms = datasets::parse_symbols(&tv.ds_symbols_text);
                    egui::Frame::new()
                        .fill(SURF)
                        .stroke(egui::Stroke::new(1.0, BORD))
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
                                                RichText::new(s)
                                                    .monospace()
                                                    .size(13.0)
                                                    .color(if on { TEXT } else { T2 }),
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
                        RichText::new("Symbols (comma or newline separated)").size(13.0).color(T2),
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
                        .stroke(egui::Stroke::new(1.0, BORD))
                        .corner_radius(4.0)
                        .inner_margin(8.0)
                        .show(ui, |ui| {
                            ui.label(RichText::new("Ask the AI").size(12.0).color(T2));
                            ui.add(
                                egui::TextEdit::singleline(&mut tv.ds_ai_prompt)
                                    .hint_text("e.g. top 10 liquid crypto majors")
                                    .desired_width(f32::INFINITY),
                            );
                            if ui
                                .add(
                                    egui::Button::new(font::semibold("Suggest").color(TEXT))
                                        .fill(SURF)
                                        .stroke(egui::Stroke::new(1.0, BORD))
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
                                egui::Button::new(font::semibold("💾 Save").color(TEXT))
                                    .fill(SURF)
                                    .stroke(egui::Stroke::new(1.0, BORD))
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
                                egui::Button::new(
                                    RichText::new("▶ Test symbol").color(if has_sym {
                                        TEXT
                                    } else {
                                        T3
                                    }),
                                )
                                .fill(SURF)
                                .stroke(egui::Stroke::new(1.0, BORD))
                                .min_size(vec2(0.0, 30.0)),
                            )
                            .clicked()
                        {
                            tv.ds_test = test_sym;
                        }
                        let _ = ui.add_enabled(
                            false,
                            egui::Button::new(RichText::new("▶ Test DataSet").color(T3))
                                .fill(SURF)
                                .stroke(egui::Stroke::new(1.0, BORD))
                                .min_size(vec2(0.0, 30.0)),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let deletable = tv.ds_sel.is_some();
                            if ui
                                .add_enabled(
                                    deletable,
                                    egui::Button::new(
                                        RichText::new("🗑 Delete").color(if deletable {
                                            TEXT
                                        } else {
                                            T3
                                        }),
                                    )
                                    .fill(SURF)
                                    .stroke(egui::Stroke::new(1.0, BORD))
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
    } else if tv.data_subtab == 5 {
        // ===== Stored — the local HistStore's inventory tree (Task 3) =====
        super::stored_tool_content(ui, ctx, tv);
    } else if tv.data_subtab == DATA_SUBTAB_VENUES {
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
    } else {
        ui.add_space(4.0);
        ui.label(font::semibold(TABS[tv.data_subtab]).size(14.0).color(TEXT));
        ui.add_space(4.0);
        for (i, p) in ["binance (live)", "bybit", "okx", "coinbase", "kraken", "yahoo", "dukascopy"]
            .iter()
            .enumerate()
        {
            ui.label(RichText::new(format!("  • {p}")).size(13.0).color(if i == 0 {
                TEXT
            } else {
                T3
            }));
        }
        ui.add_space(6.0);
        ui.weak("Only Binance streams in this build; the rest are config placeholders.");
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
