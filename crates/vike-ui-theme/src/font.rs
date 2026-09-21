//! Centralized font-weight helpers — set weight via these, never inline `FontFamily::Name(...)` at
//! call sites. Moved verbatim out of `vike-app`'s inline `font` module so any GUI crate can reach
//! them (a precondition for hoisting the tool-view block out of the CI-excluded `vike-app`).
//!
//! Each maps to a Segoe UI face a binary registers in its `install_fonts` (matching a Python
//! `font-weight`: 200→extralight, 600→semibold, 700→bold). These helpers only NAME the family —
//! registration stays in the binary (`vike-app`), so this crate depends on `egui` alone. (`reg`/
//! `light`/`semilight`/`mono` helpers existed but were never called — dropped in the dedup-app
//! cleanup; add them back if a call site ever needs one. The plain-`RichText`/`.monospace()`
//! defaults cover 400 and mono.)

use egui::{FontFamily, RichText};

fn named(s: impl Into<String>, fam: &'static str) -> RichText {
    RichText::new(s).family(FontFamily::Name(fam.into()))
}

/// Python font-weight:200 (Segoe UI Light — no static ExtraLight face exists).
pub fn extralight(s: impl Into<String>) -> RichText {
    named(s, "extralight")
}

/// Python font-weight:600 (Segoe UI Semibold).
pub fn semibold(s: impl Into<String>) -> RichText {
    named(s, "semibold")
}

/// Python font-weight:700 / setBold (Segoe UI Bold).
pub fn bold(s: impl Into<String>) -> RichText {
    named(s, "bold")
}
