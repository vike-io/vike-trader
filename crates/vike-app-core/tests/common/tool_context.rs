//! The ONE owner of everything a `WindowToolContext` borrows, for this crate's window suites
//! (`account_window_screens`, `calendar_window_screens`, `data_manager_screens`,
//! `data_manager_progressive`, `trade_cause_line`,
//! `trade_window_screens`),
//! plus the widths and pixel densities they read a window at. Each of them carried its own copy of
//! `Owned` and its `context`; the copies differed only in which fields a window reads, so there is one
//! now: [`Owned::new`] is the quiet default and a suite overrides the fields its window reads.
//!
//! The `screens` binary (`tests/screens.rs`) declares it ONCE, `#[path = "common/tool_context.rs"] mod
//! tool_context;`, and each suite reads it as `crate::tool_context`. A `tests/` SUBDIRECTORY file is not a
//! test target, so this builds no binary of its own. `allow(dead_code)`: a suite uses a subset (the
//! Data Manager suite reads no width).
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use vike_app_core::backend::tradehub_control::BackendControlLink;
use vike_app_core::data::catalog_refresh::DatahubCatalogRefresh;
use vike_app_core::data::data_manager::DataManagerInventoryLoadProgress;
use vike_app_core::data::history_column::HistoryChannelColumnLoad;
use vike_app_core::ui::windows::{
    AccountWindowHistoryContext, DataManagerInventoryContext, WindowToolContext,
    WindowToolContextMarket,
};
use vike_change_journal::ChangeProcess;
use vike_datahub_client::tool_feeds::poll::DatahubPollSnapshot;
use vike_execution::CoreSnapshot;

/// The three logical widths and two pixel densities every window suite reads. 2560 is the capture's
/// width; 640 is a small window; 1200 the common one.
pub const WIDTHS: [f32; 3] = [640.0, 1200.0, 2560.0];
pub const DENSITIES: [f32; 2] = [1.0, 2.0];

/// `f(width, ppp)` at every width and density, widths outer.
pub fn at_every_size<T>(mut f: impl FnMut(f32, f32) -> T) -> Vec<T> {
    let mut out = Vec::new();
    for w in WIDTHS {
        for d in DENSITIES {
            out.push(f(w, d));
        }
    }
    out
}

/// Everything `WindowToolContext` borrows, owned, so a pass can build one.
pub struct Owned {
    pub tool_feeds: DatahubPollSnapshot,
    pub snapshot: CoreSnapshot,
    pub textures: HashMap<String, egui::TextureHandle>,
    pub symbol_set_store: vike_data::datasets::SymbolSetStore,
    pub tree: Vec<vike_data_manager::DataManagerInventoryVenue>,
    pub gaps: vike_data_manager::DataManagerInventoryGapMap,
    pub partials: vike_data_manager::DataManagerInventoryPartialDayMap,
    /// What a running Stored load has not answered yet: nothing, for a window over a settled store.
    pub progress: DataManagerInventoryLoadProgress,
    pub feeds: Vec<(String, usize, i64, i64)>,
    pub statuses: HashMap<String, Arc<Mutex<vike_model::feed_status::DataClientStatus>>>,
    pub process: ChangeProcess,
    pub catalog_refresh: DatahubCatalogRefresh,
    pub catalog: Arc<vike_catalog::Catalog>,
    /// The By-venue table's HISTORY column answer; `None` for a window that reads none.
    pub history: Option<HistoryChannelColumnLoad>,
    pub control: BackendControlLink,
    /// The window's `(venue, symbol)`; both empty is a window with no instrument (a Trade window's
    /// first frame, or a window that has none).
    pub window: (&'static str, &'static str),
    /// Whether the window's book reads as stale. The book itself is always `None`, as for a venue
    /// with no feed (the shell reads it from the `L2BookStore`, which nothing has written).
    pub stale: bool,
    /// What the shell hands the window as `datahub_unreachable`: the session's own answer.
    pub unreachable: Option<String>,
}

impl Owned {
    /// A window over `snapshot` with nothing else in it: no store, no feed, no status line, no book, an
    /// open control link and a reachable (or unconfigured) datahub.
    pub fn new(snapshot: CoreSnapshot) -> Self {
        // The catalog handle does no I/O at construction, and nothing here ever sends.
        let (tx, _rx) = std::sync::mpsc::channel();
        Owned {
            tool_feeds: DatahubPollSnapshot::default(),
            snapshot,
            textures: HashMap::new(),
            symbol_set_store: vike_data::datasets::SymbolSetStore::default(),
            tree: Vec::new(),
            gaps: vike_data_manager::DataManagerInventoryGapMap::default(),
            partials: vike_data_manager::DataManagerInventoryPartialDayMap::default(),
            progress: DataManagerInventoryLoadProgress::default(),
            feeds: Vec::new(),
            statuses: HashMap::new(),
            process: ChangeProcess::new("vike-test", 4711, "0.1.0"),
            catalog_refresh: DatahubCatalogRefresh::new(Vec::new(), tx),
            catalog: Arc::new(vike_catalog::Catalog::from_instruments(Vec::new())),
            history: None,
            control: BackendControlLink::Open,
            window: ("", ""),
            stale: false,
            unreachable: None,
        }
    }

    pub fn context(&self) -> WindowToolContext<'_> {
        WindowToolContext {
            tool_feeds: &self.tool_feeds,
            snapshot: &self.snapshot,
            country_flags: &self.textures,
            logos: &self.textures,
            execution_history: AccountWindowHistoryContext::none(),
            feeds: &self.feeds,
            symbol_set_store: &self.symbol_set_store,
            display_time_zone: vike_ui_theme::time_zone::ThemeDisplayTimeZone::default(),
            stored: DataManagerInventoryContext {
                tree: &self.tree,
                gaps: &self.gaps,
                partials: &self.partials,
                loading: false,
                load_error: None,
                backfill_status: "",
                delete_unavailable: None,
                partials_note: None,
                history_channels: self.history.as_ref(),
                progress: &self.progress,
            },
            feed_statuses: &self.statuses,
            credentials: vike_account_manager::AccountManagerCredentialWrite {
                settings_dir: Path::new(""),
                journal: None,
                process: &self.process,
                now_ms: 0,
            },
            market: WindowToolContextMarket {
                symbol: self.window.1,
                venue: self.window.0,
                book: None,
                stale: self.stale,
                trades: None,
            },
            catalog_refresh: &self.catalog_refresh,
            catalog: &self.catalog,
            last_trade: None,
            directory: None,
            directory_unavailable: false,
            node_words: vike_tradehub_client::node_words::BackendNodeWords::NONE,
            control_link: self.control,
            node_takes_post_only: false,
            datahub_unreachable: self.unreachable.as_deref(),
        }
    }
}
