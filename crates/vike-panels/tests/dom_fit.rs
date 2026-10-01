//! The DOM fits the width it is given (finding F1 of the panels plan, deferred by its decision 7).
//!
//! The launcher opens a DOM 320 pt wide, and the header, toolbar and footer were laid out on ONE row
//! each, so at that width STALE was cut off, half the toolbar (Reduce and Recenter among it) lay past
//! the edge, and the footer's buttons sat on top of the position readout. Nothing measured any of it:
//! the one guard, `dom_a11y.rs`'s `a_narrow_dom_keeps_every_order_size_on_the_toolbar_row`, proved a
//! control never WRAPPED onto the ladder, which says nothing about a control being cut off.
//!
//! Every test here reads positions off the accessibility tree, so a GPU-less runner proves it. The DOM
//! is drawn at a sweep of widths, in every appearance, with the footer's opt-in cost-to-fill readout
//! on and off, a stale book, a long price and an open position — the widest content each strip ever
//! holds — and three properties must hold at every one of them:
//!
//! 1. **Nothing is cut off.** Every control lies inside the content area, `x` from 8 to `width − 8`
//!    (`egui_kittest`'s central panel keeps an 8 pt margin; the strips fill what is left of it).
//! 2. **Nothing lies on anything else.** No two controls overlap.
//! 3. **The strips are allocated, not spilled.** Everything above the ladder sits above the toolbar's
//!    top, and the ladder keeps its own band, at least its five-row floor, between the toolbar and the
//!    footer — so a row a strip adds pushes the ladder down instead of lying over it (a ladder's
//!    click-to-trade region is laid out after the strips and wins any click on a control that
//!    spilled onto it).
//!
//! The source strip's long address is exempt from the first property: it is informational, it is cut
//! off on purpose, and its state is the badge at its left, which always fits.
//!
//! Both surfaces are swept, Pro and Elite. They share the header, the toolbar and the footer and
//! differ only in the ladder area (Elite splits it into a liquidity heatmap and the ladder), so the
//! strips must stack the same way in both — which is a claim about the code until this measures it.
//!
//! Each test collects EVERY violation over its whole sweep and reports them together, so one run says
//! where a threshold is wrong and by how much rather than only the first width that broke.

use std::collections::BTreeMap;

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;
use vike_model::{BookLevel, L2Book, VenueCaps};
use vike_panels::dom::DomPosition;
use vike_panels::{DomInputs, DomMode, DomState};
use vike_ui_theme::appearance::Appearance;
use vike_ui_theme::metrics::Density;
use vike_ui_theme::type_scale::TextSize;

/// Half a point: the tolerance for the rounding egui does to a rect's edges.
const EPS: f32 = 0.6;

/// The DOM's five order sizes, as the toolbar labels them.
const SIZES: [&str; 5] = ["0.0010", "0.0050", "0.0100", "0.0500", "0.1000"];

/// One control's accessible name and where it landed.
struct Control {
    text: String,
    rect: egui::Rect,
}

fn book() -> L2Book {
    let mut b = L2Book::new(1.0);
    b.apply_snapshot(
        1,
        &[BookLevel::new(99.0, 1.0), BookLevel::new(98.0, 2.0), BookLevel::new(97.0, 3.0)],
        &[BookLevel::new(100.0, 1.0), BookLevel::new(101.0, 2.0), BookLevel::new(102.0, 3.0)],
    );
    b
}

fn appearance(density: Density, text_size: TextSize) -> Appearance {
    Appearance { density, text_size, ..Appearance::default() }
}

/// A DOM under the appearance `a`, drawn with the widest content each strip holds: PAPER (the wider
/// mode chip), a long price, a stale book (the STALE badge), an open position with a loss (the
/// longest readout) and, when `cost_to_fill`, the footer's two cost readouts, on the surface `mode`.
/// Built ONCE per look — installing the bundled fonts is what a harness spends its time on — and
/// resized between widths.
fn dom(a: Appearance, cost_to_fill: bool, mode: DomMode) -> Harness<'static, DomState> {
    let book = book();
    Harness::builder().with_size(egui::vec2(900.0, 560.0)).build_ui_state(
        move |ui, dom: &mut DomState| {
            if !vike_ui_theme::harness::appearance_ready(ui.ctx(), &a) {
                return;
            }
            let inputs = DomInputs {
                book: &book,
                last: Some(65_432.1),
                orders: &[],
                position: Some(DomPosition { size: 0.0123, avg_px: 65_432.1, upnl: -123.45 }),
                stale: true,
                paper: true,
                caps: VenueCaps::UNSUPPORTED,
                source: "datahub 127.0.0.1:7878 — 1/1 stream(s) live",
                absence: None,
            };
            let _ = vike_panels::dom::draw(ui, dom, &inputs);
        },
        DomState { cost_to_fill, mode, ..DomState::default() },
    )
}

/// The two surfaces a DOM window renders.
const MODES: [DomMode; 2] = [DomMode::Pro, DomMode::Elite];

/// Every button, check box and label `h` draws in a `width` × 560 window.
fn drawn(h: &mut Harness<'static, DomState>, width: f32) -> Vec<Control> {
    h.set_size(egui::vec2(width, 560.0));
    h.run();
    h.root()
        .children_recursive()
        .filter_map(|n| {
            let node = n.accesskit_node();
            if !matches!(node.role(), Role::Button | Role::CheckBox | Role::Label) {
                return None;
            }
            let text: String = node.label().or_else(|| node.value()).unwrap_or_default();
            (!text.is_empty()).then(|| Control { text, rect: n.rect() })
        })
        .collect()
}

/// The toolbar's controls: the size readout, the five sizes, the price grouping, Recenter and the
/// two check boxes.
fn is_toolbar(c: &Control) -> bool {
    let t = c.text.as_str();
    t.starts_with("qty ")
        || SIZES.contains(&t)
        || t == "group"
        || t == "1"
        || t.ends_with("price grouping")
        || t == "Recenter on last"
        || t == "Reduce"
        || t == "C2F"
}

/// The source strip: its caption, its badge and its address.
fn is_source(c: &Control) -> bool {
    c.text == "depth" || c.text == "FEED UP" || c.text.starts_with("datahub")
}

/// The DOM's strips, told apart by where they lie relative to the toolbar.
struct Bands<'a> {
    header: Vec<&'a Control>,
    toolbar: Vec<&'a Control>,
    footer: Vec<&'a Control>,
}

fn bands(cs: &[Control]) -> Bands<'_> {
    let toolbar: Vec<&Control> = cs.iter().filter(|c| is_toolbar(c)).collect();
    let top = toolbar.iter().map(|c| c.rect.min.y).fold(f32::MAX, f32::min);
    let bottom = toolbar.iter().map(|c| c.rect.max.y).fold(f32::MIN, f32::max);
    let header =
        cs.iter().filter(|c| !is_toolbar(c) && !is_source(c) && c.rect.center().y < top).collect();
    let footer = cs.iter().filter(|c| !is_toolbar(c) && c.rect.center().y > bottom).collect();
    Bands { header, toolbar, footer }
}

/// How many separate rows `cs` sit on: controls whose vertical centres are 3 pt apart or less share
/// one.
fn rows_in<'a>(cs: impl Iterator<Item = &'a Control>) -> usize {
    let mut ys: Vec<f32> = cs.map(|c| c.rect.center().y).collect();
    ys.sort_by(f32::total_cmp);
    let mut rows = 0;
    let mut last = f32::NEG_INFINITY;
    for y in ys {
        if y - last > 3.0 {
            rows += 1;
        }
        last = y;
    }
    rows
}

fn rows_of(cs: &[Control]) -> (usize, usize, usize) {
    let b = bands(cs);
    (
        rows_in(b.header.iter().copied()),
        rows_in(b.toolbar.iter().copied()),
        rows_in(b.footer.iter().copied()),
    )
}

/// One way a property failed: under which look, what failed (worded without the width, so the same
/// failure at neighbouring widths is ONE kind) and at which width.
struct Violation {
    name: String,
    what: String,
    width: f32,
}

/// Every way the three properties fail at one width under one appearance.
fn violations(name: &str, width: f32, density: Density, cs: &[Control]) -> Vec<Violation> {
    let mut out = Vec::new();
    let mut fail = |what: String| out.push(Violation { name: name.to_string(), what, width });
    // 1. nothing is cut off
    let (left, right) = (8.0, width - 8.0);
    for c in cs.iter().filter(|c| !is_source(c)) {
        if c.rect.min.x < left - EPS || c.rect.max.x > right + EPS {
            fail(format!("{:?} lies outside the content area", c.text));
        }
    }
    // 2. nothing lies on anything else
    for (i, a) in cs.iter().enumerate() {
        for b in &cs[i + 1..] {
            let both = a.rect.intersect(b.rect);
            if both.width() > EPS && both.height() > EPS {
                fail(format!("{:?} lies over {:?}", a.text, b.text));
            }
        }
    }
    // 3. the strips are allocated, not spilled
    let b = bands(cs);
    let top = b.toolbar.iter().map(|c| c.rect.min.y).fold(f32::MAX, f32::min);
    let bottom = b.toolbar.iter().map(|c| c.rect.max.y).fold(f32::MIN, f32::max);
    for c in &b.header {
        if c.rect.max.y > top + EPS {
            fail(format!("header control {:?} reaches into the toolbar", c.text));
        }
    }
    let footer_top = b.footer.iter().map(|c| c.rect.min.y).fold(f32::MAX, f32::min);
    if footer_top - bottom < 5.0 * density.metrics().row_h {
        fail("the ladder is under its five-row floor".to_string());
    }
    out
}

/// Fails, if `all` is not empty, with the violations grouped by kind: how many widths each broke at
/// and the narrowest and widest of them.
fn assert_none(what: &str, all: &[Violation]) {
    let mut kinds: BTreeMap<(&str, &str), (usize, f32, f32)> = BTreeMap::new();
    for v in all {
        let k = kinds.entry((v.name.as_str(), v.what.as_str())).or_insert((0, f32::MAX, f32::MIN));
        *k = (k.0 + 1, k.1.min(v.width), k.2.max(v.width));
    }
    let lines: Vec<String> = kinds
        .iter()
        .take(60)
        .map(|((name, what), (n, lo, hi))| format!("{name}: {what} at {n} width(s), {lo}..{hi}"))
        .collect();
    assert!(
        all.is_empty(),
        "{what}: {} violation(s) of {} kind(s):\n{}",
        all.len(),
        kinds.len(),
        lines.join("\n")
    );
}

/// The default look, at every width from below the launcher's own (296 leaves 24 pt for whatever
/// margin the workspace's window frame takes) up to a wide window, with the footer's cost readout
/// off and on. The rows a strip uses may only FALL as the window widens: a strip that gained a row at
/// a wider width would be a layout that flickers on a drag.
#[test]
fn the_default_appearance_fits_every_width_from_the_launchers_up() {
    let mut all = Vec::new();
    for mode in MODES {
        for cost_to_fill in [false, true] {
            let name =
                format!("Graphite / Normal / Standard, {mode:?}, cost-to-fill {cost_to_fill}");
            let mut h = dom(Appearance::default(), cost_to_fill, mode);
            let mut before: Option<(usize, usize, usize)> = None;
            for width in (296..=900).step_by(2) {
                let width = width as f32;
                let cs = drawn(&mut h, width);
                all.extend(violations(&name, width, Density::Normal, &cs));
                let rows = rows_of(&cs);
                if let Some(was) = before
                    && (rows.0 > was.0 || rows.1 > was.1 || rows.2 > was.2)
                {
                    all.push(Violation {
                        name: name.clone(),
                        what: format!(
                            "a strip gained a row as the window widened: {was:?} -> {rows:?}"
                        ),
                        width,
                    });
                }
                before = Some(rows);
            }
        }
    }
    assert_none("the default appearance", &all);
}

/// Denser, looser and larger looks take the same widths: a wider control needs a wider threshold,
/// and this is the test that says so when it does not have one. From 320 up — the launcher's own
/// width — because the workspace's margin is measured under the default look only.
#[test]
fn every_density_and_text_size_fits_the_same_widths() {
    let mut all = Vec::new();
    for mode in MODES {
        for density in Density::ALL {
            for text_size in TextSize::ALL {
                for cost_to_fill in [false, true] {
                    let name = format!(
                        "{density:?} / {text_size:?}, {mode:?}, cost-to-fill {cost_to_fill}"
                    );
                    let mut h = dom(appearance(density, text_size), cost_to_fill, mode);
                    for width in (320..=900).step_by(4) {
                        let width = width as f32;
                        let cs = drawn(&mut h, width);
                        all.extend(violations(&name, width, density, &cs));
                    }
                }
            }
        }
    }
    assert_none("every density and text size", &all);
}

/// A window wide enough for everything keeps every strip on ONE row, as it always has.
#[test]
fn a_wide_dom_keeps_each_strip_on_one_row() {
    for mode in MODES {
        let cs = drawn(&mut dom(Appearance::default(), false, mode), 900.0);
        assert_eq!(rows_of(&cs), (1, 1, 1), "{mode:?}");
    }
}
