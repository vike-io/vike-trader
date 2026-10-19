//! `vike-cli trade doctor` — the checks an operator ran by hand after every deploy, in ONE screen,
//! from data the node already publishes.
//!
//! A deploy is judged today by piping probe scripts over ssh: the engine table, the journal, the
//! unit start times. Most of what they look at is on the OBSERVE wire already, so this verb reads it
//! there and judges it, with the observe key and nothing else (it cannot place, cancel or change
//! anything). It sends no event and no command: everything below is a [`WireSnapshot`], a
//! [`WireDirectory`] or a `Welcome.features` list as the node publishes them.
//!
//! # The reads (all `Scope::Read`, one short-lived connection each)
//!
//! 1. `snapshot_once` — the frame. If it fails, the failure IS the first finding and nothing else is
//!    asked: a node that is still STARTING answers every verb with `starting: <progress>`
//!    (`crates/vike-tradehub/src/server/starting.rs`'s `StartProgress`), a refused key is a handshake
//!    denial, anything else is "cannot reach". A frame with `seq` 0 is the pre-fold placeholder, not a
//!    reading, and is reported as that.
//! 2. `strategy_status_with_features` — only for the node's advertised capabilities, so an empty
//!    `recon` list can be told from a node that predates the field.
//! 3. `directory` — the ACTIVE account rows, so the tier a row states can be set beside the tier an
//!    engine runs at, and an active row with no engine and no reason is found.
//!
//! # What it checks, and how each word is decided
//!
//! * `door` — ready, STARTING (WARN: retry shortly), pre-fold (WARN: only the build, `uptime` and
//!   `halt-sentinel` rows follow it, the two the publisher reads itself), refused key or unreachable
//!   (FAIL); `build` — the identity block's build string and live/paper; `core` — a handler fault
//!   (FAIL, the core is halted in safe state).
//! * `engine`, one row per account: RUNNING at a tier (WARN when the engine's tier differs from the
//!   active row's), NOT RUNNING with the node's reason (WARN), or an active row with no engine and no
//!   reason (FAIL). The node's NOT RUNNING list wins over a block of the same route key: a stopped
//!   account keeps its last block (`crates/vike-tradehub/src/publish.rs`'s `current_venue_blocks`).
//!   A binance LANE TWIN (`binance#PERP` on `BTCUSDT.P`, derived from the default account's row) has
//!   no row of its own and is judged against the default account's row.
//! * `trading`, per running engine: Reducing is WARN, Halted is FAIL (no order that opens risk is
//!   admitted; covered reduces still pass).
//! * `duplicates` — one route key published twice is FAIL (a consumer that takes the first block reads
//!   the stopped engine; a current node publishes only the newest, so this fires against an older
//!   one); a stopped account that still publishes its block is WARN (its position stays valued and
//!   nothing manages it).
//! * `keys` — a running account whose database key changed after its session started is WARN.
//! * `recon` — held reconcile rows: count, kinds, the effect of a confirm (`trade recon`'s own words)
//!   and the commands that list and resolve them. WARN while any is held.
//! * `uptime` — [`WireSnapshot::uptime_ms`]: under two minutes is WARN ("restarted recently": the
//!   rest of the table describes a node still settling), otherwise PASS with the span.
//! * `orders` / `order-age` / `positions` / `prices` — resting orders (count, and the first in
//!   registry order); the oldest resting order's age from [`WireOrderView::age_ms`] (WARN when it is
//!   older than 24 hours; the node counts from the moment it FIRST SAW the order, so a restart resets
//!   the clock and the figure is never larger than the uptime); non-flat positions (WARN when one
//!   belongs to a NOT RUNNING account); and a position with no priceable source (FAIL).
//! * `halt-sentinel` — [`WireSnapshot::halt_sentinel`]: the file exists is FAIL (every adapter
//!   refuses an order that opens or adds risk, whatever the engines say), absent is PASS.
//!
//! # Not on the wire: said, never guessed
//!
//! A check whose data the frame does not carry is an UNKNOWN row that names the command that answers
//! it. Two words tell the two causes apart: `unknown (not on the wire)` is a fact no node publishes
//! (whether a price feed is live, an account's leverage, a Hyperliquid approval, the journal's ERROR
//! lines), and `unknown (node too old)` is a fact a CURRENT node publishes and this one did not
//! (`uptime`, `order-age`, `halt-sentinel`: three additive frame fields, absent from an older node's
//! frame, which reads as "not said" and never as zero, as a young order or as "no sentinel").
//! UNKNOWN rows do not move the verdict; the line before the verdict tallies them so an OK is never
//! read as "everything was checked".
//!
//! `price-feeds` stays UNKNOWN on every node: how old a venue's last price is lives in the core's
//! price board (`vike-exec`, a measured crate), and the frame has no field for it.
//!
//! # The exit code is the verdict
//!
//! OK, WARN and FAIL exit on [`EXIT_OK`], [`EXIT_WARN`] and [`EXIT_FAIL`], one number each, pinned
//! by a test. ⚠ Two of them are the ladder's `Failed` and `Usage` numbers
//! (`crates/vike-cli/src/exit.rs`'s `Exit`): a FAIL verdict and a mistyped flag share a number, and
//! only the output tells them apart (a verdict shows the table and the `DOCTOR:` line; a usage error
//! shows neither). Two refusals never reach a verdict and keep their rungs: no observe key anywhere
//! is `Exit::Refused`, and nothing is dialled.
//!
//! # What is tested
//!
//! [`judge`] and both renderers are pure and unit-tested over synthetic snapshots (a healthy node, a
//! halted venue, a held row, a NOT RUNNING account, a stale duplicate block, the starting door, an
//! unreachable node). `crates/vike-cli/tests/trade_doctor_cli.rs` drives the shipped binary against a
//! scripted node.

use std::collections::BTreeMap;
use std::io;
use std::process::ExitCode;

use vike_model::accounts::account_keys::{AccountLabel, route_key_of};
use vike_tradehub_client::proto::{FEATURE_RECON_VERBS, STARTING_ERROR_PREFIX};
use vike_tradehub_client::wire::{
    KEY_CHANGED_WARNING, WireDirectory, WireDirectoryAccount, WireNotRunning, WireOrderView,
    WireSnapshot, WireTradingState, WireVenueBlock,
};

use super::recon::effect_of;
use crate::cmd::args;
use crate::cmd::hubkeys::{self, HubKeyring};
use crate::exit::Exit;

/// The command name every message prints.
const COMMAND: &str = "trade doctor";

/// The program name a refusal of this verb's own rule prints under.
const BIN: &str = "vike-cli trade doctor";

/// Exit number of an OK verdict.
const EXIT_OK: u8 = 0;
/// Exit number of a WARN verdict.
const EXIT_WARN: u8 = 1;
/// Exit number of a FAIL verdict.
const EXIT_FAIL: u8 = 2;

/// The most items a row lists before it says how many more there are.
const MAX_ITEMS: usize = 10;

/// A node up for less than this has just (re)started: `uptime` is a WARN.
const RESTARTED_RECENTLY_MS: u64 = 120_000;

/// A resting order older than this is a WARN: a limit order nobody has looked at for a day.
const STALE_ORDER_MS: u64 = 24 * 60 * 60 * 1000;

/// `--help`'s prose (the `usage:` line and the options table are `clap`'s).
pub(super) const LONG_ABOUT: &str = "\
Judge a running vike-tradehub in one screen, from what it already publishes: is its door ready or
still starting, which accounts run (and at which tier) or are NOT RUNNING and why, which venues are
halted or reducing, which reconcile rows are held for an operator, a stale duplicate engine block,
how long the node has been up, resting orders and the age of the oldest, non-flat positions,
positions with no price source, whether the HALT sentinel file exists, the node's build.

Read-only: it authenticates under the OBSERVE scope and can neither place nor change anything.
Each check is one row with one word: PASS, WARN or FAIL. A fact the node does not publish (whether
a price feed is live, leverage, ...) is a row saying `unknown (not on the wire)` and the command
that answers it; a fact only a newer node publishes (uptime, the age of an order, the HALT
sentinel) reads `unknown (node too old)` against an older one. Neither is guessed and neither
moves the verdict.

The last line is `DOCTOR: OK`, `DOCTOR: WARN <n>` or `DOCTOR: FAIL <n>` (the number of rows with
that word). The exit code is the verdict: 0 for OK, 1 for WARN, 2 for FAIL. A missing observe key
is refused before anything is dialled and is not a verdict.

The observe key is resolved like every hub key: VIKE_TRADEHUB_OBSERVE_KEY in the process
environment first, then the settings database (its hub_key table) — the same store the daemon
reads.";

// The `trade doctor` command line, as `clap` reads it. `--node` is required, but that is judged
// after the parse ([`DoctorCmd::into_args`]) so `--help` never has to supply one.
#[derive(clap::Args, Debug)]
pub(super) struct DoctorCmd {
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

/// The resolved `trade doctor` command line.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    node: String,
    json: bool,
}

impl DoctorCmd {
    /// The one rule after the parse: `--node` is required.
    fn into_args(self) -> Result<Args, String> {
        let node = self.node.ok_or("--node <host:port> is required")?;
        Ok(Args { node, json: self.json })
    }
}

/// `trade doctor`'s argv tail through the plane's one parser, then its rule.
#[cfg(test)]
fn parse(argv: impl IntoIterator<Item = String>) -> Result<Args, args::Stop> {
    let argv = std::iter::once("doctor".to_string()).chain(argv);
    match super::parse(argv)?.sub {
        Some(super::Sub::Doctor(cmd)) => {
            cmd.into_args().map_err(|msg| args::Stop::message(BIN, msg))
        }
        other => panic!("`trade doctor` did not read as the doctor verb: {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// the findings
// ---------------------------------------------------------------------------------------------

/// One row's word. `Unknown` is not a verdict word: it marks a fact the wire does not carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Warn,
    Fail,
    Unknown,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
            Status::Unknown => "UNKNOWN",
        }
    }
}

/// One finding: a word, what was checked, for which account (route key; empty for the node), the
/// sentence, and the sub-lines that belong under it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Check {
    status: Status,
    name: &'static str,
    scope: String,
    detail: String,
    items: Vec<String>,
}

impl Check {
    fn new(status: Status, name: &'static str, detail: impl Into<String>) -> Self {
        Check { status, name, scope: String::new(), detail: detail.into(), items: Vec::new() }
    }

    fn scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = scope.into();
        self
    }

    fn items(mut self, items: Vec<String>) -> Self {
        self.items = items;
        self
    }
}

/// A fact the wire does not carry, with the command that answers it.
fn unknown(name: &'static str, what: &str, ask: &str) -> Check {
    Check::new(Status::Unknown, name, format!("unknown (not on the wire): {what}. Ask: {ask}"))
}

/// A fact a CURRENT node puts on its frame and this node's frame lacks, because the node predates
/// the field: the same row as [`unknown`] with the other cause named, so an operator reads "upgrade
/// the node" instead of "the wire cannot say".
fn unknown_old(name: &'static str, what: &str, ask: &str) -> Check {
    Check::new(Status::Unknown, name, format!("unknown (node too old): {what}. Ask: {ask}"))
}

/// The summary of all rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Ok,
    Warn,
    Fail,
}

fn count(checks: &[Check], status: Status) -> usize {
    checks.iter().filter(|c| c.status == status).count()
}

fn verdict(checks: &[Check]) -> Verdict {
    if count(checks, Status::Fail) > 0 {
        Verdict::Fail
    } else if count(checks, Status::Warn) > 0 {
        Verdict::Warn
    } else {
        Verdict::Ok
    }
}

/// The exit number of a verdict — the only mapping, so the table and the number cannot disagree.
fn exit_number(verdict: Verdict) -> u8 {
    match verdict {
        Verdict::Ok => EXIT_OK,
        Verdict::Warn => EXIT_WARN,
        Verdict::Fail => EXIT_FAIL,
    }
}

/// The last line: `DOCTOR: OK`, `DOCTOR: WARN <n>` or `DOCTOR: FAIL <n>`.
fn summary_line(checks: &[Check]) -> String {
    match verdict(checks) {
        Verdict::Ok => "DOCTOR: OK".to_string(),
        Verdict::Warn => format!("DOCTOR: WARN {}", count(checks, Status::Warn)),
        Verdict::Fail => format!("DOCTOR: FAIL {}", count(checks, Status::Fail)),
    }
}

// ---------------------------------------------------------------------------------------------
// the reads
// ---------------------------------------------------------------------------------------------

/// What the node answered. `features` and `directory` are `None` when they were not asked (the
/// door was not ready), and an `Err` when they were asked and failed.
struct Reads {
    snap: io::Result<WireSnapshot>,
    features: Option<io::Result<Vec<String>>>,
    directory: Option<io::Result<WireDirectory>>,
}

/// The three reads, snapshot first: a door that is not ready is judged alone.
fn read_node(node: &str, key: &[u8]) -> Reads {
    let snap = vike_tradehub_client::snapshot_once(node, key);
    if !snap.as_ref().is_ok_and(|s| !super::is_pre_fold(s)) {
        return Reads { snap, features: None, directory: None };
    }
    let features = vike_tradehub_client::strategy_status_with_features(node, key).map(|(_, f)| f);
    let directory = vike_tradehub_client::directory(node, key);
    Reads { snap, features: Some(features), directory: Some(directory) }
}

// ---------------------------------------------------------------------------------------------
// the judgement (pure)
// ---------------------------------------------------------------------------------------------

/// Judge what the node answered into rows. PURE.
fn judge(node: &str, reads: &Reads) -> Vec<Check> {
    let snap = match &reads.snap {
        Ok(snap) => snap,
        Err(e) => return vec![door_failure(node, e)],
    };
    if super::is_pre_fold(snap) {
        return vec![
            Check::new(
                Status::Warn,
                "door",
                "up, but it has published only its pre-fold placeholder (seq 0): no engine, \
                 order or position row would be a reading. Ask again once it has folded",
            ),
            build_check(snap),
            // The two readings the publisher takes itself, whether or not the core has folded.
            uptime_check(snap),
            halt_check(snap),
        ];
    }
    let features: Option<&[String]> =
        reads.features.as_ref().and_then(|r| r.as_ref().ok()).map(Vec::as_slice);
    let directory: Option<&WireDirectory> = reads.directory.as_ref().and_then(|r| r.as_ref().ok());
    let accounts = collect_accounts(snap, directory);
    let rows_known = directory.is_some_and(|d| !d.accounts.is_empty());

    let mut out = vec![
        Check::new(Status::Pass, "door", format!("ready: serving snapshots (seq {})", snap.seq)),
        build_check(snap),
        uptime_check(snap),
        core_check(snap),
    ];
    if let Some(Err(e)) = &reads.directory {
        out.push(Check::new(
            Status::Unknown,
            "rows",
            format!(
                "unknown: the node's directory read failed ({e}), so the tier each active row \
                 states is not compared with the engines. Ask: vike-cli secrets accounts"
            ),
        ));
    }
    out.extend(engine_checks(&accounts, rows_known));
    out.extend(trading_checks(&accounts));
    out.extend(duplicate_checks(snap, &accounts));
    out.extend(key_checks(snap));
    out.push(recon_check(snap, node, features));
    out.extend(order_checks(snap));
    out.extend(position_checks(&accounts));
    out.extend(price_checks(&accounts));
    out.push(halt_check(snap));
    out.extend(box_only_checks(&accounts));
    out
}

/// The first finding of a node that did not give a frame.
fn door_failure(node: &str, e: &io::Error) -> Check {
    let text = e.to_string();
    if e.kind() == io::ErrorKind::InvalidData
        && let Some(at) = text.find(STARTING_ERROR_PREFIX)
    {
        let progress = text[at + STARTING_ERROR_PREFIX.len()..].to_string();
        return Check::new(
            Status::Warn,
            "door",
            "STARTING: the door listens but the accounts are not all up yet. Ask again shortly",
        )
        .items(vec![progress]);
    }
    if e.kind() == io::ErrorKind::PermissionDenied {
        return Check::new(Status::Fail, "door", "the node refused the observe handshake")
            .items(hubkeys::observe_permission_denied_lines(COMMAND, node, e));
    }
    Check::new(Status::Fail, "door", format!("cannot read a snapshot from {node}: {e}"))
}

fn build_check(snap: &WireSnapshot) -> Check {
    match &snap.identity {
        Some(id) => Check::new(
            Status::Pass,
            "build",
            format!(
                "{} — daemon \"{}\" ({})",
                id.build,
                id.name,
                if id.live { "LIVE" } else { "paper" }
            ),
        ),
        None => Check::new(
            Status::Warn,
            "build",
            "the node publishes no identity block (it predates the field), so its build is unknown",
        ),
    }
}

/// The node's age. Under [`RESTARTED_RECENTLY_MS`] is WARN: the engines, feeds and links are still
/// settling, so every other row describes a node mid-start and the journal's early lines are the
/// restart's own.
fn uptime_check(snap: &WireSnapshot) -> Check {
    let Some(ms) = snap.uptime_ms else {
        return unknown_old(
            "uptime",
            "how long the node has been up",
            "systemctl show vike-tradehub -p ActiveEnterTimestamp (on the node's box)",
        );
    };
    let up = span(ms);
    if ms < RESTARTED_RECENTLY_MS {
        Check::new(
            Status::Warn,
            "uptime",
            format!("up {up}: restarted recently, so the rest of this table is mid-start"),
        )
    } else {
        Check::new(Status::Pass, "uptime", format!("up {up}"))
    }
}

/// A span as the two coarsest units that matter: `45s`, `3m 5s`, `2h 10m`, `4d 3h`.
fn span(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, hours) = (secs / 86_400, secs % 86_400 / 3600);
    let (mins, rest) = (secs % 3600 / 60, secs % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else if mins > 0 {
        format!("{mins}m {rest}s")
    } else {
        format!("{rest}s")
    }
}

fn core_check(snap: &WireSnapshot) -> Check {
    match &snap.fault {
        Some(fault) => Check::new(
            Status::Fail,
            "core",
            format!("the core is halted in safe state after a handler fault: {fault}"),
        ),
        None => Check::new(Status::Pass, "core", "no handler fault"),
    }
}

/// An account's route key from a venue and the operator's label, through the workspace's one
/// spelling. A label the grammar refuses can never have been mounted; it stays apart from the
/// default account rather than merging into it.
fn route_key(venue: &str, label: Option<&str>) -> String {
    let label = label.map_or(AccountLabel::Default, |l| {
        AccountLabel::parse(l).unwrap_or_else(|_| AccountLabel::Named(l.to_string()))
    });
    route_key_of(venue, &label)
}

/// The key a published block answers to; a node that predates route keys sends none.
fn block_key(block: &WireVenueBlock) -> String {
    if block.route_key.is_empty() {
        route_key(&block.venue, block.account.as_deref())
    } else {
        block.route_key.clone()
    }
}

/// Everything the snapshot and the directory say about one account.
struct AccountView<'a> {
    key: String,
    /// The newest block published under this key.
    block: Option<&'a WireVenueBlock>,
    /// The node lists it NOT RUNNING.
    stopped: Option<&'a WireNotRunning>,
    /// The tier its ACTIVE row states (known only when the directory was read).
    row_tier: Option<&'a str>,
}

impl<'a> AccountView<'a> {
    /// The block of an engine that RUNS: a NOT RUNNING account's retained block is not one.
    fn running(&self) -> Option<&'a WireVenueBlock> {
        if self.stopped.is_some() { None } else { self.block }
    }
}

fn slot<'m, 'a>(
    map: &'m mut BTreeMap<String, AccountView<'a>>,
    key: String,
) -> &'m mut AccountView<'a> {
    map.entry(key.clone()).or_insert_with(|| AccountView {
        key,
        block: None,
        stopped: None,
        row_tier: None,
    })
}

/// The accounts of the frame, the NOT RUNNING list and the directory, by route key.
fn collect_accounts<'a>(
    snap: &'a WireSnapshot,
    directory: Option<&'a WireDirectory>,
) -> Vec<AccountView<'a>> {
    let mut map = BTreeMap::new();
    // The core's list only grows, so a later block under one key is the newer engine's.
    for block in &snap.venues {
        slot(&mut map, block_key(block)).block = Some(block);
    }
    for stopped in &snap.not_running {
        slot(&mut map, stopped.route_key.clone()).stopped = Some(stopped);
    }
    let rows: &[WireDirectoryAccount] = directory.map_or(&[], |d| d.accounts.as_slice());
    for account in rows {
        slot(&mut map, route_key(&account.venue, account.label.as_deref())).row_tier =
            Some(account.tier.as_str());
    }
    // A LANE TWIN (`vike_secrets::lane_twin`: binance's `PERP` perpetual lane) is derived from the default
    // account's row while it states a margin leverage and has NO row of its own, so it would read as an
    // engine "no active row states" for ever. It is judged against the row it is derived from: expected,
    // and a WARN only if it runs at another tier than that row. Recognised by the table's label AND the
    // symbol it is mounted on, the same shape the control door uses.
    for block in &snap.venues {
        let Some(twin) = vike_secrets::lane_twin(&block.venue) else { continue };
        let key = block_key(block);
        if key != route_key(&block.venue, Some(twin.label))
            || block.symbols.first().map(String::as_str) != Some(twin.symbol)
        {
            continue;
        }
        let parent = map.get(&block.venue).and_then(|p| p.row_tier);
        let own = slot(&mut map, key);
        if own.row_tier.is_none() {
            own.row_tier = parent;
        }
    }
    map.into_values().collect()
}

fn engine_checks(accounts: &[AccountView<'_>], rows_known: bool) -> Vec<Check> {
    if accounts.is_empty() {
        return vec![Check::new(
            Status::Fail,
            "engine",
            "no account is known: no engine block, no NOT RUNNING account and no active row",
        )];
    }
    accounts.iter().filter_map(|a| engine_check(a, rows_known)).collect()
}

fn engine_check(a: &AccountView<'_>, rows_known: bool) -> Option<Check> {
    let check = if let Some(n) = a.stopped {
        let tier = n.tier.as_deref().or(a.row_tier).unwrap_or("not stated");
        Check::new(Status::Warn, "engine", format!("NOT RUNNING ({tier}): {}", n.reason))
    } else if let Some(b) = a.block {
        match (b.tier.as_deref(), a.row_tier) {
            (None, _) => Check::new(
                Status::Warn,
                "engine",
                "RUNNING, tier not published (a node that predates the field): treat it as live",
            ),
            (Some(eff), Some(row)) if eff != row => Check::new(
                Status::Warn,
                "engine",
                format!("RUNNING at tier {eff}, but its active row states {row}"),
            ),
            (Some(eff), Some(_)) => Check::new(
                Status::Pass,
                "engine",
                format!("RUNNING at tier {eff} (the active row agrees)"),
            ),
            (Some(eff), None) if rows_known => Check::new(
                Status::Warn,
                "engine",
                format!(
                    "RUNNING at tier {eff}, but no active row states it (deleted, or two active \
                     tiers): it keeps running as it is"
                ),
            ),
            (Some(eff), None) => {
                Check::new(Status::Pass, "engine", format!("RUNNING at tier {eff}"))
            }
        }
    } else {
        // Only the directory names it: an active row with no engine and no reason.
        let row = a.row_tier?;
        Check::new(
            Status::Fail,
            "engine",
            format!("active row ({row}) but NO engine and NO NOT RUNNING reason on the wire"),
        )
    };
    Some(check.scope(&a.key))
}

fn trading_checks(accounts: &[AccountView<'_>]) -> Vec<Check> {
    let mut out = Vec::new();
    let mut active = Vec::new();
    for a in accounts {
        let Some(block) = a.running() else { continue };
        match block.trading_state {
            WireTradingState::Active => active.push(a.key.as_str()),
            WireTradingState::Reducing => out.push(
                Check::new(
                    Status::Warn,
                    "trading",
                    "REDUCING: only orders that reduce a position are admitted",
                )
                .scope(&a.key),
            ),
            WireTradingState::Halted => out.push(
                Check::new(
                    Status::Fail,
                    "trading",
                    "HALTED: no order that opens or adds risk is admitted (covered reduces pass)",
                )
                .scope(&a.key)
                .items(vec![
                    "why: journalctl -u vike-tradehub | grep -E 'LINK DEAD-MAN HALTED THE VENUE|HALT'"
                        .to_string(),
                    "a link dead-man halt lifts by itself when the links are back; any other: \
                     vike-cli trade resume --node <control addr>"
                        .to_string(),
                ]),
            ),
        }
    }
    if !active.is_empty() {
        out.insert(
            0,
            Check::new(
                Status::Pass,
                "trading",
                format!("{} engine(s) Active: {}", active.len(), active.join(", ")),
            ),
        );
    }
    out
}

fn duplicate_checks(snap: &WireSnapshot, accounts: &[AccountView<'_>]) -> Vec<Check> {
    let mut per_key: BTreeMap<&str, usize> = BTreeMap::new();
    for block in snap.venues.iter().filter(|b| !b.route_key.is_empty()) {
        *per_key.entry(block.route_key.as_str()).or_default() += 1;
    }
    let mut out: Vec<Check> = per_key
        .into_iter()
        .filter(|&(_, n)| n > 1)
        .map(|(key, n)| {
            Check::new(
                Status::Fail,
                "duplicates",
                format!(
                    "{n} blocks share this route key: a consumer that takes the first reads the \
                     STOPPED engine (a current node publishes only the newest)"
                ),
            )
            .scope(key)
        })
        .collect();
    for a in accounts.iter().filter(|a| a.stopped.is_some() && a.block.is_some()) {
        out.push(
            Check::new(
                Status::Warn,
                "duplicates",
                "a stopped engine's block is still published: its equity and positions stay \
                 valued, and no engine manages them",
            )
            .scope(&a.key),
        );
    }
    let unkeyed = snap.venues.iter().filter(|b| b.route_key.is_empty()).count();
    if unkeyed > 0 {
        out.push(Check::new(
            Status::Unknown,
            "duplicates",
            format!(
                "unknown (not on the wire): {unkeyed} block(s) carry no route key (a node that \
                 predates account routing), so a stale duplicate cannot be told from a second \
                 account. Ask: vike-cli trade position ls --node <observe addr>"
            ),
        ));
    }
    if out.is_empty() {
        out.push(Check::new(
            Status::Pass,
            "duplicates",
            "no route key is published twice and no stopped engine keeps a block",
        ));
    }
    out
}

fn key_checks(snap: &WireSnapshot) -> Vec<Check> {
    if snap.key_changed.is_empty() {
        return vec![Check::new(Status::Pass, "keys", "no running account reports a changed key")];
    }
    snap.key_changed
        .iter()
        .map(|key| Check::new(Status::Warn, "keys", KEY_CHANGED_WARNING).scope(key))
        .collect()
}

fn recon_check(snap: &WireSnapshot, node: &str, features: Option<&[String]>) -> Check {
    if snap.recon.is_empty() {
        return match features.map(|f| f.iter().any(|s| s == FEATURE_RECON_VERBS)) {
            Some(true) => Check::new(Status::Pass, "recon", "nothing is held for an operator"),
            Some(false) => Check::new(
                Status::Unknown,
                "recon",
                format!(
                    "unknown: this node does not advertise \"{FEATURE_RECON_VERBS}\", so it \
                     publishes no held rows and an empty list proves nothing. Ask: journalctl -u \
                     vike-tradehub | grep 'reconcile divergence'"
                ),
            ),
            None => Check::new(
                Status::Unknown,
                "recon",
                format!(
                    "unknown: the node's capabilities could not be read, so an empty list cannot \
                     be told from an older node's. Ask: vike-cli trade recon ls --node {node}"
                ),
            ),
        };
    }
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for row in &snap.recon {
        *kinds.entry(row.kind.as_str()).or_default() += 1;
    }
    let kinds = kinds.iter().map(|(k, n)| format!("{k} x{n}")).collect::<Vec<_>>().join(", ");
    let items = snap
        .recon
        .iter()
        .flat_map(|r| {
            [
                format!(
                    "#{} {} {}: {}",
                    r.id,
                    r.account,
                    r.kind,
                    r.detail.replace(['\r', '\n'], " ")
                ),
                format!("   a confirm: {}", effect_of(&r.kind, r.proposed_event_count)),
            ]
        })
        .collect();
    Check::new(
        Status::Warn,
        "recon",
        format!(
            "{} held for an operator ({kinds}). List: vike-cli trade recon ls --node {node}. \
             Resolve one (read the venue first): vike-cli trade recon confirm <id> --node \
             <control addr>",
            snap.recon.len()
        ),
    )
    .items(items)
}

/// Whether an order status word (the FSM word or an older node's Debug spelling) still rests.
fn is_resting(status: &str) -> bool {
    !matches!(
        status.to_ascii_lowercase().as_str(),
        "filled" | "canceled" | "cancelled" | "rejected" | "denied" | "expired"
    )
}

fn order_checks(snap: &WireSnapshot) -> Vec<Check> {
    let resting: Vec<&WireOrderView> =
        snap.orders.iter().filter(|o| is_resting(&o.status)).collect();
    let Some(oldest) = resting.first() else {
        return vec![Check::new(Status::Pass, "orders", "no order is resting")];
    };
    let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
    for order in &resting {
        *by_status.entry(order.status.to_ascii_uppercase()).or_default() += 1;
    }
    let by_status =
        by_status.iter().map(|(s, n)| format!("{s} x{n}")).collect::<Vec<_>>().join(", ");
    vec![
        Check::new(
            Status::Pass,
            "orders",
            format!(
                "{} resting ({by_status}); first in registry order: {} {} {}",
                resting.len(),
                oldest.client_order_id,
                oldest.venue,
                oldest.symbol
            ),
        ),
        order_age_check(&resting),
    ]
}

/// The age of the oldest resting order, from [`WireOrderView::age_ms`]. The node counts from the
/// moment it first SAW the order, so after a restart the figure is at most the uptime: an order
/// that rested through the restart reads as old as the process, which is what the node can know.
/// Orders carrying no age are ignored; when none carries one the node predates the field. `resting`
/// is never empty.
fn order_age_check(resting: &[&WireOrderView]) -> Check {
    let aged = resting.iter().filter_map(|o| o.age_ms.map(|age| (age, *o)));
    let Some((age, order)) = aged.max_by_key(|(age, _)| *age) else {
        return unknown_old(
            "order-age",
            "how old the oldest resting order is",
            &format!(
                "journalctl -u vike-tradehub | grep -F '{}', or the venue's open-orders page",
                resting[0].client_order_id
            ),
        );
    };
    let which = format!("{} {} {}", order.client_order_id, order.venue, order.symbol);
    if age > STALE_ORDER_MS {
        Check::new(
            Status::Warn,
            "order-age",
            format!(
                "the oldest resting order is {} old (over 24h): {which}. Counted from when this \
                 node first saw it, so a restart resets the clock. Look at it: vike-cli trade \
                 order ls --node <observe addr>",
                span(age)
            ),
        )
    } else {
        Check::new(
            Status::Pass,
            "order-age",
            format!("the oldest resting order is {} old: {which}", span(age)),
        )
    }
}

/// The HALT sentinel file, from [`WireSnapshot::halt_sentinel`]. Present is a FAIL: while it exists
/// every adapter refuses an order that opens or adds risk, whatever the engines' state says, and
/// nothing clears it but an operator removing the file.
fn halt_check(snap: &WireSnapshot) -> Check {
    match snap.halt_sentinel {
        Some(true) => Check::new(
            Status::Fail,
            "halt-sentinel",
            "the HALT sentinel file EXISTS: every adapter refuses an order that opens or adds \
             risk, whatever the engines' state (cancels and covered reduces still pass)",
        )
        .items(vec![
            "it does not clear itself: remove the file once the cause is dealt with \
             (docs/ops/kill-switches.md)"
                .to_string(),
            "its path: journalctl -u vike-tradehub | grep 'HALT sentinel path resolved'"
                .to_string(),
        ]),
        Some(false) => Check::new(Status::Pass, "halt-sentinel", "no HALT sentinel file"),
        None => unknown_old(
            "halt-sentinel",
            "whether the HALT sentinel file exists (adapters refuse orders while it does, \
             whatever the engines' state)",
            "journalctl -u vike-tradehub | grep 'HALT sentinel path resolved', then ls that path",
        ),
    }
}

fn position_checks(accounts: &[AccountView<'_>]) -> Vec<Check> {
    let mut items = Vec::new();
    let mut unmanaged = 0usize;
    for a in accounts {
        let Some(block) = a.block else { continue };
        for p in block.positions.iter().filter(|p| p.size != 0.0) {
            let leverage =
                if p.leverage > 0.0 { format!("{:.1}x", p.leverage) } else { "-".to_string() };
            let stopped = a.stopped.is_some();
            unmanaged += usize::from(stopped);
            items.push(format!(
                "{} {} {} {} @ {} (unrealized {:.2}, leverage {leverage}){}",
                a.key,
                p.symbol,
                p.position_side,
                p.size,
                p.avg_px,
                p.unrealized,
                if stopped { "  <- account NOT RUNNING: nothing manages it" } else { "" }
            ));
        }
    }
    if items.is_empty() {
        return vec![Check::new(Status::Pass, "positions", "flat: no non-flat position")];
    }
    let total = items.len();
    if total > MAX_ITEMS {
        items.truncate(MAX_ITEMS);
        items.push(format!("... and {} more", total - MAX_ITEMS));
    }
    let (status, tail) = if unmanaged > 0 {
        (Status::Warn, format!(", {unmanaged} on an account that is NOT RUNNING"))
    } else {
        (Status::Pass, String::new())
    };
    vec![Check::new(status, "positions", format!("{total} non-flat{tail}")).items(items)]
}

fn price_checks(accounts: &[AccountView<'_>]) -> Vec<Check> {
    let mut out = Vec::new();
    for a in accounts {
        let Some(block) = a.running() else { continue };
        if block.missing_prices > 0 {
            out.push(
                Check::new(
                    Status::Fail,
                    "prices",
                    format!(
                        "{} open position(s) have no priceable source: equity, margin and the \
                         risk sweeps cannot value them",
                        block.missing_prices
                    ),
                )
                .scope(&a.key),
            );
        }
    }
    if out.is_empty() {
        out.push(Check::new(Status::Pass, "prices", "every open position has a price source"));
    }
    out.push(unknown(
        "price-feeds",
        "whether each price-only feed is live now (a priced position can carry a stale quote, and \
         a venue with nothing open proves nothing)",
        "journalctl -u vike-datahub -u vike-tradehub | grep -i -E 'strat|runs no price feed'",
    ));
    out
}

/// The facts that live only on the node's box or in its journal.
fn box_only_checks(accounts: &[AccountView<'_>]) -> Vec<Check> {
    let mut out = vec![unknown(
        "journal",
        "ERROR lines since the node started",
        "journalctl -u vike-tradehub -u vike-gatewayhub -u vike-datahub --since <boot> | grep ' ERROR '",
    )];
    if accounts.iter().any(|a| a.key.split('#').next() == Some("hyperliquid")) {
        out.push(unknown(
            "leverage",
            "the leverage an account states and whether the exchange confirmed it",
            "vike-cli secrets accounts, and journalctl -u vike-gatewayhub | grep -i leverage",
        ));
        out.push(unknown(
            "hl-approval",
            "when a Hyperliquid API-wallet approval ends",
            "journalctl -u vike-gatewayhub | grep 'TRADE-AGENT:'",
        ));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// the renderers (pure)
// ---------------------------------------------------------------------------------------------

/// The human table: one header, one row per check with its sub-lines, the count of rows the wire
/// could not answer, and the `DOCTOR:` line.
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

/// The `--json` document.
fn json_body(node: &str, checks: &[Check]) -> String {
    let rows: Vec<serde_json::Value> = checks
        .iter()
        .map(|c| {
            let account = (!c.scope.is_empty()).then_some(c.scope.as_str());
            serde_json::json!({
                "status": c.status.word(),
                "check": c.name,
                "account": account,
                "detail": c.detail,
                "items": c.items,
            })
        })
        .collect();
    let word = match verdict(checks) {
        Verdict::Ok => "OK",
        Verdict::Warn => "WARN",
        Verdict::Fail => "FAIL",
    };
    let doc = serde_json::json!({
        "node": node,
        "verdict": word,
        "warn": count(checks, Status::Warn),
        "fail": count(checks, Status::Fail),
        "unknown": count(checks, Status::Unknown),
        "checks": rows,
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, numbers, arrays and nulls; serialization is total")
}

/// Entry point [`crate::cmd::trade::run`] routes `trade doctor` to.
pub(super) fn run(cmd: DoctorCmd, keys: &HubKeyring) -> ExitCode {
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
    let reads = read_node(&parsed.node, key.as_bytes());
    let checks = judge(&parsed.node, &reads);
    if parsed.json {
        println!("{}", json_body(&parsed.node, &checks));
    } else {
        println!("{}", human(&parsed.node, &checks));
    }
    ExitCode::from(exit_number(verdict(&checks)))
}

#[path = "doctor_tests.rs"]
#[cfg(test)]
mod doctor_tests;
