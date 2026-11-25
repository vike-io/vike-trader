//! **A backtest profile's shape as the sides that cannot link the engine need it**: the top-level
//! keys a profile may write ([`PROFILE_TOP_LEVEL_KEYS`]) and the cash a run starts with when none is
//! written ([`DEFAULT_RUN_CASH`]).
//!
//! The profile itself is `vike_backtest`'s `ProfileBacktest`, which `vike-cli` (it checks a `--set`
//! key and implies the defaults before any dial or spawn) and the simulator's engine config cannot
//! both name. What they must agree on is here, beside [`super::paramscan::PARAMSCAN_SECTIONS`], the
//! section roster one of these keys owns; the engine's side holds it to the real deserializer.

/// **Every top-level key a backtest profile takes**, in `ProfileBacktest`'s declaration order:
/// serde's own field names, `deny_unknown_fields` at that level, so a key not listed here is refused
/// at load. `base_dir` is absent on purpose: it is `#[serde(skip)]`, never a TOML key.
///
/// ⚠ `inputs` is a key the PROFILE takes and a `--set` does not: its keys are series names (`7203.T`)
/// that `--set` would split on `.`, so a reader that builds the `--set` roster leaves it out and says so.
pub const PROFILE_TOP_LEVEL_KEYS: [&str; 10] = [
    "name",
    "data",
    "engine",
    "strategy",
    "mounts",
    "risk",
    "paramscan",
    "walkforward",
    "permutation",
    "inputs",
];

/// **The cash a run starts with when its profile writes no `engine.cash`**: the CLI's implied
/// default and the simulator's own engine default, one number, in the account's money.
pub const DEFAULT_RUN_CASH: f64 = 10_000.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// The ten keys, pinned in order (a published roster renders them), with the one section a
    /// parameter-search grid is written under among them, and the run's default cash.
    #[test]
    fn the_profile_keys_and_the_default_cash_are_pinned() {
        assert_eq!(
            PROFILE_TOP_LEVEL_KEYS.join(","),
            "name,data,engine,strategy,mounts,risk,paramscan,walkforward,permutation,inputs"
        );
        for section in super::super::paramscan::PARAMSCAN_SECTIONS {
            assert!(PROFILE_TOP_LEVEL_KEYS.contains(&section), "{section}");
        }
        assert_eq!(DEFAULT_RUN_CASH.to_bits(), 10_000.0_f64.to_bits());
    }
}
