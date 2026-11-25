//! The binary tick bodies: every kind round-trips its edge values bit for bit, a JSON frame and its binary twin
//! decode to the SAME frame over a the build runner stream and over random bit patterns, the committed goldens pin
//! the layout from outside `crates/` (written by an independent encoder), a frame the codec cannot carry is
//! handed back untouched, and a malformed body is refused. Plus the decode-cost probe (`#[ignore]`d).

use std::path::PathBuf;

use vike_marketdata::{L2BookLevel, L2BookUpdate, L2BookUpdateKind, TickQuote, TickTrade};

use super::*;

const CEILING: u32 = 1 << 20;

/// The goldens: `<hex body> TAB <the same frame as JSON>` a line, written by an encoder that is not this one.
const GOLDENS: &str = "fixtures/stream_wire/v1_binary_ticks.tsv";

/// `frame`'s binary BODY (no prefix). The encoding is a function of every field's bits and nothing else,
/// so two frames with the same body are the same frame bit for bit, NaN payloads and signed zeros included.
fn body_of(frame: &StreamDown) -> Vec<u8> {
    let mut out = Vec::new();
    assert!(encode_binary_tick_into(&mut out, frame, CEILING), "a tick kind encodes: {frame:?}");
    assert_eq!(u32::from_be_bytes(out[..4].try_into().unwrap()) as usize, out.len() - 4);
    out.split_off(4)
}

fn decoded(body: &[u8]) -> StreamDown {
    decode_stream_down(body).expect("decodes")
}

fn quote(k: u32, sequence: u64, quote: TickQuote) -> StreamDown {
    StreamDown::Quote { k, sequence, quote }
}

fn level(price: f64, qty: f64) -> L2BookLevel {
    L2BookLevel::new(price, qty)
}

/// Every kind, at the values a JSON frame cannot carry as well as the ones it can.
fn edge_frames() -> Vec<StreamDown> {
    let nan = f64::from_bits(0x7ff8_dead_beef_0001);
    let odd = [nan, f64::INFINITY, f64::NEG_INFINITY, -0.0, 5e-324, f64::MAX, f64::MIN, 1e-300];
    let mut frames = Vec::new();
    for (i, &v) in odd.iter().enumerate() {
        frames.push(quote(
            u32::MAX,
            u64::MAX,
            TickQuote {
                timestamp: i64::MAX,
                local_timestamp: i64::MIN,
                bid: v,
                ask: -v,
                bid_size: v,
                ask_size: 0.0,
                symbol: ["", "€ü✓", "x"][i % 3].repeat(i).into(),
            },
        ));
        frames.push(StreamDown::Trade {
            k: 0,
            sequence: 0,
            trade: TickTrade {
                timestamp: i64::MIN,
                local_timestamp: i64::MAX,
                price: v,
                size: v,
                is_buyer_maker: i % 2 == 0,
                symbol: "7".repeat(78).into(),
            },
        });
        frames.push(StreamDown::Mark { k: 1, sequence: i as u64, price: v, timestamp: i64::MAX });
        frames.push(StreamDown::BarCloseTick { k: 1, sequence: 2, price: v, timestamp: i64::MIN });
        frames.push(StreamDown::Depth {
            k: 9,
            sequence: 3,
            tick_size: v,
            bids: vec![level(v, -v); i],
            asks: vec![level(1.0, v)],
            timestamp: -1,
        });
    }
    for (i, kind) in [
        L2BookUpdateKind::Delta,
        L2BookUpdateKind::Snapshot,
        L2BookUpdateKind::GapStart,
        L2BookUpdateKind::Stale,
        L2BookUpdateKind::LiveResume,
    ]
    .into_iter()
    .enumerate()
    {
        frames.push(StreamDown::BookDelta {
            k: 4,
            sequence: u64::MAX - i as u64,
            update: Box::new(L2BookUpdate {
                timestamp: i64::MAX,
                local_timestamp: i64::MIN,
                sequence: u64::MAX,
                kind,
                tick_size: odd[i],
                bids: (0..i * 300).map(|n| level(n as f64 * 0.1, odd[n % odd.len()])).collect(),
                asks: vec![],
                symbol: "BTCUSDT.P".into(),
            }),
        });
    }
    frames
}

/// **Every kind survives its edge values bit for bit**: NaN with a payload, both infinities, `-0.0`, the smallest
/// subnormal, the extremes, `i64::MIN`/`MAX` stamps, `u64::MAX` sequences, `u32::MAX` keys, empty, multi-byte and
/// 78-digit symbols, every book kind, both aggressor flags, empty and 1 200-level sides.
#[test]
fn every_kind_survives_its_edge_values_bit_for_bit() {
    for frame in edge_frames() {
        let body = body_of(&frame);
        let back = decoded(&body);
        assert_eq!(body_of(&back), body, "{frame:?}");
    }
    let StreamDown::Quote { quote, .. } = decoded(&body_of(&edge_frames()[0])) else {
        panic!("a quote");
    };
    assert_eq!(quote.bid.to_bits(), 0x7ff8_dead_beef_0001, "a NaN keeps its payload");
    assert_eq!(quote.bid_size.to_bits(), quote.bid.to_bits());
}

/// The repository root this binary was built in, where `fixtures/` sits.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn hex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "an even number of hex digits");
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

/// **The goldens pin the layout and the identity together.** Each line's binary body decodes to the frame its
/// JSON twin decodes to, and this build encodes that frame to the golden's bytes exactly. The bytes were written
/// by an encoder that is not this one, and they live outside `crates/`, so an edit of the layout here cannot
/// carry them along. A new kind needs a line: every kind must appear.
#[test]
fn the_goldens_decode_to_their_json_twin_and_re_encode_byte_for_byte() {
    let text = std::fs::read_to_string(repo_root().join(GOLDENS)).expect("the goldens");
    let mut kinds = std::collections::BTreeSet::new();
    let mut lines = 0;
    for line in text.lines() {
        let (binary, json) = line.split_once('\t').expect("hex TAB json");
        let golden = hex(binary);
        let from_binary = decoded(&golden);
        let from_json = decoded(json.as_bytes());
        assert_eq!(body_of(&from_json), golden, "this build writes the golden's bytes: {json}");
        assert_eq!(body_of(&from_binary), golden, "and reads them back: {json}");
        kinds.insert(golden[2]);
        lines += 1;
    }
    assert_eq!(lines, 15, "a golden added or dropped: say so here");
    assert_eq!(kinds.into_iter().collect::<Vec<_>>(), vec![1, 2, 3, 4, 5, 6], "every kind");
}

/// A small deterministic generator (xorshift64): the sweeps below are the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// Any FINITE `f64`, from its bits: the values a JSON frame carries.
    fn finite(&mut self) -> f64 {
        loop {
            let v = f64::from_bits(self.next());
            if v.is_finite() {
                return v;
            }
        }
    }

    fn text(&mut self) -> String {
        (0..self.below(24)).filter_map(|_| char::from_u32(self.below(0x11_0000) as u32)).collect()
    }

    fn levels(&mut self, max: u64) -> Vec<L2BookLevel> {
        (0..self.below(max)).map(|_| level(self.finite(), self.finite())).collect()
    }
}

/// A stream SHAPED like the CI box's strategy lane (BTCUSDT.P on a 0.1 grid near 97 000, sizes in 0.001 lots,
/// millisecond stamps of November 2026, a quote-and-trade-heavy mix with book deltas and marks): prices both as a
/// venue's decimal text parses (`"97123.4"`) and as grid arithmetic leaves them (`971234.0 * 0.1`). SYNTHETIC: no
/// live capture is replayed here (a capture would be a fixture from the CI box's store, and none is committed).
fn prod2_shaped(n: usize) -> Vec<StreamDown> {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut ticks: i64 = 971_234;
    let mut ts: i64 = 1_763_500_000_000;
    let mut sequences = [0u64; 4];
    let mut book_sequence: u64 = 48_151_623_420;
    let text = |t: i64| format!("{}.{}", t / 10, t % 10).parse::<f64>().expect("a price");
    (0..n)
        .map(|_| {
            ts += rng.below(3) as i64;
            let local = ts + 1 + rng.below(4) as i64;
            ticks += rng.below(3) as i64 - 1;
            let size = (1 + rng.below(5_000)) as f64 * 0.001;
            let symbol = vike_marketdata::Symbol::new("BTCUSDT.P");
            match rng.below(20) {
                0..=8 => {
                    sequences[0] += 1;
                    quote(
                        1,
                        sequences[0],
                        TickQuote {
                            timestamp: ts,
                            local_timestamp: local,
                            bid: text(ticks),
                            ask: (ticks + 1) as f64 * 0.1,
                            bid_size: size,
                            ask_size: (1 + rng.below(90)) as f64 * 0.001,
                            symbol,
                        },
                    )
                }
                9..=15 => {
                    sequences[1] += 1;
                    StreamDown::Trade {
                        k: 1,
                        sequence: sequences[1],
                        trade: TickTrade {
                            timestamp: ts,
                            local_timestamp: local,
                            price: text(ticks + rng.below(2) as i64),
                            size,
                            is_buyer_maker: rng.below(2) == 0,
                            symbol,
                        },
                    }
                }
                16..=18 => {
                    sequences[2] += 1;
                    book_sequence += 1 + rng.below(3);
                    let side = |rng: &mut Rng, from: i64, step: i64| {
                        (0..rng.below(4))
                            .map(|n| {
                                let qty = if rng.below(4) == 0 { 0.0 } else { size };
                                level(text(from + step * n as i64), qty)
                            })
                            .collect::<Vec<_>>()
                    };
                    StreamDown::BookDelta {
                        k: 1,
                        sequence: sequences[2],
                        update: Box::new(L2BookUpdate {
                            timestamp: ts,
                            local_timestamp: local,
                            sequence: book_sequence,
                            kind: L2BookUpdateKind::Delta,
                            tick_size: 0.1,
                            bids: side(&mut rng, ticks, -1),
                            asks: side(&mut rng, ticks + 1, 1),
                            symbol: symbol.to_string(),
                        }),
                    }
                }
                _ => {
                    sequences[3] += 1;
                    StreamDown::Mark {
                        k: 1,
                        sequence: sequences[3],
                        price: text(ticks) + 0.05,
                        timestamp: ts,
                    }
                }
            }
        })
        .collect()
}

/// Every tick kind at random finite bit patterns, random keys, sequences, stamps and symbols.
fn random_frames(n: usize) -> Vec<StreamDown> {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    (0..n)
        .map(|_| {
            let (k, sequence, timestamp) = (rng.next() as u32, rng.next(), rng.next() as i64);
            match rng.below(6) {
                0 => quote(
                    k,
                    sequence,
                    TickQuote {
                        timestamp,
                        local_timestamp: rng.next() as i64,
                        bid: rng.finite(),
                        ask: rng.finite(),
                        bid_size: rng.finite(),
                        ask_size: rng.finite(),
                        symbol: rng.text().into(),
                    },
                ),
                1 => StreamDown::Trade {
                    k,
                    sequence,
                    trade: TickTrade {
                        timestamp,
                        local_timestamp: rng.next() as i64,
                        price: rng.finite(),
                        size: rng.finite(),
                        is_buyer_maker: rng.below(2) == 1,
                        symbol: rng.text().into(),
                    },
                },
                2 => StreamDown::BookDelta {
                    k,
                    sequence,
                    update: Box::new(L2BookUpdate {
                        timestamp,
                        local_timestamp: rng.next() as i64,
                        sequence: rng.next(),
                        kind: [
                            L2BookUpdateKind::Delta,
                            L2BookUpdateKind::Snapshot,
                            L2BookUpdateKind::GapStart,
                            L2BookUpdateKind::Stale,
                            L2BookUpdateKind::LiveResume,
                        ][rng.below(5) as usize],
                        tick_size: rng.finite(),
                        bids: rng.levels(30),
                        asks: rng.levels(30),
                        symbol: rng.text(),
                    }),
                },
                3 => StreamDown::Mark { k, sequence, price: rng.finite(), timestamp },
                4 => StreamDown::BarCloseTick { k, sequence, price: rng.finite(), timestamp },
                _ => StreamDown::Depth {
                    k,
                    sequence,
                    tick_size: rng.finite(),
                    bids: rng.levels(30),
                    asks: rng.levels(30),
                    timestamp,
                },
            }
        })
        .collect()
}

/// The JSON body the writer's JSON path frames for `frame` (`crate::frame::encode_capped`), without its prefix.
fn json_body(frame: &StreamDown) -> Vec<u8> {
    let mut framed = crate::frame::encode_capped(frame, CEILING).expect("json");
    framed.split_off(4)
}

/// **A JSON frame and its binary twin decode to the SAME frame, bit for bit**, over a the build runner stream of
/// 20 000 frames and 20 000 frames of random finite bit patterns: what the core is handed does not depend on
/// which body the datahub wrote.
#[test]
fn json_and_binary_decode_to_identical_frames() {
    for frame in prod2_shaped(20_000).iter().chain(&random_frames(20_000)) {
        let (from_json, from_binary) = (decoded(&json_body(frame)), decoded(&body_of(frame)));
        let (j, b) = (body_of(&from_json), body_of(&from_binary));
        assert_eq!(j, b, "JSON and binary decode to different frames: {frame:?}");
        assert_eq!(b, body_of(frame), "and both are the frame that was sent: {frame:?}");
    }
}

/// **A frame this codec cannot carry is handed back untouched**, for the writer to frame as JSON: any kind
/// that is not a tick, a symbol over `u16::MAX` bytes, a body over the ceiling. Bytes already in the buffer stay.
#[test]
fn a_frame_the_codec_cannot_carry_is_handed_back_untouched() {
    let earlier = vec![1, 2, 3];
    let refused = |frame: &StreamDown, ceiling: u32| {
        let mut buffer = earlier.clone();
        assert!(!encode_binary_tick_into(&mut buffer, frame, ceiling), "{frame:?}");
        assert_eq!(buffer, earlier, "nothing added");
    };
    refused(&StreamDown::SubscribeOk { sid: 1, k: 1 }, CEILING);
    refused(&StreamDown::Heartbeat, CEILING);
    let book = Box::new(vike_marketdata::L2Book::new(0.1));
    refused(&StreamDown::Book { k: 1, sequence: 1, book }, CEILING);
    let fat =
        quote(1, 1, TickQuote { symbol: "x".repeat(usize::from(u16::MAX) + 1).into(), ..tick(1) });
    refused(&fat, CEILING);
    let fits =
        quote(1, 1, TickQuote { symbol: "x".repeat(usize::from(u16::MAX)).into(), ..tick(1) });
    let len = body_of(&fits).len() as u32;
    refused(&fits, len - 1);
    let mut buffer = earlier.clone();
    assert!(encode_binary_tick_into(&mut buffer, &fits, len), "exactly at the ceiling fits");
    assert_eq!(&buffer[..3], &earlier[..], "appended after what was there");
}

fn tick(ts: i64) -> TickQuote {
    TickQuote {
        timestamp: ts,
        local_timestamp: ts + 1,
        bid: 97_123.4,
        ask: 97_123.5,
        bid_size: 0.5,
        ask_size: 0.25,
        symbol: "BTCUSDT.P".into(),
    }
}

/// **A JSON body still decodes through the same door**: a datahub that never writes binary (an older one, or a
/// session that never asked) is read exactly as before.
#[test]
fn a_json_body_still_decodes() {
    assert!(matches!(
        decoded(br#"{"SubscribeOk":{"sid":1,"k":2}}"#),
        StreamDown::SubscribeOk { sid: 1, k: 2 }
    ));
    let frame = quote(1, 8, tick(5));
    assert_eq!(body_of(&decoded(&json_body(&frame))), body_of(&frame));
    let err = decode_stream_down(b"{not json").expect_err("garbage");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

/// **A malformed body is refused, never read short**: every truncation of every golden kind, a trailing byte, an
/// unknown version or kind, a flag that is not 0 or 1, an unknown book kind, a symbol that is not UTF-8, and a
/// level count no body could hold (refused before anything is allocated).
#[test]
fn a_malformed_body_is_refused_never_read_short() {
    let refused = |body: &[u8], why: &str| {
        let err = decode_stream_down(body).expect_err(why);
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{why}");
        err.to_string()
    };
    let samples = [
        quote(1, 1, tick(1)),
        StreamDown::Trade {
            k: 1,
            sequence: 1,
            trade: TickTrade {
                timestamp: 1,
                local_timestamp: 2,
                price: 1.0,
                size: 2.0,
                is_buyer_maker: true,
                symbol: "AAA".into(),
            },
        },
        StreamDown::BookDelta {
            k: 1,
            sequence: 1,
            update: Box::new(L2BookUpdate {
                timestamp: 1,
                local_timestamp: 2,
                sequence: 3,
                kind: L2BookUpdateKind::Delta,
                tick_size: 0.1,
                bids: vec![level(1.0, 0.0)],
                asks: vec![level(1.1, 2.0), level(1.2, 3.0)],
                symbol: "AAA".into(),
            }),
        },
        StreamDown::Mark { k: 1, sequence: 1, price: 1.0, timestamp: 1 },
        StreamDown::BarCloseTick { k: 1, sequence: 1, price: 1.0, timestamp: 1 },
        StreamDown::Depth {
            k: 1,
            sequence: 1,
            tick_size: 0.1,
            bids: vec![level(1.0, 2.0)],
            asks: vec![],
            timestamp: 1,
        },
    ];
    for frame in &samples {
        let body = body_of(frame);
        for cut in 1..body.len() {
            refused(&body[..cut], "a truncated body");
        }
        let mut longer = body.clone();
        longer.push(0);
        assert!(refused(&longer, "a trailing byte").contains("bytes follow"));
    }
    let quote_body = body_of(&samples[0]);
    let mut v2 = quote_body.clone();
    v2[1] = 2;
    assert!(refused(&v2, "version 2").contains("version 2"));
    let mut kind = quote_body.clone();
    kind[2] = 7;
    refused(&kind, "an unknown kind");
    let mut flag = body_of(&samples[1]);
    flag[15 + 32] = 2;
    refused(&flag, "a flag of 2");
    let mut book_kind = body_of(&samples[2]);
    book_kind[15 + 24] = 5;
    refused(&book_kind, "an unknown book kind");
    let mut utf8 = quote_body.clone();
    let last = utf8.len() - 1;
    utf8[last] = 0xff;
    refused(&utf8, "a symbol that is not UTF-8");
    let mut huge = body_of(&samples[5]);
    huge[15 + 16..15 + 20].copy_from_slice(&u32::MAX.to_le_bytes());
    refused(&huge, "a level count no body holds");
}

/// **A tick symbol over `Symbol::MAX_READ_LEN` is refused on both paths and interns nothing.** A quote's and a
/// trade's binary body (the two kinds whose symbol is interned) is refused as malformed, as the JSON twin is by
/// serde, and the text never reaches the process's table; a symbol AT the cap decodes on both paths.
#[test]
fn a_tick_symbol_over_the_read_cap_is_refused_and_never_interned() {
    let over = format!("never-interned-{}", "q".repeat(Symbol::MAX_READ_LEN - 14));
    assert_eq!(over.len(), Symbol::MAX_READ_LEN + 1);
    // Bodies are built with a placeholder of the same length, then the symbol bytes (the last field) swapped,
    // so the test itself never interns `over`.
    let placeholder = Symbol::new(&"p".repeat(over.len()));
    let trade = TickTrade {
        timestamp: 1,
        local_timestamp: 2,
        price: 1.0,
        size: 2.0,
        is_buyer_maker: false,
        symbol: placeholder,
    };
    let quoted = quote(1, 1, TickQuote { symbol: placeholder, ..tick(1) });
    for frame in [quoted.clone(), StreamDown::Trade { k: 1, sequence: 1, trade }] {
        let mut body = body_of(&frame);
        let start = body.len() - over.len();
        body[start..].copy_from_slice(over.as_bytes());
        let err = decode_stream_down(&body).expect_err("a binary symbol over the cap");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
    }
    let json =
        String::from_utf8(json_body(&quoted)).expect("JSON").replace(placeholder.as_str(), &over);
    let err = decode_stream_down(json.as_bytes()).expect_err("a JSON symbol over the cap");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
    assert!(!Symbol::is_interned(&over), "a refused symbol is never interned");
    let longest = Symbol::new(&"a".repeat(Symbol::MAX_READ_LEN));
    let at_cap = quote(1, 1, TickQuote { symbol: longest, ..tick(1) });
    assert_eq!(body_of(&decoded(&body_of(&at_cap))), body_of(&at_cap), "binary at the cap");
    assert_eq!(body_of(&decoded(&json_body(&at_cap))), body_of(&at_cap), "JSON at the cap");
}

/// A TIMING PROBE, not a gate (`#[ignore]`d; it asserts only that both decoders answer the same frames): decode and
/// encode of 200 000 the build runner frames as JSON bodies and as binary bodies, medians of nine rounds, each in turn.
/// Run on a lane: `cargo test -p vike-market-stream --release --lib -- --ignored --nocapture binary_ticks_timing`.
#[test]
#[ignore = "a timing probe: run by hand on a lane, --release --nocapture"]
fn binary_ticks_timing() {
    use std::hint::black_box;
    use std::time::Instant;
    const FRAMES: usize = 200_000;
    let frames = prod2_shaped(FRAMES);
    let json: Vec<Vec<u8>> = frames.iter().map(json_body).collect();
    let binary: Vec<Vec<u8>> = frames.iter().map(body_of).collect();
    let median = |mut runs: Vec<f64>| {
        runs.sort_by(f64::total_cmp);
        runs[runs.len() / 2]
    };
    let ns_per_frame = |f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        t.elapsed().as_nanos() as f64 / FRAMES as f64
    };
    let mut times: [Vec<f64>; 4] = Default::default();
    let mut buffer = Vec::with_capacity(64 * 1024 + 4096);
    for _ in 0..9 {
        times[0].push(ns_per_frame(&mut || {
            for body in &json {
                black_box(decode_stream_down(black_box(body)).expect("json"));
            }
        }));
        times[1].push(ns_per_frame(&mut || {
            for body in &binary {
                black_box(decode_stream_down(black_box(body)).expect("binary"));
            }
        }));
        times[2].push(ns_per_frame(&mut || {
            for frame in &frames {
                if buffer.len() > 64 * 1024 {
                    buffer.clear();
                }
                crate::frame::encode_capped_into(&mut buffer, black_box(frame), CEILING)
                    .expect("json");
            }
        }));
        buffer.clear();
        times[3].push(ns_per_frame(&mut || {
            for frame in &frames {
                if buffer.len() > 64 * 1024 {
                    buffer.clear();
                }
                assert!(encode_binary_tick_into(&mut buffer, black_box(frame), CEILING));
            }
        }));
        buffer.clear();
    }
    for (j, b) in json.iter().zip(&binary).take(1_000) {
        assert_eq!(body_of(&decoded(j)), body_of(&decoded(b)));
    }
    let bytes = |bodies: &[Vec<u8>]| {
        bodies.iter().map(|b| b.len() + 4).sum::<usize>() as f64 / FRAMES as f64
    };
    let (json_bytes, binary_bytes) = (bytes(&json), bytes(&binary));
    let [decode_json, decode_binary, encode_json, encode_binary] = times.map(median);
    // the CI box's measured lane rate (2026-11-19): about 125 KB/s of JSON ticks into the tradehub.
    let frames_per_second = 125_000.0 / json_bytes;
    println!(
        "BINARY-TICKS frames={FRAMES} bytes_per_frame json={json_bytes:.1} binary={binary_bytes:.1} \
         decode_ns json={decode_json:.1} binary={decode_binary:.1} \
         encode_ns json={encode_json:.1} binary={encode_binary:.1} \
         at_125KBps frames_per_s={frames_per_second:.0} decode_us_per_s json={:.1} binary={:.1}",
        decode_json * frames_per_second / 1e3,
        decode_binary * frames_per_second / 1e3,
    );
}
