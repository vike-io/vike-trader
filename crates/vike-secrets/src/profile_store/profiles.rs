//! The loaded profile set: `Profiles`, `ActiveProfile` and the active-profile resolution.

use super::*;

impl StoredProfile {
    /// **THE ONE PRIMARY RESOLUTION.** Every consumer asks this rather than indexing, so a table's
    /// lack of order cannot be answered two different ways in two places.
    #[must_use]
    pub fn primary(&self) -> Primary {
        if let Some(m) = self.mounts.iter().find(|m| m.is_primary) {
            return Primary::Declared(m.ord);
        }
        match self.mounts.iter().map(|m| m.ord).min() {
            Some(ord) => Primary::ImplicitFirst(ord),
            None => Primary::NoMounts,
        }
    }

    /// The mount row [`Self::primary`] names, if there is one.
    #[must_use]
    pub fn primary_mount(&self) -> Option<&MountRow> {
        let ord = match self.primary() {
            Primary::Declared(o) | Primary::ImplicitFirst(o) => o,
            Primary::NoMounts => return None,
        };
        self.mounts.iter().find(|m| m.ord == ord)
    }
}

// ⚠ THERE IS DELIBERATELY NO `refuse_unrunnable_primary` HERE, and the absence is the design.
//
// A first draft gave [`StoredProfile`] its own refusal for a primary naming a venue the node runs
// no engine for, in PR #1866's vocabulary. It had to go, for the reason #1866 itself is about:
// **two spellings of one refusal teach an operator to read two different faults into one
// situation.** This crate has no idea which ENGINES a node runs — that is a mount-layer fact and
// `vike-mount` is far above it — so its copy could only ever be a second rendering of a sentence
// whose authority lives there.
//
// ⚠ This read "This crate is a leaf with no `vike-*` dependency and no idea what a venue IS", and
// both clauses are now false: `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md`
// (accepted 2026-09-20) admitted `vike-model`, and `crate::db`'s `ensure_venue_rows` iterates
// `vike_model::VENUES` outright — this crate knows exactly what a venue is. The argument
// survives untouched because it never needed either clause: knowing the ROSTER is not knowing which
// venues THIS NODE runs an engine for, which is the question the refusal answers.
//
// The check survives and is stronger for the move: `vike_tradehub::profile_rows`'s
// `rows_to_daemon_profile` materialises stored rows back through `DaemonProfile::from_toml_str`, so
// a row-loaded profile reaches `vike_tradehub::config::DaemonProfile::refuse_unrunnable_primary` —
// the SAME function a file-loaded profile reaches, calling the SAME `no_engine_refusal` that
// `vike_tradehub::server::refusal::venue_refusal` calls on the order path. One sentence, one implementation,
// three call sites.

/// Every profile the store holds, plus the fact of whether it holds any at all.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Profiles {
    pub(super) profiles: Vec<StoredProfile>,
    pub(super) tables_present: bool,
}

/// What [`Profiles::resolve_active`] found: the active row, or WHICH of the three noes this box is
/// in.
///
/// ⚠ **The noes are separated because they name three different next commands**, and a refusal that
/// cannot tell them apart sends an operator to the wrong one — there is no settings database to
/// hold a profile at all, or the store holds none of this kind, or it holds some and none of them
/// is selected. Only the last is a state an operator caused, and only the last has a fix that does
/// not involve writing a profile first.
///
/// ⚠ **"No database" and "a database written before the profile tables existed" are ONE variant
/// here, deliberately.** [`read_profiles`] collapses those two into [`Profiles::none`] — the
/// arming-preservation property this module's doc opens with — so by the time a `Profiles` exists
/// nothing this crate can observe tells them apart, and splitting the variant would be inventing a
/// distinction the read path has already thrown away.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ActiveProfile<'a> {
    /// The one active row of this kind.
    Row(&'a StoredProfile),
    /// No profile tables were read at all — either no settings database, or one that predates them.
    NoProfileStore,
    /// The profile tables are present and hold NO profile of this kind, active or not.
    NoneStored,
    /// Profiles of this kind ARE stored, and none of them carries `active`.
    NoneActive {
        /// How many profiles of this kind the store holds. A refusal can name the count, and a
        /// caller that wants the NAMES reads them off [`Profiles::all`] — this enum deliberately
        /// borrows no list it would then have to keep in step.
        stored: usize,
    },
}

impl Profiles {
    /// **The answer for every box that has not migrated, and for every box that has no database.**
    /// Distinct from "migrated and empty" only through [`Self::tables_present`]; every RESOLVER
    /// treats the two identically, which is the arming-preservation property.
    #[must_use]
    pub fn none() -> Self {
        Profiles { profiles: Vec::new(), tables_present: false }
    }

    /// Do the profile tables exist in this store? A reporting fact, never a resolution input.
    #[must_use]
    pub fn tables_present(&self) -> bool {
        self.tables_present
    }

    /// Every stored profile, in name order.
    #[must_use]
    pub fn all(&self) -> &[StoredProfile] {
        &self.profiles
    }

    /// **THE ONE RESOLUTION OF "WHICH PROFILE OF THIS KIND DOES THIS BOX USE BY DEFAULT" — and,
    /// when there is none, WHICH of the three noes it is.**
    ///
    /// # The defect this function exists to prevent
    ///
    /// Two callers answering this question separately. The operator's whole mental model is that
    /// the profile the CLI calls the default IS the profile the daemon reads, and there are exactly
    /// two sides for that belief to be true or false between:
    ///
    /// * the **CLI** — `crates/vike-cli/src/cmd/config/mirror_recorder.rs`'s `plan`, which writes
    ///   recorder rows and reports what the store already selects;
    /// * the **daemon** — `crates/vike-datahub/src/recorder/profile.rs`'s `load_and_check_profile_row`,
    ///   which loads the row the recorder actually mounts.
    ///
    /// Two `iter().find()`s in two crates make the agreement a coincidence rather than a property:
    /// the first side that grows a tie-break, a name fallback or a kind filter of its own stops
    /// agreeing, and nothing anywhere goes red — the CLI prints a name and the daemon records
    /// something else. So the resolution is written ONCE, here, in the leaf crate both sides
    /// already depend on, and [`Self::active`] is this same answer with the reason dropped rather
    /// than a second computation of it.
    ///
    /// # ⚠ What happens when two rows of one kind carry `active`
    ///
    /// MEASURED against the schema rather than assumed. [`profile_ddl`] creates
    /// `profile_one_active_per_kind` — a UNIQUE index on `(kind)`, partial `WHERE active = 1` — and
    /// every write function in this module executes that DDL before it writes, so a STORE cannot
    /// hold the state and [`read_profiles`] cannot produce it. The one constructor that can is
    /// [`Profiles::from_rows`], which takes rows a caller invented for a rendering test.
    ///
    /// The schema is therefore the constraint, and this function is deterministic ANYWAY: it
    /// answers with the LOWEST NAME, never the first row it happens to meet. `read_profiles` sorts
    /// by name, so the two readings agree there; `from_rows` does not sort, and an answer that
    /// depended on the order a caller pushed rows in would be a tie broken by luck.
    #[must_use]
    pub fn resolve_active(&self, kind: ProfileKind) -> ActiveProfile<'_> {
        let winner = self
            .profiles
            .iter()
            .filter(|p| p.row.kind == kind && p.row.active)
            .min_by(|a, b| a.row.name.cmp(&b.row.name));
        if let Some(row) = winner {
            return ActiveProfile::Row(row);
        }
        if !self.tables_present {
            return ActiveProfile::NoProfileStore;
        }
        match self.profiles.iter().filter(|p| p.row.kind == kind).count() {
            0 => ActiveProfile::NoneStored,
            stored => ActiveProfile::NoneActive { stored },
        }
    }

    /// **The ACTIVE profile of one kind, or `None`** — [`Self::resolve_active`] with the reason
    /// discarded, for the callers that only need the winner.
    ///
    /// ⚠ It is DEFINED in terms of the resolver rather than repeating its body, so the two cannot
    /// drift apart; it is a projection, not a second resolution. `None` is what every box answers
    /// today, and `None` is what every caller must treat as "nothing here selects anything" — but a
    /// caller that has to SAY WHY it got `None` must ask the resolver instead, because the three
    /// reasons name three different next commands.
    #[must_use]
    pub fn active(&self, kind: ProfileKind) -> Option<&StoredProfile> {
        match self.resolve_active(kind) {
            ActiveProfile::Row(p) => Some(p),
            ActiveProfile::NoProfileStore
            | ActiveProfile::NoneStored
            | ActiveProfile::NoneActive { .. } => None,
        }
    }

    /// One profile by name, whatever its kind.
    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<&StoredProfile> {
        self.profiles.iter().find(|p| p.row.name == name)
    }

    /// Assemble a `Profiles` from rows a caller already has — **for rendering tests, and it says
    /// `tables_present`.**
    ///
    /// ⚠ It exists because a consumer that RENDERS these rows (`vike-cli config recorder`) must be
    /// able to test its own wording without standing up a SQLite file, and the alternative was each
    /// such consumer growing a fixture that migrates a credential store to print one line. It
    /// cannot be mistaken for a read: nothing in this module calls it, no resolver takes a
    /// `Profiles` from anywhere but [`read_profiles`], and every field it fills is already `pub` on
    /// [`StoredProfile`].
    ///
    /// ⚠ It reports `tables_present() == true`, deliberately: the one thing a caller must NOT be
    /// able to fabricate through it is [`Profiles::none`]'s answer, which is the
    /// arming-preservation signal every box that has not migrated gives. `Profiles::none()` is the
    /// constructor for that and stays the only one.
    #[must_use]
    pub fn from_rows(profiles: Vec<StoredProfile>) -> Self {
        Profiles { profiles, tables_present: true }
    }
}
