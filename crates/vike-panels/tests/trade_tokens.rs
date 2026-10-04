//! The Trade window under the design system, read off what a REAL `trade::draw` paints
//! (`Harness::output`): the installed theme, market colours, text size and density reach every row,
//! bar, text and marker. No GPU — the shapes are egui's own CPU output, so this runs on the
//! GPU-less CI runners. Carried over from the DOM's own token gate, test for test, less the
//! heatmap's (the heatmap returns with the tick chart) and the control strips' (`trade_fit.rs`
//! measures every region of this window instead).
//!
//! The shape readers below are this file's own: the kit's twins (`vike_ui_theme`'s
//! `components::testing`) are private to that crate.

use egui::{Color32, Rect, Shape, Stroke};
use egui_kittest::Harness;
use vike_model::{BookLevel, L2Book, VenueCaps};
use vike_panels::trade::{
    self, AccountMode, Exits, Grid, LadderOrder, OrderType, Origin, Position, Tradable,
    TradeAction, TradeInputs, TradeState,
};
use vike_ui_theme::appearance::Appearance;
use vike_ui_theme::components::{Status, Tokens, chip};
use vike_ui_theme::icons;
use vike_ui_theme::market::{MarketColors, MarketId};
use vike_ui_theme::metrics::Density;
use vike_ui_theme::theme::{Theme, ThemeId};
use vike_ui_theme::type_scale::{TextRole, TextSize};

/// A 1.0 tick and a 0.001 lot.
const GRID: Grid = Grid { tick: 1.0, lot: 0.001, min_qty: 0.001 };

/// Everything one frame of the window is handed, owned.
struct Scene {
    book: L2Book,
    last: Option<f64>,
    orders: Vec<LadderOrder>,
    position: Option<Position>,
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
fn orders() -> Vec<LadderOrder> {
    let order = |id: &str, side, price, is_stop| LadderOrder {
        client_order_id: id.to_string(),
        side,
        price,
        qty: 0.01,
        is_stop,
    };
    vec![order("limit-buy", 1, 98.0, false), order("stop-sell", -1, 96.0, true)]
}

/// A change to the ticket's state, made the way a trader would after the window opened.
type Tweak = fn(&mut TradeState);

/// No change.
fn as_opened(_: &mut TradeState) {}

/// The whole window under appearance `a`, at 900 × 700 — wide and tall enough that the ladder
/// stands beside the full ticket and nothing is clipped — synced, its ticket changed by `tweak`,
/// and run until it settles. LIVE, so a held order stays held.
fn window(a: Appearance, s: Scene, tweak: Tweak) -> Harness<'static, TradeState> {
    let mut h = Harness::builder().with_size(egui::vec2(900.0, 700.0)).build_ui_state(
        move |ui, st: &mut TradeState| {
            if !vike_ui_theme::harness::appearance_ready(ui.ctx(), &a) {
                return;
            }
            let inputs = TradeInputs {
                venue: "binance",
                venue_label: "Binance",
                account: None,
                symbol: "BTCUSDT",
                base: "BTC",
                quote: "USDT",
                mode: AccountMode::Live,
                tradable: Tradable::Yes,
                grid: GRID,
                book: &s.book,
                last: s.last,
                stale: s.stale,
                source: s.source,
                absence: None,
                orders: &s.orders,
                orders_why: None,
                position: s.position,
                buying_power: Some(10_000.0),
                caps: VenueCaps::UNSUPPORTED,
                bracket_why: Some(trade::ticket::TPSL_ACCOUNT_WHY),
                bracket_wire: false,
                matches: &[],
                recent: &[],
                accounts: &[],
                accounts_why: None,
                unconnected: &[],
                status: None,
            };
            let _ = trade::draw(ui, st, &inputs);
        },
        TradeState::default(),
    );
    // The first frames sync the state to the window's address, which clears the ticket: the
    // ticket's own state goes in after them.
    h.run();
    tweak(h.state_mut());
    h.run();
    h
}

/// Every shape the last frame painted, `Shape::Vec`s flattened.
fn shapes(h: &Harness<'static, TradeState>) -> Vec<Shape> {
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

/// Where the ladder painted the price `want`: a text reading exactly that, in the Body role the
/// rows are drawn in (the bar's last price reads the same digits in the Title role).
fn price_pos(s: &[Shape], want: &str, body: f32) -> Option<egui::Pos2> {
    s.iter().find_map(|s| match s {
        Shape::Text(t)
            if t.galley.text() == want
                && t.galley.job.sections.first().is_some_and(|x| x.format.font_id.size == body) =>
        {
            Some(t.pos)
        }
        _ => None,
    })
}

/// The ladder's row pitch is the density's row height (spec §3.4: "Table / ladder row", 16/18/22),
/// read off the price column: on a 1.0 tick at grouping 1, prices 100 and 99 are adjacent rows.
#[test]
fn the_ladder_rows_follow_the_density() {
    for d in Density::ALL {
        let a = Appearance { density: d, ..Appearance::default() };
        let body = a.text_size.px(TextRole::Body);
        let s = shapes(&window(a, scene(), as_opened));
        let y = |p: &str| {
            price_pos(&s, p, body).unwrap_or_else(|| panic!("{d:?}: price {p} was not painted")).y
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
        let s = shapes(&window(a, scene(), as_opened));
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
        let stale = shapes(&window(a, Scene { stale: true, ..scene() }, as_opened));
        assert!(fills(&stale).contains(&th.scrim()), "{id:?}: a stale ladder is not dimmed");
    }
}

/// Depth bars are the market set's depth fills, and the sizes its TEXT colours (spec §3.2): bids
/// 99@1 and asks 100@1 each paint "1.00", one in each side's colour.
#[test]
fn depth_bars_and_sizes_follow_each_market_set() {
    for m in MarketId::ALL {
        let c = MarketColors::of(m);
        let a = Appearance { market: m, ..Appearance::default() };
        let s = shapes(&window(a, scene(), as_opened));
        let f = fills(&s);
        assert!(f.contains(&c.up_depth) && f.contains(&c.down_depth), "{m:?}: depth bars");
        let tx = texts(&s);
        assert!(tx.iter().any(|(t, col, _)| t == "1.00" && *col == c.up_text), "{m:?}: bid size");
        assert!(tx.iter().any(|(t, col, _)| t == "1.00" && *col == c.down_text), "{m:?}: ask size");
    }
}

/// The FEED word is an outlined status badge in the FIXED status colours (spec §3.2), whatever the
/// theme: the kit's badge ink for the state the window's link is in. An empty source is a link
/// nobody reported on, `FEED ?`, never `FEED DOWN` (pre-flight I11(a)).
#[test]
fn the_feed_word_is_its_status_colour() {
    let t = Tokens::from_appearance(&Appearance::default());
    for (source, word, status) in [
        ("datahub 127.0.0.1:7878 — 1/1 stream(s) live", "FEED UP", Status::Ok),
        ("datahub 127.0.0.1:7878 — connecting, 0/1 stream(s)", "FEED DIALLING", Status::Warning),
        ("idle", "FEED DOWN", Status::Error),
        ("datahub error: connection refused", "FEED FAULT", Status::Error),
        ("datahub 127.0.0.1:7878 — no streams wanted on this venue", "FEED ?", Status::Muted),
        ("", "FEED ?", Status::Muted),
    ] {
        let s = shapes(&window(Appearance::default(), Scene { source, ..scene() }, as_opened));
        let ink = chip::badge_ink(&t, status);
        assert!(
            texts(&s).iter().any(|(tx, c, _)| tx == word && *c == ink),
            "{source:?}: {word} is not painted in {ink:?}"
        );
    }
}

/// A busy window — stale, a position, both kinds of order, Reduce only and a refused TP/SL ticked
/// (its refusal written in the warning colour), an order held for a confirm with both exits — and
/// an empty one over a link that is down: between them, every string, badge, marker and control
/// the window draws.
fn both_states() -> [(&'static str, Scene, Tweak); 2] {
    let busy = Scene {
        stale: true,
        orders: orders(),
        position: Some(Position { size: 0.5, avg_px: 99.0, upnl: 1.25 }),
        ..scene()
    };
    let armed: Tweak = |s| {
        s.reduce_only = true;
        s.tpsl = true;
        s.held = Some(TradeAction::Place {
            side: 1,
            order_type: OrderType::Limit,
            price: Some(98.0),
            qty: 0.01,
            reduce_only: false,
            exits: Some(Exits { take_profit: 99.0, stop_loss: 97.0 }),
            origin: Origin::Ladder,
        });
    };
    [("a busy window", busy, armed), ("no book, link down", bookless("idle"), as_opened)]
}

/// Every text the window draws is a ROLE size (spec §3.3: "Code names a role and never a pixel
/// size"), at both text sizes. The ratchet catches a literal; this catches a COMPUTED size no role
/// has. Icons are sized by the kit and are not asked (`texts` leaves the icon family out).
#[test]
fn every_text_the_window_draws_is_a_role_size() {
    for size in TextSize::ALL {
        let roles: Vec<f32> = TextRole::ALL.iter().map(|r| size.px(*r)).collect();
        for (what, sc, st) in both_states() {
            let a = Appearance { text_size: size, ..Appearance::default() };
            let s = shapes(&window(a, sc, st));
            let tx = texts(&s);
            assert!(!tx.is_empty(), "{size:?}, {what}: nothing was painted, so nothing was asked");
            for (text, _, sizes) in tx {
                for px in sizes {
                    assert!(roles.contains(&px), "{size:?}, {what}: {text:?} is drawn at {px} pt");
                }
            }
        }
    }
}

/// The trading palette and the DOM's own private colours, as VALUES: none may be painted by the
/// window that replaced it, which reads the theme (spec §9 step 7 of the design system). They are
/// spelled as literals because the palette they came from is deleted. (A function, not a `const`:
/// `from_rgba_unmultiplied` is not a `const fn`.) The market set is the default, Classic —
/// Exchange's pair IS the old trading green and red, and legitimately so.
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
            let s = shapes(&window(Appearance { theme: id, ..Appearance::default() }, sc, st));
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
        |source| texts(&shapes(&window(Appearance::default(), bookless(source), as_opened)));
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
