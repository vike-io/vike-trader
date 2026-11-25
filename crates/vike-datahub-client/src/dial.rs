//! `dial` — **where the backend's datahub is, and what a client signs with**: the ONE dial the
//! desktop's datahub clients take (the catalog refresh and the symbol sets in `vike-app-core`, and
//! the News, Calendar and Options poll in `crate::tool_feeds::poll`), so they can never sign with
//! different keys.
//!
//! A plain value: only a BINARY may read the process environment or the credential store, so the
//! composition root resolves it (`vike-app-core`'s `data::datahub::dial::resolve_datahub_dial`)
//! and hands the whole value down.

use vike_hub_protocol::auth::HubKeys;

/// **What a client says when there is no datahub to ask** — the ONE spelling of it, shared by the
/// catalog rows' Status cell, the disabled refresh button's hover and every poll's status, so they
/// cannot describe the same state differently. It names BOTH fixes, because either one works and a
/// sentence naming one would send half the operators down a path their box does not use.
pub const NO_BACKEND_NOTE: &str = "no backend is connected and `config.datahub_addr` is unset, so there is no datahub to ask — \
     connect a backend that advertises one, or set that key.";

/// **Where a datahub client dials, and what it signs with** — the `vike-app-core`
/// `data::stored::chart_seed::ChartSeedDial` twin, and a value for the identical reason: only a BINARY may read
/// the process environment or the credential store, so the composition root resolves this once and
/// hands the whole value down.
///
/// ⚠ A dial is identified by its address AND [`DatahubDial::epoch`]: the address is a property of
/// the ACTIVE BACKEND RECORD, which a switch replaces, and the epoch moves when this PC's device key
/// does (a key-NAME half left with decision 0288's cleanup: no datahub key is named any more).
#[derive(Clone, Debug)]
pub struct DatahubDial {
    /// The resolved datahub address — `vike-app-core`'s `resolve_datahub_address` answer.
    pub address: String,
    /// What the desktop signs its datahub dials with: this PC's device key (decision 0288; no shared
    /// datahub key exists). `None` dials unauthenticated, which fails at the handshake against a
    /// served hub (every one authenticates), naming what is missing.
    pub keys: Option<HubKeys>,
    /// `vike-app-core`'s `device_key_epoch` as it stood before `keys` was resolved: a dial resolved
    /// before this desktop held its device key is stale once the key exists.
    pub epoch: u64,
}
