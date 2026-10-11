//! **The ONE journalled settings write in this binary** — `vike_config::write_setting_row` plus the
//! `vike_model::change_journal` record, shared by every verb that writes a settings row
//! (`docs/decisions/0086`).
//!
//! # Why this is a module and not a function on one command
//!
//! It began as `crate::cmd::node`'s private `set_setting_journalled`, typed on that command's own
//! `Ctx` (which carries a keyring and a dial address a settings write uses for nothing) and with
//! that command's name baked into two operator-facing strings. [`crate::cmd::config::set`] needed
//! the same decisions — which outcome to record, which cells to fill, what a journal failure may
//! and may not do to the call, which exit rung each refusal takes — and this repository's standing
//! rule is that the second copy of a rule rots. So the BODY moved here and `crate::cmd::node`'s
//! `set_setting_journalled` became an adapter that supplies its own surface name and prints its own
//! confirmation line.
//!
//! # The four decisions this module owns
//!
//! 1. **`Outcome::AppliedPendingRestart`, always.** A write performed by a LOCAL process reaches no
//!    applier: the only hot-apply path in the tree is the daemon's own WIRE arm
//!    (`crates/vike-tradehub/src/server/settings.rs`'s `apply_set_setting`), and nothing in a CLI process
//!    holds one. So "applied, pending restart" is not a simplification here; it is the only outcome
//!    a local write can honestly claim.
//! 2. **BOTH outcomes are recorded.** A failure appends its own record carrying the writer's own
//!    reason. What is NOT recorded is a refusal by the CALLING VERB — a secret-shaped key, no
//!    resolved settings directory — because nothing was attempted against the store: this ledger's
//!    `set_setting` record carries a section cell, an old value and a new one, and a command line
//!    that never reached the writer has none of the three.
//!
//!    ⚠ **Every row-native refusal is `Outcome::Refused`, with no second shape.** The file-era
//!    writer this module replaced could leave the previous file MOVED ASIDE with the target absent
//!    (`Outcome::Stranded`) — a state a plain "refused, nothing written" row would misdescribe.
//!    `vike_secrets::write_setting_row_in` runs the whole write inside one `BEGIN IMMEDIATE` and
//!    rolls back on any refusal, so the database is BYTE-IDENTICAL on every failure path — there is
//!    no larger-than-refused state left for a ledger row to under-report.
//! 3. **A journal failure never fails the call.** The row IS in the database; sending a caller down
//!    an error path for a write that succeeded is worse than a missing ledger line. Same
//!    disposition as `crate::cmd::node`'s `record_credential_write` and
//!    `vike_ctrader::token_store`'s `record_rotation`.
//! 4. **A credential-shaped KEY is refused HERE, at the shared writer** ([`refuse_a_secret_key`]),
//!    not at whichever verb happens to be calling. It began at the leaf caller, which was safe
//!    only for as long as the caller set stayed two — `crate::cmd::node`'s three call sites write
//!    fixed literals and `crate::cmd::config::set` carried its own gate — and that is the shape
//!    `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`'s first
//!    reason names: every writer is a surface the rule has to be re-proven on.
//!
//!    ⚠ **The rule itself now lives one crate down, in the planner every surface calls**
//!    (`crates/vike-config/src/write.rs`'s `refuse_credential_key`, which `write_setting_row` runs
//!    before it opens the database). It moved there because the daemon's control channel became a
//!    settings writer (decision 0086 point 6) and refused nothing of the kind. This module still
//!    calls it EARLY, for the ledger reason in decision 2, and keeps no predicate or wording of its
//!    own. The predicate's over-matching residual is argued at that function.
//!
//! ⚠ **No `--file <path>` may ever reach this module**, and that rule is inherited rather than
//! invented: `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`
//! lists "a write aimed at an operator-supplied PATH" among the things that re-decide it from the
//! top. Its SUBJECT is the credential store, which no settings write touches — but the reason
//! transfers whole, and the destination here is the dispatcher-resolved settings directory,
//! relocatable only by the one variable that moves the store, the ledger and the policy together.

use std::path::Path;

use vike_config::{RowPlanError, write::RowReport};
use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome, Proc};

use crate::exit::{CliError, CmdResult};

/// **The budget this surface gives the settings database's write lock: the whole-process one, ~3
/// s.**
///
/// Sized against the critical section rather than against patience: one `BEGIN IMMEDIATE`, a
/// handful of row reads, a candidate resolve through the loader and one UPSERT — tens of
/// milliseconds even on a loaded box — so three seconds is roughly two orders of magnitude of
/// headroom for an overlap anybody was actually in, and short enough that an operator at a prompt
/// never has to wonder whether the command is hung. `crates/vike-app-core/src/ui/tool_views/
/// venues.rs`'s `VENUE_ARM_LOCK_BUDGET` and `crates/vike-tradehub/src/server/settings.rs`'s
/// `SETTINGS_LOCK_BUDGET` argue their own, different, numbers at their own call sites — a CLI is
/// the whole process, an egui frame owes 16 ms and a daemon connection thread has a reply deadline
/// above it.
pub(crate) const CLI_LOCK_BUDGET: std::time::Duration = std::time::Duration::from_millis(3_000);

/// Everything a journalled settings write needs from OUTSIDE the `src/cmd/` file performing it.
///
/// A struct rather than four parameters, for the reason `crate::cmd::secrets`' `Ctx` states: every
/// field is a fact only `crate::run` can know, and the rule this crate is held to is that a
/// `src/cmd/` file reads no environment and performs no second walk of its own.
#[derive(Clone, Copy)]
pub(crate) struct Ctx<'a> {
    /// The settings directory, as the boot resolved it. `None` refuses the write — a settings write
    /// with no resolved directory would have to GUESS a destination.
    pub(crate) settings_dir: Option<&'a Path>,
    /// That directory's `state` child, off the SAME walk — the change journal's home.
    ///
    /// `None` (no project above the working directory) journals NOTHING and still writes, rather
    /// than opening an append-only ledger in a guessed directory: `vike_boot::journal_boot_settings`
    /// takes the same disposition.
    pub(crate) state_dir: Option<&'a Path>,
    /// The instant a record is stamped with. `vike_model::change_journal` reads no clock, so the
    /// instant is a parameter all the way down.
    pub(crate) now_ms: i64,
    /// What to call this surface in a stderr line about the LEDGER. It prefixes nothing else: the
    /// confirmation line is the caller's to print, because only the caller knows what shape its
    /// output has.
    pub(crate) surface: &'static str,
}

/// Which rung a refusal exits on — the classification `crate::exit`'s ladder exists for.
///
/// * [`RowPlanError::BadKey`], [`RowPlanError::CredentialKey`] and a validator
///   [`vike_secrets::RowWriteError::Rejected`] refusal are `Usage`: the command line named a key no
///   section can spell, a credential-shaped key, a value with no JSON scalar form, or a write this
///   store, as it stands, will never accept unchanged. Nothing was attempted, and re-running it
///   unchanged cannot succeed — which is `crate::exit::Exit::Usage`'s stated meaning.
/// * [`vike_secrets::RowWriteError::Busy`], [`vike_secrets::RowWriteError::NoDatabase`] and
///   [`vike_secrets::RowWriteError::Sql`] are the ordinary run failure: the command line was fine,
///   the BOX is not (another writer holding the database, no database provisioned yet, a
///   permission or disk failure the engine hit). A caller FIXES a `1` and investigates — `Busy` and
///   `NoDatabase` both promise that re-running this exact command UNCHANGED can succeed once the
///   box catches up (the other writer releases, or `vike-cli secrets init` runs), which is the
///   half of `Usage`'s promise they would otherwise break.
fn rung(e: &RowPlanError) -> CliError {
    use vike_secrets::RowWriteError;
    let msg = e.to_string();
    match e {
        RowPlanError::BadKey { .. } | RowPlanError::CredentialKey { .. } => {
            CliError::usage(format!("nothing was written — {msg}"))
        }
        RowPlanError::Refused(RowWriteError::Rejected(_)) => {
            CliError::usage(format!("nothing was written — {msg}"))
        }
        RowPlanError::Refused(
            RowWriteError::Busy | RowWriteError::NoDatabase { .. } | RowWriteError::Sql(_),
        ) => CliError::failed(msg),
    }
}

/// **A credential-shaped key is refused here, before a settings directory is resolved and before a
/// ledger row could be written**, whatever the calling verb — this module's own doc, decision 4.
///
/// It holds no predicate and no wording of its own: both are `vike_config::refuse_credential_key`,
/// the refusal `vike_config::write_setting_row` itself runs for every surface (the daemon's control
/// channel included), so this binary and the daemon cannot disagree about which keys are refused or
/// what the operator is told. What it adds is the exit RUNG ([`rung`]: `Usage`, since re-running
/// the same command line cannot succeed).
pub(crate) fn refuse_a_secret_key(key: &str) -> CmdResult<()> {
    vike_config::refuse_credential_key(key).map_err(|e| rung(&e))
}

/// Set ONE settings key through `vike_config::write_setting_row` and journal the outcome, both
/// ways.
///
/// Returns the [`RowReport`] so the CALLER renders its own confirmation — this module prints
/// nothing on the success path, and exactly one line on the ledger-failure path.
pub(crate) fn set_setting_journalled(
    ctx: &Ctx<'_>,
    key: &str,
    value: &str,
) -> CmdResult<RowReport> {
    set_setting_journalled_within(ctx, key, value, CLI_LOCK_BUDGET)
}

/// [`set_setting_journalled`] with the database's wait budget as a parameter — the ONE line that
/// binds this binary to [`CLI_LOCK_BUDGET`], and the seam a contended test drives without spending
/// the shipped budget to prove a behaviour that is not about the number.
pub(crate) fn set_setting_journalled_within(
    ctx: &Ctx<'_>,
    key: &str,
    value: &str,
    budget: std::time::Duration,
) -> CmdResult<RowReport> {
    refuse_a_secret_key(key)?;
    let Some(dir) = ctx.settings_dir else {
        // ⚠ `vike-cli config show`, NOT `secrets path`: this refusal is about the SETTINGS
        // directory, and `secrets path` answers about the credential store — a different resolver
        // with a different fallback rung. `config show`'s header prints the settings directory and
        // says NONE when there is not one, which is the question being asked here.
        return Err(CliError::failed(format!(
            "no settings directory resolved, so {key} cannot be written — `cd` into the project, \
             or name one with $VIKE_SETTINGS_DIR. `vike-cli config show` prints what that \
             resolved to (and says NONE when nothing did)."
        )));
    };
    let journal = ctx
        .state_dir
        .map(|d| ChangeJournal::in_state_dir(d, Proc::current(env!("CARGO_PKG_VERSION"))));
    // ⚠ The OUTCOME comes before the failure, and the `⚠` marker is the one `crate::run` uses for
    // every other operator warning on stderr. This is the ONE path that recreates the condition
    // this writer was commissioned to end — a change on disk with no ledger row — so its wording is
    // the one that most needs to be unmistakable. "the change journal could not record X: <err>" on
    // stderr, next to a success report on stdout, reads as "the command failed" to a tired operator
    // — and stderr is what they notice first, and what a `2>&1 | tail` shows.
    let record = |outcome: Outcome, change: Change| {
        if let Some(j) = &journal
            && let Err(e) = j.append(ctx.now_ms, &change)
        {
            match outcome {
                Outcome::Applied | Outcome::AppliedPendingRestart => eprintln!(
                    "{}: ⚠ {key} WAS written, but the change journal could not record it: {e}",
                    ctx.surface
                ),
                Outcome::Stranded => unreachable!(
                    "a row-native write never leaves this state — see this module's doc"
                ),
                Outcome::Refused => eprintln!(
                    "{}: ⚠ nothing was written, and the change journal could not record the \
                     refusal of {key} either: {e}",
                    ctx.surface
                ),
            }
        }
    };
    match vike_config::write_setting_row(dir, key, value, budget) {
        Ok(report) => {
            let section = key.split('.').next().unwrap_or(key);
            record(
                Outcome::AppliedPendingRestart,
                Change::set_setting(
                    Outcome::AppliedPendingRestart,
                    Actor::cli("vike-cli"),
                    section,
                    &report.key,
                    report.old_value.as_deref(),
                    &report.new_value,
                ),
            );
            Ok(report)
        }
        Err(e) => {
            let error = e.to_string();
            let section = key.split('.').next().unwrap_or(key);
            // ⚠ `old: None` here is "no previous value was established", NOT "the key was unset in
            // the store" — the writer refused, so it either never read one or never reached the
            // point of reading one. `vike_model::change_journal::SettingTarget`'s own doc carries
            // that rule.
            record(
                Outcome::Refused,
                Change::set_setting(
                    Outcome::Refused,
                    Actor::cli("vike-cli"),
                    section,
                    key,
                    None,
                    value,
                )
                .with_reason(Some(&error)),
            );
            Err(rung(&e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exit::Exit;

    fn ctx<'a>(dir: &'a Path, state: &'a Path) -> Ctx<'a> {
        Ctx { settings_dir: Some(dir), state_dir: Some(state), now_ms: 1, surface: "test" }
    }

    /// Every file under the journal's state directory, concatenated — the ledger's layout is its
    /// own business, so these tests read what it wrote rather than reconstructing a path.
    fn journal_text(state: &Path) -> String {
        let mut out = String::new();
        let mut stack = vec![state.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(t) = std::fs::read_to_string(&p) {
                    out.push_str(&t);
                }
            }
        }
        out
    }

    fn seeded_dir() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        vike_secrets::plant_settings_rows(d.path(), &vike_secrets::StoredSettings::default())
            .expect("a fresh store plants");
        d
    }

    /// The ledger records a SUCCESS with the cells an after-the-fact reader needs, and the outcome
    /// says restart-pending rather than applied.
    #[test]
    fn an_accepted_write_is_journalled_as_applied_pending_restart() {
        let d = seeded_dir();
        let state = d.path().join("state");
        let report = set_setting_journalled(
            &ctx(d.path(), &state),
            "config.tradehub_addr",
            "127.0.0.1:7879",
        )
        .expect("the write must land");
        assert_eq!(report.old_value, None);
        let text = journal_text(&state);
        assert!(text.contains("set_setting"), "{text}");
        assert!(text.contains("config.tradehub_addr"), "{text}");
        assert!(text.contains("pending_restart"), "the outcome must not read as applied: {text}");
    }

    /// …and a REFUSAL is recorded too, with the writer's own reason.
    #[test]
    fn a_refused_write_is_journalled_with_its_reason_and_exits_on_the_usage_rung() {
        let d = seeded_dir();
        let state = d.path().join("state");
        let err = set_setting_journalled(&ctx(d.path(), &state), "config.no_such_setting", "1")
            .expect_err("an unknown key must refuse");
        assert_eq!(err.exit, Exit::Usage, "a key the loader will never take is a usage error");
        let text = journal_text(&state);
        assert!(text.contains("refused"), "{text}");
        assert!(text.contains("config.no_such_setting"), "{text}");
    }

    /// **The credential fence lives at the SHARED writer**, so every caller inherits it rather than
    /// remembering it — asserted through `set_setting_journalled` itself, which is the entry point
    /// `crate::cmd::node`'s adapter and `crate::cmd::config::set` both reach.
    #[test]
    fn a_credential_shaped_key_is_refused_at_the_shared_writer() {
        let d = seeded_dir();
        let state = d.path().join("state");
        for key in ["config.bot_token", "preferences.client_secret", "flags.api_key"] {
            let e = set_setting_journalled(&ctx(d.path(), &state), key, "x")
                .expect_err("a credential-shaped key must be refused by the writer itself");
            assert_eq!(e.exit, Exit::Usage, "{key}");
            assert!(e.msg.contains("secrets set"), "{}", e.msg);
            assert!(e.msg.contains("stdin"), "{}", e.msg);
            assert!(
                e.msg.contains("config show"),
                "a mistyped SETTINGS key must be pointed at the key list, not the store: {}",
                e.msg
            );
        }
        assert!(!state.exists(), "a verb-level refusal writes no ledger row");

        // …and an ordinary settings key passes the gate.
        set_setting_journalled(&ctx(d.path(), &state), "config.tradehub_addr", "127.0.0.1:7879")
            .expect("an ordinary settings key must pass");
    }

    /// **A `Busy` refusal takes the RUN rung, not the usage one**, and lands a `Refused` ledger row.
    ///
    /// The rung is the half a script acts on: re-running this UNCHANGED can succeed, which is
    /// exactly the half of `Exit::Usage`'s promise it would break.
    #[test]
    fn a_busy_database_is_a_run_failure_and_is_journalled_as_refused() {
        let d = seeded_dir();
        let state = d.path().join("state");
        let _held = vike_secrets::hold_write_lock(d.path());

        let e = set_setting_journalled_within(
            &ctx(d.path(), &state),
            "flags.reconcile",
            "true",
            std::time::Duration::from_millis(20),
        )
        .expect_err("a held database must refuse");
        assert_eq!(e.exit, Exit::Failed, "re-running this unchanged CAN succeed");
        let text = journal_text(&state);
        assert!(text.contains("refused"), "a busy refusal is a refusal: {text}");
    }

    /// A box with no project above it journals NOTHING and still writes — the disposition
    /// `vike_boot::journal_boot_settings` states, asserted rather than assumed.
    #[test]
    fn no_state_directory_journals_nothing_and_still_writes() {
        let d = seeded_dir();
        let c = Ctx { settings_dir: Some(d.path()), state_dir: None, now_ms: 1, surface: "test" };
        set_setting_journalled(&c, "flags.reconcile", "true")
            .expect("the write must land without a ledger");
    }
}
