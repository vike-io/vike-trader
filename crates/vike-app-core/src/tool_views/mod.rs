//! Tool-window content backed by REAL data — the egui-only tool BODIES moved down from
//! `vike-app`'s CI-excluded `main.rs` (tool-view extraction, audit F1). Batch 1 brought Tearsheet
//! (command journal), Greeks (Deribit positions × live chains), News (RSS) and Calendar
//! (ForexFactory + Nasdaq); batch 2 added Data (the Data-manager tool: Symbols / Cached Series /
//! provider sub-tabs), Stored (its HistStore-inventory sub-tab), Connections (the credential ×
//! live-status grid) and the chart window's ƒx indicator picker popup; batch 3 added the Trade
//! panel family (the largest single body) plus the DOM and Polymarket-cockpit GLUE — the widgets
//! themselves already live in vike-panels/vike-cockpit, so only vike-app's wiring around them
//! moved. One file per tool; each keeps the data-in / actions-out seam — read-only inputs arrive
//! through [`ToolCtx`], per-window view state + outgoing intents live in the separate
//! `&mut ToolView` param (the `vike_panels::dom::draw` pattern, kept separate to avoid
//! borrow-splitting pain).
//!
//! [`fx_picker_popup`] is the one member that is NOT a tool body — it is the chart window's ƒx
//! popup, grouped here because it is the same "egui-only chunk of `main.rs` that finally gets a
//! CI gate" family and it edits the same `vike_app_core::workspace` state the tools do.
//!
//! Like `workspace/` these render into a passed `egui::Ui` — egui but NOT eframe/wgpu, so they
//! build (and their pure helpers unit-test) on CI, unlike the GUI shell they were extracted from.
//! `vike-app`'s `tool_content` dispatcher constructs a [`ToolCtx`] per call and forwards; the two
//! tools that still live in `main.rs` (Options and Studio) keep their original bodies — Studio
//! needs vike-app's `fat` feature, and the Options arm owns a modal + a poll-thread wake channel.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

mod backend_settings;
mod calendar;
mod cockpit;
mod connections;
mod data;
mod dom;
mod fx_picker;
mod greeks;
mod news;
mod stored;
mod tearsheet;
mod trade;
mod venues;

pub use backend_settings::{
    BackendSettingsState, PREDATES_SETTINGS_SHOW, PREDATES_SETTINGS_WRITE, SAVED_RESTART_NOTE,
    SettingsEditState, SettingsWriteRequest, backend_settings_section, can_save,
    settings_fetch_state, settings_write_state, should_fetch_settings, should_refetch_after_write,
    start_edit, take_save,
};
pub use calendar::calendar_tool_content;
pub use cockpit::{
    CockpitCmd, POLY_CHAIN_LOOKAHEAD, POLY_PLACEHOLDER_TOKEN, POLY_STALE_MS, POLY_WINDOW_SECS,
    bucket_prob_levels, cockpit_tool_content, price_to_cents,
};
pub use connections::{BackendPicker, connections_tool_content};
pub use data::{DATA_SUBTAB_VENUES, data_tab_reads_arming, data_tool_content};
pub use dom::dom_tool_content;
pub use fx_picker::fx_picker_popup;
pub use greeks::greeks_tool_content;
pub use news::news_tool_content;
pub use stored::{save_polymarket_proxy, seed_polymarket_proxy_box, stored_tool_content};
pub use tearsheet::tearsheet_tool_content;
pub use trade::{
    HeroTotals, hero_totals, is_terminal_status, split_base_quote, trade_tool_content,
    unpriced_tooltip,
};
pub use venues::{
    ARM_RESTART_NOTE, ArmDirection, ArmEdit, NO_SETTINGS_DIR, OBSERVING_NOTE, THIN_BUILD_NOTE,
    VENUES_TAB_LABEL, VenueArmingInputs, apply_arming, arm_direction, begin_confirm, can_apply,
    credentials_cell, effective_cell, reload_venue_ceilings, take_apply, venues_tab_content,
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
    /// Read from the process env by the BINARY (env reads stay in `vike-app` — the settings-
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
    /// `vike_connections::parse_feed_status`; a venue with no live producer is simply absent and
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
    /// The book-backed windows' per-window transport (DOM ladder + Polymarket cockpit).
    pub book: BookCtx<'a>,
    /// Resolved Polymarket market SHORT NAMES keyed by token-id, cached by the background Gamma
    /// resolver. The cockpit labels its rail/ladder from this; a token with no entry falls back to
    /// the elided token-id (`poly_labels::poly_short_label`).
    pub poly_names: &'a HashMap<String, String>,
}

/// The per-window read-only transport shared by the two book-backed tool windows — a window is
/// EITHER a DOM or a cockpit, never both, so they ride ONE set of fields (exactly as `main.rs`'s
/// `tool_content` has always threaded its `dom_*` params through to whichever arm matched).
pub struct BookCtx<'a> {
    /// The window's instrument: a venue symbol for the DOM, a YES-outcome token-id for the cockpit.
    pub symbol: &'a str,
    /// The window's SELECTED venue (`"binance"`/`"bybit"`/`"okx"` for the DOM, `"polymarket"` for
    /// the cockpit) — the key for the book lookup, the order/position projection and routing.
    pub venue: &'a str,
    /// That `(venue, symbol)`'s live L2 book from the shared `data_sink::BookStore`; `None` until
    /// the first snapshot lands (the DOM then paints a synthetic stand-in, the cockpit stays empty).
    pub book: Option<&'a vike_model::L2Book>,
    /// No book update inside the window's freshness threshold (`DOM_STALE_MS` / [`POLY_STALE_MS`]) —
    /// dims the ladder and shows a STALE badge.
    pub stale: bool,
    /// This venue has a credential-gated LIVE execution client (lights the DOM's ● LIVE badge);
    /// `false` ⇒ the widget renders PAPER.
    pub live: bool,
}

/// The Stored sub-tab's read-only inputs, grouped so [`ToolCtx`] does not grow four sibling fields
/// that only one body reads. All of it is produced by `App::refresh_stored`'s background load and
/// by `App::maybe_spawn_stored_backfill`'s worker — this side only renders it.
pub struct StoredCtx<'a> {
    /// The venue-grouped inventory tree of everything in the local store (`build_tree`'s output).
    pub tree: &'a [crate::inventory::VenueNode],
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
    /// The bulk Backfill/Update progress line; empty before the first bulk click.
    pub backfill_status: &'a str,
    /// `Some(reason)` → this mode cannot Delete (a REMOTE grid: delete is a local-store
    /// operation) — grays the header Delete AND the grid's bulk Delete, `reason` as hover text.
    /// From [`crate::stored_mode::stored_mode`]; `None` in local mode.
    pub delete_unavailable: Option<&'static str>,
    /// `Some(note)` → the Partial column cannot be filled in this mode (a REMOTE grid whose datahub
    /// did not answer the cross-kind coverage verb — an older server, or a read failure) — the note
    /// is RENDERED above the grid, never a silent empty column. From
    /// [`crate::stored_mode::stored_mode`], which decides it from the addr AND what the last load
    /// negotiated (`RemoteCoverage`).
    pub partials_note: Option<&'static str>,
}
