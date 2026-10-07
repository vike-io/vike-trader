//! The node READ tools — `node_snapshot` (the pushed book), `strategy_status` and `settings_show`
//! (the two per-call reads) — and the observe connection they share.
//!
//! The order path reads the same frame through `Server::order_gate_snapshot`, one gate with two
//! first-frame waits (`FirstFrameWait`), so everything that opens, waits on or lets go of the
//! observe handle lives here. Split out of `cmd/mcp.rs` (code-layout phase 2, task 9); the module
//! doc there ("When the node connection drops") is the argument for the reconnect-on-next-call
//! design this file implements.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_tradehub_client::RemoteCoreHandle;
use vike_tradehub_client::wire::WireSnapshot;

use super::*;
use crate::cmd::nodekeys;

impl Server {
    /// Read the live node snapshot (orders / positions / equity / recent events) via the observe
    /// connection. Opens it lazily, then waits briefly for the node's first pushed frame — and
    /// answers ONLY from a connection that is still delivering frames.
    ///
    /// ⚠ **A read must never lie, and this used to.** `RemoteCoreHandle::snapshot` is a load off
    /// the handle's arc-swap cell, and the receive loop that fills it breaks on the first read
    /// error WITHOUT clearing the cell — it only flips `is_connected` to `false`. So after the
    /// node went away (a tunnel dropping, the daemon restarting) this tool kept returning the last
    /// frame the node ever pushed: complete, well-formed, `seq` intact, and with nothing on it
    /// saying it was old. An agent asking "what are my positions" got the answer from before the
    /// drop, with no error, and acted on it. That is the safety defect this method now closes.
    ///
    /// The shape: exactly TWO passes over "ensure a handle, wait for a frame, check it is live".
    /// The first pass runs on whatever handle is held; if that one is dead it is dropped and the
    /// second pass opens ONE fresh connection in this same call, so a tunnel that has come back is
    /// transparent to the agent. If the fresh connection cannot be opened, or drops again before
    /// a frame is read, the answer is an ERROR naming the address as DOWN and the last frame's
    /// `seq` as STALE — never that frame as a result. The next call tries again from scratch.
    /// There is no third pass and no sleep between the two: a read is cheap to repeat and the
    /// agent is told it may.
    ///
    /// **TWO waits share that ONE gate, and which one a caller gets is the whole difference
    /// between a read and an order** ([`FirstFrameWait`]). This tool, and the
    /// `vike://node/snapshot` resource that routes here, wait only for the node's FIRST frame on
    /// the connection ([`crate::cmd::trade::wait_for_first_frame`], the one display-read wait the
    /// `trade` REPL shares) and then MARK a frame that carries nothing built yet as `pre_fold`
    /// ([`node_read_answer`]). The ORDER path's reads go through
    /// [`Server::order_gate_snapshot`] instead and keep main's wait for a FOLD
    /// ([`wait_for_a_fold`]) — that function's doc says why the latency is paid on purpose there.
    /// The two passes, the gate and the stale-`seq` bookkeeping below live in
    /// [`Server::node_frame`] and are the same for both.
    ///
    /// The liveness check is deliberately AFTER the wait, not before it, so there is ONE gate and
    /// it covers the whole read: a handle that was alive when this call started and died while
    /// the call waited for its first frame would otherwise answer with the `seq: 0` placeholder as
    /// though the node had said "nothing". (A frame's AGE is not reported: the handle records no
    /// receipt time, and inventing one here from the call's own clock would be a number about this
    /// process, not about the node.)
    ///
    /// The stale `seq` is named on the call that FINDS the handle dead, and on that call only: the
    /// last frame lives in the handle, and the handle is let go of. A later call while the node is
    /// still down answers with [`Server::ensure_observe`]'s own connect error — still never a
    /// frame — rather than a remembered number, because remembering it would be state carried
    /// across calls, which this design deliberately has none of. Within the call, only a REAL
    /// frame's `seq` is ever named: a handle found dead while holding the `seq: 0` placeholder
    /// (opened, never delivered a frame, dropped) has no stale frame, and the error says "no frame
    /// was received" rather than calling the placeholder one. (The second pass used to assign
    /// `snap.seq` unconditionally, so a tunnel that came back and went again before its first
    /// frame reported "seq 0 is STALE" — losing the first pass's real number in exactly the
    /// flapping case the number was written for.)
    ///
    /// ⚠ **The gate is exactly as good as `is_connected`, and that bit got wider.** It flips when
    /// the receive thread's read ERRORS — a drop the TCP stack delivers (the tunnel process
    /// exiting, the daemon restarting, a FIN or RST) and, since the node grew an idle heartbeat,
    /// also when NOTHING arrives for three beats. So a link that dies without a packet (the laptop
    /// sleeping under an `ssh -L` with no `ServerAliveInterval`) no longer leaves this gate passing
    /// the pre-drop frame through as live for hours: it is caught within
    /// `vike_tradehub_client::liveness::OBSERVE_READ_TIMEOUT` and the second pass reconnects.
    /// The residuals, stated so the green is read at its width: that window is 45 s, the deadline
    /// is armed only against a node advertising the capability (a mixed-version deployment gets the
    /// old width, silently), and a frame's AGE is still not reported — the handle records no
    /// receipt time and the frame carries no node-side timestamp, so this remains a LIVENESS gate,
    /// never a freshness one.
    pub(super) fn tool_node_snapshot(&mut self) -> Result<Value, String> {
        let snap = self.node_frame(FirstFrameWait::NodeFrame)?;
        node_read_answer(&snap)
    }

    /// The node's frame as the ORDER path reads it — the venue gate ([`Server::mounted_accounts`])
    /// and the preview's account-epoch stamp ([`Server::node_accounts_epoch`]) — after main's wait
    /// for a FOLD ([`wait_for_a_fold`]), serialized as-is with no read-tool annotation: what these
    /// two read is byte-for-byte what they read before the read tools got a faster wait.
    pub(super) fn order_gate_snapshot(&mut self) -> Result<Value, String> {
        let snap = self.node_frame(FirstFrameWait::Fold)?;
        serde_json::to_value(&*snap).map_err(|e| format!("cannot serialize snapshot: {e}"))
    }

    /// The two-pass, one-gate read [`Server::tool_node_snapshot`]'s doc describes, with the
    /// first-frame wait as its one parameter. Answers ONLY a frame from a handle that is still
    /// alive after the wait; a dead one is let go of and one fresh connection is tried.
    fn node_frame(&mut self, wait: FirstFrameWait) -> Result<Arc<WireSnapshot>, String> {
        // Whether a pass has found a held handle dead — the fact that turns a plain connect error
        // into the DOWN error — and the `seq` of the last REAL frame a dead handle was holding,
        // carried into that error so the agent can tell WHICH earlier answer it must not act on.
        // The two are separate on purpose: a handle found dead on the `seq: 0` placeholder held
        // no frame, and a pass that saw a real frame must not be overwritten by a later one that
        // saw none. Before a handle has been found dead, `ensure_observe`'s own connect error is
        // returned unchanged: a node that was never reached has no stale frame to warn about.
        let mut found_dead = false;
        let mut stale_seq: Option<u64> = None;
        for _pass in 0..2 {
            if let Err(e) = self.ensure_observe() {
                return Err(if found_dead { self.observe_down(stale_seq, &e) } else { e });
            }
            let observe = self.observe.as_ref().expect("ensured");
            // A freshly-subscribed connection holds the client's own empty placeholder until the
            // node's first frame lands — how long to wait, and for what, is the caller's.
            let snap = match wait {
                FirstFrameWait::NodeFrame => crate::cmd::trade::wait_for_first_frame(observe),
                FirstFrameWait::Fold => wait_for_a_fold(observe),
            };
            // ── THE GATE ─────────────────────────────────────────────────────────────────────
            // The cell is only evidence of the node's state while the thread filling it is alive.
            if observe.is_connected() {
                return Ok(snap);
            }
            // Dead: remember what it held — if it held anything — let go of it, and let the second
            // pass open a fresh one. Dropping the handle joins its receive thread; nothing is sent
            // in either direction.
            found_dead = true;
            if snap.seq > 0 {
                stale_seq = Some(snap.seq);
            }
            self.observe = None;
        }
        Err(self.observe_down(
            stale_seq,
            "the reopened connection dropped again before a frame was read",
        ))
    }

    /// The `node_snapshot` error for a node that is DOWN: names the address, the reason this call
    /// could not get a live frame, and the `seq` of the last frame this process received — flagged
    /// STALE so the agent knows WHICH earlier answer it must not act on — and says the next call
    /// will try again, so the agent neither retries in a loop nor gives up on the session.
    /// `None` is a dead handle that never held a frame (the `seq: 0` placeholder), and is worded
    /// as exactly that rather than as "seq 0 is stale": a frame that was never received cannot be
    /// the earlier answer the agent is being told to distrust.
    fn observe_down(&self, stale_seq: Option<u64>, why: &str) -> String {
        let addr = self.node_addr.as_deref().unwrap_or("<no node configured>");
        let stale = match stale_seq {
            Some(seq) => format!(
                "The last frame this session received (seq {seq}) is STALE and is deliberately \
                 NOT returned"
            ),
            None => "No frame was received on the dropped connection, so there is no stale frame \
                     to name — any earlier node_snapshot result is older still"
                .to_string(),
        };
        format!(
            "the observe connection to {addr} is DOWN ({why}). {stale}: do not act on any earlier \
             node_snapshot result — orders and positions may have changed since it was pushed. The \
             next node_snapshot call will try to reconnect again."
        )
    }

    /// Ask the node WHAT IT IS RUNNING — the strategy-level read verb
    /// ([`vike_tradehub_client::strategy_status`]), returned as its wire payload verbatim.
    ///
    /// ⚠ **A PER-CALL connection, not `self.observe`, and that is the verb's shape rather than a
    /// shortcut.** The held observe handle is a SUBSCRIBED push pipe — it delivers `WireSnapshot`
    /// frames and nothing else — so there is no way to ask it a question at all. The client
    /// function opens one short-lived `Scope::Read` connection, sends the request, reads the
    /// answer and drops it, which is also why nothing here participates in
    /// [`Server::tool_node_snapshot`]'s liveness dance: a per-call read cannot serve a stale frame,
    /// because it holds no frame between calls.
    ///
    /// The OBSERVE key is the right (and only) credential: the node serves this verb read-only
    /// under either scope, and the client always connects under observe — a control key cannot
    /// substitute, since the node verifies each scope against its own key.
    ///
    /// The client refuses CLIENT-SIDE against a node whose `Welcome.features` does not advertise
    /// the strategy verbs, so nothing is sent to a node that could not decode the frame; that
    /// arrives here as `io::ErrorKind::Unsupported` and [`node_read_failure`] adds the one action
    /// that fixes it.
    pub(super) fn tool_strategy_status(&self) -> Result<Value, String> {
        let (addr, key) = self.observe_read_target()?;
        match vike_tradehub_client::strategy_status(addr.as_str(), key.as_bytes()) {
            // Serialized VERBATIM, the same convention `vike-cli trade status --json` follows for
            // this payload (it nests it under a `strategy_status` key beside the trading mode,
            // precisely so the payload itself stays byte-for-byte the node's): an agent gets the
            // wire shape rather than a second hand-maintained schema that could come to disagree
            // with it.
            Ok(status) => serde_json::to_value(status)
                .map_err(|e| format!("strategy_status: the node's answer did not serialize: {e}")),
            Err(e) => Err(node_read_failure("strategy_status", &addr, &e)),
        }
    }

    /// Ask the node for its EFFECTIVE SETTINGS — the read half of the settings pair
    /// ([`vike_tradehub_client::settings_show`]), returned as its wire payload verbatim.
    ///
    /// Same per-call shape and the same observe-key rule as [`Server::tool_strategy_status`]; the
    /// capability it negotiates is the settings-SHOW one, which is deliberately separate from the
    /// settings-WRITE capability `set_setting` needs — a node may serve one and not the other, and
    /// the two refusals name different strings for that reason.
    ///
    /// ⚠ Nothing here redacts anything, and nothing here needs to: `WireSettingsRow`'s values
    /// arrive already redacted, because the node performs it in the shared builder ON
    /// CONSTRUCTION. Re-applying a rule this crate would have to keep in step with the node's is
    /// the second-authority failure, not a belt.
    pub(super) fn tool_settings_show(&self) -> Result<Value, String> {
        let (addr, key) = self.observe_read_target()?;
        match vike_tradehub_client::settings_show(addr.as_str(), key.as_bytes()) {
            Ok(show) => serde_json::to_value(show)
                .map_err(|e| format!("settings_show: the node's answer did not serialize: {e}")),
            Err(e) => Err(node_read_failure("settings_show", &addr, &e)),
        }
    }

    /// The address and OBSERVE key a per-call read verb needs, or the message saying which of the
    /// two is missing. Shared by the two per-call reads so they cannot answer differently about a
    /// box that is configured the same way for both.
    pub(super) fn observe_read_target(&self) -> Result<(String, String), String> {
        let addr = self.node_addr.clone().ok_or(
            "no vike-tradehub node configured — pass `--node <host:port>` to read node state",
        )?;
        let (key, _) =
            self.keys.observe().ok_or_else(|| self.missing_key(nodekeys::OBSERVE_KEY_ENV))?;
        Ok((addr, key.to_string()))
    }

    /// The "this key is nowhere" tool error. It names BOTH sources, because the old message said
    /// "is not set in the environment" while the daemon's own keys sat unread in the credential
    /// store — an agent (and the human reading its transcript) has no way to diagnose that.
    ///
    /// ⚠ The second command it names is `backend status`, NOT `secrets list`. A node key is not in
    /// the credential store any more (`docs/decisions/0051-node-keys-live-in-their-own-store.md`),
    /// and `secrets list` deliberately does not list one — an agent told to run it would read an
    /// accurate "not there" about the wrong file and conclude the key is missing when it is not.
    pub(super) fn missing_key(&self, name: &str) -> String {
        format!(
            "{name} is set neither in the process environment nor in the node-key store — \
             run `vike-cli secrets path` to see which stores this project resolves to, and \
             `vike-cli backend status` to see whether this box holds a node key"
        )
    }

    /// Open the observe (read) connection if not already open. Errors if `--node`/observe key absent.
    ///
    /// Same rule as [`Server::ensure_control`]: a HELD handle is returned as-is, alive or not. The
    /// liveness gate is [`Server::node_frame`]'s — the one reader of the handle, behind both the
    /// read tool and the order path — which drops a dead handle and calls this again in the same
    /// call, so the "fresh connection" a reconnect opens is the same code path as the first
    /// connection, with no second way to dial the node.
    fn ensure_observe(&mut self) -> Result<(), String> {
        if self.observe.is_some() {
            return Ok(());
        }
        let addr = self.node_addr.as_ref().ok_or(
            "no vike-tradehub node configured — pass `--node <host:port>` to read node state",
        )?;
        let (key, _) =
            self.keys.observe().ok_or_else(|| self.missing_key(nodekeys::OBSERVE_KEY_ENV))?;
        let handle = RemoteCoreHandle::connect(addr.as_str(), key.as_bytes())
            .map_err(|e| format!("cannot open observe connection to {addr}: {e}"))?;
        self.observe = Some(handle);
        Ok(())
    }
}

/// Which first-frame wait a node read uses — the ONE thing the read tools and the order path do
/// differently with the shared two-pass gate ([`Server::node_frame`]).
#[derive(Clone, Copy)]
enum FirstFrameWait {
    /// The read tools (`node_snapshot` and its resource): the handle's first frame FROM THE NODE
    /// (`crate::cmd::trade::is_node_frame`), the handle's death, or ~2s —
    /// [`crate::cmd::trade::wait_for_first_frame`], the ONE display-read wait, which the `trade`
    /// REPL and its one-shot `ls` verbs take too. A death ends it because no frame can follow
    /// one, and the gate decides what a dead handle answers, as it always did.
    NodeFrame,
    /// The order path (the venue gate and the preview's epoch stamp): a frame past the node's
    /// first fold, [`wait_for_a_fold`] — the wait every read used before 2026-10-03.
    Fold,
}

/// The ORDER path's wait, unchanged from what every read used before 2026-10-03: up to ~2s for a
/// frame past the node's first fold (`seq > 0`), and nothing else ends it.
///
/// ⚠ **Main's wait on purpose, and the latency it costs is paid on purpose.** The order path's
/// reads JUDGE a write. The venue gate ([`venue_gate::venue_verdict`] over [`Server::mounted_accounts`])
/// answers [`venue_gate::VENUE_CHECK_UNVERIFIED`] when the frame's `venues` is empty — and lets the write
/// through, to the PRIMARY engine if its venue routes to nothing ([`Server::vet_commanded_venue`])
/// — and the preview stamps the frame's `accounts_epoch` for its confirm to compare. The frame a
/// node publishes before its first fold is its own placeholder: `venues: []` and epoch `0`. A read
/// that stopped on it would turn EVERY write before a node's first fold into an unverified allow,
/// widening `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md`'s declared
/// pre-fold residual from "a read made more than ~2s before the first fold" to "any read before
/// it", and a preview's and its confirm's epochs would both be `0` and compare equal. Waiting for a
/// fold is what carries a read made within ~2s of the first fold past it. The cost: against a node
/// that has not folded, each order-path read still sits out the whole ~2s (MEASURED on the latency box: 4.03
/// s per preview and per confirm, two reads each).
///
/// ⚠ **BOTH order-path reads come through here, and each is held on its own.** A write that names a
/// venue reads `venues[]` first, on the same handle, so its epoch read always finds a frame that
/// is already past the fold — which is why moving [`Server::node_accounts_epoch`] alone onto the
/// display wait reddened nothing until a VENUE-LESS preview was pinned
/// (`a_venue_less_preview_against_a_node_that_has_not_folded_stamps_the_post_fold_epoch` in the
/// node-drop suite), the one shape whose first read of the frame is the epoch's.
///
/// ⚠ An option for the owner, NOT built: the order path could take the fast wait AND refuse a
/// write outright while the frame is pre-fold (`seq: 0`). That would close the residual for the
/// whole window instead of leaving it at ~2s, at the price of refusing every write to a node until
/// it first folds — which on an idle paper node, whose first fold is the first write, is never.
fn wait_for_a_fold(observe: &RemoteCoreHandle) -> Arc<WireSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut snap = observe.snapshot();
    while snap.seq == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        snap = observe.snapshot();
    }
    snap
}

/// What a read tool says beside the NODE's own pre-fold placeholder: a frame the node really
/// published (stamped with its identity) before its first fold — see [`node_read_answer`].
const PRE_FOLD_NOTE_NODE: &str = concat!(
    "pre_fold: the node was reached, and this is the placeholder frame it publishes BEFORE its ",
    "first fold (seq 0, stamped with its identity): it has built nothing yet. venues, balance, ",
    "equity_total and accounts_epoch are placeholders, not readings: venues: [] does NOT mean ",
    "nothing is mounted, and orders: [] is only this node's own book, not the venue's. Call ",
    "node_snapshot again."
);

/// What a read tool says beside THIS CLIENT's own placeholder (`WireSnapshot::empty()`): the
/// connection is open, but no frame recognisable as the node's arrived before the read's deadline
/// — see [`node_read_answer`].
const PRE_FOLD_NOTE_CLIENT: &str = concat!(
    "pre_fold: the connection to the node is open, but no frame from it was recognised before ",
    "this read's deadline, so this is this client's OWN empty placeholder (identity: null, ",
    "seq 0), not a frame the node published. (A node too old to stamp an identity sends a ",
    "pre-fold frame that looks the same on those two fields.) Nothing in it is a reading: ",
    "venues: [] does NOT mean nothing is mounted, and orders: [] says nothing about any book. ",
    "Call node_snapshot again."
);

/// What a READ tool answers for `snap`: the `WireSnapshot` serialized as-is, plus `pre_fold` —
/// `true` when the frame carries nothing built ([`crate::cmd::trade::is_pre_fold`]: `seq: 0`) —
/// and, when it is, a `pre_fold_note` saying WHICH placeholder it is, because the two are
/// different facts: [`PRE_FOLD_NOTE_NODE`] for the frame the node publishes before its first fold
/// (stamped with its identity — [`crate::cmd::trade::is_node_frame`]), [`PRE_FOLD_NOTE_CLIENT`]
/// for this client's own `WireSnapshot::empty()`, answered when no node frame arrived in time.
///
/// ⚠ The marker exists because the faster read answers the node's placeholder at once where the
/// old one returned it only after ~2s, and an agent verifying a mount right after a node starts
/// would read its `venues: []` as "nothing is mounted". `pre_fold` is always present, so an agent
/// can rely on `false` too. The ORDER path reads the frame unmarked
/// ([`Server::order_gate_snapshot`]).
///
/// ⚠ **There was ONE note until 2026-10-04, and it was false of the client's case**: it called
/// every `seq: 0` frame "the placeholder a node publishes before its first fold", while
/// `identity: null` means nothing from the node was in hand at all. `pre_fold` itself did not
/// change: it is `true` on both.
pub(super) fn node_read_answer(snap: &WireSnapshot) -> Result<Value, String> {
    let mut answer =
        serde_json::to_value(snap).map_err(|e| format!("cannot serialize snapshot: {e}"))?;
    let pre_fold = crate::cmd::trade::is_pre_fold(snap);
    if let Some(fields) = answer.as_object_mut() {
        fields.insert("pre_fold".to_string(), Value::Bool(pre_fold));
        if pre_fold {
            let note = if crate::cmd::trade::is_node_frame(snap) {
                PRE_FOLD_NOTE_NODE
            } else {
                PRE_FOLD_NOTE_CLIENT
            };
            fields.insert("pre_fold_note".to_string(), Value::String(note.to_string()));
        }
    }
    Ok(answer)
}

/// What a per-call node READ failure says to an agent — the tool-shaped twin of
/// `crate::cmd::trade::status`'s `failure_lines`, which writes the same three diagnoses for a
/// human at a terminal.
///
/// PURE, and it exists because two failures out of the three are CONFIGURATION facts about a
/// reachable box rather than transport trouble, and an agent that cannot tell them apart retries
/// the one thing that can never succeed:
///
/// * `Unsupported` — the client refused before sending, because the node's `Welcome.features` does
///   not advertise this verb's capability. Nothing went on the wire; the fix is on the NODE and
///   the message says so, because "unsupported" alone reads as a bug in the request.
/// * `PermissionDenied` — the handshake itself was refused: the observe key presented does not
///   verify against that node's. Points at the key, not the verb.
/// * anything else — an honest transport-shaped report naming the address, which is the one an
///   agent may sensibly try again.
///
/// ⚠ The client's own sentence is KEPT in every arm rather than replaced: it names the capability
/// string, which is the thing an operator greps for.
pub(super) fn node_read_failure(tool: &str, addr: &str, err: &io::Error) -> String {
    match err.kind() {
        io::ErrorKind::Unsupported => format!(
            "{tool}: {err}. This is a fact about the NODE, not about your request — the operator \
             has to upgrade the vike-tradehub at {addr} to a build that serves this verb. Calling \
             it again against the same node cannot succeed."
        ),
        io::ErrorKind::PermissionDenied => format!(
            "{tool}: the node at {addr} refused the observe handshake: {err}. The presented \
             {observe} does not match that node's — `vike-cli secrets path` prints the store this \
             side read it from.",
            observe = nodekeys::OBSERVE_KEY_ENV
        ),
        _ => format!("{tool}: cannot query the node at {addr}: {err}"),
    }
}
