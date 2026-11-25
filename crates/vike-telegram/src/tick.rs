//! ONE poll → dispatch pass — the reusable core the poller thread calls on a timer, pure over the
//! [`TelegramDependencies`] seam (no network, no clock, no randomness of its own).
//!
//! The STEP ORDER inside [`poll_once`] is security-critical even though the steps themselves live
//! in `ledger.rs` (dedupe/mark), `config.rs` (allowlist) and `confirm.rs` (the token contract) —
//! see [`poll_once`]'s own doc.

use super::{
    CONFIRM_WINDOW_MS, Instruction, NotExecuted, Outcome, Pending, PendingConfirms, PollError,
    TelegramConfig, TelegramDependencies, TelegramUpdate, USAGE, UpdateLedger, confirm_reason,
    confirmed_by, describe, parse_instruction,
};
use vike_utility::errors::FailureOutcome;

/// File a command that did not execute under the report field of its kind: a NOT NOW in `not_now`,
/// every other no in `refused`.
fn report_not_executed(report: &mut TickReport, outcome: FailureOutcome, why: String) {
    if outcome == FailureOutcome::NotNow {
        report.not_now.push(why);
    } else {
        report.refused.push(why);
    }
}

/// What one [`poll_once`] pass did. Every field is observable from a test through the deps seam.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TickReport {
    /// chat_ids that were ignored because they are not allowlisted (logged, NEVER answered).
    pub ignored_unlisted: Vec<i64>,
    /// from_ids that were ignored because the OPTIONAL per-user allowlist is configured and does
    /// not name them — the chat passed, the person did not. Always EMPTY when no user allowlist is
    /// configured, which is the default. Logged, NEVER answered, exactly like `ignored_unlisted`.
    pub ignored_unlisted_user: Vec<i64>,
    /// update_ids the ledger had already consumed (restart/redelivery replay safety).
    pub skipped_duplicate: Vec<i64>,
    /// ⚠ update_ids DROPPED because the at-most-once ledger could not record them. Nothing was
    /// dispatched for any of these — an order this daemon cannot record is one it must not place.
    /// Non-empty means the ledger path stopped being writable while the channel was running.
    pub unrecorded: Vec<i64>,
    /// tokens issued by a preview — nothing was sent for any of these.
    pub previewed: Vec<String>,
    /// order client ids the core ACCEPTED (the only field that means an order moved).
    pub executed: Vec<String>,
    /// tokens refused because their 60 s window had elapsed.
    pub expired: Vec<String>,
    /// refusal reasons (the node's preview verdicts, its refusals of a sent command, and a command
    /// this process provably never wrote to the node: a NOT SENT reply).
    pub refused: Vec<String>,
    /// the words of a command the node answered NOT NOW (it is starting or its core is busy): nothing
    /// executed, and the same command is taken shortly. Never in `refused`.
    pub not_now: Vec<String>,
    /// ⚠ commands written to the node whose answer never came: they MAY have executed. Each reply
    /// reads OUTCOME UNKNOWN and says to check `/orders` and `/positions` before retrying.
    pub unconfirmed: Vec<String>,
    /// replies actually sent.
    pub replies: usize,
    /// ⚠ The `getUpdates` call FAILED, so this pass saw no updates at all and every field above is
    /// empty. The caller decides what to do about it — [`PollBackoff`](super::PollBackoff) turns it
    /// into a delay, a stop, or a log line — because the reaction is stateful ACROSS passes and this
    /// function is one pass.
    ///
    /// ⚠ **This pass therefore does NOT log the failure**, which is deliberate: it used to
    /// `tracing::warn!` unconditionally, and a permanently-failing endpoint (a mistyped bot token)
    /// made that ~2 lines/second forever. A caller that ignores this field is silent about an
    /// outage; `crates/vike-telegram/src/lib.rs`'s `run_loop` is the production one and does
    /// not.
    pub poll_failure: Option<PollError>,
}

/// ONE poll → dispatch pass: the reusable core the poller thread calls on a timer (the
/// `settle_once`/`redeem_once` twin). Pure over the [`TelegramDependencies`] seam — no network, no clock,
/// no randomness of its own.
///
/// Order within the pass is load-bearing:
/// 1. **dedupe** against the ledger — a replayed update does nothing at all;
/// 2. **mark** the update consumed BEFORE acting on it (at-most-once: lose a command rather than
///    place it twice) — and if the mark cannot be PERSISTED, the update is dropped here, before
///    step 4 can dispatch anything;
/// 3. **allowlist** — an unlisted chat is logged and dropped with NO reply;
/// 4. **dispatch** — read verbs answer from the snapshot, write verbs PREVIEW, and only
///    `/confirm` reaches [`TelegramDependencies::accept`].
///
/// Step 2's refusal is UNIFORM across verbs, deliberately: the ledger's guarantee is about
/// UPDATES, not about which verb an update happens to carry, and a "reads are harmless to replay"
/// carve-out would make that guarantee depend on the grammar in `parse.rs` — a coupling that rots
/// the first time a verb changes category. It is also unreachable in the normal case:
/// [`UpdateLedger::open`] proves the ledger appendable before the channel arms, so getting here
/// means the path stopped being writable under a RUNNING daemon.
///
/// A FAILED `getUpdates` is step 0: nothing else in the pass runs, and the failure is REPORTED
/// ([`TickReport::poll_failure`]) rather than logged, because whether to retry, how long to wait and
/// whether to say anything are decisions about a RUN of passes — see
/// `crates/vike-telegram/src/failure.rs`.
pub fn poll_once(
    dependencies: &dyn TelegramDependencies,
    config: &TelegramConfig,
    ledger: &UpdateLedger,
    pending: &mut PendingConfirms,
) -> TickReport {
    let mut report = TickReport::default();
    let updates = match dependencies.get_updates(ledger.offset()) {
        Ok(u) => u,
        Err(e) => {
            // REPORTED, never logged here — see this function's doc and `TickReport::poll_failure`.
            // (The error carries no url and no token: `ProdTelegramDependencies` never puts either in one.)
            report.poll_failure = Some(e);
            return report;
        }
    };

    for update in updates {
        if ledger.is_processed(update.update_id) {
            report.skipped_duplicate.push(update.update_id);
            continue;
        }
        // AT-MOST-ONCE: consumed before anything can happen because of it — and if that record
        // cannot be made DURABLE, nothing happens because of it at all.
        if let Err(e) = ledger.mark(update.update_id) {
            tracing::error!(
                update_id = update.update_id,
                %e,
                "telegram control: the at-most-once ledger could NOT record this update, so it was \
                 DROPPED unprocessed — a command this daemon cannot record is one it must not \
                 place. The control channel accepts nothing until the ledger path is writable again"
            );
            report.unrecorded.push(update.update_id);
            continue;
        }

        if update.text.trim().is_empty() {
            // A non-text update (photo / sticker / edit). Acked by the mark above, never answered.
            continue;
        }
        if !config.allows(update.chat_id) {
            // Logged, NEVER answered — a reply would confirm to a stranger that a node is here.
            tracing::warn!(
                chat_id = update.chat_id,
                update_id = update.update_id,
                "telegram control: message from an unlisted chat IGNORED (no reply sent)"
            );
            report.ignored_unlisted.push(update.chat_id);
            continue;
        }
        if !config.allows_user(update.from_id) {
            // The OPT-IN second half of the allowlist. Same discipline as the chat check and for
            // the same reason: logged, never answered — a reply would tell an unlisted member of an
            // allowlisted group that a trading node is behind this bot.
            tracing::warn!(
                chat_id = update.chat_id,
                from_id = update.from_id,
                update_id = update.update_id,
                "telegram control: message from a chat-allowlisted but USER-unlisted sender \
                 IGNORED (no reply sent)"
            );
            report.ignored_unlisted_user.push(update.from_id);
            continue;
        }
        dispatch(dependencies, pending, &update, &mut report);
    }

    pending.prune_expired(dependencies.now_ms());
    report
}

/// Send one reply and count it. A free function rather than a closure so a call site can also
/// mutate `report`'s other fields in the same arm (a `FnMut` closure capturing `report` would hold
/// the borrow across the whole arm).
fn reply(
    dependencies: &dyn TelegramDependencies,
    chat_id: i64,
    report: &mut TickReport,
    text: String,
) {
    dependencies.send_message(chat_id, &text);
    report.replies += 1;
}

/// Dispatch ONE allowlisted, not-yet-processed message.
fn dispatch(
    dependencies: &dyn TelegramDependencies,
    pending: &mut PendingConfirms,
    update: &TelegramUpdate,
    report: &mut TickReport,
) {
    let chat = update.chat_id;
    let actor = update.from_username.as_deref();
    match parse_instruction(&update.text) {
        Instruction::Ignore => {}
        Instruction::Help | Instruction::Unknown => {
            reply(dependencies, chat, report, USAGE.to_string())
        }
        Instruction::Malformed(why) => {
            reply(dependencies, chat, report, format!("{why}\n\n{USAGE}"))
        }
        Instruction::Read(verb) => {
            let text = dependencies.read(verb);
            reply(dependencies, chat, report, text);
        }
        Instruction::Write(cmd) => {
            let cmd = dependencies.fill_order_client_id(*cmd);
            let line = describe(&cmd);
            // The server-authoritative guardrail runs at PREVIEW time, so an operator learns the
            // command would be refused before spending a confirmation on it. A refused command
            // yields NO token: there is nothing worth confirming.
            let notes = match dependencies.preview(&cmd) {
                Ok(notes) => notes,
                Err(NotExecuted { outcome, why }) => {
                    tracing::warn!(
                        chat_id = chat,
                        from_id = update.from_id,
                        ?outcome,
                        %why,
                        "telegram control: write instruction not previewed, nothing sent"
                    );
                    let head = match outcome {
                        FailureOutcome::NotNow => "NOT NOW (nothing sent)",
                        FailureOutcome::NotSent => "NOT SENT",
                        FailureOutcome::Refused | FailureOutcome::OutcomeUnknown => {
                            "REFUSED (nothing sent)"
                        }
                    };
                    let text = format!("{head}\n{line}\n{why}");
                    report_not_executed(report, outcome, why);
                    reply(dependencies, chat, report, text);
                    return;
                }
            };
            // What the node says the order will go out as, ahead of the /confirm it answers.
            let notes: String = notes.iter().map(|note| format!("note: {note}\n")).collect();
            let token = dependencies.mint_token();
            pending.insert(Pending {
                token: token.clone(),
                command: cmd,
                reason: confirm_reason(chat, update.from_id, actor, &update.text),
                chat_id: chat,
                from_id: update.from_id,
                issued_ms: dependencies.now_ms(),
            });
            tracing::info!(
                chat_id = chat,
                from_id = update.from_id,
                %line,
                "telegram control: previewed (NOTHING sent; awaiting /confirm)"
            );
            let text = format!(
                "PREVIEW — nothing has been sent.\n{line}\nguardrail: OK\n{notes}\nto execute, within {}s:\n/confirm {token}",
                CONFIRM_WINDOW_MS / 1000
            );
            report.previewed.push(token);
            reply(dependencies, chat, report, text);
        }
        Instruction::Confirm(token) => {
            let Some(p) = pending.take(&token, chat) else {
                // Same answer for unknown / already-used / another chat's token: never distinguish.
                let text = "no such pending confirmation (unknown, already used, or expired).";
                reply(dependencies, chat, report, text.to_string());
                return;
            };
            // Taken (and therefore consumed) BEFORE this check — an expired token is burned, not
            // left lying around for a later retry.
            if dependencies.now_ms().saturating_sub(p.issued_ms) > CONFIRM_WINDOW_MS {
                tracing::warn!(chat_id = chat, "telegram control: confirmation EXPIRED");
                report.expired.push(token);
                let text = format!(
                    "EXPIRED — the {}s confirmation window elapsed; nothing was sent. Re-issue the instruction.",
                    CONFIRM_WINDOW_MS / 1000
                );
                reply(dependencies, chat, report, text);
                return;
            }
            let line = describe(&p.command);
            // The audit rationale names BOTH people: the one whose instruction this is (recorded
            // at preview time, inside `p.reason`) and the one who authorized executing it. In a DM
            // they are the same; in a group they need not be, because the token binds to the CHAT.
            let reason = confirmed_by(&p.reason, update.from_id, actor);
            match dependencies.accept(p.command, &reason) {
                Outcome::Sent { order_client_id, notes } => {
                    tracing::warn!(
                        chat_id = chat,
                        from_id = update.from_id,
                        previewed_by = p.from_id,
                        %order_client_id,
                        %line,
                        "telegram control: command ACCEPTED by the core (a REAL order may now be live)"
                    );
                    let mut text = format!("SENT\n{line}\ncoid={order_client_id}");
                    // What the door says about a command it sent (a flatten that closes nothing, a
                    // closed market): this phone is where the operator reads the answer.
                    for note in &notes {
                        text.push_str("\nnote: ");
                        text.push_str(note);
                    }
                    report.executed.push(order_client_id);
                    reply(dependencies, chat, report, text);
                }
                // Nothing executed: the header is which no it was, read off its type.
                Outcome::NotExecuted(NotExecuted { outcome, why }) => {
                    tracing::warn!(chat_id = chat, ?outcome, %why, "telegram control: command not executed");
                    let head = match outcome {
                        FailureOutcome::NotNow => "NOT NOW",
                        FailureOutcome::NotSent => "NOT SENT",
                        FailureOutcome::Refused | FailureOutcome::OutcomeUnknown => "REFUSED",
                    };
                    let text = format!("{head}\n{line}\n{why}");
                    report_not_executed(report, outcome, why);
                    reply(dependencies, chat, report, text);
                }
                // No answer came: neither SENT nor REFUSED may be printed, only the one wording's verdict.
                Outcome::Unanswered(unanswered) => {
                    let msg = unanswered.to_string();
                    if unanswered.failure_outcome() == FailureOutcome::NotSent {
                        tracing::warn!(chat_id = chat, %msg, "telegram control: command NOT SENT");
                        reply(dependencies, chat, report, format!("NOT SENT\n{line}\n{msg}"));
                        report.refused.push(msg);
                    } else {
                        tracing::error!(chat_id = chat, %msg, "telegram control: command outcome UNKNOWN");
                        let text =
                            format!("OUTCOME UNKNOWN\n{line}\n{msg} Check /orders and /positions.");
                        reply(dependencies, chat, report, text);
                        report.unconfirmed.push(msg);
                    }
                }
            }
        }
    }
}
