//! `vike-cli trade strategy` — the STRATEGY group's router. `ls` (what is mounted) and, since task 8
//! of the trade-CLI-plane, the two LIFECYCLE verbs `mount` / `unmount` are wired here as one-shot
//! spellings — the words that change what a running node IS rather than what its books hold.
//! `reset-budget` clears one mount's budget latch (decision 0239): `CommandWire::UnlatchMount`, behind
//! the node's `mount-unlatch` word.
//! `set-setting`, the group's third REPL lifecycle word, has no one-shot spelling yet and stays a
//! REPL-only verb (`crate::cmd::trade`'s module doc, "The strategy-LIFECYCLE half").
//!
//! # `ls`
//!
//! `vike-cli trade strategy ls --node <host:port> [--json]` — one row per mounted strategy, read off
//! the node's mount REGISTRY (not its live snapshot — a mount is a fact about what the daemon runs,
//! not a fact that rides the pushed `CoreSnapshotWire` stream). Human (default): an aligned table whose
//! header survives an empty result, and whose PRODUCT column is OMITTED entirely against a node that
//! does not advertise `FEATURE_MOUNT_CLASS` — see the capability paragraph below; `--json`: the flat
//! array `crate::cmd::trade::render` documents, with an `asset_class_known` key riding every row for
//! the SAME reason (see that paragraph too — it is one fact threaded to both renderers, not a
//! table-only fix).
//!
//! ⚠ **This reads the registry through the SAME call `crate::cmd::trade::status`'s registry half
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
//! `vike_tradehub_client::wire::MountViewWire::asset_class`'s own doc says sends an operator (or an
//! unattended agent parsing `--json`, which has no outside context to catch the false inference the
//! way a human glancing at a dash might) to upgrade a daemon that may already be current.
//! `crate::cmd::trade::status::run` already threads this through `registry_lines`; `run_ls` here now
//! does the same for BOTH renderers: `strategy_status_with_features` returns `(status, features)`,
//! `knows_class` is computed from `features.iter().any(|f| f ==
//! vike_tradehub_client::proto::FEATURE_MOUNT_CLASS)` — the identical expression `crate::cmd::trade`'s
//! own `run_status` uses for the REPL's `status` — and passed to `mounts_table` AND `mounts_json`
//! explicitly, since it is a fact about the ANSWERING NODE that no individual
//! [`crate::cmd::trade::render::MountRow`] carries.
//!
//! A too-old node (no `strategy-verbs` capability), an auth refusal and a node-side error are all
//! reported through `crate::cmd::trade::status::failure_lines`/`failure_exit` — the same wording and
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
//! ⚠ **`crate::cmd::trade::oneshot::WrapperArgs`' doc says why the argv is read element by element
//! and is never joined or re-split — the two real defects (one in a properly quoted `--params`
//! value's own whitespace, one in `--params` swallowing a trailing wrapper flag) that a
//! join-then-split design produced across two rounds of this file, both traced to the same root
//! cause: throwing away boundary information the OS had already given for free.** Because of that,
//! `--params` is an ORDINARY single-value flag exactly like `--strategy`/`--id` —
//! it takes the ONE next argv element, verbatim, and is never reassembled from pieces. Flag order
//! stops mattering entirely (relative to `--node`/`--yes`/`--json` AND to each other) except for
//! the one rule `--reason` has always had: it must come LAST, because it consumes everything after
//! it.
//!
//! ⚠ **A consequence, stated rather than hidden: an UNQUOTED `--params {"gamma": 0.0008}` no
//! longer works.** The shell has already split it into separate argv elements before this program
//! ever runs, and [`mount_verb`] does not reassemble them — reassembly is the defect the
//! paragraph above describes, not a convenience worth keeping a corruption risk for. Instead it
//! detects the shape (the JSON fails to parse alone, and more tokens were read as positionals than
//! a mount takes) and refuses with an error naming every piece it saw and the corrected, QUOTED
//! command line — an actionable refusal beats a silently corrupted strategy config.
//!
//! - `mount <account> <symbol> <interval> --strategy <path>
//!   [--id <mount-id>] [--params <json>] --node <host:port> [--yes]
//!   [--json] [--reason <text>]`, every flag in ANY order except `--reason` (must be last) —
//!   **a mount with no `--strategy` is refused by the PARSER**: `CommandWire::MountStrategy`'s
//!   contract is ONE source, the file, and a client that let the node discover that spends a round
//!   trip saying what the grammar already knew. The retired `--name`/`--rhai` are no flags any more,
//!   and the renamed `--file` is refused naming `--strategy`
//!   (`vike_client::refusals::RenamedArgumentRefusal`). ⚠ `--strategy` is a path under the
//!   **NODE's** `strategies/` folder, not this machine's, and [`MOUNT_PROSE`] says so.
//! - `unmount <mount-id> --node <host:port> [--yes] [--json] [--reason <text>]` — removes one mount
//!   BY ID. The node cancels that mount's attributed live orders and saves its durable state;
//!   **POSITIONS ARE NOT FLATTENED** (`trade position flatten`/`close-all` are the verbs that close
//!   one), and [`UNMOUNT_PROSE`] says so in exactly those words — an operator who reads "unmount" as
//!   "get me out" and is wrong about it is left holding an unattended position.
//!
//! `clap` reads the argv (the plane's one parser, `crate::cmd::trade`'s `Cli`: [`StrategyVerb`] is
//! this group's enum); both then resolve to a [`vike_client::verbs::Verb`] through their own PURE
//! rules ([`mount_verb`]/[`unmount_verb`]) and are executed by
//! [`crate::cmd::trade::oneshot::execute_write`] — the SAME preview + guardrail + confirm + send
//! engine `crate::cmd::trade::order`'s and `crate::cmd::trade::position`'s write verbs already
//! share. No second send path, no second prompt, no second outcome classifier.
//!
//! # The account: threaded onto the wire, never refused
//!
//! The `<account>` positional (`<venue>`, `<venue>/<LABEL>` or `<venue>/DEFAULT`, read by
//! `crate::cmd::trade::selector::addressed`, the plane's one WHO token; the pre-stage-5
//! `--account <LABEL>` flag is refused naming it) reaches
//! [`vike_client::verbs::Verb::MountStrategy`]'s `account` field and then
//! `CommandWire::MountStrategy`'s UNCHANGED. This module never calls
//! `crate::cmd::trade::selector::refuse_an_unaddressable_book` — that refusal is READ-side only (its
//! own doc says so: it guards a READ, a row the order read cannot attribute to an account, which
//! has nothing to do with mounting). `FEATURE_MOUNT_ACCOUNT` is
//! the capability that gates a mount naming an account, and
//! `vike_tradehub_client::tradehub_control`'s `required_feature` — consulted inside
//! `TradehubControlHandle::try_command_with_reason`, which `execute_write` already calls — is what
//! refuses it against a node too old to route it; this module re-implements none of that check.
//!
//! ⚠ **This module's doc used to say the LIFECYCLE verbs "stay one-shot spellings the group layer
//! refuses BY NAME until their own tasks land" — that task is this one, and it has landed.**
//! `crate::cmd::trade::plane::RETIRED_FLAT_SPELLINGS` still refuses the BARE, group-less spelling
//! (`vike-cli trade mount …`, with no `strategy` word) — correctly, since every verb lives in a
//! group now — but it points HERE for the replacement, and here is where `mount`/`unmount` actually
//! run.

use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

use crate::cmd::args;
use crate::cmd::trade::oneshot::{self, WrapperArgs};
use crate::cmd::trade::render::{mount_rows, mounts_json, mounts_table};
use crate::exit::CliError;
use vike_client::hubkeys::HubKeyring;
use vike_client::refusals::RenamedArgumentRefusal;
use vike_client::verbs::Verb;

const COMMAND: &str = "trade strategy";

/// The program name a refusal of this group's own rules prints under.
const BIN: &str = "vike-cli trade strategy";

// The `strategy` group's verbs. A group needs a verb: a bare `trade strategy` is refused by
// `clap`, and the roster in that refusal and in `--help` is `clap`'s own — derived from this
// enum, so it cannot name a different set than the parser accepts.
#[derive(clap::Subcommand, Debug)]
pub(super) enum StrategyVerb {
    #[command(about = "list the node's mounted-strategy registry")]
    Ls(LsCommand),
    #[command(
        about = "add a strategy to the running node — --strategy is required",
        long_about = MOUNT_PROSE,
        override_usage = "vike-cli trade strategy mount <ACCOUNT> <SYMBOL> <INTERVAL> \
                          --strategy <PATH> [OPTIONS]"
    )]
    Mount(MountCommand),
    #[command(
        about = "remove one mount by id — POSITIONS ARE NOT FLATTENED",
        long_about = UNMOUNT_PROSE,
        override_usage = "vike-cli trade strategy unmount <MOUNT-ID> [OPTIONS]"
    )]
    Unmount(UnmountCommand),
    #[command(
        name = "reset-budget",
        about = "clear ONE mount's budget latch so it trades again (only when it is flat)",
        long_about = RESET_BUDGET_PROSE
    )]
    ResetBudget(ResetBudgetCommand),
}

// `ls`'s command line — no book/symbol positional: a mount is not addressed by an account, and
// the venue/symbol/interval addressing key each [`vike_tradehub_client::wire::MountViewWire`]
// already carries is shown, not filtered on, by this first cut.
#[derive(clap::Args, Debug)]
pub(super) struct LsCommand {
    #[arg(
        long,
        value_name = "HOST:PORT",
        overrides_with = "node",
        help = "the node's observe address [default: config.node_addr]"
    )]
    node: Option<String>,
    #[arg(long, overrides_with = "json", help = "print the rows as one JSON array")]
    json: bool,
}

/// `ls`'s resolved line: [`LsCommand`] after its one rule ran.
#[derive(Debug)]
struct LsArgs {
    node: String,
    json: bool,
}

impl LsCommand {
    /// The node: `--node`, else `configured` (`config.node_addr`). PURE.
    fn into_args(self, configured: Option<&str>) -> Result<LsArgs, String> {
        let node = super::node_or_refusal(self.node, configured)?;
        Ok(LsArgs { node, json: self.json })
    }
}

/// Entry point [`crate::cmd::trade::run`] routes `trade strategy …` to, with the verb `clap` read.
/// `policy_max_notional` is threaded to the LIFECYCLE write verbs' guardrail — see
/// [`oneshot::WriteContext`] — and `ack_wait` (`config.node_ack_wait_ms`) with it, to the wait for the
/// node's verdict on the command a write verb sends.
pub(super) fn run(
    verb: StrategyVerb,
    keys: &HubKeyring,
    policy_max_notional: Option<f64>,
    ack_wait: Duration,
) -> ExitCode {
    match verb {
        StrategyVerb::Ls(cmd) => match cmd.into_args(keys.node_addr()) {
            Ok(a) => run_ls(a, keys),
            Err(msg) => args::refuse(BIN, msg),
        },
        StrategyVerb::Mount(c) => oneshot::run_verb(
            COMMAND,
            "mount",
            mount_verb(&c),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
        StrategyVerb::Unmount(c) => oneshot::run_verb(
            COMMAND,
            "unmount",
            unmount_verb(&c.ids),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
        StrategyVerb::ResetBudget(c) => oneshot::run_verb(
            COMMAND,
            "reset-budget",
            reset_budget_verb(&c.mount),
            &c.wrap,
            keys,
            policy_max_notional,
            ack_wait,
        ),
    }
}

fn run_ls(a: LsArgs, keys: &HubKeyring) -> ExitCode {
    // The OBSERVE key specifically: `Request::StrategyStatus` is read-only and answers under either
    // scope (`crate::cmd::trade::status`'s module doc), but a control key cannot authenticate a read
    // the node's own handshake still verifies against the scope it was presented for.
    let Some((key, _origin)) = keys.observe() else {
        eprintln!("{}", keys.observe_absent_message(COMMAND, &a.node));
        return ExitCode::FAILURE;
    };
    // `_with_features`, never the plain `strategy_status` wrapper — see this module's doc for the
    // Critical fix this round covers: the PRODUCT column can only be rendered honestly by a reader
    // that knows whether the node advertised `FEATURE_MOUNT_CLASS`.
    let (status, features) =
        match vike_tradehub_client::strategy_status_with_features(a.node.as_str(), key) {
            Ok(v) => v,
            Err(e) => {
                for line in crate::cmd::trade::status::failure_lines(&a.node, &e) {
                    eprintln!("{line}");
                }
                return crate::cmd::trade::status::failure_exit(&e).into();
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

// `mount`'s command line. Every flag may appear in any order among the three positionals
// (`<account> <symbol> <interval>`, kept as one list so the verb's own count rule words the
// refusal), and every value is kept RAW: [`mount_verb`] judges them.
#[derive(clap::Args, Debug)]
pub(super) struct MountCommand {
    #[arg(
        value_name = "ACCOUNT|SYMBOL|INTERVAL",
        help = "<account> <symbol> <interval>, the account <venue> | <venue>/<LABEL> | <venue>/DEFAULT"
    )]
    positionals: Vec<String>,
    #[arg(
        long,
        value_name = "PATH",
        help = "the strategy FILE: its path under the NODE's strategies/ folder (a .rs or .rhai file)"
    )]
    strategy: Option<String>,
    // The flag's name before `--strategy`: read only to be refused naming it ([`mount_verb`]).
    #[arg(long = "file", hide = true, value_name = "PATH")]
    retired_file: Option<String>,
    // The pre-stage-5 `--account <LABEL>`: hidden, and refused naming the positional ([`mount_verb`]).
    #[arg(long = "account", hide = true, value_name = "LABEL")]
    retired_account: Option<String>,
    #[arg(
        long,
        value_name = "MOUNT-ID",
        help = "an explicit mount id (else the node derives one)"
    )]
    id: Option<String>,
    #[arg(long, value_name = "JSON", help = "the strategy's params: ONE quoted JSON object")]
    params: Option<String>,
    #[command(flatten)]
    wrap: WrapperArgs,
}

// `unmount`'s one mount id.
#[derive(clap::Args, Debug)]
pub(super) struct UnmountCommand {
    #[arg(value_name = "MOUNT-ID", help = "the mount to remove")]
    ids: Vec<String>,
    #[command(flatten)]
    wrap: WrapperArgs,
}

/// The shape line of `mount`'s arity/exclusivity refusals: they keep the words they always had.
const MOUNT_SHAPE: &str = "usage: vike-cli trade strategy mount <account> <symbol> <interval> \
                           --strategy <path under the node's strategies/ folder> \
                           [--id <mount-id>] \
                           [--params <json-object>] --node <host:port> [--yes] [--json] \
                           [--reason <text to end of line>]";

/// `mount`'s long help, and the second half of its refusals ([`mount_usage`]).
const MOUNT_PROSE: &str = "Adds a strategy to the RUNNING node's core with no restart. Every \
                            flag may appear in ANY ORDER except --reason, which must be LAST (it \
                            consumes everything after it).\n\n\
                            --strategy is REQUIRED: the strategy file's path under the NODE's \
                            strategies/ folder (a .rs or .rhai file).\n\
                            ⚠ --strategy is a path on the NODE, not this machine — the node opens \
                            it, and a file that exists here proves nothing about there.\n\n\
                            <account> names WHICH ACCOUNT the mount trades and reads: a bare \
                            <venue> names none, <venue>/DEFAULT the venue's unlabelled account \
                            deliberately, <venue>/<LABEL> that account. On a venue this node runs \
                            TWO engines of, naming none is REFUSED by the node rather than \
                            resolved to the default.\n\n\
                            --params must be a JSON OBJECT (the `[strategy.params]` table) and \
                            takes EXACTLY ONE shell argument — QUOTE it, e.g. --params \
                            '{\"gamma\": 0.0008}'. An unquoted value the shell has already split \
                            on its own whitespace is refused with the pieces it saw and the \
                            corrected command line; it is never silently reassembled.\n\n\
                            ⚠ --reason must be LAST — everything after it, to end of line, is \
                            taken verbatim as the recorded rationale.";

fn mount_usage() -> String {
    format!("{MOUNT_SHAPE}\n\n{MOUNT_PROSE}")
}

/// What `mount --file` answers, at the prompt and one-shot alike: the flag is `--strategy` now.
pub(super) fn renamed_file_refusal() -> String {
    let refusal = RenamedArgumentRefusal::Renamed { old: "--file", new: "--strategy" };
    vike_utility::errors::render_refusal(refusal.id().word(), &refusal)
}

/// The shape line of `unmount`'s arity refusal.
const UNMOUNT_SHAPE: &str = "usage: vike-cli trade strategy unmount <mount-id> --node \
                             <host:port> [--yes] [--json] [--reason <text to end of line>]";

/// `unmount`'s long help, and the second half of its arity refusal ([`unmount_usage`]).
const UNMOUNT_PROSE: &str = "Removes one mount from the running core, BY ID. The node cancels \
                              that mount's attributed live orders and saves its durable state.\n\n\
                              POSITIONS ARE NOT FLATTENED — `trade position flatten`/`close-all` \
                              are the verbs that close a position. An operator who reads unmount \
                              as \"get me out\" and is wrong about it is left holding an \
                              unattended position.\n\n\
                              A mount id is ONE token: the explicit --id given at mount time, or \
                              the node-derived {venue}__{symbol}__{interval}.";

fn unmount_usage() -> String {
    format!("{UNMOUNT_SHAPE}\n\n{UNMOUNT_PROSE}")
}

/// The `--params` value as a JSON OBJECT, or the refusal that teaches the fix. `value` is the RAW
/// flag value; `positionals` are the tokens `clap` read as positionals, in the order written — an
/// UNQUOTED object the shell already split on whitespace leaves its tail pieces among them, past
/// the three real ones.
///
/// **`--params` takes EXACTLY ONE `argv` element, verbatim** — an ORDINARY value-taking flag,
/// same as `--strategy`/`--id`, and NEVER reassembled from pieces (see this
/// module's doc for why that reassembly was the defect a previous round of this reader had to
/// remove). A value that fails to parse as JSON, with more positionals than the three a mount
/// takes, is almost certainly an UNQUOTED JSON object the shell already split on whitespace before
/// this program ran — that shape is refused with a message naming every piece and the corrected,
/// quoted command line, never silently rejoined.
fn params_object(value: &str, positionals: &[String]) -> Result<Value, CliError> {
    match serde_json::from_str::<Value>(value) {
        Ok(v) if v.is_object() => Ok(v),
        Ok(v) => Err(CliError::usage(format!(
            "mount: --params must be a JSON OBJECT (the `[strategy.params]` table), got {v}\n{}",
            mount_usage()
        ))),
        Err(e) => {
            // A run of extra tokens right after the value is the signature an UNQUOTED JSON
            // object leaves once the shell has already split it on whitespace — `--params` takes
            // exactly ONE shell argument and this reader never reassembles one (reassembly is
            // what silently corrupted a quoted value's own internal whitespace, see this module's
            // doc). Detect the shape and teach the fix rather than reporting a bare parse error
            // over a fragment the operator never meant to stand alone.
            let mut pieces = vec![value];
            pieces.extend(positionals.iter().skip(3).map(String::as_str));
            if pieces.len() > 1 {
                let corrected = pieces.join(" ");
                return Err(CliError::usage(format!(
                    "mount: --params takes exactly ONE shell argument, and this value arrived \
                     split into {} pieces {pieces:?} — the shell already broke it on whitespace \
                     before this program ran, and it is never reassembled. Quote the whole object \
                     as one argument:\n  --params '{corrected}'\n{}",
                    pieces.len(),
                    mount_usage()
                )));
            }
            Err(CliError::usage(format!(
                "mount: --params is not JSON ({e}): {value}\n{}",
                mount_usage()
            )))
        }
    }
}

/// `mount <account> <symbol> <interval> --strategy <path>
/// [--id <mount-id>] [--params <json>]` — the mount-shape rules ONLY
/// (`--node`/`--yes`/`--json`/`--reason` are [`WrapperArgs`]). Mirrors `crate::cmd::trade`'s own
/// REPL `parse_mount` — the same flags, the same meanings, the venue leading exactly as it already
/// does there. PURE: no network.
///
/// **ONE strategy source, the `--strategy`.** Its absence is refused HERE rather than spending a
/// round trip on a refusal the grammar already knew. The retired `--name` and `--rhai` are no flags
/// any more: `clap` refuses either as any unknown argument. The renamed `--file` is refused FIRST,
/// naming `--strategy` (`vike_client::refusals::RenamedArgumentRefusal`). ⚠ `--strategy` is a path
/// under the **NODE's** `strategies/` folder, not this machine's.
///
/// **`<account>`** is read by `crate::cmd::trade::selector::addressed`, the plane's one WHO token
/// (the REPL's `parse_mount` reads it the same way), and its account threads straight onto
/// [`Verb::MountStrategy`]'s `account` field and then the wire — see this module's doc for why no
/// read-side refusal runs over it. The pre-stage-5 `--account <LABEL>` is refused naming it.
///
/// **`--params`** is judged by [`params_object`]. Absent ⇒ `{}`, an empty table; present-and-empty
/// is a typo, and `clap`'s blank-value rule refuses it before this runs.
fn mount_verb(cmd: &MountCommand) -> Result<Verb, CliError> {
    if cmd.retired_file.is_some() {
        return Err(CliError::usage(renamed_file_refusal()));
    }
    if cmd.retired_account.is_some() {
        return Err(CliError::usage(super::plane::renamed(
            "trade strategy mount <venue> … --account <LABEL>",
            "trade strategy mount <venue>/<LABEL> …",
        )));
    }
    oneshot::refuse_swallowed_flag::<MountCommand>(
        "mount",
        "the mount",
        cmd.wrap.reason().as_deref(),
    )?;
    let params = match cmd.params.as_deref() {
        Some(value) => Some(params_object(value, &cmd.positionals)?),
        None => None,
    };
    let positional = &cmd.positionals;
    if positional.len() != 3 {
        return Err(CliError::usage(mount_usage()));
    }
    let Some(file) = cmd.strategy.clone() else {
        return Err(CliError::usage(format!(
            "mount: a strategy source is required — pass --strategy <path> (a strategy file under \
             the NODE's strategies/ folder)\n{}",
            mount_usage()
        )));
    };

    // Read as every other verb's account token (decision 0254), refused HERE in that grammar's own
    // words rather than a round trip away. ⚠ `<venue>/DEFAULT` is not the bare venue: it says "the
    // unlabelled account, deliberately", and at two engines of one venue those are different answers.
    let (venue, account) = crate::cmd::trade::selector::addressed(&positional[0])
        .map_err(|e| CliError::usage(format!("mount: {}\n{}", e.message, mount_usage())))?;

    Ok(Verb::MountStrategy {
        venue,
        account,
        symbol: positional[1].clone(),
        interval: positional[2].clone(),
        // `None` lets the node derive `{venue}__{symbol}__{interval}`. Deliberately NOT derived
        // here: the derivation is the node's rule, and a client copy of it would be a second one to
        // keep in step for no gain — the node accepts the absence and answers with its own answer.
        controller_id: cmd.id.clone(),
        file: Some(file),
        params: params.unwrap_or_else(|| json!({})),
    })
}

/// `unmount <mount-id>` — no book: the order-client-id-shaped mount id already names the engine it
/// lives on, same reasoning `crate::cmd::trade::order`'s `cancel` uses. Mirrors
/// `crate::cmd::trade`'s own REPL `parse_unmount`. PURE.
fn unmount_verb(ids: &[String]) -> Result<Verb, CliError> {
    match ids {
        [id] => Ok(Verb::UnmountStrategy { controller_id: id.clone() }),
        _ => Err(CliError::usage(unmount_usage())),
    }
}

// ---- test seams: the same argv tails the tests always wrote, read through the plane's parser ----

/// `word` and `tokens` read as `trade strategy <word> <tokens…>` by the plane's one `clap` parser.
#[cfg(test)]
fn read(word: &str, tokens: &[&str]) -> Result<StrategyVerb, CliError> {
    let argv = ["strategy", word].into_iter().chain(tokens.iter().copied()).map(String::from);
    match super::parse(argv).map_err(|stop| CliError::usage(stop.to_string()))?.sub {
        Some(super::Sub::Strategy { verb }) => Ok(verb),
        other => panic!("`trade strategy {word}` did not read as a strategy verb: {other:?}"),
    }
}

/// [`mount_verb`] over an argv tail (no `trade strategy mount` words), through the plane's parser.
#[cfg(test)]
fn parse_mount_args(tokens: &[&str]) -> Result<Verb, CliError> {
    match read("mount", tokens)? {
        StrategyVerb::Mount(c) => mount_verb(&c),
        other => unreachable!("`mount` read as {other:?}"),
    }
}

/// [`unmount_verb`] over an argv tail, through the plane's parser.
#[cfg(test)]
fn parse_unmount_args(tokens: &[&str]) -> Result<Verb, CliError> {
    match read("unmount", tokens)? {
        StrategyVerb::Unmount(c) => unmount_verb(&c.ids),
        other => unreachable!("`unmount` read as {other:?}"),
    }
}

/// `--help`'s prose for `reset-budget`.
const RESET_BUDGET_PROSE: &str = "\
Clear ONE mount's budget LATCH so it trades again. A mount whose loss or notional budget is breached is latched \
liquidate-only for good: its own resting orders are cancelled once, its new orders are discarded, and (when its \
budget says so) its flatten is offered again at every sweep while it holds a position. `trade strategy \
reset-budget binance__BTCUSDT__1m` is the operator's way out; the account's trading state and every other mount \
are untouched, and nothing is written at any venue.

MOUNT is the id `trade strategy ls` prints (the `controller_id` works too: `maker-a` finds `maker_a`).

The node refuses, in words and before anything is sent: a mount it does not run (the refusal lists the ones it \
does), a mount that is NOT latched, and a mount that still holds a position. A mount that still holds a position \
is the one to look at first: when its budget flattens, the latch offers the close again at every sweep and the \
core says so when it is asked (the node's recent events, `MOUNT UNLATCH REFUSED`); a cancel-only budget is closed \
by hand (`trade position flatten`). The core also refuses a mount with a live order or an armed conditional (the \
published account does not attribute orders to a mount, so only the core sees them): that verdict is a line in the \
node's recent events and log, and the command reports `sent`, not `accepted`. Reset a mount you have SEEN flat.

When it is accepted the loss budget is RE-ARMED from the mount's CURRENT net realized PnL, and the node says the new \
base in its recent events (`MOUNT BUDGET UNLATCHED`): without a new base the mount would be judged on the whole of \
the loss that latched it and latch again at the next sweep. A further loss of the budget's `max_loss` latches it \
again; the notional cap is unchanged (it judges the open exposure, not history). The result survives a restart: \
the node journals a checkpoint at once.

It writes NOTHING at the venue, so it is taken on every tier and in every trading state. It needs a CONTROL-scope \
key and a node that advertises `mount-unlatch`.

--reason must be LAST - everything after it, to end of line, is taken verbatim as the recorded \
rationale. ALWAYS pass --yes when nothing is at the keyboard (see `trade halt --help`).";

/// `trade strategy reset-budget`'s command line: the mount, then the shared one-shot write flags (`--node` is the
/// CONTROL address).
#[derive(clap::Args, Debug)]
pub(super) struct ResetBudgetCommand {
    #[arg(value_name = "MOUNT", help = "the mount id `trade strategy ls` prints")]
    mount: String,
    #[command(flatten)]
    wrap: WrapperArgs,
}

/// The verb `trade strategy reset-budget MOUNT` resolves, or the usage refusal. PURE. An empty id names no mount.
fn reset_budget_verb(mount: &str) -> Result<Verb, CliError> {
    let mount = mount.trim();
    if mount.is_empty() {
        return Err(CliError::usage(
            "an empty mount id names no mount to reset (`trade strategy ls` prints the ids)"
                .to_string(),
        ));
    }
    Ok(Verb::UnlatchMount { mount_id: mount.to_string() })
}

#[path = "strategy_tests.rs"]
#[cfg(test)]
mod strategy_tests;
