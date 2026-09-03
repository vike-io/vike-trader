//! `MountError`'s `Display`/`Error` impls — split out of `lib.rs` for size; `MountError` itself
//! stays defined at the crate root because `make_engine`'s whole family returns it.

use crate::MountError;

/// Render `keys` as an aligned `[risk]` TOML fragment (the `    key = value  # meaning` body only —
/// the caller writes the `[risk]` header), padded to the widest key/value actually rendered so a
/// one-key fragment does not carry a two-key fragment's whitespace.
fn risk_table_body(f: &mut std::fmt::Formatter<'_>, keys: &[&str]) -> std::fmt::Result {
    let rows: Vec<&(&str, &str, &str)> =
        crate::BUDGET_EXAMPLES.iter().filter(|(k, _, _)| keys.contains(k)).collect();
    let key_w = rows.iter().map(|(k, _, _)| k.len()).max().unwrap_or(0);
    let val_w = rows.iter().map(|(_, v, _)| v.len()).max().unwrap_or(0);
    for (key, value, meaning) in rows {
        writeln!(f, "    {key:key_w$} = {value:val_w$}   # {meaning}")?;
    }
    Ok(())
}

/// The operator-facing diagnostic. This is the FIRST thing a new user of any binary that mounts a
/// live node hits, and it used to surface as a `Debug`-formatted `.expect()` panic naming neither
/// the knob (`VIKE_RUN_PROFILE`), the requirement (a run profile), nor the fix (a `[risk]` table) —
/// the only working example in the tree lived inside a `#[cfg(test)]` fixture. It lives HERE, on the
/// error type, rather than at any one binary's call site, because three binaries reach this wall
/// through the same `vike_run::build_node` edge (`vike-app`, `vike-tradehub`'s
/// `VIKE_TRADEHUB_LIVE=1` arm, and any future `vike_run::build_live_maker_core` caller) and
/// `vike_run::NodeError::RiskBudget` already forwards `Display` verbatim — so every one of them
/// gets the same instructions from one definition.
impl std::fmt::Display for MountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MountError::MissingRiskBudget { venue, missing, profile_supplied } => {
                writeln!(f, "live mount for venue '{venue}' refuses to start: no risk budget.")?;
                if *profile_supplied {
                    writeln!(
                        f,
                        "  cause:   the run profile in use leaves the account-dependent caps unset"
                    )?;
                } else {
                    // `VIKE_RUN_PROFILE` is the resolver EVERY binary honours (`vike_core::
                    // resolve_profile` falls back to it when no explicit path is passed); the
                    // `--profile` flag is the explicit-wins override, which vike-tradehub accepts
                    // and vike-app does not — hence "a binary that accepts one", not a flat claim.
                    writeln!(f, "  cause:   no run profile was found — VIKE_RUN_PROFILE is unset")?;
                }
                writeln!(f, "  missing: [risk] {}", missing.join(", "))?;
                writeln!(
                    f,
                    "  why:     these caps depend on your account size, so there is no safe \
                     default to guess"
                )?;
                writeln!(f)?;
                if *profile_supplied {
                    writeln!(
                        f,
                        "Add to that profile's [risk] table (the file VIKE_RUN_PROFILE / --profile \
                         points at):"
                    )?;
                    writeln!(f)?;
                    writeln!(f, "    [risk]")?;
                    risk_table_body(f, missing)?;
                } else {
                    writeln!(
                        f,
                        "Set VIKE_RUN_PROFILE=<run.toml>, or pass --profile <run.toml> to a binary \
                         that accepts one"
                    )?;
                    writeln!(
                        f,
                        "(an explicit flag wins over the env var). A minimal live profile:"
                    )?;
                    writeln!(f)?;
                    writeln!(f, "    mode = \"live\"")?;
                    writeln!(f)?;
                    // Named rather than left to surprise: `[event_source]`/`[broker]` are NOT
                    // `#[serde(default)]` on `RunProfile`, so a `[risk]`-only file is a parse
                    // error — but a multi-venue live mount ignores both (it mounts its own market
                    // table). Saying so is what stops the example reading like busywork.
                    writeln!(
                        f,
                        "    # [event_source]/[broker] are schema-required; a live venue mount \
                         reads only mode + [risk]"
                    )?;
                    writeln!(f, "    [event_source]")?;
                    writeln!(f, "    kind   = \"live_venue\"")?;
                    writeln!(f, "    venue  = \"{venue}\"")?;
                    writeln!(f, "    symbol = \"BTCUSDT\"")?;
                    writeln!(f)?;
                    writeln!(f, "    [broker]")?;
                    writeln!(f, "    kind  = \"venue\"")?;
                    writeln!(f, "    venue = \"{venue}\"")?;
                    writeln!(f)?;
                    writeln!(f, "    [risk]")?;
                    // Every cap, not just `missing`: with NO profile at all the file above must be
                    // a COMPLETE working profile, or copy-pasting it earns a second refusal.
                    let all: Vec<&str> =
                        crate::BUDGET_EXAMPLES.iter().map(|(k, _, _)| *k).collect();
                    risk_table_body(f, &all)?;
                }
                writeln!(f)?;
                write!(f, "Commented template to copy: {}", crate::EXAMPLE_PROFILE_PATH)
            }
        }
    }
}

impl std::error::Error for MountError {}
