//! **The answer SHAPE for [`Request::GatewayStatus`](crate::proto::Request::GatewayStatus)**: what the
//! datahub says about the gatewayhub, the one way a remote client can ask with the tradehub down
//! (`docs/decisions/0252-the-cli-and-the-mcp-server-are-two-doors-over-one-client-and-every-hub-has-its-own-client.md`'s
//! verdict 6: the CLI and MCP link no gatewayhub client and see it through the hubs).
//!
//! A down gatewayhub is an ANSWER here, never an error and never an empty report:
//! [`GatewayStatusReply::reached`] is false and [`GatewayStatusReply::error`] holds the words
//! `vike_gatewayhub_client::client::gateway_text` spells, the one spelling an operator greps for.
//!
//! [`GatewayStatusReportWire`] mirrors `vike_gatewayhub_client::proto::GatewayStatusReport`, a crate at this one's layer, so it
//! cannot be named here. The server's mirror (`crates/vike-datahub/src/server/gateway_status.rs`'s `mirrored`) destructures
//! the report whole, so a new field there does not compile until somebody decides. The budget rows ARE carried
//! ([`GatewayStatusReportWire::budget`]), so a datahub-side reader gets every counter the daemon reports. ⚠ **No health
//! renderer shows them**: they are SHADOW counters of order commands only (`crates/vike-gatewayhub/src/budget/mode.rs`'s
//! `MODE`), and shown beside a daemon's health they would read as headroom.

use serde::{Deserialize, Serialize};

/// What the datahub could say about the gatewayhub just now: at most a second or two old.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayStatusReply {
    /// The gatewayhub completed the datahub's handshake.
    pub reached: bool,
    /// Why there is no [`report`](Self::report), as a complete `gatewayhub <addr>: ...` sentence.
    /// `None` exactly when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The daemon's own report. `None` with `reached` true is a daemon that took the handshake and gave
    /// none: a stalled one, or one older than `gateway_status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<GatewayStatusReportWire>,
}

/// The gatewayhub daemon's report: names and counters, never values. It says nothing of any venue:
/// `GatewayStatus` touches none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayStatusReportWire {
    /// The daemon's epoch: a new number is a restart.
    pub epoch: u64,
    /// When the daemon started, epoch-ms.
    pub started_ms: i64,
    /// Milliseconds since it started.
    pub uptime_ms: u64,
    /// Connections right now.
    pub connections: u32,
    /// Open `stream` sessions.
    pub stream_sessions: u32,
    /// Fetches running right now.
    pub fetches_running: u32,
    /// The market-data venues it serves, in advertisement order.
    #[serde(default)]
    pub market_data_venues: Vec<String>,
    /// The credentialed-data venues it serves, in advertisement order.
    #[serde(default)]
    pub credentialed_data_venues: Vec<String>,
    /// The daemon's budget counters (SHADOW: counted, nothing refused; the module doc). `None` is a datahub
    /// older than this field, which said nothing of them; `Some(empty)` is a daemon that reported none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<Vec<GatewayBudgetCounter>>,
}

/// One budget counter row, mirroring `vike_gatewayhub_client::proto::BudgetCounter` (same layer: not nameable here).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayBudgetCounter {
    /// The venue.
    pub venue: String,
    /// The host or market label the ceiling is keyed by.
    pub host: String,
    /// The budget scope (ip, account, key, connection).
    pub scope: String,
    /// The traffic class (order, live, interactive, bulk).
    pub class: String,
    /// Units used in the current window.
    pub used: u64,
    /// The published ceiling.
    pub cap: u64,
    /// The window length in milliseconds.
    pub window_ms: u64,
    /// How many requests WOULD have been throttled had enforcement been on.
    pub would_throttle: u64,
}
