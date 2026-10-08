//! `scan` — the pure source scanner behind the settings-registry gate.
//!
//! String in, data out: no filesystem, no globals, so every edge case (nested parens, `)`
//! inside a literal, `env::var` mentioned in a `//!` doc block) is unit-testable directly.
//! The filesystem walk lives in `crates/vike-ops/tests/settings_secrets/settings_registry.rs`.
//!
//! Compiled only under `cfg(test)` or the `test-support` feature (like `libm_walk`): no production
//! code calls it, and a normal edge enabling the feature is refused by
//! `crates/vike-ops/tests/architecture/test_surface_gate.rs`.
//!
//! Deliberately hand-rolled rather than `regex`/`syn`: this workspace links every crate into
//! one order-signing binary and audits the whole tree with `cargo deny`, so a scanner used
//! only by a test does not justify a new dependency.
//!
//! # Three read shapes, one comment stripper
//!
//! The registry's rule is "libraries take configuration as PARAMETERS; only binaries read global
//! configuration state", and a library can break it in three ways that look nothing alike in
//! source:
//!
//! - [`find_env_reads`] / [`find_map_lookups`] — the PROCESS environment, keyed on an `env::var`
//!   call or an env-shaped map key. ⚠ The call half keyed the PATH-QUALIFIED spelling only, so an
//!   imported bare `var("VIKE_X")` matched nothing at all; it now also searches whatever bare or
//!   aliased name the file's own `use std::env…` line brings into scope, and that function's doc
//!   lists what the widening still cannot reach.
//! - [`find_calls`] — the credential STORE (its file backend, `<project>/settings/secrets.env`,
//!   on a box with no settings database), which
//!   is a plain `std::fs` read with no `env::var` anywhere near it and therefore structurally
//!   invisible to the two above. It is keyed on the NAME of the function that performs the read.
//! - [`find_path_reads`] — the SAME store, read WITHOUT calling any of those names: a
//!   `std::fs::read_to_string(".env")` written by hand. Keyed on neither a variable name nor a
//!   reader name (there is no name at all), but on the SHAPE — a filesystem opener whose path
//!   argument names the store. See its doc for what that costs and what it cannot see.
//!
//! All three run over the same `strip_comments` pass, so a `//!` doc block naming any of
//! them is not a call site in any scanner — the single most common false positive, and the reason
//! there is one stripper rather than three.

mod calls;
mod env_reads;
mod imports;
mod lexer;

pub use calls::{Call, PathRead, defines_fn, find_calls, find_path_reads};
pub use env_reads::{
    Resolved, const_table, find_env_reads, find_lookup_sites, find_map_lookups, resolve_arg,
};
pub use imports::imported_spellings;
pub use lexer::{mentions_ident, string_literals, strip_comments};

/// One `env::var` / `env::var_os` call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvRead {
    /// The RAW argument text, verbatim, parens balanced. Resolution is Task 3's job.
    pub arg: String,
    /// 1-indexed line of the call site.
    pub line: usize,
}

#[path = "scan/calls_tests.rs"]
#[cfg(test)]
mod calls_tests;
#[path = "scan/env_reads_tests.rs"]
#[cfg(test)]
mod env_reads_tests;
#[path = "scan/imports_tests.rs"]
#[cfg(test)]
mod imports_tests;
#[path = "scan/lexer_tests.rs"]
#[cfg(test)]
mod lexer_tests;
