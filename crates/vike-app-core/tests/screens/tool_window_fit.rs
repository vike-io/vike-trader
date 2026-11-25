//! **No tool window opens with a scrollbar on its typical content.**
//!
//! Every tool window is app-owned from the frame it opens (`window::show_window` pins it at
//! `w.size`, and its body scrolls inside `WindowBodyBounds`' `ScrollArea`), so the size it opens at is the
//! one per-kind table, `crates/vike-app-core/src/ui/window/spawn.rs`'s `tool_window_size`. These
//! tests draw each kind's REAL body — the `windows` function the desktop's tool call site
//! dispatches to, over typical content — the way that call site draws it (the title bar, the
//! `TOOL_BODY_MARGIN` frame, the body inside `WindowBodyBounds::show`), in a 1920 x 1080 screen whose
//! arena is the screen minus the caption strip and the status bar, and read what the body's content
//! covers against the viewport its `ScrollArea` offered it.
//!
//! The typical content, per kind ([`Typical::of`]):
//! - Account, on each of its five tabs: the seventeen accounts and six working orders of the owner's
//!   capture (`account_window_screens`' `snapshot`), eight open positions on five of those accounts, and
//!   in its History tab a page of thirty fills and fifteen orders over two venues.
//! - Options, on each of its two tabs: a BTC chain of 33 strikes around the money with every quote
//!   column filled, eight expiries and three underlyings, at the window's own strike window (the
//!   Chain); four Deribit option legs and a perpetual hedge priced off that chain, under the poll's
//!   status line (Portfolio Greeks).
//! - News: forty headlines from eight providers, the reader open on the first.
//! - Calendar: a week of economic events, seven a weekday and one each weekend day.
//! - Data: the Data Manager's seeded store on its landing screen (`data_manager_screens`' seed).
//! - Settings: the default appearance, on a desktop with a GPU candle path, and a Claude Desktop that
//!   is not connected yet (its AI section's status line and Connect Claude button).
//!
//! Studio is not drawn: its body lives in `vike-studio`, which this crate does not link
//! (`crate::ui::help` takes the Studio's shortcuts as a parameter for the same reason).

use std::collections::{BTreeMap, BTreeSet};

use egui::{Pos2, Rect, Vec2, pos2, vec2};
use vike_account_window::history::AccountWindowHistoryView;
use vike_account_window::history::page::{
    ExecutionHistoryLoad, ExecutionHistoryPage, ExecutionHistoryQuery, ExecutionHistoryServed,
    ExecutionHistoryWindow,
};
use vike_app_core::backend::connection::BackendAction;
use vike_app_core::backend::editor::{BackendEditorState, BackendRegistryUpdate};
use vike_app_core::backend::registry::BackendRegistry;
use vike_app_core::ui::appearance_settings::ThemeAppearanceSession;
use vike_app_core::ui::claude_connect::{ClaudeConnectView, ClaudeStatus};
use vike_app_core::ui::window::{
    TOOL_BODY_MARGIN, WindowKind, WindowState, show_window, tool_title_bar,
};
use vike_app_core::ui::windows::WindowView;
use vike_app_core::ui::windows::{
    BackendPickerSources, BackendSettingsEditState, BackendSettingsState,
    BackendSettingsWriteRequest, OptionsTab, account_tool_content, calendar_tool_content,
    data_tool_content, options_tool_content, settings_tool_content,
};
use vike_calendar::CalendarEconomicImportance;
use vike_data::{ExecFillRow, ExecOrderRow};
use vike_datahub_client::tool_feeds::{CalendarEconomicEvent, NewsItem};
use vike_execution::CoreSnapshot;
use vike_marketdata::{
    OptionChain, OptionChainExpiry, OptionChainSet, OptionChainStrike, OptionKind, OptionQuote,
    OptionUnderlyingKind,
};
use vike_ui_theme::appearance::{self, ThemeAppearance};
use vike_ui_theme::value::{caption, status_bar, workspace};

use crate::tool_context::Owned;

/// The screen: 1920 x 1080 logical points.
pub(crate) fn screen() -> Rect {
    Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 1080.0))
}

/// The window arena on that screen, shaped like the desktop's (`app.desktop`): the screen minus the
/// caption strip at the top and the status bar at the bottom.
pub(crate) fn arena() -> Rect {
    let s = screen();
    Rect::from_min_max(
        pos2(s.min.x, s.min.y + caption::CAPTION_HEIGHT),
        pos2(s.max.x, s.max.y - status_bar::STATUS_BAR_HEIGHT),
    )
}

/// The kinds drawn here: every tool kind but the three that fill (Chart, Trade, Polymarket, which
/// size themselves at their own spawn arms) and Studio (see the module doc).
pub(crate) const DRAWN: [WindowKind; 6] = [
    WindowKind::Account,
    WindowKind::Options,
    WindowKind::News,
    WindowKind::Calendar,
    WindowKind::Data,
    WindowKind::Settings,
];

/// Frames a window is drawn for before it is read: enough for the bundled fonts to land, the
/// opening pin to drain and every body's own layout passes to settle.
pub(crate) const FRAMES: usize = 60;

/// The datahub address the Data Manager and the Account window's History are drawn with: a configured datahub is
/// the typical case. Nothing here dials it.
const DATAHUB: &str = "127.0.0.1:9";

/// What one window looked like after [`FRAMES`] frames.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Settled {
    /// The window's `w.size` after the last frame.
    pub size: Vec2,
    /// The rect egui painted the window at.
    pub painted: Rect,
    /// The rect the body was offered (its `ScrollArea`'s viewport), read before the body drew.
    pub viewport: Rect,
    /// The rect the body's content covers, read after it drew.
    pub content: Rect,
}

impl Settled {
    /// How far the body's content runs past its viewport, on each axis (zero when it fits).
    pub(crate) fn overflow(&self) -> Vec2 {
        (self.content.max - self.viewport.max).max(Vec2::ZERO)
    }
}

/// Draw a `kind` window opened at `size` (cascade slot 0 of the [`arena`]) over `typical` content
/// ([`Typical::of`], or an Options window on its Portfolio Greeks tab) for [`FRAMES`] frames, the
/// way the desktop's tool call site draws it.
pub(crate) fn settle(mut typical: Typical, kind: WindowKind, size: Vec2) -> Settled {
    let context = egui::Context::default();
    appearance::install(&context, &ThemeAppearance::default());
    let mut w = WindowState::tool(
        "fit",
        kind,
        Rect::from_min_size(arena().min + workspace::CASCADE_ORIGIN, size),
    );
    let (mut viewport, mut content) = (Rect::NOTHING, Rect::NOTHING);
    for frame in 0..FRAMES {
        let raw = egui::RawInput {
            screen_rect: Some(screen()),
            time: Some(frame as f64 / 60.0),
            ..Default::default()
        };
        context.begin_pass(raw);
        show_window(&context, &mut w, arena(), |ui, bounds| {
            let (_, drag, _) = tool_title_bar(ui, kind, false);
            egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, |ui| {
                bounds.show(ui, |ui| {
                    viewport = ui.max_rect();
                    typical.draw(kind, ui);
                    content = ui.min_rect();
                });
            });
            drag
        });
        context.end_pass().drop_without_applying_deltas();
    }
    let painted = context.memory(|m| m.area_rect(w.id)).expect("the window was shown");
    Settled { size: w.size, painted, viewport, content }
}

/// One window's typical content and the state its body keeps between frames.
pub(crate) struct Typical {
    pub(crate) owned: Owned,
    pub(crate) tv: WindowView,
    session: ThemeAppearanceSession,
    backends: BackendRegistry,
    grids: vike_account_manager::AccountManagerGrids,
}

impl Typical {
    /// The typical content of a `kind` window (the module doc lists it).
    pub(crate) fn of(kind: WindowKind) -> Self {
        let mut owned = Owned::new(CoreSnapshot::empty("", ""));
        let mut tv = WindowView::default();
        match kind {
            WindowKind::Account => {
                owned = Owned::new(crate::account_window_screens::snapshot());
                open_positions(&mut owned.snapshot);
                tv.account.history = AccountWindowHistoryView::showing(execution_page());
            }
            WindowKind::Options => {
                owned.snapshot.positions = greek_legs();
                owned.tool_feeds.options_by_underlying =
                    BTreeMap::from([("BTC".to_string(), chain_set())]);
                owned.tool_feeds.options_underlyings =
                    ["BTC", "ETH", "SOL"].map(String::from).to_vec();
                owned.tool_feeds.options_index = option_index();
                owned.tool_feeds.options_status = LIVE_STATUS.to_string();
            }
            WindowKind::News => owned.tool_feeds.news = headlines(),
            WindowKind::Calendar => owned.tool_feeds.calendar = week_of_events(),
            // Claude Desktop installed and not connected yet: the status line and its button.
            WindowKind::Settings => {
                tv.claude = ClaudeConnectView::showing(ClaudeStatus::NotConnected);
            }
            WindowKind::Data => {
                owned = Owned::seeded();
                tv = crate::data_manager_screens::window_on(WindowView::default().data_destination);
            }
            _ => {}
        }
        Typical {
            owned,
            tv,
            // A desktop with a GPU path: its Display section is the taller one (the GPU candle
            // hint is two lines, the reason a box is disabled one).
            session: ThemeAppearanceSession::new(ThemeAppearance::default(), None)
                .with_display((true, "Local"), true),
            backends: BackendRegistry::default(),
            grids: crate::data_manager_screens::credentials_grids(),
        }
    }

    /// Draw the `kind` body, as `crates/vike-desktop/src/tool_window.rs`'s `tool_content` does.
    pub(crate) fn draw(&mut self, kind: WindowKind, ui: &mut egui::Ui) {
        let mut context = self.owned.context();
        context.execution_history.datahub_address = Some(DATAHUB);
        let tv = &mut self.tv;
        match kind {
            WindowKind::Account => account_tool_content(ui, &context, tv),
            WindowKind::Options => options_tool_content(ui, &context, tv),
            WindowKind::News => {
                vike_news::news_tool_content(ui, &context.news_inputs(ui.ctx()), &mut tv.news)
            }
            WindowKind::Calendar => calendar_tool_content(ui, &context, tv),
            WindowKind::Data => {
                let picker =
                    BackendPickerSources { backends: &self.backends, active: None, reported: None };
                let mut action: Option<BackendAction> = None;
                let mut editor = BackendEditorState::Closed;
                let mut registry: Option<BackendRegistryUpdate> = None;
                let mut settings_refresh = false;
                let mut settings_edit = BackendSettingsEditState::Idle;
                let mut settings_write: Option<BackendSettingsWriteRequest> = None;
                data_tool_content(
                    ui,
                    &context,
                    tv,
                    &self.grids,
                    &picker,
                    &mut action,
                    &mut editor,
                    &mut registry,
                    &BackendSettingsState::Idle,
                    &mut settings_refresh,
                    &mut settings_edit,
                    &mut settings_write,
                );
            }
            WindowKind::Settings => {
                settings_tool_content(ui, &mut self.session, &mut tv.claude, &Default::default())
            }
            other => panic!("{other:?} has no typical body here"),
        }
    }
}

/// Eight open positions on five of the typical Account window's accounts (blocks of
/// `account_window_screens`' `snapshot`, by index: binance, ibkr, binance PERP, bybit, bybit LIVE).
fn open_positions(snapshot: &mut CoreSnapshot) {
    let held: [(usize, &str, f64, f64); 8] = [
        (0, "BTCUSDT", 0.012, 96_410.5),
        (0, "ETHUSDT", -0.35, 3_412.25),
        (9, "AAPL", 300.0, 228.41),
        (9, "MSFT", -12.0, 415.9),
        (13, "SOLUSDT", 4.0, 242.31),
        (14, "BTCUSDT", -0.004, 96_402.0),
        (14, "DOGEUSDT", 1_250.0, 0.3812),
        (15, "BTCUSDT", 0.001, 96_398.5),
    ];
    for (at, symbol, size, average_price) in held {
        let venue = snapshot.portfolio.venues[at].venue.clone();
        snapshot.portfolio.venues[at].positions.push(vike_execution::PositionView {
            average_price,
            unrealized: -1.2792 * size.signum(),
            ..vike_execution::testing::position_view(&venue, symbol, size)
        });
    }
}

/// The expiry every option of the fixture shares.
const EXPIRY: &str = "2027-03-26";

/// The options poll's status line over a live datahub (`crate::tools`' `live_line` spelling).
pub(crate) const LIVE_STATUS: &str = "LIVE · Deribit via the datahub at 127.0.0.1:9";

/// A BTC bundle: eight expiries on the strip, the first one's chain loaded (a bundle is partial).
pub(crate) fn chain_set() -> OptionChainSet {
    let labels = ["26 Mar", "02 Apr", "09 Apr", "30 Apr", "28 May", "25 Jun", "24 Sep", "31 Dec"];
    let dates = [
        EXPIRY,
        "2027-04-02",
        "2027-04-09",
        "2027-04-30",
        "2027-05-28",
        "2027-06-25",
        "2027-09-24",
        "2027-12-31",
    ];
    let expiries: Vec<OptionChainExpiry> = labels
        .iter()
        .zip(dates)
        .zip([4_i64, 11, 18, 39, 67, 95, 186, 284])
        .map(|((label, date), dte)| OptionChainExpiry {
            date: date.to_string(),
            dte,
            label: label.to_string(),
        })
        .collect();
    let spot = 96_412.35;
    let rows = (0..33_u32)
        .map(|i| {
            let strike = 80_000.0 + 1_000.0 * f64::from(i);
            let moneyness = (strike - spot) / spot;
            let side = |kind: OptionKind, intrinsic: f64| {
                let extrinsic = 2_400.0 * (-12.0 * moneyness * moneyness).exp();
                let mark = intrinsic.max(0.0) + extrinsic;
                let right = if kind == OptionKind::Call { 'C' } else { 'P' };
                let delta = 0.5 - 4.0 * moneyness;
                Some(OptionQuote {
                    bid: Some(mark * 0.985),
                    ask: Some(mark * 1.015),
                    last: Some(mark),
                    mark: Some(mark),
                    iv: Some(0.48 + 0.6 * moneyness * moneyness),
                    open_interest: Some(1_250.0 + 37.0 * f64::from(i)),
                    volume: Some(310.5 + 11.0 * f64::from(i)),
                    delta: Some(if kind == OptionKind::Call {
                        delta.clamp(0.01, 0.99)
                    } else {
                        (delta - 1.0).clamp(-0.99, -0.01)
                    }),
                    gamma: Some(0.000_021),
                    theta: Some(-61.25),
                    vega: Some(118.4),
                    in_the_money: Some(intrinsic > 0.0),
                    instrument_name: Some(format!("BTC-26MAR27-{strike}-{right}")),
                    ..OptionQuote::new(strike, kind)
                })
            };
            OptionChainStrike {
                strike,
                call: side(OptionKind::Call, spot - strike),
                put: side(OptionKind::Put, strike - spot),
            }
        })
        .collect();
    let chain = OptionChain {
        underlying: "BTC".to_string(),
        underlying_kind: OptionUnderlyingKind::Crypto,
        underlying_price: Some(spot),
        expiry: expiries[0].clone(),
        asof_ms: vike_model::now_ms(),
        source: "deribit".to_string(),
        rows,
    };
    OptionChainSet {
        default_expiry: EXPIRY.to_string(),
        chains: BTreeMap::from([(EXPIRY.to_string(), chain)]),
        expiries,
    }
}

/// The Portfolio Greeks tab's legs: four options on the loaded chain and a perpetual hedge.
pub(crate) fn greek_legs() -> Vec<vike_execution::PositionView> {
    [
        ("BTC-26MAR27-96000-C", 1.5, 2_431.0),
        ("BTC-26MAR27-90000-P", -2.0, 905.5),
        ("BTC-26MAR27-104000-C", -1.0, 640.25),
        ("BTC-26MAR27-100000-P", 0.5, 4_210.0),
        ("BTC-PERPETUAL", -25_000.0, 96_180.0),
    ]
    .into_iter()
    .map(|(name, size, average_price)| vike_execution::PositionView {
        average_price,
        ..vike_execution::testing::position_view("deribit", name, size)
    })
    .collect()
}

/// The option index the Portfolio Greeks legs are looked up in (built as the catalog builds a Deribit option).
pub(crate) fn option_index() -> vike_options::OptionIndex {
    use vike_model::{
        Contract, ContractOption, ContractOptionExerciseStyle, Instrument, Margining,
    };
    let option = |name: &str, strike: f64, right: OptionKind| {
        Instrument::new(
            "deribit",
            name,
            "BTC",
            "BTC",
            Contract::Option(ContractOption {
                right,
                strike,
                expiry_ms: vike_options::expiry_ms(EXPIRY),
                style: ContractOptionExerciseStyle::European,
                margining: Margining::Inverse,
                settle: "BTC".to_string(),
                value_currency: Some("BTC".to_string()),
                underlying: Some("BTC".to_string()),
            }),
        )
    };
    let perpetual = Instrument::new(
        "deribit",
        "BTC-PERPETUAL",
        "BTC",
        "USD",
        Contract::Perp(vike_model::ContractPerp {
            margining: Margining::Inverse,
            settle: "BTC".to_string(),
            value_currency: Some("USD".to_string()),
            funding_interval_ms: None,
            settling_ms: None,
        }),
    );
    vike_options::OptionIndex::from_instruments(&[
        option("BTC-26MAR27-96000-C", 96_000.0, OptionKind::Call),
        option("BTC-26MAR27-90000-P", 90_000.0, OptionKind::Put),
        option("BTC-26MAR27-104000-C", 104_000.0, OptionKind::Call),
        option("BTC-26MAR27-100000-P", 100_000.0, OptionKind::Put),
        perpetual,
    ])
}

/// Forty headlines from eight providers, newest first, the lengths a real feed carries.
fn headlines() -> Vec<NewsItem> {
    let rows: [(&str, &str, &str); 10] = [
        (
            "CoinDesk",
            "crypto",
            "Bitcoin climbs back above $96,000 as spot ETF inflows resume for a third straight session",
        ),
        (
            "Reuters",
            "global",
            "Fed's Waller says another quarter-point cut in December remains on the table if inflation keeps cooling",
        ),
        ("FXStreet", "forex", "EUR/USD holds near 1.0850 ahead of German ZEW sentiment survey"),
        (
            "The Block",
            "crypto",
            "Ethereum developers set a tentative mainnet date for the next network upgrade",
        ),
        (
            "MarketWatch",
            "stocks",
            "Nvidia shares slip in premarket trading ahead of earnings on Wednesday",
        ),
        (
            "Cointelegraph",
            "crypto",
            "Solana DEX volumes hit a monthly high as memecoin trading returns",
        ),
        (
            "Bloomberg",
            "global",
            "Oil steadies after the biggest weekly drop in a month as traders weigh OPEC+ supply plans and Chinese demand",
        ),
        (
            "FXStreet",
            "forex",
            "USD/JPY: Yen weakens past 155 as the BoJ signals patience on rate hikes",
        ),
        ("Decrypt", "crypto", "Stablecoin supply reaches a record as exchange reserves fall"),
        (
            "MarketWatch",
            "stocks",
            "S&P 500 futures edge higher as Treasury yields ease from two-week highs",
        ),
    ];
    let now = vike_model::now_ms();
    (0..40_usize)
        .map(|i| {
            let (source, market, title) = rows[i % rows.len()];
            NewsItem {
                source: source.to_string(),
                title: title.to_string(),
                summary: format!(
                    "{title}. Traders and analysts said the move reflected positioning ahead of the \
                     week's data releases, with liquidity thinner than usual and several desks \
                     reporting wider spreads into the close."
                ),
                url: format!("https://news.example/{i}"),
                market: market.to_string(),
                tags: vec!["Markets".to_string()],
                timestamp_ms: now - 7 * 60_000 * i as i64,
            }
        })
        .collect()
}

/// A week of economic events in the viewer's local week (the day strip is that week): seven each
/// weekday and one each weekend day. Each row states only its instant: the Calendar derives the
/// day, the clock time and the week's range label at draw time (`CalendarWeek`).
fn week_of_events() -> Vec<CalendarEconomicEvent> {
    use chrono::{Datelike, Duration, Local, TimeZone};
    let rows: [(&str, &str, CalendarEconomicImportance, &str, &str, &str); 7] = [
        (
            "USD",
            "08:30",
            CalendarEconomicImportance::High,
            "Non-Farm Employment Change",
            "254K",
            "140K",
        ),
        (
            "EUR",
            "05:00",
            CalendarEconomicImportance::Medium,
            "German ZEW Economic Sentiment",
            "13.1",
            "10.2",
        ),
        ("GBP", "02:00", CalendarEconomicImportance::High, "CPI y/y", "2.3%", "2.2%"),
        (
            "JPY",
            "23:50",
            CalendarEconomicImportance::Other,
            "Core Machinery Orders m/m",
            "-1.9%",
            "0.4%",
        ),
        ("USD", "10:00", CalendarEconomicImportance::Medium, "ISM Services PMI", "52.1", "51.7"),
        ("AUD", "20:30", CalendarEconomicImportance::High, "Employment Change", "35.6K", "25.2K"),
        ("CAD", "08:30", CalendarEconomicImportance::Medium, "Retail Sales m/m", "0.4%", "0.3%"),
    ];
    let today = Local::now().date_naive();
    let monday = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
    let mut events = Vec::new();
    for day in 0..7_i64 {
        let date = monday + Duration::days(day);
        let per_day = if day < 5 { rows.len() } else { 1 };
        for (currency, time, importance, title, actual, forecast) in rows.iter().take(per_day) {
            let at =
                date.and_time(chrono::NaiveTime::parse_from_str(time, "%H:%M").expect("a time"));
            let timestamp_ms =
                Local.from_local_datetime(&at).earliest().map_or(0, |d| d.timestamp_millis());
            // The RAW row the poll holds; the window derives the country, flag and importance.
            events.push(CalendarEconomicEvent {
                timestamp_ms,
                currency: (*currency).to_string(),
                title: (*title).to_string(),
                impact: format!("{importance:?}"),
                forecast: (*forecast).to_string(),
                previous: (*forecast).to_string(),
                actual: (*actual).to_string(),
            });
        }
    }
    events
}

/// A loaded page of the execution history: thirty fills and fifteen orders over two venues.
fn execution_page() -> ExecutionHistoryLoad {
    let now = vike_model::now_ms();
    let markets = [
        ("binance", "BTCUSDT", 96_400.0),
        ("binance", "ETHUSDT", 3_412.5),
        ("bybit", "SOLUSDT", 242.31),
    ];
    let fills = (0..30_usize)
        .map(|i| {
            let (venue, symbol, price) = markets[i % markets.len()];
            ExecFillRow {
                ts: now - 9 * 60_000 * i as i64,
                trade_id: (4_812_330_117_u64 + i as u64).to_string(),
                order_client_id: format!("vike-20261116-{i:04}-{venue}"),
                venue: venue.to_string(),
                symbol: symbol.to_string(),
                side: if i % 2 == 0 { 1 } else { -1 },
                qty: 0.012 * (1 + i % 4) as f64,
                px: price * (1.0 + 0.0004 * i as f64),
                commission: 0.0386,
                mark_price: None,
                liquidity_side: if i % 3 == 0 { "maker" } else { "taker" }.to_string(),
                commission_asset: "USDT".to_string(),
                venue_order_id: Some((38_114_551_902_u64 + i as u64).to_string()),
                account: None,
                position_side: None,
                corrects_trade_id: None,
                correction_seq: None,
                mount_id: None,
                received_ms: None,
                ownership: None,
                journal_sequence: None,
                journal_generation: None,
            }
        })
        .collect();
    let orders = (0..15_usize)
        .map(|i| {
            let (venue, symbol, price) = markets[i % markets.len()];
            ExecOrderRow {
                ts: now - 17 * 60_000 * i as i64,
                order_client_id: format!("vike-20261116-{i:04}-{venue}"),
                venue: venue.to_string(),
                symbol: symbol.to_string(),
                side: if i % 2 == 0 { 1 } else { -1 },
                qty: 0.012 * (1 + i % 4) as f64,
                order_type: if i % 2 == 0 { "limit" } else { "market" }.to_string(),
                status: if i % 4 == 0 { "canceled" } else { "filled" }.to_string(),
                price: Some(price),
                trigger_price: None,
                venue_order_id: Some((38_114_551_902_u64 + i as u64).to_string()),
                filled_qty: 0.012 * (1 + i % 4) as f64,
                avg_fill_px: price,
                account: None,
                mount_id: None,
                ownership: None,
                journal_sequence: None,
                journal_generation: None,
                reason: None,
            }
        })
        .collect();
    let venues = BTreeMap::from([
        ("binance".to_string(), BTreeSet::from(["BTCUSDT".to_string(), "ETHUSDT".to_string()])),
        ("bybit".to_string(), BTreeSet::from(["SOLUSDT".to_string()])),
    ]);
    ExecutionHistoryLoad {
        query: ExecutionHistoryQuery {
            venue: None,
            symbol: None,
            window: ExecutionHistoryWindow::Week,
            now_ms: now,
            older_than: None,
            generation: 1,
        },
        outcome: Ok(ExecutionHistoryPage {
            fills,
            orders,
            served: ExecutionHistoryServed { fills_range: true, orders: true, recording: true },
            venues,
            ..ExecutionHistoryPage::default()
        }),
    }
}

/// **The News window's list and reader SHARE its width**: with the reader open on a headline, at the
/// default size, at a narrow one, and at one too narrow for both panes (the reader then takes the
/// whole width), nothing in the body is laid out past the window's inner rect. A toolbar that ran
/// past the edge widened every row under it, and a reader floored at its least width was drawn past
/// the window's edge, its headline and buttons cut off.
#[test]
fn the_news_list_and_reader_never_run_past_the_window_edge() {
    let mut failures = Vec::new();
    for width in [400.0, 600.0, workspace::NEWS_WINDOW_SIZE.x] {
        let s = settle(
            Typical::of(WindowKind::News),
            WindowKind::News,
            vec2(width, workspace::NEWS_WINDOW_SIZE.y),
        );
        let over = s.overflow();
        println!(
            "NEWS at {width}: viewport {:?}, content {:?}, overflow {over:?}",
            s.viewport, s.content
        );
        if over.x > 0.5 {
            failures.push(format!("News at {width} wide runs {:.1} past its edge", over.x));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// **Every tool window drawn here opens with its typical content inside it**: after [`FRAMES`]
/// frames at `tool_window_size(kind)`, the body's content runs past its viewport on neither axis —
/// so the body's `ScrollArea` shows no bar. News is a long list by nature (forty headlines): it scrolls
/// its rows in a list of its own, so the window's body must still not overflow, and the same assertion
/// holds for it. The Options window is read on both of its tabs, the Account window on all five.
///
/// ⚠ **The Account window's History tab SCROLLS, down only** (the coordinator's ruling, top layers wave
/// 3): under the equity header thirty fills are taller than any size the window may open at, so the
/// body scrolls and the lag caption is reached by scrolling, never clipped by a box of its own. It must
/// still fit across: its columns are what set the window's width.
#[test]
fn every_tool_window_opens_with_its_typical_content_inside_it() {
    use vike_account_window::AccountWindowTab;
    let mut failures = Vec::new();
    let mut greeks = Typical::of(WindowKind::Options);
    greeks.tv.options_tab = OptionsTab::PortfolioGreeks;
    let account_tabs = [
        AccountWindowTab::Positions,
        AccountWindowTab::Orders,
        AccountWindowTab::Strategies,
        AccountWindowTab::History,
    ]
    .map(|tab| {
        let mut typical = Typical::of(WindowKind::Account);
        typical.tv.account.tab = tab;
        (WindowKind::Account, format!("Account/{tab:?}"), typical)
    });
    let cases = DRAWN
        .map(|kind| (kind, format!("{kind:?}"), Typical::of(kind)))
        .into_iter()
        .chain([(WindowKind::Options, "Options/PortfolioGreeks".to_string(), greeks)])
        .chain(account_tabs);
    for (kind, name, typical) in cases {
        let size = vike_app_core::ui::window::spawn::tool_window_size(kind);
        let s = settle(typical, kind, size);
        let over = s.overflow();
        println!(
            "FIT {name}: opened {size:?}, painted {:?}, viewport {:?}, content {:?}, overflow {over:?}",
            s.painted.size(),
            s.viewport.size(),
            s.content.size()
        );
        assert_eq!(s.size, size, "{name}: the window keeps the size it opened at");
        let scrolls_down = name == "Account/History";
        if over.x > 0.5 || (over.y > 0.5 && !scrolls_down) {
            failures.push(format!("{name} at {size:?}: the body overflows by {over:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
