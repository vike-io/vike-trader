//! The cross-exchange maker's CONFIGURATION: the mount's knobs, its refusals, and the maker it builds.

#[cfg(doc)]
use crate::node::NodeConfig;
use crate::node::WiredMarket;
use vike_core::MountLeg;
use vike_mm::XemmMaker;
use vike_model::{FeeSchedule, xemm_round_trip_fee};

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
    pub(super) fn schedules(&self) -> (FeeSchedule, FeeSchedule) {
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
