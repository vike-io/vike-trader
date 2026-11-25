use super::*;
use crate::proto::Response;
use crate::wire_studio::{ParamscanEntryWire, StudioParamscanWire, TradingRecordWire};
use vike_analytics::metrics::max_drawdown;
use vike_analytics::results::TradingResults;
use vike_model::OrderDrop;

/// The 16:43 run's shape (decision 0078's amendment): the "SMA cross" template over binance BTCUSDT
/// 15m on the CI box, 2026-11-16: 66,786 bars from 2024-12-01, 2,194 closed trades.
const BARS: usize = 66_786;
const TRADES: usize = 2_194;
const FIRST_BAR_MS: i64 = 1_733_011_200_000;
const BAR_MS: i64 = 900_000;

/// A deterministic walk whose values carry every digit a real equity does (a seeded LCG, no
/// randomness crate).
fn walk(n: usize, start: f64) -> Vec<f64> {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut v = start;
    (0..n)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let step = ((state >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 0.006;
            v *= 1.0 + step;
            v
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
        is_long: true,
    }
}

/// A whole answer of the 16:43 run's size.
fn whole_answer() -> TradingRecordWire {
    let equity_curve = walk(BARS, 10_000.0);
    TradingRecordWire {
        final_equity: *equity_curve.last().unwrap(),
        equity_timestamp: (0..BARS as i64).map(|i| FIRST_BAR_MS + i * BAR_MS).collect(),
        equity_curve,
        n_trades: TRADES,
        per_symbol_pnl: vec![("BTCUSDT".to_string(), -10_611.989)],
        trades: (0..TRADES).map(trade).collect(),
        ..TradingRecordWire::default()
    }
}

fn frame_len(response: &Response) -> usize {
    serde_json::to_vec(response).expect("a response encodes").len()
}

/// The owner's measurement, held: the whole answer of the 16:43 run was about 2.8 MB; its summary
/// must stay under 100 KB, in the very frame the backtesthub writes.
#[test]
fn the_sixteen_forty_three_run_answers_in_under_a_hundred_kilobytes() {
    let whole = whole_answer();
    let whole_len = frame_len(&Response::RunResult(whole.clone()));
    let (summary, trades) = summarize(whole, 35_040.0);
    let summary_len = frame_len(&Response::RunSummary(Box::new(summary.clone())));
    eprintln!("whole answer {whole_len} bytes, summary {summary_len} bytes");
    assert!(whole_len > 2_500_000, "the whole answer is the 2.8 MB one: {whole_len}");
    assert!(summary_len < 100_000, "the summary is under 100 KB: {summary_len}");
    assert_eq!(trades.len(), TRADES, "every trade stays with the caller, none on the wire");
    assert_eq!(summary.curve_len, BARS);
    assert!(summary.curve.len() <= CURVE_POINTS + 4, "{}", summary.curve.len());
    assert_eq!(summary.n_trades, TRADES);
}

/// A summary carries the results' ONE label, built from the record with the strategy file attached
/// (`TradingResults::source_label`): the words `vike-cli report run:` heads the same run with. A
/// run whose answer named no file is labelled `(unnamed)`, never left unlabelled.
#[test]
fn a_summary_carries_the_results_label_naming_its_strategy_file() {
    let whole = TradingRecordWire {
        equity_curve: vec![10_000.0, 10_100.0],
        equity_timestamp: vec![FIRST_BAR_MS, FIRST_BAR_MS + BAR_MS],
        final_equity: 10_100.0,
        strategy_file: "trend/momentum.rs".to_string(),
        ..TradingRecordWire::default()
    };
    let (summary, _) = summarize(whole.clone(), 35_040.0);
    assert_eq!(
        summary.source_label.as_deref(),
        Some("backtest trend/momentum.rs 2024-12-01 00:00 – 2024-12-01 00:15 UTC")
    );
    let json = serde_json::to_string(&summary).unwrap();
    let label = summary.source_label.unwrap();
    assert!(json.contains(&format!(r#""source_label":"{label}","results":{{"#)), "{json}");

    let unnamed = TradingRecordWire { strategy_file: String::new(), ..whole };
    let (summary, _) = summarize(unnamed, 35_040.0);
    assert_eq!(
        summary.source_label.as_deref(),
        Some("backtest (unnamed) 2024-12-01 00:00 – 2024-12-01 00:15 UTC")
    );
}

/// The results table crosses optionally (the owner's word of 2026-11-19): a summary carrying it decodes to
/// the same bytes, one without it has no `results` key at all, and that frame (an older server's) decodes to
/// a summary without it.
#[test]
fn the_results_table_round_trips_with_and_without_it() {
    let (with, _) = summarize(whole_answer(), 35_040.0);
    let table = with.results.as_ref().expect("summarize fills the table");
    assert_eq!(table.n_trades, with.n_trades);
    let json = serde_json::to_string(&with).unwrap();
    let decoded: RunSummaryWire = serde_json::from_str(&json).unwrap();
    assert!(decoded.results.is_some());
    assert_eq!(serde_json::to_string(&decoded).unwrap(), json, "the same bytes back");

    let without = RunSummaryWire { results: None, ..with };
    let json = serde_json::to_string(&without).unwrap();
    assert!(!json.contains(r#""results""#), "{json}");
    let decoded: RunSummaryWire = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.results, None);
    assert_eq!(serde_json::to_string(&decoded).unwrap(), json, "the same bytes back");
}

/// The thinned curve's maximum drawdown is the whole curve's, to the bit, on a walk and on the case
/// a plain per-bucket min/max misses: a peak that is not its bucket's highest point (a higher point
/// later in the same bucket) followed by the trough.
#[test]
fn the_thinned_curve_keeps_the_true_maximum_drawdown() {
    let mut curves = vec![walk(BARS, 10_000.0), walk(5_001, 1.0)];
    let mut tricky = vec![100.0; 4_000];
    tricky[2_000] = 150.0; // the peak
    tricky[2_001] = 60.0; // the trough
    tricky[2_002] = 160.0; // the bucket's highest point, after the trough
    curves.push(tricky);
    for curve in curves {
        let thinned: Vec<f64> =
            downsample(&curve, CURVE_POINTS).into_iter().map(|(_, v)| v).collect();
        assert!(thinned.len() < curve.len());
        assert_eq!(
            max_drawdown(&thinned).to_bits(),
            max_drawdown(&curve).to_bits(),
            "thinned {} of {} points",
            thinned.len(),
            curve.len()
        );
    }
}

/// The thinned curve keeps both ends, the global extremes, and its indices in order.
#[test]
fn the_thinned_curve_keeps_both_ends_and_the_extremes_in_step_order() {
    let curve = walk(BARS, 10_000.0);
    let thinned = downsample(&curve, CURVE_POINTS);
    assert_eq!(thinned.first(), Some(&(0, curve[0])));
    assert_eq!(thinned.last(), Some(&(BARS - 1, curve[BARS - 1])));
    assert!(thinned.windows(2).all(|w| w[0].0 < w[1].0));
    let high = curve.iter().copied().fold(f64::MIN, f64::max);
    let low = curve.iter().copied().fold(f64::MAX, f64::min);
    assert!(thinned.iter().any(|&(_, v)| v == high));
    assert!(thinned.iter().any(|&(_, v)| v == low));
    let short = walk(10, 1.0);
    assert_eq!(downsample(&short, CURVE_POINTS).len(), 10, "a short curve crosses whole");
}

/// Pages cut the trades in close order, the last page short, a page past the end empty, a zero size
/// the default and an oversized one held at the cap.
#[test]
fn trades_page_in_close_order() {
    let trades: Vec<Trade> = (0..TRADES).map(trade).collect();
    let mut seen = Vec::new();
    for page in 0..=TRADES / TRADES_PAGE_SIZE {
        let cut = TradesPageWire::of(&trades, page, TRADES_PAGE_SIZE);
        assert_eq!(cut.total, TRADES);
        assert_eq!(cut.page, page);
        seen.extend(cut.trades);
    }
    assert_eq!(seen, trades, "the pages, joined, are every trade once, in order");
    assert_eq!(TradesPageWire::of(&trades, 10, 200).trades.len(), 194);
    assert!(TradesPageWire::of(&trades, 11, 200).trades.is_empty());
    assert!(TradesPageWire::of(&trades, usize::MAX, 200).trades.is_empty());
    assert_eq!(TradesPageWire::of(&trades, 0, 0).page_size, TRADES_PAGE_SIZE);
    assert_eq!(TradesPageWire::of(&trades, 0, 5_000).trades.len(), TRADES_PAGE_SIZE_MAX);
}

/// The whole answer of a 24-point scan of the 16:43 run's size is at the frame cap (this fixture's
/// point is a little under the real run's 2.8 MB, so 24 of them stop just short and 26 pass it); its
/// summary, every point's numbers and the best point's summary, is about a hundred kilobytes whatever
/// the point count.
#[test]
fn a_scan_at_the_frame_cap_fits_as_a_summary() {
    const POINTS: i64 = 26;
    let whole = whole_answer();
    let one_point = frame_len(&Response::ParamscanResult(StudioParamscanWire {
        entries: vec![ParamscanEntryWire { overrides: vec![], result: whole.clone() }],
        dsr: None,
        pbo: None,
        best_index: 0,
        cost_model: None,
    }));
    let cap = vike_hub_protocol::frame::MAX_FRAME_LEN as usize;
    assert!(24 * one_point > cap * 95 / 100, "24 whole points are at the cap: {}", 24 * one_point);
    assert!(POINTS as usize * one_point > cap, "{POINTS} pass it: {}", POINTS as usize * one_point);
    let (best, _) = summarize(whole, 35_040.0);
    let points = (0..POINTS)
        .map(|i| {
            ParamscanPointWire::of(
                vec![
                    ("fast".to_string(), ParamValue::Int(i)),
                    ("slow".to_string(), ParamValue::Float(20.0 + i as f64)),
                ],
                &best,
            )
        })
        .collect();
    let scan = ParamscanSummaryWire {
        run: "18f2a3-1".to_string(),
        points,
        best: Some(best),
        dsr: Some(0.5),
        pbo: Some(0.25),
        best_index: 0,
        cost_model: None,
        answered_from_saved_run: String::new(),
    };
    let len = frame_len(&Response::ParamscanSummary(Box::new(scan)));
    eprintln!(
        "one whole point {one_point} bytes; {POINTS}-point scan: whole {} bytes, summary {len} bytes",
        POINTS as usize * one_point
    );
    assert!(len < 110_000, "{len}");
}

/// The histogram is the Distribution tab's old fold: every return in one of the bins.
#[test]
fn the_histogram_bins_every_return() {
    let curve = walk(1_000, 100.0);
    let returns = metrics::returns(&curve);
    let histogram = ReturnsHistogramWire::of(&returns);
    assert_eq!(histogram.counts.len(), HISTOGRAM_BINS);
    assert_eq!(histogram.counts.iter().sum::<u64>(), returns.len() as u64);
    assert_eq!(ReturnsHistogramWire::of(&[]), ReturnsHistogramWire::default());
}

/// Refusals are counted by reason alone, across symbols and sizes, in first-seen order, with the
/// server's counts kept exact.
#[test]
fn refusals_count_by_reason_in_first_seen_order() {
    let drop = |symbol: &str, reason: &str, size: f64| OrderDrop {
        symbol: symbol.to_string(),
        reason: reason.to_string(),
        size,
        weight: 1.0,
    };
    let whole = TradingRecordWire {
        refused_orders: vec![
            (drop("BTCUSDT", "spot cannot sell", 1.0), 40_000),
            (drop("ETHUSDT", "cash gate", 2.0), 3),
            (drop("ETHUSDT", "spot cannot sell", 2.0), 2),
        ],
        ..TradingRecordWire::default()
    };
    let (summary, _) = summarize(whole, 252.0);
    assert_eq!(
        summary.refused,
        vec![("spot cannot sell".to_string(), 40_002), ("cash gate".to_string(), 3)]
    );
}

/// The sentence a group's words and an example's numbers came from: `vike_analytics::zero_trade::denial_sentence`,
/// which reads a reason's own `#` (written `##`) back too.
fn put_back(words: &str, numbers: &[String]) -> String {
    vike_analytics::zero_trade::denial_sentence(words, numbers)
}

/// **A summary's refusals are grouped by their reason without its numbers, and cross ONCE** (decision 0078's
/// amendment of 2026-11-19): [`many_refusals`]' 1,386 sentences, each naming its own quantity, are one reason. Its
/// count is the total the rows counted (saturating), no denial is lost (every sentence is the group's words with its
/// own numbers put back, and the examples are sentences of the run), and the results table's honesty block carries
/// none of them: `refused` is the one list.
#[test]
fn refusals_group_by_their_words_and_cross_once() {
    let rows = many_refusals();
    let total: u64 = rows.iter().map(|(_, n)| n).sum();
    let words = "spot cannot sell: it has #, this sells #";
    let whole = TradingRecordWire { refused_orders: rows.clone(), ..whole_answer() };
    let (summary, _) = summarize(whole, 35_040.0);
    assert_eq!(summary.refused, vec![(words.to_string(), total)]);
    for (drop, _) in &rows {
        let (template, numbers) = vike_analytics::zero_trade::denial_template(&drop.reason);
        assert_eq!((template.as_str(), put_back(words, &numbers)), (words, drop.reason.clone()));
    }
    let [detail] = summary.refused_detail.as_slice() else { panic!("one group, one detail") };
    assert_eq!((detail.first_ms, detail.last_ms), (None, None), "the wire carries no times");
    let examples: Vec<String> =
        detail.examples.iter().map(|e| put_back(words, &e.numbers)).collect();
    let sentences: Vec<String> = rows.iter().take(3).map(|(drop, _)| drop.reason.clone()).collect();
    assert_eq!(examples, sentences, "the first three, their numbers lifted out");
    assert_eq!(detail.examples[1].symbol, "ETHUSDT");
    let honesty = summary.results.as_ref().and_then(|r| r.honesty.as_ref()).expect("honesty");
    assert!(
        honesty.denials.is_empty() && honesty.denial_details.is_empty(),
        "the list crosses once"
    );

    let mut saturated = rows;
    saturated.push((saturated[5].0.clone(), u64::MAX));
    let whole = TradingRecordWire { refused_orders: saturated, ..TradingRecordWire::default() };
    assert_eq!(summarize(whole, 35_040.0).0.refused, vec![(words.to_string(), u64::MAX)]);
}

/// **A refused order of a non-finite size does not cost the summary its decode**: JSON writes a NaN or an infinity
/// as `null`, which no `f64` reads back, so `refused_detail` keeps an example's size only when finite. The summary of
/// a full record with a NaN-size and an infinite-size refusal crosses JSON and comes back equal.
#[test]
fn a_non_finite_refused_size_round_trips_through_the_summary() {
    let mut record = from_wire_record(whole_answer());
    let vike_analytics::TradingRecordSource::Backtest(simulator) = &mut record.source else {
        panic!("a backtest's record")
    };
    let drop = |size: f64| OrderDrop {
        symbol: "BTCUSDT".to_string(),
        reason: "risk gate: max notional".to_string(),
        size,
        weight: 1.0,
    };
    simulator.dropped = vec![drop(f64::NAN), drop(f64::INFINITY)];
    simulator.dropped_at = vec![FIRST_BAR_MS, FIRST_BAR_MS + BAR_MS];
    let (summary, _) = summarize_record(whole_answer(), record, None, 35_040.0);
    let frame = serde_json::to_string(&summary).expect("a summary encodes");
    let back: RunSummaryWire = serde_json::from_str(&frame).expect("and decodes");
    assert_eq!(back.refused, vec![("risk gate: max notional".to_string(), 2)]);
    assert_eq!(back.refused_detail[0].examples[0].size, None, "no size rather than a false one");
    assert_eq!(back.refused_detail, summary.refused_detail);
}

/// **An older client decodes the new summary** (decision 0078's amendment of 2026-11-19): the fields it knows keep
/// their types, the one it does not (`refused_detail`, the honesty block's `denial_details`) it skips, and it still
/// shows the denials in `refused`. `Old` is `RunSummaryWire` as it stood before the amendment, its honesty block
/// as an older `TradingResultsHonesty` had it.
#[test]
fn an_older_client_decodes_the_new_summary() {
    #[derive(serde::Deserialize)]
    struct OldHonesty {
        intrabar_both_hit: u32,
        warmup: usize,
        denials: Vec<(String, u64)>,
    }
    #[derive(serde::Deserialize)]
    struct OldResults {
        n_trades: usize,
        honesty: Option<OldHonesty>,
    }
    #[derive(serde::Deserialize)]
    struct Old {
        rows: Vec<(String, String)>,
        curve: Vec<(usize, f64)>,
        refused: Vec<(String, u64)>,
        results: Option<OldResults>,
    }
    let mut record = from_wire_record(whole_answer());
    let vike_analytics::TradingRecordSource::Backtest(simulator) = &mut record.source else {
        panic!("a backtest's record")
    };
    simulator.dropped = many_refusals().into_iter().map(|(drop, _)| drop).collect();
    simulator.dropped_at = (0..simulator.dropped.len() as i64).map(|i| FIRST_BAR_MS + i).collect();
    simulator.warmup = 6;
    let (summary, _) = summarize_record(whole_answer(), record, None, 35_040.0);
    assert!(!summary.refused_detail.is_empty(), "the new field is in the frame");
    let frame = serde_json::to_string(&summary).expect("a summary encodes");
    let old: Old = serde_json::from_str(&frame).expect("an older client decodes it");
    assert_eq!((old.rows, old.curve, old.refused), (summary.rows, summary.curve, summary.refused));
    let results = old.results.expect("the results table");
    let honesty = results.honesty.expect("its honesty block");
    assert_eq!((results.n_trades, honesty.intrabar_both_hit, honesty.warmup), (TRADES, 0, 6));
    assert!(honesty.denials.is_empty(), "the denials are in `refused`, once");
}

/// **A whole answer whose equity reached zero summarizes INVALID** (owner 2026-11-16): the words name the day, its
/// rows open with the INVALID row, it carries no Sharpe, return, PSR or return histogram, and a scan point of it is
/// INVALID too. A healthy answer carries no mark.
#[test]
fn a_run_whose_equity_reached_zero_summarizes_invalid() {
    let whole = TradingRecordWire {
        equity_curve: vec![10_000.0, 4_000.0, -50.0, 30.0],
        equity_timestamp: (0..4).map(|i| FIRST_BAR_MS + i * BAR_MS).collect(),
        final_equity: 30.0,
        n_trades: 2,
        ..TradingRecordWire::default()
    };
    let (summary, _) = summarize(whole, 35_040.0);
    let why = "equity reached 0 on 2024-12-01: metrics are not meaningful";
    assert_eq!(summary.invalid.as_deref(), Some(why));
    assert_eq!(summary.rows[0], (vike_analytics::INVALID_LABEL.to_string(), why.to_string()));
    let ratios = (summary.sharpe, summary.total_return, summary.probabilistic_sharpe);
    assert_eq!(ratios, (None, None, None));
    assert!(summary.returns_histogram.counts.is_empty());
    assert_eq!(ParamscanPointWire::of(Vec::new(), &summary).invalid.as_deref(), Some(why));

    let (healthy, _) = summarize(whole_answer(), 35_040.0);
    assert_eq!(healthy.invalid, None);
    assert!(healthy.sharpe.is_some());
}

/// FNV-1a 64 of `bytes`, as 16 hex digits: what the golden below pins a whole summary by.
fn fnv(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{hash:016x}")
}

/// A profiled run's refusals in shape (2026-11-19): 1,530 rows over 1,386 distinct reasons, each reason embedding
/// its quantity, the first 144 repeated across two symbols.
fn many_refusals() -> Vec<(OrderDrop, u64)> {
    (0..1_530u64)
        .map(|i| {
            let drop = OrderDrop {
                symbol: if i % 2 == 0 { "BTCUSDT" } else { "ETHUSDT" }.to_string(),
                reason: format!("spot cannot sell: it has 0, this sells 0.{:04}", i % 1_386),
                size: i as f64 * 0.000_1,
                weight: 1.0,
            };
            (drop, 1 + i % 3)
        })
        .collect()
}

/// **A long run's summary and trades, pinned by the FNV-1a of their bytes**: the 16:43 run's shape with a flat step
/// every seventh bar (so its returns tie) and [`many_refusals`]. Pinned against the tree BEFORE the summary read its
/// Sharpe, return and PSR moments off its report, indexed its refusals by reason and sorted its returns unstably
/// (2026-11-19): each is the same function over the same values, so no byte may move. Never re-pin it to pass.
///
/// The server's summary of the same run's FULL record ([`summarize_record`]) has a pin of its own, taken 2026-11-19
/// when the server stopped summarizing the decoded record (decision 0078's amendment of that day): it carries the
/// closed-trade block and the four simulator counters the decoded record drops, so its bytes differ from the
/// decoded one's by those and by nothing the fixes above moved.
///
/// ⚠ Both re-pinned ONCE, 2026-11-19, for a change the owner ordered, not to pass (decision 0078's amendment on the
/// denials): the 1,386 refusal sentences are one reason now, its numbers lifted out, sent once in `refused` with its
/// detail beside it and no longer in the results table's honesty block. The pins before were `64a0e7ed4756dfe9` and
/// `34e4f1a8936807f3`.
#[test]
fn a_long_runs_summary_is_pinned() {
    let mut whole = whole_answer();
    for i in (7..whole.equity_curve.len()).step_by(7) {
        whole.equity_curve[i] = whole.equity_curve[i - 1];
    }
    whole.final_equity = *whole.equity_curve.last().unwrap();
    whole.refused_orders = many_refusals();
    let (mut summary, trades) = summarize(whole.clone(), 35_040.0);
    assert_eq!(summary.refused.len(), 1);
    // The curve's times (2026-11-20) are new bytes BESIDE the pinned ones: held to the run's own timestamps here
    // and taken out of the pin, so no byte the pin guards may move.
    let kept: Vec<i64> =
        summary.curve.iter().map(|&(step, _)| whole.equity_timestamp[step]).collect();
    assert_eq!(std::mem::take(&mut summary.curve_timestamp), kept);
    let json = serde_json::to_string(&(summary, trades)).expect("a summary encodes");
    assert_eq!(fnv(json.as_bytes()), "3f8ff26eca47e396");

    let mut record = from_wire_record(whole.clone());
    record.closed_trades = Some(Default::default());
    let vike_analytics::TradingRecordSource::Backtest(counters) = &mut record.source else {
        panic!("the wire decodes a backtest's record")
    };
    (counters.intrabar_both_hit, counters.impact_unpriced) = (3, 4);
    (counters.below_min_reversals, counters.warmup) = (5, 6);
    // Handed the report a saved run computed over the same record, the summary is the bytes it computes itself.
    let report = TradingResults::from_record(None, &record, 35_040.0);
    let shared = summarize_record(whole.clone(), record.clone(), Some(report), 35_040.0);
    let (summary, trades) = summarize_record(whole, record, None, 35_040.0);
    let encoded = |s: &(RunSummaryWire, Vec<Trade>)| serde_json::to_string(s).expect("encodes");
    assert_eq!(encoded(&shared), encoded(&(summary.clone(), trades.clone())));
    let results = summary.results.as_ref().expect("the results table");
    let honesty = results.honesty.as_ref().expect("the honesty block");
    let read = (honesty.intrabar_both_hit, honesty.impact_unpriced, honesty.below_min_reversals);
    assert_eq!((read, honesty.warmup), ((3, 4, 5), 6), "every counter, none read as zero");
    assert!(results.closed_trades.is_some(), "the closed-trade block");
    assert!(summary.rows.iter().any(|(id, _)| id.starts_with("closed_")), "and its rows");
    assert_eq!(summary.refused.len(), 1);
    let mut summary = summary;
    assert_eq!(
        std::mem::take(&mut summary.curve_timestamp),
        kept,
        "the same times, out of the pin"
    );
    let json = serde_json::to_string(&(summary, trades)).expect("a summary encodes");
    assert_eq!(fnv(json.as_bytes()), "0728310984a0229a");
}

/// A writer that only counts, so a frame's length is measured without holding the frame.
struct Count(usize);

impl std::io::Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The JSON length of `response`, the frame body the hub writes.
fn encoded_len(response: &Response) -> usize {
    let mut count = Count(0);
    serde_json::to_writer(&mut count, response).expect("a response encodes");
    count.0
}

/// One-minute steps from 2024-01-01: `steps` of them, the walk's equity and each step's timestamp.
fn minute_curve(steps: usize) -> (Vec<f64>, Vec<i64>) {
    let equity = walk(steps, 100_000.0);
    (equity, (0..steps as i64).map(|i| 1_704_067_200_000 + i * 60_000).collect())
}

/// `equity` and `timestamp` read back the way `crate::BacktesthubClient::read_whole_curve` reads them:
/// every page encoded and decoded as its own frame, at `page_size`. Answers the curve and the largest
/// frame.
fn through_pages(
    equity: &[f64],
    timestamp: &[i64],
    page_size: usize,
) -> (Vec<f64>, Vec<i64>, usize) {
    let (mut back_equity, mut back_timestamp, mut largest) = (Vec::new(), Vec::new(), 0);
    for page in 0.. {
        let frame = serde_json::to_vec(&Response::CurvePage(CurvePageWire::of(
            equity, timestamp, page, page_size,
        )))
        .expect("a page encodes");
        largest = largest.max(frame.len());
        let Ok(Response::CurvePage(part)) = serde_json::from_slice(&frame) else {
            panic!("a page decodes")
        };
        if part.equity.is_empty() {
            break;
        }
        back_equity.extend(part.equity);
        back_timestamp.extend(part.timestamp);
    }
    (back_equity, back_timestamp, largest)
}

fn bits(curve: &[f64]) -> Vec<u64> {
    curve.iter().map(|v| v.to_bits()).collect()
}

/// **The sizes of decision 0078's amendment of 2026-11-18**, on a 32-month 1-minute run (2024-01-01 to
/// 2026-09-01, 1,402,560 steps, 2,194 trades): the whole answer is tens of megabytes, near the frame cap;
/// the summary is under 100 KB; and the whole curve still reads back, bit for bit, in pages each far
/// under the cap. The numbers print under `--nocapture`.
#[test]
fn a_thirty_two_month_minute_run_answers_a_summary_and_pages_its_whole_curve() {
    const STEPS: usize = 974 * 1440;
    let (equity, timestamp) = minute_curve(STEPS);
    let whole = TradingRecordWire {
        final_equity: *equity.last().unwrap(),
        equity_curve: equity.clone(),
        equity_timestamp: timestamp.clone(),
        n_trades: TRADES,
        trades: (0..TRADES).map(trade).collect(),
        ..TradingRecordWire::default()
    };
    let periods = vike_analytics::results::periods_per_year_for_interval("1m");
    let whole_len = encoded_len(&Response::RunResult(whole.clone()));
    let report = TradingResults::from_record(None, &from_wire_record(whole.clone()), periods);
    let report_len = serde_json::to_vec(&report).expect("a report encodes").len();
    let (summary, _) = summarize(whole, periods);
    let summary_len = encoded_len(&Response::RunSummary(Box::new(summary)));
    let (back_equity, back_timestamp, largest) =
        through_pages(&equity, &timestamp, CURVE_PAGE_SIZE);
    eprintln!(
        "32 months of 1m: whole RunResult {whole_len} bytes, report {report_len} bytes, summary \
         {summary_len} bytes, largest curve page {largest} bytes ({} pages)",
        STEPS.div_ceil(CURVE_PAGE_SIZE)
    );
    assert!(whole_len > 40_000_000, "the whole answer is the tens of megabytes: {whole_len}");
    assert!(summary_len < 100_000, "the summary is under 100 KB: {summary_len}");
    assert_eq!(bits(&back_equity), bits(&equity), "the pages are the curve, bit for bit");
    assert_eq!(back_timestamp, timestamp);
    assert!(largest < 4_000_000, "a default page is a few megabytes: {largest}");
}

/// **A curve past the frame cap reads back whole**: five years of 1-minute steps, whose whole answer
/// exceeds `vike_hub_protocol::frame::MAX_FRAME_LEN` and could not be written at all, cross as pages of
/// [`CURVE_PAGE_SIZE_MAX`] each under the cap, and put end to end they are the curve to the bit. A page
/// past the end is empty, a zero size is the default and an oversized one is held at the cap; a run with
/// no timestamps pages none.
#[test]
fn a_curve_past_the_frame_cap_reads_back_whole_in_pages() {
    const STEPS: usize = 1826 * 1440;
    let cap = vike_hub_protocol::frame::MAX_FRAME_LEN as usize;
    let (equity, timestamp) = minute_curve(STEPS);
    let whole = TradingRecordWire {
        equity_curve: equity.clone(),
        equity_timestamp: timestamp.clone(),
        ..TradingRecordWire::default()
    };
    let whole_len = encoded_len(&Response::RunResult(whole));
    assert!(whole_len > cap, "the whole answer is past the cap: {whole_len} > {cap}");
    let (back_equity, back_timestamp, largest) =
        through_pages(&equity, &timestamp, CURVE_PAGE_SIZE_MAX);
    assert!(largest < cap, "every page is under the cap: {largest}");
    assert_eq!(bits(&back_equity), bits(&equity));
    assert_eq!(back_timestamp, timestamp);

    let last = STEPS / CURVE_PAGE_SIZE_MAX;
    let tail = CurvePageWire::of(&equity, &timestamp, last, CURVE_PAGE_SIZE_MAX);
    assert_eq!((tail.total, tail.equity.len()), (STEPS, STEPS % CURVE_PAGE_SIZE_MAX));
    assert!(CurvePageWire::of(&equity, &timestamp, usize::MAX, 9).equity.is_empty());
    assert_eq!(CurvePageWire::of(&equity, &timestamp, 0, 0).page_size, CURVE_PAGE_SIZE);
    assert_eq!(CurvePageWire::of(&equity, &[], 0, usize::MAX).equity.len(), CURVE_PAGE_SIZE_MAX);
    assert!(CurvePageWire::of(&equity, &[], 1, 10).timestamp.is_empty());
}

/// The curve's times (2026-11-20): one a kept point, the run's own, in time order from its first bar to its last; a
/// run that kept no timestamps has none. A walk-forward's stitched curve thins its times the same way.
#[test]
fn a_thinned_curve_carries_each_kept_points_time() {
    let whole = whole_answer();
    let (summary, _) = summarize(whole.clone(), 35_040.0);
    assert_eq!(summary.curve_timestamp.len(), summary.curve.len());
    for (&(step, _), &time) in summary.curve.iter().zip(&summary.curve_timestamp) {
        assert_eq!(time, FIRST_BAR_MS + step as i64 * BAR_MS);
    }
    assert!(summary.curve_timestamp.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(summary.curve_timestamp.first(), whole.equity_timestamp.first());
    assert_eq!(summary.curve_timestamp.last(), whole.equity_timestamp.last());
    let untimed = TradingRecordWire { equity_timestamp: Vec::new(), ..whole.clone() };
    assert!(summarize(untimed, 35_040.0).0.curve_timestamp.is_empty());

    let walk = WalkforwardResultsWire {
        steps: Vec::new(),
        overall: summary.results.clone().expect("the results table"),
        equity_curve: whole.equity_curve.clone(),
        equity_timestamp: whole.equity_timestamp.clone(),
        profitable_step_share: 0.0,
        account_currency: None,
        cost_model: None,
    };
    let thinned = WalkforwardSummaryWire::of(walk);
    assert_eq!(thinned.equity_curve, summary.curve);
    assert_eq!(thinned.equity_curve_timestamp, summary.curve_timestamp);
}

/// A scan point's maximum drawdown is its own run's: its summary's results', the whole curve's.
#[test]
fn a_scan_point_carries_its_runs_maximum_drawdown() {
    let whole = whole_answer();
    let (summary, _) = summarize(whole.clone(), 35_040.0);
    let point = ParamscanPointWire::of(Vec::new(), &summary);
    let drawdown = point.max_drawdown.expect("a finite drawdown");
    assert_eq!(drawdown.to_bits(), max_drawdown(&whole.equity_curve).to_bits());
    assert!(drawdown > 0.0, "the walk draws down");
    let older = RunSummaryWire { results: None, ..summary };
    assert_eq!(ParamscanPointWire::of(Vec::new(), &older).max_drawdown, None);
}

/// **A long walk's WHOLE answer carries its stitched times in memory only** (the review of 2026-11-20): five years of
/// minute samples, timed, answer the very bytes the untimed walk answers, the Studio reply and the profile's text
/// report (what a saved run keeps) alike, under the frame cap; only the summary carries times, thinned beside its
/// curve. A time a sample on the wire would have taken this walk past `MAX_FRAME_LEN`.
#[test]
fn a_long_walks_whole_answer_carries_no_times_and_fits_the_frame_cap() {
    use vike_analytics::walkforward::WalkforwardResults;
    use vike_hub_protocol::frame::{BodyCodec, MAX_FRAME_LEN};
    const STEPS: usize = 1826 * 1440;
    let (equity, timestamp) = minute_curve(STEPS);
    let overall = TradingResults::from_record(None, &TradingRecord::default(), 252.0);
    let untimed = WalkforwardResultsWire {
        steps: Vec::new(),
        overall: overall.clone(),
        equity_curve: equity.clone(),
        equity_timestamp: Vec::new(),
        profitable_step_share: 0.5,
        account_currency: None,
        cost_model: None,
    };
    let timed = WalkforwardResultsWire { equity_timestamp: timestamp.clone(), ..untimed.clone() };
    let frame = |walk: WalkforwardResultsWire| {
        let mut frame = Vec::new();
        crate::msgpack_body::write_reply(
            &mut frame,
            BodyCodec::Json,
            &Response::WalkforwardResult(Box::new(walk)),
        )
        .expect("the whole answer is written: under the frame cap");
        frame
    };
    let whole = frame(timed.clone());
    eprintln!("a five-year minute walk's whole answer: {} bytes", whole.len());
    assert!(whole.len() < MAX_FRAME_LEN as usize);
    assert!(whole == frame(untimed), "not a byte for the times");

    let report = |equity_timestamp: Vec<i64>| {
        serde_json::to_string(&WalkforwardResults {
            steps: Vec::new(),
            overall: overall.clone(),
            equity_curve: equity.clone(),
            equity_timestamp,
            profitable_step_share: 0.5,
            account_currency: None,
        })
        .expect("the report encodes")
    };
    let text = report(timestamp.clone());
    assert!(text.len() < MAX_FRAME_LEN as usize, "{}", text.len());
    assert!(text == report(Vec::new()), "the text report and the saved run carry no times");

    let summary = WalkforwardSummaryWire::of(timed);
    assert_eq!(summary.equity_curve_timestamp.len(), summary.equity_curve.len());
    let kept: Vec<i64> = summary.equity_curve.iter().map(|&(step, _)| timestamp[step]).collect();
    assert_eq!(
        summary.equity_curve_timestamp, kept,
        "the summary thins the times beside its curve"
    );
}
