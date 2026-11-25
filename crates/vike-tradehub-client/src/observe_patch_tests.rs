use serde_json::json;

use super::*;

fn round_trip(old: &Value, new: &Value) -> Option<Value> {
    let patch = diff(old, new);
    let mut state = old.clone();
    if let Some(patch) = patch.clone() {
        apply(&mut state, patch).expect("the patch fits the state it was made against");
    }
    assert_eq!(&state, new, "apply(old, diff(old, new)) is new");
    patch
}

/// The shapes the observe stream carries: a bar series that closed a bar and moved its forming one, an order edited in
/// place beside a new one, a field gone and one come, a value of another kind.
#[test]
fn each_shape_rebuilds_the_new_value() {
    let bar =
        |t: i64, c: f64| json!({"timestamp": t, "o": 1.0, "h": 2.0, "l": 0.5, "c": c, "v": 3.0});
    let old = json!({"closed": [bar(1, 1.0), bar(2, 1.5), bar(3, 1.2)], "forming": bar(4, 1.0)});
    let new = json!({"closed": [bar(2, 1.5), bar(3, 1.2), bar(4, 1.1)], "forming": bar(5, 1.0)});
    let patch = round_trip(&old, &new).expect("changed");
    assert_eq!(patch["fields"]["closed"]["skip"], json!(1), "one bar off the front: {patch}");
    assert_eq!(
        patch["fields"]["closed"]["append"],
        json!([bar(4, 1.1)]),
        "one on the end: {patch}"
    );

    let order =
        |id: &str, status: &str| json!({"order_client_id": id, "status": status, "qty": 1.0});
    let old = json!([order("a", "ACCEPTED"), order("b", "ACCEPTED")]);
    let new = json!([order("a", "FILLED"), order("b", "ACCEPTED"), order("c", "NEW")]);
    let patch = round_trip(&old, &new).expect("changed");
    assert_eq!(patch["edits"]["0"], json!({"fields": {"status": {"value": "FILLED"}}}), "{patch}");

    round_trip(&json!({"a": 1, "b": null}), &json!({"b": null, "c": [1]}));
    round_trip(&json!({"a": 1}), &json!([1, 2]));
    round_trip(&json!([1, 2, 3]), &json!([]));
    round_trip(&json!([]), &json!([4]));
    assert_eq!(round_trip(&json!({"a": [1, {"b": 2}]}), &json!({"a": [1, {"b": 2}]})), None);
}

/// A tiny deterministic generator (no extra dev-dependency): JSON values of the stream's kinds, nested.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn value(&mut self, depth: u32) -> Value {
        // A leaf at the bottom; a container above it.
        let kinds = if depth == 0 { 4 } else { 6 };
        match self.next() % kinds {
            0 => Value::Null,
            1 => Value::from(self.next() % 5),
            2 => Value::from((self.next() % 7) as f64 * 0.25),
            3 => Value::from(["x", "y", "z"][(self.next() % 3) as usize]),
            4 => Value::Array((0..self.next() % 6).map(|_| self.value(depth - 1)).collect()),
            _ => Value::Object(
                (0..self.next() % 5)
                    .map(|_| (format!("k{}", self.next() % 6), self.value(depth - 1)))
                    .collect(),
            ),
        }
    }

    /// `value`, changed here and there: a field edited, dropped or added, an item shifted off, edited or appended.
    fn mutate(&mut self, value: &Value, depth: u32) -> Value {
        match value {
            Value::Object(map) if depth > 0 => {
                let mut map = map.clone();
                for key in map.keys().cloned().collect::<Vec<_>>() {
                    match self.next() % 5 {
                        0 => {
                            map.remove(&key);
                        }
                        1 => {
                            let changed = self.mutate(&map[&key], depth - 1);
                            map.insert(key, changed);
                        }
                        _ => {}
                    }
                }
                if self.next().is_multiple_of(3) {
                    map.insert(format!("k{}", self.next() % 8), self.value(depth - 1));
                }
                Value::Object(map)
            }
            Value::Array(items) if depth > 0 => {
                let mut items = items.clone();
                if !items.is_empty() && self.next().is_multiple_of(3) {
                    items.remove(0);
                }
                for item in &mut items {
                    if self.next().is_multiple_of(4) {
                        *item = self.mutate(item, depth - 1);
                    }
                }
                for _ in 0..self.next() % 3 {
                    items.push(self.value(depth - 1));
                }
                Value::Array(items)
            }
            _ if self.next().is_multiple_of(2) => self.value(depth),
            other => other.clone(),
        }
    }
}

/// Over thousands of random states and their successors, the patch always rebuilds the successor exactly.
#[test]
fn any_state_and_any_successor_round_trip() {
    let mut random = Lcg(0x5eed);
    for _ in 0..5_000 {
        let old = random.value(4);
        let new = random.mutate(&old, 4);
        round_trip(&old, &new);
    }
}

/// **The typed diff is the tree diff, byte for byte**: for each pair of snapshots, [`diff_snapshots`] serializes to the
/// very bytes of [`diff`] over the two trees (the builder the node used before), JSON and msgpack, and rebuilds the new
/// snapshot. The pairs: nothing changed, only the per-frame readings, an order added, one modified, one ended (its age
/// gone), one inserted ahead of another engine's, every order gone, a position changed, a bar closed and the forming one
/// moved, a series added and one hidden, fields that come and go.
#[test]
fn the_typed_diff_is_the_tree_diff() {
    use vike_hub_protocol::frame::{BodyCodec, encode_frame};

    use crate::test_support::observe_frame::full_wire_snapshot;
    use crate::wire::{BarWire, CoreSnapshotWire};

    let mut base = full_wire_snapshot();
    let order = base.orders[0].clone();
    base.orders = (0..40)
        .map(|n| {
            let mut ended = order.clone();
            ended.order_client_id = format!("c-ended-{n}");
            ended.status = "FILLED".into();
            ended.filled_qty = ended.qty;
            ended
        })
        .collect();
    let mut open = order.clone();
    open.order_client_id = "c-open".into();
    open.age_ms = Some(1_500);
    base.orders.push(open);
    let bar = |t: i64, c: f64| BarWire { timestamp: t, o: 1.0, h: 2.0, l: 0.5, c, v: 3.0 };
    base.bars[0].closed = (0..30).map(|t| bar(t, 1.0 + t as f64)).collect();
    let open_at = base.orders.len() - 1;

    // One change made to the base snapshot.
    type Change = Box<dyn Fn(&mut CoreSnapshotWire)>;
    let edits: Vec<(&str, Change)> = vec![
        ("nothing changed", Box::new(|_| {})),
        (
            "the readings only",
            Box::new(|w| {
                w.sequence += 1;
                w.published_ms = 1_700_000_000_123;
                w.uptime_ms = Some(9_000);
            }),
        ),
        (
            "an order added",
            Box::new(|w| {
                let mut new = w.orders[0].clone();
                new.order_client_id = "c-new".into();
                new.status = "ACCEPTED".into();
                new.age_ms = Some(0);
                w.orders.push(new);
            }),
        ),
        (
            "an order modified",
            Box::new(move |w| {
                w.orders[open_at].filled_qty = 0.4;
                w.orders[open_at].age_ms = Some(1_516);
            }),
        ),
        (
            "an order ended",
            Box::new(move |w| {
                let ended = &mut w.orders[open_at];
                ended.status = "FILLED".into();
                ended.age_ms = None;
                ended.reason = Some("filled".into());
            }),
        ),
        (
            "an order inserted ahead of another engine's",
            Box::new(|w| {
                let mut new = w.orders[0].clone();
                new.order_client_id = "c-mid".into();
                w.orders.insert(10, new);
            }),
        ),
        ("every order gone", Box::new(|w| w.orders.clear())),
        (
            "the first order gone",
            Box::new(|w| {
                w.orders.remove(0);
            }),
        ),
        (
            "a position changed",
            Box::new(|w| {
                w.positions[0].size = 0.75;
                w.venues[0].positions[0].size = 0.75;
            }),
        ),
        (
            "a bar closed, the forming one moved",
            Box::new(move |w| {
                let series = &mut w.bars[0];
                series.closed.remove(0);
                series.closed.push(bar(30, 2.5));
                series.forming = Some(bar(31, 2.6));
            }),
        ),
        (
            "the forming bar moved",
            Box::new(|w| w.bars[0].forming.as_mut().expect("forming").c += 0.5),
        ),
        (
            "a series added",
            Box::new(|w| {
                let mut series = w.bars[0].clone();
                series.symbol = "ETHUSDT".into();
                w.bars.push(series);
            }),
        ),
        (
            "a series hidden",
            Box::new(|w| {
                w.bars[0].closed = Vec::new();
                w.bars[0].forming = None;
            }),
        ),
        (
            "fields come and go",
            Box::new(|w| {
                w.fault = None;
                w.halt_sentinel = Some(true);
                w.key_changed = vec!["binance".into()];
                w.recent_events.push("FillEvent c-open".into());
            }),
        ),
    ];
    let mut came = base.clone();
    came.key_changed = vec!["bybit".into()];
    came.price_dials_up = Some(2);
    let pairs = edits
        .iter()
        .map(|(name, edit)| {
            let mut new = base.clone();
            edit(&mut new);
            (*name, base.clone(), new)
        })
        .chain([("fields gone", came, base.clone())]);
    for (name, old, new) in pairs {
        let (old_tree, new_tree) =
            (serde_json::to_value(&old).expect("tree"), serde_json::to_value(&new).expect("tree"));
        let typed = diff_snapshots(&old, &new).expect("typed");
        let tree = diff(&old_tree, &new_tree);
        assert_eq!(
            serde_json::to_vec(&typed).expect("json"),
            serde_json::to_vec(&tree).expect("json"),
            "{name}: the JSON bytes"
        );
        assert_eq!(
            encode_frame(BodyCodec::Msgpack, &typed).expect("msgpack"),
            encode_frame(BodyCodec::Msgpack, &tree).expect("msgpack"),
            "{name}: the msgpack bytes"
        );
        assert_eq!(typed.is_none(), old == new, "{name}: a patch exactly when something changed");
        let mut state = old_tree;
        if let Some(patch) = typed {
            apply(&mut state, patch).expect("fits");
        }
        assert_eq!(state, new_tree, "{name}: rebuilds the new snapshot");
    }
}

/// A patch that does not fit the state is an error, never a silent wrong value.
#[test]
fn a_patch_that_does_not_fit_is_refused() {
    let patch = diff(&json!([1, 2, 3, 4]), &json!([3, 4, 5])).expect("changed");
    let mut short = json!([1]);
    assert!(apply(&mut short, patch.clone()).is_err());
    let mut not_an_array = json!({"a": 1});
    assert!(apply(&mut not_an_array, patch).is_err());
    let mut missing = json!({});
    assert!(
        apply(&mut missing, json!({"fields": {"a": {"fields": {"b": {"value": 1}}}}})).is_err()
    );
}
