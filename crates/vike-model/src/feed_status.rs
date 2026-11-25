//! Live per-venue connection status: the runtime counterpart to `vike_account_manager::grid`'s
//! static credential-configuration grid.
//!
//! **The producer SETS the state; nothing recovers it from the words.** A status is a
//! [`DataClientStatus`]: the [`DataClientConnectionState`] its producer knows (it dialled, it is live,
//! it failed, it stopped) and the human line beside it. Every consumer (the status strip's dot, the
//! Connections tool, the Trade window's feed badge, the daemon's reconcile health gate) reads
//! [`DataClientStatus::state`]; [`DataClientStatus::text`] is for people only, so a word in it (a
//! backend named `error-box`, a hint that says "press Connect") can never move a dot. There is no
//! reader that guesses a state from the words, and there must not be one: a keyword classifier
//! lived here until 2026-11-21 and every round of it went wrong on a word.
//!
//! Producers: each bridge's live market feed writes its client's one line through
//! `vike_bridge_core::feed_ctx::set_status`; the desktop, which opens no venue socket, writes its own
//! through `vike_app_core::data::market_data_session` (one line per venue the datahub link carries)
//! and `vike_app_core::backend::observe_bridge` (the tradehub link). A venue with no producer is absent
//! from the map and reports [`DataClientConnectionState::Unknown`].
//!
//! No status line crosses a wire: the bridge feeds run in the gatewayhub and say their status in its
//! journal, and the desktop builds its lines from typed wire facts (the datahub's `StreamStatus`
//! frames, the observe link's own connect result). So there is no old peer whose text must be read.
//!
//! **An ORDER path answers in the same state**, through the one mapping [`order_path_state`] over the
//! facts its reading carries (up or down, the failures since it last worked). The gatewayhub reads a
//! bridge's `vike_bridge_core::venue_link::VenueLinkState` through it, and the tradehub reads the same
//! facts off the wire (`VenueLinkWire`: `up`, `failed_restarts`) through it, so both ends of the status
//! verb say one state of one path without a state crossing the wire.
//!
//! It sits in `vike-model`, the bottom layer, because its consumers have opposite dependency budgets:
//! the egui Connections tool (`vike-account-manager`) and the HEADLESS daemon's
//! `vike_tradehub::reconcile_config::health_from_feed_status`, which may not reach through an egui
//! crate. Nothing re-exports it; every consumer names this module.

/// Coarse live connection state for one venue, rendered as the Status-column dot in the
/// Connections tool. `Unknown` is the default — a venue with no live status producer wired in
/// always reads `Unknown`, never guessed as `Connected` or `Disconnected`. `Disconnected` is a
/// producer that is STOPPED or idle on purpose; a failure is `Error`, a retry `Connecting`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DataClientConnectionState {
    #[default]
    Unknown,
    Disconnected,
    Connecting,
    Connected,
    Error,
}

/// One data client's status as its PRODUCER set it: the [`state`](Self::state) every consumer reads
/// and the [`text`](Self::text) shown to people. The default is `Unknown` with no words: a line
/// nobody has written yet.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DataClientStatus {
    /// What the producer says the connection is. The only field a consumer decides anything from.
    pub state: DataClientConnectionState,
    /// The line for people, shown verbatim. Never classified.
    pub text: String,
}

impl DataClientStatus {
    /// A status in `state`, saying `text`.
    pub fn new(state: DataClientConnectionState, text: impl Into<String>) -> Self {
        DataClientStatus { state, text: text.into() }
    }
}

/// **An order path's reading in the one state vocabulary**: `up` is what its producer said (`None`: nothing yet, or a
/// word older than its bound, so not known), and `failures` the opens, restarts or sessions that failed since it last
/// worked. A path that works is `Connected`; one that is down with no failure is a reopen after the venue ended a session
/// (a close frame, an expiry), so `Connecting`; one that is down with a failure (an open, a login or a session that
/// failed, or a producer that gave up) is `Error`. Never `Disconnected`: a path nothing runs is not known (ruled U-6).
/// So a producer that counts no retries still counts a loss for a real reason as one
/// (`crates/vike-bridge-core/src/venue_link.rs`'s `VenueLinkDown::failed`): its `0` would read as a routine reopen.
#[must_use]
pub fn order_path_state(up: Option<bool>, failures: u32) -> DataClientConnectionState {
    match up {
        None => DataClientConnectionState::Unknown,
        Some(true) => DataClientConnectionState::Connected,
        Some(false) if failures == 0 => DataClientConnectionState::Connecting,
        Some(false) => DataClientConnectionState::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The order path's states: only a word that it works is `Connected`, a reopen with no failure yet is `Connecting`,
    /// any failure is `Error`, and an unsaid or stale word is `Unknown`.
    #[test]
    fn an_order_path_reads_in_the_one_state() {
        use DataClientConnectionState::{Connected, Connecting, Error, Unknown};
        assert_eq!(order_path_state(None, 0), Unknown);
        assert_eq!(order_path_state(None, 3), Unknown, "a count says nothing of an unvouched path");
        assert_eq!(order_path_state(Some(true), 0), Connected);
        assert_eq!(order_path_state(Some(false), 0), Connecting);
        assert_eq!(order_path_state(Some(false), 1), Error);
        assert_eq!(order_path_state(Some(false), u32::MAX), Error);
    }

    #[test]
    fn default_is_unknown() {
        assert_eq!(DataClientConnectionState::default(), DataClientConnectionState::Unknown);
        assert_eq!(
            DataClientStatus::default(),
            DataClientStatus::new(DataClientConnectionState::Unknown, "")
        );
    }
}
