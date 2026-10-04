//! `server::handshake` — what an UNAUTHENTICATED peer meets: the `Hello` -> `Welcome{ nonce }` ->
//! `Auth` -> verify exchange ([`run_handshake`]), the closed-gate check on the scope a peer claims
//! ([`scope_admission`]), the per-connection challenge nonce and the capability advertisement the
//! `Welcome` carries ([`served_features`]).
//!
//! Split out of `server.rs` as a pure move. The frame ceiling a peer is read under before it has
//! authenticated (`HANDSHAKE_MAX_FRAME_LEN`) and the read-end log line stay in the parent, beside
//! the other bounds `serve` holds up.

use std::net::TcpStream;

use rand::Rng; // rand 0.10 core trait — provides `fill_bytes` (formerly `RngCore` in rand 0.8)
use vike_tradehub_client::auth::{self, NodeKeys};
use vike_tradehub_client::proto::{
    FEATURE_ACCOUNT_ROUTING, FEATURE_ACCOUNT_SCOPED_REDUCE, FEATURE_ACCOUNT_SCOPED_SUBMIT,
    FEATURE_ACCOUNT_VERBS, FEATURE_BRACKET, FEATURE_DIRECTORY, FEATURE_MOUNT_ACCOUNT,
    FEATURE_MOUNT_CLASS, FEATURE_MOUNT_VERBS, FEATURE_OBSERVE_HEARTBEAT, FEATURE_SETTINGS_SHOW,
    FEATURE_SETTINGS_WRITE, FEATURE_STRATEGY_PARAMS, FEATURE_STRATEGY_VERBS, FEATURE_TEARSHEET,
    FEATURE_VENUE_ROUTING, NODE_PROTO_VERSION, Request, Response, Scope, datahub_feature,
    read_frame_raw_capped, write_frame,
};

use super::{HANDSHAKE_MAX_FRAME_LEN, log_read_end};

/// **Does this node hold a key for the scope this peer is CLAIMING?** — the handshake's
/// closed-gate check, split out so it is decidable without a socket.
///
/// ⚠ **Extracted for exactly the reason [`super::accounts::account_admission`] was**, and that function's doc
/// carries the measurement: a decision living inside [`run_handshake`] cannot be driven by any
/// test, because `run_handshake` takes a `&mut TcpStream` and there is no seam to hand it
/// anything else. Before this split, `crates/vike-tradehub/tests/` contained no test naming
/// `run_handshake` or `Scope::Account` at all — so the ONE check standing between a peer's CLAIM of
/// admin and the account plane was covered by nothing, on the binary that signs orders.
///
/// `docs/decisions/0070` names this the FIRST of the account boundary's two layers: the MAC
/// verified below folds the scope tag, so a peer cannot claim `Admin` without the admin key — and
/// this gate refuses the claim one step earlier, without consulting a key at all, on a node that
/// holds none. That ordering is the "closed gate" property: an absent key is not an empty key to
/// compare against, it is a scope that cannot be reached.
///
/// Returns the AuthDenied `reason` verbatim on refusal, so the wire text has one home.
pub(super) fn scope_admission(scope: Scope, keys: &NodeKeys) -> Result<(), &'static str> {
    match scope {
        Scope::Account if !keys.has(Scope::Account) => {
            Err("account administration is not armed on this node")
        }
        Scope::Write if !keys.has(Scope::Write) => Err("control disabled on this node"),
        _ => Ok(()),
    }
}

/// Mint a fresh 32-byte challenge nonce for one connection (CSPRNG per connection — the anti-replay
/// property: a mac captured off one connection's nonce fails against another's).
fn fresh_nonce() -> [u8; 32] {
    let mut nonce = [0u8; 32];
    rand::rng().fill_bytes(&mut nonce);
    nonce
}

/// The result of the handshake phase.
pub(super) enum HandshakeOutcome {
    /// The client authenticated under this [`Scope`]; proceed to the session with it.
    Authed(Scope),
    /// The handshake was refused (or a transport error) — the connection must close.
    Closed,
}

/// Run the `Hello` -> `Welcome{ nonce }` -> `Auth` -> verify handshake. Verifies the presented mac
/// against the REQUESTED scope's key (scope-generic): [`Scope::Read`] is granted whenever its key
/// verifies; [`Scope::Write`] additionally requires the node to HOLD a control key (absent ⇒
/// control disabled ⇒ refused before any key is consulted). Any session verb arriving before auth is
/// refused too (an unauthenticated `Subscribe` never registers).
pub(super) fn run_handshake(
    stream: &mut TcpStream,
    keys: &NodeKeys,
    peer: Option<std::net::SocketAddr>,
    datahub_advertise: Option<&str>,
    // Whether this node holds an `AccountAdminSource` — the ONE conditional entry in
    // `served_features`. See that function and `vike_tradehub_client::proto::FEATURE_ACCOUNT_VERBS`.
    account_admin: bool,
) -> HandshakeOutcome {
    // Read the opener; it MUST be Hello. PRE-AUTH, so it is read under the small
    // `HANDSHAKE_MAX_FRAME_LEN` ceiling rather than the 64 MiB `MAX_FRAME_LEN` — an unauthenticated
    // peer must not be able to name an allocation size.
    let body = match read_frame_raw_capped(stream, HANDSHAKE_MAX_FRAME_LEN) {
        Ok(b) => b,
        Err(e) => {
            log_read_end(&e, peer);
            return HandshakeOutcome::Closed;
        }
    };
    let opener = match serde_json::from_slice::<Request>(&body) {
        Ok(r) => r,
        Err(_) => {
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "expected Hello (undecodable request)".into() },
            );
            return HandshakeOutcome::Closed;
        }
    };
    let client_version = match opener {
        Request::Hello { proto_version } => proto_version,
        // Any other verb before the handshake completes is unauthenticated — refuse it. This is where
        // an unauthenticated Subscribe/Snapshot/Command is denied and the connection dropped.
        _ => {
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "not authenticated: expected Hello first".into() },
            );
            return HandshakeOutcome::Closed;
        }
    };

    // Send the challenge. We always answer Hello with our version + a fresh nonce; a client on a
    // different protocol version cannot forge a valid mac anyway (the version is signed), so a skew
    // fails cleanly at verify rather than needing a special branch here.
    let nonce = fresh_nonce();
    if write_frame(
        stream,
        &Response::Welcome {
            proto_version: NODE_PROTO_VERSION,
            nonce,
            features: served_features(datahub_advertise, account_admin),
        },
    )
    .is_err()
    {
        return HandshakeOutcome::Closed;
    }
    if client_version != NODE_PROTO_VERSION {
        tracing::debug!(
            ?peer,
            client_version,
            server_version = NODE_PROTO_VERSION,
            "vike-tradehub observe: client protocol version differs; auth will fail on the signed version"
        );
    }

    // Read the Auth answer — still PRE-AUTH, same small ceiling (a scope plus a 32-byte mac).
    let body2 = match read_frame_raw_capped(stream, HANDSHAKE_MAX_FRAME_LEN) {
        Ok(b) => b,
        Err(e) => {
            log_read_end(&e, peer);
            return HandshakeOutcome::Closed;
        }
    };
    let (scope, mac) = match serde_json::from_slice::<Request>(&body2) {
        Ok(Request::Auth { scope, mac }) => (scope, mac),
        Ok(_) => {
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "expected Auth after Welcome".into() },
            );
            return HandshakeOutcome::Closed;
        }
        Err(_) => {
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "expected Auth (undecodable request)".into() },
            );
            return HandshakeOutcome::Closed;
        }
    };

    // Control must be ENABLED on this node: a `Control` request on a node with no control key is
    // refused WITHOUT consulting it — the closed-gate shape the observe path uses for an absent key.
    // (The `CommandSink` presence is enforced per-command in `handle_connection`; the key is the auth
    // gate here.) When `VIKE_TRADEHUB_CONTROL` is off the daemon zeroes the control key, so this is
    // also the belt to that suspenders.
    // ⚠ The ADMIN gate, and it is the SAME closed-gate shape one rung up: a node that holds no
    // admin key refuses that scope WITHOUT consulting a key. On this surface it is also the
    // belt to a suspenders the BINARY already fastened — `account_admin_source` reads
    // `VIKE_TRADEHUB_ADMIN_KEY` only when the declaration armed the capability, so an
    // undeclared box reaches `serve` with an admin key that is EMPTY however its store is filled.
    // ⚠ The DECISION is [`scope_admission`]'s; these two arms keep the logging and the frame,
    // which is what a socket-less test cannot observe anyway. Kept as two blocks rather than one
    // because the two refusals log DIFFERENT sentences and a post-incident review searches for
    // them by text — see each `warn!` below.
    if scope == Scope::Account
        && let Err(reason) = scope_admission(scope, keys)
    {
        // WARN for `Control`'s reason verbatim: at the shipped `VIKE_LOG_FILE_LEVEL=warn` an
        // `info` refusal never reaches the log file, and *somebody presented an Admin handshake on
        // a node that has no admin capability* is exactly the sentence a post-incident review
        // needs to find there.
        tracing::warn!(
            ?peer,
            "vike-tradehub node: ADMIN auth refused — account administration is not armed on \
             this node"
        );
        let _ = write_frame(stream, &Response::AuthDenied { reason: reason.into() });
        return HandshakeOutcome::Closed;
    }
    if scope == Scope::Write
        && let Err(reason) = scope_admission(scope, keys)
    {
        // WARN, not info, and the level is the whole point: `deploy/vike-tradehub.service` sets
        // `Environment=VIKE_LOG_FILE_LEVEL=warn`, so an `info` refusal never reaches the log file
        // on a live box. `crate::audit::FILE_PIN` pins the ACCEPTED half of the control trail onto
        // disk; without this the file would hold a complete record of every command that was taken
        // and no evidence whatever that anyone was ever turned away — a shape that is worse than
        // the old one, because the file now LOOKS authoritative. A refused handshake is also a
        // fault rather than an ordinary event, which is the argument the audit line itself cannot
        // make (see `crate::audit::FILE_PIN`'s doc for why THAT one stayed at `info` and took a
        // pin instead). Rate: one line per refused handshake, bounded by the connection rate.
        tracing::warn!(
            ?peer,
            "vike-tradehub node: control auth refused — control disabled on this node"
        );
        let _ = write_frame(stream, &Response::AuthDenied { reason: reason.into() });
        return HandshakeOutcome::Closed;
    }

    // Verify the mac against the REQUESTED scope's key (constant-time). An absent key is a closed gate
    // — `verify` against an empty key never accepts a real mac — so `keys.has(scope)` guards it.
    let key = keys.key_for(scope);
    let mac_ok = keys.has(scope) && auth::verify(key, &nonce, NODE_PROTO_VERSION, scope, &mac);
    if mac_ok {
        if write_frame(stream, &Response::AuthOk { scope }).is_err() {
            return HandshakeOutcome::Closed;
        }
        tracing::info!(?peer, ?scope, "vike-tradehub node: authenticated");
        HandshakeOutcome::Authed(scope)
    } else {
        // WARN for the same reason as the refusal above: at the shipped `VIKE_LOG_FILE_LEVEL=warn`
        // an `info` line never reaches the file, and "somebody presented a bad mac for the Control
        // scope, repeatedly" is exactly the sentence a post-incident review needs to find there.
        tracing::warn!(?peer, ?scope, "vike-tradehub node: auth denied (bad mac / no key)");
        let _ = write_frame(stream, &Response::AuthDenied { reason: "bad mac".into() });
        HandshakeOutcome::Closed
    }
}

/// The verbs this node server answers, advertised in `Welcome.features`. (Control is gated by the
/// server's `NodeKeys`/`CommandSink`, not by an advertised feature string, so it stays off this list.)
///
/// `datahub_advertise` is the REQ-2 advertisement — `config.datahub_advertise_addr`,
/// resolved by the daemon binary (the `server` module reads no settings; audit F13): `Some(addr)` appends
/// the ONE value-carrying entry, `datahub=<addr>` (spelled by
/// `vike_tradehub_client::proto::datahub_feature`, the round-trip authority), naming where a
/// client should DIAL the datahub this backend fronts. `None` — the default, and every test
/// caller — advertises nothing: the list is byte-identical to the pre-REQ-2 one.
pub(super) fn served_features(datahub_advertise: Option<&str>, account_admin: bool) -> Vec<String> {
    let mut features = vec![
        "observe".to_string(),
        "subscribe".to_string(),
        "snapshot".to_string(),
        // The server-authoritative dry-run verb (v3): a Control peer can ask what a command WOULD do
        // without executing it. Advertised (unlike control itself) because it is read-only.
        "preview".to_string(),
        // The STRATEGY verbs (split-plane B4): the `StrategyStatus` read + the `UpdateParams`
        // command. Advertised — rather than version-bumped — because `NODE_PROTO_VERSION` is folded
        // into the signed auth MAC (a bump breaks the handshake against every running node); a
        // client that does not see this string refuses the verbs CLIENT-side, sending nothing.
        FEATURE_STRATEGY_VERBS.to_string(),
        // The SETTINGS read verb (split-plane REQ-7, read half): `SettingsShow`. Same
        // feature-not-version design as the strategy verbs. Advertised unconditionally — a node
        // started without a [`super::settings::SettingsShowSource`] still decodes the request and answers an
        // honest `Response::Error`, which is a better answer than a capability that flickers
        // with construction details (and mirrors `StrategyStatus` on an identity-less publisher).
        FEATURE_SETTINGS_SHOW.to_string(),
        // The DIRECTORY read: `Directory`. Advertised unconditionally, like its settings sibling:
        // a node started without a settings source still decodes the request and answers an
        // honest `Response::Error`.
        FEATURE_DIRECTORY.to_string(),
        // The SETTINGS WRITE verb (split-plane REQ-7, write half): `WireCommand::SetSetting`.
        // Its OWN capability (a read-half node advertises `settings-show` yet cannot decode the
        // variant — the mount-verbs argument). Advertised unconditionally, like its read
        // sibling: a server started without a settings source still decodes the command and
        // refuses it honestly inside `accept_command`.
        FEATURE_SETTINGS_WRITE.to_string(),
        // The runtime MOUNT verbs (split-plane B5): `MountStrategy`/`UnmountStrategy`. A SEPARATE
        // capability from `strategy-verbs` — a B4-era node advertises that string yet cannot
        // decode these variants, so the new verbs need their own (see `FEATURE_MOUNT_VERBS`).
        FEATURE_MOUNT_VERBS.to_string(),
        // ...and that verb's ACCOUNT field, which needs a capability of its OWN rather than riding
        // the line above it. The line above is a DECODE claim — a B4-era node cannot read the
        // variant and says so. This one guards the opposite failure: a mount-verbs node from the
        // stage in between decodes the frame perfectly, `#[serde(default)]` turns the field it
        // never heard of into `account: None`, and the mount lands on the venue's default account
        // while the operator reads a normal acknowledgement. Advertised unconditionally — the field
        // and the arm that reads it ship together, so it is a property of the BUILD.
        FEATURE_MOUNT_ACCOUNT.to_string(),
        // The observe HEARTBEAT (`run_push_writer`): this node writes an idle `Response::Pong` on
        // a subscribed stream that has had nothing to say for `liveness::OBSERVE_HEARTBEAT`.
        // Unlike every string above it this advertises no VERB — nothing new may be sent — it
        // licenses a client-side DEADLINE, so a client may treat silence as death. Advertised
        // unconditionally because every connection this server serves is heartbeaten: the
        // capability is a property of the BUILD, not of a construction detail (the
        // `settings-show` argument, and the same reason the `LinkPolicy` seam is test-only —
        // a node that could be configured not to heartbeat while still advertising it would be
        // handing its clients a false promise).
        FEATURE_OBSERVE_HEARTBEAT.to_string(),
        // The STRUCTURED live-params read: this node's `WireMountRow`s carry the mount's addressing
        // key and its typed params. Its OWN capability rather than riding `strategy-verbs`, and one
        // rung sharper than the mount verbs' reason: a node predating it ANSWERS `StrategyStatus`
        // with rows whose new fields `#[serde(default)]` fills in as empty, which a client cannot
        // tell from an honest "this mount publishes none" — see `FEATURE_STRATEGY_PARAMS`.
        // Advertised UNCONDITIONALLY, the `settings-show` argument verbatim: a node whose mounts all
        // answer `typed_params: None` is still answering honestly, and a capability that flickered
        // with a construction detail (an identity-less publisher, a mount-less core) would be worse
        // than one that always means "this build can answer the question".
        FEATURE_STRATEGY_PARAMS.to_string(),
        // WHAT PRODUCT each mount trades: this node's `WireMountRow`s carry `asset_class`, the
        // stored word of a `vike_model::AssetClass`. `docs/decisions/0061`'s Phase 5 made a mount
        // NAME its class — `NOT NULL` in the settings database, refused at store time if absent —
        // and until this string the value was visible to the database alone.
        //
        // Its own capability for the `strategy-params` reason, one rung sharper again: a `None`
        // here has THREE sources and `#[serde(default)]` flattens two of them. A node predating
        // this cannot carry the field; a CURRENT node whose mount is still TOML-backed honestly has
        // no class, because `config::MountCfg`'s field is an `Option` and that asymmetry IS the
        // migration 0061 describes. Without the string a client reports the second as the first and
        // sends an operator to upgrade a daemon that is already current.
        //
        // Advertised UNCONDITIONALLY, the `strategy-params` argument verbatim: a node whose mounts
        // all answer `None` is still answering honestly, and a capability that flickered with a
        // deployment's migration state would mean "some mount here happens to be migrated" rather
        // than "this build can answer the question".
        FEATURE_MOUNT_CLASS.to_string(),
        // ROUTING BY THE ADDRESSED VENUE (`venue_refusal`): this node refuses a command naming a
        // venue it runs no engine for instead of applying it to its primary engine. Advertised
        // UNCONDITIONALLY, and the argument is one rung past `settings-show`'s: this capability is
        // a property of the BUILD — every command on this server goes through `accept_command`,
        // which applies the gate — and the only state in which the gate can answer nothing is the
        // window before the core's first publish, which is transient and self-resolving rather
        // than a configuration. A capability that flickered off during that window would be worse
        // than one that always means "this build checks the address": a client would see it absent
        // at connect, fall back to its own venue restriction for the whole session, and refuse
        // venues the node routes perfectly well.
        //
        // ⚠ It licenses a client-side LOOSENING, not a verb. A client that does NOT see it must
        // send only venues it can positively see in `WireSnapshot::venues`; seeing it means the
        // node will say no rather than misroute, so the client may offer the operator every venue
        // and let the node answer. `FEATURE_VENUE_ROUTING`'s own doc carries the direction.
        FEATURE_VENUE_ROUTING.to_string(),
        // ACCOUNT ROUTING: this node routes an order-carrying command onto the account its wire
        // payload names, instead of always the venue's default engine — the field
        // `crates/vike-core/src/runtime/apply.rs`'s `apply_intent_routed` now reads to resolve it.
        // Advertised UNCONDITIONALLY, `FEATURE_VENUE_ROUTING`'s argument one rung further: this is
        // a property of the BUILD, not of any one process's runtime state.
        //
        // ⚠ It was WITHHELD for exactly one window and no longer than that: between
        // `lower_command` copying the wire's `account` onto `vike_model::OrderRequest` (Task 3) and
        // `apply_intent_routed` reading that field to route (Task 4). Advertising inside that window
        // would have told a compliant client (check the string, then set the field) yes while it
        // still misrouted in silence — the exact defect this string exists to prevent. The window is
        // closed; the string is truthful again.
        // `served_features_tests::the_node_advertises_account_routing_now_that_routing_reads_the_field`
        // pins the pairing, and `FEATURE_ACCOUNT_ROUTING`'s own doc in
        // `vike_tradehub_client::proto` carries the client-side refusal rule this string releases.
        //
        // ⚠ **"Withheld for exactly one window" is true of ONE PULL REQUEST, never of a release**,
        // and the paragraph above reads as if the fleet had been protected. It was not: the Task 3 →
        // Task 4 window lived and died inside #2120, while every RELEASED node from `v0.1.27`
        // through `v0.1.32` pushed this string from this very list and built its `OrderRequest`
        // without the account at all (measured over the tags). The string cannot be withdrawn from
        // nodes already shipped, so no client may rely on it any more for a labelled `Submit` —
        // it stays advertised for the clients that already check it, and the next entry is the
        // one a client now demands.
        FEATURE_ACCOUNT_ROUTING.to_string(),
        // ACCOUNT-SCOPED SUBMIT: the order plane's account claim under a string no account-dropping
        // node has ever carried — this node routes a labelled `Submit` onto the account it names
        // and REFUSES, at [`super::refusal::account_refusal`], one naming an account it runs no engine of, before
        // the Ack. UNCONDITIONAL, the argument of the entry above: `lower_command`, the core's
        // routing and the edge gate are one BUILD. (The edge gate's empty-roster window is its own
        // residual, stated on [`super::refusal::account_refusal`], and no string can flicker around it.)
        // `vike_tradehub_client::proto::FEATURE_ACCOUNT_SCOPED_SUBMIT`'s doc carries why it had to
        // be a new string; `served_features_tests::
        // the_node_advertises_account_scoped_submit_beside_the_edge_refusal_it_promises` pins it
        // beside the refusal it promises.
        FEATURE_ACCOUNT_SCOPED_SUBMIT.to_string(),
        // ACCOUNT-SCOPED REDUCING VERBS: this node narrows `MassCancel`/`Flatten`/`MarketExit` to
        // the account their payload names — `lower_command` carries the field, `vike_core`'s
        // reducing arms resolve it through `CoreThread::reduce_route_for_account`, and
        // `account_refusal` refuses an unheld one before the Ack. Advertised UNCONDITIONALLY for
        // `FEATURE_ACCOUNT_ROUTING`'s reason: a property of the BUILD.
        //
        // ⚠ It was advertised by NO node, deliberately, for as long as the field was dropped: a
        // node that did advertise it would have told a conforming client (check the string, then
        // set the field) yes while it fanned the verb over every account of the venue. Its own
        // string rather than riding `account-routing` because a node from before this build
        // advertises that one TRUTHFULLY — it routes a labelled submit — and still widens a
        // labelled reduce; a client must be able to tell the two apart, and
        // `vike_tradehub_client::remote_control`'s `required_feature` does, refusing a labelled
        // reduce locally against any node that does not carry this string.
        // `served_features_tests::the_node_advertises_account_scoped_reduce_now_that_the_core_honours_it`
        // pins the pairing.
        FEATURE_ACCOUNT_SCOPED_REDUCE.to_string(),
        // The TP/SL BRACKET (`WireCommand::Bracket`): a DECODE capability, the mount-verbs
        // argument. Advertised UNCONDITIONALLY because its arm ships in the same build
        // (`lower_command`, and `account_refusal`'s bracket arm, which serves the DEFAULT account
        // only). It was withheld for exactly the commit in which the variant decoded and the arm
        // refused.
        // `served_features_tests::the_node_advertises_the_bracket_verb_beside_the_arm_that_serves_it`
        // pins the pairing.
        FEATURE_BRACKET.to_string(),
        // The LIVE-JOURNAL REPORT verb (ruling 16): `Request::Tearsheet` → `Response::Tearsheet`.
        // Advertised UNCONDITIONALLY, the `settings-show` argument verbatim — the capability is a
        // property of this BUILD (the arm and its renderer are compiled in), not of whether this
        // particular process happens to have journaling switched on. A capability that flickered
        // with that runtime fact would be worse than one that always means "this build can answer
        // the question": the client refuses CLIENT-SIDE on a missing string
        // (`vike_tradehub_client::remote_handle`'s `tearsheet`), so flickering it off would turn a
        // node with no journal — an honest, named `Response::Error` an operator can act on — into
        // "this node does not support the verb", which is a claim about the BINARY and false.
        //
        // ⚠ This string was deliberately WITHHELD until the arm existed, and the reason is worth
        // keeping: advertising a capability before its renderer works converts a clean client-side
        // refusal into an opaque server error. The pairing is the invariant — this line and
        // [`super::tearsheet::tearsheet_reply`] ship together, and neither is correct alone.
        FEATURE_TEARSHEET.to_string(),
    ];
    // The datahub advertisement (split-plane REQ-2) — advertised ONLY when configured, so an
    // unconfigured daemon's Welcome is byte-identical to the pre-REQ-2 one. Advertisement, never
    // proxying: this server serves no data verb; the client dials the advertised address itself.
    if let Some(addr) = datahub_advertise {
        features.push(datahub_feature(addr));
    }
    // ⚠ **THE ONE CAPABILITY THIS NODE WITHHOLDS CONDITIONALLY, and the condition is the barrier.**
    // Every string above is a property of the BUILD and is advertised unconditionally — a node
    // without a `SettingsShowSource` still advertises `settings-show` and answers an honest
    // `Response::Error`, because a capability that flickers with a construction detail is worse
    // than one that always means *this build can answer the question*. The account verbs are the
    // exception, and deliberately: when the capability is absent there is no honest error to answer
    // WITH. `docs/decisions/0065` Part 1 is that the verb is refused *because there is nothing in
    // the process to refuse with*, so the advertisement has to track the same absence — a node that
    // advertised it and then refused every frame would be telling its clients a barrier had been
    // declared on a box where it had not.
    //
    // ⚠ Seeing this string is NOT authorization. Every account verb additionally requires
    // `Scope::Account`, a THIRD key whose tag is inside the signed preimage — so a Control peer that
    // reads this advertisement still cannot authenticate for it.
    if account_admin {
        features.push(FEATURE_ACCOUNT_VERBS.to_string());
    }
    features
}
