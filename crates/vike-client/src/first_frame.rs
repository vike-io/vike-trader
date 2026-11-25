//! The one wait a read that only DISPLAYS a subscribed frame takes, the two questions about a
//! frame it turns on, and how old a frame may be before a door says so ([`stale_frame_warning`]).
//! Shared so no two surfaces can disagree about when a read may answer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_tradehub_client::wire::CoreSnapshotWire;
use vike_tradehub_client::{FrameAge, TradehubCoreHandle};

/// How long a display read waits for a frame the node pushed before it answers whatever is in hand
/// — see [`wait_for_first_frame`].
const FIRST_FRAME_WAIT: Duration = Duration::from_secs(2);

/// THE DISPLAY READ'S WAIT: until `observe` holds a frame the NODE pushed ([`is_node_frame`]), the
/// handle dies, or `FIRST_FRAME_WAIT` passes — then whatever is in hand, which is the client's
/// own placeholder only if no node frame arrived in time. The node hands a new subscriber its
/// current frame at once, so against a reachable node this ends within a round trip. `alive` is
/// read BEFORE the cell, so a death seen here is ordered after every frame the receive thread
/// stored: the frame a caller then judges is that handle's last.
///
/// ONE wait for every read that only DISPLAYS a subscribed frame, so no two surfaces can disagree
/// about when a read may answer: `vike-cli`'s `with_snapshot` (the REPL's reads, over a
/// session-lifetime connection), its `connect_observe` (the one-shot `trade order ls` and
/// `trade position ls`) and the `mcp` server's `node_snapshot` tool and `vike://node/snapshot`
/// resource (`Server::node_frame` in `crates/vike-mcp/src/node_reads.rs`).
///
/// ⚠ **Not for a read that JUDGES a write.** A frame this answers may be the node's pre-fold
/// placeholder ([`is_pre_fold`]): `venues: []` and `accounts_epoch: 0`, neither a reading. The
/// `mcp` order path — its venue gate and the preview's epoch stamp — keeps its own wait for a FOLD
/// (`wait_for_a_fold`, beside `Server::node_frame`, whose doc says why the latency is paid there on
/// purpose).
/// The REPL's write path waits here for the frame the one order gate judges (`crate::order_gate::judge`),
/// so a session's first line is judged on the node's frame, not the client's placeholder; a pre-fold
/// frame shows no engine block, which the gate reads as no evidence.
///
/// ⚠ **`seq != 0` was this wait's condition until 2026-10-04**, and for a display read it was a
/// latency defect, the same one `node_snapshot` had: a node that has not folded never leaves
/// `seq: 0`, so against an idle node EVERY REPL read — not just a session's first — and every
/// one-shot `ls` sat out the whole deadline and then printed the frame that had been in hand since
/// the first round trip.
pub fn wait_for_first_frame(observe: &TradehubCoreHandle) -> Arc<CoreSnapshotWire> {
    let deadline = Instant::now() + FIRST_FRAME_WAIT;
    loop {
        let alive = observe.is_connected();
        let snap = observe.snapshot();
        if is_node_frame(&snap) || !alive || Instant::now() >= deadline {
            return snap;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether `snap` is a frame the NODE pushed, rather than the `CoreSnapshotWire::empty()` placeholder a
/// freshly opened [`TradehubCoreHandle`] holds until the node's first frame lands — the condition
/// [`wait_for_first_frame`] ends on.
///
/// Two pieces of evidence, each SUFFICIENT and neither NECESSARY, because the client's placeholder
/// carries neither (`identity: None`, `seq: 0`):
///
/// - **`identity`** — the node's publisher stamps its identity block into EVERY frame it pushes,
///   including the one it publishes before its first fold, and the shipped daemon always has one
///   (`crates/vike-tradehub/src/hub_server.rs`'s `spawn_with_mounts` call). This is the arm that
///   answers a node that has not folded.
/// - **`seq > 0`** — only a fold moves it. This is the arm that answers a node that stamps no
///   identity block (one that predates it) once that node has folded.
///
/// ⚠ **A frame that satisfies this is NOT necessarily a reading.** The stamped `seq: 0` frame is the
/// node's OWN placeholder — `vike_execution::CoreSnapshot::empty`, "published before the first real
/// build" — carrying `venues: []`, zero balance and equity, and `accounts_epoch: 0`, each of which
/// vike-core means as "has not said yet". So every display read MARKS it ([`is_pre_fold`]) instead
/// of presenting it as the node's state, and the `mcp` order path does not use this condition at all.
///
/// ⚠ **What neither arm can see: a node that stamps no identity and has never folded.** Its one
/// frame carries neither piece of evidence — on the two fields read here it is the client's
/// placeholder — so against it a read still runs to its deadline, which is what it always did.
pub fn is_node_frame(snap: &CoreSnapshotWire) -> bool {
    snap.identity.is_some() || snap.sequence > 0
}

/// Whether `snap` carries nothing BUILT yet: `seq: 0`, which only two frames carry — the node's own
/// pre-fold placeholder (stamped with its identity) and this client's own `CoreSnapshotWire::empty()`
/// (no identity). [`is_node_frame`] tells the two apart. A display read MARKS such a frame rather
/// than rendering its zeros and empty lists as the node's state — the `mcp` read tools as
/// `pre_fold` (`node_read_answer` in `crates/vike-mcp/src/node_reads.rs`), the REPL and the
/// one-shot `ls` verbs with `vike-cli`'s `pre_fold_line` and the `stamp` on every table header.
pub fn is_pre_fold(snap: &CoreSnapshotWire) -> bool {
    snap.sequence == 0
}

/// Past this a frame is old: a frame through a healthy link is well under it on a node that is trading.
pub const FRAME_AGE_BOUND_MS: i64 = 5_000;

/// What a door says of a frame `age` old before anything is read or decided off it, or `None` for one at most
/// [`FRAME_AGE_BOUND_MS`] old. An agent took orders for resting off a frame 45 s behind the node
/// (`vike_tradehub_client::proto::FEATURE_OBSERVE_NEWEST`), and a frame does not look old. A WARNING, never a
/// refusal: a quiet node pushes nothing newer, and its frame is still its state. Both doors say it: the MCP server
/// on every node read and preview (`frame_stale`), the CLI in a write's preview (the REPL's `run_write`, the
/// one-shot `execute_write`). A frame with no stamp has no age (`FrameAge::of`), so nothing is said of it. PURE.
pub fn stale_frame_warning(age: FrameAge) -> Option<String> {
    if age.age_ms <= FRAME_AGE_BOUND_MS {
        return None;
    }
    let why = if age.arrived_after_ms > FRAME_AGE_BOUND_MS {
        "it was already that old when it arrived, so the link to the node is behind"
    } else {
        "it arrived on time and the node has pushed nothing newer, which a quiet node does and a \
         busy one does not"
    };
    Some(format!(
        "this reads a frame {} ms old, past the {FRAME_AGE_BOUND_MS} ms a frame counts as current: {why}. \
         Orders, positions and the tier here may have moved since. Read again before acting on them.",
        age.age_ms
    ))
}
