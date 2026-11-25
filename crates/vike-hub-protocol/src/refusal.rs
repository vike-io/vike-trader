//! The hubs' refusal FRAME (the owner's Q8 of 2026-11-23): `{id, text}`, the one shape a hub answers a refusal it
//! DECLARED with an id in, on each hub's wire as `Response::Refused` (the datahub's, the tradehub's and the
//! gatewayhub's protocols embed it), instead of the bare `Response::Error(String)`.
//!
//! The id is what a client decides on (a type, never the text: decision 0301); the text is for a person, byte for
//! byte what the `Error` carried. The ids a client DECIDES on are declared once, by `vike_utility::refusal_enum!`, in
//! the client crate of the hub that raises them (`refusals.rs` of `vike-tradehub-client`, `vike-datahub-client` and
//! `vike-gatewayhub-client`: the lowest crate the hub and its readers both link), and read back with the id enum's
//! `from_word` (`None`: an id this build does not know, read by its text alone).
//!
//! ⚠ **A hub sends the frame only to a client that said it reads it** (`crate::features::FEATURE_REFUSAL_FRAME`, the
//! client's `crate::handshake::HandshakeFrame::Reads`): an older client cannot decode a `Refused` variant, so each
//! hub turns it back into the `Error` text it always was at the one place it writes a reply. Every refusal without
//! an id keeps `Response::Error`; refusal step 3 moves each into its hub's `refusals.rs`, and no wire change is
//! needed then, since the frame carries any id as a string.

use std::fmt;

use serde::{Deserialize, Serialize};

/// One refusal a hub declared: its stable id and the text a person reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubRefusal {
    /// The declared id (`tradehub.starting`): the id enum's `word()` on the hub's side, its `from_word` on a
    /// client's.
    pub id: String,
    /// The refusal's text, for a person: what the `Response::Error` carried.
    pub text: String,
}

impl HubRefusal {
    /// The frame of a refusal declared with `id`, worded `text`.
    pub fn new(id: &str, text: impl fmt::Display) -> Self {
        HubRefusal { id: id.to_string(), text: text.to_string() }
    }
}

/// The text alone: what a person reads, as the `Error` it replaces read.
impl fmt::Display for HubRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// A frame travels inside an `io::Error` too (an opener's refused open), so a reader can find its id by type.
impl std::error::Error for HubRefusal {}

/// A frame always has its id: what a carrier holding one shows (`vike_utility::errors`'s "A carrier shows ONE id").
impl vike_utility::errors::Declared for HubRefusal {
    fn declared_id(&self) -> Option<&str> {
        Some(&self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::HubRefusal;

    /// The frame's bytes ARE the wire: `{id, text}` under each hub's `Refused` tag.
    #[test]
    fn the_frame_is_pinned_to_its_json_bytes() {
        let frame = HubRefusal::new("tradehub.starting", "starting: binance up");
        let bytes = r#"{"id":"tradehub.starting","text":"starting: binance up"}"#;
        assert_eq!(serde_json::to_string(&frame).unwrap(), bytes);
        assert_eq!(serde_json::from_str::<HubRefusal>(bytes).unwrap(), frame);
        assert_eq!(frame.to_string(), "starting: binance up");
    }
}
