use super::*;
use tempfile::TempDir;
use vike_model::BookLevel;

// ---- URL builders --------------------------------------------------------------------------

#[test]
fn events_url_single_builds_the_documented_query() {
    let url = events_url_single(DEFAULT_BASE, "TOK", 1_000, 2_000, 500, None);
    assert_eq!(
        url,
        "https://data.vike.io/v1/events?token_id=TOK&from_ms=1000&to_ms=2000&limit=500"
    );
    let with_cursor = events_url_single(DEFAULT_BASE, "TOK", 1_000, 2_000, 500, Some("1000:5"));
    assert_eq!(
        with_cursor,
        "https://data.vike.io/v1/events?token_id=TOK&from_ms=1000&to_ms=2000&limit=500&cursor=1000:5"
    );
}

#[test]
fn events_url_multi_joins_token_ids() {
    let ids = vec!["A".to_string(), "B".to_string(), "C".to_string()];
    let url = events_url_multi(DEFAULT_BASE, &ids, 1, 2, 10, None);
    assert_eq!(url, "https://data.vike.io/v1/events?token_ids=A,B,C&from_ms=1&to_ms=2&limit=10");
}

#[test]
fn cursor_part_count_distinguishes_single_and_multi_token_shapes() {
    assert_eq!(cursor_part_count("1000:5"), 2, "single-token cursor: local_ts:seq");
    assert_eq!(cursor_part_count("1000:5:TOKEN"), 3, "multi-token cursor: local_ts:seq:token_id");
}

// ---- response parse -------------------------------------------------------------------------

#[test]
fn parse_events_response_decodes_the_live_shape() {
    let json = r#"{"events":[{"asks":"[[0.61,56.0]]","best_ask":0,"best_bid":0,"bids":"",
            "event_type":"price_change","is_snapshot":0,"local_ts":1784954000030,"price":0.61,
            "seq":27704,"side":"sell","size":56,"status":"","ts":1784954000011}],
            "has_more":true,"next_cursor":"1784954000030:27704"}"#;
    let resp = parse_events_response(json).unwrap();
    assert_eq!(resp.events.len(), 1);
    assert!(resp.has_more);
    assert_eq!(resp.next_cursor.as_deref(), Some("1784954000030:27704"));
    let row = &resp.events[0];
    assert_eq!(row.event_type, "price_change");
    assert!(!row.is_snapshot());
    assert_eq!(row.side, "sell");
    assert_eq!(row.asks, "[[0.61,56.0]]");
    assert_eq!(row.token_id, None, "single-token responses never echo token_id");
}

#[test]
fn parse_events_response_decodes_a_multi_token_row_when_present() {
    let json = r#"{"events":[{"ts":1,"local_ts":2,"seq":3,"event_type":"trade","side":"buy",
            "price":0.5,"size":1.0,"token_id":"TOKX"}],"has_more":false,"next_cursor":null}"#;
    let resp = parse_events_response(json).unwrap();
    assert_eq!(resp.events[0].token_id.as_deref(), Some("TOKX"));
    assert_eq!(resp.next_cursor, None);
}

#[test]
fn parse_events_response_rejects_garbage() {
    assert!(parse_events_response("not json").is_err());
}

// ---- row -> vike_model mapping ------------------------------------------------------------

fn row(event_type: &str, side: &str, price: f64, size: f64, bids: &str, asks: &str) -> EventRow {
    EventRow {
        ts: 100,
        local_ts: 101,
        seq: 7,
        event_type: event_type.to_string(),
        is_snapshot: if event_type == "book" { 1 } else { 0 },
        side: side.to_string(),
        price,
        size,
        best_bid: 0.0,
        best_ask: 0.0,
        bids: bids.to_string(),
        asks: asks.to_string(),
        status: String::new(),
        token_id: None,
    }
}

#[test]
fn book_row_decodes_full_depth() {
    let r = row("book", "none", 0.0, 0.0, "[[0.5,100.0]]", "[[0.51,80.0]]");
    let u = book_update_from_row(&r, "TOK").unwrap();
    assert_eq!(u.kind, BookUpdateKind::Snapshot);
    assert_eq!(u.bids, vec![BookLevel::new(0.5, 100.0)]);
    assert_eq!(u.asks, vec![BookLevel::new(0.51, 80.0)]);
    assert_eq!(
        u.tick_size, FALLBACK_TICK_SIZE,
        "no tick_size field on this endpoint — defaults to the documented fallback, never 0.0"
    );
    assert_eq!(u.symbol, "TOK");
}

/// Regression for the live-proof-verified root cause (module doc point 3):
/// `vike_sim`'s engine `L2Book::new(tick_size)` treats any NON-POSITIVE tick as "unknown"
/// and falls back to `1.0`, which quantizes a `[0.0, 1.0]`-bounded Polymarket outcome token into
/// two buckets and silently zeroes every fill for a tick-size-sensitive maker (confirmed live: 12
/// `spread_maker`/`gueant_maker` sweep configs traded 0 times over a real 26-market store built
/// with the OLD `0.0` default, while `trailing_scalper` — unaffected by book tick granularity —
/// traded normally over the same store). `book_update_from_row` must NEVER emit `0.0` (or any
/// other non-positive value) here, for ANY row kind, so a future edit cannot silently reintroduce
/// this failure mode.
#[test]
fn tick_size_is_never_the_l2book_degenerate_fallback_value() {
    for event_type in ["book", "price_change"] {
        let r = row(event_type, "buy", 0.4, 1.0, "[[0.4,1.0]]", "");
        let u = book_update_from_row(&r, "TOK").unwrap();
        assert!(u.tick_size > 0.0, "{event_type} row emitted a non-positive tick_size: {u:?}");
    }
}

#[test]
fn delta_row_parses_the_ladder_the_api_already_built_not_price_size() {
    // Verified live shape: a sell-side delta arrives with `asks` already a one-element ladder,
    // `bids` empty — decode must read `bids`/`asks`, NOT reconstruct from price/size/side (that
    // would double-apply the same level under a different code path than the API intends).
    let r = row("price_change", "sell", 0.61, 56.0, "", "[[0.61,56.0]]");
    let u = book_update_from_row(&r, "TOK").unwrap();
    assert_eq!(u.kind, BookUpdateKind::Delta);
    assert!(u.bids.is_empty());
    assert_eq!(u.asks, vec![BookLevel::new(0.61, 56.0)]);
}

#[test]
fn delta_row_zero_size_removal_round_trips() {
    let r = row("price_change", "buy", 0.38, 0.0, "[[0.38,0.0]]", "");
    let u = book_update_from_row(&r, "TOK").unwrap();
    assert_eq!(u.bids, vec![BookLevel::new(0.38, 0.0)]);
}

#[test]
fn status_rows_map_and_unrecognized_labels_are_skipped() {
    let mut r = row("status", "none", 0.0, 0.0, "", "");
    r.status = "gap_start".to_string();
    assert_eq!(book_update_from_row(&r, "TOK").unwrap().kind, BookUpdateKind::GapStart);
    r.status = "stale".to_string();
    assert_eq!(book_update_from_row(&r, "TOK").unwrap().kind, BookUpdateKind::Stale);
    r.status = "live_resume".to_string();
    assert_eq!(book_update_from_row(&r, "TOK").unwrap().kind, BookUpdateKind::LiveResume);
    r.status = "unknown-future-label".to_string();
    assert!(book_update_from_row(&r, "TOK").is_none());
}

#[test]
fn trade_rows_carry_no_book_update() {
    let r = row("trade", "sell", 0.5, 1.0, "", "");
    assert!(book_update_from_row(&r, "TOK").is_none());
}

#[test]
fn trade_from_row_inverts_the_taker_side_convention() {
    let r = row("trade", "sell", 0.95, 3.0, "", "");
    let t = trade_from_row(&r, "TOK");
    assert!(t.is_buyer_maker, "side=sell -> taker sold -> is_buyer_maker=true");
    assert_eq!(t.price, 0.95);
    assert_eq!(t.size, 3.0);
    assert_eq!(t.symbol, "TOK");

    let r2 = row("trade", "buy", 0.10, 1.0, "", "");
    assert!(!trade_from_row(&r2, "TOK").is_buyer_maker);
}

#[test]
fn quote_from_row_is_none_while_best_bid_ask_are_the_unpopulated_zero_sentinel() {
    let r = row("price_change", "buy", 0.4, 1.0, "[[0.4,1.0]]", "");
    assert_eq!(quote_from_row(&r, "TOK"), None, "verified live: always 0.0 today");
}

#[test]
fn quote_from_row_derives_once_either_side_goes_non_zero() {
    let mut r = row("book", "none", 0.0, 0.0, "[]", "[]");
    r.best_bid = 0.44;
    r.best_ask = 0.47;
    let q = quote_from_row(&r, "TOK").unwrap();
    assert_eq!(q.bid, 0.44);
    assert_eq!(q.ask, 0.47);
    assert_eq!(q.bid_size, 0.0, "no size field on this endpoint — documented gap");
    assert_eq!(q.symbol, "TOK");
}

#[test]
fn row_symbol_prefers_the_rows_own_token_id_over_the_fallback() {
    let mut r = row("trade", "buy", 0.5, 1.0, "", "");
    assert_eq!(trade_from_row(&r, "FALLBACK").symbol, "FALLBACK");
    r.token_id = Some("REAL".to_string());
    assert_eq!(trade_from_row(&r, "FALLBACK").symbol, "REAL");
}

// ---- paging ---------------------------------------------------------------------------------

#[test]
fn paginate_walks_every_page_until_has_more_is_false() {
    let pages = [
        r#"{"events":[{"ts":1,"local_ts":1,"seq":1,"event_type":"trade","side":"buy","price":0.5,"size":1.0}],"has_more":true,"next_cursor":"1:1"}"#,
        r#"{"events":[{"ts":2,"local_ts":2,"seq":2,"event_type":"trade","side":"buy","price":0.5,"size":1.0}],"has_more":true,"next_cursor":"2:2"}"#,
        r#"{"events":[{"ts":3,"local_ts":3,"seq":3,"event_type":"trade","side":"buy","price":0.5,"size":1.0}],"has_more":false,"next_cursor":null}"#,
    ];
    let mut seen_cursors: Vec<Option<String>> = Vec::new();
    let mut idx = 0usize;
    let (events, stats) = paginate(|cursor| {
        seen_cursors.push(cursor.map(str::to_string));
        let body = pages[idx].to_string();
        idx += 1;
        Ok((body.clone(), body.len() as u64))
    })
    .unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].ts, 1);
    assert_eq!(events[2].ts, 3);
    assert_eq!(stats.requests, 3);
    assert!(stats.bytes > 0);
    assert_eq!(seen_cursors, vec![None, Some("1:1".to_string()), Some("2:2".to_string())]);
}

#[test]
fn paginate_stops_defensively_when_has_more_but_no_cursor() {
    let mut idx = 0usize;
    let (events, stats) = paginate(|_cursor| {
        idx += 1;
        // Malformed page: claims more data but gives nothing to resume from.
        Ok((r#"{"events":[],"has_more":true,"next_cursor":null}"#.to_string(), 10))
    })
    .unwrap();
    assert!(events.is_empty());
    assert_eq!(stats.requests, 1, "must not loop forever chasing a missing cursor");
    assert_eq!(idx, 1);
}

#[test]
fn paginate_propagates_a_fetch_error() {
    let result = paginate(|_| Err::<(String, u64), _>(CollectError::Fetch("boom".into())));
    assert!(result.is_err());
}

#[test]
fn paginate_propagates_a_decode_error() {
    let result = paginate(|_| Ok(("not json".to_string(), 8)));
    assert!(result.is_err());
}

// ---- KindsMask --------------------------------------------------------------------------------

#[test]
fn kinds_mask_from_kind_covers_the_cli_vocabulary() {
    assert_eq!(KindsMask::from_kind("all"), Some(KindsMask::all()));
    assert_eq!(
        KindsMask::from_kind("book"),
        Some(KindsMask { book: true, trade: false, quote: false })
    );
    assert_eq!(
        KindsMask::from_kind("trade"),
        Some(KindsMask { book: false, trade: true, quote: false })
    );
    assert_eq!(
        KindsMask::from_kind("quote"),
        Some(KindsMask { book: false, trade: false, quote: true })
    );
    assert_eq!(KindsMask::from_kind("garbage"), None);
}

// ---- ingest + idempotency (a real temp DataFusionHist — no network) --------------------------

fn sample_rows() -> Vec<EventRow> {
    vec![
        row("book", "none", 0.0, 0.0, "[[0.5,100.0]]", "[[0.51,80.0]]"),
        row("price_change", "sell", 0.52, 10.0, "", "[[0.52,10.0]]"),
        row("trade", "sell", 0.51, 2.0, "", ""),
    ]
}

#[test]
fn ingest_rows_writes_each_kind_under_the_documented_commit_key_shape() {
    let dir = TempDir::new().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let rows = sample_rows();
    let counts = ingest_rows(&store, "TOK", "2026-07-28", &rows, &KindsMask::all()).unwrap();
    assert_eq!(counts.book, 2, "one snapshot + one delta row");
    assert_eq!(counts.trade, 1);
    assert_eq!(counts.quote, 0, "best_bid/best_ask are the unpopulated 0.0 sentinel");
}

#[test]
fn ingest_rows_is_idempotent_per_symbol_and_day() {
    let dir = TempDir::new().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let rows = sample_rows();
    let first = ingest_rows(&store, "TOK", "2026-07-28", &rows, &KindsMask::all()).unwrap();
    assert_eq!(first.book, 2);
    assert_eq!(first.trade, 1);
    let second = ingest_rows(&store, "TOK", "2026-07-28", &rows, &KindsMask::all()).unwrap();
    assert_eq!(second.book, 0, "same commit key -> 0 rows on the re-run");
    assert_eq!(second.trade, 0, "same commit key -> 0 rows on the re-run");
}

#[test]
fn ingest_rows_scopes_the_commit_key_by_day_so_a_new_day_still_ingests() {
    let dir = TempDir::new().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let rows = sample_rows();
    ingest_rows(&store, "TOK", "2026-07-28", &rows, &KindsMask::all()).unwrap();
    let next_day = ingest_rows(&store, "TOK", "2026-07-29", &rows, &KindsMask::all()).unwrap();
    assert_eq!(next_day.book, 2, "a different day is a different commit key -> not deduped");
}

#[test]
fn ingest_rows_groups_a_multi_token_page_by_its_own_symbol() {
    let dir = TempDir::new().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    let mut r1 = row("trade", "sell", 0.5, 1.0, "", "");
    r1.token_id = Some("TOKA".to_string());
    let mut r2 = row("trade", "sell", 0.6, 2.0, "", "");
    r2.token_id = Some("TOKB".to_string());
    let counts =
        ingest_rows(&store, "FALLBACK", "2026-07-28", &[r1, r2], &KindsMask::all()).unwrap();
    assert_eq!(counts.trade, 2);
    let a = store.scan_trades(VENUE, "TOKA", vike_data::TsRange::all()).unwrap();
    let b = store.scan_trades(VENUE, "TOKB", vike_data::TsRange::all()).unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
}

// ---- live network smokes (never run in CI; manual only) --------------------------------------

/// Proves the real endpoint against a real token, double-gated (network + key) exactly like the
/// venue demo smokes: self-skips without `VIKE_ARCHIVE_API_KEY`. Run manually:
/// `cargo test -p vike-backfill --features vike-archive --lib events_api::tests::live_fetch_one_page -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_fetch_one_page() {
    let Some(key) = std::env::var("VIKE_ARCHIVE_API_KEY").ok() else {
        eprintln!("VIKE_ARCHIVE_API_KEY not set — skipping live events-api smoke");
        return;
    };
    let Some(token) = std::env::var("VIKE_EVENTS_API_SMOKE_TOKEN").ok() else {
        eprintln!("VIKE_EVENTS_API_SMOKE_TOKEN not set — skipping (need a real token_id)");
        return;
    };
    let client = EventsClient::new(DEFAULT_BASE, Some(key));
    let (rows, stats) = client.fetch_all_single(&token, 0, i64::MAX, 500).expect("live page");
    println!("live fetch: {} rows, {} requests, {} bytes", rows.len(), stats.requests, stats.bytes);
}

/// Proves the multi-token detection call against the real server — documents today's verdict
/// (2026-07-28: NOT live, `HTTP 400`) without asserting a fixed outcome, since the parallel PR
/// may ship it later. Run manually:
/// `cargo test -p vike-backfill --features vike-archive --lib events_api::tests::live_probe_multi_token -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_probe_multi_token() {
    let Some(key) = std::env::var("VIKE_ARCHIVE_API_KEY").ok() else {
        eprintln!("VIKE_ARCHIVE_API_KEY not set — skipping live events-api smoke");
        return;
    };
    let client = EventsClient::new(DEFAULT_BASE, Some(key));
    let tokens = vec!["1".to_string(), "2".to_string()];
    let supported = client.probe_multi_token(&tokens, 0, 1);
    println!("multi-token support today: {supported}");
}

// ---- run_pool: the bounded result channel ----------------------------------------------------------

use std::ops::ControlFlow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// The memory bound the bin used to lack: a slow sink must stall the workers, not let finished
/// fetches pile up. Every result is either in a worker's hands (one per worker), in the channel
/// (`concurrency`), or in the sink (one), so the count of produced-but-unconsumed results is at
/// most `2 * concurrency + 1`. An unbounded `mpsc::channel` lets it climb toward the unit count.
#[test]
fn the_pool_never_holds_more_than_two_concurrency_plus_one_results() {
    const CONCURRENCY: usize = 4;
    const UNITS: u32 = 200;
    let produced = AtomicUsize::new(0);
    let consumed = AtomicUsize::new(0);
    let worst = AtomicUsize::new(0);
    let mut sunk = 0usize;
    run_pool(
        (0..UNITS).collect(),
        CONCURRENCY,
        |unit| {
            let made = produced.fetch_add(1, Ordering::SeqCst) + 1;
            worst.fetch_max(made - consumed.load(Ordering::SeqCst), Ordering::SeqCst);
            unit
        },
        |_| {
            std::thread::sleep(Duration::from_millis(5));
            consumed.fetch_add(1, Ordering::SeqCst);
            sunk += 1;
            ControlFlow::Continue(())
        },
    );
    assert_eq!(sunk, UNITS as usize, "every unit's result reaches the sink");
    let worst = worst.load(Ordering::SeqCst);
    assert!(
        worst <= 2 * CONCURRENCY + 1,
        "{worst} results were alive at once; the bound is 2 * {CONCURRENCY} + 1"
    );
}

/// A sink that panics unwinds out of the receive loop; the receiver must be dropped on the way so
/// workers blocked in `send` get an `Err` and exit, or the scope's join waits on them forever.
#[test]
fn a_panicking_sink_does_not_hang_the_workers() {
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(|| {
            run_pool(
                (0..100u32).collect(),
                4,
                |unit| {
                    std::thread::sleep(Duration::from_millis(1));
                    unit
                },
                |_| -> ControlFlow<()> { panic!("the sink blew up") },
            );
        });
        let _ = done_tx.send(outcome.is_err());
    });
    assert_eq!(
        done_rx.recv_timeout(Duration::from_secs(5)),
        Ok(true),
        "run_pool must return (by unwinding) within 5 s of the sink panicking"
    );
}

/// `ControlFlow::Break` stops the hand-out: a fatal ingest error must not keep fetching the other
/// 997 units.
#[test]
fn an_early_break_stops_the_pool() {
    let produced = AtomicUsize::new(0);
    let mut seen = 0usize;
    run_pool(
        (0..1000u32).collect(),
        4,
        |unit| {
            produced.fetch_add(1, Ordering::SeqCst);
            unit
        },
        |_| {
            seen += 1;
            if seen == 3 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        },
    );
    assert_eq!(seen, 3, "the sink is not called again after it breaks");
    let produced = produced.load(Ordering::SeqCst);
    assert!(produced < 50, "{produced} of 1000 units were fetched after a break at result 3");
}
