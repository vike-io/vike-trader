//! The blocking, thread-per-connection backtest server.
//!
//! [`serve`] owns a bound [`TcpListener`] and accepts forever; each accepted connection is handled
//! on its own `std::thread` running a read-frame -> handle -> write-frame loop. The store is an
//! `Arc<dyn HistStore + Send + Sync>`, so the server is BACKEND-AGNOSTIC — `DataFusionHist` in
//! production, `MemHistStore` in tests — and every connection thread shares the one store cheaply.
//!
//! Isolation guarantees:
//! - A read error or clean EOF ends ONLY that connection's loop (never the accept loop).
//! - One connection's work runs on its own thread, so even a panic inside a handler cannot take
//!   down the accept loop or another connection — it just drops that one socket.
//! - Every request produces a [`Response`]: a run failure becomes [`Response::Error`], not a
//!   dropped connection, so a client always learns the outcome.
//!
//! # Connection hygiene (PR-2)
//!
//! - **A bad request never drops the connection.** Framing and DECODING are split: the loop reads a
//!   frame's body bytes with [`read_frame_raw`] and decodes them separately, so a well-framed body
//!   that fails to decode into a known [`Request`] (an unknown/incompatible verb from a newer
//!   client) is answered with [`Response::Error`] and the loop CONTINUES — one bad request is a bad
//!   *request*, not a bad *connection*.
//! - **A half-open connection cannot pin a thread forever.** Each accepted stream gets a generous
//!   read timeout ([`IDLE_READ_TIMEOUT`]); a `WouldBlock`/`TimedOut` on an idle or half-open peer
//!   (laptop lid closed, cable pulled) closes the connection and frees the thread. A timeout is
//!   NEVER recovered into a resumed loop (which would desync the stream) — it always closes.
//! - **The `Hello` handshake is optional ON A KEY-LESS SERVER.** [`Request::Hello`] negotiates the
//!   protocol version and returns the served-verb `features`, and a client that sends a normal
//!   request WITHOUT a prior `Hello` is served exactly as before — the handshake informs, it does
//!   not gate. ⚠ That was unconditional until authentication landed; on a KEYED server it is the
//!   opposite (see the next section), and the two arms must not be confused.
//!
//! # Authentication — OPT-IN, and the credential IS the switch (0025's adopting change)
//!
//! [`serve_authed`] takes `Option<NodeKeys>`, and the two arms are the whole contract:
//!
//! - **`None` — key-LESS.** `Hello` is optional and informs rather than gates, and the `Welcome`
//!   bytes are IDENTICAL to the pre-auth protocol (its `nonce` field is `None` and skipped on the
//!   wire, and [`FEATURE_AUTH`] is not advertised). This is the default and it is what [`serve`] /
//!   [`serve_with_backfill`] still do, so every existing local flow — the Studio's
//!   `Backend::Remote`, `vike-cli backtest`, the GUI's store branch,
//!   `vike_datahub_client::RemoteHistStore` — keeps working untouched. The loopback bind guard
//!   below is what holds the posture in that mode.
//!
//!   ⚠ **This arm is no longer "every verb", and this paragraph said so until 2026-09-07.** It read
//!   "the server behaves EXACTLY as it did before authentication existed" and "every verb is served
//!   to whoever can reach the socket"; both were true when written and are now FALSE, because
//!   `crates/vike-datahub/src/server.rs`'s `delete_series_verb` answers [`KEYLESS_DELETE_REFUSAL`] on THIS arm, before the selector is
//!   validated and before the store is touched. `DeleteSeries` is the one verb a key-less server
//!   refuses — `Backfill` is also `Scope::Control` and is still served here, and that asymmetry is
//!   the argument, not an inconsistency: a backfill writes rows a re-fetch restores, a removal takes
//!   the only copy. `docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md` is the record.
//!   The still-true claim is the narrower one now written above: the HANDSHAKE is unchanged and the
//!   `Welcome` bytes are identical.
//! - **`Some(keys)` — KEYED.** `Hello` becomes MANDATORY and must be the FIRST frame; the
//!   `Welcome` answering it carries a fresh 32-byte per-connection nonce; a
//!   [`Request::Auth`] carrying the HMAC over that nonce must follow; and every subsequent verb is
//!   checked against the authenticated [`Scope`]. Any verb before `AuthOk` is refused and the
//!   connection closes.
//!
//! That shape is this workspace's credential-is-the-gate idiom (absent creds ⇒ every venue stays
//! paper), pointed at a server: an operator turns authentication ON by writing
//! `VIKE_DATAHUB_OBSERVE_KEY` / `VIKE_DATAHUB_CONTROL_KEY` into `<project>/settings/secrets.env`,
//! and there is no second switch to forget.
//!
//! # ⚠ This daemon serves the DATA plane ONLY (ruling 7)
//!
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0 split one served
//! surface into two daemons speaking ONE protocol. **The seven COMPUTE verbs left this server** —
//! `RunBacktest`, `RunSlice`, `RunSweep`, `RunWalkforward`, `RunSweepProfile`,
//! `RunWalkforwardProfile` and `ListStrategies` are served by `vike-backend backtest --addr`
//! (`vike_backtest::compute_server`). What stays is the store: the four `HistStore` reads, the four
//! catalog verbs, [`Request::Backfill`] and [`Request::DeleteSeries`].
//!
//! What did NOT change is the protocol: same `Hello`/`Auth`/`Ping` handshake, same length-prefixed
//! `serde_json` frames, same [`Scope`] rules, same bind posture. The classification of a verb onto
//! its plane is `vike_datahub_client::proto`'s `plane_of` — ONE exhaustive match in the crate BELOW
//! both daemons, because a copy in each is how two daemons come to refuse the same verb.
//!
//! A compute verb sent here is answered with `vike_datahub_client::proto`'s `wrong_plane_message`
//! naming the command that serves it, and the connection SURVIVES; the mirror-image refusal on the
//! compute daemon is the same function with the planes swapped. `served_features` withholds the
//! compute strings, so a client that reads the handshake never sends one.
//!
//! ⚠ The ENGINE did not move and never lived here: this crate has always FORWARDED to
//! `vike-backtest`. What moved is which socket answers.
//!
//! `vike_datahub_client::proto`'s `required_scope` is the ONE authority for which verb needs which scope, and its
//! match is EXHAUSTIVE (no `_` arm) so a new wire verb cannot be added unclassified — it fails to
//! compile instead. It lives in `vike-datahub-client` since the split, for the same reason
//! `plane_of` does. The split, and the one part of it that is not obvious:
//!
//! - **`Scope::Observe`** — the history and catalog READS (`LoadBars`, `ScanQuotes`,
//!   `ScanTrades`, `PropertiesAsOf`, `ListSeries`, `Inventory`, `SeriesGaps`, `Coverage`) plus
//!   `Ping`. These answer from the store and change nothing.
//! - **`Scope::Control`** — [`Request::Backfill`], which WRITES the store and spends venue-API
//!   budget from this box's IP, and [`Request::DeleteSeries`], which destroys the only copy. ⚠ This
//!   bullet used to end "**and every `Run*` verb, because they COMPILE CLIENT-SUPPLIED RHAI**" —
//!   that classification is UNCHANGED and still lives in `required_scope`, but the verbs it governs
//!   are no longer answered here, so the Rhai compiler this daemon used to hold is now the compute
//!   daemon's.
//!
//! # ⚠ A verb's ARGUMENTS are validated here too, and for a while one of them was not
//!
//! Authentication says WHO may send a verb; it says nothing about what they may send. Both fields
//! of [`Request::DeleteSeries`] now face a validator at the door of `delete_series_verb`, and only
//! one of them used to: the SELECTOR's rules were enforced (`removal::plan_removal` opens by
//! calling `SeriesSelector::validate_shape`, a method on the shared type) while the provenance
//! ASSERTION's were enforced on neither side of the wire, so a blank one passed the sweep gate and
//! the provenance check together. `delete_series_verb`'s own doc carries what that cost and why
//! refusing it narrows this wire correctly rather than departing from it. The market-data plane's
//! twin of the same rule — a spec field nothing looked at — is `crates/vike-datahub/src/md/hub.rs`'s
//! `MdHub::acquire`.
//!
//! ⚠ **A field's CONTENT and a list's LENGTH are two questions, and the second one is answered
//! here.** Bounding `MdSpec::symbol` at the hub's door says nothing about how many specs a request
//! may carry, and both of this plane's spec-list consumers are loops that CLONE every refusal into
//! a reply — so the refusal path was the expensive one. [`refuse_an_oversized_spec_list`] is the
//! whole-request check that closes it, on `MdSubscribe.specs` and on `MdUpdate`'s `add`/`remove`;
//! its own doc carries the arithmetic and why it derives no new constant.
//!
//! # The outer barrier is still the bind posture, and auth does not replace it
//!
//! The handshake is PLAINTEXT and authenticates the CONNECTION, not each frame: confidentiality
//! and integrity still come from the SSH tunnel (or a VPN) in front of it — 0025's "honest limits
//! of B" is explicit about this, and it is why the bind guard did not soften. The bind target is
//! still CLASSIFIED before the listener opens: `vike_datahub_client::bind`'s `bind_exposure` is the pure classification,
//! `vike_datahub_client::bind`'s `bind_decision` the policy (a non-loopback bind is REFUSED without the named opt-in) — the
//! exact idiom of `vike_tradehub::server`, mirrored — and the bin enforces the verdict.
//!
//! ⚠ **A non-loopback datahub is an AUTHENTICATED one, by construction rather than by advice.**
//! `vike_datahub_client::bind`'s `bind_decision` takes the `vike_datahub_client::bind`'s `ServerAuth` this process is about to serve under, and a
//! `vike_datahub_client::bind`'s `ServerAuth::Keyless` server REFUSES a non-loopback bind even WITH the opt-in
//! (`vike_datahub_client::bind`'s `BindDecision::RefuseUnauthenticated`). That sentence stood in this doc before the guard
//! composed the two knobs, and it was not true then: the opt-in alone bound and served, behind one
//! `warn!`. 0025's "what would reopen this" names a non-loopback need as the TRIGGER for adopting
//! the keys — "an opt-in warning in front of an unauthenticated write verb is the exact state this
//! record exists to prevent" — so the trigger is now enforced instead of documented. LOOPBACK is
//! untouched: a key-less loopback datahub is the ordinary developer configuration.
//!
//! Logging is at CONNECTION boundaries only (open/close/fault, plus the auth verdict), never per
//! frame — this path is not the vike-core hot fold, but per-message logging would still be noise
//! under load.

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use vike_data::removal;
use vike_data::{HistStore, TsRange};
use vike_datahub_client::market::{MD_HEARTBEAT, MdBye, MdFrame, MdSpec};
use vike_datahub_client::node_auth::{self, DATAHUB_DOMAIN, NodeKeys, Scope, fresh_nonce};
use vike_datahub_client::proto::{
    BackfillDone, DeleteDone, FEATURE_AUTH, FEATURE_BACKFILL, FEATURE_COVERAGE,
    FEATURE_DELETE_SERIES, FEATURE_MARKET_DATA, PROTO_VERSION, Plane, Request, Response, VerbScope,
    md_venue_feature, plane_of, read_frame_raw, read_frame_raw_capped, request_kind,
    required_scope, resolve_produced_by, scope_admits, write_frame, wrong_plane_message,
};

use crate::backfill::BackfillTable;
use crate::md::hub::{MdKey, push_attach_frame};
use crate::md::mailbox::{Mailbox, Recv};
use crate::md::{MD_LAPSE_BUDGET, MD_LAPSE_WINDOW, MD_WRITE_TIMEOUT, MdHub, SessionGuard};

/// A generous per-connection read timeout so a half-open or idle peer cannot park a connection
/// thread in `read` for the process lifetime (PR-2).
///
/// Sized for the localhost + SSH-tunnel deployment: a real request body is kilobytes and arrives in
/// milliseconds once its length prefix is seen, so a timeout realistically only fires BETWEEN
/// requests (an idle or half-open connection), which the loop then closes to free the thread. It is
/// deliberately far larger than any legitimate inter-request gap — a value that would clip a slow
/// but live client is worse than a leaked thread — and a timeout is never recovered into a resumed
/// loop (see [`handle_connection`]).
const IDLE_READ_TIMEOUT: Duration = Duration::from_secs(300);

/// Frame ceiling for the PRE-AUTH phase — the `Hello` and the `Auth` an unauthenticated peer sends
/// to a KEYED server. Mirrors `vike_tradehub::server`'s `HANDSHAKE_MAX_FRAME_LEN`, byte for byte
/// and for the same reason.
///
/// The shared `MAX_FRAME_LEN` is 64 MiB because a legitimate datahub *answer* (a chart's worth of
/// bars) can be large — this is the crate that made it 64 MiB. A handshake frame cannot be:
/// `Hello` is a version number and `Auth` is a scope plus a 32-byte mac, together a few hundred
/// bytes even JSON-encoded. Accepting 64 MiB of them means anyone who can reach the socket makes
/// this server allocate 64 MiB per connection by sending FOUR BYTES — no key, no round trip. 64 KiB
/// is ~200x the largest real handshake frame and 1024x cheaper to be wrong about.
///
/// `docs/decisions/0025-datahub-remote-posture.md` names this gap explicitly as a cost route B
/// repeats ("which today has NO pre-auth frame cap at all — its one shared frame ceiling is sized
/// for legitimate large answers"). Post-auth frames keep the full `MAX_FRAME_LEN`: by then the peer
/// has proved possession of a scoped key, and a profile TOML or a Rhai source legitimately runs to
/// kilobytes.
///
/// ⚠ It applies ONLY on a KEYED server. A key-less one has no pre-auth phase at all — every frame
/// is a post-auth frame by definition — so imposing it there would shrink a limit that legitimate
/// traffic already uses, for no security gain behind the loopback guard.
pub const HANDSHAKE_MAX_FRAME_LEN: u32 = 64 * 1024;

/// How long a KEYED server waits for the whole two-frame handshake before closing the connection.
///
/// Deliberately FAR shorter than [`IDLE_READ_TIMEOUT`] (which is 300 s, sized for the gap BETWEEN
/// requests on a live client): an unauthenticated peer has nothing to think about — the `Hello` is
/// a constant and the `Auth` is one HMAC over 32 bytes it was just handed — so a handshake that has
/// not completed in this window is not slow, it is a socket somebody opened and said nothing on.
/// Without it, `IDLE_READ_TIMEOUT` would let an unauthenticated peer park a connection thread for
/// five minutes per socket.
///
/// Applied as the stream's read timeout for the pre-auth phase only, then RESET to
/// [`IDLE_READ_TIMEOUT`] once `AuthOk` is written — so it bounds the handshake without clipping a
/// legitimately idle authenticated client.
pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// How many connections this server handles at once. Beyond it a connection is accepted by the
/// kernel and immediately DROPPED — no frame written, no thread spawned.
///
/// ⚠ **§5.4 of the market-data wire design: *"the accept cap must land whether or not the rest of
/// this does"*.** `serve_authed`'s uncapped `thread::spawn`-per-accept is a pre-existing hazard —
/// `std::thread::spawn` PANICS when the OS refuses a thread, and that panic is on the ACCEPT LOOP,
/// so exhausting it does not degrade this daemon, it ends it. What changed is reachability: before
/// the market-data plane every connection was short-lived, and now a `MdSubscribe` stream holds a
/// thread for as long as a desktop keeps a DOM open. Ordinary use reaches it.
///
/// ⚠ It is NOT [`crate::md::MD_MAX_STREAM_CONNS`] and neither substitutes for the other. That one
/// bounds MARKET-DATA streams (16, one of the two terms of the mailbox memory bound); this one
/// bounds THREADS, and a peer opening ordinary positional connections is invisible to it — which is
/// also what makes §5.5's `64 × 64 MiB` read-buffer arithmetic computable at all.
///
/// 64 is the value `crates/vike-tradehub/src/server.rs`'s `MAX_CONNECTIONS` argues for, adopted
/// verbatim as §5.4 says: the real population here is a desktop or two plus a CLI, so it is roughly
/// two orders of magnitude of headroom over legitimate use while still being a bound.
///
/// ⚠ It gives the accept loop no STOP FLAG, which is the dependency `deploy/vike-datahub.service`'s
/// stop block names: this is a counter and a `continue`, so the loop still polls nothing and that
/// unit's SIGTERM argument is unchanged.
pub const MAX_CONNECTIONS: usize = 64;

// Compile-time bounds on the two pre-auth limits, the tradehub `confirm.rs` idiom: a RANGE, so a
// deliberate tweak stays free while "the bound was effectively removed" and "the bound refuses
// every legitimate handshake" both fail to compile.
const _: () = assert!(
    HANDSHAKE_MAX_FRAME_LEN > 0 && HANDSHAKE_MAX_FRAME_LEN <= 1024 * 1024,
    "HANDSHAKE_MAX_FRAME_LEN must stay far under MAX_FRAME_LEN — an unauthenticated peer must not \
     be able to name a large allocation"
);
const _: () = assert!(
    HANDSHAKE_DEADLINE.as_secs() > 0 && HANDSHAKE_DEADLINE.as_secs() <= 120,
    "HANDSHAKE_DEADLINE must stay a POSITIVE, short bound — it exists to stop an unauthenticated \
     peer parking a connection thread, which a 0 (refuse everything) or a multi-minute value both \
     defeat"
);
const _: () = assert!(
    MAX_CONNECTIONS > 0 && MAX_CONNECTIONS <= 4096,
    "MAX_CONNECTIONS must stay a POSITIVE, bounded number of connection threads"
);
const _: () = assert!(
    MAX_CONNECTIONS > crate::md::MD_MAX_STREAM_CONNS,
    "the market-data stream cap is a SUBSET of the connection cap (§5.4), so a box that filled its \
     stream budget must still have connections left for the ordinary positional verbs — the \
     `MdUpdate` that RELEASES a stream's keys is itself one of them, and a cap that admitted no \
     more connections could not be un-wedged from a client"
);

/// The default listen address — localhost only, reached over an SSH tunnel
/// (`ssh -L 7878:localhost:7878 <box>`), the same posture as the tradehub node server's
/// `DEFAULT_ADDR`. The listener MUST stay bound to loopback: this protocol authenticates NOTHING
/// (see the module doc), so reachability is not defense in depth here — it is the whole barrier.
pub const DEFAULT_ADDR: &str = "127.0.0.1:7878";

/// Accept connections forever, handling each on its own thread over the shared `store`.
///
/// Returns only if `listener.incoming()` yields `None` (it does not for a TCP listener), so in
/// practice this runs for the process lifetime. An accept error is logged and the loop continues —
/// one failed accept never ends the server.
///
/// This entry mounts NO backfill table: `Request::Backfill` is answered with a clean refusal and
/// `Welcome` does not advertise [`FEATURE_BACKFILL`]. The bin's `backfill-serve` build goes
/// through [`serve_with_backfill`] instead.
pub fn serve(listener: TcpListener, store: Arc<dyn HistStore + Send + Sync>) -> io::Result<()> {
    serve_with_backfill(listener, store, None)
}

/// [`serve`] with an optional backfill-on-demand collector table (split-plane REQ-9).
///
/// `Some(table)` serves [`Request::Backfill`] through the table's venue → collector dispatch and
/// advertises [`FEATURE_BACKFILL`] in every `Welcome`; `None` is byte-identical to [`serve`]. The
/// table's entries must write through the SAME store handle `store` wraps (the
/// [`crate::backfill::real_backfill_table`] constructor guarantees it; a test fake owes the same),
/// so a backfilled range is visible to the next read on any connection.
pub fn serve_with_backfill(
    listener: TcpListener,
    store: Arc<dyn HistStore + Send + Sync>,
    backfill: Option<BackfillTable>,
) -> io::Result<()> {
    serve_authed(listener, store, backfill, None, None)
}

/// [`serve_with_backfill`] with OPTIONAL `NodeKeys` authentication — the full entry, and the one
/// the bin calls (`docs/decisions/0025-datahub-remote-posture.md`, the adopting PR).
///
/// `keys`:
/// - **`None`** — byte-identical to [`serve_with_backfill`]. No handshake is required, `Hello` is
///   optional and informs rather than gates, and `Welcome` carries no nonce and does not advertise
///   [`FEATURE_AUTH`]. This is what a server whose credential store holds no datahub keys does.
///   ⚠ Every verb is served on this arm EXCEPT `DeleteSeries`, which `crates/vike-datahub/src/server.rs`'s `delete_series_verb` refuses
///   with [`KEYLESS_DELETE_REFUSAL`] — so this is the pre-auth PROTOCOL unchanged, not the pre-auth
///   verb set. See the module doc, which carries why.
/// - **`Some(keys)`** — every connection MUST complete `Hello` → `Welcome{nonce}` → `Auth{scope,
///   mac}` → `AuthOk` before any verb is answered, and each verb is then checked against
///   `vike_datahub_client::proto`'s `required_scope`. Pre-auth frames are read under [`HANDSHAKE_MAX_FRAME_LEN`] and the whole
///   handshake under [`HANDSHAKE_DEADLINE`].
///
/// ⚠ A `Some(keys)` whose scopes are BOTH empty cannot happen through
/// `vike_datahub_client::node_auth::node_keys_from_vars` (it returns `None` when neither key is
/// configured), and if one is constructed by hand it is a CLOSED server, not an open one: every
/// `Auth` fails against an empty key, which is the credential-is-the-gate idiom's safe direction.
/// ⚠ `md` mounts the LIVE MARKET-DATA plane (`crate::md`). `None` — every entry above, and any
/// build whose operator did not set `VIKE_DATAHUB_LIVE=1` — serves no market-data verb:
/// [`Request::MdSubscribe`] is answered with a clean [`Response::Error`] naming the feature and the
/// key, the connection SURVIVES POSITIONALLY, and `Welcome` advertises neither
/// [`FEATURE_MARKET_DATA`] nor any `md_venue=` entry.
///
/// ⚠ **The parameter's TYPE is feature-free and that is load-bearing.** §8 item 4 put `MdHub` behind
/// `#[cfg(feature = "live-feeds")]`, which cannot work: a cfg'd type in a public signature forces a
/// cfg at every call site AND on the `MdSubscribe` arm below, and cfg-ing that arm away is exactly
/// what leg (3) of [`FEATURE_MARKET_DATA`]'s contract forbids — a server without the plane must
/// still DECODE the verb and refuse it cleanly. `crate::md`'s module doc carries the argument and
/// `crate::backfill::BackfillTable` is the precedent this crate had already set.
pub fn serve_authed(
    listener: TcpListener,
    store: Arc<dyn HistStore + Send + Sync>,
    backfill: Option<BackfillTable>,
    keys: Option<NodeKeys>,
    md: Option<Arc<MdHub>>,
) -> io::Result<()> {
    let backfill = backfill.map(Arc::new);
    let keys = keys.map(Arc::new);
    match keys.as_deref() {
        Some(k) => tracing::info!(
            keys = ?k,
            "vike-datahub: AUTHENTICATION REQUIRED — every connection must complete the NodeKeys \
             handshake before any verb is answered (the `Debug` above reports key PRESENCE only)"
        ),
        None => tracing::info!(
            "vike-datahub: no datahub node keys configured — serving UNAUTHENTICATED (the loopback \
             bind guard is the whole barrier). Set VIKE_DATAHUB_OBSERVE_KEY / \
             VIKE_DATAHUB_CONTROL_KEY in the credential store to require authentication"
        ),
    }
    let live = Arc::new(AtomicUsize::new(0));
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                // Reserve the slot BEFORE spawning and release it from the thread's own `ConnSlot`,
                // so a burst of accepts cannot race the count past the cap and an unwinding
                // connection thread cannot leak one. `crates/vike-tradehub/src/server.rs`'s accept
                // loop, adopted verbatim including the drop-without-a-reply.
                let taken = live.fetch_add(1, Ordering::AcqRel) + 1;
                let slot = ConnSlot(Arc::clone(&live));
                if taken > MAX_CONNECTIONS {
                    // Dropped WITHOUT a reply: writing an error frame would spend a thread and an
                    // allocation on the peer that is already exhausting them, and on a KEYED server
                    // would answer an unauthenticated stranger. `slot`/`stream` drop here.
                    tracing::warn!(
                        peer = ?stream.peer_addr().ok(),
                        live = MAX_CONNECTIONS,
                        "vike-datahub: connection REFUSED — already at MAX_CONNECTIONS; dropping \
                         without a reply"
                    );
                    continue;
                }
                let store = Arc::clone(&store);
                let backfill = backfill.clone();
                let keys = keys.clone();
                let md = md.clone();
                thread::spawn(move || {
                    let _slot = slot;
                    handle_connection(stream, store, backfill, keys.as_deref(), md)
                });
            }
            Err(e) => {
                // A failed accept is per-connection; keep serving.
                tracing::warn!(error = %e, "vike-datahub: accept failed, continuing");
            }
        }
    }
    Ok(())
}

/// Decrements the live-connection count when a connection thread ends — including on a PANIC, which
/// a plain `fetch_sub` at the end of [`handle_connection`] would leak past.
///
/// `crates/vike-tradehub/src/server.rs`'s `ConnSlot`, copied because the two servers may not share a
/// type: both crates declare `layer = 65` and `crates/vike-ops/tests/layer_gate.rs` fails on
/// `to >= from`, the same reason `crate::md::mailbox` re-implements that daemon's `Mailbox`.
struct ConnSlot(Arc<AtomicUsize>);

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The result of the pre-auth phase on a KEYED server.
enum HandshakeOutcome {
    /// The client authenticated under this [`Scope`]; proceed to the session with it as the ceiling.
    Authed(Scope),
    /// Refused (or a transport error) — the connection must close.
    Closed,
}

/// Run `Hello` → `Welcome{nonce}` → `Auth` → verify on a KEYED server. Mirrors
/// `vike_tradehub::server`'s `run_handshake`, including its refusal ordering.
///
/// Both frames are read under [`HANDSHAKE_MAX_FRAME_LEN`] rather than the shared 64 MiB ceiling —
/// an unauthenticated peer must not be able to name a large allocation — and the caller has already
/// armed [`HANDSHAKE_DEADLINE`] as the read timeout, so a peer that opens a socket and says nothing
/// frees its thread in seconds rather than minutes.
///
/// The mac is verified against the REQUESTED scope's key: a scope whose key is ABSENT is refused
/// WITHOUT consulting it (the closed-gate shape), and otherwise `node_auth::verify` decides in
/// constant time.
fn run_handshake(
    stream: &mut TcpStream,
    keys: &NodeKeys,
    features: &[String],
    peer: Option<SocketAddr>,
) -> HandshakeOutcome {
    // Frame 1 — it MUST be Hello. Anything else is an unauthenticated verb: refuse it here. This is
    // where an unauthenticated LoadBars / Backfill / RunSlice is denied and the socket dropped.
    let body = match read_frame_raw_capped(stream, HANDSHAKE_MAX_FRAME_LEN) {
        Ok(b) => b,
        Err(_) => return HandshakeOutcome::Closed,
    };
    let client_version = match serde_json::from_slice::<Request>(&body) {
        Ok(Request::Hello { proto_version }) => proto_version,
        Ok(other) => {
            tracing::info!(
                ?peer,
                verb = request_kind(&other),
                "vike-datahub: verb refused — connection is not authenticated"
            );
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "not authenticated: expected Hello first".into() },
            );
            return HandshakeOutcome::Closed;
        }
        Err(_) => {
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "expected Hello (undecodable request)".into() },
            );
            return HandshakeOutcome::Closed;
        }
    };

    // The challenge. We always answer Hello with our version + a fresh nonce; a client on another
    // protocol version cannot forge a valid mac anyway (the version is SIGNED), so a skew fails
    // cleanly at verify rather than needing a branch here.
    let nonce = fresh_nonce();
    if write_frame(
        stream,
        &Response::Welcome {
            proto_version: PROTO_VERSION,
            features: features.to_vec(),
            nonce: Some(nonce),
        },
    )
    .is_err()
    {
        return HandshakeOutcome::Closed;
    }
    if client_version != PROTO_VERSION {
        tracing::debug!(
            ?peer,
            client_version,
            server_version = PROTO_VERSION,
            "vike-datahub: client protocol version differs; auth will fail on the signed version"
        );
    }

    // Frame 2 — the Auth answer. Still PRE-AUTH, same small ceiling (a scope plus a 32-byte mac).
    let body2 = match read_frame_raw_capped(stream, HANDSHAKE_MAX_FRAME_LEN) {
        Ok(b) => b,
        Err(_) => return HandshakeOutcome::Closed,
    };
    let (scope, mac) = match serde_json::from_slice::<Request>(&body2) {
        Ok(Request::Auth { scope, mac }) => (scope, mac),
        Ok(_) => {
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "expected Auth after Welcome".into() },
            );
            return HandshakeOutcome::Closed;
        }
        Err(_) => {
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "expected Auth (undecodable request)".into() },
            );
            return HandshakeOutcome::Closed;
        }
    };

    // A scope this server holds NO key for is refused without consulting the key — the closed-gate
    // shape. It is also how an observe-only datahub declines control outright rather than letting
    // an empty key decide it by accident.
    if !keys.has(scope) {
        tracing::info!(
            ?peer,
            ?scope,
            "vike-datahub: auth refused — no key configured for this scope on this server"
        );
        let _ = write_frame(
            stream,
            // Deliberately the SAME coarse reason a bad mac gets: distinguishing "no key for that
            // scope" from "wrong key for that scope" tells an unauthenticated peer which scopes
            // this server offers. The operator's log line above carries the real answer.
            &Response::AuthDenied { reason: "bad mac".into() },
        );
        return HandshakeOutcome::Closed;
    }

    // Constant-time verify against the REQUESTED scope's key, under the DATAHUB domain separator —
    // so a tag minted for the tradehub node (which may hold the same key bytes) never verifies here.
    if node_auth::verify(DATAHUB_DOMAIN, keys.key_for(scope), &nonce, PROTO_VERSION, scope, &mac) {
        if write_frame(stream, &Response::AuthOk { scope }).is_err() {
            return HandshakeOutcome::Closed;
        }
        tracing::info!(?peer, ?scope, "vike-datahub: authenticated");
        HandshakeOutcome::Authed(scope)
    } else {
        tracing::info!(?peer, ?scope, "vike-datahub: auth denied (bad mac)");
        let _ = write_frame(stream, &Response::AuthDenied { reason: "bad mac".into() });
        HandshakeOutcome::Closed
    }
}

/// Drive one connection: read requests, answer each, until the peer closes, a read times out, or a
/// transport error ends the loop. Never panics the caller — a fault logs and returns, dropping the
/// socket.
///
/// Framing and decoding are separate (PR-2): the body bytes are read with [`read_frame_raw`] and
/// then decoded, so a well-framed but undecodable request is answered with [`Response::Error`] and
/// the loop CONTINUES — the connection survives a bad request. A read timeout (an idle/half-open
/// peer) or any other read fault CLOSES the connection; a timeout is never recovered into a resumed
/// loop, which would desync the stream.
fn handle_connection(
    mut stream: TcpStream,
    store: Arc<dyn HistStore + Send + Sync>,
    backfill: Option<Arc<BackfillTable>>,
    keys: Option<&NodeKeys>,
    md: Option<Arc<MdHub>>,
) {
    let peer = stream.peer_addr().ok();
    tracing::info!(?peer, "vike-datahub: connection opened");

    let features = served_features(backfill.is_some(), keys.is_some(), md.as_deref());

    // On a KEYED server the PRE-AUTH phase runs first, under a far shorter deadline than the idle
    // timeout below: an unauthenticated peer has nothing to think about, so a handshake that has not
    // completed in seconds is a socket somebody opened and said nothing on.
    //
    // `None` — the key-less server — takes NEITHER branch: no handshake, no deadline, no scope. That
    // is what makes the key-less path byte-identical to the pre-auth protocol rather than merely
    // similar to it.
    let authed: Option<Scope> = match keys {
        Some(keys) => {
            if let Err(e) = stream.set_read_timeout(Some(HANDSHAKE_DEADLINE)) {
                tracing::warn!(?peer, error = %e, "vike-datahub: could not set handshake deadline, closing connection");
                return;
            }
            match run_handshake(&mut stream, keys, &features, peer) {
                HandshakeOutcome::Authed(scope) => Some(scope),
                HandshakeOutcome::Closed => {
                    tracing::info!(?peer, "vike-datahub: connection closed (handshake refused)");
                    return;
                }
            }
        }
        None => None,
    };

    // Bound how long a single read may block, so a half-open peer cannot pin this thread forever. If
    // even setting the timeout fails, close rather than risk an unbounded park. On the keyed path
    // this also RESETS the short handshake deadline — an authenticated client may legitimately idle
    // between requests, exactly like an unauthenticated one always could.
    if let Err(e) = stream.set_read_timeout(Some(IDLE_READ_TIMEOUT)) {
        tracing::warn!(?peer, error = %e, "vike-datahub: could not set read timeout, closing connection");
        return;
    }

    loop {
        // Read the next frame's BODY BYTES only — decode is deliberately separate (below).
        let body = match read_frame_raw(&mut stream) {
            Ok(body) => body,
            Err(e) => {
                match e.kind() {
                    // The normal way a connection ends — no log.
                    io::ErrorKind::UnexpectedEof => {}
                    // No (further) bytes arrived within the window: an idle or half-open connection.
                    // Close it to free the thread; a live client simply reconnects for its next
                    // request. We never RESUME after a timeout — a mid-frame timeout would otherwise
                    // leave the stream desynced.
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                        tracing::info!(
                            ?peer,
                            "vike-datahub: idle read timeout, closing connection"
                        );
                    }
                    _ => {
                        tracing::warn!(?peer, error = %e, "vike-datahub: read fault, closing connection");
                    }
                }
                break;
            }
        };

        // Decode the body. A well-framed body that is not a known `Request` (an unknown/incompatible
        // verb from a mismatched client) is answered with `Response::Error` and the loop CONTINUES —
        // one bad request never drops the connection.
        //
        // ⚠ **THE STEP IS A `ControlFlow`-SHAPED VALUE RATHER THAN A `Response`, and §8 item 10's
        // "ONE arm" understates it.** `MdSubscribe` is this wire's one MODE SWITCH: it must run
        // AFTER the scope check on BOTH the keyed and the key-less arms, and then RETURN without
        // falling through to the unconditional write below — because from that point the socket
        // belongs to `run_market_writer` and the reply it writes is the LAST positional frame.
        let step = match serde_json::from_slice::<Request>(&body) {
            // ⚠ THE PLANE CHECK COMES BEFORE THE SCOPE CHECK, and the order is the whole point of
            // it (ruling 7). A verb this daemon does not serve AT ALL is not a scope question: an
            // Observe connection sending `RunBacktest` would otherwise be told "that requires the
            // Control scope", which is true of the OTHER daemon and useless here — it sends the
            // reader looking for a key when what they need is a different address. Asked first, the
            // answer names where the verb went.
            //
            // It applies on BOTH auth arms (key-less and keyed) because it is a property of this
            // build's served surface, not of the connection.
            Ok(request) if plane_of(&request) == Plane::Compute => {
                let kind = request_kind(&request);
                tracing::info!(
                    ?peer,
                    verb = kind,
                    "vike-datahub: verb refused — it moved to the compute daemon"
                );
                Step::Reply(Response::Error(wrong_plane_message(kind, Plane::Data, Plane::Compute)))
            }
            Ok(request) => match authed {
                // KEYED: check the verb against the connection's authenticated ceiling BEFORE it
                // reaches `handle_request`. A refusal is answered and the loop CONTINUES — an
                // Observe client asking for a Control verb has made a bad *request*, not opened a
                // bad *connection*, and dropping it would make an over-scoped read indistinguishable
                // from a transport fault. (An UNAUTHENTICATED verb is a different matter and is
                // refused with a closed socket, in `run_handshake`.)
                Some(scope) => {
                    let needed = required_scope(&request);
                    if scope_admits(scope, needed) {
                        dispatch(request, &store, backfill.as_deref(), true, md.as_ref())
                    } else {
                        let kind = request_kind(&request);
                        tracing::info!(
                            ?peer,
                            ?scope,
                            ?needed,
                            verb = kind,
                            "vike-datahub: verb refused — outside this connection's scope"
                        );
                        Step::Reply(Response::Error(match needed {
                            VerbScope::Handshake => format!(
                                "{kind} is a handshake frame; this connection is already \
                                 authenticated"
                            ),
                            // ⚠ The `Run*` verbs are NOT named here any more — they are not served
                            // by this daemon at all since ruling 7, and the guard above answers
                            // them before this arm is reached. `Backfill` and `DeleteSeries` are
                            // what is left behind the Control scope on the data plane.
                            _ => format!(
                                "{kind} requires the Control scope; this connection authenticated \
                                 as Observe. The Backfill store WRITE and the DeleteSeries store \
                                 REMOVAL are Control-only"
                            ),
                        }))
                    }
                }
                // KEY-LESS: no scope to check, and unchanged for every verb that predates the
                // keys. ⚠ NOT "every verb", which is what this comment claimed until 2026-09-07:
                // the `false` below IS the `keyed` argument `delete_series_verb` refuses on, so
                // the code this line annotates is what makes the old claim false.
                None => dispatch(request, &store, backfill.as_deref(), false, md.as_ref()),
            },
            Err(e) => {
                Step::Reply(Response::Error(format!("unrecognized/undecodable request: {e}")))
            }
        };

        match step {
            Step::Reply(response) => {
                if let Err(e) = write_frame(&mut stream, &response) {
                    tracing::warn!(?peer, error = %e, "vike-datahub: write fault, closing connection");
                    break;
                }
            }
            // ⚠ THE MODE SWITCH. Everything after this point on this socket is a pushed
            // `Response::Md`, and nothing reads this direction again.
            Step::ModeSwitch(guard, specs) => {
                let hub = md.expect("ModeSwitch is produced only where a hub is mounted");
                run_market_writer(stream, hub, guard, specs, peer);
                return;
            }
        }
    }
    tracing::info!(?peer, "vike-datahub: connection closed");
}

/// The refusal a COMPUTE verb gets here — one call per moved verb, so the seven arms in
/// [`handle_request`] stay explicit while the TEXT has one spelling
/// (`vike_datahub_client::proto`'s `wrong_plane_message`, which the compute daemon's mirror-image
/// refusal also calls).
///
/// ⚠ It takes the verb name as a `&'static str` rather than the `Request` because the arms that
/// call it have already consumed the request by pattern-matching it, and re-deriving the name
/// through `request_kind` would mean either matching twice or borrowing what was moved. The names
/// cannot drift: an arm naming the wrong verb would be a wrong string in a `Response::Error` on a
/// path `crates/vike-datahub/tests/plane_split.rs` drives verb by verb.
fn compute_verb_moved(verb: &'static str) -> Response {
    Response::Error(wrong_plane_message(verb, Plane::Data, Plane::Compute))
}

/// What one decoded request does to the CONNECTION, not just what it answers.
///
/// Exists because [`Request::MdSubscribe`] is this wire's one MODE SWITCH and a plain `Response`
/// cannot express it: the reply must be written and then the socket handed to a writer that never
/// reads again. §8 item 10 calls this "ONE arm before `handle_request`", and it is not — the whole
/// `match` has to yield a `ControlFlow`-shaped value or the unconditional write at the bottom of the
/// loop would write a second positional frame onto a push stream.
enum Step {
    /// Write this and keep reading.
    Reply(Response),
    /// Hand the socket to `run_market_writer` and RETURN.
    ///
    /// ⚠ **The [`SessionGuard`] is TAKEN HERE, before the switch, and that placement is the whole
    /// point of it.** `run_market_writer` used to call `hub.open_session()` itself and, on the
    /// [`crate::md::MD_MAX_STREAM_CONNS`] refusal, write a `Response::Error` and then DROP the
    /// stream — closing a connection whose refusal every doc in this change describes as leaving it
    /// POSITIONAL (`vike_datahub_client::market`'s module doc: *"A server that answers
    /// `Response::Error` … has NOT switched"*; `DatahubClient::md_subscribe` promises the caller a
    /// client "returned intact"). Opening the session in `dispatch` makes that refusal a
    /// [`Step::Reply`] like the hub-less one, on the loop that is still reading.
    ///
    /// It cannot be a check followed by an open — two connections would both pass a check — so the
    /// RESERVATION itself moves, and the guard rides the switch. If it is dropped without being
    /// used, its own `Drop` frees the slot.
    ModeSwitch(SessionGuard, Vec<MdSpec>),
}

/// The refusal a build with NO market-data plane answers [`Request::MdSubscribe`] with.
///
/// ⚠ **It is a [`Response::Error`] and the connection SURVIVES POSITIONALLY — it is deliberately
/// NOT an all-refused `MdSubscribed`.** That is why
/// `vike_datahub_client::market::MdRefusal` carries no `HubNotMounted` variant, and it is a
/// considered deviation from §4.4: "no hub" is a whole-REQUEST condition, uniform across every spec,
/// so expressing it as a PER-SPEC refusal would mode-switch a hub-less build into a heartbeat-only
/// writer that will never send a frame — worse than the refusal it replaces. This shape is
/// byte-for-byte what [`backfill_verb`] already answers for an unmounted collector table, and what
/// leg (3) of every `FEATURE_*` doc in the proto promises.
///
/// A `const` because two things must be able to name it: this refusal, and the test that proves it
/// is the same server refusing on the path a client actually takes.
/// ⚠ **IT LEADS WITH THE ENVIRONMENT VARIABLE, AND IT USED TO LEAD WITH A REBUILD THAT CANNOT BE
/// THE FIX.** The plane is armed by an OPERATOR, not by a build: `crate::datahub_cli`'s hub mount
/// carries no `#[cfg]` at all (that is the whole point of the feature-free hub), so `md.is_none()`
/// here holds if and only if `VIKE_DATAHUB_LIVE != "1"`. And `live-feeds` is an umbrella that gates
/// no code — `git grep 'feature = "live-feeds"'` finds only doc comments — so
/// `--features live-feeds` changes not one byte of the binary. Sending an operator to recompile
/// when the sole cause is an unset variable is the worst kind of accurate-sounding message. The
/// per-venue features govern a DIFFERENT refusal, `MdRefusal::VenueNotServed` /
/// `crates/vike-datahub/src/md/venues.rs`'s `missing_feature`, which is only reachable once this
/// one is not.
pub const NO_MARKET_DATA_PLANE: &str = "this datahub serves no market-data plane. It is armed by an OPERATOR, not by a build: set \
     VIKE_DATAHUB_LIVE=1 on the server and restart it. (A rebuild is a SEPARATE question and only \
     afterwards — `--features live-feeds` alone gates no code, and it is the per-venue \
     `live-<venue>` features, which imply it, that link a venue's feed; a venue this build did not \
     link is then refused BY NAME.) Nothing was subscribed and this connection is unchanged: every \
     other verb still works on it.";

/// Route one decoded request, splitting the MODE SWITCH out from everything else.
///
/// The `md.is_some()` test is what decides between the switch and [`NO_MARKET_DATA_PLANE`], and it
/// is a RUNTIME fact rather than a cfg — the same rule `FEATURE_MARKET_DATA`'s advertisement
/// follows, so "compiled with `live-feeds`" and "armed by an operator" cannot answer differently.
fn dispatch(
    request: Request,
    store: &Arc<dyn HistStore + Send + Sync>,
    backfill: Option<&BackfillTable>,
    keyed: bool,
    md: Option<&Arc<MdHub>>,
) -> Step {
    match request {
        Request::MdSubscribe { specs } => match md {
            // ⚠ ALL THREE whole-REQUEST refusals answer the same way and on the same loop: no hub,
            // an over-length spec list, and no stream-connection slot. See [`Step::ModeSwitch`] for
            // why the session is opened here rather than inside the writer.
            //
            // The ORDER is the `delete_series_verb` idiom: the refusal that is about the SERVER
            // rather than about the request comes first, then the request's own cheapest check,
            // then the one that reserves something.
            Some(hub) => match refuse_an_oversized_spec_list("MdSubscribe", "specs", specs.len()) {
                Some(why) => Step::Reply(Response::Error(why)),
                None => match hub.open_session() {
                    Ok(guard) => Step::ModeSwitch(guard, specs),
                    Err(msg) => Step::Reply(Response::Error(msg)),
                },
            },
            None => Step::Reply(Response::Error(NO_MARKET_DATA_PLANE.to_string())),
        },
        other => Step::Reply(handle_request(other, store, backfill, keyed, md.map(|h| &**h))),
    }
}

/// Map one request to its response. Pure dispatch — the read verbs call the store method directly,
/// mapping `Ok` to the matching typed response and any [`vike_data::DataError`] to
/// [`Response::Error`], so a client always learns the outcome.
///
/// ⚠ Since ruling 7 this daemon serves the DATA plane only: the seven COMPUTE verbs still decode
/// (one schema, two daemons) and are answered by [`compute_verb_moved`]. `handle_connection`
/// refuses them one step earlier, before the scope check, so these arms are the belt to that
/// braces — reached by any future caller of this function that does not run the connection loop's
/// guard first.
fn handle_request(
    request: Request,
    store: &Arc<dyn HistStore + Send + Sync>,
    backfill: Option<&BackfillTable>,
    keyed: bool,
    md: Option<&MdHub>,
) -> Response {
    match request {
        // The OPTIONAL version handshake (PR-2): answer with THIS server's `PROTO_VERSION` and the
        // verbs it serves. It is not a gate — a client may skip it and send a normal request; the
        // CLIENT compares the version and fails loudly on a mismatch (see `DatahubClient::connect`).
        Request::Hello { proto_version: client_version } => {
            tracing::debug!(client_version, "vike-datahub: hello handshake");
            Response::Welcome {
                proto_version: PROTO_VERSION,
                // `false`: this arm is only reached on a KEY-LESS server (a keyed one answers
                // `Hello` inside `run_handshake` and never returns here), so `FEATURE_AUTH` is
                // never advertised from it and `nonce` is `None` — which is exactly what makes the
                // key-less `Welcome` byte-identical to the pre-auth protocol's.
                features: served_features(backfill.is_some(), false, md),
                nonce: None,
            }
        }
        // Only reachable on a KEY-LESS server (a keyed one consumes `Auth` in `run_handshake`).
        // There is nothing to verify it against, so say so rather than pretending: a client that
        // signed a mac deserves to learn its key was never checked, not to be told "ok".
        Request::Auth { .. } => Response::AuthDenied {
            reason: "this datahub has no node keys configured and authenticates nothing; \
                     connect without Auth"
                .into(),
        },
        Request::Ping => Response::Pong,
        // ⚠ THE SEVEN COMPUTE VERBS ARE NO LONGER SERVED HERE — ruling 7 of
        // `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`. They moved to
        // `vike-backend backtest --addr` (`vike_backtest::compute_server`), and these arms are the
        // half of that split which faces a client that has not moved with them.
        //
        // They still DECODE — the `Request` schema lives in the shared client crate and always
        // will, because both daemons speak ONE protocol — so a client that sends one here gets a
        // named `Response::Error` and KEEPS ITS CONNECTION, per this protocol's decode-vs-drop
        // contract. It is a bad *request*, not a bad *connection*: the same peer's `LoadBars` on
        // the next frame still works.
        //
        // ⚠ SEVEN ARMS RATHER THAN ONE `other =>` CATCH-ALL, deliberately, and for the reason
        // `plane_of`'s own match spells out: a catch-all would silently swallow the NEXT verb
        // somebody adds, answering "that moved to the backtest daemon" about a verb that moved
        // nowhere. Written out, a new variant fails to compile here until its author classifies it.
        Request::RunBacktest(_) => compute_verb_moved("RunBacktest"),
        Request::RunSlice { .. } => compute_verb_moved("RunSlice"),
        Request::RunSweep { .. } => compute_verb_moved("RunSweep"),
        Request::RunWalkforward { .. } => compute_verb_moved("RunWalkforward"),
        Request::RunSweepProfile { .. } => compute_verb_moved("RunSweepProfile"),
        Request::RunWalkforwardProfile { .. } => compute_verb_moved("RunWalkforwardProfile"),
        Request::ListStrategies => compute_verb_moved("ListStrategies"),
        Request::LoadBars { venue, symbol, interval, start, end } => {
            match store.load_bars(&venue, &symbol, &interval, TsRange { start, end }) {
                Ok(bars) => Response::Bars(bars),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::ScanQuotes { venue, symbol, start, end } => {
            match store.scan_quotes(&venue, &symbol, TsRange { start, end }) {
                Ok(quotes) => Response::Quotes(quotes),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::ScanTrades { venue, symbol, start, end } => {
            match store.scan_trades(&venue, &symbol, TsRange { start, end }) {
                Ok(trades) => Response::Trades(trades),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::PropertiesAsOf { venue, symbol, ts } => {
            match store.properties_as_of(&venue, &symbol, ts) {
                // Box the payload — `Response::Properties` boxes `SymbolProperties` to keep the enum
                // small (see the proto); serde treats the box transparently on the wire.
                Ok(props) => Response::Properties(props.map(Box::new)),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        // Store-metadata verbs (PR-6). Unlike the `Run*` verbs these need NO `serve-datafusion` split:
        // they call `HistStore` TRAIT methods (the real manifest walk on a DataFusion backend, the
        // in-memory catalog fold on the `MemHistStore` double; a store WITHOUT the catalog verbs
        // refuses, and that refusal rides `Response::Error` like any other store error rather than
        // being served as a fabricated empty catalog), so a lean build compiles + answers them
        // without pulling DataFusion. The catalog is tiny (no Parquet scan), so shipping it whole
        // is safe.
        Request::ListSeries => match store.list_series() {
            Ok(series) => Response::SeriesList(series),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::Inventory => match store.inventory() {
            Ok(inv) => Response::Inventory(inv),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::SeriesGaps { id } => match store.series_gaps(&id) {
            Ok(gaps) => Response::SeriesGaps(gaps),
            Err(e) => Response::Error(e.to_string()),
        },
        // The cross-kind coverage report (split-plane spec §6 Q2) — the FOURTH store-metadata verb,
        // and served exactly like its three siblings above: a `HistStore` TRAIT call, so no
        // `serve-datafusion` split and no second store handle. That the trait carries it is the
        // whole reason this arm is one line: had the report stayed a concrete `DataFusionHist`
        // method, serving it would have meant threading a SECOND, concrete store through `serve`
        // (the `BackfillTable` shape) purely to reach a manifest fold the trait can express.
        Request::Coverage => match store.coverage_report() {
            Ok(report) => Response::Coverage(report),
            Err(e) => Response::Error(e.to_string()),
        },
        // Backfill-on-demand (split-plane REQ-9). The arm always DECODES (the schema lives in the
        // client crate) so a mismatched client is never dropped; whether it SERVES depends on the
        // mounted table — `None` (a default or plain `serve-datafusion` build) is a clean refusal
        // naming the missing feature, the recorder's `missing_feature` idiom.
        Request::Backfill { venue, symbol, interval, start, end } => {
            backfill_verb(&venue, &symbol, &interval, start, end, backfill, store)
        }
        // The DESTRUCTIVE verb. `keyed` is the FIRST thing it looks at — see `delete_series_verb`.
        Request::DeleteSeries { selector, produced_by, dry_run } => {
            delete_series_verb(&selector, produced_by.as_deref(), dry_run, keyed, store)
        }
        // ⚠ THE BELT TO `handle_connection`'s BRACES. `dispatch` intercepts `MdSubscribe` before
        // this function is reached, so this arm is for any FUTURE caller that does not run the
        // connection loop's guard first — the same reasoning the `compute_verb_moved` arms above
        // already carry. It must never mode-switch anything: this path has no socket to hand over.
        Request::MdSubscribe { .. } => Response::Error(
            "MdSubscribe is a connection MODE SWITCH and is handled by the connection loop; this \n             path cannot answer it"
                .into(),
        ),
        // The registry mutation. It runs on an ORDINARY short-lived connection; the FRAMES it
        // affects go to the stream connection that owns the session.
        Request::MdUpdate { session, add, remove } => md_update_verb(session, &add, &remove, md),
    }
}

/// The WHOLE-REQUEST length bound on a spec list, the market-data plane's twin of
/// `delete_series_verb`'s door check — refused before anything is opened, planned or cloned.
///
/// # ⚠ What was unbounded, and why the per-spec cap did not bound it
///
/// [`crate::md::MD_MAX_SPECS_PER_SESSION`] is a cap on the keys a session HOLDS:
/// [`MdHub::acquire`] refuses the 65th and the caller keeps going. Nothing bounded the LENGTH of
/// the `Vec<MdSpec>` a client sent, and both consumers of one are loops that CLONE every refusal
/// into a reply — `run_market_writer` over `specs`, [`MdHub::update`] over `add` and `remove`.
/// A post-auth body is read at [`vike_datahub_client::proto::MAX_FRAME_LEN`] (64 MiB) against a
/// spec that costs tens of bytes, so one legal frame carries on the order of a million of them, and
/// `write_frame` materialises the whole reply before it compares it to that same ceiling. Refusing
/// cost more than accepting, which is backwards, and `docs/decisions/0052`'s decision 2 — *"a
/// subscription's cost is bounded by server constants … never by the request"* — was false on
/// exactly this term while every OTHER term of it was true.
///
/// # Why it reuses the session cap rather than deriving a new constant
///
/// A request may not usefully name more keys than a session could hold even if every one were
/// accepted, so the already-derived number is the honest ceiling — and an UNCALIBRATED constant is
/// a flake (`CLAUDE.md`, and `MD_MAX_SYMBOL_BYTES`'s own derivation next door). The cost is that a
/// list padded with DUPLICATES past the count is refused although re-`acquire` would have no-op'd
/// them; that is accepted, and the message names both numbers so the sender sees the rule it met.
///
/// `Some(message)` is the refusal; `None` means the length is fine.
fn refuse_an_oversized_spec_list(verb: &str, field: &str, len: usize) -> Option<String> {
    let cap = crate::md::MD_MAX_SPECS_PER_SESSION;
    (len > cap as usize).then(|| {
        format!(
            "{verb}: `{field}` carries {len} specs, over MD_MAX_SPECS_PER_SESSION = {cap} — the \
             most keys ONE session may hold. Nothing was opened and nothing was changed. A list \
             this long cannot be satisfied even if every spec were valid: the surplus would be \
             refused one by one and every refusal CLONED into the reply, so the request is refused \
             whole instead. Send at most {cap}, and drop any duplicate spec — a key named twice \
             asks for nothing the first mention did not."
        )
    })
}

/// The `MdUpdate` verb: mutate one session's subscription set and report what changed.
///
/// ⚠ **An unknown or expired `session` is a [`Response::Error`], not a refusal list** — the same
/// per-spec-versus-whole-request rule [`NO_MARKET_DATA_PLANE`] carries. It is also the check that
/// stops one desktop mutating another's subscription set on a shared authenticated channel: the id
/// names a set, it authorizes nothing, and the connection's `Scope` remains the ceiling.
///
/// ⚠ **[`refuse_an_oversized_spec_list`] runs BEFORE that session lookup**, on `add` and on
/// `remove` alike: the length is refusable without knowing whose session it is, and the lookup
/// takes a lock. A message naming the session rather than the cap is the shape that says the check
/// ran too late, which is what `crates/vike-datahub-client/tests/market_data_negotiation.rs`'s
/// `an_over_length_update_list_is_refused_before_the_session_is_looked_up` sends a FICTIONAL
/// session to prove.
fn md_update_verb(
    session: vike_datahub_client::market::MdSessionId,
    add: &[MdSpec],
    remove: &[MdSpec],
    md: Option<&MdHub>,
) -> Response {
    let Some(hub) = md else {
        return Response::Error(NO_MARKET_DATA_PLANE.to_string());
    };
    // AT THE DOOR, before the session lookup takes a lock: BOTH lists, each naming its own field,
    // because `update` loops and clones over both. `remove` is bounded by the same number for the
    // same reason — a session holds at most that many keys, so a longer removal list names keys it
    // cannot be holding.
    for (field, len) in [("add", add.len()), ("remove", remove.len())] {
        if let Some(why) = refuse_an_oversized_spec_list("MdUpdate", field, len) {
            return Response::Error(why);
        }
    }
    if !hub.has_session(session) {
        return Response::Error(format!(
            "market data: no such session `{session}` on this server — it was never opened here, or \n             its stream connection has ended. Re-open a stream with MdSubscribe; nothing was changed."
        ));
    }
    let (accepted, refused, released) = hub.update(session, add, remove, vike_model::now_ms());
    Response::MdUpdated { accepted, refused, released }
}

/// The message a KEY-LESS server answers `DeleteSeries` with. A `const` because two things must be
/// able to name it: this refusal, and the test that proves it is the same server refusing on the
/// path the client actually takes.
pub const KEYLESS_DELETE_REFUSAL: &str = "this datahub holds no node keys, so it serves no delete verb at all. A key-less server \
     authenticates nothing — its handshake informs, it does not gate — so `Scope::Control` would be \
     a word nothing enforces, and an irreversible verb behind a word is the state \
     docs/decisions/0025-datahub-remote-posture.md exists to prevent. Configure \
     VIKE_DATAHUB_OBSERVE_KEY / VIKE_DATAHUB_CONTROL_KEY on this server, or run the delete on the \
     box with `vike-cli data rm --store DIR`.";

/// The `DeleteSeries` verb: refuse on a key-less server, plan, assert, then delete.
///
/// # ⚠ The key-less refusal is FIRST, and it is unconditional
///
/// Before the selector is validated, before the store is touched, before anything is reported. This
/// is the narrowing `docs/decisions/0025-datahub-remote-posture.md`'s argument demands and does not
/// itself state: 0025 records that the BACKFILL write verb is why authentication must come at the
/// first non-loopback need, and its reason 1 — *"A surface with a write verb and no way to say
/// 'reads yes, writes no' cannot be handed even a trusted LAN"* — applies with more force to a verb
/// that destroys the only copy. The answer here is stronger than 0025 asked for and in the same
/// direction: the key-less DEFAULT does not get a weaker version of this verb, it gets none.
///
/// `served_features` refuses to advertise it too, so a well-behaved client never sends one; this is
/// the half that holds for a client that does.
///
/// # ⚠ The provenance filter is RESOLVED before the sweep gate reads it
///
/// **This is a NARROWING of a signed-off wire, and it is deliberate.** Until 2026-09-11 the raw
/// `produced_by` spelling went straight to [`removal::plan_removal`] and the sweep gate below asked
/// only whether it was `None`. A BLANK value is `Some("")`, which is not `None` — so it satisfied
/// the require-provenance-before-a-wildcard-delete gate, and then satisfied the provenance check
/// too, VACUOUSLY: `crates/vike-data/src/store_kind.rs`'s `key_matches_prefix` is `starts_with`,
/// every key starts with the empty string, `RemovalPlan::verdict` found no foreign key in any
/// series, and `crates/vike-data/src/datafusion_hist.rs`'s `delete_series_checked` re-check under
/// the series lock — the TOCTOU guard the whole verb turns on — passed for the same reason. **Two
/// guards and a re-check fell to one token, on the verb that takes the only copy.**
///
/// A server that accepts a wildcard delete wearing an assertion is not honouring §5.4 of
/// `docs/superpowers/specs/2026-09-07-cli-data-rm-design.md`; it is failing to implement it. So the
/// spelling is resolved through `vike_datahub_client::proto`'s `resolve_produced_by` — the ONE
/// validator, re-exported beside the removal vocabulary precisely so both ends of this wire share
/// one definition — at the DOOR: before the plan, before `inventory()`, before a single
/// `series_commits` read.
///
/// Two consequences worth stating rather than discovering:
///
/// * **It is unconditional** — sweep or fully-named, `dry_run` or not. A blank assertion is
///   meaningless on a named series too, and it is actively harmful there: it flips a keyless series
///   from "deletable, provenance none recorded" to "refused". And refusing a DRY RUN matters because
///   the dry-run arm below returns the plan without consulting `RemovalPlan::verdict` — so a blank
///   dry run used to render `provenance: SATISFIED`, and the MCP surface's `preview_token` binds to
///   exactly that plan.
/// * **A producer PATH now RESOLVES here** rather than being asserted literally. That is the second
///   acceptance change and it closes a defect `crates/vike-cli/src/cmd/data.rs`'s
///   `refuse_a_producer_path_on_the_remote_route` had to paper over: the same command line answered
///   two ways depending on `--store` versus `--addr`. An UNDECLARED path is refused by name (it
///   fails closed and deletes nothing) instead of matching no key and reporting the operator's data
///   as foreign.
///
/// The KEY-LESS refusal stays unconditionally FIRST — see above; it is about the SERVER, not about
/// the request, and `crates/vike-datahub/tests/auth_roundtrip.rs`'s
/// `a_keyless_server_serves_no_delete_verb` pins it.
///
/// # The sweep rule
///
/// A SWEEP (any wildcarded dimension) must carry `produced_by`, and the gate now reads a RESOLVED
/// value. A fully-named single series need
/// not — that is byte-for-byte the act the Data Manager's Delete already performs behind a confirm
/// modal, and requiring more of one surface than of another for the identical operation is a rule
/// nobody keeps. ⚠ The MCP surface requires it unconditionally on ITS side, which is a decision
/// about the CALLER rather than about the store, and so does not live here.
fn delete_series_verb(
    selector: &removal::SeriesSelector,
    produced_by: Option<&str>,
    dry_run: bool,
    keyed: bool,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Response {
    if !keyed {
        return Response::Error(KEYLESS_DELETE_REFUSAL.to_string());
    }
    // AT THE DOOR: nothing is planned, no series is enumerated and no provenance is read until the
    // argument itself is known to be an assertion. The resolver's message quotes the spelling back
    // and says what is wrong with it, which is what an operator who typed something needs.
    let produced_by = match produced_by.map(resolve_produced_by) {
        Some(Ok(prefix)) => Some(prefix),
        Some(Err(why)) => return Response::Error(format!("delete_series: {why}")),
        None => None,
    };
    let produced_by = produced_by.as_deref();
    if selector.is_sweep() && produced_by.is_none() {
        return Response::Error(format!(
            "refusing a SWEEP with no provenance assertion: {} matches more than one series, and \
             deleting by name alone is what `produced_by` exists to replace. Name every dimension, \
             or pass the commit-key prefix the rows carry.",
            selector.describe()
        ));
    }
    let plan = match removal::plan_removal(store.as_ref(), selector, produced_by) {
        Ok(p) => p,
        Err(e) => return Response::Error(e.to_string()),
    };
    if dry_run {
        return Response::Deleted(DeleteDone { plan, outcome: None });
    }
    match removal::execute_removal(store.as_ref(), &plan) {
        // A partial failure rides the OUTCOME, not an error: the series that went are gone, and a
        // caller that was told only "it failed" would not know which.
        Ok(outcome) => Response::Deleted(DeleteDone { plan, outcome: Some(outcome) }),
        // A provenance refusal deleted NOTHING, so it is an error rather than an outcome — the
        // request did not happen.
        Err(e) => Response::Error(e.to_string()),
    }
}

/// The `Backfill` verb: validate the bounded range, dispatch venue → collector through the mounted
/// [`BackfillTable`], then read the range BACK through the served store handle — the write-through
/// proof — and answer [`Response::BackfillDone`]. Every failure (no table, unknown venue, inverted
/// range, collector error, read-back error) is a clean [`Response::Error`], so the client always
/// learns the outcome; a partial `BackfillDone` is never sent.
fn backfill_verb(
    venue: &str,
    symbol: &str,
    interval: &str,
    start: i64,
    end: i64,
    table: Option<&BackfillTable>,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Response {
    let Some(table) = table else {
        return Response::Error(format!(
            "backfill: not compiled into this build — rebuild vike-datahub with \
             `--features backfill-serve`. (It is a Cargo feature because the collectors are \
             heavy: they pull the venue bridge crates into this binary.) The `{FEATURE_BACKFILL}` \
             capability is deliberately absent from this server's Welcome.features."
        ));
    };
    if start > end {
        return Response::Error(format!(
            "backfill: inverted range — start {start} > end {end} (v1 takes one bounded \
             inclusive epoch-ms range per request)"
        ));
    }
    let Some(collector) = table.get(venue) else {
        return Response::Error(format!(
            "backfill: venue `{venue}` has no collector in this build. Supported: [{}]",
            table.supported().join(", ")
        ));
    };
    // Collector runs INLINE (v1 is synchronous per request; the collectors page + pace
    // internally), writing through the same store this server serves.
    let rows_written = match collector(symbol, interval, start, end) {
        Ok(n) => n as u64,
        Err(e) => {
            return Response::Error(format!("backfill {venue}/{symbol}@{interval} failed: {e}"));
        }
    };
    // Write-through proof + the client's seam bookkeeping: what the requested range now holds,
    // read through the SERVED handle — the same read the client's follow-up LoadBars would do.
    match store.load_bars(venue, symbol, interval, TsRange { start: Some(start), end: Some(end) }) {
        Ok(bars) => Response::BackfillDone(BackfillDone {
            rows_written,
            first_ts: bars.first().map(|b| b.ts),
            last_ts: bars.last().map(|b| b.ts),
        }),
        Err(e) => Response::Error(format!(
            "backfill {venue}/{symbol}@{interval}: collector wrote {rows_written} rows but the \
             read-back failed: {e}"
        )),
    }
}

/// The verbs THIS server answers, advertised in the [`Response::Welcome`] handshake. Kept in sync
/// with the arms of [`handle_request`]: the four `HistStore` read verbs the `RemoteHistStore` seam
/// consumes, and the four store-metadata verbs beside them
/// (`list_series`/`inventory`/`series_gaps`/[`FEATURE_COVERAGE`]).
///
/// ⚠ **The compute strings are GONE, and their absence is the negotiation** (ruling 7 of
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`). This list used to open
/// with `backtest` and carry `list_strategies`, `run_sweep_profile`, `run_walkforward_profile` and
/// — on a `serve-datafusion` build — `run_slice`/`run_sweep`/`run_walkforward`. Those seven verbs
/// are served by `vike-backend backtest --addr` now, and a well-behaved client that reads this list
/// learns so at the HANDSHAKE, before it ships a profile it would only be refused for. A client
/// that sends one anyway still gets the named refusal
/// (`vike_datahub_client::proto`'s `wrong_plane_message`) rather than a drop — the advertisement is
/// what stops the send, the refusal is what holds for a client that does not read it, and neither
/// substitutes for the other. This is the same three-legged shape [`FEATURE_BACKFILL`] and
/// [`FEATURE_DELETE_SERIES`] already use, applied to a REMOVAL rather than an addition.
///
/// `has_backfill` is a RUNTIME fact, not a cfg: [`FEATURE_BACKFILL`] is advertised exactly when a
/// collector table is MOUNTED, because "compiled with `backfill-serve`" and "can actually serve a
/// backfill" are the same thing only when the bin wired a table in — a `serve()` entry inside a
/// `backfill-serve` test build still must not advertise what it will refuse.
fn served_features(has_backfill: bool, requires_auth: bool, md: Option<&MdHub>) -> Vec<String> {
    let mut features = vec![
        "load_bars".to_string(),
        "scan_quotes".to_string(),
        "scan_trades".to_string(),
        "properties_as_of".to_string(),
        // PR-6 store-metadata verbs — served on EVERY build (HistStore trait methods, no DataFusion).
        "list_series".to_string(),
        "inventory".to_string(),
        "series_gaps".to_string(),
        // Their §6-Q2 sibling, likewise a trait verb on every build. UNCONDITIONAL, unlike
        // `backfill` below: there is no table to mount and nothing runtime about it, so the only
        // question its advertisement answers is "is this server older than the verb" — which is
        // precisely what the GUI's Partial column needs in order to choose between the wire answer
        // and an honest note.
        FEATURE_COVERAGE.to_string(),
    ];
    // The backfill-on-demand verb — advertised per MOUNTED TABLE, not per cfg (see the doc above).
    // This advertisement is the verb's whole negotiation: it shipped without a PROTO_VERSION bump.
    if has_backfill {
        features.push(FEATURE_BACKFILL.to_string());
    }
    // The MARKET-DATA plane, advertised per MOUNTED HUB — a RUNTIME fact exactly like `backfill`'s
    // per-mounted-table rule, NOT a build fact like `coverage`. A binary carrying `live-feeds` whose
    // operator did not set `VIKE_DATAHUB_LIVE=1` mounts no hub and advertises nothing, which is what
    // keeps "can serve" and "will serve" one answer.
    //
    // ⚠ The per-venue entries ride BESIDE the named capability rather than replacing it, and they are
    // collision-safe by construction: every capability check in this protocol family is whole-string
    // equality, so an `md_venue=binance` entry can neither satisfy nor shadow a named capability.
    // They are what lets a client learn at the HANDSHAKE which venues it may name, instead of one
    // `VenueNotServed` refusal at a time.
    if let Some(hub) = md {
        features.push(FEATURE_MARKET_DATA.to_string());
        for venue in hub.served_venues() {
            features.push(md_venue_feature(venue));
        }
    }
    // ⚠ The AUTH advertisement, and it must be LAST-but-conditional in exactly this way: a key-less
    // server never pushes it, so its whole `Welcome` — features included — is byte-identical to the
    // pre-auth protocol's. That identity is the backward-compatibility contract, not a nicety, and
    // `a_keyless_servers_welcome_is_byte_identical_to_the_pre_auth_protocol` is what holds it.
    // On a KEYED server this string is how a client learns authentication is mandatory here BEFORE
    // it sends a verb it would only be refused for.
    if requires_auth {
        features.push(FEATURE_AUTH.to_string());
        // ⚠ The DESTRUCTIVE verb rides the SAME conditional, and that pairing is the whole posture:
        // it is served exactly when the server can enforce a scope, and never otherwise. A key-less
        // server therefore does not advertise it AND refuses it (`delete_series_verb`) — the
        // advertisement is what stops a well-behaved client sending one, and the refusal is what
        // holds for a client that does. Neither substitutes for the other.
        features.push(FEATURE_DELETE_SERIES.to_string());
    }
    features
}

/// **The market-data stream writer** — everything this socket does after
/// [`Response::MdSubscribed`] is written.
///
/// It resolves or refuses each spec, writes the ONE positional reply, arms the socket for pushing,
/// hands each accepted key its status-then-snapshot (§5.2 step 5), and then drains its own mailbox
/// until the peer goes away. **It never reads this connection again**, which is the invariant
/// `vike_datahub_client::market`'s module doc states and the reason no correlation id is needed.
///
/// # ⚠ What it arms, and why the ACCOUNT plane could not
///
/// - **`set_write_timeout`([`MD_WRITE_TIMEOUT`]).** `crates/vike-tradehub/src/server.rs`'s
///   `run_push_writer` tolerates an indefinite park because its writer holds nothing; here a parked
///   writer holds a REFCOUNT on a venue socket for a client that is gone. A timed-out `write_all`
///   can leave the stream desynced mid-frame, which is harmless precisely because the only response
///   to any write fault is to CLOSE. This is the explicit disconnect-the-laggard policy.
/// - **`set_nodelay(true)`.** This module sets `set_read_timeout` and never `set_nodelay`, and this
///   is the one place in the workspace where a socket carrying small latency-sensitive frames is
///   otherwise left Nagle-coalesced — every venue socket goes through
///   `crates/vike-bridge-core/src/ws.rs`'s `configure_ws_stream` precisely to avoid that.
///
/// # ⚠ The `TapeGap` is SYNTHESIZED HERE, not dequeued
///
/// `crate::md::mailbox` records the owed range and hands it back beside the frame it precedes, so
/// the "a gap is written BEFORE the next `Trades` batch, never after" contract is STRUCTURAL rather
/// than a rule somebody has to keep. The one cost, stated rather than left for a reviewer to find:
/// this thread does a serialization `run_push_writer` never does. It is bounded — it happens only
/// when a drop actually occurred — and a client accumulating enough gaps for it to matter is already
/// inside [`MD_LAPSE_BUDGET`]'s disconnect.
///
/// Logging stays at CONNECTION BOUNDARIES: no per-frame line, and no payload in any line.
fn run_market_writer(
    mut stream: TcpStream,
    hub: Arc<MdHub>,
    guard: SessionGuard,
    specs: Vec<MdSpec>,
    peer: Option<SocketAddr>,
) {
    // ⚠ THE GUARD IS OWNED HERE AND DROPPED LAST. Its `Drop` releases every key this session held —
    // `Drop` rather than an explicit call at the end of this function so that a PANIC here also
    // releases, mirroring `crates/vike-tradehub/src/publish.rs`'s `Subscription`. A release written
    // as the last statement is correct on every ordinary return path and leaks a venue refcount
    // forever on the panic path, and a leaked nonzero refcount is never reaped.
    //
    // ⚠ It is taken by `dispatch` rather than here, and that is not a tidy-up: opening it here made
    // the MD_MAX_STREAM_CONNS refusal a write-then-drop of the socket, on the one path whose whole
    // contract is that a `Response::Error` leaves the connection positional. See `Step::ModeSwitch`.
    let session = guard.id();
    let mailbox: Arc<Mailbox> = Arc::clone(guard.mailbox());

    let mut accepted = Vec::new();
    let mut refused = Vec::new();
    for spec in &specs {
        match hub.acquire(session, spec) {
            Ok(s) => accepted.push(s),
            Err(r) => refused.push((spec.clone(), r)),
        }
    }
    tracing::info!(
        ?peer,
        %session,
        accepted = accepted.len(),
        refused = refused.len(),
        "vike-datahub md: stream opened"
    );

    // THE LAST POSITIONAL FRAME.
    let reply = Response::MdSubscribed {
        session,
        accepted: accepted.clone(),
        refused,
        heartbeat_ms: MD_HEARTBEAT.as_millis() as u64,
    };
    if write_frame(&mut stream, &reply).is_err() {
        return;
    }
    if stream.set_write_timeout(Some(MD_WRITE_TIMEOUT)).is_err() {
        return;
    }
    // A failure here is not fatal — Nagle costs latency, not correctness — so it is logged and the
    // stream serves on, rather than refusing a subscription over a socket option.
    if let Err(e) = stream.set_nodelay(true) {
        tracing::warn!(?peer, error = %e, "vike-datahub md: TCP_NODELAY refused; frames may coalesce");
    }

    // §5.2 step 5: per accepted key, its current Status and — only if that status is Live — its
    // current book. `attach_frames` enforces the order; this loop only delivers it.
    let now = vike_model::now_ms();
    for spec in &accepted {
        let key = MdKey::of(spec);
        for frame in hub.attach_frames(&key, now) {
            push_attach_frame(&mailbox, &key, frame);
        }
    }

    let mut lapses: u64 = 0;
    let mut window_start = Instant::now();
    let mut last_write = Instant::now();
    loop {
        if mailbox.must_close() {
            // The CTRL lane overflowed: a client that cannot absorb the status frames of its own
            // keys is dead, and pretending otherwise hands it a frozen ladder it believes is live.
            //
            // ⚠ The reason used to be `MdBye::SessionIdle`, whose own doc is "the session held no
            // specs for long enough that keeping the socket bought nothing" — the OPPOSITE
            // diagnosis, and the only thing anyone has to go on once the socket is gone.
            let _ = write_md(&mut stream, MdFrame::Bye(MdBye::ControlLaneOverflow));
            tracing::info!(?peer, %session, "vike-datahub md: stream closed (control lane overflow)");
            break;
        }
        if window_start.elapsed() >= MD_LAPSE_WINDOW {
            lapses = 0;
            window_start = Instant::now();
        }
        match mailbox.recv_timeout(MD_HEARTBEAT) {
            Recv::Frame { bytes, owed_gap } => {
                if let Some((key, dropped, from_seq, to_seq)) = owed_gap {
                    lapses = lapses.saturating_add(1);
                    let gap = MdFrame::TapeGap {
                        venue: key.venue.clone(),
                        symbol: key.symbol.clone(),
                        dropped,
                        from_seq,
                        to_seq,
                    };
                    if write_md(&mut stream, gap).is_err() {
                        break;
                    }
                    if lapses > MD_LAPSE_BUDGET {
                        let _ = write_md(&mut stream, MdFrame::Bye(MdBye::TooSlow { lapses }));
                        tracing::info!(
                            ?peer,
                            %session,
                            lapses,
                            "vike-datahub md: stream closed (too slow — tape lapses over budget)"
                        );
                        break;
                    }
                }
                if write_all_flush(&mut stream, &bytes).is_err() {
                    tracing::info!(?peer, %session, "vike-datahub md: stream closed (write fault)");
                    break;
                }
                last_write = Instant::now();
            }
            Recv::Timeout => {
                if last_write.elapsed() >= MD_HEARTBEAT {
                    if write_md(&mut stream, MdFrame::Heartbeat).is_err() {
                        break;
                    }
                    last_write = Instant::now();
                }
            }
            Recv::Closed => break,
        }
    }
    drop(guard);
    tracing::info!(?peer, %session, "vike-datahub md: stream ended");
}

/// Frame one [`MdFrame`] and write it — the per-SUBSCRIBER path (a heartbeat, a `Bye`, a
/// writer-synthesized `TapeGap`), as against the publisher's serialize-once fan-out.
fn write_md(stream: &mut TcpStream, frame: MdFrame) -> io::Result<()> {
    write_frame(stream, &Response::Md(Box::new(frame)))
}

/// Write pre-framed bytes and flush — `crates/vike-tradehub/src/server.rs`'s `write_all_flush`
/// shape. The bytes already carry their length prefix and already passed `MAX_FRAME_LEN`, because
/// the publisher framed them through the same `write_frame` every other frame goes through.
fn write_all_flush(stream: &mut TcpStream, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    stream.write_all(bytes)?;
    stream.flush()
}
