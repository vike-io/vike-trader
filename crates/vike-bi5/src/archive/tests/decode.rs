//! The daily decoder's tests (§3.2, §3.3): the record, the time base, and the hourly lane's twin (its
//! cross-lane half, which needs the bridge's `point_divisor`, is
//! `crates/bridges/dukascopy/tests/bi5_agrees_with_the_bridge.rs`).

use super::{day, eurusd, lzma, lzma_declared, payload, record};
use crate::archive::decode::{decode_daily_file, decode_daily_records};
use crate::archive::limits::{Bi5Limits, read_bi5_header};
use crate::archive::refusal::Bi5Refusal;
use crate::price_scale::bi5_price_scale;
use crate::tick::decode_ticks;

// ── the daily decoder (§3.2, §3.3) ──────────────────────────────────────────────────────────

#[test]
fn a_price_at_or_above_2_31_decodes_positive() {
    let raw = record(43_200_000, 0x8000_0000, u32::MAX, 1.0, 1.0);
    let ticks = decode_daily_records(&raw, day(2024, 1, 15), eurusd()).expect("one tick");
    assert_eq!(ticks[0].ask, 2_147_483_648.0 / 1e5);
    assert_eq!(ticks[0].bid, 4_294_967_295.0 / 1e5);
    assert!(ticks[0].ask > 0.0 && ticks[0].bid > 0.0);
    // ...where the hourly decoder, reading SIGNED (the design's F3), turns both negative
    let hourly = decode_ticks(&raw, 0, 1e5);
    assert!(hourly[0].ask < 0.0 && hourly[0].bid < 0.0);
}

#[test]
fn a_payload_not_a_multiple_of_20_is_refused() {
    let mut raw = payload(&[43_200_000, 43_200_001]);
    raw.push(0);
    let refusal = Bi5Refusal::PartialRecord { len: 41 };
    assert_eq!(decode_daily_records(&raw, day(2024, 1, 15), eurusd()), Err(refusal.clone()));
    assert_eq!(
        decode_daily_file(&lzma(&raw), day(2024, 1, 15), eurusd(), &Bi5Limits::DEFAULT),
        Err(refusal.clone()),
        "an unknown-size stream is refused once decoded"
    );
    assert_eq!(
        read_bi5_header(&lzma_declared(&raw), &Bi5Limits::DEFAULT),
        Err(refusal),
        "a declared size is refused from the header, before decoding"
    );
    // the whole records alone decode
    raw.pop();
    assert_eq!(decode_daily_records(&raw, day(2024, 1, 15), eurusd()).map(|t| t.len()), Ok(2));
}

#[test]
fn an_offset_of_a_full_day_is_refused_as_out_of_range() {
    let d = day(2024, 1, 15);
    assert_eq!(
        decode_daily_records(&payload(&[43_200_000, 86_400_000]), d, eurusd()),
        Err(Bi5Refusal::OutOfRange { record: 1 })
    );
    assert_eq!(
        decode_daily_records(&payload(&[u32::MAX]), d, eurusd()),
        Err(Bi5Refusal::OutOfRange { record: 0 })
    );
    // the last millisecond of the day is inside it
    let ticks = decode_daily_records(&payload(&[43_200_000, 86_399_999]), d, eurusd())
        .expect("the bound is inside the day");
    assert_eq!(ticks[1].ts, d.start_ms() + 86_399_999);
}

/// The design's kill-proof fixture: two hour-relative payloads filed as one day, the second
/// restarting at 0.
#[test]
fn an_hour_concatenation_is_refused_as_not_monotonic() {
    let concatenated = payload(&[1_000, 1_800_000, 3_500_000, 500, 2_000_000]);
    assert_eq!(
        decode_daily_records(&concatenated, day(2024, 1, 15), eurusd()),
        Err(Bi5Refusal::NotMonotonic { record: 3 })
    );
}

/// A decrease AFTER ticks past the first hour: no other check sees this file, so without the
/// monotonicity check it would decode — with a tick at 00:00:01 stored after one at 10:08.
#[test]
fn a_restart_after_day_relative_ticks_is_refused_as_not_monotonic() {
    let restarted = payload(&[36_000_000, 36_500_000, 1_000, 2_000]);
    assert_eq!(
        decode_daily_records(&restarted, day(2024, 1, 15), eurusd()),
        Err(Bi5Refusal::NotMonotonic { record: 2 })
    );
}

#[test]
fn an_all_first_hour_file_is_refused_as_ambiguous() {
    let d = day(2024, 1, 15);
    assert_eq!(
        decode_daily_records(&payload(&[0, 1_000, 3_599_999]), d, eurusd()),
        Err(Bi5Refusal::AmbiguousTimeBase)
    );
    // one tick at the first hour's end places the file
    assert_eq!(
        decode_daily_records(&payload(&[0, 1_000, 3_600_000]), d, eurusd()).map(|t| t.len()),
        Ok(3)
    );
}

#[test]
fn equal_offsets_are_accepted() {
    let d = day(2024, 1, 15);
    let ticks = decode_daily_records(&payload(&[43_200_000, 43_200_000, 43_200_001]), d, eurusd())
        .expect("several ticks may share a millisecond");
    let ts: Vec<i64> = ticks.iter().map(|t| t.ts - d.start_ms()).collect();
    assert_eq!(ts, [43_200_000, 43_200_000, 43_200_001]);
}

/// An empty payload decodes to NO ticks — never a refusal — which is what tells the store half
/// to spend no key on it.
#[test]
fn an_empty_payload_spends_nothing() {
    let d = day(2024, 1, 15);
    for blob in [Vec::new(), lzma(&[]), lzma_declared(&[])] {
        assert_eq!(
            decode_daily_file(&blob, d, eurusd(), &Bi5Limits::DEFAULT),
            Ok(Vec::new()),
            "{} bytes",
            blob.len()
        );
    }
    let empty_file = read_bi5_header(&[], &Bi5Limits::DEFAULT).expect("an empty payload");
    assert_eq!(empty_file.declared_ticks(), Some(0));
}

#[test]
fn a_good_day_decodes_end_to_end_under_either_header() {
    let d = day(2024, 1, 15);
    let raw: Vec<u8> = [
        record(36_000_229, 109_469, 109_465, 4.05, 3.6),
        record(36_000_400, 109_470, 109_466, 1.0, 2.5),
    ]
    .concat();
    for blob in [lzma(&raw), lzma_declared(&raw)] {
        let ticks = decode_daily_file(&blob, d, eurusd(), &Bi5Limits::DEFAULT).expect("a good day");
        assert_eq!(ticks.len(), 2);
        assert_eq!(ticks[0].ts, d.start_ms() + 36_000_229);
        assert_eq!(ticks[0].ask, 109_469.0 / 1e5);
        assert_eq!(ticks[0].bid, 109_465.0 / 1e5);
        // volumes pass through raw — the record's f32, widened — in millions of base units
        assert_eq!(ticks[0].ask_vol.to_bits(), f64::from(4.05f32).to_bits());
        assert_eq!(ticks[0].bid_vol.to_bits(), f64::from(3.6f32).to_bits());
        assert_eq!(ticks[1].ts, d.start_ms() + 36_000_400);
    }
    assert_eq!(
        read_bi5_header(&lzma_declared(&raw), &Bi5Limits::DEFAULT).map(|h| h.declared_ticks()),
        Ok(Some(2))
    );
    assert_eq!(
        read_bi5_header(&lzma(&raw), &Bi5Limits::DEFAULT).map(|h| h.declared_ticks()),
        Ok(None)
    );
    // a JPY pair's row scales by 1,000
    let usdjpy = bi5_price_scale("USDJPY").expect("USDJPY is admitted");
    let jpy = decode_daily_file(
        &lzma(&record(36_000_000, 148_123, 148_120, 1.0, 1.0)),
        d,
        usdjpy,
        &Bi5Limits::DEFAULT,
    )
    .expect("a JPY day");
    assert_eq!(jpy[0].ask, 148_123.0 / 1e3);
}
