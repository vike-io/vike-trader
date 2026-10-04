//! `vike-cli trade status` — ask a running vike-tradehub node WHAT IT IS DOING: the trading MODE
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
//! (`Request::Snapshot`). Both are per-call short-lived [`vike_tradehub_client::proto::Scope::Read`]
//! connections; neither carries the other's field, and inventing a merged wire verb would be a
//! protocol change (a `NODE_PROTO_VERSION` bump breaks the handshake against every running node —
//! `crates/vike-tradehub-client/src/proto.rs`'s `FEATURE_STRATEGY_VERBS` argues that at length) to
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
//! one credential, the observe key, resolved the same way `trade`/`mcp` resolve theirs: by the
//! dispatcher, process environment first and then the node-key store the daemon itself reads
//! ([`crate::cmd::nodekeys`] argues the precedence). A resolved CONTROL key cannot substitute —
//! the node verifies each scope against its own key — which is why the absent-key message here is
//! the observe-specific [`NodeKeyring::observe_absent_message`], not the either-key one.
//!
//! # The old-node refusal is CLIENT-side and the message says what to do
//!
//! The registry verb is feature-negotiated ([`vike_tradehub_client::proto::FEATURE_STRATEGY_VERBS`]):
//! against a node that does not advertise it, [`vike_tradehub_client::strategy_status`] refuses
//! CLIENT-SIDE with [`io::ErrorKind::Unsupported`] and sends nothing after the handshake — an older
//! node's serde cannot decode the frame, so sending would produce an opaque decode error at the
//! node instead of a diagnosis here. [`failure_lines`] keeps the client's own sentence (it names the
//! missing capability) and adds the one action that fixes it: upgrade that node's vike-tradehub.
//!
//! # Output
//!
//! Human (default): the trading mode (plus the fault line when the core is in safe state), a daemon
//! identity line, the daemon's effective-params line, then one aligned row per mount — strategy,
//! mode, PRODUCT and params. The mode cell is `LIVE`/`paper`, uppercase exactly when it matters.
//!
//! ⚠ The PRODUCT column appears only when the node advertised `FEATURE_MOUNT_CLASS`, and an
//! em dash in it is a FACT rather than a blank: that mount is still TOML-backed and has not named
//! its product, which `docs/decisions/0061`'s Phase 5 calls the migration. Against an older node
//! the column is absent entirely — a column of dashes would claim no mount here names a product,
//! which a client talking to a node that cannot carry the field is in no position to say. `--json`: the
//! [`WireStrategyStatus`] payload serialized verbatim under a `strategy_status` key, beside the two
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
//! ⚠ Deliberately no claim here about what any OTHER command prints — see [`json_body`], which
//! carries the gate that harvests exactly that shape.
//!
//! # What is / is not tested
//!
//! The grammar ([`parse`]), every renderer ([`mode_lines`], [`registry_lines`], [`human_lines`],
//! [`json_body`]) and BOTH halves of the failure mapping ([`failure_lines`] for the words,
//! [`failure_exit`] for the rung) are PURE and unit-tested below. The connections themselves are
//! `tests/trade_status_cli.rs`: the shipped binary against a REAL loopback node (mode + identity +
//! mounts, the identity-less server error still carrying the mode, a scripted OLD node proving
//! nothing is sent AND that its mode comes back anyway, a scripted node that answers the registry
//! and refuses the snapshot — the `trading_state_error` half — and the no-key-anywhere message).

use std::io;
use std::process::ExitCode;

use vike_tradehub_client::wire::{WireSnapshot, WireStrategyStatus, WireTradingState};

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::cmd::nodekeys::NodeKeyring;
use crate::exit::Exit;

/// The command name every message and every usage error prints. Spelled once: it is two words, and
/// a surface that hand-copies its own name is the one that ends up answering to two of them.
pub(crate) const COMMAND: &str = "trade status";

const USAGE: &str = "\
usage: vike-cli trade status --node <host:port> [--json]

Ask a running vike-tradehub node what it is doing: the trading mode (Active | Reducing | Halted),
which daemon answered (name, live/paper, build), its resolved effective params, and one row per
mounted strategy. Read-only — it authenticates under the OBSERVE scope and can neither place nor
change anything. `vike-cli trade halt` / `vike-cli trade resume` are the verbs that CHANGE the
mode; there is no spelling of this one that writes.

The observe key is resolved like every node key: VIKE_TRADEHUB_OBSERVE_KEY in the process
environment first, then <project>/settings/node.env — the same store the daemon reads.

options:
  --node HOST:PORT  the node's observe address (required)
  --json            the node's answer as JSON (the wire payload, verbatim, under `strategy_status`)
  -h, --help        this message";

/// The parsed `trade status` command line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    /// `--node`: the node's observe address. Required — there is no default node to ask.
    node: String,
    /// `--json`: print the wire payload verbatim instead of the human table.
    json: bool,
}

/// Parse everything after the `trade status` subcommand. Accepts `--flag value` and `--flag=value`
/// via the shared [`crate::cmd::args`] glue; `-h`/`--help` short-circuits through
/// [`help_requested`] so [`exit_for_parse_error`] turns it into a SUCCESS.
fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
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
    Ok(Args { node, json })
}

/// The mode cell: uppercase exactly when it matters. A live daemon (or mount) should be
/// unmissable in a table an operator skims; paper is the quiet default.
fn mode(live: bool) -> &'static str {
    if live { "LIVE" } else { "paper" }
}

/// The MODE half, as lines — the trading state, and the fault line when the core sits in safe
/// state.
///
/// ⚠ It is here rather than in [`crate::cmd::trade`] because THREE surfaces render it and they may
/// not word a kill switch differently: this command, the REPL's `status`, and the REPL's `snapshot`
/// (which prints the mode above the books). Before ruling 17 there were two spellings of it and
/// they already differed — the REPL's carried the staleness stamp, `strategy-status` had no mode at
/// all. PURE.
pub(crate) fn mode_lines(state: WireTradingState, fault: Option<&str>) -> Vec<String> {
    let mut lines = vec![format!("trading state: {state:?}")];
    if let Some(fault) = fault {
        lines.push(format!("  FAULT (core halted safe-state): {fault}"));
    }
    lines
}

/// The REGISTRY half, as lines: identity line, effective-params line, then one aligned row per
/// mount. PURE — pinned by the unit tests below and exercised end-to-end by
/// `tests/trade_status_cli.rs`. Shared with the REPL's `status`, which prints it under the mode
/// lines its own live snapshot already carries.
/// ⚠ `knows_class` is the node's `FEATURE_MOUNT_CLASS` advertisement, NOT a property of the rows.
/// It decides whether the PRODUCT column appears at all, and that is the whole reason it is a
/// parameter rather than something read off the data: `asset_class: None` means "this node cannot
/// carry it" on an old node and "this mount is not migrated yet" on a current one, and a table
/// cannot say which by looking. So an old node gets NO column — a column of dashes would read as
/// "no mount here names a product", which is a claim about the deployment that the client is in no
/// position to make.
pub(crate) fn registry_lines(status: &WireStrategyStatus, knows_class: bool) -> Vec<String> {
    let id = &status.identity;
    let mut lines = vec![
        format!("node {} — {} — build {}", id.name, mode(id.live), id.build),
        format!("params: {}", status.effective_params),
        String::new(),
    ];
    // Column widths from the data (headers included), so a long strategy name cannot shear the
    // params column. `params` is last and left free — it is prose-length.
    let strat_w =
        status.mounts.iter().map(|m| m.strategy.len()).chain(["STRATEGY".len()]).max().unwrap_or(0);
    let mode_w =
        status.mounts.iter().map(|m| mode(m.live).len()).chain(["MODE".len()]).max().unwrap_or(0);
    if !knows_class {
        lines.push(format!("  {:<strat_w$}  {:<mode_w$}  {}", "STRATEGY", "MODE", "PARAMS"));
        for m in &status.mounts {
            lines.push(format!(
                "  {:<strat_w$}  {:<mode_w$}  {}",
                m.strategy,
                mode(m.live),
                m.params
            ));
        }
        return lines;
    }
    // ⚠ An em dash, not an empty cell: a blank reads as a rendering slip, and this absence is a
    // FACT — the mount is still TOML-backed and has not named its product. `docs/decisions/0061`'s
    // Phase 5 calls that the migration, so what the operator is looking at is work left, not a bug.
    let product = |m: &vike_tradehub_client::wire::WireMountRow| {
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
        "  {:<strat_w$}  {:<mode_w$}  {:<prod_w$}  {}",
        "STRATEGY", "MODE", "PRODUCT", "PARAMS"
    ));
    for m in &status.mounts {
        lines.push(format!(
            "  {:<strat_w$}  {:<mode_w$}  {:<prod_w$}  {}",
            m.strategy,
            mode(m.live),
            product(m),
            m.params
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
    /// The MODE half — [`vike_tradehub_client::snapshot_once`] (`Request::Snapshot`).
    snap: Result<&'a WireSnapshot, &'a io::Error>,
    /// The REGISTRY half — [`vike_tradehub_client::strategy_status`] (`Request::StrategyStatus`).
    /// Its `Err` is what decides the exit code; see [`failure_exit`].
    status: Result<&'a WireStrategyStatus, &'a io::Error>,
    /// Whether the node advertised `vike_tradehub_client::proto::FEATURE_MOUNT_CLASS`.
    ///
    /// ⚠ A property of the NODE, carried beside the rows rather than read off them, because
    /// `WireMountRow::asset_class: None` means two different things and only this tells them apart:
    /// a node too old to carry the field, or a current node whose mount is not migrated yet. It
    /// rides in [`Answer`] so [`human_lines`] stays PURE — the capability is DATA here, not a call.
    knows_class: bool,
}

/// The whole human rendering: the mode half, then the registry half — each replaced by a line
/// saying why it is missing when that half failed, never by nothing. PURE.
fn human_lines(answer: &Answer<'_>) -> Vec<String> {
    let mut lines = match answer.snap {
        Ok(s) => mode_lines(s.trading_state, s.fault.as_deref()),
        Err(e) => vec![format!("trading state: UNKNOWN — the node did not answer a snapshot: {e}")],
    };
    match answer.status {
        Ok(status) => lines.extend(registry_lines(status, answer.knows_class)),
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
fn mode_failure_line(node: &str, err: &io::Error) -> String {
    format!("vike-cli {COMMAND}: the node at {node} did not answer a snapshot: {err}")
}

/// The `--json` rendering: the [`WireStrategyStatus`] payload serialized VERBATIM under
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
/// `crates/vike-ops/tests/unrun_command_gate.rs`'s `every_harvested_claim_is_declared` harvests
/// exactly that shape and demands a CHECKED or UNVERIFIABLE row for it — a doc sentence is not
/// the place to assert a sibling verb's output.
fn json_body(answer: &Answer<'_>) -> String {
    let mut body = serde_json::Map::new();
    match answer.snap {
        Ok(s) => {
            body.insert(
                "trading_state".to_string(),
                serde_json::to_value(s.trading_state)
                    .expect("WireTradingState is a plain unit enum"),
            );
            body.insert(
                "fault".to_string(),
                match &s.fault {
                    Some(f) => serde_json::Value::String(f.clone()),
                    None => serde_json::Value::Null,
                },
            );
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
                    "WireStrategyStatus is a plain struct-and-strings tree; serialization is total",
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
///
/// * [`io::ErrorKind::Unsupported`] — the FEATURE-NEGOTIATION refusal: the node's `Welcome`
///   advertised no `strategy-verbs` capability, so the client sent nothing after the handshake.
///   The client's own message names the capability and says nothing was sent; the added line
///   carries the one action that fixes it.
/// * [`io::ErrorKind::PermissionDenied`] — the handshake itself was refused: the presented
///   observe key did not verify. Points at the key, not the verb.
/// * anything else — an honest transport-shaped report naming the address.
pub(crate) fn failure_lines(node: &str, err: &io::Error) -> Vec<String> {
    match err.kind() {
        io::ErrorKind::Unsupported => vec![
            format!("vike-cli {COMMAND}: {err}"),
            format!(
                "upgrade the node at {node} to a vike-tradehub build that serves the strategy \
                 verbs, then re-run"
            ),
        ],
        // Shared with `crate::cmd::trade::order`'s `ls` — see
        // `crate::cmd::nodekeys::observe_permission_denied_lines`'s doc for why this is one
        // function rather than a copy per verb.
        io::ErrorKind::PermissionDenied => {
            crate::cmd::nodekeys::observe_permission_denied_lines(COMMAND, node, err)
        }
        _ => vec![format!("vike-cli {COMMAND}: cannot query {node}: {err}")],
    }
}

/// The RUNG the same failure exits on — the twin of [`failure_lines`], split out so the words and
/// the number are decided over the same `ErrorKind` match and cannot come to disagree about which
/// failure is which.
///
/// The rule is **THE NODE ANSWERED**: every kind that can only be produced after a reply came back
/// stays on the pre-existing rung, and the catch-all — a refused, unroutable or timed-out socket —
/// is the [`Exit::Connect`] a wrapper should back off and retry. A too-old node (`Unsupported`), a
/// key that does not verify (`PermissionDenied`) and a node-side refusal (`InvalidData`) are
/// configuration facts about a REACHABLE box, and retrying any of them forever is exactly the
/// behaviour the ladder exists to prevent.
///
/// ⚠ `InvalidData` is the arm that is easy to miss, and it is not hypothetical.
/// `crates/vike-tradehub-client/src/remote_handle.rs`'s `strategy_status` returns it for a
/// `Response::Error` and for an unexpected response kind — both AFTER the connection opened and
/// the observe handshake passed — and `crates/vike-tradehub/src/server.rs`'s
/// `Request::StrategyStatus` arm reaches the first of those whenever the node publishes no identity
/// block. Left in the catch-all, a correctly-reachable node with a permanent configuration fact
/// answered `3`, and a retry wrapper would loop on it forever.
///
/// `pub(crate)` (rather than private, like [`mode_failure_line`]) because `crate::cmd::trade::
/// strategy`'s `ls` calls the SAME `vike_tradehub_client::strategy_status_with_features` this
/// command's registry half does and must classify its failure onto the SAME rung — a second copy of
/// this match is a second copy to get wrong, exactly the reasoning `crate::cmd::nodekeys::
/// observe_permission_denied_lines`'s own doc gives for its own extraction. ⚠ This named the plain
/// `strategy_status` wrapper until Task 6's fix round 1 moved `strategy ls` onto
/// `_with_features` (to get the capability list `mounts_table`/`mounts_json`'s `knows_class` needs)
/// without sweeping this sentence — the third instance on this branch of exactly that miss.
pub(crate) fn failure_exit(err: &io::Error) -> Exit {
    match err.kind() {
        io::ErrorKind::Unsupported
        | io::ErrorKind::PermissionDenied
        | io::ErrorKind::InvalidData => Exit::Failed,
        _ => Exit::Connect,
    }
}

/// After the REGISTRY read failed with `err`, is the MODE read still worth making?
///
/// The mode is the safety-critical half — "is this node halted" — and `Request::Snapshot` is a BASE
/// verb of the protocol rather than a negotiated capability, so a node that cannot answer the
/// registry can very often still answer this. The two kinds that say YES:
///
/// * [`io::ErrorKind::Unsupported`] — the handshake PASSED and the refusal was ours, client-side,
///   because the node advertised no `strategy-verbs`. That is precisely a too-old node: reachable,
///   authenticated, and serving `Request::Snapshot` as it always has. This is the case the whole
///   degrade exists for.
/// * [`io::ErrorKind::InvalidData`] — the node ANSWERED and refused the verb (a publisher with no
///   identity block answers exactly that). The socket and the key are both good.
///
/// …and the two that say NO, because a second attempt buys nothing and costs the operator time at
/// the worst possible moment: [`io::ErrorKind::PermissionDenied`] is the same key failing the same
/// handshake a second time, and the catch-all is a socket that never opened — retrying it doubles
/// the wait before anyone is told the node is unreachable.
fn mode_read_worth_attempting(err: &io::Error) -> bool {
    matches!(err.kind(), io::ErrorKind::Unsupported | io::ErrorKind::InvalidData)
}

/// Entry point [`crate::cmd::trade::run`] routes `trade status` to. `args` is everything AFTER the
/// two words; `keys` is the [`NodeKeyring`] the dispatcher resolved (process environment first,
/// then the node-key store the daemon itself reads) — this READ verb uses only its observe half.
pub fn run(args: impl Iterator<Item = String>, keys: &NodeKeyring) -> ExitCode {
    let parsed = match parse(args) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error(COMMAND, USAGE, &msg),
    };
    // The observe key SPECIFICALLY: a control key cannot authenticate a read (the node verifies
    // each scope against its own key), so `has_any()` would be the wrong question here.
    let Some((key, _origin)) = keys.observe() else {
        eprintln!("{}", keys.observe_absent_message(COMMAND));
        return ExitCode::FAILURE;
    };
    // The REGISTRY read decides the EXIT — see this module's doc. It does not decide whether
    // anything is printed: its failure lands on stderr with the action that fixes it, and the mode
    // read below still runs whenever the node is one that could answer it.
    let status_and_features =
        vike_tradehub_client::strategy_status_with_features(parsed.node.as_str(), key.as_bytes());
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
        Ok(_) => Some(vike_tradehub_client::snapshot_once(parsed.node.as_str(), key.as_bytes())),
        Err(e) if mode_read_worth_attempting(e) => {
            Some(vike_tradehub_client::snapshot_once(parsed.node.as_str(), key.as_bytes()))
        }
        // A refused key or a socket that never opened: the same call would fail the same way, and
        // the operator is already looking at the reason. Report the mode as unreadable for that
        // reason rather than making them wait for a second identical failure.
        Err(_) => None,
    };
    let unattempted = io::Error::other(
        "not attempted — the registry read had already failed on this node (see above)",
    );
    let snap_ref = match &snap {
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
