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
//! `secrets.env` had become unreadable, STARTED ANYWAY: the settings walk answered nothing, the
//! credential loader logged one line and returned an empty map, and every venue silently dropped to
//! paper. Both halves were already correct in the library layer —
//! `crates/vike-secrets/src/store.rs`'s `SecretsError` exists precisely so that "not configured" and
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
//! | a settings file fails to load (parse error, unknown key, removed key) | fail | the same refusal a daemon performs; a `check` that passed here would contradict the binary it is checking |
//! | the credential store EXISTS and cannot be read | **warn — fail only when this box is ARMED FOR LIVE** | see the section below; the ADR classifies the daemon's own disposition here as a conforming degrade, and this verb does not overrule it |
//! | `VIKE_SETTINGS_DIR` names a path that is not a directory | fail | set-but-unhonoured. The operator named the directory; everything downstream degrades to defaults with no error |
//! | no settings directory at all | warn | the ordinary unconfigured state of a fresh clone and of CI |
//! | the credential store is ABSENT | **ok** | the LIVE GATE. `crates/vike-secrets/src/store.rs`'s `resolve` calls this an ANSWER, not a failure, and is documented to be silent about it — a warning here would contradict that |
//! | the store is readable beyond its owner (mode `& 0o077`) | warn | `crates/vike-secrets/src/store.rs`'s `PermissionWarning`: "a finding is never a refusal". Refusing would strand somebody mid-setup with every venue on paper, which is worse than the exposure |
//! | a pre-one-store `<project>/.env` beside an ABSENT store | warn | `crates/vike-secrets/src/store.rs`'s `LegacyStoreWarning`, and for its stated reason: a `.env` is also a legitimate systemd `EnvironmentFile` |
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
//!   `crates/vike-secrets/src/store.rs`'s `SecretsError` exists at all — but the box is doing what
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
//! `HYPERLIQUID_MAINNET`, `POLY_EXEC` and `POLY_RECONCILE`. Every other roster venue is what
//! `vike_bridge_core::mainnet::mainnet_switch_for` calls SWITCHLESS — deribit, oanda, ig, fxcm,
//! dukascopy, ctrader, alpaca, ibkr and aster pick their tier from the CREDENTIAL PREFIX
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
//! selects `crates/vike-tradehub/src/tradehub_cli.rs`'s `live_mount`, and that mount is
//! `crates/vike-run/src/node.rs`'s `build_node`, which issues a `make_engine` call for every venue
//! in `WIRED_MARKETS` and takes each one live iff its credentials resolve. One boolean therefore
//! speaks for all twelve wired venues, switchless included, and it is resolved from
//! `<project>/settings/flags.toml` or `VIKE_TRADEHUB_LIVE` — outside the store. This verb already
//! loads it: `vike_config::describe` returns the resolved `Settings`, so the signal cost no new
//! input, no new flag and no second parser.
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
//! * **`vike-app`, the GUI, has no live gate at all** — a venue mounts live iff its credentials are
//!   present, so on that box "armed for live" IS "the store has credentials" and the question stays
//!   unanswerable when the store cannot be read. It is interactive rather than an unattended unit,
//!   and no shipped unit runs this verb for it, so the residual is tolerated; closing it needs a
//!   live gate that binary does not have.
//! * **A venue armed by credential PREFIX under a daemon that is NOT `vike-tradehub`** is out of
//!   scope for the same reason: the node-scoped flag belongs to that daemon. `vike-recorder` and
//!   `vike-datahub` — the other two units running this check — place no orders at all.
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
//! the `settings files` row's note in [`inspect`]). It is enforced in the type anyway, because the
//! cost is one variant and the failure it prevents is the one this whole section is about.
//!
//! # `--strict` exists so the two audiences do not have to share one disposition
//!
//! `--strict` promotes every warning to a failure. It is for an operator ASKING "is this box fully
//! configured?", never for a unit: pointing an `ExecStartPre=` at it would refuse a paper daemon
//! over a 0644 store, which is exactly the refusal `crates/vike-secrets/src/store.rs`'s
//! `PermissionWarning` argues against. The shipped units run the default disposition, and their
//! comments say why.
//!
//! ⚠ **On ONE shipped layout `--strict` calls a CORRECT install an error, by construction.** Every
//! daemon unit in `deploy/` carries `EnvironmentFile=-<project>/.env` for its operator tunables, and
//! `docs/ops/recorder-deploy.md`'s step 5 creates that file while the recorder — which needs no
//! credentials — gets no `secrets.env` at all. Absent store PLUS a `<project>/.env` is exactly the
//! pair `vike_secrets::legacy_store_warning` fires on, because a pre-one-store leftover and a
//! systemd `EnvironmentFile` are the same file to anything that does not read it, and nothing here
//! reads it. So `vike-cli config check --strict` on a correctly installed recorder exits 1 with a
//! single `credential store` WARN.
//!
//! That is audit mode being literal, not a defect in the layout, and it is NOT a reason to move the
//! row: promoting it would refuse the recorder outright, demoting it to `Ok` would hide the real
//! upgrade case (credentials still sitting in the old `.env`). The other two runbooks do not trip it
//! — `docs/ops/tradehub-the CI box.md` creates an EMPTY `settings/secrets.env` (present, so the row does
//! not apply) and `docs/ops/datahub-the CI box.md` creates neither file. The units run the DEFAULT
//! disposition and pass in every case.
//!
//! # What this verb deliberately does NOT check
//!
//! `vike_config::refuse_credential_file_arming` — the refusal that stops an APPENDED line in the
//! credential store from arming real money. It is a genuine startup refusal, but only `vike-app`
//! performs it today, so folding it in here would make an `ExecStartPre=` refuse a `vike-tradehub`
//! that the daemon itself would start. That is a behaviour change to a deployment wearing a check's
//! clothes; the right fix is to give the daemon the call, not to give the pre-check a veto the
//! daemon does not have.
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
//! to carry neither (`vike_secrets::PermissionWarning`, `vike_secrets::LegacyStoreWarning`,
//! `vike_config::ConfigError`, whose own end-to-end gate is
//! `crates/vike-cli/tests/config_cli.rs`'s `a_rejected_unknown_key_never_echoes_its_value`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_secrets::{SECRETS_FILE, SETTINGS_DIR_ENV};

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
    fn phrase(self) -> String {
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
pub fn dir_origin(override_value: Option<&str>, resolved: Option<&Path>) -> DirOrigin {
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
    /// What was checked, as the operator names it (`policy.toml`, `credential store`).
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
/// (via `describe`) and `vike_secrets::resolve` — because a second parser would drift from the first
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
    let mut resolved_flags: Option<vike_config::Flags> = None;
    match vike_config::describe(settings_dir, env) {
        Ok(described) => {
            resolved_flags = Some(described.settings.flags);
            for file in &described.files {
                let detail = if file.present {
                    format!("{} — present, {} key(s) set", file.path.display(), file.keys)
                } else {
                    format!("{} — absent (compiled-in defaults)", file.path.display())
                };
                findings.push(finding(file.name, Level::Ok, detail));
            }
            // The loader's non-fatal resolutions. It returns them as DATA rather than logging
            // (`vike_config::Settings::warnings` argues why), so somebody has to surface them, and
            // a command whose entire job is judging this tree is exactly that somebody.
            for w in &described.settings.warnings {
                findings.push(finding("settings", Level::Warn, w.clone()));
            }
        }
        Err(e) => findings.push(finding("settings files", Level::Fail, e.to_string())),
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
/// Read through `vike_secrets::resolve` on a path this command NAMES, which is the shape
/// `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN` deliberately blesses
/// ("the composition root doing exactly what the rule asks for") rather than one of the sweeping
/// loaders that opens a location nothing in its signature mentions. It is also the only entry point
/// that can tell an UNREADABLE store from an absent one — the whole reason this verb exists.
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
    let path = dir.join(SECRETS_FILE);
    let resolved = match vike_secrets::resolve(&path) {
        Ok(r) => r,
        Err(e) => return vec![unreadable_store_finding(&e.to_string(), armed)],
    };

    let mut out = Vec::new();
    // KEY COUNT only. Names are `vike-cli secrets list`'s job; values are nobody's.
    out.push(match resolved.source {
        vike_secrets::Source::File(_) => finding(
            "credential store",
            Level::Ok,
            format!("{} — present, {} key(s)", path.display(), resolved.secrets.len()),
        ),
        vike_secrets::Source::None => finding(
            "credential store",
            Level::Ok,
            format!(
                "{} — absent. Absent credentials ARE the live gate: every venue stays paper, \
                 which is the designed state of an unconfigured box.",
                path.display()
            ),
        ),
    });
    if let Some(w) = &resolved.warning {
        out.push(finding("credential store", Level::Warn, w.to_string()));
    }
    if let Some(l) = &resolved.legacy {
        out.push(finding("credential store", Level::Warn, l.to_string()));
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
pub fn run(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A temp PROJECT root and its `settings/` child, both created.
    ///
    /// ⚠ Two levels, deliberately, not one bare temp directory.
    /// `vike_secrets::legacy_store_warning` probes the store's GRANDPARENT for a `<project>/.env`,
    /// so a bare temp directory aims that probe at the SYSTEM temp root — where a file somebody
    /// else left behind would flake this whole suite for reasons nobody could reproduce. Every path
    /// these tests depend on is one they created.
    fn project(tag: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "vike-cli-check-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let settings = root.join("settings");
        std::fs::create_dir_all(&settings).unwrap();
        (root, settings)
    }

    /// Write a credential store the way an operator is told to: OWNER-ONLY.
    ///
    /// ⚠ Not tidiness. `std::fs::write` lands at 0644 under the usual umask, which
    /// `vike_secrets::permission_warning` correctly reports — so a test that merely wanted "a store
    /// exists" would silently be exercising the exposed-mode WARNING instead, and
    /// `a_clean_tree_with_a_real_store_is_all_ok` would assert the opposite of its own name.
    fn write_store(settings: &Path, body: &str) {
        let path = settings.join(SECRETS_FILE);
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn find_of<'a>(report: &'a Report, subject: &str) -> &'a Finding {
        report
            .findings
            .iter()
            .find(|f| f.subject == subject)
            .unwrap_or_else(|| panic!("no `{subject}` finding in {report:#?}"))
    }

    fn level_of(report: &Report, subject: &str) -> Level {
        find_of(report, subject).level
    }

    // -- the origin rule -------------------------------------------------------------------------

    #[test]
    fn the_origin_words_are_pinned() {
        assert_eq!(DirOrigin::Named.as_str(), "named");
        assert_eq!(DirOrigin::Walk.as_str(), "walk");
        assert_eq!(DirOrigin::Unresolved.as_str(), "none");
    }

    #[test]
    fn an_unresolved_directory_reports_unresolved_whatever_the_override_says() {
        assert_eq!(dir_origin(None, None), DirOrigin::Unresolved);
        assert_eq!(dir_origin(Some("/x/settings"), None), DirOrigin::Unresolved);
    }

    /// **The blank-override rule is `vike_secrets::project_settings_dir_from`'s, and this holds the
    /// two to it.** A whitespace-only `VIKE_SETTINGS_DIR` falls THROUGH to the walk there — so
    /// calling it [`DirOrigin::Named`] here would report a rung that did not answer AND take the
    /// FAIL disposition on a directory nobody named. Driven through the REAL resolver, so the two
    /// cannot drift apart with only this file's opinion to notice.
    #[test]
    fn a_blank_override_is_the_walk_in_both_the_resolver_and_the_report() {
        let cwd = std::env::current_dir().unwrap();
        let walked = vike_secrets::project_settings_dir_from(None, &cwd);
        for blank in ["", "  ", "\t"] {
            assert_eq!(
                vike_secrets::project_settings_dir_from(Some(blank), &cwd),
                walked,
                "the resolver ignores a blank override"
            );
            assert_eq!(
                dir_origin(Some(blank), walked.as_deref()),
                if walked.is_some() { DirOrigin::Walk } else { DirOrigin::Unresolved },
                "…and so must the report"
            );
        }
        assert_eq!(
            dir_origin(Some(" /x/settings "), Some(Path::new("/x/settings"))),
            DirOrigin::Named
        );
    }

    // -- the disposition table -------------------------------------------------------------------

    #[test]
    fn a_clean_tree_with_a_real_store_is_all_ok() {
        let (root, settings) = project("clean");
        std::fs::write(settings.join("policy.toml"), "max_notional_per_order = 250\n").unwrap();
        write_store(&settings, "BINANCE_LIVE_API_KEY=k\n");

        let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
        assert_eq!(r.worst(), Level::Ok, "{r:#?}");
        assert!(!r.failed(false) && !r.failed(true));
        assert_eq!(level_of(&r, "policy.toml"), Level::Ok);
        assert_eq!(level_of(&r, "credential store"), Level::Ok);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The live gate.** An absent store is the designed state of an unconfigured box, so it is
    /// `Ok` and not even a warning — `crates/vike-secrets/src/store.rs`'s `resolve` documents that
    /// arm as an ANSWER and is deliberately silent about it. This is also the row that decides
    /// whether a shipped unit can run this verb at all: every shipped daemon is a PAPER deployment
    /// with an empty store, and a failure here would make a correct fresh install unstartable.
    #[test]
    fn an_absent_credential_store_is_not_even_a_warning() {
        let (root, settings) = project("nostore");
        let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
        assert_eq!(level_of(&r, "credential store"), Level::Ok, "{r:#?}");
        assert_eq!(r.worst(), Level::Ok);
        assert!(!r.failed(true), "not even --strict may fail a paper deployment");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// …but an absent store with the PRE-ONE-STORE `<project>/.env` sitting beside it is a WARNING,
    /// and the pair is what makes `--strict` mean something. It cannot be a refusal: a `.env` is
    /// also a legitimate systemd `EnvironmentFile` (the CI box's recorder ships one), which is exactly
    /// the argument `crates/vike-secrets/src/store.rs`'s `LegacyStoreWarning` makes.
    #[test]
    fn a_legacy_dotenv_beside_an_absent_store_warns_without_failing() {
        let (root, settings) = project("legacy");
        std::fs::write(root.join(".env"), "POLY_PROXY_ENABLED=false\n").unwrap();

        let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
        assert_eq!(r.worst(), Level::Warn, "{r:#?}");
        assert!(!r.failed(false), "a finding is never a refusal");
        assert!(r.failed(true), "…and --strict is the audience that wants it to be one");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// …and its opposite, which is the ONE row whose level is computed. On a box with nothing armed
    /// for live, a store that exists and cannot be opened is the DEGRADE
    /// `docs/decisions/0013-degrade-vs-refuse.md` records for the daemon itself — every venue was
    /// going to be paper anyway — so it warns and `--strict` counts it. Refusing here
    /// unconditionally would stop a working paper daemon over a file it never opens.
    ///
    /// A DIRECTORY where the file should be is the portable stand-in for an unreadable file (a
    /// `chmod 000` proves nothing when the test runs as root, which CI does).
    #[test]
    fn an_unreadable_credential_store_degrades_when_nothing_is_armed_for_live() {
        let (root, settings) = project("badstore");
        std::fs::create_dir_all(settings.join(SECRETS_FILE)).unwrap();
        let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
        assert_eq!(level_of(&r, "credential store"), Level::Warn, "{r:#?}");
        assert!(!r.failed(false), "a paper box must still start");
        assert!(r.failed(true), "…and --strict is the audience that wants it counted");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// …and the SAME store on a box ARMED FOR LIVE is a refusal: the operator asked for a real
    /// venue, the process cannot read the file that venue authenticates from, so it would place no
    /// orders while every surface reads live. ADR 0013 question 2 — set-but-unhonoured.
    ///
    /// ⚠ The variable is taken FROM `vike_config::CREDENTIAL_FILE_ARMING_REFUSED` rather than
    /// spelled, for the two reasons `a_removed_variable_is_a_failure_carrying_its_replacement`
    /// gives: it keys the assertion on the MECHANISM (whatever the arming table holds is what
    /// counts as live) instead of one venue's flag, and a bare env-shaped literal in a `src/` file
    /// is harvested as a READ by `crates/vike-ops/src/scan.rs`'s literal sweep, which would demand
    /// a `vike-cli` `SETTINGS` row for a variable this crate does not read.
    #[test]
    fn the_same_unreadable_store_fails_when_the_box_is_armed_for_live() {
        let (root, settings) = project("badstore-armed");
        std::fs::create_dir_all(settings.join(SECRETS_FILE)).unwrap();

        for s in vike_config::CREDENTIAL_FILE_ARMING_REFUSED {
            let r = inspect(Some(&settings), DirOrigin::Named, &map(&[(s.var, "1")]));
            let f = find_of(&r, "credential store");
            assert_eq!(f.level, Level::Fail, "{} must arm the refusal: {r:#?}", s.var);
            assert!(f.detail.contains(s.var), "the refusal must NAME what armed it: {f:?}");
            assert!(r.failed(false), "an armed box must refuse WITHOUT --strict");
        }
        // …and a DISARMING value is not armed, so the same tree degrades again — the grammar is
        // `vike_config::armed_settings_in`'s, not a second opinion held here.
        let disarmed = vike_config::CREDENTIAL_FILE_ARMING_REFUSED[0].var;
        for v in ["0", "", "true"] {
            let r = inspect(Some(&settings), DirOrigin::Named, &map(&[(disarmed, v)]));
            assert_eq!(level_of(&r, "credential store"), Level::Warn, "{v:?} arms nothing: {r:#?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The disposition itself, driven directly — no filesystem, every branch, and the property that
    /// makes the FAIL branch actionable: it names every arming source in one pass.
    #[test]
    fn the_unreadable_store_disposition_is_decided_by_the_arming_verdict_alone() {
        use vike_config::LiveArmingVerdict as V;
        assert_eq!(unreadable_store_finding("boom", &V::Unarmed).level, Level::Warn);

        let all = V::Armed(
            vike_config::CREDENTIAL_FILE_ARMING_REFUSED
                .iter()
                .map(vike_config::LiveArming::of_setting)
                .chain(std::iter::once(vike_config::TRADEHUB_LIVE_ARMING))
                .collect(),
        );
        let f = unreadable_store_finding("boom", &all);
        assert_eq!(f.level, Level::Fail);
        for s in vike_config::CREDENTIAL_FILE_ARMING_REFUSED {
            assert!(f.detail.contains(s.var), "every offender in ONE pass: {f:?}");
        }
        assert!(
            f.detail.contains(vike_config::TRADEHUB_LIVE_ARMING.source),
            "…including the NODE-scoped source, which is the one a switchless venue has: {f:?}"
        );
        assert!(f.detail.contains("boom"), "the store layer's own message survives: {f:?}");

        // ⚠ …and the third state. "Could not tell" refuses like an arm, and says WHY rather than
        // borrowing the armed wording, which would name a live venue nobody established.
        let u = unreadable_store_finding("boom", &V::Undetermined);
        assert_eq!(u.level, Level::Fail, "unknown must not degrade: {u:?}");
        assert!(u.detail.contains("could not be determined"), "{u:?}");
        assert!(!u.detail.contains("ARMED FOR LIVE ("), "it must not claim evidence: {u:?}");
    }

    /// **THE RESIDUAL THIS CHANGE CLOSES, at the disposition boundary.** A box armed ONLY by
    /// `flags.tradehub_live` — no `{VENUE}_MAINNET` anywhere, which is the shape of every box live
    /// on one of the nine SWITCHLESS venues — must FAIL on an unreadable store. Before the
    /// node-scoped source existed this exact tree took the WARN and started all-paper.
    ///
    /// Driven through the real `inspect` and the real loader, from a real `flags.toml`: the point is
    /// that the FILE reaches the disposition, not that a struct field can be set in a test.
    #[test]
    fn an_unreadable_store_fails_when_only_the_node_scoped_flag_arms_the_box() {
        let (root, settings) = project("badstore-node-armed");
        std::fs::create_dir_all(settings.join(SECRETS_FILE)).unwrap();
        std::fs::write(settings.join("flags.toml"), "tradehub_live = true\n").unwrap();

        let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
        let f = find_of(&r, "credential store");
        assert_eq!(f.level, Level::Fail, "a live NODE with an unreadable store refuses: {r:#?}");
        assert!(r.failed(false), "…without --strict");
        assert!(
            f.detail.contains("tradehub_live"),
            "the refusal must name the source, in the spelling the operator set: {f:?}"
        );
        // …and the process-env table genuinely saw NOTHING, so this is the new source answering
        // rather than an accidental `{VENUE}_MAINNET` leaking in from the test environment.
        assert!(vike_config::armed_settings_in(&map(&[])).is_empty());

        // ⚠ THE ADR 0013 GUARD: the SAME tree with the flag OFF must still warn and still start.
        // This is the paper box every shipped unit is, and the reason the unconditional FAIL was
        // narrowed in the first place — closing the hole must not reopen that one.
        std::fs::write(settings.join("flags.toml"), "tradehub_live = false\n").unwrap();
        let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
        assert_eq!(level_of(&r, "credential store"), Level::Warn, "{r:#?}");
        assert!(!r.failed(false), "a paper box must keep starting");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// …and the flag arms the disposition ONLY when the store is unreadable. An armed box whose
    /// store is fine is a correctly configured live node, not a finding — this verb detects a false
    /// belief, it does not object to going live.
    #[test]
    fn the_node_scoped_flag_is_not_itself_a_finding() {
        let (root, settings) = project("node-armed-ok");
        std::fs::write(settings.join("flags.toml"), "tradehub_live = true\n").unwrap();
        write_store(&settings, "BYBIT_DEMO_API_KEY=k\n");

        let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
        assert_eq!(r.worst(), Level::Ok, "arming is a configuration, not a defect: {r:#?}");
        assert!(!r.failed(true), "not even --strict");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A broken settings file is the same refusal the daemon performs, and the message names the
    /// file — a check that passed here would contradict the binary it is checking.
    #[test]
    fn a_broken_settings_file_fails_and_names_the_file() {
        let (root, settings) = project("brokenfile");
        std::fs::write(settings.join("policy.toml"), "max_leverage = \"not a number\"\n").unwrap();
        let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
        let f = find_of(&r, "settings files");
        assert_eq!(f.level, Level::Fail, "{r:#?}");
        assert!(f.detail.contains("policy.toml"), "{f:?}");
        assert!(r.failed(false));
        // The whole tree is ONE finding on this path, not four: `vike_config::load` refuses at the
        // FIRST offending file exactly as a daemon does, so there are no per-file rows to report.
        assert!(!r.findings.iter().any(|f| f.subject == "policy.toml"), "{r:#?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **Set-but-unhonoured is a REFUSAL** (ADR 0013 question 2): the operator named the directory,
    /// it is not there, and everything downstream degrades to defaults with no error anywhere.
    #[test]
    fn a_named_settings_directory_that_is_not_there_fails() {
        let (root, settings) = project("named-missing");
        std::fs::remove_dir_all(&settings).unwrap();

        let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
        let f = find_of(&r, "settings directory");
        assert_eq!(f.level, Level::Fail, "{r:#?}");
        assert!(r.failed(false));
        assert!(
            f.detail.contains(SETTINGS_DIR_ENV),
            "the refusal must name the variable that promised it: {f:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// …and the SAME missing directory reached by the WALK is only a warning. Nobody claimed it was
    /// there: a checkout that has never configured anything is the ordinary state of this repo.
    #[test]
    fn the_same_directory_reached_by_the_walk_is_only_a_warning() {
        let (root, settings) = project("walk-missing");
        std::fs::remove_dir_all(&settings).unwrap();

        let r = inspect(Some(&settings), DirOrigin::Walk, &map(&[]));
        assert_eq!(level_of(&r, "settings directory"), Level::Warn, "{r:#?}");
        assert!(!r.failed(false), "an unconfigured checkout is not a failure");
        assert!(r.failed(true), "…but --strict is exactly the audience that wants it to be");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// No project at all: a warning, and no credential-store row — the directory row already said
    /// every venue stays paper, and a second warning for the same fact teaches people to scroll.
    #[test]
    fn no_settings_directory_warns_once() {
        let r = inspect(None, DirOrigin::Unresolved, &map(&[]));
        assert_eq!(level_of(&r, "settings directory"), Level::Warn, "{r:#?}");
        assert!(!r.findings.iter().any(|f| f.subject == "credential store"), "{r:#?}");
        assert!(!r.failed(false));
        assert!(r.failed(true));
    }

    /// The removed-variable row. ⚠ Unreachable through the shipped binary — `crate::run` refuses
    /// first — so it is driven directly here, and the END-TO-END refusal is gated in
    /// `crates/vike-cli/tests/config_check_cli.rs`.
    ///
    /// ⚠ The variable is taken FROM the real table rather than spelled, for two reasons. It keys the
    /// assertion on the MECHANISM (whatever is removed is refused, and the refusal carries the line
    /// to paste) rather than on one name that a later phase will retire — and a bare `VIKE_*`
    /// literal in a `src/` file is harvested as a READ by `crates/vike-ops/src/scan.rs`'s literal
    /// sweep, which would demand a `vike-cli` `SETTINGS` row for a variable this crate does not read.
    #[test]
    fn a_removed_variable_is_a_failure_carrying_its_replacement() {
        let removed = vike_config::REMOVED_ENV
            .iter()
            .find(|r| r.key.is_some() && r.echo_value)
            .expect("a removed variable whose value moved to a named key");

        let r = inspect(None, DirOrigin::Unresolved, &map(&[(removed.var, "250")]));
        assert_eq!(level_of(&r, "removed environment"), Level::Fail, "{r:#?}");
        let detail = &r.findings[0].detail;
        assert!(detail.contains(removed.file), "the file that replaces it: {detail}");
        assert!(detail.contains(removed.key.unwrap()), "…and the key: {detail}");
        assert!(detail.contains("250"), "…with the operator's own value pasted in: {detail}");
    }

    // -- the exit-code rule ------------------------------------------------------------------------

    #[test]
    fn strict_promotes_warnings_and_nothing_promotes_an_ok() {
        let ok = Report {
            settings_dir: None,
            origin: DirOrigin::Unresolved,
            findings: vec![finding("a", Level::Ok, "x")],
        };
        assert!(!ok.failed(false) && !ok.failed(true));

        let warn = Report { findings: vec![finding("a", Level::Warn, "x")], ..ok.clone() };
        assert!(!warn.failed(false));
        assert!(warn.failed(true));

        let fail = Report { findings: vec![finding("a", Level::Fail, "x")], ..ok.clone() };
        assert!(fail.failed(false) && fail.failed(true));
    }

    #[test]
    fn the_level_words_are_pinned() {
        assert_eq!(Level::Ok.as_str(), "ok");
        assert_eq!(Level::Warn.as_str(), "warn");
        assert_eq!(Level::Fail.as_str(), "fail");
        assert!(Level::Ok < Level::Warn && Level::Warn < Level::Fail, "worst() depends on this");
    }

    // -- redaction ---------------------------------------------------------------------------------

    /// No credential VALUE reaches a finding, whatever the store holds — the store is disclosed by
    /// COUNT, and no key NAME is printed either.
    #[test]
    fn no_finding_carries_a_credential() {
        const LEAK: &str = "sk-do-not-print-me";
        let (root, settings) = project("redact");
        write_store(
            &settings,
            &format!("BINANCE_LIVE_API_KEY={LEAK}\nOKX_DEMO_API_PASSPHRASE={LEAK}\n"),
        );

        let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
        let rendered = format!("{r:#?}");
        assert!(!rendered.contains(LEAK), "a credential VALUE leaked: {rendered}");
        assert!(!rendered.contains("BINANCE_LIVE_API_KEY"), "a credential NAME leaked: {rendered}");
        assert!(rendered.contains("2 key(s)"), "…and the count is what IS disclosed: {rendered}");
        // …and the same property on the SERIALIZED document, which is what a monitor scrapes.
        let json = serde_json::to_string(&report_json(&r, false)).unwrap();
        assert!(!json.contains(LEAK) && !json.contains("BINANCE_LIVE_API_KEY"), "{json}");
        let _ = std::fs::remove_dir_all(&root);
    }

    // -- arg parsing + the printers ----------------------------------------------------------------

    #[test]
    fn flags_parse_and_default_to_the_human_non_strict_view() {
        assert_eq!(parse_args(std::iter::empty()).unwrap(), Args::default());
        let a = parse_args(["--json", "--strict"].map(String::from).into_iter()).unwrap();
        assert!(a.json && a.strict);
    }

    #[test]
    fn a_bad_flag_is_a_clean_error() {
        assert!(parse_args(["--nope".to_string()].into_iter()).unwrap_err().contains("--nope"));
        assert!(
            parse_args(["--json=1".to_string()].into_iter())
                .unwrap_err()
                .contains("takes no value")
        );
        assert_eq!(parse_args(["-h".to_string()].into_iter()).unwrap_err(), "help requested");
    }

    #[test]
    fn usage_documents_both_flags_and_the_exit_contract() {
        for needle in ["usage:", "--json", "--strict", "exit 0", "exit 1"] {
            assert!(USAGE.contains(needle), "USAGE must mention {needle}");
        }
    }

    #[test]
    fn both_printers_render_every_level_without_panicking() {
        let r = Report {
            settings_dir: Some(PathBuf::from("/srv/x/settings")),
            origin: DirOrigin::Named,
            findings: vec![
                finding("ok row", Level::Ok, "fine"),
                finding("warn row", Level::Warn, "a degrade"),
                // a MULTI-LINE detail, the shape `refuse_removed_env` returns
                finding("fail row", Level::Fail, "first line\nsecond line\n"),
            ],
        };
        print_human(&r, false);
        print_human(&r, true);
        let doc = report_json(&r, false);
        for field in [
            "settings_dir",
            "settings_dir_origin",
            "strict",
            "ok",
            "failures",
            "warnings",
            "findings",
        ] {
            assert!(doc.get(field).is_some(), "missing {field}");
        }
        assert_eq!(doc["ok"], serde_json::Value::Bool(false));
        assert_eq!(doc["failures"], serde_json::json!(1));
        assert_eq!(doc["warnings"], serde_json::json!(1));
        // …and the empty-directory header path.
        print_human(
            &Report { settings_dir: None, origin: DirOrigin::Unresolved, findings: vec![] },
            false,
        );
    }
}
