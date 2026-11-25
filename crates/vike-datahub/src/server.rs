//! The blocking, thread-per-connection DATA server.
//!
//! [`serve`] owns a bound [`TcpListener`] and accepts forever; each accepted connection is handled
//! on its own `std::thread` running a read-frame -> handle -> write-frame loop. The store is an
//! `Arc<dyn HistoryStore + Send + Sync>`, so the server is BACKEND-AGNOSTIC — `HistoryStoreDataFusion` in
//! production, `HistoryStoreMemory` in tests — and every connection thread shares the one store cheaply.
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
//!   read timeout ([`SERVER_IDLE_READ_TIMEOUT`] unless the box wrote `config.node_idle_read_timeout_ms`,
//!   which the boot hands in as [`ServerTimings`]); a `WouldBlock`/`TimedOut` on an idle or half-open peer
//!   (laptop lid closed, cable pulled) closes the connection and frees the thread. A timeout is
//!   NEVER recovered into a resumed loop (which would desync the stream) — it always closes.
//! - **The `Hello` handshake is optional ON A KEY-LESS SERVER.** `Hello` (`vike_hub_protocol::handshake`'s
//!   `HandshakeFrame`, the one spelling every hub shares; this protocol carries no copy) negotiates the
//!   protocol version and returns the served-verb `features`, and a client that sends a normal
//!   request WITHOUT a prior `Hello` is served exactly as before — the handshake informs, it does
//!   not gate. ⚠ That was unconditional until authentication landed; on a KEYED server it is the
//!   opposite (see the next section), and the two arms must not be confused.
//!
//! # Authentication — OPT-IN, and the credential IS the switch (0025's adopting change)
//!
//! [`serve_authed`] takes `Option<HubKeys>`, and the two arms are the whole contract:
//!
//! - **`None` — key-LESS.** `Hello` is optional and informs rather than gates, and the `Welcome`
//!   bytes are IDENTICAL to the pre-auth protocol (its `nonce` field is `None` and skipped on the
//!   wire, and [`FEATURE_AUTH`] is not advertised). This is the default and it is what [`serve`] /
//!   [`serve_with_backfill`] still do, so every existing local flow — the Studio's
//!   `Backend::Remote`, `vike-cli backtest`, the GUI's store branch,
//!   `vike_datahub_client::HistoryStoreDatahub` — keeps working untouched. The loopback bind guard
//!   below is what holds the posture in that mode.
//!
//!   ⚠ **This arm is no longer "every verb", and this paragraph said so until 2026-09-07.** It read
//!   "the server behaves EXACTLY as it did before authentication existed" and "every verb is served
//!   to whoever can reach the socket"; both were true when written and are now FALSE, because
//!   `crates/vike-datahub/src/server/delete.rs`'s `delete_series_verb` answers `DatahubRefusal::DeleteKeyless` on THIS arm, before the selector is
//!   validated and before the store is touched. `DeleteSeries` is the one verb a key-less server
//!   refuses — `Backfill` is also `Scope::Write` and is still served here, and that asymmetry is
//!   the argument, not an inconsistency: a backfill writes rows a re-fetch restores, a removal takes
//!   the only copy. `docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md` is the record.
//!   The still-true claim is the narrower one now written above: the HANDSHAKE is unchanged and the
//!   `Welcome` bytes are identical.
//! - **`Some(keys)` — KEYED.** `Hello` becomes MANDATORY and must be the FIRST frame; the
//!   `Welcome` answering it carries a fresh 32-byte per-connection nonce; a
//!   `Auth` carrying the HMAC over that nonce (or a device's `PersonAuth`) must follow; and every subsequent
//!   verb is checked against the authenticated [`Scope`]. Any verb before `AuthOk` is refused and the
//!   connection closes; a handshake frame after it is refused by name and the connection kept.
//!
//! The SERVED datahub (`vike-backend datahub`) is always `Some`: it authenticates every device by this
//! box's access list (decision 0288 item 8), and an empty list admits nobody. `None` is built only by an
//! in-process caller (a test, an example), and the loopback bind guard holds its posture.
//!
//! # ⚠ This daemon serves the DATA plane ONLY (ruling 7)
//!
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0 split one served
//! surface into two daemons speaking ONE protocol. **The COMPUTE verbs left this server** —
//! `RunBacktest`, `RunSlice`, `RunParamscan`, `RunWalkforward`, `RunParamscanProfile`,
//! `RunWalkforwardProfile`, `ListStrategies` and (ruling R1's research plane, added later)
//! `RunStudy` are served by `vike-backend backtesthub --addr`
//! (`vike_backtesthub::backtesthub_server`). What stays is the store: the `HistoryStore` reads, the four
//! catalog verbs, [`Request::Backfill`] and [`Request::DeleteSeries`].
//!
//! ⚠ This said "the FOUR `HistoryStore` reads" and the count went stale the moment
//! `docs/decisions/0084-only-the-datahub-touches-the-store.md` added six more. No number replaces
//! it: `handle_request`'s arms are the roster, and `served_features` is what a client is told.
//!
//! What did NOT change is the protocol: same `Hello`/`Auth`/`Ping` handshake, same length-prefixed
//! `serde_json` frames, same [`Scope`] rules, same bind posture. The classification of a verb onto
//! its plane is `vike_hub_protocol::plane`'s roster — ONE table in the crate BELOW
//! both daemons, because a copy in each is how two daemons come to refuse the same verb.
//!
//! A backtest verb sent here does not decode into this daemon's `Request`, and is answered with
//! `vike_hub_protocol::plane`'s `wrong_plane_message` naming the command that serves it, and the connection SURVIVES; the mirror-image refusal on the
//! backtesthub is the same function with the planes swapped. `served_features` withholds the
//! compute strings, so a client that reads the handshake never sends one.
//!
//! ⚠ The ENGINE did not move and never lived here: this crate has always FORWARDED to
//! `vike-backtest`. What moved is which socket answers.
//!
//! `vike_datahub_client::proto`'s `required_scope` is the ONE authority for which verb needs which scope, and its
//! match is EXHAUSTIVE (no `_` arm) so a new wire verb cannot be added unclassified — it fails to
//! compile instead. The split, and the one part of it that is not obvious:
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
//! `crates/vike-datahub/src/market_data/hub/subscribe.rs`'s `MarketDataHub::acquire`.
//!
//! ⚠ **A field's CONTENT and a list's LENGTH are two questions, and the second one is answered
//! here.** Bounding `MarketDataSpec::symbol` at the hub's door says nothing about how many specs a request
//! may carry, and both of this plane's spec-list consumers are loops that CLONE every refusal into
//! a reply — so the refusal path was the expensive one. [`refuse_an_oversized_spec_list`] is the
//! whole-request check that closes it, on `MarketDataSubscribe.specs` and on `MarketDataUpdate`'s `add`/`remove`;
//! its own doc carries the arithmetic and why it derives no new constant.
//!
//! # The outer barrier is still the bind posture, and auth does not replace it
//!
//! The handshake is PLAINTEXT and authenticates the CONNECTION, not each frame: confidentiality
//! and integrity still come from the SSH tunnel (or a VPN) in front of it — 0025's "honest limits
//! of B" is explicit about this, and it is why the bind guard did not soften. The bind target is
//! still CLASSIFIED before the listener opens: `vike_hub_protocol::bind`'s `bind_exposure` is the pure classification
//! every node server shares, `vike_datahub_client::bind`'s `bind_decision` the policy (a non-loopback bind is
//! REFUSED without the named opt-in) — the idiom of `vike_tradehub::server`'s own decision — and the bin enforces
//! the verdict.
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
use vike_data::store::store_kind::resolve_produced_by;
use vike_data::{HistoryStore, TimestampRange};
use vike_datahub_client::MarketDataBye;
use vike_datahub_client::catalog::{
    CATALOG_MAX_INSTRUMENTS, CatalogListing, CatalogOutcome, CatalogRefusal,
};
use vike_datahub_client::market::{MarketDataFrame, MarketDataSpec};
use vike_datahub_client::proto::{
    BACKFILL_ONE_BATCH, DeleteDone, FEATURE_BACKFILL_CANCEL, Response, SeedDone,
};
use vike_datahub_client::seed::{seed_range, validate_seed_interval};
use vike_datahub_client::{BackfillDone, FEATURE_BACKFILL, FEATURE_SEED_SERIES};
use vike_hub_protocol::wire_ident::{validate_catalog_venue, validate_seed_symbol};

use crate::backfill::BackfillTable;
use crate::catalog::{CatalogAdmission, CatalogLane, CatalogNotListed};
use crate::market_data::hub::{MarketDataKey, push_attach_frame};
use crate::market_data::mailbox::{MarketDataMailbox, MarketDataMailboxReceive};
use crate::market_data::{
    MARKET_DATA_LAPSE_BUDGET, MARKET_DATA_LAPSE_WINDOW, MARKET_DATA_WRITE_TIMEOUT, MarketDataHub,
    MarketDataSessionGuard,
};
use crate::seed::{SeedAdmission, SeedLane};

#[cfg(doc)]
use std::net::TcpListener;
#[cfg(doc)]
use vike_datahub_client::proto::Request;
#[cfg(doc)]
use vike_hub_protocol::auth::FEATURE_AUTH;
#[cfg(doc)]
use vike_hub_protocol::auth::Scope;
#[cfg(doc)]
use vike_hub_protocol::frame::read_frame_raw;

// The verb FAMILIES and the connection's STAGES, one child file each — `route`'s `handle_request` is
// the router and routes into the families. What stays in this file is the `mod` lines, the public
// ceilings and entry points other crates name by this path (re-exported from `limits` and `serve`,
// so there is one name for each). The default listen address is `vike_config::DEFAULT_DATAHUB_ADDRESS`.
mod backfill;
mod bar_resample;
mod connection;
mod delete;
mod features;
mod gateway_links;
mod gateway_status;
mod handshake;
mod limits;
mod market_data;
mod operator_instruments;
mod options_chain;
mod range_reads;
mod route;
mod seed_series;
mod series_gaps;
mod series_list;
mod serve;
mod step;
mod strat;
mod symbol_sets;
mod tool_feeds;
mod upload;
mod venue_catalog;

pub use connection::{ServerLanes, access_list};
pub use features::advertise_running_program;
pub use gateway_status::{
    CredentialsRelay, GATEWAY_STATUS_TTL, GATEWAY_STATUS_WAIT, GatewayStatusLane, credentials_relay,
};
pub use limits::{
    LOAD_BARS_CEILING, MAX_CONNECTIONS, MIN_BAR_JSON_BYTES, MIN_BOOK_LEVEL_JSON_BYTES,
    MIN_CHAIN_JSON_BYTES, MIN_COHORT_JSON_BYTES, MIN_EQUITY_JSON_BYTES,
    MIN_EXECUTION_FILL_JSON_BYTES, MIN_EXECUTION_ORDER_JSON_BYTES, MIN_FINANCIAL_FACT_JSON_BYTES,
    MIN_FORECAST_JSON_BYTES, MIN_INSTITUTIONAL_HOLDING_JSON_BYTES, MIN_PERP_METRIC_JSON_BYTES,
    MIN_QUOTE_JSON_BYTES, MIN_TEXT_JSON_BYTES, MIN_TEXT_SCORE_JSON_BYTES, MIN_TRADE_JSON_BYTES,
    RANGE_READS_IN_FLIGHT, RANGE_REPLY_WRITE_TIMEOUT, RESAMPLE_SCAN_CEILING, ReadCeilings,
    SCAN_BOOK_LEVELS_CEILING, SCAN_CHAIN_CEILING, SCAN_COHORT_CEILING, SCAN_EQUITY_CEILING,
    SCAN_EXECUTION_FILLS_CEILING, SCAN_EXECUTION_ORDERS_CEILING, SCAN_FINANCIAL_FACTS_CEILING,
    SCAN_FORECASTS_CEILING, SCAN_INSTITUTIONAL_HOLDINGS_CEILING, SCAN_PERP_METRICS_CEILING,
    SCAN_QUOTES_CEILING, SCAN_TEXT_SCORES_CEILING, SCAN_TEXTS_CEILING, SCAN_TRADES_CEILING,
    ServerTimings,
};
pub use operator_instruments::{InstrumentLookup, gateway_lookup};
pub use route::NO_MARKET_DATA_PLANE;
pub use serve::{
    serve, serve_authed, serve_authed_with_handshake_deadline, serve_with_backfill,
    serve_with_import, serve_with_lanes, serve_with_options, serve_with_read_ceilings,
};
use step::StopProbe;
pub use strat::{NO_STRATEGY_LANE, keyless_lookup};
pub(crate) use venue_catalog::catalog_outcome;

#[cfg(doc)]
use market_data::refuse_an_oversized_spec_list;
#[cfg(doc)]
use vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT;
