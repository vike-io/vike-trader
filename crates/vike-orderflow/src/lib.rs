//! vike-orderflow — pure trade-aggregation core (chart Phase 5, SP1).
//! Folds `vike_model::TradeTick` streams into orderflow primitives. No I/O, no
//! rendering, no threading. See docs/superpowers/specs/2026-07-08-vike-orderflow-design.md.
//!
//! Five builders, one file each: `classify` (`Side`/`classify`/`signed` — the load-bearing
//! semantic, `is_buyer_maker=false`→Buy/`true`→Sell, `delta = buy_vol − sell_vol` everywhere
//! below); `bars` (`OrderflowBar` + `TickBarBuilder`/`VolumeBarBuilder`/`DollarBarBuilder`,
//! the latter two splitting a trade proportionally across a threshold boundary — volume and
//! cumulative price·size respectively; plus the opt-in López-de-Prado information-driven
//! `ImbalanceBarBuilder`/`RunsBarBuilder` over a `FlowUnit` {Tick, Volume, Dollar} — imbalance
//! closes when |Σ ±unit| ≥ threshold, runs when max(buy-run, sell-run) ≥ threshold, the whole
//! crossing trade included (no split); fixed-threshold deterministic form, the EWMA-adaptive
//! threshold the noted extension); `cvd` (`CvdAccumulator`, running Σ(buy−sell));
//! `profile` (`VolumeProfileBuilder` — price-bucketed `PriceBin`s, POC, 70% value area);
//! `footprint` (`FootprintBuilder` — a per-bar `PriceBin` grid).
//!
//! Microstructure studies (steal/microstructure lane — mechanisms from the VisualHFT
//! studies + standard literature, independent implementations): `vpin` (`Vpin` —
//! fixed-volume-bucket order-flow toxicity, rolling mean of last-N bucket imbalances via
//! an O(1) rolling sum, forming/committed reads — plus `BvcVpin`, the aggressor-free
//! Easley/López de Prado/O'Hara (2012) VPIN with Bulk Volume Classification `Φ(ΔP/σ)` and a
//! tick-rule option, `libm`-erf `norm_cdf`); `imbalance` (depth-N and
//! inverse-distance-weighted book imbalance over `vike_model::L2Book` + `ImbalanceTracker`);
//! `otr` (`OtrTracker` — order-to-trade ratio from consecutive top-N snapshot diffs per
//! event-time window); `p2` (`P2Quantile` — Jain–Chlamtac P² single-quantile estimator,
//! O(1) space, for future adaptive thresholds).
//!
//! Gate discipline per builder: hand-computed expected-value tests (the real oracle — no
//! CPython twin here) + invariant/zero-skip tests + streaming↔batch bit-parity
//! (`f64::to_bits`; batch is a genuinely separate slice computation, not `push` in disguise).
//! Book-fed studies (`imbalance`/`otr`) are pure functions/scripted-snapshot gated instead
//! (no trade-slice batch twin exists for them); `p2` is gated against an exact full-sort
//! quantile oracle + the published paper example.
//!
//! SP2 renders this, SP3 backfills it — both out of scope here.
pub mod bars;
pub mod classify;
pub mod cvd;
pub mod footprint;
pub mod imbalance;
pub mod otr;
pub mod p2;
pub mod profile;
pub mod vpin;
pub use bars::{
    DollarBarBuilder, FlowUnit, ImbalanceBarBuilder, OrderflowBar, RunsBarBuilder, TickBarBuilder,
    VolumeBarBuilder,
};
pub use classify::{classify, signed, Side};
pub use cvd::CvdAccumulator;
pub use footprint::{FootprintBar, FootprintBuilder};
pub use imbalance::{depth_imbalance, weighted_depth_imbalance, ImbalanceTracker};
pub use otr::{OtrConfig, OtrSample, OtrTracker};
pub use p2::P2Quantile;
pub use profile::{PriceBin, VolumeProfile, VolumeProfileBuilder};
pub use vpin::{
    bvc_buy_fraction, norm_cdf, tick_rule_buy_fraction, BvcVpin, VolumeClassifier, Vpin,
    VpinBucket, DEFAULT_VPIN_WINDOW,
};
