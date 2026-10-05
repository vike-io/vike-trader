//! The node WRITE tools — every member of `WRITE_TOOLS` that is a node command (the order verbs and
//! the node-lifecycle three): the transport that sends a confirmed command, the node's own dry-run
//! behind the preview, the preview payload and the `--unattended` refusal.
//!
//! The reconnect-on-never-sent design these methods implement is argued on `Server::execute`'s doc
//! and in the module doc of `cmd/mcp.rs` ("When the node connection drops"). The venue gate that
//! runs before a preview is minted lives beside this file in `venue_gate`. Split out of
//! `cmd/mcp.rs` (code-layout phase 2, task 9).

use serde_json::{Value, json};
use vike_tradehub_client::wire::WireCommand;
use vike_tradehub_client::{CommandOutcome, ControlRejected, RemoteControlHandle};

use super::node_reads::node_read_failure;
use super::*;
use crate::cmd::nodekeys;
use crate::cmd::verbs;

impl Server {
    /// The `old → new` a `set_setting` preview carries — `None` for every other command.
    ///
    /// The old value is the node's own: one [`vike_tradehub_client::settings_show`] read, the same
    /// per-call shape and observe key [`Server::tool_settings_show`] uses, handed to the PURE
    /// [`verbs::SettingChange::from_show`] the `trade` REPL builds its line with. A read that fails
    /// — no node, no observe key, an older node — is not a refusal: the preview still goes out,
    /// with `old` null and `old_unread` saying why, because the write is still the caller's to
    /// confirm against what is known.
    pub(super) fn setting_change(&self, cmd: &WireCommand) -> Option<verbs::SettingChange> {
        let WireCommand::SetSetting { key, value, .. } = cmd else { return None };
        let show = self.observe_read_target().and_then(|(addr, observe_key)| {
            vike_tradehub_client::settings_show(addr.as_str(), observe_key.as_bytes())
                .map_err(|e| node_read_failure("settings_show", &addr, &e))
        });
        Some(verbs::SettingChange::from_show(key, value, show.as_ref().map_err(Clone::clone)))
    }

    /// Send an already-built [`WireCommand`] to the node's control connection (opened lazily) and
    /// report what the node did with THAT command — [`RemoteControlHandle::await_outcome`] on the
    /// ticket the send returned. The order's own progress (fills) is still observed via
    /// `node_snapshot`; this answers only "did the node accept it".
    /// `reason` is the optional agent rationale, carried BESIDE the command for the node's audit
    /// trail (sanitized server-side) — it is never folded into the order.
    ///
    /// ⚠ This used to poll `RemoteControlHandle::last_error`, a LATCH that is never cleared: after
    /// one refusal every later write tool in the session returned `isError: node rejected the
    /// command`, including commands the node accepted and EXECUTED. An agent reading that would
    /// reasonably RETRY — placing the order twice.
    ///
    /// # A dropped connection is let go of HERE, and only here — and a NEVER-SENT one is re-sent
    ///
    /// Three outcomes mean the control connection is dead, and they split TWO ways, which is the
    /// whole design:
    ///
    /// - **Never sent** — [`CommandOutcome::NeverSent`] (the sender found the link already closed
    ///   and wrote nothing) and [`ControlRejected::Gone`] (the worker had already exited, so
    ///   nothing was even enqueued). This call drops the handle, dials ONCE through the same
    ///   [`Server::ensure_control`] that opened the first connection, and SENDS THIS COMMAND on the
    ///   fresh one — reporting whatever the node then says. There is no double-execution risk to
    ///   weigh: not one byte of it reached the node, so there was no first execution to double. If
    ///   the reconnect fails, the answer is the connect error wrapped in a "not sent" that says so
    ///   and the handle is cleared, exactly as before; a second never-sent on the fresh link is
    ///   reported and not chased, because a third dial inside one tool call is a retry loop an
    ///   agent is waiting on.
    /// - **Unknown** — [`CommandOutcome::Disconnected`]: the command WAS written and its reply was
    ///   lost. The handle is dropped so the next call reconnects, and this call returns the same
    ///   UNKNOWN error, word for word, that it always has. It is never resent under any wording.
    ///
    /// ⚠ **This reverses half of what this doc said at M11, and the reversal is worth reading.**
    /// The old paragraph refused to re-send even a provably-unsent command, arguing that "the
    /// property that matters is not 'was this byte sent twice' but 'what picture is the agent
    /// writing against': a write issued straight after a drop is a write against a snapshot from
    /// BEFORE the drop". That argument was correct about the RISK and wrong about the remedy here,
    /// for a reason M11 could not see because the distinction did not exist yet:
    ///
    /// 1. It could not tell a never-sent command from an unknown one, so it had to treat every
    ///    dead link as the dangerous case. `vike-tradehub-client` now makes that distinction
    ///    STRUCTURALLY (a peer FIN observed BEFORE the write), and it is conservative: an ambiguous
    ///    failed write stays `Disconnected`.
    /// 2. The picture is not stale in this path. A write tool here reaches `execute` only after a
    ///    MANDATORY node-verified preview (`Server::call_tool`'s binding token check), and a
    ///    preview opens its own fresh connection per call — so the node dry-ran THIS command,
    ///    seconds ago, on a live link. And the node re-evaluates every command on execute
    ///    (`ControlLimits` + the core `RiskGate`) whichever connection it arrives on.
    /// 3. What the old behaviour cost was not caution but a dead end: the agent was handed an
    ///    error for a command that demonstrably had not happened, and the only way forward was a
    ///    `node_snapshot` round trip and a fresh preview+confirm — three tool calls to re-send a
    ///    command nothing had objected to.
    ///
    /// # The routine trigger USED to be the node's own idle timer
    ///
    /// `crates/vike-tradehub/src/server.rs`'s `HANDSHAKE_READ_TIMEOUT` (five minutes) was set on
    /// every accepted socket and never replaced, and a CONTROL peer never subscribes — it stays in
    /// the node's request/response read loop — so the node closed it the moment it had been quiet
    /// that long, and this arm reported `Disconnected` ("may have executed") for a command the
    /// node's thread had stopped reading before it was offered. Every pause longer than five
    /// minutes produced that transcript. BOTH halves of it are now fixed, in the two crates that
    /// owned them: the node replaces that bound at `AuthOk`
    /// (`vike_tradehub_client::liveness::AUTHED_IDLE_TIMEOUT`), so the close does not happen at
    /// all; and when a close does happen for some other reason (a daemon restart, a tunnel blip),
    /// the sender's pre-write probe reports it as never-sent and this call recovers on the spot.
    ///
    /// # Preview tokens survive the drop, and that is right
    ///
    /// [`PendingPreviews`] is per-PROCESS, not per-connection, and a reconnect leaves it alone: a
    /// token minted before the drop still confirms after it, as long as it is within
    /// [`PREVIEW_WINDOW`]. That is correct because of what a token PROVES — that a preview happened
    /// for this exact command in this session (the binding compare in [`Server::call_tool`]) — and
    /// what it does not: the node's dry-run verdict was never a reservation, and the node
    /// re-evaluates every command on execute (`ControlLimits` and the core `RiskGate`) whichever
    /// connection it arrives on. The window is what bounds a preview taken against a book that has
    /// since moved, and a drop does not make the book move any faster than the clock does.
    pub(super) fn execute(
        &mut self,
        cmd: &WireCommand,
        reason: Option<String>,
    ) -> Result<Value, String> {
        // ⚠ ONE verb leaves by a different door, and the reason is the ANSWER rather than the
        // command. Everything below is fire-and-forget over the persistent worker, whose reply
        // mapping collapses every acceptance to an empty-coid `Accepted` — which is exactly right
        // for an order and throws away the one fact a settings write exists to report, whether the
        // node applied the value LIVE or only put it on disk for the next boot.
        // `vike_tradehub_client::set_setting`'s own doc calls that out as why it is synchronous.
        // The two-call preview gate above is unaffected: this is the transport for a command that
        // has already been previewed, tokenised and confirmed.
        if matches!(cmd, WireCommand::SetSetting { .. }) {
            return self.execute_settings_write(cmd, reason.as_deref());
        }
        // PASS 1, on whatever handle is held.
        let never_sent = match self.offer_command(cmd, reason.clone()) {
            Ok(answered) => return answered,
            Err(why) => why,
        };

        // The node never saw it. Let the dead handle go and dial ONCE — the same
        // `ensure_control` that opened the first connection, so there is no second way to reach
        // the node.
        self.control = None;
        if let Err(connect_err) = self.ensure_control() {
            self.control = None;
            return Err(format!(
                "control command not sent: {never_sent} Reconnecting to send it failed, so \
                 NOTHING was sent for this call and nothing was retried: {connect_err}. The next \
                 write tool call will try to reconnect again; call node_snapshot first to see the \
                 node's real state."
            ));
        }

        // PASS 2 on the fresh connection. A second never-sent is reported, never chased: two dead
        // links in one call is a node that is going away, and a third dial would be a retry loop
        // inside a tool call an agent is waiting on.
        match self.offer_command(cmd, reason) {
            Ok(answered) => answered,
            Err(why) => {
                self.control = None;
                Err(format!(
                    "control command not sent: {why} The reconnected control connection died \
                     too, so nothing was sent for this call and nothing was retried. Call \
                     node_snapshot before trying again."
                ))
            }
        }
    }

    /// Execute a CONFIRMED `SetSetting` — the per-call, SYNCHRONOUS control verb
    /// ([`vike_tradehub_client::set_setting`]), which is the only path that carries the node's
    /// `restart_required` back.
    ///
    /// It opens a fresh short-lived `Scope::Write` connection and drops it, so none of
    /// [`Server::execute`]'s reconnect machinery applies: there is no held handle to find dead, and
    /// a failure here is a failure of THIS attempt with nothing in flight to be uncertain about.
    /// That is a real simplification and it is worth naming — the `Disconnected` case that makes an
    /// order's outcome UNKNOWN cannot arise, because the reply is read on the same call that wrote
    /// the request.
    ///
    /// The client refuses CLIENT-SIDE against a node that does not advertise the settings-write
    /// capability, and the node validates the row with its own loader before it commits. Either
    /// refusal arrives as an `io::Error` whose text is the node's or the client's own, kept
    /// verbatim. There is no confirm to enforce on either side any more (`docs/decisions/0086`
    /// point 7); the wire's field goes out `None`, and `file` is the key's own section word —
    /// both filled by [`verbs::Verb::to_wire_command`], and both kept on the wire only because the
    /// released v0.1.35 daemon still requires `file` on decode.
    fn execute_settings_write(
        &self,
        cmd: &WireCommand,
        reason: Option<&str>,
    ) -> Result<Value, String> {
        let WireCommand::SetSetting { file, key, value, .. } = cmd else {
            // Unreachable through [`Server::execute`], which matches before calling. Answered
            // rather than panicked: a tool handler's job is to answer the client.
            return Err("execute_settings_write was handed a command that is not a settings write"
                .to_string());
        };
        let addr = self.node_addr.as_ref().ok_or(
            "no vike-tradehub node configured — pass `--node <host:port>` to enable order-write tools",
        )?;
        let (control_key, _) =
            self.keys.control().ok_or_else(|| self.missing_key(nodekeys::CONTROL_KEY_ENV))?;
        match vike_tradehub_client::set_setting(
            addr.as_str(),
            control_key.as_bytes(),
            file,
            key,
            value,
            // The node has ignored the wire's `confirm` since 0086 point 7.
            None,
            reason,
        ) {
            Ok(restart_required) => Ok(json!({
                "sent": true,
                "outcome": "accepted",
                "restart_required": restart_required,
                "note": if restart_required {
                    "the node ACCEPTED and WROTE this key's row, and it applies at the next restart: the running node keeps its boot-time value until then. Every policy.* key answers this way — policy is never hot-applied. Tell the owner a restart is needed; settings_show reads the stored value back."
                } else {
                    "the node ACCEPTED this key and APPLIED IT LIVE — no restart is needed. settings_show reads back the effective value."
                }
            })),
            // ⚠ A refusal and a transport fault both land here, and both mean NOTHING WAS WRITTEN:
            // the node validates the row with its own loader before it commits, and a connection
            // that failed never delivered the request. The message is the node's or the client's
            // own, kept whole — it names the key, the loader's objection or the missing capability,
            // which is what an agent has to act on.
            Err(e) => Err(format!("settings write to {addr} was NOT applied: {e}")),
        }
    }

    /// Offer ONE command to the held control connection and resolve its ticket.
    ///
    /// `Ok(answer)` is the tool's answer for a command the node ANSWERED (or one whose outcome is
    /// unknown, which is an answer about the command). `Err(why)` is the NEVER-SENT case and only
    /// that: not one byte of this command reached the node, `why` being the sentence
    /// [`Server::execute`] embeds in whichever message it ends up returning. The split exists so
    /// the reconnect decision is made in exactly one place, over a distinction the client crate
    /// makes structurally rather than one this crate infers from an error string.
    ///
    /// The handle is dropped HERE on every dead-link finding — `Disconnected`, `NeverSent`, `Gone`
    /// — so the caller never has to remember to, and a `Refused`/timeout leaves it held.
    fn offer_command(
        &mut self,
        cmd: &WireCommand,
        reason: Option<String>,
    ) -> Result<Result<Value, String>, String> {
        // A connect failure is the TOOL's answer (M11's wording, unchanged), never a never-sent:
        // there is no held connection to let go of and nothing for a reconnect to fix — the dial
        // just failed.
        if let Err(e) = self.ensure_control() {
            return Ok(Err(e));
        }
        let addr = self.node_addr.clone().unwrap_or_default();
        let control = self.control.as_ref().expect("ensured");
        let ticket = match control.try_command_with_reason(cmd.clone(), reason) {
            Ok(ticket) => ticket,
            // The worker had already exited: the queue is closed, so NOTHING was enqueued and
            // nothing went on the wire for this call — a never-sent, and the caller may send it.
            Err(ControlRejected::Gone) => {
                self.control = None;
                return Err(format!(
                    "the control connection to {addr} had already dropped, so nothing was \
                     enqueued and nothing went on the wire."
                ));
            }
            Err(e) => return Ok(Err(control_rejection(e))),
        };
        let outcome = control.await_outcome(ticket, ACK_WAIT);
        Ok(match outcome {
            Some(CommandOutcome::Accepted { coid }) => Ok(json!({
                "sent": true,
                "outcome": "accepted",
                "client_order_id": coid,
                "note": "the node ACCEPTED this command (empty client_order_id = an account-wide verb); call node_snapshot to observe the result. The node's server-side ControlLimits + RiskGate are the enforcing gate."
            })),
            Some(CommandOutcome::Refused(err)) => Err(format!("node rejected the command: {err}")),
            // NEVER SENT: the link was already dead when the worker reached this command, so it
            // was not written at all. Hand the caller the reason rather than an answer — there is
            // nothing here for an agent to act on, and everything for this process to fix itself.
            Some(CommandOutcome::NeverSent) => {
                self.control = None;
                return Err(format!(
                    "the control connection to {addr} was already closed when this command \
                     reached the sender, so NOT ONE BYTE of it went on the wire."
                ));
            }
            // A dropped connection is NOT a refusal and NOT a success — do not let an agent infer
            // either. Say the outcome is unknown and point at the read tool that settles it. The
            // handle is discarded so the NEXT call reconnects; this command is never resent.
            Some(CommandOutcome::Disconnected) => {
                self.control = None;
                Err(format!(
                    "the control connection to {addr} dropped before the node answered this \
                     command — its outcome is UNKNOWN and it may have executed. Call node_snapshot \
                     to check BEFORE retrying. The dead connection has been discarded and the next \
                     write tool call reconnects; this command was NOT resent."
                ))
            }
            None => Ok(json!({
                "sent": true,
                "outcome": "unknown",
                "note": format!("the node did not answer within {}s — this command's outcome is NOT known and it may still execute. Call node_snapshot to check BEFORE retrying.", ACK_WAIT.as_secs())
            })),
        })
    }

    /// Open the control (write) connection if not already open. Errors if `--node` was not given or
    /// the control key is absent from the process env.
    ///
    /// "Already open" means a handle is HELD, not that it is alive: this returns early on a dead
    /// handle too, and does not probe `is_connected`. Liveness is the SENDER's finding — it is
    /// learnt from the outcome of a command that was actually offered, never from a probe before
    /// the send ([`Server::execute`]'s doc argues why a silent probe-and-reconnect was considered
    /// and refused).
    ///
    /// ⚠ This paragraph used to say [`Server::execute`] was "the ONE place that sets the field back
    /// to `None`", and that "not already open" therefore meant "found dead by the PREVIOUS write".
    /// Both halves are stale, and the second one names exactly the behaviour the never-sent
    /// recovery replaced. [`Server::offer_command`] is the place that lets a dead handle go — on
    /// `ControlRejected::Gone`, `CommandOutcome::NeverSent` and `CommandOutcome::Disconnected` —
    /// and [`Server::execute`] clears it around its two passes as well. So this is now reached
    /// TWICE per `execute` in the recovering case: once to open (or reuse) the handle for pass 1,
    /// and again for pass 2 after a never-sent finding dropped it, which is how a command the node
    /// never saw is sent on THIS call instead of the next one.
    fn ensure_control(&mut self) -> Result<(), String> {
        if self.control.is_some() {
            return Ok(());
        }
        let addr = self.node_addr.as_ref().ok_or(
            "no vike-tradehub node configured — pass `--node <host:port>` to enable order-write tools",
        )?;
        let (key, _) =
            self.keys.control().ok_or_else(|| self.missing_key(nodekeys::CONTROL_KEY_ENV))?;
        let handle = RemoteControlHandle::connect(addr.as_str(), key.as_bytes())
            .map_err(|e| format!("cannot open control connection to {addr}: {e}"))?;
        self.control = Some(handle);
        Ok(())
    }

    /// Ask the NODE what it would do with `cmd`, without sending it.
    ///
    /// ⚠ THIS IS THE ONLY VERDICT THAT MEANS ANYTHING FOR A MARKET ORDER, and market is the
    /// DEFAULT order type. The client-side [`verbs::guardrail_check`] prices an order as
    /// `price * qty`, and a market order carries no `price` — so its notional is `None` and the cap
    /// silently does not apply. The node knows the mark, holds the real `ControlLimits` and runs
    /// the same `RiskGate` a live send would hit.
    ///
    /// Returns `None` when there is no node configured, no control key, or the node could not be
    /// reached — the caller then LABELS the preview as an unverified client-side estimate rather
    /// than presenting an absent verdict as an approving one.
    pub(super) fn node_preview(&self, cmd: &WireCommand) -> Option<Value> {
        let addr = self.node_addr.as_ref()?;
        let (key, _) = self.keys.control()?;
        match vike_tradehub_client::preview_command(addr.as_str(), key.as_bytes(), cmd) {
            Ok((accepted, reason)) => Some(json!({
                "checked_by": CHECKED_BY_NODE,
                "accepted": accepted,
                "reason": reason,
            })),
            // A handshake or transport fault is NOT a verdict. Say which it was and keep the
            // preview honest about being unverified — this arm is `Some` so the preview can NAME
            // the fault it hit, and [`preview_of`] reads `checked_by` (never `is_some()`) to tell
            // it apart from a verdict.
            Err(e) => Some(json!({
                "checked_by": CHECKED_BY_NONE,
                "accepted": Value::Null,
                "reason": format!("the node could not be asked: {e}"),
            })),
        }
    }
}

/// The two `node_verdict.checked_by` values, spelled ONCE. [`Server::node_preview`] writes one of
/// them and [`preview_of`] reads it back to decide whether the preview was verified — a question
/// that must be asked of the VERDICT, never of the `Option` wrapping it.
pub(super) const CHECKED_BY_NODE: &str = "node";
/// The node was reached for, and did not answer — a transport fault, a denied handshake, or the
/// client-side refusal. See [`CHECKED_BY_NODE`].
pub(super) const CHECKED_BY_NONE: &str = "none";

/// What a rejected ENQUEUE says to an agent. [`ControlRejected::Gone`] never reaches here — it is
/// the never-sent case [`Server::offer_command`] handles by reconnecting — so this covers the two
/// that are answers.
///
/// ⚠ It used to be `{e:?}`, which printed the bare enum name. `Busy` at least suggests waiting;
/// **`UnsupportedByNode` suggested nothing at all**, and it is the arm the lifecycle verbs
/// introduced: `mount_strategy` / `unmount_strategy` are refused CLIENT-SIDE against a node whose
/// `Welcome.features` does not advertise the mount capability (an older daemon's serde cannot
/// decode the variant, so sending would produce an opaque decode error at the node instead of a
/// diagnosis here). An agent handed the word alone would read it as a transient fault and retry
/// forever against a node that can never accept it.
pub(super) fn control_rejection(e: ControlRejected) -> String {
    match e {
        ControlRejected::UnsupportedByNode => "control command not sent: this node does not \
             advertise the capability this verb requires (an older vike-tradehub) — it was REFUSED \
             CLIENT-SIDE and not one byte went on the wire. This is a fact about the node, not \
             about your arguments: retrying against the same node cannot succeed, and the operator \
             has to upgrade it."
            .to_string(),
        ControlRejected::Busy => "control command not sent: the outbound queue to the node is \
             full — the previous command(s) have not drained yet. NOTHING was enqueued for this \
             call; try again in a moment."
            .to_string(),
        // Handled as a never-sent before this is reached; spelled rather than wildcarded so a new
        // variant fails to compile here instead of falling into someone else's sentence.
        ControlRejected::Gone => {
            "control command not sent: the control connection is gone.".to_string()
        }
    }
}

/// The NODE-LIFECYCLE subset of [`WRITE_TOOLS`]: the writes that change what the node RUNS or how
/// it is CONFIGURED rather than what is in its book.
///
/// ⚠ It no longer says where these three are BUILT — [`crate::cmd::verbs`] builds all ten, like
/// every other write, since the `trade` REPL grew its own spelling of them. What is left keyed on
/// this array is the one thing that really is lifecycle-specific: [`instructions`] scopes its
/// [`INSTRUCTIONS_LIFECYCLE`] clause on it, so a fourth one joins that clause by construction.
///
/// It is a SUBSET and never a second roster: `the_lifecycle_tools_are_a_subset_of_the_write_roster`
/// holds every name here to [`WRITE_TOOLS`], because a lifecycle tool that fell out of that array
/// would lose the mandatory preview gate while keeping everything that makes it look gated.
pub(super) const LIFECYCLE_TOOLS: [&str; 3] = ["mount_strategy", "unmount_strategy", "set_setting"];

/// The UNATTENDED gate: in a session started with `--unattended`, EVERY `set_setting` is refused
/// HERE — before a preview is rendered or a token is minted, so the command is unconfirmable rather
/// than merely unconfirmed. PURE — the input is the built command.
///
/// # The ruling
///
/// The owner, on decision 0040 (the unattended agent runner): **an UNATTENDED run changes no
/// setting at all.** 2026-09-28 ruled it for `policy.*`; 2026-09-29 widened it to every key,
/// because `flags.*` holds the same class of switch — `flags.tradehub_live` arms live trading and
/// `flags.reconcile_off` takes a safety net away — and a per-key list of the dangerous ones is a
/// list that rots. An ATTENDED agent changes a live setting only after the owner said yes in chat
/// (`docs/decisions/0086` point 6); a session nobody attends has no chat to say it in, and its
/// `--allow-writes` ring would otherwise reconfigure the node through the preview token alone. The
/// operator changes settings with `vike-cli config set` or the GUI, and the refusal says so.
///
/// # Decided on the COMMAND, not the key
///
/// Every `SetSetting` is refused, whatever its key — so there is no key rule here to keep in step
/// with the node's sections, and no spelling of a key that could slip past it.
pub(super) fn unattended_refusal(cmd: &WireCommand) -> Result<(), String> {
    let WireCommand::SetSetting { key, .. } = cmd else {
        return Ok(());
    };
    Err(format!(
        "this session is `--unattended` — nobody attends it — and no setting is changed through \
         one, `{key}` included (decision 0040: the owner's rulings of 2026-09-28 for `policy.*` \
         and 2026-09-29 for every other key). The operator changes settings with `vike-cli config \
         set` or the GUI. Nothing was previewed, no preview_token was issued, and nothing was sent."
    ))
}

// ⚠ Two functions of the retype stood in this file and are DELETED:
// `fn typed_confirm_verdict(cmd)`, here — the gate that refused a policy write carrying no
// `policy_confirm` before a token was minted — and `fn policy_confirm_property()`, beside
// [`confirm_property`] — the schema argument that asked an agent to fetch the OPERATOR's retyping
// of the key. Both went with the ceremony (`docs/decisions/0086` point 7: *"confirmation over
// confirmation … a nightmare"*); the node had already stopped reading the confirm the argument
// carried. Their names stay in this comment because records and specs written while they shipped
// cite them — the repair to a citation of deleted code is a tombstone, never a deleted citation.

/// The mandatory-preview payload for a write tool: the resolved command, the SHARED client-side
/// guardrail check ([`crate::cmd::verbs::guardrail_check`] — advisory; the node's server-side gate
/// is the enforcing one), the rationale that will be recorded, and the confirm hint. PURE.
///
/// `reason` is ECHOED here on purpose: it is the one part of the request the agent cannot otherwise
/// see the effect of (it goes to the node's audit trail, not to the order), so the preview shows it
/// alongside the command it will be filed against. It is echoed as SENT — the node applies its own
/// sanitization (`vike_tradehub::audit::sanitize_reason`) before recording.
///
/// `venue_check` is [`Server::vet_commanded_venue`]'s verdict, carried here rather than recomputed:
/// a preview that reached this function was NOT refused, so the only thing left to disclose is
/// whether the venue was compared against the node's mounted set at all. It is reported even in the
/// `mounted` case, because "checked and fine" and "not checked" must not look the same to an agent
/// — the same rule `verified_by_node` is written to above.
///
/// `change` is a settings write's `old → new` ([`Server::setting_change`]) and rides the payload as
/// `change` for that tool only — every other preview's shape is exactly what it was. It is what
/// `docs/decisions/0086` point 7 puts where the retyped key was: the thing an agent shows the owner
/// before asking for the yes a live setting needs.
#[allow(clippy::too_many_arguments)] // one per disclosure the preview makes, each argued above
pub(super) fn preview_of(
    name: &str,
    cmd: &WireCommand,
    reason: Option<&str>,
    caps: verbs::GuardrailCaps,
    preview_token: &str,
    node: Option<Value>,
    venue_check: &'static str,
    change: Option<&verbs::SettingChange>,
) -> Value {
    let wire = serde_json::to_value(cmd).unwrap_or(Value::Null);
    // ⚠ Whether the node ANSWERED decides the wording, and the wording is the point. An absent
    // verdict must never read as an approving one: the client-side guardrail cannot price a market
    // order (no `price` to size against) and market is the DEFAULT order type, so on that path the
    // local check is vacuous rather than permissive-by-accident.
    //
    // ⚠ The question is asked of the VERDICT, not of the `Option`. [`Server::node_preview`] returns
    // `Some` on its FAILURE path too — a `checked_by: "none"` row carrying the transport error, so
    // the preview can name the fault rather than silently dropping the field — and `node.is_some()`
    // therefore reported `verified_by_node: true` for a node that was dialled and refused, or
    // unreachable, or that answered `AuthDenied`. That is the one flag the prompts this server
    // serves teach an agent to read as "the verdict that counts", so it must mean asked AND
    // answered. `a_node_that_could_not_be_asked_is_not_a_verified_preview` is the pin on the pure
    // payload; `a_preview_whose_node_could_not_be_asked_is_not_verified` pins the same property
    // end to end, through `handle` against a node that refuses the connection.
    let verified = node.as_ref().is_some_and(|v| v["checked_by"] == CHECKED_BY_NODE);
    let mut preview = json!({
        "will_execute": false,
        "tool": name,
        "wire_command": wire,
        "guardrail": verbs::guardrail_check(cmd, caps).to_json(),
        "node_verdict": node,
        "verified_by_node": verified,
        "venue_check": venue_check,
        "preview_token": preview_token,
        "reason": reason,
        "note": if verified {
            "PREVIEW ONLY — nothing was sent. `node_verdict` is the NODE's own dry-run against the \
             real ControlLimits + RiskGate; it is the verdict that counts. To execute, call again \
             with BOTH \"confirm\": true AND this exact \"preview_token\". The token fires once, \
             expires after 60s, and is bound to THIS command — confirming a different command with \
             it is refused. For submit_order, `wire_command.Submit.client_order_id` was minted here \
             and is the id that will be sent. Any `reason` is recorded in the node's audit trail \
             (sanitized) and never reaches the order."
        } else {
            "PREVIEW ONLY — nothing was sent. ⚠ THE NODE WAS NOT ASKED (no node configured, no \
             control key, or unreachable), so `guardrail` is an UNVERIFIED CLIENT-SIDE ESTIMATE — \
             and it cannot size a market order at all, because a market order carries no price. Do \
             not read it as approval. To execute, call again with BOTH \"confirm\": true AND this \
             exact \"preview_token\". The token fires once, expires after 60s, and is bound to THIS \
             command. The node's ControlLimits + RiskGate remain the enforcing gate regardless."
        }
    });
    if let (Some(change), Some(fields)) = (change, preview.as_object_mut()) {
        fields.insert("change".to_string(), change.to_json());
    }
    preview
}
