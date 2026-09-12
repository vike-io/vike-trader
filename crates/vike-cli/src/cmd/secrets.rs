//! `vike-cli secrets` — inspect the credential store, and set ONE key in it.
//!
//! ```text
//! vike-cli secrets list     print the KEY NAMES held in the store, the ACCOUNTS they resolve
//!                           to, and which file that is
//! vike-cli secrets path     print the store's path, whether it exists, and its exposure
//! vike-cli secrets set KEY  upsert ONE key — value from stdin, or from a named environment
//!                           variable. NEVER from argv. See the writer section below
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
//! # ONE writer, and its shape was fixed BEFORE it was built
//!
//! This section used to be headed *Read-only, always* and read "no subcommand writes anything, and
//! there is deliberately no subcommand that does". That was true, and
//! `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` is the record
//! that decided it — including, in its *What would reopen this* clause, the ONE acceptable shape a
//! writer could ever take. [`run_set`] is that shape and nothing wider:
//!
//! * a **second call site of `vike_secrets::save_credentials`** — the workspace's one in-place,
//!   byte-preserving, atomic upsert. No second writer, no second transform, and nothing here opens
//!   the store for writing at all;
//! * the value comes from **stdin or a NAMED environment variable, never argv** —
//!   `vike-cli secrets set KEY VALUE` is a usage error, because argv lands in shell history and in
//!   `ps` output for every user on the box, which is the reason this workspace already passes
//!   secrets to child processes by environment only;
//! * the key name is **validated against `vike_model::credential_keys`** and refused BY NAME
//!   otherwise, so a typo cannot write a key nothing will ever read. ⚠ The refusal is one act with
//!   THREE messages, because "outside the grid" and "read by nothing" are different facts and
//!   saying the second when only the first is true is a lie an operator acts on —
//!   [`unknown_key_message`] carries the measurement;
//! * the destination is **the PROJECT's store, never a path the operator names** — `--file` is an
//!   inspection flag on the three reading subcommands and is REFUSED here, because the same
//!   resolution that points a READ at a named file points a WRITE at it, and
//!   `set KEY --file ~/.bashrc` appended a live credential to a shell rc file and exited 0.
//!   `$VIKE_SETTINGS_DIR` is how a scripted run aims at a different project;
//! * it **CREATES no store.** An absent store is refused, naming the path and
//!   `vike-cli secrets template`. Creating the file stays the operator's decision, made with an
//!   editor or with that redirection;
//! * it is **journalled** — one `vike_model::change_journal` `credential_write` record per write,
//!   `Actor::cli`, key NAMES only;
//! * and it is **unreachable from the `mcp` arm**, which advertises no credential tool at all
//!   (`crates/vike-cli/src/cmd/mcp.rs`'s `the_mcp_surface_advertises_no_credential_writer`).
//!
//! Nothing here ever prints, logs or errors with a VALUE.
//!
//! ⚠ **`template` writes nothing either, and the shape is the reason.** It writes the key GRID
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vike_model::account_keys::accounts_in_store;
// The workspace's gated catalog of every environment variable it reads — the authority behind
// `set`'s read-but-not-settable refusal. See [`registry_readers`] for why the answer is derived
// from it rather than from a table of this command's own.
use vike_ops::settings::SETTINGS;
use vike_secrets::{SECRETS_FILE, Source, resolve};

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::exit::{CliError, CmdResult};

/// The command's own usage roster. `pub(crate)` so `crate::cmd::mcp`'s
/// `the_instructions_name_only_real_commands` can hold the MCP `instructions` text to the
/// subcommands and flags THIS module actually accepts, rather than to a copy of them.
pub(crate) const USAGE: &str = "\
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
  set KEY   upsert ONE key into an EXISTING store, preserving every other byte.
            The VALUE never appears on the command line — stdin, or a named
            environment variable, and nothing else:
              printf %s \"$SECRET\" | vike-cli secrets set BINANCE_LIVE_API_KEY
              vike-cli secrets set BINANCE_LIVE_API_KEY --from-env BINANCE_KEY
            KEY must be one the enumerable GRID holds; anything else is refused
            BY NAME, and the refusal says whether the name is read by something
            outside that grid or by nothing at all by name. Most outside-the-grid
            keys are edited in by hand; the two vike-tradehub NODE keys are the
            exception and have a command of their own, `vike-cli backend setup`,
            which the refusal names instead of sending you to an editor.
            An absent store is refused too — create it with `template`

options:
  --file PATH     list/path/template: inspect this store instead of the project's.
                  REFUSED on `set` — it cannot aim a write at an arbitrary
                  path; $VIKE_SETTINGS_DIR names a different project instead
  --venue ID      template: emit only this venue's rows
  --from-env NAME set: take the value from this environment variable, verbatim
  --json          list: the same disclosure as one JSON object — the store path,
                  the key NAMES and the accounts they resolve to. Never a value,
                  same as the human listing
  -h, --help      this message

the store is <project>/settings/secrets.env; $VIKE_SETTINGS_DIR names that directory outright";

#[derive(Debug, PartialEq, Eq)]
enum Sub {
    List,
    Path,
    Template,
    /// `set KEY` — the ONE writer. The key is carried on [`Args::key`] rather than in the variant
    /// so the flag loop below stays one shape for every subcommand.
    Set,
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    sub: Sub,
    file: Option<PathBuf>,
    /// `template --venue ID` — emit one venue's rows instead of the whole grid. Validated against
    /// [`vike_model::venues::VENUES`] at RUN time rather than parse time, so the error can name the
    /// roster; parsing stays pure and total.
    venue: Option<String>,
    /// `list --json` — the same disclosure as a MACHINE shape. See [`list_json`] for why the
    /// key-names-only guarantee is the thing that made this worth adding at all.
    json: bool,
    /// `set KEY` — the credential key NAME to upsert. The one positional argument this command
    /// accepts, and the ONLY one: a SECOND positional is the argv-value form, and it is refused
    /// (see [`ARGV_VALUE_REFUSAL`]).
    ///
    /// Validated at RUN time for the same reason `venue` is — the error names the nearest valid
    /// keys, which needs the grid, and this parser stays pure and total.
    key: Option<String>,
    /// `set KEY --from-env NAME` — take the value from the environment variable `NAME`, out of the
    /// map the DISPATCHER swept. Never `std::env::var`: nothing in THIS file reads the environment,
    /// so none of it joins `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN`.
    from_env: Option<String>,
}

/// What a value on the command line is refused WITH — spelled once, and deliberately quoting
/// NOTHING the operator typed.
///
/// ⚠ The refusal message may not echo the offending token, and that is the whole point of the
/// refusal: the token IS the secret. An error that helpfully printed `unexpected argument
/// 'sk-live-…'` would have written the credential into the terminal scrollback of the very session
/// this refusal exists to keep it out of.
///
/// ⚠ It is what EVERY unrecognised token on `set` is refused with, a mistyped FLAG included, and the
/// last line is there because of that: the price of never guessing which tokens are safe to name is
/// that a `--form-env` slip reads as a value refusal, so the message has to point at where the real
/// flags are listed. `crate::cmd::args::exit_for_parse_error` prints the USAGE directly beneath it.
const ARGV_VALUE_REFUSAL: &str = "a credential VALUE may not be given on the command line — argv \
lands in shell history and in `ps` output for every user on the box. Two forms are accepted:\n  \
printf %s \"$SECRET\" | vike-cli secrets set KEY        (the value on stdin, one line)\n  \
vike-cli secrets set KEY --from-env NAME              (the value from $NAME)\n\
(if you meant a FLAG: no token is quoted back here, because on `set` an unrecognised one is most \
likely the secret — the flags this subcommand takes are in the usage below)";

/// Parse `secrets`' own argv tail (everything after the subcommand name). PURE — no I/O, so the
/// whole grammar is unit-tested below.
fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let Some(first) = it.next() else {
        return Err("a subcommand is required (list | path | template | set)".to_string());
    };
    let sub = match first.as_str() {
        "list" => Sub::List,
        "path" => Sub::Path,
        "template" => Sub::Template,
        "set" => Sub::Set,
        "-h" | "--help" | "help" => return help_requested(),
        other => return Err(format!("unknown `secrets` subcommand '{other}'")),
    };
    let mut file = None;
    let mut venue = None;
    let mut json = false;
    let mut key: Option<String> = None;
    let mut from_env = None;
    let mut flags = Flags::new(it);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--file" => file = Some(PathBuf::from(flags.value(&flag, inline)?)),
            "--venue" => venue = Some(flags.value(&flag, inline)?),
            "--from-env" => from_env = Some(flags.value(&flag, inline)?),
            "--json" => {
                no_value(&flag, inline)?;
                json = true;
            }
            "-h" | "--help" => return help_requested(),
            // ⚠ The ONE non-flag arm, and it exists for `set KEY` alone. `Flags::next_flag` yields
            // every argument, flag or not, so a POSITIONAL arrives here — which is why every other
            // subcommand's stray argument still reads as `unknown option`, unchanged.
            //
            // A positional carrying an inline `=` (`next_flag` splits on the first one) can only be
            // a VALUE — no credential key name contains `=` — so it is refused with the rest.
            other if sub == Sub::Set && !other.starts_with('-') => {
                if key.is_some() || inline.is_some() {
                    return Err(ARGV_VALUE_REFUSAL.to_string());
                }
                key = Some(other.to_string());
            }
            // ⚠ **On `set`, NO unrecognised token is ever named back.** It is refused as a VALUE,
            // because on this subcommand the likeliest thing an unrecognised token is, is the
            // secret — and the arm below echoes what it was given.
            //
            // This used to fall through to that arm whenever the token began with a dash. Base64url
            // alphabets contain `-`, so `secrets set BINANCE_LIVE_API_KEY -sk-live-…` printed the
            // credential verbatim to stderr — the stream CI logs and every service manager captures
            // — while exiting on the usage rung: the refusal doing the exact damage it exists to
            // prevent, on the one input class neither test covered (both spelled values beginning
            // with a letter).
            //
            // It is not routed to the positional arm either: a dash-leading token accepted as a KEY
            // would reach `unknown_key_message`, which names the key it was given — the same echo,
            // one step later.
            //
            // ⚠ The first fix here EXEMPTED a leading `--`, on the argument that no value can be
            // spelled that way and a `--form-env` typo is worth naming. That was wrong and the test
            // caught it: a PEM-armoured key begins `-----BEGIN`, which starts with `--`, and it was
            // echoed in full. Any rule that decides from the token's own SHAPE is guessing about the
            // secret's alphabet, so there is no rule — the refusal names none of them, and
            // `exit_for_parse_error` prints the USAGE beneath it, which is where an operator who
            // mistyped a flag reads the flags this subcommand actually takes.
            // The token is deliberately NOT bound: there is nothing this arm may do with it.
            _ if sub == Sub::Set => return Err(ARGV_VALUE_REFUSAL.to_string()),
            other => return Err(format!("unknown option '{other}'")),
        }
    }
    // `--venue` is meaningless to the two INSPECTING subcommands, and silently ignoring a flag the
    // operator typed is how a person comes to believe they filtered something.
    if venue.is_some() && sub != Sub::Template {
        return Err("--venue applies to `template` only".to_string());
    }
    // ⚠ **`--file` is an INSPECTION flag, and it is refused on the writer.**
    //
    // It stays permitted on the three reading subcommands — it is inert for `template`, which opens
    // nothing, and refusing it would break `--file X list` habits for no safety gain. On `set` it is
    // a different flag entirely: the same resolution that pointed a READ at a file the operator
    // named pointed a WRITE at it, so `set KEY --file ~/.bashrc` appended a live credential to a
    // shell rc file and exited 0 — and the change journal then recorded the write against a "store"
    // named `bashrc`, a file that is not one.
    //
    // `docs/decisions/0036` fixes this verb's shape as *upserts ONE named key into an EXISTING
    // store and creates none*, and the store it means is the project's. An operator-supplied
    // destination is outside that fence, so the fence is what holds rather than the flag: a
    // scripted run that must aim at a different PROJECT already has `$VIKE_SETTINGS_DIR`, which
    // moves the whole settings directory — the ledger, the policy and the store together — instead
    // of pointing one write at one path.
    if file.is_some() && sub == Sub::Set {
        return Err("--file inspects a store; it cannot aim a WRITE at an arbitrary path. `set` \
                    writes the project's store — name a different project with $VIKE_SETTINGS_DIR, \
                    and `vike-cli secrets path` prints what that resolved to"
            .to_string());
    }
    // `--json` is refused on the other two for the same reason, and each has its own: `path`'s
    // product is three lines a human reads when something is already broken, and `template`'s
    // product is a FILE FORMAT — a credential store is `KEY=VALUE`, so rendering it as JSON would
    // emit something no loader on this box can read while looking like it had worked.
    if json && sub != Sub::List {
        return Err("--json applies to `list` only".to_string());
    }
    // `--from-env` refused off `set`, same rule as the two above: a flag the operator typed and the
    // program dropped is how somebody comes to believe a value was taken from somewhere it was not.
    if from_env.is_some() && sub != Sub::Set {
        return Err("--from-env applies to `set` only".to_string());
    }
    // `set` needs its key, and the message names BOTH value forms — the operator who typed
    // `secrets set` alone is the one who does not yet know how the value gets in.
    if sub == Sub::Set && key.is_none() {
        return Err(format!("`set` needs a credential KEY.\n{ARGV_VALUE_REFUSAL}"));
    }
    Ok(Args { sub, file, venue, json, key, from_env })
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

/// **Everything this command needs from OUTSIDE itself, resolved by the dispatcher's ONE boot walk
/// and its ONE environment sweep.**
///
/// A struct rather than five parameters, and not for tidiness: every field here is a fact only
/// `crate::run` can know, and the rule this crate is held to is that a `src/cmd/` file reads no
/// environment and performs no second walk of its own. Bundling them is what keeps that rule
/// visible when the list grows — `set` added three at once (the ledger's home, the environment map
/// and the instant), and three more positional `Option`s in a row is exactly the signature nobody
/// can read a call site of.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    /// `<project>/settings`, as the boot resolved it.
    pub settings_dir: Option<&'a Path>,
    /// The `$VIKE_SETTINGS_DIR` value that boot HONOURED — the rung, not a spare copy. See
    /// [`store_path`] for why it is not redundant.
    pub settings_dir_override: Option<&'a str>,
    /// `<project>/settings/state`, off the SAME walk — the change journal's home for [`run_set`].
    ///
    /// `None` (no project above the working directory) means NOTHING is journalled, rather than an
    /// append-only ledger in a guessed directory: the disposition `vike_boot::journal_boot_settings`
    /// and `vike_model::change_journal`'s `None`-handle rule already state, and the write itself
    /// still happens.
    pub state_dir: Option<&'a Path>,
    /// The process environment the dispatcher swept, for `set --from-env NAME`. A PARAMETER, so
    /// nothing under `src/cmd/` joins `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN`.
    pub env: &'a HashMap<String, String>,
    /// The instant a `credential_write` record is stamped with. `vike_model::change_journal` reads
    /// no clock, so the instant is a parameter all the way down — the same rule
    /// `vike_boot::journal_boot_settings`' `ts_ms` and `vike_ctrader::token_store::persist`'s
    /// `now_ms` follow.
    pub now_ms: i64,
}

/// Run the subcommand. Returns the process exit code.
///
/// [`Ctx`] carries everything resolved outside this file, and `store_path` carries the argument for
/// why the settings directory and the override it was resolved from are two values rather than one.
///
/// ⚠ This command already separated the help short-circuit from a usage error and already exited
/// 0 for it — but it printed the help with `eprintln!`, so `vike-cli secrets --help | less` showed
/// an empty page. Routing through [`crate::cmd::args::exit_for_parse_error`] moved the help text to
/// the stream a user is piping — and, since the exit ladder landed, is also what puts this
/// command's usage errors on the shared USAGE rung without this file naming a number.
pub fn run(args: impl Iterator<Item = String>, ctx: Ctx<'_>) -> ExitCode {
    let args = match parse(args) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("secrets", USAGE, &msg),
    };
    // ⚠ The three READING arms still return `Result<(), String>` and are converted here by
    // `From<String> for CliError`, which classifies as `Exit::Failed` — the rung they have always
    // exited on, byte-identical. Only [`run_set`] classifies, because only it has two failures a
    // caller must tell apart: a bad KEY is a command line to fix (USAGE), an absent store is a box
    // to configure (FAILED). That is the ladder's own migrate-one-verb-at-a-time shape.
    let outcome: CmdResult<()> = match args.sub {
        Sub::List => {
            run_list(&args, ctx.settings_dir, ctx.settings_dir_override).map_err(Into::into)
        }
        Sub::Path => {
            run_path(&args, ctx.settings_dir, ctx.settings_dir_override).map_err(Into::into)
        }
        Sub::Template => run_template(args.venue.as_deref()).map_err(Into::into),
        Sub::Set => run_set(&args, &ctx),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli secrets: {}", e.msg);
            e.exit.into()
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
    // ⚠ THE NODE KEYS ARE NOT LISTED HERE, and that is a design call rather than an omission.
    // Since 2026-09-08 they live in `node.env` beside this file, and `vike-cli backend status`
    // already owns the question "which node keys resolved, and from where" — it reports the pair,
    // each key's id, and whether the node answers. Listing them here too would be a SECOND answer
    // to one question, which is the failure this workspace gates against elsewhere.
    //
    // What this verb owes instead is that nobody reads the absence as "there are none": it prints
    // the venue grid, and a reader who came looking for a node key must be told where to look. The
    // pointer is unconditional — printing it only when `node.env` exists would mean a box that has
    // not migrated, whose keys are in THIS file, is told nothing.
    if !args.json {
        eprintln!(
            "note: node keys are not in this listing — they live in `{}` beside this store and are \
             reported by `vike-cli backend status`",
            vike_secrets::NODE_FILE
        );
    }
    let accounts = accounts_in_store(resolved.secrets.keys());
    if args.json {
        // ⚠ Built from the SAME two values the human branch renders — the key iterator and the
        // accounts derived from it — so a machine and a person cannot be told different things
        // about one store. The warnings above already went to stderr, which is why they are not in
        // the document: stdout under `--json` is the document and nothing else.
        println!("{}", list_json(&resolved.source, resolved.secrets.keys(), &accounts));
        return Ok(());
    }
    println!("source: {}", describe(&resolved.source));
    println!("{} secret(s):", resolved.secrets.len());
    for k in resolved.secrets.keys() {
        println!("  {k}");
    }
    println!("{} account(s) in the store:", accounts.len());
    for a in &accounts {
        println!("  {}", describe_account(a));
    }
    Ok(())
}

/// The `list --json` document: the store's path, the key NAMES, and the accounts those names
/// resolve to.
///
/// ⚠ **The property this shape exists to keep is that a VALUE cannot appear in it.** The function
/// takes an ITERATOR OF KEYS rather than the secret map, so there is no value in scope to leak by
/// accident — the guarantee is structural rather than a rule somebody has to remember while editing
/// the renderer. `list`'s whole reason for existing is that its output is safe to paste into an
/// issue, and a machine-readable output that quietly stopped being safe would be pasted more, not
/// less. `a_json_listing_carries_names_and_never_a_value` in `tests/secrets_cli.rs` asserts it over
/// a store holding recognisable values.
///
/// `store` is `null` when no store was found, which is the JSON of
/// [`describe`]'s `no store found — every venue stays paper`: a machine gets the ABSENCE as a null
/// rather than as a sentence it would have to pattern-match.
fn list_json<'a>(
    source: &Source,
    keys: impl Iterator<Item = &'a str>,
    accounts: &[vike_model::account_keys::AccountRef],
) -> String {
    let doc = serde_json::json!({
        "store": match source {
            Source::File(p) => serde_json::Value::String(p.display().to_string()),
            Source::None => serde_json::Value::Null,
        },
        "keys": keys.collect::<Vec<_>>(),
        "accounts": accounts
            .iter()
            .map(|a| serde_json::json!({
                "venue": a.venue,
                "tier": a.tier,
                // `null`, never the word DEFAULT: `AccountLabel::parse` REFUSES that spelling, so
                // emitting it would hand a machine a label it cannot feed back to the loader —
                // the same trap `describe_account` renders as `(default)` for a human.
                "label": a.label.text(),
            }))
            .collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, arrays and nulls; serialization is total")
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
    // ⚠ TWO files since 2026-09-08, and this verb is the one an operator runs to answer "which file
    // are my keys coming from". Printing one path while a second holds the node keys would make
    // this command the thing it exists to prevent.
    //
    // ⚠ `--file` is deliberately NOT honoured for this line. That flag aims the VENUE store at an
    // arbitrary path for inspection; the node store is always the project's, and pretending
    // otherwise would invent a pairing that no reader implements.
    if args.file.is_none() {
        let n = settings_dir.map_or_else(
            || vike_secrets::workspace_node_path_from(settings_dir_override),
            |d| d.join(vike_secrets::NODE_FILE),
        );
        println!("nodes:  {} ({})", n.display(), presence(&n).label());
        if let Some(w) = vike_secrets::permission_warning(&n) {
            eprintln!("⚠ {w}");
        }
    }
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
        assert_eq!(
            parse_of(&["list"]).unwrap(),
            Args {
                sub: Sub::List,
                file: None,
                venue: None,
                json: false,
                key: None,
                from_env: None
            }
        );
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
        Args {
            sub: Sub::List,
            file: file.map(PathBuf::from),
            venue: None,
            json: false,
            key: None,
            from_env: None,
        }
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
    use super::{Sub, TEMPLATE_HEADER, parse, template_body};
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

// ── set ─────────────────────────────────────────────────────────────────────────────────────────

/// `set KEY` — upsert ONE credential into an EXISTING store, and record it.
///
/// The whole of the writer `docs/decisions/0036`'s reopen clause fixed the shape of; this module's
/// doc lists the properties and why each is there. What this function adds beyond them is the
/// ORDER, and the order is the argument:
///
/// 1. **Validate the KEY first**, before anything is read, opened or asked of stdin. A refused key
///    must not have consumed the operator's piped secret on its way to the error.
/// 2. **Then open the store** — through `vike_secrets::resolve`, the same reader `list` uses, which
///    answers three ways: present (proceed), absent (REFUSE — this command creates nothing), and
///    unreadable (an ERROR, never "not configured"; a permissions bug wearing the fresh-install
///    answer is the failure `vike_secrets::resolve`'s own doc exists for). It also tells us whether
///    the key is REPLACED or APPENDED, which is the one thing `save_credentials` does not report.
/// 3. **Then take the value**, from stdin or from the named variable.
/// 4. **Then write**, through the workspace's one upsert.
/// 5. **Then record**, and a failure here does NOT fail the call — the credential IS on disk, and
///    sending a caller down an error path for a write that succeeded is worse than a missing ledger
///    line. The same disposition `vike_ctrader::token_store`'s `record_rotation` takes.
///
/// ⚠ **Nothing in this function can print, log or ERROR with a value.** The value lives in one
/// local, is moved into the update pair, and every message built here names a KEY, a PATH or an
/// environment VARIABLE. `crates/vike-cli/tests/secrets_cli.rs`'s
/// `set_from_stdin_appends_the_key_and_preserves_every_other_byte` is the assertion over the real
/// binary's two streams, and its sibling
/// `a_value_in_argv_is_refused_on_the_usage_rung_and_never_echoed` is the same claim about the
/// REFUSAL path — the one an operator reaches with the secret already typed.
fn run_set(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let key = args.key.as_deref().expect("parse refuses `set` with no key");
    // 1. The key, against the grid this workspace can actually read.
    let (venue, tier) = vike_model::credential_keys::key_owner(key)
        .ok_or_else(|| CliError::usage(unknown_key_message(key)))?;

    // 2. The store — the PROJECT's, always. `store_path` still honours `--file`, because the three
    // reading subcommands share it; `parse` is what refuses that flag here, so the destination of a
    // write is never operator-supplied and the ledger's store name below is always a real store's.
    let path = store_path(args, ctx.settings_dir, ctx.settings_dir_override);
    let resolved = resolve(&path).map_err(|e| {
        CliError::failed(format!(
            "{e} — the store is THERE and could not be read, which is a different problem from \
             having none. Nothing was written."
        ))
    })?;
    if matches!(resolved.source, Source::None) {
        return Err(CliError::failed(format!(
            "no credential store at {} — this command upserts into an existing store and creates \
             none. Make one, then set the key:\n  vike-cli secrets template > {}\n  chmod 600 {}",
            path.display(),
            path.display(),
            path.display()
        )));
    }
    // Same finding, same stream and same shape as `list`'s — a path and an octal mode, never a
    // credential. A finding is never a refusal.
    if let Some(w) = &resolved.warning {
        eprintln!("⚠ {w}");
    }
    // The one thing `vike_secrets::save_credentials` does not report back (see
    // `vike_connections::save_credentials_journalled`'s "What it does NOT claim"). Asked HERE, of a
    // map we already hold, rather than by re-reading the store after the write.
    let replaced = resolved.secrets.keys().any(|k| k == key);

    // 3. The value. AFTER the two refusals above, so neither can happen with a secret in hand.
    let value = value_for(args, ctx)?;

    // 4. The write — a SECOND CALL SITE of the one writer, never a second writer.
    vike_secrets::save_credentials(&path, &[(key.to_string(), value)])
        .map_err(|e| CliError::failed(format!("could not write {}: {e}", path.display())))?;

    // 5. The durable record.
    record_write(ctx, &path, key, venue, tier);

    println!("{key} {} in {}", if replaced { "replaced" } else { "appended" }, path.display());
    Ok(())
}

/// The value to write: stdin, or the environment variable `--from-env` named.
///
/// ⚠ **The two are trimmed DIFFERENTLY, on purpose.** A piped value arrives with the newline the
/// shell or the operator's editor put there, and `printf %s` is not what anybody types by default —
/// so stdin is trimmed, and a store full of values with trailing newlines is not a thing this
/// command can produce. An environment variable carries exactly what was exported into it, so it is
/// taken VERBATIM: trimming it would silently alter a credential whose leading or trailing
/// whitespace is real, and `vike_secrets::upsert_env` quotes such a value so it round-trips.
///
/// Both refuse EMPTY — and both refuse a value that spans more than ONE LINE. An empty value is
/// equivalent to an absent key (the venue stays on paper), so writing one would report success for
/// a change that arms nothing, which is the failure class this workspace deleted a settings key
/// over; "empty" is asked AFTER a trim, because a variable holding three spaces is that same state
/// wearing a value, and `vike_secrets::parse_dotenv` hands it back non-empty so the mount arms with
/// a garbage secret and fails at the venue instead of staying on paper.
///
/// # ⚠ ONE LINE, and the multi-line case was an INJECTION rather than an untidiness
///
/// The store's grammar is one credential per line. A `--from-env` value carrying a `\n` used to be
/// written verbatim: `vike_secrets::upsert_env` quoted it (a newline is whitespace) and joined with
/// `\n`, so the value's own break became a physical line break, and the reader then returned the
/// first half as a SILENTLY TRUNCATED credential and read the second half as a whole new
/// `KEY=VALUE` — a credential for a venue the operator never configured, past a key name this
/// command had validated. `vike-cli secrets set KEY --from-env NAME` is the CI/deploy-script form,
/// and a multi-line secret is the ordinary shape of a Vault- or Actions-injected variable.
///
/// It is refused HERE as well as in `vike_secrets::save_credentials` deliberately, and the two are
/// not redundant: the writer's refusal is the property (its byte-preservation contract cannot hold
/// over a value that ADDS lines, so every caller including the GUI needs it), while this one names
/// the VARIABLE the operator can go and look at, which an `io::Error` surfacing from three layers
/// down cannot.
fn value_for(args: &Args, ctx: &Ctx<'_>) -> CmdResult<String> {
    match args.from_env.as_deref() {
        Some(name) => {
            // The map the DISPATCHER swept — never `std::env::var`, which would put a `src/cmd/`
            // file on the settings registry's `Layer::Library` work-list.
            let value = ctx.env.get(name).cloned().unwrap_or_default();
            if value.trim().is_empty() {
                // Names the VARIABLE, never its content — and "unset, empty or blank" is ONE
                // message, because to this command they are the same state. A CI variable that
                // expanded to nothing, or a template that rendered blank, arrives as any of them.
                return Err(CliError::usage(format!(
                    "--from-env {name}: that variable is unset, empty or only whitespace in this \
                     process's environment, so there is no value to write"
                )));
            }
            if value.contains(['\n', '\r']) {
                return Err(CliError::usage(format!(
                    "--from-env {name}: that variable's value spans more than one line, and a \
                     credential is ONE line. The store cannot represent it — written out, the \
                     break would truncate the credential and turn the remainder into a second \
                     KEY=VALUE line for a key you did not name. Nothing was written."
                )));
            }
            Ok(value)
        }
        None => {
            let line = read_stdin_line().map_err(|e| {
                CliError::failed(format!("could not read the value from stdin: {e}"))
            })?;
            let value = line.trim();
            // ⚠ Asked on this arm too, though `read_stdin_line` stops at the first `\n`. It bounds
            // the value only by ACCIDENT of that choice, and only for `\n`: a lone `\r` (a CR line
            // ending, or a CRLF value pasted mid-line) survives both the read and the trim, and
            // reaches the store inside the value. One rule for both arms is cheaper to keep true
            // than an argument about which reader happens to bound what.
            if value.contains(['\n', '\r']) {
                return Err(CliError::usage(
                    "the value on stdin spans more than one line, and a credential is ONE line. \
                     The store cannot represent it — written out, the break would truncate the \
                     credential and turn the remainder into a second KEY=VALUE line for a key you \
                     did not name. Nothing was written."
                        .to_string(),
                ));
            }
            if value.is_empty() {
                let key = args.key.as_deref().unwrap_or("KEY");
                return Err(CliError::usage(format!(
                    "no value on stdin. The value never goes on the command line — one of:\n  \
                     printf %s \"$SECRET\" | vike-cli secrets set {key}\n  \
                     vike-cli secrets set {key} --from-env NAME"
                )));
            }
            Ok(value.to_string())
        }
    }
}

/// ONE line off stdin. Split out so [`value_for`]'s two arms read as the two POLICIES they are,
/// with the I/O named rather than inlined.
///
/// One line, not the whole stream: a credential is one line, and reading to EOF would let a
/// mis-aimed `cat file |` write a whole file's contents into the store as one value.
fn read_stdin_line() -> std::io::Result<String> {
    use std::io::BufRead;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}

/// The refusal for a key name `set` cannot write, with the nearest real names.
///
/// PURE, so the shape is unit-tested below. It names the offending key — a key NAME is not a secret
/// (`list` prints them, by an explicit decision in the root `CLAUDE.md`) — and never a value,
/// because it never has one: [`run_set`] validates before it reads stdin.
///
/// ⚠ **The suggestions matter more here than they would on an ordinary typo'd flag.** The store is
/// a flat `KEY=VALUE` file, so a hand-edited `BINANCE_LIVE_API_KEY_` is written just as happily as
/// the real name — and the venue then stays on paper with no error anywhere, which is failure
/// reason 3 in `docs/decisions/0036`. Refusing by name is what removes that class for this command;
/// the nearest names are what make the refusal actionable.
///
/// ⚠ **THREE refusals, not one, because ONE of them used to be FALSE.** The refusal itself is
/// unchanged in every case — `set` writes the enumerable GRID and nothing wider — but the SENTENCE
/// that explained it made a claim about the whole WORKSPACE ("setting it would write a line nothing
/// would ever load") on the strength of a fact about this one command. For a name the workspace
/// genuinely reads through some other loader that sentence is simply untrue, and it was measured
/// untrue on `VIKE_TRADEHUB_OBSERVE_KEY`: `crates/vike-tradehub/src/tradehub_cli.rs`'s
/// `start_observe_server` reads that exact name out of the credential map, and the operator who
/// followed the refusal was told a key they had just configured was inert and given no route at
/// all. The escape-hatch paragraph did not cover them either — it named the FX login pairs and "the
/// other per-bridge spellings", and a node key is neither per-bridge nor has a bridge loader to be
/// pointed at.
///
/// So the message now splits on the only question that makes the old sentence safe: **does anything
/// in this workspace read this NAME at all?** [`registry_readers`] is the answer, and the split is
/// DERIVED rather than a second roster — see that function for why the registry is the authority
/// and for the one thing it deliberately does not claim.
///
/// ⚠ **FOUR now, not three, and the fourth exists because the third's ADVICE went stale.** The
/// read-but-not-settable arm pointed every outside-the-grid name at an editor, which was the honest
/// route while nothing in this tree generated a key. `vike-cli backend setup` generates the two
/// `vike-tradehub` node keys, so for those two names the editor sentence became the same class of
/// defect the arm was built to end — correct about the refusal, wrong about the route. They are
/// separated by [`vike_model::credential_keys::is_platform_key`], the names-only table that exists
/// for exactly this distinction, and their message names the command and which BOX to run it on.
fn unknown_key_message(key: &str) -> String {
    // ⚠ **A LABELLED ACCOUNT gets its own refusal and NO suggestions**, and the reason is that the
    // obvious suggestion was dangerous rather than merely unhelpful.
    //
    // `HYPERLIQUID_LIVE_API_KEY__ALT` names a SECOND ACCOUNT — a name
    // `vike_model::account_keys::accounts_in_store` parses and
    // `vike_bridge_core::credentials::load_credentials_for_account` genuinely reads, and which
    // `secrets list` prints. This command still cannot write it (the grid is a fixed enumeration and
    // a label is an unbounded name set), so it is refused — but `nearest_keys` scored the UNLABELLED
    // base as the closest name and offered it first, and that name is real, settable and accepted.
    // Following the suggestion overwrote the DEFAULT account's live signing key with a second
    // account's, exit 0, "replaced". The second suggestion was worse in kind: it proposed writing an
    // API key into the API SECRET slot.
    //
    // So when the base resolves, the message says the one thing the operator has to know — these
    // are two different ACCOUNTS — and offers nothing to copy.
    if let Some((base, label)) = labelled_account(key) {
        return format!(
            "'{key}' names a LABELLED ACCOUNT ('{label}' on {base}), and `set` writes only the \
             fixed key grid — a label is an unbounded name set, so it is not in it.\n⚠ Do NOT set \
             '{base}' instead: that is the DEFAULT account's key, a DIFFERENT account, and writing \
             this value there would overwrite the credential that account signs with. Labelled \
             keys are read (`vike-cli secrets list` prints the accounts it found) but must be \
             added with an editor; `vike-cli secrets path` says which file."
        );
    }
    // ⚠ **THE READ-BUT-NOT-SETTABLE ARM.** Asked BEFORE the suggestions, because a name something
    // reads is not a typo of a name something else reads: offering `did you mean` for
    // `VIKE_TRADEHUB_OBSERVE_KEY` would answer a question the operator did not ask, and the
    // nearest-name list is scored against the GRID, which by construction holds nothing like it.
    // ⚠ **THE PLATFORM-KEY ARM, and it names a COMMAND rather than an editor.** The two
    // `vike-tradehub` node keys are read by this workspace, are outside the grid `set` writes, and
    // — since `vike-cli backend setup` landed — are no longer something an operator writes by hand
    // at all. They are the one outside-the-grid family with a real route, so they get their own
    // sentence: telling somebody to invent a 256-bit HMAC key in an editor is exactly the advice
    // that command exists to delete, and it is the advice this message used to give.
    if let Some(service) = vike_model::credential_keys::platform_key_service(key) {
        // ⚠ The VERB is chosen from the service, never assumed. This arm named the tradehub verb
        // unconditionally while `PLATFORM_KEYS` held one pair; the day the datahub pair joined, a
        // constant here would have sent an operator to the command for a DIFFERENT service — the
        // same defect this arm exists to end, wearing the other service's clothes.
        // ⚠ THE CLIENT LINE IS PER-SERVICE BECAUSE THE COMMAND IS. `vike-cli backend connect`
        // exists; there is no `datahub connect` — the datahub's client half is not built. Naming
        // one anyway would be this arm's own defect wearing the other service's clothes: a refusal
        // that is right about refusing and wrong about the route. Each service names only what it
        // has.
        let (verb, fallback, client) = match service {
            "vike-datahub" => (
                "datahub",
                "the datahub server",
                "\n→ On a CLIENT box there is no command yet: put the SAME pair in that box's \
                 node-key store by hand (`vike-cli secrets path` prints where), or export the two \
                 variables. A client authenticates by holding the identical pair.",
            ),
            _ => (
                "backend",
                "the tradehub daemon",
                "\n→ On a CLIENT box, `vike-cli backend connect <host> --manual` writes the pair \
                 it reads from stdin.",
            ),
        };
        return format!(
            "'{key}' IS read by this workspace — `vike_ops::settings` records {} reading it — so \
             this is NOT a line nothing would load. `set` refuses it because it is not a credential \
             you should ever TYPE: it is a 256-bit HMAC key, and a hand-pasted one that is \
             truncated fails as an opaque auth denial rather than as anything readable.\n→ On the \
             {service} box — the one RUNNING it — `vike-cli {verb} setup` MINTS both of that \
             service's node keys and prints each key's id. It never accepts a key and never prints \
             one.{client}",
            registry_readers(key).unwrap_or_else(|| fallback.to_string())
        );
    }
    if let Some(readers) = registry_readers(key) {
        return format!(
            "'{key}' IS read by this workspace — `vike_ops::settings` records {readers} reading \
             it — so this is NOT a line nothing would load. `set` refuses it for a narrower \
             reason: it writes the enumerable key GRID (`vike_model::credential_keys`) and nothing \
             wider.\n⚠ The outside-the-grid keys that live in this store — the bespoke venue \
             logins, the Telegram channel — are added with an EDITOR; `vike-cli secrets path` \
             prints the file. (The two NODE keys are the exception and have a command of their \
             own: `vike-cli backend setup`.) WHICH store a \
             given reader consults is the BINARY's choice and the registry does not record it, so \
             if the line has no effect that reader is taking the PROCESS environment instead: \
             `vike-cli config show --filter {key}` prints its row and where the value resolved \
             from."
        );
    }
    let near = nearest_keys(key);
    let tail = if near.is_empty() {
        "`vike-cli secrets template` prints every key name this workspace can read".to_string()
    } else {
        format!("did you mean: {}", near.join(", "))
    };
    // The ORIGINAL sentence, now printed ONLY where the arm above proved it true: no registry row
    // names this key, so nothing the settings gate can resolve reads it under any spelling.
    format!(
        "'{key}' is not a credential key this workspace reads — no `vike_ops::settings` row names \
         it at all — so setting it would write a line nothing would ever load — {tail}.\n⚠ The \
         BESPOKE key shapes are outside this grid on \
         purpose and cannot be set here: the FX login/password pairs, the `POLY_*` trio and the \
         other per-bridge spellings live only in each bridge's own config loader, and a LABELLED \
         account's `KEY__LABEL` is an unbounded name set. Edit those with an editor; \
         `vike-cli secrets path` says which file."
    )
}

/// **The crates `vike_ops::settings` records as READING `key`**, deduplicated and rendered — or
/// `None` when no row names it at all.
///
/// `None` is the whole point: it is the one state in which [`unknown_key_message`]'s original
/// sentence ("setting it would write a line nothing would ever load") is a true claim about the
/// workspace rather than about this command.
///
/// ⚠ **The registry is the authority here rather than a table of our own, and that is the design.**
/// A hand-kept "these names are read elsewhere" list is exactly the shape this repository has
/// watched rot: `vike_ops::settings::SETTINGS` is the workspace's own catalog of every environment
/// variable a resolvable call site reads, and `crates/vike-ops/tests/settings_registry.rs` fails CI
/// in BOTH directions over it — an undeclared read is red, and so is a row nothing reads any more.
/// A second roster here would go stale between those two gates with nothing to notice. It also
/// costs no dependency: this crate already links `vike-ops` (`default-features = false`) for
/// `config show`, whose env half is driven by the same table.
///
/// ⚠ **What it deliberately does NOT answer: WHICH store the reader consults.** The tempting
/// refinement is to split the message on
/// `vike_ops::settings::Setting::naming` — `MapLookup` ⇒ a caller-supplied map (so the credential
/// store is a plausible route), `Literal`/`Konst` ⇒ a direct `env::var` (so it is not). The first
/// half holds; **the second does not**, and a message built on it would have shipped a fresh
/// instance of the very lie this function exists to remove. `settings.rs`' own tie-break says
/// `naming` records the DIRECT read when one name is read at BOTH kinds of site, and
/// `crates/vike-backfill/src/bin/databento_backfill.rs`'s `api_key` is the counterexample in the
/// tree: its key's row is declared `Naming::Literal`, and that function asks the credential STORE
/// first and only then falls back to `env::var`. So a `Literal` row proves a direct read exists and
/// proves nothing about the store. The message states the hedge instead and sends the operator to
/// `vike-cli config show`, which resolves both stores and prints the answer for real —
/// `crates/vike-cli/src/cmd/config.rs`'s `Reads` carries the same limitation from its own side.
///
/// The crate names are rendered here rather than returned as a list because there is exactly one
/// caller and one rendering; a `Vec` would be a second shape for the same sentence.
fn registry_readers(key: &str) -> Option<String> {
    let mut krates: Vec<&'static str> =
        SETTINGS.iter().filter(|s| s.name == key).map(|s| s.krate).collect();
    krates.sort_unstable();
    krates.dedup();
    (!krates.is_empty()).then(|| krates.join(", "))
}

/// The valid key names closest to `key` — at most three, and only when they are genuinely close.
///
/// Case first, edit distance second. An operator typing a key in lower case is the single most
/// likely near-miss and is an exact match one `to_uppercase` away, while Levenshtein scores it as
/// far away as a different venue — every letter differs.
///
/// The distance CEILING is what keeps this honest: with no bound, a nonsense key returns three
/// unrelated names presented as guesses, which is worse than the bare refusal. It scales with the
/// name's length, because these names are long and a one-character slip in
/// `HYPERLIQUID_LIVE_API_PASSPHRASE` should still be caught.
fn nearest_keys(key: &str) -> Vec<String> {
    // A labelled account's base is always within the ceiling below (a `__LABEL` suffix costs a
    // handful of edits against names this long), so without this it is ALWAYS the first suggestion —
    // and it is a different ACCOUNT's real, settable key. Asked here as well as in
    // [`unknown_key_message`]'s own arm so the dangerous suggestion cannot come back through a
    // second caller; [`labelled_account`] is the one place the question is answered.
    if labelled_account(key).is_some() {
        return Vec::new();
    }
    let all = vike_model::credential_keys::lookup_keys();
    let upper = key.to_uppercase();
    if let Some(exact) = all.iter().find(|k| **k == upper) {
        return vec![exact.clone()];
    }
    let ceiling = (key.len() / 3).clamp(2, 6);
    let mut scored: Vec<(usize, String)> = all
        .into_iter()
        .map(|k| (edit_distance(&upper, &k), k))
        .filter(|(d, _)| *d <= ceiling)
        .collect();
    // Distance first, then the NAME, so the list is deterministic — two candidates at the same
    // distance must not reorder between runs.
    scored.sort();
    scored.into_iter().take(3).map(|(_, k)| k).collect()
}

/// `(base, label)` when `key` is a LABELLED ACCOUNT name whose base is a real credential key —
/// `HYPERLIQUID_LIVE_API_KEY__ALT` — and `None` otherwise.
///
/// The grammar is `vike_model::account_keys`': split at the FIRST `ACCOUNT_SEPARATOR`, which is a
/// DOUBLE underscore. The base must RESOLVE, so a single-underscore near-miss
/// (`..._API_KEY_ALT`, which nothing reads) is not one of these and still gets the ordinary
/// refusal with its suggestions — that name is a typo of a settable key, and this one is not.
fn labelled_account(key: &str) -> Option<(&str, &str)> {
    let (base, label) = key.split_once(vike_model::account_keys::ACCOUNT_SEPARATOR)?;
    vike_model::credential_keys::key_owner(base).is_some().then_some((base, label))
}

/// Levenshtein distance, two rows. Written out rather than pulled in: this crate's identity is
/// adding no dependency, and the whole algorithm is nine lines.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The durable half of [`run_set`]: ONE `credential_write` record for the key that was just
/// written.
///
/// ⚠ **It appends DIRECTLY rather than through `vike_connections::save_credentials_journalled`, the
/// wrapper the two GUI sites share, and that is a LAYERING verdict rather than a preference.** The
/// natural move is to hoist that wrapper into a crate below both surfaces — the narrowest one that
/// can see `vike_model::change_journal` and `vike_secrets` is `vike-bridge-core` (layer 30, which
/// both this crate and `vike-connections` already link). It was rejected because `vike-app-core`,
/// the wrapper's OTHER caller, has no `vike-bridge-core` edge at all: the hoist would ADD a
/// dependency edge to a crate that is not asking for one, in order to spare this file eleven lines.
/// `crates/bridges/ctrader/src/token_store.rs`'s `record_rotation` reached the same conclusion from
/// the other side of the graph (layer 40, headless), and says so in its own doc.
///
/// What keeps the spellings honest is not a shared function but a GATE:
/// `crates/vike-ops/tests/credential_writer_gate.rs` pins the EXACT set of files that call
/// `save_credentials` or its journalled wrapper, with a reason per row, so a THIRD writer cannot
/// appear unnoticed and a row whose caller is gone cannot linger.
///
/// ⚠ Nothing here can put a credential in the ledger:
/// `vike_model::change_journal::Change::credential_write` takes no old/new/value parameter at all,
/// and the only cell fed to it from this write is the key NAME.
fn record_write(ctx: &Ctx<'_>, store: &Path, key: &str, venue: &str, tier: Option<&str>) {
    use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome, Proc};

    // No project above the working directory ⇒ NO ledger. Nothing is recorded, rather than an
    // append-only record in a guessed directory — `vike_boot::journal_boot_settings`' rule.
    let Some(state_dir) = ctx.state_dir else { return };
    let journal = ChangeJournal::in_state_dir(state_dir, Proc::current(env!("CARGO_PKG_VERSION")));
    // The FILE NAME, not the path: `CredentialTarget::store` is documented as `"secrets.env"`, and
    // the ledger sits under the same `<project>/settings` the store does.
    let file = store.file_name().and_then(|n| n.to_str()).unwrap_or(SECRETS_FILE);
    // An ATTRIBUTION key has no tier — a broker/builder code is per-venue — so the cell is empty
    // rather than carrying a tier that was never part of the name.
    let change = Change::credential_write(
        Outcome::Applied,
        Actor::cli("vike-cli"),
        file,
        venue,
        tier.unwrap_or(""),
        &[key],
    );
    if let Err(e) = journal.append(ctx.now_ms, &change) {
        // stderr, and this crate has no `tracing` subscriber to reach for. The key IS saved, so
        // this is a finding about the ledger and not about the write.
        eprintln!(
            "vike-cli secrets: ⚠ {key} was saved, but the change journal in {} could not record \
             it: {e}",
            journal.dir().display()
        );
    }
}

#[cfg(test)]
mod set_tests {
    use super::*;

    fn parse_of(argv: &[&str]) -> Result<Args, String> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    /// The two ACCEPTED forms parse, and the key is the one positional.
    #[test]
    fn the_two_value_forms_parse() {
        let a = parse_of(&["set", "BINANCE_LIVE_API_KEY"]).unwrap();
        assert_eq!(a.sub, Sub::Set);
        assert_eq!(a.key.as_deref(), Some("BINANCE_LIVE_API_KEY"));
        assert_eq!(a.from_env, None);

        let b = parse_of(&["set", "BINANCE_LIVE_API_KEY", "--from-env", "SRC"]).unwrap();
        assert_eq!(b.from_env.as_deref(), Some("SRC"));
        // …and the flag may lead, so a flags-first habit keeps working.
        let c = parse_of(&["set", "--from-env=SRC", "BINANCE_LIVE_API_KEY"]).unwrap();
        assert_eq!(c.key.as_deref(), Some("BINANCE_LIVE_API_KEY"));
        assert_eq!(c.from_env.as_deref(), Some("SRC"));
    }

    /// **A VALUE IN ARGV IS REFUSED, in both spellings, and the refusal quotes NOTHING.**
    ///
    /// The last assertion is the load-bearing one: a message that echoed the rejected token would
    /// write the credential into the scrollback of the session this refusal exists to keep it out
    /// of — the refusal doing the exact damage it was built to prevent.
    #[test]
    fn a_value_in_argv_is_refused_without_echoing_it() {
        for argv in [
            &["set", "BINANCE_LIVE_API_KEY", "sk-live-do-not-print"][..],
            &["set", "BINANCE_LIVE_API_KEY=sk-live-do-not-print"][..],
        ] {
            let err = parse_of(argv).expect_err("a value in argv must be refused");
            assert!(err.contains("may not be given on the command line"), "{err}");
            assert!(err.contains("--from-env"), "the refusal must show both accepted forms: {err}");
            assert!(err.contains("stdin"), "{err}");
            assert!(!err.contains("sk-live-do-not-print"), "the refusal ECHOED the value: {err}");
        }
    }

    /// **…including a value that BEGINS WITH A DASH**, which is the input class the two spellings
    /// above could not reach and which the generic `unknown option '{other}'` arm ECHOED verbatim.
    ///
    /// base64url alphabets contain `-`, so a real credential starting with one is ordinary rather
    /// than contrived, and stderr is the stream CI logs and every service manager captures. The
    /// refusal was writing the secret into the record it exists to keep it out of.
    #[test]
    fn a_dash_leading_value_is_refused_without_echoing_it_either() {
        for argv in [
            &["set", "BINANCE_LIVE_API_KEY", "-sk-live-do-not-print"][..],
            &["set", "BINANCE_LIVE_API_KEY", "-sk=live-do-not-print"][..],
            // …and with no key yet parsed, where it must NOT be taken as the key: that path reaches
            // `unknown_key_message`, which names the key it was given — the same echo, one step on.
            &["set", "-sk-live-do-not-print"][..],
        ] {
            let err = parse_of(argv).expect_err("a dash-leading value must be refused");
            assert!(
                !err.contains("sk-live-do-not-print") && !err.contains("live-do-not-print"),
                "the refusal ECHOED the value: {err}"
            );
            assert!(err.contains("may not be given on the command line"), "{err}");
        }
    }

    /// **A mistyped LONG FLAG is refused without being quoted back either**, and the refusal points
    /// at the usage the caller prints beneath it.
    ///
    /// The first fix here exempted a leading `--` so a `--form-env` slip could be named. A PEM
    /// key begins `-----BEGIN`, which starts with `--`, and was echoed in full — so any rule that
    /// reads the token's own SHAPE is guessing about the secret's alphabet. The cost is this: on
    /// `set`, a flag typo reads as a value refusal.
    #[test]
    fn a_mistyped_flag_is_refused_without_being_quoted_back() {
        let err = parse_of(&["set", "BINANCE_LIVE_API_KEY", "--form-env", "X"]).unwrap_err();
        assert!(!err.contains("--form-env"), "even a flag typo is not quoted back: {err}");
        assert!(err.contains("if you meant a FLAG"), "…but the operator is pointed at them: {err}");

        // The other subcommands are UNCHANGED: they have no secret in argv to protect, so a typo
        // there is still named, which is the more useful answer.
        assert!(parse_of(&["list", "--jsonn"]).unwrap_err().contains("--jsonn"));
    }

    /// **`--file` is refused on the WRITER**, and permitted on the three readers.
    ///
    /// It used to resolve the same way for both, so `set KEY --file <any existing file>` appended a
    /// live credential to whatever the operator named — a shell rc file, another program's `.env` —
    /// exiting 0, with the change journal recording the write against a "store" of that file's
    /// basename. `docs/decisions/0036` fixes this verb as an upsert into the PROJECT's store.
    #[test]
    fn the_file_flag_is_refused_on_set_and_kept_on_the_readers() {
        let err =
            parse_of(&["set", "BINANCE_LIVE_API_KEY", "--file", "/tmp/anything"]).unwrap_err();
        assert!(err.contains("--file"), "{err}");
        assert!(err.contains("VIKE_SETTINGS_DIR"), "the refusal must name the way through: {err}");

        for sub in ["list", "path", "template"] {
            assert!(
                parse_of(&[sub, "--file", "/tmp/anything"]).is_ok(),
                "{sub} must keep --file: inspection is not a write"
            );
        }
    }

    /// **A LABELLED ACCOUNT is refused with NO suggestions**, because the nearest name is the
    /// DEFAULT account's key — real, settable, and a different account. Offering it invited the
    /// operator to overwrite the credential their primary account signs with.
    #[test]
    fn a_labelled_account_is_refused_without_offering_the_default_accounts_key() {
        // Composed off a REAL key rather than spelled — see the sibling test for the literal
        // harvest that avoids.
        let base = vike_model::credential_keys::lookup_keys()
            .into_iter()
            .find(|k| k.ends_with("_API_KEY"))
            .expect("the grid has an API-key row");
        let labelled = format!("{base}{}ALT", vike_model::account_keys::ACCOUNT_SEPARATOR);

        assert!(
            nearest_keys(&labelled).is_empty(),
            "a labelled account must suggest nothing: {:?}",
            nearest_keys(&labelled)
        );
        let msg = unknown_key_message(&labelled);
        assert!(msg.contains("LABELLED ACCOUNT"), "{msg}");
        assert!(msg.contains("ALT"), "the message must name the label it read: {msg}");
        assert!(msg.contains("Do NOT set"), "the message must warn AGAINST the base key: {msg}");
        assert!(
            !msg.contains("did you mean"),
            "a labelled account must offer no substitute: {msg}"
        );

        // …and a SINGLE-underscore near-miss is NOT one of these: it is a typo of a settable key,
        // and it keeps its suggestions.
        let near_miss = format!("{base}_ALT");
        assert!(nearest_keys(&near_miss).contains(&base), "a typo still gets its suggestion");
        assert!(labelled_account(&near_miss).is_none());

        // A label on a name that is NOT a credential key falls through to the ordinary refusal.
        assert!(labelled_account("NOT_A_KEY__ALT").is_none());
    }

    /// `set` with no key is a usage error that shows both forms — the operator who typed it is
    /// exactly the one who does not yet know how the value gets in.
    #[test]
    fn set_without_a_key_names_both_value_forms() {
        let err = parse_of(&["set"]).unwrap_err();
        assert!(err.contains("needs a credential KEY"), "{err}");
        assert!(err.contains("--from-env") && err.contains("stdin"), "{err}");
    }

    /// A positional on a READING subcommand is still `unknown option`, unchanged — the non-flag arm
    /// is gated on `set` alone.
    #[test]
    fn a_positional_on_a_reading_subcommand_is_unchanged() {
        for sub in ["list", "path", "template"] {
            let err = parse_of(&[sub, "stray"]).unwrap_err();
            assert!(err.contains("unknown option"), "{sub}: {err}");
        }
        assert!(parse_of(&["list", "--from-env", "X"]).unwrap_err().contains("--from-env"));
    }

    /// **An unknown key is refused BY NAME, and the message points at real ones.**
    ///
    /// The lower-case case is separate because it is the likeliest near-miss and Levenshtein scores
    /// it as far away as a different venue — every letter differs.
    #[test]
    fn an_unknown_key_is_refused_by_name_with_the_nearest_real_ones() {
        // ⚠ The near-misses are COMPOSED off a REAL key rather than spelled, for the reason
        // `vike_model::credential_keys`' own `key_owner_classifies_exactly_the_lookup_grid` gives:
        // `crates/vike-ops/tests/settings_registry.rs`'s literal harvest reads an env-shaped string
        // literal as evidence this crate READS that variable and demands a `SETTINGS` row for it.
        // This command reads no credential at all — it writes one the caller hands it — so a row
        // here would assert something false about `vike-cli`.
        let real = vike_model::credential_keys::lookup_keys()
            .into_iter()
            .find(|k| k.ends_with("_API_KEY"))
            .expect("the grid has an API-key row");
        let truncated = &real[..real.len() - 1];
        let extended = format!("{real}X");

        let msg = unknown_key_message(truncated);
        assert!(msg.contains(truncated), "the refusal must name the key: {msg}");
        assert!(msg.contains(&real), "…and suggest the real one: {msg}");

        assert_eq!(nearest_keys(&real.to_lowercase()), vec![real.clone()]);
        assert!(nearest_keys(&extended).contains(&real));

        // Nonsense suggests NOTHING, and says where the whole grid is instead. Three unrelated
        // names presented as guesses is worse than the bare refusal.
        assert!(nearest_keys("totally-unrelated-nonsense").is_empty());
        let far = unknown_key_message("totally-unrelated-nonsense");
        assert!(far.contains("secrets template"), "{far}");
        assert!(!far.contains("did you mean"), "{far}");

        // …and every real key is accepted, which is the other half of the same claim.
        for key in vike_model::credential_keys::lookup_keys() {
            assert!(
                vike_model::credential_keys::key_owner(&key).is_some(),
                "{key} is in the grid and must be settable"
            );
        }
    }

    /// The refusal for a name NOTHING reads also names the bespoke families it cannot set, rather
    /// than leaving an operator to conclude the command is broken. They are a real gap —
    /// `vike_model::credential_keys`'s own module doc calls it structural — and a gap stated is not
    /// a gap hidden.
    #[test]
    fn the_refusal_names_the_shapes_that_are_outside_the_grid() {
        // A name no registry row carries, so this is the arm that still prints the original
        // sentence. Its shape matters: the tail is what the operator gets INSTEAD of a route.
        let msg = unknown_key_message("totally-unrelated-nonsense");
        assert!(msg.contains("BESPOKE"), "{msg}");
        assert!(msg.contains("LABELLED"), "{msg}");
        assert!(msg.contains("secrets path"), "the operator must be told where to edit: {msg}");
    }

    /// **A key something READS is never called unread** — the defect this arm exists for, measured
    /// on the name it was measured on.
    ///
    /// `vike-cli secrets set VIKE_TRADEHUB_OBSERVE_KEY` answered "is not a credential key this
    /// workspace reads, so setting it would write a line nothing would ever load". Both halves were
    /// false: `crates/vike-tradehub/src/tradehub_cli.rs`'s `start_observe_server` reads that exact
    /// name through `vike_tradehub_client::auth`'s `from_vars`, and the registry carries rows for
    /// it. The refusal STANDS — `set` writes the grid and nothing wider, which
    /// `docs/decisions/0036` fences — but it now says why, and points at the route that exists.
    ///
    /// ⚠ The name is COMPOSED rather than spelled, for the reason
    /// `vike_model::credential_keys`' own `key_owner_classifies_exactly_the_lookup_grid` gives:
    /// `crates/vike-ops/tests/settings_registry.rs`' literal harvest reads an env-shaped literal in
    /// a `src/` file as evidence this crate READS that variable. `vike-cli` does read this one —
    /// `cmd/nodekeys.rs` owns that, and has its own row — but this file must not become a second
    /// sighting of it, and the same dodge keeps every name below out of the sweep too.
    #[test]
    fn a_key_something_reads_is_never_called_unread() {
        // ⚠ THE DATAHUB PAIR LEFT THIS TEST on 2026-09-08 and that is the change, not a regression.
        // It sat here because it fell through to the outside-the-grid arm — read by the workspace,
        // settable by nothing, edited in by hand. `PLATFORM_KEYS` now carries it, so it takes the
        // PLATFORM arm and is covered by `the_node_keys_refusal_names_the_command_that_mints_them`
        // instead. The property this test states is unchanged; the pair simply has a route now, and
        // a test asserting it is still told to use an EDITOR would be pinning the defect.
        let msg = unknown_key_message(&format!("VIKE_{}", "TELEGRAM_BOT_TOKEN"));
        assert!(!msg.contains("nothing would ever load"), "the measured lie is back: {msg}");
        assert!(msg.contains("IS read by this workspace"), "{msg}");
        // WHAT reads it, from the registry rather than from prose here.
        assert!(msg.contains("vike-tradehub"), "the refusal must name a reader: {msg}");
        // …and the route that exists TODAY. No command is named that cannot be run.
        assert!(msg.contains("secrets path"), "{msg}");
        assert!(msg.contains("EDITOR"), "{msg}");

        // The same for every other name found in this position: the Telegram trio, and a BESPOKE
        // venue login — which the old text contradicted itself about, calling it unloadable in one
        // sentence and pointing at its bridge's loader in the next.
        for key in [
            format!("VIKE_{}", "TELEGRAM_ALLOWED_CHAT_IDS"),
            format!("VIKE_{}", "TELEGRAM_ALLOWED_USER_IDS"),
            format!("FXCM_{}", "DEMO_USER"),
        ] {
            let msg = unknown_key_message(&key);
            assert!(!msg.contains("nothing would ever load"), "{key}: {msg}");
            assert!(msg.contains(&key), "{key}: the refusal must name the key: {msg}");
        }
    }

    /// **The two TRADEHUB node keys get a FOURTH message, and it names a command rather than an
    /// editor.** They were the specimen the read-but-not-settable arm was written for, and until
    /// `vike-cli backend setup` existed the honest advice really was "open the file" — there was no
    /// generator anywhere in this tree, and two ops runbooks recorded "a freshly generated" key with
    /// no command beside it.
    ///
    /// Now there is one, and sending an operator to an editor would be the SAME class of defect the
    /// arm above was built to end: correct about the refusal, wrong about the route. The message
    /// must name both boxes, because which command you want depends on which one you are standing
    /// at, and it must not offer the editor as an alternative — a hand-pasted 256-bit key that is
    /// truncated fails as an opaque auth denial.
    ///
    /// ⚠ The names are COMPOSED, for the reason [`a_key_something_reads_is_never_called_unread`]
    /// gives above: an env-shaped literal in a `src/` file is read by the settings registry's
    /// harvest as evidence this crate READS that variable.
    #[test]
    fn the_node_keys_refusal_names_the_command_that_mints_them() {
        // ⚠ FOUR names now, and the verb is chosen PER SERVICE. While there was one pair this arm
        // could name the tradehub verb unconditionally; with two, a constant would send an operator
        // to the command for a different service — the same defect the arm exists to end, which is
        // why the datahub pair is exercised here rather than trusted.
        for (key, verb, service) in [
            (format!("VIKE_{}", "TRADEHUB_OBSERVE_KEY"), "backend", "vike-tradehub"),
            (format!("VIKE_{}", "TRADEHUB_CONTROL_KEY"), "backend", "vike-tradehub"),
            (format!("VIKE_{}", "DATAHUB_OBSERVE_KEY"), "datahub", "vike-datahub"),
            (format!("VIKE_{}", "DATAHUB_CONTROL_KEY"), "datahub", "vike-datahub"),
        ] {
            // The table this arm keys on, asserted here too, so a drift shows up as this test
            // rather than as an operator quietly getting the wrong route.
            assert!(vike_model::credential_keys::is_platform_key(&key), "{key}");
            assert_eq!(
                vike_model::credential_keys::platform_key_service(&key),
                Some(service),
                "{key}: the classifier is what picks the verb"
            );
            let msg = unknown_key_message(&key);
            assert!(!msg.contains("nothing would ever load"), "{key}: {msg}");
            assert!(msg.contains(&key), "{key}: {msg}");
            assert!(msg.contains("IS read by this workspace"), "{key}: {msg}");
            assert!(
                msg.contains(&format!("{verb} setup")),
                "{key}: it must name the minting command for ITS service: {msg}"
            );
            // ⚠ WHICH BOX — this read "DAEMON's box" while there was one daemon, and that stopped
            // being an answer the moment a second service existed. It names the service now.
            assert!(msg.contains(service), "{key}: which box, by service: {msg}");
            assert!(
                !msg.contains("EDITOR"),
                "{key}: the editor route is exactly what `{verb} setup` deletes: {msg}"
            );
            // ⚠ AND IT MAY NOT NAME A COMMAND THAT DOES NOT EXIST. `backend connect` is real;
            // `datahub connect` is not built, and an earlier draft of this arm promised it — a
            // refusal right about refusing and wrong about the route, which is the exact failure
            // this whole arm was written to end.
            assert!(
                !msg.contains("datahub connect"),
                "{key}: there is no `datahub connect` to send anyone to: {msg}"
            );
        }
        // The tradehub half DOES have a client command, and the message still offers it.
        let th = unknown_key_message(&format!("VIKE_{}", "TRADEHUB_OBSERVE_KEY"));
        assert!(
            th.contains("backend connect"),
            "the tradehub's client half exists and is named: {th}"
        );
        // …and the arm is NARROW: a name one letter off is not a platform key and still gets the
        // ordinary outside-the-grid refusal, editor and all.
        let near = format!("VIKE_{}", "TRADEHUB_OBSERVE_KEYS");
        assert!(!vike_model::credential_keys::is_platform_key(&near));
        assert!(!unknown_key_message(&near).contains("backend setup"), "{near}");
    }

    /// …and the same claim as a PROPERTY over the whole registry, so the arm cannot be right for
    /// the seven names above and wrong for the next one added.
    ///
    /// Both directions, because either alone is satisfiable by a message that says nothing: every
    /// declared name outside the grid must be told it is read and by whom, and a name the registry
    /// does NOT carry must still get the original sentence — that sentence is correct there, and
    /// deleting it would trade one lie for a vaguer one.
    #[test]
    fn the_unread_sentence_is_printed_only_where_no_registry_row_names_the_key() {
        let mut checked = 0usize;
        for s in SETTINGS {
            // Grid keys never reach this message at all — `key_owner` accepts them and `set`
            // writes them.
            if vike_model::credential_keys::key_owner(s.name).is_some() {
                continue;
            }
            let msg = unknown_key_message(s.name);
            assert!(
                !msg.contains("nothing would ever load"),
                "{} has a registry row and must not be called unread: {msg}",
                s.name
            );
            assert!(msg.contains(s.krate), "{}: the refusal must name {}: {msg}", s.name, s.krate);
            checked += 1;
        }
        assert!(checked > 0, "the registry carries no non-grid rows — this test proved nothing");

        // The other direction. `registry_readers` is the whole discriminator, so a name it answers
        // `None` for is exactly where the original sentence still belongs.
        let unread = "totally-unrelated-nonsense";
        assert!(registry_readers(unread).is_none());
        assert!(unknown_key_message(unread).contains("nothing would ever load"));
    }

    /// **A MULTI-LINE `--from-env` value is refused, and a WHITESPACE-ONLY one with it.**
    ///
    /// The multi-line case was an INJECTION, not an untidiness: quoted and joined by
    /// `vike_secrets::upsert_env`, the value's own newline became a physical line break, so the
    /// reader returned the first half as a truncated credential and read the second half as a WHOLE
    /// NEW `KEY=VALUE` — a credential for a venue the operator never configured, past a key name
    /// this command had validated. Reproduced end to end before this refusal existed; the store
    /// afterwards listed three keys and three accounts where two had been set.
    ///
    /// The whitespace-only case is milder and the same shape: `parse_dotenv` hands three spaces back
    /// as a non-empty value, so the venue reads as CONFIGURED and arms with a garbage secret,
    /// failing at the venue instead of staying on paper — while `value_for`'s own doc said "both
    /// refuse EMPTY" and only the stdin arm looked past a zero length.
    ///
    /// It is asserted HERE, at the seam that names the variable, as well as in
    /// `vike_secrets::env_write`, which refuses it for every caller including the GUI.
    #[test]
    fn a_multiline_or_blank_env_value_is_refused_naming_the_variable_and_never_the_value() {
        let args = Args {
            sub: Sub::Set,
            file: None,
            venue: None,
            json: false,
            key: Some("BINANCE_LIVE_API_KEY".to_string()),
            from_env: Some("SRC".to_string()),
        };
        let refusal = |raw: &str| -> String {
            let env = HashMap::from([("SRC".to_string(), raw.to_string())]);
            let ctx = Ctx {
                settings_dir: None,
                settings_dir_override: None,
                state_dir: None,
                env: &env,
                now_ms: 0,
            };
            value_for(&args, &ctx).expect_err("must be refused").msg
        };

        for raw in ["abc\nOKX_LIVE_API_SECRET=injected", "tok\n", "tok\r"] {
            let msg = refusal(raw);
            assert!(msg.contains("more than one line"), "{msg}");
            assert!(msg.contains("SRC"), "the refusal must name the VARIABLE: {msg}");
            assert!(!msg.contains("injected") && !msg.contains("tok"), "it ECHOED a value: {msg}");
        }
        for blank in ["", "   ", "\t"] {
            let msg = refusal(blank);
            assert!(msg.contains("unset, empty or only whitespace"), "{msg}");
            assert!(msg.contains("SRC"), "{msg}");
        }

        // …and an ordinary one-line value still passes through VERBATIM, including the leading and
        // trailing whitespace this arm deliberately does not trim.
        let env = HashMap::from([("SRC".to_string(), " tok ".to_string())]);
        let ctx = Ctx {
            settings_dir: None,
            settings_dir_override: None,
            state_dir: None,
            env: &env,
            now_ms: 0,
        };
        assert_eq!(value_for(&args, &ctx).unwrap(), " tok ");
    }

    #[test]
    fn edit_distance_is_the_ordinary_one() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("abc", ""), 3);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn usage_documents_the_writer_and_both_of_its_value_forms() {
        for needle in ["set KEY", "--from-env", "stdin", "template"] {
            assert!(USAGE.contains(needle), "USAGE must mention {needle}");
        }
        // ⚠ The USAGE text must not teach the shape it refuses. `set KEY VALUE` appearing here as
        // an example is how somebody learns to type it.
        assert!(!USAGE.contains("set KEY VALUE"), "USAGE must not show the argv form");
    }
}
