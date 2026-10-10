//! Writing classified rows: `RowReport` (what a fill DID) and `write_rows` (the one derivation every credential write takes).

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
    /// two together are the rows a fill CARRIED.
    /// A row this fill inserted live and then DEMOTED (the canonical spelling arriving after its
    /// alias) is counted here, once, because that is what the fill did to it; the demotion is an
    /// `UPDATE` and is counted by neither field.
    pub live_rows: usize,
    /// `credential` rows INSERTED as the rollback copy of a name already holding the live row —
    /// see [`RowReport::aliases`], which NAMES them. The other half of *rows carried*.
    pub alias_rows: usize,
    /// **Two NAMES of one credential, as `(the alias, the name holding the live row)`.**
    ///
    /// Sorted, and never a value. The live case is two spellings of one tier: a store holding both
    /// `ALPACA_DEMO_API_KEY` and `ALPACA_SANDBOX_API_KEY` holds ONE credential under two names,
    /// because the hand-map files alpaca's `SANDBOX` token as the `demo` tier the venue grammar
    /// reads `DEMO` as, and §4.4 removes the store's own tier token from `field`.
    /// `credential_one_live_value` then admits exactly one of the two as live, so the other is
    /// filed `superseded_at IS NOT NULL` — which is what that column is for (§4.2: a rollback copy
    /// and the value that replaced it coexist) and is the only disposition that keeps BOTH `name`
    /// rows.
    ///
    /// Keeping both is the compatibility contract rather than tidiness: `crate::db::read_table`
    /// answers for an aliased name out of its superseded row (no live row carries it), so
    /// `crate::resolve_project` answers for both names and `vike-cli secrets list` still prints the
    /// operator's own spelling.
    ///
    /// ⚠ This is the IDENTICAL-value case. Two spellings carrying DIFFERENT values are refused by
    /// name instead — [`SchemaRefusal::CollidingLiveValues`].
    pub aliases: Vec<(String, String)>,
    /// **Names the classifier could not place** — written verbatim as infrastructure rows and
    /// reported here (§11.1). Not an error; the store legitimately holds names no venue grammar
    /// covers.
    pub unrecognised: Vec<String>,
    /// Per-KEY refusals: every other row landed. Whether that is survivable is the CALLER's call —
    /// `crate::db`'s `upsert_rows` fails the write on the first refusal.
    pub refused: Vec<SchemaRefusal>,
}

impl RowReport {
    fn sort(&mut self) {
        self.accounts_created.sort();
        self.aliases.sort();
        self.aliases.dedup();
        self.unrecognised.sort();
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
        self.unrecognised.is_empty() && self.refused.is_empty() && self.aliases.is_empty()
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
        if !self.unrecognised.is_empty() {
            write!(
                f,
                "\n  {} name(s) the classifier could not place — written VERBATIM as \
                 deployment-level rows, nothing dropped and nothing guessed: {}",
                self.unrecognised.len(),
                self.unrecognised.join(", ")
            )?;
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
/// The column records THAT a row is not the live one, and the reason rather than a date — nothing
/// in this store knows when the operator added the second spelling. It is a distinct string from
/// §4.2's `'superseded-before-schema-2'` rollback marker so the two can be told apart by eye in a
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
/// signal available here that is not row order. A hand-mapped family's store token is by
/// definition not the canonical tier word, so the name that carries the canonical token in its
/// owner prefix is the canonical spelling and the one that does not is the alias.
/// `ALPACA_DEMO_API_KEY` answers `true` here (tier `demo`) and `ALPACA_SANDBOX_API_KEY` answers
/// `false`, which is the whole of the decision.
///
/// The match is on `_{TIER}_` rather than on the bare token, so `DUKASCOPY_DEMO1_` does not read as
/// spelling `DEMO`. When NEITHER name spells the tier — two hand-mapped spellings of one account —
/// the answer is `false` for both and the row already written keeps the live value, i.e. sorted
/// order decides. That is arbitrary and is stated as such: it is reached only by a collision this
/// store has never produced, and the report NAMES both spellings either way.
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
/// The one derivation every credential write takes. It writes no `venue_setting` row and no
/// `venue_account_id` — see this module's doc for the sequencing rule that forbids both.
///
/// Rows already present under the same live NAME are left alone, which is what makes a second run a
/// no-op.
///
/// # Errors
/// The engine. Per-KEY refusals ride [`RowReport::refused`] and leave the run successful.
pub(crate) fn write_rows(
    tx: &Transaction<'_>,
    rows: &BTreeMap<String, String>,
    classify: &dyn Fn(&str) -> Classification,
) -> rusqlite::Result<RowReport> {
    let mut report = RowReport::default();
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
        // The `notes` column is §4.3's, and nothing in this tree writes a note: it is bound NULL.
        let note: Option<&str> = None;
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
        // row, and `crates/vike-secrets/tests/store/database/two_spellings.rs`'s
        // `a_second_spelling_is_filed_as_an_alias_and_both_names_still_answer` holds the alias row
        // venue-less.)
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
        if !class.recognised {
            report.unrecognised.push(name.clone());
        }
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
