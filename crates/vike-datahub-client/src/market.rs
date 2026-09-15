//! The MARKET-DATA vocabulary of the datahub wire — the types
//! [`Request::MdSubscribe`](crate::proto::Request::MdSubscribe) /
//! [`Request::MdUpdate`](crate::proto::Request::MdUpdate) and
//! [`Response::MdSubscribed`](crate::proto::Response::MdSubscribed) /
//! [`Response::MdUpdated`](crate::proto::Response::MdUpdated) /
//! [`Response::Md`](crate::proto::Response::Md) carry.
//!
//! Ports nothing — this is a new wire, designed in
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §4.
//!
//! # Why it lives HERE (layer 30) and not in either daemon
//!
//! The server (`vike-datahub`, layer 65) and the desktop's session (`vike-app-core`, layer 80) must
//! name ONE vocabulary, and neither can see the other. That is this workspace's standing rule for
//! two sides that must not disagree: *the cure is a shared crate BELOW both.* It is the same
//! argument that put [`crate::proto::plane_of`] and [`crate::proto::required_scope`] here under
//! ruling 7.
//!
//! **It adds NO dependency.** Everything named below is already a dependency of this crate:
//! `vike-data` at DEFAULT features (the `HistStore` trait and the ungated `pub mod live`, so
//! [`vike_data::StreamStatus`] and `require_live_verb` are reachable on a DataFusion-free build),
//! `vike-model` (`Level`, `TradeTick`, `LiveVerb`), `serde`/`serde_json`, and `rand` (already here
//! for [`crate::node_auth::fresh_nonce`], and what [`MdSessionId::fresh`] mints from). This crate
//! declares NO `[features]` table and must not grow one — see the module doc of [`crate::proto`]
//! for the negotiation reason, and `crates/vike-ops/tests/feature_lane_coverage.rs` for the gate
//! reason.
//!
//! # ⚠ THE ONE INVARIANT EVERYTHING ELSE HANGS OFF
//!
//! A datahub connection is POSITIONAL (one request, one reply) until — and unless —
//! [`Request::MdSubscribe`](crate::proto::Request::MdSubscribe) is answered with
//! [`Response::MdSubscribed`](crate::proto::Response::MdSubscribed). Then:
//!
//! 1. `MdSubscribed` is the LAST positional frame that socket will ever carry.
//! 2. After it, EVERY server→client frame is [`Response::Md`](crate::proto::Response::Md), and the
//!    client→server direction is SILENT — the server's writer has left the read loop and nothing
//!    reads it.
//! 3. The socket ends on [`MdFrame::Bye`] followed by a close, or on a transport fault.
//!
//! Rung 2 is what makes "no correlation id" true rather than merely convenient: a stream reader may
//! `match` on `Response::Md` and treat anything else as a protocol desync. It is also why
//! [`MdFrame::Heartbeat`] is a variant HERE rather than a reuse of `Response::Pong` (which is what
//! `crates/vike-tradehub/src/server.rs`'s `run_push_writer` does on the ACCOUNT plane) — one decode
//! path on a stream socket, one match, one invariant.
//!
//! ⚠ **Rung 1 has an exception and it must be written down.** `MdSubscribe` mode-switches ONLY when
//! it is answered with `MdSubscribed`. A server that answers `Response::Error` — one predating the
//! verb, or one built with no market-data plane — has NOT switched: the connection is still
//! positional and the client must not start a reader thread. That is leg (3) of
//! [`crate::proto::FEATURE_MARKET_DATA`]'s capability contract, and it is guaranteed structurally by
//! `crates/vike-datahub/src/server.rs`'s `handle_connection` framing/decode split.
//!
//! # What is NOT here
//!
//! The CAPS (`MD_MAX_KEYS_TOTAL`, `MD_MAILBOX_CAP`, `MD_LINGER`, `MD_WRITE_TIMEOUT`, …) are the
//! server's own and live in `crates/vike-datahub/src/md/`. The rule is
//! `crates/vike-tradehub/src/server.rs`'s `PUSH_WRITE_TIMEOUT` doc, verbatim: *"Not a `liveness`
//! constant, deliberately: the client crate's numbers are halves of a contract BOTH ends must agree
//! on, and this one has no client half."* A cap's client half is the TYPED REFUSAL carrying the
//! number ([`MdRefusal::KeyCapTotal`] and its siblings), which is the whole reason [`MdRefusal`] is
//! typed rather than a `String`.
//!
//! ⚠ **A VALIDITY rule is not a cap, and the two fall on opposite sides of that line.** The test is
//! whether the refusal can ever lift on its own: a cap frees up when another window closes, which is
//! why [`MdRefusal::is_permanent`] answers `false` for all three of them and a client keeps the spec
//! in its desired set. A malformed symbol is refused PERMANENTLY, and the permanent family is
//! exactly the one both ends compute LOCALLY — [`MdRefusal::LaneUnsupported`]'s own doc says the
//! client "calls `require_live_verb` locally, the same authority the server used". So
//! [`MD_MAX_SYMBOL_BYTES`] and [`validate_md_symbol`] live HERE, one definition for two ends, in
//! [`MdSpec::resolved_depth`]'s shape and for its stated reason.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// ---------------------------------------------------------------------------------------------
// Constants — the ones BOTH ends need
// ---------------------------------------------------------------------------------------------

/// How often a market-data stream connection writes a [`MdFrame::Heartbeat`] when it has nothing
/// else to say, so a quiet subscription proves the link is alive rather than merely silent.
///
/// ⚠ **This number is ADOPTED, not measured, and saying so is part of the contract.** §12 of the
/// design measured the venue side of this wire exhaustively and measured *nothing* bearing on LINK
/// liveness — and §12.7 refuses to derive `MD_WRITE_TIMEOUT` for exactly that reason. The heartbeat
/// is in that family. What it is adopted FROM is a real argument rather than laziness:
/// `vike_tradehub_client::liveness`'s `OBSERVE_HEARTBEAT` is 15 s, the desktop already runs one
/// such deadline against the ACCOUNT plane, and the two streams fail the same way — a peer that
/// stops writing looks identical to a peer that has nothing to write. One number for both is one
/// thing for an operator to reason about.
pub const MD_HEARTBEAT: Duration = Duration::from_secs(15);

/// The client's read deadline on a market-data stream: three heartbeats.
///
/// ⚠ **It is a FLOOR, not the contract.** The authoritative deadline is
/// `max(MD_READ_TIMEOUT, 3 × MdSubscribed.heartbeat_ms)` — the server SENDS its own heartbeat
/// period in [`Response::MdSubscribed`](crate::proto::Response::MdSubscribed), so this constant
/// never has to be a compile-time agreement between two independently-deployed binaries. The
/// account plane could not do this: `vike_tradehub_client::liveness`'s `OBSERVE_READ_TIMEOUT` is a
/// constant on BOTH sides, and that module's own doc names the hazard ("an operator who could raise
/// `OBSERVE_READ_TIMEOUT` on one side alone…"). This wire fixed it by carrying the number.
pub const MD_READ_TIMEOUT: Duration = Duration::from_secs(3 * MD_HEARTBEAT.as_secs());

/// What an [`MdSpec::depth_levels`] of `None` resolves to, per side. **MEASURED** (§12.4, the CI box,
/// 2026-09-10): an unclamped binance frame is 10,458 B at p50 and 10,479 B at max (200 levels a
/// side); clamped to 50 it is 2,721–2,736 B — a 3.8× cut. Polymarket's whole folded book is 99
/// levels, so the same clamp is nearly a no-op there (1,698 → 1,485 B at p50). 50 is the value at
/// which the DEEPEST lane in the tree is cut fourfold and the SHALLOWEST is untouched, which is
/// exactly the property a default wants.
pub const MD_DEPTH_LEVELS_DEFAULT: u16 = 50;

/// The ceiling an [`MdSpec::depth_levels`] request is CLAMPED to — §4.5's "200 available on
/// request", and equal to `crates/bridges/binance/src/family/market_feed.rs`'s `DEPTH_LEVELS`, so
/// the ceiling is the deepest thing the tree can actually produce and a clamp above it would be a
/// lie.
///
/// ⚠ **This is a SECOND constant on purpose, and §12.4 needed it to be.** That section's table row
/// reads `MD_MAX_DEPTH_LEVELS | 50/side` while its own byte-bound paragraph then reasons about "a
/// client that asks for [200/side]" — one name wearing two numbers. Split, the default is what
/// `None` means and the ceiling is what a request may reach; the memory consequence of the gap
/// between them is what the server's `MD_MAILBOX_BYTES` exists to absorb, and its declaration
/// carries that argument.
pub const MD_DEPTH_LEVELS_CEILING: u16 = 200;

/// The largest an [`MdSpec::symbol`] may be, in BYTES. A longer one is
/// [`MdRefusal::SymbolRejected`], refused before the key reaches the server's registry.
///
/// # What a client could do without it
///
/// Nothing bounded this field but the post-auth `MAX_FRAME_LEN` (64 MiB). A subscribe naming a
/// 10,000-character symbol was ACCEPTED: the key entered the hub, the reconciler called the venue's
/// `subscribe_*` with it, and — because `crates/vike-datahub/src/md/hub.rs`'s `attach_frames` emits
/// a `Status` for every accepted key whether or not a venue ever streams for it — the ctrl lane
/// carried that string back verbatim, far over `crates/vike-datahub/src/md/mod.rs`'s
/// `MD_CTRL_FRAME_CEILING_BYTES`. That constant is a TERM in `MD_MAILBOX_BYTES`' compile-time
/// assertion, which `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` leans on
/// for the claim that this whole plane stays under 32 MB. One wire field invalidated it.
///
/// # What the number is FOR, and what it is derived FROM
///
/// It makes `MD_CTRL_FRAME_CEILING_BYTES` an ARITHMETIC consequence instead of a number measured
/// against one example. Before this bound, the only thing holding that ceiling was one test framing
/// one polymarket token id — nothing made any OTHER symbol fit.
///
/// **The FLOOR — 78.** The longest symbol this wire can carry is a polymarket CLOB token id, a
/// `uint256` spelled in decimal; `2^256 - 1` is exactly 78 digits, so 78 is a MAXIMUM by
/// construction rather than a measurement — §12.3/§12.4 of
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` measured 78 and could not
/// have measured 79. Every other servable spelling is far shorter: binance `BTCUSDT.P` is 9, okx
/// `BTC-USDT-SWAP` 13, an okx dated option `BTC-USD-250627-100000-C` 23, hyperliquid `@107` 4.
///
/// **The CEILING — 109.** `crates/vike-datahub/src/md/mod.rs`'s
/// `MD_STATUS_ENVELOPE_CEILING_BYTES` is everything a framed `Status` costs with an EMPTY symbol, at
/// the widest venue slug, lane word and status spelling the roster can produce — 165 B, pinned by
/// EQUALITY (not `<=`) in `crates/vike-datahub/tests/md_hub.rs`'s
/// `a_status_frame_fits_the_declared_ctrl_ceiling`. That leaves `384 - 165 = 219` bytes of symbol
/// budget, and serde_json's worst expansion of a CONTROL-FREE string is 2x (`"` and `\` cost two
/// bytes each; every non-ASCII byte rides raw), so `219 / 2 = 109`.
///
/// **The number — 96**, inside `[78, 109]` and a judgement within an arithmetic range rather than a
/// measurement. It is 1.23x the only hard maximum this wire has, and it deliberately leaves 27 B of
/// the ceiling unspent (`165 + 2 * 96 = 357 <= 384`) so that adding a `WireStreamStatus` variant or
/// onboarding a longer venue slug does not force the SYMBOL bound to be re-derived along with the
/// envelope. Taking the ceiling itself would have coupled two unrelated things.
///
/// ⚠ **[`validate_md_symbol`]'s control-byte rule is PART of this number, not hygiene beside it.**
/// serde_json escapes a byte below `0x20` as a six-byte `\u00XX`, so without that rule the worst
/// expansion is 6x: 96 control bytes frame at 741 B, 1.9x the ceiling, and the derivation above is
/// simply false. Move the two together or not at all — the compile-time assertion beside
/// `MD_STATUS_ENVELOPE_CEILING_BYTES` is what will tell whoever tries.
///
/// ⚠ **The remedy for a genuinely longer symbol is to RE-RUN the arithmetic above and raise this,
/// never to remove the check** — `scripts/new_venue.sh`'s `MAX_VENUE_ID_LEN` words, applied here.
pub const MD_MAX_SYMBOL_BYTES: usize = 96;

/// The rules an [`MdSpec::symbol`] must satisfy, cheapest first, each naming what was wrong.
///
/// ONE definition for two ends: `crates/vike-datahub/src/md/hub.rs`'s `MdHub::acquire` is the
/// server's door and [`crate::DatahubClient::md_subscribe`] the client's, so a client cannot guard a
/// rule the server does not know, nor the reverse. It is also what
/// `crates/vike-datahub/src/md/hub.rs`'s `MdHub::add_resident` checks, so the daemon does not admit
/// through `VIKE_DATAHUB_LIVE_RESIDENT` what it refuses on the wire.
///
/// ⚠ **The message never ECHOES the symbol back**, and that is deliberate rather than terse. The
/// field being refused is the unbounded one, and a refusal quoting it is the same unbounded cost
/// wearing a log line — the hub's reconciler renders its failed keys at `info` every reap interval,
/// into a file layer that defaults to `trace`, and `CLAUDE.md`'s logging section carries what an
/// unbounded write rate there once cost a live box. It names the LENGTH and the CAP, which is what
/// an operator can act on.
///
/// ⚠ **A symbol is NOT trimmed** — only tested for being entirely whitespace. The venue is sent the
/// spelling verbatim, so silently altering it here would make the hub's key and the venue's
/// subscription disagree about what was asked for.
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

// Compile-time bounds, the `crates/vike-datahub/src/server.rs` / tradehub `confirm.rs` idiom: a
// RANGE, so a deliberate tweak stays free while "the bound was effectively removed" does not
// compile. `Duration` has no const comparison, so the checks are over millis/secs.
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

// ---------------------------------------------------------------------------------------------
// The session id
// ---------------------------------------------------------------------------------------------

/// A market-data session's opaque identity: the token a client presents in
/// [`Request::MdUpdate`](crate::proto::Request::MdUpdate) to mutate the subscription set of a
/// stream connection it opened earlier.
///
/// Minted SERVER-SIDE, once per accepted [`Request::MdSubscribe`](crate::proto::Request::MdSubscribe),
/// and never reused.
///
/// ⚠ **It is minted from THIS crate rather than from the server, and that is forced.**
/// `crates/vike-datahub/Cargo.toml` deliberately dropped `rand` when `fresh_nonce` moved down here
/// under ruling 7 ("⚠ NO `rand` any more"), so the data daemon cannot name a generator at all. The
/// mint therefore lives beside the type, in the crate that already carries the dependency for the
/// auth nonce — one RNG call site rather than two.
///
/// ⚠ **It is NOT a capability.** A session id names a subscription set; it does not authorize
/// anything. On a KEYED server the connection's [`crate::node_auth::Scope`] is the ceiling, and the
/// server additionally refuses an `MdUpdate` naming a session it does not know — see
/// `crates/vike-datahub/src/server.rs`'s `MdUpdate` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MdSessionId(pub u128);

impl MdSessionId {
    /// A fresh, never-reused id from the OS-backed default generator — the same source, and the
    /// same call, [`crate::node_auth::fresh_nonce`] draws the auth nonce from (`fill_bytes` rather
    /// than a typed `random()`, so this mint names exactly the one `rand` API this crate already
    /// depends on).
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
    /// ⚠ **HAND-WRITTEN, AND THE HEX STRING IS THE POINT.** A `#[derive]` would put a 39-digit JSON
    /// NUMBER on the wire, and three independent things in this workspace break on that:
    ///
    /// 1. **`serde_json::to_value` rejects a `u128` above `u64::MAX`** without the
    ///    `arbitrary_precision` feature, which this workspace does not enable. That function is the
    ///    workspace's own wire-shape test idiom — `crates/vike-tradehub-client/src/proto.rs`'s
    ///    `the_reason_rides_beside_the_command_never_inside_it` uses it — so a derived id would work
    ///    on the real wire and blow up in any test written the way this repo writes them.
    /// 2. **`jq` is this project's mandated JSON tool** and parses a 39-digit number to `f64`, so an
    ///    operator debugging a stream would be handed a silently CORRUPTED id.
    /// 3. **No `u128` rides any wire type in this workspace today.** The established spelling for an
    ///    opaque 128/256-bit token on THIS protocol is `Response::Welcome`'s
    ///    `nonce: Option<[u8; 32]>` — bytes or text, never a big number.
    ///
    /// The in-memory type is unchanged; only the encoding differs. [`Self::deserialize`] is its
    /// exact inverse and `a_session_id_rides_as_hex_not_a_number` is what holds the pair.
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

// ---------------------------------------------------------------------------------------------
// Specs and lanes
// ---------------------------------------------------------------------------------------------

/// Which market-data LANE a subscription names.
///
/// ⚠ **[`MdLane::Depth`] and [`MdLane::Book`] ride the SAME payload ([`BookSnapshot`]) and are
/// separate variants anyway — THE VARIANT IS THE DISCLOSURE.** A full snapshot every 100 ms with
/// every intermediate state discarded is not a lossless book, and letting it wear the book's name
/// lets a maker-fill backtest report fills it could never have got. This is the wire spelling of
/// `crates/vike-recorder/src/session.rs`'s deliberate `Stream::Book`/`Stream::Depth` split and of
/// `crates/vike-data/src/store_kind.rs`'s `depth` row, whose own words are the rule: *"the path IS
/// the disclosure, so a consumer asking for `book` never receives conflated data."*
///
/// ⚠ **On the initial venue set that is a strict PARTITION rather than a formality**, read off
/// `crates/vike-model/src/venue_caps.rs`'s `live_data` rows:
///
/// | venue | `Depth` | `Book` |
/// |---|---|---|
/// | binance / bybit / okx / aster / hyperliquid | served | REFUSED |
/// | polymarket | REFUSED | served |
///
/// So on day one **no venue serves both lanes**: `Book` resolves for polymarket alone and `Depth`
/// for the five CEX alone. Every other combination is a [`MdRefusal::LaneUnsupported`] DERIVED from
/// [`vike_data::require_live_verb`] rather than from a hand list, so the wire's refusal set cannot
/// drift from the declared matrix.
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
    /// [`vike_data::require_live_verb`] decides whether a venue may be asked for it, so §7.5's gate
    /// is a re-read of the declared capability matrix rather than a second list.
    pub fn live_verb(self) -> vike_model::LiveVerb {
        match self {
            MdLane::Depth => vike_model::LiveVerb::Depth,
            MdLane::Book => vike_model::LiveVerb::Book,
            MdLane::Trades => vike_model::LiveVerb::Trades,
        }
    }

    /// The `stream` label a venue feed passes to `LiveDataSink::stream_status` for this lane.
    ///
    /// ⚠ **This mapping is string-keyed on the wire between two INDEPENDENT producers, and that is
    /// why it is a function with a test rather than a literal at the match site.**
    /// `crates/bridges/binance/src/family/market_feed.rs` passes the literal `"depth"`;
    /// `crates/bridges/polymarket/src/market_feed.rs` passes `PumpTiming`'s `stream_label`, i.e.
    /// `PumpMode::as_str`. `crates/vike-data/src/live_rec.rs`'s `RecorderSink::stream_status`
    /// already accepts the same brittleness (`if stream != "book" { return; }`) — and §12.5 measured
    /// what a missed lane label costs: a binance depth series that had been a reconnect artefact for
    /// forty days, with no marker in the store because the disclosure never reached it.
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
/// `Eq + Hash` are load-bearing rather than derived out of habit: the client's reconciler diffs SETS
/// of these, and the server's hub keys on `(venue, symbol, lane)`.
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
    /// a smaller number, never a refusal, so a client that asked for 200 LEARNS it got 50.
    ///
    /// The `default` + `skip_serializing_if` pair is the one `Response::Welcome`'s `nonce` uses, for
    /// the same reason: a spec with no depth request is the common case and its bytes should not
    /// carry a `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth_levels: Option<u16>,
}

impl MdSpec {
    /// The subscription's IDENTITY — `(venue, symbol, lane)`, **excluding `depth_levels`**.
    ///
    /// It exists so no consumer re-derives the tuple and forgets the exclusion. Depth is not part of
    /// a subscription's identity: a client that removes `{binance, BTCUSDT, Depth, Some(200)}`
    /// removes the key it holds whatever depth it originally asked for, and two clients asking for
    /// one key at different depths share ONE venue subscription.
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

// ---------------------------------------------------------------------------------------------
// The book payload
// ---------------------------------------------------------------------------------------------

/// A full L2 state transfer for one key — the payload of both [`MdFrame::Depth`] and
/// [`MdFrame::Book`].
///
/// ⚠ **`vike_model::L2Book` deliberately never crosses this wire.** It derives serde (for journal
/// replay) and would round-trip, so this is a CHOICE: its `bids`/`asks` are private tick-indexed
/// `BTreeMap`s, and putting them on a protocol pins an internal representation into a wire designed
/// for mixed versions. The client rebuilds through `L2Book::apply_snapshot`, which is what
/// `crates/vike-app-core/src/data_sink.rs`'s `BookStore::update` already does from raw levels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookSnapshot {
    /// The venue slug this key belongs to.
    pub venue: String,
    /// The venue's own symbol spelling.
    pub symbol: String,
    /// The feed's AUTHORITATIVE price grid.
    ///
    /// ⚠ **A client MUST construct or repoint its book with THIS value before applying the levels
    /// below.** `L2Book::apply_snapshot` ticks incoming prices through `self.tick_size`, and
    /// `crates/vike-app-core/src/data_sink.rs`'s `BookStore::update` falls back to `infer_tick` only
    /// at `<= 0` — an inferred grid jitters frame to frame, which paints a ladder whose rows move
    /// under the cursor. `0.0` means the feed did not know.
    pub tick_size: f64,
    /// Bids, **BEST FIRST — DESCENDING price**. This is the exact order
    /// `crates/vike-model/src/orderbook.rs`'s `L2Book::top_n` returns (`bids.iter().rev()`), and the
    /// server produces them from that call. ⚠ Stated in the type because without it a future
    /// producer sorts ascending and every DOM ladder paints upside down — a defect no round-trip
    /// test would catch.
    pub bids: Vec<vike_model::Level>,
    /// Asks, **BEST FIRST — ASCENDING price** (`L2Book::top_n`'s `asks.iter()`).
    pub asks: Vec<vike_model::Level>,
    /// The newest stamp this LANE carries, in epoch-ms. **DISPLAY AND DIAGNOSTICS ONLY** — §7.4
    /// makes the CLIENT's own receipt authoritative for staleness, because two boxes' clocks differ
    /// and a wire timestamp would make every ladder read permanently stale or falsely live across an
    /// SSH tunnel.
    ///
    /// ⚠ **What it actually IS differs by lane, and §4.4 called it "the venue/feed stamp" for
    /// both.** `LiveDataSink::l2_snapshot` carries a `ts`, so on [`MdFrame::Depth`] this is the
    /// venue/feed stamp. `LiveDataSink::book` carries `Arc<L2Book>` and NOTHING else, and
    /// `crates/vike-model/src/orderbook.rs`'s `L2Book` has `tick_size` and `last_seq` and no time
    /// field at all — so on [`MdFrame::Book`] the only available value is the HUB's own receipt
    /// clock. Kept an `i64` rather than an `Option<i64>` because the field is diagnostic either way
    /// and an option would change the declared field list §12.1's measured envelope was sized over.
    pub venue_ts: i64,
    /// `L2Book::last_seq`, forwarded. **DIAGNOSTIC ONLY**, and this one needs its own warning: the
    /// publisher CONFLATES on a cadence, so consecutive frames legitimately skip venue sequence
    /// numbers by design and a client that tried to gap-detect from them would report a fault every
    /// frame. ⚠ It is also the value that goes into `L2Book::apply_snapshot` — never [`Self::seq`],
    /// which is per-connection accounting and would corrupt any later venue-side reasoning.
    pub venue_seq: u64,
    /// The WIRE sequence for this `(key, lane)` — a `u64` assigned by the publish thread BEFORE any
    /// drop decision, strictly +1 (§7.2). A contiguity break at the client therefore means exactly
    /// one thing: *this connection did not receive frames the server produced.*
    ///
    /// On the BOOK lanes a jump is informational and needs no client action — the lane is conflating
    /// by contract and the frame is a full self-healing snapshot. On [`MdFrame::Trades`] it is the
    /// backstop [`MdFrame::TapeGap`] is checked against.
    pub seq: u64,
}

// ---------------------------------------------------------------------------------------------
// The frame
// ---------------------------------------------------------------------------------------------

/// One PUSHED market-data frame. Rides [`Response::Md`](crate::proto::Response::Md), and appears
/// only on a stream connection (see this module's §0 invariant).
///
/// ⚠ **There is no `Quotes` lane and no `Bars` lane.** `GuiFeedSink::quote` is an explicit no-op —
/// its only consumer is the core's `PriceBoard`, which does not exist in the desktop — so a quotes
/// lane would deliver into nothing, and this repo's standing rule is that a key nothing reads is
/// worse than an unimplemented feature. Bars are refused for a different reason (§10 of the design).
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
        /// The prints, **each with an EMPTY `symbol`**. This variant's own `symbol` field, one line
        /// up, is authoritative for every tick in the batch.
        ///
        /// ⚠ **A CLIENT MUST RE-STAMP BEFORE HANDING A TICK TO ANYTHING THAT READS THE FIELD.**
        /// `vike_model::TradeTick` carries a `symbol: String` that is redundant with the envelope's,
        /// and §12.4's second finding is that the hub holds a symbol-less tick (a 78-character
        /// polymarket token id makes a real tape entry ~142 B rather than 64, which alone breaks
        /// §5.5's memory budget). `crates/vike-datahub/src/md/hub.rs`'s `push_trade` therefore blanks
        /// it on the way IN, and the publisher drains that tape straight onto this wire.
        ///
        /// ⚠ This doc used to say the storage decision *"does NOT change this wire — noted here so
        /// nobody 'fixes' the wire instead."* That is true of the TYPE and false of the VALUES, and
        /// the difference is a silent mis-key: anything downstream that groups by
        /// `TradeTick::symbol` rather than by the envelope — `crates/vike-app-core/src/orderflow.rs`'s
        /// `OrderflowAgg` feed being the case that matters — reads `""` for every print and folds
        /// every venue's tape into one bucket. Re-stamping on the SERVER was the alternative and was
        /// refused: it is a `String` allocation per tick per frame and it puts the redundant bytes
        /// back on the wire, which is 78 B/tick on polymarket against a byte-bounded mailbox
        /// (`crates/vike-datahub/src/md/mod.rs`'s `MD_MAILBOX_BYTES`, one crate up, whose own doc
        /// carries what a maximal tape frame already costs there). The cheap end of that trade is
        /// the client's, and it is one `clone_from` per batch.
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
    /// **Prints were LOST and here is how many.** The primary disclosure for the tape lane: a
    /// silently dropped print permanently corrupts CVD, delta and footprint volume in
    /// `crates/vike-app-core/src/orderflow.rs`'s `OrderflowAgg`, which has no per-trade dedup and no
    /// error channel.
    ///
    /// ⚠ **ORDERING IS CONTRACTUAL: an owed `TapeGap` for a key is written BEFORE that key's next
    /// [`MdFrame::Trades`], never after.** A client that receives the marker AFTER folding the batch
    /// has already discarded good data and kept the corrupted state, which is the opposite of what
    /// the marker is for. The server makes this structural rather than a rule — the writer thread
    /// SYNTHESIZES the gap immediately before the batch it is about to write, from the range its
    /// mailbox recorded.
    ///
    /// ⚠ **SEVERAL HOLES MERGE INTO ONE MARKER, so `dropped` below can exceed what the
    /// range appears to cover.** A subscriber's mailbox owes at most one gap per key, and the server
    /// delivers it at the EARLIEST frame any of the merged holes precedes (never the latest — that
    /// would hand the client batches to fold while prints were already missing from in front of
    /// them). So `dropped` is the TOTAL, the range is the earliest window, and a later hole inside
    /// the same disclosure reaches the client as a [`BookSnapshot::seq`] jump rather than a second
    /// marker. **Treat `dropped` as authoritative for "how much was lost" and the range as "from
    /// where"**; `crates/vike-datahub/src/md/mailbox.rs`'s `Inner::owe` carries the argument.
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

// ---------------------------------------------------------------------------------------------
// The status mirror
// ---------------------------------------------------------------------------------------------

/// The wire MIRROR of [`vike_data::StreamStatus`].
///
/// ⚠ **A mirror rather than the type itself, deliberately.** `crates/vike-data/src/live.rs`'s
/// `StreamStatus` derives `Debug, Clone, Copy, PartialEq, Eq` and NO serde; adding serde to a
/// layer-20 enum in order to serve a wire couples the two, and the precedent for mirroring instead
/// is `crates/vike-tradehub-client/src/wire.rs`'s `WireTradingState`.
///
/// ⚠ **It DIVERGES from that precedent in one way and the divergence is deliberate.**
/// `WireTradingState`'s conversion is a free `fn project_trading_state` in
/// `crates/vike-tradehub/src/publish.rs` — i.e. in the SERVER, one direction only. Here it is a
/// [`From`] pair, because BOTH ends convert (the server on the way out, the client's session on the
/// way in) and both impls are orphan-legal from this crate: `impl From<WireStreamStatus> for
/// StreamStatus` has a local type in the trait's parameter position. Two directions in one place,
/// below both ends, is strictly better than one direction in one of them.
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

// ---------------------------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------------------------

/// Why ONE spec in a subscribe/update was not served.
///
/// ⚠ **PER-SPEC. A whole-REQUEST failure is `Response::Error`, never a refusal list**, and that
/// invariant is what makes this enum coherent. It is also why §4.4's proposed `HubNotMounted`
/// variant is deliberately ABSENT: "this build serves no market-data plane" is uniform across every
/// spec in the request, so under that variant a default build would answer
/// `MdSubscribed { accepted: [], refused: [all] }` and MODE-SWITCH into a heartbeat-only writer
/// that will never send a frame — worse than the refusal it replaces, and it breaks the §0
/// invariant's usefulness. A build with no hub answers `Response::Error` and does not switch, which
/// is byte-for-byte the shape `crates/vike-datahub/src/server.rs`'s `backfill_verb` already uses for
/// an unmounted collector table. The same rule answers an `MdUpdate` naming an unknown or expired
/// session.
///
/// ⚠ **TYPED, not a `String`, because the retry split is already load-bearing on the client.**
/// `crates/vike-app-core/src/feed_lifecycle.rs`'s `FeedRetries::note_error` records
/// `LiveDataError::Unsupported` as `RetryState::Refused` and never retries it, while everything else
/// goes on a backoff ladder. [`Self::is_permanent`] is that split.
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
    /// ⚠ **There is deliberately no `impl From<MdRefusal> for vike_data::LiveDataError`.**
    /// `LiveDataError::Unsupported` carries a `&'static str` and this arrives from the wire as an
    /// owned `String`; the only conversion is `Box::leak`, which is unbounded. A client maps a
    /// permanent refusal onto its OWN `&'static str` — which it gets for free, since it calls
    /// `require_live_verb` locally, the same authority the server used — and carries this text into
    /// the per-venue status string. Say it here or somebody writes the leak.
    LaneUnsupported(String),
    /// The spec's SYMBOL is not a symbol this wire will carry — blank, over
    /// [`MD_MAX_SYMBOL_BYTES`], or carrying an ASCII control byte. The string is
    /// [`validate_md_symbol`]'s OWN message, forwarded verbatim, so the wire's refusal set cannot
    /// drift from the validator BOTH ends call — the same discipline
    /// [`MdRefusal::LaneUnsupported`] applies to the capability matrix one line above.
    ///
    /// ⚠ ONE variant rather than a blank/too-long pair because ONE validator produces all three
    /// messages, and a typed split would put the classification in two places. The message names
    /// the length and the cap and never echoes the symbol — see [`validate_md_symbol`] for why
    /// that matters on precisely this field.
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
    /// `true` for the three CAPABILITY refusals — an unknown venue, a venue this build does not
    /// link, and a lane the venue's declared caps do not serve — and for the VALIDITY refusal
    /// [`MdRefusal::SymbolRejected`], which joins them because the test is the same one: nothing
    /// that happens on this server can make the spec legal later, so a client that kept it in its
    /// desired set would retry an argument rather than a condition. A client records these the way
    /// `FeedRetries` records `Unsupported`: refused, never retried.
    ///
    /// `false` for the three CAPS: a cap can free up when another window closes, so the session
    /// keeps the spec in its DESIRED set and its reconciler retries on backoff.
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
    /// The subscriber accumulated more tape lapses than the server's budget inside its window. *"A
    /// client that cannot keep up is not being served, it is being lied to more slowly"* — and
    /// holding the socket costs a thread and a venue refcount for nothing.
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
    /// ⚠ **It is a DISTINCT reason and not [`Self::SessionIdle`], which is what the writer used to
    /// send.** They are opposite diagnoses — idle means the session asked for nothing, this means it
    /// asked for more than it could read — and the reason is the only thing an operator or a client
    /// has to go on once the socket is gone. Sending the wrong one sends the next reader looking at
    /// an empty subscription set for a problem that is a slow consumer.
    ControlLaneOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// EXHAUSTIVE over [`vike_data::StreamStatus`], and the no-`_` match inside is the load-bearing
    /// half: a round-trip over a hand-listed set proves nothing about a variant nobody added to the
    /// list, so a NEW `StreamStatus` variant must fail to COMPILE here rather than silently never
    /// crossing the wire.
    #[test]
    fn every_stream_status_round_trips() {
        let all = [
            vike_data::StreamStatus::GapStart { at_ts_ms: 1_700_000_000_000 },
            vike_data::StreamStatus::Live { gap_started_ts_ms: None },
            vike_data::StreamStatus::Live { gap_started_ts_ms: Some(1_700_000_000_000) },
            vike_data::StreamStatus::Stale {
                newest_data_ts_ms: 1_700_000_000_000,
                now_ms: 1_700_000_060_000,
            },
        ];
        for s in all {
            // The compile-time completeness guard. NO `_` arm.
            match s {
                vike_data::StreamStatus::GapStart { .. } => (),
                vike_data::StreamStatus::Live { .. } => (),
                vike_data::StreamStatus::Stale { .. } => (),
            }
            let wire = WireStreamStatus::from(s);
            assert_eq!(vike_data::StreamStatus::from(wire), s, "{s:?} did not survive the mirror");
            // ...and through the actual codec, since the mirror only earns its keep if it rides.
            let bytes = serde_json::to_vec(&wire).expect("serialize");
            let back: WireStreamStatus = serde_json::from_slice(&bytes).expect("deserialize");
            assert_eq!(back, wire);
        }
    }

    /// THIS TEST IS THE ARGUMENT FOR THE MANUAL SERDE IMPL. A derived `MdSessionId(u128)` would make
    /// `serde_json::to_value` — the workspace's own wire-shape test idiom — fail with
    /// `NumberOutOfRange` above `u64::MAX`, and would hand `jq` a 39-digit number it parses to
    /// `f64`. Both are silent corruption of an id an operator reads off a log line.
    #[test]
    fn a_session_id_rides_as_hex_not_a_number() {
        let id = MdSessionId(u128::MAX);
        let v =
            serde_json::to_value(id).expect("to_value must not choke — this is the whole point");
        assert!(v.is_string(), "the session id must ride as a STRING, got {v:?}");
        assert_eq!(v.as_str().unwrap(), "ffffffffffffffffffffffffffffffff");
        assert_eq!(v.as_str().unwrap().len(), 32, "always 32 hex chars, zero-padded");
        let back: MdSessionId = serde_json::from_value(v).expect("round trip");
        assert_eq!(back, id);

        // ...and a SMALL id is padded rather than printed short, so an id is one width everywhere.
        assert_eq!(MdSessionId(1).to_string(), "00000000000000000000000000000001");
        assert_eq!(
            serde_json::from_str::<MdSessionId>("\"00000000000000000000000000000001\"").unwrap(),
            MdSessionId(1)
        );
        // A fresh id is not the zero one — the mint is wired to a generator, not to a default.
        assert_ne!(MdSessionId::fresh(), MdSessionId(0));
    }

    /// Depth is NOT part of a key, and two specs differing only in depth name one subscription.
    #[test]
    fn depth_is_not_part_of_a_subscription_key() {
        let a = MdSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            lane: MdLane::Depth,
            depth_levels: Some(200),
        };
        let b = MdSpec { depth_levels: None, ..a.clone() };
        assert_eq!(a.key(), b.key());
        assert_eq!(a.resolved_depth(), MD_DEPTH_LEVELS_CEILING);
        assert_eq!(b.resolved_depth(), MD_DEPTH_LEVELS_DEFAULT);
        // ...and a request ABOVE the ceiling is clamped rather than refused.
        let deep = MdSpec { depth_levels: Some(5_000), ..a.clone() };
        assert_eq!(deep.resolved_depth(), MD_DEPTH_LEVELS_CEILING);
        // A zero is a floor of one, not an empty ladder.
        let zero = MdSpec { depth_levels: Some(0), ..a };
        assert_eq!(zero.resolved_depth(), 1);
    }

    /// A spec with no depth request carries NO `depth_levels` key on the wire (the `Welcome.nonce`
    /// attribute pair), and one with a depth carries it.
    #[test]
    fn an_absent_depth_is_absent_from_the_bytes() {
        let bare = MdSpec {
            venue: "polymarket".into(),
            symbol: "123".into(),
            lane: MdLane::Trades,
            depth_levels: None,
        };
        let s = serde_json::to_string(&bare).unwrap();
        assert!(!s.contains("depth_levels"), "an absent depth must not ride as null: {s}");
        let deep = MdSpec { depth_levels: Some(25), ..bare.clone() };
        assert!(serde_json::to_string(&deep).unwrap().contains("depth_levels"));
        assert_eq!(serde_json::from_str::<MdSpec>(&s).unwrap(), bare);
    }

    /// EXHAUSTIVE over [`MdRefusal`]: every variant is classified, and both classes are non-empty —
    /// a classifier that answered one way for everything would satisfy neither assertion.
    #[test]
    fn a_refusal_classifies_permanently_or_not() {
        let all = [
            MdRefusal::UnknownVenue,
            MdRefusal::VenueNotServed("binance, polymarket".into()),
            MdRefusal::LaneUnsupported("venue's declared VenueCaps.live_data serves no…".into()),
            MdRefusal::SymbolRejected("a subscription symbol of 10000 bytes exceeds…".into()),
            MdRefusal::KeyCapTotal { held: 64, cap: 64 },
            MdRefusal::KeyCapVenue { venue: "binance".into(), held: 16, cap: 16 },
            MdRefusal::SpecCapSession { held: 64, cap: 64 },
        ];
        let permanent = all.iter().filter(|r| r.is_permanent()).count();
        assert_eq!(
            permanent, 4,
            "the three CAPABILITY refusals plus the VALIDITY one are permanent"
        );
        assert_eq!(all.len() - permanent, 3, "...and the three CAPS are not");
        for r in &all {
            // The compile-time completeness guard, mirroring `is_permanent`'s own match.
            match r {
                MdRefusal::UnknownVenue
                | MdRefusal::VenueNotServed(_)
                | MdRefusal::LaneUnsupported(_)
                | MdRefusal::SymbolRejected(_) => assert!(r.is_permanent()),
                MdRefusal::KeyCapTotal { .. }
                | MdRefusal::KeyCapVenue { .. }
                | MdRefusal::SpecCapSession { .. } => assert!(!r.is_permanent()),
            }
            // ...and each one rides the codec.
            let bytes = serde_json::to_vec(r).unwrap();
            assert_eq!(&serde_json::from_slice::<MdRefusal>(&bytes).unwrap(), r);
        }
    }

    /// [`validate_md_symbol`] at its BOUNDARIES, both directions — the shape a length rule goes
    /// wrong in is admitting one byte too many or refusing one byte too few, and neither is visible
    /// from a test that only tries an absurd value.
    #[test]
    fn the_symbol_validator_is_exact_at_both_edges() {
        // The FLOOR that must be admitted: a polymarket CLOB token id is a uint256 in decimal and
        // 2^256-1 is exactly 78 digits, so this is the longest symbol this wire can ever carry.
        assert!(validate_md_symbol(&"7".repeat(78)).is_ok());
        assert!(validate_md_symbol(&"A".repeat(MD_MAX_SYMBOL_BYTES)).is_ok(), "at the bound");
        let over = validate_md_symbol(&"A".repeat(MD_MAX_SYMBOL_BYTES + 1))
            .expect_err("one byte over the bound");
        assert!(over.contains(&(MD_MAX_SYMBOL_BYTES + 1).to_string()), "the length: {over}");
        assert!(over.contains(&MD_MAX_SYMBOL_BYTES.to_string()), "...and the cap: {over}");

        // BLANK, in all three spellings an argument arrives in.
        for blank in ["", " ", "\t\n "] {
            let why = validate_md_symbol(blank).expect_err("blank {blank:?}");
            assert!(why.contains("BLANK"), "{why}");
        }

        // The CONTROL-BYTE rule, which is part of the bound rather than beside it, and its
        // NARROWNESS — the three characters most likely to be swept up with it stay legal, because
        // a venue this plane does not yet serve spells real instruments with them (an IBKR OSI
        // local symbol carries padding spaces).
        assert!(validate_md_symbol("BTC\u{1}USDT").is_err());
        assert!(validate_md_symbol("BTC\u{7f}USDT").is_err(), "DEL is an ASCII control byte");
        assert!(validate_md_symbol("BTC USDT").is_ok(), "a space is not a control byte");
        assert!(validate_md_symbol("BTC\"USDT").is_ok(), "the quote is escaped, not refused");
        assert!(validate_md_symbol("BTC\\USDT").is_ok(), "...nor the backslash");
        assert!(validate_md_symbol("BTC₿").is_ok(), "non-ASCII rides raw through serde_json");

        // ⚠ The bound is in BYTES, not characters, because bytes are what the frame budget is
        // spent in. A multi-byte symbol therefore admits FEWER characters, and that is correct.
        let three_byte = "₿".repeat(MD_MAX_SYMBOL_BYTES / 3);
        assert_eq!(three_byte.len(), MD_MAX_SYMBOL_BYTES, "the fixture must sit ON the bound");
        assert!(validate_md_symbol(&three_byte).is_ok());
        assert!(validate_md_symbol(&format!("{three_byte}a")).is_err(), "one BYTE over");
    }

    /// The lane ↔ feed-label mapping is a round trip, and it names the labels the two INDEPENDENT
    /// producers actually pass (`crates/bridges/binance/src/family/market_feed.rs`'s literal
    /// `"depth"`, polymarket's `PumpMode::as_str`). A silent mismatch here is a whole lane of gap
    /// disclosure that never reaches a client — the DISCLOSURE half of §12.5's forty-day incident.
    #[test]
    fn the_feed_stream_labels_round_trip_and_are_the_producers_own() {
        for lane in [MdLane::Depth, MdLane::Book, MdLane::Trades] {
            assert_eq!(MdLane::from_feed_stream_label(lane.feed_stream_label()), Some(lane));
        }
        assert_eq!(MdLane::Depth.feed_stream_label(), "depth");
        assert_eq!(MdLane::Book.feed_stream_label(), "book");
        assert_eq!(MdLane::Trades.feed_stream_label(), "trades");
        // A label this wire serves no lane for is `None`, never a wrong lane: a bar interval and the
        // quotes lane both arrive here and must be dropped rather than misfiled.
        assert_eq!(MdLane::from_feed_stream_label("1m"), None);
        assert_eq!(MdLane::from_feed_stream_label("quotes"), None);
    }

    /// The lane → `LiveVerb` mapping, which is what makes §7.5's gate a re-read of the declared
    /// capability matrix. The PARTITION it produces on the initial venue set is asserted here rather
    /// than described: five CEX serve Depth and refuse Book, polymarket the reverse.
    #[test]
    fn the_lane_verbs_partition_the_initial_venue_set() {
        assert_eq!(MdLane::Depth.live_verb(), vike_model::LiveVerb::Depth);
        assert_eq!(MdLane::Book.live_verb(), vike_model::LiveVerb::Book);
        assert_eq!(MdLane::Trades.live_verb(), vike_model::LiveVerb::Trades);

        for v in ["binance", "bybit", "okx", "aster", "hyperliquid"] {
            assert!(
                vike_data::require_live_verb(v, MdLane::Depth.live_verb()).is_ok(),
                "{v} must serve the Depth lane"
            );
            assert!(
                vike_data::require_live_verb(v, MdLane::Book.live_verb()).is_err(),
                "{v} declares no lossless book lane — the wire must refuse it"
            );
        }
        assert!(vike_data::require_live_verb("polymarket", MdLane::Book.live_verb()).is_ok());
        assert!(vike_data::require_live_verb("polymarket", MdLane::Depth.live_verb()).is_err());
    }

    /// A whole frame rides the codec — the round trip over the type the wire actually carries,
    /// including the level ORDER contract this type's doc states.
    #[test]
    fn a_book_frame_round_trips_best_first() {
        let snap = BookSnapshot {
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            tick_size: 0.1,
            bids: vec![(100.2, 3.0), (100.1, 5.0)],
            asks: vec![(100.3, 2.0), (100.4, 7.0)],
            venue_ts: 1_700_000_000_000,
            venue_seq: 42,
            seq: 7,
        };
        // Best-first: bids DESCEND, asks ASCEND. `L2Book::top_n`'s own order.
        assert!(snap.bids[0].0 > snap.bids[1].0);
        assert!(snap.asks[0].0 < snap.asks[1].0);
        let frame = MdFrame::Depth(snap.clone());
        let bytes = serde_json::to_vec(&frame).unwrap();
        assert_eq!(serde_json::from_slice::<MdFrame>(&bytes).unwrap(), frame);
        // ...and the two lanes are DISTINCT on the wire even though the payload is identical, which
        // is the whole disclosure property.
        assert_ne!(serde_json::to_vec(&MdFrame::Book(snap)).unwrap(), bytes);
    }
}
