//! Microstructure **studies** — the tick-driven twin of [`crate::indicators::Active`].
//!
//! `vike-orderflow` owns the compute (VPIN, depth/inverse-distance book imbalance,
//! order-to-trade ratio); this module is the chart-side GUI adapter that turns those
//! TICK-driven folds into BAR-INDEXED oscillator-pane series, exactly the shape
//! [`crate::render::render_oscillator`] already draws. Paint state (colours, widths,
//! bands, visibility) reuses [`crate::indicators::OutputLine`]/[`crate::indicators::BandLevel`]
//! verbatim, and the parameter surface reuses `vike_indicators::ParamSpec`, so a study
//! pane is indistinguishable from an indicator pane to the renderer and to the
//! settings dialogs. Purely additive: nothing here runs unless the app builds an
//! [`ActiveStudy`], so every existing render path is byte-identical.
//!
//! # The tick→bar mapping (the load-bearing design decision)
//!
//! The orderflow studies advance on their OWN clocks — VPIN on volume buckets, OTR on
//! event-time windows, imbalance on every book update — none of which are bar
//! boundaries. Rather than re-deriving a study per bar (which would need the full tick
//! history and would be O(history) every frame), each [`ActiveStudy`] keeps ONE
//! incremental study instance folded over the live tick stream and **samples** it:
//!
//! - the app feeds ticks as they arrive ([`ActiveStudy::on_trade`] / [`ActiveStudy::on_book`]);
//! - when a chart bar closes, [`ActiveStudy::close_bar`] snapshots the study's current
//!   **committed** value and appends it to the series — one value per bar, so the series
//!   is index-aligned to `ChartState::bars` like every other oscillator;
//! - **carry-last**: a bar during which the study produced no new committed value
//!   repeats the previous sample (the study's value is a level, not a per-bar event —
//!   a flat segment is the truthful reading);
//! - **pre-warmup is NaN**: bars closed before the study's first committed value sample
//!   `f64::NAN`, which `seg_line` already skips, so the pane simply starts where the
//!   study does;
//! - **forming vs closed** mirrors [`crate::indicators::Active::update`]: the live
//!   forming bar gets a SPECULATIVE tail sample from the study's *interim* read (VPIN's
//!   partial bucket, OTR's open window), appended by [`ActiveStudy::sync`] and truncated
//!   away on the next call. Committed samples are never speculative.
//!
//! [`ActiveStudy::sync`] is the per-frame reconciler: it makes the series exactly
//! `closed_len` committed samples long (padding with carry-last when the app closed
//! several bars between frames — e.g. a history seed — and truncating on a shorter
//! series, i.e. a symbol/interval swap) and then appends the forming tail.
//!
//! # Book gating (no fabricated values)
//!
//! [`StudyKind::BookImbalance`] and [`StudyKind::Otr`] are BOOK-dependent
//! ([`StudyMeta::needs_book`]). Until the chart has actually fed a book snapshot for
//! this symbol, such a study's series stays **empty** — the pane renders nothing rather
//! than a line of fabricated/NaN values. [`StudyKind::Vpin`] needs trades only and is
//! never gated. See [`ActiveStudy::is_empty`].

use egui::Color32;
use vike_indicators::{OutSpec, OutputStyle, ParamSpec, coerce};
use vike_marketdata::{L2Book, TradeTick};
use vike_orderflow::{ImbalanceTracker, OtrConfig, OtrTracker, Vpin};
use vike_ui_theme::metrics::stroke;
use vike_ui_theme::value::chart;

use crate::indicators::{BandLevel, LineDash, OutputLine, ob_fill_default, os_fill_default};
use crate::panes::PaneKey;

/// The three microstructure studies renderable as oscillator panes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum StudyKind {
    /// Volume-synchronized probability of informed trading (`vike_orderflow::Vpin`).
    /// Trades only; ∈ [0, 1].
    Vpin,
    /// Depth-N (optionally inverse-distance-weighted) book imbalance
    /// (`vike_orderflow::ImbalanceTracker`); ∈ [−1, 1], + = bid pressure.
    BookImbalance,
    /// Order-to-trade ratio over event-time windows (`vike_orderflow::OtrTracker`);
    /// unbounded above, 0 = message/trade parity.
    Otr,
}

/// Static description of a study — the [`vike_indicators::IndicatorMeta`] analogue,
/// minus the compute constructors (a study's state is built by [`StudyState::build`]
/// from the same param vector).
pub struct StudyMeta {
    /// stable registry key (workspace persistence / lookup)
    pub name: &'static str,
    /// picker label
    pub pretty: &'static str,
    pub kind: StudyKind,
    pub outputs: &'static [OutSpec],
    /// reference levels drawn as dashed hlines in the pane
    pub bands: &'static [f64],
    /// parameter surface, in the order [`StudyState::build`] reads them
    pub params: &'static [ParamSpec],
    /// `true` ⇒ this study renders NOTHING until an L2 book has been fed for the
    /// chart's symbol (see the module docs' book-gating contract).
    pub needs_book: bool,
}

const VPIN_OUT: &[OutSpec] = &[OutSpec { name: "vpin", style: OutputStyle::Line }];
const IMB_OUT: &[OutSpec] = &[OutSpec { name: "imbalance", style: OutputStyle::Line }];
const OTR_OUT: &[OutSpec] = &[OutSpec { name: "otr", style: OutputStyle::Line }];

const VPIN_PARAMS: &[ParamSpec] = &[
    ParamSpec { name: "bucket volume", default: 1000.0, min: 1.0, max: 1e12, step: 1.0 },
    ParamSpec { name: "window", default: 50.0, min: 2.0, max: 1000.0, step: 1.0 },
    // 0 = quote-midpoint classification (feeds with no aggressor flag), 1 = aggressor flag.
    ParamSpec { name: "aggressor flag", default: 1.0, min: 0.0, max: 1.0, step: 1.0 },
];
const IMB_PARAMS: &[ParamSpec] = &[
    ParamSpec { name: "depth", default: 5.0, min: 1.0, max: 100.0, step: 1.0 },
    // 0 = plain depth-N sum, 1 = inverse-distance weighted.
    ParamSpec { name: "weighted", default: 0.0, min: 0.0, max: 1.0, step: 1.0 },
];
const OTR_PARAMS: &[ParamSpec] = &[
    ParamSpec { name: "depth", default: 10.0, min: 1.0, max: 100.0, step: 1.0 },
    ParamSpec { name: "window ms", default: 100.0, min: 1.0, max: 60_000.0, step: 1.0 },
];

static STUDIES: &[StudyMeta] = &[
    StudyMeta {
        name: "vpin",
        pretty: "VPIN (order-flow toxicity)",
        kind: StudyKind::Vpin,
        outputs: VPIN_OUT,
        bands: &[0.2, 0.5, 0.8],
        params: VPIN_PARAMS,
        needs_book: false,
    },
    StudyMeta {
        name: "book_imbalance",
        pretty: "Book imbalance (depth-N)",
        kind: StudyKind::BookImbalance,
        outputs: IMB_OUT,
        bands: &[-0.5, 0.0, 0.5],
        params: IMB_PARAMS,
        needs_book: true,
    },
    StudyMeta {
        name: "otr",
        pretty: "Order-to-trade ratio",
        kind: StudyKind::Otr,
        outputs: OTR_OUT,
        bands: &[0.0],
        params: OTR_PARAMS,
        needs_book: true,
    },
];

/// Every renderable study, in picker order.
pub fn study_registry() -> &'static [StudyMeta] {
    STUDIES
}

/// Registry lookup by stable [`StudyMeta::name`].
pub fn get_study(name: &str) -> Option<&'static StudyMeta> {
    STUDIES.iter().find(|m| m.name == name)
}

/// The live compute state of one study — the tick-fed fold whose value
/// [`ActiveStudy`] samples at bar boundaries. One variant per [`StudyKind`].
enum StudyState {
    Vpin(Box<Vpin>),
    Imbalance(ImbalanceTracker),
    Otr(Box<OtrTracker>),
}

impl StudyState {
    /// Build a fresh state from a `coerce`d parameter vector (the `make_with` analogue).
    fn build(kind: StudyKind, p: &[f64]) -> StudyState {
        let g = |i: usize| p.get(i).copied().unwrap_or(f64::NAN);
        match kind {
            StudyKind::Vpin => StudyState::Vpin(Box::new(Vpin::with_params(
                g(0),
                g(1).max(2.0) as usize,
                g(2) >= 0.5,
            ))),
            StudyKind::BookImbalance => {
                StudyState::Imbalance(ImbalanceTracker::new(g(0).max(1.0) as usize, g(1) >= 0.5))
            }
            StudyKind::Otr => StudyState::Otr(Box::new(OtrTracker::new(OtrConfig {
                depth: g(0).max(1.0) as usize,
                window_ms: g(1).max(1.0) as i64,
            }))),
        }
    }

    /// The study's latest COMMITTED value (what a closing bar samples), or `None`
    /// before the study has produced one.
    fn committed(&self) -> Option<f64> {
        match self {
            StudyState::Vpin(v) => v.committed(),
            // A book imbalance IS instantaneous: its "committed" value is the last
            // good reading (the tracker already carries the last GOOD value across a
            // momentarily unreadable book).
            StudyState::Imbalance(t) => t.last(),
            StudyState::Otr(t) => t.last().map(|s| s.otr),
        }
    }

    /// The study's INTERIM value — the forming-bar read, folding the study's own
    /// partial/open unit (VPIN's unfinished volume bucket, OTR's open window) without
    /// mutating state. Falls back to [`StudyState::committed`] when the study has no
    /// distinct interim notion (imbalance).
    fn interim(&self) -> Option<f64> {
        match self {
            StudyState::Vpin(v) => v.interim().or_else(|| v.committed()),
            StudyState::Imbalance(t) => t.last(),
            StudyState::Otr(t) => t.forming().map(|s| s.otr).or_else(|| t.last().map(|s| s.otr)),
        }
    }
}

/// Per-window active microstructure study: the GUI paint state around one tick-fed
/// [`StudyState`], sampled into a bar-aligned series. The [`crate::indicators::Active`]
/// twin — same output-line/band/visibility surface, same oscillator render path — but
/// advanced by ticks rather than by bars. See the module docs for the mapping contract.
pub struct ActiveStudy {
    /// stable identity — shares [`crate::indicators::Active`]'s uid space, so
    /// [`PaneKey::Study`] keys a study pane exactly like an indicator pane.
    pub uid: u64,
    pub spec: &'static StudyMeta,
    /// one line per `spec.outputs` (all studies are single-line today)
    pub outputs: Vec<OutputLine>,
    /// live parameter values, index-aligned to `spec.params`
    pub params: Vec<f64>,
    pub visible: bool,
    pub show_bands: bool,
    pub bands: Vec<BandLevel>,
    pub show_ob_os_fill: bool,
    pub ob_fill: Color32,
    pub os_fill: Color32,
    state: StudyState,
    /// number of CLOSED-bar samples in `outputs[*].series` (the speculative forming
    /// tail, when present, sits at index `committed_len`).
    committed_len: usize,
    /// last committed sample — the carry-last value a quiet bar repeats. `NaN` until
    /// the study's first committed value.
    carry: f64,
    /// has any L2 book ever been fed? Gates `needs_book` studies (see module docs).
    saw_book: bool,
    /// trades seen since the last book snapshot — OTR's per-window trade denominator,
    /// which `OtrTracker::push` consumes at the next [`ActiveStudy::on_book`]. Inert for
    /// the other studies.
    pending_trades: u32,
}

impl ActiveStudy {
    /// Build a study at its registry-default parameters, with no samples yet.
    pub fn new(uid: u64, spec: &'static StudyMeta) -> Self {
        let params: Vec<f64> = spec.params.iter().map(|p| p.default).collect();
        Self::with_params(uid, spec, params)
    }

    /// Build a study at an explicit (raw, un-`coerce`d) parameter vector.
    pub fn with_params(uid: u64, spec: &'static StudyMeta, raw: Vec<f64>) -> Self {
        let params = coerce(spec.params, &raw);
        let outputs = spec
            .outputs
            .iter()
            .map(|o| OutputLine {
                name: o.name,
                series: Vec::new(),
                style: o.style,
                color: chart::SERIES_1,
                width: stroke::LINE, // oscillator default, matching `indicators::default_width`
                line_style: LineDash::Solid,
                visible: true,
            })
            .collect();
        // No colour chosen: the level paints in the theme's level line (see `BandLevel`).
        let bands =
            spec.bands.iter().map(|&value| BandLevel { value, color: None, show: true }).collect();
        let state = StudyState::build(spec.kind, &params);
        ActiveStudy {
            uid,
            spec,
            outputs,
            params,
            visible: true,
            show_bands: true,
            bands,
            show_ob_os_fill: false,
            ob_fill: ob_fill_default(),
            os_fill: os_fill_default(),
            state,
            committed_len: 0,
            carry: f64::NAN,
            saw_book: false,
            pending_trades: 0,
        }
    }

    /// This study's sub-pane identity — the same [`PaneKey::Study`] variant indicator
    /// oscillators use, keyed by the shared uid space.
    pub fn pane_key(&self) -> PaneKey {
        PaneKey::Study(self.uid)
    }

    /// `true` while this study must render nothing: a book-dependent study that has
    /// never seen a book (see the module docs' gating contract).
    pub fn is_empty(&self) -> bool {
        self.spec.needs_book && !self.saw_book
    }

    /// Fold one trade. Only VPIN consumes the price/size; OTR counts it toward the
    /// current window's trade denominator (flushed on the next [`ActiveStudy::on_book`]);
    /// imbalance ignores trades entirely.
    pub fn on_trade(&mut self, t: &TradeTick, mid: Option<f64>) {
        match &mut self.state {
            StudyState::Vpin(v) => {
                v.push(t, mid);
            }
            StudyState::Imbalance(_) => {}
            // `OtrTracker::push` takes the trade count observed SINCE the previous
            // snapshot, so trades accumulate here and flush on the next book update.
            StudyState::Otr(_) => self.pending_trades = self.pending_trades.saturating_add(1),
        }
        self.refresh_carry();
    }

    /// Fold one L2 book snapshot. Flips the book gate on (so a `needs_book` study
    /// starts rendering from here). `ts` is the snapshot's epoch-ms — OTR's event-time
    /// window anchor; the other studies ignore it.
    pub fn on_book(&mut self, ts: i64, book: &L2Book) {
        self.saw_book = true;
        match &mut self.state {
            StudyState::Vpin(_) => {}
            StudyState::Imbalance(t) => {
                t.update(book);
            }
            StudyState::Otr(o) => {
                o.push(ts, book, self.pending_trades);
                self.pending_trades = 0;
            }
        }
        self.refresh_carry();
    }

    /// Latch the study's newest committed value into `carry`. A study that has not
    /// produced one yet leaves `carry` at `NaN` (the pre-warmup sample).
    fn refresh_carry(&mut self) {
        if let Some(v) = self.state.committed() {
            self.carry = v;
        }
    }

    /// Sample the study for ONE closed bar: append the carry-last committed value
    /// (`NaN` before the study's first committed value). Idempotent w.r.t. ticks — it
    /// reads state, never advances it.
    pub fn close_bar(&mut self) {
        let v = self.carry;
        for line in &mut self.outputs {
            line.series.truncate(self.committed_len); // drop any speculative tail
            line.series.push(v);
        }
        self.committed_len += 1;
    }

    /// Per-frame reconciler (the [`crate::indicators::Active::update`] analogue).
    ///
    /// Makes the committed series exactly `closed_len` long — padding with carry-last
    /// samples when bars closed between frames, truncating when the chart's closed
    /// series shrank (symbol/interval swap, reload) — then appends the SPECULATIVE
    /// forming-bar sample (the study's interim read) when `forming` is set. The
    /// speculative value is dropped again at the start of the next call, so it is
    /// never mistaken for a committed sample.
    ///
    /// A book-gated study with no book yet ([`ActiveStudy::is_empty`]) is cleared to an
    /// empty series instead — the pane renders nothing.
    pub fn sync(&mut self, closed_len: usize, forming: bool) {
        if self.is_empty() {
            for line in &mut self.outputs {
                line.series.clear();
            }
            self.committed_len = 0;
            return;
        }
        if closed_len < self.committed_len {
            self.committed_len = closed_len;
            for line in &mut self.outputs {
                line.series.truncate(closed_len);
            }
        }
        while self.committed_len < closed_len {
            self.close_bar();
        }
        for line in &mut self.outputs {
            line.series.truncate(self.committed_len);
        }
        if forming {
            let v = self.state.interim().unwrap_or(self.carry);
            for line in &mut self.outputs {
                line.series.push(v);
            }
        }
    }

    /// Reconfigure to a new (raw) parameter vector and REBUILD the tick state. Unlike
    /// an indicator refold there is no stored tick history to replay, so the series is
    /// cleared and the study re-warms from the next tick — the honest reading, since a
    /// re-parameterized VPIN/OTR window genuinely knows nothing yet. Paint attributes
    /// (colour/width/bands) survive, exactly like `Active::set_params`.
    pub fn set_params(&mut self, raw: Vec<f64>) {
        self.params = coerce(self.spec.params, &raw);
        self.state = StudyState::build(self.spec.kind, &self.params);
        self.carry = f64::NAN;
        self.pending_trades = 0;
        self.committed_len = 0;
        for line in &mut self.outputs {
            line.series.clear();
        }
    }

    /// The committed (non-speculative) series length — bars sampled so far.
    pub fn committed_len(&self) -> usize {
        self.committed_len
    }

    /// The single output series (studies are single-line today); empty when gated.
    pub fn series(&self) -> &[f64] {
        self.outputs.first().map(|l| l.series.as_slice()).unwrap_or(&[])
    }
}

/// Pane registration: append each study's [`PaneKey`] to the present-pane list, in
/// order, skipping the ones that must not render — hidden studies and book-gated
/// studies with no book. The caller (`vike-app-core`'s `WinState::present_sub_panes`) folds the result
/// into the authored pane order it already builds for Volume/CVD/indicator panes, and
/// `PaneFractions::layout` sizes them all as peers.
pub fn push_study_panes(present: &mut Vec<PaneKey>, studies: &[ActiveStudy]) {
    for s in studies {
        if s.visible && !s.is_empty() {
            present.push(s.pane_key());
        }
    }
}

#[path = "studies_tests.rs"]
#[cfg(test)]
mod studies_tests;
