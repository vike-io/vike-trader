//! The CROSS-EXCHANGE maker MOUNT — stand a [`vike_mm::XemmMaker`] up on the production live core,
//! resting on one venue and hedging on another.
//!
//! Deliberately a SIBLING of the single-venue maker mount in the crate root rather than an extension
//! of it: [`crate::MakerMountConfig`] describes ONE `(venue, symbol)` and its paper builder carries a
//! tripwire (`spec.legs.is_empty()`, in [`crate::build_paper_strategy_core_with`]) asserting exactly
//! that. Both stay untouched here.
//!
//! # What the tripwire is worried about, and why this builder is allowed to proceed
//!
//! The single-venue paper builder refuses a multi-symbol mount because a single-symbol
//! `PaperExecutionClient` stamps ITS OWN symbol onto every fill it emits, whatever the request
//! named — so a two-leg rehearsal would book both legs under one symbol and look perfectly correct
//! while the live core routed them apart. The rehearsal would actively conceal the thing it exists
//! to catch.
//!
//! This builder answers that concern by CONSTRUCTION rather than by ignoring it: **two engines, two
//! single-symbol paper books, one per venue** (legal — [`vike_core::spawn_core_multi`] takes one
//! concrete client TYPE, not one instance). Each book stamps its own symbol on its own leg's fills,
//! each engine carries its own `Account`/`RiskGate` and its own [`vike_model::FeeSchedule`], and a
//! misrouted order lands in the WRONG log where the test sees it.
//!
//! `vike_paper::MultiPaperExecutionClient` is deliberately NOT used: it routes purely by
//! `request.symbol` and never consults `request.venue`, so it could not exercise the cross-venue
//! routing this rehearsal exists to prove.
//!
//! # ⚠ The live path needs FEEDS the caller wires
//!
//! Like [`crate::build_live_maker_core`], nothing here spawns a feed. An xEMM needs THREE inbound
//! streams on the one `CoreHandle`:
//!
//! 1. the MAKER venue's L1/L2 — `passive_clamp`'s anchor;
//! 2. the TAKER venue's L1 — the pricing input, delivered to `on_reference_quote` because the mount
//!    declares that leg with [`vike_core::MountLeg::at`];
//! 3. a periodic `LiveSchedule` under the mount's id — the all-feeds-dead safety sweep, the ONE lane
//!    that still fires when both venues go silent.
//!
//! Without (3) a total outage leaves the maker's quotes resting until a feed returns.

use std::sync::Arc;

use vike_core::{spawn_core_multi, CoreConfig, CoreHandle, MountLeg, StrategyMount};
use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate, RiskLimits};
use vike_mm::XemmMaker;
use vike_model::{xemm_round_trip_fee, FeeSchedule};
use vike_paper::{PaperExecutionClient, PaperFill};

use crate::node::{build_node, Node, NodeConfig, NodeError, WIRED_MARKETS};
use crate::PaperHalt;

/// Everything needed to stand up a cross-exchange maker mount.
///
/// The two legs are named as `(venue, symbol)` PAIRS rather than as a symbol plus a venue list,
/// because on this strategy the pairing is the whole configuration: which book is rested in and
/// which is crossed are different roles with different fee sides, and a config shape that let them
/// be confused would be the most expensive kind of typo available.
#[derive(Clone, Debug)]
pub struct XemmMountConfig {
    /// The venue the maker RESTS on (venue A) — where its two tagged quotes live.
    pub maker_venue: String,
    /// The symbol on the maker venue. This is the MOUNT's own symbol.
    pub maker_symbol: String,
    /// The venue the maker HEDGES on (venue B) — also the venue whose touch prices the quotes.
    pub taker_venue: String,
    /// The symbol on the taker venue. Must DIFFER from `maker_symbol` (see [`XemmConfigError`]).
    pub hedge_symbol: String,
    /// Bar interval of the mount's own series (the paper book fills on closed bars).
    pub interval: String,
    /// That interval in milliseconds — the paper rehearsal's `TickBarSynthesizer` window.
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

    /// Seed cash for the MAKER venue's engine (each venue keeps its own account — the
    /// `spawn_core_multi` firewall).
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

/// Why a cross-exchange mount was REFUSED. Every variant is a way the mount would have been
/// silently wrong at runtime rather than loudly wrong at startup — which is the entire reason these
/// are checked at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XemmConfigError {
    /// The two legs name the SAME symbol. The runtime's `resolve_intent_venue` finds a declared leg
    /// BY SYMBOL ALONE, so a leg carrying the mount's own symbol routes EVERY intent — including the
    /// symbol-less tagged maker quotes — to the taker venue. It also destroys `on_fill`'s only leg
    /// discriminator. This rules out same-ticker pairs (binance `BTCUSDT` × bybit `BTCUSDT`)
    /// entirely.
    SameSymbol(String),
    /// The two legs name the same VENUE — then it is not a cross-exchange maker, and the reference
    /// lane (which requires `m.venue != venue`) would never deliver a tick.
    SameVenue(String),
    /// The taker venue is not one `build_node` wires an engine for. `apply_intent`'s
    /// `engine_idx_for_route_key(...).unwrap_or(0)` would route the hedge to the MAKER engine instead —
    /// DOUBLING the exposure it was sent to close, with no error anywhere.
    TakerVenueNotWired(String),
    /// The taker venue's wired engine trades a DIFFERENT symbol, and `vike_mount::make_engine`
    /// never sets `extra_symbols` — so `accepts_symbol(hedge_symbol)` is false and the hedge is
    /// dropped at the engine boundary.
    HedgeSymbolNotAccepted { venue: String, wired: String, requested: String },
    /// One of the legs' fee schedules has no flat fraction (`Free`, `PerShareWithFloor`,
    /// `ProbabilityScaled`, `PercentOfUnderlying`), so the round-trip fee is UNKNOWABLE. Defaulting
    /// it to `0.0` would price a fee-bearing round trip as free and rest every quote inside
    /// break-even.
    FeeNotExpressible { maker: String, taker: String },
    /// `min_profitability + total_fee <= 0`: a zero-edge xEMM is a pure fee donation.
    NoEdge,
    /// `min_edge_ticks < 1.0` or a non-positive `maker_tick_size`: without a real one-tick standoff
    /// the passive clamp cannot certify a quote non-marketable, and it will refuse every tick.
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
                write!(f, "xemm: min_profitability + total_fee must be > 0 (a zero edge donates fees)")
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
    /// A crypto cross-exchange pair with the ARMED defaults, resolving the round-trip fee from the
    /// per-venue registry. The remaining knobs are `None` (their armed defaults).
    ///
    /// The v1 pair this was written for is **maker = hyperliquid `BTC`, taker = okx
    /// `BTC-USDT-SWAP`**: distinct symbols (which rules out every same-ticker CEX pair), both
    /// engines wired at exactly those symbols by `build_node`, hyperliquid serves lossless L1 for
    /// the clamp's anchor, both fee schedules are `PercentMakerTaker` (1.5 + 5.0 = 6.5 bp), and the
    /// measured structural basis sits on the MAKER side — where the passive clamp is the protection,
    /// so that machinery is live on day one rather than dead code.
    #[allow(clippy::too_many_arguments)]
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

    /// The two legs' fee schedules from the per-venue registry, each resolved at its OWN LANE.
    ///
    /// Every leg is a `(venue, symbol)` PAIR, and on binance/aster the symbol is what picks the
    /// order API: `BTCUSDT` is spot, `BTCUSDT.P` is the USDⓈ-M perp, and binance prices them 5x
    /// apart on the maker side. Reading the bare venue id here would price an aster `.P` hedge — the
    /// symbol `WIRED_MARKETS` actually mounts for that venue — off the wrong lane, and this number
    /// IS the maker's break-even offset (`validate` folds it into every quote), so the error would
    /// land directly in the resting price. `fee_lane` is the identity for the v1 pair
    /// (hyperliquid × okx) and for every non-`.P` symbol.
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

    /// Check every mount precondition and return the resolved round-trip fee.
    ///
    /// Called by BOTH builders before anything is spawned, so a misconfiguration is a startup error
    /// naming the pair rather than a live misroute. `require_wired` is `false` for the paper
    /// rehearsal (which builds its own two engines and therefore cannot consult `build_node`'s
    /// table) and `true` for the live path.
    pub fn validate(&self, require_wired: bool) -> Result<f64, XemmConfigError> {
        if self.maker_symbol == self.hedge_symbol {
            return Err(XemmConfigError::SameSymbol(self.maker_symbol.clone()));
        }
        if self.maker_venue == self.taker_venue {
            return Err(XemmConfigError::SameVenue(self.maker_venue.clone()));
        }
        if require_wired {
            match WIRED_MARKETS.iter().find(|(v, _)| *v == self.taker_venue) {
                None => {
                    return Err(XemmConfigError::TakerVenueNotWired(self.taker_venue.clone()));
                }
                Some((_, wired)) if *wired != self.hedge_symbol => {
                    return Err(XemmConfigError::HedgeSymbolNotAccepted {
                        venue: self.taker_venue.clone(),
                        wired: (*wired).to_string(),
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

/// Build the [`XemmMaker`] this mount runs. Each opt-in family is applied ONLY when its config
/// `Option` is `Some`, so an unnamed knob keeps `XemmMaker::new`'s ARMED default rather than a
/// neutral one — the deliberate polarity inversion documented on
/// [`vike_model::XemmParams`](vike_model::XemmParams).
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
        // Still thread the config's own dust bound through when no full discipline is named.
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
    // `hedge_ratio` has no builder (it is not a family, just a scalar), so apply it through the bag.
    let params = vike_model::XemmParams { hedge_ratio: cfg.hedge_ratio, ..m.params() };
    m.apply_params(&params);
    m
}

/// The mount's declared legs: exactly ONE, the hedge leg on the TAKER venue.
///
/// [`MountLeg::at`] is what makes the whole strategy possible — it is read by
/// `resolve_intent_venue` (so a symbol-carrying hedge routes to venue B) AND by
/// `drive_strategy_reference_quote` (so venue B's touch reaches `on_reference_quote`). The maker's
/// own symbol is deliberately NOT declared: a leg carrying it would hijack every symbol-less intent.
pub fn xemm_mount_legs(cfg: &XemmMountConfig) -> Vec<MountLeg> {
    vec![MountLeg::at(cfg.hedge_symbol.clone(), cfg.taker_venue.clone())]
}

/// A spawned PAPER cross-exchange mount: the live core with TWO engines and TWO paper books.
pub struct PaperXemmMount {
    pub handle: CoreHandle,
    /// Fills booked on the MAKER venue's paper book.
    pub maker_fills: Arc<std::sync::Mutex<Vec<PaperFill>>>,
    /// Fills booked on the TAKER venue's paper book. A hedge that reached the WRONG venue shows up
    /// as an empty log here and an unexpected row there — which is the whole point of keeping them
    /// separate.
    pub hedge_fills: Arc<std::sync::Mutex<Vec<PaperFill>>>,
}

/// Stand the cross-exchange maker up on the PRODUCTION live core over TWO paper exchanges — one per
/// venue, each with its own `Account`/`RiskGate`/[`FeeSchedule`], mounted through
/// [`spawn_core_multi`] so the cross-venue firewall is the real one.
///
/// Zero real-money / credential / geo risk, and no network: the caller drives both venues' ticks
/// into the returned handle. See the module doc for why two single-symbol books are the RIGHT answer
/// to the single-venue builder's multi-symbol tripwire rather than an evasion of it.
///
/// Each leg's fee model comes from the per-venue registry — which `PaperExecutionClient` can express
/// per book and `SimBroker` cannot express at all (it collapses maker and taker to one rate for a
/// whole backtest run), and which is why the paper path is the only honest offline rehearsal for a
/// strategy whose entire economics is `maker_fee_A + taker_fee_B`.
///
/// ⚠ **BOTH books arm the operator HALT sentinel, and this seam is why the arming has a GATE rather
/// than two per-crate assertions.** This function is a MOUNT — it stands the maker up on the
/// production live core and an operator rehearses against it — and it was the THIRD paper mount
/// seam, unarmed, at the moment `crates/vike-paper/src/lib.rs`'s `halt_path` doc claimed that
/// `vike_mount::paper_client` and `super::paper_client_for` "each assert on this, so adding a third
/// mount seam that forgets to arm would fail". It did not fail: an assertion inside seam A and an
/// assertion inside seam B say nothing about seam C, which is the shape of every roster this repo
/// has learned to gate instead of trust. `crates/vike-ops/tests/paper_mount_arming_gate.rs` now
/// enumerates the construction sites and fails on an unclassified one, so a FOURTH seam cannot be
/// added quietly.
///
/// Two books, two `with_halt_path` calls, ONE resolved path: the same
/// `vike_mount::halt::halt_path_from_env` every other mount consults, so a cross-venue rehearsal has
/// one file to `touch` and not one per leg. Arming the hedge leg matters more than arming the maker
/// leg, not less — a halt that stopped quotes while still letting hedges out would drift the
/// rehearsal's inventory in a way the real one never would.
///
/// A TEST that drives this mount and expects fills must NOT inherit that sentinel —
/// [`build_paper_xemm_core_with`] is the seam for that, and [`PaperHalt`]'s doc carries the
/// measurement of what happens when there is none.
pub fn build_paper_xemm_core(cfg: &XemmMountConfig) -> Result<PaperXemmMount, XemmConfigError> {
    build_paper_xemm_core_with(cfg, &PaperHalt::ProcessWide)
}

/// The MAKER and HEDGE paper books [`build_paper_xemm_core`] mounts, both armed with the operator
/// HALT sentinel `halt` names — see that function's doc for WHY both legs arm it.
///
/// A separate function purely so the arming is assertable on the CONSTRUCTED books
/// (`the_xemm_paper_mount_arms_both_books`) rather than on the source text — the same reason
/// `vike_mount::paper_client` and `super::paper_client_for` exist as named functions. A text gate
/// over call sites cannot tell an armed construction from an unarmed one, and the books themselves
/// are moved into their `ExecutionEngine`s and unreachable from [`PaperXemmMount`].
fn xemm_paper_books(
    cfg: &XemmMountConfig,
    halt: &PaperHalt,
) -> (PaperExecutionClient, PaperExecutionClient) {
    let (maker_schedule, hedge_schedule) = cfg.schedules();
    // ONE resolved path for both legs — a cross-venue rehearsal has one file to `touch`, not one
    // per leg. The `ProcessWide` arm memoizes through `halt_path_from_env`, so a second call would
    // return the same answer; resolving once and cloning says so at the call site instead of
    // relying on that.
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

/// [`build_paper_xemm_core`], with the operator HALT sentinel BOTH books watch named by the caller.
///
/// The parameter exists for TESTS, and it is not a nicety: this mount refuses opening orders while
/// its sentinel exists, so `crates/vike-run/tests/xemm_scripted.rs` — which drives scripted ticks
/// and asserts the maker quotes and the hedge fills — fails on any box holding an operator HALT
/// file. MEASURED on the CI box (see [`PaperHalt`]). A test pins a path it owns and never creates; a
/// daemon says nothing and gets [`PaperHalt::ProcessWide`].
pub fn build_paper_xemm_core_with(
    cfg: &XemmMountConfig,
    halt: &PaperHalt,
) -> Result<PaperXemmMount, XemmConfigError> {
    let total_fee = cfg.validate(false)?;

    // The two paper books fill with the SAME schedules `validate` just priced `total_fee` off —
    // taken from `schedules()` rather than re-looked-up, so the rehearsal's realized costs and the
    // maker's break-even offset can never disagree about which LANE each leg is on (see that
    // method's doc: on binance/aster the SYMBOL, not the venue, picks the order API).
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

/// The spawned LIVE cross-exchange mount: the twelve-venue [`Node`] with the [`XemmMaker`] folded
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

/// The LIVE twin of [`build_paper_xemm_core`]: mount the cross-exchange maker on the REAL
/// twelve-venue [`build_node`] core. Validated with `require_wired: true`, so a taker venue
/// `build_node` does not wire — or wires at a different symbol — is a STARTUP error rather than a
/// hedge that silently lands on the maker engine and doubles the exposure.
///
/// Spawns NO feed. The caller wires the maker venue's own book, the taker venue's touch, and the
/// periodic safety schedule onto `node.handle` — see the module doc.
pub fn build_live_xemm_core(
    cfg: &XemmMountConfig,
    mut node_cfg: NodeConfig,
) -> Result<LiveXemmMount, XemmMountError> {
    let total_fee = cfg.validate(true).map_err(XemmMountError::Config)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The v1 pair: hyperliquid maker `BTC` × okx taker `BTC-USDT-SWAP`.
    fn v1() -> XemmMountConfig {
        XemmMountConfig::crypto("hyperliquid", "BTC", "okx", "BTC-USDT-SWAP", 0.01, 0.0005, 0.5)
    }

    /// BOTH paper books this mount stands up arm the operator HALT sentinel.
    ///
    /// ⚠ This mount was the counter-example to a claim the tree made in writing.
    /// `crates/vike-paper/src/lib.rs`'s `halt_path` doc said `vike_mount::paper_client` and
    /// `paper_client_for` "each assert on this, so adding a third mount seam that forgets to arm
    /// would fail" — and `build_paper_xemm_core` was already that third seam, unarmed, when the
    /// sentence was written. Two per-crate assertions are not a gate over the SET of seams; that is
    /// `crates/vike-ops/tests/paper_mount_arming_gate.rs`. This test is the per-crate half for THIS
    /// seam: drop either `.with_halt_path(..)` in `xemm_paper_books` and it goes red.
    ///
    /// Both legs are asserted separately. A halt that stopped the maker's quotes while still letting
    /// hedges out would leave the rehearsal drifting inventory in a way the live mount never would,
    /// so a half-armed pair is its own failure mode rather than a partial fix.
    /// It also pins WHICH sentinel the default resolves — the process-wide one. "Armed" alone stops
    /// being enough once [`PaperHalt`] makes the choice a parameter: a mount armed at a path no
    /// operator knows about has no kill switch, and an `is_some()` check cannot tell the two apart.
    #[test]
    fn the_xemm_paper_mount_arms_both_books() {
        let process_wide = vike_mount::halt::halt_path_from_env();
        let (maker, hedge) = xemm_paper_books(&v1(), &PaperHalt::ProcessWide);
        assert_eq!(
            maker.halt_path(),
            Some(process_wide.as_path()),
            "the MAKER paper book must arm the PROCESS-WIDE HALT sentinel — this is a mount, not a \
             simulation"
        );
        assert_eq!(
            hedge.halt_path(),
            Some(process_wide.as_path()),
            "the HEDGE paper book must arm it too: a halt that stops quotes but not hedges drifts \
             the rehearsal's inventory away from what the live mount would do"
        );
        assert_eq!(
            maker.halt_path(),
            hedge.halt_path(),
            "both legs must watch ONE sentinel — a cross-venue rehearsal has one file to `touch`"
        );
    }

    /// A PINNED sentinel reaches BOTH books — the seam `crates/vike-run/tests/xemm_scripted.rs`
    /// needs so its scripted fills do not depend on whether the box running it holds a HALT file.
    #[test]
    fn a_pinned_sentinel_reaches_both_xemm_books() {
        let pinned =
            std::path::PathBuf::from("xemm-tests-own-this-sentinel-and-never-make-it/HALT");
        let (maker, hedge) = xemm_paper_books(&v1(), &PaperHalt::Pinned(pinned.clone()));
        assert_eq!(maker.halt_path(), Some(pinned.as_path()), "the maker leg must be pinned");
        assert_eq!(hedge.halt_path(), Some(pinned.as_path()), "the hedge leg must be pinned too");
    }

    /// The v1 pair validates, and resolves the 6.5 bp round-trip fee (hyperliquid maker 1.5 +
    /// okx taker 5.0) bit-for-bit — that number IS the maker's break-even offset.
    #[test]
    fn the_v1_pair_validates_and_resolves_six_and_a_half_bps() {
        let fee = v1().validate(true).expect("the v1 pair is wired and fee-expressible");
        assert_eq!(fee.to_bits(), (1.5_f64 / 10_000.0 + 5.0 / 10_000.0).to_bits());
    }

    /// THE HIJACK GUARD. Same-symbol legs are refused, which rules out every same-ticker CEX pair
    /// (binance `BTCUSDT` × bybit `BTCUSDT`) — the pairing an operator would reach for first.
    #[test]
    fn a_same_symbol_pair_is_refused() {
        let mut cfg = v1();
        cfg.taker_venue = "bybit".into();
        cfg.hedge_symbol = cfg.maker_symbol.clone();
        assert_eq!(cfg.validate(false), Err(XemmConfigError::SameSymbol("BTC".into())));
    }

    /// Two legs on one venue is not a cross-exchange maker, and the reference lane (which requires
    /// `m.venue != venue`) would never deliver a tick.
    #[test]
    fn a_same_venue_pair_is_refused() {
        let mut cfg = v1();
        cfg.taker_venue = cfg.maker_venue.clone();
        assert_eq!(cfg.validate(false), Err(XemmConfigError::SameVenue("hyperliquid".into())));
    }

    /// An unwired taker venue is a STARTUP error on the LIVE path: `apply_intent`'s
    /// `unwrap_or(0)` would otherwise route the hedge to the MAKER engine, doubling the exposure
    /// with no error anywhere.
    ///
    /// The PAPER path builds its own two engines and so does not consult the wired table at all —
    /// which is observable here as a DIFFERENT refusal for the same config (an unknown venue's fee
    /// schedule is `Free`, which has no flat fraction). Two distinct errors for one config is the
    /// proof that the wiring check is `require_wired`-gated rather than always-on.
    #[test]
    fn an_unwired_taker_venue_is_refused_on_the_live_path_only() {
        let mut cfg = v1();
        cfg.taker_venue = "kalshi".into();
        assert_eq!(cfg.validate(true), Err(XemmConfigError::TakerVenueNotWired("kalshi".into())));
        assert!(
            matches!(cfg.validate(false), Err(XemmConfigError::FeeNotExpressible { .. })),
            "paper skips the wiring check and refuses on the fee shape instead"
        );
    }

    /// The taker venue's engine trades ONE hardcoded symbol and `make_engine` sets no
    /// `extra_symbols`, so a hedge symbol that venue is not wired for would be dropped at the
    /// engine boundary. Refuse it by name.
    #[test]
    fn a_hedge_symbol_the_taker_engine_does_not_accept_is_refused() {
        let mut cfg = v1();
        cfg.hedge_symbol = "ETH-USDT-SWAP".into();
        assert_eq!(
            cfg.validate(true),
            Err(XemmConfigError::HedgeSymbolNotAccepted {
                venue: "okx".into(),
                wired: "BTC-USDT-SWAP".into(),
                requested: "ETH-USDT-SWAP".into(),
            })
        );
    }

    /// A leg whose fee SHAPE has no flat fraction is refused rather than defaulted to `0.0` — which
    /// would price a fee-bearing round trip as free and rest every quote inside break-even. Both
    /// leg positions are checked.
    #[test]
    fn a_fee_shape_without_a_flat_rate_is_refused_on_either_leg() {
        // oanda is `Free` (spread-charging) — a shape, not a value.
        let mut taker_free = v1();
        taker_free.taker_venue = "oanda".into();
        taker_free.hedge_symbol = "EURUSD".into();
        assert!(matches!(
            taker_free.validate(true),
            Err(XemmConfigError::FeeNotExpressible { .. })
        ));
        // deribit is `PercentOfUnderlying`: its flat reading UNDERSTATES the real fee.
        let mut maker_deribit = v1();
        maker_deribit.maker_venue = "deribit".into();
        maker_deribit.maker_symbol = "BTC-PERPETUAL".into();
        assert!(matches!(
            maker_deribit.validate(true),
            Err(XemmConfigError::FeeNotExpressible { .. })
        ));
    }

    /// The degenerate tunings that would make the maker either donate fees or refuse every tick.
    #[test]
    fn a_zero_edge_a_sub_tick_standoff_and_a_zero_size_are_each_refused() {
        let mut no_edge = v1();
        no_edge.min_profitability = 0.0;
        no_edge.maker_venue = "polymarket".into(); // Free ⇒ …but that refuses on fees first
        no_edge.maker_venue = "hyperliquid".into();
        // With a real fee the edge is positive even at zero profitability, so force it negative.
        no_edge.min_profitability = -1.0;
        assert_eq!(no_edge.validate(false), Err(XemmConfigError::NoEdge));

        let mut sub_tick = v1();
        sub_tick.min_edge_ticks = 0.5;
        assert_eq!(sub_tick.validate(false), Err(XemmConfigError::NoStandoff));
        let mut no_grid = v1();
        no_grid.maker_tick_size = 0.0;
        assert_eq!(no_grid.validate(false), Err(XemmConfigError::NoStandoff));

        let mut no_size = v1();
        no_size.qty = 0.0;
        assert_eq!(no_size.validate(false), Err(XemmConfigError::NoSize));
    }

    /// The mount declares exactly ONE leg — the hedge leg, on the taker venue. Declaring the
    /// maker's own symbol as well would hijack every symbol-less intent (see [`XemmConfigError`]).
    #[test]
    fn the_mount_declares_only_the_hedge_leg() {
        let legs = xemm_mount_legs(&v1());
        assert_eq!(legs, vec![MountLeg::at("BTC-USDT-SWAP", "okx")]);
    }

    /// The builder threads every named knob into the maker and leaves the ARMED defaults where a
    /// knob was not named — the polarity inversion `XemmParams` documents.
    #[test]
    fn the_builder_threads_named_knobs_and_keeps_armed_defaults() {
        let mut cfg = v1();
        cfg.basis_band = Some((40.0, 30_000, 0.05));
        cfg.naked_bands = Some((0.02, 0.05));
        cfg.breaker = Some((1_000, 2.5, 5_000));
        cfg.hedge_ratio = 1.0;
        let fee = cfg.validate(true).expect("valid");
        let m = build_xemm_maker(&cfg, fee);
        let p = m.params();
        assert_eq!(m.legs(), ("BTC", "BTC-USDT-SWAP"));
        assert_eq!(p.total_fee.to_bits(), fee.to_bits());
        assert_eq!(p.max_basis_bps.to_bits(), 40.0_f64.to_bits());
        assert_eq!(p.naked_hard_band.to_bits(), 0.05_f64.to_bits());
        assert_eq!(p.net_fill_threshold.to_bits(), 2.5_f64.to_bits());
        // unnamed: the ARMED defaults, not neutral ones.
        assert_eq!(p.max_ref_age_ms, 2_000);
        assert_eq!(p.hedge_max_attempts, 3);
        assert_eq!(p.resume_after_halt_ms, 0, "auto-resume stays manual");
    }

    /// The paper rehearsal stands up TWO engines over TWO single-symbol books and refuses a bad
    /// config before spawning anything.
    #[test]
    fn the_paper_rehearsal_spawns_two_engines_and_refuses_a_bad_config() {
        let mount = build_paper_xemm_core(&v1()).expect("the v1 pair is valid");
        assert!(mount.maker_fills.lock().unwrap().is_empty());
        assert!(mount.hedge_fills.lock().unwrap().is_empty());
        mount.handle.shutdown_and_join();

        let mut bad = v1();
        bad.hedge_symbol = bad.maker_symbol.clone();
        assert!(build_paper_xemm_core(&bad).is_err(), "a hijacking config never spawns");
    }
}
