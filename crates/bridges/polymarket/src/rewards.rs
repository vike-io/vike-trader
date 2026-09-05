//! `rewards` — Polymarket **liquidity-rewards** config, parsed off the CLOB/Gamma market objects.
//! No Python twin — new capability, entirely additive: [`RewardsConfig`] defaults to all-zero /
//! disabled, which is exactly a non-rewarded market, so attaching one to a market type changes
//! nothing until a rewarded market populates it.
//!
//! This is the PARSE half of rewards-aware quoting (the vike-mm quoting term is a separate lane).
//! It reads the reward parameters a market publishes so a maker can decide whether — and how tight
//! / how large / how patiently — to quote to actually earn the rewards.
//!
//! ## The field names differ per endpoint (live-probed 2026-07-22, via arbdub)
//! The SAME logical config is spelled three different ways on the wire. Each constructor reads ONE
//! shape and leaves the fields that shape does not carry at their zero default:
//!
//! - **CLOB `GET /markets` / `/sampling-markets` / `/simplified-markets`** — a nested `rewards`
//!   object: `{ rates: [{ asset_address, rewards_daily_rate }], min_size, max_spread }`. `rates` is
//!   `null` and `min_size`/`max_spread` are `0` on a non-rewarded market. This is the only shape that
//!   carries the **daily rate**. → [`RewardsConfig::from_clob_rewards`].
//! - **CLOB `GET /clob-markets/{condition_id}`** (the compact market object) — a nested `r` object:
//!   `{ mi (min size), ma (max spread, CENTS), e (enabled bool), moas (min order AGE seconds,
//!   observed 30) }`. This is the only shape that carries **`moas`**, which is load-bearing: an order
//!   re-quoted faster than `moas` seconds earns ZERO rewards. → [`RewardsConfig::from_compact`].
//! - **Gamma `/markets`** — flat `rewardsMinSize` / `rewardsMaxSpread` fields on the market object
//!   ([`crate::gamma::GammaMarket`] carries these). No daily rate, no `moas`. →
//!   [`RewardsConfig::from_gamma_market`].
//!
//! ⚠ The doc-prose field names `min_incentive_size` / `max_incentive_spread` are NOT the wire names —
//! they appear only in Polymarket's written docs. The wire names are the ones above.
//!
//! **Units.** `max_spread` is in **cents** on every wire shape (the compact `ma` is explicitly cents;
//! the CLOB `max_spread` and Gamma `rewardsMaxSpread` express the same value). It is stored VERBATIM —
//! no arithmetic conversion happens here, so the parse is a pure read.

use serde_json::Value;
use vike_bridge_core::json::{get_f64, get_i64};

/// One market's liquidity-rewards parameters. All fields default to zero / `false`, which is exactly
/// a market that runs no reward program — so a defaulted value is inert and byte-identical to "no
/// rewards known". A given constructor fills only the fields its wire shape carries (see the module
/// doc); use [`earns_rewards`](Self::earns_rewards) as the shape-independent "is there a program?"
/// predicate rather than reading any single field.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RewardsConfig {
    /// Minimum resting order size (shares) that qualifies for rewards. CLOB `min_size` / compact
    /// `mi` / Gamma `rewardsMinSize`. `0.0` = none / unknown.
    pub min_size: f64,
    /// Maximum spread from the midpoint, in **cents**, within which a quote qualifies. CLOB
    /// `max_spread` / compact `ma` / Gamma `rewardsMaxSpread`. Stored verbatim (no conversion).
    /// `0.0` = none / unknown.
    pub max_spread: f64,
    /// Total daily reward rate, summed over the reward tokens in the CLOB `rewards.rates` array
    /// (`rewards_daily_rate`). `0.0` on the compact / Gamma shapes, which do not carry it.
    pub daily_rate: f64,
    /// Minimum order **age** in seconds before a resting order begins scoring — the compact `moas`
    /// (observed 30). LOAD-BEARING: a maker that re-quotes faster than this earns nothing. `0` when
    /// the shape does not carry it (CLOB `/markets`, Gamma) — treat `0` as "unknown", not "no delay".
    pub min_order_age_secs: u64,
    /// The reward program's active flag — the compact `e`. Derived as "the CLOB `rates` array is
    /// non-empty" for the CLOB shape; left `false` for Gamma (which carries no active signal). Prefer
    /// [`earns_rewards`](Self::earns_rewards) over reading this directly across shapes.
    pub enabled: bool,
}

impl RewardsConfig {
    /// Does this market appear to run a liquidity-reward program? Shape-independent: `true` if the
    /// explicit `enabled` flag is set OR any of the reward parameters is non-zero. A defaulted
    /// (non-rewarded) config returns `false`.
    pub fn earns_rewards(&self) -> bool {
        self.enabled
            || self.min_size > 0.0
            || self.max_spread > 0.0
            || self.daily_rate > 0.0
            || self.min_order_age_secs > 0
    }

    /// Parse from a **Gamma** market object's flat `rewardsMinSize` / `rewardsMaxSpread` fields
    /// (string-or-number tolerant, via `json::get_f64`). Gamma carries neither the daily rate nor
    /// `moas`, so those stay `0`. A market with neither field yields [`RewardsConfig::default`].
    pub fn from_gamma_market(market: &Value) -> Self {
        Self {
            min_size: get_f64(market, "rewardsMinSize"),
            max_spread: get_f64(market, "rewardsMaxSpread"),
            ..Default::default()
        }
    }

    /// Parse from a CLOB market's nested `rewards` object (`GET /markets`, `/sampling-markets`,
    /// `/simplified-markets`): `{ rates: [{ rewards_daily_rate, .. }], min_size, max_spread }`. The
    /// daily rate is the sum of `rewards_daily_rate` over the `rates` array (naive left-to-right
    /// fold, wire order — this is NOT a parity-gated site). `enabled` is derived from a non-empty
    /// `rates` array. A `null` / absent `rates` with zero `min_size`/`max_spread` (a non-rewarded
    /// market) yields [`RewardsConfig::default`].
    pub fn from_clob_rewards(rewards: &Value) -> Self {
        let rates = rewards.get("rates").and_then(|v| v.as_array());
        let daily_rate = rates
            .map(|arr| arr.iter().map(|e| get_f64(e, "rewards_daily_rate")).sum::<f64>())
            .unwrap_or(0.0);
        Self {
            min_size: get_f64(rewards, "min_size"),
            max_spread: get_f64(rewards, "max_spread"),
            daily_rate,
            min_order_age_secs: 0,
            enabled: rates.is_some_and(|arr| !arr.is_empty()),
        }
    }

    /// Parse from a CLOB `GET /clob-markets/{condition_id}` compact `r` object:
    /// `{ mi, ma, e, moas }`. This is the only shape that carries `moas` (min order age). `moas` is
    /// clamped to a non-negative whole-seconds count. A `{}` / all-zero `r` yields
    /// [`RewardsConfig::default`] except that `daily_rate` is always `0` here (the compact shape has
    /// no rate).
    pub fn from_compact(r: &Value) -> Self {
        Self {
            min_size: get_f64(r, "mi"),
            max_spread: get_f64(r, "ma"),
            daily_rate: 0.0,
            min_order_age_secs: get_i64(r, "moas").max(0) as u64,
            enabled: r.get("e").and_then(|v| v.as_bool()).unwrap_or(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Gamma flat shape --------------------------------------------------------------------

    #[test]
    fn from_gamma_reads_flat_reward_fields() {
        // number form
        let m = serde_json::json!({ "rewardsMinSize": 50.0, "rewardsMaxSpread": 3.5 });
        let c = RewardsConfig::from_gamma_market(&m);
        assert_eq!(c.min_size, 50.0);
        assert_eq!(c.max_spread, 3.5);
        // Gamma carries neither of these
        assert_eq!(c.daily_rate, 0.0);
        assert_eq!(c.min_order_age_secs, 0);
        assert!(!c.enabled);
        assert!(c.earns_rewards());
        // string form is tolerated (Gamma string-encodes many numerics)
        let ms = serde_json::json!({ "rewardsMinSize": "100", "rewardsMaxSpread": "2" });
        let cs = RewardsConfig::from_gamma_market(&ms);
        assert_eq!(cs.min_size, 100.0);
        assert_eq!(cs.max_spread, 2.0);
    }

    #[test]
    fn from_gamma_without_reward_fields_is_default() {
        let m = serde_json::json!({ "question": "q", "conditionId": "0x1" });
        assert_eq!(RewardsConfig::from_gamma_market(&m), RewardsConfig::default());
        assert!(!RewardsConfig::from_gamma_market(&m).earns_rewards());
    }

    // ---- CLOB /markets nested `rewards{}` shape ----------------------------------------------

    #[test]
    fn from_clob_rewards_reads_rates_min_size_and_max_spread() {
        let rewards = serde_json::json!({
            "rates": [
                { "asset_address": "0xUSDC", "rewards_daily_rate": 1.0 },
                { "asset_address": "0xOTHER", "rewards_daily_rate": 0.5 }
            ],
            "min_size": 100.0,
            "max_spread": 3.0
        });
        let c = RewardsConfig::from_clob_rewards(&rewards);
        assert_eq!(c.min_size, 100.0);
        assert_eq!(c.max_spread, 3.0);
        // daily rate = Σ over the rates array (1.0 + 0.5)
        assert_eq!(c.daily_rate, 1.5);
        // a non-empty rates array marks the program active
        assert!(c.enabled);
        // compact-only field is absent here
        assert_eq!(c.min_order_age_secs, 0);
        assert!(c.earns_rewards());
    }

    #[test]
    fn from_clob_rewards_string_encoded_rate_and_sizes() {
        // the wire string-encodes numerics on some deployments — json::get_f64 tolerates it
        let rewards = serde_json::json!({
            "rates": [{ "rewards_daily_rate": "2.5" }],
            "min_size": "10",
            "max_spread": "1"
        });
        let c = RewardsConfig::from_clob_rewards(&rewards);
        assert_eq!(c.daily_rate, 2.5);
        assert_eq!(c.min_size, 10.0);
        assert_eq!(c.max_spread, 1.0);
        assert!(c.enabled);
    }

    #[test]
    fn from_clob_rewards_non_rewarded_is_default() {
        // the documented non-rewarded shape: rates null, min_size/max_spread 0
        let rewards = serde_json::json!({ "rates": null, "min_size": 0, "max_spread": 0 });
        assert_eq!(RewardsConfig::from_clob_rewards(&rewards), RewardsConfig::default());
        assert!(!RewardsConfig::from_clob_rewards(&rewards).earns_rewards());
        // an entirely empty object is likewise default
        assert_eq!(
            RewardsConfig::from_clob_rewards(&serde_json::json!({})),
            RewardsConfig::default()
        );
        // an empty rates array is NOT active and contributes no rate
        let empty = serde_json::json!({ "rates": [], "min_size": 0, "max_spread": 0 });
        let c = RewardsConfig::from_clob_rewards(&empty);
        assert!(!c.enabled);
        assert_eq!(c.daily_rate, 0.0);
    }

    // ---- CLOB /clob-markets compact `r{}` shape ----------------------------------------------

    #[test]
    fn from_compact_reads_mi_ma_e_and_moas() {
        let r = serde_json::json!({ "mi": 100.0, "ma": 3.0, "e": true, "moas": 30 });
        let c = RewardsConfig::from_compact(&r);
        assert_eq!(c.min_size, 100.0);
        assert_eq!(c.max_spread, 3.0); // cents, verbatim
        assert_eq!(c.min_order_age_secs, 30); // the load-bearing min-order-age
        assert!(c.enabled);
        // the compact shape carries no daily rate
        assert_eq!(c.daily_rate, 0.0);
        assert!(c.earns_rewards());
    }

    #[test]
    fn from_compact_non_rewarded_is_default() {
        let r = serde_json::json!({ "mi": 0, "ma": 0, "e": false, "moas": 0 });
        assert_eq!(RewardsConfig::from_compact(&r), RewardsConfig::default());
        assert_eq!(RewardsConfig::from_compact(&serde_json::json!({})), RewardsConfig::default());
        assert!(!RewardsConfig::from_compact(&r).earns_rewards());
        // a negative moas (never observed) clamps to 0 rather than wrapping the u64 cast
        let neg = serde_json::json!({ "moas": -5 });
        assert_eq!(RewardsConfig::from_compact(&neg).min_order_age_secs, 0);
    }

    #[test]
    fn from_compact_moas_alone_still_reads_as_a_program() {
        // moas>0 with no other signal still counts as a rewarded market (earns_rewards is the
        // shape-independent predicate)
        let r = serde_json::json!({ "moas": 30 });
        let c = RewardsConfig::from_compact(&r);
        assert_eq!(c.min_order_age_secs, 30);
        assert!(c.earns_rewards());
    }

    // ---- default / predicate ------------------------------------------------------------------

    #[test]
    fn default_is_non_rewarded() {
        let d = RewardsConfig::default();
        assert_eq!(d.min_size, 0.0);
        assert_eq!(d.max_spread, 0.0);
        assert_eq!(d.daily_rate, 0.0);
        assert_eq!(d.min_order_age_secs, 0);
        assert!(!d.enabled);
        assert!(!d.earns_rewards());
    }
}
