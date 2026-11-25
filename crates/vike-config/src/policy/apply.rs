//! `Policy::apply`, which folds one layer's patch in naming the file, and its refusal helpers.

use std::path::Path;

use crate::error::{ConfigError, stale_row_removal};
use crate::refusals::ConfigRefusal;

use super::patch::PolicyPatch;
use super::{
    DEADMAN_DISABLED_MS, LINK_DEADMAN_DISABLED_MS, MAX_DEADMAN_TIMEOUT_MS,
    MAX_LINK_DEADMAN_GRACE_MS, MAX_MARKET_SLIPPAGE, MIN_DEADMAN_TIMEOUT_MS,
    MIN_LINK_DEADMAN_GRACE_MS, MIN_MARKET_SLIPPAGE, Policy,
};

impl Policy {
    /// Fold one file's patch in, validating each named value against `file` so the error can
    /// point at the exact file the operator must open.
    ///
    /// Validation happens HERE — at apply time, per layer — rather than once at the end, for
    /// exactly that reason: a merged value has no file attached to it any more, and
    /// `"max_leverage = 0.5 is below the minimum"` without a filename is a grep, not a fix.
    pub(crate) fn apply(&mut self, patch: PolicyPatch, file: &Path) -> Result<(), ConfigError> {
        // The leverage CEILING. REFUSED, never clamped, at the write and at the load (this is the
        // one apply both take): a ceiling that was silently raised to 1x is a limit the operator
        // believes they set and do not have, and since it NARROWS the run profile's leverage
        // (`vike_model::ProfileRisk::narrowed_to_leverage_ceiling`) a nonsense value must never
        // reach the fold. `>= 1.0` rather than `> 0`: `initial_margin = 1/leverage` is a margin FRACTION, so a
        // leverage under 1 would demand more margin than the notional, and `0` divides by zero.
        if let Some(v) = patch.max_leverage {
            if !v.is_finite() {
                return Err(ConfigError::value(
                    file,
                    "max_leverage",
                    ConfigRefusal::NotFinite { value: v },
                ));
            }
            if v < 1.0 {
                return Err(ConfigError::value(
                    file,
                    "max_leverage",
                    ConfigRefusal::LeverageCeilingBelowOne { value: v },
                ));
            }
            self.max_leverage = Some(v);
        }
        // TOMBSTONE — see `PolicyPatch::max_total_exposure`. Refused, never applied: this key was
        // a ceiling nothing read, so accepting it would re-create exactly the false belief its
        // removal exists to end. The message names the authority that DOES enforce the concept.
        if let Some(v) = patch.max_total_exposure {
            return Err(ConfigError::value(
                file,
                "max_total_exposure",
                ConfigRefusal::RetiredMaxTotalExposure {
                    value: v,
                    removal: stale_row_removal("policy.max_total_exposure"),
                },
            ));
        }
        if let Some(v) = patch.max_notional_per_order {
            check_positive(file, "max_notional_per_order", v)?;
            self.max_notional_per_order = Some(v);
        }
        if let Some(v) = patch.max_account_exposure {
            check_positive(file, "max_account_exposure", v)?;
            self.max_account_exposure = Some(v);
        }
        if let Some(v) = patch.max_sizing_equity {
            check_positive(file, "max_sizing_equity", v)?;
            self.max_sizing_equity = Some(v);
        }
        if let Some(v) = patch.market_slippage {
            const KEY: &str = "market_slippage";
            if !v.is_finite() {
                return Err(ConfigError::value(file, KEY, ConfigRefusal::NotFinite { value: v }));
            }
            // REJECTED, not clamped: a band is the worst price an emulated market order may reach,
            // and a silently-corrected one is a limit the operator believes they set and does not
            // have. `vike_bridge_core::market_slippage::resolve_market_slippage` clamps instead,
            // because by then there is no file to name — this edge is the one that can say WHERE.
            if v > MAX_MARKET_SLIPPAGE {
                return Err(ConfigError::value(
                    file,
                    KEY,
                    ConfigRefusal::MarketSlippageAboveMaximum {
                        value: v,
                        max: MAX_MARKET_SLIPPAGE,
                    },
                ));
            }
            if v < MIN_MARKET_SLIPPAGE {
                return Err(ConfigError::value(
                    file,
                    KEY,
                    ConfigRefusal::MarketSlippageBelowMinimum {
                        value: v,
                        min: MIN_MARKET_SLIPPAGE,
                    },
                ));
            }
            self.market_slippage = Some(v);
        }
        // No numeric validation: the value space is the enum, and an illegal spelling has already
        // failed at deserialization with the legal set in the message. An unmentioned key inherits,
        // which for this field means the compiled-in `admit` — today's behaviour on every venue.
        if let Some(v) = patch.halt_admit {
            self.halt_admit = v;
        }
        // The dead-man timeout. REJECTED at the file, never clamped, on both sides: the value
        // becomes `vike_core::DeadManSilenceConfig::timeout` verbatim (that type clamps only a zero to
        // 1 ms, and a zero never reaches it — it is the explicit-off spelling, folded to `None` by
        // the composition root, as an absent key is). A sub-second value is a false halt on any
        // quiet second; a multi-day one is a switch that will never fire. Both are typos an
        // operator must be told about, and this edge is the one that can say WHERE. An unmentioned
        // key INHERITS the layer below — which for the first layer is `None`, so a file that
        // never names the key leaves it absent, and the mount's warning can see that it was.
        if let Some(v) = patch.deadman_timeout_ms {
            const KEY: &str = "deadman_timeout_ms";
            if v != DEADMAN_DISABLED_MS && v < MIN_DEADMAN_TIMEOUT_MS {
                return Err(ConfigError::value(
                    file,
                    KEY,
                    ConfigRefusal::DeadmanTimeoutBelowMinimum {
                        value: v,
                        min: MIN_DEADMAN_TIMEOUT_MS,
                        off: DEADMAN_DISABLED_MS,
                    },
                ));
            }
            if v > MAX_DEADMAN_TIMEOUT_MS {
                return Err(ConfigError::value(
                    file,
                    KEY,
                    ConfigRefusal::DeadmanTimeoutAboveMaximum {
                        value: v,
                        max: MAX_DEADMAN_TIMEOUT_MS,
                    },
                ));
            }
            self.deadman_timeout_ms = Some(v);
        }
        // No numeric validation: the value space is the enum, and an illegal spelling has already
        // failed at deserialization with the legal set in the message.
        if let Some(v) = patch.deadman_action {
            self.deadman_action = v;
        }
        // The LINK dead-man grace. Same edge, same refuse-never-clamp discipline as the timeout
        // above, and DIFFERENT numbers because it bounds a different quantity: the floor is one
        // ordinary reconnect (below it the switch trips on the venue's own backoff) and the ceiling
        // is an hour (a reported-down link with orders resting is the thing this ends). ⚠ An
        // unmentioned key inherits, and the first layer's `None` means ARMED at
        // `DEFAULT_LINK_DEADMAN_GRACE_MS` — the composition root resolves that, not this edge, so
        // a file that never names the key is indistinguishable here from one that could not.
        if let Some(v) = patch.link_deadman_grace_ms {
            const KEY: &str = "link_deadman_grace_ms";
            if v != LINK_DEADMAN_DISABLED_MS && v < MIN_LINK_DEADMAN_GRACE_MS {
                return Err(ConfigError::value(
                    file,
                    KEY,
                    ConfigRefusal::LinkDeadmanGraceBelowMinimum {
                        value: v,
                        min: MIN_LINK_DEADMAN_GRACE_MS,
                        off: LINK_DEADMAN_DISABLED_MS,
                    },
                ));
            }
            if v > MAX_LINK_DEADMAN_GRACE_MS {
                return Err(ConfigError::value(
                    file,
                    KEY,
                    ConfigRefusal::LinkDeadmanGraceAboveMaximum {
                        value: v,
                        max: MAX_LINK_DEADMAN_GRACE_MS,
                    },
                ));
            }
            self.link_deadman_grace_ms = Some(v);
        }
        // The per-mount LOSS BUDGET. Refused, never clamped, at the write and at the load: `0` would
        // latch every mount on its first loss (a halt spelled sideways — omit the key for no budget),
        // and a negative or non-finite figure is meaningless. Not `check_positive`: its message talks
        // about denying orders, which this ceiling does not do.
        if let Some(v) = patch.max_mount_loss {
            const KEY: &str = "max_mount_loss";
            if !v.is_finite() {
                return Err(ConfigError::value(file, KEY, ConfigRefusal::NotFinite { value: v }));
            }
            if v <= 0.0 {
                return Err(ConfigError::value(
                    file,
                    KEY,
                    ConfigRefusal::MountLossNotPositive { value: v },
                ));
            }
            self.max_mount_loss = Some(v);
        }
        // No numeric validation: the value space is the enum, and an illegal spelling has already
        // failed at deserialization with the legal set in the message.
        if let Some(v) = patch.mount_loss_action {
            self.mount_loss_action = v;
        }
        // The per-mount LEVERAGE CAP. Refused, never clamped, at the write and at the load, with
        // `max_leverage`'s floor and for its reason: a cap under 1x is not a leverage.
        if let Some(v) = patch.max_mount_leverage {
            const KEY: &str = "max_mount_leverage";
            if !v.is_finite() {
                return Err(ConfigError::value(file, KEY, ConfigRefusal::NotFinite { value: v }));
            }
            if v < 1.0 {
                return Err(ConfigError::value(
                    file,
                    KEY,
                    ConfigRefusal::MountLeverageBelowOne { value: v },
                ));
            }
            self.max_mount_leverage = Some(v);
        }
        // TOMBSTONE — see `PolicyPatch::rate`. Refused, never applied: this ceiling clamped
        // `Preferences::rate_utilization`, which nothing read, so it bounded nothing.
        if let Some(v) = patch.rate.and_then(|rate| rate.max_utilization) {
            return Err(ConfigError::value(
                file,
                "rate.max_utilization",
                ConfigRefusal::RetiredRateMaxUtilization {
                    value: v,
                    removal: stale_row_removal("policy.rate.max_utilization"),
                },
            ));
        }
        Ok(())
    }
}

/// A ceiling denominated in money must be a finite, strictly positive number: `0` would deny
/// every order (a silent halt), and a negative one is meaningless.
fn check_positive(file: &Path, key: &str, v: f64) -> Result<(), ConfigError> {
    if !v.is_finite() {
        return Err(ConfigError::value(file, key, ConfigRefusal::NotFinite { value: v }));
    }
    if v <= 0.0 {
        return Err(ConfigError::value(file, key, ConfigRefusal::CeilingNotPositive { value: v }));
    }
    Ok(())
}
