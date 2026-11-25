//! The profile store's error type and its operator-facing messages.

use super::*;
use crate::db::StoreError;

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// A profile-store failure. **Names a profile, a mount and a venue — never a credential**, which is
/// structural here rather than a promise: no statement in this module selects from `credential`,
/// `account` or `hub_key`.
#[derive(Debug)]
pub enum ProfileStoreError {
    /// The store itself could not be opened or read. Forwarded verbatim from `crate::db`.
    Db(StoreError),
    /// A stored `kind` this code does not read.
    ///
    /// ⚠ **This said "including `recorder`, which 0057 rules STAYS a file" and had been wrong since
    /// 2026-09-16**, when the owner overruled that NO and [`ProfileKind::parse`]'s refusal arm was
    /// deleted — see that function's own doc. The three words the schema's `CHECK` admits
    /// (`daemon`, `run`, `recorder`) are exactly the three [`ProfileKind`] parses, so this variant
    /// is now reachable only from a kind word the CHECK would itself have refused: a store written
    /// by an older schema, a hand `INSERT`, or a future word this binary predates.
    UnreadableKind {
        /// The word found in the column.
        kind: String,
    },
    /// A write would have made a second row of one kind active. The schema refuses it too; this is
    /// the message an operator can act on.
    TwoActive {
        /// The kind that would have held two.
        kind: ProfileKind,
        /// The name that already holds it.
        held_by: String,
    },
    /// **A write was attempted against a store that does not exist, and creating one is REFUSED.**
    ///
    /// ⚠ This is a live-gate refusal, not tidiness. `crate::store::StoreBackend` decides which store
    /// answers from ONE `stat` on this path, so bringing an empty database into existence makes
    /// every credential read answer from it — and an empty map is not an error downstream, it is the
    /// LIVE GATE (`crate::db`'s module doc). Creating the store is the operator's explicit act, so a
    /// profile write refuses and names the one command that may create it.
    NoStore {
        /// The database path that is absent.
        path: std::path::PathBuf,
    },
    /// A caller handed [`profile_ddl`] an asset-class word it will not render into a `CHECK`.
    ///
    /// Not a defensive flourish: this module spells NO asset-class word of its own (see
    /// [`profile_ddl`]'s doc — which since 0072 is a matter of DESIGN rather than of what the
    /// crate may name, and this variant is one of the three reasons the ruling there gives), so
    /// the vocabulary arrives as caller data and the only thing standing between it and a SQL
    /// clause is this refusal. The legitimate caller hands
    /// over `vike_model::AssetClass::SQL_WORDS`, every member of which is a bare identifier off a
    /// closed enum; anything else is a bug in the caller, and rendering it would be a bug in the
    /// schema.
    UnrenderableVocabulary {
        /// The offending word, or empty when the whole vocabulary was empty.
        word: String,
    },
    /// **The name is already held by a profile of a DIFFERENT kind, and the body write is
    /// REFUSED.**
    ///
    /// ⚠ This is a destruction refusal, not a tidiness one, and it is the twin of a guard the
    /// SELECTION verb has had since it was written —
    /// `crates/vike-cli/src/cmd/config/profile/activate.rs` refuses to activate a name stored under
    /// another kind. That the STORE verb did not was an asymmetry, not a decision. `profile.name`
    /// is `TEXT PRIMARY KEY`: **ONE namespace across all three kinds**, not one per kind.
    /// [`store_profile`] replaces a body by `DELETE FROM profile WHERE name = ?1` + `INSERT`, the
    /// `DELETE` cascades `mount`/`mount_param`/`profile_setting`/`recorder`/`subscription` away,
    /// and the `active` bit is deliberately PRESERVED across that replacement. Without this
    /// refusal, storing a `run` body under a name an ACTIVE `recorder` row holds would destroy the
    /// recorder's subscriptions AND hand the new body an `active = 1` it was never activated with
    /// — arming the run plane with no deliberate `vike-cli config profile activate` in front of it, which is
    /// the one act this whole phase rests on.
    NameHeldByAnotherKind {
        /// The name both bodies want.
        name: String,
        /// The `kind` word the row already there carries. A raw word rather than a [`ProfileKind`]:
        /// a row written by a schema this binary predates holds the name just as firmly as one it
        /// can parse, so the refusal may not depend on parsing it.
        held: String,
        /// The kind the refused write would have stored the body as.
        wanted: ProfileKind,
    },
    /// **ONE command would store two bodies of DIFFERENT kinds under ONE name, and it is refused
    /// before either is written.**
    ///
    /// ⚠ This is [`ProfileStoreError::NameHeldByAnotherKind`] reached from a command line instead of
    /// from the store, and it exists because the store CANNOT see it coming: a caller that writes
    /// several bodies writes them one at a time, so the first `store_profile` call is a legal write
    /// against a store that does not hold the name yet, and the SECOND is what the
    /// already-held refusal fires on — by which point the first body has landed and any "nothing
    /// was written" the operator reads is false. The retired
    /// `config mirror` was the measured instance: both of its halves defaulted to the name
    /// `default`, so they collided with no name typed twice.
    ///
    /// Nothing in this module constructs it — one [`store_profile`] call carries one body, so the
    /// store has no second name to compare against. It lives here rather than in the CLI so the two
    /// cross-kind refusals share the rule and the repair they both rest on
    /// (`NAME_IS_ONE_NAMESPACE`, `CHOOSE_ANOTHER_NAME`) instead of spelling them twice.
    NameWantedByTwoKinds {
        /// The name both halves of the one command want.
        name: String,
        /// The kind the FIRST half would store it as — first in the caller's own write order, so a
        /// reader can tell which body would have been the one destroyed.
        first: ProfileKind,
        /// The kind the SECOND half would store it as.
        second: ProfileKind,
    },
    /// A delete named a profile the store does not hold AS THAT KIND (see [`delete_verdict`]).
    NoSuchProfile {
        /// The name the delete was given.
        name: String,
        /// The kind the delete was given.
        kind: ProfileKind,
        /// The raw `kind` word of the profile that DOES hold the name, when another kind does.
        held: Option<String>,
    },
    /// A delete named the ACTIVE profile of its kind, which is never deleted (see
    /// [`delete_verdict`]).
    DeleteActive {
        /// The active profile's name.
        name: String,
        /// Its kind.
        kind: ProfileKind,
    },
    /// [`update_profile`]'s edit refused the body it read under the write lock; the words are the
    /// edit's own, and nothing was written.
    EditRefused(String),
}

/// **The rule both cross-kind refusals rest on, spelled ONCE.**
///
/// [`ProfileStoreError::NameHeldByAnotherKind`] and [`ProfileStoreError::NameWantedByTwoKinds`] are the same
/// destruction reached from two directions — a name the store already holds, and a name one command
/// asks for twice — so the sentence that says WHY is shared rather than copied. A second copy is
/// how the two rungs start telling an operator different things about one schema fact.
const NAME_IS_ONE_NAMESPACE: &str = "`profile.name` is ONE namespace across every kind, not one per kind, and storing a body \
     REPLACES whatever is there";

/// **The repair both cross-kind refusals name, spelled ONCE.** See `NAME_IS_ONE_NAMESPACE`.
const CHOOSE_ANOTHER_NAME: &str = "Store it under a different name — `--profile-name` / `--daemon-name` / `--recorder-name` \
     choose one, and without any of them the name is derived from the file's own stem, which is \
     how two unrelated documents both called `default` collide.";

impl std::fmt::Display for ProfileStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileStoreError::Db(e) => write!(f, "{e}"),
            ProfileStoreError::UnreadableKind { kind } => write!(
                f,
                "the settings store holds a profile of kind {kind:?}, which this binary does not \
                 read. The kinds it reads are `daemon`, `run` and `recorder`, which is also what \
                 the schema's own CHECK admits — so this row was written by a schema this binary \
                 predates, or by hand. The whole profile read stops here rather than skipping the \
                 row, because a profile set with one entry silently missing is the shape an \
                 operator reads as complete"
            ),
            ProfileStoreError::NoStore { path } => write!(
                f,
                "there is no settings database at {} and a profile write will not create one. \
                 Creating the store makes it the store that ANSWERS for credentials — EMPTY, every \
                 venue on paper — so it is the operator's act: `vike-cli secrets init` creates it EMPTY (credentials \
                 are then added with `vike-cli secrets set`), and it is the one command that may \
                 bring this file into existence",
                path.display()
            ),
            ProfileStoreError::TwoActive { kind, held_by } => write!(
                f,
                "profile kind `{}` already has an active row (`{held_by}`), and exactly one may be \
                 active. Deactivate that one first — an active profile is what this box trades, and \
                 two of them is not a state a resolver can be asked to break a tie in",
                kind.sql_word()
            ),
            ProfileStoreError::UnrenderableVocabulary { word } if word.is_empty() => write!(
                f,
                "the mount schema's asset-class vocabulary arrived EMPTY. This module spells no \
                 asset-class word of its own by design — the caller hands over \
                 `vike_model::AssetClass::SQL_WORDS` — and an empty CHECK would refuse every \
                 mount row there is"
            ),
            ProfileStoreError::UnrenderableVocabulary { word } => write!(
                f,
                "the asset-class word {word:?} is not a bare alphanumeric identifier and will not \
                 be rendered into a SQL CHECK. The vocabulary must be \
                 `vike_model::AssetClass::SQL_WORDS`, which is generated from that enum's single \
                 declaration"
            ),
            ProfileStoreError::NameHeldByAnotherKind { name, held, wanted } => write!(
                f,
                "the settings store already holds a `{held}` profile called `{name}`, and this \
                 would store a `{w}` one under that same name. NOTHING WAS WRITTEN.\n\n\
                 {NAME_IS_ONE_NAMESPACE}: the `{held}` body and every mount, parameter, \
                 setting and subscription hanging off it would be deleted, and the new `{w}` body \
                 would INHERIT the `active` bit the `{held}` row held — arming it as a kind nobody \
                 ever activated it as, with no deliberate `vike-cli config profile activate` in front of it. \
                 That is the one act this store makes deliberate, so it is refused here rather \
                 than performed.\n\n\
                 {CHOOSE_ANOTHER_NAME} If the `{held}` body is the one not wanted, \
                 `vike-cli config profile rm {held} {name} --yes` frees the name (it refuses \
                 the active profile of its kind: activate another one first).",
                w = wanted.sql_word(),
            ),
            ProfileStoreError::NoSuchProfile { name, kind, held: None } => write!(
                f,
                "the settings store holds no `{}` profile called `{name}`. NOTHING WAS DELETED.",
                kind.sql_word()
            ),
            ProfileStoreError::NoSuchProfile { name, kind, held: Some(held) } => write!(
                f,
                "the settings store holds `{name}` as a `{held}` profile, not a `{}` one. NOTHING \
                 WAS DELETED: a delete names the kind the body is stored as, so a mistyped kind \
                 cannot take another kind's body.",
                kind.sql_word()
            ),
            ProfileStoreError::DeleteActive { name, kind } => write!(
                f,
                "`{name}` is the ACTIVE `{k}` profile, the body this box reads at its next start, \
                 and the active profile is never deleted. NOTHING WAS DELETED. Point the box \
                 elsewhere first (`vike-cli config profile activate {k} <another-name>`, or \
                 `vike-cli config profile deactivate {k}`), then delete it.",
                k = kind.sql_word()
            ),
            ProfileStoreError::NameWantedByTwoKinds { name, first, second } => write!(
                f,
                "this one command would store BOTH a `{a}` profile and a `{b}` profile called \
                 `{name}`. NOTHING WAS WRITTEN.\n\n\
                 {NAME_IS_ONE_NAMESPACE}: the `{b}` half would be REFUSED by the store — \
                 `store_profile` guards the cross-kind case — but only once the `{a}` half had \
                 ALREADY COMMITTED, leaving that body on disk under a refusal whose headline says \
                 nothing was written. ⚠ It is the surviving row under a false headline that is the \
                 harm here, not a deletion: the store's guard is what prevents the delete and the \
                 inherited `active` bit. It is refused HERE, before either half is planned, \
                 because the store cannot see it coming — a body write carries ONE name, so the \
                 first half is a legal write and the second is where it would be caught.\n\n\
                 {CHOOSE_ANOTHER_NAME}",
                a = first.sql_word(),
                b = second.sql_word(),
            ),
            ProfileStoreError::EditRefused(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for ProfileStoreError {}

impl From<StoreError> for ProfileStoreError {
    fn from(e: StoreError) -> Self {
        ProfileStoreError::Db(e)
    }
}
