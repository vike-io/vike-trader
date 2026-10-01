//! The vike-tradehub **node** wire protocol: the request/response schema a thin GUI client and the
//! headless live core ("tradehub") exchange, plus the authentication challenge that gates it.
//!
//! # Framing — ONE codec across both services
//!
//! This module re-exports the length-prefixed `serde_json` frame codec from
//! [`vike_node_proto::frame`] ([`read_frame`] / [`write_frame`] / [`MAX_FRAME_LEN`]) rather than
//! growing a second one: both localhost services (the datahub data-service and this live-core node)
//! speak the identical big-endian-`u32`-length + JSON-body frame, with the same pre-allocation OOM
//! guard. A frame is `[u32 len][len bytes of UTF-8 JSON]`.
//!
//! # The connection handshake (PR-10 — Layer-2 auth)
//!
//! The core is a headless process that accepts orders; an unauthenticated peer must never reach the
//! [`Request::Command`] path. The handshake is a nonce-challenge HMAC, so a captured transcript can
//! never be replayed against a later connection (each connection mints a fresh random `nonce`):
//!
//! 1. Client → [`Request::Hello`] `{ proto_version }`.
//! 2. Server → [`Response::Welcome`] `{ proto_version, nonce, features }` — `nonce` is 32 fresh
//!    random bytes for THIS connection.
//! 3. Client → [`Request::Auth`] `{ scope, mac }` where
//!    `mac == auth::sign(key_for(scope), &nonce, proto_version, scope)` (see [`crate::auth`]).
//! 4. Server recomputes the mac with the key it holds for that `scope` and constant-time-compares
//!    ([`crate::auth::verify`]). Match → [`Response::AuthOk`]; mismatch / unknown scope / wrong key
//!    → [`Response::AuthDenied`]. Only after `AuthOk` may the client [`Request::Subscribe`],
//!    [`Request::Snapshot`], or (with [`Scope::Write`]) [`Request::Command`].
//!
//! The [`Scope`] a client authenticates under is its capability ceiling: [`Scope::Read`] is
//! read-only (snapshots + subscriptions), [`Scope::Write`] additionally admits order commands. The
//! two scopes sign under DIFFERENT keys ([`crate::auth::NodeKeys`]), so an observe-only key can never
//! produce a valid `Control` mac — the scope is bound INTO the signed message, not merely asserted.
//!
//! # No I/O here
//!
//! This MODULE opens no socket: it defines the schema and re-exports the framing, whose
//! `read_frame`/`write_frame` run over whatever stream a caller hands them. The CRATE has no server
//! loop — the node's listener lives in `vike-tradehub` — but it does dial: every connection
//! [`crate::remote_handle`] and [`crate::remote_control`] make goes through
//! `crates/vike-tradehub-client/src/handshake.rs`'s `node_handshake`, which opens the TCP stream.
//!
//! ⚠ This said "There is NO server loop and NO socket in this crate — a server binding this proto
//! lands in a later PR" until 2026-09-28. It was written for PR-10 (#727), which built the crate in
//! isolation before anything listened; PR-11 (#731) added both the server, in `vike-tradehub`, and
//! the first client that dials, in this crate — the same day.

// ONE framing across both localhost services — re-exported, never re-implemented. `read_frame_raw`
// is the framing-without-decode primitive a server uses to keep a connection alive across an
// undecodable body (a bad request, not a bad connection) — the vike-tradehub observe server relies
// on it, exactly as the vike-datahub server does.
// `read_frame_raw_capped` is that same primitive with a CALLER-CHOSEN ceiling: the node server
// reads its PRE-AUTH frames under a far smaller cap than `MAX_FRAME_LEN`, so an unauthenticated
// peer cannot make it allocate 64 MiB per connection off a four-byte length prefix.
pub use vike_node_proto::frame::{
    MAX_FRAME_LEN, read_frame, read_frame_raw, read_frame_raw_capped, write_frame,
};

use serde::{Deserialize, Serialize};

use crate::wire::{WireCommand, WireSettingsShow, WireSnapshot, WireStrategyStatus};

/// The node protocol version a [`Request::Hello`] declares and a [`Response::Welcome`] echoes. It is
/// also folded INTO the signed auth message ([`crate::auth::sign`]), so a client and server that
/// disagree on the version can never produce a matching mac — a version skew fails the handshake
/// rather than silently talking past each other. Bump on any wire-schema change.
///
/// v2: added `WireSnapshot::bars` (bounded bar tails for the observer chart).
/// v3: added the Preview request/response (server-authoritative dry-run).
/// v4: Command carries an optional operator/agent rationale, recorded in the audit trail.
///
/// ⚠ The strategy verbs (split-plane B4: [`Request::StrategyStatus`] +
/// `WireCommand::UpdateParams`) deliberately did NOT bump this: the version is folded into the
/// signed auth MAC, so a bump breaks the handshake against every running node. They ride the
/// designed forward-compat hook instead — the server advertises [`FEATURE_STRATEGY_VERBS`] in
/// `Welcome.features` and a client REFUSES CLIENT-SIDE (nothing sent) against a server that does
/// not list it.
pub const NODE_PROTO_VERSION: u32 = 4;

/// The `Welcome.features` capability string a node advertises when it serves the STRATEGY verbs
/// (split-plane B4): the [`Request::StrategyStatus`] read and the `WireCommand::UpdateParams`
/// write. A client MUST check for this before sending either — an older server cannot decode the
/// new variants (it answers `Response::Error` for the frame), so the client-side refusal is what
/// turns "undecodable request" into an actionable "this node predates strategy-verbs" — and
/// nothing goes on the wire at all ([`crate::remote_handle::strategy_status`] and
/// [`crate::remote_control::RemoteControlHandle`] both enforce it).
pub const FEATURE_STRATEGY_VERBS: &str = "strategy-verbs";

/// The `Welcome.features` capability string a node advertises when it serves the SETTINGS read
/// verb (split-plane REQ-7, read half): [`Request::SettingsShow`] →
/// [`Response::SettingsShow`]. Same forward-compat design as [`FEATURE_STRATEGY_VERBS`] — no
/// [`NODE_PROTO_VERSION`] bump (the version is folded into the signed auth MAC, so a bump breaks
/// the handshake against every running node); a client MUST check for this string before sending
/// the verb, so against an older server NOTHING goes on the wire and the caller gets an
/// actionable "this node predates settings-show" instead of an undecodable-request error
/// ([`crate::remote_handle::settings_show`] enforces it).
pub const FEATURE_SETTINGS_SHOW: &str = "settings-show";

/// The `Welcome.features` capability string a node advertises when it serves the SETTINGS WRITE
/// verb (split-plane REQ-7, write half): `WireCommand::SetSetting`, answered by
/// [`Response::SettingsWritten`]. A SEPARATE capability from [`FEATURE_SETTINGS_SHOW`]
/// deliberately — a read-half node advertises `settings-show` yet cannot decode the `SetSetting`
/// variant (serde answers `Response::Error` for the unknown variant), the same argument that gave
/// the B5 mount verbs their own string. Same no-version-bump design as every post-v4 verb: the
/// proto version is folded into the signed auth MAC, so `Welcome.features` is the forward-compat
/// hook. A client MUST check for this string before sending the verb
/// ([`crate::remote_control::set_setting`] and the
/// [`crate::remote_control::RemoteControlHandle`] queue both enforce it), so against an older
/// server NOTHING goes on the wire and the caller gets an actionable "this node predates
/// settings-write" instead of an undecodable-request error.
pub const FEATURE_SETTINGS_WRITE: &str = "settings-write";

/// The `Welcome.features` capability string a node advertises when it serves the DIRECTORY read:
/// [`Request::Directory`] → [`Response::Directory`]. Same forward-compat design as
/// [`FEATURE_SETTINGS_SHOW`]: no [`NODE_PROTO_VERSION`] bump, and a client MUST check for this
/// string before sending the verb ([`crate::remote_handle::directory`] enforces it).
pub const FEATURE_DIRECTORY: &str = "directory";

/// The `Welcome.features` capability string a node advertises when it serves the runtime
/// MOUNT/UNMOUNT verbs (split-plane B5): `WireCommand::MountStrategy` /
/// `WireCommand::UnmountStrategy`. A SEPARATE capability from [`FEATURE_STRATEGY_VERBS`]
/// deliberately — a B4-era node advertises `strategy-verbs` yet cannot decode the mount variants
/// (serde answers `Response::Error` for the unknown variant), so riding the old string would
/// defeat exactly the client-side refusal the feature list exists for. Same no-version-bump
/// argument as B4: the proto version is folded into the signed auth MAC, so features are the
/// forward-compat hook. [`crate::remote_control`]'s `required_feature` is the one authority
/// mapping commands to required capabilities.
pub const FEATURE_MOUNT_VERBS: &str = "strategy-mount-verbs";

/// The `Welcome.features` capability a node advertises when its [`crate::wire::WireMountRow`]s
/// carry the mount's ADDRESSING KEY (`venue`/`symbol`/`interval`) and its TYPED params — the
/// structured READ half of the live-parameter plane, which a client patches and hands straight back
/// through the unchanged `WireCommand::UpdateParams`.
///
/// A SEPARATE capability from [`FEATURE_STRATEGY_VERBS`], for a reason one rung sharper than the
/// mount verbs' above: a node predating this ADVERTISES `strategy-verbs` and ANSWERS a
/// [`Request::StrategyStatus`] perfectly well — with rows whose new fields are simply absent, which
/// `#[serde(default)]` renders INDISTINGUISHABLE from "this mount publishes no typed params". There
/// the old node could not DECODE and errored; here it decodes and answers something WEAKER, so
/// without its own string a client would silently do the wrong thing (offer a tuning surface seeded
/// from nothing, or report a mount as untunable when it is not) instead of failing. Check the
/// capability, never the emptiness of a field.
///
/// The WRITE half needs no capability of its own: it is `UpdateParams`, already gated by
/// [`FEATURE_STRATEGY_VERBS`]. That is the compatibility dividend of merging client-side.
///
/// No [`NODE_PROTO_VERSION`] bump, for the reason every post-v4 capability carries: the version is
/// folded into the signed auth MAC, so a bump breaks the handshake against every running node.
pub const FEATURE_STRATEGY_PARAMS: &str = "strategy-params";

/// The `Welcome.features` capability string a node advertises when its mount rows carry
/// [`crate::wire::WireMountRow::asset_class`] — WHAT PRODUCT each mount trades.
///
/// `docs/decisions/0061-an-instrument-names-its-kind.md`'s Phase 5 made a mount NAME its class:
/// `mount.asset_class` is `NOT NULL` in the settings database, with a `CHECK` whose words are
/// `vike_model::AssetClass`'s own, and `vike_tradehub::profile_rows` REFUSES to store a mount that
/// omits it. That closed the storage half and left the value visible to the database alone — a
/// mount was saying what it trades to nobody who could read it.
///
/// ⚠ **It needs its own capability because the field has THREE states and `#[serde(default)]`
/// renders two of them identically.** A `None` means:
///   1. the node predates this string — the mount may well have a class and this wire cannot carry
///      it;
///   2. the node has it and the mount is ROW-backed — then `None` is unreachable, because the
///      column is `NOT NULL` and `MountRow::new` takes the class as a parameter;
///   3. the node has it and the mount is still TOML-backed — `vike_tradehub::config::MountCfg`'s
///      `asset_class` is an `Option` there, and that asymmetry IS the migration 0061 Phase 5
///      describes, argued at that field. So `None` is HONEST and means "not migrated yet".
///
/// (1) and (3) are the pair that matters: one is a client too new for its node, the other is a
/// deployment with work left. A reader that looked only at the field would report the second as the
/// first and send somebody to upgrade a daemon that is already current. Check the capability, never
/// the emptiness — the standing rule [`FEATURE_STRATEGY_PARAMS`] states above, for the same reason.
///
/// ⚠ This gates a FIELD, not a verb, and nothing routes on it. 0061 is explicit: *"the exec plane
/// still infers nothing. A mount naming its class is a mount saying what it is, not a new routing
/// input."* A client may DISPLAY it and may not decide with it.
///
/// No [`NODE_PROTO_VERSION`] bump, for the reason every post-v4 capability carries: the version is
/// folded into the signed auth MAC, so a bump breaks the handshake against every running node.
pub const FEATURE_MOUNT_CLASS: &str = "mount-class";

/// The `Welcome.features` capability string a node advertises when it HEARTBEATS a subscribed
/// observe stream — a `Response::Pong` written whenever nothing else was sent for
/// [`crate::liveness::OBSERVE_HEARTBEAT`] (`vike-tradehub`'s `server::run_push_writer`).
///
/// Unlike every capability above it, this one does not gate a VERB — nothing new may be sent when
/// it is present. It gates a client-side DEADLINE: `crate::remote_handle::RemoteCoreHandle::connect`
/// arms [`crate::liveness::OBSERVE_READ_TIMEOUT`] on the stream only when the node lists this, so
/// the receive loop's existing error arm can end a link that died in SILENCE. Against a node that
/// does not list it the client arms nothing and behaves exactly as it did before the heartbeat
/// existed — which is the whole reason this is negotiated rather than assumed: a client that
/// deadlined every node would tear down a healthy stream to a heartbeat-less one every 45 s and
/// reconnect, turning a silent-death fix into a reconnect loop on an idle node.
///
/// No [`NODE_PROTO_VERSION`] bump, for the reason every post-v4 capability carries: the version is
/// folded into the signed auth MAC, so a bump breaks the handshake against every running node. And
/// no wire-SCHEMA change either — the heartbeat is the [`Response::Pong`] this protocol has always
/// had, so a client that predates the string (or ignores it) simply drops an unsolicited `Pong` on
/// the floor, which is what `RemoteCoreHandle`'s receive loop already did with every non-frame
/// reply.
pub const FEATURE_OBSERVE_HEARTBEAT: &str = "observe-heartbeat";

/// The `Welcome.features` capability string a node advertises when it serves the LIVE-JOURNAL
/// REPORT verb: [`Request::Tearsheet`] → [`Response::Tearsheet`], the wire half of `vike-cli
/// report` (ruling 16 of `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`).
///
/// ⚠ **THE ARM HAS LANDED, and this block asserted the opposite.** It read *"NO SHIPPED NODE
/// ADVERTISES THIS YET, and that is the state this constant exists to make legible"*, arguing that
/// `vike-tradehub`'s `served_features` deliberately omitted the string while the daemon-side
/// renderer was a follow-up, because a capability advertised before its arm works turns a clean
/// client-side refusal into an opaque server error. **That argument was honoured, not abandoned:
/// the arm and the string shipped TOGETHER.** `crates/vike-tradehub/src/server.rs`'s
/// `tearsheet_reply` folds that node's own journal through
/// `vike_report::tearsheet_from_journal`; its `served_features` pushes this string
/// UNCONDITIONALLY — a property of the BUILD rather than of the process, since the arm and its
/// renderer are compiled into every one of them (`crates/vike-tradehub/Cargo.toml` takes
/// `vike-report` as a non-optional dependency) — and that file's
/// `the_tearsheet_capability_is_advertised_now_that_the_arm_serves_it` holds the two equal, so
/// neither can move alone. The closing prediction held exactly: adding the string to that list is
/// all that switched the verb on, with no client change and no version bump.
///
/// So what this constant makes legible now is the COMPLEMENT — a daemon that PREDATES the arm,
/// which is the case a negotiated capability exists for in the first place. The client-side check
/// below is live code for that daemon, not a vestige; it is simply no longer the ordinary outcome.
/// ⚠ A node that DOES advertise this can still answer [`Response::Error`], for a wholly different
/// reason (it cannot resolve its own journal directory), and the two must not be conflated: one is
/// a claim about the BINARY, the other about how that process was started.
///
/// Same forward-compat design as [`FEATURE_STRATEGY_VERBS`]: no [`NODE_PROTO_VERSION`] bump,
/// because the version is folded into the signed auth MAC and a bump breaks the handshake against
/// every running node. A client MUST check for this string before sending the verb
/// ([`crate::remote_handle::tearsheet`] enforces it), so against a node that does not serve it
/// NOTHING goes on the wire.
pub const FEATURE_TEARSHEET: &str = "tearsheet";

/// The `Welcome.features` capability string a node advertises when it **REFUSES a command whose
/// [`crate::wire::WireCommand::addressed_venue`] names no engine it runs**, instead of letting that
/// command fall through to its PRIMARY engine.
///
/// ⚠ **This is the one capability on this list that gates no VERB and no FIELD — it declares what
/// the node does with a field it has always had, and that is exactly why it has to exist.** Every
/// order-carrying variant on this wire has named a venue since the protocol was written, so a
/// client cannot tell a node that HONOURS the name from one that ignores it by looking at any
/// frame: both accept the command and both answer `Ack`. The difference only appears on the
/// venue's books. A node without this string takes `vike_core::CoreThread::route_of`'s historical
/// `unwrap_or(0)` — an unmatched venue routes to engine 0, the primary — so the operator picks a
/// venue, sees no error, and the order is signed on a different exchange. THAT is the direction a
/// capability check has to cover: not "the node cannot decode this", but "the node decodes it and
/// quietly does something else".
///
/// So the client-side rule is the inverse of every capability above: a client does not withhold a
/// VERB, it withholds a venue CHOICE. Against a node that does not advertise this, a client must
/// send only a venue it has positive evidence the node runs (the `WireSnapshot::venues` blocks,
/// one per engine) and refuse the rest locally rather than discover the misroute on a statement.
/// `vike_app_core::backend::tradehub_control::venue_routing_verdict` is that rule, written once.
///
/// No [`NODE_PROTO_VERSION`] bump, for the reason every post-v4 capability carries: the version is
/// folded into the signed auth MAC, so a bump breaks the handshake against every running node. And
/// no wire-SCHEMA change at all here — the field is as old as the protocol.
pub const FEATURE_VENUE_ROUTING: &str = "venue-routing";

/// **This node reads the `account` field on order-carrying commands** — the wire half of
/// `docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md`.
///
/// Advertised **UNCONDITIONALLY** by a node whose build serves the field: a BUILD fact like
/// `crates/vike-datahub-client/src/proto.rs`'s `FEATURE_SEARCH_METHOD`, not a runtime one, because
/// the field and
/// the arm that reads it ship together and no configuration can separate them.
///
/// # ⚠ A client that would SET the field MUST refuse locally, without sending
///
/// This is the inverse of the courtesy every other capability buys. On the search-method verb an
/// unadvertised feature means the server still decodes the frame and the connection survives.
/// **Here it is the only thing standing between a labelled order and the WRONG BOOK**, because an
/// old node cannot know it dropped the field: `#[serde(default)]` turns the absence into
/// `account: None`, which is byte-indistinguishable from a client that named no account — and at
/// `N = 1` that routes, silently, to the venue's only engine. Which may not be the one the operator
/// named.
///
/// So the rule is: check the string, and if it is absent REFUSE before a byte goes on the wire.
/// `crates/vike-cli/src/cmd/mcp.rs`'s `control_rejection` already has the sentence —
/// `ControlRejected::UnsupportedByNode` — and it is the right one: refused client-side, nothing
/// sent, retrying cannot succeed, the operator has to upgrade the node.
///
/// # ⚠ No client may rely on this string any more — six released nodes advertise it falsely
///
/// "The field and the arm that reads it ship together" above was the INTENT, and the release tags
/// say it did not hold: every `vike-tradehub` from `v0.1.27` through `v0.1.32` advertises this
/// string while its `lower_command` builds a `vike_model::OrderRequest` with no account field, so
/// the account decodes off the wire and is discarded before routing. The rule in the section above
/// is right; the string it is written against cannot carry it, because a string a shipped node
/// already advertises can never be withdrawn from that node. So [`crate::remote_control`]'s
/// `required_feature` demands [`FEATURE_ACCOUNT_SCOPED_SUBMIT`] for a labelled `Submit` instead —
/// until a review of the trade CLI's stage-5 deletion measured the tags, it demanded THIS one — and
/// a node keeps advertising this string only for the clients that already check it.
///
/// No [`NODE_PROTO_VERSION`] bump, for the reason every post-v4 capability carries: the version is
/// folded into the signed auth MAC, so a bump breaks the handshake against every running node.
pub const FEATURE_ACCOUNT_ROUTING: &str = "account-routing";

/// **This node ROUTES a `WireCommand::Submit` naming an account onto that account's engine, and
/// REFUSES one naming an account it runs no engine of — at its edge, before the `Ack`.** The order
/// plane's account capability, issued under a string no released node has ever advertised falsely.
///
/// Advertised UNCONDITIONALLY: a property of the BUILD. `crates/vike-tradehub/src/server.rs`'s
/// `lower_command` copying the field onto `vike_model::OrderRequest`, `vike_core`'s routing reading
/// it, and that file's `account_refusal` ship together, and no configuration can separate them.
/// ⚠ One window survives, and it is the edge gate's rather than this string's: an EMPTY engine
/// roster (a core that has not published yet) is treated as UNKNOWN, so inside it an unheld account
/// is Acked and then refused by the core out of band. `account_refusal`'s own doc carries why
/// refusing there instead would deadlock a feed-less node, and
/// `docs/superpowers/specs/2026-09-22-the-order-payload-names-its-account-design.md` §5 names the
/// separate change that closes it.
///
/// # Why its OWN string rather than [`FEATURE_ACCOUNT_ROUTING`]
///
/// Because that one was advertised by released nodes that DROP the field. MEASURED against the
/// release tags, not the code comments: every `vike-tradehub` from `v0.1.27` through `v0.1.32`
/// pushes `account-routing` in `served_features`, while its `lower_command` builds the
/// `OrderRequest` field by field without the account — the type had no such field before `v0.1.33`.
/// The withholding window a comment in `served_features` describes happened inside one pull
/// request and never reached a release. So against those six releases `account-routing` says yes,
/// the frame decodes, the account is discarded, and the order is routed by venue alone: at `N = 1`
/// onto whichever account that one engine is, at `N > 1` Acked and then refused out of band.
///
/// A client cannot repair a string the fleet already advertises; it can only stop trusting it — the
/// argument [`FEATURE_MOUNT_VERBS`] was issued under, where a B4-era node advertises
/// `strategy-verbs` yet cannot decode the mount variants.
///
/// # ⚠ A client that would SET the field on a `Submit` MUST refuse locally without this string
///
/// [`crate::remote_control`]'s `required_feature` does, for EVERY named account — the positive
/// `DEFAULT` included, because a released node that drops the field routes a `DEFAULT`-named submit
/// by venue alone too, and where the venue's only engine is a LABELLED account that reaches the
/// labelled book. A submit naming NO account serialises byte-identically to the pre-field frame and
/// owes nothing, so it keeps flowing to every node.
///
/// No [`NODE_PROTO_VERSION`] bump, for the reason every post-v4 capability carries: the version is
/// folded into the signed auth MAC, so a bump breaks the handshake against every running node.
pub const FEATURE_ACCOUNT_SCOPED_SUBMIT: &str = "account-scoped-submit";

/// **This node reads the `account` field on the RUNTIME MOUNT verb** —
/// `WireCommand::MountStrategy`, the declaration half of
/// `docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md` (§5.2).
///
/// Advertised UNCONDITIONALLY by a node whose build serves the field, like
/// [`FEATURE_ACCOUNT_ROUTING`] and for the same reason: the field and the arm that reads it ship
/// together and no configuration can separate them. A node advertising this string ALWAYS
/// advertises [`FEATURE_MOUNT_VERBS`] too — the field lives ON that verb, so a build serving one
/// serves the other by construction. That is what lets
/// [`crate::remote_control`]'s `required_feature` answer with ONE string: this one is strictly the
/// stronger claim.
///
/// # Why its OWN string rather than [`FEATURE_ACCOUNT_ROUTING`]
///
/// That capability is about ORDER-carrying commands, and the two were built one stage apart: a node
/// from the stage in between advertises `account-routing` honestly — it really does route a
/// labelled `Submit` — and still drops this field, because the mount verb had not grown it yet.
/// Riding the order string would make that node look like it honours a mount's account when it
/// silently mounts on the default one. ⚠ "Honestly — it really does route a labelled `Submit`" is
/// true of no RELEASED node that predates this string: the two shipped together in `v0.1.27`, and
/// until `v0.1.33` the `Submit` half was false ([`FEATURE_ACCOUNT_SCOPED_SUBMIT`]'s doc). The
/// argument for a separate string survives; the node it imagines was never released.
///
/// # Why its OWN string rather than [`FEATURE_MOUNT_VERBS`]
///
/// The mount-verbs argument is the DECODE one: a B4-era node cannot decode the variant at all and
/// answers `Response::Error`. This field is the opposite failure and the more dangerous one — a
/// mount-verbs node decodes the frame PERFECTLY, `#[serde(default)]` turns the absent field into
/// `account: None`, and the mount lands on the venue's default account while the operator reads a
/// normal acknowledgement. `FEATURE_STRATEGY_PARAMS`'s doc states the standing rule for exactly
/// this class: **check the capability, never the emptiness of a field.**
///
/// # ⚠ A client that would SET the field MUST refuse locally, without sending
///
/// Verbatim [`FEATURE_ACCOUNT_ROUTING`]'s rule, and owed here for a reason that outlives a single
/// order: a misrouted mount is not one order on the wrong book, it is EVERY order that mount will
/// ever place, for as long as it runs. A mount naming NO account still serialises byte-identically
/// to the pre-field frame, so it owes no capability and keeps flowing to every older node.
///
/// No [`NODE_PROTO_VERSION`] bump, for the reason every post-v4 capability carries: the version is
/// folded into the signed auth MAC, so a bump breaks the handshake against every running node.
pub const FEATURE_MOUNT_ACCOUNT: &str = "strategy-mount-account";

/// **This node narrows an ACCOUNT-SCOPED RISK-REDUCING verb to the account its payload names** —
/// `WireCommand::MassCancel`, `WireCommand::Flatten` and `WireCommand::MarketExit` with
/// `account: Some(..)`.
///
/// A node advertising it does three things for such a frame, and ships them together (owner ruling
/// "B", 2026-09-26): `crates/vike-tradehub/src/server.rs`'s `lower_command` carries the field onto
/// `vike_exec::OrderIntent`'s reducing variants; `vike_core`'s `CoreThread::reduce_route_for_account`
/// resolves it through `vike_model::account_keys::route_key_of` to ONE engine, so the verb reaches
/// that book and no other; and the node's `account_refusal` refuses, before the Ack, an account it
/// does not hold — and an account named with NO venue, which resolves on no roster and would
/// otherwise reach the global arm. Advertised unconditionally: a property of the BUILD.
///
/// ⚠ **`account: None` owes this string nothing and is unchanged by it**, and that is a law rather
/// than a nicety: `docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md`
/// §4.5 rules that a risk-REDUCING venue verb naming NO account fans out to every account of that
/// venue deliberately — *"a fan-out can never reach an account the sender did not mean, because the
/// sender meant all of them"* — and the UNSCOPED panic button (`MarketExit { venue: None, account:
/// None }`) must reach every engine and can never be refused by any gate in this family. Naming an
/// account is an opt-in narrowing, never a precondition for a way out.
///
/// # Why its OWN string rather than [`FEATURE_ACCOUNT_ROUTING`]
///
/// The two were built one ruling apart, and a node from the build in between advertises
/// `account-routing` TRUTHFULLY — it routes a labelled `Submit` — while it still DROPS the field on
/// these three: its `lower_command` destructured them as `{ venue, symbol, .. }` and its core fanned
/// the verb over every account of the venue, so a `market-exit binance ALT` on a two-account node
/// cancelled the DEFAULT account's orders, flattened its positions, and answered `accepted`. Riding
/// the order string would make that node look like it honours the narrowing. So
/// [`crate::remote_control`]'s `required_feature` demands THIS string for a labelled reduce and
/// refuses the send locally against any node that does not carry it — the refusal every conforming
/// client needs against an older node, not a stopgap.
///
/// ⚠ **This string was advertised by NO node until the three halves above shipped**, deliberately,
/// exactly as [`FEATURE_ACCOUNT_ROUTING`] was withheld for the window between its field arriving and
/// routing reading it: a node that claimed the capability while fanning the verb out would have told
/// a conforming client (check the string, then set the field) yes and flattened books nobody named.
///
/// No [`NODE_PROTO_VERSION`] bump, for the reason every post-v4 capability carries: the version is
/// folded into the signed auth MAC, so a bump breaks the handshake against every running node.
pub const FEATURE_ACCOUNT_SCOPED_REDUCE: &str = "account-scoped-reduce";

/// The prefix of the ONE value-carrying `Welcome.features` entry (split-plane REQ-2): a daemon
/// configured with `config.toml`'s `datahub_advertise_addr` advertises `datahub=<addr>` — the
/// CLIENT-side DIAL address of the `vike-datahub` data service this backend fronts — so an
/// operator configures ONE address (the daemon's) and the client dials the data plane itself.
/// Advertisement, NEVER proxying: backtest/history traffic does not flow through the process
/// holding live orders (spec §1, "One address, two processes").
///
/// Unlike every constant above, this entry carries a VALUE. That is safe against every existing
/// capability check because those are all whole-string equality per feature name
/// ([`crate::remote_control`]'s `required_feature` gate and the `remote_handle` verb guards both
/// compare `f == feature`) — a `datahub=<addr>` entry can never satisfy nor shadow a named
/// capability. Build the entry with [`datahub_feature`] and read it with [`advertised_datahub`];
/// the pair is the round-trip authority (pinned by test), so the server and the client cannot
/// drift on the spelling.
pub const FEATURE_DATAHUB_PREFIX: &str = "datahub=";

/// **The ACCOUNT-ADMIN verbs** (`docs/decisions/0065-accounts-are-managed-and-the-barrier-is-
/// declared.md`): [`Request::Account`], answered by [`Response::AccountList`] /
/// [`Response::AccountWritten`].
///
/// ⚠ **This is the ONLY capability on this list a node withholds CONDITIONALLY, and the condition
/// is the whole design.** Every string above is a property of the BUILD — a node that compiled the
/// verb advertises it and answers an honest [`Response::Error`] if its source is absent (the
/// `settings-show` argument). This one is advertised only when the daemon's own three-valued
/// declaration ARMED the capability, because there is nothing honest to answer with when it is not:
/// the server holds no writer, and 0065 Part 1 is that the verb is refused *because there is
/// nothing in the process to refuse WITH*. A client that does not see this string refuses
/// CLIENT-side and sends nothing — the shape every post-v4 capability already uses.
///
/// ⚠ Seeing it is NOT authorization. Every account verb additionally requires `Scope::Account`, a
/// THIRD key ([`crate::auth::ADMIN_KEY_ENV`]) whose tag is inside the signed preimage — so a
/// Control peer that reads this advertisement still cannot authenticate for it.
///
/// No [`NODE_PROTO_VERSION`] bump, for the reason every post-v4 capability carries: the version is
/// folded into the signed auth MAC, so a bump breaks the handshake against every running node.
pub const FEATURE_ACCOUNT_VERBS: &str = "account-verbs";

/// Render the datahub advertisement entry for `Welcome.features`: `datahub=<addr>`. The server
/// side (vike-tradehub's `served_features`) formats through this so the spelling has one
/// authority. `addr` is the CLIENT-side dial address of the datahub this backend fronts,
/// verbatim — validation (host:port) happened at the config layer.
pub fn datahub_feature(addr: &str) -> String {
    format!("{FEATURE_DATAHUB_PREFIX}{addr}")
}

/// Parse the datahub advertisement out of a `Welcome.features` list: the first entry starting
/// with [`FEATURE_DATAHUB_PREFIX`], its value trimmed; `None` when no entry carries the prefix or
/// the value is empty/whitespace (an empty advertisement advertises nothing — the same
/// "absence is the answer" shape as an unset `config.datahub_addr`). A plain `"datahub"` entry
/// without the `=` is NOT an advertisement.
pub fn advertised_datahub(features: &[String]) -> Option<String> {
    features
        .iter()
        .find_map(|f| f.strip_prefix(FEATURE_DATAHUB_PREFIX))
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string)
}

/// A client-to-server request on the node connection.
///
/// The variants fall into two phases: the pre-auth handshake ([`Request::Hello`], [`Request::Auth`])
/// and the post-auth session ([`Request::Subscribe`] / [`Request::Snapshot`] / [`Request::Command`]
/// / [`Request::Ping`]). A conforming server MUST reject every session verb until an
/// [`Response::AuthOk`] has been issued, and MUST reject [`Request::Command`] unless the
/// authenticated scope is [`Scope::Write`]. (That enforcement lives in the future server; this
/// crate only defines the schema.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    /// Open the handshake, declaring the client's [`NODE_PROTO_VERSION`]. Answered by
    /// [`Response::Welcome`] (which carries the connection nonce), or [`Response::Error`].
    Hello {
        /// The client's node protocol version.
        proto_version: u32,
    },
    /// Answer the nonce challenge: `mac` is the HMAC over `(key_for(scope), nonce, proto_version,
    /// scope)` per [`crate::auth::sign`]. Answered by [`Response::AuthOk`] or
    /// [`Response::AuthDenied`].
    Auth {
        /// The capability ceiling this client is authenticating under.
        scope: Scope,
        /// The HMAC-SHA256 tag over the connection's nonce challenge (raw bytes; serialized as a
        /// JSON array of `u8`).
        mac: Vec<u8>,
    },
    /// Subscribe to a set of live [`Topic`]s (post-auth). A conforming server streams matching
    /// [`Response::SnapshotFrame`]s thereafter; the exact push cadence is a server concern.
    Subscribe {
        /// The topics to receive.
        topics: Vec<Topic>,
    },
    /// Request one point-in-time [`WireSnapshot`] (post-auth). Answered by
    /// [`Response::SnapshotFrame`].
    Snapshot,
    /// Submit one order-scoped command (post-auth, [`Scope::Write`] only). Answered by
    /// [`Response::Ack`] (the minted/echoed coid) or [`Response::Error`].
    ///
    /// `reason` (v4) is an OPTIONAL free-text operator/agent RATIONALE — *why* this command was
    /// issued. It rides BESIDE the command, never inside it: the node records it in its audit trail
    /// (`vike_tradehub::audit`) and it NEVER reaches `OrderRequest`, the core fold, the journal, or
    /// any venue. `None` (the default every pre-v4 caller had) records nothing extra.
    ///
    /// It is REMOTE FREE TEXT landing in a structured JSON log line, so the node SANITIZES it before
    /// recording (control characters stripped, length capped) — see
    /// `vike_tradehub::audit::sanitize_reason`. A client should not rely on the exact bytes it sent
    /// appearing verbatim in the trail.
    Command {
        /// The order-scoped command to execute.
        cmd: WireCommand,
        /// Optional operator/agent rationale, recorded (sanitized) in the node's audit trail.
        reason: Option<String>,
    },
    /// DRY-RUN a command (post-auth, [`Scope::Write`] only): ask the node what its server-side
    /// gate would decide WITHOUT executing anything. The command is NOT lowered, NOT handed to the
    /// core, and NO order is placed — the node runs only its edge validation (the notional/rate
    /// `ControlLimits`-shaped checks) and answers [`Response::Preview`] with the verdict. Same scope
    /// gating as [`Request::Command`]: an [`Scope::Read`] peer is refused. Purely read-only.
    Preview(WireCommand),
    /// Liveness probe (post-auth). Answered by [`Response::Pong`].
    Ping,
    /// STRATEGY-level read (split-plane B4, post-auth, EITHER scope — read-only, so
    /// [`Scope::Read`] suffices): what is this node running? Answered by
    /// [`Response::StrategyStatus`] (identity + effective params + the mounted-strategy rows), or
    /// [`Response::Error`] on a node that publishes no identity block. Feature-gated CLIENT-side on
    /// [`FEATURE_STRATEGY_VERBS`] — see that constant for why there is no version bump.
    StrategyStatus,
    /// SETTINGS read (split-plane REQ-7, read half; post-auth, EITHER scope — read-only and
    /// redacted-by-construction, so [`Scope::Read`] suffices; the scope argument lives at the
    /// server arm). Ask the node for its effective settings-file rows — the same rows
    /// `vike-cli config show`'s files table prints, rendered by the node from the shared
    /// `vike_config::show` builder. Answered by [`Response::SettingsShow`], or
    /// [`Response::Error`] on a node started without a settings source or whose settings no
    /// longer load. Feature-gated CLIENT-side on [`FEATURE_SETTINGS_SHOW`] — see that constant
    /// for why there is no version bump. The WRITE half is `WireCommand::SetSetting` (a
    /// [`Request::Command`] payload, gated on [`FEATURE_SETTINGS_WRITE`]), answered by
    /// [`Response::SettingsWritten`].
    SettingsShow,
    /// DIRECTORY read (post-auth, EITHER scope; read-only, so [`Scope::Read`] suffices): the roster
    /// venues the node's settings database names and the active accounts it holds. See
    /// [`crate::wire::WireDirectory`] for what it carries and why the observe key may read it (the
    /// owner's ruling of 2026-09-30). Answered by [`Response::Directory`], or by [`Response::Error`]
    /// — never by an empty directory — when the node has nothing to list the accounts from: it was
    /// started without a settings source or resolved no settings directory, it has no settings
    /// database (or one that predates the `account` table), or its database will not read. No error
    /// text carries a filesystem path. Feature-gated CLIENT-side on [`FEATURE_DIRECTORY`].
    Directory,
    /// LIVE-JOURNAL REPORT read (ruling 16; post-auth, EITHER scope — read-only, so
    /// [`Scope::Read`] suffices, exactly like [`Request::StrategyStatus`]). Ask the node to
    /// render a tearsheet from the journal IT is writing, and reply with [`Response::Tearsheet`]
    /// (or [`Response::Error`]).
    ///
    /// ⚠ **The JOURNAL never crosses the wire — the ANSWER does**, which is the same
    /// compute-to-data argument `vike-datahub`'s `Request::RunBacktest` is built on: a live
    /// journal is an append-only fill stream that grows for the life of the deployment, and the
    /// tearsheet computed from it is a few dozen numbers. The node reads its own journal, folds it
    /// through the shared reconstruction (`vike_report::tearsheet_from_journal` over
    /// `vike_analytics::reconstruct_trades`), and sends the summary.
    ///
    /// Feature-gated CLIENT-side on [`FEATURE_TEARSHEET`] — see that constant for why there is no
    /// version bump. ⚠ That sentence used to close *"and for why no shipped node advertises it
    /// yet"*: a node built from this tree DOES advertise it (`crates/vike-tradehub/src/server.rs`'s
    /// `tearsheet_reply` is the arm), so the negotiation is now about whether the peer PREDATES
    /// that arm rather than about whether any node serves the verb at all.
    Tearsheet {
        /// The account's starting capital, which sets the base of the realized-only equity curve
        /// and therefore scales `total_return`/`cagr`/`sharpe`. `None` ⇒ the renderer's own
        /// default (`vike_report::tearsheet_cli`'s `--seed`), so a caller with no opinion sends
        /// nothing rather than guessing a number the node already has an answer for.
        seed: Option<f64>,
        /// The Sharpe/Sortino/Calmar/CAGR annualization factor. `None` ⇒ the renderer's own
        /// default (`--periods-per-year`), for the same reason as `seed`.
        periods_per_year: Option<f64>,
    },
    /// **ACCOUNT ADMINISTRATION** (`docs/decisions/0065-accounts-are-managed-and-the-barrier-is-
    /// declared.md`; post-auth, `Scope::Account` ONLY). The settings database's `account` table, plus
    /// the one verb on this wire that carries a credential VALUE. Answered by
    /// [`Response::AccountList`], [`Response::AccountWritten`] or [`Response::Error`].
    ///
    /// ⚠ **A `Request` variant rather than a `WireCommand` payload**, and the separation is
    /// structural: [`crate::wire::WireCommand`] is a mirror of the CORE-facing verbs, every one of
    /// which is lowered into the core, vetted as an order and routed by a venue — none of which
    /// applies here. The load-bearing half is that `WireCommand` DERIVES `Debug` and a
    /// value-carrying variant may not ride it; [`crate::wire::AccountRequest`] carries its own
    /// redacting impl instead. See that type for the three parts of the barrier.
    ///
    /// Feature-gated CLIENT-side on [`FEATURE_ACCOUNT_VERBS`], which a node advertises only when
    /// its own declaration ARMED the capability — see that constant for why this one is
    /// conditional where every other is a property of the build.
    Account(Box<crate::wire::AccountRequest>),
}

/// A server-to-client response on the node connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    /// Reply to [`Request::Hello`]: the server's protocol version, the 32-byte connection `nonce`
    /// the client must sign, and the server's advertised `features` (free-form capability strings,
    /// e.g. `"subscribe"`, forward-compat room for optional verbs).
    Welcome {
        /// The server's node protocol version.
        proto_version: u32,
        /// 32 fresh random bytes for THIS connection — the anti-replay auth challenge. Serialized
        /// as a fixed-length JSON array of 32 `u8`s (see the crate-doc note on the `[u8; 32]`
        /// choice).
        nonce: [u8; 32],
        /// Free-form advertised capability strings (forward compatibility).
        features: Vec<String>,
    },
    /// Reply to a valid [`Request::Auth`]: the handshake succeeded and the session is now open at
    /// the granted `scope`.
    AuthOk {
        /// The scope actually granted (echoes the client's requested scope on success).
        scope: Scope,
    },
    /// Reply to an INVALID [`Request::Auth`] (bad mac / unknown-or-absent key for the scope /
    /// version skew): the reason is a human-readable string, never leaking key material.
    AuthDenied {
        /// A non-sensitive reason string.
        reason: String,
    },
    /// A snapshot payload — the answer to [`Request::Snapshot`] and the push shape for a
    /// [`Request::Subscribe`]d topic. BOXED since the identity block (split-plane B3) pushed the
    /// inline size over `clippy::large_enum_variant`'s bar — the same treatment as
    /// `Response::Properties` (since removed) had; serde treats `Box<T>` transparently, so the wire shape is
    /// identical to an unboxed `WireSnapshot`.
    SnapshotFrame(Box<WireSnapshot>),
    /// Reply to a [`Request::Command`]: the client-order-id the command minted or echoed.
    Ack {
        /// The (minted or echoed) client-order-id the command produced.
        coid: String,
    },
    /// Reply to [`Request::Preview`]: the server-authoritative dry-run verdict. `accepted` is `true`
    /// when the command WOULD pass the node's edge gate (nothing was executed either way); `reason`
    /// carries the human-readable refusal cause when `accepted` is `false`, and is `None` on accept.
    Preview {
        /// Whether the command would be accepted by the node's edge gate.
        accepted: bool,
        /// The refusal reason when `accepted` is `false`; `None` when accepted.
        reason: Option<String>,
    },
    /// A generic server-side failure (bad request phase, malformed command, …), stringified.
    Error(String),
    /// Reply to [`Request::Ping`].
    Pong,
    /// Reply to [`Request::StrategyStatus`] (split-plane B4): the node's identity, its resolved
    /// effective-params line, and one [`crate::wire::WireMountRow`] per mounted strategy (exactly
    /// one today; the `Vec` is the multi-mount forward design). BOXED like
    /// [`Response::SnapshotFrame`] — the identity block alone is five strings — and serde treats
    /// `Box<T>` transparently, so the wire shape is identical to an unboxed payload.
    StrategyStatus(Box<WireStrategyStatus>),
    /// Reply to [`Request::SettingsShow`] (split-plane REQ-7, read half): the node's effective
    /// settings-file rows, every cell rendered (and redacted) by the node. BOXED like its
    /// siblings — the payload is a whole settings table — and serde treats `Box<T>`
    /// transparently, so the wire shape is identical to an unboxed payload.
    SettingsShow(Box<WireSettingsShow>),
    /// Reply to [`Request::Directory`].
    Directory(Box<crate::wire::WireDirectory>),
    /// Reply to an ACCEPTED `WireCommand::SetSetting` (split-plane REQ-7, write half) — the one
    /// command whose acceptance is not an [`Response::Ack`]: nothing entered the core and there
    /// is no coid to echo; what the caller needs to know is whether the value is LIVE yet.
    /// `restart_required: false` means the node HOT-APPLIED the value (REQ-7 v2 — the node's
    /// own per-key classification decides which keys are hot-safe; policy keys never are), and
    /// is only ever sent when the apply actually executed; `true` means the row committed and the
    /// running node keeps its boot-time value until restarted. A refusal (an unknown key, a value
    /// the key cannot take, the loader rejecting the store with the new row in it, a busy
    /// database) is the ordinary [`Response::Error`] carrying the cause, and leaves the database
    /// byte-identical. There is no confirm to refuse on: `docs/decisions/0086` point 7.
    SettingsWritten {
        /// `true` ⇒ the row committed but the running node still trades on its boot-time value for
        /// this key — it applies at the next restart. `false` ⇒ the node applied it live.
        restart_required: bool,
    },
    /// Reply to [`Request::Tearsheet`] (ruling 16): the node's LIVE tearsheet, as the
    /// `vike_analytics::LiveTearsheet` JSON TEXT — the same document
    /// `vike-backend report --json` emits on the daemon's own box.
    ///
    /// ⚠ **A `String`, not a typed payload.** The first reason given here was a DEPENDENCY: the
    /// typed shape lived in `vike-report`, which links the journal, the exec lane and the data
    /// layer, and this wire crate is the LIGHT one every client links
    /// (`scripts/ci_feature_suite.sh`'s `light-consumers` lane keeps `vike-cli`'s graph free of
    /// exactly that closure). That reason
    /// EXPIRED on 2026-09-28: the type moved to `vike-analytics`, whose only vike edge is
    /// `vike-model`, so naming it here would no longer charge a client for a journal reader —
    /// typing this payload is now a choice rather than a cost, and it has not been made. The
    /// second reason stands on its own: carrying the JSON verbatim is the shape
    /// `crates/vike-datahub-client/src/proto.rs`'s `Response::Report` and
    /// `Response::ParamscanReport` already use, so a machine consumer gets the ONE schema the
    /// producer owns, never a second hand-maintained copy of it.
    Tearsheet(String),
    /// Reply to an [`Request::Account`] whose verb was [`crate::wire::AccountVerb::List`]: the
    /// node's `account` rows and the credential key NAMES each owns. **Never a value** — the
    /// reader behind it selects no `value` column at all.
    AccountList(Box<crate::wire::WireAccountList>),
    /// Reply to an ACCEPTED account WRITE — the second command on this wire whose acceptance is
    /// not an [`Response::Ack`], for [`Response::SettingsWritten`]'s reason: nothing entered the
    /// core and there is no coid to echo. A refusal (a missing or mismatched typed confirm, a key
    /// outside the grid, a row that still owns credentials, an unmigrated box) is the ordinary
    /// [`Response::Error`] carrying the cause — which names key NAMES and never a value.
    AccountWritten(Box<crate::wire::WireAccountWritten>),
}

/// The capability ceiling a client authenticates under. The scope is bound INTO the signed auth
/// message (see [`crate::auth::sign`]) and each scope signs under its OWN key
/// ([`crate::auth::NodeKeys`]), so a client holding only the observe key can never forge a `Control`
/// mac — capability is cryptographic, not a claim the server has to trust.
///
/// On THIS protocol: [`Scope::Read`] is snapshots + subscriptions and may NOT issue
/// [`Request::Command`]; [`Scope::Write`] additionally admits order commands.
///
/// ⚠ RE-EXPORTED, not declared here, since `docs/decisions/0025-datahub-remote-posture.md` gave the
/// datahub the same handshake: the scope TAG is folded into the signed message, so two `Scope`
/// enums would be two tag tables — exactly the disagreement a shared primitive exists to make
/// impossible. The variant names ARE the wire and are unchanged, so the move is invisible here.
pub use vike_node_proto::auth::Scope;

/// A live-stream topic a client may [`Request::Subscribe`] to. Kept deliberately small and
/// coarse-grained — the node publishes a whole coalesced [`WireSnapshot`], so these select WHICH
/// blocks of it a subscriber cares to be pushed:
///
/// - [`Topic::Orders`] — the order registry (OMS state changes).
/// - [`Topic::Positions`] — the per-venue position/equity blocks.
/// - [`Topic::Events`] — the recent-events feed (the bounded journal tail).
/// - [`Topic::All`] — every block (the whole snapshot); the simplest subscriber's single topic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Topic {
    /// Order-registry changes.
    Orders,
    /// Position / equity changes.
    Positions,
    /// The recent-events feed.
    Events,
    /// Everything (the whole snapshot).
    All,
}

#[path = "proto_tests.rs"]
#[cfg(test)]
mod proto_tests;
