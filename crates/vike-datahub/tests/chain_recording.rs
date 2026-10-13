//! **The `flags.record_chains` recorder, end to end, offline**: the options lane's REAL poll pass
//! (`vike_datahub::options::deribit::fetch_options`, what the `dh-options` thread runs every
//! `POLL_INTERVAL`) over the provider the lane builds (`DeribitOptions::provider`), carrying the
//! recorder the mount builds (`chain_recorder`, which
//! `crates/vike-datahub/src/datahub_cli/mounts.rs`'s `options_chain_recorder` calls with its
//! arming verdict and the store it serves), writing into a real `DataFusionHist` over a temp dir.
//! The one stand-in is the venue: a loopback HTTP server answers Deribit's
//! `get_book_summary_by_currency` with canned books (`DeribitOptionsProvider::with_book_summary_url`,
//! which exists only under vike-deribit's `test-support`, on through this crate's dev edge), so
//! nothing leaves the box.
//!
//! 1. [`an_armed_recorder_lands_every_polled_chain_in_kind_chain`] — armed: every expiry of every
//!    underlying the pass fetched lands as `kind=chain` rows under `venue=deribit`,
//!    `symbol=<underlying>`, the shared USDC book split per coin, and they survive a reopen;
//! 2. [`an_unarmed_lane_polls_but_records_nothing`] — unarmed: no recorder is built, the same pass
//!    still fetches, and the store holds no chain.
//!
//! What it does NOT cover: `DeribitOptions::start` spawning the thread and the mainnet markprice
//! socket (it would reach Deribit); the arming VERDICT (row × compaction), which
//! `src/datahub_cli_tests.rs`'s `the_chain_recorder_*` tests hold; and the mount's read of the row
//! itself, which `vike_config::CONSUMPTION`'s `flags.record_chains` row pins by its needle.
#![cfg(all(feature = "options-serve", feature = "datafusion-serve"))]

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use vike_data::rec::chain_rec::DEFAULT_CHAIN_CADENCE_MS;
use vike_data::{HistStore, TsRange};
use vike_datahub::options::deribit::{
    DeribitOptions, POLL_INTERVAL, chain_cadence_ms, chain_recorder, fetch_options, poll_interval,
};
use vike_store::DataFusionHist;

/// BTC's own book: two expiries (three quotes on the first), plus a perpetual the parse skips.
const BTC_BOOK: &str = r#"[
  {"instrument_name":"BTC-25JUN37-100000-C","bid_price":0.05,"ask_price":0.06,"mark_price":0.055,"mark_iv":62.5,"open_interest":120.0,"volume":8.0,"underlying_price":104000.0},
  {"instrument_name":"BTC-25JUN37-100000-P","bid_price":0.04,"ask_price":0.05,"mark_price":0.045,"mark_iv":61.0,"open_interest":90.0,"volume":3.0,"underlying_price":104000.0},
  {"instrument_name":"BTC-25JUN37-110000-C","bid_price":0.02,"ask_price":0.03,"mark_price":0.025,"mark_iv":64.0,"open_interest":50.0,"volume":1.0,"underlying_price":104000.0},
  {"instrument_name":"BTC-24DEC37-120000-C","bid_price":0.01,"ask_price":0.02,"mark_price":0.015,"mark_iv":70.0,"open_interest":10.0,"volume":1.0,"underlying_price":104000.0},
  {"instrument_name":"BTC-PERPETUAL","mark_price":104000.0}
]"#;

/// ETH's own book: one expiry, a call and a put.
const ETH_BOOK: &str = r#"[
  {"instrument_name":"ETH-25JUN37-4000-C","bid_price":0.05,"ask_price":0.06,"mark_price":0.055,"mark_iv":65.0,"underlying_price":3800.0},
  {"instrument_name":"ETH-25JUN37-4000-P","bid_price":0.07,"ask_price":0.08,"mark_price":0.075,"mark_iv":66.0,"underlying_price":3800.0}
]"#;

/// The shared USDC book SOL lives in, mixed with other coins that must NOT land under SOL.
const USDC_BOOK: &str = r#"[
  {"instrument_name":"SOL_USDC-25JUN37-200-C","bid_price":10.0,"ask_price":11.0,"mark_price":10.5,"mark_iv":60.0,"underlying_price":190.0},
  {"instrument_name":"SOL_USDC-25JUN37-200-P","bid_price":17.5,"ask_price":18.0,"mark_price":17.75,"mark_iv":61.0,"underlying_price":190.0},
  {"instrument_name":"BTC_USDC-25JUN37-115000-P","bid_price":5000.0,"ask_price":5100.0,"mark_price":5050.0,"mark_iv":55.0,"underlying_price":104000.0},
  {"instrument_name":"XRP_USDC-25JUN37-3-C","bid_price":0.1,"ask_price":0.12,"mark_price":0.11,"mark_iv":70.0,"underlying_price":2.4}
]"#;

/// A loopback stand-in for Deribit's book-summary endpoint: each request is answered with the
/// canned book its `currency=` names, then the connection closes. Returns the URL to hand
/// `DeribitOptionsProvider::with_book_summary_url`.
fn spawn_fake_deribit() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr: SocketAddr = listener.local_addr().expect("resolve assigned port");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().expect("clone the stream"));
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            // Drain the head so the client sees a clean exchange.
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line == "\r\n" || line == "\n" => break,
                    Ok(_) => {}
                }
            }
            let currency = request_line
                .split("currency=")
                .nth(1)
                .and_then(|rest| rest.split(['&', ' ']).next())
                .unwrap_or("");
            let rows = match currency {
                "BTC" => BTC_BOOK,
                "ETH" => ETH_BOOK,
                "USDC" => USDC_BOOK,
                _ => "[]",
            };
            let body = format!(r#"{{"jsonrpc":"2.0","result":{rows}}}"#);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.flush();
        }
    });
    format!("http://{addr}/api/v2/public/get_book_summary_by_currency")
}

/// The instruments recorded for `underlying`.
fn recorded(store: &dyn HistStore, underlying: &str) -> BTreeSet<String> {
    let rows = store.scan_chain("deribit", underlying, TsRange::all()).expect("kind=chain scans");
    rows.into_iter().map(|r| r.instrument).collect()
}

fn names(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn an_armed_recorder_lands_every_polled_chain_in_kind_chain() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = Arc::new(DataFusionHist::open(dir.path()).expect("a store"));

    // The mount's construction when ARMED, over the handle the daemon serves.
    let rec = chain_recorder(
        true,
        Arc::clone(&store) as Arc<dyn HistStore + Send + Sync>,
        chain_cadence_ms(None),
    )
    .expect("the row on builds a recorder");
    assert!(rec.enabled(), "an armed recorder is enabled");
    assert_eq!(rec.cadence_ms(), DEFAULT_CHAIN_CADENCE_MS);

    // The lane's source, and the provider its poll thread runs.
    let source = DeribitOptions::new(Some(rec), poll_interval(None));
    assert!(source.records(), "a source carrying an enabled recorder records");
    let mut provider = source.provider().with_book_summary_url(spawn_fake_deribit());
    assert!(
        provider.chain_recorder().is_some_and(|r| r.enabled()),
        "the recorder reached the provider the poll thread runs"
    );

    // One real pass.
    let fetched = fetch_options(&mut provider).expect("the pass fetched");
    assert_eq!(
        fetched.keys().map(String::as_str).collect::<Vec<_>>(),
        ["BTC", "ETH", "SOL"],
        "every underlying the provider lists was polled"
    );

    let btc = names(&[
        "BTC-25JUN37-100000-C",
        "BTC-25JUN37-100000-P",
        "BTC-25JUN37-110000-C",
        "BTC-24DEC37-120000-C",
    ]);
    let eth = names(&["ETH-25JUN37-4000-C", "ETH-25JUN37-4000-P"]);
    let sol = names(&["SOL_USDC-25JUN37-200-C", "SOL_USDC-25JUN37-200-P"]);
    assert_eq!(recorded(store.as_ref(), "BTC"), btc, "both BTC expiries, every quote");
    assert_eq!(recorded(store.as_ref(), "ETH"), eth);
    assert_eq!(recorded(store.as_ref(), "SOL"), sol, "only SOL's rows of the shared USDC book");

    let btc_rows = store.scan_chain("deribit", "BTC", TsRange::all()).expect("scans");
    assert_eq!(btc_rows.len(), btc.len(), "one row per quote, none twice");
    assert!(btc_rows.iter().all(|r| r.underlying == "BTC" && r.iv.is_some()));

    // Durable, not a cache: a fresh open of the same root reads the same rows.
    drop(provider);
    drop(source);
    drop(store);
    let reopened = DataFusionHist::open(dir.path()).expect("reopen the store");
    assert_eq!(recorded(&reopened, "BTC"), btc, "the BTC snapshot survives a reopen");
    assert_eq!(recorded(&reopened, "SOL"), sol);
}

#[test]
fn an_unarmed_lane_polls_but_records_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let store = Arc::new(DataFusionHist::open(dir.path()).expect("a store"));

    let rec = chain_recorder(
        false,
        Arc::clone(&store) as Arc<dyn HistStore + Send + Sync>,
        chain_cadence_ms(None),
    );
    assert!(rec.is_none(), "unarmed (the row off, or refused) builds no recorder");

    let source = DeribitOptions::new(rec, poll_interval(None));
    assert!(!source.records());
    let mut provider = source.provider().with_book_summary_url(spawn_fake_deribit());
    assert!(provider.chain_recorder().is_none(), "no recorder reaches the provider");

    let fetched = fetch_options(&mut provider).expect("the pass still fetches");
    assert_eq!(fetched.len(), 3, "the lane serves chains whether or not it records");
    for u in ["BTC", "ETH", "SOL"] {
        assert!(recorded(store.as_ref(), u).is_empty(), "{u}: nothing recorded with the row off");
    }
}

/// **No `config.datahub_options_poll_secs` row is the lane as it always was (12 s), and a row is
/// exactly the seconds it says** — carried to the source the mount builds.
#[test]
fn the_poll_row_is_the_interval_and_no_row_is_the_default() {
    assert_eq!(POLL_INTERVAL, Duration::from_secs(12), "the default is twelve seconds");
    assert_eq!(poll_interval(None), POLL_INTERVAL);
    assert_eq!(poll_interval(Some(12)), POLL_INTERVAL, "writing the default is the default");
    assert_eq!(poll_interval(Some(5)), Duration::from_secs(5));
    assert_eq!(poll_interval(Some(300)), Duration::from_secs(300));

    assert_eq!(DeribitOptions::new(None, poll_interval(None)).poll(), POLL_INTERVAL);
    assert_eq!(DeribitOptions::new(None, poll_interval(Some(30))).poll(), Duration::from_secs(30));
}

/// **The `config.datahub_chain_cadence_secs` row is SECONDS and the recorder counts milliseconds**,
/// no row is the minute it always recorded at, and the value reaches the recorder the mount builds.
#[test]
fn the_chain_cadence_row_is_seconds_and_no_row_is_the_default() {
    assert_eq!(DEFAULT_CHAIN_CADENCE_MS, 60_000, "the default is one snapshot a minute");
    assert_eq!(chain_cadence_ms(None), DEFAULT_CHAIN_CADENCE_MS);
    assert_eq!(chain_cadence_ms(Some(60)), DEFAULT_CHAIN_CADENCE_MS, "the default, written");
    assert_eq!(chain_cadence_ms(Some(12)), 12_000);
    assert_eq!(chain_cadence_ms(Some(3_600)), 3_600_000);

    let dir = tempfile::tempdir().expect("a temp dir");
    let store = Arc::new(DataFusionHist::open(dir.path()).expect("a store"));
    let rec = chain_recorder(
        true,
        Arc::clone(&store) as Arc<dyn HistStore + Send + Sync>,
        chain_cadence_ms(Some(120)),
    )
    .expect("armed");
    assert_eq!(rec.cadence_ms(), 120_000, "the row's seconds reach the recorder as milliseconds");
}

/// **`vike-config` restates both rows' ranges, and this holds them to this lane.** That crate is
/// below this one, so the numbers are typed there: the chain cadence's floor is the options lane's
/// own REST poll (a bucket narrower than a poll records every poll), and the shipped default of
/// each row is a value its own range takes, so writing the default back is legal.
#[test]
fn the_options_rows_ranges_hold_their_defaults_and_the_cadence_floor_is_the_poll() {
    use vike_config::config::{DATAHUB_CHAIN_CADENCE_SECS, DATAHUB_OPTIONS_POLL_SECS};

    let poll_secs = u32::try_from(POLL_INTERVAL.as_secs()).expect("seconds fit");
    assert!(DATAHUB_OPTIONS_POLL_SECS.contains(&poll_secs), "the default poll is a legal row");
    let cadence_secs = u32::try_from(DEFAULT_CHAIN_CADENCE_MS / 1_000).expect("seconds fit");
    assert!(
        DATAHUB_CHAIN_CADENCE_SECS.contains(&cadence_secs),
        "the default cadence is a legal row"
    );
    assert_eq!(
        *DATAHUB_CHAIN_CADENCE_SECS.start(),
        poll_secs,
        "vike-config refuses a cadence under the default poll: a narrower bucket records every poll"
    );
}
