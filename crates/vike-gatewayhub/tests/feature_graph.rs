//! **The cargo feature graph of this crate: the trade half implies the data half, and nothing implies it back.**
//!
//! `gatewayhub_cli/wire.rs` mounts the trade surface only NEXT TO the data lanes (`serve_wire` builds the lanes
//! first and hands them to `serve_with_trade`), and a build without `data` has a `serve_wire` that exits 2. So a
//! build with `trade`, `ibkr`, `fxcm` or `polymarket` and no `data` is a daemon that cannot serve the surface
//! its feature names (`cargo build -p vike-gatewayhub --features ibkr`, the old tradehub's spelling of "this
//! venue", was exactly that before wave 2's W15b). The manifest therefore spells `trade = ["data", ...]` and the
//! venue features through `trade`. This reads the manifest's `[features]` table and holds the implication.
//!
//! The other direction is held too: `data` must not reach `trade`, because the `datahub-all` job's guard (E)
//! builds `--features data` and refuses a signer in its tree.
//!
//! A hand parser, because the table spreads `trade` and `data` over several lines with comments between the
//! entries (the workspace's gates take no TOML dependency).

use std::collections::{BTreeMap, BTreeSet};

const MANIFEST: &str = include_str!("../Cargo.toml");

/// `[features]` as name -> entries, reading arrays that span lines and dropping `#` comments.
fn feature_table(manifest: &str) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut in_features = false;
    let mut open: Option<(String, String)> = None;
    for line in manifest.lines() {
        let code = line.split('#').next().unwrap_or("").trim();
        if let Some((name, mut body)) = open.take() {
            body.push_str(code);
            if code.contains(']') {
                out.insert(name, entries(&body));
            } else {
                open = Some((name, body));
            }
            continue;
        }
        if code.starts_with('[') {
            in_features = code == "[features]";
            continue;
        }
        if !in_features || code.is_empty() {
            continue;
        }
        let Some((name, rest)) = code.split_once('=') else { continue };
        if !rest.contains('[') {
            continue;
        }
        if rest.contains(']') {
            out.insert(name.trim().to_string(), entries(rest));
        } else {
            open = Some((name.trim().to_string(), rest.to_string()));
        }
    }
    out
}

fn entries(array: &str) -> Vec<String> {
    let inner =
        &array[array.find('[').map_or(0, |i| i + 1)..array.rfind(']').unwrap_or(array.len())];
    inner
        .split(',')
        .map(|e| e.trim().trim_matches('"').to_string())
        .filter(|e| !e.is_empty())
        .collect()
}

/// Every feature name enabling `root` turns on, through this crate's own features (`dep:` and `pkg/feature`
/// entries are not features of this table and are skipped).
fn closure(table: &BTreeMap<String, Vec<String>>, root: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![root.to_string()];
    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        for e in table.get(&name).unwrap_or_else(|| panic!("feature `{name}` is not in [features]"))
        {
            if !e.starts_with("dep:") && !e.contains('/') {
                stack.push(e.clone());
            }
        }
    }
    seen
}

#[test]
fn the_parser_sees_the_multiline_tables() {
    let table = feature_table(MANIFEST);
    // ANTI-VACUITY: the multi-line `data` and `trade` arrays, comments between entries and all, parse to
    // entries, and the single-line venue features parse to theirs.
    assert!(table["data"].iter().any(|e| e == "dep:vike-binance"), "data: {:?}", table["data"]);
    assert!(
        table["trade"].iter().any(|e| e == "dep:vike-bridge-core"),
        "trade: {:?}",
        table["trade"]
    );
    assert!(table["ibkr"].iter().any(|e| e == "trade"), "ibkr: {:?}", table["ibkr"]);
    assert!(table["default"].is_empty(), "the default build links no bridge");
}

#[test]
fn the_trade_half_and_every_venue_feature_imply_the_data_half() {
    let table = feature_table(MANIFEST);
    for feature in ["trade", "ibkr", "fxcm", "polymarket"] {
        let reached = closure(&table, feature);
        assert!(
            reached.contains("data"),
            "`{feature}` does not imply `data`: a build with it and nothing else compiles and then exits 2 \
             at start, because `serve_wire` mounts the trade surface only beside the data lanes \
             (`gatewayhub_cli/wire.rs`). Reached: {reached:?}"
        );
    }
    for feature in ["ibkr", "fxcm", "polymarket"] {
        assert!(
            closure(&table, feature).contains("trade"),
            "`{feature}` does not imply `trade`: its registry row is the trade half's"
        );
    }
}

#[test]
fn the_data_half_does_not_reach_the_signers() {
    let table = feature_table(MANIFEST);
    let reached = closure(&table, "data");
    for forbidden in ["trade", "ibkr", "fxcm", "polymarket"] {
        assert!(
            !reached.contains(forbidden),
            "`data` reaches `{forbidden}`: the `datahub-all` guard (E) builds `--features data` and refuses a \
             signer in that tree. Reached: {reached:?}"
        );
    }
}
