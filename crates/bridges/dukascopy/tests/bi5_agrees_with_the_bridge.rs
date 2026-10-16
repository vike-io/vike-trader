//! What only the BRIDGE can check about the `.bi5` format crate. `vike-bi5` (`crates/vike-bi5/src/lib.rs`)
//! cannot see this crate's bundled instrument catalog (`bundled_instruments`), its HTTP lane's
//! per-pair divisor (`point_divisor`) or its CDN fetch (`fetch_hour`), so the checks that hold the
//! format's price-scale table and decoders against them live here, beside the one live probe that
//! measures a scale row. They were unit tests of the archive's `price_scale.rs` and `archive/tests*`
//! while the format was this crate's own, and assert exactly what they did then.

use vike_bi5::{
    Bi5Day, Bi5Limits, HOUR_MS, REC_LEN, bi5_price_scale, bi5_price_scales, decode_daily_file,
    decode_ticks, decompress_bounded, tick_to_quote,
};
use vike_dukascopy::{bundled_instruments, fetch_hour, point_divisor};

const G10: [&str; 8] = ["USD", "EUR", "GBP", "JPY", "CHF", "CAD", "AUD", "NZD"];

/// A calendar day (ONE-based month, as people write it).
fn day(year: i32, month: u32, dom: u32) -> Bi5Day {
    let date = chrono::NaiveDate::from_ymd_opt(year, month, dom).expect("a real date");
    Bi5Day::from_start_ms(date.and_hms_opt(0, 0, 0).expect("midnight").and_utc().timestamp_millis())
        .expect("a midnight")
}

/// One 20-byte `>IIIff` record.
fn record(offset: u32, ask: u32, bid: u32, ask_vol: f32, bid_vol: f32) -> Vec<u8> {
    let mut r = Vec::with_capacity(REC_LEN);
    r.extend_from_slice(&offset.to_be_bytes());
    r.extend_from_slice(&ask.to_be_bytes());
    r.extend_from_slice(&bid.to_be_bytes());
    r.extend_from_slice(&ask_vol.to_be_bytes());
    r.extend_from_slice(&bid_vol.to_be_bytes());
    r
}

/// The two instruments the design names: a metal the vendor says to verify per instrument, and a
/// HUF cross where 100,000 is doubtful. Both are in `bundled_instruments`, so the HTTP lane
/// scales them today; this lane does not.
#[test]
fn xauusd_and_eurhuf_have_no_row() {
    let bundled: Vec<String> = bundled_instruments().into_iter().map(|i| i.raw_symbol).collect();
    for name in ["XAUUSD", "EURHUF"] {
        assert!(bundled.iter().any(|b| b == name), "{name} is a bundled instrument");
        assert_eq!(bi5_price_scale(name), None, "{name} must wait for a measurement");
    }
}

/// Completeness against the catalog, both ways: every bundled pair whose two legs are G10 has a
/// row, no other bundled instrument has one, and no row names an instrument the catalog lacks.
#[test]
fn every_g10_pair_of_the_catalog_has_a_row_and_no_other_instrument_does() {
    let bundled = bundled_instruments();
    for inst in &bundled {
        let g10 = G10.contains(&inst.base.as_str()) && G10.contains(&inst.quote.as_str());
        assert_eq!(
            bi5_price_scale(&inst.raw_symbol).is_some(),
            g10,
            "{}: a row exists iff both legs are G10 ({}/{})",
            inst.raw_symbol,
            inst.base,
            inst.quote
        );
    }
    for row in bi5_price_scales() {
        assert!(
            bundled.iter().any(|i| i.raw_symbol == row.instrument()),
            "{} has a row but is not a bundled instrument",
            row.instrument()
        );
    }
    let names: std::collections::BTreeSet<&str> =
        bi5_price_scales().iter().map(|r| r.instrument()).collect();
    assert_eq!(names.len(), bi5_price_scales().len(), "no instrument has two rows");
}

/// On every admitted row the archive scale is the HTTP lane's own divisor, bit for bit — so an
/// identical record decodes to an identical price whichever lane read it.
#[test]
fn every_row_agrees_with_the_http_lanes_divisor() {
    for row in bi5_price_scales() {
        assert_eq!(
            f64::from(row.point_value()).to_bits(),
            point_divisor(row.instrument()).to_bits(),
            "{}",
            row.instrument()
        );
    }
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
    let mut daily_file = Vec::new();
    lzma_rs::lzma_compress(&mut &daily_raw[..], &mut daily_file).expect("compress");
    let eurusd = bi5_price_scale("EURUSD").expect("EURUSD is admitted");
    let daily = decode_daily_file(&daily_file, d, eurusd, &Bi5Limits::DEFAULT).expect("daily");
    let hourly = decode_ticks(
        &at(in_hour),
        d.start_ms() + i64::from(hour) * HOUR_MS,
        point_divisor("EURUSD"),
    );
    assert_eq!(daily.len(), 1);
    let (a, b) = (daily[0], hourly[0]);
    assert_eq!(a.ts, b.ts);
    assert_eq!(a.ask.to_bits(), b.ask.to_bits());
    assert_eq!(a.bid.to_bits(), b.bid.to_bits());
    assert_eq!(a.ask_vol.to_bits(), b.ask_vol.to_bits());
    assert_eq!(a.bid_vol.to_bits(), b.bid_vol.to_bits());
    assert_eq!(tick_to_quote(&a, "EURUSD"), tick_to_quote(&b, "EURUSD"));
}

// ── the live probe (§3.4) ───────────────────────────────────────────────────────────────────

/// LIVE MEASUREMENT PROBE for a price-scale row — not a check of the format, and it asserts
/// nothing about any scale. For every instrument of `bundled_instruments` the archive lane does
/// NOT admit (the exotics and the metals), it downloads one liquid hour from the PUBLIC datafeed
/// (no credentials), decodes it through the format's bounded decompression, and prints its
/// first tick's RAW integer points, the UTC minute, and the price each candidate point value
/// would give. A human compares those with an independent quote for the same minute, and the
/// candidate that matches becomes the instrument's row, with that comparison as its evidence.
/// Run explicitly, never in CI:
/// `cargo test -p vike-dukascopy --test bi5_agrees_with_the_bridge live_probe -- --ignored --nocapture`
#[test]
#[ignore = "live: hits Dukascopy's public datafeed CDN — a MEASUREMENT for a scale row, run manually"]
fn live_probe_prints_raw_points_for_every_unscaled_instrument() {
    vike_log::test_init();
    // 2024-01-03 (Wed) 14:00 UTC — a liquid London/NY-overlap hour.
    let hour_start: i64 = 1_704_290_400_000;
    for instrument in bundled_instruments() {
        let name = instrument.raw_symbol;
        if bi5_price_scale(&name).is_some() {
            continue;
        }
        let blob = match fetch_hour(&name, hour_start) {
            Ok(Some(blob)) if !blob.is_empty() => blob,
            Ok(_) => {
                tracing::info!(target: "vike_dukascopy::archive", "{name}: no ticks in the probe hour");
                continue;
            }
            Err(e) => {
                tracing::warn!(target: "vike_dukascopy::archive", "{name}: fetch failed: {e}");
                continue;
            }
        };
        let raw = match decompress_bounded(&blob, &Bi5Limits::DEFAULT) {
            Ok(raw) => raw,
            Err(e) => {
                tracing::warn!(target: "vike_dukascopy::archive", "{name}: {e}");
                continue;
            }
        };
        let Some(first) = raw.chunks_exact(REC_LEN).next() else {
            tracing::info!(target: "vike_dukascopy::archive", "{name}: an empty hour");
            continue;
        };
        let be_u32 =
            |i: usize| u32::from_be_bytes([first[i], first[i + 1], first[i + 2], first[i + 3]]);
        let (offset, ask, bid) = (be_u32(0), be_u32(4), be_u32(8));
        let minute = (hour_start + i64::from(offset)) / 60_000 * 60_000;
        let at = |point_value: f64| (f64::from(ask) / point_value, f64::from(bid) / point_value);
        tracing::info!(
            target: "vike_dukascopy::archive",
            "{name} at {}: ask {ask} / bid {bid} points — at 10: {:?}, 100: {:?}, 1,000: {:?}, \
             10,000: {:?}, 100,000: {:?}",
            chrono::DateTime::from_timestamp_millis(minute).map(|t| t.to_rfc3339()).unwrap_or_default(),
            at(10.0),
            at(100.0),
            at(1_000.0),
            at(10_000.0),
            at(100_000.0),
        );
    }
}
