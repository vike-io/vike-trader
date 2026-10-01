//! The cockpit on the design system (spec §2, §3, §7): every OPAQUE colour a widget paints is a
//! token of the INSTALLED appearance, every text is a role size, no text is the theme's accent, and
//! the layout follows density and text size. `Context::run_ui` is pure CPU, so this runs on the
//! GPU-less runners.
//!
//! A translucent colour is a token painted through a faded painter (the migration plan's decision
//! 6); the test that owns each one un-premultiplies it back to its token with [`unfaded`].

use egui::epaint::ColorMode;
use egui::{Color32, FontId, Shape};
use vike_ui_theme::appearance::{self, Appearance};
use vike_ui_theme::components::{ON_FILL, Status};
use vike_ui_theme::market::{MarketColors, MarketId};
use vike_ui_theme::metrics::Density;
use vike_ui_theme::theme::{Theme, ThemeId};
use vike_ui_theme::type_scale::{TextRole, TextSize};

/// A 5-minute window's open, epoch ms (`vike_model::fair::UPDOWN_WINDOW_SECS` is 300).
const OPEN: i64 = 1_724_000_100_000;
/// The window length, in seconds.
const WIN: i64 = 300;
/// 42 s into the window: 4:18 to its close, +4:18 and +9:18 to the next two opens.
const NOW: i64 = OPEN + 42_000;
/// 5 s before the close: the countdown is urgent.
const NOW_URGENT: i64 = OPEN + 295_000;

/// A context with `a` installed and one empty pass run, so the bundled faces are bound before the
/// pass under test (`set_fonts` takes effect on the next pass).
fn ctx_with(a: &Appearance) -> egui::Context {
    let ctx = egui::Context::default();
    appearance::install(&ctx, a);
    ctx.run_ui(raw(), |_| {}).drop_without_applying_deltas();
    ctx
}

/// An 800 × 600 point screen.
fn raw() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
        ..Default::default()
    }
}

/// Every shape one pass of `add` paints, `Shape::Vec`s flattened, after the frame has passed the
/// shared geometry assertion.
fn paint(ctx: &egui::Context, mut add: impl FnMut(&mut egui::Ui)) -> Vec<Shape> {
    let mut out = ctx.run_ui(raw(), |ui| add(ui));
    vike_ui_theme::frame_sanity::assert_frame_sane(&out);
    let clipped = std::mem::take(&mut out.shapes);
    out.drop_without_applying_deltas();
    let mut flat = Vec::new();
    for c in clipped {
        flatten(c.shape, &mut flat);
    }
    flat
}

fn flatten(shape: Shape, out: &mut Vec<Shape>) {
    match shape {
        Shape::Vec(v) => v.into_iter().for_each(|s| flatten(s, out)),
        s => out.push(s),
    }
}

/// `(text, colour, font)` of every text section painted — a placeholder colour resolved to the
/// shape's fallback, and an override applied, as the painter resolves them.
fn texts(shapes: &[Shape]) -> Vec<(String, Color32, FontId)> {
    let mut out = Vec::new();
    for s in shapes {
        if let Shape::Text(t) = s {
            for sec in &t.galley.job.sections {
                let own = sec.format.color;
                let c = if own == Color32::PLACEHOLDER { t.fallback_color } else { own };
                // epaint 0.36's `ByteRange` is a range of `ByteIndex`, a `usize` newtype.
                let bytes = sec.byte_range.start.0..sec.byte_range.end.0;
                let text = t.galley.job.text[bytes].to_string();
                out.push((text, t.override_text_color.unwrap_or(c), sec.format.font_id.clone()));
            }
        }
    }
    out
}

/// Every colour the shapes carry, with where it came from, for the failure message.
fn colours(shapes: &[Shape]) -> Vec<(Color32, &'static str)> {
    let mut out = Vec::new();
    for s in shapes {
        match s {
            Shape::Rect(r) => {
                out.push((r.fill, "rect fill"));
                out.push((r.stroke.color, "rect stroke"));
            }
            Shape::LineSegment { stroke, .. } => out.push((stroke.color, "line")),
            Shape::Circle(c) => {
                out.push((c.fill, "circle fill"));
                out.push((c.stroke.color, "circle stroke"));
            }
            Shape::Path(p) => {
                out.push((p.fill, "path fill"));
                if let ColorMode::Solid(c) = p.stroke.color {
                    out.push((c, "path stroke"));
                }
            }
            Shape::Mesh(m) => out.extend(m.vertices.iter().map(|v| (v.color, "mesh vertex"))),
            _ => {}
        }
    }
    out.extend(texts(shapes).into_iter().map(|(_, c, _)| (c, "text")));
    out
}

/// A painted colour with its alpha divided back out, and that alpha: the token a faded painter
/// painted, to within the rounding a low alpha costs.
fn unfaded(c: Color32) -> ([u8; 3], u8) {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    ([r, g, b], a)
}

/// Within `tol` of `want` on every channel.
fn near(rgb: [u8; 3], want: Color32, tol: u8) -> bool {
    rgb.iter().zip([want.r(), want.g(), want.b()]).all(|(x, y)| x.abs_diff(y) <= tol)
}

/// Each theme with each market set, at the default density and text size.
fn every_theme_and_market() -> Vec<Appearance> {
    ThemeId::ALL
        .into_iter()
        .flat_map(|theme| {
            MarketId::ALL.into_iter().map(move |market| Appearance {
                theme,
                market,
                ..Appearance::default()
            })
        })
        .collect()
}

/// The opaque colours `a` lets a cockpit widget paint: the theme's, the market set's graphic and
/// text colours, the five status colours and the kit's on-fill black.
fn tokens(a: &Appearance) -> Vec<Color32> {
    let t = Theme::of(a.theme);
    let m = MarketColors::of(a.market);
    let mut v = vec![
        t.bg,
        t.grad_top,
        t.surface,
        t.card,
        t.hover,
        t.border,
        t.text,
        t.text_ui,
        t.text2,
        t.text3,
        t.analysis_line,
        t.accent,
        m.up,
        m.down,
        m.up_text,
        m.down_text,
        ON_FILL,
    ];
    v.extend(Status::ALL.map(Status::color));
    v
}

/// Every OPAQUE colour painted is a token of the installed appearance (spec §3). A translucent one
/// is a faded token and is checked by the test that owns it.
#[track_caller]
fn assert_only_tokens(shapes: &[Shape], a: &Appearance, what: &str) {
    let allowed = tokens(a);
    let strays: Vec<String> = colours(shapes)
        .into_iter()
        .filter(|(c, _)| c.a() == 255 && !allowed.contains(c))
        .map(|(c, from)| format!("{from} {c:?}"))
        .collect();
    assert!(
        strays.is_empty(),
        "{what} under {a:?} painted colours that are not tokens: {strays:#?}"
    );
}

/// Every text painted is one of the seven role sizes on `size`'s scale (spec §3.3).
#[track_caller]
fn assert_role_sizes(shapes: &[Shape], size: TextSize, what: &str) {
    let roles: Vec<f32> = TextRole::ALL.iter().map(|r| size.px(*r)).collect();
    let off: Vec<String> = texts(shapes)
        .into_iter()
        .filter(|(_, _, f)| !roles.contains(&f.size))
        .map(|(t, _, f)| format!("{t:?} at {}", f.size))
        .collect();
    assert!(off.is_empty(), "{what} at {size:?} painted sizes off the scale: {off:#?}");
}

/// The accent is a shape, never the colour of a number or a word (spec §2).
#[track_caller]
fn assert_no_accent_text(shapes: &[Shape], a: &Appearance, what: &str) {
    let accent = Theme::of(a.theme).accent;
    let hits: Vec<String> =
        texts(shapes).into_iter().filter(|(_, c, _)| *c == accent).map(|(t, _, _)| t).collect();
    assert!(hits.is_empty(), "{what} under {:?} painted text in the accent: {hits:?}", a.theme);
}

/// The helpers see what they claim: a planted stray colour, an off-scale size and accent text all
/// show up. Without this, every assertion above could pass by seeing nothing.
#[test]
fn the_helpers_see_a_stray_colour_an_off_scale_size_and_accent_text() {
    let a = Appearance::default();
    let accent = Theme::of(a.theme).accent;
    let shapes = paint(&ctx_with(&a), |ui| {
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(4.0, 4.0));
        ui.painter().rect_filled(r, 0.0, Color32::from_rgb(1, 2, 3));
        ui.label(egui::RichText::new("nine").size(9.0).color(accent));
    });
    assert!(colours(&shapes).iter().any(|(c, _)| *c == Color32::from_rgb(1, 2, 3)));
    assert!(texts(&shapes).iter().any(|(t, c, f)| t == "nine" && *c == accent && f.size == 9.0));
}

// ── the window-chain rail ─────────────────────────────────────────────────────────────────────

/// The rail at `now`: three cards (the current window and the next two), `selected` picked.
fn rail(ui: &mut egui::Ui, now: i64, selected: Option<usize>) {
    let cards: Vec<vike_cockpit::WindowCard> = vike_cockpit::chain_windows(now, WIN, 2)
        .iter()
        .map(|w| vike_cockpit::WindowCard {
            open_ms: w.open_ms,
            up_price: Some(0.62),
            dn_price: Some(0.38),
            volume: Some(12_300.0),
        })
        .collect();
    let mut state = vike_cockpit::ChainRailState { selected };
    let inputs = vike_cockpit::ChainRailInputs {
        asset: "BTC",
        cards: &cards,
        window_secs: WIN,
        now_ms: now,
    };
    let _ = vike_cockpit::draw_chain_rail(ui, &mut state, &inputs);
}

#[test]
fn the_rail_paints_only_tokens_and_no_text_in_the_accent() {
    for a in every_theme_and_market() {
        for (now, selected) in [(NOW, Some(1)), (NOW_URGENT, None)] {
            let shapes = paint(&ctx_with(&a), |ui| rail(ui, now, selected));
            assert_only_tokens(&shapes, &a, "the rail");
            assert_no_accent_text(&shapes, &a, "the rail");
        }
    }
}

#[test]
fn the_rail_paints_role_sizes_only() {
    for text_size in TextSize::ALL {
        let a = Appearance { text_size, ..Appearance::default() };
        let shapes = paint(&ctx_with(&a), |ui| rail(ui, NOW, Some(1)));
        assert_role_sizes(&shapes, text_size, "the rail");
    }
}

/// The accent marks a SELECTION and nothing else: with nothing picked it is nowhere, and with one
/// card picked exactly that card carries the 2 px edge.
#[test]
fn the_selected_card_is_outlined_in_each_themes_accent_and_nothing_else_is() {
    for theme in ThemeId::ALL {
        let a = Appearance { theme, ..Appearance::default() };
        let accent = Theme::of(theme).accent;
        let none = paint(&ctx_with(&a), |ui| rail(ui, NOW, None));
        assert!(!colours(&none).iter().any(|(c, _)| *c == accent), "{theme:?}: nothing is picked");
        let one = paint(&ctx_with(&a), |ui| rail(ui, NOW, Some(1)));
        let edges = one
            .iter()
            .filter(|s| {
                matches!(s, Shape::Rect(r)
                    if r.stroke.color == accent && r.stroke.width == 2.0)
            })
            .count();
        assert_eq!(edges, 1, "{theme:?}: exactly the picked card carries the accent edge");
    }
}

/// A countdown running out is the status red in every market set. Under Colour-blind the market
/// "down" is orange, which is why it may not be "down".
#[test]
fn the_urgent_countdown_is_the_status_red_under_every_market_set() {
    for market in MarketId::ALL {
        let a = Appearance { market, ..Appearance::default() };
        let t = texts(&paint(&ctx_with(&a), |ui| rail(ui, NOW_URGENT, None)));
        assert!(
            t.iter().any(|(s, c, _)| s == "0:05" && *c == Status::Error.color()),
            "{market:?}: {t:?}"
        );
    }
}

#[test]
fn the_current_countdown_is_display_semibold_in_the_text_colour() {
    for theme in ThemeId::ALL {
        let a = Appearance { theme, ..Appearance::default() };
        let t = texts(&paint(&ctx_with(&a), |ui| rail(ui, NOW, None)));
        let want = egui::FontFamily::Name(vike_ui_theme::fonts::MONO_SEMIBOLD.into());
        assert!(
            t.iter().any(|(s, c, f)| s == "4:18"
                && *c == Theme::of(theme).text
                && f.size == TextSize::Standard.px(TextRole::Display)
                && f.family == want),
            "{theme:?}: {t:?}"
        );
    }
}

#[test]
fn the_odds_are_the_market_sets_text_colours() {
    for market in MarketId::ALL {
        let a = Appearance { market, ..Appearance::default() };
        let m = MarketColors::of(market);
        let t = texts(&paint(&ctx_with(&a), |ui| rail(ui, NOW, None)));
        assert!(t.iter().any(|(s, c, _)| s == "U 0.62" && *c == m.up_text), "{market:?}: {t:?}");
        assert!(t.iter().any(|(s, c, _)| s == "D 0.38" && *c == m.down_text), "{market:?}: {t:?}");
    }
}

/// A card holds its five texts — asset, volume, countdown, up and down odds — inside its edges,
/// none overlapping another, at every density and both text sizes. It judges what the viewer sees:
/// each text's painted extent (`TextShape::visual_bounding_rect`, the measure `frame_sanity`'s
/// clipped-text check uses), because the rail places painter text by anchor, not by layout.
#[test]
fn the_rail_cards_hold_their_text_at_every_density_and_text_size() {
    /// `chain.rs`'s `CARD_W`: a card is the opaque 118 px fill, whatever colour a build paints it.
    const CARD_W: f32 = 118.0;
    for density in Density::ALL {
        for text_size in TextSize::ALL {
            let a = Appearance { density, text_size, ..Appearance::default() };
            let shapes = paint(&ctx_with(&a), |ui| rail(ui, NOW, None));
            let cards: Vec<egui::Rect> = shapes
                .iter()
                .filter_map(|s| match s {
                    Shape::Rect(r)
                        if r.fill.a() == 255 && (r.rect.width() - CARD_W).abs() < 0.01 =>
                    {
                        Some(r.rect)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(cards.len(), 3, "{density:?} {text_size:?}: three cards");
            let inks: Vec<egui::Rect> = shapes
                .iter()
                .filter_map(|s| match s {
                    Shape::Text(t) => Some(t.visual_bounding_rect()),
                    _ => None,
                })
                .collect();
            for card in &cards {
                let inside: Vec<&egui::Rect> =
                    inks.iter().filter(|r| card.contains(r.center())).collect();
                assert_eq!(inside.len(), 5, "{density:?} {text_size:?}: {inside:?}");
                for r in &inside {
                    assert!(
                        card.expand(0.5).contains_rect(**r),
                        "{density:?} {text_size:?}: {r:?} leaves {card:?}"
                    );
                }
                for (i, x) in inside.iter().enumerate() {
                    for y in &inside[i + 1..] {
                        assert!(
                            !x.intersects(**y),
                            "{density:?} {text_size:?}: {x:?} overlaps {y:?}"
                        );
                    }
                }
            }
        }
    }
}

// ── the Price-to-Beat header ──────────────────────────────────────────────────────────────────

/// The header at `now`, spot above the reference (Up is winning). Returns the rect it took.
fn header(ui: &mut egui::Ui, now: i64) -> egui::Rect {
    let inputs = vike_cockpit::PtbInputs {
        reference_price: 60_000.0,
        spot: 60_012.5,
        resolution_ts: OPEN + WIN * 1000,
        now_ms: now,
        up_price: Some(0.62),
        dn_price: Some(0.38),
    };
    vike_cockpit::draw_ptb_header(ui, &inputs);
    ui.min_rect()
}

#[test]
fn the_header_paints_only_tokens_and_no_text_in_the_accent() {
    for a in every_theme_and_market() {
        for now in [NOW, NOW_URGENT] {
            let shapes = paint(&ctx_with(&a), |ui| {
                header(ui, now);
            });
            assert_only_tokens(&shapes, &a, "the header");
            assert_no_accent_text(&shapes, &a, "the header");
        }
    }
}

#[test]
fn the_header_paints_role_sizes_only() {
    for text_size in TextSize::ALL {
        let a = Appearance { text_size, ..Appearance::default() };
        let shapes = paint(&ctx_with(&a), |ui| {
            header(ui, NOW);
        });
        assert_role_sizes(&shapes, text_size, "the header");
    }
}

/// THE cockpit countdown is the Hero role (spec §3.3; decision 3): 40 px JetBrains Mono SemiBold,
/// in the text colour, never the accent.
#[test]
fn the_countdown_is_the_hero_role_in_the_text_colour_in_every_theme() {
    let semibold = egui::FontFamily::Name(vike_ui_theme::fonts::MONO_SEMIBOLD.into());
    for theme in ThemeId::ALL {
        let a = Appearance { theme, ..Appearance::default() };
        let t = texts(&paint(&ctx_with(&a), |ui| {
            header(ui, NOW);
        }));
        assert!(
            t.iter().any(|(s, c, f)| s == "4:18"
                && *c == Theme::of(theme).text
                && f.size == TextSize::Standard.px(TextRole::Hero)
                && f.family == semibold),
            "{theme:?}: {t:?}"
        );
    }
}

#[test]
fn the_urgent_header_countdown_is_the_status_red_under_every_market_set() {
    for market in MarketId::ALL {
        let a = Appearance { market, ..Appearance::default() };
        let t = texts(&paint(&ctx_with(&a), |ui| {
            header(ui, NOW_URGENT);
        }));
        assert!(
            t.iter().any(|(s, c, _)| s == "0:05" && *c == Status::Error.color()),
            "{market:?}: {t:?}"
        );
    }
}

#[test]
fn spot_delta_and_odds_are_the_market_sets_text_colours() {
    for market in MarketId::ALL {
        let a = Appearance { market, ..Appearance::default() };
        let m = MarketColors::of(market);
        let t = texts(&paint(&ctx_with(&a), |ui| {
            header(ui, NOW);
        }));
        for (s, want) in [
            ("60012.50", m.up_text),
            ("+12.50", m.up_text),
            ("Up 0.62", m.up_text),
            ("Dn 0.38", m.down_text),
        ] {
            assert!(t.iter().any(|(x, c, _)| x == s && *c == want), "{market:?} {s}: {t:?}");
        }
    }
}

/// The Hero countdown's ROW stays inside the header at both text sizes. The header's texts are
/// labels, and egui lays a label out by its row box — for Hero a 52.8 px row — so a strip shorter
/// than that row lays the label out across its own edges, over the rule and into the ticket's gap.
/// The digits' ink alone would fit a 44 px strip, which is why this measures the row, not the ink.
///
/// The row box is the galley's OWN rect placed at the shape's `pos`, never a rect grown rightward
/// from `pos`: the countdown sits in a right-to-left layout, so egui lays it out right-aligned and
/// its galley rect ENDS at `pos` (epaint's `Galley::rect`: with `Align::RIGHT`, `rect.right()` is
/// 0). Measured the other way, today's right-aligned 22 px countdown read as `[790, 842]` — past
/// the 800 px strip — while it paints at `[738, 790]`.
#[test]
fn the_header_holds_the_hero_countdown_at_every_text_size() {
    for text_size in TextSize::ALL {
        let a = Appearance { text_size, ..Appearance::default() };
        let mut rect = egui::Rect::NOTHING;
        let shapes = paint(&ctx_with(&a), |ui| rect = header(ui, NOW));
        assert!(
            rect.height() >= 44.0,
            "{text_size:?}: the header never shrinks below today's 44 px"
        );
        let row = shapes
            .iter()
            .find_map(|s| match s {
                Shape::Text(t) if t.galley.text() == "4:18" => {
                    Some(t.galley.rect.translate(t.pos.to_vec2()))
                }
                _ => None,
            })
            .expect("the countdown is painted");
        assert!(rect.expand(0.5).contains_rect(row), "{text_size:?}: {row:?} leaves {rect:?}");
    }
}

// ── the probability ladder ────────────────────────────────────────────────────────────────────

/// Bids on 40–48¢ at 900 down to 100 shares, asks on 52–60¢ at 110 up to 990; inside 48 / 52,
/// so 49–51 is the spread. A resting BUY at 45¢ and a take-profit on the ask side at 55¢.
fn ladder(ui: &mut egui::Ui, stale: bool, selected: Option<i64>) {
    let levels: Vec<vike_cockpit::ProbLevel> = (40..=60)
        .map(|c: i64| vike_cockpit::ProbLevel {
            price_cents: c,
            bid_size: if c <= 48 { (49 - c) as f64 * 100.0 } else { 0.0 },
            ask_size: if c >= 52 { (c - 51) as f64 * 110.0 } else { 0.0 },
        })
        .collect();
    let orders = vec![
        vike_cockpit::ProbOrder {
            client_order_id: "o1".into(),
            side: 1,
            price_cents: 45,
            qty: 10.0,
            marker: vike_cockpit::ProbMarker::Resting,
        },
        vike_cockpit::ProbOrder {
            client_order_id: "o2".into(),
            side: -1,
            price_cents: 55,
            qty: 10.0,
            marker: vike_cockpit::ProbMarker::TakeProfit,
        },
    ];
    let mut state = vike_cockpit::ProbLadderState { hover: None, selected };
    let inputs = vike_cockpit::ProbLadderInputs {
        asset: "BTC 5m",
        levels: &levels,
        orders: &orders,
        inside_bid: Some(48),
        inside_ask: Some(52),
        stale,
    };
    let _ = vike_cockpit::draw_ladder(ui, &mut state, &inputs);
}

/// A rung's price text: digits, then the cent sign.
fn is_rung_price(s: &str) -> bool {
    s.strip_suffix('¢').is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
}

#[test]
fn the_ladder_paints_only_tokens_and_no_text_in_the_accent() {
    for a in every_theme_and_market() {
        for (stale, selected) in [(false, Some(45)), (true, None)] {
            let shapes = paint(&ctx_with(&a), |ui| ladder(ui, stale, selected));
            assert_only_tokens(&shapes, &a, "the ladder");
            assert_no_accent_text(&shapes, &a, "the ladder");
        }
    }
}

#[test]
fn the_ladder_paints_role_sizes_only() {
    for text_size in TextSize::ALL {
        let a = Appearance { text_size, ..Appearance::default() };
        let shapes = paint(&ctx_with(&a), |ui| ladder(ui, true, Some(45)));
        assert_role_sizes(&shapes, text_size, "the ladder");
    }
}

/// Spec §3.4 names the ladder row: 16 / 18 / 22 px by density.
#[test]
fn a_rung_is_the_densitys_row_height() {
    for density in Density::ALL {
        let a = Appearance { density, ..Appearance::default() };
        let shapes = paint(&ctx_with(&a), |ui| ladder(ui, false, None));
        let mut ys: Vec<f32> = shapes
            .iter()
            .filter_map(|s| match s {
                Shape::Text(t) if is_rung_price(t.galley.text()) => Some(t.pos.y),
                _ => None,
            })
            .collect();
        ys.sort_by(f32::total_cmp);
        assert!(ys.len() > 10, "{density:?}: {} rungs", ys.len());
        for w in ys.windows(2) {
            assert!((w[1] - w[0] - density.metrics().row_h).abs() < 0.01, "{density:?}: {ys:?}");
        }
    }
}

/// The depth heatmap is the market set's up/down, faded by size; the size labels are its text
/// colours. A bar's alpha is above the heatmap's floor of 30 whenever it has size, so `31..=150`
/// finds the BARS and not the inside-market tint (alpha 30, the same colours): the deepest ask
/// (990) is 150 and the deepest bid (900) about 139.
#[test]
fn depth_bars_and_size_labels_are_the_market_colours() {
    for market in MarketId::ALL {
        let a = Appearance { market, ..Appearance::default() };
        let m = MarketColors::of(market);
        let shapes = paint(&ctx_with(&a), |ui| ladder(ui, false, None));
        let fills: Vec<([u8; 3], u8)> = shapes
            .iter()
            .filter_map(|s| match s {
                Shape::Rect(r) if r.fill.a() < 255 && r.fill.a() > 0 => Some(unfaded(r.fill)),
                _ => None,
            })
            .collect();
        for (want, what) in [(m.up, "up"), (m.down, "down")] {
            assert!(
                fills.iter().any(|(rgb, alpha)| (31..=150).contains(alpha) && near(*rgb, want, 5)),
                "{market:?}: no {what} depth bar in {fills:?}"
            );
        }
        let t = texts(&shapes);
        assert!(t.iter().any(|(s, c, _)| s == "900" && *c == m.up_text), "{market:?}: {t:?}");
        assert!(t.iter().any(|(s, c, _)| s == "990" && *c == m.down_text), "{market:?}: {t:?}");
    }
}

/// The rungs strictly inside the spread (49–51¢) are the neutral analysis line — spec §3.2's token
/// for the boundary between plus and minus — never the accent.
#[test]
fn the_spread_band_is_the_analysis_line() {
    for theme in ThemeId::ALL {
        let a = Appearance { theme, ..Appearance::default() };
        let line = Theme::of(theme).analysis_line;
        let shapes = paint(&ctx_with(&a), |ui| ladder(ui, false, None));
        let band = shapes
            .iter()
            .filter(|s| {
                matches!(s, Shape::Rect(r)
                    if r.fill.a() > 0 && r.fill.a() < 255 && near(unfaded(r.fill).0, line, 3))
            })
            .count();
        assert_eq!(band, 3, "{theme:?}: rungs 49, 50 and 51");
    }
}

/// The picked rung carries the accent's selected-row marker: a 2 px edge at its left.
#[test]
fn the_picked_rung_carries_the_accent_edge_in_each_theme() {
    for theme in ThemeId::ALL {
        let a = Appearance { theme, ..Appearance::default() };
        let accent = Theme::of(theme).accent;
        let shapes = paint(&ctx_with(&a), |ui| ladder(ui, false, Some(45)));
        let edges = shapes
            .iter()
            .filter(|s| {
                matches!(s, Shape::Rect(r)
                    if r.fill == accent && (r.rect.width() - 2.0).abs() < 0.01)
            })
            .count();
        assert_eq!(edges, 1, "{theme:?}");
    }
}

/// A marker is the market colour, solid, with its glyph in the kit's on-fill black.
#[test]
fn markers_are_the_market_colour_with_an_on_fill_glyph() {
    for market in MarketId::ALL {
        let a = Appearance { market, ..Appearance::default() };
        let m = MarketColors::of(market);
        let shapes = paint(&ctx_with(&a), |ui| ladder(ui, false, None));
        let solid: Vec<Color32> = shapes
            .iter()
            .filter_map(|s| match s {
                Shape::Rect(r) if r.fill.a() == 255 => Some(r.fill),
                _ => None,
            })
            .collect();
        assert!(solid.contains(&m.up), "{market:?}: the resting buy and the take-profit are up");
        let cancel = vike_ui_theme::icons::CANCEL.accessible_label("");
        let t = texts(&shapes);
        assert!(t.iter().any(|(s, c, _)| *s == cancel && *c == ON_FILL), "{market:?}: {t:?}");
        assert!(t.iter().any(|(s, c, _)| s == "T" && *c == ON_FILL), "{market:?}: {t:?}");
    }
}

/// A frozen book says so in the WARNING status in every market set — never the market "down" —
/// and the ladder under it is dimmed with the theme's background.
#[test]
fn the_stale_book_shows_a_warning_badge_under_every_market_set() {
    for a in every_theme_and_market() {
        let shapes = paint(&ctx_with(&a), |ui| ladder(ui, true, None));
        let warn = Status::Warning.color();
        let t = texts(&shapes);
        assert!(t.iter().any(|(s, c, _)| s == "STALE" && *c == warn), "{a:?}: {t:?}");
        assert!(colours(&shapes).iter().any(|(c, _)| *c == warn), "{a:?}: the badge's outline");
        let bg = Theme::of(a.theme).bg;
        let scrim = shapes.iter().any(|s| {
            matches!(s, Shape::Rect(r)
                if unfaded(r.fill).1 == 150 && near(unfaded(r.fill).0, bg, 2))
        });
        assert!(scrim, "{a:?}: the stale dim is the background at 150");
    }
}

// ── the one-click ticket ──────────────────────────────────────────────────────────────────────

/// The ticket with a $10 stake, Up at 0.60 and Down at 0.40.
fn ticket(ui: &mut egui::Ui, armed: bool) {
    let mut state = vike_cockpit::TicketState { armed, size: 10.0 };
    let inputs = vike_cockpit::TicketInputs {
        up_price: Some(0.60),
        dn_price: Some(0.40),
        up_win_payout: None,
        dn_win_payout: None,
    };
    let _ = vike_cockpit::draw_ticket(ui, &mut state, &inputs);
}

#[test]
fn the_ticket_paints_only_tokens_and_no_text_in_the_accent() {
    for a in every_theme_and_market() {
        for armed in [false, true] {
            let shapes = paint(&ctx_with(&a), |ui| ticket(ui, armed));
            assert_only_tokens(&shapes, &a, "the ticket");
            assert_no_accent_text(&shapes, &a, "the ticket");
        }
    }
}

#[test]
fn the_ticket_paints_role_sizes_only() {
    for text_size in TextSize::ALL {
        let a = Appearance { text_size, ..Appearance::default() };
        let shapes = paint(&ctx_with(&a), |ui| ticket(ui, true));
        assert_role_sizes(&shapes, text_size, "the ticket");
    }
}

/// Armed, the buy buttons are the kit's buy and sell: the market set's up and down, filled, with
/// the on-fill black label.
#[test]
fn armed_buy_buttons_are_market_fills_with_the_on_fill_label() {
    for market in MarketId::ALL {
        let a = Appearance { market, ..Appearance::default() };
        let m = MarketColors::of(market);
        let shapes = paint(&ctx_with(&a), |ui| ticket(ui, true));
        let fills: Vec<Color32> = shapes
            .iter()
            .filter_map(|s| match s {
                Shape::Rect(r) => Some(r.fill),
                _ => None,
            })
            .collect();
        assert!(fills.contains(&m.up) && fills.contains(&m.down), "{market:?}: {fills:?}");
        let t = texts(&shapes);
        for face in ["BUY UP  0.60", "BUY DOWN  0.40"] {
            assert!(
                t.iter().any(|(s, c, _)| s == face && *c == ON_FILL),
                "{market:?} {face}: {t:?}"
            );
        }
    }
}

/// Disarmed, no button carries a market colour: the mode reads from the buttons' shape (grey
/// outlines) before anything is clicked. A GUARD: today's disarmed look is already grey, and this
/// holds the kit version to it.
#[test]
fn disarmed_buy_buttons_carry_no_market_colour() {
    for market in MarketId::ALL {
        let a = Appearance { market, ..Appearance::default() };
        let m = MarketColors::of(market);
        let shapes = paint(&ctx_with(&a), |ui| ticket(ui, false));
        let marked = shapes.iter().any(|s| match s {
            Shape::Rect(r) => [r.fill, r.stroke.color]
                .into_iter()
                .filter(|c| c.a() > 0)
                .any(|c| near(unfaded(c).0, m.up, 2) || near(unfaded(c).0, m.down, 2)),
            _ => false,
        });
        assert!(!marked, "{market:?}: a disarmed button is painted in a market colour");
    }
}

#[test]
fn the_payout_preview_is_the_market_sets_text_colours() {
    for market in MarketId::ALL {
        let a = Appearance { market, ..Appearance::default() };
        let m = MarketColors::of(market);
        let t = texts(&paint(&ctx_with(&a), |ui| ticket(ui, false)));
        let up = "Up  to win $6.67 · total $16.67";
        let dn = "Dn  to win $15.00 · total $25.00";
        assert!(t.iter().any(|(s, c, _)| s == up && *c == m.up_text), "{market:?}: {t:?}");
        assert!(t.iter().any(|(s, c, _)| s == dn && *c == m.down_text), "{market:?}: {t:?}");
    }
}
