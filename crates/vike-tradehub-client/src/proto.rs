//! The vike-tradehub **node** wire protocol: the request/response schema a thin client and the
//! headless live core ("tradehub") exchange, plus the authentication challenge that gates it.
//!
//! # Framing — ONE codec across both services
//!
//! [`read_frame`] / [`write_frame`] / [`MAX_FRAME_LEN`] are re-exported from
//! [`vike_node_proto::frame`], never re-implemented: both node services (datahub and this one)
//! speak `[u32 big-endian len][len bytes of UTF-8 JSON]` with the same pre-allocation OOM guard.
//!
//! # The connection handshake
//!
//! An unauthenticated peer must never reach [`Request::Command`]. Each connection mints a fresh
//! `nonce`, so a captured transcript can never be replayed:
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
//! The [`Scope`] is the client's capability ceiling, and each scope signs under its OWN key
//! ([`crate::auth::NodeKeys`]): the scope is bound INTO the signed message, so an observe-only key
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
//! `node_handshake`.

// ONE framing across both services. `read_frame_raw` decodes nothing (a server keeps a connection
// alive across an undecodable body); `read_frame_raw_capped` takes a CALLER-CHOSEN ceiling, so the
// node reads PRE-AUTH frames under a far smaller cap than `MAX_FRAME_LEN`.
pub use vike_node_proto::frame::{
    MAX_FRAME_LEN, read_frame, read_frame_raw, read_frame_raw_capped, write_frame,
};

use serde::{Deserialize, Serialize};

use crate::wire::{WireCommand, WireSettingsShow, WireSnapshot, WireStrategyStatus};

/// The node protocol version a [`Request::Hello`] declares and a [`Response::Welcome`] echoes. It
/// is folded INTO the signed auth message ([`crate::auth::sign`]), so a version skew fails the
/// handshake rather than silently talking past each other — and every running node with it. Bump
/// only on a wire-SCHEMA change; an additive verb is a `FEATURE_*` string instead (module doc).
///
/// v2: added `WireSnapshot::bars` (bounded bar tails for the observer chart).
/// v3: added the Preview request/response (server-authoritative dry-run).
/// v4: Command carries an optional operator/agent rationale, recorded in the audit trail.
pub const NODE_PROTO_VERSION: u32 = 4;

/// Gates the STRATEGY verbs: the [`Request::StrategyStatus`] read and the
/// `WireCommand::UpdateParams` write. The first verbs shipped this way rather than by a
/// [`NODE_PROTO_VERSION`] bump, which would have broken the handshake against every running node
/// (the version is in the MAC); an older server cannot decode them, so the client-side refusal
/// turns "undecodable request" into "this node predates strategy-verbs", with nothing on the wire.
pub const FEATURE_STRATEGY_VERBS: &str = "strategy-verbs";

/// Gates the SETTINGS read: [`Request::SettingsShow`] → [`Response::SettingsShow`].
pub const FEATURE_SETTINGS_SHOW: &str = "settings-show";

/// Gates the SETTINGS WRITE: `WireCommand::SetSetting`, answered by [`Response::SettingsWritten`].
/// Its own string, not [`FEATURE_SETTINGS_SHOW`]: a read-half node cannot decode `SetSetting`.
pub const FEATURE_SETTINGS_WRITE: &str = "settings-write";

/// Gates the DIRECTORY read: [`Request::Directory`] → [`Response::Directory`].
pub const FEATURE_DIRECTORY: &str = "directory";

/// Gates the runtime MOUNT/UNMOUNT verbs: `WireCommand::MountStrategy` /
/// `WireCommand::UnmountStrategy`. Its own string: a node with only [`FEATURE_STRATEGY_VERBS`]
/// cannot decode the mount variants. [`crate::remote_control`]'s `required_feature` is the one
/// authority mapping commands to required capabilities.
pub const FEATURE_MOUNT_VERBS: &str = "strategy-mount-verbs";

/// Gates the STRUCTURED params read: [`crate::wire::WireMountRow`]s carrying the mount's
/// addressing key (`venue`/`symbol`/`interval`) and TYPED params, which a client patches and sends
/// back through the unchanged `WireCommand::UpdateParams` (still gated by
/// [`FEATURE_STRATEGY_VERBS`]).
///
/// ⚠ An older node ANSWERS [`Request::StrategyStatus`] fine, with the new fields defaulted empty —
/// indistinguishable from "this mount publishes no typed params". Check this capability, never the
/// emptiness of the field.
pub const FEATURE_STRATEGY_PARAMS: &str = "strategy-params";

/// Gates ADDRESSED params updates: the node honours `WireCommand::UpdateParams`'s `mount_id` and
/// its [`crate::wire::WireMountRow`]s carry the id (one string for both halves).
///
/// ⚠ A node without it DECODES an addressed update, drops the unknown key and retunes the FIRST
/// mount on the series, answering a normal Ack. So the client refuses an addressed update against
/// it (`required_feature`); an unaddressed update needs only [`FEATURE_STRATEGY_VERBS`].
pub const FEATURE_PARAMS_BY_MOUNT: &str = "strategy-params-mount";

/// Gates a FIELD: mount rows carry [`crate::wire::WireMountRow::asset_class`], what product each
/// mount trades (`docs/decisions/0061-an-instrument-names-its-kind.md`, Phase 5).
///
/// ⚠ A `None` means either "the node predates this string" or, from a node that has it, "the mount
/// is still TOML-backed, not migrated yet" (a row-backed mount always has a class). Only the
/// capability tells a client too new for its node from a deployment with work left. Nothing routes
/// on it: a client may DISPLAY it and may not decide with it (0061).
pub const FEATURE_MOUNT_CLASS: &str = "mount-class";

/// Gates a client-side DEADLINE, not a verb: the node writes a heartbeat `Response::Pong` whenever
/// nothing else was sent for [`crate::liveness::OBSERVE_HEARTBEAT`], and only then does
/// `RemoteCoreHandle::connect` arm [`crate::liveness::OBSERVE_READ_TIMEOUT`]. Deadlining a node
/// that does not heartbeat would turn a silent-death fix into a reconnect loop on an idle node. No
/// schema change: a client that ignores the string drops an unsolicited `Pong`, as it always did.
pub const FEATURE_OBSERVE_HEARTBEAT: &str = "observe-heartbeat";

/// Gates the LIVE-JOURNAL REPORT verb: [`Request::Tearsheet`] → [`Response::Tearsheet`]. The arm
/// and this string ship TOGETHER (`crates/vike-tradehub/src/server/tearsheet.rs`'s
/// `tearsheet_reply`; the node advertises it unconditionally), so the check refuses only a daemon
/// that PREDATES the arm.
///
/// ⚠ A node that advertises this can still answer [`Response::Error`] (it cannot resolve its own
/// journal directory): one is a claim about the BINARY, the other about how that process started.
pub const FEATURE_TEARSHEET: &str = "tearsheet";

/// Declares that the node REFUSES a command whose
/// [`crate::wire::WireCommand::addressed_venue`] names no engine it runs, instead of falling
/// through to its PRIMARY engine.
///
/// ⚠ Gates no verb and no field: the venue field is as old as the protocol, and an older node
/// accepts it, Acks, and signs the order on engine 0, a different exchange. So the client rule is
/// inverted: against a node without this string a client sends only a venue it has positive
/// evidence the node runs (`WireSnapshot::venues`) and refuses the rest locally
/// (`vike_app_core::backend::tradehub_control::venue_routing_verdict`).
pub const FEATURE_VENUE_ROUTING: &str = "venue-routing";

/// Declares that the node reads the `account` field on order-carrying commands.
///
/// # ⚠ No client may rely on this string — six released nodes advertise it falsely
///
/// Every `vike-tradehub` from `v0.1.27` through `v0.1.32` advertises it while discarding the
/// account before routing, and a string a shipped node advertises can never be withdrawn. So
/// [`crate::remote_control`]'s `required_feature` demands [`FEATURE_ACCOUNT_SCOPED_SUBMIT`] for a
/// labelled `Submit`; nodes keep advertising this one only for clients that already check it. The
/// rule it was written for still stands: a client that would SET the field must refuse locally
/// without the capability (`ControlRejected::UnsupportedByNode`), because an old node's
/// `#[serde(default)]` turns the absence into `account: None` and routes to the WRONG BOOK.
pub const FEATURE_ACCOUNT_ROUTING: &str = "account-routing";

/// Declares that the node ROUTES a `WireCommand::Submit` naming an account onto that account's
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
/// (`WireCommand::MountStrategy`). A node advertising it always advertises [`FEATURE_MOUNT_VERBS`]
/// too, so `required_feature` answers with this one string.
///
/// ⚠ Its own string because the failure is silent: a mount-verbs node DECODES the frame, defaults
/// the absent field to `account: None`, and mounts on the venue's default account while the
/// operator reads a normal acknowledgement — and a misrouted mount is every order it will ever
/// place. A client that would SET the field MUST refuse locally without it; a mount naming no
/// account owes nothing.
pub const FEATURE_MOUNT_ACCOUNT: &str = "strategy-mount-account";

/// Declares that the node narrows an ACCOUNT-SCOPED RISK-REDUCING verb (`WireCommand::MassCancel`,
/// `WireCommand::Flatten`, `WireCommand::MarketExit` with `account: Some(..)`) to that account's
/// ONE engine, refusing before the Ack an account it does not hold (or one named with no venue).
///
/// ⚠ **`account: None` owes this string nothing and is unchanged by it**: an unscoped reducing
/// verb fans out to every account of the venue deliberately, and the unscoped panic button
/// (`MarketExit { venue: None, account: None }`) must reach every engine and can never be refused
/// by any gate in this family. Naming an account is an opt-in narrowing, never a precondition for a
/// way out. An older node with only [`FEATURE_ACCOUNT_ROUTING`] DROPS the field and fans the verb
/// out, so `required_feature` demands THIS string for a labelled reduce.
pub const FEATURE_ACCOUNT_SCOPED_REDUCE: &str = "account-scoped-reduce";

/// Gates the TP/SL BRACKET verb, `WireCommand::Bracket` (a DECODE capability, like
/// [`FEATURE_MOUNT_VERBS`]).
///
/// ⚠ **A node advertising this serves the DEFAULT account only** (`vike_model::BracketSpec` names
/// no account) and refuses a bracket on a venue whose published accounts are anything but its one
/// default book. A bracket that names its account needs a NEW word, never this one widened;
/// `crate::wire::WireBracketSpec` refuses unknown keys on decode, so an account-carrying bracket
/// gets an error rather than the default book.
pub const FEATURE_BRACKET: &str = "bracket";

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
/// ⚠ **The ONLY capability a node withholds CONDITIONALLY**: advertised only when the daemon's own
/// three-valued declaration ARMED it, because otherwise the server holds no writer to answer with.
/// Seeing it is NOT authorization: every account verb also requires `Scope::Account`, a THIRD key
/// ([`crate::auth::ADMIN_KEY_ENV`]) bound inside the signed preimage.
pub const FEATURE_ACCOUNT_VERBS: &str = "account-verbs";

/// Render the datahub advertisement entry, `datahub=<addr>`; the server (`served_features`)
/// formats through this. `addr` is verbatim, validated at the config layer.
pub fn datahub_feature(addr: &str) -> String {
    format!("{FEATURE_DATAHUB_PREFIX}{addr}")
}

/// Parse the datahub advertisement out of a `Welcome.features` list: the first entry with
/// [`FEATURE_DATAHUB_PREFIX`], value trimmed; `None` when absent or empty (absence is the answer).
/// A plain `"datahub"` entry without the `=` is NOT an advertisement.
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
/// Two phases: the pre-auth handshake ([`Request::Hello`], [`Request::Auth`]) and the post-auth
/// session. A conforming server rejects every session verb until it issued [`Response::AuthOk`],
/// and [`Request::Command`] unless the authenticated scope is [`Scope::Write`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    /// Open the handshake, declaring the client's [`NODE_PROTO_VERSION`]. Answered by
    /// [`Response::Welcome`] (which carries the connection nonce), or [`Response::Error`].
    Hello {
        /// The client's node protocol version.
        proto_version: u32,
    },
    /// Answer the nonce challenge with the [`crate::auth::sign`] tag. Answered by
    /// [`Response::AuthOk`] or [`Response::AuthDenied`].
    Auth {
        /// The capability ceiling this client is authenticating under.
        scope: Scope,
        /// The HMAC-SHA256 tag (raw bytes; a JSON array of `u8`).
        mac: Vec<u8>,
    },
    /// Subscribe to live [`Topic`]s; the server streams [`Response::SnapshotFrame`]s thereafter.
    Subscribe {
        /// The topics to receive.
        topics: Vec<Topic>,
    },
    /// Request one point-in-time [`WireSnapshot`]. Answered by [`Response::SnapshotFrame`].
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
        cmd: WireCommand,
        /// Optional operator/agent rationale, recorded (sanitized) in the node's audit trail.
        reason: Option<String>,
    },
    /// DRY-RUN a command ([`Scope::Write`] only): the node runs only its edge validation and
    /// answers [`Response::Preview`]; nothing is lowered, handed to the core or placed.
    Preview(WireCommand),
    /// Liveness probe (post-auth). Answered by [`Response::Pong`].
    Ping,
    /// What is this node running? Either scope. Answered by [`Response::StrategyStatus`], or
    /// [`Response::Error`] on a node that publishes no identity block. Gated on
    /// [`FEATURE_STRATEGY_VERBS`].
    StrategyStatus,
    /// The node's effective settings rows, rendered (and redacted) by the node. Either scope.
    /// Answered by [`Response::SettingsShow`], or [`Response::Error`] on a node with no settings
    /// source or settings that no longer load. Gated on [`FEATURE_SETTINGS_SHOW`]; the write half
    /// is `WireCommand::SetSetting` ([`FEATURE_SETTINGS_WRITE`]).
    SettingsShow,
    /// The roster venues the node's settings database names and the active accounts it holds
    /// ([`crate::wire::WireDirectory`]). Either scope. Answered by [`Response::Directory`], or by
    /// [`Response::Error`] — never an empty directory — when the node has nothing to list the
    /// accounts from; no error text carries a filesystem path. Gated on [`FEATURE_DIRECTORY`].
    Directory,
    /// Render a tearsheet from the journal the node is writing. Either scope. Answered by
    /// [`Response::Tearsheet`] or [`Response::Error`]. The JOURNAL never crosses the wire, the
    /// ANSWER does (compute-to-data). Gated on [`FEATURE_TEARSHEET`].
    Tearsheet {
        /// The account's starting capital (the equity curve's base). `None` ⇒ the renderer's own
        /// default (`vike_report::tearsheet_cli`'s `--seed`).
        seed: Option<f64>,
        /// The Sharpe/Sortino/Calmar/CAGR annualization factor. `None` ⇒ the renderer's default.
        periods_per_year: Option<f64>,
    },
    /// **ACCOUNT ADMINISTRATION** (`Scope::Account` ONLY, `docs/decisions/0065`): the settings
    /// database's `account` table, plus the one verb on this wire that carries a credential VALUE.
    /// Answered by [`Response::AccountList`], [`Response::AccountWritten`] or [`Response::Error`].
    /// Gated on [`FEATURE_ACCOUNT_VERBS`].
    ///
    /// ⚠ A `Request` variant, not a `WireCommand` payload: `WireCommand` mirrors the CORE-facing
    /// verbs and DERIVES `Debug`, and a value-carrying variant may not ride it;
    /// [`crate::wire::AccountRequest`] carries its own redacting impl.
    Account(Box<crate::wire::AccountRequest>),
}

/// A server-to-client response on the node connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    /// Reply to [`Request::Hello`]: the server's version, the connection `nonce` to sign, and its
    /// advertised `features` (the `FEATURE_*` capability strings).
    Welcome {
        /// The server's node protocol version.
        proto_version: u32,
        /// 32 fresh random bytes for THIS connection (a fixed-length JSON array of 32 `u8`s).
        nonce: [u8; 32],
        /// Free-form advertised capability strings (forward compatibility).
        features: Vec<String>,
    },
    /// The handshake succeeded; the session is open at the granted `scope`.
    AuthOk {
        /// The scope actually granted (echoes the client's requested scope on success).
        scope: Scope,
    },
    /// Reply to an INVALID [`Request::Auth`]; the reason never leaks key material.
    AuthDenied {
        /// A non-sensitive reason string.
        reason: String,
    },
    /// The answer to [`Request::Snapshot`] and the push shape for a subscription. BOXED for
    /// `clippy::large_enum_variant`; serde treats `Box<T>` transparently.
    SnapshotFrame(Box<WireSnapshot>),
    /// Reply to a [`Request::Command`]: the client-order-id the command minted or echoed.
    Ack {
        /// The (minted or echoed) client-order-id the command produced.
        coid: String,
    },
    /// Reply to [`Request::Preview`]: whether the command WOULD pass the node's edge gate (nothing
    /// was executed either way).
    Preview {
        /// Whether the command would be accepted by the node's edge gate.
        accepted: bool,
        /// The refusal reason when `accepted` is `false`; `None` when accepted.
        reason: Option<String>,
    },
    /// A generic server-side failure (bad request phase, malformed command, …), stringified.
    Error(String),
    /// Reply to [`Request::Ping`]; also the observe stream's idle heartbeat.
    Pong,
    /// Reply to [`Request::StrategyStatus`]: identity, effective params, one
    /// [`crate::wire::WireMountRow`] per mounted strategy. BOXED like [`Response::SnapshotFrame`].
    StrategyStatus(Box<WireStrategyStatus>),
    /// Reply to [`Request::SettingsShow`]: every cell rendered (and redacted) by the node. BOXED.
    SettingsShow(Box<WireSettingsShow>),
    /// Reply to [`Request::Directory`].
    Directory(Box<crate::wire::WireDirectory>),
    /// Reply to an ACCEPTED `WireCommand::SetSetting` — not an [`Response::Ack`]: nothing entered
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
    /// Reply to [`Request::Tearsheet`]: the `vike_analytics::LiveTearsheet` JSON TEXT, the document
    /// `vike-backend report --json` emits on the daemon's own box.
    ///
    /// ⚠ **A `String`, not a typed payload**, the shape
    /// `crates/vike-datahub-client/src/proto.rs`'s `Response::Report` uses: a machine consumer gets
    /// the ONE schema the producer owns, never a hand-maintained copy. (Typing it would no longer
    /// cost a client a journal reader since the type moved to `vike-analytics`; it is a choice.)
    Tearsheet(String),
    /// Reply to an [`Request::Account`] whose verb was [`crate::wire::AccountVerb::List`]: the
    /// node's `account` rows and the credential key NAMES each owns. **Never a value.**
    AccountList(Box<crate::wire::WireAccountList>),
    /// Reply to an ACCEPTED account WRITE (not an [`Response::Ack`], for the
    /// [`Response::SettingsWritten`] reason). A refusal is the ordinary [`Response::Error`], naming
    /// key NAMES and never a value.
    AccountWritten(Box<crate::wire::WireAccountWritten>),
}

/// The capability ceiling a client authenticates under, bound INTO the signed auth message with
/// its OWN key per scope: [`Scope::Read`] is snapshots + subscriptions, [`Scope::Write`]
/// additionally admits order commands.
///
/// RE-EXPORTED from the shared primitive (`docs/decisions/0025-datahub-remote-posture.md`): the
/// scope TAG is in the signed message, so two `Scope` enums would be two tag tables.
pub use vike_node_proto::auth::Scope;

/// A live-stream topic a client may [`Request::Subscribe`] to — WHICH blocks of the coalesced
/// [`WireSnapshot`] a subscriber wants pushed.
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
