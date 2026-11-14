//! **The desktop's door onto the server's symbol lists** (the Data Manager's DataSets): the data daemon's
//! `SymbolSets`, `SetSymbolSet` and `DeleteSymbolSet` verbs, each on its own short connection and its own thread, so
//! the frame never waits on a socket. The owner's ruling of 2026-11-12: the lists live in the server's settings
//! database, and the desktop's `storage/datasets.json` is read by nothing (no migration).
//!
//! The shell follows the catalog's dial ([`crate::data::catalog_wire::CatalogDial`], the active backend's datahub and
//! observe key) with [`SymbolSetsLink::follow`], drains [`SymbolSetsLink::poll`] once per frame and adopts what it
//! answers. Until a server answers, the desktop shows NO list, read-only (`vike_data::datasets::SymbolSetStore`'s
//! `Default`): no list is compiled in (the owner's ruling of 2026-11-13).
//!
//! ⚠ **A save or a delete is a CONTROL verb**, dialled under the WRITE scope. Since decision 0280 the dial carries the
//! datahub control key beside the platform observe name (`crate::backend::backend_registry::datahub_observe_keys`),
//! so a keyed datahub serves it; a record with a CUSTOM observe name has no control twin, the handshake refuses its
//! write and the refusal is what [`SymbolSetsLink::poll`] answers. A key-less loopback datahub serves it.

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};

use vike_data::datasets::{SymbolSet, SymbolSetStore};
use vike_datahub_client::DatahubClient;
use vike_hub_protocol::auth::Scope;

use crate::data::catalog_wire::CatalogDial;
use crate::lock;

/// One answer of the server: every list after the act, or why there is none.
type Answer = Result<Vec<SymbolSet>, String>;

/// The verb a thread runs.
enum Act {
    Read,
    Save(SymbolSet),
    Delete(String),
}

/// **The link**: the dial it last asked through, and the channel the answers come back on.
pub struct SymbolSetsLink {
    /// `(address, key name)` of the dial the lists were last READ through: a new pair asks again.
    followed: Mutex<Option<(String, String)>>,
    tx: Sender<Answer>,
    rx: Mutex<Receiver<Answer>>,
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
    pub fn follow(&self, dial: Option<CatalogDial>) {
        let Some(dial) = dial else { return };
        let pair = (dial.addr.clone(), dial.key_name.clone());
        {
            let mut followed = lock(&self.followed);
            if followed.as_ref() == Some(&pair) {
                return;
            }
            *followed = Some(pair);
        }
        self.spawn(dial, Act::Read);
    }

    /// **Save one list** through `dial`; the answer arrives through [`Self::poll`]. No dial is an answer in words.
    pub fn save(&self, dial: Option<CatalogDial>, set: SymbolSet) {
        self.act(dial, Act::Save(set));
    }

    /// **Remove the list called `name`** through `dial`; as [`Self::save`].
    pub fn delete(&self, dial: Option<CatalogDial>, name: String) {
        self.act(dial, Act::Delete(name));
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

    fn act(&self, dial: Option<CatalogDial>, act: Act) {
        match dial {
            Some(dial) => self.spawn(dial, act),
            None => {
                let _ = self.tx.send(Err(crate::data::catalog_wire::NO_BACKEND_NOTE.to_string()));
            }
        }
    }

    fn spawn(&self, dial: CatalogDial, act: Act) {
        let tx = self.tx.clone();
        let spawned = std::thread::Builder::new().name("app-symbol-sets".into()).spawn(move || {
            let _ = tx.send(run(&dial, act));
        });
        if let Err(e) = spawned {
            let _ = self.tx.send(Err(format!("the symbol lists could not be asked for: {e}")));
        }
    }
}

/// One verb on its own connection: a read under the observe scope, a write under the control scope.
fn run(dial: &CatalogDial, act: Act) -> Answer {
    let scope = if matches!(act, Act::Read) { Scope::Read } else { Scope::Write };
    let mut client = DatahubClient::connect_with_keys(&dial.addr, dial.keys.as_ref(), scope)
        .map_err(|e| format!("the datahub at {} could not be reached: {e}", dial.addr))?;
    match act {
        Act::Read => client.symbol_sets(),
        Act::Save(set) => client.set_symbol_set(set),
        Act::Delete(name) => client.delete_symbol_set(&name),
    }
}

#[path = "symbol_sets_tests.rs"]
#[cfg(test)]
mod symbol_sets_tests;
