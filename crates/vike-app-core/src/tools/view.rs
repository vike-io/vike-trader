//! The tools' shared state: the fetched data and the per-window view state.

use super::calendar::CalEvent;
use super::tool_feeds::ToolFeedsLink;
use vike_data::StoreKindId;
use vike_datahub_client::tool_feeds::{DividendRow, EarningsRow, IpoRow, NewsItem};
use vike_marketdata::OptionKind;

/// How a polled feed stands, decided where its status sentence is (never recovered from the
/// sentence): what a view colours the sentence by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FeedHealth {
    /// Not asked yet (or the datahub changed and everything was cleared): no state to colour.
    #[default]
    Unasked,
    /// A held snapshot, fresh, from sources that answered.
    Live,
    /// A held snapshot that is old, or that the server's latest poll reached no source for.
    Stale,
    /// The server's first poll is still in flight; asked again shortly.
    Warming,
    /// Nothing to show and a reason why: a refusal, an unarmed lane, a missing key, no backend, a
    /// dead link, an old server.
    Unavailable,
}

#[derive(Clone, Default)]
pub struct ToolData {
    // News, Calendar, Earnings, Dividends and IPO are the backend datahub's snapshots (`super::tool_feeds`' module
    // doc). Each `*_status` is that feed's ONE sentence: LIVE with its age, or the reason it is not (the
    // server's own words for a refusal), with the last good rows kept beside it.
    pub news: Vec<NewsItem>,
    pub news_status: String,
    /// How the news feed stands, as DATA beside its sentence: the footer's colour reads this and
    /// never the words (`super::tool_feeds`' `judge` sets both).
    pub news_health: FeedHealth,
    pub calendar: Vec<CalEvent>,
    pub cal_status: String,
    pub cal_range: String, // "Jun 29 — Jul 5, 2026"
    // earnings/dividends/IPO counts per ISO date — drives the day-card rows
    pub cal_equity: std::collections::HashMap<String, (usize, usize, usize)>,
    pub cal_earnings: Vec<EarningsRow>, // Finnhub earnings calendar rows (Earnings page)
    pub cal_earnings_status: String,
    pub cal_dividends: Vec<DividendRow>, // FMP dividends calendar rows (Dividends page)
    pub cal_dividends_status: String,
    pub cal_ipos: Vec<IpoRow>, // Nasdaq IPO calendar rows (IPO page)
    pub cal_ipo_status: String,
    /// The UI's half of the News and Calendar poll thread: the interest those windows record and
    /// the Refresh wake. Every clone is the same link.
    pub feeds_link: ToolFeedsLink,
    // Per-underlying options books — the source of truth the Options tool reads, as the backend's
    // datahub last answered them (`super::options`' module doc). `opt_by_underlying[u]` holds the
    // chains of the expiries that were ASKED for and the strip of every expiry the server lists;
    // `opt_underlyings` is the selector order (only those that answered, canonical order).
    pub opt_by_underlying: std::collections::BTreeMap<String, vike_marketdata::UnderlyingChains>,
    pub opt_underlyings: Vec<String>,
    pub opt_status: String,
    /// Every deribit option and future by name, as its instrument payload defines it — what the
    /// Greeks tool looks a name up in (never parse). Built by the options poll thread from the
    /// datahub's deribit instrument list; empty until the first list lands, and a name it does not
    /// hold prices as `unpriced`.
    pub opt_index: vike_options::OptionIndex,
    /// The UI's half of the options poll thread: the picks the Options and Greeks windows record
    /// and the Refresh wake. Every clone is the same link, so the frame's copy of this struct
    /// reaches the thread.
    pub opt_link: super::options::OptionsLink,
}

/// A prefilled options order ticket awaiting the user's Confirm. Built from a chain bid/ask cell
/// click (see `vike_chart::OptionOrderClick`); the order is submitted to the deribit exec engine
/// ONLY when the user clicks Confirm in the modal — NEVER on the chain click that opened it.
/// `side` is vike's `i32` order side (+1 Buy / −1 Sell); `price`/`qty` are editable in the modal.
#[derive(Debug, Clone, PartialEq)]
pub struct OptOrderTicket {
    pub instrument: String,
    pub side: i32,
    pub price: f64,
    pub qty: f64,
    pub kind: OptionKind,
    pub strike: f64,
}

/// Per-window UI state for the tool windows (selection/filters/page/expiry). This is VIEW state,
/// not fetched data, so it lives with the tools — NOT in `workspace::WinState` (which stays free
/// of app content) and not in the shared (fetch-thread-owned) ToolData. The App owns one per tool
/// window, keyed by window id; the window loop takes it out of the map before show_window and
/// writes it back after (the same mutate-around-the-closure pattern the chart windows use).
#[derive(Clone)]
pub struct ToolView {
    // Options
    pub opt_underlying_sel: Option<String>, // None = follow the first fetched underlying (BTC)
    pub opt_expiry_sel: Option<String>,     // None = follow the nearest expiry
    pub opt_strike_window: usize,           // display half-window (±N strike rows around ATM)
    // Options order ticket (Deribit chain-click → confirm → deribit exec). `opt_order_ticket` is
    // the modal's live, editable state (set on a bid/ask cell click, cleared on Confirm/Cancel);
    // `opt_submit` is the OUT slot the App drains AFTER the user clicks Confirm — the only path to
    // an actual order.
    pub opt_order_ticket: Option<OptOrderTicket>,
    pub opt_submit: Option<OptOrderTicket>, // OUT: confirmed order — App builds the OrderRequest
    pub opt_cancel: Option<String>, // OUT: cancel this deribit client_order_id (chain marker click)
    // Tearsheet: the execution history (Fills · Orders · Summary) — its tab, filters, the rows on
    // screen and the load in flight (`crate::ui::tool_views::HistoryView`).
    pub hist: crate::ui::tool_views::HistoryView,
    // News
    pub news_sel: usize,
    pub news_query: String,
    pub news_markets: std::collections::HashSet<String>,
    pub news_categories: std::collections::HashSet<String>,
    pub news_providers: std::collections::HashSet<String>,
    pub news_follow: bool,
    pub news_reader_open: bool,
    pub news_open_url: Option<String>, // OUT: app opens this URL after the frame
    // Calendar
    pub cal_page: u8,         // 0=Economic 1=Earnings 2=Dividends 3=IPO
    pub cal_selected_day: i8, // -1 = none
    pub cal_high_only: bool,
    // Data
    /// Which Data-Manager rail destination this window is showing.
    ///
    /// ⚠ Was `data_subtab: usize` with an index-map comment and an append-only rule, both of which
    /// rested on the claim that the workspace file persisted it. It does not — see
    /// `crates/vike-app-core/src/ui/tool_views/data_rail.rs`'s module doc for the measurement.
    pub data_dest: crate::ui::tool_views::DataDest,
    pub data_sel: Option<String>,    // selected cached-series key
    pub data_delete: Option<String>, // OUT: app drops this feed after the frame
    pub data_log: Vec<String>,       // activity log lines (newest last)
    // Connections
    /// A PENDING one-shot account preselection for the Credentials destination's chip strip, seeded
    /// by `crate::ui::startup::StartupLayout::connections_account` (the `connections-account` QA
    /// capture arm) and consumed — `take`n — by `data.rs`'s `data_body`, on `DataDest::Credentials`.
    ///
    /// ⚠ `take`, not read: the panel's own selection lives in the widget's egui temp memory, so a
    /// value that stayed here would be re-applied every frame and would silently undo an
    /// operator's chip click. `None` is the ordinary state and the whole of it on any launch that
    /// did not ask for a preselection.
    pub connections_account: Option<vike_model::accounts::account_keys::AccountLabel>,
    /// The Backend destination's settings-table row filter (`set · N` / `all · N`). Per window, so
    /// two Data Manager windows can watch different slices of one node's settings.
    pub connections_settings_filter: crate::ui::tool_views::SettingsFilter,
    // Stored (Data Manager "Stored" tab, Task 3) — HistStore inventory tree actions. The tree
    // data itself is shared App state (`App::stored_tree`, like `symbols_catalog`); only this
    // window's transient actions/confirm-modal state lives here.
    pub stored_auto_requested: bool, // one-shot: the first-shown load has already been requested
    pub stored_refresh: bool,        // OUT: (re)run the background inventory load
    /// The last series row clicked in the shared tree: (venue, symbol, kind, interval). Set on
    /// every `StoredSelection` (the tree's single click affordance doubles as "Open" AND as the
    /// target for the Delete control kept app-side, next to Refresh).
    pub stored_last_sel: Option<(String, String, StoreKindId, Option<String>)>,
    /// Pending delete awaiting the confirm modal's click: (venue, symbol, kind, interval).
    pub stored_confirm_delete: Option<(String, String, StoreKindId, Option<String>)>,
    /// OUT: confirmed delete — App reconstructs the `SeriesId` and calls `delete_series`.
    pub stored_delete: Option<(String, String, StoreKindId, Option<String>)>,
    /// OUT: Open-in-chart — (venue, symbol, interval); App spawns/points a chart window at it.
    pub stored_open: Option<(String, String, Option<String>)>,
    /// Data Manager grid v2 (dm-gridv2): persisted state for
    /// `vike_data_manager::stored_catalog_grid` — query, sort,
    /// multi-select, and the active left-nav view, all in one place so the sidebar and grid render
    /// off the identical `active_view`.
    pub stored_grid: vike_data_manager::GridState,

    /// The **Polymarket proxy** box's edit buffer. Seeded once per window-open from the value
    /// actually in force (see `stored_proxy_loaded`), then owned by the operator's typing.
    pub stored_proxy: vike_data_manager::ProxyEdit,

    /// One-shot: has `stored_proxy` been seeded from the resolver yet? Without this the box would
    /// be re-filled from disk every frame and the operator could never type into it.
    pub stored_proxy_loaded: bool,

    /// OUT: the operator clicked Save on the proxy box. Carries the value to write to
    /// `venue.polymarket.socks_proxy`, already in stored form (`none` for a cleared box). `App`
    /// performs the settings-database write and clears this — the view does no I/O, like every
    /// `stored_*` action above it.
    pub stored_proxy_save: Option<String>,
    /// Pending bulk delete awaiting the confirm modal: the grid's `selected` set at the moment
    /// `BulkAction::Delete` fired (snapshotted so a selection change while the modal is open can't
    /// silently widen/narrow what gets deleted).
    pub stored_confirm_bulk_delete: Option<Vec<vike_data_manager::SeriesKey>>,
    /// OUT: confirmed bulk delete — App reconstructs each `SeriesId` and calls `delete_series`,
    /// same per-key path as the single-row `stored_delete`.
    pub stored_bulk_delete: Vec<vike_data_manager::SeriesKey>,
    /// OUT (dm-bulk-backfill): the grid's `selected` set at the moment `BulkAction::Backfill` OR
    /// `BulkAction::Update` fired — both alias to the same MVP path (see
    /// `App::maybe_spawn_stored_backfill`'s doc for why there's no separate "to-now" mode yet).
    /// Unlike bulk delete this needs no confirm modal (non-destructive, network-only), so it's
    /// drained straight into the App's backfill spawn, same deferred-mutation shape as
    /// `stored_bulk_delete`.
    pub stored_backfill: Vec<vike_data_manager::SeriesKey>,
    // DataSets (Symbols tab) — the editor working copy + OUT actions the App applies
    pub ds_sel: Option<String>, // selected DataSet name (None until first load)
    pub ds_name: String,        // form: name
    pub ds_provider: String,    // form: provider
    pub ds_interval: String,    // form: interval
    pub ds_benchmark: String,   // form: benchmark
    pub ds_symbols_text: String, // form: free-text symbols
    pub ds_sym_sel: Option<String>, // "select one to Test" list pick
    pub ds_ai_prompt: String,   // "Ask the AI" prompt
    pub ds_save: bool,          // OUT: persist the working copy
    pub ds_delete: Option<String>, // OUT: delete this DataSet
    pub ds_test: Option<String>, // OUT: open a chart for this symbol
    // Trade window (`crate::ui::tool_views::trade_tool_content`) — the widget's own state, the
    // window's account, the glue's caches, and this frame's OUT intents the shell drains.
    /// The widget's cross-frame view state (`vike_panels::trade::TradeState`).
    pub trade: vike_panels::trade::TradeState,
    /// The window's account label as TEXT, `None` for the venue's default account. The window's
    /// venue and symbol live on its `WinState`. Read through
    /// `crate::orders::order_dispatch::TradeAddress::parse`, and a text that is not a label makes the
    /// window send nothing — never an order on the default account.
    pub trade_account: Option<String>,
    /// OUT: this frame's widget intents. The shell applies the picks and notes and addresses the
    /// rest to the window's venue, account and symbol for the dispatcher.
    pub trade_actions: Vec<vike_panels::trade::TradeAction>,
    /// OUT: the instrument a window with none opens on (its first frame's seed).
    pub trade_pick: Option<crate::ui::tool_views::TradePick>,
    /// OUT: the window size a view-control click asks for.
    pub trade_resize: Option<egui::Vec2>,
    /// What the status strip says: the followed order's progress, or a note or refusal the shell
    /// put there.
    pub trade_status: Option<(vike_panels::trade::StatusKind, String)>,
    /// The client order id the status strip follows; `None` while it shows a note or refusal.
    pub trade_status_coid: Option<String>,
    /// The newest order on the window's address when the window last looked.
    pub trade_seen: Option<crate::ui::tool_views::SeenOrder>,
    /// The recently used instruments, newest first, as `(venue, venue-native symbol)`.
    pub trade_recent: Vec<(String, String)>,
    /// The window's catalog answers, cached per instrument, query and catalog (I13).
    pub trade_catalog: crate::ui::tool_views::TradeCatalog,
    // Account window (`crate::ui::tool_views::account_tool_content`)
    pub account_cancel: Option<String>, // OUT: cancel this client_order_id
    // Polymarket cockpit (WinKind::Polymarket) — per-window widget view state for the four
    // `vike-cockpit` widgets, plus this-frame order intents the App drains onto the command lane
    // (same seam as `trade`/`trade_actions`). The chain rail latches its window selection into
    // `cockpit_chain`; the header is a stateless readout (no state field).
    pub cockpit_chain: vike_cockpit::ChainRailState, // window-chain rail selection
    pub cockpit_ladder: vike_cockpit::ProbLadderState, // probability-ladder hover/selection
    pub cockpit_ticket: vike_cockpit::TicketState,   // arm switch + stake
    /// OUT: probability-ladder click-trade intents (rest-a-limit / cancel) — App maps to Commands.
    pub cockpit_ladder_actions: Vec<vike_cockpit::ProbLadderAction>,
    /// OUT: one-click-ticket intents (BuyUp/BuyDown/size/arm) — App maps the market buys to Commands.
    pub cockpit_ticket_actions: Vec<vike_cockpit::TicketAction>,
    /// The Data Manager Providers/Event plane's "Data providers" group: the admin key as typed this
    /// session, the two key buffers and the one background job (`crate::backend::provider_keys`).
    pub provider_keys: crate::backend::provider_keys::ProviderKeysView,
}

impl Default for ToolView {
    fn default() -> Self {
        Self {
            opt_underlying_sel: None,
            opt_expiry_sel: None,
            opt_strike_window: 12,
            opt_order_ticket: None,
            opt_submit: None,
            opt_cancel: None,
            hist: crate::ui::tool_views::HistoryView::default(),
            news_sel: 0,
            news_query: String::new(),
            news_markets: std::collections::HashSet::new(),
            news_categories: std::collections::HashSet::new(),
            news_providers: std::collections::HashSet::new(),
            news_follow: true,
            news_reader_open: true,
            news_open_url: None,
            cal_page: 0,
            cal_selected_day: -1,
            cal_high_only: false,
            // ⚠ The window opens on BROWSE now, not on an editor. It used to open on index 0, the
            // DataSet editor, because that was the leftmost tab — a position, not a decision.
            // `the_default_destination_is_browse` in `tool_views::data_rail` holds the new one.
            data_dest: crate::ui::tool_views::DataDest::default(),
            data_sel: None,
            data_delete: None,
            data_log: Vec::new(),
            connections_account: None,
            connections_settings_filter: crate::ui::tool_views::SettingsFilter::default(),
            stored_auto_requested: false,
            stored_refresh: false,
            stored_last_sel: None,
            stored_confirm_delete: None,
            stored_delete: None,
            stored_open: None,
            stored_grid: vike_data_manager::GridState::default(),
            stored_proxy: vike_data_manager::ProxyEdit::default(),
            stored_proxy_loaded: false,
            stored_proxy_save: None,
            stored_confirm_bulk_delete: None,
            stored_bulk_delete: Vec::new(),
            stored_backfill: Vec::new(),
            ds_sel: None,
            ds_name: String::new(),
            ds_provider: String::new(),
            ds_interval: String::new(),
            ds_benchmark: String::new(),
            ds_symbols_text: String::new(),
            ds_sym_sel: None,
            ds_ai_prompt: String::new(),
            ds_save: false,
            ds_delete: None,
            ds_test: None,
            trade: vike_panels::trade::TradeState::default(),
            trade_account: None,
            trade_actions: Vec::new(),
            trade_pick: None,
            trade_resize: None,
            trade_status: None,
            trade_status_coid: None,
            trade_seen: None,
            trade_recent: Vec::new(),
            trade_catalog: crate::ui::tool_views::TradeCatalog::default(),
            account_cancel: None,
            cockpit_chain: vike_cockpit::ChainRailState::default(),
            cockpit_ladder: vike_cockpit::ProbLadderState::default(),
            cockpit_ticket: vike_cockpit::TicketState::default(),
            cockpit_ladder_actions: Vec::new(),
            cockpit_ticket_actions: Vec::new(),
            provider_keys: crate::backend::provider_keys::ProviderKeysView::default(),
        }
    }
}
