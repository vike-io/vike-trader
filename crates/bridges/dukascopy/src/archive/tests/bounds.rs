//! The bounds' tests (§5): the caps, the declared size, bombs, the capped sink and the refusals.

use std::io::Write;

use super::lzma_encoder::{Ending, run};
use super::{day, eurusd, header_then, inflate, limits, lzma, lzma_declared, payload};
use crate::archive::decode::{CappedSink, decode_daily_file};
use crate::archive::limits::{Bi5Header, Bi5Limits, MIB, PROPS_LIMIT, read_bi5_header};
use crate::archive::refusal::Bi5Refusal;
use crate::data::REC_LEN;

// ── the bounds (§5) ─────────────────────────────────────────────────────────────────────────

#[test]
fn the_default_limits_are_the_designs() {
    assert_eq!(Bi5Limits::DEFAULT.max_compressed_bytes, 16 * 1024 * 1024);
    assert_eq!(Bi5Limits::DEFAULT.max_decoded_bytes, 64 * 1024 * 1024);
    assert_eq!(Bi5Limits::DEFAULT.max_decoded_bytes / REC_LEN, 3_355_443, "whole ticks");
}

/// At the DEFAULT: a file of exactly 16 MiB passes the compressed gate (and is then refused by
/// its header, for another reason), one byte more does not.
#[test]
fn the_compressed_cap_admits_its_bound_and_refuses_one_byte_past_it() {
    let cap = Bi5Limits::DEFAULT.max_compressed_bytes;
    let over_declared = 64 * MIB as u64 + 20;
    let mut blob = header_then(over_declared, &[]);
    blob.resize(cap, 0);
    assert_eq!(
        inflate(&blob, Bi5Limits::DEFAULT),
        Err(Bi5Refusal::DeclaredTooLarge { declared: over_declared, cap: 64 * MIB })
    );
    blob.push(0);
    assert_eq!(
        inflate(&blob, Bi5Limits::DEFAULT),
        Err(Bi5Refusal::CompressedTooLarge { len: cap + 1, cap })
    );
    // and a real stream, at a cap of exactly its own length and one byte under it
    let stream = lzma(&payload(&[43_200_000]));
    assert_eq!(inflate(&stream, limits(stream.len(), 1_000)).map(|r| r.len()), Ok(20));
    assert_eq!(
        inflate(&stream, limits(stream.len() - 1, 1_000)),
        Err(Bi5Refusal::CompressedTooLarge { len: stream.len(), cap: stream.len() - 1 })
    );
}

/// The header's size is checked BEFORE decoding: the body behind the over-cap header is not a
/// stream at all, and decoding it would have refused it as something else.
#[test]
fn a_declared_size_over_the_cap_is_refused_before_decoding() {
    let garbage = [0xA5u8; 64];
    assert_eq!(
        inflate(&header_then(1_020, &garbage), limits(MIB, 1_000)),
        Err(Bi5Refusal::DeclaredTooLarge { declared: 1_020, cap: 1_000 })
    );
    // at the bound, a real stream declaring exactly the cap decodes
    let zeros = vec![0u8; 1_000];
    assert_eq!(inflate(&lzma_declared(&zeros), limits(MIB, 1_000)), Ok(zeros));
    // and at the DEFAULT, the header alone: the largest whole-record size under 64 MiB is
    // admitted, the next whole-record size is not
    let largest = 3_355_443 * 20;
    assert_eq!(
        read_bi5_header(&header_then(largest, &[]), &Bi5Limits::DEFAULT),
        Ok(Bi5Header { declared_bytes: Some(largest) })
    );
    assert_eq!(
        read_bi5_header(&header_then(largest + 20, &[]), &Bi5Limits::DEFAULT),
        Err(Bi5Refusal::DeclaredTooLarge { declared: largest + 20, cap: 64 * MIB })
    );
}

/// `lzma-rs`'s encoder writes an 8 MiB dictionary, so under a small cap the DICTIONARY window
/// (`memlimit`) is what refuses the byte past it.
#[test]
fn the_decoded_cap_holds_at_its_bound_through_the_dictionary_window() {
    let cap = 1_000;
    let exact = vec![7u8; cap];
    assert_eq!(inflate(&lzma(&exact), limits(MIB, cap)), Ok(exact));
    assert_eq!(
        inflate(&lzma(&[7u8; 1_001]), limits(MIB, cap)),
        Err(Bi5Refusal::DecodedTooLarge { cap })
    );
}

/// A 4 KiB dictionary flushes to the sink every 4 KiB, so under a larger cap the SINK is what
/// refuses the byte past it.
#[test]
fn the_decoded_cap_holds_at_its_bound_through_the_sink() {
    let cap = 8_192;
    assert_eq!(
        inflate(&run(&[0], 8_192, 4_096, Ending::EndMarker), limits(MIB, cap)).map(|r| r.len()),
        Ok(cap)
    );
    assert_eq!(
        inflate(&run(&[0], 8_193, 4_096, Ending::EndMarker), limits(MIB, cap)),
        Err(Bi5Refusal::DecodedTooLarge { cap })
    );
}

/// At the DEFAULT: an unknown-size stream of exactly 64 MiB is admitted.
#[test]
fn the_default_decoded_cap_admits_exactly_64_mib() {
    let cap = Bi5Limits::DEFAULT.max_decoded_bytes;
    let stream = run(&[0], cap as u64, 4 * MIB as u32, Ending::EndMarker);
    assert_eq!(inflate(&stream, Bi5Limits::DEFAULT).map(|r| r.len()), Ok(cap));
}

/// 70 MiB of zeros in a few KB, its size not declared, under the measured 4 MiB dictionary. The
/// stream is CUT SHORT at its end, so a decoder that read past the cap would reach the cut and
/// say `Truncated` — `DecodedTooLarge` proves it stopped within one window of the cap.
#[test]
fn a_bomb_is_refused_at_the_cap() {
    let bomb = run(&[0], 70 * MIB as u64, 4 * MIB as u32, Ending::CutShort);
    assert!(bomb.len() < 64 * 1024, "a bomb is a few KB, this one is {} bytes", bomb.len());
    assert_eq!(
        inflate(&bomb, Bi5Limits::DEFAULT),
        Err(Bi5Refusal::DecodedTooLarge { cap: Bi5Limits::DEFAULT.max_decoded_bytes })
    );
}

/// The same bomb with a header claiming a 4 GiB dictionary: the window is held to the cap
/// (`memlimit`), so it never grows past it, and the refusal is again `DecodedTooLarge` rather
/// than the cut's `Truncated`.
#[test]
fn a_bomb_claiming_a_4_gib_dictionary_is_refused_at_the_cap() {
    let bomb = run(&[0], 70 * MIB as u64, u32::MAX, Ending::CutShort);
    assert_eq!(
        inflate(&bomb, Bi5Limits::DEFAULT),
        Err(Bi5Refusal::DecodedTooLarge { cap: Bi5Limits::DEFAULT.max_decoded_bytes })
    );
}

#[test]
fn the_capped_sink_never_allocates_past_its_cap() {
    let mut sink = CappedSink::new(100, None);
    sink.write_all(&[1; 60]).expect("under the cap");
    sink.write_all(&[2; 40]).expect("exactly the cap");
    assert!(sink.buf.capacity() <= 100, "capacity {}", sink.buf.capacity());
    assert!(sink.write_all(&[3]).is_err(), "one byte past the cap");
    assert!(sink.refused);
    assert_eq!(sink.buf.len(), 100);
    assert!(sink.buf.capacity() <= 100, "capacity {}", sink.buf.capacity());

    // a write that would cross the cap is refused WHOLE — nothing of it is kept
    let mut sink = CappedSink::new(100, None);
    sink.write_all(&[1; 60]).expect("under the cap");
    assert!(sink.write_all(&[2; 41]).is_err());
    assert_eq!(sink.buf.len(), 60);
    assert!(sink.buf.capacity() <= 100, "capacity {}", sink.buf.capacity());

    // a declared size sizes it up front, never past the cap
    assert_eq!(CappedSink::new(100, Some(80)).buf.capacity(), 80);
    assert!(CappedSink::new(100, Some(u64::MAX)).buf.capacity() <= 100);
}

#[test]
fn a_truncated_stream_is_refused() {
    let raw = payload(&[36_000_000, 36_000_001, 36_000_002]);
    for stream in [lzma(&raw), lzma_declared(&raw)] {
        let cut = &stream[..stream.len() - 6];
        assert_eq!(inflate(cut, Bi5Limits::DEFAULT), Err(Bi5Refusal::Truncated));
        assert_eq!(
            decode_daily_file(cut, day(2024, 1, 15), eurusd(), &Bi5Limits::DEFAULT),
            Err(Bi5Refusal::Truncated)
        );
    }
    // inside the header itself
    let stream = lzma(&raw);
    assert_eq!(inflate(&stream[..10], Bi5Limits::DEFAULT), Err(Bi5Refusal::Truncated));
    assert_eq!(read_bi5_header(&stream[..12], &Bi5Limits::DEFAULT), Err(Bi5Refusal::Truncated));
}

#[test]
fn an_impossible_properties_byte_is_refused_from_the_header() {
    let mut stream = lzma(&payload(&[36_000_000]));
    stream[0] = PROPS_LIMIT;
    let refused = read_bi5_header(&stream, &Bi5Limits::DEFAULT).err();
    assert_eq!(refused.as_ref().map(Bi5Refusal::class), Some("Corrupt"));
    assert_eq!(inflate(&stream, Bi5Limits::DEFAULT).err(), refused);
}

/// The classes are what a report carries; a renamed class is a changed report.
#[test]
fn every_refusal_names_its_class() {
    let all = [
        Bi5Refusal::CompressedTooLarge { len: 2, cap: 1 },
        Bi5Refusal::DeclaredTooLarge { declared: 2, cap: 1 },
        Bi5Refusal::DecodedTooLarge { cap: 1 },
        Bi5Refusal::Truncated,
        Bi5Refusal::Corrupt { reason: "r".to_string() },
        Bi5Refusal::PartialRecord { len: 21 },
        Bi5Refusal::OutOfRange { record: 0 },
        Bi5Refusal::NotMonotonic { record: 3 },
        Bi5Refusal::AmbiguousTimeBase,
    ];
    let classes: Vec<&str> = all.iter().map(Bi5Refusal::class).collect();
    assert_eq!(
        classes,
        [
            "CompressedTooLarge",
            "DeclaredTooLarge",
            "DecodedTooLarge",
            "Truncated",
            "Corrupt",
            "PartialRecord",
            "OutOfRange",
            "NotMonotonic",
            "AmbiguousTimeBase",
        ]
    );
    for refusal in &all {
        assert!(refusal.to_string().starts_with(refusal.class()), "{refusal}");
    }
}
