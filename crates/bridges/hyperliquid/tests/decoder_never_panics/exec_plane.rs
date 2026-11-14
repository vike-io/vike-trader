//! The exec plane (default-on `execution` feature): user-data pump, reconcile and settlement decoders.

use std::sync::atomic::AtomicBool;

use super::*;
use proptest::test_runner::TestCaseError;
use vike_bridge_core::json_arb::obj_of;
use vike_hyperliquid::execution::OrderClientIdVenueRegistry;
use vike_hyperliquid::execution::event_mapper::{
    self, SubmittedOrder, cloid_from_order_client_id, map_order_status,
};
use vike_hyperliquid::execution::settlement;
use vike_hyperliquid::{execution::user_data, reconcile};

/// The order client ids the fixture registry knows, so a frame's `cloid` can resolve.
const KNOWN_ORDER_CLIENT_IDS: [&str; 3] = ["c1", "c2", "c3"];

fn cloid() -> BoxedStrategy<Value> {
    prop_oneof![
        6 => prop::sample::select(KNOWN_ORDER_CLIENT_IDS.iter().map(|c| cloid_from_order_client_id(c)).collect::<Vec<_>>())
            .prop_map(Value::String),
        1 => Just(Value::String(String::new())),
        1 => KIT.arb_leaf(),
    ]
    .boxed()
}

/// The fixture a pump thread would hold: a symbology, a registry that knows [`KNOWN_ORDER_CLIENT_IDS`] and
/// has retired a few order_ids, and the first-snapshot flag.
fn fixture() -> (Symbology, OrderClientIdVenueRegistry, AtomicBool) {
    let meta = serde_json::json!({"universe": [
        {"name": "BTC", "szDecimals": 5, "maxLeverage": 50},
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25},
    ]});
    let spot = serde_json::json!({
        "tokens": [
            {"index": 0, "name": "USDC", "szDecimals": 8},
            {"index": 150, "name": "HYPE", "szDecimals": 2},
        ],
        "universe": [{"name": "@107", "index": 107, "tokens": [150, 0]}],
    });
    let registry = OrderClientIdVenueRegistry::new();
    for c in KNOWN_ORDER_CLIENT_IDS {
        let _ = registry.register(c);
    }
    registry.retire_oid(1);
    registry.retire_oid(2);
    (Symbology::from_meta(&meta, &spot), registry, AtomicBool::new(false))
}

fn order_row() -> BoxedStrategy<Value> {
    let order = obj_of(vec![
        ("coin", KIT.pick(COINS)),
        ("side", KIT.pick(&["A", "B", ""])),
        ("limitPx", KIT.numv()),
        ("sz", KIT.numv()),
        ("origSz", KIT.numv()),
        ("oid", small_id()),
        ("cloid", cloid()),
        ("timestamp", jint()),
    ]);
    obj_of(vec![
        ("order", junk_or(order)),
        (
            "status",
            KIT.pick(&[
                "open",
                "resting",
                "triggered",
                "filled",
                "canceled",
                "marginCanceled",
                "scheduledCancel",
                "rejected",
                "tickRejected",
                "somethingNew",
                "",
            ]),
        ),
        ("statusTimestamp", jint()),
    ])
}

fn fill_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("coin", KIT.pick(COINS)),
        ("px", KIT.numv()),
        ("sz", KIT.numv()),
        ("side", KIT.pick(&["A", "B", ""])),
        ("time", jint()),
        ("oid", small_id()),
        ("tid", small_id()),
        ("cloid", cloid()),
        ("crossed", prop_oneof![any::<bool>().prop_map(Value::Bool), KIT.arb_leaf()].boxed()),
        ("fee", KIT.numv()),
        ("feeToken", KIT.pick(&["USDC", ""])),
    ])
}

fn order_updates_frame() -> BoxedStrategy<Value> {
    chan_frame("orderUpdates", array_of(order_row(), 5))
}

fn user_fills_frame() -> BoxedStrategy<Value> {
    let body = obj_of(vec![
        ("isSnapshot", prop_oneof![any::<bool>().prop_map(Value::Bool), KIT.arb_leaf()].boxed()),
        ("fills", junk_or(array_of(fill_row(), 5))),
    ]);
    chan_frame("userFills", body)
}

/// A private-WS frame of either lane (plus pongs and random JSON).
fn private_frame() -> BoxedStrategy<Value> {
    prop_oneof![
        order_updates_frame(),
        user_fills_frame(),
        chan_frame("pong", KIT.arb_json(ALL_KEYS)),
        KIT.arb_json(ALL_KEYS),
    ]
    .boxed()
}

/// One `/exchange` `statuses[i]`: a string status or a `{resting|filled|error}` object.
fn exchange_status() -> BoxedStrategy<Value> {
    prop_oneof![
        KIT.pick(&["waitingForTrigger", "waitingForFill", "weird", ""]),
        obj_of(vec![("resting", junk_or(obj_of(vec![("oid", small_id())])))]),
        obj_of(vec![(
            "filled",
            junk_or(obj_of(vec![
                ("totalSz", KIT.numv()),
                ("avgPx", KIT.numv()),
                ("oid", small_id()),
            ])),
        )]),
        obj_of(vec![("error", KIT.arb_leaf())]),
        KIT.arb_json(ALL_KEYS),
    ]
    .boxed()
}

fn exchange_response() -> BoxedStrategy<Value> {
    let data = obj_of(vec![("statuses", junk_or(array_of(exchange_status(), 5)))]);
    let response = obj_of(vec![("type", KIT.pick(&["order"])), ("data", junk_or(data))]);
    obj_of(vec![
        ("status", KIT.pick(&["ok", "err", ""])),
        (
            "response",
            junk_or(prop_oneof![response, KIT.arb_str().prop_map(Value::String).boxed()].boxed()),
        ),
    ])
}

fn submitted_orders() -> BoxedStrategy<Vec<SubmittedOrder>> {
    prop::collection::vec(
        (
            KIT.arb_str(),
            prop::sample::select(COINS),
            prop_oneof![Just(1), Just(-1), Just(0)],
            any::<f64>(),
            any::<i64>(),
        )
            .prop_map(|(order_client_id, coin, side, req_sz, ts)| SubmittedOrder {
                order_client_id,
                coin: coin.to_string(),
                side,
                req_sz,
                ts,
            }),
        0..5,
    )
    .boxed()
}

/// `frontendOpenOrders` / `userFills` REST rows (the reconcile client's inputs).
fn open_order_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("coin", KIT.pick(COINS)),
        ("oid", small_id()),
        ("cloid", cloid()),
        ("side", KIT.pick(&["A", "B", ""])),
        ("orderType", KIT.pick(&["Limit", "Stop Market", ""])),
        ("origSz", KIT.numv()),
        ("sz", KIT.numv()),
        ("status", KIT.pick(&["open", "canceled", "marginCanceled", "weird", ""])),
        ("timestamp", jint()),
    ])
}

fn position_row() -> BoxedStrategy<Value> {
    let leverage = obj_of(vec![("type", KIT.pick(&["cross", "isolated", ""])), ("value", jint())]);
    let position = obj_of(vec![
        ("coin", KIT.pick(COINS)),
        ("szi", KIT.numv()),
        ("entryPx", KIT.numv()),
        ("leverage", junk_or(leverage)),
    ]);
    obj_of(vec![("position", junk_or(position))])
}

fn positions_body() -> BoxedStrategy<Value> {
    obj_of(vec![("assetPositions", junk_or(array_of(position_row(), 5))), ("time", jint())])
}

fn balances_body() -> BoxedStrategy<Value> {
    let row = obj_of(vec![
        ("coin", KIT.pick(&["USDC", "HYPE", "+10", ""])),
        ("total", KIT.numv()),
        ("hold", KIT.numv()),
    ]);
    obj_of(vec![
        ("balances", junk_or(array_of(row, 5))),
        ("marginSummary", junk_or(obj_of(vec![("accountValue", KIT.numv())]))),
    ])
}

/// An outcome row: `{outcome, name, description, sideSpecs, quoteToken}`.
fn outcome_row() -> BoxedStrategy<Value> {
    let side = obj_of(vec![("name", KIT.pick(&["Yes", "No", ""]))]);
    obj_of(vec![
        ("outcome", prop_oneof![(0u64..6).prop_map(|n| Value::Number(n.into())), jint()].boxed()),
        ("name", KIT.arb_leaf()),
        ("description", KIT.arb_leaf()),
        ("sideSpecs", junk_or(array_of(side, 4))),
        ("quoteToken", KIT.pick(&["USDH", ""])),
    ])
}

fn outcome_meta_body() -> BoxedStrategy<Value> {
    let question = obj_of(vec![
        ("question", jint()),
        ("name", KIT.arb_leaf()),
        ("description", KIT.arb_leaf()),
        ("fallbackOutcome", jint()),
        ("namedOutcomes", junk_or(array_of(jint(), 4))),
        ("settledNamedOutcomes", junk_or(array_of(jint(), 4))),
    ]);
    obj_of(vec![
        ("outcomes", junk_or(array_of(outcome_row(), 4))),
        ("questions", junk_or(array_of(question, 3))),
    ])
}

fn settled_body() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("spec", junk_or(outcome_row())),
        ("settleFraction", KIT.numv()),
        ("details", KIT.arb_leaf()),
    ])
}

fn spot_balances_body() -> BoxedStrategy<Value> {
    let row = obj_of(vec![
        (
            "coin",
            prop_oneof![
                (0u32..64).prop_map(|n| Value::String(format!("+{n}"))),
                KIT.pick(&["USDC", "+", "+x", "+99999999999999999999"]),
            ]
            .boxed(),
        ),
        ("total", KIT.numv()),
        ("hold", KIT.numv()),
    ]);
    obj_of(vec![("balances", junk_or(array_of(row, 6)))])
}

/// One frame through the REAL pump decode against the shared fixture; no event flood (a row
/// yields <= 1 event).
fn check_frame(
    frame: &Value,
    fx: &(Symbology, OrderClientIdVenueRegistry, AtomicBool),
    spawn_ms: i64,
) -> Result<(), TestCaseError> {
    let data = frame.get("data");
    let n_rows = data.and_then(Value::as_array).map_or(0, Vec::len)
        + data.and_then(|d| d.get("fills")).and_then(Value::as_array).map_or(0, Vec::len);
    let events = user_data::map_frame_to_events(frame, &fx.0, &fx.1, &fx.2, spawn_ms);
    prop_assert!(events.len() <= n_rows, "flood: {} events from {n_rows} rows", events.len());
    let _ = event_mapper::map_order_updates(frame, "hyperliquid");
    let _ = event_mapper::map_user_fills(frame, "hyperliquid");
    let _ = event_mapper::map_order_update(frame, "hyperliquid");
    Ok(())
}

fn drive_reconcile_text(body: &str) {
    let _ = reconcile::parse_orders(body);
    let _ = reconcile::parse_fills(body);
    let _ = reconcile::parse_positions(body);
    let _ = reconcile::parse_balance(body, vike_model::AssetClass::CryptoPerp);
    let _ = reconcile::parse_balance(body, vike_model::AssetClass::CryptoSpot);
    let _ = settlement::parse_outcome_meta(body);
    let _ = settlement::parse_settled_outcome(body);
    let _ = settlement::parse_spot_balances(body);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// (a) arbitrary text / byte noise through every reconcile + outcome-settlement parser.
    #[test]
    fn reconcile_parsers_are_total_over_text(text in any::<String>()) {
        drive_reconcile_text(&text);
    }

    #[test]
    fn reconcile_parsers_survive_byte_noise(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        drive_reconcile_text(&String::from_utf8_lossy(&bytes));
    }

    /// (b) the reconcile bodies: arrays of order / fill rows (whole and cut off), position and
    /// balance objects; a valid array must parse to exactly one order report per row.
    #[test]
    fn reconcile_parsers_are_total_over_bodies(
        orders in array_of(open_order_row(), 6),
        fills in array_of(fill_row(), 6),
        positions in positions_body(),
        balances in balances_body(),
        cut in 0usize..600,
    ) {
        let n = orders.as_array().map_or(0, Vec::len);
        let body = orders.to_string();
        drive_reconcile_text(&body);
        drive_reconcile_text(&truncated(&body, cut));
        let parsed = reconcile::parse_orders(&body);
        prop_assert!(matches!(&parsed, Ok(r) if r.len() == n), "one report per row");
        let n_fills = fills.as_array().map_or(0, Vec::len);
        let parsed = reconcile::parse_fills(&fills.to_string());
        prop_assert!(matches!(&parsed, Ok(r) if r.len() <= n_fills), "a fill report per row at most");
        let _ = reconcile::parse_positions(&positions.to_string());
        let _ = reconcile::parse_balance(&balances.to_string(), vike_model::AssetClass::CryptoPerp);
        let _ = reconcile::parse_balance(&balances.to_string(), vike_model::AssetClass::CryptoSpot);
    }

    /// (b') arbitrary JSON over the real field names through every exec-plane decoder.
    #[test]
    fn execution_decoders_are_total_over_arbitrary_json(
        v in KIT.arb_json(ALL_KEYS),
        spawn_ms in any::<i64>(),
    ) {
        drive_reconcile_text(&v.to_string());
        let fx = fixture();
        check_frame(&v, &fx, spawn_ms)?;
        let order = SubmittedOrder {
            order_client_id: "c1".to_string(),
            coin: "BTC".to_string(),
            side: 1,
            req_sz: 1.0,
            ts: 0,
        };
        let _ = map_order_status(&v, "hyperliquid", &order);
        let _ = event_mapper::map_order_response(&v, "hyperliquid", std::slice::from_ref(&order));
    }

    /// (b'') the private-WS pump decode over well-shaped `orderUpdates` / `userFills` frames,
    /// whole and cut off.
    #[test]
    fn pump_decode_is_total_over_private_frames(
        frame in private_frame(),
        spawn_ms in prop_oneof![Just(0i64), 0i64..4_102_444_800_000, any::<i64>()],
    ) {
        let fx = fixture();
        check_frame(&frame, &fx, spawn_ms)?;
    }

    /// (c) a short sequence of private frames into ONE registry / symbology / snapshot flag:
    /// the first-snapshot floor, the replay branch and the retired-oid cancel suppression all
    /// see state left behind by the previous frame.
    #[test]
    fn pump_decode_survives_frame_sequences(
        frames in prop::collection::vec(private_frame(), 1..8),
        spawn_ms in prop_oneof![Just(0i64), 0i64..4_102_444_800_000],
    ) {
        let fx = fixture();
        for f in &frames {
            check_frame(f, &fx, spawn_ms)?;
        }
    }

    /// The `/exchange` order response, positionally zipped with what was submitted: never more
    /// events than orders, whatever the envelope.
    #[test]
    fn order_response_is_total(
        resp in exchange_response(),
        orders in submitted_orders(),
        status in exchange_status(),
    ) {
        let events = event_mapper::map_order_response(&resp, "hyperliquid", &orders);
        prop_assert!(events.len() <= orders.len(), "{} events for {} orders", events.len(), orders.len());
        if let Some(o) = orders.first() {
            let _ = map_order_status(&status, "hyperliquid", o);
        }
    }

    #[test]
    fn cloid_derivation_is_total(order_client_id in any::<String>()) {
        let cloid = cloid_from_order_client_id(&order_client_id);
        prop_assert_eq!(cloid.len(), 34, "0x + 32 hex chars");
    }

    /// HIP-4 outcome settlement: the parsers, then the pure derivation over whatever they
    /// produced, then the closing fill for an arbitrary local position.
    #[test]
    fn outcome_settlement_is_total(
        meta in outcome_meta_body(),
        settled in prop::collection::vec(settled_body(), 0..4),
        balances in spot_balances_body(),
        wallet in any::<String>(),
        signed_quantity in any::<f64>(),
        ts in any::<i64>(),
        token in any::<String>(),
        fraction in any::<f64>(),
    ) {
        let meta = settlement::parse_outcome_meta(&meta.to_string()).expect("valid JSON");
        let settled: Vec<settlement::SettledOutcome> = settled
            .iter()
            .filter_map(|s| settlement::parse_settled_outcome(&s.to_string()).expect("valid JSON"))
            .collect();
        let balances = settlement::parse_spot_balances(&balances.to_string()).expect("valid JSON");
        let _ = settlement::settlement_candidates(&meta, &balances);
        let _ = meta.settled_by_question();
        let fills = settlement::derive_outcome_settlements(&settled, &balances, &wallet);
        prop_assert!(fills.len() <= balances.len(), "a settlement per held side token at most");
        for fill in &fills {
            let _ = settlement::settlement_fill_event(fill, signed_quantity, ts);
            let _ = settlement::settlement_key(fill.outcome, fill.side, &wallet);
        }
        let _ = settlement::decode_token_name(&token);
        let _ = settlement::payout_for_side(0, fraction);
        let _ = settlement::payout_for_side(1, fraction);
        let _ = settlement::payout_for_side(7, fraction);
    }
}
