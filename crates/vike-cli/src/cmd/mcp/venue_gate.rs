//! The VENUE gate — the refusal of a write whose venue (or account) the node does not mount, and
//! the node-frame reads it stands on.
//!
//! It runs inside `Server::call_tool`'s `is_write_tool` arm BEFORE a preview token is minted, so a
//! new write tool routed through that arm inherits it. `Server::vet_commanded_venue` is the I/O
//! half (it asks the node what it mounts); `venue_verdict` is the PURE decision, and the split is
//! what makes the refusal testable without a node. Split out of `cmd/mcp.rs` (code-layout phase 2,
//! task 9); `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md` carries the
//! disposition.

use vike_tradehub_client::wire::WireCommand;

use super::*;

impl Server {
    /// Refuse a write whose venue this node does not mount — and answer which of the three
    /// [`VENUE_CHECK_MOUNTED`] dispositions the preview should report.
    ///
    /// # The defect this closes, read off the write path
    ///
    /// A venue string is carried VERBATIM from the tool argument to the core and is never compared
    /// to anything on the way: [`crate::cmd::verbs::verb_from_tool_args`] takes it as text,
    /// `vike_tradehub::server`'s `accept_command` vets only the rate token and the notional cap,
    /// and its `lower_command` copies it onto the `OrderRequest`. The node's dry-run is the same
    /// vetting (`ControlLimits::preview_vet` is `notional_reason` and nothing else), so a PREVIEW
    /// accepts it too. What then happens in the core is the part worth stating exactly, because it
    /// is not a rejection: `vike_core`'s `CoreThread::apply_intent_routed` resolves the engine with
    /// `route_of(…)` and then `unwrap_or(0)` — **a venue that routes to nothing is routed to the
    /// PRIMARY engine**, whichever venue happens to be mounted first. And because the string is not
    /// in `vike_model::VENUES`, `preflight_order_at`'s unknown-venue affordance answers `Ok(())`,
    /// so every capability check (order kind, TIF, margin mode) is SKIPPED for it in silence.
    ///
    /// So the measured failure — an agent inventing `venue: "node"` from the operator's wording,
    /// previewed, confirmed, and really resting in the book — was not a validation gap at one
    /// layer. Nothing anywhere between the tool argument and the venue edge had an opinion. On a
    /// single-venue paper mount that costs nothing; on a multi-venue live mount it books the
    /// operator's order on an account they never named, with the capability preflight off.
    ///
    /// # What this compares, and why it is the node's own answer rather than a roster
    ///
    /// The evidence is `node_snapshot`'s `venues[].venue` — the per-engine ledger blocks the node
    /// publishes — and NOT `vike_model::VENUES`. A roster check would refuse a legitimate mount:
    /// the unknown-venue affordance above exists for paper and sim engines behind non-roster ids,
    /// and a node is free to mount one. Asking the node keeps the question "does this command
    /// route" rather than "is this a venue we ship".
    ///
    /// ⚠ **The comparison is EXACT, and case-folding it would reopen the hole.** Routing compares
    /// the payload's venue to an engine's `route_key` with `==`
    /// (`vike_core`'s `engine_idx_for_route_key`), so `"Binance"` routes to nothing on a node
    /// mounting `"binance"` — and therefore falls back to the primary engine exactly as `"node"`
    /// did. A refusal looser than the routing admits strings the routing then silently redirects.
    ///
    /// ⚠ **NO EVIDENCE IS NOT A REFUSAL.** When the mounted set cannot be read the command is
    /// allowed through and the preview says so ([`VENUE_CHECK_UNVERIFIED`]) — the disposition
    /// `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md` argues, and the
    /// reason it is not reckless is that the mounted set is read over the node connection the write
    /// itself needs: a node that cannot be read is overwhelmingly a node that cannot be written to
    /// either, and [`Server::execute`] fails there anyway. The one configuration where the two come
    /// apart is a session holding a CONTROL key and no OBSERVE key, which is a declared residual
    /// rather than an oversight — every documented setup resolves both from one store.
    /// The I/O half; [`venue_verdict`] is the decision, and the split is what makes the refusal
    /// testable without a node.
    pub(super) fn vet_commanded_venue(
        &mut self,
        cmd: &WireCommand,
    ) -> Result<&'static str, ToolError> {
        let venue = commanded_venue(cmd);
        // Asked ONLY when there is a venue to check, so a venue-less verb — the panic button
        // above all — costs no node round trip and cannot be delayed by one.
        let mounted = venue.and_then(|_| self.mounted_accounts());
        venue_verdict(venue, commanded_account(cmd), mounted.as_deref()).map_err(ToolError::refused)
    }

    /// The venues this node reports mounting, or `None` when that could not be established.
    ///
    /// Read through [`Server::order_gate_snapshot`] rather than a second dial, which buys three
    /// things: the reconnect-and-liveness gate `node_snapshot` owns applies here too (a STALE frame
    /// can never answer this question), the answer is the same frame the refusal tells the agent to
    /// go and read, and there is no second way to reach the node. ⚠ It is NOT the read tool's
    /// fast wait: this read waits for a FOLD, which is what keeps the window described below as
    /// narrow as it has always been — see [`node_reads::wait_for_a_fold`].
    ///
    /// `None` covers every "no evidence" shape — no node configured, no observe key, a node that
    /// is down — and one more that is worth spelling out: a frame carrying an EMPTY `venues` array.
    /// That is a node that published no ledger block, not a node that mounts nothing, and reading
    /// it as the latter would refuse every write against it.
    ///
    /// ⚠ **That shape is REAL and was measured**, not a defensive hypothetical: a `vike-tradehub`
    /// daemon that has not folded anything yet publishes a frame with `seq: 0`, an `identity`
    /// block, the primary `venue` and `symbol` — and `venues: []`, because `vike_core`'s
    /// `CoreSnapshot::empty` carries no ledger blocks while `CoreSnapshot::build` always carries at
    /// least the primary one. So **this gate is inert between a node's start and its first fold**,
    /// reporting [`VENUE_CHECK_UNVERIFIED`] for every write in that window. Read off the
    /// `vike-agent-eval` transcript of a freshly started paper node, 2026-09-06, which is why that
    /// harness rests an order before the case runs.
    ///
    /// ⚠ **The top-level `venue` may NOT be used to close that window**, tempting as it is: it is
    /// the PRIMARY engine's venue, and on a multi-venue node in the same pre-fold window it is not
    /// the whole mounted set — refusing on it would reject a legitimate write to a mounted
    /// secondary while telling the operator, falsely, that the node mounts only the primary. A
    /// wrong refusal on the order path is worse than a disclosed unmade check. Closing it properly
    /// means the node publishing its mounted set in the placeholder frame, which
    /// `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md` carries as a reopen
    /// condition.
    /// **The ROUTE KEYS this node mounts** — one per engine, not one per venue.
    ///
    /// ⚠ **Reads `venues[].route_key`, not `venues[].venue`**, and that is
    /// `docs/decisions/0041-an-unmountable-venue-is-refused-at-the-preview.md`'s verdict 2: the
    /// check must ask the ROUTING question and not a neighbouring one, because a refusal looser
    /// than the routing *"admits precisely the strings the routing silently redirects."* Two
    /// accounts of one venue publish two identical `venue` strings — so the old projection could
    /// not tell a fifty-account node from a one-account node, and the gate it fed answered
    /// `mounted` for a command that would reach whichever engine came first.
    ///
    /// ⚠ **An OLD node omits the field**, and the `unwrap_or` below reads that as the venue itself
    /// rather than as an empty key. That is deliberate and it is the only reading that is safe in
    /// both directions: a single-account node's route key EQUALS its venue, so an old node's blocks
    /// project to exactly the set the previous implementation produced, and the gate's behaviour
    /// against every node in production today is unchanged.
    /// **This node's ACCOUNT-SET digest right now**, or `None` when the node could not be ASKED.
    ///
    /// `Some(0)` still collapses two states — a node predating the field and a node with no
    /// accounts — and that collapse is sound, because both are the node ANSWERING and neither can
    /// move an epoch it does not publish. Two `Some(0)`s compare EQUAL, which is the pre-field
    /// behaviour exactly.
    ///
    /// ⚠ **An UNREACHABLE node used to be folded into that same `0`, and it is now `None`.** The
    /// two are not the same fact: "the node says its epoch is 0" is an answer, and "I could not
    /// reach the node" is the absence of one. Folding them let the confirm guard report a LOST
    /// LINK as a CHANGED ACCOUNT SET — telling an agent that an account "may have been mounted,
    /// unmounted, or relabelled" when what happened is that the read socket died.
    ///
    /// ⚠ The fold's real cost was not the wording. It made the guard's verdict turn on WHEN
    /// visibility was lost: a read failing at BOTH preview and confirm stamped `0` both times and
    /// compared equal, so the confirm PROCEEDED — while a read failing only at confirm REFUSED.
    /// The case with less information was the one that was allowed through. MEASURED as CI run
    /// 35437491370, where `a_write_after_a_drop_is_refused_once_and_the_next_call_reconnects`
    /// received `(16314315392579917129 -> 0)` in place of the link answer it asserts.
    ///
    /// ⚠ It is still not evidence of anything ON ITS OWN. `None` says nothing about whether the
    /// set moved, and the confirm guard therefore refuses on it in NEITHER direction; a caller
    /// that wants to know whether the LINK is up must ask the thing that writes, not this.
    pub(super) fn node_accounts_epoch(&mut self) -> Option<u64> {
        let snapshot = self.order_gate_snapshot().ok()?;
        Some(snapshot["accounts_epoch"].as_u64().unwrap_or(0))
    }

    fn mounted_accounts(&mut self) -> Option<Vec<String>> {
        let snap = self.order_gate_snapshot().ok()?;
        let keys: Vec<String> = snap["venues"]
            .as_array()?
            .iter()
            .filter_map(|v| {
                v["route_key"].as_str().filter(|k| !k.is_empty()).or_else(|| v["venue"].as_str())
            })
            .filter(|k| !k.is_empty())
            .map(str::to_string)
            .collect();
        (!keys.is_empty()).then_some(keys)
    }
}

/// The three `venue_check` dispositions a write reports, spelled ONCE.
/// [`Server::vet_commanded_venue`] writes one of them; it is the one field that says whether the
/// venue in `wire_command` was compared against anything at all.
///
/// ⚠ Reported on BOTH halves of the two-call gate — the preview payload ([`preview_of`]) and the
/// answer to the confirmed call ([`Server::call_tool`]'s write arm). The confirm half is not
/// decoration: `unverified` is an ALLOW, so an accepted write that stated nothing would leave the
/// record of the write that actually happened unable to say whether the gate ran.
///
/// `mounted` — the command names a venue and the node reports mounting it.
pub(super) const VENUE_CHECK_MOUNTED: &str = "mounted";
/// `none` — the command names NO venue THE NODE COULD MOUNT, so there is nothing to check. The
/// `market_exit` / `mass_cancel` "every engine" shape, and every venue-less verb.
///
/// ⚠ It also covers `delete_series`, whose selector DOES carry a `venue` — a STORE partition, which
/// is not a mount and which the node has no opinion about. Reporting `unverified` there would say a
/// check had been attempted and had failed, which is worse than saying there was none to make.
/// See [`VENUE_CHECK_MOUNTED`].
pub(super) const VENUE_CHECK_NONE: &str = "none";
/// `unverified` — the command names a venue and the node's mounted set could not be READ (no node,
/// no observe key, or the node is down). The command was NOT refused on an absence, and the
/// preview says so rather than presenting an unmade check as a passed one. See
/// [`VENUE_CHECK_MOUNTED`].
pub(super) const VENUE_CHECK_UNVERIFIED: &str = "unverified";

/// The venue a write command NAMES, or `None` when it names none.
///
/// ⚠ **Exhaustive on purpose.** A wildcard arm would answer `None` — "nothing to check" — for a
/// future venue-carrying variant, which is the gate silently not covering the verb that needed it.
/// The two strategy-level variants are matched even though this server serves no tool that builds
/// one, so a tool for them inherits [`Server::vet_commanded_venue`] the day it is added rather than
/// needing this function edited as well; both name a mount venue the node must already run, which
/// is the same question.
pub(super) fn commanded_venue(cmd: &WireCommand) -> Option<&str> {
    match cmd {
        WireCommand::Submit(o) => Some(o.venue.as_str()),
        WireCommand::Bracket(b) => Some(b.venue.as_str()),
        WireCommand::Flatten { venue, .. } => Some(venue.as_str()),
        // OPTIONAL by declaration, and an omitted one MEANS every engine — see
        // [`Server::vet_commanded_venue`]'s note on why that may not be refused.
        WireCommand::MassCancel { venue, .. } | WireCommand::MarketExit { venue, .. } => {
            venue.as_deref()
        }
        WireCommand::UpdateParams { venue, .. } | WireCommand::MountStrategy { venue, .. } => {
            Some(venue.as_str())
        }
        // Order-scoped by id, account-wide, or not a venue question at all.
        WireCommand::Cancel(_)
        | WireCommand::Modify { .. }
        | WireCommand::SetTradingState(_)
        | WireCommand::UnmountStrategy { .. }
        | WireCommand::SetSetting { .. } => None,
    }
}

/// WHICH ACCOUNT of [`commanded_venue`]'s venue this command names, if any — the second half of the
/// addressing pair [`venue_verdict`] judges.
///
/// ⚠ Deliberately SEPARATE from `commanded_venue` rather than folded into a route key there: the
/// gate needs to know whether an account was NAMED, not merely what key it resolves to. Those come
/// apart on exactly one value — `DEFAULT`, whose route key is the bare venue — and that is the case
/// the whole distinction exists for.
fn commanded_account(cmd: &WireCommand) -> Option<&str> {
    match cmd {
        WireCommand::Submit(o) => o.account.as_deref(),
        WireCommand::MassCancel { account, .. }
        | WireCommand::Flatten { account, .. }
        | WireCommand::MarketExit { account, .. }
        | WireCommand::MountStrategy { account, .. } => account.as_deref(),
        // No account field: order-scoped by id, or not an addressing question at all.
        WireCommand::Cancel(_)
        | WireCommand::Bracket(_)
        | WireCommand::Modify { .. }
        | WireCommand::SetTradingState(_)
        | WireCommand::UpdateParams { .. }
        | WireCommand::UnmountStrategy { .. }
        | WireCommand::SetSetting { .. } => None,
    }
}

/// Does this command's venue pass, and what should the preview report? PURE — the decision half of
/// [`Server::vet_commanded_venue`], which supplies both inputs.
///
/// `venue` is [`commanded_venue`]'s answer; `mounted` is the node's own reported set, `None`
/// meaning it could not be established. `Err` is the refusal TEXT, and it is written to be acted
/// on: it names the offending value, names what the node actually mounts, names the tool that
/// reports it, and states the mechanism — because "not mounted" without the routing consequence
/// reads as a naming quibble rather than as "this would have gone somewhere else".
pub(super) fn venue_verdict(
    venue: Option<&str>,
    account: Option<&str>,
    mounted: Option<&[String]>,
) -> Result<&'static str, String> {
    // A command that names NO venue is not a command with a missing one: `market_exit` and
    // `mass_cancel` declare the argument optional and MEAN "every engine" when it is omitted, and
    // `cancel_order` / `modify` / `set_trading_state` have no venue at all. A gate that fired on a
    // valid omission would break the panic button — a worse failure than the one being closed.
    let Some(venue) = venue else {
        return Ok(VENUE_CHECK_NONE);
    };
    let Some(mounted) = mounted else {
        return Ok(VENUE_CHECK_UNVERIFIED);
    };
    // EXACT, never case-folded — see [`Server::vet_commanded_venue`].
    //
    // ⚠ **THERE IS NO EXACT-MATCH ARM, and its absence is the fix rather than an omission.**
    // One stood here and answered `mounted` for a BARE VENUE on a multi-account node — because the
    // default account's route key IS the bare venue, so the string is genuinely in the published
    // set and the arm fired before anything counted. Measured:
    // `venue_verdict("binance", ["binance", "binance#ALT"])` returned `mounted`, which is the
    // misroute this gate exists to refuse, arriving through the fix for it.
    //
    // The CARRIER COUNT below needs no such arm, because it degenerates correctly on its own —
    // which is why one question answers all four cases instead of two questions answering two each:
    //
    //   `binance#ALT` on a two-account node  -> only `binance#ALT` is a key OF `binance#ALT`  -> 1
    //   `binance`     on a one-account node  -> only `binance`                                -> 1
    //   `binance`     on a two-account node  -> `binance` and `binance#ALT`                   -> 2
    //   `nosuchvenue` anywhere               -> nothing                                       -> 0
    // ⚠ **A NAMED ACCOUNT IS NEVER AMBIGUOUS, AND THAT INCLUDES `DEFAULT`.** This arm runs before
    // the carrier count on purpose: the count answers *"how many engines could the caller have
    // meant"*, and a caller who NAMED an account meant that one. Folding the two together is the
    // same defect the core's mount arm carries a warning about — an ABSENT account and an explicit
    // `DEFAULT` are different rows of the routing table, and `DEFAULT`'s route key is the BARE
    // VENUE, so a count taken without asking whether an account was named refuses the one caller
    // who said exactly what they meant.
    //
    // The question left is simply whether that route key is one this node runs, which is the same
    // question the not-mounted arm below asks — so a wrong label is refused by NAME rather than
    // reported as ambiguity.
    if let Some(account) = account {
        let label = vike_model::accounts::account_keys::parse_wire_account(account).map_err(|e| {
            format!(
                "account {account:?} is not a legal account name — {e}. Nothing was previewed, no \
                 preview_token was issued, and nothing was sent. Call node_snapshot and read \
                 `venues[].account`, or omit the field to name no account."
            )
        })?;
        let key = vike_model::accounts::account_keys::route_key_of(venue, &label);
        return if mounted.contains(&key) {
            Ok(VENUE_CHECK_MOUNTED)
        } else {
            Err(format!(
                "account {account:?} of venue {venue:?} is NOT MOUNTED on this node — its route \
                 key would be {key:?}, and this node mounts: {}. Nothing was previewed, no \
                 preview_token was issued, and nothing was sent. Call node_snapshot, read \
                 `venues[].route_key`, and re-issue naming one of them EXACTLY (the comparison is \
                 case-sensitive).",
                mounted.join(", ")
            ))
        };
    }
    // ⚠ **AMBIGUITY IS A REFUSAL, and it is NOT the same fact as absent evidence.** The caller
    // named NO account and a bare venue; count how many of this node's engines carry it.
    //
    // 0041's verdict 3 survives untouched one arm up: a node we could not READ degrades to
    // `unverified`, because the mounted set arrives over the very connection the write needs. But
    // *"the node told me there are three candidates"* is POSITIVE evidence of a problem, and a gate
    // that treated it identically to *"I could not read the node"* would be discarding the more
    // informative of the two answers.
    //
    // ⚠ And it refuses BEFORE a `preview_token` is minted — 0041's verdict 1 shape, and the
    // property `crates/vike-agent-eval/src/grade.rs`'s `NoPreviewTokenIssued` already grades: a
    // refusal that still handed back a token would have given away the second half of the two-call
    // gate. This function returning `Err` is what keeps the mint unreached.
    let carriers: Vec<&String> = mounted
        .iter()
        .filter(|m| vike_model::accounts::account_keys::label_of_route_key(venue, m).is_some())
        .collect();
    if carriers.len() > 1 {
        return Err(format!(
            "venue {venue:?} is AMBIGUOUS on this node — it mounts {} accounts of it ({}), and a \
             command naming the venue alone does not say which. Nothing was previewed, no \
             preview_token was issued, and nothing was sent. Re-issue naming the ACCOUNT: call \
             node_snapshot, read `venues[].route_key`, and use one of those EXACTLY. This is not a \
             node that failed to answer — it answered, and the answer was more than one.",
            carriers.len(),
            carriers.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    if carriers.len() == 1 {
        return Ok(VENUE_CHECK_MOUNTED);
    }
    Err(format!(
        "venue {venue:?} is NOT MOUNTED on this node — nothing was previewed, no preview_token was \
         issued, and nothing was sent. This node mounts: {}. Call node_snapshot, read \
         `venues[].venue`, and re-issue the command naming one of them EXACTLY (the comparison is \
         case-sensitive). Nothing further down the line would have caught this: a venue string the \
         node does not mount routes to no engine and falls back to the node's FIRST one, which is \
         not the account you named.",
        mounted.join(", ")
    ))
}
