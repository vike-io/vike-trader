//! `vike-cli config unset <key>` — **remove ONE settings row**, keep the integrity seal valid in the
//! same transaction, and record it in the change journal (decision 0161, which narrows
//! `docs/decisions/0086`'s "no repair command" ruling to the one cause it still holds for).
//!
//! # What commissioned it
//!
//! A key a release retires leaves rows behind on every box that wrote it, and a row the loader
//! refuses makes `vike-tradehub`, `trade`, `mcp` and `config check` refuse the WHOLE store. Nothing
//! could remove the row: a hand `DELETE` changes the `setting` row count the seal holds, so the next
//! boot refuses it as an erase; `config set` refuses to touch a store that does not boot clean; and
//! restoring the backup restores the row. Every future retirement was a brick risk. This verb is the
//! missing remover, and the whole of it is a call to `vike_config::unset_setting_row` through
//! `crate::cmd::settings_write`.
//!
//! # The surface
//!
//! ```text
//! vike-cli config unset <dotted.key>
//! ```
//!
//! - The KEY is the dotted spelling `config show` renders, and its first segment picks the section
//!   (`policy.` / `config.` / `preferences.` / `flags.`). It may name a key the loader NO LONGER
//!   KNOWS — a retired, tombstoned or hand-inserted row — which is the point of the verb, and a key
//!   that has no row is refused by name. A `venue.…` key is another table (`docs/decisions/0095`) and
//!   is refused as such: this verb removes `setting` rows only.
//! - **There is no value, and none is ever printed.** A leftover key's value is not ours to repeat
//!   (a credential pasted into the wrong key is a plausible first-time mistake), so the report names
//!   the key and what it resolves to NOW — and the change journal records `<set>` → `<unset>`.
//!   Because the key is the whole command line, a token that looks like `KEY=VALUE`, and any second
//!   token, is refused quoting nothing the operator typed.
//! - **A credential-shaped key is refused** (`vike_config::refuse_credential_key`, the one fence
//!   every settings writer shares): the database is the credential store, and no verb that takes a
//!   key from the command line may reach it. ⚠ The consequence is a declared residual: a hand-
//!   inserted `setting` row whose NAME is credential-shaped cannot be removed by this verb.
//! - **There is no `--file`, no `--confirm` and no `--force`.** The destination is the dispatcher-
//!   resolved settings directory, and the guard is the validator, not a retype.
//!
//! # What it refuses, and what it deliberately lets through
//!
//! The validator is `vike_config::unset_setting_row`'s own and it is NARROW: **the seal's row count
//! must already be sound.** A count that is off (rows arrived or left by a hand `INSERT`/`DELETE`,
//! or a restored backup) is the erase detector's finding, and removing a row would only move the
//! count and hide it; that case stays a restore from the nightly backup and the refusal says so. A
//! store that is refusing for a ROW — the state this verb exists for — is let through, which is
//! exactly why `config set`'s "the current store must boot clean" cannot serve here. If another
//! refused row remains, the report says so and names it, and the verb is run again for that one.
//!
//! # What the operator is told afterwards
//!
//! **Nothing is live.** A removal is a LOCAL write and the daemon has no wire arm for it, so the
//! running process keeps the value it booted with and the next start reads the default. (Even the
//! two log-level preferences, which a running node can take over `vike-cli trade set-setting`, can
//! only be MOVED live, never removed.) `vike_tradehub::hot_reload`'s `HotClass` classifies a wire
//! write; a removal never reaches it.
//!
//! This verb is not on the MCP surface, for `config set`'s reason (`crate::cmd::config::set`).

use std::process::ExitCode;

use vike_config::write::{KeyAfterRemoval, RowRemoval};

use crate::cmd::args;
use crate::cmd::settings_write;
use crate::exit::{CliError, CmdResult};

/// Everything this verb needs from outside itself — the dispatcher's ONE boot walk and its ONE
/// clock read, as `crate::cmd::config::set::Ctx`.
#[derive(Clone, Copy)]
pub(crate) struct Ctx<'a> {
    /// The settings directory, as the boot resolved it.
    pub settings_dir: Option<&'a std::path::Path>,
    /// That directory's `state` child: the change journal's home. `None` journals nothing and still
    /// removes.
    pub state_dir: Option<&'a std::path::Path>,
    /// The instant a journal record is stamped with.
    pub now_ms: i64,
}

/// The parsed command line.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    key: String,
}

// `config unset`'s arguments, as `clap` reads them: ONE positional and nothing else.
//
// ⚠ The positional is collected as a LIST and counted by hand, exactly as `config set`'s are, so
// a second token is refused by `into_args` quoting nothing: `clap`'s own "unexpected argument"
// message would repeat what the operator typed, and a token after the key is where a pasted value
// lands. (A `//` comment, not `///`: a doc comment on a derive struct becomes `--help` text.)
#[derive(clap::Args, Debug)]
#[command(
    about = "Remove ONE row from the settings database, keeping the seal valid, and record it in \
             the change journal",
    after_help = "The verb for a retired or refused row: `vike-cli config check` names each one \
                  and gives the line to run. It takes NO value and shows none; a key with no row \
                  is refused. See docs/ops/config-unset.md."
)]
pub(crate) struct UnsetCmd {
    #[arg(
        value_name = "KEY",
        help = "the dotted key `vike-cli config show` renders (policy.* | config.* | \
                preferences.* | flags.*) — it may name a key the loader no longer knows. A \
                credential-shaped key is refused, and a venue.* key is another table"
    )]
    positionals: Vec<String>,
}

impl UnsetCmd {
    fn into_args(self) -> Result<Args, String> {
        let mut positionals = self.positionals;
        match positionals.len() {
            0 => return Err("a KEY is required (the dotted key `config show` renders)".to_string()),
            1 => {}
            n => {
                return Err(format!(
                    "exactly ONE key (got {n}) — `config unset` removes a row and takes no value"
                ));
            }
        }
        let key = positionals.pop().expect("one positional");
        refuse_a_token_that_is_no_key(&key)?;
        Ok(Args { key })
    }
}

/// A token that cannot be a key is refused QUOTING NOTHING of what was typed, the rule `config set`'s
/// one-positional refusal states at length: with no value to take, the one place a pasted
/// credential can land is the key's own position.
///
/// * An `=` in it is the `KEY=VALUE` habit, and the right half of it may be a credential.
/// * No `.` in it means it names no section (every key is `section.key`), so it is not a key at all
///   — and echoing it through the planner's "unknown section" menu would repeat a token that may
///   be a bare secret. A typo'd section WORD with a dot (`polciy.max_leverage`) is still answered by
///   the planner's menu, which quotes the word: that one is plainly a settings key.
fn refuse_a_token_that_is_no_key(token: &str) -> Result<(), String> {
    if token.contains('=') {
        return Err("that looks like KEY=VALUE, and `config unset` takes the KEY alone — it \
                    removes a row and takes no value, and nothing you typed is repeated back. \
                    `vike-cli config show` prints every key this verb takes."
            .to_string());
    }
    if !token.contains('.') {
        return Err("a KEY is the dotted spelling `vike-cli config show` renders — \
                    policy.<key>, config.<key>, preferences.<key> or flags.<key> — and nothing \
                    you typed is repeated back."
            .to_string());
    }
    Ok(())
}

/// Entry point: gate, remove, report. `cmd` is everything after `config unset`, parsed.
pub(crate) fn run(cmd: UnsetCmd, ctx: Ctx<'_>) -> ExitCode {
    let parsed = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse("vike-cli config unset", msg),
    };
    match execute(&parsed, &ctx) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("vike-cli config unset: {}", e.msg);
            e.exit.into()
        }
    }
}

/// Read `config unset`'s argv tail (the arguments after `config unset`) through the plane's own
/// parser, so a test meets the grammar an operator does.
#[cfg(test)]
fn parse(argv: impl Iterator<Item = String>) -> Result<Args, args::Stop> {
    let argv = std::iter::once("unset".to_string()).chain(argv);
    match crate::cmd::config::parse(argv)? {
        crate::cmd::config::Sub::Unset(cmd) => {
            cmd.into_args().map_err(|msg| args::Stop::message("vike-cli config unset", msg))
        }
        other => unreachable!("`unset` parsed as {other:?}"),
    }
}

/// Gate, remove, and return the lines to print.
fn execute(args: &Args, ctx: &Ctx<'_>) -> CmdResult<Vec<String>> {
    let key = args.key.trim();
    // A venue setting is a row of ANOTHER table, so it is named and refused here rather than
    // reported as an unknown settings section.
    if vike_secrets::venue_setting::parse_venue_setting_key(key).is_some() {
        return Err(CliError::usage(
            "that is a venue setting, a row of the `venue_setting` table, not a `setting` row — \
             `config unset` removes only policy.*, config.*, preferences.* and flags.* rows, and \
             nothing was removed"
                .to_string(),
        ));
    }
    settings_write::refuse_a_secret_key_to_unset(key)?;

    let report = settings_write::unset_setting_journalled(
        &settings_write::Ctx {
            settings_dir: ctx.settings_dir,
            state_dir: ctx.state_dir,
            now_ms: ctx.now_ms,
            surface: "vike-cli config unset",
        },
        key,
    )?;
    Ok(report_lines(&report))
}

/// What an accepted removal prints — PURE, so the wording is unit-tested rather than eyeballed.
///
/// In the order an operator needs it: WHAT went (the key, never a value), what the key resolves to
/// NOW, that the seal moved with it, that it is restart-to-apply, and whether the store boots clean
/// — because a removal that cures one refused row may leave another, and the operator must not
/// leave believing a store is fixed that still refuses.
fn report_lines(report: &RowRemoval) -> Vec<String> {
    let key = &report.key;
    let mut lines =
        vec![format!("removed {key} from the settings database (its value is not shown)")];
    lines.push(match &report.now {
        KeyAfterRemoval::Default(Some(default)) => {
            format!("  it now resolves to its compiled-in default: {default}")
        }
        KeyAfterRemoval::Default(None) => "  it now resolves to its compiled-in default, which is \
             UNSET: no value is stored, and the key's own built-in behaviour for an unset key \
             applies"
            .to_string(),
        KeyAfterRemoval::Unknown => "  the loader does not know this key (a retired or \
             hand-inserted row), so nothing read it and no value changes"
            .to_string(),
    });
    if key.starts_with("policy.") && matches!(report.now, KeyAfterRemoval::Default(_)) {
        // An UNSET ceiling is an ABSENT ceiling — uncapped, or the switch off. The one policy key
        // whose unset default is an armed protection says so itself (`Default(Some(..))`).
        let absent = if report.now == KeyAfterRemoval::Default(None) {
            " — an UNSET policy key is an ABSENT ceiling (uncapped, or its switch off)"
        } else {
            ""
        };
        lines.push(format!(
            "  ⚠ a policy ceiling row is gone: the compiled-in default applies from the next \
             start{absent} (`vike-cli config show --filter {key}` shows the key's row)"
        ));
    }
    lines.push(match report.seal {
        Some((_, 0)) => "  that was the store's last row, so the seal went with it, in the same \
             transaction — an empty store carries none, and the next write seals it afresh"
            .to_string(),
        Some((before, after)) => format!(
            "  the settings seal moved with it, in the same transaction: {before} -> {after} rows"
        ),
        None => "  this store was never sealed, and a removal does not seal it".to_string(),
    });
    lines.push(
        "not live: the RUNNING process keeps its boot-time value — restart it for this to take \
         effect. A removal has no live path at all (the tradehub's wire carries only writes); to move \
         one of the two log-level preferences on a running tradehub, write the value you want over \
         `vike-cli trade set-setting`."
            .to_string(),
    );
    lines.push(match &report.still_refused {
        None => "store: boots clean".to_string(),
        Some(why) => format!(
            "⚠ store: STILL REFUSED by another row — {why}\n  `vike-cli config check` names every \
             refused row and the `config unset` line for each; run this verb again for the next one."
        ),
    });
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(argv: &[&str]) -> Result<Args, args::Stop> {
        parse(argv.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn one_key_parses_and_help_is_a_success() {
        let a = parsed(&["policy.max_total_exposure"]).unwrap();
        assert_eq!(a.key, "policy.max_total_exposure");
        assert!(parsed(&["--help"]).unwrap_err().is_help());
        assert!(parsed(&["-h"]).unwrap_err().is_help());
    }

    /// A missing key, a second token and an unknown flag are all refused — and the second token
    /// is refused QUOTING NOTHING, because it is where a pasted value lands.
    #[test]
    fn a_missing_key_a_second_token_and_a_stray_flag_are_refused_without_quoting_the_value() {
        let err = parsed(&[]).unwrap_err();
        assert!(err.contains("KEY"), "{err}");
        let err = parsed(&["config.log_dir", "hunter2-not-for-the-log"]).unwrap_err();
        assert!(err.contains("exactly ONE key"), "{err}");
        assert!(err.contains("no value"), "{err}");
        assert!(!err.contains("hunter2"), "a token after the key was repeated back: {err}");
        assert!(parsed(&["config.log_dir", "--nope"]).unwrap_err().contains("--nope"));
        assert!(parsed(&["--file", "/tmp/x", "config.log_dir"]).unwrap_err().contains("--file"));
    }

    /// **`KEY=VALUE` is the `.env` habit, and the right half may be a credential.** The token is
    /// refused and nothing of it is quoted — neither half.
    #[test]
    fn a_key_equals_value_token_is_refused_quoting_nothing() {
        let err = parsed(&["BINANCE_LIVE_API_KEY=sk_live_deadbeef"]).unwrap_err();
        assert!(!err.contains("sk_live_deadbeef"), "THE VALUE was echoed back: {err}");
        assert!(!err.contains("BINANCE_LIVE_API_KEY"), "the whole token was echoed back: {err}");
        assert!(err.contains("KEY=VALUE") && err.contains("config show"), "{err}");
    }

    /// A bare token with no `.` names no section, so it is not a key: refused quoting nothing, since
    /// a pasted secret with no `=` in it would otherwise be echoed by the unknown-section menu.
    #[test]
    fn a_token_with_no_dot_is_refused_quoting_nothing() {
        let err = parsed(&["sk_live_deadbeefdeadbeef"]).unwrap_err();
        assert!(!err.contains("sk_live"), "a bare token was echoed back: {err}");
        assert!(err.contains("dotted") && err.contains("policy."), "{err}");
    }

    /// The report names the key and what it resolves to now, says the seal moved and that nothing
    /// is live — and carries no value of the removed row, in any arm.
    #[test]
    fn the_report_names_the_key_the_new_resolution_and_that_nothing_is_live() {
        let known = RowRemoval {
            key: "preferences.log_level".to_string(),
            now: KeyAfterRemoval::Default(Some("info".to_string())),
            seal: Some((3, 2)),
            still_refused: None,
        };
        let text = report_lines(&known).join("\n");
        assert!(text.contains("removed preferences.log_level"), "{text}");
        assert!(text.contains("its value is not shown"), "{text}");
        assert!(text.contains("compiled-in default: info"), "{text}");
        assert!(text.contains("3 -> 2 rows"), "the seal's move is evidence: {text}");
        assert!(text.contains("not live") && text.contains("restart"), "{text}");
        assert!(text.contains("store: boots clean"), "{text}");
        assert!(!text.contains("⚠"), "a clean removal raises no warning: {text}");

        let retired = RowRemoval {
            key: "policy.max_total_exposure".to_string(),
            now: KeyAfterRemoval::Unknown,
            seal: Some((2, 1)),
            still_refused: None,
        };
        let text = report_lines(&retired).join("\n");
        assert!(text.contains("does not know this key"), "{text}");
        assert!(
            !text.contains("policy ceiling row is gone"),
            "a retired key was no ceiling: {text}"
        );
    }

    /// A removed policy CEILING gets its own warning, and one that is a default of "no value" says
    /// so; a store that still refuses is told, with the other row's reason and the next step.
    #[test]
    fn a_removed_ceiling_and_a_store_that_still_refuses_are_both_said_plainly() {
        let report = RowRemoval {
            key: "policy.max_notional_per_order".to_string(),
            now: KeyAfterRemoval::Default(None),
            seal: None,
            still_refused: Some(
                "settings database (section `preferences`): rate_utilization".to_string(),
            ),
        };
        let text = report_lines(&report).join("\n");
        assert!(text.contains("policy ceiling row is gone"), "{text}");
        assert!(text.contains("UNSET") && text.contains("ABSENT ceiling"), "{text}");
        assert!(text.contains("never sealed"), "{text}");
        assert!(text.contains("STILL REFUSED") && text.contains("rate_utilization"), "{text}");
        assert!(text.contains("run this verb again"), "{text}");
    }

    /// The removal that empties a sealed store says the seal went with the last row — not that it
    /// "moved to 0 rows", which would read as the sealed-and-empty state `config check` fails.
    #[test]
    fn the_last_row_is_reported_as_taking_the_seal_with_it() {
        let report = RowRemoval {
            key: "flags.reconcile_off".to_string(),
            now: KeyAfterRemoval::Default(Some("false".to_string())),
            seal: Some((1, 0)),
            still_refused: None,
        };
        let text = report_lines(&report).join("\n");
        assert!(text.contains("last row, so the seal went with it"), "{text}");
        assert!(!text.contains("1 -> 0 rows"), "{text}");
    }

    /// A venue key is named as another table, not reported as an unknown section.
    #[test]
    fn a_venue_key_is_refused_as_another_table() {
        let ctx = Ctx { settings_dir: None, state_dir: None, now_ms: 1 };
        let e = execute(&Args { key: "venue.polymarket.proxy_host".to_string() }, &ctx)
            .expect_err("a venue key is not a setting row");
        assert_eq!(e.exit, crate::exit::Exit::Usage);
        assert!(
            e.msg.contains("venue_setting") && e.msg.contains("nothing was removed"),
            "{}",
            e.msg
        );
    }
}
