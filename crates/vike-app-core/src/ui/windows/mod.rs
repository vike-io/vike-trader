//! Tool-window content backed by REAL data — the egui-only tool BODIES moved down from
//! `vike-app`'s CI-excluded `main.rs` (tool-view extraction, audit F1). Batch 1 brought Tearsheet
//! (command journal), Greeks (Deribit positions × live chains), News (RSS; its body has since left
//! for `vike-news`, and this module builds its inputs with [`WindowToolContext::news_inputs`]) and Calendar
//! (ForexFactory + Nasdaq; its body has since left for `vike-calendar`, and [`calendar_tool_content`]
//! is the glue that hands it its inputs); batch 2 added Data (the Data-manager tool: Symbols / Cached Series /
//! provider sub-tabs), Stored (its HistoryStore-inventory sub-tab), Connections (the credential ×
//! live-status grid) and the chart window's ƒx indicator picker popup; batch 3 added the Trade
//! panel family (the largest single body, now the Account window) plus the DOM and
//! Polymarket-cockpit GLUE — the widgets themselves already live in vike-order-entry's `cockpit` module, so
//! only vike-app's wiring around them moved. The DOM's glue has since given way to the Trade
//! window's (`trade/mod.rs`, over `vike_order_entry::trade_window`). One file per tool; each keeps the data-in /
//! actions-out seam — read-only inputs arrive through [`WindowToolContext`], per-window view state + outgoing
//! intents live in the separate `&mut WindowView` param (the `vike_order_entry::trade_window::draw` pattern, kept
//! separate to avoid borrow-splitting pain).
//!
//! [`chart_study_picker_popup`] is the one member that is NOT a tool body — it is the chart window's ƒx
//! popup, grouped here because it is the same "egui-only chunk of `main.rs` that finally gets a
//! CI gate" family and it edits the same `vike_app_core::ui::window` state the tools do.
//!
//! Like `window/` these render into a passed `egui::Ui` — egui but NOT eframe/wgpu, so they
//! build (and their pure helpers unit-test) on CI, unlike the GUI shell they were extracted from.
//! `vike-desktop`'s `tool_content` dispatcher constructs a [`WindowToolContext`] per call and forwards; the
//! one tool that still lives in the shell (Studio) keeps its original body — it stayed because it
//! needed vike-app's `fat` feature when these moved (that feature is deleted; the Studio has been
//! unconditional since split-plane I7). Options was the other holdout until 2026-11-17: its
//! confirm-ticket modal moved first (design-system step 7, PR 7b; it is
//! [`vike_order_entry::option_chain::ticket::order_ticket`] since wave 3), then the whole
//! body with the Greeks window as its second tab ([`options_tool_content`]); the poll thread's wake
//! is a clone of `tool_feeds.options_link`, so the body reaches it through [`WindowToolContext`]. (This named
//! `vike-app` as the dispatcher until 2026-09-28.)

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

mod account;
mod backend;
mod calendar;
mod chart_study_picker;
mod cockpit;
pub mod data_manager;
mod options;
mod settings;
mod trade_window;
// The per-window VIEW state every tool body reads and writes (M30: UI state, so it is the windows').
mod view;

use crate::backend::tradehub_control::BackendControlLink;
pub use account::{AccountWindowHistoryContext, account_tool_content};
pub use backend::settings::{
    BACKEND_SETTINGS_SECTION_TITLE, BackendSettingsDigest, BackendSettingsEditState,
    BackendSettingsFilter, BackendSettingsState, BackendSettingsWriteRequest,
    backend_settings_section, env_shadow, settings_fetch_state, settings_write_state,
    should_fetch_settings, should_refetch_after_write,
};
pub use backend::{BackendPickerSources, backend_tab, strip_row};
pub use calendar::{calendar_tool_content, calendar_tool_content_at};
pub use chart_study_picker::chart_study_picker_popup;
pub use cockpit::follow::{CockpitFollow, follow_cockpit};
pub use cockpit::{POLYMARKET_PLACEHOLDER_TOKEN, POLYMARKET_STALE_MS, cockpit_tool_content};
pub use data_manager::instruments::{REFRESH_LABEL, instruments_screen};
pub use data_manager::rail::{DataManagerDestination, DataManagerRailCounts};
pub use data_manager::{
    DataManagerCredentialRead, DataManagerCredentialReads, data_tool_content,
    save_polymarket_proxy, seed_polymarket_proxy_box, stored_tool_content,
};
pub use options::{OptionsTab, options_tool_content};
pub use settings::{CONNECT_CLAUDE, settings_tool_content};
pub(crate) use trade_window::venue_label;
pub use trade_window::{
    TradeWindowCatalog, TradeWindowPick, TradeWindowSeenOrder, trade_tool_content,
};
// The title bar's tests draw the view controls the way the window does; nothing else calls it
// from outside `trade_window`.
pub use trade_window::shell::TradeWindowHeldPreview;
pub(crate) use trade_window::shell::preview_held_orders;
pub use trade_window::shell::{
    TradeWindowAccountDirectory, TradeWindowFrame, account_directory_due, account_directory_reply,
    account_directory_unavailable, forget_held_of_closed_windows, on_backend_switch, refuse_unsent,
    route_node_refusals, route_rejects, spawn_directory_fetch,
};
#[cfg(test)]
pub(crate) use trade_window::title_bar_view_controls;
pub use view::WindowView;

/// The grouped READ-ONLY inputs of the extracted tool bodies (audit F8) — snapshots, fetched tool
/// data, and decoded textures, bundled so the per-tool signatures stop re-threading them one by
/// one. Mutable per-window state stays OUT of this struct on purpose: `&mut WindowView` rides as a
/// separate parameter so a body can hold `WindowToolContext` borrows while mutating the view state.
pub struct WindowToolContext<'a> {
    /// Per-tool fetched data (the REST fetchers' output: news/calendar/options chains + statuses).
    pub tool_feeds: &'a vike_datahub_client::tool_feeds::poll::DatahubPollSnapshot,
    /// The lossy published core snapshot (orders/positions) — Greeks reads positions off it.
    pub snapshot: &'a vike_execution::CoreSnapshot,
    /// Country-flag textures keyed by lowercase iso2 (Calendar's country column).
    pub country_flags: &'a HashMap<String, egui::TextureHandle>,
    /// News-provider favicon textures keyed by source name (News avatars).
    pub logos: &'a HashMap<String, egui::TextureHandle>,
    /// Where the Account window's History tab reads the account's EXECUTION HISTORY: the resolved datahub address and
    /// the shell's dial ([`AccountWindowHistoryContext`]). ⚠ It read a LOCAL
    /// command-journal directory here (`config.journal_dir` of this PC's database) until 2026-10-11,
    /// which a thin client never has; that field is deleted.
    pub execution_history: AccountWindowHistoryContext<'a>,
    /// The live-feed catalog rows the Data tool's "Cached Series" table renders, one per running
    /// bar feed: `(series_key, bar_count, first_open_ms, last_open_ms)` where `series_key` is the
    /// `venue:SYMBOL@interval` string the chart windows subscribe under.
    pub feeds: &'a [(String, usize, i64, i64)],
    /// The DataSet store (symbol universes) — the Data tool's Symbols tab edits a working copy of
    /// one of these, and the Stored sub-tab lists their names as the grid's Watchlists.
    pub symbol_set_store: &'a vike_data::datasets::SymbolSetStore,
    /// The app-wide display timezone — the Data tool stamps its activity-log lines with it.
    pub display_time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone,
    /// The resolved datahub's `HistoryStore` inventory + its load state (the Data tool's Stored sub-tab).
    pub stored: DataManagerInventoryContext<'a>,
    /// Per-venue live feed-status handles (`venue -> Arc<Mutex<DataClientStatus>>`), one entry per
    /// already-producing feed. The Connections tool snapshots each and reads the STATE its producer
    /// set; a venue with no live producer is simply absent and renders as `Unknown`.
    pub feed_statuses: &'a HashMap<String, Arc<Mutex<vike_model::feed_status::DataClientStatus>>>,
    /// WHERE a credential write from a tool body lands, and where it is RECORDED — the resolved
    /// credential store plus the append-only change journal, both derived from the binary's ONE boot
    /// walk (`vike_boot::Booted`).
    ///
    /// ⚠ Not to be confused with the COMMAND journal (`config.journal_dir`, per-order records,
    /// written on the trade hub's box and read by no tool body); this is the CHANGE journal
    /// (`<project>/settings/state/changes`, per-operator-action records). Different directory,
    /// different rate class, different reader —
    /// `vike_change_journal`'s module doc keeps them apart on purpose.
    ///
    /// The Connections editor's Save arm is the consumer today. It is on `WindowToolContext` rather than
    /// threaded per body because both halves are `'static` in the binary (a `OnceLock` each), so
    /// this costs the per-frame construction two pointer copies and an already-taken timestamp.
    pub credentials: vike_account_manager::AccountManagerCredentialWrite<'a>,
    /// The book-backed windows' per-window transport (Trade window + Polymarket cockpit).
    pub market: WindowToolContextMarket<'a>,
    /// The instrument catalog the Trade window's picker searches and reads grids from — the same
    /// `vike_catalog::Catalog` the chart window's symbol picker searches (`App::symbols_catalog`).
    ///
    /// The `Arc` itself rather than the catalog behind it, because the Trade window caches its
    /// catalog answers (I13) and must tell a refreshed catalog from the one it read: the shell
    /// replaces the `Arc` on every refresh, so its pointer is the catalog's identity
    /// ([`TradeWindowCatalog`]).
    pub catalog: &'a Arc<vike_catalog::Catalog>,
    /// The instrument the most recently used Trade window trades (spec §3.11); `None` before any.
    /// A new Trade window opens on it. The shell writes it from the window the trader last acted
    /// in, never from every window every frame.
    pub last_trade: Option<&'a TradeWindowPick>,
    /// The node's answer to "which venues and accounts does your settings database hold"
    /// (`vike_tradehub_client::directory`): venue names and the account list come from here and
    /// from nowhere in code (the owner's rulings of 2026-09-30). `None` before the first reply and
    /// against a node that predates the verb.
    pub directory: Option<&'a vike_tradehub_client::wire::AccountDirectoryWire>,
    /// Whether the ACTIVE backend's `Directory` fetch ended with no list (`account_directory_unavailable`):
    /// the Trade window then says "Account list unavailable." (the owner's decision of 10-03, item 5)
    /// and stays usable on the keys and the accounts the snapshot runs.
    pub directory_unavailable: bool,
    /// What the observe frame says beyond the snapshot (`BackendObserveBridgeHandle::node_words`): the Trade window
    /// adds one calm line to the strip of an account whose key changed under it. A side input, not a
    /// `CoreSnapshot` field (a measured crate's type).
    pub node_words: &'a vike_tradehub_client::node_words::BackendNodeWords,
    /// Whether this desktop can send an order at all (`BackendControlLink::of` over the control handle):
    /// a read-only desktop and a lost control link are the Trade window's stated cause up front
    /// (the owner's decision of 10-03, item 11).
    pub control_link: BackendControlLink,
    /// Whether the connected node carries a post-only order (`command_lift::node_takes_post_only`
    /// over the control handle's own gate): the Trade ticket's Post only is greyed with "the node is
    /// too old" while this is `false`. `false` too with no control channel, where the window takes no
    /// order anyway ([`BackendControlLink`]).
    pub node_takes_post_only: bool,
    /// Why the market-data datahub cannot be reached, in plain words, while it has never answered
    /// (`MarketDataSession::unreachable_why`), else `None`. A venue the datahub does not feed has no status
    /// line to say it, so the Trade window's empty ladder states it as the cause.
    pub datahub_unreachable: Option<&'a str>,
    /// **The instrument catalog the symbol picker searches, and the per-venue refresh control over
    /// it** — `crate::data::catalog_refresh::DatahubCatalogRefresh`, owned by the binary because it holds the
    /// picker's publish channel and the providers only the binary knows it linked.
    ///
    /// A `&` rather than a snapshot: the Instruments screen reads `rows()` (one lock, one small
    /// Vec) and calls `request()` on a click, and a snapshot taken before the draw could not carry
    /// the second half. Every mutation behind it happens on a worker thread.
    pub catalog_refresh: &'a crate::data::catalog_refresh::DatahubCatalogRefresh,
}

impl<'a> WindowToolContext<'a> {
    /// What the News window (`vike_news::news_tool_content`) reads: the poll's news half, the
    /// source icons and what Follow chart follows: the focused chart
    /// ([`crate::ui::window::focused_chart`] over `egui_context`) and the catalog its asset is
    /// read through, as `crates/vike-desktop/src/tool_window.rs`'s `tool_content` hands it over.
    pub fn news_inputs(&self, egui_context: &egui::Context) -> vike_news::NewsInputs<'a> {
        let td = self.tool_feeds;
        vike_news::NewsInputs {
            items: &td.news,
            status: &td.news_status,
            health: td.news_health,
            link: &td.feeds_link,
            logos: self.logos,
            chart: crate::ui::window::focused_chart(egui_context),
            catalog: self.catalog,
        }
    }
}

/// The per-window read-only transport shared by the two book-backed tool windows — a window is
/// EITHER a Trade window or a cockpit, never both, so they ride ONE set of fields (exactly as
/// `main.rs`'s `tool_content` has always threaded its book params through to whichever arm matched).
pub struct WindowToolContextMarket<'a> {
    /// The window's instrument: a venue-native symbol for the Trade window, a YES-outcome token-id
    /// for the cockpit. EMPTY on a Trade window's first frame, before its seed is applied.
    pub symbol: &'a str,
    /// The window's venue (`"polymarket"` for the cockpit) — the key for the book lookup, the
    /// order/position projection and routing.
    pub venue: &'a str,
    /// That `(venue, symbol)`'s live L2 book from the shared `data_sink::L2BookStore`; `None` until
    /// the first snapshot lands (the Trade window then draws the absence, the cockpit stays empty).
    pub book: Option<&'a vike_marketdata::L2Book>,
    /// No book update inside the window's freshness threshold (the shell's `TRADE_STALE_MS` /
    /// [`POLYMARKET_STALE_MS`]) — dims the ladder and shows a STALE badge.
    ///
    /// (A `live` flag — "this venue has a credential-gated LIVE execution client" — stood beside it
    /// for the DOM window's title. It is gone: the Trade window reads the account's mode from its
    /// venue block, `vike_execution::VenueBlock::mode`, and the cockpit never read it.)
    pub stale: bool,
    /// The live trade tape the Trade window's tick chart reads its last two minutes of prints from
    /// ([`crate::data::data_sink::TickStore::recent`], which takes nothing from it); `None` where
    /// there is no store to read (a test, a build with no feed).
    pub trades: Option<&'a crate::data::data_sink::TickStore>,
}

/// The Stored sub-tab's read-only inputs, grouped so [`WindowToolContext`] does not grow four sibling fields
/// that only one body reads. All of it is produced by `App::refresh_stored`'s background load and
/// by `App::maybe_spawn_stored_backfill`'s worker — this side only renders it.
pub struct DataManagerInventoryContext<'a> {
    /// The venue-grouped inventory tree of everything in the resolved datahub's store
    /// (`build_inventory_tree`'s output).
    pub tree: &'a [vike_data_manager::DataManagerInventoryVenue],
    /// Per-series coverage gaps, painted as the grid's gap column.
    pub gaps: &'a vike_data_manager::DataManagerInventoryGapMap,
    /// Per-INSTRUMENT cross-kind partial days, painted as the grid's Partial column — the sibling
    /// of `gaps`: that one answers "is this series missing days", this one "does this instrument
    /// have days where some kinds are present and others are not". Empty until the background load
    /// produces a coverage report — in EITHER mode since spec §6-Q2 put `coverage_report` on the
    /// `HistoryStore` trait with a datahub wire verb behind it. Still empty on a store-less THIN build
    /// and against a datahub older than that verb, where `partials_note` says so visibly.
    pub partials: &'a vike_data_manager::DataManagerInventoryPartialDayMap,
    /// `true` while a background inventory load is in flight (disables Refresh, shows a hint).
    pub loading: bool,
    /// `Some(reason)` → the last load could not READ the store at all, and this is the reason to
    /// SHOW — the store was unreachable, it refused, or it did not answer inside
    /// `crate::data::data_manager::RPC_DEADLINE`. `None` → [`Self::tree`] is the store's real answer,
    /// INCLUDING when that answer is "nothing recorded".
    ///
    /// ⚠ **It exists because those two were one picture.** An unreachable datahub and a genuinely
    /// empty store both produce an empty tree, so the grid drew the same thing for both and the
    /// screen could not tell an operator which they were looking at — with the load itself silent
    /// at every log level, there was nowhere else to find out either. Rendering it is
    /// `crates/vike-app-core/src/ui/windows/data_manager/inventory/mod.rs`'s `stored_tool_content`'s job, beside the
    /// empty-tree branch it sits under.
    pub load_error: Option<&'a str>,
    /// The bulk Backfill/Update progress line; empty before the first bulk click.
    pub backfill_status: &'a str,
    /// `Some(reason)` → this mode cannot Delete — grays the header Delete AND the grid's bulk
    /// Delete, `reason` as hover text. From [`crate::data::data_manager::mode::stored_mode`], which sets
    /// it with a datahub resolved and without one (no delete of its own to send, and no store of its
    /// own).
    pub delete_unavailable: Option<&'static str>,
    /// `Some(note)` → the Partial column cannot be filled in this mode (a REMOTE grid whose datahub
    /// did not answer the cross-kind coverage verb — an older server, or a read failure) — the note
    /// is RENDERED above the grid, never a silent empty column. From
    /// [`crate::data::data_manager::mode::stored_mode`], which decides it from the addr AND what the last load
    /// negotiated (`DatahubCoverage`).
    pub partials_note: Option<&'static str>,
    /// What the last load got from the datahub's history-channels read — the By-venue table's
    /// HISTORY column (`crate::data::history_column`). `None` until a load answers it: the column
    /// then says it is not loaded rather than drawing nothing.
    pub history_channels: Option<&'a crate::data::history_column::HistoryChannelColumnLoad>,
    /// What the running load has not answered yet, and why a pass could not answer: the grid's
    /// `checking…` cells, the counts still being counted, and the causes beside the columns.
    pub progress: &'a crate::data::data_manager::DataManagerInventoryLoadProgress,
}

impl<'a> DataManagerInventoryContext<'a> {
    /// The context a shell builds from the [`crate::data::data_manager::DataManagerInventory`] it holds:
    /// `datahub_address` is the RESOLVED datahub address the mode gates are decided from
    /// ([`crate::data::data_manager::mode::stored_mode`], with what the last load negotiated), and
    /// `backfill_status` the bulk Backfill's line.
    pub fn of(
        catalog: &'a crate::data::data_manager::DataManagerInventory,
        datahub_address: Option<&str>,
        backfill_status: &'a str,
    ) -> Self {
        let mode = crate::data::data_manager::mode::stored_mode(datahub_address, catalog.coverage);
        Self {
            tree: &catalog.tree,
            gaps: &catalog.gaps,
            partials: &catalog.partials,
            loading: catalog.loading,
            load_error: catalog.error.as_deref(),
            backfill_status,
            delete_unavailable: mode.delete_unavailable,
            partials_note: mode.partials_unavailable,
            history_channels: catalog.history.as_ref(),
            progress: &catalog.progress,
        }
    }

    /// The inventory has not landed yet: the load runs and no tree is held. Every count is still
    /// being counted, the stale one included (the tree is all it reads).
    pub fn listing(&self) -> bool {
        self.loading && self.tree.is_empty()
    }

    /// The gap count is not final: the inventory or a gap probe has not answered.
    pub fn gaps_counting(&self) -> bool {
        self.listing() || !self.progress.gaps_pending.is_empty()
    }

    /// The partial-day count is not final: the inventory or the coverage report has not answered.
    pub fn partials_counting(&self) -> bool {
        self.listing() || self.progress.coverage_pending
    }
}
