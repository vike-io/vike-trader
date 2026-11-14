//! **The answer SHAPE for [`Request::GatewayLinks`](crate::proto::Request::GatewayLinks)**: the venue sessions the gatewayhub
//! recorded, one row per session of every venue (`vike_secrets::link_sessions` holds the table and its contract).
//!
//! The datahub answers from the settings store it already opens and dials nothing, so the history reads with the gatewayhub down,
//! which is when it is wanted. A surface still links no gatewayhub client (decision 0253).
//!
//! A row is [`vike_model::LinkSessionRow`], the type the store reads (`vike_secrets::link_sessions`), with no copy between:
//! this crate's default build names no `vike-secrets`, and both name `vike-model`. Words and numbers, never a URL and never a
//! key: the store's one door to `how` saw to that, and that type's test pins the fields an Observe key may read.

use serde::{Deserialize, Serialize};
use vike_model::LinkSessionRow;

/// What a read of the history answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewayLinksReply {
    /// The sessions, newest first.
    pub rows: Vec<LinkSessionRow>,
    /// The read stopped at its row limit, so an older session asked for is not among them.
    #[serde(default)]
    pub truncated: bool,
}
