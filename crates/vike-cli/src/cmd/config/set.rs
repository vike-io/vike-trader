//! `vike-cli config set <key> <value>` — **write ONE settings row**, and record it in the change
//! journal (`docs/decisions/0086`: settings live only in the database, and a write is one row).
//!
//! # What commissioned it, measured
//!
//! The change journal on a live box held, across two months, twenty-nine boot records and
//! twenty-eight venue mounts and **zero settings writes** — while the credential store's own backup
//! files proved it had changed at least eight times. The journal was never broken: the only
//! journalling settings writers an operator could reach were two `backend` sub-verbs, each writing
//! one fixed key. Every real change was made with an editor over ssh, which no software can see.
//! This verb is the missing writer, and the whole of it is a call to
//! `vike_config::write_setting_row` through `crate::cmd::settings_write` — the path
//! `backend connect` already proves works.
//!
//! It also answers the owner's standing complaint that an operator who must ssh in and hand-edit a
//! database is an operator who leaves.
//!
//! # The surface
//!
//! ```text
//! vike-cli config set <dotted.key> <value>
//! ```
//!
//! - The KEY is the dotted spelling `vike-cli config show` renders, and its first segment picks the
//!   settings SECTION (`policy.` / `config.` / `preferences.` / `flags.`). **There is deliberately
//!   no `--file` flag**: the key already names the section, and an operator-supplied destination
//!   is one of the things
//!   `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` names as
//!   re-deciding it from the top. That record's SUBJECT is the credential store and this verb
//!   touches none of it (see *The fence* below) — but the reason transfers whole.
//! - The VALUE is an ordinary command-line argument, and that is a decision rather than an
//!   oversight. The credential surface refuses a value in argv because argv is visible in `ps` to
//!   every user on the box, and a venue key read out of a process listing is a stolen key. A
//!   settings value is not secret — a notional ceiling, a log level, a bind address — so argv costs
//!   nothing there, and a settings verb that could not take one would be unscriptable. The property
//!   that keeps the two apart is mechanical rather than remembered: this verb REFUSES a
//!   credential-shaped key outright (`vike_config::refuse_credential_key`, the refusal every
//!   settings writer shares), so a value that would have needed hiding never reaches the command
//!   line in the first place.
//! - A `venue.<venue>[.<tier>].<field>` KEY is a different table entirely (`docs/decisions/0095`)
//!   and is validated against the DECLARED catalog, `vike_model::venues::venue_fields`, before the store is
//!   opened: an undeclared field, a field written at the wrong scope, or a value that fails the
//!   field's grammar is refused with nothing written. A venue field marked `secret` is refused on
//!   the command line outright — its value is taken from stdin (`-`) only — and every accepted write
//!   is journalled exactly as a settings-section write is.
//!
//! # The fence: this verb does not touch the credential store
//!
//! `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md` is about the
//! credential store, and three mechanical facts put this verb outside it: it calls
//! `vike_config::write_setting_row`, never `save_credentials_to_store` (the gate that polices
//! credential writers detects one by those function NAMES); it writes settings ROWS and never a
//! credential or node-key row; and the key it will accept is gated by
//! `vike_config::refuse_credential_key`, so a credential-shaped name is refused at the door. Three
//! of that record's rules are honoured anyway, because they were paid for: no operator-supplied
//! destination, one journal record per write, and no secret in argv.
//!
//! What it does NOT inherit is that record's MCP rule, which is credential-scoped. Whether a local
//! settings writer belongs on the agent surface is a separate question and the answer here is NO
//! for now: `crate::cmd::mcp` already advertises a `set_setting` tool, but that one is a
//! `WireCommand` aimed at a REMOTE node behind a mandatory preview token, and an agent editing the
//! box's own settings database with no node and no preview in front of it is new ground. It is not
//! advertised by this verb and adding it later is its own one-paragraph ruling.
//!
//! # No retype confirm, for any key (0086 point 7)
//!
//! ⚠ **This verb used to demand `--confirm <key>` for the policy risk ceilings and the live-arm /
//! safety-override flags, and the ceremony is DELETED — not merely made optional.** The owner's
//! ruling: *"confirmation over confirmation … `--confirm flags.tradehub_live` is a nightmare"*.
//! `--confirm` is a hard usage error now, naming itself as removed, rather than a silently accepted
//! no-op — a script that still passes it must be told, not let believe it did something.
//!
//! What guards a mistake instead: `vike_config::write_setting_row`'s own loader-backed bounds check
//! (run TWICE — the current store must already boot clean, and the candidate must too, resolving no
//! key but the one named), and the *old -> new* report this verb prints, which makes a fat-fingered
//! ceiling reversible by hand without reading anything.
//!
//! # What the operator is told afterwards
//!
//! **Every write is restart-to-apply.** That is true by construction: the only hot-apply path in
//! this tree is the daemon's own WIRE arm, and nothing in the tree watches the settings database for
//! a LOCAL write (`crates/vike-tradehub/src/tradehub_cli.rs`'s boot anchor states it outright).
//!
//! It also prints the key's READ verdict, from the same `vike_config::CONSUMPTION` table
//! `config show` renders. A control over a key nothing reads is worse than no control — it is
//! positive confirmation that the operator has looked — and that failure is precisely what
//! `docs/decisions/0055-every-setting-is-editable-from-the-ui.md` names as a condition for
//! reopening itself.

use std::process::ExitCode;

use vike_config::{Consumer, write::RowReport};

use crate::cmd::args::{self, exit_for_parse_error};
use crate::cmd::settings_write;
use crate::exit::{CliError, CmdResult};

/// The usage line, printed by `config set --help` and by every parse refusal.
pub(crate) const SET_USAGE: &str = "\
usage: vike-cli config set <key> <value>

  <key>      the dotted key `vike-cli config show` renders (policy.* | config.* |
             preferences.* | flags.* | venue.<venue>[.<tier>].<field>) — its first segment picks
             the section, or `venue` routes to the declared venue-settings catalog
  <value>    the new value, parsed as TOML when it is one (250, true, \"text\") and taken as a
             string otherwise. Never a credential: a secret-shaped key is refused, and
             `vike-cli secrets set` is the surface that writes those (stdin, never argv)
  -          read the value from stdin — REQUIRED for a secret venue field
             (`venue.polymarket.socks_proxy`)
  --         end of options: everything after it is a positional, verbatim. Use it for a
             value that begins with `--` (`config set preferences.x -- --dark`)

  There is no `--confirm` any more (docs/decisions/0086): a write needs no retyped key.";

/// Everything this verb needs from outside itself — the dispatcher's ONE boot walk and its ONE
/// clock read. `crate::cmd::secrets`' `Ctx` carries the argument for the shape.
#[derive(Clone, Copy)]
pub(crate) struct Ctx<'a> {
    /// The settings directory, as the boot resolved it — the destination, and the reason this verb
    /// needs no `--file`.
    pub settings_dir: Option<&'a std::path::Path>,
    /// That directory's `state` child, off the same walk: the change journal's home. `None`
    /// journals nothing and still writes.
    pub state_dir: Option<&'a std::path::Path>,
    /// The instant a journal record is stamped with, read once by the dispatcher because
    /// `vike_model::change_journal` reads no clock.
    pub now_ms: i64,
}

/// The parsed command line.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    key: String,
    value: String,
}

/// Entry point: parse, gate, write, report. `args` is everything after `config set`.
pub(crate) fn run(args: impl Iterator<Item = String>, ctx: Ctx<'_>) -> ExitCode {
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

/// PURE argv grammar — two positionals and nothing else, so the whole of it is unit-tested.
///
/// ⚠ The VALUE positional is taken RAW and is never split on `=`, unlike the shared
/// [`crate::cmd::args::Flags`] iterator: `config set config.log_dir a=b` is a legitimate value, and
/// a value-splitting parser would silently write `a`.
///
/// ⚠ **`--` ends option parsing**, and everything after it is a positional verbatim. Without it a
/// settings VALUE cannot begin with `--` at all (`config set preferences.chart_style --dark` is
/// `unknown argument: --dark`), and worse, a value spelled `--help` prints usage and exits 0 — so a
/// scripted `config set <key> "$VALUE"` whose variable happened to hold `--help` SUCCEEDED while
/// changing nothing. One `--`, the convention every shell and every operator already knows, fixes
/// both; an unknown flag BEFORE it is still refused by name.
///
/// ⚠ **`--confirm` is refused by NAME rather than silently ignored** (0086 point 7): a script that
/// still passes the removed flag must be told, not let believe it retyped anything.
fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut out = Args::default();
    let mut positionals: Vec<String> = Vec::new();
    let mut end_of_options = false;
    for arg in args {
        if end_of_options {
            positionals.push(arg);
            continue;
        }
        match arg.as_str() {
            "--" => end_of_options = true,
            "-h" | "--help" => return args::help_requested(),
            _ if arg.starts_with("--confirm") => {
                return Err(
                    "--confirm was removed (docs/decisions/0086): `config set` writes one row \
                     with no retype — there is nothing left to confirm"
                        .to_string(),
                );
            }
            _ if arg.starts_with("--") => return Err(format!("unknown argument: {arg}")),
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
/// `.env`-file habit that produces a one-positional command line, so
/// `vike-cli config set BINANCE_LIVE_API_KEY=sk_live_…` printed the credential in full on the
/// stream CI logs and every service manager capture — the exact defect 0036's 2026-09-13 narrowing
/// amendment records ("a refusal that echoed a dash-leading token"), and the rule
/// `crate::cmd::secrets`' `ARGV_VALUE_REFUSAL` arm spends thirty lines of comment on: **a refusal
/// quotes nothing an operator typed** once the token might be a secret.
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
                back. Credentials live in the store, not in the settings database: `vike-cli \
                secrets set <KEY>` is the writer and it takes the value on stdin, never on the \
                command line. (If it was a settings key, `vike-cli config show` prints every key \
                this verb takes.)"
            .to_string();
    }
    format!(
        "a VALUE is required — `config set {token} <value>`. To READ a key instead, run \
         `vike-cli config show --filter {token}`"
    )
}

/// Gate, write, report.
fn execute(args: &Args, ctx: &Ctx<'_>) -> CmdResult<()> {
    let key = args.key.trim();
    // ⚠ **VENUE SETTINGS ARE ROWS, so they branch BEFORE the section grammar** — `vike_config::
    // Config` carries `#[serde(deny_unknown_fields)]` and has no `venue` field, so no section can
    // spell one of these. `venue_write` validates against `vike_model::venues::venue_fields` and journals.
    if vike_secrets::venue_setting::parse_venue_setting_key(key).is_some() {
        for line in venue_write(args, ctx, &mut std::io::stdin().lock())? {
            println!("{line}");
        }
        return Ok(());
    }
    settings_write::refuse_a_secret_key(key)?;

    let report = settings_write::set_setting_journalled(
        &settings_write::Ctx {
            settings_dir: ctx.settings_dir,
            state_dir: ctx.state_dir,
            now_ms: ctx.now_ms,
            surface: "vike-cli config set",
        },
        key,
        &args.value,
    )?;
    for line in report_lines(&report) {
        println!("{line}");
    }
    Ok(())
}

/// What an accepted write prints — PURE, so the wording is unit-tested rather than eyeballed.
///
/// Three things, in the order an operator needs them: WHAT landed, with the previous value (so a
/// mistake is visible immediately and reversible by hand); that it is restart-to-apply; and whether
/// anything READS it.
fn report_lines(report: &RowReport) -> Vec<String> {
    let was = match &report.old_value {
        Some(old) => format!("was {old}"),
        None => "no previous value in the store".to_string(),
    };
    vec![
        format!("{} = {}  ({was})", report.key, report.new_value),
        "not live: the RUNNING process keeps its boot-time value — restart it for this to take \
         effect. A running node can take only the two log-level preferences live, over \
         `vike-cli trade set-setting`."
            .to_string(),
        read_verdict(&report.key),
    ]
}

/// The key's READ verdict, from the table `config show` renders.
///
/// ⚠ **A `policy.*` key gets its own line rather than SILENCE**, and the silence was the defect.
/// `vike_config::CONSUMPTION` carries no policy row, so this returned `None` for every policy key —
/// meaning the loud "⚠ NOTHING READS THIS KEY" fired for a `config`/`flags`/`preferences` key and
/// nothing at all was said for a policy one. `policy.max_leverage` is the tree's one declared-
/// unconsumed policy field (`crates/vike-config/tests/policy_is_consumed.rs`'s `POLICY_CONSUMERS`
/// carries it as `Consumed::No`), and it is also a risk ceiling, so silence there read as "this is
/// wired" when it was not — `docs/decisions/0055-every-setting-is-editable-from-the-ui.md`'s second
/// reopening condition exactly: a control given while the operator now believes they have looked.
fn read_verdict(key: &str) -> String {
    let Some(consumer) = vike_config::consumer_of(key) else {
        return "read by: no verdict — the consumption table covers `config`, `preferences` and \
                `flags`; for a policy key the question is answered in \
                `crates/vike-config/tests/policy_is_consumed.rs`'s `POLICY_CONSUMERS`, which \
                carries one row per ceiling."
            .to_string();
    };
    match consumer {
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

/// **Write ONE `venue_setting` row** — the branch [`execute`] takes for a `venue.*` key — and return
/// the lines to print.
///
/// The key must name a field `vike_model::venues::venue_fields` DECLARES, at the scope it declares (a
/// tier-scoped field needs a tier segment, a machine-scoped one must not have one), and the value
/// must satisfy the field's grammar — all checked BEFORE the store is opened, so a refused write
/// touches nothing. That closes the hole this function's previous doc recorded: a mistyped tier
/// (`venue.ibkr.demoo.backend`) used to parse as a machine-scoped field `DEMOO.BACKEND` and be stored
/// for nothing to read.
///
/// A SECRET field takes its value from stdin only — the value argument must be `-` — and nothing
/// here, in the journal or in `config show` ever prints it.
///
/// Journalled through `vike_secrets::set_venue_setting_in_journalled`, the same change journal every
/// other settings write lands in (decision 0086).
fn venue_write(
    args: &Args,
    ctx: &Ctx<'_>,
    stdin: &mut dyn std::io::BufRead,
) -> CmdResult<Vec<String>> {
    let key = args.key.trim();
    let Some((venue, tier, field_upper)) =
        vike_secrets::venue_setting::parse_venue_setting_key(key)
    else {
        return Err(CliError::usage(format!("`{key}` is not a venue key")));
    };
    let field = field_upper.to_ascii_lowercase();
    let Some(declared) = vike_model::venues::venue_fields::venue_field(&venue, &field) else {
        let known: Vec<String> =
            vike_model::venues::venue_fields::fields_of(&venue).map(declared_key).collect();
        return Err(CliError::usage(format!(
            "`{key}` is not a declared field, so nothing would read it and nothing was written. \
             {venue}'s declared fields: {}. If you meant a CREDENTIAL, `vike-cli secrets set <KEY>` \
             is the writer (stdin, never argv). `vike-cli config show` lists every venue field.",
            if known.is_empty() { "none".to_string() } else { known.join(", ") }
        )));
    };
    match (declared.tier_scoped, tier.as_deref()) {
        (true, None) | (false, Some(_)) => {
            return Err(CliError::usage(format!(
                "`{key}` is written as `{}` — nothing was written.",
                declared_key(declared)
            )));
        }
        _ => {}
    }
    let value = if args.value == "-" {
        let mut line = String::new();
        stdin.read_line(&mut line).map_err(|e| CliError::failed(format!("reading stdin: {e}")))?;
        line.trim_end_matches(['\n', '\r']).to_string()
    } else if declared.secret {
        return Err(CliError::usage(format!(
            "`{key}` holds a secret, so it is never taken from the command line (the value you \
             typed was not used and is not repeated here). Pass `-` and write the value on stdin: \
             `vike-cli config set {key} -`"
        )));
    } else {
        args.value.clone()
    };
    // The grammar validates the TRIMMED value (`FieldGrammar::check`), so the row stores that value:
    // a padded argument or a piped line's trailing spaces / `\r\n` must not be recorded as part of
    // it (the row, the journal record and the next write's `(was …)` all carry what is stored).
    let value = value.trim().to_string();
    declared
        .grammar
        .check(&value)
        .map_err(|why| CliError::usage(format!("`{key}`: {why} — nothing was written")))?;

    let Some(dir) = ctx.settings_dir else {
        return Err(CliError::failed(format!(
            "no settings directory resolved, so {key} cannot be written — `cd` into the project, or \
             name one with $VIKE_SETTINGS_DIR"
        )));
    };
    let db = vike_secrets::db_path_in(dir);
    if !db.is_file() {
        return Err(CliError::failed(format!(
            "this box has no settings database, so it holds no venue settings — {} does not \
             exist. `vike-cli secrets init` is what creates it; then this command writes the \
             value as a row.",
            db.display()
        )));
    }
    let journal = vike_secrets::AccountJournal {
        actor: vike_model::change_journal::Actor::cli("vike-cli"),
        proc: vike_model::change_journal::Proc::current(env!("CARGO_PKG_VERSION")),
        now_ms: ctx.now_ms,
    };
    // ⚠ Both arms of `set_venue_setting_in_journalled` carry a journal outcome now, not only the
    // Ok one — a REFUSED write whose own refusal also failed to journal must say so too, the same
    // two-outcome shape `crate::cmd::settings_write::set_setting_journalled_within` prints for the
    // settings-row case (that module's `record` closure), mirrored here because a `src/cmd/` file,
    // unlike `vike-secrets`, may print.
    let (previous, journal_error) = match vike_secrets::set_venue_setting_in_journalled(
        dir,
        &venue,
        tier.as_deref(),
        &field_upper,
        &value,
        declared.secret,
        journal,
    ) {
        Ok(ok) => ok,
        Err(refusal) => {
            let (e, journal_error) = *refusal;
            if let Some(je) = journal_error {
                eprintln!(
                    "vike-cli config set: ⚠ nothing was written, and the change journal could not \
                     record the refusal of {key} either: {je}"
                );
            }
            return Err(CliError::failed(e.to_string()));
        }
    };
    if let Some(e) = journal_error {
        eprintln!(
            "vike-cli config set: ⚠ {key} WAS written, but the change journal could not record it: {e}"
        );
    }
    let shown = |v: &str| if declared.secret { "<set>".to_string() } else { v.to_string() };
    Ok(vec![
        match &previous {
            Some(old) => format!("{key} = {} (was {})", shown(&value), shown(old)),
            None => format!("{key} = {} (new row)", shown(&value)),
        },
        format!("  in: {}", db.display()),
        format!("  what it does: {}", declared.doc),
        "  not live: a RUNNING process keeps what it booted with — restart it for this to take effect"
            .to_string(),
    ])
}

/// The spelling an operator types for a declared field: `venue.<v>.<field>` or
/// `venue.<v>.<paper|demo|live>.<field>`.
fn declared_key(f: &vike_model::venues::venue_fields::VenueField) -> String {
    if f.tier_scoped {
        format!("venue.{}.<paper|demo|live>.{}", f.venue, f.field)
    } else {
        format!("venue.{}.{}", f.venue, f.field)
    }
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
    fn a_key_and_a_value_parse_in_order() {
        let a = parsed(&["config.tradehub_addr", "127.0.0.1:7879"]).unwrap();
        assert_eq!(a.key, "config.tradehub_addr");
        assert_eq!(a.value, "127.0.0.1:7879");
        assert_eq!(parsed(&["--help"]).unwrap_err(), HELP_SENTINEL);
    }

    /// **`--confirm` is refused BY NAME, never silently accepted.**
    #[test]
    fn a_confirm_flag_is_refused_as_removed() {
        for argv in [
            &["policy.max_notional_per_order", "3", "--confirm", "policy.max_notional_per_order"][..],
            &["policy.max_notional_per_order", "3", "--confirm=policy.max_notional_per_order"][..],
        ] {
            let err = parsed(argv).unwrap_err();
            assert!(err.contains("removed"), "{argv:?}: {err}");
            assert!(err.contains("0086"), "{argv:?}: {err}");
        }
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

    /// **THE REFUSAL MAY NOT REPEAT A CREDENTIAL BACK.**
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
        let err = parsed(&["config.log_dir"]).unwrap_err();
        assert!(err.contains("config.log_dir"), "{err}");
    }

    /// ⚠ `--` ends option parsing.
    #[test]
    fn a_double_dash_ends_option_parsing_so_a_value_may_begin_with_dashes() {
        let a = parsed(&["preferences.chart_style", "--", "--dark"]).unwrap();
        assert_eq!(a.key, "preferences.chart_style");
        assert_eq!(a.value, "--dark");

        let a = parsed(&["--", "policy.max_notional_per_order", "--help"]).unwrap();
        assert_eq!(a.value, "--help", "a VALUE spelled --help must be written, not print usage");

        assert!(parsed(&["--file", "/tmp/x", "a.b", "c"]).unwrap_err().contains("--file"));
    }

    /// **The credential fence, asserted at the verb.**
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
        for key in ["config.tradehub_addr", "policy.max_notional_per_order", "flags.reconcile"] {
            settings_write::refuse_a_secret_key(key).expect("an ordinary settings key must pass");
        }
    }

    /// The success report says what landed and names the previous value — which is what makes a
    /// fat-fingered ceiling reversible by hand.
    #[test]
    fn the_report_states_the_old_value_and_that_nothing_is_live() {
        let r = RowReport {
            key: "policy.max_notional_per_order".to_string(),
            old_value: Some("250".to_string()),
            new_value: "500".to_string(),
        };
        let text = report_lines(&r).join("\n");
        assert!(text.contains("was 250"), "{text}");
        assert!(text.contains("500"), "{text}");
        assert!(text.contains("restart"), "the operator must learn it is not live: {text}");

        let r = RowReport {
            key: "config.log_dir".to_string(),
            old_value: None,
            new_value: "\"/tmp/x\"".to_string(),
        };
        assert!(report_lines(&r).join("\n").contains("no previous value"));
    }

    /// The READ verdict is the same table `config show` renders — and a `policy.*` key, which that
    /// table does not carry, gets a LINE rather than silence.
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

    fn store() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        vike_secrets::plant_settings_rows(
            d.path(),
            &vike_secrets::StoredSettings {
                arming: vec![vike_secrets::ArmingRow {
                    venue: "binance".to_string(),
                    label: None,
                    mode: "paper".to_string(),
                    max_exposure: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
        d
    }

    fn venue_rows(d: &std::path::Path) -> Vec<vike_secrets::VenueSettingRow> {
        match vike_secrets::read_settings_in(d).unwrap() {
            vike_secrets::SettingsSource::Rows { rows, .. } => rows.venue,
            other => panic!("{other}"),
        }
    }

    fn set(d: &std::path::Path, key: &str, value: &str, stdin: &str) -> CmdResult<Vec<String>> {
        let args = Args { key: key.to_string(), value: value.to_string() };
        let ctx = Ctx { settings_dir: Some(d), state_dir: Some(&d.join("state")), now_ms: 1 };
        venue_write(&args, &ctx, &mut stdin.as_bytes())
    }

    /// Review Focus 4.
    #[test]
    fn an_undeclared_venue_field_is_refused_and_nothing_is_written() {
        let d = store();
        for key in [
            "venue.polymarket.proxy_hots",
            "venue.ibkr.demoo.backend",
            "venue.binanse.demo.backend",
            "venue.binance.mainnet",
        ] {
            let e = set(d.path(), key, "x", "").expect_err(key);
            assert_eq!(e.exit, crate::exit::Exit::Usage, "{key}");
            assert!(e.msg.contains("config show"), "it must point at the declared list: {}", e.msg);
        }
        assert!(venue_rows(d.path()).is_empty(), "a refused key wrote a row");
    }

    #[test]
    fn a_field_written_at_the_wrong_scope_or_grammar_is_refused() {
        let d = store();
        let e = set(d.path(), "venue.ibkr.host", "<host>", "").unwrap_err();
        assert!(e.msg.contains("venue.ibkr.<paper|demo|live>.host"), "{}", e.msg);
        let e = set(d.path(), "venue.polymarket.demo.proxy_host", "h", "").unwrap_err();
        assert!(e.msg.contains("venue.polymarket.proxy_host"), "{}", e.msg);
        let e = set(d.path(), "venue.polymarket.proxy_port", "ten", "").unwrap_err();
        assert!(e.msg.contains("a whole number"), "{}", e.msg);
        assert!(venue_rows(d.path()).is_empty());
    }

    #[test]
    fn a_declared_field_is_written_in_the_store_spelling() {
        let d = store();
        set(d.path(), "venue.ibkr.demo.port", "4002", "").unwrap();
        assert_eq!(
            venue_rows(d.path()),
            vec![vike_secrets::VenueSettingRow {
                venue: "ibkr".to_string(),
                tier: Some("demo".to_string()),
                field: "PORT".to_string(),
                value: "4002".to_string(),
            }]
        );
    }

    /// **The row stores the value the grammar CHECKED.** `FieldGrammar::check` validates the
    /// trimmed value, so a padded argument (`" 4002 "`) or a piped line with trailing spaces and a
    /// `\r\n` passed the check and was stored with the padding: the row, its change-journal record,
    /// `config show` and the `(was …)` of the next write all carried whitespace the operator never
    /// meant. Every reader trims, so behaviour never changed — only what was recorded.
    #[test]
    fn the_stored_value_is_the_value_the_grammar_checked() {
        let d = store();
        set(d.path(), "venue.ibkr.demo.port", " 4002 ", "").unwrap();
        let report = set(d.path(), "venue.ibkr.paper.port", "-", "4004 \r\n").unwrap().join("\n");
        assert!(report.contains("venue.ibkr.paper.port = 4004 (new row)"), "{report}");
        let stored = |tier: &str| {
            venue_rows(d.path())
                .into_iter()
                .find(|r| r.tier.as_deref() == Some(tier))
                .map(|r| r.value)
        };
        assert_eq!(stored("demo").as_deref(), Some("4002"));
        assert_eq!(stored("paper").as_deref(), Some("4004"));
        // …and a rewrite reports the previous value without the padding too.
        let report = set(d.path(), "venue.ibkr.demo.port", "4003", "").unwrap().join("\n");
        assert!(report.contains("= 4003 (was 4002)"), "{report}");
    }

    /// Review Focus 5.
    #[test]
    fn a_secret_venue_field_is_never_echoed() {
        let d = store();
        let e = set(d.path(), "venue.polymarket.socks_proxy", "socks5h://u:hunter2@h:1", "")
            .unwrap_err();
        assert_eq!(e.exit, crate::exit::Exit::Usage);
        assert!(!e.msg.contains("hunter2"), "{}", e.msg);
        assert!(e.msg.contains(" -"), "it must say how to pass the value on stdin: {}", e.msg);
        assert!(venue_rows(d.path()).is_empty());

        let lines = set(d.path(), "venue.polymarket.socks_proxy", "-", "socks5h://u:hunter2@h:1\n")
            .unwrap();
        let printed = lines.join("\n");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(printed.contains("<set>"), "{printed}");
        assert_eq!(venue_rows(d.path())[0].value, "socks5h://u:hunter2@h:1", "the value is stored");
        let mut journal = String::new();
        for e in std::fs::read_dir(d.path().join("state").join("changes")).unwrap().flatten() {
            journal.push_str(&std::fs::read_to_string(e.path()).unwrap());
        }
        assert!(journal.contains("venue.polymarket.socks_proxy") && !journal.contains("hunter2"));
    }

    #[test]
    fn a_venue_write_is_journalled() {
        let d = store();
        set(d.path(), "venue.polymarket.proxy_port", "11080", "").unwrap();
        let mut journal = String::new();
        for e in std::fs::read_dir(d.path().join("state").join("changes")).unwrap().flatten() {
            journal.push_str(&std::fs::read_to_string(e.path()).unwrap());
        }
        assert!(journal.contains("set_setting") && journal.contains("venue.polymarket.proxy_port"));
        assert!(journal.contains("11080") && journal.contains("pending_restart"), "{journal}");
    }
}
