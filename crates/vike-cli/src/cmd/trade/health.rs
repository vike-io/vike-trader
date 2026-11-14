//! `vike-cli trade health` — the checks an operator ran by hand after every deploy, in ONE screen,
//! from data the node already publishes.
//!
//! The reads, the rows and the `--json` document are `crates/vike-client/src/health.rs`'s, shared with
//! the MCP tool `health_node`, and that module's doc says what each word means. This file is the verb:
//! its command line, its table and its exit code.
//!
//! # The exit code is the verdict
//!
//! OK, WARN and FAIL exit on [`EXIT_OK`], [`EXIT_WARN`] and [`EXIT_FAIL`], one number each, pinned
//! by a test. ⚠ Two of them are the ladder's `Failed` and `Usage` numbers
//! (`crates/vike-cli/src/exit.rs`'s `Exit`): a FAIL verdict and a mistyped flag share a number, and
//! only the output tells them apart (a verdict shows the table and the `HEALTH:` line; a usage error
//! shows neither). Two refusals never reach a verdict and keep their rungs: no observe key anywhere
//! is `Exit::Refused`, and nothing is dialled.
//!
//! # What is tested
//!
//! `judge` and both renderers are pure and unit-tested over synthetic snapshots (a healthy node, a
//! halted venue, a held row, a NOT RUNNING account, a stale duplicate block, the starting door, an
//! unreachable node). They stay in this crate's `health_tests.rs`, because this verb's output is what
//! they pin. `crates/vike-cli/tests/shipped_cli/trade_health_cli.rs` drives the shipped binary against
//! a scripted node.

use std::process::ExitCode;

use vike_client::health::{
    COMMAND, Check, HubLeg, Status, Verdict, ask_datahub, count, document, judge, read_node,
    verdict,
};
use vike_client::hubkeys::HubKeyring;
use vike_hub_protocol::auth::HubKeys;

use crate::cmd::args;
use crate::exit::Exit;

/// The program name a refusal of this verb's own rule prints under.
const BIN: &str = "vike-cli trade health";

/// Exit number of an OK verdict.
const EXIT_OK: u8 = 0;
/// Exit number of a WARN verdict.
const EXIT_WARN: u8 = 1;
/// Exit number of a FAIL verdict.
const EXIT_FAIL: u8 = 2;

/// `--help`'s prose (the `usage:` line and the options table are `clap`'s).
pub(super) const LONG_ABOUT: &str = "\
Judge a running vike-tradehub in one screen, from what it already publishes: is its door ready or
still starting, which accounts run (and at which tier) or are NOT RUNNING and why, which venues are
halted or reducing, which reconcile rows are held for an operator, a stale duplicate engine block,
how long the node has been up, resting orders and the age of the oldest, non-flat positions,
positions with no price source, whether the HALT sentinel file exists, the node's build.

Read-only: it authenticates under the OBSERVE scope and can neither place nor change anything.
It also asks the datahub at the config.datahub_addr row's address, under the datahub's observe key,
what it sees of the gatewayhub, so a dead gatewayhub, a dead tradehub and a degraded datahub read
differently, with the tradehub down too. It never dials the gatewayhub itself.
Each check is one row with one word: PASS, WARN or FAIL. A fact the node does not publish (whether
a price feed is live, leverage, ...) is a row saying `unknown (not on the wire)` and the command
that answers it; a fact only a newer node publishes (uptime, the age of an order, the HALT
sentinel) reads `unknown (node too old)` against an older one. Neither is guessed and neither
moves the verdict.

The last line is `HEALTH: OK`, `HEALTH: WARN <n>` or `HEALTH: FAIL <n>` (the number of rows with
that word). The exit code is the verdict: 0 for OK, 1 for WARN, 2 for FAIL. A missing observe key
is refused before anything is dialled and is not a verdict.

The observe key is resolved like every hub key: VIKE_TRADEHUB_OBSERVE_KEY from the settings
database (its hub_key table) — the same store the daemon reads, and the only place a hub key
lives (no environment variable carries it).";

// The `trade health` command line, as `clap` reads it. `--node` is required, but that is judged
// after the parse ([`HealthCommand::into_args`]) so `--help` never has to supply one.
#[derive(clap::Args, Debug)]
pub(super) struct HealthCommand {
    #[arg(
        long,
        value_name = "HOST:PORT",
        overrides_with = "node",
        help = "the node's observe address (required)"
    )]
    node: Option<String>,
    #[arg(long, overrides_with = "json", help = "the report as one JSON document, for scripts")]
    json: bool,
}

/// The resolved `trade health` command line.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    node: String,
    json: bool,
}

impl HealthCommand {
    /// The one rule after the parse: `--node` is required.
    fn into_args(self) -> Result<Args, String> {
        let node = self.node.ok_or("--node <host:port> is required")?;
        Ok(Args { node, json: self.json })
    }
}

/// `trade health`'s argv tail through the plane's one parser, then its rule.
#[cfg(test)]
fn parse(argv: impl IntoIterator<Item = String>) -> Result<Args, args::Stop> {
    parse_trade_verb!(argv, "health", Health, BIN)
}

/// The exit number of a verdict — the only mapping, so the table and the number cannot disagree.
fn exit_number(verdict: Verdict) -> u8 {
    match verdict {
        Verdict::Ok => EXIT_OK,
        Verdict::Warn => EXIT_WARN,
        Verdict::Fail => EXIT_FAIL,
    }
}

/// The last line: `HEALTH: OK`, `HEALTH: WARN <n>` or `HEALTH: FAIL <n>`.
fn summary_line(checks: &[Check]) -> String {
    match verdict(checks) {
        Verdict::Ok => "HEALTH: OK".to_string(),
        Verdict::Warn => format!("HEALTH: WARN {}", count(checks, Status::Warn)),
        Verdict::Fail => format!("HEALTH: FAIL {}", count(checks, Status::Fail)),
    }
}

// ---------------------------------------------------------------------------------------------
// the renderers (pure)
// ---------------------------------------------------------------------------------------------

/// The human table: one header, one row per check with its sub-lines, the count of rows the wire
/// could not answer, and the `HEALTH:` line.
fn human(node: &str, checks: &[Check]) -> String {
    let name_w = checks.iter().map(|c| c.name.len()).chain(["CHECK".len()]).max().unwrap_or(0);
    let scope_w = checks.iter().map(|c| c.scope.len()).chain(["ACCOUNT".len()]).max().unwrap_or(0);
    let mut out = format!(
        "vike-cli {COMMAND}: {node}\n{:<7}  {:<name_w$}  {:<scope_w$}  DETAIL",
        "STATUS", "CHECK", "ACCOUNT"
    );
    for c in checks {
        out.push_str(&format!(
            "\n{:<7}  {:<name_w$}  {:<scope_w$}  {}",
            c.status.word(),
            c.name,
            c.scope,
            c.detail
        ));
        for item in &c.items {
            out.push_str(&format!("\n         - {item}"));
        }
    }
    let not_on_the_wire = count(checks, Status::Unknown);
    if not_on_the_wire > 0 {
        out.push_str(&format!(
            "\n{not_on_the_wire} fact(s) are not on the wire (or the node is too old to publish \
             them): each UNKNOWN row above names the command that answers it"
        ));
    }
    out.push('\n');
    out.push_str(&summary_line(checks));
    out
}

/// The `--json` document, as text.
fn json_body(node: &str, checks: &[Check]) -> String {
    serde_json::to_string_pretty(&document(node, checks))
        .expect("a tree of strings, numbers, arrays and nulls; serialization is total")
}

/// Entry point [`crate::cmd::trade::run`] routes `trade health` to.
///
/// `datahub_addr` is the `config.datahub_addr` row, and `datahub_keys` reads the store's datahub pair, called only here.
/// `settings_dir` is this box's store: the venue addresses the gatewayhub probed are read from it
/// (`crate::cmd::config::url::health_check`), a row the wire does not carry.
pub(super) fn run(
    cmd: HealthCommand,
    keys: &HubKeyring,
    datahub_addr: Option<&str>,
    datahub_keys: &dyn Fn() -> Option<HubKeys>,
    settings_dir: Option<&std::path::Path>,
) -> ExitCode {
    let parsed = match cmd.into_args() {
        Ok(a) => a,
        Err(msg) => return args::refuse(BIN, msg),
    };
    // The OBSERVE key specifically: a control key cannot authenticate a read. A missing key is not
    // a verdict, so it keeps the refused rung and nothing is dialled.
    let Some((key, _origin)) = keys.observe() else {
        eprintln!("{}", keys.observe_absent_message(COMMAND));
        return Exit::Refused.into();
    };
    let mut reads = read_node(&parsed.node, key.as_bytes());
    let addr = datahub_addr
        .filter(|a| !a.trim().is_empty())
        .unwrap_or(vike_config::DEFAULT_DATAHUB_ADDRESS);
    let answer = ask_datahub(addr, datahub_keys().as_ref());
    reads.hubs = Some(HubLeg { addr: addr.to_string(), answer });
    let mut checks = judge(&parsed.node, &reads);
    // A probe failure is a WARN and never more: it halts nothing.
    checks.extend(crate::cmd::config::url::health_check(settings_dir));
    if parsed.json {
        println!("{}", json_body(&parsed.node, &checks));
    } else {
        println!("{}", human(&parsed.node, &checks));
    }
    ExitCode::from(exit_number(verdict(&checks)))
}

#[path = "health_tests.rs"]
#[cfg(test)]
mod health_tests;
