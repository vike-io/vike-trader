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
//!   dropped connection, so a client always learns the outcome. ⚠ **One exception, and it is not a
//!   failure:** a `Backfill` whose client CLOSED the connection while it ran is stopped at the next
//!   chunk boundary and answered by closing without a reply, because nobody is there to read one —
//!   see `StopProbe` and `backfill_verb`. A `Backfill` an OPERATOR cancelled from another connection
//!   (`Request::CancelBackfill`) is not that exception: its client is still there, and is answered.
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
//!   `crates/vike-datahub/src/server/delete.rs`'s `delete_series_verb` answers [`KEYLESS_DELETE_REFUSAL`] on THIS arm, before the selector is
//!   validated and before the store is touched. `DeleteSeries` is the one verb a key-less server
//!   refuses — `Backfill` is also `Scope::Write` and is still served here, and that asymmetry is
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
//! `VIKE_DATAHUB_OBSERVE_KEY` / `VIKE_DATAHUB_CONTROL_KEY` into the node-key store (`vike-cli
//! datahub setup` mints and writes the pair), and there is no second switch to forget.
//!
//! # ⚠ This daemon serves the DATA plane ONLY (ruling 7)
//!
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0 split one served
//! surface into two daemons speaking ONE protocol. **The COMPUTE verbs left this server** —
//! `RunBacktest`, `RunSlice`, `RunParamscan`, `RunWalkforward`, `RunParamscanProfile`,
//! `RunWalkforwardProfile`, `ListStrategies` and (ruling R1's research plane, added later)
//! `RunStudy` are served by `vike-backend backtest --addr`
//! (`vike_backtest::compute_server`). What stays is the store: the `HistStore` reads, the four
//! catalog verbs, [`Request::Backfill`] and [`Request::DeleteSeries`].
//!
//! ⚠ This said "the FOUR `HistStore` reads" and the count went stale the moment
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md` added six more. No number replaces
//! it: `handle_request`'s arms are the roster, and `served_features` is what a client is told.
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
//! - **`Scope::Read`** — the history and catalog READS (`LoadBars`, `ScanQuotes`,
//!   `ScanTrades`, `PropertiesAsOf`, `ListSeries`, `Inventory`, `SeriesGaps`, `Coverage`) plus
//!   `Ping`. These answer from the store and change nothing. `ListBackfills` joins them from the
//!   other side: it answers from the backfill registry, not the store, and changes nothing either
//!   (`docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`).
//! - **`Scope::Write`** — [`Request::Backfill`], which WRITES the store and spends venue-API
//!   budget from this box's IP, and [`Request::DeleteSeries`], which destroys the only copy. ⚠ This
//!   bullet used to end "**and every `Run*` verb, because they COMPILE CLIENT-SUPPLIED RHAI**" —
//!   that classification is UNCHANGED and still lives in `required_scope`, but the verbs it governs
//!   are no longer answered here, so the Rhai compiler this daemon used to hold is now the compute
//!   daemon's. `CancelBackfill` sits here too: it writes nothing, but it stops work another
//!   connection started, and like `Backfill` — unlike `DeleteSeries` — a key-less loopback server
//!   serves it (0101).
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
//! twin of the same rule — a spec field nothing looked at — is
//! `crates/vike-datahub/src/md/hub/subscribe.rs`'s `MdHub::acquire`.
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

use std::cell::Cell;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use vike_catalog::{CatalogAvailability, catalog_availability};
use vike_data::store::removal;
use vike_data::{HistStore, TsRange};
use vike_datahub_client::MdBye;
use vike_datahub_client::catalog::{
    CATALOG_MAX_INSTRUMENTS, CatalogListing, CatalogOutcome, CatalogRefusal, validate_catalog_venue,
};
use vike_datahub_client::market::{MD_HEARTBEAT, MdFrame, MdSpec};
use vike_datahub_client::proto::{
    BACKFILL_ONE_BATCH, DeleteDone, FEATURE_BACKFILL_CANCEL, FEATURE_HISTORY_CHANNELS, Plane,
    Request, Response, SeedDone, VerbScope, plane_of, read_frame_raw, read_frame_raw_capped,
    request_kind, required_scope, resolve_produced_by, scope_admits, write_frame,
    wrong_plane_message,
};
use vike_datahub_client::seed::{seed_range, validate_seed_interval, validate_seed_symbol};
use vike_datahub_client::{
    BackfillDone, DATA_PLANE_SENTINEL, FEATURE_AUTH, FEATURE_BACKFILL, FEATURE_BACKFILL_FUNDING,
    FEATURE_COVERAGE, FEATURE_DELETE_SERIES, FEATURE_MARKET_DATA, FEATURE_SCAN_BOOK_UPDATES,
    FEATURE_SCAN_COHORT, FEATURE_SCAN_DEPTH, FEATURE_SCAN_EQUITY, FEATURE_SCAN_EXEC_FILLS,
    FEATURE_SCAN_LIMIT, FEATURE_SCAN_PERP_METRICS, FEATURE_SEED_CLASS, FEATURE_SEED_SERIES,
    FEATURE_SERIES_FACTS, FEATURE_VENUE_CATALOG, PROTO_VERSION, md_venue_feature,
};
use vike_node_proto::auth::{self, DATAHUB_DOMAIN, NodeKeys, Scope, fresh_nonce};

use crate::backfill::BackfillTable;
use crate::catalog::{CatalogAdmission, CatalogLane};
use crate::import::ImportLane;
use crate::md::hub::{MdKey, push_attach_frame};
use crate::md::mailbox::{Mailbox, Recv};
use crate::md::{MD_LAPSE_BUDGET, MD_LAPSE_WINDOW, MD_WRITE_TIMEOUT, MdHub, SessionGuard};
use crate::seed::{SeedAdmission, SeedLane};

// The verb FAMILIES, one child file each — `handle_request` below is the router and routes into
// them. What stays in this file is the connection (accept loop, handshake, `handle_connection`,
// `dispatch`, `StopProbe`), the public constants and ceilings other crates name by this path, the
// Welcome advertisement (`served_features`) and the tests.
mod backfill;
mod delete;
mod market_data;
mod range_reads;
mod seed_series;
mod venue_catalog;

use backfill::{backfill_verb, cancel_backfill_verb, no_backfill_registry};
use delete::delete_series_verb;
use market_data::{md_update_verb, refuse_an_oversized_spec_list, run_market_writer};
use range_reads::{
    LOAD_BARS, SCAN_BOOK_UPDATES, SCAN_COHORT, SCAN_DEPTH, SCAN_EQUITY, SCAN_PERP_METRICS,
    SCAN_QUOTES, SCAN_TRADES, book_stored_rows, exec_fills_verb, range_verb, row_count,
};
use seed_series::seed_series_verb;
use venue_catalog::venue_catalog_verb;

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
///
/// Every production entry passes this constant; [`serve_authed_with_handshake_deadline`] is the one
/// that takes another, so `crates/vike-datahub/tests/auth_roundtrip.rs` can watch the deadline fire
/// in half a second rather than ten.
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

/// The most bars ONE `Request::LoadBars` reply may carry — and so the most this server READS for
/// one. A request with no `limit` over a range holding more is refused BY NAME; a `limit` above it
/// is clamped to it. `C` in `docs/superpowers/specs/2026-10-01-loadbars-bounded-read-design.md`.
///
/// ⚠ **This exists because the client's RANGE used to set this daemon's allocation.** The verb
/// loaded the whole requested range and cut the reply to the `limit` afterwards, so a request with
/// no `limit` over a whole `5s` FX series decoded on the order of 6 GB of rows and then serialised
/// them to JSON — beside the market-data and recorder planes, under one memory cap — before
/// `write_frame` found the body over [`vike_datahub_client::proto::MAX_FRAME_LEN`] and dropped the
/// connection. Now the store stops reading at this many rows (`vike_data::HistStore`'s
/// `load_bars_head`), and a range that holds more is answered with a named error on a connection
/// that stays open.
///
/// ⚠ **The value is DERIVED from the frame, so that it refuses nothing that works today** — 520,223
/// at the 64 MiB frame. No real bar is shorter on the wire than [`MIN_BAR_JSON_BYTES`], so no reply
/// of more than this many bars fits [`vike_datahub_client::proto::MAX_FRAME_LEN`], whatever the
/// prices: every request this refuses already failed at `write_frame`, after the full read. The
/// compile-time assertions below hold the derivation, and
/// `crates/vike-datahub/tests/load_bars_bounded.rs`'s
/// `the_smallest_real_bar_is_no_shorter_than_the_ceiling_assumes` measures the per-bar figure with
/// serde rather than trusting it. A LOWER value would bound memory harder and refuse replies that
/// fit a frame today — the owner's ruling (Q2 of that design) was this one.
///
/// ⚠ **It bounds ONE request, not the daemon.** [`MAX_CONNECTIONS`] requests each near this
/// ceiling, with their JSON frames, can still add up past the unit's memory cap; a daemon-wide
/// bound is its own change (Q3 of that design). Read at the decoded rate the design measured
/// (about 173 bytes a bar), this is about 90 MB of rows per request.
pub const LOAD_BARS_CEILING: usize =
    vike_datahub_client::proto::MAX_FRAME_LEN as usize / MIN_BAR_JSON_BYTES;

/// The fewest JSON bytes one REAL bar takes inside a `Response::Bars` frame, its separating comma
/// included — the input to [`LOAD_BARS_CEILING`]'s derivation.
///
/// A real bar has a 13-digit epoch-ms `ts` (every instant since 2001-09-09), five `f64`s that
/// serde_json writes in at least three characters each (`0.0`; a NaN is `null`, four), and four
/// optional fields that are `null` at their shortest:
/// `{"ts":1000000000000,"open":0.0,"high":0.0,"low":0.0,"close":0.0,"volume":0.0,"funding":null,`
/// `"bid":null,"ask":null,"symbol":null}` is 128 bytes, and its comma makes 129. A fixture whose
/// `ts` counts from zero is shorter, and is not a real bar.
pub const MIN_BAR_JSON_BYTES: usize = 129;

// [`LOAD_BARS_CEILING`]'s derivation, asserted rather than trusted — the
// `crates/vike-datahub-client/src/named_run.rs` idiom. The FIRST is the property the owner ruled
// for: one bar past the ceiling, each at its shortest, must already overrun the frame, so the
// ceiling can only refuse a reply that could not have been sent — it holds by construction for the
// division above and is what fails if that line becomes a hand-picked number that is too low. The
// SECOND is a RANGE, the tradehub `confirm.rs` idiom: a change to the frame or to the per-bar floor
// moves this ceiling, and with it the most rows one request makes this daemon read, so one that
// moves it far is a decision to re-derive here rather than a side effect.
const _: () = assert!(
    (LOAD_BARS_CEILING as u64 + 1) * MIN_BAR_JSON_BYTES as u64
        > vike_datahub_client::proto::MAX_FRAME_LEN as u64,
    "LOAD_BARS_CEILING must refuse NOTHING a frame could carry: one bar past it, each bar at \
     MIN_BAR_JSON_BYTES, must already overrun MAX_FRAME_LEN"
);
const _: () = assert!(
    LOAD_BARS_CEILING >= 100_000 && LOAD_BARS_CEILING <= 1_000_000,
    "LOAD_BARS_CEILING left the range it was derived in (about 520,000 bars, about 90 MB of decoded \
     rows per request) — re-derive it from MAX_FRAME_LEN and MIN_BAR_JSON_BYTES on its doc"
);

// ---- the per-kind ceilings of every other range read ---------------------------------------------
//
// [`LOAD_BARS_CEILING`]'s shape, once per row kind the scan verbs answer with: the most rows one
// reply can carry, derived as `MAX_FRAME_LEN / MIN_<KIND>_JSON_BYTES`, so — the owner's ruling for
// bars, carried over unchanged (`docs/superpowers/specs/2026-10-02-remaining-whole-range-reads-design.md`,
// section 2) — each refuses NOTHING that fits a frame today. A request with a `limit` reads
// `min(limit, ceiling)` rows; one with no `limit` reads `ceiling + 1` and is refused BY NAME above
// the ceiling, where it used to read the client's whole range and fail at `write_frame`.
//
// ⚠ **A ceiling bounds ROWS, and a reply under it can still pass the frame** — every row of it
// wider than the minimum the ceiling was derived from, which real rows always are. `range_verb`'s
// byte cap (`fit_to_frame`) closes that band: it counts the reply's exact JSON and cuts it to a
// shorter whole-`ts` page, or refuses it by name when no `limit` was sent.
//
// ⚠ **Each minimum is the shortest REAL row, and "shortest" is per FIELD, not "every optional field
// `null`".** serde_json spells `Some(0.0)` in three bytes and `None` in four, and `Some("")` in two,
// so a row is shortest with its `Option<f64>`s at `0.0`, its strings EMPTY, its integers `0` and its
// booleans `true` — except `ts`, which is a real epoch-ms instant and so at least 13 digits (every
// instant since 2001-09-09, [`MIN_BAR_JSON_BYTES`]'s argument). An over-estimated minimum gives a
// ceiling too LOW, which refuses replies that fit; that is the direction this derivation must
// never err in. `crates/vike-datahub/tests/scan_ceilings.rs`'s
// `the_smallest_real_row_of_every_kind_is_no_shorter_than_its_ceiling_assumes` measures every figure
// below with the encoder `write_frame` uses, and tries the longer spelling of every field against it.

/// The fewest JSON bytes one real `QuoteTick` takes inside a `Response::Quotes` frame, comma
/// included: `{"ts":1000000000000,"local_ts":0,"bid":0.0,"ask":0.0,"bid_size":0.0,"ask_size":0.0,`
/// `"symbol":""}` is 95. The symbol is re-injected from the request and is never empty in practice;
/// counting it empty is the safe direction.
pub const MIN_QUOTE_JSON_BYTES: usize = 96;
/// The most quotes one `Request::ScanQuotes` reply may carry, and so the most it reads.
pub const SCAN_QUOTES_CEILING: usize = ceiling_for(MIN_QUOTE_JSON_BYTES);

/// One real `TradeTick`, comma included: `{"ts":1000000000000,"local_ts":0,"price":0.0,"size":0.0,`
/// `"is_buyer_maker":true,"symbol":""}` is 90 (`true` is the shorter boolean).
pub const MIN_TRADE_JSON_BYTES: usize = 91;
/// The most trades one `Request::ScanTrades` reply may carry, and so the most it reads.
pub const SCAN_TRADES_CEILING: usize = ceiling_for(MIN_TRADE_JSON_BYTES);

/// One price LEVEL of a `BookUpdate`, comma included: `[0.0,0.0]` is 9 — a level is a two-element
/// array on the wire (`vike_marketdata::orderbook`'s `BookLevel` serialises through a tuple).
///
/// ⚠ **The book ceiling counts STORED rows, and a stored row is a level** — the unit the store's
/// budget counts (`vike_data`'s book codec writes one row per level, and ONE placeholder row for an
/// event with none). So the floor is a level's cost, not an event's: an event of `k` levels costs at
/// least `k` of these plus its own fields, and an event of none costs far more than one.
pub const MIN_BOOK_LEVEL_JSON_BYTES: usize = 10;
/// The most stored book rows (levels) one `Request::ScanBookUpdates` or `Request::ScanDepth` reply
/// may carry, and so the most either reads — the two verbs answer with the same row type.
pub const SCAN_BOOK_LEVELS_CEILING: usize = ceiling_for(MIN_BOOK_LEVEL_JSON_BYTES);

/// One real `CohortRow`, comma included: `{"ts":1000000000000,"asset":"","axis":"","cohort":"",`
/// `"grading":"","label_basis":"","long_usd":0.0,"total_usd":0.0}` is 114.
pub const MIN_COHORT_JSON_BYTES: usize = 115;
/// The most cohort rows one `Request::ScanCohort` reply may carry, and so the most it reads.
pub const SCAN_COHORT_CEILING: usize = ceiling_for(MIN_COHORT_JSON_BYTES);

/// One real `PerpMetricRow`, comma included: `{"ts":1000000000000,"premium":0.0,"open_interest":0.0}`
/// is 54 — `Some(0.0)` is one byte shorter than the `null` of a row without open interest.
pub const MIN_PERP_METRIC_JSON_BYTES: usize = 55;
/// The most perp-metric rows one `Request::ScanPerpMetrics` reply may carry, and so the most it reads.
pub const SCAN_PERP_METRICS_CEILING: usize = ceiling_for(MIN_PERP_METRIC_JSON_BYTES);

/// One real `EquitySample`, comma included: `{"ts":1000000000000,"venue":"","equity":0.0,`
/// `"realized":0.0,"unrealized":0.0,"missing_prices":0}` is 95.
pub const MIN_EQUITY_JSON_BYTES: usize = 96;
/// The most equity samples one `Request::ScanEquity` reply may carry, and so the most it reads.
pub const SCAN_EQUITY_CEILING: usize = ceiling_for(MIN_EQUITY_JSON_BYTES);

/// One real `ExecFillRow`, comma included: `{"ts":1000000000000,"trade_id":"","client_order_id":"",`
/// `"venue":"","symbol":"","side":0,"qty":0.0,"px":0.0,"commission":0.0,"mark_price":0.0,`
/// `"liquidity_side":"","commission_asset":""}` is 182.
pub const MIN_EXEC_FILL_JSON_BYTES: usize = 183;
/// The most fills one `Request::ScanExecFills` reply may carry, and so the most it reads.
///
/// ⚠ **This verb cannot be paged past it.** The variant carries no range and no `limit`, so a series
/// holding more is refused whole; such a series cannot fit a frame today either, so the refusal
/// takes nothing away. The way past it — a range and a limit on the variant — is a wire change with
/// a capability string, deferred until a series nears this ceiling (the design's Q1).
pub const SCAN_EXEC_FILLS_CEILING: usize = ceiling_for(MIN_EXEC_FILL_JSON_BYTES);

/// `MAX_FRAME_LEN / min_row_bytes` — the one derivation every ceiling above shares.
const fn ceiling_for(min_row_bytes: usize) -> usize {
    vike_datahub_client::proto::MAX_FRAME_LEN as usize / min_row_bytes
}

/// One row past `ceiling`, each at `min_row_bytes`, overruns the frame — the property the owner
/// ruled for, asserted for every kind below rather than trusted.
const fn refuses_nothing_that_fits(ceiling: usize, min_row_bytes: usize) -> bool {
    (ceiling as u64 + 1) * min_row_bytes as u64 > vike_datahub_client::proto::MAX_FRAME_LEN as u64
}

// Every ceiling refuses nothing that fits (the FIRST group), and every ceiling stays in the range
// it was derived in (the SECOND): a change to the frame or to a per-row floor moves the most rows
// one request makes this daemon read, so one that moves it far is a decision to re-derive above
// rather than a side effect. The book's range is wider because its unit is a level, not an event.
const _: () = assert!(
    refuses_nothing_that_fits(SCAN_QUOTES_CEILING, MIN_QUOTE_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_TRADES_CEILING, MIN_TRADE_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_BOOK_LEVELS_CEILING, MIN_BOOK_LEVEL_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_COHORT_CEILING, MIN_COHORT_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_PERP_METRICS_CEILING, MIN_PERP_METRIC_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_EQUITY_CEILING, MIN_EQUITY_JSON_BYTES)
        && refuses_nothing_that_fits(SCAN_EXEC_FILLS_CEILING, MIN_EXEC_FILL_JSON_BYTES),
    "every scan ceiling must refuse NOTHING a frame could carry: one row past it, each row at its \
     kind's minimum, must already overrun MAX_FRAME_LEN"
);
const _: () = assert!(
    SCAN_QUOTES_CEILING >= 100_000
        && SCAN_QUOTES_CEILING <= 1_000_000
        && SCAN_TRADES_CEILING >= 100_000
        && SCAN_TRADES_CEILING <= 1_000_000
        && SCAN_COHORT_CEILING >= 100_000
        && SCAN_COHORT_CEILING <= 1_000_000
        && SCAN_PERP_METRICS_CEILING >= 1_000_000
        && SCAN_PERP_METRICS_CEILING <= 2_000_000
        && SCAN_EQUITY_CEILING >= 100_000
        && SCAN_EQUITY_CEILING <= 1_000_000
        && SCAN_EXEC_FILLS_CEILING >= 100_000
        && SCAN_EXEC_FILLS_CEILING <= 1_000_000
        && SCAN_BOOK_LEVELS_CEILING >= 1_000_000
        && SCAN_BOOK_LEVELS_CEILING <= 10_000_000,
    "a scan ceiling left the range it was derived in — re-derive it from MAX_FRAME_LEN and its \
     kind's minimum on the constants above"
);

/// Every range read's ceiling, as one value a server carries — [`ReadCeilings::PRODUCTION`] in every
/// entry a daemon runs, a small injected set in [`serve_with_read_ceilings`], so a test can drive
/// each over-ceiling refusal through the real verb with a handful of rows. Each field is the ceiling
/// of the verb it names; `book_levels` serves both `ScanBookUpdates` and `ScanDepth`.
///
/// ⚠ A ceiling of zero is not "nothing": the store reads a zero budget as NO budget. Every field is
/// at least one, and the production values are asserted to be far above it.
///
/// `frame_bytes` is the BYTE ceiling every one of those replies is cut to (`fit_to_frame`):
/// `MAX_FRAME_LEN` in production, injectable for the same reason the row ceilings are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadCeilings {
    pub bars: usize,
    pub quotes: usize,
    pub trades: usize,
    pub book_levels: usize,
    pub cohort: usize,
    pub perp_metrics: usize,
    pub equity: usize,
    pub exec_fills: usize,
    pub frame_bytes: usize,
}

impl ReadCeilings {
    /// The ceilings every production entry serves: [`LOAD_BARS_CEILING`], the seven above, and the
    /// frame `write_frame` refuses to pass.
    pub const PRODUCTION: Self = Self {
        bars: LOAD_BARS_CEILING,
        quotes: SCAN_QUOTES_CEILING,
        trades: SCAN_TRADES_CEILING,
        book_levels: SCAN_BOOK_LEVELS_CEILING,
        cohort: SCAN_COHORT_CEILING,
        perp_metrics: SCAN_PERP_METRICS_CEILING,
        equity: SCAN_EQUITY_CEILING,
        exec_fills: SCAN_EXEC_FILLS_CEILING,
        frame_bytes: vike_datahub_client::proto::MAX_FRAME_LEN as usize,
    };
}

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
/// `crate::backfill::real_backfill_table` constructor guarantees it; a test fake owes the same),
/// so a backfilled range is visible to the next read on any connection.
pub fn serve_with_backfill(
    listener: TcpListener,
    store: Arc<dyn HistStore + Send + Sync>,
    backfill: Option<BackfillTable>,
) -> io::Result<()> {
    serve_authed(listener, store, backfill, None, None, None, None)
}

/// [`serve_with_backfill`] with OPTIONAL `NodeKeys` authentication — the full entry, and the one
/// the bin calls (`docs/decisions/0025-datahub-remote-posture.md`, the adopting PR).
///
/// `keys`:
/// - **`None`** — byte-identical to [`serve_with_backfill`]. No handshake is required, `Hello` is
///   optional and informs rather than gates, and `Welcome` carries no nonce and does not advertise
///   [`FEATURE_AUTH`]. This is what a server whose credential store holds no datahub keys does.
///   ⚠ Every verb is served on this arm EXCEPT `DeleteSeries`, which `crates/vike-datahub/src/server/delete.rs`'s `delete_series_verb` refuses
///   with [`KEYLESS_DELETE_REFUSAL`] — so this is the pre-auth PROTOCOL unchanged, not the pre-auth
///   verb set. See the module doc, which carries why.
/// - **`Some(keys)`** — every connection MUST complete `Hello` → `Welcome{nonce}` → `Auth{scope,
///   mac}` → `AuthOk` before any verb is answered, and each verb is then checked against
///   `vike_datahub_client::proto`'s `required_scope`. Pre-auth frames are read under [`HANDSHAKE_MAX_FRAME_LEN`] and the whole
///   handshake under [`HANDSHAKE_DEADLINE`].
///
/// ⚠ A `Some(keys)` whose scopes are BOTH empty cannot happen through
/// `vike_node_proto::auth::node_keys_from_vars` (it returns `None` when neither key is
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
    seed: Option<Arc<SeedLane>>,
    catalog: Option<Arc<CatalogLane>>,
) -> io::Result<()> {
    serve_with_import(listener, store, backfill, keys, md, seed, catalog, None)
}

/// [`serve_authed`] with the ARCHIVE IMPORT lane as an eighth argument — the entry the bin calls.
///
/// `import`:
/// - **`None`** — byte-identical to [`serve_authed`]: `Welcome` advertises neither
///   `archive_import` nor any `import_format=` entry, and `Request::ImportArchive` is answered with
///   `crate::import::NO_IMPORT_LANE` on a connection that survives.
/// - **`Some(lane)`** — the verb is served through `crate::import::import_archive_verb` (Control
///   scope on a KEYED server, served on a key-less loopback one exactly as `Backfill` is —
///   `docs/decisions/0100`'s verdict 1), and `Welcome` advertises the capability TOGETHER with one
///   `import_format=<id>` per registered format.
///
/// ⚠ A separate entry rather than an eighth parameter on [`serve_authed`] because that function has
/// callers across four crates' test suites, and every one of them mounts no import lane — the
/// argument would be a `None` spelled at each of them for nothing.
#[allow(clippy::too_many_arguments)] // `serve_authed`'s seven, plus the import lane
pub fn serve_with_import(
    listener: TcpListener,
    store: Arc<dyn HistStore + Send + Sync>,
    backfill: Option<BackfillTable>,
    keys: Option<NodeKeys>,
    md: Option<Arc<MdHub>>,
    seed: Option<Arc<SeedLane>>,
    catalog: Option<Arc<CatalogLane>>,
    import: Option<Arc<ImportLane>>,
) -> io::Result<()> {
    serve_inner(
        listener,
        store,
        backfill,
        keys,
        md,
        seed,
        catalog,
        import,
        ReadCeilings::PRODUCTION,
        HANDSHAKE_DEADLINE,
    )
}

/// [`serve`] with every range read's ceiling as a PARAMETER instead of [`ReadCeilings::PRODUCTION`].
///
/// Every production entry passes the constants; this one exists so a test can drive each
/// over-ceiling refusal through the real verb on loopback with a handful of rows rather than half a
/// million. Key-less, with no backfill table, market-data hub, seed lane or catalog lane — exactly
/// [`serve`] in every other respect.
pub fn serve_with_read_ceilings(
    listener: TcpListener,
    store: Arc<dyn HistStore + Send + Sync>,
    ceilings: ReadCeilings,
) -> io::Result<()> {
    serve_inner(listener, store, None, None, None, None, None, None, ceilings, HANDSHAKE_DEADLINE)
}

/// [`serve_authed`] with the pre-auth handshake deadline as a PARAMETER — a test seam, the
/// [`serve_with_read_ceilings`] shape: production always passes [`HANDSHAKE_DEADLINE`], and a test
/// that must watch the deadline fire waits half a second instead of ten. Nothing else differs: no
/// backfill, market-data, seed, catalog or import lane is mounted.
pub fn serve_authed_with_handshake_deadline(
    listener: TcpListener,
    store: Arc<dyn HistStore + Send + Sync>,
    keys: Option<NodeKeys>,
    deadline: Duration,
) -> io::Result<()> {
    serve_inner(
        listener,
        store,
        None,
        keys,
        None,
        None,
        None,
        None,
        ReadCeilings::PRODUCTION,
        deadline,
    )
}

/// The accept loop behind every entry above, with the range reads' ceilings as its ninth argument —
/// [`ReadCeilings::PRODUCTION`] everywhere but [`serve_with_read_ceilings`] — and the pre-auth
/// handshake deadline as its tenth: [`HANDSHAKE_DEADLINE`] everywhere but
/// [`serve_authed_with_handshake_deadline`].
#[allow(clippy::too_many_arguments)] // `serve_with_import`'s eight, the read ceilings, the deadline
fn serve_inner(
    listener: TcpListener,
    store: Arc<dyn HistStore + Send + Sync>,
    backfill: Option<BackfillTable>,
    keys: Option<NodeKeys>,
    md: Option<Arc<MdHub>>,
    seed: Option<Arc<SeedLane>>,
    catalog: Option<Arc<CatalogLane>>,
    import: Option<Arc<ImportLane>>,
    ceilings: ReadCeilings,
    handshake_deadline: Duration,
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
                let seed = seed.clone();
                let catalog = catalog.clone();
                let import = import.clone();
                thread::spawn(move || {
                    let _slot = slot;
                    handle_connection(
                        stream,
                        store,
                        backfill,
                        keys.as_deref(),
                        md,
                        seed,
                        catalog,
                        import,
                        ceilings,
                        handshake_deadline,
                    )
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
/// type: both crates declare `layer = 65` and `crates/vike-ops/tests/arch/layer_gate.rs` fails on
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
/// `vike_tradehub::server::handshake`'s `run_handshake`, including its refusal ordering.
///
/// Both frames are read under [`HANDSHAKE_MAX_FRAME_LEN`] rather than the shared 64 MiB ceiling —
/// an unauthenticated peer must not be able to name a large allocation — and the caller has already
/// armed the handshake deadline ([`HANDSHAKE_DEADLINE`] in production) as the read timeout, so a
/// peer that opens a socket and says nothing frees its thread in seconds rather than minutes.
///
/// The mac is verified against the REQUESTED scope's key: a scope whose key is ABSENT is refused
/// WITHOUT consulting it (the closed-gate shape), and otherwise `auth::verify` decides in
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
    if auth::verify(DATAHUB_DOMAIN, keys.key_for(scope), &nonce, PROTO_VERSION, scope, &mac) {
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
///
/// `handshake_deadline` is the pre-auth read timeout on a KEYED server — [`HANDSHAKE_DEADLINE`]
/// from every production entry.
#[allow(clippy::too_many_arguments)] // the stream, the server's seven handles, the read ceilings,
// and the handshake deadline
fn handle_connection(
    mut stream: TcpStream,
    store: Arc<dyn HistStore + Send + Sync>,
    backfill: Option<Arc<BackfillTable>>,
    keys: Option<&NodeKeys>,
    md: Option<Arc<MdHub>>,
    seed: Option<Arc<SeedLane>>,
    catalog: Option<Arc<CatalogLane>>,
    import: Option<Arc<ImportLane>>,
    ceilings: ReadCeilings,
    handshake_deadline: Duration,
) {
    let peer = stream.peer_addr().ok();
    tracing::info!(?peer, "vike-datahub: connection opened");
    // Nagle OFF before the first frame, on every connection — positional and, after a mode switch,
    // the market-data stream alike (`run_market_writer` inherits it). Why, and why a refusal is
    // only logged: `vike_node_proto::frame::configure_node_stream`.
    if let Err(e) = vike_node_proto::frame::configure_node_stream(&stream) {
        tracing::warn!(?peer, error = %e, "vike-datahub: TCP_NODELAY refused; frames may wait for an ACK");
    }

    let features = served_features(
        backfill.is_some(),
        backfill.as_deref().is_some_and(BackfillTable::has_funding),
        keys.is_some(),
        md.as_deref(),
        seed.is_some(),
        catalog.is_some(),
        import.as_deref(),
    );

    // On a KEYED server the PRE-AUTH phase runs first, under a far shorter deadline than the idle
    // timeout below: an unauthenticated peer has nothing to think about, so a handshake that has not
    // completed in seconds is a socket somebody opened and said nothing on.
    //
    // `None` — the key-less server — takes NEITHER branch: no handshake, no deadline, no scope. That
    // is what makes the key-less path byte-identical to the pre-auth protocol rather than merely
    // similar to it.
    let authed: Option<Scope> = match keys {
        Some(keys) => {
            if let Err(e) = stream.set_read_timeout(Some(handshake_deadline)) {
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

        // THIS REQUEST'S STOP PROBE, over a SHARED borrow of the stream: from here until `dispatch`
        // returns, this loop does not read the socket, so a peek is the only thing looking at it.
        // Only `backfill_verb` and the archive import ever ask it — every other verb leaves it
        // unasked, which costs them nothing and changes nothing. See `StopProbe`.
        let probe = StopProbe::new(&stream, peer);

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
                Step::reply(Response::Error(wrong_plane_message(kind, Plane::Data, Plane::Compute)))
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
                        dispatch(
                            request,
                            &store,
                            backfill.as_deref(),
                            true,
                            md.as_ref(),
                            seed.as_deref(),
                            catalog.as_deref(),
                            import.as_deref(),
                            ceilings,
                            &probe,
                        )
                    } else {
                        let kind = request_kind(&request);
                        tracing::info!(
                            ?peer,
                            ?scope,
                            ?needed,
                            verb = kind,
                            "vike-datahub: verb refused — outside this connection's scope"
                        );
                        Step::reply(Response::Error(match needed {
                            VerbScope::Handshake => format!(
                                "{kind} is a handshake frame; this connection is already \
                                 authenticated"
                            ),
                            // ⚠ The `Run*` verbs are NOT named here any more — they are not served
                            // by this daemon at all since ruling 7, and the guard above answers
                            // them before this arm is reached. `Backfill`, `ImportArchive` and
                            // `DeleteSeries` are what is left behind the Control scope on the data
                            // plane (`ImportArchive` since `docs/decisions/0100`'s verdict 1).
                            _ => format!(
                                "{kind} requires the Control scope; this connection authenticated \
                                 as Observe. The Backfill store WRITE, the ImportArchive store \
                                 WRITE and the DeleteSeries store REMOVAL are Control-only"
                            ),
                        }))
                    }
                }
                // KEY-LESS: no scope to check, and unchanged for every verb that predates the
                // keys. ⚠ NOT "every verb", which is what this comment claimed until 2026-09-07:
                // the `false` below IS the `keyed` argument `delete_series_verb` refuses on, so
                // the code this line annotates is what makes the old claim false.
                None => dispatch(
                    request,
                    &store,
                    backfill.as_deref(),
                    false,
                    md.as_ref(),
                    seed.as_deref(),
                    catalog.as_deref(),
                    import.as_deref(),
                    ceilings,
                    &probe,
                ),
            },
            Err(e) => {
                Step::reply(Response::Error(format!("unrecognized/undecodable request: {e}")))
            }
        };
        // Read before the stream is borrowed mutably to write: the probe's borrow ends here.
        let mode_lost = probe.mode_lost();

        match step {
            Step::Reply(response) => {
                if let Err(e) = write_frame(&mut stream, &*response) {
                    tracing::warn!(?peer, error = %e, "vike-datahub: write fault, closing connection");
                    break;
                }
                // A peek that could not put the socket back into blocking mode leaves it in a mode
                // nobody can vouch for: the reply is written, and then the connection closes rather
                // than reading the next frame from a socket that may answer `WouldBlock` mid-frame.
                if mode_lost {
                    tracing::warn!(
                        ?peer,
                        "vike-datahub: a backfill's peer check could not restore blocking mode on \
                         this socket; closing the connection after its reply"
                    );
                    break;
                }
            }
            // THE CLOSE. The request ran for a client that has gone; nobody would read a reply, so
            // none is written — which is also what keeps a misleading "write fault" out of the log.
            // `backfill_verb` has logged the request's own line already.
            Step::Close => break,
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
    ///
    /// ⚠ **BOXED, and the reason is a property of the wire rather than of this loop.**
    /// [`Response`] carries the STUDIO answers, and those grew a cost-model stamp
    /// (`vike_datahub_client::wire_studio`'s `WireCostModel` — ruling 0063), which pushed this
    /// enum's largest variant far past its second. `Step` is built once per decoded request and
    /// `ModeSwitch` is the common-sized arm, so an unboxed `Response` makes every mode switch pay
    /// the reply arm's width. The indirection is the same move
    /// `Request::RunWalkforward`'s own `slice` field already makes for the same reason, and it is
    /// invisible outside this file: `Step` is a control-flow value, never serialized, so nothing
    /// about the frame changes.
    ///
    /// [`Step::reply`] is the constructor — prefer it to spelling the `Box` at each site.
    Reply(Box<Response>),
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
    /// Close the connection WITHOUT writing anything — the request ran for a client that has gone.
    ///
    /// Produced by [`dispatch`] exactly when the request's [`StopProbe`] saw the peer gone, which
    /// only `backfill_verb` and the archive import (`crate::import::import_archive_verb`, between
    /// days) ever ask — so no other verb can reach this arm.
    /// `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §3 is the
    /// argument: a reply would be read by nobody, and writing it would only put a misleading
    /// "write fault" into the log. The thread then ends and its [`ConnSlot`] is released, as on any
    /// close.
    Close,
}

impl Step {
    /// [`Step::Reply`] with the boxing done once rather than at each of its construction sites —
    /// see that variant's doc for why it is boxed at all.
    fn reply(response: Response) -> Step {
        Step::Reply(Box::new(response))
    }
}

/// **The stop probe one request runs under** — whether the client that sent it is still there,
/// asked without reading anything the protocol needs. The connection's half of
/// `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §2.
///
/// # Why the connection has to be ASKED
///
/// `handle_connection` runs one request at a time on the connection's own thread, and
/// `backfill_verb` runs its collector inline, so while a long backfill runs nothing reads this
/// socket: a client that went away left a FIN unread in the kernel, and the daemon fetched on for
/// nobody — the incident the design was written from (four OANDA pairs over 21 years, stopped only
/// by a restart that took the recorder down with it). The market-data writer learns a peer is gone
/// from a failed heartbeat write; this wire is strictly request/response and the verb writes nothing
/// until it answers, so it cannot learn it that way. A heartbeat or progress frame is refused by the
/// design: an old client would read an unsolicited frame as its reply.
///
/// # What one ask does — a NON-BLOCKING PEEK, and nothing else
///
/// [`StopProbe::should_stop`] puts the socket into non-blocking mode, `peek`s one byte, and puts it
/// back (`peek_peer`). The verdicts, per std's `TcpStream::peek` and the platform's `recv(MSG_PEEK)`:
///
/// - `Ok(0)` — the peer sent FIN: **gone**.
/// - `WouldBlock` — nothing to read: **present**.
/// - `Ok(n > 0)` — the client sent bytes, a PIPELINED frame: **present**. `peek` leaves them in the
///   kernel, so the loop reads them as the next request once this one is answered.
/// - `Interrupted` — asked again. Any other error (a reset, an abort) — **gone**.
///
/// It is asked BETWEEN chunks of a chunked collector and never inside one, so it costs three
/// syscalls beside a chunk that costs at least a venue round trip. The archive import
/// (`crate::import::import_archive_verb`) asks the SAME probe between DAYS, which is its unit —
/// `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §8 named that verb
/// as one that should take it when it was built. A watcher thread (one more thread
/// per backfill, which must be stopped and would hold the socket with the loop) and TCP keepalive (a
/// new dependency, and blind on the deployed shape, where the peer is sshd on loopback) were weighed
/// and refused for v1 — the design's §2 table.
///
/// # What it LATCHES, and what it cannot see
///
/// - **Latched**: once a peek has seen the peer gone, every later ask answers `true` without
///   touching the socket — a gone peer does not come back.
/// - ⚠ **A half-close cancels.** A client that shuts down its WRITE side and still waits for the
///   answer reads as gone, because its FIN is all a peek can see. No vike client does that; one that
///   did would get no reply.
/// - ⚠ **A half-OPEN peer is invisible** — power off, a cable pulled — because it sends nothing.
///   Through the deployed `ssh -L` tunnel the datahub's peer is sshd on loopback, so what closes the
///   socket for a vanished laptop is sshd's own keepalive, and the peek then sees that FIN.
/// - ⚠ **If blocking mode cannot be restored**, `mode_lost` is set and the connection CLOSES after
///   the request's reply rather than serving on a socket in an unknown mode.
///
/// # Platforms
///
/// `set_nonblocking` and `peek` are std on every target, and so is the mapping above: unix `recv`
/// with `MSG_PEEK | O_NONBLOCK` answers `EAGAIN`/`EWOULDBLOCK`, Windows `recv(MSG_PEEK)` on a socket
/// put into non-blocking mode by `ioctlsocket(FIONBIO)` answers `WSAEWOULDBLOCK`, and std maps both
/// to `ErrorKind::WouldBlock`; a graceful close answers `0` on both. The receive TIMEOUT
/// (`SO_RCVTIMEO`) is a separate socket option that neither mode switch touches, so
/// [`IDLE_READ_TIMEOUT`] still holds once blocking mode is back. The tests in this file's `tests`
/// module run on Linux in CI; on Windows the same calls are compiled, and the behaviour above is the
/// platform's documented one rather than a run.
///
/// ⚠ The `Cell`s make it `!Sync`, which is right: it lives on the connection's thread and is handed
/// to the collector as a borrowed `&dyn Fn() -> bool` on that same thread.
struct StopProbe<'s> {
    stream: &'s TcpStream,
    peer: Option<SocketAddr>,
    /// LATCHED `true` once a peek has seen the peer gone.
    peer_gone: Cell<bool>,
    /// `true` once a peek could not put the socket back into blocking mode.
    mode_lost: Cell<bool>,
}

impl<'s> StopProbe<'s> {
    /// A probe over `stream` that has asked nothing yet — building one costs no syscall.
    fn new(stream: &'s TcpStream, peer: Option<SocketAddr>) -> Self {
        StopProbe { stream, peer, peer_gone: Cell::new(false), mode_lost: Cell::new(false) }
    }

    /// `true` means "stop now": the client is gone. Peeks once per call until it has said `true`,
    /// and from then on answers `true` without touching the socket.
    fn should_stop(&self) -> bool {
        if self.peer_gone.get() {
            return true;
        }
        let peek = peek_peer(self.stream);
        if !peek.blocking_restored {
            self.mode_lost.set(true);
        }
        if peek.gone {
            self.peer_gone.set(true);
        }
        peek.gone
    }

    /// Whether a peek has seen the peer gone — read WITHOUT peeking, which is what lets
    /// [`dispatch`] ask it after every request without touching the socket of one that never ran a
    /// backfill.
    fn peer_gone(&self) -> bool {
        self.peer_gone.get()
    }

    /// Whether a peek left the socket in a mode it could not restore.
    fn mode_lost(&self) -> bool {
        self.mode_lost.get()
    }
}

/// What one [`peek_peer`] saw.
struct Peek {
    /// The peer sent FIN, or the socket errored: nobody is there.
    gone: bool,
    /// Blocking mode is back on — `false` only when switching it back FAILED.
    blocking_restored: bool,
}

/// One non-blocking peek at `stream` — `StopProbe`'s whole I/O, and its doc carries the verdicts.
///
/// If non-blocking mode cannot be SET, nothing was changed and nothing can be asked without
/// blocking, so the peer reads as present: a probe that cannot see is not evidence of absence.
fn peek_peer(stream: &TcpStream) -> Peek {
    if stream.set_nonblocking(true).is_err() {
        return Peek { gone: false, blocking_restored: true };
    }
    let mut byte = [0u8; 1];
    let gone = loop {
        match stream.peek(&mut byte) {
            Ok(0) => break true,
            Ok(_) => break false,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break false,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break true,
        }
    };
    let blocking_restored = stream.set_nonblocking(false).is_ok();
    Peek { gone, blocking_restored }
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

/// Route one decoded request, splitting the MODE SWITCH — and the CLOSE — out from everything else.
///
/// The `md.is_some()` test is what decides between the switch and [`NO_MARKET_DATA_PLANE`], and it
/// is a RUNTIME fact rather than a cfg — the same rule `FEATURE_MARKET_DATA`'s advertisement
/// follows, so "compiled with `live-feeds`" and "armed by an operator" cannot answer differently.
///
/// The close is [`Step::Close`]: a request whose `probe` saw its client gone is answered by closing
/// the connection, never by a write. The check reads the probe's latch and does not peek, so a
/// request that never asked the probe — every verb but `Backfill` and `ImportArchive` — cannot
/// reach it.
#[allow(clippy::too_many_arguments)] // `handle_request`'s ten, which it routes
fn dispatch(
    request: Request,
    store: &Arc<dyn HistStore + Send + Sync>,
    backfill: Option<&BackfillTable>,
    keyed: bool,
    md: Option<&Arc<MdHub>>,
    seed: Option<&SeedLane>,
    catalog: Option<&CatalogLane>,
    import: Option<&ImportLane>,
    ceilings: ReadCeilings,
    probe: &StopProbe,
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
                Some(why) => Step::reply(Response::Error(why)),
                None => match hub.open_session() {
                    Ok(guard) => Step::ModeSwitch(guard, specs),
                    Err(msg) => Step::reply(Response::Error(msg)),
                },
            },
            None => Step::reply(Response::Error(NO_MARKET_DATA_PLANE.to_string())),
        },
        other => {
            let response = handle_request(
                other,
                store,
                backfill,
                keyed,
                md.map(|h| &**h),
                seed,
                catalog,
                import,
                ceilings,
                probe,
            );
            if probe.peer_gone() { Step::Close } else { Step::reply(response) }
        }
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
///
/// `probe` is the request's [`StopProbe`]; the `Backfill` and `ImportArchive` arms hand it on (the
/// `CancelBackfill` arm reads its peer address, for the log line, and asks it nothing).
#[allow(clippy::too_many_arguments)] // seven it always took, the read ceilings, the import lane, the stop probe
fn handle_request(
    request: Request,
    store: &Arc<dyn HistStore + Send + Sync>,
    backfill: Option<&BackfillTable>,
    keyed: bool,
    md: Option<&MdHub>,
    seed: Option<&SeedLane>,
    catalog: Option<&CatalogLane>,
    import: Option<&ImportLane>,
    ceilings: ReadCeilings,
    probe: &StopProbe,
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
                features: served_features(
                    backfill.is_some(),
                    backfill.is_some_and(BackfillTable::has_funding),
                    false,
                    md,
                    seed.is_some(),
                    catalog.is_some(),
                    import,
                ),
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
        Request::RunParamscan { .. } => compute_verb_moved("RunSweep"),
        Request::RunWalkforward { .. } => compute_verb_moved("RunWalkforward"),
        Request::RunParamscanProfile { .. } => compute_verb_moved("RunSweepProfile"),
        Request::RunWalkforwardProfile { .. } => compute_verb_moved("RunWalkforwardProfile"),
        // Ruling R1's research-plane verb — the EIGHTH compute verb, refused by name like the
        // seven ruling 7 moved. This daemon will never serve it: a study RUNS an engine.
        Request::RunStudy(_) => compute_verb_moved("RunStudy"),
        Request::ListStrategies => compute_verb_moved("ListStrategies"),
        // ...and the NAMED RUN pair (`docs/decisions/0064-a-named-run-carries-no-source.md`), the
        // NINTH and TENTH compute verbs. ⚠ Their SCOPE is Observe, which is unlike every other
        // `Run*` verb and is what the record turns on — but scope is not plane, and this daemon
        // holds no strategy roster and no engine to answer them with.
        Request::RunNamed(_) => compute_verb_moved("RunNamed"),
        Request::NamedStrategies => compute_verb_moved("NamedStrategies"),
        // ---- every RANGE read: `LoadBars`, the two L1 scans, and the six of `docs/decisions/0084` --
        //
        // ⚠ **Each one goes through `range_verb`, and each one's store read is BUDGETED** — the
        // head of what the reply can carry, never the client's range. `range_verb`'s doc carries
        // the two arms and why every reply within the ceiling is byte-identical to the old one.
        //
        // ⚠ **TWO caps, and they do different jobs — neither is redundant.** The budgeted read is
        // what bounds the ALLOCATION (a whole-range depth scan is ~27 GB for the live store's worst
        // single day); `cap_to_whole_ts` inside `range_verb` then bounds the FRAME to the `limit`. A
        // store that inherits a trait DEFAULT narrows nothing, so the frame cap is the only one that
        // fires there — which is exactly why it stays.
        Request::LoadBars { venue, symbol, interval, start, end, limit } => {
            let range = TsRange { start, end };
            range_verb(
                &LOAD_BARS,
                &format!("{venue}:{symbol}:{interval}"),
                range,
                limit,
                ceilings.bars,
                ceilings.frame_bytes,
                |n| store.load_bars_head(&venue, &symbol, &interval, range, n),
                row_count,
                |r| r.ts,
                Response::Bars,
            )
        }
        Request::ScanQuotes { venue, symbol, start, end, limit } => {
            let range = TsRange { start, end };
            range_verb(
                &SCAN_QUOTES,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.quotes,
                ceilings.frame_bytes,
                |n| store.scan_quotes_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.ts,
                Response::Quotes,
            )
        }
        Request::ScanTrades { venue, symbol, start, end, limit } => {
            let range = TsRange { start, end };
            range_verb(
                &SCAN_TRADES,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.trades,
                ceilings.frame_bytes,
                |n| store.scan_trades_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.ts,
                Response::Trades,
            )
        }
        //
        // ⚠ Plain `HistStore` TRAIT calls, exactly like the three above and like the store-metadata
        // verbs below — no `serve-datafusion` split, no mounted table, nothing runtime. A store
        // that does not hold the kind answers through its own error channel and rides
        // `Response::Error` like any other store failure; this daemon does not translate that into
        // an empty success, because "the store holds none" and "this store cannot answer" are the
        // two facts 0084's whole reader argument turns on.
        Request::ScanBookUpdates { venue, symbol, start, end, limit } => {
            let range = TsRange { start, end };
            range_verb(
                &SCAN_BOOK_UPDATES,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.book_levels,
                ceilings.frame_bytes,
                |n| store.scan_book_updates_capped(&venue, &symbol, range, Some(n)),
                book_stored_rows,
                |r| r.ts,
                Response::BookUpdates,
            )
        }
        Request::ScanDepth { venue, symbol, start, end, limit } => {
            let range = TsRange { start, end };
            range_verb(
                &SCAN_DEPTH,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.book_levels,
                ceilings.frame_bytes,
                |n| store.scan_depth_capped(&venue, &symbol, range, Some(n)),
                book_stored_rows,
                |r| r.ts,
                Response::Depth,
            )
        }
        // ⚠ `asset`, not `symbol` — the trait's own spelling for this one verb, carried through the
        // wire variant so the two cannot be transposed at either end.
        Request::ScanCohort { venue, asset, start, end, limit } => {
            let range = TsRange { start, end };
            range_verb(
                &SCAN_COHORT,
                &format!("{venue}:{asset}"),
                range,
                limit,
                ceilings.cohort,
                ceilings.frame_bytes,
                |n| store.scan_cohort_capped(&venue, &asset, range, Some(n)),
                row_count,
                |r| r.ts,
                Response::Cohort,
            )
        }
        Request::ScanPerpMetrics { venue, symbol, start, end, limit } => {
            let range = TsRange { start, end };
            range_verb(
                &SCAN_PERP_METRICS,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.perp_metrics,
                ceilings.frame_bytes,
                |n| store.scan_perp_metrics_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.ts,
                Response::PerpMetrics,
            )
        }
        Request::ScanEquity { venue, symbol, start, end, limit } => {
            let range = TsRange { start, end };
            range_verb(
                &SCAN_EQUITY,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.equity,
                ceilings.frame_bytes,
                |n| store.scan_equity_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.ts,
                Response::Equity,
            )
        }
        // ⚠ No range: `HistStore::scan_exec_fills` takes none, and the variant carries none — so
        // this verb has only the no-`limit` arm, read as a head of `ceiling + 1`.
        Request::ScanExecFills { venue, symbol } => {
            exec_fills_verb(store, &venue, &symbol, ceilings.exec_fills, ceilings.frame_bytes)
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
        Request::SeriesFacts { id } => match store.series_facts(&id) {
            Ok(facts) => Response::SeriesFacts(Box::new(facts)),
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
            backfill_verb(&venue, &symbol, &interval, (start, end), backfill, store, probe)
        }
        // The operator's door onto RUNNING backfills — the registry the mounted table keeps. Both
        // decode on every build; a server with no table runs no backfill and answers the capability
        // refusal (`no_backfill_registry`). Neither asks this request's probe: they run nothing.
        Request::ListBackfills => match backfill {
            Some(table) => Response::RunningBackfills(table.running()),
            None => Response::Error(no_backfill_registry("ListBackfills")),
        },
        Request::CancelBackfill { venue, symbol, interval } => {
            cancel_backfill_verb(&venue, &symbol, &interval, backfill, probe.peer)
        }
        // The HISTORY-CHANNELS read (`docs/decisions/0102`). Served on EVERY build: whether a lane
        // is mounted travels inside the answer, so a server with no table answers too and says so
        // row by row. It asks the table a lookup and its credential probe a word, and calls no
        // collector — `crate::history`'s module doc carries what it must never do.
        Request::HistoryChannels => {
            crate::history::history_channels_verb(store, backfill, vike_model::now_ms())
        }
        // The ARCHIVE IMPORT (`docs/decisions/0100`). It DECODES on every build — the schema lives
        // in the client crate — so a client that sends it is answered rather than dropped; whether
        // it SERVES depends on the MOUNTED lane, and `None` answers the capability refusal having
        // touched nothing (`served_features` withholds `archive_import` for the same reason, which
        // is what a well-behaved client reads first). The request's stop probe goes with it: the
        // import asks it between DAYS, and a client that went away ends the request at that
        // boundary with a close, as a stopped `Backfill` does.
        Request::ImportArchive(spec) => {
            crate::import::import_archive_verb(&spec, import, &|| probe.should_stop())
        }
        // The CHART-GAP SEED. Decodes on every build like its neighbour above; whether it FETCHES
        // depends on the ARMED lane, and an unarmed one answers a successful no-op rather than a
        // refusal — `seed_series_verb`'s own doc carries why those two differ here and nowhere else
        // on this wire.
        Request::SeedSeries { venue, symbol, interval, class } => {
            seed_series_verb(&venue, &symbol, &interval, class, backfill, seed, store)
        }
        // The VENUE CATALOG. Decodes on every build like both neighbours above, and — unlike either
        // — it never touches `store`, which is the whole of `docs/decisions/0062`'s decision 1 and
        // why this arm passes no store handle at all. An unarmed lane answers a successful
        // `NotArmed`; a venue this build cannot serve answers a successful `NotServed`. See
        // `venue_catalog_verb` for why so much of this verb's surface is SUCCESS rather than error.
        Request::VenueCatalog { venue } => venue_catalog_verb(&venue, catalog),
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

/// The message a KEY-LESS server answers `DeleteSeries` with. A `const` because two things must be
/// able to name it: this refusal, and the test that proves it is the same server refusing on the
/// path the client actually takes.
pub const KEYLESS_DELETE_REFUSAL: &str = "this datahub holds no node keys, so it serves no delete verb at all. A key-less server \
     authenticates nothing — its handshake informs, it does not gate — so `Scope::Write` would be \
     a word nothing enforces, and an irreversible verb behind a word is the state \
     docs/decisions/0025-datahub-remote-posture.md exists to prevent. Configure \
     VIKE_DATAHUB_OBSERVE_KEY / VIKE_DATAHUB_CONTROL_KEY on this server, or run the delete on the \
     box with `vike-cli data hist rm --store DIR`.";

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
/// ⚠ **`run_sweep_profile` and `run_sweep` are spelled the old way deliberately.** The
/// `sweep` -> `paramscan` rename moved the Rust identifiers and left every NEGOTIATED TOKEN where
/// it was — an older client compares a capability string literally, so renaming one refuses every
/// peer that shipped before the rename. The rename pass rewrote this paragraph anyway, which is how
/// a doc naming two strings that appear on no wire shipped once already; the compute daemon's
/// `served_features` (`crates/vike-backtest/src/compute_server.rs`) is what actually pushes them,
/// and it is the authority.
///
/// `has_backfill` is a RUNTIME fact, not a cfg: [`FEATURE_BACKFILL`] is advertised exactly when a
/// collector table is MOUNTED, because "compiled with `backfill-serve`" and "can actually serve a
/// backfill" are the same thing only when the bin wired a table in — a `serve()` entry inside a
/// `backfill-serve` test build still must not advertise what it will refuse. [`FEATURE_BACKFILL_CANCEL`]
/// rides the same fact: the registry it opens is the mounted table's. `has_backfill_funding`
/// is the same fact one level down: the mounted table carries a FUNDING lane
/// (`crate::backfill::BackfillTable::has_funding`). `import` is the same fact for the archive import
/// lane: `crate::import::advertised` pushes the capability and its format entries exactly when a
/// lane is mounted.
fn served_features(
    has_backfill: bool,
    has_backfill_funding: bool,
    requires_auth: bool,
    md: Option<&MdHub>,
    has_seed: bool,
    has_catalog: bool,
    import: Option<&ImportLane>,
) -> Vec<String> {
    let mut features = vec![
        // ⚠ The PLANE SENTINEL, pushed through the shared constant rather than spelled here: a
        // client (`vike_datahub_client::DatahubClient::connect_authed_on`) reads it to tell THIS
        // daemon — whose Write scope carries `Backfill` and `DeleteSeries` — from the compute one
        // before it signs anything. A literal that drifted from the client's copy would make this
        // daemon an unplaceable peer: still refused a compute-plane key, but refused to every
        // caller that asks for the data plane by name as well. The value is the frozen token it
        // always was — `"load_bars"`.
        DATA_PLANE_SENTINEL.to_string(),
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
        // The CHART-GAP SEED's CLASS field (`docs/decisions/0061` Phase 3). ⚠ **UNCONDITIONAL, and
        // deliberately NOT beside `FEATURE_SEED_SERIES` below**, although the two describe one
        // verb. That one is a RUNTIME fact — is this box's lane armed — and this one is a BUILD
        // fact, the `FEATURE_COVERAGE` shape: the only question its advertisement answers is "is
        // this daemon older than the field", and a build that decodes the field honours it whether
        // or not any lane is armed. Gating it on `has_seed` would make an unarmed-but-modern daemon
        // indistinguishable from an old one, and a client would then withhold a class from a server
        // that understands it perfectly well — and withholding it is the silent downgrade the field
        // exists to prevent. `FEATURE_SEED_CLASS`' own doc carries the three legs.
        FEATURE_SEED_CLASS.to_string(),
        // The SIX tick-level and research reads `docs/decisions/0084-only-the-datahub-touches-
        // the-store.md` asked this wire to grow. UNCONDITIONAL, the `FEATURE_COVERAGE` shape:
        // they are `vike_data::HistStore` trait verbs with no table to mount, so every build
        // that serves at all serves them and the advertisement answers exactly one question —
        // is this server older than the verb? Six strings rather than one family string;
        // `FEATURE_SCAN_BOOK_UPDATES`' own doc argues why.
        FEATURE_SCAN_BOOK_UPDATES.to_string(),
        FEATURE_SCAN_DEPTH.to_string(),
        FEATURE_SCAN_COHORT.to_string(),
        FEATURE_SCAN_PERP_METRICS.to_string(),
        FEATURE_SCAN_EQUITY.to_string(),
        FEATURE_SCAN_EXEC_FILLS.to_string(),
        // The ROW CAP on a range scan. UNCONDITIONAL like the six above and for the same reason —
        // it is a property of this build's `handle_request`, not of a mounted table — and the
        // advertisement is load-bearing rather than informational: an older server IGNORES an
        // unknown `limit` field (serde skips unknown fields) and answers with EVERY row, so a
        // client that assumed the cap without checking would ask for a page and get a frame that
        // overruns `MAX_FRAME_LEN`. Silence here means "do not send one".
        FEATURE_SCAN_LIMIT.to_string(),
        // 0084's SEVENTH verb — the one that record did not price, because it measured the
        // `HistStore` trait and this was inherent to `DataFusionHist`. Unconditional, the
        // `FEATURE_COVERAGE` shape.
        FEATURE_SERIES_FACTS.to_string(),
        // The HISTORY-CHANNELS read (`docs/decisions/0102`). UNCONDITIONAL, the `FEATURE_COVERAGE`
        // shape and deliberately NOT `FEATURE_BACKFILL`'s per-mounted-table rule below: whether a
        // lane is mounted travels INSIDE the answer, so a table-less build still serves the verb,
        // and gating the string on the table would make an unmounted modern server look like an
        // old one. The advertisement answers one question — is this server older than the verb.
        FEATURE_HISTORY_CHANNELS.to_string(),
    ];
    // The backfill-on-demand verb — advertised per MOUNTED TABLE, not per cfg (see the doc above).
    // This advertisement is the verb's whole negotiation: it shipped without a PROTO_VERSION bump.
    if has_backfill {
        features.push(FEATURE_BACKFILL.to_string());
        // ...and the operator's door onto the backfills it runs, on the SAME condition: the
        // registry `ListBackfills` reads and `CancelBackfill` flags lives with the mounted table,
        // and a server with none runs nothing to list or stop. Not tied to keys — a key-less
        // loopback server serves both, as it serves `Backfill`
        // (`docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`).
        features.push(FEATURE_BACKFILL_CANCEL.to_string());
    }
    // ...and its FUNDING lane, per mounted funding SOURCE rather than per table: a table with none
    // can only refuse `interval=funding`, and a server older than the lane refuses it as an
    // unmeasurable step — so silence here is what makes a client refuse first, sending nothing
    // (`FEATURE_BACKFILL_FUNDING`'s own doc).
    if has_backfill_funding {
        features.push(FEATURE_BACKFILL_FUNDING.to_string());
    }
    // The CHART-GAP SEED lane — advertised per ARMED LANE, a runtime fact like `backfill`'s
    // per-mounted-table rule. `has_seed` is `SeedLane`'s mere existence, and that is deliberate:
    // `crate::datahub_cli` builds one only under `VIKE_DATAHUB_CHART_SEED=1`, so there is no
    // `enabled: bool` for this advertisement to get out of step with.
    //
    // ⚠ Its absence does NOT make the verb refuse, which is the one place this capability differs
    // from every other on this wire — an unarmed server answers `SeedDone { armed: false }` and
    // writes nothing. `FEATURE_SEED_SERIES`' own doc argues why that difference IS the Observe
    // classification rather than a leniency beside it, and
    // `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` puts removing it in the reopen
    // list. The advertisement is still what stops a well-behaved client sending, so an operator
    // learns which switch is off instead of watching a chart that never fills.
    if has_seed {
        features.push(FEATURE_SEED_SERIES.to_string());
    }
    // The VENUE-CATALOG lane — advertised per ARMED LANE, the same runtime rule as `seed_series`
    // above, and `has_catalog` is `CatalogLane`'s mere existence for the same reason (there is no
    // `enabled: bool` for the advertisement to get out of step with).
    //
    // ⚠ Its absence does NOT make the verb refuse either: an unarmed server answers
    // `CatalogOutcome::NotArmed` and calls no venue. What the advertisement buys here is
    // specifically the thing an EMPTY LIST would destroy — a client that knows the lane is unarmed
    // says so, where a client that merely got no instruments could not tell that apart from `ig`
    // and `ibkr`, which genuinely have none (`docs/decisions/0062`'s decision 5).
    if has_catalog {
        features.push(FEATURE_VENUE_CATALOG.to_string());
    }
    // The ARCHIVE IMPORT lane — advertised per MOUNTED LANE, the `backfill` rule, and the
    // capability rides TOGETHER with one `import_format=<id>` entry per registered format. A daemon
    // with no project above it, or a build without the format registry, mounts none and pushes
    // nothing here (`crate::import::mount`), so "can serve" and "will serve" stay one answer.
    features.extend(crate::import::advertised(import));
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
    // THE RECORDING plane's venue roster — `rec_venue=<slug>`, one entry per venue this build can
    // RECORD, and the twin of the `md_venue=` block directly above.
    //
    // ⚠ **A BUILD fact, like `FEATURE_COVERAGE` and UNLIKE the `md_venue=` entries it sits beside.**
    // The difference is deliberate and it follows the QUESTION each answers. `md_venue=` answers
    // "will this process serve me a live tick", which is false without a mounted hub however the
    // binary was compiled — a runtime fact. This one answers "if I write `okx` into a subscription
    // row, will the daemon refuse to start at its next restart", and that is decided by
    // `crate::recording::build_recording_feed`'s compiled arms alone: a serve-only invocation of a
    // `record-binance` build still could not record `okx`, and could record `binance` the moment an
    // operator adds the flag. Gating this on the `--record` flag would make the advertisement
    // answer a different question from the one a client asks it.
    //
    // ⚠ A build with NO recording plane pushes NOTHING here, and `FEATURE_REC_VENUE_PREFIX`'s own
    // doc carries what a client must do about it: an empty advertisement is AMBIGUOUS (an old
    // server, or a `record`-less build) and is therefore the same answer as an unreachable server —
    // warn and proceed, never refuse. The refusal is available only where the server advertised at
    // least one venue.
    #[cfg(feature = "record")]
    for venue in crate::recording::supported() {
        features.push(vike_datahub_client::proto::rec_venue_feature(venue));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// **The `rec_venue=` advertisement is exactly what THIS BUILD can record — both ways.**
    ///
    /// ⚠ It is the only thing that proves the push in [`served_features`] happens at all. The
    /// prefix, the builder and the reader are unit-tested in the light crate
    /// (`crates/vike-datahub-client/src/proto_tests.rs`'s `the_rec_venue_feature_round_trips`), but a
    /// round trip in that crate says nothing about whether a server ever WRITES one — and a read
    /// half with no write half is worse than neither, because a client that refuses an
    /// unadvertised venue would then refuse every venue against a server that records it perfectly
    /// well.
    ///
    /// ⚠ **Both configurations are asserted and neither is vacuous.** The DEFAULT build carries no
    /// recording plane, so it must advertise NOTHING — that arm runs in the derived roster lane on
    /// every PR. A `record-*` build must advertise exactly `crate::recording::supported()`, and
    /// that arm runs in `cargo test -p vike-datahub --features
    /// record-polymarket,record-binance`, the lane `scripts/ci_feature_suite.sh`'s
    /// `recorder-venues` arm spells for the recorder's venue feeds.
    #[test]
    fn the_recordable_venues_are_advertised_as_this_build_can_record_them() {
        let features = served_features(false, false, false, None, false, false, None);
        let advertised = vike_datahub_client::proto::advertised_rec_venues(&features);

        #[cfg(feature = "record")]
        {
            let can_record: Vec<String> =
                crate::recording::supported().into_iter().map(str::to_string).collect();
            assert_eq!(
                advertised, can_record,
                "the advertisement must BE `crate::recording::supported()`, in order — a \
                 client refuses a `record add` for any venue it does not see here"
            );
        }
        #[cfg(not(feature = "record"))]
        assert!(
            advertised.is_empty(),
            "a build with no recording plane records nothing and must advertise nothing — an \
             empty advertisement is AMBIGUOUS by design and a client degrades on it: {advertised:?}"
        );

        // …and it never collides with the LIVE plane's entries, whatever this build carries.
        assert!(
            vike_datahub_client::advertised_md_venues(&features).is_empty(),
            "no md hub was mounted, so no `md_venue=` entry may appear: {features:?}"
        );
    }

    /// The accepted socket carries `TCP_NODELAY` (`vike_node_proto::frame::configure_node_stream`)
    /// — the market-data stream included, since it is this socket after the mode switch. Read off
    /// the socket itself rather than timed, so it holds on any OS: a clone of the served stream is
    /// the same socket, and once a `Pong` has come back `handle_connection` is past the line that
    /// arms it. (`tests/wire_latency.rs` pins what it is FOR, on Linux.)
    #[test]
    fn the_accepted_socket_has_nagle_off() {
        let (mut client, served) = socket_pair();
        let clone = served.try_clone().expect("clone the served socket");
        assert!(!clone.nodelay().expect("read TCP_NODELAY"), "guard: a fresh socket has Nagle on");
        let store: Arc<dyn HistStore + Send + Sync> = Arc::new(vike_data::MemHistStore::new());
        thread::spawn(move || {
            handle_connection(
                served,
                store,
                None,
                None,
                None,
                None,
                None,
                None,
                ReadCeilings::PRODUCTION,
                HANDSHAKE_DEADLINE,
            )
        });
        write_frame(&mut client, &Request::Ping).expect("ping");
        let answer = vike_node_proto::frame::read_frame::<_, Response>(&mut client).expect("pong");
        assert!(matches!(answer, Response::Pong), "expected Pong, got {answer:?}");
        assert!(clone.nodelay().expect("read TCP_NODELAY"), "the accepted socket has Nagle on");
    }

    // ── T4: THE PEEK PROBE ON A REAL SOCKET ─────────────────────────────────────────────────────
    //
    // `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §6, T4: `false`
    // while the peer is connected and silent, `false` once it has sent bytes (which stay unread),
    // `true` once it has closed — and the socket BLOCKS again afterwards. Real loopback sockets, the
    // kind the server serves; this runs on Linux in CI, and `StopProbe`'s doc carries what that does
    // and does not say about Windows.

    use std::io::{Read, Write};

    /// A connected loopback pair: the client's end, and the end a server would serve.
    fn socket_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let client = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (served, _) = listener.accept().expect("accept");
        (client, served)
    }

    /// Wait — blocking, bounded — until `served` has something to report: bytes, or the FIN. A
    /// BLOCKING peek returns as soon as either has arrived, so the probe below is asked about a
    /// state the kernel already holds rather than one still in flight.
    fn wait_for_the_peer(served: &TcpStream) -> usize {
        served.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
        let mut byte = [0u8; 1];
        served.peek(&mut byte).expect("the peer's bytes or FIN arrive on loopback")
    }

    /// **T4, the half K4 kills.** A connected, silent peer is PRESENT, and the probe leaves the
    /// socket BLOCKING: a read under a short timeout waits that timeout out. A probe that left the
    /// socket non-blocking would make the read answer `WouldBlock` at once — and the connection
    /// loop's next `read_frame_raw` would then close a live connection as "idle".
    #[test]
    fn a_silent_connected_peer_is_present_and_the_socket_blocks_again_afterwards() {
        let (_client, served) = socket_pair();
        let probe = StopProbe::new(&served, None);

        assert!(!probe.should_stop(), "a connected, silent peer must read as present");
        assert!(!probe.peer_gone(), "nothing latched");
        assert!(!probe.mode_lost(), "blocking mode was restored");

        let timeout = Duration::from_millis(400);
        served.set_read_timeout(Some(timeout)).expect("timeout");
        let started = Instant::now();
        let mut byte = [0u8; 1];
        let read = (&served).read(&mut byte);
        let waited = started.elapsed();
        assert!(
            matches!(&read, Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)),
            "a silent peer's read must time out: {read:?}"
        );
        assert!(
            waited >= timeout - Duration::from_millis(100),
            "the read returned after {waited:?}, not after its {timeout:?} timeout: the probe left \
             the socket NON-blocking"
        );
    }

    /// **T4, the half K5 kills.** A peer that has SENT bytes — a pipelined frame — is present, and
    /// the bytes are still there afterwards: a peek consumes nothing the protocol needs.
    #[test]
    fn a_peer_that_sent_bytes_is_present_and_the_bytes_stay_unread() {
        let (mut client, served) = socket_pair();
        client.write_all(b"frame").expect("write");
        assert!(wait_for_the_peer(&served) > 0, "guard: the bytes arrived");
        let probe = StopProbe::new(&served, None);

        assert!(!probe.should_stop(), "pipelined bytes are a live client, never a disconnect");
        assert!(!probe.peer_gone());

        let mut got = [0u8; 5];
        (&served).read_exact(&mut got).expect("the bytes are still there");
        assert_eq!(&got, b"frame", "the probe consumed nothing");
    }

    /// **T4, the half K3 kills.** A peer that has CLOSED is gone — and so is one that only shut its
    /// WRITE side, which is the half-close the probe's doc says cancels — and the answer LATCHES.
    #[test]
    fn a_closed_peer_is_gone_and_a_half_close_reads_the_same() {
        for half_close in [false, true] {
            let (client, served) = socket_pair();
            let _kept = if half_close {
                client.shutdown(std::net::Shutdown::Write).expect("half-close");
                Some(client)
            } else {
                drop(client);
                None
            };
            assert_eq!(wait_for_the_peer(&served), 0, "guard: the FIN arrived");
            let probe = StopProbe::new(&served, None);

            assert!(probe.should_stop(), "half_close = {half_close}: a closed peer is gone");
            assert!(probe.peer_gone(), "half_close = {half_close}");
            assert!(probe.should_stop(), "half_close = {half_close}: and it stays gone");
            assert!(!probe.mode_lost(), "half_close = {half_close}");
        }
    }

    /// The LATCH, white-box: once the probe has said "stop" it never peeks again, so a peer that
    /// would read as present cannot un-say it. Planted on a live, silent connection — the one state
    /// in which a fresh peek would answer `false`.
    #[test]
    fn once_the_probe_has_said_stop_it_keeps_saying_it_without_peeking() {
        let (_client, served) = socket_pair();
        let probe = StopProbe::new(&served, None);
        assert!(!probe.should_stop(), "guard: a fresh peek of this peer answers present");

        probe.peer_gone.set(true);

        assert!(probe.should_stop(), "a latched probe must not be un-latched by a peek");
    }
}
