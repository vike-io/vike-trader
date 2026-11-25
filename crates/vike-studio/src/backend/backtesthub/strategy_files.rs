//! The strategy files the backtest server serves (the owner's ruling of 2026-11-13: the backtesthub
//! manages them): the Strategy pane's list, Open and Save, and a Build of the editor buffer, each a
//! dial on a worker thread. The list and Open are Read dials ([`connect_observe`]); Save and Build
//! write code, so they sign with this PC's device key at the Write scope ([`connect`]).

use std::sync::mpsc::Receiver;

use vike_backtesthub_client::proto::WireStrategyFile;
use vike_hub_protocol::auth::HubKeys;
use vike_studio_core::spawn_outcome;

use super::{build, connect, connect_observe};

/// The backtest server's `strategies/` folder with each file's states, on a worker — the Strategy
/// pane's list. An OBSERVE dial ([`connect_observe`]): the verb is `VerbScope::Read`.
pub(crate) fn spawn_backtesthub_strategy_files(
    address: String,
    keys: Option<HubKeys>,
) -> Receiver<Result<Vec<WireStrategyFile>, String>> {
    spawn_outcome(move || {
        connect_observe(&address, keys.as_ref()).map_err(|e| e.to_string())?.list_strategy_files()
    })
}

/// One file's text off the backtest server's `strategies/` folder, on a worker — the Strategy
/// pane's Open. An OBSERVE dial, like [`spawn_backtesthub_strategy_files`].
pub(crate) fn spawn_backtesthub_strategy_file_read(
    address: String,
    keys: Option<HubKeys>,
    file: String,
) -> Receiver<Result<String, String>> {
    spawn_outcome(move || {
        connect_observe(&address, keys.as_ref())
            .map_err(|e| e.to_string())?
            .read_strategy_file(&file)
    })
}

/// Write one strategy file into the backtest server's `strategies/` folder, on a worker — the
/// Strategy pane's Save (the owner's ruling of 2026-11-13: the backtesthub manages the strategy
/// files). A CONTROL dial with this PC's device key ([`connect`]): a save writes code, and the
/// server refuses it on any other connection.
pub(crate) fn spawn_backtesthub_strategy_file_save(
    address: String,
    key: Option<HubKeys>,
    file: String,
    text: String,
) -> Receiver<Result<(String, WireStrategyFile), String>> {
    spawn_outcome(move || {
        connect(&address, key.as_ref()).map_err(|e| e.to_string())?.save_strategy_file(&file, &text)
    })
}

/// **A Build, through the backtesthub, on a worker** (`BuildStrategyFile`; decision 0288's amendment of
/// 2026-11-16: the strategy builder is reached by the server's daemons alone, so Studio holds no builder key):
/// the receiver yields the sha256 of `text` built as the file `file` — the sha that unblocks Run — or the
/// refusal as one sentence (a compile failure is rustc's diagnostics VERBATIM). The wait is a compile, so it is
/// legitimately minutes. The pre-dial refusals are `build`'s. Signed with
/// this PC's device key (`connect`): the daemon takes a Build from a Control connection only. `sdk` is the
/// toolbar's SDK pick (`None`: as the file says), so a Build compiles against what a Run will.
pub fn spawn_backtesthub_build(
    address: String,
    key: Option<HubKeys>,
    file: String,
    text: String,
    sdk: Option<u32>,
) -> Receiver<Result<String, String>> {
    spawn_outcome(move || {
        if let Some(why) = build::refusal(&file, &text) {
            return Err(why);
        }
        connect(&address, key.as_ref()).map_err(|e| e.to_string())?.build_strategy_file(
            file.trim(),
            &text,
            sdk,
        )
    })
}
