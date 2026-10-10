//! The hand-written rows of the settings registry, one module per family of reading crates.
//!
//! A row in the registry's STEP-2 target shape — a pure parser reading a caller-supplied map,
//! `Layer::Injected` + `Naming::MapLookup`, where only the name, the reading crate, the default,
//! the `Scope` and the `Medium` vary — is spelled as ONE constructor call below, named by that
//! scope and that medium: `vike_env`, `vike_cred`, `vike_node`, `venue_cred`, `venue_unread`,
//! `external_env` or `external_cred`. Every other row stays a `Setting { .. }` literal, so the field
//! names are on the page wherever a row is not that shape: any other `Layer` or `Naming`, a
//! `Medium` no constructor carries (`Refused`, `CompileTime`), a comment that annotates one FIELD
//! rather than the whole row, or a default too long for a call to fit on one line (rustfmt would
//! break it one argument per line, which is longer and less legible than the literal).
//!
//! ⚠ **The medium is in the constructor's NAME on purpose**: a row cannot be added without choosing
//! where its value comes from, and decision 0111's process-environment ratchet
//! (`crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `PROCESS_ENV_PIN`) keys on that
//! choice. A `vike_env` row joins the pin unless `ENV_ALLOWLIST` excuses it.
//!
//! The constructors fix only what their names say. `tests.rs`'s `scope_matches_the_name_prefix`
//! still holds a `vike_*` row to a `VIKE_` name and the others to any other, and
//! `layer_and_naming_agree_on_every_row` still holds the `Injected`/`MapLookup` pairing every one
//! of them builds.

use crate::settings::{Layer, Medium, Naming, Scope, Setting};

pub(super) mod bridges;
pub(super) mod config;
pub(super) mod connections;
pub(super) mod daemons;
pub(super) mod gui;
pub(super) mod platform;
pub(super) mod research;

/// The one body every constructor below shares: an `Injected`/`MapLookup` row.
const fn map_row(
    name: &'static str,
    krate: &'static str,
    scope: Scope,
    medium: Medium,
    default: &'static str,
) -> Setting {
    Setting {
        name,
        krate,
        scope,
        layer: Layer::Injected,
        naming: Naming::MapLookup,
        medium,
        default,
    }
}

/// A `Scope::Vike` row (a `VIKE_*` knob) looked up in the PROCESS ENVIRONMENT a root swept and
/// handed down.
const fn vike_env(name: &'static str, krate: &'static str, default: &'static str) -> Setting {
    map_row(name, krate, Scope::Vike, Medium::ProcessEnv, default)
}

/// A `Scope::Vike` row looked up in the CREDENTIAL map (a store key, or a flag folded into it).
const fn vike_cred(name: &'static str, krate: &'static str, default: &'static str) -> Setting {
    map_row(name, krate, Scope::Vike, Medium::CredentialMap, default)
}

/// A `Scope::Vike` row looked up in the NODE-KEY map.
const fn vike_node(name: &'static str, krate: &'static str, default: &'static str) -> Setting {
    map_row(name, krate, Scope::Vike, Medium::NodeKeyMap, default)
}

/// A `Scope::Venue` row (a venue gate or credential) looked up in the CREDENTIAL map.
const fn venue_cred(name: &'static str, krate: &'static str, default: &'static str) -> Setting {
    map_row(name, krate, Scope::Venue, Medium::CredentialMap, default)
}

/// A `Scope::Venue`-shaped name under which NO value is read: an HTTP header the code writes, a
/// variable put into a child process's environment, or a test fixture inside a `src/` file.
const fn venue_unread(name: &'static str, krate: &'static str, default: &'static str) -> Setting {
    map_row(name, krate, Scope::Venue, Medium::NotRead, default)
}

/// A `Scope::External` row (a third-party or OS-provided name) looked up in the PROCESS
/// ENVIRONMENT a root swept and handed down.
const fn external_env(name: &'static str, krate: &'static str, default: &'static str) -> Setting {
    map_row(name, krate, Scope::External, Medium::ProcessEnv, default)
}

/// A `Scope::External` row looked up in the CREDENTIAL map.
const fn external_cred(name: &'static str, krate: &'static str, default: &'static str) -> Setting {
    map_row(name, krate, Scope::External, Medium::CredentialMap, default)
}
