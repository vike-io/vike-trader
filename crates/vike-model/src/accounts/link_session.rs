//! One recorded venue SESSION of one socket, as a reader of the history gets it: the row
//! `vike_secrets::link_sessions` reads out of the settings database's `link_session` table, and the
//! row the datahub's `GatewayLinks` answer (`vike_datahub_client::gateway_links::GatewayLinksReply`)
//! carries to an operator. ONE type for both since 2026-11-11, where the wire held a field-for-field
//! copy with the same bytes.
//!
//! ⚠ **Every field here crosses an Observe-scope wire.** A field added to this row reaches every
//! reader of the history the day it is added, so only words and numbers may join it: never a URL and
//! never a key. The one free-text column, `how`, enters the store through
//! `vike_secrets::link_sessions::plain_words` alone. The compiler no longer stops a new store column
//! at the wire (the datahub destructured the store row whole into its copy until the two became
//! one); the test beside this module does, by the owner's ruling of 2026-11-11: it serialises a row
//! with every field set and pins the exact key set, so a new field fails there until somebody
//! decides it may be read under an Observe key.

use serde::{Deserialize, Serialize};

/// One venue session of one socket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkSessionRow {
    /// The venue's roster word.
    pub venue: String,
    /// The account's route key (`bybit#DEMO`); on the price plane, the symbol.
    pub route: String,
    /// `trade` or `price` (`vike_secrets::link_sessions::PLANE_TRADE` / `PLANE_PRICE`), as written.
    pub plane: String,
    /// Which socket of that route.
    pub lane: String,
    /// When it opened, epoch-ms.
    pub opened_ms: i64,
    /// When it closed, epoch-ms; `None` while it is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_ms: Option<i64>,
    /// Seconds it lived; `None` while it is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lived_s: Option<f64>,
    /// How it ended, in plain words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub how: Option<String>,
    /// We ended it (a stop, a restart), so it is no drop.
    #[serde(default)]
    pub ours: bool,
    /// Seconds until the next session of this socket opened; `None` until one has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_open_s: Option<f64>,
}

#[cfg(test)]
#[path = "link_session_tests.rs"]
mod link_session_tests;
