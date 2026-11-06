//! The public entries and the accept loop behind them, with the connection cap's slot.

use std::io;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use vike_data::HistStore;
use vike_hub_proto::auth::HubKeys;
use vike_hub_proto::limits::SERVER_HANDSHAKE_DEADLINE;

use crate::backfill::BackfillTable;
use crate::catalog::CatalogLane;
use crate::import::ImportLane;
use crate::md::MdHub;
use crate::options::OptionsLane;
use crate::seed::SeedLane;

use super::connection::{ConnFacts, ServerLanes, handle_connection};
use super::limits::{MAX_CONNECTIONS, ReadCeilings, ServerTimings};

#[cfg(doc)]
use super::features::KEYLESS_DELETE_REFUSAL;
#[cfg(doc)]
use vike_datahub_client::proto::{Request, Response};
#[cfg(doc)]
use vike_datahub_client::{FEATURE_AUTH, FEATURE_BACKFILL, FEATURE_MARKET_DATA};
#[cfg(doc)]
use vike_hub_proto::limits::SERVER_HANDSHAKE_MAX_FRAME_LEN;

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

/// [`serve_with_backfill`] with OPTIONAL `HubKeys` authentication — the full entry, and the one
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
///   `vike_datahub_client::proto`'s `required_scope`. Pre-auth frames are read under [`SERVER_HANDSHAKE_MAX_FRAME_LEN`] and the whole
///   handshake under [`SERVER_HANDSHAKE_DEADLINE`].
///
/// ⚠ A `Some(keys)` whose scopes are BOTH empty cannot happen through
/// `vike_hub_proto::auth::hub_keys_from_vars` (it returns `None` when neither key is
/// configured), and if one is constructed by hand it is a CLOSED server, not an open one: every
/// `Auth` fails against an empty key, which is the credential-is-the-gate idiom's safe direction.
/// ⚠ `md` mounts the LIVE MARKET-DATA plane (`crate::md`). `None` — every entry above, and any
/// build whose operator did not write the `flags.datahub_live` row — serves no market-data verb:
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
    keys: Option<HubKeys>,
    md: Option<Arc<MdHub>>,
    seed: Option<Arc<SeedLane>>,
    catalog: Option<Arc<CatalogLane>>,
) -> io::Result<()> {
    serve_with_import(listener, store, backfill, keys, md, seed, catalog, None, false)
}

/// [`serve_authed`] with the ARCHIVE IMPORT lane as an eighth argument.
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
///
/// `journal_materializer_mounted` — whether THIS process runs the journal materializer (it writes
/// live executions into `kind=exec_fill` / `kind=exec_order`). The datahub's boot supplies it, being
/// the one place that decides to spawn one; it gates `Welcome`'s
/// `vike_datahub_client::proto::FEATURE_EXEC_MATERIALIZE` and nothing else. Every other entry above
/// passes `false`: none of them spawns a materializer.
pub fn serve_with_import(
    listener: TcpListener,
    store: Arc<dyn HistStore + Send + Sync>,
    backfill: Option<BackfillTable>,
    keys: Option<HubKeys>,
    md: Option<Arc<MdHub>>,
    seed: Option<Arc<SeedLane>>,
    catalog: Option<Arc<CatalogLane>>,
    import: Option<Arc<ImportLane>>,
    journal_materializer_mounted: bool,
) -> io::Result<()> {
    serve_with_options(
        listener,
        store,
        backfill,
        keys,
        md,
        seed,
        catalog,
        import,
        journal_materializer_mounted,
        None,
    )
}

/// [`serve_with_import`] with the OPTIONS lane as a tenth argument.
///
/// `options`:
/// - **`None`** — byte-identical to [`serve_with_import`]: `Request::OptionsChain` is answered with
///   a successful `OptionsChainOutcome::NotArmed` having polled nothing. `Welcome` advertises
///   `options_chain` EITHER way — a build fact (`vike_datahub_client::FEATURE_OPTIONS_CHAIN`'s doc).
/// - **`Some(lane)`** — the verb answers from the lane's snapshots (`crate::options`).
///
/// ⚠ A separate entry for [`serve_with_import`]'s reason: its callers mount no options lane.
pub fn serve_with_options(
    listener: TcpListener,
    store: Arc<dyn HistStore + Send + Sync>,
    backfill: Option<BackfillTable>,
    keys: Option<HubKeys>,
    md: Option<Arc<MdHub>>,
    seed: Option<Arc<SeedLane>>,
    catalog: Option<Arc<CatalogLane>>,
    import: Option<Arc<ImportLane>>,
    journal_materializer_mounted: bool,
    options: Option<Arc<OptionsLane>>,
) -> io::Result<()> {
    serve_with_lanes(
        listener,
        store,
        backfill,
        keys,
        md,
        seed,
        ServerLanes { catalog, options, tool_feeds: None, strat: None },
        import,
        journal_materializer_mounted,
        ServerTimings::PRODUCTION,
    )
}

/// [`serve_with_options`] with the OBSERVE lanes as ONE [`ServerLanes`] value — the entry the bin calls.
///
/// ⚠ A struct rather than an eleventh positional argument: this signature was at clippy's ten-argument
/// ceiling with the options lane, and the lanes are exactly the arguments that keep arriving
/// (`ServerLanes`' own doc). `catalog` and `options` move into it; the earlier entries stay as they
/// were and build the value themselves.
///
/// `lanes.tool_feeds`:
/// - **`None`** — byte-identical to [`serve_with_options`]: `Request::NewsFeed`,
///   `Request::EconomicCalendar` and `Request::EquityCalendar` are answered with a successful
///   `ToolFeedOutcome::NotArmed` having polled nothing. `Welcome` advertises `tool_feeds` EITHER
///   way — a build fact (`vike_datahub_client::FEATURE_TOOL_FEEDS`'s doc).
/// - **`Some(lane)`** — the verbs answer from the lane's caches (`crate::tool_feeds`).
///
/// ⚠ A separate entry for [`serve_with_import`]'s reason: its callers mount no tool-feeds lane.
///
/// `timings` — the idle read timeout and the market-data heartbeat period, the two connection facts
/// an operator may set (`config.node_idle_read_timeout_ms`, `config.md_heartbeat_ms`). The boot builds
/// it with [`ServerTimings::from_rows`] from the box's rows; every entry above passes
/// [`ServerTimings::PRODUCTION`], so a box with no row serves exactly the constants it always did. It
/// is the tenth parameter and the last this signature takes before a struct is due (clippy's
/// `too-many-arguments-threshold` is 10): the next fact joins [`ServerTimings`] or [`ServerLanes`].
pub fn serve_with_lanes(
    listener: TcpListener,
    store: Arc<dyn HistStore + Send + Sync>,
    backfill: Option<BackfillTable>,
    keys: Option<HubKeys>,
    md: Option<Arc<MdHub>>,
    seed: Option<Arc<SeedLane>>,
    lanes: ServerLanes,
    import: Option<Arc<ImportLane>>,
    journal_materializer_mounted: bool,
    timings: ServerTimings,
) -> io::Result<()> {
    let facts = ConnFacts {
        ceilings: ReadCeilings::PRODUCTION,
        handshake_deadline: SERVER_HANDSHAKE_DEADLINE,
        timings,
        journal_materializer_mounted,
    };
    serve_inner(listener, store, backfill, keys, md, seed, lanes, import, facts)
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
    let facts = ConnFacts {
        ceilings,
        handshake_deadline: SERVER_HANDSHAKE_DEADLINE,
        timings: ServerTimings::PRODUCTION,
        journal_materializer_mounted: false,
    };
    serve_inner(listener, store, None, None, None, None, ServerLanes::default(), None, facts)
}

/// [`serve_authed`] with the pre-auth handshake deadline as a PARAMETER — a test seam, the
/// [`serve_with_read_ceilings`] shape: production always passes [`SERVER_HANDSHAKE_DEADLINE`], and a test
/// that must watch the deadline fire waits half a second instead of ten. Nothing else differs: no
/// backfill, market-data, seed, catalog or import lane is mounted.
pub fn serve_authed_with_handshake_deadline(
    listener: TcpListener,
    store: Arc<dyn HistStore + Send + Sync>,
    keys: Option<HubKeys>,
    deadline: Duration,
) -> io::Result<()> {
    let facts = ConnFacts {
        ceilings: ReadCeilings::PRODUCTION,
        handshake_deadline: deadline,
        timings: ServerTimings::PRODUCTION,
        journal_materializer_mounted: false,
    };
    serve_inner(listener, store, None, keys, None, None, ServerLanes::default(), None, facts)
}

/// The accept loop behind every entry above. `facts` carries the range reads' ceilings —
/// [`ReadCeilings::PRODUCTION`] everywhere but [`serve_with_read_ceilings`] —, the pre-auth
/// handshake deadline — [`SERVER_HANDSHAKE_DEADLINE`] everywhere but
/// [`serve_authed_with_handshake_deadline`] —, the operator's two timings —
/// [`ServerTimings::PRODUCTION`] everywhere but [`serve_with_lanes`] — and the boot's materializer
/// fact ([`serve_with_import`]'s `journal_materializer_mounted`).
fn serve_inner(
    listener: TcpListener,
    store: Arc<dyn HistStore + Send + Sync>,
    backfill: Option<BackfillTable>,
    keys: Option<HubKeys>,
    md: Option<Arc<MdHub>>,
    seed: Option<Arc<SeedLane>>,
    lanes: ServerLanes,
    import: Option<Arc<ImportLane>>,
    facts: ConnFacts,
) -> io::Result<()> {
    let backfill = backfill.map(Arc::new);
    let keys = keys.map(Arc::new);
    match keys.as_deref() {
        Some(k) => tracing::info!(
            keys = ?k,
            "vike-datahub: AUTHENTICATION REQUIRED — every connection must complete the HubKeys \
             handshake before any verb is answered (the `Debug` above reports key PRESENCE only)"
        ),
        None => tracing::info!(
            "vike-datahub: no datahub hub keys configured — serving UNAUTHENTICATED (the loopback \
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
                let lanes = lanes.clone();
                let import = import.clone();
                thread::Builder::new().name("dh-conn".into()).spawn(move || {
                    let _slot = slot;
                    handle_connection(
                        stream,
                        store,
                        backfill,
                        keys.as_deref(),
                        md,
                        seed,
                        lanes,
                        import,
                        facts,
                    )
                })?;
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
/// type: both crates declare `layer = 65` and `crates/vike-ops/tests/architecture/layer_gate.rs` fails on
/// `to >= from`, the same reason `crate::md::mailbox` re-implements that daemon's `Mailbox`.
struct ConnSlot(Arc<AtomicUsize>);

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
