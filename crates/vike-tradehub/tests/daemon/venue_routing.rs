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
//! - **An UNADDRESSED command still reaches today's engine set**, driven from wire BYTES with no
//!   venue in them rather than from a struct carrying a `None`, so the compatibility claim is
//!   about what an older client actually writes.
//! - **A submit with the venue KEY ABSENT never decoded in the first place** — the honest form of
//!   "an old client sends an unaddressed order", which on this wire has never existed.
//! - **The capability is advertised**, so a client can tell this node from one that misroutes
//!   silently (`FEATURE_VENUE_ROUTING`; the client half is
//!   `vike_app_core::tradehub_control::venue_routing_verdict`).
//! - **The dry-run answers the same verdict**, because a preview that stays quiet about the order
//!   landing on a different book than it names is worse than no preview.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use vike_exec::BarUpdate;
use vike_model::Bar;
use vike_run::{
    MultiStrategyMount, PaperHalt, PaperMountOpts, StrategyMountSpec,
    build_paper_multi_strategy_core_with,
};
use vike_tradehub::config::DaemonProfile;
use vike_tradehub::publish::{self, PublisherHandle};
use vike_tradehub::server;
use vike_tradehub_client::auth;
use vike_tradehub_client::proto::{
    FEATURE_VENUE_ROUTING, NODE_PROTO_VERSION, Request, Response, Scope, read_frame, write_frame,
};
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};
use vike_tradehub_client::{NodeKeys, RemoteCoreHandle};

const OBSERVE_KEY: &[u8] = b"observe-secret-key-for-venue-routing-tests";
const CONTROL_KEY: &[u8] = b"control-secret-key-for-venue-routing-tests";

/// The two venues this suite's daemon mounts. `polymarket` is declared FIRST, so it is the PRIMARY
/// engine — which is what makes the binance assertions load-bearing: an order that failed to route
/// would land on polymarket, not on the venue it named.
const PRIMARY_VENUE: &str = "polymarket";
const PRIMARY_SYMBOL: &str = "VR_ROUTE_TOK";
const SECOND_VENUE: &str = "binance";
const SECOND_SYMBOL: &str = "VR_ROUTE_BTC";
/// A venue id no engine in this daemon is built for. It IS a real roster venue
/// (`vike_model::VENUES`), deliberately: a refusal that only triggered on nonsense strings would
/// miss the case that actually happens, which is an operator naming a venue the platform supports
/// and THIS node does not run.
const UNMOUNTED_VENUE: &str = "okx";

/// A HALT sentinel path this file OWNS and never creates — a paper mount is HALT-armed by design,
/// so a test expecting orders to book must not inherit the operator's kill switch off whatever box
/// runs it. The owning `TempDir` is returned alongside and MUST be bound for as long as the mount
/// lives (the paper client consults the path on every opening order).
fn own_sentinel(name: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .prefix(&format!("vike-tradehub-venue-routing-{name}-"))
        .tempdir()
        .expect("temp sentinel root");
    let path = root.path().join("HALT");
    (root, path)
}

fn opts_pinned_to(sentinel: &Path) -> PaperMountOpts {
    assert!(
        !sentinel.exists(),
        "the pinned sentinel must NOT exist, or the mount refuses opening orders: {}",
        sentinel.display()
    );
    PaperMountOpts { halt: PaperHalt::Pinned(sentinel.to_path_buf()), ..Default::default() }
}

fn bar(ts: i64, px: f64) -> Bar {
    Bar {
        ts,
        open: px,
        high: px,
        low: px,
        close: px,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Resolve a validated profile's rows the way `main.rs`'s multi paper arm does.
fn resolve_mounts(profile: &DaemonProfile) -> Vec<StrategyMountSpec> {
    profile
        .mount_rows()
        .into_iter()
        .map(|row| {
            let cfg = row.to_mount_config();
            let mut spec = row.to_mount_spec();
            spec.controller_id = Some(row.derived_controller_id());
            let strategy = row.resolve_strategy(&cfg).expect("a validated row resolves");
            StrategyMountSpec { strategy, spec }
        })
        .collect()
}

/// The TWO-VENUE profile every test here mounts: one `buy_hold` row per venue, polymarket first.
fn two_venue_profile() -> DaemonProfile {
    DaemonProfile::from_toml_str(
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
symbol = "VR_ROUTE_BTC"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 1.0
"#,
    )
    .expect("a two-venue [[mounts]] profile parses and validates")
}

/// Build the two-engine paper node, publish it, and serve it on an ephemeral loopback port with a
/// CONTROL key and the core's `CommandSink` threaded in.
///
/// ⚠ It WAITS for the core to publish its engine set before returning. That is not tidiness: the
/// routing gate reads the roster off the published snapshot, and an EMPTY roster is treated as
/// UNKNOWN and refuses nothing (`server::venue_refusal` argues why refusing on it would deadlock a
/// feed-less daemon for ever). A test that raced that window would pass or fail on timing rather
/// than on the property, which is the one thing a routing test must not do.
fn spawn_two_venue_node(
    tag: &str,
) -> (MultiStrategyMount, tempfile::TempDir, PublisherHandle, SocketAddr) {
    let profile = two_venue_profile();
    let (halt_dir, sentinel) = own_sentinel(tag);
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));
    // One bar per mount so the core folds, goes dirty and PUBLISHES — the only way its engine set
    // becomes visible to the server (a paper daemon with no feed publishes nothing until something
    // happens to it).
    let bars = mount.handle.bar_sender();
    for (venue, symbol) in [(PRIMARY_VENUE, PRIMARY_SYMBOL), (SECOND_VENUE, SECOND_SYMBOL)] {
        bars.close(BarUpdate {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: "1m".to_string(),
            bar: bar(60_000, 0.50),
        })
        .expect("the core is alive");
    }

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    assert!(
        wait_until(10, || {
            let v = publisher.engine_venues();
            v.iter().any(|x| x == PRIMARY_VENUE) && v.iter().any(|x| x == SECOND_VENUE)
        }),
        "the core must publish BOTH engines before the routing gate can check an address; got {:?}",
        publisher.engine_venues()
    );
    let commands = Some(mount.handle.command_sink());
    let server_publisher = publisher.clone();
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            server_publisher,
            NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()),
            commands,
            server::ControlLimitsConfig::default(),
            None,
            // No `AccountAdminSource`: the account capability is an ABSENCE on every box that
            // has not DECLARED a barrier, which is every fixture here and every shipped box today.
            None,
            None,
        );
    });
    (mount, halt_dir, publisher, addr)
}

/// A resting limit far from any driven price — it stays WORKING, so the snapshot deterministically
/// carries it with the engine that owns it.
fn resting_submit(coid: &str, venue: &str, symbol: &str) -> WireCommand {
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
        account: None,
    })
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
    let mac = auth::sign(CONTROL_KEY, &nonce, NODE_PROTO_VERSION, Scope::Control);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Control, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: Scope::Control } => {}
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

// ---------------------------------------------------------------------------------------------
// The assertion that would have caught this
// ---------------------------------------------------------------------------------------------

/// **Two addresses, two books.** Each submit names its own venue and appears in the snapshot owned
/// by THAT venue's engine — `OrderView::venue` is the OWNING ENGINE's venue (`CoreSnapshot::build`
/// zips each registry with the engine it came from), so this discriminates the engine that handled
/// the order rather than echoing the string the request carried.
///
/// The binance half is the load-bearing one: polymarket is the PRIMARY, so an order that failed to
/// route would land there, and this assertion fails naming the wrong owner.
#[test]
fn each_addressed_submit_reaches_the_engine_it_names() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("reaches");
    let (mut ctl, _features) = control_stream(addr);

    for (coid, venue, symbol) in
        [("vr-primary", PRIMARY_VENUE, PRIMARY_SYMBOL), ("vr-second", SECOND_VENUE, SECOND_SYMBOL)]
    {
        write_frame(&mut ctl, &command(resting_submit(coid, venue, symbol))).expect("command");
        match read_frame::<_, Response>(&mut ctl).expect("ack") {
            Response::Ack { coid: echoed } => assert_eq!(echoed, coid),
            other => panic!("expected Ack{{{coid}}}, got {other:?}"),
        }
    }

    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    assert!(
        wait_until(10, || {
            let s = observer.snapshot();
            ["vr-primary", "vr-second"]
                .iter()
                .all(|c| s.orders.iter().any(|o| &o.client_order_id == c))
        }),
        "both control-submitted orders must reach the pushed snapshot"
    );
    let snap = observer.snapshot();
    let owner = |coid: &str| {
        snap.orders
            .iter()
            .find(|o| o.client_order_id == coid)
            .unwrap_or_else(|| panic!("order {coid} present"))
            .venue
            .clone()
    };
    assert_eq!(
        owner("vr-primary"),
        PRIMARY_VENUE,
        "the order naming the primary venue is owned by the primary engine"
    );
    assert_eq!(
        owner("vr-second"),
        SECOND_VENUE,
        "the order naming the SECOND venue must be owned by the SECOND engine — owned by \
         `{PRIMARY_VENUE}` here would be exactly the misroute this suite exists for"
    );

    mount.handle.shutdown_and_join();
}

// ---------------------------------------------------------------------------------------------
// The refusal
// ---------------------------------------------------------------------------------------------

/// **An address this node cannot match is REFUSED, and the refusal names what it does have.**
/// Nothing is booked — on either engine — so the operator's retry is against a node whose state is
/// exactly what it was.
#[test]
fn an_unmatched_address_is_refused_naming_the_venues_this_node_runs() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("unmatched");
    let (mut ctl, _features) = control_stream(addr);

    write_frame(&mut ctl, &command(resting_submit("vr-nowhere", UNMOUNTED_VENUE, "ANY")))
        .expect("command");
    let msg = match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Error(msg) => msg,
        other => panic!("expected Error for an unmatched venue, got {other:?}"),
    };
    assert!(msg.contains(UNMOUNTED_VENUE), "the refusal names the venue that was asked for: {msg}");
    assert!(msg.contains(PRIMARY_VENUE), "...and the venues this node DOES run: {msg}");
    assert!(msg.contains(SECOND_VENUE), "...both of them: {msg}");

    // The connection SURVIVES a refusal (it is not fatal), so the same peer can immediately send a
    // correctly-addressed command — which is what makes "refusing costs one retry" true rather
    // than merely claimed.
    write_frame(&mut ctl, &command(resting_submit("vr-retry", SECOND_VENUE, SECOND_SYMBOL)))
        .expect("retry");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, "vr-retry"),
        other => panic!("expected Ack after the refusal, got {other:?}"),
    }

    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    assert!(
        wait_until(10, || observer
            .snapshot()
            .orders
            .iter()
            .any(|o| o.client_order_id == "vr-retry")),
        "the retry must book (so the absence below is a real absence, not an un-drained snapshot)"
    );
    assert!(
        !observer.snapshot().orders.iter().any(|o| o.client_order_id == "vr-nowhere"),
        "the refused order must exist on NO engine — refusing means refusing, not re-addressing: \
         {:?}",
        observer
            .snapshot()
            .orders
            .iter()
            .map(|o| (&o.client_order_id, &o.venue))
            .collect::<Vec<_>>()
    );

    mount.handle.shutdown_and_join();
}

/// The risk-REDUCING venue verbs are refused on an unmatched address too, and the UNSCOPED panic
/// button never is.
///
/// ⚠ The judgment here is deliberate and goes the same way as the submit: a `market-exit okx`
/// against a node with no okx engine used to reach `exit_scope_engines`' empty-set fallback and act
/// on engine 0 — it would CANCEL the resting orders of a book the operator was not talking about.
/// Refusing costs a retry; acting cancels the wrong quotes. The unscoped form (`venue: None`) names
/// the whole set and is the one shape that must never need an argument, so the gate cannot see it
/// at all (`WireCommand::addressed_venue` answers `None`).
#[test]
fn a_scoped_exit_is_refused_on_an_unmatched_venue_while_the_unscoped_panic_button_is_not() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("exit");
    let (mut ctl, _features) = control_stream(addr);

    write_frame(
        &mut ctl,
        &command(WireCommand::MarketExit {
            venue: Some(UNMOUNTED_VENUE.to_string()),
            account: None,
        }),
    )
    .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Error(msg) => {
            assert!(msg.contains(UNMOUNTED_VENUE), "the scoped exit refusal names it: {msg}")
        }
        other => panic!("expected Error for a scoped exit on an unmatched venue, got {other:?}"),
    }

    write_frame(&mut ctl, &command(WireCommand::MarketExit { venue: None, account: None }))
        .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        // The account-wide convention: an empty coid, and an Ack rather than a refusal.
        Response::Ack { coid } => assert!(coid.is_empty(), "account-wide verbs echo no coid"),
        other => panic!(
            "the UNSCOPED panic button must never be refused by the routing gate, got {other:?}"
        ),
    }

    mount.handle.shutdown_and_join();
}

// ---------------------------------------------------------------------------------------------
// Old clients
// ---------------------------------------------------------------------------------------------

/// **An UNADDRESSED command still reaches today's engine set** — driven from wire BYTES carrying no
/// venue, not from a Rust struct holding a `None`.
///
/// `MassCancel` is the shape that carries the claim: its `venue` is the wire's own "no venue" and
/// the JSON below is what a client that names none actually writes. It fans out across every
/// engine exactly as it did before this gate existed, because `addressed_venue` answers `None` and
/// the gate never sees it.
#[test]
fn an_unaddressed_command_from_the_wire_is_accepted_unchanged() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("unaddressed");
    let (mut ctl, _features) = control_stream(addr);

    let reply = send_raw(
        &mut ctl,
        serde_json::json!({
            "Command": { "cmd": { "MassCancel": { "venue": null, "symbol": null } }, "reason": null }
        }),
    );
    match reply {
        Response::Ack { coid } => assert!(coid.is_empty(), "account-wide verbs echo no coid"),
        other => panic!("an unaddressed MassCancel must still be accepted, got {other:?}"),
    }

    // ...and the same for the account-wide kill switch, whose variant has no venue FIELD at all.
    let reply = send_raw(
        &mut ctl,
        serde_json::json!({
            "Command": { "cmd": { "SetTradingState": "Halted" }, "reason": null }
        }),
    );
    match reply {
        Response::Ack { coid } => assert!(coid.is_empty()),
        other => panic!("SetTradingState names no venue and must be accepted, got {other:?}"),
    }

    mount.handle.shutdown_and_join();
}

/// **There has never BEEN an unaddressed submit on this wire**, and this is where that is written
/// down rather than assumed: a `Submit` body with the `venue` key absent does not decode, because
/// `WireOrderRequest::venue` is a plain `String` with no `#[serde(default)]`.
///
/// It matters for the compatibility argument. "An old client sends no venue" is the usual shape of
/// a routing fix's back-compat worry, and here it is IMPOSSIBLE — every client that has ever
/// submitted an order named a venue, which is why the fix is a check on a field rather than a new
/// field, and why no `NODE_PROTO_VERSION` bump is involved.
#[test]
fn a_submit_with_no_venue_key_has_never_decoded() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("novenue");
    let (mut ctl, _features) = control_stream(addr);

    let reply = send_raw(
        &mut ctl,
        serde_json::json!({
            "Command": {
                "cmd": { "Submit": {
                    "client_order_id": "vr-keyless",
                    "symbol": PRIMARY_SYMBOL,
                    "side": 1,
                    "qty": 1.0,
                    "order_type": "limit",
                    "price": 0.10
                }},
                "reason": null
            }
        }),
    );
    match reply {
        Response::Error(msg) => assert!(
            msg.contains("undecodable") || msg.contains("venue"),
            "the frame is refused at DECODE, before any gate: {msg}"
        ),
        other => panic!("a venue-less Submit body must not decode, got {other:?}"),
    }

    mount.handle.shutdown_and_join();
}

// ---------------------------------------------------------------------------------------------
// The capability, and the dry-run
// ---------------------------------------------------------------------------------------------

/// The node ADVERTISES that it checks the address. Without this string a client cannot tell this
/// node from one that accepts every venue and applies it to its primary book — both answer `Ack`,
/// and the difference shows up only on a venue's statement.
#[test]
fn the_node_advertises_that_it_routes_by_the_addressed_venue() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("feature");
    let (_ctl, features) = control_stream(addr);
    assert!(
        features.iter().any(|f| f == FEATURE_VENUE_ROUTING),
        "Welcome.features must carry `{FEATURE_VENUE_ROUTING}`: {features:?}"
    );
    mount.handle.shutdown_and_join();
}

/// The DRY-RUN answers the same routing verdict the real command would, and still executes
/// nothing. A preview whose whole job is "what would this do" but that is silent about the order
/// landing on a book it does not name is worse than no preview at all.
#[test]
fn the_dry_run_reports_the_same_routing_refusal() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("preview");
    let (mut ctl, _features) = control_stream(addr);

    write_frame(&mut ctl, &Request::Preview(resting_submit("vr-dry", UNMOUNTED_VENUE, "ANY")))
        .expect("preview");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Preview { accepted, reason } => {
            assert!(!accepted, "an unmatched venue previews as NOT accepted");
            let reason = reason.expect("a refusal carries its reason");
            assert!(reason.contains(UNMOUNTED_VENUE), "{reason}");
            assert!(reason.contains(SECOND_VENUE), "{reason}");
        }
        other => panic!("expected Preview, got {other:?}"),
    }

    // ...and a correctly-addressed one previews as accepted.
    write_frame(
        &mut ctl,
        &Request::Preview(resting_submit("vr-dry-ok", SECOND_VENUE, SECOND_SYMBOL)),
    )
    .expect("preview");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Preview { accepted, reason } => {
            assert!(accepted, "a matched venue previews as accepted; reason: {reason:?}")
        }
        other => panic!("expected Preview, got {other:?}"),
    }

    mount.handle.shutdown_and_join();
}
