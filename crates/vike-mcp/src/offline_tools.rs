//! The OFFLINE strategy-authoring tools — `validate_strategy`, `list_templates`, `discover_params`
//! and `list_indicators`.
//!
//! None of them takes `&Server`: each is a pure `vike_rhai` / `vike_indicators` call that opens no
//! socket, which is exactly what `annotations.openWorldHint == false` declares in `tools_spec` and
//! what the `offline` profile ring is derived from. Split out of `src/lib.rs` (code-layout phase 2,
//! task 9); `Server::call_tool` routes to them by name.
//!
//! `decode_order_id` is pure too and answers with no server, but `Server::call_tool` hands it what the
//! node gave ([`DecodeNode`]: the frame, and the mount rows for an id naming a strategy), so its spec
//! says `openWorldHint: true`. It declares `Row::offline_too`: the `offline` ring serves it reading no
//! node, advertised there with `openWorldHint: false`.

use std::sync::Arc;

use serde_json::{Value, json};

use vike_client::indicators;
use vike_model::orders::client_id::{
    OrderClientIdReading, OrderClientIdStrategyCode, parse_order_client_id,
};
use vike_tradehub_client::wire::{CoreSnapshotWire, MountViewWire};

// ---- pure tool implementations (no `self` / no network) --------------------------------------

/// Compile-check a Rhai strategy OFFLINE (the absorbed `vike-mcp` `validate_strategy` tool —
/// compile IS validation). Goes through `vike_rhai::discover_params`, the broker-free public
/// compile path: it compiles the script and runs its top level exactly once — the SAME error
/// surface as mounting the strategy (`RhaiStrategy::compile` does the same one-time top-level
/// run). A bad script is an `ok:false` ANSWER (the agent reads the error and fixes the script),
/// never a tool error; only a missing argument is.
pub(super) fn tool_validate_strategy(args: &Value) -> Result<Value, String> {
    let script = args
        .get("script")
        .and_then(Value::as_str)
        .ok_or("validate_strategy requires a `script` string argument")?;
    Ok(match vike_rhai::discover_params(script) {
        Ok(_) => json!({ "ok": true }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    })
}

/// The starter Rhai strategies, as `{name, code}` rows — the absorbed `vike-mcp` `list_templates`
/// tool. The sources are read from `vike_rhai::TEMPLATES` — the ONE source since 2026-09-18,
/// where this crate kept a verbatim hand copy before. Each is
/// parameterized via `param()` so it drops straight into `run_sweep`, and each is pinned by two
/// tests below — one that COMPILES it, and one that EXECUTES it over a broker double and asserts
/// an order arrives. The second exists because an agent COPIES this list, and a script whose
/// every call sits inside `fn on_bar()` compiles perfectly while failing on every single bar.
pub(super) fn tool_list_templates() -> Value {
    let templates: Vec<Value> = vike_rhai::TEMPLATES
        .iter()
        .map(|(name, code)| json!({ "name": name, "code": code }))
        .collect();
    json!({ "templates": templates })
}

pub(super) fn tool_discover_params(args: &Value) -> Result<Value, String> {
    let script = args
        .get("script")
        .and_then(Value::as_str)
        .ok_or("discover_params requires a `script` string argument")?;
    let params =
        vike_rhai::discover_params(script).map_err(|e| format!("rhai compile error: {e}"))?;
    let params: Vec<Value> = params
        .into_iter()
        .map(|(name, default)| json!({ "name": name, "default": default }))
        .collect();
    Ok(json!({ "params": params }))
}

/// The HOST-BOUND callable set — `vike_rhai::is_callable`, what
/// `crates/vike-rhai/src/engine/builtin.rs`'s `register_indicators` actually registers under a bare name
/// OR a per-line accessor (`bollinger` has no bare call and three accessors) — joined onto its
/// `vike_indicators::registry()` metadata. NEVER the registry itself: a script calling a name the
/// host does not bind hits a function-not-found error every bar and self-disables, so a roster that
/// over-advertises hands an agent names that produce a strategy which looks mounted and never
/// trades. The filter and the rows the human `vike-cli indicators` listing renders are all
/// `crates/vike-client/src/indicators.rs`'s (`bound_metas`/`detail_row`/`compact_row`), so the two
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
pub(super) fn tool_list_indicators(args: &Value) -> Result<Value, String> {
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

/// `decode_order_id`'s one argument. Only a missing id is a tool error: an id this platform did not
/// mint is an answer (`form: "other"`).
pub(super) fn decode_order_id_arg(args: &Value) -> Result<&str, String> {
    args.get("order_client_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "decode_order_id requires an `order_client_id` string argument".to_string())
}

/// Whether `id` names a strategy (a v2 id whose code is not `0000`): the one name the node's mount
/// rows give, so the one id worth reading them for.
pub(super) fn names_a_strategy(id: &str) -> bool {
    matches!(parse_order_client_id(id), Some(OrderClientIdReading::V2(p)) if p.strat != OrderClientIdStrategyCode::NONE)
}

/// What a decode had from the node; the answer's `node` field names the variant.
pub(super) enum DecodeNode {
    /// `offline`: the `offline` ring reads no node.
    Offline,
    /// `none`: no `--node`.
    NoNode,
    /// `read`: the node's frame, and its mount rows when the id names a strategy (none otherwise).
    Read(Arc<CoreSnapshotWire>, Vec<MountViewWire>),
    /// `unreachable`: a read it needed failed (`node_error`) and the names it gives are null; the
    /// frame's names stay when only the mount rows failed.
    Unreachable(Option<Arc<CoreSnapshotWire>>, String),
}

/// What an order client id says (decision 0270's MCP decode surface, the twin of `vike-cli trade order
/// decode`): `vike_client::verbs::decode_order_client_id`'s answer, the codes and the words with the names `node`
/// gave (the account's label, the strategy behind a mount's code, the group's legs resting now), plus
/// `node`, so a null name is never read as one the node does not know when no node was asked.
pub(super) fn tool_decode_order_id(id: &str, node: DecodeNode) -> Value {
    let (frame, mounts, said, why) = match node {
        DecodeNode::Offline => (None, Vec::new(), "offline", None),
        DecodeNode::NoNode => (None, Vec::new(), "none", None),
        DecodeNode::Read(frame, mounts) => (Some(frame), mounts, "read", None),
        DecodeNode::Unreachable(frame, why) => (frame, Vec::new(), "unreachable", Some(why)),
    };
    let mut answer = vike_client::verbs::decode_order_client_id(id, frame.as_deref(), &mounts);
    answer["node"] = json!(said);
    if let Some(why) = why {
        answer["node_error"] = json!(why);
    }
    answer
}
