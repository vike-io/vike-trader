//! Centralized font-weight helpers — set weight via these, never inline `FontFamily::Name(...)` at
//! call sites.
//!
//! Each names a family [`crate::fonts::definitions`] binds to a BUNDLED face (design system spec
//! §3.3): Inter 600 and 700 for words, JetBrains Mono 600 for heavy numbers. The plain
//! `RichText`/`.monospace()` defaults cover the 400 weights. There is no binary-side registration
//! any more: every GUI binary gets the families from `crate::appearance::install`.

use egui::{FontFamily, RichText};

use crate::fonts::{BOLD, MONO_SEMIBOLD, SEMIBOLD};

fn named(s: impl Into<String>, fam: &'static str) -> RichText {
    RichText::new(s).family(FontFamily::Name(fam.into()))
}

/// Weight 600: Inter SemiBold.
pub fn semibold(s: impl Into<String>) -> RichText {
    named(s, SEMIBOLD)
}

/// Weight 700: Inter Bold.
pub fn bold(s: impl Into<String>) -> RichText {
    named(s, BOLD)
}

/// Weight 600 for numbers: JetBrains Mono SemiBold.
pub fn mono_semibold(s: impl Into<String>) -> RichText {
    named(s, MONO_SEMIBOLD)
}
