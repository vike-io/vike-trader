//! Golden-fixture reconciliation gate: fixtures/recon/<scenario>/ → expected Recon events.
//! Pure diff/resolve, so this is a bit-exact merge gate — no competitor has one. Add a directory
//! per scenario; the test auto-discovers them.

use std::collections::HashSet;
use std::path::Path;

use indexmap::IndexMap;
use vike_exec::recon::{diff, resolve, LocalView, ReconPolicy};
use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

fn load<T: serde::de::DeserializeOwned + Default>(dir: &Path, name: &str) -> T {
    let p = dir.join(name);
    if !p.exists() {
        return T::default();
    }
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn recon_golden_fixtures() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/recon");
    let mut checked = 0;
    for entry in std::fs::read_dir(&root).unwrap() {
        let dir = entry.unwrap().path();
        if !dir.is_dir() {
            continue;
        }
        let orders: Vec<OrderStatusReport> = load(&dir, "orders.json");
        let fills: Vec<FillReport> = load(&dir, "fills.json");
        let positions: Vec<PositionStatusReport> = load(&dir, "positions.json");
        // Minimal local view: empty engine state (extend the fixture schema as scenarios grow).
        let empty_orders = IndexMap::new();
        let seen = HashSet::new();
        let pos = IndexMap::new();
        let local = LocalView {
            venue: "binance",
            orders: &empty_orders,
            seen_trade_ids: &seen,
            positions: &pos,
            qty_tol: 1e-9,
            cash: Default::default(),
        };
        let recon = resolve(
            diff(&orders, &fills, &positions, &local, None),
            &ReconPolicy::default(),
            None,
            None,
        );
        let got = serde_json::to_value(&recon.events).unwrap();
        let expected: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("expected.json")).unwrap())
                .unwrap();
        assert_eq!(got, expected, "fixture {:?} diverged", dir.file_name());
        checked += 1;
    }
    assert!(checked > 0, "no recon fixtures found under {:?}", root);
}
