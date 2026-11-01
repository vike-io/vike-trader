//! What this crate's `src/` unit tests and its integration suites both use, so neither keeps a copy:
//! a `#[cfg(test)]` module is out of an integration suite's reach, and `tests/common/` out of a unit
//! test's. Behind `test-support` (only the crate's self dev-dependency turns it on), so a shipped
//! build never compiles it.

use std::path::Path;

use vike_catalog::{CatalogMode, CatalogSource};

use crate::backend::backend_registry::DATAHUB_OBSERVE_KEY_NAME;
use crate::data::catalog_refresh::{VenueCatalogRow, VenueRefreshState};
use crate::data::catalog_wire::CatalogDial;

/// A dial of the datahub at `addr` under the observe key's NAME and with no key material: how a test
/// reaches a scripted fake datahub (which opens every connection key-less) or names a dead port.
/// `CatalogDial` carries no socket, so it costs nothing until something dials it.
pub fn datahub_dial(addr: &str) -> CatalogDial {
    CatalogDial {
        addr: addr.to_string(),
        key_name: DATAHUB_OBSERVE_KEY_NAME.to_string(),
        keys: None,
    }
}

/// An idle row of `venue` with a server address and nothing fetched yet, on the given ROUTE (what
/// `VenueCatalogRow::availability` reads): `catalog_refresh`'s, the Instruments screen's and the
/// `screens` binary's catalog fixture.
pub fn catalog_row(
    venue: &str,
    source: Option<CatalogSource>,
    mode: Option<CatalogMode>,
) -> VenueCatalogRow {
    VenueCatalogRow {
        venue: venue.into(),
        mode,
        source,
        server: Some("127.0.0.1:7878".into()),
        stamp: None,
        baseline: None,
        local: None,
        own_keys: false,
        state: VenueRefreshState::Idle,
    }
}

/// `dir` — the caller's own self-deleting directory, handed back — with a settings database planted
/// in it holding one row, `preferences.log_level = "info"`: the store an appearance save writes into.
pub fn planted<D: AsRef<Path>>(dir: D) -> D {
    vike_secrets::plant_settings_rows(
        dir.as_ref(),
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "preferences".to_string(),
                key: "log_level".to_string(),
                value: "\"info\"".to_string(),
            }],
            ..Default::default()
        },
    )
    .expect("a fresh store plants");
    dir
}

/// The stored value of `preferences.<key>` in `dir`'s settings database; `None` when the store, its
/// rows or the row are absent.
pub fn preference(dir: &Path, key: &str) -> Option<String> {
    let read = vike_secrets::read_settings_in(dir).ok()?;
    read.rows()?
        .settings
        .iter()
        .find(|r| r.section == "preferences" && r.key == key)
        .map(|r| r.value.clone())
}

/// A loopback address nothing listens on: an ephemeral port bound, read and released (a hard-coded
/// port could collide on a shared runner), so a connect to it is refused at once, never left hanging.
pub fn dead_addr() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let addr = listener.local_addr().expect("ephemeral local_addr").to_string();
    drop(listener);
    addr
}
