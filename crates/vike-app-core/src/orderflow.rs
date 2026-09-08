//! App-side per-bar orderflow aggregation: bucket the drained trade tape into the chart's
//! bars (by open-time) and retain per-bar footprint cells — the substrate SP2 renders from.
//!
//! **Pre-bar-0 ticks are never silently lost** (#937 follow-up). [`OrderflowAgg::ingest`] used to
//! `continue` past every tick whose timestamp fell before the first bar's open — which, when the
//! bar grid was EMPTY, meant *every* tick of the batch. Both lanes that feed this aggregator hit
//! it: the live trade drain and the staged aggTrades backfill (`core_sync::sync_from_core`). #937
//! made the backfill side observable with a `warn!` but deliberately left the drop in place. The
//! two dispositions now are, and neither is silent:
//!
//! * **No grid at all** (`bar_ots.is_empty()`) — the tick is *unplaceable for now*, and the cause
//!   is transient by construction: a Data-manager delete removes `charts[k]` alongside `of_aggs[k]`,
//!   so a re-add presents an aggregator whose chart has not synced its bars yet. Those ticks are
//!   HELD in [`OrderflowAgg`]`::pending` and folded in — each at ITS OWN bar, by timestamp — on the
//!   first later `ingest` that carries a grid. Bounded by [`PENDING_MAX_TICKS`]; at the bound the
//!   OLDEST held ticks are evicted, counted in `OrderflowAgg::evicted_ticks` and `warn!`ed.
//! * **A grid exists but the tick predates bar 0** — *unplaceable by construction*, not transient:
//!   `cells` is index-aligned to the chart's bars, which is only sound because a chart's bar list
//!   grows forward (the standing assumption this whole struct already rests on), so no later grid
//!   this aggregator can be fed will contain that timestamp. Discarding is correct; discarding
//!   *silently* was the defect. Counted in `OrderflowAgg::dropped_before_grid` and `warn!`ed on a
//!   doubling schedule. A held tick that is still too old when the grid appears takes this same
//!   path — which is why the hold needs no separate age bound: staleness resolves itself into a
//!   counted discard at flush time.
use std::collections::BTreeMap;
use std::sync::Arc;
use vike_model::TradeTick;
use vike_orderflow::{FootprintBar, PriceBin};

/// A "nice" 1/2/5×10^k bucket tick ~ price × 0.0002 (≈2 bps), for readable footprint/profile
/// granularity across instruments (BTC ~$10, ETH ~$0.5, a $0.5 coin ~$0.0001). `price <= 0 → 1.0`.
///
/// Mirrors the 1/2/5 tiering of the chart's `scale::nice_step_ceil` (its `<=` boundaries), but
/// rounds to the NEAREST nice step (not ceil) so ~2 bps lands on a readable width either side:
/// 12.8 → 10 (not 20), 0.6 → 0.5 (not 1.0). Used as the derived default at the `OrderflowAgg::new`
/// site when the user hasn't pinned `of_tick_size`.
///
/// ## ⚠ The DECADE CLIFF — the largest divergence MAGNITUDE the libm survey found
///
/// The magnitude line (`let mag = libm::pow(10.0, libm::log10(raw).floor());`) is spelled with the
/// `libm` CRATE rather than the `f64::log10`/`f64::powf` METHODS. IEEE 754 requires `+ - * /` and
/// `sqrt` to be correctly rounded; it requires NOTHING of `log10` or `pow`. A method call reaches
/// the PLATFORM's libm — glibc on the CI runners, the MSVC runtime on the Windows dev box — and the
/// two are each entitled to their own last bit.
///
/// ⚠ Read the heading precisely: the survey ranked the POTENTIAL magnitude of a divergence at each
/// site, and did NOT observe one here. No divergence has been measured at this function; what has
/// been established is that if one ever occurs, this is the shape that turns a last bit into a
/// factor of ten. The same shape lives in `crates/vike-chart/src/scale.rs`'s `nice_step_ceil` and
/// `log_nice_values` (its own module doc argues the case there); this function is the instance
/// whose OUTPUT is a bucket width rather than a gridline position, which is why it is written up
/// here at length rather than by reference.
///
/// At a smooth site a libm disagreement costs a last bit. **Here it would cost a factor of ten**,
/// because `.floor()` applied to a logarithm is a QUANTIZER: it maps a continuum onto integers, so
/// the two libms agree on the answer everywhere EXCEPT arbitrarily close to an exact power of ten —
/// and exactly there, a disagreement in the last bit flips the floor by a whole one. `raw` is
/// `price * 0.0002`, so a price of 5_000.0 gives `raw == 1.0` and a price of 50_000.0 gives
/// `raw == 10.0`: both land on a decade boundary, and both are ordinary instrument prices rather
/// than contrived ones (0.0002 is not representable, but at both prices the product rounds back to
/// exactly 1.0 and exactly 10.0). If one platform were to return
/// `log10(10.0) == 0.9999999999999999` where the other returns exactly `1.0`, `mag` comes back as
/// 1.0 on one box and 10.0 on the other; the `n = raw / mag` tiering then reads 10.0 vs 1.0, the
/// `< 1.5 / < 3.5 / < 7.5` ladder picks 10.0 vs 1.0, and the returned bucket width is 10.0 on one
/// platform and 1.0 on the other — a footprint/volume-profile grid ten times finer or coarser from
/// identical inputs, with nothing anywhere reporting an error.
///
/// The `libm` crate is a pure-Rust FDLIBM port that returns the same bits on every target, which
/// removes the disagreement rather than making the cliff less steep. The cliff itself is inherent
/// to "quantize a logarithm" and is not a defect: the function is CORRECT either side of it, it
/// simply must not answer differently on two machines.
pub fn nice_orderflow_tick(price: f64) -> f64 {
    // Guard non-positive / non-finite (NaN or ±inf) → the neutral $1 tick. `!(price > 0.0)` would
    // be the terse NaN-catching idiom but trips `clippy::neg_cmp_op_on_partial_ord`; this form is
    // equivalent for 0/negatives, still catches NaN, and also rejects inf (which would otherwise
    // propagate through the log/pow math to a garbage bucket width).
    if !price.is_finite() || price <= 0.0 {
        return 1.0;
    }
    let raw = price * 0.0002;
    // ⚠ `libm::` on BOTH halves, not the `f64` methods — see the decade-cliff section on this
    // function's doc. `libm::pow` rather than a `powi`: the exponent is an `f64` that only happens
    // to hold an integral value, and the measured "powers of ten are exactly representable"
    // exemption applies to the INTEGER-exponent `powi` path, not to this one.
    let mag = libm::pow(10.0, libm::log10(raw).floor());
    let n = raw / mag;
    let nice = if n < 1.5 {
        1.0
    } else if n < 3.5 {
        2.0
    } else if n < 7.5 {
        5.0
    } else {
        10.0
    };
    nice * mag
}

/// The `before_id` to pass straight through to
/// `vike_binance::agg_trades_backfill_reported`/`backfill_agg_trades_backward`: `min_live` (the
/// live trades feed's earliest-seen aggTrade id) itself — NOT `min_live - 1`.
///
/// SP3-final-review M1 fix: the pager already treats `before_id` as an EXCLUSIVE upper bound —
/// see `backfill_agg_trades_backward`'s doc comment in vike-binance ("every id this emits is
/// `< before_id`") and its `debug_assert!(t.id < before_id, ...)`. So passing `min_live`
/// unmodified emits every backfilled id strictly `< min_live`, which correctly INCLUDES the
/// boundary trade at id `min_live - 1` while staying disjoint from the live feed — the
/// no-double-count invariant (`global-constraints.md`) still holds: every emitted id is
/// `< min_live <= every live id`. The previous `min_live - 1` double-subtracted against the
/// pager's own exclusivity, so it silently dropped the boundary trade (would only have emitted
/// ids `< min_live - 1`) without buying back any real overlap protection.
///
/// `None` when there's no room below id 1 to page into (`min_live <= 1`) — mirrors `main.rs`'s
/// `maybe_spawn_backfill`, which used to inline this guarded by `if min_live <= 1 { return }`;
/// extracted here purely so it's unit-testable without spawning a real backfill thread.
pub fn backfill_before_id(min_live: u64) -> Option<u64> {
    (min_live > 1).then_some(min_live)
}

/// Per-aggregator ceiling on [`OrderflowAgg`]'s held pre-grid ticks, in ticks — the ONE path on
/// which a held tick can still be lost, and it counts and `warn!`s when it is.
///
/// Sized above what the producers can hand this aggregator while its grid is empty. The bulk
/// producer is the staged aggTrades backfill: `maybe_spawn_backfill` runs
/// `vike_binance::agg_trades_backfill_reported(…, max_pages = 300, …)` at `limit=1000` a page and a
/// symbol only ever receives ONE walk's worth of ticks — the `bf_spawned` gate is insert-only, and
/// the retry lane that can reopen it (`feed_lifecycle::BackfillRetries`) re-walks only a symbol
/// whose walk delivered NOTHING — so a symbol's ENTIRE backfill is
/// ≤ 300_000 ticks — all of which `core_sync` may hand over in ONE `ingest` once an aggregator
/// re-registers. The cap leaves ~1.6× headroom above that, while capping the buffer's own memory
/// (a `TradeTick` is 8+8+8+8+1 bytes of struct plus a heap `symbol` — order 100 MB at this cap, in
/// a GUI process) far below what an unbounded hold would reach on a live tape that never gets a
/// grid. The sibling ceiling one layer up is `core_sync::BF_PENDING_MAX_TICKS`.
pub const PENDING_MAX_TICKS: usize = 500_000;

/// Doubling log gate. Given a CUMULATIVE loss `count` and the count at which the next log line is
/// due, return `Some(next_threshold)` when this count deserves a line — `None` otherwise.
///
/// Both loss counters here are folded on a path that runs EVERY FRAME, so "log when it happens"
/// would be 60 lines/s in exactly the pathological case worth logging about (an aggregator whose
/// chart never gets bars, evicting on every drain). Doubling the threshold off the observed count
/// bounds the whole process's output to ≤ 64 lines per counter while still reporting the first
/// occurrence immediately and every order-of-magnitude escalation after it. Idempotent by
/// construction: after a line at `count`, the threshold is `2·count > count`, so re-checking an
/// UNCHANGED counter is always `None`.
fn due_to_log(count: u64, next_at: u64) -> Option<u64> {
    (count >= next_at).then(|| count.saturating_mul(2).max(1))
}

pub struct OrderflowAgg {
    tick_size: f64,
    cells: Vec<BTreeMap<i64, (f64, f64)>>, // index-aligned to chart bars
    /// SP3 Task B #1 (SP2 final-review finding B): `footprints()` used to rebuild the whole
    /// `Vec<FootprintBar>` from `cells` on EVERY call — O(full history), and `main.rs`'s
    /// per-window draw loop calls it once per open orderflow chart EVERY FRAME, so this went hot
    /// the moment backfill started filling history. Rebuilt only inside `ingest` (see `dirty`
    /// below); `footprints()` then just hands this out in O(1). `Arc` (not `Rc`) — nothing in
    /// this crate assumes single-threadedness the way vike-chart's UI-thread-only `ChartState`
    /// does, and the brief calls for `Arc` explicitly.
    cache: Arc<Vec<FootprintBar>>,
    /// SP3 TB-fix (post-Task-B review finding, MEDIUM): monotonic counter bumped by 1 every time
    /// `cache` is actually rebuilt — the exact same `if self.dirty` rebuild path in `ingest` that
    /// reallocates `cache`. `chart::ChartInputs::footprint_gen` carries this out to vike-chart,
    /// which keys its CVD recompute cache on `(generation, len)` instead of the footprint slice's
    /// own `(ptr, len)` (see `model.rs`'s `CvdCacheKey` doc). The pointer key was only sound
    /// because an unchanged aggregator hands out the SAME `Arc` every frame (Task B #1) — but that
    /// breaks down across a CVD toggle-off/on cycle: `cache` keeps rebuilding (new trades keep
    /// landing, more so once backfill raises reingest frequency) while `ChartState::cvd_cache`
    /// isn't read at all (CVD pane not drawn), so by the time CVD comes back on the allocator may
    /// have recycled a freed `Vec<FootprintBar>` buffer's address for the CURRENT one — an ABA
    /// hazard that a same-length coincidence would turn into a stale served frame. A monotonic
    /// counter can't be reused this way, so it closes the gap.
    generation: u64,
    /// True when `cells` changed since `cache` was last rebuilt. Set in `ingest` (and would be
    /// set on a `tick_size` change too, if this struct ever grows a setter for it — today
    /// `tick_size` is fixed at construction with no mutator, so that trigger is currently
    /// vacuous). Always false again by the time `ingest` returns: the rebuild happens inline
    /// there — the only `&mut self` entry point — rather than lazily inside `footprints`,
    /// because the real call site (`main.rs`'s per-window loop) reaches `OrderflowAgg` through
    /// `of_aggs: &HashMap<..>`, a SHARED borrow held alongside a concurrent `&mut` iteration over
    /// `self.wins` — `footprints(&self)` has no way to trigger a rebuild itself.
    dirty: bool,
    /// **Ticks that arrived while `bar_ots` was EMPTY** (#937 follow-up — see the module doc).
    /// Held in arrival order (chronological on both feeding lanes: the live drain is a time-ordered
    /// tape, and `backfill_agg_trades_backward` replays its pages `.rev()`, oldest first) and
    /// folded in by TIMESTAMP on the first `ingest` that carries a grid, so a held tick lands in
    /// its own bar rather than being appended to the newest one. Empty in steady state.
    pending: Vec<TradeTick>,
    /// `pending`'s ceiling — [`PENDING_MAX_TICKS`] in production. A field rather than a constant
    /// read at the use site so the eviction path is testable at a sane size (`set_pending_cap`,
    /// `#[cfg(test)]`) instead of only through a half-million-element fixture.
    pending_cap: usize,
    /// Cumulative held ticks evicted by `pending_cap` (oldest-first). `0` in every normal case.
    evicted: u64,
    /// Cumulative ticks discarded because they predate bar 0 of a NON-empty grid — unplaceable by
    /// construction (module doc), so discarding is right; this counter is what makes it not silent.
    dropped_before_grid: u64,
    /// Next `evicted` value that warrants a `warn!` — see [`due_to_log`].
    evict_warn_at: u64,
    /// Next `dropped_before_grid` value that warrants a `warn!` — see [`due_to_log`].
    drop_warn_at: u64,
}

impl OrderflowAgg {
    pub fn new(tick_size: f64) -> Self {
        OrderflowAgg {
            tick_size: if tick_size > 0.0 { tick_size } else { 1.0 },
            cells: Vec::new(),
            cache: Arc::new(Vec::new()),
            generation: 0,
            dirty: false,
            pending: Vec::new(),
            pending_cap: PENDING_MAX_TICKS,
            evicted: 0,
            dropped_before_grid: 0,
            // Both gates start at 1, so the FIRST loss of each kind reports immediately.
            evict_warn_at: 1,
            drop_warn_at: 1,
        }
    }
    pub fn tick_size(&self) -> f64 {
        self.tick_size
    }
    /// Monotonic footprint-cache generation (SP3 TB-fix — see the `generation` field's doc):
    /// bumped by 1 every time `ingest` actually rebuilds `cache`. `0` for a freshly-constructed
    /// aggregator that has never ingested a real change.
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Ticks currently HELD because no bar grid existed when they arrived (#937 follow-up — see
    /// the module doc). `0` in steady state; a non-zero value across frames means that chart's
    /// bars have not synced yet.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
    /// Cumulative held ticks evicted by the [`PENDING_MAX_TICKS`] bound (oldest-first) — the one
    /// remaining lossy path, and it is counted here and `warn!`ed rather than silent.
    pub fn evicted_ticks(&self) -> u64 {
        self.evicted
    }
    /// Cumulative ticks discarded for predating bar 0 of a NON-empty grid — unplaceable by
    /// construction, so a correct discard, but never a silent one.
    pub fn dropped_before_grid(&self) -> u64 {
        self.dropped_before_grid
    }
    pub fn ingest(&mut self, trades: &[TradeTick], bar_ots: &[i64]) {
        if self.cells.len() < bar_ots.len() {
            self.cells.resize_with(bar_ots.len(), BTreeMap::new);
            self.dirty = true;
        }
        if bar_ots.is_empty() {
            // No grid to place anything in YET — hold, never drop (#937 follow-up). `cells` is
            // untouched, so `dirty` stays false and the rebuild below is correctly skipped: a
            // held tick has changed no footprint, hence no `generation` bump either.
            self.hold(trades);
        } else {
            // A grid exists. Flush anything held FIRST — it is strictly older than this batch on
            // both lanes — then fold this batch. `place` buckets by TIMESTAMP, so a held tick
            // lands in ITS bar; nothing is appended to the newest bar just for being late.
            if !self.pending.is_empty() {
                let held = std::mem::take(&mut self.pending);
                for t in &held {
                    self.place(t, bar_ots);
                }
            }
            for t in trades {
                self.place(t, bar_ots);
            }
        }
        self.report_losses();
        // SP3 Task B #1: rebuild the cached snapshot only when something actually changed.
        // `ingest` runs every frame regardless (even with an empty `trades` slice — a quiet
        // market, or a frame between backfill pages; see `main.rs`'s `sync_from_core`), so
        // gating the rebuild on `dirty` is what keeps a no-op frame O(1) instead of O(history).
        if self.dirty {
            self.cache = Arc::new(self.build_footprints());
            // SP3 TB-fix: bump in the SAME branch that reallocates `cache`, never on a no-op
            // ingest — `generation` must track `cache`'s CONTENT changes 1:1, not `ingest` calls.
            self.generation += 1;
            self.dirty = false;
        }
    }
    /// Fold ONE tick into the bar that contains its timestamp. The only site that touches `cells`.
    fn place(&mut self, t: &TradeTick, bar_ots: &[i64]) {
        if t.size <= 0.0 {
            return;
        }
        // bar index = last bar with ot <= t.ts (partition_point).
        let idx = bar_ots.partition_point(|&ot| ot <= t.ts);
        if idx == 0 {
            // Older than bar 0 of a REAL grid: no future grid will contain it (`cells` is
            // index-aligned to a forward-only bar list), so discarding is the correct disposition
            // — but it is counted, and `report_losses` surfaces it. Holding instead would pin a
            // tick that can never be placed. Callers with an EMPTY grid never reach here: `ingest`
            // routes them to `hold`.
            self.dropped_before_grid += 1;
            return;
        }
        // `cells.len() >= bar_ots.len() >= idx >= 1` here (it is only ever grown, never shrunk, and
        // `ingest` grows it to `bar_ots.len()` before this runs), so the clamp and the emptiness
        // guard are both defensive: they keep a future caller that shrinks the grid out of a panic.
        if self.cells.is_empty() {
            return;
        }
        let i = (idx - 1).min(self.cells.len() - 1);
        let bucket = (t.price / self.tick_size).round() as i64;
        let e = self.cells[i].entry(bucket).or_insert((0.0, 0.0));
        let (b, s) = vike_orderflow::signed(t);
        e.0 += b;
        e.1 += s;
        self.dirty = true;
    }
    /// Hold a batch that arrived with no bar grid, evicting the OLDEST held ticks past
    /// `pending_cap`. Zero/negative-size ticks are `place` no-ops, so they are never held.
    ///
    /// Eviction takes the FRONT because the buffer accumulates oldest-first (see `pending`'s doc):
    /// dropping the front truncates the DEEPEST history and keeps the ticks adjacent to the live
    /// splice, which are the ones a chart's visible bars can still attach footprints to. Same
    /// rationale, and the same direction, as `core_sync::trim_pending_backfill` one layer up —
    /// deliberately re-stated here rather than called, so this module stays the self-contained
    /// primitive `core_sync` depends on and not the other way round.
    fn hold(&mut self, trades: &[TradeTick]) {
        self.pending.extend(trades.iter().filter(|t| t.size > 0.0).cloned());
        let over = self.pending.len().saturating_sub(self.pending_cap);
        if over > 0 {
            self.pending.drain(..over);
            self.evicted += over as u64;
        }
    }
    /// Surface the two loss counters on a doubling schedule (see [`due_to_log`]) — the whole point
    /// of the change: a tick this aggregator cannot keep must leave a trace.
    fn report_losses(&mut self) {
        if let Some(next) = due_to_log(self.evicted, self.evict_warn_at) {
            self.evict_warn_at = next;
            tracing::warn!(
                evicted_ticks = self.evicted,
                cap = self.pending_cap,
                held = self.pending.len(),
                "orderflow: held-tick cap reached while waiting for a bar grid — dropped the \
                 OLDEST held ticks; this chart's footprint/CVD will be truncated at its far end"
            );
        }
        if let Some(next) = due_to_log(self.dropped_before_grid, self.drop_warn_at) {
            self.drop_warn_at = next;
            tracing::warn!(
                dropped_ticks = self.dropped_before_grid,
                bars = self.cells.len(),
                "orderflow: discarded trades older than the chart's first bar — they cannot be \
                 placed in a forward-only bar grid (counted, not silent)"
            );
        }
    }
    /// Override the held-tick ceiling so the eviction path is exercisable without a
    /// half-million-element fixture. Test-only: production always uses [`PENDING_MAX_TICKS`].
    #[cfg(test)]
    fn set_pending_cap(&mut self, cap: usize) {
        self.pending_cap = cap;
    }
    /// The cached per-bar footprint snapshot — an O(1) `Arc` clone, current as of the last
    /// `ingest` call. See `cache`'s doc for why the rebuild can't happen lazily in here.
    pub fn footprints(&self) -> Arc<Vec<FootprintBar>> {
        self.cache.clone()
    }
    /// The actual `cells` → `Vec<FootprintBar>` fold, run only from `ingest` when `dirty`.
    fn build_footprints(&self) -> Vec<FootprintBar> {
        self.cells
            .iter()
            .enumerate()
            .map(|(i, m)| FootprintBar {
                bar_index: i as u64,
                cells: m
                    .iter()
                    .map(|(&bk, &(b, s))| PriceBin {
                        price: bk as f64 * self.tick_size,
                        buy_vol: b,
                        sell_vol: s,
                    })
                    .collect(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nice_tick_scales_with_price() {
        // ~2bps, rounded to a 1/2/5 nice step
        assert_eq!(nice_orderflow_tick(64000.0), 10.0); // 64000*0.0002=12.8 → nice 10
        assert_eq!(nice_orderflow_tick(3000.0), 0.5); // 0.6 → nice 0.5
        assert_eq!(nice_orderflow_tick(0.5), 0.0001); // 0.0001 → nice 0.0001
        assert_eq!(nice_orderflow_tick(0.0), 1.0); // guard
        assert!(nice_orderflow_tick(64000.0) > 0.0);
    }
    fn tk(ts: i64, p: f64, s: f64, ibm: bool) -> TradeTick {
        TradeTick {
            ts,
            local_ts: 0,
            price: p,
            size: s,
            is_buyer_maker: ibm,
            symbol: "BTCUSDT".into(),
        }
    }
    #[test]
    fn buckets_trades_into_bars_by_ot() {
        let mut a = OrderflowAgg::new(1.0);
        // bars open at ots 1000, 2000, 3000
        let ots = [1000, 2000, 3000];
        // trades: t@1500→bar0, t@2500→bar1, t@2999→bar1, t@3001→bar2, t@500→discarded (before
        // bar0 of a REAL grid: unplaceable by construction — but COUNTED, see the assert below).
        a.ingest(
            &[
                tk(1500, 100.0, 2.0, false),
                tk(2500, 101.0, 3.0, true),
                tk(2999, 101.0, 1.0, false),
                tk(3001, 102.0, 4.0, false),
                tk(500, 99.0, 9.0, false),
            ],
            &ots,
        );
        let fps = a.footprints();
        assert_eq!(fps.len(), 3);
        // bar0: buy 2@100
        assert_eq!(fps[0].cells, vec![PriceBin { price: 100.0, buy_vol: 2.0, sell_vol: 0.0 }]);
        // bar1: sell 3@101 + buy 1@101 → one bucket 101
        assert_eq!(fps[1].cells, vec![PriceBin { price: 101.0, buy_vol: 1.0, sell_vol: 3.0 }]);
        // bar2: buy 4@102
        assert_eq!(fps[2].cells, vec![PriceBin { price: 102.0, buy_vol: 4.0, sell_vol: 0.0 }]);
        // The t@500 tick predates bar 0 of a real grid, so it is discarded — but ACCOUNTED FOR.
        assert_eq!(a.dropped_before_grid(), 1, "a pre-bar-0 discard must be counted, not silent");
        assert_eq!(a.pending_len(), 0, "a real grid existed, so nothing is held");
    }
    #[test]
    fn incremental_ingest_accumulates() {
        let mut a = OrderflowAgg::new(1.0);
        let ots = [1000, 2000];
        a.ingest(&[tk(1500, 100.0, 1.0, false)], &ots);
        a.ingest(&[tk(1600, 100.0, 2.0, false)], &ots); // same bar0, later frame
        assert_eq!(
            a.footprints()[0].cells,
            vec![PriceBin { price: 100.0, buy_vol: 3.0, sell_vol: 0.0 }]
        );
    }
    #[test]
    fn zero_size_and_growth() {
        let mut a = OrderflowAgg::new(1.0);
        a.ingest(&[tk(1500, 100.0, 0.0, false)], &[1000]); // zero-size skipped
        assert_eq!(a.footprints()[0].cells.len(), 0);
    }

    // === SP3 Task B #1 (SP2 review finding B): footprints() cache =========================

    /// The cache must equal a fresh rebuild after `ingest`, AND a repeat `footprints()` call
    /// with no `ingest` in between must be O(1) — proved via `Arc::ptr_eq`, not just content
    /// equality, since two independently-rebuilt-but-equal `Vec`s would pass a content check
    /// while still failing to demonstrate the cache actually skipped the rebuild.
    #[test]
    fn footprints_cache_equals_fresh_rebuild_and_is_stable_across_repeat_calls() {
        let mut a = OrderflowAgg::new(1.0);
        let ots = [1000, 2000];
        a.ingest(&[tk(1500, 100.0, 2.0, false), tk(2500, 101.0, 1.0, true)], &ots);

        let first = a.footprints();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].cells, vec![PriceBin { price: 100.0, buy_vol: 2.0, sell_vol: 0.0 }]);
        assert_eq!(first[1].cells, vec![PriceBin { price: 101.0, buy_vol: 0.0, sell_vol: 1.0 }]);

        // repeat call, no ingest between: same allocation (served from cache), not a coincidental
        // content match from an independent rebuild.
        let second = a.footprints();
        assert!(Arc::ptr_eq(&first, &second), "an unchanged aggregator must hand out the SAME Arc");
        assert_eq!(*first, *second);
    }

    /// `ingest` must NOT rebuild the cache when nothing actually changed (empty trades, no new
    /// bars) — this is the exact shape `main.rs`'s per-frame `sync_from_core` drain calls
    /// `ingest` with on a quiet market. A genuine trade afterward must still invalidate it.
    #[test]
    fn footprints_cache_survives_a_no_op_ingest_but_invalidates_on_real_change() {
        let mut a = OrderflowAgg::new(1.0);
        let ots = [1000, 2000];
        a.ingest(&[tk(1500, 100.0, 2.0, false)], &ots);
        let before = a.footprints();

        a.ingest(&[], &ots); // no trades, same bar_ots → true no-op
        let after_noop = a.footprints();
        assert!(
            Arc::ptr_eq(&before, &after_noop),
            "a no-op ingest (empty trades) must not rebuild the footprint cache"
        );

        a.ingest(&[tk(1600, 100.0, 1.0, false)], &ots); // a real trade lands
        let after_real = a.footprints();
        assert!(
            !Arc::ptr_eq(&before, &after_real),
            "a real ingest must invalidate (rebuild) the footprint cache"
        );
        assert_eq!(
            after_real[0].cells,
            vec![PriceBin { price: 100.0, buy_vol: 3.0, sell_vol: 0.0 }]
        );
    }

    /// A zero-size (skipped) trade must NOT count as a change — it never touches `cells`.
    #[test]
    fn footprints_cache_survives_a_zero_size_trade() {
        let mut a = OrderflowAgg::new(1.0);
        a.ingest(&[tk(1500, 100.0, 1.0, false)], &[1000]);
        let before = a.footprints();
        a.ingest(&[tk(1600, 100.0, 0.0, false)], &[1000]); // zero-size: skipped, no real change
        let after = a.footprints();
        assert!(
            Arc::ptr_eq(&before, &after),
            "a zero-size trade must not rebuild the footprint cache"
        );
    }

    // === SP3 TB-fix (post-Task-B review finding, MEDIUM): monotonic `generation` ============

    /// `generation()` must bump exactly in lockstep with a genuine cache rebuild — mirrors
    /// `footprints_cache_survives_a_no_op_ingest_but_invalidates_on_real_change` above, from the
    /// generation side: a no-op `ingest` (empty trades, unchanged `bar_ots`) must NOT bump it, a
    /// real trade landing must, and it must keep climbing (never reset) across successive
    /// rebuilds. This is the property vike-chart's CVD cache leans on to trust "content changed"
    /// without re-deriving it from the footprint slice's own (ABA-able) address.
    #[test]
    fn generation_bumps_only_on_real_cache_rebuild() {
        let mut a = OrderflowAgg::new(1.0);
        let ots = [1000, 2000];
        assert_eq!(a.generation(), 0, "a fresh aggregator starts at generation 0");

        a.ingest(&[tk(1500, 100.0, 2.0, false)], &ots);
        assert_eq!(a.generation(), 1, "a real trade must bump the generation");

        a.ingest(&[], &ots); // no-op: empty trades, unchanged bar_ots
        assert_eq!(a.generation(), 1, "a no-op ingest must not bump the generation");

        a.ingest(&[tk(1600, 100.0, 0.0, false)], &ots); // zero-size: also a no-op
        assert_eq!(a.generation(), 1, "a zero-size trade must not bump the generation");

        a.ingest(&[tk(1700, 100.0, 1.0, false)], &ots); // a second real trade
        assert_eq!(a.generation(), 2, "generation must be monotonic across successive rebuilds");
    }

    /// Two `footprints()` snapshots taken across a real `ingest` must carry different
    /// generations — the exact property `ChartState::cvd_shared`'s `(generation, len)` key relies
    /// on to treat them as distinct even if the allocator happened to hand the second `Arc`'s
    /// backing `Vec<FootprintBar>` the SAME address the first's occupied (freed in between) —
    /// the toggle-off/on ABA scenario the review finding was about.
    #[test]
    fn footprints_across_an_ingest_carry_different_generations() {
        let mut a = OrderflowAgg::new(1.0);
        let ots = [1000, 2000];
        a.ingest(&[tk(1500, 100.0, 2.0, false)], &ots);
        let gen_before = a.generation();
        let _first = a.footprints();

        a.ingest(&[tk(1600, 101.0, 1.0, true)], &ots);
        let gen_after = a.generation();
        let _second = a.footprints();

        assert_ne!(
            gen_before, gen_after,
            "an ingest that changes content must change the generation"
        );
        assert_eq!(gen_after, gen_before + 1);
    }

    // === SP3 Task 3: background aggTrades backfill — accumulate, no double-count ============

    /// `sync_from_core` feeds a symbol's backfill batch and its live batch through TWO SEPARATE
    /// `ingest` calls (the backfill drain runs first, but on whatever frame(s) its batches
    /// arrive — generally not the same frame as the live drain). Because `ingest` has NO id
    /// dedup — it only ACCUMULATES, relying entirely on the CALLER to keep backfill ids
    /// strictly below every live id (`global-constraints.md`'s no-double-count invariant) — two
    /// `ingest` calls over a disjoint split of a trade set must land the exact same footprint as
    /// one `ingest` call over the union, INCLUDING at a "boundary bar": a bar that receives
    /// trades from both sides of the split. bar0 below is exactly that case: two backfill trades
    /// and one live trade all land in it. This is the property the whole feature leans on for
    /// correctness.
    #[test]
    fn backfill_then_live_ingest_equals_one_ingest_of_the_union() {
        let ots = [1000, 2000];
        // Backfill trades (ids conceptually < the live feed's earliest id — strictly older).
        let backfill = vec![tk(1200, 100.0, 3.0, true), tk(1500, 100.0, 1.0, false)];
        // Live trades (ids >= the live feed's earliest id): one more in the SAME bar0, plus one
        // in bar1.
        let live = vec![tk(1900, 100.0, 1.0, false), tk(2500, 101.0, 2.0, true)];

        let mut staged = OrderflowAgg::new(1.0);
        staged.ingest(&backfill, &ots); // backfill drains first (sync_from_core's doc)
        staged.ingest(&live, &ots); // ... then the live drain, same or a later frame

        let mut union_batch = backfill.clone();
        union_batch.extend(live.clone());
        let mut combined = OrderflowAgg::new(1.0);
        combined.ingest(&union_batch, &ots); // one ingest over the union, for comparison

        assert_eq!(*staged.footprints(), *combined.footprints());
        // Exact boundary-bar content: bar0 accumulated across BOTH `ingest` calls (2 backfill +
        // 1 live trade, all bucket 100) — nothing dropped, nothing double-counted.
        assert_eq!(
            staged.footprints()[0].cells,
            vec![PriceBin { price: 100.0, buy_vol: 2.0, sell_vol: 3.0 }]
        );
        assert_eq!(
            staged.footprints()[1].cells,
            vec![PriceBin { price: 101.0, buy_vol: 0.0, sell_vol: 2.0 }]
        );
    }

    /// `backfill_before_id`'s boundary (SP3-final-review M1 fix): the returned `before_id`
    /// EQUALS `min_live` — the pager it feeds (`backfill_agg_trades_backward`) already treats
    /// `before_id` as exclusive, so passing `min_live` through unmodified is what correctly
    /// includes the boundary trade (id `min_live - 1`) while staying disjoint from the live feed
    /// — with no room to page at all once there's no id below it (`min_live <= 1`). See the
    /// function's doc for the full no-double-count argument.
    #[test]
    fn backfill_before_id_equals_min_live_since_the_pager_bound_is_exclusive() {
        assert_eq!(backfill_before_id(10_000), Some(10_000));
        assert_eq!(backfill_before_id(2), Some(2));
        assert_eq!(backfill_before_id(1), None); // no room below id 1
        assert_eq!(backfill_before_id(0), None);
    }

    // === #937 follow-up: ticks preceding bar 0 are no longer silently dropped ================

    /// The defect, at its worst: an `ingest` whose `bar_ots` is EMPTY used to drop the ENTIRE
    /// batch — `partition_point` returns `0` for every tick, and `idx == 0` was a bare `continue`.
    /// #937 made the backfill lane's version of this observable with a `warn!` but left the loss
    /// in place; the live drain's version (`core_sync`'s `of_by_venue_symbol` loop, which passes
    /// `charts.get(key)…unwrap_or_default()`) was not even logged. Now the batch is HELD.
    #[test]
    fn ticks_arriving_before_any_bar_exists_are_held_not_dropped() {
        let mut a = OrderflowAgg::new(1.0);
        a.ingest(&[tk(1500, 100.0, 2.0, false), tk(2500, 101.0, 3.0, true)], &[]);

        assert_eq!(a.pending_len(), 2, "an empty bar grid must HOLD the batch, never drop it");
        assert_eq!(
            a.dropped_before_grid(),
            0,
            "holding is not a discard — nothing is counted lost"
        );
        assert_eq!(a.evicted_ticks(), 0);
        // Nothing was placed, so nothing was rendered: no cells, no cache rebuild, no generation
        // bump (the `(generation, len)` CVD key must not see a change that did not happen).
        assert!(a.footprints().is_empty());
        assert_eq!(a.generation(), 0, "a held tick changes no footprint, so no cache rebuild");
    }

    /// Held ticks must fold in at THEIR bar once the grid appears — the ordering requirement.
    /// Appending them to the newest bar (the naive "flush at the tail" fix) would smear a whole
    /// backfill page onto the forming bar and produce a CVD step that never happened.
    #[test]
    fn held_ticks_land_in_their_own_bar_once_the_grid_appears() {
        let mut a = OrderflowAgg::new(1.0);
        // Frame 1: chart deleted / not yet synced — no bars at all.
        a.ingest(
            &[
                tk(1500, 100.0, 2.0, false), // → bar0
                tk(2500, 101.0, 3.0, true),  // → bar1
                tk(3500, 102.0, 4.0, false), // → bar2
            ],
            &[],
        );
        assert_eq!(a.pending_len(), 3);

        // Frame 2: the chart's bars arrive. Nothing new on the tape this frame.
        let ots = [1000, 2000, 3000];
        a.ingest(&[], &ots);

        let fps = a.footprints();
        assert_eq!(fps.len(), 3);
        assert_eq!(fps[0].cells, vec![PriceBin { price: 100.0, buy_vol: 2.0, sell_vol: 0.0 }]);
        assert_eq!(fps[1].cells, vec![PriceBin { price: 101.0, buy_vol: 0.0, sell_vol: 3.0 }]);
        assert_eq!(fps[2].cells, vec![PriceBin { price: 102.0, buy_vol: 4.0, sell_vol: 0.0 }]);
        assert_eq!(a.pending_len(), 0, "the hold is released once it has been folded in");
        assert_eq!(a.dropped_before_grid(), 0);
        assert_eq!(a.generation(), 1, "the flush is a real content change: exactly one rebuild");
    }

    /// The whole point, stated as an equivalence: a batch that arrives DURING the grid outage must
    /// end up exactly where it would have landed had the outage never happened. This is the #937
    /// trigger end-to-end — a Data-manager delete drops `charts[k]` alongside `of_aggs[k]`, so a
    /// re-add briefly presents an aggregator with an empty grid — and it is the property a reader
    /// should check first if this file ever regresses.
    #[test]
    fn a_grid_outage_costs_nothing_versus_the_same_batch_ingested_in_grid() {
        let ots = [1000, 2000, 3000];
        let batch = vec![
            tk(1500, 100.0, 2.0, false),
            tk(2500, 101.0, 3.0, true),
            tk(2999, 101.0, 1.0, false),
            tk(3001, 102.0, 4.0, false),
        ];

        let mut outage = OrderflowAgg::new(1.0);
        outage.ingest(&batch, &[]); // the chart's bars are gone this frame
        outage.ingest(&[], &ots); // ... and back the next one

        let mut healthy = OrderflowAgg::new(1.0);
        healthy.ingest(&batch, &ots); // the same batch, no outage

        assert_eq!(*outage.footprints(), *healthy.footprints());
        assert_eq!(outage.dropped_before_grid(), 0);
        assert_eq!(outage.evicted_ticks(), 0);
    }

    /// Held ticks and a fresh batch in the SAME `ingest` must both place correctly — the held ones
    /// are flushed first (they are older on both feeding lanes), and accumulation across the two
    /// sources is plain addition in the shared bucket.
    #[test]
    fn a_flush_and_the_same_frames_fresh_trades_accumulate_in_one_bucket() {
        let mut a = OrderflowAgg::new(1.0);
        a.ingest(&[tk(1500, 100.0, 2.0, false)], &[]); // held
        let ots = [1000, 2000];
        a.ingest(&[tk(1600, 100.0, 5.0, false)], &ots); // flush + this frame's live tick

        assert_eq!(
            a.footprints()[0].cells,
            vec![PriceBin { price: 100.0, buy_vol: 7.0, sell_vol: 0.0 }]
        );
        assert_eq!(a.pending_len(), 0);
    }

    /// A held tick that is STILL older than bar 0 when the grid finally appears takes the
    /// unplaceable-by-construction path — discarded (no future grid can contain it: `cells` is
    /// index-aligned to a forward-only bar list), but COUNTED. This is also why the hold needs no
    /// separate age bound: staleness resolves itself into a counted discard at flush time.
    #[test]
    fn held_ticks_older_than_the_grid_that_finally_appears_are_counted_not_silent() {
        let mut a = OrderflowAgg::new(1.0);
        a.ingest(&[tk(500, 99.0, 9.0, false), tk(1500, 100.0, 2.0, false)], &[]);
        assert_eq!(a.pending_len(), 2);

        let ots = [1000, 2000]; // bar 0 opens AFTER the first held tick
        a.ingest(&[], &ots);

        assert_eq!(a.pending_len(), 0, "a flushed tick is never re-held — that would pin it");
        assert_eq!(a.dropped_before_grid(), 1, "the un-placeable held tick is counted");
        // The placeable one still landed.
        assert_eq!(
            a.footprints()[0].cells,
            vec![PriceBin { price: 100.0, buy_vol: 2.0, sell_vol: 0.0 }]
        );
    }

    /// The bound, behaving as documented: past `pending_cap` the OLDEST held ticks are evicted
    /// (keeping the ticks adjacent to the live splice, the ones a chart's visible bars can still
    /// attach footprints to) and the loss is counted. Run at a test-sized cap — the production
    /// `PENDING_MAX_TICKS` is deliberately above what any producer can emit.
    #[test]
    fn the_hold_bound_evicts_the_oldest_and_counts_the_loss() {
        let mut a = OrderflowAgg::new(1.0);
        a.set_pending_cap(3);
        // Five ticks, one per price, all inside bar0 of the grid that arrives later.
        a.ingest(
            &[
                tk(1100, 100.0, 1.0, false),
                tk(1200, 101.0, 1.0, false),
                tk(1300, 102.0, 1.0, false),
                tk(1400, 103.0, 1.0, false),
                tk(1500, 104.0, 1.0, false),
            ],
            &[],
        );
        assert_eq!(a.pending_len(), 3, "the hold is capped");
        assert_eq!(a.evicted_ticks(), 2, "and the overflow is counted, not silent");

        a.ingest(&[], &[1000, 2000]);
        // The two OLDEST (100.0, 101.0) are the ones gone; the newest three survived.
        assert_eq!(
            a.footprints()[0].cells,
            vec![
                PriceBin { price: 102.0, buy_vol: 1.0, sell_vol: 0.0 },
                PriceBin { price: 103.0, buy_vol: 1.0, sell_vol: 0.0 },
                PriceBin { price: 104.0, buy_vol: 1.0, sell_vol: 0.0 },
            ]
        );
    }

    /// The cap holds ACROSS calls too — a chart whose kline feed never delivers keeps receiving
    /// live ticks frame after frame, and that is exactly the case the bound exists for.
    #[test]
    fn the_hold_bound_holds_across_successive_gridless_frames() {
        let mut a = OrderflowAgg::new(1.0);
        a.set_pending_cap(4);
        for i in 0..10 {
            a.ingest(&[tk(1000 + i, 100.0, 1.0, false)], &[]);
        }
        assert_eq!(a.pending_len(), 4, "the buffer never exceeds the cap, however many frames run");
        assert_eq!(a.evicted_ticks(), 6);
    }

    /// A zero-size tick is a `place` no-op, so it must not consume hold capacity either.
    #[test]
    fn zero_size_ticks_are_not_held() {
        let mut a = OrderflowAgg::new(1.0);
        a.ingest(&[tk(1500, 100.0, 0.0, false), tk(1600, 100.0, 1.0, false)], &[]);
        assert_eq!(a.pending_len(), 1);
    }

    /// A grid-less `ingest` must stay a no-op for the render side: no cache rebuild, no generation
    /// bump, and the SAME `Arc` handed out — the invariant vike-chart's CVD cache key relies on.
    #[test]
    fn a_gridless_ingest_does_not_rebuild_the_cache_or_bump_the_generation() {
        let mut a = OrderflowAgg::new(1.0);
        let ots = [1000, 2000];
        a.ingest(&[tk(1500, 100.0, 2.0, false)], &ots);
        let before = a.footprints();
        let gen_before = a.generation();

        a.ingest(&[tk(1600, 100.0, 1.0, false)], &[]); // grid vanished for a frame: held
        assert!(
            Arc::ptr_eq(&before, &a.footprints()),
            "holding a tick changes no footprint, so the cache must not be rebuilt"
        );
        assert_eq!(a.generation(), gen_before);

        a.ingest(&[], &ots); // grid back: the held tick lands and the cache rebuilds once
        assert_eq!(a.generation(), gen_before + 1);
        assert_eq!(
            a.footprints()[0].cells,
            vec![PriceBin { price: 100.0, buy_vol: 3.0, sell_vol: 0.0 }]
        );
    }

    /// [`due_to_log`]'s doubling schedule: the FIRST loss of a kind reports immediately, an
    /// unchanged counter never re-reports (idempotence — this runs on a per-frame path), and the
    /// threshold doubles off the observed count so a pathological aggregator costs ≤ 64 lines
    /// instead of 60/s.
    #[test]
    fn due_to_log_reports_the_first_loss_then_doubles() {
        // Fresh counter: nothing lost yet, nothing to say.
        assert_eq!(due_to_log(0, 1), None);
        // First loss reports, and arms the next line at 2×.
        assert_eq!(due_to_log(1, 1), Some(2));
        // Unchanged counter, re-checked next frame: silent.
        assert_eq!(due_to_log(1, 2), None);
        assert_eq!(due_to_log(2, 2), Some(4));
        assert_eq!(due_to_log(3, 4), None);
        assert_eq!(due_to_log(9, 4), Some(18)); // a jump reports and re-arms off the JUMPED-TO count
        // Saturating, not panicking/looping, at the top of the range.
        assert_eq!(due_to_log(u64::MAX, u64::MAX), Some(u64::MAX));
    }
}
