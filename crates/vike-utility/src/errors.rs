//! The failure and refusal vocabulary every crate above this floor shares: what a failed write DID
//! ([`FailureOutcome`]), how a refusal is declared (`refusal_enum!`, exported at this crate's root as
//! `vike_utility::refusal_enum!`), the one text a person reads for one ([`render_refusal`]) and a refusal
//! handed across a seam that holds no refusal enum, its id kept ([`CarriedRefusal`]).
//!
//! **A decision about an error is made on a type, never on its text.** A refusal enum gives code a
//! variant and an id to decide on; its text is for a person and may be reworded freely.
//!
//! **`refusal_enum!`.** One declaration per refusal enum. Each variant carries ONE text literal, a
//! `format!` string over the variant's own fields, and ONE id, readable dotted or snake words set in the
//! declaration (never derived from the variant's position):
//! ```text
//! vike_utility::refusal_enum! {
//!     /// The doc.
//!     #[derive(Debug, Clone, PartialEq)]
//!     pub enum ProfileRefusal with ids ProfileRefusalId {
//!         /// The variant's doc.
//!         StepShorterThanTest { step: u32, test: u32 }
//!             => "profile.walkforward.step_shorter_than_test":
//!                "walkforward step {step} is shorter than its test window {test}",
//!         Empty => "profile.empty": "the profile is empty",
//!     }
//! }
//! ```
//! From that one table it generates:
//! - the refusal enum itself (its attributes and docs as written), with `Display` writing the literal
//!   through `write!`, so the compiler checks every `{hole}` against the variant's fields (`{word:?}`
//!   works) and a field the text never names is an unused-variable warning;
//! - the ID enum (here `ProfileRefusalId`, `Debug, Clone, Copy, PartialEq, Eq, Hash`) through
//!   `word_enum!`'s DEFINING form, one unit member per variant: its `word()` is the id, `ALL` the closed
//!   list a published catalog walks, `WORDS` every id, `from_word` the exact reader of an id that came
//!   back (`None` for one this build does not know). An id listed twice is an unreachable arm;
//! - `id()` on the refusal and `template()` on both: the literal itself, its holes unfilled, which is
//!   what a catalog publishes;
//! - `rendered()` on the refusal: [`render_refusal`] of its shown id (`shown_id()`, below) and its
//!   text, what a surface SHOWS. Its `Display` stays the bare text, for a frame's text, an event's reason
//!   and a carrier's hole. Every id is published in the workspace's `refusals.json`
//!   (`crates/vike-backend/tests/refusal_catalog.rs`);
//! - `carried()` on the refusal: the [`CarriedRefusal`] a producer hands across a seam that holds no
//!   refusal enum, in place of `.to_string()`, which would drop the id;
//! - [`Declared`] on the refusal, its shown id.
//!
//! **No generated member warns unused.** A refusal enum may sit in a crate-PRIVATE position (a binary's
//! module, a test) and be only displayed. In another crate rustc drops the `dead_code` lint for an item a
//! foreign macro made (`crates/vike-utility/tests/refusal_enum_private.rs`); in this crate it would fire
//! (`crates/vike-utility/src/errors_tests.rs`'s `DisplayedOnly`), so the refusal's `Display` names every
//! generated member and each is live wherever the refusal is, whatever crate declares it.
//!
//! The variant's id is part of the published contract once a surface shows it: keep it when the text
//! is reworded, and add a new variant rather than repurpose an id.
//!
//! # A carrier shows ONE id
//!
//! A variant that only forwards another refusal is marked `carries <field>`, its field any [`Declared`]
//! value (a refusal enum, a [`CarriedRefusal`]):
//! ```text
//!     StrategyCostRefused { refusal: CostRefusal } => carries refusal,
//!         "profile.strategy.cost_refused": "{refusal}",
//! ```
//! Its `shown_id()` is the carried refusal's id when that has one, else the variant's own (`id()` stays the
//! variant's own: a type to decide on). `rendered()` and `carried()` read it, so a refusal wrapped in carriers
//! at any depth shows its innermost id, once. The marker is opt-in: a variant whose own id is the news (a
//! row that holds another sentence as its detail) stays unmarked and shows its own.

use core::fmt;

/// What a failed write (an order, a cancel, a modify, a hub command) DID, in the four words every
/// caller decides on. A success is not one of them: it is the write's own `Ok`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureOutcome {
    /// It never left: nothing reached the other side, so it is safe to send again.
    NotSent,
    /// It may have happened (sent, then the answer was lost). Never reject it and never send it
    /// again: resolve it by a read of what the other side holds.
    OutcomeUnknown,
    /// Transient: the other side answered "not now" (busy, rate limited, starting); nothing
    /// happened, so ask again later.
    NotNow,
    /// Final: the other side answered no, and asking again gets the same answer.
    Refused,
}

/// The one text a person reads for a refusal: `error [<id>]: <text>`. Every surface that shows a
/// refusal (the CLI, MCP, the desktop, Telegram) renders it here, from a typed refusal
/// (`render_refusal(refusal.id().word(), &refusal)`) or an id and a text that crossed a wire.
#[must_use]
pub fn render_refusal(id: &str, text: impl fmt::Display) -> String {
    format!("error [{id}]: {text}")
}

/// [`render_refusal`] for a refusal that crossed a wire or a table with its id OPTIONAL (an order's refusal,
/// `vike_execution::OrderView::refusal_id`): the rendered form with an id, the bare `text` without one (a producer
/// that sets none, or an older node), so no surface invents an id.
#[must_use]
pub fn render_refusal_or_text(id: Option<&str>, text: &str) -> String {
    match id {
        Some(id) => render_refusal(id, text),
        None => text.to_owned(),
    }
}

/// A value whose id a person reads: a refusal enum's shown id (`refusal_enum!`), or a
/// [`CarriedRefusal`]'s own (`None` for free text). What a `carries` field must be.
pub trait Declared {
    /// The one id this value shows, if it has one. Nothing invents one.
    fn declared_id(&self) -> Option<&str>;
}

/// A refusal handed across a seam that holds no refusal enum (a bridge's error, a child's answer): its
/// declared id, or `None` for free text (a producer with no `refusal_enum!`, a venue's own words), and
/// its BARE text, never a `rendered()` one. A producer of a refusal enum hands `refusal.carried()`; one
/// of free text adds `.into()`, so `?` on a `Result<_, String>` converts by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedRefusal {
    id: Option<String>,
    text: String,
}

impl CarriedRefusal {
    /// `text` (bare) under `id`.
    #[must_use]
    pub fn new(id: Option<String>, text: String) -> Self {
        // A rendered text in a carrier would show its id twice once the carrier is rendered.
        debug_assert!(
            !text.starts_with("error ["),
            "a carried refusal holds the BARE text: {text}"
        );
        Self { id, text }
    }

    /// The one text a person reads for it ([`render_refusal_or_text`]): the bare text when it has no id.
    #[must_use]
    pub fn rendered(&self) -> String {
        render_refusal_or_text(self.id.as_deref(), &self.text)
    }

    /// Its BARE text, what `Display` writes, borrowed: what a wire that keeps its own text bytes writes.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl Declared for CarriedRefusal {
    fn declared_id(&self) -> Option<&str> {
        self.id.as_deref()
    }
}

impl fmt::Display for CarriedRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl From<String> for CarriedRefusal {
    fn from(text: String) -> Self {
        Self::new(None, text)
    }
}

impl From<&str> for CarriedRefusal {
    fn from(text: &str) -> Self {
        Self::new(None, text.to_owned())
    }
}

/// Declare a refusal enum, its id enum and their members from one table; see
/// `crates/vike-utility/src/errors.rs`'s module doc for the form and what it generates.
#[macro_export]
macro_rules! refusal_enum {
    // `shown_id`'s value for one variant: its own id, or the `carries` field's when that has one.
    (@shown $own:expr) => { $own };
    (@shown $own:expr, $carried:ident) => {
        $crate::errors::Declared::declared_id($carried).unwrap_or($own)
    };
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident with ids $id:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident $({ $($field:ident : $field_type:ty),+ $(,)? })?
                    => $(carries $carried:ident,)? $id_word:literal : $text:literal,
            )+
        }
    ) => {
        $(#[$meta])*
        $vis enum $name {
            $($(#[$variant_meta])* $variant $({ $($field : $field_type),+ })?,)+
        }

        $crate::word_enum! {
            /// The stable id of each variant of the refusal enum declared beside it by
            /// `vike_utility::refusal_enum!`: its `word()` is the id a person reads.
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
            $vis enum $id {
                $($variant => $id_word,)+
            }
        }

        impl $id {
            /// The text template of this id: the variant's one literal, its `{holes}` unfilled.
            #[must_use]
            pub const fn template(self) -> &'static str {
                match self {
                    $($id::$variant => $text,)+
                }
            }
        }

        impl $name {
            /// This refusal's id.
            #[must_use]
            pub const fn id(&self) -> $id {
                match self {
                    $($name::$variant { .. } => $id::$variant,)+
                }
            }

            /// This refusal's text template: its variant's one literal, its `{holes}` unfilled.
            #[must_use]
            pub const fn template(&self) -> &'static str {
                self.id().template()
            }

            /// The one id a person reads for this refusal: a `carries` variant's carried id when that has one,
            /// else this variant's own (`vike_utility::errors`'s module doc, "A carrier shows ONE id").
            #[must_use]
            pub fn shown_id(&self) -> &str {
                match self {
                    $($name::$variant { $($carried,)? .. } =>
                        $crate::refusal_enum!(@shown $id::$variant.word() $(, $carried)?),)+
                }
            }

            /// The one text a person reads for this refusal, `error [<shown id>]: <text>`
            /// (`vike_utility::errors::render_refusal`). `Display` stays the bare text: a frame's text, an event's
            /// reason and a carrier's hole are that.
            #[must_use]
            pub fn rendered(&self) -> ::std::string::String {
                $crate::errors::render_refusal(self.shown_id(), self)
            }

            /// This refusal handed across a seam that holds no refusal enum, its shown id kept: what a producer
            /// hands in place of `.to_string()`.
            #[must_use]
            pub fn carried(&self) -> $crate::errors::CarriedRefusal {
                $crate::errors::CarriedRefusal::new(
                    ::core::option::Option::Some(self.shown_id().to_owned()),
                    ::std::string::ToString::to_string(self),
                )
            }
        }

        impl $crate::errors::Declared for $name {
            fn declared_id(&self) -> ::core::option::Option<&str> {
                ::core::option::Option::Some(self.shown_id())
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                // Names every generated member (`template` reaches `id` and the id's `template`, `rendered`
                // reaches `shown_id`, `parse` reaches `ALL` and `word`), so none is `dead_code` while the refusal
                // is used: see the module doc.
                let _ = (
                    $name::template,
                    $name::rendered,
                    $name::carried,
                    &$id::WORDS,
                    $id::from_word,
                    $id::parse,
                );
                match self {
                    $($name::$variant $({ $($field),+ })? => ::core::write!(formatter, $text),)+
                }
            }
        }
    };
}

#[path = "errors_tests.rs"]
#[cfg(test)]
mod errors_tests;
