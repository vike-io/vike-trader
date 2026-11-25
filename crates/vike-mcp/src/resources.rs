//! The MCP resources (each a second way in to a tool, but the compiled-in refusal catalog), and the per-call
//! agent-transcript record.

use serde_json::{Value, json};

#[cfg(doc)]
use super::protocol::tool_ok;
use super::scope::ToolAccess;
use super::trace::{self, TOKEN_MINTED, TOKEN_PRESENTED, Verdict};
use super::{Server, ToolError, is_write_tool};

/// The `resources/list` payload.
///
/// A resource is a READ the agent does not have to choose a tool for: the same data
/// `node_snapshot` and `run_backtest` return, addressable by URI so a client can pin it into a
/// conversation (or re-read it) without a tool call. Both are DERIVED — [`Server::read_resource`]
/// routes each one into the tool's own implementation, so a resource can never answer differently
/// from the tool beside it.
///
/// ⚠ Neither is a write, and no resource ever will be. `resources/read` has no preview gate and no
/// `confirm` argument, because there is nothing here to confirm; a URI that changed node state
/// would be a write with the whole gate routed around it. That is why `read-only` withholds neither
/// of them — and why `offline` withholds BOTH, since both reach a tool that opens a socket.
///
/// The third, `vike://refusals`, reaches no tool: it is [`crate::REFUSAL_CATALOG`], what this build carries, so
/// every profile serves it ([`UNSCOPED_RESOURCES`]).
pub(super) fn resources_spec() -> Value {
    json!([
        {
            "uri": "vike://node/snapshot",
            "name": "Node snapshot",
            "description": "The live vike-tradehub's orders, positions, per-venue equity and recent events — the same payload the node_snapshot tool returns, `bars` left out. ⚠ Top-level `positions`, `balance` and `trading_state` are the PRIMARY engine's alone and `equity_total` covers only its currency group, so on a node with several accounts read `venues[].positions` before calling anything flat. Requires --node and this box's device key on the node's access list (role observe or higher); reading it is what opens the connection.",
            "mimeType": "application/json"
        },
        {
            "uri": "vike://backtest/last",
            "name": "Last backtest report",
            "description": "Whatever the run_backtest tool last returned IN THIS SESSION. Per-process: there is no report store, so a fresh session serves an error rather than an older run's report.",
            "mimeType": "application/json"
        },
        {
            "uri": REFUSAL_CATALOG_URI,
            "name": "Refusal catalog",
            "description": "Every refusal id this build can show (the `[id]` of an `error [id]: text` answer), with its text template and the crate that declares it: the release's refusals.json. Compiled in, so it opens no connection.",
            "mimeType": "application/json"
        }
    ])
}

/// The refusal catalog's URI: [`crate::REFUSAL_CATALOG`], compiled in.
const REFUSAL_CATALOG_URI: &str = "vike://refusals";

/// The resources that are a second way in to NO tool, served under every profile: each reads only what this build
/// carries, so no profile has anything to withhold. `every_resource_is_a_way_in_to_a_named_tool` holds every
/// advertised resource to exactly one of this list and [`RESOURCE_TOOLS`].
pub(super) const UNSCOPED_RESOURCES: [&str; 1] = [REFUSAL_CATALOG_URI];

/// WHICH TOOL each resource is a second way in to — the mapping that lets one scope gate cover both
/// surfaces.
///
/// ⚠ It is a table rather than a field on the spec because it states a fact about the
/// IMPLEMENTATION ([`Server::read_resource`]'s arms), not about the advertisement, and the two must
/// be compared rather than assumed equal: `every_resource_is_a_way_in_to_a_named_tool` walks
/// [`resources_spec`] against this in both directions, so a third resource added with no row here
/// reddens rather than quietly escaping the profile.
pub(super) const RESOURCE_TOOLS: [(&str, &str); 2] =
    [("vike://node/snapshot", "node_snapshot"), ("vike://backtest/last", "run_backtest")];

/// The `resources/list` payload for one [`ToolAccess`] — [`resources_spec`] minus every resource
/// whose tool this session withholds, and minus any on neither list (none is:
/// `every_resource_is_a_way_in_to_a_named_tool`).
pub(super) fn resources_spec_for(access: &ToolAccess) -> Value {
    let spec = resources_spec();
    let kept: Vec<Value> = spec
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|r| {
            let uri = r["uri"].as_str().unwrap_or_default();
            UNSCOPED_RESOURCES.contains(&uri)
                || RESOURCE_TOOLS
                    .iter()
                    .find(|(u, _)| *u == uri)
                    .is_some_and(|(_, tool)| access.admits(tool))
        })
        .cloned()
        .collect();
    Value::Array(kept)
}

/// What a write's answer says when it did not plainly happen: an outcome other than `accepted` (the
/// command may still execute), or a delete that left series behind.
fn unsettled_write(answer: &Value) -> Option<String> {
    if let Some(outcome) = answer["outcome"].as_str().filter(|o| *o != "accepted") {
        let note = answer["note"].as_str().unwrap_or_default();
        return Some(format!("outcome {outcome}: {note}"));
    }
    let failed = answer["failed"].as_u64().filter(|n| *n > 0)?;
    let (deleted, failures) = (&answer["deleted"], &answer["failures"]);
    Some(format!("partial delete, {failed} series failed and {deleted} went: {failures}"))
}

/// The one rendering of a structured payload into the `text` a client displays — shared by
/// [`tool_ok`] and [`Server::read_resource`] so a resource and its tool cannot present the same
/// document differently.
pub(super) fn render(structured: &Value) -> String {
    serde_json::to_string_pretty(structured).unwrap_or_default()
}

impl Server {
    /// Append ONE `tools/call` to the agent transcript, if one was asked for.
    ///
    /// ⚠ **Everything here is DERIVED from the call and its outcome** — no state is carried between
    /// calls and nothing is re-executed to find out what happened, so the record cannot describe a
    /// different call from the one the client was answered with. The classification:
    ///
    ///   * a write tool's mandatory preview is [`Verdict::Preview`], never `Ok` — "the agent called
    ///     submit_order" and "an order left this process" are different facts;
    ///   * a GATE saying no is [`Verdict::Refused`], read off [`ToolError::refused`] rather than
    ///     sniffed out of the message, because a reworded sentence must not silently reclassify;
    ///   * a panic is an [`Verdict::Error`] like any other failure — the session survives it, and so
    ///     does the record;
    ///   * a write the tool answered for and that did not plainly happen stays [`Verdict::Ok`], the
    ///     tool did answer, and says what it said in `detail` ([`unsettled_write`]).
    ///
    /// The token identity is [`TOKEN_MINTED`] when the preview issued it and [`TOKEN_PRESENTED`]
    /// when the call supplied one; that constant's own doc argues why "spent" would be a claim this
    /// record cannot make.
    ///
    /// A failed append goes to **stderr**. It is not fatal — a server that stopped answering an
    /// agent because it could not write a log line would be trading availability for bookkeeping —
    /// and it is not silent either, which is the failure mode that makes an empty transcript
    /// indistinguishable from a quiet night.
    pub(super) fn trace_call(
        &self,
        name: &str,
        args: &Value,
        outcome: &std::thread::Result<Result<Value, ToolError>>,
    ) {
        let Some(trace) = &self.trace else { return };
        let raw_token = args.get("preview_token").and_then(Value::as_str);
        let presented = raw_token.map(trace::presented_token);
        // A gate's refusal quotes the token it was handed, for the client. The record quotes the
        // bounded form: `detail` is not redacted.
        let detail_of = |e: &ToolError| match raw_token {
            Some(raw) => {
                let form = trace::presented_token(raw);
                e.message.replace(&format!("{raw:?}"), &format!("{form:?}"))
            }
            None => e.message.clone(),
        };
        let (verdict, detail, token, token_role) = match outcome {
            Ok(Ok(structured)) if structured["will_execute"] == json!(false) => (
                Verdict::Preview,
                None,
                structured["preview_token"].as_str().map(str::to_string),
                Some(TOKEN_MINTED),
            ),
            Ok(Ok(structured)) => {
                let detail = is_write_tool(name).then(|| unsettled_write(structured)).flatten();
                (Verdict::Ok, detail, presented, Some(TOKEN_PRESENTED))
            }
            Ok(Err(e)) if e.refused => {
                (Verdict::Refused, Some(detail_of(e)), presented, Some(TOKEN_PRESENTED))
            }
            Ok(Err(e)) => (Verdict::Error, Some(detail_of(e)), presented, Some(TOKEN_PRESENTED)),
            Err(_) => (
                Verdict::Error,
                Some(format!("tool panicked: {name}")),
                presented,
                Some(TOKEN_PRESENTED),
            ),
        };
        // A call that neither minted nor presented a token names neither — `"token": null` beside a
        // `"token_role": null` reads as "not a gated write", which is what it is.
        let (token, token_role) = match token {
            Some(t) => (Some(t), token_role),
            None => (None, None),
        };
        let call = trace::Call {
            tool: name,
            write: is_write_tool(name),
            profile: self.access.profile_name(),
            args,
            verdict,
            detail,
            token,
            token_role,
        };
        if let Err(e) = trace.append(vike_model::now_ms(), &call) {
            eprintln!("vike-mcp: the agent transcript was NOT written: {e}");
        }
    }

    /// Serve one resource by URI.
    ///
    /// ⚠ Every arm here goes through the SAME implementation the matching tool uses — the node
    /// snapshot through [`Server::tool_node_snapshot`], the report through whatever `run_backtest`
    /// last returned. A resource is a second WAY IN, never a second source: a hand-rolled fetch
    /// beside the tool's would be free to disagree with it, and an agent reading both would have no
    /// way to tell which one was lying.
    ///
    /// `&mut self` rather than the `&self` a read suggests, because the snapshot arm opens the
    /// observe connection lazily — reading is the thing that connects.
    pub(super) fn read_resource(&mut self, uri: &str) -> Result<String, String> {
        // ⚠ THE SAME SCOPE GATE `call_tool` applies, and for the same reason a resource routes into
        // a tool's implementation at all: this is a second WAY IN, so a resource served while its
        // tool is withheld would hand back exactly the answer the profile refused. [`RESOURCE_TOOLS`]
        // is the mapping, held equal to [`resources_spec`] by
        // `every_resource_is_a_way_in_to_a_named_tool`.
        if let Some((_, tool)) = RESOURCE_TOOLS.iter().find(|(u, _)| *u == uri)
            && !self.access.admits(tool)
        {
            return Err(self.access.refusal(tool));
        }
        match uri {
            "vike://node/snapshot" => self.tool_node_snapshot().map(|snap| render(&snap)),
            // ABSENT is an error, not an empty document: an agent handed `{}` would summarise it as
            // a backtest that produced nothing, which is a different claim from "none has run".
            "vike://backtest/last" => self.last_backtest.clone().ok_or_else(|| {
                "no backtest has run in this session yet — call the run_backtest tool first. This \
                 resource is per-process: there is no report store, so it is empty in a fresh \
                 session even if a backtest ran in an earlier one."
                    .to_string()
            }),
            REFUSAL_CATALOG_URI => Ok(crate::REFUSAL_CATALOG.to_string()),
            // A test-only URI that panics, pinning the catch_unwind guard on `resources/read` the
            // way `__test_panic` pins the one on `tools/call`. Never in `resources_spec`.
            #[cfg(test)]
            "vike://__test_panic" => panic!("kaboom"),
            other => Err(format!(
                "unknown resource uri: {other:?} — call resources/list for what this server serves"
            )),
        }
    }
}
