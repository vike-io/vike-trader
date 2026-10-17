//! A minimal LZMA encoder for the fixtures `lzma-rs`'s own cannot produce, and its self-test.

use crate::archive::limits::{MIB, UNKNOWN_SIZE};

// ── a minimal LZMA ENCODER, for the fixtures `lzma-rs`'s cannot produce ───────────────────────
//
// `lzma-rs`'s encoder emits LITERALS only — 70 MiB of zeros comes out at megabytes, not the few
// KB a real decompression bomb is — and it always writes an 8 MiB dictionary, so it cannot make
// a stream whose dictionary window flushes to the sink before a small cap.
// This one writes `lc = lp = pb = 0` streams of literals followed by rep0 matches that repeat
// the LAST byte (distance 1), then — unless the size is declared — the end-of-stream marker.
// It is the exact inverse of `lzma-rs`'s range DECODER, and
// `the_run_encoder_writes_streams_lzma_rs_itself_decodes` checks it against that decoder before
// any other test leans on it.

/// How a run stream ends.
#[derive(Clone, Copy, Debug)]
pub(super) enum Ending {
    /// The header declares the size; no end marker.
    Declared,
    /// Unknown size in the header, then the end-of-stream marker.
    EndMarker,
    /// `EndMarker`, then the stream's last five bytes cut off — a decoder that reaches the end
    /// fails there, so a refusal of another class proves it stopped earlier.
    CutShort,
}

/// The LZMA SDK's range encoder.
struct RangeEnc {
    out: Vec<u8>,
    low: u64,
    range: u32,
    cache: u8,
    pending: u64,
}

impl RangeEnc {
    fn bit(&mut self, prob: &mut u16, bit: bool) {
        let bound = (self.range >> 11) * u32::from(*prob);
        if bit {
            *prob -= *prob >> 5;
            self.low += u64::from(bound);
            self.range -= bound;
        } else {
            *prob += (0x800 - *prob) >> 5;
            self.range = bound;
        }
        self.normalize();
    }

    /// Fixed-probability bits, most significant first (a match distance's direct bits).
    fn direct(&mut self, value: u32, bits: u32) {
        for i in (0..bits).rev() {
            self.range >>= 1;
            if (value >> i) & 1 == 1 {
                self.low += u64::from(self.range);
            }
            self.normalize();
        }
    }

    /// A bit tree, most significant bit first.
    fn tree(&mut self, probs: &mut [u16], bits: u32, value: u32) {
        let mut node = 1usize;
        for i in (0..bits).rev() {
            let bit = (value >> i) & 1 == 1;
            self.bit(&mut probs[node], bit);
            node = (node << 1) | usize::from(bit);
        }
    }

    /// A bit tree, least significant bit first (the distance's align bits).
    fn reverse_tree(&mut self, probs: &mut [u16], bits: u32, value: u32) {
        let mut node = 1usize;
        for i in 0..bits {
            let bit = (value >> i) & 1 == 1;
            self.bit(&mut probs[node], bit);
            node = (node << 1) | usize::from(bit);
        }
    }

    fn normalize(&mut self) {
        while self.range < 1 << 24 {
            self.range <<= 8;
            self.shift_low();
        }
    }

    fn shift_low(&mut self) {
        if self.low < 0xFF00_0000 || self.low > 0xFFFF_FFFF {
            let carry = (self.low >> 32) as u8;
            let mut byte = self.cache;
            loop {
                self.out.push(byte.wrapping_add(carry));
                byte = 0xFF;
                self.pending -= 1;
                if self.pending == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
        }
        self.pending += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }

    fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.out
    }
}

const HALF: u16 = 0x400;

/// One length coder's probabilities (the decoder keeps two: rep matches and new matches).
struct LenProbs {
    choice: u16,
    choice2: u16,
    low: [u16; 8],
    mid: [u16; 8],
    high: [u16; 256],
}

impl LenProbs {
    fn new() -> LenProbs {
        LenProbs { choice: HALF, choice2: HALF, low: [HALF; 8], mid: [HALF; 8], high: [HALF; 256] }
    }

    /// `v` = match length − 2, at position state 0.
    fn encode(&mut self, rc: &mut RangeEnc, v: u32) {
        if v < 8 {
            rc.bit(&mut self.choice, false);
            rc.tree(&mut self.low, 3, v);
        } else if v < 16 {
            rc.bit(&mut self.choice, true);
            rc.bit(&mut self.choice2, false);
            rc.tree(&mut self.mid, 3, v - 8);
        } else {
            rc.bit(&mut self.choice, true);
            rc.bit(&mut self.choice2, true);
            rc.tree(&mut self.high, 8, v - 16);
        }
    }
}

/// The encoder's model: the decoder's probability slots for state × position state 0.
struct RunEncoder {
    rc: RangeEnc,
    state: usize,
    is_match: [u16; 12],
    is_rep: [u16; 12],
    is_rep_g0: [u16; 12],
    is_rep0_long: [u16; 12],
    literal: [u16; 0x100],
    rep_len: LenProbs,
    match_len: LenProbs,
    pos_slot: [u16; 64],
    align: [u16; 16],
}

impl RunEncoder {
    fn new(dict: u32, declared: Option<u64>) -> RunEncoder {
        let mut out = vec![0u8]; // lc = lp = pb = 0
        out.extend_from_slice(&dict.to_le_bytes());
        out.extend_from_slice(&declared.unwrap_or(UNKNOWN_SIZE).to_le_bytes());
        RunEncoder {
            rc: RangeEnc { out, low: 0, range: u32::MAX, cache: 0, pending: 1 },
            state: 0,
            is_match: [HALF; 12],
            is_rep: [HALF; 12],
            is_rep_g0: [HALF; 12],
            is_rep0_long: [HALF; 12],
            literal: [HALF; 0x100],
            rep_len: LenProbs::new(),
            match_len: LenProbs::new(),
            pos_slot: [HALF; 64],
            align: [HALF; 16],
        }
    }

    fn literal(&mut self, byte: u8) {
        let s = self.state;
        assert!(s < 7, "only a literal after a literal is modelled (no matched literal)");
        self.rc.bit(&mut self.is_match[s], false);
        self.rc.tree(&mut self.literal, 8, u32::from(byte));
        self.state = if s < 4 { 0 } else { s - 3 };
    }

    /// A rep0 match of `len` (1 = a short rep), repeating at the last distance — 1 here.
    fn rep0(&mut self, len: u32) {
        assert!((1..=273).contains(&len), "an LZMA match is 1..=273 bytes");
        let s = self.state;
        self.rc.bit(&mut self.is_match[s], true);
        self.rc.bit(&mut self.is_rep[s], true);
        self.rc.bit(&mut self.is_rep_g0[s], false);
        if len == 1 {
            self.rc.bit(&mut self.is_rep0_long[s], false);
            self.state = if s < 7 { 9 } else { 11 };
        } else {
            self.rc.bit(&mut self.is_rep0_long[s], true);
            self.rep_len.encode(&mut self.rc, len - 2);
            self.state = if s < 7 { 8 } else { 11 };
        }
    }

    /// The end-of-stream marker: a new match of length 2 at distance `0xFFFF_FFFF` — pos slot
    /// 63, then 26 direct bits and 4 align bits, all ones.
    fn end_marker(&mut self) {
        let s = self.state;
        self.rc.bit(&mut self.is_match[s], true);
        self.rc.bit(&mut self.is_rep[s], false);
        self.match_len.encode(&mut self.rc, 0);
        self.rc.tree(&mut self.pos_slot, 6, 63);
        self.rc.direct(0x03FF_FFFF, 26);
        self.rc.reverse_tree(&mut self.align, 4, 0xF);
    }
}

/// `prefix` as literals, then its last byte repeated until the stream decodes to `total` bytes.
pub(super) fn run(prefix: &[u8], total: u64, dict: u32, ending: Ending) -> Vec<u8> {
    assert!(!prefix.is_empty() && prefix.len() as u64 <= total);
    let declared = matches!(ending, Ending::Declared).then_some(total);
    let mut e = RunEncoder::new(dict, declared);
    for &b in prefix {
        e.literal(b);
    }
    let mut left = total - prefix.len() as u64;
    while left > 0 {
        let len = left.min(273);
        e.rep0(len as u32);
        left -= len;
    }
    if !matches!(ending, Ending::Declared) {
        e.end_marker();
    }
    let mut out = e.rc.finish();
    if matches!(ending, Ending::CutShort) {
        out.truncate(out.len() - 5);
    }
    out
}

#[test]
fn the_run_encoder_writes_streams_lzma_rs_itself_decodes() {
    let cases: [(&[u8], u64); 7] = [
        (&[7], 1),
        (&[1, 2, 3], 3),
        (&[0], 2),
        (&[9], 275),
        (&[0], 5_000),
        (&[4, 5], 8_193),
        (&[3], 300_000),
    ];
    for (prefix, total) in cases {
        let last = *prefix.last().expect("a prefix");
        let mut want = prefix.to_vec();
        want.resize(total as usize, last);
        for ending in [Ending::Declared, Ending::EndMarker] {
            for dict in [4_096, 4 * MIB as u32, u32::MAX] {
                let stream = run(prefix, total, dict, ending);
                let mut got = Vec::new();
                lzma_rs::lzma_decompress(&mut &stream[..], &mut got).unwrap_or_else(|e| {
                    panic!("{ending:?} {total} bytes, dict {dict}: lzma-rs refused it: {e}")
                });
                assert_eq!(got, want, "{ending:?} {total} bytes, dict {dict}");
            }
        }
        let cut = run(prefix, total, 4_096, Ending::CutShort);
        let mut got = Vec::new();
        assert!(
            lzma_rs::lzma_decompress(&mut &cut[..], &mut got).is_err(),
            "a stream cut short must not decode cleanly ({total} bytes)"
        );
    }
}
