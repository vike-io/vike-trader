//! `AccountResolver`: which `account` row a credential name belongs to, resolved once per run and never guessed.

use std::collections::BTreeMap;

use rusqlite::Transaction;

use super::*;

// ---------------------------------------------------------------------------------------------
// The account resolver
// ---------------------------------------------------------------------------------------------

/// **Which `account` row a credential name belongs to, resolved once per run and never guessed.**
///
/// TWO lookups, in order, and the order is the whole design:
///
/// 1. **By OWNER PREFIX.** A name this store has never seen — `OKX_DEMO_API_PASSPHRASE` added to a
///    box that already holds `OKX_DEMO_API_KEY` — resolves through the prefix its siblings imply.
///    See [`Classification::owner_prefix`] for why this is a DERIVATION from stored data rather
///    than a heuristic, and why it is what lets dukascopy's two accounts survive a re-run with no
///    label and no discriminator column.
/// 2. **By `(venue, tier, label)`.** The last resort, and it is what unifies two spellings of one
///    tier: `ALPACA_SANDBOX_API_KEY` and `ALPACA_DEMO_API_KEY` have DIFFERENT owner prefixes and
///    are one account, because the hand-map files alpaca's `SANDBOX` token as the `demo` tier.
///    ⚠ That unification is also what makes the two names collide at one `(account_id, field)` —
///    [`RowReport::aliases`] is the disposition, and it belongs to the WRITER rather than to this
///    resolver, which is doing exactly what it should here.
///    ⚠ It REFUSES ([`SchemaRefusal::AmbiguousAccount`]) rather than picking when the classifier
///    offered no [`AccountKey::discriminator`] and more than one unlabelled row matches — the
///    state dukascopy would put it in if its hand-map ever stopped discriminating.
///
/// ⚠ **This said "THREE lookups" and put a lookup BY NAME first, and there is no such lookup here.**
/// The by-name answer is real but it is [`write_rows`]' `existing` set, which SKIPS a name the
/// store already holds before a resolver is ever consulted — so the account is not re-resolved, it
/// is not touched at all. That is what makes a second run a no-op, and reading it as a first
/// lookup here sends anybody debugging idempotence into the wrong function.
pub(crate) struct AccountResolver {
    by_prefix: BTreeMap<String, i64>,
    by_key: BTreeMap<(String, String, Option<String>, Option<String>), i64>,
    /// `(venue, tier)` pairs that ALREADY hold more than one UNLABELLED account row.
    ///
    /// ⚠ **The one thing lookup 3 must never answer for.** `by_key` is a map, so two unlabelled
    /// rows of one `(venue, tier)` collapse into one entry and the second silently wins — which is
    /// precisely the dukascopy shape, and precisely the class of silent wrong answer §1 of the
    /// spec is about. This set is how lookup 3 knows to REFUSE instead of picking, and it is
    /// counted at LOAD rather than asked per row because the map has already lost the evidence by
    /// the time a lookup happens.
    ambiguous_unlabelled: std::collections::BTreeSet<(String, String)>,
    /// Every `(venue, tier)` that holds at least one UNLABELLED account row — the state
    /// [`AccountResolver::ambiguous_unlabelled`] is the SECOND sighting of. Kept alongside it so a
    /// run that CREATES the second one (dukascopy's pair, on migration day) reaches the same
    /// verdict as the run that merely finds them, rather than refusing only from the next run on.
    unlabelled_seen: std::collections::BTreeSet<(String, String)>,
    pub(super) created: Vec<(i64, String, String)>,
}

impl AccountResolver {
    /// Build the resolver from what the database already holds.
    ///
    /// Reads every live credential row's `(name, field, account_id)` and reconstructs each
    /// account's owner prefix from it, which is the state lookup 2 rests on.
    pub(super) fn load(tx: &Transaction<'_>) -> rusqlite::Result<AccountResolver> {
        let mut by_prefix: BTreeMap<String, i64> = BTreeMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT name, field, account_id FROM credential \
                 WHERE superseded_at IS NULL AND account_id IS NOT NULL",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
            })?;
            for row in rows {
                let (name, field, id) = row?;
                if let Some(prefix) = name.strip_suffix(field.as_str()) {
                    by_prefix.insert(prefix.to_string(), id);
                }
            }
        }
        let mut by_key = BTreeMap::new();
        let mut ambiguous_unlabelled = std::collections::BTreeSet::new();
        let mut unlabelled_seen = std::collections::BTreeSet::new();
        {
            // The venue by its number ([`VenueLink`]): lookup 3 keys on `(venue, tier, label)`, and
            // a key resolved through a text cell that disagreed with the number would file a
            // credential against an account of another venue.
            let link = VenueLink::of(tx, "account", "a")?;
            let mut stmt = tx.prepare(&format!(
                "SELECT a.id, {}, a.tier, a.label FROM account a {}",
                link.name, link.join
            ))?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })?;
            for row in rows {
                let (id, venue, tier, label) = row?;
                let unlabelled = label.is_none();
                // The discriminator is derivation-time only and is stored nowhere, so an existing
                // row joins this map under `None` — lookup 3's shape, which is the only one that
                // consults it.
                by_key.insert((venue.clone(), tier.clone(), label, None), id);
                if unlabelled && !unlabelled_seen.insert((venue.clone(), tier.clone())) {
                    // ⚠ A SECOND unlabelled row of this `(venue, tier)`. The map just lost one of
                    // them; record the pair so lookup 3 refuses rather than answering with
                    // whichever row happened to be read last. This is dukascopy after a migration,
                    // and it is not an error — it is the schema working.
                    ambiguous_unlabelled.insert((venue, tier));
                }
            }
        }
        Ok(AccountResolver {
            by_prefix,
            by_key,
            ambiguous_unlabelled,
            unlabelled_seen,
            created: Vec::new(),
        })
    }

    /// The `account.id` for `key`, creating the row when nothing answers.
    pub(super) fn resolve(
        &mut self,
        tx: &Transaction<'_>,
        name: &str,
        key: &AccountKey,
        owner_prefix: Option<&str>,
        note: Option<&str>,
    ) -> Result<i64, ResolveError> {
        if let Some(id) = owner_prefix.and_then(|p| self.by_prefix.get(p)) {
            return Ok(*id);
        }
        let map_key =
            (key.venue.clone(), key.tier.clone(), key.label.clone(), key.discriminator.clone());
        if let Some(id) = self.by_key.get(&map_key) {
            // ⚠ **THE REFUSAL, and it guards lookup 3 rather than sitting after it.** A hit here on
            // an UNLABELLED account whose `(venue, tier)` already holds more than one such row is
            // the one answer nothing may give: `by_key` is a map, so it is holding whichever of the
            // two was read last, and returning it would file this key against an account picked by
            // row order. Dukascopy's pair after a migration is exactly that state, and it is
            // reached by any key whose prefix is NEW while its `(venue, tier, label)` is not — a
            // canonical-tier `DUKASCOPY_DEMO_LOGIN` beside the two indexed sets, say.
            //
            // A DISCRIMINATED key never arrives here, because the hand-map's discriminator is part
            // of `map_key` and the table's rows joined `by_key` under `None`; its account is found
            // by the owner prefix above, which is what that prefix exists for.
            if key.label.is_none()
                && key.discriminator.is_none()
                && self.ambiguous_unlabelled.contains(&(key.venue.clone(), key.tier.clone()))
            {
                return Err(ResolveError::Refused(SchemaRefusal::AmbiguousAccount {
                    key: name.to_string(),
                    venue: key.venue.clone(),
                    tier: key.tier.clone(),
                }));
            }
            if let Some(p) = owner_prefix {
                self.by_prefix.insert(p.to_string(), *id);
            }
            return Ok(*id);
        }
        let link = VenueLink::of(tx, "account", "a").map_err(ResolveError::Sql)?;
        tx.execute(
            &format!(
                "INSERT INTO account ({}, tier, label, notes) VALUES ({}, ?2, ?3, ?4)",
                link.columns,
                link.values("?1")
            ),
            (&key.venue, &key.tier, &key.label, note),
        )
        .map_err(ResolveError::Sql)?;
        let id = tx.last_insert_rowid();
        if let Some(p) = owner_prefix {
            self.by_prefix.insert(p.to_string(), id);
        }
        self.by_key.insert(map_key, id);
        if key.label.is_none() && key.discriminator.is_some() {
            // ⚠ **THE SAME-RUN / LATER-RUN PARITY, and it needs this line as well as the flag
            // below.** [`AccountResolver::load`] joins every row of the `account` table under a
            // `None` discriminator — the column does not exist, so it cannot do otherwise — while
            // a row CREATED in this run joins under the discriminator that created it. So an
            // UNDISCRIMINATED key arriving after a discriminated one (`DUKASCOPY_DEMO_LOGIN` after
            // `DUKASCOPY_DEMO1_LOGIN`) MISSED in the same run and HIT on the next, and the two
            // runs then did different things to a store neither of them had changed: the first
            // created a THIRD dukascopy account for it, the second either attached it to an
            // existing one or refused it. Registering the discriminator-less shape here is what
            // makes the two agree. `or_insert`, so the first account created keeps the entry —
            // which of the two it is decides nothing once the pair is flagged ambiguous below.
            self.by_key.entry((key.venue.clone(), key.tier.clone(), None, None)).or_insert(id);
        }
        if key.label.is_none()
            && !self.unlabelled_seen.insert((key.venue.clone(), key.tier.clone()))
        {
            // The SECOND unlabelled account of this `(venue, tier)` — dukascopy, on migration day.
            // Recorded now so a later key in the SAME run reaches the same refusal a later RUN
            // would, rather than the two disagreeing about a store neither of them changed.
            self.ambiguous_unlabelled.insert((key.venue.clone(), key.tier.clone()));
        }
        self.created.push((id, key.venue.clone(), key.tier.clone()));
        Ok(id)
    }
}

/// The two ways [`AccountResolver::resolve`] can fail: the engine, or a refusal that names a key.
pub(super) enum ResolveError {
    Sql(rusqlite::Error),
    Refused(SchemaRefusal),
}
