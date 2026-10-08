//! `server`'s phase 2, the AUTHED SESSION: `serve_session` and the reply halves of two of its arms.
use super::{
    Accepted, AccountActor, AccountAdminSource, AccountRequest, Arc, CommandSink, ControlLimits,
    ControlLimitsConfig, LinkPolicy, NodeKeys, PublisherHandle, Request, Response, Scope,
    SettingsShowSource, SocketAddr, TcpStream, WireMountRow, WireStrategyStatus, accept_command,
    account_admission, account_refusal, bracket_refusal, command_kind, log_read_end,
    read_frame_raw, run_push_writer, tearsheet_reply, venue_refusal, write_all_flush, write_frame,
};

/// [`handle_connection`]'s phase 2: the authed session — request/response until the client
/// `Subscribe`s and this thread becomes the push writer, or a fault or a refusal closes it.
#[allow(clippy::too_many_arguments)] // see `handle_connection`'s note — it hands this function the
// whole per-connection input set it holds, minus what only the handshake needed.
pub(super) fn serve_session(
    mut stream: TcpStream,
    publisher: PublisherHandle,
    keys: &NodeKeys,
    commands: Option<CommandSink>,
    limits: ControlLimitsConfig,
    settings: Option<Arc<SettingsShowSource>>,
    accounts: Option<Arc<AccountAdminSource>>,
    scope: Scope,
    peer: Option<SocketAddr>,
    policy: LinkPolicy,
) {
    // WHO this connection authenticated AS — the stable, non-secret fingerprint of the key whose
    // mac verified ([`NodeKeys::key_id`]). Resolved ONCE here, immediately after the handshake,
    // because this is the only place the granted scope and the server's keys are both in hand; it
    // is what the change journal records as the actor, since the daemon authenticates a KEY and
    // there are no human accounts in this system. `None` is unreachable on this path (the scope was
    // granted, so its key is non-empty) and is carried honestly rather than unwrapped — an absent
    // key must record NO id, never an invented one.
    let key_id = keys.key_id(scope);
    // The peer as a STRING, rendered once — the change journal takes `Option<&str>`, and
    // re-rendering it per write would be a second spelling of the cell an audit record is read by.
    let peer_str = peer.map(|p| p.to_string());

    // Control-command limits (a no-op for an Observe peer, which never reaches the Command arm):
    // this connection's own token bucket over the server-lifetime config.
    let mut limits = ControlLimits::new(limits);
    // ONE-SHOT latch for the "this node cannot check where your command is going" warning — see its
    // emission in the `Request::Command` arm. Per CONNECTION, not per order, and not per process:
    // a new control link is a new operator session that has not been told.
    let mut unrouted_warned = false;

    // --- The request loop: authed Observe session, until Subscribe ---
    loop {
        let body = match read_frame_raw(&mut stream) {
            Ok(b) => b,
            Err(e) => {
                log_read_end(&e, peer);
                return;
            }
        };
        let request = match serde_json::from_slice::<Request>(&body) {
            Ok(r) => r,
            Err(e) => {
                // A well-framed but undecodable body is a bad request, not a bad connection.
                let _ = write_frame(
                    &mut stream,
                    &Response::Error(format!("unrecognized/undecodable request: {e}")),
                );
                continue;
            }
        };

        match request {
            // Transition to push mode — this thread becomes the writer for the rest of the session.
            Request::Subscribe { topics } => {
                // THE PUSH WRITE BOUND, applied at the exact transition it governs: from here on
                // this thread only writes, so neither read policy above bounds anything any more,
                // and without this a peer that keeps its socket open and stops reading parks the
                // thread in `write_all` for as long as it likes (`PUSH_WRITE_TIMEOUT`'s doc). Fatal
                // on failure for the same reason the authed-policy replacement is: carrying on
                // would run the writer unbounded, which is precisely the defect.
                if let Err(e) = stream.set_write_timeout(policy.push_write_timeout) {
                    tracing::warn!(
                        ?peer, error = %e,
                        "vike-tradehub observe: could not apply the push write bound, closing"
                    );
                    return;
                }
                tracing::info!(?peer, ?topics, "vike-tradehub observe: subscribed (push mode)");
                run_push_writer(&mut stream, &publisher, peer, policy);
                return;
            }
            // A point-in-time snapshot off the publisher's current cell.
            Request::Snapshot => {
                let frame = publisher.snapshot_frame_now();
                if let Err(e) = write_all_flush(&mut stream, &frame) {
                    tracing::warn!(?peer, error = %e, "vike-tradehub observe: snapshot write fault, closing");
                    return;
                }
            }
            Request::Ping => {
                if write_frame(&mut stream, &Response::Pong).is_err() {
                    return;
                }
            }
            // Control path (PR-12): only a Control-authenticated peer, on a node with a CommandSink,
            // may lower a command into the core. Every other case is refused, never folded.
            Request::Command { cmd: wire_cmd, reason } => {
                // (a) Scope gate: an Observe peer is read-only and can never command.
                if scope != Scope::Write {
                    let _ = write_frame(
                        &mut stream,
                        &Response::AuthDenied {
                            reason: "read-only: authenticated as Observe".into(),
                        },
                    );
                    continue;
                }
                // (b) Sink gate: control may be authenticated but not ENABLED on this node.
                let Some(sink) = &commands else {
                    let _ = write_frame(
                        &mut stream,
                        &Response::Error("control not enabled on this node".into()),
                    );
                    continue;
                };
                // (c) The SHARED acceptance path — the same `accept_command` the Telegram channel
                // calls: edge limits (rate token + notional cap), rationale sanitization, lowering,
                // the single-writer lane, and the audit record, in that ONE order for every
                // surface. This connection's own `limits` bucket and TCP `peer` are what make it
                // this surface's call; everything else is common by construction. `settings` is
                // the REQ-7 write lowering's source (the same boot-threaded handle the
                // `SettingsShow` arm reads) — a `SetSetting` on a source-less server refuses
                // inside, every other command ignores it.
                // (c.1) THE ENGINE ROSTER this command's address is checked against — read off the
                // publisher's snapshot cell PER COMMAND rather than captured at connect, because
                // this roster has exactly ONE transition, EMPTY -> PUBLISHED, and a per-command
                // read is what lets a connection opened BEFORE the core's first publish start
                // checking the moment it publishes instead of staying blind for as long as that
                // peer holds the socket. `Publisher::engine_venues`' own doc is the authority for
                // why EMPTY means "this core has not published yet" and never "this node runs no
                // engines". Operator cadence, one `Vec` per command, never the fold.
                //
                // ⚠ **This used to give a RUNTIME-MOUNT rationale — "a node can MOUNT an engine at
                // runtime (`WireCommand::MountStrategy`) and a roster frozen at handshake would
                // keep refusing a venue the node had since acquired" — and it was false in a way
                // that INVERTED the failure it named.** A runtime mount cannot add an engine:
                // `vike_core`'s `CoreThread::mount_strategy_runtime` refuses a mount whose route
                // key no engine ALREADY carries ("a mount cannot conjure one"), so this roster
                // never grows a venue after the first publish. And what a handshake-frozen roster
                // would actually cost is the OPPOSITE of over-refusal: frozen EMPTY on a peer that
                // connected before that first publish, it would check NOTHING for the life of the
                // socket — which is precisely the window the ⚠ warning below exists to announce.
                //
                // Written out rather than deleted, because the per-command read LOOKS like it is
                // defending against a roster that grows, and the next reader will re-derive the
                // same wrong reason from the same silence. (c.2) reached the true one first and
                // said it for both; this line now carries it too.
                let engines = publisher.engine_venues();
                // (c.2) …and the ROUTE-KEY roster the ACCOUNT gate is checked against, read off
                // the same cell PER COMMAND — for the reason (c.1) now gives as well, which this
                // line reached first and which is the true one for BOTH: this roster has exactly
                // ONE transition, EMPTY -> PUBLISHED. Nothing adds an engine at runtime
                // (`CoreThread::mount_strategy_runtime` refuses a mount whose route key no engine
                // carries — "a mount cannot conjure one"), so what a per-command read buys is that
                // a connection opened BEFORE the core's first publish starts checking the moment it
                // publishes, instead of staying blind for as long as that peer holds the socket.
                // A SECOND read rather than a second field of one: the two gates take different
                // slices because they answer different questions (`account_refusal`'s doc argues
                // it), and both are empty together because both project one `portfolio.venues`.
                let route_keys = publisher.engine_route_keys();
                // (c.3) …and the engine BLOCKS, the same cell a third time, for the one gate that
                // needs more than a key: a bracket's, which must know what its engine trades
                // (`bracket_refusal`). Read after the route keys, so the one EMPTY -> PUBLISHED
                // transition can only make this read the fuller of the two.
                let blocks = publisher.engine_blocks();
                // (c.4) …and the ORDER a `Modify` names, the same cell once more, for the one gate
                // that needs it: the notional ceiling, which sizes a `Modify` with the contract
                // multiplier of the instrument that order rests on (a bare coid reaches its engine
                // no other way). Empty for every other verb, and for an order the snapshot does not
                // hold — which the ceiling sizes at 1.0, as it always did. Read after the blocks,
                // so the one EMPTY -> PUBLISHED transition can only make it the fresher read.
                let orders = publisher.open_orders_named_by(&wire_cmd);
                // ⚠ ONCE PER CONNECTION, not per order: the pre-publish window in which this node
                // cannot check an address is exactly the state nobody noticed for as long as the
                // fallback was silent, so it says so — and it says it once, because a per-order
                // line on a busy control link is a line an operator scrolls past.
                if engines.is_empty() && !unrouted_warned {
                    unrouted_warned = true;
                    tracing::warn!(
                        peer = ?peer,
                        "node command routing UNCHECKED on this connection: the core has published \
                         no engine set yet, so a command naming a venue this node does not run \
                         cannot be refused and falls through to the PRIMARY engine (the historical \
                         behaviour). It resolves itself on this core's first publish"
                    );
                }
                match accept_command(
                    wire_cmd,
                    reason.as_deref(),
                    &mut limits,
                    sink,
                    settings.as_deref(),
                    &engines,
                    &route_keys,
                    &blocks,
                    &orders,
                    peer,
                    key_id.as_deref(),
                ) {
                    Ok(Accepted::Coid(coid)) => {
                        if write_frame(&mut stream, &Response::Ack { coid }).is_err() {
                            return;
                        }
                    }
                    // The REQ-7 settings write: its acceptance is not an Ack (nothing entered
                    // the core, no coid) — the reply carries the restart-to-apply signal.
                    Ok(Accepted::SettingsWritten { restart_required }) => {
                        if write_frame(&mut stream, &Response::SettingsWritten { restart_required })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = write_frame(&mut stream, &Response::Error(e.message()));
                        // Only a `Gone` (the core's lane closed) ends the connection — a refusal or
                        // a transient `Busy` leaves this peer free to try again, exactly as before.
                        if e.is_fatal() {
                            return;
                        }
                    }
                }
            }
            // Preview path (v3): a Control peer asks what a command WOULD do WITHOUT executing it.
            // Server-authoritative dry-run — READ-ONLY: nothing is lowered, nothing reaches the core,
            // no order is placed, no rate token is consumed, and it is NOT audit-logged as an executed
            // command. Four steps are evaluated, the ones `accept_command` runs before it lowers: the
            // edge policy (the notional cap), the venue gate, the account gate and a bracket's own
            // verdict — see the comment above the chain below.
            Request::Preview(wire_cmd) => {
                // Scope gate: an Observe peer is read-only and can never command — nor preview a
                // command (same gate as `Request::Command`).
                if scope != Scope::Write {
                    let _ = write_frame(
                        &mut stream,
                        &Response::AuthDenied {
                            reason: "read-only: authenticated as Observe".into(),
                        },
                    );
                    continue;
                }
                // The dry-run answers the SAME routing verdict the real command would get, and
                // must: a preview whose whole job is "what would this do" but that stays silent
                // about the command landing on a different book than the one it names is worse
                // than no preview. Ordered exactly as `accept_command` reaches them (the edge gate
                // first, then the venue address, then the ACCOUNT within it, then a BRACKET's own
                // verdict, `bracket_refusal` — the very call `accept_command` makes at its step
                // 1d), so the two cannot report different reasons for one frame — and the account
                // gate is here for the same reason the venue one is: a preview that stayed silent
                // about the named BOOK not existing would hand the operator a dry run whose real
                // send is a refusal. The fourth step is the bracket's own twin of that argument: an
                // inverted bracket, or one its engine cannot hold, must not preview as accepted when
                // its send is refused. Every other refusal `lower_command` makes is a malformed
                // frame, not a verdict, and is not previewed. Read-only like the rest of this arm —
                // every step is pure, and each roster read is a snapshot-cell load. The blocks are
                // read ONCE, ahead of the chain: the edge gate sizes with the addressed engine's
                // contract multiplier and the bracket verdict reads what it trades, and the two
                // must see the same roster. The order a `Modify` names is read beside them, for the
                // edge gate, so the preview sizes a `Modify` exactly as the send will.
                let blocks = publisher.engine_blocks();
                let orders = publisher.open_orders_named_by(&wire_cmd);
                let reason = limits
                    .preview_vet(&wire_cmd, &blocks, &orders)
                    .or_else(|| venue_refusal(&wire_cmd, &publisher.engine_venues()))
                    .or_else(|| account_refusal(&wire_cmd, &publisher.engine_route_keys()))
                    .or_else(|| bracket_refusal(&wire_cmd, &blocks));
                tracing::debug!(
                    ?peer,
                    kind = command_kind(&wire_cmd),
                    accepted = reason.is_none(),
                    "vike-tradehub control: preview (dry-run, nothing executed)"
                );
                if write_frame(
                    &mut stream,
                    &Response::Preview { accepted: reason.is_none(), reason },
                )
                .is_err()
                {
                    return;
                }
            }
            // STRATEGY-level read (split-plane B4): what is this node running? Post-auth under
            // EITHER scope — it is read-only (identity + rendered params, data every pushed
            // snapshot already carries), so Observe suffices, exactly like `Snapshot`. Answered
            // from the publisher's process-static identity block plus its per-mount rows
            // (split-plane I10): a `[[mounts]]` daemon publishes one `WireMountRow` per mount
            // (`publish::spawn_with_mounts`); a publisher spawned without rows (the mount-less
            // `publish::spawn`, every pre-I10 caller) falls back to deriving the single row from
            // the identity block — byte-identical to the pre-I10 answer.
            //
            // ⚠ THE FALLBACK'S `live: id.live` IS THE WEAKER ANSWER, and it is kept only because
            // an identity-only publisher has no better one. `WireNodeIdentity::live` is the
            // PROCESS-wide `flags.tradehub_live` gate; `WireMountRow::live` is documented as "this
            // MOUNT trades LIVE", a per-VENUE question the gate cannot answer — a mount whose venue
            // has no credentials, or one declared `data_only = true`, is gated LIVE and executes on
            // the paper book. `vike-tradehub`'s own daemon no longer takes this path at all: `main`
            // publishes a row per mount (single-mount daemons included) whose `live` comes from
            // `build_node`'s arming record. Do not "simplify" by dropping those rows again.
            Request::StrategyStatus => {
                let resp = strategy_status_response(&publisher);
                if write_frame(&mut stream, &resp).is_err() {
                    return;
                }
            }
            // **ACCOUNT ADMINISTRATION** (`docs/decisions/0065`): the settings database's `account`
            // table, plus the one verb on this wire that carries a credential VALUE.
            //
            // The three parts of the barrier meet here, and each is a different KIND of thing:
            //
            //   1. STRUCTURAL — `accounts` is `None` on every box that has not DECLARED one, so
            //      there is no writer in this process to call and the frame is refused because
            //      there is nothing to refuse WITH. `served_features` withheld the capability
            //      string too, so a conforming client sent nothing; what reaches this arm with
            //      `None` is a client that skipped the negotiation.
            //   2. AUTHORIZATION — `Scope::Account`, a THIRD key. Checked here rather than at the
            //      handshake because the handshake grants a scope and this arm is where a scope
            //      becomes a capability; a Control peer that reached this frame is refused by name.
            //   3. CONFIDENTIALITY — decided at BOOT and unknowable here (`AccountAdminSource`'s
            //      own doc measures why the listener and the peer both answer wrongly), so it is
            //      carried on the handle rather than asked at the frame.
            //
            // ⚠ The refusals are audited NOWHERE, which is this daemon's uniform rule for every
            // refused command (`accept_command`'s `SetSetting` arm declares the same thing and
            // argues it). The ACCEPTED writes journal themselves, inside `apply`.
            Request::Account(req) => {
                let resp =
                    account_response(&accounts, scope, &mut limits, peer, &peer_str, &key_id, &req);
                if write_frame(&mut stream, &resp).is_err() {
                    return;
                }
            }
            // SETTINGS read (split-plane REQ-7, read half): the node's effective settings-file
            // rows, from the boot-threaded [`SettingsShowSource`]. Post-auth under EITHER scope —
            // Observe suffices, and the argument is [`SettingsShowSource`]'s doc: the payload is
            // the FILES half only, redacted ON CONSTRUCTION in the shared `vike_config::show`
            // builder (the env-registry half, whose credential grid discloses `<set>`/`<unset>`
            // per credential key, is never served), leaving paths/addresses/flags — the
            // disclosure class an Observe peer already reads off the identity block. Read-only ⇒
            // no audit record, exactly like `Snapshot`/`StrategyStatus`.
            Request::SettingsShow => {
                let resp = match &settings {
                    Some(src) => src.response(),
                    // A server constructed without a source (possible through `serve(.., None)`;
                    // the shipped daemon always passes one) has no settings truth to report — an
                    // honest error, never a fabricated empty table (the `StrategyStatus`
                    // identity-less shape).
                    None => Response::Error(
                        "settings unavailable: this node was started without a settings source"
                            .into(),
                    ),
                };
                if write_frame(&mut stream, &resp).is_err() {
                    return;
                }
            }
            // DIRECTORY read: the settings database's venues and active accounts. Observe
            // suffices because the owner ruled on 2026-09-30 that the observe key may read the
            // account list — NOT because names are left out: the reply still says which venues
            // hold which tiers of account (`SettingsShowSource::directory` argues the scope), and
            // carries no credential value, key name or `credential` row. Read-only ⇒ no audit
            // record, like `Snapshot`/`SettingsShow`.
            Request::Directory => {
                let resp = match &settings {
                    Some(src) => src.directory(),
                    None => Response::Error(
                        "directory unavailable: this node was started without a settings source"
                            .into(),
                    ),
                };
                if write_frame(&mut stream, &resp).is_err() {
                    return;
                }
            }
            // LIVE-JOURNAL REPORT read (ruling 16 of the datahub market-data wire design). Post-auth
            // under EITHER scope — Observe suffices, exactly like `Snapshot`/`StrategyStatus`: the
            // payload is a few dozen aggregate numbers over fills this node's snapshot already
            // publishes positions and PnL for, it executes nothing, and so it takes no audit record.
            //
            // ⚠ **This arm used to be a written IOU** — a named refusal, paired with a
            // `served_features` that deliberately withheld `FEATURE_TEARSHEET` so a conforming
            // client refused the verb client-side and nothing reached here. Both halves flip
            // together or neither does: the capability now rides the advertisement (see
            // `served_features`) and [`tearsheet_reply`] is the renderer behind it. What did NOT
            // change is the honesty rule the IOU followed — every way this can fail (no settings
            // source, no journal enabled, an unreadable journal) answers a `Response::Error` naming
            // the cause, never a fabricated empty tearsheet.
            //
            // The JOURNAL does not cross the wire, the ANSWER does — `Request::Tearsheet`'s own doc
            // carries that compute-to-data argument, and [`tearsheet_reply`] carries which journal
            // is read and the one rung this build cannot see.
            Request::Tearsheet { seed, periods_per_year } => {
                let resp = tearsheet_reply(settings.as_deref(), seed, periods_per_year);
                if write_frame(&mut stream, &resp).is_err() {
                    return;
                }
            }
            // Handshake verbs after AuthOk are a protocol error, but not fatal.
            Request::Hello { .. } | Request::Auth { .. } => {
                let _ = write_frame(
                    &mut stream,
                    &Response::Error("already authenticated; handshake is complete".into()),
                );
            }
        }
    }
}

/// The [`Request::StrategyStatus`] reply, composed from the publisher alone — the reply half of that
/// arm in [`serve_session`], split out so the arm reads as one request, one reply. The arm's own
/// comment (what the fallback row and the live overlay each argue) stays above it.
fn strategy_status_response(publisher: &PublisherHandle) -> Response {
    match publisher.identity() {
        Some(id) => {
            let mut mounts = publisher.mounts();
            if mounts.is_empty() {
                mounts = vec![WireMountRow {
                    strategy: id.strategy.clone(),
                    params: id.params.clone(),
                    live: id.live,
                    venue: String::new(),
                    symbol: String::new(),
                    interval: String::new(),
                    typed_params: None,
                    // ⚠ `None` and it cannot be otherwise HERE: this arm synthesises a
                    // row from the process-static IDENTITY BLOCK, which carries a
                    // strategy name and a params string and no mount config at all.
                    // The comment above says this daemon no longer takes this path —
                    // `main` publishes a row per mount, and THOSE carry the class from
                    // the profile. This is the old-shape fallback, so its `None` means
                    // "this row was derived, not published", which is the same thing it
                    // already means for the three empty addressing fields above.
                    asset_class: None,
                }];
            }
            // The LIVE overlay (`FEATURE_STRATEGY_PARAMS`): the rows above are the
            // process-static boot block, so their key is empty and their `params`
            // string is whatever the profile rendered at boot. The core's own snapshot
            // is the only thing that knows what each mount holds NOW, so read it here
            // and write the addressing key + typed params onto the rows, by MOUNT
            // ORDER — the one alignment both sides share (`publish::live_mount_params`
            // filters the residual row out precisely so this index means the same
            // thing on both sides).
            //
            // ⚠ OVERLAY, never a replacement: a row the snapshot cannot match (the
            // identity fallback above, a core whose mount count disagrees with the boot
            // block's, a mid-remount read) KEEPS its present shape — empty key, `None`
            // params — rather than being dropped. A status that omits a mount is worse
            // than one that admits it cannot type it: the omission reads as "that mount
            // is gone", which is a claim about a LIVE book.
            for (row, (venue, symbol, interval, typed)) in
                mounts.iter_mut().zip(publisher.live_mount_params())
            {
                row.venue = venue;
                row.symbol = symbol;
                row.interval = interval;
                row.typed_params = typed;
            }
            Response::StrategyStatus(Box::new(WireStrategyStatus {
                effective_params: id.params.clone(),
                identity: id,
                mounts,
            }))
        }
        // An identity-less publisher (possible through `publish::spawn(.., None)`;
        // the shipped daemon always passes an identity) has no mounted truth to
        // report — an honest error, never a fabricated empty status.
        None => Response::Error(
            "strategy status unavailable: this node publishes no identity block".into(),
        ),
    }
}

/// The [`Request::Account`] reply — admission first ([`account_admission`]), then the rate token,
/// then the write — the reply half of that arm in [`serve_session`], split out so the arm reads as
/// one request, one reply. The three parts of the barrier that meet at that arm are argued in the
/// comment that stays above it.
fn account_response(
    accounts: &Option<Arc<AccountAdminSource>>,
    scope: Scope,
    limits: &mut ControlLimits,
    peer: Option<SocketAddr>,
    peer_str: &Option<String>,
    key_id: &Option<String>,
    req: &AccountRequest,
) -> Response {
    // ⚠ The decision is [`account_admission`]'s, not this arm's — see its doc for the
    // mutation that measured what an in-arm `match` cost. This tuple exists only to
    // bind `src` once admission has already said yes.
    match (accounts, account_admission(accounts.is_some(), scope)) {
        (Some(src), Ok(())) => {
            // ⚠ `req.verb.word()`, never `{req:?}` and never the request. The `Debug`
            // impl on `AccountRequest` redacts — but a redacting impl is a PROMISE, and
            // `word()` cannot carry a value at all. This is the line that says an
            // account verb happened on a box where the audit trail is the only record.
            tracing::warn!(
                ?peer,
                verb = req.verb.word(),
                barrier = src.barrier.as_str(),
                "vike-tradehub node: ACCOUNT ADMIN verb accepted — this peer may write \
                 the credential store"
            );
            // The rate token, consumed for an account verb exactly as it is for every
            // control command: a flood is still a flood, and this surface opens a
            // SQLite transaction per frame. The notional cap is n/a — an account verb
            // is not an order and carries no qty×price to size, the `SetSetting`
            // vetting decision verbatim.
            match limits.vet_rate() {
                Some(refusal) => Response::Error(refusal),
                None => {
                    let actor = AccountActor {
                        peer: peer_str.as_deref(),
                        scope: "admin",
                        key_id: key_id.as_deref(),
                    };
                    match src.apply(req, &actor) {
                        Ok(r) => r,
                        Err(reason) => Response::Error(reason),
                    }
                }
            }
        }
        // Both refusals — absent capability and wrong scope — carry the message
        // `account_admission` composed, so there is ONE authority for each string.
        (_, Err(refusal)) => Response::Error(refusal),
        // Unreachable by construction: admission is handed `accounts.is_some()`, so an
        // `Ok` with a `None` source cannot occur. A daemon REFUSES rather than panics
        // (`docs/decisions/0013`), and the message says the impossible thing happened
        // rather than pretending the capability is merely unarmed.
        (None, Ok(())) => Response::Error(
            "account administration: internal inconsistency — admission passed while \
             this node holds no account writer. Nothing was written. Please report \
             this, it indicates a defect rather than a configuration problem."
                .into(),
        ),
    }
}
