//! A PAPER node mounting the outcome-token A-S maker: the node the hub-facing test harnesses (this
//! crate's, the tradehub's, `vike-cli`'s, `vike-mcp`'s and `vike-telegram`'s) stand up, built the
//! way the daemon builds one (the maker's `from_params` over the `outcome_token` preset, on a
//! [`MountPlacement`]), so no test keeps a maker construction of its own.
//!
//! The maker is the shipped strategy FILE (`strategies/market_making/spread_maker.rs`'s
//! `SpreadMaker`), which only the dev-only check crate compiles and so no library may name: the
//! caller hands its reader in
//! (`vike_strategies_check::makers::spread_maker::SpreadMaker::from_params`).

use vike_core::LiveBroker;
use vike_market_making::{MakerParamError, SpreadMakerPreset};
use vike_model::Strategy;
use vike_model::params::Params;

use crate::{
    MountPlacement, StrategyCorePaper, StrategyCorePaperConfig, build_paper_strategy_core_with,
};

/// The outcome-token A-S maker anchored on `resolution_ts` (epoch-ms; `None` ⇒ the constant
/// `tau_hold`), read by `from_params`.
#[must_use]
pub fn outcome_maker<S>(
    from_params: fn(&Params) -> Result<S, MakerParamError>,
    resolution_ts: Option<i64>,
) -> S {
    let mut params = Params::new();
    params.insert("preset", SpreadMakerPreset::OutcomeToken.word());
    if let Some(ts) = resolution_ts {
        params.insert("resolution_ts", ts);
    }
    from_params(&params).expect("the outcome-token preset reads")
}

/// The paper mount of `(venue, symbol)` at the outcome-token preset's paper equity and grid
/// (`1_000` and `0.01`).
#[must_use]
pub fn outcome_spec(venue: &str, symbol: &str) -> MountPlacement {
    MountPlacement::new(venue, symbol, 1_000.0, 0.01)
}

/// [`outcome_maker`] on [`outcome_spec`], over a paper book armed with `opts`.
#[must_use]
pub fn paper_maker_core_with<S: Strategy<LiveBroker> + Send + 'static>(
    from_params: fn(&Params) -> Result<S, MakerParamError>,
    venue: &str,
    symbol: &str,
    resolution_ts: Option<i64>,
    opts: StrategyCorePaperConfig,
) -> StrategyCorePaper {
    let maker = Box::new(outcome_maker(from_params, resolution_ts));
    build_paper_strategy_core_with(maker, &outcome_spec(venue, symbol), opts)
}

/// [`paper_maker_core_with`] with the default options (the PROCESS-WIDE halt, like a daemon).
#[must_use]
pub fn paper_maker_core<S: Strategy<LiveBroker> + Send + 'static>(
    from_params: fn(&Params) -> Result<S, MakerParamError>,
    venue: &str,
    symbol: &str,
    resolution_ts: Option<i64>,
) -> StrategyCorePaper {
    paper_maker_core_with(
        from_params,
        venue,
        symbol,
        resolution_ts,
        StrategyCorePaperConfig::default(),
    )
}
