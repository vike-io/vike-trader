//! **One archive-import day's refusal and bar count**, the words the store half
//! (`vike_backfill::archive_import`) mints and the datahub's archive verb carries
//! (`vike_datahub_client::archive`) unchanged: one type each, here because both crates name this one,
//! the client sits below the store half and cannot name it, and the store half names no wire type.
//!
//! The serde derives ARE the wire's: `{"class", "detail"}` and `{"interval", "rows"}`, field names and
//! order as the archive verb has always sent them. A field added here is a wire change.

use serde::{Deserialize, Serialize};

/// Why ONE archive day was refused (or not wholly stored): a stable CLASS token and an operator's
/// sentence.
///
/// ⚠ **A string class rather than an enum, because of layering:** the classes are each FORMAT's own
/// (`AmbiguousTimeBase`, `NotMonotonic` for a Dukascopy daily file), minted below every format
/// (`TooRecent`, `Overlapped`, `OutsideDay` by the store half) or by a lane and a decoder, so an enum
/// would make each new decoder refusal a wire change. `class` is a CamelCase token a client may match
/// on; `detail` says what was wrong, what was and was not written, and what to do. Neither carries
/// file bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveDayRefusal {
    /// The refusal's stable class token — `AmbiguousTimeBase`, `NotMonotonic`, `MixedLayout`…
    pub class: String,
    /// An operator's sentence: what was wrong, and whether a commit key was spent.
    pub detail: String,
}

impl ArchiveDayRefusal {
    /// A refusal of `class` saying `detail`.
    pub fn new(class: &str, detail: String) -> Self {
        Self { class: class.to_string(), detail }
    }
}

/// How many bars ONE interval wrote for one day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveDayBars {
    /// The interval, as the request named it.
    pub interval: String,
    /// Bars written — `0` when the day's bars at this interval were already stored.
    pub rows: u64,
}
