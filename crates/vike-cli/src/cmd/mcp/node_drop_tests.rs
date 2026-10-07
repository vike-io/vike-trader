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
//! `mcp/tests/node_lifecycle.rs`'s `only_a_frame_the_node_pushed_ends_the_first_frame_wait` and, one per arm,
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
use std::net::{SocketAddr, TcpListener};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_mount::{MakerMount, MakerMountConfig, build_paper_maker_core};
use vike_tradehub::{publish, server};
use vike_tradehub_client::wire::{WireCommand, WireNodeIdentity};
use vike_tradehub_client::{CommandOutcome, NodeKeys};

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

#[path = "node_drop_tests/drops.rs"]
#[cfg(test)]
mod drops;
#[path = "node_drop_tests/policy_write.rs"]
#[cfg(test)]
mod policy_write;
#[path = "node_drop_tests/tunnel.rs"]
#[cfg(test)]
mod tunnel;
