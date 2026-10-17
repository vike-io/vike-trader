//! [`SymbolList`]: the other symbols an engine trades, shared between publishes.
//!
//! [`crate::VenueBlock::extra_symbols`] is the engine's `extra_symbols` verbatim, rebuilt into every
//! snapshot, and a publish runs per event on a sporadic feed (`vike_core::snapshot::build`, on the
//! latency-gated hop). A `Vec<String>` made that one allocation per symbol per publish, up to
//! [`crate::MAX_EXTRA_SYMBOLS`] of them (decision 0173). Held here it is one `Arc` the engine keeps
//! until its list changes (`ExecutionEngine::published_extra_symbols`), so a snapshot clone and a
//! publish copy a pointer.
//!
//! An engine that admitted nothing (every venue mount) holds NO `Arc` at all: the empty list is a
//! `None`, so cloning it, comparing it and publishing it cost what an empty `Vec` did.
//!
//! A reader changes nothing it did with the `Vec`: this dereferences to `[String]`, iterates by
//! reference, compares equal to a `Vec<String>`, and prints (`Debug`) as the slice does. What it is
//! not: growable. A reader that wants an owned list says `to_vec()`.

use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

/// An immutable, cheaply cloned list of symbols. See the module doc.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SymbolList(Option<Arc<[String]>>);

impl SymbolList {
    /// The symbols, in the engine's order.
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        self.0.as_deref().unwrap_or(&[])
    }

    /// Whether both lists are the SAME allocation, not merely equal: the proof that a publish shared
    /// the engine's list instead of copying it. Two empty lists share nothing and answer `false`.
    #[must_use]
    pub fn is_shared_with(&self, other: &Self) -> bool {
        matches!((&self.0, &other.0), (Some(a), Some(b)) if Arc::ptr_eq(a, b))
    }
}

impl Deref for SymbolList {
    type Target = [String];

    fn deref(&self) -> &[String] {
        self.as_slice()
    }
}

/// The slice's own `Debug`, so a block's `{:?}` is the text it was with a `Vec<String>`.
impl fmt::Debug for SymbolList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_slice(), f)
    }
}

impl From<Vec<String>> for SymbolList {
    fn from(symbols: Vec<String>) -> Self {
        SymbolList((!symbols.is_empty()).then(|| Arc::from(symbols)))
    }
}

impl FromIterator<String> for SymbolList {
    fn from_iter<I: IntoIterator<Item = String>>(symbols: I) -> Self {
        symbols.into_iter().collect::<Vec<_>>().into()
    }
}

impl<'a> IntoIterator for &'a SymbolList {
    type Item = &'a String;
    type IntoIter = std::slice::Iter<'a, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

impl PartialEq<Vec<String>> for SymbolList {
    fn eq(&self, other: &Vec<String>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// It reads as the `Vec<String>` it replaced: same iteration, same equality, same `{:?}`, and
    /// the empty list is the default and holds no allocation.
    #[test]
    fn it_reads_as_the_vec_it_replaced() {
        let none = SymbolList::default();
        assert!(none.is_empty());
        assert_eq!(none, Vec::<String>::new());
        assert_eq!(format!("{none:?}"), format!("{:?}", Vec::<String>::new()));
        assert!(none.0.is_none(), "the empty list holds no Arc");
        assert!(SymbolList::from(Vec::new()).0.is_none());

        let names = vec!["ETHUSDT".to_string(), "SOL\"USDT".to_string()];
        let list = SymbolList::from(names.clone());
        assert_eq!(list, names);
        assert_eq!(list.len(), 2);
        assert_eq!(list.iter().map(String::as_str).collect::<Vec<_>>(), ["ETHUSDT", "SOL\"USDT"]);
        assert_eq!((&list).into_iter().count(), 2);
        assert_eq!(format!("{list:?}"), format!("{names:?}"));
        assert_eq!(list.to_vec(), names);
        assert_eq!(list.iter().cloned().collect::<SymbolList>(), list);
        assert_ne!(list, SymbolList::from(vec!["ETHUSDT".to_string()]));
        // A clone shares the allocation: that is the point of the type.
        let again = list.clone();
        assert!(list.is_shared_with(&again));
        assert!(!list.is_shared_with(&SymbolList::from(names)), "equal is not shared");
        assert!(!none.is_shared_with(&SymbolList::default()), "two empties share nothing");
    }
}
