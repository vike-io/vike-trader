//! **The Options window: its two tabs, the Portfolio Greeks status line and the first frame.**
//!
//! The Greeks window became the Options window's Portfolio Greeks tab on 2026-11-17. Its header had
//! run together: the options poll's status sat right-aligned on the heading's row, so a sentence
//! wider than the space left ("…the first chain pass has not landed yet…") was drawn over
//! "Portfolio Greeks · Deribit options", and every new sentence the poll wrote in the seconds after
//! the window opened (none, then waiting for the instrument list, then the datahub's answer) re-laid
//! that row. The status now has a row of its own under the heading, wraps, and takes its line even
//! while empty.
//!
//! Each test draws the REAL body (`options_tool_content`) the way the desktop's tool call site draws
//! it (`tool_window_fit`'s `Typical`: a BTC chain, four option legs and a perpetual hedge) and reads
//! what was painted: each text's string, its layout rect and the clip it was painted under. Frames
//! run through `Context::run_pass`, which re-runs a pass egui asked to discard, as the app's
//! integration does.

use egui::epaint::ClippedShape;
use egui::{Rect, Shape, Vec2, vec2};
use vike_app_core::ui::window::spawn::tool_window_size;
use vike_app_core::ui::window::{
    TOOL_BODY_MARGIN, WindowKind, WindowState, show_window, tool_title_bar,
};
use vike_app_core::ui::windows::OptionsTab;
use vike_ui_theme::appearance::{self, ThemeAppearance};
use vike_ui_theme::value::workspace;

use crate::tool_window_fit::{LIVE_STATUS, Typical, arena, screen};

/// The datahub's answer before its first chain pass, as the owner's window showed it on 2026-11-17.
const NOT_LANDED: &str = "the datahub at 127.0.0.1:9 could not serve the BTC option chain: the first \
                          chain pass has not landed yet. Ask again; this clears by itself once a \
                          venue pass lands.";

/// Frames drawn with no window first, so the bundled fonts have landed before the window opens —
/// as the app's have long before an operator opens a window.
const WARM: usize = 3;

/// One painted text: its string, its layout rect and the clip it was painted under.
#[derive(Debug, Clone, PartialEq)]
struct Text {
    text: String,
    rect: Rect,
    clip: Rect,
}

/// Every text `shapes` paints, in paint order.
fn texts(shapes: Vec<ClippedShape>) -> Vec<Text> {
    fn walk(clip: Rect, shape: Shape, out: &mut Vec<Text>) {
        match shape {
            Shape::Vec(v) => v.into_iter().for_each(|s| walk(clip, s, out)),
            Shape::Text(t) => out.push(Text {
                text: t.galley.text().to_string(),
                rect: t.galley.rect.translate(t.pos.to_vec2()),
                clip,
            }),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for c in shapes {
        walk(c.clip_rect, c.shape, &mut out);
    }
    out
}

/// Open an Options window of `size` over `typical` and return the texts each of its first `frames`
/// frames painted.
fn open(mut typical: Typical, size: Vec2, frames: usize) -> Vec<Vec<Text>> {
    let context = egui::Context::default();
    appearance::install(&context, &ThemeAppearance::default());
    let raw = |frame: usize| egui::RawInput {
        screen_rect: Some(screen()),
        time: Some(frame as f64 / 60.0),
        ..Default::default()
    };
    for frame in 0..WARM {
        context.run_pass(raw(frame), |_| {}).drop_without_applying_deltas();
    }
    let at = Rect::from_min_size(arena().min + workspace::CASCADE_ORIGIN, size);
    let mut w = WindowState::tool("options", WindowKind::Options, at);
    (0..frames)
        .map(|frame| {
            let mut out = context.run_pass(raw(WARM + frame), |context| {
                show_window(context, &mut w, arena(), |ui, bounds| {
                    let (_, drag, _) = tool_title_bar(ui, WindowKind::Options, false);
                    egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, |ui| {
                        bounds.show(ui, |ui| typical.draw(WindowKind::Options, ui));
                    });
                    drag
                });
            });
            let shapes = std::mem::take(&mut out.shapes);
            out.drop_without_applying_deltas();
            texts(shapes)
        })
        .collect()
}

/// The typical Options window on `tab`, its status line `status`.
fn options_on(tab: OptionsTab, status: &str) -> Typical {
    let mut typical = Typical::of(WindowKind::Options);
    typical.tv.options_tab = tab;
    typical.owned.tool_feeds.options_status = status.to_string();
    typical
}

/// What the window painted after it settled.
fn settled(typical: Typical, size: Vec2) -> Vec<Text> {
    open(typical, size, 10).pop().expect("frames were drawn")
}

/// The one painted text that is exactly `s`; the test fails if there is none or more than one.
fn the<'a>(texts: &'a [Text], s: &str) -> &'a Text {
    let found: Vec<&Text> = texts.iter().filter(|t| t.text == s).collect();
    assert_eq!(found.len(), 1, "{s:?} painted {} times", found.len());
    found[0]
}

fn painted(texts: &[Text], s: &str) -> bool {
    texts.iter().any(|t| t.text == s)
}

/// Do two rects share any area? (`Rect::intersects` also counts two rects that only touch.)
fn overlap(a: Rect, b: Rect) -> bool {
    let i = a.intersect(b);
    i.width() > 0.0 && i.height() > 0.0
}

/// **Both tabs render in the Options window**: the tab row names both, the Chain draws the chain's
/// grid and none of the Greeks table, and Portfolio Greeks draws the legs' table, its NET row and the
/// poll's status line, and none of the chain.
#[test]
fn both_tabs_render_in_the_options_window() {
    let size = tool_window_size(WindowKind::Options);
    let chain = settled(options_on(OptionsTab::Chain, LIVE_STATUS), size);
    for s in ["Chain", "Portfolio Greeks", "Strike", "IV"] {
        assert!(painted(&chain, s), "the Chain tab paints {s:?}");
    }
    assert!(!painted(&chain, "NET"), "the Chain tab draws no Greeks table");

    let greeks = settled(options_on(OptionsTab::PortfolioGreeks, LIVE_STATUS), size);
    for s in ["Chain", "Instrument", "NET", "BTC-26MAR27-96000-C", "BTC-PERPETUAL", LIVE_STATUS] {
        assert!(painted(&greeks, s), "the Portfolio Greeks tab paints {s:?}");
    }
    assert!(!painted(&greeks, "Strike"), "the Portfolio Greeks tab draws no chain");
}

/// **The Options window opens large enough for both tabs**: at its `tool_window_size`, all 24 strikes
/// of the chain's strike window and every Greeks leg are painted inside the clip they are drawn under,
/// so neither tab needs a scroll bar. (`tool_window_fit` holds the body itself inside the window.)
#[test]
fn the_options_window_shows_every_strike_and_every_leg_with_no_scrollbar() {
    let size = tool_window_size(WindowKind::Options);
    let inside =
        |t: &Text| t.rect.top() >= t.clip.top() - 0.5 && t.rect.bottom() <= t.clip.bottom() + 0.5;

    let chain = settled(options_on(OptionsTab::Chain, LIVE_STATUS), size);
    // The fixture's strikes run 80,000..=112,000 around a 96,412 spot: `vike_options::atm_strike_window`'s
    // 12 below the first strike at or above spot (97,000) and 12 from it are 85,000..=108,000.
    for k in 85..=108 {
        let label = vike_options::chain::columns::fmt_strike(f64::from(k) * 1_000.0);
        let t = the(&chain, &label);
        assert!(inside(t), "strike {label} is clipped at {:?} by {:?}", t.rect, t.clip);
    }
    let unstreamed = vike_options::chain::columns::fmt_strike(109_000.0);
    assert!(!painted(&chain, &unstreamed), "the grid shows no strike the ticker does not stream");

    let greeks = settled(options_on(OptionsTab::PortfolioGreeks, LIVE_STATUS), size);
    for leg in [
        "BTC-26MAR27-96000-C",
        "BTC-26MAR27-90000-P",
        "BTC-26MAR27-104000-C",
        "BTC-26MAR27-100000-P",
        "BTC-PERPETUAL",
        "NET",
        "NET Δ · USD",
    ] {
        let t = the(&greeks, leg);
        assert!(inside(t), "{leg} is clipped at {:?} by {:?}", t.rect, t.clip);
    }
}

/// **The heading and the status line never intersect**, on a narrow window (where the status wraps
/// onto several lines) and on the window's own width: the status is on a row of its own under the
/// heading, inside the window, and it shares its area with no other text.
#[test]
fn the_heading_and_the_status_line_never_intersect() {
    let height = tool_window_size(WindowKind::Options).y;
    for width in [420.0, tool_window_size(WindowKind::Options).x] {
        for legs in [true, false] {
            let mut typical = options_on(OptionsTab::PortfolioGreeks, NOT_LANDED);
            if !legs {
                typical.owned.snapshot.positions.clear();
            }
            let texts = settled(typical, vec2(width, height));
            let status = the(&texts, NOT_LANDED);
            let subtitle = the(&texts, "· Deribit options");
            assert!(
                status.rect.top() >= subtitle.rect.bottom(),
                "{width}: the status sits under the heading"
            );
            assert!(status.rect.width() < width, "{width}: the status wraps inside the window");
            for other in texts.iter().filter(|t| t.text != NOT_LANDED) {
                assert!(
                    !overlap(status.rect, other.rect),
                    "{width} (legs {legs}): the status {:?} runs over {:?} at {:?}",
                    status.rect,
                    other.text,
                    other.rect
                );
            }
            if width < 500.0 {
                assert!(
                    status.rect.height() > 1.5 * subtitle.rect.height(),
                    "{width}: a narrow window wraps the status onto more lines"
                );
            }
        }
    }
}

/// **The status line's arrival moves nothing**: the poll writes its first sentence a moment after the
/// window opens, and an empty status already takes the line one sentence fills, so every other text
/// keeps its rows. (Rows, not digits: a greek is priced off the wall clock, so two draws may print
/// a different last digit.)
#[test]
fn an_empty_status_line_holds_the_line_its_first_sentence_fills() {
    let size = tool_window_size(WindowKind::Options);
    for legs in [true, false] {
        let rows = |status: &str| {
            let mut typical = options_on(OptionsTab::PortfolioGreeks, status);
            if !legs {
                typical.owned.snapshot.positions.clear();
            }
            // The NET row's two empty cells paint empty texts too: only the words are compared.
            settled(typical, size)
                .into_iter()
                .filter(|t| !t.text.is_empty() && t.text != status)
                .map(|t| t.rect.y_range())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            rows(""),
            rows(LIVE_STATUS),
            "legs {legs}: the status line's arrival moved the window"
        );
    }
}

/// **The first frame after open is already final**: with the data unchanged, every text the window
/// paints on the first frame it is seen is where it is thirty frames later, on both tabs (the
/// Portfolio Greeks tab with legs, and with none, as the owner's window was). A text keeps its rows,
/// and one whose words did not change keeps its place too.
#[test]
fn nothing_moves_between_the_first_and_the_thirtieth_frame_after_open() {
    let size = tool_window_size(WindowKind::Options);
    let mut empty = options_on(OptionsTab::PortfolioGreeks, NOT_LANDED);
    empty.owned.snapshot.positions.clear();
    let cases = [
        ("Chain", options_on(OptionsTab::Chain, NOT_LANDED)),
        ("Portfolio Greeks", options_on(OptionsTab::PortfolioGreeks, NOT_LANDED)),
        ("Portfolio Greeks, no legs", empty),
    ];
    for (name, typical) in cases {
        let frames = open(typical, size, 31);
        // egui lays a NEW window out once unseen (its `Area`'s sizing pass, nothing painted) and
        // paints it from the next frame on: that frame is the first the operator sees.
        let shown = frames.iter().position(|f| !f.is_empty());
        assert_eq!(shown, Some(1), "{name}: the window is painted from the frame after it opens");
        let (first, last) = (&frames[1], &frames[30]);
        assert_eq!(first.len(), last.len(), "{name}: texts on the first frame seen, then later");
        let moved: Vec<_> = first
            .iter()
            .zip(last)
            .filter(|(a, b)| {
                a.rect.y_range() != b.rect.y_range() || (a.text == b.text && a.rect != b.rect)
            })
            .take(5)
            .map(|(a, b)| format!("{:?} {:?} -> {:?} {:?}", a.text, a.rect, b.text, b.rect))
            .collect();
        assert!(moved.is_empty(), "{name}: the layout changed after open: {moved:#?}");
    }
}
