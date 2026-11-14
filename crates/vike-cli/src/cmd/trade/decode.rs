//! `vike-cli trade order decode <ID> [--node <HOST:PORT>] [--json]`: what an order client id says, in words (decision
//! 0270's decode surfaces; the MCP tool `decode_order_id` is its twin). Every field and the words are
//! `vike_client::verbs::decode_order_client_id`'s, so the two doors cannot read one id two ways; this verb adds only where the
//! CLI finds names:
//!
//! - **No `--node`**: the codes and words from the id alone. No dial, no key.
//! - **`--node`**: one observe read of the node's frame (the account's label, the legs of its group resting now) and,
//!   only for an id whose strategy code is not `0000`, its mount rows (the strategy behind the code). An unreachable
//!   node is that read's own refusal, as `order ls` gives it; mount rows that cannot be read leave the strategy to the
//!   local store.
//! - **This box's vike.db**: a strategy code the node did not name is looked up in the `strategy_code` rows
//!   (`vike_secrets::strategy_code::strategy_name_for`), READ-ONLY: a box with no store reads as no row and none is
//!   created, and a store that cannot be read is said on stderr and leaves the name out (a name decides nothing).
//!
//! A v1 id says only that it was minted before v2; any other text is "not a form this platform mints". Exit 0 either
//! way: the answer is the decode.

use std::path::Path;
use std::process::ExitCode;

use serde_json::Value;
use vike_client::hubkeys::HubKeyring;
use vike_client::verbs;
use vike_model::orders::client_id::{
    OrderClientIdReading, OrderClientIdStrategyCode, describe_order_client_id,
    parse_order_client_id,
};
use vike_tradehub_client::wire::{CoreSnapshotWire, MountViewWire};

const COMMAND: &str = "trade order decode";

pub(super) const PROSE: &str = "\
What an order client id says, in words: who placed it (G GUI, C CLI, T Telegram, M an AI agent,
S a strategy, E the engine itself), the strategy and the account it carries, what it does
(opens, closes with reduce-only, or closes with a plain order that may open the other side), its
leg of a bracket or order list, and its replacement number. An id minted before v2 says only
that; any other text is not a form this platform mints.

With no --node it reads the id alone and dials nothing. --node <HOST:PORT> (the observe address)
adds what only the node knows: the account's label, the strategy behind a code, and the other legs
of its group resting now. A strategy code the node does not name is looked up in this box's
settings database (the strategy_code rows), read-only.

--json prints one JSON document, the fields the MCP tool decode_order_id answers with.";

#[derive(clap::Args, Debug)]
pub(super) struct DecodeCommand {
    #[arg(value_name = "ID", help = "the order client id to read")]
    id: String,
    #[arg(
        long,
        value_name = "HOST:PORT",
        overrides_with = "node",
        help = "the node's observe address: adds the names it gives (optional)"
    )]
    node: Option<String>,
    #[arg(long, overrides_with = "json", help = "print the answer as one JSON document")]
    json: bool,
}

pub(super) fn run(c: DecodeCommand, keys: &HubKeyring, settings_dir: Option<&Path>) -> ExitCode {
    let (frame, mounts) = match c.node.as_deref() {
        None => (None, Vec::new()),
        Some(node) => match crate::cmd::trade::observe_read(COMMAND, node, keys) {
            Ok(frame) if names_a_strategy(&c.id) => (Some(frame), mount_rows(node, keys)),
            Ok(frame) => (Some(frame), Vec::new()),
            Err(exit) => return exit,
        },
    };
    let doc = decode(&c.id, frame.as_deref(), &mounts, settings_dir);
    if c.json {
        println!("{}", serde_json::to_string_pretty(&doc).expect("a Value always serialises"));
    } else {
        println!("{}", text(&doc));
    }
    ExitCode::SUCCESS
}

/// Whether `id` carries a strategy code other than `0000`, the one thing the node's mount rows can name: no other id
/// costs that second read.
fn names_a_strategy(id: &str) -> bool {
    matches!(parse_order_client_id(id), Some(OrderClientIdReading::V2(p)) if p.strat != OrderClientIdStrategyCode::NONE)
}

/// The node's mount rows, for the strategy behind a code; none when they cannot be read.
fn mount_rows(node: &str, keys: &HubKeyring) -> Vec<MountViewWire> {
    keys.observe()
        .and_then(|(key, _)| {
            vike_tradehub_client::strategy_status_with_features(node, key.as_bytes()).ok()
        })
        .map(|(status, _)| status.mounts)
        .unwrap_or_default()
}

/// [`verbs::decode_order_client_id`]'s answer, with the strategy named from this box's `strategy_code` rows when the node named
/// none. PURE but for that read-only lookup.
pub(super) fn decode(
    id: &str,
    frame: Option<&CoreSnapshotWire>,
    mounts: &[MountViewWire],
    settings_dir: Option<&Path>,
) -> Value {
    let mut doc = verbs::decode_order_client_id(id, frame, mounts);
    let Some(code) = doc["strategy"].as_str().and_then(OrderClientIdStrategyCode::parse) else {
        return doc;
    };
    let Some(dir) = settings_dir.filter(|_| doc["strategy_name"].is_null()) else {
        return doc;
    };
    let row = match vike_secrets::strategy_code::strategy_name_for(dir, code.id()) {
        Ok(Some(row)) => row,
        Ok(None) => return doc,
        Err(e) => {
            eprintln!(
                "vike-cli {COMMAND}: the local strategy_code rows could not be read ({e}); the strategy \
                 is named by its code alone"
            );
            return doc;
        }
    };
    // The words again, with the name: the account's label stays what the node gave.
    let mut words = describe_order_client_id(
        &parse_order_client_id(id).expect("an id that carried a strategy parsed"),
    );
    words.account_label = doc["account_label"].as_str().map(str::to_string);
    words.strategy_name = Some(row.strategy_name.clone());
    doc["words"] = words.to_string().into();
    doc["strategy_name"] = row.strategy_name.into();
    if doc["mount_id"].is_null() {
        doc["mount_id"] = row.mount_id.into();
    }
    doc
}

/// The human answer: the id and its words, then what the node or the store named beside them.
pub(super) fn text(doc: &Value) -> String {
    let mut out = format!(
        "{}\n  {}",
        doc["id"].as_str().unwrap_or_default(),
        doc["words"].as_str().unwrap_or_default()
    );
    if let Some(key) = doc["route_key"].as_str() {
        out.push_str(&format!("\n  account: {key}"));
    }
    if let Some(mount) = doc["mount_id"].as_str() {
        out.push_str(&format!("\n  mount: {mount}"));
    }
    let siblings: Vec<&str> =
        doc["siblings"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if !siblings.is_empty() {
        out.push_str(&format!("\n  its group, resting now: {}", siblings.join(", ")));
    }
    out
}

#[path = "decode_tests.rs"]
#[cfg(test)]
mod decode_tests;
