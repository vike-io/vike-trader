//! **WHICH SOURCE answers for every settings key on this box** —
//! `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s crossing, as one
//! decision taken ONCE per run.
//!
//! # The rule
//!
//! > **One settings SOURCE per run, chosen on one probe before any key is looked up: a settings
//! > database carrying an ADOPTION row answers for every settings key and the four files are not
//! > opened for resolution at all; a store without one leaves the files answering exactly as they
//! > do today, forever; and an ADOPTED store that cannot be read whole is a refusal naming the
//! > store — never a fall-through, never a resolved default.**
//!
//! The probe is [`vike_secrets::Adoption`]'s row. Not the database's existence, not the settings
//! tables' existence (`vike-cli secrets migrate` creates BOTH through the shared DDL batch, so on
//! every fresh box they are present and EMPTY — MEASURED, and it is what kills the obvious probe),
//! not a row count (an accident moves it in both directions), and not a `precedence` column (0057
//! refuses one: it puts the type's seal one `UPDATE` away from gone).
//!
//! # ⚠ This is `vike_secrets::Backend`'s shape, not an argument that settings are special
//!
//! `crates/vike-secrets/src/store.rs`'s `Backend` decides which store answers for a credential NAME
//! on ONE probe, before any lookup. This decides which source answers for a SECTION and key on one
//! probe, before any lookup. **A key missing from the source that answered is MISSING** and
//! resolves to its compiled-in default — it does not go looking in a file, exactly as a credential
//! missing from the answering store hits the live gate rather than a second lookup. The root
//! `CLAUDE.md` states why a per-KEY fallback is forbidden, and the reason transfers unchanged: it
//! makes *where is my setting* a question with two answers, and on a half-filled store it reads
//! half from each.
//!
//! What today's loader does is not a chain either, and the distinction matters for reading the
//! diff: [`crate::load_with_source`] is a LAYERED apply over one typed model where every key passes
//! through every layer into the same `Policy::apply`. The crossing REMOVES a layer. It adds no
//! lookup order, and none may be added.
//!
//! # ⚠ Two things that must never be built
//!
//! * **A variable that picks the authority** (`VIKE_SETTINGS_AUTHORITY=files`). A variable that
//!   decides which artifact holds the ceilings is a variable that can widen them — the environment
//!   layer `Policy` is sealed against ([`crate::layers`]' sealed `EnvOverride`/`CliOverride`)
//!   wearing a different carrier. **This module reads no environment and adds no `env::var` call
//!   anywhere**, so `crates/vike-ops/tests/settings_registry.rs` gains no row and its `LIBRARY_PIN`
//!   does not grow.
//! * **A per-FILE probe** (*"if `policy.toml` is absent, the policy rows answer"*). Four probes,
//!   answerable differently per section, half-and-half on a half-deleted box — the per-key chain
//!   wearing a section's clothes.

use vike_secrets::{Adoption, DbError, SettingsSource, StoredSettings};

/// **The settings store, as the loader takes it** — the ARM, never an `Option`.
///
/// ⚠ **The `Option<&StoredSettings>` this replaced threw away the distinction the probe needs**,
/// and it did so at FIVE call sites that each independently rewrote a `DbError` into
/// *"there is no database"*: `vike-boot`, `vike-cli`'s `config show` and `config check`, and
/// `vike-tradehub`'s `SettingsShow`. `vike_secrets::SettingsSource` carried the arms the whole time
/// and `rows()` collapsed them one line later, so nothing about WHICH state answered survived into
/// the resolved [`crate::Settings`].
///
/// Three of those states are value-identical no-ops today and the other two differ only by a
/// warning string, which is exactly why the collapse was invisible. The day the files retire they
/// stop being equivalent: *never mirrored*, *tables absent*, *empty tables*, *previous encoding*
/// and *unreadable schema* would all resolve to no ceiling and every venue `paper`, and no caller
/// could refuse on a distinction it cannot see.
#[derive(Debug, Clone, Copy)]
pub enum StoreLayer<'a> {
    /// The tables exist; these are their rows. `adopted` is the seal, read from the same open.
    Rows {
        /// The two tables' rows.
        rows: &'a StoredSettings,
        /// The seal. `None` is the ordinary state and means the FILES answer.
        adopted: Option<&'a Adoption>,
    },
    /// No settings database on this box at all. The files answer.
    NoDatabase,
    /// A database that predates the settings tables. The files answer.
    TablesAbsent,
    /// **The store could not be read** — a `DbError`, a corrupt file, or a schema outside
    /// `vike_secrets::READABLE_SCHEMA_VERSIONS`.
    ///
    /// ⚠ This is NOT *"so use the files"*. Whether this box is ADOPTED is itself a fact IN the
    /// store, so a store that will not open cannot answer *am I the authority here* — and falling
    /// through to the files on that silence is the ladder re-entering through the error path. The
    /// tree already has this shape one module over: [`crate::arming`]'s
    /// `LiveArmingVerdict::Undetermined`, whose own doc says conflating *the sources all answered
    /// NO* with *one could not be consulted* is the defect a second source exists to repair.
    ///
    /// The DISPOSITION is: resolve WITHOUT the store layer and MARK the result on
    /// [`crate::Settings::store_refusal`]. No root refuses to start on it, and the measurement that
    /// settled that — an unreadable store means an empty CREDENTIAL map, hence an all-paper mount,
    /// because 0054 puts both in one database — is carried at [`crate::load_with_source`]'s own arm.
    /// The enforcement point is `vike-cli config check`'s `Level::Fail`, which fires at a deploy
    /// pre-flight rather than at a running daemon.
    Unreadable(&'a str),

    /// **This caller resolves no store, and says why.**
    ///
    /// Load-bearing rather than a convenience: it is what keeps a store-blind caller compiling
    /// while forcing it to DECLARE what was previously an anonymous `None`. It is `vike-boot`'s
    /// `BootSpec` idiom applied one crate down, and it is how the surviving store-blind callers say
    /// so in their own diffs.
    NotConsulted(&'static str),
}

impl<'a> StoreLayer<'a> {
    /// The arm for a `vike_secrets::read_settings_in` result the caller is holding.
    ///
    /// ONE mapping, so the five hand-rolled `Err` → *"no database"* rewrites this replaced cannot
    /// come back one call site at a time. `None` is a caller with no settings directory, which has
    /// no database by construction.
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

    /// **WHICH SOURCE answers for this run** — the probe, and the only place it is asked.
    ///
    /// [`Authority::Store`] for an adopted store and for nothing else. An UNREADABLE store answers
    /// [`Authority::Files`] here, and that is a REPORTED unknown rather than a fall-through: the
    /// resolution that follows carries [`crate::Settings::store_refusal`], which says in as many
    /// words that these values are not necessarily what a daemon on this box would resolve.
    #[must_use]
    pub fn authority(&self) -> Authority {
        match self.adoption() {
            Some(_) => Authority::Store,
            None => Authority::Files,
        }
    }
}

/// **Which source answered for every settings key on this process.**
///
/// A resolved FACT about this box and this run, carried on [`crate::Settings`] so that nothing
/// downstream has to re-derive it from a path — and so `vike-cli config show`'s precedence header
/// is a statement about THIS box rather than a hand-typed sentence naming a layer nobody reads,
/// which is how the retired per-project file came to be advertised for two months.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Authority {
    /// The four settings files answer; a store, if any, is read BELOW them (0057 Phase 1's mirror).
    /// **The default, and the state of every box until an operator runs `vike-cli config adopt`.**
    #[default]
    Files,
    /// The settings database answers for every key, and the four files are not opened for
    /// resolution at all.
    Store,
}

impl Authority {
    /// The stable machine word, for `--json` and for a log line.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Authority::Files => "files",
            Authority::Store => "db",
        }
    }
}

impl std::fmt::Display for Authority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Authority::Files => "the four settings files",
            Authority::Store => "the settings database",
        })
    }
}
