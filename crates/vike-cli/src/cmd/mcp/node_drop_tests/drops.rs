//! The tests that drive the server across a node drop, and the other tests a real node answers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::tunnel::Tunnel;
use super::*;
use crate::cmd::mcp::venue_gate::{VENUE_CHECK_MOUNTED, VENUE_CHECK_NONE};

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
