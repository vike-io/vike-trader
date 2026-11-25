//! What `vike-cli trade health --json` and the MCP tool `health_node` both answer: the reads, the one
//! judgement over them, and the document. The two doors call these functions and nothing else judges a
//! node, so they cannot disagree. Each door renders its own: the table, the exit code and `--help` are
//! `crates/vike-cli/src/cmd/trade/health.rs`'s, the tool's `gatewayhub` block is
//! `crates/vike-mcp/src/node_reads.rs`'s.
//!
//! It judges from data the node already publishes, with the observe keys and nothing else (it cannot
//! place, cancel or change anything). It sends no event and no command: everything below is a
//! [`CoreSnapshotWire`], a [`AccountDirectoryWire`], a `Welcome.features` list or a [`GatewayStatusReply`] as the
//! hubs publish them. It links no gatewayhub client: decision 0253 is why it asks the datahub.
//!
//! # The reads (all `Scope::Read`, one short-lived connection each)
//!
//! 1. `snapshot_once` — the frame. If it fails, the failure IS the first finding and nothing else is
//!    asked of the node: a node that is still STARTING answers every verb with `starting: <progress>`
//!    (`crates/vike-tradehub/src/server/starting.rs`'s `StartProgress`), a refused key is a handshake
//!    denial, anything else is "does not answer". A frame with `seq` 0 is the pre-fold placeholder, not a
//!    reading, and is reported as that.
//! 2. `strategy_status_with_features` — only for the node's advertised capabilities, so an empty
//!    `recon` list can be told from a node that predates the field.
//! 3. `directory` — the ACTIVE account rows, so the tier a row states can be set beside the tier an
//!    engine runs at, and an active row with no engine and no reason is found.
//! 4. [`ask_datahub`] — asked WHATEVER the door said, of the `config.datahub_addr` row's address, under the
//!    same device key at `Read`. It is what still answers with the tradehub down.
//!
//! # What it checks, and how each word is decided
//!
//! * `door` — ready, STARTING (WARN: retry shortly), pre-fold (WARN: only the build, `uptime` and
//!   `halt-sentinel` rows follow it, the two the publisher reads itself), refused key or unreachable
//!   (FAIL); `build` — the identity block's build string and what the node runs ([`running_tiers`]);
//!   `core` — a handler fault (FAIL, the core is halted in safe state); `counters` — the core's health counters
//!   ([`CoreSnapshotWire::counters`]): a venue event dropped for a NaN or an infinity is FAIL (no venue sends one),
//!   every other counter that moved (the restored orders among them) is an item of the row, information only;
//!   `log-lines` — the tradehub's own ERROR and WARN lines per target ([`CoreSnapshotWire::log_lines`]): an ERROR line
//!   within the last hour of the frame is WARN (the count is rising), never FAIL; older ones and WARN lines are items;
//!   `alerts-unsent` — the alerts that failed every ask to their webhook target
//!   ([`CoreSnapshotWire::alert_deliveries_failed`]): any is WARN until the node restarts (each was never sent), never FAIL;
//!   `journal-size` — the tradehub journal's size and growth at the node's last measure ([`CoreSnapshotWire::journal`]):
//!   WARN while the node judges it over either alert row, never FAIL. (`journal`, below, is another row: the other two
//!   daemons' log, which no frame carries.)
//! * `engine`, one row per account: RUNNING at a tier (WARN when the engine's tier differs from the
//!   active row's), NOT RUNNING with the node's reason (WARN), or an active row with no engine and no
//!   reason (FAIL). No account at all is WARN when the directory read and lists no row (nothing
//!   configured yet: a fresh server's empty node, owner 2026-11-16), FAIL when it did not read.
//!   The node's NOT RUNNING list wins over a block of the same route key: a stopped
//!   account keeps its last block (`crates/vike-tradehub/src/publish.rs`'s `current_venue_blocks`).
//!   An account's rows fold as arming folds them (`crates/vike-config/src/account_arming.rs`'s
//!   `stated_tier`): a `paper` row beside a `live` one states `live`, and two non-paper tiers state
//!   none, which is WARN on an engine that runs, because it keeps running and a restart will not run it.
//!   A RUNNING engine whose venue path is down ([`CoreSnapshotWire::venue_link_down`]: Dukascopy's JForex sidecar, which
//!   the gatewayhub's bridge restarts by itself, cTrader's account session, which the venue can end on a live
//!   socket, and OANDA's transactions stream; the last two state their own effect on orders) is never PASS: the row is
//!   WARN and adds the reason, the restarts that failed and the next try when the bridge states them, and what to expect
//!   (orders refused until the bridge has it back, unless the bridge says otherwise). ⚠ Only a venue whose
//!   bridge reports its path is covered; a PASS is still not a check of any other venue's path.
//!   A binance LANE TWIN (`binance#PERP` on `BTCUSDT.P`, derived from the default account's row) has
//!   no row of its own and is judged against the default account's row.
//!   An engine whose trade lease is closed for good ([`CoreSnapshotWire::trade_link_down`]), or whose lease has no live
//!   adapter behind it ([`VenueLinkDownWire::no_adapter`]), does not read RUNNING: WARN `NOT TRADING`.
//! * `gateway-link` and `price-link` — the tradehub's own links to the other two hubs
//!   ([`CoreSnapshotWire::trade_link_down`], [`CoreSnapshotWire::price_link_down`]; `gateway_link_checks` and
//!   `price_link_checks` carry the words and what each decides), and on `gateway-link` one row per account for the
//!   gatewayhub's session with the venue (`venue_session_checks`, [`CoreSnapshotWire::venue_path_up`]). This is what stops a
//!   node that lost a dead gatewayhub from reading `HEALTH: OK`. Neither row dials anything. `price-link` is UNKNOWN when
//!   no dial holds a session ([`CoreSnapshotWire::price_dials_up`]): none lost is then no reading of the datahub.
//! * `datahub` and `gatewayhub` — the datahub's word on the gatewayhub (`hub_checks` carries the words): this is how
//!   an operator tells a dead gatewayhub from a dead tradehub from a degraded datahub. This crate links no gatewayhub
//!   client, so it asks the datahub, and it names the hop and claims nothing past it: an up gatewayhub says nothing of a
//!   venue. A node advertising another datahub than the one asked is said on a second `datahub` row, UNKNOWN.
//! * `lists` — one WARN row per venue whose last kept-list read the gatewayhub recorded as REFUSED (oanda's start or
//!   refresh read, ig's list walk; the report's `list_refusals`, read from the daemon's memory): when, the refusal in
//!   words and `vike-cli data catalog refresh --venue V`, which reads it again (nothing does by itself: no timer). No row
//!   when none is refused, or when the datahub gives no report.
//! * `trading`, per running engine: Reducing is WARN, Halted is FAIL (no order that opens risk is
//!   admitted; covered reduces still pass).
//! * `duplicates` — one route key published twice is FAIL (a consumer that takes the first block reads
//!   the stopped engine; a current node publishes only the newest, so this fires against an older
//!   one); a stopped account that still publishes its block is WARN (its position stays valued and
//!   nothing manages it).
//! * `keys` — a running account whose database key changed after its session started is WARN.
//! * `balance` — a running account whose venue balance the node holds back from its engine
//!   ([`CoreSnapshotWire::balance_withheld`]: its equity is the engine's own book, not the venue's wallet) is WARN, one
//!   row per account with the reason in short words. No row when none is held (a node that predates the field
//!   publishes none either, so a PASS row would claim what it cannot know).
//! * `rate` — a running account whose conversion-rate table ([`VenueBlockWire::quote_rates`]) has a rate that is
//!   STALE while a position is open in a symbol quoted in its currency (`QuoteRateViewWire::held`) is WARN, one row
//!   per account naming the rates: the position is valued at the last rate and an opening order on that currency
//!   is denied (`quote-rate-stale`). No row otherwise: a stale rate nobody holds anything in is only shown under
//!   `trade equity`, and a node that predates the flag publishes none, so a PASS row would claim what it cannot know.
//! * `stops` — the operator's armed CORE-HELD reduce-only stops ([`CoreSnapshotWire::core_held_stops_listed`], the
//!   reduce-only trigger rows of the one emulated-order list): held by the node,
//!   not the venue, so no `orders` row counts them. PASS while all are armed, WARN while any is HELD for its
//!   account's first reconcile pass or was RE-ARMED after a close failed. No row when none is listed.
//! * `mounts` — a mount whose budget latch is spent (`MountViewWire::latched`, from the `StrategyStatus` read that
//!   also supplies the capabilities): WARN, one row, one item per mount, with the verb that clears it
//!   (`trade strategy reset-budget`). No row when none is latched (a node from before the flag writes none, so a PASS
//!   would claim what it cannot know). UNKNOWN when the read failed: silence there would read as "none is latched".
//!   On a node that dials no price feed for a mount's venue (`HubIdentityWire::prices_venue`: a paper-only node prices
//!   only the keyless venues it wired) a second `mounts` row is WARN with each such mount as an item, in the node's own
//!   words (`vike_tradehub_client::wire::no_price_feed`): no price reaches it, so it never trades.
//! * `recon` — held reconcile rows: count, kinds, the effect of a confirm (`trade reconcile`'s own words)
//!   and the commands that list and resolve them. WARN while any is held.
//! * `uptime` — [`CoreSnapshotWire::uptime_ms`]: under two minutes is WARN ("restarted recently": the
//!   rest of the table describes a node still settling), otherwise PASS with the span.
//! * `orders` / `order-age` / `positions` / `prices` — resting orders (count, and the first in
//!   registry order); the oldest resting order's age from [`OrderViewWire::age_ms`] (WARN when it is
//!   older than 24 hours; the node counts from the moment it FIRST SAW the order, so a restart resets
//!   the clock and the figure is never larger than the uptime); non-flat positions (WARN when one
//!   belongs to a NOT RUNNING account); and a position with no priceable source (FAIL).
//! * `halt-sentinel` — [`CoreSnapshotWire::halt_sentinel`]: the file exists is FAIL (every adapter
//!   refuses an order that opens or adds risk, whatever the engines say), absent is PASS.
//!
//! # Not on the wire: said, never guessed
//!
//! A check whose data the frame does not carry is an UNKNOWN row that names the command that answers
//! it. Two words tell the two causes apart: `unknown (not on the wire)` is a fact no node publishes
//! (whether a price feed is live, an account's leverage, a Hyperliquid approval, the other two daemons'
//! ERROR lines), and `unknown (node too old)` is a fact a CURRENT node publishes and this one did not
//! (`uptime`, `order-age`, `halt-sentinel`, `counters`, `log-lines`, `alerts-unsent`, `journal-size`: additive frame fields, absent
//! from an older node's frame, which reads as "not said" and never as zero, as a young order or as "no sentinel").
//! UNKNOWN rows do not move the verdict; the line before the verdict tallies them so an OK is never
//! read as "everything was checked".
//!
//! `price-feeds` stays UNKNOWN on every node: how old a venue's last price is lives in the core's
//! price board (`vike-execution`, a measured crate), and the frame has no field for it.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::gateway_status::GatewayStatusReply;
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_hub_protocol::call::HubCallError;
use vike_model::ArmingBlock;
use vike_model::TradingState;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::{AccountRow, AccountRowActiveTier, AccountRows};
use vike_tradehub_client::node_words::running_tiers;
use vike_tradehub_client::observe_decode::map_counters;
use vike_tradehub_client::proto::{
    FEATURE_HUB_LINKS, FEATURE_RECONCILE_VERBS, STARTING_ERROR_PREFIX, advertised_datahub,
};
use vike_tradehub_client::wire::{
    AccountDirectoryAccountWire, AccountDirectoryWire, ArmingNotRunningViewWire,
    CORE_HELD_STOP_MEANING, CoreSnapshotWire, KEY_CHANGED_WARNING, MountViewWire,
    OrderEmulationStopViewWire, OrderViewWire, PriceLinkDownWire, TradeLinkDownWire,
    VenueBlockWire, VenueLinkDownWire, account_label, block_key, route_key,
};
use vike_utility::errors::FailureOutcome;

use crate::first_frame::is_pre_fold;
use crate::hubkeys;

mod accounts;
mod book;
mod hubs;
mod links;

use accounts::{AccountView, collect_accounts, engine_checks};
use book::{
    alert_deliveries_check, balance_checks, box_only_checks, counters_check, duplicate_checks,
    halt_check, journal_size_check, key_checks, log_lines_check, mount_checks,
    no_price_feed_mount_check, order_checks, position_checks, price_checks, rate_checks,
    reconcile_check, stop_checks, trading_checks,
};
use hubs::{Beside, foreign_datahub_check, hub_checks};
use links::{gateway_link_checks, leased_blocks, price_link_checks, switched_off};

/// The command name every message prints.
pub const COMMAND: &str = "trade health";

/// The most items a row lists before it says how many more there are.
const MAX_ITEMS: usize = 10;

/// A node up for less than this has just (re)started: `uptime` is a WARN.
pub const RESTARTED_RECENTLY_MS: u64 = 120_000;

/// A resting order older than this is a WARN: a limit order nobody has looked at for a day.
const STALE_ORDER_MS: u64 = vike_model::MS_PER_DAY_U64;

/// How long the datahub gets to answer. It caps its own wait on the gatewayhub at a few seconds
/// (`crates/vike-datahub/src/server/gateway_status.rs`'s `GATEWAY_STATUS_WAIT`), so past this the datahub itself is stuck.
const DATAHUB_REPLY_WAIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------------------------
// the findings
// ---------------------------------------------------------------------------------------------

/// One row's word. `Unknown` is not a verdict word: it marks a fact the wire does not carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pass,
    Warn,
    Fail,
    Unknown,
}

impl Status {
    pub fn word(self) -> &'static str {
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
pub struct Check {
    pub status: Status,
    pub name: &'static str,
    pub scope: String,
    pub detail: String,
    pub items: Vec<String>,
}

impl Check {
    pub fn new(status: Status, name: &'static str, detail: impl Into<String>) -> Self {
        Check { status, name, scope: String::new(), detail: detail.into(), items: Vec::new() }
    }

    pub fn scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = scope.into();
        self
    }

    pub fn items(mut self, items: Vec<String>) -> Self {
        self.items = items;
        self
    }
}

/// A fact the wire does not carry, with the command that answers it.
pub fn unknown(name: &'static str, what: &str, ask: &str) -> Check {
    Check::new(Status::Unknown, name, format!("unknown (not on the wire): {what}. Ask: {ask}"))
}

/// A fact a CURRENT node puts on its frame and this node's frame lacks, because the node predates
/// the field: the same row as [`unknown`] with the other cause named, so an operator reads "upgrade
/// the node" instead of "the wire cannot say".
fn unknown_old(name: &'static str, what: &str, ask: &str) -> Check {
    Check::new(Status::Unknown, name, format!("unknown (node too old): {what}. Ask: {ask}"))
}

/// How an operator reads the node's log ON ITS BOX, right on both kinds of server install (decision 0260): the
/// supervisor keeps every daemon in one log behind `bin/vikectl logs`, and a box that keeps system units has one
/// journal per unit. `units` is that box's `-u` list; `filter` follows the log (`| grep …`, `--since …`).
fn box_log(units: &str, filter: &str) -> String {
    format!("bin/vikectl logs{filter} (system units: journalctl {units})")
}

/// Whether a daemon runs, asked ON ITS BOX, on both kinds of server install (decision 0260).
fn box_status(unit: &str) -> String {
    format!("bin/vikectl status (system units: systemctl status {unit})")
}

/// The summary of all rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Ok,
    Warn,
    Fail,
}

pub fn count(checks: &[Check], status: Status) -> usize {
    checks.iter().filter(|c| c.status == status).count()
}

pub fn verdict(checks: &[Check]) -> Verdict {
    if count(checks, Status::Fail) > 0 {
        Verdict::Fail
    } else if count(checks, Status::Warn) > 0 {
        Verdict::Warn
    } else {
        Verdict::Ok
    }
}

// ---------------------------------------------------------------------------------------------
// the reads
// ---------------------------------------------------------------------------------------------

/// What the node answered. `features` and `directory` are `None` when they were not asked (the
/// door was not ready), and an `Err` when they were asked and failed.
pub struct Reads {
    pub snap: Result<CoreSnapshotWire, HubCallError>,
    pub features: Option<Result<Vec<String>, HubCallError>>,
    pub directory: Option<Result<AccountDirectoryWire, HubCallError>>,
    /// The mount rows the same `StrategyStatus` read answered (their `latched` flag is the budget latch, decision
    /// 0239). `None` when the read was not asked or failed (`features` carries the failure, and the `mounts` row says it).
    pub mounts: Option<Vec<MountViewWire>>,
    /// What the datahub said of the gatewayhub. `None` when it was not asked, and then no row says anything of either.
    pub hubs: Option<HubLeg>,
}

/// The node's three reads, snapshot first: a door that is not ready is asked nothing more.
pub fn read_node(node: &str, key: vike_hub_protocol::access::Signer<'_>) -> Reads {
    let snap = vike_tradehub_client::snapshot_once(node, key);
    if !snap.as_ref().is_ok_and(|s| !is_pre_fold(s)) {
        return Reads { snap, features: None, directory: None, mounts: None, hubs: None };
    }
    let status = vike_tradehub_client::strategy_status_with_features(node, key);
    let mounts = status.as_ref().ok().map(|(s, _)| s.mounts.clone());
    let features = status.map(|(_, f)| f);
    let directory = vike_tradehub_client::directory(node, key);
    Reads { snap, features: Some(features), directory: Some(directory), mounts, hubs: None }
}

/// The datahub this box dials, and what it answered.
pub struct HubLeg {
    pub addr: String,
    pub answer: DatahubAnswer,
}

/// What the datahub answered when asked about the gatewayhub.
pub enum DatahubAnswer {
    /// It could not be dialled, or stopped answering.
    NoAnswer(String),
    /// It answered and refused this box: its key or its absence, or this client's protocol (an older
    /// datahub, a version skew).
    Refused(String),
    /// It answered and predates the verb: unknown, never "down".
    TooOld,
    /// Its word, which a gatewayhub that is down is one of.
    Said(GatewayStatusReply),
}

/// Ask the datahub at `addr` about the gatewayhub. `keys` is `None` on a box holding no device key, which still
/// serves a key-less datahub.
pub fn ask_datahub(addr: &str, keys: Option<&HubKeys>) -> DatahubAnswer {
    let mut client = match DatahubClient::connect_with_keys(addr, keys, Scope::Read) {
        Ok(client) => client,
        // A refused key, an older datahub, a version skew: it answered, and none of them is "no answer".
        Err(e) if e.failure_outcome() == FailureOutcome::Refused => {
            return DatahubAnswer::Refused(e.to_string());
        }
        Err(e) => return DatahubAnswer::NoAnswer(e.to_string()),
    };
    if !client.serves_gateway_status() {
        return DatahubAnswer::TooOld;
    }
    client.set_reply_timeout(Some(DATAHUB_REPLY_WAIT));
    client.gateway_status().map_or_else(DatahubAnswer::NoAnswer, DatahubAnswer::Said)
}

// ---------------------------------------------------------------------------------------------
// the judgement (pure)
// ---------------------------------------------------------------------------------------------

/// Judge what the node answered into rows. PURE.
pub fn judge(node: &str, reads: &Reads) -> Vec<Check> {
    let hubs = reads.hubs.as_ref();
    let snap = match &reads.snap {
        Ok(snap) => snap,
        Err(e) => {
            let (door, answered) = door_failure(node, e);
            let mut out = vec![door];
            out.extend(hub_checks(hubs, Beside::no_rows(answered)));
            return out;
        }
    };
    if is_pre_fold(snap) {
        let mut out = vec![
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
        out.extend(hub_checks(hubs, Beside::no_rows(true)));
        return out;
    }
    let features: Option<&[String]> =
        reads.features.as_ref().and_then(|r| r.as_ref().ok()).map(Vec::as_slice);
    let directory: Option<&AccountDirectoryWire> =
        reads.directory.as_ref().and_then(|r| r.as_ref().ok());
    let accounts = collect_accounts(snap, directory);
    // A directory that read and lists nothing is an answer: no account is active. Only a failed read
    // leaves the rows unknown.
    let rows_known = directory.is_some();

    let mut out = vec![
        Check::new(
            Status::Pass,
            "door",
            format!("ready: serving snapshots (seq {})", snap.sequence),
        ),
        build_check(snap),
        uptime_check(snap),
        core_check(snap),
        counters_check(snap),
        log_lines_check(snap),
        alert_deliveries_check(snap),
        journal_size_check(snap),
    ];
    if let Some(Err(e)) = &reads.directory {
        out.push(Check::new(
            Status::Unknown,
            "rows",
            format!(
                "unknown: the node's directory read failed ({e}), so the tier each active row \
                 states is not compared with the engines. Ask: vike-cli account ls"
            ),
        ));
    }
    out.extend(engine_checks(&accounts, rows_known, &snap.venue_link_down, &snap.trade_link_down));
    out.extend(gateway_link_checks(
        &accounts,
        &snap.trade_link_down,
        features,
        hubs,
        &snap.venue_link_down,
        &snap.venue_path_up,
    ));
    let price_links =
        price_link_checks(&accounts, &snap.price_link_down, features, snap.price_dials_up);
    let price_link_reads = price_links.iter().any(|c| c.status != Status::Unknown);
    out.extend(price_links);
    // A lease to judge the orders by, or nothing here that wants the gatewayhub (a paper-only box, every account off).
    let needs_no_gatewayhub_row = !leased_blocks(&accounts).is_empty()
        || !snap.trade_link_down.is_empty()
        || !accounts.iter().any(|a| a.stopped.is_some_and(|n| !switched_off(n)));
    out.extend(hub_checks(
        hubs,
        Beside { door_answered: true, needs_no_gatewayhub_row, price_link_reads },
    ));
    out.extend(foreign_datahub_check(features, hubs));
    out.extend(trading_checks(&accounts));
    out.extend(duplicate_checks(snap, &accounts));
    out.extend(key_checks(snap));
    out.extend(balance_checks(snap));
    out.extend(rate_checks(&accounts));
    out.extend(stop_checks(snap, node));
    out.extend(match &reads.features {
        Some(Err(e)) => Some(Check::new(
            Status::Unknown,
            "mounts",
            format!(
                "unknown: the node's StrategyStatus read failed ({e}), so a mount's budget latch \
                 cannot be seen. Ask: vike-cli trade strategy ls --node {node}"
            ),
        )),
        _ => mount_checks(reads.mounts.as_deref().unwrap_or_default()),
    });
    out.extend(no_price_feed_mount_check(
        snap.identity.as_ref(),
        reads.mounts.as_deref().unwrap_or_default(),
    ));
    out.push(reconcile_check(snap, node, features));
    out.extend(order_checks(snap));
    out.extend(position_checks(&accounts));
    out.extend(price_checks(&accounts));
    out.push(halt_check(snap));
    out.extend(box_only_checks(&accounts));
    out
}

/// The first finding of a node that did not give a frame, and whether its process answered at all.
fn door_failure(node: &str, e: &HubCallError) -> (Check, bool) {
    match (e, e.failure_outcome()) {
        // The node's own "starting" answer (decided by the client crate, by the frame's id or an older
        // node's words); the progress shown is the text after its prefix.
        (_, FailureOutcome::NotNow) => {
            let text = e.to_string();
            let progress = match text.find(STARTING_ERROR_PREFIX) {
                Some(at) => text[at + STARTING_ERROR_PREFIX.len()..].to_string(),
                None => text,
            };
            let door = Check::new(
                Status::Warn,
                "door",
                "STARTING: the door listens but the accounts are not all up yet. Ask again shortly",
            );
            (door.items(vec![progress]), true)
        }
        (HubCallError::AccessDenied(_), _) => {
            let door = Check::new(Status::Fail, "door", "the node refused the observe handshake");
            (door.items(hubkeys::observe_permission_denied_lines(COMMAND, node, e)), true)
        }
        (_, FailureOutcome::Refused) => (
            Check::new(Status::Fail, "door", format!("TRADEHUB at {node} gave no snapshot: {e}")),
            true,
        ),
        _ => {
            let door = format!("TRADEHUB at {node} does not answer: {e}");
            let door = Check::new(Status::Fail, "door", door).items(vec![
                "a dead SSH forward reads the same as a dead daemon. Ask: vike-cli backend status"
                    .to_string(),
            ]);
            (door, false)
        }
    }
}

fn build_check(snap: &CoreSnapshotWire) -> Check {
    match &snap.identity {
        Some(id) => Check::new(
            Status::Pass,
            "build",
            format!("{} — daemon \"{}\" ({})", id.build, id.name, running_tiers(Some(snap), &[])),
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
fn uptime_check(snap: &CoreSnapshotWire) -> Check {
    let Some(ms) = snap.uptime_ms else {
        return unknown_old(
            "uptime",
            "how long the node has been up",
            "ps -o lstart= -p <the tradehub's pid from bin/vikectl status> (system units: systemctl show \
             vike-tradehub -p ActiveEnterTimestamp), on the node's box",
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
pub fn span(ms: u64) -> String {
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

fn core_check(snap: &CoreSnapshotWire) -> Check {
    match &snap.fault {
        Some(fault) => Check::new(
            Status::Fail,
            "core",
            format!("the core is halted in safe state after a handler fault: {fault}"),
        ),
        None => Check::new(Status::Pass, "core", "no handler fault"),
    }
}

/// Whether `block` has the shape of `twin`, as far as a frame can say (`vike_venues::AccountRowLaneTwin::is_shaped`). A tier
/// word the parser refuses reads as a tier not said.
pub(crate) fn is_lane_twin_block(
    twin: &vike_venues::AccountRowLaneTwin,
    block: &VenueBlockWire,
) -> bool {
    let tier: Option<vike_model::AccountTier> =
        block.tier.as_deref().and_then(|word| word.parse().ok());
    block.symbols.first().is_some_and(|symbol| twin.is_shaped(&block_key(block), symbol, tier))
}

/// What a lost link says for itself, or that it says nothing.
fn said(why: &str) -> &str {
    if why.is_empty() { "no reason given" } else { why }
}

/// How one lost trade link reads after the account's name: the ONE wording, `trade status` prints it too.
pub fn trade_link_words(link: &TradeLinkDownWire) -> String {
    let why = said(&link.why);
    if link.closed {
        format!(
            "its trade lease to the gatewayhub is CLOSED FOR GOOD ({why}): no order or cancel of this account \
             reaches the venue and nothing redials. Fix the cause, then set the account's `account.active` off \
             and on to open a new lease"
        )
    } else {
        format!(
            "its trade link to the gatewayhub is DOWN for {} ({why}), redialling: no order of this account \
             reaches the venue until it is back",
            span(link.down_ms)
        )
    }
}

/// How one lost price link reads after the venue's name: the ONE wording, `trade status` prints it too.
pub fn price_link_words(link: &PriceLinkDownWire) -> String {
    format!(
        "its price feed's lane to the datahub is DOWN for {} ({}): this venue's strategies hear no quote, tick or bar",
        span(link.down_ms),
        said(&link.why)
    )
}

// ---------------------------------------------------------------------------------------------
// the document (pure)
// ---------------------------------------------------------------------------------------------

/// The `--json` document: `node`, `verdict`, the three counts, `checks[]`.
pub fn document(node: &str, checks: &[Check]) -> Value {
    let rows: Vec<Value> = checks
        .iter()
        .map(|c| {
            let account = (!c.scope.is_empty()).then_some(c.scope.as_str());
            json!({
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
    json!({
        "node": node,
        "verdict": word,
        "warn": count(checks, Status::Warn),
        "fail": count(checks, Status::Fail),
        "unknown": count(checks, Status::Unknown),
        "checks": rows,
    })
}

/// What a confirm DOES to a row of this kind, in one line. The whole menu: the node's fold
/// (`vike_core`'s `confirm_reconcile`) is the authority, this only words it. `vike-cli trade reconcile` words
/// a held row with it too.
pub fn effect_of(kind: &str, proposed_events: usize) -> String {
    use vike_model::ReconcileDivergenceKind as Kind;
    // The wire carries the kind's word; a word this build does not know (a newer node's kind) keeps
    // the generic line. Every known kind is decided below, exhaustively.
    let generic = || format!("fold {proposed_events} proposed event(s); the detail says which");
    let Some(kind) = Kind::from_word(kind) else { return generic() };
    match kind {
        Kind::OrphanLocalPosition => "drop the LOCAL position at its own average price (zero PnL, no \
                                      fee); right only if the venue really holds nothing"
            .to_string(),
        // A venue row it cannot price proposes no fill, and is held with its confirm refused or
        // acknowledging only (`vike_execution`'s `UnpricedHold`), so zero events is the tell here too.
        Kind::PositionOnlyExternal if proposed_events == 0 => {
            "books nothing: no fill can be proposed from this venue row; the detail says why, and \
             whether a confirm refuses or only acknowledges"
                .to_string()
        }
        Kind::PositionOnlyExternal => {
            "adopt the VENUE's position into the book (cost basis = the venue's average price)"
                .to_string()
        }
        // A drift on a FLAT, UNPRICED venue row proposes no fill (the node drops the local remainder
        // instead), so zero events is the tell; the detail line below it says which case it is.
        Kind::PositionDrift if proposed_events == 0 => {
            "if the detail says the venue row is flat and unpriced: close the LOCAL remainder at \
             its own average price (zero PnL, no fee), never at a venue price; if it says a spot \
             wallet holds less: shrink the LOCAL position to the wallet's figure the same way; \
             otherwise folds nothing"
                .to_string()
        }
        Kind::PositionDrift => {
            "resize the book to the VENUE's position, booking any close at the venue's average \
             price"
                .to_string()
        }
        Kind::MissingFill => "book the fill the book missed".to_string(),
        Kind::BalanceDrift => "adopt the VENUE's cash balance as the book's new anchor".to_string(),
        Kind::OrphanLocalOrder => "acknowledge only: folds and cancels nothing".to_string(),
        Kind::OrderTermsDrift => {
            "adopt the VENUE's terms (limit price, trigger price and/or quantity) onto the book's order; nothing \
             is sent to the venue"
                .to_string()
        }
        Kind::JournalDivergence => {
            "re-register the lost order(s) the detail names; acknowledge only if it names none"
                .to_string()
        }
        Kind::MissingTerminal | Kind::UnknownOrder => generic(),
    }
}
