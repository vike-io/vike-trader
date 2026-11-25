//! The OFFLINE strategy-authoring tools — `validate_strategy`, `list_templates`, `discover_params`,
//! `list_indicators` and `profile_template`.
//!
//! None of them takes `&Server`: each is a pure `vike_rhai` / `vike_indicators` call, or a read of
//! the compiled-in profile schema, that opens no socket, which is exactly what
//! `annotations.openWorldHint == false` declares in `tools_spec` and what the `offline` profile ring
//! is derived from. Split out of `src/lib.rs` (code-layout phase 2,
//! task 9); `Server::call_tool` routes to them by name.
//!
//! `decode_order_id` is pure too and answers with no server, but `Server::call_tool` hands it what the
//! node gave ([`DecodeNode`]: the frame, and the mount rows for an id naming a strategy), so its spec
//! says `openWorldHint: true`. It declares `Row::offline_too`: the `offline` ring serves it reading no
//! node, advertised there with `openWorldHint: false`.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use vike_client::indicators;
use vike_client::refusals::ProfileTemplateRefusal;
use vike_tradehub_client::wire::{CoreSnapshotWire, MountViewWire};

use super::tool_args::{
    DecodeOrderIdArgs, DiscoverParamsArgs, ListIndicatorsArgs, ProfileTemplateArgs,
    ValidateStrategyArgs, parse_args,
};

// ---- pure tool implementations (no `self` / no network) --------------------------------------

/// Compile-check a Rhai strategy OFFLINE (the absorbed `vike-mcp` `validate_strategy` tool —
/// compile IS validation). Goes through `vike_rhai::discover_params`, the broker-free public
/// compile path: it compiles the script and runs its top level exactly once — the SAME error
/// surface as mounting the strategy (`RhaiStrategy::compile` does the same one-time top-level
/// run). A bad script is an `ok:false` ANSWER (the agent reads the error and fixes the script),
/// never a tool error; only a missing argument is.
pub(super) fn tool_validate_strategy(args: &Value) -> Result<Value, String> {
    let script = script_arg::<ValidateStrategyArgs>("validate_strategy", args, |a| a.script)?;
    let error = vike_rhai::discover_params(&script).err().map(|e| e.to_string());
    Ok(json!(ValidateStrategyAnswer { ok: error.is_none(), error }))
}

/// `validate_strategy`'s answer, its `outputSchema` too.
#[derive(Serialize, JsonSchema)]
pub(super) struct ValidateStrategyAnswer {
    /// true: the script compiled and its top level ran
    ok: bool,
    /// why it did not, in rhai's words
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// `list_templates`' answer, its `outputSchema` too.
#[derive(Serialize, JsonSchema)]
pub(super) struct ListTemplatesAnswer {
    /// the starter strategies, each {name, code}
    templates: Vec<Value>,
}

/// `discover_params`' answer, its `outputSchema` too.
#[derive(Serialize, JsonSchema)]
pub(super) struct DiscoverParamsAnswer {
    /// the top level's param() knobs, each {name, default}, in declaration order
    params: Vec<Value>,
}

/// `list_indicators`' answer, its `outputSchema` too.
#[derive(Serialize, JsonSchema)]
pub(super) struct ListIndicatorsAnswer {
    /// how many rows the two lists hold
    count: usize,
    /// the host-bound indicators: the compact roster, or the ones `name` or `category` asked for in full
    indicators: Vec<Value>,
    /// your own, from user_data/indicators, in full
    user_indicators: Vec<Value>,
    /// the compact roster's: how to ask for more
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<&'static str>,
}

impl ListIndicatorsAnswer {
    fn of(
        indicators: Vec<Value>,
        user_indicators: Vec<Value>,
        note: Option<&'static str>,
    ) -> Value {
        let count = indicators.len() + user_indicators.len();
        json!(Self { count, indicators, user_indicators, note })
    }
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
    json!(ListTemplatesAnswer { templates })
}

/// The `script` a compile tool takes, read into its struct `T`: absent or no text is the one refusal.
fn script_arg<T: DeserializeOwned>(
    tool: &str,
    args: &Value,
    script: impl FnOnce(T) -> Option<String>,
) -> Result<String, String> {
    let missing = format!("{tool} requires a `script` string argument");
    parse_args(args, |_, _| missing.clone()).map(script)?.ok_or(missing)
}

pub(super) fn tool_discover_params(args: &Value) -> Result<Value, String> {
    let script = script_arg::<DiscoverParamsArgs>("discover_params", args, |a| a.script)?;
    let params =
        vike_rhai::discover_params(&script).map_err(|e| format!("rhai compile error: {e}"))?;
    let params: Vec<Value> = params
        .into_iter()
        .map(|(name, default)| json!({ "name": name, "default": default }))
        .collect();
    Ok(json!(DiscoverParamsAnswer { params }))
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
    // Both read as absent when they are no text, so the parse refuses nothing.
    let args: ListIndicatorsArgs = parse_args(args, |_, _| String::new())?;
    let name = args.name.filter(|s| !s.trim().is_empty());
    let category = args.category.filter(|s| !s.trim().is_empty());
    // The user's own installed indicators (`vike_client::indicators::user_rows`, the rows `vike-cli indicators`
    // lists): callable by the same spelling as a built-in, so they are listed under their own key, searched
    // FIRST by `name`, and in no `category` (a user indicator has none).
    let users = indicators::user_rows();
    let user_payload = |rows: &[indicators::UserRow]| -> Vec<Value> {
        rows.iter().map(indicators::user_detail_row).collect()
    };
    if let Some(name) = name.as_deref() {
        if let Some(user) = users.iter().find(|u| u.name == name.trim()) {
            let user_indicators = user_payload(std::slice::from_ref(user));
            return Ok(ListIndicatorsAnswer::of(Vec::new(), user_indicators, None));
        }
        let m = indicators::find_bound(name)?;
        return Ok(ListIndicatorsAnswer::of(vec![indicators::detail_row(m)], Vec::new(), None));
    }
    let all = indicators::bound_metas();
    if let Some(category) = category.as_deref() {
        let rows = indicators::filter_by_category(&all, category)?;
        let payload: Vec<Value> = rows.iter().map(|m| indicators::detail_row(m)).collect();
        return Ok(ListIndicatorsAnswer::of(payload, Vec::new(), None));
    }
    let payload: Vec<Value> = all.iter().map(|m| indicators::compact_row(m)).collect();
    let user_indicators = user_payload(&users);
    Ok(ListIndicatorsAnswer::of(payload, user_indicators, Some(ROSTER_NOTE)))
}

/// The compact roster's `note`.
const ROSTER_NOTE: &str = "the callable roster, compact, and your own (`user_indicators`, from user_data/indicators, in full). Call again with `name` for one indicator in full (its parameters with their defaults, and the value it returns) or with `category` for a whole built-in family. A name absent from both lists is NOT callable: rhai resolves a function name when the line runs, so a script naming it compiles and then fails on every bar until the strategy switches itself off. `name` on such a name answers with the reason it is held back.";

/// The published backtest-profile schema, the release's `profile.json`, compiled in. It is rendered from the profile's
/// types (`crates/vike-backtest/src/run/profile_surface_tests.rs`'s `bless_the_committed_fixture`), so this server
/// answers what this build's backtesthub parses and writes none of it by hand.
pub(super) const PROFILE_SCHEMA: &str =
    include_str!("../../vike-backtest/tests/fixtures/profile.json");

/// `profile_template`: what a backtest profile may say, projected from [`PROFILE_SCHEMA`]. With no `table`, every
/// table with its key names (a few KB); with one, each of its keys in full, the size `list_indicators` keeps by the
/// same split. An unknown table is refused naming every table.
pub(super) fn tool_profile_template(args: &Value) -> Result<Value, String> {
    // `table` reads as absent when it is no text, so the parse refuses nothing.
    let args: ProfileTemplateArgs = parse_args(args, |_, _| String::new())?;
    let schema: Value = serde_json::from_str(PROFILE_SCHEMA)
        // not-a-refusal: the schema is the repo's own compiled-in file; an unreadable one is a build bug
        .map_err(|e| format!("this build's profile schema is not JSON: {e}"))?;
    let tables = schema["tables"].as_array().map(Vec::as_slice).unwrap_or_default();
    let keys = schema["keys"].as_array().map(Vec::as_slice).unwrap_or_default();
    let Some(name) = args.table else {
        let rows: Vec<Value> = tables
            .iter()
            .map(|table| {
                let names: Vec<&Value> = keys_of(keys, table["table"].as_str().unwrap_or_default())
                    .map(|key| &key["key"])
                    .collect();
                json!({
                    "table": table["table"],
                    "required": table["required"],
                    "array_of_tables": table["array_of_tables"],
                    "keys": names,
                })
            })
            .collect();
        return Ok(json!({
            "tables": rows,
            "note": "every table of a backtest profile TOML, \"\" the top level. Call again with `table` for each of its keys in full: type, required, default, the words it takes and what it means."
        }));
    };
    let Some(table) = tables.iter().find(|table| table["table"] == name.as_str()) else {
        let names: Vec<String> = tables
            .iter()
            .map(|table| format!("{:?}", table["table"].as_str().unwrap_or_default()))
            .collect();
        return Err(ProfileTemplateRefusal::UnknownTable { table: name, tables: names.join(", ") }
            .rendered());
    };
    let rosters = schema["rosters"].as_array().map(Vec::as_slice).unwrap_or_default();
    let rows: Vec<Value> = keys_of(keys, &name)
        .map(|key| {
            let mut row =
                json!({ "key": key["key"], "type": key["type"], "required": key["required"] });
            if key["shape"] != "scalar" {
                row["shape"] = key["shape"].clone();
            }
            if key["default"]["kind"] == "named" {
                row["default"] = key["default"]["value"].clone();
            }
            if let Some(roster) = rosters.iter().find(|roster| roster["id"] == key["roster_id"]) {
                row["values"] = roster["members"].clone();
            }
            if key["short"].is_string() {
                row["about"] = key["short"].clone();
            }
            row
        })
        .collect();
    Ok(json!({
        "table": name,
        "required": table["required"],
        "array_of_tables": table["array_of_tables"],
        "keys": rows,
    }))
}

/// The keys of one profile table, in the schema's order.
fn keys_of<'a>(keys: &'a [Value], table: &'a str) -> impl Iterator<Item = &'a Value> {
    keys.iter().filter(move |key| key["table"] == table)
}

/// `decode_order_id`'s one argument. Only a missing id is a tool error: an id this platform did not
/// mint is an answer (`form: "other"`).
pub(super) fn decode_order_id_arg(args: &Value) -> Result<String, String> {
    let missing = "decode_order_id requires an `order_client_id` string argument";
    let args: DecodeOrderIdArgs = parse_args(args, |_, _| missing.to_string())?;
    args.order_client_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| missing.to_string())
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
