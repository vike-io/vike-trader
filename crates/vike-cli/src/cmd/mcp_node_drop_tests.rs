//! The MCP server across a NODE DROP — a real in-process node, a real socket close, no scripting.
//!
//! What these prove is the "reconnect on the next call, never serve a stale frame" contract the
//! module doc of `mcp.rs` states under *When the node connection drops*. Each one drives the real
//! [`Server`] (its private fields are reachable here because this is a child module of `mcp`,
//! which is also why it is not an integration test: `Server`'s doc argues that no public
//! constructor may take a node address or a key) against a REAL paper node — the same
//! `vike_mount::build_paper_maker_core` + `vike_tradehub::{server, publish}` harness
//! `crates/vike-cli/tests/trade_node_e2e.rs` stands up — over a loopback [`Tunnel`] the test owns.
//!
//! # Why a tunnel, and why that is the honest simulation
//!
//! "The node goes away" has to close the client's sockets from the FAR side, because that is the
//! only thing the two client handles can observe: `RemoteCoreHandle`'s receive loop breaks on a
//! read error and `RemoteControlHandle`'s worker breaks on a failed write-then-read. Nothing in
//! `server::serve` lets a test reach the accepted streams it holds — it accepts forever and hands
//! each connection to its own thread — so the sockets the test closes are its own: a byte-copying
//! relay between an ephemeral loopback port and the node, whose [`Tunnel::cut`] closes every
//! socket it holds and stops accepting. That is not a stand-in for the failure; it IS the failure
//! the fix was written for. The remote-node setup an operator runs is exactly this shape — an
//! SSH tunnel in front of the daemon — and the tunnel dying is the first of the two triggers the
//! module doc names. The node itself keeps running behind it, which is also what lets the tests
//! prove the reconnected picture is POST-drop truth: an order placed at the real node while the
//! tunnel was down must show up in the frame the reconnected handle answers with.
//!
//! ⚠ Two facts measured on the Windows dev box while writing this, both of which shaped the
//! harness and neither of which is a property of the code under test:
//!
//! - **A paper core with no feed and no commands never publishes.** `seq` is bumped per publish
//!   and a publish follows a fold, so a freshly mounted node answers its `seq: 0` frame forever.
//!   `tool_node_snapshot` returns that at once, MARKED `pre_fold`, when the node STAMPS its frames
//!   with an identity block, as the shipped daemon does ([`spawn_stamped_node`]) — and only at its
//!   first-frame deadline when it does not, because on the two fields the wait reads an unstamped
//!   `seq: 0` frame is the client's own placeholder (`crates/vike-cli/src/cmd/trade.rs`'s
//!   `is_node_frame`). Every test that
//!   needs a frame past the first fold therefore [`seed_order`]s the node before its first read,
//!   the way the e2e suite's own submit does.
//! - **`shutdown` on a duplicated socket handle does not wake a blocked `read` on Windows** (it does
//!   on Linux, where CI runs). A relay that parked its pumps in `io::copy` and shut the clones from
//!   `cut` took 120 s to die — the pumps woke only when the node next pushed a frame — so the pumps
//!   poll a read timeout against a stop flag and CLOSE their sockets on exit instead. The same
//!   platform fact means dropping a LIVE `RemoteCoreHandle` here blocks until its peer sends or
//!   closes: the tunnel therefore cuts itself on `Drop`, and each test declares its tunnel AFTER its
//!   `Server` so the sockets are closed before the server's handle is joined. The node's own book
//!   is read off the core's snapshot cell rather than through a second live observer for the same
//!   reason.
//!
//! # The kill proof
//!
//! [`node_snapshot_after_the_node_goes_away_is_an_error_not_a_stale_frame`] is the test that must
//! go RED when `tool_node_snapshot`'s `is_connected` gate is removed (replace the condition with
//! `true`): without the gate the dead handle's cell answers with the last frame, `unwrap_err`
//! panics on an `Ok`, and the assertion names the frame it was handed. It was run that way once
//! when this file was written and restored; the commit message carries both outcomes.
//!
//! [`a_second_drop_before_a_frame_keeps_naming_the_first_drops_stale_seq`] is the kill proof for
//! the stale-`seq` bookkeeping: with the second pass assigning `snap.seq` unconditionally (as it
//! did for one commit) the error names the placeholder's `seq 0` and that test's "must not say
//! seq 0" assertion fails (run when the cutter below was rewritten: RED, on the gate path, naming
//! `seq 0`). That bookkeeping runs on ONE path only — the second pass holding a live handle that
//! then dies inside its first-frame wait — and the test reaches it through two CONDITIONS, not a
//! clock:
//!
//! - the cutter drops the reopened tunnel once the silent node has REGISTERED the reopened
//!   subscription, after which nothing left in the client's connect can fail — so the second pass
//!   always holds a live handle, and the test asserts the gate path's own reason wording;
//! - that node's frames cannot end the wait — it stamps no identity block and never folds past
//!   `seq: 0`, so `crates/vike-cli/src/cmd/trade.rs`'s `is_node_frame` is false for every one of
//!   them (both asserted off the
//!   node). Whether its one frame crosses the relay before the cut is a race this test does NOT
//!   control, and this is why it does not have to: either way the wait is still open when the
//!   handle dies.
//!
//! ⚠ It used to be killed by TIMING — a cutter thread slept 300 ms into what was then a wait that
//! could only end at its 2 s deadline — and on a box so loaded that the reconnect took longer than
//! that, the call failed at the CONNECT instead, the one path where the bookkeeping never runs, so
//! the kill proof could pass without proving anything. The one clock left is the tool's own
//! deadline, which the cut has to beat by the time a loopback relay takes to close; and since the
//! wait now also ends the moment the handle dies, the test no longer sits that deadline out either.
//!
//! ⚠ **This test is not a guard on the wait condition itself**, and two more mutations measured
//! where its edges are. A wait that ends at once (on the placeholder) reddens it, but at PASS ONE —
//! `snapshot_then_drop` is handed the placeholder — not at the second pass. A wait WIDENED to also
//! accept an unstamped pre-fold frame (`|| !snap.venue.is_empty()`, roughly "this handle received
//! anything") stayed GREEN, because the cut beat the silent frame across the relay; for such a
//! condition this test would be a race rather than a red. The condition's own guards are
//! `mcp_tests.rs`'s `only_a_frame_the_node_pushed_ends_the_first_frame_wait` and, one per arm,
//! [`a_node_that_has_not_folded_is_answered_at_once_and_marked_pre_fold`] and
//! [`a_folded_node_that_stamps_nothing_is_answered_without_waiting_out_the_deadline`] below. If the
//! condition is ever widened that way on purpose, re-plan the silent node here as one that sends no
//! frame at all.
//!
//! # The read tool and the order path wait DIFFERENTLY, and both halves are held
//!
//! Every drop test here reads through `tool_node_snapshot`, and the gate they hold is shared with
//! the order path (`mcp/node_reads.rs`'s `Server::node_frame`). What is NOT shared is the first-frame wait:
//! the read tool stops at the node's first frame and marks a placeholder `pre_fold`, while the
//! venue gate and the preview's epoch stamp keep waiting for a FOLD, exactly as before
//! (`mcp/node_reads.rs`'s `wait_for_a_fold` says why).
//! [`a_preview_against_a_node_that_has_not_folded_waits_for_the_fold_before_judging_the_venue`] is
//! the test that goes RED if the order path is ever moved onto the read tool's wait — and it holds
//! the VENUE read only: a write that names a venue reads `venues[]` first on the same handle, so
//! its epoch read always finds a post-fold frame, and moving `Server::node_accounts_epoch` ALONE
//! stayed green there.
//! [`a_venue_less_preview_against_a_node_that_has_not_folded_stamps_the_post_fold_epoch`] is the
//! epoch read's own guard: `cancel_order` names no venue, so the epoch read is that preview's
//! first read of the frame.
//!
//! [`a_trade_read_answers_an_idle_stamped_node_without_waiting_out_the_deadline`] holds the same
//! read wait one surface over — the `trade` plane's display reads share it
//! (`crates/vike-cli/src/cmd/trade.rs`'s `wait_for_first_frame`). It lives here because this file
//! owns the only real node in this crate's unit tests, the reason the next section gives.
//!
//! # The one test here that is not about a drop, and why it lives here anyway
//!
//! [`the_venue_gate_reads_a_real_nodes_mounted_set_and_refuses_what_is_not_in_it`] never cuts
//! anything. It is here because this file owns the only REAL NODE in `vike-cli`, and the venue
//! gate's two halves need different harnesses: the DECISION is a pure function
//! (`mcp/venue_gate.rs`'s `venue_verdict`) and is unit-tested beside it, while the EVIDENCE — `mcp.rs`'s
//! `Server::mounted_venues`, a stringly projection of `venues[].venue` out of the node's own frame
//! — can only be exercised against a node that publishes one. Its failure mode is what makes that
//! worth a test rather than a note: `mounted_venues` answers `Option`, and `None` means "no
//! evidence", which the gate deliberately ALLOWS. So a renamed or re-nested wire field disarms the
//! gate completely and every pure test stays green. Standing up a second node harness in a second
//! file to hold one test would duplicate [`Tunnel`] and everything around it; this test uses
//! [`spawn_node`], [`server_at`], [`seed_order`] and [`book`] and opens no tunnel at all.
//!
//! [`a_policy_write_previews_the_nodes_old_value_and_lands_on_its_token`] is the second, for the
//! same reason: a settings write's preview shows the node's CURRENT value (`old → new`), and a
//! value read OFF A NODE can only be proven against one. It stands its node up with a settings
//! store ([`spawn_node_with_settings`]) and, like the venue test, opens no tunnel.
//!
//! # Two shapes of close, and the verb for each
//!
//! [`Tunnel::cut`] is the node GOING AWAY: every socket closed AND the port stops accepting, so the
//! redial that follows must fail. [`Tunnel::drop_links`] is ONE CONNECTION ending while the node
//! stays up and reachable — the node's own idle close, a daemon that restarted, a tunnel reaping a
//! forwarded connection — so the redial must SUCCEED. They test opposite halves and neither can
//! stand in for the other: `a_write_after_a_drop_is_refused_once_and_the_next_call_reconnects` uses
//! the first, `a_write_after_the_nodes_idle_close_is_delivered_on_the_same_call` the second.
//!
//! # What these tests do NOT cover, stated so the green is read at its width
//!
//! Every drop here is one the socket REPORTS: the relay closes its sockets, the client's read
//! errors or its pre-write probe sees the FIN, `is_connected` flips. A link that dies SILENTLY —
//! the sleeping laptop under an `ssh -L` with no `ServerAliveInterval` — cannot be planted by a
//! relay at all (a relay that stops forwarding still holds sockets that the OS will eventually
//! report on), and it is now caught one layer down instead: `vike-tradehub-client`'s
//! `a_silently_dead_observe_link_flips_is_connected` scripts a node that goes silent while HOLDING
//! its socket open, which is the only honest way to plant it. What this file proves about that fix
//! is only its consequence — `is_connected` is the bit `tool_node_snapshot` gates on, and these
//! tests hold the gate.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_mount::{MakerMount, MakerMountConfig, build_paper_maker_core};
use vike_tradehub::{publish, server};
use vike_tradehub_client::wire::WireNodeIdentity;
use vike_tradehub_client::{CommandOutcome, NodeKeys};

use super::venue_gate::{VENUE_CHECK_MOUNTED, VENUE_CHECK_NONE};
use super::*;
use crate::cmd::nodekeys;

/// The paper mount's symbol. Any string works — nothing resolves it against a venue.
const TOKEN: &str = "MCP_NODE_DROP_TOKEN";
/// Far-future resolution so the A-S horizon is positive (mirrors the vike-mount offline mount test).
const RESOLUTION_TS: i64 = 3_000_000_000;
/// Obviously-fake HMAC keys — any bytes work as long as both sides agree.
const OBSERVE_KEY: &str = "DUMMY-observe-key-for-the-mcp-node-drop-tests";
const CONTROL_KEY: &str = "DUMMY-control-key-for-the-mcp-node-drop-tests";
/// The name [`spawn_stamped_node`]'s identity block carries — what a test reads back to prove an
/// answer is the node's frame and not the client's placeholder, which carries no identity at all.
const STAMPED_NAME: &str = "mcp-node-drop-stamped";

/// Build a PAPER node and serve it on an ephemeral loopback port with both scopes keyed and the
/// core's `CommandSink` threaded in (control ENABLED). The returned mount keeps the core alive;
/// the address is the NODE's — the tests never point the server at it directly.
///
/// Its publisher stamps NO identity block — the shape of a node that predates the block, and of
/// every node in this file but [`spawn_stamped_node`]'s.
fn spawn_node() -> (MakerMount, SocketAddr) {
    let (mount, addr, _publisher) = spawn_node_serving(None, None);
    (mount, addr)
}

/// [`spawn_node`], with the publisher STAMPING an identity block into every frame — which is what
/// the shipped daemon always does (`crates/vike-tradehub/src/node.rs`'s `spawn_with_mounts`
/// call), the `seq: 0` frame of a node that has folded nothing included.
fn spawn_stamped_node() -> (MakerMount, SocketAddr) {
    let (mount, addr, _publisher) = spawn_node_serving(None, Some(stamped_identity()));
    (mount, addr)
}

/// The identity block [`spawn_stamped_node`]'s publisher stamps into every frame.
fn stamped_identity() -> WireNodeIdentity {
    WireNodeIdentity {
        name: STAMPED_NAME.to_string(),
        strategy: "spread_maker".to_string(),
        params: String::new(),
        live: false,
        build: "vike-cli mcp_node_drop_tests".to_string(),
        advertise_addr: String::new(),
    }
}

/// [`spawn_node`], with the node also serving a SETTINGS source over `settings_dir` — the store a
/// `set_setting` writes one row into (`docs/decisions/0086`), through the same arm the shipped
/// daemon runs. No hot-apply seam: a policy key is never hot, so every write here answers restart.
fn spawn_node_with_settings(settings_dir: &std::path::Path) -> (MakerMount, SocketAddr) {
    let settings = server::settings::SettingsShowSource {
        settings_dir: Some(settings_dir.to_path_buf()),
        env: HashMap::new(),
        hot: None,
    };
    let (mount, addr, _publisher) = spawn_node_serving(Some(settings), None);
    (mount, addr)
}

/// The one node spawner the three above share. Also hands back the node's PUBLISHER, for the test
/// that has to act on a NODE-side event (a subscription registering) rather than on a clock.
fn spawn_node_serving(
    settings: Option<server::settings::SettingsShowSource>,
    identity: Option<WireNodeIdentity>,
) -> (MakerMount, SocketAddr, publish::PublisherHandle) {
    let cfg = MakerMountConfig::outcome_token("polymarket", TOKEN, Some(RESOLUTION_TS));
    let mount = build_paper_maker_core(&cfg);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(mount.handle.snapshot_cell(), identity);
    let serving = publisher.clone();
    let commands = Some(mount.handle.command_sink());
    let keys = NodeKeys::new(OBSERVE_KEY.as_bytes().to_vec(), CONTROL_KEY.as_bytes().to_vec());
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            serving,
            keys,
            commands,
            server::control::ControlLimitsConfig::default(),
            settings,
            // No `AccountAdminSource`: the account capability is an ABSENCE on every box that
            // has not DECLARED a barrier, which is every fixture here and every shipped box today.
            None,
            None,
        );
    });
    (mount, addr, publisher)
}

/// The relayed socket pairs a [`Tunnel`] currently holds: each pump's OWN stop flag beside its
/// join handle. Named so the two sites that spell it agree, and because clippy's
/// `type_complexity` (a merge gate on Linux CI, which this Windows box cannot run) refused the
/// inline spelling.
type Links = Arc<Mutex<Vec<(Arc<AtomicBool>, JoinHandle<()>)>>>;

/// A loopback relay standing in front of the node — the SSH tunnel of the remote-node setup.
///
/// Every accepted client socket is paired with a fresh connection to the node and pumped both
/// ways by two [`pump`] threads. The accept loop polls a non-blocking listener and the pumps poll
/// a read timeout, both against one stop flag, so [`Tunnel::cut`] can end every thread and let
/// each one CLOSE its sockets — the far-side close the client handles under test then observe.
/// Dropping the listener is what makes the port refuse the server's next dial (a preview, a
/// reconnect) rather than hang it.
struct Tunnel {
    /// Where the [`Server`] under test is pointed.
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    /// One entry per relayed socket pair: the pump's OWN stop flag and its join handle. Per-LINK
    /// rather than one shared flag, so [`Tunnel::drop_links`] can end the connections currently
    /// held through the tunnel while the accept loop keeps running and the next dial succeeds.
    links: Links,
    /// How many client connections the accept loop has taken — the one thing a test can observe
    /// about WHEN the server under test dials, without a clock: a fresh observe handle and every
    /// per-call verb (a preview's node dry-run) each open one.
    accepted: Arc<AtomicUsize>,
    accept: Option<JoinHandle<()>>,
}

impl Tunnel {
    /// Open a relay to `node`, bound at `bind` (`127.0.0.1:0` for any port, or a previous
    /// tunnel's address to come back on the SAME port, the way a restarted tunnel does).
    fn open(node: SocketAddr, bind: &str) -> io::Result<Tunnel> {
        let listener = TcpListener::bind(bind)?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let links: Links = Arc::default();
        let accepted = Arc::new(AtomicUsize::new(0));
        let accept = {
            let stop = Arc::clone(&stop);
            let links = Arc::clone(&links);
            let accepted = Arc::clone(&accepted);
            thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let client = match listener.accept() {
                        Ok((s, _)) => {
                            accepted.fetch_add(1, Ordering::AcqRel);
                            s
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(_) => break,
                    };
                    let Ok(upstream) = TcpStream::connect(node) else { continue };
                    // An accepted socket may inherit the listener's non-blocking flag; the pumps
                    // want a blocking read bounded by a timeout, not a spinning one.
                    client.set_nonblocking(false).expect("blocking client side");
                    let (c2, u2) = (client.try_clone().unwrap(), upstream.try_clone().unwrap());
                    // ONE flag per relayed pair (both directions share it), raised either by the
                    // whole tunnel dying or by `drop_links` ending just this connection.
                    let link = Arc::new(AtomicBool::new(false));
                    let mut held = links.lock().unwrap();
                    held.push((
                        Arc::clone(&link),
                        thread::spawn({
                            let (stop, link) = (Arc::clone(&stop), Arc::clone(&link));
                            move || pump(client, upstream, &stop, &link)
                        }),
                    ));
                    held.push((
                        Arc::clone(&link),
                        thread::spawn({
                            let (stop, link) = (Arc::clone(&stop), Arc::clone(&link));
                            move || pump(u2, c2, &stop, &link)
                        }),
                    ));
                }
                // Dropping `listener` here is what closes the port.
            })
        };
        Ok(Tunnel { addr, stop, links, accepted, accept: Some(accept) })
    }

    /// The tunnel dies: the port stops accepting and every relayed socket is closed from this
    /// side, so the node sees its peers go and the server under test sees the node go. Joins
    /// everything it spawned, so nothing is still copying bytes when the caller continues.
    fn cut(self) {
        drop(self);
    }

    /// Close every RELAYED SOCKET while the tunnel keeps accepting — the node is fine, the port is
    /// fine, but the connections held through it end CLEANLY (each pump shuts its writer, so both
    /// peers see EOF, never an RST).
    ///
    /// This is the "the node closed the idle link" failure, and it needs its own verb because
    /// [`Tunnel::cut`] cannot express it: cut also drops the listener, so the redial that must
    /// SUCCEED for the property under test would be refused. Nothing in `server::serve` lets a
    /// test reach the accepted streams the node holds (the harness doc says so), so closing them
    /// from the middle is how a test plants a close the node itself would have sent — which is
    /// also literally what an operator's SSH tunnel does when it reaps one forwarded connection.
    ///
    /// Joins the pumps, so every FIN is on the wire before this returns.
    fn drop_links(&self) {
        for (flag, _) in self.links.lock().unwrap().iter() {
            flag.store(true, Ordering::Release);
        }
        for (_, pump) in self.links.lock().unwrap().drain(..) {
            let _ = pump.join();
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        for (_, p) in self.links.lock().unwrap().drain(..) {
            let _ = p.join();
        }
    }
}

/// Copy bytes one way until either side ends, `stop` (the whole tunnel) is raised, or `link` (this
/// one relayed connection) is, then shut the writer so the far end sees EOF, and let both sockets
/// close on return. A bounded read rather than `io::copy`, for the Windows reason the module doc
/// measures.
fn pump(mut from: TcpStream, mut to: TcpStream, stop: &AtomicBool, link: &AtomicBool) {
    from.set_read_timeout(Some(Duration::from_millis(20))).expect("bounded read");
    let mut buf = [0u8; 8192];
    while !stop.load(Ordering::Acquire) && !link.load(Ordering::Acquire) {
        match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if to.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
    let _ = to.shutdown(Shutdown::Write);
}

/// A [`Server`] pointed at `addr` with BOTH node keys resolved — the configuration `vike-cli mcp
/// --node <tunnel>` runs with on a box whose store carries both keys. Everything else is what
/// [`test_server`] gives, so a field added there is not forgotten here.
fn server_at(addr: SocketAddr) -> Server {
    let mut s = test_server();
    s.node_addr = Some(addr.to_string());
    let keyed: HashMap<String, String> = [
        (nodekeys::OBSERVE_KEY_ENV.to_string(), OBSERVE_KEY.to_string()),
        (nodekeys::CONTROL_KEY_ENV.to_string(), CONTROL_KEY.to_string()),
    ]
    .into_iter()
    .collect();
    s.keys = nodekeys::resolve(&keyed, &HashMap::new(), None);
    s
}

/// Poll `cond` up to `secs`. The core folds on its own thread, the publisher fans out on another,
/// the relay copies on two more and the observer receives on yet another — so a fact takes a
/// moment to travel.
fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// A resting limit far from any market: with no feed it stays WORKING forever, so the node's
/// snapshot deterministically carries exactly what was placed.
fn submit_args() -> Value {
    json!({
        "venue": "polymarket", "symbol": TOKEN, "side": 1, "qty": 20.0,
        "order_type": "limit", "price": 0.40
    })
}

/// Place [`submit_args`] at the REAL node, straight past the tunnel, and return its coid once the
/// node has accepted it. Used to make the paper core FOLD (so it publishes a `seq > 0` frame at
/// all — see the module doc) and to change the node's book while the tunnel is down. A control
/// handle, not an observer: its `Drop` wakes its worker through the command channel, so it joins
/// promptly on every platform.
fn seed_order(node: SocketAddr) -> String {
    let direct =
        RemoteControlHandle::connect(node, CONTROL_KEY.as_bytes()).expect("direct control");
    let cmd = verbs::wire_command_for("submit_order", &submit_args()).unwrap();
    let cmd = verbs::fill_client_order_id(cmd, &mut verbs::coid_minter());
    let coid = match &cmd {
        WireCommand::Submit(o) => o.client_order_id.clone(),
        other => panic!("a submit, got {other:?}"),
    };
    let ticket = direct.try_command(cmd).expect("enqueued");
    assert_eq!(
        direct.await_outcome(ticket, Duration::from_secs(5)),
        Some(CommandOutcome::Accepted { coid: coid.clone() }),
        "the paper node accepts the resting limit"
    );
    coid
}

/// The coids in the node's OWN book, read off the core's snapshot cell — the node's truth, with
/// no second connection in the way.
fn book(mount: &MakerMount) -> Vec<String> {
    mount.handle.snapshot_cell().load().orders.iter().map(|o| o.client_order_id.clone()).collect()
}

/// Preview `submit_args` through the real tool arm and return the token it minted.
fn preview(s: &mut Server) -> (Value, String) {
    let pv = s.call_tool("submit_order", &submit_args()).expect("a preview is never an error");
    let token = pv["preview_token"].as_str().expect("a preview mints a token").to_string();
    (pv, token)
}

/// Confirm a previously minted token through the real tool arm — whatever it answers.
fn confirm(s: &mut Server, token: &str) -> Result<Value, String> {
    let mut args = submit_args();
    args["confirm"] = json!(true);
    args["preview_token"] = json!(token);
    // The MESSAGE is what every assertion in this file reads; the `refused` bit `ToolError` also
    // carries belongs to the transcript's classification and is pinned where that lives.
    s.call_tool("submit_order", &args).map_err(|e| e.message)
}

/// Bring the tunnel back — on the SAME port when the OS lets us (a restarted tunnel does), else on
/// a fresh one with the server re-pointed. The property under test is "no process restart", not
/// "same port": the server keeps every field it has, only `node_addr` is (possibly) reassigned.
fn restore_tunnel(node: SocketAddr, previous: SocketAddr, s: &mut Server) -> Tunnel {
    let tunnel = Tunnel::open(node, &previous.to_string())
        .or_else(|_| Tunnel::open(node, "127.0.0.1:0"))
        .expect("reopen the tunnel");
    s.node_addr = Some(tunnel.addr.to_string());
    tunnel
}

/// Take one live snapshot through the tunnel, then cut it and wait for the server's observe
/// handle to notice — the shared preamble of the two read tests. Returns the `seq` the dead
/// handle is holding, which is the number the error must name.
fn snapshot_then_drop(s: &mut Server, tunnel: Tunnel) -> u64 {
    let first = s.tool_node_snapshot().expect("a live node answers");
    assert!(first["seq"].as_u64().unwrap() > 0, "a real frame, not the placeholder: {first}");

    tunnel.cut();
    // The receive thread learns of the close asynchronously (its blocking read returns an error);
    // the property under test starts once the handle KNOWS. Read the stale seq AFTER that, since a
    // frame may have landed between the first read and the cut.
    let observe = s.observe.as_ref().expect("the first read opened the observe connection");
    assert!(wait_until(10, || !observe.is_connected()), "the observe handle must notice the drop");
    observe.snapshot().seq
}

/// **THE SAFETY DEFECT.** After the node goes away, `node_snapshot` used to answer with the last
/// frame the node ever pushed — complete, `seq` intact, nothing marking it as old. Now it is an
/// error that names the address DOWN, the frame STALE, and the `seq` of that frame.
#[test]
fn node_snapshot_after_the_node_goes_away_is_an_error_not_a_stale_frame() {
    let (mount, node) = spawn_node();
    seed_order(node);
    let mut s = server_at(node);
    let tunnel = Tunnel::open(node, "127.0.0.1:0").expect("open the tunnel");
    let addr = tunnel.addr;
    s.node_addr = Some(addr.to_string());
    let stale_seq = snapshot_then_drop(&mut s, tunnel);

    let err = s.tool_node_snapshot().unwrap_err();
    assert!(err.contains("DOWN"), "the error must say the connection is DOWN: {err}");
    assert!(err.contains("STALE"), "…and that the last frame is STALE: {err}");
    assert!(
        err.contains(&format!("seq {stale_seq}")),
        "…and name the stale frame's seq {stale_seq} so the agent knows which answer to distrust: {err}"
    );
    assert!(err.contains(&addr.to_string()), "…and the address it could not reach: {err}");
    assert!(s.observe.is_none(), "the dead handle must be let go of, so the next call redials");

    // Still down on the next call — still an error, still no handle held. The stale `seq` is not
    // in it any more: the last frame lived in the handle this process let go of, and carrying it
    // across calls would be exactly the new state the design refuses. What the agent gets is the
    // connect failure itself, which is still never a frame.
    let again = s.tool_node_snapshot().unwrap_err();
    assert!(again.contains("cannot open observe connection"), "{again}");
    assert!(again.contains(&addr.to_string()), "{again}");
    assert!(s.observe.is_none());
    assert_eq!(book(&mount).len(), 1, "reads changed nothing at the node");
}

/// The node comes back and the SAME server — no restart, no new `Server` — answers fresh: the
/// reconnected frame carries an order placed at the real node while the tunnel was down, which
/// the dead handle's cell could not possibly have held.
#[test]
fn node_snapshot_reconnects_once_the_node_is_back() {
    let (_mount, node) = spawn_node();
    seed_order(node);
    let mut s = server_at(node);
    let tunnel = Tunnel::open(node, "127.0.0.1:0").expect("open the tunnel");
    let previous = tunnel.addr;
    s.node_addr = Some(previous.to_string());
    let stale_seq = snapshot_then_drop(&mut s, tunnel);
    let down = s.tool_node_snapshot().unwrap_err();
    assert!(down.contains("DOWN"), "{down}");

    // Something happens at the node while the tunnel is down — straight to the node, not through
    // the server under test.
    let coid = seed_order(node);

    let _tunnel = restore_tunnel(node, previous, &mut s);
    // The fresh handle's first frame is whatever the publisher last framed; the order reaches it
    // a fold and a poll later, so read until it does — every read is a live one now.
    let mut last = Value::Null;
    assert!(
        wait_until(10, || {
            last = s.tool_node_snapshot().expect("the reconnected read must not error");
            last["orders"]
                .as_array()
                .is_some_and(|os| os.iter().any(|o| o["client_order_id"] == coid))
        }),
        "the reconnected snapshot must carry the order placed while the tunnel was down: {last}"
    );
    assert!(
        last["seq"].as_u64().unwrap() > stale_seq,
        "the frame is NEWER than the one the dead handle held (seq {stale_seq}): {last}"
    );
    assert!(
        s.observe.as_ref().is_some_and(RemoteCoreHandle::is_connected),
        "the answer came from a live handle held for the next call"
    );
}

/// A write after a drop is refused ONCE with the unknown / not-sent error, the dead handle is
/// discarded so the NEXT call reconnects, and after the tunnel is back the next preview + confirm
/// reaches the node. The refused command never lands: the node's book holds exactly the two
/// orders that were confirmed while the tunnel was up.
#[test]
fn a_write_after_a_drop_is_refused_once_and_the_next_call_reconnects() {
    let (mount, node) = spawn_node();
    let mut s = server_at(node);
    let tunnel = Tunnel::open(node, "127.0.0.1:0").expect("open the tunnel");
    let previous = tunnel.addr;
    s.node_addr = Some(previous.to_string());

    // 1. Up: preview + confirm reaches the node.
    let (pv1, token1) = preview(&mut s);
    assert_eq!(pv1["verified_by_node"], true, "the node dry-ran it through the tunnel: {pv1}");
    let ok1 = confirm(&mut s, &token1).expect("accepted through the tunnel");
    assert_eq!(ok1["outcome"], "accepted", "{ok1}");
    let coid1 = ok1["client_order_id"].as_str().unwrap().to_string();
    assert!(s.control.is_some(), "the control connection is held after a successful write");

    // 2. Down: the preview cannot be verified (the port refuses), and the confirm is refused with
    //    an outcome the agent cannot mistake for success — and NOTHING is retried.
    tunnel.cut();
    let (pv2, token2) = preview(&mut s);
    assert_eq!(pv2["verified_by_node"], false, "a dead tunnel cannot verify: {pv2}");
    let err = confirm(&mut s, &token2).unwrap_err();
    assert!(
        err.contains("UNKNOWN") || err.contains("not sent"),
        "the write after the drop must answer UNKNOWN or not-sent: {err}"
    );
    assert!(err.contains("node_snapshot"), "…and route the agent through the read tool: {err}");
    assert!(s.control.is_none(), "the dead control handle must be discarded for the next call");
    // The very next write, still down, reconnects — and fails at the CONNECTION, which is the
    // proof it tried: a "Gone" here would mean the dead handle was kept.
    let (_pv, token_down) = preview(&mut s);
    let still_down = confirm(&mut s, &token_down).unwrap_err();
    assert!(
        still_down.contains("cannot open control connection"),
        "with the tunnel still down the next call must REDIAL, not answer Gone: {still_down}"
    );
    assert!(s.control.is_none());

    // 3. Back: the same server, no restart — preview + confirm reach the node again.
    let _tunnel = restore_tunnel(node, previous, &mut s);
    let (pv3, token3) = preview(&mut s);
    assert_eq!(pv3["verified_by_node"], true, "verified again through the new tunnel: {pv3}");
    let ok3 = confirm(&mut s, &token3).expect("accepted through the restored tunnel");
    assert_eq!(ok3["outcome"], "accepted", "{ok3}");
    let coid3 = ok3["client_order_id"].as_str().unwrap().to_string();
    assert_ne!(coid1, coid3, "a fresh mint per order");
    assert!(s.control.is_some(), "…and the fresh handle is held for the next call");

    // THE proof that the refused write was never sent: the node's own book holds exactly the two
    // confirmed orders and nothing from step 2.
    assert!(
        wait_until(10, || book(&mount).contains(&coid3)),
        "the restored confirm must reach the node's book"
    );
    let ids = book(&mount);
    assert_eq!(ids.len(), 2, "exactly the two orders confirmed while the tunnel was up: {ids:?}");
    assert!(ids.contains(&coid1) && ids.contains(&coid3), "{ids:?}");
}

/// A dead handle that never held a frame is reported as NO frame, not as "seq 0 is STALE". An
/// UNSEEDED node never publishes (module doc), so the observe handle sits on the placeholder; cut
/// under it, the tool must say no frame was received — the placeholder was never an answer the
/// agent could have acted on, so naming it as the one to distrust would point at nothing.
#[test]
fn a_dead_handle_that_never_held_a_frame_is_reported_as_no_frame_not_as_seq_zero() {
    let (_mount, node) = spawn_node();
    let mut s = server_at(node);
    let tunnel = Tunnel::open(node, "127.0.0.1:0").expect("open the tunnel");
    let addr = tunnel.addr;
    s.node_addr = Some(addr.to_string());
    let first = s.tool_node_snapshot().expect("a live but silent node answers the placeholder");
    assert_eq!(first["seq"], 0, "an unseeded paper node never publishes: {first}");

    tunnel.cut();
    let observe = s.observe.as_ref().expect("the first read opened the observe connection");
    assert!(wait_until(10, || !observe.is_connected()), "the observe handle must notice the drop");

    let err = s.tool_node_snapshot().unwrap_err();
    assert!(err.contains("DOWN"), "the error must say the connection is DOWN: {err}");
    assert!(err.contains("No frame was received"), "…and that no frame was ever held: {err}");
    assert!(!err.contains("seq 0"), "the placeholder must not be named as a stale frame: {err}");
    assert!(err.contains(&addr.to_string()), "…and the address it could not reach: {err}");
    assert!(s.observe.is_none(), "the dead handle must be let go of, so the next call redials");
}

/// **THE FLAPPING TUNNEL.** Pass 1 finds the held handle dead on a REAL frame (seq N); the tunnel
/// comes back — to a SILENT node, whose frames can never end the tool's first-frame wait — and goes
/// again before the fresh handle holds a frame that would. The error must still name seq N: the
/// placeholder's 0 is not a frame, and overwriting N with it (which the second pass did for one
/// commit) told the agent to distrust an answer it never received while the seq-N picture it
/// actually holds went unnamed. See the module doc for what makes the second death a CONDITION
/// rather than a clock.
#[test]
fn a_second_drop_before_a_frame_keeps_naming_the_first_drops_stale_seq() {
    let (_mount, node) = spawn_node();
    seed_order(node);
    let mut s = server_at(node);
    let tunnel = Tunnel::open(node, "127.0.0.1:0").expect("open the tunnel");
    s.node_addr = Some(tunnel.addr.to_string());
    let stale_seq = snapshot_then_drop(&mut s, tunnel);
    assert!(stale_seq > 0, "pass 1 must hold a real frame for this test to mean anything");

    // The tunnel comes back in front of a node whose frames cannot end the wait: it stamps no
    // identity block and, never seeded, never folds past `seq: 0` — so on the two fields the wait
    // reads, its one frame is the client's own placeholder. Both halves are asserted off the node,
    // not assumed.
    let (silent_mount, silent_node, silent_publisher) = spawn_node_serving(None, None);
    assert!(silent_publisher.identity().is_none(), "the silent node must stamp no identity");
    let flapping = Tunnel::open(silent_node, "127.0.0.1:0").expect("reopen the tunnel");
    s.node_addr = Some(flapping.addr.to_string());
    // Cut once the silent node has REGISTERED the reopened connection's subscription — an event,
    // not a delay. The node registers it only after reading the client's `Subscribe`, and what is
    // left of the client's connect after writing that frame is local and cannot fail, so the
    // second pass is GUARANTEED to hold a live handle that then dies inside its wait: the one path
    // on which the stale-`seq` bookkeeping under test runs at all.
    let cutter = thread::spawn(move || {
        let subscribed = wait_until(10, || silent_publisher.subscriber_count() > 0);
        flapping.cut();
        subscribed
    });
    let err = s.tool_node_snapshot().unwrap_err();
    assert!(
        cutter.join().expect("the cutter thread completes"),
        "the reopened connection never subscribed at the silent node, so nothing below is about \
         a second pass that held a live handle: {err}"
    );
    assert_eq!(silent_mount.handle.snapshot_cell().load().seq, 0, "the silent node never folded");

    assert!(
        err.contains("dropped again before a frame was read"),
        "the SECOND pass held a live handle and found it dead — the gate's path, not the \
         connect's: {err}"
    );
    assert!(err.contains("DOWN"), "the error must say the connection is DOWN: {err}");
    assert!(
        err.contains(&format!("seq {stale_seq}")),
        "the FIRST drop's real seq {stale_seq} must survive the second drop: {err}"
    );
    assert!(err.contains("STALE"), "…flagged STALE: {err}");
    assert!(!err.contains("seq 0"), "the reopened handle's placeholder must not replace it: {err}");
    assert!(s.observe.is_none(), "both dead handles must be let go of");
}

/// **A READ IS ANSWERED AT ONCE — AND SAYS WHEN IT CARRIES NOTHING BUILT.** The shipped daemon stamps
/// its identity block into EVERY frame it publishes, including the one it publishes before its
/// first fold — and that frame is the node's own PLACEHOLDER (`vike_core`'s `CoreSnapshot::empty`):
/// `venues: []`, zero balance and equity, `accounts_epoch: 0`, none of them a reading. The stamp
/// tells it from the client's own `WireSnapshot::empty()` (which carries none), so `node_snapshot`
/// answers as soon as it lands instead of sitting out the deadline for a fold that an idle node
/// never makes — and marks it `pre_fold`, so an agent does not read `venues: []` as "nothing is
/// mounted".
///
/// The CONDITION is what is asserted first: the node's stamp, from a node whose own cell never left
/// `seq: 0`, MARKED as pre-fold — the marker is what main cannot produce, so it is the assertion
/// that tells the two apart, not the clock. The time bound is the symptom, at half the deadline.
#[test]
fn a_node_that_has_not_folded_is_answered_at_once_and_marked_pre_fold() {
    let (mount, node) = spawn_stamped_node();
    let mut s = server_at(node);

    let started = Instant::now();
    let snap = s.tool_node_snapshot().expect("a live node answers");
    let took = started.elapsed();

    assert_eq!(
        snap["identity"]["name"], STAMPED_NAME,
        "the node's stamped frame, never the client's placeholder: {snap}"
    );
    assert_eq!(snap["seq"], 0, "an unseeded paper node never folds, so it stays at seq 0: {snap}");
    assert_eq!(mount.handle.snapshot_cell().load().seq, 0, "…and the node's own cell agrees");
    assert_eq!(snap["pre_fold"], true, "a frame with nothing built must SAY so: {snap}");
    let note = snap["pre_fold_note"].as_str().expect("…in words an agent reads");
    assert!(
        note.contains("does NOT mean") && note.contains("venues"),
        "the note must say an empty venues list is not an answer about mounting: {note}"
    );
    assert!(
        took < Duration::from_secs(1),
        "the node's frame is in hand after one round trip, so the read must not wait out the \
         first-frame deadline — it took {took:?}"
    );
}

/// The other half of the same condition: a node that stamps NO identity block — one that predates
/// it — is answered as soon as a frame past its first fold lands, as it always was, and that frame
/// is NOT marked pre-fold. Pinned because waiting on `identity` alone would pass the test above
/// and quietly make every read against such a node sit out the whole deadline.
#[test]
fn a_folded_node_that_stamps_nothing_is_answered_without_waiting_out_the_deadline() {
    let (_mount, node) = spawn_node();
    seed_order(node);
    let mut s = server_at(node);

    let started = Instant::now();
    let snap = s.tool_node_snapshot().expect("a live node answers");
    let took = started.elapsed();

    assert!(snap["identity"].is_null(), "this node stamps no identity block: {snap}");
    assert!(snap["seq"].as_u64().unwrap() > 0, "a frame past the first fold: {snap}");
    assert_eq!(snap["pre_fold"], false, "a built frame is not marked pre-fold: {snap}");
    assert!(snap.get("pre_fold_note").is_none(), "…and carries no pre-fold note: {snap}");
    assert!(
        took < Duration::from_secs(1),
        "a folded node's frame is in hand after one round trip, so the read must not wait out \
         the first-frame deadline — it took {took:?}"
    );
}

/// **ORDER GATING IS NOT ON THE FAST PATH.** The venue gate reads `venues[]` off the node's frame
/// and the preview stamps its `accounts_epoch`, and on the placeholder a node publishes before its
/// first fold both are empty answers: `venues: []` makes the gate answer UNVERIFIED and let the
/// write through (decision 0041's declared pre-fold residual), and the epoch stamped is `0`. So the
/// write path keeps main's wait for a FOLD, and that wait is what carries a read made just before
/// the node's first fold past it.
///
/// The assertions are on what the preview DECIDED — the order of its own steps, the venue check,
/// the epoch it stamped — never on how long anything took. What the test controls is only WHEN the
/// node folds, on the first of two events after the preview's read has subscribed:
///
/// - the preview DIALS AGAIN (its node dry-run: the tunnel's second connection). A write path that
///   answered its venue read off the stamped placeholder does that at once, so the fold lands after
///   the venue was already judged `unverified` — RED, by event;
/// - the read is still waiting `FOLD_FALLBACK` later, which only a write path waiting for a fold
///   is. That one IS a delay, and it only paces the fold: it is far inside the read's deadline, and
///   no verdict is drawn from it.
#[test]
fn a_preview_against_a_node_that_has_not_folded_waits_for_the_fold_before_judging_the_venue() {
    /// How long the folder lets a write-path read sit on the placeholder before folding the node
    /// under it — a quarter of the read's ~2s deadline.
    const FOLD_FALLBACK: Duration = Duration::from_millis(500);

    let (mount, node, publisher) = spawn_node_serving(None, Some(stamped_identity()));
    let mut s = server_at(node);
    let tunnel = Tunnel::open(node, "127.0.0.1:0").expect("open the tunnel");
    s.node_addr = Some(tunnel.addr.to_string());
    let dialed = Arc::clone(&tunnel.accepted);
    let folder = thread::spawn(move || {
        let subscribed = wait_until(10, || publisher.subscriber_count() > 0);
        let fallback = Instant::now() + FOLD_FALLBACK;
        let dialed_before_the_fold = loop {
            if dialed.load(Ordering::Acquire) >= 2 {
                break true;
            }
            if Instant::now() >= fallback {
                break false;
            }
            thread::sleep(Duration::from_millis(5));
        };
        // Straight at the node, past the server under test.
        seed_order(node);
        (subscribed, dialed_before_the_fold)
    });
    let (pv, token) = preview(&mut s);
    let (subscribed, dialed_before_the_fold) = folder.join().expect("the folding thread completes");
    assert!(subscribed, "the preview never opened its observe connection: {pv}");

    assert!(
        !dialed_before_the_fold,
        "the preview moved past its venue read while the node had built nothing — the write path \
         answered off the stamped placeholder instead of waiting for the fold: {pv}"
    );
    assert_eq!(
        pv["venue_check"], VENUE_CHECK_MOUNTED,
        "the preview must judge the venue against the POST-fold frame, never against the stamped \
         placeholder's empty venues list: {pv}"
    );
    let stamped = s.pending.take(&token).expect("the preview issued a token").accounts_epoch;
    let node_epoch = mount.handle.snapshot_cell().load().accounts_epoch;
    assert_ne!(node_epoch, 0, "the folded node publishes a real account-set digest");
    assert_eq!(stamped, Some(node_epoch), "…and the preview stamped THAT, not the placeholder's 0");
}

/// **THE EPOCH STAMP WAITS FOR THE FOLD ON ITS OWN READ.** The test above holds the order path's
/// wait through the VENUE gate, and that is exactly why it cannot hold the epoch stamp as well: a
/// write that names a venue reads `venues[]` FIRST, on the same held handle, so by the time the
/// preview reaches `Server::node_accounts_epoch` that handle has already sat out the fold, and ANY
/// wait answers the post-fold frame. Moving `node_accounts_epoch` alone onto the read tool's fast
/// wait survived every test (an independent review's finding). A VENUE-LESS verb is the one shape
/// whose first read of the frame IS the epoch's: `cancel_order` names no venue, so
/// `Server::vet_commanded_venue` never calls `Server::mounted_accounts` (asserted below as
/// `venue_check: none`), the node dry-run dials its own per-call connection, and the observe
/// handle is opened by the epoch read itself.
///
/// Against a node that has not folded, that read must still sit on the stamped placeholder until
/// the fold lands and stamp the POST-fold epoch. Stamping the placeholder's `0` instead is not
/// harmless: the confirm, read after the fold, sees the real digest and refuses an "account-set
/// change" that never happened.
///
/// What is asserted is the EVENT — the node folds while the preview call is still out — never how
/// long anything took. The folder waits for the preview's subscription to register at the node,
/// then for whichever comes first: the call RETURNING (a fast read answers off the stamped
/// placeholder at once, so the fold lands after the stamp: RED, by event), or `FOLD_FALLBACK`,
/// which only paces the fold. It is far inside the read's ~2s deadline, and no verdict is drawn
/// from it.
#[test]
fn a_venue_less_preview_against_a_node_that_has_not_folded_stamps_the_post_fold_epoch() {
    /// How long the folder lets the epoch read sit on the stamped placeholder before folding the
    /// node under it — a quarter of the read's ~2s deadline. Pacing only.
    const FOLD_FALLBACK: Duration = Duration::from_millis(500);

    let (mount, node, publisher) = spawn_node_serving(None, Some(stamped_identity()));
    let cell = mount.handle.snapshot_cell();
    let mut s = server_at(node);
    let returned = Arc::new(AtomicBool::new(false));
    let folder = thread::spawn({
        let returned = Arc::clone(&returned);
        move || {
            let subscribed = wait_until(10, || publisher.subscriber_count() > 0);
            let fallback = Instant::now() + FOLD_FALLBACK;
            let returned_before_the_fold = loop {
                if returned.load(Ordering::Acquire) {
                    break true;
                }
                if Instant::now() >= fallback {
                    break false;
                }
                thread::sleep(Duration::from_millis(5));
            };
            // Nothing has folded the node up to here, so the fold below is its FIRST.
            let unfolded = cell.load().seq == 0;
            // Straight at the node, past the server under test.
            seed_order(node);
            (subscribed, returned_before_the_fold, unfolded)
        }
    });
    let previewed = s.call_tool("cancel_order", &json!({ "client_order_id": "neverplaced1" }));
    returned.store(true, Ordering::Release);
    let (subscribed, returned_before_the_fold, unfolded) =
        folder.join().expect("the folding thread completes");
    let pv = previewed.expect("a preview is never an error");

    assert!(subscribed, "the preview never opened its observe connection: {pv}");
    assert!(unfolded, "the node folded before the planted fold, so nothing here is pre-fold: {pv}");
    assert_eq!(
        pv["venue_check"], VENUE_CHECK_NONE,
        "a venue-less verb, so no venue read waited out the fold ahead of the epoch read: {pv}"
    );
    assert!(
        !returned_before_the_fold,
        "the preview returned while the node had built nothing — the epoch read answered off the \
         stamped placeholder instead of waiting for the fold: {pv}"
    );
    let token = pv["preview_token"].as_str().expect("a preview mints a token");
    let stamped = s.pending.take(token).expect("the preview issued a token").accounts_epoch;
    let node_epoch = mount.handle.snapshot_cell().load().accounts_epoch;
    assert_ne!(node_epoch, 0, "the folded node publishes a real account-set digest");
    assert_eq!(
        stamped,
        Some(node_epoch),
        "the preview stamped the POST-fold epoch, not the placeholder's 0: {pv}"
    );
}

/// **THE `trade` READS ANSWER AN IDLE NODE AT ONCE TOO.** `crate::cmd::trade`'s `connect_observe`
/// is the subscribe-and-wait every display read of the `trade` plane goes through (`trade order
/// ls`, `trade position ls`; the REPL's own reads share the same wait). It waited for `seq != 0`,
/// which an idle node never reaches, so every such read against one sat out the whole ~2s deadline
/// and then printed the very frame it had held since the first round trip — the read tool's defect,
/// one surface over.
///
/// Here rather than in `crate::cmd::trade`'s own tests because this file owns the only real node in
/// this crate's unit tests (see the module doc). The CONDITION is asserted first — the node's
/// stamped frame, from a node whose own cell never left `seq: 0` — and the time bound is the
/// symptom, at half the deadline.
#[test]
fn a_trade_read_answers_an_idle_stamped_node_without_waiting_out_the_deadline() {
    let (mount, node) = spawn_stamped_node();

    let started = Instant::now();
    let (handle, snap) =
        crate::cmd::trade::connect_observe(&node.to_string(), OBSERVE_KEY.as_bytes())
            .expect("a live node accepts the observe handshake");
    let took = started.elapsed();

    assert_eq!(
        snap.identity.as_ref().map(|i| i.name.as_str()),
        Some(STAMPED_NAME),
        "the node's stamped frame, never the client's placeholder"
    );
    assert_eq!(snap.seq, 0, "an unseeded paper node never folds, so it stays at seq 0");
    assert_eq!(mount.handle.snapshot_cell().load().seq, 0, "…and the node's own cell agrees");
    assert!(
        took < Duration::from_secs(1),
        "the node's frame is in hand after one round trip, so the read must not wait out the \
         first-frame deadline — it took {took:?}"
    );
    drop(handle);
}

/// **THE NEVER-SENT RECOVERY.** The link the MCP server holds is closed CLEANLY under it — what a
/// node's own idle close looked like, and what a daemon restart or a tunnel reaping one forwarded
/// connection still looks like — while the node itself keeps running and the port keeps accepting.
/// The next confirmed write must reach the node's book ON THIS CALL, not on the next one.
///
/// Why that is safe here and nowhere else: the sender proved the command was NEVER SENT (a peer FIN
/// observed BEFORE the write, `vike_tradehub_client`'s `link_is_dead`), so re-sending it cannot
/// double an execution that never happened. A command whose outcome is UNKNOWN is still refused —
/// `a_write_after_a_drop_is_refused_once_and_the_next_call_reconnects` above holds that half, and
/// the client crate's `a_post_send_loss_is_unknown` holds the classification underneath it.
///
/// The book assertion is the whole test: exactly TWO orders, one from before the close and one from
/// after, proves both that the recovery delivered and that it delivered ONCE.
#[test]
fn a_write_after_the_nodes_idle_close_is_delivered_on_the_same_call() {
    let (mount, node) = spawn_node();
    let mut s = server_at(node);
    let tunnel = Tunnel::open(node, "127.0.0.1:0").expect("open the tunnel");
    s.node_addr = Some(tunnel.addr.to_string());

    // 1. A confirmed write over a healthy link — this is what leaves a live control handle held.
    let (_pv1, token1) = preview(&mut s);
    let ok1 = confirm(&mut s, &token1).expect("accepted through the tunnel");
    assert_eq!(ok1["outcome"], "accepted", "{ok1}");
    let coid1 = ok1["client_order_id"].as_str().unwrap().to_string();
    assert!(s.control.is_some(), "the control connection is held after a successful write");

    // 2. The held connections are closed CLEANLY — the node is untouched and still reachable, so
    //    a redial succeeds. `drop_links` joins its pumps, so both FINs are on the wire before this
    //    returns; the short settle is for the client's socket to have the FIN queued and readable
    //    at the moment the sender peeks (loopback delivery, not a synchronisation the code needs).
    tunnel.drop_links();
    thread::sleep(Duration::from_millis(100));

    // 3. THE PROPERTY. One preview + one confirm, and the confirm ANSWERS ACCEPTED — the sender
    //    found the link dead before writing, this call reconnected and sent the command itself.
    //    Before this, the same sequence returned an UNKNOWN error and the order landed only if the
    //    agent went through node_snapshot and previewed + confirmed all over again.
    let (pv2, token2) = preview(&mut s);
    assert_eq!(pv2["verified_by_node"], true, "the node is still there to dry-run it: {pv2}");
    let ok2 =
        confirm(&mut s, &token2).expect("the never-sent command must be delivered, not refused");
    assert_eq!(
        ok2["outcome"], "accepted",
        "a command the node never saw must be sent on THIS call, not deferred to the next: {ok2}"
    );
    let coid2 = ok2["client_order_id"].as_str().unwrap().to_string();
    assert_ne!(coid1, coid2, "a fresh mint per order");
    assert!(s.control.is_some(), "…over a handle now held for the next call");

    // 4. It reached the NODE, and exactly once. Two confirmed writes, two orders in the book.
    assert!(
        wait_until(10, || book(&mount).contains(&coid2)),
        "the recovered confirm must reach the node's book"
    );
    let ids = book(&mount);
    assert_eq!(ids.len(), 2, "exactly the two confirmed orders — nothing was sent twice: {ids:?}");
    assert!(ids.contains(&coid1) && ids.contains(&coid2), "{ids:?}");
}

/// A venue the node does not mount is REFUSED, against a node that really publishes its mounted
/// set — and the venue it DOES mount is previewed, confirmed and stamped `mounted` on both halves.
///
/// ⚠ **What this holds that the pure tests cannot.** `mcp/venue_gate.rs`'s `venue_verdict` is unit-tested in
/// every direction it has, but its refusing direction is only ever reached when
/// `Server::mounted_venues` answers `Some(..)` — and nothing else in this crate ever makes it do
/// so (`test_server()` has no node, so every inline preview test takes the `unverified` ALLOW
/// path). That projection is `snap["venues"][*]["venue"]`, three stringly hops into a wire type
/// this crate does not own: rename the field, nest it one level deeper, or drop it from the
/// publisher, and `mounted_venues` answers `None` for every write on every node. `None` is
/// "no evidence", which the gate deliberately ALLOWS — so the gate would be disarmed completely,
/// in silence, with every pure test still green. This is the test that reddens instead.
///
/// The refusal half asserts the message names the venue the node ACTUALLY reports, which is what
/// makes it a test of the evidence rather than of the decision; the allow half is the control that
/// stops the whole thing passing because the gate refuses everything.
#[test]
fn the_venue_gate_reads_a_real_nodes_mounted_set_and_refuses_what_is_not_in_it() {
    // A string no node mounts, and deliberately a REAL venue rather than the incident's invented
    // `"node"`: the gate compares against what this node reports, not against the shipped roster,
    // and a roster check would let this one through as a known venue.
    const UNMOUNTED: &str = "binance";

    let (mount, node) = spawn_node();
    seed_order(node);
    let mut s = server_at(node);

    // What the node itself says it mounts, read through the same tool the refusal tells the agent
    // to call — so the expectation below is the NODE's answer rather than this file's memory of
    // how `spawn_node` was configured.
    let snap = s.tool_node_snapshot().expect("a live node answers");
    let mounted: Vec<String> = snap["venues"]
        .as_array()
        .expect("a real frame carries a venues array")
        .iter()
        .filter_map(|v| v["venue"].as_str())
        .map(str::to_string)
        .collect();
    assert!(!mounted.is_empty(), "the seeded node must publish at least one venue block: {snap}");
    assert!(
        !mounted.iter().any(|v| v == UNMOUNTED),
        "{UNMOUNTED} must NOT be mounted: {mounted:?}"
    );

    // 1. THE REFUSAL. Not an answer carrying a warning — a tool ERROR, so there is no payload for
    //    a token to ride on.
    let mut bad = submit_args();
    bad["venue"] = json!(UNMOUNTED);
    let err = s.call_tool("submit_order", &bad).expect_err("an unmounted venue must be refused");
    assert!(err.message.contains(UNMOUNTED), "the refusal must name the offending value: {err:?}");
    for venue in &mounted {
        assert!(
            err.message.contains(venue),
            "the refusal must name the venue the NODE reports mounting ({venue}) — this is the \
             assertion that fails if `Server::mounted_venues` stops reading the frame: {err:?}"
        );
    }
    assert!(err.message.contains("node_snapshot"), "…and the read tool that reports it: {err:?}");
    assert!(err.refused, "a GATE said no — the transcript must classify it as a refusal: {err:?}");
    // Nothing was minted AT ALL — not "a token that cannot be used", none. `next` is the mint
    // counter, so this is the whole session's history, not just the current entry.
    assert_eq!(
        s.pending.next, 0,
        "the refusal must come BEFORE the token is minted: the command has to be unconfirmable, \
         not merely unconfirmed"
    );

    // 2. THE ALLOW PATH — the control. Without it, a `mounted_venues` that answered `Some(vec![])`
    //    (or a verdict that refused unconditionally) would pass step 1 and break every real write.
    let (pv, token) = preview(&mut s);
    assert_eq!(
        pv["venue_check"], VENUE_CHECK_MOUNTED,
        "a venue the node reports must be compared and PASS, not fall through to unverified: {pv}"
    );
    let ok = confirm(&mut s, &token).expect("the mounted venue is accepted");
    assert_eq!(ok["outcome"], "accepted", "{ok}");
    // ...and the ACCEPTED answer states the disposition too. `unverified` is a real allow, so the
    // record of the write that actually happened has to say whether the gate ran.
    assert_eq!(
        ok["venue_check"], VENUE_CHECK_MOUNTED,
        "the executed write states its venue check: {ok}"
    );
    let coid = ok["client_order_id"].as_str().unwrap().to_string();

    // 3. THE BOOK. The seeded order plus the accepted one, and nothing from the refused call —
    //    the operator-facing property, read off the node's own snapshot cell.
    assert!(wait_until(10, || book(&mount).contains(&coid)), "the accepted order reaches the book");
    let ids = book(&mount);
    assert_eq!(
        ids.len(),
        2,
        "the seed and the accepted order — the refused one never left: {ids:?}"
    );
}

/// The one policy ceiling [`a_policy_write_previews_the_nodes_old_value_and_lands_on_its_token`]
/// writes — a risk ceiling, the key class the deleted retype was demanded for.
const CEILING_KEY: &str = "policy.max_notional_per_order";

/// What the node's settings DATABASE holds for the ceiling — the node's truth, read off the store
/// rather than off anything the tool answered.
fn stored_ceiling(dir: &std::path::Path) -> Option<String> {
    let source = vike_secrets::read_settings_in(dir).expect("read the node's settings database");
    let rows = &source.rows().expect("rows").settings;
    rows.iter()
        .find(|r| r.section == "policy" && r.key == "max_notional_per_order")
        .map(|r| r.value.clone())
}

/// **A policy `set_setting` against a REAL node: the preview shows the node's OWN old value, and
/// the preview token alone confirms it** (`docs/decisions/0086` point 7 — no `policy_confirm`, no
/// retype).
///
/// The pure tests beside [`Server::call_tool`] can only prove the `change` field's SHAPE — with no
/// node there is no old value to show. This is the half that proves the value is READ from the node
/// the write is aimed at, and that the preview → token → write path lands the row with nothing else
/// asked for. The old value the preview must show is read through the node's own `SettingsShow`, so
/// the assertion is about the line and not about how the node renders a float.
#[test]
fn a_policy_write_previews_the_nodes_old_value_and_lands_on_its_token() {
    let store = tempfile::tempdir().expect("tempdir");
    vike_secrets::plant_settings_rows(
        store.path(),
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "policy".into(),
                key: "max_notional_per_order".into(),
                value: "100".into(),
            }],
            arming: Vec::new(),
            venue: Vec::new(),
        },
    )
    .expect("seed the node's settings database");
    let (_mount, node) = spawn_node_with_settings(store.path());
    // The module doc's first measured fact: a paper core that has folded nothing never publishes,
    // and the preview and the confirm each read the node's frame for its account epoch.
    seed_order(node);
    let mut s = server_at(node);
    let old = vike_tradehub_client::settings_show(node, OBSERVE_KEY.as_bytes())
        .expect("the node serves its settings")
        .rows
        .into_iter()
        .find(|r| r.key == CEILING_KEY)
        .expect("the node carries the ceiling row")
        .value;

    let args = json!({ "key": CEILING_KEY, "value": "250" });
    let pv =
        s.call_tool("set_setting", &args).expect("a policy write PREVIEWS — nothing refuses it");
    assert_eq!(pv["will_execute"], json!(false), "{pv}");
    assert_eq!(
        pv["change"]["old"],
        json!(old),
        "the node's CURRENT value, read off the node: {pv}"
    );
    assert_eq!(pv["change"]["new"], json!("250"), "{pv}");
    assert_eq!(pv["change"]["line"], json!(format!("{CEILING_KEY}: {old} → 250")), "{pv}");
    assert_eq!(stored_ceiling(store.path()).as_deref(), Some("100"), "a preview writes nothing");

    let mut confirming = args.clone();
    confirming["confirm"] = json!(true);
    confirming["preview_token"] = pv["preview_token"].clone();
    let ok = s.call_tool("set_setting", &confirming).expect("the token alone confirms it");
    assert_eq!(ok["outcome"], "accepted", "{ok}");
    assert_eq!(ok["restart_required"], json!(true), "policy is never hot: {ok}");
    assert_eq!(
        stored_ceiling(store.path()).as_deref(),
        Some("250"),
        "the confirmed write lands in the node's settings database: {ok}"
    );
}
