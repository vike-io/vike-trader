//! PROTOTYPE (strong-types spike) — integer-keyed client order id.
//!
//! The trillion-scale ceiling for `client_order_id`: instead of a `String` on the hot path, carry
//! a 16-byte `Copy` integer identity everywhere (Event field, FSM/registry key, dedup key) and
//! materialize the on-wire `<8hex-session><seq>` string ONLY at the venue REST/WS boundary
//! (`write_wire`) and parse it back ONLY when a venue echoes it on an exec report (`parse_wire`).
//!
//! The live `ClientOrderIdGenerator` prefix is `uuid4().hex[:8]` — 8 hex chars = 32 bits — so the
//! session packs losslessly into a `u32`, and `seq` is the existing `u64` counter. That makes the
//! id self-describing: it renders to / parses from the exact wire bytes with no generator in
//! scope, so serde stays byte-identical to the frozen `fixtures/r5/coid.json` bytes — see the gate
//! tests. Those bytes were exported from the Python app before its retirement and ARE the oracle
//! now (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`): the gate replays
//! them, it does not re-derive anything from Python, and the comparison stays exact because a
//! frozen fixture is a claim about THIS code not changing its wire bytes unnoticed.
//!
//! Scope note: this models the LIVE generator (hex session). The String generator also accepts
//! non-hex test sessions (e.g. `"sess1234"`); those never occur live and are out of scope here.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// 16-byte `Copy` order identity. Replaces `client_order_id: String` on the hot path.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct ClientOrderId {
    /// 8-hex live session prefix (`0xdeadbeef` ⇄ `"deadbeef"`), packed.
    session: u32,
    /// Per-session counter (`itertools.count()` twin).
    seq: u64,
}

impl ClientOrderId {
    pub fn new(session: u32, seq: u64) -> Self {
        Self { session, seq }
    }
    pub fn session(self) -> u32 {
        self.session
    }
    pub fn seq(self) -> u64 {
        self.seq
    }

    /// Zero-alloc render into a caller-owned stack buffer; returns the wire `&str`.
    /// Byte-identical to the old `format!("{session}{seq}")` (8 hex, zero-padded, + decimal seq).
    /// Max width = 8 hex + 20 decimal digits (`u64::MAX`) = 28 bytes.
    pub fn write_wire<'a>(&self, buf: &'a mut [u8; 28]) -> &'a str {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for (i, slot) in buf[..8].iter_mut().enumerate() {
            *slot = HEX[((self.session >> ((7 - i) * 4)) & 0xf) as usize];
        }
        // decimal seq into a scratch, then append — the SAME render the String twin mints with
        // (`client_order_id::write_decimal`), so the two can't drift apart byte-wise.
        let mut tmp = [0u8; 20];
        let digits = crate::client_order_id::write_decimal(self.seq, &mut tmp);
        buf[8..8 + digits.len()].copy_from_slice(digits);
        let len = 8 + digits.len();
        std::str::from_utf8(&buf[..len]).expect("ASCII")
    }

    /// Recover the integer key from a venue-echoed wire clOrdId. `None` = not one of ours
    /// (foreign/manual/other-session order) → reconcile keeps the raw string on that path.
    pub fn parse_wire(wire: &str) -> Option<Self> {
        if wire.len() < 9 {
            return None; // 8 hex + ≥1 digit
        }
        let (sess, seq) = wire.split_at(8);
        if !sess.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(Self { session: u32::from_str_radix(sess, 16).ok()?, seq: seq.parse().ok()? })
    }
}

impl fmt::Display for ClientOrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}{}", self.session, self.seq)
    }
}

/// Serializes AS the wire string (`"deadbeef0"`), so fixtures/journal/exec_db JSON is unchanged.
impl Serialize for ClientOrderId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}
impl<'de> Deserialize<'de> for ClientOrderId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Self::parse_wire(&w)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid client_order_id: {w:?}")))
    }
}

/// Zero-alloc twin of [`super::client_order_id::ClientOrderIdGenerator`] — hands out integer keys.
pub struct ClientOrderIdGeneratorU64 {
    session: u32,
    seq: u64,
}

impl ClientOrderIdGeneratorU64 {
    pub fn new(session: u32) -> Self {
        Self { session, seq: 0 }
    }
    /// Build from the same 8-hex session string the String generator uses (parity harness).
    pub fn from_hex_session(session: &str) -> Option<Self> {
        Some(Self { session: u32::from_str_radix(session, 16).ok()?, seq: 0 })
    }
    /// O(1), zero allocation.
    pub fn generate(&mut self) -> ClientOrderId {
        let id = ClientOrderId { session: self.session, seq: self.seq };
        self.seq += 1;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE gate: the integer id's wire form must be byte-identical to the old
    /// `format!("{session}{seq}")` across the live (hex) sessions + a wide seq range — the same
    /// contract `client_order_id::generate_is_wire_identical_to_format` pins for the String twin.
    #[test]
    fn wire_is_byte_identical_to_format() {
        for session in ["deadbeef", "00000000", "a1b2c3d4", "ffffffff"] {
            let mut g = ClientOrderIdGeneratorU64::from_hex_session(session).unwrap();
            let mut buf = [0u8; 28];
            for seq in 0..5000u64 {
                let id = g.generate();
                assert_eq!(id.write_wire(&mut buf), format!("{session}{seq}"), "seq {seq}");
                // Display must agree with the zero-alloc writer.
                assert_eq!(id.to_string(), format!("{session}{seq}"));
            }
        }
    }

    /// Round-trips through the wire string: parse(render(id)) == id.
    #[test]
    fn parse_wire_roundtrips() {
        let mut g = ClientOrderIdGeneratorU64::from_hex_session("a1b2c3d4").unwrap();
        let mut buf = [0u8; 28];
        for _ in 0..1000 {
            let id = g.generate();
            let wire = id.write_wire(&mut buf).to_owned();
            assert_eq!(ClientOrderId::parse_wire(&wire), Some(id));
        }
    }

    /// Foreign / malformed ids are rejected (→ reconcile keeps the raw string instead).
    #[test]
    fn parse_wire_rejects_foreign() {
        assert_eq!(ClientOrderId::parse_wire(""), None);
        assert_eq!(ClientOrderId::parse_wire("deadbeef"), None); // no seq
        assert_eq!(ClientOrderId::parse_wire("zzzzzzzz0"), None); // non-hex session
        assert_eq!(ClientOrderId::parse_wire("manual-order-1"), None);
    }

    /// serde emits the exact r5 `coid.json` strings (`session="deadbeef"`, seq 0..11) and
    /// round-trips them — so the golden fixture / exec_db JSON is byte-identical.
    #[test]
    fn serde_matches_r5_coid_fixture() {
        let expected = [
            "deadbeef0",
            "deadbeef1",
            "deadbeef2",
            "deadbeef3",
            "deadbeef4",
            "deadbeef5",
            "deadbeef6",
            "deadbeef7",
            "deadbeef8",
            "deadbeef9",
            "deadbeef10",
            "deadbeef11",
        ];
        let mut g = ClientOrderIdGeneratorU64::from_hex_session("deadbeef").unwrap();
        for want in expected {
            let id = g.generate();
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{want}\""));
            let back: ClientOrderId = serde_json::from_str(&json).unwrap();
            assert_eq!(back, id);
        }
    }

    /// The whole point: the id is 16 bytes and `Copy` (vs a 24-byte `String` + heap).
    #[test]
    fn is_sixteen_byte_copy() {
        assert_eq!(std::mem::size_of::<ClientOrderId>(), 16);
        fn assert_copy<T: Copy>() {}
        assert_copy::<ClientOrderId>();
    }
}
