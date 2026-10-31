//! A reconcile fill report must carry the key the ENGINE folded the same execution under, or the pass
//! raises a `MissingFill` for a fill the book already holds (`vike_exec::recon::diff` matches a fill by its
//! `trade_id` and by nothing else). Driven off REAL captures (`tests/fixtures/activity_round_trips.json`).
//!
//! ⚠ **The row of a CLOSE carries a NEW `dealId`.** Measured on the demo gateway: the sync confirm of a
//! close names the CLOSED position's `dealId` (the open's own), while the detailed `/history/activity` row
//! of that close has a `dealId` of its own that no confirm ever names; the one id on both is the close's
//! `dealReference` (the one the close POST answered), which is what `event_mapper::confirm_trade_id` keys
//! the engine's close fill on. Keyed on the row's `dealId`, every close the node sent came back as a held
//! `MissingFill` (and under `hybrid`/`synthesize` would be booked a second time). An OPEN does not do this:
//! its row's `dealId` is the position's, the one the confirm names.

use serde_json::{Value, json};
use vike_exec::recon::{Divergence, LocalCash, OwnedLocalState, diff};
use vike_ig::recon_client::parse_activity_fills;
use vike_model::FillReport;
use vike_model::events::Event;

fn fixture() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/activity_round_trips.json");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing captured fixture {}: {e}", path.display()));
    serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("malformed captured fixture {}: {e}", path.display()))
}

/// The `trade_id` of the one bare `Event::Fill` the engine folds for a synchronous market confirm.
fn engine_trade_id(confirm: &Value) -> String {
    vike_ig::map_confirm("coid", 1, true, confirm)
        .iter()
        .find_map(|e| match e {
            Event::Fill(f) => Some(f.trade_id.as_str().to_string()),
            _ => None,
        })
        .expect("a market confirm folds one fill")
}

/// The reports one activity body yields for `epic`.
fn reports(rows: &[Value], epic: &str) -> Vec<FillReport> {
    parse_activity_fills(&json!({ "activities": rows }).to_string(), epic).expect("the body parses")
}

/// The ONE report a single captured row yields.
fn report_of(row: &Value) -> FillReport {
    let epic = row["epic"].as_str().expect("a row names its epic").to_string();
    let mut all = reports(std::slice::from_ref(row), &epic);
    assert_eq!(all.len(), 1, "an executed position row is one fill report: {all:?}");
    all.remove(0)
}

/// An OPEN's report is keyed on the row's `dealId`, the same id the confirm of that open folded under (the
/// path that never failed: pinned so the close's rule cannot spread to it).
#[test]
fn an_open_row_carries_the_trade_id_its_confirm_folded_under() {
    let fx = fixture();
    let open = &fx["open"];
    let report = report_of(&open["activity"]);
    assert_eq!(report.trade_id.as_str(), engine_trade_id(&open["confirm"]));
    assert_eq!(report.trade_id.as_str(), "DIAAAAYQ6XSQUBW", "the position's own dealId");
    assert_eq!(report.venue_order_id.as_str(), "vk6e26723c8525e84a", "our reference, descriptive");
    assert_eq!((report.side, report.last_qty, report.last_px), (1, 0.04, 1.33048));
}

/// A CLOSE's report is keyed on the row's `details.dealReference`, the id the confirm of that close folded
/// under (RED while it was keyed on the row's own, new `dealId`).
#[test]
fn a_close_row_carries_the_trade_id_its_confirm_folded_under() {
    let fx = fixture();
    let close = &fx["close"];
    let report = report_of(&close["activity"]);
    assert_eq!(
        report.trade_id.as_str(),
        engine_trade_id(&close["confirm"]),
        "reconcile and the engine must name the close alike"
    );
    assert_eq!(report.trade_id.as_str(), "X7U5B8MBHC8TYSM", "the close's own reference");
    assert_ne!(
        report.trade_id.as_str(),
        close["activity"]["dealId"].as_str().unwrap(),
        "not the row's dealId: no confirm names it"
    );
    assert_eq!((report.side, report.last_qty, report.last_px), (-1, 1.0, 346.44));
}

/// The whole point, as the engine sees it: after the node folded an open and its close, a reconcile pass
/// over IG's activity rows of both finds NO divergence (RED: one `MissingFill`, the close).
#[test]
fn a_round_trip_the_engine_folded_raises_no_missing_fill() {
    let fx = fixture();
    let (open, close) = (&fx["open"], &fx["close"]);
    let fills: Vec<FillReport> = [&open["activity"], &close["activity"]].map(report_of).to_vec();
    let local = OwnedLocalState {
        venue: "ig".to_string(),
        orders: Default::default(),
        seen_trade_ids: [engine_trade_id(&open["confirm"]), engine_trade_id(&close["confirm"])]
            .into_iter()
            .collect(),
        positions: Default::default(),
        qty_tol: 1e-9,
        cash: LocalCash::default(),
    };
    let found = diff(&[], &fills, &[], &local.as_view(), None);
    assert!(found.is_empty(), "every fill is one the book already holds: {found:?}");

    // ...and a close the book does NOT hold (a restart that lost it, a close made in IG's own UI) is still
    // raised, never guessed away: a held `MissingFill` under its reference.
    let none_seen = OwnedLocalState { seen_trade_ids: Default::default(), ..local };
    let found = diff(&[], &fills, &[], &none_seen.as_view(), None);
    assert!(
        found.iter().filter(|d| matches!(d, Divergence::MissingFill(_))).count() == 2,
        "an unknown fill stays a MissingFill: {found:?}"
    );
}

/// A close the book holds under no id the row can name is never keyed on its `dealId` (that is the defect):
/// a close row WITHOUT a reference is skipped, as an id-less row always was.
#[test]
fn a_close_row_without_a_reference_is_skipped_never_keyed_on_its_deal_id() {
    let mut row = fixture()["close"]["activity"].clone();
    row["details"].as_object_mut().unwrap().remove("dealReference");
    assert!(reports(&[row], "UA.D.AAPL.CASH.IP").is_empty());
}

/// A partial close is a close too (`POSITION_PARTIALLY_CLOSED`: IG's documented action type, not yet seen on
/// the wire), and a row with no `actions` at all stays an open's: keyed on its `dealId`, as before.
#[test]
fn a_partial_close_is_keyed_like_a_close_and_a_row_without_actions_like_an_open() {
    let mut partial = fixture()["close"]["activity"].clone();
    partial["details"]["actions"][0]["actionType"] = json!("POSITION_PARTIALLY_CLOSED");
    assert_eq!(report_of(&partial).trade_id.as_str(), "X7U5B8MBHC8TYSM");

    let mut bare = fixture()["close"]["activity"].clone();
    bare["details"].as_object_mut().unwrap().remove("actions");
    assert_eq!(report_of(&bare).trade_id.as_str(), "DIAAAAYQ6YLZDAX");
}
