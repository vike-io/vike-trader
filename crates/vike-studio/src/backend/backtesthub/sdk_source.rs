//! The backtest server's strategy SDKs (decision 0296, verdict 3): the SDK source view's list of them
//! and one file or folder of one, each a dial on a worker thread. Both are Read dials
//! ([`connect_observe`]): they read shipped source and change nothing, as the strategy-file read does.
//! Each answer is the client's typed wire value (`crates/vike-backtesthub-client/src/sdk_source.rs`); an
//! older server's refusal is the client's own sentence, nothing sent.

use std::sync::mpsc::Receiver;

use vike_backtesthub_client::sdk_source::{WireSdkFile, WireSdkListing};
use vike_hub_protocol::auth::HubKeys;
use vike_studio_core::spawn_outcome;

use super::connect_observe;

/// The server's SDKs, newest first, on a worker. An OBSERVE dial.
pub(crate) fn spawn_backtesthub_sdks(
    address: String,
    keys: Option<HubKeys>,
) -> Receiver<Result<WireSdkListing, String>> {
    spawn_outcome(move || {
        connect_observe(&address, keys.as_ref()).map_err(|e| e.to_string())?.list_sdks()
    })
}

/// The file or folder `path` of the server's SDK `sdk`, on a worker. An OBSERVE dial.
pub(crate) fn spawn_backtesthub_sdk_read(
    address: String,
    keys: Option<HubKeys>,
    sdk: String,
    path: String,
) -> Receiver<Result<WireSdkFile, String>> {
    spawn_outcome(move || {
        connect_observe(&address, keys.as_ref())
            .map_err(|e| e.to_string())?
            .read_sdk_file(&sdk, &path)
    })
}
