//! `vike-cli secrets accounts` — the settings database's ACCOUNT TABLE, printed, plus the parked
//! venue confirmations a live mount left behind.
//!
//! A read, and the companion `set-book` (in `book`) cannot be used without: it addresses a row by
//! `id`, which is not a thing an operator can know from anywhere else. Split out of
//! `cmd/secrets.rs` (code-layout phase 2, task 10).

use std::path::Path;

use super::*;

/// `accounts` — the settings database's `account` TABLE, printed.
///
/// A read, and the one this command was missing: `list` prints the accounts DERIVED from key names
/// (`vike_model::accounts::account_keys::accounts_in_store`, the grammar every caller uses today), which is a
/// different question and gives a different answer. That reader cannot see dukascopy's two demo
/// accounts at all — the grammar deliberately does not retro-fit a venue that bakes an account
/// INDEX into its tier token — and it has no `id` to print, because a name carries none.
///
/// ⚠ **It is also what makes [`run_set_book`] usable.** That verb addresses a row by `id`, and `id`
/// is a database surrogate: it is not in any key name, not in a `policy` row, and not derivable from
/// anything an operator can read. Without this listing, the writer would address rows by a handle
/// nobody can obtain.
///
/// # The three answers, kept three
///
/// `vike_secrets::resolve_accounts_in` answers with `Accounts::Known(rows)` — possibly EMPTY — or
/// with `Accounts::Unanswerable`, and this function may not merge them. An unmigrated box is the
/// second: it has no `account` table, its credentials are answering perfectly well under their
/// legacy names, and printing `0 accounts` there would be an assertion about a store holding
/// sixteen of them. `vike_secrets::NoAccountTable`'s own `Display` is what says which store is
/// answering instead.
///
/// It exits SUCCESS on that path, the same posture `run_list` takes for an absent store: an
/// unmigrated box is an ordinary state, not a fault. The store that EXISTS and cannot be READ is
/// the loud case, and it arrives as an error from the resolver.
///
/// # ⚠ The `credential keys` column is what makes the listing ACTIONABLE, not decoration
///
/// An `id` addresses a row; it does not IDENTIFY one. For the pair this feature exists for, the
/// account table's every other cell is equal on both rows — `(dukascopy, demo, label NULL, book
/// NULL)` twice — so a listing built from `vike_secrets::Account` alone shows the operator two
/// lines differing by an opaque integer and no way to choose between them. Choosing wrongly points
/// an account at the other legal entity.
///
/// So this also reads `vike_secrets::resolve_account_keys_in` and prints, per row, the credential
/// key names that row owns (and the owner prefixes they imply). That reader selects `name` and
/// `field` and never `value`, so the guarantee below is unchanged. Where more than one row shares
/// `(venue, tier, label)` — the state `vike_secrets`' own resolver REFUSES to guess in — the full
/// key names are printed under the table for exactly those rows.
///
/// # ⚠ It prints the SCOPE of an id, because a listing is where somebody writes one down
///
/// `account.id` is a SQLite rowid: stable for the life of this database FILE — an `AUTOINCREMENT`
/// mark since stage 4, so a REMOVED id never comes back, but the mark is IN the file and a
/// delete-and-re-migrate repair re-assigns every number. `vike_secrets::
/// Account::id` carries the argument; the closing line here is what puts it in front of the person
/// about to note *account 2 is the Swiss one* on a sticky note.
///
/// # ⚠ `last verified` is a COLUMN here, because a writer with no reader is not a fix
///
/// `vike_secrets::Account::last_verified_at` gained its first writer on 2026-09-15 (a venue
/// handshake, folded by [`run_confirm`]) and the column answered *when did this credential last
/// authenticate* — in a listing that did not print it. It appeared at one site, inside `confirm`'s
/// own per-record echo, which an operator reaches only once they already suspect something. So the
/// incident the column exists to remove survived it: *never verified* and *verified three weeks
/// ago* still looked identical to *fine* in the place people actually look.
///
/// It is rendered [`NEVER_VERIFIED`] rather than a dash when the column is `None`, for the reason
/// that constant carries, and a footer paragraph says what the state means so that a freshly
/// migrated box — where it is EVERY row — does not read the column as an alarm.
///
/// # ⚠ It also reports what a live mount PARKED, on both of its exits
///
/// [`report_parked_confirmations`] — including the `Backend::Absent` exit, which returns early and
/// used to return before the notice existed for it. That function carries the argument.
///
/// Nothing here can print a credential: one reader selects from `account` alone, the other selects
/// key NAMES, and a parked record holds a venue account id, a key PREFIX and a row id.
pub(super) fn run_accounts(ctx: &Ctx<'_>) -> Result<(), String> {
    let settings = settings_dir_of(ctx.settings_dir, ctx.settings_dir_override);
    let accounts = vike_secrets::resolve_accounts_in(&settings)
        .map_err(|e| format!("{e} — nothing to list"))?;

    let rows = match &accounts {
        vike_secrets::Accounts::Known(rows) => rows,
        vike_secrets::Accounts::Unanswerable(why) => {
            println!("no account table: {why}");
            println!(
                "\nthe accounts of a store like that are in the key NAMES — `vike-cli secrets \
                 list` prints them. `vike-cli secrets migrate --dry-run` shows what moving this \
                 box into the settings database would do."
            );
            // ⚠ **AND THE PARKED CONFIRMATIONS, on the way out.** This arm returns early, and it
            // used to return before the notice at the foot of this function ever ran — so the one
            // population that cannot discover a parked record any other way was the one population
            // never told about it. A mount parks on this box exactly as it does on a migrated one.
            // See [`report_parked_confirmations`].
            report_parked_confirmations(
                &settings.join(vike_model::paths::state_path::STATE_SUBDIR),
                Fold::BlockedNoAccountTable,
            );
            return Ok(());
        }
    };

    // ⚠ **THE SEVENTH COLUMN IS WHAT MAKES THIS LISTING USABLE**, and the verb was unusable without
    // it. `set-book` addresses a row by `id`, and for the ONE pair this whole feature exists for —
    // dukascopy's two demo rows — every other cell is identical: same venue, same tier, both labels
    // NULL, both books not yet known. Two lines differing by an opaque integer is not information
    // an operator can act on, and acting on it wrongly routes orders to the other legal entity.
    //
    // The discriminating fact is already in the store, one table over: each row's own credential
    // key NAMES. `DUKASCOPY_DEMO1_*` belongs to one row and `DUKASCOPY_DEMO2_*` to the other, and
    // that prefix is the same derivation the migration itself uses to re-find an account
    // (`vike_secrets::AccountKeys`). NAMES ONLY — the reader never selects a value.
    //
    // A store that cannot be keyed answers `None` (a file store, which the `Unanswerable` arm above
    // has already returned for), and a row with no live credential row is simply absent from the
    // map. Neither is an error and neither blanks the rest of the listing.
    //
    // ⚠ A FAILURE here is said out loud rather than rendered as an empty column. The store has
    // already opened once (the read above), so this can only fail on something new — and a blank
    // discriminator that looks like *this row owns no keys* is precisely the confident-about-what-
    // it-cannot-see answer the three-state `Accounts` enum exists to refuse.
    let keys = match vike_secrets::resolve_account_keys_in(&settings) {
        Ok(Some(map)) => map,
        Ok(None) => std::collections::BTreeMap::new(),
        Err(e) => {
            println!(
                "⚠ the credential key names could not be read ({e}), so the `credential keys` \
                 column below is EMPTY — that is this command failing, not the rows owning no \
                 keys. Do not choose a row from this listing until it reads again."
            );
            std::collections::BTreeMap::new()
        }
    };
    let prefixes_of = |id: i64| -> String {
        match keys.get(&id) {
            Some(k) if !k.prefixes.is_empty() => k.prefixes.join(" "),
            // A row whose credential names carry no derivable prefix still has NAMES, and the block
            // below prints them; a row with neither has outlived every key that made it.
            Some(k) if !k.names.is_empty() => "(see key names below)".to_string(),
            _ => "(no credential rows)".to_string(),
        }
    };

    println!("account table in {}", vike_secrets::db_path_in(&settings).display());
    if rows.is_empty() {
        println!("  (none)");
        return Ok(());
    }
    // A fixed-width listing rather than a table library: the columns are short and the widest cell
    // is a venue id. `id` comes first because it is the identity and the argument `set-book` takes.
    // The last column carries only `INACTIVE`, so the header leaves it blank rather than naming a
    // cell that is empty on every ordinary row.
    println!(
        "  {:>4}  {:<12} {:<6} {:<10} {:<24} {:<26} {:<20}",
        "id", "venue", "tier", "label", "book", "credential keys", "last verified"
    );
    for a in rows {
        println!(
            "  {:>4}  {:<12} {:<6} {:<10} {:<24} {:<26} {:<20} {}",
            a.id,
            a.venue,
            a.tier,
            // ⚠ NEVER a synthesised label. `None` is the ordinary answer and the owner refused the
            // provisional spellings at the spec's signature; rendering `DEFAULT` here would print a
            // string `vike_model::accounts::account_keys::AccountLabel::parse` refuses as reserved, and
            // rendering an index token would put an identity back in the column the schema exists
            // to stop carrying one.
            a.label.as_deref().unwrap_or("-"),
            // `None` is *not yet known*, never *this account has no book*. See
            // `vike_secrets::Account::venue_account_id`.
            a.venue_account_id.as_deref().unwrap_or("(not yet known)"),
            prefixes_of(a.id),
            // ⚠ **`NEVER VERIFIED`, in capitals, and NEVER a dash or a blank.** This column's whole
            // reason for existing is a measured incident: nothing recorded when a credential last
            // authenticated, so *never verified* and *verified three weeks ago* were both rendered
            // as nothing at all and both read as *fine*. A `-` here would rebuild that — it is the
            // same glyph this listing already uses for an ABSENT LABEL, which genuinely is the
            // ordinary answer and genuinely is fine. So the absence is spelled out as a state.
            //
            // The `Some` value is an RFC 3339 instant written by
            // `vike_secrets::BookSource::Handshake` and by nothing else, so a date in this column
            // means A VENUE ANSWERED — never *an operator typed a number in*.
            // `vike_secrets::Account::last_verified_at` carries that argument.
            a.last_verified_at.as_deref().unwrap_or(NEVER_VERIFIED),
            if a.active { "" } else { "INACTIVE" }
        );
    }

    // ⚠ …and for the rows a prefix might STILL not separate, the full key names. This block fires
    // only for a `(venue, tier, label)` that more than one row shares — which is exactly the state
    // `vike_secrets::AccountResolver` refuses to guess in, and exactly the state an operator has to
    // resolve by hand before `set-book` can be aimed. On an ordinary store it prints nothing.
    let mut groups: std::collections::BTreeMap<_, Vec<&vike_secrets::Account>> =
        std::collections::BTreeMap::new();
    for a in rows {
        groups.entry((a.venue.as_str(), a.tier.as_str(), a.label.as_deref())).or_default().push(a);
    }
    for (key, group) in groups.iter().filter(|(_, g)| g.len() > 1) {
        let (venue, tier, label) = *key;
        println!(
            "\n⚠ {} rows share ({venue}, {tier}, label={}) — nothing in the account table tells \
             them apart, so identify each one by the credential keys it owns:",
            group.len(),
            label.unwrap_or("none")
        );
        for a in group {
            let names = keys.get(&a.id).map(|k| k.names.join(", ")).unwrap_or_default();
            println!(
                "    account {}: {}",
                a.id,
                if names.is_empty() {
                    "(no credential rows name this account)"
                } else {
                    names.as_str()
                }
            );
        }
    }

    let unknown = rows.iter().filter(|a| a.venue_account_id.is_none()).count();
    let unverified = rows.iter().filter(|a| a.last_verified_at.is_none()).count();
    println!("\n{} account(s), {unknown} with no venue account id yet", rows.len());
    if unknown > 0 {
        println!(
            "a blank book means the store has not been told which account the row is — not that \
             the account has none. `vike-cli secrets set-book --id N --venue-account-id VALUE` \
             writes one, and `--clear` puts one back to blank."
        );
    }
    // ⚠ **The `last verified` column, explained where it is printed.** The column exists because
    // its absence was a measured failure: with nothing recording when a credential last
    // authenticated, a row that had NEVER worked and a row that worked three weeks ago were
    // indistinguishable from a row that is fine. Printing the column without this paragraph would
    // half-fix that — an operator reading `{NEVER_VERIFIED}` on every row of a freshly migrated box
    // needs to be told it is the ordinary state of a store nothing has mounted yet, or the column
    // becomes an alarm that is always on and is therefore read as furniture.
    if unverified > 0 {
        println!(
            "{unverified} row(s) read `{NEVER_VERIFIED}`: nothing has ever authenticated as that \
             account AND SAID SO. On a freshly migrated box that is every row and is expected — it \
             is not a fault, and it is also not the same as *fine*, which is the whole point of the \
             column. ⚠ `vike-cli secrets set-book` deliberately leaves it alone: an operator typing \
             a number read off the venue's own web page has not authenticated anything. A date \
             appears only when a venue's own handshake is folded — today that is a live dukascopy \
             mount, parked into the state directory and folded by `vike-cli secrets confirm`."
        );
    }
    // ⚠ The scope of `id`, stated where the ids are printed. It is a SQLite rowid, AUTOINCREMENT
    // since stage 4 of the settings-store plane — so within one file a removed id is never handed
    // out again — but the high-water mark is a row of that same file, so a re-migration (the
    // documented repair for a half-finished one) still re-assigns every number. A book written
    // against a remembered id after that names whichever row now holds it — on dukascopy, the
    // other broker. That is why the line below says FILE rather than forever.
    println!(
        "ids are stable for the life of this database file only: deleting it and re-running \
         `secrets migrate` re-numbers these rows and carries no book across, so identify each row \
         from its credential keys again rather than from a remembered id."
    );

    report_parked_confirmations(
        &settings.join(vike_model::paths::state_path::STATE_SUBDIR),
        Fold::Reachable,
    );
    Ok(())
}

/// What this listing renders for [`vike_secrets::Account::last_verified_at`] when the column is
/// `None` — spelled ONCE, because the row printer, the footer paragraph and the tests that gate
/// both must agree, and because the value is the finding rather than a formatting detail.
///
/// ⚠ **Not `-` and not a blank.** The column exists because the ABSENCE of a verification record
/// was indistinguishable from a healthy row; rendering it as a dash — the same glyph this listing
/// uses for an absent LABEL, which really is fine — would put that back.
pub(super) const NEVER_VERIFIED: &str = "NEVER VERIFIED";

/// **Whether anything on THIS box could fold a parked confirmation**, which decides what
/// [`report_parked_confirmations`] tells the operator to do next.
///
/// Not a detail of wording: the two boxes need opposite instructions, and the box that needs the
/// longer one is the box the notice was previously unreachable on.
enum Fold {
    /// The store answered with an `account` table — `vike-cli secrets confirm` resolves each record
    /// and writes the rows it can.
    Reachable,
    /// A `Backend::Absent` box. `vike-cli secrets confirm` refuses by NAME and KEEPS every record;
    /// the migration is the only way through. See [`run_confirm`]'s `Unanswerable` arm, which is
    /// the behaviour this arm describes rather than duplicates.
    BlockedNoAccountTable,
}

/// **The PARKED confirmations, surfaced in the listing an operator actually reads** — on **both**
/// of [`run_accounts`]' exits.
///
/// A live mount records what its venue's handshake answered and cannot fold it: the daemon's
/// sandbox grants `<project>/settings/state` and not the database. Without this notice the only way
/// to learn that a fold is waiting — or that the store and a venue DISAGREE about which account a
/// credential set is — would be to run a verb nobody has a reason to think of. A READ verb may not
/// write, so this names `confirm` rather than folding anything itself.
///
/// # ⚠ Why it is a FUNCTION, and why the [`Fold`] parameter exists
///
/// It was inline at the foot of [`run_accounts`], below an `Unanswerable` arm that returns EARLY —
/// so on a `Backend::Absent` box the notice rendered for nobody. That is precisely the population it
/// was written for: a mount parks records on such a box exactly as it does on a migrated one
/// (parking is not what waits for the migration — folding is), and an unmigrated operator has no
/// other way to learn that records are accumulating, or that `confirm` will refuse them until the
/// store moves. A record parked and never mentioned is the same silence this whole path exists to
/// remove.
///
/// The `Fold` arm is what keeps the two answers honest rather than merely reachable: pointing an
/// unmigrated operator at `secrets confirm` with no further comment would send them to a verb that
/// exits non-zero with a refusal, which reads as a broken tool instead of as a box that has not
/// migrated yet.
///
/// # It is a NOTICE and never an error
///
/// A store with nothing parked prints nothing at all — an absent file is an ANSWER
/// (`vike_model::accounts::account_confirmation::read`), and a box that has mounted no confirming venue is
/// the ordinary case. A state directory that will not READ is a finding about the notice rather
/// than about the rows above it, so it is said out loud and changes no exit code.
///
/// Nothing here can print a credential: a record holds a venue account id, a credential key PREFIX
/// and a row id. See `vike_model::accounts::account_confirmation::ConfirmationRecord`.
fn report_parked_confirmations(state: &Path, fold: Fold) {
    let parked = match vike_model::accounts::account_confirmation::read(Some(state)) {
        Ok(parked) if parked.is_empty() => return,
        Ok(parked) => parked,
        Err(e) => {
            println!(
                "\n⚠ the parked venue confirmations in {} could not be read ({e}), so this \
                 listing cannot say whether a fold is waiting. The rows above are unaffected.",
                state.display()
            );
            return;
        }
    };

    println!("\n{} venue confirmation(s) parked by a live mount and not yet folded:", parked.len());
    for rec in &parked {
        let at = vike_model::time::epoch_ms_to_utc_timestamp(rec.at_ms);
        println!(
            "    {} / {}: the venue answered `{}` at {at}",
            rec.venue, rec.key_prefix, rec.handshake_account_id
        );
    }
    match fold {
        Fold::Reachable => println!(
            "`vike-cli secrets confirm --dry-run` shows what each would do; \
             `vike-cli secrets confirm` folds them into the `book` and `last verified` columns \
             above."
        ),
        // ⚠ The `Backend::Absent` answer (or a database older than the account table), and it is
        // deliberately the LONGER one. It has to say three things an unmigrated operator cannot
        // infer: that the records are safe, that `confirm` will refuse rather than half-work, and
        // that the refusal is about the STORE rather than about the records.
        Fold::BlockedNoAccountTable => println!(
            "⚠ NOTHING ON THIS BOX CAN FOLD THESE YET, and they are not lost. There is no \
             `account` table for a book or a verification timestamp to be written INTO, so \
             `vike-cli secrets confirm` refuses by name, writes nothing, creates no database and \
             KEEPS every record. Recording is not what waits for the migration; folding is. The \
             way through:\n  vike-cli secrets migrate --dry-run\n  vike-cli secrets migrate\n  \
             vike-cli secrets confirm"
        ),
    }
}
