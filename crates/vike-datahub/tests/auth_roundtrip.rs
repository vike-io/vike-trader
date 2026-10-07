//! The AUTHENTICATION gate for the datahub wire (`docs/decisions/0025-datahub-remote-posture.md`,
//! the adopting PR): hermetic, loopback-only, over the in-memory `MemHistStore` double.
//!
//! Five properties, each in its own child file under `auth_roundtrip/`:
//!
//! 1. **A key-LESS server's HANDSHAKE is unchanged, and its verb set is unchanged but for one** —
//!    not "still works", but BYTE-IDENTICAL on the `Welcome` frame, and serving every verb EXCEPT
//!    the destructive `DeleteSeries` with no handshake at all. This is the backward-compatibility
//!    contract every existing local flow (`vike-cli backtest`, the Studio's `Backend::Remote`,
//!    `RemoteHistStore`, the GUI's store branch) depends on, and it is the one an implementation is
//!    most likely to break by accident.
//!
//!    ⚠ **This property read "BYTE-IDENTICAL … and serving every verb" until 2026-09-07, and its
//!    second half is now false — the tests below say so.** `a_keyless_server_serves_no_delete_verb`
//!    drives the wire with the server built BOTH ways and asserts that a key-less one neither
//!    advertises `delete_series` nor answers the request, and
//!    `a_keyless_server_serves_every_verb_with_no_handshake` carries a NAMED arm for that verb
//!    rather than passing by substring accident. The BYTE half is untouched and still exact: a
//!    key-less `Welcome` encodes with no `nonce` field and no auth advertisement, which is what
//!    `a_keyless_servers_welcome_is_byte_identical_to_the_pre_auth_protocol` compares. `Backfill`
//!    is `Scope::Write` too and is NOT withheld for key-lessness — only `delete_series_verb`
//!    gates on `keyed` — which is the asymmetry the record argues: a backfill writes rows a
//!    re-fetch restores, a removal takes the only copy
//!    (`docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md`).
//!    ⚠ Do not read that as "these tests see a backfill served". They do not, and cannot: `spawn`
//!    passes `None` for the backfill table, so `backfill_verb` answers the missing-feature error on
//!    the very arm they drive. What `a_keyless_server_serves_every_verb_with_no_handshake` proves
//!    about `Backfill` is the narrower and load-bearing thing — the refusal it gets does NOT mention
//!    a scope, so it is not being refused for key-lessness.
//! 2. **A KEYED server refuses every verb pre-auth** — exhaustively, driven off
//!    `vike_datahub_client::proto`'s `required_scope`'s own classification rather than a hand-written list, so the
//!    table cannot silently fall behind the enum.
//! 3. **The scope split is enforced** — Observe reads but is refused `Backfill` AND the
//!    Rhai-compiling `Run*` verbs; Control does both.
//! 4. **A bad mac is denied** — wrong key, wrong scope, a tag from the tradehub domain, and a mac
//!    replayed from another connection.
//! 5. **The pre-auth bounds hold** — the frame cap and the handshake deadline.
//!
//! The composed AUTHED round trip carrying real data lives with its DataFusion sibling in
//! `auth_roundtrip/preauth_bounds.rs`, behind `serve-datafusion`.

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use vike_data::store::removal::SeriesSelector;
use vike_data::{HistStore, MemHistStore, SeriesId};
use vike_datahub::md::MdHub;
use vike_datahub::serve_authed;
use vike_datahub::server::{HANDSHAKE_DEADLINE, HANDSHAKE_MAX_FRAME_LEN};
// ⚠ `VerbScope`/`required_scope` MOVED to the light client crate when ruling 7 gave the compute
// verbs a second daemon: one table below both servers, so `vike-backtest` (layer 30) enforces the
// same classification `vike-datahub` (layer 65) does. No `pub use` shim was left behind.
use vike_datahub_client::market::{MdLane, MdSessionId, MdSpec};
use vike_datahub_client::proto::{Request, Response, write_frame};
use vike_datahub_client::proto::{VerbScope, required_scope};
use vike_datahub_client::{DatahubClient, FEATURE_AUTH, PROTO_VERSION, read_frame};
use vike_node_proto::auth::{self, DATAHUB_DOMAIN, NodeKeys, Scope};

mod common;
use common::{OBSERVE_KEY, exchange, keys};

// ⚠ `MINIMAL_BAR_PROFILE` is GONE from this file: it existed to give `RunBacktest` a valid payload,
// and that verb is the COMPUTE daemon's since ruling 7. The profile itself moved with it, to
// `crates/vike-backtest/tests/compute_plane.rs`.

/// Bind an ephemeral loopback listener and spawn `serve_authed` over a fresh in-memory store.
///
/// ⚠ **No `MdHub` — this is the DEFAULT-BUILD path**, and that is what makes
/// `a_hubless_server_refuses_md_subscribe_and_stays_positional` a test of the
/// shipped default rather than of a configuration nobody runs.
fn spawn(keys: Option<NodeKeys>) -> SocketAddr {
    spawn_with_md(keys, None)
}

/// [`spawn`] with an optional market-data hub mounted.
fn spawn_with_md(keys: Option<NodeKeys>, md: Option<Arc<MdHub>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve_authed(listener, store, None, keys, md, None, None);
    });
    addr
}

/// One sample of every `Request` variant THIS DAEMON SERVES — the DATA plane plus the handshake.
///
/// ⚠ **The seven COMPUTE verbs are deliberately absent since ruling 7** (`docs/superpowers/specs/
/// 2026-09-09-datahub-market-data-wire-design.md`). They still DECODE here — one schema, two
/// daemons — but this server answers them with a wrong-plane refusal before the scope check runs,
/// so driving them through the auth tests below would prove the refusal, not the authentication.
/// Their refusal is `tests/plane_split.rs`'s subject; their SCOPE classification is still pinned
/// below, because `required_scope` is one table for both daemons.
///
/// ⚠ Kept in step with the enum by
/// `the_sample_set_covers_every_verb_scope_classification` below, not by hope: that test asserts
/// this set hits all three [`VerbScope`]s and both Control sub-families (the write verb AND the
/// Rhai-compiling ones), so a new verb that changes the shape of the classification cannot leave
/// this file quietly under-covering.
fn every_request() -> Vec<Request> {
    let series = SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1h".to_string()));
    vec![
        Request::Hello { proto_version: PROTO_VERSION },
        Request::Auth { scope: Scope::Read, mac: vec![0u8; 32] },
        Request::Ping,
        Request::LoadBars {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanQuotes {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanTrades {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::PropertiesAsOf { venue: "binance".into(), symbol: "BTCUSDT".into(), ts: 0 },
        // The SIX tick-level and research reads (`docs/decisions/0084`). Ordinary POSITIONAL
        // Observe verbs — one frame out, one frame back — so they belong on the shared long-lived
        // connection every sweep below drives, unlike `MdSubscribe` further down. These servers
        // mount a store holding none of these kinds, so each answers an empty success or a store
        // error; both are what the sweeps assert against (a refusal naming a SCOPE is the failure
        // they look for, and neither of those is one).
        Request::ScanBookUpdates {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanDepth {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        // ⚠ `asset`, not `symbol` — this verb's middle field is the odd one out, and spelling it
        // here is what keeps the sweep honest about the shape it is driving.
        Request::ScanCohort {
            venue: "binance".into(),
            asset: "BTC".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanPerpMetrics {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanEquity {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanExecFills { venue: "binance".into(), symbol: "BTCUSDT".into() },
        Request::ListSeries,
        Request::Inventory,
        // 0084's SEVENTH verb — an ordinary positional Observe read, so it rides the shared
        // long-lived connection every sweep below drives.
        Request::SeriesFacts { id: series.clone() },
        Request::SeriesGaps { id: series },
        Request::Backfill {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            start: 0,
            end: 1,
        },
        Request::DeleteSeries {
            selector: SeriesSelector::new("bar", "binance"),
            produced_by: Some("klines:".to_string()),
            dry_run: true,
        },
        // The operator's door onto RUNNING backfills (`docs/decisions/0101`): the list is Observe
        // and the cancel is Control. These servers mount no collector table, so both answer the
        // capability refusal, which names no scope — the sweeps below then see the list REACHED on
        // every connection and the cancel REACHED on a key-less server and on Control, and refused
        // on SCOPE for Observe. `crates/vike-datahub/tests/backfill_cancel.rs` drives both against a
        // MOUNTED table, where they answer for real.
        Request::ListBackfills,
        Request::CancelBackfill {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
        },
        // The HISTORY-CHANNELS read (`docs/decisions/0102`): Observe, served on EVERY build — these
        // servers mount no table and still answer it, every built row then reading not mounted — so
        // the sweeps below see it REACHED on every connection, Observe included.
        Request::HistoryChannels,
        // The ARCHIVE IMPORT — the second store WRITE on the Control side. A PLAN-ONLY request, so
        // even a server that mounts a lane would write nothing; these servers mount none and answer
        // the capability refusal, which names no scope — so the sweeps below see the verb REACHED on
        // a key-less server and on a Control connection, and refused on SCOPE for Observe.
        Request::ImportArchive(vike_datahub_client::archive::ImportSpec {
            format: "dukascopy-bi5".into(),
            dataset: "EURUSD".into(),
            from_day: None,
            to_day: None,
            bars: vec!["1m".into()],
            dry_run: true,
            verify: false,
        }),
        // The CHART-GAP SEED — the one WRITE-shaped verb on the OBSERVE side of `required_scope`
        // (`docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`), so it belongs in every
        // sweep below and specifically in `observe_reads`: an Observe connection must be ALLOWED it.
        // Safe to drive on the shared long-lived connection because these servers mount no seed
        // lane, and an UNARMED lane answers a positional `SeriesSeeded { armed: false }` having
        // called no venue — the ordinary success this classification rests on.
        Request::SeedSeries {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            // 0061 Phase 3 added this field. `None` is the pre-field frame BYTE-IDENTICALLY
            // (`skip_serializing_if`), which is what keeps this sweep a statement about the
            // verb rather than about the field.
            class: None,
        },
        // The VENUE CATALOG — Observe like its neighbour above, but for the OPPOSITE reason: that
        // one is a WRITE that earns Observe by passing 0058's four-part rule, this one is not a
        // write at all and is outside it
        // (`docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`).
        // Belongs in every sweep below and specifically in `observe_reads`: an Observe connection
        // must be ALLOWED it. Safe to drive on the shared long-lived connection because these
        // servers mount no catalog lane, and an UNARMED lane answers a positional
        // `VenueCatalog(CatalogListing { outcome: NotArmed, .. })` having called no venue — the
        // ordinary success this classification rests on.
        Request::VenueCatalog { venue: "binance".into() },
        // The market-data SET MUTATION — an ordinary positional Observe verb on a short-lived
        // connection, so it belongs in every sweep below.
        Request::MdUpdate { session: MdSessionId::fresh(), add: Vec::new(), remove: Vec::new() },
        // ⚠⚠ **`Request::MdSubscribe` IS DELIBERATELY ABSENT, AND PUTTING IT HERE BREAKS THIS FILE
        // SILENTLY.** It is this wire's one MODE SWITCH: `observe_reads` and `control_does_both`
        // drive every verb in this list over ONE long-lived connection through `exchange` (write
        // one frame, read one frame), and a subscribed connection stops answering positionally —
        // the loop's next `exchange` would read a pushed `MdFrame::Heartbeat` as if it were the
        // next verb's REPLY, and every remaining assertion in the sweep would pass against the
        // wrong frame. It fails SILENTLY, not loudly, which is the worst way for a test to be
        // wrong.
        //
        // What it costs to leave it out is REAL and is paid for by name elsewhere, because
        // `MdSubscribe` is the one verb in this protocol that converts a connection into an
        // UNBOUNDED WRITER and a pre-auth leak of it would be a market-data firehose plus a venue
        // refcount for an unauthenticated peer:
        //   * the pre-auth refusal — `a_keyed_server_refuses_md_subscribe_before_auth`;
        //   * the mode switch itself — `md_subscribe_is_the_last_positional_frame_on_its_socket`;
        //   * the hub-less refusal — `a_hubless_server_refuses_md_subscribe_and_stays_positional`.
        // And `the_sample_set_covers_every_verb_scope_classification` asserts the mode-switch verbs
        // ARE covered somewhere, so this omission cannot be confused with a forgotten one.
    ]
}

/// Open a socket and complete the handshake as `scope`, returning the authenticated stream.
fn authed_stream(addr: SocketAddr, keys: &NodeKeys, scope: Scope) -> TcpStream {
    let mut s = TcpStream::connect(addr).expect("connect");
    write_frame(&mut s, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut s).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce.expect("a keyed server's Welcome carries a nonce"),
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(DATAHUB_DOMAIN, keys.key_for(scope), &nonce, PROTO_VERSION, scope);
    write_frame(&mut s, &Request::Auth { scope, mac }).expect("auth");
    match read_frame::<_, Response>(&mut s).expect("auth answer") {
        Response::AuthOk { scope: granted } => assert_eq!(granted, scope),
        other => panic!("expected AuthOk, got {other:?}"),
    }
    s
}

#[path = "auth_roundtrip/keyed_and_bad_mac.rs"]
mod keyed_and_bad_mac;
#[path = "auth_roundtrip/keyless.rs"]
mod keyless;
#[path = "auth_roundtrip/md_mode_switch.rs"]
mod md_mode_switch;
#[path = "auth_roundtrip/preauth_bounds.rs"]
mod preauth_bounds;
#[path = "auth_roundtrip/scope_split.rs"]
mod scope_split;
