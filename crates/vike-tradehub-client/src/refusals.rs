//! The tradehub's refusals a CLIENT decides on, declared once (`vike_utility::refusal_enum!`) in the crate the node
//! and every reader of its wire link, and sent as the refusal frame (`vike_hub_protocol::refusal::HubRefusal`,
//! [`crate::proto::Response::Refused`]) to a client that said it reads one. Their texts are byte for byte what the
//! node's `Response::Error` always carried, so an older client, answered that `Error`, reads what it always read.
//!
//! A refusal only SHOWN to a person keeps `Response::Error` until refusal step 3 declares it here.

use vike_hub_protocol::refusal::HubRefusal;
use vike_utility::errors::FailureOutcome;

vike_utility::refusal_enum! {
    /// A node refusal a client decides on.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum TradehubRefusal with ids TradehubRefusalId {
        /// The node's door answers every verb while its engines are still starting
        /// (`crates/vike-tradehub/src/server/starting.rs`): nothing was done, ask again shortly. `progress` is where
        /// the start has got to.
        Starting { progress: String } => "tradehub.starting": "starting: {progress}",
        /// The node's core could not take a command because its ingest lane was FULL
        /// (`crates/vike-tradehub/src/server/control.rs`'s `ControlAcceptError::Busy`): nothing was executed, and
        /// the same command is taken shortly.
        Busy => "tradehub.busy": "core busy, retry",
    }
}

impl From<TradehubRefusal> for HubRefusal {
    fn from(refusal: TradehubRefusal) -> Self {
        HubRefusal::new(refusal.id().word(), &refusal)
    }
}

/// What a node's refusal FRAME says the refused verb DID, read by its id: [`FailureOutcome::NotNow`] for a node
/// still starting or a busy core (nothing done, asked again shortly); [`FailureOutcome::Refused`] for every other id,
/// one this build does not know included (the node answered, and its text is shown).
#[must_use]
pub fn frame_outcome(refusal: &HubRefusal) -> FailureOutcome {
    match TradehubRefusalId::from_word(&refusal.id) {
        Some(TradehubRefusalId::Starting | TradehubRefusalId::Busy) => FailureOutcome::NotNow,
        None => FailureOutcome::Refused,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{BUSY_ERROR, STARTING_ERROR_PREFIX, is_starting_error, starting_error};

    /// **The frame and the older text agree**: a starting node's frame carries the id a client decides on and the
    /// very text an older client is answered, which still begins with the prefix an older client reads.
    #[test]
    fn a_starting_frame_reads_not_now_by_its_id_and_keeps_the_older_text() {
        let frame = HubRefusal::from(TradehubRefusal::Starting { progress: "binance up".into() });
        assert_eq!(frame.id, "tradehub.starting");
        assert_eq!(frame.text, starting_error("binance up"));
        assert!(frame.text.starts_with(STARTING_ERROR_PREFIX) && is_starting_error(&frame.text));
        assert_eq!(frame_outcome(&frame), FailureOutcome::NotNow);
        let unknown =
            HubRefusal::new("tradehub.from_a_newer_node", "starting: words alone decide nothing");
        assert_eq!(frame_outcome(&unknown), FailureOutcome::Refused);
    }

    /// **A busy core's frame is NOT NOW by its id, in the bytes every node has sent**: an older client, answered
    /// the `Error` of the same text, still reads it by [`BUSY_ERROR`].
    #[test]
    fn a_busy_frame_reads_not_now_by_its_id_and_keeps_the_older_bytes() {
        let frame = HubRefusal::from(TradehubRefusal::Busy);
        assert_eq!(frame.id, "tradehub.busy");
        assert_eq!(frame.text, BUSY_ERROR);
        assert_eq!(frame_outcome(&frame), FailureOutcome::NotNow);
    }
}
