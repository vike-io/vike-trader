//! The backfill-on-demand collector seam (split-plane REQ-9): the venue → collector dispatch
//! table behind `Request::Backfill`.
//!
//! # Why a TABLE and not direct calls
//!
//! The real collectors (the rows of `KLINE_SOURCES`, directly below) do venue REST I/O and are
//! compiled only once `backfill-serve` turns their bridge crates on — a property the SERVER must not
//! inherit unconditionally. The table is the smallest injection point that solves both at once:
//!
//! - **Build shape**: the table TYPE is feature-free (`Box<dyn Fn>` over scalars — no
//!   `vike-backfill` name anywhere in its signature), so `server.rs` dispatches through it on
//!   every build; only `real_backfill_table` — the constructor that names every collector crate —
//!   sits behind `backfill-serve`, which is what keeps the default server's dependency tree
//!   collector-free. ⚠ That property is what bounds how far 0059 Phase 3's collapse could reach in
//!   this file: the constructor FOLDS `KLINE_SOURCES` instead of listing six closures, but
//!   `vike_data::source::KlineSource` may NOT appear in [`BackfillTable`]'s own type, or the
//!   feature-free half would be gone.
//! - **Testability**: CI is deterministic and network-free, so the composed roundtrip test
//!   (`tests/backfill_roundtrip.rs`) installs FAKE entries that write known bars through the same
//!   `Arc<DataFusionHist>` the server serves — proving request → real store → `BackfillDone` →
//!   `LoadBars` without a venue on the wire. The real collectors are never called in CI; only the
//!   `#[ignore]`d probes in `tests/venue_interval_matrix.rs` drive them against venues.
//!
//! # The write-through contract
//!
//! Every entry writes INTO the store the server serves — the closure captures the same
//! `Arc<DataFusionHist>` handle `serve` holds (upcast to the trait for serving) — so a backfilled
//! range is visible to the very next `LoadBars` on any connection, and is never lost ("writers
//! live next to the data", Principle 3). The verb handler reads the range back through the served
//! handle before replying; [`BackfillDone`](vike_datahub_client::proto::BackfillDone) carries what
//! it found.
//!
//! # Lanes
//!
//! An entry carries a [`BackfillLane`], and the REQUEST picks the lane: `Backfill` reaches the
//! funding lane exactly when its interval is the reserved `vike_data::source::FUNDING_INTERVAL`
//! label and a bar lane otherwise ([`BackfillTable::get`]), while the chart seed reaches the kline
//! lane and nothing else ([`BackfillTable::get_for_seed`]) — so opening a chart never starts a tick
//! download or a funding fetch. The funding lane is negotiated apart from the verb, by
//! `vike_datahub_client::FEATURE_BACKFILL_FUNDING`, which a server advertises exactly when its
//! table's [`BackfillTable::has_funding`] answers `true`. The tick lane is dukascopy's alone and is
//! mounted the same way — one [`BackfillLane::TickBars`] row `real_backfill_table` writes by hand
//! (dukascopy has no `KlineSource` to fold a registry over), fetching through
//! `vike_dukascopy::fetch_quotes_range` and storing+resampling through
//! `vike_backfill::venues::dukascopy::backfill_quotes_then_bars` — reachable only from a `Backfill`
//! request naming dukascopy, never from the chart seed.

/// One venue's collector: `(symbol, interval, start_ms, end_ms)` → rows written (0 = the window
/// was already ingested — the collectors are idempotent by commit key). The venue and the store
/// are baked into the closure by the table's constructor; errors are stringified because the wire
/// carries them as `Response::Error` text either way.
pub type BackfillFn = Box<dyn Fn(&str, &str, i64, i64) -> Result<usize, String> + Send + Sync>;

/// Which history a table entry produces. The `Backfill` verb picks the lane from the request's
/// interval; the chart seed may only use [`BackfillLane::Klines`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackfillLane {
    /// The venue's own OHLCV bars (`KLINE_SOURCES`).
    Klines,
    /// Bars RESAMPLED from the venue's ticks, which are stored as `kind=quote` first.
    TickBars,
    /// The market funding-rate series, `interval=funding` (`FUNDING_SOURCES`).
    Funding,
}

/// The venue → collector dispatch table `serve_with_backfill` mounts, one entry per
/// `(venue, lane)`. Order is the declared order — it is what the unknown-venue errors print — and
/// for the real table it is decided by the registries it folds: the bar lane's `KLINE_SOURCES`
/// and the funding lane's `FUNDING_SOURCES`, whose own declaration orders this constructor
/// preserves.
pub struct BackfillTable {
    entries: Vec<(String, BackfillLane, BackfillFn)>,
}

impl BackfillTable {
    /// A table of KLINE entries — the test seam, unchanged for every existing caller. Production
    /// code goes through `real_backfill_table` instead, so a fake can never be mounted by
    /// accident: this constructor takes closures, and the only closures naming real collectors
    /// live there.
    pub fn new(entries: Vec<(String, BackfillFn)>) -> Self {
        Self { entries: entries.into_iter().map(|(v, f)| (v, BackfillLane::Klines, f)).collect() }
    }

    /// Add one entry on `lane`.
    pub fn with(mut self, venue: &str, lane: BackfillLane, collect: BackfillFn) -> Self {
        self.entries.push((venue.to_string(), lane, collect));
        self
    }

    /// The venues that answer a BAR interval (klines and tick-resampled bars), in declared order
    /// — the unknown-venue error's set.
    pub fn supported(&self) -> Vec<&str> {
        self.venues_where(|lane| lane != BackfillLane::Funding)
    }

    /// The venues with a funding-rate source, in declared order.
    pub fn funding_supported(&self) -> Vec<&str> {
        self.venues_where(|lane| lane == BackfillLane::Funding)
    }

    /// The venues the chart seed may reach — klines only.
    pub fn seed_supported(&self) -> Vec<&str> {
        self.venues_where(|lane| lane == BackfillLane::Klines)
    }

    /// Whether any funding lane is mounted — what `backfill_funding` advertises.
    pub fn has_funding(&self) -> bool {
        !self.funding_supported().is_empty()
    }

    /// The entry for a `Backfill` request: the funding lane when `interval` is
    /// `vike_data::source::FUNDING_INTERVAL`, otherwise the venue's bar lane.
    pub fn get(&self, venue: &str, interval: &str) -> Option<&BackfillFn> {
        let funding = interval == vike_data::source::FUNDING_INTERVAL;
        self.entries
            .iter()
            .find(|(v, lane, _)| v == venue && (*lane == BackfillLane::Funding) == funding)
            .map(|(_, _, f)| f)
    }

    /// The entry the chart seed may use: klines only, so opening a chart never starts a tick
    /// download or a funding fetch.
    pub fn get_for_seed(&self, venue: &str) -> Option<&BackfillFn> {
        self.entries
            .iter()
            .find(|(v, lane, _)| v == venue && *lane == BackfillLane::Klines)
            .map(|(_, _, f)| f)
    }

    fn venues_where(&self, keep: impl Fn(BackfillLane) -> bool) -> Vec<&str> {
        self.entries.iter().filter(|(_, lane, _)| keep(*lane)).map(|(v, _, _)| v.as_str()).collect()
    }
}

impl std::fmt::Debug for BackfillTable {
    /// Venue names only — the closures have nothing printable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackfillTable")
            .field("supported", &self.supported())
            .field("funding", &self.funding_supported())
            .finish()
    }
}

/// **Every kline source this daemon can dispatch.** The ONE roster: [`real_backfill_table`],
/// directly below, FOLDS it into the wire verb's dispatch table rather than re-listing six
/// closures.
///
/// A `static` (not a `const`) so the lookups below hand out a genuinely `'static` borrow — which is
/// what lets the datahub's table move one into each of its `'static` closures.
///
/// ⚠ **ORDER IS OBSERVABLE.** It is what the datahub's unknown-venue refusal prints and what the
/// `Welcome` frame advertises (`BackfillTable::supported` preserves it), and
/// `crates/vike-datahub/tests/backfill_roundtrip.rs`'s `the_real_table_names_every_kline_venue`
/// pins the rendered order. This is the collectors' own order — the three that shipped first, then
/// the three 0059 Phase 2 dispatched.
///
/// ⚠ **Each row spells `&vike_<venue>::<module>::<Type>`, and the gate DERIVES the registered set
/// from that spelling.** (It was `&crate::venues::<venue>::<Type>` in `vike-backfill` until
/// docs/decisions/0094 moved every impl into the bridge crate that already owned the fetcher it
/// wraps, and this whole static up into THIS file — the one process that actually dispatches a
/// venue string to a collector — so `vike-backfill` no longer names a single kline bridge.)
/// `crates/vike-ops/tests/collector_dispatch_gate.rs` is a text scan — it lives in `vike-ops`,
/// which may not take a normal edge to this crate (layer 15 against this crate's 65), and a
/// dev-edge would drag every kline bridge crate into a test build every PR runs — and THIS FILE is
/// the registry it walks: both `REGISTRY_FILE` and `WIRE_TABLE_FILE` name it now, since the static
/// and the fold that consumes it live side by side. A row written some other way is a row that scan
/// cannot see, and the failure is safe: the venue then reads as WRITTEN-BUT-UNREGISTERED and the
/// gate goes red.
#[cfg(feature = "backfill-serve")]
pub static KLINE_SOURCES: &[&'static dyn vike_data::source::KlineSource] = &[
    &vike_binance::data::BinanceKlines,
    &vike_bybit::data::BybitKlines,
    &vike_okx::data::OkxKlines,
    &vike_aster::data::AsterKlines,
    &vike_deribit::data::DeribitKlines,
    &vike_hyperliquid::history::HyperliquidKlines,
];

/// **Every funding-rate source the data plane serves** — the ONE funding roster, folded by
/// [`real_backfill_table`] into its `Funding` lane. It replaced `vike-backfill`'s
/// `funding_rate::source_by_name` (docs/decisions/0094).
#[cfg(feature = "backfill-serve")]
pub static FUNDING_SOURCES: &[&'static dyn vike_data::source::FundingRateSource] =
    &[&vike_binance::data::BinanceFunding, &vike_hyperliquid::funding::HyperliquidFunding];

/// The PRODUCTION table: **every venue-direct collector this daemon can dispatch**, each writing
/// through `store`, the SAME `Arc<DataFusionHist>` the caller serves.
///
/// ⚠ **It names NO venue, and that is 0059 Phase 3's whole point.** This was six hand-written
/// closures naming six `backfill_<venue>_klines` functions — the second of two independent copies
/// of one roster, the other being the supervisor's own `COLLECTORS` table (a since-deleted
/// `supervisor/registry.rs`), in a crate that cannot see this one. Both collapsed into
/// [`KLINE_SOURCES`] — since docs/decisions/0094 a static of THIS file, directly above, rather than
/// a cross-crate one — and this constructor FOLDS it: adding a venue costs one impl in
/// the venue's own bridge crate and one row in that static, nothing else here. What the fold could
/// not do, while the supervisor lived (docs/decisions/0094 deleted it), was silently disagree with
/// it about which fetcher a venue gets — the class of bug the two copies made possible.
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
/// `vike_hyperliquid::history::HyperliquidKlines::fetch` REFUSES those spellings rather than
/// guessing a coin, fetching the wrong book and spending the commit key that would make a
/// corrective re-fetch a silent zero-row success.
///
/// ⚠ **`BackfillTable`'s own TYPE stays feature-free** — the fold happens here, inside
/// `backfill-serve`, and produces the same feature-free entries it always did (a venue string and
/// a `Box<dyn Fn…>`, now tagged with a [`BackfillLane`]). No
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
/// Behind `backfill-serve` together with [`KLINE_SOURCES`] and [`FUNDING_SOURCES`] above — between
/// them, this feature is what turns on every collector's own bridge crate; a default or plain
/// `serve-datafusion` build carries none of them. Every row takes the plain pager, whose internal
/// paging/pacing is already venue-safe; the `_paced` twins and their per-process pace file belonged
/// to the one-shot CLI programs, which docs/decisions/0094 deleted.
///
/// ⚠ **The FUNDING lane is folded here too**, from [`FUNDING_SOURCES`], and it is what replaced the
/// `funding_rate_backfill` program (docs/decisions/0094): each row becomes a
/// [`BackfillLane::Funding`] entry, ingested by `vike_backfill::funding_rate::backfill_funding_rate`
/// through `store` under the reserved `interval=funding` label. Those entries answer only a request
/// whose interval IS that label, so they appear in [`BackfillTable::funding_supported`] and in
/// neither [`BackfillTable::supported`] nor the chart seed's set.
///
/// ⚠ **The TICK lane is ONE HAND-WRITTEN ROW, not a fold — this function's LAST line.** Dukascopy
/// carries no `KlineSource` impl to fold a registry over: its fetch is already `(symbol, start_ms,
/// end_ms) -> Vec<QuoteTick>`, with no one-symbol seam left to adapt
/// (`crates/vike-ops/tests/collector_dispatch_gate.rs`'s `written_kline_collectors` doc says why).
/// So its [`BackfillLane::TickBars`] entry composes `vike_dukascopy::fetch_quotes_range` (the
/// bridge's keyless `.bi5` fetch) with `vike_backfill::venues::dukascopy::backfill_quotes_then_bars`
/// (store the ticks as quotes, then resample to `interval` bars) directly — the way every kline
/// venue's row did before [`KLINE_SOURCES`] existed to fold. It answers a `Backfill` request naming
/// dukascopy at any BAR interval (never the reserved `funding` label);
/// [`BackfillTable::get_for_seed`] still restricts the chart seed to [`BackfillLane::Klines`], so a
/// chart open still cannot reach it.
#[cfg(feature = "backfill-serve")]
pub fn real_backfill_table(store: std::sync::Arc<vike_data::DataFusionHist>) -> BackfillTable {
    let mut table = BackfillTable::new(
        KLINE_SOURCES
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
    );
    for source in FUNDING_SOURCES.iter().copied() {
        let store = std::sync::Arc::clone(&store);
        table = table.with(
            source.venue(),
            BackfillLane::Funding,
            Box::new(move |symbol: &str, _interval: &str, start: i64, end: i64| {
                vike_backfill::funding_rate::backfill_funding_rate(
                    &store, source, symbol, start, end,
                )
                .map(|written| {
                    // `BackfillDone` carries one count, the RATE rows; the premium half lands
                    // under its own commit key, so a re-run can write premium rows while
                    // reporting 0 rate rows — the log is where an operator sees both.
                    tracing::info!(
                        "funding {}:{symbol} [{start}, {end}]: {} rate rows, {} premium rows",
                        source.venue(),
                        written.rate_rows,
                        written.premium_rows
                    );
                    written.rate_rows
                })
                .map_err(|e| e.to_string())
            }),
        );
    }
    let store = std::sync::Arc::clone(&store);
    table.with(
        vike_backfill::venues::dukascopy::VENUE,
        BackfillLane::TickBars,
        Box::new(move |symbol: &str, interval: &str, start: i64, end: i64| {
            vike_backfill::venues::dukascopy::backfill_quotes_then_bars(
                &store,
                symbol,
                interval,
                start,
                end,
                vike_dukascopy::fetch_quotes_range,
            )
            .map_err(|e| e.to_string())
        }),
    )
}
