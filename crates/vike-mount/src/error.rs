//! `MountError` — the error every `make_engine*` entry point returns — and its `Display`/`Error` impls.
//! The crate root re-exports it, so callers name `vike_mount::MountError`.

#[cfg(doc)]
use crate::{arm_universal_defaults, make_engine, require_live_risk_budget, would_mount_live};

/// [`make_engine`]'s one failure mode: a LIVE mount refuses to start because the operator supplied
/// no account-dependent risk budget (rationale: [`require_live_risk_budget`]; contrast the
/// universal defaults [`arm_universal_defaults`] arms unconditionally). Never raised for a
/// paper/backtest mount: only pre-connect when [`would_mount_live`] says the venue's live config
/// is present (the primary site), or at the post-merge backstop for a live arm the probe misses.
#[derive(Debug)]
pub enum MountError {
    /// `missing` lists EVERY missing `risk.*` key, so one fix cycle closes the gate.
    /// `profile_supplied`: a `[risk]` table reached this mount at all (`MountEnv::risk_profile`
    /// was `Some`). The fixes differ: no profile ⇒ "create one and point at it";
    /// a profile omitting the caps ⇒ "add these lines to it".
    MissingRiskBudget { venue: String, missing: Vec<&'static str>, profile_supplied: bool },
}

/// Render `keys` as the aligned body of a `[risk]` TOML fragment (the caller writes the header),
/// padded to the widest key/value actually rendered.
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

/// The operator-facing diagnostic: the FIRST wall a new user of a live-mounting binary hits, so it
/// names the knob (`VIKE_RUN_PROFILE`), the requirement and a paste-ready fix. It lives on the error
/// type, not at a call site: every binary reaches it through `vike_mount::build_node` (and
/// `vike_mount::build_live_maker_core`), and `vike_mount::NodeError::RiskBudget` forwards `Display`
/// verbatim, so all get the same instructions from one definition.
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
                    // `VIKE_RUN_PROFILE` is honoured by EVERY binary (`vike_core::resolve_profile`'s
                    // fallback); `--profile` is not accepted by all — hence "a binary that accepts one".
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
                    // ⚠ Never print `[event_source]`/`[broker]` here: `RunProfile` refuses a profile
                    // carrying either by name, so the paste-ready file would fail startup. A `mode`
                    // + `[risk]` file is the complete minimal profile.
                    writeln!(f, "    [risk]")?;
                    // Every cap, not just `missing`: with no profile the printed file must be
                    // complete, or pasting it earns a second refusal.
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
