//! `Told`: the symbols this lease has TOLD the daemon's reconcile client the engine admitted
//! (decisions 0173 and 0174).
//!
//! The daemon tells its reconcile client (`ReconClient::admit_symbol`) on each GRANTED COMMIT
//! (`TradeSession::admit_symbol`, a `TradeUp::SymbolFacts` with `probe` false), the only signal it gets for an admission; a look (`symbol_facts`, a probe) tells it nothing. The
//! restore planner reads an order a pass's reports lack as gone, so the node must know exactly which
//! symbols that client reads: this is the node's mirror of it.
//!
//! * **Only a granted commit is recorded**, in arrival order, de-duplicated and bounded by the
//!   engine's own cap (`MAX_EXTRA_SYMBOLS`) as the daemon's list is, so it never claims a symbol the
//!   daemon ignored. A refused symbol is not recorded.
//! * **A new daemon knows nothing.** [`Told::forget_all`] (the link calls it when a reconnect finds
//!   another `gw_epoch`) empties the claim and keeps what was told as OWED, so the re-teaching
//!   worker can ask again ([`Told::owed`]); a symbol granted again leaves the owed list.
//! * Never held across I/O: every method is a short critical section.

use std::sync::{Mutex, PoisonError};

use vike_exec::MAX_EXTRA_SYMBOLS;

#[derive(Default)]
struct Lists {
    /// Told to the CURRENT daemon, in arrival order.
    current: Vec<String>,
    /// Told to a previous daemon and not granted again since, oldest first.
    owed: Vec<String>,
}

/// See the module doc.
#[derive(Default)]
pub(crate) struct Told(Mutex<Lists>);

impl Told {
    fn lists(&self) -> std::sync::MutexGuard<'_, Lists> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The daemon granted `symbol`'s facts (and so told its reconcile client).
    pub(crate) fn grant(&self, symbol: &str) {
        let mut l = self.lists();
        l.owed.retain(|s| s != symbol);
        if l.current.len() < MAX_EXTRA_SYMBOLS && !l.current.iter().any(|s| s == symbol) {
            l.current.push(symbol.to_string());
        }
    }

    /// A new daemon: nothing is told any more, and what was is owed.
    pub(crate) fn forget_all(&self) {
        let mut l = self.lists();
        let gone = std::mem::take(&mut l.current);
        for s in gone {
            if !l.owed.contains(&s) {
                l.owed.push(s);
            }
        }
        l.owed.truncate(MAX_EXTRA_SYMBOLS);
    }

    /// What the current daemon's reconcile client was told.
    pub(crate) fn current(&self) -> Vec<String> {
        self.lists().current.clone()
    }

    /// Whether the current daemon's reconcile client was told `symbol`.
    pub(crate) fn has(&self, symbol: &str) -> bool {
        self.lists().current.iter().any(|s| s == symbol)
    }

    /// What a previous daemon was told and the current one has not been.
    pub(crate) fn owed(&self) -> Vec<String> {
        self.lists().owed.clone()
    }
}
