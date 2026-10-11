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

/// The command that writes the run profile's rows (decision 0111: the run profile is the ACTIVE
/// `run` row of the settings database, and no binary reads a profile file).
const WRITER: &str = "vike-cli config bootstrap-run";

/// Render `keys` as aligned `--risk.<key> <value>` arguments of [`WRITER`], one per line, padded to
/// the widest key/value actually rendered, each with its meaning.
fn risk_flag_lines(f: &mut std::fmt::Formatter<'_>, keys: &[&str]) -> std::fmt::Result {
    let rows: Vec<&(&str, &str, &str)> =
        crate::BUDGET_EXAMPLES.iter().filter(|(k, _, _)| keys.contains(k)).collect();
    let key_w = rows.iter().map(|(k, _, _)| k.len()).max().unwrap_or(0);
    let val_w = rows.iter().map(|(_, v, _)| v.len()).max().unwrap_or(0);
    for (key, value, meaning) in rows {
        writeln!(f, "    --risk.{key:key_w$} {value:val_w$}   # {meaning}")?;
    }
    Ok(())
}

/// The operator-facing diagnostic: the FIRST wall a new user of a live-mounting binary hits, so it
/// names where the budget lives (the ACTIVE `run` row), the requirement and a paste-ready fix. It
/// lives on the error type, not at a call site: every binary reaches it through
/// `vike_mount::build_node` (and `vike_mount::build_live_maker_core`), and
/// `vike_mount::NodeError::RiskBudget` forwards `Display` verbatim, so all get the same
/// instructions from one definition.
impl std::fmt::Display for MountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MountError::MissingRiskBudget { venue, missing, profile_supplied } => {
                writeln!(f, "live mount for venue '{venue}' refuses to start: no risk budget.")?;
                if *profile_supplied {
                    writeln!(
                        f,
                        "  cause:   the active run profile leaves the account-dependent caps unset"
                    )?;
                } else {
                    writeln!(
                        f,
                        "  cause:   no run profile is in force — the settings database has no \
                         ACTIVE `run` row"
                    )?;
                }
                writeln!(f, "  missing: [risk] {}", missing.join(", "))?;
                writeln!(
                    f,
                    "  why:     these caps depend on your account size, so there is no safe \
                     default to guess"
                )?;
                writeln!(f)?;
                if *profile_supplied {
                    // `bootstrap-run` REPLACES the body stored under a name, so the fix is the
                    // active row's whole body again plus these keys — `vike-cli config show`
                    // prints what it carries now.
                    writeln!(
                        f,
                        "Re-write the active run profile with `{WRITER} <its name> --mode live …` \
                         — every key it carries now (`vike-cli config show` lists them), plus:"
                    )?;
                    writeln!(f)?;
                    risk_flag_lines(f, missing)?;
                } else {
                    writeln!(
                        f,
                        "Write one — `{WRITER}` stores it and makes it the ACTIVE `run` row in \
                         one step:"
                    )?;
                    writeln!(f)?;
                    writeln!(f, "    {WRITER} run-live --mode live \\")?;
                    // Every cap, not just `missing`: with no profile the printed command must be
                    // complete, or running it earns a second refusal.
                    let all: Vec<&str> =
                        crate::BUDGET_EXAMPLES.iter().map(|(k, _, _)| *k).collect();
                    let flags: Vec<String> = crate::BUDGET_EXAMPLES
                        .iter()
                        .map(|(k, v, _)| format!("--risk.{k} {v}"))
                        .collect();
                    writeln!(f, "        {}", flags.join(" "))?;
                    writeln!(f)?;
                    writeln!(f, "  where")?;
                    risk_flag_lines(f, &all)?;
                }
                writeln!(f)?;
                writeln!(f, "then restart the daemon: it reads the run profile once, at boot.")?;
                write!(f, "Every run-profile key, commented: {}", crate::EXAMPLE_PROFILE_PATH)
            }
        }
    }
}

impl std::error::Error for MountError {}
