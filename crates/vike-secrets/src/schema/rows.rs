//! Writing classified rows: `RowReport` (what a fill DID) and `write_rows` (the one derivation a create and a reshape share).

use std::collections::BTreeMap;

use rusqlite::Transaction;

use super::resolver::ResolveError;
use super::*;

// ---------------------------------------------------------------------------------------------
// Writing classified rows
// ---------------------------------------------------------------------------------------------

/// **What a schema-2 fill DID** — counts and NAMES, never a value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowReport {
    /// `account` rows this run created, as `(id, venue, tier)`.
    ///
    /// ⚠ The `id` is carried because `(venue, tier)` is NOT a key: dukascopy has TWO accounts at
    /// `(dukascopy, demo)` and neither carries a label, so a report keyed on the pair alone would
    /// fold ruling 1's whole point into one row — which it did, until a test said so. The id is the
    /// identity the owner's signature names, and it is not a secret.
    pub accounts_created: Vec<(i64, String, String)>,
    /// `credential` rows INSERTED with `superseded_at IS NULL`.
    ///
    /// ⚠ It counts INSERTS, and an ALIAS row is not one of them — it is inserted
    /// `superseded_at IS NOT NULL` by construction and counts under [`RowReport::alias_rows`]. The
    /// two together are the rows a fill CARRIED, which is what [`reshape_into`]'s guard compares.
    /// A row this fill inserted live and then DEMOTED (the canonical spelling arriving after its
    /// alias) is counted here, once, because that is what the fill did to it; the demotion is an
    /// `UPDATE` and is counted by neither field.
    pub live_rows: usize,
    /// `credential` rows INSERTED as the rollback copy of a name already holding the live row —
    /// see [`RowReport::aliases`], which NAMES them. The other half of *rows carried*.
    pub alias_rows: usize,
    /// **Two NAMES of one credential, as `(the alias, the name holding the live row)`.**
    ///
    /// Sorted, and never a value. The live case is the legacy tier spelling: a store holding both
    /// `{VENUE}_LIVE_API_KEY` and `{VENUE}_MAINNET_API_KEY` holds ONE credential under two names,
    /// because `vike_model::accounts::account_keys` normalizes `MAINNET` onto `LIVE` and §4.4 removes the
    /// store's own tier token from `field`. `credential_one_live_value` then admits exactly one of
    /// the two as live, so the other is filed `superseded_at IS NOT NULL` — which is what that
    /// column is for (§4.2: a rollback copy and the value that replaced it coexist) and is the
    /// only disposition that keeps BOTH `name` rows.
    ///
    /// Keeping both is the compatibility contract rather than tidiness: `crate::db::read_table`
    /// answers for an aliased name out of its superseded row (no live row carries it), so
    /// `crate::resolve_project` returns the same map before and after the upgrade and
    /// `vike-cli secrets list` still prints the operator's own spelling.
    ///
    /// ⚠ This is the IDENTICAL-value case. Two spellings carrying DIFFERENT values are refused by
    /// name instead — [`SchemaRefusal::CollidingLiveValues`].
    pub aliases: Vec<(String, String)>,
    /// **The key NAMES this fill WROTE**, sorted — live rows and superseded ones alike.
    ///
    /// ⚠ It exists because a SCHEMA UPGRADE inserts every row in the store while INSERTING NO NEW
    /// KEY, so a caller that records what a migration did from the pending set alone would journal
    /// nothing at all for the one act that rewrites the whole table and cannot be undone.
    ///
    /// ⚠ **`crate::db::migrate` performs that fold, into [`crate::Migration::inserted_keys`]** —
    /// this doc named `crates/vike-cli/src/cmd/secrets/migrate.rs`'s `record_migration` as the folder,
    /// which it is not: that function READS `inserted_keys` and hands the names to the change
    /// journal, and by the time it sees a `Migration` the fold has already happened. The
    /// distinction matters because the fold is what makes the CLI's guard (`if
    /// done.inserted_keys.is_empty() { return }`) record an upgrade at all; a reader who believed
    /// the CLI owned it would look for the bug in the wrong crate.
    pub written_names: Vec<String>,
    /// Superseded `credential` rows written from §4.2's commented-out assignments, by NAME.
    pub superseded_rows: Vec<String>,
    /// `notes` attached from §4.3's prose comments.
    pub notes_attached: usize,
    /// Comment lines that sat above no key and were therefore attached to nothing.
    pub unattached_prose_lines: usize,
    /// **Names the classifier could not place** — written verbatim as infrastructure rows and
    /// reported here (§11.1). Not an error; the store legitimately holds names no venue grammar
    /// covers.
    pub unrecognised: Vec<String>,
    /// **Rows §7 and §6 will move and this change deliberately did not** — by name, with the
    /// section that will take them. This is the next change's work-list.
    pub pending_moves: Vec<(String, PendingMove)>,
    /// Per-KEY refusals. The run still succeeded and every other row landed — the same disposition
    /// `crate::Ambiguity::DisagreesWithDatabase` already takes.
    pub refused: Vec<SchemaRefusal>,
}

impl RowReport {
    fn sort(&mut self) {
        self.accounts_created.sort();
        self.aliases.sort();
        self.aliases.dedup();
        self.superseded_rows.sort();
        self.written_names.sort();
        self.written_names.dedup();
        self.unrecognised.sort();
        self.pending_moves.sort();
        self.refused.sort();
        self.refused.dedup();
    }

    /// Is there anything worth printing? A clean fill on a store with no oddities reports nothing
    /// but its counts.
    ///
    /// ⚠ An ALIAS counts as something worth printing: a name the operator wrote has stopped being
    /// the live row for its credential, and a store that does that in silence is the defect §1 is
    /// about wearing a smaller hat.
    #[must_use]
    pub fn is_quiet(&self) -> bool {
        self.unrecognised.is_empty()
            && self.refused.is_empty()
            && self.pending_moves.is_empty()
            && self.aliases.is_empty()
    }

    /// Fold a second fill's findings into this one — a run that RESHAPES and then carries new file
    /// keys in the same transaction does both, and the operator wants ONE report.
    pub fn absorb(&mut self, other: RowReport) {
        self.accounts_created.extend(other.accounts_created);
        self.live_rows += other.live_rows;
        self.alias_rows += other.alias_rows;
        self.aliases.extend(other.aliases);
        self.superseded_rows.extend(other.superseded_rows);
        self.written_names.extend(other.written_names);
        self.notes_attached += other.notes_attached;
        self.unrecognised.extend(other.unrecognised);
        self.pending_moves.extend(other.pending_moves);
        self.refused.extend(other.refused);
        // ⚠ **A `SupersededKeyIsNotInTheStore` the OTHER half then wrote is not a finding**, and
        // leaving it in printed one on a run that did everything right. BOTH halves are handed the
        // same [`FileComments`], and a commented-out rollback line whose live key is arriving from
        // the FILE in this very run is seen twice: the reshape half runs first, over the OLD
        // table, where that key has no live row — so it refuses — and the fill half then inserts
        // the live row and rescues the same comment successfully. The refusal's own claim ("has no
        // live row in this store") is false by the time the transaction commits, and the evidence
        // is in this very report: the name is in `superseded_rows`. Dropped here rather than
        // suppressed at the source, because each half is individually right about the store it saw.
        let rescued: std::collections::BTreeSet<&str> =
            self.superseded_rows.iter().map(String::as_str).collect();
        self.refused.retain(|r| {
            !matches!(r, SchemaRefusal::SupersededKeyIsNotInTheStore { key }
                if rescued.contains(key.as_str()))
        });
        // `unattached_prose_lines` is a property of the FILE, not of a fill, so both halves saw the
        // same number and adding them would double it.
        self.sort();
    }
}

impl std::fmt::Display for RowReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} account row(s), {} live credential row(s)",
            self.accounts_created.len(),
            self.live_rows
        )?;
        if !self.superseded_rows.is_empty() {
            write!(
                f,
                ", {} superseded value(s) rescued from commented-out lines: {}",
                self.superseded_rows.len(),
                self.superseded_rows.join(", ")
            )?;
        }
        if !self.aliases.is_empty() {
            write!(
                f,
                "\n  {} name(s) are a SECOND SPELLING of a credential this store already holds — \
                 one account, one field, one value, two names. The live row is the canonical \
                 spelling's and the other is filed as its rollback copy; BOTH names still answer:",
                self.aliases.len()
            )?;
            for (alias, live) in &self.aliases {
                write!(f, "\n    - {alias} -> the live row is {live}'s")?;
            }
        }
        if self.notes_attached > 0 || self.unattached_prose_lines > 0 {
            write!(
                f,
                "\n  {} comment block(s) kept as notes; {} comment line(s) sat above no key and \
                 were attached to nothing",
                self.notes_attached, self.unattached_prose_lines
            )?;
        }
        if !self.unrecognised.is_empty() {
            write!(
                f,
                "\n  {} name(s) the classifier could not place — written VERBATIM as \
                 deployment-level rows, nothing dropped and nothing guessed: {}",
                self.unrecognised.len(),
                self.unrecognised.join(", ")
            )?;
        }
        if !self.pending_moves.is_empty() {
            write!(
                f,
                "\n  ⚠ {} row(s) are classified but NOT MOVED by this change — they keep their \
                 legacy name in `credential`, so every reader still finds them. The move waits on \
                 the map renderer (spec 6.2/12):",
                self.pending_moves.len()
            )?;
            for (name, mv) in &self.pending_moves {
                write!(f, "\n    - {name}: {mv}")?;
            }
        }
        if !self.refused.is_empty() {
            write!(
                f,
                "\n  ⚠ {} key(s) were REFUSED and nothing was written for them:",
                self.refused.len()
            )?;
            for r in &self.refused {
                write!(f, "\n    - {r}")?;
            }
        }
        Ok(())
    }
}

/// The `superseded_at` marker an ALIAS row carries.
///
/// Same shape and same reasoning as §4.2's `'superseded-before-schema-2'`: the column records THAT
/// a row is not the live one, and the reason rather than a date — nothing in this store knows when
/// the operator added the second spelling, and §4.3's rule forbids parsing prose for a fact code
/// uses. It is a distinct string from the rollback marker so the two can be told apart by eye in a
/// `SELECT`, which is the only way anybody will ever look at them.
const ALIAS_MARK: &str = "alias-of-the-canonical-tier-spelling";

/// The LIVE row at `(account_id, field)`, as `(id, name, value)`, or `None`.
///
/// `credential_one_live_value` admits at most one, so this is a lookup and not a scan.
fn live_row_at(
    tx: &Transaction<'_>,
    account_id: i64,
    field: &str,
) -> rusqlite::Result<Option<(i64, String, String)>> {
    let mut stmt = tx.prepare(
        "SELECT id, name, value FROM credential \
         WHERE account_id = ?1 AND field = ?2 AND superseded_at IS NULL",
    )?;
    let mut rows = stmt.query((account_id, field))?;
    match rows.next()? {
        Some(r) => Ok(Some((r.get(0)?, r.get(1)?, r.get(2)?))),
        None => Ok(None),
    }
}

/// **Does this NAME spell the tier its classification resolved to?**
///
/// The tiebreak when two names collide at one `(account, field)` with the same value, and the one
/// signal available here that is not row order. The classifier NORMALIZES a tier
/// (`vike_model::accounts::account_keys::AccountRef::tier`: the legacy `MAINNET` resolves to `LIVE`), so the
/// name that still carries the canonical token in its owner prefix is the canonical spelling and
/// the one that does not is the alias. `ASTER_LIVE_API_KEY` answers `true` here and
/// `ASTER_MAINNET_API_KEY` answers `false`, which is the whole of the decision.
///
/// The match is on `_{TIER}_` rather than on the bare token, so `DUKASCOPY_DEMO1_` does not read as
/// spelling `DEMO`. When NEITHER name spells the tier — every hand-mapped family, whose store token
/// is by definition not the canonical one (`ALPACA_SANDBOX_`) — the answer is `false` for both and
/// the row already written keeps the live value, i.e. sorted order decides. That is arbitrary and
/// is stated as such: it is reached only by a collision this store has never produced, and the
/// report NAMES both spellings either way.
fn spells_its_tier(name: &str, field: &str, tier: &str) -> bool {
    // An EMPTY tier would make the needle `__`, which a labelled name contains — so it is refused
    // outright rather than left to produce an answer from no evidence. Unreachable today (the
    // caller has a `Placement::Account`, whose tier is a non-empty `ACCOUNT_TIERS` member by the
    // time this runs), and cheap enough to state.
    if tier.is_empty() {
        return false;
    }
    let Some(prefix) = name.strip_suffix(field) else { return false };
    format!("_{prefix}").contains(&format!("_{}_", tier.to_ascii_uppercase()))
}

/// **Fill a schema-2 store's `account` and `credential` tables from a name→value map.**
///
/// The one derivation, used by BOTH a fresh create and [`reshape_into`], so a store born at 2 and a
/// store carried to 2 cannot be classified differently. It writes no `venue_setting` row and no
/// `venue_account_id` — see this module's doc for the sequencing rule that forbids both.
///
/// Rows already present under the same live NAME are left alone, which is what makes a second run a
/// no-op.
///
/// # Errors
/// The engine. Per-KEY refusals ride [`RowReport::refused`] and leave the run successful.
pub fn write_rows(
    tx: &Transaction<'_>,
    rows: &BTreeMap<String, String>,
    comments: &FileComments,
    classify: &dyn Fn(&str) -> Classification,
) -> rusqlite::Result<RowReport> {
    let mut report =
        RowReport { unattached_prose_lines: comments.unattached_prose_lines, ..Default::default() };
    let mut resolver = AccountResolver::load(tx)?;
    // ONE spelling of `credential`'s venue link for all four INSERTs below ([`VenueLink`]). Built
    // here, from the store as this call finds it: every caller has run the funnel or the `DDL`
    // batch first, and nothing below changes a table's shape.
    let link = VenueLink::of(tx, "credential", "c")?;

    let existing: std::collections::BTreeSet<String> = {
        let mut stmt = tx.prepare("SELECT name FROM credential WHERE superseded_at IS NULL")?;
        let names = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = std::collections::BTreeSet::new();
        for n in names {
            out.insert(n?);
        }
        out
    };

    for (name, value) in rows {
        if existing.contains(name) {
            continue;
        }
        let class = classify(name);
        let note = comments.notes.get(name).map(String::as_str);
        let owner_prefix = class.owner_prefix(name);
        if owner_prefix.is_none() && class.needs_owner_prefix() {
            report.refused.push(SchemaRefusal::FieldIsNotASuffix {
                key: name.clone(),
                field: class.field.clone(),
            });
            continue;
        }
        let (account_id, venue) = match &class.placement {
            Placement::Account(key) => {
                if !ACCOUNT_TIERS.contains(&key.tier.as_str()) {
                    report.refused.push(SchemaRefusal::UnknownTier {
                        key: name.clone(),
                        tier: key.tier.clone(),
                    });
                    continue;
                }
                match resolver.resolve(tx, name, key, owner_prefix, note) {
                    Ok(id) => (Some(id), None),
                    Err(ResolveError::Sql(e)) => return Err(e),
                    Err(ResolveError::Refused(r)) => {
                        report.refused.push(r);
                        continue;
                    }
                }
            }
            Placement::Venue(v) => {
                if !venue_on_roster(tx, v)? {
                    report.refused.push(SchemaRefusal::VenueNotOnRoster {
                        key: name.clone(),
                        venue: v.clone(),
                    });
                    continue;
                }
                (None, Some(v.clone()))
            }
            Placement::Infrastructure => (None, None),
        };
        // ⚠ **TWO NAMES, ONE `(account_id, field)`** — the one collision this store can actually
        // produce, and the reason this block exists rather than letting the engine answer. See
        // [`RowReport::aliases`] and [`SchemaRefusal::CollidingLiveValues`]; `spells_its_tier`
        // decides which of the two spellings keeps the live row.
        //
        // ⚠ Both `INSERT`s in this block fire only when `account_id` is `Some`, which happens
        // only on the `Placement::Account` arm above — and that arm always pairs
        // `account_id = Some(id)` with `venue = None`. So `venue` is provably `None` on every row
        // either statement below can write, and the `venue_id` they write is NULL with it: an
        // alias row is filed against an ACCOUNT and names no venue at all (`credential`'s
        // `CHECK (account_id IS NULL OR venue_id IS NULL)` refuses the other combination; it named
        // the text `venue` until the venue-links plan's second release). They name the link
        // through [`VenueLink`] anyway, like the two `credential` inserts further down — the ones
        // reached with `account_id` NONE (`Placement::Venue`/`Placement::Infrastructure`), which
        // can carry a venue — so that every `credential` INSERT is one spelling, and the second
        // release dropped the text in one place.
        // (⚠ They named `venue` and no `venue_id` until the venue-links plan's reader task. The
        // plan read that as a venue-scoped alias row left without its number; there is no such
        // row, and `crates/vike-secrets/tests/migration/database/two_spellings.rs`'s
        // `a_legacy_tier_spelling_is_filed_as_an_alias_and_both_names_still_answer` holds the alias
        // row venue-less.)
        if let Some(id) = account_id
            && let Some((other_id, other_name, other_value)) = live_row_at(tx, id, &class.field)?
        {
            if &other_value != value {
                report.refused.push(SchemaRefusal::CollidingLiveValues {
                    key: name.clone(),
                    other: other_name,
                    field: class.field.clone(),
                });
                continue;
            }
            let tier = match &class.placement {
                Placement::Account(key) => key.tier.as_str(),
                // Unreachable: `account_id` is `Some` only on the `Placement::Account` arm above.
                _ => "",
            };
            if spells_its_tier(name, &class.field, tier)
                && !spells_its_tier(&other_name, &class.field, tier)
            {
                // The NEWCOMER is the canonical spelling and the row already holding the live value
                // is the alias. Demote that one — an `UPDATE`, counted by neither counter — and let
                // this one land LIVE, which is an insert and counts as one.
                tx.execute(
                    "UPDATE credential SET superseded_at = ?2 WHERE id = ?1",
                    (other_id, ALIAS_MARK),
                )?;
                report.aliases.push((other_name, name.clone()));
                tx.execute(
                    &format!(
                        "INSERT INTO credential \
                         (account_id, {}, field, value, name, secret, notes) \
                         VALUES (?1, {}, ?3, ?4, ?5, ?6, ?7)",
                        link.columns,
                        link.values("?2")
                    ),
                    (account_id, &venue, &class.field, value, name, i64::from(class.secret), note),
                )?;
                report.live_rows += 1;
            } else {
                // The ordinary direction: the row already written keeps the live value and THIS
                // name is filed as its rollback copy, so both names survive and `read_table`
                // answers for either.
                tx.execute(
                    &format!(
                        "INSERT INTO credential \
                         (account_id, {}, field, value, name, secret, notes, superseded_at) \
                         VALUES (?1, {}, ?3, ?4, ?5, ?6, ?7, ?8)",
                        link.columns,
                        link.values("?2")
                    ),
                    (
                        account_id,
                        &venue,
                        &class.field,
                        value,
                        name,
                        i64::from(class.secret),
                        note,
                        ALIAS_MARK,
                    ),
                )?;
                report.aliases.push((name.clone(), other_name));
                report.alias_rows += 1;
            }
            report.written_names.push(name.clone());
            if note.is_some() {
                report.notes_attached += 1;
            }
            if let Some(mv) = class.pending_move {
                report.pending_moves.push((name.clone(), mv));
            }
            continue;
        }
        tx.execute(
            &format!(
                "INSERT INTO credential (account_id, {}, field, value, name, secret, notes) \
                 VALUES (?1, {}, ?3, ?4, ?5, ?6, ?7)",
                link.columns,
                link.values("?2")
            ),
            (account_id, &venue, &class.field, value, name, i64::from(class.secret), note),
        )?;
        report.live_rows += 1;
        report.written_names.push(name.clone());
        if note.is_some() {
            report.notes_attached += 1;
        }
        if !class.recognised {
            report.unrecognised.push(name.clone());
        }
        if let Some(mv) = class.pending_move {
            report.pending_moves.push((name.clone(), mv));
        }
    }

    // §4.2 — the commented-out rollback copies, AFTER the live rows so the "is there a live row"
    // test below sees this run's own inserts.
    for (name, value) in &comments.superseded {
        let live: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential WHERE name = ?1 AND superseded_at IS NULL)",
            (name,),
            |r| r.get(0),
        )?;
        if !live {
            // ⚠ A commented assignment whose key has no live row is not a SUPERSEDED value — it is
            // a disabled key, and writing it would introduce a credential the store does not
            // otherwise hold, out of a line the one parser skips. Reported, not written.
            report.refused.push(SchemaRefusal::SupersededKeyIsNotInTheStore { key: name.clone() });
            continue;
        }
        let already: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential WHERE name = ?1 AND superseded_at IS NOT NULL)",
            (name,),
            |r| r.get(0),
        )?;
        if already {
            continue;
        }
        let class = classify(name);
        let owner_prefix = class.owner_prefix(name);
        let (account_id, venue) = match &class.placement {
            Placement::Account(key) => match resolver.resolve(tx, name, key, owner_prefix, None) {
                Ok(id) => (Some(id), None),
                Err(ResolveError::Sql(e)) => return Err(e),
                Err(ResolveError::Refused(r)) => {
                    report.refused.push(r);
                    continue;
                }
            },
            Placement::Venue(v) => {
                if !venue_on_roster(tx, v)? {
                    report.refused.push(SchemaRefusal::VenueNotOnRoster {
                        key: name.clone(),
                        venue: v.clone(),
                    });
                    continue;
                }
                (None, Some(v.clone()))
            }
            Placement::Infrastructure => (None, None),
        };
        // `superseded_at` records that the value WAS replaced, not when — the file's comment is the
        // only evidence of a date and §4.3's rule forbids parsing prose for a fact code uses. The
        // marker is the migration that rescued it.
        tx.execute(
            &format!(
                "INSERT INTO credential \
                 (account_id, {}, field, value, name, secret, superseded_at) \
                 VALUES (?1, {}, ?3, ?4, ?5, ?6, 'superseded-before-schema-2')",
                link.columns,
                link.values("?2")
            ),
            (account_id, &venue, &class.field, value, name, i64::from(class.secret)),
        )?;
        report.superseded_rows.push(name.clone());
        report.written_names.push(name.clone());
    }

    report.accounts_created = std::mem::take(&mut resolver.created);
    report.sort();
    Ok(report)
}

/// Whether the store's `venue` table holds `venue` — the question [`write_rows`] asks before it
/// files a venue-scoped row, whose link is the number [`VenueLink::values`] looks up by this name
/// (see [`SchemaRefusal::VenueNotOnRoster`]).
fn venue_on_roster(tx: &Transaction<'_>, venue: &str) -> rusqlite::Result<bool> {
    tx.query_row("SELECT EXISTS(SELECT 1 FROM venue WHERE name = ?1)", [venue], |r| r.get(0))
}
