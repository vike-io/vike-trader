//! **The DESTRUCTIVE verb's input validation, driven through the REAL `DeleteSeries` arm over a
//! loopback socket** — not through `removal`'s pure helpers, because the defect this file pins was
//! a gate the SERVER stood down while every helper below it behaved exactly as documented.
//!
//! # What a client could do before this file existed
//!
//! `crates/vike-datahub/src/server.rs`'s `delete_series_verb` gated its sweep on
//! `produced_by.is_none()`. The wire field is `Option<String>`, so a BLANK value is `Some("")` —
//! not absent — and it satisfied the gate. It then satisfied the provenance check too, vacuously:
//! `crates/vike-data/src/store_kind.rs`'s `key_matches_prefix` is `starts_with`, and every key
//! starts with the empty string. Two guards, one token, on a verb that takes the only copy.
//!
//! `crates/vike-data/src/store_kind.rs`'s `resolve_produced_by` had refused exactly that spelling
//! since it was written and said so in its own doc. It had ONE caller in the tree
//! (`crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series`, the LOCAL route) and the server
//! was not it.
//!
//! # ⚠ `MemHistStore` cannot demonstrate this, and a test over it would have passed before the fix
//!
//! That double inherits the REFUSING trait defaults for `series_commits` and
//! `delete_series_checked`, so today's code already answers `Response::Error` over it — for an
//! unrelated reason. A test asserting `Response::Error` alone would have been green against the
//! defect. Hence [`PlantedStore`]: a double that CAN answer provenance and CAN delete, and records
//! what it deleted, so "the wildcard delete happened" is what a failure says.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use vike_data::removal::SeriesSelector;
use vike_data::store_kind::key_matches_prefix;
use vike_data::{
    DataError, ExecFillRow, ExecOrderRow, HistStore, SeriesCoverage, SeriesId, TsRange,
};
use vike_datahub::serve_authed;
use vike_datahub_client::node_auth::{self, DATAHUB_DOMAIN, NodeKeys, Scope};
use vike_datahub_client::proto::{PROTO_VERSION, Request, Response, read_frame, write_frame};
use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

const OBSERVE_KEY: &[u8] = b"observe-key-observe-key-observe-";
const CONTROL_KEY: &[u8] = b"control-key-control-key-control-";

/// The commit key every planted series records — a real `klines:` prefix, so a NON-blank assertion
/// over it genuinely succeeds and the tests below are not all measuring the same refusal.
const PLANTED_COMMIT: &str = "klines:binance:BTCUSDT:1h:2026-09-01";

// ------------------------------------------------------------------------------------------------
// The double
// ------------------------------------------------------------------------------------------------

/// A `HistStore` that enumerates a planted set, reports one commit key per series, and RECORDS
/// every `delete_series_checked` it was asked to perform.
///
/// ⚠ Its `delete_series_checked` honours the same `starts_with` re-check
/// `crates/vike-data/src/datafusion_hist.rs`'s `delete_series_checked` performs under the series
/// lock. Without that the double would be MORE permissive than the real store, and every absence
/// this file asserts would be an absence the production path might not have.
struct PlantedStore {
    series: Vec<SeriesId>,
    deleted: Mutex<Vec<SeriesId>>,
}

impl PlantedStore {
    fn new(series: Vec<SeriesId>) -> Self {
        Self { series, deleted: Mutex::new(Vec::new()) }
    }
    fn deleted(&self) -> Vec<SeriesId> {
        self.deleted.lock().unwrap().clone()
    }
}

impl HistStore for PlantedStore {
    fn list_series(&self) -> Result<Vec<SeriesId>, DataError> {
        Ok(self.series.clone())
    }
    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, DataError> {
        Ok(self
            .series
            .iter()
            .map(|id| (id.clone(), SeriesCoverage { rows: 1, ..SeriesCoverage::default() }))
            .collect())
    }
    fn series_commits(&self, _id: &SeriesId) -> Result<Vec<String>, DataError> {
        Ok(vec![PLANTED_COMMIT.to_string()])
    }
    fn delete_series_checked(
        &self,
        id: &SeriesId,
        require_produced_by: Option<&str>,
    ) -> Result<(), DataError> {
        if let Some(prefix) = require_produced_by
            && !key_matches_prefix(PLANTED_COMMIT, prefix)
        {
            return Err(DataError::Query(format!("{} carries a foreign key", id.kind)));
        }
        self.deleted.lock().unwrap().push(id.clone());
        Ok(())
    }

    // ---- inert stubs: this double exists for the delete path and nothing else -----------------
    fn load_bars(&self, _: &str, _: &str, _: &str, _: TsRange) -> Result<Vec<Bar>, DataError> {
        Ok(Vec::new())
    }
    fn scan_quotes(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<QuoteTick>, DataError> {
        Ok(Vec::new())
    }
    fn scan_trades(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<TradeTick>, DataError> {
        Ok(Vec::new())
    }
    fn append_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[Bar],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_quotes(
        &self,
        _: &str,
        _: &str,
        _: &[QuoteTick],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_trades(
        &self,
        _: &str,
        _: &str,
        _: &[TradeTick],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn append_book_updates(
        &self,
        _: &str,
        _: &str,
        _: &[BookUpdate],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_book_updates(
        &self,
        _: &str,
        _: &str,
        _: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        Ok(Vec::new())
    }
    fn append_symbol_properties(
        &self,
        _: &str,
        _: &str,
        _: &[(i64, SymbolProperties)],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_symbol_properties(
        &self,
        _: &str,
        _: &str,
        _: TsRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
        Ok(Vec::new())
    }
    fn append_equity(
        &self,
        _: &str,
        _: &str,
        _: &[EquitySample],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_equity(&self, _: &str, _: &str, _: TsRange) -> Result<Vec<EquitySample>, DataError> {
        Ok(Vec::new())
    }
    fn append_exec_fills(
        &self,
        _: &str,
        _: &str,
        _: &[ExecFillRow],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_exec_fills(&self, _: &str, _: &str) -> Result<Vec<ExecFillRow>, DataError> {
        Ok(Vec::new())
    }
    fn append_exec_orders(
        &self,
        _: &str,
        _: &str,
        _: &[ExecOrderRow],
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn scan_exec_orders(&self, _: &str, _: &str) -> Result<Vec<ExecOrderRow>, DataError> {
        Ok(Vec::new())
    }
    fn resample_quotes_to_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TsRange,
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
    fn resample_trades_to_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TsRange,
        _: Option<&str>,
    ) -> Result<usize, DataError> {
        Ok(0)
    }
}

// ------------------------------------------------------------------------------------------------
// Harness
// ------------------------------------------------------------------------------------------------

fn keys() -> NodeKeys {
    NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec())
}

fn planted() -> Vec<SeriesId> {
    ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
        .into_iter()
        .map(|s| SeriesId::per_symbol("bar", "binance", s, Some("1h".to_string())))
        .collect()
}

/// A KEYED server over a planted store. Keyed because `delete_series_verb` refuses a key-LESS one
/// outright and this file is about the check AFTER that one.
fn spawn(store: Arc<PlantedStore>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let served: Arc<dyn HistStore + Send + Sync> = store;
    thread::spawn(move || {
        let _ = serve_authed(listener, served, None, Some(keys()), None);
    });
    addr
}

fn control_stream(addr: SocketAddr) -> TcpStream {
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut s).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce.expect("a keyed server's Welcome carries a nonce"),
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = node_auth::sign(
        DATAHUB_DOMAIN,
        keys().key_for(Scope::Control),
        &nonce,
        PROTO_VERSION,
        Scope::Control,
    );
    write_frame(&mut s, &Request::Auth { scope: Scope::Control, mac }).expect("auth");
    match read_frame::<_, Response>(&mut s).expect("auth answer") {
        Response::AuthOk { .. } => {}
        other => panic!("expected AuthOk, got {other:?}"),
    }
    s
}

fn delete(
    addr: SocketAddr,
    selector: SeriesSelector,
    produced_by: Option<&str>,
    dry_run: bool,
) -> Response {
    let mut s = control_stream(addr);
    let req =
        Request::DeleteSeries { selector, produced_by: produced_by.map(str::to_string), dry_run };
    write_frame(&mut s, &req).expect("write DeleteSeries");
    read_frame::<_, Response>(&mut s).expect("read the answer")
}

fn error_text(r: &Response) -> &str {
    match r {
        Response::Error(m) => m.as_str(),
        other => panic!("expected Response::Error, got {other:?}"),
    }
}

// ------------------------------------------------------------------------------------------------
// The floor — this harness can actually delete, so every absence below means something
// ------------------------------------------------------------------------------------------------

/// ⚠ **The non-vacuity floor.** Every test below asserts that NOTHING was deleted; if the harness
/// could never delete anything they would all pass against a server that refused every request for
/// any reason at all. This one proves a legitimate assertion still goes through and still removes
/// the planted series.
#[test]
fn the_suite_can_actually_delete() {
    let store = Arc::new(PlantedStore::new(planted()));
    let addr = spawn(Arc::clone(&store));
    let r = delete(addr, SeriesSelector::new("bar", "binance"), Some("klines:"), false);
    match r {
        Response::Deleted(done) => {
            let outcome = done.outcome.expect("a non-dry run carries an outcome");
            assert_eq!(outcome.deleted.len(), 3, "{outcome:?}");
        }
        other => panic!("a valid literal prefix must be served, got {other:?}"),
    }
    assert_eq!(store.deleted().len(), 3, "the store really removed them");
}

// ------------------------------------------------------------------------------------------------
// The blank assertion
// ------------------------------------------------------------------------------------------------

/// **A BLANK `produced_by` is refused AT THE SERVER, and nothing is deleted.**
///
/// Fails against the pre-fix server as `Response::Deleted` with three series removed — and the
/// recorder assertion is what makes the failure message say *the wildcard delete happened* rather
/// than *the variant was wrong*.
#[test]
fn a_blank_produced_by_is_refused_at_the_server_and_deletes_nothing() {
    let store = Arc::new(PlantedStore::new(planted()));
    let addr = spawn(Arc::clone(&store));
    let r = delete(addr, SeriesSelector::new("bar", "binance"), Some(""), false);
    let msg = error_text(&r);
    assert!(msg.contains("BLANK"), "the refusal names what was wrong: {msg}");
    assert!(
        msg.contains("matches every key"),
        "...and why an empty prefix is not an assertion: {msg}"
    );
    assert!(store.deleted().is_empty(), "a wildcard delete happened: {:?}", store.deleted());
}

/// **A WHITESPACE `produced_by` is refused the same way** — and this is the test that had to be
/// written carefully.
///
/// ⚠ Against the PRE-FIX server `Some("   ")` already answered `Response::Error`, because a
/// whitespace prefix is a literal prefix matching no key and `RemovalPlan::verdict` refuses it. A
/// `matches!(_, Response::Error(_))` assertion would therefore have been GREEN against the defect —
/// a needle matching a substring of the wrong answer, which this repository has been bitten by
/// twice. Two discriminations instead:
///
/// 1. the message must carry the BLANK sentence and must NOT carry `provenance REFUSED`, which is
///    the pre-fix answer's own wording;
/// 2. the `dry_run: true` arm changes VARIANT rather than text: the server's dry-run path returns
///    the plan WITHOUT consulting `RemovalPlan::verdict`, so before the fix a blank dry run
///    rendered a plan reading `provenance: SATISFIED`.
#[test]
fn a_whitespace_produced_by_is_refused_the_same_way() {
    let store = Arc::new(PlantedStore::new(planted()));
    let addr = spawn(Arc::clone(&store));

    let r = delete(addr, SeriesSelector::new("bar", "binance"), Some("   "), false);
    let msg = error_text(&r);
    assert!(msg.contains("BLANK"), "the refusal is about the ASSERTION, not the data: {msg}");
    assert!(
        !msg.contains("provenance REFUSED"),
        "this must not be the plan-time verdict's refusal — that one reads as a finding about the \
         operator's store: {msg}"
    );
    assert!(store.deleted().is_empty(), "{:?}", store.deleted());

    // The VARIANT discrimination: a dry run answered `Deleted` before the fix.
    let dry = delete(addr, SeriesSelector::new("bar", "binance"), Some("\t \n"), true);
    let msg = error_text(&dry);
    assert!(msg.contains("BLANK"), "a dry run is refused at the same door: {msg}");
}

/// **The refusal is about the ASSERTION, not about the sweep** — so a fully-named series is refused
/// too, where the sweep gate is not in play at all.
///
/// Fails against the pre-fix server as `Deleted`, with the one named series removed.
#[test]
fn a_blank_produced_by_is_refused_for_a_fully_named_series_too() {
    let store = Arc::new(PlantedStore::new(planted()));
    let addr = spawn(Arc::clone(&store));
    let selector = SeriesSelector {
        kind: "bar".into(),
        venue: "binance".into(),
        symbol: Some("BTCUSDT".into()),
        group: None,
        interval: Some("1h".into()),
    };
    assert!(!selector.is_sweep(), "the fixture must not be a sweep, or this proves nothing");
    let r = delete(addr, selector, Some(""), false);
    assert!(error_text(&r).contains("BLANK"), "{r:?}");
    assert!(store.deleted().is_empty(), "{:?}", store.deleted());
}

// ------------------------------------------------------------------------------------------------
// Regression pins on the reorder
// ------------------------------------------------------------------------------------------------

/// **The sweep gate still refuses a MISSING assertion.** Passes before AND after — it is a
/// REGRESSION pin on moving the sweep gate behind the resolver, not a fail-first test, and it is
/// labelled so nobody counts it as one.
#[test]
fn the_sweep_gate_still_refuses_a_missing_assertion() {
    let store = Arc::new(PlantedStore::new(planted()));
    let addr = spawn(Arc::clone(&store));
    let r = delete(addr, SeriesSelector::new("bar", "binance"), None, false);
    let msg = error_text(&r);
    assert!(msg.contains("SWEEP"), "{msg}");
    assert!(store.deleted().is_empty(), "{:?}", store.deleted());
}

/// **A declared producer PATH is RESOLVED by the server**, the way the local route has always
/// resolved it. `crates/vike-data/src/demo.rs` is a declared producer in `STORE_KINDS` whose
/// commit-key template carries the `demo-tape:` prefix.
///
/// Fails against the pre-fix server: the path was asserted LITERALLY, no key started with it, and
/// the operator was told their store carried foreign provenance — a wrong answer that reads as a
/// serious finding about the data, which is exactly what let it survive.
///
/// Here the resolved prefix does NOT match the planted `klines:` key, so the correct answer is a
/// provenance refusal naming the resolved prefix rather than the path. That is the discrimination:
/// before the fix the message quoted the PATH, after it the resolved PREFIX.
#[test]
fn a_declared_producer_path_is_resolved_by_the_server() {
    let store = Arc::new(PlantedStore::new(planted()));
    let addr = spawn(Arc::clone(&store));
    let r = delete(
        addr,
        SeriesSelector::new("bar", "binance"),
        Some("crates/vike-data/src/demo.rs"),
        false,
    );
    let msg = error_text(&r);
    assert!(
        msg.contains("demo-tape:"),
        "the server resolved the path to the prefix STORE_KINDS declares: {msg}"
    );
    assert!(
        !msg.contains("crates/vike-data/src/demo.rs"),
        "...and did not assert the PATH literally: {msg}"
    );
    assert!(store.deleted().is_empty(), "{:?}", store.deleted());
}

/// **An UNDECLARED producer path is refused by name**, rather than asserted literally and reported
/// as foreign data. Fails closed: nothing is deleted either way, but the message is the difference
/// between "your argument was never resolved" and "your store is foreign".
#[test]
fn an_undeclared_producer_path_is_refused_by_name() {
    let store = Arc::new(PlantedStore::new(planted()));
    let addr = spawn(Arc::clone(&store));
    let r = delete(
        addr,
        SeriesSelector::new("bar", "binance"),
        Some("crates/vike-data/src/no_such_producer.rs"),
        false,
    );
    let msg = error_text(&r);
    assert!(msg.contains("STORE_KINDS"), "the refusal names the roster it consulted: {msg}");
    assert!(store.deleted().is_empty(), "{:?}", store.deleted());
}
