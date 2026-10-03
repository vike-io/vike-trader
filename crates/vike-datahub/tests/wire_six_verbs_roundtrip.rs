//! The COMPOSED-PATH gate for the six verbs
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md` asked this wire to grow: a real
//! `DataFusionHist` seeded with all six kinds, served by `vike_datahub::serve` on an ephemeral
//! loopback port, read back through `vike_datahub_client::RemoteHistStore` — real rows crossing a
//! real socket and coming back equal to what the LOCAL store answers.
//!
//! # Why this file exists
//!
//! Until this change all six methods on `RemoteHistStore` returned `Err(unsupported(..))`, and that
//! refusal was CORRECT for a client with no verb to ask with — an empty `Ok` would have let a
//! caller mistake "the RPC store cannot answer this" for "there is genuinely no data". 0084
//! measured what the refusals cost: four crates opened `DataFusionHist` directly rather than go
//! through the server, BECAUSE the server had no verb to go through. Its verdict is that the store
//! has ONE reader; these verbs are what makes obeying it possible.
//!
//! So the regression this file exists to catch is precise: **a verb silently falling back to a
//! refusal, or to an empty success.** Both would read as "the wire is fine" at every call site that
//! matters, and the second is the worse one — `scan_perp_metrics` shipped in exactly that state
//! once (absent from `remote.rs` entirely, inheriting `HistStore`'s `Ok(Vec::new())` default), and
//! a thin client asking a remote datahub for open interest got a confident EMPTY answer
//! indistinguishable from a store that genuinely holds none.
//!
//! # What each assertion is worth
//!
//! Every verb is asserted TWICE and neither half is redundant:
//!
//! * **wire answer == local answer** — the store's own semantics are the oracle, so this file never
//!   restates what a `kind=` partition means and cannot drift from `crates/vike-data`'s own
//!   round-trip suites;
//! * **the answer is NON-EMPTY** — without it every equality above holds vacuously against a
//!   refusal-turned-empty, which is the exact failure being gated.
//!
//! Two assertions carry more than one verb each. `book_and_depth_are_different_lanes` is why
//! `Response::BookUpdates` and `Response::Depth` are separate variants over one payload type: they
//! read different `kind=` partitions, and a shared reply would make a desync between them decode as
//! plausible data rather than fail. And `a_still_unserved_read_still_refuses` is the SCOPE control —
//! without it this file would pass just as well against a client that had stopped refusing
//! everything, which is a different and much worse change than the one that landed.
//!
//! Behind `serve-datafusion` like `composed_store_roundtrip.rs` — the feature that makes
//! `DataFusionHist` nameable here. A default
//! `cargo test -p vike-datahub` compiles this file to nothing; CI runs it in the hist job's
//! `cargo test -p vike-datahub --features serve-datafusion`.
#![cfg(feature = "serve-datafusion")]

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;

use tempfile::TempDir;
use vike_data::{CohortRow, DataFusionHist, ExecFillRow, HistStore, PerpMetricRow, TsRange};
use vike_datahub::serve;
use vike_datahub_client::{RemoteHistStore, Request, Response, read_frame, write_frame};
use vike_model::{BookLevel, BookUpdate, BookUpdateKind, EquitySample};

const VENUE: &str = "binance";
const SYMBOL: &str = "WIRE6USDT";
/// ⚠ The cohort lane is keyed by ASSET, not by symbol — the one verb in this family whose middle
/// argument differs, and the reason it gets its own constant here instead of reusing [`SYMBOL`].
const ASSET: &str = "WIRE6";

/// A book event at `ts`. `tick_size` and the level values are arbitrary but FIXED, so an equality
/// failure names a transport defect rather than a fixture that moved.
fn book_event(ts: i64, seq: u64, price: f64, qty: f64) -> BookUpdate {
    BookUpdate {
        ts,
        local_ts: ts + 2,
        seq,
        kind: BookUpdateKind::Delta,
        tick_size: 0.01,
        bids: vec![BookLevel { price, qty }],
        asks: vec![BookLevel { price: price + 0.01, qty: qty / 2.0 }],
        symbol: String::new(),
    }
}

/// The LOSSLESS book lane's seed.
fn seeded_book() -> Vec<BookUpdate> {
    vec![book_event(1_000, 1, 100.0, 3.0), book_event(2_000, 2, 100.5, 4.0)]
}

/// The CONFLATING depth lane's seed — deliberately DIFFERENT values from [`seeded_book`], which is
/// what lets `book_and_depth_are_different_lanes` tell a crossed wire from a correct one. Equal
/// fixtures would make that test pass under exactly the bug it exists to find.
fn seeded_depth() -> Vec<BookUpdate> {
    vec![book_event(1_500, 11, 200.0, 7.0), book_event(2_500, 12, 200.5, 8.0)]
}

fn seeded_cohort() -> Vec<CohortRow> {
    vec![
        CohortRow {
            ts: 1_000,
            asset: ASSET.to_string(),
            axis: "size".to_string(),
            cohort: "whale".to_string(),
            grading: "decile".to_string(),
            label_basis: "notional".to_string(),
            long_usd: 1_250_000.0,
            total_usd: 3_000_000.0,
        },
        CohortRow {
            ts: 2_000,
            asset: ASSET.to_string(),
            axis: "size".to_string(),
            cohort: "shrimp".to_string(),
            grading: "decile".to_string(),
            label_basis: "notional".to_string(),
            long_usd: 12_500.0,
            total_usd: 40_000.0,
        },
    ]
}

/// ⚠ One row carries `open_interest: None` on purpose: the column is `Option<f64>`, and a fixture
/// with every field populated cannot tell a correct null round-trip from one that silently becomes
/// `Some(0.0)`.
fn seeded_perp_metrics() -> Vec<PerpMetricRow> {
    vec![
        PerpMetricRow { ts: 1_000, premium: 0.000_125, open_interest: Some(4_200.5) },
        PerpMetricRow { ts: 2_000, premium: -0.000_25, open_interest: None },
    ]
}

fn seeded_equity() -> Vec<EquitySample> {
    vec![
        EquitySample {
            ts: 1_000,
            venue: VENUE.to_string(),
            equity: 10_000.0,
            realized: 0.0,
            unrealized: 0.0,
            missing_prices: 0,
        },
        EquitySample {
            ts: 2_000,
            venue: VENUE.to_string(),
            equity: 10_125.5,
            realized: 100.25,
            unrealized: 25.25,
            missing_prices: 1,
        },
    ]
}

/// ⚠ One row carries `mark_price: None`, for [`seeded_perp_metrics`]'s reason.
fn seeded_exec_fills() -> Vec<ExecFillRow> {
    vec![
        ExecFillRow {
            ts: 1_000,
            trade_id: "t-1".to_string(),
            client_order_id: "coid-1".to_string(),
            venue: VENUE.to_string(),
            symbol: SYMBOL.to_string(),
            side: 1,
            qty: 0.5,
            px: 100.25,
            commission: 0.05,
            mark_price: Some(100.30),
            liquidity_side: "maker".to_string(),
            commission_asset: "USDT".to_string(),
        },
        ExecFillRow {
            ts: 2_000,
            trade_id: "t-2".to_string(),
            client_order_id: "coid-2".to_string(),
            venue: VENUE.to_string(),
            symbol: SYMBOL.to_string(),
            side: -1,
            qty: 0.25,
            px: 101.75,
            commission: 0.02,
            mark_price: None,
            liquidity_side: "taker".to_string(),
            commission_asset: "USDT".to_string(),
        },
    ]
}

/// A real `DataFusionHist` over a temp dir holding all six kinds. The `TempDir` is returned so the
/// caller keeps it alive for the test's duration — dropping it deletes the store under the server.
fn seeded_store() -> (TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().expect("temp store dir");
    let store = DataFusionHist::open(dir.path()).expect("open store");

    // Each append is asserted on its ROW COUNT rather than merely unwrapped: a writer that accepted
    // the call and persisted nothing would otherwise leave every read below vacuously empty, and
    // this file would then be gating the wire against an empty store instead of against the wire.
    let n = store.append_book_updates(VENUE, SYMBOL, &seeded_book(), Some("seed-book")).unwrap();
    assert!(n > 0, "book updates land ({n} rows)");
    let n = store.append_depth(VENUE, SYMBOL, &seeded_depth(), Some("seed-depth")).unwrap();
    assert!(n > 0, "depth updates land ({n} rows)");
    let n = store.append_cohort(VENUE, ASSET, &seeded_cohort(), Some("seed-cohort")).unwrap();
    assert!(n > 0, "cohort rows land ({n} rows)");
    let n = store
        .append_perp_metrics(VENUE, SYMBOL, &seeded_perp_metrics(), Some("seed-perp"))
        .unwrap();
    assert!(n > 0, "perp metric rows land ({n} rows)");
    let n = store.append_equity(VENUE, SYMBOL, &seeded_equity(), Some("seed-equity")).unwrap();
    assert!(n > 0, "equity samples land ({n} rows)");
    let n =
        store.append_exec_fills(VENUE, SYMBOL, &seeded_exec_fills(), Some("seed-fills")).unwrap();
    assert!(n > 0, "exec fills land ({n} rows)");

    (dir, Arc::new(store))
}

/// Field-by-field `BookUpdate` comparison with every f64 compared by `to_bits()`.
///
/// ⚠ A helper rather than `assert_eq!`, because `BookUpdate` derives no `PartialEq` — and the
/// house discipline would want this shape even if it did: `crates/vike-data/tests/
/// hist_datafusion.rs`'s `book_updates_roundtrip_bit_eq` compares the local round trip exactly this
/// way, so a JSON frame must not perturb a single bit that the Parquet round trip preserves.
fn assert_book_bit_eq(expected: &[BookUpdate], got: &[BookUpdate], what: &str) {
    assert_eq!(expected.len(), got.len(), "{what}: event count");
    for (i, (x, y)) in expected.iter().zip(got).enumerate() {
        assert_eq!(x.ts, y.ts, "{what}: ts[{i}]");
        assert_eq!(x.local_ts, y.local_ts, "{what}: local_ts[{i}]");
        assert_eq!(x.seq, y.seq, "{what}: seq[{i}]");
        assert_eq!(x.kind, y.kind, "{what}: kind[{i}]");
        assert_eq!(x.tick_size.to_bits(), y.tick_size.to_bits(), "{what}: tick_size[{i}]");
        for (side, xs, ys) in [("bids", &x.bids, &y.bids), ("asks", &x.asks, &y.asks)] {
            assert_eq!(xs.len(), ys.len(), "{what}: {side} len[{i}]");
            for (j, (a, b)) in xs.iter().zip(ys).enumerate() {
                assert_eq!(a.price.to_bits(), b.price.to_bits(), "{what}: {side}[{i}][{j}].price");
                assert_eq!(a.qty.to_bits(), b.qty.to_bits(), "{what}: {side}[{i}][{j}].qty");
            }
        }
    }
}

/// Bind an ephemeral loopback listener and serve `store` on a detached thread — the sibling suites'
/// convention.
fn spawn_server(store: Arc<dyn HistStore + Send + Sync>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// All six verbs answer over the wire with the SAME rows the local store answers with, and none of
/// the six answers empty.
///
/// ⚠ The non-empty half is what makes the equality half worth anything. Every one of these methods
/// returned `Err` before this change and two of them once returned `Ok(vec![])`; an equality
/// assertion alone would hold against a wire that had quietly gone back to either.
#[test]
fn all_six_verbs_carry_real_rows_over_the_composed_path() {
    let (_dir, store) = seeded_store();
    let addr = spawn_server(store.clone());
    let remote = RemoteHistStore::new(addr.to_string());
    let all = TsRange::all();

    let wire = remote.scan_book_updates(VENUE, SYMBOL, all).expect("scan_book_updates is served");
    assert!(!wire.is_empty(), "the book lane answers non-empty");
    assert_book_bit_eq(&store.scan_book_updates(VENUE, SYMBOL, all).unwrap(), &wire, "book");

    let wire = remote.scan_depth(VENUE, SYMBOL, all).expect("scan_depth is served");
    assert!(!wire.is_empty(), "the depth lane answers non-empty");
    assert_book_bit_eq(&store.scan_depth(VENUE, SYMBOL, all).unwrap(), &wire, "depth");

    let wire = remote.scan_cohort(VENUE, ASSET, all).expect("scan_cohort is served");
    assert!(!wire.is_empty(), "the cohort lane answers non-empty");
    assert_eq!(wire, store.scan_cohort(VENUE, ASSET, all).unwrap(), "cohort: wire == local");

    let wire = remote.scan_perp_metrics(VENUE, SYMBOL, all).expect("scan_perp_metrics is served");
    assert!(!wire.is_empty(), "the perp-metrics lane answers non-empty");
    assert_eq!(wire, store.scan_perp_metrics(VENUE, SYMBOL, all).unwrap(), "perp: wire == local");
    assert!(
        wire.iter().any(|r| r.open_interest.is_none()),
        "the seeded NULL open interest survives the wire as None rather than Some(0.0): {wire:?}"
    );

    let wire = remote.scan_equity(VENUE, SYMBOL, all).expect("scan_equity is served");
    assert!(!wire.is_empty(), "the equity lane answers non-empty");
    assert_eq!(wire, store.scan_equity(VENUE, SYMBOL, all).unwrap(), "equity: wire == local");

    let wire = remote.scan_exec_fills(VENUE, SYMBOL).expect("scan_exec_fills is served");
    assert!(!wire.is_empty(), "the exec-fill lane answers non-empty");
    assert_eq!(wire, store.scan_exec_fills(VENUE, SYMBOL).unwrap(), "fills: wire == local");
    assert!(
        wire.iter().any(|r| r.mark_price.is_none()),
        "the seeded NULL mark price survives the wire as None: {wire:?}"
    );
}

/// The two book-shaped verbs read DIFFERENT lanes, which is the whole reason `Response::BookUpdates`
/// and `Response::Depth` are separate variants over one payload type.
///
/// ⚠ This is the assertion a shared reply variant would defeat SILENTLY. The two lanes carry the
/// same Rust type, so a server arm answering the wrong one would deserialize perfectly at the
/// client and hand a caller a conflated book where it asked for a lossless one — plausible data,
/// no error, and a replay that is wrong in a way nothing downstream can see.
#[test]
fn book_and_depth_are_different_lanes() {
    let (_dir, store) = seeded_store();
    let addr = spawn_server(store);
    let remote = RemoteHistStore::new(addr.to_string());
    let all = TsRange::all();

    let book = remote.scan_book_updates(VENUE, SYMBOL, all).expect("book ok");
    let depth = remote.scan_depth(VENUE, SYMBOL, all).expect("depth ok");

    // The seqs ARE the discriminator: the two fixtures were seeded with disjoint ones precisely so
    // a crossed arm shows up here as a value mismatch rather than as two equal-looking books.
    let book_seqs: Vec<u64> = book.iter().map(|u| u.seq).collect();
    let depth_seqs: Vec<u64> = depth.iter().map(|u| u.seq).collect();
    assert_eq!(book_seqs, vec![1, 2], "the book lane carries its own seeded seqs: {book_seqs:?}");
    assert_eq!(depth_seqs, vec![11, 12], "the depth lane carries its own: {depth_seqs:?}");
}

/// The server ADVERTISES all six capability strings, unconditionally — they are plain `HistStore`
/// trait verbs with no table to mount, so every build that serves at all serves them and the
/// advertisement answers exactly one question: *is this server older than the verb?*
#[test]
fn the_server_advertises_all_six_capabilities() {
    let (_dir, store) = seeded_store();
    let addr = spawn_server(store);
    let client = vike_datahub_client::DatahubClient::connect(addr).expect("handshake on connect");
    let features = client.features();
    for want in [
        vike_datahub_client::FEATURE_SCAN_BOOK_UPDATES,
        vike_datahub_client::FEATURE_SCAN_DEPTH,
        vike_datahub_client::FEATURE_SCAN_COHORT,
        vike_datahub_client::FEATURE_SCAN_PERP_METRICS,
        vike_datahub_client::FEATURE_SCAN_EQUITY,
        vike_datahub_client::FEATURE_SCAN_EXEC_FILLS,
    ] {
        assert!(
            features.iter().any(|f| f == want),
            "every serving build advertises `{want}`: {features:?}"
        );
    }
}

/// ⚠ **THE SCOPE CONTROL, and this file is worth much less without it.** A read that 0084 did NOT
/// add a verb for must STILL refuse over RPC rather than answer an empty success.
///
/// Every assertion above would pass just as well against a `RemoteHistStore` that had stopped
/// refusing ANYTHING — a much larger and much worse change than the one that landed, and one whose
/// symptom is precisely the fabricated "no data" the refusals exist to prevent. `scan_funding` is
/// the witness: same file, same `unsupported` helper, no wire verb, and the seeded store above
/// holds no funding rows either — so an empty `Ok` here would be indistinguishable from the truth
/// at every call site, which is why the refusal rather than the emptiness is what gets asserted.
#[test]
fn a_still_unserved_read_still_refuses() {
    let (_dir, store) = seeded_store();
    let addr = spawn_server(store);
    let remote = RemoteHistStore::new(addr.to_string());

    let err = remote
        .scan_funding(VENUE, SYMBOL, TsRange::all())
        .expect_err("scan_funding has no wire verb and must refuse, not answer empty");
    let msg = err.to_string();
    assert!(
        msg.contains("scan_funding"),
        "the refusal names the method so a log line explains itself: {msg}"
    );
}

// ---- the ROW CAP -------------------------------------------------------------------------------

/// A book lane whose events SHARE timestamps — three `ts` values, two events each.
///
/// ⚠ The sharing is the whole fixture. A cap can only be proven to cut on a whole-`ts` boundary
/// against rows that straddle one; with one event per `ts` every cut is trivially whole and the
/// test would pass against a server that truncated blindly.
fn capped_seed() -> Vec<BookUpdate> {
    vec![
        book_event(1_000, 1, 100.0, 1.0),
        book_event(1_000, 2, 100.1, 2.0),
        book_event(2_000, 3, 200.0, 3.0),
        book_event(2_000, 4, 200.1, 4.0),
        book_event(3_000, 5, 300.0, 5.0),
        book_event(3_000, 6, 300.1, 6.0),
    ]
}

/// Ask the server one raw `ScanBookUpdates` with `limit`, and return the rows.
///
/// Raw frames rather than `RemoteHistStore` deliberately: the client does not SEND a limit yet —
/// paging is the next change — so this drives the wire directly, the way
/// `crates/vike-backtest/tests/compute_plane.rs` drives the plane refusals.
fn scan_capped(addr: SocketAddr, limit: Option<u32>) -> Vec<BookUpdate> {
    let mut s = std::net::TcpStream::connect(addr).expect("connect");
    let req = Request::ScanBookUpdates {
        venue: VENUE.to_string(),
        symbol: "CAPUSDT".to_string(),
        start: None,
        end: None,
        limit,
    };
    write_frame(&mut s, &req).expect("write");
    match read_frame::<_, Response>(&mut s).expect("read") {
        Response::BookUpdates(rows) => rows,
        other => panic!("expected BookUpdates, got {other:?}"),
    }
}

/// ⚠ **The cap cuts ONLY on a whole-`ts` boundary, and all three of its branches are exercised.**
///
/// A paging client continues from `last_ts + 1` — the reply carries no cursor, which is what keeps
/// the cap additive — so a page cut mid-`ts` would strand the rest of that timestamp's rows beyond
/// the client's own next bound and lose them in SILENCE. That is the failure this test exists for,
/// and it is invisible to any assertion that only checks a row COUNT.
#[test]
fn the_row_cap_cuts_only_on_a_whole_ts_boundary() {
    let dir = tempfile::tempdir().expect("temp store dir");
    let store = DataFusionHist::open(dir.path()).expect("open store");
    let n = store.append_book_updates(VENUE, "CAPUSDT", &capped_seed(), Some("cap")).unwrap();
    assert!(n > 0, "the capped seed lands ({n} rows)");
    let addr = spawn_server(Arc::new(store));

    // No cap — every row, the pre-cap behaviour an absent field still means.
    assert_eq!(scan_capped(addr, None).len(), 6, "an absent cap returns everything");

    // The cap falls EXACTLY on a boundary (4 = the end of the ts=2000 group) — take it as asked.
    let four = scan_capped(addr, Some(4));
    assert_eq!(four.len(), 4, "a cap on a boundary is exact");
    assert_eq!(four.last().unwrap().ts, 2_000);

    // A group STRADDLES the cap (3 lands inside ts=2000) — stop SHORT, at the last whole group.
    let three = scan_capped(addr, Some(3));
    assert_eq!(three.len(), 2, "a straddled cap stops short rather than splitting a ts: {three:?}");
    assert!(three.iter().all(|u| u.ts == 1_000), "...and the page is whole groups only");

    // The straddling group is the FIRST one — take it WHOLE, OVER the cap. Returning nothing would
    // be correct by the letter and useless: a paging client would ask the same question forever.
    let one = scan_capped(addr, Some(1));
    assert_eq!(one.len(), 2, "a cap inside the first group yields that group whole: {one:?}");
    assert!(one.iter().all(|u| u.ts == 1_000));

    // A zero cap is not a way to ask for nothing — it is treated as absent.
    assert_eq!(scan_capped(addr, Some(0)).len(), 6, "a zero cap reads as no cap");
}

/// ⚠ **A request with no `limit` is BYTE-IDENTICAL to the frame that predates the field**, which is
/// the whole of what makes the cap additive: an older server must not merely tolerate the new field
/// but never see it. `#[serde(skip_serializing_if = "Option::is_none")]` is what holds that, and it
/// is one attribute away from silently emitting `"limit":null` to every peer on the wire.
#[test]
fn a_request_without_a_limit_is_byte_identical_to_the_old_frame() {
    let req = Request::ScanBookUpdates {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        start: None,
        end: None,
        limit: None,
    };
    let json = serde_json::to_string(&req).expect("serialize");
    assert!(!json.contains("limit"), "an absent cap must not reach the wire at all: {json}");

    let capped = Request::ScanBookUpdates {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        start: None,
        end: None,
        limit: Some(500),
    };
    let json = serde_json::to_string(&capped).expect("serialize");
    assert!(json.contains("\"limit\":500"), "...and a present one does: {json}");
}

/// The server ADVERTISES the cap, and the advertisement is load-bearing: an older server IGNORES an
/// unknown `limit` (serde skips unknown fields) and answers with EVERY row, so a client that
/// assumed the cap without checking would ask for a page and get a frame overrunning
/// `MAX_FRAME_LEN`. Silence means "do not send one".
#[test]
fn the_server_advertises_the_row_cap() {
    let (_dir, store) = seeded_store();
    let addr = spawn_server(store);
    let client = vike_datahub_client::DatahubClient::connect(addr).expect("handshake on connect");
    assert!(
        client.features().iter().any(|f| f == vike_datahub_client::FEATURE_SCAN_LIMIT),
        "every serving build advertises `{}`: {:?}",
        vike_datahub_client::FEATURE_SCAN_LIMIT,
        client.features()
    );
}

/// ⚠ **The client PAGES, and the proof is a row count no single page could carry.**
///
/// `RemoteHistStore::PAGE_ROWS` is 10 000, so a series of 12 000 events cannot come back in one
/// capped answer: a broken loop — one that stopped on the first page, or treated the server's SHORT
/// straddle page as the end — returns 10 000 or fewer and looks entirely healthy. The count is what
/// makes this test able to fail.
///
/// Driven against the REAL page size rather than an injected one. A test-only page override would
/// prove the loop runs and say nothing about the constant every caller actually gets, and the
/// constant is the half that decides whether a frame overruns.
#[test]
fn a_scan_wider_than_one_page_comes_back_whole() {
    const ROWS: i64 = 12_000;

    let dir = tempfile::tempdir().expect("temp store dir");
    let store = DataFusionHist::open(dir.path()).expect("open store");
    // Distinct `ts` per event, so every page boundary is a clean one and this test measures the
    // LOOP rather than the straddle rule (`the_row_cap_cuts_only_on_a_whole_ts_boundary` owns that).
    let seed: Vec<BookUpdate> =
        (0..ROWS).map(|i| book_event(1_000 + i, i as u64, 100.0 + i as f64, 1.0)).collect();
    let n = store.append_book_updates(VENUE, "PAGEDUSDT", &seed, Some("paged")).unwrap();
    assert!(n > 0, "the paged seed lands ({n} rows)");

    let local: Arc<dyn HistStore + Send + Sync> = Arc::new(store);
    let addr = spawn_server(local.clone());
    let remote = RemoteHistStore::new(addr.to_string());

    let wire = remote.scan_book_updates(VENUE, "PAGEDUSDT", TsRange::all()).expect("scan ok");
    assert_eq!(
        wire.len(),
        ROWS as usize,
        "every row crosses the wire — a page-sized answer means the loop stopped early"
    );
    assert_book_bit_eq(
        &local.scan_book_updates(VENUE, "PAGEDUSDT", TsRange::all()).unwrap(),
        &wire,
        "paged",
    );

    // ...and a BOUNDED range still terminates and still carries its whole span.
    let half = TsRange::of(1_000, 1_000 + ROWS / 2 - 1);
    let bounded = remote.scan_book_updates(VENUE, "PAGEDUSDT", half).expect("bounded scan ok");
    assert_eq!(bounded.len(), (ROWS / 2) as usize, "a bounded paged scan is exact");
}
