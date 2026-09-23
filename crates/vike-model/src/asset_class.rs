//! The asset-class taxonomy every instrument is tagged with. Ports no Python file — vike-native.
//!
//! # ⚠ The variant list is declared ONCE, by the `asset_classes!` invocation below
//!
//! `docs/decisions/0061-an-instrument-names-its-kind.md` uses this taxonomy rather than widening it,
//! and its Phase 5 stores a variant as a word in the settings database — *"a column plus a `CHECK`
//! of the permitted words, with the enum in Rust remaining the single place the list is DEFINED.
//! Two lists — one in code, one in SQL — would be the fifth partial encoding this record refuses
//! everywhere else."*
//!
//! `vike-secrets` — which owns that schema — declares **no `vike-*` dependency at all**, by design
//! and by gate (`crates/vike-boot/tests/dependency_floor.rs`), so it cannot name [`AssetClass`] and
//! could only ever hold a second copy of the word list. ⚠ That is NOT a layer verdict, and it must
//! not be re-argued as one: this crate is layer 10 and `vike-secrets` is layer 15, so the layer
//! rule would PERMIT the edge — it is the zero-dependency floor that refuses it, and moving this
//! type down did not relax that floor by one crate. [`AssetClass::SQL_WORDS`] is how the schema
//! builder does not have to duplicate anything: `vike_secrets::profile_store::profile_ddl` takes
//! the word list as a PARAMETER and spells none of its own, so the `CHECK` clause in the database
//! is this declaration, rendered.
//!
//! The macro is what makes that single declaration total: [`AssetClass::ALL`],
//! [`AssetClass::SQL_WORDS`], [`AssetClass::sql_word`] and [`AssetClass::from_sql_word`] are all
//! expanded from the same variant list, so a twelfth variant joins every one of them or joins none.
//! ⚠ The picker-tab grouping is deliberately NOT generated, and deliberately NOT here:
//! `vike_catalog::tab_for` is a free function over `vike_catalog::Tab` carrying an exhaustive
//! `match`, because a tab is a JUDGEMENT about where a class belongs in the Symbol picker — a GUI
//! concern, not an instrument one — and that `match` is what forces a new variant's author to make
//! it.
//!
//! # ⚠ The stored word is the SERDE word, deliberately — it is not a third spelling
//!
//! [`AssetClass::sql_word`] returns the variant's own name, which is exactly what this type's
//! derived `Serialize` already writes into `vike_catalog::CatalogCache` on disk.
//! `sql_word_is_the_serde_word` pins that equality for every variant. A separate lowercase spelling
//! was available and refused: it would have been a second string form of one vocabulary, which is
//! the defect this module's doc opens with.
//!
//! ⚠ It is NOT `crates/vike-ops/src/docs_data.rs`'s `asset_class_name` either, and must never be
//! wired to it. That is a PUBLISHED page slug (kebab-case, with `Index` rendered `index-instrument`
//! so it cannot collide with a directory's own index page) — coupling a database column to a
//! published URL would make a docs rename a schema migration.

use serde::{Deserialize, Serialize};

/// Declare the asset-class vocabulary ONCE. See this module's doc for why every derived form is
/// expanded from here rather than written beside the enum.
macro_rules! asset_classes {
    ($($variant:ident,)+) => {
        /// One asset class per instrument. Drives picker tabs, symbology expectations, market-hours,
        /// and — since `docs/decisions/0061` — the `mount.asset_class` column of the settings store.
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Serialize, Deserialize)]
        pub enum AssetClass {
            $($variant,)+
        }

        impl AssetClass {
            /// Every variant, in declaration order. Complete BY CONSTRUCTION: the enum is declared
            /// nowhere but this macro invocation, so a variant cannot exist and be absent here.
            pub const ALL: &'static [AssetClass] = &[$(AssetClass::$variant,)+];

            /// Every variant's stored word, in the same order as [`AssetClass::ALL`] — the list a
            /// SQL `CHECK` is rendered from, and the reason `vike-secrets` spells none of its own.
            pub const SQL_WORDS: &'static [&'static str] = &[$(stringify!($variant),)+];

            /// This class's stored word. Never operator input — a `&'static str` off a closed enum,
            /// which is what keeps it safe to interpolate into a `CHECK` clause (the same argument
            /// `vike_secrets::profile_store::ProfileKind::sql_word` makes for its own).
            #[must_use]
            pub const fn sql_word(self) -> &'static str {
                match self {
                    $(AssetClass::$variant => stringify!($variant),)+
                }
            }

            /// Parse a stored word back. `None` for anything outside the vocabulary — a word the
            /// schema's `CHECK` would have refused, so reaching that arm means the row was written
            /// by something other than this tree.
            #[must_use]
            pub fn from_sql_word(word: &str) -> Option<AssetClass> {
                $(if word == stringify!($variant) {
                    return Some(AssetClass::$variant);
                })+
                None
            }
        }
    };
}

asset_classes! {
    Equity,
    Etf,
    CryptoSpot,
    CryptoPerp,
    CryptoFuture,
    Option,
    Fx,
    Future,
    Index,
    PredictionMarket,
    Cfd,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two generated lists describe the SAME declaration, so they cannot disagree in length or
    /// in order. Cheap, and it is what would catch a hand-edit of the expansion.
    #[test]
    fn all_and_sql_words_are_the_same_declaration() {
        assert_eq!(AssetClass::ALL.len(), AssetClass::SQL_WORDS.len());
        for (class, word) in AssetClass::ALL.iter().zip(AssetClass::SQL_WORDS) {
            assert_eq!(class.sql_word(), *word);
        }
    }

    /// Every word is distinct — a duplicate would make `from_sql_word` answer for the first variant
    /// and silently re-tag every row carrying the second.
    #[test]
    fn every_stored_word_is_distinct() {
        let mut seen: Vec<&str> = AssetClass::SQL_WORDS.to_vec();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len(), "two variants share a stored word: {seen:?}");
    }

    /// Round trip, both directions, for every variant — and a refusal for a word outside the
    /// vocabulary, which is the case a hand-written `mount` row would hit.
    #[test]
    fn every_class_round_trips_through_its_stored_word() {
        for class in AssetClass::ALL {
            assert_eq!(AssetClass::from_sql_word(class.sql_word()), Some(*class));
        }
        assert_eq!(AssetClass::from_sql_word("cryptospot"), None, "the word is case-sensitive");
        assert_eq!(AssetClass::from_sql_word("Perp"), None);
        assert_eq!(AssetClass::from_sql_word(""), None);
    }

    /// ⚠ **The stored word is the SERDE word.** `vike_catalog::CatalogCache` already writes these
    /// strings to disk through the derived `Serialize`, so the database column and the cache file
    /// carry ONE spelling. If a `#[serde(rename…)]` is ever added, this fails rather than quietly
    /// minting a second string form of one vocabulary.
    #[test]
    fn sql_word_is_the_serde_word() {
        for class in AssetClass::ALL {
            let json = serde_json::to_string(class).expect("AssetClass serializes");
            assert_eq!(
                json,
                format!("\"{}\"", class.sql_word()),
                "the serde spelling and the stored word have diverged for {class:?}"
            );
            let back: AssetClass =
                serde_json::from_str(&json).expect("AssetClass round-trips through serde");
            assert_eq!(back, *class);
        }
    }

    /// A stored word must be safe to render into a SQL `CHECK` as a single-quoted literal without
    /// any escaping — which is what lets `vike_secrets::profile_store::profile_ddl` interpolate it.
    /// ASCII letters only, non-empty. A variant named with a quote or a backslash would fail here
    /// long before it reached a database.
    #[test]
    fn every_stored_word_is_a_bare_sql_identifier_literal() {
        for word in AssetClass::SQL_WORDS {
            assert!(!word.is_empty(), "an empty stored word cannot be told from a missing claim");
            assert!(
                word.chars().all(|c| c.is_ascii_alphanumeric()),
                "{word:?} is not renderable as a bare quoted SQL literal"
            );
        }
    }
}
