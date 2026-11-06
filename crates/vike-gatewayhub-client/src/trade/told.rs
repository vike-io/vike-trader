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
//! * **The daemon's own word beats the mirror** (`gw_trade_recon_read`): a daemon that names the symbols its
//!   client READ in a reconcile reply ([`Told::name`]) is the authority on what that client reads, and
//!   [`Told::readable`] claims only the told symbols it names. The mirror alone can claim a symbol the daemon
//!   ignored (two commits racing at the last slot, an adapter restarted inside one daemon session so that its
//!   new client was told nothing); the named list can never widen the claim, because only a symbol this lease
//!   told is ever claimed, in the spelling the engine holds it by.
//! * Never held across I/O: every method is a short critical section.

use std::sync::{Mutex, PoisonError};

use vike_exec::MAX_EXTRA_SYMBOLS;

#[derive(Default)]
struct Lists {
    /// Told to the CURRENT daemon, in arrival order.
    current: Vec<String>,
    /// Told to a previous daemon and not granted again since, oldest first.
    owed: Vec<String>,
    /// What the CURRENT daemon's last list-kind reconcile reply said its client reads (`None` = it has not
    /// said, or says nothing for this venue).
    named: Option<Vec<String>>,
    /// The told symbols the daemon did not name, as last reported (so a standing difference is said once).
    unnamed_said: Vec<String>,
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

    /// The daemon FORGOT `symbol` (the engine gave it back, decision 0205): it is neither told nor owed any more,
    /// and its place under the cap is free for the next symbol granted.
    pub(crate) fn forget(&self, symbol: &str) {
        let mut l = self.lists();
        l.current.retain(|s| s != symbol);
        l.owed.retain(|s| s != symbol);
    }

    /// A new daemon: nothing is told any more, and what was is owed. What the old one named is no word of
    /// the new one's.
    pub(crate) fn forget_all(&self) {
        let mut l = self.lists();
        l.named = None;
        l.unnamed_said.clear();
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

    /// **The daemon named the symbols its client read** in the list-kind reply it just sent (`None`: that
    /// reply named nothing, so the daemon's word is not known, which is the mirror alone).
    pub(crate) fn name(&self, read: Option<Vec<String>>) {
        self.lists().named = read;
    }

    /// **What the lease may claim its daemon's client reads**: the told symbols, narrowed to the ones the
    /// daemon named in its last list-kind reply when it named a list (matched without case: the client
    /// upper-cases what it keeps, the engine spells its symbol as the venue does and that spelling is what
    /// the claim carries). The second half is the told symbols the daemon did NOT name, when that set
    /// differs from the one last reported (`Some(empty)` = they are named again): the caller says it once.
    pub(crate) fn readable(&self) -> (Vec<String>, Option<Vec<String>>) {
        let mut l = self.lists();
        let Some(named) = l.named.clone() else {
            return (l.current.clone(), None);
        };
        let (read, unnamed): (Vec<String>, Vec<String>) = l
            .current
            .iter()
            .cloned()
            .partition(|s| named.iter().any(|n| n.eq_ignore_ascii_case(s)));
        let changed = (unnamed != l.unnamed_said).then(|| unnamed.clone());
        l.unnamed_said = unnamed;
        (read, changed)
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
