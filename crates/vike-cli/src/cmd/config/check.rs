//! `vike-cli config check` — **would this box's configuration load, and is anything the operator
//! believes is armed silently not there?**
//!
//! The sibling of [`crate::cmd::config`]'s `show`, answering a different question with a different
//! output. `show` DISCLOSES — every setting, its effective value, and the layer that set it — and it
//! always exits 0 unless the loader itself refused. `check` JUDGES: it resolves the SAME directory
//! through the SAME loader and hands back an EXIT CODE, so a systemd unit can put it in an
//! `ExecStartPre=` and refuse to start rather than start degraded.
//!
//! Until this verb existed nothing did that. A daemon whose `settings/` was never created, or whose
//! credential store had become unreadable, STARTED ANYWAY: the settings walk answered nothing, the
//! credential loader logged one line and returned an empty map, and every venue silently dropped to
//! paper. Both halves were already correct in the library layer —
//! `crates/vike-secrets/src/store/secret_map.rs`'s `SecretsError` exists precisely so that "not configured" and
//! "cannot open" never look the same — but nothing at DEPLOY time acted on either.
//!
//! ⚠ "Acted on" is not "refuses". The two halves get DIFFERENT dispositions, and the table below is
//! where each one is argued: a named-and-missing settings directory refuses outright, while an
//! unreadable store refuses only on a box ARMED FOR LIVE and otherwise reports the degrade. The
//! whole point of the verb is that the two states stop being indistinguishable, not that both stop
//! a daemon.
//!
//! # The exit code IS the product, and it is not "any finding fails"
//!
//! `docs/decisions/0013-degrade-vs-refuse.md` is the rule this verb applies:
//!
//! > **Refuse** when a silent difference would leave the operator believing a protection is armed
//! > when it is not. **Degrade** when "unconfigured" is a legitimate state the operator may have
//! > chosen.
//!
//! So the three levels below are not severities anyone tuned — each row's level is an argument, and
//! [`Level::Fail`] is reserved for the cases that ADR calls a refusal:
//!
//! | finding | level | why |
//! |---|---|---|
//! | a removed environment variable is set | fail | the operator asked for a ceiling and does not have it — ADR question 2 |
//! | a settings row fails to load (an illegal value, an unknown or removed key) | fail | the same refusal a daemon performs; a `check` that passed here would contradict the binary it is checking |
//! | the credential store EXISTS and cannot be read | **warn — fail only when this box is ARMED FOR LIVE** | see the section below; the ADR classifies the daemon's own disposition here as a conforming degrade, and this verb does not overrule it |
//! | `VIKE_SETTINGS_DIR` names a path that is not a directory | fail | set-but-unhonoured. The operator named the directory; everything downstream degrades to defaults with no error |
//! | no settings directory at all | warn | the ordinary unconfigured state of a fresh clone and of CI |
//! | the credential store is ABSENT | **ok** | the LIVE GATE. `crates/vike-secrets/src/store/backend.rs`'s `resolve` calls this an ANSWER, not a failure, and is documented to be silent about it — a warning here would contradict that |
//! | the store is readable beyond its owner (mode `& 0o077`) | warn | `crates/vike-secrets/src/store/warnings.rs`'s `PermissionWarning`: "a finding is never a refusal". Refusing would strand somebody mid-setup with every venue on paper, which is worse than the exposure |
//!
//! ⚠ **The absent-store row is the one that decides whether this verb can be put in a unit at all.**
//! Every shipped daemon unit is a PAPER deployment with an empty credential store, so a `check` that
//! failed on an absent store would make a correct fresh install unstartable — the case
//! `docs/decisions/0013-degrade-vs-refuse.md` names under "what would reopen this" (a node that will
//! not start cannot flatten a position either).
//!
//! # The one ARMING-AWARE row, and why it is not an amendment to ADR 0013
//!
//! An unreadable credential store is the case this verb was written for, and it is also the one case
//! where a fixed level would be wrong in one direction or the other.
//!
//! ADR 0013's case table records the DAEMON's disposition — `vike_bridge_core::credentials`'s
//! `load_workspace_secrets_at` logs one line, returns an empty map, and every venue drops to paper —
//! as a **conforming degrade**, and that reading is correct on its own terms: falling back to paper
//! reduces the program's authority to act, which is the ADR's own refinement. This verb does not
//! contradict it and the ADR is unamended. **A pre-check that refused here unconditionally would
//! stop a working PAPER daemon over a file it never uses** — and every unit in `deploy/` is exactly
//! that: an empty store, no venue, no control surface.
//!
//! ⚠ **This paragraph used to cite the CI box as that paper box, and the CI box is NOT one.** Re-measured
//! 2026-08-09, read-only: its unit exports `VIKE_TRADEHUB_LIVE=1` from
//! `EnvironmentFile=-<project>/.env`, its `run-live.toml` is `mode = "live"`, and its store holds a
//! `BYBIT_DEMO_*` pair — so it runs the LIVE twelve-venue mount against a demo credential TIER,
//! which is a different thing from a paper mount and takes the FAIL branch below. Nothing about it
//! changes today (its store reads fine, and that unit deliberately carries no `ExecStartPre=` — the
//! shipped TEMPLATE does), but the claim was load-bearing for the WARN default and it was wrong, so
//! it is corrected rather than quietly dropped. The argument survives on the shipped units, which
//! are the paper deployments it needed.
//!
//! What the ADR's rule actually keys on is the OPERATOR'S BELIEF — "refuse when a silent difference
//! would leave the operator believing a protection is armed when it is not" — and whether they
//! believe a live venue is in force is not a matter of opinion here: it is encoded, in the arming
//! flags. So the level is computed from them:
//!
//! * **nothing armed ⇒ WARN.** Every venue was already going to be paper. The store still has to be
//!   fixed — "not configured" and "cannot open" must never look the same, which is why
//!   `crates/vike-secrets/src/store/secret_map.rs`'s `SecretsError` exists at all — but the box is doing what
//!   its configuration says. `--strict` counts it, which is the audience that wants it counted.
//! * **armed for live ⇒ FAIL.** The operator has asked for a real venue and the process cannot read
//!   the file that venue authenticates from, so it will place NO orders while every surface says
//!   live. That is ADR question 2 exactly: set-but-unhonoured, a false belief rather than a reduced
//!   one.
//!
//! ⚠ The arming signal is `vike_config::armed_for_live`, and it reads TWO independent sources —
//! never the credential store, which is both correct (`crates/vike-config/src/arming.rs` refuses an
//! arming line in that file outright) and unavoidable (the store is the thing that could not be
//! read). One is `vike_config::armed_settings_in` over the PROCESS ENVIRONMENT, the same table
//! `vike_config::CREDENTIAL_FILE_ARMING_REFUSED` uses. The other is the RESOLVED
//! `flags.tradehub_live`, and the section below is why it had to exist.
//!
//! ## ⚠ Why a second source: the env table can only see 4 of the 14 roster venues
//!
//! `CREDENTIAL_FILE_ARMING_REFUSED` holds `BINANCE_MAINNET`, `BYBIT_MAINNET`, `OKX_MAINNET`,
//! `HYPERLIQUID_MAINNET` (retired switches decision 0095 deleted — the rows stay so a stale one is
//! still refused rather than looking like configuration), `POLY_EXEC` and `POLY_RECONCILE`. Every
//! other roster venue — deribit, oanda, ig, fxcm,
//! dukascopy, ctrader, alpaca, ibkr and aster — has never had a `{VENUE}_MAINNET`-shaped switch and
//! picks its tier from the CREDENTIAL PREFIX
//! (`DERIBIT_LIVE_*` vs `DERIBIT_DEMO_*`) or from which gateway they log into. There is no flag to
//! read, so `armed_settings_in` alone returned nothing and a box running live on any of those read
//! as UNARMED: it took the WARN and the daemon started all-paper while every surface said live —
//! the set-but-unhonoured false belief this row exists to catch.
//!
//! ⚠ **It could not be closed by widening the table.** For a switchless venue, "am I armed for
//! live" is answerable only from the credential prefixes — inside the very file that could not be
//! read. The signal and the failure are the same object.
//!
//! **What closes it is `flags.tradehub_live`**, which is NODE-scoped rather than venue-scoped: it
//! selects `crates/vike-tradehub/src/tradehub_cli/live_mount.rs`'s `live_mount`, and that mount is
//! `crates/vike-mount/src/node/build.rs`'s `build_node`, which issues a `make_engine` call for every venue
//! in `WIRED_MARKETS` and takes each one live iff its credentials resolve. One boolean therefore
//! speaks for all twelve wired venues, switchless included, and it is resolved from the
//! `flags.tradehub_live` row or `VIKE_TRADEHUB_LIVE` — outside the credential rows. This verb
//! already loads it: `vike_config::describe` returns the resolved `Settings`, so the signal cost
//! no new input, no new flag and no second parser.
//!
//! ⚠ **The run profile was the obvious source and is the wrong one** — worth stating because it is
//! what the next reader will reach for. A profile names one venue (`venue = "bybit"`), but
//! `build_node` does not mount the profile's venue, it mounts the whole table; the profile picks
//! what the STRATEGY trades. MEASURED on the CI box (2026-08-09, read-only): `settings/tradehub.toml` and
//! `settings/run-live.toml` both name bybit and the store holds `BYBIT_DEMO_*` only — yet a
//! `DERIBIT_LIVE_*` pair appended to that same store goes live on the same start with no profile
//! edit. `crates/vike-config/src/arming.rs` carries the full argument.
//!
//! ## ⚠ What REMAINS after that, stated rather than implied
//!
//! * **The GUI's residual is gone with its mount.** `vike-app` had no live gate at all — a venue
//!   mounted live iff its credentials were present, so on that box "armed for live" WAS "the store
//!   has credentials" — and it was tolerated as interactive, with no shipped unit running this verb
//!   for it. `vike-desktop` mounts no venue since the desktop lost its local core (#1610), so there
//!   is no GUI box left on which the question arises. (This bullet described that residual in the
//!   present tense until 2026-09-28.)
//! * **A venue armed by credential PREFIX under a daemon that is NOT `vike-tradehub`** is out of
//!   scope for the same reason: the node-scoped flag belongs to that daemon. Every other unit in
//!   `deploy/` whose `ExecStartPre=` runs this check places no orders at all. (This named
//!   `vike-recorder` and `vike-datahub` as "the other two" until 2026-09-28; the recorder's unit
//!   was deleted on 2026-09-10, and more units run the check now.)
//! * ⚠ **…which cuts the other way too, and is the one behaviour change worth knowing about before
//!   you install this.** The signal is the BOX's, not the caller's: this verb cannot know which
//!   daemon is about to start, so a co-located recorder that SHARES the tradehub's settings
//!   directory and `EnvironmentFile=` inherits its arming, and an unreadable store there now FAILS
//!   where it used to WARN. That is not hypothetical — MEASURED on the CI box (2026-08-09, read-only):
//!   `vike-recorder` and `vike-tradehub` run from the same `WorkingDirectory`, the same
//!   `VIKE_SETTINGS_DIR` and the same `EnvironmentFile=`, which carries `VIKE_TRADEHUB_LIVE=1`.
//!   Nothing breaks there today (that store reads fine, and neither installed unit carries an
//!   `ExecStartPre=`), but on a box built from the shipped TEMPLATE the pair would refuse together.
//!   Accepted deliberately: a recorder whose credential store is unreadable on a box that declares
//!   itself live is a real misconfiguration in either daemon's terms, and the alternative — asking
//!   the caller which daemon it is — is a flag an operator can get wrong, guarding a refusal that
//!   exists precisely because operators get things wrong. If it ever needs narrowing, the honest cut
//!   is a per-unit settings directory, not a `--for-daemon` argument.
//!
//! ## ⚠ And the hole one level up, which is why the verdict has three states
//!
//! A source that cannot be CONSULTED must not be reported as a source that said no — that is the
//! same "no signal ⇒ assume paper" inference, moved up a level. `vike_config::armed_for_live`
//! therefore returns `LiveArmingVerdict::Undetermined` when the settings tree did not load, and
//! `refuses()` is true for it, so the unreadable-store row FAILS rather than degrading. In this verb
//! that branch is belt-and-braces: a tree that will not load already fails at the dispatcher (see
//! the `settings rows` finding's note in [`inspect`]). It is enforced in the type anyway, because the
//! cost is one variant and the failure it prevents is the one this whole section is about.
//!
//! # `--strict` exists so the two audiences do not have to share one disposition
//!
//! `--strict` promotes every warning to a failure. It is for an operator ASKING "is this box fully
//! configured?", never for a unit: pointing an `ExecStartPre=` at it would refuse a paper daemon
//! over a 0644 store, which is exactly the refusal `crates/vike-secrets/src/store/warnings.rs`'s
//! `PermissionWarning` argues against. The shipped units run the default disposition, and their
//! comments say why.
//!
//! A `<project>/.env` beside an absent store is no finding: nothing reads a credential from a file
//! since the credential FILE store was removed (2026-10-07), and every daemon unit in `deploy/`
//! carries that file as its `EnvironmentFile=` for operator tunables.
//!
//! # What this verb deliberately does NOT check
//!
//! `vike_config::refuse_credential_file_arming` — the refusal that stops an APPENDED line in the
//! credential store from arming real money. It is a genuine startup refusal, and the daemons run it
//! themselves: `vike-boot`'s step 3 performs it for every root that boots with
//! `Credentials::LoadWith`, `vike-tradehub` among them, so a unit whose store arms real money
//! already refuses to start without this verb's help. ⚠ This said "only `vike-app` performs it
//! today", and that folding it in here would make an `ExecStartPre=` refuse a `vike-tradehub` the
//! daemon itself would start, until 2026-09-28 — the fix it asked for ("give the daemon the call")
//! is what `vike-boot` did.
//!
//! ⚠ **The arming-aware row above is not the same shape, and the difference is the whole test this
//! file applies.** That row DETECTS a false belief — the operator asked for live, and the process
//! cannot reach the credentials, so what runs is not what they configured. Refusing on
//! `refuse_credential_file_arming` would instead veto a configuration that WORKS exactly as its
//! operator wrote it, on a hardening argument (a plaintext file should not be an arming surface);
//! that is a policy change, and a policy change belongs in the daemon that will live with it, not in
//! the guard standing in front of it. A pre-check may detect; it may not legislate. Note also that
//! the arming row reads the PROCESS ENVIRONMENT and never treats an arming flag as an offence in
//! itself — it only decides how bad an already-broken store is.
//!
//! # Redaction
//!
//! Same rule as `show`, and it is load-bearing for the same reason: this output lands in a journal
//! and in pasted issues. The credential store is disclosed by KEY COUNT only — never a name, never a
//! value — and every other detail string is a PATH, a count, or a message from a library documented
//! to carry neither (`vike_secrets::PermissionWarning`, `vike_config::ConfigError`,
//! whose own end-to-end gate is
//! `crates/vike-cli/tests/config_cli.rs`'s `an_unknown_row_key_never_echoes_its_value`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_secrets::SETTINGS_DIR_ENV;

use crate::cmd::args::{self, Flags};

pub(crate) const USAGE: &str = "\
usage: vike-cli config check [--json] [--strict]

  --json      the same report as one JSON object
  --strict    promote every WARNING to a failure. For an operator asking `is this box fully
              configured?`, NOT for a systemd ExecStartPre= — it refuses a paper deployment
              over an absent-but-legitimate configuration

exit 0 = nothing is broken; exit 1 = something the operator believes is configured is not";

// ---------------------------------------------------------------------------------------------
// Where the settings directory came from
// ---------------------------------------------------------------------------------------------

/// Which rung answered "where is `<project>/settings`?" — an answer only the DISPATCHER has.
///
/// It arrives as a parameter rather than being re-derived here for the reason `config show`'s
/// `settings_dir` does: a command that re-resolved the directory could name one nothing else reads.
/// It also keeps `VIKE_SETTINGS_DIR` out of this file as a read — the composition root owns the one
/// `std::env::vars()` sweep, and this module only needs to be TOLD which rung won.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirOrigin {
    /// `VIKE_SETTINGS_DIR` named it outright, so no walk happened at all.
    Named,
    /// The project walk found it: a checkout's workspace root, else a deployment's own `settings/`.
    Walk,
    /// Neither — no project above the working directory and no override.
    Unresolved,
}

impl DirOrigin {
    /// The stable wire spelling, pinned by test: `--json` is meant to be read by a monitor.
    fn as_str(self) -> &'static str {
        match self {
            DirOrigin::Named => "named",
            DirOrigin::Walk => "walk",
            DirOrigin::Unresolved => "none",
        }
    }

    /// How the header says it, in the operator's own terms.
    pub(crate) fn phrase(self) -> String {
        match self {
            DirOrigin::Named => format!("named outright by {SETTINGS_DIR_ENV}"),
            DirOrigin::Walk => "found by the project walk".to_string(),
            DirOrigin::Unresolved => "not resolved".to_string(),
        }
    }
}

/// Which rung answered, given the override's RAW value and what the resolver returned. PURE.
///
/// ⚠ The blank-value rule is `vike_secrets::project_settings_dir_from`'s, restated here because this
/// function has to agree with it and cannot call it. A whitespace-only `VIKE_SETTINGS_DIR` falls
/// THROUGH to the walk there, so reporting it as [`DirOrigin::Named`] would name a rung that did not
/// answer — and, worse, would take the FAIL disposition below on a directory nobody named.
/// `a_blank_override_is_the_walk_in_both_the_resolver_and_the_report` holds the two to the same rule.
pub(crate) fn dir_origin(override_value: Option<&str>, resolved: Option<&Path>) -> DirOrigin {
    if resolved.is_none() {
        return DirOrigin::Unresolved;
    }
    match override_value.map(str::trim).filter(|s| !s.is_empty()) {
        Some(_) => DirOrigin::Named,
        None => DirOrigin::Walk,
    }
}

// ---------------------------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------------------------

/// What one finding means for the exit code. Ordered, so the worst finding is `.max()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    /// Nothing to do. Includes the deliberately-unconfigured states — see the module doc's table.
    Ok,
    /// A degrade the operator may have chosen. Exit 0, unless `--strict`.
    Warn,
    /// A refusal: something the operator believes is configured is not. Always exit 1.
    Fail,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "fail",
        }
    }

    /// The human column. Fixed width so the details line up without a width pass.
    fn tag(self) -> &'static str {
        match self {
            Level::Ok => "OK  ",
            Level::Warn => "WARN",
            Level::Fail => "FAIL",
        }
    }
}

/// One thing that was checked, and what was found.
///
/// INVARIANT: `detail` never contains a credential VALUE — see the module doc's redaction note. It
/// may be multi-line (`vike_config::refuse_removed_env` returns a whole operator-facing block);
/// [`print_human`] indents the continuations.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Finding {
    /// What was checked, as the operator names it (`settings database`, `credential store`).
    subject: String,
    level: Level,
    detail: String,
}

fn finding(subject: impl Into<String>, level: Level, detail: impl Into<String>) -> Finding {
    Finding { subject: subject.into(), level, detail: detail.into() }
}

/// Everything one `check` run found.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Report {
    settings_dir: Option<PathBuf>,
    origin: DirOrigin,
    findings: Vec<Finding>,
}

impl Report {
    fn count(&self, level: Level) -> usize {
        self.findings.iter().filter(|f| f.level == level).count()
    }

    /// The worst thing found — `Ok` for an empty report, which cannot happen (every run checks at
    /// least the removed environment) but must not panic if it ever did.
    fn worst(&self) -> Level {
        self.findings.iter().map(|f| f.level).max().unwrap_or(Level::Ok)
    }

    /// The whole product of this command. `--strict` promotes warnings; nothing promotes an `Ok`.
    fn failed(&self, strict: bool) -> bool {
        match self.worst() {
            Level::Fail => true,
            Level::Warn => strict,
            Level::Ok => false,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The inspection
// ---------------------------------------------------------------------------------------------

/// Run every check and collect the findings. The ONE place a level is decided, so the disposition
/// table in the module doc has a single implementation to disagree with.
///
/// `settings_dir` and `origin` come from the dispatcher; `env` is its one `std::env::vars()` sweep.
/// Everything else is read here, through the SAME entry points the daemons use — `vike_config::load`
/// (via `describe`) and `vike_secrets::resolve_store_in` — because a second parser would drift from the first
/// and a check that disagrees with the binary it is checking is worse than no check.
fn inspect(
    settings_dir: Option<&Path>,
    origin: DirOrigin,
    env: &HashMap<String, String>,
) -> Report {
    let mut findings = Vec::new();

    // ── 1. A REMOVED environment variable ────────────────────────────────────────────────────
    // ⚠ In practice this row can only ever read OK, and that is not an oversight: `crate::run`
    // calls `vike_config::refuse_removed_env` BEFORE routing any verb, so a set variable exits the
    // process with the full refusal long before `check` is reached. It is computed rather than
    // asserted so the row stays correct if that order ever changes, and so the report is complete —
    // an operator running `check` is asking WHICH things were checked, and a check nobody can see
    // was performed is indistinguishable from one that was not. The refusal itself is gated
    // end-to-end, through the shipped binary, by `crates/vike-cli/tests/config_check_cli.rs`'s
    // `a_removed_environment_variable_refuses_from_the_dispatcher`, whose name says which layer
    // answers.
    match vike_config::refuse_removed_env(env) {
        Ok(()) => {
            findings.push(finding("removed environment", Level::Ok, "no removed variable is set"))
        }
        Err(msg) => findings.push(finding("removed environment", Level::Fail, msg.trim_end())),
    }

    // ── 2. The settings directory itself ─────────────────────────────────────────────────────
    findings.push(settings_dir_finding(settings_dir, origin));

    // ── 3. The four settings files, through the loader every daemon goes through ─────────────
    // A load ERROR is one finding, not four: `vike_config::load` refuses at the FIRST offending
    // file exactly as a daemon does, so reporting what it refused is reporting what would happen.
    // Its message already names the file and the key.
    //
    // ⚠ The `Err` arm below is UNREACHABLE through the shipped binary, for the same reason the
    // removed-environment row above it can only ever read OK: `crate::run` calls `resolve_policy`,
    // which calls `vike_config::load` on this same directory, BEFORE routing any verb — and
    // `describe` starts by calling `load` again. A tree that fails here already exited the process
    // one layer up, with the loader's own message. Declared rather than deleted, and computed
    // rather than asserted, for the reasons that row gives: the ordering could change, a file could
    // change between the two loads, and a report an operator reads must say WHAT was checked. The
    // process-level refusal is gated in `crates/vike-cli/tests/config_check_cli.rs`, whose
    // "refusals that fire ONE LAYER UP" section pins which layer answers.
    //
    // ⚠ CAPTURED, not re-loaded. The node-scoped half of the arming signal is the RESOLVED
    // `flags.tradehub_live`, and it must be the same resolution this report just described —
    // re-calling `load` for it would be a second answer that can disagree with the first. `None`
    // here means the tree did not load AT ALL, which `armed_for_live` treats as "could not tell",
    // never as "not armed"; see the module doc's three-states section.
    //
    // ⚠ **THROUGH THE SETTINGS DATABASE TOO, and that is the whole reason this block is not
    // `describe`.** Decision 0057's Phase 1 makes the store a LAYER of the loader that
    // `vike_boot::boot` applies, so a pre-flight that resolved the four files ALONE would pass a
    // tree whose boot then refuses — from the `ExecStartPre=` of the unit whose start it was
    // supposed to protect. The rows go in exactly as the boot puts them in, so a typo'd row key or
    // a row that breaks a bound is refused HERE, by name, with the daemon still stopped.
    //
    // ⚠ A store that will not OPEN is a **FAIL** here, and the severity CHANGED with the crossing.
    // It was a `Warn` while the files always won, because an unreadable store then cost this box
    // nothing it resolved. It cannot stay one: whether a box resolves from the rows or from the
    // files is itself a fact IN the store, so a store that will not open means this command cannot
    // tell which artifact holds the ceilings — and this command's exit code is a deployed daemon's
    // `ExecStartPre=`. Reporting `Warn` would let that daemon start on a resolution nobody can
    // vouch for, which is the precise silence it exists to break.
    //
    // ⚠ The store finding is also where an ADOPTED box says so out loud: `SettingsSource`'s own
    // `Display` renders the seal, so *"ADOPTED … the rows answer and the settings files are not
    // read"* is one `Level::Ok` line rather than something an operator has to infer from a path.
    let store = settings_dir.map(vike_secrets::read_settings_in);
    match &store {
        Some(Ok(found)) => {
            findings.push(finding("settings database", Level::Ok, found.to_string()));
        }
        Some(Err(e)) => {
            findings.push(finding(
                "settings database",
                Level::Fail,
                format!(
                    "{e} — this box's settings store could not be opened, so every ceiling below \
                     resolves to its compiled-in default. There is no repair command \
                     (`docs/decisions/0086`): restore the settings database from the box's nightly \
                     backup"
                ),
            ));
        }
        None => {}
    }
    let mut store_refusal_scratch = String::new();
    let source = vike_config::StoreLayer::of(store.as_ref(), &mut store_refusal_scratch);
    // ⚠ **THE ENFORCEMENT POINT, and until this block existed it reported `OK` for both of the
    // states below.** `vike_config::Settings::seal_refusal`'s doc names this command as the surface
    // that stops a start — a deploy pre-flight and the daemons' `ExecStartPre=` both run it — which
    // is what licenses `apply_rows` marking rather than refusing. The `Some(Ok(found))` arm
    // above pushes `Level::Ok` unconditionally, rendering `SettingsSource`'s `Display`; measured at
    // `650907a37`, a box with a destroyed settings table printed *"ADOPTED … so the rows answer"*
    // and `Nothing is broken.` at exit 0.
    if let Some(Ok(src)) = &store
        && let (Some(found), Some(seal)) = (src.rows(), src.adoption())
    {
        // An ADOPTED box with no rows resolves every value from the compiled-in defaults, which
        // for `policy.max_notional_per_order` is NO CEILING. `config_adopt.rs`'s `adopt` refuses to
        // CREATE this state by name — *"sealing a store with no settings rows would seal this box
        // into resolving from NOTHING — no ceiling, no dead-man"* — so a box that is IN it arrived
        // by some other route, and the command that refuses to create it must not report it as OK.
        if found.settings.is_empty() && found.arming.is_empty() {
            findings.push(finding(
                "settings seal",
                Level::Fail,
                "this box's settings store is SEALED and carries NO settings rows at all, so \
                 every value it resolves is a compiled-in default — no ceiling, and every venue \
                 capped `paper` with nothing recording that an arming was ever stated. A sealed \
                 store with no rows means the rows were erased after the seal — a write always \
                 leaves at least the row it just wrote. There is no repair command \
                 (`docs/decisions/0086`): restore the settings database from the box's nightly \
                 backup"
                    .to_string(),
            ));
        }
        match vike_config::adoption_integrity(found, seal) {
            Ok(warnings) => {
                for why in warnings {
                    findings.push(finding("settings seal", Level::Warn, why));
                }
            }
            Err(e) => findings.push(finding("settings seal", Level::Fail, e.to_string())),
        }
    }
    // ⚠ **The UNKNOWN-SECTION row, named — the declared residual of
    // `crates/vike-config/src/mirror.rs`'s `section_values`, which reads such a row as nothing.**
    // That reader deliberately skips rather than refusing (see its own note for why), and this is
    // the surface where the skip stops being silent: `config show`'s ORIGIN column can only report
    // a key it RESOLVED, so a row outside the four-word vocabulary is invisible everywhere else.
    // `Fail`, unconditionally: `docs/decisions/0086` makes the rows the ONLY settings layer on
    // every box, so such a row is the only copy of whatever it holds on every box that carries one.
    if let Some(Ok(found)) = &store
        && let Some(rows) = found.rows()
    {
        let unknown: Vec<&str> = rows
            .settings
            .iter()
            .map(|r| r.section.as_str())
            .filter(|s| !vike_secrets::section_is_known(s))
            .collect();
        if !unknown.is_empty() {
            findings.push(finding(
                "settings rows",
                Level::Fail,
                format!(
                    "{} row(s) carry a section outside `{:?}` ({unknown:?}) and are read as \
                     NOTHING — no loader applies them and no other surface can name them. \
                     `setting.section`'s own CHECK refuses one, so a hand `INSERT` against an \
                     altered schema produced these. There is no repair command \
                     (`docs/decisions/0086`): restore the settings database from the box's \
                     nightly backup.",
                    unknown.len(),
                    vike_secrets::SETTINGS_SECTIONS
                ),
            ));
        }
    }

    let mut resolved_flags: Option<vike_config::Flags> = None;
    match vike_config::describe_with_source(settings_dir, source, env) {
        Ok(described) => {
            resolved_flags = Some(described.settings.flags);
            // ⚠ **A row that will not parse or apply is MARKED, never returned as `Err`** —
            // `crate::mirror::apply_rows`'s own doc argues why: a hard `Err` here would take this
            // very command down with the box it exists to diagnose. So the mark is checked
            // EXPLICITLY, as its own `Fail` finding, rather than trusted to surface through the
            // generic warnings loop below (which also carries it, at `Warn`, since
            // `mark_seal_refusal` pushes both channels) — this is the ENFORCEMENT POINT
            // `vike_config::Settings::seal_refusal`'s own doc names for exactly this command.
            if let Some(why) = &described.settings.seal_refusal {
                findings.push(finding("settings rows", Level::Fail, why.clone()));
            }
            // The loader's non-fatal resolutions. It returns them as DATA rather than logging
            // (`vike_config::Settings::warnings` argues why), so somebody has to surface them, and
            // a command whose entire job is judging this tree is exactly that somebody.
            for w in &described.settings.warnings {
                findings.push(finding("settings", Level::Warn, w.clone()));
            }
        }
        Err(e) => findings.push(finding("settings rows", Level::Fail, e.to_string())),
    }

    // ── 4. The credential store ──────────────────────────────────────────────────────────────
    // `armed_for_live` decides ONE thing: whether an unreadable store is a degrade or a refusal
    // (the module doc's arming-aware section is the argument). It is not itself a finding — an
    // armed flag is a configuration, not a defect, and this verb detects rather than legislates.
    findings
        .extend(store_findings(settings_dir, &vike_config::armed_for_live(resolved_flags, env)));

    Report { settings_dir: settings_dir.map(Path::to_path_buf), origin, findings }
}

/// The settings directory's own row — the one place `VIKE_SETTINGS_DIR` earns a FAIL.
///
/// The split is ADR 0013's question 2, and nothing else: an ABSENT answer is a choice (a checkout
/// with no `settings/` is the ordinary state of this repo), while a NAMED directory that is not
/// there is a set-but-unhonoured value. Every daemon unit in `deploy/` names one, which is what
/// makes this row the pre-check's teeth: the install recipe that forgot `install -d …/settings` now
/// stops the unit instead of producing a daemon with no ceiling and no credentials.
fn settings_dir_finding(settings_dir: Option<&Path>, origin: DirOrigin) -> Finding {
    let Some(dir) = settings_dir else {
        return finding(
            "settings directory",
            Level::Warn,
            format!(
                "NONE — no project above the working directory and no {SETTINGS_DIR_ENV}. Every \
                 setting is a compiled-in default and no credential store can be found, so every \
                 venue stays paper."
            ),
        );
    };
    if dir.is_dir() {
        return finding(
            "settings directory",
            Level::Ok,
            format!("{} — {}", dir.display(), origin.phrase()),
        );
    }
    match origin {
        DirOrigin::Named => finding(
            "settings directory",
            Level::Fail,
            format!(
                "{dir} — {SETTINGS_DIR_ENV} names it and it is NOT a directory. Every setting \
                 falls back to a compiled-in default and no credential can be found, with no \
                 error anywhere: create it with `install -d -m700 {dir} {dir}/state`.",
                dir = dir.display()
            ),
        ),
        // The walk can answer with a directory that does not exist — `nearest_project_marker`
        // matches a `Cargo.toml` and then names its sibling `settings/`, which is every checkout
        // that has never configured anything. Nobody claimed it was there, so nobody is misled.
        DirOrigin::Walk | DirOrigin::Unresolved => finding(
            "settings directory",
            Level::Warn,
            format!(
                "{} — the project walk answered here, but the directory does not exist. Nothing \
                 is configured; create it, or name one with {SETTINGS_DIR_ENV}.",
                dir.display()
            ),
        ),
    }
}

/// The credential store's rows: what is there, and every finding the store layer hands back.
///
/// Read through `vike_secrets::resolve_store_in` on a settings DIRECTORY this command was HANDED,
/// which is the shape `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN`
/// deliberately blesses ("the composition root doing exactly what the rule asks for") rather than
/// one of the sweeping loaders that opens a location nothing in its signature mentions. It is also
/// the only entry point that can tell an UNREADABLE store from an absent one — the whole reason
/// this verb exists.
///
/// ⚠ **It reports the store that ANSWERS** — the settings database, the only credential store since
/// the credential FILE store was removed (2026-10-07).
///
/// `armed` is `vike_config::armed_for_live`'s verdict over the PROCESS ENVIRONMENT and the RESOLVED
/// flags, and it decides exactly one level: the unreadable-store row's. The module doc's
/// arming-aware section carries the argument; the short form is that an unreadable store on a paper
/// box is the degrade ADR 0013 records, and on a box armed for live it is a false belief.
fn store_findings(
    settings_dir: Option<&Path>,
    armed: &vike_config::LiveArmingVerdict,
) -> Vec<Finding> {
    let Some(dir) = settings_dir else {
        // Nothing to report beyond the directory row above, which already said every venue stays
        // paper. A second warning for the same fact would just teach people to scroll.
        return Vec::new();
    };
    let db = vike_secrets::db_path_in(dir);
    // ⚠ **The STORE THAT ANSWERS** — the settings database, the only credential store since the
    // credential FILE store was removed (2026-10-07).
    //
    // `resolve_store_in` is the one front door `vike_secrets::resolve_project` itself is written in
    // terms of, so what this row reports is now literally what a daemon's credential read would
    // return. The settings directory arrives as a `&Path` this command was handed, so the read
    // cannot walk for a store from a working directory — which is why this is not the defect
    // `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` ratchets down, on that
    // table's own stated criterion.
    let resolved = match vike_secrets::resolve_store_in(dir, vike_secrets::Table::Credential) {
        Ok(r) => r,
        Err(e) => return vec![unreadable_store_finding(&e.to_string(), armed)],
    };

    let mut out = Vec::new();
    // KEY COUNT only. Names are `vike-cli secrets list`'s job; values are nobody's.
    out.push(match &resolved.source {
        // ⚠ Says DATABASE and prints the DATABASE's path, deliberately — the same word
        // `vike-cli secrets list` uses. 0054's constraint 2 is that an operator reads the store with
        // `cat` today and `sqlite3` is not installed on the live box, so a row that named a `.db`
        // in a sentence saying "credential store" would hand them a path they cannot open and no
        // hint why.
        vike_secrets::Source::Database(p) => finding(
            "credential store",
            Level::Ok,
            format!(
                "{} — present, {} key(s). This is the settings DATABASE, not a text file.",
                p.display(),
                resolved.secrets.len()
            ),
        ),
        vike_secrets::Source::None => finding(
            "credential store",
            Level::Ok,
            format!(
                "{} — absent. Absent credentials ARE the live gate: every venue stays paper, \
                 which is the designed state of an unconfigured box. `vike-cli secrets init` \
                 creates the store.",
                db.display()
            ),
        ),
    });
    if let Some(w) = &resolved.warning {
        out.push(finding("credential store", Level::Warn, w.to_string()));
    }
    out
}

/// The one row whose LEVEL is computed rather than declared — see the module doc's arming-aware
/// section, which is the authority for the argument this function implements.
///
/// PURE, and split out for that reason: the disposition is the interesting part of this file and it
/// is decided in one place, over one input, so a test can drive both branches without a filesystem.
fn unreadable_store_finding(error: &str, armed: &vike_config::LiveArmingVerdict) -> Finding {
    match armed {
        vike_config::LiveArmingVerdict::Unarmed => finding(
            "credential store",
            Level::Warn,
            format!(
                "{error}. Nothing on this box is ARMED FOR LIVE, so the effect is the degrade \
                 `docs/decisions/0013-degrade-vs-refuse.md` records for the daemon itself: an empty \
                 map, every venue on paper, authority reduced rather than redirected. FIX IT ANYWAY \
                 — `no credentials` and `cannot open` must never look the same, which is the whole \
                 reason `vike_secrets::SecretsError` exists — but it does not stop this box \
                 starting. `--strict` counts it as an error."
            ),
        ),
        // ARMED. Every source is named in ONE pass, the shape
        // `vike_config::refuse_credential_file_arming` uses: fixing a box one restart at a time is
        // a worse experience than being handed the list. NAMES only — these are the environment and
        // the settings tree, not the store, and no value of any of them is echoed.
        vike_config::LiveArmingVerdict::Armed(sources) => {
            let names: Vec<&str> = sources.iter().map(|s| s.source).collect();
            finding(
                "credential store",
                Level::Fail,
                format!(
                    "{error}. This box is ARMED FOR LIVE ({}) and cannot read the file its venues \
                     authenticate from, so it would place NO orders while every surface reads live \
                     — set-but-unhonoured, the false belief \
                     `docs/decisions/0013-degrade-vs-refuse.md` refuses on (question 2). Repair the \
                     file's ownership/permissions, or disarm by clearing the source(s) above and \
                     start as the paper deployment you then have.",
                    names.join(", ")
                ),
            )
        }
        // ⚠ UNKNOWN is not UNARMED. The node-scoped source is a resolved setting and the settings
        // tree did not load, so nothing here can say this box is paper — and "no signal ⇒ assume
        // paper" is the exact inference that left a switchless-venue box degrading. Unreachable
        // through the shipped binary (the dispatcher refuses a tree that will not load, and this
        // report carries its own `settings files` FAIL besides), which is why it can afford to be
        // the strict answer: it costs a correct deployment nothing.
        vike_config::LiveArmingVerdict::Undetermined => finding(
            "credential store",
            Level::Fail,
            format!(
                "{error}. …and whether this box is ARMED FOR LIVE could not be determined, because \
                 the settings tree did not load — so `flags.tradehub_live`, the node-scoped half of \
                 that signal, has no resolved value. An unreadable store is only a tolerable degrade \
                 on a box known to be paper, and this one is not known to be anything. Fix the \
                 settings failure reported above first; this row is a consequence of it."
            ),
        ),
    }
}

// ---------------------------------------------------------------------------------------------
// Command entry
// ---------------------------------------------------------------------------------------------

/// The parsed `config check` command line.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    json: bool,
    strict: bool,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args::default();
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--json" => {
                args::no_value(&flag, inline)?;
                out.json = true;
            }
            "--strict" => {
                args::no_value(&flag, inline)?;
                out.strict = true;
            }
            "-h" | "--help" => return args::help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(out)
}

/// Entry point [`crate::cmd::config`] routes the `check` verb to.
///
/// `settings_dir` and `origin` are the dispatcher's own answers — see [`DirOrigin`].
pub(crate) fn run(
    args: impl Iterator<Item = String>,
    settings_dir: Option<&Path>,
    origin: DirOrigin,
) -> ExitCode {
    let args = match parse_args(args) {
        Ok(a) => a,
        Err(msg) => return args::exit_for_parse_error("config check", USAGE, &msg),
    };
    let env: HashMap<String, String> = std::env::vars().collect();
    let report = inspect(settings_dir, origin, &env);

    if args.json {
        match serde_json::to_string_pretty(&report_json(&report, args.strict)) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("vike-cli config check: cannot serialize the report: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        print_human(&report, args.strict);
    }

    if report.failed(args.strict) {
        // ONE line on stderr as well, so `journalctl -p err` catches an ExecStartPre= refusal
        // without anyone having to read the whole report first. The report itself stays on stdout:
        // it is this command's normal output, whatever the verdict.
        eprintln!(
            "vike-cli config check: FAILED — {} error(s), {} warning(s){}",
            report.count(Level::Fail),
            report.count(Level::Warn),
            if args.strict { " (--strict: warnings count as errors)" } else { "" }
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------------------------
// Printers
// ---------------------------------------------------------------------------------------------

/// The machine view — one object, so a monitor does not have to parse the table.
fn report_json(report: &Report, strict: bool) -> serde_json::Value {
    serde_json::json!({
        "settings_dir": report.settings_dir.as_ref().map(|p| p.display().to_string()),
        "settings_dir_origin": report.origin.as_str(),
        "strict": strict,
        // The verdict, pre-computed: a reader must not have to re-implement the `--strict`
        // promotion rule to learn what the exit code was.
        "ok": !report.failed(strict),
        "failures": report.count(Level::Fail),
        "warnings": report.count(Level::Warn),
        "findings": report.findings.iter().map(|f| serde_json::json!({
            "subject": f.subject,
            "level": f.level.as_str(),
            "detail": f.detail,
        })).collect::<Vec<_>>(),
    })
}

/// The human view: the header, one line per finding, then the verdict.
fn print_human(report: &Report, strict: bool) {
    match &report.settings_dir {
        Some(d) => println!("settings directory: {} ({})", d.display(), report.origin.phrase()),
        None => println!("settings directory: NONE ({})", report.origin.phrase()),
    }
    println!();

    let width = report.findings.iter().map(|f| f.subject.len()).max().unwrap_or(0);
    for f in &report.findings {
        let mut lines = f.detail.lines();
        println!("{}  {:<width$}  {}", f.level.tag(), f.subject, lines.next().unwrap_or_default());
        // A multi-line detail (the removed-variable refusal is a whole block) is indented under
        // the column rather than truncated: the paste-ready TOML line it carries is the entire
        // point of that message. The 8 is the header's own lead — a 4-char level tag plus the two
        // two-space gutters — so a continuation lands exactly under the first line's detail.
        for line in lines {
            println!("{:width$}        {line}", "");
        }
    }

    println!();
    let (fails, warns) = (report.count(Level::Fail), report.count(Level::Warn));
    println!("{fails} error(s), {warns} warning(s).");
    if fails > 0 {
        println!(
            "Something this box is configured for is not in force. Fix the FAIL lines above — a \
             daemon started in this state degrades silently."
        );
    } else if warns > 0 && !strict {
        println!(
            "Nothing is broken. The warnings are states an operator may legitimately have chosen \
             (`--strict` counts them as errors)."
        );
    } else if warns > 0 {
        println!("--strict: the warnings above are counted as errors.");
    }
}

#[path = "tests/check.rs"]
#[cfg(test)]
mod config_check_tests;
