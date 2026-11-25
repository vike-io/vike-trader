//! The ONE-SHOT reads: one fresh short-lived [`Scope::Read`] connection, one request, one reply.
//!
//! Every reply is read under a bound: [`DIRECTORY_REPLY_TIMEOUT`] for the directory, and
//! `crate::liveness::ONE_SHOT_REPLY_TIMEOUT` (armed by `crate::handshake::one_shot_handshake`) for
//! the rest, so a node that went silent after the handshake fails the call instead of parking it.
//!
//! Each returns a [`HubCallError`] (`crate::handshake`'s module doc says which phase is which variant): a
//! capability the node does not advertise is `NotOffered`, nothing sent; the node's own no is `Refused`
//! (`NotNow` for a node still starting); a reply of another verb is `Protocol`.

use std::net::ToSocketAddrs;
use std::time::Duration;

use crate::handshake::{
    Refusal, ReplyWords, ask, hub_handshake, one_shot_handshake, require_feature,
};
use crate::liveness::DIRECTORY_REPLY_TIMEOUT;
use crate::proto::{
    FEATURE_DIRECTORY, FEATURE_SETTINGS_SHOW, FEATURE_STRATEGY_PARAMS, FEATURE_STRATEGY_VERBS,
    FEATURE_VENUE_READ, FEATURE_VENUE_READ_POSITIONS, Request, Response,
};
use crate::wire::{AccountDirectoryWire, CoreSnapshotWire, SettingsShowWire, StrategyStatusWire};
use vike_hub_protocol::access::Signer;
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::call::HubCallError;
use vike_model::{VenueRead, VenueReadKind};

// Each verb's refusal and reply wording, named once so the string table below pins exactly what
// the verbs send.
const SNAPSHOT: ReplyWords = ReplyWords::read("snapshot", "SnapshotFrame");
const STATUS: ReplyWords = ReplyWords::read("strategy status", "StrategyStatus");
const STATUS_REFUSED: Refusal = Refusal::OlderHub("StrategyStatus");
const PARAMS: ReplyWords = ReplyWords::read("strategy params", "StrategyStatus");
const PARAMS_REFUSED: Refusal = Refusal::OlderHub("the structured params read was");
const SETTINGS: ReplyWords = ReplyWords::read("settings show", "SettingsShow");
const SETTINGS_REFUSED: Refusal = Refusal::OlderHub("SettingsShow");
const DIRECTORY: ReplyWords = ReplyWords::read("directory", "Directory");
const DIRECTORY_REFUSED: Refusal = Refusal::OlderHub("Directory");
const VENUE_READ: ReplyWords = ReplyWords::read("venue read", "VenueRead");
const VENUE_READ_REFUSED: Refusal = Refusal::OlderHub("VenueRead");
const VENUE_READ_POSITIONS_REFUSED: Refusal = Refusal::OlderHub("VenueRead positions");

/// Ask a node for ONE point-in-time [`CoreSnapshotWire`]: handshake, [`Request::Snapshot`], read the
/// [`Response::SnapshotFrame`], drop.
///
/// ⚠ **Named for the ROUND TRIP**: [`crate::tradehub_handle::TradehubCoreHandle::snapshot`] reads the
/// last frame a subscription already pushed into a local cell (no I/O, possibly stale); this goes
/// to the node and back. A one-shot command needing one fact (the trading mode, say) wants this.
///
/// Not feature-negotiated: [`Request::Snapshot`] is a BASE verb every node serves. A server-side
/// [`Response::Error`] is [`HubCallError::Refused`] and an unexpected reply [`HubCallError::Protocol`],
/// both the node ANSWERING, which a caller's exit ladder keys on (`HubCallError::failure_outcome`).
pub fn snapshot_once<'k, A: ToSocketAddrs>(
    addr: A,
    observe_key: impl Into<Signer<'k>>,
) -> Result<CoreSnapshotWire, HubCallError> {
    let (mut stream, features) = one_shot_handshake(addr, observe_key, Scope::Read)?;
    match ask(&mut stream, &features, &Request::Snapshot)? {
        Response::SnapshotFrame(snap) => Ok(*snap),
        other => Err(SNAPSHOT.mismatch(other)),
    }
}

/// Ask a node WHAT IT IS RUNNING — the strategy-level read verb ([`Request::StrategyStatus`] →
/// [`Response::StrategyStatus`]) over a per-call [`Scope::Read`] connection.
///
/// **Feature-negotiated, refused CLIENT-SIDE**: sent only when the node advertises
/// [`FEATURE_STRATEGY_VERBS`]; otherwise [`HubCallError::NotOffered`] naming the capability, and
/// NOTHING is sent after the handshake. A server-side [`Response::Error`] (e.g. a node publishing
/// no identity block) is [`HubCallError::Refused`], an unexpected reply [`HubCallError::Protocol`].
pub fn strategy_status<'k, A: ToSocketAddrs>(
    addr: A,
    observe_key: impl Into<Signer<'k>>,
) -> Result<StrategyStatusWire, HubCallError> {
    strategy_status_with_features(addr, observe_key).map(|(status, _)| status)
}

/// [`strategy_status`], keeping the node's advertised `Welcome.features` — the same call, so the
/// two cannot answer differently.
///
/// For a caller rendering a mount row: [`crate::proto::FEATURE_MOUNT_CLASS`] tells an old node
/// (which cannot carry `MountViewWire::asset_class`) from a current one whose mount simply has no
/// class yet; `#[serde(default)]` makes both `None`. The features come back VERBATIM, the wire's
/// own vocabulary, rather than as a second list of decoded booleans.
pub fn strategy_status_with_features<'k, A: ToSocketAddrs>(
    addr: A,
    observe_key: impl Into<Signer<'k>>,
) -> Result<(StrategyStatusWire, Vec<String>), HubCallError> {
    strategy_status_under(addr, observe_key, FEATURE_STRATEGY_VERBS, STATUS_REFUSED, STATUS)
}

/// Ask a node for its mounts' STRUCTURED LIVE PARAMS — wire-identically [`strategy_status`], but
/// negotiated on [`FEATURE_STRATEGY_PARAMS`] (refused client-side, nothing sent, without it).
///
/// ⚠ Why a second function rather than a field check: an older node ANSWERS this request, with
/// rows whose `venue`/`symbol`/`interval`/`typed_params` default to empty — indistinguishable from
/// "these mounts publish no typed params". Check the capability, never the emptiness of a field.
///
/// Each row's addressing key is what `CommandWire::UpdateParams` targets: a caller patches
/// `typed_params` and sends it back, naming the row's `mount_id` whenever the node advertises
/// [`crate::proto::FEATURE_PARAMS_BY_MOUNT`] (two mounts can share a key).
pub fn strategy_params<'k, A: ToSocketAddrs>(
    addr: A,
    observe_key: impl Into<Signer<'k>>,
) -> Result<StrategyStatusWire, HubCallError> {
    strategy_status_under(addr, observe_key, FEATURE_STRATEGY_PARAMS, PARAMS_REFUSED, PARAMS)
        .map(|(status, _)| status)
}

/// The one `Request::StrategyStatus` exchange behind [`strategy_status_with_features`] and
/// [`strategy_params`]: a [`Scope::Read`] handshake, `feature` required (refused client-side with
/// `refusal`, nothing sent), the request, and the status with the node's advertised features; any other
/// reply is `words`' mismatch.
fn strategy_status_under<'k, A: ToSocketAddrs>(
    addr: A,
    observe_key: impl Into<Signer<'k>>,
    feature: &str,
    refusal: Refusal,
    words: ReplyWords,
) -> Result<(StrategyStatusWire, Vec<String>), HubCallError> {
    let (mut stream, features) = one_shot_handshake(addr, observe_key, Scope::Read)?;
    require_feature(&features, feature, refusal)?;
    match ask(&mut stream, &features, &Request::StrategyStatus)? {
        Response::StrategyStatus(status) => Ok((*status, features)),
        other => Err(words.mismatch(other)),
    }
}

/// Ask a node for its EFFECTIVE SETTINGS ([`Request::SettingsShow`] → [`Response::SettingsShow`]),
/// [`strategy_status`]'s per-call shape.
///
/// **Feature-negotiated on [`FEATURE_SETTINGS_SHOW`], refused CLIENT-SIDE** with
/// [`HubCallError::NotOffered`] and nothing sent (the GUI renders that as "server predates
/// settings-show"). A server-side [`Response::Error`] (no settings source, or settings that no
/// longer load) is [`HubCallError::Refused`] carrying the node's text.
pub fn settings_show<'k, A: ToSocketAddrs>(
    addr: A,
    observe_key: impl Into<Signer<'k>>,
) -> Result<SettingsShowWire, HubCallError> {
    let (mut stream, features) = one_shot_handshake(addr, observe_key, Scope::Read)?;
    require_feature(&features, FEATURE_SETTINGS_SHOW, SETTINGS_REFUSED)?;
    match ask(&mut stream, &features, &Request::SettingsShow)? {
        Response::SettingsShow(show) => Ok(*show),
        other => Err(SETTINGS.mismatch(other)),
    }
}

/// Ask a node for its DIRECTORY — the roster venues its settings database names and the active
/// accounts it holds ([`Request::Directory`]), [`settings_show`]'s per-call shape.
///
/// **Feature-negotiated on [`FEATURE_DIRECTORY`], refused CLIENT-SIDE**, nothing sent. A node that
/// cannot list its accounts answers [`Response::Error`] ([`HubCallError::Refused`]), never an
/// empty directory.
///
/// ⚠ **The reply is read under [`DIRECTORY_REPLY_TIMEOUT`]**: the handshake's bound is cleared on
/// the way out, so without it a node that never answered parked the desktop's directory slot for
/// the session. Expiry is an error like any refusal; the caller asks again later.
pub fn directory<'k, A: ToSocketAddrs>(
    addr: A,
    observe_key: impl Into<Signer<'k>>,
) -> Result<AccountDirectoryWire, HubCallError> {
    directory_with_deadline(addr, observe_key, DIRECTORY_REPLY_TIMEOUT)
}

/// [`directory`] with the reply deadline as a PARAMETER — the seam the unit tests drive in
/// milliseconds. Single-caller in production.
pub(crate) fn directory_with_deadline<'k, A: ToSocketAddrs>(
    addr: A,
    observe_key: impl Into<Signer<'k>>,
    reply_deadline: Duration,
) -> Result<AccountDirectoryWire, HubCallError> {
    let (mut stream, features, _) = hub_handshake(addr, observe_key, Scope::Read)?;
    stream.set_read_timeout(Some(reply_deadline)).map_err(HubCallError::NotSent)?;
    require_feature(&features, FEATURE_DIRECTORY, DIRECTORY_REFUSED)?;
    match ask(&mut stream, &features, &Request::Directory)? {
        Response::Directory(dir) => Ok(*dir),
        other => Err(DIRECTORY.mismatch(other)),
    }
}

/// Ask a node to READ one account at its venue ([`Request::VenueRead`] → [`Response::VenueRead`]): its
/// balance with the allowances the venue states, the venue's own list of its open orders, or the positions
/// it holds. `account` is `DEFAULT` for the venue's unlabelled account, else the label. [`directory`]'s
/// per-call shape over a [`Scope::Read`] connection.
///
/// **Feature-negotiated on [`FEATURE_VENUE_READ`] (positions: on [`FEATURE_VENUE_READ_POSITIONS`], which
/// the kind's name needs to decode), refused CLIENT-SIDE** ([`HubCallError::NotOffered`], nothing sent
/// after the handshake). Every refusal the NODE makes is its [`Response::Error`], surfaced as
/// [`HubCallError::Refused`] carrying the operator's sentence (the account is not running there, the
/// venue's bridge offers no such read, the read came too soon after the last, the venue refused): the node
/// ANSWERED, which an exit ladder keys on.
///
/// ⚠ **The reply is read under `crate::liveness::ONE_SHOT_REPLY_TIMEOUT`**, which armed on the way out of
/// the handshake: the node bounds its own ask of the gatewayhub below that, so a slow venue comes back as
/// the node's words and not as this call's timeout.
pub fn venue_read<'k, A: ToSocketAddrs>(
    addr: A,
    observe_key: impl Into<Signer<'k>>,
    venue: &str,
    account: &str,
    kind: VenueReadKind,
) -> Result<VenueRead, HubCallError> {
    let (mut stream, features) = one_shot_handshake(addr, observe_key, Scope::Read)?;
    // ⚠ Positions owes its OWN word: a node with only `venue-read` cannot decode the kind's name.
    match kind {
        VenueReadKind::Positions => {
            require_feature(&features, FEATURE_VENUE_READ_POSITIONS, VENUE_READ_POSITIONS_REFUSED)?
        }
        VenueReadKind::Balance | VenueReadKind::OpenOrders => {
            require_feature(&features, FEATURE_VENUE_READ, VENUE_READ_REFUSED)?
        }
    }
    let read = Request::VenueRead { venue: venue.to_string(), account: account.to_string(), kind };
    match ask(&mut stream, &features, &read)? {
        Response::VenueRead(read) => Ok(*read),
        other => Err(VENUE_READ.mismatch(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⚠ THE STRING TABLE: every one-shot verb's refusal and reply-mismatch text, byte for byte as
    /// each verb spelled it inline before the shared helpers existed (the literals below are
    /// copied from that code). Callers and operators read these; a helper edit must not drift
    /// them. The two write verbs (`tradehub_control`'s `preview_command`, `set_setting`) are pinned
    /// through the same helpers with their own words.
    #[test]
    fn every_one_shot_refusal_and_mismatch_text_is_pinned() {
        let refused = |feature: &str, refusal: Refusal| {
            let err = require_feature(&["observe".to_string()], feature, refusal)
                .expect_err("an unadvertised feature is refused");
            assert!(
                matches!(&err, HubCallError::NotOffered { feature: named, .. } if named == feature),
                "{err:?}"
            );
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
                refused(FEATURE_VENUE_READ, VENUE_READ_REFUSED),
                "this node does not advertise the \"venue-read\" capability (an older vike-tradehub) — VenueRead refused client-side, nothing was sent",
            ),
            (
                refused(FEATURE_VENUE_READ_POSITIONS, VENUE_READ_POSITIONS_REFUSED),
                "this node does not advertise the \"venue-read-positions\" capability (an older vike-tradehub) — VenueRead positions refused client-side, nothing was sent",
            ),
            (
                refused("bracket", Refusal::OlderHub("preview")),
                "this node does not advertise the \"bracket\" capability (an older vike-tradehub) — preview refused client-side, nothing was sent",
            ),
            (
                refused("settings-write", Refusal::OlderHub("SetSetting")),
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
        // The variant each reply is: the node's own no, a reply of another verb, a refused key.
        let mismatch = |words: ReplyWords, reply: Response, is: fn(&HubCallError) -> bool| {
            let err = words.mismatch(reply);
            assert!(is(&err), "{err:?}");
            err.to_string()
        };
        let answered: fn(&HubCallError) -> bool =
            |e| matches!(e, HubCallError::Refused { id: None, .. });
        let skew: fn(&HubCallError) -> bool = |e| matches!(e, HubCallError::Protocol(_));
        let denied: fn(&HubCallError) -> bool = |e| matches!(e, HubCallError::AccessDenied(_));
        for (got, want) in [
            (
                mismatch(SNAPSHOT, Response::Error("boom".into()), answered),
                "tradehub snapshot error: boom",
            ),
            (
                mismatch(SNAPSHOT, Response::Pong, skew),
                "tradehub snapshot: expected SnapshotFrame, got Pong",
            ),
            (
                mismatch(STATUS, Response::Error("boom".into()), answered),
                "tradehub strategy status error: boom",
            ),
            (
                mismatch(STATUS, Response::Pong, skew),
                "tradehub strategy status: expected StrategyStatus, got Pong",
            ),
            (
                mismatch(PARAMS, Response::Error("boom".into()), answered),
                "tradehub strategy params error: boom",
            ),
            (
                mismatch(PARAMS, Response::Pong, skew),
                "tradehub strategy params: expected StrategyStatus, got Pong",
            ),
            (
                mismatch(SETTINGS, Response::Error("boom".into()), answered),
                "tradehub settings show error: boom",
            ),
            (
                mismatch(SETTINGS, Response::Pong, skew),
                "tradehub settings show: expected SettingsShow, got Pong",
            ),
            (
                mismatch(DIRECTORY, Response::Error("boom".into()), answered),
                "tradehub directory error: boom",
            ),
            (
                mismatch(DIRECTORY, Response::Pong, skew),
                "tradehub directory: expected Directory, got Pong",
            ),
            (
                mismatch(VENUE_READ, Response::Error("not offered by binance".into()), answered),
                "tradehub venue read error: not offered by binance",
            ),
            (
                mismatch(VENUE_READ, Response::Pong, skew),
                "tradehub venue read: expected VenueRead, got Pong",
            ),
            // A READ verb has no AuthDenied arm: it is just another unexpected reply.
            (
                mismatch(STATUS, Response::AuthDenied { reason: "no".into() }, skew),
                "tradehub strategy status: expected StrategyStatus, got AuthDenied",
            ),
            (
                mismatch(preview, Response::AuthDenied { reason: "no".into() }, denied),
                "error [hub.access.denied]: tradehub preview denied: no",
            ),
            (
                mismatch(preview, Response::Error("boom".into()), answered),
                "tradehub preview error: boom",
            ),
            (
                mismatch(preview, Response::Pong, skew),
                "tradehub preview: expected Preview, got Pong",
            ),
            (
                mismatch(write, Response::AuthDenied { reason: "no".into() }, denied),
                "error [hub.access.denied]: tradehub settings write denied: no",
            ),
            (
                mismatch(write, Response::Error("boom".into()), answered),
                "tradehub settings write refused: boom",
            ),
            (
                mismatch(write, Response::Pong, skew),
                "tradehub settings write: expected SettingsWritten, got Pong",
            ),
        ] {
            assert_eq!(got, want);
        }
    }
}
