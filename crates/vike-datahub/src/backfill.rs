//! The backfill-on-demand collector seam (split-plane REQ-9): the venue → collector dispatch
//! table behind `Request::Backfill`.
//!
//! # Why a TABLE and not direct calls
//!
//! The real collectors (`vike_backfill::backfill_binance_klines` and siblings) do venue REST I/O
//! and are compiled only under `vike-backfill/venue-backfill` — two properties the SERVER must not
//! inherit unconditionally. The table is the smallest injection point that solves both at once:
//!
//! - **Build shape**: the table TYPE is feature-free (`Box<dyn Fn>` over scalars — no
//!   `vike-backfill` name anywhere in its signature), so `server.rs` dispatches through it on
//!   every build; only [`real_backfill_table`] — the constructor that names the collectors — sits
//!   behind `backfill-serve`, which is what keeps the default server's dependency tree
//!   collector-free.
//! - **Testability**: CI is deterministic and network-free, so the composed roundtrip test
//!   (`tests/backfill_roundtrip.rs`) installs FAKE entries that write known bars through the same
//!   `Arc<DataFusionHist>` the server serves — proving request → real store → `BackfillDone` →
//!   `LoadBars` without a venue on the wire. The real collectors are never called in CI; only the
//!   `#[ignore]`d live paths in `vike-backfill` drive them against venues.
//!
//! # The write-through contract
//!
//! Every entry writes INTO the store the server serves — the closure captures the same
//! `Arc<DataFusionHist>` handle `serve` holds (upcast to the trait for serving) — so a backfilled
//! range is visible to the very next `LoadBars` on any connection, and is never lost ("writers
//! live next to the data", Principle 3). The verb handler reads the range back through the served
//! handle before replying; [`BackfillDone`](vike_datahub_client::proto::BackfillDone) carries what
//! it found.

/// One venue's collector: `(symbol, interval, start_ms, end_ms)` → rows written (0 = the window
/// was already ingested — the collectors are idempotent by commit key). The venue and the store
/// are baked into the closure by the table's constructor; errors are stringified because the wire
/// carries them as `Response::Error` text either way.
pub type BackfillFn = Box<dyn Fn(&str, &str, i64, i64) -> Result<usize, String> + Send + Sync>;

/// The venue → collector dispatch table `serve_with_backfill` mounts. Order is the declared
/// order — it is what the unknown-venue error prints, so keep it stable and alphabetical-ish
/// (the real table is binance/bybit/okx, the collectors' own order).
pub struct BackfillTable {
    entries: Vec<(String, BackfillFn)>,
}

impl BackfillTable {
    /// A table from explicit `(venue, collector)` entries — the test seam. Production code goes
    /// through [`real_backfill_table`] instead, so a fake can never be mounted by accident: this
    /// constructor takes closures, and the only closures naming real collectors live there.
    pub fn new(entries: Vec<(String, BackfillFn)>) -> Self {
        Self { entries }
    }

    /// The venues this table can backfill, in declared order — the unknown-venue error's
    /// "supported" set, and the `Welcome` advertisement's evidence.
    pub fn supported(&self) -> Vec<&str> {
        self.entries.iter().map(|(venue, _)| venue.as_str()).collect()
    }

    /// The collector for `venue`, or `None` (→ the error naming [`Self::supported`]).
    pub fn get(&self, venue: &str) -> Option<&BackfillFn> {
        self.entries.iter().find(|(v, _)| v == venue).map(|(_, f)| f)
    }
}

impl std::fmt::Debug for BackfillTable {
    /// Venue names only — the closures have nothing printable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackfillTable").field("supported", &self.supported()).finish()
    }
}

/// The PRODUCTION table: the three venue-direct kline collectors `vike-backfill` ships —
/// `backfill_binance_klines` / `backfill_bybit_klines` / `backfill_okx_klines` — each writing
/// through `store`, the SAME `Arc<DataFusionHist>` the caller serves. These are the exact
/// functions the fat GUI's `maybe_spawn_backfill` family calls today; a later PR re-points the
/// GUI onto the wire verb and deletes those call sites (spec §3).
///
/// Behind `backfill-serve` because this is the one function that NAMES the collectors, and the
/// name is what drags `vike-backfill/venue-backfill` (seven bridge crates) into the build.
/// Deliberately NOT the `_paced` variants: the pace session is a per-process file the long-running
/// CLI backfills own; a server answering interactive chart-gap requests takes the plain pager,
/// whose internal paging/pacing is already venue-safe.
#[cfg(feature = "backfill-serve")]
pub fn real_backfill_table(store: std::sync::Arc<vike_data::DataFusionHist>) -> BackfillTable {
    let binance = std::sync::Arc::clone(&store);
    let bybit = std::sync::Arc::clone(&store);
    let okx = store;
    BackfillTable::new(vec![
        (
            "binance".to_string(),
            Box::new(move |symbol, interval, start, end| {
                vike_backfill::backfill_binance_klines(&binance, symbol, interval, start, end)
                    .map_err(|e| e.to_string())
            }),
        ),
        (
            "bybit".to_string(),
            Box::new(move |symbol, interval, start, end| {
                vike_backfill::backfill_bybit_klines(&bybit, symbol, interval, start, end)
                    .map_err(|e| e.to_string())
            }),
        ),
        (
            "okx".to_string(),
            Box::new(move |symbol, interval, start, end| {
                vike_backfill::backfill_okx_klines(&okx, symbol, interval, start, end)
                    .map_err(|e| e.to_string())
            }),
        ),
    ])
}
