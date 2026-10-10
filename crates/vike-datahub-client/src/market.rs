//! The MARKET-DATA vocabulary of the datahub wire — the types
//! [`Request::MdSubscribe`](crate::proto::Request::MdSubscribe) /
//! [`Request::MdUpdate`](crate::proto::Request::MdUpdate) and
//! [`Response::MdSubscribed`](crate::proto::Response::MdSubscribed) /
//! [`Response::MdUpdated`](crate::proto::Response::MdUpdated) /
//! [`Response::Md`](crate::proto::Response::Md) carry. Designed in
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §4; every `§` below is
//! that document's.
//!
//! It lives in this crate (layer 25) because the server (`vike-datahub`) and the desktop session
//! (`vike-app-core`) must name ONE vocabulary and neither can see the other: the shared crate BELOW
//! both, the argument that put [`crate::proto::plane_of`] and [`crate::proto::required_scope`] here.
//! It adds no dependency (`vike-data` at DEFAULT features reaches [`vike_data::StreamStatus`] and
//! `require_live_verb`; `rand` is already here for [`vike_node_proto::auth::fresh_nonce`]). Declared
//! ungated: a default `vike-datahub` build must DECODE these verbs in order to refuse them cleanly.
//!
//! # ⚠ The §0 invariant everything else hangs off
//!
//! A datahub connection is POSITIONAL (one request, one reply) until — and unless —
//! [`Request::MdSubscribe`](crate::proto::Request::MdSubscribe) is answered with
//! [`Response::MdSubscribed`](crate::proto::Response::MdSubscribed). Then:
//!
//! 1. `MdSubscribed` is the LAST positional frame that socket will ever carry.
//! 2. After it, EVERY server→client frame is [`Response::Md`](crate::proto::Response::Md), and the
//!    client→server direction is SILENT — the server's writer has left the read loop.
//! 3. The socket ends on [`MdFrame::Bye`] followed by a close, or on a transport fault.
//!
//! Rung 2 is what makes "no correlation id" true: a stream reader may `match` on `Response::Md` and
//! treat anything else as a protocol desync — which is why [`MdFrame::Heartbeat`] is a variant HERE
//! rather than a reuse of `Response::Pong` (as `crates/vike-tradehub/src/server.rs`'s
//! `run_push_writer` does on the ACCOUNT plane).
//!
//! ⚠ **Rung 1's exception:** `MdSubscribe` mode-switches ONLY when answered with `MdSubscribed`. A
//! server answering `Response::Error` (one predating the verb, or built with no market-data plane)
//! has NOT switched, and the client must not start a reader thread — leg 3 of
//! [`crate::proto::FEATURE_MARKET_DATA`]'s contract
//! (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`),
//! guaranteed by `crates/vike-datahub/src/server/connection.rs`'s `handle_connection` framing/decode
//! split.
//!
//! # What is NOT here
//!
//! The CAPS (`MD_MAX_KEYS_TOTAL`, `MD_MAILBOX_CAP`, `MD_LINGER`, …) are the server's own, in
//! `crates/vike-datahub/src/md/`; a cap's client half is the TYPED REFUSAL carrying the number
//! ([`MdRefusal::KeyCapTotal`] and its siblings). ⚠ A VALIDITY rule is not a cap: it never lifts on
//! its own, and both ends compute it LOCALLY, so [`MD_MAX_SYMBOL_BYTES`] and [`validate_md_symbol`]
//! live HERE.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ---- Constants — the ones BOTH ends need ------------------------------------------------------

/// How often a market-data stream connection writes a [`MdFrame::Heartbeat`] when it has nothing
/// else to say, so a quiet subscription proves the link is alive rather than merely silent.
///
/// ⚠ **ADOPTED, not measured:** §12 measured nothing bearing on LINK liveness. It is
/// `vike_tradehub_client::liveness`'s `OBSERVE_HEARTBEAT` (15 s), because the account plane's
/// stream fails the same way — a peer that stops writing looks like one with nothing to write — and
/// one number for both is one thing for an operator to reason about.
pub const MD_HEARTBEAT: Duration = Duration::from_secs(15);

/// The client's read deadline on a market-data stream: three heartbeats.
///
/// ⚠ **A FLOOR, not the contract.** The authoritative deadline is
/// `max(MD_READ_TIMEOUT, 3 × MdSubscribed.heartbeat_ms)`: the server SENDS its heartbeat period in
/// [`Response::MdSubscribed`](crate::proto::Response::MdSubscribed), so this constant never has to
/// be a compile-time agreement between two independently-deployed binaries (the hazard
/// `vike_tradehub_client::liveness`'s `OBSERVE_READ_TIMEOUT`, a constant on BOTH sides, carries).
pub const MD_READ_TIMEOUT: Duration = Duration::from_secs(3 * MD_HEARTBEAT.as_secs());

/// What an [`MdSpec::depth_levels`] of `None` resolves to, per side. **MEASURED** (§12.4): clamped
/// to 50, a binance frame (200 levels a side) shrinks 3.8×, from ~10.5 KB to ~2.7 KB, while
/// polymarket's whole 99-level book is nearly untouched — the deepest lane cut fourfold, the
/// shallowest left alone, which is the property a default wants.
pub const MD_DEPTH_LEVELS_DEFAULT: u16 = 50;

/// The ceiling an [`MdSpec::depth_levels`] request is CLAMPED to — §4.5's "200 available on
/// request", equal to `crates/bridges/binance/src/family/market_feed.rs`'s `DEPTH_LEVELS`, the
/// deepest thing the tree can produce.
///
/// ⚠ **A SECOND constant on purpose:** the default is what `None` means, the ceiling what a request
/// may reach (§12.4 had one name wearing both numbers). The memory cost of the gap between them is
/// what the server's `MD_MAILBOX_BYTES` absorbs, and its declaration carries that argument.
pub const MD_DEPTH_LEVELS_CEILING: u16 = 200;

/// The largest an [`MdSpec::symbol`] may be, in BYTES. A longer one is
/// [`MdRefusal::SymbolRejected`], refused before the key reaches the server's registry.
///
/// It makes `crates/vike-datahub/src/md/mod.rs`'s `MD_CTRL_FRAME_CEILING_BYTES` (a `Status` frame
/// echoes its key's symbol) an ARITHMETIC consequence; that ceiling is a term in `MD_MAILBOX_BYTES`'
/// assertion, the plane's under-32-MB claim
/// (`docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md`).
///
/// - **FLOOR 78:** a polymarket CLOB token id is a decimal `uint256`, and `2^256 - 1` has exactly
///   78 digits; every other servable spelling is far shorter.
/// - **CEILING 109:** `crates/vike-datahub/src/md/mod.rs`'s `MD_STATUS_ENVELOPE_CEILING_BYTES` (a
///   `Status` with an EMPTY symbol at the widest slug, lane and status, 165 B, pinned by EQUALITY in
///   `crates/vike-datahub/tests/md_hub/depth_and_ctrl_lane.rs`'s
///   `a_status_frame_fits_the_declared_ctrl_ceiling`) leaves `384 - 165 = 219` bytes, and
///   serde_json at most doubles a CONTROL-FREE string: `219 / 2 = 109`.
/// - **96**, a judgement inside that range, leaves 27 B unspent (`165 + 2 * 96 = 357`), so a new
///   `WireStreamStatus` variant or a longer slug does not force a re-derivation.
///
/// ⚠ **[`validate_md_symbol`]'s control-byte rule is PART of this number:** serde_json escapes a
/// byte below `0x20` to six (`\u00XX`), so 96 control bytes would frame at 741 B. Move the two
/// together. For a genuinely longer symbol, RE-RUN the arithmetic and raise this — never remove the
/// check (`scripts/new_venue.sh`'s `MAX_VENUE_ID_LEN` words, applied here).
pub const MD_MAX_SYMBOL_BYTES: usize = 96;

/// The rules an [`MdSpec::symbol`] must satisfy, cheapest first, each naming what was wrong.
///
/// ONE definition for two ends: `crates/vike-datahub/src/md/hub/subscribe.rs`'s `MdHub::acquire` is
/// the server's door (and `MdHub::add_resident` checks it too, so `VIKE_DATAHUB_LIVE_RESIDENT`
/// cannot admit what the wire refuses), [`crate::DatahubClient::md_subscribe`] the client's.
///
/// ⚠ **The message never ECHOES the symbol:** the refused field is the unbounded one, and the hub's
/// reconciler renders failed keys at `info` every reap interval into a file layer that defaults to
/// `trace`. It names the LENGTH and the CAP.
///
/// ⚠ **Not trimmed** (only tested for being all whitespace): the venue is sent the spelling
/// verbatim, so altering it here would make the hub's key and the venue's subscription disagree.
pub fn validate_md_symbol(symbol: &str) -> Result<(), String> {
    if symbol.trim().is_empty() {
        return Err(format!(
            "a subscription symbol is BLANK ({} bytes, all whitespace), so it names no instrument. \
             A venue asked to subscribe to it either refuses or — worse — opens a stream that never \
             delivers, and the key is retried for as long as the session holds it. Pass the venue's \
             OWN spelling (`BTCUSDT.P`, a polymarket token id).",
            symbol.len()
        ));
    }
    if symbol.len() > MD_MAX_SYMBOL_BYTES {
        return Err(format!(
            "a subscription symbol of {} bytes exceeds MD_MAX_SYMBOL_BYTES = {MD_MAX_SYMBOL_BYTES}. \
             That bound is DERIVED from the control-frame ceiling this plane's memory assertion is \
             built on rather than chosen, and the longest symbol any roster venue can spell is a \
             78-digit polymarket token id. Pass the venue's own spelling.",
            symbol.len()
        ));
    }
    if let Some(at) = symbol.bytes().position(|b| b.is_ascii_control()) {
        return Err(format!(
            "a subscription symbol carries an ASCII CONTROL byte at offset {at}. No venue spells \
             one, and it is refused as PART of the length bound rather than beside it: serde_json \
             escapes such a byte to six, which breaks the 2x worst-case expansion \
             MD_MAX_SYMBOL_BYTES was derived against. Space, quote, backslash and every non-ASCII \
             byte remain legal."
        ));
    }
    Ok(())
}

// Compile-time bounds, the `crates/vike-datahub/src/server.rs` idiom: a RANGE, so a deliberate
// tweak stays free while "the bound was effectively removed" does not compile. `Duration` has no
// const comparison, so the checks are over millis/secs.
const _: () = assert!(
    MD_READ_TIMEOUT.as_millis() >= 2 * MD_HEARTBEAT.as_millis(),
    "MD_READ_TIMEOUT must allow at least TWO missed heartbeats — one missed beat is evidence of a \
     loaded box, not a dead link"
);
const _: () = assert!(
    MD_HEARTBEAT.as_secs() > 0 && MD_HEARTBEAT.as_secs() <= 60,
    "MD_HEARTBEAT must stay a POSITIVE, short period: 0 would write continuously and a multi-minute \
     value would make the read deadline useless as a liveness signal"
);
const _: () = assert!(
    MD_DEPTH_LEVELS_DEFAULT > 0 && MD_DEPTH_LEVELS_DEFAULT <= MD_DEPTH_LEVELS_CEILING,
    "the default depth must be POSITIVE and must not exceed the ceiling it is clamped against"
);
const _: () = assert!(
    MD_MAX_SYMBOL_BYTES >= 78,
    "MD_MAX_SYMBOL_BYTES must admit the longest symbol this wire can carry: a polymarket CLOB token \
     id is a uint256 spelled in decimal and 2^256-1 is exactly 78 digits, so 78 is a maximum by \
     construction and a bound under it refuses a real instrument. The UPPER half of this bound's \
     derivation is the ctrl-frame assertion in crates/vike-datahub/src/md/mod.rs, which the server \
     crate owns because it owns that ceiling"
);

// ---- The session id -----------------------------------------------------------------------------

/// A market-data session's opaque identity: the token a client presents in
/// [`Request::MdUpdate`](crate::proto::Request::MdUpdate) to mutate the subscription set of a
/// stream connection it opened earlier. Minted SERVER-SIDE, once per accepted
/// [`Request::MdSubscribe`](crate::proto::Request::MdSubscribe), and never reused.
///
/// ⚠ **Minted from THIS crate, by force:** `crates/vike-datahub/Cargo.toml` names no `rand`, so the
/// mint lives beside the type, in the crate that already carries `rand` for the auth nonce — one RNG
/// call site rather than two.
///
/// ⚠ **It is NOT a capability.** It names a subscription set and authorizes nothing: on a KEYED
/// server the connection's [`vike_node_proto::auth::Scope`] is the ceiling, and the server refuses
/// an `MdUpdate` naming a session it does not know (`crates/vike-datahub/src/server.rs`'s `MdUpdate`
/// arm).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MdSessionId(pub u128);

impl MdSessionId {
    /// A fresh, never-reused id from the OS-backed default generator — the same `fill_bytes` call
    /// [`vike_node_proto::auth::fresh_nonce`] draws the auth nonce from.
    pub fn fresh() -> Self {
        use rand::Rng; // rand 0.10 core trait — provides `fill_bytes`
        let mut raw = [0u8; 16];
        rand::rng().fill_bytes(&mut raw);
        MdSessionId(u128::from_be_bytes(raw))
    }
}

impl fmt::Display for MdSessionId {
    /// 32 lowercase hex characters, zero-padded — the same shape a nonce is printed in, and a
    /// spelling that survives `jq`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

impl FromStr for MdSessionId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        u128::from_str_radix(s, 16)
            .map(MdSessionId)
            .map_err(|e| format!("not a hex market-data session id: {e}"))
    }
}

impl Serialize for MdSessionId {
    /// ⚠ **HAND-WRITTEN: the HEX STRING is the point.** A `#[derive]` would put a 39-digit JSON
    /// NUMBER on the wire: `serde_json::to_value` rejects a `u128` above `u64::MAX` (no
    /// `arbitrary_precision` here), which breaks the workspace's wire-shape test idiom; `jq` parses
    /// it to `f64` and hands an operator a CORRUPTED id; and opaque tokens on this protocol are bytes
    /// or text (`Response::Welcome`'s `nonce`), never a big number. [`Self::deserialize`] is the
    /// exact inverse; `a_session_id_rides_as_hex_not_a_number` holds the pair.
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for MdSessionId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct HexVisitor;
        impl Visitor<'_> for HexVisitor {
            type Value = MdSessionId;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a market-data session id as lowercase hex")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<MdSessionId, E> {
                MdSessionId::from_str(v).map_err(E::custom)
            }
        }
        d.deserialize_str(HexVisitor)
    }
}

// ---- Specs and lanes ----------------------------------------------------------------------------

/// Which market-data LANE a subscription names.
///
/// ⚠ **[`MdLane::Depth`] and [`MdLane::Book`] ride the SAME payload ([`BookSnapshot`]) and stay
/// separate variants: THE VARIANT IS THE DISCLOSURE.** A 100 ms conflated snapshot is not a
/// lossless book, and wearing the book's name would let a maker-fill backtest report fills it
/// could never have got (`crates/vike-data/src/store/store_kind.rs`'s `depth` row: *"the path IS
/// the disclosure"*).
///
/// Admission is DERIVED from [`vike_data::require_live_verb`] over the declared capability matrix
/// (`crates/vike-model/src/venues/venue_caps.rs`'s `live_data` rows), never a hand list, so the
/// wire's [`MdRefusal::LaneUnsupported`] set cannot drift from it;
/// `crates/vike-datahub-client/tests/md_lane_caps.rs` drives that over the whole roster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MdLane {
    /// The CONFLATING L2 lane — `DataClient::subscribe_depth`, delivered through
    /// `LiveDataSink::l2_snapshot`. Latest-wins by contract; a superseded frame is not a loss.
    Depth,
    /// The LOSSLESS L2 lane — `DataClient::subscribe_book`, delivered through `LiveDataSink::book`.
    Book,
    /// The trade tape — `DataClient::subscribe_trades`, delivered through `LiveDataSink::trade`. A
    /// dropped print is a LOSS and is disclosed as [`MdFrame::TapeGap`].
    Trades,
}

impl MdLane {
    /// The [`vike_model::LiveVerb`] this lane is served by — the seam through which
    /// [`vike_data::require_live_verb`] decides whether a venue may be asked for it (§7.5's gate).
    pub fn live_verb(self) -> vike_model::LiveVerb {
        match self {
            MdLane::Depth => vike_model::LiveVerb::Depth,
            MdLane::Book => vike_model::LiveVerb::Book,
            MdLane::Trades => vike_model::LiveVerb::Trades,
        }
    }

    /// The `stream` label a venue feed passes to `LiveDataSink::stream_status` for this lane.
    ///
    /// ⚠ **String-keyed between two INDEPENDENT producers, hence a function with a test rather than a
    /// literal at the match site:** `crates/bridges/binance/src/family/market_feed.rs` passes the
    /// literal `"depth"`, `crates/bridges/polymarket/src/market_feed.rs` passes `PumpMode::as_str`. A
    /// missed label silently drops that lane's status disclosures (§12.5).
    pub fn feed_stream_label(self) -> &'static str {
        match self {
            MdLane::Depth => "depth",
            MdLane::Book => "book",
            MdLane::Trades => "trades",
        }
    }

    /// Parse a feed's `stream` label back to a lane; `None` for a label this wire serves no lane for
    /// (an interval like `"1m"`, or `"quotes"` — see [`MdFrame`] for why there is no quotes lane).
    pub fn from_feed_stream_label(label: &str) -> Option<Self> {
        match label {
            "depth" => Some(MdLane::Depth),
            "book" => Some(MdLane::Book),
            "trades" => Some(MdLane::Trades),
            _ => None,
        }
    }
}

/// ONE subscription request: a venue, that venue's OWN symbol spelling, and a lane.
///
/// `Eq + Hash` are load-bearing: the client's reconciler diffs SETS of these, and the server's hub
/// keys on `(venue, symbol, lane)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MdSpec {
    /// A `vike_model::VENUES` slug. An unknown one is [`MdRefusal::UnknownVenue`].
    pub venue: String,
    /// The venue's OWN spelling, exactly as `vike_data::DataClient` takes it (`"BTCUSDT.P"`, a
    /// polymarket token id, …). Symbol MAPPING is vike-catalog's concern and does not happen here.
    pub symbol: String,
    /// Which lane.
    pub lane: MdLane,
    /// Levels per side for [`MdLane::Depth`]/[`MdLane::Book`]; ignored on [`MdLane::Trades`].
    /// `None` = [`MD_DEPTH_LEVELS_DEFAULT`]; anything above [`MD_DEPTH_LEVELS_CEILING`] is CLAMPED,
    /// and the clamped number comes back in `MdSubscribed.accepted` — a clamp is an acceptance with
    /// a smaller number, never a refusal. Omitted from the bytes when `None`, the common case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth_levels: Option<u16>,
}

impl MdSpec {
    /// The subscription's IDENTITY — `(venue, symbol, lane)`, **excluding `depth_levels`**, so no
    /// consumer re-derives the tuple and forgets the exclusion. Removing
    /// `{binance, BTCUSDT, Depth, Some(200)}` removes the key whatever depth it asked for, and two
    /// clients asking for one key at different depths share ONE venue subscription.
    pub fn key(&self) -> (&str, &str, MdLane) {
        (&self.venue, &self.symbol, self.lane)
    }

    /// The server-authoritative depth for this spec: the request clamped to
    /// `[1, MD_DEPTH_LEVELS_CEILING]`, or [`MD_DEPTH_LEVELS_DEFAULT`] when none was asked for.
    /// Lives here so the server's clamp and any client-side prediction of it are ONE function.
    pub fn resolved_depth(&self) -> u16 {
        match self.depth_levels {
            None => MD_DEPTH_LEVELS_DEFAULT,
            Some(0) => 1,
            Some(n) => n.min(MD_DEPTH_LEVELS_CEILING),
        }
    }
}

// ---- The book payload ---------------------------------------------------------------------------

/// A full L2 state transfer for one key — the payload of both [`MdFrame::Depth`] and
/// [`MdFrame::Book`].
///
/// ⚠ **`vike_model::L2Book` never crosses this wire:** its private tick-indexed `BTreeMap`s would
/// pin an internal representation into a wire designed for mixed versions. The client rebuilds
/// through `L2Book::apply_snapshot`, as `crates/vike-app-core/src/data/data_sink.rs`'s
/// `BookStore::update` already does from raw levels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookSnapshot {
    /// The venue slug this key belongs to.
    pub venue: String,
    /// The venue's own symbol spelling.
    pub symbol: String,
    /// The feed's AUTHORITATIVE price grid; `0.0` means the feed did not know.
    ///
    /// ⚠ **A client MUST construct or repoint its book with THIS value before applying the
    /// levels.** `L2Book::apply_snapshot` ticks prices through `self.tick_size`, and
    /// `BookStore::update` falls back to `infer_tick` only at `<= 0` — an inferred grid jitters frame
    /// to frame, and the ladder's rows move under the cursor.
    pub tick_size: f64,
    /// Bids, **BEST FIRST — DESCENDING price**: the order
    /// `crates/vike-marketdata/src/orderbook.rs`'s `L2Book::top_n` returns, which the server
    /// produces them from. ⚠ Stated in the type because a producer that sorted ascending would paint
    /// every DOM ladder upside down, which no round-trip test catches.
    pub bids: Vec<vike_model::BookLevel>,
    /// Asks, **BEST FIRST — ASCENDING price** (`L2Book::top_n`'s `asks.iter()`).
    pub asks: Vec<vike_model::BookLevel>,
    /// The newest stamp this LANE carries, epoch-ms — **DISPLAY AND DIAGNOSTICS ONLY**: the CLIENT's
    /// own receipt is authoritative for staleness (§7.4), since two boxes' clocks differ. ⚠ On
    /// [`MdFrame::Depth`] it is the venue/feed stamp; `LiveDataSink::book` carries no time, so on
    /// [`MdFrame::Book`] it is the HUB's receipt clock.
    pub venue_ts: i64,
    /// `L2Book::last_seq`, forwarded. **DIAGNOSTIC ONLY:** the publisher CONFLATES, so frames skip
    /// venue sequence numbers by design. ⚠ It is the value that goes into `L2Book::apply_snapshot`
    /// — never [`Self::seq`], which is per-connection accounting.
    pub venue_seq: u64,
    /// The WIRE sequence for this `(key, lane)`, assigned BEFORE any drop decision, strictly +1
    /// (§7.2): a break means *this connection missed frames the server produced.* On the BOOK lanes
    /// that needs no action (each frame is a full snapshot); on [`MdFrame::Trades`] it is the
    /// backstop [`MdFrame::TapeGap`] is checked against.
    pub seq: u64,
}

// ---- The frame ----------------------------------------------------------------------------------

/// One PUSHED market-data frame. Rides [`Response::Md`](crate::proto::Response::Md), and appears
/// only on a stream connection (see this module's §0 invariant).
///
/// ⚠ **There is no `Quotes` lane and no `Bars` lane.** `GuiFeedSink::quote` is a no-op (its only
/// consumer, the core's `PriceBoard`, does not exist in the desktop), so a quotes lane would deliver
/// into nothing; bars are refused for the reason §10 gives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MdFrame {
    /// The CONFLATING lane's state transfer. See [`MdLane`] for why the variant, not a field, is
    /// what discloses which lane produced it.
    Depth(BookSnapshot),
    /// The LOSSLESS lane's state transfer.
    Book(BookSnapshot),
    /// A batch of trade prints for one key, oldest first.
    Trades {
        /// The venue slug.
        venue: String,
        /// The venue's own symbol spelling.
        symbol: String,
        /// The prints, **each with an EMPTY `symbol`**: this variant's own `symbol` is authoritative
        /// for every tick in the batch.
        ///
        /// ⚠ **A CLIENT MUST RE-STAMP BEFORE HANDING A TICK TO ANYTHING THAT READS THE FIELD**
        /// (`crates/vike-orderflow/src/bar_agg.rs`'s `OrderflowAgg` would fold every venue's tape
        /// into one `""` bucket). `crates/vike-datahub/src/md/hub.rs`'s `push_trade` blanks it on
        /// the way IN to keep a polymarket tape entry small (§12.4); re-stamping on the SERVER would
        /// cost a `String` per tick and put the bytes back on a byte-bounded mailbox, where the
        /// client's end is one `clone_from` per batch.
        ticks: Vec<vike_model::TradeTick>,
        /// The wire sequence — see [`BookSnapshot::seq`].
        seq: u64,
    },
    /// A stream-health disclosure forwarded verbatim from the venue feed. Without it a remote book
    /// is indistinguishable at the desktop from a frozen ladder in a quiet market.
    Status {
        /// The venue slug.
        venue: String,
        /// The venue's own symbol spelling.
        symbol: String,
        /// Which lane the status is about.
        lane: MdLane,
        /// The status.
        status: WireStreamStatus,
    },
    /// **Prints were LOST and here is how many.** The tape lane's primary disclosure: a silently
    /// dropped print permanently corrupts CVD, delta and footprint volume in
    /// `crates/vike-orderflow/src/bar_agg.rs`'s `OrderflowAgg`, which has no per-trade dedup and no
    /// error channel.
    ///
    /// ⚠ **ORDERING IS CONTRACTUAL: an owed `TapeGap` for a key is written BEFORE that key's next
    /// [`MdFrame::Trades`], never after** (after the fold is too late to discard the corrupted
    /// state); the writer synthesizes it immediately before the batch.
    ///
    /// ⚠ **SEVERAL HOLES MERGE INTO ONE MARKER**, at the EARLIEST frame any of them precedes:
    /// `dropped` is the TOTAL, the range the earliest window, and a later hole inside it shows as a
    /// [`BookSnapshot::seq`] jump (`crates/vike-datahub/src/md/mailbox.rs`'s `Inner::owe`).
    TapeGap {
        /// The venue slug.
        venue: String,
        /// The venue's own symbol spelling.
        symbol: String,
        /// How many prints were lost.
        dropped: u64,
        /// The wire seq of the last frame delivered before the hole.
        from_seq: u64,
        /// The wire seq of the first frame delivered after it.
        to_seq: u64,
    },
    /// Written after [`MD_HEARTBEAT`] of silence so a quiet subscription proves the link is alive.
    Heartbeat,
    /// The server is ending this stream, and why. The socket closes after it.
    Bye(MdBye),
}

// ---- The status mirror --------------------------------------------------------------------------

/// The wire MIRROR of [`vike_data::StreamStatus`], which derives no serde: a layer-20 enum does not
/// grow serde to serve a wire (the precedent is `crates/vike-tradehub-client/src/wire.rs`'s
/// `WireTradingState`). Unlike that precedent's one-way server-side projection, this is a [`From`]
/// PAIR, below both ends, because BOTH convert (the server out, the client's session in) and both
/// impls are orphan-legal from this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireStreamStatus {
    /// The stream can no longer be trusted from `at_ts_ms` — transport error, server close, or an
    /// idle-watchdog trip.
    GapStart {
        /// When the gap opened (epoch-ms).
        at_ts_ms: i64,
    },
    /// The stream is live again; `gap_started_ts_ms` echoes the matching `GapStart` (or the `Stale`
    /// episode's trip time) when there was one.
    Live {
        /// The episode this recovery closes, if any.
        gap_started_ts_ms: Option<i64>,
    },
    /// The transport is alive but no FRESH DATA has arrived — the silently-failed re-subscribe a
    /// socket-liveness watchdog cannot see.
    Stale {
        /// The newest data stamp seen (epoch-ms).
        newest_data_ts_ms: i64,
        /// When the staleness was judged (epoch-ms).
        now_ms: i64,
    },
}

impl From<vike_data::StreamStatus> for WireStreamStatus {
    fn from(s: vike_data::StreamStatus) -> Self {
        match s {
            vike_data::StreamStatus::GapStart { at_ts_ms } => {
                WireStreamStatus::GapStart { at_ts_ms }
            }
            vike_data::StreamStatus::Live { gap_started_ts_ms } => {
                WireStreamStatus::Live { gap_started_ts_ms }
            }
            vike_data::StreamStatus::Stale { newest_data_ts_ms, now_ms } => {
                WireStreamStatus::Stale { newest_data_ts_ms, now_ms }
            }
        }
    }
}

impl From<WireStreamStatus> for vike_data::StreamStatus {
    fn from(s: WireStreamStatus) -> Self {
        match s {
            WireStreamStatus::GapStart { at_ts_ms } => {
                vike_data::StreamStatus::GapStart { at_ts_ms }
            }
            WireStreamStatus::Live { gap_started_ts_ms } => {
                vike_data::StreamStatus::Live { gap_started_ts_ms }
            }
            WireStreamStatus::Stale { newest_data_ts_ms, now_ms } => {
                vike_data::StreamStatus::Stale { newest_data_ts_ms, now_ms }
            }
        }
    }
}

// ---- Refusals -----------------------------------------------------------------------------------

/// Why ONE spec in a subscribe/update was not served.
///
/// ⚠ **PER-SPEC. A whole-REQUEST failure is `Response::Error`, never a refusal list.** So there is
/// no `HubNotMounted` (§4.4): uniform across every spec, it would answer
/// `MdSubscribed { accepted: [], refused: [all] }` and MODE-SWITCH into a writer that never sends a
/// frame. A build with no hub answers `Response::Error` and does not switch (as
/// `crates/vike-datahub/src/server/backfill.rs`'s `backfill_verb` does for an unmounted collector
/// table); so does an `MdUpdate` naming an unknown or expired session.
///
/// ⚠ **TYPED, because the client's retry split is load-bearing:**
/// `crates/vike-app-core/src/ui/feed_lifecycle.rs`'s `FeedRetries::note_error` never retries
/// `LiveDataError::Unsupported`. [`Self::is_permanent`] is that split.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MdRefusal {
    /// The venue slug is not in `vike_model::VENUES`.
    UnknownVenue,
    /// A real venue, but this BUILD links no market-data client for it — the `md_venue=` set the
    /// server advertised says which it does. The string names the supported set.
    VenueNotServed(String),
    /// This venue's declared `vike_model::VenueCaps.live_data` serves no such lane. The string is
    /// [`vike_data::require_live_verb`]'s OWN `&'static str`, forwarded verbatim, so the wire's
    /// refusal set cannot drift from the declared matrix.
    ///
    /// ⚠ **Deliberately no `impl From<MdRefusal> for vike_data::LiveDataError`:** its `Unsupported`
    /// carries a `&'static str` and this arrives as an owned `String`, so the only conversion is an
    /// unbounded `Box::leak`. A client calls `require_live_verb` locally (the same authority the
    /// server used) for its OWN `&'static str`, and carries this text into the per-venue status
    /// string.
    LaneUnsupported(String),
    /// The spec's SYMBOL is not one this wire will carry — blank, over [`MD_MAX_SYMBOL_BYTES`], or
    /// carrying an ASCII control byte. The string is [`validate_md_symbol`]'s OWN message, forwarded
    /// verbatim (the [`MdRefusal::LaneUnsupported`] discipline), and never echoes the symbol. ONE
    /// variant, because ONE validator produces all three messages.
    SymbolRejected(String),
    /// The process-wide on-demand key budget is full.
    KeyCapTotal {
        /// Keys currently held.
        held: u32,
        /// The cap.
        cap: u32,
    },
    /// This VENUE's key budget is full — the cap that protects the order-signing daemon's share of
    /// the box's per-IP venue budget.
    KeyCapVenue {
        /// The venue.
        venue: String,
        /// Keys currently held on it.
        held: u32,
        /// The cap.
        cap: u32,
    },
    /// This SESSION already holds its maximum number of specs.
    SpecCapSession {
        /// Specs currently held.
        held: u32,
        /// The cap.
        cap: u32,
    },
}

impl MdRefusal {
    /// Whether retrying this spec can ever succeed on this server.
    ///
    /// `true` for the CAPABILITY refusals (an unknown venue, a venue this build does not link, a
    /// lane the declared caps do not serve) and the VALIDITY refusal [`MdRefusal::SymbolRejected`]:
    /// nothing on this server can make the spec legal later, so a client records them the way
    /// `FeedRetries` records `Unsupported` — refused, never retried. `false` for the three CAPS: a
    /// cap frees up when another window closes, so the session keeps the spec DESIRED and its
    /// reconciler retries on backoff.
    ///
    /// The match is exhaustive with no `_` arm, so a new refusal cannot inherit a classification.
    pub fn is_permanent(&self) -> bool {
        match self {
            MdRefusal::UnknownVenue
            | MdRefusal::VenueNotServed(_)
            | MdRefusal::LaneUnsupported(_)
            | MdRefusal::SymbolRejected(_) => true,
            MdRefusal::KeyCapTotal { .. }
            | MdRefusal::KeyCapVenue { .. }
            | MdRefusal::SpecCapSession { .. } => false,
        }
    }
}

/// Why the server is ending a market-data stream. Carried by [`MdFrame::Bye`], which is the last
/// frame on that socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MdBye {
    /// The subscriber accumulated more tape lapses than the server's budget inside its window: a
    /// client that cannot keep up is being lied to more slowly, and holding the socket costs a
    /// thread and a venue refcount for nothing.
    TooSlow {
        /// How many lapses were counted.
        lapses: u64,
    },
    /// The server is shutting down.
    ServerStopping,
    /// The session held no specs for long enough that keeping the socket bought nothing.
    SessionIdle,
    /// The reserved CONTROL lane overflowed: this subscriber could not absorb even the `Status`
    /// frames its own keys produced.
    ///
    /// ⚠ **Distinct from [`Self::SessionIdle`]**, the opposite diagnosis (idle asked for nothing,
    /// this asked for more than it could read): the reason is all a client or an operator has once
    /// the socket is gone.
    ControlLaneOverflow,
}

#[path = "market_tests.rs"]
#[cfg(test)]
mod market_tests;
