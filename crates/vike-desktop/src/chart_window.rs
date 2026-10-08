//! The chart window's title-bar toolbar — chart-style menu, symbol/interval dropdowns, the
//! sync-group chip, the ƒx/OF/Cmp popups, the LIVE badge, and the window controls (clone /
//! minimize / maximize / close), ALL ON ONE BAR (the egui analog of vike's `UnifiedTitleBar`).
//!
//! Extracted out of `main.rs` to shrink that file and co-locate the chart-window chrome — a pure
//! module extraction at the time; design system step 7 has since put its colours and text sizes on
//! the installed appearance (`title_bar`'s own doc). This is app-specific chrome bound
//! to app-local types (`TitleActions`, the `SYMS`/`IVLS`/… constants), so it
//! stays in vike-desktop rather than moving into vike-chart, which is egui + egui_plot but no eframe
//! and depends down-only on vike-indicators + vike-model.

use eframe::egui;
use vike_chart::chart;

use vike_catalog::{Catalog, SearchFilter};
use vike_ui_theme::appearance;
use vike_ui_theme::icons::{self, Icon};
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::theme::Theme;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::{chrome, status};
// The instrument search-result row moved DOWN into vike-app-core together with the ƒx picker
// (tool-view extraction batch 2) — BOTH symbol pickers paint it and only one of the two still
// lives here. Re-imported under its original bare name so the call site below reads unchanged.
use vike_app_core::ui::symbol_row::search_result_row;
use vike_app_core::ui::workspace::{DEFAULT_VENUE, menu::submenu};
use vike_ui_theme::value::desktop::{self, STYLE_ICON_SIZE};

use crate::{COMPARE_COLORS, IVLS, SYMS, TICK_IVLS, TitleActions, VOLUME_IVLS};

/// Custom single-row title bar — the egui analog of vike's `UnifiedTitleBar`,
/// the same bar as every other window's (`vike_ui_theme::chrome`: 25 pt, the window controls 20 pt
/// squares four apart, four from the right edge; left margin 9; ~14px brand→symbol and ~20px
/// symbol→chips gaps). Holds the chart-style
/// menu, the symbol + interval dropdowns, the ticker, the LIVE badge, and the window
/// controls (clone / minimize / maximize / close) ALL ON ONE BAR. Returns the
/// frame's actions and the bar's drag delta (to move the window).
///
/// Its text follows the installed text size: the symbol, the interval and the window-control
/// icons are the Title role, and everything else on the bar is the Strong role.
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
    // chart windows only (this function, unlike `workspace::tool_title_bar`, is only ever called for
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
    // THE SERIES THIS NODE PUBLISHES. The `catalog` above is a venue-provider fetch and this
    // binary links ONE provider (Deribit), so through it an operator can reach `binance` (the
    // `SYMS` quick-picks) and `deribit` and nothing else — while a backend mounts whatever it
    // mounts. These rows come off the WIRE instead, which is how a bybit series becomes reachable
    // without restoring a venue edge the rename deliberately removed.
    backend: &[vike_app_core::ui::series_follow::PublishedSeries],
    // The node's directory: each venue's own spelling (`venue.title`) for the picker's chips. `None`
    // before its first reply, when a venue is spelled by its key.
    directory: Option<&vike_tradehub_client::wire::WireDirectory>,
    // What the badge may claim — `● LIVE` only when bars have actually arrived.
    feed: vike_app_core::ui::series_follow::ChartFeed,
) -> (TitleActions, egui::Vec2) {
    use egui::{Align, Button, Layout, RichText, Sense, UiBuilder, Vec2};
    let mut a = TitleActions::default();
    // The installed appearance: this frame's theme and text size, read from the context like every
    // other painter — never `palette`'s compile-time Graphite. The window's title (symbol, interval)
    // and its controls are the Title role; everything else on the bar is Strong (owner decisions 1
    // and 3). The bar's HEIGHT is every window's (`vike_ui_theme::chrome::TITLE_BAR_H`): one
    // header, defined once.
    let look = appearance::current(ui.ctx());
    let t = Theme::of(look.theme);
    let title_px = look.text_size.px(TextRole::Title);
    let bar_px = look.text_size.px(TextRole::Strong);

    // reserve the full-width bar strip + sense drag / double-click on it
    let (bar_rect, bar_resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), chrome::TITLE_BAR_H),
        Sense::click_and_drag(),
    );
    // No fill of its own: the window's BG shows through and keeps the rounded top corners —
    // unless the header gradient is on (spec §2), which `vike_ui_theme::header` paints with those
    // corners kept. (no bottom divider — vike's title bar has none)
    vike_ui_theme::header::paint_header_background(
        ui.painter(),
        bar_rect,
        &vike_ui_theme::appearance::current(ui.ctx()),
    );
    if bar_resp.double_clicked() {
        a.toggle_max = true;
    }
    let drag = if bar_resp.dragged() { bar_resp.drag_delta() } else { Vec2::ZERO };

    // Reserve the right-edge controls' width so the left cluster can't overlap them.
    let ctrl_w = chrome::CONTROLS_W;

    // LEFT cluster: [style] ~14px [symbol▾] ~20px [interval▾]  bars  ticker  ●LIVE
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(egui::Rect::from_min_max(
                bar_rect.min + egui::vec2(chrome::TITLE_INSET, space::HAIR),
                egui::pos2(bar_rect.max.x - ctrl_w, bar_rect.max.y - space::HAIR),
            ))
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = space::MD;
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
            let (brect, bresp) = ui.allocate_exact_size(desktop::BRAND_SIZE, egui::Sense::click());
            chart::draw_style_icon(ui.painter(), brect, chart::ChartStyle::Candles);
            egui::Popup::menu(&bresp).id(id.with("style_popup")).show(|ui| {
                ui.set_min_width(desktop::STYLE_MENU_W);
                for (sec, styles) in chart::STYLE_SECTIONS {
                    ui.label(
                        RichText::new(*sec)
                            .size(look.text_size.px(TextRole::Caption))
                            .color(t.text3),
                    );
                    for &st in *styles {
                        let (rr, rresp) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 0.0).max(desktop::STYLE_ROW),
                            egui::Sense::click(),
                        );
                        if rresp.hovered() {
                            ui.painter().rect_filled(rr, egui::CornerRadius::same(RADIUS), t.hover);
                        }
                        if style == st {
                            // The selected-row marker, one of the accent's shapes (spec §2): an edge,
                            // never an accent-coloured word (§4.3).
                            let edge =
                                egui::Rect::from_min_size(rr.min, egui::vec2(2.0, rr.height()));
                            ui.painter().rect_filled(edge, 0.0, t.accent);
                        }
                        let icon = egui::Rect::from_min_size(
                            rr.left_center() + egui::vec2(space::MD, -STYLE_ICON_SIZE.y / 2.0),
                            STYLE_ICON_SIZE,
                        );
                        chart::draw_style_icon(ui.painter(), icon, st);
                        ui.painter().text(
                            egui::pos2(rr.left() + desktop::STYLE_LABEL_X, rr.center().y),
                            egui::Align2::LEFT_CENTER,
                            st.label(),
                            egui::FontId::proportional(bar_px),
                            t.text_ui,
                        );
                        if rresp.clicked() {
                            a.set_style = Some(st);
                        }
                    }
                    ui.separator();
                }
            });
            ui.add_space(space::MD); // brand→symbol (vike main spacing = 6)
            // symbol + interval are PLAIN clickable text (vike has NO dropdown ▼ arrows) — a
            // frameless button that opens a picker popup, not an egui ComboBox.
            // Cross-venue: a non-Binance chart shows a dim `VENUE:` prefix so the
            // button is unambiguous (Binance keeps its bare symbol — zero change).
            let sym_label = if venue == DEFAULT_VENUE {
                symbol.to_string()
            } else {
                format!("{}:{symbol}", venue.to_uppercase())
            };
            let sresp = ui.add(
                egui::Button::new(egui::RichText::new(&sym_label).size(title_px)).frame(false),
            );
            egui::Popup::menu(&sresp).id(id.with("sym_popup")).show(|ui| {
                // The catalog has not loaded yet (and the node publishes nothing): the built-in
                // quick-picks, as before. These are Binance symbols, so selecting one also resets
                // the venue to Binance — otherwise a quick-pick from a Bybit/OKX chart would keep
                // the wrong venue.
                if catalog.is_empty() && backend.is_empty() {
                    ui.set_min_width(desktop::SYMBOL_MENU_W);
                    for s in SYMS {
                        if ui.selectable_label(venue == DEFAULT_VENUE && symbol == s, s).clicked() {
                            a.new_symbol = Some(s.to_string());
                            a.new_venue = Some(DEFAULT_VENUE.to_string());
                        }
                    }
                    return;
                }
                // The shared picker (`vike_app_core::ui::symbol_picker`): one line per underlying,
                // a chip per venue, spot and perpetual only. The Trade window's picker is the same
                // widget over the same model, so the two cannot drift apart.
                let out = vike_app_core::ui::symbol_picker::symbol_picker(
                    ui,
                    id.with("sym_picker"),
                    &vike_app_core::ui::symbol_picker::PickerSources {
                        catalog,
                        directory,
                        backend,
                        venue,
                        symbol,
                        interval,
                    },
                );
                if let Some(p) = out.pick {
                    a.new_symbol = Some(p.symbol);
                    a.new_venue = Some(p.venue);
                    // Feed-routing slice 1: the picked instrument's asset class, so
                    // `ensure_feed_on` routes non-spot symbols to their native product feed (e.g.
                    // OKX derivatives) instead of the spot bar feed.
                    a.new_asset_class = p.asset_class;
                    a.new_interval = p.interval;
                }
                a.load_venue = out.load_venue;
            });
            ui.add_space(space::MD); // symbol→interval (vike main spacing = 6)
            let iresp = ui
                .add(egui::Button::new(egui::RichText::new(interval).size(title_px)).frame(false));
            egui::Popup::menu(&iresp).id(id.with("ivl_popup")).show(|ui| {
                ui.set_min_width(desktop::INTERVAL_MENU_W);
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
            ui.add_space(space::MD); // interval→sync chip (chart sync seam, task B8)
            // Sync group chip: a filled dot colored by group (1..=4), a hollow gray
            // dot when ungrouped. Click cycles None -> 1 -> 2 -> 3 -> 4 -> None; the
            // actual registry feed/harvest lives in the App::ui window loop (main.rs),
            // this widget only reports the click. The four colours (red, blue, green, yellow) are
            // `ui-theme.toml`'s `desktop` rows `SYNC_GROUP_1`..`SYNC_GROUP_4`.
            const GROUP_COLORS: [egui::Color32; 4] = [
                vike_ui_theme::value::desktop::SYNC_GROUP_1,
                vike_ui_theme::value::desktop::SYNC_GROUP_2,
                vike_ui_theme::value::desktop::SYNC_GROUP_3,
                vike_ui_theme::value::desktop::SYNC_GROUP_4,
            ];
            let (chip_label, chip_color) = match sync_group {
                // Defensive index (never panic on a hand-edited/corrupted out-of-range
                // group in workspace.json — same `.get().unwrap_or(..)` idiom persist.rs
                // uses for `style: usize`): an out-of-1..=4 value just paints the muted grey, as
                // "no group" does — the muted status (spec §3.2), the same in every theme.
                Some(g) => (
                    "●",
                    GROUP_COLORS.get(g.wrapping_sub(1) as usize).copied().unwrap_or(status::MUTED),
                ),
                None => ("○", status::MUTED),
            };
            if ui
                .add(
                    Button::new(RichText::new(chip_label).color(chip_color).size(bar_px))
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
            ui.add_space(space::MD); // sync chip→ƒx (vike main spacing = 6)
            if ui
                .add(vike_ui_theme::components::button::IconButton::new(
                    icons::INDICATORS,
                    "Indicators",
                ))
                .clicked()
            {
                a.open_picker = true;
            }
            ui.add_space(space::MD); // ƒx→OF (SP2 orderflow controls, Task 7)
            // "OF" opens the Orderflow popup: CVD / Volume Profile toggles + a tick-size
            // override. Any of the three ON (this popup's two toggles, or the Footprint
            // STYLE via the style menu above) drives `WinState::orderflow_on()`, which the
            // window loop uses to lazily subscribe the trade feed + register an aggregator
            // (`main.rs`'s `of_wanted` collection). An accent underline marks it while either
            // toggle is on (mirrors the sync chip's filled-vs-hollow "is this active" convention).
            let of_active = cvd_on || profile_on;
            let of_resp = ui
                .button(RichText::new("OF").size(bar_px))
                .on_hover_text("Orderflow: CVD / Volume Profile / tick size");
            if of_active {
                on_marker(ui, of_resp.rect, t.accent);
            }
            egui::Popup::menu(&of_resp).id(id.with("of_popup")).show(|ui| {
                ui.set_min_width(desktop::OF_MENU_W);
                let mut cvd = cvd_on;
                if ui.checkbox(&mut cvd, "CVD").changed() {
                    a.cvd_on = Some(cvd);
                }
                let mut profile = profile_on;
                if ui.checkbox(&mut profile, "Volume Profile").changed() {
                    a.profile_on = Some(profile);
                }
                ui.separator();
                ui.label("Tick size");
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
                ui.label("Backfill hours");
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
            ui.add_space(space::MD); // OF→Compare (C2a Task 4)
            // "Cmp" opens the Compare popup: a symbol entry (Enter adds a
            // %-normalized overlay series), quick-picks from the known symbol list,
            // and a colored chip per current overlay (✕ removes it). Adding the FIRST
            // overlay auto-switches this window's price scale to Percent (see
            // `WinState::add_compare`) so the overlay is visible. An accent underline marks it
            // while any overlay is active (same convention as the OF/sync chips).
            let cmp_active = !compare.is_empty();
            let cmp_resp = ui
                .button(RichText::new("Cmp").size(bar_px))
                .on_hover_text("Compare: overlay another symbol (% scale)");
            if cmp_active {
                on_marker(ui, cmp_resp.rect, t.accent);
            }
            egui::Popup::menu(&cmp_resp).id(id.with("compare_popup")).show(|ui| {
                ui.set_min_width(desktop::CMP_MENU_W);
                ui.label("Compare symbol");
                // Free-text entry. `title_bar` is a stateless free fn, so the cross-frame
                // input buffer lives in egui temp memory keyed by this window's id (the
                // standard egui idiom for transient widget state without a struct field).
                let buf_id = id.with("compare_entry");
                let mut buf = ui.data_mut(|d| d.get_temp::<String>(buf_id).unwrap_or_default());
                let entry = ui.add(
                    egui::TextEdit::singleline(&mut buf)
                        .hint_text("e.g. ETHUSDT")
                        .desired_width(desktop::CMP_ENTRY_W),
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
                    egui::ScrollArea::vertical().max_height(desktop::CMP_LIST_H).show(ui, |ui| {
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
                            ui.add_space(space::SM);
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
                            ui.label(sym.as_str());
                            if ui
                                .add(vike_ui_theme::components::button::IconButton::new(
                                    icons::REMOVE,
                                    "Remove overlay",
                                ))
                                .clicked()
                            {
                                a.remove_compare = Some(sym.clone());
                            }
                            // C2b Task 9: per-symbol placement + scale menu. Own-paned iff
                            // present in `series_pane`; the two placement items are mutually
                            // exclusive on that. Deliberately NO "merge into another pane"
                            // (Task 6 review: the series loop is one-symbol-per-pane) and NO
                            // "Left" pin (Task 7 review: Left silently aliases Right).
                            let (more, _) = submenu(icons::MORE).ui(ui, |ui| {
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
                                submenu("Pin to scale").ui(ui, |ui| {
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
                            icons::named(more, "Placement and scale");
                        });
                    }
                }
            });
            ui.add_space(space::LG); // Compare→LIVE (vike status_margin = 8)
            // "Bars are arriving" is a status: the status green on every theme and market set
            // (spec §3.2), never the market's "up".
            let tone = if feed.is_live() { status::OK } else { ui.visuals().weak_text_color() };
            ui.label(RichText::new(feed.badge()).color(tone).size(bar_px))
                .on_hover_text(feed.hint());
        },
    );

    // RIGHT cluster: window controls — the Body role's glyph in a 20 pt square, four apart, four
    // from the right edge, flat; the same as every other window's bar. Order L→R: ─ □ ✕.
    let controls = bar_rect.with_max_x(bar_rect.max.x - chrome::WINDOW_CONTROLS_PAD);
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(controls.shrink2(egui::vec2(space::NONE, space::HAIR)))
            .layout(Layout::right_to_left(Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = chrome::WINDOW_CONTROL_GAP;
            ui.visuals_mut().button_frame = false;
            let ctrl = |ui: &mut egui::Ui, icon: Icon, tip: &str| -> bool {
                let button = Button::new(icon.rich().size(look.text_size.px(TextRole::Body)));
                icons::named(ui.add_sized(Vec2::splat(chrome::WINDOW_CONTROL), button), tip)
                    .clicked()
            };
            if ctrl(ui, icons::CLOSE, "Close") {
                a.close = true;
            }
            if ctrl(ui, if is_max { icons::RESTORE } else { icons::MAXIMIZE }, "Maximize / restore")
            {
                a.toggle_max = true;
            }
            if ctrl(ui, icons::MINIMIZE, "Minimize to rail") {
                a.minimize = true;
            }
        },
    );

    (a, drag)
}

/// The mark under a title-bar toggle that is on (OF, Cmp): a 2 px underline in the theme's accent.
/// The accent is a shape, never a word's colour (spec §2, §4.3).
fn on_marker(ui: &egui::Ui, under: egui::Rect, accent: egui::Color32) {
    let edge = egui::Stroke::new(stroke::EDGE, accent);
    ui.painter().hline(under.x_range(), under.bottom() + space::HAIR, edge);
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Shape;
    use vike_app_core::ui::series_follow::ChartFeed;
    use vike_ui_theme::appearance::{Appearance, install};
    use vike_ui_theme::market::MarketId;
    use vike_ui_theme::theme::{Theme, ThemeId};
    use vike_ui_theme::type_scale::{TextRole, TextSize};

    fn raw() -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 500.0),
            )),
            ..Default::default()
        }
    }

    fn ctx_with(a: &Appearance) -> egui::Context {
        let ctx = egui::Context::default();
        install(&ctx, a);
        ctx
    }

    /// The shapes of the second of two passes: a popup lays itself out on a sizing pass that paints
    /// nothing.
    fn second_pass(ctx: &egui::Context, mut add: impl FnMut(&mut egui::Ui)) -> Vec<Shape> {
        ctx.run_ui(raw(), |ui| add(ui)).drop_without_applying_deltas();
        let mut out = ctx.run_ui(raw(), |ui| add(ui));
        let mut stack: Vec<Shape> =
            std::mem::take(&mut out.shapes).into_iter().map(|c| c.shape).collect();
        out.drop_without_applying_deltas();
        let mut flat = Vec::new();
        while let Some(s) = stack.pop() {
            match s {
                Shape::Vec(v) => stack.extend(v),
                s => flat.push(s),
            }
        }
        flat
    }

    /// `(text, size, colour)` of every text painted.
    fn texts(shapes: &[Shape]) -> Vec<(String, f32, egui::Color32)> {
        shapes
            .iter()
            .filter_map(|s| match s {
                Shape::Text(t) => {
                    let f = &t.galley.job.sections.first()?.format;
                    let c = if f.color == egui::Color32::PLACEHOLDER {
                        t.fallback_color
                    } else {
                        f.color
                    };
                    Some((
                        t.galley.text().to_string(),
                        f.font_id.size,
                        t.override_text_color.unwrap_or(c),
                    ))
                }
                _ => None,
            })
            .collect()
    }

    /// One chart title bar in window `chart`: sync group 1, the given feed, OF on or off, and the
    /// given compare overlays.
    fn bar(ui: &mut egui::Ui, feed: ChartFeed, of_on: bool, compare: &[String]) {
        let catalog = Catalog::from_instruments(Vec::new());
        let (panes, scales) = (indexmap::IndexMap::new(), indexmap::IndexMap::new());
        let _ = title_bar(
            ui,
            egui::Id::new("chart"),
            "BTCUSDT",
            DEFAULT_VENUE,
            "1m",
            chart::ChartStyle::Candles,
            Some(1),
            false,
            of_on,
            false,
            None,
            0.0,
            compare,
            &panes,
            &scales,
            &catalog,
            &[],
            None,
            feed,
        );
    }

    /// The bar names roles, never pixels: the Title role for the window's title (symbol and
    /// interval), the Body role for its three window controls (the same glyph size as every other
    /// window's bar), the Strong role for the rest. Both text sizes, because at Standard a literal 12
    /// or 14 passes by coincidence.
    #[test]
    fn every_title_bar_text_is_the_title_or_strong_role() {
        for size in TextSize::ALL {
            let ctx = ctx_with(&Appearance { text_size: size, ..Appearance::default() });
            let shapes = second_pass(&ctx, |ui| bar(ui, ChartFeed::Live, true, &[]));
            let roles =
                [size.px(TextRole::Title), size.px(TextRole::Body), size.px(TextRole::Strong)];
            for (text, px, _) in texts(&shapes) {
                assert!(roles.contains(&px), "{size:?}: {text:?} is {px}, not {roles:?}");
            }
        }
    }

    /// "● LIVE" says bars are arriving — a status, so it is the status green on every theme and every
    /// market set (spec §3.2). It was the market's "up", which the Colour-blind set paints blue.
    #[test]
    fn the_live_badge_is_the_status_green_on_every_theme_and_market_set() {
        for theme in ThemeId::ALL {
            for market in MarketId::ALL {
                let ctx = ctx_with(&Appearance { theme, market, ..Appearance::default() });
                let shapes = second_pass(&ctx, |ui| bar(ui, ChartFeed::Live, false, &[]));
                let live = texts(&shapes)
                    .into_iter()
                    .find(|(t, _, _)| t == ChartFeed::Live.badge())
                    .map(|(_, _, c)| c);
                assert_eq!(live, Some(status::OK), "{theme:?} + {market:?}");
            }
        }
    }

    /// OF and Cmp, when on, are marked by an accent SHAPE — a 2 px underline — and their words stay
    /// the text colour (spec §2, §4.3; owner decision 6), on every theme.
    #[test]
    fn an_active_toggle_is_underlined_in_the_accent_and_its_word_is_not() {
        let compare = vec!["ETHUSDT".to_string()];
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            let shapes = second_pass(&ctx, |ui| bar(ui, ChartFeed::Live, true, &compare));
            let accent = Theme::of(id).accent;
            assert!(!texts(&shapes).iter().any(|(_, _, c)| *c == accent), "{id:?}: an accent word");
            let underlines = shapes
                .iter()
                .filter(|s| {
                    matches!(s, Shape::LineSegment { stroke, .. }
                        if stroke.color == accent && stroke.width == 2.0)
                })
                .count();
            assert_eq!(underlines, 2, "{id:?}: OF and Cmp are both on");
        }
    }

    /// The chart-style menu marks the selected style with a 2 px accent edge, not an accent word
    /// (owner decision 6). Its rows are the Strong role, and its headings the caption grey at the
    /// Caption role.
    #[test]
    fn the_style_menu_marks_the_selected_style_with_an_accent_edge() {
        for id in ThemeId::ALL {
            let ctx = ctx_with(&Appearance { theme: id, ..Appearance::default() });
            // A popup FADES IN over `animation_time`, and a fading painter multiplies a rect's fill
            // (a text keeps its colour and carries an opacity factor instead), so on the frames a
            // test runs the edge would be a dimmed accent. No animation: the popup is fully opaque.
            ctx.all_styles_mut(|s| s.animation_time = 0.0);
            egui::Popup::open_id(&ctx, egui::Id::new("chart").with("style_popup"));
            let shapes = second_pass(&ctx, |ui| bar(ui, ChartFeed::Live, false, &[]));
            let t = Theme::of(id);
            let words = texts(&shapes);
            let selected = words
                .iter()
                .find(|(w, _, _)| w == chart::ChartStyle::Candles.label())
                .expect("the style menu is open");
            assert_eq!(selected.2, t.text_ui, "{id:?}: the selected style's word");
            assert_eq!(selected.1, TextSize::default().px(TextRole::Strong), "{id:?}");
            let (heading, _) = chart::STYLE_SECTIONS[0];
            let caption = TextSize::default().px(TextRole::Caption);
            assert!(words.contains(&(heading.to_string(), caption, t.text3)), "{id:?}: {heading}");
            let edges = shapes
                .iter()
                .filter(
                    |s| matches!(s, Shape::Rect(r) if r.fill == t.accent && r.rect.width() == 2.0),
                )
                .count();
            assert_eq!(edges, 1, "{id:?}: one selected row, one edge");
        }
    }
}
