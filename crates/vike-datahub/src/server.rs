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
//!   `crates/vike-datahub/src/server.rs`'s `delete_series_verb` answers [`KEYLESS_DELETE_REFUSAL`] on THIS arm, before the selector is
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

use std::cell::Cell;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use vike_catalog::{CatalogAvailability, catalog_availability};
use vike_data::removal;
use vike_data::{HistStore, TsRange};
use vike_datahub_client::catalog::{
    CATALOG_MAX_INSTRUMENTS, CatalogListing, CatalogOutcome, CatalogRefusal, validate_catalog_venue,
};
use vike_datahub_client::market::{MD_HEARTBEAT, MdBye, MdFrame, MdSpec};
use vike_datahub_client::proto::{
    BACKFILL_ONE_BATCH, BackfillDone, DATA_PLANE_SENTINEL, DeleteDone, FEATURE_AUTH,
    FEATURE_BACKFILL, FEATURE_BACKFILL_CANCEL, FEATURE_BACKFILL_FUNDING, FEATURE_COVERAGE,
    FEATURE_DELETE_SERIES, FEATURE_HISTORY_CHANNELS, FEATURE_MARKET_DATA,
    FEATURE_SCAN_BOOK_UPDATES, FEATURE_SCAN_COHORT, FEATURE_SCAN_DEPTH, FEATURE_SCAN_EQUITY,
    FEATURE_SCAN_EXEC_FILLS, FEATURE_SCAN_LIMIT, FEATURE_SCAN_PERP_METRICS, FEATURE_SEED_CLASS,
    FEATURE_SEED_SERIES, FEATURE_SERIES_FACTS, FEATURE_VENUE_CATALOG, PROTO_VERSION, Plane,
    Request, Response, SeedDone, VerbScope, md_venue_feature, plane_of, read_frame_raw,
    read_frame_raw_capped, request_kind, required_scope, resolve_produced_by, scope_admits,
    write_frame, wrong_plane_message,
};
use vike_datahub_client::seed::{seed_range, validate_seed_interval, validate_seed_symbol};
use vike_node_proto::auth::{self, DATAHUB_DOMAIN, NodeKeys, Scope, fresh_nonce};

use crate::backfill::BackfillTable;
use crate::catalog::{CatalogAdmission, CatalogLane};
use crate::import::ImportLane;
use crate::md::hub::{MdKey, push_attach_frame};
use crate::md::mailbox::{Mailbox, Recv};
use crate::md::{MD_LAPSE_BUDGET, MD_LAPSE_WINDOW, MD_WRITE_TIMEOUT, MdHub, SessionGuard};
use crate::seed::{SeedAdmission, SeedLane};

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
///   ⚠ Every verb is served on this arm EXCEPT `DeleteSeries`, which `crates/vike-datahub/src/server.rs`'s `delete_series_verb` refuses
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
    serve_inner(listener, store, None, None, None, None, None, None, ceilings)
}

/// The accept loop behind every entry above, with the range reads' ceilings as its ninth argument —
/// [`ReadCeilings::PRODUCTION`] everywhere but [`serve_with_read_ceilings`].
#[allow(clippy::too_many_arguments)] // `serve_with_import`'s eight, plus the read ceilings
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
#[allow(clippy::too_many_arguments)] // the stream, the server's seven handles, the read ceilings
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
) {
    let peer = stream.peer_addr().ok();
    tracing::info!(?peer, "vike-datahub: connection opened");

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

/// Cap `rows` to about `limit`, cutting ONLY on a whole-`ts` boundary.
///
/// ⚠ **The softness is the point.** A paging client continues from `last_ts + 1` — the reply
/// carries no cursor, for the wire-compatibility reason [`FEATURE_SCAN_LIMIT`] argues — so a page
/// cut mid-`ts` would strand the rest of that timestamp's rows on the far side of the client's own
/// next bound. They would vanish with no error and no gap: a short answer shaped exactly like the
/// truth. `scan_book_updates` makes it worse still, since it REGROUPS on `(ts, seq)` and a mid-`ts`
/// cut splits one logical event into two partial ones.
///
/// Three cases, and the third is the one that is easy to get wrong:
///
/// * the cap falls exactly on a group boundary — take `limit` rows;
/// * a group STRADDLES the cap — walk back to that group's first row and stop before it, so the
///   page is short of the cap rather than over it;
/// * the straddling group is the FIRST one — take it WHOLE, over the cap. Returning nothing here
///   would be correct by the letter and useless: a client would ask the identical question
///   forever. Progress outranks the cap, and a single `ts` larger than a page is the one shape
///   this wire cannot bound.
///
/// Every row family reaching this function is ts-ascending, which is what makes the walk a local
/// one; `vike_data::store_kind`'s codecs sort on `ts` first for every kind.
fn cap_to_whole_ts<T>(mut rows: Vec<T>, limit: Option<u32>, ts_of: impl Fn(&T) -> i64) -> Vec<T> {
    let Some(limit) = limit.map(|l| l as usize) else { return rows };
    if limit == 0 || rows.len() <= limit {
        return rows;
    }
    let straddling = ts_of(&rows[limit - 1]);
    if ts_of(&rows[limit]) != straddling {
        rows.truncate(limit);
        return rows;
    }
    let mut cut = limit - 1;
    while cut > 0 && ts_of(&rows[cut - 1]) == straddling {
        cut -= 1;
    }
    if cut == 0 {
        let mut end = limit;
        while end < rows.len() && ts_of(&rows[end]) == straddling {
            end += 1;
        }
        cut = end;
    }
    rows.truncate(cut);
    rows
}

/// What a range verb is called, what its rows are, and what its ceiling is named — the vocabulary
/// of its refusal, one per verb [`range_verb`] serves.
struct RangeVerb {
    verb: &'static str,
    unit: &'static str,
    ceiling_name: &'static str,
}

const LOAD_BARS: RangeVerb =
    RangeVerb { verb: "LoadBars", unit: "bars", ceiling_name: "LOAD_BARS_CEILING" };
const SCAN_QUOTES: RangeVerb =
    RangeVerb { verb: "ScanQuotes", unit: "quotes", ceiling_name: "SCAN_QUOTES_CEILING" };
const SCAN_TRADES: RangeVerb =
    RangeVerb { verb: "ScanTrades", unit: "trades", ceiling_name: "SCAN_TRADES_CEILING" };
const SCAN_BOOK_UPDATES: RangeVerb = RangeVerb {
    verb: "ScanBookUpdates",
    unit: "book levels",
    ceiling_name: "SCAN_BOOK_LEVELS_CEILING",
};
const SCAN_DEPTH: RangeVerb =
    RangeVerb { verb: "ScanDepth", unit: "book levels", ceiling_name: "SCAN_BOOK_LEVELS_CEILING" };
const SCAN_COHORT: RangeVerb =
    RangeVerb { verb: "ScanCohort", unit: "cohort rows", ceiling_name: "SCAN_COHORT_CEILING" };
const SCAN_PERP_METRICS: RangeVerb = RangeVerb {
    verb: "ScanPerpMetrics",
    unit: "perp-metric rows",
    ceiling_name: "SCAN_PERP_METRICS_CEILING",
};
const SCAN_EQUITY: RangeVerb =
    RangeVerb { verb: "ScanEquity", unit: "equity samples", ceiling_name: "SCAN_EQUITY_CEILING" };

/// One range verb — `LoadBars` and the seven scans — answered from a read that stops at what the
/// reply can carry. `read(n)` is the verb's bounded store read for `n` rows: a COMPLETE PREFIX of
/// the range holding at least `n` stored rows unless it is the whole range (`HistStore`'s
/// `load_bars_head`, or a `scan_*_capped` with `Some(n)`). `stored_rows` counts what the store's `n`
/// counts — rows, except for the book kinds, whose store counts one row per LEVEL
/// ([`book_stored_rows`]).
///
/// ⚠ **The store is asked for a HEAD, never for the range.** Every one of these arms used to read
/// the client's whole range — `LoadBars` through `load_bars`, the three research scans through their
/// unbudgeted reads on EVERY page, the four tick scans whenever no `limit` was sent — and then
/// [`cap_to_whole_ts`], so the frame was bounded and the allocation was whatever the client asked
/// for: gigabytes for one request over a long `5s` series, and, through `RemoteHistStore`'s pager,
/// the rest of the range again for every page. Both arms below need only the head:
///
/// * **With a `limit` `L`**, the store is asked for `min(L, ceiling)` rows and the reply is
///   [`cap_to_whole_ts`] of them — BYTE-IDENTICAL to the old reply whenever `L` is within the
///   ceiling, because a complete prefix of at least `L` rows holds every row the cap's decision
///   looks at (`docs/superpowers/specs/2026-10-01-loadbars-bounded-read-design.md`'s section 2
///   carries the three cases). A `limit` above the ceiling is CLAMPED to it: a page shorter than
///   its `limit` is already legal under the soft cap, and every reply longer than the ceiling
///   overran the frame anyway (see [`LOAD_BARS_CEILING`] and its siblings).
/// * **With no `limit`** — and with `Some(0)`, which [`cap_to_whole_ts`] has always read as "no
///   cap" — the store is asked for `ceiling + 1` rows. More than `ceiling` back means the range
///   holds more than one reply can carry, and it is REFUSED BY NAME rather than read, clamped in
///   silence, or sent to fail at `write_frame`; fewer means the answer IS the whole range, which the
///   contract's at-least half guarantees, so the reply is exactly the old one.
///
/// The cap stays HERE, in the server, for the reason it exists at all: a store promises only that
/// its prefix never splits a timestamp, and cutting on a whole-`ts` boundary is the wire's rule.
/// ⚠ For the book kinds the cap counts EVENTS while the read counts levels, exactly as it has since
/// budgets landed; a page of events at least `L` long is not promised there, only a complete one.
///
/// ⚠ **…and then the BYTES are cut too, because a row ceiling is not a frame.** Each ceiling is the
/// frame divided by its kind's SHORTEST row, so a reply under it of wider rows can still overrun
/// `frame_bytes` — a 30-day window of `5s` bars, 518,400 of them at about 146 bytes, was one — and
/// before this step that reply was read, serialised and dropped at `write_frame`. [`fit_to_frame`]
/// counts every row's exact JSON and cuts on a whole `ts`: with a `limit`, the shorter page it
/// answers is as legal as any soft-capped page; with none, a reply over the frame is REFUSED BY
/// NAME. Neither arm ever answers an EMPTY page in place of rows (`fit_to_frame`'s doc says why).
/// Every reply that fits is untouched, so byte-identical to before this step existed.
#[allow(clippy::too_many_arguments)] // the verb, its series, range, limit, two ceilings, three row functions, its variant
fn range_verb<T: serde::Serialize>(
    what: &RangeVerb,
    series: &str,
    range: TsRange,
    limit: Option<u32>,
    ceiling: usize,
    frame_bytes: usize,
    read: impl FnOnce(usize) -> Result<Vec<T>, vike_data::DataError>,
    stored_rows: impl Fn(&[T]) -> usize,
    ts_of: impl Fn(&T) -> i64,
    wrap: fn(Vec<T>) -> Response,
) -> Response {
    match limit.filter(|l| *l > 0) {
        Some(limit) => {
            let n = (limit as usize).min(ceiling);
            let rows = match read(n) {
                Ok(rows) => rows,
                Err(e) => return Response::Error(e.to_string()),
            };
            // `n <= limit`, so it is a `u32` again without loss.
            let rows = cap_to_whole_ts(rows, Some(n as u32), &ts_of);
            match fit_to_frame(rows, wrap, frame_bytes, &ts_of) {
                FrameFit::Whole(rows) | FrameFit::Prefix(rows) => wrap(rows),
                FrameFit::FirstTsOver { ts } => {
                    Response::Error(first_ts_over_frame(what, series, ts, frame_bytes))
                }
            }
        }
        None => {
            let rows = match read(ceiling.saturating_add(1)) {
                Ok(rows) => rows,
                Err(e) => return Response::Error(e.to_string()),
            };
            if stored_rows(&rows) > ceiling {
                return Response::Error(range_refusal(what, series, range, ceiling));
            }
            match fit_to_frame(rows, wrap, frame_bytes, &ts_of) {
                FrameFit::Whole(rows) => wrap(rows),
                FrameFit::Prefix(_) | FrameFit::FirstTsOver { .. } => {
                    Response::Error(range_over_frame(what, series, range, frame_bytes))
                }
            }
        }
    }
}

/// What [`fit_to_frame`] found.
enum FrameFit<T> {
    /// Every row fits the frame, and the rows are handed back untouched.
    Whole(Vec<T>),
    /// They did not all fit: the longest whole-`ts` prefix that does — never empty.
    Prefix(Vec<T>),
    /// The rows of the FIRST timestamp alone take more than the frame, so no page can carry them.
    FirstTsOver { ts: i64 },
}

/// Cut `rows` (ts-ascending) to the longest whole-`ts` prefix whose reply — `wrap`'s variant
/// around them — fits `frame_bytes`, counting EXACTLY rather than estimating: each row's JSON is
/// written into a [`ByteCount`] that allocates nothing, plus one separating comma per row after the
/// first, plus the envelope (the length of `wrap` around no rows, measured once). That is the body
/// `write_frame` builds, byte for byte, so "fits here" and "passes `write_frame`" are the same
/// statement. The cost is one extra serialisation pass of CPU and no memory.
///
/// ⚠ **It NEVER answers an empty prefix in place of rows.** A paging client stops on its first
/// EMPTY page (`RemoteHistStore`'s `paged`), so an empty answer from a range that goes on reads as
/// its end and the rest is lost with no error — the loss
/// `docs/superpowers/specs/2026-10-01-tick-scan-paging-silent-loss-design.md` fixed, made again by a
/// different cap. When the first timestamp's rows alone overrun the frame the answer is
/// [`FrameFit::FirstTsOver`], which every caller turns into a NAMED error.
///
/// No row width is assumed anywhere, so there is no "skip the count when every row is narrow"
/// shortcut: even a bar carries an `Option<String>` symbol, so no kind here has a widest row.
fn fit_to_frame<T: serde::Serialize>(
    mut rows: Vec<T>,
    wrap: fn(Vec<T>) -> Response,
    frame_bytes: usize,
    ts_of: impl Fn(&T) -> i64,
) -> FrameFit<T> {
    let mut total = json_len(&wrap(Vec::new()));
    // `rows[..boundary]` is the longest whole-`ts` prefix known to fit: it is moved only at a `ts`
    // change, and only while every row before it has been counted without passing the frame.
    let mut boundary = 0usize;
    for i in 0..rows.len() {
        if i > 0 && ts_of(&rows[i]) != ts_of(&rows[i - 1]) {
            boundary = i;
        }
        total = total.saturating_add(usize::from(i > 0)).saturating_add(json_len(&rows[i]));
        if total > frame_bytes {
            if boundary == 0 {
                return FrameFit::FirstTsOver { ts: ts_of(&rows[0]) };
            }
            rows.truncate(boundary);
            return FrameFit::Prefix(rows);
        }
    }
    FrameFit::Whole(rows)
}

/// An `io::Write` that keeps only a COUNT of what is written to it — so [`json_len`] measures a
/// row's exact JSON without building it.
struct ByteCount(usize);

impl io::Write for ByteCount {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The exact length of `value`'s compact JSON — what `serde_json::to_vec` would produce, and so
/// what it contributes to a frame body. A value serde cannot write (none of the row types here
/// can fail) counts as larger than any frame, which refuses rather than sends it.
fn json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    let mut count = ByteCount(0);
    match serde_json::to_writer(&mut count, value) {
        Ok(()) => count.0,
        Err(_) => usize::MAX,
    }
}

/// The named refusal for a reply with no `limit` whose rows number no more than their ceiling but
/// whose JSON passes the frame — the case a row ceiling derived from the SHORTEST row cannot see.
fn range_over_frame(what: &RangeVerb, series: &str, range: TsRange, frame_bytes: usize) -> String {
    let bound = |b: Option<i64>| b.map_or_else(|| "open".to_string(), |t| t.to_string());
    format!(
        "{} {series} over [{}, {}] is more than one reply can carry: its {} pass MAX_FRAME_LEN \
         ({frame_bytes} bytes) of JSON although they number no more than {}, and this request \
         sent no `limit`, so it is refused rather than sent to fail at the frame. Send a `limit` \
         and start each next request at the last `ts` + 1 of the reply before it \
         (`RemoteHistStore` pages that way), or narrow the range.",
        what.verb,
        bound(range.start),
        bound(range.end),
        what.unit,
        what.ceiling_name,
    )
}

/// The named error for a page whose FIRST timestamp's rows alone pass the frame — answered in place
/// of an EMPTY page, which a pager would read as the end of the range.
fn first_ts_over_frame(what: &RangeVerb, series: &str, ts: i64, frame_bytes: usize) -> String {
    format!(
        "{} {series}: the {} at ts {ts} alone pass MAX_FRAME_LEN ({frame_bytes} bytes), so no \
         page can carry them; none is sent, because an empty page would read as the end of the \
         range. This range cannot be read past ts {ts} over this wire.",
        what.verb, what.unit,
    )
}

/// The rows a book answer stands for in the STORE's count — one per level, and one placeholder for
/// an event with none (`vike_data`'s book codec, `book_rows`) — so a no-`limit` read of
/// `ceiling + 1` stored rows is judged in the unit it was asked in.
fn book_stored_rows(events: &[vike_model::BookUpdate]) -> usize {
    events.iter().map(|e| (e.bids.len() + e.asks.len()).max(1)).sum()
}

/// The named refusal [`range_verb`] answers a no-`limit` request over `ceiling` with: the verb, the
/// series, the range, the ceiling by value AND by name, and the remedy.
fn range_refusal(what: &RangeVerb, series: &str, range: TsRange, ceiling: usize) -> String {
    let bound = |b: Option<i64>| b.map_or_else(|| "open".to_string(), |t| t.to_string());
    format!(
        "{} {series} over [{}, {}] holds more than {ceiling} {} ({}, the most one reply can \
         carry) and this request sent no `limit`, so it is refused and the rest of the range was \
         not read. Send a `limit` and start each next request at the last `ts` + 1 of the reply \
         before it (`RemoteHistStore` pages that way), or narrow the range.",
        what.verb,
        bound(range.start),
        bound(range.end),
        what.unit,
        what.ceiling_name,
    )
}

/// `Request::ScanExecFills`, answered from a HEAD of `ceiling + 1` fills: the whole series when it
/// holds no more than `ceiling`, and a named refusal when it holds more. The variant carries no range
/// and no `limit`, so this is [`range_verb`]'s no-`limit` arm and nothing else — see
/// [`SCAN_EXEC_FILLS_CEILING`] for why a refusal here takes nothing away. Its byte check is
/// [`range_verb`]'s no-`limit` one: a series under the ceiling whose fills' JSON passes the frame is
/// refused by name too.
fn exec_fills_verb(
    store: &Arc<dyn HistStore + Send + Sync>,
    venue: &str,
    symbol: &str,
    ceiling: usize,
    frame_bytes: usize,
) -> Response {
    let rows = match store.scan_exec_fills_head(
        venue,
        symbol,
        TsRange::all(),
        ceiling.saturating_add(1),
    ) {
        Ok(rows) => rows,
        Err(e) => return Response::Error(e.to_string()),
    };
    if rows.len() > ceiling {
        return Response::Error(format!(
            "ScanExecFills {venue}:{symbol} holds more than {ceiling} fills \
             (SCAN_EXEC_FILLS_CEILING, the most one reply can carry), and this verb carries no \
             range and no `limit` to page with, so it is refused and the rest of the series was not \
             read. A range and a limit on this verb is a wire change deferred until a series nears \
             this ceiling."
        ));
    }
    match fit_to_frame(rows, Response::ExecFills, frame_bytes, |r| r.ts) {
        FrameFit::Whole(rows) => Response::ExecFills(rows),
        FrameFit::Prefix(_) | FrameFit::FirstTsOver { .. } => Response::Error(format!(
            "ScanExecFills {venue}:{symbol} is more than one reply can carry: its fills' JSON \
             passes MAX_FRAME_LEN ({frame_bytes} bytes) although they number no more than \
             SCAN_EXEC_FILLS_CEILING, and this verb carries no range and no `limit` to page with, \
             so it is refused rather than sent to fail at the frame."
        )),
    }
}

/// The `stored_rows` of every row kind but the book's: one stored row per answered row.
fn row_count<T>(rows: &[T]) -> usize {
    rows.len()
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
     authenticates nothing — its handshake informs, it does not gate — so `Scope::Write` would be \
     a word nothing enforces, and an irreversible verb behind a word is the state \
     docs/decisions/0025-datahub-remote-posture.md exists to prevent. Configure \
     VIKE_DATAHUB_OBSERVE_KEY / VIKE_DATAHUB_CONTROL_KEY on this server, or run the delete on the \
     box with `vike-cli data hist rm --store DIR`.";

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

/// The `Backfill` verb: validate the bounded range and the interval, dispatch venue → collector
/// through the mounted [`BackfillTable`], then read the range BACK through the served store handle
/// — the write-through proof — and answer [`Response::BackfillDone`]. Every failure (no table,
/// unmeasurable interval, zero-width interval, unknown venue, inverted range, collector error,
/// read-back error) is a clean [`Response::Error`], so the client always learns the outcome; a
/// partial `BackfillDone` is never sent. The ONE request that answers nothing is one whose client
/// has gone — the next section.
///
/// # ⚠ A request whose client has gone STOPS, and answers by closing
///
/// The collector runs inline, for as long as its window takes, and nothing else reads the socket
/// meanwhile — so the request's [`StopProbe`] goes to the collector as its fifth argument
/// (`crate::backfill::BackfillFn`'s doc), and a CHUNKED collector asks it between chunks: a
/// non-blocking peek that reads a FIN as gone and a pipelined frame as present. Once it has seen
/// the client gone, the ingest stops at that boundary with every chunk before it stored, this verb
/// logs ONE info line — the series, the window, and the collector's own account of the chunks it
/// stored, the rows it wrote and that repeating the request resumes — and the connection closes
/// WITHOUT a reply ([`Step::Close`]): nobody would read one. The read-back below does not run.
/// `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §1–§3 is the
/// argument, and the probe's own doc carries what it cannot see (a half-OPEN peer) and what it
/// reads too eagerly (a half-CLOSE, which cancels).
///
/// A ONE-BATCH lane — the keyless kline fold and the funding lane — never asks the probe, so it
/// runs to its end exactly as before, whoever is still listening; so does a chunked request whose
/// client stays. Nothing about the wire moved: no frame is written before the reply, `BackfillDone`
/// is unchanged and so is `PROTO_VERSION`.
///
/// # ⚠ An OPERATOR'S cancel stops it too — and is ANSWERED
///
/// While its collector runs, the request is REGISTERED in the table's registry
/// (`crate::backfill::BackfillTable::register`), which is what `Request::ListBackfills` lists and
/// `Request::CancelBackfill` flags from another connection. The probe the collector gets is the
/// COMPOSED one, `cancelled || peer gone`, latched: the cancel flag is asked first (it costs no
/// syscall) and, once raised, is never lowered. Each ask also counts one boundary, the listing's
/// progress figure. A request a cancel stopped differs from one whose client left in exactly one
/// way: its client is still THERE, so it gets a [`Response::Error`] naming the cancel and carrying
/// the collector's own account — the chunks stored, the rows written, and that repeating the
/// request resumes (the design's §3 and Q5). Never `BackfillDone`: the window is not in the store.
/// A collector that finished before it reached a boundary at which the flag was up answers
/// `BackfillDone` as usual — the window IS whole, and a cancel that stopped nothing is no reason to
/// say otherwise. The registration is dropped as soon as the collector returns, and its `Drop` is
/// what removes the entry on a PANIC too.
///
/// # ⚠ The read-back asks for the range's EDGES, never for its bars
///
/// The proof reports two numbers — the range's first and last stored `ts` — and the window it reads
/// is the CLIENT's, so it can be years of a fine interval. Reading it back with
/// `HistStore::load_bars` built every row of that window inside this daemon, whose memory cap is
/// shared with the market-data and recorder planes, to look at two of them: one long request could
/// OOM-kill the process. It goes through `HistStore::bar_edges` instead, which `DataFusionHist` —
/// the store this daemon serves — answers from the `ts` column alone
/// (`crates/vike-datahub/tests/backfill_readback.rs`'s `the_backfill_readback_never_loads_the_range`
/// pins that this verb no longer reaches `load_bars`). What the proof proves narrows with it: the
/// rows are committed and readable through the served handle, not that every column of them
/// decodes — `bar_edges`' own doc says so.
///
/// The reply is byte-identical to the load-based one for every lane, `first_ts`/`last_ts` included:
/// `crates/vike-data/tests/bar_edges.rs` holds the store's answer equal to the edges derived from
/// `load_bars`, and `crates/vike-datahub/tests/backfill_readback.rs` holds the verb's reply equal
/// to them through a real store.
///
/// # The funding lane
///
/// An `interval` of `vike_data::source::FUNDING_INTERVAL` (`"funding"`) is a reserved LABEL, not a
/// step: it routes to `venue`'s [`crate::backfill::BackfillLane::Funding`] entry — the market
/// funding-rate series, one row per `crate::backfill::FUNDING_SOURCES` source — instead of a bar
/// lane. It is exempt from the forming-bar refusal below, and it ALONE is: a funding point is a
/// settled event with no close time, while every other unmeasurable step is still refused. Its
/// unknown-venue refusal names the FUNDING set rather than the bar one, because the two differ and
/// a bar-venue list would send an operator to a venue with no funding source. A client learns the
/// lane exists from [`FEATURE_BACKFILL_FUNDING`], never from this refusal.
///
/// # ⚠ The interval check, and why it is a REFUSAL rather than validation hygiene
///
/// `vike_backfill`'s shared ingest (`klines::ingest_klines`) drops the still-forming last candle
/// through `drop_forming_tail`, which measures the step with `vike_model::time::interval_ms` — and
/// that parser splits on a SINGLE trailing character over `s`/`m`/`h`/`d`. `1w`, `1M` and `1mo`
/// have no width there, so the guard **declines to act** (its own pinned test,
/// `unparseable_interval_leaves_bars_untouched`, says so: an unknown interval must never panic or
/// drop data, so the decision belongs to the caller). The caller is this verb.
///
/// ⚠ **The seam decides too now, and this verb's refusal is still not redundant.** 0059 Phase 1 put
/// the same refusal into `vike_backfill::klines::ingest_klines`, so a request that got past here
/// would be refused one layer down rather than writing a forming bar. Two things keep this one: it
/// answers BEFORE the venue lookup, so the message is about the step rather than about a collector
/// (`the_interval_refusal_precedes_the_venue_lookup` pins that ordering), and it answers before a
/// collector is entered at all, which on a synchronous per-request verb is the difference between a
/// refusal and a held connection. Both spell the one predicate,
/// `vike_model::time::measures_bar_step`, so they cannot drift about WHICH steps are refused.
///
/// What that costs if nobody decides: the venue's still-open weekly candle is stored as a CLOSED
/// bar, and the window's commit key is spent. `vike_data::DataFusionHist`'s `commit_rows` checks
/// `has_commit` before anything else, so a corrective re-fetch of the same window returns `Ok(0)`
/// and reports success over the wrong row. There is no verb, flag or helper in this workspace that
/// retires a single commit key — the only remedy is destroying the whole `(venue, symbol,
/// interval)` series. And on a KEY-LESS server this verb is served while `delete_series_verb` is
/// withheld (`crate::datahub_cli`'s module doc, per
/// `docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md`), on the argument that
/// "a backfill writes rows a re-fetch restores" — which is exactly the premise that fails here.
///
/// So the refusal sits where the seed verb's already does (`validate_seed_interval`, a narrower
/// allowlist on a narrower surface). This was the one automated dispatch path with no gate at all,
/// which is why 0059 Phase 2 could not widen the table without adding it — and since
/// docs/decisions/0094 deleted the collector supervisor, whose roster validator refused the same
/// steps at startup, it is the only refusal that answers before the collector seam.
///
/// ⚠ It is deliberately NOT a per-venue interval table —
/// `docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md`'s Phase 1 owns
/// that, and a whole-verb refusal of an interval the STORE cannot measure is a different claim
/// from "this venue serves that step". Nor is it `SEED_INTERVALS`: that set is narrower still and
/// belongs to an OBSERVE-scoped verb; this one is Control-scoped and an operator asking for `3h`
/// on deribit is asking for something real.
fn backfill_verb(
    venue: &str,
    symbol: &str,
    interval: &str,
    (start, end): (i64, i64),
    table: Option<&BackfillTable>,
    store: &Arc<dyn HistStore + Send + Sync>,
    probe: &StopProbe,
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
    // THE FUNDING LANE. `interval=funding` is a reserved LABEL, not a step
    // (`vike_data::source::FUNDING_INTERVAL`): a funding point is a settled event with no close
    // time, so the forming-bar refusal below does not apply to it.
    let funding = interval == vike_data::source::FUNDING_INTERVAL;
    // THE FORMING-BAR REFUSAL. Before the venue is looked up, so an operator gets the same answer
    // whichever venue they named — the `seed_series_verb` ordering rule — and before anything is
    // fetched, because the row this prevents cannot be taken back.
    if !funding && !vike_model::time::measures_bar_step(interval) {
        return Response::Error(format!(
            "backfill: interval {interval:?} has no bar width this store can measure \
             (`vike_model::time::interval_ms` reads a count plus one of s/m/h/d, so `1w`, `1M` and \
             `1mo` are outside it). Refused HERE, before any venue is dispatched to, because the \
             collectors' still-forming-candle guard silently DECLINES on an interval it cannot \
             measure: the venue's open candle would be stored as a closed bar and the window's \
             commit key spent, making a corrective re-fetch a silent zero-row success. Ask for a \
             step the store can measure, or resample from one."
        ));
    }
    // THE ZERO-WIDTH REFUSAL, for every lane. `0m` MEASURES — `interval_ms` reads it as `Some(0)` —
    // so the forming-bar refusal above lets it through, but a bucket of no width is no bar: the
    // tick lane's resample would divide by it, and no kline venue serves such a step. Before the
    // venue lookup, for the same reason as the refusal above.
    if vike_model::time::interval_ms(interval) == Some(0) {
        return Response::Error(format!(
            "backfill: interval {interval:?} has zero width — a bar of no duration is no bar, so \
             there is nothing to fetch or resample. Refused before any venue is dispatched to; \
             nothing was fetched and no commit key was spent. Ask for a positive step such as `1m`."
        ));
    }
    let Some((lane, collector)) = table.get_with_lane(venue, interval) else {
        let (what, set) = if funding {
            ("funding-rate collector", table.funding_supported())
        } else {
            ("collector", table.supported())
        };
        return Response::Error(format!(
            "backfill: venue `{venue}` has no {what} in this build. Supported: [{}]",
            set.join(", ")
        ));
    };
    // THE REGISTRY. Only a request that is about to RUN is registered — every refusal above has
    // answered already — and it stays listed exactly while its collector runs: `registration`'s
    // `Drop` removes it below, or on the unwind if the collector panics.
    let registration = table.register(venue, symbol, interval, (start, end), probe.peer, lane);
    let running = registration.request();
    // Set when the probe said "stop" because of the OPERATOR's flag rather than the peer.
    let stopped_by_cancel = Cell::new(false);
    // THE COMPOSED PROBE: `cancelled || peer gone`, latched by both halves. The flag first — it is
    // an atomic load where the other half is three syscalls — and every ask counts one boundary.
    let should_stop = || {
        running.reached_a_boundary();
        if running.cancelled() {
            stopped_by_cancel.set(true);
            return true;
        }
        probe.should_stop()
    };
    // Collector runs INLINE (v1 is synchronous per request; the collectors page + pace
    // internally), writing through the same store this server serves — under the request's stop
    // probe, which a chunked collector asks between chunks (see the doc above).
    let outcome = collector(symbol, interval, start, end, &should_stop);
    // It has stopped running, whatever it answered: out of the registry before anything else, so a
    // cancel that arrives during the read-back below finds nothing to flag rather than a request
    // that can no longer stop.
    drop(registration);
    // THE CLIENT IS GONE. Whatever the collector answered, nobody is there to read it: one info line
    // at the request boundary, and `dispatch` turns this into `Step::Close` on the probe's latch, so
    // the `Response` below is never written. No read-back — it would prove a write to nobody.
    if probe.peer_gone() {
        let account = match &outcome {
            Ok(rows) => format!("the collector finished first: {rows} rows written"),
            Err(e) => e.clone(),
        };
        tracing::info!(
            peer = ?probe.peer,
            venue,
            symbol,
            interval,
            start,
            end,
            "vike-datahub: backfill STOPPED at a chunk boundary — its client closed the connection \
             while it ran, so no reply is written and the connection closes. Every chunk before the \
             boundary stays stored, and repeating the request resumes there. The collector's \
             account: {account}"
        );
        return Response::Error(format!(
            "backfill {venue}/{symbol}@{interval} [{start}, {end}]: stopped — its client closed \
             the connection while it ran (nothing failed; repeating the request resumes). \
             {account}"
        ));
    }
    // AN OPERATOR CANCELLED IT. The client is still here, so it is ANSWERED — with an error, never
    // `BackfillDone`, because the window is not in the store. The collector's own text is the
    // account of what IS: its stop names the boundary, the chunks stored and the rows written.
    // (An `Ok` here means the collector reached its end without stopping on the flag: the window is
    // whole and the ordinary answer below is the true one.)
    if stopped_by_cancel.get()
        && let Err(account) = &outcome
    {
        tracing::info!(
            peer = ?probe.peer,
            venue,
            symbol,
            interval,
            start,
            end,
            "vike-datahub: backfill STOPPED at a chunk boundary — CANCELLED by an operator \
             (`CancelBackfill` from another connection); its client is answered with the cancel. \
             Every chunk before the boundary stays stored, and repeating the request resumes there. \
             The collector's account: {account}"
        );
        return Response::Error(format!(
            "backfill {venue}/{symbol}@{interval} [{start}, {end}]: CANCELLED by an operator \
             (`CancelBackfill`) and stopped at a chunk boundary — nothing failed: every chunk \
             before the boundary is stored, and repeating the request resumes there. {account}"
        ));
    }
    let rows_written = match outcome {
        Ok(n) => n as u64,
        Err(e) => {
            return Response::Error(format!("backfill {venue}/{symbol}@{interval} failed: {e}"));
        }
    };
    // Write-through proof + the client's seam bookkeeping: what the requested range now holds,
    // asked of the SERVED handle — the same series, range, parts and row filter the client's
    // follow-up LoadBars would read. ⚠ EDGES, never bars: this window is the CLIENT's and can span
    // years of a fine interval, and loading it would hold every row in this daemon's memory only to
    // read two timestamps off the ends (see the doc above, and `HistStore::bar_edges`).
    match store.bar_edges(venue, symbol, interval, TsRange { start: Some(start), end: Some(end) }) {
        Ok(edges) => Response::BackfillDone(BackfillDone {
            rows_written,
            first_ts: edges.first_ts,
            last_ts: edges.last_ts,
        }),
        Err(e) => Response::Error(format!(
            "backfill {venue}/{symbol}@{interval}: collector wrote {rows_written} rows but the \
             read-back failed: {e}"
        )),
    }
}

/// The refusal a server with NO collector table answers both registry verbs with — it runs no
/// backfill, so there is nothing to list and nothing to stop, and `served_features` withholds
/// [`FEATURE_BACKFILL_CANCEL`] for the same reason. An error rather than an empty list, so "this
/// server cannot run a backfill at all" never reads as "nothing is running".
///
/// ⚠ It must not mention a SCOPE: a key-less server answers it to every caller, and
/// `crates/vike-datahub/tests/auth_roundtrip.rs` reads a scope word in a key-less answer as a
/// refusal on authentication grounds.
fn no_backfill_registry(verb: &str) -> String {
    format!(
        "{verb}: this datahub mounts no collector table, so it runs no Backfill to list or stop — \
         the `{FEATURE_BACKFILL_CANCEL}` capability is deliberately absent from its \
         Welcome.features. Nothing was changed."
    )
}

/// **The `CancelBackfill` verb**: raise the cancel flag of every running request on one series
/// that can stop, and answer at once with which it flagged and which it could not
/// (`crate::backfill::BackfillTable::cancel` does the work; this verb adds the door and the log).
///
/// It does not wait: a flagged request stops at its NEXT chunk boundary on its own connection's
/// thread, and answers ITS client there (see [`backfill_verb`]'s cancel section). Nothing stored is
/// touched, which is why the verb is Control and not keys-only, and is served on a key-less
/// loopback server exactly as `Backfill` is —
/// `docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`.
/// The scope check happened before this was reached, in `handle_connection`.
///
/// ONE info line per cancel — an operator's act on other connections' work is worth the line, and
/// it is at a request boundary, not per frame.
fn cancel_backfill_verb(
    venue: &str,
    symbol: &str,
    interval: &str,
    table: Option<&BackfillTable>,
    peer: Option<SocketAddr>,
) -> Response {
    let Some(table) = table else {
        return Response::Error(no_backfill_registry("CancelBackfill"));
    };
    let done = table.cancel(venue, symbol, interval);
    let ids = |rows: &[vike_datahub_client::proto::RunningBackfill]| {
        rows.iter().map(|r| r.id).collect::<Vec<_>>()
    };
    tracing::info!(
        ?peer,
        venue,
        symbol,
        interval,
        flagged = ?ids(&done.flagged),
        unstoppable = ?ids(&done.unstoppable),
        "vike-datahub: CancelBackfill — the flagged requests stop at their next chunk boundary; \
         the unstoppable ones run to their end ({BACKFILL_ONE_BATCH})"
    );
    Response::BackfillsCancelled(done)
}

/// **Gate 2b's whole body: a class claim this server cannot HONOUR is refused by name.**
/// `Some(message)` is the refusal; `None` means the claim is one the route will obey.
///
/// The third leg of `vike_datahub_client::proto::FEATURE_SEED_CLASS`, and the only one a client
/// cannot skip. Two refusals, and they answer different questions:
///
/// **(a) UNADDRESSABLE** — `vike_catalog::addressing_for(venue)` says this venue's data path cannot
/// address that class at all (`Option` at bybit, `Equity` anywhere in crypto, anything at all at
/// `fxcm`, anything at an unknown venue, whose row REFUSES by construction). The same table, and
/// the same question, `crates/bridges/bybit/src/data.rs`'s and
/// `crates/bridges/binance/src/data.rs`'s `route_target` each ask as their own first arm — asked
/// here too because this door is reached by an OBSERVE client and those two are not on the path
/// (see `seed_series_verb`'s ⚠ on the four class-less seams below this one).
///
/// **(b) UNHONOURABLE ON THIS SPELLING** — the claim is addressable but would have to CHANGE the
/// route, and nothing between this door and the collector carries it. At a
/// `vike_catalog::Naming::PerpSuffix` venue the workspace's own spelling IS the claim: a bare
/// symbol names the spot listing and a `vike_catalog::PERP_SUFFIX` one names the perpetual. So a
/// `CryptoPerp` claim on a bare symbol, or a spot claim on a suffixed one, is a request this server
/// would answer from the OTHER book while reporting rows written — 0061's measured bug, one layer
/// below the one the capability string closes. Refusing it names the spelling that works, which is
/// an ACT rather than a complaint.
///
/// ⚠ **(b) is the load-bearing half and it is also the one a reader will want to relax.** It looks
/// redundant beside the bridges' own `contradicting_claim_refusal`, and it is not: those refuse a
/// claim that contradicts the suffix, while this refuses a claim the suffix does not ALREADY make.
/// A bare `BTCUSDT` claimed `CryptoPerp` passes binance's `route_target` happily — it is exactly
/// how that function reaches `fapi` — and would be correct the moment the class reaches the bridge.
/// Until it does, honouring it here would write the perpetual tape under the SPOT series key, since
/// `SeedLane::admit`'s ledger and the `store.load_bars` read-back below both key on
/// `(venue, symbol, interval)` and 0061's store-key verdict keeps that key the SYMBOL. **So this
/// refusal is what lets the ledger key stay a triple**, and relaxing it without threading the class
/// to the collector re-opens both defects at once.
///
/// ⚠ **What it does NOT catch, declared rather than implied.** At a `Naming::VenueNative` venue the
/// symbol already names its own product, so any addressable class passes (b) — including a WRONG
/// one. `BTC-USDT-SWAP` claimed as `AssetClass::Option` is addressable at okx (its row carries
/// `Option`) and is nonsense, and this door cannot see that without importing okx's symbology,
/// which `vike-catalog`'s addressing table deliberately is not. The claim is inert there — it
/// changes no route and writes no row differently — so the residual is a claim that is ignored
/// rather than a book that is wrong. Closing it is the same work as (b)'s relaxation: thread the
/// class to the bridge, where the venue's own parser can answer.
fn refuse_unhonourable_class(
    venue: &str,
    symbol: &str,
    class: vike_model::AssetClass,
) -> Option<String> {
    let row = vike_catalog::addressing_for(venue);
    // (a)
    if !row.addresses(class) {
        return Some(format!(
            "venue `{venue}`'s kline path addresses {:?}, and {class:?} is not one of them. \
             Nothing was fetched. An unknown venue addresses NOTHING here by construction — \
             `vike_catalog::addressing_for`'s fallback refuses rather than answering permissive, \
             which is the whole of docs/decisions/0061's Phase 1.",
            row.classes
        ));
    }
    // (b) — only a venue whose SPELLING carries the product can have a claim that contradicts it.
    // A `VenueNative` or `NoDerivative` venue's symbol already names its own book, so an addressable
    // claim there is inert rather than unhonourable (see this function's last ⚠).
    if !matches!(row.naming, vike_catalog::Naming::PerpSuffix) {
        return None;
    }
    let (_, suffixed) = vike_catalog::split_perp(symbol);
    let wants_perp =
        matches!(class, vike_model::AssetClass::CryptoPerp | vike_model::AssetClass::CryptoFuture);
    if wants_perp == suffixed {
        return None;
    }
    Some(if wants_perp {
        format!(
            "a {class:?} claim on `{venue}` needs the `{}` spelling, and this symbol carries none. \
             Nothing was fetched and nothing was written. At this venue the SPELLING is the claim: \
             a bare symbol names the spot listing, and that is the series key a seed writes under \
             and the one a chart then reads back. Honouring the claim on a bare symbol would store \
             the perpetual's tape under the spot series — the wrong-book defect \
             docs/decisions/0061 exists to close. Ask for the same symbol with `{}` appended.",
            vike_catalog::PERP_SUFFIX,
            vike_catalog::PERP_SUFFIX
        )
    } else {
        format!(
            "this symbol carries `{}` — which says PERPETUAL — and the caller claimed {class:?}. \
             Two claims that disagree, so neither is obeyed and nothing was fetched. Drop the \
             suffix or drop the claim. (The bridges' own `route_target` refuses the identical \
             pair; it is repeated at this door because an OBSERVE client reaches the door and not \
             the bridge.)",
            vike_catalog::PERP_SUFFIX
        )
    })
}

/// **The CHART-GAP SEED verb** — one bounded window of klines for a series a chart cannot paint.
///
/// ⚠ **This is a WRITE served to an OBSERVE connection**, against
/// `docs/decisions/0052`'s forward ruling, and
/// `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` is the argument. Every line below
/// is a term of that argument rather than defensive tidying — read the record before relaxing one.
///
/// # The order of the gates, and why each sits where it does
///
/// 1. **ARMED?** The refusal that is about the SERVER rather than about the request comes first —
///    the `delete_series_verb` idiom. ⚠ **And it is not a refusal**: an unarmed lane answers a
///    SUCCESSFUL [`SeedDone`] with `armed: false`, having touched nothing. That is 0057's reach
///    property 3, the leg that makes an Observe classification honest rather than convenient: an
///    Observe connection's authority is to name the series its chart is open on, and whether that
///    becomes a write is a fact about the server's configuration. Returning an error here would
///    make the verb mean "do this", which is the thing it must not mean.
/// 2. **INTERVAL, then SYMBOL** — [`validate_seed_interval`] / [`validate_seed_symbol`], the SAME
///    functions the client predicts with. ⚠ **These two are a security boundary, not validation
///    hygiene.** `crates/bridges/binance/src/family/klines.rs`'s `klines_url` interpolates both
///    fields into a REST query string with no allowlist and no encoding (bybit and okx validate in
///    their own code tables; binance does not, and the per-venue interval table that would own this
///    is deferred). Until this verb, everything reaching that line came from an operator's argv or a
///    `VerbScope::Write` client. They run BEFORE the venue lookup so a bad interval is refused
///    identically whichever venue was named, and before the lane is consulted so a malformed request
///    cannot spend a token.
/// 3. **BUILD** — no mounted table is the `backfill_verb` refusal, verbatim in spirit: a lane armed
///    on a build with no collectors can serve nothing.
/// 4. **VENUE** — the server's own table, and its refusal names the supported set.
/// 5. **THE LANE** — [`SeedLane::admit`]: the ledger (a repeat is free, which is the SERVER-side leg
///    of "exactly one fetch per series"), the lifetime series cap, then the per-venue token bucket.
///    Last, because it is the only check with a side effect that a later refusal should not have
///    paid for.
///
/// Then the collector runs INLINE over a window the SERVER computed
/// ([`seed_range`]), exactly as `backfill_verb` runs one over a window the CLIENT named — same
/// table, same write-through, same read-back proof. The whole difference between the two verbs is
/// who chose that window, which is the whole difference between Control and Observe here.
///
/// # ⚠ Gate 2b — THE CLASS RE-CHECK, and why it is a gate rather than a routing input
///
/// `docs/decisions/0061` Phase 3 lets the request name what KIND of instrument `symbol` is, and
/// `vike_datahub_client::proto::FEATURE_SEED_CLASS` states the three legs that field needs. This is
/// the third and the only one that holds against a client that skipped the other two —
/// [`refuse_unhonourable_class`] is the whole of it, and it is deliberately a REFUSAL rather than a
/// route.
///
/// **The class reaches no bridge from here, and that is this phase's declared boundary.** Between
/// this door and a venue's `fetch_klines_range_classed` sit four seams that carry no class:
/// [`BackfillFn`], `vike_backfill::kline_source::backfill_kline_source`, the
/// `vike_data::source::KlineSource::fetch` trait method and each collector's own bridge call. So the
/// only claim this server can honour is one the UNCLASSED route would already obey — and a claim it
/// cannot honour must be REFUSED, never
/// dropped, because dropping it is precisely the wrong-book answer wearing a success that the
/// capability's own doc calls "the measured bug reproduced by its own fix".
fn seed_series_verb(
    venue: &str,
    symbol: &str,
    interval: &str,
    class: Option<vike_model::AssetClass>,
    table: Option<&BackfillTable>,
    lane: Option<&SeedLane>,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Response {
    // Gate 1 — see the doc: a SUCCESS, not a refusal, and deliberately so.
    let Some(lane) = lane else {
        return Response::SeriesSeeded(SeedDone {
            armed: false,
            repeated: false,
            rows_written: 0,
            range: None,
            first_ts: None,
            last_ts: None,
        });
    };
    // Gates 2 — before the venue lookup and before a token can be spent.
    if let Err(why) = validate_seed_interval(interval) {
        return Response::Error(format!("seed: {why}"));
    }
    if let Err(why) = validate_seed_symbol(symbol) {
        return Response::Error(format!("seed: {why}"));
    }
    // Gate 2b — THE CLASS RE-CHECK. Beside its two siblings and for the same reason: before the
    // venue lookup, so the answer does not depend on which build this is, and before the lane, so a
    // claim this server cannot honour cannot spend a token or enter the ledger.
    if let Some(class) = class
        && let Some(why) = refuse_unhonourable_class(venue, symbol, class)
    {
        return Response::Error(format!("seed: {why}"));
    }
    // Gate 3.
    let Some(table) = table else {
        return Response::Error(format!(
            "seed: this build has no collectors — rebuild vike-datahub with              `--features backfill-serve`. The `{FEATURE_SEED_SERIES}` capability is deliberately              absent from this server's Welcome.features."
        ));
    };
    // Gate 4 — the KLINE lane only: a chart open never starts a tick download or a funding fetch.
    let Some(collector) = table.get_for_seed(venue) else {
        return Response::Error(format!(
            "seed: venue `{venue}` has no collector in this build. Supported: [{}]",
            table.seed_supported().join(", ")
        ));
    };
    let Some((start, end)) = seed_range(interval, vike_model::now_ms()) else {
        // Unreachable behind gate 2 — the two read the same set — and answered rather than
        // `unwrap`ped because this is the one place a divergence between them would land.
        return Response::Error(format!(
            "seed: interval {interval:?} passed the permitted set but has no bar width, which              means `vike_datahub_client::seed`'s SEED_INTERVALS and `vike_model::time::interval_ms`              have diverged. Nothing was fetched."
        ));
    };
    // Gate 5.
    let repeated = match lane.admit(venue, symbol, interval, Instant::now()) {
        SeedAdmission::Fetch => false,
        SeedAdmission::Repeated => true,
        SeedAdmission::Refused(why) => return Response::Error(why),
    };
    let rows_written = if repeated {
        0
    } else {
        // A stop probe that never fires: the seed is one bounded window on the keyless kline lane,
        // which has no chunk boundary to stop at — and `backfill_verb`'s probe is that verb's alone.
        match collector(symbol, interval, start, end, &|| false) {
            Ok(n) => n as u64,
            Err(e) => return Response::Error(format!("seed {venue}/{symbol}@{interval}: {e}")),
        }
    };
    // The write-through proof, through the SERVED handle — the same read the client's follow-up
    // `LoadBars` does, so a `SeedDone` reporting bars is a promise that read will find them.
    //
    // ⚠ This stays a `load_bars`, and that is NOT the defect `backfill_verb`'s read-back had: the
    // window here is `seed_range`'s — the last `SEED_BARS` bars ending now, chosen by THIS server —
    // so the read is bounded for as long as that constant is, where `backfill_verb`'s window is
    // whatever the client asked for and goes through `HistStore::bar_edges` for that reason. This
    // lane reports only the ends too, so it could move as well; it was left alone because a read
    // that cannot hurt is not worth changing under a reply that must stay byte-identical. If
    // `SEED_BARS` ever outgrows a chart's width, move it.
    match store.load_bars(venue, symbol, interval, TsRange { start: Some(start), end: Some(end) }) {
        Ok(bars) => Response::SeriesSeeded(SeedDone {
            armed: true,
            repeated,
            rows_written,
            range: Some((start, end)),
            first_ts: bars.first().map(|b| b.ts),
            last_ts: bars.last().map(|b| b.ts),
        }),
        Err(e) => Response::Error(format!(
            "seed {venue}/{symbol}@{interval}: wrote {rows_written} rows but the read-back              failed: {e}"
        )),
    }
}

/// **Serve one venue's instrument list** — the [`Request::VenueCatalog`] handler.
///
/// ⚠ **It takes no `store` parameter, and that absence is the whole of
/// `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`'s decision 1.**
/// Its two neighbours — `backfill_verb` and `seed_series_verb` — both write the served store and
/// both read it back; this one cannot, because it is handed no handle. That is why 0058's four-part
/// rule is inapplicable here rather than satisfied, and why the additive/idempotent leg of it is
/// declared VACUOUS in that record rather than claimed: there is no durable shared state for the
/// leg to grip.
///
/// # ⚠ Almost every outcome is a SUCCESS, which is the opposite of this file's usual shape
///
/// An unarmed lane, a credentialed venue, an un-enumerable venue and a venue this build does not
/// serve are all `Response::VenueCatalog` values. Only two things are [`Response::Error`]: a
/// MALFORMED venue string, and a provider that failed mid-fetch. The reason is that "which venues
/// can I refresh" is a question the Data Manager asks every time it opens, and a stream of errors
/// is the wrong shape for a routine answer — while an EMPTY LIST would be the wrong shape for a
/// refusal, because for `ig` and `ibkr` an empty list is the truthful answer to a different
/// question (0062's decision 5).
///
/// # The gate order, and why each step is where it is
///
/// 1. **SHAPE** ([`validate_catalog_venue`]) — the one genuinely exceptional input, refused before
///    anything is consulted.
/// 2. **ARMING** — a success, and deliberately before the venue's nature: an unarmed server should
///    say so about EVERY venue rather than leaking which ones it would have served.
/// 3. **THE VENUE'S OWN NATURE** (`vike_catalog::catalog_availability`) — consulted BEFORE the
///    mounted table, so `ig` is told it has no bulk list rather than that this build lacks a
///    provider. Both are true; only the first is useful, and only the first stays true after a
///    rebuild.
/// 4. **THE TABLE** — a `PublicBulk` venue with no mounted provider is a BUILD fact, and the
///    refusal names what is served.
/// 5. **THE LANE'S BOUNDS** — last, because the bucket is the only check with a side effect a
///    refusal should not have paid for. The memo is consulted inside `admit`, before the bucket, so
///    a fresh answer spends no token.
fn venue_catalog_verb(venue: &str, lane: Option<&CatalogLane>) -> Response {
    // Gate 1.
    if let Err(why) = validate_catalog_venue(venue) {
        return Response::Error(format!("catalog: {why}"));
    }
    let listed =
        |outcome| Response::VenueCatalog(CatalogListing { venue: venue.to_string(), outcome });
    // Gate 2 — see the doc: a SUCCESS, not a refusal, and deliberately so.
    let Some(lane) = lane else {
        return listed(CatalogOutcome::NotArmed);
    };
    let supported = || lane.table().supported().iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // Gate 3.
    match catalog_availability(venue) {
        CatalogAvailability::Credentialed => {
            return listed(CatalogOutcome::Refused(CatalogRefusal::NeedsCredentials));
        }
        CatalogAvailability::NoBulkList { why } => {
            return listed(CatalogOutcome::Refused(CatalogRefusal::NoBulkList {
                why: why.to_string(),
            }));
        }
        // A well-formed slug that names no roster venue. Reported as NOT SERVED rather than as a
        // venue property, because this server genuinely cannot serve it and saying anything about
        // its catalog would be inventing a fact about a venue that does not exist.
        CatalogAvailability::UnknownVenue => {
            return listed(CatalogOutcome::Refused(CatalogRefusal::NotServed {
                supported: supported(),
            }));
        }
        CatalogAvailability::PublicBulk => {}
    }
    // Gate 4.
    let Some(provider) = lane.table().get(venue) else {
        return listed(CatalogOutcome::Refused(CatalogRefusal::NotServed {
            supported: supported(),
        }));
    };
    // Gate 5.
    let now = Instant::now();
    match lane.admit(venue, now) {
        CatalogAdmission::Cached(instruments) => listed(CatalogOutcome::Listed {
            truncated: instruments.len() >= CATALOG_MAX_INSTRUMENTS,
            cached: true,
            instruments,
        }),
        CatalogAdmission::Refused(why) => Response::Error(why),
        CatalogAdmission::Fetch => match provider() {
            Ok(instruments) => {
                // ⚠ The cap is applied by the provider seam (`crate::catalog`'s `list_via`), so a
                // full-length list IS a truncated one. Computing the flag from the length rather
                // than carrying it through the memo keeps the cached and fresh arms answering
                // identically, which is the property `cached` exists to make visible.
                let truncated = instruments.len() >= CATALOG_MAX_INSTRUMENTS;
                lane.remember(venue, instruments.clone(), now);
                listed(CatalogOutcome::Listed { instruments, truncated, cached: false })
            }
            // ⚠ NOTHING is memoized on a failure — see `CatalogLane::remember`'s doc: caching a
            // blip would turn a venue being briefly down into six hours of refusal.
            Err(e) => Response::Error(format!("catalog {venue}: {e}")),
        },
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
            vike_datahub_client::proto::advertised_md_venues(&features).is_empty(),
            "no md hub was mounted, so no `md_venue=` entry may appear: {features:?}"
        );
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
