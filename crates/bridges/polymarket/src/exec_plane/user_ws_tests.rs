use super::*;
use crate::config::PolymarketCreds;
use crate::exec_plane::registry::PolymarketRegistry;
use std::assert_matches;
use vike_model::events::LiquiditySide;

// ---- dust-snap tracker, scripted through the real decoder ----

fn taker_trade(id: &str, size: &str, price: &str) -> serde_json::Value {
    serde_json::json!({
        "event_type": "trade", "type": "TRADE", "id": id, "status": "MATCHED",
        "asset_id": "111", "side": "BUY", "size": size, "price": price,
        "taker_order_id": "0xORD", "maker_orders": []
    })
}

fn order_update(matched: &str) -> serde_json::Value {
    serde_json::json!({
        "event_type": "order", "type": "UPDATE", "id": "0xORD", "asset_id": "111",
        "original_size": "100", "size_matched": matched
    })
}

/// Every emitted bare `Fill`'s (trade_id, qty) in order.
fn fills(evs: &[Event]) -> Vec<(String, f64)> {
    evs.iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some((f.trade_id.to_string(), f.last_qty)),
            _ => None,
        })
        .collect()
}

fn tracked_reg() -> (PolymarketRegistry, FillTracker) {
    let reg = PolymarketRegistry::new();
    let _ = reg.on_accept("coid-1", "0xORD", 1);
    let t = FillTracker::new();
    t.register("coid-1", 100.0);
    (reg, t)
}

#[test]
fn dust_overfill_is_snapped_before_emission() {
    let (reg, t) = tracked_reg();
    let a = decode_user_with_tracker(&taker_trade("t1", "60", "0.50"), &reg, Some(&t));
    assert_eq!(fills(&a), vec![("t1:0xORD".to_string(), 60.0)]);
    // venue overfills the tail by 0.02 → snapped down to the exact 40.0 remaining
    let b = decode_user_with_tracker(&taker_trade("t2", "40.02", "0.51"), &reg, Some(&t));
    assert_eq!(fills(&b), vec![("t2:0xORD".to_string(), 40.0)]);
    // fully filled: a further dust match snaps to zero and emits NOTHING
    assert!(
        decode_user_with_tracker(&taker_trade("t3", "0.01", "0.51"), &reg, Some(&t)).is_empty()
    );
}

#[test]
fn untracked_tracker_none_is_byte_identical() {
    let (reg, t) = tracked_reg();
    let frame = taker_trade("t1", "100.02", "0.50");
    let plain = decode_user(&frame, &reg);
    assert_eq!(fills(&plain), vec![("t1:0xORD".to_string(), 100.02)]);
    // ...and the same via the explicit-None entry point
    assert_eq!(fills(&decode_user_with_tracker(&frame, &reg, None)), fills(&plain));
    // tracked, the same frame snaps
    assert_eq!(
        fills(&decode_user_with_tracker(&frame, &reg, Some(&t))),
        vec![("t1:0xORD".to_string(), 100.0)]
    );
}

/// FINDING 3 regression (this is the test the old
/// `residual_seen_first_at_the_terminal_update_completes_there` claimed to be): the dust
/// residual is minted at the TERMINAL UPDATE, and exactly once across a duplicate terminal.
#[test]
fn dust_residual_is_minted_at_the_terminal_update_exactly_once() {
    let (reg, t) = tracked_reg();
    // venue's matches stop 0.02 short of our 100 — but the order may still be RESTING, so
    // nothing is synthesized at the trade event.
    let a = decode_user_with_tracker(&taker_trade("t1", "99.98", "0.62"), &reg, Some(&t));
    assert_eq!(fills(&a), vec![("t1:0xORD".to_string(), 99.98)], "no dust mid-life");

    // the venue now calls it done → ONE synthetic completing fill, then the terminal wrap
    let term = decode_user_with_tracker(&order_update("100"), &reg, Some(&t));
    let got = fills(&term);
    assert_eq!(got.len(), 1, "exactly one dust fill: {term:?}");
    assert_eq!(got[0].0, "0xORD:dust");
    assert!((got[0].1 - 0.02).abs() < 1e-9, "{got:?}");
    if let Event::Fill(f) = &term[0] {
        assert_eq!(f.last_px, 0.62, "synthetic fill mints at the last fill price");
        assert_eq!(f.side, 1);
    } else {
        panic!("expected the dust Fill first: {term:?}");
    }
    assert_matches!(term[2], Event::OrderFilled(_));

    // a duplicate terminal must NOT mint a second dust fill
    let dup = decode_user_with_tracker(&order_update("100"), &reg, Some(&t));
    assert!(fills(&dup).is_empty(), "duplicate terminal re-minted: {dup:?}");
    assert_matches!(dup[0], Event::OrderFilled(_));
    assert!(t.is_empty(), "completed order evicted");
}

/// FINDING 2 regression: a sub-tolerance remainder that is still RESTING must not be force
/// completed mid-life — otherwise the venue's later real fill of that remainder is folded on
/// top of an already-minted synthetic one (double-counted position).
#[test]
fn a_resting_sub_threshold_remainder_is_not_force_completed() {
    let (reg, t) = tracked_reg();
    let a = decode_user_with_tracker(&taker_trade("t1", "99.97", "0.62"), &reg, Some(&t));
    assert_eq!(fills(&a), vec![("t1:0xORD".to_string(), 99.97)], "no dust: {a:?}");
    assert_eq!(t.len(), 1, "still tracked — the 0.03 may still be working");

    // the venue really fills the remainder later: it is emitted ONCE, as a real fill
    let b = decode_user_with_tracker(&taker_trade("t2", "0.03", "0.62"), &reg, Some(&t));
    assert_eq!(fills(&b), vec![("t2:0xORD".to_string(), 0.03)]);
    // and the terminal now has nothing left to synthesize
    let term = decode_user_with_tracker(&order_update("100"), &reg, Some(&t));
    assert!(fills(&term).is_empty(), "no dust after a complete fill: {term:?}");
    assert_matches!(term[0], Event::OrderFilled(_));
}

/// FINDING 1 regression, through the real decoder: the venue sends MATCHED, MINED and
/// CONFIRMED for the SAME match. Each is re-emitted (the core dedups on the composite id) but
/// the tracker's cumulative must advance only once — otherwise the tail fill gets snapped and
/// real qty is silently discarded.
#[test]
fn status_repeats_do_not_inflate_the_cumulative() {
    let (reg, t) = tracked_reg();
    for status in ["MATCHED", "MINED", "CONFIRMED"] {
        let mut ev = taker_trade("t1", "0.02", "0.50");
        ev["status"] = serde_json::json!(status);
        let evs = decode_user_with_tracker(&ev, &reg, Some(&t));
        assert_eq!(fills(&evs), vec![("t1:0xORD".to_string(), 0.02)], "{status}");
    }
    // the real tail: 99.98 remains, so it must pass through UNSNAPPED and in full
    let tail = decode_user_with_tracker(&taker_trade("t2", "99.98", "0.50"), &reg, Some(&t));
    assert_eq!(fills(&tail), vec![("t2:0xORD".to_string(), 99.98)], "tail was snapped away");
    let term = decode_user_with_tracker(&order_update("100"), &reg, Some(&t));
    assert!(fills(&term).is_empty(), "nothing left to synthesize: {term:?}");
}

/// TEST GAP: the maker branch (`maker_orders[].matched_amount`) is the path a mounted
/// SpreadMaker actually takes; prove the tracker applies there too.
#[test]
fn maker_branch_snaps_and_completes_through_the_tracker() {
    let (reg, t) = tracked_reg();
    let maker_trade = |id: &str, amt: &str| {
        serde_json::json!({
            "event_type": "trade", "type": "TRADE", "id": id, "status": "MATCHED",
            "asset_id": "111", "side": "BUY", "size": "5", "price": "0.9",
            "taker_order_id": "0xSOMEONE",
            "maker_orders": [{ "order_id": "0xORD", "matched_amount": amt, "price": "0.62" }]
        })
    };
    let a = decode_user_with_tracker(&maker_trade("m1", "60"), &reg, Some(&t));
    assert_eq!(fills(&a), vec![("m1:0xORD".to_string(), 60.0)]);
    // 40.02 for the tail → snapped down to the exact 40.0 remaining
    let b = decode_user_with_tracker(&maker_trade("m2", "40.02"), &reg, Some(&t));
    assert_eq!(fills(&b), vec![("m2:0xORD".to_string(), 40.0)]);
    // and a repeat of that maker match folds nothing further
    let c = decode_user_with_tracker(&maker_trade("m2", "40.02"), &reg, Some(&t));
    assert_eq!(fills(&c), vec![("m2:0xORD".to_string(), 40.0)]);
    assert_eq!(t.check_dust_residual("coid-1"), None, "exactly full");
}

#[test]
fn non_dust_remainder_is_never_synthesized_at_the_terminal() {
    let (reg, t) = tracked_reg();
    decode_user_with_tracker(&taker_trade("t1", "40", "0.5"), &reg, Some(&t));
    let term = decode_user_with_tracker(&order_update("100"), &reg, Some(&t));
    assert!(fills(&term).is_empty(), "60 short is a real remainder, never synthesized: {term:?}");
    assert_matches!(term[0], Event::OrderFilled(_));
    assert!(t.is_empty(), "terminal evicts even without a dust mint");
}

#[test]
fn cancellation_evicts_and_never_mints_dust() {
    let (reg, t) = tracked_reg();
    decode_user_with_tracker(&taker_trade("t1", "90", "0.62"), &reg, Some(&t)); // 10 short
    assert_eq!(t.len(), 1);
    let cancel =
        serde_json::json!({ "event_type": "order", "type": "CANCELLATION", "id": "0xORD" });
    let evs = decode_user_with_tracker(&cancel, &reg, Some(&t));
    assert!(fills(&evs).is_empty(), "a cancel never mints a completing fill: {evs:?}");
    assert_matches!(&evs[0], Event::OrderCanceled(c) if c.client_order_id == "coid-1");
    assert!(t.is_empty(), "cancel evicts the ledger entry");
}

#[test]
fn subscribe_carries_auth() {
    let creds = PolymarketCreds {
        api_key: "k".into(),
        secret: "s".into(),
        passphrase: "p".into(),
        ..Default::default()
    };
    let v: serde_json::Value =
        serde_json::from_str(&user_subscribe_message(&creds, &["111".into()])).unwrap();
    assert_eq!(v["type"], "user");
    assert_eq!(v["auth"]["apiKey"], "k");
    assert_eq!(v["markets"][0], "111");
}

#[test]
fn taker_fill_is_rekeyed_to_coid() {
    let reg = PolymarketRegistry::new();
    let _ = reg.on_accept("my-coid", "0xTAKER", 1); // we are the taker, BUY
    let trade = serde_json::json!({
        "event_type": "trade", "type": "TRADE", "id": "trd1", "status": "MATCHED",
        "asset_id": "111", "side": "BUY", "size": "100", "price": "0.52",
        "taker_order_id": "0xTAKER", "maker_orders": []
    });
    let evs = decode_user(&trade, &reg);
    assert_eq!(evs.len(), 2);
    match (&evs[0], &evs[1]) {
        (Event::Fill(f), Event::OrderPartiallyFilled(w)) => {
            assert_eq!(f.client_order_id, "my-coid");
            assert_eq!(f.trade_id, "trd1:0xTAKER");
            assert_eq!(f.side, 1);
            assert_eq!(f.last_qty, 100.0);
            assert_eq!(f.last_px, 0.52);
            assert_eq!(f.liquidity_side, LiquiditySide::Taker);
            assert_eq!(w.client_order_id, "my-coid");
        }
        other => panic!("expected Fill + OrderPartiallyFilled, got {other:?}"),
    }
}

/// A trade frame with no wire `id` emits NOTHING — the bare `Fill` and its
/// `OrderPartiallyFilled` wrap stand or fall together, because the composite
/// `"{id}:{order_id}"` is the dedup key on both core paths and `":{order_id}"` is per-ORDER, so
/// the MATCHED → MINED → CONFIRMED repeats of one match would each book again.
///
/// This gates the refusal, not the type: reverting `decode_trade`'s empty-`id` guard to a
/// permissive composite turns the asserted 0 back into 2 per frame. Absent, `null` and `""` all
/// reach the guard, and the taker AND maker legs are both covered.
#[test]
fn a_trade_frame_without_an_id_emits_no_fill_on_either_leg() {
    for id in [None, Some(serde_json::Value::Null), Some(serde_json::json!(""))] {
        for (label, taker, maker_oid) in
            [("taker", "0xTAKER", "0xSTRANGER"), ("maker", "0xSTRANGER", "0xMAKER")]
        {
            let reg = PolymarketRegistry::new();
            let _ = reg.on_accept("my-coid", "0xTAKER", 1);
            let _ = reg.on_accept("maker-coid", "0xMAKER", -1);
            let mut trade = serde_json::json!({
                "event_type": "trade", "type": "TRADE", "status": "MATCHED",
                "asset_id": "111", "side": "BUY", "size": "100", "price": "0.52",
                "taker_order_id": taker,
                "maker_orders": [{"order_id": maker_oid, "matched_amount": "100", "price": "0.52"}]
            });
            if let Some(v) = id.clone() {
                trade["id"] = v;
            }
            let evs = decode_user(&trade, &reg);
            assert!(
                evs.is_empty(),
                "an id-less trade frame must emit nothing on the {label} leg (id={id:?}), got \
                     {evs:?}"
            );
        }
    }
}

/// The sibling guard on the ORDER lane: a terminal `UPDATE` with no `id` mints neither the
/// `:dust` completion nor the `:filled` terminal marker. Reverting either to a permissive
/// constructor turns the asserted 0 back into 1-3 events.
///
/// ⚠ Reached here by registering the EMPTY clob id, which is the only way an order frame can
/// re-key to a coid and still carry no id — the shape the guard exists for.
#[test]
fn a_terminal_order_update_without_an_id_emits_nothing() {
    let reg = PolymarketRegistry::new();
    let _ = reg.on_accept("my-coid", "", 1);
    let update = serde_json::json!({
        "event_type": "order", "type": "UPDATE", "id": "", "asset_id": "111",
        "original_size": "100", "size_matched": "100"
    });
    let evs = decode_user(&update, &reg);
    assert!(evs.is_empty(), "no id ⇒ no terminal wrap and no dust mint, got {evs:?}");
}

/// ...and the two minted ids keep their exact byte shapes, which the dedup sets and
/// `tests/offline/dust_snap_engine.rs` both depend on.
#[test]
fn the_minted_terminal_ids_keep_their_byte_shapes() {
    let (reg, t) = tracked_reg();
    decode_user_with_tracker(&taker_trade("t1", "99.999", "0.62"), &reg, Some(&t));
    let evs = decode_user_with_tracker(&order_update("100"), &reg, Some(&t));
    let ids: Vec<String> = evs
        .iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some(f.trade_id.to_string()),
            Event::OrderFilled(w) => Some(w.fill.trade_id.to_string()),
            Event::OrderPartiallyFilled(w) => Some(w.fill.trade_id.to_string()),
            _ => None,
        })
        .collect();
    assert!(ids.contains(&"0xORD:dust".to_string()), "the dust mint's shape: {ids:?}");
    assert!(ids.contains(&"0xORD:filled".to_string()), "the terminal marker's shape: {ids:?}");
}

#[test]
fn maker_fill_uses_maker_orders_entry_and_registered_side() {
    let reg = PolymarketRegistry::new();
    let _ = reg.on_accept("maker-coid", "0xMAKER", -1); // our resting SELL was hit
    let trade = serde_json::json!({
        "event_type": "trade", "type": "TRADE", "id": "trd2", "status": "MATCHED",
        "asset_id": "111", "side": "BUY", "size": "100", "price": "0.99",
        "taker_order_id": "0xSOMEONE",
        "maker_orders": [{ "order_id": "0xMAKER", "matched_amount": "40", "price": "0.55" }]
    });
    let evs = decode_user(&trade, &reg);
    assert_eq!(evs.len(), 2);
    if let Event::Fill(f) = &evs[0] {
        assert_eq!(f.client_order_id, "maker-coid");
        assert_eq!(f.trade_id, "trd2:0xMAKER");
        assert_eq!(f.side, -1); // our registered side, not the taker's BUY
        assert_eq!(f.last_qty, 40.0); // the maker entry's matched_amount
        assert_eq!(f.last_px, 0.55); // the maker entry's price
        assert_eq!(f.liquidity_side, LiquiditySide::Maker);
    } else {
        panic!("expected a maker Fill");
    }
}

#[test]
fn failed_and_retrying_trades_emit_nothing() {
    let reg = PolymarketRegistry::new();
    let _ = reg.on_accept("c", "0xTAKER", 1);
    for status in ["FAILED", "RETRYING"] {
        let trade = serde_json::json!({
            "event_type": "trade", "id": "t", "status": status,
            "asset_id": "111", "side": "BUY", "size": "1", "price": "0.5",
            "taker_order_id": "0xTAKER", "maker_orders": []
        });
        assert!(decode_user(&trade, &reg).is_empty(), "{status} must emit nothing");
    }
}

/// Unknown ids still emit NOTHING at decode time (this is the pre-existing contract, unchanged)
/// — but they are now STAGED rather than discarded, which is the whole fix. Previously named
/// `unknown_orders_are_dropped`, and that name was the bug: "unknown" conflated "not ours" with
/// "not yet ours".
#[test]
fn unknown_orders_emit_nothing_but_are_staged_not_dropped() {
    let reg = PolymarketRegistry::new(); // empty — nothing is ours (yet)
    let trade = serde_json::json!({
        "event_type": "trade", "id": "t", "status": "MATCHED",
        "asset_id": "111", "side": "BUY", "size": "1", "price": "0.5",
        "taker_order_id": "0xNOTOURS",
        "maker_orders": [{ "order_id": "0xALSONOT", "matched_amount": "1", "price": "0.5" }]
    });
    assert!(decode_user(&trade, &reg).is_empty(), "nothing can be emitted without a coid");
    // …but the frame is held under BOTH ids it names, since either could turn out to be ours.
    assert_eq!(reg.pending_len(), 2);
    assert_eq!(reg.pending_stats().parked, 2);
    assert_eq!(reg.pending_stats().expired, 0, "held, not lost");
}

#[test]
fn order_update_full_fill_is_terminal_and_cancel_rekeys() {
    let reg = PolymarketRegistry::new();
    let _ = reg.on_accept("coid-x", "0xORD", 1);
    let full = serde_json::json!({
        "event_type": "order", "type": "UPDATE", "id": "0xORD",
        "original_size": "100", "size_matched": "100"
    });
    assert_matches!(&decode_user(&full, &reg)[0],
            Event::OrderFilled(f) if f.client_order_id == "coid-x" && f.fill.last_qty == 0.0);

    let partial = serde_json::json!({
        "event_type": "order", "type": "UPDATE", "id": "0xORD",
        "original_size": "100", "size_matched": "40"
    });
    assert!(decode_user(&partial, &reg).is_empty()); // partial handled by trade events, not here

    let cancel =
        serde_json::json!({ "event_type": "order", "type": "CANCELLATION", "id": "0xORD" });
    assert_matches!(&decode_user(&cancel, &reg)[0],
            Event::OrderCanceled(c) if c.client_order_id == "coid-x");
}

// ---- the ack race: an executed fill that arrives before its order is registered ----

mod staging {
    use super::*;
    use crate::exec_plane::pending_events::{DEFAULT_TTL_MS, PendingStats};
    use crate::exec_plane::registry::DEFAULT_SETTLING_GRACE_MS;
    use std::assert_matches;

    fn taker_trade_for(order_id: &str) -> serde_json::Value {
        serde_json::json!({
            "event_type": "trade", "type": "TRADE", "id": "trdX", "status": "MATCHED",
            "asset_id": "111", "side": "BUY", "size": "25", "price": "0.43",
            "timestamp": "1700",
            "taker_order_id": order_id, "maker_orders": []
        })
    }

    /// Every emitted bare `Fill` as (coid, trade_id, side, qty, px).
    fn shape(evs: &[Event]) -> Vec<(String, String, i32, f64, f64)> {
        evs.iter()
            .filter_map(|e| match e {
                Event::Fill(f) => Some((
                    f.client_order_id.to_string(),
                    f.trade_id.to_string(),
                    f.side,
                    f.last_qty,
                    f.last_px,
                )),
                _ => None,
            })
            .collect()
    }

    /// **THE BUG.** A trade event that beats the HTTP ack used to be discarded at the re-key
    /// site, taking its position and realized PnL with it. It is now staged and delivered in
    /// full the moment `on_accept` writes the id.
    #[test]
    fn a_fill_that_beats_the_ack_is_delivered_when_the_order_registers() {
        let reg = PolymarketRegistry::new();
        let trade = taker_trade_for("0xORD");

        // t0 — the venue matched before our submit's HTTP response got home.
        assert!(decode_user(&trade, &reg).is_empty(), "nothing can be keyed yet");
        assert_eq!(reg.pending_stats().parked, 1);
        assert_eq!(reg.pending_stats().expired, 0, "NOT discarded");

        // t1 — the ack lands and the exec thread registers the id.
        let claimed = reg.on_accept("coid-1", "0xORD", 1);
        assert_eq!(claimed.len(), 1, "the staged frame comes back: {claimed:?}");

        // t2 — `crate::exec_plane::client::emit_replayed` decodes and sends it.
        let evs = replay_parked(&claimed, &reg, None);
        assert_eq!(
            shape(&evs),
            vec![("coid-1".to_string(), "trdX:0xORD".to_string(), 1, 25.0, 0.43)],
            "the fill is recovered whole: {evs:?}"
        );
        assert_matches!(evs[1], Event::OrderPartiallyFilled(_), "and its FSM wrap");
        assert_eq!(reg.pending_stats().replayed, 1);
        assert_eq!(reg.pending_len(), 0, "the park is drained");
    }

    /// The recovered event must be INDISTINGUISHABLE from what the live path would have emitted
    /// had the id been present — that is why the raw frame is stored and re-decoded rather than
    /// a fill being reconstructed from extracted fields.
    #[test]
    fn a_replayed_fill_is_identical_to_the_one_the_live_path_would_have_emitted() {
        let trade = taker_trade_for("0xORD");

        let raced = PolymarketRegistry::new();
        assert!(decode_user(&trade, &raced).is_empty());
        let replayed = replay_parked(&raced.on_accept("c", "0xORD", -1), &raced, None);

        let clean = PolymarketRegistry::new();
        let _ = clean.on_accept("c", "0xORD", -1);
        let live = decode_user(&trade, &clean);

        assert_eq!(shape(&replayed), shape(&live));
        assert_eq!(replayed.len(), live.len());
        assert_eq!(format!("{replayed:?}"), format!("{live:?}"), "byte-for-byte");
    }

    /// On this venue every post-acceptance terminal arrives ONLY on the user channel, so a
    /// terminal UPDATE lost to the ack race strands the order at PartiallyFilled forever — the
    /// order-shaped way to silently vanish.
    #[test]
    fn a_terminal_order_update_that_beats_the_ack_is_delivered_too() {
        let reg = PolymarketRegistry::new();
        let terminal = serde_json::json!({
            "event_type": "order", "type": "UPDATE", "id": "0xORD", "asset_id": "111",
            "original_size": "100", "size_matched": "100"
        });
        assert!(decode_user(&terminal, &reg).is_empty());
        assert_eq!(reg.pending_stats().parked, 1);

        let evs = replay_parked(&reg.on_accept("coid-t", "0xORD", 1), &reg, None);
        assert_matches!(
            &evs[0], Event::OrderFilled(f) if f.client_order_id == "coid-t",
            "the terminal is recovered: {evs:?}"
        );
    }

    /// A CANCELLATION that beats the ack is likewise recovered.
    #[test]
    fn a_cancellation_that_beats_the_ack_is_delivered_too() {
        let reg = PolymarketRegistry::new();
        let cancel =
            serde_json::json!({ "event_type": "order", "type": "CANCELLATION", "id": "0xORD" });
        assert!(decode_user(&cancel, &reg).is_empty());
        let evs = replay_parked(&reg.on_accept("coid-c", "0xORD", 1), &reg, None);
        assert_matches!(&evs[0], Event::OrderCanceled(c) if c.client_order_id == "coid-c");
    }

    /// A PLACEMENT decodes to nothing, so staging it would burn the bound to replay a no-op —
    /// and it is the MOST common frame in the ack race, arriving at acceptance by definition.
    #[test]
    fn a_placement_frame_is_never_staged() {
        let reg = PolymarketRegistry::new();
        let placement = serde_json::json!({
            "event_type": "order", "type": "PLACEMENT", "id": "0xORD",
            "original_size": "100", "size_matched": "0"
        });
        assert!(decode_user(&placement, &reg).is_empty());
        assert_eq!(reg.pending_len(), 0, "nothing worth holding");
        // …nor is a NON-terminal update (the trade events carry those partials)
        let partial = serde_json::json!({
            "event_type": "order", "type": "UPDATE", "id": "0xORD",
            "original_size": "100", "size_matched": "40"
        });
        assert!(decode_user(&partial, &reg).is_empty());
        assert_eq!(reg.pending_len(), 0);
    }

    /// A non-fillable status carries no money, so there is nothing to lose and nothing to hold.
    #[test]
    fn a_failed_trade_for_an_unknown_id_is_not_staged() {
        let reg = PolymarketRegistry::new();
        let mut t = taker_trade_for("0xORD");
        t["status"] = serde_json::json!("FAILED");
        assert!(decode_user(&t, &reg).is_empty());
        assert_eq!(reg.pending_len(), 0);
    }

    /// THE DISCRIMINATOR. When we take liquidity, `maker_orders` lists STRANGERS. Staging them
    /// would fire an expiry `warn!` on every ordinary taker fill and drown the signal — so a
    /// frame with a resolved leg is treated as fully attributed.
    #[test]
    fn a_trade_with_one_resolved_leg_does_not_stage_its_counterparties() {
        let reg = PolymarketRegistry::new();
        let _ = reg.on_accept("coid-1", "0xOURS", 1);
        let trade = serde_json::json!({
            "event_type": "trade", "type": "TRADE", "id": "t9", "status": "MATCHED",
            "asset_id": "111", "side": "BUY", "size": "10", "price": "0.5",
            "taker_order_id": "0xOURS",
            "maker_orders": [
                { "order_id": "0xSTRANGER1", "matched_amount": "6", "price": "0.5" },
                { "order_id": "0xSTRANGER2", "matched_amount": "4", "price": "0.5" }
            ]
        });
        let evs = decode_user(&trade, &reg);
        assert_eq!(shape(&evs).len(), 1, "only our leg emits: {evs:?}");
        assert_eq!(reg.pending_len(), 0, "counterparties are not 'not YET ours'");
        assert_eq!(reg.pending_stats().parked, 0);
    }

    /// …but when NOTHING resolves we cannot yet tell which id is ours, so the frame is held
    /// under every one of them and whichever registers claims it.
    #[test]
    fn a_fully_unresolved_trade_is_staged_under_every_id_and_the_right_one_claims_it() {
        let reg = PolymarketRegistry::new();
        let trade = serde_json::json!({
            "event_type": "trade", "type": "TRADE", "id": "t9", "status": "MATCHED",
            "asset_id": "111", "side": "BUY", "size": "10", "price": "0.5",
            "taker_order_id": "0xTAKER",
            "maker_orders": [{ "order_id": "0xMINE", "matched_amount": "10", "price": "0.5" }]
        });
        assert!(decode_user(&trade, &reg).is_empty());
        assert_eq!(reg.pending_len(), 2);

        // our resting MAKER order was the one that got hit
        let evs = replay_parked(&reg.on_accept("coid-m", "0xMINE", -1), &reg, None);
        assert_eq!(
            shape(&evs),
            vec![("coid-m".to_string(), "t9:0xMINE".to_string(), -1, 10.0, 0.5)],
            "our maker leg, with OUR side — not the taker's BUY: {evs:?}"
        );
        // the copy staged under the taker id is untouched, and re-decoding did not re-stage
        assert_eq!(reg.pending_len(), 1);
        assert_eq!(reg.pending_stats().parked, 2, "no re-park during replay");
    }

    /// Race 2, end to end: `crate::exec_plane::client` reads any HTTP 200 from `cancel_order` as success and
    /// runs `registry.remove`, so an order the venue JUST MATCHED is torn down like one that
    /// really rested. The settling grace keeps that already-executed fill foldable.
    #[test]
    fn a_match_that_raced_its_own_cancel_still_folds() {
        let reg = PolymarketRegistry::new();
        let _ = reg.on_accept("coid-r", "0xRACED", -1);
        reg.remove("coid-r"); // the cancel "succeeded"…

        let evs = decode_user(&taker_trade_for("0xRACED"), &reg); // …but the match was first
        assert_eq!(
            shape(&evs),
            vec![("coid-r".to_string(), "trdX:0xRACED".to_string(), -1, 25.0, 0.43)],
            "real money must fold, not vanish: {evs:?}"
        );
        assert_eq!(reg.pending_stats().settled, 1);
        assert_eq!(reg.pending_len(), 0, "resolved outright — never staged");
    }

    /// "not ours": no registration ever arrives, so the TTL discards it — LOUDLY (a `warn!` in
    /// `expire_pending_at`) and COUNTED, which is the difference from the bug being fixed.
    #[test]
    fn a_genuinely_unknown_trade_expires_and_is_counted() {
        // ttl 0 ⇒ already past it on the next sweep; no sleeping, no clock injection.
        let reg = PolymarketRegistry::with_limits(64, 8, 0, DEFAULT_SETTLING_GRACE_MS);
        assert!(decode_user(&taker_trade_for("0xFOREIGN"), &reg).is_empty());
        assert_eq!(reg.pending_len(), 1);
        assert_eq!(reg.pending_stats().expired, 0, "not swept yet");

        // the NEXT inbound frame sweeps (see decode_user_with_tracker)
        let _ = decode_user(&serde_json::json!({ "event_type": "noise" }), &reg);
        assert_eq!(reg.pending_len(), 0);
        assert_eq!(reg.pending_stats().expired, 1, "counted, not silent");
        assert_eq!(reg.pending_stats().replayed, 0);
        // and it is really gone — a late registration finds nothing
        assert!(reg.on_accept("coid-late", "0xFOREIGN", 1).is_empty());
    }

    /// The bound behaves at the decoder seam: at capacity the OLDEST staged order is shed and
    /// counted as `evicted` (a bound problem), never as `expired` (a foreign event).
    #[test]
    fn the_bound_sheds_the_oldest_and_keeps_the_newest_claimable() {
        let reg = PolymarketRegistry::with_limits(2, 8, DEFAULT_TTL_MS, DEFAULT_SETTLING_GRACE_MS);
        for id in ["0xA", "0xB", "0xC"] {
            assert!(decode_user(&taker_trade_for(id), &reg).is_empty());
        }
        assert_eq!(reg.pending_len(), 2, "held at the bound");
        assert_eq!(reg.pending_stats().evicted, 1);
        assert_eq!(reg.pending_stats().expired, 0, "an eviction is not an expiry");
        assert!(reg.on_accept("c", "0xA", 1).is_empty(), "the oldest was shed");
        assert_eq!(reg.on_accept("c", "0xC", 1).len(), 1, "the newest survived");
    }

    /// REGRESSION: the steady state — a registered order's fill — is completely untouched. No
    /// staging, no counter movement, no extra events.
    #[test]
    fn a_normal_registered_fill_is_unchanged() {
        let reg = PolymarketRegistry::new();
        let _ = reg.on_accept("coid-n", "0xORD", 1);
        let evs = decode_user(&taker_trade_for("0xORD"), &reg);
        assert_eq!(
            shape(&evs),
            vec![("coid-n".to_string(), "trdX:0xORD".to_string(), 1, 25.0, 0.43)]
        );
        assert_eq!(evs.len(), 2, "Fill + wrap, nothing else");
        assert_eq!(reg.pending_len(), 0);
        assert_eq!(
            reg.pending_stats(),
            PendingStats::default(),
            "the whole staging lane is inert in the steady state"
        );
    }

    /// The A3 resync used to replay through the SAME re-key gate, which is why it could never
    /// recover what the live path dropped. It now stages too.
    #[test]
    fn the_history_resync_path_stages_instead_of_dropping() {
        let reg = PolymarketRegistry::new();
        let row = serde_json::json!({
            "id": "h1", "status": "CONFIRMED", "asset_id": "111",
            "side": "BUY", "size": "10", "price": "0.5",
            "taker_order_id": "0xLATE", "maker_orders": []
        });
        assert!(decode_typed("trade", &row, &reg).is_empty());
        assert_eq!(reg.pending_stats().parked, 1);
        assert_eq!(replay_parked(&reg.on_accept("coid-h", "0xLATE", 1), &reg, None).len(), 2);
    }

    /// The dust-snap tracker still applies to a recovered fill — replay goes through the real
    /// `emit_fill`, so nothing about the tracked path is special-cased.
    #[test]
    fn a_replayed_fill_still_goes_through_the_dust_snap_tracker() {
        let reg = PolymarketRegistry::new();
        let t = FillTracker::new();
        let mut trade = taker_trade_for("0xORD");
        trade["size"] = serde_json::json!("25.02"); // a cent-tick overfill of our 25

        assert!(decode_user_with_tracker(&trade, &reg, Some(&t)).is_empty());
        t.register("coid-1", 25.0); // the exec thread registers on the ack, then …
        let claimed = reg.on_accept("coid-1", "0xORD", 1);
        let evs = replay_parked(&claimed, &reg, Some(&t));
        assert_eq!(shape(&evs)[0].3, 25.0, "snapped down on replay: {evs:?}");
    }
}
