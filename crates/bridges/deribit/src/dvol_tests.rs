//! No live network: the pure decode + the opt-in recorder gate are exercised over a
//! DataFusion-free in-crate trade-capturing [`HistStore`] double (MemHistStore's `append_trades`
//! is an inert stub, so a trade-round-trip needs this local double) plus the shared
//! `RecordingSink`.
use super::*;

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use vike_data::{DataError, ExecFillRow, ExecOrderRow, RecordingSink, SinkCall, TsRange};
use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties};

fn in_range(ts: i64, range: TsRange) -> bool {
    range.start.map(|s| ts >= s).unwrap_or(true) && range.end.map(|e| ts <= e).unwrap_or(true)
}

/// A `kind=trade`-capturing [`HistStore`]: honors the batch-level `commit_key` idempotency
/// contract (a repeated key is a no-op) and empty-batch fidelity, matching `DataFusionHist`.
/// Every other method is an inert stub — this double exists for the DVOL trade seam only.
#[derive(Default)]
struct CaptureStore {
    trades: Mutex<HashMap<(String, String), Vec<TradeTick>>>,
    seen: Mutex<HashSet<String>>,
}

impl HistStore for CaptureStore {
    fn load_bars(&self, _v: &str, _s: &str, _i: &str, _r: TsRange) -> Result<Vec<Bar>, DataError> {
        Ok(Vec::new())
    }
    fn scan_quotes(&self, _v: &str, _s: &str, _r: TsRange) -> Result<Vec<QuoteTick>, DataError> {
        Ok(Vec::new())
    }
    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<TradeTick>, DataError> {
        let map = self.trades.lock().unwrap();
        let mut out: Vec<TradeTick> = map
            .get(&(venue.to_string(), symbol.to_string()))
            .map(|rows| rows.iter().filter(|t| in_range(t.ts, range)).cloned().collect())
            .unwrap_or_default();
        out.sort_by_key(|r| r.ts);
        Ok(out)
    }
    fn append_bars(
        &self,
        _v: &str,
        _s: &str,
        _i: &str,
        _b: &[Bar],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_quotes(
        &self,
        _v: &str,
        _s: &str,
        _t: &[QuoteTick],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_trades(
        &self,
        venue: &str,
        symbol: &str,
        ticks: &[TradeTick],
        commit_key: Option<&str>,
    ) -> Result<usize, DataError> {
        if ticks.is_empty() {
            return Ok(0);
        }
        if let Some(k) = commit_key {
            let mut seen = self.seen.lock().unwrap();
            if !seen.insert(k.to_string()) {
                return Ok(0); // already ingested — batch-level no-op
            }
        }
        self.trades
            .lock()
            .unwrap()
            .entry((venue.to_string(), symbol.to_string()))
            .or_default()
            .extend_from_slice(ticks);
        Ok(ticks.len())
    }
    fn append_book_updates(
        &self,
        _v: &str,
        _s: &str,
        _u: &[BookUpdate],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_book_updates(
        &self,
        _v: &str,
        _s: &str,
        _r: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        Ok(Vec::new())
    }
    fn append_symbol_properties(
        &self,
        _v: &str,
        _s: &str,
        _r: &[(i64, SymbolProperties)],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_symbol_properties(
        &self,
        _v: &str,
        _s: &str,
        _r: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        Ok(Vec::new())
    }
    fn append_equity(
        &self,
        _v: &str,
        _s: &str,
        _r: &[EquitySample],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_equity(&self, _v: &str, _s: &str, _r: TsRange) -> Result<Vec<EquitySample>, DataError> {
        Ok(Vec::new())
    }
    fn append_exec_fills(
        &self,
        _v: &str,
        _s: &str,
        _r: &[ExecFillRow],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_exec_fills(&self, _v: &str, _s: &str) -> Result<Vec<ExecFillRow>, DataError> {
        Ok(Vec::new())
    }
    fn append_exec_orders(
        &self,
        _v: &str,
        _s: &str,
        _r: &[ExecOrderRow],
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_exec_orders(&self, _v: &str, _s: &str) -> Result<Vec<ExecOrderRow>, DataError> {
        Ok(Vec::new())
    }
    fn resample_quotes_to_bars(
        &self,
        _v: &str,
        _s: &str,
        _i: &str,
        _r: TsRange,
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn resample_trades_to_bars(
        &self,
        _v: &str,
        _s: &str,
        _i: &str,
        _r: TsRange,
        _k: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
}

/// A real Deribit `deribit_volatility_index.{index}` subscription frame.
fn dvol_frame(index: &str, ts: i64, vol: f64) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "subscription",
        "params": {
            "channel": format!("deribit_volatility_index.{index}"),
            "data": { "timestamp": ts, "volatility": vol, "index_name": index }
        }
    })
}

fn recorded_marks(sink: &RecordingSink) -> Vec<(String, String, u64, i64)> {
    sink.recorded()
        .into_iter()
        .filter_map(|c| match c {
            SinkCall::MarkTick { venue, symbol, px, ts } => Some((venue, symbol, px.to_bits(), ts)),
            _ => None,
        })
        .collect()
}

#[test]
fn parses_dvol_subscription_frame() {
    let tick = parse_dvol_frame(&dvol_frame("btc_usd", 1_700_000_000_000, 62.5)).expect("parsed");
    assert_eq!(tick.index_name, "btc_usd");
    assert_eq!(tick.ts, 1_700_000_000_000);
    assert_eq!(tick.volatility.to_bits(), 62.5f64.to_bits());
}

#[test]
fn parse_falls_back_to_channel_suffix_for_index_and_defaults_ts() {
    // No `index_name`/`timestamp` in data: index comes from the channel suffix, ts defaults 0.
    let f = serde_json::json!({
        "method": "subscription",
        "params": {
            "channel": "deribit_volatility_index.eth_usd",
            "data": { "volatility": 70.0 }
        }
    });
    let tick = parse_dvol_frame(&f).expect("parsed");
    assert_eq!(tick.index_name, "eth_usd");
    assert_eq!(tick.ts, 0);
    assert_eq!(tick.volatility.to_bits(), 70.0f64.to_bits());
}

#[test]
fn parse_rejects_non_dvol_and_malformed_frames() {
    // wrong method
    assert!(parse_dvol_frame(&serde_json::json!({"method": "heartbeat"})).is_none());
    // a user.trades subscription (wrong channel)
    assert!(
        parse_dvol_frame(&serde_json::json!({
            "method": "subscription",
            "params": {"channel": "user.trades.any.any.raw", "data": []}
        }))
        .is_none()
    );
    // a DVOL frame missing the load-bearing `volatility` field
    assert!(
        parse_dvol_frame(&serde_json::json!({
            "method": "subscription",
            "params": {"channel": "deribit_volatility_index.btc_usd", "data": {"timestamp": 1}}
        }))
        .is_none()
    );
    // the subscribe ack (a JSON-RPC response, no `method`)
    assert!(
        parse_dvol_frame(&serde_json::json!({
            "id": 1, "result": ["deribit_volatility_index.btc_usd"]
        }))
        .is_none()
    );
}

#[test]
fn channel_and_symbol_mapping() {
    assert_eq!(dvol_channel("btc"), "deribit_volatility_index.btc_usd");
    assert_eq!(dvol_channel("ETH"), "deribit_volatility_index.eth_usd");
    assert_eq!(dvol_symbol("btc_usd").as_deref(), Some("DVOL-BTC"));
    assert_eq!(dvol_symbol("eth_usd").as_deref(), Some("DVOL-ETH"));
    assert_eq!(dvol_symbol("btc"), None); // no `_usd` suffix
    assert_eq!(dvol_symbol(""), None);
}

#[test]
fn dvol_tick_maps_to_a_trade_row() {
    let row =
        dvol_trade_tick(&DvolTick { index_name: "btc_usd".into(), ts: 123, volatility: 55.5 });
    assert_eq!(row.ts, 123);
    assert_eq!(row.local_ts, 123);
    assert_eq!(row.price.to_bits(), 55.5f64.to_bits());
    assert_eq!(row.size.to_bits(), 0.0f64.to_bits());
    assert!(!row.is_buyer_maker);
}

#[test]
fn on_frame_emits_mark_tick_and_records_when_enabled() {
    let sink = RecordingSink::default();
    let store = Arc::new(CaptureStore::default());
    let rec = DvolRecorder::new(store.clone(), true);

    let txt = dvol_frame("btc_usd", 1_700_000_000_000, 62.5).to_string();
    assert_eq!(on_dvol_frame(&txt, &sink, Some(&rec)), FrameOutcome::Confirm);

    // live seam: exactly one mark tick under the synthetic DVOL-BTC symbol
    assert_eq!(
        recorded_marks(&sink),
        vec![("deribit".into(), "DVOL-BTC".into(), 62.5f64.to_bits(), 1_700_000_000_000)]
    );
    // store: one trade row, the DVOL value carried as price
    let got = store.scan_trades("deribit", "DVOL-BTC", TsRange::all()).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].price.to_bits(), 62.5f64.to_bits());
    assert_eq!(got[0].ts, 1_700_000_000_000);
    assert_eq!(got[0].size.to_bits(), 0.0f64.to_bits());
}

#[test]
fn on_frame_emits_mark_tick_but_records_nothing_when_recorder_disabled() {
    let sink = RecordingSink::default();
    let store = Arc::new(CaptureStore::default());
    let rec = DvolRecorder::new(store.clone(), false);

    let txt = dvol_frame("eth_usd", 1_700_000_000_000, 70.0).to_string();
    assert_eq!(on_dvol_frame(&txt, &sink, Some(&rec)), FrameOutcome::Confirm);

    // the live-seam mark tick STILL flows (recording is the only opt-in half)
    assert_eq!(
        recorded_marks(&sink),
        vec![("deribit".into(), "DVOL-ETH".into(), 70.0f64.to_bits(), 1_700_000_000_000)]
    );
    // recording OFF ⇒ the store is untouched (byte-identical to no recorder)
    assert!(store.scan_trades("deribit", "DVOL-ETH", TsRange::all()).unwrap().is_empty());
}

#[test]
fn on_frame_records_nothing_with_no_recorder() {
    let sink = RecordingSink::default();
    // None recorder: mark tick flows, nothing recorded (the default / unwired shape).
    assert_eq!(
        on_dvol_frame(&dvol_frame("btc_usd", 1, 60.0).to_string(), &sink, None),
        FrameOutcome::Confirm
    );
    assert_eq!(recorded_marks(&sink).len(), 1);
}

#[test]
fn on_frame_ignores_junk_and_non_dvol_frames() {
    let sink = RecordingSink::default();
    assert_eq!(on_dvol_frame("not json", &sink, None), FrameOutcome::Ignore);
    assert_eq!(on_dvol_frame(r#"{"id":1,"result":["ok"]}"#, &sink, None), FrameOutcome::Ignore);
    assert!(recorded_marks(&sink).is_empty(), "an ignored frame emits nothing");
}

#[test]
fn record_same_bucket_twice_is_one_row_next_bucket_lands() {
    let store = Arc::new(CaptureStore::default());
    let rec = DvolRecorder::new(store.clone(), true); // default 60 s cadence
    let t0: i64 = 60_000 * 28_000_000; // a minute-bucket boundary
    let tick = |ts: i64, vol: f64| DvolTick { index_name: "btc_usd".into(), ts, volatility: vol };

    rec.record("DVOL-BTC", &tick(t0, 60.0));
    rec.record("DVOL-BTC", &tick(t0 + 30_000, 61.0)); // same minute bucket → dropped
    assert_eq!(store.scan_trades("deribit", "DVOL-BTC", TsRange::all()).unwrap().len(), 1);

    rec.record("DVOL-BTC", &tick(t0 + 60_000, 62.0)); // next minute → a second sample lands
    assert_eq!(store.scan_trades("deribit", "DVOL-BTC", TsRange::all()).unwrap().len(), 2);
}

#[test]
fn disabled_record_writes_nothing() {
    let store = Arc::new(CaptureStore::default());
    let rec = DvolRecorder::new(store.clone(), false);
    rec.record("DVOL-BTC", &DvolTick { index_name: "btc_usd".into(), ts: 1, volatility: 60.0 });
    assert!(store.scan_trades("deribit", "DVOL-BTC", TsRange::all()).unwrap().is_empty());
}

/// The FLAG gate + the cadence parse, driven over an injected map — nothing here touches the
/// process env (`std::env::set_var` is an `unsafe fn` since edition 2024, which this workspace
/// forbids), so no scheduling of sibling tests can race it either.
#[test]
fn from_flag_gate_and_cadence() {
    let store = Arc::new(CaptureStore::default());
    let vars = |v: Option<&str>| -> std::collections::HashMap<String, String> {
        v.map(|v| (RECORD_DVOL_CADENCE_ENV.to_string(), v.to_string())).into_iter().collect()
    };
    let rec = |enabled: bool, v: Option<&str>| {
        DvolRecorder::from_flag_and_vars(store.clone(), enabled, &vars(v))
    };

    // The gate is the PARAMETER, and nothing else: no environment state can flip it, which is
    // the whole point of taking the caller's decision instead of re-reading `VIKE_RECORD_DVOL`
    // here (a variable that now configures nothing anywhere — `vike_config::DEAD_FLAG_KEYS`).
    assert!(!rec(false, None).enabled(), "flag off → disabled");
    assert!(rec(true, None).enabled(), "flag on → enabled");

    // cadence parse/clamp (read independently of the enable gate)
    assert_eq!(rec(true, None).cadence_ms(), DEFAULT_DVOL_CADENCE_MS);
    assert_eq!(rec(true, Some("5000")).cadence_ms(), 5_000);
    assert_eq!(rec(true, Some("0")).cadence_ms(), 1, "0 clamps to 1 ms");
    assert_eq!(
        rec(true, Some("not-a-number")).cadence_ms(),
        DEFAULT_DVOL_CADENCE_MS,
        "unparsable falls back to the default"
    );
    // A disabled recorder still resolves its cadence — the two are independent reads.
    assert_eq!(rec(false, Some("5000")).cadence_ms(), 5_000);
}

#[test]
fn public_subscribe_frame_shape() {
    let sub = build_public_subscribe(&[dvol_channel("btc"), dvol_channel("eth")]);
    let v: Value = serde_json::from_str(&sub).unwrap();
    assert_eq!(v.get("method").and_then(|m| m.as_str()), Some("public/subscribe"));
    let channels = v.pointer("/params/channels").and_then(|c| c.as_array()).unwrap();
    assert_eq!(channels.len(), 2);
    assert_eq!(channels[0].as_str(), Some("deribit_volatility_index.btc_usd"));
    assert_eq!(channels[1].as_str(), Some("deribit_volatility_index.eth_usd"));
}
