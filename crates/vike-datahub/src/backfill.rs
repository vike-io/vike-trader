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
//!   every build; only [`real_backfill_table`] — the constructor that names the collector crate —
//!   sits behind `backfill-serve`, which is what keeps the default server's dependency tree
//!   collector-free. ⚠ That property is what bounds how far 0059 Phase 3's collapse could reach in
//!   this file: the constructor FOLDS `vike_backfill::kline_source::KLINE_SOURCES` instead of
//!   listing six closures, but `vike_backfill::kline_source::KlineSource` may NOT appear in
//!   [`BackfillTable`]'s own type, or the feature-free half would be gone.
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
/// order — it is what the unknown-venue error prints — and for the real table it is decided in ONE
/// place, `vike_backfill::kline_source::KLINE_SOURCES`, whose own declaration order this
/// constructor preserves.
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

/// The PRODUCTION table: **every venue-direct kline collector `vike-backfill` ships**, each writing
/// through `store`, the SAME `Arc<DataFusionHist>` the caller serves.
///
/// ⚠ **It names NO venue, and that is 0059 Phase 3's whole point.** This was six hand-written
/// closures naming six `backfill_<venue>_klines` functions — the second of two independent copies
/// of one roster, the other being the supervisor's own `COLLECTORS` table (a since-deleted
/// `supervisor/registry.rs`), in a crate that cannot see this one. Both collapsed into
/// `crates/vike-backfill/src/kline_source.rs`'s `KLINE_SOURCES`, and this constructor now FOLDS
/// that static: adding a venue costs one impl over there and nothing at all here. What the fold
/// cannot do is silently disagree with the supervisor about which fetcher a venue gets, which is
/// the class of bug the two copies made possible.
///
/// ⚠ **It named three of six until 0059 Phase 2**, while six collectors were written. Aster and
/// deribit already matched the dispatch shape and were reachable from nothing at all; hyperliquid
/// needed a one-symbol adapter. Nothing in the tree could see that, because every test near this
/// table walked from a row OUTWARD; `crates/vike-ops/tests/collector_dispatch_gate.rs` is the walk
/// in the other direction and now compares the written collector modules to the ONE registry.
///
/// ⚠ **The per-venue spellings this comment used to argue about are GONE with the closures** — the
/// crate-root re-exports for binance/bybit/okx versus module paths for the other three, and
/// hyperliquid naming its `_by_symbol` ADAPTER rather than the collector proper. The adapter's
/// reason survives and has simply moved inside the seam: a one-symbol dispatch can only express
/// `coin == symbol`, which is right for every HL perp and WRONG for HL spot, so
/// `vike_backfill::hyperliquid::HyperliquidKlines::fetch` REFUSES those spellings rather than
/// guessing a coin, fetching the wrong book and spending the commit key that would make a
/// corrective re-fetch a silent zero-row success.
///
/// ⚠ **`BackfillTable`'s own TYPE stays feature-free** — the fold happens here, inside
/// `backfill-serve`, and produces the same `Vec<(String, Box<dyn Fn…>)>` it always did. No
/// `vike_backfill` name reaches the struct's signature, so `server.rs` still dispatches through it
/// on every build and a default server's dependency tree is still collector-free. Folding a
/// `Box<dyn KlineSource>` into the table's type instead would have undone that, which is the one
/// way an otherwise-correct version of this refactor goes wrong.
///
/// ORDER is `KLINE_SOURCES`' declared order, which is what the unknown-venue error prints and what
/// `Welcome` advertises — see that static's own ⚠ note, and
/// `tests/backfill_roundtrip.rs`'s `the_real_table_names_every_kline_venue`, which pins the rendered
/// list.
///
/// Behind `backfill-serve` because this is the one function that NAMES the collector crate, and the
/// name is what drags `vike-backfill/venue-backfill` (seven bridge crates) into the build.
/// Deliberately NOT the `_paced` variants: the pace session is a per-process file the long-running
/// CLI backfills own; a server answering interactive chart-gap requests takes the plain pager,
/// whose internal paging/pacing is already venue-safe. (`KLINE_SOURCES` carries the plain pager for
/// the same reason — the `_paced` twins are a bin concern and are not registry rows.)
#[cfg(feature = "backfill-serve")]
pub fn real_backfill_table(store: std::sync::Arc<vike_data::DataFusionHist>) -> BackfillTable {
    BackfillTable::new(
        vike_backfill::kline_source::KLINE_SOURCES
            .iter()
            .map(|source| {
                let store = std::sync::Arc::clone(&store);
                let source = *source;
                let collect: BackfillFn = Box::new(move |symbol, interval, start, end| {
                    vike_backfill::kline_source::backfill_kline_source(
                        &store, source, symbol, interval, start, end,
                    )
                    .map_err(|e| e.to_string())
                });
                (source.venue().to_string(), collect)
            })
            .collect(),
    )
}
