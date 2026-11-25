//! `word_enum!` — ONE spelling of a closed word vocabulary: an enum, its roster and its words, from
//! one table. Exported at this crate's root as `vike_utility::word_enum!` (a `#[macro_export]` macro has
//! no module path).
//!
//! **Why one macro.** `vike-model` spelled a variant's word six ways (`as_str`, `word`, `as_word`,
//! `name`, `sql_word`, `FromStr`) and a roster four (`ALL`, `NAMES`, `KNOWN`, `WORDS`), with three hand
//! macros beside them; a reader could not tell which spelling an enum had without opening it. Every
//! enum this macro serves carries the same members, and these names are canonical:
//!
//! - `ALL` — every member, a fixed-size array in the table's order;
//! - `WORDS` — the words, in the same order (`WORDS[i] == ALL[i].word()`);
//! - `word(self)` — the one word, `const` and exhaustive;
//! - `from_word(&str)` — the EXACT inverse of `word`, `None` for anything else (a stored or wire
//!   word, read back as it was written);
//! - `parse(&str)` — the case-insensitive (ASCII) inverse, `None` for anything else (a word an
//!   operator types). Whitespace is the caller's: nothing here trims.
//!
//! It derives NO serde: an enum that rides a wire, a journal or a stored row keeps its own frozen
//! serde tag, and the enum's own tests hold that tag (or its alias) equal to the word.
//!
//! **Two forms.**
//! - DEFINING: the invocation declares the enum (its attributes, its docs) and the word of each
//!   variant; the enum exists nowhere else, so the roster is complete by construction.
//!   ```text
//!   word_enum! {
//!       /// The doc.
//!       #[derive(Debug, Clone, Copy, PartialEq, Eq)]
//!       pub enum Network {
//!           Mainnet => "mainnet",
//!           Testnet => "testnet",
//!       }
//!   }
//!   ```
//! - IMPL-ONLY: the enum stays a literal `pub enum` block (it carries serde attributes a reader should
//!   see on the block, as `vike_model::run_request::replay::ReplayKind` does), and the invocation adds
//!   the members. `word` is an exhaustive `match` over the listed variants, so a variant the table
//!   forgets is a compile error and a variant listed twice is an unreachable arm.
//!   ```text
//!   word_enum! {
//!       impl Network {
//!           Mainnet => "mainnet",
//!           Testnet => "testnet",
//!       }
//!   }
//!   ```
//!
//! A table lists its variants in DECLARATION order, so `ALL` is the order a derived `Ord` sorts in.
//! A word listed twice is an unreachable arm of `from_word`; the enum's own test holds the words
//! distinct and every member's round trip.
//!
//! A refusal's IDS are a vocabulary of this kind too: `refusal_enum!`
//! (`crates/vike-utility/src/errors.rs`) declares its id enum through the DEFINING form, so an id
//! carries the same `ALL`, `WORDS`, `word`, `from_word` and `parse` as every other closed vocabulary.

/// Declare a closed word vocabulary once; see `crates/vike-utility/src/word_enum.rs`'s module doc for
/// the members, the two forms and what it deliberately does not derive.
#[macro_export]
macro_rules! word_enum {
    // The members both forms expand to. First, so neither form's matcher ever sees `@members`.
    (@members $name:ident { $($variant:ident => $word:literal,)+ }) => {
        impl $name {
            /// Every member, in the table's (declaration) order.
            pub const ALL: [$name; [$($word),+].len()] = [$($name::$variant,)+];

            /// Every member's word, in [`Self::ALL`]'s order.
            pub const WORDS: [&'static str; [$($word),+].len()] = [$($word,)+];

            /// The one word of this member. Exhaustive and `const`, so a new variant must be given
            /// its word before the crate builds.
            #[must_use]
            pub const fn word(self) -> &'static str {
                match self {
                    $($name::$variant => $word,)+
                }
            }

            /// The member whose word is EXACTLY `word`, or `None`: the inverse of [`Self::word`]
            /// for a word read back as it was written.
            #[must_use]
            pub fn from_word(word: &str) -> ::core::option::Option<$name> {
                match word {
                    $($word => ::core::option::Option::Some($name::$variant),)+
                    _ => ::core::option::Option::None,
                }
            }

            /// The member whose word equals `text` ignoring ASCII case, or `None`: the inverse of
            /// [`Self::word`] for a word an operator types. The caller owns the refusal sentence
            /// (render [`Self::WORDS`] in it) and any trimming.
            #[must_use]
            pub fn parse(text: &str) -> ::core::option::Option<$name> {
                $name::ALL.into_iter().find(|member| member.word().eq_ignore_ascii_case(text))
            }
        }
    };
    // IMPL-ONLY: the enum is declared elsewhere, as a literal `pub enum` block.
    (
        impl $name:ident {
            $($variant:ident => $word:literal,)+
        }
    ) => {
        $crate::word_enum! { @members $name { $($variant => $word,)+ } }
    };
    // DEFINING: the enum is declared here and nowhere else.
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident => $word:literal,)+
        }
    ) => {
        $(#[$meta])*
        $vis enum $name {
            $($(#[$variant_meta])* $variant,)+
        }

        $crate::word_enum! { @members $name { $($variant => $word,)+ } }
    };
}

#[path = "word_enum_tests.rs"]
#[cfg(test)]
mod word_enum_tests;
