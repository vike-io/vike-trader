//! `EngineError` — the error every `make_engine*` entry point returns — and its `Display`/`Error` impls,
//! plus [`ArmingNotRunning`], the account that does not run and why. The crate root re-exports both, so
//! callers name `vike_tradehub_core::EngineError` and `vike_tradehub_core::ArmingNotRunning`.

use vike_model::accounts::account_keys::AccountLabel;

#[cfg(doc)]
use crate::{
    arm_universal_defaults, make_engine, make_engine_accounts, require_live_risk_budget,
    venue_would_go_live,
};

/// **An account this process knows and does NOT run** — and why (decision 0156).
///
/// An account runs at the tier its active row states or not at all: every outcome but a `paper`
/// row on the paper simulator and a `demo`/`live` row armed at exactly that tier is one of these,
/// with its [`vike_model::ArmingBlock`] as the reason — never a paper engine in its place.
/// [`make_engine_accounts`] lists them beside the engines it built; `crate::assemble_runtime` hands them
/// to the core (its refusal of an order addressed to one names the reason) and returns them to the
/// composition root, which publishes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmingNotRunning {
    /// The venue id.
    pub venue: String,
    /// Which account of it.
    pub account: AccountLabel,
    /// The tier its active row states; `None` when no active row states exactly one.
    pub tier: Option<vike_model::AccountTier>,
    /// Why it does not run — never a block under which an account runs
    /// ([`vike_model::ArmingBlock::runs`]).
    pub block: vike_model::ArmingBlock,
}

impl ArmingNotRunning {
    /// The account's ROUTING key — the one an order or a mount addressed to it names
    /// (`vike_model::accounts::account_keys::route_key_of`, the bare venue for the default account).
    #[must_use]
    pub fn route_key(&self) -> String {
        vike_model::accounts::account_keys::route_key_of(&self.venue, &self.account)
    }

    /// The core's and the wire's view of it: the account, the tier, the block and its badge
    /// ([`vike_model::ArmingBlock::as_str`]) as the reason a person reads. An account held by the FX
    /// market's close reads when it logs in instead ([`Self::view_at`], on the wall clock now).
    #[must_use]
    pub fn view(&self) -> vike_execution::ArmingNotRunningView {
        self.view_at(vike_model::now_ms())
    }

    /// [`Self::view`] at `now_ms`: for [`vike_model::ArmingBlock::FxMarketClosed`] the reason is
    /// `vike_venues::session::fx_login_closed_words` of the next open, so the door's refusal of its
    /// orders and the published list both say when the account logs in; every other block reads its badge.
    #[must_use]
    pub fn view_at(&self, now_ms: i64) -> vike_execution::ArmingNotRunningView {
        let badge = self.block.as_str();
        let reason = match self.block {
            vike_model::ArmingBlock::FxMarketClosed => {
                vike_venues::session::fx_login_closed_until(&self.venue, now_ms)
                    .map_or_else(|| badge.to_string(), vike_venues::session::fx_login_closed_words)
            }
            _ => badge.to_string(),
        };
        vike_execution::ArmingNotRunningView {
            account: vike_model::accounts::account_keys::AccountNamed::of(
                &self.venue,
                self.account.clone(),
            ),
            tier: self.tier,
            reason,
            block: Some(self.block),
        }
    }
}

impl std::fmt::Display for ArmingNotRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} does not run: {}", self.route_key(), self.block.as_str())
    }
}

/// [`make_engine`]'s two failure modes. ONE refuses the node: a LIVE mount refuses to start
/// because the operator supplied no account-dependent risk budget (rationale:
/// [`require_live_risk_budget`]; contrast the universal defaults [`arm_universal_defaults`] arms
/// unconditionally) — never raised for a paper/backtest mount, only pre-connect when
/// [`venue_would_go_live`] says the venue's live config is present (the primary site), or at the
/// post-merge backstop for a live arm the probe misses. The OTHER is an account that does not run
/// ([`ArmingNotRunning`]): no engine for it, and [`make_engine_accounts`] lists it rather than failing.
#[derive(Debug)]
pub enum EngineError {
    /// `missing` lists EVERY missing `risk.*` key, so one fix cycle closes the gate.
    /// `profile_supplied`: a `[risk]` table reached this mount at all (`AdapterEnv::risk_profile`
    /// was `Some`). The fixes differ: no profile ⇒ "create one and point at it";
    /// a profile omitting the caps ⇒ "add these lines to it".
    MissingRiskBudget { venue: String, missing: Vec<&'static str>, profile_supplied: bool },
    /// The account does not run (decision 0156): no engine is built for it, and no paper engine in
    /// its place.
    NotRunning(ArmingNotRunning),
    /// The active run profile's `[risk]` sets venue-owned instrument fields (`risk.tick_size`,
    /// `risk.lot_size`, `risk.min_qty`, `risk.min_notional`) and the account mounts at `demo` or
    /// `live`, where the VENUE owns that grid (decision 0156 item 7: the refusal a `mode = "live"`
    /// profile used to meet at load is the mount's now). The mount is refused before any credential
    /// is read; a `paper` account may use the fields. `fields` names every offending key.
    VenueOwnedRiskFields { venue: String, tier: vike_model::AccountTier, fields: Vec<&'static str> },
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
/// `vike_tradehub_core::assemble_runtime` (and the `build_live_*_core` builders over it), and
/// `vike_tradehub_core::RuntimeError::RiskBudget` forwards `Display` verbatim, so all get the same
/// instructions from one definition.
impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::NotRunning(not_running) => write!(f, "{not_running}"),
            EngineError::VenueOwnedRiskFields { venue, tier, fields } => write!(
                f,
                "{tier} start of venue '{venue}' refused: the active run profile's [risk] sets \n                 venue-owned instrument field(s) [{}], and at tier `{tier}` the venue owns \n                 tick_size/lot_size/min_qty/min_notional. Remove {} from the run profile (re-write it \n                 with `{WRITER} <its name> …` without them: `vike-cli config show` lists what it \n                 carries); only a `paper` account may take them. Nothing was started.",
                fields.join(", "),
                if fields.len() == 1 { "it" } else { "them" }
            ),
            EngineError::MissingRiskBudget { venue, missing, profile_supplied } => {
                writeln!(f, "live start of venue '{venue}' refused: no risk budget.")?;
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
                        "Re-write the active run profile with `{WRITER} <its name> …` — every key \
                         it carries now (`vike-cli config show` lists them), plus:"
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
                    writeln!(f, "    {WRITER} run-live \\")?;
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
                write!(f, "Every run-profile key is listed by `{WRITER} --help`.")
            }
        }
    }
}

impl std::error::Error for EngineError {}
