//! `vike-cli config set <key> <value>` — **write ONE setting into this box's settings files**, and
//! record it in the change journal.
//!
//! # What commissioned it, measured
//!
//! The change journal on a live box held, across two months, twenty-nine boot records and
//! twenty-eight venue mounts and **zero settings writes** — while the credential store's own backup
//! files proved it had changed at least eight times. The journal was never broken: the only
//! journalling settings writers an operator could reach were two `backend` sub-verbs, each writing
//! one fixed key. Every real change was made with an editor over ssh, which no software can see.
//! This verb is the missing writer, and the whole of it is a call to `vike_config::set_setting`
//! through `crate::cmd::settings_write` — the path `backend connect` already proves works.
//!
//! It also answers the owner's standing complaint that an operator who must ssh in and hand-edit
//! TOML is an operator who leaves.
//!
//! # The surface
//!
//! ```text
//! vike-cli config set <dotted.key> <value> [--confirm <dotted.key>]
//! ```
//!
//! - The KEY is the dotted spelling `vike-cli config show` renders, and its first segment picks the
//!   file (`policy.` / `config.` / `preferences.` / `flags.`). **There is deliberately no `--file`
//!   flag**: the key already names the file, and an operator-supplied destination PATH is one of
//!   the things
//!   `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` names as
//!   re-deciding it from the top. That record's SUBJECT is the credential store and this verb
//!   touches none of it (see *The fence* below) — but the reason transfers whole.
//! - The VALUE is an ordinary command-line argument, and that is a decision rather than an
//!   oversight. The credential surface refuses a value in argv because argv is visible in `ps` to
//!   every user on the box, and a venue key read out of a process listing is a stolen key. A
//!   settings value is not secret — a notional ceiling, a log level, a bind address — so argv costs
//!   nothing there, and a settings verb that could not take one would be unscriptable. The property
//!   that keeps the two apart is mechanical rather than remembered: this verb REFUSES a
//!   credential-shaped key outright (`vike_config::is_secret_key`), so a value that would have
//!   needed hiding never reaches the command line in the first place.
//! - `--confirm` is the retyped-key ceremony, demanded for the policy RISK CEILINGS and for the
//!   live-arm / safety-override FLAGS, and for nothing else. See *The confirm* below.
//!
//! # The fence: this verb does not touch the credential store
//!
//! `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` is about the
//! credential store, and three mechanical facts put this verb outside it: it calls
//! `vike_config::set_setting`, never `save_credentials` (the gate that polices credential writers
//! detects one by those function NAMES); it writes the settings TOMLs and never the two `.env`
//! stores; and the key it will accept is gated by `is_secret_key`, so a credential-shaped name is
//! refused at the door. Three of that record's rules are honoured anyway, because they were paid
//! for: no operator-supplied destination path, one journal record per write, and no secret in argv.
//!
//! What it does NOT inherit is that record's MCP rule, which is credential-scoped. Whether a local
//! settings writer belongs on the agent surface is a separate question and the answer here is NO
//! for now: `crate::cmd::mcp` already advertises a `set_setting` tool, but that one is a
//! `WireCommand` aimed at a REMOTE node behind a mandatory preview token, and an agent editing the
//! box's own `policy.toml` with no node and no preview in front of it is new ground. It is not
//! advertised by this verb and adding it later is its own one-paragraph ruling.
//!
//! ⚠ **That ruling scoped itself to `tools_spec` and there is a SECOND agent-facing channel it did
//! not name: the generated skill pages.** `skills/*/SKILL.md` are rendered from
//! `crates/vike-cli/src/lib.rs`'s `COMMANDS`, so changing the `config` summary put "a journalled
//! one-key write (`set`)" in front of an agent on three pages as a side effect — inherited rather
//! than decided, and the same prose channel 0036's *The question the instructions field asked*
//! section reasons about (text an agent acts on, with no preview and no gate in front of it).
//!
//! **The decision, made rather than inherited: the skill pages MAY name it, and the whole of the
//! argument is the one about DESCRIPTION vs CALLABLE.** A skill page describes a command an agent
//! would have to run through a shell it was separately granted; `tools_spec` is a callable the MCP
//! server offers and the agent may invoke directly. Withholding the name from the description
//! removes no capability — anything that can run `vike-cli` can run `config set` — it only makes
//! the surface less discoverable than it is, which is the "security by not writing it down" shape
//! this tree refuses elsewhere. **The containment is the shell grant, not this verb**, and the
//! ruling should not be read as claiming otherwise. What stays refused is the MCP TOOL — a callable
//! with no shell in front of it.
//!
//! ⚠ **That ruling first rested on three "properties that make the verb safe", and the
//! load-bearing one was FALSE on a deployed box.** It read *"every key it writes is restart-to-apply
//! so nothing it does takes effect in a running process"*. The first half is true and the inference
//! is not: restart-to-apply is a DELAY, and nothing in this system requires a human to end it.
//! `deploy/vike-tradehub.service` carries `Restart=on-failure` with `RestartSec=5`, so the
//! LIVE order-signing daemon restarts itself on any crash or OOM kill; `deploy/sbin/
//! vike-trader-ci-deploy` restarts its whole roster unattended on a green tag; and the box's
//! unattended-upgrade window restarts services daily. A flag written at 03:00 is therefore in force
//! at the next one of those, with nobody present, and the operator who would have noticed is the
//! one who never typed a restart. A property that holds only until the next crash is not a gate.
//!
//! What actually holds, and is all this file claims:
//!
//! 1. **No credential value passes through it.** `vike_config::is_secret_key` refuses a
//!    credential-shaped KEY at the shared writer, the store is a different file with a different
//!    writer, and the parse refusals quote nothing that might be one.
//! 2. **Every write lands one `vike_model::change_journal` row naming the actor** — and every
//!    REFUSAL lands one too, so "did something try this" has an answer either way. That is
//!    ATTRIBUTION AFTER THE FACT, not prevention, and the verb exists precisely because every
//!    settings change on the live box used to be invisible.
//! 3. **The keys with a blast radius cannot be written in one un-retyped line** — the policy risk
//!    ceilings and, since this finding, the live-arm and safety-override flags
//!    (`vike_config::typed_confirm_reason`, which is where that class is listed and argued).
//!    ⚠ Read that as what it is: a retype stops a MISTAKE, not an intention. `--confirm <key>` is
//!    one more argument for a human and for an agent alike, and nothing here is a barrier to
//!    something that meant it.
//! 4. **Nothing takes effect in the process that is running right now** — stated as the TIMING fact
//!    it is, which is what the operator needs after a write, and not as a safety property.
//!
//! # The confirm
//!
//! A policy RISK CEILING (`max_notional_per_order` and its siblings) demands the same ceremony
//! every other write path in this tree demands: retype the exact dotted key. The ARMING ceilings
//! (`policy.venues.<venue>`, `policy.accounts.<venue>.<LABEL>`) do not, which is the ruling the
//! *every setting is editable* record makes and the line no existing site can draw — the daemon's
//! `apply_set_setting` and the GUI's `can_save_fields` both key on the FILE, and the two classes
//! share one. `vike_config::typed_confirm_reason` is that line, drawn on the KEY, in the crate
//! all three surfaces already depend on.
//!
//! ⚠ **And a file-keyed rule cannot see the other direction either, which is how this verb shipped
//! its first asymmetry.** The ceremony covered `policy.*` and nothing else, so
//! `policy.max_leverage` — a field nothing in the tree reads, as this verb's own READ verdict says
//! — demanded a retype while `flags.tradehub_live`, whose own doc calls it *"the single largest
//! blast radius on this list"*, and `flags.tradehub_control`, which OPENS the daemon's remote
//! order-origination surface, were one-line non-interactive writes. Both live in `flags.toml`, so
//! no file-keyed rule anywhere in the tree could have drawn that line. The predicate now carries a
//! flags class whose membership rule is *the reason it renders* — a flag that puts real money on a
//! real wire or turns off a default-on guard — and the list, the reason per row and the residual
//! are at `vike_config::write`'s `CONFIRMED_FLAG_KEYS` rather than restated here.
//!
//! ⚠ Read that as a CORRECTION and not as a summary: this paragraph sourced the class from
//! `crates/vike-config/src/flags.rs`'s declared safety OVERRIDES *"plus the live arm"*, which is a
//! strictly narrower set than the sentence the operator is shown — and the flag it left outside was
//! `flags.tradehub_control`, while the flag that merely widens THAT surface's bind address was
//! inside. The rule and the rendered reason are one claim now; the const is the authority.
//!
//! The obvious objection — *a local write does not go through the daemon, so the confirm is
//! ceremony this surface can skip* — inverts the argument. `crate::cmd::trade`'s `typed_key_confirm`
//! compares CLIENT-side on purpose: "the value sent is a value the operator TYPED… the friction IS
//! the protection". It was never a protocol property to be inherited from a peer, and a local write
//! is if anything more exposed, since nothing downstream re-asks. Dropping it here would make this
//! the ONE surface on which a risk ceiling moves with no ceremony at all.
//!
//! Two spellings, because a CLI must be scriptable and an operator must not be able to satisfy a
//! confirm by pressing return: `--confirm <key>`, compared for exact equality, and — when the flag
//! is absent and stdin is a terminal — an interactive retype. Neither is ever pre-filled from the
//! key the parser already holds; that is the one rule all four existing surfaces share.
//!
//! # What the operator is told afterwards, and why it is not a hot/cold verdict
//!
//! **Every local write is restart-to-apply**, and the verb says so in words. That is true by
//! construction rather than by classification: the only hot-apply path in this tree is the daemon's
//! WIRE arm, and nothing in the tree watches a settings file
//! (`crates/vike-tradehub/src/tradehub_cli.rs`'s boot anchor states it outright). Naming which keys
//! a running node could take live would mean reading
//! `crates/vike-tradehub/src/hot_reload.rs`'s `CLASSIFICATION`, and this crate may not: both crates
//! declare architecture layer 65 and a normal dependency must declare a STRICTLY lower one. Copying
//! the rows down instead would put a second copy of a table in the crate that cannot see the first.
//!
//! ⚠ **So it states the BOUND, and used to imply a set.** The line read "a running node can take a
//! few keys live over its control channel", which for all but two keys in the tree sends an
//! operator on a wild goose chase: `classify` answers `HotClass::Restart` for the whole policy file
//! before its table is consulted, every `SettingsFile::Config` row is `Restart`, and the only
//! `HotClass::Hot` rows are `preferences.log_level` and `preferences.log_file_level`. Somebody who
//! has just written `policy.max_notional_per_order` at 03:00 was being pointed at a route that
//! would also have needed a restart — while the two keys where it genuinely works (including the
//! file level whose restart gap has a measured 341 GB cost) got the same vague sentence. Saying
//! "only the two log-level preferences" names no table row and copies nothing down a layer; it is a
//! COUNT of an upper bound, which is the one thing a lower crate can state about a higher one's
//! table without duplicating it. If that bound ever grows, this sentence is wrong in the SAFE
//! direction — it under-promises.
//!
//! It also prints the key's READ verdict, from the same `vike_config::CONSUMPTION` table
//! `config show` renders. A control over a key nothing reads is worse than no control — it is
//! positive confirmation that the operator has looked — and that failure is precisely what the
//! editability ruling names as a condition for reopening itself. One call, one line, and the write
//! still happens: refusing a key the loader accepts would make this the first place in the tree
//! where the verb and `deny_unknown_fields` disagree.

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

use vike_config::{Consumer, SettingsFile, SettingsWrite};

use crate::cmd::args::{self, exit_for_parse_error};
use crate::cmd::config_check::DirOrigin;
use crate::cmd::settings_write;
use crate::exit::{CliError, CmdResult};

/// The usage line, printed by `config set --help` and by every parse refusal.
pub(crate) const SET_USAGE: &str = "\
usage: vike-cli config set <key> <value> [--confirm <key>]

  <key>      the dotted key `vike-cli config show` renders (policy.* | config.* |
             preferences.* | flags.*) — its first segment picks the file
  <value>    the new value, parsed as TOML when it is one (250, true, \"text\") and taken as a
             string otherwise. Never a credential: a secret-shaped key is refused, and
             `vike-cli secrets set` is the surface that writes those (stdin, never argv)
  --confirm  retype the key EXACTLY. Required for a policy RISK CEILING and for a live-arm
             or safety-override flag (flags.tradehub_live and its class); on a terminal the
             verb asks instead. The arming ceilings (policy.venues.*, policy.accounts.*)
             need no confirm
  --         end of options: everything after it is a positional, verbatim. Use it for a
             value that begins with `--` (`config set preferences.x -- --dark`)";

/// Everything this verb needs from outside itself — the dispatcher's ONE boot walk and its ONE
/// clock read. `crate::cmd::secrets`' `Ctx` carries the argument for the shape.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    /// The settings directory, as the boot resolved it — the destination, and the reason this verb
    /// needs no `--file`.
    pub settings_dir: Option<&'a Path>,
    /// WHICH rung resolved that directory. A READ verb wants it
    /// (`crate::cmd::config_check::DirOrigin`'s own doc says so); a WRITE verb needs it more.
    /// `crate::cmd::config`'s `print_header` exists because "which project am I reading?" is, in
    /// its own words, the question behind every other question here — and this verb is the one that
    /// CHANGES the answer. The project walk has captured the wrong tree three times on record
    /// (#1089 the crate-directory match, #1101 the stranger's manifest, and the `/opt/vike`
    /// deployment whose credentials went silent), and there are two project roots on the live box,
    /// so a confirmation reading `policy.toml: …` names nothing an operator can check at 03:00.
    pub origin: DirOrigin,
    /// That directory's `state` child, off the same walk: the change journal's home. `None`
    /// journals nothing and still writes.
    pub state_dir: Option<&'a Path>,
    /// The instant a journal record is stamped with, read once by the dispatcher because
    /// `vike_model::change_journal` reads no clock.
    pub now_ms: i64,
}

/// The parsed command line.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    key: String,
    value: String,
    confirm: Option<String>,
}

/// Entry point: parse, gate, write, report. `args` is everything after `config set`.
pub fn run(args: impl Iterator<Item = String>, ctx: Ctx<'_>) -> ExitCode {
    let parsed = match parse(args) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error("config set", SET_USAGE, &msg),
    };
    match execute(&parsed, &ctx) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli config set: {}", e.msg);
            e.exit.into()
        }
    }
}

/// PURE argv grammar — two positionals and one valued flag, so the whole of it is unit-tested.
///
/// ⚠ The VALUE positional is taken RAW and is never split on `=`, unlike the shared
/// [`crate::cmd::args::Flags`] iterator: `config set config.log_dir a=b` is a legitimate value, and
/// a value-splitting parser would silently write `a`. The flag half keeps the `--confirm=key` form
/// because a key contains no `=`.
///
/// ⚠ **`--` ends option parsing**, and everything after it is a positional verbatim. Without it a
/// settings VALUE cannot begin with `--` at all (`config set preferences.chart_style --dark` is
/// `unknown argument: --dark`), and worse, a value spelled `--help` prints usage and exits 0 — so a
/// scripted `config set <key> "$VALUE"` whose variable happened to hold `--help` SUCCEEDED while
/// changing nothing. One `--`, the convention every shell and every operator already knows, fixes
/// both; an unknown flag BEFORE it is still refused by name, which is what keeps
/// `there_is_no_way_to_name_a_destination_path` meaning what it says. The alternative — "stop
/// parsing flags once two positionals are collected" — was refused because it silently reclassifies
/// a mistyped trailing flag as a third positional, and a mistyped flag is the thing that most needs
/// to be named.
fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args::default();
    let mut positionals: Vec<String> = Vec::new();
    let mut end_of_options = false;
    let mut it = args;
    while let Some(arg) = it.next() {
        if end_of_options {
            positionals.push(arg);
            continue;
        }
        match arg.as_str() {
            "--" => end_of_options = true,
            "-h" | "--help" => return args::help_requested(),
            _ if arg.starts_with("--") => {
                let (flag, inline) = match arg.split_once('=') {
                    Some((f, v)) => (f.to_string(), Some(v.to_string())),
                    None => (arg.clone(), None),
                };
                match flag.as_str() {
                    "--confirm" => {
                        let value = match inline {
                            Some(v) => v,
                            None => {
                                it.next().ok_or_else(|| "--confirm requires a value".to_string())?
                            }
                        };
                        out.confirm = Some(value);
                    }
                    other => return Err(format!("unknown argument: {other}")),
                }
            }
            _ => positionals.push(arg),
        }
    }
    match positionals.len() {
        0 => return Err("a KEY is required (the dotted key `config show` renders)".to_string()),
        1 => return Err(missing_value_message(&positionals[0])),
        2 => {}
        _ => {
            return Err(format!(
                "exactly ONE key and ONE value (got {}) — quote a value containing spaces",
                positionals.len()
            ));
        }
    }
    out.value = positionals.pop().expect("two positionals");
    out.key = positionals.pop().expect("two positionals");
    Ok(out)
}

/// The one-positional refusal — **and the reason it is a function rather than a `format!`**.
///
/// The old wording echoed the operator's token VERBATIM, twice, to stderr. `KEY=VALUE` is the
/// dotenv muscle memory that produces a one-positional command line, so
/// `vike-cli config set BINANCE_LIVE_API_KEY=sk_live_…` printed the credential in full on the
/// stream CI logs and every service manager capture — the exact defect 0036's 2026-09-13 narrowing
/// amendment records ("a refusal that echoed a dash-leading token"), and the rule
/// `crate::cmd::secrets`' `ARGV_VALUE_REFUSAL` arm spends thirty lines of comment on: **a refusal
/// quotes nothing an operator typed** once the token might be a secret.
///
/// The module doc's fence argument — "a value that would have needed hiding never reaches the
/// command line" — is true of the WRITE path (the key is gated) and was false of this one, because
/// `refuse_a_secret_key` runs inside `execute`, i.e. after `parse`.
///
/// So the token is tested BOTH whole and pre-`=` (the `KEY=VALUE` slip puts the shape on the left
/// of the sign), and on a match nothing is quoted at all. `exit_for_parse_error` prints
/// [`SET_USAGE`] beneath either message, which is where an operator who merely mistyped reads the
/// spelling. The `config show --filter` pointer stays for an ordinary key, which is the only case
/// it helps.
fn missing_value_message(token: &str) -> String {
    let head = token.split_once('=').map(|(k, _)| k).unwrap_or(token);
    if vike_config::is_secret_key(token) || vike_config::is_secret_key(head) {
        return "that looks like a credential, and this verb neither writes one nor repeats one \
                back. Credentials live in the store, not in the settings files: `vike-cli secrets \
                set <KEY>` is the writer and it takes the value on stdin, never on the command \
                line. (If it was a settings key, `vike-cli config show` prints every key this verb \
                takes.)"
            .to_string();
    }
    format!(
        "a VALUE is required — `config set {token} <value>`. To READ a key instead, run \
         `vike-cli config show --filter {token}`"
    )
}

/// Which settings file a dotted key belongs to, refusing a first segment that names none of the
/// four. The refusal names all four sections AND the read verb, because an operator who does not
/// know the spelling needs the thing that prints it, not a list to guess from.
///
/// ⚠ **It also states its DISPOSITION**, which is the question this whole verb exists to answer:
/// can the operator tell which of write / refuse / partly-happened occurred? Four refusals reach an
/// operator from this verb and only two of them used to say. See [`execute`] for the other repair.
fn file_for(key: &str) -> CmdResult<SettingsFile> {
    let section = key.split('.').next().unwrap_or(key);
    SettingsFile::parse(section).ok_or_else(|| {
        CliError::usage(format!(
            "nothing was written — `{key}` does not name a settings file. A key is spelled \
             `policy.<key>`, `config.<key>`, `preferences.<key>` or `flags.<key>` (got first \
             segment `{section}`). `vike-cli config show` prints every key in the spelling this \
             verb takes."
        ))
    })
}

/// Resolve the typed confirm for `key`, or refuse.
///
/// `Ok(())` when no ceremony is owed. When one is: the `--confirm` flag must carry the key
/// EXACTLY; absent, a terminal is asked to retype it and a pipe is refused by name. Never
/// pre-filled from `key`.
fn resolve_confirm(key: &str, supplied: Option<&str>) -> CmdResult<()> {
    let Some(why) = vike_config::typed_confirm_reason(key) else {
        // ⚠ A confirm supplied for a key that needs none is NOT an error. It is what a script that
        // confirms everything does, and refusing it would punish the cautious spelling.
        return Ok(());
    };
    let typed = match supplied {
        Some(v) => v.trim().to_string(),
        None if std::io::stdin().is_terminal() => prompt_for_key(why)?,
        None => {
            // ⚠ The REASON comes from the predicate, never from this site. The ceremony covers two
            // classes now — the policy risk ceilings and the live-arm/safety-override flags — and a
            // refusal hard-coding "policy RISK CEILING" would be confidently wrong about
            // `flags.tradehub_live`, which is the one key it most needs to be right about.
            return Err(CliError::usage(format!(
                "`{key}` is {why}. It is written only when the exact key is retyped: re-run with \
                 `--confirm <key>`, or run this on a terminal and the verb will ask. (The ARMING \
                 ceilings — policy.venues.* and policy.accounts.* — need no confirm.)"
            )));
        }
    };
    if typed == key {
        Ok(())
    } else {
        Err(CliError::usage(
            "that is not the key — nothing was written. The confirm must repeat the key exactly."
                .to_string(),
        ))
    }
}

/// Ask the operator to retype the key, on a terminal.
///
/// ⚠ The prompt deliberately does NOT echo the key — it is on the command line the operator just
/// typed, and repeating it here would put the answer next to the question and turn a retype into a
/// copy. The same rule `crate::cmd::trade`'s `typed_key_confirm` states.
fn prompt_for_key(why: &str) -> CmdResult<String> {
    println!("  this is {why} — it is written only if you retype the key EXACTLY");
    print!("retype the key to confirm: ");
    let _ = std::io::stdout().flush();
    let mut typed = String::new();
    match std::io::stdin().read_line(&mut typed) {
        Ok(0) | Err(_) => {
            Err(CliError::usage("no confirmation was given — nothing was written".to_string()))
        }
        Ok(_) => Ok(typed.trim().to_string()),
    }
}

/// Gate, write, report.
///
/// ⚠ **The gate ORDER is load-bearing and it changed.** `file_for` runs FIRST — it opens nothing,
/// so the fence is not weakened by a pure lookup in front of it — because the secret gate's
/// predicate matches any leaf ending `_KEY`/`_USER`/`_LOGIN`/… and a mistyped SECTION
/// (`confgi.api_key`) used to be answered "credential-shaped, use `vike-cli secrets set`", which
/// then refuses it too because it is not in `vike_model::credential_keys`. A typo got a
/// confidently wrong diagnosis and a dead end.
///
/// The secret gate then runs BEFORE `resolve_confirm`, so a credential-shaped policy key is refused
/// rather than first being made to retype itself. It is called explicitly here even though
/// [`crate::cmd::settings_write::set_setting_journalled`] runs the same gate: that one is the
/// STRUCTURAL fence every caller inherits (decision 5 of that module), and this one is an ordering
/// choice this verb makes about its own ceremony. The predicate is pure and idempotent, so the
/// second call costs a string compare.
fn execute(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let key = args.key.trim();
    // ⚠ **VENUE SETTINGS ARE ROWS, so they branch BEFORE `file_for`** — which resolves a key to the
    // settings FILE that spells it, and no file can spell one of these: `vike_config::Config`
    // carries `#[serde(deny_unknown_fields)]` and has no `venue` field. That is why they live in
    // their own table (`vike_secrets::VenueSettingRow` carries the argument), and why this verb is
    // the only writer they have.
    //
    // ⚠ Without this branch the ten values `secrets move-venue-config` relocates are READ-ONLY.
    // `secrets set` writes the `credential` table, so it would not update one — it would create a
    // SHADOW row the fold reports as a collision and resolves in the credential's favour, leaving
    // the operator's new value ignored with no error anywhere. That is the defect this workspace
    // removed for credentials on 2026-09-21; re-creating it one table over would be worse, because
    // there the route at least existed.
    if let Some((venue, tier, field)) = vike_secrets::parse_venue_setting_key(key) {
        return set_venue_setting(args, ctx, key, &venue, tier.as_deref(), &field);
    }
    let file = file_for(key)?;
    settings_write::refuse_a_secret_key(key)?;
    resolve_confirm(key, args.confirm.as_deref())?;

    let write = settings_write::set_setting_journalled(
        &settings_write::Ctx {
            settings_dir: ctx.settings_dir,
            state_dir: ctx.state_dir,
            now_ms: ctx.now_ms,
            surface: "vike-cli config set",
        },
        file,
        key,
        &args.value,
    )?;
    // `settings_dir` is `Some` by construction here: `set_setting_journalled` refuses a `None` one
    // before it writes anything, so a returned `SettingsWrite` proves a directory answered.
    for line in report(&write, ctx.settings_dir, ctx.origin) {
        println!("{line}");
    }
    Ok(())
}

/// What an accepted write prints — PURE, so the wording is unit-tested rather than eyeballed.
///
/// Four things, in the order an operator needs them: WHICH FILE, by absolute path and by which rung
/// resolved its directory; WHAT landed (with the previous value, so a mistake is visible
/// immediately and reversible by hand); whether it is LIVE (never — see the module doc); and
/// whether anything READS it.
fn report(write: &SettingsWrite, settings_dir: Option<&Path>, origin: DirOrigin) -> Vec<String> {
    let was = match &write.old_value {
        Some(old) => format!("was {old}"),
        None => "the file did not set it before".to_string(),
    };
    let mut lines = Vec::new();
    // ⚠ FIRST, and absolute. `config show` answers "which project am I reading?" in its header and
    // this verb answered nothing at all — it printed a bare `policy.toml`, which on a box with two
    // project roots is a line that looks identical whichever tree it wrote.
    //
    // ⚠ **And "absolute" was a claim rather than a property.** `vike_model::state_path::
    // project_settings_dir_from` takes the `$VIKE_SETTINGS_DIR` override VERBATIM — no
    // canonicalisation — so `VIKE_SETTINGS_DIR=settings vike-cli config set …` printed
    // `wrote settings/policy.toml (named outright by VIKE_SETTINGS_DIR)`, which is the exact
    // ambiguity this line exists to close, in the case it was added for: the WALK rung is absolute
    // because it starts at the working directory and the three shipped units pass absolute paths,
    // so the only rung that can be relative is the one a human types at a prompt at 03:00.
    // `std::path::absolute` resolves it against the working directory without touching the
    // filesystem (no symlink resolution, no existence check — this line reports where the write
    // WENT, it does not re-derive it).
    if let Some(dir) = settings_dir {
        let target = dir.join(write.file);
        let shown = std::path::absolute(&target).unwrap_or(target);
        lines.push(format!("wrote {} ({})", shown.display(), origin.phrase()));
    }
    lines.push(format!("{}: {} = {}  ({was})", write.file, write.key, write.new_value));
    // ⚠ **Is this write IN FORCE?** On a box that has run `vike-cli config adopt`, the settings
    // DATABASE answers for every settings key and the four files are not opened for resolution at
    // all — so a file write that did not reach a row is a write nothing reads. This verb passes
    // `RowSync::Mirror`, so it normally does reach one; this line exists for the state where the
    // re-derive failed, because a bare success there is positive confirmation of something false.
    if !write.in_force {
        lines.push(
            "⚠ NOT IN FORCE: this box resolves its settings from the settings database (`vike-cli \
             config adopt` sealed it), and this value did not reach a row — so nothing reads it. \
             Run `vike-cli config mirror` to file it, or `vike-cli config compare` to see what the \
             two sources disagree about."
                .to_string(),
        );
    }

    // ⚠ This said "a running node can take a FEW keys live", which for all but two keys in the tree
    // is a wild goose chase: `crates/vike-tradehub/src/hot_reload.rs`'s `classify` answers
    // `HotClass::Restart` for the whole policy file before its table is even consulted, every
    // `SettingsFile::Config` row is `Restart`, and the only `HotClass::Hot` rows are the two log
    // levels. So the BOUND is stated instead of implied. That names no table row and copies nothing
    // down a layer — which matters, because vike-cli and vike-tradehub both declare layer 65 and
    // this crate cannot see that table at all (module doc).
    lines.push(
        "not live: the RUNNING process keeps its boot-time value — restart it for this to take \
         effect. A running node can take only the two log-level preferences live, over \
         `vike-cli trade set-setting`."
            .to_string(),
    );
    lines.push(read_verdict(&write.key));
    lines
}

/// The key's READ verdict, from the table `config show` renders.
///
/// ⚠ **A `policy.*` key gets its own line rather than SILENCE**, and the silence was the defect.
/// `vike_config::CONSUMPTION` carries no policy row, so this returned `None` for every policy key —
/// meaning the loud "⚠ NOTHING READS THIS KEY" fired for a `config`/`flags`/`preferences` key and
/// nothing at all was said for a policy one. `policy.max_leverage` is the tree's one declared-
/// unconsumed policy field (`crates/vike-config/tests/policy_is_consumed.rs`'s `POLICY_CONSUMERS`
/// carries it as `Consumed::No`), and it is also a risk ceiling, so it is the single key this verb
/// makes an operator RETYPE — the ceremony reads as "this matters", the silence read as "this is
/// wired", and neither was true. That is
/// `docs/decisions/0055-every-setting-is-editable-from-the-ui.md`'s second reopening condition
/// exactly: a control given while the operator now believes they have looked.
///
/// The honest cheap answer is to say WHERE the question is answered instead of guessing at it.
/// Moving `POLICY_CONSUMERS` out of that test file and into `vike-config` beside `CONSUMPTION` is
/// the better fix and is deliberately NOT done here: it is a `vike-config` API addition with its
/// own consumers (`config show`'s READ column would want it too), and folding it into a CLI branch
/// would land a shared table as a side effect.
fn read_verdict(key: &str) -> String {
    let Some(consumer) = vike_config::consumer_of(key) else {
        return "read by: no verdict — the consumption table covers `config`, `preferences` and \
                `flags`; for a policy key the question is answered in \
                `crates/vike-config/tests/policy_is_consumed.rs`'s `POLICY_CONSUMERS`, which \
                carries one row per ceiling."
            .to_string();
    };
    match consumer {
        // ⚠ The VERDICT is interposed deliberately. "NOTHING READS THIS KEY" was true about the
        // FILE key and silent about the variable, and `why` then described a library that reads
        // the variable — which for six flags is reached from nothing a binary runs. An operator
        // who read this line and exported the variable got the same nothing, believing otherwise.
        // `vike_config::Reader::verdict` is the one home for that sentence, shared with
        // `config show`, so the two surfaces cannot drift.
        Consumer::Not { why, reader } => {
            format!(
                "⚠ NOTHING READS THIS KEY, so the value above changes no behaviour. {} {why}",
                reader.verdict()
            )
        }
        c @ Consumer::At { .. } => match c.binary() {
            Some(bin) => {
                format!("read by: {bin} — a box not running that binary is unaffected by this key")
            }
            None => {
                "read by: a library, so every binary that links it honours this key".to_string()
            }
        },
    }
}

/// **Write ONE `venue_setting` row** — the branch [`execute`] takes for a `venue.*` key.
///
/// ⚠ It reports the same three things the file path reports (what changed, from what, where it
/// landed) because an operator cannot tell from the key which route their write took, and a verb
/// that goes quiet on one of its two destinations is how a write gets believed that did not happen.
///
/// ⚠ **A mistyped TIER is refused by the DATABASE, not by a check here.** The DDL's
/// `CHECK (tier IS NULL OR tier IN ('sim','demo','live'))` is the validation a `config` map field
/// could never have given — that shape would have accepted `venue.ibkr.demoo.backend` in silence
/// and read it by nothing. The error surfaces as a store error naming the constraint.
fn set_venue_setting(
    args: &Args,
    ctx: &Ctx<'_>,
    key: &str,
    venue: &str,
    tier: Option<&str>,
    field: &str,
) -> CmdResult<()> {
    settings_write::refuse_a_secret_key(key)?;
    resolve_confirm(key, args.confirm.as_deref())?;

    let dir = match ctx.settings_dir {
        Some(d) => d.to_path_buf(),
        None => vike_secrets::workspace_settings_dir_from(None),
    };
    let db = vike_secrets::db_path_in(&dir);
    if !db.is_file() {
        return Err(CliError::failed(format!(
            "this box has no settings database, so it holds no venue settings — {} does not \
             exist. `vike-cli secrets migrate` is what creates it, and \
             `vike-cli secrets move-venue-config` is what files these values as rows.",
            db.display()
        )));
    }

    let previous = vike_secrets::set_venue_setting_in(&dir, venue, tier, field, &args.value)
        .map_err(|e| CliError::failed(e.to_string()))?;

    match &previous {
        Some(old) => println!("{key} = {} (was {old})", args.value),
        None => println!("{key} = {} (new row)", args.value),
    }
    println!("  in: {}", db.display());
    println!(
        "  ⚠ A RUNNING daemon still holds what it BOOTED with — restart it for this to take \
         effect. Readers find it under its legacy credential name either way: the credential map \
         folds this row back in."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::args::HELP_SENTINEL;
    use crate::exit::Exit;

    fn parsed(argv: &[&str]) -> Result<Args, String> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn a_key_and_a_value_parse_in_order_and_the_confirm_takes_both_forms() {
        let a = parsed(&["config.tradehub_addr", "127.0.0.1:7879"]).unwrap();
        assert_eq!(a.key, "config.tradehub_addr");
        assert_eq!(a.value, "127.0.0.1:7879");
        assert_eq!(a.confirm, None);
        for argv in [
            &["policy.max_account_exposure", "3", "--confirm", "policy.max_account_exposure"][..],
            &["policy.max_account_exposure", "3", "--confirm=policy.max_account_exposure"][..],
        ] {
            let a = parsed(argv).unwrap();
            assert_eq!(a.confirm.as_deref(), Some("policy.max_account_exposure"), "{argv:?}");
        }
        assert_eq!(parsed(&["--help"]).unwrap_err(), HELP_SENTINEL);
    }

    /// ⚠ The VALUE is never re-split on `=`. This is the assertion that goes red the day somebody
    /// routes these positionals through the shared flag iterator.
    #[test]
    fn a_value_containing_an_equals_sign_survives_whole() {
        let a = parsed(&["config.log_dir", "a=b=c"]).unwrap();
        assert_eq!(a.value, "a=b=c");
    }

    #[test]
    fn a_missing_value_and_a_third_positional_are_both_refused_by_name() {
        let err = parsed(&["config.log_dir"]).unwrap_err();
        assert!(err.contains("VALUE"), "{err}");
        assert!(err.contains("config show"), "it must point at the READ verb: {err}");
        assert!(parsed(&[]).unwrap_err().contains("KEY"));
        let err = parsed(&["a.b", "one", "two"]).unwrap_err();
        assert!(err.contains("ONE key and ONE value"), "{err}");
        assert!(parsed(&["a.b", "c", "--nope"]).unwrap_err().contains("--nope"));
    }

    /// **THE REFUSAL MAY NOT REPEAT A CREDENTIAL BACK.** A one-positional command line used to be
    /// answered with that positional echoed VERBATIM, twice, to stderr — and `KEY=VALUE` is the
    /// dotenv muscle memory that produces one, so a pasted venue key was printed in full on the
    /// stream CI logs and every service manager captures. The gate that would have caught it runs
    /// inside `execute`, i.e. after `parse`.
    ///
    /// Both halves of the slip are covered: the shape on a bare token, and the shape on the
    /// pre-`=` half of a `KEY=VALUE`.
    #[test]
    fn a_missing_value_never_echoes_a_credential_shaped_token() {
        for argv in ["BINANCE_LIVE_API_KEY=sk_live_deadbeef", "ACME_API_SECRET", "config.bot_token"]
        {
            let err = parsed(&[argv]).unwrap_err();
            assert!(!err.contains(argv), "the whole token was echoed back: {err}");
            assert!(!err.contains("sk_live_deadbeef"), "THE VALUE was echoed back: {err}");
            assert!(err.contains("secrets set"), "it must name the credential writer: {err}");
            assert!(err.contains("stdin"), "…and the channel a credential arrives on: {err}");
        }
        // …and an ORDINARY key is still quoted, which is the only case the `--filter` pointer
        // helps: over-redacting every refusal would cost the operator the spelling they mistyped.
        let err = parsed(&["config.log_dir"]).unwrap_err();
        assert!(err.contains("config.log_dir"), "{err}");
    }

    /// ⚠ `--` ends option parsing. Without it a VALUE could not begin with `--` at all, and a value
    /// spelled `--help` printed usage and exited 0 — a scripted `config set <key> "$VALUE"` that
    /// SUCCEEDED while changing nothing. An unknown flag BEFORE the separator is still refused by
    /// name, which is the property the no-`--file` test depends on.
    #[test]
    fn a_double_dash_ends_option_parsing_so_a_value_may_begin_with_dashes() {
        let a = parsed(&["preferences.chart_style", "--", "--dark"]).unwrap();
        assert_eq!(a.key, "preferences.chart_style");
        assert_eq!(a.value, "--dark");

        let a = parsed(&[
            "--confirm=policy.max_notional_per_order",
            "--",
            "policy.max_notional_per_order",
            "--help",
        ])
        .unwrap();
        assert_eq!(a.value, "--help", "a VALUE spelled --help must be written, not print usage");
        assert_eq!(a.confirm.as_deref(), Some("policy.max_notional_per_order"));

        // …and an unknown flag before the separator is still named.
        assert!(parsed(&["--file", "/tmp/x", "a.b", "c"]).unwrap_err().contains("--file"));
    }

    /// **The credential fence, asserted at the verb** — the gate itself now lives at the shared
    /// writer (`crate::cmd::settings_write`, decision 5), and this asserts the wording an operator
    /// meets, including the half that fixes a real dead end.
    ///
    /// ⚠ The shapes ERR TOWARDS REFUSING, and `config.no_such_key` is the case that proves it: any
    /// leaf ending `_key` is caught, credential or not. That is the safe direction, but it used to
    /// end in a DEAD END — "use `vike-cli secrets set <KEY>`", which then refuses the key too
    /// because it is not in `vike_model::credential_keys`. So the message now answers both
    /// readings, and this pins that it does.
    #[test]
    fn a_secret_shaped_key_is_refused_and_names_both_ways_out() {
        for key in
            ["config.bot_token", "preferences.client_secret", "flags.api_key", "config.no_such_key"]
        {
            let e = settings_write::refuse_a_secret_key(key)
                .expect_err("a credential-shaped key must be refused");
            assert_eq!(e.exit, Exit::Usage, "{key}");
            assert!(e.msg.contains("secrets set"), "{}", e.msg);
            assert!(e.msg.contains("stdin"), "it must say how a credential arrives: {}", e.msg);
            assert!(
                e.msg.contains("config show"),
                "a mistyped SETTINGS key must not be routed into the credential store: {}",
                e.msg
            );
        }
        // …and an ordinary settings key is not caught by the shapes.
        for key in ["config.tradehub_addr", "policy.max_notional_per_order", "flags.reconcile"] {
            settings_write::refuse_a_secret_key(key).expect("an ordinary settings key must pass");
        }
    }

    /// ⚠ **`file_for` runs BEFORE the secret gate**, and this is the case that made it necessary: a
    /// mistyped SECTION whose leaf happens to be credential-shaped used to be diagnosed as a
    /// credential and routed into a store that then refuses it. `file_for` opens nothing, so
    /// putting a pure lookup in front weakens no fence.
    #[test]
    fn a_mistyped_section_is_a_section_error_not_a_credential_diagnosis() {
        let e = execute(
            &Args { key: "confgi.api_key".to_string(), value: "x".to_string(), confirm: None },
            &Ctx { settings_dir: None, origin: DirOrigin::Unresolved, state_dir: None, now_ms: 0 },
        )
        .expect_err("a typo'd section must refuse");
        assert!(e.msg.contains("does not name a settings file"), "{}", e.msg);
        assert!(!e.msg.contains("secrets set"), "not a credential diagnosis: {}", e.msg);
    }

    /// The file is derived from the key's FIRST SEGMENT, and an unknown one is a usage error that
    /// names all four spellings plus the read verb.
    #[test]
    fn the_file_comes_from_the_keys_first_segment() {
        assert_eq!(file_for("policy.max_account_exposure").unwrap(), SettingsFile::Policy);
        assert_eq!(file_for("config.log_dir").unwrap(), SettingsFile::Config);
        assert_eq!(file_for("preferences.log_level").unwrap(), SettingsFile::Preferences);
        assert_eq!(file_for("flags.reconcile").unwrap(), SettingsFile::Flags);
        let e = file_for("polciy.max_leverage").expect_err("a typo'd section must refuse");
        assert_eq!(e.exit, Exit::Usage);
        for spelling in ["policy.", "config.", "preferences.", "flags."] {
            assert!(e.msg.contains(spelling), "{}", e.msg);
        }
        assert!(e.msg.contains("config show"), "{}", e.msg);
    }

    /// **The ceremony line**, at the verb: a risk ceiling needs the exact key, an arming ceiling
    /// needs nothing, and a non-policy key needs nothing.
    #[test]
    fn only_a_risk_ceiling_demands_the_retyped_key() {
        resolve_confirm("policy.max_notional_per_order", Some("policy.max_notional_per_order"))
            .expect("an exact retype confirms");
        let e =
            resolve_confirm("policy.max_notional_per_order", Some("policy.max_account_exposure"))
                .expect_err("a mismatch must refuse");
        assert_eq!(e.exit, Exit::Usage);
        assert!(e.msg.contains("not the key"), "{}", e.msg);
        for key in ["policy.venues.binance", "config.log_dir", "flags.reconcile"] {
            resolve_confirm(key, None)
                .unwrap_or_else(|e| panic!("{key} needs no confirm: {}", e.msg));
            // …and a confirm nobody asked for is accepted rather than refused.
            resolve_confirm(key, Some("whatever")).expect("a needless confirm is not an error");
        }
    }

    /// The success report says what landed, says it is NOT live, and names the previous value —
    /// which is what makes a fat-fingered ceiling reversible by hand without reading the file.
    #[test]
    fn the_report_states_the_old_value_and_that_nothing_is_live() {
        let w = SettingsWrite {
            file: "policy.toml",
            key: "policy.max_notional_per_order".to_string(),
            old_value: Some("250".to_string()),
            new_value: "500".to_string(),
            // A fixture of what the WRITER returned, so `true` is the value every one of these
            // surfaces has always seen: none of them is on an adopted box. `vike_config::RowSync`
            // carries when it is false.
            in_force: true,
        };
        let dir = Path::new("/srv/vike-<unit>/settings");
        let text = report(&w, Some(dir), DirOrigin::Named).join("\n");
        assert!(text.contains("policy.toml"), "{text}");
        assert!(text.contains("was 250"), "{text}");
        assert!(text.contains("500"), "{text}");
        assert!(text.contains("restart"), "the operator must learn it is not live: {text}");
        let w = SettingsWrite {
            file: "config.toml",
            key: "config.log_dir".to_string(),
            old_value: None,
            new_value: "\"/tmp/x\"".to_string(),
            // A fixture of what the WRITER returned, so `true` is the value every one of these
            // surfaces has always seen: none of them is on an adopted box. `vike_config::RowSync`
            // carries when it is false.
            in_force: true,
        };
        assert!(
            report(&w, Some(dir), DirOrigin::Walk).join("\n").contains("did not set it before")
        );
    }

    /// **WHICH DIRECTORY was written, on the FIRST line, absolute, with the rung that resolved it.**
    ///
    /// `config show`'s header answers "which project am I reading?" and this verb — strictly more
    /// consequential — answered nothing: it printed a bare `policy.toml`. The project walk has
    /// captured the wrong tree three times on record and the live box has two project roots, so at
    /// 03:00 a correct write and a write into a stranger's tree produced identical output.
    #[test]
    fn the_first_report_line_names_the_absolute_file_and_the_rung_that_resolved_it() {
        let w = SettingsWrite {
            file: "policy.toml",
            key: "policy.max_notional_per_order".to_string(),
            old_value: None,
            new_value: "3".to_string(),
            // A fixture of what the WRITER returned, so `true` is the value every one of these
            // surfaces has always seen: none of them is on an adopted box. `vike_config::RowSync`
            // carries when it is false.
            in_force: true,
        };
        let dir = Path::new("/srv/vike-<unit>/settings");
        for (origin, needle) in
            [(DirOrigin::Named, "VIKE_SETTINGS_DIR"), (DirOrigin::Walk, "project walk")]
        {
            let first = report(&w, Some(dir), origin).remove(0);
            assert!(first.starts_with("wrote "), "{first}");
            assert!(
                first.contains(&dir.join("policy.toml").display().to_string()),
                "the ABSOLUTE path, not a bare file name: {first}"
            );
            assert!(first.contains(needle), "{origin:?}: {first}");
        }
    }

    /// ⚠ **…and "absolute" is a PROPERTY, not a claim.** `project_settings_dir_from` takes the
    /// `$VIKE_SETTINGS_DIR` override verbatim, so `VIKE_SETTINGS_DIR=settings` printed
    /// `wrote settings/policy.toml (named outright by VIKE_SETTINGS_DIR)` — the very ambiguity the
    /// line was added to close, in the one rung that can produce it. (The walk rung starts at the
    /// working directory and the three shipped units pass absolute paths; a relative settings
    /// directory can only ever come from a human at a prompt.)
    #[test]
    fn a_relative_settings_directory_is_still_reported_absolutely() {
        let w = SettingsWrite {
            file: "policy.toml",
            key: "policy.max_notional_per_order".to_string(),
            old_value: None,
            new_value: "3".to_string(),
            // A fixture of what the WRITER returned, so `true` is the value every one of these
            // surfaces has always seen: none of them is on an adopted box. `vike_config::RowSync`
            // carries when it is false.
            in_force: true,
        };
        let first = report(&w, Some(Path::new("settings")), DirOrigin::Named).remove(0);
        let shown = first
            .strip_prefix("wrote ")
            .and_then(|s| s.split(" (").next())
            .expect("the first line is `wrote <path> (<rung>)`");
        assert!(
            Path::new(shown).is_absolute(),
            "the line claims an absolute path and must be one: {first}"
        );
        assert!(first.ends_with("policy.toml (named outright by VIKE_SETTINGS_DIR)"), "{first}");
    }

    /// The READ verdict is the same table `config show` renders — and a `policy.*` key, which that
    /// table does not carry, gets a LINE rather than silence.
    ///
    /// The silence was the defect: the loud "⚠ NOTHING READS THIS KEY" fired for a
    /// config/flags/preferences key and nothing was said for a policy one — so `max_leverage`, the
    /// tree's one declared-unconsumed ceiling AND a key this verb makes an operator RETYPE, was
    /// also the key it silently declined to warn about.
    ///
    /// ⚠ The fixture is `max_notional_per_order` rather than the key the argument is about, and
    /// that is not laziness: `crates/vike-config/tests/policy_is_consumed.rs`'s
    /// `an_unconsumed_field_is_really_unconsumed` scans every non-comment line outside
    /// `crates/vike-config/` for a declared-unconsumed field's dotted name and calls a hit a READ.
    /// A test FIXTURE naming it would redden that gate, which is the gate working. The property
    /// under test holds for any `policy.*` key — none of them carries a `CONSUMPTION` row — and the
    /// argument belongs in this comment, which that scan skips.
    #[test]
    fn the_read_verdict_follows_the_consumption_table_and_says_so_for_a_policy_key() {
        let line = read_verdict("policy.max_notional_per_order");
        assert!(
            line.contains("no verdict"),
            "a policy key may not be answered with silence: {line}"
        );
        assert!(
            line.contains("crates/vike-config/tests/policy_is_consumed.rs"),
            "it must name where the question IS answered: {line}"
        );
        let key = "config.tradehub_addr";
        let line = read_verdict(key);
        match vike_config::consumer_of(key).expect("a config key carries a consumption row") {
            Consumer::Not { .. } => assert!(line.contains("NOTHING READS"), "{line}"),
            Consumer::At { .. } => assert!(line.contains("read by"), "{line}"),
        }
    }

    /// ⚠ The not-live line states a BOUND rather than implying a set. "a few keys" pointed an
    /// operator who had just moved a risk ceiling at a control channel that would also have needed
    /// a restart; only the two log-level preferences are `HotClass::Hot`.
    #[test]
    fn the_not_live_line_bounds_the_hot_set_rather_than_hinting_at_it() {
        let w = SettingsWrite {
            file: "policy.toml",
            key: "policy.max_notional_per_order".to_string(),
            old_value: None,
            new_value: "250".to_string(),
            // A fixture of what the WRITER returned, so `true` is the value every one of these
            // surfaces has always seen: none of them is on an adopted box. `vike_config::RowSync`
            // carries when it is false.
            in_force: true,
        };
        let text = report(&w, Some(Path::new("/s")), DirOrigin::Walk).join("\n");
        assert!(text.contains("only the two log-level preferences"), "{text}");
        assert!(!text.contains("a few keys"), "the vague wording must not come back: {text}");
    }
}
