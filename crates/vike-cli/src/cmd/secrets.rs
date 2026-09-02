//! `vike-cli secrets` — inspect the credential store.
//!
//! ```text
//! vike-cli secrets list     print the KEY NAMES held in the store, the ACCOUNTS they resolve
//!                           to, and which file that is
//! vike-cli secrets path     print the store's path, whether it exists, and its exposure
//! ```
//!
//! **Both subcommands report an over-permissive mode.** `list` always did, because it opens the
//! store and `vike_secrets::resolve` hands the finding back with the credentials; `path` never did,
//! because it opens nothing — so the command documented as the safe FIRST one, and the one the
//! README and the ops runbook name first, was the one that stayed silent about a 0666 credential
//! file. It now asks the same question through `vike_secrets::permission_warning`, which `stat`s the
//! file without reading a byte of it, so "opens nothing" still holds.
//!
//! # One store
//!
//! ```text
//! <project>/settings/secrets.env
//! ```
//!
//! `path` reports where that resolved to for THIS invocation, which is the question an operator
//! must be able to answer without reading source: the directory is found by walking up from the
//! working directory, and `VIKE_SETTINGS_DIR` names it outright, so "which file am I editing?" has
//! an answer that depends on where you are standing. Both subcommands take the SAME resolved
//! directory the dispatcher hands every other subcommand, so this output cannot drift from what a
//! daemon loads.
//!
//! ⚠ **"The same resolved directory" is two values, not one** — the directory the boot resolved AND
//! the `$VIKE_SETTINGS_DIR` value it honoured. A boot USED to return `None` for the first while
//! still holding the second, and passing only the first (then falling back to an override-blind
//! resolver) is exactly how this command came to print a file nothing reads. That upstream cause is
//! fixed — `vike_boot::boot` honours a name with no walk — so the second value is now a rung LABEL
//! here rather than a fallback, and the fallback arm it feeds is unreachable and deliberately kept.
//! `store_path` is where that whole argument lives, with the input that used to reach it.
//!
//! # Read-only, always
//!
//! No subcommand writes anything, and there is deliberately no subcommand that does. The store
//! is the user's only copy of live venue credentials; creating, moving or rewriting it is their
//! decision, made with an editor. `path` prints the location so they can.
//!
//! ⚠ **`template` does not break that rule, and the shape is the reason.** It writes the key GRID
//! to **stdout** and takes no destination argument, so putting it in a file is a redirection the
//! operator types — `vike-cli secrets template > settings/secrets.env`. A `--out PATH` flag was
//! deliberately NOT added: the moment this command owns a path it can truncate a live store, which
//! is the one thing this workspace never does. The shell's `>` can too, but that is the operator's
//! own keystroke against their own path, not a behaviour of ours.
//!
//! `list` prints key NAMES and never a value — that output is routinely pasted into an issue.
//! `template` prints names with EMPTY values for the same reason: it is a shape, and a shape is
//! safe to paste.
//!
//! # Why this command lives in `vike-cli` and not in a bridge crate
//!
//! `vike-secrets` has ZERO dependencies and no transport stack, so linking it here costs this
//! DataFusion-free, fast-lane CLI nothing. Routing through `vike-bridge-core` would have dragged
//! `ureq`/`tungstenite`/`rustls` into the binary for a `KEY=VALUE` parser.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_model::account_keys::accounts_in_store;
use vike_secrets::{resolve, Source, SECRETS_FILE};

use crate::cmd::args::{exit_for_parse_error, help_requested, Flags};

const USAGE: &str = "\
usage: vike-cli secrets <subcommand> [options]

subcommands:
  list      print the KEY NAMES held in the store (never the values), and the
            ACCOUNTS those names resolve to — a `KEY__LABEL` names an account,
            a `KEY_LABEL` does not and would be read by nothing
  path      print the store's path, whether it exists, and whether its mode
            exposes it beyond its owner (both subcommands warn about that)
  template  print an EMPTY credential grid — every key name this workspace can
            look up, with no values — to stdout. Redirect it yourself:
              vike-cli secrets template > settings/secrets.env
            ⚠ that redirection TRUNCATES an existing store; this command has no
            --out flag on purpose, so it can never do that on its own

options:
  --file PATH     inspect this store instead of the project's (list, path)
  --venue ID      template: emit only this venue's rows
  -h, --help      this message

the store is <project>/settings/secrets.env; $VIKE_SETTINGS_DIR names that directory outright";

#[derive(Debug, PartialEq, Eq)]
enum Sub {
    List,
    Path,
    Template,
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    sub: Sub,
    file: Option<PathBuf>,
    /// `template --venue ID` — emit one venue's rows instead of the whole grid. Validated against
    /// [`vike_model::venues::VENUES`] at RUN time rather than parse time, so the error can name the
    /// roster; parsing stays pure and total.
    venue: Option<String>,
}

/// Parse `secrets`' own argv tail (everything after the subcommand name). PURE — no I/O, so the
/// whole grammar is unit-tested below.
fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let Some(first) = it.next() else {
        return Err("a subcommand is required (list | path)".to_string());
    };
    let sub = match first.as_str() {
        "list" => Sub::List,
        "path" => Sub::Path,
        "template" => Sub::Template,
        "-h" | "--help" | "help" => return help_requested(),
        other => return Err(format!("unknown `secrets` subcommand '{other}'")),
    };
    let mut file = None;
    let mut venue = None;
    let mut flags = Flags::new(it);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--file" => file = Some(PathBuf::from(flags.value(&flag, inline)?)),
            "--venue" => venue = Some(flags.value(&flag, inline)?),
            "-h" | "--help" => return help_requested(),
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    // `--venue` is meaningless to the two INSPECTING subcommands, and silently ignoring a flag the
    // operator typed is how a person comes to believe they filtered something. `--file` stays
    // permitted everywhere: it is inert for `template`, which opens nothing, but refusing it would
    // break `--file X list`-style habits for no safety gain.
    if venue.is_some() && sub != Sub::Template {
        return Err("--venue applies to `template` only".to_string());
    }
    Ok(Args { sub, file, venue })
}

/// The store this invocation inspects: `--file` if given, else the store inside the settings
/// directory the DISPATCHER resolved, else the walk from the working directory — under the SAME
/// `$VIKE_SETTINGS_DIR` value that dispatcher's boot was handed.
///
/// PURE — no I/O, so the whole override grammar is unit-tested. BOTH `settings_dir` and
/// `settings_dir_override` come from `crate::run`'s single environment sweep, not from a read of
/// this library file's own.
///
/// ⚠ **The last arm takes the override, and the override-BLIND
/// `vike_secrets::workspace_dotenv_path` it used to call is wrong there.** The tempting argument is
/// that this arm runs only when the dispatcher's boot resolved NO settings directory, and that a
/// boot resolving nothing means there was no override to honour — so the blind spelling is the same
/// pure resolver under the same `None` and cannot answer differently. **That was false**, because
/// `vike_boot::boot` resolved the directory as `spec.cwd.and_then(..)`: with no readable working
/// directory it yielded `None` *while still returning the override it was given*. `std::env::
/// current_dir()` fails whenever the directory a process started in has been removed, unmounted or
/// made unsearchable, so the reachable input was `$VIKE_SETTINGS_DIR=/srv/x/settings` plus a
/// vanished working directory — and there the two spellings diverge:
/// `vike_secrets::workspace_dotenv_path` falls through to the RELATIVE last resort
/// `settings/secrets.env` while every daemon on that box reads `/srv/x/settings/secrets.env`
/// (`vike_bridge_core::credentials::load_workspace_secrets_from_env` ->
/// `vike_secrets::resolve_project` -> `vike_secrets::workspace_dotenv_path_from`, all of which
/// carry the override).
///
/// Printing a location the rest of the program does not use is the one failure this command cannot
/// have: `path` exists to answer *which file are my keys actually coming from*, and it is the
/// command an operator runs when something is already wrong. `vike_secrets::dotenv_path_for` is
/// where that divergence is pinned.
///
/// # ⚠ The UPSTREAM cause is fixed, so this arm is now unreachable — and it stays
///
/// `vike_boot::boot` calls `vike_secrets::project_settings_dir_for`, which honours a name with no
/// walk, so a boot that returns `settings_dir: None` now necessarily returns
/// `settings_dir_override: None` as well. **This function's `settings_dir_override` parameter can
/// therefore only ever arrive as `None` from `crate::run`** — which makes the last arm
/// byte-identical to the blind spelling it replaced, on every input this dispatcher can produce.
///
/// It is KEPT, and that is a decision rather than an oversight. Three reasons, in order:
///
/// 1. **It is the belt.** The upstream fix is one expression in another crate. If it regresses to
///    an `and_then` on the working directory, this arm is what keeps `secrets path` naming the file
///    the daemons read, instead of quietly printing a relative last resort again.
/// 2. **The function is still CORRECT for the pairing**, and it is unit-tested for it directly
///    (`the_store_honours_the_override_when_no_settings_dir_was_resolved` calls it with synthetic
///    values, so it does not depend on the boot to reach that input at all).
/// 3. Deleting it would cost a parameter and buy nothing: the argument is a `Option<&str>` the
///    dispatcher already holds for `config check`'s origin verdict.
///
/// `a_boot_with_no_working_directory_resolves_the_named_directory` below is where the reachability
/// is measured — it now asserts the pairing is GONE, which is the assertion that would go red the
/// day the boot starts dropping names again.
fn store_path(
    args: &Args,
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> PathBuf {
    if let Some(p) = &args.file {
        return p.clone();
    }
    match settings_dir {
        Some(d) => d.join(SECRETS_FILE),
        None => vike_secrets::workspace_dotenv_path_from(settings_dir_override),
    }
}

/// Run the subcommand. Returns the process exit code.
///
/// `settings_dir` is `<project>/settings`, resolved once by the dispatcher, and
/// `settings_dir_override` is the `$VIKE_SETTINGS_DIR` value that same boot honoured — BOTH are
/// needed, and `store_path` carries the argument for why the second one is not redundant.
///
/// ⚠ This command already separated the help short-circuit from a usage error and already exited
/// 0 for it — but it printed the help with `eprintln!`, so `vike-cli secrets --help | less` showed
/// an empty page. Routing through [`crate::cmd::args::exit_for_parse_error`] keeps the exit codes
/// exactly as they were and moves the help text to the stream a user is piping.
pub fn run(
    args: impl Iterator<Item = String>,
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> ExitCode {
    let args = match parse(args) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("secrets", USAGE, &msg),
    };
    let outcome = match args.sub {
        Sub::List => run_list(&args, settings_dir, settings_dir_override),
        Sub::Path => run_path(&args, settings_dir, settings_dir_override),
        Sub::Template => run_template(args.venue.as_deref()),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("vike-cli secrets: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// `list` — the key NAMES in the store. Never a value.
///
/// The header names the FILE before the keys, because "where did these come from" is the question
/// the list itself cannot answer.
///
/// # …and the ACCOUNTS those names resolve to
///
/// A flat key list cannot answer the one question a SECOND account per venue raises: *was my
/// labelled key understood as an account, or is it just a string in a file?* Both spellings look
/// identical to a reader —
///
/// ```text
/// HYPERLIQUID_LIVE_API_KEY__ALT     an account named ALT
/// HYPERLIQUID_LIVE_API_KEY_ALT      one underscore, and NOT an account
/// ```
///
/// — and both appear in the list above with nothing to tell them apart. The second is a key nothing
/// will ever read: the grammar splits at a DOUBLE underscore
/// ([`vike_model::account_keys::ACCOUNT_SEPARATOR`]), so a single one leaves the whole string as one
/// base name belonging to the default account, and an operator who typed it would have added a
/// credential that is silently inert.
///
/// So the accounts are printed DERIVED from the same key names, through
/// [`vike_model::account_keys::accounts_in_store`] — the enumeration the grammar itself defines.
/// A labelled key that made it into an account row is a labelled key
/// `vike_bridge_core::credentials::load_credentials_for_account` can read.
///
/// ⚠ **This is a CREDENTIAL-STORE disclosure and says nothing about ARMING.** An account listed
/// here is an account whose credentials exist; whether it mounts is a policy question, and today
/// the answer is that no labelled account mounts at all (`policy.toml`'s `[accounts]` table is
/// parsed, validated and folded by nothing — `vike_config::load` warns about exactly that). The
/// heading says "in the store" for that reason, and must keep saying something like it.
///
/// ⚠ Names the grammar recognises as no credential at all contribute no row and that is a
/// CLASSIFICATION rather than a gap — attribution codes, the `POLY_*` L2 trio, dukascopy's numbered
/// sub-account, aster's `TESTNET` tier. [`vike_model::account_keys::account_ref_from_key`] is the
/// authority on which and why. So the account count is deliberately NOT a count of the venues an
/// operator has configured, and nothing here claims it is.
fn run_list(
    args: &Args,
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> Result<(), String> {
    let resolved = resolve(&store_path(args, settings_dir, settings_dir_override))
        .map_err(|e| e.to_string())?;
    if let Some(w) = &resolved.warning {
        eprintln!("⚠ {w}");
    }
    // Set only when the store is ABSENT: `no store found — every venue stays paper` is the right
    // answer for a fresh install and a badly misleading one for an upgrade whose `.env` never moved.
    if let Some(w) = &resolved.legacy {
        eprintln!("⚠ {w}");
    }
    println!("source: {}", describe(&resolved.source));
    println!("{} secret(s):", resolved.secrets.len());
    for k in resolved.secrets.keys() {
        println!("  {k}");
    }
    let accounts = accounts_in_store(resolved.secrets.keys());
    println!("{} account(s) in the store:", accounts.len());
    for a in &accounts {
        println!("  {}", describe_account(a));
    }
    Ok(())
}

/// One account row: `venue/TIER label`, with the unlabelled account rendered as `(default)`.
///
/// ⚠ NOT [`vike_model::account_keys::AccountLabel`]'s own `Display`, which renders the default
/// account as the bare word `DEFAULT`. That spelling is the one
/// [`vike_model::account_keys::AccountLabel::parse`] REFUSES, so printing it in a column an
/// operator will copy into `policy.toml`'s `[accounts]` table would hand them a line the loader
/// rejects by name. The parentheses are what say "this is a description, not a label".
fn describe_account(account: &vike_model::account_keys::AccountRef) -> String {
    let label = account.label.text().unwrap_or("(default)");
    format!("{}/{} {label}", account.venue, account.tier)
}

/// **What a probe of the store's path can honestly answer — and why that is THREE states.**
///
/// [`Path::exists`] has only two, because it maps EVERY error to `false`: `EACCES` on a directory
/// in the path, `ENOTDIR`, `ELOOP`, an I/O error on the filesystem. So a project root this process
/// cannot search reported the store as `absent`, and [`run_path`] then printed the fresh-install
/// answer — *"nothing here reads any other location — create it"* — for a permissions problem. Every
/// venue does drop to paper either way, which is exactly what makes the two indistinguishable
/// downstream and exactly why they must not read the same here.
///
/// That is the conflation [`vike_secrets::legacy_store_warning`] already refuses one layer down
/// (*only `NotFound` counts as absent; any other error means absence could not be ESTABLISHED*) and
/// the one [`vike_secrets::resolve`]'s unreadable arm exists for. [`Path::try_exists`] is the same
/// probe without the swallowing: `Ok(false)` is `NotFound` and nothing else.
#[derive(Debug)]
enum Presence {
    /// `stat` answered: the store is there.
    Present,
    /// `NotFound` — the ordinary unconfigured state, and the ONLY established absence.
    Absent,
    /// The probe failed for some other reason. Absence was not established, so nothing printed here
    /// may say "absent".
    Undetermined(std::io::Error),
}

impl Presence {
    /// The parenthesised state on `path`'s first line. Deliberately shares no word with the other
    /// two arms — an operator greps this line, and "absent" appearing in an undetermined answer
    /// would hand back the very conflation this type exists to break.
    fn label(&self) -> String {
        match self {
            Presence::Present => "present".to_string(),
            Presence::Absent => "absent".to_string(),
            Presence::Undetermined(e) => format!("could not be determined: {e}"),
        }
    }
}

/// Probe `path` without swallowing the reason. See [`Presence`].
fn presence(path: &Path) -> Presence {
    match path.try_exists() {
        Ok(true) => Presence::Present,
        Ok(false) => Presence::Absent,
        Err(e) => Presence::Undetermined(e),
    }
}

/// `path` — where the store is, and whether it is there. Opens nothing, so it is the safe first
/// command when something is misconfigured.
///
/// ⚠ It also reports the store's PERMISSIONS and whether the path is a SYMLINK, and that is not a
/// contradiction of "opens nothing": [`vike_secrets::permission_warning`] `lstat`s the file, reads
/// `st_mode` and follows the link only far enough to `readlink` + `stat` it, never its contents, so
/// no credential value enters this process. It has to be here rather than only in `list`, because
/// this is the command the README and the ops runbook name FIRST — the one an operator runs before
/// they know anything is wrong. Measured on a clean install at modes 600/640/644/664/666, `path`
/// printed ZERO warnings at every one while `list` warned from 640 up: the safe first command was
/// the one that stayed quiet about a world-writable credential file.
///
/// ⚠ **"Whether it is there" has THREE answers, not two** — see [`Presence`]. The undetermined one
/// is a FINDING and not a refusal, the same disposition every other finding on this command has:
/// `path` is what an operator runs when something is already broken, so the command that reports
/// the trouble must not become another thing that failed. It prints the path, says it could not
/// answer, and exits 0.
fn run_path(
    args: &Args,
    settings_dir: Option<&Path>,
    settings_dir_override: Option<&str>,
) -> Result<(), String> {
    let p = store_path(args, settings_dir, settings_dir_override);
    let state = presence(&p);
    println!("store:  {} ({})", p.display(), state.label());
    // Same stream and same shape as `list`'s: stderr, `⚠`, the store's own `Display`. A finding is
    // never a refusal (see `vike_secrets::PermissionWarning`), so this changes no exit code.
    if let Some(w) = vike_secrets::permission_warning(&p) {
        eprintln!("⚠ {w}");
    }
    match state {
        Presence::Present => {}
        Presence::Absent => {
            // ⚠ Ask ONLY here. `nothing here reads any other location` below is true and was never
            // checked: measured on a checkout with the pre-one-store `.env` still beside
            // `Cargo.toml`, this command printed that line and said nothing about the file the
            // operator believed was being read. A `.env` beside a store that EXISTS is a systemd
            // `EnvironmentFile` and is not a finding — see `vike_secrets::legacy_store_warning`.
            if let Some(w) = vike_secrets::legacy_store_warning(&p) {
                eprintln!("⚠ {w}");
            }
            println!();
            println!("nothing here reads any other location — create it to configure a venue:");
            println!("  mkdir -p {}", p.parent().unwrap_or(Path::new(".")).display());
            println!("  $EDITOR {}", p.display());
            println!("  chmod 600 {}   # it is plaintext credentials", p.display());
        }
        // Neither branch above is honest here: the create-one hint would advise creating a file
        // that may already exist, and silence would leave the `absent` reading standing. Say what
        // was not established, and name the answer this is NOT — spelled by `describe` rather than
        // copied, so the two cannot drift.
        Presence::Undetermined(e) => eprintln!(
            "⚠ whether the credential store {} is there could not be determined: {e} — only \
             NotFound establishes absence, so this is NOT `{}`. A store that is PRESENT and \
             unreachable leaves every venue on paper exactly as an absent one does, and it is a \
             different problem with a different fix: check the permissions of each directory on \
             that path. Nothing has been created, moved or deleted.",
            p.display(),
            describe(&Source::None)
        ),
    }
    Ok(())
}

fn describe(source: &Source) -> String {
    match source {
        Source::File(p) => format!("the project's credential store {}", p.display()),
        Source::None => "no store found — every venue stays paper".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_of(args: &[&str]) -> Result<Args, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn each_subcommand_parses() {
        assert_eq!(parse_of(&["list"]).unwrap().sub, Sub::List);
        assert_eq!(parse_of(&["path"]).unwrap().sub, Sub::Path);
    }

    #[test]
    fn options_parse_in_both_flag_forms() {
        assert_eq!(
            parse_of(&["list", "--file", "/tmp/a.env"]).unwrap().file,
            Some(PathBuf::from("/tmp/a.env"))
        );
        assert_eq!(
            parse_of(&["path", "--file=/tmp/b.env"]).unwrap().file,
            Some(PathBuf::from("/tmp/b.env"))
        );
    }

    #[test]
    fn defaults_are_none_so_the_runner_resolves_them() {
        assert_eq!(parse_of(&["list"]).unwrap(), Args { sub: Sub::List, file: None, venue: None });
    }

    #[test]
    fn usage_errors_are_clean() {
        assert!(parse_of(&[]).unwrap_err().contains("subcommand is required"));
        assert!(parse_of(&["frobnicate"]).unwrap_err().contains("unknown `secrets` subcommand"));
        assert!(parse_of(&["list", "--nope"]).unwrap_err().contains("unknown option"));
        assert!(parse_of(&["list", "--file"]).unwrap_err().contains("requires a value"));
    }

    #[test]
    fn help_short_circuits_at_both_levels() {
        assert_eq!(parse_of(&["--help"]).unwrap_err(), "help requested");
        assert_eq!(parse_of(&["list", "-h"]).unwrap_err(), "help requested");
    }

    fn args_with(file: Option<&str>) -> Args {
        Args { sub: Sub::List, file: file.map(PathBuf::from), venue: None }
    }

    /// **The store is the settings directory's `secrets.env`, and nothing resolves a second one.**
    ///
    /// `--file` outranks it so an operator can inspect a store directly; with neither, the walk from
    /// the working directory answers — the same resolver `load_workspace_dotenv` takes, so the two
    /// cannot disagree about what a daemon on this box would read.
    ///
    /// With NO override in hand the last arm is byte-identical to the blind spelling it replaced,
    /// which is why that substitution changed nothing for the ordinary checkout. The arm where it
    /// is NOT identical has its own test below.
    #[test]
    fn the_store_is_secrets_env_inside_the_dispatchers_settings_dir() {
        let settings = Path::new("/opt/vike/settings");
        assert_eq!(
            store_path(&args_with(None), Some(settings), None),
            settings.join("secrets.env")
        );
        assert_eq!(
            store_path(&args_with(Some("/tmp/explicit.env")), Some(settings), None),
            PathBuf::from("/tmp/explicit.env")
        );
        assert_eq!(store_path(&args_with(None), None, None), vike_secrets::workspace_dotenv_path());
        assert_eq!(
            store_path(&args_with(Some("/tmp/explicit.env")), None, None),
            PathBuf::from("/tmp/explicit.env")
        );
    }

    /// **Given a `None` directory beside a `Some` override, [`store_path`] resolves the NAMED
    /// store — and this is the test that would go red the day somebody spells the blind resolver
    /// here again.**
    ///
    /// The claim it refutes is a reasonable-sounding one: *the last arm of [`store_path`] runs only
    /// when the dispatcher's boot found nothing, and in that case
    /// `vike_secrets::workspace_dotenv_path` is the same pure resolver under the same `None`, so it
    /// cannot answer differently.* It cannot — the two resolvers genuinely diverge on that input,
    /// which is what this pins.
    ///
    /// What the defect COST, when the pairing was reachable: on such a box every daemon read the
    /// named store (`load_workspace_secrets_from_env` -> `resolve_project` ->
    /// `workspace_dotenv_path_from`, the override carried the whole way), while `vike-cli secrets
    /// path` printed the relative last resort `settings/secrets.env`. That is the one answer this
    /// command may not get wrong — the operator running it is already looking for a
    /// misconfiguration, and it would point them at a file nothing on the box reads.
    ///
    /// ⚠ **`crate::run` can no longer PRODUCE this pairing**, because `vike_boot::boot` honours a
    /// name with no walk (`vike_secrets::project_settings_dir_for`). The inputs here are therefore
    /// synthetic ON PURPOSE: this is a unit test of [`store_path`]'s own contract, so it keeps
    /// covering the fallback arm whether or not any caller can reach it, and it is deliberately not
    /// deleted along with the reachability —
    /// `a_boot_with_no_working_directory_resolves_the_named_directory` is the pin on the upstream
    /// half, and if that one regresses this one is what still holds the behaviour.
    ///
    /// The `assert_ne!` is deliberate and load-bearing: the two `assert_eq!`s above it pass under
    /// the blind spelling too whenever the CHECKOUT the test runs in happens to walk to a matching
    /// path, and only the inequality states the property that actually broke.
    #[test]
    fn the_store_honours_the_override_when_no_settings_dir_was_resolved() {
        let named = "/srv/vike-<unit>/settings";

        // What every OTHER credential reader on that box resolves, override in hand.
        let daemon_reads = vike_secrets::workspace_dotenv_path_from(Some(named));
        assert_eq!(daemon_reads, Path::new(named).join(SECRETS_FILE));

        assert_eq!(
            store_path(&args_with(None), None, Some(named)),
            daemon_reads,
            "`secrets path` must name the file the rest of the program opens"
        );
        assert_ne!(
            vike_secrets::workspace_dotenv_path(),
            daemon_reads,
            "the override-BLIND spelling cannot produce the named store — that is the defect"
        );

        // `--file` still outranks everything, override or no override.
        assert_eq!(
            store_path(&args_with(Some("/tmp/explicit.env")), None, Some(named)),
            PathBuf::from("/tmp/explicit.env")
        );
        // …and a settings directory that WAS resolved is still what answers: the override rung is a
        // fallback, never a second opinion about a directory the boot already named.
        let settings = Path::new("/opt/vike/settings");
        assert_eq!(
            store_path(&args_with(None), Some(settings), Some(named)),
            settings.join(SECRETS_FILE)
        );
    }

    /// **The reachability half — and the direction it measures has FLIPPED, which is the finding.**
    ///
    /// It used to assert that this dispatcher really does hand [`store_path`] a `None` directory
    /// beside a `Some` override, because `vike_boot::boot` resolved the directory as
    /// `spec.cwd.and_then(..)` and so dropped a name that needed no walk. #1514 taught this command
    /// to survive that pairing; the ROOT CAUSE is now fixed in `vike_boot::boot`, which calls
    /// `vike_secrets::project_settings_dir_for` — so the pairing no longer exists and the FIRST
    /// assertion below is the one that would go red if it came back.
    ///
    /// The test is kept rather than deleted precisely because of that: it is the pin on the upstream
    /// behaviour this command's fallback rung was written for, and a regression there is silent —
    /// every daemon on the box keeps reading the named store while `secrets path` starts printing a
    /// relative last resort, with nothing failing anywhere. `store_path`'s own unit test
    /// (`the_store_honours_the_override_when_no_settings_dir_was_resolved`) still drives the
    /// synthetic pairing directly, so the fallback stays covered whether or not a boot can produce
    /// it.
    ///
    /// It runs the REAL `vike_boot::boot` under the same spec `crate::resolve_policy` builds, with
    /// the input that used to produce the pairing: no working directory. `cwd` is a `BootSpec`
    /// FIELD, so this needs no process-global mutation and races nothing — the same reason
    /// `vike_secrets::project_settings_dir_for` takes it as a parameter one layer down.
    ///
    /// ⚠ `settings: SettingsLoad::Load` mirrors `crate::resolve_policy` rather than skipping: the
    /// claim is about the sequence this binary actually runs. The named directory below does not
    /// exist, so the loader opens no file (absent files inside a settings directory are skipped
    /// individually), and `env` is this test's own map — nothing here reads the real environment or
    /// the real settings tree.
    #[test]
    fn a_boot_with_no_working_directory_resolves_the_named_directory() {
        let named = "/srv/vike-<unit>/settings";
        let env: std::collections::HashMap<String, String> =
            [("VIKE_SETTINGS_DIR".to_string(), named.to_string())].into_iter().collect();

        let booted = vike_boot::boot(&vike_boot::BootSpec {
            env: &env,
            cwd: None,
            identity: vike_boot::Identity { name: "vike-cli", version: "0.0.0-test" },
            removed_env: vike_boot::RemovedEnv::Refuse,
            settings: vike_boot::SettingsLoad::Load,
            credentials: vike_boot::Credentials::Deferred("this test opens no credential file"),
            log_home: vike_boot::LogHome::Elsewhere("this CLI builds no subscriber"),
            disclosure: vike_boot::Disclosure::Skip("no disclosure is rendered here"),
        })
        .expect("a boot with no working directory is a legitimate boot, not a failure");

        assert_eq!(
            booted.settings_dir.as_deref(),
            Some(Path::new(named)),
            "a NAMED settings directory needs no walk — dropping it because the working directory \
             is gone is the defect this asserts against"
        );
        assert_eq!(
            booted.settings_dir_override.as_deref(),
            Some(named),
            "…and the rung that answered is still reported"
        );

        assert_eq!(
            store_path(
                &args_with(None),
                booted.settings_dir.as_deref(),
                booted.settings_dir_override.as_deref(),
            ),
            Path::new(named).join(SECRETS_FILE),
            "end to end: the dispatcher's own values must resolve the NAMED store — now through \
             the FIRST arm, where they used to reach the fallback"
        );
    }

    /// Every `Source` variant must be distinguishable in the output, and none may carry a value.
    #[test]
    fn describe_names_the_store_and_never_a_secret() {
        let found = describe(&Source::File(PathBuf::from("/p/settings/secrets.env")));
        assert!(found.contains("/p/settings/secrets.env"));
        assert!(describe(&Source::None).contains("paper"));
        assert_ne!(found, describe(&Source::None));
        assert!(!found.contains('='), "a store description must never carry a KEY=value: {found}");
    }

    /// **Only `NotFound` is absence; a probe that FAILED is its own answer.**
    ///
    /// This is the guard, and it is the whole point of [`Presence`]. The old probe was
    /// `Path::exists`, which folds EVERY error into `false` — so an unsearchable project root
    /// (`EACCES`) reported the store as cleanly absent and `path` printed the fresh-install advice
    /// for a permissions bug. The `exists()` assertion below is that defect, reproduced: it is what
    /// this command used to print from, and it still answers `false` here.
    ///
    /// ⚠ The third state is produced by probing THROUGH a regular file (`ENOTDIR`) rather than by
    /// `chmod 0o000` on a parent — a mode-based denial is a no-op for root, and CI runs as root, so
    /// that shape would pass VACUOUSLY there. `ENOTDIR` is uid-independent. It is the same trick
    /// `crates/vike-secrets/src/store.rs`'s `only_not_found_counts_as_absent_and_a_rootless_path_is_skipped`
    /// uses, for the same reason. Unix-only: Windows reports a path under a file as
    /// `ERROR_PATH_NOT_FOUND`, which IS `NotFound`, so there is no non-`NotFound` error to make
    /// portably — the two states above are still checked there.
    #[test]
    fn only_not_found_reads_as_absent_and_a_failed_probe_says_so() {
        let d = std::env::temp_dir().join(format!(
            "vike-cli-presence-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let store = d.join("secrets.env");

        let p = presence(&store);
        assert!(matches!(p, Presence::Absent), "nothing there: NotFound IS established absence");
        assert_eq!(p.label(), "absent");

        std::fs::write(&store, "BINANCE_LIVE_API_KEY=never-printed\n").unwrap();
        let p = presence(&store);
        assert!(matches!(p, Presence::Present), "a file that stats is present");
        assert_eq!(p.label(), "present");

        #[cfg(unix)]
        {
            let under_a_file = store.join("settings").join(SECRETS_FILE);
            assert!(
                !under_a_file.exists(),
                "the OLD probe answers `false` here — that is the defect, not the fixture"
            );
            let p = presence(&under_a_file);
            assert!(matches!(p, Presence::Undetermined(_)), "ENOTDIR is not absence: {p:?}");
            let label = p.label();
            assert!(label.contains("could not be determined"), "{label}");
            assert!(!label.contains("absent"), "a failed probe must not read as absence: {label}");
        }

        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn usage_documents_every_subcommand_and_where_the_store_is() {
        for needle in ["list", "path", "--file", "settings/secrets.env", "VIKE_SETTINGS_DIR"] {
            assert!(USAGE.contains(needle), "USAGE must mention {needle}");
        }
    }
}

// ── template ────────────────────────────────────────────────────────────────────────────────────

/// The header the emitted grid carries. Separate constant so the test below can assert the two
/// warnings a reader most needs are present without matching the whole banner.
const TEMPLATE_HEADER: &str = "\
# vike credential store — TEMPLATE, generated by `vike-cli secrets template`.
#
# Every name below is DERIVED from the venue roster (vike_model::venues::VENUES) crossed with the
# tiers and suffixes in vike_model::credential_keys. Nothing here is hand-listed, so a venue added
# to the roster appears here the same day.
#
# HOW TO USE IT: fill in the values for the venues you actually trade and DELETE every other line.
# An EMPTY value is equivalent to an absent key — the venue stays on paper — so leaving the blanks
# is harmless, merely noisy.
#
# ⚠ ARMING IS NOT A CREDENTIAL QUESTION ALONE. `policy.venues.<venue>` in policy.toml is a ceiling
#   consulted BEFORE the credential is read, and it defaults to `paper` for every venue. Filling a
#   key in here arms nothing by itself.
#
# ⚠ THIS GRID IS NOT COMPLETE, and the gap is structural. It covers the venues whose credentials
#   are spelled {VENUE}_{TIER}_{SUFFIX}. Venues with a BESPOKE shape — the FX brokers, which use a
#   login/password pair rather than a key/secret — cannot be derived from any central table in this
#   workspace; their keys live only in each bridge's own `crates/bridges/<venue>/src/config.rs`,
#   which is the authority for them. Write those by hand.
#
# ⚠ LEGACY TIER SPELLINGS ARE OMITTED ON PURPOSE. `credential_keys()` also yields the pre-rename
#   `MAINNET` tier, because the loader still reads it. Emitting it here would teach a deprecated
#   spelling to somebody writing their first store, so this grid stops at SIM/DEMO/LIVE.
";

/// Emit the empty credential grid on **stdout**.
///
/// Writes no file and takes no path — see this module's *Read-only, always* section for why there
/// is no `--out`. Pure apart from the printing: [`template_body`] builds the whole string, so the
/// grammar is unit-tested without capturing stdout.
fn run_template(venue: Option<&str>) -> Result<(), String> {
    let body = template_body(venue)?;
    print!("{body}");
    Ok(())
}

/// Build the template text. PURE — the testable half of [`run_template`].
///
/// `venue` filters to one roster id; `None` emits the whole grid. An unknown id is an ERROR naming
/// the roster rather than an empty emission, because a silent empty file is indistinguishable from
/// "this venue needs no credentials" and would be redirected straight over a store.
fn template_body(venue: Option<&str>) -> Result<String, String> {
    use vike_model::credential_keys::starter_keys;
    use vike_model::venues::VENUES;

    let venues: Vec<&str> = match venue {
        None => VENUES.to_vec(),
        Some(v) => {
            let lower = v.to_lowercase();
            if !VENUES.contains(&lower.as_str()) {
                return Err(format!("unknown venue '{v}' — the roster is: {}", VENUES.join(", ")));
            }
            vec![VENUES.iter().find(|r| **r == lower).copied().expect("membership just checked")]
        }
    };

    // ⚠ The key names are COMPOSED in `vike_model::credential_keys::starter_keys`, not here, and
    // that is not a style preference: `crates/vike-ops/tests/settings_registry.rs`'s
    // `generated_key_sites` reads a call to `credential_key` as "this crate READS these variables"
    // and would then demand the whole grid's worth of `SETTINGS` rows for `vike-cli`. This command
    // reads none of them — it prints names. `starter_keys`' doc carries the full argument.
    let mut out = String::from(TEMPLATE_HEADER);
    for v in venues {
        out.push_str(&format!("\n# ── {v} ──\n"));
        // `starter_keys` emits the credential rows first and the attribution rows (if any) after,
        // so a single latch puts the note immediately before the first attribution row and never
        // again. A previous version tested the tail of `out`, which re-emitted the note before the
        // SECOND attribution key because the tail was by then the first key's own line.
        let mut noted = false;
        for key in starter_keys(v) {
            if !noted && (key.ends_with("_BROKER_CODE") || key.ends_with("_BUILDER_CODE")) {
                out.push_str("# optional — affiliate/builder attribution, absent = unattributed\n");
                noted = true;
            }
            out.push_str(&key);
            out.push_str("=\n");
        }
    }
    Ok(out)
}

#[cfg(test)]
mod template_tests {
    use super::{parse, template_body, Sub, TEMPLATE_HEADER};
    use vike_model::venues::VENUES;

    fn parse_of(argv: &[&str]) -> Result<super::Args, String> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn template_is_a_subcommand_and_takes_venue() {
        assert_eq!(parse_of(&["template"]).unwrap().sub, Sub::Template);
        assert_eq!(
            parse_of(&["template", "--venue", "okx"]).unwrap().venue.as_deref(),
            Some("okx")
        );
    }

    /// `--venue` on an inspecting subcommand is REFUSED rather than ignored — a silently dropped
    /// filter lets somebody believe they scoped an output that was never scoped.
    #[test]
    fn venue_is_refused_on_list_and_path() {
        for sub in ["list", "path"] {
            let err = parse_of(&[sub, "--venue", "okx"]).unwrap_err();
            assert!(err.contains("--venue"), "{sub}: {err}");
        }
    }

    /// EVERY roster venue appears, so a venue added to `VENUES` cannot silently miss the template.
    /// This is the completeness property the capability-map playbook asks of any per-venue output.
    #[test]
    fn every_roster_venue_is_emitted() {
        let body = template_body(None).unwrap();
        for v in VENUES {
            assert!(body.contains(&format!("# ── {v} ──")), "no section for {v}");
            assert!(
                body.contains(&format!("{}_LIVE_API_KEY=", v.to_uppercase())),
                "no LIVE key row for {v}"
            );
        }
    }

    /// The emitted grid carries NO values — it is routinely redirected into a real store, and a
    /// stray value would be a credential written by us.
    #[test]
    fn no_row_carries_a_value() {
        let body = template_body(None).unwrap();
        for line in body.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
            assert!(line.ends_with('='), "row carries a value: {line}");
            assert_eq!(line.matches('=').count(), 1, "row has more than one '=': {line}");
        }
    }

    /// The legacy `MAINNET` tier is READ by the loader but must not be TAUGHT here.
    #[test]
    fn the_legacy_tier_is_not_emitted() {
        let body = template_body(None).unwrap();
        assert!(!body.contains("_MAINNET_"), "template teaches the deprecated MAINNET tier");
    }

    /// The two warnings a first-time reader most needs: that the grid is incomplete for the FX
    /// venues, and that a filled key arms nothing without the policy ceiling.
    #[test]
    fn the_header_carries_both_load_bearing_warnings() {
        assert!(TEMPLATE_HEADER.contains("BESPOKE"), "no incompleteness warning");
        assert!(TEMPLATE_HEADER.contains("policy.venues"), "no arming-ceiling warning");
    }

    #[test]
    fn one_venue_filter_emits_only_that_venue() {
        let body = template_body(Some("okx")).unwrap();
        assert!(body.contains("OKX_LIVE_API_KEY="));
        assert!(!body.contains("BINANCE_LIVE_API_KEY="));
    }

    /// An unknown venue ERRORS and names the roster. An empty emission would be redirected over a
    /// store and look like "this venue needs nothing".
    #[test]
    fn an_unknown_venue_errors_and_names_the_roster() {
        let err = template_body(Some("kraken_futures_x")).unwrap_err();
        assert!(err.contains("unknown venue"), "{err}");
        assert!(err.contains(VENUES[0]), "error should list the roster: {err}");
    }

    #[test]
    fn the_venue_filter_is_case_insensitive() {
        assert!(template_body(Some("OKX")).unwrap().contains("OKX_LIVE_API_KEY="));
    }
}

#[cfg(test)]
mod template_shape_tests {
    use super::template_body;

    /// The attribution note appears at most ONCE per venue. An earlier version tested the tail of
    /// the buffer and re-emitted it before every attribution key after the first.
    #[test]
    fn the_attribution_note_is_not_repeated() {
        let body = template_body(None).unwrap();
        let notes = body.matches("affiliate/builder attribution").count();
        let venues_with_attribution = vike_model::venues::VENUES
            .iter()
            .filter(|v| !vike_model::attribution::attribution_for(v).is_none())
            .count();
        assert_eq!(
            notes, venues_with_attribution,
            "the note must appear exactly once per attributed venue"
        );
    }
}
