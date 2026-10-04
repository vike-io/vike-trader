//! Tool-window content backed by REAL data — the egui-only tool BODIES moved down from
//! `vike-app`'s CI-excluded `main.rs` (tool-view extraction, audit F1). Batch 1 brought Tearsheet
//! (command journal), Greeks (Deribit positions × live chains), News (RSS) and Calendar
//! (ForexFactory + Nasdaq); batch 2 added Data (the Data-manager tool: Symbols / Cached Series /
//! provider sub-tabs), Stored (its HistStore-inventory sub-tab), Connections (the credential ×
//! live-status grid) and the chart window's ƒx indicator picker popup; batch 3 added the Trade
//! panel family (the largest single body, now the Account window) plus the DOM and
//! Polymarket-cockpit GLUE — the widgets themselves already live in vike-panels/vike-cockpit, so
//! only vike-app's wiring around them moved. The DOM's glue has since given way to the Trade
//! window's (`trade.rs`, over `vike_panels::trade`). One file per tool; each keeps the data-in /
//! actions-out seam — read-only inputs arrive through [`ToolCtx`], per-window view state + outgoing
//! intents live in the separate `&mut ToolView` param (the `vike_panels::trade::draw` pattern, kept
//! separate to avoid borrow-splitting pain).
//!
//! [`fx_picker_popup`] is the one member that is NOT a tool body — it is the chart window's ƒx
//! popup, grouped here because it is the same "egui-only chunk of `main.rs` that finally gets a
//! CI gate" family and it edits the same `vike_app_core::ui::workspace` state the tools do.
//!
//! Like `workspace/` these render into a passed `egui::Ui` — egui but NOT eframe/wgpu, so they
//! build (and their pure helpers unit-test) on CI, unlike the GUI shell they were extracted from.
//! `vike-desktop`'s `tool_content` dispatcher constructs a [`ToolCtx`] per call and forwards; the
//! one tool that still lives in the shell (Studio) keeps its original body — it stayed because it
//! needed vike-app's `fat` feature when these moved (that feature is deleted; the Studio has been
//! unconditional since split-plane I7). Options was the other holdout: its confirm-ticket modal
//! moved here too (design-system step 7, PR 7b) as [`order_ticket`], onto the component kit — the
//! chain-click prefill and the poll-thread wake channel stay in `vike-desktop`'s `tool_content`,
//! since only it owns the poll thread. (This named `vike-app` as the dispatcher until 2026-09-28.)

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

mod account;
mod backend_settings;
mod calendar;
mod cockpit;
mod connections;
mod data;
pub mod data_rail;
pub mod data_screens;
mod fx_picker;
mod greeks;
pub mod instruments;
mod news;
mod order_ticket;
mod settings;
mod stored;
mod tearsheet;
mod trade;
mod trade_shell;
mod venues;

pub use account::{
    HeroTotals, account_tool_content, hero_totals, is_terminal_status, unpriced_tooltip,
};
pub use backend_settings::{
    BackendDigest, BackendSettingsState, PREDATES_SETTINGS_SHOW, PREDATES_SETTINGS_WRITE,
    SAVED_RESTART_NOTE, SECTION_TITLE, SettingsEditState, SettingsFilter, SettingsWriteRequest,
    UNREAD_VERDICT_POINTER, backend_settings_section, env_shadow, origin_kind, row_is_finding,
    row_is_set, row_read_by_nothing, settings_fetch_state, settings_write_state,
    should_fetch_settings, should_refetch_after_write, start_edit, take_save, write_target,
};
pub use calendar::calendar_tool_content;
pub use cockpit::{
    CockpitCmd, POLY_CHAIN_LOOKAHEAD, POLY_PLACEHOLDER_TOKEN, POLY_STALE_MS, bucket_prob_levels,
    cockpit_tool_content, price_to_cents,
};
pub use connections::{
    BackendPicker, BodyLayout, ConnectionsTab, TabRow, ambient_strip, backend_tab,
    connections_body, connections_tool_content, credentials_line, hairline_segments, status_line,
    strip_reservation, tab_bar, title_bar_tabs,
};
pub use data::{data_tab_reads_arming, data_tool_content};
pub use data_rail::{DataDest, RailCounts, RailGroup};
pub use fx_picker::fx_picker_popup;
pub use greeks::greeks_tool_content;
pub use instruments::{INSTRUMENTS_INTRO, REFRESH_LABEL, instruments_screen, instruments_summary};
pub use news::news_tool_content;
pub use order_ticket::{TicketChoice, order_ticket};
pub use settings::settings_tool_content;
pub use stored::{save_polymarket_proxy, seed_polymarket_proxy_box, stored_tool_content};
pub use tearsheet::tearsheet_tool_content;
pub use trade::{ControlLink, SeenOrder, TradeCatalog, TradePick, trade_tool_content, venue_label};
// The title bar's tests draw the view controls the way the window does; nothing else calls it
// from outside `trade`.
#[cfg(test)]
pub(crate) use trade::title_bar_view_controls;
pub use trade_shell::{
    DIRECTORY_MAX_AGE, DirectorySlot, NOT_SENT_NO_CONTROL, TradeFrame, apply_directory_result,
    directory_due, directory_reply, directory_unavailable, forget_held_of_closed_windows,
    on_backend_switch, refuse_unsent, route_rejects, should_fetch_directory, spawn_directory_fetch,
};
pub use venues::{
    ARM_BUSY_NOTE, ARM_RESTART_NOTE, ArmDirection, ArmEdit, NO_SETTINGS_DIR, OBSERVING_NOTE,
    THIN_BUILD_NOTE, VENUE_ARM_LOCK_BUDGET, VENUES_TAB_LABEL, VenueArmingInputs, apply_arming,
    arm_direction, credentials_cell, effective_cell, reload_venue_ceilings, venues_tab_content,
};

/// The grouped READ-ONLY inputs of the extracted tool bodies (audit F8) — snapshots, fetched tool
/// data, and decoded textures, bundled so the per-tool signatures stop re-threading them one by
/// one. Mutable per-window state stays OUT of this struct on purpose: `&mut ToolView` rides as a
/// separate parameter so a body can hold `ToolCtx` borrows while mutating the view state.
pub struct ToolCtx<'a> {
    /// Per-tool fetched data (the REST fetchers' output: news/calendar/options chains + statuses).
    pub td: &'a crate::tools::ToolData,
    /// The lossy published core snapshot (orders/positions) — Greeks reads positions off it.
    pub snap: &'a vike_core::CoreSnapshot,
    /// Country-flag textures keyed by lowercase iso2 (Calendar's country column).
    pub flags: &'a HashMap<String, egui::TextureHandle>,
    /// News-provider favicon textures keyed by source name (News avatars).
    pub logos: &'a HashMap<String, egui::TextureHandle>,
    /// The command-journal dir the Tearsheet reads — the SAME `VIKE_JOURNAL_DIR` that turns
    /// journaling ON for the producer, so the panel reads exactly what this session records.
    /// Read from the process env by the BINARY (env reads stay in `vike-desktop` — the settings-
    /// registry rule: libraries take configuration as parameters); `None` ⇒ nothing was journaled
    /// and the Tearsheet shows its setup hint.
    pub journal_dir: Option<std::path::PathBuf>,
    /// The live-feed catalog rows the Data tool's "Cached Series" table renders, one per running
    /// bar feed: `(series_key, bar_count, first_open_ms, last_open_ms)` where `series_key` is the
    /// `SYMBOL@interval` string the chart windows subscribe under.
    pub feeds: &'a [(String, usize, i64, i64)],
    /// The DataSet store (symbol universes) — the Data tool's Symbols tab edits a working copy of
    /// one of these, and the Stored sub-tab lists their names as the grid's Watchlists.
    pub dsets: &'a vike_data::datasets::Store,
    /// The app-wide display timezone — the Data tool stamps its activity-log lines with it.
    pub display_tz: vike_chart::DisplayTz,
    /// The local `HistStore` inventory + its load state (the Data tool's Stored sub-tab).
    pub stored: StoredCtx<'a>,
    /// Per-venue live feed-status handles (`venue -> Arc<Mutex<status string>>`), one entry per
    /// already-producing bridge feed. The Connections tool snapshots each and parses it through
    /// `vike_model::feed_status::parse_feed_status`; a venue with no live producer is simply absent and
    /// renders as `Unknown`.
    pub feed_statuses: &'a HashMap<String, Arc<Mutex<String>>>,
    /// WHERE a credential write from a tool body lands, and where it is RECORDED — the resolved
    /// credential store plus the append-only change journal, both derived from the binary's ONE boot
    /// walk (`vike_boot::Booted`).
    ///
    /// ⚠ Not to be confused with [`ToolCtx::journal_dir`] two fields up. That one is the COMMAND
    /// journal (`VIKE_JOURNAL_DIR`, per-order records, read by the Tearsheet); this is the CHANGE
    /// journal (`<project>/settings/state/changes`, per-operator-action records). Different
    /// directory, different rate class, different reader —
    /// `vike_model::change_journal`'s module doc keeps them apart on purpose.
    ///
    /// The Connections editor's Save arm is the consumer today. It is on `ToolCtx` rather than
    /// threaded per body because both halves are `'static` in the binary (a `OnceLock` each), so
    /// this costs the per-frame construction two pointer copies and an already-taken timestamp.
    pub credentials: vike_connections::CredentialWrite<'a>,
    /// The book-backed windows' per-window transport (Trade window + Polymarket cockpit).
    pub book: BookCtx<'a>,
    /// The instrument catalog the Trade window's picker searches and reads grids from — the same
    /// `vike_catalog::Catalog` the chart window's symbol picker searches (`App::symbols_catalog`).
    ///
    /// The `Arc` itself rather than the catalog behind it, because the Trade window caches its
    /// catalog answers (I13) and must tell a refreshed catalog from the one it read: the shell
    /// replaces the `Arc` on every refresh, so its pointer is the catalog's identity
    /// ([`TradeCatalog`]).
    pub symbols: &'a Arc<vike_catalog::Catalog>,
    /// The instrument the most recently used Trade window trades (spec §3.11); `None` before any.
    /// A new Trade window opens on it. The shell writes it from the window the trader last acted
    /// in, never from every window every frame.
    pub last_trade: Option<&'a TradePick>,
    /// The node's answer to "which venues and accounts does your settings database hold"
    /// (`vike_tradehub_client::directory`): venue names and the account list come from here and
    /// from nowhere in code (the owner's rulings of 2026-09-30). `None` before the first reply and
    /// against a node that predates the verb.
    pub directory: Option<&'a vike_tradehub_client::wire::WireDirectory>,
    /// Whether the ACTIVE backend's `Directory` fetch ended with no list (`directory_unavailable`):
    /// the Trade window then says "Account list unavailable." (the owner's decision of 10-03, item 5)
    /// and stays usable on the keys and the accounts the snapshot runs.
    pub directory_unavailable: bool,
    /// Whether this desktop can send an order at all (`ControlLink::of` over the control handle):
    /// a read-only desktop and a lost control link are the Trade window's stated cause up front
    /// (the owner's decision of 10-03, item 11).
    pub control_link: ControlLink,
    /// **The instrument catalog the symbol picker searches, and the per-venue refresh control over
    /// it** — `crate::data::catalog_refresh::CatalogRefresh`, owned by the binary because it holds the
    /// picker's publish channel and the providers only the binary knows it linked.
    ///
    /// A `&` rather than a snapshot: the Instruments screen reads `rows()` (one lock, one small
    /// Vec) and calls `request()` on a click, and a snapshot taken before the draw could not carry
    /// the second half. Every mutation behind it happens on a worker thread.
    pub catalog: &'a crate::data::catalog_refresh::CatalogRefresh,
    /// Resolved Polymarket market SHORT NAMES keyed by token-id, cached by the background Gamma
    /// resolver. The cockpit labels its rail/ladder from this; a token with no entry falls back to
    /// the elided token-id (`poly_labels::poly_short_label`).
    pub poly_names: &'a HashMap<String, String>,
}

/// The per-window read-only transport shared by the two book-backed tool windows — a window is
/// EITHER a Trade window or a cockpit, never both, so they ride ONE set of fields (exactly as
/// `main.rs`'s `tool_content` has always threaded its book params through to whichever arm matched).
pub struct BookCtx<'a> {
    /// The window's instrument: a venue-native symbol for the Trade window, a YES-outcome token-id
    /// for the cockpit. EMPTY on a Trade window's first frame, before its seed is applied.
    pub symbol: &'a str,
    /// The window's venue (`"polymarket"` for the cockpit) — the key for the book lookup, the
    /// order/position projection and routing.
    pub venue: &'a str,
    /// That `(venue, symbol)`'s live L2 book from the shared `data_sink::BookStore`; `None` until
    /// the first snapshot lands (the Trade window then draws the absence, the cockpit stays empty).
    pub book: Option<&'a vike_model::L2Book>,
    /// No book update inside the window's freshness threshold (the shell's `TRADE_STALE_MS` /
    /// [`POLY_STALE_MS`]) — dims the ladder and shows a STALE badge.
    ///
    /// (A `live` flag — "this venue has a credential-gated LIVE execution client" — stood beside it
    /// for the DOM window's title. It is gone: the Trade window reads the account's mode from its
    /// venue block, `vike_core::VenueBlock::mode`, and the cockpit never read it.)
    pub stale: bool,
}

/// The Stored sub-tab's read-only inputs, grouped so [`ToolCtx`] does not grow four sibling fields
/// that only one body reads. All of it is produced by `App::refresh_stored`'s background load and
/// by `App::maybe_spawn_stored_backfill`'s worker — this side only renders it.
pub struct StoredCtx<'a> {
    /// The venue-grouped inventory tree of everything in the local store (`build_tree`'s output).
    pub tree: &'a [vike_data_manager::model::VenueNode],
    /// Per-series coverage gaps, painted as the grid's gap column.
    pub gaps: &'a vike_data_manager::GapMap,
    /// Per-INSTRUMENT cross-kind partial days, painted as the grid's Partial column — the sibling
    /// of `gaps`: that one answers "is this series missing days", this one "does this instrument
    /// have days where some kinds are present and others are not". Empty until the background load
    /// produces a coverage report — in EITHER mode since spec §6-Q2 put `coverage_report` on the
    /// `HistStore` trait with a datahub wire verb behind it. Still empty on a store-less THIN build
    /// and against a datahub older than that verb, where `partials_note` says so visibly.
    pub partials: &'a vike_data_manager::PartialDayMap,
    /// `true` while a background inventory load is in flight (disables Refresh, shows a hint).
    pub loading: bool,
    /// `Some(reason)` → the last load could not READ the store at all, and this is the reason to
    /// SHOW — the store was unreachable, it refused, or it did not answer inside
    /// [`crate::data::stored_load::RPC_DEADLINE`]. `None` → [`Self::tree`] is the store's real answer,
    /// INCLUDING when that answer is "nothing recorded".
    ///
    /// ⚠ **It exists because those two were one picture.** An unreachable datahub and a genuinely
    /// empty store both produce an empty tree, so the grid drew the same thing for both and the
    /// screen could not tell an operator which they were looking at — with the load itself silent
    /// at every log level, there was nowhere else to find out either. Rendering it is
    /// `crates/vike-app-core/src/ui/tool_views/stored.rs`'s `stored_tool_content`'s job, beside the
    /// empty-tree branch it sits under.
    pub load_error: Option<&'a str>,
    /// The bulk Backfill/Update progress line; empty before the first bulk click.
    pub backfill_status: &'a str,
    /// `Some(reason)` → this mode cannot Delete (a REMOTE grid: delete is a local-store
    /// operation) — grays the header Delete AND the grid's bulk Delete, `reason` as hover text.
    /// From [`crate::data::stored_mode::stored_mode`]; `None` in local mode.
    pub delete_unavailable: Option<&'static str>,
    /// `Some(note)` → the Partial column cannot be filled in this mode (a REMOTE grid whose datahub
    /// did not answer the cross-kind coverage verb — an older server, or a read failure) — the note
    /// is RENDERED above the grid, never a silent empty column. From
    /// [`crate::data::stored_mode::stored_mode`], which decides it from the addr AND what the last load
    /// negotiated (`RemoteCoverage`).
    pub partials_note: Option<&'static str>,
    /// What the last load got from the datahub's history-channels read — the By-venue table's
    /// HISTORY column (`crate::data::history_column`). `None` until a load answers it: the column
    /// then says it is not loaded rather than drawing nothing.
    pub history: Option<&'a crate::data::history_column::HistoryLoad>,
}
