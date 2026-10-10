//! The CROSS-EXCHANGE maker MOUNT — stand a [`vike_mm::XemmMaker`] up on the production live core,
//! resting on one venue and hedging on another.
//!
//! A SIBLING of the single-venue maker mount, not an extension: [`crate::MakerMountConfig`] is ONE
//! `(venue, symbol)` and its paper builder's tripwire (`spec.legs.is_empty()`, in
//! [`crate::build_paper_strategy_core_with`]) refuses multi-symbol mounts, because a single-symbol
//! `PaperExecutionClient` stamps ITS OWN symbol on every fill — a two-leg rehearsal would book both
//! legs under one symbol and conceal a live misroute.
//!
//! This builder meets that by CONSTRUCTION: **two engines, two single-symbol paper books, one per
//! venue** ([`vike_core::spawn_core_multi`] takes one client TYPE, not one instance), each with its
//! own `Account`/`RiskGate`/[`vike_model::FeeSchedule`], so a misrouted order lands in the WRONG
//! log. Not `vike_paper::MultiPaperExecutionClient`: it routes by `request.symbol` only, never
//! `request.venue`.
//!
//! # ⚠ The live path needs FEEDS the caller wires
//!
//! Like [`crate::build_live_maker_core`], nothing here spawns a feed. Three inbound streams on the
//! one `CoreHandle`:
//!
//! 1. the MAKER venue's L1/L2 — `passive_clamp`'s anchor;
//! 2. the TAKER venue's L1 — delivered to `on_reference_quote` via [`vike_core::MountLeg::at`];
//! 3. a periodic `LiveSchedule` under the mount's id — the ONE lane that fires when both venues go
//!    silent; without it a total outage leaves the quotes resting.

use std::sync::Arc;

use vike_core::{CoreConfig, CoreHandle, MountLeg, StrategyMount, spawn_core_multi};
use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate};
use vike_mm::XemmMaker;
use vike_model::RiskLimits;
use vike_model::{FeeSchedule, xemm_round_trip_fee};
use vike_paper::{PaperExecutionClient, PaperFill};

use crate::node::{Node, NodeConfig, NodeError, WiredMarket, build_node};
use crate::run::PaperHalt;

/// Everything needed to stand up a cross-exchange maker mount. Legs are `(venue, symbol)` PAIRS:
/// resting and crossing are different roles with different fee sides, and must not be confusable.
#[derive(Clone, Debug)]
pub struct XemmMountConfig {
    /// The venue the maker RESTS on (venue A).
    pub maker_venue: String,
    /// The MOUNT's own symbol, on the maker venue.
    pub maker_symbol: String,
    /// The venue the maker HEDGES on (venue B), whose touch prices the quotes.
    pub taker_venue: String,
    /// Must DIFFER from `maker_symbol` (see [`XemmConfigError`]).
    pub hedge_symbol: String,
    /// Bar interval of the mount's series (the paper book fills on closed bars).
    pub interval: String,
    /// `interval` in ms — the paper rehearsal's `TickBarSynthesizer` window.
    pub interval_ms: i64,

    /// Base quote size per side, in base units.
    pub qty: f64,
    /// Required edge per side, as a fraction of the reference price.
    pub min_profitability: f64,
    /// Minimum standoff inside the maker venue's own touch, in ticks (`>= 1.0`).
    pub min_edge_ticks: f64,
    /// The maker venue's price grid.
    pub maker_tick_size: f64,
    /// Fraction of a maker fill to offset on the taker venue (`1.0` = fully hedged).
    pub hedge_ratio: f64,
    /// Residual below which nothing is hedged — set it to the TAKER venue's `min_qty`.
    pub hedge_dust: f64,

    /// Seed cash for the MAKER engine (one account per venue: the `spawn_core_multi` firewall).
    pub maker_seed_cash: f64,
    /// Seed cash for the TAKER venue's engine.
    pub taker_seed_cash: f64,
    /// Paper-fill slippage on both legs' books.
    pub slippage: f64,

    /// Opt-in basis halt band as `(max_basis_bps, halflife_ms, clamp)`. `None` ⇒ the estimator's
    /// [`vike_model::XemmParams::default`] values (it warms and publishes but never halts).
    pub basis_band: Option<(f64, i64, f64)>,
    /// Opt-in soft/hard naked bands, in base units. `None` ⇒ `XemmMaker::new`'s `qty` / `3·qty`.
    pub naked_bands: Option<(f64, f64)>,
    /// Opt-in freshness bounds `(max_ref_age_ms, max_own_touch_age_ms, max_emission_gap_ms)`.
    /// `None` ⇒ the ARMED defaults (2 s / 2 s / 5 s).
    pub freshness: Option<(i64, i64, i64)>,
    /// Opt-in hedge discipline `(timeout_ms, max_attempts, dust)` overriding `hedge_dust` above.
    /// `None` ⇒ the armed defaults (3 s / 3 attempts).
    pub hedge_discipline: Option<(i64, u32, f64)>,
    /// Opt-in one-sided-fill breaker `(window_ms, net_threshold, cooldown_ms)`. `None` ⇒ off.
    pub breaker: Option<(i64, f64, i64)>,
    /// Opt-in anti-churn tolerance `(price_bps, size_bps)`. `None` ⇒ every tick re-prices.
    pub refresh_tolerance: Option<(f64, f64)>,
    /// How long after a halt the maker may AUTO-resume, in ms. `0` (the default) = manual only.
    pub resume_after_halt_ms: i64,
}

/// Why a cross-exchange mount was REFUSED: each variant would be silently wrong at runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XemmConfigError {
    /// The legs name the SAME symbol. `resolve_intent_venue` finds a declared leg BY SYMBOL ALONE, so
    /// EVERY intent (the symbol-less maker quotes too) would route to the taker venue, and `on_fill`
    /// loses its leg discriminator. Rules out same-ticker pairs (binance × bybit `BTCUSDT`).
    SameSymbol(String),
    /// The legs share a VENUE: the reference lane (`m.venue != venue`) would never deliver a tick.
    SameVenue(String),
    /// No `build_node` engine for the taker venue: `apply_intent`'s
    /// `engine_idx_for_route_key(...).unwrap_or(0)` sends the hedge to the MAKER engine, DOUBLING the
    /// exposure it was sent to close, silently.
    TakerVenueNotWired(String),
    /// The taker engine trades a DIFFERENT symbol and `crate::make_engine` sets no `extra_symbols`,
    /// so `accepts_symbol(hedge_symbol)` is false and the hedge is dropped.
    HedgeSymbolNotAccepted { venue: String, wired: String, requested: String },
    /// A leg's schedule has no flat fraction (`Free`, `PerShareWithFloor`, `ProbabilityScaled`,
    /// `PercentOfUnderlying`); defaulting to `0.0` would rest every quote inside break-even.
    FeeNotExpressible { maker: String, taker: String },
    /// `min_profitability + total_fee <= 0`: a zero-edge xEMM is a pure fee donation.
    NoEdge,
    /// `min_edge_ticks < 1.0` or `maker_tick_size <= 0`: without a one-tick standoff the passive
    /// clamp refuses every tick.
    NoStandoff,
    /// A non-positive quote size.
    NoSize,
}

impl std::fmt::Display for XemmConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            XemmConfigError::SameSymbol(s) => write!(
                f,
                "xemm: both legs name symbol `{s}` — the runtime resolves a declared leg's venue by \
                 symbol alone, so the maker's own quotes would route to the taker venue"
            ),
            XemmConfigError::SameVenue(v) => {
                write!(f, "xemm: both legs are on venue `{v}` — that is not a cross-exchange maker")
            }
            XemmConfigError::TakerVenueNotWired(v) => write!(
                f,
                "xemm: no engine is wired for taker venue `{v}` — the hedge would silently route to \
                 the MAKER engine and double the exposure"
            ),
            XemmConfigError::HedgeSymbolNotAccepted { venue, wired, requested } => write!(
                f,
                "xemm: venue `{venue}` is wired for `{wired}`, not `{requested}` — its engine would \
                 reject the hedge (make_engine sets no extra_symbols)"
            ),
            XemmConfigError::FeeNotExpressible { maker, taker } => write!(
                f,
                "xemm: the {maker}-maker / {taker}-taker round-trip fee has no flat fraction — see \
                 vike_model::xemm_round_trip_fee; it must not be defaulted to 0.0"
            ),
            XemmConfigError::NoEdge => {
                write!(
                    f,
                    "xemm: min_profitability + total_fee must be > 0 (a zero edge donates fees)"
                )
            }
            XemmConfigError::NoStandoff => write!(
                f,
                "xemm: min_edge_ticks must be >= 1 and maker_tick_size > 0 — without a one-tick \
                 standoff the passive clamp refuses every quote"
            ),
            XemmConfigError::NoSize => write!(f, "xemm: qty must be > 0"),
        }
    }
}

impl std::error::Error for XemmConfigError {}

impl XemmMountConfig {
    /// A crypto pair with the ARMED defaults (optional knobs `None`); fees from the per-venue registry.
    ///
    /// The v1 pair: **maker = hyperliquid `BTC`, taker = okx `BTC-USDT-SWAP`** — distinct symbols,
    /// both wired by `build_node`, lossless hyperliquid L1 for the clamp anchor, `PercentMakerTaker`
    /// fees (1.5 + 5.0 = 6.5 bp), and the measured structural basis on the MAKER side, where the
    /// passive clamp protects.
    pub fn crypto(
        maker_venue: impl Into<String>,
        maker_symbol: impl Into<String>,
        taker_venue: impl Into<String>,
        hedge_symbol: impl Into<String>,
        qty: f64,
        min_profitability: f64,
        maker_tick_size: f64,
    ) -> Self {
        XemmMountConfig {
            maker_venue: maker_venue.into(),
            maker_symbol: maker_symbol.into(),
            taker_venue: taker_venue.into(),
            hedge_symbol: hedge_symbol.into(),
            interval: "1m".into(),
            interval_ms: 60_000,
            qty,
            min_profitability,
            min_edge_ticks: 1.0,
            maker_tick_size,
            hedge_ratio: 1.0,
            hedge_dust: 0.0,
            maker_seed_cash: 10_000.0,
            taker_seed_cash: 10_000.0,
            slippage: 0.0,
            basis_band: None,
            naked_bands: None,
            freshness: None,
            hedge_discipline: None,
            breaker: None,
            refresh_tolerance: None,
            resume_after_halt_ms: 0,
        }
    }

    /// The two legs' fee schedules, each resolved at its OWN LANE. On binance/aster the SYMBOL picks
    /// the order API (`BTCUSDT` spot, `BTCUSDT.P` USDⓈ-M perp; binance maker fees 5x apart), and this
    /// number IS the maker's break-even offset, so a bare-venue lookup would misprice every resting
    /// quote. `fee_lane` is the identity for the v1 pair and every non-`.P` symbol.
    fn schedules(&self) -> (FeeSchedule, FeeSchedule) {
        (
            vike_model::fee_schedule_for(vike_catalog::fee_lane(
                &self.maker_venue,
                &self.maker_symbol,
            )),
            vike_model::fee_schedule_for(vike_catalog::fee_lane(
                &self.taker_venue,
                &self.hedge_symbol,
            )),
        )
    }

    /// Check every mount precondition and return the round-trip fee. Both builders call it before
    /// spawning. `wired`: `None` for the paper rehearsal (it builds its own engines), the node's
    /// [`NodeConfig::markets`] for the live path.
    pub fn validate(&self, wired: Option<&[WiredMarket]>) -> Result<f64, XemmConfigError> {
        if self.maker_symbol == self.hedge_symbol {
            return Err(XemmConfigError::SameSymbol(self.maker_symbol.clone()));
        }
        if self.maker_venue == self.taker_venue {
            return Err(XemmConfigError::SameVenue(self.maker_venue.clone()));
        }
        if let Some(wired) = wired {
            match wired.iter().find(|m| m.venue == self.taker_venue) {
                None => {
                    return Err(XemmConfigError::TakerVenueNotWired(self.taker_venue.clone()));
                }
                Some(m) if m.symbol != self.hedge_symbol => {
                    return Err(XemmConfigError::HedgeSymbolNotAccepted {
                        venue: self.taker_venue.clone(),
                        wired: m.symbol.to_string(),
                        requested: self.hedge_symbol.clone(),
                    });
                }
                Some(_) => {}
            }
        }
        let (maker, taker) = self.schedules();
        let Some(total_fee) = xemm_round_trip_fee(maker, taker, false) else {
            return Err(XemmConfigError::FeeNotExpressible {
                maker: self.maker_venue.clone(),
                taker: self.taker_venue.clone(),
            });
        };
        if !(self.qty.is_finite() && self.qty > 0.0) {
            return Err(XemmConfigError::NoSize);
        }
        let edge = self.min_profitability + total_fee;
        if !(edge.is_finite() && edge > 0.0) {
            return Err(XemmConfigError::NoEdge);
        }
        if self.min_edge_ticks < 1.0
            || !(self.maker_tick_size.is_finite() && self.maker_tick_size > 0.0)
        {
            return Err(XemmConfigError::NoStandoff);
        }
        Ok(total_fee)
    }
}

/// Build the [`XemmMaker`]. An opt-in family applies ONLY when `Some`, so an unnamed knob keeps
/// `XemmMaker::new`'s ARMED default (the polarity inversion on
/// [`vike_model::XemmParams`](vike_model::XemmParams)).
pub fn build_xemm_maker(cfg: &XemmMountConfig, total_fee: f64) -> XemmMaker {
    let mut m = XemmMaker::new(
        &cfg.maker_symbol,
        &cfg.hedge_symbol,
        cfg.qty,
        cfg.min_profitability,
        total_fee,
        cfg.maker_tick_size,
    );
    if let Some((bps, halflife, clamp)) = cfg.basis_band {
        m = m.with_basis_band(bps, halflife, clamp);
    }
    if let Some((soft, hard)) = cfg.naked_bands {
        m = m.with_naked_bands(soft, hard);
    }
    if let Some((r, o, g)) = cfg.freshness {
        m = m.with_freshness(r, o, g);
    }
    match cfg.hedge_discipline {
        Some((timeout, attempts, dust)) => m = m.with_hedge_discipline(timeout, attempts, dust),
        // Still thread the config's dust bound through.
        None => {
            let d = m.params();
            m = m.with_hedge_discipline(d.hedge_timeout_ms, d.hedge_max_attempts, cfg.hedge_dust);
        }
    }
    if let Some((w, t, c)) = cfg.breaker {
        m = m.with_fill_breaker(w, t, c);
    }
    if let Some((p, s)) = cfg.refresh_tolerance {
        m = m.with_refresh_tolerance(p, s);
    }
    m = m.with_resume_after(cfg.resume_after_halt_ms);
    // `hedge_ratio` is a scalar with no builder: apply it through the bag.
    let params = vike_model::XemmParams { hedge_ratio: cfg.hedge_ratio, ..m.params() };
    m.apply_params(&params);
    m
}

/// The mount's declared legs: exactly ONE, the hedge on the TAKER venue. [`MountLeg::at`] is read
/// by `resolve_intent_venue` (the hedge routes to venue B) AND `drive_strategy_reference_quote`
/// (B's touch reaches `on_reference_quote`). The maker's own symbol is NOT declared: it would
/// hijack every symbol-less intent.
pub(crate) fn xemm_mount_legs(cfg: &XemmMountConfig) -> Vec<MountLeg> {
    vec![MountLeg::at(cfg.hedge_symbol.clone(), cfg.taker_venue.clone())]
}

/// A spawned PAPER cross-exchange mount: the live core with TWO engines and TWO paper books.
pub struct PaperXemmMount {
    pub handle: CoreHandle,
    /// Fills booked on the MAKER venue's paper book.
    pub maker_fills: Arc<std::sync::Mutex<Vec<PaperFill>>>,
    /// Fills booked on the TAKER venue's paper book; a misrouted hedge shows as empty here.
    pub hedge_fills: Arc<std::sync::Mutex<Vec<PaperFill>>>,
}

/// Stand the cross-exchange maker up on the PRODUCTION live core over TWO paper exchanges, one per
/// venue with its own `Account`/`RiskGate`/[`FeeSchedule`], through [`spawn_core_multi`] (the real
/// cross-venue firewall). No money, credentials or network: the caller drives both venues' ticks.
///
/// Per-leg fees come from the registry; `SimBroker` collapses maker and taker to one rate, so this
/// is the only honest offline rehearsal of `maker_fee_A + taker_fee_B` economics.
///
/// ⚠ **BOTH books arm the operator HALT sentinel, and this seam is why arming has a GATE.** It was
/// the unarmed THIRD paper mount seam while `crates/vike-paper/src/lib.rs`'s `halt_path` doc claimed
/// per-seam assertions in `crates/vike-mount/src/paper_fallback.rs`'s `paper_client` and
/// `crates/vike-mount/src/run/paper.rs`'s `paper_client_for` made that impossible;
/// `crates/vike-ops/tests/architecture/paper_mount_arming_gate.rs` now fails on an unclassified site.
///
/// ONE resolved path (`vike_bridge_core::halt::halt_path_from_env`, as every mount): one file to
/// `touch`. The hedge leg matters as much: a halt stopping quotes but not hedges would drift
/// inventory as the real run never would. A TEST expecting fills uses
/// [`build_paper_xemm_core_with`]; [`PaperHalt`]'s doc has the measurement.
pub fn build_paper_xemm_core(cfg: &XemmMountConfig) -> Result<PaperXemmMount, XemmConfigError> {
    build_paper_xemm_core_with(cfg, &PaperHalt::ProcessWide)
}

/// The MAKER and HEDGE paper books [`build_paper_xemm_core`] mounts, both armed with `halt`.
/// Separate so `the_xemm_paper_mount_arms_both_books` asserts on the CONSTRUCTED books (as for
/// `paper_fallback::paper_client` and `run::paper_client_for`): once moved into their
/// `ExecutionEngine`s they are unreachable from [`PaperXemmMount`].
fn xemm_paper_books(
    cfg: &XemmMountConfig,
    halt: &PaperHalt,
) -> (PaperExecutionClient, PaperExecutionClient) {
    let (maker_schedule, hedge_schedule) = cfg.schedules();
    // Resolve ONCE and clone: says "one path" at the call site rather than relying on memoization.
    let halt_path = halt.resolve();
    let maker = PaperExecutionClient::with_fee_schedule(
        &cfg.maker_venue,
        &cfg.maker_symbol,
        cfg.slippage,
        maker_schedule,
    )
    .with_halt_path(halt_path.clone());
    let hedge = PaperExecutionClient::with_fee_schedule(
        &cfg.taker_venue,
        &cfg.hedge_symbol,
        cfg.slippage,
        hedge_schedule,
    )
    .with_halt_path(halt_path);
    (maker, hedge)
}

/// [`build_paper_xemm_core`] with the HALT sentinel named by the caller. For TESTS:
/// `crates/vike-mount/tests/xemm_scripted.rs` fails on any box holding an operator HALT file —
/// MEASURED on the CI box (see [`PaperHalt`]). A test pins a path it never creates; a daemon gets
/// [`PaperHalt::ProcessWide`].
pub fn build_paper_xemm_core_with(
    cfg: &XemmMountConfig,
    halt: &PaperHalt,
) -> Result<PaperXemmMount, XemmConfigError> {
    let total_fee = cfg.validate(None)?;

    // The books fill with the SAME `schedules()` `validate` priced `total_fee` off, so realized
    // costs and the break-even offset agree on each leg's LANE.
    let (maker_client, hedge_client) = xemm_paper_books(cfg, halt);
    let maker_fills = Arc::clone(&maker_client.fills);
    let hedge_fills = Arc::clone(&hedge_client.fills);

    let maker_engine = ExecutionEngine::new(
        Account::new(1.0, &cfg.maker_venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        maker_client,
        &cfg.maker_venue,
        &cfg.maker_symbol,
    );
    let hedge_engine = ExecutionEngine::new(
        Account::new(1.0, &cfg.taker_venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        hedge_client,
        &cfg.taker_venue,
        &cfg.hedge_symbol,
    );

    let config = CoreConfig {
        seed_cash: cfg.maker_seed_cash,
        strategy: Some(StrategyMount {
            account: None,
            venue: cfg.maker_venue.clone(),
            symbol: cfg.maker_symbol.clone(),
            interval: cfg.interval.clone(),
            strategy: Box::new(build_xemm_maker(cfg, total_fee)),
            symbols: xemm_mount_legs(cfg),
            underlying_symbol: None,
            controller_id: None,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core_multi(maker_engine, vec![(cfg.taker_seed_cash, hedge_engine)], config);
    Ok(PaperXemmMount { handle, maker_fills, hedge_fills })
}

/// The spawned LIVE cross-exchange mount: the wired-market [`Node`] with the [`XemmMaker`] folded
/// into its [`CoreConfig::strategy`].
pub struct LiveXemmMount {
    pub node: Node,
}

/// Why a live mount failed: a configuration refusal, or the node's own fault.
#[derive(Debug)]
pub enum XemmMountError {
    Config(XemmConfigError),
    Node(NodeError),
}

impl std::fmt::Display for XemmMountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            XemmMountError::Config(e) => write!(f, "{e}"),
            XemmMountError::Node(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for XemmMountError {}

/// The LIVE twin of [`build_paper_xemm_core`], on the REAL [`build_node`] core. Validated against
/// `node_cfg.markets`, so an unwired or wrong-symbol taker venue is a STARTUP error. Spawns NO feed
/// (module doc: the caller wires three).
pub fn build_live_xemm_core(
    cfg: &XemmMountConfig,
    mut node_cfg: NodeConfig,
) -> Result<LiveXemmMount, XemmMountError> {
    let total_fee = cfg.validate(Some(node_cfg.markets)).map_err(XemmMountError::Config)?;
    node_cfg.core_config.strategy = Some(StrategyMount {
        account: None,
        venue: cfg.maker_venue.clone(),
        symbol: cfg.maker_symbol.clone(),
        interval: cfg.interval.clone(),
        strategy: Box::new(build_xemm_maker(cfg, total_fee)),
        symbols: xemm_mount_legs(cfg),
        underlying_symbol: None,
        controller_id: None,
    });
    Ok(LiveXemmMount { node: build_node(node_cfg).map_err(XemmMountError::Node)? })
}

#[path = "xemm_tests.rs"]
#[cfg(test)]
mod xemm_tests;
