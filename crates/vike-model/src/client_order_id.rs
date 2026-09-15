//! Session/uuid-prefixed client_order_id generator. Exact port of `exec/coid.py`.
//!
//! A bare counter resets on restart and could collide with a still-open prior-session order id;
//! a per-session prefix avoids that. The id is the STRICTEST common denominator across venues —
//! ALPHANUMERIC only, <=32 chars — so the SAME id is valid as Binance `newClientOrderId`,
//! Bybit `orderLinkId`, AND OKX `clOrdId` (OKX rejects non-alphanumeric with "Parameter clOrdId
//! error" and caps at 32). No separator: `<8-hex-session><seq>` (fixed-width session prefix keeps
//! concatenation unambiguous).
//!
//! # Why this lives in vike-model and not in vike-exec
//!
//! It USED to live in `vike-exec`, and `vike_exec::{ClientOrderIdGenerator, is_valid_crypto_coid}`
//! still resolve — vike-exec re-exports this module verbatim, so no call site moved. It came DOWN
//! for two reasons that were always true and one that finally bit:
//!
//! - `client_order_id` is a FIELD of [`crate::OrderRequest`], and [`is_valid_crypto_coid`] is a
//!   venue CHARSET rule about that field — a pure domain fact, no I/O, no engine, exactly this
//!   crate's charter.
//! - The module has zero dependencies beyond `std`, so nothing about it ever needed the exec layer.
//! - And the thing that bit: `vike-cli` — the deliberately LIGHT, DataFusion-free, transport-free
//!   command surface — must PRE-MINT a coid for every order it sends to a `vike-tradehub` node
//!   (the node's `lower_command` REFUSES a remote submit with an empty one, so before this the
//!   `trade` REPL could not place an order at all). Reaching for the generator through vike-exec
//!   would have dragged tokio (`rt-multi-thread`), `core_affinity`, `indexmap` and `ustr` into that
//!   CLI, and duplicating a 15-line mint would have forked the id FORMAT between the live core and
//!   the remote surfaces — the drift this workspace spends whole gates preventing. vike-model was
//!   already in the CLI's graph, so the move costs zero packages and leaves ONE generator.

/// `^[A-Za-z0-9]{1,32}$` — valid on Binance + Bybit + OKX.
pub fn is_valid_crypto_coid(coid: &str) -> bool {
    !coid.is_empty() && coid.len() <= 32 && coid.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Renders `n` as ASCII decimal digits right-to-left into `buf`, returning just the digit slice.
///
/// THE one hand-rolled decimal render behind both coid mints — this module's `String`
/// [`ClientOrderIdGenerator::generate`] and the integer-key spike's
/// `vike_exec::client_order_id_u64::ClientOrderId::write_wire` — which previously carried
/// byte-identical copies of this loop. `format!`'s `fmt::Display` machinery is deliberately avoided
/// here: both callers mint on the order hot path. Each caller stays independently byte-gated against
/// `format!` (`generate_is_wire_identical_to_format` / `wire_is_byte_identical_to_format`), so this
/// fold is pinned from both sides.
///
/// `pub`, not `pub(crate)`: the second caller is `vike_exec::client_order_id_u64`, which stayed in
/// vike-exec when this module moved down (it is an exec-side spike over an exec-side hot path).
/// Widening one internal helper is the price of keeping ONE copy of the digit fold; the alternative
/// was a second byte-identical loop in the other crate, which is exactly what this function exists
/// to prevent.
///
/// `buf` is 20 bytes because `u64::MAX` is 20 decimal digits — the widest render possible, so the
/// walk can never underflow past the start.
#[inline]
pub fn write_decimal(n: u64, buf: &mut [u8; 20]) -> &[u8] {
    let mut i = buf.len();
    let mut n = n;
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    &buf[i..]
}

/// Generates `<8-hex-session><n>` ids — unique across sessions + alphanumeric (valid on all venues).
/// The sequence starts at 0 (Python `itertools.count()`).
pub struct ClientOrderIdGenerator {
    session: String,
    seq: u64,
}

impl ClientOrderIdGenerator {
    /// `session: None` → derive a fresh random 8-hex prefix (live). Tests/fixtures pass a
    /// fixed session for determinism (`Date/rand` are banned in workflow scripts; the live
    /// prefix comes from the runtime at R5b wiring).
    pub fn new(session: Option<&str>) -> Self {
        ClientOrderIdGenerator { session: resolve_session(session), seq: 0 }
    }

    /// The instance-origin twin of [`new`](Self::new): a fresh generator whose session carries an
    /// [`crate::instance_origin::InstanceOrigin`] claim, so every id it mints is recognisable as
    /// THIS instance's on a venue that echoes the id back
    /// (`crates/vike-model/src/venue_coid_budget.rs` says which do).
    ///
    /// `origin: None` is BYTE-IDENTICAL to `new(session)` — the whole feature is off unless an
    /// operator declares a tag, and that identity is what
    /// `an_unconfigured_instance_mints_exactly_the_ids_it_always_did` pins.
    ///
    /// The claim rides INSIDE the session string rather than as a fourth field, which is what
    /// makes [`state`](Self::state)/[`resume`](Self::resume) — and therefore the journal's
    /// `coid_session` and the whole replay determinism fence — carry it with no schema change at
    /// all. `crates/vike-model/src/instance_origin.rs`'s module doc argues the format.
    pub fn with_origin(
        origin: Option<&crate::instance_origin::InstanceOrigin>,
        session: Option<&str>,
    ) -> Self {
        let hex = resolve_session(session);
        let session = match origin {
            Some(o) => o.session_prefix(&hex),
            None => hex,
        };
        ClientOrderIdGenerator { session, seq: 0 }
    }

    /// The origin claim this generator's ids carry, read back out of the session it is minting
    /// under — `None` for an untagged session, including every session RESUMED from a journal
    /// written before an origin was configured.
    ///
    /// It parses rather than remembering, so a resumed generator answers about the ids it is
    /// ACTUALLY minting instead of about the configuration it was started with. That difference is
    /// the one an operator has to be told about: a restart resumes `coid_session` verbatim
    /// (replay determinism), so a newly-configured origin does not appear on the wire until a
    /// fresh session — `crates/vike-core/src/runtime/mod.rs`'s `assemble_core` warns when the two
    /// disagree, and `docs/ops/double-live-instances.md` says what to do about it.
    pub fn origin(&self) -> Option<&str> {
        crate::instance_origin::origin_of_coid(&self.session)
    }

    /// Resume a generator from a persisted `(session, seq)` — journal-replay / restart continuity
    /// (spec 2026-07-10 §A). The next [`generate`](Self::generate) yields `<session><seq>`, so ids
    /// pick up exactly where the pre-restart session left off and never collide with a still-open
    /// prior order id. The inverse of [`state`](Self::state): `resume(x.state())` reproduces `x`.
    pub fn resume(session: String, seq: u64) -> Self {
        ClientOrderIdGenerator { session, seq }
    }

    /// The current `(session, seq)` for persistence into a journal `Snap` record. `seq` is the
    /// NEXT id's counter (what [`generate`](Self::generate) will use next), so restoring via
    /// [`resume`](Self::resume) is an exact continuation.
    pub fn state(&self) -> (String, u64) {
        (self.session.clone(), self.seq)
    }

    pub fn generate(&mut self) -> String {
        let seq = self.seq;
        self.seq += 1;
        // Build "<session><seq>" in ONE right-sized allocation — no `format!` fmt::Display
        // machinery (which starts from an empty String and grows-reallocates) on the tick hot
        // path. Wire bytes stay byte-identical to `format!("{session}{seq}")`, pinned by
        // `generate_is_wire_identical_to_format`, so the r5 golden and every venue's wire are
        // unchanged. NOTE: the fuller "key orders off the integer, stringify only at the venue
        // edge" idea is deliberately NOT pursued — client_order_id is a String across the whole
        // ported wire schema (events, OrderRequest, registry, exec_db); rewriting that for one
        // small alloc/order is the wrong trade. Revisit only if profiling shows coid alloc is hot.
        let mut scratch = [0u8; 20];
        let digits = write_decimal(seq, &mut scratch);
        let mut coid = String::with_capacity(self.session.len() + digits.len());
        coid.push_str(&self.session);
        coid.push_str(std::str::from_utf8(digits).expect("ASCII digits"));
        assert!(is_valid_crypto_coid(&coid), "client_order_id violates crypto charset: {coid:?}");
        coid
    }
}

/// The session half: a caller-supplied one when non-empty, otherwise a fresh
/// [`crate::instance_origin::SESSION_HEX_LEN`]-character random prefix. Split out of
/// [`ClientOrderIdGenerator::new`] so the origin-aware constructor derives the SAME random half
/// rather than a second copy of the derivation.
fn resolve_session(session: Option<&str>) -> String {
    match session {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            // uuid4().hex[:8] equivalent from OS entropy, without a uuid dependency
            let mut buf = [0u8; 4];
            getrandom_fill(&mut buf);
            buf.iter().map(|b| format!("{b:02x}")).collect()
        }
    }
}

/// Minimal OS-entropy fill (std-only; RtlGenRandom/getrandom would add a dep for 4 bytes).
/// Hashes ASLR'd addresses + thread id — NOT crypto-grade, but the prefix only needs to
/// differ across process restarts (same bar as uuid4's collision use here).
fn getrandom_fill(buf: &mut [u8]) {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut h = RandomState::new().build_hasher(); // seeded from OS entropy per-process
    h.write_u64(std::process::id() as u64);
    let bits = h.finish().to_le_bytes();
    buf.copy_from_slice(&bits[..buf.len()]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_is_wire_identical_to_format() {
        // THE parity gate for piece 4: the hand-rolled mint must produce byte-identical ids to the
        // old `format!("{session}{seq}")` across a wide sequence range and several sessions. This
        // is what keeps r5 (and every venue's on-wire clOrdId) unchanged.
        for session in ["deadbeef", "00000000", "a1b2c3d4", "ffffffff"] {
            let mut m = ClientOrderIdGenerator::new(Some(session));
            for seq in 0..5000u64 {
                assert_eq!(m.generate(), format!("{session}{seq}"), "seq {seq}");
            }
        }
    }

    #[test]
    fn generate_starts_at_zero_and_increments() {
        let mut m = ClientOrderIdGenerator::new(Some("sess1234"));
        assert_eq!(m.generate(), "sess12340");
        assert_eq!(m.generate(), "sess12341");
        assert_eq!(m.generate(), "sess12342");
    }

    #[test]
    fn generated_ids_stay_valid_crypto_coids() {
        let mut m = ClientOrderIdGenerator::new(Some("deadbeef"));
        for _ in 0..1000 {
            assert!(is_valid_crypto_coid(&m.generate()));
        }
    }

    #[test]
    fn resume_continues_the_sequence_without_collision() {
        // A pre-restart session minted deadbeef0..deadbeef2; journal-replay persists (session, seq)
        // and resume() picks up EXACTLY at the next id — never re-minting a still-open prior id.
        let mut pre = ClientOrderIdGenerator::new(Some("deadbeef"));
        assert_eq!(pre.generate(), "deadbeef0");
        assert_eq!(pre.generate(), "deadbeef1");
        assert_eq!(pre.generate(), "deadbeef2");
        let (session, seq) = pre.state();
        assert_eq!((session.as_str(), seq), ("deadbeef", 3));

        let mut resumed = ClientOrderIdGenerator::resume(session, seq);
        assert_eq!(resumed.generate(), "deadbeef3", "resume continues, no id reuse");
        assert_eq!(resumed.generate(), "deadbeef4");
    }

    /// THE distinguishability property: two instances minting concurrently produce ids that say
    /// which is which, and each instance's ids keep saying it across a restart.
    ///
    /// The restart half is what makes the tag more than noise, and it is asserted through the
    /// REAL restart path — `state()` into `resume()`, which is what the journal's `coid_session`
    /// does — rather than by re-constructing with the same origin.
    #[test]
    fn two_instances_mint_distinguishable_ids_that_survive_a_restart() {
        use crate::instance_origin::InstanceOrigin;
        let west = InstanceOrigin::parse("west").unwrap();
        let east = InstanceOrigin::parse("east").unwrap();

        let mut a = ClientOrderIdGenerator::with_origin(Some(&west), Some("deadbeef"));
        let mut b = ClientOrderIdGenerator::with_origin(Some(&east), Some("deadbeef"));
        // Same session hex on purpose: WITHOUT the tag these two would mint identical ids, which
        // is exactly the duplicate-coid failure the journal lock prevents on one machine and
        // cannot prevent across two.
        let (first_a, first_b) = (a.generate(), b.generate());
        assert_eq!(first_a, "westVdeadbeef0");
        assert_eq!(first_b, "eastVdeadbeef0");
        assert_ne!(first_a, first_b, "two instances must never mint the same id");
        assert_eq!(crate::instance_origin::origin_of_coid(&first_a), Some("west"));
        assert_eq!(crate::instance_origin::origin_of_coid(&first_b), Some("east"));

        // ...and a restart of A (journal `Snap` -> `resume`) keeps A's claim and A's sequence.
        let (session, seq) = a.state();
        let mut restarted = ClientOrderIdGenerator::resume(session, seq);
        assert_eq!(restarted.origin(), Some("west"), "the claim survives a restart");
        assert_eq!(restarted.generate(), "westVdeadbeef1", "and so does the sequence");
    }

    /// An instance with no origin configured mints exactly the ids it always did — asserted
    /// against the OTHER constructor rather than against a literal, so the byte-identity claim is
    /// about the two code paths and not about one hand-written string.
    #[test]
    fn an_unconfigured_instance_mints_exactly_the_ids_it_always_did() {
        let mut plain = ClientOrderIdGenerator::new(Some("deadbeef"));
        let mut untagged = ClientOrderIdGenerator::with_origin(None, Some("deadbeef"));
        for _ in 0..64 {
            assert_eq!(plain.generate(), untagged.generate());
        }
        assert_eq!(plain.state(), untagged.state());
        assert_eq!(untagged.origin(), None);
    }

    /// A tagged id is still a legal client order id on every venue that constrains one — the
    /// charset rule this module has enforced since it was ported.
    #[test]
    fn tagged_ids_stay_valid_crypto_coids() {
        let o = crate::instance_origin::InstanceOrigin::parse("wxyz").unwrap();
        let mut m = ClientOrderIdGenerator::with_origin(Some(&o), None);
        for _ in 0..1000 {
            let id = m.generate();
            assert!(is_valid_crypto_coid(&id), "{id}");
        }
    }

    #[test]
    fn state_round_trips_through_resume() {
        let mut m = ClientOrderIdGenerator::new(Some("a1b2c3d4"));
        for _ in 0..17 {
            m.generate();
        }
        let (session, seq) = m.state();
        // resume(state()) reproduces the generator: same next id from both.
        let mut clone = ClientOrderIdGenerator::resume(session, seq);
        assert_eq!(clone.generate(), m.generate());
    }
}
