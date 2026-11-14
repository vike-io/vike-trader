//! Session/uuid-prefixed order_client_id generator. Exact port of `exec/coid.py`.
//!
//! A bare counter resets on restart and could collide with a still-open prior-session order id;
//! a per-session prefix avoids that. The id is the STRICTEST common denominator across venues —
//! ALPHANUMERIC only, <=32 chars — so the SAME id is valid as Binance `newClientOrderId`,
//! Bybit `orderLinkId`, AND OKX `clOrdId` (OKX rejects non-alphanumeric with "Parameter clOrdId
//! error" and caps at 32). No separator: `<8-hex-session><seq>` (fixed-width session prefix keeps
//! concatenation unambiguous).
//!
//! # Why this lives in vike-model and not in vike-execution
//!
//! It USED to live in `vike-execution`, which re-exported it for a while; that alias is gone, and every
//! caller names it here. It came DOWN for two reasons that were always true and one that finally
//! bit:
//!
//! - `order_client_id` is a FIELD of [`crate::OrderRequest`], and [`is_valid_crypto_order_client_id`] is a
//!   venue CHARSET rule about that field — a pure domain fact, no I/O, no engine, exactly this
//!   crate's charter.
//! - The module has zero dependencies beyond `std`, so nothing about it ever needed the exec layer.
//! - And the thing that bit: `vike-cli` — the deliberately LIGHT, DataFusion-free, transport-free
//!   command surface — must PRE-MINT an order client id for every order it sends to a `vike-tradehub` node
//!   (the node's `lower_command` REFUSES a remote submit with an empty one, so before this the
//!   `trade` REPL could not place an order at all). Reaching for the generator through vike-execution
//!   would have dragged tokio (`rt-multi-thread`), `core_affinity`, `indexmap` and `ustr` into that
//!   CLI, and duplicating a 15-line mint would have forked the id FORMAT between the live core and
//!   the remote surfaces — the drift this workspace spends whole gates preventing. vike-model was
//!   already in the CLI's graph, so the move costs zero packages and leaves ONE generator.
//!
//! # Version 2
//!
//! A session string carrying the `W` mark (`[<tag>]W<base>`, [`OrderClientIdGenerator::new_v2`])
//! mints the v2 ids of `docs/decisions/0270-client-order-ids-carry-source-strategy-account-intent-and-group.md`
//! through a [`OrderClientIdPlan`]: source, strategy, account, a fixed-width unique part, intent, leg role and
//! replacement. Every other session is v1 and mints exactly the bytes above, through the plan too.
//! The v2 vocabulary (the plan, the codes, [`parse_order_client_id`] and [`describe_order_client_id`]) is a private child
//! module whose names are this module's.

mod format;
pub mod origin;

#[cfg(any(test, feature = "test-support"))]
pub use format::order_client_id_readings;
pub use format::{
    FULL_U_WIDTH, MAX_GENERATION, MAX_ROLE_NUMBER, OrderClientIdAccountCode, OrderClientIdIntent,
    OrderClientIdLabels, OrderClientIdLegRole, OrderClientIdParts, OrderClientIdPlan,
    OrderClientIdReading, OrderClientIdShape, OrderClientIdSource, OrderClientIdStem,
    OrderClientIdStrategyCode, OrderClientIdWords, U_WIDTHS, describe_order_client_id,
    next_generation, order_client_id_roll_point, parse_order_client_id, write_base36,
};

/// The keys the order client id was sent under before the owner's 2026-11-11 rename, with the key
/// each is now (`old`, `new`). Built from parts so the scripted renames leave the old spelling alone.
pub const RETIRED_ORDER_CLIENT_ID_KEYS: [(&str, &str); 2] = [
    (concat!("client", "_order_id"), "order_client_id"),
    (concat!("client", "_order_ids"), "order_client_ids"),
];

/// The one-line refusal for a JSON request `body` that still names a retired order client id key AS A
/// KEY ([`RETIRED_ORDER_CLIENT_ID_KEYS`]), else `None`. Every request type keeps `#[serde(default)]` on
/// its optional id, so decoding such a body would DROP the id and mint a new one: a retried order would be
/// placed twice. A door calls this before it decodes; a body without the old word costs one scan.
pub fn retired_order_client_id_refusal(body: &[u8]) -> Option<String> {
    let needle = RETIRED_ORDER_CLIENT_ID_KEYS[0].0.as_bytes();
    if !body.windows(needle.len()).any(|w| w == needle) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    retired_order_client_id_key(&value)
}

/// [`retired_order_client_id_refusal`] over an already-parsed request (an MCP tool's arguments).
pub fn retired_order_client_id_key(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => map.iter().find_map(|(key, inner)| {
            RETIRED_ORDER_CLIENT_ID_KEYS
                .iter()
                .find(|(old, _)| old == key)
                .map(|(old, new)| {
                    format!("`{old}` is now `{new}`: send the order client id under its new key")
                })
                .or_else(|| retired_order_client_id_key(inner))
        }),
        serde_json::Value::Array(items) => items.iter().find_map(retired_order_client_id_key),
        _ => None,
    }
}

/// `^[A-Za-z0-9]{1,32}$` — valid on Binance + Bybit + OKX.
pub fn is_valid_crypto_order_client_id(order_client_id: &str) -> bool {
    !order_client_id.is_empty()
        && order_client_id.len() <= 32
        && order_client_id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Renders `n` as ASCII decimal digits right-to-left into `buffer`, returning just the digit slice.
///
/// THE one hand-rolled decimal render behind the order client id mint, this module's `String`
/// [`OrderClientIdGenerator::generate`]. `format!`'s `fmt::Display` machinery is deliberately
/// avoided here: the caller mints on the order hot path. It stays byte-gated against `format!`
/// (`generate_is_wire_identical_to_format`).
///
/// It is `pub` because vike-execution's integer-key order client id spike (`order_client_id_u64`, deleted) once
/// shared it; nothing outside this module calls it now.
///
/// `buffer` is 20 bytes because `u64::MAX` is 20 decimal digits — the widest render possible, so the
/// walk can never underflow past the start.
#[inline]
pub fn write_decimal(n: u64, buffer: &mut [u8; 20]) -> &[u8] {
    let mut i = buffer.len();
    let mut n = n;
    loop {
        i -= 1;
        buffer[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    &buffer[i..]
}

/// Generates `<8-hex-session><n>` ids — unique across sessions + alphanumeric (valid on all venues).
/// The sequence starts at 0 (Python `itertools.count()`).
///
/// A `W`-marked session string makes it a v2 generator (see the module doc): the session's
/// version is read OFF the string by every constructor, so a journal's `order_client_id_session` alone says
/// which ids a resumed generator mints.
pub struct OrderClientIdGenerator {
    session: String,
    sequence: u64,
    /// A v2 session's `(base, tag prefix length)`; `None` for a v1 session.
    v2: Option<(u64, usize)>,
}

impl OrderClientIdGenerator {
    /// `session: None` → derive a fresh random 8-hex prefix (live). Tests/fixtures pass a
    /// fixed session for determinism (`Date/rand` are banned in workflow scripts; the live
    /// prefix comes from the runtime at R5b wiring).
    pub fn new(session: Option<&str>) -> Self {
        OrderClientIdGenerator::resume(resolve_session(session), 0)
    }

    /// A fresh v2 session (`[<tag>]W<base>`): `base` is random below `36^12 - 2^40`, so the unique
    /// part never wraps for `seq < 2^40`. `origin` stamps the instance tag; a client passes `None`.
    pub fn new_v2(origin: Option<&crate::orders::client_id::origin::OrderClientIdOrigin>) -> Self {
        let mut buffer = [0u8; 8];
        getrandom_fill(&mut buffer);
        let base = u64::from_le_bytes(buffer) % format::V2_BASE_LIMIT;
        OrderClientIdGenerator::resume(format::v2_session(origin, base), 0)
    }

    /// Whether this session mints v2 ids.
    pub fn is_v2(&self) -> bool {
        self.v2.is_some()
    }

    /// Whether a boot should switch to a fresh v2 session: a v1 session always (the one-time
    /// switch), a v2 one once its sequence reaches `roll_point` ([`order_client_id_roll_point`]).
    pub fn due_to_roll(&self, roll_point: u64) -> bool {
        self.v2.is_none() || self.sequence >= roll_point
    }

    /// A v2 session's `(base, tag prefix length)`.
    pub(super) fn v2_state(&self) -> Option<(u64, usize)> {
        self.v2
    }

    pub(super) fn session_str(&self) -> &str {
        &self.session
    }

    /// The sequence number of the id being minted, advancing the counter.
    pub(super) fn take_sequence(&mut self) -> u64 {
        let seq = self.sequence;
        self.sequence += 1;
        seq
    }

    /// The instance-origin twin of [`new`](Self::new): a fresh generator whose session carries an
    /// [`crate::orders::client_id::origin::OrderClientIdOrigin`] claim, so every id it mints is recognisable as
    /// THIS instance's on a venue that echoes the id back
    /// (`crates/vike-venues/src/venue_order_client_id_budget.rs` says which do).
    ///
    /// `origin: None` is BYTE-IDENTICAL to `new(session)` — the whole feature is off unless an
    /// operator declares a tag, and that identity is what
    /// `an_unconfigured_instance_mints_exactly_the_ids_it_always_did` pins.
    ///
    /// The claim rides INSIDE the session string rather than as a fourth field, which is what
    /// makes [`state`](Self::state)/[`resume`](Self::resume) — and therefore the journal's
    /// `order_client_id_session` and the whole replay determinism fence — carry it with no schema change at
    /// all. `crates/vike-model/src/orders/client_id/origin.rs`'s module doc argues the format.
    pub fn with_origin(
        origin: Option<&crate::orders::client_id::origin::OrderClientIdOrigin>,
        session: Option<&str>,
    ) -> Self {
        let hex = resolve_session(session);
        let session = match origin {
            Some(o) => o.session_prefix(&hex),
            None => hex,
        };
        OrderClientIdGenerator::resume(session, 0)
    }

    /// The origin claim this generator's ids carry, read back out of the session it is minting
    /// under — `None` for an untagged session, including every session RESUMED from a journal
    /// written before an origin was configured.
    ///
    /// It parses rather than remembering, so a resumed generator answers about the ids it is
    /// ACTUALLY minting instead of about the configuration it was started with. That difference is
    /// the one an operator has to be told about: a restart resumes `order_client_id_session` verbatim
    /// (replay determinism), so a newly-configured origin does not appear on the wire until a
    /// fresh session — `crates/vike-core/src/runtime/assemble.rs`'s `assemble_core` warns when the two
    /// disagree, and `docs/ops/double-live-instances.md` says what to do about it.
    pub fn origin(&self) -> Option<&str> {
        crate::orders::client_id::origin::origin_of_order_client_id(&self.session)
    }

    /// Resume a generator from a persisted `(session, seq)` — journal-replay / restart continuity
    /// (spec 2026-07-10 §A). The next [`generate`](Self::generate) yields `<session><seq>`, so ids
    /// pick up exactly where the pre-restart session left off and never collide with a still-open
    /// prior order id. The inverse of [`state`](Self::state): `resume(x.state())` reproduces `x`.
    pub fn resume(session: String, seq: u64) -> Self {
        let v2 = format::decode_v2_session(&session);
        OrderClientIdGenerator { session, sequence: seq, v2 }
    }

    /// The current `(session, seq)` for persistence into a journal `Snapshot` record. `seq` is the
    /// NEXT id's counter (what [`generate`](Self::generate) will use next), so restoring via
    /// [`resume`](Self::resume) is an exact continuation.
    pub fn state(&self) -> (String, u64) {
        (self.session.clone(), self.sequence)
    }

    /// The v1 mint, `<session><seq>`: what [`OrderClientIdPlan::mint`] calls for a v1 session. On a v2
    /// session it is what an older build resuming the session mints (unique, read as untagged);
    /// a v2 id comes from a plan.
    pub fn generate(&mut self) -> String {
        let seq = self.sequence;
        self.sequence += 1;
        // Build "<session><seq>" in ONE right-sized allocation — no `format!` fmt::Display
        // machinery (which starts from an empty String and grows-reallocates) on the tick hot
        // path. Wire bytes stay byte-identical to `format!("{session}{seq}")`, pinned by
        // `generate_is_wire_identical_to_format`, so the r5 golden and every venue's wire are
        // unchanged. NOTE: the fuller "key orders off the integer, stringify only at the venue
        // edge" idea is deliberately NOT pursued — order_client_id is a String across the whole
        // ported wire schema (events, OrderRequest, registry, exec_db); rewriting that for one
        // small alloc/order is the wrong trade. Revisit only if profiling shows order client id alloc is hot.
        let mut scratch = [0u8; 20];
        let digits = write_decimal(seq, &mut scratch);
        let mut order_client_id = String::with_capacity(self.session.len() + digits.len());
        order_client_id.push_str(&self.session);
        order_client_id.push_str(std::str::from_utf8(digits).expect("ASCII digits"));
        assert!(
            is_valid_crypto_order_client_id(&order_client_id),
            "order_client_id violates crypto charset: {order_client_id:?}"
        );
        order_client_id
    }
}

/// The session half: a caller-supplied one when non-empty, otherwise a fresh
/// [`crate::orders::client_id::origin::SESSION_HEX_LEN`]-character random prefix. Split out of
/// [`OrderClientIdGenerator::new`] so the origin-aware constructor derives the SAME random half
/// rather than a second copy of the derivation.
fn resolve_session(session: Option<&str>) -> String {
    match session {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            // uuid4().hex[:8] equivalent from OS entropy, without a uuid dependency
            let mut buffer = [0u8; 4];
            getrandom_fill(&mut buffer);
            vike_utility::hex::hex_lower(&buffer)
        }
    }
}

/// Minimal OS-entropy fill (std-only; RtlGenRandom/getrandom would add a dep for 4 bytes).
/// Hashes ASLR'd addresses + thread id — NOT crypto-grade, but the prefix only needs to
/// differ across process restarts (same bar as uuid4's collision use here).
fn getrandom_fill(buffer: &mut [u8]) {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut h = RandomState::new().build_hasher(); // seeded from OS entropy per-process
    h.write_u64(std::process::id() as u64);
    let bits = h.finish().to_le_bytes();
    buffer.copy_from_slice(&bits[..buffer.len()]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A retired key is found as a KEY at any depth; the same word as a VALUE, or no word at all, is not.
    #[test]
    fn a_retired_order_client_id_key_is_refused_at_any_depth_and_only_as_a_key() {
        let (old, new) = RETIRED_ORDER_CLIENT_ID_KEYS[1];
        let nested =
            format!(r#"{{"Preview":{{"OrderList":{{"legs":[{{"qty":1.0,"{old}":["a"]}}]}}}}}}"#);
        assert_eq!(
            retired_order_client_id_refusal(nested.as_bytes()).as_deref(),
            Some(
                format!("`{old}` is now `{new}`: send the order client id under its new key")
                    .as_str()
            )
        );
        let as_value = format!(r#"{{"reason":"{old}"}}"#);
        assert_eq!(retired_order_client_id_refusal(as_value.as_bytes()), None, "a value is no key");
        assert_eq!(retired_order_client_id_refusal(br#"{"order_client_id":"c1"}"#), None);
    }

    #[test]
    fn generate_is_wire_identical_to_format() {
        // THE parity gate for piece 4: the hand-rolled mint must produce byte-identical ids to the
        // old `format!("{session}{seq}")` across a wide sequence range and several sessions. This
        // is what keeps r5 (and every venue's on-wire clOrdId) unchanged.
        for session in ["deadbeef", "00000000", "a1b2c3d4", "ffffffff"] {
            let mut m = OrderClientIdGenerator::new(Some(session));
            for seq in 0..5000u64 {
                assert_eq!(m.generate(), format!("{session}{seq}"), "seq {seq}");
            }
        }
    }

    #[test]
    fn generate_starts_at_zero_and_increments() {
        let mut m = OrderClientIdGenerator::new(Some("sess1234"));
        assert_eq!(m.generate(), "sess12340");
        assert_eq!(m.generate(), "sess12341");
        assert_eq!(m.generate(), "sess12342");
    }

    #[test]
    fn generated_ids_stay_valid_crypto_order_client_ids() {
        let mut m = OrderClientIdGenerator::new(Some("deadbeef"));
        for _ in 0..1000 {
            assert!(is_valid_crypto_order_client_id(&m.generate()));
        }
    }

    #[test]
    fn resume_continues_the_sequence_without_collision() {
        // A pre-restart session minted deadbeef0..deadbeef2; journal-replay persists (session, seq)
        // and resume() picks up EXACTLY at the next id — never re-minting a still-open prior id.
        let mut pre = OrderClientIdGenerator::new(Some("deadbeef"));
        assert_eq!(pre.generate(), "deadbeef0");
        assert_eq!(pre.generate(), "deadbeef1");
        assert_eq!(pre.generate(), "deadbeef2");
        let (session, seq) = pre.state();
        assert_eq!((session.as_str(), seq), ("deadbeef", 3));

        let mut resumed = OrderClientIdGenerator::resume(session, seq);
        assert_eq!(resumed.generate(), "deadbeef3", "resume continues, no id reuse");
        assert_eq!(resumed.generate(), "deadbeef4");
    }

    /// THE distinguishability property: two instances minting concurrently produce ids that say
    /// which is which, and each instance's ids keep saying it across a restart.
    ///
    /// The restart half is what makes the tag more than noise, and it is asserted through the
    /// REAL restart path — `state()` into `resume()`, which is what the journal's `order_client_id_session`
    /// does — rather than by re-constructing with the same origin.
    #[test]
    fn two_instances_mint_distinguishable_ids_that_survive_a_restart() {
        use crate::orders::client_id::origin::OrderClientIdOrigin;
        let west = OrderClientIdOrigin::parse("west").unwrap();
        let east = OrderClientIdOrigin::parse("east").unwrap();

        let mut a = OrderClientIdGenerator::with_origin(Some(&west), Some("deadbeef"));
        let mut b = OrderClientIdGenerator::with_origin(Some(&east), Some("deadbeef"));
        // Same session hex on purpose: WITHOUT the tag these two would mint identical ids, which
        // is exactly the duplicate-order-client-id failure the journal lock prevents on one machine and
        // cannot prevent across two.
        let (first_a, first_b) = (a.generate(), b.generate());
        assert_eq!(first_a, "westVdeadbeef0");
        assert_eq!(first_b, "eastVdeadbeef0");
        assert_ne!(first_a, first_b, "two instances must never mint the same id");
        assert_eq!(
            crate::orders::client_id::origin::origin_of_order_client_id(&first_a),
            Some("west")
        );
        assert_eq!(
            crate::orders::client_id::origin::origin_of_order_client_id(&first_b),
            Some("east")
        );

        // ...and a restart of A (journal `Snapshot` -> `resume`) keeps A's claim and A's sequence.
        let (session, seq) = a.state();
        let mut restarted = OrderClientIdGenerator::resume(session, seq);
        assert_eq!(restarted.origin(), Some("west"), "the claim survives a restart");
        assert_eq!(restarted.generate(), "westVdeadbeef1", "and so does the sequence");
    }

    /// An instance with no origin configured mints exactly the ids it always did — asserted
    /// against the OTHER constructor rather than against a literal, so the byte-identity claim is
    /// about the two code paths and not about one hand-written string.
    #[test]
    fn an_unconfigured_instance_mints_exactly_the_ids_it_always_did() {
        let mut plain = OrderClientIdGenerator::new(Some("deadbeef"));
        let mut untagged = OrderClientIdGenerator::with_origin(None, Some("deadbeef"));
        for _ in 0..64 {
            assert_eq!(plain.generate(), untagged.generate());
        }
        assert_eq!(plain.state(), untagged.state());
        assert_eq!(untagged.origin(), None);
    }

    /// A tagged id is still a legal order client id on every venue that constrains one — the
    /// charset rule this module has enforced since it was ported.
    #[test]
    fn tagged_ids_stay_valid_crypto_order_client_ids() {
        let o = crate::orders::client_id::origin::OrderClientIdOrigin::parse("wxyz").unwrap();
        let mut m = OrderClientIdGenerator::with_origin(Some(&o), None);
        for _ in 0..1000 {
            let id = m.generate();
            assert!(is_valid_crypto_order_client_id(&id), "{id}");
        }
    }

    #[test]
    fn state_round_trips_through_resume() {
        let mut m = OrderClientIdGenerator::new(Some("a1b2c3d4"));
        for _ in 0..17 {
            m.generate();
        }
        let (session, seq) = m.state();
        // resume(state()) reproduces the generator: same next id from both.
        let mut clone = OrderClientIdGenerator::resume(session, seq);
        assert_eq!(clone.generate(), m.generate());
    }
}
