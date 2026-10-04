//! The component gallery (design system spec §8): every kit component, in every theme, at every
//! density — drawn by the app's own renderer off-screen and saved as one PNG per theme with the
//! three densities side by side, `gallery-<theme>.png`.
//!
//! The `png-export` lane (`scripts/ci_feature_suite.sh`) runs it on Mesa's software rasterizer and
//! gates every tile's pixels with `vike_ui_theme::pixel_liveness`; `just qa-shots` captures it on
//! the dev box for a human to judge. Each tile is laid out once to measure it, then drawn at exactly
//! that height, so every quarter of a tile carries components (the quadrant floor below relies on
//! it). In every theme the Normal column turns the window-header gradient on and the other two
//! leave it off, so each sheet shows the header both ways.
//!
//! ```sh
//! cargo run -p vike-ui-theme --features png-export --example gallery            # all four themes
//! cargo run -p vike-ui-theme --features png-export --example gallery -- dusk    # one
//! ```

use std::cell::Cell;

use vike_ui_theme::appearance::{self, Appearance};
use vike_ui_theme::components::button::{ActionButton, IconButton};
use vike_ui_theme::components::chip::{self, Mode, Presence};
use vike_ui_theme::components::input::{self, Field};
use vike_ui_theme::components::overlay;
use vike_ui_theme::components::rail::{self, RailItem};
use vike_ui_theme::components::section;
use vike_ui_theme::components::segmented::{self, Segment};
use vike_ui_theme::components::state::{self, Load};
use vike_ui_theme::components::table::{self, Column};
use vike_ui_theme::components::tabs::{self, Tab};
use vike_ui_theme::components::toggle;
use vike_ui_theme::components::window;
use vike_ui_theme::components::{Status, Tokens};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::Density;
use vike_ui_theme::offscreen::{TexDelta, rasterize};
use vike_ui_theme::pixel_liveness::{LivenessSpec, assert_pixels_live, pixel_report};
use vike_ui_theme::theme::{Theme, ThemeId};
use vike_ui_theme::type_scale::TextRole;

/// Pixels per point: the Studio harness's, for text a human can read on the sheet.
const PPP: f32 = 1.5;
/// One tile's width in points — one density's column.
const TILE_W: f32 = 560.0;
/// The room a tile keeps around its components, in points.
const MARGIN: f32 = 12.0;

/// Liveness floors for ONE TILE. The mechanism and the rebaseline-free argument are
/// `vike_ui_theme::pixel_liveness`'s; the numbers are facts about this page, so they live here.
///
/// - `min_distinct_colors: 64` — a tile carries about forty labelled controls, a table, a rail,
///   seventy-seven icons and every text role, all anti-aliased: a healthy tile measures distinct
///   colours in the thousands on either rasterizer, a dead one measures 1.
/// - `max_dominant_share: 0.95` — the theme's background is the largest flat region of a dense
///   page; a dead tile is 100.00% one colour.
/// - `min_quadrant_distinct: Some(8)` — TRUE of this page by construction: the tile is exactly as
///   tall as its content (`measure`), and every section but the rail spans its width, so every
///   quarter carries components.
const LIVENESS: LivenessSpec = LivenessSpec {
    min_distinct_colors: 64,
    max_dominant_share: 0.95,
    min_quadrant_distinct: Some(8),
};

fn main() {
    let want: Vec<String> = std::env::args().skip(1).collect();
    let themes: Vec<ThemeId> = ThemeId::ALL
        .into_iter()
        .filter(|t| want.is_empty() || want.iter().any(|w| w == t.key()))
        .collect();
    assert!(!themes.is_empty(), "no known theme in {want:?} (graphite|midnight|dusk|carbon)");
    for theme in themes {
        let tiles: Vec<(Density, image::RgbaImage)> = Density::ALL
            .into_iter()
            .map(|d| {
                let a = Appearance {
                    theme,
                    density: d,
                    header_gradient: d == Density::Normal,
                    ..Appearance::default()
                };
                (d, render_tile(&a))
            })
            .collect();
        let sheet = side_by_side(&tiles, Theme::of(theme).bg);
        let path = format!("gallery-{}.png", theme.key());
        sheet.save(&path).expect("write PNG");
        // Liveness AFTER the save, as the other two harnesses do: a red run leaves its PNG behind.
        for (d, img) in &tiles {
            let report = pixel_report(img.as_raw(), img.width(), img.height());
            println!("pixel liveness [{} {}]: {report}", theme.key(), d.key());
            assert_pixels_live(&report, &LIVENESS);
        }
        println!("wrote {path} ({}x{})", sheet.width(), sheet.height());
    }
}

/// One tile: measured, then drawn at exactly its content's height.
fn render_tile(a: &Appearance) -> image::RgbaImage {
    let h = measure(a);
    let (ctx, tex, out) = frames(a, h);
    rasterize(&ctx, tex, out, [(TILE_W * PPP) as u32, (h * PPP) as u32], PPP, "gallery")
}

/// The page's height at this appearance, rounded up to an even number of points, so it is a whole
/// number of pixels at 1.5 pixels per point.
fn measure(a: &Appearance) -> f32 {
    let ctx = context(a);
    let used = Cell::new(0.0f32);
    for f in 0..2 {
        // Two passes: egui settles some sizes (wrapped rows, columns) on the second, and the
        // second is the one measured.
        let out = ctx.run_ui(input(10_000.0, f, None), |ui| {
            used.set(page(ui, a, &Cell::new(None)).max.y);
        });
        out.drop_without_applying_deltas();
    }
    (used.get() / 2.0).ceil() * 2.0
}

fn context(a: &Appearance) -> egui::Context {
    let ctx = egui::Context::default();
    appearance::install(&ctx, a);
    ctx.set_pixels_per_point(PPP);
    ctx
}

fn input(height: f32, frame: u32, pointer: Option<egui::Pos2>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(TILE_W, height))),
        // From one second on: egui's spinner arc is 240° × sin(time), a dot at time zero.
        time: Some(1.0 + f64::from(frame) / 60.0),
        events: pointer.map(|p| vec![egui::Event::PointerMoved(p)]).unwrap_or_default(),
        ..Default::default()
    }
}

/// Three frames on one context, texture deltas accumulated in order (the `export_png.rs` loop).
/// Frame 0 lays the page out and records where the table's third row is; frames 1 and 2 move the
/// pointer there, so the picture shows a hovered row.
fn frames(a: &Appearance, h: f32) -> (egui::Context, Vec<TexDelta>, egui::FullOutput) {
    let ctx = context(a);
    let hover_at = Cell::new(None);
    let mut tex: Vec<TexDelta> = Vec::new();
    let mut last = None;
    for f in 0..3 {
        let pointer = if f == 0 { None } else { hover_at.get() };
        let mut out = ctx.run_ui(input(h, f, pointer), |ui| {
            page(ui, a, &hover_at);
        });
        tex.extend(
            out.textures_delta
                .set
                .iter()
                .flat_map(|(id, deltas)| deltas.iter().map(move |d| (*id, d.clone()))),
        );
        // Harvested above; every frame but the last is dropped, and egui 0.36 panics on a dropped
        // delta list that still holds entries (`export_png.rs` carries the incident).
        out.textures_delta.clear();
        last = Some(out);
    }
    (ctx, tex, last.expect("three frames ran"))
}

/// The three density tiles left to right, on the theme's background.
fn side_by_side(tiles: &[(Density, image::RgbaImage)], bg: egui::Color32) -> image::RgbaImage {
    let w: u32 = tiles.iter().map(|(_, t)| t.width()).sum();
    let h: u32 = tiles.iter().map(|(_, t)| t.height()).max().unwrap_or(1);
    let [r, g, b, _] = bg.to_array();
    let mut sheet = image::RgbaImage::from_pixel(w, h, image::Rgba([r, g, b, 255]));
    let mut x = 0i64;
    for (_, t) in tiles {
        image::imageops::replace(&mut sheet, t, x, 0);
        x += i64::from(t.width());
    }
    sheet
}

fn section_title(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.add_space(t.metrics.gap);
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .font(t.font(TextRole::Caption))
            .color(t.theme.text3),
    );
}

/// Every component, top to bottom. Returns the rect the page used, and records the table's third
/// row in `hover_at`.
fn page(ui: &mut egui::Ui, a: &Appearance, hover_at: &Cell<Option<egui::Pos2>>) -> egui::Rect {
    let t = Tokens::of(ui.ctx());
    egui::Frame::new()
        .fill(t.theme.bg)
        .inner_margin(egui::Margin::same(MARGIN as i8))
        .show(ui, |ui| {
            ui.set_width(TILE_W - 2.0 * MARGIN);
            ui.spacing_mut().item_spacing.y = t.metrics.gap;
            let gradient = if a.header_gradient { "on" } else { "off" };
            let title =
                format!("{} · {} · header gradient {gradient}", a.theme.key(), a.density.key());
            ui.label(egui::RichText::new(title).font(t.font(TextRole::Title)).color(t.theme.text));

            section_title(ui, &t, "Window header");
            window::header(ui, icons::DATA, "Data Manager", false);
            window::header(ui, icons::CHART, "BTCUSDT · 1m", true);

            section_title(ui, &t, "Buttons");
            ui.horizontal_wrapped(|ui| {
                ui.add(ActionButton::primary("Save"));
                ui.add(ActionButton::secondary((icons::REFRESH, "Refresh")));
                ui.add(ActionButton::buy("Buy"));
                ui.add(ActionButton::sell("Sell"));
                ui.add(ActionButton::danger((icons::DELETE, "Delete")));
                ui.add(IconButton::new(icons::SETTINGS, "Settings"));
            });
            ui.horizontal_wrapped(|ui| {
                ui.add(ActionButton::primary("Apply").disabled_because("Nothing changed"));
                ui.add(
                    ActionButton::secondary((icons::BACKFILL, "Backfill"))
                        .disabled_because("No store"),
                );
            });

            section_title(ui, &t, "Tabs and segmented control");
            tabs::underline(
                ui,
                &mut 0u8,
                &[
                    Tab { value: 0, label: "Credentials", count: Some(12) },
                    Tab { value: 1, label: "Backend", count: None },
                    Tab { value: 2, label: "Accounts", count: Some(0) },
                ],
            );
            segmented::segmented(
                ui,
                &mut 1u8,
                &[
                    Segment { value: 0, label: "1D", why: "One day" },
                    Segment { value: 1, label: "1W", why: "One week" },
                    Segment { value: 2, label: "1M", why: "One month" },
                ],
            );

            section_title(ui, &t, "Inputs");
            input::text(
                ui,
                &mut String::new(),
                Field { hint: "Search symbols", ..Field::default() },
            );
            input::text(ui, &mut "BTCUSDT".to_string(), Field::default()).request_focus();
            input::text(
                ui,
                &mut "kraken".to_string(),
                Field { error: Some("Unknown venue"), ..Field::default() },
            );
            input::number(
                ui,
                &mut "0.25".to_string(),
                Field { unit: Some("BTC"), ..Field::default() },
            );
            input::number(
                ui,
                &mut "abc".to_string(),
                Field { unit: Some("USD"), ..Field::default() },
            );

            section_title(ui, &t, "Switch, checkbox, radio");
            ui.horizontal_wrapped(|ui| {
                toggle::switch(ui, &mut true, "Live orders");
                toggle::switch(ui, &mut false, "Sound");
                toggle::checkbox(ui, &mut true, "Show volume");
                toggle::checkbox(ui, &mut false, "Show gaps");
                toggle::radio(ui, &mut 0u8, 0, "UTC");
                toggle::radio(ui, &mut 0u8, 1, "Local");
            });

            section_title(ui, &t, "Chips, dots, badges and marks");
            ui.horizontal_wrapped(|ui| {
                for m in Mode::ALL {
                    chip::mode(ui, m);
                }
                chip::account(ui, "binance · main", Mode::Live, true);
                chip::account(ui, "binance · hedge", Mode::Demo, false);
            });
            ui.horizontal_wrapped(|ui| {
                for (s, word) in [
                    (Status::Ok, "Connected"),
                    (Status::Warning, "Reconnecting"),
                    (Status::Error, "Failed"),
                    (Status::Info, "Syncing"),
                    (Status::Muted, "Off"),
                ] {
                    chip::status_dot(ui, s, word);
                }
            });
            ui.horizontal_wrapped(|ui| {
                chip::count(ui, Some(3));
                chip::count(ui, Some(1204));
                chip::count(ui, None);
                chip::badge(ui, "READ-ONLY", Status::Warning);
                chip::badge(ui, "MUTED", Status::Muted);
            });
            ui.horizontal_wrapped(|ui| {
                for m in Presence::ALL {
                    chip::presence(ui, m, "BINANCE_LIVE_API_KEY");
                }
                chip::presence_legend(ui);
            });

            section_title(ui, &t, "Sections");
            section::section(
                ui,
                "gallery-section",
                "Positions",
                |ui| {
                    ui.add(ActionButton::secondary((icons::REFRESH, "Refresh")));
                },
                |ui| {
                    ui.horizontal(|ui| {
                        section::meta_cell(ui, "Account", "binance · main", false);
                        section::meta_cell(ui, "Key", "BINANCE_LIVE_API_KEY", true);
                    });
                },
            );
            section::breadcrumb(ui, &["Data Manager", "All series"], "1,204 series · 3 venues");
            section::context_bar(ui, |ui| {
                chip::status_dot(ui, Status::Ok, "local store · 1,204 series");
            });

            section_title(ui, &t, "Data table");
            let cols = [
                Column { title: "Symbol", weight: 2.0, min_w: 60.0, numeric: false },
                Column { title: "Venue", weight: 1.5, min_w: 50.0, numeric: false },
                Column { title: "Bars", weight: 1.0, min_w: 40.0, numeric: true },
                Column { title: "Last", weight: 1.0, min_w: 50.0, numeric: true },
            ];
            let rows = [
                ["BTCUSDT", "binance", "43200", "64,102.5"],
                ["ETHUSDT", "binance", "43200", "3,120.25"],
                ["SOL-PERP", "hyperliquid", "12876", "142.87"],
                ["a symbol name long enough to be truncated", "okx", "9", "0.0042"],
                ["XAUUSD", "dukascopy", "7214", "2,401.10"],
                ["TKN", "polymarket", "300", "0.31"],
            ];
            let seen =
                table::data_table(ui, "gallery-table", &cols, rows.len(), Some(1), |r, c| {
                    rows[r][c].to_string()
                });
            if let Some((_, row)) = seen.rows.get(2) {
                hover_at.set(Some(row.center()));
            }

            section_title(ui, &t, "Navigation rail");
            ui.allocate_ui(egui::vec2(220.0, 0.0), |ui| {
                rail::nav_rail(
                    ui,
                    &mut 1u8,
                    &[
                        RailItem {
                            value: 0,
                            group: "Browse",
                            icon: icons::OVERVIEW,
                            label: "Overview",
                            count: None,
                        },
                        RailItem {
                            value: 1,
                            group: "Browse",
                            icon: icons::ALL_SERIES,
                            label: "All series",
                            count: Some(1204),
                        },
                        RailItem {
                            value: 2,
                            group: "Browse",
                            icon: icons::HAS_GAPS,
                            label: "Has gaps",
                            count: Some(3),
                        },
                        RailItem {
                            value: 3,
                            group: "Live",
                            icon: icons::CACHED_FEEDS,
                            label: "Cached feeds",
                            count: Some(2),
                        },
                        RailItem {
                            value: 4,
                            group: "Live",
                            icon: icons::ACTIVITY_LOG,
                            label: "Activity log",
                            count: None,
                        },
                    ],
                );
            });

            section_title(ui, &t, "Menu, tooltip, toasts");
            ui.horizontal_top(|ui| {
                // A `Frame` lays its contents out in its PARENT's layout, and this row is
                // horizontal: the menu and the tooltip each stack top-down, as egui's own do.
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    ui.set_width(200.0);
                    ui.vertical(|ui| {
                        overlay::menu_item(ui, Some(icons::SAVE), "Save layout", Some("Ctrl+S"));
                        overlay::menu_item(ui, None, "Load layout", None);
                        overlay::menu_item(ui, Some(icons::DELETE), "Delete layout", None);
                    });
                });
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.vertical(|ui| {
                        overlay::tooltip_body(
                            ui,
                            "Stops the selected subscription; its cached bars go with it.",
                        );
                    });
                });
            });
            for (s, text) in [
                (Status::Ok, "Saved to the store"),
                (Status::Warning, "Rate limited — retrying in 5 s"),
                (Status::Error, "Backfill failed: the venue did not answer"),
            ] {
                overlay::toast_body(ui, s, text);
            }

            section_title(ui, &t, "Panes with no rows");
            ui.columns(3, |cols| {
                state::view(&mut cols[0], Load::Loading("Reading the store…"));
                state::view(&mut cols[1], Load::Empty("No series match this filter"));
                state::view(&mut cols[2], Load::Unreachable("The datahub did not answer"));
            });

            section_title(ui, &t, "Icons");
            ui.horizontal_wrapped(|ui| {
                for (name, icon) in icons::ALL {
                    let glyph = icon.rich().size(t.text.px(TextRole::Title)).color(t.theme.text2);
                    ui.label(glyph).on_hover_text(*name);
                }
            });

            section_title(ui, &t, "Brand");
            let (r, _) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), egui::Sense::hover());
            vike_ui_theme::brand::paint_mark(ui.painter(), r);

            section_title(ui, &t, "Text roles");
            for role in TextRole::ALL {
                let line = format!("{role:?} · {} px", t.text.px(role));
                ui.label(egui::RichText::new(line).font(t.font(role)).color(t.theme.text));
            }
        })
        .response
        .rect
}
