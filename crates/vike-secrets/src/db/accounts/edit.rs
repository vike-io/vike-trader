//! The account lifecycle writer: `AccountEdit`, `AccountWrite`, `edit_account` and its guards.

use super::model::{account_from_row, account_select};
use super::*;
#[cfg(doc)]
use crate::db::open::BUSY_TIMEOUT;

// ---------------------------------------------------------------------------------------------
// Writing — the account LIFECYCLE
// ---------------------------------------------------------------------------------------------

// ⚠ `ACCOUNT_LABEL_MAX_LEN` and `RESERVED_ACCOUNT_LABEL` USED TO BE DECLARED HERE, as a second
// spelling of `vike_model::accounts::account_keys::MAX_LABEL_LEN` and `RESERVED_DEFAULT_LABEL`. The only
// reason the doc ever gave for the duplication was *"this crate declares no `vike-*` dependency at
// all … so the label grammar cannot be imported here"*, and
// `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (accepted 2026-09-20)
// made that false: the manifest declares `vike-model`. The two constants are IMPORTED at the top of
// this file and the local names are gone rather than kept as aliases — a symbol has one name.
//
// What the collapse costs is one assertion. `crates/vike-bridge-core/tests/account_label_spellings.rs`
// held the two numbers equal; with one declaration that comparison cannot fail, so it was deleted
// there rather than left reading like coverage — the same disposition
// `crates/vike-bridge-core/tests/settings_dir_spellings.rs` records for the resolver merge. The
// drift it guarded is still caught: that file's remaining PREDICATE test builds its at-cap and
// over-long cases from `MAX_LABEL_LEN` and requires `AccountLabel::parse` and
// [`normalized_account_label`] to answer identically on every one, so a store floor that stopped
// agreeing with the authority above it is still a red test.

/// **The store's own floor under an account label** — `Some(label)` when it is one this store will
/// write, `None` when it is not.
///
/// A-Z and 0-9 only, 1..=`vike_model::accounts::account_keys::MAX_LABEL_LEN` characters, never
/// `vike_model::accounts::account_keys::RESERVED_DEFAULT_LABEL`.
///
/// ⚠ **This floor STAYS, and 0072 did not touch it.** `AccountLabel::parse` is the AUTHORITY — it
/// is what every operator-facing surface validates through, and its refusals name the rule that was
/// broken — and this is the store's own floor under it, exactly as [`normalized_venue_account_id`]
/// is the floor under whatever a caller thinks a book looks like. 0072 admitted an EDGE; it ruled
/// nothing about collapsing a validation layer into the one above it, and a store that trusted its
/// callers to have validated would be a store whose `CHECK`s are the only thing left.
///
/// ⚠ **It REPAIRS nothing, not even case.** `alt` is refused rather than uppercased, for the reason
/// `vike_model::accounts::account_keys::AccountLabel::parse` refuses it: a spelling this store fixed on the
/// operator's behalf is a spelling nobody learns, and the label they then write into
/// `policy.accounts.<venue>.<LABEL>` would match no row. That is a stricter rule than
/// [`normalized_venue_account_id`]'s, which trims invisible characters at the edges — a book is a
/// number read off a venue's page and pasted, a label is a name somebody chose.
#[must_use]
pub fn normalized_account_label(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.len() > MAX_LABEL_LEN {
        return None;
    }
    if raw.chars().any(|c| !c.is_ascii_uppercase() && !c.is_ascii_digit()) {
        return None;
    }
    if raw == RESERVED_DEFAULT_LABEL {
        return None;
    }
    Some(raw.to_string())
}

/// **WHICH act on the `account` table** — the parameter that keeps the lifecycle ONE writer.
///
/// ⚠ **A parameter rather than four functions, and that is the load-bearing choice.**
/// `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs` pins the SET of function names that write this
/// store, and its `GROWTH_GUIDANCE` states the rule at exactly this shape: *"a SECOND FUNCTION for
/// a second column is the shape to refuse here: this one grew a parameter instead."* That was
/// written about [`BookSource`] growing onto [`set_venue_account_id`]; this enum is the same answer
/// one size up. A `pub fn` per act would be a name per act for the gate to learn, a transaction per
/// act to keep in step, and a place per act for the refusals below to diverge — and the count is
/// deliberately not written down here, because it has already moved once ([`AccountEdit::SetTier`]
/// made four acts five) and a number in this paragraph would be the thing that rots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountEdit<'a> {
    /// **ADD a row.** `label` is `None` for the unlabelled account a venue's plain keys address,
    /// and a second unlabelled row at one `(venue, tier)` is REFUSED —
    /// [`DbErrorKind::AmbiguousUnlabelledAccount`].
    ///
    /// ⚠ **A row is not a CEILING.** `policy.venues.<venue>` is consulted ABOVE the credential read
    /// by `vike_mount::make_engine`, so an account plus its keys leaves the venue on PAPER until a
    /// policy edit. Creating one arms nothing, and a caller's reply must say so.
    Create {
        /// A `vike_model::VENUES` id. ⚠ **NOT validated here**, and the disposition is deliberate:
        /// `account.venue` is a bare `TEXT` column precisely because this verb does not police the
        /// roster (the spec's §2.3 and `crates/vike-secrets/tests/gates/store_link.rs` both rest on
        /// that). Every operator-facing caller validates against `vike_model::VENUES` before it
        /// gets here; what the store enforces is the schema's `CHECK` on the tier and the guards
        /// below on the label.
        ///
        /// ⚠ **The REASON this used to give was false** — *"this crate declares no `vike-*`
        /// dependency, so the roster is not reachable"*. That edge was admitted by
        /// `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` (accepted
        /// 2026-09-20), and [`ensure_venue_rows`] a thousand lines below iterates
        /// `vike_model::VENUES` in this very file. The roster IS reachable; it is simply
        /// not consulted here.
        ///
        /// ⚠ **And the CORRECTION itself carried a false detail until the branch review caught
        /// it:** it read *"which also moved this crate from layer 15 to 10"*. 0072 moved no crate.
        /// MEASURED — `crates/vike-secrets/Cargo.toml` declares `layer = 15` and
        /// `crates/vike-model/Cargo.toml` declares `10` — so 0072's *"Layers 15 → 10, strictly
        /// down"* names the EDGE's direction, which is what `crates/vike-ops/tests/architecture/layer_gate.rs`
        /// asks of a normal dependency. `crates/vike-secrets/src/schema.rs`'s module doc had
        /// already been corrected to exactly this; the two sites came from one hand-off and only
        /// one of them was swept, so the crate stated both at once. `one_authority_gate` is
        /// markdown-only and cannot see a contradiction between two Rust doc comments.
        venue: &'a str,
        /// One of [`crate::schema::ACCOUNT_TIERS`] — refused by the table's own `CHECK` otherwise.
        tier: &'a str,
        /// The operator's name for the ROLE, or `None` for the unlabelled account.
        label: Option<&'a str>,
    },
    /// **CHANGE one row's label**, and nothing else. Not `id`, not the key prefixes (those are
    /// derived from `credential.name`, which this never touches), not `venue_account_id`.
    ///
    /// ⚠ Refused outright when the row's own credential keys SPELL the old label —
    /// [`DbErrorKind::AccountKeysPinTheLabel`].
    Rename {
        /// The row, by [`Account::id`].
        id: i64,
        /// The new label, or `None` to clear it back to the unlabelled state (which is refused if
        /// that would make a second unlabelled row at this `(venue, tier)`).
        label: Option<&'a str>,
    },
    /// **DEACTIVATE or re-activate a row** — `account.active`, the reversible act, and the one a UI
    /// leads with.
    ///
    /// [`Accounts::active_for_venue`] filters on this column, so every consumer already reads a
    /// deactivated row exactly as it would read a deleted one; and `account_one_account_per_book`'s
    /// `WHERE active = 1` frees the BOOK while the row survives as evidence.
    ///
    /// ⚠ **Two residuals a caller must state rather than let an operator discover.** The arming
    /// snapshot is read ONCE at boot (`vike_mount`'s `AccountDirectory`, carried on
    /// `MountPolicy`), so deactivating an account while a daemon runs changes nothing until it
    /// restarts — the running engines keep their credentials and keep trading. And
    /// `vike-cli secrets confirm`'s fold silently keeps a parked confirmation whose address matches
    /// no active row, which is indistinguishable from what a re-migration looks like.
    SetActive {
        /// The row, by [`Account::id`].
        id: i64,
        /// `false` = the operator no longer uses this account.
        active: bool,
    },
    /// **MOVE a row to another tier** — `account.tier`, and nothing else. Not the label, not the
    /// id, not `venue_account_id`, and above all **not the credential key NAMES**, which is the
    /// whole source of this variant's one interesting refusal.
    ///
    /// The act exists because `account.tier` is otherwise write-once: a row is minted either by the
    /// migration reading a key NAME or by [`AccountEdit::Create`], and an operator who picked the
    /// wrong `--tier` on that create had no way back that did not go through `Remove` — which is
    /// refused the moment the row owns a credential, so the only cure was to delete the credentials
    /// first. `Rename` is the sibling act on the OTHER half of `UNIQUE (venue, tier, label)`.
    ///
    /// ⚠ **Refused when the row's own credential keys SPELL a different tier** —
    /// [`DbErrorKind::AccountKeysPinTheTier`], the exact twin of
    /// [`DbErrorKind::AccountKeysPinTheLabel`] and for the same mechanism one column along: nothing
    /// here rewrites a credential NAME, so `BINANCE_LIVE_API_KEY` keeps spelling `LIVE` after the
    /// move, and `crate::schema::AccountResolver` would derive `live` out of it again and CREATE A
    /// SECOND ROW the next time that key is written.
    ///
    /// ⚠ **Refused for a tier outside [`crate::schema::ACCOUNT_TIERS`]**
    /// ([`DbErrorKind::AccountTierUnknown`]), BEFORE the store is opened. [`AccountEdit::Create`]
    /// leans on the table's own `CHECK` for that and this one cannot: the keys refusal above
    /// compares an account-tier word against the target, so a target outside the vocabulary would
    /// make every key read as a contradiction and the operator would be handed a refusal about
    /// their credentials for what is a typo in a flag.
    ///
    /// ⚠ **It moves the row across `UNIQUE (venue, tier, label)`**, so the destination tier's label
    /// guards apply exactly as they do on a create — including the ambiguity guard no index can
    /// make ([`DbErrorKind::AmbiguousUnlabelledAccount`]: moving an unlabelled row onto a tier that
    /// already has one plants `crate::SchemaRefusal::AmbiguousAccount` for the next credential
    /// write).
    ///
    /// ⚠ **It changes [`Account::armed`]**, which `crate::settings::fold_arming_into_accounts`
    /// recomputes on every commit: an account is armed exactly where the operator's arming mode
    /// names the tier the row CARRIES, so moving a row is moving it in and out of that answer. The
    /// mount does not read that column yet (see [`Account::armed`]), so this changes nothing a
    /// venue does today — and a caller's reply must say which of those two it is rather than
    /// letting an operator assume either.
    SetTier {
        /// The row, by [`Account::id`].
        id: i64,
        /// The destination tier, one of [`crate::schema::ACCOUNT_TIERS`]. ⚠ **The legacy `sim`
        /// spelling is NOT accepted here**, even though `crate::schema::account_tier_named` takes
        /// it as an INPUT word: nothing in this workspace writes `sim` any more, a migrated store's
        /// own `CHECK` refuses it, and silently storing `paper` for it would be this verb
        /// normalizing where [`AccountEdit::Create`] beside it does not.
        tier: &'a str,
    },
    /// **DELETE a row.** Refused while it still owns live `credential` rows, NAMING them by key
    /// name — [`DbErrorKind::AccountHasCredentials`].
    Remove {
        /// The row, by [`Account::id`].
        id: i64,
    },
}

impl AccountEdit<'_> {
    /// A short, stable word for the act — for a log line, a journal cell and a reply. Total by
    /// construction, so a new variant is a compile error rather than a record with a wrong verb.
    #[must_use]
    pub fn verb(&self) -> &'static str {
        match self {
            AccountEdit::Create { .. } => "create",
            AccountEdit::Rename { .. } => "rename",
            AccountEdit::SetActive { active: true, .. } => "activate",
            AccountEdit::SetActive { active: false, .. } => "deactivate",
            AccountEdit::SetTier { .. } => "set-tier",
            AccountEdit::Remove { .. } => "remove",
        }
    }
}

/// What [`edit_account`] did — the row before and after, so a caller can echo the change rather
/// than re-reading and hoping it is describing the same transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountWrite {
    /// The row as the write transaction FOUND it. `None` for an [`AccountEdit::Create`], which
    /// found nothing.
    pub before: Option<Account>,
    /// The row as it stands now. `None` for an [`AccountEdit::Remove`], which left nothing.
    pub after: Option<Account>,
    /// [`AccountEdit::verb`] for the act that was performed.
    pub verb: &'static str,
    /// `false` when the row already held exactly this state and no statement changed anything — a
    /// rename to the label already carried, a deactivate of an already-inactive row.
    ///
    /// Idempotent rather than an error, the same rule [`BookWrite::changed`] states: re-running a
    /// command is not a mistake, and refusing the second run would break a script that re-asserts
    /// a known state.
    pub changed: bool,
    /// The live credential key NAMES this row owned when the transaction looked — **names, never
    /// values**, from the same value-free statement [`read_account_keys`] uses.
    ///
    /// Carried on the RESULT rather than left for the caller to fetch, because the thing an
    /// operator has to check before deactivating an account is exactly this list, and a second
    /// read outside the transaction could describe a different row.
    pub keys: Vec<String>,
}

/// **The ONE writer of the `account` table's lifecycle columns** — create, rename, re-tier,
/// (de)activate, remove — in ONE transaction, told by [`AccountEdit`] which act it is performing.
///
/// [`set_venue_account_id`] is its sibling and writes a DIFFERENT column (`venue_account_id`, plus
/// `last_verified_at` under a handshake); the two are deliberately separate because that one
/// records a claim about a BROKER and these record a claim about an operator's own filing.
///
/// # What it never does
///
/// * **It never creates the settings database.** [`open_for_write`] creates a database when the
///   path is empty, so the `created` branch below UNLINKS what it made and refuses with
///   [`DbErrorKind::VanishedDatabase`] — the same invariant [`upsert_rows`] and
///   [`set_venue_account_id`] hold, and the reason is sharper here than anywhere: the mere
///   EXISTENCE of `<project>/settings/db/vike.db` is the whole of [`crate::store::Backend`]'s
///   per-run choice, so minting one would hand that box an EMPTY store in an act nobody asked
///   to be a migration. [`create_store`] stays the only creator.
/// * **It never reads a credential VALUE.** The one `credential` statement it issues is
///   `SELECT name FROM credential WHERE account_id = ?1 AND superseded_at IS NULL` — there is no
///   `value` column in it, so no refusal, no `Debug`, no log line and no reply reachable from
///   [`AccountWrite`] can be a credential.
/// * **It never touches `venue_account_id` or `last_verified_at`.** A rename changes a LABEL and
///   may change nothing else — that is the rule `crates/vike-model/src/accounts/account_confirmation.rs`'s
///   *the address is the KEY PREFIX, never the row id* exists to protect, and it is why the
///   labelled-keys case below is REFUSED rather than cascaded.
///
/// # IMMEDIATE, for [`set_venue_account_id`]'s reason verbatim
///
/// Every arm here is a read-then-write: the row is `SELECT`ed, the refusals are decided from what
/// it holds, and only then is the statement issued. A DEFERRED transaction acquires SHARED on that
/// `SELECT` and must PROMOTE to RESERVED at the write, and SQLite refuses that promotion with
/// `SQLITE_BUSY` **without consulting the busy handler at all** — so [`BUSY_TIMEOUT`] could never
/// cover it. IMMEDIATE takes RESERVED up front, which is also what makes the decision and the write
/// atomic against another writer rather than merely likely to be.
///
/// # What it refuses, and why each refusal is a refusal rather than a guess
///
/// * **a malformed label** ([`DbErrorKind::AccountLabelMalformed`]), echoing nothing;
/// * **an `id` no row carries** ([`DbErrorKind::NoSuchAccount`]) — never a create. A rename that
///   invented a row for a mistyped id is how an operator ends up editing a stranger;
/// * **a SECOND unlabelled row at one `(venue, tier)`**
///   ([`DbErrorKind::AmbiguousUnlabelledAccount`]) — the sharpest one, and the one no schema
///   constraint can make: it plants `crate::SchemaRefusal::AmbiguousAccount` for the next
///   credential key that arrives, so allowing it would be arming a refusal for a write nobody has
///   made yet;
/// * **a label another row of that `(venue, tier)` carries** ([`DbErrorKind::AccountLabelTaken`])
///   — `UNIQUE (venue, tier, label)` is the authority; this pre-check exists only so the refusal
///   can name the other row, the same two-layer shape [`DbErrorKind::BookHeldByAnother`] has;
/// * **a label another ACTIVE row of that venue already names as its BOOK**
///   ([`DbErrorKind::AccountLabelHeldAsBook`]) — no constraint enforces this one at all, and the
///   failure it prevents is a MOUNT refusing as ambiguous at the next restart;
/// * **a rename of a row whose credential keys SPELL the old label**
///   ([`DbErrorKind::AccountKeysPinTheLabel`]);
/// * **a tier that is no [`crate::schema::ACCOUNT_TIERS`] member**
///   ([`DbErrorKind::AccountTierUnknown`]), decided before the store is opened, so a typo costs no
///   lock — and, more to the point, so it cannot be reported as a refusal about credentials;
/// * **a RE-TIER of a row whose credential keys SPELL a different tier**
///   ([`DbErrorKind::AccountKeysPinTheTier`]) — the label refusal's mechanism one column along,
///   and the one guard in this function that has to cross two tier VOCABULARIES to ask its
///   question at all (that variant's doc is the authority on why);
/// * **a REMOVE of a row that still owns live credential rows**
///   ([`DbErrorKind::AccountHasCredentials`]), naming them by KEY NAME. The foreign key would
///   refuse it anyway — `credential.account_id REFERENCES account(id)` with no `ON DELETE`, under
///   an [`open_for_write`] that verifies `PRAGMA foreign_keys` took — but its words are
///   `FOREIGN KEY constraint failed`, which names nothing an operator can act on.
///
/// # Errors
/// [`DbError`] for every refusal above, for a store that will not open, and for a statement the
/// engine rejects. **Every one of them wrote nothing**: one transaction, rolled back whole.
pub fn edit_account(path: &Path, edit: AccountEdit<'_>) -> Result<AccountWrite, DbError> {
    let refuse = |kind| DbError { path: path.to_path_buf(), kind };
    // The label is validated BEFORE the store is opened, so a malformed one costs no lock and
    // leaves no journal file behind. `None` is a legitimate value on both arms that take one.
    let label: Option<String> = match &edit {
        AccountEdit::Create { label, .. } | AccountEdit::Rename { label, .. } => match label {
            Some(raw) => match normalized_account_label(raw) {
                Some(l) => Some(l),
                None => return Err(refuse(DbErrorKind::AccountLabelMalformed)),
            },
            None => None,
        },
        AccountEdit::SetActive { .. }
        | AccountEdit::SetTier { .. }
        | AccountEdit::Remove { .. } => None,
    };
    // The TIER is validated on the same rule and for the same two reasons: a word outside the
    // vocabulary costs no lock and leaves no journal file — and, peculiar to this verb, the keys
    // guard below compares an ACCOUNT_TIERS word against this one, so an unknown target would make
    // every credential key read as a contradiction and hand the operator a refusal about their
    // credentials for what is a typo in a flag. [`AccountEdit::Create`] leans on the table's own
    // `CHECK` instead, which it can afford to: nothing downstream of it compares tiers.
    if let AccountEdit::SetTier { tier, .. } = &edit
        && !crate::schema::ACCOUNT_TIERS.contains(tier)
    {
        return Err(refuse(DbErrorKind::AccountTierUnknown));
    }

    let (mut conn, created) = open_for_write(path)?;
    // ⚠ The ONLY safe act on this path is to put back what we found — see the doc's *never creates*
    // section. Close the engine BEFORE unlinking: an open handle keeps the file alive on Windows.
    if created {
        drop(conn);
        let _ = std::fs::remove_file(path);
        return Err(refuse(DbErrorKind::VanishedDatabase));
    }

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| DbError::sql(path, e))?;
    // ⚠ This verb never passes through `fill_into`, so it runs the write funnel itself — see
    // [`ensure_venue_rows`]'s own doc: the `Create` arm's INSERT looks its `venue_id` up in the
    // roster. Every statement below meets `venue_id` `NOT NULL` on every `account` row, which is why
    // the label guards and the re-activation's book check filter on the number alone
    // (`crate::schema::venue_is`).
    tx.execute_batch(crate::schema::DDL).map_err(|e| DbError::sql(path, e))?;
    ensure_venue_rows(&tx).map_err(|e| DbError::sql(path, e))?;

    let verb = edit.verb();
    let out = match edit {
        AccountEdit::Create { venue, tier, .. } => {
            guard_account_label(&tx, path, venue, tier, label.as_deref(), None)?;
            // `label` is written as supplied — `NULL` for the unlabelled account. `venue` and
            // `tier` are the caller's; the table's `CHECK (tier IN …)` is what refuses a tier
            // outside `ACCOUNT_TIERS`, and it arrives as a `DbErrorKind::Sqlite` naming the
            // constraint. `active` takes the column's `DEFAULT 1`; `venue_account_id`, `parent_id`,
            // `last_verified_at` and `notes` stay NULL — a new row knows nothing about its book,
            // and filling one in is `set_venue_account_id`'s job and nobody else's. The venue is
            // written through `crate::schema::VenueLink`, the way every other writer of the link
            // writes it: the number from the `venue` parameter in a sub-select.
            let link = crate::schema::VenueLink::of(&tx, "account", "a")
                .map_err(|e| DbError::sql(path, e))?;
            tx.execute(
                &format!(
                    "INSERT INTO account ({}, tier, label) VALUES ({}, ?2, ?3)",
                    link.columns,
                    link.values("?1")
                ),
                (venue, tier, label.as_deref()),
            )
            .map_err(|e| DbError::sql(path, e))?;
            let id = tx.last_insert_rowid();
            let after = account_row(&tx, path, id)?;
            AccountWrite { before: None, after, verb, changed: true, keys: Vec::new() }
        }
        AccountEdit::Rename { id, .. } => {
            let Some(before) = account_row(&tx, path, id)? else {
                return Err(refuse(DbErrorKind::NoSuchAccount { id }));
            };
            let keys = account_key_names(&tx, path, id)?;
            if before.label == label {
                // Already exactly this label. Not an error — see `AccountWrite::changed`.
                let out = AccountWrite {
                    after: Some(before.clone()),
                    before: Some(before),
                    verb,
                    changed: false,
                    keys,
                };
                return commit_account_write(tx, path, out);
            }
            // ⚠ THE LABELLED-KEYS REFUSAL. A key spelling `__{OLD}` keeps spelling it after a
            // rename — nothing here rewrites a credential name, and nothing in this workspace may —
            // so the classifier would derive the OLD label from it again and CREATE a second row.
            // See `DbErrorKind::AccountKeysPinTheLabel`.
            if let Some(old) = &before.label {
                let suffix = format!("__{old}");
                let pinned: Vec<String> =
                    keys.iter().filter(|k| k.ends_with(&suffix)).cloned().collect();
                if !pinned.is_empty() {
                    return Err(refuse(DbErrorKind::AccountKeysPinTheLabel {
                        id,
                        label: old.clone(),
                        keys: pinned,
                    }));
                }
            }
            guard_account_label(
                &tx,
                path,
                &before.venue,
                &before.tier,
                label.as_deref(),
                Some(id),
            )?;
            tx.execute("UPDATE account SET label = ?2 WHERE id = ?1", (id, label.as_deref()))
                .map_err(|e| DbError::sql(path, e))?;
            let after = account_row(&tx, path, id)?;
            AccountWrite { before: Some(before), after, verb, changed: true, keys }
        }
        AccountEdit::SetActive { id, active } => {
            let Some(before) = account_row(&tx, path, id)? else {
                return Err(refuse(DbErrorKind::NoSuchAccount { id }));
            };
            let keys = account_key_names(&tx, path, id)?;
            if before.active == active {
                let out = AccountWrite {
                    after: Some(before.clone()),
                    before: Some(before),
                    verb,
                    changed: false,
                    keys,
                };
                return commit_account_write(tx, path, out);
            }
            // ⚠ RE-ACTIVATING can collide where deactivating never can: while this row was off,
            // `account_one_account_per_book`'s `WHERE active = 1` let another row take its BOOK.
            // The index is the authority and would refuse the UPDATE; asking first is what lets the
            // refusal name the other row, the same two-layer shape every guard here has.
            if active && let Some(book) = &before.venue_account_id {
                let holder: Option<i64> = tx
                    .query_row(
                        &format!(
                            "SELECT id FROM account WHERE {} AND venue_account_id = ?2 \
                             AND active = 1 AND id <> ?3",
                            crate::schema::venue_is("venue_id", "?1")
                        ),
                        (&before.venue, book, id),
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| DbError::sql(path, e))?;
                if let Some(holder) = holder {
                    return Err(refuse(DbErrorKind::BookHeldByAnother {
                        id,
                        venue: before.venue.clone(),
                        holder,
                    }));
                }
            }
            tx.execute("UPDATE account SET active = ?2 WHERE id = ?1", (id, i64::from(active)))
                .map_err(|e| DbError::sql(path, e))?;
            let after = account_row(&tx, path, id)?;
            AccountWrite { before: Some(before), after, verb, changed: true, keys }
        }
        AccountEdit::SetTier { id, tier } => {
            let Some(before) = account_row(&tx, path, id)? else {
                return Err(refuse(DbErrorKind::NoSuchAccount { id }));
            };
            let keys = account_key_names(&tx, path, id)?;
            if before.tier == tier {
                // Already exactly this tier. Not an error — see `AccountWrite::changed`.
                let out = AccountWrite {
                    after: Some(before.clone()),
                    before: Some(before),
                    verb,
                    changed: false,
                    keys,
                };
                return commit_account_write(tx, path, out);
            }
            // ⚠ THE KEY-SPELLING REFUSAL, and the reason it is written as a MAP UP rather than a
            // render DOWN. `account_tier_a_key_spells` takes each key NAME to an `ACCOUNT_TIERS`
            // word, so `tier` — already proven to be one of those three above — is compared
            // against its own kind. The tempting spelling is the other direction (uppercase the
            // account tier and look for `_{TIER}_` in the name), and it is wrong on exactly one
            // tier: `paper`'s key token is `SIM`, so `_PAPER_` matches nothing and every move on
            // and off `paper` would be silently permitted by a guard that looked like it worked.
            // See `DbErrorKind::AccountKeysPinTheTier`.
            let pinned: Vec<String> = keys
                .iter()
                .filter(|k| account_tier_a_key_spells(k.as_str()).is_some_and(|t| t != tier))
                .cloned()
                .collect();
            if let Some(first) = pinned.first() {
                // The tier the keys spell, taken from the evidence rather than from `before.tier`:
                // a row whose stored tier and whose keys already disagree (which a hand-edited
                // store can be in) must have its refusal name what the CLASSIFIER will read, since
                // that is what mints the second row.
                let spelled = account_tier_a_key_spells(first.as_str())
                    .unwrap_or(before.tier.as_str())
                    .to_string();
                return Err(refuse(DbErrorKind::AccountKeysPinTheTier {
                    id,
                    tier: spelled,
                    keys: pinned,
                }));
            }
            // ⚠ The row is moving ACROSS `UNIQUE (venue, tier, label)`, so the destination tier's
            // guards are the create guards, asked with the label the row already carries. The
            // unlabelled arm is the one that matters and the one no index can make: moving an
            // unlabelled row onto a tier that already has one is the `AmbiguousAccount` plant.
            guard_account_label(&tx, path, &before.venue, tier, before.label.as_deref(), Some(id))?;
            tx.execute("UPDATE account SET tier = ?2 WHERE id = ?1", (id, tier))
                .map_err(|e| DbError::sql(path, e))?;
            let after = account_row(&tx, path, id)?;
            AccountWrite { before: Some(before), after, verb, changed: true, keys }
        }
        AccountEdit::Remove { id } => {
            let Some(before) = account_row(&tx, path, id)? else {
                return Err(refuse(DbErrorKind::NoSuchAccount { id }));
            };
            let keys = account_key_names(&tx, path, id)?;
            // ⚠ THE REFUSAL THE WHOLE VERB IS BUILT AROUND, and it is a PRE-CHECK over a
            // second-layer guarantee: the foreign key refuses this delete anyway, but its words
            // name nothing. See `DbErrorKind::AccountHasCredentials`.
            if !keys.is_empty() {
                return Err(refuse(DbErrorKind::AccountHasCredentials {
                    id,
                    venue: before.venue.clone(),
                    tier: before.tier.clone(),
                    keys,
                }));
            }
            // ⚠ A SUB-ACCOUNT's master may not vanish under it either: `account.parent_id
            // REFERENCES account(id)` is the same `NO ACTION` clause, so the engine refuses that
            // too. Nothing writes `parent_id` in this tree today, so there is no pre-check for it
            // and no message to write — the day something does, this is where the refusal goes.
            // ⚠ **THIS STATEMENT USED TO BE WHAT MADE AN `account.id` REUSABLE — it is the only
            // one in the tree that frees a number, and since stage 4 that number stays freed.**
            // The comment here read: *"`account.id INTEGER PRIMARY KEY` carries no `AUTOINCREMENT`
            // (the schema has none anywhere), so SQLite hands a new row `max(rowid) + 1`: deleting
            // the row with the LARGEST id frees that id for the next `Create`. Remove account 16,
            // add an account, and the new one IS account 16."* [`Account::id`]'s own doc had
            // promised ids were never reused within a file, justified by *"nothing in this crate
            // ever deletes a row"*, and this line is what falsified that.
            //
            // The cure is the spec's §4.1 — `id INTEGER PRIMARY KEY AUTOINCREMENT`, which SQLite
            // cannot add by `ALTER`, so it was delivered by a table REBUILD. The engine now keeps a
            // high-water mark and this `DELETE` does not move it, so an id an operator, a runbook, a
            // GUI cell or a wire client remembered names the removed account or nothing at all,
            // never a stranger.
            // `crates/vike-secrets/tests/accounts/lifecycle.rs`'s
            // `a_removed_id_is_never_handed_to_the_next_created_account` is the pin, inverted from
            // the one that used to record the reuse.
            //
            // ⚠ What did NOT change: the ECHO is still the identification. `vike-cli`'s `echo_row`
            // prints the row's credential KEY NAMES before the ceremony, because an id is only
            // stable for the life of ONE database file and a re-migration re-numbers everything.
            tx.execute("DELETE FROM account WHERE id = ?1", [id])
                .map_err(|e| DbError::sql(path, e))?;
            AccountWrite {
                before: Some(before),
                after: None,
                verb,
                changed: true,
                keys: Vec::new(),
            }
        }
    };
    commit_account_write(tx, path, out)
}

/// One `account` row by id, read INSIDE the transaction that is about to write it — so the echo a
/// caller renders is what the write actually saw, not what a read beforehand happened to find.
fn account_row(
    tx: &rusqlite::Transaction<'_>,
    path: &Path,
    id: i64,
) -> Result<Option<Account>, DbError> {
    let select = account_select(tx).map_err(|e| DbError::sql(path, e))?;
    tx.query_row(&format!("{select} WHERE a.id = ?1"), [id], account_from_row)
        .optional()
        .map_err(|e| DbError::sql(path, e))
}

/// The live credential key NAMES one account owns.
///
/// ⚠ **`SELECT name` — there is no `value` column in this statement**, which is what makes every
/// refusal and every echo built from it structurally incapable of carrying a credential, exactly as
/// [`read_account_keys`] is. `superseded_at IS NULL` matches that reader too: a name rotated out is
/// not evidence about who the row is today.
fn account_key_names(
    tx: &rusqlite::Transaction<'_>,
    path: &Path,
    id: i64,
) -> Result<Vec<String>, DbError> {
    let mut stmt = tx
        .prepare(
            "SELECT name FROM credential \
             WHERE account_id = ?1 AND superseded_at IS NULL ORDER BY name",
        )
        .map_err(|e| DbError::sql(path, e))?;
    let rows =
        stmt.query_map([id], |r| r.get::<_, String>(0)).map_err(|e| DbError::sql(path, e))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| DbError::sql(path, e))?);
    }
    Ok(out)
}

/// **WHICH [`crate::schema::ACCOUNT_TIERS`] tier a credential key NAME spells**, or `None` when it
/// spells none this workspace can classify.
///
/// ⚠ **The whole point is that it answers in ONE vocabulary.** A key name carries a
/// `vike_model::credential_keys::CREDENTIAL_TIERS` token — `SIM` | `DEMO` | `LIVE`, uppercase — and
/// `account.tier` carries an `ACCOUNT_TIERS` word. The two are different alphabets and a comparison
/// between them can never be equal, so a guard written across them is an assertion that cannot fire
/// for its stated reason. [`crate::schema::account_tier_named`] is the one function with both
/// directions, and it is what maps `SIM` onto `paper` — the pair the naive spelling gets wrong.
///
/// # The two classifiers, in the order the store's own classifier consults them
///
/// 1. `crate::venue_setting::HAND_MAPPED_ACCOUNTS` — the families whose store token is NOT a
///    credential tier at all (alpaca's `SANDBOX`, dukascopy's `DEMO1`/`DEMO2`, the `POLY_` trio).
///    That table answers with an `ACCOUNT_TIERS` word directly, so there is nothing to map. It is
///    asked FIRST for the reason `crate::venue_setting` states: `DUKASCOPY_DEMO1_` must not be
///    read as spelling `DEMO`, and a grammar-first order would let it.
/// 2. `vike_model::accounts::account_keys::account_ref_from_key`, mapped up through `account_tier_named`.
///
/// ⚠ **`None` is a real answer and it PERMITS the move** — see
/// [`DbErrorKind::AccountKeysPinTheTier`]'s declared residual. Aster's `TESTNET` token reaches
/// neither classifier, so an aster row's keys pin nothing.
fn account_tier_a_key_spells(name: &str) -> Option<&'static str> {
    for (head, token, _venue, tier, _discriminator, _why) in
        crate::venue_setting::HAND_MAPPED_ACCOUNTS
    {
        if name.starts_with(&crate::venue_setting::hand_mapped_prefix(head, token)) {
            return crate::schema::account_tier_named(tier);
        }
    }
    let token = vike_model::accounts::account_keys::account_ref_from_key(name)?.tier;
    crate::schema::account_tier_named(token)
}

/// The three LABEL guards, asked of the `(venue, tier)` that is about to carry `label`.
///
/// `exclude` is the row being edited, so a rename to the label a row already carries is not refused
/// by the row itself. Each guard asks a question the schema either cannot answer
/// ([`DbErrorKind::AmbiguousUnlabelledAccount`], [`DbErrorKind::AccountLabelHeldAsBook`]) or
/// answers without naming anybody ([`DbErrorKind::AccountLabelTaken`]).
///
/// ⚠ This doc spent a day on [`account_tier_a_key_spells`] instead: that function was inserted
/// BETWEEN these lines and this `fn`, so the paragraph describing an `exclude` parameter sat on a
/// function that has none. It compiles either way, which is why no lane could see it.
fn guard_account_label(
    tx: &rusqlite::Transaction<'_>,
    path: &Path,
    venue: &str,
    tier: &str,
    label: Option<&str>,
    exclude: Option<i64>,
) -> Result<(), DbError> {
    let refuse = |kind| DbError { path: path.to_path_buf(), kind };
    // No row can carry a negative rowid, so this is "exclude nothing" without a second statement.
    let exclude = exclude.unwrap_or(-1);
    // The venue by its NUMBER: every caller runs inside `edit_account`'s transaction, and
    // `venue_id` is `NOT NULL` on every row.
    let of_venue = crate::schema::venue_is("venue_id", "?1");
    match label {
        // ⚠ THE AMBIGUITY GUARD. `UNIQUE (venue_id, tier, label)` cannot make this one: NULLs are
        // distinct in a SQLite index, so the engine would take a second unlabelled row happily and
        // `crate::schema`'s `AccountResolver` would then hold two rows under one `by_key` entry —
        // which is what makes the NEXT credential key for that venue refuse as ambiguous.
        None => {
            let holder: Option<i64> = tx
                .query_row(
                    &format!(
                        "SELECT id FROM account \
                         WHERE {of_venue} AND tier = ?2 AND label IS NULL AND id <> ?3"
                    ),
                    (venue, tier, exclude),
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| DbError::sql(path, e))?;
            if let Some(holder) = holder {
                return Err(refuse(DbErrorKind::AmbiguousUnlabelledAccount {
                    venue: venue.to_string(),
                    tier: tier.to_string(),
                    holder,
                }));
            }
        }
        Some(label) => {
            let holder: Option<i64> = tx
                .query_row(
                    &format!(
                        "SELECT id FROM account \
                         WHERE {of_venue} AND tier = ?2 AND label = ?3 AND id <> ?4"
                    ),
                    (venue, tier, label, exclude),
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| DbError::sql(path, e))?;
            if let Some(holder) = holder {
                return Err(refuse(DbErrorKind::AccountLabelTaken {
                    venue: venue.to_string(),
                    tier: tier.to_string(),
                    label: label.to_string(),
                    holder,
                }));
            }
            // ⚠ The BOOK collision, scoped to ACTIVE rows exactly as `account_one_account_per_book`
            // is — `vike_dukascopy::resolve_account` filters on `active` before it
            // matches either column, so an inactive row naming this string cannot make a mount
            // ambiguous and refusing for it would refuse a legitimate state.
            let holder: Option<i64> = tx
                .query_row(
                    &format!(
                        "SELECT id FROM account \
                         WHERE {of_venue} AND venue_account_id = ?2 AND active = 1 AND id <> ?3"
                    ),
                    (venue, label, exclude),
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| DbError::sql(path, e))?;
            if let Some(holder) = holder {
                return Err(refuse(DbErrorKind::AccountLabelHeldAsBook {
                    venue: venue.to_string(),
                    label: label.to_string(),
                    holder,
                }));
            }
        }
    }
    Ok(())
}

/// Commit an [`edit_account`] transaction and hand back its result — the two lines every arm ends
/// with, so an early return on a no-op path cannot forget the commit.
///
/// ⚠ **It also re-derives [`Account::armed`], and this is the one place that cannot be forgotten.**
/// `Create` mints a row the DDL gives `armed = 0`, and a row minted at a tier its venue's arming
/// line names is one the OLD model would already resolve as armed — so leaving the default would
/// make the column disagree with `vike_config::VenuePolicy::account` for exactly the rows an
/// operator just created. `crate::settings::fold_arming_into_accounts` is a pure function of the
/// two tables and idempotent, so running it on every arm (including `Remove`, which changes the
/// row SET) costs a few tens of `UPDATE`s and removes the question of which verbs need it.
///
/// ⚠ **DECLARED RESIDUAL, carried deliberately rather than fixed here: the fold is not scoped to
/// the edited row, so one verb can move ANOTHER row's `armed` bit while [`AccountWrite`]'s echo
/// names only the row the operator asked about.** That is the column being DERIVED rather than
/// stored — the other row's bit was already stale and is now correct — but an operator reading
/// the echo would not learn that anything else moved. Harmless while nothing reads the column,
/// and nothing does: see [`Account::armed`], and
/// `crates/vike-secrets/src/settings/arming.rs`'s `fold_arming_into_accounts` for why `venue_arming` is
/// still the ceiling the mount consults. It becomes a REPORTING question the moment a reader
/// exists. Raised by the Task 5 review and deferred to the branch review, with this note as the
/// record so the next author meets it here rather than rediscovering it.
fn commit_account_write(
    tx: rusqlite::Transaction<'_>,
    path: &Path,
    out: AccountWrite,
) -> Result<AccountWrite, DbError> {
    crate::settings::fold_arming_into_accounts(&tx).map_err(|e| DbError::sql(path, e))?;
    tx.commit().map_err(|e| DbError::sql(path, e))?;
    Ok(out)
}
