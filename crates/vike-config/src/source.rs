//! **The settings store, as the loader takes it** — `docs/decisions/0086`: settings live only in
//! the database, and there is no second source to choose between any more.
//!
//! # The rule
//!
//! > **A key resolves from the settings database's rows, or — when no row names it — from its
//! > compiled-in default. There is no file layer to fall through to, and no per-key fallback: a key
//! > missing from the store is MISSING, exactly as a credential missing from the answering store
//! > hits the live gate rather than a second lookup.**
//!
//! [`StoreLayer`] carries whether this box's rows are there, and whether the store can be read at
//! all — never which of two sources decides.
//!
//! # What the seal still is
//!
//! [`StoreLayer::Rows`] carries an `adopted: Option<&Adoption>`. Every write moves it
//! (`vike_secrets::write_setting_row_in`, 0086 point 6's *"set by the first write on a box that has
//! none, moved by every write"*), so it is the store's own INTEGRITY seal — the counts an
//! [`crate::mirror::adoption_integrity`] check verifies against. `None` is the ordinary state of a
//! store nothing has ever written to (fresh tables, no rows, nothing to check); `Some` is a store at
//! least one write has touched, and its counts must still match.
//!
//! # ⚠ Two things that must never be built
//!
//! * **A variable that picks a source.** Nothing here reads the environment, and
//!   `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` gains no row for this module.
//! * **A per-key fallback** (*"if the row is missing, check somewhere else"*). There is nowhere
//!   else, by ruling.

use vike_secrets::{Adoption, DbError, SettingsSource, StoredSettings};

/// **The settings store, as the loader takes it** — the ARM, never an `Option`.
///
/// ⚠ **The `Option<&StoredSettings>` this replaced threw away the distinction the probe needs**,
/// and it did so at several call sites that each independently rewrote a `DbError` into
/// *"there is no database"*. `vike_secrets::SettingsSource` carries the arms the whole time and
/// `rows()` collapsed them one line later, so nothing about WHICH state answered survived into the
/// resolved [`crate::Settings`].
#[derive(Debug, Clone, Copy)]
pub enum StoreLayer<'a> {
    /// The tables exist; these are their rows. `adopted` is the integrity seal, read from the same
    /// open — `None` for a store nothing has ever written a row to.
    Rows {
        /// The two tables' rows.
        rows: &'a StoredSettings,
        /// The seal, or `None` for a never-written store. See this module's doc.
        adopted: Option<&'a Adoption>,
    },
    /// No settings database on this box at all. Every key resolves to its compiled-in default.
    NoDatabase,
    /// A database that predates the settings tables. Every key resolves to its compiled-in default.
    TablesAbsent,
    /// **The store could not be read** — a `DbError`, a corrupt file, or a schema other than
    /// `vike_secrets::SCHEMA_VERSION`.
    ///
    /// The DISPOSITION is: resolve WITHOUT it and MARK the result on
    /// [`crate::Settings::store_refusal`]. No root refuses to start on it alone — the measurement
    /// that settled that is carried at [`crate::load_with_source`]'s own arm — and the enforcement
    /// point is `vike-cli config check`'s `Level::Fail`, which fires at a deploy pre-flight rather
    /// than at a running daemon.
    Unreadable(&'a str),

    /// **This caller resolves no store, and says why.**
    ///
    /// Load-bearing rather than a convenience: it is what keeps a store-blind caller compiling while
    /// forcing it to DECLARE what was previously an anonymous `None`. It is `vike-boot`'s `BootSpec`
    /// idiom applied one crate down, and it is how the surviving store-blind callers say so in their
    /// own diffs.
    NotConsulted(&'static str),
}

impl<'a> StoreLayer<'a> {
    /// The arm for a `vike_secrets::read_settings_in` result the caller is holding.
    ///
    /// `refusal` is scratch the CALLER owns, because [`StoreLayer`] is `Copy` and borrows rather
    /// than allocates — the same reason the environment arrives as a borrowed map.
    #[must_use]
    pub fn of(read: Option<&'a Result<SettingsSource, DbError>>, refusal: &'a mut String) -> Self {
        match read {
            None => StoreLayer::NoDatabase,
            Some(Ok(SettingsSource::Rows { rows, adopted })) => {
                StoreLayer::Rows { rows, adopted: adopted.as_ref() }
            }
            Some(Ok(SettingsSource::NoDatabase { .. })) => StoreLayer::NoDatabase,
            Some(Ok(SettingsSource::TablesAbsent { .. })) => StoreLayer::TablesAbsent,
            Some(Err(e)) => {
                *refusal = e.to_string();
                StoreLayer::Unreadable(refusal)
            }
        }
    }

    /// The rows, or `None` for every arm that has none.
    #[must_use]
    pub fn rows(&self) -> Option<&'a StoredSettings> {
        match self {
            StoreLayer::Rows { rows, .. } => Some(rows),
            _ => None,
        }
    }

    /// The seal, or `None`.
    #[must_use]
    pub fn adoption(&self) -> Option<&'a Adoption> {
        match self {
            StoreLayer::Rows { adopted, .. } => *adopted,
            _ => None,
        }
    }
}
