//! `server::handshake` — what an UNAUTHENTICATED peer meets: the ONE hub handshake
//! (`vike_hub_protocol::handshake::run_keyed_handshake`, decision 0288) under this node's domain, version and
//! advertisement ([`run_handshake`]), this node's admission of a claimed scope ([`scope_admission`]), and the
//! capability advertisement the `Welcome` carries ([`served_features`]).
//!
//! ⚠ **This node used to run a handshake of its own** beside the one the datahub, the backtesthub and the
//! gatewayhub share, and the owner's word ended it (decision 0288: *"WHY SEPARATE!!??? WHY NOT UNIFIED FOR
//! ALL??"*). Its frames were already the shared frames' bytes over the shared codec; what was this node's is a
//! VALUE: its refusals name the scope they refuse ("control disabled on this node", "account administration is
//! not armed on this node"), where the data planes answer a coarse `bad mac`, and they are logged at `warn`. Both
//! ride [`scope_admission`] and the note closure in [`run_handshake`]. Its result is that module's
//! `HandshakeOutcome`; a device's `PersonAuth` names the person on it.

use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use vike_hub_protocol::access::FEATURE_ACCESS_REGISTER;
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_hub_protocol::frame::MAX_FRAME_LEN;
use vike_hub_protocol::handshake::{
    HandshakeNote, HandshakeOutcome, HandshakeSpec, RegisterDoor, run_keyed_handshake,
};
use vike_tradehub_client::auth;
use vike_tradehub_client::proto::{
    FEATURE_ACCOUNT_ROUTING, FEATURE_ACCOUNT_SCOPED_REDUCE, FEATURE_ACCOUNT_SCOPED_SUBMIT,
    FEATURE_ACCOUNT_TRADING_STATE, FEATURE_ACCOUNT_VERBS, FEATURE_AMEND_REPLACE, FEATURE_BRACKET,
    FEATURE_BRACKET_ACCOUNT, FEATURE_BRACKET_STEM, FEATURE_COMBO, FEATURE_CORE_HELD_STOPS,
    FEATURE_CREDENTIAL_ADMIN, FEATURE_DIRECTORY, FEATURE_EMULATED_ORDERS, FEATURE_HUB_LINKS,
    FEATURE_MOUNT_ACCOUNT, FEATURE_MOUNT_CLASS, FEATURE_MOUNT_UNLATCH, FEATURE_MOUNT_VERBS,
    FEATURE_OBSERVE_COMPRESSED, FEATURE_OBSERVE_HEARTBEAT, FEATURE_OBSERVE_NEWEST,
    FEATURE_OBSERVE_PATCHES, FEATURE_OBSERVE_SERIES, FEATURE_ONE_ACCOUNT_VENUE,
    FEATURE_ORDER_ATTRIBUTES, FEATURE_ORDER_DISPLAY, FEATURE_ORDER_LIST,
    FEATURE_ORDER_MODIFY_TRIGGER, FEATURE_ORDER_PEG, FEATURE_ORDER_RELEASE,
    FEATURE_ORDER_TIME_IN_FORCE, FEATURE_ORDER_TRAILING, FEATURE_PARAMS_BY_MOUNT,
    FEATURE_RECONCILE_CANCEL, FEATURE_RECONCILE_VERBS, FEATURE_SETTINGS_SHOW,
    FEATURE_SETTINGS_WRITE, FEATURE_STRATEGY_PARAMS, FEATURE_STRATEGY_VERBS,
    FEATURE_UNADMIT_SYMBOL, FEATURE_VENUE_READ, FEATURE_VENUE_READ_POSITIONS, FEATURE_VENUE_RESUME,
    FEATURE_VENUE_ROUTING, NODE_PROTO_VERSION, Request, datahub_feature,
};

use super::log_read_end;

/// **May this node grant the scope a peer is CLAIMING?** — this node's admission, asked by the shared handshake
/// before any key or row is consulted, split out so it is decidable without a socket.
///
/// ⚠ **Extracted for exactly the reason [`super::accounts::account_admission`] was**, and that function's doc
/// carries the measurement: a decision living inside the handshake cannot be driven by a test, because it takes a
/// `&mut TcpStream`. Before this split, `crates/vike-tradehub/tests/` contained no test naming `Scope::Account` at
/// all — so the ONE check standing between a peer's CLAIM of admin and the account plane was covered by nothing, on
/// the binary that signs orders.
///
/// `docs/decisions/0070` names this the FIRST of the account boundary's two layers. The closed-gate property: an
/// unarmed capability is not an empty key to compare against, it is a scope that cannot be reached, for a shared
/// key and for a device alike.
///
/// - `Account` needs `account_admin`: this node holds an account writer, which it does only where the box DECLARED
///   the barrier (0065's barriers 1 and 3).
/// - `Write` needs `control` (the node holds a command sink: the flag is on) together with the access list, where a
///   device's role grants it (decision 0288), or a control key held: the shipped daemon loads none (decision 0288
///   item 8), so that arm answers only for keys a TEST builds by hand (a node with the key but no command sink
///   still serves `Preview`).
///
/// Returns the AuthDenied `reason` verbatim on refusal, so the wire text has one home.
pub(super) fn scope_admission(
    scope: Scope,
    keys: &HubKeys,
    control: bool,
    account_admin: bool,
) -> Result<(), &'static str> {
    match scope {
        Scope::Account if !account_admin => Err("account administration is not armed on this node"),
        Scope::Write if !(keys.has(Scope::Write) || control && keys.access().is_some()) => {
            Err("control disabled on this node")
        }
        _ => Ok(()),
    }
}

/// Run the ONE hub handshake (`Hello` -> `Welcome{ nonce }` -> `Auth` | `PersonAuth` | `Register` -> verify) under
/// this node's domain, version and advertisement. [`scope_admission`] answers first, in this node's words; a shared
/// key's mac is then verified against the REQUESTED scope's key, a device's signature against its access row. Any
/// session verb arriving before auth is refused (an unauthenticated `Subscribe` never registers). Both reads share
/// ONE `deadline`, counted from the call. `register` is the invitation door (decision 0288 item 7.3), advertised as
/// `access-register` only where it is held.
pub(super) fn run_handshake(
    stream: &mut TcpStream,
    keys: &HubKeys,
    peer: Option<SocketAddr>,
    datahub_advertise: Option<&str>,
    control: bool,
    // Whether this node holds an `AccountAdminSource` — the ONE conditional entry in
    // `served_features`. See that function and `vike_tradehub_client::proto::FEATURE_ACCOUNT_VERBS`.
    account_admin: bool,
    register: Option<RegisterDoor<'_>>,
    deadline: Duration,
) -> HandshakeOutcome {
    let mut features = served_features(datahub_advertise, account_admin);
    if register.is_some() {
        features.push(FEATURE_ACCESS_REGISTER.to_string());
    }
    let spec = HandshakeSpec {
        domain: auth::DOMAIN,
        proto_version: NODE_PROTO_VERSION,
        plane: None,
        features: &features,
        deadline,
        reply_ceiling: MAX_FRAME_LEN,
    };
    let outcome = run_keyed_handshake(
        stream,
        keys,
        spec,
        |scope| scope_admission(scope, keys, control, account_admin),
        register,
        |_: &Request| "a session verb",
        |note| log_note(note, peer),
    );
    if let HandshakeOutcome::Authed { scope, key_id, person } = &outcome {
        tracing::info!(
            ?peer,
            ?scope,
            key_id = key_id.as_deref(),
            person = person.as_ref().map(|p| p.person.as_str()),
            device = person.as_ref().map(|p| p.device.as_str()),
            "vike-tradehub: authenticated"
        );
    }
    outcome
}

/// One handshake step, in this node's words and at its levels.
fn log_note(note: HandshakeNote, peer: Option<SocketAddr>) {
    match note {
        // An unauthenticated verb, an undecodable frame or a second `Hello`: answered on the wire and dropped, as
        // `crate::audit`'s module doc declares (the pre-auth protocol refusals log nothing).
        HandshakeNote::VerbRefused(_) | HandshakeNote::Authenticated(_) => {}
        HandshakeNote::VersionDiffers(client_version) => tracing::debug!(
            ?peer,
            client_version,
            server_version = NODE_PROTO_VERSION,
            "vike-tradehub observe: client protocol version differs; auth will fail on the signed version"
        ),
        // WARN, not info, and the level is the whole point: a live box runs its `preferences.log_file_level` row
        // at `warn`, so an `info` refusal never reaches the log file there. `crate::audit::FILE_PIN` pins the
        // ACCEPTED half of the control trail onto disk; without these the file would hold every command taken and
        // no evidence that anyone was ever turned away. Each refusal keeps its own sentence: a post-incident
        // review searches for them by text.
        HandshakeNote::Refused(Scope::Account, _) => tracing::warn!(
            ?peer,
            "vike-tradehub: ADMIN auth refused — account administration is not armed on this node"
        ),
        HandshakeNote::Refused(scope, reason) => {
            tracing::warn!(
                ?peer,
                ?scope,
                reason,
                "vike-tradehub: control auth refused — control disabled on this node"
            )
        }
        HandshakeNote::NoKey(scope) | HandshakeNote::BadMac(scope) => {
            tracing::warn!(?peer, ?scope, "vike-tradehub: auth denied (bad mac / no key)")
        }
        HandshakeNote::PersonDenied(scope, why) => tracing::warn!(
            ?peer,
            ?scope,
            why = why.as_str(),
            "vike-tradehub: a device's auth denied (access denied)"
        ),
        HandshakeNote::ReadEnded(kind) => log_read_end(&std::io::Error::from(kind), peer),
    }
}

// The verbs this node server answers, advertised in `Welcome.features`. (Control is gated by the
/// server's `HubKeys`/`CommandSink`, not by an advertised feature string, so it stays off this list.)
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
        // honest refusal, which is a better answer than a capability that flickers
        // with construction details (and mirrors `StrategyStatus` on an identity-less publisher).
        FEATURE_SETTINGS_SHOW.to_string(),
        // The DIRECTORY read: `Directory`. Advertised unconditionally, like its settings sibling:
        // a node started without a settings source still decodes the request and answers an
        // honest refusal.
        FEATURE_DIRECTORY.to_string(),
        // The SETTINGS WRITE verb (split-plane REQ-7, write half): `CommandWire::SetSetting`.
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
        // A subscriber that says what it has read gets the newest frame next (`run_push_writer`). A property of the BUILD.
        FEATURE_OBSERVE_NEWEST.to_string(),
        // The SHAPED subscription (`Request::SubscribeWith`, `super::observer_push`) and its compressed body: a DECODE
        // capability whose writer ships in the same build, so advertised UNCONDITIONALLY. Whether a body IS compressed is
        // the `config.observer_frame_compression` row's (off by default), and the reader tells by the body's first byte.
        FEATURE_OBSERVE_COMPRESSED.to_string(),
        // ...and its PATCHES (`ObserveSubscriptionWire::patches`, `Request::ObserveResync`): a whole snapshot, then what
        // changed, numbered. A DECODE capability whose writer ships in the same build, so advertised UNCONDITIONALLY.
        FEATURE_OBSERVE_PATCHES.to_string(),
        // ...and the SERIES a subscriber shows (`ObserveSubscriptionWire::series`, `Request::ObserveSeries`): the bars of
        // those alone, every other series by its key; orders, positions, accounts and halts always. A DECODE capability
        // whose writer ships in the same build, so advertised UNCONDITIONALLY.
        FEATURE_OBSERVE_SERIES.to_string(),
        // The msgpack body (`vike_hub_protocol::frame::BodyCodec`): a client that sees it writes msgpack with field names,
        // and each request, and a subscription's every frame, is answered in the form it arrived in (`serve_session`). A
        // DECODE capability of the BUILD, advertised UNCONDITIONALLY; a client without it is answered in JSON.
        vike_hub_protocol::features::FEATURE_WIRE_MSGPACK.to_string(),
        // The refusal frame (`vike_tradehub_client::proto::Response::Refused`), sent to a client that says it reads it
        // (`HandshakeFrame::Reads`): the starting door's answer carries its id. A property of the BUILD, advertised
        // UNCONDITIONALLY; a client that never says so is answered the `Error` it always read.
        vike_hub_protocol::features::FEATURE_REFUSAL_FRAME.to_string(),
        // The STRUCTURED live-params read: this node's `MountViewWire`s carry the mount's addressing
        // key and its typed params. Its OWN capability rather than riding `strategy-verbs`, and one
        // rung sharper than the mount verbs' reason: a node predating it ANSWERS `StrategyStatus`
        // with rows whose new fields `#[serde(default)]` fills in as empty, which a client cannot
        // tell from an honest "this mount publishes none" — see `FEATURE_STRATEGY_PARAMS`.
        // Advertised UNCONDITIONALLY, the `settings-show` argument verbatim: a node whose mounts all
        // answer `typed_params: None` is still answering honestly, and a capability that flickered
        // with a construction detail (an identity-less publisher, a mount-less core) would be worse
        // than one that always means "this build can answer the question".
        FEATURE_STRATEGY_PARAMS.to_string(),
        // The mount ID on both halves: this node's `MountViewWire`s carry `mount_id`, and its
        // `UpdateParams` honours `mount_id` (the core refuses an ambiguous or unknown one instead of
        // retuning the first mount on the series). One string for the pair, because the id is only
        // useful with both. Its OWN capability, for the `strategy-mount-account` reason: a node from
        // before it DECODES an addressed update, drops the key and retunes the FIRST mount, behind a
        // normal Ack - see `FEATURE_PARAMS_BY_MOUNT`. Advertised UNCONDITIONALLY: the field and the
        // arm that reads it ship together, so it is a property of the BUILD.
        FEATURE_PARAMS_BY_MOUNT.to_string(),
        // WHAT PRODUCT each mount trades: this node's `MountViewWire`s carry `asset_class`, the
        // stored word of a `vike_model::AssetClass`. `docs/decisions/0061`'s Phase 5 made a mount
        // NAME its class — `NOT NULL` in the settings database, refused at store time if absent —
        // and until this string the value was visible to the database alone.
        //
        // Its own capability for the `strategy-params` reason, one rung sharper again: a `None`
        // here has THREE sources and `#[serde(default)]` flattens two of them. A node predating
        // this cannot carry the field; a CURRENT node whose mount is still TOML-backed honestly has
        // no class, because `config::MountConfig`'s field is an `Option` and that asymmetry IS the
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
        // send only venues it can positively see in `CoreSnapshotWire::venues`; seeing it means the
        // node will say no rather than misroute, so the client may offer the operator every venue
        // and let the node answer. `FEATURE_VENUE_ROUTING`'s own doc carries the direction.
        FEATURE_VENUE_ROUTING.to_string(),
        // ACCOUNT ROUTING: this node routes an order-carrying command onto the account its wire
        // payload names, instead of always the venue's default engine — the field
        // `crates/vike-core/src/runtime/apply/routed.rs`'s `apply_intent_routed` now reads to resolve it.
        // Advertised UNCONDITIONALLY, `FEATURE_VENUE_ROUTING`'s argument one rung further: this is
        // a property of the BUILD, not of any one process's runtime state.
        //
        // ⚠ It was WITHHELD for exactly one window and no longer than that: between
        // `lower_command` copying the wire's `account` onto `vike_model::OrderRequest` (Task 3) and
        // `apply_intent_routed` reading that field to route (Task 4). Advertising inside that window
        // would have told a compliant client (check the string, then set the field) yes while it
        // still misrouted in silence — the exact defect this string exists to prevent. The window is
        // closed; the string is truthful again.
        // `server::tests::served_features::the_node_advertises_account_routing_now_that_routing_reads_the_field`
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
        // be a new string; `server::tests::served_features::
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
        // `vike_tradehub_client::tradehub_control`'s `required_feature` does, refusing a labelled
        // reduce locally against any node that does not carry this string.
        // `server::tests::served_features::the_node_advertises_account_scoped_reduce_now_that_the_core_honours_it`
        // pins the pairing.
        FEATURE_ACCOUNT_SCOPED_REDUCE.to_string(),
        // THE ONE-ACCOUNT RULE (decision 0254): this door refuses a venue-scoped command that names no account
        // unless the venue runs exactly one (`super::refusal::one_account_addressed`), so a client may send a
        // bare reducing verb without it fanning over every account. UNCONDITIONAL: a property of the BUILD.
        FEATURE_ONE_ACCOUNT_VENUE.to_string(),
        // The TP/SL BRACKET (`CommandWire::Bracket`): a DECODE capability, the mount-verbs
        // argument. Advertised UNCONDITIONALLY because its arm ships in the same build
        // (`lower_command`, and `account_refusal`'s bracket arm, which serves the DEFAULT account
        // only). It was withheld for exactly the commit in which the variant decoded and the arm
        // refused.
        // `server::tests::served_features::the_node_advertises_the_bracket_verb_beside_the_arm_that_serves_it`
        // pins the pairing.
        FEATURE_BRACKET.to_string(),
        // ...and that verb's ACCOUNT field (`OrderBracketWire::account`), under a string of its OWN rather
        // than riding the line above (the mount-account argument): `lower_command` carries the field,
        // `vike_core`'s `lower_bracket` routes by it, and `account_refusal`/`bracket_refusal` judge the
        // NAMED account's engine, all in this build. Advertised UNCONDITIONALLY, a property of the BUILD.
        // `server::tests::served_features::the_node_advertises_the_bracket_account_beside_the_arms_that_read_it`
        // pins the pairing.
        FEATURE_BRACKET_ACCOUNT.to_string(),
        // ...and its group STEM (`OrderBracketWire::order_client_id`, decision 0270), under a string of its own by the
        // same argument: `lower_command` carries it onto the core's spec, the core (`lower_bracket`) derives the leg ids
        // from it, and the door refuses a stem whose legs were used before the last restart. A property of the BUILD.
        FEATURE_BRACKET_STEM.to_string(),
        // The operator's RECONCILE verb: `CommandWire::ReconcileConfirm` plus the held divergences in
        // `CoreSnapshotWire::recon`. Advertised UNCONDITIONALLY, the bracket argument: a DECODE
        // capability whose arm (`lower_command`) and published field ship in the same build.
        FEATURE_RECONCILE_VERBS.to_string(),
        // The operator's CANCEL of one held `UnknownOrder` row (`CommandWire::ReconcileCancel`, decision
        // 0196 rule (b)): a DECODE capability whose arm (`lower_command`) and core command ship in the
        // same build, so it is advertised UNCONDITIONALLY, the recon-verbs argument. Whether a given
        // row can be cancelled (tier, kind, id) is the core's verdict, a note and a WARN.
        FEATURE_RECONCILE_CANCEL.to_string(),
        // The operator's PER-VENUE RESUME (`CommandWire::ResumeVenue`, decision 0180): a DECODE
        // capability whose arm (`lower_command`) and core command ship in the same build, so it is
        // advertised UNCONDITIONALLY, the recon-verbs argument. A node without the link dead-man
        // armed still advertises it and answers the verb with the core's refusal (a WARN and a
        // note): a capability that flickered with the switch would turn that into "unsupported".
        FEATURE_VENUE_RESUME.to_string(),
        // The ACCOUNT-scoped trading state (`CommandWire::SetAccountTradingState`): a DECODE
        // capability whose arm (`lower_command`), edge gate (`account_refusal`) and core command ship
        // in the same build, so it is advertised UNCONDITIONALLY, the venue-resume argument.
        FEATURE_ACCOUNT_TRADING_STATE.to_string(),
        // The LISTING of the operator's armed core-held reduce-only stops (`CoreSnapshotWire::core_held_stops`,
        // decision 0199's amendment): a READING, not a verb, advertised UNCONDITIONALLY (a property of the
        // BUILD: the publisher projects the field in every frame). Its only job is to tell an empty list
        // from a node that predates the field, for the readers of that older list (MCP, an older desktop).
        FEATURE_CORE_HELD_STOPS.to_string(),
        // The ONE list of every order the node emulates (`CoreSnapshotWire::emulated_orders`, decision 0275): a READING
        // the publisher projects in every frame, so a property of the BUILD, advertised UNCONDITIONALLY. It tells an
        // empty list from a node that predates the field, so `trade order stops` (which reads its core-held stops
        // from it) never reads silence as "none armed".
        FEATURE_EMULATED_ORDERS.to_string(),
        // The links to the other hubs (`CoreSnapshotWire::trade_link_down`, `price_link_down`): a READING, a property of
        // the BUILD (the publisher stamps both in every frame), so it is advertised UNCONDITIONALLY. Its only job is to
        // let `trade health` read an empty list as "every link is up" and not as a node that predates them.
        FEATURE_HUB_LINKS.to_string(),
        // A modify of a resting order that the venue cannot change in place is carried out as
        // cancel-then-new (decision 0244): a property of the BUILD (the core's amend arm ships in the
        // same binary), advertised UNCONDITIONALLY. It gates no verb: the command is the same frame.
        FEATURE_AMEND_REPLACE.to_string(),
        // The GIVE-BACK of an admitted symbol (`CommandWire::UnadmitSymbol`, decision 0205): a DECODE capability
        // whose arm (`lower_command`), edge gate (`unadmit_refusal`) and core statement
        // (`SymbolAdmission::release`) ship in the same build, so it is advertised UNCONDITIONALLY, the
        // account-trading-state argument.
        FEATURE_UNADMIT_SYMBOL.to_string(),
        // The UN-LATCH of one mount's budget (`CommandWire::UnlatchMount`, decision 0239) and the `latched` flag of the
        // mount rows it is judged by (`MountViewWire::latched`): a DECODE capability whose arm (`lower_command`), edge gate
        // (`plan_unlatch`) and core command ship in the same build, so it is advertised UNCONDITIONALLY, the
        // unadmit-symbol argument. The flag is written in every `StrategyStatus` reply of this build, which is why one
        // string says both.
        FEATURE_MOUNT_UNLATCH.to_string(),
        // The operator's READ of one account at its venue: `Request::VenueRead` → `Response::VenueRead`.
        // Advertised UNCONDITIONALLY, the `settings-show` argument: it is a property of this BUILD (the arm
        // and [`super::venue_read::venue_read_reply`] are compiled in). A node with no lease registry (no
        // gatewayhub behind it), an account that is not running here and a bridge that offers no such read
        // each answer an honest refusal naming the cause, which a capability that flickered with
        // them would turn into "this node does not support the verb", a claim about the BINARY.
        FEATURE_VENUE_READ.to_string(),
        // The THIRD kind of that read, `VenueReadKind::Positions`: a DECODE capability of its own on top
        // of `venue-read` (a node that has only the older word cannot decode the kind's name), whose arm
        // (`venue_read_reply`, which decodes the kind) ships in the same build, so it is advertised
        // UNCONDITIONALLY for the same reason. Whether the gatewayhub behind this node carries the matching
        // word is answered per read, in words, by the lease client.
        FEATURE_VENUE_READ_POSITIONS.to_string(),
        // The order's LIFETIME on a submit (`OrderRequestWire::time_in_force` / `gtd_expiry`, `post-only`
        // included): a FIELD capability whose reader (`lower_command`'s submit arm, with
        // `vike_model::lifetime_refusal`) ships in the same build, so it is advertised UNCONDITIONALLY, the
        // bracket-account argument: a property of the BUILD. Withholding it would leave a client unable to
        // send the lifetime this node reads; advertising it from a build that did not read it is the silent
        // drop the string exists to prevent.
        // `server::tests::served_features::the_node_advertises_the_order_lifetime_beside_the_arm_that_reads_it`
        // pins the pairing.
        FEATURE_ORDER_TIME_IN_FORCE.to_string(),
        // The three wave-2 order FIELDS (`OrderRequestWire::display_quantity` / `release`, `CommandWire::Modify`'s
        // `new_trigger_price`), the `order-tif` argument word for word: `lower_command` reads each onto the core
        // `OrderRequest` / `OrderIntent::Modify` in this build. ⚠ ONE build advertises all three, and the client's
        // `required_feature` relies on it (it asks one word of a submit naming two fields).
        // `server::tests::served_features::the_node_advertises_the_wave_two_order_fields_beside_the_arm_that_reads_them`
        // pins the pairing.
        FEATURE_ORDER_DISPLAY.to_string(),
        FEATURE_ORDER_RELEASE.to_string(),
        FEATURE_ORDER_MODIFY_TRIGGER.to_string(),
        // The wave-3 trail and limit offset (`OrderRequestWire::trail` / `limit_offset`), the same argument:
        // `lower_command` reads both onto the core `OrderRequest` in this build. It ships after the three above,
        // and the client's `required_feature` relies on that (its one word covers a trailing submit naming them).
        // `server::tests::served_features::the_node_advertises_the_trailing_fields_beside_the_arm_that_reads_them`
        // pins the pairing.
        FEATURE_ORDER_TRAILING.to_string(),
        // The wave-5 peg (`OrderRequestWire::peg`), the same argument: `lower_command` reads it onto the core
        // `OrderRequest` in this build, after `order-trailing` (the client's one word covers a pegged submit).
        // `server::tests::served_features::the_node_advertises_the_peg_beside_the_arm_that_reads_it` pins the pairing.
        FEATURE_ORDER_PEG.to_string(),
        // The wave-4 ORDER LIST (`CommandWire::OrderList`): a DECODE capability whose door (`order_list_refusal`) and
        // lowering (`lower_command`'s `OrderList` arm onto `OrderIntent::OrderList`) ship in this build, so it is
        // advertised unconditionally, the bracket's argument. What the core does with a list is the core's verdict.
        // `server::tests::order_list_door::the_node_advertises_the_order_list_beside_the_arm_that_lowers_it` pins it.
        FEATURE_ORDER_LIST.to_string(),
        // The wave-6 COMBO (`CommandWire::Combo`): a DECODE capability whose door (`combo_refusal`) and lowering
        // (`lower_command`'s `Combo` arm onto `OrderIntent::Combo`) ship in this build, the order list's argument. What
        // a venue takes (only deribit's row a combo, only a paper book a leg limit) is the core's verdict.
        // `server::tests::combo_door::the_node_advertises_the_combo_beside_the_arm_that_lowers_it` pins it.
        FEATURE_COMBO.to_string(),
        // The attributes wave's submit terms (`OrderRequestWire::attributes` / `trail_percent` / `adjust`, and a release's
        // price venue `OrderReleaseWire::venue`), the `order-peg` argument: `lower_command` reads each onto the core
        // `OrderRequest` in this build, with the rules that judge them, and the door asks a price venue's engine
        // (`release_venue_refusal`). `server::tests::served_features::the_node_advertises_the_attributes_beside_the_arm_that_reads_them`
        // pins the pairing.
        FEATURE_ORDER_ATTRIBUTES.to_string(),
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
    // refusal, because a capability that flickers with a construction detail is worse
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
        // ⚠ Rides the SAME condition and no other: the credential retire / presence verbs and the
        // data-provider names are answered by the very handle that answers the account verbs, so a
        // node that advertised this without the capability would claim a barrier it does not hold.
        // A client owes BOTH strings before it sends one (`vike_tradehub_client::exchange_once`).
        features.push(FEATURE_CREDENTIAL_ADMIN.to_string());
    }
    features
}
