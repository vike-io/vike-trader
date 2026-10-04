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

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use vike_exec::BarUpdate;
use vike_model::Bar;
use vike_mount::{
    MultiStrategyMount, PaperHalt, PaperMountOpts, StrategyMountSpec,
    build_paper_multi_strategy_core_with,
};
use vike_tradehub::config::DaemonProfile;
use vike_tradehub::publish::{self, PublisherHandle};
use vike_tradehub::server;
use vike_tradehub_client::auth;
use vike_tradehub_client::proto::{
    FEATURE_ACCOUNT_SCOPED_REDUCE, FEATURE_BRACKET, FEATURE_VENUE_ROUTING, NODE_PROTO_VERSION,
    Request, Response, Scope, read_frame, write_frame,
};
use vike_tradehub_client::wire::{WireBracketSpec, WireCommand, WireOrderRequest};
use vike_tradehub_client::{NodeKeys, RemoteCoreHandle};

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
            bar: bar(60_000, 0.50),
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
    let (halt_dir, sentinel) = own_sentinel(tag);
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(profile), opts_pinned_to(&sentinel));

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let commands = Some(mount.handle.command_sink());
    let server_publisher = publisher.clone();
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            server_publisher,
            NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()),
            commands,
            server::control::ControlLimitsConfig::default(),
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
// WHICH BOOK OF IT — the ACCOUNT gate, on the same path
// ---------------------------------------------------------------------------------------------

/// ⚠ **THE WIRING, which is the half a unit test cannot reach.** `server::refusal::account_refusal` has its
/// own pure-verdict suite (`server.rs`'s `account_refusal_tests`) and a mutation proof that the
/// FUNCTION works. Neither says the gate is ON THE PATH: a refactor that dropped
/// `accept_command`'s step 1c, or a surface that passed an EMPTY route-key roster, would leave
/// every one of those tests green and restore the defect in full. This test is that claim, over a
/// real handshake, a real `accept_command` and a real published roster.
///
/// # What the fixture gives, and why a two-ACCOUNT core is not needed to prove it
///
/// This daemon runs ONE account of each venue, so its published route keys are the bare venue ids
/// (`route_key_of` renders the default account that way). A submit naming `ALT` on `binance`
/// therefore composes `binance#ALT`, which no engine here carries — and that is precisely the
/// spec's opening symptom, `binance/NOSUCH` on a node that does not run `NOSUCH`. It used to be
/// Acked and refused out of band by the core, so the client printed `accepted` over an order that
/// never existed.
///
/// ⚠ **The VENUE gate must not be what answers**, and the negative assertion is what makes this
/// test discriminate rather than merely pass: `binance` IS a venue this node runs, so
/// `venue_refusal` is silent on this frame and its sentence must be absent from the reply. A gate
/// that refused here in the venue's words would be the widening that was deliberately not done.
#[test]
fn an_unheld_account_is_refused_over_the_wire_naming_the_accounts_this_node_holds() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("account");
    let (mut ctl, _features) = control_stream(addr);

    write_frame(
        &mut ctl,
        &command(resting_submit_for("vr-alt", SECOND_VENUE, SECOND_SYMBOL, Some("ALT"))),
    )
    .expect("command");
    let msg = match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Error(msg) => msg,
        other => panic!(
            "a submit naming an account this node does not hold must be REFUSED BEFORE THE ACK — \
             an `Ack` here is the whole defect, got {other:?}"
        ),
    };
    assert!(msg.contains("no account `ALT`"), "the refusal names the account asked for: {msg}");
    assert!(msg.contains(SECOND_VENUE), "...and the venue it was asked for on: {msg}");
    assert!(
        msg.contains("DEFAULT"),
        "...and the account this node DOES hold, under the one spelling a client can send back: \
         {msg}"
    );
    assert!(
        !msg.contains("runs no engine for venue"),
        "⚠ this must NOT be the VENUE gate's sentence — `{SECOND_VENUE}` is a venue this node \
         runs, and answering in those words would be the widening that was deliberately refused: \
         {msg}"
    );

    // ⚠ `DEFAULT` OVER THE WIRE IS ACCEPTED — the ruling, end to end. `route_key_of` renders the
    // unlabelled account as the BARE VENUE ID, so a gate that suffixed the label text would look
    // for `binance#DEFAULT`, match nothing, and refuse the one spelling that addresses the book
    // this node actually has. `vike_core`'s
    // `a_payload_naming_default_reaches_the_unlabelled_account_of_a_two_account_venue` is the same
    // row one plane down.
    write_frame(
        &mut ctl,
        &command(resting_submit_for("vr-default", SECOND_VENUE, SECOND_SYMBOL, Some("DEFAULT"))),
    )
    .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Ack { coid } => assert_eq!(coid, "vr-default"),
        other => panic!("`DEFAULT` NAMES the account this node holds and must be Acked: {other:?}"),
    }

    // ...and a submit naming NO account is byte-identical to before this gate existed. Its case is
    // `vike_core`'s `ambiguous_accounts`, not this gate's, and on a single-account venue there is
    // nothing ambiguous about it.
    write_frame(&mut ctl, &command(resting_submit("vr-bare", SECOND_VENUE, SECOND_SYMBOL)))
        .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Ack { coid } => assert_eq!(coid, "vr-bare"),
        other => panic!("an account-less submit must be unchanged, got {other:?}"),
    }

    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    assert!(
        wait_until(10, || {
            let s = observer.snapshot();
            ["vr-default", "vr-bare"]
                .iter()
                .all(|c| s.orders.iter().any(|o| &o.client_order_id == c))
        }),
        "the two ACCEPTED orders must book, so the absence below is a real absence rather than an \
         un-drained snapshot"
    );
    assert!(
        !observer.snapshot().orders.iter().any(|o| o.client_order_id == "vr-alt"),
        "the refused order must exist on NO engine — not on the venue it named, not on the \
         primary: {:?}",
        observer
            .snapshot()
            .orders
            .iter()
            .map(|o| (&o.client_order_id, &o.venue))
            .collect::<Vec<_>>()
    );

    mount.handle.shutdown_and_join();
}

/// The DRY-RUN answers the same ACCOUNT verdict, for the venue twin's reason one field along: a
/// preview that is silent about the named BOOK not existing hands the operator a rehearsal whose
/// real send is a refusal. It is a SEPARATE arm in `handle_connection` from the command one, so it
/// is a separate claim — sharing `accept_command` would have made this redundant, and it does not.
#[test]
fn the_dry_run_reports_the_same_account_refusal() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("acct-preview");
    let (mut ctl, _features) = control_stream(addr);

    write_frame(
        &mut ctl,
        &Request::Preview(resting_submit_for(
            "vr-acct-dry",
            SECOND_VENUE,
            SECOND_SYMBOL,
            Some("ALT"),
        )),
    )
    .expect("preview");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Preview { accepted, reason } => {
            assert!(!accepted, "an unheld account previews as NOT accepted");
            let reason = reason.expect("a refusal carries its reason");
            assert!(reason.contains("no account `ALT`"), "{reason}");
            assert!(reason.contains("DEFAULT"), "…naming what this node holds: {reason}");
        }
        other => panic!("expected Preview, got {other:?}"),
    }

    // ...and the held account previews as accepted, so the arm is not simply refusing everything.
    write_frame(
        &mut ctl,
        &Request::Preview(resting_submit_for(
            "vr-acct-dry-ok",
            SECOND_VENUE,
            SECOND_SYMBOL,
            Some("DEFAULT"),
        )),
    )
    .expect("preview");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Preview { accepted, reason } => {
            assert!(accepted, "`DEFAULT` names a held book; reason: {reason:?}")
        }
        other => panic!("expected Preview, got {other:?}"),
    }

    mount.handle.shutdown_and_join();
}

/// **THE MOUNT PLANE, over the wire** — the same gate, one rung up the stakes.
///
/// `WireCommand::MountStrategy` names an account too, and until this landed nothing at the edge
/// looked at it: the frame was Acked, `lower_command` validated the SPEC (which says nothing about
/// accounts), and `vike_core`'s `CoreThread::mount_strategy_runtime` refused it on the far side of
/// the single-writer lane as a recent-events note. That is the order plane's defect verbatim,
/// except that what a client hears `accepted` about is not one order but every order the strategy
/// would ever place — `WireCommand::MountStrategy`'s `account` doc is where that argument lives.
///
/// ⚠ **What makes this discriminate rather than merely pass**: in the un-gated state this frame is
/// NOT an error — `buy_hold` with default params is a spec `mount_factory::validate_spec` accepts,
/// so the reply is a plain `Ack` and the refusal arrives nowhere a client is listening. So the
/// failure mode this test is written against is an ACK, not a differently-worded error, and the
/// assertion below names the account to keep a spec refusal from being read as this gate's.
#[test]
fn an_unheld_account_on_a_mount_is_refused_over_the_wire() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("mount-account");
    let (mut ctl, _features) = control_stream(addr);

    write_frame(
        &mut ctl,
        &command(mount_on(SECOND_VENUE, SECOND_SYMBOL, Some("ALT"), "vr-mount-alt")),
    )
    .expect("command");
    let msg = match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Error(msg) => msg,
        other => panic!(
            "a MOUNT naming an account this node does not hold must be REFUSED BEFORE THE ACK — \
             an `Ack` here is the whole defect, and it is the one a strategy's entire order flow \
             rides on, got {other:?}"
        ),
    };
    assert!(msg.contains("no account `ALT`"), "the refusal names the account asked for: {msg}");
    assert!(msg.contains(SECOND_VENUE), "...and the venue it was asked for on: {msg}");
    assert!(
        msg.contains("DEFAULT"),
        "...and the account this node DOES hold, under the one spelling a client can send back: \
         {msg}"
    );
    assert!(
        !msg.contains("runs no engine for venue"),
        "⚠ this must NOT be the VENUE gate's sentence — `{SECOND_VENUE}` is a venue this node \
         runs: {msg}"
    );

    // ...and a mount naming the account this node DOES hold is ACCEPTED at the edge, so the arm is
    // not simply refusing every mount. The account-wide-verb convention is an empty echoed coid.
    write_frame(
        &mut ctl,
        &command(mount_on(SECOND_VENUE, SECOND_SYMBOL, Some("DEFAULT"), "vr-mount-default")),
    )
    .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Ack { coid } => assert!(coid.is_empty(), "mount verbs echo no coid"),
        other => panic!("`DEFAULT` NAMES the account this node holds and must be Acked: {other:?}"),
    }

    mount.handle.shutdown_and_join();
}

/// **THE REDUCING PLANE, over the wire** — owner ruling "B", 2026-09-26: a mass-cancel, a flatten
/// or a market exit naming an account this node does not hold is REFUSED BEFORE THE ACK.
///
/// Until this, all three were Acked whatever account they named: `lower_command` dropped the field
/// and the core fanned the verb over every account of the venue, so on a two-account box
/// `market-exit binance ALT` cancelled and flattened the DEFAULT account too. The core narrows now,
/// and a named account it does not hold is exactly the out-of-band refusal this edge exists to move
/// in front of the Ack — so the failure this is written against is an `Ack`, as on the mount plane.
///
/// This node runs ONE account of `binance`, so `ALT` is unheld and `DEFAULT` is the account it
/// holds; the roster in the refusal must say so under the one spelling a client can send back.
#[test]
fn an_unheld_account_on_a_reducing_verb_is_refused_over_the_wire() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("reduce-account");
    let (mut ctl, features) = control_stream(addr);
    assert!(
        features.iter().any(|f| f == FEATURE_ACCOUNT_SCOPED_REDUCE),
        "the node honours a labelled reduce, so it must advertise it: {features:?}"
    );

    let reducers = |account: &str| {
        let account = Some(account.to_string());
        vec![
            (
                "The mass-cancel",
                WireCommand::MassCancel {
                    venue: Some(SECOND_VENUE.into()),
                    symbol: None,
                    account: account.clone(),
                },
            ),
            (
                "The flatten",
                WireCommand::Flatten {
                    venue: SECOND_VENUE.into(),
                    symbol: SECOND_SYMBOL.into(),
                    account: account.clone(),
                },
            ),
            (
                "The market exit",
                WireCommand::MarketExit { venue: Some(SECOND_VENUE.into()), account },
            ),
        ]
    };

    for (subject, cmd) in reducers("ALT") {
        write_frame(&mut ctl, &command(cmd)).expect("command");
        let msg = match read_frame::<_, Response>(&mut ctl).expect("reply") {
            Response::Error(msg) => msg,
            other => panic!(
                "{subject} naming an account this node does not hold must be REFUSED BEFORE THE \
                 ACK — an `Ack` here is the whole defect, got {other:?}"
            ),
        };
        assert!(msg.contains("no account `ALT`"), "{subject}: names the account: {msg}");
        assert!(msg.contains(SECOND_VENUE), "{subject}: …and the venue: {msg}");
        assert!(msg.contains("DEFAULT"), "{subject}: …and the account this node holds: {msg}");
        assert!(msg.contains(&format!("{subject} was REFUSED")), "{subject}: its subject: {msg}");
        assert!(
            !msg.contains("runs no engine for venue"),
            "{subject}: ⚠ NOT the venue gate's sentence — `{SECOND_VENUE}` is run here: {msg}"
        );
    }

    // …and the account this node DOES hold is accepted on all three — the arm is not simply
    // refusing every labelled reduce. The account-wide convention: an empty echoed coid.
    for (subject, cmd) in reducers("DEFAULT") {
        write_frame(&mut ctl, &command(cmd)).expect("command");
        match read_frame::<_, Response>(&mut ctl).expect("reply") {
            Response::Ack { coid } => assert!(coid.is_empty(), "{subject}: echoes no coid"),
            other => panic!("{subject} naming `DEFAULT` names a held book: {other:?}"),
        }
    }

    // ⚠ An account with NO venue is refused, never widened into the global exit it would
    // otherwise reach.
    write_frame(
        &mut ctl,
        &command(WireCommand::MarketExit { venue: None, account: Some("ALT".into()) }),
    )
    .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Error(msg) => {
            assert!(msg.contains("`ALT`") && msg.contains("no venue"), "names both: {msg}")
        }
        other => panic!("an account with no venue must be refused, got {other:?}"),
    }

    // ⚠ …and the UNSCOPED panic button, on the same connection, is still Acked.
    write_frame(&mut ctl, &command(WireCommand::MarketExit { venue: None, account: None }))
        .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Ack { coid } => assert!(coid.is_empty(), "account-wide verbs echo no coid"),
        other => panic!("the UNSCOPED panic button must never be refused, got {other:?}"),
    }

    mount.handle.shutdown_and_join();
}

/// The DRY-RUN answers the same reducing-plane verdicts, because `handle_connection`'s preview arm
/// runs the gates itself rather than through `accept_command`: an unheld account and an account
/// with no venue both preview as NOT accepted, and the held account previews as accepted.
#[test]
fn the_dry_run_reports_the_same_reducing_verb_refusals() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("reduce-preview");
    let (mut ctl, _features) = control_stream(addr);

    for (cmd, want_accepted, needle) in [
        (
            WireCommand::MarketExit {
                venue: Some(SECOND_VENUE.into()),
                account: Some("ALT".into()),
            },
            false,
            "no account `ALT`",
        ),
        (WireCommand::MarketExit { venue: None, account: Some("ALT".into()) }, false, "no venue"),
        (
            WireCommand::MarketExit {
                venue: Some(SECOND_VENUE.into()),
                account: Some("DEFAULT".into()),
            },
            true,
            "",
        ),
    ] {
        let label = format!("{cmd:?}");
        write_frame(&mut ctl, &Request::Preview(cmd)).expect("preview");
        match read_frame::<_, Response>(&mut ctl).expect("reply") {
            Response::Preview { accepted, reason } => {
                assert_eq!(accepted, want_accepted, "{label}: reason {reason:?}");
                if !want_accepted {
                    let reason = reason.expect("a refusal carries its reason");
                    assert!(reason.contains(needle), "{label}: {reason}");
                }
            }
            other => panic!("expected Preview, got {other:?}"),
        }
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

// ---------------------------------------------------------------------------------------------
// The TP/SL BRACKET — default account only, over a real core
// ---------------------------------------------------------------------------------------------
//
// ⚠ The TWO-ACCOUNT refusal has no test in this file, and cannot cheaply have one: the paper
// fixture builds ONE engine per distinct venue (`vike_mount`'s `build_paper_multi_strategy_core_with`
// does not split a venue by account), so this node never publishes two route keys of one venue.
// That verdict is `server::refusal::account_refusal`'s, pinned by its unit suite
// (`a_bracket_goes_only_to_a_venue_whose_one_account_is_the_default` and its siblings), and the
// gate being ON THE PATH is what the submit/mount/reduce account tests above prove over the wire.
// The refusals that ARE reachable here — the empty roster, the values, a symbol the engine does
// not trade, and an engine mounted on binance's spot lane — are proven below as a client sees
// them: `Response::Error`, never an `Ack`, and the same sentence on a Preview.

/// A bracket on `venue`/`symbol`. The limit entry at 0.10 sits far under the 0.50 bar the harness
/// drives, so it never fills, and both exits stay HELD.
fn bracket_on(venue: &str, symbol: &str) -> WireCommand {
    WireCommand::Bracket(WireBracketSpec {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty: 20.0,
        entry_price: Some(0.10),
        stop_loss: 0.05,
        take_profit: 0.90,
    })
}

/// **A bracket books its entry on the engine it names and holds both exits off the venue.** The
/// `Ack` echoes no coid (the runtime mints all three). The operator's handle is the ENTRY's id off
/// the snapshot, and cancelling it drops both held exits.
///
/// The second venue is the load-bearing one: the primary is polymarket, so a bracket that failed to
/// route would land there.
#[test]
fn a_bracket_books_its_entry_and_holds_both_exits_on_the_engine_it_names() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("bracket-held");
    let (mut ctl, features) = control_stream(addr);
    assert!(features.iter().any(|f| f == FEATURE_BRACKET), "advertised: {features:?}");

    write_frame(&mut ctl, &command(bracket_on(SECOND_VENUE, SECOND_SYMBOL))).expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, "", "the runtime mints the ids after the Ack"),
        other => panic!("expected Ack, got {other:?}"),
    }

    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    let held = |s: &vike_tradehub_client::wire::WireSnapshot| {
        s.held_exits.iter().filter(|h| h.symbol == SECOND_SYMBOL).count()
    };
    assert!(
        wait_until(10, || held(&observer.snapshot()) == 2),
        "both exits must be HELD off the venue until the entry fills"
    );
    let snap = observer.snapshot();
    let parent = snap
        .held_exits
        .iter()
        .find(|h| h.symbol == SECOND_SYMBOL)
        .and_then(|h| h.parent_order_id.clone())
        .expect("a held exit names its entry");
    let entry = snap
        .orders
        .iter()
        .find(|o| o.client_order_id == parent)
        .expect("the entry is in the order registry");
    assert_eq!(entry.venue, SECOND_VENUE, "owned by the engine the bracket named, not the primary");
    assert_eq!((entry.side, entry.order_type.as_str()), (1, "limit"));
    assert!(
        snap.held_exits
            .iter()
            .filter(|h| h.symbol == SECOND_SYMBOL)
            .all(|h| h.side == -1 && h.parent_order_id.as_deref() == Some(parent.as_str())),
        "both exits close the entry and name it"
    );

    write_frame(&mut ctl, &command(WireCommand::Cancel(parent.clone()))).expect("cancel");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { .. } => {}
        other => panic!("expected Ack for the entry's cancel, got {other:?}"),
    }
    assert!(
        wait_until(10, || held(&observer.snapshot()) == 0),
        "cancelling the entry drops the exits it held"
    );
    mount.handle.shutdown_and_join();
}

/// **A bracket the venue cannot hold is refused WHOLE, and no exit is stranded.** Polymarket's
/// capability row has no `stop` kind, so the core's preflight refuses all three legs atomically.
/// The node has already sent `Ack` (this is the core's verdict, not the edge's), and the snapshot
/// shows three rejected orders and nothing held.
#[test]
fn a_bracket_its_venue_cannot_hold_is_refused_whole_and_strands_no_exit() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("bracket-refused");
    let (mut ctl, _features) = control_stream(addr);
    write_frame(&mut ctl, &command(bracket_on(PRIMARY_VENUE, PRIMARY_SYMBOL))).expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { .. } => {}
        other => panic!("expected Ack, got {other:?}"),
    }
    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    let rejected = |s: &vike_tradehub_client::wire::WireSnapshot| {
        s.orders
            .iter()
            .filter(|o| o.symbol == PRIMARY_SYMBOL && o.qty > 10.0 && o.status == "Rejected")
            .count()
    };
    assert!(wait_until(10, || rejected(&observer.snapshot()) == 3), "entry, SL and TP all refused");
    let snap = observer.snapshot();
    assert!(!snap.held_exits.iter().any(|h| h.symbol == PRIMARY_SYMBOL), "no exit is left held");
    assert!(
        snap.orders
            .iter()
            .filter(|o| o.symbol == PRIMARY_SYMBOL && o.qty > 10.0)
            .all(|o| o.status == "Rejected"),
        "no leg is working"
    );
    mount.handle.shutdown_and_join();
}

/// The well-formed second-venue bracket with one field changed by `f`.
fn second_venue_bracket_with(f: fn(&mut WireBracketSpec)) -> WireCommand {
    let WireCommand::Bracket(mut b) = bracket_on(SECOND_VENUE, SECOND_SYMBOL) else {
        unreachable!("bracket_on builds a bracket")
    };
    f(&mut b);
    WireCommand::Bracket(b)
}

/// The three brackets the ordinary fixture refuses for their VALUES or their ENGINE, each beside
/// the needle its refusal must carry: a symbol the binance engine does not trade (it is mounted on
/// [`SECOND_SYMBOL`], so the refusal names that symbol — a bracket goes to that one engine, which
/// would place it on its own instrument), a `side` that is neither +1 nor -1, and an INVERTED long
/// (a stop above its entry).
fn unhonourable_brackets() -> Vec<(&'static str, WireCommand, &'static str)> {
    vec![
        (
            "a symbol its engine does not trade",
            bracket_on(SECOND_VENUE, SECOND_SPOT_SYMBOL),
            "trades `VR_ROUTE_BTC.P`, not `VR_ROUTE_BTC`",
        ),
        ("side 0", second_venue_bracket_with(|b| b.side = 0), "`side`"),
        ("an inverted long", second_venue_bracket_with(|b| b.stop_loss = 0.20), "inverted"),
    ]
}

/// ⚠ **A bracket the node cannot honour is REFUSED BEFORE THE ACK, and books nothing.** Each of
/// [`unhonourable_brackets`] is a refusal a client meets as `Response::Error` on a live connection,
/// where it would otherwise have been Acked and failed at RELEASE, with the position already open.
///
/// The well-formed bracket sent last on the same connection proves both halves of "books nothing":
/// the connection survives each refusal, and the snapshot then holds exactly ONE bracket's exits.
#[test]
fn a_bracket_the_node_cannot_honour_is_refused_before_the_ack_and_books_nothing() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("bracket-unhonourable");
    let (mut ctl, _features) = control_stream(addr);

    for (label, cmd, needle) in unhonourable_brackets() {
        write_frame(&mut ctl, &command(cmd)).expect("command");
        match read_frame::<_, Response>(&mut ctl).expect("reply") {
            Response::Error(msg) => assert!(msg.contains(needle), "{label}: {msg}"),
            other => panic!("{label} must be REFUSED BEFORE THE ACK, got {other:?}"),
        }
    }

    write_frame(&mut ctl, &command(bracket_on(SECOND_VENUE, SECOND_SYMBOL))).expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, ""),
        other => panic!("the well-formed bracket must be Acked, got {other:?}"),
    }
    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    let held = |s: &vike_tradehub_client::wire::WireSnapshot| {
        s.held_exits.iter().filter(|h| h.symbol == SECOND_SYMBOL).count()
    };
    assert!(wait_until(10, || held(&observer.snapshot()) == 2), "the accepted bracket's exits");
    let snap = observer.snapshot();
    assert_eq!(held(&snap), 2, "ONE bracket reached the core — none of the three refused ones");
    assert_eq!(
        snap.orders.iter().filter(|o| o.qty > 10.0 && o.side == 1).count(),
        1,
        "one entry booked on any engine, and it is the accepted bracket's: {:?}",
        snap.orders.iter().map(|o| (&o.client_order_id, &o.symbol, o.qty)).collect::<Vec<_>>()
    );
    mount.handle.shutdown_and_join();
}

/// Send `cmd` as a PREVIEW and then as a COMMAND on one control connection, and return the two
/// refusals — the preview's reason and the command's `Response::Error` — panicking if either was
/// accepted. A refused command books nothing, so the pair leaves the node as it found it.
fn preview_then_command_refusals(
    ctl: &mut TcpStream,
    label: &str,
    cmd: WireCommand,
) -> (String, String) {
    write_frame(ctl, &Request::Preview(cmd.clone())).expect("preview");
    let previewed = match read_frame::<_, Response>(ctl).expect("reply") {
        Response::Preview { accepted, reason } => {
            assert!(!accepted, "{label} previews as NOT accepted; reason {reason:?}");
            reason.expect("a refusal carries its reason")
        }
        other => panic!("{label}: expected Preview, got {other:?}"),
    };
    write_frame(ctl, &command(cmd)).expect("command");
    let sent = match read_frame::<_, Response>(ctl).expect("reply") {
        Response::Error(msg) => msg,
        other => panic!("{label} must be REFUSED BEFORE THE ACK, got {other:?}"),
    };
    (previewed, sent)
}

/// ⚠ **The DRY RUN refuses a bracket exactly as the send does — the same SENTENCE, not merely
/// a refusal.** `handle_connection`'s preview arm runs the gates itself rather than through
/// `accept_command`; what keeps the two from drifting is that both call `server`'s
/// `bracket_refusal`, the bracket's one verdict. Without the preview's fourth step each of these
/// previewed `accepted` while its send came back an error. The well-formed bracket still previews
/// as accepted, so the step is not refusing every bracket.
#[test]
fn the_dry_run_reports_the_same_bracket_refusals() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("bracket-preview");
    let (mut ctl, _features) = control_stream(addr);

    for (label, cmd, needle) in unhonourable_brackets() {
        let (previewed, sent) = preview_then_command_refusals(&mut ctl, label, cmd);
        assert!(previewed.contains(needle), "{label}: {previewed}");
        assert_eq!(previewed, sent, "{label}: the Preview and the Command give ONE sentence");
    }

    write_frame(&mut ctl, &Request::Preview(bracket_on(SECOND_VENUE, SECOND_SYMBOL)))
        .expect("preview");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Preview { accepted, reason } => {
            assert!(accepted, "the well-formed bracket previews as accepted; reason: {reason:?}")
        }
        other => panic!("expected Preview, got {other:?}"),
    }

    mount.handle.shutdown_and_join();
}

/// ⚠ **A bracket to a binance engine MOUNTED ON THE SPOT LANE is refused, whatever symbol it
/// names — and above all when it names the `.P` the old refusal told the operator to use.** This is
/// the shipped daemon's shape (its binance engine is spot), driven end to end. The adapter picks
/// its lane once, from the engine's own symbol, and signs every order on it: a `.P` bracket the
/// node admitted would go out on the SPOT lane, its entry would fill, and its stop-loss would be
/// rejected at release, leaving the position protected by its take-profit alone. A paper engine
/// cannot show that last step (it fills stops on either lane), so what this proves is the half
/// that matters on a live box: the node refuses BEFORE THE ACK, the Preview says the same sentence,
/// the sentence names what the engine trades, and nothing is booked.
#[test]
fn a_bracket_to_a_spot_mounted_binance_engine_is_refused_whatever_symbol_it_names() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_spot_binance_node("bracket-spot-engine");
    let (mut ctl, _features) = control_stream(addr);

    // The `.P` FIRST: that is the frame the old check admitted.
    for symbol in [SECOND_SYMBOL, SECOND_SPOT_SYMBOL] {
        let label = format!("a bracket on `{symbol}` to the spot-mounted engine");
        let (previewed, sent) =
            preview_then_command_refusals(&mut ctl, &label, bracket_on(SECOND_VENUE, symbol));
        assert!(sent.contains("spot lane"), "{label}: names the lane: {sent}");
        assert!(sent.contains("trades `VR_ROUTE_BTC`"), "{label}: names what it trades: {sent}");
        assert_eq!(previewed, sent, "{label}: the Preview and the Command give ONE sentence");
    }

    // A plain order on the engine's own symbol, so the absence below is a real absence and not an
    // un-drained snapshot: once it has booked, everything sent before it has been folded.
    write_frame(
        &mut ctl,
        &command(resting_submit("vr-spot-control", SECOND_VENUE, SECOND_SPOT_SYMBOL)),
    )
    .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, "vr-spot-control"),
        other => panic!("the plain control order must be Acked, got {other:?}"),
    }
    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    assert!(
        wait_until(10, || observer
            .snapshot()
            .orders
            .iter()
            .any(|o| o.client_order_id == "vr-spot-control")),
        "the control order must book"
    );
    let snap = observer.snapshot();
    assert!(snap.held_exits.is_empty(), "no exit is held: {:?}", snap.held_exits);
    let large: Vec<_> = snap.orders.iter().filter(|o| o.qty > 10.0).collect();
    assert!(
        large.len() == 1 && large[0].client_order_id == "vr-spot-control",
        "no bracket leg was booked on any engine — only the control order: {:?}",
        large.iter().map(|o| (&o.client_order_id, &o.symbol, o.qty)).collect::<Vec<_>>()
    );
    mount.handle.shutdown_and_join();
}

/// ⚠ **The one verb an EMPTY roster refuses, over the wire.** Before the core's first publish this
/// node cannot tell whether a venue runs exactly one account, so a bracket is refused — while a
/// `Submit` on the same connection is still Acked (every other verb reads the empty roster as
/// UNKNOWN, the deadlock rule). Once the node has published, the same bracket is Acked.
#[test]
fn a_bracket_is_refused_before_the_first_publish_and_accepted_after_it() {
    vike_log::test_init();
    let (mount, _halt_dir, publisher, addr) = spawn_unpublished_two_venue_node("bracket-empty");
    let (mut ctl, _features) = control_stream(addr);
    assert!(
        publisher.engine_route_keys().is_empty(),
        "precondition: no bar was driven, so the core has published nothing yet"
    );

    write_frame(&mut ctl, &command(bracket_on(SECOND_VENUE, SECOND_SYMBOL))).expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Error(msg) => assert!(msg.contains("not published"), "{msg}"),
        other => panic!("a bracket on an unpublished roster must be REFUSED, got {other:?}"),
    }

    write_frame(&mut ctl, &command(resting_submit("vr-empty", SECOND_VENUE, SECOND_SYMBOL)))
        .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, "vr-empty"),
        other => panic!("every other verb reads an empty roster as UNKNOWN, got {other:?}"),
    }

    publish_both_engines(&mount, &publisher);
    write_frame(&mut ctl, &command(bracket_on(SECOND_VENUE, SECOND_SYMBOL))).expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, ""),
        other => panic!("after the first publish the bracket must be Acked, got {other:?}"),
    }
    mount.handle.shutdown_and_join();
}

/// ⚠ **DECLARED RESIDUAL, pinned so it cannot change silently: a bracket frame delivered twice is
/// TWO brackets.** Nothing on the wire names a bracket (`BracketSpec` has no id), so the node has
/// nothing to dedup by. A `Submit` re-sent with its id would be refused by the venue. The guard is
/// the client's: a `Disconnected` command is never re-sent. When a bracket gains a client-minted id,
/// this test flips.
#[test]
fn a_bracket_frame_delivered_twice_is_two_brackets() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("bracket-twice");
    let (mut ctl, _features) = control_stream(addr);
    for _ in 0..2 {
        write_frame(&mut ctl, &command(bracket_on(SECOND_VENUE, SECOND_SYMBOL))).expect("command");
        match read_frame::<_, Response>(&mut ctl).expect("ack") {
            Response::Ack { coid } => assert_eq!(coid, ""),
            other => panic!("expected Ack, got {other:?}"),
        }
    }
    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    assert!(
        wait_until(10, || observer
            .snapshot()
            .held_exits
            .iter()
            .filter(|h| h.symbol == SECOND_SYMBOL)
            .count()
            == 4),
        "two brackets, four held exits"
    );
    let parents: std::collections::BTreeSet<String> = observer
        .snapshot()
        .held_exits
        .iter()
        .filter(|h| h.symbol == SECOND_SYMBOL)
        .filter_map(|h| h.parent_order_id.clone())
        .collect();
    assert_eq!(parents.len(), 2, "two distinct entries: {parents:?}");
    mount.handle.shutdown_and_join();
}

/// **An Observe peer cannot place a bracket.** A bracket rides `Request::Command`, so the scope gate
/// that refuses an Observe `Submit` refuses it too, with nothing new to build.
#[test]
fn an_observe_peer_cannot_place_a_bracket() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("bracket-observe");
    let mut stream = TcpStream::connect(addr).expect("connect");
    let (nonce, _features) = hello_welcome(&mut stream);
    let mac = auth::sign(OBSERVE_KEY, &nonce, NODE_PROTO_VERSION, Scope::Read);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Read, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: Scope::Read } => {}
        other => panic!("expected AuthOk(Observe), got {other:?}"),
    }
    write_frame(&mut stream, &command(bracket_on(SECOND_VENUE, SECOND_SYMBOL))).expect("command");
    match read_frame::<_, Response>(&mut stream).expect("reply") {
        Response::AuthDenied { reason } => assert!(reason.contains("read-only"), "{reason}"),
        other => panic!("a bracket under Observe must be AuthDenied, got {other:?}"),
    }
    mount.handle.shutdown_and_join();
}
