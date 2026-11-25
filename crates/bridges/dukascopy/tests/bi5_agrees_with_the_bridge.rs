//! What only the BRIDGE can check about the `.bi5` format crate. `vike-bi5` (`crates/vike-bi5/src/lib.rs`)
//! cannot see this crate's instrument list (`vike_dukascopy::instrument::grid`), its HTTP lane's point value
//! (`history_point_value`) or its CDN fetch (`fetch_hour`), so the checks that hold the format's price scale and
//! decoders against them live here, beside the one live probe that measured the scale per class.

use vike_bi5::test_support::{lzma, record};
use vike_bi5::{
    Bi5Day, Bi5Limits, Bi5PriceScale, REC_LEN, decode_daily_file, decode_ticks, decompress_bounded,
    tick_to_quote,
};
use vike_dukascopy::DukascopyTicks;
use vike_dukascopy::instrument::grid::history_point_value;
use vike_dukascopy::test_support::listed;
use vike_math::powers::power_of_ten;

/// A calendar day (ONE-based month, as people write it).
fn day(year: i32, month: u32, dom: u32) -> Bi5Day {
    let date = chrono::NaiveDate::from_ymd_opt(year, month, dom).expect("a real date");
    Bi5Day::from_start_ms(date.and_hms_opt(0, 0, 0).expect("midnight").and_utc().timestamp_millis())
        .expect("a midnight")
}

/// **The two lanes scale every listed instrument alike, bit for bit**: the archive lane's scale built from a row's price
/// step (what the datahub's import reads off the kept list) is the HTTP lane's point value for the same row, for every
/// row of the list, so an identical record decodes to an identical price whichever lane read it. The metals and the HUF
/// and CZK crosses included, which the old suffix rule scaled by 100,000 (a hundred and ten times too large: the probe
/// below measured XAU/USD at 1,000).
#[test]
fn the_two_lanes_scale_every_listed_row_alike() {
    let rows = listed();
    for row in &rows {
        let name = row.history_name();
        let archive = Bi5PriceScale::from_tick_size(&name, row.tick_size())
            .unwrap_or_else(|| panic!("{name}: a list row's step is a power of ten"));
        let http = history_point_value(&rows, &name).expect("a listed row");
        assert_eq!(f64::from(archive.point_value()).to_bits(), http.to_bits(), "{name}");
    }
    let at = |name: &str| history_point_value(&rows, name).expect(name);
    assert_eq!(
        (at("EURUSD"), at("USDJPY"), at("XAUUSD"), at("EURHUF"), at("EURCZK")),
        (100_000.0, 1_000.0, 1_000.0, 1_000.0, 10_000.0)
    );
}

/// One record, decoded by BOTH lanes' decoders at the same instant: the same `Tick`, bit for
/// bit, and so the same stored quote. The daily lane is read through `decode_daily_file`, the
/// public door to the record decoder, so the record travels as an LZMA-alone file.
#[test]
fn a_daily_tick_is_bit_identical_to_the_hourly_decoders() {
    let d = day(2024, 1, 15);
    let hour = 10u32;
    let in_hour = 1_500u32;
    let at = |offset: u32| record(offset, 109_469, 109_465, 4.05, 3.6);
    let daily_raw = at(hour * 3_600_000 + in_hour);
    let daily_file = lzma(&daily_raw);
    let eurusd = Bi5PriceScale::from_tick_size("EURUSD", 0.00001).expect("EURUSD's step");
    let daily = decode_daily_file(&daily_file, d, &eurusd, &Bi5Limits::DEFAULT).expect("daily");
    let hourly = decode_ticks(
        &at(in_hour),
        d.start_ms() + i64::from(hour) * vike_model::MS_PER_HOUR,
        history_point_value(&listed(), "EURUSD").expect("listed"),
    );
    assert_eq!(daily.len(), 1);
    let (a, b) = (daily[0], hourly[0]);
    assert_eq!(a.timestamp, b.timestamp);
    assert_eq!(a.ask.to_bits(), b.ask.to_bits());
    assert_eq!(a.bid.to_bits(), b.bid.to_bits());
    assert_eq!(a.ask_vol.to_bits(), b.ask_vol.to_bits());
    assert_eq!(a.bid_vol.to_bits(), b.bid_vol.to_bits());
    assert_eq!(tick_to_quote(&a, "EURUSD"), tick_to_quote(&b, "EURUSD"));
}

// ── the live probe (§3.4) ───────────────────────────────────────────────────────────────────

/// One instrument of each class the kept list carries, by the name the keyless datafeed takes, with the tick scale
/// its list row states (`10^-tick_scale` is its price step; the SDK's tick scale is its pip scale plus one). The pips
/// are the server's as the login of 2026-11-13 stated them (`findings/wave5/SYM-DUKA-CHECK.md`: every predefined
/// instrument on the account kept the SDK's pip except EUR/THB, 0.01 against 0.001; VOL.IDX/USD is 0.1).
const PROBED: &[(&str, u32)] = &[
    ("EURUSD", 5),
    ("USDJPY", 3),
    ("USDTRY", 5),
    ("EURHUF", 3),
    ("EURCZK", 4),
    ("EURTHB", 3),
    ("XAUUSD", 3),
    ("XAGUSD", 3),
    ("BTCUSD", 1),
    ("VOLIDXUSD", 2),
    ("USA500IDXUSD", 3),
    ("DEUIDXEUR", 3),
    ("BRENTCMDUSD", 3),
    ("COPPERCMDUSD", 4),
    ("SAPDEEUR", 3),
];

/// LIVE MEASUREMENT PROBE for the `.bi5` point scale of each CLASS — not a check of the format, and it asserts
/// nothing about any scale. For one instrument of each class ([`PROBED`]) it downloads one liquid hour from the
/// PUBLIC datafeed (no credentials, the bridge's own client and retry schedule, one instrument every few seconds),
/// decodes it through the format's bounded decompression, and prints its first tick's RAW integer points, the UTC
/// minute, the price at `10^tick_scale` (the hypothesis: the point value IS the list row's tick scale) and the price
/// each other candidate would give. A human compares those with an independent quote for the same minute.
/// Run explicitly, never in CI:
/// `cargo test -p vike-dukascopy --test integration bi5_agrees_with_the_bridge::live_probe -- --ignored --nocapture`
#[test]
#[ignore = "live: hits Dukascopy's public datafeed CDN — a MEASUREMENT of the point scale, run manually"]
fn live_probe_prints_raw_points_for_one_instrument_of_each_class() {
    vike_log::test_init();
    // 2024-01-03 (Wed) 14:00 UTC — a liquid London/NY-overlap hour, with the US cash session open for the indices.
    let hour_start: i64 = 1_704_290_400_000;
    let feed = DukascopyTicks::from_urls(&vike_venues::venue_urls::VenueUrls::seed("dukascopy"))
        .expect("the seed's history row");
    for &(name, tick_scale) in PROBED {
        // slowly: one hour file every few seconds, well inside what the history lane asks of the CDN
        std::thread::sleep(std::time::Duration::from_secs(3));
        let blob = match feed.fetch_hour(name, hour_start) {
            Ok(Some(blob)) if !blob.is_empty() => blob,
            Ok(_) => {
                println!("PROBE {name}: no ticks in the probe hour");
                continue;
            }
            Err(e) => {
                println!("PROBE {name}: fetch failed: {e}");
                continue;
            }
        };
        let raw = match decompress_bounded(&blob, &Bi5Limits::DEFAULT) {
            Ok(raw) => raw,
            Err(e) => {
                println!("PROBE {name}: {e}");
                continue;
            }
        };
        let Some(first) = raw.as_chunks::<REC_LEN>().0.first() else {
            println!("PROBE {name}: an empty hour");
            continue;
        };
        let be_u32 =
            |i: usize| u32::from_be_bytes([first[i], first[i + 1], first[i + 2], first[i + 3]]);
        let (offset, ask, bid) = (be_u32(0), be_u32(4), be_u32(8));
        let minute = (hour_start + i64::from(offset)) / 60_000 * 60_000;
        let at = |point_value: f64| (f64::from(ask) / point_value, f64::from(bid) / point_value);
        println!(
            "PROBE {name} at {}: ask {ask} / bid {bid} points, {} ticks in the hour; at 10^{tick_scale} (the list row): \
             {:?}; at 10: {:?}, 100: {:?}, 1,000: {:?}, 10,000: {:?}, 100,000: {:?}",
            chrono::DateTime::from_timestamp_millis(minute)
                .map(|t| t.to_rfc3339())
                .unwrap_or_default(),
            raw.len() / REC_LEN,
            at(power_of_ten(tick_scale as i32).unwrap_or(f64::NAN)),
            at(10.0),
            at(100.0),
            at(1_000.0),
            at(10_000.0),
            at(100_000.0),
        );
    }
}
