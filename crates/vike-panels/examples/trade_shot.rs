//! The Trade window's body → PNG, off-screen: the REAL `trade::draw` on the app's own renderer, posed
//! with the numbers of the owner's v3 design (`BTCUSDT`, 65,432.5, a long position of 0.050 at
//! 65,410.2, three working orders) so a capture can be laid beside a capture of that design and
//! looked at together ("exactly the same", owner, 2026-10-04).
//!
//! It draws the window's BODY (the instrument bar, the chart, the ladder, the ticket, the strip): the
//! title bar belongs to the shell above this crate, so its four toggles are looked at in the real app.
//! No backend and no catalog: what it answers is "does the layout draw right on a real rasterizer",
//! never "does it look right connected".
//!
//! A pose takes ticket states after a `+`, each worth looking at: `market`, `stop`, `tpsl` (TP/SL on),
//! `reduce`, `quote` (the size in USDT), `flat` (no position), `dead` (an account that does not trade
//! the symbol), `nobp` (no buying power), `big` (a size whose Buy and Sell labels do not fit side by
//! side), and the look: `small` and `large` (the other two text sizes; no word is Standard),
//! `comfortable` and `compact` (the densities):
//! `trade_shot out ticket+stop+tpsl under+flat+large`. On a chart pose, `heat`, `lines` and `bubbles`
//! show that one layer alone (`chart+heat`). `dump` prints every control's name and rect (points, from
//! the body's top-left) beside the capture, as `<png>.rects`, to measure against the design.
//!
//! A pose is a view (`beside`, `beside-vol` — the same with the ladder's Vol column on, over the
//! design's own volumes —, `under`, `ticket`, `chart`, `chart-under`, `chart-ticket`,
//! `under-ticket`) or a menu opened by a click on the button the accessibility tree names
//! (`menu-account`, `menu-symbol`).
//!
//! ```sh
//! cargo build -p vike-panels --features png-export --example trade_shot
//! target/debug/examples/trade_shot.exe <out dir> [pose]...
//! ```

use vike_model::{BookLevel, L2Book};
use vike_panels::trade::chart::Print;
use vike_panels::trade::{
    self, AccountMode, AccountRow, Grid, LadderOrder, OrderType, Panel, PickRow, Position,
    StatusKind, StatusLine, Tradable, TradeInputs, TradeState, Unconnected, View, layout,
};
use vike_ui_theme::appearance::{self, Appearance};
use vike_ui_theme::components::Tokens;
use vike_ui_theme::metrics::Density;
use vike_ui_theme::offscreen::{TexDelta, rasterize};
use vike_ui_theme::type_scale::TextSize;

const PPP: f32 = 2.0;

/// Liveness floors for a pose (`vike_ui_theme::pixel_liveness`: non-golden, argued per harness): the
/// window's body carries two panes of text and a dozen rows of coloured depth bars, so a healthy
/// frame measures distinct colours in the thousands where a dead one measures 1; no single colour
/// (the theme's background) may eat the frame; and no quadrant floor, because which pane a view
/// puts where is a layout fact this gate does not own.
const LIVENESS: vike_ui_theme::pixel_liveness::LivenessSpec =
    vike_ui_theme::pixel_liveness::LivenessSpec {
        min_distinct_colors: 64,
        max_dominant_share: 0.97,
        min_quadrant_distinct: None,
    };
const GRID: Grid = Grid { tick: 0.1, lot: 0.001, min_qty: 0.001 };

/// What a pose draws: a view, and the button (by its accessible name) to click before the capture.
struct Pose {
    view: View,
    click: Option<&'static str>,
    /// Whether the ladder's Vol column is on.
    vol: bool,
    /// The ticket states after a `+`.
    mods: Vec<String>,
}

impl Pose {
    fn has(&self, word: &str) -> bool {
        self.mods.iter().any(|m| m == word)
    }
}

fn pose(full: &str) -> Pose {
    let mut words = full.split('+');
    let name = words.next().unwrap_or_default();
    let mods: Vec<String> = words.map(String::from).collect();
    let view = |chart, ladder, panel| View { chart, ladder, panel };
    let at = |v| Pose { view: v, click: None, vol: false, mods: mods.clone() };
    let beside = view(false, true, Panel::Beside);
    match name {
        "beside" => at(beside),
        "beside-vol" => Pose { vol: true, ..at(beside) },
        "under" => at(view(false, true, Panel::Under)),
        "ticket" => at(view(false, false, Panel::Beside)),
        "chart" => at(view(true, true, Panel::Beside)),
        "chart-ticket" => at(view(true, false, Panel::Beside)),
        "chart-under" => at(view(true, true, Panel::Under)),
        "under-ticket" => at(view(false, false, Panel::Under)),
        "menu-account" => Pose { click: Some("Binance Perp · main"), ..at(beside) },
        "menu-symbol" => Pose { click: Some("BTCUSDT"), ..at(beside) },
        other => panic!("unknown pose {other}"),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: trade_shot <out dir> [pose]..."));
    let mut poses: Vec<String> = args.collect();
    if poses.is_empty() {
        poses = ["beside", "under", "ticket"].map(String::from).to_vec();
    }
    std::fs::create_dir_all(&dir).expect("out dir");
    for name in poses {
        shot(&pose(&name), &dir.join(format!("trade-{name}.png")));
    }
}

/// The v3 design's book: eleven asks from the 65,432.5 best ask up, eleven bids from 65,432.4 down.
fn book() -> L2Book {
    let asks = [
        (65_432.5, 1.667),
        (65_432.6, 1.506),
        (65_432.7, 1.967),
        (65_432.8, 2.752),
        (65_432.9, 0.273),
        (65_433.0, 2.988),
        (65_433.1, 3.912),
        (65_433.2, 1.262),
        (65_433.3, 0.445),
        (65_433.4, 3.660),
        (65_433.5, 2.527),
    ];
    let bids = [
        (65_432.4, 1.365),
        (65_432.3, 3.151),
        (65_432.2, 2.266),
        (65_432.1, 0.705),
        (65_432.0, 3.616),
        (65_431.9, 2.842),
        (65_431.8, 3.063),
        (65_431.7, 3.355),
        (65_431.6, 0.638),
        (65_431.5, 1.362),
        (65_431.4, 0.564),
    ];
    let level = |&(p, q): &(f64, f64)| BookLevel::new(p, q);
    let mut b = L2Book::new(GRID.tick);
    b.apply_snapshot(1, &bids.map(|l| level(&l)), &asks.map(|l| level(&l)));
    b
}

/// The three working orders of the design: a sell limit at 65,433.2, a buy STOP at 65,433.4 and a
/// buy limit at 65,431.8.
fn orders() -> Vec<LadderOrder> {
    let one = |id: &str, side, price, is_stop| LadderOrder {
        client_order_id: id.to_string(),
        side,
        price,
        qty: 0.010,
        is_stop,
    };
    vec![
        one("o1", -1, 65_433.2, false),
        one("o2", 1, 65_433.4, true),
        one("o3", 1, 65_431.8, false),
    ]
}

/// What the design's Vol column prints, row by row from 65,433.5 down to 65,431.4, as one print per
/// row: the capture's stand-in for a real tape, where the chart's walk would give every row its
/// own random total.
fn design_tape(now_ms: i64) -> Vec<Print> {
    let volumes = [
        4.125, 1.660, 1.475, 6.988, 4.021, 4.738, 1.880, 6.180, 6.812, 0.553, 4.486, 1.441, 3.100,
        2.598, 5.895, 4.403, 2.601, 5.578, 5.263, 4.197, 2.218, 6.226,
    ];
    volumes
        .iter()
        .enumerate()
        .map(|(i, v)| Print {
            ms: now_ms - 1_000,
            price: 65_433.5 - 0.1 * i as f64,
            size: *v,
            buy: true,
        })
        .collect()
}

/// The design's account list: Binance Perp twice, Bybit, OKX, Hyperliquid (Deribit has no account).
fn accounts() -> Vec<AccountRow<'static>> {
    let row = |venue, venue_label, account: Option<&'static str>, symbol, mode| AccountRow {
        venue,
        venue_label,
        product: if venue == "hyperliquid" { "" } else { "Perp" },
        account,
        symbol,
        name: account.unwrap_or("main"),
        mode,
        why_not: None,
    };
    vec![
        row("binance", "Binance", None, "BTCUSDT", AccountMode::Demo),
        row("binance", "Binance", Some("sub-2"), "BTCUSDT", AccountMode::Demo),
        row("bybit", "Bybit", None, "BTCUSDT", AccountMode::Live),
        row("okx", "OKX", None, "BTC-USDT-SWAP", AccountMode::Paper),
        row("hyperliquid", "Hyperliquid", None, "BTC", AccountMode::Live),
    ]
}

/// The design's symbol list, one row per instrument and venue.
fn matches() -> Vec<PickRow<'static>> {
    let row = |venue, venue_label, symbol, pair: &str, current| PickRow {
        venue,
        venue_label,
        symbol,
        pair: pair.to_string(),
        current,
    };
    vec![
        row("binance", "Binance", "BTCUSDT", "BTC/USDT perpetual", true),
        row("bybit", "Bybit", "BTCUSDT", "BTC/USDT perpetual", false),
        row("okx", "OKX", "BTC-USDT-SWAP", "BTC/USDT perpetual", false),
        row("hyperliquid", "Hyperliquid", "BTC", "BTC/USDC perpetual", false),
        row("binance", "Binance", "ETHUSDT", "ETH/USDT perpetual", false),
        row("bybit", "Bybit", "ETHUSDT", "ETH/USDT perpetual", false),
        row("okx", "OKX", "ETH-USDT-SWAP", "ETH/USDT perpetual", false),
        row("hyperliquid", "Hyperliquid", "ETH", "ETH/USDC perpetual", false),
    ]
}

/// A wandering top of book over the last two minutes, and the prints on it, for the chart: a
/// deterministic walk (no clock but the one given), so a capture is the same every time.
fn walk(now_ms: i64) -> (Vec<L2Book>, Vec<Print>) {
    let mut seed = 0x2545_F491_4F6C_DD1D_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1_u64 << 53) as f64
    };
    let mut mid = 65_432.45_f64;
    let mut books = Vec::new();
    let mut prints = Vec::new();
    for k in 0..480_i64 {
        mid = (mid + (next() - 0.5) * 0.8).clamp(65_431.0, 65_433.6);
        let bid = (mid * 10.0).floor() / 10.0;
        let mut b = L2Book::new(GRID.tick);
        let mut levels = |side: f64| -> Vec<BookLevel> {
            (0..12)
                .map(|i| {
                    BookLevel::new(
                        bid + side * (f64::from(i) * 0.1 + 0.1 * (side + 1.0) / 2.0),
                        0.3 + next() * 3.8,
                    )
                })
                .collect()
        };
        b.apply_snapshot(1, &levels(-1.0), &levels(1.0));
        books.push(b);
        if next() < 0.17 {
            prints.push(Print {
                ms: now_ms - 120_000 + k * 250,
                price: bid + if next() < 0.5 { 0.0 } else { 0.1 },
                size: 0.01 + next().powi(3) * 1.5,
                buy: next() < 0.5,
            });
        }
    }
    (books, prints)
}

/// Add one frame's texture uploads to the pile every frame's must reach the renderer in.
fn keep(tex: &mut Vec<TexDelta>, out: &mut egui::FullOutput) {
    tex.extend(
        out.textures_delta
            .set
            .iter()
            .flat_map(|(id, deltas)| deltas.iter().map(move |d| (*id, d.clone()))),
    );
    out.textures_delta.clear();
}

/// The centre of the first accessibility node whose label starts with `label` (a button names its icon after its words), if `out` names one.
fn find_button(out: &egui::FullOutput, label: &str) -> Option<egui::Pos2> {
    let update = out.platform_output.accesskit_update.as_ref()?;
    update.nodes.iter().find_map(|(_, node)| {
        (node.label().is_some_and(|l| l.starts_with(label)))
            .then(|| node.bounds())
            .flatten()
            .map(|b| egui::pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32))
    })
}

/// Draw one pose and save it (the image type is the renderer's, so it is saved here, never named).
fn shot(pose: &Pose, path: &std::path::Path) {
    let mut a = Appearance::default();
    if pose.has("small") {
        a.text_size = TextSize::Small;
    }
    if pose.has("large") {
        a.text_size = TextSize::Large;
    }
    if pose.has("comfortable") {
        a.density = Density::Comfortable;
    }
    if pose.has("compact") {
        a.density = Density::Compact;
    }
    let ctx = egui::Context::default();
    appearance::install(&ctx, &a);
    ctx.set_pixels_per_point(PPP);
    ctx.enable_accesskit();
    // Priming frame: fonts bind at the NEXT pass. Its texture deltas are kept like every frame's.
    let mut tex: Vec<TexDelta> = Vec::new();
    let mut prime = ctx.run_ui(egui::RawInput::default(), |_| {});
    keep(&mut tex, &mut prime);
    let now_ms = vike_model::now_ms();
    let (books, mut prints) = walk(now_ms);
    if pose.vol {
        prints = design_tape(now_ms);
    }
    let book = book();
    let orders = orders();
    let accounts = accounts();
    let matches = matches();
    let unconnected =
        [Unconnected { venue_label: "Deribit", product: "Perp", symbol: "BTC-PERPETUAL" }];
    let recent = [("binance", "BTCUSDT"), ("binance", "ETHUSDT"), ("bybit", "SOLUSDT")];
    let inputs = TradeInputs {
        venue: "binance",
        venue_label: "Binance",
        product: "Perp",
        account: None,
        symbol: "BTCUSDT",
        base: "BTC",
        quote: "USDT",
        mode: AccountMode::Demo,
        tradable: if pose.has("dead") {
            Tradable::No { trades: &[], why: Some("The server has stopped trading.") }
        } else {
            Tradable::Yes
        },
        grid: GRID,
        book: &book,
        last: Some(65_432.5),
        stale: false,
        source: "datahub 127.0.0.1:7878 — 1/1 stream(s) live",
        absence: None,
        orders: &orders,
        orders_why: None,
        position: (!pose.has("flat")).then_some(Position {
            size: 0.050,
            avg_px: 65_410.2,
            upnl: 1.12,
        }),
        buying_power: (!pose.has("nobp")).then_some(65_400.0),
        caps: vike_model::venues::venue_caps::BINANCE,
        bracket_why: None,
        bracket_wire: true,
        matches: &matches,
        recent: &recent,
        accounts: &accounts,
        accounts_why: None,
        unconnected: &unconnected,
        tape: &prints,
        status: Some(StatusLine {
            kind: StatusKind::Info,
            text: "Ready · one-click trading is on",
        }),
    };
    let mut state = TradeState::default();
    state.view = pose.view;
    state.vol = pose.vol;
    let size = layout::spec_size(pose.view);
    let t = Tokens::of(&ctx);
    let chrome = layout::chrome();
    let body = size - chrome;
    // A menu hangs below its button and can reach past the window: give the capture the room.
    let canvas = if pose.click.is_some() { egui::vec2(size.x.max(480.0), size.y) } else { size };
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, canvas);
    let mut last = None;
    let mut button = None;
    let body_origin = egui::pos2((size.x - body.x) / 2.0, 0.0);
    let mut nodes: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let frames = if pose.click.is_some() { 24 } else { 4 };
    for f in 0..frames {
        let mut events = Vec::new();
        if let (Some(at), true) = (button, f >= 6) {
            let press = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            match f {
                6 => events.push(egui::Event::PointerMoved(at)),
                7 => events.push(press(true)),
                8 => events.push(press(false)),
                _ => {}
            }
        }
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(1.0 + f64::from(f) / 30.0),
                events,
                ..Default::default()
            },
            |ui| {
                let rect = egui::Rect::from_min_size(
                    egui::pos2((size.x - body.x) / 2.0, 0.0),
                    egui::vec2(body.x, body.y),
                );
                ui.painter().rect_filled(screen, 0.0, t.theme.bg);
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                let _ = trade::draw(&mut child, &mut state, &inputs);
            },
        );
        if f == 0 {
            // The first frame syncs the state to the window's address; what a trader sets comes after.
            state.order_type = if pose.has("market") {
                OrderType::Market
            } else if pose.has("stop") {
                OrderType::Stop
            } else {
                OrderType::Limit
            };
            state.price = "65432.4".to_string();
            if pose.has("heat") || pose.has("lines") || pose.has("bubbles") {
                state.layers.heatmap = pose.has("heat");
                state.layers.bid_ask = pose.has("lines");
                state.layers.trades = pose.has("bubbles");
            }
            state.tpsl = pose.has("tpsl");
            state.reduce_only = pose.has("reduce");
            if pose.has("quote") {
                state.unit = trade::SizeUnit::Quote;
                state.size = "654.3".to_string();
            }
            if pose.has("big") {
                state.size = "1234567890.123".to_string();
            }
            // The history is the window's own, forgotten on a new address: seeded after the sync.
            if pose.view.chart {
                state.history.clear();
                for (k, b) in books.iter().enumerate() {
                    state.history.observe(now_ms - 120_000 + k as i64 * 250, b);
                }
            }
        }
        if button.is_none()
            && let Some(label) = pose.click
        {
            button = find_button(&out, label);
        }
        if pose.has("dump")
            && let Some(update) = out.platform_output.accesskit_update.as_ref()
        {
            for (id, node) in &update.nodes {
                if let (Some(b), label) = (node.bounds(), node.label().or_else(|| node.value())) {
                    let at = |v: f64| v as f32;
                    let origin = (body_origin.x, body_origin.y);
                    nodes.insert(
                        format!("{id:?}"),
                        format!(
                            "{:?}\t{}\t{:.1}\t{:.1}\t{:.1}\t{:.1}",
                            node.role(),
                            label.unwrap_or_default().replace(['\t', '\n'], " "),
                            at(b.x0) - origin.0,
                            at(b.y0) - origin.1,
                            at(b.x1 - b.x0),
                            at(b.y1 - b.y0),
                        ),
                    );
                }
            }
        }
        keep(&mut tex, &mut out);
        last = Some(out);
    }
    if pose.click.is_some() {
        assert!(button.is_some(), "no button named {:?} in the accessibility tree", pose.click);
    }
    let px = [(canvas.x * PPP) as u32, (canvas.y * PPP) as u32];
    let img = rasterize(&ctx, tex, last.expect("frames ran"), px, PPP, "trade_shot");
    img.save(path).expect("write PNG");
    if pose.has("dump") {
        let lines: Vec<&str> = nodes.values().map(String::as_str).collect();
        std::fs::write(path.with_extension("rects"), lines.join("\n")).expect("write rects");
    }
    // After the save, so a red run leaves its PNG as the diagnostic.
    let report =
        vike_ui_theme::pixel_liveness::pixel_report(img.as_raw(), img.width(), img.height());
    println!("pixel liveness [{}]: {report}", path.display());
    vike_ui_theme::pixel_liveness::assert_pixels_live(&report, &LIVENESS);
    println!("wrote {} ({}x{})", path.display(), img.width(), img.height());
}
