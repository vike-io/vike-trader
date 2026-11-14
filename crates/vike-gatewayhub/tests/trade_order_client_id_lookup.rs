//! **The lookup-by-order-client-id coverage table, one test per venue** (plan B.7, W9b).
//!
//! After a gatewayhub epoch change the tradehub asks `ResolveOrders` about every submit it left UNKNOWN,
//! and the gatewayhub answers by running `ExecutionClient::confirm` (the venue re-query by order client id) where the
//! venue has one (`TradeLane::resolves_by_order_client_id`), and says `NoResolver` where it has not. The table the
//! lane answers from is `bridge_trade::ORDER_CLIENT_ID_LOOKUP`; this file pins every row to the thing it claims:
//! **a venue's bridge builds its exec client `with_confirm(..)` if and only if its row says `true`.**
//!
//! Why a source pin and not a call: `confirm` on a real bridge client needs a live session, and the
//! property that matters is wiring, which is a line of text per bridge. The first measurement
//! (`w2_w0_baselines.md`, section 10) found the plan's table wrong on exactly this point: binance, aster
//! and deribit had a `confirm` on the wrapper and a `resolve_ambiguous_submit` inside the submit path,
//! but the wrapper delegated to an actor spawned WITHOUT `with_confirm`, so `confirm` was a clean no-op.
//! binance and aster have since wired it (the same by-order-client-id lookup the submit-timeout path runs); deribit
//! has not, and the table records the code as it is. Each no-op venue's residual is in its bridge's
//! `CLAUDE.md` ("UNKNOWN submits after a gatewayhub restart").
//!
//! The venues that re-query, as `ORDER_CLIENT_ID_LOOKUP` lists them: bybit, okx, ig, polymarket, binance, aster, oanda, fxcm, hyperliquid.
//!
//! The scan reads the bridge sources at run time and ignores comment lines (so a doc may name
//! `with_confirm(`), and checks it found a file before it trusts an absence.
#![cfg(feature = "trade")]

use std::fs;
use std::path::{Path, PathBuf};

use vike_gatewayhub::trade::adapters::bridge_trade::{ORDER_CLIENT_ID_LOOKUP, TradeLaneBridge};
use vike_gatewayhub::trade::lane::TradeLane;

/// The bridge crate directory of each roster venue, relative to `crates/bridges/`.
#[rustfmt::skip]
const BRIDGE_DIRS: &[(&str, &str)] = &[
    ("binance", "binance"),
    ("bybit", "bybit"),
    ("okx", "okx"),
    ("deribit", "deribit"),
    ("oanda", "oanda"),
    ("ig", "ig"),
    ("fxcm", "fxcm"),
    ("dukascopy", "dukascopy"),
    ("polymarket", "polymarket"),
    ("ibkr", "vike-ibkr"),
    ("ctrader", "ctrader"),
    ("alpaca", "alpaca"),
    ("aster", "aster"),
    ("hyperliquid", "hyperliquid"),
    // vike:new-venue:row ("{venue}", "{venue}"), // TODO(new-venue: {venue}): its directory under `crates/bridges/` (the lookup-by-order-client-id pin reads that crate's source)
];

fn crate_dir(venue: &str) -> &'static str {
    BRIDGE_DIRS.iter().find(|(v, _)| *v == venue).map(|(_, dir)| *dir).unwrap_or_else(|| {
        panic!("{venue} is on the roster but this test knows no bridge directory for it")
    })
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Whether the bridge's `src/` wires a venue re-query behind `confirm` (a non-comment `with_confirm(`
/// call), and how many source files the scan read.
fn wires_a_confirm(venue: &str) -> (bool, usize) {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../bridges").join(crate_dir(venue)).join("src");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let wired = files.iter().any(|f| {
        fs::read_to_string(f)
            .expect("a readable bridge source")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .any(|l| l.contains(".with_confirm("))
    });
    (wired, files.len())
}

fn the_row(venue: &str) -> bool {
    let rows: Vec<_> = ORDER_CLIENT_ID_LOOKUP.iter().filter(|(v, _)| *v == venue).collect();
    assert_eq!(
        rows.len(),
        1,
        "{venue} must have exactly one ORDER_CLIENT_ID_LOOKUP row, found {rows:?}"
    );
    rows[0].1
}

/// One venue's pin: the table, the lane's answer and the bridge's wiring agree.
fn pin(venue: &str, expected: bool) {
    assert_eq!(
        the_row(venue),
        expected,
        "{venue}: the ORDER_CLIENT_ID_LOOKUP row changed - update the plan's B.7 table and the bridge page"
    );
    let (wired, read) = wires_a_confirm(venue);
    assert!(
        read > 0,
        "{venue}: the scan found no source under crates/bridges/{}",
        crate_dir(venue)
    );
    assert_eq!(
        wired,
        expected,
        "{venue}: its bridge {} an `ExecutionClientActor::with_confirm(..)` but the table says it {}",
        if wired { "wires" } else { "does not wire" },
        if expected { "re-queries by order client id" } else { "does not" },
    );
    let lane = TradeLaneBridge::new(
        &[],
        std::collections::HashMap::new(),
        &vike_secrets::account_directory::AccountDirectory::unread(),
        &std::collections::BTreeMap::new(),
    );
    assert_eq!(
        lane.resolves_by_order_client_id(venue),
        expected,
        "{venue}: the lane answers from the table"
    );
}

macro_rules! order_client_id_case {
    ($name:ident, $venue:literal, $resolves:literal) => {
        #[test]
        fn $name() {
            pin($venue, $resolves);
        }
    };
}

// Re-queries by order client id: bybit (order-link-id, realtime then history), okx (`clOrdId`), binance + aster
// (`origClientOrderId`), ig (dealRef), oanda (`GET /orders/@{order_client_id}`), fxcm (the RequestID of a lost placement).
order_client_id_case!(binance_confirms_by_order_client_id, "binance", true);
order_client_id_case!(aster_confirms_by_order_client_id, "aster", true);
order_client_id_case!(bybit_confirms_by_order_client_id, "bybit", true);
order_client_id_case!(okx_confirms_by_order_client_id, "okx", true);
order_client_id_case!(ig_confirms_by_order_client_id, "ig", true);
order_client_id_case!(oanda_confirms_by_order_client_id, "oanda", true);
order_client_id_case!(polymarket_confirms_by_derived_order_id, "polymarket", true);
order_client_id_case!(fxcm_confirms_by_request_id, "fxcm", true);
// Its own actor, so its own `with_confirm` (`orderStatus` by the cloid, a pure function of the order client id).
order_client_id_case!(hyperliquid_confirms_by_cloid, "hyperliquid", true);
// A no-op `confirm`: the wrapper delegates to an actor with no `with_confirm`. The query exists inside
// the submit-timeout path (`resolve_ambiguous_submit`) for deribit, not on this seam.
order_client_id_case!(deribit_confirm_is_a_no_op, "deribit", false);
// No lookup at all.
order_client_id_case!(ctrader_has_no_lookup_by_order_client_id, "ctrader", false);
order_client_id_case!(alpaca_has_no_lookup_by_order_client_id, "alpaca", false);
order_client_id_case!(ibkr_has_no_lookup_by_order_client_id, "ibkr", false);
order_client_id_case!(dukascopy_has_no_lookup_by_order_client_id, "dukascopy", false);

/// A venue added to the roster without a row is a table with a hole: `resolves_by_order_client_id` would answer
/// `false` for it by omission, which reads like a decision.
#[test]
fn every_roster_venue_has_exactly_one_row_and_no_row_names_a_stranger() {
    for venue in vike_model::VENUES {
        the_row(venue);
    }
    for (venue, _) in ORDER_CLIENT_ID_LOOKUP {
        assert!(
            vike_model::VENUES.contains(venue),
            "ORDER_CLIENT_ID_LOOKUP names {venue}, which is not on the roster"
        );
    }
    assert_eq!(ORDER_CLIENT_ID_LOOKUP.len(), vike_model::VENUES.len());
}

/// The scan sees `with_confirm(` where it is: if it read nothing, every absence above would pass.
#[test]
fn the_scan_finds_the_confirms() {
    for venue in
        ["bybit", "okx", "binance", "aster", "ig", "oanda", "polymarket", "fxcm", "hyperliquid"]
    {
        assert!(wires_a_confirm(venue).0, "{venue}: the scan did not find its `with_confirm(`");
    }
}

/// The lane answers `false` for an id it has no row for (a test's `"sim"`): never a confirm to launch.
#[test]
fn an_unknown_venue_does_not_resolve() {
    let lane = TradeLaneBridge::new(
        &[],
        std::collections::HashMap::new(),
        &vike_secrets::account_directory::AccountDirectory::unread(),
        &std::collections::BTreeMap::new(),
    );
    assert!(!lane.resolves_by_order_client_id("sim"));
}

/// The per-venue tests above are named for reading; THIS one is the floor under them. A venue added to the
/// roster is pinned here whether or not anyone wrote it an `order_client_id_case!`: its `ORDER_CLIENT_ID_LOOKUP` row must agree
/// with whether its bridge wires `with_confirm(..)`.
#[test]
fn every_roster_venue_row_agrees_with_its_bridge_source() {
    for venue in vike_model::VENUES {
        let (wired, read) = wires_a_confirm(venue);
        assert!(read > 0, "{venue}: the scan found no bridge source");
        assert_eq!(
            the_row(venue),
            wired,
            "{venue}: ORDER_CLIENT_ID_LOOKUP and the bridge's `with_confirm` disagree"
        );
    }
}
