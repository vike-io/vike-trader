//! The DOM under the design system, read off what a REAL `draw` paints (`Harness::output`): the
//! installed theme, market colours, text size and density reach every row, bar, text and marker.
//! No GPU — the shapes are egui's own CPU output, so this runs on the GPU-less CI runners.
//!
//! The shape readers below are this file's own: the kit's twins (`vike_ui_theme`'s
//! `components::testing`) are private to that crate.

use egui::{Color32, Rect, Shape, Stroke};
use egui_kittest::Harness;
use vike_model::{BookLevel, L2Book, VenueCaps};
use vike_panels::DomMode;
use vike_panels::{DomInputs, DomOrder, DomPosition, DomState};
use vike_ui_theme::appearance::Appearance;
use vike_ui_theme::components::{Status, Tokens, chip};
use vike_ui_theme::icons;
use vike_ui_theme::market::{MarketColors, MarketId};
use vike_ui_theme::metrics::Density;
use vike_ui_theme::theme::{Theme, ThemeId};
use vike_ui_theme::type_scale::{TextRole, TextSize};

/// Everything one frame of the DOM is handed, owned.
struct Scene {
    book: L2Book,
    last: Option<f64>,
    orders: Vec<DomOrder>,
    position: Option<DomPosition>,
    stale: bool,
    source: &'static str,
}

/// Bids 99/98/97 and asks 100/101/102 on a 1.0 tick, the last price 99.5, a live link.
fn scene() -> Scene {
    let mut book = L2Book::new(1.0);
    book.apply_snapshot(
        1,
        &[BookLevel::new(99.0, 1.0), BookLevel::new(98.0, 2.0), BookLevel::new(97.0, 3.0)],
        &[BookLevel::new(100.0, 1.0), BookLevel::new(101.0, 2.0), BookLevel::new(102.0, 3.0)],
    );
    Scene {
        book,
        last: Some(99.5),
        orders: Vec::new(),
        position: None,
        stale: false,
        source: "datahub 127.0.0.1:7878 — 1/1 stream(s) live",
    }
}

/// No levels on either side and no mark, over the link line `source`.
fn bookless(source: &'static str) -> Scene {
    Scene { book: L2Book::new(1.0), last: None, source, ..scene() }
}

/// A resting limit BUY and a resting stop SELL, one on each side, both on drawn rows.
fn orders() -> Vec<DomOrder> {
    let order = |id: &str, side, price, is_stop| DomOrder {
        client_order_id: id.to_string(),
        side,
        price,
        qty: 0.01,
        is_stop,
        filled_qty: 0.0,
    };
    vec![order("limit-buy", 1, 98.0, false), order("stop-sell", -1, 96.0, true)]
}

/// The DOM under appearance `a`, at 900 × 700 — the size the accessibility suites use, wide enough
/// that nothing is clipped — run until it settles.
fn dom(a: Appearance, s: Scene, state: DomState) -> Harness<'static, DomState> {
    let mut h = Harness::builder().with_size(egui::vec2(900.0, 700.0)).build_ui_state(
        move |ui, st: &mut DomState| {
            if !vike_ui_theme::harness::appearance_ready(ui.ctx(), &a) {
                return;
            }
            let inputs = DomInputs {
                book: &s.book,
                last: s.last,
                orders: &s.orders,
                position: s.position,
                stale: s.stale,
                paper: true,
                caps: VenueCaps::UNSUPPORTED,
                source: s.source,
                absence: None,
            };
            let _ = vike_panels::dom::draw(ui, st, &inputs);
        },
        state,
    );
    h.run();
    h
}

/// Every shape the last frame painted, `Shape::Vec`s flattened.
fn shapes(h: &Harness<'static, DomState>) -> Vec<Shape> {
    fn flatten(s: Shape, out: &mut Vec<Shape>) {
        match s {
            Shape::Vec(v) => v.into_iter().for_each(|s| flatten(s, out)),
            s => out.push(s),
        }
    }
    let mut out = Vec::new();
    for c in &h.output().shapes {
        flatten(c.shape.clone(), &mut out);
    }
    out
}

/// `(rect, fill, stroke)` of every rectangle painted.
fn rects(s: &[Shape]) -> Vec<(Rect, Color32, Stroke)> {
    s.iter()
        .filter_map(|s| match s {
            Shape::Rect(r) => Some((r.rect, r.fill, r.stroke)),
            _ => None,
        })
        .collect()
}

fn fills(s: &[Shape]) -> Vec<Color32> {
    rects(s).into_iter().map(|(_, f, _)| f).collect()
}

/// The stroke colour of every line segment painted.
fn segments(s: &[Shape]) -> Vec<Color32> {
    s.iter()
        .filter_map(|s| match s {
            Shape::LineSegment { stroke, .. } => Some(stroke.color),
            _ => None,
        })
        .collect()
}

/// `(text, colour, sizes)` of every text painted: the colour as the painter resolves it, and the
/// size of every section that is NOT in the icon family (icons are sized by the kit, text by roles).
fn texts(s: &[Shape]) -> Vec<(String, Color32, Vec<f32>)> {
    let icons = vike_ui_theme::icons::family();
    s.iter()
        .filter_map(|s| match s {
            Shape::Text(t) => {
                let first = t.galley.job.sections.first();
                let c = first
                    .map(|s| s.format.color)
                    .filter(|c| *c != Color32::PLACEHOLDER)
                    .unwrap_or(t.fallback_color);
                let sizes = t
                    .galley
                    .job
                    .sections
                    .iter()
                    .filter(|s| s.format.font_id.family != icons)
                    .map(|s| s.format.font_id.size)
                    .collect();
                Some((t.galley.text().to_string(), t.override_text_color.unwrap_or(c), sizes))
            }
            _ => None,
        })
        .collect()
}

/// Where the text reading exactly `want` was painted.
fn text_pos(s: &[Shape], want: &str) -> Option<egui::Pos2> {
    s.iter().find_map(|s| match s {
        Shape::Text(t) if t.galley.text() == want => Some(t.pos),
        _ => None,
    })
}

/// The ladder's row pitch is the density's row height (spec §3.4: "Table / ladder row", 16/18/22),
/// read off the price column: on a 1.0 tick at grouping 1, prices 100 and 99 are adjacent rows.
#[test]
fn the_ladder_rows_follow_the_density() {
    for d in Density::ALL {
        let s = shapes(&dom(
            Appearance { density: d, ..Appearance::default() },
            scene(),
            DomState::default(),
        ));
        let y = |p: &str| {
            text_pos(&s, p).unwrap_or_else(|| panic!("{d:?}: price {p} was not painted")).y
        };
        let pitch = y("99") - y("100");
        assert!(near(pitch, d.metrics().row_h), "{d:?}: rows are {pitch} pt apart");
    }
}

/// Two layout lengths are the same length. Rows sit at integer offsets today, so the difference is
/// exact; the 0.01 pt allows for float arithmetic, and is a thousandth of the smallest row.
fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

/// The ladder paints in the INSTALLED theme — every theme, not only the default whose colours a
/// compile-time constant would also match: zebra rows on the surface, the last price outlined in the
/// accent (spec §2: the last-price outline), the spread line in the analysis line (spec §3.2), and
/// a stale ladder under the theme's scrim.
#[test]
fn the_ladder_paints_in_each_installed_theme() {
    let row = Density::Normal.metrics().row_h;
    for id in ThemeId::ALL {
        let th = Theme::of(id);
        let a = Appearance { theme: id, ..Appearance::default() };
        let s = shapes(&dom(a, scene(), DomState::default()));
        let r = rects(&s);
        // A zebra row spans the ladder (wider than any kit control); a kit control is control_h
        // tall, not row_h, so neither check can be met by a segment or a button.
        assert!(
            r.iter()
                .any(|(rc, f, _)| *f == th.surface && near(rc.height(), row) && rc.width() > 400.0),
            "{id:?}: no zebra row on the surface"
        );
        assert!(
            r.iter().any(|(rc, _, st)| st.color == th.accent && near(rc.height(), row)),
            "{id:?}: the last price is not outlined in the accent"
        );
        assert!(segments(&s).contains(&th.analysis_line), "{id:?}: no analysis line");
        let stale = shapes(&dom(a, Scene { stale: true, ..scene() }, DomState::default()));
        assert!(fills(&stale).contains(&th.scrim()), "{id:?}: a stale ladder is not dimmed");
    }
}

/// Depth bars are the market set's depth fills, and the sizes its TEXT colours (spec §3.2): bids
/// 99@1 and asks 100@1 each paint "1.00", one in each side's colour.
#[test]
fn depth_bars_and_sizes_follow_each_market_set() {
    for m in MarketId::ALL {
        let c = MarketColors::of(m);
        let s = shapes(&dom(
            Appearance { market: m, ..Appearance::default() },
            scene(),
            DomState::default(),
        ));
        let f = fills(&s);
        assert!(f.contains(&c.up_depth) && f.contains(&c.down_depth), "{m:?}: depth bars");
        let tx = texts(&s);
        assert!(tx.iter().any(|(t, col, _)| t == "1.00" && *col == c.up_text), "{m:?}: bid size");
        assert!(tx.iter().any(|(t, col, _)| t == "1.00" && *col == c.down_text), "{m:?}: ask size");
    }
}

/// The heatmap is the shared ramp. `tick: 5` makes the first drawn frame (tick 6) push a column,
/// and the fullest cell in it — asks 102@3 — is the ramp's hot end.
#[test]
fn the_heatmap_is_the_shared_ramp() {
    let st = DomState { mode: DomMode::Elite, tick: 5, ..DomState::default() };
    let s = shapes(&dom(Appearance::default(), scene(), st));
    assert!(fills(&s).contains(&vike_ui_theme::heat::ramp(1.0)));
}

/// The feed word is an outlined status badge in the FIXED status colours (spec §3.2), whatever the
/// theme: the kit's badge ink for the state `source_status` gives it.
#[test]
fn the_feed_word_is_its_status_colour() {
    let t = Tokens::from_appearance(&Appearance::default());
    for (source, word, status) in [
        ("datahub 127.0.0.1:7878 — 1/1 stream(s) live", "FEED UP", Status::Ok),
        ("datahub 127.0.0.1:7878 — connecting, 0/1 stream(s)", "FEED DIALLING", Status::Warning),
        ("idle", "FEED DOWN", Status::Error),
        ("datahub error: connection refused", "FEED FAULT", Status::Error),
        ("datahub 127.0.0.1:7878 — no streams wanted on this venue", "FEED ?", Status::Muted),
    ] {
        let s =
            shapes(&dom(Appearance::default(), Scene { source, ..scene() }, DomState::default()));
        let ink = chip::badge_ink(&t, status);
        assert!(
            texts(&s).iter().any(|(tx, c, _)| tx == word && *c == ink),
            "{source:?}: {word} is not painted in {ink:?}"
        );
    }
}

/// A busy ladder — stale, a position, both kinds of order, Reduce and C2F on — and an empty one
/// over a link that is down: between them, every string, badge, marker and control the DOM draws.
fn both_states() -> [(&'static str, Scene, DomState); 2] {
    let busy = Scene {
        stale: true,
        orders: orders(),
        position: Some(DomPosition { size: 0.5, avg_px: 99.0, upnl: 1.25 }),
        ..scene()
    };
    let armed = DomState { cost_to_fill: true, reduce_only: true, ..DomState::default() };
    [("a busy ladder", busy, armed), ("no book, link down", bookless("idle"), DomState::default())]
}

/// Every text the DOM draws is a ROLE size (spec §3.3: "Code names a role and never a pixel size"),
/// at both text sizes. The ratchet catches a literal; this catches a COMPUTED size no role has.
/// Icons are sized by the kit and are not asked (`texts` leaves the icon family out).
#[test]
fn every_text_the_dom_draws_is_a_role_size() {
    for size in TextSize::ALL {
        let roles: Vec<f32> = TextRole::ALL.iter().map(|r| size.px(*r)).collect();
        for (what, sc, st) in both_states() {
            let s = shapes(&dom(Appearance { text_size: size, ..Appearance::default() }, sc, st));
            for (text, _, sizes) in texts(&s) {
                for px in sizes {
                    assert!(roles.contains(&px), "{size:?}, {what}: {text:?} is drawn at {px} pt");
                }
            }
        }
    }
}

/// The trading palette and the DOM's own private colours, as VALUES: none may be painted once the
/// DOM reads the theme (spec §9 step 7). They are spelled as literals because the palette they came
/// from is deleted in this order of work. (A function, not a `const`: `from_rgba_unmultiplied` is
/// not a `const fn`.) The market set is the default, Classic — Exchange's pair IS the old trading
/// green and red, and legitimately so.
fn retired() -> [(&'static str, Color32); 13] {
    [
        ("PANEL", Color32::from_rgb(15, 19, 27)),
        ("PANEL2", Color32::from_rgb(12, 16, 23)),
        ("RULE", Color32::from_rgb(29, 36, 45)),
        ("TXT", Color32::from_rgb(210, 214, 220)),
        ("MUTED", Color32::from_rgb(140, 149, 160)),
        ("FAINT", Color32::from_rgb(88, 99, 115)),
        ("ACCENT, the DOM's LAST", Color32::from_rgb(240, 180, 41)),
        ("UP, the DOM's BID", Color32::from_rgb(46, 189, 133)),
        ("DOWN, the DOM's ASK", Color32::from_rgb(246, 70, 93)),
        ("the mode button's text", Color32::from_rgb(18, 16, 10)),
        ("the marker letter", Color32::from_rgb(10, 13, 17)),
        ("the marker outline", Color32::from_gray(235)),
        ("the stale overlay", Color32::from_rgba_unmultiplied(8, 11, 15, 150)),
    ]
}

#[test]
fn nothing_is_painted_in_the_trading_palette() {
    for id in ThemeId::ALL {
        for (what, sc, st) in both_states() {
            let s = shapes(&dom(Appearance { theme: id, ..Appearance::default() }, sc, st));
            let mut painted: Vec<Color32> = fills(&s);
            painted.extend(rects(&s).into_iter().map(|(_, _, stroke)| stroke.color));
            painted.extend(segments(&s));
            painted.extend(texts(&s).into_iter().map(|(_, c, _)| c));
            for (name, c) in retired() {
                assert!(!painted.contains(&c), "{id:?}, {what}: {name} {c:?} is still painted");
            }
        }
    }
}

/// Loading, empty and unreachable are three renderings (spec §4.2), and all three are STILL (owner
/// decision 5): no icon while connecting, the tray over a live link with no depth, the unreachable
/// cloud in the warning colour over a link that is down. An icon is found by its codepoint as
/// painted text (`Icon::accessible_label("")`).
#[test]
fn the_three_bookless_states_render_differently() {
    let tray = icons::EMPTY.accessible_label("");
    let cloud = icons::UNREACHABLE.accessible_label("");
    let painted =
        |source| texts(&shapes(&dom(Appearance::default(), bookless(source), DomState::default())));
    let connecting = painted("datahub 127.0.0.1:7878 — connecting, 0/1 stream(s)");
    assert!(
        !connecting.iter().any(|(t, ..)| *t == tray || *t == cloud),
        "connecting: {connecting:?}"
    );
    let up = painted("datahub 127.0.0.1:7878 — 1/1 stream(s) live");
    assert!(up.iter().any(|(t, ..)| *t == tray), "link up, no depth: {up:?}");
    let down = painted("idle");
    assert!(down.iter().any(|(t, c, _)| *t == cloud && *c == Status::Warning.color()), "{down:?}");
}

/// A control strip holds its controls. The header, the toolbar and the footer are each one
/// control high plus an inset (`strips`: +2 pt, the footer +4 pt), and what a strip lays out is
/// clipped to it (`strip`), so a control laid out lower than its strip would lose its bottom
/// edge — the chosen segment's accent outline first. Checked at every density, on what a real
/// `draw` paints: every shape clipped to a control strip lies inside it, top to bottom.
///
/// ⚠ Pinned because egui's `horizontal_wrapped` — the row the kit's segmented control lays
/// itself out in — opens that row at `Spacing::interact_size.y`, not at the density's control
/// height. With egui's 18 pt default a 24 pt segment sat 3 pt low and ran 2 pt past its strip
/// (4 pt at Comfortable), measured 2026-09-29 against the shipped kit. `strip` now sets
/// `interact_size.y` to the density's control height, the same kind of DOM-owned layout wrapping
/// `one_line` already is.
#[test]
fn the_control_strips_hold_their_controls_at_every_density() {
    for d in Density::ALL {
        let m = d.metrics();
        // header and toolbar, then the footer
        let control_strips = [m.control_h + 2.0, m.control_h + 4.0];
        let st = DomState { cost_to_fill: true, ..DomState::default() };
        let a = Appearance { density: d, ..Appearance::default() };
        let h = dom(a, Scene { stale: true, ..scene() }, st);
        let mut checked = 0;
        for c in &h.output().shapes {
            if !control_strips.iter().any(|sh| near(c.clip_rect.height(), *sh)) {
                continue;
            }
            checked += 1;
            let r = c.shape.visual_bounding_rect();
            assert!(
                r.min.y >= c.clip_rect.min.y - 0.5 && r.max.y <= c.clip_rect.max.y + 0.5,
                "{d:?}: a shape at y {:.1}..{:.1} spills past its strip at y {:.1}..{:.1}",
                r.min.y,
                r.max.y,
                c.clip_rect.min.y,
                c.clip_rect.max.y
            );
        }
        assert!(
            checked > 0,
            "{d:?}: nothing was clipped to a control strip, so nothing was checked"
        );
    }
}
