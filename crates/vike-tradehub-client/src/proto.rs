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
//! This crate is the primitive built in isolation before anything listens: it defines the schema,
//! the framing re-export, and the auth math + tests. There is NO server loop and NO socket in this
//! crate — a server binding this proto lands in a later PR.

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
/// `vike_report::LiveTearsheet::from_journal`; its `served_features` pushes this string
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
/// `vike_app_core::tradehub_control::venue_routing_verdict` is that rule, written once.
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
/// No [`NODE_PROTO_VERSION`] bump, for the reason every post-v4 capability carries: the version is
/// folded into the signed auth MAC, so a bump breaks the handshake against every running node.
pub const FEATURE_ACCOUNT_ROUTING: &str = "account-routing";

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
/// silently mounts on the default one.
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

/// **This node routes an ACCOUNT-SCOPED RISK-REDUCING verb onto the account its payload names** —
/// `WireCommand::MassCancel`, `WireCommand::Flatten` and `WireCommand::MarketExit` with
/// `account: Some(..)`.
///
/// ⚠ **NO NODE ADVERTISES THIS STRING TODAY, DELIBERATELY, AND THAT IS THE POINT OF IT EXISTING.**
/// It is absent from `crates/vike-tradehub/src/server.rs`'s `served_features` on purpose, so that
/// [`crate::remote_control`]'s `required_feature` refuses these three verbs client-side whenever
/// they name an account. The string is the SHAPE of a claim this node cannot yet make, written down
/// so the claim can be made in one line the day it becomes true.
///
/// # What the node does NOT do — read this before restoring the claim
///
/// The field reaches the daemon and stops there. `crates/vike-tradehub/src/server.rs`'s
/// `lower_command` destructures these three variants as `{ venue, symbol, .. }` and builds
/// `vike_exec::OrderIntent::MassCancel { venue, symbol }` — the `..` swallows the account, because
/// **those `OrderIntent` variants carry no account field at all**. `vike_core`'s
/// `CoreThread::exit_scope_engines` then resolves the venue to EVERY engine of that exchange. So a
/// `market-exit binance ALT` on a two-account node cancels the DEFAULT account's orders and
/// flattens its positions too, and answers `accepted`.
///
/// # What must be true before this is advertised
///
/// Three things, and the first two are one change:
///
/// 1. `vike_exec::OrderIntent`'s three reducing variants grow an
///    `Option<vike_model::account_keys::AccountLabel>`, and every construction site is updated.
/// 2. `CoreThread::exit_scope_engines` takes that account and resolves
///    `vike_model::account_keys::route_key_of(venue, account)` to ONE engine when it is `Some`,
///    keeping its EXISTING fan-out untouched when it is `None`.
/// 3. `lower_command` stops dropping the field.
///
/// ⚠ **Step 2's `None` arm must stay byte-identical**, and that is a law rather than a nicety:
/// `docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md` §4.5 rules
/// that a risk-REDUCING venue verb naming NO account fans out to every account of that venue
/// deliberately — *"a fan-out can never reach an account the sender did not mean, because the
/// sender meant all of them"* — while a risk-INCREASING one refuses. And the UNSCOPED panic button
/// (`MarketExit { venue: None }`) names no venue either, so it must reach every engine and can
/// never be refused by any gate in this family.
///
/// # Why the claim is withdrawn rather than the verb refused at the node
///
/// A node refusing a verb its own advertised capability says it serves is a second defect in a
/// safer coat: the client did everything right. Withdrawing the claim tells a conforming client the
/// truth, costs an operator one clear local refusal instead of a silent wrong-book flatten, and is
/// exactly what [`FEATURE_ACCOUNT_ROUTING`]'s own history did — that string was WITHHELD for the
/// window between the field arriving and the routing reading it, then restored in one line.
///
/// ⚠ **This doc is the thing to delete when the claim becomes true.** A withholding comment that
/// outlives its reason is how the next reader learns a false rule; the order plane's was removed
/// the moment routing read the field, and so should this be.
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
    /// LIVE-JOURNAL REPORT read (ruling 16; post-auth, EITHER scope — read-only, so
    /// [`Scope::Read`] suffices, exactly like [`Request::StrategyStatus`]). Ask the node to
    /// render a tearsheet from the journal IT is writing, and reply with [`Response::Tearsheet`]
    /// (or [`Response::Error`]).
    ///
    /// ⚠ **The JOURNAL never crosses the wire — the ANSWER does**, which is the same
    /// compute-to-data argument `vike-datahub`'s `Request::RunBacktest` is built on: a live
    /// journal is an append-only fill stream that grows for the life of the deployment, and the
    /// tearsheet computed from it is a few dozen numbers. The node reads its own journal, folds it
    /// through `vike_report`'s reconstruction, and sends the summary.
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
    /// [`Response::Properties`]; serde treats `Box<T>` transparently, so the wire shape is
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
    /// Reply to an ACCEPTED `WireCommand::SetSetting` (split-plane REQ-7, write half) — the one
    /// command whose acceptance is not an [`Response::Ack`]: nothing entered the core and there
    /// is no coid to echo; what the caller needs to know is whether the value is LIVE yet.
    /// `restart_required: false` means the node HOT-APPLIED the value (REQ-7 v2 — the node's
    /// own per-key classification decides which keys are hot-safe; policy keys never are), and
    /// is only ever sent when the apply actually executed; `true` means the write landed on
    /// disk and the running node keeps its boot-time value until restarted. A refusal (bad
    /// file/key/value, a policy write without its typed confirm, the loader rejecting the
    /// would-be file) is the ordinary [`Response::Error`] carrying the cause.
    SettingsWritten {
        /// `true` ⇒ the write landed on disk but the running node still trades on its boot-time
        /// value for this key — restart the backend to apply. `false` ⇒ the node applied it live.
        restart_required: bool,
    },
    /// Reply to [`Request::Tearsheet`] (ruling 16): the node's LIVE tearsheet, as the
    /// `vike_report::LiveTearsheet` JSON TEXT — the same document
    /// `vike-backend report --json` emits on the daemon's own box.
    ///
    /// ⚠ **A `String`, not a typed payload, and the reason is a dependency rather than laziness.**
    /// The typed shape lives in `vike-report`, which links `vike-core`/`vike-exec`/`vike-data`;
    /// this wire crate is the LIGHT one every client links (`scripts/ci_feature_suite.sh`'s
    /// `light-consumers` lane exists to keep `vike-cli`'s graph free of exactly that closure), so
    /// naming the type here would charge every client of this protocol for the renderer. Carrying
    /// the JSON verbatim is the shape `crates/vike-datahub-client/src/proto.rs`'s
    /// `Response::Report` and
    /// `Response::ParamscanReport` already use for the same reason: a machine consumer gets the ONE
    /// schema the producer owns, never a second hand-maintained copy of it.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every wire message must round-trip byte-stably through JSON (the frame payload codec). This
    /// also pins that `[u8; 32]` (the `Welcome` nonce) survives a serde round-trip under the derive.
    #[test]
    fn request_response_round_trip_through_json() {
        use crate::wire::{WireCommand, WireTradingState};

        let reqs = vec![
            Request::Hello { proto_version: NODE_PROTO_VERSION },
            Request::Auth { scope: Scope::Write, mac: vec![1, 2, 3, 4] },
            Request::Subscribe { topics: vec![Topic::Orders, Topic::All] },
            Request::Snapshot,
            // The v4 Command shape, BOTH rationale arms — `None` (every pre-v4 caller's shape) and
            // `Some` (an operator/agent rationale riding beside the command).
            Request::Command { cmd: WireCommand::Cancel("c-1".into()), reason: None },
            Request::Command {
                cmd: WireCommand::Cancel("c-2".into()),
                reason: Some("stale quote after the feed gap".into()),
            },
            Request::Preview(WireCommand::SetTradingState(WireTradingState::Halted)),
            Request::Ping,
            // The B4 strategy verbs: the read as its own request, the write as a Command payload.
            Request::StrategyStatus,
            // The REQ-7 settings read.
            Request::SettingsShow,
            Request::Command {
                cmd: WireCommand::UpdateParams {
                    venue: "binance".into(),
                    symbol: "BTCUSDT".into(),
                    interval: "1m".into(),
                    params: serde_json::json!({"SpreadMaker": {"qty": 2.0}}),
                },
                reason: Some("widen ahead of the print".into()),
            },
            // The B5 mount verbs: both source arms (registry name / rhai path) and the unmount.
            Request::Command {
                cmd: WireCommand::MountStrategy {
                    venue: "binance".into(),
                    // NAMED here and ABSENT on the mount below, so this round trip carries both
                    // wire states of the field rather than only the one that changes no bytes.
                    account: Some("ALT".into()),
                    symbol: "BTCUSDT".into(),
                    interval: "1m".into(),
                    controller_id: Some("grid-a".into()),
                    name: Some("grid".into()),
                    rhai: None,
                    params: serde_json::json!({"qty": 1.0}),
                },
                reason: Some("second mount for the session".into()),
            },
            Request::Command {
                cmd: WireCommand::MountStrategy {
                    venue: "binance".into(),
                    account: None,
                    symbol: "BTCUSDT".into(),
                    interval: "1m".into(),
                    controller_id: None,
                    name: None,
                    rhai: Some("strategies/breaker.rhai".into()),
                    params: serde_json::json!({}),
                },
                reason: None,
            },
            Request::Command {
                cmd: WireCommand::UnmountStrategy { controller_id: "grid-a".into() },
                reason: Some("done for the day".into()),
            },
            // The REQ-7 settings WRITE: a Command payload like every control verb — a non-policy
            // edit (no confirm) and a policy edit carrying its typed confirm.
            Request::Command {
                cmd: WireCommand::SetSetting {
                    file: "config.toml".into(),
                    key: "config.tradehub_addr".into(),
                    value: "127.0.0.1:7879".into(),
                    confirm: None,
                },
                reason: None,
            },
            Request::Command {
                cmd: WireCommand::SetSetting {
                    file: "policy.toml".into(),
                    key: "policy.max_notional_per_order".into(),
                    value: "250".into(),
                    confirm: Some("policy.max_notional_per_order".into()),
                },
                reason: Some("tighter cap for the weekend".into()),
            },
            // The ruling-16 report verb, BOTH optional arms: absent (the caller has no opinion and
            // the node's own defaults apply) and present. The absent arm is the load-bearing one —
            // a `None` that encoded as anything other than JSON `null` would have the node silently
            // rescale every return ratio in the answer.
            Request::Tearsheet { seed: None, periods_per_year: None },
            Request::Tearsheet { seed: Some(25_000.0), periods_per_year: Some(365.0) },
        ];
        for r in reqs {
            let js = serde_json::to_string(&r).unwrap();
            let back: Request = serde_json::from_str(&js).unwrap();
            assert_eq!(r, back);
        }

        let resps = vec![
            Response::Welcome {
                proto_version: NODE_PROTO_VERSION,
                nonce: [7u8; 32],
                features: vec!["subscribe".into()],
            },
            Response::AuthOk { scope: Scope::Read },
            Response::AuthDenied { reason: "bad mac".into() },
            Response::Ack { coid: "c-1".into() },
            // Both Preview verdicts: accepted (no reason) and rejected (a reason).
            Response::Preview { accepted: true, reason: None },
            Response::Preview { accepted: false, reason: Some("rate limited".into()) },
            Response::Error("nope".into()),
            Response::Pong,
            Response::StrategyStatus(Box::new(crate::wire::WireStrategyStatus {
                identity: crate::wire::WireNodeIdentity {
                    name: "the build runner".into(),
                    strategy: "spread_maker".into(),
                    params: "qty=1".into(),
                    live: false,
                    build: "test-build".into(),
                    advertise_addr: String::new(),
                },
                effective_params: "qty=1".into(),
                mounts: vec![crate::wire::WireMountRow {
                    strategy: "spread_maker".into(),
                    params: "qty=1".into(),
                    live: false,
                    venue: "binance".into(),
                    symbol: "BTCUSDT".into(),
                    interval: "1m".into(),
                    typed_params: Some(serde_json::json!({"SpreadMaker": {"qty": 1.0}})),
                    asset_class: Some("CryptoPerp".into()),
                }],
            })),
            // The REQ-7 settings payload — both the settings-dir arms, a value-carrying row and a
            // redacted one (the sentinel is the VALUE on the wire; redaction happened server-side).
            Response::SettingsShow(Box::new(crate::wire::WireSettingsShow {
                settings_dir: Some("/srv/vike-<unit>/settings".into()),
                rows: vec![
                    crate::wire::WireSettingsRow {
                        section: "config.toml".into(),
                        key: "config.tradehub_addr".into(),
                        value: "127.0.0.1:7879".into(),
                        origin: "config.toml".into(),
                        read_by: "tradehub".into(),
                    },
                    crate::wire::WireSettingsRow {
                        section: "config.toml".into(),
                        key: "config.bot_token".into(),
                        value: "<set>".into(),
                        origin: "env:ACME_TOKEN".into(),
                        read_by: "NO".into(),
                    },
                ],
            })),
            Response::SettingsShow(Box::new(crate::wire::WireSettingsShow {
                settings_dir: None,
                rows: Vec::new(),
            })),
            // The REQ-7 write reply — both arms are LIVE since v2: `false` = the node hot-applied
            // the key, `true` = restart-to-apply. The shape was pinned both ways from v1 so the
            // hot-reload could land without a schema change — and it did.
            Response::SettingsWritten { restart_required: true },
            Response::SettingsWritten { restart_required: false },
            // The ruling-16 report reply: the `LiveTearsheet` document as JSON TEXT, carried
            // verbatim (see the variant's own doc for why this wire is a string).
            Response::Tearsheet(r#"{"trades":3,"sharpe":1.25}"#.into()),
        ];
        for r in resps {
            let js = serde_json::to_string(&r).unwrap();
            let back: Response = serde_json::from_str(&js).unwrap();
            assert_eq!(r, back);
        }
    }

    /// The v4 rationale rides BESIDE the command on the wire (its own `reason` field next to `cmd`),
    /// never INSIDE the [`WireCommand`] — so the order-write vocabulary is byte-identical whether a
    /// rationale is given or not, and the reason can never be mistaken for an order field. Also pins
    /// that `Preview` stays a NEWTYPE (it is not audited, so it carries no rationale).
    #[test]
    fn the_reason_rides_beside_the_command_never_inside_it() {
        use crate::wire::WireCommand;

        let cmd = WireCommand::Cancel("c-1".into());
        let bare = serde_json::to_value(&cmd).unwrap();

        let with = serde_json::to_value(Request::Command {
            cmd: cmd.clone(),
            reason: Some("flat before the close".into()),
        })
        .unwrap();
        assert_eq!(with["Command"]["cmd"], bare, "the command itself is untouched by the reason");
        assert_eq!(with["Command"]["reason"], "flat before the close");

        let without = serde_json::to_value(Request::Command { cmd, reason: None }).unwrap();
        assert_eq!(without["Command"]["cmd"], bare, "…and identical with no reason given");
        assert!(without["Command"]["reason"].is_null(), "an absent rationale serializes as null");

        // Preview is deliberately NOT audited, so it stays a newtype (no `reason` to carry).
        let pv = serde_json::to_value(Request::Preview(WireCommand::Cancel("c-1".into()))).unwrap();
        assert_eq!(pv["Preview"], bare, "Preview is still a newtype over the command");
    }

    /// The REQ-2 advertisement round-trip: what [`datahub_feature`] renders,
    /// [`advertised_datahub`] reads back verbatim — the one pinned spelling for both sides.
    #[test]
    fn datahub_feature_round_trips_through_advertised_datahub() {
        let features =
            vec!["observe".to_string(), datahub_feature("127.0.0.1:7878"), "preview".to_string()];
        assert_eq!(advertised_datahub(&features), Some("127.0.0.1:7878".to_string()));
    }

    /// No `datahub=` entry — including a plain `"datahub"` without the `=` — is no advertisement,
    /// and an empty/whitespace value advertises nothing (absence is the answer, the same shape as
    /// an unset `config.datahub_addr`).
    #[test]
    fn a_missing_or_empty_datahub_entry_is_no_advertisement() {
        assert_eq!(advertised_datahub(&[]), None);
        assert_eq!(advertised_datahub(&["observe".to_string(), "preview".to_string()]), None);
        assert_eq!(advertised_datahub(&["datahub".to_string()]), None, "no `=` — not the entry");
        assert_eq!(advertised_datahub(&["datahub=".to_string()]), None, "empty value");
        assert_eq!(advertised_datahub(&["datahub=   ".to_string()]), None, "whitespace value");
    }

    /// The value-carrying entry can never satisfy a NAMED capability check: every existing check
    /// is whole-string equality per feature name, and `datahub=<addr>` equals none of them.
    #[test]
    fn the_datahub_entry_shadows_no_named_capability() {
        let entry = datahub_feature("127.0.0.1:7878");
        for named in [
            FEATURE_STRATEGY_VERBS,
            FEATURE_SETTINGS_SHOW,
            FEATURE_MOUNT_VERBS,
            FEATURE_STRATEGY_PARAMS,
            FEATURE_MOUNT_CLASS,
        ] {
            assert_ne!(entry, named);
        }
    }

    /// The 32-byte nonce is carried as a fixed `[u8; 32]` array; confirm the derive emits a 32-long
    /// JSON array (not a base64 string or a truncated form), so the wire shape is unambiguous.
    #[test]
    fn welcome_nonce_is_a_fixed_32_array() {
        let w = Response::Welcome { proto_version: 1, nonce: [9u8; 32], features: vec![] };
        let v: serde_json::Value = serde_json::to_value(&w).unwrap();
        let arr = v["Welcome"]["nonce"].as_array().expect("nonce is a JSON array");
        assert_eq!(arr.len(), 32);
    }

    /// A frame written with the re-exported codec reads back identically — proving the re-export is
    /// the SAME framing, and that a `Request` survives the length-prefixed round-trip.
    #[test]
    fn frame_round_trip_uses_the_reexported_codec() {
        let msg = Request::Hello { proto_version: NODE_PROTO_VERSION };
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &msg).unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        let back: Request = read_frame(&mut cursor).unwrap();
        assert_eq!(msg, back);
    }
}
