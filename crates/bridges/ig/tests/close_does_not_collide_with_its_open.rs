//! Closing an IG position must BOOK, and it must book once — driven off REAL captured confirms.
//!
//! `crates/bridges/ig/src/exec.rs` can now close a position (`POST /positions/otc` +
//! `_method: DELETE`). That is only half the job: a close whose fill is deduped away is
//! indistinguishable from having no close path at all, and IG hands out exactly the id that causes
//! it.
//!
//! ⚠ **IG reuses the closed position's `dealId` on a close confirm.** The two fixtures here are the
//! same round trip, captured live from `demo-api.ig.com` on 2026-08-21: opening
//! `CS.D.EURUSD.MINI.IP` answered `dealId: DIAAAAYB6YDK2A7`, and closing that very position answered
//! a confirm carrying **the same** `dealId`. The engine deduplicates fills by `trade_id`, so keying
//! a close fill on `dealId` makes the close collide with its own open, drops it, and leaves the
//! position open in local state forever — while IG says flat.
//!
//! `event_mapper::confirm_trade_id` is the rule that avoids it (a close is keyed on
//! `dealReference`, which is unique per deal REQUEST), and these tests pin it from BOTH lanes,
//! because the two lanes agreeing is itself load-bearing: if the sync mapper and its streamed twin
//! derived different ids for the same close, the silent dedup would become a DOUBLE-booked position.

use std::collections::HashSet;

use serde_json::Value;
use vike_model::events::Event;

fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.json"));
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing captured fixture {}: {e}", path.display()));
    serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("malformed captured fixture {}: {e}", path.display()))
}

/// The one bare `Event::Fill` a market confirm dual-publishes (the Account-side copy, which is what
/// `seen_trade_ids` deduplicates on).
fn bare_fill(events: &[Event]) -> &vike_model::events::FillEvent {
    events
        .iter()
        .find_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a bare Fill among {events:?}"))
}

/// **The defect, from the sync lane.** Open and close are different executions and must carry
/// different `trade_id`s — otherwise the engine's dedup silently discards the close.
#[test]
fn a_close_fill_and_its_own_open_fill_are_distinct_executions() {
    let open = vike_ig::map_confirm("coid-open", 1, true, &fixture("confirm_open"));
    let close = vike_ig::map_confirm("coid-close", 2, true, &fixture("confirm_close"));

    let (o, c) = (bare_fill(&open), bare_fill(&close));
    assert_eq!(
        o.trade_id, "DIAAAAYB6YDK2A7",
        "an OPEN is still keyed on its dealId — unchanged behaviour"
    );
    assert_ne!(
        o.trade_id,
        c.trade_id,
        "IG reused dealId {} on the close confirm; keying the close fill on it collides with the \
         open and the position never closes locally",
        o.trade_id.as_str()
    );
    assert_eq!(
        c.trade_id, "YRUE8K3G8PYTYS9",
        "the close is keyed on its own dealReference, which is unique per deal REQUEST"
    );

    // ...and the consequence, spelled out the way the engine sees it.
    let mut seen: HashSet<String> = HashSet::new();
    assert!(seen.insert(o.trade_id.as_str().to_string()), "the open books");
    assert!(seen.insert(c.trade_id.as_str().to_string()), "and so does the close");
}

/// The close fill has to describe the CLOSE, not the position: opposite side, the closed size, the
/// closing level. A close booked with the open's side would double the position instead of flatten
/// it.
#[test]
fn the_close_fill_carries_the_closing_side_size_and_level() {
    let close = vike_ig::map_confirm("coid-close", 2, true, &fixture("confirm_close"));
    let f = bare_fill(&close);
    assert_eq!(f.side, -1, "IG's confirm says direction SELL — the closing side");
    assert!((f.last_qty - 0.1).abs() < 1e-12, "the size closed: {}", f.last_qty);
    assert!((f.last_px - 1.16782).abs() < 1e-12, "the level closed at: {}", f.last_px);
    assert_eq!(f.symbol, "CS.D.EURUSD.MINI.IP");
    assert_eq!(f.client_order_id, "coid-close");
}

/// ⚠ **The cross-lane property.** The same close confirm arrives twice — once as the synchronous
/// `/confirms` reply, once on the Lightstreamer `CONFIRMS` stream (the exec thread records the
/// close's `dealReference`, so the stream routes it back to the same order). They MUST derive the
/// same `trade_id`, or the second copy books a second position instead of being deduped.
#[test]
fn both_lanes_derive_the_same_execution_id_for_one_close() {
    let confirm = fixture("confirm_close");
    let sync = vike_ig::map_confirm("coid-close", 2, true, &confirm);
    let streamed = vike_ig::decode_trade_confirm(&confirm, "coid-close", 2);
    assert_eq!(
        bare_fill(&sync).trade_id,
        bare_fill(&streamed).trade_id,
        "a divergence here turns today's silent dedup into a DOUBLE-booked position"
    );
}

/// The reason `build_close_request` closes by `epic` rather than by `dealId`: IG nets across every
/// deal in the epic and answers ONE confirm for the WHOLE size. IG's mappers are whole-fill, so a
/// per-deal close would emit several fills for one `client_order_id` and terminalize it twice.
#[test]
fn a_multi_deal_close_is_still_one_confirm_one_fill_one_terminal() {
    let confirm = fixture("confirm_close_two_deals");
    assert_eq!(
        confirm["affectedDeals"].as_array().map(Vec::len),
        Some(2),
        "the capture really did close two separate deals"
    );
    let evs = vike_ig::map_confirm("coid-flatten", 3, true, &confirm);
    let fills = evs.iter().filter(|e| matches!(e, Event::Fill(_))).count();
    let terminals = evs.iter().filter(|e| matches!(e, Event::OrderFilled(_))).count();
    assert_eq!(fills, 1, "one fill for the whole flatten: {evs:?}");
    assert_eq!(terminals, 1, "exactly one terminal: {evs:?}");
    let f = bare_fill(&evs);
    assert!((f.last_qty - 0.2).abs() < 1e-12, "the COMBINED size of both deals: {}", f.last_qty);
    assert_eq!(f.trade_id, "LJVRYMVLVHQTYS9", "keyed on the close's own dealReference");
}
