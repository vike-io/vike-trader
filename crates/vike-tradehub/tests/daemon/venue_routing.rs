//! **Which BOOK does a remote control command land on?** — the node-side routing gate, end to end
//! over a real TWO-ENGINE paper core, a real [`vike_tradehub::server`] and a real control
//! handshake. Loopback only; no creds, no network, no feature flags.
//!
//! # The defect this suite exists for
//!
//! Every order-carrying variant of `WireCommand` names a venue and always has, and the daemon's
//! `lower_command` copies that string onto `vike_model::OrderRequest::venue`. What was missing was
//! anyone CHECKING it: `vike_core`'s `CoreThread::route_of` resolves the string to an engine index
//! and takes `.unwrap_or(0)` when it resolves to none, and engine 0 is the PRIMARY. So a control
//! peer naming a venue this process ran no engine for — an operator's DOM on a second exchange, a
//! `submit okx …` against a binance-primary daemon, a typo — had its order risk-gated, signed and
//! sent by the PRIMARY venue's execution client, answered `Ack`, and shown in the snapshot. No
//! error, anywhere, on the path that signs real orders.
//!
//! # What is proven here
//!
//! - **Routing is by the address, and a different address is a different book.** Two submits, two
//!   venues, one daemon: each order appears in the snapshot owned by the engine it named
//!   (`OrderView::venue` is documented as "owning engine's venue"), so a misroute is visible as a
//!   wrong owner rather than inferred.
//! - **An address this node cannot match is REFUSED, naming what it does have.** The reply is a
//!   `Response::Error` and NOTHING is booked — not on the named venue, not on the primary.
//! - **…and so is an ACCOUNT of a venue it DOES run**, which is the same defect one field along
//!   and reached the same way: `server::refusal::account_refusal` on `accept_command`'s step 1c, against
//!   the published ROUTE KEYS rather than the venue ids. Its pure verdict has a unit suite; what
//!   is proven HERE is that the gate is on the PATH, which is the half a refactor can silently
//!   delete. `DEFAULT` over the wire is accepted, because it NAMES the unlabelled book rather than
//!   declining to name one.
//! - **…and so is a runtime MOUNT naming one**, which is the same gate one plane up and the one
//!   whose un-gated failure is an `Ack` rather than a differently-worded error — a mount's spec
//!   validates fine, so nothing else at the edge had anything to say about the account it named.
//! - **…and so is a mass-cancel, flatten or market exit naming one** (owner ruling "B",
//!   2026-09-26) — plus an account named with NO venue, refused whatever the roster, while the
//!   account-less shapes and the unscoped panic button are still Acked on the same connection. The
//!   node advertises `account-scoped-reduce` in the same breath, and the dry-run answers alike.
//! - **An UNADDRESSED command still reaches today's engine set**, driven from wire BYTES with no
//!   venue in them rather than from a struct carrying a `None`, so the compatibility claim is
//!   about what an older client actually writes.
//! - **A submit with the venue KEY ABSENT never decoded in the first place** — the honest form of
//!   "an old client sends an unaddressed order", which on this wire has never existed.
//! - **The capability is advertised**, so a client can tell this node from one that misroutes
//!   silently (`FEATURE_VENUE_ROUTING`; the client half is
//!   `vike_app_core::backend::tradehub_control::venue_routing_verdict`).
//! - **The dry-run answers the same verdict**, because a preview that stays quiet about the order
//!   landing on a different book than it names is worse than no preview.
//! - **A TP/SL BRACKET books its entry on the engine it names and HOLDS both exits**, and the node
//!   refuses before the `Ack` the brackets it cannot honour — an unpublished roster, a symbol its
//!   engine does not trade, ANY bracket to an engine mounted on binance's spot lane (including the
//!   `.P` one), a bad value, each previewed as refused in the same sentence — while a bracket the
//!   VENUE cannot hold is refused whole by the core.

use std::net::{SocketAddr, TcpStream};

use vike_exec::BarUpdate;
use vike_marketdata::test_support::flat_bar_zero_volume;
use vike_mount::{MultiStrategyMount, build_paper_multi_strategy_core_with};
use vike_tradehub::config::DaemonProfile;
use vike_tradehub::publish::{self, PublisherHandle};
use vike_tradehub_client::auth;
use vike_tradehub_client::proto::{
    FEATURE_ACCOUNT_SCOPED_REDUCE, FEATURE_BRACKET, FEATURE_VENUE_ROUTING, NODE_PROTO_VERSION,
    Request, Response, Scope, read_frame, write_frame,
};
use vike_tradehub_client::wire::{WireBracketSpec, WireCommand, WireOrderRequest};
use vike_tradehub_client::{NodeKeys, RemoteCoreHandle};

use crate::support::{opts_pinned_to, own_sentinel, resolve_mounts, serve_on_loopback, wait_until};

const OBSERVE_KEY: &[u8] = b"observe-secret-key-for-venue-routing-tests";
const CONTROL_KEY: &[u8] = b"control-secret-key-for-venue-routing-tests";

/// The two venues this suite's daemon mounts. `polymarket` is declared FIRST, so it is the PRIMARY
/// engine — which is what makes the binance assertions load-bearing: an order that failed to route
/// would land on polymarket, not on the venue it named.
const PRIMARY_VENUE: &str = "polymarket";
const PRIMARY_SYMBOL: &str = "VR_ROUTE_TOK";
const SECOND_VENUE: &str = "binance";
/// A binance PERP symbol (the `.P` suffix picks the perp lane), so the bracket tests below can book
/// on the second venue at all: the node refuses a bracket to a binance engine mounted on the SPOT
/// lane, whose stop-loss the venue cannot hold (`crates/vike-tradehub/src/server/refusal.rs`'s
/// `bracket_engine_refusal`). Every other test here is lane-blind.
const SECOND_SYMBOL: &str = "VR_ROUTE_BTC.P";
/// The SPOT twin of [`SECOND_SYMBOL`]. The ordinary fixture mounts it nowhere, so a bracket naming
/// it there names a symbol its engine does not trade; [`spawn_spot_binance_node`] mounts binance ON
/// it, the shape of the shipped daemon, whose binance engine is spot.
const SECOND_SPOT_SYMBOL: &str = "VR_ROUTE_BTC";
/// A venue id no engine in this daemon is built for. It IS a real roster venue
/// (`vike_model::VENUES`), deliberately: a refusal that only triggered on nonsense strings would
/// miss the case that actually happens, which is an operator naming a venue the platform supports
/// and THIS node does not run.
const UNMOUNTED_VENUE: &str = "okx";

/// The TWO-VENUE profile every test here mounts: one `buy_hold` row per venue, polymarket first,
/// binance on [`SECOND_SYMBOL`].
fn two_venue_profile() -> DaemonProfile {
    two_venue_profile_with_binance_on(SECOND_SYMBOL)
}

/// [`two_venue_profile`] with binance's one mount on `binance_symbol` — which is the symbol its
/// ENGINE is mounted on, and so the lane every order sent to that engine travels.
fn two_venue_profile_with_binance_on(binance_symbol: &str) -> DaemonProfile {
    DaemonProfile::from_toml_str(&format!(
        r#"
[[mounts]]
venue = "polymarket"
symbol = "VR_ROUTE_TOK"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 1.0

[[mounts]]
venue = "binance"
symbol = "{binance_symbol}"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 1.0
"#
    ))
    .expect("a two-venue [[mounts]] profile parses and validates")
}

/// Build the two-engine paper node, publish it, and serve it on an ephemeral loopback port with a
/// CONTROL key and the core's `CommandSink` threaded in.
///
/// ⚠ It WAITS for the core to publish its engine set before returning. That is not tidiness: the
/// routing gate reads the roster off the published snapshot, and an EMPTY roster is treated as
/// UNKNOWN and refuses nothing (`server::refusal::venue_refusal` argues why refusing on it would deadlock a
/// feed-less daemon for ever). A test that raced that window would pass or fail on timing rather
/// than on the property, which is the one thing a routing test must not do.
fn spawn_two_venue_node(
    tag: &str,
) -> (MultiStrategyMount, tempfile::TempDir, PublisherHandle, SocketAddr) {
    let (mount, halt_dir, publisher, addr) = spawn_unpublished_two_venue_node(tag);
    publish_both_engines(&mount, &publisher);
    (mount, halt_dir, publisher, addr)
}

/// Drive the first publish: one bar per mount so the core folds, goes dirty and PUBLISHES — the
/// only way its engine set becomes visible to the server (a paper daemon with no feed publishes
/// nothing until something happens to it) — then wait until both engines are on the roster.
fn publish_both_engines(mount: &MultiStrategyMount, publisher: &PublisherHandle) {
    publish_both_engines_with_binance_on(mount, publisher, SECOND_SYMBOL);
}

/// [`publish_both_engines`] for a node whose binance engine is mounted on `binance_symbol`.
fn publish_both_engines_with_binance_on(
    mount: &MultiStrategyMount,
    publisher: &PublisherHandle,
    binance_symbol: &str,
) {
    let bars = mount.handle.bar_sender();
    for (venue, symbol) in [(PRIMARY_VENUE, PRIMARY_SYMBOL), (SECOND_VENUE, binance_symbol)] {
        bars.close(BarUpdate {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: "1m".to_string(),
            bar: flat_bar_zero_volume(60_000, 0.50),
        })
        .expect("the core is alive");
    }
    assert!(
        wait_until(10, || {
            let v = publisher.engine_venues();
            v.iter().any(|x| x == PRIMARY_VENUE) && v.iter().any(|x| x == SECOND_VENUE)
        }),
        "the core must publish BOTH engines before the routing gate can check an address; got {:?}",
        publisher.engine_venues()
    );
}

/// [`spawn_two_venue_node`] WITHOUT the first publish: the node serves, and its published roster is
/// still EMPTY — the window the gates read as UNKNOWN. Only the bracket tests want this; every other
/// test must go through [`spawn_two_venue_node`], for the timing reason its doc gives.
fn spawn_unpublished_two_venue_node(
    tag: &str,
) -> (MultiStrategyMount, tempfile::TempDir, PublisherHandle, SocketAddr) {
    spawn_unpublished_node_of(tag, &two_venue_profile())
}

/// ⚠ **The SHIPPED daemon's shape: a binance engine mounted on the SPOT lane** (its
/// `crates/vike-tradehub/src/wired_markets.rs`'s `BINANCE_MARKET` is spot `BTCUSDT`), published and
/// served. The ordinary fixture's binance engine is a perp, so it can never show what a bracket does
/// to a spot-mounted engine — which is where the frame's symbol and the engine's lane disagree.
fn spawn_spot_binance_node(
    tag: &str,
) -> (MultiStrategyMount, tempfile::TempDir, PublisherHandle, SocketAddr) {
    let (mount, halt_dir, publisher, addr) =
        spawn_unpublished_node_of(tag, &two_venue_profile_with_binance_on(SECOND_SPOT_SYMBOL));
    publish_both_engines_with_binance_on(&mount, &publisher, SECOND_SPOT_SYMBOL);
    (mount, halt_dir, publisher, addr)
}

/// Serve `profile`'s paper node, unpublished — the body [`spawn_unpublished_two_venue_node`] and
/// [`spawn_spot_binance_node`] share.
fn spawn_unpublished_node_of(
    tag: &str,
    profile: &DaemonProfile,
) -> (MultiStrategyMount, tempfile::TempDir, PublisherHandle, SocketAddr) {
    let (halt_dir, sentinel) = own_sentinel("venue-routing", tag);
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(profile), opts_pinned_to(&sentinel));

    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let addr = serve_on_loopback(
        publisher.clone(),
        NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()),
        Some(mount.handle.command_sink()),
        None,
        None,
    );
    (mount, halt_dir, publisher, addr)
}

/// A resting limit far from any driven price — it stays WORKING, so the snapshot deterministically
/// carries it with the engine that owns it.
fn resting_submit(coid: &str, venue: &str, symbol: &str) -> WireCommand {
    resting_submit_for(coid, venue, symbol, None)
}

/// [`resting_submit`] NAMING AN ACCOUNT — the three wire states of
/// `WireOrderRequest::account` (absent, `DEFAULT`, a label) reach the daemon from here, because the
/// account gate's whole subject is which of the three arrived.
fn resting_submit_for(coid: &str, venue: &str, symbol: &str, account: Option<&str>) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: coid.to_string(),
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty: 20.0,
        order_type: "limit".to_string(),
        price: Some(0.10),
        trigger_price: None,
        reduce_only: false,
        account: account.map(str::to_string),
    })
}

/// A RUNTIME MOUNT frame naming an account — the mount plane's twin of [`resting_submit_for`].
/// `buy_hold` with default params is deliberately a spec the edge's own
/// `vike_tradehub::mount_factory::validate_spec` ACCEPTS, so nothing but the account gate can
/// refuse it and a refusal cannot be mistaken for a spec complaint. The `controller_id` is given
/// explicitly so an accepted mount lands on its own identity rather than colliding with the
/// profile row already mounted on this series.
fn mount_on(venue: &str, symbol: &str, account: Option<&str>, controller_id: &str) -> WireCommand {
    WireCommand::MountStrategy {
        venue: venue.to_string(),
        account: account.map(str::to_string),
        symbol: symbol.to_string(),
        interval: "1m".to_string(),
        controller_id: Some(controller_id.to_string()),
        name: Some("buy_hold".to_string()),
        rhai: None,
        params: serde_json::json!({}),
    }
}

fn hello_welcome(stream: &mut TcpStream) -> ([u8; 32], Vec<String>) {
    write_frame(stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    match read_frame::<_, Response>(stream).expect("welcome") {
        Response::Welcome { nonce, features, .. } => (nonce, features),
        other => panic!("expected Welcome, got {other:?}"),
    }
}

/// Complete the handshake as `Control` and return the authed stream (NOT subscribed — control stays
/// in the request/response loop) beside the node's advertised features.
fn control_stream(addr: SocketAddr) -> (TcpStream, Vec<String>) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    let (nonce, features) = hello_welcome(&mut stream);
    let mac = auth::sign(CONTROL_KEY, &nonce, NODE_PROTO_VERSION, Scope::Write);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Write, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: Scope::Write } => {}
        other => panic!("expected AuthOk(Control), got {other:?}"),
    }
    (stream, features)
}

fn command(cmd: WireCommand) -> Request {
    Request::Command { cmd, reason: None }
}

/// Send one already-serialized request BODY (raw JSON) and read the reply — the seam the
/// old-client tests need, because what an older client writes is BYTES, and a `WireCommand` built
/// in Rust can only ever carry the fields today's type has.
fn send_raw(stream: &mut TcpStream, body: serde_json::Value) -> Response {
    write_frame(stream, &body).expect("raw request");
    read_frame::<_, Response>(stream).expect("reply")
}

#[cfg(test)]
#[path = "venue_routing/account_gate.rs"]
mod account_gate;
#[cfg(test)]
#[path = "venue_routing/book_gate.rs"]
mod book_gate;
#[cfg(test)]
#[path = "venue_routing/bracket.rs"]
mod bracket;
#[cfg(test)]
#[path = "venue_routing/old_clients_and_capability.rs"]
mod old_clients_and_capability;
