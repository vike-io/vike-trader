//! The chart window's title-bar toolbar — chart-style menu, symbol/interval dropdowns, the
//! sync-group chip, the ƒx/OF/Cmp popups, the LIVE badge, and the window controls (clone /
//! minimize / maximize / close), ALL ON ONE BAR (the egui analog of vike's `UnifiedTitleBar`).
//!
//! Extracted verbatim out of `main.rs` (pure module-extraction — behavior byte-identical) purely
//! to shrink that file and co-locate the chart-window chrome. This is app-specific chrome bound
//! to app-local types (`TitleActions`, the `SYMS`/`IVLS`/… constants, `font::`, `theme::`), so it
//! stays in vike-app rather than moving into vike-chart, which is egui + egui_plot but no eframe
//! and depends down-only on vike-indicators + vike-model.

use eframe::egui;
use vike_chart::chart;

use crate::font;
use crate::theme;
use vike_catalog::{Catalog, SearchFilter, Tab};
// The instrument search-result row moved DOWN into vike-app-core together with the ƒx picker
// (tool-view extraction batch 2) — BOTH symbol pickers paint it and only one of the two still
// lives here. Re-imported under its original bare name so the call site below reads unchanged.
use vike_app_core::symbol_row::search_result_row;

use crate::{
    ACCENT, COMPARE_COLORS, DEFAULT_VENUE, IVLS, SYMS, TICK_IVLS, TitleActions, UP, VOLUME_IVLS,
};

/// The picker's asset-class tabs, in display order — "All" is the absence of a tab
/// (`SearchFilter.tab == None`), rendered as the first button. Fixed list because `vike_catalog`
/// exposes no `Tab::all()`; kept in sync with the `Tab` enum by hand.
const PICKER_TABS: [Tab; 8] = [
    Tab::Stocks,
    Tab::Forex,
    Tab::Crypto,
    Tab::Perps,
    Tab::Options,
    Tab::Futures,
    Tab::Indices,
    Tab::Prediction,
];

/// The venues the chart symbol-picker's venue filter cycles over (a curated crypto subset of the
/// wired `CatalogProvider`s). `None` = every venue. Hyperliquid is included so its spot pairs
/// (chartable once the feed symbology loads) and perps are directly filterable here.
const PICKER_VENUES: [&str; 4] = ["binance", "bybit", "okx", "hyperliquid"];

/// Render the picker's asset-class **tab** row ("All" + each [`Tab`]) and the **venue** filter
/// cycle button, editing `filter` in place. Small horizontal button strips above the results list.
fn picker_filter_controls(ui: &mut egui::Ui, filter: &mut SearchFilter) {
    // Asset-class tabs. "All" == `filter.tab == None`. Wraps if the popup is narrow.
    ui.horizontal_wrapped(|ui| {
        if ui.selectable_label(filter.tab.is_none(), "All").clicked() {
            filter.tab = None;
        }
        for t in PICKER_TABS {
            if ui.selectable_label(filter.tab == Some(t), t.label()).clicked() {
                filter.tab = Some(t);
            }
        }
    });
    // Venue filter: a single cycle button None → binance → bybit → okx → None. Kept compact (a
    // dropdown would crowd the narrow popup); the label shows the current selection.
    ui.horizontal(|ui| {
        let cur = filter.venue.as_deref().map(|v| v.to_uppercase());
        let label = format!("Venue: {}", cur.as_deref().unwrap_or("All"));
        if ui.button(label).clicked() {
            let next = match filter.venue.as_deref() {
                None => Some(PICKER_VENUES[0].to_string()),
                Some(v) => {
                    let i = PICKER_VENUES.iter().position(|x| *x == v);
                    match i {
                        Some(i) if i + 1 < PICKER_VENUES.len() => {
                            Some(PICKER_VENUES[i + 1].to_string())
                        }
                        _ => None, // last venue (or unknown) → back to All
                    }
                }
            };
            filter.venue = next;
        }
    });
}

/// Custom single-row title bar — the egui analog of vike's `UnifiedTitleBar`,
/// matched to its measurements (BAR_H 30; symbol/chips 14px; window buttons 30px
/// wide / 15px glyph, contiguous, hard right edge; left margin 8; ~14px
/// brand→symbol and ~20px symbol→chips gaps). Holds the chart-style menu, the
/// symbol + interval dropdowns, the ticker, the LIVE badge, and the window
/// controls (clone / minimize / maximize / close) ALL ON ONE BAR. Returns the
/// frame's actions and the bar's drag delta (to move the window).
///
/// Note: egui has no light (300) font weight, so the symbol/chips match vike on
/// size (14px) but render at egui's single weight.
#[allow(clippy::too_many_arguments)]
pub(crate) fn title_bar(
    ui: &mut egui::Ui,
    id: egui::Id,
    symbol: &str,
    // Cross-exchange symbol search: this window's data venue (`"binance"`/`"bybit"`/`"okx"`).
    // Shown as a dim prefix on the symbol button for a non-Binance chart, and — crucially — reset
    // onto `a.new_venue` whenever a plain quick-pick is chosen, so switching back to a Binance
    // quick-pick from a Bybit chart also flips the feed venue back.
    venue: &str,
    interval: &str,
    style: chart::ChartStyle,
    sync_group: Option<u8>,
    is_max: bool,
    // SP2 orderflow (Task 7): current toggle state for the Orderflow popup below —
    // chart windows only (this function, unlike `tool_title_bar`, is only ever called for
    // `WinKind::Chart`, so no extra kind-gating is needed here).
    cvd_on: bool,
    profile_on: bool,
    of_tick_size: Option<f64>,
    // SP3 follow-up (Task 2): the GLOBAL `App::of_backfill_hours` value, threaded in read-only
    // so the OF popup can render/edit it — every chart window's popup shows (and can edit) the
    // SAME global value; there is no per-window copy (see `TitleActions::new_backfill_hours`).
    of_backfill_hours: f64,
    // C2a Task 4: this window's current Compare overlays (read-only), for the Compare popup's
    // chip list — colored per-entry with `COMPARE_COLORS[i]` to match the plotted line.
    compare: &[String],
    // C2b Task 9: this window's own-pane assignment + secondary-axis pins (read-only), for
    // the per-chip "Move to …" / "Pin to scale" menu. Both are `mem::take`-n out of `w`
    // before `show_window` at the call site (`w` is borrowed there), so these are the live
    // locals — a symbol is own-paned iff present in `series_pane`; its pin reads from
    // `series_scale` (absent ⇒ the `Percent` default).
    series_pane: &indexmap::IndexMap<String, chart::PaneKey>,
    series_scale: &indexmap::IndexMap<String, chart::ScaleAssign>,
    // Symbol-search: the CROSS-VENUE instrument universe (Binance + Bybit + OKX keyless catalogs,
    // aggregated into one `vike_catalog::Catalog`), for the symbol-picker + Compare search boxes.
    // Ranked/filtered via `catalog.search(query, &SearchFilter, limit)`. Empty until the background
    // fetch lands — both popups fall back to the built-in `SYMS` quick-picks meanwhile.
    catalog: &Catalog,
) -> (TitleActions, egui::Vec2) {
    use egui::{Align, Button, Layout, RichText, Sense, TextStyle, UiBuilder, Vec2};
    let mut a = TitleActions::default();
    const BAR_H: f32 = 30.0;

    // reserve the full-width bar strip + sense drag / double-click on it
    let (bar_rect, bar_resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), BAR_H), Sense::click_and_drag());
    // No bg fill: the window's BG shows through, so the header is the SAME color as the
    // window (like vike) AND the window's rounded top corners stay visible (a square fill
    // here would hide them). Just a 1px bottom divider like vike's docked title bar.
    // (no bottom divider — vike's title bar has none)
    if bar_resp.double_clicked() {
        a.toggle_max = true;
    }
    let drag = if bar_resp.dragged() { bar_resp.drag_delta() } else { Vec2::ZERO };

    // bump Body+Button text to 14px to match vike's title (no light weight in egui)
    fn set14(ui: &mut egui::Ui) {
        for ts in [TextStyle::Body, TextStyle::Button] {
            if let Some(f) = ui.style_mut().text_styles.get_mut(&ts) {
                f.size = 14.0;
            }
        }
    }

    // Reserve the right-edge controls' width so the left cluster can't overlap them.
    let ctrl_w = 4.0 * 30.0 + 6.0;

    // LEFT cluster: [style] ~14px [symbol▾] ~20px [interval▾]  bars  ticker  ●LIVE
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(egui::Rect::from_min_max(
                bar_rect.min + egui::vec2(8.0, 1.0),
                egui::pos2(bar_rect.max.x - ctrl_w, bar_rect.max.y - 1.0),
            ))
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            set14(ui);
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.spacing_mut().button_padding.y = 0.0; // combos = text height → all items center evenly
            // Frameless title-bar widgets: vike renders symbol/interval/ƒx as PLAIN TEXT on
            // the dark bar (no box). Make inactive widgets transparent; show HOVER only on hover.
            {
                let w = ui.visuals_mut();
                w.widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
                w.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
                w.widgets.inactive.bg_stroke = egui::Stroke::NONE;
                w.widgets.hovered.bg_stroke = egui::Stroke::NONE;
                w.widgets.active.bg_stroke = egui::Stroke::NONE;
                w.widgets.open.bg_stroke = egui::Stroke::NONE;
                w.button_frame = false;
            }
            // chart-style brand icon → the full vike style menu
            // Candlestick brand icon → opens the style menu (each row a real mini-icon).
            let (brect, bresp) =
                ui.allocate_exact_size(egui::vec2(22.0, 18.0), egui::Sense::click());
            chart::draw_style_icon(ui.painter(), brect, chart::ChartStyle::Candles);
            egui::Popup::menu(&bresp).id(id.with("style_popup")).show(|ui| {
                ui.set_min_width(186.0);
                for (sec, styles) in chart::STYLE_SECTIONS {
                    ui.label(
                        RichText::new(*sec)
                            .size(11.0)
                            .color(egui::Color32::from_rgb(120, 128, 140)),
                    );
                    for &st in *styles {
                        let (rr, rresp) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width().max(172.0), 22.0),
                            egui::Sense::click(),
                        );
                        if rresp.hovered() {
                            ui.painter().rect_filled(rr, 4.0, theme::HOVER);
                        }
                        let icon = egui::Rect::from_min_size(
                            egui::pos2(rr.left() + 6.0, rr.center().y - 8.0),
                            egui::vec2(18.0, 16.0),
                        );
                        chart::draw_style_icon(ui.painter(), icon, st);
                        let tcol = if style == st { theme::ACCENT } else { theme::TEXT_UI };
                        ui.painter().text(
                            egui::pos2(rr.left() + 30.0, rr.center().y),
                            egui::Align2::LEFT_CENTER,
                            st.label(),
                            egui::FontId::proportional(14.0),
                            tcol,
                        );
                        if rresp.clicked() {
                            a.set_style = Some(st);
                        }
                    }
                    ui.separator();
                }
            });
            ui.add_space(6.0); // brand→symbol (vike main spacing = 6)
            // symbol + interval are PLAIN clickable text (vike has NO dropdown ▼ arrows) — a
            // frameless button that opens a picker popup, not an egui ComboBox.
            // Cross-venue: a non-Binance chart shows a dim `VENUE:` prefix so the
            // button is unambiguous (Binance keeps its bare symbol — zero change).
            let sym_label = if venue == DEFAULT_VENUE {
                symbol.to_string()
            } else {
                format!("{}:{symbol}", venue.to_uppercase())
            };
            let sresp =
                ui.add(egui::Button::new(font::extralight(&sym_label).size(15.0)).frame(false));
            egui::Popup::menu(&sresp).id(id.with("sym_popup")).show(|ui| {
                ui.set_min_width(250.0);
                // TradingView-style search: a text field over the live instrument universe. The
                // query buffer lives in egui temp memory keyed by this window's id (same idiom as
                // the Compare entry below) so it persists across the popup's immediate-mode frames.
                let q_id = id.with("sym_search");
                let mut q = ui.data_mut(|d| d.get_temp::<String>(q_id).unwrap_or_default());
                let entry = ui.add(
                    egui::TextEdit::singleline(&mut q)
                        .hint_text("Search symbol")
                        .desired_width(234.0),
                );
                // Focus the box when the popup opens (nothing else focused yet) so the user can
                // type immediately — but don't re-steal focus every frame thereafter.
                if ui.memory(|m| m.focused().is_none()) {
                    entry.request_focus();
                }
                // Asset-class tab + venue filter (per-picker UI state, held in egui temp memory
                // keyed by this window's id — same idiom as the `q` search buffer; default = All).
                let f_id = id.with("sym_filter");
                let mut filter =
                    ui.data_mut(|d| d.get_temp::<SearchFilter>(f_id).unwrap_or_default());
                picker_filter_controls(ui, &mut filter);
                ui.separator();
                egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                    if catalog.is_empty() {
                        // Catalog not loaded yet (or fetch failed): fall back to quick-picks. These
                        // are Binance symbols, so selecting one also resets the venue to Binance —
                        // otherwise a quick-pick from a Bybit/OKX chart would keep the wrong venue.
                        for s in SYMS {
                            if ui
                                .selectable_label(venue == DEFAULT_VENUE && symbol == s, s)
                                .clicked()
                            {
                                a.new_symbol = Some(s.to_string());
                                a.new_venue = Some(DEFAULT_VENUE.to_string());
                            }
                        }
                    } else {
                        let matches = catalog.search(&q, &filter, 60);
                        if matches.is_empty() {
                            ui.add_space(4.0);
                            ui.weak("  No matching symbols");
                        }
                        for m in matches {
                            // "selected" = same venue AND symbol as the current chart.
                            let is_cur =
                                m.venue == venue && symbol.eq_ignore_ascii_case(&m.raw_symbol);
                            if search_result_row(ui, is_cur, m) {
                                a.new_symbol = Some(m.raw_symbol.clone());
                                a.new_venue = Some(m.venue.clone());
                                // Feed-routing slice 1: carry the picked instrument's asset class so
                                // `ensure_feed_on` can route non-spot symbols to their native product
                                // feed (e.g. OKX derivatives) instead of the spot bar feed.
                                a.new_asset_class = Some(m.asset_class);
                                q.clear();
                            }
                        }
                    }
                });
                ui.data_mut(|d| d.insert_temp(q_id, q));
                ui.data_mut(|d| d.insert_temp(f_id, filter));
            });
            ui.add_space(6.0); // symbol→interval (vike main spacing = 6)
            let iresp =
                ui.add(egui::Button::new(font::extralight(interval).size(15.0)).frame(false));
            egui::Popup::menu(&iresp).id(id.with("ivl_popup")).show(|ui| {
                ui.set_min_width(70.0);
                // Grouped: time (venue klines) / ticks / volume (Task B5), separated by
                // `ui.separator()`. All three groups emit the same `new_interval` string shape —
                // `ensure_feed` routes each one by `tickvol::BarKind::parse`.
                for iv in IVLS {
                    if ui.selectable_label(interval == iv, iv).clicked() {
                        a.new_interval = Some(iv.to_string());
                    }
                }
                ui.separator();
                for iv in TICK_IVLS {
                    if ui.selectable_label(interval == iv, iv).clicked() {
                        a.new_interval = Some(iv.to_string());
                    }
                }
                ui.separator();
                for iv in VOLUME_IVLS {
                    if ui.selectable_label(interval == iv, iv).clicked() {
                        a.new_interval = Some(iv.to_string());
                    }
                }
            });
            ui.add_space(6.0); // interval→sync chip (chart sync seam, task B8)
            // Sync group chip: a filled dot colored by group (1..=4), a hollow gray
            // dot when ungrouped. Click cycles None -> 1 -> 2 -> 3 -> 4 -> None; the
            // actual registry feed/harvest lives in the App::ui window loop (main.rs),
            // this widget only reports the click.
            const GROUP_COLORS: [egui::Color32; 4] = [
                egui::Color32::RED,
                egui::Color32::BLUE,
                egui::Color32::GREEN,
                egui::Color32::YELLOW,
            ];
            let (chip_label, chip_color) = match sync_group {
                // Defensive index (never panic on a hand-edited/corrupted out-of-range
                // group in workspace.json — same `.get().unwrap_or(..)` idiom persist.rs
                // uses for `style: usize`): an out-of-1..=4 value just paints gray.
                Some(g) => (
                    "●",
                    GROUP_COLORS
                        .get(g.wrapping_sub(1) as usize)
                        .copied()
                        .unwrap_or(egui::Color32::GRAY),
                ),
                None => ("○", egui::Color32::GRAY),
            };
            if ui
                .add(
                    Button::new(RichText::new(chip_label).color(chip_color).size(14.0))
                        .frame(false),
                )
                .on_hover_text("Sync group")
                .clicked()
            {
                a.new_group = Some(match sync_group {
                    None => Some(1),
                    Some(1) => Some(2),
                    Some(2) => Some(3),
                    Some(3) => Some(4),
                    Some(_) => None, // 4 -> None (also the defensive fallback for out-of-range)
                });
            }
            ui.add_space(6.0); // sync chip→ƒx (vike main spacing = 6)
            if ui.button(RichText::new("ƒx").size(13.0)).on_hover_text("Indicators").clicked() {
                a.open_picker = true;
            }
            ui.add_space(6.0); // ƒx→OF (SP2 orderflow controls, Task 7)
            // "OF" opens the Orderflow popup: CVD / Volume Profile toggles + a tick-size
            // override. Any of the three ON (this popup's two toggles, or the Footprint
            // STYLE via the style menu above) drives `WinState::orderflow_on()`, which the
            // window loop uses to lazily subscribe the trade feed + register an aggregator
            // (`main.rs`'s `of_wanted` collection). Label tints ACCENT while either toggle
            // is on (mirrors the sync chip's filled-vs-hollow "is this active" convention).
            let of_active = cvd_on || profile_on;
            let of_label = RichText::new("OF").size(13.0).color(if of_active {
                ACCENT
            } else {
                ui.visuals().text_color()
            });
            let of_resp =
                ui.button(of_label).on_hover_text("Orderflow: CVD / Volume Profile / tick size");
            egui::Popup::menu(&of_resp).id(id.with("of_popup")).show(|ui| {
                ui.set_min_width(170.0);
                let mut cvd = cvd_on;
                if ui.checkbox(&mut cvd, "CVD").changed() {
                    a.cvd_on = Some(cvd);
                }
                let mut profile = profile_on;
                if ui.checkbox(&mut profile, "Volume Profile").changed() {
                    a.profile_on = Some(profile);
                }
                ui.separator();
                ui.label(RichText::new("Tick size").size(12.0));
                let mut auto = of_tick_size.is_none();
                if ui.checkbox(&mut auto, "Auto").changed() {
                    a.of_tick_size =
                        Some(if auto { None } else { Some(of_tick_size.unwrap_or(1.0)) });
                }
                if !auto {
                    let mut val = of_tick_size.unwrap_or(1.0);
                    let resp = ui.add(
                        egui::DragValue::new(&mut val)
                            .speed(0.01)
                            .range(0.0..=f64::MAX)
                            .max_decimals(6),
                    );
                    if resp.changed() {
                        a.of_tick_size = Some(Some(val));
                    }
                }
                ui.separator();
                // SP3 follow-up (Task 2): backfill-hours field. GLOBAL (`App::of_backfill_hours`,
                // not per-window — see `TitleActions::new_backfill_hours`'s doc), so every open
                // chart's OF popup edits the same value. Changing it after a symbol's backfill has
                // already spawned does NOT retrigger it — `maybe_spawn_backfill`'s `bf_spawned` is
                // a run-once-per-symbol gate (see its doc); the tooltip below says so.
                ui.label(RichText::new("Backfill hours").size(12.0));
                let mut bf_hours = of_backfill_hours;
                let bf_resp = ui
                    .add(
                        egui::DragValue::new(&mut bf_hours)
                            .range(0.0..=24.0)
                            .speed(0.5)
                            .suffix("h"),
                    )
                    .on_hover_text(
                        "History to backfill for CVD/profile (0 = live only). Takes effect for \
                         charts whose backfill hasn't started yet.",
                    );
                if bf_resp.changed() {
                    a.new_backfill_hours = Some(bf_hours);
                }
            });
            ui.add_space(6.0); // OF→Compare (C2a Task 4)
            // "Cmp" opens the Compare popup: a symbol entry (Enter adds a
            // %-normalized overlay series), quick-picks from the known symbol list,
            // and a colored chip per current overlay (✕ removes it). Adding the FIRST
            // overlay auto-switches this window's price scale to Percent (see
            // `WinState::add_compare`) so the overlay is visible. Label tints ACCENT
            // while any overlay is active (same convention as the OF/sync chips).
            let cmp_active = !compare.is_empty();
            let cmp_label = RichText::new("Cmp").size(13.0).color(if cmp_active {
                ACCENT
            } else {
                ui.visuals().text_color()
            });
            let cmp_resp =
                ui.button(cmp_label).on_hover_text("Compare: overlay another symbol (% scale)");
            egui::Popup::menu(&cmp_resp).id(id.with("compare_popup")).show(|ui| {
                ui.set_min_width(250.0);
                ui.label(RichText::new("Compare symbol").size(12.0));
                // Free-text entry. `title_bar` is a stateless free fn, so the cross-frame
                // input buffer lives in egui temp memory keyed by this window's id (the
                // standard egui idiom for transient widget state without a struct field).
                let buf_id = id.with("compare_entry");
                let mut buf = ui.data_mut(|d| d.get_temp::<String>(buf_id).unwrap_or_default());
                let entry = ui.add(
                    egui::TextEdit::singleline(&mut buf)
                        .hint_text("e.g. ETHUSDT")
                        .desired_width(160.0),
                );
                // Enter (or the Add button) commits; symbols are upper-cased to match the
                // `SYMBOL@interval` chart-key convention (`WinState::add_compare` dedups/ignores-self).
                // Both operands are evaluated BEFORE the `||` (never short-circuited) so the Add
                // button is drawn every frame — immediate-mode widgets can't be conditional.
                let enter = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let add_clicked = ui.button("Add").clicked();
                if enter || add_clicked {
                    let sym = buf.trim().to_uppercase();
                    if !sym.is_empty() {
                        a.add_compare = Some(sym);
                    }
                    buf.clear();
                }
                ui.data_mut(|d| d.insert_temp(buf_id, buf));
                // Live search results when the catalog has loaded AND the user is typing —
                // filtered to instruments not already overlaid and not this window's own primary;
                // clicking a row adds it as a compare overlay. Falls back to the `SYMS` quick-picks
                // when the catalog is empty (not fetched yet) or the entry is blank.
                let filter_q = ui.data(|d| d.get_temp::<String>(buf_id).unwrap_or_default());
                let excluded = |s: &str| {
                    s.eq_ignore_ascii_case(symbol)
                        || compare.iter().any(|c| c.eq_ignore_ascii_case(s))
                };
                if !catalog.is_empty() && !filter_q.trim().is_empty() {
                    ui.separator();
                    egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                        let mut any = false;
                        // Compare overlays are Binance-namespaced (same `{sym}@{interval}` key +
                        // same feed), so the Compare search stays Binance-only — cross-venue
                        // overlays are out of scope (the PRIMARY picker above is the cross-venue
                        // one). The venue `SearchFilter` restricts to Binance so the Bybit/OKX rows
                        // the aggregated catalog contains never appear here.
                        let binance_only =
                            SearchFilter { tab: None, venue: Some(DEFAULT_VENUE.to_string()) };
                        for m in catalog.search(&filter_q, &binance_only, 60) {
                            if excluded(&m.raw_symbol) {
                                continue;
                            }
                            any = true;
                            if search_result_row(ui, false, m) {
                                a.add_compare = Some(m.raw_symbol.clone());
                                ui.data_mut(|d| d.insert_temp(buf_id, String::new()));
                            }
                        }
                        if !any {
                            ui.add_space(4.0);
                            ui.weak("  No matching symbols");
                        }
                    });
                } else {
                    // Quick-picks: the known symbols not already overlaid and not this window's own.
                    let picks: Vec<&str> = SYMS.iter().copied().filter(|s| !excluded(s)).collect();
                    if !picks.is_empty() {
                        ui.separator();
                        for s in picks {
                            if ui.selectable_label(false, s).clicked() {
                                a.add_compare = Some(s.to_string());
                            }
                        }
                    }
                }
                // Current overlays as colored chips (color matches the plotted line).
                if !compare.is_empty() {
                    ui.separator();
                    for (i, sym) in compare.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let color = COMPARE_COLORS[i % COMPARE_COLORS.len()];
                            ui.colored_label(color, "●");
                            ui.label(RichText::new(sym).size(13.0));
                            if ui.small_button("✕").on_hover_text("Remove overlay").clicked() {
                                a.remove_compare = Some(sym.clone());
                            }
                            // C2b Task 9: per-symbol placement + scale menu. Own-paned iff
                            // present in `series_pane`; the two placement items are mutually
                            // exclusive on that. Deliberately NO "merge into another pane"
                            // (Task 6 review: the series loop is one-symbol-per-pane) and NO
                            // "Left" pin (Task 7 review: Left silently aliases Right).
                            ui.menu_button("⋯", |ui| {
                                if series_pane.contains_key(sym) {
                                    if ui.button("Move to price pane (overlay)").clicked() {
                                        a.series_to_overlay = Some(sym.clone());
                                        ui.close();
                                    }
                                } else if ui.button("Move to own pane").clicked() {
                                    a.series_to_own_pane = Some(sym.clone());
                                    ui.close();
                                }
                                let cur = series_scale.get(sym).copied().unwrap_or_default();
                                ui.menu_button("Pin to scale", |ui| {
                                    if ui
                                        .selectable_label(
                                            cur == chart::ScaleAssign::Percent,
                                            "Percent",
                                        )
                                        .clicked()
                                    {
                                        a.set_series_scale =
                                            Some((sym.clone(), chart::ScaleAssign::Percent));
                                        ui.close();
                                    }
                                    if ui
                                        .selectable_label(cur == chart::ScaleAssign::Right, "Right")
                                        .clicked()
                                    {
                                        a.set_series_scale =
                                            Some((sym.clone(), chart::ScaleAssign::Right));
                                        ui.close();
                                    }
                                    // Absolute-shared-axis: render the compare at its TRUE
                                    // absolute price on the PRIMARY (Linear/Log) axis, sharing
                                    // one scale (no rebasing, no secondary axis). Only takes
                                    // visual effect while the price scale is Linear/Log.
                                    if ui
                                        .selectable_label(
                                            cur == chart::ScaleAssign::SharedLinear,
                                            "Absolute (shared)",
                                        )
                                        .clicked()
                                    {
                                        a.set_series_scale =
                                            Some((sym.clone(), chart::ScaleAssign::SharedLinear));
                                        ui.close();
                                    }
                                });
                            });
                        });
                    }
                }
            });
            ui.add_space(8.0); // Compare→LIVE (vike status_margin = 8)
            ui.label(RichText::new("● LIVE").color(UP).size(12.0));
        },
    );

    // RIGHT cluster: window controls — contiguous (0 gap), 30px wide, 15px glyph,
    // flat, hard against the right edge. Order L→R: ＋ ─ □ ✕ (vike layout).
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(bar_rect.shrink2(egui::vec2(0.0, 1.0)))
            .layout(Layout::right_to_left(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.visuals_mut().button_frame = false;
            let ctrl = |ui: &mut egui::Ui, glyph: &str, tip: &str| -> bool {
                ui.add_sized([30.0, BAR_H - 2.0], Button::new(RichText::new(glyph).size(15.0)))
                    .on_hover_text(tip)
                    .clicked()
            };
            if ctrl(ui, "✕", "Close") {
                a.close = true;
            }
            if ctrl(ui, if is_max { "❐" } else { "□" }, "Maximize / restore") {
                a.toggle_max = true;
            }
            if ctrl(ui, "─", "Minimize to rail") {
                a.minimize = true;
            }
        },
    );

    (a, drag)
}
