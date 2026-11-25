//! The node READ tools — `node_snapshot` (the pushed book), `strategy_status`, `settings_show`,
//! `health_node` and `report` (the per-call reads; `report` reads the datahub too) — and the observe
//! connection they share.
//!
//! The order path reads the same frame through `Server::order_gate_snapshot`, one gate with two
//! first-frame waits (`FirstFrameWait`), so everything that opens, waits on or lets go of the
//! observe handle lives here. Split out of `src/lib.rs` (code-layout phase 2, task 9); the module
//! doc there ("When the node connection drops") is the argument for the reconnect-on-next-call
//! design this file implements.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_tradehub_client::TradehubCoreHandle;
use vike_tradehub_client::wire::CoreSnapshotWire;

use super::tool_args::{ReportArgs, parse_args};
use super::*;
use vike_client::account_report;
use vike_client::health::{DatahubAnswer, HubLeg, ask_datahub, document, judge, read_node};
use vike_client::hubkeys;
use vike_hub_protocol::access::Signer;
use vike_hub_protocol::call::HubCallError;

impl Server {
    /// Read the live node snapshot (orders / positions / equity / recent events) via the observe
    /// connection. Opens it lazily, then waits briefly for the node's first pushed frame — and
    /// answers ONLY from a connection that is still delivering frames.
    ///
    /// ⚠ **A read must never lie, and this used to.** `TradehubCoreHandle::snapshot` is a load off
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
    /// the connection ([`vike_client::first_frame::wait_for_first_frame`], the one display-read wait the
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
    /// though the node had said "nothing". The answer says how old the frame is ([`stamp_frame_age`]).
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
    /// `vike_tradehub_client::liveness::OBSERVE_READ_TIMEOUT` (or the wider window this box's
    /// `preferences.observe_stall_timeout_ms` row asked for) and the second pass reconnects.
    /// The residuals, stated so the green is read at its width: that window is 45 s by default, and the deadline
    /// is armed only against a node advertising the capability (a mixed-version deployment gets the
    /// old width, silently). This remains a LIVENESS gate: FRESHNESS is what the answer's
    /// `frame_age_ms` and `frame_stale` tell the caller, and an agent decides.
    pub(super) fn tool_node_snapshot(&mut self) -> Result<Value, String> {
        let snap = self.node_frame(FirstFrameWait::NodeFrame)?;
        let mut answer = node_read_answer(&snap)?;
        stamp_frame_age(&mut answer, self.frame_age());
        Ok(answer)
    }

    /// How old the frame this session's observe connection holds is: `None` with no connection, or for a frame with no
    /// stamp (a node that predates it).
    pub(super) fn frame_age(&self) -> Option<vike_tradehub_client::FrameAge> {
        self.observe.as_ref().and_then(TradehubCoreHandle::frame_age)
    }

    /// The node's frame as the ORDER path reads it — the venue gate ([`Server::vet_commanded_venue`])
    /// and the confirm's account-epoch check ([`Server::node_accounts_epoch`]) — after main's wait
    /// for a FOLD ([`wait_for_a_fold`]), serialized as-is with no read-tool annotation: what these
    /// two read is byte-for-byte what they read before the read tools got a faster wait.
    pub(super) fn order_gate_snapshot(&mut self) -> Result<Value, String> {
        let snap = self.order_gate_frame()?;
        serde_json::to_value(&*snap).map_err(|e| format!("cannot serialize snapshot: {e}"))
    }

    /// The same frame, typed, or the sentence saying why there is none: what a preview stamps its
    /// account epoch from and reads the addressed engine's tier and contract value off, in ONE read so
    /// a preview waits out no more folds than it did before it carried them.
    pub(super) fn order_gate_frame(&mut self) -> Result<Arc<CoreSnapshotWire>, String> {
        self.node_frame(FirstFrameWait::Fold)
    }

    /// The two-pass, one-gate read [`Server::tool_node_snapshot`]'s doc describes, with the
    /// first-frame wait as its one parameter. Answers ONLY a frame from a handle that is still
    /// alive after the wait; a dead one is let go of and one fresh connection is tried.
    fn node_frame(&mut self, wait: FirstFrameWait) -> Result<Arc<CoreSnapshotWire>, String> {
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
                FirstFrameWait::NodeFrame => {
                    vike_client::first_frame::wait_for_first_frame(observe)
                }
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
            if snap.sequence > 0 {
                stale_seq = Some(snap.sequence);
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
    /// shortcut.** The held observe handle is a SUBSCRIBED push pipe — it delivers `CoreSnapshotWire`
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
    /// arrives here as `HubCallError::NotOffered` and [`node_read_failure`] adds the one action
    /// that fixes it.
    pub(super) fn tool_strategy_status(&self) -> Result<Value, String> {
        let (addr, key) = self.observe_read_target()?;
        match vike_tradehub_client::strategy_status(addr.as_str(), key) {
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
    /// ⚠ Nothing here redacts anything, and nothing here needs to: `SettingsShowRowWire`'s values
    /// arrive already redacted, because the node performs it in the shared builder ON
    /// CONSTRUCTION. Re-applying a rule this crate would have to keep in step with the node's is
    /// the second-authority failure, not a belt.
    pub(super) fn tool_settings_show(&self) -> Result<Value, String> {
        let (addr, key) = self.observe_read_target()?;
        match vike_tradehub_client::settings_show(addr.as_str(), key) {
            Ok(show) => serde_json::to_value(show)
                .map_err(|e| format!("settings_show: the node's answer did not serialize: {e}")),
            Err(e) => Err(node_read_failure("settings_show", &addr, &e)),
        }
    }

    /// Judge the node: `vike-cli trade health --json`'s document, by the same functions
    /// (`crates/vike-client/src/health.rs`), plus what the datahub says of the gatewayhub.
    ///
    /// ⚠ **A dead tradehub is a RESULT here, not a tool error**, because the verdict is the product:
    /// the first row says it does not answer and `verdict` is FAIL. What IS an error is a question this
    /// box cannot ask at all (no `--node`, no observe key). The read is bounded and inline, like the
    /// other per-call reads: a black-holed link costs the connect timeouts and the datahub's reply wait.
    pub(super) fn tool_health_node(&self) -> Result<Value, String> {
        let (addr, key) = self.observe_read_target()?;
        let mut reads = read_node(&addr, key);
        let answer = ask_datahub(&self.datahub_addr, self.datahub_keys.as_ref());
        let leg = HubLeg { addr: self.datahub_addr.clone(), answer };
        let gatewayhub = gatewayhub_block(&leg);
        reads.hubs = Some(leg);
        let mut doc = document(&addr, &judge(&addr, &reads));
        doc["gatewayhub"] = gatewayhub;
        Ok(doc)
    }

    /// A LIVE account's report: its own fills out of the datahub (`--addr`), its tier and fold guards from
    /// the node's snapshot, folded as `vike-cli report account:<venue>/<LABEL> --json` folds them
    /// ([`vike_client::account_report::read_live_side`], the one read both doors call), and answered as
    /// that command's document. A stored run's report is `vike-cli report`'s and stays out of this server.
    ///
    /// The arguments are refused by name BEFORE anything is dialled, and a box with no node or no observe
    /// key is told so ([`Server::observe_read_target`]) before the account is asked for. Every refusal of
    /// the read itself (an account the node names nowhere, a datahub that records no live executions, an
    /// opening position nothing can read) comes back as an error in its own words, never as an empty sheet,
    /// which would report a flat account to somebody whose account is not flat.
    pub(super) fn tool_report(&self, args: &Value) -> Result<Value, String> {
        let args: ReportArgs = parse_args(args, report_refusal)?;
        let (cash, periods_per_year) = (args.cash, args.periods_per_year);
        let (from, to) = (args.from, args.to);
        if let (Some(from), Some(to)) = (from, to)
            && from > to
        {
            return Err(format!(
                "report: `from` ({from}) is after `to` ({to}): the window would hold nothing, which reads \
                 exactly like an account that never traded"
            ));
        }
        let (addr, _) = self.observe_read_target()?;
        let venue = args.venue.ok_or(
            "report: name the account: `venue` (node_snapshot's venues[].venue) and, for a labelled \
             account, `account`",
        )?;
        let label = match args.account.as_deref() {
            None => vike_model::accounts::account_keys::AccountLabel::default(),
            Some(word) => vike_model::accounts::account_keys::parse_wire_account(word)
                .map_err(|e| format!("report: `account` {word:?} is no account label: {e}"))?,
        };
        let query = account_report::LiveQuery {
            account: account_report::account_ref(&venue, label)
                .map_err(|e| format!("report: {e}"))?,
            window: vike_data::TimestampRange { start: from, end: to },
            mount_id: None,
            title: None,
        };
        let fold = account_report::LiveFold {
            cash: cash.unwrap_or(vike_analytics::results::DEFAULT_REPORT_SEED),
            cash_from: if cash.is_some() { "`cash`" } else { "the default; `cash` sets it" }
                .to_string(),
            periods_per_year,
            mark_to_market: None,
            without_foreign: false,
        };
        // Answered inline on the loop's thread, so every read of it is bounded by `HUB_READ_WAIT`, as
        // `list_series`'s is: a silent datahub must not hold the whole server for the client's five minutes.
        let store = match self.datahub_keys.clone() {
            Some(keys) => {
                vike_datahub_client::HistoryStoreDatahub::with_keys(&self.datahub_addr, keys)
            }
            None => vike_datahub_client::HistoryStoreDatahub::new(&self.datahub_addr),
        }
        .with_reply_timeout(HUB_READ_WAIT);
        let side = account_report::read_live_side(
            &addr,
            &self.keys,
            &store,
            &self.datahub_addr,
            &query,
            &fold,
            vike_model::now_ms(),
        )
        .map_err(|e| format!("report: {e}"))?;
        serde_json::to_value(&side.sheet)
            .map_err(|e| format!("report: the results did not serialize: {e}"))
    }

    /// The address and OBSERVE key a per-call read verb needs, or the message saying which of the
    /// two is missing. Shared by the per-call reads so they cannot answer differently about a
    /// box that is configured the same way for all of them.
    pub(super) fn observe_read_target(&self) -> Result<(String, Signer<'_>), String> {
        let addr = self.node_addr.clone().ok_or_else(missing_node)?;
        let (key, _) = self.keys.observe().ok_or_else(|| self.missing_key())?;
        Ok((addr, key))
    }

    /// The "nothing to sign with" tool error: this box holds no device key (decision 0288 item 8), in the one body
    /// every door answers (`vike_client::hubkeys::HubKeyring::absent_refusal`), its cure following the node this
    /// server dials.
    ///
    /// ⚠ **With no settings directory resolved there is no store to name**, and "not in the store"
    /// sent an agent to look for a key that may well be there: a server launched outside the project
    /// folder, over ssh above all, read nothing, and the true reason went only to stderr, which an
    /// agent does not see. The body says which.
    pub(super) fn missing_key(&self) -> String {
        self.keys.absent_refusal(self.node_addr.as_deref()).rendered()
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
        let addr = self.node_addr.as_ref().ok_or_else(missing_node)?;
        let (key, _) = self.keys.observe().ok_or_else(|| self.missing_key())?;
        let handle = TradehubCoreHandle::connect_with_read_timeout(
            addr.as_str(),
            key,
            self.keys.patience().observe_stall,
        )
        .map_err(|e| format!("cannot open observe connection to {addr}: {e}"))?;
        self.observe = Some(handle);
        Ok(())
    }
}

/// The "no node to dial" tool error, every node tool's: no `--node` and no `config.node_addr` row, in the one body
/// `vike-cli trade` refuses with too (`vike_client::resolved::node_dial_address`), naming `vike-cli backend connect`.
pub(super) fn missing_node() -> String {
    vike_client::refusals::NodeAddressRefusal::Unset.rendered()
}

/// Which first-frame wait a node read uses — the ONE thing the read tools and the order path do
/// differently with the shared two-pass gate ([`Server::node_frame`]).
#[derive(Clone, Copy)]
enum FirstFrameWait {
    /// The read tools (`node_snapshot` and its resource): the handle's first frame FROM THE NODE
    /// (`vike_client::first_frame::is_node_frame`), the handle's death, or ~2s —
    /// [`vike_client::first_frame::wait_for_first_frame`], the ONE display-read wait, which the `trade`
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
/// reads JUDGE a write. The venue gate (`vike_client::order_gate::judge` over [`Server::vet_commanded_venue`]'s read)
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
fn wait_for_a_fold(observe: &TradehubCoreHandle) -> Arc<CoreSnapshotWire> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut snap = observe.snapshot();
    while snap.sequence == 0 && Instant::now() < deadline {
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

/// What a read tool says beside THIS CLIENT's own placeholder (`CoreSnapshotWire::empty()`): the
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

/// What a READ tool answers for `snap`: the `CoreSnapshotWire` serialized as-is, less its `bars`, plus
/// `pre_fold` — `true` when the frame carries nothing built ([`vike_client::first_frame::is_pre_fold`]: `seq: 0`) —
/// and, when it is, a `pre_fold_note` saying WHICH placeholder it is, because the two are
/// different facts: [`PRE_FOLD_NOTE_NODE`] for the frame the node publishes before its first fold
/// (stamped with its identity — [`vike_client::first_frame::is_node_frame`]), [`PRE_FOLD_NOTE_CLIENT`]
/// for this client's own `CoreSnapshotWire::empty()`, answered when no node frame arrived in time.
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
pub(super) fn node_read_answer(snap: &CoreSnapshotWire) -> Result<Value, String> {
    let mut answer =
        serde_json::to_value(snap).map_err(|e| format!("cannot serialize snapshot: {e}"))?;
    let pre_fold = vike_client::first_frame::is_pre_fold(snap);
    if let Some(fields) = answer.as_object_mut() {
        // One real answer measured 81.6k characters, 47k of them this field, and a client stops
        // showing a text result past 50,000. No skill or prompt reads it, and the wire keeps it.
        fields.remove("bars");
        fields.insert("pre_fold".to_string(), Value::Bool(pre_fold));
        if pre_fold {
            let note = if vike_client::first_frame::is_node_frame(snap) {
                PRE_FOLD_NOTE_NODE
            } else {
                PRE_FOLD_NOTE_CLIENT
            };
            fields.insert("pre_fold_note".to_string(), Value::String(note.to_string()));
        }
    }
    Ok(answer)
}

/// Say how old the frame behind `answer` is, and past the bound what that means, in the words both doors say it with
/// (`vike_client::first_frame::stale_frame_warning`). Says nothing for a frame with no stamp: inventing an age from
/// this call's clock would be a number about this process.
pub(super) fn stamp_frame_age(answer: &mut Value, age: Option<vike_tradehub_client::FrameAge>) {
    let (Some(age), Some(fields)) = (age, answer.as_object_mut()) else {
        return;
    };
    fields.insert("frame_age_ms".to_string(), json!(age.age_ms));
    fields.insert("frame_arrived_after_ms".to_string(), json!(age.arrived_after_ms));
    if let Some(warning) = vike_client::first_frame::stale_frame_warning(age) {
        fields.insert("frame_stale".to_string(), json!(warning));
    }
}

/// `report`'s refusal of an argument of the wrong type ([`parse_args`]), by name and never dropped: a
/// silently-lost `cash` hands back ratios the agent believes are scaled to its account, a dropped bound
/// answers about the account's whole history as though it were the window asked for, and a dropped
/// `account` reads the venue's unlabelled account, which is a different book.
fn report_refusal(name: &str, value: &Value) -> String {
    match name {
        "cash" | "periods_per_year" => format!("report: `{name}` must be a number, got {value}"),
        "from" | "to" => {
            format!("report: `{name}` must be an instant in epoch-ms, a whole number, got {value}")
        }
        // `venue` and `account`, the two words.
        _ => format!("report: `{name}` must be a string, got {value}"),
    }
}

/// `health_node`'s `gatewayhub`: what the datahub said, by the hop it came through. The rows in
/// `checks[]` judge it; this is the same answer as numbers. A datahub that could not be asked is
/// `reached: false` with the reason, which claims nothing about the gatewayhub past that hop. The
/// daemon's budget rows stay out (`vike_datahub_client::gateway_status`'s module doc: beside a daemon's
/// health they would read as headroom).
pub(super) fn gatewayhub_block(leg: &HubLeg) -> Value {
    let addr = &leg.addr;
    let (reached, error, report) = match &leg.answer {
        DatahubAnswer::Said(reply) => (reply.reached, reply.error.clone(), reply.report.as_ref()),
        DatahubAnswer::NoAnswer(why) => {
            (false, Some(format!("the datahub at {addr} does not answer: {why}")), None)
        }
        DatahubAnswer::Refused(why) => {
            (false, Some(format!("the datahub at {addr} refused this box: {why}")), None)
        }
        DatahubAnswer::TooOld => {
            (false, Some(format!("the datahub at {addr} is too old to say")), None)
        }
    };
    json!({
        "via": "datahub",
        "reached": reached,
        "error": error,
        "uptime_ms": report.map(|r| r.uptime_ms),
        "epoch": report.map(|r| r.epoch),
        "connections": report.map(|r| r.connections),
        "stream_sessions": report.map(|r| r.stream_sessions),
        "fetches_running": report.map(|r| r.fetches_running),
        "market_data_venues": report.map(|r| &r.market_data_venues),
        "credentialed_data_venues": report.map(|r| &r.credentialed_data_venues),
    })
}

/// What a per-call node READ failure says to an agent: the one sort and wording both doors share
/// (`vike_client::hubkeys::node_read_failure_lines`, whose doc says why an agent must tell an old node and a
/// refused key from a socket), its lines as one answer.
pub(super) fn node_read_failure(tool: &str, addr: &str, err: &HubCallError) -> String {
    hubkeys::node_read_failure_lines(tool, addr, "this verb", err).join("\n")
}
