//! Mount configuration: the strategy-free `MountPlacement` every mount builder takes, and the static fee
//! lane a maker's break-even floor is armed from.

#[cfg(doc)]
use vike_core::{Mount, spawn_core};
#[cfg(doc)]
use vike_paper::ExecutionClientPaper;

#[cfg(doc)]
use super::paper::build_paper_strategy_core_with;
#[cfg(doc)]
use super::sink::TickBarSynthesizer;

/// The STRATEGY-FREE mount: everything [`spawn_core`] needs to place ANY strategy on a
/// `(venue, symbol, interval)` series and price its paper fills, with no strategy knob in it, so
/// any boxed `Strategy<LiveBroker> + Send` gets the identical mount.
///
/// ⚠ No strategy's opinion lives here: a maker's knobs are its own params table (the spread maker
/// file's `SpreadMaker::from_params` over a `vike_market_making::SpreadMakerPreset`), and a hand-built
/// `MountPlacement` states its paper equity and price grid ([`Self::new`]) rather than inheriting a
/// maker's opinion of how much an asset needs.
#[derive(Clone, Debug)]
pub struct MountPlacement {
    /// venue tag; also the paper client's + engine's venue.
    pub venue: String,
    /// the mount SYMBOL: the instrument the strategy trades.
    pub symbol: String,
    /// **WHICH ACCOUNT of [`Self::venue`] this mount trades on.** `None` is the venue's DEFAULT
    /// account: the same engine, `route_key` (the bare venue id) and `LIVE-<route_key>.lock`.
    /// `Some(label)` routes orders AND `OrderBroker` reads (position / equity / multiplier / lot size) to
    /// that account's engine, and [`Self::symbol`] is the symbol it is armed on
    /// (`crate::account_symbols_for`).
    ///
    /// ⚠ **A named account that is not ARMED must never fall through to the default one.** The root
    /// checks it against the SAME `crate::venue_account_arming` the fan-out selects with and DROPS
    /// the mount with an error naming venue, account and strategy; `vike_core`'s `assemble_core`
    /// panics on a named account with no engine, the backstop no future root can reach past.
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
    /// the mounted bar-series interval (must match the synthesized bars: [`TickBarSynthesizer`]).
    pub interval: String,
    /// the [`TickBarSynthesizer`] window in ms (e.g. `60_000` for `"1m"`): a feed / paper-fill
    /// concern, not a strategy knob.
    pub interval_ms: i64,
    /// The venue PRICE GRID of [`Self::symbol`]: the step the mount's feed sizes its L2 book on and,
    /// on a maker mount, the grid the maker quotes on (one number, so the two cannot disagree). A
    /// mount fact like [`Self::interval_ms`]; nothing in this crate reads it.
    pub tick_size: f64,
    /// Optional CROSS-SYMBOL underlying the strategy WATCHES
    /// ([`Mount::underlying_symbol`]); `None` ⇒ no routing.
    pub underlying_symbol: Option<String>,
    /// Optional explicit mount id ([`Mount::controller_id`]); `None` ⇒
    /// `{venue}__{symbol}__{interval}`. REQUIRED when two mounts share one triple: `assemble_core`
    /// PANICS on a duplicate rather than silently sharing durable state.
    pub controller_id: Option<String>,
    /// Opt-in ADDITIONAL symbols this mount may trade and read ([`Mount::symbols`]).
    ///
    /// The STRATEGY declares them ([`Self::declare_strategy_legs`] over `vike_model::Strategy::symbols`,
    /// called by the daemon when it resolves the mount); a profile declares none, so EMPTY on every
    /// mount whose strategy trades its own symbol alone. A paper rehearsal gives each leg a book of
    /// its own ([`build_paper_strategy_core_with`]): a single-book [`ExecutionClientPaper`] stamps
    /// its own symbol on every fill and would book both legs under one.
    pub legs: Vec<vike_core::MountLeg>,
    /// paper account seed (equity), and the paper fill-cost scalars.
    pub seed_cash: f64,
    pub slippage: f64,
    pub maker_fee: f64,
    pub taker_fee: f64,
    /// The mount's STRATEGY CODE ([`Mount::order_client_id_code`], decision 0270), which the composition root reads
    /// out of the settings store; `None` (every builder here) mints `0000`.
    pub order_client_id_code: Option<vike_model::orders::client_id::OrderClientIdStrategyCode>,
    /// The strategy FILE the mounted strategy was built from and the sha256 of the bytes the composition
    /// root read, which every builder names to the core (`vike_core::CoreConfig::mount_strategy_files`,
    /// `MountPlacement::name_strategy_file`) so it is journaled as the mount opens; `None` (every builder
    /// here) journals nothing.
    pub strategy_file: Option<vike_journal::JournalStrategyFile>,
}

impl MountPlacement {
    /// Declare what the mounted strategy trades beside this mount's own symbol
    /// (`vike_model::Strategy::symbols`) as legs on this mount's venue: a blank, the own symbol and a
    /// repeat declare nothing. The node admits each into the venue engine and the core routes an
    /// order naming one there, refusing every other symbol. The rule is `vike_core`'s
    /// [`vike_core::MountLeg::declare_strategy_legs`], which a runtime mount declares through too.
    ///
    /// # Errors
    /// A leg the control door writes another way (the C-D4 follow-up: `ethusdt.s` is `ETHUSDT.S` on
    /// bybit), in the rule's words; no leg is declared then.
    pub fn declare_strategy_legs(&mut self, symbols: Vec<String>) -> Result<(), String> {
        vike_core::MountLeg::declare_strategy_legs(
            &mut self.legs,
            &self.venue,
            &self.symbol,
            symbols,
        )
    }

    /// A mount of `(venue, symbol)` on the `1m` series (a `60_000` ms window), with `seed_cash` of
    /// paper equity and the venue price grid `tick_size`; the paper fill costs come from the
    /// venue's fee registry (`paper_client_for`: zero slippage, no flat fee), the default
    /// account, no underlying, no explicit id, no leg and no strategy code.
    #[must_use]
    pub fn new(
        venue: impl Into<String>,
        symbol: impl Into<String>,
        seed_cash: f64,
        tick_size: f64,
    ) -> Self {
        MountPlacement {
            venue: venue.into(),
            symbol: symbol.into(),
            account: None,
            interval: "1m".to_string(),
            interval_ms: 60_000,
            tick_size,
            underlying_symbol: None,
            controller_id: None,
            legs: Vec::new(),
            seed_cash,
            slippage: 0.0,
            maker_fee: 0.0,
            taker_fee: 0.0,
            order_client_id_code: None,
            strategy_file: None,
        }
    }

    /// Name [`Self::strategy_file`] in `files` (`vike_core::CoreConfig::mount_strategy_files`) under the
    /// mount id the core keys this mount by, so the core journals the file as the mount opens. Nothing for
    /// a mount no file names.
    pub(crate) fn name_strategy_file(
        &self,
        files: &mut std::collections::HashMap<String, vike_journal::JournalStrategyFile>,
    ) {
        if let Some(file) = &self.strategy_file {
            let mount_id = vike_core::strategy_state::mount_id_with(
                self.controller_id.as_deref(),
                &self.venue,
                &self.symbol,
                &self.interval,
            );
            files.insert(mount_id, file.clone());
        }
    }
}

/// The SINGLE-VENUE round-trip maker fee of `(venue, symbol)`'s STATIC fee lane: what a maker's
/// break-even floor is armed from at mount (`vike_market_making::SpreadMakerPreset::arms_fee_floor`). Lane-keyed
/// like `paper_client_for`'s paper book (on binance/aster the SYMBOL picks the order API,
/// `vike_venues::fee_lane`'s `.P` split), so the floor and the simulated fills read the SAME schedule.
///
/// ⚠ It LOGS, because a fee nobody could name must not look like a fee of zero:
/// [`vike_model::maker_round_trip_fee`] refuses by SHAPE (`Free` FX/CFD, ibkr per-share,
/// polymarket's `p(1−p)`, deribit's premium cap) with `None`, which arms no floor. So once, at
/// mount: armed at `info`, unarmed at `warn` naming venue, lane and shape.
///
/// ⚠ The rate is the STATIC registry row. That is the whole answer for a PAPER mount, which has no
/// venue account to report a rate; a LIVE mount starts from it and is re-armed at its account's own
/// resolved rate once the engine has read it (`vike_market_making::rearm_fee_floor` through a mount's
/// [`crate::FeeRearm`], [`crate::build_live_multi_strategy_core_armed`]), the rate
/// `crate::resolve_fee_schedule` prefers.
pub fn maker_round_trip_fee_for(venue: &str, symbol: &str) -> Option<f64> {
    let lane = crate::engine::kept_fee_lane(venue, symbol);
    let schedule = vike_venues::fee_schedule_for(lane);
    match vike_model::maker_round_trip_fee(schedule) {
        Some(rate) => {
            tracing::info!(
                venue,
                symbol,
                fee_lane = lane,
                round_trip_fee_bps = rate * 1e4,
                "maker break-even floor ARMED: a quote narrower than ½·fee·mid will not be posted"
            );
            Some(rate)
        }
        None => {
            tracing::warn!(
                venue,
                symbol,
                fee_lane = lane,
                ?schedule,
                "maker break-even floor NOT armed: this venue's fee shape has no flat fraction of \
                 price, so nothing here can say what a round trip costs. The maker will quote \
                 WITHOUT a break-even floor — this is an absent bar, NOT a zero fee."
            );
            None
        }
    }
}
