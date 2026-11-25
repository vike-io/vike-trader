//! **The desktop's door onto the server's symbol lists** (the Data Manager's DataSets): the data daemon's
//! `SymbolSets`, `SetSymbolSet` and `DeleteSymbolSet` verbs, each on its own short connection and its own thread, so
//! the frame never waits on a socket. The owner's ruling of 2026-11-12: the lists live in the server's settings
//! database, and the desktop's `storage/datasets.json` is read by nothing (no migration).
//!
//! The shell follows the catalog's dial ([`vike_datahub_client::dial::DatahubDial`], the active backend's datahub and
//! this desktop's device key) with [`SymbolSetsLink::follow`], drains [`SymbolSetsLink::poll`] once per frame and adopts what it
//! answers. Until a server answers, the desktop shows NO list, read-only (`vike_data::datasets::SymbolSetStore`'s
//! `Default`): no list is compiled in (the owner's ruling of 2026-11-13).
//!
//! ⚠ **A save or a delete is a CONTROL verb**, dialled under the WRITE scope with this desktop's device key
//! (`crate::backend::registry::datahub_dial_keys`, decision 0288): a keyed datahub serves it when the
//! device's role admits Write, and otherwise refuses it at the handshake, which is what [`SymbolSetsLink::poll`]
//! answers. A key-less loopback datahub serves it.

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};

use vike_data::datasets::{SymbolSet, SymbolSetStore};
use vike_datahub_client::DatahubClient;
use vike_datahub_client::dial::DatahubDial;
use vike_hub_protocol::auth::Scope;
use vike_utility::mutex::lock;

/// One answer of the server: every list after the act, or why there is none.
type DataManagerSymbolSetAnswer = Result<Vec<SymbolSet>, String>;

/// The verb a thread runs.
enum DataManagerSymbolSetAction {
    Read,
    Save(SymbolSet),
    Delete(String),
}

/// **The link**: the dial it last asked through, and the channel the answers come back on.
pub struct SymbolSetsLink {
    /// `(address, device key epoch)` of the dial the lists were last READ through: a new one asks again.
    followed: Mutex<Option<(String, u64)>>,
    tx: Sender<DataManagerSymbolSetAnswer>,
    rx: Mutex<Receiver<DataManagerSymbolSetAnswer>>,
}

impl Default for SymbolSetsLink {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { followed: Mutex::new(None), tx, rx: Mutex::new(rx) }
    }
}

impl SymbolSetsLink {
    /// **Follow the dial**: a dial this link has not read through asks the server for its lists, once. `None` (no
    /// datahub) asks nothing and keeps what the desktop shows. Cheap on the steady state: one lock and one compare.
    pub fn follow(&self, dial: Option<DatahubDial>) {
        let Some(dial) = dial else { return };
        let pair = (dial.address.clone(), dial.epoch);
        {
            let mut followed = lock(&self.followed);
            if followed.as_ref() == Some(&pair) {
                return;
            }
            *followed = Some(pair);
        }
        self.spawn(dial, DataManagerSymbolSetAction::Read);
    }

    /// **Save one list** through `dial`; the answer arrives through [`Self::poll`]. No dial is an answer in words.
    pub fn save(&self, dial: Option<DatahubDial>, set: SymbolSet) {
        self.act(dial, DataManagerSymbolSetAction::Save(set));
    }

    /// **Remove the list called `name`** through `dial`; as [`Self::save`].
    pub fn delete(&self, dial: Option<DatahubDial>, name: String) {
        self.act(dial, DataManagerSymbolSetAction::Delete(name));
    }

    /// **The newest answer, if one landed**: the [`SymbolSetStore`] to show (an empty table is an empty store, writable),
    /// or why nothing changed.
    pub fn poll(&self) -> Option<Result<SymbolSetStore, String>> {
        let rx = lock(&self.rx);
        let mut newest = None;
        while let Ok(answer) = rx.try_recv() {
            newest = Some(answer.map(SymbolSetStore::from_server));
        }
        newest
    }

    fn act(&self, dial: Option<DatahubDial>, act: DataManagerSymbolSetAction) {
        match dial {
            Some(dial) => self.spawn(dial, act),
            None => {
                let _ = self.tx.send(Err(vike_datahub_client::dial::NO_BACKEND_NOTE.to_string()));
            }
        }
    }

    fn spawn(&self, dial: DatahubDial, act: DataManagerSymbolSetAction) {
        let tx = self.tx.clone();
        let spawned = std::thread::Builder::new().name("ui-symbol-sets".into()).spawn(move || {
            let _ = tx.send(run(&dial, act));
        });
        if let Err(e) = spawned {
            let _ = self.tx.send(Err(format!("the symbol lists could not be asked for: {e}")));
        }
    }
}

/// One verb on its own connection: a read under the observe scope, a write under the control scope.
fn run(dial: &DatahubDial, act: DataManagerSymbolSetAction) -> DataManagerSymbolSetAnswer {
    let scope =
        if matches!(act, DataManagerSymbolSetAction::Read) { Scope::Read } else { Scope::Write };
    let mut client = DatahubClient::connect_with_keys(&dial.address, dial.keys.as_ref(), scope)
        .map_err(|e| format!("the datahub at {} could not be reached: {e}", dial.address))?;
    match act {
        DataManagerSymbolSetAction::Read => client.symbol_sets(),
        DataManagerSymbolSetAction::Save(set) => client.set_symbol_set(set),
        DataManagerSymbolSetAction::Delete(name) => client.delete_symbol_set(&name),
    }
}

#[path = "symbol_sets_tests.rs"]
#[cfg(test)]
mod symbol_sets_tests;
