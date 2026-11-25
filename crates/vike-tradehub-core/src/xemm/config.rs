//! The cross-exchange maker's CONFIGURATION: the strategy-free [`MountPlacement`] whose ONE declared leg
//! is the hedge on the taker venue, the maker's [`XemmParams`] and their refusals. The maker itself
//! is built by the CALLER (no library builds a strategy): the mount builders hand its builder the
//! mount, the bag and the round trip [`validate_xemm_mount`] priced.

#[cfg(doc)]
use crate::node::RuntimeConfig;
use crate::node::RuntimeMarket;
use crate::run::MountPlacement;
use vike_core::MountLeg;
use vike_model::{FeeSchedule, XemmParams, xemm_round_trip_fee};

/// Why a cross-exchange mount was REFUSED: each variant would be silently wrong at runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XemmConfigError {
    /// The mount does not declare exactly ONE leg (the hedge); it declares this many. A second leg
    /// has no role in the maker, and the maker's own symbol declared as a leg would hijack every
    /// symbol-less intent.
    NotOneHedgeLeg(usize),
    /// The hedge leg is NAMED like the maker's own symbol and names no account. A strategy reaches a
    /// declared leg BY ITS NAME, and the mount's own name is the mount's own instrument (decision 0277's
    /// rule 1), so the hedge would land on the MAKER's book and `on_fill` would lose its leg
    /// discriminator. A same-ticker pair (binance × bybit `BTCUSDT.P`, one user name on every venue)
    /// names its hedge by its account instead, `BTCUSDT.P@bybit/LABEL` (decision 0293), which passes.
    SameSymbol(String),
    /// The hedge leg's name is a `SYMBOL@BOOK` the leg rule refuses (`vike_core::MountLeg::named`), in
    /// its words.
    HedgeLegName(String),
    /// The legs share a VENUE: the reference lane (`m.venue != venue`) would never deliver a tick.
    SameVenue(String),
    /// No `assemble_runtime` engine for the taker venue: `apply_intent`'s
    /// `engine_index_for_route_key(...).unwrap_or(0)` sends the hedge to the MAKER engine, DOUBLING the
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
    /// `min_edge_ticks < 1.0` or a mount `tick_size <= 0`: without a one-tick standoff the passive
    /// clamp refuses every tick.
    NoStandoff,
    /// A non-positive quote size.
    NoSize,
}

impl std::fmt::Display for XemmConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            XemmConfigError::NotOneHedgeLeg(n) => write!(
                f,
                "xemm: the mount must declare exactly one leg, the hedge on the taker venue; it \
                 declares {n}"
            ),
            XemmConfigError::SameSymbol(s) => write!(
                f,
                "xemm: both legs are named `{s}` — a strategy reaches a leg by its name and `{s}` is \
                 the maker's own instrument, so the hedge would land on the maker's book; name the \
                 hedge by its account, `{s}@<taker venue>/<LABEL>`"
            ),
            XemmConfigError::HedgeLegName(why) => write!(f, "xemm: {why}"),
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
                "xemm: the hedge's account of venue `{venue}` is mounted on `{wired}` (the symbol its \
                 row states; empty when it states none), not `{requested}` — its engine would reject \
                 the hedge (make_engine sets no extra_symbols)"
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
                "xemm: min_edge_ticks must be >= 1 and the mount's tick_size > 0 — without a \
                 one-tick standoff the passive clamp refuses every quote"
            ),
            XemmConfigError::NoSize => write!(f, "xemm: qty must be > 0"),
        }
    }
}

impl std::error::Error for XemmConfigError {}

/// The mount of a crypto pair: resting on `maker_venue`/`maker_symbol` at the maker venue's price
/// grid `maker_tick_size`, its ONE declared leg the hedge on the taker venue ([`MountLeg::at`]), on
/// the `1m` series with `10_000` of paper equity per venue and zero paper slippage.
///
/// [`MountLeg::at`] is read by `resolve_intent_venue` (the hedge routes to venue B) AND
/// `drive_strategy_reference_quote` (B's touch reaches `on_reference_quote`). The maker's own symbol
/// is NOT declared: it would hijack every symbol-less intent.
///
/// `hedge_symbol` is the hedge leg's NAME, the one the maker's orders and fills carry: a plain symbol
/// on `taker_venue`'s account the venue rule resolves, or `SYMBOL@venue/LABEL` for the hedge on that
/// named account (decision 0293), whose venue is then its own. A pair of ONE user name on two venues
/// (binance × bybit `BTCUSDT.P`) names its hedge so; [`validate_xemm_mount`] refuses it plain.
///
/// The v1 pair: **maker = hyperliquid `BTC`, taker = okx `BTC-USDT-SWAP`** — distinct symbols,
/// both wired by `assemble_runtime`, lossless hyperliquid L1 for the clamp anchor, `PercentMakerTaker`
/// fees (1.5 + 5.0 = 6.5 bp), and the measured structural basis on the MAKER side, where the
/// passive clamp protects.
pub fn xemm_mount(
    maker_venue: impl Into<String>,
    maker_symbol: impl Into<String>,
    taker_venue: impl Into<String>,
    hedge_symbol: impl Into<String>,
    maker_tick_size: f64,
) -> MountPlacement {
    let (maker_venue, hedge_symbol) = (maker_venue.into(), hedge_symbol.into());
    // A name the leg rule refuses stays a plain leg under its name, which `validate_xemm_mount` refuses.
    let hedge = match MountLeg::named(&hedge_symbol, &maker_venue) {
        Ok(leg) if leg.names_an_account() => leg,
        _ => MountLeg::at(hedge_symbol, taker_venue),
    };
    MountPlacement {
        legs: vec![hedge],
        ..MountPlacement::new(maker_venue, maker_symbol, 10_000.0, maker_tick_size)
    }
}

/// The maker's tunables at `qty` per side and `min_profitability` (a fraction of the reference
/// price), every guard at its ARMED default ([`XemmParams::default`]'s polarity inversion) and the
/// naked bands the cross-exchange maker's constructor derives (`qty` soft, `3·qty` hard). `total_fee`
/// and `maker_tick_size` are mount facts, whatever the bag says: the round trip
/// [`validate_xemm_mount`] returns, which the mount builders hand the maker builder, and the
/// mount's `tick_size`.
pub fn xemm_params(qty: f64, min_profitability: f64) -> XemmParams {
    XemmParams {
        qty,
        min_profitability,
        naked_band: qty,
        naked_hard_band: 3.0 * qty,
        ..XemmParams::default()
    }
}

/// The hedge leg: the mount's one declared leg, on its own venue or (a leg with none) the mount's,
/// which [`validate_xemm_mount`] then refuses as the same venue.
fn hedge_leg(spec: &MountPlacement) -> Result<&MountLeg, XemmConfigError> {
    let [leg] = spec.legs.as_slice() else {
        return Err(XemmConfigError::NotOneHedgeLeg(spec.legs.len()));
    };
    Ok(leg)
}

/// The two legs' fee schedules, each resolved at its OWN LANE. On binance/aster the SYMBOL picks the
/// order API (`BTCUSDT` spot, `BTCUSDT.P` USDⓈ-M perp; binance maker fees 5x apart), and this number
/// IS the maker's break-even offset, so a bare-venue lookup would misprice every resting quote.
/// `fee_lane` is the identity for the v1 pair and every non-`.P` symbol.
fn schedules(
    spec: &MountPlacement,
    taker_venue: &str,
    hedge_symbol: &str,
) -> (FeeSchedule, FeeSchedule) {
    (
        vike_venues::fee_schedule_for(crate::engine::kept_fee_lane(&spec.venue, &spec.symbol)),
        vike_venues::fee_schedule_for(crate::engine::kept_fee_lane(taker_venue, hedge_symbol)),
    )
}

/// Check every mount precondition and return the round-trip fee. Both builders call it before
/// spawning. `wired`: `None` for the paper rehearsal (it builds its own engines); for the live path the
/// node's [`crate::ArmingTable::wired_markets`] over the node's backend, and the account rows the node
/// mounts from, whose taker venue's DEFAULT account states the symbol the hedge must name
/// (`vike_config::account_arming::account_symbol`; none stated is no symbol, and any hedge is refused).
pub fn validate_xemm_mount(
    spec: &MountPlacement,
    params: &XemmParams,
    wired: Option<(&[RuntimeMarket], &vike_secrets::account_directory::AccountDirectory)>,
) -> Result<f64, XemmConfigError> {
    let leg = hedge_leg(spec)?;
    let (taker_venue, hedge_symbol) =
        (leg.venue.as_deref().unwrap_or(&spec.venue), leg.symbol.as_str());
    // SameSymbol only when the two legs share a NAME: a hedge named by its account
    // (`BTCUSDT.P@bybit/LABEL`) is told apart from the maker's `BTCUSDT.P` (decision 0293).
    if spec.symbol == leg.name {
        return Err(XemmConfigError::SameSymbol(spec.symbol.clone()));
    }
    if spec.venue == taker_venue {
        return Err(XemmConfigError::SameVenue(spec.venue.clone()));
    }
    MountLeg::named(&leg.name, &spec.venue).map_err(XemmConfigError::HedgeLegName)?;
    if let Some((wired, accounts)) = wired {
        if !wired.iter().any(|m| m.venue == taker_venue) {
            return Err(XemmConfigError::TakerVenueNotWired(taker_venue.to_string()));
        }
        // The hedge's OWN account: the one its name binds, else the venue's default (decision 0293).
        let mounted = vike_config::account_arming::account_symbol(
            Some(accounts),
            taker_venue,
            &leg.account.clone().unwrap_or_default(),
        )
        .unwrap_or_default();
        if mounted != hedge_symbol {
            return Err(XemmConfigError::HedgeSymbolNotAccepted {
                venue: taker_venue.to_string(),
                wired: mounted.to_string(),
                requested: hedge_symbol.to_string(),
            });
        }
    }
    let (maker, taker) = schedules(spec, taker_venue, hedge_symbol);
    let Some(total_fee) = xemm_round_trip_fee(maker, taker, false) else {
        return Err(XemmConfigError::FeeNotExpressible {
            maker: spec.venue.clone(),
            taker: taker_venue.to_string(),
        });
    };
    if !(params.qty.is_finite() && params.qty > 0.0) {
        return Err(XemmConfigError::NoSize);
    }
    let edge = params.min_profitability + total_fee;
    if !(edge.is_finite() && edge > 0.0) {
        return Err(XemmConfigError::NoEdge);
    }
    if params.min_edge_ticks < 1.0 || !(spec.tick_size.is_finite() && spec.tick_size > 0.0) {
        return Err(XemmConfigError::NoStandoff);
    }
    Ok(total_fee)
}
