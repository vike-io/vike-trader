use std::io::{self, Write};

use super::*;

/// How many bytes [`write_columns`] gathers before each write.
const STAGE: usize = 8 * 1024;

/// Bytes gathered into [`STAGE`]-byte writes, so a column streamed eight bytes at a time costs one call per
/// stage, not per word.
struct Staged<'a> {
    out: &'a mut dyn Write,
    buffer: Vec<u8>,
}

impl Staged<'_> {
    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.buffer.len() + bytes.len() > STAGE {
            self.out.write_all(&self.buffer)?;
            self.buffer.clear();
        }
        if bytes.len() > STAGE {
            return self.out.write_all(bytes);
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    fn finish(self) -> io::Result<()> {
        self.out.write_all(&self.buffer)
    }
}

/// The STREAMED writer the datahub used until 2026-11-19 (a column at a time, one pass over the rows per
/// column, in [`STAGE`]-byte writes), kept VERBATIM as a reference: the one-pass encoder that replaced it on the
/// wire must write its bytes, for every kind. Exactly [`columns_len`] bytes.
///
/// # Panics
/// On more than `u32::MAX` rows ([`head`]).
fn write_columns<R: ColumnRow>(
    out: &mut dyn Write,
    magic: &ColumnsMagic,
    rows: &[R],
) -> io::Result<()> {
    const { assert!(R::WORDS <= MAX_COLUMN_WORDS && R::TEXTS <= MAX_TEXT_COLUMNS) };
    let mut staged = Staged { out, buffer: Vec::with_capacity(STAGE) };
    staged.put(&head(magic, rows.len()))?;
    for c in 0..R::WORDS {
        for row in rows {
            staged.put(&row.words()[c].to_le_bytes())?;
        }
    }
    for row in rows {
        staged.put(&[row.flags()])?;
    }
    if R::LEVEL_COUNTS.is_some() {
        for price in [true, false] {
            for row in rows {
                for l in row.levels().into_iter().flatten() {
                    let value = if price { l.price } else { l.qty };
                    staged.put(&value.to_bits().to_le_bytes())?;
                }
            }
        }
    }
    let mut runs = SymbolRuns::new();
    for row in rows {
        add_to_runs(&mut runs, 1, row.symbol());
    }
    let mut tail = Vec::new();
    append_runs(&mut tail, &runs);
    for c in 0..R::TEXTS {
        let mut runs = SymbolRuns::new();
        for row in rows {
            add_to_runs(&mut runs, 1, row.texts()[c]);
        }
        append_runs(&mut tail, &runs);
    }
    staged.put(&tail)?;
    staged.finish()
}

/// A bar-columns body in memory.
fn encode_bar_columns(rows: &[Bar]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bar_columns_into(&mut out, rows);
    out
}

/// A bar-columns body, plain or compressed, back as its bars.
fn decode_bar_columns(body: &[u8]) -> Result<Vec<Bar>, String> {
    let mut out = Vec::new();
    decode_bar_columns_into(body, &mut out).map(|_| out)
}

/// The streamed writer's body of `rows`, as `magic`'s reply.
fn streamed<R: ColumnRow>(magic: &ColumnsMagic, rows: &[R]) -> Vec<u8> {
    let mut out = Vec::new();
    write_columns(&mut out, magic, rows).expect("a Vec takes every write");
    out
}

/// A bar-columns body LZ4-compressed.
fn compress_bar_columns(body: &[u8]) -> Vec<u8> {
    compress_columns(&BAR_COLUMNS, body)
}

fn bar(i: i64, symbol: Option<&str>) -> Bar {
    let close = 4_000.0 + i as f64 * 0.1 + 1e-9;
    Bar {
        timestamp: 1_700_000_000_000 + i * 60_000,
        open: close - 0.3,
        high: close + 0.7,
        low: -0.0,
        close,
        volume: f64::MIN_POSITIVE * i as f64,
        funding: (i % 3 == 0).then_some(1e-4 * i as f64),
        bid: (i % 2 == 0).then_some(close - 0.05),
        ask: (i % 5 == 0).then_some(f64::MAX),
        symbol: symbol.map(Into::into),
    }
}

fn rows() -> Vec<Bar> {
    let mut rows: Vec<Bar> = (0..40).map(|i| bar(i, Some("SPX500.CFD"))).collect();
    rows.extend((40..45).map(|i| bar(i, None)));
    rows.extend((45..50).map(|i| bar(i, Some("Ünïcode/ß"))));
    rows.push(bar(50, Some("")));
    rows
}

/// One bar as its bits: the timestamp, the five required fields, the three optional ones, the symbol.
type BarBits = (i64, [u64; 5], [Option<u64>; 3], Option<String>);

/// Every field's bits, so `-0.0` against `0.0` (equal as floats) is a difference here.
fn bits(rows: &[Bar]) -> Vec<BarBits> {
    rows.iter()
        .map(|b| {
            (
                b.timestamp,
                [b.open, b.high, b.low, b.close, b.volume].map(f64::to_bits),
                [b.funding, b.bid, b.ask].map(|v| v.map(f64::to_bits)),
                b.symbol.map(String::from),
            )
        })
        .collect()
}

#[test]
fn the_columns_carry_every_field_bit_for_bit_and_the_length_is_counted_exactly() {
    for rows in [Vec::new(), vec![bar(7, None)], rows()] {
        let body = encode_bar_columns(&rows);
        let counted = COLUMNS_EMPTY_LEN
            + rows
                .iter()
                .enumerate()
                .map(|(i, r)| columns_row_len(i.checked_sub(1).map(|p| &rows[p]), r))
                .sum::<usize>();
        assert_eq!(body.len(), counted, "the fit's count is the body's length");
        assert_eq!(bits(&decode_bar_columns(&body).unwrap()), bits(&rows));
    }
}

/// The contract: the rows a JSON reply carries. JSON writes a non-finite float as `null`, which reads
/// back as `None` for an optional field, so the columns say absent there too.
#[test]
fn the_columns_answer_the_rows_the_json_reply_answers() {
    let mut rows = rows();
    rows[3].funding = Some(f64::NAN);
    rows[4].bid = Some(f64::INFINITY);
    rows[5].ask = Some(f64::NEG_INFINITY);
    let json: Vec<Bar> = serde_json::from_slice(&serde_json::to_vec(&rows).unwrap()).unwrap();
    let columns = decode_bar_columns(&encode_bar_columns(&rows)).unwrap();
    assert_eq!(bits(&columns), bits(&json));
    assert_eq!((columns[3].funding, columns[4].bid, columns[5].ask), (None, None, None));
}

#[test]
fn a_required_field_keeps_its_bits_even_where_json_has_no_answer() {
    let mut row = bar(1, None);
    row.close = f64::NAN;
    let back = decode_bar_columns(&encode_bar_columns(&[row.clone()])).unwrap();
    assert_eq!(back[0].close.to_bits(), row.close.to_bits());
}

#[test]
fn a_malformed_body_is_refused_never_read_short() {
    let body = encode_bar_columns(&rows());
    let refused = |bytes: &[u8], why: &str| {
        let e = decode_bar_columns(bytes).expect_err(why);
        assert!(e.contains("malformed bar-columns reply"), "{why}: {e}");
    };
    refused(&[], "empty");
    refused(b"{\"Error\":\"x\"}", "a JSON body");
    for cut in [3, 7, 8, 100, body.len() - 1] {
        refused(&body[..cut], "truncated");
    }
    let mut longer = body.clone();
    longer.push(0);
    refused(&longer, "a trailing byte");
    let mut fewer_rows = body.clone();
    fewer_rows[4] -= 1;
    refused(&fewer_rows, "a row count that disagrees with the columns");
    let presence = HEAD + 9 * 8 * rows().len();
    let mut stray_bit = body.clone();
    stray_bit[presence] |= 0b1000;
    refused(&stray_bit, "a flag bit no field has");
    let one = encode_bar_columns(&[bar(1, Some("AB"))]);
    let mut not_utf8 = one.clone();
    let last = not_utf8.len() - 1;
    not_utf8[last] = 0xFF;
    refused(&not_utf8, "a symbol that is not UTF-8");
    let mut short_run = one.clone();
    short_run[HEAD + row_bytes::<Bar>() + RUNS] = 0; // the one run's count: 0 of the 1 row
    refused(&short_run, "runs that do not cover the rows");
}

/// The streamed writer is the encoder, byte for byte, and its counted length is exact; an LZ4 body carries
/// the same bars; a compressed body that is garbled, cut, mis-sized or over the frame is refused, and its
/// stated length is checked before anything is allocated.
#[test]
fn the_streamed_and_the_compressed_bodies_carry_the_same_bars_and_a_garbled_one_is_refused() {
    let rows = rows();
    let plain = encode_bar_columns(&rows);
    assert_eq!(streamed(&BAR_COLUMNS, &rows), plain, "the streamed body is the encoded one");
    assert_eq!(columns_len(&rows), plain.len());

    let packed = compress_bar_columns(&plain);
    assert!(packed.starts_with(&BAR_COLUMNS_LZ4_MAGIC));
    assert_eq!(bits(&decode_bar_columns(&packed).unwrap()), bits(&rows));

    let refused = |bytes: &[u8], why: &str| {
        let e = decode_bar_columns(bytes).expect_err(why);
        assert!(e.contains("malformed bar-columns reply"), "{why}: {e}");
    };
    let mut over = BAR_COLUMNS_LZ4_MAGIC.to_vec();
    over.extend_from_slice(&(MAX_FRAME_LEN + 1).to_le_bytes());
    refused(&over, "a stated length over the frame");
    refused(&packed[..6], "cut inside the head");
    refused(&packed[..packed.len() - 2], "a cut block");
    let mut longer = packed.clone();
    longer[4] = longer[4].wrapping_add(1);
    refused(&longer, "a stated length the block does not fill");
    refused(&compress_bar_columns(&plain[..plain.len() - 1]), "a cut body inside a good block");
    refused(&compress_bar_columns(b"{\"Bars\":[]}"), "a block that holds no bar columns");
}

/// The encoder as it was first written (one `write_all` per value), kept VERBATIM as the reference the bulk
/// encoders must match byte for byte: the wire format is this function's output.
fn reference_body(rows: &[Bar]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let n = u32::try_from(rows.len()).expect("a reply carries fewer than u32::MAX bars");
    out.write_all(&BAR_COLUMNS_MAGIC).unwrap();
    out.write_all(&n.to_le_bytes()).unwrap();
    let finite = |v: Option<f64>| v.filter(|v| v.is_finite());
    let words: [&dyn Fn(&Bar) -> u64; 9] = [
        &|b: &Bar| b.timestamp as u64,
        &|b: &Bar| b.open.to_bits(),
        &|b: &Bar| b.high.to_bits(),
        &|b: &Bar| b.low.to_bits(),
        &|b: &Bar| b.close.to_bits(),
        &|b: &Bar| b.volume.to_bits(),
        &|b: &Bar| finite(b.funding).map_or(0, f64::to_bits),
        &|b: &Bar| finite(b.bid).map_or(0, f64::to_bits),
        &|b: &Bar| finite(b.ask).map_or(0, f64::to_bits),
    ];
    for word in words {
        for b in rows {
            out.write_all(&word(b).to_le_bytes()).unwrap();
        }
    }
    for b in rows {
        let present = u8::from(finite(b.funding).is_some())
            | (u8::from(finite(b.bid).is_some()) << 1)
            | (u8::from(finite(b.ask).is_some()) << 2);
        out.write_all(&[present]).unwrap();
    }
    let mut runs: Vec<(u32, Option<&str>)> = Vec::new();
    for b in rows {
        match runs.last_mut() {
            Some((count, symbol)) if *symbol == b.symbol.as_deref() => *count += 1,
            _ => runs.push((1, b.symbol.as_deref())),
        }
    }
    out.write_all(&(runs.len() as u32).to_le_bytes()).unwrap();
    for (count, symbol) in runs {
        out.write_all(&count.to_le_bytes()).unwrap();
        let len = symbol.map_or(0, |s| s.len() as u32 + 1);
        out.write_all(&len.to_le_bytes()).unwrap();
        out.write_all(symbol.map_or(&[][..], str::as_bytes)).unwrap();
    }
    out
}

/// The wire bytes are the reference encoder's, through `write_columns`, `encode_bar_columns_into`, an append
/// after bytes already held and a splice of bodies, at sizes around the relay's 16 384-row parts; and one body
/// spelled out field by field pins the table.
#[test]
fn the_bulk_encoders_write_the_reference_bytes() {
    let edges = [0usize, 1, 2, 16_383, 16_384, 16_385, 20_000];
    for n in edges {
        let rows: Vec<Bar> = (0..n as i64)
            .map(|i| bar(i, [Some("SPX500.CFD"), None, Some("Ü")][(i as usize / 997) % 3]))
            .collect();
        let reference = reference_body(&rows);
        assert_eq!(encode_bar_columns(&rows), reference, "{n} rows, in memory");
        assert_eq!(streamed(&BAR_COLUMNS, &rows), reference, "{n} rows, streamed");
        let mut appended = vec![7u8, 7];
        encode_bar_columns_into(&mut appended, &rows);
        assert_eq!(appended[2..], reference[..], "{n} rows, appended after what was there");
        assert_eq!(appended[..2], [7, 7]);
        // The same rows kept as bodies of runs (empty ones and cuts inside a symbol run among them), spliced whole,
        // are the same body.
        let cuts = [0, n / 3, n / 3, (n / 2 + 1).min(n), n];
        let bodies: Vec<(Vec<u8>, usize)> = cuts
            .windows(2)
            .map(|cut| (encode_bar_columns(&rows[cut[0]..cut[1]]), cut[1] - cut[0]))
            .collect();
        let runs: Vec<(&[u8], std::ops::Range<usize>)> =
            bodies.iter().map(|(body, len)| (body.as_slice(), 0..*len)).collect();
        let mut spliced = vec![7u8];
        splice_columns_into::<Bar>(&mut spliced, &BAR_COLUMNS, &runs).expect("a splice");
        assert_eq!(spliced[1..], reference[..], "{n} rows in {} bodies", runs.len());
    }

    let one = Bar {
        timestamp: 1,
        open: 1.0,
        high: 2.0,
        low: 0.5,
        close: 1.5,
        volume: 3.0,
        funding: None,
        bid: Some(1.25),
        ask: Some(f64::NAN),
        symbol: Some("AB".into()),
    };
    let mut golden = b"\0BAR".to_vec();
    golden.extend_from_slice(&1u32.to_le_bytes());
    for word in [1u64, 0x3FF0 << 48, 0x4000 << 48, 0x3FE0 << 48, 0x3FF8 << 48, 0x4008 << 48] {
        golden.extend_from_slice(&word.to_le_bytes());
    }
    golden.extend_from_slice(&0u64.to_le_bytes()); // funding: absent
    golden.extend_from_slice(&(0x3FF4u64 << 48).to_le_bytes()); // bid 1.25
    golden.extend_from_slice(&0u64.to_le_bytes()); // ask: NaN, so absent as JSON reads it
    golden.push(0b010);
    for word in [1u32, 1, 3] {
        golden.extend_from_slice(&word.to_le_bytes()); // one run, of one row, "AB" (len + 1)
    }
    golden.extend_from_slice(b"AB");
    assert_eq!(encode_bar_columns(std::slice::from_ref(&one)), golden);
}

/// A decode APPENDS to what the caller holds, and a body that does not decode appends nothing.
#[test]
fn a_decode_appends_and_a_refused_one_appends_nothing() {
    let rows = rows();
    let mut out = vec![bar(-1, None)];
    let body = encode_bar_columns(&rows);
    assert_eq!(decode_bar_columns_into(&body, &mut out).unwrap(), rows.len());
    assert_eq!(bits(&out[1..]), bits(&rows));
    assert_eq!(bits(&out[..1]), bits(&[bar(-1, None)]));
    let packed = compress_bar_columns(&body);
    assert_eq!(decode_bar_columns_into(&packed, &mut out).unwrap(), rows.len());
    assert_eq!(bits(&out[1 + rows.len()..]), bits(&rows));
    let held = out.len();
    let mut cut = body.clone();
    cut.pop();
    assert!(decode_bar_columns_into(&cut, &mut out).is_err());
    let mut stray = body.clone();
    stray[HEAD + 9 * 8 * rows.len()] |= 0b1000;
    assert!(decode_bar_columns_into(&stray, &mut out).is_err());
    assert_eq!(out.len(), held, "a refused body appends nothing");
}

/// The decoder as it was first written (a bounds-checked slice per word, a fresh `Vec` per body), for the
/// probe's "before".
fn reference_decode(body: &[u8]) -> Vec<Bar> {
    let n = u32::from_le_bytes(body[4..8].try_into().unwrap()) as usize;
    let columns = &body[8..8 + row_bytes::<Bar>() * n];
    let word = |column: usize, i: usize| {
        let start = (column * n + i) * 8;
        u64::from_le_bytes(columns[start..start + 8].try_into().expect("eight bytes"))
    };
    let present = &columns[9 * 8 * n..];
    let optional = |column: usize, i: usize, bit: u8| {
        (present[i] & bit != 0).then(|| f64::from_bits(word(column, i)))
    };
    (0..n)
        .map(|i| Bar {
            timestamp: word(0, i) as i64,
            open: f64::from_bits(word(1, i)),
            high: f64::from_bits(word(2, i)),
            low: f64::from_bits(word(3, i)),
            close: f64::from_bits(word(4, i)),
            volume: f64::from_bits(word(5, i)),
            funding: optional(6, i, 1),
            bid: optional(7, i, 2),
            ask: optional(8, i, 4),
            symbol: None,
        })
        .collect()
}

/// The backtest sandbox's msgpack columns as `origin/main` had them (`BarColumns` in
/// `crates/vike-backtesthub/src/strategy_sandbox/wire.rs`), the probe's reference: each field one msgpack
/// `bin` of little-endian words, the symbol as runs.
mod msgpack_columns {
    use std::borrow::Cow;

    use serde::{Deserialize, Serialize};
    use vike_marketdata::Bar;

    #[derive(Debug, Default)]
    struct Bin<'a>(Cow<'a, [u8]>);

    impl Bin<'_> {
        fn put(&mut self, bits: u64) {
            self.0.to_mut().extend_from_slice(&bits.to_le_bytes());
        }

        fn word(&self, i: usize) -> u64 {
            let mut word = [0u8; 8];
            word.copy_from_slice(&self.0[i * 8..i * 8 + 8]);
            u64::from_le_bytes(word)
        }
    }

    impl Serialize for Bin<'_> {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_bytes(&self.0)
        }
    }

    impl<'de: 'a, 'a> Deserialize<'de> for Bin<'a> {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct Bytes;
            impl<'de> serde::de::Visitor<'de> for Bytes {
                type Value = Bin<'de>;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("a column of bytes")
                }
                fn visit_borrowed_bytes<E: serde::de::Error>(
                    self,
                    bytes: &'de [u8],
                ) -> Result<Bin<'de>, E> {
                    Ok(Bin(Cow::Borrowed(bytes)))
                }
                fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Bin<'de>, E> {
                    Ok(Bin(Cow::Owned(bytes.to_vec())))
                }
            }
            d.deserialize_bytes(Bytes)
        }
    }

    #[derive(Debug, Default, Serialize, Deserialize)]
    pub(super) struct BarColumns<'a> {
        rows: usize,
        #[serde(borrow)]
        timestamp: Bin<'a>,
        #[serde(borrow)]
        open: Bin<'a>,
        #[serde(borrow)]
        high: Bin<'a>,
        #[serde(borrow)]
        low: Bin<'a>,
        #[serde(borrow)]
        close: Bin<'a>,
        #[serde(borrow)]
        volume: Bin<'a>,
        #[serde(borrow)]
        funding: Bin<'a>,
        #[serde(borrow)]
        bid: Bin<'a>,
        #[serde(borrow)]
        ask: Bin<'a>,
        #[serde(borrow)]
        present: Bin<'a>,
        symbol: Vec<(usize, Option<String>)>,
    }

    impl BarColumns<'_> {
        pub(super) fn fill(&mut self, rows: &[Bar]) {
            let c = self;
            c.rows = rows.len();
            for column in [
                &mut c.timestamp,
                &mut c.open,
                &mut c.high,
                &mut c.low,
                &mut c.close,
                &mut c.volume,
                &mut c.funding,
                &mut c.bid,
                &mut c.ask,
                &mut c.present,
            ] {
                column.0.to_mut().clear();
            }
            c.symbol.clear();
            for b in rows {
                c.timestamp.put(b.timestamp as u64);
                c.open.put(b.open.to_bits());
                c.high.put(b.high.to_bits());
                c.low.put(b.low.to_bits());
                c.close.put(b.close.to_bits());
                c.volume.put(b.volume.to_bits());
                c.funding.put(b.funding.map_or(0, f64::to_bits));
                c.bid.put(b.bid.map_or(0, f64::to_bits));
                c.ask.put(b.ask.map_or(0, f64::to_bits));
                let flags = u8::from(b.funding.is_some())
                    | (u8::from(b.bid.is_some()) << 1)
                    | (u8::from(b.ask.is_some()) << 2);
                c.present.0.to_mut().push(flags);
                match c.symbol.last_mut() {
                    Some((count, symbol)) if symbol.as_deref() == b.symbol.as_deref() => {
                        *count += 1
                    }
                    _ => c.symbol.push((1, b.symbol.map(String::from))),
                }
            }
        }

        pub(super) fn push_into(self, out: &mut Vec<Bar>) {
            let n = self.rows;
            let optional = |column: &Bin<'_>, i: usize, bit: u8| {
                ((self.present.0[i] & bit) != 0).then(|| f64::from_bits(column.word(i)))
            };
            out.reserve(n);
            let mut symbols =
                self.symbol.iter().flat_map(|(count, s)| std::iter::repeat_n(s, *count));
            for i in 0..n {
                out.push(Bar {
                    timestamp: self.timestamp.word(i) as i64,
                    open: f64::from_bits(self.open.word(i)),
                    high: f64::from_bits(self.high.word(i)),
                    low: f64::from_bits(self.low.word(i)),
                    close: f64::from_bits(self.close.word(i)),
                    volume: f64::from_bits(self.volume.word(i)),
                    funding: optional(&self.funding, i, 1),
                    bid: optional(&self.bid, i, 2),
                    ask: optional(&self.ask, i, 4),
                    symbol: symbols.next().cloned().flatten().map(Into::into),
                });
            }
        }
    }
}

/// A TIMING PROBE, not a gate (`#[ignore]`d; it asserts only that every codec answers the same bars): encode
/// and decode of 1.4 M one-minute bars, in the sandbox relay's parts of 16 384 and in the datahub's pages of
/// 200 000, for the old msgpack columns (the reference), this codec as first written, and this codec now.
/// Every buffer is kept across rounds where the codec's API allows it (so a round times the codec, not the
/// page faults of a fresh 170 MB `Vec`); medians of nine rounds, each codec in turn. Run on a lane:
/// `cargo test -p vike-datahub-client --release --lib -- --ignored --nocapture bar_columns_codec_timing`.
#[test]
#[ignore = "a timing probe: run by hand on a lane, --release --nocapture"]
fn bar_columns_codec_timing() {
    use std::time::Instant;
    const BARS: i64 = 974 * 1_440;
    let bars: Vec<Bar> = (0..BARS)
        .map(|i| {
            let close = 4_000.0 + (i % 997) as f64 * 0.25;
            Bar {
                timestamp: 1_640_995_200_000 + i * 60_000,
                open: close - 0.5,
                high: close + 1.25,
                low: close - 1.75,
                close,
                volume: (i % 31) as f64,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect();
    let median = |mut runs: Vec<f64>| {
        runs.sort_by(f64::total_cmp);
        runs[runs.len() / 2]
    };
    let ms = |f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        t.elapsed().as_secs_f64() * 1e3
    };
    for part in [16_384usize, 200_000] {
        let parts: Vec<&[Bar]> = bars.chunks(part).collect();
        let mut times: [(Vec<f64>, Vec<f64>); 4] = Default::default();
        let mut bodies: Vec<Vec<u8>> = vec![Vec::new(); parts.len()];
        let mut out: Vec<Bar> = Vec::with_capacity(bars.len());
        let mut frame = Vec::new();
        let mut columns = msgpack_columns::BarColumns::default();
        for _ in 0..9 {
            // msgpack columns: fill the reused part, serialize into its reused body, decode each and append.
            times[0].0.push(ms(&mut || {
                for (p, body) in parts.iter().zip(&mut bodies) {
                    columns.fill(p);
                    body.clear();
                    rmp_serde::encode::write_named(body, &columns).unwrap();
                }
            }));
            out.clear();
            times[0].1.push(ms(&mut || {
                for body in &bodies {
                    let part: msgpack_columns::BarColumns<'_> =
                        rmp_serde::from_slice(body).unwrap();
                    part.push_into(&mut out);
                }
            }));
            assert_eq!(out.len(), bars.len());
            // This codec as first written: one write per value; a fresh Vec per body, then appended.
            times[1].0.push(ms(&mut || {
                for (p, body) in parts.iter().zip(&mut bodies) {
                    *body = reference_body(p);
                }
            }));
            out.clear();
            times[1].1.push(ms(&mut || {
                for body in &bodies {
                    out.extend(reference_decode(body));
                }
            }));
            assert_eq!(out.len(), bars.len());
            // Now: `write_columns` (the datahub's streamed reply) into one reused frame a part...
            times[2].0.push(ms(&mut || {
                for p in &parts {
                    frame.clear();
                    write_columns(&mut frame, &BAR_COLUMNS, p).unwrap();
                }
            }));
            // ...and `encode_bar_columns_into` each reused body, decoded straight into one Vec.
            times[3].0.push(ms(&mut || {
                for (p, body) in parts.iter().zip(&mut bodies) {
                    body.clear();
                    encode_bar_columns_into(body, p);
                }
            }));
            out.clear();
            times[3].1.push(ms(&mut || {
                for body in &bodies {
                    decode_bar_columns_into(body, &mut out).unwrap();
                }
            }));
            assert_eq!(bits(&out), bits(&bars));
        }
        let [msgpack, first, written, now] = times;
        println!("parts of {part}:");
        println!(
            "  msgpack columns (reference)       encode {:7.1} ms   decode {:7.1} ms",
            median(msgpack.0),
            median(msgpack.1)
        );
        println!(
            "  bar columns as first written      encode {:7.1} ms   decode {:7.1} ms",
            median(first.0),
            median(first.1)
        );
        println!("  bar columns now, write_columns     encode {:7.1} ms", median(written.0));
        println!(
            "  bar columns now, encode/decode _into  {:7.1} ms   decode {:7.1} ms",
            median(now.0),
            median(now.1)
        );
    }
}

// ---- the tick-level kinds: the same codec through each row's table -----------------------------------

fn quotes() -> Vec<TickQuote> {
    let quote = |i: i64, symbol: &str| TickQuote {
        timestamp: 1_700_000_000_000 + i * 37,
        local_timestamp: if i % 3 == 0 { 0 } else { 1_700_000_000_004 + i * 37 },
        bid: 64_000.0 + i as f64 * 0.1 + 1e-9,
        ask: 64_000.5 + i as f64 * 0.1,
        bid_size: if i % 4 == 0 { 0.0 } else { -0.0 },
        ask_size: f64::MIN_POSITIVE * i as f64,
        symbol: symbol.into(),
    };
    let mut rows: Vec<TickQuote> = (0..30).map(|i| quote(i, "BTCUSDT.P")).collect();
    rows.extend((30..33).map(|i| quote(i, "")));
    rows.extend((33..36).map(|i| quote(i, "Ünïcode/ß")));
    rows.push(quote(36, "BTCUSDT.P"));
    rows
}

fn trades() -> Vec<TickTrade> {
    let trade = |i: i64, symbol: &str| TickTrade {
        timestamp: 1_700_000_000_000 + (i / 3) * 5,
        local_timestamp: if i % 2 == 0 { 0 } else { 1_700_000_000_010 + i },
        price: 64_000.0 + (i % 17) as f64 * 0.5,
        size: 0.001 * i as f64,
        is_buyer_maker: i % 3 != 1,
        symbol: symbol.into(),
    };
    let mut rows: Vec<TickTrade> = (0..40).map(|i| trade(i, "ETHUSDT")).collect();
    rows.extend((40..44).map(|i| trade(i, "")));
    rows
}

/// Every kind of event, with no level, one side only, both sides and a snapshot's many levels.
fn books() -> Vec<L2BookUpdate> {
    let event = |i: usize, kind: L2BookUpdateKind, bids: usize, asks: usize, symbol: &str| {
        let level =
            |k: usize| L2BookLevel::new(100.0 + k as f64 * 0.5 + i as f64, 1.0 / (k + 1) as f64);
        L2BookUpdate {
            timestamp: 1_700_000_000_000 + (i / 2) as i64 * 100,
            local_timestamp: if i.is_multiple_of(2) { 0 } else { 1_700_000_000_005 + i as i64 },
            sequence: u64::MAX - i as u64,
            kind,
            tick_size: 0.01,
            bids: (0..bids).map(level).collect(),
            asks: (0..asks).map(|k| level(k + 1_000)).collect(),
            symbol: symbol.to_string(),
        }
    };
    let mut rows = vec![event(0, L2BookUpdateKind::Snapshot, 400, 399, "BTC-PERP")];
    for (i, kind) in BOOK_KINDS.into_iter().enumerate() {
        rows.push(event(i + 1, kind, i % 3, (i + 1) % 2, "BTC-PERP"));
    }
    rows.push(event(9, L2BookUpdateKind::GapStart, 0, 0, ""));
    rows.push(event(10, L2BookUpdateKind::Delta, 1, 0, "Ü"));
    rows
}

fn perp_metrics() -> Vec<PerpMetricRow> {
    (0..20)
        .map(|i| PerpMetricRow {
            ts: 1_700_000_000_000 + i * 3_600_000,
            premium: -1.25e-5 * i as f64,
            open_interest: (i % 3 != 0).then_some(1e9 + i as f64),
        })
        .collect()
}

/// A body in memory, by the one-pass encoder.
fn encoded<R: ColumnRow>(magic: &ColumnsMagic, rows: &[R]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_columns_into(&mut out, magic, rows);
    out
}

fn decoded<R: ColumnRow>(magic: &ColumnsMagic, body: &[u8]) -> Result<Vec<R>, String> {
    let mut out = Vec::new();
    decode_columns_into(body, magic, &mut out).map(|_| out)
}

/// Every check a kind's rows must pass, for none, one and all of `rows`: the counted length is the body's; the
/// streamed body is the encoded one; the rows decode back to the same bytes, plain and LZ4 (the compressed
/// body unpacks to the plain one); and a decode appends to what the caller holds.
fn holds_every_way<R: ColumnRow>(magic: &ColumnsMagic, rows: &[R]) {
    for rows in [&rows[..0], &rows[..1], rows] {
        let plain = encoded(magic, rows);
        let what = format!("{} x{}", magic.name, rows.len());
        assert!(plain.starts_with(&magic.plain), "{what}");
        assert_eq!(columns_len(rows), plain.len(), "{what}: the counted length is the body's");
        assert_eq!(streamed(magic, rows), plain, "{what}: the streamed body is the encoded one");
        let back: Vec<R> = decoded(magic, &plain).expect("decodes");
        assert_eq!(encoded(magic, &back), plain, "{what}: every field crossed bit for bit");
        let packed = compress_columns(magic, &plain);
        assert!(packed.starts_with(&magic.lz4), "{what}");
        let unpacked = lz4_flex::block::decompress(&packed[8..], plain.len()).expect("unpacks");
        assert_eq!(unpacked, plain, "{what}: LZ4 is an identity");
        let back: Vec<R> = decoded(magic, &packed).expect("decodes compressed");
        assert_eq!(encoded(magic, &back), plain, "{what}: the compressed body, the same rows");
        let mut held: Vec<R> = decoded(magic, &plain).expect("decodes");
        assert_eq!(decode_columns_into(&plain, magic, &mut held).unwrap(), rows.len());
        assert_eq!(held.len(), 2 * rows.len(), "{what}: a decode appends");
        assert_eq!(check_columns::<R>(&plain, magic), Ok(rows.len()), "{what}: a keeper's check");
        let kept = plain_columns(packed, magic).expect("a keeper's plain body");
        assert_eq!(kept, plain, "{what}: the plain body of the compressed one");
    }
}

#[test]
fn every_kind_crosses_bit_for_bit_streamed_or_packed_and_its_length_is_counted_exactly() {
    holds_every_way(&QUOTE_COLUMNS, &quotes());
    holds_every_way(&TRADE_COLUMNS, &trades());
    holds_every_way(&BOOK_UPDATE_COLUMNS, &books());
    holds_every_way(&DEPTH_COLUMNS, &books());
    holds_every_way(&PERP_METRIC_COLUMNS, &perp_metrics());
}

/// The contract: the rows a JSON reply carries. `rows` and their JSON round trip encode to the same body.
fn answers_what_json_answers<R>(magic: &ColumnsMagic, rows: &[R])
where
    R: ColumnRow + Serialize + serde::de::DeserializeOwned,
{
    let json: Vec<R> = serde_json::from_slice(&serde_json::to_vec(rows).unwrap()).unwrap();
    assert_eq!(encoded(magic, &json), encoded(magic, rows), "{}", magic.name);
}

#[test]
fn every_kind_answers_the_rows_its_json_reply_answers() {
    answers_what_json_answers(&QUOTE_COLUMNS, &quotes());
    answers_what_json_answers(&TRADE_COLUMNS, &trades());
    answers_what_json_answers(&BOOK_UPDATE_COLUMNS, &books());
    // A non-finite optional reads `None` through JSON's `null`, and so here.
    let mut perp = perp_metrics();
    perp[1].open_interest = Some(f64::NAN);
    perp[2].open_interest = Some(f64::INFINITY);
    answers_what_json_answers(&PERP_METRIC_COLUMNS, &perp);
    let back: Vec<PerpMetricRow> =
        decoded(&PERP_METRIC_COLUMNS, &encoded(&PERP_METRIC_COLUMNS, &perp)).unwrap();
    let interest = (back[1].open_interest, back[2].open_interest, back[3].open_interest);
    assert_eq!(interest, (None, None, None));
    assert_eq!(back[4].open_interest, Some(1e9 + 4.0));
    // The flags and the levels read back as they were, not by default.
    let back: Vec<TickTrade> =
        decoded(&TRADE_COLUMNS, &encoded(&TRADE_COLUMNS, &trades())).unwrap();
    let maker = |t: &[TickTrade]| t.iter().map(|t| t.is_buyer_maker).collect::<Vec<_>>();
    assert_eq!(maker(&back), maker(&trades()));
    let book = encoded(&BOOK_UPDATE_COLUMNS, &books());
    let back: Vec<L2BookUpdate> = decoded(&BOOK_UPDATE_COLUMNS, &book).unwrap();
    let shape = |b: &[L2BookUpdate]| {
        b.iter().map(|e| (e.kind, e.bids.len(), e.asks.len(), e.symbol.clone())).collect::<Vec<_>>()
    };
    assert_eq!(shape(&back), shape(&books()));
}

#[test]
fn a_required_tick_float_keeps_its_bits_where_json_has_no_answer() {
    let mut rows = quotes();
    rows[2].bid = f64::NAN;
    rows[3].ask = f64::NEG_INFINITY;
    let back: Vec<TickQuote> = decoded(&QUOTE_COLUMNS, &encoded(&QUOTE_COLUMNS, &rows)).unwrap();
    assert_eq!(back[2].bid.to_bits(), rows[2].bid.to_bits());
    assert_eq!(back[3].ask.to_bits(), rows[3].ask.to_bits());
}

/// Each reply's magic is its own: a body is refused under another reply's, so a crossed answer is an error and
/// never plausible rows (the book and the depth lane carry one row type).
#[test]
fn a_reply_is_refused_under_another_replys_magic() {
    let magics = [
        BAR_COLUMNS,
        QUOTE_COLUMNS,
        TRADE_COLUMNS,
        BOOK_UPDATE_COLUMNS,
        DEPTH_COLUMNS,
        PERP_METRIC_COLUMNS,
    ];
    let mut every: Vec<[u8; 4]> = magics.iter().flat_map(|m| [m.plain, m.lz4]).collect();
    assert!(every.iter().all(|m| m[0] == 0), "a NUL first, which no JSON body has");
    every.sort_unstable();
    every.dedup();
    assert_eq!(every.len(), 2 * magics.len(), "every magic is distinct");
    let book = encoded(&BOOK_UPDATE_COLUMNS, &books());
    let e = decoded::<L2BookUpdate>(&DEPTH_COLUMNS, &book).expect_err("book rows are not depth's");
    assert!(e.contains("malformed depth-columns reply"), "{e}");
    let packed = compress_columns(&BOOK_UPDATE_COLUMNS, &book);
    assert!(decoded::<L2BookUpdate>(&DEPTH_COLUMNS, &packed).is_err(), "compressed too");
    assert!(decoded::<TickTrade>(&TRADE_COLUMNS, &encoded(&QUOTE_COLUMNS, &quotes())).is_err());
    assert!(decoded::<TickQuote>(&QUOTE_COLUMNS, &encode_bar_columns(&rows())).is_err());
}

/// `body` is refused as `magic`'s rows of `R`, in the codec's words, and a keeper's check of it ([`check_columns`])
/// refuses it in the same words with no row built.
fn refused<R: ColumnRow>(magic: &ColumnsMagic, body: &[u8], why: &str) {
    let Err(e) = decoded::<R>(magic, body) else { panic!("{why}: it decoded") };
    assert!(e.contains(&format!("malformed {}-columns reply", magic.name)), "{why}: {e}");
    assert_eq!(check_columns::<R>(body, magic), Err(e), "{why}: the keeper's check");
}

#[test]
fn a_malformed_tick_body_is_refused_and_appends_nothing() {
    // A book event's kind byte past the five kinds, on the LAST row: the rows before it are taken back.
    let rows = books();
    let book = encoded(&BOOK_UPDATE_COLUMNS, &rows);
    let mut kind = book.clone();
    kind[HEAD + row_bytes::<L2BookUpdate>() * rows.len() - 1] = 5;
    refused::<L2BookUpdate>(&BOOK_UPDATE_COLUMNS, &kind, "a book kind no event has");
    let mut more_levels = book.clone();
    more_levels[HEAD + 8 * 4 * rows.len()] += 1; // the first event's bid count, one past its levels
    refused::<L2BookUpdate>(&BOOK_UPDATE_COLUMNS, &more_levels, "a count the levels do not fill");
    refused::<L2BookUpdate>(&BOOK_UPDATE_COLUMNS, &book[..book.len() - 30], "cut");
    let mut held = vec![rows[0].clone()];
    for body in [&kind, &more_levels] {
        assert!(decode_columns_into(body, &BOOK_UPDATE_COLUMNS, &mut held).is_err());
    }
    assert_eq!(held.len(), 1, "a refused body appends nothing");
    // A quote run with no symbol (the empty symbol's length `1` set to `0`): a quote always carries one.
    let one = [TickQuote { symbol: Default::default(), ..quotes()[0].clone() }];
    let mut no_symbol = encoded(&QUOTE_COLUMNS, &one);
    let len_at = no_symbol.len() - 4;
    no_symbol[len_at] = 0;
    refused::<TickQuote>(&QUOTE_COLUMNS, &no_symbol, "a quote without a symbol");
    // A perp-metric run that names a symbol: the kind carries none.
    let perp = encoded(&PERP_METRIC_COLUMNS, &perp_metrics()[..2]);
    let mut named = perp[..perp.len() - 4].to_vec();
    named.extend_from_slice(&2u32.to_le_bytes());
    named.push(b'X');
    refused::<PerpMetricRow>(&PERP_METRIC_COLUMNS, &named, "a perp metric with a symbol");
    let mut stray = encoded(&TRADE_COLUMNS, &trades());
    stray[HEAD + 8 * 4 * trades().len()] |= 0b10;
    refused::<TickTrade>(&TRADE_COLUMNS, &stray, "a flag bit a trade has no use for");
}

// ---- the row-shaped scans: the same codec, their strings as text columns ----------------------------

fn equity() -> Vec<EquitySample> {
    (0..12)
        .map(|i| EquitySample {
            timestamp: 1_700_000_000_000 + i * 60_000,
            venue: if i < 8 { "binance" } else { "TOTAL" }.to_string(),
            equity: 10_000.0 + i as f64 * 0.25 + 1e-9,
            realized: -0.0,
            unrealized: i as f64 * -1.5,
            missing_prices: (i % 3) as u32,
        })
        .collect()
}

fn forecasts() -> Vec<ExecForecastRow> {
    (0..10)
        .map(|i| ExecForecastRow {
            ts: 1_700_000_000_000 + i * 1_000,
            mount_id: if i % 4 == 3 { "mount-b" } else { "mount-a" }.to_string(),
            value: 0.1 * i as f64 - 0.3,
            horizon_ms: 3_600_000,
            confidence: (i % 3 != 0).then_some(i as f64 / 10.0),
        })
        .collect()
}

fn cohorts() -> Vec<CohortRow> {
    (0..9)
        .map(|i| CohortRow {
            ts: 1_700_000_000_000 + (i / 3) * 3_600_000,
            asset: "BTC".to_string(),
            axis: ["size", "pnl", "tier"][i as usize % 3].to_string(),
            cohort: format!("{i}xWhale"),
            grading: if i % 2 == 0 { "graded" } else { "" }.to_string(),
            label_basis: "Ünïcode".to_string(),
            long_usd: 1e6 * i as f64,
            total_usd: 2e6 * i as f64 + 0.5,
        })
        .collect()
}

fn texts() -> Vec<TextRow> {
    (0..6)
        .map(|i| TextRow {
            received_ms: 1_700_000_000_000 + i,
            published_ms: if i == 2 { 0 } else { 1_699_999_000_000 + i },
            kind: if i < 4 { "news" } else { "economic_calendar" }.to_string(),
            source: "CoinDesk".to_string(),
            source_id: format!("id-{i}"),
            subject: String::new(),
            period: "2026Q3".to_string(),
            title: format!("Title {i}"),
            summary: "a summary".repeat(i as usize),
            body: String::new(),
            url: format!("https://example.invalid/{i}"),
            market: "crypto".to_string(),
            tags: "a\u{1f}b".to_string(),
            currency: if i >= 4 { "USD" } else { "" }.to_string(),
            impact: "High".to_string(),
            forecast: String::new(),
            previous: "1.5%".to_string(),
            actual: String::new(),
            amends: if i == 5 { "id-4" } else { "" }.to_string(),
        })
        .collect()
}

fn chain() -> Vec<ChainRow> {
    (0..14)
        .map(|i| {
            let some = |k: i64| ((i + k) % 3 != 0).then_some(0.5 * (i + k) as f64);
            ChainRow {
                ts: 1_700_000_000_000 + (i / 7) * 60_000,
                underlying: "BTC".to_string(),
                instrument: format!(
                    "BTC-27JUN26-{}-{}",
                    100_000 + i,
                    if i % 2 == 0 { "C" } else { "P" }
                ),
                expiry_ms: 1_782_547_200_000,
                strike: 100_000.0 + i as f64,
                kind: if i % 2 == 0 { OptionKind::Call } else { OptionKind::Put },
                bid: some(0),
                ask: some(1),
                mark: some(2),
                iv: some(3),
                open_interest: some(4),
                volume: some(5),
                delta: some(6).map(|d| -d),
                gamma: some(7),
                theta: some(8),
                vega: Some(-0.0),
            }
        })
        .collect()
}

fn financial_facts() -> Vec<FinancialFactRow> {
    (0..5)
        .map(|i| FinancialFactRow {
            filed_ms: 1_700_000_000_000 + i * 86_400_000,
            cik: "0000320193".to_string(),
            entity: "Apple Inc.".to_string(),
            taxonomy: "us-gaap".to_string(),
            concept: if i < 3 { "Revenues" } else { "AccountsPayableCurrent" }.to_string(),
            unit: "USD".to_string(),
            period_start: if i == 4 { "" } else { "2023-01-01" }.to_string(),
            period_end: "2023-12-31".to_string(),
            value: 3.8e11 + i as f64,
            accession: format!("0000320193-24-00000{i}"),
            fiscal_year: "2023".to_string(),
            fiscal_period: "FY".to_string(),
            form: "10-K".to_string(),
            frame: String::new(),
        })
        .collect()
}

fn holdings() -> Vec<InstitutionalHoldingRow> {
    (0..5)
        .map(|i| InstitutionalHoldingRow {
            filed_ms: 1_700_000_000_000,
            accession: "0001067983-24-000006".to_string(),
            filer_cik: "0001067983".to_string(),
            filer_name: "BERKSHIRE HATHAWAY INC".to_string(),
            period: "2023-12-31".to_string(),
            submission_type: "13F-HR".to_string(),
            amendment_type: String::new(),
            issuer: format!("ISSUER {i}"),
            class_title: "COM".to_string(),
            cusip: format!("03783310{i}"),
            value_usd: i64::MAX - i,
            shares: -i,
            shares_type: "SH".to_string(),
            put_call: if i == 3 { "Put" } else { "" }.to_string(),
        })
        .collect()
}

fn text_scores() -> Vec<TextScoreRow> {
    (0..6)
        .map(|i| TextScoreRow {
            received_ms: 1_700_000_000_000 + i / 2,
            text_id: format!("text-{}", i / 2),
            chunk: (i % 2) as u32,
            scored_ms: 1_700_000_100_000 + i,
            model: "finbert@rev1".to_string(),
            values: "0.1\u{1f}0.7\u{1f}0.2".to_string(),
        })
        .collect()
}

fn fills() -> Vec<ExecFillRow> {
    (0..8)
        .map(|i| {
            let word = |k: i64| ((i + k) % 2 == 0).then(|| format!("w{}", i + k));
            ExecFillRow {
                ts: 1_700_000_000_000 + i * 10,
                trade_id: format!("t{i}"),
                order_client_id: if i == 1 { String::new() } else { format!("o{}", i / 2) },
                venue: "binance".to_string(),
                symbol: "BTCUSDT".to_string(),
                side: if i % 2 == 0 { 1 } else { -1 },
                qty: 0.001 * (i + 1) as f64,
                px: 64_000.0 + i as f64,
                commission: 0.01,
                mark_price: (i % 3 != 0).then_some(64_001.0),
                liquidity_side: if i % 2 == 0 { "maker" } else { "taker" }.to_string(),
                commission_asset: "USDT".to_string(),
                venue_order_id: word(0),
                account: word(1),
                position_side: (i == 3).then(|| "LONG".to_string()),
                corrects_trade_id: (i == 4).then(|| "t2".to_string()),
                correction_seq: (i == 4).then_some(0),
                mount_id: word(0),
                received_ms: (i % 4 == 0).then_some(1_700_000_000_500 + i),
                ownership: (i != 6).then(|| "ours".to_string()),
                journal_sequence: (i % 3 == 0).then_some(i),
                journal_generation: (i % 3 == 1).then_some(-i),
            }
        })
        .collect()
}

fn orders() -> Vec<ExecOrderRow> {
    (0..8)
        .map(|i| ExecOrderRow {
            ts: 1_700_000_000_000 + i * 10,
            order_client_id: format!("o{}", i / 3),
            venue: "bybit".to_string(),
            symbol: if i < 6 { "ETHUSDT" } else { "SOLUSDT" }.to_string(),
            side: if i % 2 == 0 { 1 } else { -1 },
            qty: 0.5,
            order_type: if i % 3 == 0 { "MARKET" } else { "LIMIT" }.to_string(),
            status: ["NEW", "PARTIALLY_FILLED", "FILLED"][i as usize % 3].to_string(),
            price: (i % 3 != 0).then_some(3_000.0 + i as f64),
            trigger_price: (i == 5).then_some(2_900.0),
            venue_order_id: (i % 3 != 0).then(|| format!("v{i}")),
            filled_qty: 0.25 * (i % 3) as f64,
            avg_fill_px: if i % 3 == 0 { 0.0 } else { 3_000.5 },
            account: Some("DEFAULT".to_string()),
            mount_id: (i < 4).then(|| "mount-a".to_string()),
            ownership: None,
            journal_sequence: (i != 2).then_some(i * 7),
            journal_generation: (i != 2).then_some(1),
            reason: (i == 7).then(|| "rejected: Ünïcode".to_string()),
        })
        .collect()
}

/// [`holds_every_way`], and the rows decoded EQUAL the rows (every field, the strings and the optional ones
/// included), for none, one and all of them.
fn crosses_equal<R: ColumnRow + PartialEq + std::fmt::Debug>(magic: &ColumnsMagic, rows: &[R]) {
    holds_every_way(magic, rows);
    for rows in [&rows[..0], &rows[..1], rows] {
        let back: Vec<R> = decoded(magic, &encoded(magic, rows)).expect("decodes");
        assert_eq!(back, rows, "{} x{}", magic.name, rows.len());
        let back: Vec<R> =
            decoded(magic, &compress_columns(magic, &encoded(magic, rows))).expect("decodes");
        assert_eq!(back, rows, "{} x{} compressed", magic.name, rows.len());
    }
}

#[test]
fn every_row_shaped_scan_crosses_equal_streamed_or_packed_and_its_length_is_counted_exactly() {
    crosses_equal(&EQUITY_COLUMNS, &equity());
    crosses_equal(&FORECAST_COLUMNS, &forecasts());
    crosses_equal(&COHORT_COLUMNS, &cohorts());
    crosses_equal(&TEXT_COLUMNS, &texts());
    crosses_equal(&CHAIN_COLUMNS, &chain());
    crosses_equal(&FINANCIAL_FACT_COLUMNS, &financial_facts());
    crosses_equal(&INSTITUTIONAL_HOLDING_COLUMNS, &holdings());
    crosses_equal(&TEXT_SCORE_COLUMNS, &text_scores());
    crosses_equal(&EXECUTION_FILL_COLUMNS, &fills());
    crosses_equal(&EXECUTION_ORDER_COLUMNS, &orders());
}

#[test]
fn every_row_shaped_scan_answers_the_rows_its_json_reply_answers() {
    answers_what_json_answers(&EQUITY_COLUMNS, &equity());
    answers_what_json_answers(&COHORT_COLUMNS, &cohorts());
    answers_what_json_answers(&TEXT_COLUMNS, &texts());
    answers_what_json_answers(&FINANCIAL_FACT_COLUMNS, &financial_facts());
    answers_what_json_answers(&INSTITUTIONAL_HOLDING_COLUMNS, &holdings());
    answers_what_json_answers(&TEXT_SCORE_COLUMNS, &text_scores());
    // A non-finite optional float reads `None` through JSON's `null`, and so here, in its word.
    let mut forecast = forecasts();
    forecast[1].confidence = Some(f64::NAN);
    forecast[2].confidence = Some(f64::NEG_INFINITY);
    answers_what_json_answers(&FORECAST_COLUMNS, &forecast);
    let back: Vec<ExecForecastRow> =
        decoded(&FORECAST_COLUMNS, &encoded(&FORECAST_COLUMNS, &forecast)).unwrap();
    assert_eq!((back[1].confidence, back[2].confidence), (None, None));
    let mut options = chain();
    options[1].iv = Some(f64::INFINITY);
    options[2].vega = Some(f64::NAN);
    answers_what_json_answers(&CHAIN_COLUMNS, &options);
    let mut fill = fills();
    fill[1].mark_price = Some(f64::NAN);
    answers_what_json_answers(&EXECUTION_FILL_COLUMNS, &fill);
    let mut order = orders();
    order[1].trigger_price = Some(f64::INFINITY);
    answers_what_json_answers(&EXECUTION_ORDER_COLUMNS, &order);
    // An optional integer of zero is present, not absent: its flag says so.
    let back: Vec<ExecFillRow> =
        decoded(&EXECUTION_FILL_COLUMNS, &encoded(&EXECUTION_FILL_COLUMNS, &fills())).unwrap();
    assert_eq!((back[4].correction_seq, back[0].journal_sequence), (Some(0), Some(0)));
    assert_eq!(back[5].correction_seq, None);
}

#[test]
fn a_row_shaped_reply_is_refused_under_another_replys_magic_and_every_magic_is_distinct() {
    let magics = [
        BAR_COLUMNS,
        QUOTE_COLUMNS,
        TRADE_COLUMNS,
        BOOK_UPDATE_COLUMNS,
        DEPTH_COLUMNS,
        PERP_METRIC_COLUMNS,
        EQUITY_COLUMNS,
        FORECAST_COLUMNS,
        COHORT_COLUMNS,
        TEXT_COLUMNS,
        CHAIN_COLUMNS,
        FINANCIAL_FACT_COLUMNS,
        INSTITUTIONAL_HOLDING_COLUMNS,
        TEXT_SCORE_COLUMNS,
        EXECUTION_FILL_COLUMNS,
        EXECUTION_ORDER_COLUMNS,
    ];
    let mut every: Vec<[u8; 4]> = magics.iter().flat_map(|m| [m.plain, m.lz4]).collect();
    // The hub frame's own msgpack magics are not columns.
    every.extend([*b"\0MPK", *b"\0MLZ"]);
    assert!(every.iter().all(|m| m[0] == 0), "a NUL first, which no JSON body has");
    every.sort_unstable();
    every.dedup();
    assert_eq!(every.len(), 2 * magics.len() + 2, "every magic is distinct");
    let fill = encoded(&EXECUTION_FILL_COLUMNS, &fills());
    assert!(decoded::<ExecOrderRow>(&EXECUTION_ORDER_COLUMNS, &fill).is_err());
    let text = encoded(&TEXT_COLUMNS, &texts());
    assert!(decoded::<TextScoreRow>(&TEXT_SCORE_COLUMNS, &text).is_err());
}

#[test]
fn a_malformed_text_column_is_refused_and_appends_nothing() {
    let rows = cohorts();
    let body = encoded(&COHORT_COLUMNS, &rows);
    // The last text column (`label_basis`, one run): its count one short of the rows.
    let run = "Ünïcode".len();
    let count_at = body.len() - run - 8;
    let mut short = body.clone();
    short[count_at] -= 1;
    refused::<CohortRow>(&COHORT_COLUMNS, &short, "a text column that does not cover the rows");
    // The same run's length set to none: a cohort always carries its label basis.
    let mut none = body[..body.len() - run].to_vec();
    let len_at = none.len() - 4;
    none[len_at..].copy_from_slice(&0u32.to_le_bytes());
    refused::<CohortRow>(&COHORT_COLUMNS, &none, "a text the kind always has, absent");
    refused::<CohortRow>(&COHORT_COLUMNS, &body[..body.len() - 1], "cut inside a text");
    let mut stray = body.clone();
    stray.push(0);
    refused::<CohortRow>(&COHORT_COLUMNS, &stray, "a byte after the last text run");
    let mut held = vec![rows[0].clone()];
    for bad in [&short, &none, &stray] {
        assert!(decode_columns_into(bad, &COHORT_COLUMNS, &mut held).is_err());
    }
    assert_eq!(held.len(), 1, "a refused body appends nothing");
    // A flag bit a fill has no use for.
    let fills = fills();
    let mut flag = encoded(&EXECUTION_FILL_COLUMNS, &fills);
    flag[HEAD + 8 * 10 * fills.len()] |= 0b1_0000;
    refused::<ExecFillRow>(&EXECUTION_FILL_COLUMNS, &flag, "a flag bit a fill has no use for");
}

/// A kind of no text column writes no text run: its body is byte for byte what it was before the text columns.
#[test]
fn a_kind_without_text_columns_keeps_its_body() {
    assert_eq!(columns_empty_len::<Bar>(), COLUMNS_EMPTY_LEN);
    assert_eq!(columns_empty_len::<TickQuote>(), COLUMNS_EMPTY_LEN);
    assert_eq!(columns_empty_len::<TextRow>(), COLUMNS_EMPTY_LEN + 4 * 16);
    assert_eq!(encoded(&EQUITY_COLUMNS, &[] as &[EquitySample]).len(), COLUMNS_EMPTY_LEN);
}

// ---- a run's expansion is bounded: count x length comes off the wire --------------------------------

/// Rows in a body for [`a_run_that_expands_past_one_frame_of_text_is_refused_plain_and_lz4`].
const EXPANDED_ROWS: usize = 10_000;

/// `body`, whose LAST run column is ONE run of the one-byte value `x` over every row, with that value made
/// `len` bytes long: the run then copies `len` bytes into each of its rows when it is decoded.
fn lengthened(mut body: Vec<u8>, len: usize) -> Vec<u8> {
    let at = body.len() - 5;
    assert_eq!(body[at..], [2, 0, 0, 0, b'x'], "guard: the last run is the value `x`");
    body.truncate(at);
    body.extend_from_slice(&(len as u32 + 1).to_le_bytes());
    body.extend(std::iter::repeat_n(b'x', len));
    body
}

/// `rows`' body (its last run column the value `x`) decodes with that value a hundred bytes long, and is refused,
/// plain and compressed, appending nothing, once that value's copies pass `MAX_FRAME_LEN`.
fn refuses_an_expansion_past_the_frame<R: ColumnRow>(magic: &ColumnsMagic, rows: &[R]) {
    let long = MAX_FRAME_LEN as usize / EXPANDED_ROWS + 1;
    assert!(rows.len() == EXPANDED_ROWS && EXPANDED_ROWS * long > MAX_FRAME_LEN as usize);
    let fine = lengthened(encoded(magic, rows), 100);
    assert_eq!(decoded::<R>(magic, &fine).map(|r| r.len()), Ok(EXPANDED_ROWS), "{}", magic.name);
    let body = lengthened(encoded(magic, rows), long);
    assert!(body.len() < 2_000_000, "guard: a small body");
    for body in [compress_columns(magic, &body), body] {
        let mut out: Vec<R> = Vec::new();
        let Err(e) = decode_columns_into(&body, magic, &mut out) else {
            panic!("{}: a run expanding to {} bytes decoded", magic.name, EXPANDED_ROWS * long)
        };
        assert!(e.contains("more text than one reply can carry"), "{}: {e}", magic.name);
        assert!(out.is_empty(), "{}: a refused body appends nothing", magic.name);
        let kept = plain_columns(body, magic).expect("a plain body");
        let refused = check_columns::<R>(&kept, magic).expect_err("a keeper refuses it too");
        assert!(
            refused.contains("more text than one reply can carry"),
            "{}: {refused}",
            magic.name
        );
    }
}

/// **A run whose copies pass one frame of text is refused**, for each magic family: bars (the symbol run, the
/// path bar columns took on `main`), ticks (a trade's symbol) and the D3 text kind (its last text column). Each
/// body is a few hundred KB (LZ4: a few KB) and would otherwise ask for ~67 MB, and ~TB at the wire's widest.
#[test]
fn a_run_that_expands_past_one_frame_of_text_is_refused_plain_and_lz4() {
    let bars: Vec<Bar> = (0..EXPANDED_ROWS as i64)
        .map(|i| Bar { symbol: Some("x".into()), ..Bar::ohlcv(i, 1.0, 2.0, 0.5, 1.5, 3.0) })
        .collect();
    refuses_an_expansion_past_the_frame(&BAR_COLUMNS, &bars);
    let trades: Vec<TickTrade> = (0..EXPANDED_ROWS as i64)
        .map(|i| TickTrade {
            timestamp: i,
            local_timestamp: 0,
            price: 1.0,
            size: 0.5,
            is_buyer_maker: i % 2 == 0,
            symbol: "x".into(),
        })
        .collect();
    refuses_an_expansion_past_the_frame(&TRADE_COLUMNS, &trades);
    let base = texts()[0].clone();
    let texts: Vec<TextRow> = (0..EXPANDED_ROWS as i64)
        .map(|i| TextRow { received_ms: i, amends: "x".to_string(), ..base.clone() })
        .collect();
    refuses_an_expansion_past_the_frame(&TEXT_COLUMNS, &texts);
}

// ---- bodies already encoded, spliced: the encoder's bytes, copied ------------------------------------

/// `rows` kept as three bodies, each cut every way (none of it, all of it, its head, its tail, its middle), spliced
/// into one: always the body the cut rows encode to, a run of one symbol going on across two bodies included, and
/// [`splice_columns_within`] counts that body's length exactly before it writes.
fn splices_to_the_encoders_bytes<R: ColumnRow + Clone>(magic: &ColumnsMagic, rows: &[R]) {
    let n = rows.len();
    let edges = [0, n / 3, n * 2 / 3 + 1, n];
    let bodies: Vec<(Vec<u8>, &[R])> = edges
        .windows(2)
        .map(|edge| (encoded(magic, &rows[edge[0]..edge[1]]), &rows[edge[0]..edge[1]]))
        .collect();
    let cuts_of = |len: usize| [0..0, 0..len, 0..len / 2, len / 2..len, len / 3..len - len / 3];
    for a in cuts_of(bodies[0].1.len()) {
        for b in cuts_of(bodies[1].1.len()) {
            for c in cuts_of(bodies[2].1.len()) {
                let cuts = [a.clone(), b.clone(), c.clone()];
                let runs: Vec<(&[u8], std::ops::Range<usize>)> = bodies
                    .iter()
                    .zip(&cuts)
                    .map(|((body, _), cut)| (body.as_slice(), cut.clone()))
                    .collect();
                let mut spliced = Vec::new();
                splice_columns_into::<R>(&mut spliced, magic, &runs).expect("a splice");
                let joined: Vec<R> = bodies
                    .iter()
                    .zip(&cuts)
                    .flat_map(|((_, rows), cut)| rows[cut.clone()].iter().cloned())
                    .collect();
                assert!(spliced == encoded(magic, &joined), "{}: {cuts:?}", magic.name);
                // The counted length is the body's: a byte under it appends nothing, exactly it appends the body.
                let mut within = vec![7u8];
                let most = spliced.len() - 1;
                let under = splice_columns_within::<R>(&mut within, magic, &runs, most);
                assert!(under == Ok(false) && within == [7], "{}: {cuts:?}", magic.name);
                let fits = splice_columns_within::<R>(&mut within, magic, &runs, spliced.len());
                assert!(fits == Ok(true) && within[1..] == spliced[..], "{}: {cuts:?}", magic.name);
            }
        }
    }
}

/// **A splice of encoded bodies is byte for byte the body their cut rows encode to**, for bars (optional fields, a
/// symbol of none), quotes, trades, book events (their levels) and a kind with text columns.
#[test]
fn a_splice_writes_the_encoders_bytes() {
    splices_to_the_encoders_bytes(&BAR_COLUMNS, &rows());
    splices_to_the_encoders_bytes(&QUOTE_COLUMNS, &quotes());
    splices_to_the_encoders_bytes(&TRADE_COLUMNS, &trades());
    splices_to_the_encoders_bytes(&BOOK_UPDATE_COLUMNS, &books());
    splices_to_the_encoders_bytes(&TEXT_COLUMNS, &texts());
}

/// **A splice refuses a body of another magic and a cut past the rows, and appends nothing**; [`columns_word`] reads a
/// word where it lies and nothing past the rows.
#[test]
fn a_splice_refuses_what_it_cannot_cut_and_a_word_is_read_in_place() {
    let rows = trades();
    let body = encoded(&TRADE_COLUMNS, &rows);
    let body = body.as_slice();
    let mut out = vec![7u8];
    for (magic, cut) in [(&QUOTE_COLUMNS, 0..1), (&TRADE_COLUMNS, 0..rows.len() + 1)] {
        let e = splice_columns_into::<TickTrade>(&mut out, magic, &[(body, 0..1), (body, cut)]);
        assert!(e.is_err() && out == [7], "{}: {e:?}", magic.name);
    }
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(columns_word(body, 0, i), Some(row.timestamp as u64));
        assert_eq!(columns_word(body, 2, i), Some(row.price.to_bits()));
    }
    assert_eq!(columns_word(body, 0, rows.len()), None);
    assert_eq!(columns_word(&body[..6], 0, 0), None);
    assert_eq!((columns_rows(body), columns_rows(&body[..6])), (Some(rows.len()), None));
}

// ---- a body KEPT as it came: its plain bytes, checked as the decoder checks it ----------------------

/// **A kind's own `check_row` refuses exactly what its `from_columns` refuses**, for the four kinds that state their
/// checks without building a row (the backtesthub keeps them by the million): every flag byte, with a symbol, an empty
/// one and none.
#[test]
fn check_row_refuses_what_from_columns_refuses() {
    fn agrees<R: ColumnRow>(name: &str) {
        let (words, texts) = ([7u64; MAX_COLUMN_WORDS], [None; MAX_TEXT_COLUMNS]);
        for flags in 0..=u8::MAX {
            for symbol in [None, Some(""), Some("BTCUSDT")] {
                let built = R::from_columns(&words, flags, [Vec::new(), Vec::new()], symbol, texts);
                let checked = R::check_row(&words, flags, symbol, texts);
                assert_eq!(checked, built.map(drop), "{name}: flags {flags}, symbol {symbol:?}");
            }
        }
    }
    agrees::<Bar>("bar");
    agrees::<TickQuote>("quote");
    agrees::<TickTrade>("trade");
    agrees::<L2BookUpdate>("book");
}

/// **A kept body is the reply's PLAIN body**: a plain one as it came, an LZ4 one decompressed to the same bytes; one
/// under another reply's magic, or an LZ4 one stating more than a frame, is refused.
#[test]
fn a_kept_body_is_the_plain_body_and_another_magic_is_refused() {
    let plain = encoded(&TRADE_COLUMNS, &trades());
    assert_eq!(plain_columns(plain.clone(), &TRADE_COLUMNS), Ok(plain.clone()));
    let packed = compress_columns(&TRADE_COLUMNS, &plain);
    assert_eq!(plain_columns(packed.clone(), &TRADE_COLUMNS), Ok(plain.clone()));
    let crossed = plain_columns(encoded(&QUOTE_COLUMNS, &quotes()), &TRADE_COLUMNS);
    let crossed = crossed.expect_err("a quote body on a trade read");
    assert!(crossed.contains("does not open with the trade-columns magic"), "{crossed}");
    let packed_quotes = compress_columns(&QUOTE_COLUMNS, &encoded(&QUOTE_COLUMNS, &quotes()));
    assert!(plain_columns(packed_quotes, &TRADE_COLUMNS).is_err(), "a compressed one too");
    let mut huge = packed;
    huge[4..8].copy_from_slice(&(MAX_FRAME_LEN + 1).to_le_bytes());
    let huge = plain_columns(huge, &TRADE_COLUMNS).expect_err("a length past the frame");
    assert!(huge.contains("over MAX_FRAME_LEN"), "{huge}");
}
