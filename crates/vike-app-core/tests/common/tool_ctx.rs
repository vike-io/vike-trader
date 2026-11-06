//! The ONE owner of everything a `ToolCtx` borrows, for this crate's window suites
//! (`account_window_screens`, `data_manager_screens`, `trade_cause_line`, `trade_window_screens`),
//! plus the widths and pixel densities they read a window at. Each of them carried its own copy of
//! `Owned` and its `ctx`; the copies differed only in which fields a window reads, so there is one
//! now: [`Owned::new`] is the quiet default and a suite overrides the fields its window reads.
//!
//! The `screens` binary (`tests/screens.rs`) declares it ONCE, `#[path = "common/tool_ctx.rs"] mod
//! tool_ctx;`, and each suite reads it as `crate::tool_ctx`. A `tests/` SUBDIRECTORY file is not a
//! test target, so this builds no binary of its own. `allow(dead_code)`: a suite uses a subset (the
//! Data Manager suite reads no width).
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use vike_app_core::data::catalog_refresh::CatalogRefresh;
use vike_app_core::data::history_column::HistoryLoad;
use vike_app_core::tools::ToolData;
use vike_app_core::ui::tool_views::{BookCtx, ControlLink, HistoryCtx, StoredCtx, ToolCtx};
use vike_change_journal::Proc;
use vike_exec::CoreSnapshot;

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

/// Everything `ToolCtx` borrows, owned, so a pass can build one.
pub struct Owned {
    pub td: ToolData,
    pub snap: CoreSnapshot,
    pub textures: HashMap<String, egui::TextureHandle>,
    pub dsets: vike_data::datasets::Store,
    pub tree: Vec<vike_data_manager::model::VenueNode>,
    pub gaps: vike_data_manager::GapMap,
    pub partials: vike_data_manager::PartialDayMap,
    pub feeds: Vec<(String, usize, i64, i64)>,
    pub statuses: HashMap<String, Arc<Mutex<String>>>,
    pub proc: Proc,
    pub catalog: CatalogRefresh,
    pub symbols: Arc<vike_catalog::Catalog>,
    /// The By-venue table's HISTORY column answer; `None` for a window that reads none.
    pub history: Option<HistoryLoad>,
    pub control: ControlLink,
    /// The window's `(venue, symbol)`; both empty is a window with no instrument (a Trade window's
    /// first frame, or a window that has none).
    pub window: (&'static str, &'static str),
    /// Whether the window's book reads as stale. The book itself is always `None`, as for a venue
    /// with no feed (the shell reads it from the `BookStore`, which nothing has written).
    pub stale: bool,
    /// What the shell hands the window as `datahub_unreachable`: the session's own answer.
    pub unreachable: Option<String>,
}

impl Owned {
    /// A window over `snap` with nothing else in it: no store, no feed, no status line, no book, an
    /// open control link and a reachable (or unconfigured) datahub.
    pub fn new(snap: CoreSnapshot) -> Self {
        // The catalog handle does no I/O at construction, and nothing here ever sends.
        let (tx, _rx) = std::sync::mpsc::channel();
        Owned {
            td: ToolData::default(),
            snap,
            textures: HashMap::new(),
            dsets: vike_data::datasets::Store::default(),
            tree: Vec::new(),
            gaps: vike_data_manager::GapMap::default(),
            partials: vike_data_manager::PartialDayMap::default(),
            feeds: Vec::new(),
            statuses: HashMap::new(),
            proc: Proc::new("vike-test", 4711, "0.1.0"),
            catalog: CatalogRefresh::new(Vec::new(), None, None, None, tx),
            symbols: Arc::new(vike_catalog::Catalog::from_instruments(Vec::new())),
            history: None,
            control: ControlLink::Open,
            window: ("", ""),
            stale: false,
            unreachable: None,
        }
    }

    pub fn ctx(&self) -> ToolCtx<'_> {
        ToolCtx {
            td: &self.td,
            snap: &self.snap,
            flags: &self.textures,
            logos: &self.textures,
            history: HistoryCtx::none(),
            feeds: &self.feeds,
            dsets: &self.dsets,
            display_tz: vike_chart::DisplayTz::default(),
            stored: StoredCtx {
                tree: &self.tree,
                gaps: &self.gaps,
                partials: &self.partials,
                loading: false,
                load_error: None,
                backfill_status: "",
                delete_unavailable: None,
                partials_note: None,
                history: self.history.as_ref(),
            },
            feed_statuses: &self.statuses,
            credentials: vike_gateway_panel::CredentialWrite {
                settings_dir: Path::new(""),
                journal: None,
                proc: &self.proc,
                now_ms: 0,
            },
            book: BookCtx {
                symbol: self.window.1,
                venue: self.window.0,
                book: None,
                stale: self.stale,
                trades: None,
            },
            catalog: &self.catalog,
            symbols: &self.symbols,
            last_trade: None,
            directory: None,
            directory_unavailable: false,
            key_changed: &[],
            control_link: self.control,
            node_takes_post_only: false,
            datahub_unreachable: self.unreachable.as_deref(),
        }
    }
}
