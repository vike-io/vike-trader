//! The hand-written rows of the settings registry, one module per family of reading crates.
//!
//! A row in the registry's STEP-2 target shape — a pure parser reading a caller-supplied map,
//! `Layer::Injected` + `Naming::MapLookup`, where only the name, the reading crate, the default
//! and the `Scope` vary — is spelled as ONE constructor call below, named by that scope:
//! `venue_map`, `vike_map` or `external_map`. Every other row stays a `Setting { .. }` literal, so
//! the field names are on the page wherever a row is not that shape: any other `Layer` or
//! `Naming`, a comment that annotates one FIELD rather than the whole row, or a default too long
//! for a call to fit on one line (rustfmt would break it one argument per line, which is longer
//! and less legible than the literal).
//!
//! The constructors fix only what their names say. `tests.rs`'s `scope_matches_the_name_prefix`
//! still holds a `vike_map` row to a `VIKE_` name and the other two to any other, and
//! `layer_and_naming_agree_on_every_row` still holds the `Injected`/`MapLookup` pairing every one
//! of them builds.

use crate::settings::{Layer, Naming, Scope, Setting};

pub(super) mod bridges;
pub(super) mod config;
pub(super) mod connections;
pub(super) mod daemons;
pub(super) mod gui;
pub(super) mod platform;
pub(super) mod research;

/// A `Scope::Venue` row (a venue gate or credential) read by a pure parser from a caller-supplied
/// map: `Layer::Injected` + `Naming::MapLookup`.
const fn venue_map(name: &'static str, krate: &'static str, default: &'static str) -> Setting {
    Setting {
        name,
        krate,
        scope: Scope::Venue,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default,
    }
}

/// A `Scope::Vike` row (a `VIKE_*` knob) read by a pure parser from a caller-supplied map:
/// `Layer::Injected` + `Naming::MapLookup`.
const fn vike_map(name: &'static str, krate: &'static str, default: &'static str) -> Setting {
    Setting {
        name,
        krate,
        scope: Scope::Vike,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default,
    }
}

/// A `Scope::External` row (a third-party or OS-provided name) read by a pure parser from a
/// caller-supplied map: `Layer::Injected` + `Naming::MapLookup`.
const fn external_map(name: &'static str, krate: &'static str, default: &'static str) -> Setting {
    Setting {
        name,
        krate,
        scope: Scope::External,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        default,
    }
}
