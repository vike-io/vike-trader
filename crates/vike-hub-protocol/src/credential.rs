//! The credential-PRESENCE word: whether the credential a venue's credentialed lane reads is stored
//! on the gatewayhub's box. A word, never a value.
//!
//! ONE type for both hub protocols. The gatewayhub answers it (`vike-gatewayhub-client`'s
//! `Response::PresenceIs`), and the datahub forwards it unchanged into its history-channels report
//! (`vike-datahub-client`'s `HistoryChannelWire::credential`), which the CLI and the GUI render with
//! [`CredentialPresence::phrase`]. The two clients sit at one layer and neither may name the other,
//! so the word lives here, below both: one set of variants, one set of serde tags, one phrase.
//!
//! ⚠ **The variant NAMES are the wire** of both protocols (serde's default tags); the gatewayhub's
//! goldens pin them (`crates/vike-gatewayhub-client/tests/wire_tag_fixtures.rs`'s `WIRE_ENUMS`).
//!
//! ⚠ **`Unreadable` is never collapsed into `Absent`**
//! (`docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md`,
//! verdict 6): reported as "not stored", it sends the operator to store a key they already stored.
//! And an UNREACHABLE gatewayhub answers [`CredentialPresence::NotChecked`] (nothing was read), never
//! `Absent`.

use serde::{Deserialize, Serialize};

/// **Whether the credential a lane reads is STORED on the gatewayhub's box**: a presence word,
/// never a value. An Observe key may learn this (the history-channels design's Q1): the daemons'
/// startup logs already print it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialPresence {
    /// No lane needs a credential for this venue or row: a keyless lane, or a venue no lane serves.
    NotNeeded,
    /// The credential is stored, with a non-blank value.
    Present,
    /// The store answered and does not hold it (or holds it blank), or the daemon resolved no
    /// settings directory and so has no store to hold it.
    Absent,
    /// A store exists and could not be read, so whether it is stored cannot be told.
    Unreadable,
    /// Nothing was read: the venue needs a credential and nothing holds a way to look (no lane
    /// mounted for it, a lane mounted without a probe), or the gatewayhub could not be asked. Not
    /// `Absent`, and the answer every row of a client's own compiled table carries.
    NotChecked,
}

impl CredentialPresence {
    /// Every word, in declaration order.
    pub const ALL: [CredentialPresence; 5] = [
        CredentialPresence::NotNeeded,
        CredentialPresence::Present,
        CredentialPresence::Absent,
        CredentialPresence::Unreadable,
        CredentialPresence::NotChecked,
    ];

    /// The words a reader is shown, after `credential:`.
    #[must_use]
    pub fn phrase(self) -> &'static str {
        match self {
            CredentialPresence::NotNeeded => "none needed",
            CredentialPresence::Present => "stored on the gatewayhub's box",
            CredentialPresence::Absent => "not stored on the gatewayhub's box",
            CredentialPresence::Unreadable => {
                "unknown — the gatewayhub's credential store could not be read (its log names why); \
                 this is not the same as the key being absent"
            }
            CredentialPresence::NotChecked => {
                "not checked — nothing was read to tell whether it is stored"
            }
        }
    }
}
