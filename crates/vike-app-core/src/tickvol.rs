//! Client-side tick/volume interval bars: TradeStore trades → vike-orderflow builders
//! → vike_model::Bar series for the normal ChartState::sync path.
//!
//! `BarKind` classifies an interval string: the fixed kline set passes through unchanged
//! (the existing Binance kline feed owns those), `"<n>t"` / `"<x>v"` select the tick/volume
//! aggregators below. `TickVolAgg` wraps one `vike-orderflow` builder (`TickBarBuilder` or
//! `VolumeBarBuilder`) and converts each closed `OrderflowBar` to a `vike_model::Bar` — the
//! same conversion contract `vike_bridge_core::klines::kline_to_bar` uses for its own
//! not-sourced-from-this-feed fields (`funding`/`bid`/`ask`/`symbol` all `None`; `symbol` is
//! stamped later by the core runtime dispatch, same as every other bar producer).
//!
//! Feed routing (which venues/symbols get a `TickVolAgg` and where `ingest` is called from the
//! drained `TradeStore`) lives in `main.rs`: `App::ensure_feed_on` creates one aggregator per chart
//! key over the (once per symbol) trade feed, and `App::sync_from_core` drains the trade tape and
//! calls `ingest`/`forming` each frame (Task B5) — this module itself stays pure aggregation.
use vike_orderflow::{OrderflowBar, TickBarBuilder, VolumeBarBuilder};

/// The set of interval strings the live Binance kline feed already serves natively — anything
/// in this set is a `Kline` passthrough even if it would otherwise parse as `<n>t`/`<x>v`
/// (there's no such collision today, but `"1s"` deliberately proves the precedence rule).
const KLINE_SET: [&str; 5] = ["1s", "1m", "5m", "15m", "1h"];

/// One chart interval's *kind*: an existing venue kline interval, or a client-side tick-count /
/// volume-threshold aggregation built from the trade tape.
#[derive(Clone, PartialEq, Debug)]
pub enum BarKind {
    Kline(String),
    Tick(u32),
    Volume(f64),
}

impl BarKind {
    /// Parse a chart interval string. Precedence: the known kline set wins verbatim first
    /// (so `"1s"` is a `Kline`, never a would-be seconds-tick misparse); then `"<n>t"` with
    /// `n >= 1` is `Tick(n)`; then `"<x>v"` with `x > 0.0` is `Volume(x)`; anything else
    /// (including a malformed `"0t"`/`"-1v"`) falls back to `Kline` passthrough so an unknown
    /// string never panics or silently drops — it just won't match a live feed either.
    pub fn parse(s: &str) -> BarKind {
        if KLINE_SET.contains(&s) {
            return BarKind::Kline(s.to_string());
        }
        if let Some(n) = s.strip_suffix('t').and_then(|p| p.parse::<u32>().ok())
            && n >= 1
        {
            return BarKind::Tick(n);
        }
        if let Some(v) = s.strip_suffix('v').and_then(|p| p.parse::<f64>().ok())
            && v > 0.0
        {
            return BarKind::Volume(v);
        }
        BarKind::Kline(s.to_string())
    }
}

/// The one `vike-orderflow` builder backing a `TickVolAgg`, picked by `BarKind` at construction.
enum Builder {
    Tick(TickBarBuilder),
    Vol(VolumeBarBuilder),
}

/// Trade→bar aggregator for one (symbol, tick/volume interval) pair. `closed` accumulates every
/// bar the builder has emitted so far, in order — `Arc`-wrapped (like vike-core's own per-series
/// `closed`, see `runtime.rs`'s `Arc::make_mut(&mut series.closed).push(..)`) so the caller (Task
/// B5's `sync_from_core`) can feed it straight into `ChartState::sync`, which takes `&Arc<Vec<Bar>>`.
pub struct TickVolAgg {
    builder: Builder,
    pub closed: std::sync::Arc<Vec<vike_model::Bar>>,
    /// What [`TickVolAgg::new`] was built from, retained so [`TickVolAgg::mark_gap`] can build a
    /// FRESH builder. `vike_orderflow`'s `TickBarBuilder`/`VolumeBarBuilder` expose only
    /// `new`/`push`/`forming`/`from_trades` — no reset — and re-deriving the kind from an interval
    /// string at the gap site would put a second `BarKind::parse` in the tree.
    kind: BarKind,
    /// The tape-gap EPOCH this aggregator has already repaired to — the reader thread's
    /// per-`(venue, symbol)` counter as of the last [`TickVolAgg::mark_gap`] that acted. See that
    /// method for why the aggregator holds this rather than the caller.
    gap_epoch: u64,
}

/// Convert one closed/forming `OrderflowBar` to `vike_model::Bar`. `ts` is the bar's first-trade
/// timestamp (there's no separate "bar open" clock for tick/volume bars — the first contributing
/// trade defines it); `volume` is total traded size (`buy_vol + sell_vol`) since tick/volume
/// bars have no separate quote-volume source. `funding`/`bid`/`ask`/`symbol` all get the same
/// neutral `None` the live kline feed uses for the same not-sourced-here fields (see
/// `vike_bridge_core::klines::kline_to_bar`); `symbol` in particular is attached later by the
/// core runtime dispatch for every bar producer, not by the producer itself.
fn to_bar(ob: &OrderflowBar) -> vike_model::Bar {
    vike_model::Bar {
        ts: ob.first_ts,
        open: ob.o,
        high: ob.h,
        low: ob.l,
        close: ob.c,
        volume: ob.buy_vol + ob.sell_vol,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// The ONE builder construction, so `new` and `mark_gap` cannot pick different builders for the
/// same [`BarKind`]. `None` for `BarKind::Kline` — that interval is served by a venue feed.
fn build(kind: &BarKind) -> Option<Builder> {
    match kind {
        BarKind::Tick(n) => Some(Builder::Tick(TickBarBuilder::new(*n))),
        BarKind::Volume(v) => Some(Builder::Vol(VolumeBarBuilder::new(*v))),
        BarKind::Kline(_) => None,
    }
}

impl TickVolAgg {
    /// `None` for `BarKind::Kline` — that interval is served by the live venue feed, not this
    /// aggregator.
    pub fn new(kind: &BarKind) -> Option<TickVolAgg> {
        let builder = build(kind)?;
        Some(TickVolAgg {
            builder,
            closed: std::sync::Arc::new(Vec::new()),
            kind: kind.clone(),
            gap_epoch: 0,
        })
    }

    /// **A hole opened in this key's tape at `epoch`** — repair what the hole corrupted, and answer
    /// whether this call was the one that did it.
    ///
    /// `epoch` is a monotonic per-`(venue, symbol)` counter the producer bumps on every disclosed
    /// tape gap (`MdFrame::TapeGap`, and the reconnect bracket the session synthesizes locally). A
    /// COUNTER rather than a boolean because the producer runs on another thread and cannot reach
    /// these aggregators at all: they live on the frame thread, folded by
    /// [`core_sync::sync_from_core`](crate::core_sync::sync_from_core). Returning `bool` — `true`
    /// exactly when the epoch had moved — is what lets that fold DISCARD the batch it just drained,
    /// which is the half that matters: the drain and the epoch read are two separate instants, so a
    /// gap landing between them would otherwise fold a hole into a repaired aggregator for one
    /// frame. **Read the epoch AFTER the drain.**
    ///
    /// # The repair is NARROW here, and WHOLESALE on `OrderflowAgg`. That asymmetry is the design.
    ///
    /// Every bar already in `closed` closed BEFORE the hole and is exactly as correct as it was;
    /// only the FORMING bar straddles it — a `100t` bar that counted 60 pre-gap prints and then
    /// counts 40 post-gap ones is a hundred-trade bar over a window that lost trades. So the
    /// builder is rebuilt from [`TickVolAgg::kind`] and `closed` is kept whole. `OrderflowAgg`
    /// cannot do this: CVD is CUMULATIVE and its `cells` are index-aligned to the chart's bars with
    /// no per-trade dedup, so a hole understates one bar's cells and every CVD value after it.
    pub fn mark_gap(&mut self, epoch: u64) -> bool {
        if epoch == self.gap_epoch {
            return false;
        }
        self.gap_epoch = epoch;
        if let Some(fresh) = build(&self.kind) {
            self.builder = fresh;
        }
        true
    }

    /// Fold `trades` (in order) into the builder, appending every newly-closed bar to `closed`.
    pub fn ingest(&mut self, trades: &[vike_model::TradeTick]) {
        for t in trades {
            match &mut self.builder {
                Builder::Tick(b) => {
                    if let Some(ob) = b.push(t) {
                        std::sync::Arc::make_mut(&mut self.closed).push(to_bar(&ob));
                    }
                }
                Builder::Vol(b) => {
                    for ob in b.push(t) {
                        std::sync::Arc::make_mut(&mut self.closed).push(to_bar(&ob));
                    }
                }
            }
        }
    }

    /// Snapshot of the builder's open (not-yet-closed) bar — the chart's forming bar. Pure read.
    pub fn forming(&self) -> Option<vike_model::Bar> {
        match &self.builder {
            Builder::Tick(b) => b.forming().map(|ob| to_bar(&ob)),
            Builder::Vol(b) => b.forming().map(|ob| to_bar(&ob)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn t(ts: i64, price: f64, size: f64, ibm: bool) -> vike_model::TradeTick {
        vike_model::TradeTick {
            ts,
            local_ts: 0,
            price,
            size,
            is_buyer_maker: ibm,
            symbol: "BTCUSDT".into(),
        }
    }
    #[test]
    fn parse_barkind() {
        assert_eq!(BarKind::parse("1s"), BarKind::Kline("1s".into())); // known kline set wins over 's' suffix logic
        assert_eq!(BarKind::parse("1m"), BarKind::Kline("1m".into()));
        assert_eq!(BarKind::parse("100t"), BarKind::Tick(100));
        assert_eq!(BarKind::parse("10v"), BarKind::Volume(10.0));
        assert_eq!(BarKind::parse("weird"), BarKind::Kline("weird".into()));
    }
    #[test]
    fn tick_agg_builds_bars_and_forming() {
        let mut a = TickVolAgg::new(&BarKind::Tick(2)).unwrap();
        a.ingest(&[t(1, 100.0, 1.0, false), t(2, 101.0, 2.0, true), t(3, 99.0, 3.0, false)]);
        assert_eq!(a.closed.len(), 1);
        let b = &a.closed[0];
        assert_eq!(
            (b.ts, b.open, b.high, b.low, b.close, b.volume),
            (1, 100.0, 101.0, 100.0, 101.0, 3.0)
        );
        let f = a.forming().unwrap();
        assert_eq!((f.ts, f.open, f.volume), (3, 99.0, 3.0));
        assert!(TickVolAgg::new(&BarKind::Kline("1m".into())).is_none());
    }
    /// The NARROW repair: a disclosed tape gap discards the FORMING bar (which straddles the hole)
    /// and keeps every bar that closed before it. The second call at the SAME epoch answers
    /// `false` and touches nothing — the fold runs every frame, so an idempotent epoch is what
    /// keeps it from rebuilding the builder sixty times a second.
    #[test]
    fn a_tape_gap_discards_only_the_forming_bar() {
        let mut a = TickVolAgg::new(&BarKind::Tick(2)).unwrap();
        a.ingest(&[t(1, 100.0, 1.0, false), t(2, 101.0, 2.0, true), t(3, 99.0, 3.0, false)]);
        assert_eq!(a.closed.len(), 1, "one closed bar plus a forming one");
        assert!(a.forming().is_some());

        assert!(a.mark_gap(1), "the epoch moved — this call repaired");
        assert_eq!(a.closed.len(), 1, "bars that closed BEFORE the hole are untouched");
        assert!(a.forming().is_none(), "the bar straddling the hole is gone");

        assert!(!a.mark_gap(1), "same epoch: nothing to repair");
        a.ingest(&[t(4, 98.0, 1.0, false)]);
        assert_eq!(a.forming().unwrap().volume, 1.0, "post-gap prints start a clean bar");
    }

    #[test]
    fn volume_agg_splits() {
        let mut a = TickVolAgg::new(&BarKind::Volume(2.0)).unwrap();
        a.ingest(&[t(1, 100.0, 5.0, false)]);
        assert_eq!(a.closed.len(), 2); // 5.0 → two 2.0 bars + 1.0 forming
        assert_eq!(a.forming().unwrap().volume, 1.0);
    }
}
