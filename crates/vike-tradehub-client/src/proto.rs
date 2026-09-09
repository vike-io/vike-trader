//! The vike-tradehub **node** wire protocol: the request/response schema a thin GUI client and the
//! headless live core ("tradehub") exchange, plus the authentication challenge that gates it.
//!
//! # Framing — ONE codec across both services
//!
//! This module re-exports the length-prefixed `serde_json` frame codec from
//! [`vike_datahub_client::proto`] ([`read_frame`] / [`write_frame`] / [`MAX_FRAME_LEN`]) rather than
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
//!    [`Request::Snapshot`], or (with [`Scope::Control`]) [`Request::Command`].
//!
//! The [`Scope`] a client authenticates under is its capability ceiling: [`Scope::Observe`] is
//! read-only (snapshots + subscriptions), [`Scope::Control`] additionally admits order commands. The
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
pub use vike_datahub_client::proto::{
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
/// authenticated scope is [`Scope::Control`]. (That enforcement lives in the future server; this
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
    /// Submit one order-scoped command (post-auth, [`Scope::Control`] only). Answered by
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
    /// DRY-RUN a command (post-auth, [`Scope::Control`] only): ask the node what its server-side
    /// gate would decide WITHOUT executing anything. The command is NOT lowered, NOT handed to the
    /// core, and NO order is placed — the node runs only its edge validation (the notional/rate
    /// `ControlLimits`-shaped checks) and answers [`Response::Preview`] with the verdict. Same scope
    /// gating as [`Request::Command`]: an [`Scope::Observe`] peer is refused. Purely read-only.
    Preview(WireCommand),
    /// Liveness probe (post-auth). Answered by [`Response::Pong`].
    Ping,
    /// STRATEGY-level read (split-plane B4, post-auth, EITHER scope — read-only, so
    /// [`Scope::Observe`] suffices): what is this node running? Answered by
    /// [`Response::StrategyStatus`] (identity + effective params + the mounted-strategy rows), or
    /// [`Response::Error`] on a node that publishes no identity block. Feature-gated CLIENT-side on
    /// [`FEATURE_STRATEGY_VERBS`] — see that constant for why there is no version bump.
    StrategyStatus,
    /// SETTINGS read (split-plane REQ-7, read half; post-auth, EITHER scope — read-only and
    /// redacted-by-construction, so [`Scope::Observe`] suffices; the scope argument lives at the
    /// server arm). Ask the node for its effective settings-file rows — the same rows
    /// `vike-cli config show`'s files table prints, rendered by the node from the shared
    /// `vike_config::show` builder. Answered by [`Response::SettingsShow`], or
    /// [`Response::Error`] on a node started without a settings source or whose settings no
    /// longer load. Feature-gated CLIENT-side on [`FEATURE_SETTINGS_SHOW`] — see that constant
    /// for why there is no version bump. The WRITE half is `WireCommand::SetSetting` (a
    /// [`Request::Command`] payload, gated on [`FEATURE_SETTINGS_WRITE`]), answered by
    /// [`Response::SettingsWritten`].
    SettingsShow,
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
}

/// The capability ceiling a client authenticates under. The scope is bound INTO the signed auth
/// message (see [`crate::auth::sign`]) and each scope signs under its OWN key
/// ([`crate::auth::NodeKeys`]), so a client holding only the observe key can never forge a `Control`
/// mac — capability is cryptographic, not a claim the server has to trust.
///
/// On THIS protocol: [`Scope::Observe`] is snapshots + subscriptions and may NOT issue
/// [`Request::Command`]; [`Scope::Control`] additionally admits order commands.
///
/// ⚠ RE-EXPORTED, not declared here, since `docs/decisions/0025-datahub-remote-posture.md` gave the
/// datahub the same handshake: the scope TAG is folded into the signed message, so two `Scope`
/// enums would be two tag tables — exactly the disagreement a shared primitive exists to make
/// impossible. The variant names ARE the wire and are unchanged, so the move is invisible here.
pub use vike_datahub_client::node_auth::Scope;

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
            Request::Auth { scope: Scope::Control, mac: vec![1, 2, 3, 4] },
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
            Response::AuthOk { scope: Scope::Observe },
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
