//! `observe_patch` — the PATCH a delta frame of the observe stream carries (`Response::SnapshotPatch`): what changed
//! between two JSON states, and its application. The node diffs the JSON of the snapshot it sends against the JSON of
//! the one it sent last ([`diff`]); the reader applies the patch to the state it holds ([`apply`]) and has the node's
//! state again, value for value, so it decodes into the very `CoreSnapshotWire` a whole frame would.
//!
//! ## The patch, node by node
//!
//! - `{"value": V}` — the value is now `V`.
//! - `{"fields": {KEY: NODE, ...}, "removed": [KEY, ...]}` — an object: each named field changes by its node (a field
//!   that was not there arrives as `{"value": V}`), each removed one goes. `{}` changes nothing.
//! - `{"skip": S, "keep": K, "edits": {"I": NODE, ...}, "append": [V, ...]}` — an array: the first `S` items go (`skip`
//!   absent is 0), the next `K` stay, item `I` of those changes by its node, and the `append` items follow. That is a
//!   bar series' shape (one bar off the front, one on the end, the forming one edited), the event ring's, and the order
//!   registry's (one order edited in place, a new one appended).
//!
//! Whatever [`diff`] chooses, `apply(old, diff(old, new))` is `new`: the array's `skip` is a guess at a shift (where
//! `new`'s first item sits in `old`), and every kept item that is not equal gets its edit, so a wrong guess costs bytes,
//! never a value.
//!
//! ## The node's diff is typed
//!
//! The node builds the patch of two snapshots with [`diff_snapshots`]: [`diff`] of their two trees, the same bytes, made
//! from the typed snapshots, so an order, a series or a bar that did not change costs one typed comparison and no tree.
//! The published list held every order the process ever saw, so the trees grew with the node's uptime; measured on
//! the CI box 2026-11-19, building and diffing them cost one patched observer 0.7 of a core. It now holds the open orders
//! and those that ended within the hour (`vike_core::snapshot::ENDED_ORDER_PUBLISH_MS`), still hundreds on a busy node.

use std::convert::Infallible;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::wire::{BarSeriesWire, CoreSnapshotWire};

/// The patch that turns `old` into `new`, `None` when they are equal.
pub fn diff(old: &Value, new: &Value) -> Option<Value> {
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => object_diff(old, new),
        (Value::Array(old), Value::Array(new)) => array_diff(old, new),
        _ if old == new => None,
        _ => Some(replaced(new.clone())),
    }
}

fn replaced(value: Value) -> Value {
    Value::Object(Map::from_iter([("value".to_string(), value)]))
}

fn object_diff(old: &Map<String, Value>, new: &Map<String, Value>) -> Option<Value> {
    let mut fields = Map::new();
    for (key, value) in new {
        match old.get(key) {
            Some(was) => {
                if let Some(patch) = diff(was, value) {
                    fields.insert(key.clone(), patch);
                }
            }
            None => {
                fields.insert(key.clone(), replaced(value.clone()));
            }
        }
    }
    let removed: Vec<Value> = old
        .keys()
        .filter(|key| !new.contains_key(*key))
        .map(|key| Value::String(key.clone()))
        .collect();
    if fields.is_empty() && removed.is_empty() {
        return None;
    }
    let mut node = Map::new();
    if !fields.is_empty() {
        node.insert("fields".to_string(), Value::Object(fields));
    }
    if !removed.is_empty() {
        node.insert("removed".to_string(), Value::Array(removed));
    }
    Some(Value::Object(node))
}

fn array_diff(old: &[Value], new: &[Value]) -> Option<Value> {
    let Ok(patch) = list_diff(
        old,
        new,
        |was, now| Ok::<_, Infallible>(diff(was, now)),
        |item| Ok(item.clone()),
    );
    patch
}

/// The array node of `old` and `new`, items of any kind: `item` is the patch of a kept pair, `tree` an appended item's
/// value. The one array grammar, so the typed diff and the tree diff cannot drift apart.
fn list_diff<T: PartialEq, E>(
    old: &[T],
    new: &[T],
    item: impl Fn(&T, &T) -> Result<Option<Value>, E>,
    tree: impl Fn(&T) -> Result<Value, E>,
) -> Result<Option<Value>, E> {
    let Some(first) = new.first() else {
        return Ok((!old.is_empty()).then(|| replaced(Value::Array(Vec::new()))));
    };
    // Where the new array starts in the old one: a series that closed a bar starts one in.
    let skip = old.iter().position(|was| was == first).unwrap_or(0);
    let keep = (old.len() - skip).min(new.len());
    let mut edits = Map::new();
    for (at, (was, now)) in old[skip..skip + keep].iter().zip(new).enumerate() {
        if let Some(patch) = item(was, now)? {
            edits.insert(at.to_string(), patch);
        }
    }
    // Nothing shifted, edited or appended: the arrays are equal (one pass over the items, not a comparison and a pass).
    if skip == 0 && keep == old.len() && keep == new.len() && edits.is_empty() {
        return Ok(None);
    }
    let mut node = Map::new();
    if skip > 0 {
        node.insert("skip".to_string(), Value::from(skip));
    }
    node.insert("keep".to_string(), Value::from(keep));
    if !edits.is_empty() {
        node.insert("edits".to_string(), Value::Object(edits));
    }
    if keep < new.len() {
        let append = new[keep..].iter().map(tree).collect::<Result<_, _>>()?;
        node.insert("append".to_string(), Value::Array(append));
    }
    Ok(Some(Value::Object(node)))
}

/// **[`diff`] of the two snapshots' trees, made from the typed snapshots**: the same patch, byte for byte
/// (`observe_patch_tests.rs`'s `the_typed_diff_is_the_tree_diff`), but an unchanged order, series or bar costs one
/// typed comparison, never its tree. The orders and the bar series go item by item; the rest of the snapshot, small and
/// mostly per-frame readings, goes through [`diff`] as one tree.
///
/// Typed `==` stands in for the trees' `==` where an array looks for its shift: equal snapshots always have equal trees,
/// and only a non-finite number (the tree writes it `null`) can make two typed items unequal whose trees are equal, which
/// may find another shift: bytes, never a value.
pub fn diff_snapshots(
    old: &CoreSnapshotWire,
    new: &CoreSnapshotWire,
) -> serde_json::Result<Option<Value>> {
    let patch = diff(&serde_json::to_value(rest(old))?, &serde_json::to_value(rest(new))?);
    let orders = list_diff(&old.orders, &new.orders, typed, |order| serde_json::to_value(order))?;
    let bars = list_diff(&old.bars, &new.bars, series_diff, |series| serde_json::to_value(series))?;
    Ok(with_fields(patch, [("bars", bars), ("orders", orders)]))
}

/// `wire` without the two lists [`diff_snapshots`] compares item by item (every field named, so a field added to the
/// snapshot cannot be left out of the patch).
fn rest(wire: &CoreSnapshotWire) -> CoreSnapshotWire {
    CoreSnapshotWire {
        sequence: wire.sequence,
        venue: wire.venue.clone(),
        symbol: wire.symbol.clone(),
        trading_state: wire.trading_state,
        balance: wire.balance,
        equity_total: wire.equity_total,
        venues: wire.venues.clone(),
        orders: Vec::new(),
        positions: wire.positions.clone(),
        held_exits: wire.held_exits.clone(),
        recent_events: wire.recent_events.clone(),
        fault: wire.fault.clone(),
        bars: Vec::new(),
        identity: wire.identity.clone(),
        accounts_epoch: wire.accounts_epoch,
        not_running: wire.not_running.clone(),
        key_changed: wire.key_changed.clone(),
        balance_withheld: wire.balance_withheld.clone(),
        venue_link_down: wire.venue_link_down.clone(),
        venue_path_up: wire.venue_path_up.clone(),
        trade_link_down: wire.trade_link_down.clone(),
        price_link_down: wire.price_link_down.clone(),
        price_dials_up: wire.price_dials_up,
        core_held_stops: wire.core_held_stops.clone(),
        emulated_orders: wire.emulated_orders.clone(),
        reconcile: wire.reconcile.clone(),
        uptime_ms: wire.uptime_ms,
        halt_sentinel: wire.halt_sentinel,
        primary_currency: wire.primary_currency.clone(),
        published_ms: wire.published_ms,
        counters: wire.counters,
        log_lines: wire.log_lines.clone(),
        alert_deliveries_failed: wire.alert_deliveries_failed,
        journal: wire.journal,
    }
}

/// One bar series' patch: its closed bars item by item, the rest as one small tree.
fn series_diff(old: &BarSeriesWire, new: &BarSeriesWire) -> serde_json::Result<Option<Value>> {
    if old == new {
        return Ok(None);
    }
    let rest = |s: &BarSeriesWire| {
        serde_json::to_value(BarSeriesWire {
            venue: s.venue.clone(),
            symbol: s.symbol.clone(),
            interval: s.interval.clone(),
            closed: Vec::new(),
            forming: s.forming.clone(),
        })
    };
    let patch = diff(&rest(old)?, &rest(new)?);
    let closed = list_diff(&old.closed, &new.closed, typed, |bar| serde_json::to_value(bar))?;
    Ok(with_fields(patch, [("closed", closed)]))
}

/// One item's patch: nothing when it did not change, else [`diff`] of its two trees.
fn typed<T: PartialEq + Serialize>(old: &T, new: &T) -> serde_json::Result<Option<Value>> {
    if old == new {
        return Ok(None);
    }
    Ok(diff(&serde_json::to_value(old)?, &serde_json::to_value(new)?))
}

/// The object node `patch` (or nothing) with `lists`' patches added among its fields: those fields are empty lists on
/// both sides of `patch`, so it carries none of them.
fn with_fields<const N: usize>(
    patch: Option<Value>,
    lists: [(&str, Option<Value>); N],
) -> Option<Value> {
    let mut lists =
        lists.into_iter().filter_map(|(key, list)| Some((key.to_string(), list?))).peekable();
    if lists.peek().is_none() {
        return patch;
    }
    // Two objects' patch is an object node or nothing.
    let mut node = match patch {
        Some(Value::Object(node)) => node,
        _ => Map::new(),
    };
    if let Value::Object(fields) = node.entry("fields").or_insert_with(|| Value::Object(Map::new()))
    {
        fields.extend(lists);
    }
    Some(Value::Object(node))
}

/// Apply `patch` ([`diff`]'s) to `target`. An error is a patch that does not fit the state it was applied to (a reader
/// that missed a frame): the caller asks for a whole frame again.
pub fn apply(target: &mut Value, patch: Value) -> Result<(), String> {
    let Value::Object(mut node) = patch else { return Err("a patch node is not an object".into()) };
    if let Some(value) = node.remove("value") {
        *target = value;
        return Ok(());
    }
    if let Some(keep) = node.remove("keep") {
        let Value::Array(items) = target else {
            return Err("an array patch on a value that is not one".into());
        };
        let count = |value: Option<Value>, what: &str| -> Result<usize, String> {
            value
                .map_or(Some(0), |v| v.as_u64())
                .and_then(|n| usize::try_from(n).ok())
                .ok_or(format!("{what} is not a count"))
        };
        let skip = count(node.remove("skip"), "skip")?;
        let keep = count(Some(keep), "keep")?;
        if skip.checked_add(keep).is_none_or(|end| end > items.len()) {
            return Err(format!("an array patch keeps {skip}+{keep} of {} items", items.len()));
        }
        let mut kept: Vec<Value> = items.drain(skip..skip + keep).collect();
        if let Some(edits) = node.remove("edits") {
            let Value::Object(edits) = edits else { return Err("edits are not an object".into()) };
            for (at, edit) in edits {
                let at: usize = at.parse().map_err(|_| format!("edit index {at:?}"))?;
                let length = kept.len();
                let item =
                    kept.get_mut(at).ok_or_else(|| format!("edit index {at} past {length}"))?;
                apply(item, edit)?;
            }
        }
        if let Some(append) = node.remove("append") {
            let Value::Array(append) = append else { return Err("append is not an array".into()) };
            kept.extend(append);
        }
        *items = kept;
        return Ok(());
    }
    if node.is_empty() {
        return Ok(());
    }
    let Value::Object(map) = target else {
        return Err("an object patch on a value that is not one".into());
    };
    if let Some(removed) = node.remove("removed") {
        let Value::Array(removed) = removed else { return Err("removed is not an array".into()) };
        for key in removed {
            let Value::String(key) = key else {
                return Err("a removed key is not a string".into());
            };
            map.remove(&key);
        }
    }
    if let Some(fields) = node.remove("fields") {
        let Value::Object(fields) = fields else { return Err("fields are not an object".into()) };
        for (key, edit) in fields {
            match map.get_mut(&key) {
                Some(field) => apply(field, edit)?,
                None => {
                    let Value::Object(mut edit) = edit else {
                        return Err("a new field's patch is not an object".into());
                    };
                    let value = edit
                        .remove("value")
                        .ok_or(format!("the new field {key:?} carries no value"))?;
                    map.insert(key, value);
                }
            }
        }
    }
    Ok(())
}

#[path = "observe_patch_tests.rs"]
#[cfg(test)]
mod observe_patch_tests;
