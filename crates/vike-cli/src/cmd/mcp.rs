//! `vike-cli mcp` — a LOCAL stdio MCP server exposing the vike agent surface as tools, so Claude (or
//! any MCP client) can run the full loop: CREATE + BACKTEST (author a Rhai strategy, discover its
//! knobs, see the host-bound indicator set, run a backtest) AND — against a running vike-tradehub
//! node — read live state and place orders through a mandatory PREVIEW gate. Part of the "vike-cli
//! as the one agent surface" program
//! (`docs/superpowers/specs/2026-07-26-vike-cli-agent-surface-program.md`).
//!
//! # Transport (hand-rolled, no SDK)
//!
//! MCP's stdio transport is JSON-RPC 2.0 framed as NEWLINE-delimited JSON (one message per line, no
//! embedded newlines). We hand-roll it with `serde_json` — the SAME "no async, no new transport
//! stack, stdio-JSON" convention the dukascopy sidecar, vike-datahub, and vike-tradehub use (a full
//! MCP SDK would drag in tokio + a second async stack the workspace rejects). Requests carry an `id`
//! and get exactly one response; notifications (no `id`) get none.
//!
//! # Tools
//!
//! READ-ONLY / safe (`readOnlyHint`): `validate_strategy`, `discover_params`, `list_templates`,
//! `list_indicators` (all OFFLINE — pure vike-script, no server), `run_backtest`, `run_sweep`,
//! `run_walk_forward` (remote datahub runs — a backtest MUTATES nothing), `list_strategies`,
//! `list_series` (remote datahub metadata), `node_snapshot`. This is the ABSORBED `vike-mcp` tool
//! surface (Phase A of retiring that crate): the run/list tools go over the datahub RPC verbs
//! instead of a local DataFusion store, so this CLI stays DataFusion-free; `validate_strategy` /
//! `list_templates` are offline ports (note the argument is named `script` here — vike-mcp said
//! `code` — matching this file's existing `discover_params`).
//!
//! LIVE ORDER-WRITE (`destructiveHint`), gated: `submit_order`, `cancel_order`, `modify`,
//! `flatten`, `market_exit`, `set_trading_state`, `mass_cancel` — the FULL `trade`-REPL write-verb
//! roster, resolved through the SAME [`crate::cmd::verbs`] construction site + guardrail the REPL
//! uses (one vocabulary, two surfaces). **Mandatory preview:** a write tool called WITHOUT
//! `confirm: true` EXECUTES NOTHING — it returns the resolved `WireCommand` + a client-side guardrail
//! check + a note, so the agent (and the human watching it) sees exactly what would happen; only a
//! second call with `confirm: true` sends it. The server-side `ControlLimits` (notional + rate) and
//! the core `RiskGate` on the vike-tradehub node are the ENFORCING backstop regardless — the client
//! preview is UX, the node gate is truth. Writes go to a running node named by `--node <addr>` with
//! `VIKE_TRADEHUB_CONTROL_KEY` (reads need `VIKE_TRADEHUB_OBSERVE_KEY`) — taken from the process
//! environment, else from the credential store the daemon itself reads
//! (`<project>/settings/secrets.env`, see [`crate::cmd::nodekeys`]); absent from BOTH, the trade
//! tools return a clean error naming both places and the create+backtest tools still work.
//!
//! **Every `submit_order` carries a coid.** `client_order_id` stays an OPTIONAL argument, but the
//! node REFUSES a remote submit with an empty one, so when the agent omits it this server MINTS one
//! ([`verbs::fill_client_order_id`], the same generator and wire form the live core uses) before the
//! preview renders. The preview therefore shows a real id; a subsequent `confirm: true` call mints
//! its OWN unless the agent pins the previewed id back — `client_order_id` in the accepted response
//! is always the authoritative one.
//!
//! **Each write tool reports ITS OWN command's outcome** — [`Server::execute`] awaits the
//! `CommandTicket` the send returned, so `accepted` / `rejected` / `unknown` always describe the
//! command the agent just issued. It used to poll the handle's LATCHING `last_error`, which made
//! every tool call after the first refusal return that same refusal, including commands the node
//! executed — an agent reading that would reasonably retry and place the order twice.
//!
//! **Rationale (node proto v4).** Every write tool also takes an optional `reason` string — WHY the
//! agent is issuing this command. It rides BESIDE the command
//! (`Request::Command { cmd, reason }`), is echoed back in the mandatory preview so the agent (and
//! the human watching it) sees exactly what will be recorded, and lands in the node's audit trail —
//! sanitized server-side (`vike_tradehub::audit::sanitize_reason`). It NEVER reaches the order, the
//! core fold, the journal, or a venue.

use std::io::{self, BufRead, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use vike_datahub_client::DatahubClient;
use vike_model::client_order_id::ClientOrderIdGenerator;
use vike_tradehub_client::wire::WireCommand;
use vike_tradehub_client::{CommandOutcome, RemoteControlHandle, RemoteCoreHandle};

use crate::cmd::args::{self, Flags};
use crate::cmd::indicators;
use crate::cmd::nodekeys::{self, NodeKeyring};
use crate::cmd::verbs;

/// The datahub address the `run_backtest` tool ships profiles to (mirrors the `backtest` command's
/// default; overridable with `--addr`).
const DEFAULT_ADDR: &str = "127.0.0.1:7878";
const SERVER_NAME: &str = "vike-cli";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The MCP protocol version we default to when a client does not declare one (we ECHO the client's
/// requested version on `initialize` when present). `2024-11-05` is the widely-supported baseline.
const DEFAULT_PROTOCOL: &str = "2024-11-05";
const USAGE: &str = "usage: vike-cli mcp [--addr 127.0.0.1:7878] [--node <host:port>]";
/// How long a write tool waits for the node's verdict on the command it just sent before reporting
/// the outcome as unknown. A DEADLINE, not a sleep — `await_outcome` returns the instant that
/// command's reply lands — so it is generous on purpose: an agent acts on this answer, and "unknown"
/// is the expensive one.
const ACK_WAIT: Duration = Duration::from_secs(2);

/// The MCP server state: the datahub address for backtests, the optional vike-tradehub node address,
/// and the two lazily-opened persistent node connections (control = write, observe = read). The
/// connections are persistent — NOT per-call — because a control command is fire-and-forget over a
/// background worker (a per-call connect could drop the socket before the command is sent) and an
/// observe connection needs to stay subscribed to keep receiving snapshot pushes.
struct Server {
    datahub_addr: String,
    node_addr: Option<String>,
    control: Option<RemoteControlHandle>,
    observe: Option<RemoteCoreHandle>,
    /// The advisory preview caps, resolved once at startup — see [`verbs::guardrail_caps`]. The
    /// notional half is this machine's `policy.toml` ceiling (settings unification, Phase 5); the
    /// node's own `ControlLimits` is what actually enforces.
    caps: verbs::GuardrailCaps,
    /// The two node keys, resolved ONCE by the dispatcher from the process environment and then the
    /// credential store `vike-tradehub` itself reads. This used to be two `std::env::var` calls, so
    /// an agent on a box whose keys live in the store got "not set in the environment" for every
    /// live tool. See [`crate::cmd::nodekeys`].
    keys: NodeKeyring,
    /// This server's client-order-id generator — the node REFUSES a `Submit` with an empty
    /// `client_order_id`, so a tool call that omits the optional argument must still arrive with
    /// one. See [`verbs::coid_minter`].
    coids: ClientOrderIdGenerator,
}

/// Entry point the dispatcher routes to. Parses config, then serves the stdio MCP loop until EOF.
///
/// `policy_max_notional` is this machine's `max_notional_per_order` policy ceiling, already
/// resolved by [`crate::run`] (it replaced the removed `VIKE_MAX_ORDER_NOTIONAL`), and used only
/// by the mandatory preview's advisory guardrail. `keys` is the resolved [`NodeKeyring`] — same
/// story: the dispatcher owns the environment sweep and the credential-store read, this file takes
/// the answer.
pub fn run(
    args: impl Iterator<Item = String>,
    policy_max_notional: Option<f64>,
    keys: &NodeKeyring,
) -> ExitCode {
    let (datahub_addr, node_addr) = match parse_config(args) {
        Ok(c) => c,
        Err(msg) => return args::exit_for_parse_error("mcp", USAGE, &msg),
    };
    let mut server = Server {
        datahub_addr,
        node_addr,
        control: None,
        observe: None,
        caps: verbs::guardrail_caps(policy_max_notional),
        keys: keys.clone(),
        coids: verbs::coid_minter(),
    };
    let stdin = io::stdin();
    let stdout = io::stdout();
    match serve(stdin.lock(), stdout.lock(), &mut server) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli mcp: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Parse `--addr <datahub>` (default [`DEFAULT_ADDR`]) and the optional `--node <host:port>` (the
/// vike-tradehub node the trade tools control), via the shared [`crate::cmd::args`] glue. A
/// `--help`/`-h` short-circuits through [`args::help_requested`]; [`args::exit_for_parse_error`] in
/// [`run`] is what turns that back into a stdout usage and an exit 0.
fn parse_config(args: impl Iterator<Item = String>) -> Result<(String, Option<String>), String> {
    let mut addr = DEFAULT_ADDR.to_string();
    let mut node: Option<String> = None;
    let mut flags = Flags::new(args);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--addr" => addr = flags.value(&flag, inline)?,
            "--node" => node = Some(flags.value(&flag, inline)?),
            "-h" | "--help" => return args::help_requested(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok((addr, node))
}

/// The stdio loop: read newline-delimited JSON-RPC messages, dispatch each through the [`Server`],
/// write one response per request (notifications get none). A malformed line is answered with a
/// JSON-RPC parse error rather than killing the connection. Split from [`run`] so tests drive it.
fn serve(reader: impl BufRead, mut writer: impl Write, server: &mut Server) -> io::Result<()> {
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => server.handle(&msg),
            Err(e) => Some(rpc_error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(resp) = response {
            writeln!(writer, "{resp}")?;
            writer.flush()?;
        }
    }
    Ok(())
}

impl Server {
    /// Dispatch one JSON-RPC message, returning the response (`None` for a notification — no `id`).
    fn handle(&mut self, msg: &Value) -> Option<Value> {
        let method = msg.get("method").and_then(Value::as_str)?;
        let id = msg.get("id").cloned();
        match method {
            "initialize" => {
                let proto = msg
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(DEFAULT_PROTOCOL)
                    .to_string();
                Some(rpc_result(
                    id?,
                    json!({
                        "protocolVersion": proto,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
                    }),
                ))
            }
            "notifications/initialized" => None,
            "ping" => Some(rpc_result(id?, json!({}))),
            "tools/list" => Some(rpc_result(id?, json!({ "tools": tools_spec() }))),
            "tools/call" => {
                let id = id?;
                let name = msg.pointer("/params/name").and_then(Value::as_str).unwrap_or_default();
                let empty = json!({});
                let arguments = msg.pointer("/params/arguments").unwrap_or(&empty);
                // A panicking tool handler must not take down the stdio session (the vike-mcp
                // `rpc.rs` catch_unwind idiom): surface it as an `isError` tool result, same as
                // `Err(String)`, not a JSON-RPC protocol error and never a dead loop.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.call_tool(name, arguments)
                }));
                Some(match outcome {
                    Ok(Ok(structured)) => rpc_result(id, tool_ok(structured)),
                    Ok(Err(e)) => rpc_result(id, tool_err(&e)),
                    Err(_) => rpc_result(id, tool_err(&format!("tool panicked: {name}"))),
                })
            }
            _ => id.map(|id| rpc_error(id, -32601, &format!("method not found: {method}"))),
        }
    }

    /// Run one tool by name, returning its `structuredContent` on success or an error string
    /// (surfaced as an `isError` tool result). The order-write tools require `confirm: true` to
    /// execute; without it they return a preview and touch NO network.
    fn call_tool(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        match name {
            "validate_strategy" => tool_validate_strategy(args),
            "discover_params" => tool_discover_params(args),
            "list_templates" => Ok(tool_list_templates()),
            "list_indicators" => tool_list_indicators(args),
            "run_backtest" => tool_run_backtest(&self.datahub_addr, args),
            "run_sweep" => tool_run_sweep(&self.datahub_addr, args),
            "run_walk_forward" => tool_run_walk_forward(&self.datahub_addr, args),
            "list_strategies" => tool_list_strategies(&self.datahub_addr),
            "list_series" => tool_list_series(&self.datahub_addr),
            "node_snapshot" => self.tool_node_snapshot(),
            // The order-write tools: build the WireCommand (through the SHARED
            // `crate::cmd::verbs` construction site the trade REPL uses), gate on `confirm`.
            "submit_order" | "cancel_order" | "modify" | "flatten" | "market_exit"
            | "set_trading_state" | "mass_cancel" => {
                let cmd = verbs::wire_command_for(name, args)?;
                // MINT the client-order-id when the agent did not supply one — the node REFUSES a
                // remote `Submit` with an empty `client_order_id`, so `submit_order` without the
                // optional argument used to be un-executable. Minted BEFORE the confirm branch so
                // the preview shows a real id; a `confirm: true` call mints its own (the preview
                // note says so, and `client_order_id` in the accepted response is authoritative).
                let cmd = verbs::fill_client_order_id(cmd, &mut self.coids);
                // The optional rationale is read SEPARATELY (it rides beside the command, never
                // inside it) and carried straight through to the node's audit trail.
                let reason = verbs::reason_from_tool_args(args);
                if args.get("confirm").and_then(Value::as_bool) != Some(true) {
                    // MANDATORY PREVIEW: describe what WOULD happen, execute nothing.
                    return Ok(preview_of(name, &cmd, reason.as_deref(), self.caps));
                }
                self.execute(&cmd, reason)
            }
            // A test-only tool that panics, pinning the catch_unwind guard in `handle` (mirrors
            // the vike-mcp `rpc.rs` fake host's "panic" tool). Never advertised in `tools_spec`.
            #[cfg(test)]
            "__test_panic" => panic!("kaboom"),
            other => Err(format!("unknown tool: {other}")),
        }
    }

    /// Read the live node snapshot (orders / positions / equity / recent events) via the observe
    /// connection. Opens it lazily, then waits briefly for the node's first pushed frame.
    fn tool_node_snapshot(&mut self) -> Result<Value, String> {
        self.ensure_observe()?;
        let observe = self.observe.as_ref().expect("ensured");
        // A freshly-subscribed connection returns the empty placeholder (seq 0) until the node
        // pushes its first coalesced frame; wait up to ~2s for a real one.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut snap = observe.snapshot();
        while snap.seq == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            snap = observe.snapshot();
        }
        // Serialize the WireSnapshot as-is (it is already a flat, serde projection).
        serde_json::to_value(&*snap).map_err(|e| format!("cannot serialize snapshot: {e}"))
    }

    /// Send an already-built [`WireCommand`] to the node's control connection (opened lazily) and
    /// report what the node did with THAT command — [`RemoteControlHandle::await_outcome`] on the
    /// ticket the send returned. The order's own progress (fills) is still observed via
    /// `node_snapshot`; this answers only "did the node accept it".
    /// `reason` is the optional agent rationale, carried BESIDE the command for the node's audit
    /// trail (sanitized server-side) — it is never folded into the order.
    ///
    /// ⚠ This used to poll `RemoteControlHandle::last_error`, a LATCH that is never cleared: after
    /// one refusal every later write tool in the session returned `isError: node rejected the
    /// command`, including commands the node accepted and EXECUTED. An agent reading that would
    /// reasonably RETRY — placing the order twice.
    fn execute(&mut self, cmd: &WireCommand, reason: Option<String>) -> Result<Value, String> {
        self.ensure_control()?;
        let control = self.control.as_ref().expect("ensured");
        let ticket = control
            .try_command_with_reason(cmd.clone(), reason)
            .map_err(|e| format!("control command not sent: {e:?}"))?;
        match control.await_outcome(ticket, ACK_WAIT) {
            Some(CommandOutcome::Accepted { coid }) => Ok(json!({
                "sent": true,
                "outcome": "accepted",
                "client_order_id": coid,
                "note": "the node ACCEPTED this command (empty client_order_id = an account-wide verb); call node_snapshot to observe the result. The node's server-side ControlLimits + RiskGate are the enforcing gate."
            })),
            Some(CommandOutcome::Refused(err)) => {
                Err(format!("node rejected the command: {err}"))
            }
            // A dropped connection is NOT a refusal and NOT a success — do not let an agent infer
            // either. Say the outcome is unknown and point at the read tool that settles it.
            Some(CommandOutcome::Disconnected) => Err(
                "the control connection dropped before the node answered this command — its outcome is UNKNOWN and it may have executed. Call node_snapshot to check BEFORE retrying."
                    .to_string(),
            ),
            None => Ok(json!({
                "sent": true,
                "outcome": "unknown",
                "note": format!("the node did not answer within {}s — this command's outcome is NOT known and it may still execute. Call node_snapshot to check BEFORE retrying.", ACK_WAIT.as_secs())
            })),
        }
    }

    /// Open the control (write) connection if not already open. Errors if `--node` was not given or
    /// the control key is absent from the process env.
    fn ensure_control(&mut self) -> Result<(), String> {
        if self.control.is_some() {
            return Ok(());
        }
        let addr = self.node_addr.as_ref().ok_or(
            "no vike-tradehub node configured — pass `--node <host:port>` to enable order-write tools",
        )?;
        let (key, _) =
            self.keys.control().ok_or_else(|| self.missing_key(nodekeys::CONTROL_KEY_ENV))?;
        let handle = RemoteControlHandle::connect(addr.as_str(), key.as_bytes())
            .map_err(|e| format!("cannot open control connection to {addr}: {e}"))?;
        self.control = Some(handle);
        Ok(())
    }

    /// The "this key is nowhere" tool error. It names BOTH sources, because the old message said
    /// "is not set in the environment" while the daemon's own keys sat unread in the credential
    /// store — an agent (and the human reading its transcript) has no way to diagnose that.
    fn missing_key(&self, name: &str) -> String {
        format!(
            "{name} is set neither in the process environment nor in the credential store — \
             run `vike-cli secrets path` to see which store this project resolves to, and \
             `vike-cli secrets list` to see whether the key is in it"
        )
    }

    /// Open the observe (read) connection if not already open. Errors if `--node`/observe key absent.
    fn ensure_observe(&mut self) -> Result<(), String> {
        if self.observe.is_some() {
            return Ok(());
        }
        let addr = self.node_addr.as_ref().ok_or(
            "no vike-tradehub node configured — pass `--node <host:port>` to read node state",
        )?;
        let (key, _) =
            self.keys.observe().ok_or_else(|| self.missing_key(nodekeys::OBSERVE_KEY_ENV))?;
        let handle = RemoteCoreHandle::connect(addr.as_str(), key.as_bytes())
            .map_err(|e| format!("cannot open observe connection to {addr}: {e}"))?;
        self.observe = Some(handle);
        Ok(())
    }
}

// ---- pure tool implementations (no `self` / no network) --------------------------------------

/// Compile-check a Rhai strategy OFFLINE (the absorbed `vike-mcp` `validate_strategy` tool —
/// compile IS validation). Goes through `vike_script::discover_params`, the broker-free public
/// compile path: it compiles the script and runs its top level exactly once — the SAME error
/// surface as mounting the strategy (`RhaiStrategy::compile` does the same one-time top-level
/// run). A bad script is an `ok:false` ANSWER (the agent reads the error and fixes the script),
/// never a tool error; only a missing argument is.
fn tool_validate_strategy(args: &Value) -> Result<Value, String> {
    let script = args
        .get("script")
        .and_then(Value::as_str)
        .ok_or("validate_strategy requires a `script` string argument")?;
    Ok(match vike_script::discover_params(script) {
        Ok(_) => json!({ "ok": true }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    })
}

/// The starter Rhai strategies, as `{name, code}` rows — the absorbed `vike-mcp` `list_templates`
/// tool. The sources are STATIC DATA copied verbatim from `vike_studio_core::templates` (importing
/// that crate would drag DataFusion into this deliberately DataFusion-free CLI); each is
/// parameterized via `param()` so it drops straight into `run_sweep`, and each is pinned by two
/// tests below — one that COMPILES it, and one that EXECUTES it over a broker double and asserts
/// an order arrives. The second exists because an agent COPIES this list, and a script whose
/// every call sits inside `fn on_bar()` compiles perfectly while failing on every single bar.
fn tool_list_templates() -> Value {
    let templates: Vec<Value> =
        TEMPLATES.iter().map(|(name, code)| json!({ "name": name, "code": code })).collect();
    json!({ "templates": templates })
}

/// `(name, source)` starters (verbatim from `vike-studio-core/src/templates.rs`).
///
/// ⚠ That crate is NOT imported (it would drag DataFusion into this deliberately DataFusion-free
/// CLI), so these are a HAND COPY with no cross-crate drift gate — the two must be edited together.
/// Each side carries its own EXECUTION gate over its own copy instead:
/// `every_shipped_template_reaches_the_broker` below, and
/// `vike-studio-core/tests/templates_execute.rs`.
const TEMPLATES: &[(&str, &str)] =
    &[("SMA cross", SMA_CROSS), ("RSI reversion", RSI_REVERSION), ("Donchian breakout", BREAKOUT)];

const SMA_CROSS: &str = r#"
let fast = param("fast", 5.0);
let slow = param("slow", 20.0);
fn on_bar() {
    let f = sma(fast.to_int()); let s = sma(slow.to_int());
    if s.is_nan() { return; }
    let target = if f > s { 1.0 } else { -1.0 };
    let delta = target - position();
    if abs(delta) > 1e-12 { market(if delta > 0.0 { 1 } else { -1 }, abs(delta)); }
}
"#;

const RSI_REVERSION: &str = r#"
let len = param("len", 14.0);
let lo = param("lo", 30.0);
let hi = param("hi", 70.0);
fn on_bar() {
    let r = rsi(len.to_int());
    if r.is_nan() { return; }
    if r < lo && position() <= 0.0 { market(1, 1.0); }
    if r > hi && position() >= 0.0 { market(-1, 1.0); }
}
"#;

const BREAKOUT: &str = r#"
let len = param("len", 20.0);
fn on_bar() {
    let h = sma(len.to_int());   // symmetric proxy. donchian is NOT bound: it is multi-output
                                 // (upper/mid/lower) and the bridge binds single-output indicators
                                 // only, so `donchian(len)` is a function-not-found at the first
                                 // call rather than a silent upper-band read.
    if h.is_nan() { return; }
    if high() > h && position() <= 0.0 { market(1, 1.0); }
    if low() < h && position() >= 0.0 { market(-1, 1.0); }
}
"#;

fn tool_discover_params(args: &Value) -> Result<Value, String> {
    let script = args
        .get("script")
        .and_then(Value::as_str)
        .ok_or("discover_params requires a `script` string argument")?;
    let params =
        vike_script::discover_params(script).map_err(|e| format!("rhai compile error: {e}"))?;
    let params: Vec<Value> = params
        .into_iter()
        .map(|(name, default)| json!({ "name": name, "default": default }))
        .collect();
    Ok(json!({ "params": params }))
}

/// The HOST-BOUND callable set — `vike_script::is_callable`, what
/// `crates/vike-script/src/engine.rs`'s `register_indicators` actually registers under a bare name
/// OR a per-line accessor (`bollinger` has no bare call and three accessors) — joined onto its
/// `vike_indicators::registry()` metadata. NEVER the registry itself: a script calling a name the
/// host does not bind hits a function-not-found error every bar and self-disables, so a roster that
/// over-advertises hands an agent names that produce a strategy which looks mounted and never
/// trades. The filter, the rows and the human `vike-cli indicators` listing are all
/// `crates/vike-cli/src/cmd/indicators.rs`'s (`bound_metas`/`detail_row`/`compact_row`), so the two
/// surfaces cannot disagree about what is callable.
///
/// # ⚠ TIERED, because this roster is now long
///
/// The bound set was three names when this tool was written and is the near-whole catalog now, so
/// a single full response is no longer small: a full row carries a label, a category, every
/// parameter with its default and the value that comes back, and an agent that asked "what can I
/// call" would be handed tens of kilobytes — most of it about indicators it will not use — inside
/// its context window.
///
/// So the DEFAULT answer is the compact roster (name + category, the two fields that make a list
/// navigable), and detail is pulled per `name` or per `category`. That is the only size decision
/// this file makes: the `tools/list` DESCRIPTION deliberately names none of the set (see
/// [`tools_spec`]), because that text ships in every session whether the tool is called or not.
fn tool_list_indicators(args: &Value) -> Result<Value, String> {
    let name = args.get("name").and_then(Value::as_str).filter(|s| !s.trim().is_empty());
    let category = args.get("category").and_then(Value::as_str).filter(|s| !s.trim().is_empty());
    if let Some(name) = name {
        let m = indicators::find_bound(name)?;
        return Ok(json!({ "indicators": [indicators::detail_row(m)], "count": 1 }));
    }
    let all = indicators::bound_metas();
    if let Some(category) = category {
        let rows = indicators::filter_by_category(&all, category)?;
        let payload: Vec<Value> = rows.iter().map(|m| indicators::detail_row(m)).collect();
        return Ok(json!({ "indicators": payload, "count": payload.len() }));
    }
    let payload: Vec<Value> = all.iter().map(|m| indicators::compact_row(m)).collect();
    Ok(json!({
        "indicators": payload,
        "count": payload.len(),
        "note": "the callable roster, compact. Call again with `name` for one indicator in full (its parameters with their defaults, and the value it returns) or with `category` for a whole family. A name absent from this list is NOT callable: rhai resolves a function name when the line runs, so a script naming it compiles and then fails on every bar until the strategy switches itself off. `name` on such a name answers with the reason it is held back."
    }))
}

fn tool_run_backtest(datahub_addr: &str, args: &Value) -> Result<Value, String> {
    let profile = args
        .get("profile")
        .and_then(Value::as_str)
        .ok_or("run_backtest requires a `profile` string argument")?;
    let mut profile_toml = profile.to_string();
    if let Some(script) = args.get("script").and_then(Value::as_str) {
        profile_toml = crate::cmd::backtest::inject_script_src(&profile_toml, script)?;
    }
    let mut client = DatahubClient::connect(datahub_addr)
        .map_err(|e| format!("cannot connect to datahub at {datahub_addr}: {e}"))?;
    let report_json = client.run_backtest(&profile_toml)?;
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server report was not valid JSON: {e}"))?;
    Ok(json!({ "report": report }))
}

/// List the compiled NATIVE backtest-strategy roster the remote vike-datahub server advertises
/// (`vike_backtest::harness::STRATEGIES` over RPC) — the names a profile's `strategy.name` can
/// resolve, so an agent can discover which strategies exist before authoring a profile. Connects
/// to the datahub like `run_backtest`; a connect failure or a server-side error is a clean tool
/// error.
fn tool_list_strategies(datahub_addr: &str) -> Result<Value, String> {
    let mut client = DatahubClient::connect(datahub_addr)
        .map_err(|e| format!("cannot connect to datahub at {datahub_addr}: {e}"))?;
    let strategies = client.list_strategies()?;
    Ok(json!({ "strategies": strategies }))
}

/// Resolve the profile TOML a run tool ships from its arguments: the required `profile` string,
/// with the optional `script` injected as `[strategy.params].src` (the `run_backtest`/`backtest
/// --script` idiom). Returned as TEXT — the run tools ship it verbatim, exactly like the
/// subcommands, so the SERVER's `BacktestProfile::from_toml_str` is the only profile parser.
fn profile_from_args(args: &Value, tool: &str) -> Result<String, String> {
    let profile = args.get("profile").and_then(Value::as_str).ok_or_else(|| {
        format!("{tool} requires a `profile` string argument (a backtest profile TOML)")
    })?;
    let mut profile_toml = profile.to_string();
    if let Some(script) = args.get("script").and_then(Value::as_str) {
        profile_toml = crate::cmd::backtest::inject_script_src(&profile_toml, script)?;
    }
    Ok(profile_toml)
}

/// Run a remote parameter-grid sweep on the vike-datahub server — the absorbed `vike-mcp`
/// `run_sweep`, gone remote: EXACTLY the request the `sweep` subcommand builds (the SAME profile
/// TOML over the SAME `run_sweep_profile` wire verb), so the tool and the subcommand can never
/// drift. The optional `rank_by` argument names the server-side metric.
///
/// NOTE the trade this shares with `run_backtest`: shipping the TOML verbatim means a profile-SHAPE
/// error (no `[sweep]` table, bad range, unknown strategy) now surfaces from the SERVER rather than
/// before the connect. One parser, one error source.
fn tool_run_sweep(datahub_addr: &str, args: &Value) -> Result<Value, String> {
    let profile_toml = profile_from_args(args, "run_sweep")?;
    let rank_by = args.get("rank_by").and_then(Value::as_str);
    let mut client = DatahubClient::connect(datahub_addr)
        .map_err(|e| format!("cannot connect to datahub at {datahub_addr}: {e}"))?;
    let report_json = client.run_sweep_profile(&profile_toml, rank_by)?;
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server sweep report was not valid JSON: {e}"))?;
    Ok(json!({ "sweep": report }))
}

/// Run a remote anchored out-of-sample walk-forward on the vike-datahub server — the absorbed
/// `vike-mcp` `run_walk_forward`, gone remote: the SAME request the `walkforward` subcommand builds
/// (the profile TOML over the `run_walkforward_profile` wire verb). The split count rides IN the
/// profile's `[walkforward]` table.
fn tool_run_walk_forward(datahub_addr: &str, args: &Value) -> Result<Value, String> {
    let profile_toml = profile_from_args(args, "run_walk_forward")?;
    let mut client = DatahubClient::connect(datahub_addr)
        .map_err(|e| format!("cannot connect to datahub at {datahub_addr}: {e}"))?;
    let report_json = client.run_walkforward_profile(&profile_toml)?;
    let report: Value = serde_json::from_str(&report_json)
        .map_err(|e| format!("server walk-forward report was not valid JSON: {e}"))?;
    Ok(json!({ "walkforward": report }))
}

/// List every stored series the remote vike-datahub server holds, with its cheap coverage — the
/// absorbed `vike-mcp` `list_series`, gone remote over the datahub `Inventory` metadata verb (the
/// coverage-carrying superset of `ListSeries`, so an agent can pick a `[data]` range, not just a
/// name). Rows are `{kind, venue, symbol, interval, first_ts, last_ts, rows}`.
fn tool_list_series(datahub_addr: &str) -> Result<Value, String> {
    let mut client = DatahubClient::connect(datahub_addr)
        .map_err(|e| format!("cannot connect to datahub at {datahub_addr}: {e}"))?;
    let inventory = client.inventory()?;
    let series: Vec<Value> = inventory
        .into_iter()
        .map(|(id, cov)| {
            json!({
                "kind": id.kind, "venue": id.venue, "symbol": id.symbol, "interval": id.interval,
                "first_ts": cov.first_ts, "last_ts": cov.last_ts, "rows": cov.rows,
            })
        })
        .collect();
    Ok(json!({ "series": series }))
}

/// The mandatory-preview payload for a write tool: the resolved command, the SHARED client-side
/// guardrail check ([`crate::cmd::verbs::guardrail_check`] — advisory; the node's server-side gate
/// is the enforcing one), the rationale that will be recorded, and the confirm hint. PURE.
///
/// `reason` is ECHOED here on purpose: it is the one part of the request the agent cannot otherwise
/// see the effect of (it goes to the node's audit trail, not to the order), so the preview shows it
/// alongside the command it will be filed against. It is echoed as SENT — the node applies its own
/// sanitization (`vike_tradehub::audit::sanitize_reason`) before recording.
fn preview_of(
    name: &str,
    cmd: &WireCommand,
    reason: Option<&str>,
    caps: verbs::GuardrailCaps,
) -> Value {
    let wire = serde_json::to_value(cmd).unwrap_or(Value::Null);
    json!({
        "will_execute": false,
        "tool": name,
        "wire_command": wire,
        "guardrail": verbs::guardrail_check(cmd, caps).to_json(),
        "reason": reason,
        "note": "PREVIEW ONLY — nothing was sent. Call again with \"confirm\": true to execute. For submit_order, `wire_command.Submit.client_order_id` was MINTED for this preview: pass it back as `client_order_id` on the confirming call to pin that exact id, otherwise a fresh one is minted and returned in the accepted response. The vike-tradehub node's ControlLimits (notional + rate) and RiskGate are the enforcing gate. Any `reason` is recorded in the node's audit trail (sanitized) and never reaches the order."
    })
}

/// The optional `reason` property every WRITE tool advertises — one definition, spliced into each
/// tool's `inputSchema` so the wording (and the fact that it is never required) can never drift
/// between tools.
fn reason_property() -> Value {
    json!({
        "type": "string",
        "description": "optional rationale — WHY this command is being issued. Recorded in the node's audit trail (control characters stripped, capped at 512 chars); never reaches the order, the core, or the venue."
    })
}

/// The `tools/list` payload — each tool's schema + risk annotations.
fn tools_spec() -> Value {
    json!([
        {
            "name": "validate_strategy",
            "description": "Compile a Rhai strategy; returns {ok, error?}. Compile IS validation. Offline — no server.",
            "inputSchema": { "type": "object", "properties": { "script": { "type": "string", "description": "Rhai strategy source" } }, "required": ["script"] },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        },
        {
            "name": "discover_params",
            "description": "Discover the tunable param(name, default) knobs an authored Rhai strategy declares. Offline — no server.",
            "inputSchema": { "type": "object", "properties": { "script": { "type": "string" } }, "required": ["script"] },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        },
        {
            "name": "list_templates",
            "description": "List starter Rhai strategies as {name, code} — each parameterized via param() so it drops straight into run_sweep. Offline — no server.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        },
        {
            "name": "list_indicators",
            // ⚠ This text ships in EVERY session's tools/list, called or not, so it names none of
            // the roster — see `list_indicators_description_does_not_enumerate_the_set`. It used
            // to splice `RHAI_INDICATORS.join(", ")` in, which was 13 characters when the host
            // bound three names and is kilobytes now that it binds the catalog.
            "description": "List the HOST-BOUND indicators a Rhai strategy can actually call — what the vike-script host registers, which is not every vike-indicators registry name; a script calling an unbound one compiles and then fails on every bar. No arguments returns the compact roster (name + category). Pass `category` for one family in full, or `name` for one indicator in full — its parameters with defaults and its output line — which also answers WHY a registry name is not callable. The roster is not repeated in this text on purpose: it is long, and a description is sent whether you call the tool or not.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "one indicator, in full (its parameters with defaults, and the value it returns) — or, for a registry name the host holds back, the reason" },
                    "category": { "type": "string", "description": "one family, in full — the `category` value the roster reports (case-insensitive)" }
                }
            },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false }
        },
        {
            "name": "run_backtest",
            "description": "Run a backtest on the remote vike-datahub server from a profile TOML, optionally injecting a Rhai script. Returns the BacktestReport JSON.",
            "inputSchema": {
                "type": "object",
                "properties": { "profile": { "type": "string" }, "script": { "type": "string" } },
                "required": ["profile"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "run_sweep",
            "description": "Run a parameter-grid sweep on the remote vike-datahub server from a profile TOML with a [sweep] table (each key overrides strategy.params.<key> across a value grid), optionally injecting a Rhai script. Returns the server-ranked SweepReport JSON (one row per grid point, each with its BacktestReport, best first).",
            "inputSchema": {
                "type": "object",
                "properties": { "profile": { "type": "string", "description": "a backtest profile TOML with [data]/[strategy]/[sweep] (+ optional [engine])" }, "script": { "type": "string", "description": "optional Rhai source injected as [strategy.params].src" }, "rank_by": { "type": "string", "description": "sharpe (default) | return | max_dd | equity — applied server-side" } },
                "required": ["profile"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "run_walk_forward",
            "description": "Run an anchored out-of-sample walk-forward on the remote vike-datahub server from a profile TOML with a [walkforward] table (n_splits), optionally injecting a Rhai script. Returns the stitched WalkForwardReport JSON (OOS windows + oos_sharpe + wf_consistency).",
            "inputSchema": {
                "type": "object",
                "properties": { "profile": { "type": "string", "description": "a backtest profile TOML with [data]/[strategy]/[walkforward] (+ optional [engine])" }, "script": { "type": "string", "description": "optional Rhai source injected as [strategy.params].src" } },
                "required": ["profile"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "list_strategies",
            "description": "List the compiled native backtest strategies the remote vike-datahub server offers (the names a profile's strategy.name can resolve). Requires a reachable datahub (--addr).",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
        },
        {
            "name": "list_series",
            "description": "List every stored data series the remote vike-datahub server holds, with coverage: {kind, venue, symbol, interval, first_ts, last_ts, rows} — what a profile's [data] table can name. Requires a reachable datahub (--addr).",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": true }
        },
        {
            "name": "node_snapshot",
            "description": "Read the running vike-tradehub node's live state (orders, positions, per-venue equity, recent events). Requires --node + VIKE_TRADEHUB_OBSERVE_KEY.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "submit_order",
            "description": "Submit an order on the vike-tradehub node. WITHOUT confirm:true this only PREVIEWS (executes nothing). Requires --node + VIKE_TRADEHUB_CONTROL_KEY.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "venue": { "type": "string" }, "symbol": { "type": "string" },
                    "side": { "type": "integer", "description": "1 buy / -1 sell" },
                    "qty": { "type": "number" },
                    "order_type": { "type": "string", "description": "market | limit | stop | take_profit (default market)" },
                    "price": { "type": "number" }, "trigger_price": { "type": "number" },
                    "reduce_only": { "type": "boolean" },
                    "reason": reason_property(),
                    "confirm": { "type": "boolean", "description": "must be true to actually execute; else a preview is returned" }
                },
                "required": ["venue", "symbol", "side", "qty"]
            },
            "annotations": { "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }
        },
        {
            "name": "cancel_order",
            "description": "Cancel one resting order by client_order_id. WITHOUT confirm:true this only PREVIEWS.",
            "inputSchema": {
                "type": "object",
                "properties": { "client_order_id": { "type": "string" }, "reason": reason_property(), "confirm": { "type": "boolean" } },
                "required": ["client_order_id"]
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "modify",
            "description": "Modify one resting order's qty and/or price by client_order_id (at least one of new_qty/new_price). PREVIEW IS MANDATORY: WITHOUT confirm:true this only PREVIEWS (executes nothing).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "client_order_id": { "type": "string" },
                    "new_qty": { "type": "number" }, "new_price": { "type": "number" },
                    "reason": reason_property(),
                    "confirm": { "type": "boolean", "description": "must be true to actually execute; else a preview is returned" }
                },
                "required": ["client_order_id"]
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "flatten",
            "description": "Close the (venue, symbol) net position with a reduce-only market order. WITHOUT confirm:true this only PREVIEWS.",
            "inputSchema": {
                "type": "object",
                "properties": { "venue": { "type": "string" }, "symbol": { "type": "string" }, "reason": reason_property(), "confirm": { "type": "boolean" } },
                "required": ["venue", "symbol"]
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "market_exit",
            "description": "PANIC BUTTON — cancel every live order then flatten every position (optionally scoped to one venue). WITHOUT confirm:true this only PREVIEWS.",
            "inputSchema": {
                "type": "object",
                "properties": { "venue": { "type": "string", "description": "omit for every engine" }, "reason": reason_property(), "confirm": { "type": "boolean" } }
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "mass_cancel",
            "description": "Cancel EVERY live order, optionally scoped to one venue and/or symbol. PREVIEW IS MANDATORY: WITHOUT confirm:true this only PREVIEWS (executes nothing).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "venue": { "type": "string", "description": "omit for every venue" },
                    "symbol": { "type": "string", "description": "omit for every symbol" },
                    "reason": reason_property(),
                    "confirm": { "type": "boolean", "description": "must be true to actually execute; else a preview is returned" }
                }
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        },
        {
            "name": "set_trading_state",
            "description": "Set the account trading state / kill switch. WITHOUT confirm:true this only PREVIEWS.",
            "inputSchema": {
                "type": "object",
                "properties": { "state": { "type": "string", "description": "active | reducing | halted" }, "reason": reason_property(), "confirm": { "type": "boolean" } },
                "required": ["state"]
            },
            "annotations": { "destructiveHint": true, "openWorldHint": true }
        }
    ])
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tool_ok(structured: Value) -> Value {
    let text = serde_json::to_string_pretty(&structured).unwrap_or_default();
    json!({ "content": [ { "type": "text", "text": text } ], "structuredContent": structured, "isError": false })
}

fn tool_err(message: &str) -> Value {
    json!({ "content": [ { "type": "text", "text": message } ], "isError": true })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The EXECUTION gate on the shipped templates (see `every_shipped_template_reaches_the_broker`):
    // the real `RhaiStrategy` mounted over vike-model's shared `MockBroker` (its `test-support`
    // feature, already a dev-dep of this crate for `tests/init_cli.rs`) and driven over real bars.
    use vike_model::strategy::MockBroker;
    use vike_model::{Bar, Strategy};
    use vike_script::RhaiStrategy;

    fn server() -> Server {
        Server {
            datahub_addr: DEFAULT_ADDR.to_string(),
            node_addr: None,
            control: None,
            observe: None,
            caps: verbs::GuardrailCaps::default(),
            keys: NodeKeyring::default(),
            coids: verbs::coid_minter(),
        }
    }

    fn req(id: i64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn call(name: &str, args: Value) -> Value {
        server().handle(&req(1, "tools/call", json!({ "name": name, "arguments": args }))).unwrap()
    }

    #[test]
    fn initialize_echoes_protocol_and_names_the_server() {
        let resp = server()
            .handle(&req(1, "initialize", json!({ "protocolVersion": "2025-06-18" })))
            .unwrap();
        assert_eq!(resp["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(resp["result"]["serverInfo"]["name"], "vike-cli");
    }

    /// A function name NOTHING binds — the specimen the non-vacuity proof below rewrites a template
    /// to call. Typo-shaped on purpose: the failure it stands for is a misspelling, and the two
    /// assertions at the call site prove it is unbound rather than trusting this comment.
    const UNBOUND_WITNESS: &str = "sma_typo";

    /// The MCP write-tool roster — must stay the FULL `trade`-REPL write-verb set (the drift the
    /// shared `cmd::verbs` module exists to prevent).
    const WRITE_TOOLS: [&str; 7] = [
        "submit_order",
        "cancel_order",
        "modify",
        "flatten",
        "market_exit",
        "set_trading_state",
        "mass_cancel",
    ];

    #[test]
    fn tools_list_has_read_and_write_tools_with_correct_hints() {
        let resp = server().handle(&req(2, "tools/list", json!({}))).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"run_backtest") && names.contains(&"submit_order"));
        // The absorbed vike-mcp read surface + the closed write-verb drift are all advertised.
        for n in [
            "validate_strategy",
            "discover_params",
            "list_templates",
            "list_series",
            "run_sweep",
            "run_walk_forward",
            "modify",
            "mass_cancel",
        ] {
            assert!(names.contains(&n), "{n} must be advertised in tools/list");
        }
        for t in tools {
            let n = t["name"].as_str().unwrap();
            if WRITE_TOOLS.contains(&n) {
                assert_eq!(t["annotations"]["destructiveHint"], true, "{n} must be destructive");
                // The mandatory-preview gate is part of each write tool's advertised contract.
                let desc = t["description"].as_str().unwrap();
                assert!(desc.contains("confirm"), "{n} must document the preview gate: {desc}");
            } else {
                assert_eq!(t["annotations"]["readOnlyHint"], true, "{n} must be read-only");
            }
        }
    }

    #[test]
    fn discover_params_tool_returns_the_declared_knobs() {
        let resp = call(
            "discover_params",
            json!({ "script": "let qty = param(\"qty\", 2.5);\nfn on_bar() {}" }),
        );
        assert_eq!(resp["result"]["isError"], false);
        let params = resp["result"]["structuredContent"]["params"].as_array().unwrap();
        assert_eq!(params[0]["name"], "qty");
        assert_eq!(params[0]["default"], 2.5);
    }

    #[test]
    fn list_indicators_returns_exactly_the_host_bound_set() {
        // The tool must advertise ONLY what the Rhai host binds (vike_script::RHAI_INDICATORS),
        // never the whole `vike_indicators::registry()` — an agent acts on this list, and any
        // unbound name it is handed produces a script that silently self-disables. Both sides read
        // the SAME derived list, so this cannot pass while the tool under-reports either.
        let resp = call("list_indicators", json!({}));
        assert_eq!(resp["result"]["isError"], false);
        let inds = resp["result"]["structuredContent"]["indicators"].as_array().unwrap();
        let mut names: Vec<&str> = inds.iter().map(|i| i["name"].as_str().unwrap()).collect();
        names.sort_unstable();
        // The union, for the reason spelled out on `is_callable`: an indicator reachable only
        // through `bollinger_mid(20)` is still an indicator this tool must offer.
        let mut expected: Vec<&str> = vike_indicators::registry()
            .iter()
            .map(|m| m.name)
            .filter(|n| vike_script::is_callable(n))
            .collect();
        expected.sort_unstable();
        assert_eq!(names, expected, "list_indicators must be the Rhai host-bound set");
        assert_eq!(resp["result"]["structuredContent"]["count"], names.len());
        // ⚠ The DEFAULT response stays compact: name + category, and no per-indicator detail. The
        // roster is the whole catalog now, so a default that carried every parameter and output
        // line would spend an agent's context on indicators it never asked about.
        for i in inds {
            assert!(i["category"].as_str().is_some_and(|s| !s.is_empty()), "{i}");
            assert!(i.get("params").is_none(), "the default roster must stay compact: {i}");
        }
    }

    /// Detail is PULLED, per name or per family — the other half of the tiering above.
    #[test]
    fn list_indicators_narrows_by_name_and_by_category() {
        // Derived: whatever the host binds first, never a hard-coded name.
        let first = vike_script::RHAI_INDICATORS.first().expect("the host binds something");
        let one = call("list_indicators", json!({ "name": first }));
        assert_eq!(one["result"]["isError"], false);
        let rows = one["result"]["structuredContent"]["indicators"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], *first);
        // The two fields a compact roster CANNOT carry, and the reason detail exists at all: a
        // multi-parameter, multi-output indicator is indistinguishable from a simple one without
        // them.
        assert!(rows[0]["params"].is_array(), "a detail row must carry its parameters");
        assert!(rows[0]["outputs"].as_array().is_some_and(|o| !o.is_empty()));
        let category = rows[0]["category"].as_str().unwrap().to_string();

        let family = call("list_indicators", json!({ "category": category.to_lowercase() }));
        assert_eq!(family["result"]["isError"], false, "the category match is case-insensitive");
        let fam = family["result"]["structuredContent"]["indicators"].as_array().unwrap();
        assert!(fam.iter().any(|r| r["name"] == *first));
        assert!(fam.iter().all(|r| r["category"] == category.as_str()));

        // A name nobody can call is an ERROR, not an empty list: an empty answer reads as "that
        // family is empty in this build", which sends somebody hunting through a config.
        let miss = call("list_indicators", json!({ "name": "sma_typo" }));
        assert_eq!(miss["result"]["isError"], true);
        let bad = call("list_indicators", json!({ "category": "not-a-category" }));
        assert_eq!(bad["result"]["isError"], true);
        assert!(bad["result"]["content"][0]["text"].as_str().unwrap().contains(&category));
    }

    /// A registry indicator the host HOLDS BACK answers with the host's own REASON. An agent that
    /// reached for one — they are real indicator names, and a model has seen them — otherwise gets
    /// "unknown", re-checks its spelling, and tries again with the same name.
    ///
    /// Derived on every axis: which name is held back, and what the reason says, both come from
    /// `vike-script`. A build binding the entire registry has nothing to explain and skips — stated
    /// rather than silent, because a vacuous pass should be readable in the test, not inferred.
    #[test]
    fn list_indicators_says_why_a_registry_name_is_not_callable() {
        // ⚠ NOT `!RHAI_INDICATORS.contains(..)` any more. That set is the BARE names, and since
        // per-line accessors landed a name can be absent from it and still perfectly callable —
        // `bollinger` is. A genuinely held-back indicator is one no spelling reaches, which is
        // exactly what `is_callable` answers.
        let held = vike_indicators::registry().iter().find(|m| !vike_script::is_callable(m.name));
        let Some(held) = held else {
            return; // nothing is held back in this build
        };
        let resp = call("list_indicators", json!({ "name": held.name }));
        assert_eq!(resp["result"]["isError"], true, "a name a script cannot call is not an answer");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let why = vike_script::unbound_reason(held.name)
            .expect("a held-back registry indicator must carry a reason");
        assert!(text.contains(why), "the tool must quote the host's own reason: {text}");
    }

    /// ⚠ The `tools/list` description ships in EVERY session, whether the tool is called or not, so
    /// it must NOT enumerate the roster. It used to — `RHAI_INDICATORS.join(", ")` was spliced in,
    /// which cost 13 characters while the host bound three names and would cost kilobytes of every
    /// agent's context now that it binds the catalog.
    #[test]
    fn list_indicators_description_does_not_enumerate_the_set() {
        let resp = server().handle(&req(2, "tools/list", json!({}))).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        let tool = tools.iter().find(|t| t["name"] == "list_indicators").unwrap();
        let desc = tool["description"].as_str().unwrap();
        assert!(desc.contains("HOST-BOUND"), "{desc}");
        assert!(
            !desc.contains(&vike_script::RHAI_INDICATORS.join(", ")),
            "the description must not carry the roster: {desc}"
        );
        // A FIXED ceiling, deliberately independent of the set: the point is that this text cannot
        // grow when an indicator is added.
        const MAX_DESCRIPTION: usize = 700;
        assert!(
            desc.len() <= MAX_DESCRIPTION,
            "the tools/list description is {} bytes — it ships in every session and must stay \
             bounded; describe the tool, do not list its answer",
            desc.len()
        );
        // ...and it must tell the agent how to GET the set, or bounding it just hid the answer.
        assert!(desc.contains("category") && desc.contains("name"), "{desc}");
    }

    #[test]
    fn a_panicking_tool_becomes_iserror_not_a_dead_session() {
        // Mirrors vike-mcp's rpc.rs `tools_call_panic_becomes_iserror` pin: the catch_unwind in
        // `handle` turns a panicking tool into an `isError` tool RESULT — never a JSON-RPC
        // protocol error, never a crashed stdio loop.
        let resp = call("__test_panic", json!({}));
        assert_eq!(resp["result"]["isError"], true);
        assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("panicked"));
        assert!(resp.get("error").is_none(), "a caught tool panic is NOT a protocol error");
    }

    #[test]
    fn submit_order_without_confirm_only_previews_and_touches_no_network() {
        // node_addr is None; a preview must NOT try to connect (it would error). It returns the
        // resolved wire command with will_execute:false.
        let resp = call(
            "submit_order",
            json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 0.5, "order_type": "limit", "price": 100.0 }),
        );
        assert_eq!(resp["result"]["isError"], false, "a preview succeeds even with no node");
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], false);
        assert_eq!(sc["wire_command"]["Submit"]["symbol"], "BTCUSDT");
        assert_eq!(sc["wire_command"]["Submit"]["qty"], 0.5);
        assert_eq!(sc["guardrail"]["notional"], 50.0); // 0.5 * 100
    }

    /// Every WRITE tool advertises the optional `reason` (node proto v4) — and never requires it, so
    /// an agent that ignores it keeps working exactly as before.
    #[test]
    fn every_write_tool_advertises_an_optional_reason() {
        let resp = server().handle(&req(2, "tools/list", json!({}))).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        for name in WRITE_TOOLS {
            let tool = tools.iter().find(|t| t["name"] == name).expect("advertised");
            let props = &tool["inputSchema"]["properties"];
            assert_eq!(props["reason"]["type"], "string", "{name} must offer a `reason` string");
            assert!(
                props["reason"]["description"].as_str().unwrap().contains("audit"),
                "{name}: the reason description must say where it lands"
            );
            let required = tool["inputSchema"]["required"].as_array();
            assert!(
                required.is_none_or(|r| !r.iter().any(|v| v == "reason")),
                "{name}: `reason` must stay OPTIONAL"
            );
        }
        // A READ tool has no rationale to record.
        let snap = tools.iter().find(|t| t["name"] == "node_snapshot").unwrap();
        assert!(snap["inputSchema"]["properties"]["reason"].is_null());
    }

    /// The preview ECHOES the rationale (so the agent sees what will be filed against the command)
    /// while leaving the resolved command untouched — the reason rides beside it, never inside it.
    #[test]
    fn a_write_preview_echoes_the_reason_without_changing_the_command() {
        let bare = call("cancel_order", json!({ "client_order_id": "c-1" }));
        assert_eq!(bare["result"]["structuredContent"]["reason"], Value::Null);

        let withr = call(
            "cancel_order",
            json!({ "client_order_id": "c-1", "reason": "  stale quote after the feed gap  " }),
        );
        let sc = &withr["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], false);
        assert_eq!(sc["reason"], "stale quote after the feed gap", "trimmed, and echoed back");
        assert_eq!(
            sc["wire_command"], bare["result"]["structuredContent"]["wire_command"],
            "the wire command is byte-identical with and without a reason"
        );
        // A blank rationale is no rationale (never an empty string in the preview).
        let blank = call("cancel_order", json!({ "client_order_id": "c-1", "reason": "   " }));
        assert_eq!(blank["result"]["structuredContent"]["reason"], Value::Null);
    }

    #[test]
    fn submit_order_missing_required_field_is_a_tool_error() {
        let resp = call("submit_order", json!({ "venue": "sim", "side": 1, "qty": 1.0 })); // no symbol
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn submit_order_with_confirm_but_no_node_is_a_clean_error() {
        let resp = call(
            "submit_order",
            json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0, "confirm": true }),
        );
        assert_eq!(resp["result"]["isError"], true, "confirm without a node must error, not panic");
    }

    #[test]
    fn set_trading_state_preview_maps_the_wire_command() {
        let resp = call("set_trading_state", json!({ "state": "halted" }));
        assert_eq!(
            resp["result"]["structuredContent"]["wire_command"]["SetTradingState"],
            "Halted"
        );
    }

    #[test]
    fn market_exit_preview_allows_an_omitted_venue() {
        let resp = call("market_exit", json!({}));
        assert_eq!(resp["result"]["isError"], false);
        assert!(resp["result"]["structuredContent"]["wire_command"]["MarketExit"].is_object());
    }

    #[test]
    fn list_strategies_is_advertised_read_only() {
        let resp = server().handle(&req(3, "tools/list", json!({}))).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        let tool = tools
            .iter()
            .find(|t| t["name"] == "list_strategies")
            .expect("list_strategies must be advertised in tools/list");
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
    }

    #[test]
    fn list_strategies_without_a_reachable_datahub_is_a_clean_error() {
        // Point at a port nothing is listening on: the connect must fail into a clean tool error,
        // never a panic. (`127.0.0.1:1` refuses immediately.)
        let mut s = Server {
            datahub_addr: "127.0.0.1:1".to_string(),
            node_addr: None,
            control: None,
            observe: None,
            caps: verbs::GuardrailCaps::default(),
            keys: NodeKeyring::default(),
            coids: verbs::coid_minter(),
        };
        let resp = s
            .handle(&req(1, "tools/call", json!({ "name": "list_strategies", "arguments": {} })))
            .unwrap();
        assert_eq!(resp["result"]["isError"], true, "an unreachable datahub must error, not panic");
    }

    #[test]
    fn node_snapshot_without_node_is_a_clean_error() {
        let resp = call("node_snapshot", json!({}));
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn unknown_method_is_a_jsonrpc_error() {
        let resp = server().handle(&req(9, "no/such", json!({}))).unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn a_notification_gets_no_response() {
        assert!(server()
            .handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .is_none());
    }

    // ---- the absorbed vike-mcp tool surface (Phase A) ----------------------------------------

    #[test]
    fn validate_strategy_answers_ok_and_compile_errors_offline() {
        // A good script is an `ok:true` ANSWER; a bad script is an `ok:false` ANSWER carrying the
        // compile error — NOT an isError tool failure (the agent reads the error and fixes the
        // script). Mirrors vike-mcp's validate_strategy semantics; the argument is `script` here
        // (vike-mcp said `code`), aligned with this file's discover_params.
        let good = call(
            "validate_strategy",
            json!({ "script": "let fast = param(\"fast\", 5.0);\nfn on_bar() {}" }),
        );
        assert_eq!(good["result"]["isError"], false);
        assert_eq!(good["result"]["structuredContent"]["ok"], true);

        let bad = call("validate_strategy", json!({ "script": "fn on_bar( {" }));
        assert_eq!(bad["result"]["isError"], false, "a compile error is an ANSWER, not a failure");
        let sc = &bad["result"]["structuredContent"];
        assert_eq!(sc["ok"], false);
        assert!(!sc["error"].as_str().unwrap().is_empty());
    }

    #[test]
    fn validate_strategy_missing_script_arg_is_a_tool_error() {
        let resp = call("validate_strategy", json!({}));
        assert_eq!(resp["result"]["isError"], true);
        assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("script"));
    }

    #[test]
    fn list_templates_returns_named_parameterized_sources_that_compile() {
        let resp = call("list_templates", json!({}));
        assert_eq!(resp["result"]["isError"], false);
        let ts = resp["result"]["structuredContent"]["templates"].as_array().unwrap();
        assert!(ts.iter().any(|t| t["name"] == "SMA cross"));
        // Every advertised template must actually compile against THIS build's Rhai host and
        // expose param() knobs (so it drops straight into run_sweep) — the same pin
        // vike-studio-core keeps on its own copy of these sources.
        //
        // ⚠ Compiling is NOT the property that matters most, and this test cannot see it:
        // `discover_params` runs the script's TOP LEVEL only, and every template's real work sits
        // inside `fn on_bar()`. `every_shipped_template_reaches_the_broker` below is the gate that
        // covers what this one structurally cannot.
        for t in ts {
            let (name, code) = (t["name"].as_str().unwrap(), t["code"].as_str().unwrap());
            let params = vike_script::discover_params(code)
                .unwrap_or_else(|e| panic!("template {name} must compile: {e}"));
            assert!(!params.is_empty(), "template {name} must expose param()s");
            assert!(code.contains("on_bar"), "template {name} must define on_bar");
        }
    }

    /// A deterministic strictly-RISING bar series. Monotone closes are enough to drive every
    /// shipped template past its warm-up and into a decision: `sma(5)` rises above `sma(20)`,
    /// `rsi(14)` climbs past the reversion band's `hi`, and `high()` clears the breakout channel.
    fn rising_bars(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let c = 100.0 + i as f64;
                Bar {
                    ts: i as i64 * 60_000,
                    open: c,
                    high: c,
                    low: c,
                    close: c,
                    volume: 1.0,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: Some("BTCUSDT".into()),
                }
            })
            .collect()
    }

    /// ⚠ **The gate behind "an agent can copy a template and it will actually trade."**
    ///
    /// `list_templates` is the surface an AGENT copies from, and until this test it was the one
    /// template surface with no behavioral gate at all: the compile pin above is the whole of what
    /// this crate checked. (`crates/vike-studio-core/tests/templates_execute.rs` is the twin over
    /// that crate's own copy of these sources; it runs them through the real Run pipeline instead.)
    ///
    /// Parsing is the wrong bar. Rhai resolves a REGISTERED function when its line RUNS, not at
    /// compile time, and `discover_params` runs only the top level — so every call a template makes
    /// (all of them inside `fn on_bar()`) is unchecked by a compile gate. Three mistakes land in
    /// that blind spot identically: an unbound NAME (a typo, or any name outside
    /// `vike_script::RHAI_INDICATORS` — the set the host actually registers), a wrong
    /// ARITY, and a wrong ARGUMENT TYPE (`rhai`'s `resolve_fn` hashes each argument's `TypeId` and
    /// performs no INT->FLOAT coercion for a registered function, so `market(1, 1)` or `sma(5.0)`
    /// misses just as hard as a typo). Each raises `ErrorFunctionNotFound` on every bar;
    /// `RhaiStrategy`'s hook runner swallows it (fail-safe: zero orders that bar) and self-disables
    /// after 10 consecutive errors. The strategy looks mounted and silently never trades. Only
    /// EXECUTION distinguishes that from a strategy that simply saw no signal.
    ///
    /// Non-vacuity is demonstrated rather than argued — see
    /// `an_unbound_call_compiles_and_then_reaches_the_broker_with_nothing` below, which drives the
    /// same series and shows this assertion genuinely fails for such a script.
    #[test]
    fn every_shipped_template_reaches_the_broker() {
        for (name, src) in TEMPLATES {
            let mut strat = RhaiStrategy::<MockBroker>::compile(src)
                .unwrap_or_else(|e| panic!("template {name} must compile: {e}"));
            let mut broker = MockBroker::default();
            for bar in rising_bars(80) {
                broker.px = bar.close;
                strat.on_bar(&mut broker, &bar);
            }
            assert!(
                !broker.markets.is_empty(),
                "template {name} placed no order over 80 rising bars — a template an agent COPIES \
                 must actually trade. Check that every function it calls is host-bound \
                 (`vike_script::RHAI_INDICATORS`, plus `crates/vike-script/src/engine.rs`'s \
                 `register_reads`/`register_verbs`/`build_engine`) and that each call's arity and \
                 ARGUMENT TYPES match the registration exactly — rhai coerces neither."
            );
            for (symbol, side, qty) in &broker.markets {
                assert_eq!(symbol, "BTCUSDT", "template {name} must route to the bar's own symbol");
                assert!(*side == 1 || *side == -1, "template {name} sent side {side}, not ±1");
                assert!(*qty > 0.0, "template {name} sent a non-positive qty {qty}");
            }
        }
    }

    /// The proof that the gate above is not vacuous, and a live specimen of the failure it exists
    /// to catch.
    ///
    /// The SMA-cross template with its `sma(` calls rewritten to [`UNBOUND_WITNESS`] still COMPILES
    /// and still reports its `param()` knobs — both compile-only gates stay green on it — while
    /// reaching the broker exactly never.
    ///
    /// ⚠ The witness used to be `wma`, a REAL registry indicator the host did not bind. It stopped
    /// being a witness when the host widened to the registry, so it is now a name outside the
    /// registry ENTIRELY — asserted, not assumed, on both axes below. Do not restore `wma`: a
    /// bound name here makes this test pass for the wrong reason and quietly turns
    /// `every_shipped_template_reaches_the_broker` into a claim about nothing.
    #[test]
    fn an_unbound_call_compiles_and_then_reaches_the_broker_with_nothing() {
        let unbound = SMA_CROSS.replace("sma(", &format!("{UNBOUND_WITNESS}("));
        assert!(
            unbound.contains(&format!("{UNBOUND_WITNESS}(")),
            "test premise: the rewrite must have applied"
        );
        assert!(
            vike_script::discover_params(&unbound).is_ok(),
            "test premise: a call to an unbound function still COMPILES — that is the whole hazard"
        );
        assert!(
            !vike_script::RHAI_INDICATORS.contains(&UNBOUND_WITNESS),
            "test premise: the witness must stay outside the host-bound set"
        );
        assert!(
            vike_indicators::get(UNBOUND_WITNESS).is_none(),
            "test premise: the witness must not be a registry indicator either"
        );

        let mut strat = RhaiStrategy::<MockBroker>::compile(&unbound).expect("still compiles");
        let mut broker = MockBroker::default();
        for bar in rising_bars(80) {
            broker.px = bar.close;
            strat.on_bar(&mut broker, &bar);
        }
        assert!(
            broker.markets.is_empty(),
            "a script calling an unbound host function must reach the broker with NOTHING — \
             otherwise `every_shipped_template_reaches_the_broker` could not detect one"
        );
    }

    /// A minimal, VALID sweep profile (shape errors must never be what a connect test trips on).
    const SWEEP_PROFILE: &str = "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\nfrom = \"0\"\nto = \"100000\"\n[strategy]\nname = \"buy_hold\"\n[sweep]\nfast = [5, 10]\n";

    /// The tool's OWN argument checks still fire before any connect. Profile-SHAPE errors (no
    /// `[sweep]`/`[walkforward]` table, bad range, unknown strategy) deliberately moved SERVER-side
    /// when these tools started shipping the TOML verbatim — one profile parser in the workspace,
    /// one error source; the server's `run_sweep_profile` arm answers them (pinned by
    /// `vike-datahub`'s `run_sweep_walkforward_profile_roundtrip` test).
    #[test]
    fn run_tools_reject_a_missing_profile_arg_before_any_connect() {
        for tool in ["run_sweep", "run_walk_forward"] {
            let resp = call(tool, json!({}));
            assert_eq!(resp["result"]["isError"], true, "{tool}");
            let text = resp["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("profile"), "{tool}: {text}");
        }
        // An unparsable `script` injection is likewise a local error (it rewrites the TOML here).
        let resp = call("run_sweep", json!({ "profile": "this is [not valid", "script": "x" }));
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn remote_run_tools_without_a_reachable_datahub_are_clean_errors() {
        // Point at a port nothing is listening on (`127.0.0.1:1` refuses immediately): each remote
        // tool's connect failure must be a clean tool error, never a panic — same pin as
        // `list_strategies_without_a_reachable_datahub_is_a_clean_error`.
        let mut s = Server {
            datahub_addr: "127.0.0.1:1".to_string(),
            node_addr: None,
            control: None,
            observe: None,
            caps: verbs::GuardrailCaps::default(),
            keys: NodeKeyring::default(),
            coids: verbs::coid_minter(),
        };
        let wf_profile = format!("{SWEEP_PROFILE}[walkforward]\nn_splits = 4\n");
        for (tool, args) in [
            ("run_sweep", json!({ "profile": SWEEP_PROFILE })),
            ("run_walk_forward", json!({ "profile": wf_profile })),
            ("list_series", json!({})),
        ] {
            let resp = s
                .handle(&req(1, "tools/call", json!({ "name": tool, "arguments": args })))
                .unwrap();
            assert_eq!(resp["result"]["isError"], true, "{tool} must error, not panic");
            let text = resp["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("cannot connect"), "{tool}: {text}");
        }
    }

    #[test]
    fn modify_without_confirm_only_previews_the_shared_wire_command() {
        // The verbs-module construction site: the SAME WireCommand::Modify the trade REPL's
        // `modify c-1 --qty 2` builds, preview-gated like every other write tool.
        let resp = call("modify", json!({ "client_order_id": "c-1", "new_qty": 2.0 }));
        assert_eq!(resp["result"]["isError"], false, "a preview succeeds even with no node");
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], false);
        assert_eq!(sc["wire_command"]["Modify"]["client_order_id"], "c-1");
        assert_eq!(sc["wire_command"]["Modify"]["new_qty"], 2.0);
        assert_eq!(sc["wire_command"]["Modify"]["new_price"], Value::Null);
        assert_eq!(sc["guardrail"]["within_limits"], true, "no order size to check");
    }

    #[test]
    fn modify_with_nothing_to_change_is_a_tool_error() {
        let resp = call("modify", json!({ "client_order_id": "c-1" }));
        assert_eq!(resp["result"]["isError"], true);
        assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("new_qty"));
    }

    #[test]
    fn mass_cancel_preview_allows_an_omitted_scope() {
        let resp = call("mass_cancel", json!({}));
        assert_eq!(resp["result"]["isError"], false);
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["will_execute"], false);
        assert_eq!(sc["wire_command"]["MassCancel"]["venue"], Value::Null);
        assert_eq!(sc["wire_command"]["MassCancel"]["symbol"], Value::Null);
    }

    #[test]
    fn mass_cancel_with_confirm_but_no_node_is_a_clean_error() {
        let resp = call("mass_cancel", json!({ "venue": "sim", "confirm": true }));
        assert_eq!(resp["result"]["isError"], true, "confirm without a node must error, not panic");
    }
}
