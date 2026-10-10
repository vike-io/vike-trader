//! The blocking, thread-per-connection DATA server.
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
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Instant;

use vike_catalog::{CatalogAvailability, catalog_availability};
use vike_data::store::removal;
use vike_data::{HistStore, TsRange};
use vike_datahub_client::MdBye;
use vike_datahub_client::catalog::{
    CATALOG_MAX_INSTRUMENTS, CatalogListing, CatalogOutcome, CatalogRefusal, validate_catalog_venue,
};
use vike_datahub_client::market::{MD_HEARTBEAT, MdFrame, MdSpec};
use vike_datahub_client::proto::{
    BACKFILL_ONE_BATCH, DeleteDone, FEATURE_BACKFILL_CANCEL, Response, SeedDone,
    resolve_produced_by, write_frame,
};
use vike_datahub_client::seed::{seed_range, validate_seed_interval, validate_seed_symbol};
use vike_datahub_client::{BackfillDone, FEATURE_BACKFILL, FEATURE_SEED_SERIES};

use crate::backfill::BackfillTable;
use crate::catalog::{CatalogAdmission, CatalogLane};
use crate::md::hub::{MdKey, push_attach_frame};
use crate::md::mailbox::{Mailbox, Recv};
use crate::md::{MD_LAPSE_BUDGET, MD_LAPSE_WINDOW, MD_WRITE_TIMEOUT, MdHub, SessionGuard};
use crate::seed::{SeedAdmission, SeedLane};

#[cfg(doc)]
use std::net::TcpListener;
#[cfg(doc)]
use vike_datahub_client::FEATURE_AUTH;
#[cfg(doc)]
use vike_datahub_client::proto::{Request, read_frame_raw};
#[cfg(doc)]
use vike_node_proto::auth::Scope;

// The verb FAMILIES and the connection's STAGES, one child file each — `route`'s `handle_request` is
// the router and routes into the families. What stays in this file is the `mod` lines, the public
// ceilings and entry points other crates name by this path (re-exported from `limits` and `serve`,
// so there is one name for each) and `DEFAULT_ADDR`.
mod backfill;
mod connection;
mod delete;
mod features;
mod handshake;
mod limits;
mod market_data;
mod range_reads;
mod route;
mod seed_series;
mod serve;
mod step;
mod venue_catalog;

pub use features::KEYLESS_DELETE_REFUSAL;
pub use limits::{
    HANDSHAKE_DEADLINE, HANDSHAKE_MAX_FRAME_LEN, LOAD_BARS_CEILING, MAX_CONNECTIONS,
    MIN_BAR_JSON_BYTES, MIN_BOOK_LEVEL_JSON_BYTES, MIN_COHORT_JSON_BYTES, MIN_EQUITY_JSON_BYTES,
    MIN_EXEC_FILL_JSON_BYTES, MIN_PERP_METRIC_JSON_BYTES, MIN_QUOTE_JSON_BYTES,
    MIN_TRADE_JSON_BYTES, ReadCeilings, SCAN_BOOK_LEVELS_CEILING, SCAN_COHORT_CEILING,
    SCAN_EQUITY_CEILING, SCAN_EXEC_FILLS_CEILING, SCAN_PERP_METRICS_CEILING, SCAN_QUOTES_CEILING,
    SCAN_TRADES_CEILING,
};
pub use route::NO_MARKET_DATA_PLANE;
pub use serve::{
    serve, serve_authed, serve_authed_with_handshake_deadline, serve_with_backfill,
    serve_with_import, serve_with_read_ceilings,
};
use step::StopProbe;

#[cfg(doc)]
use limits::IDLE_READ_TIMEOUT;
#[cfg(doc)]
use market_data::refuse_an_oversized_spec_list;

/// The default listen address — localhost only, reached over an SSH tunnel
/// (`ssh -L 7878:localhost:7878 <box>`), the same posture as the tradehub node server's
/// `DEFAULT_ADDR`. The listener MUST stay bound to loopback: this protocol authenticates NOTHING
/// (see the module doc), so reachability is not defense in depth here — it is the whole barrier.
pub const DEFAULT_ADDR: &str = "127.0.0.1:7878";
