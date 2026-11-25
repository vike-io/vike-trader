//! A TIMING PROBE, not a gate (`#[ignore]`d, it asserts only that every read returns the same rows):
//! how long a backtest's data read takes through `HistoryStoreDatahub` against the real store, served by
//! the real datahub on loopback. 32 months of `1m` bars per lane, one part per UTC day as a backfill
//! writes them, three lanes (a price lane and two input lanes, the audit's reference run).
//!
//! It prints, for one lane and for the three: the OLD path (one connection, JSON 10 000-row pages, lanes
//! one after another, which is what one shared handle did), JSON at the big page, the big page as binary
//! COLUMNS (what `HistoryStoreDatahub` reads now), and the three lanes from three threads through ONE
//! handle (the pool). Plus one `series_facts` round trip, a proxy for the manifest read each page pays on
//! the server. Run on a lane:
//! `cargo test -p vike-datahub --features datafusion-serve --release --test datafusion_serve -- --ignored --nocapture bar_load_timing`.
#![cfg(feature = "datafusion-serve")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use vike_data::{HistoryStore, SeriesId, TimestampRange};
use vike_datahub::test_support::spawn_server;
use vike_datahub_client::{DatahubClient, HistoryStoreDatahub};
use vike_marketdata::Bar;
use vike_model::StoreKindId;
use vike_store::HistoryStoreDataFusion;

const VENUE: &str = "dukascopy";
const LANES: [&str; 3] = ["SPX500.CFD", "USA30.CFD", "USATECH.CFD"];
const INTERVAL: &str = "1m";
const DAYS: i64 = 974; // 32 months
const DAY_MS: i64 = 86_400_000;
const FIRST_DAY: i64 = 1_640_995_200_000; // 2022-01-01T00:00:00Z

fn day_bars(day: i64, salt: f64) -> Vec<Bar> {
    (0..1_440)
        .map(|m| {
            let close = 4_000.0 + salt + ((day * 1_440 + m) % 997) as f64 * 0.25;
            Bar {
                timestamp: FIRST_DAY + day * DAY_MS + m * 60_000,
                open: close - 0.5,
                high: close + 1.25,
                low: close - 1.75,
                close,
                volume: (m % 31) as f64,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect()
}

/// A JSON read by hand: one connection, `page`-row pages from `last + 1` to the empty one (10 000 is the
/// old wire behaviour).
fn json_page_read(server: SocketAddr, symbol: &str, page: u32) -> Vec<Bar> {
    let mut client = DatahubClient::connect(server).expect("dial");
    let (mut out, mut start) = (Vec::new(), None);
    loop {
        let page = client
            .load_bars(VENUE, symbol, INTERVAL, TimestampRange { start, end: None }, Some(page))
            .expect("a JSON page");
        let Some(last) = page.last().map(|b| b.timestamp) else { break };
        out.extend(page);
        start = Some(last + 1);
    }
    out
}

fn timed<T>(what: &str, f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let out = f();
    let took = t.elapsed();
    println!("{what:<58} {:>9.3} s", took.as_secs_f64());
    (out, took)
}

#[test]
#[ignore = "a timing probe: run by hand on a lane, --release --nocapture"]
fn bar_load_timing() {
    let dir = tempfile::tempdir().expect("temp store dir");
    let store = HistoryStoreDataFusion::open(dir.path()).expect("open store");
    let seeded = Instant::now();
    for (i, symbol) in LANES.iter().enumerate() {
        for day in 0..DAYS {
            let key = format!("seed-{symbol}-{day}");
            store
                .append_bars(VENUE, symbol, INTERVAL, &day_bars(day, i as f64), Some(&key))
                .unwrap();
        }
    }
    let rows = (DAYS * 1_440) as usize;
    println!(
        "seeded {} lanes x {rows} bars in {:.1} s",
        LANES.len(),
        seeded.elapsed().as_secs_f64()
    );
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    let server = spawn_server(Arc::clone(&store));
    let handle = Arc::new(HistoryStoreDatahub::new(server.to_string()));
    let read = |h: &HistoryStoreDatahub, symbol: &str| {
        h.load_bars(VENUE, symbol, INTERVAL, TimestampRange::all()).expect("a big-page read")
    };
    let _warm = read(&handle, LANES[0]);

    let big_page = HistoryStoreDatahub::BAR_PAGE_ROWS;
    let (small, _) =
        timed("one lane, OLD: JSON, 10 000-row pages", || json_page_read(server, LANES[0], 10_000));
    let (json, _) =
        timed("one lane, JSON, BAR_PAGE_ROWS pages", || json_page_read(server, LANES[0], big_page));
    let (columns, _) =
        timed("one lane, NEW: columns, BAR_PAGE_ROWS pages", || read(&handle, LANES[0]));
    assert_eq!(small.len(), rows);
    assert!(small == json && json == columns, "the same bars at either page size, JSON or columns");
    let thin = HistoryStoreDatahub::new(server.to_string()).with_compressed_bars();
    let (packed_read, _) =
        timed("one lane, columns LZ4 (a thin-link handle)", || read(&thin, LANES[0]));
    assert!(packed_read == columns, "the same bars compressed");
    let page = &columns[..big_page as usize];
    let mut plain = Vec::new();
    vike_datahub_client::proto::encode_bar_columns_into(&mut plain, page);
    let bars = &vike_datahub_client::proto::BAR_COLUMNS;
    let packed = vike_datahub_client::proto::compress_columns(bars, &plain);
    println!(
        "one {big_page}-bar page: JSON {:.1} MB, columns {:.1} MB, LZ4 {:.1} MB",
        serde_json::to_vec(page).unwrap().len() as f64 / 1e6,
        plain.len() as f64 / 1e6,
        packed.len() as f64 / 1e6
    );

    timed("three lanes, OLD: JSON 10 000-row pages, one after another", || {
        for symbol in LANES {
            assert_eq!(json_page_read(server, symbol, 10_000).len(), rows);
        }
    });
    timed("three lanes, NEW columns, one after another, one handle", || {
        for symbol in LANES {
            assert_eq!(read(&handle, symbol).len(), rows);
        }
    });
    timed("three lanes, NEW columns, three threads, one handle", || {
        let readers: Vec<_> = LANES
            .into_iter()
            .map(|symbol| {
                let handle = Arc::clone(&handle);
                thread::spawn(move || read(&handle, symbol).len())
            })
            .collect();
        for reader in readers {
            assert_eq!(reader.join().unwrap(), rows);
        }
    });

    let id = SeriesId::per_symbol(StoreKindId::Bar, VENUE, LANES[0], Some(INTERVAL.to_string()));
    let (_, ten) = timed("10 x series_facts (manifest read + stats + round trip)", || {
        for _ in 0..10 {
            handle.series_facts(&id).expect("series facts");
        }
    });
    println!("one manifest round trip ~ {:.2} ms", ten.as_secs_f64() * 100.0);
}

/// One raw connection that DISCARDS each reply as it reads it (no buffer), so the process' memory during a
/// read is the server's: `RANGE_READS_IN_FLIGHT` and the page sizes are judged by what the DATAHUB holds.
struct Discarding(std::net::TcpStream);

impl Discarding {
    fn open(server: SocketAddr) -> Self {
        use vike_hub_protocol::frame::{read_frame, write_frame};
        use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
        let mut stream = std::net::TcpStream::connect(server).expect("dial");
        let hello = HandshakeFrame::Hello { proto_version: vike_datahub_client::PROTO_VERSION };
        write_frame(&mut stream, &hello).expect("hello");
        let welcome = read_frame::<_, HandshakeReply>(&mut stream);
        assert!(matches!(welcome, Ok(HandshakeReply::Welcome { .. })));
        Self(stream)
    }

    /// Ask one page of `LANES[0]` and read its reply into nothing; the reply's body length.
    fn page(
        &mut self,
        limit: u32,
        columns: Option<vike_datahub_client::proto::BarColumnsBody>,
    ) -> u64 {
        use std::io::Read;
        use vike_datahub_client::proto::Request;
        use vike_hub_protocol::frame::write_frame;
        let request = Request::LoadBars {
            venue: VENUE.to_string(),
            symbol: LANES[0].to_string(),
            interval: INTERVAL.to_string(),
            start: None,
            end: None,
            limit: Some(limit),
            source: None,
            columns,
            resample_to: None,
        };
        write_frame(&mut self.0, &request).expect("the request");
        let mut prefix = [0u8; 4];
        self.0.read_exact(&mut prefix).expect("the prefix");
        let len = u64::from(u32::from_be_bytes(prefix));
        let copied =
            std::io::copy(&mut (&mut self.0).take(len), &mut std::io::sink()).expect("the body");
        assert_eq!(copied, len);
        len
    }
}

/// `VmRSS` or `VmHWM` of this process, in KiB.
#[cfg(target_os = "linux")]
fn status_kib(field: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with(field))?.split_whitespace().nth(1)?.parse().ok()
        })
        .unwrap_or(0)
}

/// The most this process' resident set grew while `f` ran, in MB: the peak reset (`clear_refs` 5) to
/// the current set first.
#[cfg(target_os = "linux")]
fn peak_growth_mb(f: impl FnOnce()) -> f64 {
    std::fs::write("/proc/self/clear_refs", "5").expect("reset the peak resident set");
    let before = status_kib("VmRSS:");
    f();
    (status_kib("VmHWM:").saturating_sub(before)) as f64 / 1024.0
}

/// A MEMORY probe, the datahub's side of one page (`#[ignore]`d, Linux only): the peak resident growth
/// while a raw client reads one page and discards it, for JSON and columns at the page sizes in play,
/// and for eight such reads at once (`RANGE_READS_IN_FLIGHT` admits four). Run it with glibc's mmap
/// threshold FIXED, so every large buffer is mapped and unmapped and the resident set follows what is
/// live (the dynamic threshold keeps freed pages and hides the next page's peak):
/// `env MALLOC_MMAP_THRESHOLD_=65536 cargo test -p vike-datahub --features datafusion-serve --release --test datafusion_serve -- --ignored --nocapture bar_page_memory`.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "a memory probe: run by hand on a lane, --release --nocapture, with MALLOC_MMAP_THRESHOLD_"]
fn bar_page_memory() {
    use vike_datahub_client::proto::BarColumnsBody::{Lz4, Plain};

    let dir = tempfile::tempdir().expect("temp store dir");
    let store = HistoryStoreDataFusion::open(dir.path()).expect("open store");
    for day in 0..DAYS {
        let key = format!("seed-{day}");
        store.append_bars(VENUE, LANES[0], INTERVAL, &day_bars(day, 0.0), Some(&key)).unwrap();
    }
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(store);
    let server = spawn_server(Arc::clone(&store));
    let mut one = Discarding::open(server);
    let _warm = one.page(10_000, None);
    for (what, limit, columns) in [
        ("JSON, 10 000 bars", 10_000, None),
        ("JSON, 200 000 bars", 200_000, None),
        ("columns, 100 000 bars", 100_000, Some(Plain)),
        ("columns, 200 000 bars", 200_000, Some(Plain)),
        ("columns LZ4, 200 000 bars", 200_000, Some(Lz4)),
    ] {
        let mut body = 0;
        let mb = peak_growth_mb(|| body = one.page(limit, columns));
        println!(
            "one page, {what:<28} body {:>7.1} MB   datahub peak +{mb:>6.1} MB",
            body as f64 / 1e6
        );
    }
    for (what, columns) in [("JSON", None), ("columns", Some(Plain))] {
        let mut clients: Vec<_> = (0..8).map(|_| Discarding::open(server)).collect();
        let mb = peak_growth_mb(|| {
            thread::scope(|s| {
                for c in &mut clients {
                    s.spawn(move || c.page(200_000, columns));
                }
            });
        });
        println!(
            "eight 200 000-bar {what} pages at once (RANGE_READS_IN_FLIGHT = {}): datahub peak +{mb:.1} MB",
            vike_datahub::server::RANGE_READS_IN_FLIGHT
        );
    }
}
