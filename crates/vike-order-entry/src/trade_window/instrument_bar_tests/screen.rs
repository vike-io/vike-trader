//! The instrument bar drawn on a harness: the account and symbol pickers, the feed badge, and the
//! bar's fit at every width in every look.

use crate::trade_window::{TradeWindowAccountPickerConnectRow, TradeWindowTradable};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use std::collections::BTreeMap;
use vike_marketdata::{L2Book, L2BookLevel};
use vike_venues::VenueCapabilities;

use super::*;

/// What one harness draws. Owned, so the harness can lend it to `TradeWindowInputs` every frame.
#[derive(Clone)]
struct Scene {
    venue: &'static str,
    venue_label: &'static str,
    account: Option<&'static str>,
    symbol: &'static str,
    mode: Option<AccountTier>,
    source: Option<DataClientStatus>,
    accounts: Vec<TradeWindowAccountPickerRow<'static>>,
    recent: Vec<(&'static str, &'static str)>,
    matches: Vec<SymbolPickerRow<'static>>,
    unconnected: Vec<TradeWindowAccountPickerConnectRow<'static>>,
    /// Whether the book has both sides, so the bar shows a spread.
    spread: bool,
}

/// A window on okx's BTC swap, on the venue's default account, PAPER, with a live depth link.
fn okx() -> Scene {
    Scene {
        venue: "okx",
        venue_label: "OKX",
        account: None,
        symbol: "BTC-USDT-SWAP",
        mode: Some(AccountTier::Paper),
        source: Some(live_source()),
        accounts: vec![row("okx", "OKX", None, "BTC-USDT-SWAP", Some(AccountTier::Paper))],
        recent: Vec::new(),
        matches: Vec::new(),
        unconnected: Vec::new(),
        spread: true,
    }
}

/// The widest bar the window draws (the Trade window's fit scene): a long swap symbol, a long
/// venue and account, and whichever mode chip and FEED word it is given.
fn widest(mode: Option<AccountTier>, source: DataClientStatus) -> Scene {
    Scene {
        venue: "hyperliquid",
        venue_label: "Hyperliquid",
        account: Some("sub-account-2"),
        symbol: "BTC-USDT-SWAP",
        mode,
        source: Some(source),
        accounts: vec![row(
            "hyperliquid",
            "Hyperliquid",
            Some("sub-account-2"),
            "BTC-USDT-SWAP",
            mode,
        )],
        recent: Vec::new(),
        matches: Vec::new(),
        unconnected: Vec::new(),
        spread: true,
    }
}

/// The two looks the widest bar is measured in: a LIVE account on a live link, and an account
/// of unknown mode on a link that is dialling — the widest chip and the widest FEED word.
fn widest_looks() -> [(Option<AccountTier>, DataClientStatus); 2] {
    [(Some(AccountTier::Live), live_source()), (None, dialling_source())]
}

struct Fixture {
    state: TradeWindowState,
    scene: Scene,
    emitted: Vec<TradeWindowAction>,
    /// The band the bar was drawn into, as the window's `draw` sizes it.
    band: egui::Rect,
    /// How many rows the bar took ([`rows_for`]).
    rows: usize,
}

/// A one-level book, 65,432.0 / 65,432.5: a spread of exactly 0.5.
fn book() -> L2Book {
    let mut b = L2Book::new(0.1);
    b.apply_snapshot(1, &[L2BookLevel::new(65_432.0, 1.0)], &[L2BookLevel::new(65_432.5, 1.0)]);
    b
}

/// The same book with its asks gone: no spread to show.
fn bids_only() -> L2Book {
    let mut b = L2Book::new(0.1);
    b.apply_snapshot(1, &[L2BookLevel::new(65_432.0, 1.0)], &[]);
    b
}

/// The bar alone, drawn as the window draws it: into a clipped band [`height`] high across the
/// width, under the appearance `look`. Tooltips show at once: egui's 0.5 s delay outlasts
/// `Harness::run`.
fn harness(scene: Scene, look: ThemeAppearance, size: egui::Vec2) -> Harness<'static, Fixture> {
    let (two_sided, one_sided) = (book(), bids_only());
    Harness::builder().with_size(size).build_ui_state(
        move |ui, f: &mut Fixture| {
            if !vike_ui_theme::harness::appearance_ready(ui.ctx(), &look) {
                return;
            }
            ui.ctx().all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
            let s = &f.scene;
            let book = if s.spread { &two_sided } else { &one_sided };
            let inputs = TradeWindowInputs {
                venue: s.venue,
                venue_label: s.venue_label,
                product: "",
                account: s.account,
                symbol: s.symbol,
                base: "BTC",
                quote: "USDT",
                mode: s.mode,
                tradable: TradeWindowTradable::Yes,
                grid: BTC,
                book,
                last: Some(65_432.1),
                stale: false,
                source: s.source.as_ref(),
                absence: None,
                top: None,
                orders: &[],
                orders_why: None,
                position: None,
                buying_power: None,
                value: vike_model::ContractValue::default(),
                capabilities: VenueCapabilities::UNSUPPORTED,
                bracket_why: s
                    .account
                    .map(|_| crate::trade_window::ticket::TAKE_PROFIT_STOP_LOSS_ACCOUNT_WHY),
                bracket_wire: false,
                post_only_wire: false,
                matches: &s.matches,
                recent: &s.recent,
                accounts: &s.accounts,
                accounts_why: None,
                unconnected: &s.unconnected,
                tape: &[],
                status: None,
                held_says: None,
            };
            let t = ThemeTokens::of(ui.ctx());
            let full = ui.available_rect_before_wrap();
            let n = rows_for(ui.ctx(), &t, &inputs, full.width());
            f.rows = n;
            let band =
                egui::Rect::from_min_size(full.min, egui::vec2(full.width(), height_of(n, &t)));
            let mut region = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(band)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            region.shrink_clip_rect(band);
            bar(&mut region, &t, &mut f.state, &inputs, &mut f.emitted);
            f.band = band;
        },
        Fixture {
            state: TradeWindowState::default(),
            scene,
            emitted: Vec::new(),
            band: egui::Rect::NOTHING,
            rows: 0,
        },
    )
}

/// Every word the last frame painted.
fn painted(h: &Harness<'_, Fixture>) -> Vec<String> {
    fn walk(s: &egui::Shape, out: &mut Vec<String>) {
        match s {
            egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for c in &h.output().shapes {
        walk(&c.shape, &mut out);
    }
    out
}

/// Open the account picker: the bar's button names the window's venue and account.
fn open_accounts(h: &mut Harness<'static, Fixture>, venue_and_account: &str) {
    h.run();
    h.get_by_label_contains(venue_and_account).click();
    h.run();
}

/// An account whose mode the backend did not report reads `MODE ?` in the bar AND in the
/// account picker — the list a trader picks where an order goes from — and never PAPER (spec
/// §4.3). An older node reports no mode for any account, so every row is unknown, and such a row
/// still picks like any other.
#[test]
fn an_unknown_mode_reads_mode_unknown_and_never_paper() {
    let mut scene = okx();
    scene.mode = None;
    scene.accounts = vec![
        row("okx", "OKX", None, "BTC-USDT-SWAP", None),
        row("bybit", "Bybit", None, "BTCUSDT", None),
    ];
    let mut h = harness(scene, ThemeAppearance::default(), egui::vec2(600.0, 420.0));
    open_accounts(&mut h, "OKX · main");
    let words = painted(&h);
    assert_eq!(
        words.iter().filter(|w| *w == "MODE ?").count(),
        3,
        "the bar's chip and both rows: {words:?}"
    );
    assert!(!words.iter().any(|w| w.contains("PAPER")), "{words:?}");
    h.get_by_label("Bybit, main · BTCUSDT, MODE ?").click();
    h.run();
    assert_eq!(
        h.state().emitted,
        [TradeWindowAction::PickAccount {
            venue: "bybit".to_string(),
            account: None,
            symbol: "BTCUSDT".to_string(),
        }]
    );
}

/// A venue spells one instrument its own way, so picking an account on ANOTHER venue opens that
/// venue's own symbol, never the window's; and a recent instrument is a (venue, symbol) pair, so
/// picking one moves the window to its venue too.
#[test]
fn a_pick_on_another_venue_carries_that_venues_own_symbol() {
    let mut scene = okx();
    scene.accounts.push(row("binance", "Binance", None, "BTCUSDT", Some(AccountTier::Demo)));
    scene.recent = vec![("bybit", "BTCUSDT")];
    let mut h = harness(scene, ThemeAppearance::default(), egui::vec2(600.0, 420.0));
    open_accounts(&mut h, "OKX · main");
    h.get_by_label("Binance, main · BTCUSDT, DEMO").click();
    h.run();
    assert_eq!(
        h.state().emitted,
        [TradeWindowAction::PickAccount {
            venue: "binance".to_string(),
            account: None,
            symbol: "BTCUSDT".to_string(),
        }]
    );
    h.state_mut().emitted.clear();
    h.get_by_label_contains("BTC-USDT-SWAP").click();
    h.run();
    h.get_by_label_contains("BTCUSDT · bybit").click();
    h.run();
    assert_eq!(
        h.state().emitted,
        [TradeWindowAction::PickSymbol {
            venue: "bybit".to_string(),
            symbol: "BTCUSDT".to_string()
        }]
    );
}

/// An account the database holds but the server does not run is listed greyed, says why on
/// hover, and takes no click (spec §3.3). Hover needs the zero tooltip delay `harness` sets.
#[test]
fn a_greyed_account_says_why_and_takes_no_pick() {
    let mut scene = okx();
    scene.accounts.push(TradeWindowAccountPickerRow {
        why_not: Some(NOT_RUNNING),
        ..row("okx", "OKX", Some("HEDGE"), "BTC-USDT-SWAP", Some(AccountTier::Live))
    });
    let mut h = harness(scene, ThemeAppearance::default(), egui::vec2(600.0, 420.0));
    open_accounts(&mut h, "OKX · main");
    h.get_by_label("OKX, HEDGE · BTC-USDT-SWAP, LIVE").hover();
    h.run();
    assert!(h.query_by_label(NOT_RUNNING).is_some(), "the reason shows on hover");
    h.get_by_label("OKX, HEDGE · BTC-USDT-SWAP, LIVE").click();
    h.run();
    assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
}

/// A venue with no depth line shows `FEED ?` where the window names its link, not `FEED DOWN`. A
/// window narrow enough for the bar to take three rows names it in the bar's last row (a wider
/// one has the status strip's right end, `status_line::status_line_tests`).
#[test]
fn an_empty_source_shows_feed_unknown_in_the_narrow_bar() {
    let mut scene = okx();
    scene.source = None;
    let mut h = harness(scene, ThemeAppearance::default(), egui::vec2(300.0, 200.0));
    h.run();
    assert!(h.query_by_label("FEED ?").is_some());
    assert!(h.query_by_label("FEED DOWN").is_none());
}

/// A wide bar does not carry the FEED word: the strip does, and a window names its link once.
#[test]
fn a_wide_bar_leaves_the_feed_word_to_the_strip() {
    let mut h = harness(okx(), ThemeAppearance::default(), egui::vec2(700.0, 200.0));
    h.run();
    assert!(h.query_by_label_contains("FEED").is_none());
}

/// The FW2 review: the bar's price is the book's MID on the desktop, because no last trade
/// price reaches it (a snapshot from the node carries no marks), so its hover leads with that
/// and never calls it the last price.
#[test]
fn the_bars_price_says_it_is_the_books_mid() {
    let mut h = harness(okx(), ThemeAppearance::default(), egui::vec2(600.0, 200.0));
    h.run();
    h.get_by_label("65,432.1").hover();
    h.run();
    assert!(h.query_by_label_contains("Mid price").is_some(), "the hover names the mid");
    assert!(h.query_by_label_contains("Last price").is_none(), "and no last price");
}

/// The symbol picker's DRAWN rows (the owner's v3 design): one per instrument and venue, named
/// "symbol, pair, venue"; the arrows move the highlight, wrapping; Enter opens the highlighted row
/// and a click opens the one clicked — each on its OWN venue.
#[test]
fn the_symbol_pickers_rows_pick_by_key_and_by_click() {
    let mut scene = okx();
    scene.matches = vec![
        pick_row("binance", "Binance", "BTCUSDT.P", "BTC/USDT perpetual"),
        pick_row("okx", "OKX", "ETH-USDT-SWAP", "ETH/USDT perpetual"),
    ];
    let first = "BTCUSDT.P, BTC/USDT perpetual, Binance";
    let second = "ETH-USDT-SWAP, ETH/USDT perpetual, OKX";
    let mut h = harness(scene, ThemeAppearance::default(), egui::vec2(700.0, 480.0));
    h.run();
    h.get_by_label_contains("BTC-USDT-SWAP").click();
    h.run();
    assert!(h.query_by_label(first).is_some(), "the first row");
    assert!(h.query_by_label(second).is_some(), "the second row");
    for (key, hot) in
        [(egui::Key::ArrowDown, 1), (egui::Key::ArrowDown, 0), (egui::Key::ArrowUp, 1)]
    {
        h.key_press(key);
        h.run();
        assert_eq!(h.state().state.hot, hot, "{key:?}");
    }
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(
        h.state().emitted,
        [TradeWindowAction::PickSymbol {
            venue: "okx".to_string(),
            symbol: "ETH-USDT-SWAP".to_string()
        }]
    );
    h.state_mut().emitted.clear();
    h.get_by_label_contains("BTC-USDT-SWAP").click();
    h.run();
    h.get_by_label(first).click();
    h.run();
    assert_eq!(
        h.state().emitted,
        [TradeWindowAction::PickSymbol {
            venue: "binance".to_string(),
            symbol: "BTCUSDT.P".to_string()
        }],
        "a row opens ITS venue's own spelling"
    );
}

/// An empty list says what to do, or what was not found.
#[test]
fn an_empty_list_says_what_to_do_or_what_was_not_found() {
    let mut h = harness(okx(), ThemeAppearance::default(), egui::vec2(700.0, 480.0));
    h.run();
    h.get_by_label_contains("BTC-USDT-SWAP").click();
    h.run();
    assert!(h.query_by_label_contains("Type a symbol or a coin").is_some());
    h.state_mut().state.query = "zzz".to_string();
    h.run();
    assert!(h.query_by_label_contains("No symbol matches").is_some());
}

/// Every unconnected venue's button reads "Connect"; its accessible NAME carries the venue, so
/// a screen reader (and a test) can tell one from another without the label beside it (W2
/// review minor 7). The venue is the row's own label, never a name spelled here.
#[test]
fn each_connect_button_names_the_venue_it_connects() {
    let mut scene = okx();
    scene.unconnected = vec![
        TradeWindowAccountPickerConnectRow { venue_label: "Bybit", product: "", symbol: "" },
        TradeWindowAccountPickerConnectRow { venue_label: "Deribit", product: "", symbol: "" },
    ];
    let mut h = harness(scene, ThemeAppearance::default(), egui::vec2(600.0, 420.0));
    open_accounts(&mut h, "OKX · main");
    assert!(h.query_by_label("Connect Bybit").is_some(), "the first venue's button");
    h.get_by_label("Connect Deribit").click();
    h.run();
    assert_eq!(h.state().emitted, [TradeWindowAction::OpenConnections]);
}

/// The bar's widget-id audit (W4 fix round 2's parked minor): the symbol search keeps the
/// trader's focus — every keystroke — while what the bar shows changes under it: the mode chip
/// (a mode becoming unknown and known again), the spread (a book losing a side and getting it
/// back), the FEED badge (the link going unreported and live again), and in the picker itself
/// the Recent row and the matches coming and going. Each change is ONE frame and the next
/// keystroke the frame after it: egui drops the focus of a field whose id did not come back at
/// the end of the frame that moved it, so the keystroke after it is lost (the field asks for
/// the focus again only once it has none).
#[test]
fn a_focused_symbol_search_keeps_every_keystroke_while_the_bar_changes() {
    let mut h = harness(okx(), ThemeAppearance::default(), egui::vec2(600.0, 480.0));
    h.run();
    h.get_by_label_contains("BTC-USDT-SWAP").click();
    h.run();
    h.event(egui::Event::Text("B".to_string()));
    h.run();
    assert_eq!(h.state().state.query, "B", "CONTROL: the search has the focus");
    let eth = pick_row("okx", "OKX", "ETH-USDT-SWAP", "ETH/USDT perpetual");
    let changes = [
        "the mode becomes unknown",
        "the spread goes",
        "the link goes unreported",
        "a recent pick appears",
        "a match appears",
        "the mode is known again",
        "the spread comes back",
        "the link is live again",
        "the recent pick goes",
        "the match goes",
    ];
    let change = |s: &mut Scene, what: &str| match what {
        "the mode becomes unknown" => s.mode = None,
        "the spread goes" => s.spread = false,
        "the link goes unreported" => s.source = None,
        "a recent pick appears" => s.recent = vec![("bybit", "BTCUSDT")],
        "a match appears" => s.matches = vec![eth.clone()],
        "the mode is known again" => s.mode = Some(AccountTier::Paper),
        "the spread comes back" => s.spread = true,
        "the link is live again" => s.source = Some(live_source()),
        "the recent pick goes" => s.recent.clear(),
        _ => s.matches.clear(),
    };
    let mut typed = String::from("B");
    for (what, key) in changes.into_iter().zip("TCUSDTSWAP".chars()) {
        change(&mut h.state_mut().scene, what);
        h.step();
        h.event(egui::Event::Text(key.to_string()));
        h.step();
        h.run();
        typed.push(key);
        assert_eq!(h.state().state.query, typed, "{what}: every keystroke kept");
    }
}

/// The bar's id audit, the account picker's half: its rows are a LIST, and a list changes under
/// a held press (the node's directory reply arrives, an account starts running). A press held
/// on an account's row while a row appears ABOVE it is released on THAT account — never on the
/// row that took its place, which would move the window, and every order sent from it after, to
/// another account. CONTROL: a plain click on the row picks it.
#[test]
fn a_press_on_an_account_row_held_while_a_row_appears_above_it_picks_that_account() {
    let binance = || TradeWindowAction::PickAccount {
        venue: "binance".to_string(),
        account: None,
        symbol: "BTCUSDT".to_string(),
    };
    let mut scene = okx();
    scene.accounts.push(row("binance", "Binance", None, "BTCUSDT", Some(AccountTier::Demo)));
    let mut h = harness(scene, ThemeAppearance::default(), egui::vec2(600.0, 420.0));
    open_accounts(&mut h, "OKX · main");
    h.get_by_label("Binance, main · BTCUSDT, DEMO").click();
    h.run();
    assert_eq!(h.state().emitted, [binance()], "CONTROL: a click picks the row");
    h.state_mut().emitted.clear();
    open_accounts(&mut h, "OKX · main");
    let at = h.get_by_label("Binance, main · BTCUSDT, DEMO").rect().center();
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    h.event(egui::Event::PointerMoved(at));
    h.event(button(true));
    h.step();
    let bybit = row("bybit", "Bybit", None, "BTCUSDT", Some(AccountTier::Paper));
    h.state_mut().scene.accounts.insert(1, bybit);
    h.step();
    h.step();
    let moved = h.get_by_label("Binance, main · BTCUSDT, DEMO").rect().center();
    assert!(moved.y > at.y, "CONTROL: the row moved down under the press: {at:?} -> {moved:?}");
    h.event(button(false));
    h.run();
    assert_eq!(h.state().emitted, [binance()], "the account pressed, and nothing else");
}

/// Press the primary button on the control named `label`, let `change` move the scene under
/// the held button (each a frame of its own), then release where it went down. Answers where
/// `label` was, and where it is just before the release (a pick closes the picker).
fn press_change_release(
    h: &mut Harness<'static, Fixture>,
    label: &str,
    change: impl FnOnce(&mut Scene),
) -> (egui::Pos2, egui::Pos2) {
    let at = h.get_by_label(label).rect().center();
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    h.event(egui::Event::PointerMoved(at));
    h.event(button(true));
    h.step();
    change(&mut h.state_mut().scene);
    h.step();
    h.step();
    let moved = h.get_by_label(label).rect().center();
    h.event(button(false));
    h.run();
    (at, moved)
}

/// The F wave's review, parked minor 6: the symbol picker's half of the id audit. Its Recent
/// row is a LIST, and it re-ranks under a held press (the window moved, so a pick went to the
/// front). A press held on a recent pick while another appears before it is released on THAT
/// pick — never on the one that took its place, which would move the window to another venue
/// or symbol. CONTROLS: a plain click picks it, and the pick did move under the press.
#[test]
fn a_press_on_a_recent_pick_held_while_the_picks_re_rank_picks_that_pick() {
    let eth = || TradeWindowAction::PickSymbol {
        venue: "binance".to_string(),
        symbol: "ETHUSDT".to_string(),
    };
    let mut scene = okx();
    scene.recent = vec![("bybit", "BTCUSDT"), ("binance", "ETHUSDT")];
    let mut h = harness(scene, ThemeAppearance::default(), egui::vec2(600.0, 480.0));
    let open = |h: &mut Harness<'static, Fixture>| {
        h.run();
        h.get_by_label_contains("BTC-USDT-SWAP").click();
        h.run();
    };
    open(&mut h);
    h.get_by_label("ETHUSDT · binance").click();
    h.run();
    assert_eq!(h.state().emitted, [eth()], "CONTROL: a click picks it");
    h.state_mut().emitted.clear();
    open(&mut h);
    let (at, moved) = press_change_release(&mut h, "ETHUSDT · binance", |s| {
        s.recent.insert(0, ("okx", "SOLUSDT"));
    });
    assert_ne!(moved, at, "CONTROL: the pick moved under the press");
    assert_eq!(h.state().emitted, [eth()], "the pick pressed, and nothing else");
}

/// ...and the rows' half: a press held on a row while a row appears ABOVE it (the catalog's
/// answer to what the trader typed arriving) is released on THAT row. The row's key is its venue
/// and symbol, never its place. CONTROLS: a plain click picks it, and the row did move down under
/// the press.
#[test]
fn a_press_on_a_row_held_while_a_row_appears_above_it_picks_that_row() {
    let matching = |venue, label, symbol: &'static str| pick_row(venue, label, symbol, "Coin");
    let eth = || TradeWindowAction::PickSymbol {
        venue: "okx".to_string(),
        symbol: "ETH-USDT-SWAP".to_string(),
    };
    let mut scene = okx();
    scene.matches =
        vec![matching("binance", "Binance", "BTCUSDT.P"), matching("okx", "OKX", "ETH-USDT-SWAP")];
    let row = "ETH-USDT-SWAP, Coin, OKX";
    let mut h = harness(scene, ThemeAppearance::default(), egui::vec2(700.0, 480.0));
    let open = |h: &mut Harness<'static, Fixture>| {
        h.run();
        h.get_by_label_contains("BTC-USDT-SWAP").click();
        h.run();
    };
    open(&mut h);
    h.get_by_label(row).click();
    h.run();
    assert_eq!(h.state().emitted, [eth()], "CONTROL: a click picks it");
    h.state_mut().emitted.clear();
    open(&mut h);
    let (at, moved) = press_change_release(&mut h, row, |s| {
        s.matches.insert(0, matching("bybit", "Bybit", "SOLUSDT.P"));
    });
    assert!(moved.y > at.y, "CONTROL: the row moved down under the press: {at:?} -> {moved:?}");
    assert_eq!(h.state().emitted, [eth()], "the row pressed, and nothing else");
}

/// Half a point: the tolerance for the rounding egui does to a rect's edges.
const EPS: f32 = 0.6;

/// One control's accessible name and where it landed.
struct Control {
    text: String,
    rect: egui::Rect,
}

/// Every button and label the last frame drew.
fn controls(h: &Harness<'_, Fixture>) -> Vec<Control> {
    h.root()
        .children_recursive()
        .filter_map(|n| {
            let node = n.accesskit_node();
            if !matches!(node.role(), Role::Button | Role::Label) {
                return None;
            }
            let text: String = node.label().or_else(|| node.value()).unwrap_or_default();
            (!text.is_empty()).then(|| Control { text, rect: n.rect() })
        })
        .collect()
}

/// The bar fits every width a window gives it, in every look: every control lies inside the band
/// `height` gave it, and none lies on another. Swept over windows 260 to 900 pt wide (the bar
/// is 16 pt narrower: `egui_kittest`'s central panel keeps an 8 pt margin) — from under the
/// ticket-alone window's 280 (W2 review minor 3) — every density and text size, with the widest
/// content the bar holds. The pickers, the mode, the FEED badge and the price are drawn at every
/// width; the spread where it fits (`price_group` gives it up first). Every violation is
/// collected and reported together, so one run says where a threshold is wrong and by how much.
#[test]
fn the_bar_fits_every_width_in_every_look() {
    let always = ["BTC-USDT-SWAP", "Hyperliquid · sub-account-2", "65,432.1"];
    let mut found: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for density in ThemeDensity::ALL {
        for text_size in ThemeTextSize::ALL {
            let look = ThemeAppearance { density, text_size, ..ThemeAppearance::default() };
            for (mode, source) in widest_looks() {
                let mut h = harness(widest(mode, source), look, egui::vec2(900.0, 200.0));
                for w in (260..=900).step_by(10) {
                    h.set_size(egui::vec2(w as f32, 200.0));
                    h.run();
                    let band = h.state().band;
                    let cs = controls(&h);
                    let mut fail = |what: String| {
                        let key = format!("{density:?}/{text_size:?}/{mode:?}: {what}");
                        found.entry(key).or_default().push(w);
                    };
                    let chip = if mode == Some(AccountTier::Live) { "LIVE" } else { MODE_UNKNOWN };
                    // The FEED word is the bar's only in a window narrow enough for three rows.
                    let feed = (h.state().rows == 3).then_some("FEED ");
                    for want in always.iter().chain([&chip]).chain(feed.iter()) {
                        if !cs.iter().any(|c| c.text.starts_with(want)) {
                            fail(format!("{want:?} is not drawn"));
                        }
                    }
                    // W4 fix round 1, M4: the spread gives way only where it does not fit: a
                    // bar with room for everything on one row draws it.
                    if w == 900 && !cs.iter().any(|c| c.text == "Spread") {
                        fail("the spread is not drawn with room to spare".to_string());
                    }
                    for c in &cs {
                        if !band.expand(EPS).contains_rect(c.rect) {
                            fail(format!("{:?} lies outside the bar's band", c.text));
                        }
                    }
                    for (i, a) in cs.iter().enumerate() {
                        for b in &cs[i + 1..] {
                            let o = a.rect.intersect(b.rect);
                            if o.width() > EPS && o.height() > EPS {
                                fail(format!("{:?} lies on {:?}", a.text, b.text));
                            }
                        }
                    }
                }
            }
        }
    }
    let report: Vec<String> =
        found.iter().map(|(what, widths)| format!("{what} at {widths:?}")).collect();
    assert!(report.is_empty(), "{}", report.join("\n"));
}

/// Re-measures what [`trade::TWO_ROWS_BELOW`] and [`trade::THREE_ROWS_BELOW`] are measured from: the widest
/// bar, drawn on one row in a window wide enough for anything, in every look. Run it with
/// `cargo test -p vike-order-entry --lib print_bar_widths -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, not a check: it re-derives the bar's two thresholds"]
fn print_bar_widths() {
    let (mut one_row, mut first_of_two) = (0.0_f32, 0.0_f32);
    for density in ThemeDensity::ALL {
        for text_size in ThemeTextSize::ALL {
            let look = ThemeAppearance { density, text_size, ..ThemeAppearance::default() };
            let t = ThemeTokens::from_appearance(&look);
            for (mode, source) in widest_looks() {
                let mut h = harness(widest(mode, source), look, egui::vec2(2000.0, 200.0));
                h.run();
                let cs = controls(&h);
                let width_of = |prefix: &str| {
                    cs.iter()
                        .find(|c| c.text.starts_with(prefix))
                        .map_or(f32::NAN, |c| c.rect.width())
                };
                let span = |left: bool| {
                    let side: Vec<&Control> =
                        cs.iter().filter(|c| (c.rect.center().x < 1000.0) == left).collect();
                    let min = side.iter().map(|c| c.rect.min.x).fold(f32::MAX, f32::min);
                    let max = side.iter().map(|c| c.rect.max.x).fold(f32::MIN, f32::max);
                    max - min
                };
                let (pickers, prices) = (span(true), span(false));
                let whole = pickers + t.metrics.gap + prices;
                let k = scale(&t);
                one_row = one_row.max(whole / k);
                first_of_two = first_of_two.max(pickers / k);
                println!(
                    "{density:?}/{text_size:?}/{mode:?}: one row {whole:.1} (/{k:.3} = \
                     {:.1}); pickers and chip {pickers:.1} (/{k:.3} = {:.1}); symbol {:.1}, \
                     account {:.1}, chip {:.1}, prices {prices:.1}",
                    whole / k,
                    pickers / k,
                    width_of("BTC-USDT-SWAP"),
                    width_of("Hyperliquid"),
                    width_of(if mode == Some(AccountTier::Live) { "LIVE" } else { "MODE ?" }),
                );
            }
        }
    }
    println!("measured: TWO_ROWS_BELOW >= {one_row:.1}, THREE_ROWS_BELOW >= {first_of_two:.1}");
}
