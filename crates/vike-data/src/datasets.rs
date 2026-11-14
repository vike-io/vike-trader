//! DataSets — named symbol collections (WealthLab's first-class concept), ported from vike's
//! `data/datasets.py`. A DataSet bundles symbols + a default provider/interval/benchmark so the
//! Data Manager can edit/test a whole universe.
//!
//! **Where they live** (the owner's ruling of 2026-11-12): in the SERVER's settings database, table
//! `symbol_set`, read and written through the data daemon's `SymbolSets` / `SetSymbolSet` /
//! `DeleteSymbolSet` verbs (`vike_datahub_client::proto::Request`). [`SymbolSetStore`] is the desktop's
//! in-memory copy of what the server answered, and it touches no file: the old
//! `storage/datasets.json` is read by nothing and a leftover copy is ignored (no migration).
//!
//! **No list is compiled in** (the owner's ruling of 2026-11-13, "remove all hardcoded symbols"): an
//! EMPTY answer is an empty store, and the Data Manager says how to start one. A desktop no server has
//! answered holds the same empty store READ-ONLY (`SymbolSetStore::default`, [`SymbolSetStore::writable`]
//! `false`), because there is nowhere to save to.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolSet {
    pub name: String,
    pub symbols: Vec<String>,
    pub provider: String, // "Auto" | a venue id ([`provider_names_a_venue`])
    pub interval: String,
    pub benchmark: String,
    #[serde(default)]
    pub user: bool, // true = user-created (shows under "My DataSets")
}

/// The server's lists as the desktop shows them. `Default` is a desktop no server has answered: no
/// list, and read-only.
#[derive(Debug, Default, Clone)]
pub struct SymbolSetStore {
    pub sets: Vec<SymbolSet>,
    /// Whether a save or a delete can reach a server: `false` until one has answered, and the Data
    /// Manager then refuses both.
    pub writable: bool,
}

/// Whether a set's `provider` names a venue. `Auto` is the Data Manager form's sentinel for "any
/// venue" and NOT a venue, so it may never reach a backfill key or open a chart: the datahub would be
/// asked for a venue nobody has, and would answer with a refusal naming its own roster. A blank
/// provider names none either; the sentinel is case-blind.
#[must_use]
pub fn provider_names_a_venue(provider: &str) -> bool {
    let p = provider.trim();
    !p.is_empty() && !p.eq_ignore_ascii_case("Auto")
}

/// Split a free-text symbol blob (commas / whitespace / newlines) → upper, deduped, ordered.
pub fn parse_symbols(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for token in text.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
        let t = token.trim().to_uppercase();
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

impl SymbolSetStore {
    /// What a server answered: its lists, writable. An empty table is an empty store; saving a set
    /// stores it.
    #[must_use]
    pub fn from_server(sets: Vec<SymbolSet>) -> Self {
        SymbolSetStore { sets, writable: true }
    }

    pub fn get(&self, name: &str) -> Option<&SymbolSet> {
        self.sets.iter().find(|s| s.name == name)
    }

    /// A fresh untitled user DataSet name that doesn't collide.
    pub fn fresh_name(&self) -> String {
        let mut n = 1;
        loop {
            let name = if n == 1 { "New DataSet".to_string() } else { format!("New DataSet {n}") };
            if self.get(&name).is_none() {
                return name;
            }
            n += 1;
        }
    }
}

#[path = "datasets_tests.rs"]
#[cfg(test)]
mod datasets_tests;
