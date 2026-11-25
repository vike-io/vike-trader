//! The vike-tradehub **node** wire protocol: the request/response schema a thin client and the
//! headless live core ("tradehub") exchange, plus the authentication challenge that gates it.
//!
//! # Framing — ONE codec across both services
//!
//! The codec is [`vike_hub_protocol::frame`]'s (`read_frame`, `write_frame`, `MAX_FRAME_LEN`), named at that
//! path by every caller, never re-implemented and never re-exported here: both node services (datahub and this
//! one) speak `[u32 big-endian len][len bytes of body]` with the same pre-allocation OOM guard.
//!
//! # The body: JSON, or msgpack with field names (one switch per connection)
//!
//! A node of this build advertises [`FEATURE_WIRE_MSGPACK`]. A client that sees it writes every
//! session [`Request`] in msgpack with field names ([`node_codec`], `vike_hub_protocol::frame::BodyCodec`),
//! and the node answers each request, and a subscription's every frame, in the form the request came
//! in. Both ends READ every form, by the body's first bytes, so an old client against this node and
//! this client against an old node both stay on JSON, and no verb is converted one by one: the enums
//! here are serde types, and msgpack with field names carries them as JSON does (a field an older
//! reader does not know is skipped). The handshake's own frames stay JSON. JSON stays at the EDGES:
//! the CLI's `--json`, MCP's side of `vike-mcp`, the Telegram Bot API.
//!
//! # The connection handshake
//!
//! An unauthenticated peer must never reach [`Request::Command`]. The handshake is the ONE every hub
//! runs (`vike_hub_protocol::handshake`, decision 0288), in its own frames, under this node's
//! [`crate::auth::DOMAIN`]; each connection mints a fresh `nonce`, so a captured transcript can never
//! be replayed:
//!
//! 1. Client → `Hello { proto_version }`.
//! 2. Server → `Welcome { proto_version, features, nonce }` — `nonce` is 32 fresh random bytes for
//!    THIS connection.
//! 3. Client → `Auth { scope, mac }` where `mac == auth::sign(key_for(scope), &nonce, proto_version,
//!    scope)` (see [`crate::auth`]), or a device's `PersonAuth { key_id, scope, sig }` where the node
//!    advertises `person-auth`.
//! 4. Server: its own admission of the scope, then the mac against the key it holds for that `scope`
//!    (constant time), or the signature against the device's access row. Match → `AuthOk`; otherwise
//!    → `AuthDenied`. Only after `AuthOk` may the client [`Request::Subscribe`],
//!    [`Request::Snapshot`], or (with [`Scope::Write`]) [`Request::Command`].
//!
//! The [`Scope`] is the client's capability ceiling, and each scope signs under its OWN key
//! ([`vike_hub_protocol::auth::HubKeys`]): the scope is bound INTO the signed message, so an observe-only key
//! can never produce a valid `Control` mac.
//!
//! # Capabilities: an additive verb is a feature string, never a version bump
//!
//! [`NODE_PROTO_VERSION`] is folded into the signed auth MAC, so a bump breaks the handshake against
//! every running node. A new verb (or a field an old node would silently misread) is instead a
//! `FEATURE_*` string the node advertises in `Welcome.features`, and a client REFUSES CLIENT-SIDE,
//! sending nothing, against a node that does not list it
//! (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`).
//! Every check is whole-string equality. ⚠ Check the capability, never the emptiness of a field:
//! `#[serde(default)]` renders an old node's missing field and an honest empty one identically.
//!
//! # No I/O here
//!
//! This module opens no socket; the framing runs over whatever stream a caller hands it. Every
//! connection this crate makes goes through `crates/vike-tradehub-client/src/handshake.rs`'s
//! `hub_handshake`.

use serde::{Deserialize, Serialize};
// `Scope` (the capability ceiling a client authenticates under, bound INTO the signed auth message
// with its OWN key per scope) is the shared primitive's (`docs/decisions/0025-datahub-remote-posture.md`):
// the scope TAG is in the signed message, so two `Scope` enums would be two tag tables. It is named
// at `vike_hub_protocol::auth::Scope` everywhere, never re-exported through either client. Named here
// for the docs alone since the handshake frames left this enum (decision 0288).
#[cfg(doc)]
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::features::{FEATURE_WIRE_MSGPACK, value_after};
use vike_utility::errors::FailureOutcome;

use crate::wire::{CommandWire, CoreSnapshotWire, SettingsShowWire, StrategyStatusWire};

/// The hub protocol version a handshake's `Hello` declares and its `Welcome` echoes. It
/// is folded INTO the signed auth message ([`crate::auth::sign`]), so a version skew fails the
/// handshake rather than silently talking past each other — and every running node with it. Bump
/// only on a wire-SCHEMA change; an additive verb is a `FEATURE_*` string instead (module doc).
///
/// v2: added `CoreSnapshotWire::bars` (bounded bar tails for the observer chart).
/// v3: added the Preview request/response (server-authoritative dry-run).
/// v4: Command carries an optional operator/agent rationale, recorded in the audit trail.
/// v5: the phase-4 renames the owner ruled (P4.5c): the `Recon*` command words (`ConfirmReconcile`,
/// `CancelReconcileOrder`) and every renamed field key of the snapshot and the commands. An old node or client is refused at the handshake, naming both
/// numbers (`handshake.rs`'s `a_node_from_before_the_phase_four_renames_is_refused_at_the_handshake`).
pub const NODE_PROTO_VERSION: u32 = 5;

/// Gates the STRATEGY verbs: the [`Request::StrategyStatus`] read and the
/// `CommandWire::UpdateParams` write. The first verbs shipped this way rather than by a
/// [`NODE_PROTO_VERSION`] bump, which would have broken the handshake against every running node
/// (the version is in the MAC); an older server cannot decode them, so the client-side refusal
/// turns "undecodable request" into "this node predates strategy-verbs", with nothing on the wire.
pub const FEATURE_STRATEGY_VERBS: &str = "strategy-verbs";

/// Gates the SETTINGS read: [`Request::SettingsShow`] → [`Response::SettingsShow`].
pub const FEATURE_SETTINGS_SHOW: &str = "settings-show";

/// Gates the SETTINGS WRITE: `CommandWire::SetSetting`, answered by [`Response::SettingsWritten`].
/// Its own string, not [`FEATURE_SETTINGS_SHOW`]: a read-half node cannot decode `SetSetting`.
pub const FEATURE_SETTINGS_WRITE: &str = "settings-write";

/// The node's refusal of a `CommandWire::SetSetting` from a device whose role is not `admin`, as a
/// [`Response::Error`] (the owner's ruling of 2026-11-16, decision 0288's amendment): `control` trades
/// and changes no setting. Every surface that writes a setting over the wire shows it whole.
pub const SETTINGS_NEED_ADMIN: &str = "changing settings needs the admin role; ask the owner";

/// Gates the DIRECTORY read: [`Request::Directory`] → [`Response::Directory`].
pub const FEATURE_DIRECTORY: &str = "directory";

/// Gates the runtime MOUNT/UNMOUNT verbs: `CommandWire::MountStrategy` /
/// `CommandWire::UnmountStrategy`. Its own string: a node with only [`FEATURE_STRATEGY_VERBS`]
/// cannot decode the mount variants. [`crate::tradehub_control`]'s `required_feature` is the one
/// authority mapping commands to required capabilities.
pub const FEATURE_MOUNT_VERBS: &str = "strategy-mount-verbs";

/// Gates the STRUCTURED params read: [`crate::wire::MountViewWire`]s carrying the mount's
/// addressing key (`venue`/`symbol`/`interval`) and TYPED params, which a client patches and sends
/// back through the unchanged `CommandWire::UpdateParams` (still gated by
/// [`FEATURE_STRATEGY_VERBS`]).
///
/// ⚠ An older node ANSWERS [`Request::StrategyStatus`] fine, with the new fields defaulted empty —
/// indistinguishable from "this mount publishes no typed params". Check this capability, never the
/// emptiness of the field.
pub const FEATURE_STRATEGY_PARAMS: &str = "strategy-params";

/// Gates ADDRESSED params updates: the node honours `CommandWire::UpdateParams`'s `mount_id` and
/// its [`crate::wire::MountViewWire`]s carry the id (one string for both halves).
///
/// ⚠ A node without it DECODES an addressed update, drops the unknown key and retunes the FIRST
/// mount on the series, answering a normal Ack. So the client refuses an addressed update against
/// it (`required_feature`); an unaddressed update needs only [`FEATURE_STRATEGY_VERBS`].
pub const FEATURE_PARAMS_BY_MOUNT: &str = "strategy-params-mount";

/// Gates a FIELD: mount rows carry [`crate::wire::MountViewWire::asset_class`], what product each
/// mount trades (`docs/decisions/0061-an-instrument-names-its-kind.md`, Phase 5).
///
/// ⚠ A `None` means either "the node predates this string" or, from a node that has it, "the mount
/// is still TOML-backed, not migrated yet" (a row-backed mount always has a class). Only the
/// capability tells a client too new for its node from a deployment with work left. Nothing routes
/// on it: a client may DISPLAY it and may not decide with it (0061).
pub const FEATURE_MOUNT_CLASS: &str = "mount-class";

/// Gates a client-side DEADLINE, not a verb: the node writes a heartbeat `Response::Pong` whenever
/// nothing else was sent for [`crate::liveness::OBSERVE_HEARTBEAT`], and only then does
/// `TradehubCoreHandle::connect` arm [`crate::liveness::OBSERVE_READ_TIMEOUT`]. Deadlining a node
/// that does not heartbeat would turn a silent-death fix into a reconnect loop on an idle node. No
/// schema change: a client that ignores the string drops an unsolicited `Pong`, as it always did.
pub const FEATURE_OBSERVE_HEARTBEAT: &str = "observe-heartbeat";

/// Gates [`Request::SubscribeNewest`] and [`Request::FrameRead`]: this node holds a subscriber that asks for it to one
/// frame on the way, and hands it the NEWEST when the reader says it has read the last.
///
/// ⚠ **Dropping frames at the node's mailbox cannot keep a slow link current.** The writer's frames sit in every buffer
/// between it and the reader (two kernels, an ssh tunnel's channel window, more than a minute's worth on a slow link), the
/// reader takes them in order, and the mailbox only starts to drop once all of that is full. Measured: one `vike-mcp` through
/// an ssh tunnel on Windows read frames 1.5 s old, then 59.5 s old a minute later, against a flat 1.1 s on the box, and showed
/// an order ACCEPTED about 45 s after it was cancelled. Only the reader knows what it has read, so it says so.
pub const FEATURE_OBSERVE_NEWEST: &str = "observe-newest";

/// Gates [`Request::SubscribeWith`] and its `compressed` option: this node decodes the subscription and, where its
/// `config.observer_frame_compression` row is on, sends that subscriber each frame's body DEFLATED behind
/// [`crate::observe_stream::COMPRESSED_BODY_MARKER`]. A property of the BUILD: with the row off the node sends the plain
/// body, which the reader tells apart by its first byte (a JSON body never starts with the marker). A client that does
/// not see this string subscribes as it always did and is never sent a body it cannot read.
pub const FEATURE_OBSERVE_COMPRESSED: &str = "observe-compressed";

/// Gates [`ObserveSubscriptionWire::patches`] and [`Request::ObserveResync`]: after a whole snapshot, this node sends a
/// subscriber that asked only what CHANGED ([`Response::SnapshotPatch`], [`crate::observe_patch`]), numbered so a
/// reader that missed one asks for a whole snapshot again. A property of the BUILD. It changes how the state is sent,
/// never what the reader rebuilds: the same `CoreSnapshotWire`, value for value. A client that does not see it is sent
/// whole frames, byte for byte as before.
pub const FEATURE_OBSERVE_PATCHES: &str = "observe-patches";

/// Gates [`ObserveSubscriptionWire::series`] and [`Request::ObserveSeries`]: this node sends a subscriber that stated
/// the bar series it shows the bars of those alone (every other series still by its key, with no bar, so the reader
/// still knows what the node has), and the orders, positions, accounts and halts as ever. A property of the BUILD. A
/// client that states no set, or does not see the word, gets every series as before.
pub const FEATURE_OBSERVE_SERIES: &str = "observe-series";

/// The form a client writes its requests in to a node that advertised `features`: msgpack with field names where the
/// node says [`FEATURE_WIRE_MSGPACK`] (every hub's one word, a property of the node's BUILD), else JSON, as every
/// client before it did. Every reply is read in whatever form it comes.
pub fn node_codec(features: &[String]) -> vike_hub_protocol::frame::BodyCodec {
    use vike_hub_protocol::frame::BodyCodec;
    if features.iter().any(|f| f == FEATURE_WIRE_MSGPACK) {
        BodyCodec::Msgpack
    } else {
        BodyCodec::Json
    }
}

/// Declares that the node REFUSES a command whose
/// [`crate::wire::CommandWire::addressed_venue`] names no engine it runs, instead of falling
/// through to its PRIMARY engine.
///
/// ⚠ Gates no verb and no field: the venue field is as old as the protocol, and an older node
/// accepts it, Acks, and signs the order on engine 0, a different exchange. So the client rule is
/// inverted: against a node without this string a client sends only a venue it has positive
/// evidence the node runs (`CoreSnapshotWire::venues`) and refuses the rest locally
/// ([`crate::venue_routing::venue_routing_verdict`]).
pub const FEATURE_VENUE_ROUTING: &str = "venue-routing";

/// Declares that the node reads the `account` field on order-carrying commands.
///
/// # ⚠ No client may rely on this string — six released nodes advertise it falsely
///
/// Every `vike-tradehub` from `v0.1.27` through `v0.1.32` advertises it while discarding the
/// account before routing, and a string a shipped node advertises can never be withdrawn. So
/// [`crate::tradehub_control`]'s `required_feature` demands [`FEATURE_ACCOUNT_SCOPED_SUBMIT`] for a
/// labelled `Submit`; nodes keep advertising this one only for clients that already check it. The
/// rule it was written for still stands: a client that would SET the field must refuse locally
/// without the capability (`ControlRejected::UnsupportedByHub`), because an old node's
/// `#[serde(default)]` turns the absence into `account: None` and routes to the WRONG BOOK.
pub const FEATURE_ACCOUNT_ROUTING: &str = "account-routing";

/// Declares that the node ROUTES a `CommandWire::Submit` naming an account onto that account's
/// engine, and REFUSES one naming an account it runs no engine of, at its edge before the `Ack`
/// (`crates/vike-tradehub/src/server/control.rs`'s `lower_command`). A BUILD property, issued
/// under a string no released node ever advertised falsely (see [`FEATURE_ACCOUNT_ROUTING`]).
///
/// ⚠ A client that would SET the field on a `Submit` MUST refuse locally without this string, for
/// EVERY named account, `DEFAULT` included (an old node routes a `DEFAULT`-named submit by venue
/// alone, possibly onto a labelled book). A submit naming NO account serialises byte-identically
/// to the pre-field frame and flows to every node. One window survives at the node's edge: an
/// EMPTY engine roster (a core that has not published yet) Acks an unheld account and the core
/// refuses it out of band.
pub const FEATURE_ACCOUNT_SCOPED_SUBMIT: &str = "account-scoped-submit";

/// Declares that the node reads the `account` field on the RUNTIME MOUNT verb
/// (`CommandWire::MountStrategy`). A node advertising it always advertises [`FEATURE_MOUNT_VERBS`]
/// too, so `required_feature` answers with this one string.
///
/// ⚠ Its own string because the failure is silent: a mount-verbs node DECODES the frame, defaults
/// the absent field to `account: None`, and mounts on the venue's default account while the
/// operator reads a normal acknowledgement — and a misrouted mount is every order it will ever
/// place. A client that would SET the field MUST refuse locally without it; a mount naming no
/// account owes nothing.
pub const FEATURE_MOUNT_ACCOUNT: &str = "strategy-mount-account";

/// Declares that the node narrows an ACCOUNT-SCOPED RISK-REDUCING verb (`CommandWire::MassCancel`,
/// `CommandWire::Flatten`, `CommandWire::MarketExit` with `account: Some(..)`) to that account's
/// ONE engine, refusing before the Ack an account it does not hold (or one named with no venue).
///
/// ⚠ **`account: None` owes this string nothing**: a reducing verb naming a venue and no account owes
/// [`FEATURE_ONE_ACCOUNT_VENUE`] instead, and the unscoped panic button
/// (`MarketExit { venue: None, account: None }`) must reach every engine and can never be refused
/// by any gate in this family. An older node with only [`FEATURE_ACCOUNT_ROUTING`] DROPS the field and
/// fans the verb out, so `required_feature` demands THIS string for a labelled reduce.
pub const FEATURE_ACCOUNT_SCOPED_REDUCE: &str = "account-scoped-reduce";

/// Declares that the node's door refuses a venue-scoped command that names no account unless that venue
/// has exactly ONE account (the owner's ruling of 2026-11-07, decision 0254), so a client may send a
/// bare `close-all <venue>` as `account: None`.
///
/// ⚠ **A node without it fans a bare reducing verb (`MassCancel`, `Flatten`, `MarketExit` naming a venue
/// and no account) over EVERY account of the venue**, behind a normal Ack, which is what the ruling
/// forbids, so `required_feature` demands this string for one. The node-wide panic button names no venue
/// and owes nothing; an order that names no account owes nothing either, because the core refuses it on
/// a venue it runs two engines of.
pub const FEATURE_ONE_ACCOUNT_VENUE: &str = "one-account-venue";

/// Gates the TP/SL BRACKET verb, `CommandWire::Bracket` (a DECODE capability, like
/// [`FEATURE_MOUNT_VERBS`]).
///
/// ⚠ **A node advertising only this serves the DEFAULT account only** (its `OrderBracketWire` has no
/// `account` key) and refuses a bracket on a venue whose published accounts are anything but its one
/// default book. A bracket that names its account needs [`FEATURE_BRACKET_ACCOUNT`], never this one
/// widened; `crate::wire::OrderBracketWire` refuses unknown keys on decode, so an account-carrying
/// bracket sent to such a node gets an error rather than the default book.
pub const FEATURE_BRACKET: &str = "bracket";

/// Declares that the node reads the `account` field of `CommandWire::Bracket` (`OrderBracketWire::account`)
/// and routes the bracket onto that account's engine, judging it by THAT engine (a lane twin's `.P`
/// perp engine, a labelled account), refusing an account it does not run before the Ack. A node
/// advertising it always advertises [`FEATURE_BRACKET`] too.
///
/// ⚠ Its own string, the [`FEATURE_ACCOUNT_SCOPED_SUBMIT`] argument for a bracket: a node from before
/// the field REFUSES the frame (`deny_unknown_fields`), which is loud and safe, but a client must not
/// have to learn that from a decode error, and no node may be told "bracket" is enough to name a book.
/// A client that would SET the field MUST refuse locally without this string, for EVERY named
/// account, `DEFAULT` included. A bracket naming NO account serialises byte-identically to the
/// pre-field frame, owes this string nothing, and still reaches only the default book of a venue that
/// runs exactly one engine.
pub const FEATURE_BRACKET_ACCOUNT: &str = "bracket-account";

/// Declares that the node reads the group STEM of `CommandWire::Bracket` (`OrderBracketWire::order_client_id`,
/// decision 0270): it derives the three leg ids from it and refuses the bracket whole when one of them is in use,
/// so a bracket resent after an unclear reply is retry-safe. A FIELD capability with the `bracket-account`
/// argument: a node from before the field REFUSES the key (`deny_unknown_fields`), loud and safe, but a client must
/// not learn that from a decode error, so a client that would set the stem refuses locally without this string. A
/// node advertising it advertises [`FEATURE_BRACKET_ACCOUNT`] too, so the one word covers a bracket that names
/// both. A bracket naming no stem serialises byte-identically to the pre-field frame and owes nothing.
pub const FEATURE_BRACKET_STEM: &str = "bracket-stem";

/// Gates the operator's RECONCILE verb: `CommandWire::ReconcileConfirm` (a DECODE capability, like
/// [`FEATURE_BRACKET`]) and the held divergences the node publishes in
/// [`crate::wire::CoreSnapshotWire::reconcile`], which `trade reconcile ls` reads. One string for the pair,
/// because the id is only useful with both halves.
pub const FEATURE_RECONCILE_VERBS: &str = "recon-verbs";

/// Gates the operator's CANCEL of ONE held `UnknownOrder` row: `CommandWire::ReconcileCancel` (a DECODE
/// capability, like [`FEATURE_RECONCILE_VERBS`], whose string a node that serves this one also advertises).
/// A node that predates the string cannot decode the variant, so a client refuses locally rather than
/// send it (decision 0196, rule (b)).
pub const FEATURE_RECONCILE_CANCEL: &str = "recon-cancel";

/// Gates the operator's PER-VENUE RESUME: `CommandWire::ResumeVenue` (a DECODE capability, like
/// [`FEATURE_RECONCILE_VERBS`]): release the link dead-man's halt of ONE venue, which `SetTradingState`
/// cannot do (it also clears every other venue's halt and the drawdown latch). A node that predates
/// the string cannot decode the variant, so a client refuses locally rather than send it.
pub const FEATURE_VENUE_RESUME: &str = "venue-resume";

/// Gates the operator's ACCOUNT-scoped trading state: `CommandWire::SetAccountTradingState` (a DECODE
/// capability, like [`FEATURE_VENUE_RESUME`]): halt, reduce or resume ONE account's engine and no other,
/// which `SetTradingState` cannot do (it is node-wide and also loosens every engine a latch or an
/// operator halt holds). A node that predates the string cannot decode the variant, so a client refuses
/// locally rather than send it: ⚠ never the node-wide `SetTradingState` as a stand-in, which would act on
/// every account.
pub const FEATURE_ACCOUNT_TRADING_STATE: &str = "account-trading-state";

/// Gates the operator's READ of ONE account at its venue: [`Request::VenueRead`] → [`Response::VenueRead`]
/// (the account's balance with its allowances, or the venue's own list of its open orders). A DECODE
/// capability, like [`FEATURE_DIRECTORY`]: a node that predates the string cannot decode the request, so a
/// client refuses locally rather than send it.
///
/// ⚠ The string says the NODE serves the verb, nothing about what is behind it. The node asks the account's
/// open trade lease, and the gatewayhub answers `not offered by <venue>` for a bridge that has no account-wide
/// read of that kind (everything but Polymarket, Hyperliquid, Deribit, Bybit's balance and IG's open orders), or the
/// lease's own words when it predates the read: those
/// arrive as a [`Response::Error`], never as an absent capability.
pub const FEATURE_VENUE_READ: &str = "venue-read";

/// Gates the THIRD kind of that read, [`vike_model::VenueReadKind::Positions`] (the positions the venue
/// says the account holds), and the [`vike_model::VenueRead::Positions`] answer. A DECODE capability
/// on top of [`FEATURE_VENUE_READ`]: a node that has only `venue-read` cannot decode the kind's name, so
/// the client refuses positions locally against it and sends nothing; the two older kinds are unaffected.
/// A node that advertises this word advertises `venue-read` too.
pub const FEATURE_VENUE_READ_POSITIONS: &str = "venue-read-positions";

/// Says the node LISTS its armed core-held reduce-only stops in [`crate::wire::CoreSnapshotWire::core_held_stops`]
/// (decision 0199's amendment): a READING, not a verb, so nothing is refused locally for want of it. Its job
/// is to tell an empty list from a node that predates the field, for the readers that still read that list (MCP,
/// an older desktop); the CLI reads [`FEATURE_EMULATED_ORDERS`]'s list instead.
pub const FEATURE_CORE_HELD_STOPS: &str = "core-held-stops";

/// Says the node LISTS every order it EMULATES in [`crate::wire::CoreSnapshotWire::emulated_orders`] (decision 0275): the
/// trigger kinds it holds (a core-held stop among them) and the working kinds (`chase`, `iceberg`), one list. A
/// READING, not a verb, so nothing is refused locally for want of it; it tells an empty list from a node that
/// predates the field (`trade health` adds its `stops` row from the list alone; `trade order stops` says a node
/// without the word lists none either, never only "none armed"). [`FEATURE_CORE_HELD_STOPS`]'s list stays beside it
/// for the readers that know only that one.
pub const FEATURE_EMULATED_ORDERS: &str = "emulated-orders";

/// Says the node PUBLISHES its links to the other hubs ([`crate::wire::CoreSnapshotWire::trade_link_down`] and
/// [`crate::wire::CoreSnapshotWire::price_link_down`]): a READING, not a verb, so nothing is refused for want of it. Its
/// job is to let an empty list mean "every link is up" (`trade health`'s `PASS`), where a node that predates the
/// fields writes the same nothing.
pub const FEATURE_HUB_LINKS: &str = "hub-links";

/// Says the node CHANGES a resting order by cancel-then-new where its venue cannot amend that kind in place
/// (decision 0244): a `CommandWire::Modify` of a stop on a venue whose stop cannot move, or of a limit order
/// on a venue with no amend, is carried out as a new order placed under a new order client id that stays
/// linked to the first, instead of being refused. The command is the SAME frame, so nothing is refused
/// locally for want of the word; it lets a surface (the ladder's drag, `vike-cli`'s help) say what a modify
/// will do, and tell a node that predates it, which answers such a modify with an advisory and changes
/// nothing.
pub const FEATURE_AMEND_REPLACE: &str = "amend-replace";

/// Gates the operator's GIVE-BACK of an admitted symbol: `CommandWire::UnadmitSymbol` (a DECODE capability, like
/// [`FEATURE_ACCOUNT_TRADING_STATE`]): the account's engine takes an on-demand symbol out and its slot (one of
/// sixteen) is free for the next one, which the engine could not do before (decision 0205). A node that
/// predates the string cannot decode the variant, so a client refuses locally rather than send it.
pub const FEATURE_UNADMIT_SYMBOL: &str = "unadmit-symbol";

/// Gates the operator's UN-LATCH of ONE mount's budget: `CommandWire::UnlatchMount` (a DECODE capability, like
/// [`FEATURE_UNADMIT_SYMBOL`]): the mount's fire-once liquidate-only latch is cleared when it is flat and quiet
/// and its loss budget counts from where it stands, which nothing but a restart's re-judgement or an unmount and
/// a mount could do before (decision 0239). A node that predates the string cannot decode the variant, so a
/// client refuses locally rather than send it. The same string says the node WRITES `MountViewWire::latched`:
/// without it that flag reads `false` whatever the mount's state, so a client must not print "not latched".
pub const FEATURE_MOUNT_UNLATCH: &str = "mount-unlatch";

/// Gates the order's LIFETIME on a submit: `OrderRequestWire::time_in_force` (`day`, `ioc`, `fok`, `gtd`
/// with `OrderRequestWire::gtd_expiry`, `post-only`; the default `gtc` is the absent field). A FIELD
/// capability, not a verb: a node that predates the string decodes the frame, `#[serde(default)]` drops
/// the two keys, and it acts on a DIFFERENT order behind a normal Ack, a `post-only` order sent as a
/// plain GTC limit that takes liquidity. So a client refuses locally rather than send one
/// (`crate::tradehub_control`'s `required_feature`), and a submit that names no lifetime serialises
/// byte-identically to the frame from before and owes nothing.
pub const FEATURE_ORDER_TIME_IN_FORCE: &str = "order-tif";

/// Gates the order's DISPLAY quantity on a submit, `OrderRequestWire::display_quantity` (an iceberg's visible slice). A
/// FIELD capability like [`FEATURE_ORDER_TIME_IN_FORCE`]: a node that predates it drops the key and rests the WHOLE quantity in
/// view behind a normal Ack, so a client refuses locally rather than send one. A submit that names none is the frame
/// from before and owes nothing.
pub const FEATURE_ORDER_DISPLAY: &str = "order-display";

/// Gates the order's RELEASE on a submit, `OrderRequestWire::release` (hold the order until a moment, FIX
/// EffectiveTime). A FIELD capability like [`FEATURE_ORDER_TIME_IN_FORCE`]: a node that predates it drops the key and sends the
/// order AT ONCE behind a normal Ack, so a client refuses locally rather than send one. Named for the field and its
/// type (`release`, `vike_model::OrderRelease`), not for its one condition today: a condition added to the release later
/// takes a word of its own, and `crate::wire::OrderReleaseWire` refuses a key it does not know.
pub const FEATURE_ORDER_RELEASE: &str = "order-release";

/// Gates a modify's TRIGGER price, `CommandWire::Modify::new_trigger_price`. A FIELD capability like
/// [`FEATURE_ORDER_TIME_IN_FORCE`]: a node that predates it decodes the modify, drops the key and changes the rest of it, or
/// nothing, behind a normal Ack, so a client refuses locally rather than send one. A modify that names no trigger is
/// the frame from before and owes nothing.
pub const FEATURE_ORDER_MODIFY_TRIGGER: &str = "order-modify-trigger";

/// Gates a submit's TRAIL and LIMIT OFFSET, `OrderRequestWire::trail` / `OrderRequestWire::limit_offset` (the
/// `"trailing"` and `"trailing_limit"` kinds). A FIELD capability like [`FEATURE_ORDER_TIME_IN_FORCE`]: a node that predates
/// it drops both keys behind a decode that succeeds (it then refuses the kind word, `Unrecognized` there, but a
/// trail on another kind it would drop in silence), so a client refuses locally rather than send one. A submit that
/// names neither is the frame from before and owes nothing.
pub const FEATURE_ORDER_TRAILING: &str = "order-trailing";

/// Gates a submit's PEG, `OrderRequestWire::peg` (the `"pegged"` kind). A FIELD capability like
/// [`FEATURE_ORDER_TIME_IN_FORCE`]: a node that predates it drops the key behind a decode that succeeds (it then refuses the
/// kind word, `Unrecognized` there), so a client refuses locally rather than send one. A submit that names none is
/// the frame from before and owes nothing.
pub const FEATURE_ORDER_PEG: &str = "order-peg";

/// Gates the ORDER LIST verb, `CommandWire::OrderList` (`crate::wire::OrderListWire`: an OCO, OTO or OUO group or a
/// ladder, every leg on ONE account and one symbol, decision 0254). A DECODE capability like [`FEATURE_BRACKET`]: a
/// node that predates it cannot decode the variant, so a client refuses locally rather than send it. Its `account`
/// field ships WITH the word, so a list that names its account owes nothing more (`OrderListWire` refuses an unknown
/// key on decode, so a field added later takes a word of its own).
pub const FEATURE_ORDER_LIST: &str = "order-list";

/// Gates the COMBO verb, `CommandWire::Combo` (`crate::wire::OrderComboWire`: two legs or more traded as ONE order on ONE
/// account, decision 0254, priced by its net, by a limit per leg, or at market; wave 6 of the order kinds). A DECODE
/// capability like [`FEATURE_ORDER_LIST`]: a node that predates it cannot decode the variant, so a client refuses locally
/// rather than send it. Every field ships WITH the word (`OrderComboWire` refuses an unknown key on decode, so a field
/// added later takes a word of its own).
pub const FEATURE_COMBO: &str = "combo";

/// Gates the attributes wave's submit terms (decision 0267): `OrderRequestWire::attributes`,
/// `OrderRequestWire::trail_percent`, `OrderRequestWire::adjust` and a release's price VENUE (`OrderReleaseWire::venue`).
/// A FIELD capability like [`FEATURE_ORDER_TIME_IN_FORCE`]: a node that predates it drops the first three behind a decode that
/// succeeds and sends a plain limit, a plain stop or a trail of no distance (refused by its terms rule), so a client
/// refuses locally rather than send one. A submit that names none is the frame from before and owes nothing.
pub const FEATURE_ORDER_ATTRIBUTES: &str = "order-attributes";

/// The prefix of the ONE value-carrying `Welcome.features` entry: a daemon configured with
/// `config.datahub_advertise_addr` advertises `datahub=<addr>`, the CLIENT-side dial address of the
/// `vike-datahub` it fronts, so a client dials the data plane itself (advertisement, NEVER
/// proxying: history traffic does not flow through the process holding live orders). Safe beside
/// the named capabilities because every check is whole-string equality. Build it with
/// [`datahub_feature`], read it with [`advertised_datahub`]; the pair is the one spelling.
pub const FEATURE_DATAHUB_PREFIX: &str = "datahub=";

/// Gates the ACCOUNT-ADMIN verbs (`docs/decisions/0065-accounts-are-managed-and-the-barrier-is-
/// declared.md`): [`Request::Account`], answered by [`Response::AccountList`] /
/// [`Response::AccountWritten`].
///
/// ⚠ **The capability a node withholds CONDITIONALLY (with [`FEATURE_CREDENTIAL_ADMIN`], which rides it)**: advertised only when the daemon's own
/// three-valued declaration ARMED it, because otherwise the server holds no writer to answer with.
/// Seeing it is NOT authorization: every account verb also requires `Scope::Account`, bound inside the
/// signed preimage, which only a device whose role is `admin` on the access list reaches (decision 0288).
pub const FEATURE_ACCOUNT_VERBS: &str = "account-verbs";

/// Gates the credential RETIRE and PRESENT-NAMES verbs and the two data-provider key names:
/// [`crate::wire::AccountVerbWire::RetireCredential`], [`crate::wire::AccountVerbWire::HeldCredentials`]
/// (answered by [`Response::CredentialsHeld`]) and [`crate::wire::AccountVerbWire::SetCredential`] for
/// `vike_venues::credential_keys::DATA_PROVIDER_KEYS`
/// (`docs/decisions/0144-credential-writes-say-their-intent-and-a-delete-retires-the-row.md`).
///
/// ⚠ **Advertised exactly when [`FEATURE_ACCOUNT_VERBS`] is** (the same capability handle answers
/// both), and a client must see BOTH before it sends one of these: a node that predates this string
/// cannot decode the two new variants and refuses the provider names, and the refusal is made
/// CLIENT-SIDE so nothing goes on the wire after the handshake. Seeing it is no authorization:
/// every account verb also requires `Scope::Account`.
pub const FEATURE_CREDENTIAL_ADMIN: &str = "credential-admin";

/// The prefix of the [`Response::Error`] text a node answers EVERY session verb with while it is
/// still STARTING its engines (its door listens before the accounts are up): `starting: <where the
/// start has got to>`. A prefix on the existing reply and not a new variant, so a client that
/// predates it decodes the answer as an ordinary refusal instead of failing the frame. Build the
/// text with [`starting_error`], recognise it with [`is_starting_error`]; the pair is the one
/// spelling, so no reader greps a word of its own.
pub const STARTING_ERROR_PREFIX: &str = "starting: ";

/// The text for a node that is still starting; `progress` says where the start has got to (which accounts are up,
/// which are still starting): [`crate::refusals::TradehubRefusal::Starting`]'s, byte for byte what the
/// [`Response::Error`] always carried. A node of this build answers it as the refusal FRAME to a client that reads
/// one ([`Response::Refused`]).
pub fn starting_error(progress: &str) -> String {
    crate::refusals::TradehubRefusal::Starting { progress: progress.to_string() }.to_string()
}

/// Whether a [`Response::Error`] text is the answer of a node that is still starting (not a
/// refusal of the verb itself): the caller should try again shortly, never read it as a no.
pub fn is_starting_error(text: &str) -> bool {
    text.starts_with(STARTING_ERROR_PREFIX)
}

/// The [`Response::Error`] text a node's control door answers a command with when its core's ingest
/// lane is FULL (`crates/vike-tradehub/src/server/control.rs`'s `ControlAcceptError::Busy`): nothing was
/// executed, and the same command is taken shortly. The exact bytes every node has sent, and the text of
/// [`crate::refusals::TradehubRefusal::Busy`], the frame a node of this build answers a client that reads one.
pub const BUSY_ERROR: &str = "core busy, retry";

/// What a node's [`Response::Error`] text says the refused verb DID: [`FailureOutcome::NotNow`] for a
/// node still starting ([`is_starting_error`]) or a busy core ([`BUSY_ERROR`]), nothing executed and
/// asked again shortly; every other text is the node's final [`FailureOutcome::Refused`]. A refusal FRAME is
/// read by its id instead ([`crate::refusals::frame_outcome`]).
///
/// shortcut: decides on an older node's TEXT (one that predates `refusal_frame`, so it sends a starting or busy
/// answer as text; `tradehub_control::CommandRefusal::Text`); delete the text path once every node speaks
/// `refusal_frame`.
#[must_use]
pub fn refusal_outcome(text: &str) -> FailureOutcome {
    if is_starting_error(text) || text == BUSY_ERROR {
        FailureOutcome::NotNow
    } else {
        FailureOutcome::Refused
    }
}

/// Render the datahub advertisement entry, `datahub=<addr>`; the server (`served_features`)
/// formats through this. `addr` is verbatim, validated at the config layer.
pub fn datahub_feature(addr: &str) -> String {
    format!("{FEATURE_DATAHUB_PREFIX}{addr}")
}

/// Parse the datahub advertisement out of a `Welcome.features` list: the first entry with
/// [`FEATURE_DATAHUB_PREFIX`], value trimmed; `None` when absent or empty (absence is the answer).
/// A plain `"datahub"` entry without the `=` is NOT an advertisement.
pub fn advertised_datahub(features: &[String]) -> Option<String> {
    value_after(features, FEATURE_DATAHUB_PREFIX)
}

/// A client-to-server request on the node connection.
///
/// The post-auth SESSION's verbs. The pre-auth handshake is the ONE every hub runs, in its own frames
/// (`vike_hub_protocol::handshake::HandshakeFrame` and `HandshakeReply`, decision 0288): this enum carries
/// no handshake frame. A conforming server rejects every session verb until it issued `AuthOk`, and
/// [`Request::Command`] unless the authenticated scope is [`Scope::Write`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    /// Subscribe to live [`Topic`]s; the server streams [`Response::SnapshotFrame`]s thereafter.
    Subscribe {
        /// The topics to receive.
        topics: Vec<Topic>,
    },
    /// [`Self::Subscribe`] for a reader that says when it has read a frame ([`Self::FrameRead`]): the node sends the next
    /// frame only then, and the newest it has. Only to a node advertising [`FEATURE_OBSERVE_NEWEST`].
    SubscribeNewest {
        /// The topics to receive.
        topics: Vec<Topic>,
    },
    /// [`Self::Subscribe`] with the stream's OPTIONS ([`ObserveSubscriptionWire`]). Only to a node advertising
    /// [`FEATURE_OBSERVE_COMPRESSED`] (the first word that decodes it), and an option only where its own word is
    /// advertised.
    SubscribeWith(ObserveSubscriptionWire),
    /// The reader of a [`Self::SubscribeNewest`] (or of a [`Self::SubscribeWith`] that asked for the newest) has read a
    /// frame. Unanswered.
    FrameRead,
    /// The reader of a subscription that asked for patches cannot apply the next one (it missed a frame): the node's
    /// next frame is a whole snapshot. Unanswered.
    ObserveResync,
    /// The bar series the reader of a [`Self::SubscribeWith`] shows NOW (a chart opened or closed): the node sends the
    /// bars of those alone, and every other series by its key with no bar; `None` is every series again. Orders,
    /// positions, accounts and halts always flow. Only to a node advertising [`FEATURE_OBSERVE_SERIES`]. Unanswered.
    ObserveSeries {
        /// The series shown, `None` for all.
        series: Option<Vec<SeriesKeyWire>>,
    },
    /// Request one point-in-time [`CoreSnapshotWire`]. Answered by [`Response::SnapshotFrame`].
    Snapshot,
    /// Submit one order-scoped command ([`Scope::Write`] only). Answered by [`Response::Ack`] or
    /// [`Response::Error`].
    ///
    /// `reason` (v4) is an optional operator/agent RATIONALE riding BESIDE the command: the node
    /// records it in its audit trail and it never reaches `OrderRequest`, the fold, the journal or a
    /// venue. Remote free text, so the node SANITIZES it (`vike_tradehub::audit::sanitize_reason`);
    /// do not rely on the exact bytes appearing in the trail.
    Command {
        /// The order-scoped command to execute.
        command: CommandWire,
        /// Optional operator/agent rationale, recorded (sanitized) in the node's audit trail.
        reason: Option<String>,
    },
    /// DRY-RUN a command ([`Scope::Write`] only): the node runs only its edge validation and
    /// answers [`Response::Preview`]; nothing is lowered, handed to the core or placed.
    Preview(CommandWire),
    /// Liveness probe (post-auth). Answered by [`Response::Pong`].
    Ping,
    /// What is this node running? Either scope. Answered by [`Response::StrategyStatus`], or
    /// [`Response::Error`] on a node that publishes no identity block. Gated on
    /// [`FEATURE_STRATEGY_VERBS`].
    StrategyStatus,
    /// The node's effective settings rows, rendered (and redacted) by the node. Either scope.
    /// Answered by [`Response::SettingsShow`], or [`Response::Error`] on a node with no settings
    /// source or settings that no longer load. Gated on [`FEATURE_SETTINGS_SHOW`]; the write half
    /// is `CommandWire::SetSetting` ([`FEATURE_SETTINGS_WRITE`]).
    SettingsShow,
    /// The roster venues the node's settings database names and the active accounts it holds
    /// ([`crate::wire::AccountDirectoryWire`]). Either scope. Answered by [`Response::Directory`], or by
    /// [`Response::Error`] — never an empty directory — when the node has nothing to list the
    /// accounts from; no error text carries a filesystem path. Gated on [`FEATURE_DIRECTORY`].
    Directory,
    /// **An operator's READ of one account at its venue**: its balance (with the allowance each exchange
    /// contract holds, where the venue has one) or the venue's OWN list of the account's open orders. Either
    /// scope, like [`Request::Directory`]: it changes nothing at the venue, in the book or on disk, and an
    /// Observe peer is already shown the balance and the orders in every frame (the argument is
    /// `crates/vike-tradehub/src/server/venue_read.rs`'s module doc). Answered by [`Response::VenueRead`], or
    /// by [`Response::Error`] in the operator's words: the account is not running on this node, the venue's
    /// bridge offers no such read, the read was asked too soon after the last, or the venue refused. Gated on
    /// [`FEATURE_VENUE_READ`].
    ///
    /// The read rides the account's open trade lease to the gatewayhub, which asks the venue with the
    /// account's own credentials; it is NOT a reconcile pass (it folds nothing, so `flags.poly_reconcile`
    /// is not needed) and the answer is kept by nothing.
    VenueRead {
        /// The venue's roster id (`polymarket`).
        venue: String,
        /// Which account of the venue, spelled as the wire spells every account: `DEFAULT` for the unlabelled
        /// one, else its label. Always named, so a venue with two accounts is never read at the wrong one.
        account: String,
        /// Which read.
        kind: vike_model::VenueReadKind,
    },
    /// **ACCOUNT ADMINISTRATION** (`Scope::Account` ONLY, `docs/decisions/0065`): the settings
    /// database's `account` table, plus the one verb on this wire that carries a credential VALUE.
    /// Answered by [`Response::AccountList`], [`Response::AccountWritten`] or [`Response::Error`].
    /// Gated on [`FEATURE_ACCOUNT_VERBS`].
    ///
    /// ⚠ A `Request` variant, not a `CommandWire` payload: `CommandWire` mirrors the CORE-facing
    /// verbs and DERIVES `Debug`, and a value-carrying variant may not ride it;
    /// [`crate::wire::AccountRequestWire`] carries its own redacting impl.
    Account(Box<crate::wire::AccountRequestWire>),
}

/// A server-to-client response on the node connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    /// A session verb this connection's scope does not reach (a `Command` on an observe connection). The
    /// handshake's own refusal is `vike_hub_protocol::handshake::HandshakeReply::AuthDenied`, the same bytes.
    /// The reason never leaks key material.
    AuthDenied {
        /// A non-sensitive reason string.
        reason: String,
    },
    /// The answer to [`Request::Snapshot`] and the push shape for a subscription. BOXED for
    /// `clippy::large_enum_variant`; serde treats `Box<T>` transparently.
    SnapshotFrame(Box<CoreSnapshotWire>),
    /// The push shape for a subscription that asked for patches ([`ObserveSubscriptionWire::patches`]): a whole
    /// snapshot first, then what changed. BOXED like [`Self::SnapshotFrame`].
    SnapshotPatch(Box<SnapshotPatchWire>),
    /// Reply to a [`Request::Command`]: the order-client-id the command minted or echoed, and the
    /// node's NOTES on an order it sent (a sentence each, in plain words: today the one that the
    /// order was placed into a closed market).
    Ack {
        /// The (minted or echoed) order-client-id the command produced.
        order_client_id: String,
        /// What the node says about the command it ACCEPTED: never a refusal (the command was sent), only a
        /// thing the operator should know (an order placed into a closed market, a flatten whose engines
        /// show nothing to close). Empty for every other command and for one with nothing to say.
        ///
        /// ⚠ **Additive, no feature word**: an empty list writes no key, so every frame without a note is
        /// byte-identical to the one before the field; a client from before it ignores the key it does not
        /// know (this enum denies no unknown field), and a node from before it never writes one, which
        /// reads back as empty. A client prints what arrives and asks nothing of the node first.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notes: Vec<String>,
        /// The KIND word of an order the node EMULATES because its venue does not hold it (decision 0275:
        /// `chase` for a pegged order, `iceberg` for a visible size), beside the one `notes` sentence that says so.
        /// `None` for every other command. Additive, no feature word, as `notes`: absent writes no key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        emulated: Option<String>,
    },
    /// Reply to [`Request::Preview`]: whether the command WOULD pass the node's edge gate (nothing
    /// was executed either way).
    Preview {
        /// Whether the command would be accepted by the node's edge gate.
        accepted: bool,
        /// The refusal reason when `accepted` is `false`; `None` when accepted. Always the BARE text.
        reason: Option<String>,
        /// WHICH refusal `reason` is: the id of the node's refusal, shown with it once (the `CarriedRefusal`
        /// [`crate::tradehub_control::preview_command`] returns), never decided on. A READING, no feature word, as
        /// [`crate::wire::OrderViewWire::refusal_id`]: absent = no id, or a node that predates the field, and an
        /// older client ignores the key (`proto_tests.rs`'s `a_preview_refusal_id_is_additive_both_ways`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refusal_id: Option<String>,
        /// What the node says about an order it would accept, a sentence each: today, the quantity the order
        /// will go out with when the venue's step moves it. Additive, no feature word, as [`Self::Ack`]'s `notes`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notes: Vec<String>,
    },
    /// A generic server-side failure (bad request phase, malformed command, …), stringified.
    Error(String),
    /// Reply to [`Request::Ping`]; also the observe stream's idle heartbeat.
    Pong,
    /// Reply to [`Request::StrategyStatus`]: identity, effective params, one
    /// [`crate::wire::MountViewWire`] per mounted strategy. BOXED like [`Response::SnapshotFrame`].
    StrategyStatus(Box<StrategyStatusWire>),
    /// Reply to [`Request::SettingsShow`]: every cell rendered (and redacted) by the node. BOXED.
    SettingsShow(Box<SettingsShowWire>),
    /// Reply to [`Request::Directory`].
    Directory(Box<crate::wire::AccountDirectoryWire>),
    /// Reply to an ACCEPTED `CommandWire::SetSetting` — not an [`Response::Ack`]: nothing entered
    /// the core, and what the caller needs is whether the value is LIVE. `restart_required: false`
    /// means the node HOT-APPLIED it (its own per-key classification; policy keys never are);
    /// `true` means the row committed and applies at the next restart. A refusal is the ordinary
    /// [`Response::Error`] and leaves the database byte-identical (no confirm to refuse on:
    /// `docs/decisions/0086` point 7).
    SettingsWritten {
        /// `true` ⇒ the row committed but the running node still trades on its boot-time value for
        /// this key — it applies at the next restart. `false` ⇒ the node applied it live.
        restart_required: bool,
    },
    /// Reply to an [`Request::Account`] whose verb was [`crate::wire::AccountVerbWire::List`]: the
    /// node's `account` rows and the credential key NAMES each owns. **Never a value.**
    AccountList(Box<crate::wire::AccountListWire>),
    /// Reply to an ACCEPTED account WRITE (not an [`Response::Ack`], for the
    /// [`Response::SettingsWritten`] reason). A refusal is the ordinary [`Response::Error`], naming
    /// key NAMES and never a value.
    AccountWritten(Box<crate::wire::AccountWriteWire>),
    /// Reply to an [`Request::Account`] whose verb was [`crate::wire::AccountVerbWire::HeldCredentials`]:
    /// the asked names the node's store holds LIVE. **Names only, never a value.**
    CredentialsHeld(Box<crate::wire::CredentialsHeldWire>),
    /// Reply to [`Request::VenueRead`]: what the venue said about the account. BOXED like the other payloads.
    /// A refusal (not running here, not offered by the venue's bridge, asked too soon, the venue's own) is the
    /// ordinary [`Response::Error`].
    VenueRead(Box<vike_model::VenueRead>),
    /// A refusal the node DECLARED with an id ([`crate::refusals`]): the refusal FRAME
    /// (`vike_hub_protocol::refusal::HubRefusal`), only to a client that said it reads it (its
    /// `vike_hub_protocol::handshake::HandshakeFrame::Reads`, sent to a node advertising
    /// `vike_hub_protocol::features::FEATURE_REFUSAL_FRAME`); any other client is sent its text as the
    /// [`Response::Error`] it always read ([`Response::older_form`]).
    Refused(vike_hub_protocol::refusal::HubRefusal),
}

impl Response {
    /// The reply a client sees in this one's place when it did not say it reads the refusal frame
    /// (`reads_refusal_frame`: what its connection said, `vike_hub_protocol::handshake::reads_refusal_frame`): a
    /// refusal FRAME becomes the [`Response::Error`] of the same text. `None`: send this reply as it is. The node's
    /// session replies and its starting door ask it, so an older client never meets a variant it cannot decode.
    #[must_use]
    pub fn older_form(&self, reads_refusal_frame: bool) -> Option<Self> {
        match self {
            Response::Refused(refusal) if !reads_refusal_frame => {
                Some(Response::Error(refusal.text.clone()))
            }
            _ => None,
        }
    }
}

/// What a [`Request::SubscribeWith`] asks of the observe stream beside its topics. Each option is one this client can
/// READ; what the node sends is its own row's answer (a node whose compression row is off sends plain bodies).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObserveSubscriptionWire {
    /// The topics to receive.
    pub topics: Vec<Topic>,
    /// The reader says what it has read ([`Request::FrameRead`]) and is sent the newest frame next, as a
    /// [`Request::SubscribeNewest`] is.
    #[serde(default)]
    pub newest: bool,
    /// The reader inflates a compressed frame body ([`crate::observe_stream::decode_response`]).
    #[serde(default)]
    pub compressed: bool,
    /// The reader rebuilds the snapshot from [`Response::SnapshotPatch`] frames ([`crate::observe_stream::ObserveReceiver`]):
    /// only to a node advertising [`FEATURE_OBSERVE_PATCHES`].
    #[serde(default)]
    pub patches: bool,
    /// The bar series the reader SHOWS ([`Request::ObserveSeries`] changes it later); only to a node advertising
    /// [`FEATURE_OBSERVE_SERIES`]. `None` is every series, as a subscriber that says nothing gets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<Vec<SeriesKeyWire>>,
}

/// One bar series by its key: the `(venue, symbol, interval)` of a `crate::wire::BarSeriesWire`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SeriesKeyWire {
    pub venue: String,
    pub symbol: String,
    pub interval: String,
}

/// One frame of a subscription that asked for patches ([`ObserveSubscriptionWire::patches`]): the WHOLE snapshot
/// (`base` absent) or what changed since the frame numbered `base` ([`crate::observe_patch`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotPatchWire {
    /// This frame's number on its subscription, from 1, one more with each frame: a reader that holds another number
    /// than `base` missed a frame, and asks for a whole one again ([`Request::ObserveResync`]).
    pub frame: u64,
    /// The frame `patch` applies to; absent, `patch` is the whole [`CoreSnapshotWire`]'s tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<u64>,
    /// The snapshot's tree, or the patch of it ([`crate::observe_patch`]). Held as a `serde_json::Value` (the diff's
    /// one shape), it crosses in the connection's form: as msgpack on a msgpack connection, the same tree.
    pub patch: serde_json::Value,
}

/// A live-stream topic a client may [`Request::Subscribe`] to — WHICH blocks of the coalesced
/// [`CoreSnapshotWire`] a subscriber wants pushed.
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
