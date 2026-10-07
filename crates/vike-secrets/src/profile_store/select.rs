//! Profile selection (the ruling) and the fence on writing an active row.

// ---------------------------------------------------------------------------------------------
// THE RULING: selection
// ---------------------------------------------------------------------------------------------

/// Where a resolved profile selection CAME FROM.
///
/// It is reported rather than merely used, and that is §7 of the schema spec (`shadowed` — *"a file
/// could never report this, because a file cannot know it lost"*). With the row winning, an operator
/// whose `ExecStart --config` or whose `.env` `VIKE_RUN_PROFILE` has stopped deciding anything must
/// be told so at startup, or the ruling delivers the exact defect it was ruling against: positive
/// confirmation of something false.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionSource {
    /// A `profile` row with `active = 1`. **The winner, by the owner's ruling.**
    StoreRow,
    /// An explicit path the caller was given — `--config <path>` (which
    /// `crates/vike-tradehub/src/tradehub_cli/args.rs`'s `parse_args_from` makes REQUIRED) or
    /// `--profile <path>`.
    ExplicitArg,
    /// An environment variable — `VIKE_RUN_PROFILE`, which on the shipped deployment arrives from
    /// `EnvironmentFile=-<root>/.env`, an untracked file at the project root.
    EnvVar,
}

impl SelectionSource {
    /// The word a startup line prints.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            SelectionSource::StoreRow => "store row",
            SelectionSource::ExplicitArg => "explicit argument",
            SelectionSource::EnvVar => "environment variable",
        }
    }
}

/// One rung that held a value and LOST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shadowed {
    /// Which rung.
    pub source: SelectionSource,
    /// What it held. A profile NAME or a path — never a secret.
    pub value: String,
}

/// The outcome of [`select`]: who won, with what, and who lost while still holding a value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selected {
    /// The winning value, or `None` when no rung held one.
    pub value: Option<String>,
    /// Which rung won. `None` exactly when [`Self::value`] is `None`.
    pub source: Option<SelectionSource>,
    /// Every rung that held a value and did not win, highest first.
    pub shadowed: Vec<Shadowed>,
}

impl Selected {
    /// Did a store row beat something that is still set? The one condition a startup line must
    /// WARN about rather than merely record.
    #[must_use]
    pub fn row_shadowed_something(&self) -> bool {
        self.source == Some(SelectionSource::StoreRow) && !self.shadowed.is_empty()
    }
}

/// **THE OWNER'S RULING, WRITTEN ONCE: the row wins.**
///
/// Precedence, highest first: an active `profile` row, then an explicit argument, then the
/// environment. Every rung that held a value and lost is reported in [`Selected::shadowed`], so a
/// caller can say what stopped mattering.
///
/// # ⚠ Why the ruling is safe to implement as stated, and where the safety actually lives
///
/// 0057 Question 3 warns that *"if the row wins, a migration that writes one ARMS a mount that a
/// missing line was holding on paper"*. That hazard is real and it is **not in this function**: this
/// function only decides which of the values it was handed wins. The hazard is in WRITING a row, and
/// [`plan_active_row`] is the only thing in this tree that may.
///
/// A blank or whitespace-only value is treated as ABSENT at every rung — the same reading
/// `vike_model::paths::state_path`'s `VIKE_SETTINGS_DIR` handling gives an empty override, and the reason
/// is the same: `Environment=VIKE_RUN_PROFILE=` in a unit file is somebody unsetting it, not
/// somebody naming a profile called "".
#[must_use]
pub fn select(row: Option<&str>, explicit: Option<&str>, env: Option<&str>) -> Selected {
    let rungs: [(SelectionSource, Option<&str>); 3] = [
        (SelectionSource::StoreRow, row),
        (SelectionSource::ExplicitArg, explicit),
        (SelectionSource::EnvVar, env),
    ];
    let mut out = Selected::default();
    for (source, held) in rungs {
        let Some(v) = held.map(str::trim).filter(|v| !v.is_empty()) else { continue };
        if out.value.is_none() {
            out.value = Some(v.to_string());
            out.source = Some(source);
        } else {
            out.shadowed.push(Shadowed { source, value: v.to_string() });
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// THE FENCE: may a migration write an active row?
// ---------------------------------------------------------------------------------------------

/// What a migration proposes to do about ONE kind's active row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivePlan {
    /// Write `active = 1` on this profile. Only ever produced when doing so **reproduces the
    /// selection that is in force today**, so the resolved outcome is unchanged by construction.
    Write {
        /// The profile that is already in force, and which the row will name.
        name: String,
    },
    /// Write NO active row, and say why. The migration still lands the BODIES — which is 0057's own
    /// *"the store holds profile BODIES"* — and selection stays exactly where it is.
    Withhold {
        /// The operator-facing reason, which names what would have changed.
        reason: WithholdReason,
    },
}

/// Why [`plan_active_row`] refused to write an active row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WithholdReason {
    /// **The dangerous one.** Nothing selects a profile of this kind today. Writing a row would
    /// hand the resolver a value where it has none, which is the arming-by-migration 0057
    /// Question 3 is about. For a RUN profile this is not even a paper/live difference: with no run
    /// profile a live mount REFUSES TO START (`vike_mount::MountError::MissingRiskBudget` — see
    /// `vike_config::ceilings`'s `absent_means` for `max_notional_per_order`), so the row would turn
    /// a daemon that exits FAILURE into a daemon that trades.
    NothingSelectedToday,
    /// Something IS selected today, but the profile being migrated is not it. Writing the row would
    /// silently repoint the daemon at a different body.
    WouldRepoint {
        /// What is in force today.
        in_force: String,
        /// What the row would have named.
        proposed: String,
    },
    /// The store already holds an active row for this kind, and it names something else. This is
    /// not a migration's decision to overturn.
    AlreadyActive {
        /// The name the store already carries.
        held_by: String,
    },
}

impl std::fmt::Display for WithholdReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WithholdReason::NothingSelectedToday => write!(
                f,
                "no active row was written: nothing selects a profile of this kind on this box \
                 today, and a row that selected one would be this migration ARMING a mount that an \
                 absent selection was holding. The profile BODY was stored; activate it \
                 deliberately if that is what you want"
            ),
            WithholdReason::WouldRepoint { in_force, proposed } => write!(
                f,
                "no active row was written: `{in_force}` is what this box runs today and the row \
                 would have named `{proposed}`, which is a change of what the daemon trades rather \
                 than a change of where it is stored"
            ),
            WithholdReason::AlreadyActive { held_by } => write!(
                f,
                "no active row was written: the store already selects `{held_by}` for this kind, \
                 and a migration does not overturn a selection an operator made"
            ),
        }
    }
}

/// **THE ONE PLACE A MIGRATION MAY DECIDE TO ARM SOMETHING — and it says no unless it can prove it
/// is not arming anything.**
///
/// The rule, in one sentence: *write an active row only when the row names exactly what is already
/// in force, so the resolved selection before and after the migration is the same string.*
///
/// * `in_force` — what selects this kind TODAY, with the store's own rung removed: the `--config`
///   argument for a daemon profile, `--profile`/`VIKE_RUN_PROFILE` for a run profile, reduced to the
///   profile NAME the body was stored under. `None` means nothing selects one.
/// * `already_active` — what the store's `active` row holds for this kind before the migration.
/// * `proposed` — the name the migration is storing the body under.
///
/// # ⚠ Read the `None` arm twice
///
/// `in_force: None` is the state that LOOKS safest and is the one the record singles out. On a box
/// where `flags.tradehub_live` is on and no run profile is selected, today's outcome is a daemon
/// that refuses to start. Writing a row there does not "restore" anything: it starts a live mount
/// that has never run.
#[must_use]
pub fn plan_active_row(
    in_force: Option<&str>,
    already_active: Option<&str>,
    proposed: &str,
) -> ActivePlan {
    if let Some(held) = already_active {
        if held == proposed {
            return ActivePlan::Write { name: proposed.to_string() };
        }
        return ActivePlan::Withhold {
            reason: WithholdReason::AlreadyActive { held_by: held.to_string() },
        };
    }
    match in_force {
        None => ActivePlan::Withhold { reason: WithholdReason::NothingSelectedToday },
        Some(n) if n == proposed => ActivePlan::Write { name: proposed.to_string() },
        Some(n) => ActivePlan::Withhold {
            reason: WithholdReason::WouldRepoint {
                in_force: n.to_string(),
                proposed: proposed.to_string(),
            },
        },
    }
}
