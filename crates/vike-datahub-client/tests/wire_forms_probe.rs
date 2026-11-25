//! **What the codec switch saves on the datahub's largest listings** (`FEATURE_WIRE_MSGPACK`): the bytes of each
//! reply in JSON, msgpack and msgpack-in-LZ4, the time to write and to read it, and what the bytes cost on the
//! desktop's tunnel (13-40 KB/s, `HistoryStoreDatahub::with_compressed_bars`' doc). `#[ignore]`d and PRINT-ONLY.
//!
//! `InstrumentLists` is every kept list of a box: a lane run reads the real ones from `<repo>/tmp/probe_instruments/`
//! (copied there from a project's `instruments/`, read-only) and falls back, saying so, to the two lists recorded
//! from the venues. The other three are built at the CI box's sizes as measured on 2026-11-19: 2,230 series, 3,363
//! stamped days (BTCUSDT 1d), 400 gaps.
//!
//! ```sh
//! cargo test -p vike-datahub-client --release --test integration -- --ignored --nocapture wire_forms_probe::
//! ```

use std::time::{Duration, Instant};

use vike_catalog::VenueInstrumentList;
use vike_data::{BarDay, SeriesId};
use vike_datahub_client::catalog::InstrumentListsReply;
use vike_datahub_client::proto::Response;
use vike_hub_protocol::frame::{BodyCodec, decode_body, encode_reply};
use vike_model::StoreKindId;

const FORMS: [BodyCodec; 3] = [BodyCodec::Json, BodyCodec::Msgpack, BodyCodec::MsgpackLz4];

/// The kept lists of `<repo>/tmp/probe_instruments/`, else the recorded ones, and which.
fn kept_lists() -> (Vec<VenueInstrumentList>, &'static str) {
    let read = |dir: &str| -> Vec<VenueInstrumentList> {
        let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
        let mut paths: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        paths.iter().filter_map(|p| serde_json::from_slice(&std::fs::read(p).ok()?).ok()).collect()
    };
    let real = read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tmp/probe_instruments"));
    if !real.is_empty() {
        return (real, "the box's kept lists (tmp/probe_instruments)");
    }
    let recorded =
        read(concat!(env!("CARGO_MANIFEST_DIR"), "/../vike-catalog/tests/fixtures/instruments"));
    (
        recorded,
        "the RECORDED lists only (no tmp/probe_instruments): far smaller than a box's 13.4 MB",
    )
}

/// The median of `runs` timings of `f`.
fn median(runs: usize, mut f: impl FnMut()) -> Duration {
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    times.sort();
    times[runs / 2]
}

/// The datahub's answer in `codec`: the reply form, LZ4 where it shrinks the body.
fn reply_body(codec: BodyCodec, response: &Response) -> Vec<u8> {
    encode_reply(codec, response).expect("encodes").split_off(4)
}

fn report(name: &str, response: &Response) {
    let json_len = reply_body(BodyCodec::Json, response).len();
    println!("{name}:");
    for codec in FORMS {
        let body = reply_body(codec, response);
        let runs = if json_len > 1 << 20 { 5 } else { 25 };
        let write = median(runs, || drop(reply_body(codec, response)));
        let read = median(runs, || drop(decode_body::<Response>(&body).expect("decodes")));
        let tunnel = |bytes_per_second: f64| body.len() as f64 / bytes_per_second;
        println!(
            "  {:<11} {:>11} bytes ({:>5.1}% of JSON)  write {:>9.3} ms  read {:>9.3} ms  tunnel {:>7.1} s at 40 KB/s, {:>7.1} s at 13 KB/s",
            format!("{codec:?}"),
            body.len(),
            100.0 * body.len() as f64 / json_len as f64,
            write.as_secs_f64() * 1e3,
            read.as_secs_f64() * 1e3,
            tunnel(40_000.0),
            tunnel(13_000.0),
        );
    }
}

#[test]
#[ignore = "print-only probe of the codec switch's bytes and time; run it on a lane"]
fn the_largest_listings_in_each_form() {
    let (venues, source) = kept_lists();
    let rows: usize = venues.iter().map(|v| v.instruments.len()).sum();
    println!("InstrumentLists from {source}: {} venues, {rows} rows", venues.len());
    report(
        "InstrumentLists",
        &Response::InstrumentLists(InstrumentListsReply {
            venues,
            operator: Vec::new(),
            splits: Vec::new(),
        }),
    );

    let kinds =
        [StoreKindId::Bar, StoreKindId::Quote, StoreKindId::Trade, StoreKindId::PerpMetrics];
    let intervals = ["1m", "1h", "1d", "funding"];
    let series: Vec<SeriesId> = (0..2_230)
        .map(|i| {
            let kind = kinds[i % kinds.len()];
            let interval =
                (kind == StoreKindId::Bar).then(|| intervals[i % intervals.len()].to_string());
            SeriesId::per_symbol(
                kind,
                ["binance", "okx", "bybit", "dukascopy"][i % 4],
                format!("SYM{i:04}USDT"),
                interval,
            )
        })
        .collect();
    report("ListSeries (2,230 series)", &Response::SeriesList(series));

    let days: Vec<BarDay> = (0..3_363)
        .map(|d| BarDay {
            day: 1_420_070_400_000 + d * 86_400_000,
            stamp: 1_700_000_000_000 + d as u64 * 977,
            provisional: false,
        })
        .collect();
    report("SeriesDays (3,363 days)", &Response::SeriesDays(Some(days)));

    let gaps: Vec<(i64, i64)> = (0..400)
        .map(|i| {
            (1_600_000_000_000 + i * 604_800_000, 1_600_000_000_000 + i * 604_800_000 + 172_800_000)
        })
        .collect();
    report("SeriesGaps (400 gaps)", &Response::SeriesGaps(gaps));
}
