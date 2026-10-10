//! `Policy::apply`, which folds one layer's patch in naming the file, and its refusal helpers.

use std::path::Path;
use vike_model::VENUES;
use vike_model::accounts::account_keys::{AccountLabel, parse_wire_account};

use crate::error::{ConfigError, STALE_ROW_REMOVAL};
use crate::venue_mode::{did_you_mean, legal_modes, roster_id};

use super::patch::PolicyPatch;
use super::{
    DEADMAN_DISABLED_MS, LINK_DEADMAN_DISABLED_MS, MAX_DEADMAN_TIMEOUT_MS,
    MAX_LINK_DEADMAN_GRACE_MS, MAX_MARKET_SLIPPAGE, MIN_DEADMAN_TIMEOUT_MS,
    MIN_LINK_DEADMAN_GRACE_MS, MIN_MARKET_SLIPPAGE, Policy, VenueMode,
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
        // The `[venues]` ceilings. The MODE has already been validated by serde (the value type is
        // an enum, so an illegal spelling never reaches here); what is left is the VENUE, which
        // `deny_unknown_fields` structurally cannot see — see `PolicyPatch::venues`.
        //
        // An unmentioned venue INHERITS, like every other key: a `[venues]` table naming two venues
        // leaves the other twelve at whatever the layer below said, which for the only layer that
        // exists is the `paper` default.
        if let Some(venues) = patch.venues {
            for (name, mode) in venues {
                let Some(id) = roster_id(&name) else {
                    return Err(unknown_venue(file, &name, mode));
                };
                self.venues.set(id, mode);
            }
        }
        // The `[accounts]` per-account ceilings. Same split as `[venues]` above, one level deeper:
        // serde has already refused an illegal MODE, so what is left is the VENUE and the LABEL.
        // An unnamed account inherits nothing — `VenuePolicy::account` resolves a labelled account
        // with no line to `paper`, which is the safe end and the one this file's absence must mean.
        if let Some(accounts) = patch.accounts {
            for (name, labelled) in accounts {
                let Some(id) = roster_id(&name) else {
                    return Err(unknown_account_venue(file, &name));
                };
                for (label, mode) in labelled {
                    let parsed = AccountLabel::parse(&label).map_err(|e| {
                        bad_account_label(file, &name, &label, mode, &e.to_string())
                    })?;
                    // `parse` returns `Named` for everything it accepts, so this is `Some`.
                    // Refusing rather than dropping the line keeps that assumption from failing
                    // silently if it ever stops holding.
                    let Some(text) = parsed.text() else {
                        return Err(bad_account_label(
                            file,
                            &name,
                            &label,
                            mode,
                            "resolves to the default account, whose ceiling is the venue's own line",
                        ));
                    };
                    self.venues.set_account(id, text, mode);
                }
            }
        }
        // The `[account_exposure]` per-account figures. Same three-way split as `[accounts]` above
        // — serde has refused a non-number, so the VENUE, the LABEL and the FIGURE are left — with
        // two differences, each argued at `PolicyPatch::account_exposure`: the label reader ADMITS
        // `DEFAULT` (the unlabelled account has no venue line to inherit a figure from), and the
        // figure itself is checked, because the fold is a `min` and a negative or non-finite
        // ceiling would become the binding one and refuse every order on that account.
        if let Some(caps) = patch.account_exposure {
            for (name, labelled) in caps {
                let Some(id) = roster_id(&name) else {
                    return Err(unknown_exposure_venue(file, &name));
                };
                for (label, cap) in labelled {
                    let parsed = parse_wire_account(&label).map_err(|e| {
                        bad_exposure_label(file, &name, &label, cap, &e.to_string())
                    })?;
                    if !cap.is_finite() || cap <= 0.0 {
                        return Err(bad_exposure_figure(file, &name, &label, cap));
                    }
                    // ⚠ **A LABELLED account must be armed before it can be capped**, and this is
                    // a refusal rather than a silent accept for two reasons that point the same
                    // way. It would mean NOTHING: a labelled account with no `[accounts]` line
                    // resolves to `paper` (`VenuePolicy::account`'s `(None, false)` arm), and a
                    // ceiling on an account that trades nothing caps nothing. And it would be
                    // LOST: the mirror writes one `venue_arming` row per account the `[accounts]`
                    // table names, so a figure with no mode line beside it has no row to ride and
                    // would vanish the moment the store became the authority. The unlabelled
                    // account is exempt — every roster venue always has a row.
                    if let Some(text) = parsed.text()
                        && self.venues.account_stated(id, text).is_none()
                    {
                        return Err(unarmed_exposure_account(file, &name, text, cap));
                    }
                    // `parse_wire_account` answers `Default` for the reserved spelling and `Named`
                    // for everything else, so this covers both and the key is the label TEXT.
                    let text = parsed
                        .text()
                        .unwrap_or(vike_model::accounts::account_keys::RESERVED_DEFAULT_LABEL);
                    self.venues.set_account_exposure(id, text, cap);
                }
            }
        }
        Ok(())
    }
}

/// The refusal for an `[account_exposure]` table naming no venue — [`unknown_account_venue`]'s twin,
/// separate for that function's own reason: an operator greps for the key they typed, and the two
/// tables are typed in different places.
fn unknown_exposure_venue(file: &Path, name: &str) -> ConfigError {
    let hint = match did_you_mean(name) {
        Some(id) if name.to_ascii_lowercase() == id => {
            format!(" — venue ids are lower-case, so write `{id}`")
        }
        Some(id) => format!(" — did you mean `{id}`?"),
        None => String::new(),
    };
    ConfigError::Value {
        file: file.to_path_buf(),
        key: format!("account_exposure.{name}"),
        message: format!(
            "`{name}` is not a venue this build knows{hint}. The roster is: {}",
            VENUES.join(", ")
        ),
    }
}

/// The refusal for a label `parse_wire_account` will not take. ⚠ Unlike `[accounts]`'s twin this
/// one ACCEPTS `DEFAULT`, so the message must not repeat that table's "the venue's own line" advice
/// — here the unlabelled account is a legal subject and saying otherwise would send an operator to
/// delete a line that is correct.
fn bad_exposure_label(file: &Path, venue: &str, label: &str, cap: f64, why: &str) -> ConfigError {
    ConfigError::Value {
        file: file.to_path_buf(),
        key: format!("account_exposure.{venue}.{label}"),
        message: format!(
            "`{label}` (= {cap}) is not a legal account name — {why}. Labels are the \
             `__SUFFIX` of a credential key (`BINANCE_LIVE_API_KEY__ALT` is account `ALT`), and \
             `DEFAULT` names the unlabelled account, which IS allowed here"
        ),
    }
}

/// The refusal for a figure that would bind rather than cap. A `0` or a negative ceiling passes
/// serde and then refuses every order on that account, which reads as a broken venue rather than as
/// a settings mistake — so it is named here, where the file and the key are still in hand.
fn bad_exposure_figure(file: &Path, venue: &str, label: &str, cap: f64) -> ConfigError {
    ConfigError::Value {
        file: file.to_path_buf(),
        key: format!("account_exposure.{venue}.{label}"),
        message: format!(
            "{cap} is not a usable exposure ceiling — it must be a positive, finite number. The \
             fold is a `min`, so this figure would become the BINDING ceiling and refuse every \
             order on that account. To switch the lane off for this account, delete the line \
             rather than writing zero"
        ),
    }
}

/// The refusal for an `[accounts]` table naming no venue — the `[venues]` refusal one level down,
/// and separate from it because the KEY it must blame is different and an operator greps for the
/// key they typed.
fn unknown_account_venue(file: &Path, name: &str) -> ConfigError {
    let hint = match did_you_mean(name) {
        Some(id) if name.to_ascii_lowercase() == id => {
            format!(" Did you mean `{id}`? Venue ids are lowercase.")
        }
        Some(id) => format!(" Did you mean `{id}`?"),
        None => String::new(),
    };
    ConfigError::Value {
        file: file.to_path_buf(),
        key: format!("accounts.{name}"),
        message: format!(
            "names no venue vike has a bridge for, so every account ceiling under it would apply \
             to NOTHING while reading like it applies to something.{hint} The venues are: {}.",
            VENUES.join(", "),
        ),
    }
}

/// The refusal for an `[accounts]` key that is not a legal account label.
///
/// Refused rather than ignored, for the reason every refusal in this file is: an operator who
/// wrote `[accounts.bybit] alt = "paper"` believes they capped an account, while the credential
/// store they wrote the labelled key into spells the label the other way. A loader that silently
/// dropped the line — or quietly uppercased it — would hand them positive confirmation of a ceiling
/// they do not have, and here the belief runs the dangerous way round.
///
/// `reason` is [`vike_model::accounts::account_keys::AccountKeyError`]'s own rendering, so the legal shape a
/// refusal describes and the shape the parser accepts cannot drift.
fn bad_account_label(
    file: &Path,
    venue: &str,
    label: &str,
    mode: VenueMode,
    reason: &str,
) -> ConfigError {
    ConfigError::Value {
        file: file.to_path_buf(),
        key: format!("accounts.{venue}.{label}"),
        message: format!(
            "\"{mode}\" is stated for an account label vike cannot address: {reason}. An account \
             label is the suffix of that account's credential keys, after the double underscore. \
             (The legal modes are {}.)",
            legal_modes(),
        ),
    }
}

/// The refusal for a `[venues]` key naming no venue — by NAME, with the roster in the message.
///
/// Refused rather than ignored, for the reason every tombstone above is refused: an operator who
/// wrote `venues.bybitt = "paper"` believes they capped bybit, and a load that accepted the line
/// would hand them positive confirmation of a ceiling they do not have. That is worse here than for
/// a tombstone, because the belief runs the DANGEROUS way round — they think a venue is held to
/// paper and it is not.
///
/// The roster is rendered from [`vike_model::VENUES`] and the modes from
/// [`crate::venue_mode::legal_modes`], so neither list can drift from what the loader accepts.
fn unknown_venue(file: &Path, name: &str, mode: VenueMode) -> ConfigError {
    let hint = match did_you_mean(name) {
        // Not repaired, only suggested — see `did_you_mean`'s doc. The lowercase remark is added
        // ONLY when case is the whole difference: appended to a plain typo it reads as an
        // explanation of that typo and sends the operator looking at the wrong thing.
        Some(id) if name.to_ascii_lowercase() == id => {
            format!(" Did you mean `{id}`? Venue ids are lowercase.")
        }
        Some(id) => format!(" Did you mean `{id}`?"),
        None => String::new(),
    };
    ConfigError::Value {
        file: file.to_path_buf(),
        key: format!("venues.{name}"),
        message: format!(
            "\"{mode}\" names no venue vike has a bridge for, so this ceiling would apply to \
             NOTHING while reading like it applies to something.{hint} The venues are: {}. \
             (The legal modes are {}.)",
            VENUES.join(", "),
            legal_modes(),
        ),
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

/// The refusal for an `[account_exposure]` line on a labelled account that `[accounts]` never
/// armed.
///
/// ⚠ It names the FIX rather than only the fault, because the operator's intent is obvious and the
/// missing line is one word: they wanted this account capped, and an account with no mode line does
/// not trade at all.
fn unarmed_exposure_account(file: &Path, venue: &str, label: &str, cap: f64) -> ConfigError {
    ConfigError::Value {
        file: file.to_path_buf(),
        key: format!("account_exposure.{venue}.{label}"),
        message: format!(
            "caps `{label}` at {cap}, but no `policy.accounts.{venue}.{label}` row arms it — a \
             labelled account with no mode row resolves to `paper`, so this ceiling would cap an \
             account that trades nothing, and the store has no row to carry it on. Arm it first:\n\n\
             \x20\x20\x20\x20vike-cli config set policy.accounts.{venue}.{label} demo   # or \
             live\n\nThe unlabelled account needs no such row — its mode is the venue's own."
        ),
    }
}
