//! The credential-PRESENCE word: whether the credential a venue's credentialed lane reads is stored
//! on the gatewayhub's box. A word, never a value.
//!
//! Account vocabulary, so it lives with the accounts: the venue bridges that probe a store
//! (`vike_secrets`' presence function maps the store's answers onto it), the gatewayhub that answers
//! it on its wire and the datahub that forwards it unchanged into its history-channels report all name
//! this one type, with one set of variants, one set of serde tags and one phrase.
//!
//! ⚠ **The variant NAMES are the wire** of both hub protocols (serde's default tags): the variants,
//! their order and their names are byte-for-byte the type's they replace, and a rename here is a wire
//! change.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire tags are the variant names, in declaration order, as the type this one replaces wrote
    /// them: a JSON round trip of every word, and the tags spelled out.
    #[test]
    fn the_wire_tags_are_the_variant_names() {
        let tags: Vec<String> = CredentialPresence::ALL
            .iter()
            .map(|p| serde_json::to_string(p).expect("a unit variant serializes"))
            .collect();
        assert_eq!(
            tags,
            [r#""NotNeeded""#, r#""Present""#, r#""Absent""#, r#""Unreadable""#, r#""NotChecked""#]
        );
        for p in CredentialPresence::ALL {
            let back: CredentialPresence =
                serde_json::from_str(&serde_json::to_string(&p).expect("serializes"))
                    .expect("reads back");
            assert_eq!(back, p);
        }
    }

    /// `Unreadable` never reads as `Absent` to an operator: the two phrases differ, and every word has
    /// one.
    #[test]
    fn unreadable_is_never_phrased_as_absent() {
        assert_ne!(CredentialPresence::Unreadable.phrase(), CredentialPresence::Absent.phrase());
        assert!(CredentialPresence::ALL.iter().all(|p| !p.phrase().is_empty()));
    }
}
