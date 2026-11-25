//! The tools' per-window view state (the fetched data is the datahub poll's snapshot,
//! `vike_datahub_client::tool_feeds::poll::DatahubPollSnapshot`).
//!
//! The tool windows' DISPLAY half. **Every tool's data comes from the backend's DATAHUB; none opens
//! a socket to a provider or a venue** (decision 0128 for the Options tool, and the tool-feeds
//! record for the rest), and the poll that asks it lives in `vike-datahub-client`
//! (`vike_datahub_client::tool_feeds::poll`: the News and Calendar half and the Options half of the
//! ONE datahub poll, its snapshot, its links and the two threads `spawn_tool_fetchers` starts):
//!   News     → the datahub's `NewsFeed` snapshot (it polls the RSS feeds); the window, its view
//!              state and the topic classifier are `vike-news`'s.
//!   Calendar → the datahub's `EconomicCalendar` (ForexFactory) and `EquityCalendar` (Finnhub
//!              earnings, FMP dividends, Nasdaq IPOs) snapshots; the window, its view state, the
//!              display rows, the week it shows and the currency-to-country map are
//!              `vike-calendar`'s.
//!   Options  → the `OptionsChain` verb for the chains and the deribit `VenueCatalog` listing for
//!              the [`vike_options::OptionIndex`] the Greeks tool looks names up in.
//!              (The Deribit `options_provider` that stood here left with decision 0128: the
//!              provider and both feeds are the datahub's options lane now.)
//! (Data Manager reads the app's own live-feed state, not here.)
//! Also home to [`WindowView`] — the per-window VIEW state for these tools.
//!
//! This module reads NEITHER the environment nor the credential store: the provider keys live on
//! the datahub's box, and the datahub dial arrives as a closure resolved by the app root and
//! handed to the poll's `spawn_tool_fetchers`.

use vike_model::StoreKindId;
use vike_order_entry::option_chain::ticket::OptionChainTicket;

/// Per-window UI state for the tool windows (selection/filters/page/expiry). This is VIEW state,
/// not fetched data, so it lives with the tools — NOT in `window::WindowState` (which stays free
/// of app content) and not in the shared (poll-thread-owned) `DatahubPollSnapshot`. The App owns one per tool
/// window, keyed by window id; the window loop takes it out of the map before show_window and
/// writes it back after (the same mutate-around-the-closure pattern the chart windows use).
#[derive(Clone)]
pub struct WindowView {
    // Options
    pub options_tab: crate::ui::windows::OptionsTab, // which tab the Options window shows
    pub options_underlying_selection: Option<String>, // None = follow the first fetched underlying (BTC)
    pub options_expiry_selection: Option<String>,     // None = follow the nearest expiry
    pub options_strike_window: usize, // display half-window (±N strike rows around ATM)
    // Options order ticket (Deribit chain-click → confirm → deribit exec). `options_order_ticket` is
    // the modal's live, editable state (set on a bid/ask cell click, cleared on Confirm/Cancel);
    // `options_submit` is the OUT slot the App drains AFTER the user clicks Confirm — the only path to
    // an actual order.
    pub options_order_ticket: Option<OptionChainTicket>,
    pub options_submit: Option<OptionChainTicket>, // OUT: confirmed order — App builds the OrderRequest
    pub options_cancel: Option<String>, // OUT: cancel this deribit order_client_id (chain marker click)
    // The Account window's tab and its cancel slot, and its History tab's execution history (Fills ·
    // Order history · Summary): its tab, filters, the rows on screen and the load in flight.
    pub account: vike_account_window::AccountWindowView,
    /// The News window's view state: selection, filters, reader, and the URL the shell opens after the
    /// frame (`vike_news::NewsView`).
    pub news: vike_news::NewsView,
    /// The Calendar window's view state: its page, selected day card, High-only filter and the week
    /// shown (`vike_calendar::CalendarState`).
    pub calendar: vike_calendar::CalendarState,
    // Data
    /// Which Data-Manager rail destination this window is showing.
    ///
    /// ⚠ Was `data_subtab: usize` with an index-map comment and an append-only rule, both of which
    /// rested on the claim that the workspace file persisted it. It does not — see
    /// `crates/vike-app-core/src/ui/windows/data_manager/rail.rs`'s module doc for the measurement.
    pub data_destination: crate::ui::windows::DataManagerDestination,
    pub data_selection: Option<String>, // selected cached-series key
    pub data_delete: Option<String>,    // OUT: app drops this feed after the frame
    pub data_log: Vec<String>,          // activity log lines (newest last)
    // Connections
    /// A PENDING one-shot account preselection for the Gateway destination's chip strip, seeded
    /// by `crate::ui::startup::StartupLayout::gateway_account` (the `gateway-account` QA
    /// capture arm) and consumed — `take`n — by `data/mod.rs`'s `data_body`, on `DataManagerDestination::Gateway`.
    ///
    /// ⚠ `take`, not read: the panel's own selection lives in the widget's egui temp memory, so a
    /// value that stayed here would be re-applied every frame and would silently undo an
    /// operator's chip click. `None` is the ordinary state and the whole of it on any launch that
    /// did not ask for a preselection.
    pub gateway_account: Option<vike_model::accounts::account_keys::AccountLabel>,
    /// This window's credential-store reads, held while a destination that renders them stays on
    /// screen (`DataManagerCredentialReads`'s doc: read on arrival, after this app's own writes
    /// and on the Gateway screen's Refresh, never per frame).
    pub data_credentials: crate::ui::windows::DataManagerCredentialReads,
    /// The Backend destination's settings-table row filter (`set · N` / `all · N`). Per window, so
    /// two Data Manager windows can watch different slices of one node's settings.
    pub connections_settings_filter: crate::ui::windows::BackendSettingsFilter,
    // Stored (Data Manager "Stored" tab, Task 3) — HistoryStore inventory tree actions. The tree
    // data itself is shared App state (`App::stored`, like `symbols_catalog`); only this
    // window's transient actions/confirm-modal state lives here.
    /// The device key epoch the empty tab's automatic load was asked at (`None`: never): once per
    /// window, and again when the key moves, so a tab first shown before the connect made it reloads.
    pub stored_auto_requested: Option<u64>,
    pub stored_refresh: bool, // OUT: (re)run the background inventory load
    /// The last series row clicked in the shared tree, with its whole identity (a grouped and a
    /// per-symbol series can share a label). Set on every `DataManagerInventorySeriesKey` (the tree's single click
    /// affordance doubles as "Open" AND as the target for the Delete control kept app-side, next to
    /// Refresh).
    pub stored_last_selection: Option<vike_data_manager::DataManagerInventorySeriesKey>,
    /// Pending delete awaiting the confirm modal's click: (venue, symbol, kind, interval).
    pub stored_confirm_delete: Option<(String, String, StoreKindId, Option<String>)>,
    /// OUT: confirmed delete — App reconstructs the `SeriesId` and calls `delete_series`.
    pub stored_delete: Option<(String, String, StoreKindId, Option<String>)>,
    /// OUT: Open-in-chart — (venue, symbol, interval); App spawns/points a chart window at it.
    pub stored_open: Option<(String, String, Option<String>)>,
    /// Data Manager grid v2 (dm-gridv2): persisted state for
    /// `vike_data_manager::inventory_grid` — query, sort,
    /// multi-select, and the active left-nav view, all in one place so the sidebar and grid render
    /// off the identical `active_view`.
    pub stored_grid: vike_data_manager::DataManagerInventoryGridState,

    /// The **Polymarket proxy** box's edit buffer. Seeded once per window-open from the value
    /// actually in force (see `stored_proxy_loaded`), then owned by the operator's typing.
    pub stored_proxy: vike_data_manager::DataManagerPolymarketProxyEdit,

    /// One-shot: has `stored_proxy` been seeded from the resolver yet? Without this the box would
    /// be re-filled from disk every frame and the operator could never type into it.
    pub stored_proxy_loaded: bool,

    /// OUT: the operator clicked Save on the proxy box. Carries the value to write to
    /// `venue.polymarket.socks_proxy`, already in stored form (`none` for a cleared box). `App`
    /// performs the settings-database write and clears this — the view does no I/O, like every
    /// `stored_*` action above it.
    pub stored_proxy_save: Option<String>,
    /// Pending bulk delete awaiting the confirm modal: the grid's `selected` set at the moment
    /// `DataManagerInventoryGridBulkAction::Delete` fired (snapshotted so a selection change while the modal is open can't
    /// silently widen/narrow what gets deleted).
    pub stored_confirm_bulk_delete: Option<Vec<vike_data_manager::DataManagerInventorySeriesKey>>,
    /// OUT: confirmed bulk delete — App reconstructs each `SeriesId` and calls `delete_series`,
    /// same per-key path as the single-row `stored_delete`.
    pub stored_bulk_delete: Vec<vike_data_manager::DataManagerInventorySeriesKey>,
    /// OUT (dm-bulk-backfill): the grid's `selected` set at the moment `DataManagerInventoryGridBulkAction::Backfill` OR
    /// `DataManagerInventoryGridBulkAction::Update` fired — both alias to the same MVP path (see
    /// `App::maybe_spawn_stored_backfill`'s doc for why there's no separate "to-now" mode yet).
    /// Unlike bulk delete this needs no confirm modal (non-destructive, network-only), so it's
    /// drained straight into the App's backfill spawn, same deferred-mutation shape as
    /// `stored_bulk_delete`.
    pub stored_backfill: Vec<vike_data_manager::DataManagerInventorySeriesKey>,
    // DataSets (Symbols tab) — the editor working copy + OUT actions the App applies
    pub symbol_set_selection: Option<String>, // selected DataSet name (None until first load)
    pub symbol_set_name: String,              // form: name
    pub symbol_set_provider: String,          // form: provider
    pub symbol_set_interval: String,          // form: interval
    pub symbol_set_benchmark: String,         // form: benchmark
    pub symbol_set_symbols_text: String,      // form: free-text symbols
    pub symbol_set_symbol_selection: Option<String>, // "select one to Test" list pick
    pub symbol_set_ai_prompt: String,         // "Ask the AI" prompt
    pub symbol_set_save: bool,                // OUT: persist the working copy
    pub symbol_set_delete: Option<String>,    // OUT: delete this DataSet
    /// OUT: open a chart on this series: the symbol on the set's provider (none for `Auto`) and interval.
    pub symbol_set_test: Option<crate::ui::startup::SeriesSpec>,
    // Trade window (`crate::ui::windows::trade_tool_content`) — the widget's own state, the
    // window's account, the glue's caches, and this frame's OUT intents the shell drains.
    /// The widget's cross-frame view state (`vike_order_entry::trade_window::TradeWindowState`).
    pub trade: vike_order_entry::trade_window::TradeWindowState,
    /// The window's account label as TEXT, `None` for the venue's default account. The window's
    /// venue and symbol live on its `WindowState`. Read through
    /// `vike_order_entry::trade_window::address::TradeWindowAddress::parse`, and a text that is not a label makes the
    /// window send nothing — never an order on the default account.
    pub trade_account: Option<String>,
    /// The account the window was DRAWN addressing when that is not the one `trade_account` names: a
    /// perpetual at a venue's default account goes to the venue's lane twin while one is up
    /// (`vike_order_entry::dispatch::lane_twin_account`). Set every frame by `trade_tool_content`; read, with
    /// `trade_account`, through `WindowView::trade_account_in_force` by everything that builds the window's
    /// address, so the orders, the lookups and the refusals of one window name one book.
    pub trade_lane: Option<String>,
    /// OUT: this frame's widget intents. The shell applies the picks and notes and addresses the
    /// rest to the window's venue, account and symbol for the dispatcher.
    pub trade_actions: Vec<vike_order_entry::trade_window::TradeWindowAction>,
    /// OUT: the instrument a window with none opens on (its first frame's seed).
    pub trade_pick: Option<crate::ui::windows::TradeWindowPick>,
    /// OUT: the window size a view-control click asks for.
    pub trade_resize: Option<egui::Vec2>,
    /// What the status strip says: the followed order's progress, or a note or refusal the shell
    /// put there.
    pub trade_status: Option<(vike_order_entry::trade_window::TradeWindowStatusLineKind, String)>,
    /// The order client id the status strip follows; `None` while it shows a note or refusal.
    pub trade_status_order_client_id: Option<String>,
    /// The node's NOTE on an order it accepted and sent, as `(order client id, text)`: the status strip
    /// appends it to that order's words while it follows the order (an empty id, a bracket's, binds to the
    /// first order the strip follows). The closed-market note rides here because the strip is rebuilt from
    /// the order every frame, which would wipe a plain strip text within a frame of the order appearing.
    pub trade_note: Option<(String, String)>,
    /// The node's dry run of the order held for a confirm (`preview_held_orders`).
    pub trade_preview: Option<crate::ui::windows::TradeWindowHeldPreview>,
    /// The newest order on the window's address when the window last looked.
    pub trade_seen: Option<crate::ui::windows::TradeWindowSeenOrder>,
    /// The recently used instruments, newest first, as `(venue, venue-native symbol)`.
    pub trade_recent: Vec<(String, String)>,
    /// The window's catalog answers, cached per instrument, query and catalog (I13).
    pub trade_catalog: crate::ui::windows::TradeWindowCatalog,
    // Polymarket cockpit (WindowKind::Polymarket) — per-window widget view state for the four
    // `vike-order-entry` cockpit widgets, plus this-frame order intents the App drains onto the command lane
    // (same seam as `trade`/`trade_actions`). The chain rail latches its window selection into
    // `cockpit_chain`; the header is a stateless readout (no state field).
    /// window-chain rail selection
    pub cockpit_chain: vike_order_entry::cockpit::CockpitChainRailState,
    /// probability-ladder hover/selection
    pub cockpit_ladder: vike_order_entry::cockpit::CockpitProbabilityLadderState,
    /// arm switch + stake
    pub cockpit_ticket: vike_order_entry::cockpit::CockpitTicketState,
    /// OUT: probability-ladder click-trade intents (rest-a-limit / cancel) — App maps to Commands.
    pub cockpit_ladder_actions: Vec<vike_order_entry::cockpit::CockpitProbabilityLadderAction>,
    /// OUT: one-click-ticket intents (BuyUp/BuyDown/size/arm) — App maps the market buys to Commands.
    pub cockpit_ticket_actions: Vec<vike_order_entry::cockpit::CockpitTicketAction>,
    /// Which live window the cockpit follows and its ask of the datahub (`crate::ui::windows::follow_cockpit`).
    pub cockpit_follow: crate::ui::windows::CockpitFollow,
    /// The Data Manager Providers/Event plane's "Data providers" group: the two key buffers and the
    /// one background job (`crate::backend::provider_keys`).
    pub provider_keys: crate::backend::provider_keys::BackendProviderKeysView,
    /// The Data Manager Overview's split warnings: the datahub's open list and the one read in flight
    /// (`crate::data::split_warnings`).
    pub split_warnings: crate::data::split_warnings::DataManagerSplitWarningsView,
    /// The Settings window's AI section: Claude Desktop's last status and the last Connect Claude
    /// click (`crate::ui::claude_connect`).
    pub claude: crate::ui::claude_connect::ClaudeConnectView,
}

impl WindowView {
    /// The account text the Trade window's address is built from: the lane twin it was drawn on
    /// ([`Self::trade_lane`]), else the account the trader picked ([`Self::trade_account`]); `None` is
    /// the venue's default account.
    pub(crate) fn trade_account_in_force(&self) -> Option<&str> {
        self.trade_lane.as_deref().or(self.trade_account.as_deref())
    }
}

impl Default for WindowView {
    fn default() -> Self {
        Self {
            options_tab: crate::ui::windows::OptionsTab::Chain,
            options_underlying_selection: None,
            options_expiry_selection: None,
            options_strike_window: 12,
            options_order_ticket: None,
            options_submit: None,
            options_cancel: None,
            account: vike_account_window::AccountWindowView::default(),
            news: vike_news::NewsView::default(),
            calendar: vike_calendar::CalendarState::default(),
            // ⚠ The window opens on BROWSE now, not on an editor. It used to open on index 0, the
            // DataSet editor, because that was the leftmost tab — a position, not a decision.
            // `the_default_destination_is_browse` in `windows::data_manager::rail` holds the new one.
            data_destination: crate::ui::windows::DataManagerDestination::default(),
            data_selection: None,
            data_delete: None,
            data_log: Vec::new(),
            gateway_account: None,
            data_credentials: crate::ui::windows::DataManagerCredentialReads::default(),
            connections_settings_filter: crate::ui::windows::BackendSettingsFilter::default(),
            stored_auto_requested: None,
            stored_refresh: false,
            stored_last_selection: None,
            stored_confirm_delete: None,
            stored_delete: None,
            stored_open: None,
            stored_grid: vike_data_manager::DataManagerInventoryGridState::default(),
            stored_proxy: vike_data_manager::DataManagerPolymarketProxyEdit::default(),
            stored_proxy_loaded: false,
            stored_proxy_save: None,
            stored_confirm_bulk_delete: None,
            stored_bulk_delete: Vec::new(),
            stored_backfill: Vec::new(),
            symbol_set_selection: None,
            symbol_set_name: String::new(),
            symbol_set_provider: String::new(),
            symbol_set_interval: String::new(),
            symbol_set_benchmark: String::new(),
            symbol_set_symbols_text: String::new(),
            symbol_set_symbol_selection: None,
            symbol_set_ai_prompt: String::new(),
            symbol_set_save: false,
            symbol_set_delete: None,
            symbol_set_test: None,
            trade: vike_order_entry::trade_window::TradeWindowState::default(),
            trade_account: None,
            trade_lane: None,
            trade_actions: Vec::new(),
            trade_pick: None,
            trade_resize: None,
            trade_status: None,
            trade_status_order_client_id: None,
            trade_note: None,
            trade_preview: None,
            trade_seen: None,
            trade_recent: Vec::new(),
            trade_catalog: crate::ui::windows::TradeWindowCatalog::default(),
            cockpit_chain: vike_order_entry::cockpit::CockpitChainRailState::default(),
            cockpit_ladder: vike_order_entry::cockpit::CockpitProbabilityLadderState::default(),
            cockpit_ticket: vike_order_entry::cockpit::CockpitTicketState::default(),
            cockpit_ladder_actions: Vec::new(),
            cockpit_ticket_actions: Vec::new(),
            cockpit_follow: crate::ui::windows::CockpitFollow::default(),
            provider_keys: crate::backend::provider_keys::BackendProviderKeysView::default(),
            split_warnings: crate::data::split_warnings::DataManagerSplitWarningsView::default(),
            claude: crate::ui::claude_connect::ClaudeConnectView::default(),
        }
    }
}
