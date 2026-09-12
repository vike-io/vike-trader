//! OKX order-book **CRC32 checksum** validation for the `books` channel (audit br1).
//!
//! This is NOT a Python port (the oracle app has no live OKX depth); it implements OKX's own
//! documented WS order-book checksum, cited inline below.
//!
//! ## OKX's documented algorithm (WS `books` "Checksum")
//! After merging the incremental `update` frames into the initial snapshot, a client can verify the
//! book is uncorrupted by re-deriving a CRC32 over the top of the MERGED book and comparing it to the
//! `checksum` the frame carried:
//!  1. Take the best **25** levels per side (bids high→low, asks low→high).
//!  2. Build a string by INTERLEAVING the two sides rank-by-rank, each level contributing its RAW
//!     `price:size` wire strings, everything joined by `:` —
//!     `bid0px:bid0sz:ask0px:ask0sz:bid1px:bid1sz:ask1px:ask1sz:…`. When one side is shorter it simply
//!     stops contributing at its last rank; the longer side's remaining levels are appended in order.
//!  3. `checksum = CRC32(string)` — CRC-32/ISO-HDLC (poly `0x04C11DB7` reflected, init/xorout all-ones;
//!     the same CRC used by zlib/PNG and `java.util.zip.CRC32`) — REINTERPRETED as a signed 32-bit
//!     integer, which is how OKX transmits the field.
//!
//! A mismatch means a delta was dropped or mis-applied WITHOUT breaking the `seqId`/`prevSeqId` chain —
//! the one book-corruption class the sequence check cannot catch — so the local book must be discarded
//! and resynced from a fresh snapshot.
//!
//! ## The strings must be OKX's, verbatim
//! The CRC is over the EXACT `price`/`size` strings OKX sent, so this module keeps a raw-string mirror
//! of the merged book ([`ChecksumBook`]). The folded [`vike_model::L2Book`] cannot serve here: it
//! quantizes price to an integer tick and stores f64 sizes, so reformatting those back to strings
//! would not reproduce OKX's formatting (e.g. a `"0.10000000"` size, a `"60000.0"` price) and the CRC
//! would never match. Ordering is by numeric price ([`f64::total_cmp`]); the value fed to the CRC is
//! always the original string, never the parsed float.
//!
//! ## ⚠ OKX DEPRECATED this field (2026-06-23)
//! Per the OKX V5 API changelog (2026-06-23), the `books`/`books-l2-tbt`/`books50-l2-tbt` channels now
//! send `checksum` **fixed to `0`** and direct clients to use `seqId`/`prevSeqId` instead (which this
//! feed already does — see `market_feed::depth_main`). So against a CURRENT live OKX stream this
//! validator is DORMANT by design: [`ChecksumBook`] is wired into the depth decode, but the caller
//! SKIPS validation whenever `checksum == 0`, so a zeroed field can never false-trip a resync. It
//! still fires for any NON-zero checksum — a pre-deprecation stream captured/replayed offline, or the
//! day OKX (or another venue reusing this) restores a real checksum — giving genuine corruption
//! detection with zero risk to the live feed. The `books5`/`bbo-tbt` channels never carried a checksum
//! and are unaffected.

use std::cmp::Ordering;
use std::collections::BTreeMap;

/// Best-25 depth OKX folds the checksum over, per side.
const TOP: usize = 25;

/// CRC-32/ISO-HDLC over `bytes` (the zlib/PNG/`java.util.zip.CRC32` CRC — what OKX specifies).
/// Bitwise reference form (reflected poly `0xEDB88320`, init/xorout all-ones); table-free so it needs
/// no dependency and stays `unsafe`-free. Depth frames arrive at ~100 ms cadence, far off any hot
/// path, so the per-byte loop is irrelevant. Anchored by the canonical check value `CRC32("123456789")
/// == 0xCBF43926` in the tests.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            // mask = 0xFFFFFFFF when the low bit is set, else 0 — branch-free poly conditional.
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Build OKX's checksum string from ALREADY-SORTED top-of-book slices (bids high→low, asks low→high):
/// interleave rank-by-rank, each level as `price:size` (RAW wire strings), joined by `:`, capped at
/// the top [`TOP`] per side. A shorter side stops contributing at its last rank. Pure over its inputs
/// — the interleave rule is unit-tested directly against OKX's documented format.
pub fn okx_checksum_string(bids: &[(&str, &str)], asks: &[(&str, &str)]) -> String {
    let nb = bids.len().min(TOP);
    let na = asks.len().min(TOP);
    let mut parts: Vec<&str> = Vec::with_capacity((nb + na) * 2);
    for i in 0..nb.max(na) {
        if i < nb {
            parts.push(bids[i].0);
            parts.push(bids[i].1);
        }
        if i < na {
            parts.push(asks[i].0);
            parts.push(asks[i].1);
        }
    }
    parts.join(":")
}

/// Total-ordering key over an f64 price (via [`f64::total_cmp`]) so the merged mirror can be a
/// `BTreeMap` — used ONLY for level ordering/identity; the bytes fed to the CRC are always the raw
/// price/size strings stored alongside, never this float. OKX emits each price level with one
/// canonical string, so distinct wire prices map to distinct keys.
#[derive(Clone, Copy)]
struct PriceKey(f64);
impl PartialEq for PriceKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.total_cmp(&other.0) == Ordering::Equal
    }
}
impl Eq for PriceKey {}
impl PartialOrd for PriceKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for PriceKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// A size string that means "remove this level" — OKX signals removal with a `"0"` quantity.
fn is_removal(size: &str) -> bool {
    size.parse::<f64>().map(|v| v == 0.0).unwrap_or(false)
}

/// A raw-string mirror of the merged OKX order book, kept in lockstep with the folded
/// [`vike_model::L2Book`] purely so the CRC32 can be computed over OKX's exact wire strings (see the
/// module doc for why the f64 book cannot be used). Bids/asks are keyed by numeric price for ordering;
/// each value is the `(price, size)` pair EXACTLY as OKX sent it. A `size` of `0` removes the level.
#[derive(Default)]
pub struct ChecksumBook {
    /// price → (price_str, size_str); best bid is the LAST key (iterate `.rev()`).
    bids: BTreeMap<PriceKey, (String, String)>,
    /// price → (price_str, size_str); best ask is the FIRST key.
    asks: BTreeMap<PriceKey, (String, String)>,
}

impl ChecksumBook {
    pub fn new() -> Self {
        Self::default()
    }

    fn fold(side: &mut BTreeMap<PriceKey, (String, String)>, levels: &[(String, String)]) {
        for (px, sz) in levels {
            let Ok(p) = px.parse::<f64>() else { continue };
            if is_removal(sz) {
                side.remove(&PriceKey(p));
            } else {
                side.insert(PriceKey(p), (px.clone(), sz.clone()));
            }
        }
    }

    /// Rebuild the mirror from a full snapshot (clears both sides first) — mirrors
    /// [`vike_model::L2Book::apply_snapshot`]. Called on every OKX `action:"snapshot"`, so it also
    /// re-syncs the mirror after a reconnect (OKX always re-sends a snapshot first).
    pub fn apply_snapshot(&mut self, bids: &[(String, String)], asks: &[(String, String)]) {
        self.bids.clear();
        self.asks.clear();
        Self::fold(&mut self.bids, bids);
        Self::fold(&mut self.asks, asks);
    }

    /// Merge an incremental delta into the mirror (upsert; `size == 0` removes) — mirrors
    /// [`vike_model::L2Book::apply_delta`]. Only call when the delta was actually applied to the f64
    /// book, so the two stay in lockstep.
    pub fn apply_delta(&mut self, bids: &[(String, String)], asks: &[(String, String)]) {
        Self::fold(&mut self.bids, bids);
        Self::fold(&mut self.asks, asks);
    }

    /// The CRC32 of the current merged book, as the signed i32 OKX transmits.
    pub fn computed_checksum(&self) -> i32 {
        let bids: Vec<(&str, &str)> =
            self.bids.iter().rev().take(TOP).map(|(_, (p, s))| (p.as_str(), s.as_str())).collect();
        let asks: Vec<(&str, &str)> =
            self.asks.iter().take(TOP).map(|(_, (p, s))| (p.as_str(), s.as_str())).collect();
        crc32(okx_checksum_string(&bids, &asks).as_bytes()) as i32
    }

    /// Does the merged book's CRC32 equal OKX's `expected` checksum?
    pub fn verify(&self, expected: i32) -> bool {
        self.computed_checksum() == expected
    }

    #[cfg(test)]
    fn bid_len(&self) -> usize {
        self.bids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(levels: &[(&str, &str)]) -> Vec<(String, String)> {
        levels.iter().map(|(p, s)| (p.to_string(), s.to_string())).collect()
    }

    // ---- CRC32 primitive: anchored to the externally-published check value ----

    /// The canonical CRC-32/ISO-HDLC check value: `CRC32("123456789") == 0xCBF43926`. This is the
    /// external ground truth that proves `crc32` is the exact CRC OKX (and zlib/PNG) use.
    #[test]
    fn crc32_matches_the_canonical_check_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn crc32_of_empty_is_zero() {
        assert_eq!(crc32(b""), 0);
    }

    /// A second independent known vector (the classic pangram), for good measure.
    #[test]
    fn crc32_of_the_quick_brown_fox() {
        assert_eq!(crc32(b"The quick brown fox jumps over the lazy dog"), 0x414F_A339);
    }

    // ---- interleave string: pinned to OKX's documented bid-first, per-rank format ----

    /// OKX interleaves the two sides rank-by-rank, bid THEN ask at each rank, each level as
    /// `price:size`, joined by `:`. Pinning the exact string against the documented rule.
    #[test]
    fn checksum_string_interleaves_bid_then_ask_per_rank() {
        let bids = [("100.5", "2"), ("100.4", "3")]; // high→low
        let asks = [("100.6", "4"), ("100.7", "5")]; // low→high
        assert_eq!(okx_checksum_string(&bids, &asks), "100.5:2:100.6:4:100.4:3:100.7:5");
    }

    /// When one side is shorter, it stops contributing at its last rank and the longer side's
    /// remaining levels are appended in order (OKX's asymmetric rule).
    #[test]
    fn checksum_string_handles_asymmetric_sides() {
        // more bids than asks
        let bids = [("100.0", "1"), ("99.0", "2"), ("98.0", "3")];
        let asks = [("101.0", "9")];
        assert_eq!(okx_checksum_string(&bids, &asks), "100.0:1:101.0:9:99.0:2:98.0:3");
        // more asks than bids
        let bids = [("100.0", "1")];
        let asks = [("101.0", "9"), ("102.0", "8")];
        assert_eq!(okx_checksum_string(&bids, &asks), "100.0:1:101.0:9:102.0:8");
    }

    /// Only the best 25 per side enter the string, even when the book is deeper.
    #[test]
    fn checksum_string_caps_at_twenty_five_per_side() {
        let bids: Vec<(String, String)> =
            (0..30).map(|i| (format!("{}", 100 - i), "1".to_string())).collect();
        let refs: Vec<(&str, &str)> = bids.iter().map(|(p, s)| (p.as_str(), s.as_str())).collect();
        let s = okx_checksum_string(&refs, &[]);
        // 25 levels × "price:size" = 50 colon-separated tokens.
        assert_eq!(s.split(':').count(), 50);
        assert!(s.starts_with("100:1:"), "best bid first: {s}");
        assert!(s.ends_with(":76:1"), "25th bid (100-24=76) last, 26th (75) dropped: {s}");
    }

    // ---- ChecksumBook: merged raw-string book, known-good and corruption cases ----

    /// A known-good book validates clean against its own freshly-computed checksum, and any OTHER
    /// value (a wrong/corrupted checksum) is rejected.
    #[test]
    fn a_known_good_book_validates_and_a_wrong_checksum_is_rejected() {
        let mut book = ChecksumBook::new();
        book.apply_snapshot(
            &owned(&[("100.0", "5"), ("99.0", "3")]),
            &owned(&[("101.0", "4"), ("102.0", "2")]),
        );
        let good = book.computed_checksum();
        assert!(book.verify(good), "the book validates against its own checksum");
        assert!(!book.verify(good.wrapping_add(1)), "a wrong checksum is detected");
    }

    /// A CORRUPTED book (a delta silently changed a resting size) no longer matches the checksum that
    /// was valid for the pre-delta book — the exact class the seq chain cannot catch.
    #[test]
    fn a_corrupted_book_no_longer_matches_the_old_checksum() {
        let mut book = ChecksumBook::new();
        book.apply_snapshot(&owned(&[("100.0", "5")]), &owned(&[("101.0", "4")]));
        let before = book.computed_checksum();
        // a delta bumps the best-bid size 5 → 8: the book is now different, the old checksum is stale.
        book.apply_delta(&owned(&[("100.0", "8")]), &[]);
        assert!(!book.verify(before), "the mutated book must not match the old checksum");
        assert!(book.verify(book.computed_checksum()), "but matches its own current checksum");
    }

    /// A `size == 0` delta REMOVES the level (OKX's removal convention), and the checksum reflects the
    /// smaller book.
    #[test]
    fn a_zero_size_delta_removes_the_level() {
        let mut book = ChecksumBook::new();
        book.apply_snapshot(&owned(&[("100.0", "5"), ("99.0", "3")]), &owned(&[("101.0", "4")]));
        assert_eq!(book.bid_len(), 2);
        book.apply_delta(&owned(&[("99.0", "0")]), &[]);
        assert_eq!(book.bid_len(), 1, "the 99.0 bid was removed");
        // equals a book that never had the 99.0 level
        let mut reference = ChecksumBook::new();
        reference.apply_snapshot(&owned(&[("100.0", "5")]), &owned(&[("101.0", "4")]));
        assert_eq!(book.computed_checksum(), reference.computed_checksum());
    }

    /// The CRC is over OKX's RAW strings, not reparsed floats: a `"0.10000000"` size and a `"60000.0"`
    /// price must appear verbatim in the checksum string (a f64 round-trip would collapse them to
    /// `"0.1"`/`"60000"` and never match OKX).
    #[test]
    fn raw_price_and_size_strings_are_preserved_verbatim() {
        let mut book = ChecksumBook::new();
        book.apply_snapshot(&owned(&[("60000.0", "0.10000000")]), &owned(&[("60001.0", "1.5")]));
        let bids: Vec<(&str, &str)> =
            book.bids.iter().rev().map(|(_, (p, s))| (p.as_str(), s.as_str())).collect();
        let asks: Vec<(&str, &str)> =
            book.asks.iter().map(|(_, (p, s))| (p.as_str(), s.as_str())).collect();
        assert_eq!(okx_checksum_string(&bids, &asks), "60000.0:0.10000000:60001.0:1.5");
    }

    /// Deltas can arrive in any order; the mirror always re-sorts, so the checksum is stable
    /// regardless of arrival order (best bid first, best ask first).
    #[test]
    fn levels_are_resorted_so_arrival_order_does_not_matter() {
        let mut a = ChecksumBook::new();
        a.apply_snapshot(&owned(&[("100.0", "5")]), &owned(&[("101.0", "4")]));
        a.apply_delta(&owned(&[("99.0", "3"), ("100.5", "2")]), &[]); // out-of-order inserts
        let mut b = ChecksumBook::new();
        b.apply_snapshot(
            &owned(&[("100.5", "2"), ("100.0", "5"), ("99.0", "3")]),
            &owned(&[("101.0", "4")]),
        );
        assert_eq!(a.computed_checksum(), b.computed_checksum());
    }
}
