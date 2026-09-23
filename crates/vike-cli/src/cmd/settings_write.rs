//! **The ONE journalled settings write in this binary** — `vike_config::set_setting` plus the
//! `vike_model::change_journal` record, shared by every verb that edits a settings TOML.
//!
//! # Why this is a module and not a function on one command
//!
//! It began as `crate::cmd::node`'s private `set_setting_journalled`, typed on that command's own
//! `Ctx` (which carries a keyring and a dial address a settings write uses for nothing) and with
//! that command's name baked into two operator-facing strings. [`crate::cmd::config_set`] needed
//! the same decisions — which outcome to record, which cells to fill, what a journal failure may
//! and may not do to the call, which exit rung each refusal takes — and this repository's standing
//! rule is that the second copy of a rule rots. So the BODY moved here and `crate::cmd::node`'s
//! `set_setting_journalled` became an adapter that supplies its own surface name and prints its own
//! confirmation line.
//!
//! # The five decisions this module owns
//!
//! 1. **`Outcome::AppliedPendingRestart`, always.** A write performed by a LOCAL process reaches no
//!    applier: the only hot-apply path in the tree is the WIRE arm
//!    (`crates/vike-tradehub/src/server.rs`'s `apply_set_setting`, which consults
//!    `crates/vike-tradehub/src/hot_reload.rs`'s `classify` and then pokes a `HotApplyHandle` the
//!    daemon holds). Nothing in a CLI process holds one, and nothing in this tree watches a
//!    settings file — `crates/vike-tradehub/src/tradehub_cli.rs`'s boot anchor says so in as many
//!    words. So "applied, pending restart" is not a simplification here; it is the only outcome a
//!    local write can honestly claim.
//! 2. **BOTH outcomes are recorded.** A failure appends its own record carrying the
//!    loader's own reason — which is what `crates/vike-app-core/src/tool_views/venues.rs` does for
//!    the GUI-local arming write, and is deliberately NOT what the daemon's wire path does —
//!    there, `accept_command` reaches the journal after `apply_set_setting` has already returned
//!    through a `?`, so an early return skips it. ⚠ That asymmetry was called *"an artifact of
//!    statement order rather than a decision"* here, and the daemon side has since been made to
//!    argue it where it happens (`crates/vike-tradehub/src/server.rs`'s `accept_command`, at the
//!    `?`): that surface journals no refusal for ANY control verb, so recording one for `SetSetting`
//!    alone would make its trail inconsistent with itself. It remains the under-recording
//!    direction and is declared as such on both sides; the LOCAL surface still follows the half
//!    that records, because nothing else on this box records the attempt at all. What is NOT
//!    recorded is a refusal by the CALLING VERB — a secret-shaped key, a missing typed confirm —
//!    because nothing was attempted against a file: this ledger's `set_setting` record carries a
//!    file cell, an old value and a new one, and a command line that never reached the writer has
//!    none of the three.
//!
//!    ⚠ **WHICH outcome a failure records is not this module's call and is not a constant.**
//!    `vike_config::journal_outcome` decides, because the writer is the authority on what it did:
//!    every variant is `Outcome::Refused` except `SettingsWriteError::Stranded`, which is
//!    `Outcome::Stranded`. That one moved the previous file aside and left the target absent, so a
//!    row reading "refused and nothing was written" would be the opposite of the event — and a
//!    ledger reader branches on the outcome. Both journalling surfaces used to answer `Refused`
//!    flat, which reproduced, in this branch's own error path, the confidently-wrong
//!    accountability record the ledger exists to remove.
//! 3. **A journal failure never fails the call.** The bytes ARE on disk; sending a caller down an
//!    error path for a write that succeeded is worse than a missing ledger line. Same disposition
//!    as `crate::cmd::node`'s `record_credential_write` and `vike_ctrader::token_store`'s
//!    `record_rotation`.
//! 4. **The exit rung is classified here**, so two verbs cannot answer differently. See [`rung`].
//! 5. **A credential-shaped KEY is refused HERE, at the shared writer** ([`refuse_a_secret_key`]),
//!    not at whichever verb happens to be calling. It began at the leaf caller, which was safe
//!    only for as long as the caller set stayed two — `crate::cmd::node`'s three call sites write
//!    fixed literals and `crate::cmd::config_set` carried its own gate — and that is the shape
//!    `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`'s first
//!    reason names: every writer is a surface the rule has to be re-proven on. A third caller now
//!    inherits the journal, the outcome, the rung, the no-path rule AND the fence, rather than
//!    three of the five. Nothing pins this caller set the way
//!    `crates/vike-ops/tests/credential_writer_gate.rs`'s `WRITER_CALLERS` pins the credential one,
//!    which is exactly why the fence must be structural rather than remembered.
//!
//!    ⚠ **Its predicate over-matches, and the residual is declared rather than assumed away.**
//!    `vike_config::is_secret_key` is a REDACTION predicate: it matches a leaf ending in any of
//!    `vike_config::redact::SECRET_SUFFIXES`, where over-matching is free (a knob wrongly printed
//!    as `<set>` costs one grep). Reused as a WRITE REFUSAL it is not free — `_USER` and `_LOGIN`
//!    in particular name IDENTIFIERS rather than secrets, so a future `config.db_user` or
//!    `config.proxy_login` would be a legitimate settings field this verb could not write.
//!    **Forking the predicate is refused**: `crates/vike-config/src/redact.rs`'s module doc argues
//!    that a second copy of a SECURITY table is the one duplication this tree cannot afford, and a
//!    narrower write-side twin is exactly that copy. What is done instead is to make the refusal
//!    answer BOTH readings — a credential, or a settings key this verb does not take — so neither
//!    operator is routed into a dead end. The day such a field lands the refusal names the way out,
//!    and `crates/vike-config/tests/boot.rs`'s
//!    `the_redaction_shapes_cover_a_credential_shaped_settings_key` is where the emptiness of the
//!    overlap is asserted rather than believed.
//!
//!    ⚠ **Moving the gate here was immediately load-bearing, and the evidence is a test that went
//!    red.** `a_refused_write_is_journalled_with_its_reason_and_exits_on_the_usage_rung` used
//!    `config.no_such_key` as its unknown-key fixture — a leaf ending `_key`, so the fence caught
//!    it before the loader could, and no `Outcome::Refused` row was appended. That is the gate
//!    working (a verb-level refusal writes no ledger row, decision 2), and it is also a live
//!    demonstration of the over-match above: the fixture is now `config.no_such_setting`. A fixture
//!    for an UNKNOWN key must not also be credential-SHAPED, or it tests two things and pins
//!    neither.
//!
//! ⚠ **No `--file <path>` may ever reach this module**, and that rule is inherited rather than
//! invented: `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`
//! lists "a write aimed at an operator-supplied PATH" among the things that re-decide it from the
//! top. Its SUBJECT is the credential store, which no settings write touches — but the reason
//! transfers whole, and the destination here is the dispatcher-resolved settings directory,
//! relocatable only by the one variable that moves the store, the ledger and the policy together.

use std::path::Path;

use vike_config::{LockBudget, SettingsFile, SettingsWrite, SettingsWriteError};
use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome, Proc};

use crate::exit::{CliError, CmdResult};

/// **The budget this surface gives the settings-directory lock: the whole-process one.**
///
/// ⚠ The shared writer's budget used to be a private constant every caller inherited, and its
/// argument was written for THIS caller and no other: *"a CLI that hangs forever at 03:00 with
/// nothing on the terminal is the worst outcome available here — there is no supervisor to notice
/// and no timeout above it."* That is exactly right about a CLI and was wrong about the two callers
/// it also governed (an egui frame and a daemon connection thread), which is why the budget is now
/// a parameter — see `vike_config::write`'s module doc.
///
/// So this site does not get a new number; it gets the OLD one, named and argued where it is
/// chosen. A `vike-cli config set` is the whole process: nothing else is waiting on this thread,
/// there is no frame to paint and no peer to answer, and a human typed the command and is watching
/// the terminal. ~3 s is two orders of magnitude of headroom over the critical section and is short
/// enough that the operator never has to wonder whether the command is hung — and when it does run
/// out, the refusal says NOTHING WAS WRITTEN and exits on a rung whose meaning is "re-running this
/// unchanged can succeed".
pub(crate) const CLI_LOCK_BUDGET: LockBudget = LockBudget::DEFAULT;

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
/// * [`SettingsWriteError::BadKey`] and [`SettingsWriteError::Validation`] are `Usage`: the command
///   line named a key the file has no place for, or a value its loader will never take. Nothing was
///   attempted, and re-running it unchanged cannot succeed — which is `crate::exit::Exit::Usage`'s
///   stated meaning.
/// * [`SettingsWriteError::CurrentFile`], [`SettingsWriteError::Io`],
///   [`SettingsWriteError::Lock`], [`SettingsWriteError::Busy`] and
///   [`SettingsWriteError::Stranded`] are the ordinary run
///   failure: the command line was fine, the BOX is not (a hand-broken TOML, a permission, a full
///   disk, a sentinel owned by another account, another writer holding the directory lock). A
///   caller FIXES a `2` and investigates a `1`.
///   `Busy` sits here rather than on the usage rung precisely because re-running it UNCHANGED can
///   succeed, which is the half of `Usage`'s promise it would break.
///
/// ⚠ **One imprecision, declared rather than hidden.** `Validation` runs the WHOLE would-be file
/// through the loader, so a file that already carried an unrelated defect refuses here too, and
/// that refusal is not about the command line. It still exits on the usage rung, because the half
/// of that rung's promise a caller acts on — re-running unchanged cannot succeed — holds in both
/// cases, and the message is the loader's own, which names the offending key either way.
///
/// ⚠ This is a DIVERGENCE from the wrapper this module was lifted from, which routed every refusal
/// through `CliError::failed`. It is inert for `crate::cmd::node`, whose three call sites write
/// fixed literals into keys the loader is known to accept, and it is the whole point for
/// [`crate::cmd::config_set`], where both the key and the value come from argv.
fn rung(e: &SettingsWriteError) -> CliError {
    let msg = e.to_string();
    match e {
        // ⚠ **Both usage refusals STATE THEIR DISPOSITION**, and `Validation` is why. It renders
        // the loader's own `ConfigError` Display — `<absolute path to policy.toml>: max_levrage:
        // unknown field …` — a message that names a real file on disk and nothing else. It is the
        // commonest refusal an operator will meet (a mistyped leaf, a wrong type), and to a tired
        // one at 03:00 it reads as a complaint about that file's CURRENT contents, i.e. "I have
        // just broken policy.toml", when not a byte moved. The loader's message stays verbatim
        // underneath, because it is the message a restart would have raised; what is added is the
        // three words that make the disposition readable from stderr alone rather than from the
        // exit code. `resolve_confirm`'s mismatch and `SettingsWriteError::Busy` already say it.
        SettingsWriteError::Validation(_) => CliError::usage(format!(
            "nothing was written — the would-be file was refused by the loader:\n  {msg}"
        )),
        SettingsWriteError::BadKey { .. } => {
            CliError::usage(format!("nothing was written — {msg}"))
        }
        SettingsWriteError::CurrentFile { .. }
        | SettingsWriteError::Io { .. }
        | SettingsWriteError::Lock { .. }
        | SettingsWriteError::Busy { .. }
        | SettingsWriteError::Stranded { .. } => CliError::failed(msg),
    }
}

/// **A credential-shaped key is refused here and never written**, whatever the file and whatever
/// the calling verb — decision 5 of this module's doc, which carries the whole argument including
/// the over-matching residual.
///
/// `vike_config::is_secret_key` matches on the dotted key's LEAF, and no settings field is
/// credential-shaped today, so this is unreachable on the current key set and can break no real
/// workflow. It is wired now because the alternative is remembering.
///
/// ⚠ **The message answers BOTH readings of a match**, and that is the fix for a real dead end:
/// the predicate catches any leaf ending `_KEY`/`_USER`/`_LOGIN`/… , so a plain typo
/// (`config.no_such_key`) used to be diagnosed "credential-shaped — `vike-cli secrets set <KEY>` is
/// the writer", and `secrets set` then refused it too because it is not in
/// `vike_model::credential_keys`. An operator who mistyped a settings key must be pointed at the
/// list of settings keys, not into the credential store.
pub(crate) fn refuse_a_secret_key(key: &str) -> CmdResult<()> {
    if !vike_config::is_secret_key(key) {
        return Ok(());
    }
    Err(CliError::usage(format!(
        "`{key}` has a credential-shaped name and no settings write will take one.\n  \
         If you meant a CREDENTIAL: they live in the store, not in the settings files — \
         `vike-cli secrets set <KEY>` is the writer, it takes the value on stdin rather than on \
         the command line, and `vike-cli secrets path` prints which file it opens.\n  \
         If you meant a SETTING: this is not a key the loader knows (no settings field is \
         credential-shaped) — `vike-cli config show` prints every key this verb takes, in the \
         spelling it takes them."
    )))
}

/// Set ONE settings key through `vike_config::set_setting` and journal the outcome, both ways.
///
/// The write is comment-preserving, loader-validated before a byte lands, and atomic; the record is
/// `vike_model::change_journal`'s `set_setting`, whose cells are exactly the ones that function
/// returns. Returns the [`SettingsWrite`] so the CALLER renders its own confirmation — this module
/// prints nothing on the success path, and exactly one line on the ledger-failure path.
pub(crate) fn set_setting_journalled(
    ctx: &Ctx<'_>,
    file: SettingsFile,
    key: &str,
    value: &str,
) -> CmdResult<SettingsWrite> {
    set_setting_journalled_within(ctx, file, key, value, CLI_LOCK_BUDGET)
}

/// [`set_setting_journalled`] with the lock budget as a parameter — the ONE line that binds this
/// binary to [`CLI_LOCK_BUDGET`], and the seam a contended test drives without spending the
/// shipped budget to prove a behaviour that is not about the number.
pub(crate) fn set_setting_journalled_within(
    ctx: &Ctx<'_>,
    file: SettingsFile,
    key: &str,
    value: &str,
    budget: LockBudget,
) -> CmdResult<SettingsWrite> {
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
    // The OUTCOME is a PARAMETER rather than read back off the change: `Change::outcome` is private
    // and adding an accessor to `vike-model` for one stderr line is the wrong direction of edit.
    //
    // ⚠ THREE texts, not two. It was a `bool` — written / not written — and
    // `Outcome::Stranded` is neither: nothing landed AND the previous file was moved aside. A line
    // saying "nothing was written" over that state is the same lie as the ledger row this branch
    // just stopped writing, on the stream the operator actually reads first.
    let record = |outcome: Outcome, change: Change| {
        if let Some(j) = &journal
            && let Err(e) = j.append(ctx.now_ms, &change)
        {
            match outcome {
                Outcome::Applied | Outcome::AppliedPendingRestart => eprintln!(
                    "{}: ⚠ {key} WAS written, but the change journal could not record it: {e}",
                    ctx.surface
                ),
                Outcome::Stranded => eprintln!(
                    "{}: ⚠ {key} was NOT written and the previous file was moved aside — see the \
                     error below for where it is — and the change journal could not record that \
                     either: {e}",
                    ctx.surface
                ),
                Outcome::Refused => eprintln!(
                    "{}: ⚠ nothing was written, and the change journal could not record the \
                     refusal of {key} either: {e}",
                    ctx.surface
                ),
            }
        }
    };
    // ⚠ `RowSync::Mirror` — this is an OPERATOR SHELL outside every daemon's mount namespace, so it
    // may write the rows and must: on an adopted box a settings file decides nothing, and a `config
    // set` that reported success while changing no resolved value would be the capability
    // regression `vike_config::RowSync` exists to refuse.
    match vike_config::set_setting_within(
        dir,
        file,
        key,
        value,
        budget,
        vike_config::RowSync::Mirror,
    ) {
        Ok(write) => {
            record(
                Outcome::AppliedPendingRestart,
                Change::set_setting(
                    Outcome::AppliedPendingRestart,
                    Actor::cli("vike-cli"),
                    write.file,
                    &write.key,
                    write.old_value.as_deref(),
                    &write.new_value,
                ),
            );
            Ok(write)
        }
        Err(e) => {
            let error = e.to_string();
            // ⚠ **NOT a blanket `Outcome::Refused`.** That variant's own definition is "the change
            // was refused and NOTHING WAS WRITTEN", which is true of every failure here except
            // `SettingsWriteError::Stranded` — where the previous file HAS been moved aside and the
            // target is absent, a larger change than the write would have been. A ledger reader
            // branches on the outcome, so filing that under `Refused` reproduced, in the one error
            // path this branch adds, exactly the confidently-wrong accountability record the lock
            // was taken to remove. `vike_config::journal_outcome` is the mapping, drawn once in the
            // crate that owns the error so the GUI and this surface cannot answer differently.
            let outcome = vike_config::journal_outcome(&e);
            // ⚠ **`old: None` here is "no previous value was established", NOT "the key was unset
            // in the file"** — the writer refused, so it either never read one or (on `Stranded`)
            // had the whole previous file moved aside underneath it, and `new` is the value that
            // did NOT land. `vike_model::change_journal::SettingTarget`'s own doc carries that
            // rule, because the alternative — a per-outcome row shape — would let a reader trust
            // these cells on the rows that still spell them. A reader BRANCHES on the outcome
            // first; that is the same discipline `journal_outcome` exists to keep honest, and it is
            // the reason this call passes the outcome rather than a constant.
            //
            // …and the stderr line follows the same fact: `Stranded` is not "nothing was written".
            record(
                outcome,
                Change::set_setting(
                    outcome,
                    Actor::cli("vike-cli"),
                    file.file_name(),
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

    /// The ledger records a SUCCESS with the cells an after-the-fact reader needs, and the outcome
    /// says restart-pending rather than applied.
    #[test]
    fn an_accepted_write_is_journalled_as_applied_pending_restart() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        let write = set_setting_journalled(
            &ctx(d.path(), &state),
            SettingsFile::Config,
            "config.tradehub_addr",
            "127.0.0.1:7879",
        )
        .expect("the write must land");
        assert_eq!(write.file, "config.toml");
        assert_eq!(write.old_value, None);
        let text = journal_text(&state);
        assert!(text.contains("set_setting"), "{text}");
        assert!(text.contains("config.tradehub_addr"), "{text}");
        assert!(text.contains("pending_restart"), "the outcome must not read as applied: {text}");
    }

    /// …and a REFUSAL is recorded too, with the loader's reason — the half the daemon's wire path
    /// skips by statement order (see this module's doc) and the half a local surface keeps.
    #[test]
    fn a_refused_write_is_journalled_with_its_reason_and_exits_on_the_usage_rung() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        let err = set_setting_journalled(
            &ctx(d.path(), &state),
            SettingsFile::Config,
            "config.no_such_setting",
            "1",
        )
        .expect_err("an unknown key must refuse");
        assert_eq!(err.exit, Exit::Usage, "a key the loader will never take is a usage error");
        let text = journal_text(&state);
        assert!(text.contains("refused"), "{text}");
        assert!(text.contains("config.no_such_setting"), "{text}");
        assert!(!d.path().join("config.toml").exists(), "a refused write creates no file");
    }

    /// **The credential fence lives at the SHARED writer** (decision 5), so every caller inherits
    /// it rather than remembering it — asserted through `set_setting_journalled` itself, which is
    /// the entry point `crate::cmd::node`'s adapter and `crate::cmd::config_set` both reach.
    ///
    /// The refusal is on the usage rung, nothing is written, and — the half that fixes a real dead
    /// end — the message answers BOTH readings of a shape match, so an operator who mistyped a
    /// SETTINGS key is pointed at `config show` rather than into the credential store.
    #[test]
    fn a_credential_shaped_key_is_refused_at_the_shared_writer() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        for key in ["config.bot_token", "preferences.client_secret", "flags.api_key"] {
            let e = set_setting_journalled(&ctx(d.path(), &state), SettingsFile::Config, key, "x")
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
        assert!(!d.path().join("config.toml").exists(), "nothing may be written");
        assert!(!state.exists(), "a verb-level refusal writes no ledger row (decision 2)");

        // …and an ordinary settings key passes the gate.
        set_setting_journalled(
            &ctx(d.path(), &state),
            SettingsFile::Config,
            "config.tradehub_addr",
            "127.0.0.1:7879",
        )
        .expect("an ordinary settings key must pass");
    }

    /// **THE BUDGET THIS SURFACE ASKED FOR, proven from the outside and without a stopwatch.**
    ///
    /// `SettingsWriteError::Busy` reports the budget it was GIVEN, so holding the directory and
    /// reading the number back off the real [`set_setting_journalled`] proves which budget the
    /// production path passed — no duration is measured and no timing is asserted.
    ///
    /// ⚠ It genuinely spends [`CLI_LOCK_BUDGET`] (~3 s) doing so, and that is the price of an
    /// airtight proof rather than a shape assertion on a constant the production path might not
    /// use. The kill is clean in both directions: point the call at a different budget and the
    /// reported number stops matching; delete the `LockBudget` parameter and it stops compiling.
    ///
    /// The sibling below drives the same plumbing with a millisecond budget, which is where the
    /// RUNG and the ledger row are asserted.
    #[test]
    fn the_cli_write_spends_the_budget_this_surface_declares() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(d.path().join(vike_config::SETTINGS_LOCK_FILE))
            .expect("the sentinel opens");
        held.try_lock().expect("this test must be the holder");

        let e = set_setting_journalled(
            &ctx(d.path(), &state),
            SettingsFile::Flags,
            "flags.reconcile",
            "true",
        )
        .expect_err("a held directory must refuse");
        assert!(
            e.msg.contains(&format!("waited {} ms", CLI_LOCK_BUDGET.max_wait_ms())),
            "the refusal must report the budget this surface chose ({} ms): {}",
            CLI_LOCK_BUDGET.max_wait_ms(),
            e.msg
        );
        assert!(CLI_LOCK_BUDGET.max_wait_ms() > 0, "a CLI is the whole process — it may wait");
        drop(held);
    }

    /// **A `Busy` refusal takes the RUN rung, not the usage one**, and lands a `Refused` ledger row
    /// — driven through the budgeted seam so it costs milliseconds rather than the shipped budget.
    ///
    /// The rung is the half a script acts on: re-running this UNCHANGED can succeed, which is
    /// exactly the half of `Exit::Usage`'s promise it would break.
    #[test]
    fn a_busy_directory_is_a_run_failure_and_is_journalled_as_refused() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(d.path().join(vike_config::SETTINGS_LOCK_FILE))
            .expect("the sentinel opens");
        held.try_lock().expect("this test must be the holder");

        let e = set_setting_journalled_within(
            &ctx(d.path(), &state),
            SettingsFile::Flags,
            "flags.reconcile",
            "true",
            vike_config::LockBudget::NON_BLOCKING,
        )
        .expect_err("a held directory must refuse");
        assert_eq!(e.exit, Exit::Failed, "re-running this unchanged CAN succeed");
        assert!(e.msg.contains("NOTHING was written"), "{}", e.msg);
        let text = journal_text(&state);
        assert!(text.contains("refused"), "a busy refusal is a refusal: {text}");
        assert!(!d.path().join("flags.toml").exists(), "nothing may be written");
        drop(held);
    }

    /// **A usage refusal states its DISPOSITION**, which is the question this verb exists to
    /// answer: write, refuse, or partly-happened? `Validation` renders the loader's own message —
    /// `<absolute path>: max_levrage: unknown field …` — which names a real file and nothing else,
    /// and reads to a tired operator as "I have just broken policy.toml" when not a byte moved.
    #[test]
    fn a_usage_refusal_says_that_nothing_was_written() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        let before = "tradehub_addr = \"127.0.0.1:7879\"\n";
        std::fs::write(d.path().join("config.toml"), before).unwrap();
        for key in ["config.no_such_setting", "preferences.log_level"] {
            let e = set_setting_journalled(&ctx(d.path(), &state), SettingsFile::Config, key, "1")
                .expect_err("a key this file cannot take must refuse");
            assert_eq!(e.exit, Exit::Usage, "{key}");
            assert!(
                e.msg.starts_with("nothing was written"),
                "the disposition must be readable from stderr alone: {}",
                e.msg
            );
        }
        assert_eq!(
            std::fs::read_to_string(d.path().join("config.toml")).unwrap(),
            before,
            "and it is true"
        );
    }

    /// A box with no project above it journals NOTHING and still writes — the disposition
    /// `vike_boot::journal_boot_settings` states, asserted rather than assumed.
    #[test]
    fn no_state_directory_journals_nothing_and_still_writes() {
        let d = tempfile::tempdir().unwrap();
        let c = Ctx { settings_dir: Some(d.path()), state_dir: None, now_ms: 1, surface: "test" };
        set_setting_journalled(&c, SettingsFile::Flags, "flags.reconcile", "true")
            .expect("the write must land without a ledger");
        assert!(d.path().join("flags.toml").exists());
    }
}
