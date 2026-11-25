use std::time::Instant;

use serde::Serialize;
use vike_analytics::TradingRecord;
use vike_analytics::record::{TradingRecordSimulator, TradingRecordSource};
use vike_analytics::results::TradingResults;
use vike_hub_protocol::frame::{BodyCodec, decode_body, encode_reply};
use vike_model::{OrderDrop, Trade};

use super::*;
use crate::summary::{CurvePageWire, RunSummaryWire, TradesPageWire, summarize};
use crate::wire_studio::TradingRecordWire;

const FIRST_BAR_MS: i64 = 1_733_011_200_000;
const BAR_MS: i64 = 900_000;

/// A seeded walk whose values carry every digit a real equity does, a flat step every seventh (ties).
fn walk(n: usize) -> Vec<f64> {
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut equity = 10_000.0;
    (0..n)
        .map(|i| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            if !i.is_multiple_of(7) {
                equity *= 1.0 + ((state >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 0.006;
            }
            equity
        })
        .collect()
}

fn trade(i: usize) -> Trade {
    let entry = 96_515.51 + i as f64 * 1.375_25;
    Trade {
        entry_price: entry,
        exit_price: entry * 1.003_141_59,
        size: 1.0,
        pnl: entry * 0.003_141_59 - 193.65,
        fees: 193.654_321,
        entry_timestamp: FIRST_BAR_MS + (i as i64) * 30 * BAR_MS,
        exit_timestamp: FIRST_BAR_MS + (i as i64) * 30 * BAR_MS + 7 * BAR_MS,
        symbol: "BTCUSDT".to_string(),
        mae: 0.012_345_678,
        mfe: 0.023_456_789,
        is_long: !i.is_multiple_of(3),
    }
}

/// The JOAT run's refusals in shape (1,530 rows over 1,386 distinct reasons, each embedding its quantity), worded
/// at the length of the spot law's own sentence.
fn joat_refusals() -> Vec<(OrderDrop, u64)> {
    (0..1_530u64)
        .map(|i| {
            let drop = OrderDrop {
                symbol: if i % 2 == 0 { "BTCUSDT" } else { "ETHUSDT" }.to_string(),
                reason: format!(
                    "spot cannot sell: the account holds 0 of the base asset, and this market sell asks \
                     0.{:04} — a spot account sells only what it holds",
                    i % 1_386
                ),
                size: i as f64 * 0.000_1,
                weight: 1.0,
            };
            (drop, 1 + i % 3)
        })
        .collect()
}

/// A whole answer of the JOAT run's shape: 66,786 steps, 400 trades, its refusals.
fn joat_whole_answer() -> TradingRecordWire {
    let equity_curve = walk(66_786);
    TradingRecordWire {
        final_equity: *equity_curve.last().unwrap(),
        equity_timestamp: (0..equity_curve.len() as i64)
            .map(|i| FIRST_BAR_MS + i * BAR_MS)
            .collect(),
        equity_curve,
        n_trades: 400,
        per_symbol_pnl: vec![("BTCUSDT".to_string(), -10_611.989), ("ETHUSDT".to_string(), 0.0)],
        trades: (0..400).map(trade).collect(),
        refused_orders: joat_refusals(),
        ..TradingRecordWire::default()
    }
}

/// A long run's `TradingResults`: 20,000 steps, 400 trades, the JOAT refusals.
fn long_results(name: &str) -> TradingResults {
    let equity_curve = walk(20_000);
    let record = TradingRecord {
        final_equity: *equity_curve.last().unwrap(),
        equity_timestamp: (0..20_000).map(|i| FIRST_BAR_MS + i * BAR_MS).collect(),
        equity_curve,
        trades: (0..400).map(trade).collect(),
        n_trades: 400,
        source: TradingRecordSource::Backtest(TradingRecordSimulator {
            dropped: joat_refusals().into_iter().map(|(drop, _)| drop).collect(),
            ..Default::default()
        }),
        ..Default::default()
    };
    TradingResults::from_record(Some(name.to_string()), &record, 35_040.0)
}

/// A parameter scan's report in its document's shape: rows best first, each its overrides and its own results.
fn sweep_report_text(points: usize) -> String {
    #[derive(Serialize)]
    struct Row {
        overrides: Vec<(String, f64)>,
        results: TradingResults,
    }
    #[derive(Serialize)]
    struct Report {
        rank_by: &'static str,
        rows: Vec<Row>,
    }
    let base = long_results("scan");
    let rows = (0..points)
        .map(|i| Row {
            overrides: vec![("fast".to_string(), 5.0 + i as f64), ("slow".to_string(), 50.0)],
            results: TradingResults { sharpe: base.sharpe + i as f64 * 0.01, ..base.clone() },
        })
        .collect();
    serde_json::to_string(&Report { rank_by: "sharpe", rows }).expect("a report encodes")
}

/// Odd values, every one canonical `serde_json` output: extremes, a negative zero, escapes, unicode, mixed and
/// empty lists, a table, rows of pairs, keys out of sorted order, a key twice, a list of floats with a null.
fn odd_text() -> String {
    let rows: Vec<String> =
        (0..5).map(|i| format!("{{\"z\":{i},\"a\":{}.5,\"m\":null}}", i)).collect();
    format!(
        "{{\"zeta\":-0.0,\"alpha\":[1e+300,5e-324,-1.7976931348623157e+308,0.1,1.0],\"big\":[18446744073709551615,\
         -9223372036854775808,0,7],\"text\":\"tab\\t quote\\\" slash\\\\ \\u0001 é ✓\",\"empty\":[],\"none\":{{}},\
         \"mixed\":[1,2.5,\"x\",true,null],\"pairs\":[[\"a\",1],[\"b\",2],[\"c\",3]],\"table\":[{}],\"twice\":1,\
         \"twice\":2,\"floats\":[1.5,null,2.5],\"nested\":[[[1.0,2.0],[3.0,4.0]],[[5.0,6.0],[7.0,8.0]]],\
         \"small\":[1,2,3,4,5,6,7,8,9],\"stamps\":[{FIRST_BAR_MS},{},{}]}}",
        rows.join(","),
        FIRST_BAR_MS + BAR_MS,
        FIRST_BAR_MS + 2 * BAR_MS,
    )
}

/// Every text report variant, holding `text`.
fn every_report(text: &str) -> Vec<Response> {
    let t = || text.to_string();
    vec![
        Response::Report(t()),
        Response::ParamscanReport(t()),
        Response::WalkforwardReport(t()),
        Response::CombinatorialWalkforwardReport(t()),
        Response::PermutationReport(t()),
        Response::StudyReport(t()),
    ]
}

/// The text a report variant holds.
fn text_of(response: &Response) -> &str {
    text_of_any(response).expect("a text report")
}

/// The text a text report holds, or `None` for any other response.
fn text_of_any(response: &Response) -> Option<&str> {
    match response {
        Response::Report(t)
        | Response::ParamscanReport(t)
        | Response::WalkforwardReport(t)
        | Response::CombinatorialWalkforwardReport(t)
        | Response::PermutationReport(t)
        | Response::StudyReport(t) => Some(t),
        _ => None,
    }
}

/// A msgpack reply body as the backtesthub writes it.
fn msgpack_reply(response: &Response) -> Vec<u8> {
    encode_reply(BodyCodec::Msgpack, &StructuredReports(response)).expect("encodes")[4..].to_vec()
}

/// **A report crosses msgpack as its tree and reads back as the VERY text**, for every report variant and every
/// shape: a real report, a scan's rows, and the odd values. The body holds no JSON string of the report, and a JSON
/// writer of [`StructuredReports`] writes the derive's bytes. MUTATION PROOF: write the tree through
/// `serde_json::Value` (sorted keys), or drop the `Negative`/`Unsigned` split, and the text differs.
#[test]
fn a_report_crosses_as_its_tree_and_reads_back_as_its_very_text() {
    let real = serde_json::to_string(&long_results("long")).expect("a report encodes");
    for text in [real, sweep_report_text(3), odd_text()] {
        for response in every_report(&text) {
            let body = msgpack_reply(&response);
            assert!(
                rmp_serde::from_slice::<std::collections::BTreeMap<String, String>>(&body).is_err(),
                "the report crossed as text, not as its tree"
            );
            let back: Response = decode_body(&body).expect("decodes");
            assert_eq!(text_of(&back), text, "the very text");
            assert_eq!(
                serde_json::to_string(&StructuredReports(&response)).unwrap(),
                serde_json::to_string(&response).unwrap(),
                "a JSON frame of a report is today's bytes"
            );
        }
    }
}

/// A report that is not a JSON object or list, or not JSON at all, crosses as its text and reads back as it.
#[test]
fn a_report_that_is_no_tree_crosses_as_its_text() {
    for text in ["not json", "\"a string\"", "42", ""] {
        let back: Response =
            decode_body(&msgpack_reply(&Response::Report(text.to_string()))).expect("decodes");
        assert_eq!(text_of(&back), text);
    }
}

/// Random documents, every one `serde_json`'s own output, cross msgpack and come back to the character.
#[test]
fn random_documents_come_back_to_the_character() {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move |n: u64| {
        state =
            state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) % n
    };
    fn value(next: &mut impl FnMut(u64) -> u64, depth: u32) -> serde_json::Value {
        use serde_json::{Value, json};
        let kind = if depth > 3 { next(5) } else { next(9) };
        match kind {
            0 => json!(null),
            1 => json!(next(2) == 1),
            2 => json!(next(1 << 40) as i64 - (1 << 39)),
            3 => json!(f64::from_bits(
                next(u64::MAX).wrapping_mul(0x9e37_79b9) & 0x7fef_ffff_ffff_ffff
            )),
            4 => json!(format!("s{}\n\"{}", next(1000), next(7))),
            5 => (0..next(12)).map(|_| json!(next(1000) as f64 / 7.0)).collect::<Value>(),
            6 => (0..next(6)).map(|_| value(next, depth + 1)).collect::<Value>(),
            7 => {
                let width = next(4) + 1;
                (0..next(8))
                    .map(|_| {
                        (0..width)
                            .map(|k| (format!("k{k}"), value(next, depth + 1)))
                            .collect::<Value>()
                    })
                    .collect::<Value>()
            }
            _ => (0..next(5)).map(|k| (format!("f{k}"), value(next, depth + 1))).collect::<Value>(),
        }
    }
    for _ in 0..400 {
        let doc = serde_json::json!({ "root": value(&mut next, 0), "list": value(&mut next, 1) });
        let text = serde_json::to_string(&doc).expect("encodes");
        let back: Response =
            decode_body(&msgpack_reply(&Response::Report(text.clone()))).expect("decodes");
        assert_eq!(text_of(&back), text);
    }
}

/// The columns' wire: a list of floats is one `F64_COLUMN`, a table writes its keys once, small integers stay
/// msgpack's own. Shorter than the JSON text it carries.
#[test]
fn columns_write_a_curve_once_and_a_tables_keys_once() {
    let curve: Vec<f64> = walk(1_000);
    let text = serde_json::to_string(&serde_json::json!({ "curve": curve })).unwrap();
    let body = msgpack_reply(&Response::Report(text.clone()));
    assert!(body.len() < 1_000 * 8 + 64, "a float column is eight bytes a value: {}", body.len());
    let rows: Vec<serde_json::Value> = (0..200)
        .map(|i| serde_json::json!({ "entry_price": i as f64 + 0.5, "symbol": "BTCUSDT" }))
        .collect();
    let table = serde_json::to_string(&serde_json::json!({ "rows": rows })).unwrap();
    let body = msgpack_reply(&Response::Report(table.clone()));
    let keys = body.windows(b"entry_price".len()).filter(|w| *w == b"entry_price").count();
    assert_eq!(keys, 1, "a table's key crosses once");
    assert!(body.len() * 2 < table.len(), "{} against {}", body.len(), table.len());
}

/// A garbled column, or a table whose columns do not line up, is refused, never read short.
#[test]
fn a_garbled_column_is_refused() {
    let mut odd = vec![0x81, 0xa6];
    odd.extend_from_slice(b"Report");
    let refuse = |payload: &[u8]| {
        let mut body = odd.clone();
        body.extend_from_slice(payload);
        rmp_serde::from_slice::<Response>(&body).is_err()
    };
    assert!(refuse(&[0x91, 0xc4, 0x03, F64_COLUMN, 1, 2]), "a float column of three bytes");
    assert!(refuse(&[0x91, 0xc4, 0x01, 9]), "an unknown head");
    // [TABLE, ["a"], [1, 2], [3]]: two columns for one key.
    assert!(
        refuse(&[0x94, 0xc4, 0x01, TABLE, 0x91, 0xa1, b'a', 0x92, 1, 2, 0x91, 3]),
        "misaligned"
    );
    // [ROWS, [1, 2], [3]]: columns of two lengths.
    assert!(refuse(&[0x93, 0xc4, 0x01, ROWS, 0x92, 1, 2, 0x91, 3]), "misaligned rows");
    // [1, TABLE]: a head inside a list.
    assert!(refuse(&[0x92, 1, 0xc4, 0x01, TABLE]), "a head inside a list");
}

/// The typed columns: a curve page, the whole answer and a trades page read back to the bit through msgpack (the
/// sandbox pipe's and a msgpack reply's form), and their JSON is today's.
#[test]
fn the_typed_columns_read_back_to_the_bit_and_keep_their_json() {
    let page = CurvePageWire {
        total: 3,
        page: 0,
        page_size: 3,
        equity: vec![1.0, -0.0, f64::NAN],
        timestamp: vec![i64::MIN, 0, i64::MAX],
    };
    let back: CurvePageWire =
        rmp_serde::from_slice(&rmp_serde::to_vec_named(&page).unwrap()).unwrap();
    let bits = |v: &[f64]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
    assert_eq!(bits(&back.equity), bits(&page.equity));
    assert_eq!(back.timestamp, page.timestamp);
    assert_eq!(
        serde_json::to_string(&CurvePageWire {
            equity: vec![1.0, 2.5],
            timestamp: vec![1, 2],
            ..page
        })
        .unwrap(),
        "{\"total\":3,\"page\":0,\"page_size\":3,\"equity\":[1.0,2.5],\"timestamp\":[1,2]}"
    );
    let whole = joat_whole_answer();
    let back: TradingRecordWire =
        rmp_serde::from_slice(&rmp_serde::to_vec_named(&whole).unwrap()).unwrap();
    assert_eq!(back, whole);
    let trades = TradesPageWire::of(&whole.trades, 0, 200);
    let back: TradesPageWire =
        rmp_serde::from_slice(&rmp_serde::to_vec_named(&trades).unwrap()).unwrap();
    assert_eq!(back, trades);
    let (summary, _) = summarize(whole, 35_040.0);
    let back: RunSummaryWire =
        rmp_serde::from_slice(&rmp_serde::to_vec_named(&summary).unwrap()).unwrap();
    // JSON, not `==`: the results' `source` and `period` are `#[serde(skip)]`, lost by either codec alike.
    assert_eq!(serde_json::to_string(&back).unwrap(), serde_json::to_string(&summary).unwrap());
    assert_eq!(
        bits(&back.curve.iter().map(|p| p.1).collect::<Vec<_>>()),
        bits(&summary.curve.iter().map(|p| p.1).collect::<Vec<_>>())
    );
}

/// **The JSON path and the msgpack path read the same summary and print the same `--json`**: the JOAT-shaped
/// summary decoded from a JSON frame and from a msgpack one (plain and LZ4) is one value, and
/// `serde_json::to_string` of it (what `vike-cli backtest run --json` prints) is one string.
#[test]
fn the_json_path_and_the_msgpack_path_read_one_summary() {
    let (summary, _) = summarize(joat_whole_answer(), 35_040.0);
    let response = Response::RunSummary(Box::new(summary));
    let json: Response =
        decode_body(&encode_reply(BodyCodec::Json, &StructuredReports(&response)).unwrap()[4..])
            .unwrap();
    for codec in [BodyCodec::Msgpack, BodyCodec::MsgpackLz4] {
        let body = encode_reply(codec, &StructuredReports(&response)).unwrap();
        let msgpack: Response = decode_body(&body[4..]).unwrap();
        let (Response::RunSummary(a), Response::RunSummary(b)) = (&json, &msgpack) else {
            panic!("a summary")
        };
        assert_eq!(format!("{a:?}"), format!("{b:?}"), "{codec:?}");
        assert_eq!(serde_json::to_string(a).unwrap(), serde_json::to_string(b).unwrap());
    }
}

/// The median of `runs` timings of `work`, in microseconds.
fn median_micros(runs: usize, mut work: impl FnMut()) -> u128 {
    let mut took: Vec<u128> = (0..runs)
        .map(|_| {
            let start = Instant::now();
            work();
            start.elapsed().as_micros()
        })
        .collect();
    took.sort_unstable();
    took[runs / 2]
}

/// **Bytes and time, before and after** (run on a lane with `--ignored --nocapture`, release): each message's JSON
/// frame (today) against its msgpack and LZ4 bodies, and the encode-and-decode time of each, the median of 15.
#[test]
#[ignore = "a measurement: prints bytes and times, asserts only that every form reads back"]
fn bytes_and_time_before_and_after() {
    let (summary, _) = summarize(joat_whole_answer(), 35_040.0);
    let report = serde_json::to_string(&long_results("long")).unwrap();
    let sweep = sweep_report_text(40);
    let page = CurvePageWire::of(&walk(263_000), &(0..263_000).collect::<Vec<i64>>(), 0, 263_000);
    let messages = [
        ("JOAT summary (RunSummary)", Response::RunSummary(Box::new(summary))),
        ("long report (Report)", Response::Report(report)),
        ("40-point sweep (ParamscanReport)", Response::ParamscanReport(sweep)),
        ("JOAT whole answer (RunResult)", Response::RunResult(joat_whole_answer())),
        ("263k-step curve page (CurvePage)", Response::CurvePage(page)),
    ];
    eprintln!("| message | codec | report as | bytes | encode+decode µs |");
    eprintln!("|---|---|---|---|---|");
    for (name, response) in &messages {
        for codec in [BodyCodec::Json, BodyCodec::Msgpack, BodyCodec::MsgpackLz4] {
            // A report crosses as its tree (what the daemon writes) or as its text in a msgpack string (the derive).
            for tree in [true, false] {
                let encode = || match tree {
                    true => encode_reply(codec, &StructuredReports(response)).unwrap(),
                    false => encode_reply(codec, response).unwrap(),
                };
                if !tree && (codec == BodyCodec::Json || text_of_any(response).is_none()) {
                    continue;
                }
                let body = encode();
                let back: Response = decode_body(&body[4..]).unwrap();
                assert_eq!(
                    serde_json::to_string(&back).unwrap(),
                    serde_json::to_string(response).unwrap(),
                    "{name} reads back in {codec:?}"
                );
                let micros = median_micros(15, || {
                    std::hint::black_box(decode_body::<Response>(&encode()[4..]).unwrap());
                });
                let form = if text_of_any(response).is_none() {
                    "-"
                } else if tree && codec != BodyCodec::Json {
                    "tree"
                } else {
                    "text"
                };
                eprintln!("| {name} | {codec:?} | {form} | {} | {micros} |", body.len() - 4);
            }
        }
    }
}
