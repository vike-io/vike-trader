//! The ONE-SHOT reads: one fresh short-lived [`Scope::Read`] connection, one request, one reply.

use std::io;
use std::net::ToSocketAddrs;
use std::time::Duration;

use crate::handshake::{Refusal, ReplyWords, node_handshake, require_feature};
use crate::liveness::DIRECTORY_REPLY_TIMEOUT;
use crate::proto::{
    FEATURE_DIRECTORY, FEATURE_SETTINGS_SHOW, FEATURE_STRATEGY_PARAMS, FEATURE_STRATEGY_VERBS,
    FEATURE_TEARSHEET, Request, Response, Scope, read_frame, write_frame,
};
use crate::wire::{WireDirectory, WireSettingsShow, WireSnapshot, WireStrategyStatus};

// Each verb's refusal and reply wording, named once so the string table below pins exactly what
// the verbs send.
const SNAPSHOT: ReplyWords = ReplyWords::read("snapshot", "SnapshotFrame");
const STATUS: ReplyWords = ReplyWords::read("strategy status", "StrategyStatus");
const STATUS_REFUSED: Refusal = Refusal::OlderNode("StrategyStatus");
const PARAMS: ReplyWords = ReplyWords::read("strategy params", "StrategyStatus");
const PARAMS_REFUSED: Refusal = Refusal::OlderNode("the structured params read was");
const SETTINGS: ReplyWords = ReplyWords::read("settings show", "SettingsShow");
const SETTINGS_REFUSED: Refusal = Refusal::OlderNode("SettingsShow");
const DIRECTORY: ReplyWords = ReplyWords::read("directory", "Directory");
const DIRECTORY_REFUSED: Refusal = Refusal::OlderNode("Directory");
const TEARSHEET: ReplyWords = ReplyWords::read("tearsheet", "Tearsheet");
const TEARSHEET_REFUSED: Refusal = Refusal::Plain("Tearsheet");

/// Ask a node for ONE point-in-time [`WireSnapshot`]: handshake, [`Request::Snapshot`], read the
/// [`Response::SnapshotFrame`], drop.
///
/// ⚠ **Named for the ROUND TRIP**: [`crate::remote_handle::RemoteCoreHandle::snapshot`] reads the
/// last frame a subscription already pushed into a local cell (no I/O, possibly stale); this goes
/// to the node and back. A one-shot command needing one fact (the trading mode, say) wants this.
///
/// Not feature-negotiated: [`Request::Snapshot`] is a BASE verb every node serves. A server-side
/// [`Response::Error`] and an unexpected reply both surface as [`io::ErrorKind::InvalidData`], the
/// shape every one-shot read shares so a caller's exit ladder can key on "the node ANSWERED".
pub fn snapshot_once<A: ToSocketAddrs>(addr: A, observe_key: &[u8]) -> io::Result<WireSnapshot> {
    let (mut stream, _features) = node_handshake(addr, observe_key, Scope::Read)?;
    write_frame(&mut stream, &Request::Snapshot)?;
    match read_frame::<_, Response>(&mut stream)? {
        Response::SnapshotFrame(snap) => Ok(*snap),
        other => Err(SNAPSHOT.mismatch(other)),
    }
}

/// Ask a node WHAT IT IS RUNNING — the strategy-level read verb ([`Request::StrategyStatus`] →
/// [`Response::StrategyStatus`]) over a per-call [`Scope::Read`] connection.
///
/// **Feature-negotiated, refused CLIENT-SIDE**: sent only when the node advertises
/// [`FEATURE_STRATEGY_VERBS`]; otherwise [`io::ErrorKind::Unsupported`] naming the capability, and
/// NOTHING is sent after the handshake. A server-side [`Response::Error`] (e.g. a node publishing
/// no identity block) or an unexpected reply is [`io::ErrorKind::InvalidData`].
pub fn strategy_status<A: ToSocketAddrs>(
    addr: A,
    observe_key: &[u8],
) -> io::Result<WireStrategyStatus> {
    strategy_status_with_features(addr, observe_key).map(|(status, _)| status)
}

/// [`strategy_status`], keeping the node's advertised `Welcome.features` — the same call, so the
/// two cannot answer differently.
///
/// For a caller rendering a mount row: [`crate::proto::FEATURE_MOUNT_CLASS`] tells an old node
/// (which cannot carry `WireMountRow::asset_class`) from a current one whose mount simply has no
/// class yet; `#[serde(default)]` makes both `None`. The features come back VERBATIM, the wire's
/// own vocabulary, rather than as a second list of decoded booleans.
pub fn strategy_status_with_features<A: ToSocketAddrs>(
    addr: A,
    observe_key: &[u8],
) -> io::Result<(WireStrategyStatus, Vec<String>)> {
    let (mut stream, features) = node_handshake(addr, observe_key, Scope::Read)?;
    require_feature(&features, FEATURE_STRATEGY_VERBS, STATUS_REFUSED)?;
    write_frame(&mut stream, &Request::StrategyStatus)?;
    match read_frame::<_, Response>(&mut stream)? {
        Response::StrategyStatus(status) => Ok((*status, features)),
        other => Err(STATUS.mismatch(other)),
    }
}

/// Ask a node for its mounts' STRUCTURED LIVE PARAMS — wire-identically [`strategy_status`], but
/// negotiated on [`FEATURE_STRATEGY_PARAMS`] (refused client-side, nothing sent, without it).
///
/// ⚠ Why a second function rather than a field check: an older node ANSWERS this request, with
/// rows whose `venue`/`symbol`/`interval`/`typed_params` default to empty — indistinguishable from
/// "these mounts publish no typed params". Check the capability, never the emptiness of a field.
///
/// Each row's addressing key is what `WireCommand::UpdateParams` targets: a caller patches
/// `typed_params` and sends it back, naming the row's `mount_id` whenever the node advertises
/// [`crate::proto::FEATURE_PARAMS_BY_MOUNT`] (two mounts can share a key).
pub fn strategy_params<A: ToSocketAddrs>(
    addr: A,
    observe_key: &[u8],
) -> io::Result<WireStrategyStatus> {
    let (mut stream, features) = node_handshake(addr, observe_key, Scope::Read)?;
    require_feature(&features, FEATURE_STRATEGY_PARAMS, PARAMS_REFUSED)?;
    write_frame(&mut stream, &Request::StrategyStatus)?;
    match read_frame::<_, Response>(&mut stream)? {
        Response::StrategyStatus(status) => Ok(*status),
        other => Err(PARAMS.mismatch(other)),
    }
}

/// Ask a node for its EFFECTIVE SETTINGS ([`Request::SettingsShow`] → [`Response::SettingsShow`]),
/// [`strategy_status`]'s per-call shape.
///
/// **Feature-negotiated on [`FEATURE_SETTINGS_SHOW`], refused CLIENT-SIDE** with
/// [`io::ErrorKind::Unsupported`] and nothing sent (the GUI renders that as "server predates
/// settings-show"). A server-side [`Response::Error`] (no settings source, or settings that no
/// longer load) is [`io::ErrorKind::InvalidData`] carrying the node's text.
pub fn settings_show<A: ToSocketAddrs>(
    addr: A,
    observe_key: &[u8],
) -> io::Result<WireSettingsShow> {
    let (mut stream, features) = node_handshake(addr, observe_key, Scope::Read)?;
    require_feature(&features, FEATURE_SETTINGS_SHOW, SETTINGS_REFUSED)?;
    write_frame(&mut stream, &Request::SettingsShow)?;
    match read_frame::<_, Response>(&mut stream)? {
        Response::SettingsShow(show) => Ok(*show),
        other => Err(SETTINGS.mismatch(other)),
    }
}

/// Ask a node for its DIRECTORY — the roster venues its settings database names and the active
/// accounts it holds ([`Request::Directory`]), [`settings_show`]'s per-call shape.
///
/// **Feature-negotiated on [`FEATURE_DIRECTORY`], refused CLIENT-SIDE**, nothing sent. A node that
/// cannot list its accounts answers [`Response::Error`] ([`io::ErrorKind::InvalidData`]), never an
/// empty directory.
///
/// ⚠ **The reply is read under [`DIRECTORY_REPLY_TIMEOUT`]**: the handshake's bound is cleared on
/// the way out, so without it a node that never answered parked the desktop's directory slot for
/// the session. Expiry is an error like any refusal; the caller asks again later.
pub fn directory<A: ToSocketAddrs>(addr: A, observe_key: &[u8]) -> io::Result<WireDirectory> {
    directory_with_deadline(addr, observe_key, DIRECTORY_REPLY_TIMEOUT)
}

/// [`directory`] with the reply deadline as a PARAMETER — the seam the unit tests drive in
/// milliseconds. Single-caller in production.
pub(crate) fn directory_with_deadline<A: ToSocketAddrs>(
    addr: A,
    observe_key: &[u8],
    reply_deadline: Duration,
) -> io::Result<WireDirectory> {
    let (mut stream, features) = node_handshake(addr, observe_key, Scope::Read)?;
    stream.set_read_timeout(Some(reply_deadline))?;
    require_feature(&features, FEATURE_DIRECTORY, DIRECTORY_REFUSED)?;
    write_frame(&mut stream, &Request::Directory)?;
    match read_frame::<_, Response>(&mut stream)? {
        Response::Directory(dir) => Ok(*dir),
        other => Err(DIRECTORY.mismatch(other)),
    }
}

/// Ask a node to render a LIVE TEARSHEET from the journal IT is writing ([`Request::Tearsheet`]),
/// [`settings_show`]'s per-call shape. Returns the `vike_analytics::LiveTearsheet` JSON TEXT
/// verbatim (see [`Response::Tearsheet`] for why a string).
///
/// The node renders next to the data and returns the ANSWER (a few dozen numbers) rather than
/// shipping an ever-growing journal: the compute-to-data rule
/// `crates/vike-datahub-client/src/client/compute.rs`'s `run_backtest` is built on.
///
/// **Feature-negotiated on [`FEATURE_TEARSHEET`], refused CLIENT-SIDE** against a daemon that
/// PREDATES the arm (`crates/vike-tradehub/src/server/tearsheet.rs`'s `tearsheet_reply`; current
/// nodes advertise it), nothing sent. ⚠ Do NOT read a [`Response::Error`] as that refusal: a node
/// that serves the verb can still fail to resolve its own journal directory, surfaced as
/// [`io::ErrorKind::InvalidData`] — one is a claim about the peer's BINARY, the other about how
/// that process was started.
///
/// `seed` and `periods_per_year` are `None` for "the renderer's own default"; the node owns those.
pub fn tearsheet<A: ToSocketAddrs>(
    addr: A,
    observe_key: &[u8],
    seed: Option<f64>,
    periods_per_year: Option<f64>,
) -> io::Result<String> {
    let (mut stream, features) = node_handshake(addr, observe_key, Scope::Read)?;
    require_feature(&features, FEATURE_TEARSHEET, TEARSHEET_REFUSED)?;
    write_frame(&mut stream, &Request::Tearsheet { seed, periods_per_year })?;
    match read_frame::<_, Response>(&mut stream)? {
        Response::Tearsheet(json) => Ok(json),
        other => Err(TEARSHEET.mismatch(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⚠ THE STRING TABLE: every one-shot verb's refusal and reply-mismatch text, byte for byte as
    /// each verb spelled it inline before the shared helpers existed (the literals below are
    /// copied from that code). Callers and operators read these; a helper edit must not drift
    /// them. The two write verbs (`remote_control`'s `preview_command`, `set_setting`) are pinned
    /// through the same helpers with their own words.
    #[test]
    fn every_one_shot_refusal_and_mismatch_text_is_pinned() {
        let refused = |feature: &str, refusal: Refusal| {
            let err = require_feature(&["observe".to_string()], feature, refusal)
                .expect_err("an unadvertised feature is refused");
            assert_eq!(err.kind(), io::ErrorKind::Unsupported);
            err.to_string()
        };
        for (got, want) in [
            (
                refused(FEATURE_STRATEGY_VERBS, STATUS_REFUSED),
                "this node does not advertise the \"strategy-verbs\" capability (an older vike-tradehub) — StrategyStatus refused client-side, nothing was sent",
            ),
            (
                refused(FEATURE_STRATEGY_PARAMS, PARAMS_REFUSED),
                "this node does not advertise the \"strategy-params\" capability (an older vike-tradehub) — the structured params read was refused client-side, nothing was sent",
            ),
            (
                refused(FEATURE_SETTINGS_SHOW, SETTINGS_REFUSED),
                "this node does not advertise the \"settings-show\" capability (an older vike-tradehub) — SettingsShow refused client-side, nothing was sent",
            ),
            (
                refused(FEATURE_DIRECTORY, DIRECTORY_REFUSED),
                "this node does not advertise the \"directory\" capability (an older vike-tradehub) — Directory refused client-side, nothing was sent",
            ),
            (
                refused(FEATURE_TEARSHEET, TEARSHEET_REFUSED),
                "this node does not advertise the \"tearsheet\" capability — Tearsheet refused client-side, nothing was sent",
            ),
            (
                refused("bracket", Refusal::OlderNode("preview")),
                "this node does not advertise the \"bracket\" capability (an older vike-tradehub) — preview refused client-side, nothing was sent",
            ),
            (
                refused("settings-write", Refusal::OlderNode("SetSetting")),
                "this node does not advertise the \"settings-write\" capability (an older vike-tradehub) — SetSetting refused client-side, nothing was sent",
            ),
        ] {
            assert_eq!(got, want);
        }
        assert!(
            require_feature(&["directory".to_string()], FEATURE_DIRECTORY, DIRECTORY_REFUSED)
                .is_ok()
        );

        let preview = ReplyWords {
            name: "preview",
            expected: "Preview",
            error_word: "error",
            auth_denied: true,
        };
        let write = ReplyWords {
            name: "settings write",
            expected: "SettingsWritten",
            error_word: "refused",
            auth_denied: true,
        };
        let mismatch = |words: ReplyWords, reply: Response, kind: io::ErrorKind| {
            let err = words.mismatch(reply);
            assert_eq!(err.kind(), kind, "{err}");
            err.to_string()
        };
        let bad = io::ErrorKind::InvalidData;
        for (got, want) in [
            (
                mismatch(SNAPSHOT, Response::Error("boom".into()), bad),
                "tradehub snapshot error: boom",
            ),
            (
                mismatch(SNAPSHOT, Response::Pong, bad),
                "tradehub snapshot: expected SnapshotFrame, got Pong",
            ),
            (
                mismatch(STATUS, Response::Error("boom".into()), bad),
                "tradehub strategy status error: boom",
            ),
            (
                mismatch(STATUS, Response::Pong, bad),
                "tradehub strategy status: expected StrategyStatus, got Pong",
            ),
            (
                mismatch(PARAMS, Response::Error("boom".into()), bad),
                "tradehub strategy params error: boom",
            ),
            (
                mismatch(PARAMS, Response::Pong, bad),
                "tradehub strategy params: expected StrategyStatus, got Pong",
            ),
            (
                mismatch(SETTINGS, Response::Error("boom".into()), bad),
                "tradehub settings show error: boom",
            ),
            (
                mismatch(SETTINGS, Response::Pong, bad),
                "tradehub settings show: expected SettingsShow, got Pong",
            ),
            (
                mismatch(DIRECTORY, Response::Error("boom".into()), bad),
                "tradehub directory error: boom",
            ),
            (
                mismatch(DIRECTORY, Response::Pong, bad),
                "tradehub directory: expected Directory, got Pong",
            ),
            (
                mismatch(TEARSHEET, Response::Error("boom".into()), bad),
                "tradehub tearsheet error: boom",
            ),
            (
                mismatch(TEARSHEET, Response::Pong, bad),
                "tradehub tearsheet: expected Tearsheet, got Pong",
            ),
            // A READ verb has no AuthDenied arm: it is just another unexpected reply.
            (
                mismatch(STATUS, Response::AuthDenied { reason: "no".into() }, bad),
                "tradehub strategy status: expected StrategyStatus, got AuthDenied",
            ),
            (
                mismatch(
                    preview,
                    Response::AuthDenied { reason: "no".into() },
                    io::ErrorKind::PermissionDenied,
                ),
                "tradehub preview denied: no",
            ),
            (
                mismatch(preview, Response::Error("boom".into()), bad),
                "tradehub preview error: boom",
            ),
            (
                mismatch(preview, Response::Pong, bad),
                "tradehub preview: expected Preview, got Pong",
            ),
            (
                mismatch(
                    write,
                    Response::AuthDenied { reason: "no".into() },
                    io::ErrorKind::PermissionDenied,
                ),
                "tradehub settings write denied: no",
            ),
            (
                mismatch(write, Response::Error("boom".into()), bad),
                "tradehub settings write refused: boom",
            ),
            (
                mismatch(write, Response::Pong, bad),
                "tradehub settings write: expected SettingsWritten, got Pong",
            ),
        ] {
            assert_eq!(got, want);
        }
    }
}
