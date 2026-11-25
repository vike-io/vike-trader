//! THE EXECUTION STREAM'S WIRE, PINNED BY COMMITTED GOLDENS OUTSIDE `crates/`.
//!
//! The open and its reply are positional frames, and everything after them is `execution_stream`'s two frame
//! enums: their serde tags and field keys are the wire between two independently deployed daemons, and a rename
//! compiles clean and passes every round trip because both ends move together. The goldens are what does not
//! move with the edit: the four `fixtures/datahub_wire/v1_execution_stream_*.jsonl` files (the requests, the
//! responses, and the frames up and down), one JSON frame BODY per line (the framing is `write_frame`'s),
//! authored with the stream (goldens of THIS build's tags).
//!
//! The checks, all against the fixtures (the shared ones of `crates/vike-hub-protocol/src/test_support.rs`): every golden decodes
//! into this build's type and this build still PRODUCES every key and value of it (a SUBSET check: an additive
//! field is allowed, a renamed one never is); every variant of the stream's enums appears at its own position in
//! a golden; and every `ExecutionStream` verb of `Request` / `Response` has a golden. A report's ownership rides as
//! the model's own `ExecutionOwnership`, so its variants are read where the model declares it (`OWNERSHIP_SOURCE`).

use std::path::PathBuf;

use vike_datahub_client::execution_stream::{
    ExecutionRecordBodyWire, ExecutionStreamDown, ExecutionStreamUp,
};
use vike_datahub_client::proto::{Request, Response};
use vike_hub_protocol::test_support::{Goldens, Site, variants_of};

const REQUESTS: &str = "fixtures/datahub_wire/v1_execution_stream_requests.jsonl";
const RESPONSES: &str = "fixtures/datahub_wire/v1_execution_stream_responses.jsonl";
const UP: &str = "fixtures/datahub_wire/v1_execution_stream_up.jsonl";
const DOWN: &str = "fixtures/datahub_wire/v1_execution_stream_down.jsonl";
const PROTO_SOURCE: &str = "crates/vike-datahub-client/src/proto.rs";
const STREAM_SOURCE: &str = "crates/vike-datahub-client/src/execution_stream.rs";
const OWNERSHIP_SOURCE: &str = "crates/vike-model/src/orders/execution_report.rs";

/// The stream's wire enums, by the file that declares them.
const WIRE_ENUMS: &[(&str, &[&str])] = &[
    (STREAM_SOURCE, &["ExecutionStreamUp", "ExecutionStreamDown", "ExecutionRecordBodyWire"]),
    (OWNERSHIP_SOURCE, &["ExecutionOwnership"]),
];

/// Where each enum's value sits inside a golden frame.
const SITES: &[(&str, &[Site])] = &[
    ("ExecutionStreamUp", &[Site { file: UP, path: &[] }]),
    ("ExecutionStreamDown", &[Site { file: DOWN, path: &[] }]),
    ("ExecutionRecordBodyWire", &[Site { file: UP, path: &["Records", "*", "body"] }]),
    (
        "ExecutionOwnership",
        &[Site { file: UP, path: &["Records", "*", "body", "Report", "ownership"] }],
    ),
];

fn goldens() -> Goldens {
    Goldens::at(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

#[test]
fn the_golden_stream_frames_decode_and_re_encode() {
    let goldens = goldens();
    goldens.assert_round_trip::<Request>(REQUESTS, 2, PROTO_SOURCE);
    goldens.assert_round_trip::<Response>(RESPONSES, 3, PROTO_SOURCE);
    goldens.assert_round_trip::<ExecutionStreamUp>(UP, 5, STREAM_SOURCE);
    goldens.assert_round_trip::<ExecutionStreamDown>(DOWN, 2, STREAM_SOURCE);
}

/// A variant added to a stream enum must arrive with a golden at its own position.
#[test]
fn every_stream_variant_has_a_golden() {
    let goldens = goldens();
    goldens.assert_every_enum_has_a_golden_site(WIRE_ENUMS, SITES);
    goldens.assert_every_variant_is_covered(WIRE_ENUMS, SITES, "fixtures/datahub_wire/");
}

/// A stream verb added to `Request` / `Response` (a variant named `ExecutionStream...`) must arrive with a golden.
#[test]
fn every_stream_verb_the_source_declares_has_a_golden() {
    let goldens = goldens();
    let src = goldens.read_text(PROTO_SOURCE);
    for (name, file) in [("Request", REQUESTS), ("Response", RESPONSES)] {
        let golden = goldens.tags_at(&Site { file, path: &[] });
        let declared: Vec<String> = variants_of(&src, name)
            .into_iter()
            .filter(|v| v.starts_with("ExecutionStream"))
            .collect();
        assert_eq!(declared.len(), 2, "the scanner found {declared:?} in {name}");
        for variant in declared {
            assert!(golden.contains(&variant), "`{name}::{variant}` has no golden in {file}");
        }
    }
}

/// The reason rides only a report that has one: in the goldens a `DENIED` (the gate's words) and a `REJECTED` (the
/// venue's) carry it, every other order's bytes carry no `reason` key, and a record without the key decodes as no
/// reason.
#[test]
fn the_reason_rides_only_a_report_that_has_one() {
    let goldens = goldens();
    let mut with_reason = Vec::new();
    for line in goldens.lines(UP) {
        let ExecutionStreamUp::Records(records) = serde_json::from_str(&line).expect("decodes")
        else {
            continue;
        };
        for record in records {
            let ExecutionRecordBodyWire::Report(report) = record.body else { continue };
            let Some(order) = report.order else { continue };
            let bytes = serde_json::to_string(&order).expect("encodes");
            assert_eq!(bytes.contains("\"reason\""), order.reason.is_some(), "{bytes}");
            if order.reason.is_some() {
                with_reason.push(order.status);
            }
        }
    }
    assert_eq!(with_reason, ["DENIED", "REJECTED"], "the goldens' reasons");
}

/// The decode-vs-drop pair inside the stream: a record with a field from a newer peer decodes (no
/// `deny_unknown_fields`), and a body kind this build does not know is an ERROR, never a default.
#[test]
fn an_unknown_field_is_ignored_and_an_unknown_body_kind_is_a_decode_error() {
    let widened = r#"{"Heartbeat":{"head_sequence":7,"a_field_from_a_newer_peer":1}}"#;
    assert_eq!(
        serde_json::from_str::<ExecutionStreamUp>(widened).expect("decodes"),
        ExecutionStreamUp::Heartbeat { head_sequence: 7 }
    );
    let unknown = r#"{"Records":[{"sequence":2,"previous_sequence":1,"recorded_ms":5,"body":{"Funding":{}}}]}"#;
    assert!(serde_json::from_str::<ExecutionStreamUp>(unknown).is_err());
}

/// What the OMS journaled reaches the datahub whole: `From<&ExecutionReport>`, then a trip through the frame's
/// JSON, leaves every field the sink stores in the stored row's spelling (`OrderType::as_str`,
/// `OrderStatus::as_str`, the materializer's liquidity and position-side words, no position side for a one-way
/// fill), the reason on the order row, and the fee correction's price.
#[test]
fn a_report_reaches_the_wire_in_the_stored_rows_spelling() {
    use vike_datahub_client::execution_stream::{
        ExecutionReportFeeCorrectionWire, ExecutionReportFillWire, ExecutionReportOrderWire,
        ExecutionReportWire,
    };
    use vike_model::events::{LiquiditySide, PositionSide};
    use vike_model::orders::client_id::OrderClientIdStrategyCode;
    use vike_model::orders::{
        ExecutionOwnership, ExecutionReport, ExecutionReportFeeCorrection, ExecutionReportFill,
        ExecutionReportOrder,
    };
    use vike_model::{OrderStatus, OrderType};

    let fill = ExecutionReportFill {
        trade_id: "t-1".into(),
        side: -1,
        qty: 0.25,
        price: 101.5,
        commission: 0.01,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Maker,
        mark_price: Some(101.4),
        position_side: PositionSide::Short,
    };
    let report = ExecutionReport {
        caused_by_sequence: 41,
        at_ms: 1_760_000_000_123,
        ownership: ExecutionOwnership::Ours,
        account: Some("A".into()),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        order_client_id: "S0001A1x".into(),
        venue_order_id: Some("v-9".into()),
        mount_id: Some("maker-1".into()),
        strategy_code: OrderClientIdStrategyCode::new(1),
        order: Some(ExecutionReportOrder {
            side: -1,
            qty: 1.0,
            order_type: OrderType::StopLimit,
            status: OrderStatus::PartiallyFilled,
            price: Some(101.0),
            trigger_price: Some(102.0),
            filled_qty: 0.25,
            average_fill_price: 101.5,
        }),
        fill: Some(fill.clone()),
        fee_correction: Some(ExecutionReportFeeCorrection {
            corrects_trade_id: "t-0".into(),
            commission: 0.002,
            commission_asset: "BTC".into(),
            price: 99.5,
        }),
        reason: Some("the venue said so".into()),
    };
    let sent = ExecutionReportWire::from(&report);
    let received: ExecutionReportWire =
        serde_json::from_str(&serde_json::to_string(&sent).expect("encodes")).expect("decodes");
    assert_eq!(
        received,
        ExecutionReportWire {
            caused_by_sequence: 41,
            at_ms: 1_760_000_000_123,
            ownership: ExecutionOwnership::Ours,
            account: Some("A".into()),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            order_client_id: "S0001A1x".into(),
            venue_order_id: Some("v-9".into()),
            mount_id: Some("maker-1".into()),
            order: Some(ExecutionReportOrderWire {
                side: -1,
                qty: 1.0,
                order_type: "stop_limit".into(),
                status: "PARTIALLY_FILLED".into(),
                price: Some(101.0),
                trigger_price: Some(102.0),
                filled_qty: 0.25,
                average_fill_price: 101.5,
                reason: Some("the venue said so".into()),
            }),
            fill: Some(ExecutionReportFillWire {
                trade_id: "t-1".into(),
                side: -1,
                qty: 0.25,
                price: 101.5,
                commission: 0.01,
                commission_asset: "USDT".into(),
                liquidity_side: "maker".into(),
                mark_price: Some(101.4),
                position_side: Some("SHORT".into()),
            }),
            fee_correction: Some(ExecutionReportFeeCorrectionWire {
                corrects_trade_id: "t-0".into(),
                commission: 0.002,
                commission_asset: "BTC".into(),
                price: 99.5,
            }),
        }
    );

    // A foreign one-way fill with no order: no mount, no position side, an empty liquidity word.
    let foreign = ExecutionReport {
        ownership: ExecutionOwnership::Foreign,
        mount_id: None,
        strategy_code: None,
        order: None,
        fee_correction: None,
        reason: None,
        fill: Some(ExecutionReportFill {
            liquidity_side: LiquiditySide::Unknown,
            position_side: PositionSide::Both,
            ..fill
        }),
        ..report
    };
    let wire = ExecutionReportWire::from(&foreign);
    assert_eq!(wire.ownership, ExecutionOwnership::Foreign);
    assert_eq!((wire.mount_id, wire.order), (None, None));
    let fill = wire.fill.expect("the fill rides");
    assert_eq!((fill.liquidity_side.as_str(), fill.position_side), ("", None));
}
