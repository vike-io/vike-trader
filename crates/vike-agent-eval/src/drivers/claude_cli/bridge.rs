//! The harness side of the MCP bridge: the descriptor, the `--mcp-config`, the tiny server and the `mcp-bridge` byte pump.

use std::io::Write;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::Path;

use serde_json::{Value, json};

use super::{
    BRIDGE_CONNECT_TIMEOUT, BUDGET_EXHAUSTED, MCP_SERVER_NAME, PROTOCOL_VERSION, SERVER_INFO_NAME,
};
use crate::drivers::ToolChannel;
use crate::world::mcp::ToolResult;

/// Where the bridge finds the harness, written by the harness into the case's own work directory.
///
/// ⚠ The nonce is a HANDSHAKE, not a secret. It exists so a stray local process that happens to
/// connect to the loopback port is refused rather than served the trading tools; it does not defend
/// against anything that can read the case's work directory, and it is not asked to — the tools on
/// the far side reach one throwaway PAPER node whose keys were minted for that case alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bridge {
    /// `127.0.0.1:<port>` — always loopback, never a routable address.
    pub addr: String,
    pub nonce: String,
}

impl Bridge {
    pub fn to_json(&self) -> Value {
        json!({ "addr": self.addr, "nonce": self.nonce })
    }

    pub fn from_json(v: &Value) -> Result<Self, String> {
        let addr = v["addr"].as_str().ok_or("the bridge descriptor names no `addr`")?;
        let nonce = v["nonce"].as_str().ok_or("the bridge descriptor names no `nonce`")?;
        if !addr.starts_with("127.0.0.1:") {
            return Err(format!(
                "the bridge descriptor names {addr:?}, which is not a loopback address — this \
                 bridge connects to nothing else"
            ));
        }
        Ok(Self { addr: addr.to_string(), nonce: nonce.to_string() })
    }
}

/// The `--mcp-config` document that points the CLI at this harness.
///
/// One stdio server, whose command is THIS binary re-entered as `mcp-bridge`. No `env` map: the
/// bridge takes its one input as an argv PATH, so nothing about the harness's whereabouts becomes
/// an environment variable a settings-registry row would then have to exist for.
pub fn mcp_config(bridge_exe: &Path, descriptor: &Path) -> Value {
    let mut servers = serde_json::Map::new();
    servers.insert(
        MCP_SERVER_NAME.to_string(),
        json!({
            "type": "stdio",
            "command": bridge_exe.display().to_string(),
            "args": ["mcp-bridge", descriptor.display().to_string()],
        }),
    );
    json!({ "mcpServers": Value::Object(servers) })
}

/// The `--allowedTools` names for one roster, DERIVED from what `tools/list` answered.
///
/// ⚠ Every roster tool must be named. `--tools ""` removes the built-in tools but does NOT filter
/// the MCP roster (measured: a run naming one tool still exposed both), and `--allowedTools` is a
/// PERMISSION list — with `--permission-prompts none` an unnamed tool is denied automatically, so a
/// hand-written subset would silently evaluate the agent against a smaller surface than the server
/// advertises.
pub fn allowed_tool_names(tools: &[Value]) -> Vec<String> {
    tools
        .iter()
        .filter_map(|t| t["name"].as_str())
        .map(|name| format!("mcp__{MCP_SERVER_NAME}__{name}"))
        .collect()
}

/// What one answer to the CLI's MCP client is: a line to write back, or nothing at all.
///
/// ⚠ `PartialEq` only — `serde_json::Value` is not `Eq` (it carries `f64`), so a derived `Eq` here
/// would not compile.
#[derive(Debug, Clone, PartialEq)]
pub enum Served {
    Answer(Value),
    /// A notification (no `id`) takes no answer. Answering one is a protocol error.
    Nothing,
}

fn rpc_ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_err(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// One `tools/call` answer in the MCP envelope, from what the real server said.
///
/// ⚠ The server's own text is passed through VERBATIM, `isError` included — the same rule
/// `crates/vike-agent-eval/src/drivers/anthropic.rs`'s `tool_result_block` obeys, for the same reason: the
/// MCP refusals are sentences written for an agent to act on, and a harness that paraphrased one
/// would be evaluating its paraphrase instead of the shipped surface. `structuredContent` is
/// omitted rather than sent as `null` when the answer carried none, because the field is specified
/// as an object.
pub fn tool_envelope(result: &ToolResult) -> Value {
    let mut out = json!({
        "content": [{ "type": "text", "text": result.text }],
        "isError": result.is_error,
    });
    if result.structured.is_object() {
        out["structuredContent"] = result.structured.clone();
    }
    out
}

/// Answer one JSON-RPC message from the CLI's MCP client.
///
/// This is the whole server the CLI sees, and it is deliberately tiny: it advertises the tools
/// capability and nothing else, so the client asks for nothing else. `Err` is reserved for the ONE
/// thing that must abort the drive — the real `vike-cli mcp` server on the far side of the channel
/// being gone. A tool that answered `isError` is not that: it is information the agent must read.
pub fn serve_rpc(
    msg: &Value,
    tools: &[Value],
    instructions: &str,
    channel: &mut dyn ToolChannel,
    calls_left: &mut usize,
) -> Result<Served, String> {
    let Some(id) = msg.get("id").filter(|v| !v.is_null()).cloned() else {
        return Ok(Served::Nothing);
    };
    match msg["method"].as_str().unwrap_or_default() {
        // ⚠ The real server's `instructions` are FORWARDED here, and this is the one place the
        // bridge may not be a tiny server of its own invention. The field is part of the surface
        // under test (`crates/vike-cli/src/cmd/mcp.rs`'s `instructions`), and this handshake is the
        // only route by which a REAL MCP client — the Claude Code CLI — can be handed it. A bridge
        // that answered with its own empty handshake would silently withhold a shipped surface from
        // exactly the driver that exists to measure how a real client treats it. OMITTED rather
        // than sent empty when the server served none: the field is optional and an empty string is
        // not the same claim as an absent one.
        "initialize" => {
            let mut result = json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": SERVER_INFO_NAME, "version": env!("CARGO_PKG_VERSION") },
            });
            if !instructions.is_empty() {
                result["instructions"] = json!(instructions);
            }
            Ok(Served::Answer(rpc_ok(&id, result)))
        }
        "ping" => Ok(Served::Answer(rpc_ok(&id, json!({})))),
        // ⚠ The roster VERBATIM, `annotations` and all. The tool descriptions ARE the surface under
        // test, and the write-refusal checks read `destructiveHint` off this same roster.
        "tools/list" => Ok(Served::Answer(rpc_ok(&id, json!({ "tools": tools })))),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or_default().to_string();
            let args = match msg["params"]["arguments"].clone() {
                Value::Null => json!({}),
                other => other,
            };
            if *calls_left == 0 {
                let refusal = ToolResult {
                    is_error: true,
                    text: BUDGET_EXHAUSTED.to_string(),
                    structured: Value::Null,
                };
                return Ok(Served::Answer(rpc_ok(&id, tool_envelope(&refusal))));
            }
            *calls_left -= 1;
            // A tool the server does not advertise still goes THROUGH the server, which answers
            // `unknown tool` — the same choice the API driver makes, because "the agent called a
            // tool that does not exist" is a finding the transcript must carry.
            let result = channel.call(&name, &args)?;
            Ok(Served::Answer(rpc_ok(&id, tool_envelope(&result))))
        }
        other => Ok(Served::Answer(rpc_err(
            &id,
            -32601,
            &format!(
                "this harness's MCP bridge serves initialize, ping, tools/list and tools/call; it \
                 was asked for {other:?}"
            ),
        ))),
    }
}

/// The `mcp-bridge` mode: a byte pump between the CLI's stdio and the harness's loopback socket.
///
/// It holds no state and makes no decision — every JSON-RPC frame is forwarded unread, so the
/// protocol lives in exactly one place ([`serve_rpc`], in the harness) and this half cannot answer
/// differently. It exists only because `--mcp-config` can name a command to spawn or a URL to
/// fetch, and this crate may not add an HTTP server to be the URL.
pub fn run_bridge(descriptor: &Path) -> Result<(), String> {
    let raw = std::fs::read_to_string(descriptor)
        .map_err(|e| format!("read the bridge descriptor {}: {e}", descriptor.display()))?;
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("the bridge descriptor {} is not JSON: {e}", descriptor.display()))?;
    let bridge = Bridge::from_json(&parsed)?;
    let addr: SocketAddr = bridge
        .addr
        .parse()
        .map_err(|e| format!("the bridge descriptor's addr {:?} is not one: {e}", bridge.addr))?;
    let stream = TcpStream::connect_timeout(&addr, BRIDGE_CONNECT_TIMEOUT)
        .map_err(|e| format!("connect the MCP bridge to {addr}: {e}"))?;
    let mut up = stream.try_clone().map_err(|e| format!("split the MCP bridge connection: {e}"))?;
    // The handshake line, first and once: the harness serves nothing until it matches.
    up.write_all(format!("{}\n", json!({ "nonce": bridge.nonce })).as_bytes())
        .and_then(|()| up.flush())
        .map_err(|e| format!("greet the harness over the MCP bridge: {e}"))?;

    // CLI stdin -> harness, on its own thread; harness -> CLI stdout, here. Each half ends when its
    // source reaches EOF, and the process ends with the second — which is what the CLI's own
    // shutdown of its MCP servers produces.
    std::thread::Builder::new()
        .name("eval-cli-in".into())
        .spawn(move || {
            let mut stdin = std::io::stdin();
            let _ = std::io::copy(&mut stdin, &mut up);
            let _ = up.shutdown(Shutdown::Write);
        })
        .map_err(|e| format!("spawn the MCP bridge stdin thread: {e}"))?;
    let mut down = stream;
    let mut stdout = std::io::stdout();
    std::io::copy(&mut down, &mut stdout)
        .map_err(|e| format!("forward the harness's answers to the CLI: {e}"))?;
    stdout.flush().map_err(|e| format!("flush the MCP bridge's output: {e}"))?;
    Ok(())
}
