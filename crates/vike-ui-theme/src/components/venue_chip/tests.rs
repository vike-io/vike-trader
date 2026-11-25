//! Tests for `venue_chip`: the venue chip, the source chip and the underlying row.

use super::*;
use crate::appearance::ThemeAppearance;
use crate::components::ThemeStatus;
use crate::components::testing::{circle_strokes, circles, harness, named, paint, strokes};
use crate::painted::context_with;
use crate::theme::{ThemePalette, ThemePaletteId};
use egui::accesskit::Role;
use std::cell::Cell;
use std::rc::Rc;

fn chip<'a>(label: &'a str, mark: ThemeVenueChipMark, why: &'a str) -> ThemeVenueChip<'a> {
    ThemeVenueChip { label, mark, current: false, cold: false, why }
}

/// The mode is a dot whose SHAPE carries it, on every theme: LIVE is the only filled one, and
/// it is filled in the accent; DEMO is a ring in the info blue; PAPER a ring in the
/// caption grey. (The pill-not-dot rule of `chip::account` is a different component's.)
#[test]
fn a_mode_is_a_dot_whose_shape_carries_it() {
    for id in ThemePaletteId::ALL {
        let context = context_with(&ThemeAppearance { theme: id, ..ThemeAppearance::default() });
        let th = ThemePalette::of(id);
        let one = |m: AccountTier| {
            paint(&context, |ui| {
                venue_chip(ui, &chip("X", ThemeVenueChipMark::Tier(m), "x"));
            })
        };
        let live = one(AccountTier::Live);
        assert!(circles(&live).contains(&th.accent), "{id:?}: LIVE is a filled accent dot");
        let demo = one(AccountTier::Demo);
        assert!(
            circles(&demo).iter().all(|c| *c == egui::Color32::TRANSPARENT),
            "{id:?}: DEMO is a ring, never filled"
        );
        assert!(circle_strokes(&demo).contains(&ThemeStatus::Info.color()), "{id:?}");
        let paper = one(AccountTier::Paper);
        assert!(circle_strokes(&paper).contains(&th.text_tertiary), "{id:?}");
        assert!(
            !circles(&paper).contains(&th.accent) && !circles(&demo).contains(&th.accent),
            "{id:?}: only LIVE is filled in the accent"
        );
    }
}

/// A chip with no account and a cold one draw a DASHED outline — line segments, and no
/// bordered rectangle — where a plain chip draws the bordered rectangle.
#[test]
fn no_account_and_cold_are_dashed() {
    let context = context_with(&ThemeAppearance::default());
    let th = ThemePalette::of(ThemeAppearance::default().theme);
    let plain = paint(&context, |ui| {
        venue_chip(ui, &chip("OKX", ThemeVenueChipMark::Hidden, "OKX"));
    });
    assert!(strokes(&plain).contains(&th.border) && !has_segments(&plain));
    for dashed_chip in [
        chip("HL", ThemeVenueChipMark::NoAccount, "HL, no account — view only"),
        ThemeVenueChip {
            cold: true,
            ..chip("ASTER", ThemeVenueChipMark::Hidden, "Aster, not loaded — click to load")
        },
    ] {
        let shapes = paint(&context, |ui| {
            venue_chip(ui, &dashed_chip);
        });
        assert!(has_segments(&shapes), "{}: dashed outline", dashed_chip.label);
        assert!(!plain_rect_edge(&shapes), "{}: no solid border", dashed_chip.label);
    }
}

fn has_segments(shapes: &[Shape]) -> bool {
    shapes.iter().any(|s| matches!(s, Shape::LineSegment { .. }))
}

/// True when some rectangle paints a visible stroke: a plain chip's border.
fn plain_rect_edge(shapes: &[Shape]) -> bool {
    shapes
        .iter()
        .any(|s| matches!(s, Shape::Rect(r) if r.stroke.width > 0.0 && r.stroke.color.a() > 0))
}

/// The mark is never the only carrier: the accessible name is the `why` text, in words, and
/// the current chip is a Label while the others are Buttons.
#[test]
fn the_chip_is_named_in_words_and_the_current_one_is_a_label() {
    let h = harness(ThemeAppearance::default(), |ui| {
        venue_chip(
            ui,
            &chip("BYBIT", ThemeVenueChipMark::Tier(AccountTier::Demo), "Bybit, DEMO account"),
        );
        venue_chip(
            ui,
            &ThemeVenueChip {
                current: true,
                ..chip("BINANCE", ThemeVenueChipMark::Hidden, "Binance, this window")
            },
        );
    });
    assert!(named(&h, Role::Button).iter().any(|n| n == "Bybit, DEMO account"));
    assert!(named(&h, Role::Label).iter().any(|n| n.contains("Binance, this window")));
}

/// Review Focus 6: a line with every roster venue wraps its chips instead of clipping them off
/// the popup's edge, and is taller than a one-line row.
#[test]
fn a_line_with_every_venue_wraps_instead_of_clipping() {
    let context = context_with(&ThemeAppearance::default());
    let labels = [
        "BINANCE",
        "BYBIT",
        "OKX",
        "HYPERLIQUID",
        "DERIBIT",
        "ASTER",
        "ALPACA",
        "OANDA",
        "CTRADER",
        "IG",
        "DUKASCOPY",
        "FXCM",
        "IBKR",
        "POLYMARKET",
    ];
    let chips: Vec<ThemeVenueChip<'_>> =
        labels.iter().map(|l| chip(l, ThemeVenueChipMark::Hidden, l)).collect();
    let tall = Rc::new(Cell::new(0.0_f32));
    let one = Rc::new(Cell::new(0.0_f32));
    let (t2, o2) = (tall.clone(), one.clone());
    let shapes = paint(&context, |ui| {
        let region = Rect::from_min_size(pos2(0.0, 0.0), vec2(470.0, 600.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(region), |ui| {
            let before = ui.cursor().top();
            underlying_row(
                ui,
                "many",
                &ThemeVenueChipUnderlyingRow {
                    heading: "BTC",
                    kind: "perpetual",
                    chips: &chips,
                    hot: false,
                    current: false,
                },
            );
            t2.set(ui.cursor().top() - before);
            let before = ui.cursor().top();
            underlying_row(
                ui,
                "few",
                &ThemeVenueChipUnderlyingRow {
                    heading: "BTC",
                    kind: "spot",
                    chips: &chips[..2],
                    hot: false,
                    current: false,
                },
            );
            o2.set(ui.cursor().top() - before);
        });
    });
    assert!(tall.get() > one.get() * 1.5, "wrapped {} vs one line {}", tall.get(), one.get());
    for s in &shapes {
        if let Shape::Rect(r) = s {
            assert!(
                r.rect.right() <= 470.5,
                "a chip ends at {} outside the 470 pt row",
                r.rect.right()
            );
        }
    }
}

/// The row answers which part was clicked: a chip click is `Chip(i)` and never also `Row`.
#[test]
fn a_chip_click_is_not_a_row_click() {
    use egui_kittest::kittest::Queryable;
    let picked: Rc<Cell<Option<ThemeVenueChipUnderlyingRowPick>>> = Rc::new(Cell::new(None));
    let p2 = picked.clone();
    let chips = [
        chip("BYBIT", ThemeVenueChipMark::Hidden, "Bybit"),
        chip("BINANCE", ThemeVenueChipMark::Hidden, "Binance"),
    ];
    let mut h = harness(ThemeAppearance::default(), move |ui| {
        let got = underlying_row(
            ui,
            "r",
            &ThemeVenueChipUnderlyingRow {
                heading: "BTC",
                kind: "perpetual",
                chips: &chips,
                hot: false,
                current: false,
            },
        );
        if got.is_some() {
            p2.set(got);
        }
    });
    h.get_by_label("Binance").click();
    h.run();
    assert_eq!(picked.get(), Some(ThemeVenueChipUnderlyingRowPick::Chip(1)));
}

/// A source chip is a TOGGLE (announced as a checkbox, still clickable while on), and a cold one
/// is a dashed plain button.
#[test]
fn a_source_chip_is_a_toggle_and_a_cold_one_is_dashed() {
    use egui_kittest::kittest::Queryable;
    let on_clicked = Rc::new(Cell::new(false));
    let o2 = on_clicked.clone();
    let mut h = harness(ThemeAppearance::default(), move |ui| {
        if source_chip(ui, "Bybit", true, false, "Bybit: 1,402 listed").clicked() {
            o2.set(true);
        }
        source_chip(ui, "Aster", false, true, "Aster, not loaded — click to load");
    });
    assert!(named(&h, Role::CheckBox).iter().any(|n| n == "Bybit: 1,402 listed"));
    assert!(named(&h, Role::Button).iter().any(|n| n == "Aster, not loaded — click to load"));
    h.get_by_label("Bybit: 1,402 listed").click();
    h.run();
    assert!(on_clicked.get(), "a chip that is ON can still be clicked, to turn it off");
    let context = context_with(&ThemeAppearance::default());
    let shapes = paint(&context, |ui| {
        source_chip(ui, "Aster", false, true, "Aster");
    });
    assert!(has_segments(&shapes), "a cold source chip is dashed");
}

/// A flat row is one Button named "symbol, pair, venue", clickable across its whole width.
#[test]
fn a_flat_row_is_one_named_button_and_a_click_answers() {
    use egui_kittest::kittest::Queryable;
    let clicked = Rc::new(Cell::new(false));
    let c2 = clicked.clone();
    let mut h = harness(ThemeAppearance::default(), move |ui| {
        let row = ThemeVenueChipFlatRow {
            symbol: "BTC-USDT-SWAP",
            pair: "BTC/USDT perpetual",
            venue: "okx",
            hot: false,
            current: false,
        };
        if flat_row(ui, "r", &row).clicked() {
            c2.set(true);
        }
    });
    assert!(named(&h, Role::Button).iter().any(|n| n == "BTC-USDT-SWAP, BTC/USDT perpetual, okx"));
    h.get_by_label("BTC-USDT-SWAP, BTC/USDT perpetual, okx").click();
    h.run();
    assert!(clicked.get());
}
