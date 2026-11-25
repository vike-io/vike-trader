//! `vike-cli trade status` — ask a running vike-tradehub WHAT IT IS DOING: the trading MODE
//! and the mounted-strategy REGISTRY, in one output.
//!
//! ⚠ **This was `vike-cli strategy-status`, a top-level verb, and the move is ruling 17 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`.** Two things were wrong
//! and only one of them was the name. The name half: it is a question about a node the `trade`
//! surface already holds an Observe connection to — the same family as that REPL's `orders`,
//! `positions` and the old `state` read — and it was the only member of the family promoted to the
//! top level. The half that mattered: the REPL's `state` was a READ and a WRITE on one word
//! (`state` printed the mode, `state halted` HALTED THE DAEMON, and the only thing between them was
//! a second token), so renaming `status` around it would have left a destructive action reachable
//! by adding a word to a read. `state` is therefore GONE — from the REPL and from `vike-cli trade`
//! alike — `halt`/`resume` are their own verbs named for what they DO, and this ONE read answers
//! both former questions: the mode the old `state` printed, and the registry `strategy-status`
//! returned.
//!
//! # Two reads, because the node has no ONE verb that answers both
//!
//! The registry comes from [`vike_tradehub_client::strategy_status`] (`Request::StrategyStatus`, the
//! split-plane B4 read verb) and the mode from [`vike_tradehub_client::snapshot_once`]
//! (`Request::Snapshot`). Both are per-call short-lived [`vike_hub_protocol::auth::Scope::Read`]
//! connections; neither carries the other's field, and inventing a merged wire verb would be a
//! protocol change (a `NODE_PROTO_VERSION` bump breaks the handshake against every running node —
//! `crates/vike-tradehub-client/src/proto.rs`'s `FEATURE_STRATEGY_VERBS` says why) to
//! save one round trip on an operator-typed command.
//!
//! ⚠ **The registry read runs FIRST and decides the EXIT CODE — it does not decide what is
//! printed.** It is the feature-negotiated half, so its failure is the one an operator has to act
//! on (upgrade that node) and the one a wrapper's exit ladder has to see. But NEITHER half may be
//! swallowed when the other fails: this verb's whole reason for existing is that "is this node
//! halted" must not be a question you can ask and get silence to.
//!
//! So both halves are rendered as an answer-or-a-reason ([`Answer`]), and each failure is reported
//! on stderr as well:
//!
//! * the MODE read faulted after the registry answered — the registry still prints, and the mode
//!   cell (or, under `--json`, a `trading_state_error` key) says why it is missing;
//! * the REGISTRY read failed on a node that ANSWERED — a too-old node (`Unsupported`), or one that
//!   refused the verb (`InvalidData`) — and the mode read is attempted ANYWAY and printed, because
//!   `Request::Snapshot` is a base verb of the protocol that every node has always served. That is
//!   [`mode_read_worth_attempting`], which carries the two kinds it deliberately does NOT retry
//!   under. The exit is still the registry's, so a wrapper cannot read a partial answer as a whole
//!   one.
//!
//! ⚠ Until 2026-09-10 a registry failure returned before the mode read ran, so `trade status`
//! against a node too old for `strategy-verbs` printed no trading mode AT ALL — the operator asked
//! a halted node what it was doing and was told only to upgrade it.
//!
//! # Read-only, so the OBSERVE key suffices
//!
//! Both wire verbs answer under either scope because they are read-only
//! (`crates/vike-tradehub/src/server.rs`'s `Request::StrategyStatus` and `Request::Snapshot` arms),
//! and both client functions always connect under `Scope::Read` — so this command needs exactly
//! one credential, this box's device key, resolved the same way `trade`/`mcp` resolve theirs: by the
//! dispatcher, from this box's `device_key` row ([`vike_client::hubkeys`] argues it). The absent-key
//! message here is the read-specific [`HubKeyring::observe_absent_message`], not the either-half one.
//!
//! # The old-node refusal is CLIENT-side and the message says what to do
//!
//! The registry verb is feature-negotiated ([`vike_tradehub_client::proto::FEATURE_STRATEGY_VERBS`]):
//! against a node that does not advertise it, [`vike_tradehub_client::strategy_status`] refuses
//! CLIENT-SIDE with [`HubCallError::NotOffered`] and sends nothing after the handshake — an older
//! node's serde cannot decode the frame, so sending would produce an opaque decode error at the
//! node instead of a diagnosis here. [`failure_lines`] keeps the client's own sentence (it names the
//! missing capability) and adds the one action that fixes it: upgrade that node's vike-tradehub.
//!
//! # Output
//!
//! Human (default): the trading mode (plus the fault line when the core is in safe state), a daemon
//! identity line naming what the node runs ([`vike_tradehub_client::node_words::running_tiers`]), the daemon's
//! effective-params line, then one aligned row per mount — strategy, mode, PRODUCT and params. The mode cell is the
//! tier the mount's account runs at NOW and which account that is (`render.rs`'s `mount_mode`: `demo (bybit#DEMO)`,
//! `LIVE (bybit#LIVE)`), or `NOT RUNNING (...)`; a row that states no tier reads `?`. Uppercase exactly when it matters.
//!
//! ⚠ `trading state:` is the PRIMARY engine's. When any engine's own block is `Halted`/`Reducing`
//! (a venue-scoped link dead-man halt, decision 0180, or a dead primary venue) [`venue_state_lines`]
//! adds `halted venues: …` / `reducing venues: …` beneath it, and `--json` carries the same names
//! as `halted_venues` / `reducing_venues` arrays. The top line's wording is unchanged.
//!
//! ⚠ When the node holds an account's venue balance back from its engine (`CoreSnapshotWire::balance_withheld`: the
//! engine's equity is then its own book, not the venue's wallet) [`balance_held_lines`] adds one `<route key>:
//! balance held: <reason> - …` line per account beneath those, and `--json` carries the list under
//! `balance_withheld`, only when it is not empty (a frame without the key says nothing, never "none held").
//!
//! ⚠ What stands between the node and the venues gets a line per entry the same way ([`link_lines`]): each account the
//! node does NOT run and why, each venue path that is down, each lost trade link to the gatewayhub and price link to
//! the datahub. `--json` carries the frame's lists under `not_running`, `venue_link_down`, `trade_link_down` and
//! `price_link_down`, each only when it is not empty. The exit stays the registry's.
//!
//! ⚠ The PRODUCT column appears only when the node advertised `FEATURE_MOUNT_CLASS`, and an
//! em dash in it is a FACT rather than a blank: that mount is still TOML-backed and has not named
//! its product, which `docs/decisions/0061`'s Phase 5 calls the migration. Against an older node
//! the column is absent entirely — a column of dashes would claim no mount here names a product,
//! which a client talking to a node that cannot carry the field is in no position to say. `--json`: the
//! [`StrategyStatusWire`] payload serialized verbatim under a `strategy_status` key, beside the two
//! mode fields — the wire shape is NESTED rather than flattened so that it stays the node's own
//! schema and cannot drift from it. The two added keys have no wire struct of their own to be
//! verbatim about.
//!
//! ⚠ **A half that could not be read is a KEY in the JSON, never an absence.** `trading_state_error`
//! / `strategy_status_error` carry the reason, and the same reason goes to stderr. The earlier shape
//! omitted the mode keys silently on the argument that a consumer requiring `trading_state` would
//! then fail loudly — which is true of a strict deserializer and false of `jq .trading_state`, of
//! `serde_json::Value` indexing, and of every operator reading a terminal. A kill-switch read that
//! answers nothing must SAY it answered nothing.
//!
//! ⚠ **A `seq` 0 frame is no reading of the mode** ([`vike_client::first_frame::is_pre_fold`]): it says `Active`, no
//! fault and no halted venue whatever the node's state is, and the node's publisher lists no lost trade link or venue
//! path on it, because it has no engine block to check them against. So this verb prints `pre_fold_line` (stderr too under `--json`) and
//! `trading state: UNKNOWN`, and `--json` carries `pre_fold: true` in place of `trading_state`, `fault`, `halted_venues`
//! and `reducing_venues`. Those four keys are absent on that frame alone, and `pre_fold` is the key that says why. The exit stays the registry's.
//!
//! ⚠ Deliberately no claim here about what any OTHER command prints — see [`json_body`], which
//! carries the gate that harvests exactly that shape.
//!
//! # What is / is not tested
//!
//! The grammar ([`StatusCommand::into_args`]), every renderer ([`mode_lines`], [`registry_lines`], [`human_lines`],
//! [`json_body`]) and BOTH halves of the failure mapping ([`failure_lines`] for the words,
//! [`failure_exit`] for the rung) are PURE and unit-tested below. The connections themselves are
//! `tests/shipped_cli/trade_status_cli.rs`: the shipped binary against a REAL loopback node (mode + identity +
//! mounts, the identity-less server error still carrying the mode, a scripted OLD node proving
//! nothing is sent AND that its mode comes back anyway, a scripted node that answers the registry
//! and refuses the snapshot — the `trading_state_error` half — and the no-key-anywhere message).

// History, kept so a citation by symbol still lands: `USAGE` was this verb's hand-kept usage text;
// `trade status --help` is rendered from its `clap` struct now (decision 0155).

use std::fmt;
use std::process::ExitCode;

use vike_hub_protocol::call::HubCallError;
use vike_model::TradingState;
use vike_tradehub_client::wire::{CoreSnapshotWire, StrategyStatusWire};

use crate::cmd::args;
use crate::exit::Exit;
use vike_client::hubkeys::HubKeyring;

/// The command name every message and every usage error prints. Spelled once: it is two words, and
/// a surface that hand-copies its own name is the one that ends up answering to two of them.
pub(crate) const COMMAND: &str = "trade status";

/// The program name a refusal of this verb's own rule prints under.
const BIN: &str = "vike-cli trade status";

/// `--help`'s prose (the `usage:` line and the options table are `clap`'s).
pub(super) const LONG_ABOUT: &str = "\
Ask a running vike-tradehub what it is doing: the trading mode (active | reducing | halted),
which daemon answered (name, the tiers it runs, build), its resolved effective params, and one row per
mounted strategy. Read-only — it authenticates under the OBSERVE scope and can neither place nor
change anything. `vike-cli trade halt` / `vike-cli trade resume` are the verbs that CHANGE the
mode; there is no spelling of this one that writes.

It signs with this box's device key (a device_key row of its settings database; no environment
variable carries a key), and the node's access list decides what that key may do: `vike-cli access
ls` on the server shows it.";

// The `trade status` command line, as `clap` reads it. The node (`--node`, else `config.node_addr`) is
// judged after the parse ([`StatusCommand::into_args`]) so `--help` never has to supply one.
#[derive(clap::Args, Debug)]
pub(super) struct StatusCommand {
    #[arg(
        long,
        value_name = "HOST:PORT",
        overrides_with = "node",
        help = "the node's observe address [default: config.node_addr]"
    )]
    node: Option<String>,
    #[arg(
        long,
        overrides_with = "json",
        help = "the node's answer as JSON (the wire payload, verbatim, under `strategy_status`)"
    )]
    json: bool,
}

/// The resolved `trade status` command line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    /// The node's observe address: `--node`, else `config.node_addr`. There is no compiled-in default node to ask.
    node: String,
    /// `--json`: print the wire payload verbatim instead of the human table.
    json: bool,
}

impl StatusCommand {
    /// The one rule after the parse: the node, `--node` else `configured` (`config.node_addr`).
    fn into_args(self, configured: Option<&str>) -> Result<Args, String> {
        let node = super::node_or_refusal(self.node, configured)?;
        Ok(Args { node, json: self.json })
    }
}

/// `trade status`'s argv tail through the plane's one parser, then its rule.
#[cfg(test)]
fn parse(argv: impl IntoIterator<Item = String>) -> Result<Args, args::Stop> {
    parse_trade_verb!(argv, "status", Status, BIN, None)
}

/// The MODE half, as lines — the trading state, and the fault line when the core sits in safe
/// state.
///
/// ⚠ It is here rather than in [`crate::cmd::trade`] because THREE surfaces render it and they may
/// not word a kill switch differently: this command, the REPL's `status`, and the REPL's `snapshot`
/// (which prints the mode above the books). Before ruling 17 there were two spellings of it and
/// they already differed — the REPL's carried the staleness stamp, `strategy-status` had no mode at
/// all. PURE.
pub(crate) fn mode_lines(state: TradingState, fault: Option<&str>) -> Vec<String> {
    let mut lines = vec![format!("trading state: {}", state.word())];
    if let Some(fault) = fault {
        lines.push(format!("  FAULT (core halted safe-state): {fault}"));
    }
    lines
}

/// The route keys (venue id when a node names none) of the engines whose OWN block is in `state`.
/// The published frame already carries the newest block of each key.
///
/// ⚠ An account on the frame's `not_running` list is STOPPED (a deactivation, or a start that failed): its
/// engine is gone and refuses every order, but a stopped account that was not started again keeps its last
/// block (the frame's rule, so its position stays valued). That block's `Halted` is history, not a
/// halt: it is not named here.
fn venues_in_state(snap: &CoreSnapshotWire, state: TradingState) -> Vec<String> {
    snap.venues
        .iter()
        .filter(|v| v.trading_state == state)
        .map(vike_tradehub_client::wire::block_key)
        .filter(|key| snap.stopped_row(key).is_none())
        .collect()
}

/// The PER-VENUE half of the mode, as lines: `halted venues: a, b` / `reducing venues: c`, and
/// nothing when every engine is Active.
///
/// ⚠ [`mode_lines`]' `trading state` is the snapshot's top-level field, the PRIMARY engine's. A
/// venue-scoped link dead-man halt (decision 0180) halts one venue while the primary's trades on (it
/// reads `Active` while that venue refuses every opening order), and a dead primary venue reads
/// `Halted` for the whole node. The wire already carries each engine's own state in its
/// `VenueBlockWire`, so these lines say which. They add to the top line, never replace it. PURE.
pub(crate) fn venue_state_lines(snap: &CoreSnapshotWire) -> Vec<String> {
    [TradingState::Halted, TradingState::Reducing]
        .into_iter()
        .filter_map(|state| {
            let names = venues_in_state(snap, state);
            (!names.is_empty()).then(|| format!("{} venues: {}", state.word(), names.join(", ")))
        })
        .collect()
}

/// The accounts whose venue balance the node HOLDS BACK from their engine, as lines (`oanda: balance held:
/// assumed USD - ...`, [`vike_tradehub_client::wire::BalanceWithheldWire::line`]), and nothing when none is.
///
/// ⚠ Nothing here is a statement about a node that predates the field: its frame has no key, which reads as
/// "none reported", never as "none held". The lines add to the mode half, never replace it. PURE.
pub(crate) fn balance_held_lines(snap: &CoreSnapshotWire) -> Vec<String> {
    snap.balance_withheld
        .iter()
        .map(vike_tradehub_client::wire::BalanceWithheldWire::line)
        .collect()
}

/// What stands between this node and the venues, as lines: each account it does NOT run and why, each venue path that
/// is down, each lost link to the other two hubs. Nothing when the frame lists none, and that is never a statement
/// about a node that predates a list (its frame has no key). PURE.
pub(crate) fn link_lines(snap: &CoreSnapshotWire) -> Vec<String> {
    let not_running = snap.not_running.iter().map(|n| {
        let tier = n.tier.as_deref().unwrap_or("not stated");
        format!("{}: NOT RUNNING ({tier}): {}", n.route_key, n.say())
    });
    let venue_paths = snap.venue_link_down.iter().map(|d| format!("{}: {}", d.route_key, d.say()));
    let trade_links = snap
        .trade_link_down
        .iter()
        .map(|l| format!("{}: {}", l.route_key, vike_client::health::trade_link_words(l)));
    let price_links = snap
        .price_link_down
        .iter()
        .map(|l| format!("{}: {}", l.venue, vike_client::health::price_link_words(l)));
    not_running.chain(venue_paths).chain(trade_links).chain(price_links).collect()
}

/// The REGISTRY half, as lines: identity line, effective-params line, then one aligned row per
/// mount. PURE — pinned by the unit tests below and exercised end-to-end by
/// `tests/shipped_cli/trade_status_cli.rs`. Shared with the REPL's `status`, which prints it under the mode
/// lines its own live snapshot already carries.
/// ⚠ `knows_class` is the node's `FEATURE_MOUNT_CLASS` advertisement, NOT a property of the rows.
/// It decides whether the PRODUCT column appears at all, and that is the whole reason it is a
/// parameter rather than something read off the data: `asset_class: None` means "this node cannot
/// carry it" on an old node and "this mount is not migrated yet" on a current one, and a table
/// cannot say which by looking. So an old node gets NO column — a column of dashes would read as
/// "no mount here names a product", which is a claim about the deployment that the client is in no
/// position to make.
///
/// `frame` is the snapshot read beside it, when there is one: what the node runs is its engines', and only without one
/// the mount rows' ([`vike_tradehub_client::node_words::running_tiers`]).
pub(crate) fn registry_lines(
    status: &StrategyStatusWire,
    knows_class: bool,
    frame: Option<&CoreSnapshotWire>,
) -> Vec<String> {
    use super::render::mount_mode;
    let id = &status.identity;
    let runs = vike_tradehub_client::node_words::running_tiers(frame, &status.mounts);
    let mut lines = vec![
        format!("node {} — {runs} — build {}", id.name, id.build),
        format!("params: {}", status.effective_params),
        String::new(),
    ];
    // Column widths from the data (headers included), so a long strategy name cannot shear the
    // params column. `params` is last and left free — it is prose-length.
    let strategy_w =
        status.mounts.iter().map(|m| m.strategy.len()).chain(["STRATEGY".len()]).max().unwrap_or(0);
    let mode_w =
        status.mounts.iter().map(|m| mount_mode(m).len()).chain(["MODE".len()]).max().unwrap_or(0);
    if !knows_class {
        lines.push(format!("  {:<strategy_w$}  {:<mode_w$}  {}", "STRATEGY", "MODE", "PARAMS"));
        for m in &status.mounts {
            lines.push(format!(
                "  {:<strategy_w$}  {:<mode_w$}  {}{}",
                m.strategy,
                mount_mode(m),
                m.shown_params(),
                super::render::latch_note(m.latched, &m.mount_id)
            ));
        }
        return lines;
    }
    // ⚠ An em dash, not an empty cell: a blank reads as a rendering slip, and this absence is a
    // FACT — the mount is still TOML-backed and has not named its product. `docs/decisions/0061`'s
    // Phase 5 calls that the migration, so what the operator is looking at is work left, not a bug.
    let product = |m: &vike_tradehub_client::wire::MountViewWire| {
        m.asset_class.clone().unwrap_or_else(|| "—".to_string())
    };
    let prod_w = status
        .mounts
        .iter()
        .map(|m| product(m).chars().count())
        .chain(["PRODUCT".len()])
        .max()
        .unwrap_or(0);
    lines.push(format!(
        "  {:<strategy_w$}  {:<mode_w$}  {:<prod_w$}  {}",
        "STRATEGY", "MODE", "PRODUCT", "PARAMS"
    ));
    for m in &status.mounts {
        lines.push(format!(
            "  {:<strategy_w$}  {:<mode_w$}  {:<prod_w$}  {}{}",
            m.strategy,
            mount_mode(m),
            product(m),
            m.shown_params(),
            super::render::latch_note(m.latched, &m.mount_id)
        ));
    }
    lines
}

/// What ONE `trade status` run actually got back: each half is either the node's answer or the
/// reason there isn't one.
///
/// ⚠ Both halves are a `Result` rather than an `Option` deliberately. An `Option` loses the reason,
/// and every renderer below then has the choice of printing nothing — which is the exact failure
/// this verb exists to remove. Carrying the error into the renderer makes "say why" the only shape
/// available.
struct Answer<'a> {
    /// The MODE half — [`vike_tradehub_client::snapshot_once`] (`Request::Snapshot`), or why there
    /// is none (its error, or why it was not attempted).
    snap: Result<&'a CoreSnapshotWire, &'a dyn fmt::Display>,
    /// The REGISTRY half — [`vike_tradehub_client::strategy_status`] (`Request::StrategyStatus`).
    /// Its `Err` is what decides the exit code; see [`failure_exit`].
    status: Result<&'a StrategyStatusWire, &'a HubCallError>,
    /// Whether the node advertised `vike_tradehub_client::proto::FEATURE_MOUNT_CLASS`.
    ///
    /// ⚠ A property of the NODE, carried beside the rows rather than read off them, because
    /// `MountViewWire::asset_class: None` means two different things and only this tells them apart:
    /// a node too old to carry the field, or a current node whose mount is not migrated yet. It
    /// rides in [`Answer`] so [`human_lines`] stays PURE — the capability is DATA here, not a call.
    knows_class: bool,
}

/// The whole human rendering: the mode half, then the registry half — each replaced by a line
/// saying why it is missing when that half failed, never by nothing. PURE.
fn human_lines(answer: &Answer<'_>) -> Vec<String> {
    let mut lines = match answer.snap {
        Ok(s) => {
            let mut lines = match super::pre_fold_line(s) {
                Some(mark) => vec![
                    mark.to_string(),
                    "trading state: UNKNOWN — the node has built nothing yet".to_string(),
                ],
                None => mode_lines(s.trading_state, s.fault.as_deref()),
            };
            lines.extend(venue_state_lines(s));
            lines.extend(balance_held_lines(s));
            lines.extend(link_lines(s));
            lines
        }
        Err(e) => vec![format!("trading state: UNKNOWN — the node did not answer a snapshot: {e}")],
    };
    match answer.status {
        Ok(status) => lines.extend(registry_lines(status, answer.knows_class, answer.snap.ok())),
        // One line, not the full [`failure_lines`] pair: the ACTION (upgrade this node, fix this
        // key) belongs on stderr with the other diagnostics, and what stdout owes the operator here
        // is an account of the half of the answer they did not get.
        Err(e) => lines
            .push(format!("mounted strategies: UNAVAILABLE — the node did not answer them: {e}")),
    }
    lines
}

/// The stderr diagnostic for a MODE read that failed — the twin of [`failure_lines`], which does the
/// same job for the registry half.
///
/// ⚠ It exists because the human rendering's "trading state: UNKNOWN" cell is on STDOUT, and under
/// `--json` stdout is a document a script parses rather than a transcript a human reads. A failed
/// read that shows up in neither stream is the silence this whole ruling is about, so the reason
/// goes to stderr in BOTH modes and the stdout cell stays as well: two streams, two audiences.
fn mode_failure_line(node: &str, err: &HubCallError) -> String {
    format!("vike-cli {COMMAND}: the node at {node} did not answer a snapshot: {err}")
}

/// The `--json` rendering: the [`StrategyStatusWire`] payload serialized VERBATIM under
/// `strategy_status`, beside the mode fields.
///
/// ⚠ The nesting is the point. This crate's machine-output convention — whose precedent is the
/// `backtest` verb's own json flag — is that a machine gets the WIRE schema, not a second
/// hand-maintained shape that could drift from it. Flattening the two into one object would create
/// exactly that second shape; nesting keeps `identity` / `effective_params` / `mounts` byte-for-byte
/// what the node sent, and the two fields beside it are the mode half, which has no wire struct of
/// its own.
///
/// ⚠ **A half that failed is an `*_error` KEY, not an omission**, and the earlier shape was the
/// other way round: the mode keys were dropped in silence, on the argument that a consumer
/// requiring `trading_state` would then fail loudly. That argument holds only for a strict typed
/// deserializer. `jq .trading_state` answers `null` for an absent key exactly as it does for a null
/// one, `serde_json::Value` indexing returns `Value::Null` for both, and a human sees a document
/// with one fewer line — so for three of the four consumers this surface actually has, an omission
/// was indistinguishable from "there is no mode", which is the reading that must never be
/// available. `trading_state`/`fault` are still absent when unknown (nothing invents a value), and
/// `trading_state_error` is what says so out loud; `strategy_status_error` is its twin for the
/// registry half.
///
/// ⚠ Deliberately no claim here about what any OTHER command prints:
/// `crates/vike-ops/tests/docs/unrun_command_gate/rules.rs`'s `every_harvested_claim_is_declared` harvests
/// exactly that shape and demands a CHECKED or UNVERIFIABLE row for it — a doc sentence is not
/// the place to assert a sibling verb's output.
fn json_body(answer: &Answer<'_>) -> String {
    let mut body = serde_json::Map::new();
    // Additive, the identity line's word: `strategy_status.identity.live` stays the wire's `flags.tradehub_live`.
    let mounts = answer.status.map(|status| status.mounts.as_slice()).unwrap_or_default();
    body.insert(
        "running_tiers".to_string(),
        serde_json::Value::String(vike_tradehub_client::node_words::running_tiers(
            answer.snap.ok(),
            mounts,
        )),
    );
    match answer.snap {
        Ok(s) => {
            if super::is_pre_fold(s) {
                body.insert("pre_fold".to_string(), serde_json::Value::Bool(true));
            } else {
                body.insert(
                    "trading_state".to_string(),
                    serde_json::to_value(s.trading_state)
                        .expect("TradingState is a plain unit enum"),
                );
                body.insert(
                    "fault".to_string(),
                    match &s.fault {
                        Some(f) => serde_json::Value::String(f.clone()),
                        None => serde_json::Value::Null,
                    },
                );
                // Additive: the engines whose OWN block is not Active (`trading_state` is the primary's).
                for (key, state) in [
                    ("halted_venues", TradingState::Halted),
                    ("reducing_venues", TradingState::Reducing),
                ] {
                    body.insert(key.to_string(), serde_json::json!(venues_in_state(s, state)));
                }
            }
            // Additive and ONLY when something is held (the wire's own rule): an empty list from a node that
            // predates the field would claim "none held", which it cannot know.
            if !s.balance_withheld.is_empty() {
                body.insert(
                    "balance_withheld".to_string(),
                    serde_json::to_value(&s.balance_withheld)
                        .expect("BalanceWithheldWire is two strings; serialization is total"),
                );
            }
            // The same rule for what stands between the node and the venues: each list as the frame carries it.
            for (key, list) in [
                ("not_running", serde_json::to_value(&s.not_running)),
                ("venue_link_down", serde_json::to_value(&s.venue_link_down)),
                ("trade_link_down", serde_json::to_value(&s.trade_link_down)),
                ("price_link_down", serde_json::to_value(&s.price_link_down)),
            ] {
                let list = list.expect("a list of strings and numbers; serialization is total");
                if list.as_array().is_some_and(|rows| !rows.is_empty()) {
                    body.insert(key.to_string(), list);
                }
            }
        }
        Err(e) => {
            body.insert(
                "trading_state_error".to_string(),
                serde_json::Value::String(e.to_string()),
            );
        }
    }
    match answer.status {
        Ok(status) => {
            body.insert(
                "strategy_status".to_string(),
                serde_json::to_value(status).expect(
                    "StrategyStatusWire is a plain struct-and-strings tree; serialization is total",
                ),
            );
        }
        Err(e) => {
            body.insert(
                "strategy_status_error".to_string(),
                serde_json::Value::String(e.to_string()),
            );
        }
    }
    serde_json::to_string_pretty(&serde_json::Value::Object(body))
        .expect("a map of already-serialized values re-serializes")
}

/// Map the registry call's failure to the lines this command prints on stderr — PURE, so the
/// old-node refusal's wording is pinned by a unit test rather than needing a downgraded daemon.
/// The sort and the words are `vike_client::hubkeys::node_read_failure_lines`, the one both doors
/// share (`trade venue`'s `failure_lines` reaches it too, after answering the node's own words).
pub(crate) fn failure_lines(node: &str, err: &HubCallError) -> Vec<String> {
    vike_client::hubkeys::node_read_failure_lines(
        &format!("vike-cli {COMMAND}"),
        node,
        "the strategy verbs",
        err,
    )
}

/// The RUNG the same failure exits on — the twin of [`failure_lines`], split out so the words and
/// the number are decided beside each other, over the same error.
///
/// The rule is **THE NODE ANSWERED AND REFUSED** (`HubCallError::failure_outcome`'s `Refused`, the
/// one reading every hub client's caller shares, through `crate::exit::CliError::hub_call`): it stays
/// on the pre-existing rung, and everything else — a refused, unroutable or timed-out socket, an
/// answer lost after the request left, and a node still STARTING, which answers NOT NOW — is the
/// [`Exit::Connect`] a wrapper should back off and retry. A too-old node (`NotOffered`), a key that
/// does not verify (`AccessDenied`), a version skew (`Protocol`) and a node-side refusal (`Refused`)
/// are configuration facts about a REACHABLE box, and retrying any of them forever is exactly the
/// behaviour the ladder exists to prevent.
///
/// ⚠ `Refused` is the arm that is easy to miss, and it is not hypothetical.
/// `crates/vike-tradehub-client/src/tradehub_handle.rs`'s `strategy_status` returns it for a
/// `Response::Error` — AFTER the connection opened and the observe handshake passed — and
/// `crates/vike-tradehub/src/server.rs`'s `Request::StrategyStatus` arm reaches it whenever the node
/// publishes no identity block. Left in the catch-all, a correctly-reachable node with a permanent
/// configuration fact answered `3`, and a retry wrapper would loop on it forever.
///
/// `pub(crate)` (rather than private, like [`mode_failure_line`]) because `crate::cmd::trade::
/// strategy`'s `ls` calls the SAME `vike_tradehub_client::strategy_status_with_features` this
/// command's registry half does and must classify its failure onto the SAME rung — a second copy of
/// this match is a second copy to get wrong, exactly the reasoning `vike_client::hubkeys::
/// observe_permission_denied_lines`'s own doc gives for its own extraction. ⚠ This named the plain
/// `strategy_status` wrapper until Task 6's fix round 1 moved `strategy ls` onto
/// `_with_features` (to get the capability list `mounts_table`/`mounts_json`'s `knows_class` needs)
/// without sweeping this sentence — the third instance on this branch of exactly that miss.
pub(crate) fn failure_exit(err: &HubCallError) -> Exit {
    crate::exit::CliError::hub_call(err, String::new()).exit
}

/// After the REGISTRY read failed with `err`, is the MODE read still worth making?
///
/// The mode is the safety-critical half — "is this node halted" — and `Request::Snapshot` is a BASE
/// verb of the protocol rather than a negotiated capability, so a node that cannot answer the
/// registry can very often still answer this. The variants that say YES:
///
/// * [`HubCallError::NotOffered`] — the handshake PASSED and the refusal was ours, client-side,
///   because the node advertised no `strategy-verbs`. That is precisely a too-old node: reachable,
///   authenticated, and serving `Request::Snapshot` as it always has. This is the case the whole
///   degrade exists for. (A device key against a node without `person-auth` is `NotOffered` too, and
///   spends one futile dial: harmless.)
/// * [`HubCallError::Refused`], [`HubCallError::NotNow`] and [`HubCallError::Protocol`] — the node
///   ANSWERED: refused the verb (a publisher with no identity block answers exactly that), said not
///   yet, or spoke another protocol. The socket is good, and its answer to the mode is worth a line.
///
/// …and the ones that say NO, because a second attempt buys nothing and costs the operator time at
/// the worst possible moment: [`HubCallError::AccessDenied`] is the same key failing the same
/// handshake a second time, and the rest is a socket that never opened or went silent — retrying it
/// doubles the wait before anyone is told the node is unreachable.
fn mode_read_worth_attempting(err: &HubCallError) -> bool {
    matches!(
        err,
        HubCallError::NotOffered { .. }
            | HubCallError::Refused { .. }
            | HubCallError::NotNow { .. }
            | HubCallError::Protocol(_)
    )
}

/// Entry point [`crate::cmd::trade::run`] routes `trade status` to, with the flags `clap` read;
/// `keys` is the [`HubKeyring`] the dispatcher resolved (this box's device key) — this READ verb uses
/// only its observe half.
pub(super) fn run(cmd: StatusCommand, keys: &HubKeyring) -> ExitCode {
    let parsed = match cmd.into_args(keys.node_addr()) {
        Ok(a) => a,
        Err(msg) => return args::refuse(BIN, msg),
    };
    // The observe key SPECIFICALLY: a control key cannot authenticate a read (the node verifies
    // each scope against its own key), so `has_any()` would be the wrong question here.
    let Some((key, _origin)) = keys.observe() else {
        eprintln!("{}", keys.observe_absent_message(COMMAND, &parsed.node));
        return ExitCode::FAILURE;
    };
    // The REGISTRY read decides the EXIT — see this module's doc. It does not decide whether
    // anything is printed: its failure lands on stderr with the action that fixes it, and the mode
    // read below still runs whenever the node is one that could answer it.
    let status_and_features =
        vike_tradehub_client::strategy_status_with_features(parsed.node.as_str(), key);
    let knows_class = status_and_features.as_ref().is_ok_and(|(_, f)| {
        f.iter().any(|s| s == vike_tradehub_client::proto::FEATURE_MOUNT_CLASS)
    });
    let status = status_and_features.map(|(s, _)| s);
    if let Err(e) = &status {
        for line in failure_lines(&parsed.node, e) {
            eprintln!("{line}");
        }
    }
    // …and the MODE read is best-effort on top of it, including after a registry failure.
    let snap = match &status {
        Ok(_) => Some(vike_tradehub_client::snapshot_once(parsed.node.as_str(), key)),
        Err(e) if mode_read_worth_attempting(e) => {
            Some(vike_tradehub_client::snapshot_once(parsed.node.as_str(), key))
        }
        // A refused key or a socket that never opened: the same call would fail the same way, and
        // the operator is already looking at the reason. Report the mode as unreadable for that
        // reason rather than making them wait for a second identical failure.
        Err(_) => None,
    };
    let unattempted =
        "not attempted — the registry read had already failed on this node (see above)";
    let snap_ref: Result<&CoreSnapshotWire, &dyn fmt::Display> = match &snap {
        Some(Ok(s)) => Ok(s),
        Some(Err(e)) => Err(e),
        None => Err(&unattempted),
    };
    // Only a read that was MADE and failed earns its own stderr line; one that was never attempted
    // has its reason on stderr already, immediately above, and repeating it would bury it.
    if let Some(Err(e)) = &snap {
        eprintln!("{}", mode_failure_line(&parsed.node, e));
    }
    let answer = Answer { snap: snap_ref, status: status.as_ref(), knows_class };
    if parsed.json {
        // The human output carries this line; a `--json` stdout stays one document (`pre_fold` says it there).
        if let Some(mark) = snap_ref.ok().and_then(super::pre_fold_line) {
            eprintln!("{mark}");
        }
        println!("{}", json_body(&answer));
    } else {
        for line in human_lines(&answer) {
            println!("{line}");
        }
    }
    match &status {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => failure_exit(e).into(),
    }
}

#[path = "status_tests.rs"]
#[cfg(test)]
mod status_tests;
