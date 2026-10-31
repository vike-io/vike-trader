//! **The Account window, read off a real frame at the widths and pixel densities a desktop has.**
//!
//! The window was captured on a 2560-wide screen against the live node (v0.1.86) and four things
//! were wrong that no test could see, because every earlier test ran on egui's 10,000-point default
//! canvas, where nothing is ever cut:
//!
//! 1. the equity table listed `binance`, `bybit` and `hyperliquid` twice with nothing to tell the two
//!    accounts of a venue apart, so a LIVE account read like a demo one (a safety defect);
//! 2. numbers (`1006910.45`) and venue names (`hyperliquid`) were cut to `1006910…` / `hyperliq…` in
//!    fixed-width cells, and nothing compared a text's width to its cell's;
//! 3. a working order read `SELL 0.0010 stop ACCEPTED`, with no venue, symbol or client order id;
//! 4. the tables took the left quarter of the window however wide it was.
//!
//! Each test below renders `account_tool_content` over a fixture shaped like that run (seventeen
//! equity rows, duplicates of a venue among them, seven-digit equities, six working orders over two
//! venues) at three widths and two densities, and reads the painted text: a `Shape::Text`'s galley
//! says whether it was elided (`Galley::elided`), what it says and where it sits.
//!
//! ⚠ Rows are told apart by their VERTICAL position (a band of text on one line), never by index into
//! a flat list of strings, so a cell that moves to another row fails the test that owns it.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use egui::epaint::Shape;
use egui::{Color32, Pos2, Rect, vec2};
use vike_app_core::data::catalog_refresh::CatalogRefresh;
use vike_app_core::tools::{ToolData, ToolView};
use vike_app_core::ui::tool_views::{
    BookCtx, ControlLink, HistoryCtx, StoredCtx, ToolCtx, account_tool_content,
};
use vike_change_journal::Proc;
use vike_exec::{BalanceMode, CoreSnapshot, OrderView, PositionView, PriceSource, VenueBlock};
use vike_model::accounts::account_keys::{parse_wire_account, route_key_of};
use vike_model::events::PositionSide;
use vike_model::{AccountTier, MarginMode, OrderStatus, OrderType, TradingState};
use vike_ui_theme::appearance::{self, Appearance};
use vike_ui_theme::components::{Tokens, chip};
use vike_ui_theme::value::account;

/// The three logical widths and two pixel densities every test reads. 2560 is the capture's width;
/// 640 is a small window; 1200 is the common one.
const WIDTHS: [f32; 3] = [640.0, 1200.0, 2560.0];
const DENSITIES: [f32; 2] = [1.0, 2.0];

/// The word the Trade window draws for an account whose tier the node did not say.
const TIER_UNKNOWN: &str = "MODE ?";
/// What an unlabelled account is called (the Trade window's picker calls it the same).
const MAIN: &str = "main";

/// One equity row of the fixture: the account a venue block names, its tier and its figures.
struct Acct {
    venue: &'static str,
    label: Option<&'static str>,
    tier: Option<AccountTier>,
    equity: f64,
    upnl: f64,
    fees: f64,
    funding: f64,
    missing: u32,
}

const fn acct(
    venue: &'static str,
    label: Option<&'static str>,
    tier: Option<AccountTier>,
    equity: f64,
) -> Acct {
    Acct { venue, label, tier, equity, upnl: 0.0, fees: 0.0, funding: 0.0, missing: 0 }
}

/// The capture's seventeen rows, shaped like it: venues one account each, then second accounts of
/// binance (the perp lane), oanda, ig, bybit and hyperliquid. The two LIVE accounts are the rows a
/// trader must never read as demo; `aster` is a node that did not say its tier.
///
/// ⚠ It names twelve of the roster's fourteen venues, on purpose: a file that names all (or all but
/// one) is taken by `crates/vike-ops/tests/venues/new_venue_gate.rs` for a per-venue table and must
/// then carry a scaffold marker; this fixture is not one. Keep two venues out of it.
fn fixture() -> Vec<Acct> {
    let (d, l, p) = (Some(AccountTier::Demo), Some(AccountTier::Live), Some(AccountTier::Paper));
    vec![
        Acct { fees: 0.3589, ..acct("binance", None, d, 4984.44) },
        Acct { fees: 0.0710, ..acct("okx", None, d, 4999.53) },
        Acct {
            upnl: -0.3178,
            fees: 0.0073,
            funding: -0.0010,
            ..acct("hyperliquid", None, d, 978.37)
        },
        acct("aster", None, None, 0.0),
        Acct { fees: 0.0100, ..acct("deribit", None, d, 999.98) },
        acct("alpaca", None, p, 49686.02),
        Acct { fees: 0.1200, ..acct("ctrader", None, d, 19997.13) },
        acct("ig", None, d, 19976.59),
        acct("oanda", None, d, 100000.00),
        Acct { missing: 2, ..acct("ibkr", None, d, 1006910.45) },
        acct("polymarket", None, l, 1000.00),
        acct("oanda", Some("SUB"), d, 1000.00),
        acct("ig", Some("SUB"), d, 10000.00),
        Acct { upnl: -1.2792, fees: 2.6112, ..acct("binance", Some("PERP"), d, 4990.70) },
        Acct { fees: 0.9520, ..acct("bybit", None, d, 124856.78) },
        acct("bybit", Some("LIVE"), l, 1.00),
        acct("hyperliquid", Some("LIVE"), l, 8.80),
    ]
}

fn venue_block(a: &Acct) -> VenueBlock {
    let label = a.label.map(|l| parse_wire_account(l).expect("a fixture label parses"));
    VenueBlock {
        venue: a.venue.to_string(),
        route_key: label.as_ref().map_or_else(|| a.venue.to_string(), |l| route_key_of(a.venue, l)),
        account: label,
        symbol: String::new(),
        extra_symbols: Default::default(),
        tier: a.tier,
        balance: 0.0,
        realized_pnl: 0.0,
        fees_paid: a.fees,
        funding_paid: a.funding,
        balance_mode: BalanceMode::Delta,
        contract_values: Default::default(),
        contract_value_default: vike_model::ContractValue::default(),
        admits_symbols: false,
        quote_rates: Vec::new(),
        quote_rates_in: Default::default(),
        ledger_ccy: Default::default(),
        ccy_assumed: false,
        colliding_fills_refused: 0,
        fees_unconverted: 0,
        fees_at_stale_rate: 0,
        equity: a.equity,
        unrealized: a.upnl,
        missing_prices: a.missing,
        margin_used: 0.0,
        free_bp: 0.0,
        margin_ratio: 0.0,
        fee_schedule: None,
        trading_state: TradingState::Active,
        positions: Vec::new(),
    }
}

/// One working order of the fixture.
struct Work {
    venue: &'static str,
    label: Option<&'static str>,
    symbol: &'static str,
    side: i32,
    qty: f64,
    kind: OrderType,
    status: OrderStatus,
}

/// Six orders over two venues; the second and fifth are the long words a real node sends
/// (`stop_limit`, `PARTIALLY_FILLED`) that the old fixed cells cut.
fn orders() -> Vec<Work> {
    let o = |venue, label, symbol, side, qty, kind, status| Work {
        venue,
        label,
        symbol,
        side,
        qty,
        kind,
        status,
    };
    vec![
        o("bybit", Some("LIVE"), "BTCUSDT", -1, 0.0010, OrderType::Stop, OrderStatus::Canceled),
        o("bybit", Some("LIVE"), "BTCUSDT", -1, 0.0010, OrderType::Stop, OrderStatus::Accepted),
        o("bybit", None, "ETHUSDT", 1, 0.0100, OrderType::StopLimit, OrderStatus::PartiallyFilled),
        o("hyperliquid", Some("LIVE"), "BTC", -1, 0.0010, OrderType::Market, OrderStatus::Filled),
        o("hyperliquid", None, "ETH", 1, 0.0250, OrderType::Limit, OrderStatus::Accepted),
        o("hyperliquid", None, "BTC", -1, 0.0002, OrderType::TakeProfit, OrderStatus::Accepted),
    ]
}

/// The client order id of fixture order `i`: a long, unique, venue-bearing id.
fn coid(i: usize, o: &Work) -> String {
    format!("vike-20261024-{i:04}-{}", o.venue)
}

fn order_view(i: usize, o: &Work) -> OrderView {
    OrderView {
        client_order_id: coid(i, o),
        venue: o.venue.to_string(),
        account: o.label.map(|l| parse_wire_account(l).expect("a fixture label parses")),
        symbol: o.symbol.to_string(),
        side: o.side,
        qty: o.qty,
        order_type: o.kind.clone(),
        price: None,
        trigger_price: None,
        status: o.status,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        data_delay: None,
        reason: None,
        pending_price: None,
    }
}

fn snapshot() -> CoreSnapshot {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = fixture().iter().map(venue_block).collect();
    snap.portfolio.equity_total = 1_350_390.12;
    snap.portfolio.missing_prices_total = 2;
    snap.orders = orders().iter().enumerate().map(|(i, o)| order_view(i, o)).collect();
    snap
}

/// Everything `ToolCtx` borrows, owned, so a pass can build one.
struct Owned {
    td: ToolData,
    snap: CoreSnapshot,
    textures: HashMap<String, egui::TextureHandle>,
    dsets: vike_data::datasets::Store,
    gaps: vike_data_manager::GapMap,
    partials: vike_data_manager::PartialDayMap,
    statuses: HashMap<String, Arc<std::sync::Mutex<String>>>,
    proc: Proc,
    catalog: CatalogRefresh,
    symbols: Arc<vike_catalog::Catalog>,
}

impl Owned {
    fn new() -> Self {
        // The catalog handle does no I/O at construction and the Account window never reads it.
        let (tx, _rx) = std::sync::mpsc::channel();
        Owned {
            td: ToolData::default(),
            snap: snapshot(),
            textures: HashMap::new(),
            dsets: vike_data::datasets::Store::default(),
            gaps: vike_data_manager::GapMap::default(),
            partials: vike_data_manager::PartialDayMap::default(),
            statuses: HashMap::new(),
            proc: Proc::new("vike-test", 4711, "0.1.0"),
            catalog: CatalogRefresh::new(Vec::new(), None, None, None, tx),
            symbols: Arc::new(vike_catalog::Catalog::from_instruments(Vec::new())),
        }
    }

    fn ctx(&self) -> ToolCtx<'_> {
        ToolCtx {
            td: &self.td,
            snap: &self.snap,
            flags: &self.textures,
            logos: &self.textures,
            history: HistoryCtx::none(),
            feeds: &[],
            dsets: &self.dsets,
            display_tz: vike_chart::DisplayTz::default(),
            stored: StoredCtx {
                tree: &[],
                gaps: &self.gaps,
                partials: &self.partials,
                loading: false,
                load_error: None,
                backfill_status: "",
                delete_unavailable: None,
                partials_note: None,
                history: None,
            },
            feed_statuses: &self.statuses,
            credentials: vike_gateway_panel::CredentialWrite {
                settings_dir: Path::new(""),
                journal: None,
                proc: &self.proc,
                now_ms: 0,
            },
            book: BookCtx { symbol: "", venue: "", book: None, stale: false, trades: None },
            catalog: &self.catalog,
            symbols: &self.symbols,
            last_trade: None,
            directory: None,
            directory_unavailable: false,
            key_changed: &[],
            control_link: ControlLink::Open,
            node_takes_post_only: false,
            datahub_unreachable: None,
        }
    }
}

/// One run of painted text: what it says, where it sits, and whether the layout cut it short.
#[derive(Clone, Debug)]
struct Txt {
    text: String,
    rect: Rect,
    elided: bool,
}

/// One painted rectangle: where, the fill, the stroke's colour.
#[derive(Clone, Copy, Debug)]
struct Painted {
    rect: Rect,
    fill: Color32,
    edge: Color32,
}

/// One frame of the window at one width and density, read as lines of text.
struct Frame {
    width: f32,
    /// Lines of text, top to bottom; each line's texts left to right.
    lines: Vec<Vec<Txt>>,
    boxes: Vec<Painted>,
    tokens: Tokens,
}

fn flatten(shape: Shape, out: &mut Vec<Shape>) {
    match shape {
        Shape::Vec(v) => v.into_iter().for_each(|s| flatten(s, out)),
        s => out.push(s),
    }
}

/// Texts on one line sit within a few points of each other (a chip's centre against a number's);
/// two lines are a row (18 points) apart.
const SAME_LINE: f32 = 6.0;

/// One pass of the window at `time` seconds with `events` delivered, and the shapes it painted.
fn run_pass(
    ctx: &egui::Context,
    owned: &Owned,
    tv: &mut ToolView,
    (width, ppp): (f32, f32),
    (time, events): (f64, Vec<egui::Event>),
) -> Vec<Shape> {
    let mut raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 1600.0))),
        time: Some(time),
        events,
        ..Default::default()
    };
    raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);
    let mut full = ctx.run_ui(raw, |ui| account_tool_content(ui, &owned.ctx(), tv));
    full.textures_delta.clear();
    let mut shapes = Vec::new();
    for clipped in std::mem::take(&mut full.shapes) {
        flatten(clipped.shape, &mut shapes);
    }
    shapes
}

fn render(width: f32, ppp: f32) -> Frame {
    frame_of(&Owned::new(), width, ppp)
}

/// The window over `owned`'s snapshot, read as [`render`] reads the fixture's.
fn frame_of(owned: &Owned, width: f32, ppp: f32) -> Frame {
    let ctx = egui::Context::default();
    appearance::install(&ctx, &Appearance::default());
    let mut tv = ToolView::default();
    // Pass 0 lets the bundled fonts land (they apply on the NEXT pass); 1 and 2 are the layout, and
    // the last is the one read.
    let mut shapes = Vec::new();
    for pass in 0..3u32 {
        let at = (f64::from(pass) / 60.0, Vec::new());
        shapes = run_pass(&ctx, owned, &mut tv, (width, ppp), at);
    }
    read(&shapes, width, &ctx)
}

/// The painted `shapes` as lines of text and boxes.
fn read(shapes: &[Shape], width: f32, ctx: &egui::Context) -> Frame {
    let mut texts = Vec::new();
    let mut boxes = Vec::new();
    for s in shapes {
        match s {
            Shape::Text(t) => texts.push(Txt {
                text: t.galley.text().to_string(),
                rect: t.galley.rect.translate(t.pos.to_vec2()),
                elided: t.galley.elided,
            }),
            Shape::Rect(r) => {
                boxes.push(Painted { rect: r.rect, fill: r.fill, edge: r.stroke.color });
            }
            _ => {}
        }
    }
    texts.sort_by(|a, b| a.rect.center().y.total_cmp(&b.rect.center().y));
    let mut lines: Vec<Vec<Txt>> = Vec::new();
    for t in texts {
        match lines.last_mut() {
            Some(l) if (t.rect.center().y - l[l.len() - 1].rect.center().y).abs() < SAME_LINE => {
                l.push(t);
            }
            _ => lines.push(vec![t]),
        }
    }
    for l in &mut lines {
        l.sort_by(|a, b| a.rect.left().total_cmp(&b.rect.left()));
    }
    Frame { width, lines, boxes, tokens: Tokens::of(ctx) }
}

impl Frame {
    fn line_with(&self, words: &[&str]) -> usize {
        self.lines
            .iter()
            .position(|l| words.iter().all(|w| l.iter().any(|t| t.text == *w)))
            .unwrap_or_else(|| panic!("no line of text holds {words:?}: {:#?}", self.lines))
    }

    /// The Accounts table's header line, and the lines of its rows (one per equity row).
    fn account_rows(&self) -> (&[Txt], &[Vec<Txt>]) {
        let head = self.line_with(&["Venue", "Equity"]);
        let orders = self
            .lines
            .iter()
            .position(|l| l.iter().any(|t| t.text.starts_with("Orders (")))
            .expect("the Orders title is painted");
        (&self.lines[head], &self.lines[head + 1..orders])
    }

    /// The lines of the working-orders rows.
    fn order_rows(&self) -> &[Vec<Txt>] {
        let at = self
            .lines
            .iter()
            .position(|l| l.iter().any(|t| t.text.starts_with("Orders (")))
            .expect("the Orders title is painted");
        &self.lines[at + 1..]
    }

    /// The painted rectangles on `line`'s row: a chip is one.
    fn boxes_on(&self, line: &[Txt]) -> Vec<Painted> {
        let y = line.iter().map(|t| t.rect.center().y).sum::<f32>() / line.len() as f32;
        self.boxes
            .iter()
            .copied()
            .filter(|b| (b.rect.center().y - y).abs() < SAME_LINE && b.rect.width() < 120.0)
            .collect()
    }
}

/// Every (width, density) the tests read, with its frame.
fn frames() -> Vec<Frame> {
    let mut out = Vec::new();
    for w in WIDTHS {
        for d in DENSITIES {
            out.push(render(w, d));
        }
    }
    out
}

fn tier_word(tier: Option<AccountTier>) -> &'static str {
    tier.map_or(TIER_UNKNOWN, chip::tier_label)
}

fn count(line: &[Txt], text: &str) -> usize {
    line.iter().filter(|t| t.text == text).count()
}

/// Panic with every failure at once: which row, at which width and density, missed what.
fn all_ok(what: &str, failures: Vec<String>) {
    assert!(failures.is_empty(), "{what}:\n{}", failures.join("\n"));
}

/// ⚠ **Every equity row says WHICH ACCOUNT and WHICH TIER it is** (defect 1), in words, on its own
/// line, at every width and density: two `bybit` rows are told apart by their label, and the tier
/// is a LIVE / DEMO / PAPER word, or `MODE ?` where the node did not say (never PAPER).
#[test]
fn every_equity_row_names_its_account_and_its_tier() {
    let mut failures = Vec::new();
    for f in frames() {
        let (_, rows) = f.account_rows();
        if rows.len() != fixture().len() {
            failures.push(format!(
                "{}: {} equity lines, not {}",
                f.width,
                rows.len(),
                fixture().len()
            ));
            continue;
        }
        for (a, line) in fixture().iter().zip(rows) {
            // The words the row must carry. A LIVE account NAMED `LIVE` carries the word twice, once
            // as its name and once as its tier: counted, so one cannot stand in for the other.
            let want = [
                a.venue.to_string(),
                a.label.unwrap_or(MAIN).to_string(),
                tier_word(a.tier).to_string(),
                format!("{:.2}", a.equity),
            ];
            for w in &want {
                let needed = want.iter().filter(|x| *x == w).count();
                if count(line, w) < needed {
                    failures.push(format!(
                        "{}w: the {} {:?} row has no {w:?} (x{needed}): {:?}",
                        f.width,
                        a.venue,
                        a.label,
                        line.iter().map(|t| t.text.as_str()).collect::<Vec<_>>()
                    ));
                }
            }
        }
    }
    all_ok("an equity row must name its account and tier", failures);
}

/// ⚠ **A LIVE row is drawn differently from a demo one**, by the theme's own tier chip (LIVE filled
/// in the accent, DEMO outlined in the info blue, owner rulings of 2026-10-03 and 2026-10-05), and a
/// tier the node did not say is never drawn as LIVE's fill.
#[test]
fn a_live_row_is_drawn_differently_from_a_demo_row() {
    let mut failures = Vec::new();
    for f in frames() {
        let (_, rows) = f.account_rows();
        if rows.len() != fixture().len() {
            failures.push(format!(
                "{}: {} equity lines, not {}",
                f.width,
                rows.len(),
                fixture().len()
            ));
            continue;
        }
        let (live_fill, ..) = chip::tier_colours(AccountTier::Live, &f.tokens);
        let (_, demo_edge, _) = chip::tier_colours(AccountTier::Demo, &f.tokens);
        assert_ne!(live_fill, Color32::TRANSPARENT, "LIVE is a filled chip");
        for (a, line) in fixture().iter().zip(rows) {
            let boxes = f.boxes_on(line);
            let filled_live = boxes.iter().any(|b| b.fill == live_fill);
            let outlined_demo =
                boxes.iter().any(|b| b.fill == Color32::TRANSPARENT && b.edge == demo_edge.color);
            let bad = match a.tier {
                Some(AccountTier::Live) => (!filled_live).then_some("no filled LIVE chip"),
                Some(AccountTier::Demo) => {
                    (!outlined_demo || filled_live).then_some("not an outlined DEMO chip")
                }
                // PAPER, and a tier the node did not say: anything but LIVE's fill.
                _ => filled_live.then_some("wears LIVE's fill"),
            };
            if let Some(why) = bad {
                failures.push(format!("{}w {} {:?}: {why}: {boxes:?}", f.width, a.venue, a.label));
            }
        }
    }
    all_ok("a LIVE row must look unlike a demo row", failures);
}

/// **No number or name is cut** (defect 2): at 1200 and 2560 no text of the equity rows, the
/// header or the order rows is elided, and no two texts of one row overlap. The equity table never
/// cuts at ANY width (its columns are sized from what they hold). An order's client id may be cut
/// below that, to its row's room, and then carries its whole text on hover.
#[test]
fn no_text_of_the_tables_is_elided() {
    let mut failures = Vec::new();
    for f in frames() {
        let (head, rows) = f.account_rows();
        let wide = f.width >= 1200.0;
        let mut checked: Vec<(&str, &[Txt])> = vec![("header", head)];
        checked.extend(rows.iter().map(|l| ("equity row", l.as_slice())));
        if wide {
            checked.extend(f.order_rows().iter().map(|l| ("order row", l.as_slice())));
        }
        for (what, line) in checked {
            for t in line.iter().filter(|t| t.elided) {
                failures.push(format!("{}w {what}: {:?} is elided", f.width, t.text));
            }
            for pair in line.windows(2) {
                if pair[0].rect.right() > pair[1].rect.left() + 0.5 {
                    failures.push(format!(
                        "{}w {what}: {:?} overlaps {:?}",
                        f.width, pair[0].text, pair[1].text
                    ));
                }
            }
        }
    }
    all_ok("the tables must not cut or collide text", failures);
}

/// ⚠ **Every working order names its venue, its symbol and its client order id** (defect 3), and a
/// labelled account's order names the account too. Six rows over two venues, in the order sent.
#[test]
fn every_working_order_names_its_venue_symbol_and_client_order_id() {
    let mut failures = Vec::new();
    for f in frames() {
        let rows = f.order_rows();
        if rows.len() != orders().len() {
            failures.push(format!(
                "{}: {} order lines, not {}",
                f.width,
                rows.len(),
                orders().len()
            ));
            continue;
        }
        for (i, (o, line)) in orders().iter().zip(rows).enumerate() {
            let has = |p: &dyn Fn(&str) -> bool| line.iter().any(|t| p(&t.text));
            let who = format!("{}w order {i} ({} {})", f.width, o.venue, o.symbol);
            if !has(&|s| s.contains(o.venue)) {
                failures.push(format!("{who}: no venue"));
            }
            if let Some(label) = o.label
                && !has(&|s| s.contains(label))
            {
                failures.push(format!("{who}: no account {label:?}"));
            }
            if !has(&|s| s == o.symbol) {
                failures.push(format!("{who}: no symbol"));
            }
            if !has(&|s| s == coid(i, o)) {
                failures.push(format!("{who}: no client order id"));
            }
            // The words that were always there stay, whole (`stop_limit`, `PARTIALLY_FILLED`).
            for word in [o.status.as_str(), o.kind.as_str()] {
                if !has(&|s| s == word) {
                    failures.push(format!("{who}: no {word:?}"));
                }
            }
        }
    }
    all_ok("an order row must say what it is", failures);
}

/// **The tables use the window's width up to the content cap, not its left quarter** (defect 4):
/// the table's right edge (the Funding header's) reaches most of `min(window, MAX_W)`, and nothing
/// of the equity table is painted past the cap.
#[test]
fn the_tables_use_the_available_width_up_to_the_cap() {
    let mut failures = Vec::new();
    for f in frames() {
        let (head, rows) = f.account_rows();
        let left = head.iter().map(|t| t.rect.left()).fold(f32::INFINITY, f32::min);
        let cap = (f.width - left).min(account::MAX_W);
        let funding = head.iter().find(|t| t.text == "Funding").expect("a Funding header");
        let used = funding.rect.right() - left;
        if used < 0.7 * cap {
            failures.push(format!(
                "{}w: the table ends {used:.0} points in, under 70% of its {cap:.0} points",
                f.width
            ));
        }
        let farthest =
            rows.iter().flatten().chain(head).map(|t| t.rect.right() - left).fold(0.0, f32::max);
        if farthest > cap + 1.0 {
            failures.push(format!(
                "{}w: text is painted {farthest:.0} points in, past the cap {cap:.0}",
                f.width
            ));
        }
    }
    all_ok("the tables must fill the window up to the cap", failures);
}

/// **A column is one column**: the four numbers of every row end where their header ends, and the
/// tier chips and the balance-mode tags of all rows start at one x. A cell that took only the room
/// its content used (a LIVE chip is narrower than a DEMO one) would slide every cell after it, and
/// the numbers would no longer sit under their headers.
#[test]
fn every_row_lines_up_under_its_headers() {
    const SLACK: f32 = 1.5;
    let mut failures = Vec::new();
    for f in frames() {
        let (head, rows) = f.account_rows();
        let ends: Vec<f32> = ["Equity", "uPnL", "Fees", "Funding"]
            .iter()
            .map(|h| head.iter().find(|t| t.text == *h).expect("a numeric header").rect.right())
            .collect();
        let (mut chips, mut modes) = (Vec::new(), Vec::new());
        for (a, line) in fixture().iter().zip(rows) {
            let nums: Vec<f32> = line
                .iter()
                .filter(|t| t.text.parse::<f64>().is_ok())
                .map(|t| t.rect.right())
                .collect();
            if nums.len() != ends.len() {
                failures.push(format!("{}w {}: {} numbers, not 4", f.width, a.venue, nums.len()));
                continue;
            }
            for (n, e) in nums.iter().zip(&ends) {
                if (n - e).abs() > SLACK {
                    failures.push(format!(
                        "{}w {} {:?}: a number ends at {n:.0}, its header at {e:.0}",
                        f.width, a.venue, a.label
                    ));
                }
            }
            // The chip is the LAST text that says the tier (an account may be NAMED `LIVE`).
            chips.extend(line.iter().rev().find(|t| t.text == tier_word(a.tier)));
            modes.extend(line.iter().find(|t| t.text == "\u{0394}"));
        }
        for (what, xs) in [("tier chips", &chips), ("mode tags", &modes)] {
            let lefts: Vec<f32> = xs.iter().map(|t| t.rect.left()).collect();
            let (lo, hi) =
                lefts.iter().fold((f32::MAX, f32::MIN), |(lo, hi), x| (lo.min(*x), hi.max(*x)));
            if lefts.len() != fixture().len() || hi - lo > SLACK {
                failures.push(format!(
                    "{}w: the {what} start between {lo:.0} and {hi:.0} ({} of {})",
                    f.width,
                    lefts.len(),
                    fixture().len()
                ));
            }
        }
    }
    all_ok("the Accounts table's columns must line up", failures);
}

/// A position of `venue`: priced by the last trade, or (`unpriced`) by no source at all.
fn position(venue: &str, symbol: &str, unpriced: bool) -> PositionView {
    PositionView {
        venue: venue.into(),
        symbol: symbol.into(),
        position_side: PositionSide::Both,
        size: 1.0,
        avg_px: 100.0,
        unrealized: 0.0,
        mark_source: (!unpriced).then_some(PriceSource::LastTrade),
        leverage: 0.0,
        liq_price: 0.0,
        margin_mode: MarginMode::Cross,
        isolated_margin: None,
    }
}

/// The texts that appear when the pointer rests on the unpriced badge of equity row `row` for over
/// half a second (egui's tooltip delay), and were not there before.
fn tooltip_of(owned: &Owned, row: usize, (width, ppp): (f32, f32)) -> Vec<String> {
    let ctx = egui::Context::default();
    appearance::install(&ctx, &Appearance::default());
    let mut tv = ToolView::default();
    // 0.3 s a pass: the pointer lands on pass 3 and has been still for 0.6 s by pass 5.
    let mut pass = |n: u32, events: Vec<egui::Event>| {
        run_pass(&ctx, owned, &mut tv, (width, ppp), (f64::from(n) * 0.3, events))
    };
    let mut shapes = Vec::new();
    for n in 0..3 {
        shapes = pass(n, Vec::new());
    }
    let before = read(&shapes, width, &ctx);
    let badge = before.account_rows().1[row].last().expect("the row paints its badge last");
    let _ = pass(3, vec![egui::Event::PointerMoved(badge.rect.center())]);
    for n in 4..7 {
        shapes = pass(n, Vec::new());
    }
    let mut then: Vec<String> = before.lines.iter().flatten().map(|t| t.text.clone()).collect();
    let mut new = Vec::new();
    for s in &shapes {
        if let Shape::Text(t) = s {
            let text = t.galley.text().to_string();
            match then.iter().position(|x| *x == text) {
                Some(i) => {
                    then.swap_remove(i);
                }
                None => new.push(text),
            }
        }
    }
    new
}

/// ⚠ **An unpriced badge lists the positions of ITS OWN account and no other's.** A node that runs a
/// demo and a LIVE bybit lists two blocks named `bybit`; the badge's hover text found the block BY
/// VENUE NAME, so the second row's tooltip named the first one's positions, a LIVE account's under a
/// demo row and the reverse. Six accounts over three venues, each with its own unpriced symbols and
/// a priced one that must not be listed, are hovered at every width and density.
#[test]
fn an_unpriced_badge_lists_its_own_accounts_positions_and_no_other() {
    // (venue, account label, the symbols no source prices).
    let cases: [(&str, Option<&str>, &[&str]); 6] = [
        ("binance", None, &["ADAUSDT"]),
        ("binance", Some("PERP"), &["SHIBUSDT", "XRPUSDT"]),
        ("bybit", None, &["DOGEUSDT", "PEPEUSDT"]),
        ("bybit", Some("LIVE"), &["WIFUSDT"]),
        ("hyperliquid", None, &["kPEPE"]),
        ("hyperliquid", Some("LIVE"), &["kBONK", "kFLOKI"]),
    ];
    let row_of = |venue: &str, label: Option<&str>| {
        fixture()
            .iter()
            .position(|a| a.venue == venue && a.label == label)
            .expect("the fixture holds the account")
    };
    let mut owned = Owned::new();
    for (venue, label, symbols) in cases {
        let block = &mut owned.snap.portfolio.venues[row_of(venue, label)];
        block.missing_prices = u32::try_from(symbols.len()).expect("a few symbols");
        block.positions = std::iter::once(position(venue, "BTCUSDT", false))
            .chain(symbols.iter().map(|s| position(venue, s, true)))
            .collect();
    }
    let mut failures = Vec::new();
    for width in WIDTHS {
        for ppp in DENSITIES {
            for (venue, label, symbols) in cases {
                let said = tooltip_of(&owned, row_of(venue, label), (width, ppp));
                let want = symbols.join(", ");
                let others = cases
                    .iter()
                    .filter(|c| (c.0, c.1) != (venue, label))
                    .flat_map(|c| c.2.iter().copied())
                    .chain(std::iter::once("BTCUSDT"));
                let leaked: Vec<&str> =
                    others.filter(|s| said.iter().any(|t| t.contains(*s))).collect();
                if !said.contains(&want) || !leaked.is_empty() {
                    failures.push(format!(
                        "{width}w x{ppp} {venue} {label:?}: wanted {want:?}, hovering it painted \
                         {said:?} (names {leaked:?} of other accounts or a priced position)"
                    ));
                }
            }
        }
    }
    all_ok("an unpriced badge must list its own account's positions", failures);
}

/// ⚠ **A conversion rate is drawn, and a stale one under a position says so** (decision 0183): the Account
/// window lists the rates behind an account's figures under the Accounts table, in the one wording every
/// surface uses, whole at every width and density. A node with no such account draws no rates section at all
/// (every other test of this file runs over that fixture and finds the rows where they always were).
#[test]
fn a_stale_conversion_rate_is_drawn_whole_and_a_plain_node_draws_none() {
    use vike_exec::QuoteRateView;

    let plain = frame_of(&Owned::new(), 1200.0, 1.0);
    assert!(
        plain.lines.iter().flatten().all(|t| t.text != "Conversion rates"),
        "no foreign-quoted account, no section"
    );

    let mut owned = Owned::new();
    let at = fixture().iter().position(|a| a.venue == "oanda" && a.label.is_none()).expect("oanda");
    let block = &mut owned.snap.portfolio.venues[at];
    block.quote_rates_in = "USD".into();
    block.quote_rates = vec![
        QuoteRateView {
            ccy: "JPY".into(),
            rate: 1.0 / 150.0,
            as_of_ms: 1,
            stale: true,
            held: true,
        },
        QuoteRateView { ccy: "GBP".into(), rate: 1.35, as_of_ms: 2, stale: false, held: false },
    ];
    let mut failures = Vec::new();
    for width in WIDTHS {
        for ppp in DENSITIES {
            let f = frame_of(&owned, width, ppp);
            let texts: Vec<&Txt> = f.lines.iter().flatten().collect();
            for want in [
                "Conversion rates",
                "1 JPY = 0.006667 USD STALE (an open position is valued at it)",
                "1 GBP = 1.350000 USD",
            ] {
                match texts.iter().find(|t| t.text == want) {
                    None => failures.push(format!("{width}w x{ppp}: {want:?} is not painted")),
                    Some(t) if t.elided => failures.push(format!("{width}w x{ppp}: {want:?} cut")),
                    Some(_) => {}
                }
            }
            // the section sits between the Accounts table and the Orders title
            let title = f.line_with(&["Conversion rates"]);
            let orders =
                f.lines.iter().position(|l| l.iter().any(|t| t.text.starts_with("Orders (")));
            if orders.is_none_or(|o| o < title) {
                failures.push(format!("{width}w x{ppp}: the rates are not above the Orders title"));
            }
        }
    }
    all_ok("a conversion rate must be drawn whole", failures);
}
