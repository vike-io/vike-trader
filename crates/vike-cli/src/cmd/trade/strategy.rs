//! `vike-cli trade strategy` — the STRATEGY group's router. `ls` (what is mounted) and, since task 8
//! of the trade-CLI-plane, the two LIFECYCLE verbs `mount` / `unmount` are wired here as one-shot
//! spellings — the words that change what a running node IS rather than what its books hold.
//! `set-setting`, the group's third REPL lifecycle word, has no one-shot spelling yet and stays a
//! REPL-only verb (`crate::cmd::trade`'s module doc, "The strategy-LIFECYCLE half").
//!
//! # `ls`
//!
//! `vike-cli trade strategy ls --node <host:port> [--json]` — one row per mounted strategy, read off
//! the node's mount REGISTRY (not its live snapshot — a mount is a fact about what the daemon runs,
//! not a fact that rides the pushed `WireSnapshot` stream). Human (default): an aligned table whose
//! header survives an empty result, and whose PRODUCT column is OMITTED entirely against a node that
//! does not advertise `FEATURE_MOUNT_CLASS` — see the capability paragraph below; `--json`: the flat
//! array `crate::cmd::trade::render` documents, with an `asset_class_known` key riding every row for
//! the SAME reason (see that paragraph too — it is one fact threaded to both renderers, not a
//! table-only fix).
//!
//! ⚠ **This reads the registry through the SAME call `crate::cmd::trade_status`'s registry half
//! makes, and opens no second connection kind.**
//! `vike_tradehub_client::strategy_status_with_features` is a single short-lived `Scope::Read`
//! request-response (`Request::StrategyStatus`) — nothing like
//! `crate::cmd::trade::connect_observe`'s subscribe-and-wait-for-a-frame sequence, which is what
//! `order ls`/`position ls` need because their rows come off the pushed snapshot. A mount doesn't
//! live on that stream at all, so borrowing the snapshot machinery here would be reaching for the
//! wrong read rather than reusing the right one.
//!
//! ⚠ **This used to call the features-DISCARDING `strategy_status` wrapper instead, and that was a
//! real defect, not a simplification: `strategy_status` and `strategy_status_with_features` are NOT
//! interchangeable, because the features list is what
//! [`crate::cmd::trade::render::mounts_table`]'s AND [`crate::cmd::trade::render::mounts_json`]'s
//! `knows_class` parameter needs.** Without it, `ls` rendered/serialized an em dash / a bare `null`
//! for every `asset_class: None` regardless of WHY it was `None` — collapsing "an old node that
//! cannot carry the field" into "a current, unmigrated mount that genuinely names no product," which
//! `vike_tradehub_client::wire::WireMountRow::asset_class`'s own doc says sends an operator (or an
//! unattended agent parsing `--json`, which has no outside context to catch the false inference the
//! way a human glancing at a dash might) to upgrade a daemon that may already be current.
//! `crate::cmd::trade_status::run` already threads this through `registry_lines`; `run_ls` here now
//! does the same for BOTH renderers: `strategy_status_with_features` returns `(status, features)`,
//! `knows_class` is computed from `features.iter().any(|f| f ==
//! vike_tradehub_client::proto::FEATURE_MOUNT_CLASS)` — the identical expression `crate::cmd::trade`'s
//! own `run_status` uses for the REPL's `status` — and passed to `mounts_table` AND `mounts_json`
//! explicitly, since it is a fact about the ANSWERING NODE that no individual
//! [`crate::cmd::trade::render::MountRow`] carries.
//!
//! A too-old node (no `strategy-verbs` capability), an auth refusal and a node-side error are all
//! reported through `crate::cmd::trade_status::failure_lines`/`failure_exit` — the same wording and
//! rung `trade status` uses for the identical failure, rather than a second copy of that
//! classification.
//!
//! # `mount` / `unmount` — the LIFECYCLE half, one-shot
//!
//! Both MIRROR `crate::cmd::trade`'s own REPL grammar (`parse_mount`/`parse_unmount`) rather than
//! inventing a second one: the same flags, the same meanings, the venue leading exactly as it
//! already does at the prompt. The one thing that differs is mechanical, not semantic: the REPL
//! reads one line of TEXT, where whitespace is only ever a token separator the operator typed
//! themselves, while a one-shot invocation receives the OS's own ARGV, where quoting has already
//! been resolved and every element is exactly what the operator meant — whitespace included.
//!
//! ⚠ **`crate::cmd::trade::oneshot::take_wrapper_flags` walks that argv directly and never joins
//! or re-splits it — see that function's own doc for the two real defects (one in a properly
//! quoted `--params` value's own whitespace, one in `--params` swallowing a trailing wrapper flag)
//! that a join-then-split design produced across two rounds of this file, both traced to the same
//! root cause: throwing away boundary information the OS had already given for free.** Because of
//! that, `--params` is now an ORDINARY single-value flag exactly like `--name`/`--rhai`/`--id`/
//! `--account` — it takes the ONE next argv element, verbatim, and is never reassembled from
//! pieces. Flag order stops mattering entirely (relative to `--node`/`--yes`/`--json` AND to each
//! other) except for the one rule `--reason` has always had: it must come LAST, because it
//! consumes everything after it.
//!
//! ⚠ **A consequence, stated rather than hidden: an UNQUOTED `--params {"gamma": 0.0008}` no
//! longer works.** The shell has already split it into separate argv elements before this program
//! ever runs, and [`parse_mount_args`] does not reassemble them — reassembly is the defect the
//! paragraph above describes, not a convenience worth keeping a corruption risk for. Instead it
//! detects the shape (the JSON fails to parse alone, and a run of non-flag elements follows it)
//! and refuses with an error naming every piece it saw and the corrected, QUOTED command line — an
//! actionable refusal beats a silently corrupted strategy config.
//!
//! - `mount <venue> <symbol> <interval> (--name <strategy> | --rhai <path-on-the-node>)
//!   [--account <LABEL|DEFAULT>] [--id <mount-id>] [--params <json>] --node <host:port> [--yes]
//!   [--json] [--reason <text>]`, every flag in ANY order except `--reason` (must be last) —
//!   **`--name` XOR `--rhai` is refused by the PARSER**, naming both spellings:
//!   `WireCommand::MountStrategy`'s contract is an exclusive choice, and a client that let the
//!   node discover that spends a round trip saying what the grammar already knew. ⚠ `--rhai` is a
//!   path on the **NODE's** filesystem, not this machine's, and [`MOUNT_USAGE`] says so.
//! - `unmount <mount-id> --node <host:port> [--yes] [--json] [--reason <text>]` — removes one mount
//!   BY ID. The node cancels that mount's attributed live orders and saves its durable state;
//!   **POSITIONS ARE NOT FLATTENED** (`trade position flatten`/`close-all` are the verbs that close
//!   one), and [`UNMOUNT_USAGE`] says so in exactly those words — an operator who reads "unmount" as
//!   "get me out" and is wrong about it is left holding an unattended position.
//!
//! Both resolve to a [`crate::cmd::verbs::Verb`] through their own PURE parser
//! ([`parse_mount_args`]/[`parse_unmount_args`]) and are executed by
//! [`crate::cmd::trade::oneshot::execute_write`] — the SAME preview + guardrail + confirm + send
//! engine `crate::cmd::trade::order`'s and `crate::cmd::trade::position`'s write verbs already
//! share. No second send path, no second prompt, no second outcome classifier.
//!
//! # The account: threaded onto the wire, never refused
//!
//! `--account` reaches [`crate::cmd::verbs::Verb::MountStrategy`]'s `account` field and then
//! `WireCommand::MountStrategy`'s UNCHANGED. This module never calls
//! `crate::cmd::trade::selector::refuse_an_unaddressable_book` — that refusal is READ-side only (its
//! own doc says so: it exists because a snapshot's per-row views carry no account field to check a
//! label against, a fact about READS with nothing to do with mounting). `FEATURE_MOUNT_ACCOUNT` is
//! the capability that gates a mount naming an account, and
//! `vike_tradehub_client::remote_control`'s `required_feature` — consulted inside
//! `RemoteControlHandle::try_command_with_reason`, which `execute_write` already calls — is what
//! refuses it against a node too old to route it; this module re-implements none of that check.
//!
//! ⚠ **This module's doc used to say the LIFECYCLE verbs "stay one-shot spellings the group layer
//! refuses BY NAME until their own tasks land" — that task is this one, and it has landed.**
//! `crate::cmd::trade::plane::RETIRED_FLAT_SPELLINGS` still refuses the BARE, group-less spelling
//! (`vike-cli trade mount …`, with no `strategy` word) — correctly, since every verb lives in a
//! group now — but it points HERE for the replacement, and here is where `mount`/`unmount` actually
//! run.

use std::process::ExitCode;

use serde_json::{Value, json};

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::cmd::nodekeys::NodeKeyring;
use crate::cmd::trade::oneshot::{self, WriteCtx};
use crate::cmd::trade::render::{mount_rows, mounts_json, mounts_table};
use crate::cmd::verbs::Verb;
use crate::exit::{CliError, Exit};

/// The group's own verb roster: name, one-line description. [`usage`] and [`verb_names`] are both
/// DERIVED from this, so a verb cannot be listed in one place and missing from another.
pub(crate) const VERBS: &[(&str, &str)] = &[
    ("ls", "list the node's mounted-strategy registry"),
    ("mount", "add a strategy to the running node — --name XOR --rhai is required"),
    ("unmount", "remove one mount by id — POSITIONS ARE NOT FLATTENED"),
];

const COMMAND: &str = "trade strategy";

/// The group-level usage/help block, with the verb list rendered from [`VERBS`] rather than typed
/// out a second time.
fn usage() -> String {
    let verbs = VERBS
        .iter()
        .map(|(name, desc)| format!("  {name:<10} {desc}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "usage: vike-cli trade strategy <verb> [options]\n\nverbs:\n{verbs}\n\n  -h, --help   this \
         message"
    )
}

/// The verb names alone, `|`-joined — the roster half of the "needs a verb" / "unknown verb"
/// messages, derived from [`VERBS`] so it cannot name a different set than [`usage`] does.
fn verb_names() -> String {
    VERBS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(" | ")
}

/// One resolved READ/meta `strategy` sub-command — the LIFECYCLE (WRITE) verbs bypass this type
/// entirely, exactly as `crate::cmd::trade::order`'s and `crate::cmd::trade::position`'s do (see
/// `order`'s module doc): their result is a [`crate::cmd::verbs::Verb`], the SAME shared
/// wire-vocabulary type the REPL and the `mcp` tools already construct through, and wrapping it a
/// second time here would be exactly the second construction site that type exists to prevent.
#[derive(Debug)]
enum StrategyVerb {
    Ls(LsArgs),
}

/// Parse everything after the `strategy` word: the verb, then that verb's own flags. PURE.
fn parse(mut args: impl Iterator<Item = String>) -> Result<StrategyVerb, String> {
    let Some(verb) = args.next() else {
        return Err(format!("`trade strategy` needs a verb ({})", verb_names()));
    };
    match verb.as_str() {
        "-h" | "--help" | "help" => help_requested(),
        "ls" => Ok(StrategyVerb::Ls(parse_ls(args)?)),
        other => Err(format!("unknown `trade strategy` verb '{other}' ({})", verb_names())),
    }
}

/// `ls`'s own parsed line — no book/symbol positional: a mount is not addressed by an account, and
/// the venue/symbol/interval addressing key each [`vike_tradehub_client::wire::WireMountRow`]
/// already carries is shown, not filtered on, by this first cut.
#[derive(Debug)]
struct LsArgs {
    node: String,
    json: bool,
}

/// `ls`'s own grammar: flags only. PURE.
fn parse_ls(args: impl Iterator<Item = String>) -> Result<LsArgs, String> {
    let mut node: Option<String> = None;
    let mut json = false;
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--node" => node = Some(flags.value(&flag, inline)?),
            "--json" => {
                no_value(&flag, inline)?;
                json = true;
            }
            "-h" | "--help" => return help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let node = node.ok_or("--node <host:port> is required")?;
    Ok(LsArgs { node, json })
}

/// Entry point [`crate::cmd::trade::run`] routes `trade strategy …` to. `args` is everything AFTER
/// the `strategy` word. `policy_max_notional` is threaded to the LIFECYCLE write verbs' advisory
/// guardrail — see [`oneshot::WriteCtx`]. The two WRITE verbs are claimed BEFORE [`parse`], for the
/// same reason `crate::cmd::trade::order`'s and `crate::cmd::trade::position`'s are.
pub(crate) fn run(
    args: impl Iterator<Item = String>,
    keys: &NodeKeyring,
    policy_max_notional: Option<f64>,
) -> ExitCode {
    let mut args = args.peekable();
    match args.peek().map(String::as_str) {
        Some("mount") => {
            args.next();
            return run_mount(args, keys, policy_max_notional);
        }
        Some("unmount") => {
            args.next();
            return run_unmount(args, keys, policy_max_notional);
        }
        _ => {}
    }
    match parse(args) {
        Ok(StrategyVerb::Ls(a)) => run_ls(a, keys),
        Err(msg) => exit_for_parse_error(COMMAND, &usage(), &msg),
    }
}

fn run_ls(a: LsArgs, keys: &NodeKeyring) -> ExitCode {
    // The OBSERVE key specifically: `Request::StrategyStatus` is read-only and answers under either
    // scope (`crate::cmd::trade_status`'s module doc), but a control key cannot authenticate a read
    // the node's own handshake still verifies against the scope it was presented for.
    let Some((key, _origin)) = keys.observe() else {
        eprintln!("{}", keys.observe_absent_message(COMMAND));
        return ExitCode::FAILURE;
    };
    // `_with_features`, never the plain `strategy_status` wrapper — see this module's doc for the
    // Critical fix this round covers: the PRODUCT column can only be rendered honestly by a reader
    // that knows whether the node advertised `FEATURE_MOUNT_CLASS`.
    let (status, features) = match vike_tradehub_client::strategy_status_with_features(
        a.node.as_str(),
        key.as_bytes(),
    ) {
        Ok(v) => v,
        Err(e) => {
            for line in crate::cmd::trade_status::failure_lines(&a.node, &e) {
                eprintln!("{line}");
            }
            return crate::cmd::trade_status::failure_exit(&e).into();
        }
    };
    let knows_class =
        features.iter().any(|f| f == vike_tradehub_client::proto::FEATURE_MOUNT_CLASS);
    let rows = mount_rows(&status);
    if a.json {
        println!("{}", mounts_json(&rows, knows_class));
    } else {
        println!("{}", mounts_table(&rows, knows_class));
    }
    ExitCode::SUCCESS
}

// ---- the LIFECYCLE verbs, one-shot (task 8) --------------------------------------------------

/// `mount`'s usage/help block.
const MOUNT_USAGE: &str = "usage: vike-cli trade strategy mount <venue> <symbol> <interval> \
                            (--name <strategy> | --rhai <path-on-the-node>) \
                            [--account <LABEL|DEFAULT>] [--id <mount-id>] \
                            [--params <json-object>] --node <host:port> [--yes] [--json] \
                            [--reason <text to end of line>]\n\n\
                            Adds a strategy to the RUNNING node's core with no restart. Every \
                            flag may appear in ANY ORDER except --reason, which must be LAST (it \
                            consumes everything after it).\n\n\
                            --name and --rhai are EXCLUSIVE and one is REQUIRED: a registry name \
                            or a Rhai script path, never both, never neither.\n\
                            ⚠ --rhai is a path on the NODE's filesystem, not this machine's — the \
                            node opens it, and a path that exists here proves nothing about \
                            there.\n\n\
                            --account names WHICH ACCOUNT of <venue> the mount trades and reads: \
                            omitted names none, DEFAULT names the venue's unlabelled account \
                            deliberately, a label names that account. On a venue this node runs \
                            TWO engines of, omitting it is REFUSED by the node rather than \
                            resolved to the default.\n\n\
                            --params must be a JSON OBJECT (the `[strategy.params]` table) and \
                            takes EXACTLY ONE shell argument — QUOTE it, e.g. --params \
                            '{\"gamma\": 0.0008}'. An unquoted value the shell has already split \
                            on its own whitespace is refused with the pieces it saw and the \
                            corrected command line; it is never silently reassembled.\n\n\
                            ⚠ --reason must be LAST — everything after it, to end of line, is \
                            taken verbatim as the recorded rationale.";

/// `unmount`'s usage/help block.
const UNMOUNT_USAGE: &str = "usage: vike-cli trade strategy unmount <mount-id> --node \
                              <host:port> [--yes] [--json] [--reason <text to end of line>]\n\n\
                              Removes one mount from the running core, BY ID. The node cancels \
                              that mount's attributed live orders and saves its durable state.\n\n\
                              POSITIONS ARE NOT FLATTENED — `trade position flatten`/`close-all` \
                              are the verbs that close a position. An operator who reads unmount \
                              as \"get me out\" and is wrong about it is left holding an \
                              unattended position.\n\n\
                              A mount id is ONE token: the explicit --id given at mount time, or \
                              the node-derived {venue}__{symbol}__{interval}.";

/// `mount <venue> <symbol> <interval> (--name <strategy> | --rhai <path>)
/// [--account <LABEL|DEFAULT>] [--id <mount-id>] [--params <json>]` — the mount-shape grammar ONLY
/// (no `--node`/`--yes`/`--json`/`--reason`, which [`run_mount`] extracts first via
/// [`oneshot::take_wrapper_flags`]). Mirrors `crate::cmd::trade`'s own REPL `parse_mount` — the same
/// flags, the same meanings, the venue leading exactly as it already does there. PURE: no network.
///
/// **`--name` XOR `--rhai`.** Both-or-neither is refused HERE, naming both spellings, rather than
/// spending a round trip on a refusal the grammar already knew — `WireCommand::MountStrategy`'s
/// contract is an exclusive choice. ⚠ `--rhai` is a path on the **NODE's** filesystem, not this
/// machine's.
///
/// **`--account`** threads straight onto [`Verb::MountStrategy`]'s `account` field and then the
/// wire, unchanged — see this module's doc for why no selector/refusal runs over it. Validated with
/// `vike_model::account_keys::parse_wire_account`, the one reader every surface (the REPL, this
/// one, the `mcp` tool) shares, so the grammar is never spelled twice.
///
/// **`--params` takes EXACTLY ONE `tokens` element, verbatim** — an ORDINARY value-taking flag,
/// same as `--name`/`--rhai`/`--id`/`--account`, and NEVER reassembled from pieces (see this
/// module's doc for why that reassembly was the defect a previous round of this function had to
/// remove). A value that fails to parse as JSON, with a run of non-flag elements following it, is
/// almost certainly an UNQUOTED JSON object the shell already split on whitespace before this
/// program ran — that shape is refused with a message naming every piece and the corrected,
/// quoted command line, never silently rejoined. Absent ⇒ `{}`, an empty table; present-and-empty
/// is a typo and is refused, exactly as the REPL refuses it.
pub(crate) fn parse_mount_args(tokens: &[&str]) -> Result<Verb, CliError> {
    let mut name: Option<String> = None;
    let mut rhai: Option<String> = None;
    let mut id: Option<String> = None;
    let mut account: Option<String> = None;
    let mut params: Option<Value> = None;
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0usize;
    while i < tokens.len() {
        let tok = tokens[i];
        // Both spellings, the same pair the REPL's own `parse_mount` accepts: `--flag value` and
        // `--flag=value`.
        let (flag, inline) = match tok.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v)),
            _ => (tok, None),
        };
        match flag {
            "--name" | "--rhai" | "--id" | "--account" => {
                let value = match inline {
                    Some(v) => {
                        i += 1;
                        v
                    }
                    None => {
                        let v = *tokens.get(i + 1).ok_or_else(|| {
                            CliError::usage(format!("mount: {flag} needs a value\n{MOUNT_USAGE}"))
                        })?;
                        i += 2;
                        v
                    }
                };
                // A value that is itself a flag means the operator's value went missing and the
                // NEXT flag was eaten as one — the failure `submit --coid has space` taught.
                if value.is_empty() || value.starts_with("--") {
                    return Err(CliError::usage(format!(
                        "mount: {flag} needs a value, got {value:?}\n{MOUNT_USAGE}"
                    )));
                }
                let slot = match flag {
                    "--name" => &mut name,
                    "--rhai" => &mut rhai,
                    "--account" => &mut account,
                    _ => &mut id,
                };
                if slot.is_some() {
                    return Err(CliError::usage(format!(
                        "mount: {flag} given twice\n{MOUNT_USAGE}"
                    )));
                }
                *slot = Some(value.to_string());
            }
            "--params" => {
                let value = match inline {
                    Some(v) => {
                        i += 1;
                        v
                    }
                    None => {
                        let v = *tokens.get(i + 1).ok_or_else(|| {
                            CliError::usage(format!("mount: --params needs a value\n{MOUNT_USAGE}"))
                        })?;
                        i += 2;
                        v
                    }
                };
                if params.is_some() {
                    return Err(CliError::usage(format!(
                        "mount: --params given twice\n{MOUNT_USAGE}"
                    )));
                }
                if value.starts_with("--") {
                    return Err(CliError::usage(format!(
                        "mount: --params needs a value, got {value:?}\n{MOUNT_USAGE}"
                    )));
                }
                if value.is_empty() {
                    return Err(CliError::usage(format!(
                        "mount: --params needs a JSON object — drop the flag entirely for an \
                         empty params table\n{MOUNT_USAGE}"
                    )));
                }
                match serde_json::from_str::<Value>(value) {
                    Ok(v) if v.is_object() => params = Some(v),
                    Ok(v) => {
                        return Err(CliError::usage(format!(
                            "mount: --params must be a JSON OBJECT (the `[strategy.params]` \
                             table), got {v}\n{MOUNT_USAGE}"
                        )));
                    }
                    Err(e) => {
                        // A run of non-flag tokens right after the value is the signature an
                        // UNQUOTED JSON object leaves once the shell has already split it on
                        // whitespace — `--params` takes exactly ONE shell argument and this
                        // parser never reassembles one (reassembly is what silently corrupted a
                        // quoted value's own internal whitespace, see this module's doc). Detect
                        // the shape and teach the fix rather than reporting a bare parse error
                        // over a fragment the operator never meant to stand alone.
                        let mut pieces = vec![value];
                        let mut j = i;
                        while let Some(next) = tokens.get(j).copied() {
                            if next.starts_with("--") {
                                break;
                            }
                            pieces.push(next);
                            j += 1;
                        }
                        if pieces.len() > 1 {
                            let corrected = pieces.join(" ");
                            return Err(CliError::usage(format!(
                                "mount: --params takes exactly ONE shell argument, and this \
                                 value arrived split into {} pieces {pieces:?} — the shell \
                                 already broke it on whitespace before this program ran, and it \
                                 is never reassembled. Quote the whole object as one \
                                 argument:\n  --params '{corrected}'\n{MOUNT_USAGE}",
                                pieces.len()
                            )));
                        }
                        return Err(CliError::usage(format!(
                            "mount: --params is not JSON ({e}): {value}\n{MOUNT_USAGE}"
                        )));
                    }
                }
            }
            other if other.starts_with("--") => {
                return Err(CliError::usage(format!(
                    "mount: unexpected flag {other:?}\n{MOUNT_USAGE}"
                )));
            }
            _ => {
                positional.push(tok);
                i += 1;
            }
        }
    }

    if positional.len() != 3 {
        return Err(CliError::usage(MOUNT_USAGE.to_string()));
    }
    match (&name, &rhai) {
        (Some(_), Some(_)) => {
            return Err(CliError::usage(format!(
                "mount: --name and --rhai are EXCLUSIVE — a mount's strategy source is either a \
                 registry name or a Rhai script path, never both\n{MOUNT_USAGE}"
            )));
        }
        (None, None) => {
            return Err(CliError::usage(format!(
                "mount: a strategy source is required — pass --name <strategy> (a registry name) \
                 or --rhai <path> (a script on the NODE's filesystem)\n{MOUNT_USAGE}"
            )));
        }
        _ => {}
    }

    // Refused HERE, in this parser's own vocabulary, rather than a round trip away — but read with
    // `parse_wire_account`, the one authority on the grammar, so this edge adds a message and not a
    // second set of rules. ⚠ It admits `DEFAULT`, which `policy.toml` refuses: on the wire that
    // spelling is how an operator says "the unlabelled account, deliberately" as distinct from
    // saying nothing, and at two engines of one venue those are different answers.
    if let Some(a) = &account
        && let Err(e) = vike_model::account_keys::parse_wire_account(a)
    {
        return Err(CliError::usage(format!(
            "mount: --account {a:?} — {e}. Drop the flag to name no account, or pass DEFAULT to \
             name the venue's unlabelled account deliberately\n{MOUNT_USAGE}"
        )));
    }

    Ok(Verb::MountStrategy {
        venue: positional[0].to_string(),
        account,
        symbol: positional[1].to_string(),
        interval: positional[2].to_string(),
        // `None` lets the node derive `{venue}__{symbol}__{interval}`. Deliberately NOT derived
        // here: the derivation is the node's rule, and a client copy of it would be a second one to
        // keep in step for no gain — the node accepts the absence and answers with its own answer.
        controller_id: id,
        name,
        rhai,
        params: params.unwrap_or_else(|| json!({})),
    })
}

fn run_mount(
    args: impl Iterator<Item = String>,
    keys: &NodeKeyring,
    policy_max_notional: Option<f64>,
) -> ExitCode {
    let (rest, flags) = match oneshot::take_wrapper_flags(args) {
        Ok(v) => v,
        Err(msg) => {
            return exit_for_parse_error(&format!("{COMMAND} mount"), MOUNT_USAGE, &msg);
        }
    };
    if flags.help {
        println!("{MOUNT_USAGE}");
        return ExitCode::SUCCESS;
    }
    let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
    let verb = match parse_mount_args(&refs) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vike-cli {COMMAND} mount: {}", e.msg);
            return e.exit.into();
        }
    };
    let Some(node) = flags.node else {
        eprintln!("vike-cli {COMMAND} mount: --node <host:port> is required\n{MOUNT_USAGE}");
        return Exit::Usage.into();
    };
    run_write(verb, node, keys, policy_max_notional, flags.yes, flags.json, flags.reason, "mount")
}

/// `unmount <mount-id>` — no book: the client-order-id-shaped mount id already names the engine it
/// lives on, same reasoning `crate::cmd::trade::order`'s `cancel` uses. Mirrors
/// `crate::cmd::trade`'s own REPL `parse_unmount`. PURE.
pub(crate) fn parse_unmount_args(tokens: &[&str]) -> Result<Verb, CliError> {
    match tokens {
        [id] => Ok(Verb::UnmountStrategy { controller_id: (*id).to_string() }),
        _ => Err(CliError::usage(UNMOUNT_USAGE.to_string())),
    }
}

fn run_unmount(
    args: impl Iterator<Item = String>,
    keys: &NodeKeyring,
    policy_max_notional: Option<f64>,
) -> ExitCode {
    let (rest, flags) = match oneshot::take_wrapper_flags(args) {
        Ok(v) => v,
        Err(msg) => {
            return exit_for_parse_error(&format!("{COMMAND} unmount"), UNMOUNT_USAGE, &msg);
        }
    };
    if flags.help {
        println!("{UNMOUNT_USAGE}");
        return ExitCode::SUCCESS;
    }
    let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
    let verb = match parse_unmount_args(&refs) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vike-cli {COMMAND} unmount: {}", e.msg);
            return e.exit.into();
        }
    };
    let Some(node) = flags.node else {
        eprintln!("vike-cli {COMMAND} unmount: --node <host:port> is required\n{UNMOUNT_USAGE}");
        return Exit::Usage.into();
    };
    run_write(verb, node, keys, policy_max_notional, flags.yes, flags.json, flags.reason, "unmount")
}

/// Build a [`WriteCtx`] and hand `verb` to [`oneshot::execute_write`] — the ONE tail every LIFECYCLE
/// write verb's `run_*` function shares once its own grammar has resolved a `Verb` and a `--node`,
/// the same tail `crate::cmd::trade::order`'s and `crate::cmd::trade::position`'s own `run_write`
/// helpers share for the order-write verbs.
#[allow(clippy::too_many_arguments)]
fn run_write(
    verb: Verb,
    node: String,
    keys: &NodeKeyring,
    policy_max_notional: Option<f64>,
    yes: bool,
    json: bool,
    reason: Option<String>,
    verb_word: &str,
) -> ExitCode {
    let ctx = WriteCtx { node, keys, policy_max_notional, yes, json, reason };
    match oneshot::execute_write(verb, &ctx) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli {COMMAND} {verb_word}: {}", e.msg);
            e.exit.into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<StrategyVerb, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_verb_names_the_roster() {
        let e = parsed(&[]).unwrap_err();
        assert!(e.contains("needs a verb"), "{e}");
        assert!(e.contains("ls"), "{e}");
    }

    #[test]
    fn help_short_circuits_before_any_verb_specific_parsing() {
        for spelling in ["-h", "--help", "help"] {
            let e = parsed(&[spelling]).unwrap_err();
            assert_eq!(e, crate::cmd::args::HELP_SENTINEL);
        }
    }

    #[test]
    fn an_unknown_verb_names_itself_and_the_roster() {
        let e = parsed(&["bogus"]).unwrap_err();
        assert!(e.contains("bogus"), "{e}");
        assert!(e.contains("ls"), "{e}");
    }

    #[test]
    fn ls_requires_a_node() {
        let e = parsed(&["ls"]).unwrap_err();
        assert!(e.contains("--node"), "{e}");
    }

    #[test]
    fn ls_parses_the_node_and_json_flags() {
        let v = parsed(&["ls", "--node", "n:1", "--json"]).expect("a valid ls line parses");
        let StrategyVerb::Ls(a) = v;
        assert_eq!(a.node, "n:1");
        assert!(a.json);
    }

    #[test]
    fn ls_help_short_circuits() {
        let e = parsed(&["ls", "--help"]).unwrap_err();
        assert_eq!(e, crate::cmd::args::HELP_SENTINEL);
    }

    #[test]
    fn an_unknown_flag_is_rejected_by_name() {
        let e = parsed(&["ls", "--node", "n:1", "--bogus"]).unwrap_err();
        assert!(e.contains("--bogus"), "{e}");
    }

    // ---- the LIFECYCLE verbs' own parsers (task 8) ----------------------------------------------

    #[test]
    fn mount_requires_name_xor_rhai_and_names_both_spellings() {
        let e =
            parse_mount_args(&["binance", "BTCUSDT", "1m"]).expect_err("one source is required");
        assert!(e.msg.contains("--name"), "name both spellings: {}", e.msg);
        assert!(e.msg.contains("--rhai"), "name both spellings: {}", e.msg);

        let e = parse_mount_args(&["binance", "BTCUSDT", "1m", "--name", "x", "--rhai", "y.rhai"])
            .expect_err("both is a parse error, not something the node discovers");
        assert!(e.msg.contains("--name") && e.msg.contains("--rhai"));
    }

    #[test]
    fn unmount_help_states_that_positions_are_not_flattened() {
        assert!(
            UNMOUNT_USAGE.contains("POSITIONS ARE NOT FLATTENED"),
            "an operator who reads unmount as `get me out` and is wrong is left holding an \
             unattended position"
        );
    }

    #[test]
    fn mount_parses_the_positionals_and_defaults_params_to_an_empty_object() {
        let v = parse_mount_args(&["binance", "BTCUSDT", "1m", "--name", "grid"]).unwrap();
        match v {
            Verb::MountStrategy {
                venue,
                symbol,
                interval,
                name,
                rhai,
                params,
                account,
                controller_id,
            } => {
                assert_eq!(venue, "binance");
                assert_eq!(symbol, "BTCUSDT");
                assert_eq!(interval, "1m");
                assert_eq!(name.as_deref(), Some("grid"));
                assert_eq!(rhai, None);
                assert_eq!(params, json!({}));
                assert_eq!(account, None);
                assert_eq!(controller_id, None);
            }
            other => panic!("expected a MountStrategy, got {other:?}"),
        }
    }

    #[test]
    fn mount_threads_the_account_and_id_onto_the_verb() {
        let v = parse_mount_args(&[
            "binance",
            "BTCUSDT",
            "1m",
            "--rhai",
            "strategies/x.rhai",
            "--account",
            "ALT",
            "--id",
            "my-mount",
        ])
        .unwrap();
        match v {
            Verb::MountStrategy { account, controller_id, rhai, .. } => {
                assert_eq!(account.as_deref(), Some("ALT"));
                assert_eq!(controller_id.as_deref(), Some("my-mount"));
                assert_eq!(rhai.as_deref(), Some("strategies/x.rhai"));
            }
            other => panic!("expected a MountStrategy, got {other:?}"),
        }
    }

    /// ⚠ **The regression this task exists to guard against, stated as its own test.** The critical
    /// fix that made `order submit`/`mass-cancel` and `position flatten`/`close-all` REFUSE a
    /// labelled book (`crate::cmd::trade::selector::refuse_an_unroutable_account`) must NEVER reach
    /// `mount`: this verb never goes through that module's `venue/LABEL` grammar at all — its venue
    /// is a bare positional and `--account` is a separate flag validated straight off
    /// `vike_model::account_keys::parse_wire_account` — and `crates/vike-tradehub/src/server.rs`'s
    /// `lower_command` genuinely reads `WireCommand::MountStrategy`'s `account` field into a real
    /// `MountSpec`, unlike the order-carrying commands' `OrderRequest`, which has none. So a
    /// labelled account here is not merely untouched by the new refusal — there is no `Book` in this
    /// function's data flow for that refusal to ever be called on. This test pins that a labelled
    /// account still PASSES and still THREADS onto the verb after the fix landed, duplicating
    /// `mount_threads_the_account_and_id_onto_the_verb`'s assertion deliberately: a future edit that
    /// broke this by making `mount` share the order/position grammar should redden a test whose name
    /// says exactly why that would be wrong.
    ///
    /// ⚠ **The refusal this test was written against is DELETED**, and two sentences above are
    /// history rather than the tree: stage 5 of
    /// `docs/superpowers/specs/2026-09-22-the-order-payload-names-its-account-design.md` removed
    /// `refuse_an_unroutable_account` on 2026-09-26, and `vike_model::OrderRequest` DOES carry an
    /// account now — it was "has none" when this was written. The order/position verbs thread a
    /// label too, so the two planes no longer differ in whether a label passes. What this test still
    /// guards is the part that outlived its reason for being written: `mount` takes its account from
    /// a bare `--account` flag, never from the book grammar, and a labelled one threads onto the
    /// verb. The name is kept so its history reads true.
    #[test]
    fn mount_with_a_labelled_account_still_passes_after_the_order_write_refusal_landed() {
        let v =
            parse_mount_args(&["binance", "BTCUSDT", "1m", "--name", "grid", "--account", "ALT"])
                .expect("mount never refuses a labelled account — it has no Book to refuse");
        match v {
            Verb::MountStrategy { account, .. } => assert_eq!(
                account.as_deref(),
                Some("ALT"),
                "the label must still thread onto the wire's MountStrategy field"
            ),
            other => panic!("expected a MountStrategy, got {other:?}"),
        }
    }

    /// The wire's three account states mean the DEFAULT spelling names the account POSITIVELY
    /// (`"DEFAULT"`) — same rule `crate::cmd::trade::order`'s `parse_submit_args` pins for a bare
    /// book, and the reason `--account DEFAULT` is admitted here even though `policy.toml` refuses
    /// that spelling for itself (this module's doc explains the asymmetry).
    #[test]
    fn mount_account_default_is_the_positive_wire_spelling() {
        let v = parse_mount_args(&[
            "binance",
            "BTCUSDT",
            "1m",
            "--name",
            "grid",
            "--account",
            "DEFAULT",
        ])
        .unwrap();
        match v {
            Verb::MountStrategy { account, .. } => assert_eq!(account.as_deref(), Some("DEFAULT")),
            other => panic!("expected a MountStrategy, got {other:?}"),
        }
    }

    #[test]
    fn mount_rejects_an_invalid_account_naming_default() {
        let e =
            parse_mount_args(&["binance", "BTCUSDT", "1m", "--name", "grid", "--account", "b a d"])
                .expect_err("an invalid account label is refused");
        assert!(e.msg.contains("DEFAULT"), "{}", e.msg);
    }

    /// An operator who forgets to quote `--params {"gamma": 0.0008}` hands the SHELL, not this
    /// binary, the job of splitting on whitespace — the JSON is already SEPARATE argv elements
    /// before `main` ever runs, and there is no way for this parser to tell that apart from a
    /// coid or symbol that also happens not to start with `--`. Silently reassembling them was the
    /// defect two previous rounds of this file had to remove; this test drives the REAL
    /// `take_wrapper_flags` (the only entry point that can reproduce the shape at all) and pins the
    /// TEACHING refusal instead: every piece named, and the corrected, quoted command line printed.
    #[test]
    fn mount_params_unquoted_and_split_by_the_shell_is_refused_with_the_fix() {
        let args =
            ["binance", "BTCUSDT", "1m", "--name", "grid", "--params", "{\"gamma\":", "0.0008}"];
        let (rest, _flags) =
            oneshot::take_wrapper_flags(args.iter().map(|s| s.to_string())).unwrap();
        let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
        let e =
            parse_mount_args(&refs).expect_err("an unquoted, shell-split JSON value is refused");
        assert!(e.msg.contains("ONE shell argument"), "{}", e.msg);
        assert!(
            e.msg.contains("--params '{\"gamma\": 0.0008}'"),
            "names the corrected, quoted command line: {}",
            e.msg
        );
    }

    /// **The defect a previous round of this fix introduced, and this pins the cure for**: a
    /// PROPERLY QUOTED `--params` argument (`--params '{"note":"a  b"}'` at a real shell prompt)
    /// reaches this process as ONE `String` with its internal whitespace intact — the OS never
    /// touches it. `--params` is now an ORDINARY single-element flag, taken verbatim and never
    /// reassembled from pieces, so that whitespace survives with no special handling at all. This
    /// test still drives the REAL `take_wrapper_flags` end to end (not a hand-built token array),
    /// because that is the entry point an operator's argv actually reaches.
    #[test]
    fn mount_params_with_embedded_whitespace_survives_take_wrapper_flags_intact() {
        let args =
            ["binance", "BTCUSDT", "1m", "--name", "grid", "--params", "{\"note\":\"a  b\"}"];
        let (rest, _flags) =
            oneshot::take_wrapper_flags(args.iter().map(|s| s.to_string())).unwrap();
        let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
        let v = parse_mount_args(&refs).unwrap();
        match v {
            Verb::MountStrategy { params, .. } => assert_eq!(
                params.get("note").and_then(Value::as_str),
                Some("a  b"),
                "the two spaces inside the quoted JSON string must survive byte-for-byte: {params}"
            ),
            other => panic!("expected a MountStrategy, got {other:?}"),
        }
    }

    /// `--node`/`--yes`/`--json` must survive `--params` regardless of which side of it they land
    /// on — the exact regression a previous round of this fix introduced by capturing the raw line
    /// BEFORE those wrapper flags were stripped, which let `--params` (reading to end of line)
    /// swallow whatever wrapper flag followed it. `--params` is a one-element flag now, so nothing
    /// after it is at risk; this test pins that `--node` specifically, placed AFTER `--params` (the
    /// order `MOUNT_USAGE` itself documents), still reaches `WrapperFlags::node`.
    #[test]
    fn a_wrapper_flag_after_params_is_not_swallowed() {
        let args = [
            "binance",
            "BTCUSDT",
            "1m",
            "--name",
            "grid",
            "--params",
            "{\"gamma\":0.0008}",
            "--node",
            "host:1234",
            "--yes",
            "--json",
        ];
        let (rest, flags) =
            oneshot::take_wrapper_flags(args.iter().map(|s| s.to_string())).unwrap();
        assert_eq!(flags.node.as_deref(), Some("host:1234"));
        assert!(flags.yes);
        assert!(flags.json);
        let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
        let v = parse_mount_args(&refs).unwrap();
        match v {
            Verb::MountStrategy { params, .. } => assert_eq!(params, json!({ "gamma": 0.0008 })),
            other => panic!("expected a MountStrategy, got {other:?}"),
        }
    }

    #[test]
    fn mount_params_must_be_a_json_object() {
        let e =
            parse_mount_args(&["binance", "BTCUSDT", "1m", "--name", "grid", "--params", "[1]"])
                .expect_err("an array is not a params object");
        assert!(e.msg.contains("JSON OBJECT"), "{}", e.msg);
    }

    #[test]
    fn mount_params_with_no_value_at_all_is_a_usage_error() {
        let e = parse_mount_args(&["binance", "BTCUSDT", "1m", "--name", "grid", "--params"])
            .expect_err("--params with nothing after it needs a value");
        assert!(e.msg.contains("--params") && e.msg.contains("needs a value"), "{}", e.msg);
    }

    #[test]
    fn mount_params_explicitly_empty_is_a_typo_not_an_empty_table() {
        let e = parse_mount_args(&["binance", "BTCUSDT", "1m", "--name", "grid", "--params", ""])
            .expect_err("an explicit empty --params value is a typo, not an empty table");
        assert!(e.msg.contains("empty params table"), "{}", e.msg);
    }

    #[test]
    fn mount_wrong_positional_count_is_a_usage_error() {
        assert!(parse_mount_args(&["binance", "BTCUSDT"]).is_err());
        assert!(parse_mount_args(&["binance", "BTCUSDT", "1m", "extra", "--name", "x"]).is_err());
    }

    #[test]
    fn unmount_parses_a_bare_mount_id() {
        let v = parse_unmount_args(&["binance__BTCUSDT__1m"]).unwrap();
        assert_eq!(v, Verb::UnmountStrategy { controller_id: "binance__BTCUSDT__1m".to_string() });
    }

    #[test]
    fn unmount_rejects_zero_or_extra_tokens() {
        assert!(parse_unmount_args(&[]).is_err());
        assert!(parse_unmount_args(&["a", "b"]).is_err());
    }
}
