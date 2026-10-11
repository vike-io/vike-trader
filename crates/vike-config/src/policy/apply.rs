//! `Policy::apply`, which folds one layer's patch in naming the file, and its refusal helpers.

use std::path::Path;

use crate::error::{ConfigError, STALE_ROW_REMOVAL};

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
        if let Some(v) = patch.max_leverage {
            if !v.is_finite() {
                return Err(ConfigError::value(file, "max_leverage", v, "is not a finite number"));
            }
            if v < 1.0 {
                return Err(ConfigError::value(
                    file,
                    "max_leverage",
                    v,
                    "is below the minimum leverage 1 (1 = no leverage; a ceiling under 1x is not \
                     representable, and 0 would divide by zero deriving the margin requirement)",
                ));
            }
            self.max_leverage = v;
        }
        // TOMBSTONE — see `PolicyPatch::max_total_exposure`. Refused, never applied: this key was
        // a ceiling nothing read, so accepting it would re-create exactly the false belief its
        // removal exists to end. The message names the authority that DOES enforce the concept.
        if let Some(v) = patch.max_total_exposure {
            return Err(ConfigError::value(
                file,
                "max_total_exposure",
                v,
                &format!(
                    "is no longer a policy key — it was read by NOTHING here, so it capped \
                     nothing. Two ceilings replace it, and WHICH ONE YOU WANT DEPENDS ON WHAT YOU \
                     MEANT. Per INSTRUMENT: `max_total_exposure` in your RUN PROFILE's `[risk]` \
                     table (vike_model::ProfileRisk -> RiskLimits), which RiskGate evaluates on \
                     every order and which a live mount already refuses to start without — it caps \
                     ONE SYMBOL at ONE VENUE, so size it per instrument. Per ACCOUNT — the \
                     aggregate this key's NAME reads as: the `policy.max_account_exposure` row \
                     (`vike-cli config set`), which caps the whole account's projected open \
                     notional across every symbol and denies with `over-account-exposure`. \
                     {STALE_ROW_REMOVAL}"
                ),
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
                return Err(ConfigError::value(file, KEY, v, "is not a finite number"));
            }
            // REJECTED, not clamped: a band is the worst price an emulated market order may reach,
            // and a silently-corrected one is a limit the operator believes they set and does not
            // have. `vike_bridge_core::market_slippage::resolve_market_slippage` clamps instead,
            // because by then there is no file to name — this edge is the one that can say WHERE.
            if v > MAX_MARKET_SLIPPAGE {
                return Err(ConfigError::value(
                    file,
                    KEY,
                    v,
                    &format!(
                        "exceeds the allowed maximum {MAX_MARKET_SLIPPAGE} (a band is a FRACTION: \
                         0.002 = 0.2%). The maximum is the widest band already compiled in, so this \
                         key can only tighten a venue, never widen one"
                    ),
                ));
            }
            if v < MIN_MARKET_SLIPPAGE {
                return Err(ConfigError::value(
                    file,
                    KEY,
                    v,
                    &format!(
                        "is below the allowed minimum {MIN_MARKET_SLIPPAGE} — a band this tight is \
                         not marketable, so the emulated order cancels unfilled, which on a tripped \
                         stop is a protective exit that did not exit"
                    ),
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
        // becomes `vike_core::DeadManConfig::timeout` verbatim (that type clamps only a zero to
        // 1 ms, and a zero never reaches it — it is the explicit-off spelling, folded to `None` by
        // the composition root, as an absent key is). A sub-second value is a false halt on any
        // quiet second; a multi-day one is a switch that will never fire. Both are typos an
        // operator must be told about, and this edge is the one that can say WHERE. An unmentioned
        // key INHERITS the layer below — which for the first layer is `None`, so a file that
        // never names the key leaves it absent, and the mount's warning can see that it was.
        if let Some(v) = patch.deadman_timeout_ms {
            const KEY: &str = "deadman_timeout_ms";
            if v != DEADMAN_DISABLED_MS && v < MIN_DEADMAN_TIMEOUT_MS {
                return Err(ConfigError::Value {
                    file: file.to_path_buf(),
                    key: KEY.to_string(),
                    message: format!(
                        "{v} is below the minimum armed timeout {MIN_DEADMAN_TIMEOUT_MS} ms (one \
                         second). The switch trips on SILENCE across the whole core's ingest, and a \
                         live instrument goes sub-second stretches without a tick routinely — a \
                         timeout this short is a false halt on any quiet second. Write \
                         {DEADMAN_DISABLED_MS} to disable the switch outright, or \
                         {MIN_DEADMAN_TIMEOUT_MS} or more to arm it"
                    ),
                });
            }
            if v > MAX_DEADMAN_TIMEOUT_MS {
                return Err(ConfigError::Value {
                    file: file.to_path_buf(),
                    key: KEY.to_string(),
                    message: format!(
                        "{v} exceeds the maximum {MAX_DEADMAN_TIMEOUT_MS} ms (one day). A dead-man \
                         that waits longer than a day is not a dead-man — a feed silent that long \
                         has left a book resting through an entire session, which is what the \
                         switch exists to prevent. This is usually a unit slip (seconds written as \
                         milliseconds); the value is in MILLISECONDS"
                    ),
                });
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
                return Err(ConfigError::Value {
                    file: file.to_path_buf(),
                    key: KEY.to_string(),
                    message: format!(
                        "{v} is below the minimum armed grace {MIN_LINK_DEADMAN_GRACE_MS} ms \
                         (thirty seconds). An ORDINARY reconnect of the slowest lane that \
                         reports a dead link — the L2 depth driver — takes up to 23 s \
                         (vike_bridge_core::depth's CONNECT_10S dial, the venue's REST book seed \
                         and its 3 s DEPTH_BACKOFF), so a grace shorter than this cancels the \
                         book on a re-dial rather than on a dead link. Write \
                         {LINK_DEADMAN_DISABLED_MS} to disable the switch outright, or \
                         {MIN_LINK_DEADMAN_GRACE_MS} or more to arm it"
                    ),
                });
            }
            if v > MAX_LINK_DEADMAN_GRACE_MS {
                return Err(ConfigError::Value {
                    file: file.to_path_buf(),
                    key: KEY.to_string(),
                    message: format!(
                        "{v} exceeds the maximum {MAX_LINK_DEADMAN_GRACE_MS} ms (one hour). A link \
                         the bridge has reported DOWN for an hour, with orders resting behind it, \
                         is exactly what this switch exists to end — a grace longer than that is a \
                         switch that will never fire. This is usually a unit slip (seconds written \
                         as milliseconds); the value is in MILLISECONDS"
                    ),
                });
            }
            self.link_deadman_grace_ms = Some(v);
        }
        // TOMBSTONE — see `PolicyPatch::rate`. Refused, never applied: this ceiling clamped
        // `Preferences::rate_utilization`, which nothing read, so it bounded nothing.
        if let Some(v) = patch.rate.and_then(|rate| rate.max_utilization) {
            return Err(ConfigError::value(
                file,
                "rate.max_utilization",
                v,
                &format!(
                    "is no longer a policy key — it clamped `preferences.rate_utilization`, which \
                     was read by NOTHING, so this ceiling bounded nothing. Every pacer takes its \
                     target fraction from the compiled-in \
                     vike_model::rate_limits::DEFAULT_UTILIZATION instead (a \
                     `preferences.rate_utilization` row has the same problem). \
                     {STALE_ROW_REMOVAL}. The knob returns, per-venue, when \
                     vike_model::RateLimitConfig can reach a pacer"
                ),
            ));
        }
        Ok(())
    }
}

/// A ceiling denominated in money must be a finite, strictly positive number: `0` would deny
/// every order (a silent halt), and a negative one is meaningless.
fn check_positive(file: &Path, key: &str, v: f64) -> Result<(), ConfigError> {
    if !v.is_finite() {
        return Err(ConfigError::value(file, key, v, "is not a finite number"));
    }
    if v <= 0.0 {
        return Err(ConfigError::value(
            file,
            key,
            v,
            "must be greater than 0 (a ceiling of 0 denies every order — omit the key to leave \
             it uncapped)",
        ));
    }
    Ok(())
}
