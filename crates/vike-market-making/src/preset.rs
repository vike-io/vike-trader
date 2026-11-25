//! What a MOUNT hands the maker: the two PRESETS a profile names (`preset = "…"`, the bag the spread
//! maker's `from_params` starts from) and [`rearm_fee_floor`], the break-even floor moved to the
//! mount's own account fee rate. The maker, its reader and its whole `[strategy.params]` key set
//! (`SpreadMaker::PARAM_KEYS`) are the strategy file `strategies/market_making/spread_maker.rs`.
//!
//! The presets were `vike_tradehub_core::MakerConfig::{outcome_token, crypto}` until step 10 of the
//! one-broker plan: A-S defaults are maker knowledge, so they live beside the maker, and a daemon's
//! maker profile is an ordinary params table over the preset it names. Two numbers a preset cannot
//! hold, because they are VENUE facts the mount supplies: its price grid (`tick_size`, which the
//! mount's feed sizes its book on too) and the static fee lane the crypto preset arms its floor from
//! ([`SpreadMakerPreset::arms_fee_floor`]).

use vike_model::FeeSchedule;
use vike_model::params::{ParamValue, Params};

/// A named starting bag for the spread maker's `from_params`: the A-S knobs one PRICE DOMAIN needs.
/// The table's own keys override it, key by key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpreadMakerPreset {
    /// `[0,1]` OUTCOME-TOKEN prices: [`vike_model::AsParams::default`] (the Bernoulli variance cap,
    /// the `[tick, 1−tick]` wall clamp, the time-to-resolution horizon anchored on the table's
    /// `resolution_ts`, else the constant `tau_hold`), a 20-share clip on a `0.01` grid. The tuning
    /// belongs to the price domain, not to a venue (docs/decisions/0098).
    OutcomeToken,
    /// An UNBOUNDED `$`-priced asset (a BTC/ETH perp mid ~$64k, an equity). The A-S knobs and WHY:
    /// - `variance_mode = "raw_local"`: the Bernoulli cap `p(1−p)` goes NEGATIVE for a price `> 1`.
    /// - `horizon_mode = "constant_tau"`: an open-ended market never resolves.
    /// - `price_domain = "unbounded"`: no `[tick, 1−tick]` clamp; only the standoff + `bid < s < ask`.
    /// - `min_half_spread_ticks = 2`: the intensity half-spread is SUB-TICK at `$`-scale (~0.02 at a
    ///   $64k mid on a $1 tick), so both sides snapped onto ONE tick and the quote collapsed to
    ///   `None` (the live HL/BTC no-quote bug).
    /// - `max_half_spread_ticks = 60`: `½·γ·σ̂²·H` grows as the SQUARE of the move, so a spike would
    ///   post off the book exactly when spreads are richest. ~$60 half / ~19 bp of a $64k mid keeps
    ///   the widening through an ordinary move (~$9/tick) and stays fillable in a spike (pinned by
    ///   this crate's `crypto_width_tuning_tests` workbench).
    /// - `round_trip_fee_rate`: ARMED BY THE MOUNT from the venue's fee schedule when the table
    ///   states none ([`Self::arms_fee_floor`]), the one knob the library default leaves OFF: the
    ///   half-spread is floored at the BREAK-EVEN width `½·m·s`, and a mount that cannot cover its
    ///   round trip under the cap posts NOTHING ([`vike_model::AsParams::round_trip_fee_rate`];
    ///   `crate::avellaneda`'s `bounded_half_spread` for why it refuses instead of widening). ⚠ Armed
    ///   by DEFAULT because the mount it was written for was measured losing money on every
    ///   completed round trip: a silent halt beats a silent bleed, and the halt is attributable (the
    ///   maker's edge-triggered `warn!`, the startup `effective_params` line). A fee SHAPE with no
    ///   flat fraction of price arms nothing and says so, never a silent `0.0`.
    ///
    /// `gamma`/`q_scale` are STARTING values (from the width workbench; re-tune freely):
    /// - `gamma = 5e-4`: the vol half-spread is `½·γ·V`, `V = σ̂²·H` in PRICE². A BTC-scale `σ̂²·H` at
    ///   the default `tau_hold_ms` is O(1e3–1e4), so a small `γ` keeps it to a few ticks.
    /// - `q_scale = 3e-2`: `q_norm = position/q_scale`; the outcome-token `100` (a SHARE count) would
    ///   make a ~0.005 BTC clip `q_norm ≈ 5e-5` and kill the skew. Per clip the reservation skew is
    ///   `(0.005/0.03)·γV ≈ 0.17·γV`, a THIRD of the `0.5·γV` half-spread: ~3 clips (≈0.015 BTC)
    ///   shift it a full half-spread, ~6 pull the risk-reducing side to the mid.
    ///
    /// Its clip is `0.005` on a `1.0` grid, and its fixed-spread seed `half_spread` (unused while
    /// A-S prices) is the reader's `2·tick_size` default.
    Crypto,
}

impl SpreadMakerPreset {
    /// The `preset` word that names it.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            SpreadMakerPreset::OutcomeToken => "outcome_token",
            SpreadMakerPreset::Crypto => "crypto",
        }
    }

    /// Whether a mount arms this preset's break-even floor from its venue's fee schedule when the
    /// table states no `round_trip_fee_rate` ([`Self::Crypto`]'s doc).
    #[must_use]
    pub fn arms_fee_floor(self) -> bool {
        self == SpreadMakerPreset::Crypto
    }

    /// The preset's own keys, as the bag the spread maker's `from_params` reads.
    #[must_use]
    pub fn params(self) -> Params {
        let mut p = Params::new();
        match self {
            SpreadMakerPreset::OutcomeToken => {
                p.insert("qty", 20.0);
                p.insert("tick_size", 0.01);
                p.insert("half_spread", 0.01);
            }
            SpreadMakerPreset::Crypto => {
                p.insert("qty", 0.005);
                p.insert("tick_size", 1.0);
                p.insert("variance_mode", "raw_local");
                p.insert("horizon_mode", "constant_tau");
                p.insert("price_domain", "unbounded");
                p.insert("min_half_spread_ticks", 2.0);
                p.insert("max_half_spread_ticks", 60.0);
                p.insert("gamma", 5e-4);
                p.insert("q_scale", 3e-2);
            }
        }
        p
    }

    /// `table`'s keys over [`Self::params`].
    pub fn under(self, table: &Params) -> Params {
        let mut bag = self.params();
        for (k, v) in table {
            bag.insert(k.clone(), v.clone());
        }
        bag
    }
}

/// The maker `params` describes, with its break-even floor moved to `schedule`, the fee schedule its
/// mount's account resolved (the venue's reported rate, else the static lane): `Some(bag)` to
/// rebuild the maker from when the floor MOVES, `None` to keep the maker the mount was built with,
/// bit for bit. Kept, with a `warn`, when no engine of the account answered (`schedule` `None`) or
/// the schedule's SHAPE has no flat fraction of price ([`vike_model::maker_round_trip_fee`]'s
/// refusals) while a floor is held: that floor is then the best bar there is. `venue`/`symbol` only
/// name the mount in those lines.
#[must_use]
pub fn rearm_fee_floor(
    params: &Params,
    schedule: Option<FeeSchedule>,
    venue: &str,
    symbol: &str,
) -> Option<Params> {
    let held = params.get("round_trip_fee_rate").and_then(ParamValue::as_f64);
    let Some(schedule) = schedule else {
        tracing::warn!(
            venue,
            symbol,
            "maker break-even floor kept at the static fee lane: no engine of the mount's account \
             resolved a fee rate"
        );
        return None;
    };
    match vike_model::maker_round_trip_fee(schedule) {
        Some(rate) if held != Some(rate) => {
            tracing::info!(
                venue,
                symbol,
                round_trip_fee_bps = rate * 1e4,
                "maker break-even floor ARMED from the account's own fee rate"
            );
            let mut armed = params.clone();
            armed.insert("round_trip_fee_rate", rate);
            Some(armed)
        }
        Some(_) => None,
        None => {
            if held.is_some() {
                tracing::warn!(
                    venue,
                    symbol,
                    ?schedule,
                    "maker break-even floor kept at the static fee lane: the account's fee rate has \
                     no flat fraction of price"
                );
            }
            None
        }
    }
}
