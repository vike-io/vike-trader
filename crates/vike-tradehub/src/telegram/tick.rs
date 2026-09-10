//! ONE poll → dispatch pass — the reusable core the poller thread calls on a timer, pure over the
//! [`TelegramDeps`] seam (no network, no clock, no randomness of its own).
//!
//! The STEP ORDER inside [`poll_once`] is security-critical even though the steps themselves live
//! in `ledger.rs` (dedupe/mark), `config.rs` (allowlist) and `confirm.rs` (the token contract) —
//! see [`poll_once`]'s own doc.

use super::{
    CONFIRM_WINDOW_MS, Instruction, Pending, PendingConfirms, PollError, TelegramConfig,
    TelegramDeps, TgUpdate, USAGE, UpdateLedger, confirm_reason, confirmed_by, describe, fill_coid,
    parse_instruction,
};

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
    /// coids the core ACCEPTED (the only field that means an order moved).
    pub executed: Vec<String>,
    /// tokens refused because their 60 s window had elapsed.
    pub expired: Vec<String>,
    /// refusal reasons (guardrail verdicts + `accept_command` errors).
    pub refused: Vec<String>,
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
    /// outage; `crates/vike-tradehub/src/telegram/mod.rs`'s `spawn` is the production one and does
    /// not.
    pub poll_failure: Option<PollError>,
}

/// ONE poll → dispatch pass: the reusable core the poller thread calls on a timer (the
/// `settle_once`/`redeem_once` twin). Pure over the [`TelegramDeps`] seam — no network, no clock,
/// no randomness of its own.
///
/// Order within the pass is load-bearing:
/// 1. **dedupe** against the ledger — a replayed update does nothing at all;
/// 2. **mark** the update consumed BEFORE acting on it (at-most-once: lose a command rather than
///    place it twice) — and if the mark cannot be PERSISTED, the update is dropped here, before
///    step 4 can dispatch anything;
/// 3. **allowlist** — an unlisted chat is logged and dropped with NO reply;
/// 4. **dispatch** — read verbs answer from the snapshot, write verbs PREVIEW, and only
///    `/confirm` reaches [`TelegramDeps::accept`].
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
/// `crates/vike-tradehub/src/telegram/failure.rs`.
pub fn poll_once(
    deps: &dyn TelegramDeps,
    cfg: &TelegramConfig,
    ledger: &UpdateLedger,
    pending: &mut PendingConfirms,
) -> TickReport {
    let mut report = TickReport::default();
    let updates = match deps.get_updates(ledger.offset()) {
        Ok(u) => u,
        Err(e) => {
            // REPORTED, never logged here — see this function's doc and `TickReport::poll_failure`.
            // (The error carries no url and no token: `ProdTelegramDeps` never puts either in one.)
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
        if !cfg.allows(update.chat_id) {
            // Logged, NEVER answered — a reply would confirm to a stranger that a node is here.
            tracing::warn!(
                chat_id = update.chat_id,
                update_id = update.update_id,
                "telegram control: message from an unlisted chat IGNORED (no reply sent)"
            );
            report.ignored_unlisted.push(update.chat_id);
            continue;
        }
        if !cfg.allows_user(update.from_id) {
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
        dispatch(deps, pending, &update, &mut report);
    }

    pending.prune_expired(deps.now_ms());
    report
}

/// Send one reply and count it. A free function rather than a closure so a call site can also
/// mutate `report`'s other fields in the same arm (a `FnMut` closure capturing `report` would hold
/// the borrow across the whole arm).
fn reply(deps: &dyn TelegramDeps, chat_id: i64, report: &mut TickReport, text: String) {
    deps.send_message(chat_id, &text);
    report.replies += 1;
}

/// Dispatch ONE allowlisted, not-yet-processed message.
fn dispatch(
    deps: &dyn TelegramDeps,
    pending: &mut PendingConfirms,
    update: &TgUpdate,
    report: &mut TickReport,
) {
    let chat = update.chat_id;
    let actor = update.from_username.as_deref();
    match parse_instruction(&update.text) {
        Instruction::Ignore => {}
        Instruction::Help | Instruction::Unknown => reply(deps, chat, report, USAGE.to_string()),
        Instruction::Malformed(why) => reply(deps, chat, report, format!("{why}\n\n{USAGE}")),
        Instruction::Read(verb) => {
            let text = deps.read(verb);
            reply(deps, chat, report, text);
        }
        Instruction::Write(cmd) => {
            let cmd = fill_coid(cmd, &deps.mint_coid());
            let line = describe(&cmd);
            // The server-authoritative guardrail runs at PREVIEW time, so an operator learns the
            // command would be refused before spending a confirmation on it. A refused command
            // yields NO token: there is nothing worth confirming.
            if let Some(refusal) = deps.preview(&cmd) {
                tracing::warn!(
                    chat_id = chat,
                    from_id = update.from_id,
                    %refusal,
                    "telegram control: write instruction REFUSED by the server-edge guardrail"
                );
                let text = format!("REFUSED (nothing sent)\n{line}\n{refusal}");
                report.refused.push(refusal);
                reply(deps, chat, report, text);
                return;
            }
            let token = deps.mint_token();
            pending.insert(Pending {
                token: token.clone(),
                cmd,
                reason: confirm_reason(chat, update.from_id, actor, &update.text),
                chat_id: chat,
                from_id: update.from_id,
                issued_ms: deps.now_ms(),
            });
            tracing::info!(
                chat_id = chat,
                from_id = update.from_id,
                %line,
                "telegram control: previewed (NOTHING sent; awaiting /confirm)"
            );
            let text = format!(
                "PREVIEW — nothing has been sent.\n{line}\nguardrail: OK\n\nto execute, within {}s:\n/confirm {token}",
                CONFIRM_WINDOW_MS / 1000
            );
            report.previewed.push(token);
            reply(deps, chat, report, text);
        }
        Instruction::Confirm(token) => {
            let Some(p) = pending.take(&token, chat) else {
                // Same answer for unknown / already-used / another chat's token: never distinguish.
                let text = "no such pending confirmation (unknown, already used, or expired).";
                reply(deps, chat, report, text.to_string());
                return;
            };
            // Taken (and therefore consumed) BEFORE this check — an expired token is burned, not
            // left lying around for a later retry.
            if deps.now_ms().saturating_sub(p.issued_ms) > CONFIRM_WINDOW_MS {
                tracing::warn!(chat_id = chat, "telegram control: confirmation EXPIRED");
                report.expired.push(token);
                let text = format!(
                    "EXPIRED — the {}s confirmation window elapsed; nothing was sent. Re-issue the instruction.",
                    CONFIRM_WINDOW_MS / 1000
                );
                reply(deps, chat, report, text);
                return;
            }
            let line = describe(&p.cmd);
            // The audit rationale names BOTH people: the one whose instruction this is (recorded
            // at preview time, inside `p.reason`) and the one who authorized executing it. In a DM
            // they are the same; in a group they need not be, because the token binds to the CHAT.
            let reason = confirmed_by(&p.reason, update.from_id, actor);
            match deps.accept(p.cmd, &reason) {
                Ok(coid) => {
                    tracing::warn!(
                        chat_id = chat,
                        from_id = update.from_id,
                        previewed_by = p.from_id,
                        %coid,
                        %line,
                        "telegram control: command ACCEPTED by the core (a REAL order may now be live)"
                    );
                    let text = format!("SENT\n{line}\ncoid={coid}");
                    report.executed.push(coid);
                    reply(deps, chat, report, text);
                }
                Err(msg) => {
                    tracing::warn!(chat_id = chat, %msg, "telegram control: command refused");
                    let text = format!("REFUSED\n{line}\n{msg}");
                    report.refused.push(msg);
                    reply(deps, chat, report, text);
                }
            }
        }
    }
}
