//! IG's two fill paths make OPPOSITE choices about a missing `dealId`, and the asymmetry is the
//! point: what a synthesized id is allowed to be depends on what the wire guarantees.
//!
//! - `exec::map_confirm` decodes `GET /confirms/{ref}`, the SYNCHRONOUS confirmation of ONE deal. It
//!   emits at most one fill per call, so `coid` alone identifies that fill and a coid-derived id is a
//!   true per-fill key: re-polling the same confirm re-derives the SAME id and dedups. It
//!   SYNTHESIZES.
//! - `event_mapper::map_trade_update` decodes the Lightstreamer trade-update STREAM, where several
//!   distinct executions (OPEN / AMENDED / PARTIALLY_CLOSED) arrive for one coid. A coid-derived id
//!   there would collapse genuinely different fills into ONE — the mirror-image defect. It DROPS.
//!
//! Both used to spell `deal_id.unwrap_or_default()`, i.e. `""`, which skipped the engine's dedup
//! guard entirely and re-booked commission and realized PnL on replay.

use serde_json::json;
use vike_model::events::Event;

fn confirm(deal_id: Option<&str>) -> serde_json::Value {
    let mut v = json!({
        "dealStatus": "ACCEPTED",
        "epic": "CS.D.EURUSD.CFD.IP",
        "level": 1.0912,
        "size": 1.0,
        "direction": "BUY",
    });
    if let Some(d) = deal_id {
        v["dealId"] = json!(d);
    }
    v
}

fn fills(evs: &[Event]) -> Vec<&vike_model::events::FillEvent> {
    evs.iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f),
            _ => None,
        })
        .collect()
}

// --- the synchronous confirm: SYNTHESIZE ---------------------------------------------------------

#[test]
fn a_confirm_with_a_deal_id_uses_it_verbatim() {
    let evs = vike_ig::map_confirm("coid-1", 100, true, &confirm(Some("DIAAA1")));
    let f = fills(&evs);
    assert_eq!(f.len(), 1, "one market confirm ⇒ one fill: {evs:?}");
    assert_eq!(f[0].trade_id, "DIAAA1");
}

#[test]
fn a_confirm_without_a_deal_id_synthesizes_a_stable_id_rather_than_dropping_the_fill() {
    // The fill genuinely happened (level + size are on the wire), so dropping it would lose a real
    // position change. Synthesizing is safe here because the confirm is single-deal.
    let evs = vike_ig::map_confirm("coid-1", 100, true, &confirm(None));
    let f = fills(&evs);
    assert_eq!(f.len(), 1, "the fill must still be published: {evs:?}");
    assert!(!f[0].trade_id.as_str().is_empty(), "and it must carry a real id");
    assert!(f[0].trade_id.starts_with("IG-CONFIRM-"), "got {}", f[0].trade_id);
}

#[test]
fn the_synthesized_confirm_id_is_identical_on_a_second_decode() {
    // THE property that makes synthesis legitimate: a re-poll of the same confirm must re-derive the
    // same id so the engine's `seen_trade_ids` recognises it as the duplicate it is. An id built from
    // a wall clock or a counter would differ here — that is exactly the trap this asserts against.
    let a = vike_ig::map_confirm("coid-1", 100, true, &confirm(None));
    let b = vike_ig::map_confirm("coid-1", 999, true, &confirm(None)); // note: different ts
    assert_eq!(
        fills(&a)[0].trade_id,
        fills(&b)[0].trade_id,
        "a synthesized id must be a pure function of replay-stable fields — the timestamp must not \
         enter it, or every replay mints a new id and dedup can never fire"
    );
}

#[test]
fn an_empty_deal_id_is_treated_as_absent_not_passed_through() {
    let evs = vike_ig::map_confirm("coid-1", 100, true, &confirm(Some("")));
    let f = fills(&evs);
    assert_eq!(f.len(), 1);
    assert!(f[0].trade_id.starts_with("IG-CONFIRM-"), "got {}", f[0].trade_id);
}

#[test]
fn two_different_coids_get_two_different_synthesized_ids() {
    let a = vike_ig::map_confirm("coid-1", 100, true, &confirm(None));
    let b = vike_ig::map_confirm("coid-2", 100, true, &confirm(None));
    assert_ne!(fills(&a)[0].trade_id, fills(&b)[0].trade_id);
}
