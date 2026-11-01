//! The reconcile fill floor between two runs of the node: where the next client starts reading fills.
//!
//! # Why it exists
//!
//! [`crate::recon_client::OandaReconClient`] reads an account's fills from a transaction-id floor, `sinceid`. At
//! connect that floor was the account's `lastTransactionID` AS IT IS THEN, so a fill that happened while the node
//! was down lay below it: measured 2026-10-26 (a stop fired 35 s into a 150 s restart), the first pass found a held
//! `PositionDrift` and no row that named the order. The venue's transaction ids are monotonic per account, so the
//! floor the LAST run had reached is a point the next run can start from instead.
//!
//! # The contract
//!
//! - **What is kept is the client's own floor**, the id its last successful fill pass read up to
//!   (`OandaReconClient::fetch_fill_reports` writes it when it moves). Not the exec side's stream watermark: that
//!   one jumps to the account's latest transaction at the first order of a run, before any pass has read the gap, and
//!   a floor written from it would skip the very fill this file exists to find. A floor only ever stands for fills a
//!   pass has REPORTED, so a stale file reads a little too much (the reconcile engine's `trade_id` dedup, restored
//!   across a restart, absorbs a fill the last run folded live) and never too little.
//! - **One small JSON file per account** under the declared state directory (`ProcessFacts::state_dir`, which the
//!   boot resolved: never beside `vike.db`, never a setting), `oanda-fill-floor-<hash>.json` holding
//!   `{"floor":<id>}`. The name carries a stable hash of the account id (`vike_util::fnv`), never the id. It is
//!   written whole and atomically (`vike_util::atomic_file::write_atomic`, through
//!   `vike_paths::state_path::write_path`, which refuses a symlinked target), so a crash leaves the old floor or the
//!   new one.
//! - **Nothing persisted is today's behaviour.** A first start (no file), a start handed no state directory, a file
//!   that exists and cannot be read (an `error!`, and the file is never touched), a floor ABOVE the account's latest
//!   transaction (an account that was reset) and a floor more than [`MAX_LOOKBACK`] transactions behind it all start
//!   at the account's latest transaction, as before this module. The bound keeps the first `sinceid` request small
//!   and the window the reconcile engine must absorb short; OANDA's cap on one request is not measured.
//! - **Best effort.** A write that fails is said once at `warn` and the run goes on with the floor in memory.
//! - Two clients of one account (two mounted symbols) share the file; the later write wins. A client then starts at
//!   worst where it would have started before this module, never above the account's latest transaction.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;

/// The most transactions a persisted floor may lie behind the account's latest one and still be used.
pub(crate) const MAX_LOOKBACK: u64 = 500;

/// Where one account's floor is kept.
pub(crate) struct FloorFile {
    dir: PathBuf,
    name: String,
    /// A failed write is said once, not at every pass.
    warned: AtomicBool,
}

impl FloorFile {
    pub(crate) fn new(state_dir: &Path, account_id: &str) -> Self {
        Self {
            dir: state_dir.to_path_buf(),
            name: format!(
                "oanda-fill-floor-{}.json",
                vike_util::fnv::fnv1a64_hex(account_id.as_bytes())
            ),
            warned: AtomicBool::new(false),
        }
    }

    /// The floor the last run left: `None` for a first start (no file, silent) and for a file that cannot be
    /// read or parsed (an `error!`: a store that exists and cannot be read is not "nothing persisted", and it is
    /// never touched here).
    pub(crate) fn read(&self) -> Option<u64> {
        let path = self.dir.join(&self.name);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                tracing::error!(
                    venue = "oanda", file = %path.display(), error = %e,
                    "oanda: the persisted reconcile fill floor exists and cannot be read; starting at the \
                     account's latest transaction, and the file is left as it is"
                );
                return None;
            }
        };
        let floor = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("floor").and_then(serde_json::Value::as_u64));
        if floor.is_none() {
            tracing::error!(
                venue = "oanda", file = %path.display(),
                "oanda: the persisted reconcile fill floor is not `{{\"floor\":<id>}}`; starting at the \
                 account's latest transaction, and the file is left as it is"
            );
        }
        floor
    }

    /// Keep `floor` for the next run. Best effort (the module doc).
    pub(crate) fn write(&self, floor: u64) {
        let body = json!({ "floor": floor }).to_string();
        let written = vike_paths::state_path::write_path(Some(self.dir.as_path()), &self.name)
            .and_then(|path| vike_util::atomic_file::write_atomic(&path, body.as_bytes()));
        if let Err(e) = written
            && !self.warned.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(
                venue = "oanda", dir = %self.dir.display(), error = %e,
                "oanda: could not keep the reconcile fill floor for the next start; a fill that happens \
                 while the node is down will not name its order after it (said once)"
            );
        }
    }
}

/// The floor a client starts from, given what the last run left (`persisted`) and the account's latest
/// transaction id now (`latest`): the persisted one when it is usable, else `latest` (the module doc's list).
pub(crate) fn start_floor(persisted: Option<u64>, latest: u64) -> u64 {
    match persisted {
        None => latest,
        Some(floor) if floor > latest => {
            tracing::warn!(
                venue = "oanda",
                floor,
                latest,
                "oanda: the persisted fill floor is above the account's latest transaction (an account that \
                 was reset?); starting at the latest"
            );
            latest
        }
        Some(floor) if latest - floor > MAX_LOOKBACK => {
            tracing::warn!(
                venue = "oanda",
                floor,
                latest,
                max = MAX_LOOKBACK,
                "oanda: the persisted fill floor lies too far behind the account's latest transaction to \
                 read the gap in one request; fills made while the node was down are not named, starting at \
                 the latest"
            );
            latest
        }
        Some(floor) => {
            if floor < latest {
                tracing::info!(
                    venue = "oanda",
                    floor,
                    latest,
                    "oanda: reconcile reads fills from the floor the last run left, so a fill made while the \
                     node was down names its order"
                );
            }
            floor
        }
    }
}

#[path = "fill_floor_tests.rs"]
#[cfg(test)]
mod fill_floor_tests;
